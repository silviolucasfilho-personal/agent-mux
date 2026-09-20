//! The remote control end to end, with no terminal and no browser.
//!
//! A headless `App` (the shape `agent-mux run` and the workflow CLI use)
//! plus the real server on an ephemeral port, driven by a hand-written
//! WebSocket client that shares no code with `src/remote/ws.rs`.

#[path = "support/ws_client.rs"]
mod ws_client;

use agent_mux::app::App;
use agent_mux::config::{Profile, RemoteSettings};
use agent_mux::events::AppEvent;
use agent_mux::remote::{self, RemoteServer};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use ws_client::{Msg, WsClient};

const TOKEN: &str = "test-token-0123456789";
const SHORT: Duration = Duration::from_secs(3);

fn shell_profile(args: &[&str]) -> Profile {
    #[cfg(windows)]
    let command = "cmd.exe";
    #[cfg(not(windows))]
    let command = "sh";
    Profile {
        name: "shell".into(),
        command: command.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        default_dir: Some(std::env::temp_dir().to_string_lossy().into_owned()),
        tracing: None,
        model: None,
        bypass_approvals: None,
    }
}

struct Harness {
    app: App,
    rx: mpsc::Receiver<AppEvent>,
    server: Option<RemoteServer>,
    addr: std::net::SocketAddr,
}

impl Harness {
    async fn start(settings: RemoteSettings) -> Harness {
        let (tx, rx) = mpsc::channel(1024);
        let mut app = App::new(vec![shell_profile(&[])], None, tx.clone());
        app.clipboard_enabled = false;
        app.set_pane_size(24, 80);
        let server = remote::start(settings, tx).await.expect("server starts");
        let addr = server.hub.addr;
        app.remote = Some(std::sync::Arc::clone(&server.hub));
        Harness {
            app,
            rx,
            server: Some(server),
            addr,
        }
    }

    async fn default() -> Harness {
        Harness::start(RemoteSettings {
            enabled: true,
            listen: "127.0.0.1:0".into(),
            token: Some(TOKEN.into()),
            ..RemoteSettings::default()
        })
        .await
    }

    /// Pumps events (including remote commands) until `pred` or the deadline.
    async fn pump_until(&mut self, timeout: Duration, mut pred: impl FnMut(&App) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if pred(&self.app) {
                return true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(remaining.min(Duration::from_millis(20)), self.rx.recv())
                .await
            {
                Ok(Some(event)) => self.app.handle_event(event),
                Ok(None) => break,
                // A tick keeps the session list flowing while nothing else
                // is happening, exactly as the TUI's 250 ms timer does.
                Err(_) => self.app.on_tick(Instant::now()),
            }
        }
        pred(&self.app)
    }

    /// Pumps for a fixed span, with no condition.
    async fn pump_for(&mut self, span: Duration) {
        self.pump_until(span, |_| false).await;
    }

    async fn finish(mut self) {
        self.app.kill_all();
        if let Some(server) = self.server.take() {
            server.shutdown(Duration::from_millis(500)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_page_needs_the_token_and_then_sets_a_cookie() {
    let h = Harness::default().await;

    let denied = ws_client::get(h.addr, "/").unwrap();
    assert_eq!(denied.status, 401);
    assert!(
        denied.body.contains("Access token"),
        "the 401 should offer the token form"
    );
    assert!(denied.header("set-cookie").is_none());

    assert_eq!(ws_client::get(h.addr, "/?token=wrong").unwrap().status, 401);

    let ok = ws_client::get(h.addr, &format!("/?token={TOKEN}")).unwrap();
    assert_eq!(ok.status, 200);
    let cookie = ok.header("set-cookie").expect("cookie is set");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    assert!(ok.body.contains("agent-mux"));
    // The page's own security headers travel with it.
    assert!(ok.header("content-security-policy").is_some());
    assert_eq!(ok.header("x-frame-options"), Some("DENY"));

    // Assets are public; unknown paths are not, and cannot be escaped from.
    assert_eq!(
        ws_client::get(h.addr, "/assets/app.js").unwrap().status,
        200
    );
    assert_eq!(
        ws_client::get(h.addr, "/assets/nope.js").unwrap().status,
        404
    );
    assert_eq!(
        ws_client::get(h.addr, "/assets/../src/main.rs")
            .unwrap()
            .status,
        404
    );

    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_is_greeted_with_the_session_list_and_the_catalog() {
    let mut h = Harness::default().await;
    let mut ws = WsClient::connect(h.addr, TOKEN).expect("upgrade");
    ws.send_json(serde_json::json!({"t": "hello"}));

    h.pump_for(Duration::from_millis(300)).await;

    let hello = ws.recv_json("hello", SHORT).expect("hello frame");
    assert_eq!(hello["cols"], 80);
    assert_eq!(hello["rows"], 24);
    assert_eq!(hello["allow_kill"], true);

    let sessions = ws.recv_json("sessions", SHORT).expect("sessions frame");
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 0);
    assert_eq!(sessions["cols"], 80);

    let catalog = ws.recv_json("catalog", SHORT).expect("catalog frame");
    let profiles = catalog["profiles"].as_array().unwrap();
    assert_eq!(profiles[0]["name"], "shell");

    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_mirrors_and_takes_typed_input() {
    let mut h = Harness::default().await;
    // `cat` echoes whatever is typed, which is the smallest thing that
    // proves the whole loop: input in, PTY out, frame to the browser.
    let idx = h
        .app
        .launch(shell_profile(&["-c", "cat"]), std::env::temp_dir())
        .unwrap();
    let session_id = h.app.sessions[idx].id;

    let mut ws = WsClient::connect(h.addr, TOKEN).expect("upgrade");
    ws.send_json(serde_json::json!({"t": "hello"}));
    h.pump_for(Duration::from_millis(300)).await;

    let sessions = ws.recv_json("sessions", SHORT).expect("sessions frame");
    let listed = &sessions["sessions"][0];
    assert_eq!(listed["session_id"], session_id);
    assert_eq!(listed["name"], "shell");
    assert_eq!(sessions["selected"], session_id);

    // Selecting re-sends the whole screen.
    ws.send_json(serde_json::json!({"t": "select", "s": session_id}));
    h.pump_for(Duration::from_millis(200)).await;
    let (resync_id, _) = ws.recv_kind(0x02, SHORT).expect("a resync frame");
    assert_eq!(resync_id, session_id);

    // Typing reaches the PTY and the echo comes back as output.
    ws.send_input(session_id, b"hello-from-the-phone\n");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = false;
    while Instant::now() < deadline && !seen {
        h.pump_for(Duration::from_millis(100)).await;
        seen = ws.wait_for_output(
            session_id,
            "hello-from-the-phone",
            Duration::from_millis(200),
        );
    }
    assert!(seen, "the session never echoed the typed text back");

    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_named_key_is_encoded_server_side_and_reaches_the_session() {
    let mut h = Harness::default().await;
    let idx = h
        .app
        .launch(shell_profile(&["-c", "cat"]), std::env::temp_dir())
        .unwrap();
    let session_id = h.app.sessions[idx].id;

    let mut ws = WsClient::connect(h.addr, TOKEN).expect("upgrade");
    ws.send_json(serde_json::json!({"t": "hello", "focus": session_id}));
    h.pump_for(Duration::from_millis(300)).await;
    let _ = ws.recv_kind(0x02, SHORT);

    // A helper button: text, then Enter as a named key rather than bytes.
    ws.send_input(session_id, b"typed-then-enter");
    ws.send_json(serde_json::json!({"t": "key", "s": session_id, "key": "enter"}));

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = false;
    while Instant::now() < deadline && !seen {
        h.pump_for(Duration::from_millis(100)).await;
        seen = ws.wait_for_output(session_id, "typed-then-enter", Duration::from_millis(200));
    }
    assert!(seen, "the named Enter key never reached the session");

    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn killing_is_refused_when_the_server_disallows_it() {
    let mut h = Harness::start(RemoteSettings {
        enabled: true,
        listen: "127.0.0.1:0".into(),
        token: Some(TOKEN.into()),
        allow_kill: false,
        ..RemoteSettings::default()
    })
    .await;
    let idx = h
        .app
        .launch(shell_profile(&["-c", "sleep 30"]), std::env::temp_dir())
        .unwrap();
    let session_id = h.app.sessions[idx].id;

    let mut ws = WsClient::connect(h.addr, TOKEN).expect("upgrade");
    ws.send_json(serde_json::json!({"t": "hello"}));
    h.pump_for(Duration::from_millis(300)).await;

    ws.send_json(serde_json::json!({"t": "kill", "id": "r1", "s": session_id}));
    h.pump_for(Duration::from_millis(300)).await;

    let result = ws.recv_json("result", SHORT).expect("a result frame");
    assert_eq!(result["ok"], false);
    assert_eq!(result["id"], "r1");
    assert!(
        result["error"].as_str().unwrap().contains("disabled"),
        "{result}"
    );
    assert!(
        !matches!(
            h.app.sessions[0].status(Instant::now()),
            agent_mux::status::Status::Exited(_)
        ),
        "the session must still be alive"
    );

    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn killing_works_when_it_is_allowed() {
    let mut h = Harness::default().await;
    let idx = h
        .app
        .launch(shell_profile(&["-c", "sleep 30"]), std::env::temp_dir())
        .unwrap();
    let session_id = h.app.sessions[idx].id;

    let mut ws = WsClient::connect(h.addr, TOKEN).expect("upgrade");
    ws.send_json(serde_json::json!({"t": "hello"}));
    h.pump_for(Duration::from_millis(300)).await;

    ws.send_json(serde_json::json!({"t": "kill", "s": session_id}));
    let exited = h
        .pump_until(Duration::from_secs(5), |app| {
            matches!(
                app.sessions[0].status(Instant::now()),
                agent_mux::status::Status::Exited(_)
            )
        })
        .await;
    assert!(exited, "the session should have been killed");

    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn launching_a_session_from_the_remote_creates_one() {
    let mut h = Harness::default().await;
    let mut ws = WsClient::connect(h.addr, TOKEN).expect("upgrade");
    ws.send_json(serde_json::json!({"t": "hello"}));
    h.pump_for(Duration::from_millis(300)).await;

    let dir = std::env::temp_dir().to_string_lossy().into_owned();
    ws.send_json(serde_json::json!({"t": "launch", "id": "L1", "profile": "shell", "dir": dir}));
    let made = h
        .pump_until(Duration::from_secs(5), |app| !app.sessions.is_empty())
        .await;
    assert!(made, "no session appeared");

    let result = ws.recv_json("result", SHORT).expect("a result frame");
    assert_eq!(result["ok"], true);
    assert_eq!(result["id"], "L1");
    assert_eq!(result["s"], h.app.sessions[0].id);

    // An unknown profile is reported, not silently ignored.
    ws.send_json(serde_json::json!({"t": "launch", "id": "L2", "profile": "nope", "dir": dir}));
    h.pump_for(Duration::from_millis(300)).await;
    let mut refusal = None;
    while let Some(v) = ws.recv_json("result", Duration::from_millis(500)) {
        if v["id"] == "L2" {
            refusal = Some(v);
            break;
        }
    }
    let refusal = refusal.expect("a refusal for L2");
    assert_eq!(refusal["ok"], false);
    assert!(
        refusal["error"].as_str().unwrap().contains("nope"),
        "{refusal}"
    );

    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_upgrade_refuses_a_bad_token_a_foreign_origin_and_a_crowd() {
    let mut h = Harness::start(RemoteSettings {
        enabled: true,
        listen: "127.0.0.1:0".into(),
        token: Some(TOKEN.into()),
        ..RemoteSettings::default()
    })
    .await;

    assert_eq!(
        WsClient::connect(h.addr, "wrong-token-here").err(),
        Some(401)
    );
    assert_eq!(
        WsClient::connect_with(
            h.addr,
            &[
                &format!("Cookie: amx_token={TOKEN}"),
                "Origin: http://evil.example",
            ]
        )
        .err(),
        Some(403)
    );

    // Fill every slot, then check the next one is turned away.
    let mut clients = Vec::new();
    for _ in 0..8 {
        clients.push(WsClient::connect(h.addr, TOKEN).expect("upgrade"));
    }
    h.pump_for(Duration::from_millis(200)).await;
    assert_eq!(WsClient::connect(h.addr, TOKEN).err(), Some(503));

    drop(clients);
    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_frame_closes_the_socket() {
    let mut h = Harness::default().await;
    let mut ws = WsClient::connect(h.addr, TOKEN).expect("upgrade");
    ws.send_text(&"x".repeat(2 * 1024 * 1024));
    h.pump_for(Duration::from_millis(300)).await;

    let deadline = Instant::now() + SHORT;
    let mut closed = None;
    while Instant::now() < deadline && closed.is_none() {
        match ws.recv(Duration::from_millis(200)) {
            Some(Msg::Close(code)) => closed = Some(code),
            Some(_) => {}
            None => {}
        }
    }
    assert_eq!(
        closed,
        Some(1009),
        "expected a 1009 close for an oversized frame"
    );

    h.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_closes_every_client() {
    let mut h = Harness::default().await;
    let mut ws = WsClient::connect(h.addr, TOKEN).expect("upgrade");
    ws.send_json(serde_json::json!({"t": "hello"}));
    h.pump_for(Duration::from_millis(200)).await;

    let server = h.server.take().unwrap();
    server.shutdown(Duration::from_millis(500)).await;

    let deadline = Instant::now() + SHORT;
    let mut closed = None;
    while Instant::now() < deadline && closed.is_none() {
        match ws.recv(Duration::from_millis(200)) {
            Some(Msg::Close(code)) => closed = Some(code),
            Some(_) => {}
            None => break,
        }
    }
    assert_eq!(closed, Some(1001), "expected a going-away close");

    h.finish().await;
}
