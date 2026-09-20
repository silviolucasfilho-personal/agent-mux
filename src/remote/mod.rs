//! Remote control: the running TUI serves a small web app so a phone or
//! another computer can watch every session and type into it.
//!
//! The rule that shapes the module: **a server task never touches `App`.**
//! `App` is one owned value on the main loop's stack, so commands travel in
//! as [`bridge::RemoteCommand`] on the existing `AppEvent` channel and
//! frames travel out through the [`bridge::RemoteHub`]. That keeps the
//! remote from adding any locking to the terminal path, and it is why the
//! integration test can drive the whole feature with no terminal at all.
//!
//! Nothing here is on by default: `[remote] enabled` or `--remote` opts in,
//! the listener binds loopback unless told otherwise, and a per-run bearer
//! token guards every route. There is no TLS -- `docs/remote-control.md`
//! covers Tailscale and SSH tunnels for going beyond the machine.

pub mod bridge;
pub mod http;
pub mod protocol;
pub mod server;
pub mod token;
pub mod web;
pub mod ws;

use crate::app::Notice;
use crate::config::RemoteSettings;
use crate::events::AppEvent;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// A running remote-control server.
pub struct RemoteServer {
    pub hub: Arc<bridge::RemoteHub>,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for RemoteServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteServer")
            .field("addr", &self.hub.addr)
            .finish()
    }
}

/// Binds the listener and starts accepting. The caller keeps the handle;
/// dropping it leaves the task running until shutdown.
pub async fn start(
    settings: RemoteSettings,
    tx: mpsc::Sender<AppEvent>,
) -> anyhow::Result<RemoteServer> {
    let token = match settings.token.as_deref() {
        Some(t) => token::Token::from_config(t).map_err(|e| anyhow::anyhow!("[remote] {e}"))?,
        None => token::Token::generate(),
    };
    let (listener, addr) = server::bind(&settings.listen)
        .await
        .map_err(|e| anyhow::anyhow!("cannot listen on {}: {e}", settings.listen))?;
    let hub = Arc::new(bridge::RemoteHub::new(settings, token, addr));
    let (shutdown, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(server::serve(
        listener,
        Arc::clone(&hub),
        tx,
        server::Limits::default(),
        shutdown_rx,
    ));
    Ok(RemoteServer {
        hub,
        shutdown,
        task,
    })
}

impl RemoteServer {
    /// The line the TUI shows at startup and in the About overlay.
    pub fn banner(&self) -> Notice {
        if self.hub.is_loopback() {
            Notice::info(format!("remote: {}  (v shows it again)", self.hub.url()))
        } else {
            // Worth a warning colour: the token is the only thing between
            // the network and every session's keyboard, over plain HTTP.
            Notice::warn(format!(
                "remote: {} — not loopback and not encrypted; prefer Tailscale or an SSH tunnel",
                self.hub.url()
            ))
        }
    }

    /// Closes every client and joins the accept task, bounded.
    pub async fn shutdown(self, deadline: Duration) {
        self.hub.close_all(ws::close::GOING_AWAY);
        let _ = self.shutdown.send(true);
        // Give the connection tasks a moment to flush their close frames.
        let _ = tokio::time::timeout(deadline, self.task).await;
    }
}

/// Applies `--remote` / `--remote <addr>` over the resolved settings.
/// A bare flag enables; an address also overrides `listen`.
pub fn apply_remote_cli(settings: &mut RemoteSettings, args: &[String]) {
    let Some(i) = args.iter().position(|a| a == "--remote") else {
        return;
    };
    settings.enabled = true;
    if let Some(next) = args.get(i + 1)
        && !next.starts_with('-')
    {
        settings.listen = normalize_listen(next);
    }
}

/// Accepts `9000`, `:9000`, `0.0.0.0:9000` and `host:port`.
fn normalize_listen(value: &str) -> String {
    let v = value.trim();
    if let Some(port) = v.strip_prefix(':') {
        return format!("127.0.0.1:{port}");
    }
    if !v.contains(':') && v.chars().all(|c| c.is_ascii_digit()) {
        return format!("127.0.0.1:{v}");
    }
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_flag_enables_and_an_address_overrides_listen() {
        let mut s = RemoteSettings::default();
        apply_remote_cli(&mut s, &args(["agent-mux", "--remote"].as_ref()));
        assert!(s.enabled);
        assert_eq!(s.listen, "127.0.0.1:7681");

        let mut s = RemoteSettings::default();
        apply_remote_cli(
            &mut s,
            &args(["agent-mux", "--remote", "0.0.0.0:9000"].as_ref()),
        );
        assert!(s.enabled);
        assert_eq!(s.listen, "0.0.0.0:9000");

        // A following flag is not an address.
        let mut s = RemoteSettings::default();
        apply_remote_cli(
            &mut s,
            &args(["agent-mux", "--remote", "--hide-sidebar"].as_ref()),
        );
        assert!(s.enabled);
        assert_eq!(s.listen, "127.0.0.1:7681");

        // Absent flag changes nothing.
        let mut s = RemoteSettings::default();
        apply_remote_cli(&mut s, &args(["agent-mux"].as_ref()));
        assert!(!s.enabled);
    }

    #[test]
    fn shorthand_addresses_expand_to_loopback() {
        assert_eq!(normalize_listen("9000"), "127.0.0.1:9000");
        assert_eq!(normalize_listen(":9000"), "127.0.0.1:9000");
        assert_eq!(normalize_listen("0.0.0.0:9000"), "0.0.0.0:9000");
        assert_eq!(normalize_listen("phone.local:80"), "phone.local:80");
    }

    #[tokio::test]
    async fn start_binds_a_port_and_shutdown_joins() {
        let (tx, _rx) = mpsc::channel(16);
        let settings = RemoteSettings {
            enabled: true,
            listen: "127.0.0.1:0".into(),
            token: Some("a-token-long-enough".into()),
            ..RemoteSettings::default()
        };
        let server = start(settings, tx).await.unwrap();
        assert_ne!(server.hub.addr.port(), 0, "an ephemeral port was assigned");
        assert!(server.hub.url().contains("a-token-long-enough"));
        assert!(server.banner().text.contains("remote:"));
        server.shutdown(Duration::from_millis(500)).await;
    }

    #[tokio::test]
    async fn a_short_configured_token_is_refused_at_startup() {
        let (tx, _rx) = mpsc::channel(16);
        let settings = RemoteSettings {
            enabled: true,
            listen: "127.0.0.1:0".into(),
            token: Some("tiny".into()),
            ..RemoteSettings::default()
        };
        let err = start(settings, tx).await.unwrap_err().to_string();
        assert!(err.contains("at least 16"), "{err}");
    }

    #[tokio::test]
    async fn an_off_loopback_bind_warns() {
        let (tx, _rx) = mpsc::channel(16);
        let settings = RemoteSettings {
            enabled: true,
            listen: "0.0.0.0:0".into(),
            token: Some("a-token-long-enough".into()),
            ..RemoteSettings::default()
        };
        let server = start(settings, tx).await.unwrap();
        let banner = server.banner();
        assert!(banner.text.contains("not encrypted"), "{}", banner.text);
        server.shutdown(Duration::from_millis(200)).await;
    }
}
