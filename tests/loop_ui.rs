//! The Loops sidebar section, its preview, the dialog and the Loops view
//! as the user drives them: four sections in the layout, the keys of the
//! section, the hint lines under 100 columns, the help overlay's height,
//! and the dialog offering no Antigravity profile.

use agent_mux::app::loops::LoopField;
use agent_mux::app::{App, Mode, SidebarSection};
use agent_mux::config::Profile;
use agent_mux::loops::registry::{self, LoopEntry};
use agent_mux::loops::{Level, patterns};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::PathBuf;
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
    app.set_pane_size(30, 100);
    app.loops_file = Some(temp.path().join("loops.json"));
    app.runtime_dir = Some(temp.path().join("runtime"));
    app.load_loop_registry();
    (app, temp)
}

fn entry(temp: &tempfile::TempDir, pattern: &str) -> LoopEntry {
    let ws = temp.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let p = patterns::find(pattern).unwrap();
    registry::new_entry(
        &ws,
        p,
        "claude",
        "Claude Code",
        p.default_interval_s,
        Level::L1,
        agent_mux::loops::now(),
    )
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

#[test]
fn the_sidebar_has_four_sections_and_tab_visits_loops() {
    let (mut app, _temp) = app_with(vec![profile("Claude Code", "claude")]);
    let (active, agents, loops, workflows, history) = agent_mux::ui::sidebar_areas(33, 1, 1, 1);
    assert_eq!(
        [
            active.height,
            agents.height,
            loops.height,
            workflows.height,
            history.height
        ],
        [8, 7, 7, 6, 4],
        "history stays compact and the three development sections share the freed rows"
    );
    assert_eq!(loops.y, agents.y + agents.height);
    assert_eq!(workflows.y, loops.y + loops.height);
    assert_eq!(history.y, workflows.y + workflows.height);
    // A short terminal still fits every block: history gives way first and
    // the active quarter never moves; sixteen rows give history its cap.
    let (a, b, c, w, d) = agent_mux::ui::sidebar_areas(13, 1, 1, 1);
    assert_eq!(
        [a.height, b.height, c.height, w.height, d.height],
        [3, 2, 2, 2, 3]
    );
    let (a, b, c, w, d) = agent_mux::ui::sidebar_areas(16, 1, 1, 1);
    assert_eq!(
        [a.height, b.height, c.height, w.height, d.height],
        [3, 3, 3, 2, 4]
    );
    // an empty section shrinks to a border and one line; the others share
    // what it frees
    let (_, agents, loops, workflows, _) = agent_mux::ui::sidebar_areas(33, 1, 0, 1);
    assert_eq!([agents.height, loops.height, workflows.height], [9, 3, 8]);
    let (_, agents, loops, workflows, _) = agent_mux::ui::sidebar_areas(33, 1, 0, 0);
    assert_eq!([agents.height, loops.height, workflows.height], [14, 3, 3]);

    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Agents);
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Loops);
    let screen = render(&app, 120, 34);
    assert!(screen.contains("Loops [0]"), "{screen}");
    assert!(
        screen.contains("No loops yet"),
        "the preview explains loops:\n{screen}"
    );
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::Workflows);
    app.handle_key(&key(KeyCode::Tab), Instant::now());
    assert_eq!(app.sidebar_section, SidebarSection::History);
}

#[test]
fn the_hint_lines_fit_a_hundred_columns_and_the_help_fits() {
    let (mut app, temp) = app_with(vec![profile("Claude Code", "claude")]);
    app.loop_registry.add(entry(&temp, "daily-triage"));
    app.sidebar_section = SidebarSection::Loops;
    let screen = render(&app, 100, 34);
    let last = screen.lines().last().unwrap().trim_end();
    assert!(last.contains("[a] add"), "{last}");
    assert!(last.chars().count() <= 100);
    app.loop_registry.pause_all = true;
    let screen = render(&app, 100, 34);
    let last = screen.lines().last().unwrap().trim_end();
    assert!(last.contains("LOOPS PAUSED"), "{last}");
    assert!(last.chars().count() <= 100);
    assert!(screen.contains("PAUSED"), "{screen}");
    app.loop_registry.pause_all = false;

    app.mode = Mode::Help;
    let screen = render(&app, 100, 70);
    assert!(screen.contains("Loops section"), "{screen}");
    assert!(screen.contains("kill switch"), "{screen}");
    assert!(
        screen.contains("[Esc] or [?] to close"),
        "the overlay fits:\n{screen}"
    );
    // the build stamp closes the overlay, inside its 84-column box
    assert!(
        screen.contains(&format!("agent-mux {}", agent_mux::build_info::VERSION)),
        "the version line:\n{screen}"
    );
    assert!(screen.contains(agent_mux::build_info::COMMIT), "{screen}");
    assert!(screen.contains("built "), "{screen}");
}

#[test]
fn keys_of_the_loops_section_pause_run_and_toggle_the_kill_switch() {
    let (mut app, temp) = app_with(vec![profile("Claude Code", "claude")]);
    let e = entry(&temp, "daily-triage");
    let id = e.id.clone();
    app.loop_registry.add(e);
    app.sidebar_section = SidebarSection::Loops;

    app.handle_key(&key(KeyCode::Char('p')), Instant::now());
    assert!(app.loop_registry.find(&id).unwrap().paused());
    assert!(app.notice.as_ref().unwrap().text.contains("paused"));
    app.handle_key(&key(KeyCode::Char('p')), Instant::now());
    assert!(!app.loop_registry.find(&id).unwrap().paused());
    // the registry file follows
    let saved = registry::load(app.loops_file.as_ref().unwrap());
    assert_eq!(saved.loops.len(), 1);

    app.handle_key(&key(KeyCode::Char('K')), Instant::now());
    assert!(app.loop_registry.pause_all);
    assert!(app.notice.as_ref().unwrap().text.contains("LOOPS PAUSED"));
    let screen = render(&app, 120, 34);
    assert!(screen.contains("Loops [1] PAUSED"), "{screen}");
    // run now while the kill switch is on is refused with a notice
    app.handle_key(&key(KeyCode::Char('r')), Instant::now());
    assert!(app.notice.as_ref().unwrap().text.contains("paused"));
    app.handle_key(&key(KeyCode::Char('K')), Instant::now());
    assert!(!app.loop_registry.pause_all);

    // remove asks first, then removes; the workspace files stay
    app.handle_key(&key(KeyCode::Char('x')), Instant::now());
    assert!(matches!(app.mode, Mode::ConfirmRemoveLoop));
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    assert!(matches!(app.mode, Mode::Control));
    assert_eq!(app.loop_registry.loops.len(), 1);
    app.handle_key(&key(KeyCode::Char('x')), Instant::now());
    app.handle_key(&key(KeyCode::Char('y')), Instant::now());
    assert!(app.loop_registry.loops.is_empty());
    assert!(temp.path().join("proj").is_dir());

    // E opens the Loops view from any section, Esc closes it
    app.sidebar_section = SidebarSection::Active;
    app.handle_key(&key(KeyCode::Char('E')), Instant::now());
    assert!(matches!(app.mode, Mode::LoopsView(_)));
    let screen = render(&app, 120, 40);
    assert!(screen.contains("History"), "{screen}");
    assert!(screen.contains("Setup"), "{screen}");
    assert!(
        !screen.contains("Readiness |"),
        "three tabs merged into Setup\n{screen}"
    );
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    assert!(matches!(app.mode, Mode::Control));
}

#[test]
fn the_dialog_lists_no_antigravity_profile_and_validates() {
    let (mut app, temp) = app_with(vec![
        profile("Antigravity", "agy"),
        profile("Codex", "codex"),
        profile("Claude Code", "claude"),
    ]);
    app.sidebar_section = SidebarSection::Loops;
    app.handle_key(&key(KeyCode::Char('a')), Instant::now());
    let Mode::NewLoop(dialog) = &app.mode else {
        panic!("{:?}", app.notice);
    };
    let names: Vec<&str> = dialog.profiles.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["Codex", "Claude Code"]);
    assert_eq!(dialog.level, Level::L1);
    assert_eq!(dialog.every, "1d", "the pattern's default cadence");
    assert_eq!(dialog.max_tokens, "100000");
    let screen = render(&app, 120, 40);
    assert!(screen.contains("Add loop"), "{screen}");
    assert!(screen.contains("Antigravity: not supported"), "{screen}");
    assert!(
        screen.contains("Select subfolder"),
        "the Workspace field is the shared directory picker: {screen}"
    );

    // an empty workspace path is refused with a field error
    if let Mode::NewLoop(d) = &mut app.mode {
        d.workspace.clear();
    }
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    let Mode::NewLoop(dialog) = &app.mode else {
        panic!("the dialog stays open on an error");
    };
    assert!(dialog.error.as_deref().unwrap().contains("workspace"));

    // a real workspace, register only (no scaffold), saves an entry
    let ws = temp.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    if let Mode::NewLoop(d) = &mut app.mode {
        d.workspace = ws.to_string_lossy().into_owned();
        d.field = LoopField::Scaffold;
    }
    app.handle_key(&key(KeyCode::Char(' ')), Instant::now());
    if let Mode::NewLoop(d) = &app.mode {
        assert!(!d.scaffold);
    }
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(app.mode, Mode::Control), "{:?}", app.notice);
    assert_eq!(app.loop_registry.loops.len(), 1);
    let saved = app.loop_registry.loops[0].clone();
    assert_eq!(saved.pattern, "daily-triage");
    assert_eq!(saved.harness, "codex");
    assert_eq!(saved.workspace, ws);
    assert!(!ws.join("STATE.md").exists(), "register only wrote nothing");
    assert_eq!(app.sidebar_section, SidebarSection::Loops);

    // edit prefills and keeps the id
    app.handle_key(&key(KeyCode::Char('e')), Instant::now());
    let Mode::NewLoop(dialog) = &app.mode else {
        panic!();
    };
    assert_eq!(dialog.editing.as_deref(), Some(saved.id.as_str()));
    assert_eq!(dialog.every, "1d");
    app.handle_key(&key(KeyCode::Esc), Instant::now());
    let _ = PathBuf::new();
}

/// One stored run of `pattern`, with the detail a finished run carries.
fn store_run(
    db: &std::path::Path,
    loop_id: &str,
    run_id: &str,
    outcome: agent_mux::loops::Outcome,
    detail: serde_json::Value,
) {
    use agent_mux::loops::store as lstore;
    let conn = agent_mux::tracing::store::open_aux(db).unwrap();
    let started = agent_mux::loops::parse_timestamp(run_id).unwrap();
    let mut run = lstore::LoopRun {
        id: run_id.into(),
        loop_id: loop_id.into(),
        workspace: "/w".into(),
        pattern: "pr-babysitter".into(),
        harness: "claude".into(),
        level: Level::L1,
        effective_level: Level::L1,
        launch_id: None,
        scheduled_ns: agent_mux::loops::to_ns(started),
        started_ns: Some(agent_mux::loops::to_ns(started)),
        ended_ns: Some(agent_mux::loops::to_ns(started) + 63_000_000_000),
        outcome,
        items_found: Some(8),
        actions_taken: Some(0),
        escalations: Some(2),
        tokens: Some(417_000),
        cost_usd: Some(1.18),
        readiness_score: Some(100),
        worktree: None,
        branch: None,
        decision: None,
        decided_ns: None,
        detail,
        pattern_hash: None,
    };
    if outcome == agent_mux::loops::Outcome::NoOp {
        run.items_found = Some(0);
        run.escalations = Some(0);
    }
    lstore::upsert_run(&conn, &run).unwrap();
}

const STATE: &str = r#"# PR Babysitter — proj

Last run: 2026-09-17T17:36:53Z

## High Priority (loop is acting or waiting on human)

- [ ] #2238 atualiza arquivos para v0.34.2 — conflicts (CONFLICTING); touches `.github/workflows/**`
  Loop action: reported only, unchanged since 2026-09-13.
  Human decision: rebase spec-wave/update-v0.34.2 on develop, then merge.

## Watch List

- #2237 bump vitest from 4.1.10 to 4.1.11 — CLEAN, verify green, no review yet

## Recent Noise (ignored this run)

- (no drafts open)

---
Run log: 2026-09-17T17:36:53Z | 8 findings | 0 actions | 2 escalations
Fingerprint: 2238|DIRTY
"#;

#[test]
fn the_report_tab_shows_what_the_run_found_and_who_has_to_act() {
    let (mut app, temp) = app_with(vec![profile("Claude Code", "claude")]);
    let db = temp.path().join("traces.db");
    let _store =
        agent_mux::tracing::store::open_rw(&db, agent_mux::tracing::store::OpenOptions::default())
            .unwrap();
    app.trace_db_path = Some(db.clone());
    let e = entry(&temp, "pr-babysitter");
    let loop_id = e.id.clone();
    app.loop_registry.loops.push(e);

    store_run(
        &db,
        &loop_id,
        "2026-09-17T17:36:53Z",
        agent_mux::loops::Outcome::Escalated,
        serde_json::json!({
            "summary": "8 open PRs; #2238 conflicts, #1919 blocked on a missing check",
            "verifier": {"ran": false, "verdict": null},
            "files": [],
            "exit_code": 0,
            "final_message": "The open PR queue has two items that need a human.",
            "delta": {"new": [], "gone": [], "moved": [],
                      "changed": [["#2238", "CLEAN", "CONFLICTING after a push"]]}
        }),
    );
    // the run's own copy of the state file is what the Report tab reads
    let runtime = app.loops_runtime_dir();
    agent_mux::loops::state::write_snapshot(&runtime, &loop_id, "2026-09-17T17:36:53Z", STATE)
        .unwrap();

    app.handle_key(&key(KeyCode::Char('E')), Instant::now());
    let out = render(&app, 120, 40);
    assert!(out.contains("Report"), "the first tab is the report\n{out}");
    assert!(
        out.contains("NEEDS YOU"),
        "the outcome is what the user must do\n{out}"
    );
    assert!(
        out.contains("8 found") && out.contains("2 for you"),
        "{out}"
    );
    assert!(
        out.contains("417k tokens") && out.contains("$1.18"),
        "{out}"
    );
    assert!(out.contains("1m 03s"), "the duration is human\n{out}");
    assert!(
        out.contains("#2238") && out.contains("CONFLICTING after a push"),
        "the changelog line names what moved\n{out}"
    );
    assert!(out.contains("Needs you (1)"), "{out}");
    assert!(
        out.contains("Decide"),
        "the human decision is on screen\n{out}"
    );
    assert!(out.contains("rebase spec-wave"), "{out}");
    assert!(out.contains("Watching (1)"), "{out}");
    assert!(out.contains("What the run said"), "{out}");
    // the empty-bucket placeholder is not an item
    assert!(
        out.contains("Ignored (0)") || !out.contains("no drafts open"),
        "{out}"
    );
    // one date format: the state file's RFC 3339 stamp reads like the rest
    assert!(out.contains("last run Sep 17 17:36"), "{out}");
    assert!(!out.contains("17:36:53Z"), "{out}");
    // the view is the whole screen: no sidebar shows down its left edge,
    // and its own hints take the status bar's row
    let first = out.lines().next().unwrap();
    assert!(first.starts_with("┌ Loops (1)"), "{first}");
    let last = out.lines().last().unwrap();
    assert!(last.contains("[Esc] close"), "{last}");
}

#[test]
fn the_runs_timeline_folds_quiet_runs_and_opens_the_selected_one() {
    let (mut app, temp) = app_with(vec![profile("Claude Code", "claude")]);
    let db = temp.path().join("traces.db");
    let _store =
        agent_mux::tracing::store::open_rw(&db, agent_mux::tracing::store::OpenOptions::default())
            .unwrap();
    app.trace_db_path = Some(db.clone());
    let e = entry(&temp, "pr-babysitter");
    let loop_id = e.id.clone();
    app.loop_registry.loops.push(e);

    store_run(
        &db,
        &loop_id,
        "2026-09-17T17:36:53Z",
        agent_mux::loops::Outcome::Escalated,
        serde_json::json!({"summary": "two PRs need a human", "exit_code": 0,
                           "verifier": {"ran": false, "verdict": null}}),
    );
    for (i, when) in [
        "2026-09-17T17:21:53Z",
        "2026-09-17T17:06:53Z",
        "2026-09-17T16:51:53Z",
    ]
    .iter()
    .enumerate()
    {
        store_run(
            &db,
            &loop_id,
            when,
            agent_mux::loops::Outcome::NoOp,
            serde_json::json!({"quiet": true, "exit_code": i}),
        );
    }

    app.handle_key(&key(KeyCode::Char('E')), Instant::now());
    app.handle_key(&key(KeyCode::Char('2')), Instant::now()); // the Runs timeline
    let out = render(&app, 120, 40);
    assert!(
        out.contains("quiet ×3"),
        "three quiet runs fold into one row\n{out}"
    );
    assert!(out.contains("NEEDS YOU"), "{out}");
    assert!(out.contains("two PRs need a human"), "{out}");
    // the selected run opens with why it ended that way
    assert!(out.contains("Why"), "{out}");
    assert!(out.contains("Verifier"), "{out}");
    assert!(out.contains("the run only reports"), "{out}");
    assert!(out.contains("exit 0"), "the run facts are on screen\n{out}");
    assert!(
        out.contains("report only · readiness 100"),
        "every run in the timeline carries the score it ran under\n{out}"
    );
}

/// The sidebar row and the preview name the workspace, not a cut-off path,
/// and no hint line wraps or loses its last hint.
#[test]
fn a_loop_is_named_by_its_workspace_and_its_hints_fit() {
    let (mut app, temp) = app_with(vec![profile("Claude Code", "claude")]);
    app.loop_registry.add(entry(&temp, "daily-triage"));
    app.sidebar_section = SidebarSection::Loops;
    let screen = render(&app, 100, 34);
    let row = screen
        .lines()
        .find(|l| l.contains("daily-triage") && l.starts_with('│'))
        .unwrap();
    assert!(
        row.contains("daily-triage proj"),
        "the row names the workspace: {row}"
    );
    let title = screen.lines().next().unwrap();
    assert!(
        title.contains("daily-triage · proj · Claude Code"),
        "{title}"
    );
    assert!(
        !title.contains('…'),
        "no cut-off path in the title: {title}"
    );
    app.refresh_loop_cards(Instant::now());
    let screen = render(&app, 100, 34);
    assert!(
        screen
            .lines()
            .any(|l| l.contains(" Every ") && l.contains("1d · ") && l.contains("proj")),
        "the cadence and the full path share a row\n{screen}"
    );
    // the preview's hint line keeps whole hints on one row
    assert!(
        !screen
            .lines()
            .any(|l| l.contains("│kill switch") || l.contains("│loops view")),
        "a hint wrapped:\n{screen}"
    );
    let last = screen.lines().last().unwrap().trim_end();
    assert!(last.ends_with("[?] help"), "{last}");
}

/// The preview leads with what the loop found, then what it may do and
/// what stands in the way, in words rather than level codes; the Setup
/// tab lists what each step needs.
#[test]
fn the_card_says_what_the_loop_may_do_and_setup_says_what_it_needs() {
    let (mut app, temp) = app_with(vec![profile("Claude Code", "claude")]);
    app.loop_registry.add(entry(&temp, "daily-triage"));
    app.sidebar_section = SidebarSection::Loops;
    app.refresh_loop_cards(Instant::now());
    let screen = render(&app, 120, 34);
    assert!(screen.contains("NO RUNS YET"), "the answer leads\n{screen}");
    assert!(
        screen.contains("Allowed to  report only"),
        "the level in words\n{screen}"
    );
    assert!(
        screen.contains("to propose a fix for you to review: "),
        "what the next step needs\n{screen}"
    );
    assert!(
        screen.contains("(now 7)") && screen.contains("a triage skill"),
        "the gap in plain words\n{screen}"
    );
    assert!(
        screen
            .lines()
            .any(|l| l.contains(" Today ") && l.contains("0 of 2 runs")),
        "{screen}"
    );
    assert!(
        screen.contains("6 of 6 files missing") && screen.contains("[E] Setup tab"),
        "setup folds to one line\n{screen}"
    );
    for code in ["Breaker", "ceiling", " L0", "L1 (", "Readiness  "] {
        assert!(!screen.contains(code), "{code:?} left the card\n{screen}");
    }

    app.handle_key(&key(KeyCode::Char('E')), Instant::now());
    app.handle_key(&key(KeyCode::Char('3')), Instant::now());
    let setup = render(&app, 120, 100);
    assert!(setup.contains("What this loop may do"), "{setup}");
    assert!(setup.contains("✗ report only  ← set"), "{setup}");
    assert!(setup.contains("needs a state file"), "{setup}");
    assert!(
        setup.contains("✗ propose a fix for you to review"),
        "{setup}"
    );
    assert!(setup.contains("Readiness █"), "{setup}");
    assert!(setup.contains("Budget"), "{setup}");
    assert!(setup.contains("Files · "), "{setup}");
    assert!(
        !setup.contains("needs L"),
        "no level codes in the needs lines\n{setup}"
    );
}

/// `I` gathers what waits on a human across loops and workflows: a loop
/// run to decide, a plan to review, a workflow run that did not finish.
/// The status bar counts them; each is decided, opened or dismissed in
/// place, and the Loops view has no inbox tab of its own any more.
#[test]
fn the_inbox_gathers_loop_runs_plans_and_failed_runs() {
    use agent_mux::app::inbox::InboxItem;
    use agent_mux::app::workflows::{PlannedWorkflow, RecentWorkflowRun};
    let (mut app, temp) = app_with(vec![profile("Claude Code", "claude")]);
    let db = temp.path().join("traces.db");
    let _store =
        agent_mux::tracing::store::open_rw(&db, agent_mux::tracing::store::OpenOptions::default())
            .unwrap();
    app.trace_db_path = Some(db.clone());
    let e = entry(&temp, "pr-babysitter");
    let loop_id = e.id.clone();
    app.loop_registry.loops.push(e);
    store_run(
        &db,
        &loop_id,
        "2026-09-17T17:36:53Z",
        agent_mux::loops::Outcome::Escalated,
        serde_json::json!({"summary": "two PRs need a human"}),
    );
    app.planned_workflows.push(PlannedWorkflow {
        id: "plan-1".into(),
        task: "audit the parser".into(),
        workspace: temp.path().to_path_buf(),
        harness: agent_mux::harness::Harness::Claude,
        profile: None,
        budget_tokens: None,
        name: "parser-audit".into(),
        document: "[workflow]\nname = \"parser-audit\"\n".into(),
        problems: Vec::new(),
        raw: None,
        run_id: None,
    });
    app.recent_workflow_runs.push(RecentWorkflowRun {
        run_id: "run-00000007".into(),
        name: "migrate".into(),
        harness: "claude".into(),
        status: "failed".into(),
        sessions: 3,
        tokens: 0,
        cost_usd: 0.0,
        ended_at: time::OffsetDateTime::now_utc(),
        result: serde_json::Value::Null,
        error: Some("budget exceeded".into()),
        notes: Vec::new(),
    });
    app.refresh_loop_cards(Instant::now());
    assert_eq!(app.inbox_count(), 3);
    let screen = render(&app, 120, 34);
    let last = screen.lines().last().unwrap();
    assert!(last.starts_with("● 3 need you [I]"), "{last}");

    app.handle_key(&key(KeyCode::Char('I')), Instant::now());
    let Mode::Inbox(state) = &app.mode else {
        panic!("not the inbox: {:?}", app.mode)
    };
    assert!(matches!(state.items[0], InboxItem::Loop(_)), "loops first");
    let screen = render(&app, 120, 34);
    assert!(screen.contains("Needs you (3)"), "{screen}");
    assert!(screen.contains(" Loops"), "{screen}");
    assert!(screen.contains(" Workflows"), "{screen}");
    assert!(screen.contains("pr-babysitter @ w"), "{screen}");
    assert!(
        screen.contains("[a] done"),
        "a run with no branch is done or dismissed, not applied\n{screen}"
    );

    // the failed run is dismissed, not deleted
    app.handle_key(&key(KeyCode::End), Instant::now());
    let screen = render(&app, 120, 34);
    assert!(screen.contains("FAILED · migrate"), "{screen}");
    app.handle_key(&key(KeyCode::Char('x')), Instant::now());
    assert_eq!(app.inbox_count(), 2);

    // a loop run is decided in place
    app.handle_key(&key(KeyCode::Home), Instant::now());
    app.handle_key(&key(KeyCode::Char('a')), Instant::now());
    let Mode::Inbox(state) = &app.mode else {
        panic!()
    };
    assert_eq!(state.items.len(), 1, "{:?}", app.notice);

    // Enter on the plan opens the Workflows view on it
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    let Mode::WorkflowsView(v) = &app.mode else {
        panic!("not the view: {:?}", app.mode)
    };
    assert_eq!(
        v.selected_row(),
        Some(&agent_mux::app::workflows_view::RunRow::Planned(
            "plan-1".into()
        ))
    );

    // the Loops view keeps three tabs
    app.mode = Mode::Control;
    app.handle_key(&key(KeyCode::Char('E')), Instant::now());
    let screen = render(&app, 120, 34);
    assert!(screen.contains("Report | History | Setup"), "{screen}");
    assert!(!screen.contains("Inbox |"), "{screen}");
}
