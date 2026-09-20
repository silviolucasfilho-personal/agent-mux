//! The listener, the three routes, and one task per connection.
//!
//! A connection task owns its socket and its outbound queue and nothing
//! else: commands go to the App through `AppEvent::Remote`, frames come
//! back through the hub. Nothing here can touch a `Session`.

use super::bridge::{ClientId, Outbound, RemoteCommand, RemoteHub};
use super::http::{self, Parsed, Request, Response};
use super::protocol::{self, ClientMsg, NamedKey};
use super::ws::{self, FrameReader, Message, Opcode, WsError, close};
use crate::events::AppEvent;
use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_clients: usize,
    pub max_message: usize,
    pub ping_every: Duration,
    pub idle_timeout: Duration,
    /// Typed bytes per second, and the burst a paste may use at once.
    pub input_bytes_per_s: f64,
    pub input_burst: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_clients: 8,
            max_message: 1024 * 1024,
            ping_every: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(45),
            input_bytes_per_s: 64.0 * 1024.0,
            input_burst: 128.0 * 1024.0,
        }
    }
}

pub async fn bind(listen: &str) -> std::io::Result<(TcpListener, std::net::SocketAddr)> {
    let listener = TcpListener::bind(listen).await?;
    let addr = listener.local_addr()?;
    Ok((listener, addr))
}

/// Accepts until `shutdown` flips. Each connection gets its own task so one
/// slow phone cannot stall the others.
pub async fn serve(
    listener: TcpListener,
    hub: Arc<RemoteHub>,
    tx: mpsc::Sender<AppEvent>,
    limits: Limits,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() { return; }
            }
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else { continue };
                let hub = Arc::clone(&hub);
                let tx = tx.clone();
                let limits = limits.clone();
                let shutdown = shutdown.clone();
                tokio::spawn(async move {
                    handle_connection(stream, hub, tx, limits, shutdown).await;
                });
            }
        }
    }
}

enum Route {
    Index { set_cookie: bool },
    Asset(&'static super::web::Asset),
    Upgrade(String),
    Reply(Response),
}

fn security_headers(r: Response) -> Response {
    r.header("Referrer-Policy", "no-referrer")
        .header("X-Frame-Options", "DENY")
        .header("X-Content-Type-Options", "nosniff")
        .header(
            "Content-Security-Policy",
            "default-src 'self'; connect-src 'self' ws: wss:; img-src 'self' data:; base-uri 'none'; form-action 'self'",
        )
}

/// Which token the request presented, if any.
fn presented(req: &Request) -> Option<&str> {
    req.query("token").or_else(|| req.cookie("amx_token"))
}

fn route(req: &Request, hub: &RemoteHub, limits: &Limits) -> Route {
    if req.method != "GET" {
        return Route::Reply(Response::text(405, "GET only"));
    }
    let authorized = presented(req).is_some_and(|t| hub.token().verify(t));

    match req.path.as_str() {
        "/" | "/index.html" => {
            if !authorized {
                let page = super::web::unauthorized();
                return Route::Reply(security_headers(
                    Response::new(401)
                        .body(page.content_type, Cow::Borrowed(page.body))
                        .header("Cache-Control", "no-store"),
                ));
            }
            // Move the token out of the URL and into an HttpOnly cookie so
            // page script can never read it and a shared link cannot carry
            // it by accident.
            Route::Index {
                set_cookie: req.query("token").is_some(),
            }
        }
        "/ws" => {
            if !authorized {
                // A close code the client can tell apart from a network
                // failure, so it stops retrying and asks for the token.
                return Route::Reply(Response::text(401, "unauthorized"));
            }
            if let (Some(origin), Some(host)) = (req.header("origin"), req.host_only())
                && http::host_of(origin).as_deref() != Some(host.as_str())
            {
                return Route::Reply(Response::text(403, "origin mismatch"));
            }
            if !req
                .header("upgrade")
                .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
                || !req.header_has_token("connection", "upgrade")
                || req.header("sec-websocket-version").map(str::trim) != Some("13")
            {
                return Route::Reply(Response::text(426, "expected a websocket upgrade"));
            }
            let Some(key) = req.header("sec-websocket-key") else {
                return Route::Reply(Response::text(400, "missing Sec-WebSocket-Key"));
            };
            if hub.client_count() >= limits.max_clients {
                return Route::Reply(Response::text(503, "too many remote clients"));
            }
            Route::Upgrade(key.to_string())
        }
        path => match path.strip_prefix("/assets/").and_then(super::web::lookup) {
            Some(asset) => Route::Asset(asset),
            None => Route::Reply(Response::text(404, "not found")),
        },
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    hub: Arc<RemoteHub>,
    tx: mpsc::Sender<AppEvent>,
    limits: Limits,
    shutdown: watch::Receiver<bool>,
) {
    let mut head = Vec::with_capacity(1024);
    let req = loop {
        match http::parse_request(&head) {
            Parsed::Complete { req, .. } => break req,
            Parsed::Invalid => {
                let _ = stream
                    .write_all(&Response::text(400, "bad request").to_bytes())
                    .await;
                return;
            }
            Parsed::Partial => {}
        }
        if head.len() > http::MAX_HEAD_BYTES {
            let _ = stream
                .write_all(&Response::text(413, "head too large").to_bytes())
                .await;
            return;
        }
        let mut chunk = [0u8; 2048];
        // A client that opens a socket and says nothing must not hold a slot.
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await;
        match read {
            Ok(Ok(0)) | Err(_) => return,
            Ok(Ok(n)) => head.extend_from_slice(&chunk[..n]),
            Ok(Err(_)) => return,
        }
    };

    match route(&req, &hub, &limits) {
        Route::Reply(res) => {
            let _ = stream.write_all(&res.to_bytes()).await;
        }
        Route::Asset(asset) => {
            let res = Response::new(200)
                .body(asset.content_type, Cow::Borrowed(asset.body))
                .header("Cache-Control", "public, max-age=86400");
            let _ = stream.write_all(&res.to_bytes()).await;
        }
        Route::Index { set_cookie } => {
            let page = super::web::index();
            let mut res = security_headers(
                Response::new(200)
                    .body(page.content_type, Cow::Borrowed(page.body))
                    .header("Cache-Control", "no-store"),
            );
            if set_cookie {
                res = res.header(
                    "Set-Cookie",
                    format!(
                        "amx_token={}; HttpOnly; SameSite=Strict; Path=/",
                        hub.token().expose()
                    ),
                );
            }
            let _ = stream.write_all(&res.to_bytes()).await;
        }
        Route::Upgrade(key) => {
            let accept = ws::accept_key(&key);
            if stream
                .write_all(&Response::switching_protocols(&accept).to_bytes())
                .await
                .is_ok()
            {
                serve_ws(stream, hub, tx, limits, shutdown).await;
            }
        }
    }
}

/// Spends bytes from a refilling bucket; `false` means "over budget".
struct InputBudget {
    tokens: f64,
    last: Instant,
    rate: f64,
    burst: f64,
}

impl InputBudget {
    fn new(limits: &Limits) -> InputBudget {
        InputBudget {
            tokens: limits.input_burst,
            last: Instant::now(),
            rate: limits.input_bytes_per_s,
            burst: limits.input_burst,
        }
    }

    fn spend(&mut self, n: usize) -> bool {
        let now = Instant::now();
        self.tokens =
            (self.tokens + now.duration_since(self.last).as_secs_f64() * self.rate).min(self.burst);
        self.last = now;
        if self.tokens < n as f64 {
            return false;
        }
        self.tokens -= n as f64;
        true
    }
}

async fn serve_ws(
    mut stream: TcpStream,
    hub: Arc<RemoteHub>,
    tx: mpsc::Sender<AppEvent>,
    limits: Limits,
    mut shutdown: watch::Receiver<bool>,
) {
    let (client, mut outbound) = hub.register();
    let mut reader = FrameReader::new(limits.max_message);
    let mut budget = InputBudget::new(&limits);
    let mut ping = tokio::time::interval(limits.ping_every);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_seen = Instant::now();
    let mut closing: Option<u16> = None;

    loop {
        tokio::select! {
            incoming = reader.next(&mut stream) => {
                last_seen = Instant::now();
                match incoming {
                    Err(WsError::Protocol(code)) => { closing = Some(code); break }
                    Err(_) => break,
                    Ok(Message::Close(_, _)) => { closing = Some(close::NORMAL); break }
                    Ok(Message::Ping(p)) => {
                        if write_frame(&mut stream, Opcode::Pong, &p).await.is_err() { break }
                    }
                    Ok(Message::Pong(_)) => {}
                    Ok(Message::Binary(frame)) => {
                        let Some((kind, session, payload)) = protocol::decode_binary(&frame) else { continue };
                        if kind != protocol::KIND_INPUT { continue }
                        if !budget.spend(payload.len()) {
                            hub.send_to(client, Outbound::text(&protocol::ServerMsg::Notice {
                                level: "warn".into(),
                                text: "typing too fast; some input was dropped".into(),
                            }));
                            continue;
                        }
                        if tx.send(AppEvent::Remote(RemoteCommand::Input {
                            client, session, bytes: payload.to_vec(),
                        })).await.is_err() { break }
                    }
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<ClientMsg>(&text) {
                            Err(e) => {
                                hub.send_to(client, Outbound::text(&protocol::ServerMsg::Notice {
                                    level: "error".into(),
                                    text: format!("bad message: {e}"),
                                }));
                            }
                            Ok(ClientMsg::Ping) => {
                                hub.send_to(client, Outbound::text(&protocol::ServerMsg::Pong));
                            }
                            Ok(msg) => {
                                if let Some(cmd) = into_command(msg, client, &hub)
                                    && tx.send(AppEvent::Remote(cmd)).await.is_err() { break }
                            }
                        }
                    }
                }
            }
            frame = outbound.recv() => {
                let Some(frame) = frame else { break };
                let sent = match &frame {
                    Outbound::Text(t) => write_frame(&mut stream, Opcode::Text, t.as_bytes()).await,
                    Outbound::Binary(b) => write_frame(&mut stream, Opcode::Binary, b).await,
                    Outbound::Close(code) => { closing = Some(*code); break }
                };
                if sent.is_err() { break }
            }
            _ = ping.tick() => {
                if last_seen.elapsed() > limits.idle_timeout { closing = Some(close::GOING_AWAY); break }
                if write_frame(&mut stream, Opcode::Ping, b"").await.is_err() { break }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() { closing = Some(close::GOING_AWAY); break }
            }
        }
    }

    if let Some(code) = closing {
        let _ = write_frame(&mut stream, Opcode::Close, &ws::close_payload(code, "")).await;
    }
    let _ = stream.shutdown().await;
    hub.unregister(client);
    let _ = tx
        .send(AppEvent::Remote(RemoteCommand::Goodbye { client }))
        .await;
}

async fn write_frame(
    stream: &mut TcpStream,
    opcode: Opcode,
    payload: &[u8],
) -> std::io::Result<()> {
    stream
        .write_all(&ws::encode_frame(opcode, payload, None))
        .await
}

/// Turns a parsed message into a command, refusing the ones this server is
/// configured not to allow before they ever reach the App.
fn into_command(msg: ClientMsg, client: ClientId, hub: &RemoteHub) -> Option<RemoteCommand> {
    let deny = |id: Option<String>, what: &str| {
        hub.send_to(
            client,
            Outbound::text(&protocol::ServerMsg::Result {
                id,
                ok: false,
                s: None,
                run: None,
                error: Some(format!("{what} is disabled on this server")),
            }),
        );
        None
    };
    match msg {
        ClientMsg::Ping => None,
        ClientMsg::Hello { focus } => Some(RemoteCommand::Hello { client, focus }),
        ClientMsg::Select { s } => Some(RemoteCommand::Select { client, session: s }),
        ClientMsg::Resync { s } => Some(RemoteCommand::Resync { client, session: s }),
        ClientMsg::Catalog => Some(RemoteCommand::Catalog { client }),
        ClientMsg::Paste { s, text } => Some(RemoteCommand::Paste {
            client,
            session: s,
            text,
        }),
        ClientMsg::Key { s, key, ch, mods } => match NamedKey::parse(&key, ch.as_deref(), &mods) {
            Some(key) => Some(RemoteCommand::Key {
                client,
                session: s,
                key,
            }),
            None => {
                hub.send_to(
                    client,
                    Outbound::text(&protocol::ServerMsg::Notice {
                        level: "error".into(),
                        text: format!("unknown key {key:?}"),
                    }),
                );
                None
            }
        },
        ClientMsg::Launch { id, profile, dir } => {
            if !hub.settings.allow_launch {
                return deny(id, "launching");
            }
            Some(RemoteCommand::Launch {
                client,
                req_id: id,
                profile,
                dir,
            })
        }
        ClientMsg::Kill { id, s } => {
            if !hub.settings.allow_kill {
                return deny(id, "killing");
            }
            Some(RemoteCommand::Kill {
                client,
                req_id: id,
                session: s,
            })
        }
        ClientMsg::LaunchSkill {
            id,
            skill,
            harness,
            dir,
        } => {
            if !hub.settings.allow_launch {
                return deny(id, "launching");
            }
            Some(RemoteCommand::LaunchSkill {
                client,
                req_id: id,
                skill,
                harness,
                dir,
            })
        }
        ClientMsg::StartLoop { id, loop_id } => {
            if !hub.settings.allow_launch {
                return deny(id, "launching");
            }
            Some(RemoteCommand::StartLoop {
                client,
                req_id: id,
                loop_id,
            })
        }
        ClientMsg::StartWorkflow {
            id,
            workflow,
            workspace,
            harness,
            args,
        } => {
            if !hub.settings.allow_launch {
                return deny(id, "launching");
            }
            Some(RemoteCommand::StartWorkflow {
                client,
                req_id: id,
                workflow,
                workspace,
                harness,
                args,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RemoteSettings;
    use crate::remote::token::Token;

    const TOKEN: &str = "a-token-long-enough";

    fn hub_with(settings: RemoteSettings) -> Arc<RemoteHub> {
        Arc::new(RemoteHub::new(
            settings,
            Token::from_config(TOKEN).unwrap(),
            "127.0.0.1:7681".parse().unwrap(),
        ))
    }

    fn hub() -> Arc<RemoteHub> {
        hub_with(RemoteSettings::default())
    }

    fn req(raw: &str) -> Request {
        match http::parse_request(raw.as_bytes()) {
            Parsed::Complete { req, .. } => req,
            other => panic!("{other:?}"),
        }
    }

    fn status(route: &Route) -> u16 {
        match route {
            Route::Reply(r) => r.status,
            Route::Index { .. } | Route::Asset(_) => 200,
            Route::Upgrade(_) => 101,
        }
    }

    #[test]
    fn the_index_needs_a_token_and_then_sets_a_cookie() {
        let hub = hub();
        let limits = Limits::default();
        assert_eq!(
            status(&route(&req("GET / HTTP/1.1\r\n\r\n"), &hub, &limits)),
            401
        );
        assert_eq!(
            status(&route(
                &req("GET /?token=wrong HTTP/1.1\r\n\r\n"),
                &hub,
                &limits
            )),
            401
        );
        let with_query = route(
            &req(&format!("GET /?token={TOKEN} HTTP/1.1\r\n\r\n")),
            &hub,
            &limits,
        );
        assert!(matches!(with_query, Route::Index { set_cookie: true }));
        // Once the cookie is set the query is no longer needed.
        let with_cookie = route(
            &req(&format!(
                "GET / HTTP/1.1\r\nCookie: amx_token={TOKEN}\r\n\r\n"
            )),
            &hub,
            &limits,
        );
        assert!(matches!(with_cookie, Route::Index { set_cookie: false }));
    }

    #[test]
    fn assets_are_public_but_only_from_the_table() {
        let hub = hub();
        let limits = Limits::default();
        assert!(matches!(
            route(&req("GET /assets/app.js HTTP/1.1\r\n\r\n"), &hub, &limits),
            Route::Asset(_)
        ));
        // No path joining: traversal is simply not in the table.
        for path in ["/assets/../src/main.rs", "/assets/nope.js", "/other"] {
            assert_eq!(
                status(&route(
                    &req(&format!("GET {path} HTTP/1.1\r\n\r\n")),
                    &hub,
                    &limits
                )),
                404,
                "{path}"
            );
        }
    }

    #[test]
    fn the_upgrade_checks_token_origin_headers_and_capacity() {
        let hub = hub();
        let limits = Limits::default();
        let good = format!(
            "GET /ws HTTP/1.1\r\nHost: 127.0.0.1:7681\r\nCookie: amx_token={TOKEN}\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        );
        assert!(matches!(
            route(&req(&good), &hub, &limits),
            Route::Upgrade(_)
        ));

        // No token.
        assert_eq!(
            status(&route(
                &req("GET /ws HTTP/1.1\r\nUpgrade: websocket\r\n\r\n"),
                &hub,
                &limits
            )),
            401
        );
        // A page on another origin may not open the socket.
        let cross = good.replace(
            "Host: 127.0.0.1:7681",
            "Host: 127.0.0.1:7681\r\nOrigin: http://evil.example",
        );
        assert_eq!(status(&route(&req(&cross), &hub, &limits)), 403);
        // Matching origin is fine.
        let same = good.replace(
            "Host: 127.0.0.1:7681",
            "Host: 127.0.0.1:7681\r\nOrigin: http://127.0.0.1:7681",
        );
        assert!(matches!(
            route(&req(&same), &hub, &limits),
            Route::Upgrade(_)
        ));
        // Missing the handshake headers.
        let bad = good.replace("Sec-WebSocket-Version: 13\r\n", "");
        assert_eq!(status(&route(&req(&bad), &hub, &limits)), 426);

        // Capacity.
        let small = Limits {
            max_clients: 1,
            ..Limits::default()
        };
        let _slot = hub.register();
        assert_eq!(status(&route(&req(&good), &hub, &small)), 503);
    }

    #[test]
    fn non_get_is_refused() {
        let hub = hub();
        assert_eq!(
            status(&route(
                &req("POST / HTTP/1.1\r\n\r\n"),
                &hub,
                &Limits::default()
            )),
            405
        );
    }

    #[test]
    fn the_index_response_carries_the_security_headers() {
        let res = security_headers(Response::new(200));
        let names: Vec<&str> = res.headers.iter().map(|(k, _)| k.as_str()).collect();
        for want in [
            "Referrer-Policy",
            "X-Frame-Options",
            "X-Content-Type-Options",
            "Content-Security-Policy",
        ] {
            assert!(names.contains(&want), "missing {want}");
        }
    }

    #[test]
    fn disallowed_commands_are_refused_before_reaching_the_app() {
        let hub = hub_with(RemoteSettings {
            allow_kill: false,
            allow_launch: false,
            ..RemoteSettings::default()
        });
        let (client, mut rx) = hub.register();

        assert!(
            into_command(
                ClientMsg::Kill {
                    id: Some("r".into()),
                    s: 1
                },
                client,
                &hub
            )
            .is_none()
        );
        let Some(Outbound::Text(json)) = rx.try_recv().ok() else {
            panic!("expected a refusal frame")
        };
        assert!(
            json.contains("\"ok\":false") && json.contains("killing is disabled"),
            "{json}"
        );

        assert!(
            into_command(
                ClientMsg::Launch {
                    id: None,
                    profile: "p".into(),
                    dir: "/".into()
                },
                client,
                &hub
            )
            .is_none()
        );
        // Reading is always allowed.
        assert!(matches!(
            into_command(ClientMsg::Select { s: 3 }, client, &hub),
            Some(RemoteCommand::Select { session: 3, .. })
        ));
    }

    #[test]
    fn allowed_commands_map_across() {
        let hub = hub();
        let (client, _rx) = hub.register();
        assert!(matches!(
            into_command(ClientMsg::Hello { focus: Some(2) }, client, &hub),
            Some(RemoteCommand::Hello { focus: Some(2), .. })
        ));
        assert!(matches!(
            into_command(
                ClientMsg::Key {
                    s: 1,
                    key: "esc".into(),
                    ch: None,
                    mods: vec![]
                },
                client,
                &hub
            ),
            Some(RemoteCommand::Key { session: 1, .. })
        ));
        // An unknown key name is reported, not forwarded.
        assert!(
            into_command(
                ClientMsg::Key {
                    s: 1,
                    key: "hyper".into(),
                    ch: None,
                    mods: vec![]
                },
                client,
                &hub
            )
            .is_none()
        );
        assert!(matches!(
            into_command(ClientMsg::Kill { id: None, s: 9 }, client, &hub),
            Some(RemoteCommand::Kill { session: 9, .. })
        ));
    }

    #[test]
    fn the_input_budget_refills_over_time() {
        let limits = Limits {
            input_bytes_per_s: 1000.0,
            input_burst: 100.0,
            ..Limits::default()
        };
        let mut budget = InputBudget::new(&limits);
        assert!(budget.spend(100));
        assert!(!budget.spend(100), "burst should be exhausted");
        // Rewind the clock rather than sleeping.
        budget.last -= Duration::from_millis(200);
        assert!(budget.spend(100), "should have refilled");
    }
}
