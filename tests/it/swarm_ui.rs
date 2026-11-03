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
        "the plan waits for you: 3 task(s), 1 error(s) to fix first",
        "one task per subsystem",
        "y approve · p re-plan",
        // one footer: the global one
        "[F1 Commands] Swarm",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    assert!(!out.contains("NEEDS YOU"), "no strip of its own:\n{out}");
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
fn y_approves_only_a_plan_without_errors_and_d_deletes_a_draft_task() {
    let mut f = fixture();
    let mut bad = task("docs", &[]);
    bad.harness = Some("gemini".into());
    let swarm = seed(&f, SwarmStatus::Review, vec![task("schema", &[]), bad]);
    open_screen(&mut f);

    press(&mut f, KeyCode::Char('y'));
    assert!(view(&f.app).confirm.is_none());
    assert_eq!(
        notice(&f.app),
        "fix the plan first: docs: harness `gemini` is not installed"
    );

    press(&mut f, KeyCode::Char('G'));
    press(&mut f, KeyCode::Char('d'));
    let out = render(&f.app, 120, 30);
    assert!(out.contains("Delete `docs` from the plan?"), "{out}");
    press(&mut f, KeyCode::Char('y'));
    assert_eq!(view(&f.app).board.len(), 1);

    press(&mut f, KeyCode::Char('g'));
    press(&mut f, KeyCode::Char('y'));
    let out = render(&f.app, 120, 30);
    assert!(
        out.contains("Approve the plan? 1 task(s); workers start now."),
        "{out}"
    );
    press(&mut f, KeyCode::Char('y'));
    let conn = open_aux(&f.db).unwrap();
    assert_eq!(
        record::get_swarm(&conn, &swarm.id).unwrap().unwrap().status,
        SwarmStatus::Running
    );
    assert_eq!(
        notice(&f.app),
        "plan approved: 1 task(s); workers start now"
    );
    let first = record::board(&conn, &swarm.id).unwrap()[0].task.id.clone();
    assert!(bstore::plan_blocks(&conn, &first).is_none());
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
        "Start a swarm",
        "Goal: what done looks like",
        "Coordinator: claude",
        "Max parallel workers: 3",
        "Approve the plan before workers start: yes",
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
    let out = render(&f.app, 140, 32);
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
    for c in "Second goal".chars() {
        press(&mut f, KeyCode::Char(c));
    }
    let out = render(&f.app, 140, 40);
    assert!(out.contains("Workers may run on: [x] claude"), "{out}");
    f.app.handle_key(
        &KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    assert!(view(&f.app).modal.is_none(), "{}", notice(&f.app));
    let conn = open_aux(&f.db).unwrap();
    let live = record::live_swarms_in(&conn, &f.ws.to_string_lossy()).unwrap();
    assert_eq!(live.len(), 2, "{}", notice(&f.app));
    let second = live.iter().find(|s| s.id != stuck.id).unwrap();
    assert_eq!(second.goal, "Second goal");
    assert_eq!(record::worker_pool(&conn, second), ["claude"]);
    assert!(
        notice(&f.app).contains("is planning with claude"),
        "{}",
        notice(&f.app)
    );
    // The cursor is on the new swarm.
    assert_eq!(
        view(&f.app).swarm.as_ref().map(|s| s.id.clone()),
        Some(second.id.clone())
    );

    // A start that fails keeps the form, with the reason drawn in it.
    f.app.trace_db_path = None;
    press(&mut f, KeyCode::Char('n'));
    for c in "Third".chars() {
        press(&mut f, KeyCode::Char(c));
    }
    f.app.handle_key(
        &KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    let out = render(&f.app, 140, 40);
    assert!(
        matches!(
            view(&f.app).modal,
            Some(Modal::Launch { error: Some(_), .. })
        ),
        "{out}"
    );
    assert!(out.contains("✗ a swarm needs the trace store"), "{out}");
    assert!(out.contains("Third"), "{out}");
}

#[test]
fn the_commands_menu_submits_the_launch_form_too() {
    let mut f = fixture();
    open_screen(&mut f);
    press(&mut f, KeyCode::Enter);
    for c in "Via F1".chars() {
        press(&mut f, KeyCode::Char(c));
    }
    f.app
        .execute_command(agent_mux::keymap::ActionId::SubmitDialog, Instant::now());
    assert!(view(&f.app).modal.is_none(), "{}", notice(&f.app));
    assert!(notice(&f.app).contains("is planning"), "{}", notice(&f.app));
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
