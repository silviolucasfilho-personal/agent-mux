//! The Swarm screen drawn into a test terminal and driven through the App
//! (`app::swarm_board`, `ui::swarm_board`), in the Agents layout: a plan in
//! review drawn as a tree beside the swarm's card with what needs you, `y` approving only a plan without
//! errors, `d` deleting the swarm (screen and sidebar), `y` merging a finished branch, the
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
            review: agent_mux::bus::swarm::review::ReviewSettings::off(),
            merge_gate: Default::default(),
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
            review: None,
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

    // e and p no longer edit the plan: that is said to the coordinator.
    press(&mut f, KeyCode::Char('G'));
    for c in ['e', 'p'] {
        press(&mut f, KeyCode::Char(c));
        assert!(view(&f.app).confirm.is_none(), "{c}");
        assert!(
            notice(&f.app).contains("said to the coordinator"),
            "{c}: {}",
            notice(&f.app)
        );
    }
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
        "Reviews: auto · another harness than the worker's",
        "▸ Advanced: goal now, workers' pool",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    assert!(!out.contains("Max parallel workers"), "folded:\n{out}");
    // Advanced unfolds the limits (past the review).
    press(&mut f, KeyCode::Tab);
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

/// The periodic read runs on a thread: the screen keeps its rows (and the
/// keyboard) until it lands, then shows what a synchronous read shows; a
/// synchronous read in between makes the older background one stale.
#[test]
fn the_periodic_read_runs_off_the_ui_thread() {
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
    let send = |body: &str| {
        bstore::send_message(
            &conn,
            None,
            &swarm.workspace,
            "user",
            None,
            "directive",
            body,
            None,
            now_ns(),
        )
        .unwrap();
    };
    let wait = |v: &mut SwarmBoard| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while v.loading() {
            assert!(
                Instant::now() < deadline,
                "the background read never landed"
            );
            if v.poll_refresh() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    };
    send("one");
    v.refresh_in_background(&[], None, now_ns(), f.db.clone());
    assert!(v.loading());
    assert!(
        !v.refresh_due(&[], Instant::now() + Duration::from_secs(5)),
        "one read at a time"
    );
    assert!(wait(&mut v), "the read is applied");
    assert_eq!(v.messages.len(), 1);
    assert_eq!(v.board.len(), 1);
    assert_eq!(
        v.swarm.as_ref().map(|s| s.id.clone()),
        Some(swarm.id.clone())
    );

    // a synchronous read after it was started wins
    v.refresh_in_background(&[], None, now_ns(), f.db.clone());
    send("two");
    v.refresh(&[], None, now_ns());
    assert_eq!(v.messages.len(), 2);
    assert!(!v.loading(), "the older read is dropped");
    assert!(!v.poll_refresh());
    assert_eq!(v.messages.len(), 2);
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
fn a_swarm_session_keeps_the_cursor_when_a_session_elsewhere_ends() {
    use agent_mux::app::agents_list::AgentKind;
    let mut f = fixture();
    f.app.agents.show_swarm = true;
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
    // with show_swarm the worker gathers under its swarm's row
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

#[test]
fn swarm_sessions_stay_off_the_agents_list_unless_the_option_says_so() {
    use agent_mux::app::agents_list::AgentKind;
    let mut f = fixture();
    for _ in 0..2 {
        f.app.focus_list();
        press(&mut f, KeyCode::Char('n'));
        press(&mut f, KeyCode::Enter);
        press(&mut f, KeyCode::Enter);
    }
    assert_eq!(f.app.sessions.len(), 2, "{}", notice(&f.app));
    f.app.sessions[1].group = Some(agent_mux::tree::GroupRef {
        kind: agent_mux::tree::GroupKind::Swarm,
        id: "swarm-1".into(),
        title: "the web run".into(),
        detail: "1".into(),
        label: "w1".into(),
    });
    let listed = |app: &agent_mux::app::App| -> Vec<usize> {
        app.agent_lines()
            .iter()
            .filter_map(|l| match l.kind {
                AgentKind::Session(i) => Some(i),
                _ => None,
            })
            .collect()
    };
    let swarm_row = |app: &agent_mux::app::App| {
        app.agent_lines()
            .iter()
            .any(|l| matches!(&l.kind, AgentKind::Run(k) if k.starts_with("swarm:")))
    };
    // the default: the worker is on the Swarm screen only
    assert_eq!(listed(&f.app), vec![0]);
    assert!(!swarm_row(&f.app));
    // a selected worker leaves the cursor on a listed session
    f.app.selected = 1;
    assert_eq!(f.app.agent_row(), Some(AgentKind::Session(0)));
    // stepping through sessions with the sidebar hidden skips it
    f.app.selected = 0;
    f.app.sidebar_hidden = true;
    f.app.focus_list();
    press(&mut f, KeyCode::Down);
    assert_eq!(f.app.selected, 0);
    // `[agents] show_swarm = true` steps onto it and lists it under its harness
    f.app.agents.show_swarm = true;
    press(&mut f, KeyCode::Down);
    assert_eq!(f.app.selected, 1);
    f.app.sidebar_hidden = false;
    assert_eq!(listed(&f.app), vec![0, 1]);
    assert!(swarm_row(&f.app));
    f.app.kill_all();
}

#[test]
fn o_picks_another_swarm_to_show() {
    let mut f = fixture();
    let older = seed(&f, SwarmStatus::Review, vec![task("a", &[])]);
    let newer = seed(&f, SwarmStatus::Review, vec![task("b", &[])]);
    open_screen(&mut f);
    assert_eq!(view(&f.app).scope.as_deref(), Some(newer.id.as_str()));

    press(&mut f, KeyCode::Char('o'));
    let Some(Modal::Picker { swarms, cursor }) = &view(&f.app).modal else {
        panic!("no picker: {:?}", view(&f.app).status_msg);
    };
    // the folder's live swarms, newest first, the cursor on the one shown
    let ids: Vec<&str> = swarms.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec![newer.id.as_str(), older.id.as_str()]);
    assert_eq!(*cursor, 0);
    let screen = render(&f.app, 140, 40);
    assert!(screen.contains("Swarms"), "{screen}");
    assert!(
        screen.contains(agent_mux::bus::short_id(&older.id)),
        "{screen}"
    );

    press(&mut f, KeyCode::Down);
    press(&mut f, KeyCode::Enter);
    let v = view(&f.app);
    assert!(v.modal.is_none());
    assert_eq!(v.scope.as_deref(), Some(older.id.as_str()));
    assert_eq!(
        v.swarm.as_ref().map(|s| s.id.as_str()),
        Some(older.id.as_str())
    );
    f.app.kill_all();
}

/// The operator's own session, then a swarm started from the screen: its
/// coordinator's session is the last one.
fn start_swarm_beside_a_session(f: &mut Fixture) -> usize {
    f.app.focus_list();
    press(f, KeyCode::Char('n'));
    press(f, KeyCode::Enter);
    press(f, KeyCode::Enter);
    assert_eq!(f.app.sessions.len(), 1);
    open_screen(f);
    press(f, KeyCode::Enter); // the launch form
    press(f, KeyCode::Enter); // start
    assert_eq!(f.app.sessions.len(), 2, "{}", notice(&f.app));
    1
}

fn back_to_tree(f: &mut Fixture) {
    f.app.handle_key(
        &KeyEvent::new(
            KeyCode::Char('L'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        Instant::now(),
    );
    assert!(!view(&f.app).pane_focused);
}

#[test]
fn the_screen_reopens_on_the_coordinator_and_enter_on_the_swarm_types_into_it() {
    use agent_mux::app::swarm_board::{PaneView, Row};
    let mut f = fixture();
    let coord = start_swarm_beside_a_session(&mut f);
    assert_eq!(f.app.pane_session(), Some(coord));
    back_to_tree(&mut f);
    // leave the screen with another session selected, and come back from F1
    f.app.mode = Mode::Main;
    f.app.selected = 0;
    f.app.open_swarm_view();
    let v = view(&f.app);
    assert_eq!(
        v.rows.get(v.cursor),
        Some(&Row::Coordinator(0)),
        "{:?}",
        v.rows
    );
    assert_eq!(v.pane(), PaneView::Session(coord));
    assert_eq!(
        f.app.selected, coord,
        "the pane's session is the selected one"
    );
    press(&mut f, KeyCode::Enter);
    assert_eq!(f.app.pane_session(), Some(coord));
    back_to_tree(&mut f);
    // Enter on the swarm row goes to the coordinator's chat too
    press(&mut f, KeyCode::Char('g'));
    assert_eq!(
        view(&f.app).rows.get(view(&f.app).cursor),
        Some(&Row::Swarm(0))
    );
    press(&mut f, KeyCode::Enter);
    let v = view(&f.app);
    assert!(v.pane_focused);
    assert_eq!(v.rows.get(v.cursor), Some(&Row::Coordinator(0)));
    assert_eq!(f.app.pane_session(), Some(coord));
    // Alt+letter is the session's, as on the Agents screen
    assert!(view(&f.app).text_entry_active());
    f.app.handle_key(
        &KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT),
        Instant::now(),
    );
    assert_eq!(f.app.pane_session(), Some(coord));
    f.app.kill_all();
}

#[test]
fn a_click_selects_a_tree_row_and_a_click_on_the_terminal_types_into_it() {
    use agent_mux::app::swarm_board::Row;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut f = fixture();
    let coord = start_swarm_beside_a_session(&mut f);
    back_to_tree(&mut f);
    press(&mut f, KeyCode::Char('g'));
    let _ = render(&f.app, 140, 40);
    let click = |f: &mut Fixture, column: u16, row: u16| {
        f.app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            Instant::now(),
        );
    };
    let d = view(&f.app).drawn.get();
    // the tree's second row is the coordinator's
    click(&mut f, d.tree.x + 3, d.tree.y + 2);
    let v = view(&f.app);
    assert_eq!(v.rows.get(v.cursor), Some(&Row::Coordinator(0)));
    assert!(!v.pane_focused);
    let _ = render(&f.app, 140, 40);
    // a click on its terminal gives it the keyboard
    let d = view(&f.app).drawn.get();
    click(&mut f, d.pane.x + 10, d.pane.y + 5);
    assert!(view(&f.app).pane_focused);
    assert_eq!(f.app.pane_session(), Some(coord));
    // a click on the tree takes the keyboard back
    click(&mut f, d.tree.x + 3, d.tree.y + 1);
    let v = view(&f.app);
    assert!(!v.pane_focused);
    assert_eq!(v.rows.get(v.cursor), Some(&Row::Swarm(0)));
    f.app.kill_all();
}

#[test]
fn a_restart_reconnects_the_coordinator_to_its_swarm() {
    use agent_mux::app::swarm_board::PaneView;
    let mut f = fixture();
    let swarm = seed(&f, SwarmStatus::Running, vec![task("a", &[])]);
    let conn = open_aux(&f.db).unwrap();
    // the coordinator as the last run left it: a launch with its swarm
    // metadata and a known conversation
    record::set_coordinator_launch(&conn, None, &swarm.id, "old-coord", now_ns()).unwrap();
    conn.execute(
        "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
         VALUES ('claude:conv-1', 'claude', 'conv-1', ?1, 1, 1)",
        rusqlite::params![f.ws.to_string_lossy()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug,
             content_mode, correlation_plan, session_key, started_ns, ended_ns, agent_mux_version, metadata)
         VALUES ('old-coord', 'r', 1, 'claude · coordinator', 'claude', ?1, 'p', 'metadata', 'known',
             'claude:conv-1', 1, 2, 'test', ?2)",
        rusqlite::params![
            f.ws.to_string_lossy(),
            serde_json::json!({"swarm_id": swarm.id, "swarm_role": "coordinator"}).to_string()
        ],
    )
    .unwrap();
    // a sessions.json written before the link was saved: no conversation
    // known, no swarm (the fallback finds it by profile and folder)
    let file = f.ws.parent().unwrap().join("sessions.json");
    let mut profile = f.app.profiles[0].clone();
    profile.name = "claude · coordinator".into();
    agent_mux::persistence::save_sessions(
        &file,
        &[agent_mux::persistence::SavedSession {
            swarm: None,
            launch: None,
            profile,
            dir: f.ws.clone(),
            skill_id: None,
            conversation: None,
            continued_from: None,
        }],
    )
    .unwrap();
    f.app.sessions_file = Some(file.clone());
    f.app.restore_saved_sessions();
    assert_eq!(f.app.sessions.len(), 1, "{}", notice(&f.app));
    let s = &f.app.sessions[0];
    assert_eq!(
        s.group.as_ref().map(|g| g.kind),
        Some(agent_mux::tree::GroupKind::Swarm)
    );
    let args = s.profile.args.join(" ");
    assert!(args.contains("conv-1"), "resumes the conversation: {args}");
    // the swarm records the new launch: ★ is its terminal again
    let new_launch = format!("session-{}", s.id);
    let run = record::get_swarm(&conn, &swarm.id).unwrap().unwrap();
    assert_eq!(run.planner_launch.as_deref(), Some(new_launch.as_str()));
    open_screen(&mut f);
    f.app.open_swarm_view();
    assert_eq!(view(&f.app).pane(), PaneView::Session(0));
    f.app.kill_all();
}

/// Starts one session of the fixture's fake `claude` and makes it a worker
/// of `swarm`'s first task (its sidebar group and its board row).
fn seed_worker(f: &mut Fixture, swarm: &SwarmRun) -> usize {
    f.app.focus_list();
    press(f, KeyCode::Char('n'));
    press(f, KeyCode::Enter);
    press(f, KeyCode::Enter);
    let idx = f.app.sessions.len() - 1;
    let s = &mut f.app.sessions[idx];
    s.group = Some(agent_mux::tree::GroupRef {
        kind: agent_mux::tree::GroupKind::Swarm,
        id: swarm.id.clone(),
        title: "the follow-ups".into(),
        detail: agent_mux::bus::short_id(&swarm.id).into(),
        label: "schema".into(),
    });
    let launch = format!("session-{}", s.id);
    let conn = open_aux(&f.db).unwrap();
    let board = record::board(&conn, &swarm.id).unwrap();
    conn.execute(
        "UPDATE agent_tasks SET status = 'running', assigned_to = ?1 WHERE id = ?2",
        rusqlite::params![launch, board[0].task.id],
    )
    .unwrap();
    idx
}

/// The screen on a closed swarm, chosen in the picker (`o`) as the
/// operator does: a closed swarm is not shown by default.
fn open_closed(f: &mut Fixture, swarm: &SwarmRun) {
    open_screen(f);
    press(f, KeyCode::Char('o'));
    let Some(Modal::Picker { swarms, .. }) = &view(&f.app).modal else {
        panic!("no picker: {:?}", view(&f.app).status_msg);
    };
    let at = swarms
        .iter()
        .position(|s| s.id == swarm.id)
        .expect("listed");
    for _ in 0..at {
        press(f, KeyCode::Down);
    }
    press(f, KeyCode::Enter);
}

fn closed_swarm(f: &Fixture) -> SwarmRun {
    let swarm = seed(f, SwarmStatus::Running, vec![task("schema", &[])]);
    let conn = open_aux(&f.db).unwrap();
    record::end_swarm(&conn, None, &swarm, now_ns()).unwrap();
    record::get_swarm(&conn, &swarm.id).unwrap().unwrap()
}

#[test]
fn d_then_y_deletes_a_closed_swarm_and_the_screen_shows_no_swarm() {
    let mut f = fixture();
    let swarm = closed_swarm(&f);
    open_closed(&mut f, &swarm);
    assert_eq!(view(&f.app).swarm.as_ref().map(|s| &s.id), Some(&swarm.id));
    let out = render(&f.app, 140, 32);
    assert!(
        out.contains("d deletes it"),
        "the closed swarm's hint:\n{out}"
    );
    // d on a task row asks too, for the whole swarm
    press(&mut f, KeyCode::Char('G'));
    press(&mut f, KeyCode::Char('d'));
    let q = view(&f.app)
        .confirm
        .as_ref()
        .map(|c| c.question())
        .unwrap_or_default();
    assert!(q.starts_with("Delete swarm "), "{q}");
    assert!(q.contains("\"Ship the follow-ups\""), "{q}");
    assert!(q.contains("traces stay"), "{q}");
    assert!(!q.contains("It is running"), "{q}");
    let out = render(&f.app, 140, 32);
    assert!(out.contains("Delete swarm"), "the confirmation:\n{out}");
    press(&mut f, KeyCode::Char('y'));
    let conn = open_aux(&f.db).unwrap();
    assert!(record::get_swarm(&conn, &swarm.id).unwrap().is_none());
    assert!(notice(&f.app).starts_with("deleted"), "{}", notice(&f.app));
    assert!(view(&f.app).swarm.is_none());
    assert!(view(&f.app).scope.is_none());
    let out = render(&f.app, 140, 32);
    assert!(out.contains("No swarm in this workspace"), "{out}");
}

#[test]
fn d_then_anything_but_y_deletes_nothing() {
    for no in [KeyCode::Char('n'), KeyCode::Esc] {
        let mut f = fixture();
        let swarm = closed_swarm(&f);
        open_closed(&mut f, &swarm);
        press(&mut f, KeyCode::Char('d'));
        assert!(view(&f.app).confirm.is_some());
        press(&mut f, no);
        assert!(view(&f.app).confirm.is_none());
        assert_eq!(notice(&f.app), "Not done.");
        let conn = open_aux(&f.db).unwrap();
        assert!(record::get_swarm(&conn, &swarm.id).unwrap().is_some());
    }
}

#[test]
fn d_on_a_running_swarm_stops_its_sessions_then_deletes_it() {
    let mut f = fixture();
    let swarm = seed(&f, SwarmStatus::Running, vec![task("schema", &[])]);
    let idx = seed_worker(&mut f, &swarm);
    let worker = f.app.sessions[idx].id;
    open_screen(&mut f);
    press(&mut f, KeyCode::Char('d'));
    let q = view(&f.app)
        .confirm
        .as_ref()
        .map(|c| c.question())
        .unwrap_or_default();
    assert!(q.contains("It is running"), "{q}");
    assert!(q.contains("1 task(s) are cancelled"), "{q}");
    press(&mut f, KeyCode::Char('y'));
    assert!(
        notice(&f.app).starts_with(&format!("deleted {}", swarm.id)),
        "{}",
        notice(&f.app)
    );
    assert!(
        notice(&f.app).contains("stopped 1 session(s)"),
        "{}",
        notice(&f.app)
    );
    assert!(f.app.sessions.iter().all(|s| s.id != worker));
    let conn = open_aux(&f.db).unwrap();
    assert!(record::get_swarm(&conn, &swarm.id).unwrap().is_none());
    let left: i64 = conn
        .query_row("SELECT COUNT(*) FROM swarm_tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
    f.app.kill_all();
}

#[test]
fn edit_is_unavailable_on_the_swarm_screen_and_delete_is() {
    let mut f = fixture();
    seed(&f, SwarmStatus::Running, vec![task("schema", &[])]);
    open_screen(&mut f);
    use agent_mux::keymap::ActionId;
    assert_eq!(
        f.app.command_availability(ActionId::Edit).reason(),
        Some("Changes to the plan are said to the coordinator")
    );
    assert!(f.app.command_availability(ActionId::Delete).enabled());
    press(&mut f, KeyCode::Char('e'));
    assert!(view(&f.app).confirm.is_none());
    assert!(
        notice(&f.app).contains("said to the coordinator"),
        "{}",
        notice(&f.app)
    );
}

#[test]
fn d_on_a_swarm_group_in_the_agents_sidebar_deletes_it() {
    use agent_mux::app::agents_list::AgentKind;
    let mut f = fixture();
    let swarm = closed_swarm(&f);
    seed_worker(&mut f, &swarm);
    f.app.agents.show_swarm = true;
    f.app.focus_list();
    let group = AgentKind::Run(format!("swarm:{}", swarm.id));
    let lines = f.app.agent_lines();
    let at = lines
        .iter()
        .position(|l| l.kind == group)
        .expect("the swarm's group row");
    f.app.select_agent_line(&lines, at);
    assert_eq!(f.app.agent_row(), Some(group.clone()));
    assert!(
        f.app
            .command_availability(agent_mux::keymap::ActionId::Delete)
            .enabled()
    );
    press(&mut f, KeyCode::Char('d'));
    let out = render(&f.app, 140, 32);
    assert!(out.contains("Delete swarm"), "the confirmation:\n{out}");
    press(&mut f, KeyCode::Char('y'));
    assert!(notice(&f.app).starts_with("deleted"), "{}", notice(&f.app));
    let conn = open_aux(&f.db).unwrap();
    assert!(record::get_swarm(&conn, &swarm.id).unwrap().is_none());
    assert!(f.app.sessions.is_empty());
    assert!(!f.app.agent_lines().iter().any(|l| l.kind == group));
    f.app.kill_all();
}

fn wheel(f: &mut Fixture, down: bool, column: u16, row: u16) {
    use crossterm::event::{MouseEvent, MouseEventKind};
    f.app.handle_mouse(
        MouseEvent {
            kind: if down {
                MouseEventKind::ScrollDown
            } else {
                MouseEventKind::ScrollUp
            },
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
        Instant::now(),
    );
}

#[test]
fn the_wheel_and_page_keys_scroll_the_swarms_card_and_stop_at_its_end() {
    let mut f = fixture();
    let tasks: Vec<PlannedTask> = (1..=30).map(|n| task(&format!("t{n:02}"), &[])).collect();
    seed(&f, SwarmStatus::Review, tasks);
    open_screen(&mut f);
    let out = render(&f.app, 120, 20);
    assert!(!out.contains("t30"), "the plan overflows the card:\n{out}");
    let pane = view(&f.app).drawn.get().pane;
    let (col, row) = (pane.x + pane.width / 2, pane.y + pane.height / 2);

    // the wheel over the card scrolls it, without Tab first
    wheel(&mut f, true, col, row);
    assert_eq!(view(&f.app).panel_scroll, 3);
    // far past the end: it stops with the last line at the bottom
    for _ in 0..100 {
        wheel(&mut f, true, col, row);
        render(&f.app, 120, 20);
    }
    let v = view(&f.app);
    let max = v.panel_max.get().unwrap();
    assert!(max > 0);
    assert_eq!(v.panel_scroll, max, "clamped to the card's end");
    let out = render(&f.app, 120, 20);
    assert!(out.contains("t30"), "the end is in view:\n{out}");

    // one wheel turn up moves at once (no backlog past the end)
    wheel(&mut f, false, col, row);
    assert_eq!(view(&f.app).panel_scroll, max - 3);

    // PgUp from the tree scrolls the card back to the top
    for _ in 0..20 {
        press(&mut f, KeyCode::PageUp);
    }
    assert_eq!(view(&f.app).panel_scroll, 0);
    press(&mut f, KeyCode::PageDown);
    assert!(view(&f.app).panel_scroll > 3, "a page, not a line");
}
