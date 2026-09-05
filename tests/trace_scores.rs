//! Scores: recorded in the store, cycled from the browser with `s`, and
//! posted to Langfuse as a `score-create` batch on the ingestion endpoint.

use agent_mux::app::{App, Mode, TraceBrowserState};
use agent_mux::config::ResolvedLangfuse;
use agent_mux::tracing::langfuse::basic_auth;
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::scores::{self, VERDICT};
use agent_mux::tracing::store::model::{LaunchRow, StoreOp, TraceRow, TraceStatus};
use agent_mux::tracing::store::writer::{WriterConfig, spawn_writer};
use agent_mux::tracing::store::{OpenOptions, open_aux, open_ro, open_rw};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn launch() -> LaunchRow {
    LaunchRow {
        id: "l1".into(),
        run_id: "run-s".into(),
        agent_mux_session: 1,
        profile: "Claude Code".into(),
        provider: "claude".into(),
        cwd: "/proj".into(),
        project_slug: "-proj".into(),
        content_mode: "full".into(),
        correlation_plan: "deterministic".into(),
        correlation: Some("deterministic".into()),
        session_key: Some("claude:s1".into()),
        injected_session_id: true,
        attached: false,
        started_ns: 1_000,
        ended_ns: None,
        termination: None,
        exit_code: None,
        parse_errors: None,
        dropped_ops: None,
        reported_cost_usd: None,
        reported_lines_added: None,
        reported_lines_removed: None,
        agent_mux_version: "test".into(),
        user_id: None,
        release: None,
        environment: None,
        tags: vec![],
        metadata: None,
    }
}

fn trace(id: &str, ordinal: i64) -> TraceRow {
    TraceRow {
        id: id.into(),
        session_key: "claude:s1".into(),
        provider: "claude".into(),
        session_id: "s1".into(),
        launch_id: Some("l1".into()),
        ordinal,
        name: format!("turn {ordinal}"),
        status: TraceStatus::Closed,
        start_ns: 1_000_000_000 * ordinal,
        end_ns: Some(1_000_000_000 * ordinal + 500),
        input: Some("go".into()),
        output: Some("ok".into()),
        thinking: None,
        skills: None,
        reported_duration_ms: None,
        reported_message_count: None,
        session_cost_usd: None,
        timing_approx: false,
        ordinal_salted: false,
        metadata: None,
    }
}

fn seeded_store(dir: &Path) -> PathBuf {
    let db = dir.join("traces.db");
    let store = open_rw(
        &db,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run-s".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    let handle = spawn_writer(store, WriterConfig::new(30), Box::new(|_, _| {}), None);
    handle.tx.try_send(StoreOp::Launch(launch())).unwrap();
    handle.tx.try_send(StoreOp::Trace(trace("t1", 1))).unwrap();
    handle.tx.try_send(StoreOp::Trace(trace("t2", 2))).unwrap();
    assert!(handle.finish(Duration::from_secs(5)));
    db
}

#[test]
fn scores_record_latest_wins_and_clear() {
    let temp = tempfile::tempdir().unwrap();
    let db = seeded_store(temp.path());
    let conn = open_aux(&db).unwrap();
    let first = scores::record(&conn, "trace", "t1", VERDICT, 1.0, Some("nice")).unwrap();
    assert!(first.id > 0);
    assert_eq!(first.comment.as_deref(), Some("nice"));
    std::thread::sleep(Duration::from_millis(2));
    scores::record(&conn, "trace", "t1", VERDICT, 0.0, None).unwrap();
    scores::record(&conn, "trace", "t2", "quality", 0.8, None).unwrap();
    let latest = scores::latest_trace_scores(&conn, VERDICT).unwrap();
    assert_eq!(latest.get("t1"), Some(&0.0), "the latest verdict wins");
    assert_eq!(latest.get("t2"), None, "other names are not verdicts");
    assert_eq!(scores::for_target(&conn, "trace", "t1").unwrap().len(), 2);
    assert_eq!(scores::clear(&conn, "trace", "t1", VERDICT).unwrap(), 2);
    assert!(
        scores::latest_trace_scores(&conn, VERDICT)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        scores::for_target(&conn, "trace", "t2").unwrap()[0].value,
        0.8
    );
}

/// Answers one request with `status` and `body`, handing back what it saw.
fn serve_once(
    status: &'static str,
    body_out: &'static str,
) -> (String, std::sync::mpsc::Receiver<(String, String, String)>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).unwrap();
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
            let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let header = |name: &str| -> Option<String> {
                head.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
                })
            };
            let len: usize = header("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if buf.len() < end + 4 + len {
                continue;
            }
            let body = String::from_utf8_lossy(&buf[end + 4..end + 4 + len]).into_owned();
            let path = head
                .lines()
                .next()
                .unwrap_or("")
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_string();
            tx.send((path, header("authorization").unwrap_or_default(), body))
                .unwrap();
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_out}",
                body_out.len()
            );
            stream.write_all(resp.as_bytes()).unwrap();
            return;
        }
    });
    (addr, rx)
}

#[test]
fn a_score_is_posted_to_langfuse_as_a_score_create_batch() {
    let (addr, rx) = serve_once(
        "207 Multi-Status",
        r#"{"successes":[{"id":"x","status":201}],"errors":[]}"#,
    );
    let lf = ResolvedLangfuse {
        host: addr,
        public_key: "pk-lf".into(),
        secret_key: "sk-lf".into(),
        flush_interval_ms: 100,
        secret_from_file: false,
        legacy_keys: false,
    };
    let score = scores::Score {
        id: 3,
        target: "trace".into(),
        target_id: "t1".into(),
        name: VERDICT.into(),
        value: 1.0,
        comment: None,
        created_ns: 1_756_548_000_000_000_000,
    };
    scores::export(&lf, &score, "t1").unwrap();
    let (path, auth, body) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(path, "/api/public/ingestion");
    assert_eq!(auth, basic_auth("pk-lf", "sk-lf"));
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["batch"][0]["type"], "score-create");
    assert_eq!(v["batch"][0]["body"]["traceId"], "t1");
    assert_eq!(v["batch"][0]["body"]["name"], "verdict");
    assert_eq!(v["batch"][0]["body"]["value"], 1.0);
    assert_eq!(v["batch"][0]["body"]["dataType"], "NUMERIC");

    // a per-item error inside a 207 is a failure the caller hears about
    let (addr, _rx) = serve_once(
        "207 Multi-Status",
        r#"{"successes":[],"errors":[{"id":"x","status":400,"message":"trace not found"}]}"#,
    );
    let lf = ResolvedLangfuse { host: addr, ..lf };
    let err = scores::export(&lf, &score, "t1").unwrap_err();
    assert!(err.contains("trace not found"), "{err}");
    // bad credentials
    let (addr, _rx) = serve_once("401 Unauthorized", "{}");
    let lf = ResolvedLangfuse { host: addr, ..lf };
    assert!(
        scores::export(&lf, &score, "t1")
            .unwrap_err()
            .contains("credentials")
    );
}

#[test]
fn s_in_the_browser_cycles_the_verdict() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::Instant;

    let temp = tempfile::tempdir().unwrap();
    let db = seeded_store(temp.path());
    let mut browser = TraceBrowserState::new(Some(&db), None);
    browser.all_projects = true;
    browser.reload_sessions();
    browser.load_turns();
    assert_eq!(browser.turns.len(), 2);
    // the newest turn is selected on load
    let selected = browser.turns[browser.selected_turn].id.clone();
    assert!(browser.scores.is_empty());

    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let mut app = App::new(vec![], None, tx);
    app.mode = Mode::TraceBrowser(Box::new(browser));
    app.handle_key(
        &KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        Instant::now(),
    );
    let notice = app.notice.clone().expect("a notice says what happened");
    assert!(notice.text.ends_with(": good"), "{}", notice.text);
    let Mode::TraceBrowser(b) = &mut app.mode else {
        panic!()
    };
    assert_eq!(b.scores.get(&selected), Some(&1.0));
    assert!(b.cycle_score().unwrap().ends_with(": bad"));
    assert_eq!(b.scores.get(&selected), Some(&0.0));
    assert!(b.cycle_score().unwrap().ends_with(": verdict cleared"));
    assert!(!b.scores.contains_key(&selected));
    // the rows are in the store, latest first cleared
    let ro = open_ro(&db).unwrap();
    assert!(
        scores::for_target(&ro, "trace", &selected)
            .unwrap()
            .is_empty()
    );
    // no store: a clear error, no panic
    let mut bare = TraceBrowserState::new(None, None);
    assert_eq!(bare.cycle_score().unwrap_err(), "no turn selected");
}
