//! End-to-end for OpenCode: the plan injects `--session <id>`, a fake
//! `opencode` (a shell script) records its argv and stays up while the test
//! writes the session's rows into a database in OpenCode's shape, and the
//! pipeline (correlate by the row's existence -> read rows -> assemble ->
//! store) must land the launch, session, turn and observation rows in the
//! trace store under the `opencode` provider.
#![cfg(unix)]

use agent_mux::app::{App, Mode};
use agent_mux::config::{self, Profile};
use agent_mux::events::AppEvent;
use agent_mux::status::Status;
use agent_mux::tracing::TraceRuntime;
use agent_mux::tracing::opencode_db::fixture;
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
        match tokio::time::timeout(remaining.min(Duration::from_millis(100)), rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                app.handle_pty_output(id, &bytes, Instant::now());
            }
            Ok(Some(AppEvent::PtyExit { id })) => app.handle_pty_exit(id),
            Ok(Some(AppEvent::TraceStats { launch_id, stats })) => {
                app.handle_trace_stats(&launch_id, stats)
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    pred(app)
}

const ASSISTANT: &str = r#"{"time":{"created":1793454359025,"streamed":1793454361672,"completed":1793454361768},"agent":"build","model":{"id":"big-pickle","providerID":"opencode","variant":"default"},"content":[{"type":"reasoning","text":"Let me look."},{"type":"text","text":"running"},{"type":"tool","id":"call_1","name":"shell","executed":false,"state":{"status":"completed","input":{"command":"cargo test"},"content":[{"type":"text","text":"ok"}],"metadata":{"exit":0}},"time":{"created":1793454361263,"ran":1793454361667,"completed":1793454361720}}],"finish":"stop","cost":0.25,"tokens":{"input":85,"output":2,"reasoning":0,"cache":{"read":16614,"write":0}}}"#;

#[tokio::test]
async fn an_opencode_launch_is_traced_from_its_database() {
    let temp = tempfile::tempdir().unwrap();
    let workdir = temp.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    let db_path = temp.path().join("store").join("traces.db");
    let opencode_db = temp.path().join("opencode").join("opencode.db");
    let conn = fixture::create(&opencode_db);
    let args_file = temp.path().join("args.txt");

    // the fake records what it was launched with and stays up a while
    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script_path = bin_dir.join("opencode");
    std::fs::write(
        &script_path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nsleep 3\nexit 0\n",
            args_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let toml = format!(
        r#"
        [tracing]
        db_path = "{db}"
        content_mode = "full"
        poll_interval_ms = 60
        flush_interval_ms = 40
        opencode_db = "{ocdb}"

        [[profiles]]
        name = "OpenCode"
        command = "{cmd}"
        args = []
        default_dir = "{dir}"
        "#,
        db = db_path.display(),
        ocdb = opencode_db.display(),
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
    assert!(matches!(app.mode, Mode::NewSession(_)));
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(
        matches!(app.mode, Mode::Main),
        "spawn failed: {:?}",
        app.notice
    );
    assert_eq!(app.sessions.len(), 1);
    assert!(app.sessions[0].trace.is_some(), "pipeline attached");

    // the fake is up: read the session id the plan injected
    let started = pump_until(&mut rx, &mut app, Duration::from_secs(10), |_| {
        args_file.is_file()
    })
    .await;
    assert!(started, "fake opencode never started");
    let args: Vec<String> = std::fs::read_to_string(&args_file)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let at = args
        .iter()
        .position(|a| a == "--session")
        .unwrap_or_else(|| panic!("no --session injected: {args:?}"));
    let sid = args[at + 1].clone();
    assert!(sid.starts_with("ses_"), "{sid}");
    assert!(
        args.iter().any(|a| a == "--standalone"),
        "the MCP fragment needs --standalone: {args:?}"
    );

    // OpenCode writes the session, then its rows, while the session runs
    fixture::session(&conn, &sid, &workdir.to_string_lossy(), 1793454359000);
    fixture::message(
        &conn,
        &sid,
        1,
        "user",
        r#"{"time":{"created":1793454359013},"text":"run the tests","files":[]}"#,
    );
    fixture::message(&conn, &sid, 2, "assistant", ASSISTANT);
    fixture::message(
        &conn,
        &sid,
        3,
        "idle",
        r#"{"time":{"created":1793454362000},"outcome":"succeeded"}"#,
    );

    let exited = pump_until(&mut rx, &mut app, Duration::from_secs(15), |a| {
        matches!(a.sessions[0].status(Instant::now()), Status::Exited(_))
    })
    .await;
    assert!(exited, "fake opencode never exited");

    app.kill_all();
    let rt = app.take_tracing().unwrap();
    rt.shutdown(Duration::from_secs(5)).await;

    let conn = open_ro(&db_path).unwrap();
    let (termination, correlation, session_key): (String, String, String) = conn
        .query_row(
            "SELECT termination, correlation, session_key FROM launches",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(termination, "exit");
    assert_eq!(correlation, "deterministic");
    assert_eq!(session_key, format!("opencode:{sid}"));
    let (provider, cwd, transcript): (String, String, String) = conn
        .query_row(
            "SELECT provider, cwd, transcript_path FROM sessions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(provider, "opencode");
    assert_eq!(cwd, workdir.to_string_lossy());
    assert_eq!(transcript, opencode_db.to_string_lossy());
    let (status, name, input, output, provided): (String, String, String, String, Option<f64>) =
        conn.query_row(
            "SELECT status, name, input, output, json_extract(metadata, '$.provided_cost.total') FROM traces WHERE ordinal = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(status, "closed", "the idle row closes the turn");
    assert_eq!(name, "OpenCode: run the tests");
    assert_eq!(input, "run the tests");
    assert_eq!(output, "running");
    assert_eq!(provided, Some(0.25), "opencode's own cost is kept");
    let (model_id, input_tokens, cache_read): (String, i64, Option<i64>) = conn
        .query_row(
            // `model` as opencode names it; `model_id` is the price table's
            // normalized id, which a model without a price row has not
            "SELECT model, input_tokens, cache_read_tokens FROM observations WHERE name = 'assistant'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert!(model_id.ends_with("big-pickle"), "{model_id}");
    assert_eq!(input_tokens, 85);
    assert_eq!(cache_read, Some(16614));
    let (tool_end, tool_output): (Option<i64>, String) = conn
        .query_row(
            "SELECT end_ns, output FROM observations WHERE name = 'shell'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(tool_end.is_some(), "tool row closed on result");
    assert_eq!(tool_output, "ok");
}

#[tokio::test]
async fn a_resumed_opencode_session_primes_its_history() {
    // `--session <id>` on the command line: the rows so far are history
    // (read, never re-recorded); only new rows become turns
    let temp = tempfile::tempdir().unwrap();
    let workdir = temp.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    let db_path = temp.path().join("store").join("traces.db");
    let opencode_db = temp.path().join("opencode").join("opencode.db");
    let conn = fixture::create(&opencode_db);
    fixture::session(&conn, "ses_old", &workdir.to_string_lossy(), 1793454359000);
    fixture::message(
        &conn,
        "ses_old",
        1,
        "user",
        r#"{"time":{"created":1793454359013},"text":"earlier prompt","files":[]}"#,
    );
    fixture::message(&conn, "ses_old", 2, "assistant", ASSISTANT);
    fixture::message(
        &conn,
        "ses_old",
        3,
        "idle",
        r#"{"time":{"created":1793454362000},"outcome":"succeeded"}"#,
    );

    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script_path = bin_dir.join("opencode");
    std::fs::write(&script_path, "#!/bin/sh\nsleep 2\nexit 0\n").unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let toml = format!(
        r#"
        [tracing]
        db_path = "{db}"
        content_mode = "full"
        poll_interval_ms = 60
        flush_interval_ms = 40
        opencode_db = "{ocdb}"

        [[profiles]]
        name = "OpenCode"
        command = "{cmd}"
        args = ["--session", "ses_old"]
        default_dir = "{dir}"
        "#,
        db = db_path.display(),
        ocdb = opencode_db.display(),
        cmd = script_path.display(),
        dir = workdir.display(),
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    let (tx, mut rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let mut app = App::new(cfg.profiles, Some(runtime), tx);
    app.clipboard_enabled = false;
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(
        matches!(app.mode, Mode::Main),
        "spawn failed: {:?}",
        app.notice
    );

    // a new prompt arrives while the session runs
    let _ = pump_until(&mut rx, &mut app, Duration::from_millis(600), |_| false).await;
    fixture::message(
        &conn,
        "ses_old",
        4,
        "user",
        r#"{"time":{"created":1793454400000},"text":"and now this","files":[]}"#,
    );
    let exited = pump_until(&mut rx, &mut app, Duration::from_secs(15), |a| {
        matches!(a.sessions[0].status(Instant::now()), Status::Exited(_))
    })
    .await;
    assert!(exited, "fake opencode never exited");
    app.kill_all();
    let rt = app.take_tracing().unwrap();
    rt.shutdown(Duration::from_secs(5)).await;

    let conn = open_ro(&db_path).unwrap();
    let (correlation, session_key): (String, String) = conn
        .query_row("SELECT correlation, session_key FROM launches", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(correlation, "deterministic");
    assert_eq!(session_key, "opencode:ses_old");
    let inputs: Vec<String> = conn
        .prepare("SELECT input FROM traces ORDER BY ordinal")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        inputs,
        vec!["and now this"],
        "history primed, not re-recorded"
    );
}
