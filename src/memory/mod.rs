//! Shared memory through ai-memory (https://github.com/akitaonrails/ai-memory):
//! agent-mux installs and wires it, ai-memory does the remembering. One
//! ai-memory server (loopback, `127.0.0.1:49374`) captures every session
//! through the hooks it puts into each harness and answers them over MCP,
//! so Claude Code, Codex and Antigravity share one memory per project
//! whether or not agent-mux launched them.
//!
//! This module reads the state (`State::detect`): the binary, the server,
//! the launchd agent that keeps it running, and for each harness whether
//! its CLI is installed and whether ai-memory's MCP entry and hooks are in
//! its configuration. `install` turns what is missing into steps, each
//! one a command shown before it runs; `cli` and the Memory screen
//! (`crate::app::memory_view`) share them.
//!
//! What ai-memory writes, as read in its v2.6.7 source (`install_mcp.rs`,
//! `install_hooks.rs`, `docs/mcp-install.md`):
//!
//! | harness | MCP entry | hooks |
//! |---|---|---|
//! | Claude Code | `~/.claude.json` `mcpServers.ai-memory` | `~/.claude/settings.json` |
//! | Codex | `~/.codex/config.toml` `[mcp_servers.ai-memory]` | `~/.codex/hooks.json` |
//! | Antigravity | `~/.gemini/config/mcp_config.json` `mcpServers.ai-memory` | `~/.gemini/config/hooks.json` (group `ai-memory`) |
//!
//! `CLAUDE_CONFIG_DIR` and `CODEX_HOME` move the first two, as they do for
//! ai-memory's installers. A hook is ai-memory's when its command names
//! the ai-memory binary or its staged hooks (`.../ai-memory/hooks/...`).

pub mod backfill;
pub mod cli;
pub mod feed;
pub mod install;
pub mod query;

use crate::harness::Harness;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The server's default address (`ai-memory serve --transport http`).
pub const DEFAULT_ADDR: &str = "127.0.0.1:49374";
/// The launchd agent's label (`packaging/launchd`).
pub const LAUNCHD_LABEL: &str = "com.github.akitaonrails.ai-memory";
/// The name of ai-memory's MCP entry in each harness.
pub const MCP_NAME: &str = "ai-memory";
/// The harnesses agent-mux wires, in order.
pub const HARNESSES: [Harness; 3] = [Harness::Claude, Harness::Codex, Harness::Antigravity];

/// ai-memory's name for a harness (`--agent` / `--client`).
pub fn agent_name(h: Harness) -> &'static str {
    match h {
        Harness::Claude => "claude-code",
        Harness::Codex => "codex",
        Harness::Antigravity => "antigravity-cli",
    }
}

/// A harness's short name, for steps and the Memory screen.
pub fn label(h: Harness) -> &'static str {
    match h {
        Harness::Claude => "Claude Code",
        Harness::Codex => "Codex",
        Harness::Antigravity => "Antigravity",
    }
}

/// Where agent-mux puts a release it downloads.
pub fn install_dir(home: &Path) -> PathBuf {
    home.join("Applications").join("ai-memory")
}

/// The launchd agent's file.
pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

/// ai-memory's data folder: `$AI_MEMORY_DATA_DIR`, else its macOS default.
pub fn data_dir(home: &Path) -> PathBuf {
    match std::env::var_os("AI_MEMORY_DATA_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => home.join("Library/Application Support/ai-memory"),
    }
}

/// The server address the clients use: `$AI_MEMORY_SERVER_URL`'s host and
/// port, else the default.
pub fn server_addr() -> String {
    std::env::var("AI_MEMORY_SERVER_URL")
        .ok()
        .and_then(|u| {
            let rest = u.split("://").nth(1)?;
            let host = rest.split('/').next()?;
            (!host.is_empty()).then(|| host.to_string())
        })
        .unwrap_or_else(|| DEFAULT_ADDR.to_string())
}

/// The ai-memory binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binary {
    pub path: PathBuf,
    /// `ai-memory --version`, first line.
    pub version: Option<String>,
}

/// One harness's wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wiring {
    pub harness: Harness,
    /// Its CLI is on `PATH`.
    pub cli: bool,
    pub mcp: bool,
    pub hooks: bool,
}

impl Wiring {
    pub fn complete(&self) -> bool {
        self.mcp && self.hooks
    }
}

/// Everything the install flow looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub binary: Option<Binary>,
    /// The data folder has its `config.toml` (`ai-memory init` ran).
    pub initialized: bool,
    /// Something answers on the server's address.
    pub server: bool,
    pub launchd: bool,
    pub harnesses: Vec<Wiring>,
    /// agent-mux feeds ai-memory from its traces (`feed`).
    pub feed: bool,
}

impl State {
    /// Reads the machine. Spawns `ai-memory --version` and connects to the
    /// server's port; touches nothing.
    pub fn detect() -> State {
        let home = home();
        let binary = find_binary(&home).map(|path| Binary {
            version: version(&path),
            path,
        });
        State {
            binary,
            initialized: data_dir(&home).join("config.toml").is_file(),
            server: server_up(&server_addr(), Duration::from_millis(400)),
            launchd: plist_path(&home).is_file(),
            harnesses: HARNESSES
                .iter()
                .map(|&h| wiring(&home, h, on_path(h.as_str()).is_some()))
                .collect(),
            feed: feed::enabled(),
        }
    }

    /// Whether every harness whose CLI is installed is wired, and the
    /// server runs.
    pub fn ready(&self) -> bool {
        self.binary.is_some()
            && self.server
            && self
                .harnesses
                .iter()
                .filter(|w| w.cli)
                .all(Wiring::complete)
    }

    pub fn wiring(&self, h: Harness) -> Option<&Wiring> {
        self.harnesses.iter().find(|w| w.harness == h)
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// A program on `PATH`.
pub fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// The ai-memory binary: `$AI_MEMORY_BIN`, then `PATH`, then where
/// agent-mux installs a release, then the menu bar app's.
pub fn find_binary(home: &Path) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AI_MEMORY_BIN").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    on_path("ai-memory")
        .or_else(|| Some(install_dir(home).join("ai-memory")).filter(|p| p.is_file()))
        .or_else(|| {
            Some(PathBuf::from(
                "/Applications/AI Memory.app/Contents/Resources/runtime/ai-memory",
            ))
            .filter(|p| p.is_file())
        })
}

fn version(bin: &Path) -> Option<String> {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Whether something accepts connections on `addr`.
pub fn server_up(addr: &str, timeout: Duration) -> bool {
    use std::net::ToSocketAddrs;
    addr.to_socket_addrs()
        .ok()
        .into_iter()
        .flatten()
        .any(|a| std::net::TcpStream::connect_timeout(&a, timeout).is_ok())
}

/// The harness's configuration files ai-memory writes: (MCP, hooks).
pub fn config_files(home: &Path, h: Harness) -> (PathBuf, PathBuf) {
    match h {
        Harness::Claude => {
            let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from);
            match dir {
                Some(d) => (d.join(".claude.json"), d.join("settings.json")),
                None => (
                    home.join(".claude.json"),
                    home.join(".claude").join("settings.json"),
                ),
            }
        }
        Harness::Codex => {
            let dir = std::env::var_os("CODEX_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            (dir.join("config.toml"), dir.join("hooks.json"))
        }
        Harness::Antigravity => {
            let dir = home.join(".gemini").join("config");
            (dir.join("mcp_config.json"), dir.join("hooks.json"))
        }
    }
}

/// One harness's wiring, from its files.
pub fn wiring(home: &Path, h: Harness, cli: bool) -> Wiring {
    let (mcp_file, hooks_file) = config_files(home, h);
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
    let mcp = match h {
        Harness::Codex => codex_has_mcp(&read(&mcp_file)),
        _ => json_has_mcp(&read(&mcp_file)),
    };
    Wiring {
        harness: h,
        cli,
        mcp,
        hooks: hooks_mention_ai_memory(&read(&hooks_file)),
    }
}

/// A JSON config with `mcpServers.ai-memory`.
pub fn json_has_mcp(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("mcpServers")?.get(MCP_NAME).cloned())
        .is_some()
}

/// A Codex `config.toml` with `[mcp_servers.ai-memory]`.
pub fn codex_has_mcp(text: &str) -> bool {
    text.parse::<toml::Table>()
        .ok()
        .and_then(|t| t.get("mcp_servers")?.get(MCP_NAME).cloned())
        .is_some()
}

/// A hooks file with a command that runs ai-memory: the binary
/// (`'/…/ai-memory' hook --event …`) or a staged script
/// (`…/ai-memory/hooks/<agent>/….sh`), or Antigravity's `ai-memory` group.
pub fn hooks_mention_ai_memory(text: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    if v.get(MCP_NAME).is_some() || v.get("hooks").and_then(|h| h.get(MCP_NAME)).is_some() {
        return true;
    }
    let mut found = false;
    visit_strings(&v, &mut |s| {
        if is_ai_memory_command(s) {
            found = true;
        }
    });
    found
}

fn is_ai_memory_command(s: &str) -> bool {
    let words = s.replace(['\'', '"'], " ");
    words.split_whitespace().any(|w| {
        let w = w.trim_end_matches(['/', ';']);
        w == "ai-memory" || w.ends_with("/ai-memory") || w.contains("/ai-memory/hooks/")
    })
}

fn visit_strings(v: &Value, f: &mut impl FnMut(&str)) {
    match v {
        Value::String(s) => f(s),
        Value::Array(a) => a.iter().for_each(|x| visit_strings(x, f)),
        Value::Object(o) => o.values().for_each(|x| visit_strings(x, f)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_entries_are_found_in_each_format() {
        assert!(json_has_mcp(
            r#"{"mcpServers":{"ai-memory":{"type":"http","url":"http://127.0.0.1:49374/mcp"}}}"#
        ));
        assert!(!json_has_mcp(r#"{"mcpServers":{"other":{}}}"#));
        assert!(!json_has_mcp("not json"));
        assert!(codex_has_mcp(
            "model = \"x\"\n[mcp_servers.ai-memory]\nurl = \"http://127.0.0.1:49374/mcp\"\n"
        ));
        assert!(!codex_has_mcp(
            "[mcp_servers.agent-mux]\ncommand = \"agent-mux\"\n"
        ));
    }

    #[test]
    fn hooks_are_ai_memorys_when_a_command_runs_it() {
        let native = r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"'/Users/u/Applications/ai-memory/ai-memory' hook --event session-start --agent claude-code --server-url http://127.0.0.1:49374"}]}]}}"#;
        assert!(hooks_mention_ai_memory(native));
        let script = r#"{"hooks":{"Stop":[{"command":"sh '/Users/u/Library/Application Support/ai-memory/hooks/codex/stop.sh'"}]}}"#;
        assert!(hooks_mention_ai_memory(script));
        assert!(hooks_mention_ai_memory(
            r#"{"ai-memory":{"PreInvocation":[]}}"#
        ));
        // agent-mux's own tracing hooks are not ai-memory's
        let ours = r#"{"hooks":{"SessionStart":[{"hooks":[{"command":"'/Users/u/.cargo/bin/agent-mux' trace hook claude"}]}]}}"#;
        assert!(!hooks_mention_ai_memory(ours));
        assert!(!hooks_mention_ai_memory(""));
    }

    #[test]
    fn wiring_reads_each_harnesss_files() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        std::fs::create_dir_all(home.join(".gemini/config")).unwrap();
        std::fs::write(
            home.join(".gemini/config/mcp_config.json"),
            r#"{"mcpServers":{"ai-memory":{"serverUrl":"http://127.0.0.1:49374/mcp"}}}"#,
        )
        .unwrap();
        let w = wiring(home, Harness::Antigravity, true);
        assert!(w.mcp && !w.hooks && !w.complete());
        std::fs::write(
            home.join(".gemini/config/hooks.json"),
            r#"{"ai-memory":{"PreInvocation":[{"command":"/x/ai-memory hook"}]}}"#,
        )
        .unwrap();
        assert!(wiring(home, Harness::Antigravity, true).complete());
        assert!(!wiring(home, Harness::Claude, true).mcp);
    }

    #[test]
    fn the_server_address_follows_the_clients_variable() {
        assert_eq!(DEFAULT_ADDR, "127.0.0.1:49374");
        assert!(!server_up("127.0.0.1:1", Duration::from_millis(100)));
    }
}
