//! End-to-end: a fake `claude` binary (a shell script) reads the injected
//! `--session-id` from its argv, appends Claude-shaped JSONL into a temp
//! claude_dir, and exits; the full pipeline (plan -> spawn -> correlate ->
//! tail -> assemble -> export) must land turn spans AND both lifecycle spans
//! on a stub server, with the flush completing after `kill_all` within the
//! shutdown deadline.
#![cfg(unix)]

use agent_mux::app::{App, Mode};
use agent_mux::config::{self, Profile};
use agent_mux::events::AppEvent;
use agent_mux::langfuse::LangfuseRuntime;
use agent_mux::status::Status;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Minimal always-200 HTTP stub recording request bodies.
fn stub_server() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let bodies_thread = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let bodies = Arc::clone(&bodies_thread);
            std::thread::spawn(move || {
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let request = loop {
                        if let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers =
                                String::from_utf8_lossy(&buf[..header_end]).into_owned();
                            let content_length = headers
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
                                break Some(body);
                            }
                        }
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => break None,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let Some(body) = request else { return };
                    bodies.lock().unwrap().push(body);
                    if stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    (addr, bodies)
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
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                app.handle_pty_output(id, &bytes, Instant::now());
            }
            Ok(Some(AppEvent::PtyExit { id })) => app.handle_pty_exit(id),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    pred(app)
}

fn all_span_names(bodies: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    for body in bodies {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
            continue;
        };
        let Some(resource_spans) = v["resourceSpans"].as_array() else {
            continue;
        };
        for rs in resource_spans {
            let Some(scope_spans) = rs["scopeSpans"].as_array() else {
                continue;
            };
            for ss in scope_spans {
                if let Some(spans) = ss["spans"].as_array() {
                    for span in spans {
                        if let Some(name) = span["name"].as_str() {
                            names.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    names
}

#[tokio::test]
async fn full_pipeline_exports_turn_and_lifecycle_spans() {
    let (addr, bodies) = stub_server();
    let temp = tempfile::tempdir().unwrap();
    let claude_dir = temp.path().join("claude-home");
    let workdir = temp.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    let projects_dir = claude_dir
        .join("projects")
        .join(agent_mux::history::project_slug(&workdir));
    std::fs::create_dir_all(&projects_dir).unwrap();

    // The fake `claude`: finds --session-id in argv, appends a Claude-shaped
    // conversation into the expected transcript path, lingers briefly so the
    // tailer sees a live file, exits 0.
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
printf '%s\n' '{{"type":"user","timestamp":"2026-08-31T10:00:00Z","message":{{"role":"user","content":[{{"type":"text","text":"run the tests"}}]}}}}' >> "$out"
sleep 0.2
printf '%s\n' '{{"type":"assistant","timestamp":"2026-08-31T10:00:01Z","message":{{"role":"assistant","model":"claude-fable-5","usage":{{"input_tokens":42,"output_tokens":7}},"content":[{{"type":"text","text":"running"}},{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":"cargo test"}}}}]}}}}' >> "$out"
printf '%s\n' '{{"type":"user","timestamp":"2026-08-31T10:00:02Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"ok","is_error":false}}]}}}}' >> "$out"
sleep 0.5
exit 0
"#,
        proj = projects_dir.display()
    );
    std::fs::write(&script_path, script).unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Resolved settings pointing at the temp dirs and the stub server.
    let toml = format!(
        r#"
        [langfuse]
        enabled = true
        host = "{addr}"
        public_key = "pk-lf-e2e"
        secret_key = "sk-lf-e2e"
        content_mode = "full"
        poll_interval_ms = 60
        flush_interval_ms = 60
        claude_dir = "{claude}"

        [[profiles]]
        name = "Claude Code"
        command = "{cmd}"
        args = []
        default_dir = "{dir}"
        "#,
        claude = claude_dir.display(),
        cmd = script_path.display(),
        dir = workdir.display(),
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_langfuse(cfg.langfuse.as_ref(), &|_| None).unwrap();
    let profiles: Vec<Profile> = cfg.profiles;

    let (tx, mut rx) = mpsc::channel(1024);
    let runtime = LangfuseRuntime::new(resolved, tx.clone());
    let mut app = App::new(profiles, Some(runtime), tx);
    app.clipboard_enabled = false;

    // n -> Enter creates the session through the real dialog path.
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(&key(KeyCode::Char('n')), Instant::now());
    assert!(matches!(app.mode, Mode::NewSession(_)));
    app.handle_key(&key(KeyCode::Enter), Instant::now());
    assert!(matches!(app.mode, Mode::Control), "spawn failed: {:?}", app.error);
    assert_eq!(app.sessions.len(), 1);
    assert!(app.sessions[0].trace.is_some(), "pipeline attached");

    // Session runs to completion.
    let exited = pump_until(&mut rx, &mut app, Duration::from_secs(15), |a| {
        matches!(a.sessions[0].status(Instant::now()), Status::Exited(_))
    })
    .await;
    assert!(exited, "fake claude never exited");
    assert_eq!(app.sessions[0].status(Instant::now()), Status::Exited(Some(0)));

    // Quit path: kill_all, then the bounded shutdown flush.
    app.kill_all();
    let rt = app.take_langfuse().unwrap();
    let flush_started = Instant::now();
    rt.shutdown(Duration::from_secs(5)).await;
    assert!(
        flush_started.elapsed() < Duration::from_secs(5),
        "shutdown overstayed: {:?}",
        flush_started.elapsed()
    );

    let bodies = bodies.lock().unwrap();
    let names = all_span_names(&bodies);
    assert!(
        names.iter().any(|n| n == "session_started"),
        "session_started missing: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "session_ended"),
        "session_ended missing: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "turn 1"),
        "turn root missing: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "assistant"),
        "generation missing: {names:?}"
    );
    assert!(names.iter().any(|n| n == "Bash"), "tool span missing: {names:?}");

    // Spot-check content and identity on the turn root.
    let joined = bodies.join("\n");
    assert!(joined.contains("run the tests"), "full-mode trace input missing");
    assert!(joined.contains("gen_ai.usage.input_tokens"), "usage attrs missing");
    assert!(
        joined.contains("langfuse.session.id"),
        "session grouping attr missing"
    );
}
