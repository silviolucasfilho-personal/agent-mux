//! The bridge between the server's tokio tasks and the `App`.
//!
//! `App` is a single owned value on the main loop's stack: no task may
//! touch it. Commands travel in as `AppEvent::Remote`; frames travel out
//! through the [`RemoteHub`], which the server's per-connection tasks own
//! one queue each of. The hub never blocks and never awaits while holding
//! its lock.

use super::protocol::{NamedKey, ServerMsg};
use super::token::Token;
use crate::config::RemoteSettings;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Frames queued per client. PTY chunks are at most 4 KiB (`Session::spawn`
/// reads into a 4096-byte buffer), so a full queue holds about 1 MiB.
pub const CLIENT_QUEUE: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(pub u64);

/// One frame, encoded once and shared by every recipient.
#[derive(Debug, Clone)]
pub enum Outbound {
    Text(Arc<str>),
    Binary(Arc<[u8]>),
    Close(u16),
}

impl Outbound {
    pub fn text(msg: &ServerMsg) -> Outbound {
        Outbound::Text(Arc::from(msg.to_json()))
    }
}

/// Everything a client asks the App to do. `session` is always a stable
/// `Session.id`.
#[derive(Debug)]
pub enum RemoteCommand {
    /// The socket authenticated and said hello; the App answers with the
    /// opening frames.
    Hello {
        client: ClientId,
        focus: Option<usize>,
    },
    Goodbye {
        client: ClientId,
    },
    Input {
        client: ClientId,
        session: usize,
        bytes: Vec<u8>,
    },
    Key {
        client: ClientId,
        session: usize,
        key: NamedKey,
    },
    Paste {
        client: ClientId,
        session: usize,
        text: String,
    },
    Select {
        client: ClientId,
        session: usize,
    },
    Resync {
        client: ClientId,
        session: usize,
    },
    Catalog {
        client: ClientId,
    },
    Launch {
        client: ClientId,
        req_id: Option<String>,
        profile: String,
        dir: String,
    },
    Kill {
        client: ClientId,
        req_id: Option<String>,
        session: usize,
    },
    LaunchSkill {
        client: ClientId,
        req_id: Option<String>,
        skill: String,
        harness: Option<String>,
        dir: Option<String>,
    },
    StartLoop {
        client: ClientId,
        req_id: Option<String>,
        loop_id: String,
    },
    StartWorkflow {
        client: ClientId,
        req_id: Option<String>,
        workflow: String,
        workspace: String,
        harness: Option<String>,
        args: serde_json::Value,
    },
}

struct ClientSlot {
    tx: mpsc::Sender<Outbound>,
    /// Which session's output this client wants.
    focus: Option<usize>,
    /// The client fell behind and its stream has a hole: no more output
    /// until a resync re-establishes the screen.
    needs_resync: bool,
}

#[derive(Default)]
struct HubInner {
    clients: HashMap<ClientId, ClientSlot>,
    next: u64,
}

/// Fan-out to the connected clients.
pub struct RemoteHub {
    inner: Mutex<HubInner>,
    pub settings: RemoteSettings,
    token: Token,
    pub addr: std::net::SocketAddr,
}

impl std::fmt::Debug for RemoteHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteHub")
            .field("addr", &self.addr)
            .field("clients", &self.client_count())
            .finish()
    }
}

impl RemoteHub {
    pub fn new(settings: RemoteSettings, token: Token, addr: std::net::SocketAddr) -> RemoteHub {
        RemoteHub {
            inner: Mutex::new(HubInner::default()),
            settings,
            token,
            addr,
        }
    }

    pub fn token(&self) -> &Token {
        &self.token
    }

    /// The URL to open, token included. The only place the secret is put
    /// into a string a human will see.
    pub fn url(&self) -> String {
        format!("http://{}/?token={}", self.addr, self.token.expose())
    }

    pub fn is_loopback(&self) -> bool {
        self.addr.ip().is_loopback()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HubInner> {
        // A panic while holding this lock would only ever poison a map of
        // channel senders; recovering keeps the TUI alive.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn client_count(&self) -> usize {
        self.lock().clients.len()
    }

    pub fn register(&self) -> (ClientId, mpsc::Receiver<Outbound>) {
        let (tx, rx) = mpsc::channel(CLIENT_QUEUE);
        let mut inner = self.lock();
        inner.next += 1;
        let id = ClientId(inner.next);
        inner.clients.insert(
            id,
            ClientSlot {
                tx,
                focus: None,
                needs_resync: false,
            },
        );
        (id, rx)
    }

    pub fn unregister(&self, id: ClientId) {
        self.lock().clients.remove(&id);
    }

    pub fn set_focus(&self, id: ClientId, session: Option<usize>) {
        if let Some(slot) = self.lock().clients.get_mut(&id) {
            slot.focus = session;
            // A switch is served by a resync anyway, so any hole is healed.
            slot.needs_resync = false;
        }
    }

    pub fn focus(&self, id: ClientId) -> Option<usize> {
        self.lock().clients.get(&id).and_then(|s| s.focus)
    }

    /// Queues one frame. `false` when the client is gone.
    pub fn send_to(&self, id: ClientId, out: Outbound) -> bool {
        let mut inner = self.lock();
        let Some(slot) = inner.clients.get_mut(&id) else {
            return false;
        };
        // Control frames are idempotent snapshots: dropping one costs at
        // most a second of staleness, so a full queue is not fatal.
        slot.tx.try_send(out).is_ok()
    }

    pub fn broadcast(&self, out: Outbound) {
        let mut inner = self.lock();
        for slot in inner.clients.values_mut() {
            let _ = slot.tx.try_send(out.clone());
        }
    }

    /// Terminal output for `session`, to every client watching it. A client
    /// whose queue is full is marked for resync and receives nothing more
    /// until it gets one -- a gap in a terminal stream corrupts the screen,
    /// so a clean redraw beats a partial one.
    pub fn push_output(&self, session: usize, frame: Arc<[u8]>) {
        let mut inner = self.lock();
        for slot in inner.clients.values_mut() {
            if slot.focus != Some(session) || slot.needs_resync {
                continue;
            }
            if slot.tx.try_send(Outbound::Binary(frame.clone())).is_err() {
                slot.needs_resync = true;
            }
        }
    }

    /// Clients that fell behind and now have room again, with the session
    /// each is watching.
    pub fn take_resync_requests(&self) -> Vec<(ClientId, usize)> {
        let inner = self.lock();
        inner
            .clients
            .iter()
            .filter(|(_, s)| s.needs_resync && s.tx.capacity() > 0)
            .filter_map(|(id, s)| s.focus.map(|f| (*id, f)))
            .collect()
    }

    /// Delivers a resync and clears the hole it heals.
    pub fn deliver_resync(&self, id: ClientId, frame: Arc<[u8]>) -> bool {
        let mut inner = self.lock();
        let Some(slot) = inner.clients.get_mut(&id) else {
            return false;
        };
        if slot.tx.try_send(Outbound::Binary(frame)).is_err() {
            return false;
        }
        slot.needs_resync = false;
        true
    }

    /// Asks every client to close, for shutdown.
    pub fn close_all(&self, code: u16) {
        self.broadcast(Outbound::Close(code));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub() -> RemoteHub {
        RemoteHub::new(
            RemoteSettings::default(),
            Token::from_config("a-token-long-enough").unwrap(),
            "127.0.0.1:7681".parse().unwrap(),
        )
    }

    #[tokio::test]
    async fn output_reaches_only_the_clients_watching_that_session() {
        let hub = hub();
        let (a, mut rx_a) = hub.register();
        let (b, mut rx_b) = hub.register();
        hub.set_focus(a, Some(1));
        hub.set_focus(b, Some(2));

        hub.push_output(1, Arc::from(&b"for-one"[..]));
        assert!(matches!(rx_a.try_recv(), Ok(Outbound::Binary(_))));
        assert!(rx_b.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_slow_client_is_marked_for_resync_and_then_withheld() {
        let hub = hub();
        let (id, mut rx) = hub.register();
        hub.set_focus(id, Some(1));

        // Overrun the queue by one frame.
        for _ in 0..CLIENT_QUEUE + 1 {
            hub.push_output(1, Arc::from(&b"x"[..]));
        }
        assert_eq!(hub.take_resync_requests(), vec![]); // no room yet

        // Draining makes room; the client is owed a resync, not more output.
        let _ = rx.try_recv().unwrap();
        assert_eq!(hub.take_resync_requests(), vec![(id, 1)]);
        drain(&mut rx);
        hub.push_output(1, Arc::from(&b"withheld"[..]));
        assert_eq!(drain(&mut rx), 0, "output flowed despite the hole");

        // The resync heals it and output resumes.
        assert!(hub.deliver_resync(id, Arc::from(&b"screen"[..])));
        assert!(matches!(rx.try_recv(), Ok(Outbound::Binary(_))));
        assert_eq!(hub.take_resync_requests(), vec![]);
        hub.push_output(1, Arc::from(&b"again"[..]));
        assert!(matches!(rx.try_recv(), Ok(Outbound::Binary(_))));
    }

    fn drain(rx: &mut mpsc::Receiver<Outbound>) -> usize {
        let mut n = 0;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        n
    }

    #[tokio::test]
    async fn switching_focus_clears_a_pending_hole() {
        let hub = hub();
        let (id, mut rx) = hub.register();
        hub.set_focus(id, Some(1));
        for _ in 0..CLIENT_QUEUE + 1 {
            hub.push_output(1, Arc::from(&b"x"[..]));
        }
        drain(&mut rx);
        assert_eq!(hub.take_resync_requests(), vec![(id, 1)]);
        hub.set_focus(id, Some(2));
        assert_eq!(hub.take_resync_requests(), vec![]);
    }

    #[tokio::test]
    async fn unregistered_clients_are_no_longer_addressable() {
        let hub = hub();
        let (id, _rx) = hub.register();
        assert_eq!(hub.client_count(), 1);
        hub.unregister(id);
        assert_eq!(hub.client_count(), 0);
        assert!(!hub.send_to(id, Outbound::Close(1000)));
        assert!(!hub.deliver_resync(id, Arc::from(&b""[..])));
    }

    #[test]
    fn the_url_carries_the_token_and_loopback_is_detected() {
        let hub = hub();
        assert_eq!(
            hub.url(),
            "http://127.0.0.1:7681/?token=a-token-long-enough"
        );
        assert!(hub.is_loopback());
        let open = RemoteHub::new(
            RemoteSettings::default(),
            Token::from_config("a-token-long-enough").unwrap(),
            "0.0.0.0:7681".parse().unwrap(),
        );
        assert!(!open.is_loopback());
        // Debug must not leak the token.
        assert!(!format!("{open:?}").contains("a-token"));
    }
}
