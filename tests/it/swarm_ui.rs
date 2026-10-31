//! Swarm Mission Control drawn into a test terminal: the mission strip and
//! the lanes, a waiting lane's thread with its clock and whole question, the
//! task tab, a message opened in full and a reply that answers it.

use agent_mux::app::swarm_view::{Focus, Modal, Outcome, PanelTab, SwarmView};
use agent_mux::app::{App, Mode};
use agent_mux::bus::store as bstore;
use agent_mux::bus::swarm::{LaneState, LiveSwarmSession};
use agent_mux::bus::{NewTaskSpec, TaskIsolation, TaskStatus};
use agent_mux::config::Profile;
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::{OpenOptions, open_aux, open_rw};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::{Path, PathBuf};
use std::time::Instant;
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

fn live(idx: usize, launch: &str, provider: &str, state: &str, ws: &str) -> LiveSwarmSession {
    LiveSwarmSession {
        session_idx: idx,
        session_id: idx + 1,
        launch_id: launch.into(),
        provider: provider.into(),
        profile_name: format!("{provider} profile"),
        agent_persona: (idx == 0).then(|| "coordinator".to_string()),
        state: state.into(),
        workspace: ws.into(),
        is_delegated_worker: idx > 0,
        turns: 1,
        tokens: 1_000,
        cost_usd: 0.01,
        running_tool: None,
        quiet_ns: None,
    }
}

/// A store with a coordinator, two workers, a task per worker and a
/// two-paragraph question on the feed; answers the workspace and the DB.
fn swarm_fixture(dir: &Path) -> (PathBuf, PathBuf, Vec<LiveSwarmSession>, i64) {
    let db = dir.join("traces.db");
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
    let conn = open_aux(&db).unwrap();
    let ws_path = dir.join("my-project");
    std::fs::create_dir_all(&ws_path).unwrap();
    let ws = bstore::normalize_workspace(&ws_path);
    // 2026-10-30 14:05:09 UTC, as nanoseconds.
    let now = 1_793_455_509_000_000_000i64;

    bstore::set_coordinator(
        &conn,
        Some(&db),
        &ws,
        "coord-1111",
        Some("Ship it"),
        Some(4),
        Some(TaskIsolation::Shared),
        now - 600_000_000_000,
    )
    .unwrap();
    let tasks = bstore::split_tasks(
        &conn,
        Some(&db),
        &ws,
        "coord-1111",
        &[
            NewTaskSpec {
                title: "Add swarm view state machine".into(),
                instructions: "the view".into(),
                assigned_to: Some("worker-aaaa".into()),
                ..Default::default()
            },
            NewTaskSpec {
                title: "Wire bus store queries".into(),
                instructions: "the store".into(),
                assigned_to: Some("worker-bbbb".into()),
                ..Default::default()
            },
        ],
        now - 500_000_000_000,
    )
    .unwrap();
    for (t, who) in tasks.iter().zip(["worker-aaaa", "worker-bbbb"]) {
        bstore::update_task(
            &conn,
            Some(&db),
            &t.id,
            who,
            TaskStatus::Running,
            None,
            None,
            None,
            None,
            now - 400_000_000_000,
        )
        .unwrap();
    }
    bstore::send_message(
        &conn,
        Some(&db),
        &ws,
        "worker-bbbb",
        Some("coord-1111"),
        "question",
        "Should cancelled tasks release their worktrees immediately?\n\nContext: the scaffolder prunes on cancel unless the tree is dirty.",
        None,
        now - 200_000_000_000,
    )
    .unwrap();

    let sessions = vec![
        live(0, "coord-1111", "claude", "working", &ws),
        live(1, "worker-aaaa", "claude", "working", &ws),
        live(2, "worker-bbbb", "codex", "idle", &ws),
    ];
    (ws_path, db, sessions, now)
}

fn app() -> App {
    let (tx, _rx) = mpsc::channel(16);
    App::new(
        vec![Profile {
            name: "shell".into(),
            command: "sh".into(),
            args: vec![],
            default_dir: None,
            tracing: None,
            model: None,
            bypass_approvals: None,
        }],
        None,
        tx,
    )
}

fn open(ws: &Path, db: &Path, sessions: &[LiveSwarmSession], now: i64) -> SwarmView {
    SwarmView::open(ws.to_path_buf(), Some(open_aux(db).unwrap()), sessions, now)
}

fn select(view: &mut SwarmView, name: &str) {
    view.selected_lane = view
        .lanes
        .iter()
        .position(|l| l.name == name)
        .unwrap_or_else(|| panic!("no lane {name}: {:?}", view.lanes));
}

#[test]
fn the_mission_strip_and_the_lanes_say_what_is_happening() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, db, sessions, now) = swarm_fixture(dir.path());
    let mut app = app();
    app.mode = Mode::Swarm(Box::new(open(&ws, &db, &sessions, now)));
    let screen = render(&app, 120, 36);
    let strip = screen.lines().next().unwrap();
    for fact in ["Ship it", "executing", "0/2 done", "2/4 workers", "10m"] {
        assert!(
            strip.contains(fact),
            "{fact:?} missing from the strip: {strip}"
        );
    }
    assert!(!strip.contains(&*dir.path().to_string_lossy()), "{strip}");
    for row in [
        "★ coordinator·claude",
        "worker·claude",
        "Add swarm view state machine",
        "worker·codex",
        "waiting 3m on coordinator",
        "plan: 0/2 tasks done · 1 open question",
    ] {
        assert!(screen.contains(row), "{row:?} missing:\n{screen}");
    }
    assert!(
        !screen.contains("W2") && !screen.contains("1969"),
        "{screen}"
    );

    // A narrow terminal still draws.
    let narrow = render(&app, 80, 24);
    assert!(narrow.contains("coordinator·claude"), "{narrow}");
}

#[test]
fn a_waiting_lane_opens_on_its_thread_with_a_clock_and_the_whole_question() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, db, sessions, now) = swarm_fixture(dir.path());
    let mut view = open(&ws, &db, &sessions, now);
    select(&mut view, "worker·codex");
    assert_eq!(view.selected().unwrap().state, LaneState::Waiting);
    assert_eq!(view.tab(), PanelTab::Thread);
    let mut app = app();
    app.mode = Mode::Swarm(Box::new(view));
    let screen = render(&app, 120, 36);
    assert!(
        screen.contains("Context: the scaffolder prunes on cancel"),
        "the whole question is in the thread:\n{screen}"
    );
    assert!(screen.contains("open question"), "{screen}");
    let header = screen
        .lines()
        .find(|l| l.contains("worker·codex → coordinator·claude"))
        .filter(|l| l.contains("open question"))
        .unwrap_or_else(|| panic!("no message header:\n{screen}"));
    let cell = header
        .split('│')
        .find(|c| c.contains("→"))
        .unwrap()
        .trim_start();
    let clock = cell.trim_start_matches("▸ ").trim_start().as_bytes();
    assert!(
        clock.len() > 8 && clock[2] == b':' && clock[5] == b':',
        "expected HH:MM:SS first: {header:?}"
    );
    assert!(!header.contains("2026-"), "{header}");
}

#[test]
fn the_task_tab_shows_the_selected_lanes_task() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, db, sessions, now) = swarm_fixture(dir.path());
    let mut view = open(&ws, &db, &sessions, now);
    select(&mut view, "worker·claude");
    view.handle_key(key(KeyCode::Char('4')));
    assert_eq!(view.tab(), PanelTab::Task);
    let mut app = app();
    app.mode = Mode::Swarm(Box::new(view));
    let screen = render(&app, 120, 36);
    assert!(
        screen.contains("── Instructions ──") && screen.contains("the view"),
        "{screen}"
    );
}

#[test]
fn enter_opens_a_message_in_full_and_m_answers_it() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, db, sessions, now) = swarm_fixture(dir.path());
    let mut view = open(&ws, &db, &sessions, now);
    select(&mut view, "worker·codex");
    view.handle_key(key(KeyCode::Tab));
    assert_eq!(view.focus, Focus::Panel);
    let question = view.selected_message().unwrap().id.clone();
    view.handle_key(key(KeyCode::Enter));
    assert!(matches!(view.modal, Some(Modal::ViewMessage { .. })));
    let mut app = app();
    app.mode = Mode::Swarm(Box::new(view));
    let screen = render(&app, 120, 36);
    assert!(screen.contains("worker·codex") && screen.contains("coordinator·claude"));

    let Mode::Swarm(mut view) = app.mode else {
        unreachable!()
    };
    view.handle_key(key(KeyCode::Char('m')));
    match &view.modal {
        Some(Modal::ComposeMessage {
            to_launch,
            reply_to,
            topic_idx,
            ..
        }) => {
            assert_eq!(to_launch.as_deref(), Some("worker-bbbb"));
            assert_eq!(reply_to.as_deref(), Some(question.as_str()));
            assert_eq!(*topic_idx, 1, "a question gets an answer");
        }
        other => panic!("expected the composer, got {other:?}"),
    }
}

/// The fixture plus a question to the operator and a failed task.
fn needs_fixture(dir: &Path) -> (PathBuf, PathBuf, Vec<LiveSwarmSession>, i64, String, String) {
    let (ws, db, sessions, now) = swarm_fixture(dir);
    let conn = open_aux(&db).unwrap();
    let norm = bstore::normalize_workspace(&ws);
    let q = bstore::send_message(
        &conn,
        Some(&db),
        &norm,
        "worker-aaaa",
        Some("user"),
        "question",
        "May I drop the v15 trigger while migrating?",
        None,
        now - 60_000_000_000,
    )
    .unwrap();
    let failed = bstore::split_tasks(
        &conn,
        Some(&db),
        &norm,
        "coord-1111",
        &[NewTaskSpec {
            title: "UI tests".into(),
            instructions: "the tests".into(),
            assigned_to: Some("worker-cccc".into()),
            ..Default::default()
        }],
        now - 300_000_000_000,
    )
    .unwrap()
    .remove(0);
    bstore::update_task(
        &conn,
        Some(&db),
        &failed.id,
        "worker-cccc",
        TaskStatus::Failed,
        None,
        Some("cargo test exit 101".into()),
        None,
        None,
        now - 120_000_000_000,
    )
    .unwrap();
    (ws, db, sessions, now, q.id, failed.id)
}

#[test]
fn nothing_needing_you_is_one_quiet_line() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, db, sessions, now) = swarm_fixture(dir.path());
    let mut app = app();
    app.mode = Mode::Swarm(Box::new(open(&ws, &db, &sessions, now)));
    let screen = render(&app, 120, 20);
    assert!(
        screen
            .lines()
            .nth(1)
            .unwrap()
            .contains("Nothing needs you."),
        "{screen}"
    );
}

#[test]
fn the_strip_lists_what_needs_you_and_bang_acts_on_it() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, db, sessions, now, question, failed) = needs_fixture(dir.path());
    let view = open(&ws, &db, &sessions, now);
    assert_eq!(view.needs.len(), 2, "{:#?}", view.needs);
    let mut app = app();
    app.mode = Mode::Swarm(Box::new(view));
    let screen = render(&app, 120, 20);
    let strip: Vec<&str> = screen.lines().skip(1).take(3).collect();
    assert!(strip[0].contains("NEEDS YOU (2)"), "{screen}");
    assert!(
        strip[1].contains("question  worker·claude asks you: \"May I drop the v15 trigger"),
        "{screen}"
    );
    assert!(
        strip[2].contains("failed    worker-c failed \"UI tests\": cargo test exit 101"),
        "{screen}"
    );
    assert!(screen.contains("waiting 1m on you"), "{screen}");

    let Mode::Swarm(view) = app.mode else {
        unreachable!()
    };
    let mut view = *view;
    // `!` focuses the strip; Enter on a question answers it.
    view.handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::SHIFT));
    assert_eq!(view.focus, Focus::Needs);
    view.handle_key(key(KeyCode::Enter));
    match &view.modal {
        Some(Modal::ComposeMessage {
            to_launch,
            reply_to,
            ..
        }) => {
            assert_eq!(to_launch.as_deref(), Some("worker-aaaa"));
            assert_eq!(reply_to.as_deref(), Some(question.as_str()));
        }
        other => panic!("expected the composer, got {other:?}"),
    }
    view.handle_key(key(KeyCode::Esc));

    // `r` retries the failed task; `d` dismisses it.
    view.handle_key(key(KeyCode::Down));
    assert_eq!(
        view.handle_key(key(KeyCode::Char('r'))),
        Outcome::RetryTask {
            task_id: failed.clone()
        }
    );
    view.handle_key(key(KeyCode::Char('d')));
    assert_eq!(view.needs.len(), 1);
    assert!(
        view.needs
            .iter()
            .all(|n| n.task_id.as_deref() != Some(&failed))
    );

    // A refresh does not bring the dismissed failure back.
    view.refresh(&sessions, now + 1);
    assert_eq!(view.needs.len(), 1);
}

#[test]
fn activity_tells_the_story_with_task_events() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, db, sessions, now, _, _) = needs_fixture(dir.path());
    let mut view = open(&ws, &db, &sessions, now);
    view.handle_key(key(KeyCode::Char('a')));
    view.handle_key(key(KeyCode::Tab));
    // It opens on the newest line; `g` goes back to how it began.
    view.handle_key(key(KeyCode::Char('g')));
    let mut app = app();
    app.mode = Mode::Swarm(Box::new(view));
    let screen = render(&app, 120, 40);
    for line in [
        "coordinator·claude planned \"Add swarm view state",
        "worker-c failed \"UI tests\"",
    ] {
        assert!(screen.contains(line), "{line:?} missing:\n{screen}");
    }
    let Mode::Swarm(mut view) = app.mode else {
        unreachable!()
    };
    view.handle_key(key(KeyCode::Char('G')));
    app.mode = Mode::Swarm(view);
    let screen = render(&app, 120, 40);
    assert!(
        screen.contains("worker·claude → User  open question"),
        "{screen}"
    );
}
