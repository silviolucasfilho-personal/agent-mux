//! The flow builder as the user drives it: `f` in the Workflows section,
//! steps added one by one, what passes between them, agents written on
//! the spot, the review and the save. Nothing here spawns a harness.

use agent_mux::app::flow_builder::{Field, FlowBuilderState, Overlay, Screen};
use agent_mux::app::{App, Mode, SidebarSection};
use agent_mux::config::Profile;
use agent_mux::workflows::builder::{FieldKind, Role};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::time::Instant;
use tokio::sync::mpsc;

fn app() -> (App, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::channel(32);
    let profile = Profile {
        name: "Claude Code".into(),
        command: "claude".into(),
        args: vec![],
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
    };
    let mut app = App::new(vec![profile], None, tx);
    app.clipboard_enabled = false;
    app.set_pane_size(40, 160);
    app.library_root = Some(temp.path().join("library"));
    app.skills_dir = Some(temp.path().join("library").join("skills"));
    app.skill_install_home = Some(temp.path().join("home"));
    app.loops_file = Some(temp.path().join("loops.json"));
    app.runtime_dir = Some(temp.path().join("runtime"));
    app.reload_skills();
    app.load_loop_registry();
    app.history_sessions.clear();
    std::fs::create_dir_all(temp.path().join("ws")).unwrap();
    (app, temp)
}

fn render(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(160, 44)).unwrap();
    terminal
        .draw(|f| agent_mux::ui::draw(f, app, Instant::now()))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(&KeyEvent::new(code, KeyModifiers::NONE), Instant::now());
}

fn ctrl(app: &mut App, c: char) {
    app.handle_key(
        &KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
        Instant::now(),
    );
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn st(app: &App) -> &FlowBuilderState {
    match &app.mode {
        Mode::FlowBuilder(s) => s,
        other => panic!("not in the flow builder: {other:?}"),
    }
}

/// Moves the field cursor to `f` on the Steps screen.
fn to_field(app: &mut App, f: Field) {
    let target = st(app)
        .fields()
        .iter()
        .position(|x| *x == f)
        .unwrap_or_else(|| panic!("{f:?} is not a field here"));
    press(app, KeyCode::Home);
    while st(app).field > target {
        press(app, KeyCode::Up);
    }
    while st(app).field < target {
        press(app, KeyCode::Down);
    }
}

/// In a list overlay, moves to the option whose label contains `text`.
fn pick(app: &mut App, text: &str) {
    let Some(Overlay::Pick { options, .. }) = &st(app).overlay else {
        panic!("no list open");
    };
    let target = options
        .iter()
        .position(|o| o.label.contains(text))
        .unwrap_or_else(|| panic!("no option {text:?}: {options:?}"));
    for _ in 0..40 {
        let Some(Overlay::Pick { selected, .. }) = &st(app).overlay else {
            break;
        };
        if *selected == target {
            break;
        }
        press(
            app,
            if *selected < target {
                KeyCode::Down
            } else {
                KeyCode::Up
            },
        );
    }
    press(app, KeyCode::Enter);
}

/// Opens the agent picker for the selected step (Enter on Who).
fn open_agents(app: &mut App) {
    if st(app).focus == agent_mux::app::flow_builder::Focus::List {
        press(app, KeyCode::Right);
    }
    to_field(app, Field::Who);
    press(app, KeyCode::Enter);
    assert!(
        matches!(st(app).overlay, Some(Overlay::Agent(_))),
        "the picker is open"
    );
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

/// Adds a step running as the `n`th shape, named `name`.
fn add_step(app: &mut App, name: &str, shape: usize) {
    press(app, KeyCode::Char('n'));
    for _ in 0..shape {
        press(app, KeyCode::Right);
    }
    press(app, KeyCode::Tab);
    ctrl(app, 'u');
    type_text(app, name);
    press(app, KeyCode::Enter);
    assert!(st(app).overlay.is_none(), "the step was added");
}

#[test]
fn a_flow_is_built_step_by_step_and_saved() {
    let (mut app, temp) = app();
    let ws = temp.path().join("ws");
    to_workflows(&mut app);
    press(&mut app, KeyCode::Char('n'));
    assert!(matches!(app.mode, Mode::FlowBuilder(_)));
    let text = render(&app);
    assert!(text.contains("New flow"), "{text}");
    assert!(text.contains("flow settings"), "{text}");
    assert!(text.contains("add a step"), "{text}");

    // the flow's own settings: a name and the workspace
    press(&mut app, KeyCode::Right);
    to_field(&mut app, Field::FlowName);
    press(&mut app, KeyCode::Enter);
    ctrl(&mut app, 'u');
    type_text(&mut app, "sec-review");
    press(&mut app, KeyCode::Enter);
    to_field(&mut app, Field::FlowDescription);
    press(&mut app, KeyCode::Enter);
    type_text(&mut app, "Review a change and check every finding");
    press(&mut app, KeyCode::Enter);
    to_field(&mut app, Field::FlowWorkspace);
    press(&mut app, KeyCode::Enter);
    ctrl(&mut app, 'u');
    type_text(&mut app, &ws.to_string_lossy());
    press(&mut app, KeyCode::Enter);
    assert_eq!(st(&app).draft.name(), "sec-review");
    assert_eq!(st(&app).workspace, ws.to_string_lossy());

    // 1 brief: once, a prompt
    add_step(&mut app, "brief", 0);
    assert_eq!(
        st(&app).current_field(),
        Some(Field::Does),
        "straight to what it does"
    );
    press(&mut app, KeyCode::Enter);
    pick(&mut app, "write a prompt");
    assert!(matches!(st(&app).overlay, Some(Overlay::Prompt { .. })));
    type_text(&mut app, "Write a brief of the change.");
    press(&mut app, KeyCode::Enter);

    // 2 review: in parallel over two lenses, a skill, reads the brief
    press(&mut app, KeyCode::Esc);
    add_step(&mut app, "review", 1);
    press(&mut app, KeyCode::Enter);
    pick(&mut app, "wf-review-find");
    to_field(&mut app, Field::Items);
    press(&mut app, KeyCode::Enter);
    pick(&mut app, "a list I type");
    type_text(&mut app, "security, bugs");
    press(&mut app, KeyCode::Enter);
    to_field(&mut app, Field::Gets);
    press(&mut app, KeyCode::Enter);
    pick(&mut app, "brief's answer");
    let text = render(&app);
    assert!(text.contains("⇉ 2 in parallel"), "{text}");
    assert!(text.contains("├─ security"), "{text}");

    // what review gives: findings, a list of items with a file and a severity
    press(&mut app, KeyCode::Char('2'));
    assert_eq!(st(&app).screen, Screen::Exchange);
    press(&mut app, KeyCode::Char('n'));
    type_text(&mut app, "findings");
    press(&mut app, KeyCode::Enter);
    for _ in 0..6 {
        press(&mut app, KeyCode::Right);
    }
    let schema = st(&app).ex_schema().unwrap();
    assert!(matches!(
        st(&app).draft.fields(&schema)[0].kind,
        FieldKind::ListOf(_)
    ));
    press(&mut app, KeyCode::Enter); // into the item group
    assert_eq!(st(&app).ex_groups, vec!["finding".to_string()]);
    press(&mut app, KeyCode::Char('n'));
    type_text(&mut app, "file");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char(' ')); // required
    press(&mut app, KeyCode::Char('n'));
    type_text(&mut app, "severity");
    press(&mut app, KeyCode::Enter);
    for _ in 0..4 {
        press(&mut app, KeyCode::Right);
    }
    press(&mut app, KeyCode::Enter); // its values
    ctrl(&mut app, 'u');
    type_text(&mut app, "high, medium, low");
    press(&mut app, KeyCode::Enter);
    let text = render(&app);
    assert!(text.contains("What passes between steps"), "{text}");
    assert!(text.contains("high · medium · low"), "{text}");
    assert!(text.contains("review › finding gives"), "{text}");
    press(&mut app, KeyCode::Esc); // out of the group
    press(&mut app, KeyCode::Esc); // back to the steps
    assert_eq!(st(&app).screen, Screen::Steps);

    // who runs review: a new agent, written here, saved in the workspace
    open_agents(&mut app);
    let text = render(&app);
    assert!(
        text.contains("skeptic · built-in"),
        "the built-in agents are listed: {text}"
    );
    press(&mut app, KeyCode::Char('n'));
    type_text(&mut app, "code-reviewer");
    press(&mut app, KeyCode::Tab);
    type_text(&mut app, "Reviews a change for defects");
    press(&mut app, KeyCode::Tab);
    type_text(
        &mut app,
        "You review code and report only what the code proves.",
    );
    let text = render(&app);
    assert!(text.contains("New agent"), "{text}");
    assert!(text.contains("✓ can read the context file"), "{text}");
    ctrl(&mut app, 's');
    assert!(st(&app).overlay.is_none(), "{:?}", app.notice);
    let agent_file = ws.join(".agent-mux/agents/code-reviewer.toml");
    let spec = agent_mux::agents::AgentSpec::parse(&std::fs::read_to_string(&agent_file).unwrap())
        .unwrap();
    assert_eq!(
        spec.tools_label(),
        "read, shell",
        "the form starts on read and run commands"
    );
    assert_eq!(
        st(&app).draft.agent(1, Role::Step).as_deref(),
        Some("code-reviewer")
    );

    // 3 check: each finding, voted on by the built-in skeptic
    press(&mut app, KeyCode::Esc);
    add_step(&mut app, "check", 2);
    let text = render(&app);
    assert!(
        text.contains("every finding from every review session"),
        "{text}"
    );
    to_field(&mut app, Field::VotersWho);
    press(&mut app, KeyCode::Enter);
    let Some(Overlay::Agent(p)) = &st(&app).overlay else {
        panic!("the agent picker is open");
    };
    let at = p
        .options
        .iter()
        .position(|o| o.as_deref() == Some("skeptic"))
        .unwrap();
    for _ in 0..at {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        st(&app).draft.agent(2, Role::Voters).as_deref(),
        Some("skeptic")
    );
    to_field(&mut app, Field::Votes);
    press(&mut app, KeyCode::Left); // 3 → 2 votes
    assert_eq!(st(&app).draft.check_votes(2), Some((2, 2)));

    // review: the flow in words, the checks, then save
    press(&mut app, KeyCode::Char('3'));
    let text = render(&app);
    assert!(text.contains("In words"), "{text}");
    assert!(text.contains("✓ ready to run"), "{text}");
    assert!(
        text.contains("review: code-reviewer runs wf-review-find for each of security, bugs"),
        "{text}"
    );
    assert!(text.contains("sec-review.toml"), "{text}");
    press(&mut app, KeyCode::Char('s'));
    let saved = temp.path().join("library/workflows/sec-review.toml");
    let doc_text = std::fs::read_to_string(&saved).unwrap();
    let doc = agent_mux::workflows::parse(&doc_text).unwrap();
    let infos = app.all_skill_infos();
    assert!(
        agent_mux::workflows::validate(&doc, Some(&infos)).is_empty(),
        "{doc_text}"
    );
    assert_eq!(
        doc.step("review").unwrap().agent.as_deref(),
        Some("code-reviewer")
    );
    assert_eq!(
        doc.step("check")
            .unwrap()
            .verify
            .as_ref()
            .unwrap()
            .runner
            .agent
            .as_deref(),
        Some("skeptic")
    );
    assert!(!st(&app).dirty);
    assert!(
        app.workflow_entries()
            .iter()
            .any(|e| e.name == "sec-review"),
        "the library lists it"
    );

    // Enter saves again and opens the run dialog on it
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(app.mode, Mode::WorkflowDialog(_)),
        "{:?}",
        app.notice
    );
}

#[test]
fn leaving_with_changes_asks_first_and_a_document_opens_in_the_builder() {
    let (mut app, _temp) = app();
    to_workflows(&mut app);
    press(&mut app, KeyCode::Char('n'));
    add_step(&mut app, "only", 0);
    press(&mut app, KeyCode::Esc); // fields → list
    press(&mut app, KeyCode::Esc); // list → close?
    assert!(matches!(st(&app).overlay, Some(Overlay::Confirm { .. })));
    press(&mut app, KeyCode::Char('n'));
    assert!(matches!(app.mode, Mode::FlowBuilder(_)), "n stays");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('y'));
    assert!(matches!(app.mode, Mode::Control));

    // o on a built-in: every step is there, and nothing changed yet
    let pos = app
        .workflow_entries()
        .iter()
        .position(|e| e.name == "review-changes")
        .unwrap();
    for _ in 0..40 {
        if matches!(
            app.selected_workflow_row(),
            Some(agent_mux::app::workflows::WorkflowRow::Doc(i)) if i == pos
        ) {
            break;
        }
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char('e'));
    let s = st(&app);
    assert_eq!(s.draft.name(), "review-changes");
    assert_eq!(
        s.draft.step_ids(),
        vec!["dimensions", "find", "confirmed", "report"]
    );
    assert!(!s.dirty);
    assert_eq!(s.problems(), 0, "{:?}", s.checks);
    press(&mut app, KeyCode::Esc);
    assert!(
        matches!(app.mode, Mode::Control),
        "unchanged: closes without asking"
    );
}

#[test]
fn a_built_in_agent_is_edited_as_a_library_copy() {
    let (mut app, temp) = app();
    to_workflows(&mut app);
    press(&mut app, KeyCode::Char('n'));
    add_step(&mut app, "look", 0);
    open_agents(&mut app);
    let Some(Overlay::Agent(p)) = &st(&app).overlay else {
        panic!("the agent picker is open");
    };
    let at = p
        .options
        .iter()
        .position(|o| o.as_deref() == Some("reviewer"))
        .unwrap();
    for _ in 0..at {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char('e'));
    let copy = temp.path().join("library/agents/reviewer.toml");
    let req = app.take_editor_request().expect("the editor opens");
    assert_eq!(
        req.path, copy,
        "the built-in is copied into the library first"
    );
    assert_eq!(
        std::fs::read_to_string(&copy).unwrap(),
        agent_mux::agents::builtin("reviewer").unwrap()
    );
    // the edit comes back: the copy now replaces the built-in
    let edited = std::fs::read_to_string(&copy).unwrap().replace(
        "You are a careful code reviewer.",
        "You are a terse reviewer.",
    );
    std::fs::write(&copy, edited).unwrap();
    app.editor_finished(req, Ok(()));
    let entry = st(&app).catalog.entry("reviewer").unwrap().clone();
    assert_eq!(entry.source.label(), "library");
    assert!(
        entry
            .spec
            .unwrap()
            .instructions
            .starts_with("You are a terse reviewer.")
    );
    assert!(
        app.notice
            .as_ref()
            .unwrap()
            .text
            .contains("agent reviewer saved")
    );
}

#[test]
fn a_built_in_workflow_is_edited_as_a_copy_and_restored() {
    let (mut app, temp) = app();
    to_workflows(&mut app);
    let pos = app
        .workflow_entries()
        .iter()
        .position(|e| e.name == "santa-review")
        .unwrap();
    for _ in 0..40 {
        if matches!(
            app.selected_workflow_row(),
            Some(agent_mux::app::workflows::WorkflowRow::Doc(i)) if i == pos
        ) {
            break;
        }
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char('e'));
    assert!(render(&app).contains("Built-in flow"));
    // change what it says it does, then save: the library copy wins
    press(&mut app, KeyCode::Up); // flow settings
    press(&mut app, KeyCode::Right);
    to_field(&mut app, Field::FlowDescription);
    press(&mut app, KeyCode::Enter);
    ctrl(&mut app, 'u');
    type_text(&mut app, "My stricter review");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Char('s'));
    let copy = temp.path().join("library/workflows/santa-review.toml");
    assert!(
        std::fs::read_to_string(&copy)
            .unwrap()
            .contains("My stricter review")
    );
    assert!(
        app.notice
            .as_ref()
            .unwrap()
            .text
            .contains("replaces the built-in")
    );
    let text = render(&app);
    assert!(text.contains("Your copy of a built-in"), "{text}");
    assert!(text.contains("restore built-in"), "{text}");
    // R, confirmed: the copy goes and the built-in text is back
    press(&mut app, KeyCode::Char('R'));
    press(&mut app, KeyCode::Char('y'));
    assert!(!copy.exists());
    assert_ne!(st(&app).draft.flow_str("description"), "My stricter review");
    assert!(render(&app).contains("Built-in flow"));
}
