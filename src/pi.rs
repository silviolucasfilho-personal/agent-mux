//! Pi's per-launch MCP channel.
//!
//! Pi (0.73.1, probed 2026-11-10) has no MCP client on purpose: its README
//! says "No MCP … build an extension that adds MCP support", and `pi --help`
//! has no MCP flag, variable or config key. It does load an extension file
//! per launch with `-e <path>` (repeatable; jiti, so plain ES modules work),
//! and an extension may register tools with `pi.registerTool` whose
//! `parameters` Pi validates as plain JSON Schema. agent-mux's extension
//! (`src/pi_mcp_extension.js`) is therefore a one-server MCP client: it
//! starts the server named in `AGENT_MUX_PI_MCP`, lists its tools and
//! registers each one under its own name. Probed: `pi --mode rpc -e <it>`
//! lists the 24 `agent_mux_*` tools among the active tools, and a call
//! returns the server's result.
//!
//! The extension file lives under `~/.agent-mux/pi-extension/`, agent-mux's
//! own directory, like the OpenCode plugin; nothing under `~/.pi` is
//! written. `mcp::register::plan` writes it (when missing or changed) and
//! returns the `-e` argument and the variable.

use serde_json::json;
use std::path::{Path, PathBuf};

/// The extension compiled into agent-mux.
pub const EXTENSION_JS: &str = include_str!("pi_mcp_extension.js");
/// The variable the extension reads the server from.
pub const MCP_ENV: &str = "AGENT_MUX_PI_MCP";
/// Pi's flag that loads one extension file.
pub const EXTENSION_FLAG: &str = "-e";

/// `<home>/.agent-mux/pi-extension/agent-mux-mcp.js`.
pub fn extension_path(home: &Path) -> PathBuf {
    home.join(".agent-mux")
        .join("pi-extension")
        .join("agent-mux-mcp.js")
}

/// Writes the extension when it is missing or differs from the one
/// compiled in, and returns its path. A relative home is refused, so the
/// file never lands in whatever folder agent-mux runs in.
pub fn install_extension(home: &Path) -> std::io::Result<PathBuf> {
    if !home.is_absolute() {
        return Err(std::io::Error::other(format!(
            "no home directory to write the Pi extension under ({})",
            home.display()
        )));
    }
    let path = extension_path(home);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(EXTENSION_JS) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, EXTENSION_JS)?;
    }
    Ok(path)
}

/// The `AGENT_MUX_PI_MCP` value: the server's name, program and arguments.
pub fn mcp_spec(name: &str, exe: &Path, argv: &[String]) -> String {
    json!({ "name": name, "command": exe.to_string_lossy(), "args": argv }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_once_and_refuses_a_relative_home() {
        let home = tempfile::tempdir().unwrap();
        let path = install_extension(home.path()).unwrap();
        assert_eq!(path, extension_path(home.path()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_JS);
        // a stale copy is replaced
        std::fs::write(&path, "old").unwrap();
        install_extension(home.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_JS);
        assert!(install_extension(Path::new("rel")).is_err());
    }

    #[test]
    fn spec_keeps_paths_as_arguments() {
        let spec: serde_json::Value = serde_json::from_str(&mcp_spec(
            "agent-mux",
            Path::new("/opt/a b/agent-mux"),
            &["mcp".into(), "--db".into(), "/tmp/a b/t.db".into()],
        ))
        .unwrap();
        assert_eq!(spec["command"], "/opt/a b/agent-mux");
        assert_eq!(spec["args"][2], "/tmp/a b/t.db");
        assert_eq!(spec["name"], "agent-mux");
    }

    #[test]
    fn extension_reads_the_variable_and_registers_tools() {
        assert!(EXTENSION_JS.contains("export default async function"));
        assert!(EXTENSION_JS.contains(MCP_ENV));
        for needle in ["initialize", "tools/list", "tools/call", "session_shutdown"] {
            assert!(EXTENSION_JS.contains(needle), "{needle}");
        }
    }
}
