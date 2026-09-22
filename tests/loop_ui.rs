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
    let (active, agents, loops, workflows, history) = agent_mux::ui::sidebar_areas(33, 1, 0, 0);
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
    let (a, b, c, w, d) = agent_mux::ui::sidebar_areas(13, 1, 0, 0);
    assert_eq!(
        [a.height, b.height, c.height, w.height, d.height],
        [3, 2, 2, 2, 3]
    );
    let (a, b, c, w, d) = agent_mux::ui::sidebar_areas(16, 1, 0, 0);
    assert_eq!(
        [a.height, b.height, c.height, w.height, d.height],
        [3, 3, 3, 2, 4]
    );

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
    assert!(screen.contains("Runs"), "{screen}");
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
    assert!(out.contains("not required at L1"), "{out}");
    assert!(out.contains("exit 0"), "the run facts are on screen\n{out}");
}
