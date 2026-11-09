//! The server shell and the trace API: the SPA and its assets, the
//! `/api/trace/{tool}` envelope, JSON errors, and the binary's `web` arm.

use super::support::{get, post, seeded_store, start};
use serde_json::json;

#[test]
fn the_spa_and_its_assets_are_served() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);

    let (status, ctype, body) = get(&base, "/");
    assert_eq!(status, 200);
    assert!(ctype.starts_with("text/html"), "{ctype}");
    assert!(body.contains("<div id=\"app\">"));
    for script in [
        "app.js",
        "trace.js",
        "usage.js",
        "tools.js",
        "swarm.js",
        "runs.js",
        "library.js",
    ] {
        assert!(body.contains(&format!("/static/{script}")), "{script}");
        let (status, ctype, text) = get(&base, &format!("/static/{script}"));
        assert_eq!(status, 200, "{script}");
        assert!(ctype.starts_with("text/javascript"), "{script}: {ctype}");
        assert!(!text.trim().is_empty(), "{script}");
    }
    // app.js owns the registry and the client, before the pages load
    let (_, _, app) = get(&base, "/static/app.js");
    for name in [
        "registerPage",
        "api",
        "fmtTokens",
        "fmtCost",
        "fmtDuration",
        "fmtTime",
        "table",
        "charts",
        "filterBar",
        "live",
        "subscribe",
        "EventSource",
    ] {
        assert!(app.contains(name), "app.js exposes {name}");
    }
    let (_, _, trace_js) = get(&base, "/static/trace.js");
    for page in [
        "'/sessions'",
        "'/sessions/:key'",
        "'/search'",
        "'/skills'",
        "'/health'",
    ] {
        assert!(trace_js.contains(page), "trace.js registers {page}");
    }
    let (status, ctype, _) = get(&base, "/static/app.css");
    assert_eq!(status, 200);
    assert!(ctype.starts_with("text/css"));

    let (status, _, body) = get(&base, "/static/nope.js");
    assert_eq!(status, 404);
    let err: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(err["ok"], false);
    assert!(err["error"].as_str().unwrap().contains("nope.js"));

    server.abort();
}

#[test]
fn the_trace_api_answers_the_service_envelope() {
    let temp = tempfile::tempdir().unwrap();
    let (db, ws) = seeded_store(temp.path());
    let (base, server) = start(&db);

    // list_sessions: the envelope the MCP server returns as structuredContent
    let (status, env) = post(&base, "/api/trace/list_sessions", &json!({}));
    assert_eq!(status, 200, "{env}");
    assert_eq!(env["schema_version"], 1);
    assert!(env["as_of"].is_string());
    assert!(env["coverage"]["status"].is_string());
    let sessions = env["data"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2, "{env}");
    assert_eq!(env["data"]["total_matching"], 2);
    let claude = sessions
        .iter()
        .find(|s| s["session_key"] == "claude:s1")
        .expect("the claude session");
    assert_eq!(claude["provider"], "claude");
    assert_eq!(claude["cwd"].as_str(), Some(ws.to_string_lossy().as_ref()));
    assert_eq!(claude["turns"], 2);
    assert_eq!(claude["tools"], 2);

    // a filter travels as the tool's arguments
    let (status, env) = post(
        &base,
        "/api/trace/list_sessions",
        &json!({"provider": "codex"}),
    );
    assert_eq!(status, 200);
    assert_eq!(env["data"]["sessions"].as_array().unwrap().len(), 1);

    // an empty body counts as no arguments
    let res = ureq::post(format!("{base}/api/trace/get_health"))
        .send_empty()
        .unwrap();
    assert_eq!(res.status().as_u16(), 200);

    // get_session and get_timeline
    let (status, env) = post(
        &base,
        "/api/trace/get_session",
        &json!({"session_key": "claude:s1"}),
    );
    assert_eq!(status, 200, "{env}");
    assert_eq!(env["data"]["session"]["session_key"], "claude:s1");
    assert_eq!(env["data"]["session"]["completed_turns"], 2);
    assert_eq!(env["data"]["session"]["total_tools"], 2);
    let (status, env) = post(
        &base,
        "/api/trace/get_timeline",
        &json!({"session_key": "claude:s1"}),
    );
    assert_eq!(status, 200, "{env}");
    let turns = env["data"]["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0]["observations"][0]["name"], "Edit");

    // search, skills, health, briefing, compare_runs reach the service
    let (status, env) = post(&base, "/api/trace/search_traces", &json!({"query": "auth"}));
    assert_eq!(status, 200, "{env}");
    assert_eq!(env["data"]["matches"].as_array().unwrap().len(), 1);
    assert_eq!(env["data"]["matches"][0]["session_key"], "codex:s2");
    let (status, env) = post(&base, "/api/trace/analyze_skills", &json!({}));
    assert_eq!(status, 200, "{env}");
    assert!(env["data"]["skills"].is_array());
    let (status, env) = post(&base, "/api/trace/get_health", &json!({}));
    assert_eq!(status, 200, "{env}");
    assert_eq!(env["data"]["db_available"], true);
    assert_eq!(
        env["data"]["db_path"].as_str(),
        Some(db.to_string_lossy().as_ref())
    );
    // the briefing rolls sessions up per workspace: under the all-workspaces
    // default it answers, with a count for a `--workspace` scope only
    let (status, env) = post(&base, "/api/trace/briefing", &json!({}));
    assert_eq!(status, 200, "{env}");
    assert!(env["data"]["total_sessions"].is_number(), "{env}");
    let (status, env) = post(
        &base,
        "/api/trace/compare_runs",
        &json!({"a": "claude:s1", "b": "codex:s2"}),
    );
    assert!(status == 200 || status == 404, "{status} {env}");

    server.abort();
}

#[test]
fn errors_are_json_with_a_status() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);

    let (status, err) = post(&base, "/api/trace/delegate", &json!({}));
    assert_eq!(status, 404);
    assert_eq!(err["ok"], false);
    assert!(err["error"].as_str().unwrap().contains("delegate"));

    let (status, err) = post(
        &base,
        "/api/trace/get_session",
        &json!({"session_key": "nope"}),
    );
    assert_eq!(status, 404, "{err}");
    assert_eq!(err["ok"], false);
    assert!(
        err["error"].as_str().unwrap().starts_with("[NOT_FOUND]"),
        "{err}"
    );

    let (status, err) = post(&base, "/api/trace/list_sessions", &json!({"bogus": 1}));
    assert_eq!(status, 400, "{err}");
    assert_eq!(err["ok"], false);
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .starts_with("[INVALID_ARGUMENT]")
    );

    let res = ureq::post(format!("{base}/api/trace/list_sessions"))
        .config()
        .http_status_as_error(false)
        .build()
        .header("content-type", "application/json")
        .send("{not json".as_bytes())
        .unwrap();
    assert_eq!(res.status().as_u16(), 400);
    let text = res.into_body().read_to_string().unwrap();
    assert!(text.contains("\"ok\":false"), "{text}");

    let (status, err) = post(&base, "/api/nothing/here", &json!({}));
    assert_eq!(status, 404);
    assert_eq!(err["ok"], false);

    server.abort();
}

#[test]
fn a_missing_store_keeps_health_and_fails_the_rest_with_503() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("absent.db");
    let (base, server) = start(&db);
    let (status, env) = post(&base, "/api/trace/get_health", &json!({}));
    assert_eq!(status, 200, "{env}");
    assert_eq!(env["data"]["db_available"], false);
    let (status, err) = post(&base, "/api/trace/list_sessions", &json!({}));
    assert_eq!(status, 503, "{err}");
    assert_eq!(err["ok"], false);
    assert!(!db.exists(), "the server never creates a store");
    server.abort();
}

#[test]
fn the_binary_prints_a_loopback_url_and_serves_it() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Kill(
        Command::new(env!("CARGO_BIN_EXE_agent-mux"))
            .args(["web", "--port", "0", "--db"])
            .arg(&db)
            .env("HOME", temp.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut lines = BufReader::new(child.0.stdout.take().unwrap()).lines();
    let url = lines.next().expect("a line on stdout").unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert!(url.ends_with('/'), "{url}");
    let base = url.trim_end_matches('/');
    let (status, _, body) = get(base, "/");
    assert_eq!(status, 200);
    assert!(body.contains("agent-mux"));
    let (status, env) = post(base, "/api/trace/list_sessions", &json!({}));
    assert_eq!(status, 200, "{env}");
    assert_eq!(env["data"]["total_matching"], 2);
}

/// Reads `/api/live` on a thread and hands over each event as (name, data).
fn live_events(base: &str) -> std::sync::mpsc::Receiver<(String, serde_json::Value)> {
    use std::io::{BufRead, BufReader};
    let (tx, rx) = std::sync::mpsc::channel();
    let res = ureq::get(format!("{base}/api/live"))
        .call()
        .expect("a live stream");
    let ctype = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ctype.starts_with("text/event-stream"), "{ctype}");
    let reader = BufReader::new(res.into_body().into_reader());
    std::thread::spawn(move || {
        let mut event = String::new();
        for line in reader.lines() {
            let Ok(line) = line else { return };
            if let Some(name) = line.strip_prefix("event:") {
                event = name.trim().to_string();
            } else if let Some(data) = line.strip_prefix("data:") {
                let data = serde_json::from_str(data.trim()).unwrap_or_default();
                if tx.send((std::mem::take(&mut event), data)).is_err() {
                    return;
                }
            }
        }
    });
    rx
}

#[test]
fn live_streams_a_change_when_the_feed_advances() {
    use agent_mux::tracing::pricing::PriceTable;
    use agent_mux::tracing::store::model::{SessionRow, StoreOp};
    use agent_mux::tracing::store::{OpenOptions, open_rw};
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let (db, ws) = seeded_store(temp.path());
    let (base, server) = start(&db);
    let events = live_events(&base);

    let (name, ready) = events.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(name, "ready");
    let first = ready["seq"].as_i64().unwrap();
    assert!(first > 0, "{ready}");

    // nothing moves: no change event
    assert!(events.recv_timeout(Duration::from_millis(1500)).is_err());

    let mut store = open_rw(
        &db,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run-live".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    store
        .apply(&[StoreOp::Session(SessionRow {
            key: "claude:s3".into(),
            provider: "claude".into(),
            session_id: "s3".into(),
            user_id: None,
            cwd: Some(ws.to_string_lossy().into_owned()),
            project_slug: None,
            transcript_path: None,
            title: Some("a new one".into()),
            seen_ns: agent_mux::tracing::store::now_ns(),
            extra: None,
        })])
        .unwrap();

    let (name, change) = events.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(name, "change");
    assert_eq!(change["previous"].as_i64(), Some(first));
    assert!(change["seq"].as_i64().unwrap() > first, "{change}");
    server.abort();
}

#[test]
fn live_answers_ready_without_a_store() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("absent.db");
    let (base, server) = start(&db);
    let events = live_events(&base);
    let (name, ready) = events
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    assert_eq!(name, "ready");
    assert_eq!(ready["seq"], 0);
    assert!(!db.exists(), "the stream never creates a store");
    server.abort();
}
