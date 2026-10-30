//! The swarm's state and wakes: a coordinator is recorded in `swarm_state`,
//! not as a bus message, so workers get no phantom mail and promoting another
//! session keeps the goal; a broadcast wake (`WakeWorkspace`) reaches every
//! live session of the workspace but the sender; and `agent-mux swarm start`
//! without a running TUI refuses instead of inventing a coordinator. The
//! screen refreshes only when the bus's change feed, a live session or a
//! lease's expiry says so, and a swarm's `max_workers` refuses the spawn
//! that would exceed it.

use agent_mux::app::swarm_view::SwarmView;
use agent_mux::app::{App, Mode};
use agent_mux::bus::control::ControlRequest;
use agent_mux::bus::store as bstore;
use agent_mux::bus::swarm::{LiveSwarmSession, load_snapshot};
use agent_mux::bus::{DEFAULT_LEASE_TTL_NS, NewTaskSpec, TaskIsolation};
use agent_mux::config::Profile;
use agent_mux::events::AppEvent;
use agent_mux::status::Status;
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::{OpenOptions, latest_change_seq, open_aux, open_rw};
use agent_mux::tree::{GroupKind, GroupRef};
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

#[test]
fn bus_writes_advance_the_change_feed() {
    let dir = tempfile::tempdir().unwrap();
    let db = open_store(dir.path());
    let conn = open_aux(&db).unwrap();
    let ws = bstore::normalize_workspace(dir.path());
    let mut seen = latest_change_seq(&conn).unwrap();
    let mut advanced = |what: &str| {
        let seq = latest_change_seq(&conn).unwrap();
        assert!(seq > seen, "{what} did not advance the change feed");
        seen = seq;
    };

    bstore::send_message(
        &conn,
        Some(&db),
        &ws,
        "a",
        None,
        "message",
        "hi",
        None,
        1_000,
    )
    .unwrap();
    advanced("send_message");
    let created = bstore::split_tasks(
        &conn,
        Some(&db),
        &ws,
        "a",
        &[NewTaskSpec {
            title: "t".into(),
            instructions: "i".into(),
            files: vec!["src/x.rs".into()],
            ..Default::default()
        }],
        2_000,
    )
    .unwrap();
    advanced("split_tasks");
    bstore::claim_task(&conn, Some(&db), &created[0].id, "b", 3_000).unwrap();
    advanced("claim_task (task update + lease)");
    bstore::release_leases(&conn, &ws, "b", Some(&created[0].id), &[]).unwrap();
    advanced("release_leases");
    bstore::set_coordinator(&conn, Some(&db), &ws, "c", Some("g"), None, None, 4_000).unwrap();
    advanced("set_coordinator");
    bstore::clear_swarm(&conn, Some(&db), &ws).unwrap();
    advanced("clear_swarm");
}

#[test]
fn the_screen_refreshes_only_when_something_it_shows_changed() {
    let dir = tempfile::tempdir().unwrap();
    let db = open_store(dir.path());
    let conn = open_aux(&db).unwrap();
    let ws = bstore::normalize_workspace(dir.path());
    let t0 = 1_000_000_000_000i64; // ns
    let tick = Instant::now();
    let later = |n: u64| tick + Duration::from_secs(n);

    let mut view = SwarmView::open(
        dir.path().to_path_buf(),
        Some(open_aux(&db).unwrap()),
        &[],
        t0,
    );
    assert!(
        !view.refresh_due(&[], t0, tick),
        "nothing changed since open"
    );

    // A bus write: due once, then quiet again after the read.
    bstore::send_message(&conn, Some(&db), &ws, "a", None, "message", "hi", None, t0).unwrap();
    assert!(!view.refresh_due(&[], t0, tick), "checks are rate limited");
    assert!(view.refresh_due(&[], t0, later(1)));
    view.refresh(&[], t0);
    assert_eq!(view.snapshot.messages.len(), 1);
    assert!(!view.refresh_due(&[], t0, later(2)));

    // A live session changing state: due without any store write.
    let mut live = vec![LiveSwarmSession {
        session_idx: 0,
        session_id: 1,
        launch_id: "w-1".into(),
        provider: "claude".into(),
        profile_name: "Claude Code".into(),
        agent_persona: None,
        state: "working".into(),
        workspace: ws.clone(),
        is_delegated_worker: true,
        turns: 1,
        tokens: 10,
        cost_usd: 0.0,
        running_tool: None,
    }];
    assert!(view.refresh_due(&live, t0, later(3)));
    view.refresh(&live, t0);
    assert!(!view.refresh_due(&live, t0, later(4)));
    live[0].state = "idle".into();
    assert!(view.refresh_due(&live, t0, later(5)));
    view.refresh(&live, t0);

    // A lease the snapshot shows expiring: due when its time comes.
    bstore::split_tasks(
        &conn,
        Some(&db),
        &ws,
        "a",
        &[NewTaskSpec {
            title: "t".into(),
            instructions: "i".into(),
            files: vec!["src/x.rs".into()],
            assigned_to: Some("w-1".into()),
            ..Default::default()
        }],
        t0,
    )
    .unwrap();
    assert!(view.refresh_due(&live, t0, later(6)));
    view.refresh(&live, t0);
    assert_eq!(view.snapshot.leases.len(), 1);
    assert!(!view.refresh_due(&live, t0, later(7)));
    let expired = t0 + DEFAULT_LEASE_TTL_NS;
    assert!(view.refresh_due(&live, expired, later(8)));
    view.refresh(&live, expired);
    assert!(
        view.snapshot.leases.is_empty(),
        "expired leases are left out"
    );
    assert!(!view.refresh_due(&live, expired, later(9)));
}

fn bus_group(label: &str) -> GroupRef {
    GroupRef {
        kind: GroupKind::Bus,
        id: "swarm".into(),
        title: "swarm".into(),
        detail: String::new(),
        label: label.into(),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn max_workers_refuses_the_spawn_past_the_cap_without_counting_the_coordinator() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().canonicalize().unwrap();
    let db = open_store(&ws);
    let (tx, _rx) = mpsc::channel(256);
    let mut app = App::new(shell_profiles(&ws), None, tx);
    app.trace_db_path = Some(db.clone());
    let now = Instant::now();
    for _ in 0..2 {
        app.focus_list();
        app.handle_key(&key(KeyCode::Char('n')), now);
        app.handle_key(&key(KeyCode::Enter), now);
        app.handle_key(&key(KeyCode::Enter), now);
    }
    assert_eq!(app.sessions.len(), 2);
    // Session 0 is the coordinator, session 1 a delegated worker: both sit in a bus group.
    let coordinator = format!("session-{}", app.sessions[0].id);
    app.sessions[0].group = Some(bus_group("coordinator"));
    app.sessions[1].group = Some(bus_group("worker"));
    let conn = open_aux(&db).unwrap();
    let norm = bstore::normalize_workspace(&ws);
    bstore::set_coordinator(
        &conn,
        Some(&db),
        &norm,
        &coordinator,
        Some("g"),
        Some(1),
        None,
        1,
    )
    .unwrap();

    let spawn = ControlRequest::SpawnWorker {
        parent_launch: Some(coordinator.clone()),
        workspace: ws.to_string_lossy().into_owned(),
        task_id: "task-x".into(),
        title: "one more".into(),
        instructions: "…".into(),
        harness: Some("claude".into()),
        agent: None,
        isolation: TaskIsolation::Shared,
    };
    let resp = app.handle_bus_control(spawn.clone(), now);
    assert!(!resp.ok);
    let err = resp.error.unwrap_or_default();
    assert!(err.contains("worker cap reached: 1/1"), "{err}");
    assert_eq!(app.sessions.len(), 2, "nothing was spawned");

    // A cap of 0 refuses before counting anyone.
    bstore::set_coordinator(
        &conn,
        Some(&db),
        &norm,
        &coordinator,
        None,
        Some(0),
        None,
        2,
    )
    .unwrap();
    let resp = app.handle_bus_control(spawn, now);
    let err = resp.error.unwrap_or_default();
    assert!(err.contains("worker cap reached: 1/0"), "{err}");
    app.kill_all();
}
