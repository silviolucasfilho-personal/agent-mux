//! The Swarm screen drawn into a test terminal and driven through the App
//! (`app::swarm_board`, `ui::swarm_board`): a plan in review drawn as a
//! board with what needs the operator, `y` approving only a plan without
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
    f.app.sessions.clear();
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

#[test]
fn a_plan_in_review_is_drawn_as_a_board_with_what_needs_you() {
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
    for want in [
        "◆ Ship the follow-ups",
        "· review · 0/3 done",
        "0/2 workers",
        "⚑ NEEDS YOU (1)",
        "the plan waits for you: 3 task(s), 1 error(s) to fix first",
        "Plan (3) · planner: claude · \"one task per subsystem\"",
        "schema",
        "queries ",
        "← schema",
        "schema · draft · Do schema",
        "1 Task",
        "Instructions for schema.",
        "── Done when ──",
        "y approve",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    if std::env::var_os("SWARM_UI_PRINT").is_some() {
        println!("{out}");
    }
    for width in [80u16, 120, 180] {
        let out = render(&f.app, width, 24);
        for want in [
            "◆ Ship",
            "NEEDS YOU (1)",
            "Plan (3)",
            "schema",
            "queries",
            "1 Task",
            "y approve",
        ] {
            assert!(
                out.contains(want),
                "{width} columns: missing {want:?} in:\n{out}"
            );
        }
        if width == 80 && std::env::var_os("SWARM_UI_PRINT").is_some() {
            println!("{out}");
        }
    }
    // The task with an error is marked on the board and explains itself.
    let docs_row = out.lines().find(|l| l.contains("Do docs")).unwrap();
    assert!(docs_row.contains("!·"), "{docs_row}");
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
        view(&f.app).status_msg.as_deref(),
        Some("fix the plan first: docs: harness `gemini` is not installed")
    );

    press(&mut f, KeyCode::Char('G'));
    press(&mut f, KeyCode::Char('d'));
    let out = render(&f.app, 120, 30);
    assert!(out.contains("Delete `docs` from the plan?"), "{out}");
    press(&mut f, KeyCode::Char('y'));
    assert_eq!(view(&f.app).board.len(), 1);

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
        view(&f.app).status_msg.as_deref(),
        Some("plan approved: 1 task(s); workers start now")
    );
    assert!(bstore::plan_blocks(&conn, &swarm.workspace).is_none());
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
    assert!(out.contains("⇡"), "{out}");

    press(&mut f, KeyCode::Char('y'));
    let out = render(&f.app, 120, 30);
    assert!(
        out.contains("Merge loop/bus-a into the workspace's branch"),
        "{out}"
    );
    press(&mut f, KeyCode::Char('y'));
    assert!(
        view(&f.app)
            .status_msg
            .as_deref()
            .is_some_and(|m| m.starts_with("merged loop/bus-a (")),
        "{:?}",
        view(&f.app).status_msg
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
    assert!(out.contains("Nothing needs you."), "{out}");
    press(&mut f, KeyCode::Enter);
    assert!(matches!(view(&f.app).modal, Some(Modal::Launch { .. })));
    let out = render(&f.app, 120, 30);
    for want in [
        "Start a swarm",
        "Goal: what done looks like",
        "Planner: claude",
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
