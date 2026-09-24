//! The `trace hook` command against a real store: rows land with the
//! configured content policy, duplicates collapse, a locked store is given
//! up on within the budget, and agy gets its stdout contract. Then the
//! pipeline side: a hook-announced session is adopted and hook rows pin
//! its timings.

use agent_mux::config::{self, Profile};
use agent_mux::tracing::TraceRuntime;
use agent_mux::tracing::cli::{HOOK_BUSY_CAP, hook_run};
use agent_mux::tracing::hooks::{ContentPolicy, HookSource, parse};
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::{OpenOptions, insert_hook_event, open_hook_sink, open_ro, open_rw};
use agent_mux::transcript::parse_rfc3339_nanos;
use std::path::Path;
use std::time::{Duration, Instant};

fn store_at(dir: &Path) -> std::path::PathBuf {
    let db = dir.join("traces.db");
    let store = open_rw(
        &db,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run-hooks".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    let _ = store.end_run();
    db
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

const CLAUDE_POST: &str = r#"{"session_id":"sess-hook","prompt_id":"p1","transcript_path":"/tmp/t.jsonl","cwd":"/proj","hook_event_name":"PostToolUse","tool_name":"Bash","tool_use_id":"toolu_9","tool_input":{"command":"cargo test"},"tool_response":{"stdout":"ok"}}"#;

#[test]
fn hook_rows_land_with_policy_and_dedupe() {
    let dir = tempfile::tempdir().unwrap();
    let db = store_at(dir.path());
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let db_s = db.to_string_lossy().into_owned();
    let home_s = home.to_string_lossy().into_owned();
    // default policy (full, no profiles.toml under the fake home)
    let out = hook_run(
        &args(&[
            "claude", "--db", &db_s, "--home", &home_s, "--launch", "launch-x",
        ]),
        Some(CLAUDE_POST),
    );
    assert!(out.inserted, "{:?}", out.error);
    assert!(out.response.is_none());
    // duplicate delivery
    let again = hook_run(
        &args(&["claude", "--db", &db_s, "--home", &home_s]),
        Some(CLAUDE_POST),
    );
    assert!(!again.inserted);
    assert!(again.error.is_none());
    // metadata mode via the registration flag
    let mut pre = CLAUDE_POST.replace("PostToolUse", "PreToolUse");
    pre = pre.replace("toolu_9", "toolu_10");
    let meta = hook_run(
        &args(&[
            "claude",
            "--db",
            &db_s,
            "--home",
            &home_s,
            "--content-mode",
            "metadata",
        ]),
        Some(&pre),
    );
    assert!(meta.inserted, "{:?}", meta.error);
    let conn = open_ro(&db).unwrap();
    let rows: Vec<(String, Option<String>, String, String)> = conn
        .prepare("SELECT event, launch_id, tool_use_id, payload FROM hook_events ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "PostToolUse");
    assert_eq!(rows[0].1.as_deref(), Some("launch-x"));
    assert!(
        rows[0].3.contains("cargo test"),
        "full mode keeps the masked command: {}",
        rows[0].3
    );
    assert_eq!(rows[1].0, "PreToolUse");
    assert_eq!(rows[1].3, "{}", "metadata mode stores no bodies");
    // Codex notify passes the payload as the last argument
    let notify = r#"{"type":"agent-turn-complete","thread-id":"thread-1","turn-id":"turn-1","cwd":"/proj","last-assistant-message":"done"}"#;
    let out = hook_run(
        &args(&["codex-notify", "--db", &db_s, "--home", &home_s, notify]),
        None,
    );
    assert!(out.inserted, "{:?}", out.error);
    // agy answers the contract even when nothing is stored
    let out = hook_run(
        &args(&["agy", "--event", "Stop", "--db", &db_s, "--home", &home_s]),
        Some(r#"{"nope":true}"#),
    );
    assert!(!out.inserted);
    assert_eq!(out.response.as_deref(), Some(r#"{"decision":"proceed"}"#));
    let out = hook_run(
        &args(&[
            "agy",
            "--event",
            "PostToolUse",
            "--db",
            &db_s,
            "--home",
            &home_s,
        ]),
        Some(r#"{"conversationId":"conv-1","workspacePaths":["/proj"],"stepIdx":4}"#),
    );
    assert!(out.inserted, "{:?}", out.error);
    assert_eq!(out.response.as_deref(), Some("{}"));
}

#[test]
fn hook_gives_up_within_budget_when_the_store_is_locked() {
    let dir = tempfile::tempdir().unwrap();
    let db = store_at(dir.path());
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let blocker = rusqlite::Connection::open(&db).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let started = Instant::now();
    let out = hook_run(
        &args(&[
            "claude",
            "--db",
            &db.to_string_lossy(),
            "--home",
            &home.to_string_lossy(),
        ]),
        Some(CLAUDE_POST),
    );
    let elapsed = started.elapsed();
    assert!(!out.inserted);
    assert!(
        out.error.as_deref().unwrap_or("").contains("insert"),
        "{:?}",
        out.error
    );
    assert!(
        elapsed < HOOK_BUSY_CAP + Duration::from_millis(1500),
        "hook overstayed the lock budget: {elapsed:?}"
    );
    blocker.execute_batch("ROLLBACK;").unwrap();
}

#[test]
fn missing_store_and_bad_payload_are_silent() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_string_lossy().into_owned();
    let out = hook_run(
        &args(&[
            "claude",
            "--db",
            &dir.path().join("none.db").to_string_lossy(),
            "--home",
            &home,
        ]),
        Some(CLAUDE_POST),
    );
    assert!(!out.inserted);
    assert!(
        out.error
            .as_deref()
            .unwrap_or("")
            .contains("no trace store")
    );
    let out = hook_run(&args(&["claude", "--home", &home]), Some("not json"));
    assert!(out.error.as_deref().unwrap_or("").starts_with("payload"));
    let out = hook_run(&args(&["nope"]), Some("{}"));
    assert!(
        out.error
            .as_deref()
            .unwrap_or("")
            .contains("unknown hook source")
    );
}

fn ns(ts: &str) -> i64 {
    parse_rfc3339_nanos(ts).unwrap() as i64
}

/// A launch that could not be correlated (`--continue`: no id, no flag)
/// is adopted through a hook announcement, and hook rows pin its tool
/// timings — the whole Task 2 path end to end, without a real CLI.
#[tokio::test]
async fn announced_session_is_adopted_and_hook_pins_apply() {
    let temp = tempfile::tempdir().unwrap();
    let claude_dir = temp.path().join("claude-home");
    let workdir = temp.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    let db_path = temp.path().join("traces.db");
    let toml = format!(
        r#"
        [tracing]
        db_path = "{db}"
        poll_interval_ms = 40
        flush_interval_ms = 30
        claude_dir = "{claude}"

        [[profiles]]
        name = "Claude Code"
        command = "claude"
        args = ["--continue"]
        "#,
        db = db_path.display(),
        claude = claude_dir.display(),
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    let profile: Profile = cfg.profiles[0].clone();
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let mut rt = TraceRuntime::new(resolved, tx).unwrap();
    let plan = rt.plan_launch(&profile, &workdir).unwrap();
    assert_eq!(
        plan.extra_args[0], "--settings",
        "--continue: no id to inject, only the hook registration"
    );
    let launch_id = plan.launch_id.clone();
    let handle = rt.start_session(1, plan);

    // the CLI writes its transcript…
    let session_id = "cont-1111-2222";
    let projects_dir = claude_dir
        .join("projects")
        .join(agent_mux::history::project_slug(&workdir));
    std::fs::create_dir_all(&projects_dir).unwrap();
    let transcript = projects_dir.join(format!("{session_id}.jsonl"));
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","timestamp":"2026-09-01T10:00:00Z","message":{"role":"user","content":[{"type":"text","text":"run the tests"}]}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-09-01T10:00:04Z","message":{"role":"assistant","model":"claude-haiku-4-5","usage":{"input_tokens":4,"output_tokens":2},"content":[{"type":"text","text":"running"},{"type":"tool_use","id":"toolu_h1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-09-01T10:00:05Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_h1","content":"ok","is_error":false}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    // …and its hooks announce the session and time the tool call
    let sink = open_hook_sink(&db_path, Duration::from_millis(500)).unwrap();
    let policy = ContentPolicy::full();
    let mk = |json: serde_json::Value, ts: i64| {
        parse(
            HookSource::Claude,
            None,
            &json,
            &policy,
            ts,
            Some(launch_id.clone()),
        )
        .unwrap()
    };
    let common = |event: &str| {
        serde_json::json!({
            "session_id": session_id, "hook_event_name": event, "cwd": workdir.to_string_lossy(),
            "transcript_path": transcript.to_string_lossy(), "prompt_id": "p1"
        })
    };
    let pre = ns("2026-09-01T10:00:03.600Z");
    let post = ns("2026-09-01T10:00:05.900Z");
    let mut start = common("SessionStart");
    start["source"] = serde_json::Value::from("startup");
    insert_hook_event(&sink, &mk(start, ns("2026-09-01T09:59:59Z"))).unwrap();
    let mut pre_v = common("PreToolUse");
    pre_v["tool_name"] = serde_json::Value::from("Bash");
    pre_v["tool_use_id"] = serde_json::Value::from("toolu_h1");
    insert_hook_event(&sink, &mk(pre_v, pre)).unwrap();
    let mut post_v = common("PostToolUse");
    post_v["tool_name"] = serde_json::Value::from("Bash");
    post_v["tool_use_id"] = serde_json::Value::from("toolu_h1");
    insert_hook_event(&sink, &mk(post_v, post)).unwrap();
    let mut stop = common("Stop");
    stop["turn_number"] = serde_json::Value::from(1);
    insert_hook_event(&sink, &mk(stop, ns("2026-09-01T10:00:06.400Z"))).unwrap();
    drop(sink);

    tokio::time::sleep(Duration::from_millis(700)).await;
    handle.mark_exited(Some(0));
    tokio::time::sleep(Duration::from_millis(400)).await;
    rt.shutdown(Duration::from_secs(3)).await;

    let conn = open_ro(&db_path).unwrap();
    let (correlation, plan_label, meta): (String, String, String) = conn
        .query_row(
            "SELECT correlation, correlation_plan, metadata FROM launches",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(correlation, "announced");
    assert_eq!(
        plan_label, "announced+none",
        "the plan could only wait for an announcement; the hook supplied the session"
    );
    assert!(
        meta.contains("\"session_start_source\":\"startup\""),
        "{meta}"
    );
    assert!(meta.contains("\"hook_events\""), "{meta}");
    let (status, start_ns, end_ns): (String, i64, i64) = conn
        .query_row(
            "SELECT status, start_ns, end_ns FROM traces WHERE ordinal = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "closed");
    assert_eq!(start_ns, ns("2026-09-01T10:00:00Z"));
    assert_eq!(
        end_ns,
        ns("2026-09-01T10:00:06.400Z"),
        "Stop hook pinned the turn end"
    );
    let (tool_start, tool_end, tool_meta): (i64, i64, String) = conn
        .query_row(
            "SELECT start_ns, end_ns, metadata FROM observations WHERE tool_id = 'toolu_h1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(tool_start, pre, "PreToolUse pinned the start");
    assert_eq!(tool_end, post, "PostToolUse pinned the end");
    assert!(tool_meta.contains("\"hook_timed\":true"), "{tool_meta}");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM hook_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 4);
}

/// Task 3 end to end: a fake `claude` receives the per-launch `--settings`
/// registration and, like the real CLI, runs the registered command
/// (`agent-mux trace hook claude --home …`) around its transcript lines.
/// The rows must reach the store through the real binary and pin the
/// launch, the turn, and the tool call.
#[cfg(unix)]
#[tokio::test]
async fn registered_hooks_flow_through_a_fake_cli_into_the_store() {
    use agent_mux::app::{App, Mode};
    use agent_mux::status::Status;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let claude_dir = temp.path().join("claude-home");
    let workdir = temp.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    std::fs::create_dir_all(home.join(".agent-mux")).unwrap();
    let db_path = temp.path().join("store").join("traces.db");
    let projects_dir = claude_dir
        .join("projects")
        .join(agent_mux::history::project_slug(&workdir));
    std::fs::create_dir_all(&projects_dir).unwrap();
    // the hook command resolves the store from `--home`
    std::fs::write(
        home.join(".agent-mux").join("profiles.toml"),
        format!("[tracing]\ndb_path = \"{}\"\n", db_path.display()),
    )
    .unwrap();

    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script_path = bin_dir.join("claude");
    let script = format!(
        r#"#!/bin/sh
sid=""; settings=""; prev=""
for a in "$@"; do
  [ "$prev" = "--session-id" ] && sid="$a"
  [ "$prev" = "--settings" ] && settings="$a"
  prev="$a"
done
[ -n "$sid" ] || exit 3
case "$settings" in *'"hooks"'*'"PostToolUse"'*) ;; *) exit 4 ;; esac
[ -n "$AGENT_MUX_SESSION_ID" ] || exit 5
out="{proj}/$sid.jsonl"
now() {{ date -u +%Y-%m-%dT%H:%M:%S.000Z; }}
hook() {{ printf '%s' "$1" | "{exe}" trace hook claude --home "{home}" >/dev/null 2>&1 || exit 6; }}
common="\"session_id\":\"$sid\",\"transcript_path\":\"$out\",\"cwd\":\"{cwd}\",\"prompt_id\":\"p1\""
hook "{{$common,\"hook_event_name\":\"SessionStart\",\"source\":\"startup\"}}"
hook "{{$common,\"hook_event_name\":\"UserPromptSubmit\",\"prompt\":\"run the tests\"}}"
printf '%s\n' "{{\"type\":\"user\",\"timestamp\":\"$(now)\",\"cwd\":\"{cwd}\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"run the tests\"}}]}}}}" >> "$out"
sleep 0.2
printf '%s\n' "{{\"type\":\"assistant\",\"timestamp\":\"$(now)\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-haiku-4-5\",\"usage\":{{\"input_tokens\":42,\"output_tokens\":7}},\"content\":[{{\"type\":\"text\",\"text\":\"running\"}},{{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{{\"command\":\"cargo test\"}}}}]}}}}" >> "$out"
hook "{{$common,\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"Bash\",\"tool_use_id\":\"t1\",\"tool_input\":{{\"command\":\"cargo test\"}}}}"
sleep 0.2
printf '%s\n' "{{\"type\":\"user\",\"timestamp\":\"$(now)\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"ok\",\"is_error\":false}}]}}}}" >> "$out"
hook "{{$common,\"hook_event_name\":\"PostToolUse\",\"tool_name\":\"Bash\",\"tool_use_id\":\"t1\",\"tool_input\":{{\"command\":\"cargo test\"}},\"tool_response\":{{\"stdout\":\"ok\"}}}}"
hook "{{$common,\"hook_event_name\":\"Stop\",\"turn_number\":1,\"last_assistant_message\":\"running\"}}"
hook "{{$common,\"hook_event_name\":\"SessionEnd\",\"reason\":\"prompt_input_exit\"}}"
sleep 0.6
exit 0
"#,
        proj = projects_dir.display(),
        cwd = workdir.display(),
        exe = env!("CARGO_BIN_EXE_agent-mux"),
        home = home.display(),
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
    let home_s = home.to_string_lossy().into_owned();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|k| {
        (k == "HOME").then(|| home_s.clone())
    })
    .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let mut app = App::new(cfg.profiles, Some(runtime), tx);
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

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut exited = false;
    while Instant::now() < deadline {
        if matches!(app.sessions[0].status(Instant::now()), Status::Exited(_)) {
            exited = true;
            break;
        }
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(agent_mux::events::AppEvent::PtyOutput { id, bytes })) => {
                app.handle_pty_output(id, &bytes, Instant::now())
            }
            Ok(Some(agent_mux::events::AppEvent::PtyExit { id })) => app.handle_pty_exit(id),
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    assert!(exited, "fake claude never exited");
    assert_eq!(
        app.sessions[0].status(Instant::now()),
        Status::Exited(Some(0)),
        "the fake CLI rejected its launch flags"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    app.kill_all();
    let rt = app.take_tracing().unwrap();
    rt.shutdown(Duration::from_secs(5)).await;

    let conn = open_ro(&db_path).unwrap();
    let hook_rows: i64 = conn
        .query_row(
            "SELECT count(*) FROM hook_events WHERE provider = 'claude'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hook_rows, 6, "every registered event reached the store");
    let launch_ids: Vec<Option<String>> = {
        let mut st = conn
            .prepare("SELECT DISTINCT launch_id FROM hook_events")
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(launch_ids.len(), 1);
    assert!(
        launch_ids[0].is_some(),
        "AGENT_MUX_SESSION_ID tied the rows to the launch"
    );
    let (correlation, plan_label, meta, exit_code): (String, String, String, i64) = conn
        .query_row(
            "SELECT correlation, correlation_plan, metadata, exit_code FROM launches",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(exit_code, 0);
    assert_eq!(plan_label, "announced+deterministic");
    assert!(
        correlation == "announced" || correlation == "deterministic",
        "{correlation}"
    );
    assert!(meta.contains("\"hooks\":true"), "{meta}");
    assert!(
        meta.contains("\"session_start_source\":\"startup\""),
        "{meta}"
    );
    assert!(
        meta.contains("\"session_end_reason\":\"prompt_input_exit\""),
        "{meta}"
    );
    let (status, tk): (String, Option<String>) = conn
        .query_row(
            "SELECT status, json_extract(metadata, '$.turn_key') FROM traces WHERE ordinal = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "closed");
    assert_eq!(
        tk.as_deref(),
        Some("p1"),
        "UserPromptSubmit pinned the prompt id"
    );
    let (tool_meta, tool_end): (String, Option<i64>) = conn
        .query_row(
            "SELECT metadata, end_ns FROM observations WHERE name = 'Bash'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(tool_meta.contains("\"hook_timed\":true"), "{tool_meta}");
    assert!(tool_end.is_some());
}
