//! The bus around a swarm: a broadcast wake (`WakeWorkspace`) reaches
//! every live session of the workspace but the sender; a wake is pasted and
//! its Enter written apart, so a TUI that takes a burst as a paste still
//! submits it, and a queued wake expires; `agent-mux swarm start` without a
//! running TUI refuses instead of recording a swarm nobody runs; bus and
//! swarm writes advance the change feed; a finished branch is merged with a
//! merge commit, and a conflict changes nothing.

use agent_mux::app::{App, Mode, PendingWake, WAKE_TTL};
use agent_mux::bus::control::ControlRequest;
use agent_mux::bus::store as bstore;
use agent_mux::bus::{NewTaskSpec, TaskIsolation};
use agent_mux::config::Profile;
use agent_mux::events::AppEvent;
use agent_mux::status::Status;
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::{OpenOptions, latest_change_seq, open_aux, open_rw};
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
    assert!(
        agent_mux::bus::swarm::record::live_swarm(&conn, &ws)
            .unwrap()
            .is_none(),
        "no swarm is recorded without a TUI to run it"
    );
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
        // What the tick does for wakes: their Enter is a write of its own.
        app.flush_pending_bus_wakes(Instant::now());
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

/// A stand-in for Claude Code's and Codex's input box (probed 2026-11-02):
/// it enables bracketed paste, and a read that is a paste (bracketed, or a
/// burst over 64 bytes) takes `\r` as text; only a `\r` arriving on its own
/// submits, printing `SUBMIT[<text>]`.
const PASTE_AWARE_TUI: &str = r#"
import os, sys, termios, tty
tty.setraw(0)
os.write(1, b"\x1b[?2004h> ")
buf, pasting = b"", False
while True:
    chunk = os.read(0, 4096)
    if not chunk:
        break
    if chunk.startswith(b"\x1b[200~"):
        pasting, chunk = True, chunk[6:]
    if pasting and b"\x1b[201~" in chunk:
        chunk = chunk.replace(b"\x1b[201~", b"")
        buf += chunk
        pasting = False
        continue
    if pasting or len(chunk) > 64:
        buf += chunk.replace(b"\r", b"\n")
        continue
    for c in chunk:
        if c == 13:
            os.write(1, b"\r\nSUBMIT[" + buf + b"]\r\n> ")
            buf = b""
        else:
            buf += bytes([c])
"#;

#[cfg(unix)]
#[tokio::test]
async fn a_long_wake_is_submitted_by_a_tui_that_takes_a_burst_as_a_paste() {
    if Command::new("python3").arg("-c").arg("1").output().is_err() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().canonicalize().unwrap();
    let script = ws.join("tui.py");
    std::fs::write(&script, PASTE_AWARE_TUI).unwrap();
    let profiles = vec![Profile {
        name: "paste-aware".into(),
        command: "python3".into(),
        args: vec![script.to_string_lossy().into_owned()],
        default_dir: Some(ws.to_string_lossy().into_owned()),
        tracing: None,
        model: None,
        bypass_approvals: None,
    }];
    let (tx, mut rx) = mpsc::channel(256);
    let mut app = App::new(profiles, None, tx);
    let now = Instant::now();
    app.focus_list();
    app.handle_key(&key(KeyCode::Char('n')), now);
    app.handle_key(&key(KeyCode::Enter), now);
    app.handle_key(&key(KeyCode::Enter), now);
    assert_eq!(app.sessions.len(), 1);
    assert!(
        pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
            a.sessions[0].parser.screen().bracketed_paste()
                && a.sessions[0].status(Instant::now()) == Status::Idle
        })
        .await,
        "{}",
        screen(&app, 0)
    );

    let prompt = "[plan-approved] The operator approved your plan: delegate the tasks now, \
                  one worker per task, then wait for their results.";
    assert!(prompt.len() > 100);
    let resp = app.handle_bus_control(
        ControlRequest::WakeSession {
            launch_id: format!("session-{}", app.sessions[0].id),
            prompt: prompt.into(),
        },
        Instant::now(),
    );
    assert_eq!(
        resp.detail.as_deref(),
        Some("injected into idle PTY"),
        "{resp:?}"
    );
    assert!(
        pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
            screen(a, 0)
                .replace('\n', "")
                .contains("SUBMIT[[plan-approved]")
        })
        .await,
        "the wake was pasted but not submitted:\n{}",
        screen(&app, 0)
    );
    assert!(app.pending_wake_submits.is_empty());
    app.kill_all();
}

#[cfg(unix)]
#[tokio::test]
async fn a_queued_wake_older_than_its_ttl_is_dropped_not_typed_in_late() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::channel(256);
    let mut app = App::new(shell_profiles(&ws), None, tx);
    let now = Instant::now();
    app.focus_list();
    app.handle_key(&key(KeyCode::Char('n')), now);
    app.handle_key(&key(KeyCode::Enter), now);
    app.handle_key(&key(KeyCode::Enter), now);
    assert!(
        pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
            a.sessions[0].status(Instant::now()) == Status::Idle
        })
        .await
    );
    let id = format!("session-{}", app.sessions[0].id);
    let t = Instant::now();
    let Some(stale) = t.checked_sub(WAKE_TTL) else {
        return; // the clock started less than WAKE_TTL ago
    };
    app.pending_bus_wakes = vec![
        PendingWake {
            launch_id: id.clone(),
            prompt: "echo stale-wake".into(),
            queued_at: stale,
        },
        PendingWake {
            launch_id: id,
            prompt: "echo fresh-wake".into(),
            queued_at: t,
        },
    ];
    app.flush_pending_bus_wakes(t);
    assert!(app.pending_bus_wakes.is_empty());
    assert!(
        pump_until(&mut rx, &mut app, Duration::from_secs(10), |a| {
            screen(a, 0).contains("fresh-wake")
        })
        .await
    );
    assert!(
        !screen(&app, 0).contains("stale-wake"),
        "{}",
        screen(&app, 0)
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
    let swarm = agent_mux::bus::swarm::record::create_swarm(
        &conn,
        Some(&db),
        &agent_mux::bus::swarm::record::NewSwarm {
            workspace: ws.clone(),
            goal: "g".into(),
            planner_harness: "claude".into(),
            max_workers: 2,
            isolation: TaskIsolation::Shared,
            gate: true,
            check_cmd: None,
            owner_run: None,
        },
        4_000,
    )
    .unwrap();
    advanced("create_swarm");
    agent_mux::bus::swarm::record::set_status(
        &conn,
        Some(&db),
        &swarm.id,
        agent_mux::bus::swarm::record::SwarmStatus::Running,
        5_000,
    )
    .unwrap();
    advanced("set_status");
}

#[test]
fn a_review_records_what_happened_to_a_branch() {
    let dir = tempfile::tempdir().unwrap();
    let db = open_store(dir.path());
    let conn = open_aux(&db).unwrap();
    let ws = bstore::normalize_workspace(dir.path());
    let seq = latest_change_seq(&conn).unwrap();
    bstore::record_review(
        &conn,
        Some(&db),
        &ws,
        "task-a",
        true,
        Some("loop/bus-a"),
        Some("abc123"),
        1,
    )
    .unwrap();
    bstore::record_review(
        &conn,
        Some(&db),
        &ws,
        "task-b",
        false,
        Some("loop/bus-b"),
        None,
        2,
    )
    .unwrap();
    assert!(latest_change_seq(&conn).unwrap() > seq);
    let reviews = bstore::reviews(&conn, &ws).unwrap();
    let got: Vec<_> = reviews
        .iter()
        .map(|r| (r.task_id.as_str(), r.verdict.as_str()))
        .collect();
    assert_eq!(got, [("task-a", "merged"), ("task-b", "discarded")]);
    assert_eq!(reviews[0].detail.as_deref(), Some("abc123"));
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository on `main` with a `feature` branch that changes `file`
/// to `feature` (or, with `clash`, a `main` that changes it too).
fn repo(dir: &Path, clash: bool) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("file"), "base\n").unwrap();
    git(dir, &["add", "file"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("file"), "feature\n").unwrap();
    git(dir, &["commit", "-q", "-am", "feature"]);
    git(dir, &["checkout", "-q", "main"]);
    if clash {
        std::fs::write(dir.join("file"), "main\n").unwrap();
        git(dir, &["commit", "-q", "-am", "main"]);
    }
}

#[test]
fn merging_a_branch_makes_a_merge_commit_and_a_conflict_changes_nothing() {
    let clean = tempfile::tempdir().unwrap();
    repo(clean.path(), false);
    let head = agent_mux::bus::swarm::merge_branch(clean.path(), "feature").unwrap();
    assert_eq!(git(clean.path(), &["rev-parse", "--short", "HEAD"]), head);
    assert_eq!(
        std::fs::read_to_string(clean.path().join("file")).unwrap(),
        "feature\n"
    );
    let parents = git(clean.path(), &["rev-list", "--parents", "-n", "1", "HEAD"]);
    assert_eq!(
        parents.split(' ').count(),
        3,
        "a merge commit has two parents"
    );

    let clash = tempfile::tempdir().unwrap();
    repo(clash.path(), true);
    let before = git(clash.path(), &["rev-parse", "HEAD"]);
    let err = agent_mux::bus::swarm::merge_branch(clash.path(), "feature").unwrap_err();
    assert!(
        err.contains("CONFLICT") && err.contains("merge aborted"),
        "{err}"
    );
    assert_eq!(git(clash.path(), &["rev-parse", "HEAD"]), before);
    assert_eq!(
        git(clash.path(), &["status", "--porcelain"]),
        "",
        "the workspace is as it was"
    );

    let err = agent_mux::bus::swarm::merge_branch(clean.path(), "no-such-branch").unwrap_err();
    assert!(!err.is_empty());
}
