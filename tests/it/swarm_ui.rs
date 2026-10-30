//! The Swarm Control Room drawn into a test terminal: the feed's clock, the
//! task the preview details, and a message opened in full.

use agent_mux::app::swarm_view::{Modal, PreviewMode, SwarmPane, SwarmView};
use agent_mux::app::{App, Mode};
use agent_mux::bus::store as bstore;
use agent_mux::bus::swarm::LiveSwarmSession;
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

#[test]
fn the_feed_shows_a_clock_not_the_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, db, sessions, now) = swarm_fixture(dir.path());
    let mut app = app();
    app.mode = Mode::Swarm(Box::new(open(&ws, &db, &sessions, now)));
    let screen = render(&app, 120, 36);
    assert!(!screen.contains("1969"), "{screen}");
    let feed_line = screen
        .lines()
        .find(|l| l.contains("[question]"))
        .unwrap_or_else(|| panic!("no feed row:\n{screen}"));
    // HH:MM:SS right after the row marker; the date stays out of the feed.
    let cell = feed_line
        .split('│')
        .find(|seg| seg.contains("[question]"))
        .unwrap_or("")
        .trim_start();
    let clock = cell.trim_start_matches("▸ ").trim_start();
    let b = clock.as_bytes();
    assert!(
        b.len() > 8 && b[2] == b':' && b[5] == b':' && b[8] == b' ',
        "expected HH:MM:SS at the row start, got {clock:?}"
    );
    assert!(!feed_line.contains("2026-"), "{feed_line}");
}

#[test]
fn task_detail_follows_the_selected_agent_unless_the_board_has_focus() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, db, sessions, now) = swarm_fixture(dir.path());
    let mut view = open(&ws, &db, &sessions, now);
    view.pane = SwarmPane::Preview;
    view.preview_mode = PreviewMode::TaskDetail;
    view.selected_agent = 2; // W2 = worker-bbbb
    assert_eq!(
        view.detail_task().map(|t| t.title.as_str()),
        Some("Wire bus store queries")
    );
    let mut app = app();
    app.mode = Mode::Swarm(Box::new(view));
    let screen = render(&app, 120, 36);
    assert!(screen.contains("Wire bus store queries"), "{screen}");

    let mut view = open(&ws, &db, &sessions, now);
    view.pane = SwarmPane::Board;
    view.selected_task = 0;
    assert_eq!(
        view.detail_task().map(|t| t.title.as_str()),
        Some("Add swarm view state machine")
    );
}

#[test]
fn enter_opens_a_message_in_full_and_m_replies_to_its_sender() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, db, sessions, now) = swarm_fixture(dir.path());
    let mut view = open(&ws, &db, &sessions, now);
    view.pane = SwarmPane::Messages;
    view.selected_message = 0;
    view.handle_key(key(KeyCode::Enter));
    assert!(matches!(view.modal, Some(Modal::ViewMessage { .. })));
    let mut app = app();
    app.mode = Mode::Swarm(Box::new(view));
    let screen = render(&app, 120, 36);
    assert!(
        screen.contains("Context: the scaffolder prunes on cancel"),
        "the second paragraph is on screen:\n{screen}"
    );
    assert!(screen.contains("W2 (codex)") && screen.contains("★ Coord (claude)"));

    let Mode::Swarm(mut view) = app.mode else {
        unreachable!()
    };
    view.handle_key(key(KeyCode::Char('m')));
    match &view.modal {
        Some(Modal::ComposeMessage {
            to_launch,
            topic_idx,
            ..
        }) => {
            assert_eq!(to_launch.as_deref(), Some("worker-bbbb"));
            assert_eq!(*topic_idx, 1, "a question gets an answer");
        }
        other => panic!("expected the composer, got {other:?}"),
    }
}
