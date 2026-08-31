use agent_mux::langfuse::export::{ExporterConfig, probe, spawn_exporter};
use agent_mux::langfuse::otlp::{Span, STATUS_UNSET, str_attr};
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

#[derive(Clone, Copy, Debug)]
enum Scripted {
    Ok200,
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

/// Minimal scripted HTTP/1.1 server on a std TcpListener (no dev-deps).
/// Handles sequential keep-alive requests per connection; unscripted
/// requests get 200 `{}`.
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
                let mut chunk = [0u8; 4096];
                loop {
                    // read until a full request (headers + Content-Length body) is in buf
                    let request = loop {
                        if let Some(header_end) =
                            buf.windows(4).position(|w| w == b"\r\n\r\n")
                        {
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
                    let path = request_line.split_whitespace().nth(1).unwrap_or("").to_string();
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
                        .unwrap_or(Scripted::Ok200);
                    let response = match action {
                        Scripted::Ok200 => {
                            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_string()
                        }
                        Scripted::Status(code) => format!(
                            "HTTP/1.1 {code} X\r\nContent-Length: 0\r\n\r\n"
                        ),
                        Scripted::RateLimited(seconds) => format!(
                            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: {seconds}\r\nContent-Length: 0\r\n\r\n"
                        ),
                        Scripted::Hang(pause) => {
                            std::thread::sleep(pause);
                            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_string()
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

fn test_config(stub: &Stub) -> ExporterConfig {
    let mut cfg = ExporterConfig::new(&stub.addr, "pk-test", "sk-test", 30);
    cfg.connect_timeout = Duration::from_millis(500);
    cfg.request_timeout = Duration::from_secs(5);
    cfg.backoff_base = Duration::from_millis(10);
    cfg
}

fn span(name: &str) -> Span {
    Span {
        trace_id: [7; 16],
        span_id: [8; 8],
        parent_span_id: None,
        name: name.into(),
        start_nanos: 1,
        end_nanos: 2,
        attributes: vec![str_attr("langfuse.session.id", "s")],
        status_code: STATUS_UNSET,
        status_message: None,
    }
}

type StatusLog = Arc<Mutex<Vec<(&'static str, String)>>>;

fn status_sink() -> (StatusLog, agent_mux::langfuse::export::StatusSink) {
    let log: StatusLog = Arc::new(Mutex::new(Vec::new()));
    let log_clone = Arc::clone(&log);
    (
        log,
        Box::new(move |class, msg| log_clone.lock().unwrap().push((class, msg))),
    )
}

fn wait_for<F: Fn() -> bool>(pred: F, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    pred()
}

#[test]
fn posts_correct_path_headers_and_body() {
    let stub = stub_server();
    let (_log, sink) = status_sink();
    let handle = spawn_exporter(test_config(&stub), sink);
    handle.tx.try_send(span("turn 1")).unwrap();
    assert!(wait_for(
        || !stub.requests.lock().unwrap().is_empty(),
        Duration::from_secs(5)
    ));
    assert!(handle.finish(Duration::from_secs(5)));
    let requests = stub.requests.lock().unwrap();
    let req = &requests[0];
    assert_eq!(req.path, "/api/public/otel/v1/traces");
    assert_eq!(
        req.header("authorization"),
        Some(format!("Basic {}", agent_mux::langfuse::otlp::base64_encode(b"pk-test:sk-test")).as_str())
    );
    assert_eq!(req.header("content-type"), Some("application/json"));
    assert_eq!(req.header("x-langfuse-ingestion-version"), Some("4"));
    assert!(req.header("user-agent").unwrap().starts_with("agent-mux/"));
    let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
    assert_eq!(
        body["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["name"],
        "turn 1"
    );
    assert_eq!(
        body["resourceSpans"][0]["resource"]["attributes"][0]["key"],
        "service.name"
    );
}

#[test]
fn rate_limit_honors_retry_after_then_succeeds() {
    let stub = stub_server();
    stub.script.lock().unwrap().push_back(Scripted::RateLimited(1));
    let (_log, sink) = status_sink();
    let handle = spawn_exporter(test_config(&stub), sink);
    let started = Instant::now();
    handle.tx.try_send(span("limited")).unwrap();
    assert!(wait_for(
        || stub.requests.lock().unwrap().len() >= 2,
        Duration::from_secs(5)
    ));
    // the retry waited out Retry-After: 1
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "retry came too fast: {:?}",
        started.elapsed()
    );
    assert!(handle.finish(Duration::from_secs(5)));
    assert_eq!(stub.requests.lock().unwrap().len(), 2);
}

#[test]
fn two_auth_failures_disable_the_exporter() {
    let stub = stub_server();
    {
        let mut script = stub.script.lock().unwrap();
        script.push_back(Scripted::Status(401));
        script.push_back(Scripted::Status(401));
    }
    let (log, sink) = status_sink();
    let handle = spawn_exporter(test_config(&stub), sink);
    // two batches, two 401s
    handle.tx.try_send(span("a")).unwrap();
    assert!(wait_for(|| stub.requests.lock().unwrap().len() == 1, Duration::from_secs(5)));
    handle.tx.try_send(span("b")).unwrap();
    assert!(wait_for(|| stub.requests.lock().unwrap().len() == 2, Duration::from_secs(5)));
    // third batch: disabled, no request goes out
    handle.tx.try_send(span("c")).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(stub.requests.lock().unwrap().len(), 2);
    let dropped = Arc::clone(&handle.dropped);
    assert!(handle.finish(Duration::from_secs(5)));
    assert!(dropped.load(Ordering::Relaxed) >= 3);
    let log = log.lock().unwrap();
    assert_eq!(
        log.iter().filter(|(class, _)| *class == "auth").count(),
        1,
        "auth status emitted once per run: {log:?}"
    );
}

#[test]
fn breaker_opens_after_threshold_and_half_open_probe_recovers() {
    let stub = stub_server();
    {
        let mut script = stub.script.lock().unwrap();
        script.push_back(Scripted::Status(500));
        script.push_back(Scripted::Status(500));
        // after the open window, the half-open probe gets a 200 (default)
    }
    let mut cfg = test_config(&stub);
    cfg.attempts = 1; // one failed POST = one failed batch
    cfg.breaker_threshold = 2;
    cfg.breaker_open = Duration::from_millis(300);
    let (log, sink) = status_sink();
    let handle = spawn_exporter(cfg, sink);
    let dropped = Arc::clone(&handle.dropped);
    handle.tx.try_send(span("f1")).unwrap();
    assert!(wait_for(|| stub.requests.lock().unwrap().len() == 1, Duration::from_secs(5)));
    handle.tx.try_send(span("f2")).unwrap();
    assert!(wait_for(|| stub.requests.lock().unwrap().len() == 2, Duration::from_secs(5)));
    // breaker now open: this batch is dropped without a request. Wait on
    // the exporter's own drop counter (f1+f2 batches counted too), not a
    // wall-clock sleep that races the 300ms breaker window on slow CI.
    handle.tx.try_send(span("dropped")).unwrap();
    assert!(wait_for(
        || dropped.load(Ordering::Relaxed) >= 3,
        Duration::from_secs(5)
    ));
    assert_eq!(stub.requests.lock().unwrap().len(), 2);
    assert!(
        log.lock().unwrap().iter().any(|(class, _)| *class == "breaker"),
        "breaker status emitted"
    );
    // after the window, the half-open probe goes through and succeeds
    std::thread::sleep(Duration::from_millis(250));
    handle.tx.try_send(span("recovered")).unwrap();
    assert!(wait_for(|| stub.requests.lock().unwrap().len() == 3, Duration::from_secs(5)));
    assert!(handle.finish(Duration::from_secs(5)));
}

#[test]
fn queue_is_bounded_and_rejects_when_full() {
    let stub = stub_server();
    stub.script.lock().unwrap().push_back(Scripted::Hang(Duration::from_millis(700)));
    let mut cfg = test_config(&stub);
    cfg.max_batch_spans = 1; // first span flushes immediately, hanging the thread
    let (_log, sink) = status_sink();
    let handle = spawn_exporter(cfg, sink);
    handle.tx.try_send(span("hang")).unwrap();
    std::thread::sleep(Duration::from_millis(100)); // let the flush start
    // while the exporter hangs, the bounded queue eventually rejects
    let mut rejected = false;
    for i in 0..5000 {
        if handle.tx.try_send(span(&format!("s{i}"))).is_err() {
            rejected = true;
            break;
        }
    }
    assert!(rejected, "queue accepted 5000 spans while the exporter hung");
    assert!(handle.finish(Duration::from_secs(10)));
}

#[test]
fn shutdown_deadline_is_respected_with_hanging_server() {
    let stub = stub_server();
    stub.script.lock().unwrap().push_back(Scripted::Hang(Duration::from_secs(3)));
    let mut cfg = test_config(&stub);
    cfg.max_batch_spans = 1;
    cfg.request_timeout = Duration::from_secs(10);
    let (_log, sink) = status_sink();
    let handle = spawn_exporter(cfg, sink);
    handle.tx.try_send(span("stuck")).unwrap();
    std::thread::sleep(Duration::from_millis(100)); // flush starts, hangs
    let started = Instant::now();
    let finished = handle.finish(Duration::from_millis(300));
    assert!(!finished, "finish should time out while the server hangs");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "finish overstayed its deadline: {:?}",
        started.elapsed()
    );
}

#[test]
fn breaker_open_skips_the_final_flush() {
    let stub = stub_server();
    stub.script.lock().unwrap().push_back(Scripted::Status(500));
    let mut cfg = test_config(&stub);
    cfg.attempts = 1;
    cfg.breaker_threshold = 1; // first failure opens the breaker
    cfg.breaker_open = Duration::from_secs(60);
    let (_log, sink) = status_sink();
    let handle = spawn_exporter(cfg, sink);
    handle.tx.try_send(span("opens")).unwrap();
    assert!(wait_for(|| stub.requests.lock().unwrap().len() == 1, Duration::from_secs(5)));
    // queue another span, then finish: the final drain must be skipped
    // instantly because the breaker is open
    handle.tx.try_send(span("never sent")).unwrap();
    let started = Instant::now();
    assert!(handle.finish(Duration::from_secs(5)));
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "final flush was attempted against a dead endpoint: {:?}",
        started.elapsed()
    );
    assert_eq!(stub.requests.lock().unwrap().len(), 1);
}

#[test]
fn doctor_probe_reports_ok_and_auth_errors() {
    let stub = stub_server();
    assert!(probe(&stub.addr, "pk", "sk").is_ok());
    let request = {
        let requests = stub.requests.lock().unwrap();
        requests.last().unwrap().clone()
    };
    assert_eq!(request.path, "/api/public/otel/v1/traces");
    assert_eq!(request.body, r#"{"resourceSpans":[]}"#);
    stub.script.lock().unwrap().push_back(Scripted::Status(401));
    let err = probe(&stub.addr, "pk", "bad").unwrap_err();
    assert!(err.contains("authentication"), "{err}");
}
