//! End-to-end: a fake `claude` binary (a shell script) reads the injected
//! `--session-id` from its argv, appends Claude-shaped JSONL into a temp
//! claude_dir, and exits; the full pipeline (plan -> spawn -> correlate ->
//! tail -> assemble -> store) must land the launch, session, turn, and
//! observation rows in the SQLite store, with the flush completing after
//! `kill_all` within the shutdown deadline.
#![cfg(unix)]

use agent_mux::app::{App, Mode};
use agent_mux::config::{self, Profile};
use agent_mux::events::AppEvent;
use agent_mux::status::Status;
use agent_mux::tracing::TraceRuntime;
use agent_mux::tracing::store::open_ro;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

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
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                app.handle_pty_output(id, &bytes, Instant::now());
            }
            Ok(Some(AppEvent::PtyExit { id })) => app.handle_pty_exit(id),
            Ok(Some(AppEvent::TraceStats { launch_id, stats })) => {
                app.handle_trace_stats(&launch_id, stats)
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    pred(app)
}

#[tokio::test]
async fn full_pipeline_stores_launch_session_turn_and_observations() {
    let temp = tempfile::tempdir().unwrap();
    let claude_dir = temp.path().join("claude-home");
    let workdir = temp.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    let db_path = temp.path().join("store").join("traces.db");
    let projects_dir = claude_dir
        .join("projects")
        .join(agent_mux::history::project_slug(&workdir));
    std::fs::create_dir_all(&projects_dir).unwrap();

    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script_path = bin_dir.join("claude");
    let script = format!(
        r#"#!/bin/sh
sid=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--session-id" ]; then sid="$a"; fi
  prev="$a"
done
[ -n "$sid" ] || exit 3
out="{proj}/$sid.jsonl"
printf '%s\n' '{{"type":"user","timestamp":"2026-08-31T10:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":[{{"type":"text","text":"run the tests"}}]}}}}' >> "$out"
sleep 0.2
printf '%s\n' '{{"type":"assistant","timestamp":"2026-08-31T10:00:01Z","message":{{"role":"assistant","model":"claude-haiku-4-5-20251001","usage":{{"input_tokens":42,"output_tokens":7}},"content":[{{"type":"text","text":"running"}},{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":"cargo test"}}}}]}}}}' >> "$out"
printf '%s\n' '{{"type":"user","timestamp":"2026-08-31T10:00:02Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"ok","is_error":false}}]}}}}' >> "$out"
sleep 0.5
exit 0
"#,
        proj = projects_dir.display(),
        cwd = workdir.display(),
    );
    std::fs::write(&script_path, script).unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let toml = format!(
        r#"
        [tracing]
        db_path = "{db}"
        content_mode = "full"
        poll_interval_ms = 60
        flush_interval_ms = 40
        claude_dir = "{claude}"

        [[profiles]]
        name = "Claude Code"
        command = "{cmd}"
        args = []
        default_dir = "{dir}"
        "#,
        db = db_path.display(),
        claude = claude_dir.display(),
        cmd = script_path.display(),
        dir = workdir.display(),
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    let profiles: Vec<Profile> = cfg.profiles;

    let (tx, mut rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let mut app = App::new(profiles, Some(runtime), tx);
    app.clipboard_enabled = false;

    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    assert!(matches!(app.mode, Mode::NewSession(_)));
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(
        matches!(app.mode, Mode::Control),
        "spawn failed: {:?}",
        app.notice
    );
    assert_eq!(app.sessions.len(), 1);
    assert!(app.sessions[0].trace.is_some(), "pipeline attached");

    let exited = pump_until(&mut rx, &mut app, Duration::from_secs(15), |a| {
        matches!(a.sessions[0].status(Instant::now()), Status::Exited(_))
    })
    .await;
    assert!(exited, "fake claude never exited");
    assert_eq!(
        app.sessions[0].status(Instant::now()),
        Status::Exited(Some(0))
    );

    app.kill_all();
    let rt = app.take_tracing().unwrap();
    let flush_started = Instant::now();
    rt.shutdown(Duration::from_secs(5)).await;
    assert!(
        flush_started.elapsed() < Duration::from_secs(5),
        "shutdown overstayed: {:?}",
        flush_started.elapsed()
    );

    let conn = open_ro(&db_path).unwrap();
    let (termination, exit_code, correlation, session_key): (String, i64, String, String) = conn
        .query_row(
            "SELECT termination, exit_code, correlation, session_key FROM launches",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(termination, "exit");
    assert_eq!(exit_code, 0);
    assert_eq!(correlation, "deterministic");
    assert!(session_key.starts_with("claude:"));
    let (status, name, input, output): (String, String, String, String) = conn
        .query_row(
            "SELECT status, name, input, output FROM traces WHERE ordinal = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(status, "closed");
    assert_eq!(name, "Claude Code: run the tests");
    assert_eq!(input, "run the tests");
    assert_eq!(output, "running");
    let (model_id, input_tokens, cost): (String, i64, f64) = conn
        .query_row(
            "SELECT model_id, input_tokens, total_cost_usd FROM observations WHERE name = 'assistant'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(model_id, "claude-haiku-4-5");
    assert_eq!(input_tokens, 42);
    assert!(cost > 0.0);
    let (tool_end, tool_output): (Option<i64>, String) = conn
        .query_row(
            "SELECT end_ns, output FROM observations WHERE name = 'Bash'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(tool_end.is_some(), "tool row closed on result");
    assert_eq!(tool_output, "ok");
    let (title, cwd): (String, String) = conn
        .query_row("SELECT title, cwd FROM sessions", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(title, "run the tests");
    assert_eq!(cwd, workdir.to_string_lossy());
    // run ended cleanly
    let ended: Option<i64> = conn
        .query_row("SELECT ended_ns FROM runs", [], |r| r.get(0))
        .unwrap();
    assert!(ended.is_some());
}

#[tokio::test]
async fn toggle_tracing_attaches_and_stops_on_demand() {
    let temp = tempfile::tempdir().unwrap();
    let claude_dir = temp.path().join("claude-home");
    let workdir = temp.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    let db_path = temp.path().join("traces.db");

    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script_path = bin_dir.join("claude");
    std::fs::write(&script_path, "#!/bin/sh\nsleep 0.8\nexit 0\n").unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let toml = format!(
        r#"
        [tracing]
        db_path = "{db}"
        poll_interval_ms = 60
        flush_interval_ms = 40
        claude_dir = "{claude}"

        [[profiles]]
        name = "Claude Code"
        command = "{cmd}"
        args = []
        default_dir = "{dir}"
        [profiles.tracing]
        enabled = false
        "#,
        db = db_path.display(),
        claude = claude_dir.display(),
        cmd = script_path.display(),
        dir = workdir.display(),
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    let profiles: Vec<Profile> = cfg.profiles;

    let (tx, mut rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let mut app = App::new(profiles, Some(runtime), tx);
    app.clipboard_enabled = false;

    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert_eq!(app.sessions.len(), 1);
    assert!(
        app.sessions[0].trace.is_none(),
        "initially untraced because profile enabled = false"
    );

    let notice_text = |app: &App| {
        app.notice
            .as_ref()
            .map(|n| n.text.clone())
            .unwrap_or_default()
    };
    app.handle_key(&key(KeyCode::Char('t')), Instant::now());
    assert!(
        app.sessions[0].trace.is_some(),
        "tracing attached after pressing 't'"
    );
    assert!(notice_text(&app).contains("tracing started"));
    assert_eq!(
        app.notice.as_ref().map(|n| n.level),
        Some(agent_mux::app::NoticeLevel::Info)
    );
    app.handle_key(&key(KeyCode::Char('t')), Instant::now());
    assert!(
        app.sessions[0].trace.is_none(),
        "tracing detached after pressing 't' again"
    );
    assert!(notice_text(&app).contains("tracing stopped"));

    pump_until(&mut rx, &mut app, Duration::from_secs(5), |a| {
        matches!(a.sessions[0].status(Instant::now()), Status::Exited(_))
    })
    .await;

    app.kill_all();
    let rt = app.take_tracing().unwrap();
    rt.shutdown(Duration::from_secs(3)).await;

    let conn = open_ro(&db_path).unwrap();
    let (attached, termination, correlation): (i64, String, String) = conn
        .query_row(
            "SELECT attached, termination, correlation FROM launches",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(attached, 1, "attached via the toggle");
    assert_eq!(termination, "stopped");
    assert_eq!(
        correlation, "none",
        "nothing to adopt for a script that writes no transcript"
    );
}
