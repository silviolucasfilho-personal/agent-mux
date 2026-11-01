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
    app.handle_key(&key(KeyCode::Enter), Instant::now()); // the chooser starts on a session
    assert!(matches!(app.mode, Mode::NewSession(_)));
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(
        matches!(app.mode, Mode::Main),
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
    app.handle_key(&key(KeyCode::Enter), Instant::now()); // the chooser starts on a session
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
    app.focus_list();
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

/// A Claude session that forks or continues writes a `continued-in` line
/// and then keeps talking in a NEW transcript. Before this was followed,
/// the launch stayed live against a file that had stopped growing: the
/// open turn never closed and none of the successor's work was recorded.
#[tokio::test]
async fn a_continued_session_is_followed_into_its_successor_transcript() {
    const NEXT: &str = "b2d35ace-0000-4000-8000-000000000002";
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
    // The successor file repeats the whole conversation (that is what a
    // fork does), then carries the new work. It is written in full before
    // the hand-off line so the test does not race the prime pass.
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
next="{proj}/{next}.jsonl"
printf '%s\n' '{{"type":"user","timestamp":"2026-09-06T10:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":[{{"type":"text","text":"first prompt"}}]}}}}' >> "$out"
printf '%s\n' '{{"type":"assistant","timestamp":"2026-09-06T10:00:01Z","message":{{"role":"assistant","model":"claude-haiku-4-5-20251001","usage":{{"input_tokens":10,"output_tokens":2}},"content":[{{"type":"text","text":"answering"}}]}}}}' >> "$out"
sleep 0.8
# the fork's own file: the copied history, verbatim but re-keyed
printf '%s\n' '{{"type":"user","timestamp":"2026-09-06T10:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":[{{"type":"text","text":"first prompt"}}]}}}}' >> "$next"
printf '%s\n' '{{"type":"assistant","timestamp":"2026-09-06T10:00:01Z","message":{{"role":"assistant","model":"claude-haiku-4-5-20251001","usage":{{"input_tokens":10,"output_tokens":2}},"content":[{{"type":"text","text":"answering"}}]}}}}' >> "$next"
sleep 0.3
printf '%s\n' '{{"type":"continued-in","timestamp":"2026-09-06T10:00:02Z","sessionId":"'"$sid"'","continuedInSessionId":"{next}"}}' >> "$out"
sleep 0.9
# work that only ever exists in the successor
printf '%s\n' '{{"type":"user","timestamp":"2026-09-06T10:00:10Z","cwd":"{cwd}","message":{{"role":"user","content":[{{"type":"text","text":"second prompt"}}]}}}}' >> "$next"
printf '%s\n' '{{"type":"assistant","timestamp":"2026-09-06T10:00:11Z","message":{{"role":"assistant","model":"claude-haiku-4-5-20251001","usage":{{"input_tokens":20,"output_tokens":4}},"content":[{{"type":"text","text":"still here"}},{{"type":"tool_use","id":"t9","name":"Bash","input":{{"command":"echo hi"}}}}]}}}}' >> "$next"
printf '%s\n' '{{"type":"user","timestamp":"2026-09-06T10:00:12Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t9","content":"hi","is_error":false}}]}}}}' >> "$next"
sleep 0.9
exit 0
"#,
        proj = projects_dir.display(),
        cwd = workdir.display(),
        next = NEXT,
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

    let (tx, mut rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let mut app = App::new(cfg.profiles, Some(runtime), tx);
    app.clipboard_enabled = false;

    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    app.handle_key(&key(KeyCode::Enter), Instant::now()); // the chooser starts on a session
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(app.mode, Mode::Main), "spawn: {:?}", app.notice);

    let exited = pump_until(&mut rx, &mut app, Duration::from_secs(20), |a| {
        matches!(a.sessions[0].status(Instant::now()), Status::Exited(_))
    })
    .await;
    assert!(exited, "fake claude never exited");

    app.kill_all();
    let rt = app.take_tracing().unwrap();
    rt.shutdown(Duration::from_secs(5)).await;

    let conn = open_ro(&db_path).unwrap();

    // the launch now points at the session actually being written
    let session_key: String = conn
        .query_row("SELECT session_key FROM launches", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        session_key,
        format!("claude:{NEXT}"),
        "launch follows the successor"
    );

    // both sessions are on record, not just the one that ended
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sessions, 2, "predecessor and successor both stored");

    // the predecessor's turn is closed and says where the talk went
    let (status, continued): (String, Option<String>) = conn
        .query_row(
            "SELECT t.status, json_extract(t.metadata, '$.continued_in')
             FROM traces t WHERE t.session_key != ?1",
            [&session_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "closed", "the handed-off turn does not stay open");
    assert_eq!(continued.as_deref(), Some(NEXT));

    // the successor's new turn is recorded, and its copied history is not
    // re-exported as a second one
    let successor_turns: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM traces WHERE session_key = ?1",
            [&session_key],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        successor_turns, 1,
        "primed history is replayed, not re-recorded"
    );
    let (input, output): (String, String) = conn
        .query_row(
            "SELECT input, output FROM traces WHERE session_key = ?1",
            [&session_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(input, "second prompt");
    assert_eq!(output, "still here");

    // and the work inside it lands, which is the whole point
    let tool_output: String = conn
        .query_row(
            "SELECT o.output FROM observations o JOIN traces t ON t.id = o.trace_id
             WHERE t.session_key = ?1 AND o.name = 'Bash'",
            [&session_key],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tool_output, "hi");
}

/// The quit path: a traced session still running when agent-mux exits is
/// saved with the conversation its launch was correlated to, so the next
/// start resumes it instead of opening a blank one.
#[tokio::test]
async fn a_running_traced_session_is_saved_with_its_conversation() {
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
printf '%s\n' '{{"type":"user","timestamp":"2026-08-31T10:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":[{{"type":"text","text":"hello"}}]}}}}' >> "{proj}/$sid.jsonl"
sleep 30
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
    let (tx, _rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let mut app = App::new(cfg.profiles, Some(runtime), tx);
    app.clipboard_enabled = false;
    let sessions_path = temp.path().join("sessions.json");
    app.set_sessions_file(&sessions_path);

    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    app.handle_key(&key(KeyCode::Enter), Instant::now()); // the chooser starts on a session
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert_eq!(app.sessions.len(), 1, "spawn failed: {:?}", app.notice);

    // wait for the pipeline to record which conversation the launch is
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut key_in_store: Option<String> = None;
    while Instant::now() < deadline && key_in_store.is_none() {
        tokio::time::sleep(Duration::from_millis(50)).await;
        key_in_store = open_ro(&db_path).ok().and_then(|c| {
            c.query_row(
                "SELECT session_key FROM launches WHERE session_key IS NOT NULL",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok()
        });
    }
    let key_in_store = key_in_store.expect("the launch was never correlated");
    let id = key_in_store.strip_prefix("claude:").unwrap().to_string();

    app.save_active_sessions().unwrap();
    let saved = agent_mux::persistence::load_saved_sessions(&sessions_path);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].conversation.as_deref(), Some(id.as_str()));

    app.kill_all();
    if let Some(rt) = app.take_tracing() {
        rt.shutdown(Duration::from_secs(5)).await;
    }
}

/// An in-session `/clear` or `/resume` fires a `SessionStart` hook event with
/// the new session id and transcript path. The pipeline must follow the continuation,
/// re-point the launch's `session_key` to the new session, and `save_active_sessions`
/// must save the newest conversation so a subsequent restart restores the last session,
/// not the penultimate one.
#[tokio::test]
async fn a_session_cleared_or_resumed_via_hook_follows_continuation_and_persists_latest() {
    use agent_mux::tracing::hooks::{ContentPolicy, HookSource, parse};
    use agent_mux::tracing::store::{insert_hook_event, open_hook_sink, poke_wake};

    const NEXT: &str = "b2d35ace-0000-4000-8000-000000000099";
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

    // Fake claude starts with an initial session and stays alive waiting
    let script = format!(
        r#"#!/bin/sh
sid=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--session-id" ]; then sid="$a"; fi
  prev="$a"
done
[ -n "$sid" ] || exit 3
printf '%s\n' '{{"type":"user","timestamp":"2026-09-01T10:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":[{{"type":"text","text":"hello"}}]}}}}' >> "{proj}/$sid.jsonl"
printf '%s\n' '{{"type":"assistant","timestamp":"2026-09-01T10:00:01Z","message":{{"role":"assistant","model":"claude-haiku-4-5-20251001","usage":{{"input_tokens":10,"output_tokens":2}},"content":[{{"type":"text","text":"hi"}}]}}}}' >> "{proj}/$sid.jsonl"
sleep 30
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
        poll_interval_ms = 50
        flush_interval_ms = 30
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
    let (tx, mut rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let mut app = App::new(cfg.profiles.clone(), Some(runtime), tx);
    app.clipboard_enabled = false;
    let sessions_path = temp.path().join("sessions.json");
    app.set_sessions_file(&sessions_path);

    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert_eq!(app.sessions.len(), 1, "spawn failed: {:?}", app.notice);

    let launch_id = app.sessions[0].trace.as_ref().unwrap().launch_id.clone();

    // Wait for the pipeline to correlate the initial session
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut initial_sid: Option<String> = None;
    while Instant::now() < deadline && initial_sid.is_none() {
        tokio::time::sleep(Duration::from_millis(50)).await;
        initial_sid = open_ro(&db_path).ok().and_then(|c| {
            c.query_row(
                "SELECT session_key FROM launches WHERE id = ?1 AND session_key IS NOT NULL",
                [&launch_id],
                |r| r.get::<_, String>(0),
            )
            .ok()
        });
    }
    let initial_key = initial_sid.expect("initial session was not correlated");
    let initial_id = initial_key.strip_prefix("claude:").unwrap().to_string();
    assert_ne!(initial_id, NEXT);

    // Now write the successor transcript (as Claude does on /clear or /resume)
    let next_transcript = projects_dir.join(format!("{NEXT}.jsonl"));
    std::fs::write(
        &next_transcript,
        format!(
            "{{\"type\":\"user\",\"timestamp\":\"2026-09-01T10:05:00Z\",\"cwd\":\"{}\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"resumed work\"}}]}}}}\n{{\"type\":\"assistant\",\"timestamp\":\"2026-09-01T10:05:01Z\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-haiku-4-5-20251001\",\"usage\":{{\"input_tokens\":10,\"output_tokens\":2}},\"content\":[{{\"type\":\"text\",\"text\":\"resumed response\"}}]}}}}\n",
            workdir.display()
        ),
    )
    .unwrap();

    // Fire the SessionStart hook announcing NEXT (with source "clear")
    let sink = open_hook_sink(&db_path, Duration::from_millis(500)).unwrap();
    let policy = ContentPolicy::full();
    let start_event = parse(
        HookSource::Claude,
        None,
        &serde_json::json!({
            "session_id": NEXT,
            "hook_event_name": "SessionStart",
            "source": "clear",
            "cwd": workdir.to_string_lossy(),
            "transcript_path": next_transcript.to_string_lossy(),
        }),
        &policy,
        agent_mux::tracing::store::now_ns(),
        Some(launch_id.clone()),
    )
    .unwrap();
    insert_hook_event(&sink, &start_event).unwrap();
    drop(sink);
    poke_wake(&db_path);

    // Pump events until launches.session_key points to claude:NEXT
    let followed = pump_until(&mut rx, &mut app, Duration::from_secs(10), |_| {
        open_ro(&db_path)
            .ok()
            .and_then(|c| {
                c.query_row(
                    "SELECT session_key FROM launches WHERE id = ?1",
                    [&launch_id],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
            .as_deref()
            == Some(&format!("claude:{NEXT}"))
    })
    .await;
    assert!(followed, "pipeline did not follow continuation into NEXT");

    // Save active sessions: must record NEXT, not initial_id
    app.save_active_sessions().unwrap();
    let saved = agent_mux::persistence::load_saved_sessions(&sessions_path);
    assert_eq!(saved.len(), 1);
    assert_eq!(
        saved[0].conversation.as_deref(),
        Some(NEXT),
        "persisted conversation must be NEXT (latest), not initial_id (anti-last)"
    );

    // Kill app1 and restore in app2
    app.kill_all();
    if let Some(rt) = app.take_tracing() {
        rt.shutdown(Duration::from_secs(5)).await;
    }

    let (tx2, _rx2) = mpsc::channel(1024);
    let mut app2 = App::new(cfg.profiles, None, tx2);
    app2.set_sessions_file(&sessions_path);
    app2.restore_saved_sessions();

    assert_eq!(app2.sessions.len(), 1);
    assert_eq!(
        app2.sessions[0].profile.args,
        vec!["--dangerously-skip-permissions", "--resume", NEXT],
        "restored session must resume NEXT (latest), not initial_id (anti-last)"
    );
    app2.kill_all();
}

#[tokio::test]
async fn an_antigravity_session_persists_newest_conversation_on_disk_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let brain_dir = temp.path().join("brain");
    let workdir = temp.path().join("workspace");
    std::fs::create_dir_all(&workdir).unwrap();

    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let agy_bin = bin_dir.join("agy");
    std::fs::write(&agy_bin, "#!/bin/sh\nsleep 30\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&agy_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let old_conv = "agy-old-session-001";
    let new_conv = "agy-new-session-002";

    // Create older transcript for workdir
    let old_dir = brain_dir
        .join(old_conv)
        .join(".system_generated")
        .join("logs");
    std::fs::create_dir_all(&old_dir).unwrap();
    std::fs::write(
        old_dir.join("transcript.jsonl"),
        format!(
            "{{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"created_at\":\"2026-08-30T10:00:00Z\",\"content\":\"work in {}\"}}\n",
            workdir.display()
        ),
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(50));

    // Create newer transcript for workdir
    let new_dir = brain_dir
        .join(new_conv)
        .join(".system_generated")
        .join("logs");
    std::fs::create_dir_all(&new_dir).unwrap();
    std::fs::write(
        new_dir.join("transcript.jsonl"),
        format!(
            "{{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"created_at\":\"2026-08-30T11:00:00Z\",\"content\":\"more work in {}\"}}\n",
            workdir.display()
        ),
    )
    .unwrap();

    let (tx, _rx) = mpsc::channel(1024);
    let profile = Profile {
        name: "Antigravity".into(),
        command: agy_bin.to_string_lossy().into_owned(),
        args: vec![],
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
    };
    let mut app = App::new(vec![profile.clone()], None, tx);
    app.antigravity_dir = Some(brain_dir.clone());
    let sessions_path = temp.path().join("sessions.json");
    app.set_sessions_file(&sessions_path);

    let session = agent_mux::session::Session::spawn(
        0,
        profile,
        workdir.clone(),
        24,
        80,
        mpsc::channel(100).0,
        &[],
        &[],
    )
    .unwrap();
    app.sessions.push(session);

    // Save active sessions: must record new_conv because disk has the latest active session!
    app.save_active_sessions().unwrap();
    let saved = agent_mux::persistence::load_saved_sessions(&sessions_path);
    assert_eq!(saved.len(), 1);
    assert_eq!(
        saved[0].conversation.as_deref(),
        Some(new_conv),
        "persisted conversation must be the newest on disk ({new_conv}), not the stale old one ({old_conv})"
    );

    // Restart into app2: must restore new_conv
    app.kill_all();
    let (tx2, _rx2) = mpsc::channel(1024);
    let mut app2 = App::new(vec![], None, tx2);
    app2.antigravity_dir = Some(brain_dir);
    app2.set_sessions_file(&sessions_path);
    app2.restore_saved_sessions();

    assert_eq!(app2.sessions.len(), 1);
    assert_eq!(
        app2.sessions[0].profile.args,
        vec!["--conversation", new_conv],
        "restored session must resume newest conversation {new_conv}"
    );
    app2.kill_all();
}

#[tokio::test]
async fn a_claude_session_persists_newest_conversation_on_disk_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let claude_dir = temp.path().join("claude");
    let workdir = temp.path().join("workspace");
    std::fs::create_dir_all(&workdir).unwrap();

    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let claude_bin = bin_dir.join("claude");
    std::fs::write(&claude_bin, "#!/bin/sh\nsleep 30\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let old_conv = "claude-old-session-001";
    let new_conv = "claude-new-session-002";

    let proj_dir = claude_dir
        .join("projects")
        .join(agent_mux::history::project_slug(&workdir));
    std::fs::create_dir_all(&proj_dir).unwrap();

    // Older transcript
    std::fs::write(
        proj_dir.join(format!("{old_conv}.jsonl")),
        format!(
            "{{\"type\":\"user\",\"timestamp\":\"2026-08-30T10:00:00Z\",\"cwd\":\"{}\"}}\n",
            workdir.display()
        ),
    )
    .unwrap();

    std::thread::sleep(Duration::from_millis(50));

    // Newer transcript
    std::fs::write(
        proj_dir.join(format!("{new_conv}.jsonl")),
        format!(
            "{{\"type\":\"user\",\"timestamp\":\"2026-08-30T11:00:00Z\",\"cwd\":\"{}\"}}\n",
            workdir.display()
        ),
    )
    .unwrap();

    let (tx, _rx) = mpsc::channel(1024);
    let profile = Profile {
        name: "Claude Code".into(),
        command: claude_bin.to_string_lossy().into_owned(),
        args: vec!["--resume".into(), old_conv.into()],
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
    };
    let mut app = App::new(vec![profile.clone()], None, tx);
    app.claude_dir = Some(claude_dir.clone());
    let sessions_path = temp.path().join("sessions.json");
    app.set_sessions_file(&sessions_path);

    let session = agent_mux::session::Session::spawn(
        0,
        profile,
        workdir.clone(),
        24,
        80,
        mpsc::channel(100).0,
        &[],
        &[],
    )
    .unwrap();
    app.sessions.push(session);

    // Save active sessions: must record new_conv because disk has the latest active session!
    app.save_active_sessions().unwrap();
    let saved = agent_mux::persistence::load_saved_sessions(&sessions_path);
    assert_eq!(saved.len(), 1);
    assert_eq!(
        saved[0].conversation.as_deref(),
        Some(new_conv),
        "persisted conversation must be the newest on disk ({new_conv}), not the stale old one ({old_conv})"
    );

    // Restart into app2: must restore new_conv
    app.kill_all();
    let (tx2, _rx2) = mpsc::channel(1024);
    let mut app2 = App::new(vec![], None, tx2);
    app2.claude_dir = Some(claude_dir);
    app2.set_sessions_file(&sessions_path);
    app2.restore_saved_sessions();

    assert_eq!(app2.sessions.len(), 1);
    assert_eq!(
        app2.sessions[0].profile.args,
        vec!["--resume", new_conv],
        "restored session must resume newest conversation {new_conv}"
    );
    app2.kill_all();
}

/// A sessions file saved by an older agent-mux may name one agy
/// conversation for several panes; resuming it in each made every pane
/// after the first say "Conversation already open". The first resumes it,
/// the rest start a new conversation, and the next save keeps them apart.
#[tokio::test]
async fn sessions_sharing_one_conversation_restore_only_one_onto_it() {
    let temp = tempfile::tempdir().unwrap();
    let workdir = temp.path().join("workspace");
    std::fs::create_dir_all(&workdir).unwrap();
    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let agy_bin = bin_dir.join("agy");
    std::fs::write(&agy_bin, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&agy_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    let conv = "3dab1569-3974-4883-880f-efa63cff9272";
    let profile = Profile {
        name: "Antigravity".into(),
        command: agy_bin.to_string_lossy().into_owned(),
        args: vec![
            "--dangerously-skip-permissions".into(),
            "--conversation".into(),
            conv.into(),
        ],
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
    };
    let entry = agent_mux::persistence::SavedSession {
        profile,
        dir: workdir.clone(),
        skill_id: None,
        conversation: Some(conv.into()),
        continued_from: None,
    };
    let sessions_path = temp.path().join("sessions.json");
    agent_mux::persistence::save_sessions(&sessions_path, &vec![entry; 3]).unwrap();

    let (tx, _rx) = mpsc::channel(1024);
    let mut app = App::new(vec![], None, tx);
    app.antigravity_dir = Some(temp.path().join("brain"));
    app.set_sessions_file(&sessions_path);
    app.restore_saved_sessions();

    assert_eq!(app.sessions.len(), 3);
    assert_eq!(
        app.sessions[0].profile.args,
        vec!["--dangerously-skip-permissions", "--conversation", conv]
    );
    for s in &app.sessions[1..] {
        assert_eq!(
            s.profile.args,
            vec!["--dangerously-skip-permissions"],
            "a second pane must not resume a conversation another holds"
        );
        assert_eq!(s.conversation, None);
    }

    let saved = agent_mux::persistence::load_saved_sessions(&sessions_path);
    let convs: Vec<_> = saved.iter().map(|s| s.conversation.as_deref()).collect();
    assert_eq!(convs, vec![Some(conv), None, None]);
    app.kill_all();
}

/// What the store says each workspace held wins over a dropped or missing
/// conversation: a pane in another folder that was saved onto one agy
/// conversation (the guess from disk matched a transcript that only
/// mentioned its folder) comes back as the newest conversation launched in
/// its own folder, and so does one saved without any.
#[tokio::test]
async fn restored_agy_sessions_take_their_own_workspace_conversation_from_the_store() {
    let temp = tempfile::tempdir().unwrap();
    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let agy_bin = bin_dir.join("agy");
    std::fs::write(&agy_bin, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&agy_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ws = |n: &str| {
        let d = temp.path().join(n);
        std::fs::create_dir_all(&d).unwrap();
        d
    };
    let (a, b, c) = (ws("explorable"), ws("shareable"), ws("langfuse"));

    let db = temp.path().join("traces.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        for m in agent_mux::tracing::store::schema::MIGRATIONS {
            conn.execute_batch(m).unwrap();
        }
        conn.pragma_update(
            None,
            "user_version",
            agent_mux::tracing::store::schema::SCHEMA_VERSION,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runs (id, pid, agent_mux_version, started_ns, heartbeat_ns)
             VALUES ('r', 1, '0', 1, 1)",
            [],
        )
        .unwrap();
        let session = |conv: &str, cwd: &std::path::Path, seen: i64| {
            conn.execute(
                "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
                 VALUES (?1, 'antigravity', ?2, ?3, ?4, ?4)",
                rusqlite::params![
                    format!("antigravity:{conv}"),
                    conv,
                    cwd.to_str().unwrap(),
                    seen
                ],
            )
            .unwrap();
        };
        let launch = |id: &str, cwd: &std::path::Path, conv: &str| {
            conn.execute(
                "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd,
                   project_slug, content_mode, correlation_plan, session_key, started_ns,
                   agent_mux_version)
                 VALUES (?1, 'r', 1, 'agy', 'antigravity', ?2, 'p', 'full', 'none', ?3, 1, '0')",
                rusqlite::params![id, cwd.to_str().unwrap(), format!("antigravity:{conv}")],
            )
            .unwrap();
        };
        session("conv-a", &a, 30);
        session("conv-b", &b, 10);
        session("conv-c", &c, 10);
        session("conv-c-older", &c, 5);
        launch("la", &a, "conv-a");
        launch("lb", &b, "conv-b");
        launch("lc", &c, "conv-c");
        launch("lc0", &c, "conv-c-older");
        // the bad restore: shareable's pane launched onto explorable's conversation
        launch("lb-bad", &b, "conv-a");
    }

    let entry = |dir: &std::path::Path, conv: Option<&str>| {
        let mut args = vec!["--dangerously-skip-permissions".to_string()];
        if let Some(conv) = conv {
            args.extend(["--conversation".into(), conv.into()]);
        }
        agent_mux::persistence::SavedSession {
            profile: Profile {
                name: "Antigravity".into(),
                command: agy_bin.to_string_lossy().into_owned(),
                args,
                default_dir: None,
                tracing: None,
                model: None,
                bypass_approvals: None,
            },
            dir: dir.to_path_buf(),
            skill_id: None,
            conversation: conv.map(Into::into),
            continued_from: None,
        }
    };
    let sessions_path = temp.path().join("sessions.json");
    agent_mux::persistence::save_sessions(
        &sessions_path,
        &[
            entry(&a, Some("conv-a")),
            entry(&b, Some("conv-a")),
            entry(&c, None),
        ],
    )
    .unwrap();

    let (tx, _rx) = mpsc::channel(1024);
    let mut app = App::new(vec![], None, tx);
    app.antigravity_dir = Some(temp.path().join("brain"));
    app.trace_db_path = Some(db);
    app.set_sessions_file(&sessions_path);
    app.restore_saved_sessions();

    let args: Vec<_> = app
        .sessions
        .iter()
        .map(|s| s.profile.args.clone())
        .collect();
    let resumed = |conv: &str| -> Vec<String> {
        vec![
            "--dangerously-skip-permissions".into(),
            "--conversation".into(),
            conv.into(),
        ]
    };
    assert_eq!(
        args,
        vec![resumed("conv-a"), resumed("conv-b"), resumed("conv-c")]
    );

    let saved = agent_mux::persistence::load_saved_sessions(&sessions_path);
    let convs: Vec<_> = saved.iter().map(|s| s.conversation.as_deref()).collect();
    assert_eq!(convs, vec![Some("conv-a"), Some("conv-b"), Some("conv-c")]);
    app.kill_all();
}
