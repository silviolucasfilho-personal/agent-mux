//! The Workflows sidebar section, its preview, the run and compose
//! dialogs, and the Workflows view (`W`) as the user drives them. Nothing
//! here spawns a harness: starting a run needs a profile whose command
//! exists, and the tests stop at the dialog's validation.

use agent_mux::app::workflows_view::{DialogField, DialogPurpose, RunRow};
use agent_mux::app::{App, Mode, SidebarSection};
use agent_mux::config::Profile;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::time::Instant;
use tokio::sync::mpsc;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn profile(name: &str, command: &str) -> Profile {
    Profile {
        name: name.into(),
        command: command.into(),
        args: vec![],
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
    }
}

fn app_with(profiles: Vec<Profile>) -> (App, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::channel(32);
    let mut app = App::new(profiles, None, tx);
    app.clipboard_enabled = false;
    app.set_pane_size(30, 120);
    app.library_root = Some(temp.path().join("library"));
    app.skills_dir = Some(temp.path().join("library").join("skills"));
    app.skill_install_home = Some(temp.path().join("home"));
    app.loops_file = Some(temp.path().join("loops.json"));
    app.runtime_dir = Some(temp.path().join("runtime"));
    app.reload_skills();
    app.load_loop_registry();
    app.history_sessions.clear();
    (app, temp)
}

fn render(app: &App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| agent_mux::ui::draw(f, app, Instant::now()))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(&key(code), Instant::now());
}

fn to_workflows(app: &mut App) {
    for _ in 0..6 {
        if app.sidebar_section == SidebarSection::Workflows {
            return;
        }
        press(app, KeyCode::Tab);
    }
    panic!("Tab never reached the Workflows section");
}

#[test]
fn the_sidebar_has_a_workflows_section_between_loops_and_history() {
    let (mut app, _temp) = app_with(vec![profile("Claude Code", "claude")]);
    let text = render(&app, 120, 40);
    assert!(
        text.contains("Workflows [0]") || text.contains("Workflows ["),
        "{text}"
    );
    assert_eq!(app.sidebar_section, SidebarSection::Active);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.sidebar_section, SidebarSection::Agents);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.sidebar_section, SidebarSection::Loops);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.sidebar_section, SidebarSection::Workflows);
    assert_eq!(
        app.workflow_list.len(),
        8,
        "the built-ins are listed on entry"
    );
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.sidebar_section, SidebarSection::History);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.sidebar_section, SidebarSection::Active);

    to_workflows(&mut app);
    let text = render(&app, 120, 40);
    assert!(text.contains("Workflows [1/8]"), "{text}");
    assert!(text.contains("review-changes"), "{text}");
    assert!(
        text.contains("[c] compose"),
        "the hint line names the keys\n{text}"
    );
    // the preview card describes the selected document
    assert!(
        text.contains("Choose the review lenses for the change"),
        "{text}"
    );
    assert!(text.contains("fanout"), "{text}");

    // j/k move within the section and continue into History and back
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected_workflow, 1);
    assert_eq!(app.selected_workflow_entry().unwrap().name, "understand");
    for _ in 0..10 {
        press(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.selected_workflow, 7, "no history sessions, so it stays");
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.selected_workflow, 6);
    for _ in 0..7 {
        press(&mut app, KeyCode::Char('k'));
    }
    assert_eq!(
        app.sidebar_section,
        SidebarSection::Loops,
        "k leaves upward"
    );
}

#[test]
fn enter_opens_the_run_dialog_with_the_documents_args() {
    let (mut app, _temp) = app_with(vec![
        profile("Claude Code", "claude"),
        profile("Codex", "codex"),
        profile("Antigravity", "agy"),
    ]);
    to_workflows(&mut app);
    // research has a required arg
    while app.selected_workflow_entry().unwrap().name != "research" {
        press(&mut app, KeyCode::Char('j'));
    }
    press(&mut app, KeyCode::Enter);
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!("not the dialog: {:?} notice {:?}", app.mode, app.notice);
    };
    assert_eq!(
        d.purpose,
        DialogPurpose::Run {
            name: "research".into()
        }
    );
    assert_eq!(d.arg_names, vec!["question"]);
    assert_eq!(d.profiles.len(), 3, "every harness is allowed");
    assert_eq!(d.field, DialogField::Workspace);
    let text = render(&app, 120, 40);
    assert!(text.contains("Run research"), "{text}");
    assert!(text.contains("question"), "{text}");
    assert!(text.contains("(required)"), "{text}");

    // Tab walks Workspace -> Profile -> arg -> Budget -> Max cost -> Isolation
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!()
    };
    assert_eq!(d.field, DialogField::Arg(0));
    for c in "why?".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Tab);
    for c in "400k".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!()
    };
    assert_eq!(d.field, DialogField::Isolation);
    press(&mut app, KeyCode::Right);
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!()
    };
    assert_eq!(
        d.isolation,
        agent_mux::workflows::document::Isolation::Worktree
    );
    assert_eq!(d.args_value(), serde_json::json!({ "question": "why?" }));
    assert_eq!(d.budget_tokens().unwrap(), Some(400_000));
    // the profile cycles through harnesses
    press(&mut app, KeyCode::BackTab);
    press(&mut app, KeyCode::BackTab);
    press(&mut app, KeyCode::BackTab);
    press(&mut app, KeyCode::BackTab);
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!()
    };
    assert_eq!(d.field, DialogField::Profile);
    press(&mut app, KeyCode::Right);
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!()
    };
    assert_eq!(d.harness(), Some(agent_mux::harness::Harness::Codex));

    // Enter with a missing workspace stays in the dialog with an error
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::BackTab);
    press(&mut app, KeyCode::BackTab);
    let Mode::WorkflowDialog(d) = &mut app.mode else {
        panic!()
    };
    d.workspace = "/nonexistent/dir".into();
    press(&mut app, KeyCode::Enter);
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!("left the dialog")
    };
    assert!(
        d.error.as_deref().unwrap().contains("does not exist"),
        "{:?}",
        d.error
    );
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Control));
}

#[test]
fn c_opens_the_compose_dialog_and_w_the_view() {
    let (mut app, _temp) = app_with(vec![profile("Claude Code", "claude")]);
    to_workflows(&mut app);
    press(&mut app, KeyCode::Char('c'));
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!("not the dialog: {:?}", app.mode);
    };
    assert_eq!(d.purpose, DialogPurpose::Plan);
    assert_eq!(d.field, DialogField::Task);
    for c in "audit the parser".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    let text = render(&app, 120, 40);
    assert!(text.contains("Compose a workflow"), "{text}");
    assert!(text.contains("audit the parser"), "{text}");
    press(&mut app, KeyCode::Esc);

    press(&mut app, KeyCode::Char('W'));
    let Mode::WorkflowsView(v) = &app.mode else {
        panic!("not the view: {:?}", app.mode);
    };
    assert!(v.rows.is_empty(), "no runs yet");
    let text = render(&app, 120, 40);
    assert!(text.contains("Workflow runs (0)"), "{text}");
    assert!(text.contains("no runs yet"), "{text}");
    assert!(text.contains("Report"), "{text}");
    press(&mut app, KeyCode::Tab);
    let Mode::WorkflowsView(v) = &app.mode else {
        panic!()
    };
    assert_eq!(v.tab, agent_mux::app::workflows_view::ViewTab::Steps);
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Control));
}

#[test]
fn a_planned_document_is_listed_run_saved_or_discarded_from_the_view() {
    let (mut app, temp) = app_with(vec![profile("Claude Code", "claude")]);
    let ws = temp.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    app.planned_workflows.push(agent_mux::app::workflows::PlannedWorkflow {
        id: "plan-1".into(),
        task: "look around".into(),
        workspace: ws.clone(),
        harness: agent_mux::harness::Harness::Claude,
        profile: None,
        budget_tokens: None,
        name: "quick-look".into(),
        document: "[workflow]\nname = \"quick-look\"\ndescription = \"d\"\n[[steps]]\nid = \"s\"\nprompt = \"hi\"\n".into(),
        problems: Vec::new(),
        raw: None,
        run_id: None,
    });
    press(&mut app, KeyCode::Char('W'));
    let Mode::WorkflowsView(v) = &app.mode else {
        panic!()
    };
    assert_eq!(v.rows[0], RunRow::Header("Planned"));
    assert_eq!(v.selected, 1);
    let text = render(&app, 120, 40);
    assert!(text.contains("quick-look"), "{text}");
    assert!(text.contains("valid · Enter runs it"), "{text}");

    // s saves it under a name typed in the footer
    press(&mut app, KeyCode::Char('s'));
    let text = render(&app, 120, 40);
    assert!(
        text.contains("Save to the library as: quick-look_"),
        "{text}"
    );
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Backspace);
    for c in "peek".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    let saved = temp.path().join("library/workflows/quick-peek.toml");
    assert!(saved.is_file(), "{:?}", app.notice);
    assert!(
        std::fs::read_to_string(&saved)
            .unwrap()
            .contains("name = \"quick-peek\"")
    );
    assert!(
        app.workflow_list.iter().any(|e| e.name == "quick-peek"),
        "the section list reloaded"
    );

    // x discards it
    press(&mut app, KeyCode::Char('x'));
    assert!(app.planned_workflows.is_empty());
    let Mode::WorkflowsView(v) = &app.mode else {
        panic!()
    };
    assert!(v.rows.iter().all(|r| !matches!(r, RunRow::Planned(_))));
}

#[test]
fn the_help_overlay_and_the_section_hints_mention_workflows() {
    let (mut app, _temp) = app_with(vec![profile("Claude Code", "claude")]);
    press(&mut app, KeyCode::Char('?'));
    let text = render(&app, 120, 45);
    assert!(text.contains("workflows view"), "{text}");
    assert!(text.contains("Workflows section"), "{text}");
}

fn press_mod(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.handle_key(&KeyEvent::new(code, mods), Instant::now());
}

#[test]
fn the_task_field_takes_a_multi_line_prompt() {
    let (mut app, temp) = app_with(vec![profile("Claude Code", "claude")]);
    to_workflows(&mut app);
    press(&mut app, KeyCode::Char('c'));

    // a prompt with paragraphs: Alt+Enter makes the newlines, Enter is
    // still reserved for submitting
    for c in "Audit the parser for unchecked indexing.".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press_mod(&mut app, KeyCode::Enter, KeyModifiers::ALT);
    press_mod(&mut app, KeyCode::Enter, KeyModifiers::ALT);
    for c in "Check every slice and every unwrap.".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press_mod(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
    for c in "Report by file.".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!("the dialog closed: {:?}", app.mode)
    };
    assert_eq!(
        d.task.text,
        "Audit the parser for unchecked indexing.\n\nCheck every slice and every unwrap.\nReport by file."
    );

    // it is drawn over several rows, wrapped, not truncated to one
    let text = render(&app, 120, 40);
    assert!(text.contains("Audit the parser for unchecked"), "{text}");
    assert!(text.contains("Report by file."), "{text}");
    assert!(
        text.contains("[Alt+Enter] newline"),
        "the keys are on screen\n{text}"
    );

    // editing in the middle: up a row, home, then type
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Home);
    for c in "Please ".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!()
    };
    assert!(
        d.task.text.contains("\nPlease Check every slice"),
        "{:?}",
        d.task.text
    );
    // Backspace at the cursor, not at the end of the whole text
    press(&mut app, KeyCode::Backspace);
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!()
    };
    assert!(
        d.task.text.contains("\nPleaseCheck every"),
        "{:?}",
        d.task.text
    );
    assert!(
        d.task.text.ends_with("Report by file."),
        "the end is untouched"
    );

    // Tab leaves the field even though the cursor is mid-text
    press(&mut app, KeyCode::Tab);
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!()
    };
    assert_eq!(d.field, DialogField::Workspace);
    press(&mut app, KeyCode::BackTab);

    // Ctrl+U clears it
    press_mod(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!()
    };
    assert!(d.task.text.is_empty());
    drop(temp);
}

#[test]
fn ctrl_e_composes_the_field_in_the_editor_and_brings_it_back() {
    let (mut app, _temp) = app_with(vec![profile("Claude Code", "claude")]);
    app.editor = Some("true".into());
    to_workflows(&mut app);
    press(&mut app, KeyCode::Char('c'));
    for c in "short start".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press_mod(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
    let req = app.take_editor_request().expect("an editor request");
    assert_eq!(req.asset_id, "dialog:text");
    assert_eq!(req.command, vec!["true".to_string()]);
    assert_eq!(
        std::fs::read_to_string(&req.path).unwrap(),
        "short start",
        "the editor opens on what was typed"
    );
    assert!(
        matches!(app.mode, Mode::WorkflowDialog(_)),
        "the dialog stays open while the editor runs"
    );

    // the user writes a long prompt and saves
    let long = "A much longer prompt.\n\nWith several paragraphs,\nand lines.\n";
    std::fs::write(&req.path, long).unwrap();
    app.editor_finished(req, Ok(()));
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!("the dialog closed: {:?}", app.mode)
    };
    assert_eq!(d.task.text, long.trim_end_matches('\n'));
    assert_eq!(
        d.field,
        DialogField::Task,
        "the same field is still focused"
    );
}

#[test]
fn an_argument_keeps_a_multi_line_prompt_verbatim() {
    let (mut app, _temp) = app_with(vec![profile("Claude Code", "claude")]);
    to_workflows(&mut app);
    while app.selected_workflow_entry().unwrap().name != "migrate" {
        press(&mut app, KeyCode::Char('j'));
    }
    press(&mut app, KeyCode::Enter);
    // Workspace -> Profile -> arg pattern
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!("{:?}", app.mode)
    };
    assert_eq!(d.field, DialogField::Arg(0));
    for c in "old_api(".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Tab);
    for c in "Call new_api instead.".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press_mod(&mut app, KeyCode::Enter, KeyModifiers::ALT);
    for c in "Keep the argument order.".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    let Mode::WorkflowDialog(d) = &app.mode else {
        panic!()
    };
    let args = d.args_value();
    assert_eq!(args["pattern"], "old_api(");
    assert_eq!(
        args["replacement"], "Call new_api instead.\nKeep the argument order.",
        "the newline survives into the run's args"
    );
    // focused, it shows every line; its label is padded, not overrun
    let text = render(&app, 120, 40);
    assert!(text.contains("Call new_api instead."), "{text}");
    assert!(text.contains("Keep the argument order."), "{text}");
    assert!(
        text.lines()
            .any(|l| l.contains("replacement") && l.contains("Call new_api")),
        "the label keeps its column\n{text}"
    );
    // unfocused, it previews one line with a marker
    press(&mut app, KeyCode::Tab);
    let text = render(&app, 120, 40);
    assert!(text.contains("Call new_api instead.…"), "{text}");
}

/// A finished run's result is often one paragraph far wider than the pane
/// and far taller than it: the detail pane wraps it and the whole of it is
/// reachable by scrolling.
#[test]
fn a_long_result_wraps_and_scrolls_in_the_result_tab() {
    use agent_mux::workflows::store as wstore;

    let (mut app, temp) = app_with(vec![profile("Claude Code", "claude")]);
    let db = temp.path().join("traces.db");
    let _store =
        agent_mux::tracing::store::open_rw(&db, agent_mux::tracing::store::OpenOptions::default())
            .unwrap();
    let conn = agent_mux::tracing::store::open_aux(&db).unwrap();
    // one very long line, then numbered lines past the bottom of the pane
    let mut result = format!("opening {} sentence FIRSTMARK\n", "very long ".repeat(40));
    for i in 1..=80 {
        result.push_str(&format!("finding {i}\n"));
    }
    result.push_str("LASTMARK");
    wstore::upsert_run(
        &conn,
        &wstore::WorkflowRun {
            id: "run-00000001".into(),
            workflow: "map-codebase".into(),
            source: "library".into(),
            document_hash: "h".into(),
            document: "[workflow]\nname = \"map-codebase\"\n".into(),
            workspace: temp.path().display().to_string(),
            harness: "claude".into(),
            profile: "Claude Code".into(),
            args: serde_json::Value::Null,
            budget_tokens: None,
            started_ns: 1,
            ended_ns: Some(2),
            status: "finished".into(),
            sessions: 3,
            tokens: Some(1000),
            cost_usd: Some(0.5),
            result: serde_json::Value::String(result),
            error: None,
            resumed_from: None,
        },
    )
    .unwrap();
    drop(conn);
    app.trace_db_path = Some(db);

    press(&mut app, KeyCode::Char('W'));
    // Progress → Document → Result
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    let Mode::WorkflowsView(v) = &app.mode else {
        panic!("the view is open")
    };
    assert_eq!(v.tab, agent_mux::app::workflows_view::ViewTab::Result);

    let top = render(&app, 100, 30);
    assert!(top.contains("FIRSTMARK"), "the long line wraps\n{top}");
    assert!(
        !top.contains("LASTMARK"),
        "the tail is below the fold\n{top}"
    );
    assert!(
        top.lines()
            .any(|l| l.contains("Result") && l.contains("1–") && l.contains("/89")),
        "the pane title carries the scroll position\n{top}"
    );
    assert!(!top.lines().any(|l| l.contains("very long very long very long very long very long very long very long very long very long")), "no row runs past the pane");

    // End reaches the bottom, Home comes back
    press(&mut app, KeyCode::End);
    let bottom = render(&app, 100, 30);
    assert!(
        bottom.contains("LASTMARK"),
        "End scrolls to the end\n{bottom}"
    );
    assert!(!bottom.contains("FIRSTMARK"), "{bottom}");
    press(&mut app, KeyCode::Home);
    assert_eq!(render(&app, 100, 30), top);

    // the wheel over the detail pane scrolls it too
    press(&mut app, KeyCode::Right);
    app.scroll_workflows_view(3);
    let Mode::WorkflowsView(v) = &app.mode else {
        panic!()
    };
    assert_eq!(v.scroll_offset, 3);
    assert!(v.max_scroll() > 60, "the wrapped rows outgrow the pane");
}

/// A finished review run: two findings found, one of them refuted by two of
/// three voters, and a report written from the survivor.
#[test]
fn the_report_tab_leads_with_the_verdict_and_shows_what_was_refuted() {
    use agent_mux::workflows::store as wstore;

    let (mut app, temp) = app_with(vec![profile("Claude Code", "claude")]);
    let db = temp.path().join("traces.db");
    let _store =
        agent_mux::tracing::store::open_rw(&db, agent_mux::tracing::store::OpenOptions::default())
            .unwrap();
    let conn = agent_mux::tracing::store::open_aux(&db).unwrap();
    let document = r#"
[workflow]
name = "review-changes"
description = "d"
output = "report"
[schemas.finding]
fields.file = { type = "string", required = true }
fields.line = { type = "integer" }
fields.title = { type = "string", required = true }
fields.why = { type = "string", required = true }
fields.severity = { type = "string", enum = ["high", "medium", "low"], required = true }
[schemas.verdict]
fields.refuted = { type = "boolean", required = true }
fields.reason = { type = "string", required = true }
[[steps]]
id = "confirmed"
kind = "pipeline"
phase = "Verify"
over = ["a", "b"]
verify = { prompt = "refute", votes = 3, result = "verdict", keep = "refuted < 2" }
[[steps]]
id = "report"
kind = "single"
phase = "Report"
prompt = "write it"
input = "confirmed"
"#;
    wstore::upsert_run(
        &conn,
        &wstore::WorkflowRun {
            id: "run-00000002".into(),
            workflow: "review-changes".into(),
            source: "built-in".into(),
            document_hash: "h".into(),
            document: document.into(),
            workspace: temp.path().display().to_string(),
            harness: "claude".into(),
            profile: "Claude Code".into(),
            args: serde_json::Value::Null,
            budget_tokens: None,
            started_ns: 1_000_000_000,
            ended_ns: Some(373_000_000_000),
            status: "finished".into(),
            sessions: 7,
            tokens: Some(1_900_000),
            cost_usd: Some(3.1),
            result: serde_json::Value::String(
                "# Review of the branch\n\nOne finding survived.\n\n## High\n\nbody".into(),
            ),
            error: None,
            resumed_from: None,
        },
    )
    .unwrap();
    let kept = serde_json::json!({"file": "src/app/loops.rs", "line": 1572,
        "title": "final_message is cut at 2000 bytes", "why": "a long narrative loses its block",
        "severity": "high"});
    let gone = serde_json::json!({"file": "src/ui.rs", "line": 40, "title": "not a bug",
        "why": "it reads oddly", "severity": "low"});
    let step = |session: &str, item: &serde_json::Value, refuted: bool, reason: &str| {
        wstore::upsert_step(
            &conn,
            &wstore::WorkflowStep {
                run_id: "run-00000002".into(),
                session: session.into(),
                step_id: "confirmed".into(),
                item: item.clone(),
                launch_id: None,
                phase: "Verify".into(),
                harness: "claude".into(),
                kind: "object".into(),
                started_ns: Some(0),
                ended_ns: Some(60_000_000_000),
                tokens: Some(1000),
                cost_usd: Some(0.1),
                worktree: None,
                changed_files: serde_json::Value::Null,
                result: serde_json::json!({"refuted": refuted, "reason": reason}),
            },
        )
        .unwrap();
    };
    step("confirmed[0]/vote1", &kept, false, "it is real");
    step("confirmed[0]/vote2", &kept, true, "guarded above");
    step("confirmed[0]/vote3", &kept, false, "it is real");
    step("confirmed[1]/vote1", &gone, true, "the caller checks it");
    step("confirmed[1]/vote2", &gone, true, "unreachable branch");
    step("confirmed[1]/vote3", &gone, false, "maybe");
    drop(conn);
    app.trace_db_path = Some(db);

    press(&mut app, KeyCode::Char('W'));
    let Mode::WorkflowsView(v) = &app.mode else {
        panic!("the view is open")
    };
    assert_eq!(v.tab, agent_mux::app::workflows_view::ViewTab::Report);

    let out = render(&app, 120, 40);
    assert!(out.contains("finished"), "{out}");
    assert!(
        out.contains("Review of the branch"),
        "the verdict is the answer's heading\n{out}"
    );
    assert!(
        out.contains("1 finding"),
        "the kept findings are counted\n{out}"
    );
    assert!(out.contains("1 refuted"), "so are the dropped ones\n{out}");
    assert!(out.contains("1 high"), "{out}");
    assert!(
        out.contains("1.9M tokens") && out.contains("$3.10"),
        "{out}"
    );
    assert!(out.contains("6m 12s"), "the duration is human\n{out}");
    assert!(
        out.contains("src/app/loops.rs:1572"),
        "a finding carries its file and line\n{out}"
    );
    assert!(out.contains("HIGH"), "{out}");
    assert!(
        out.contains("1/3"),
        "the votes against are on the row\n{out}"
    );
    assert!(out.contains("Refuted (1)"), "{out}");
    assert!(
        out.contains("the caller checks it"),
        "a refuter's reason is on screen\n{out}"
    );

    // the ledger is one tab away and names every session
    press(&mut app, KeyCode::Tab);
    let steps = render(&app, 120, 40);
    assert!(steps.contains("confirmed[0]/vote1"), "{steps}");
    assert!(steps.contains("Verify · confirmed"), "{steps}");
}
