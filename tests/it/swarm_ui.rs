//! The Swarm screen drawn into a test terminal and driven through the App
//! (`app::swarm_board`, `ui::swarm_board`), in the Agents layout: a plan in
//! review drawn as a tree beside the swarm's card with what needs you, `y` approving only a plan without
//! errors, `d` deleting a draft task, `y` merging a finished branch, the
//! launch card and form, and a refresh only when the store moved.

#![cfg(unix)]

use agent_mux::app::swarm_board::{Modal, SwarmBoard};
use agent_mux::app::{App, Mode};
use agent_mux::bus::TaskIsolation;
use agent_mux::bus::TaskStatus;
use agent_mux::bus::store as bstore;
use agent_mux::bus::swarm::plan::{Plan, PlannedTask};
use agent_mux::bus::swarm::record::{self, NewSwarm, SwarmRun, SwarmStatus};
use agent_mux::config::Profile;
use agent_mux::harness::Harness;
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::{OpenOptions, now_ns, open_aux, open_rw};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
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

struct Fixture {
    _dir: tempfile::TempDir,
    ws: PathBuf,
    db: PathBuf,
    app: App,
}

/// A store, a workspace and an App whose only harness is a fake `claude`
/// given by its absolute path (installed, whatever this machine has).
fn fixture() -> Fixture {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("traces.db");
    let _ = open_rw(
        &db,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "r".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    let ws = dir.path().join("my-project");
    std::fs::create_dir_all(&ws).unwrap();
    let ws = ws.canonicalize().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (tx, _rx) = mpsc::channel(256);
    let mut app = App::new(
        vec![Profile {
            name: "Claude Code".into(),
            command: claude.to_string_lossy().into_owned(),
            args: vec![],
            default_dir: Some(ws.to_string_lossy().into_owned()),
            tracing: None,
            model: None,
            bypass_approvals: None,
        }],
        None,
        tx,
    );
    app.trace_db_path = Some(db.clone());
    app.library_root = Some(dir.path().join("library"));
    Fixture {
        _dir: dir,
        ws,
        db,
        app,
    }
}

fn task(key: &str, deps: &[&str]) -> PlannedTask {
    PlannedTask {
        key: key.into(),
        title: format!("Do {key}"),
        instructions: format!("Instructions for {key}."),
        acceptance: Some(format!("{key} works")),
        files: vec![format!("src/{key}.rs")],
        depends_on: deps.iter().map(|s| s.to_string()).collect(),
        harness: Some("claude".into()),
        ..Default::default()
    }
}

/// A swarm in `status` with `tasks` on its board (another TUI's, so this
/// App's dispatcher leaves it alone).
fn seed(f: &Fixture, status: SwarmStatus, tasks: Vec<PlannedTask>) -> SwarmRun {
    let conn = open_aux(&f.db).unwrap();
    let swarm = record::create_swarm(
        &conn,
        None,
        &NewSwarm {
            workspace: f.ws.to_string_lossy().into_owned(),
            goal: "Ship the follow-ups".into(),
            planner_harness: "claude".into(),
            max_workers: 2,
            isolation: TaskIsolation::Shared,
            gate: true,
            check_cmd: None,
            owner_run: Some("another-tui".into()),
            worker_pool: vec![],
            headless: false,
        },
        now_ns(),
    )
    .unwrap();
    record::replace_draft(
        &conn,
        None,
        &swarm,
        &Plan {
            summary: "one task per subsystem".into(),
            tasks,
        },
        now_ns(),
    )
    .unwrap();
    record::set_plan_result(
        &conn,
        None,
        &swarm.id,
        Some("one task per subsystem"),
        None,
        status,
        now_ns(),
    )
    .unwrap();
    record::get_swarm(&conn, &swarm.id).unwrap().unwrap()
}

fn open_screen(f: &mut Fixture) {
    let conn = open_aux(&f.db).unwrap();
    let view = SwarmBoard::open(
        f.ws.clone(),
        Some(conn),
        &[],
        None,
        vec![Harness::Claude],
        Harness::Claude,
        now_ns(),
    );
    f.app.mode = Mode::Swarm(Box::new(view));
    // As `open_swarm_view` does: a second read, with the plan's checks.
    f.app.handle_key(
        &KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        Instant::now(),
    );
}

fn view(app: &App) -> &SwarmBoard {
    match &app.mode {
        Mode::Swarm(v) => v,
        other => panic!("not on the Swarm screen: {other:?}"),
    }
}

fn press(f: &mut Fixture, c: KeyCode) {
    f.app.handle_key(&key(c), Instant::now());
}

fn notice(app: &App) -> String {
    app.notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default()
}

#[test]
fn a_plan_in_review_is_drawn_as_a_tree_beside_the_swarms_card() {
    let mut f = fixture();
    let mut bad = task("docs", &[]);
    bad.harness = Some("gemini".into());
    seed(
        &f,
        SwarmStatus::Review,
        vec![task("schema", &[]), task("queries", &["schema"]), bad],
    );
    open_screen(&mut f);
    let out = render(&f.app, 140, 32);
    if std::env::var_os("SWARM_UI_PRINT").is_some() {
        println!("{out}");
    }
    for want in [
        // the tree, where the Agents list is
        "Swarms • Keyboard focus",
        "▾ ◆ Ship the follow-ups  ⚑1",
        "· 1 schema",
        "· 2 queries",
        "· 3 docs",
        "+ new swarm  n",
        // the swarm's card in the main pane
        "◆ Ship the follow-ups",
        "review · 0/3 done · 0/2 workers",
        "── Needs you (1) ──",
        "the plan waits for your go in the coordinator's chat: 3 task(s), 1 error(s) to fix first",
        "one task per subsystem",
        "waiting for you to confirm in the coordinator's chat",
        // the swarm row's tabs and the plan as a table with each task's word
        "1 Board   2 Live   3 Timeline   4 Log",
        "· waiting    1 schema",
        "· waiting    3 docs",
        // one footer: the global one
        "[F1 Commands] Swarm",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    assert!(!out.contains("NEEDS YOU"), "no strip of its own:\n{out}");
    // The Timeline tab: one lane per agent against the swarm's clock; the
    // Log tab: the swarm's events, filtered by kind (t) and text (/).
    press(&mut f, KeyCode::Char('3'));
    let out = render(&f.app, 140, 32);
    for want in [
        "Timeline · 1×",
        "◆ Ship the follow-ups",
        "· schema · draft",
        "█ working  ░ alive",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    press(&mut f, KeyCode::Char('4'));
    let out = render(&f.app, 140, 32);
    for want in ["4 Log (1)", " all ", " decisions ", "started"] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    press(&mut f, KeyCode::Tab);
    assert!(view(&f.app).in_log());
    press(&mut f, KeyCode::Char('/'));
    for c in "nothing-like-this".chars() {
        press(&mut f, KeyCode::Char(c));
    }
    assert!(f.app.text_entry_active(), "typing the filter is text entry");
    press(&mut f, KeyCode::Enter);
    let v = view(&f.app);
    assert_eq!(v.log_filter, "nothing-like-this");
    assert!(v.log_entries().is_empty(), "{:?}", v.log_entries());
    let out = render(&f.app, 140, 32);
    assert!(out.contains("No event matches."), "{out}");
    press(&mut f, KeyCode::Char('t'));
    assert_eq!(
        view(&f.app).log_kind,
        agent_mux::tracing::swarm_report::LogKind::Decisions
    );
    // Esc: back to the tree; 1: the Board again
    press(&mut f, KeyCode::Esc);
    assert!(!view(&f.app).in_log());
    press(&mut f, KeyCode::Char('1'));
    for width in [80u16, 120, 180] {
        let out = render(&f.app, width, 24);
        for want in ["Swarms", "◆ Ship", "1 schema", "Needs you (1)"] {
            assert!(
                out.contains(want),
                "{width} columns: missing {want:?} in:\n{out}"
            );
        }
    }
    // A task: its card, and the task with an error is marked in the tree.
    f.app.handle_key(&key(KeyCode::Down), Instant::now());
    let out = render(&f.app, 140, 32);
    for want in [
        "schema · draft · Do schema",
        "1 Task",
        "Instructions for schema.",
        "── Done when ──",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let docs_row = out.lines().find(|l| l.contains("3 docs")).unwrap();
    assert!(docs_row.contains("✗ gemini"), "{docs_row}");
    press(&mut f, KeyCode::Char('G'));
    let out = render(&f.app, 140, 32);
    assert!(
        out.contains("✗ error: harness `gemini` is not installed"),
        "{out}"
    );
}

#[test]
fn y_never_approves_a_plan_it_points_at_the_chat_and_the_plan_keys_are_gone() {
    let mut f = fixture();
    let mut bad = task("docs", &[]);
    bad.harness = Some("gemini".into());
    seed(&f, SwarmStatus::Review, vec![task("schema", &[]), bad]);
    open_screen(&mut f);

    press(&mut f, KeyCode::Char('y'));
    assert!(view(&f.app).confirm.is_none());
    assert_eq!(
        notice(&f.app),
        "fix the plan first: docs: harness `gemini` is not installed"
    );

    // d, e and p no longer edit the plan: that is said to the coordinator.
    press(&mut f, KeyCode::Char('G'));
    press(&mut f, KeyCode::Char('d'));
    assert!(view(&f.app).confirm.is_none());
    assert!(
        notice(&f.app).contains("said to the coordinator"),
        "{}",
        notice(&f.app)
    );
    assert_eq!(view(&f.app).board.len(), 2);

    // A clean plan: y does not approve it (coordinator-first §5.3); it says
    // where the confirmation happens and puts the cursor on the coordinator.
    let mut f = fixture();
    let swarm = seed(&f, SwarmStatus::Review, vec![task("schema", &[])]);
    open_screen(&mut f);
    press(&mut f, KeyCode::Char('y'));
    assert!(view(&f.app).confirm.is_none());
    assert!(
        notice(&f.app).contains("Confirm the plan in the coordinator's chat"),
        "{}",
        notice(&f.app)
    );
    let conn = open_aux(&f.db).unwrap();
    assert_eq!(
        record::get_swarm(&conn, &swarm.id).unwrap().unwrap().status,
        SwarmStatus::Review
    );
    let first = record::board(&conn, &swarm.id).unwrap()[0].task.id.clone();
    assert!(bstore::plan_blocks(&conn, &first).is_some());
    let out = render(&f.app, 120, 30);
    assert!(
        out.contains("confirm in the coordinator's chat"),
        "the Board says so:\n{out}"
    );
    // The CLI's approve (scripts) still works, recorded as the CLI's.
    f.app
        .swarm_action_on(
            Some(&swarm.id),
            &f.ws,
            agent_mux::bus::swarm::record::SwarmVerb::Approve,
            None,
            None,
            &agent_mux::bus::swarm::record::Decider::CLI,
        )
        .unwrap();
    assert_eq!(
        record::get_swarm(&conn, &swarm.id).unwrap().unwrap().status,
        SwarmStatus::Running
    );
    let d = record::launch_decision(&conn, &swarm.id).unwrap();
    assert_eq!(d.by, agent_mux::bus::swarm::record::DecidedBy::Cli);
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn y_merges_a_finished_branch_into_the_workspace() {
    let mut f = fixture();
    git(&f.ws, &["init", "-q", "-b", "main"]);
    std::fs::write(f.ws.join("a.txt"), "one\n").unwrap();
    git(&f.ws, &["add", "."]);
    git(&f.ws, &["commit", "-qm", "init"]);
    git(&f.ws, &["checkout", "-qb", "loop/bus-a"]);
    std::fs::write(f.ws.join("b.txt"), "two\n").unwrap();
    git(&f.ws, &["add", "."]);
    git(&f.ws, &["commit", "-qm", "the work"]);
    git(&f.ws, &["checkout", "-q", "main"]);

    let swarm = seed(&f, SwarmStatus::Integrating, vec![task("a", &[])]);
    let conn = open_aux(&f.db).unwrap();
    let board = record::board(&conn, &swarm.id).unwrap();
    bstore::update_task(
        &conn,
        None,
        &board[0].task.id,
        "worker-1",
        TaskStatus::Done,
        Some(serde_json::json!("added b")),
        None,
        None,
        Some("loop/bus-a".into()),
        now_ns(),
    )
    .unwrap();
    open_screen(&mut f);
    let out = render(&f.app, 120, 30);
    assert!(out.contains("review    a finished on loop/bus-a"), "{out}");
    assert!(out.contains("⇡ 1 a"), "{out}");

    press(&mut f, KeyCode::Down);
    press(&mut f, KeyCode::Char('y'));
    let out = render(&f.app, 120, 30);
    assert!(
        out.contains("Merge loop/bus-a into the workspace's branch"),
        "{out}"
    );
    press(&mut f, KeyCode::Char('y'));
    assert!(
        notice(&f.app).starts_with("merged loop/bus-a ("),
        "{}",
        notice(&f.app)
    );
    assert!(
        f.ws.join("b.txt").exists(),
        "the branch's work is in the workspace"
    );
    let reviews = bstore::reviews(&conn, &swarm.workspace).unwrap();
    assert_eq!(reviews[0].verdict, "merged");
    assert_eq!(view(&f.app).board[0].review.as_deref(), Some("merged"));
}

#[test]
fn with_no_swarm_the_screen_offers_one_and_the_form_starts_it() {
    let mut f = fixture();
    open_screen(&mut f);
    let out = render(&f.app, 120, 30);
    assert!(out.contains("No swarm in this workspace."), "{out}");
    assert!(out.contains("no swarms: n starts one"), "{out}");
    press(&mut f, KeyCode::Enter);
    assert!(matches!(view(&f.app).modal, Some(Modal::Launch { .. })));
    let out = render(&f.app, 120, 30);
    for want in [
        "New swarm",
        "Coordinator: ◂ claude ▸",
        "Tell it the goal there",
        "▸ Advanced: goal now, workers' pool",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    assert!(!out.contains("Max parallel workers"), "folded:\n{out}");
    // Advanced unfolds the limits.
    press(&mut f, KeyCode::Tab);
    press(&mut f, KeyCode::Char(' '));
    let out = render(&f.app, 120, 36);
    for want in [
        "▾ Advanced",
        "Goal (optional here",
        "Workers may run on: [x] claude",
        "Max parallel workers: 3",
        "Plan approval: in the coordinator's chat",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    press(&mut f, KeyCode::Esc);
    assert!(view(&f.app).modal.is_none());
}

#[test]
fn the_screen_refreshes_only_when_the_store_or_a_session_moved() {
    let f = fixture();
    let swarm = seed(&f, SwarmStatus::Review, vec![task("a", &[])]);
    let conn = open_aux(&f.db).unwrap();
    let mut v = SwarmBoard::open(
        f.ws.clone(),
        Some(open_aux(&f.db).unwrap()),
        &[],
        None,
        vec![Harness::Claude],
        Harness::Claude,
        now_ns(),
    );
    let t0 = Instant::now();
    assert!(!v.refresh_due(&[], t0), "nothing moved");
    assert!(
        !v.refresh_due(&[], t0 + Duration::from_secs(1)),
        "still nothing"
    );
    bstore::send_message(
        &conn,
        None,
        &swarm.workspace,
        "user",
        None,
        "directive",
        "hello",
        None,
        now_ns(),
    )
    .unwrap();
    assert!(
        !v.refresh_due(&[], t0 + Duration::from_millis(1200)),
        "at most one look per half second"
    );
    assert!(
        v.refresh_due(&[], t0 + Duration::from_secs(2)),
        "a message moved the store"
    );
    v.refresh(&[], None, now_ns());
    assert_eq!(v.messages.len(), 1);
    assert!(!v.refresh_due(&[], t0 + Duration::from_secs(3)));
}

fn pump(
    rx: &mut mpsc::Receiver<agent_mux::events::AppEvent>,
    app: &mut App,
    ms: u64,
    mut done: impl FnMut(&App) -> bool,
) -> bool {
    use agent_mux::events::AppEvent;
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        if done(app) {
            return true;
        }
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::PtyOutput { id, bytes } => {
                    app.handle_pty_output(id, &bytes, Instant::now())
                }
                AppEvent::PtyExit { id } => app.handle_pty_exit(id),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done(app)
}

#[test]
fn a_workers_terminal_fills_the_pane_and_enter_types_into_it() {
    let mut f = fixture();
    // A plain shell is the worker of the first task.
    let (tx, mut rx) = mpsc::channel(256);
    let mut app = App::new(
        vec![Profile {
            name: "shell".into(),
            command: "sh".into(),
            args: vec![],
            default_dir: Some(f.ws.to_string_lossy().into_owned()),
            tracing: None,
            model: None,
            bypass_approvals: None,
        }],
        None,
        tx,
    );
    app.trace_db_path = Some(f.db.clone());
    app.set_pane_size(30, 108);
    app.focus_list();
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert_eq!(app.sessions.len(), 1);
    let launch = format!("session-{}", app.sessions[0].id);
    let swarm = seed(&f, SwarmStatus::Running, vec![task("schema", &[])]);
    let conn = open_aux(&f.db).unwrap();
    let board = record::board(&conn, &swarm.id).unwrap();
    conn.execute(
        "UPDATE agent_tasks SET status = 'running', assigned_to = ?1 WHERE id = ?2",
        rusqlite::params![launch, board[0].task.id],
    )
    .unwrap();
    assert!(pump(&mut rx, &mut app, 5000, |a| !a.sessions[0]
        .parser
        .screen()
        .contents()
        .trim()
        .is_empty()));
    f.app = app;
    open_screen(&mut f);
    // ↓ to the task: its live terminal in the main pane, at the pane's size.
    press(&mut f, KeyCode::Down);
    let v = view(&f.app);
    assert_eq!(v.pane(), agent_mux::app::swarm_board::PaneView::Session(0));
    let out = render(&f.app, 140, 32);
    assert!(out.contains("shell — "), "the Agents pane's title:\n{out}");
    // Enter gives it the keyboard; what is typed reaches the shell.
    press(&mut f, KeyCode::Enter);
    assert!(view(&f.app).pane_focused);
    for c in "echo swarm-pane-$((6*7))".chars() {
        press(&mut f, KeyCode::Char(c));
    }
    press(&mut f, KeyCode::Enter);
    assert!(
        pump(&mut rx, &mut f.app, 5000, |a| a.sessions[0]
            .parser
            .screen()
            .contents()
            .contains("swarm-pane-42")),
        "{}",
        f.app.sessions[0].parser.screen().contents()
    );
    let out = render(&f.app, 240, 32);
    assert!(out.contains("Keyboard goes to shell"), "{out}");
    // The focus chord takes the keyboard back to the tree.
    f.app.handle_key(
        &KeyEvent::new(
            KeyCode::Char('L'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Instant::now(),
    );
    assert!(!view(&f.app).pane_focused);
    assert!(
        matches!(f.app.mode, Mode::Swarm(_)),
        "still on the Swarm screen"
    );
    // The swarm row's Live tab is the mosaic: one tile per live terminal,
    // showing the bottom of its screen; Enter on a tile connects to it.
    press(&mut f, KeyCode::Char('g'));
    press(&mut f, KeyCode::Char('2'));
    let out = render(&f.app, 140, 32);
    assert!(out.contains("2 Live (1)"), "{out}");
    assert!(out.contains("1 schema · "), "the tile's title:\n{out}");
    assert!(
        out.contains("swarm-pane-42"),
        "the tile shows the screen:\n{out}"
    );
    press(&mut f, KeyCode::Tab);
    assert!(view(&f.app).in_mosaic());
    press(&mut f, KeyCode::Enter);
    assert!(view(&f.app).pane_focused);
    assert_eq!(
        view(&f.app).pane(),
        agent_mux::app::swarm_board::PaneView::Session(0)
    );
    f.app.kill_all();
}

#[test]
fn a_swarm_that_needs_you_shows_on_the_agents_screen() {
    let mut f = fixture();
    let conn = open_aux(&f.db).unwrap();
    let swarm = seed(&f, SwarmStatus::Review, vec![task("schema", &[])]);
    // this TUI dispatches it
    record::set_owner(&conn, &swarm.id, &f.app.app_run_id).unwrap();
    f.app.on_tick(Instant::now());
    assert_eq!(f.app.swarm_needs, 1, "the plan waits for approval");
    let out = render(&f.app, 140, 44);
    assert!(out.contains("⚑ swarm needs you (1)"), "{out}");
    assert!(out.contains("⚑ swarm 1"), "the status bar:\n{out}");
}

#[test]
fn r_on_a_failed_agy_task_moves_it_to_claude_and_puts_it_back() {
    let mut f = fixture();
    let mut t = task("spec", &[]);
    t.harness = Some("agy".into());
    t.agent = Some("doc-writer".into());
    let swarm = seed(&f, SwarmStatus::Running, vec![t]);
    let conn = open_aux(&f.db).unwrap();
    let id = record::board(&conn, &swarm.id).unwrap()[0].task.id.clone();
    bstore::update_task(
        &conn,
        None,
        &id,
        "worker-1",
        TaskStatus::Failed,
        None,
        Some("agy lacks write_file".into()),
        None,
        None,
        now_ns(),
    )
    .unwrap();
    open_screen(&mut f);
    press(&mut f, KeyCode::Down);
    press(&mut f, KeyCode::Char('r'));
    let out = render(&f.app, 140, 40);
    for want in [
        "Retry task",
        "Saving puts the task back on the board",
        "Harness",
        "Agent (empty: none)",
        "Isolation",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    // agy cannot run doc-writer with its tools: the form starts on claude
    // and says why. The agent goes too.
    assert!(
        out.contains("agy runs the `doc-writer` agent read-only")
            && out.contains("moved to claude"),
        "{out}"
    );
    press(&mut f, KeyCode::Tab);
    for _ in 0.."doc-writer".len() {
        press(&mut f, KeyCode::Backspace);
    }
    f.app.handle_key(
        &KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    let t = bstore::get_task(&conn, &id).unwrap().unwrap();
    assert_eq!(
        t.status,
        TaskStatus::Open,
        "back on the board: {:?}",
        notice(&f.app)
    );
    assert_eq!(t.target_harness.as_deref(), Some("claude"));
    assert_eq!(t.target_agent, None);
    assert!(
        notice(&f.app).contains("is back on the board on claude"),
        "{}",
        notice(&f.app)
    );
}

#[test]
fn a_second_swarm_starts_beside_a_stuck_one_and_a_failed_start_stays_in_the_form() {
    let mut f = fixture();
    let stuck = seed(&f, SwarmStatus::Running, vec![task("a", &[])]);
    open_screen(&mut f);
    // `n` on the stuck swarm's row opens the form: no "close it first".
    press(&mut f, KeyCode::Char('n'));
    assert!(
        matches!(view(&f.app).modal, Some(Modal::Launch { .. })),
        "{}",
        notice(&f.app)
    );
    // Enter starts the coordinator at once: the goal is said in its chat.
    let out = render(&f.app, 140, 40);
    assert!(out.contains("Coordinator: ◂ claude ▸"), "{out}");
    press(&mut f, KeyCode::Enter);
    assert!(view(&f.app).modal.is_none(), "{}", notice(&f.app));
    let conn = open_aux(&f.db).unwrap();
    let live = record::live_swarms_in(&conn, &f.ws.to_string_lossy()).unwrap();
    assert_eq!(live.len(), 2, "{}", notice(&f.app));
    let second = live.iter().find(|s| s.id != stuck.id).unwrap();
    assert_eq!(second.goal, "");
    assert!(!second.headless);
    assert_eq!(record::worker_pool(&conn, second), ["claude"]);
    assert!(
        notice(&f.app).contains("say the goal in the claude coordinator's chat"),
        "{}",
        notice(&f.app)
    );
    // The cursor is on the new swarm.
    assert_eq!(
        view(&f.app).swarm.as_ref().map(|s| s.id.clone()),
        Some(second.id.clone())
    );

    // The keyboard went to the coordinator's chat; the focus chord brings it
    // back to the tree.
    assert!(view(&f.app).pane_focused);
    f.app.handle_key(
        &KeyEvent::new(
            KeyCode::Char('L'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Instant::now(),
    );
    assert!(!view(&f.app).pane_focused);

    // A start that fails keeps the form, with the reason drawn in it. `n`
    // on a swarm row (the top one) opens it; on the coordinator it adds a task.
    f.app.trace_db_path = None;
    press(&mut f, KeyCode::Char('g'));
    press(&mut f, KeyCode::Char('n'));
    press(&mut f, KeyCode::Right); // another coordinator, kept on failure
    let chosen = match &view(&f.app).modal {
        Some(Modal::Launch { planner, .. }) => *planner,
        _ => panic!("no form"),
    };
    f.app.handle_key(
        &KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    let out = render(&f.app, 140, 40);
    assert!(
        matches!(
            view(&f.app).modal,
            Some(Modal::Launch { error: Some(_), planner, .. }) if planner == chosen
        ),
        "{out}"
    );
    assert!(out.contains("✗ a swarm needs the trace store"), "{out}");
}

#[test]
fn the_commands_menu_submits_the_launch_form_too() {
    let mut f = fixture();
    open_screen(&mut f);
    press(&mut f, KeyCode::Enter);
    f.app
        .execute_command(agent_mux::keymap::ActionId::SubmitDialog, Instant::now());
    assert!(view(&f.app).modal.is_none(), "{}", notice(&f.app));
    assert!(
        notice(&f.app).contains("started: say the goal in the claude coordinator's chat"),
        "{}",
        notice(&f.app)
    );
    // The keyboard went to the coordinator's chat.
    assert!(view(&f.app).pane_focused, "{}", notice(&f.app));
}

#[test]
fn saving_the_retry_form_at_once_moves_an_agy_writing_task_to_claude() {
    // What the operator did: `r` on a stuck agy doc-writer task, then Ctrl+S.
    let mut f = fixture();
    let mut t = task("spec", &[]);
    t.harness = Some("agy".into());
    t.agent = Some("doc-writer".into());
    let swarm = seed(&f, SwarmStatus::Running, vec![t]);
    let conn = open_aux(&f.db).unwrap();
    let id = record::board(&conn, &swarm.id).unwrap()[0].task.id.clone();
    bstore::update_task(
        &conn,
        None,
        &id,
        "worker-1",
        TaskStatus::Failed,
        None,
        Some("agy lacks write_file".into()),
        None,
        None,
        now_ns(),
    )
    .unwrap();
    open_screen(&mut f);
    press(&mut f, KeyCode::Down);
    press(&mut f, KeyCode::Char('r'));
    f.app.handle_key(
        &KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    let t = bstore::get_task(&conn, &id).unwrap().unwrap();
    assert_eq!(t.status, TaskStatus::Open, "{}", notice(&f.app));
    assert_eq!(t.target_harness.as_deref(), Some("claude"));
    assert_eq!(
        t.target_agent.as_deref(),
        Some("doc-writer"),
        "kept on claude"
    );
}

#[test]
fn an_agy_worker_with_a_writing_agent_is_refused_at_spawn() {
    let mut f = fixture();
    let resp = f.app.handle_bus_control(
        agent_mux::bus::control::ControlRequest::SpawnWorker {
            parent_launch: None,
            workspace: f.ws.to_string_lossy().into_owned(),
            task_id: "task-x".into(),
            title: "Write the spec".into(),
            instructions: "Write it".into(),
            harness: Some("agy".into()),
            agent: Some("doc-writer".into()),
            isolation: TaskIsolation::Shared,
            opening_prompt: None,
            swarm: None,
        },
        Instant::now(),
    );
    assert!(!resp.ok);
    assert!(
        resp.error
            .as_deref()
            .unwrap_or_default()
            .contains("read-only"),
        "{resp:?}"
    );
    assert!(f.app.sessions.is_empty(), "no worker started");
}

/// An older swarm left open in another folder with something for the
/// operator, and a newer one running in this folder.
fn an_old_swarm_that_needs_you_elsewhere(f: &mut Fixture) -> (SwarmRun, SwarmRun) {
    let conn = open_aux(&f.db).unwrap();
    let old = seed(f, SwarmStatus::Review, vec![task("old", &[])]);
    let elsewhere = f.ws.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    conn.execute(
        "UPDATE swarms SET workspace = ?1 WHERE id = ?2",
        rusqlite::params![elsewhere.to_string_lossy(), old.id],
    )
    .unwrap();
    record::set_owner(&conn, &old.id, &f.app.app_run_id).unwrap();
    let new = seed(f, SwarmStatus::Running, vec![task("new", &[])]);
    f.app.on_tick(Instant::now());
    assert_eq!(f.app.swarm_needs_first.as_deref(), Some(old.id.as_str()));
    (old, new)
}

#[test]
fn f1_open_swarm_opens_the_folders_swarm_before_an_older_one_that_needs_you() {
    let mut f = fixture();
    let (_old, new) = an_old_swarm_that_needs_you_elsewhere(&mut f);
    // a plain session in this folder, in no swarm, is selected
    f.app.focus_list();
    press(&mut f, KeyCode::Char('n'));
    press(&mut f, KeyCode::Enter);
    press(&mut f, KeyCode::Enter);
    assert_eq!(f.app.sessions.len(), 1, "{}", notice(&f.app));
    f.app
        .execute_command(agent_mux::keymap::ActionId::Swarm, Instant::now());
    assert_eq!(
        view(&f.app).swarm.as_ref().map(|s| s.id.clone()),
        Some(new.id.clone()),
        "the run in this folder, not the old one"
    );
    f.app.kill_all();
}

#[test]
fn with_several_swarms_that_need_you_the_newest_is_the_one_to_open() {
    let mut f = fixture();
    let conn = open_aux(&f.db).unwrap();
    let older = seed(&f, SwarmStatus::Review, vec![task("a", &[])]);
    let newer = seed(&f, SwarmStatus::Review, vec![task("b", &[])]);
    record::set_owner(&conn, &older.id, &f.app.app_run_id).unwrap();
    record::set_owner(&conn, &newer.id, &f.app.app_run_id).unwrap();
    f.app.on_tick(Instant::now());
    assert_eq!(f.app.swarm_needs, 2);
    assert_eq!(f.app.swarm_needs_first.as_deref(), Some(newer.id.as_str()));
}

#[test]
fn a_swarm_row_keeps_the_cursor_when_a_session_elsewhere_ends() {
    use agent_mux::app::agents_list::AgentKind;
    let mut f = fixture();
    f.app.clean = false;
    for _ in 0..2 {
        f.app.focus_list();
        press(&mut f, KeyCode::Char('n'));
        press(&mut f, KeyCode::Enter);
        press(&mut f, KeyCode::Enter);
    }
    assert_eq!(f.app.sessions.len(), 2, "{}", notice(&f.app));
    f.app.sessions[0].group = Some(agent_mux::tree::GroupRef {
        kind: agent_mux::tree::GroupKind::Swarm,
        id: "swarm-1".into(),
        title: "the web run".into(),
        detail: "1".into(),
        label: "w1".into(),
    });
    f.app.selected = 0;
    let lines = f.app.agent_lines();
    let at = lines
        .iter()
        .position(|l| matches!(&l.kind, AgentKind::Run(k) if k.starts_with("swarm:")))
        .expect("a swarm group row");
    f.app.select_agent_line(&lines, at);
    // the other session ends; the selected one is still selected
    let mut gone = f.app.sessions.remove(1);
    gone.kill();
    assert!(
        matches!(f.app.agent_row(), Some(AgentKind::Run(k)) if k.starts_with("swarm:")),
        "{:?}",
        f.app.agent_row()
    );
    f.app.kill_all();
}
