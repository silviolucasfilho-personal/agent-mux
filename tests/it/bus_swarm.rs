//! The swarm's state and wakes: a coordinator is recorded in `swarm_state`,
//! not as a bus message, so workers get no phantom mail and promoting another
//! session keeps the goal; a broadcast wake (`WakeWorkspace`) reaches every
//! live session of the workspace but the sender; and `agent-mux swarm start`
//! without a running TUI refuses instead of inventing a coordinator.

use agent_mux::app::{App, Mode};
use agent_mux::bus::control::ControlRequest;
use agent_mux::bus::store as bstore;
use agent_mux::bus::swarm::{LiveSwarmSession, load_snapshot};
use agent_mux::bus::{NewTaskSpec, TaskIsolation};
use agent_mux::config::Profile;
use agent_mux::events::AppEvent;
use agent_mux::status::Status;
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::{OpenOptions, open_aux, open_rw};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

fn open_store(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("traces.db");
    let _ = open_rw(
        &db,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run-swarm".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    db
}

#[test]
fn promoting_a_coordinator_keeps_the_goal_and_posts_no_message() {
    let dir = tempfile::tempdir().unwrap();
    let db = open_store(dir.path());
    let conn = open_aux(&db).unwrap();
    let ws = bstore::normalize_workspace(dir.path());

    let first = bstore::set_coordinator(
        &conn,
        Some(&db),
        &ws,
        "coord-aaaa0001",
        Some("Ship the swarm"),
        Some(3),
        Some(TaskIsolation::Worktree),
        1_000,
    )
    .unwrap();
    assert_eq!(first.goal.as_deref(), Some("Ship the swarm"));
    assert_eq!(first.max_workers, Some(3));
    assert_eq!(first.isolation, Some(TaskIsolation::Worktree));

    // A worker on the board: it must not see the coordinator record as mail.
    bstore::split_tasks(
        &conn,
        Some(&db),
        &ws,
        "coord-aaaa0001",
        &[NewTaskSpec {
            title: "Wire the feed".into(),
            instructions: "…".into(),
            assigned_to: Some("worker-bbbb0001".into()),
            ..Default::default()
        }],
        2_000,
    )
    .unwrap();

    // Promote another session without a goal: the goal and parameters stay.
    let promoted = bstore::set_coordinator(
        &conn,
        Some(&db),
        &ws,
        "coord-cccc0002",
        None,
        None,
        None,
        3_000,
    )
    .unwrap();
    assert_eq!(promoted.coordinator_launch, "coord-cccc0002");
    assert_eq!(promoted.goal.as_deref(), Some("Ship the swarm"));
    assert_eq!(promoted.max_workers, Some(3));
    assert_eq!(promoted.started_ns, 1_000);
    assert_eq!(promoted.updated_ns, 3_000);

    // The snapshot names a member only for a session it can see: the promoted
    // one is live here, the first coordinator is gone.
    let live = vec![LiveSwarmSession {
        session_idx: 0,
        session_id: 1,
        launch_id: "coord-cccc0002".into(),
        provider: "claude".into(),
        profile_name: "Claude Code".into(),
        agent_persona: None,
        state: "idle".into(),
        workspace: ws.clone(),
        is_delegated_worker: false,
        turns: 0,
        tokens: 0,
        cost_usd: 0.0,
        running_tool: None,
    }];
    let snap = load_snapshot(Some(&conn), dir.path(), &live, None, 4);
    assert_eq!(
        snap.coordinator.as_ref().map(|c| c.launch_id.as_str()),
        Some("coord-cccc0002")
    );
    assert!(snap.workers.is_empty());
    assert_eq!(snap.goal.as_deref(), Some("Ship the swarm"));
    assert_eq!(snap.max_workers, Some(3));
    assert!(snap.messages.is_empty(), "the registry is not a message");
    assert_eq!(
        bstore::count_unread_messages(&conn, &ws, "worker-bbbb0001").unwrap(),
        0
    );

    assert_eq!(bstore::clear_swarm(&conn, Some(&db), &ws).unwrap(), 1);
    assert!(bstore::swarm_state(&conn, &ws).unwrap().is_none());
    let snap = load_snapshot(Some(&conn), dir.path(), &[], None, 5);
    assert!(snap.goal.is_none());
}

#[test]
fn swarm_start_without_a_tui_refuses_instead_of_recording_a_phantom() {
    let dir = tempfile::tempdir().unwrap();
    let db = open_store(dir.path());
    let sock = dir.path().join("no-such.sock");
    let out = Command::new(env!("CARGO_BIN_EXE_agent-mux"))
        .args(["swarm", "start", "Ship it", "--workspace"])
        .arg(dir.path())
        .arg("--db")
        .arg(&db)
        .env("AGENT_MUX_CONTROL_SOCK", &sock)
        .env("HOME", dir.path())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("running agent-mux TUI"), "{stderr}");
    let conn = open_aux(&db).unwrap();
    let ws = bstore::normalize_workspace(dir.path());
    assert!(bstore::swarm_state(&conn, &ws).unwrap().is_none());
    assert!(
        bstore::list_workspace_messages(&conn, &ws, 10)
            .unwrap()
            .is_empty()
    );
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn shell_profiles(dir: &Path) -> Vec<Profile> {
    vec![Profile {
        name: "shell".into(),
        command: "sh".into(),
        args: vec![],
        default_dir: Some(dir.to_string_lossy().into_owned()),
        tracing: None,
        model: None,
        bypass_approvals: None,
    }]
}

async fn pump_until(
    rx: &mut mpsc::Receiver<AppEvent>,
    app: &mut App,
    timeout: Duration,
    mut pred: impl FnMut(&App) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred(app) {
            return true;
        }
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(200));
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                app.handle_pty_output(id, &bytes, Instant::now());
            }
            Ok(Some(AppEvent::PtyExit { id })) => app.handle_pty_exit(id),
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    pred(app)
}

fn screen(app: &App, idx: usize) -> String {
    app.sessions[idx].parser.screen().contents()
}

#[cfg(unix)]
#[tokio::test]
async fn wake_workspace_reaches_every_idle_session_but_the_sender() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::channel(256);
    let mut app = App::new(shell_profiles(&ws), None, tx);
    let now = Instant::now();
    for _ in 0..2 {
        // A new session takes the keyboard; the next `n` needs the list.
        app.focus_list();
        app.handle_key(&key(KeyCode::Char('n')), now);
        app.handle_key(&key(KeyCode::Enter), now);
        assert!(matches!(app.mode, Mode::NewSession(_)));
        app.handle_key(&key(KeyCode::Enter), now);
    }
    assert_eq!(app.sessions.len(), 2);
    assert!(
        pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
            !screen(a, 0).trim().is_empty() && !screen(a, 1).trim().is_empty()
        })
        .await
    );
    // Both shells idle: nothing printed for the activity window.
    assert!(
        pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
            let t = Instant::now();
            a.sessions.iter().all(|s| s.status(t) == Status::Idle)
        })
        .await
    );

    let resp = app.handle_bus_control(
        ControlRequest::WakeWorkspace {
            workspace: ws.to_string_lossy().into_owned(),
            prompt: "echo swarm-wake-all".into(),
            exclude: None,
        },
        Instant::now(),
    );
    assert!(resp.ok, "{resp:?}");
    assert_eq!(
        resp.detail.as_deref(),
        Some("woke 2 session(s), queued 0 until idle")
    );
    assert!(
        pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
            screen(a, 0).contains("swarm-wake-all") && screen(a, 1).contains("swarm-wake-all")
        })
        .await,
        "s0: {}\ns1: {}",
        screen(&app, 0),
        screen(&app, 1)
    );

    assert!(
        pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
            let t = Instant::now();
            a.sessions.iter().all(|s| s.status(t) == Status::Idle)
        })
        .await
    );
    let sender = format!("session-{}", app.sessions[0].id);
    let resp = app.handle_bus_control(
        ControlRequest::WakeWorkspace {
            workspace: ws.to_string_lossy().into_owned(),
            prompt: "echo swarm-wake-others".into(),
            exclude: Some(sender),
        },
        Instant::now(),
    );
    assert_eq!(
        resp.detail.as_deref(),
        Some("woke 1 session(s), queued 0 until idle")
    );
    assert!(
        pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
            screen(a, 1).contains("swarm-wake-others")
        })
        .await
    );
    assert!(!screen(&app, 0).contains("swarm-wake-others"));

    // A workspace none of the sessions runs in wakes nobody.
    let other = tempfile::tempdir().unwrap();
    let resp = app.handle_bus_control(
        ControlRequest::WakeWorkspace {
            workspace: other.path().to_string_lossy().into_owned(),
            prompt: "echo never".into(),
            exclude: None,
        },
        Instant::now(),
    );
    assert_eq!(
        resp.detail.as_deref(),
        Some("woke 0 session(s), queued 0 until idle")
    );
    app.kill_all();
}
