//! The Langfuse sink against a scripted in-process HTTP server: batching,
//! auth, OTLP partial success, retries, breaker, rate limiting, queue drops,
//! the bounded final drain, and the doctor probe.

use agent_mux::tracing::langfuse::map::MapCtx;
use agent_mux::tracing::langfuse::{ExporterConfig, probe_endpoint, spawn_exporter};
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::model::{
    Level, ObservationRow, ObservationType, StoreOp, TraceRow, TraceStatus,
};
use agent_mux::tracing::store::query::LaunchStats;
use agent_mux::tracing::usage::NormalizedUsage;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
struct Recorded {
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug)]
enum Scripted {
    /// 200 with an OTLP `ExportTraceServiceResponse` body.
    Ok(String),
    Status(u16),
    /// 429 with a Retry-After (seconds) header.
    RateLimited(u64),
    /// Sleep before answering 200.
    Hang(Duration),
}

struct Stub {
    addr: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    script: Arc<Mutex<VecDeque<Scripted>>>,
}

impl Stub {
    fn script(&self, steps: &[Scripted]) {
        self.script.lock().unwrap().extend(steps.iter().cloned());
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    fn wait_for_requests(&self, n: usize, timeout: Duration) -> Vec<Recorded> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.requests.lock().unwrap().len() >= n {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.requests()
    }
}

const OK_BODY: &str = "{}";

/// Minimal scripted HTTP/1.1 server on a std TcpListener. Sequential
/// keep-alive requests per connection; unscripted requests get 200.
fn stub_server() -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let requests: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
    let script: Arc<Mutex<VecDeque<Scripted>>> = Arc::new(Mutex::new(VecDeque::new()));
    let requests_thread = Arc::clone(&requests);
    let script_thread = Arc::clone(&script);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let requests = Arc::clone(&requests_thread);
            let script = Arc::clone(&script_thread);
            std::thread::spawn(move || {
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    let request = loop {
                        if let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let header_text =
                                String::from_utf8_lossy(&buf[..header_end]).into_owned();
                            let content_length = header_text
                                .lines()
                                .find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().ok())?
                                })
                                .unwrap_or(0);
                            let body_start = header_end + 4;
                            if buf.len() >= body_start + content_length {
                                let body = String::from_utf8_lossy(
                                    &buf[body_start..body_start + content_length],
                                )
                                .into_owned();
                                buf.drain(..body_start + content_length);
                                break Some((header_text, body));
                            }
                        }
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => break None,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let Some((header_text, body)) = request else {
                        return;
                    };
                    let mut lines = header_text.lines();
                    let request_line = lines.next().unwrap_or_default();
                    let path = request_line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("")
                        .to_string();
                    let headers = lines
                        .filter_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            Some((k.trim().to_string(), v.trim().to_string()))
                        })
                        .collect();
                    requests.lock().unwrap().push(Recorded {
                        path,
                        headers,
                        body,
                    });
                    let action = script
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or_else(|| Scripted::Ok(OK_BODY.into()));
                    let response = match action {
                        Scripted::Ok(body) => format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        ),
                        Scripted::Status(code) => {
                            format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\n\r\n")
                        }
                        Scripted::RateLimited(seconds) => format!(
                            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: {seconds}\r\nContent-Length: 0\r\n\r\n"
                        ),
                        Scripted::Hang(pause) => {
                            std::thread::sleep(pause);
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{OK_BODY}",
                                OK_BODY.len()
                            )
                        }
                    };
                    if stream.write_all(response.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    Stub {
        addr,
        requests,
        script,
    }
}

/// Production policy with test-sized timings.
fn cfg(addr: &str) -> ExporterConfig {
    let mut c = ExporterConfig::with_endpoint(addr, "pk", "sk", 20);
    c.attempts = 2;
    c.backoff_base = Duration::from_millis(5);
    c.retry_after_cap = Duration::from_secs(1);
    c.breaker_threshold = 2;
    c.breaker_open = Duration::from_millis(300);
    c.connect_timeout = Duration::from_secs(1);
    c.request_timeout = Duration::from_secs(3);
    c
}

fn ctx() -> MapCtx {
    MapCtx {
        prices: PriceTable::builtin(),
        version: "test".into(),
        user_id: Some("me".into()),
        release: None,
        environment: Some("test".into()),
        tags: vec!["agent-mux".into()],
    }
}

type Notes = Arc<Mutex<Vec<(&'static str, String)>>>;
type Stats = Arc<Mutex<Vec<(String, LaunchStats)>>>;

fn sinks() -> (
    Notes,
    Stats,
    agent_mux::tracing::langfuse::StatusSink,
    agent_mux::tracing::langfuse::StatsSink,
) {
    let notes: Notes = Arc::new(Mutex::new(Vec::new()));
    let stats: Stats = Arc::new(Mutex::new(Vec::new()));
    let n = Arc::clone(&notes);
    let s = Arc::clone(&stats);
    (
        notes,
        stats,
        Box::new(move |class, msg| n.lock().unwrap().push((class, msg))),
        Box::new(move |id, st| s.lock().unwrap().push((id.to_string(), st))),
    )
}

fn trace(n: u8, status: TraceStatus) -> StoreOp {
    StoreOp::Trace(TraceRow {
        id: format!("{:032x}", n),
        session_key: "claude:sess".into(),
        provider: "claude".into(),
        session_id: "sess".into(),
        launch_id: Some("L1".into()),
        ordinal: i64::from(n),
        name: format!("turn {n}"),
        status,
        start_ns: 1_756_980_000_000_000_000 + i64::from(n) * 1_000_000_000,
        end_ns: (status != TraceStatus::Open).then_some(1_756_980_005_000_000_000),
        input: Some("q".into()),
        output: None,
        thinking: None,
        skills: None,
        reported_duration_ms: None,
        reported_message_count: None,
        session_cost_usd: None,
        timing_approx: false,
        ordinal_salted: false,
        metadata: None,
    })
}

fn generation(n: u8, trace_n: u8, tokens: i64) -> StoreOp {
    StoreOp::Observation(ObservationRow {
        id: format!("{:016x}", n),
        trace_id: format!("{:032x}", trace_n),
        parent_id: None,
        obs_type: ObservationType::Generation,
        name: "assistant".into(),
        kind: None,
        start_ns: 1_756_980_001_000_000_000,
        end_ns: Some(1_756_980_002_000_000_000),
        level: Level::Default,
        status_message: None,
        model: Some("claude-haiku-4-5".into()),
        input: None,
        output: Some("a".into()),
        thinking: None,
        usage_raw: None,
        usage: Some(NormalizedUsage {
            input: Some(tokens - 5),
            output: Some(5),
            cache_read: None,
            cache_write: None,
            cache_write_1h: None,
            reasoning: None,
            total: Some(tokens),
        }),
        tool_id: None,
        tool_name: None,
        skill: None,
        mcp_server: None,
        path: None,
        is_error: false,
        ts_approx: false,
        metadata: serde_json::Map::new(),
    })
}

/// The spans of one OTLP export.
fn spans(r: &Recorded) -> Vec<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    v["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// A span's attribute value, from the OTLP KeyValue list.
fn attr<'a>(span: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    span["attributes"]
        .as_array()?
        .iter()
        .find(|a| a["key"] == key)?["value"]["stringValue"]
        .as_str()
}

/// Langfuse observation types, in order.
fn batch_types(r: &Recorded) -> Vec<String> {
    spans(r)
        .iter()
        .map(|s| {
            attr(s, "langfuse.observation.type")
                .unwrap_or("?")
                .to_string()
        })
        .collect()
}

#[test]
fn batches_carry_auth_and_rejections_and_stats_are_reported() {
    let stub = stub_server();
    stub.script(&[Scripted::Ok(
        r#"{"partialSuccess":{"rejectedSpans":"1","errorMessage":"one bad span"}}"#.into(),
    )]);
    let (notes, stats, status, stats_sink) = sinks();
    let handle = spawn_exporter(cfg(&stub.addr), ctx(), status, stats_sink);
    handle.tx.try_send(trace(1, TraceStatus::Closed)).unwrap();
    handle.tx.try_send(generation(1, 1, 120)).unwrap();
    handle.tx.try_send(generation(2, 1, 30)).unwrap();
    // launch and session rows have no Langfuse shape and are skipped
    handle
        .tx
        .try_send(StoreOp::Session(
            agent_mux::tracing::store::model::SessionRow {
                key: "claude:sess".into(),
                provider: "claude".into(),
                session_id: "sess".into(),
                user_id: None,
                cwd: None,
                project_slug: None,
                transcript_path: None,
                title: None,
                seen_ns: 0,
                extra: None,
            },
        ))
        .unwrap();
    let requests = stub.wait_for_requests(1, Duration::from_secs(5));
    assert_eq!(requests.len(), 1, "one batch");
    let r = &requests[0];
    assert_eq!(r.path, "/api/public/otel/v1/traces");
    assert_eq!(r.header("authorization"), Some("Basic cGs6c2s="));
    assert_eq!(r.header("content-type"), Some("application/json"));
    assert_eq!(r.header("x-langfuse-sdk-name"), Some("agent-mux"));
    assert_eq!(batch_types(r), vec!["span", "generation", "generation"]);
    let exported = spans(r);
    assert_eq!(attr(&exported[0], "session.id"), Some("sess"));
    assert_eq!(attr(&exported[0], "langfuse.trace.name"), Some("turn 1"));
    let usage: serde_json::Value =
        serde_json::from_str(attr(&exported[1], "langfuse.observation.usage_details").unwrap())
            .unwrap();
    assert_eq!(usage["total"], 120);
    // the generations hang under the turn's own root span
    assert_eq!(exported[1]["parentSpanId"], exported[0]["spanId"]);
    assert_eq!(exported[1]["traceId"], exported[0]["traceId"]);
    let dropped = handle.dropped.load(Ordering::Relaxed);
    assert!(handle.finish(Duration::from_secs(2)));
    let notes = notes.lock().unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].0, "rejected");
    assert!(
        notes[0].1.contains("1 span(s) rejected (one bad span)"),
        "{:?}",
        notes[0].1
    );
    assert_eq!(dropped, 3, "a partially rejected export counts its batch");
    let stats = stats.lock().unwrap();
    let (id, last) = stats.last().expect("stats after the flush");
    assert_eq!(id, "L1");
    assert_eq!(last.turns, 1);
    assert_eq!(last.total_tokens, Some(150));
    assert!(last.cost_usd.unwrap() > 0.0);
    assert!(last.running_tool.is_none());
}

#[test]
fn two_auth_failures_disable_export_for_the_run() {
    let stub = stub_server();
    stub.script(&[Scripted::Status(401), Scripted::Status(403)]);
    let (notes, _stats, status, stats_sink) = sinks();
    let handle = spawn_exporter(cfg(&stub.addr), ctx(), status, stats_sink);
    for n in 1..=3u8 {
        handle.tx.try_send(trace(n, TraceStatus::Open)).unwrap();
        std::thread::sleep(Duration::from_millis(80)); // past the flush interval: one batch each
    }
    std::thread::sleep(Duration::from_millis(100));
    let dropped = handle.dropped.load(Ordering::Relaxed);
    assert!(handle.finish(Duration::from_secs(2)));
    let requests = stub.requests();
    assert_eq!(requests.len(), 2, "the third batch never left the process");
    assert_eq!(dropped, 3);
    let notes = notes.lock().unwrap();
    assert!(notes.iter().any(|(c, _)| *c == "auth"), "{notes:?}");
    assert_eq!(notes.iter().filter(|(c, _)| *c == "dropped").count(), 1);
}

#[test]
fn server_errors_retry_then_open_the_breaker() {
    let stub = stub_server();
    stub.script(&[
        Scripted::Status(500),
        Scripted::Status(503),
        Scripted::Status(500),
        Scripted::Status(502),
    ]);
    let (notes, _stats, status, stats_sink) = sinks();
    let handle = spawn_exporter(cfg(&stub.addr), ctx(), status, stats_sink);
    for n in 1..=3u8 {
        handle.tx.try_send(trace(n, TraceStatus::Open)).unwrap();
        std::thread::sleep(Duration::from_millis(120));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(handle.finish(Duration::from_secs(2)));
    let requests = stub.requests();
    assert_eq!(
        requests.len(),
        4,
        "two attempts per batch for two batches, then the breaker swallowed the third"
    );
    let notes = notes.lock().unwrap();
    assert!(notes.iter().any(|(c, _)| *c == "breaker"), "{notes:?}");
}

#[test]
fn rate_limiting_honors_retry_after_without_counting_as_failure() {
    let stub = stub_server();
    stub.script(&[Scripted::RateLimited(1)]);
    let (notes, _stats, status, stats_sink) = sinks();
    let handle = spawn_exporter(cfg(&stub.addr), ctx(), status, stats_sink);
    let t0 = Instant::now();
    handle.tx.try_send(trace(1, TraceStatus::Closed)).unwrap();
    let requests = stub.wait_for_requests(2, Duration::from_secs(5));
    assert_eq!(requests.len(), 2);
    assert!(
        t0.elapsed() >= Duration::from_millis(900),
        "waited Retry-After"
    );
    assert!(handle.finish(Duration::from_secs(2)));
    let notes = notes.lock().unwrap();
    assert!(notes.is_empty(), "{notes:?}");
}

#[test]
fn a_full_queue_drops_and_counts_instead_of_blocking() {
    let stub = stub_server();
    stub.script(&[Scripted::Hang(Duration::from_millis(700))]);
    let (_notes, _stats, status, stats_sink) = sinks();
    let mut c = cfg(&stub.addr);
    c.max_batch_events = 1;
    let handle = spawn_exporter(c, ctx(), status, stats_sink);
    handle.tx.try_send(trace(1, TraceStatus::Open)).unwrap();
    std::thread::sleep(Duration::from_millis(60)); // the thread is now stuck in the hanging POST
    let mut refused = 0u32;
    for n in 0..(agent_mux::tracing::langfuse::QUEUE_CAPACITY + 50) {
        if handle
            .tx
            .try_send(trace((n % 200) as u8, TraceStatus::Open))
            .is_err()
        {
            refused += 1;
        }
    }
    assert!(
        refused >= 50,
        "try_send refused once the queue was full: {refused}"
    );
    let t0 = Instant::now();
    let _ = handle.finish(Duration::from_millis(300));
    assert!(t0.elapsed() < Duration::from_secs(3), "finish is bounded");
}

#[test]
fn final_drain_is_bounded_by_the_deadline() {
    let stub = stub_server();
    stub.script(&[Scripted::Hang(Duration::from_secs(2))]);
    let (_notes, _stats, status, stats_sink) = sinks();
    let mut c = cfg(&stub.addr);
    c.flush_interval = Duration::from_secs(5); // nothing flushes before the drain
    let handle = spawn_exporter(c, ctx(), status, stats_sink);
    handle.tx.try_send(trace(1, TraceStatus::Closed)).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    let t0 = Instant::now();
    let finished = handle.finish(Duration::from_millis(300));
    assert!(!finished, "the hanging server outlived the deadline");
    assert!(
        t0.elapsed() < Duration::from_millis(900),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(
        stub.wait_for_requests(1, Duration::from_secs(1)).len(),
        1,
        "the drain did try"
    );
}

#[test]
fn probe_reports_each_outcome() {
    let stub = stub_server();
    assert_eq!(probe_endpoint(&cfg(&stub.addr)), Ok(()));
    stub.script(&[Scripted::Status(401)]);
    let err = probe_endpoint(&cfg(&stub.addr)).unwrap_err();
    assert!(err.contains("authentication"), "{err}");
    stub.script(&[Scripted::Status(500)]);
    let err = probe_endpoint(&cfg(&stub.addr)).unwrap_err();
    assert!(err.contains("reach"), "{err}");
    stub.script(&[Scripted::Status(404)]);
    let err = probe_endpoint(&cfg(&stub.addr)).unwrap_err();
    assert!(err.contains("404"), "{err}");
    let requests = stub.requests();
    assert_eq!(requests[0].body, r#"{"resourceSpans":[]}"#);
    assert_eq!(requests[0].path, "/api/public/otel/v1/traces");
    assert!(probe_endpoint(&cfg("http://127.0.0.1:9")).is_err());
}

/// End to end: a fake `claude` launched twice through the app, once with
/// backend `both` and once with `langfuse`. The first lands rows in
/// SQLite *and* events on the server with matching ids; the second leaves
/// no trace rows locally but a launch row that says where they went. A
/// replay of the local session then reaches the server with the same ids.
#[cfg(unix)]
#[tokio::test]
async fn launches_route_rows_by_backend_and_replay_reaches_langfuse() {
    use agent_mux::app::{App, Mode};
    use agent_mux::config;
    use agent_mux::tracing::TraceRuntime;
    use agent_mux::tracing::langfuse::replay_sessions;
    use agent_mux::tracing::store::open_ro;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::os::unix::fs::PermissionsExt;

    let stub = stub_server();
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
sid=""; prev=""
for a in "$@"; do
  [ "$prev" = "--session-id" ] && sid="$a"
  prev="$a"
done
[ -n "$sid" ] || exit 3
out="{proj}/$sid.jsonl"
printf '%s\n' '{{"type":"user","timestamp":"2026-09-04T10:00:00Z","cwd":"{cwd}","message":{{"role":"user","content":[{{"type":"text","text":"run the tests"}}]}}}}' >> "$out"
sleep 0.2
printf '%s\n' '{{"type":"assistant","timestamp":"2026-09-04T10:00:01Z","message":{{"role":"assistant","model":"claude-haiku-4-5","usage":{{"input_tokens":42,"output_tokens":7}},"content":[{{"type":"text","text":"running"}},{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":"cargo test"}}}}]}}}}' >> "$out"
printf '%s\n' '{{"type":"user","timestamp":"2026-09-04T10:00:02Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"ok","is_error":false}}]}}}}' >> "$out"
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
        poll_interval_ms = 50
        flush_interval_ms = 30
        claude_dir = "{claude}"
        hooks = "off"
        backend = "both"

        [tracing.langfuse]
        host = "{host}"
        public_key = "pk"
        secret_key = "sk"
        flush_interval_ms = 40

        [[profiles]]
        name = "Claude Both"
        command = "{cmd}"
        args = []
        default_dir = "{dir}"

        [[profiles]]
        name = "Claude Langfuse"
        command = "{cmd}"
        args = []
        default_dir = "{dir}"
        [profiles.tracing]
        backend = "langfuse"
        "#,
        db = db_path.display(),
        claude = claude_dir.display(),
        host = stub.addr,
        cmd = script_path.display(),
        dir = workdir.display(),
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    assert!(resolved.langfuse.is_some());
    let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    assert!(runtime.langfuse_configured());
    let mut app = App::new(cfg.profiles, Some(runtime), tx);
    app.clipboard_enabled = false;
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

    // launch 1: profile "Claude Both" (dialog default)
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    let Mode::NewSession(dialog) = &app.mode else {
        panic!("dialog");
    };
    assert!(
        dialog.langfuse_available,
        "credentials resolved: the field cycles"
    );
    assert_eq!(dialog.backend, config::Backend::Both, "the global default");
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(
        matches!(app.mode, Mode::Control),
        "spawn failed: {:?}",
        app.notice
    );
    assert!(
        pump_until_exit(&mut app, &mut rx, 0).await,
        "first fake claude never exited"
    );
    // launch 2: profile "Claude Langfuse"
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    app.handle_key(&key(KeyCode::Down), Instant::now());
    let Mode::NewSession(dialog) = &app.mode else {
        panic!("dialog");
    };
    assert_eq!(dialog.profile_idx, 1);
    assert_eq!(
        dialog.backend,
        config::Backend::Langfuse,
        "the profile's own default"
    );
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(
        matches!(app.mode, Mode::Control),
        "spawn failed: {:?}",
        app.notice
    );
    assert!(
        pump_until_exit(&mut app, &mut rx, 1).await,
        "second fake claude never exited"
    );
    assert_eq!(
        app.sessions[0].trace.as_ref().unwrap().backend,
        config::Backend::Both
    );
    assert_eq!(
        app.sessions[1].trace.as_ref().unwrap().backend,
        config::Backend::Langfuse
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    app.kill_all();
    let rt = app.take_tracing().unwrap();
    rt.shutdown(Duration::from_secs(5)).await;

    // the local store: one turn (from the `both` launch), two launch rows
    let conn = open_ro(&db_path).unwrap();
    let traces: i64 = conn
        .query_row("SELECT count(*) FROM traces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(traces, 1, "the langfuse-only launch left no trace rows");
    let (local_trace_id, local_session_key): (String, String) = conn
        .query_row("SELECT id, session_key FROM traces", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    let backends: Vec<(String, String)> = {
        let mut st = conn
            .prepare("SELECT profile, json_extract(metadata, '$.backend') FROM launches ORDER BY started_ns")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(
        backends,
        vec![
            ("Claude Both".to_string(), "both".to_string()),
            ("Claude Langfuse".to_string(), "langfuse".to_string()),
        ]
    );
    let observations: i64 = conn
        .query_row("SELECT count(*) FROM observations", [], |r| r.get(0))
        .unwrap();
    assert!(
        observations >= 2,
        "generation + tool locally: {observations}"
    );

    // the server: trace-create for both sessions, observations for both,
    // and the `both` trace under the same id as the store
    let requests = stub.wait_for_requests(1, Duration::from_secs(5));
    let mut trace_ids = std::collections::HashSet::new();
    let mut kinds = std::collections::HashMap::<String, usize>::new();
    for r in &requests {
        assert_eq!(r.path, "/api/public/otel/v1/traces");
        for span in spans(r) {
            let kind = attr(&span, "langfuse.observation.type")
                .unwrap_or("?")
                .to_string();
            *kinds.entry(kind).or_default() += 1;
            if attr(&span, "langfuse.trace.name").is_some() {
                trace_ids.insert(span["traceId"].as_str().unwrap().to_string());
                assert_eq!(attr(&span, "langfuse.trace.input"), Some("run the tests"));
                assert!(span.get("parentSpanId").is_none(), "the turn is the root");
            }
        }
    }
    assert_eq!(
        trace_ids.len(),
        2,
        "both launches reached Langfuse: {kinds:?}"
    );
    assert!(
        trace_ids.contains(&local_trace_id),
        "same id in both stores"
    );
    assert!(
        kinds.get("generation").copied().unwrap_or(0) >= 2,
        "{kinds:?}"
    );
    assert!(kinds.get("tool").copied().unwrap_or(0) >= 2, "{kinds:?}");
    let before = stub.requests().len();

    // replay: dry run sends nothing, a real run re-sends the local session
    let ctx = MapCtx {
        prices: PriceTable::builtin(),
        version: "test".into(),
        user_id: None,
        release: None,
        environment: None,
        tags: vec![],
    };
    let dry = replay_sessions(
        &conn,
        cfg_for(&stub.addr),
        ctx.clone(),
        std::slice::from_ref(&local_session_key),
        true,
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(dry.sessions, 1);
    assert!(dry.events >= 3, "{dry:?}");
    assert!(
        dry.first_event
            .as_deref()
            .unwrap()
            .contains("langfuse.trace.name"),
        "the first span is the turn root: {:?}",
        dry.first_event
    );
    assert_eq!(stub.requests().len(), before, "dry run sent nothing");
    let sent = replay_sessions(
        &conn,
        cfg_for(&stub.addr),
        ctx,
        &[local_session_key],
        false,
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(sent.events, dry.events);
    assert_eq!(sent.dropped, 0);
    assert!(sent.notes.is_empty(), "{:?}", sent.notes);
    let requests = stub.wait_for_requests(before + 1, Duration::from_secs(5));
    let replayed = spans(&requests[before]);
    assert!(attr(&replayed[0], "langfuse.trace.name").is_some());
    assert_eq!(
        replayed[0]["traceId"],
        local_trace_id.as_str(),
        "the replay carries the store's ids, so Langfuse merges"
    );
}

#[cfg(unix)]
fn cfg_for(addr: &str) -> ExporterConfig {
    let mut c = cfg(addr);
    c.flush_interval = Duration::from_millis(30);
    c
}

#[cfg(unix)]
async fn pump_until_exit(
    app: &mut agent_mux::app::App,
    rx: &mut tokio::sync::mpsc::Receiver<agent_mux::events::AppEvent>,
    idx: usize,
) -> bool {
    use agent_mux::events::AppEvent;
    use agent_mux::status::Status;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if matches!(app.sessions[idx].status(Instant::now()), Status::Exited(_)) {
            return true;
        }
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                app.handle_pty_output(id, &bytes, Instant::now())
            }
            Ok(Some(AppEvent::PtyExit { id })) => app.handle_pty_exit(id),
            Ok(Some(AppEvent::TraceStats { launch_id, stats })) => {
                app.handle_trace_stats(&launch_id, stats)
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => return false,
        }
    }
    false
}
