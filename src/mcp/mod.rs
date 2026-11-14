//! The agent-mux MCP surface: a read-only stdio server over
//! `TraceService` (`agent-mux mcp serve --stdio`), the per-launch
//! registration fragments for Claude Code, Codex and OpenCode, and the
//! opt-in installer for Antigravity (`agent-mux mcp install agy`).
//!
//! Probed on 2026-09-15: Claude Code 2.1.273 `--mcp-config <configs...>`
//! (JSON strings or files); Codex 0.154.0 `-c mcp_servers.<name>.command=…`
//! / `.args=[…]`; agy 1.2.3 `agy mcp add <name> <cmd> -- <args…>` writing
//! `~/.gemini/config/mcp_config.json`, no per-launch flag; opencode 2.0.32
//! (2026-10-31) `mcp.servers.<name>` in `OPENCODE_CONFIG_CONTENT`, read by a
//! `--standalone` launch (`crate::opencode`). 2026-11-10: grok 1.0.70 has
//! no per-launch route for its TUI and gets a user-level entry through
//! `grok mcp add` (`install`); pi 0.73.1 has no MCP client and gets
//! agent-mux's client extension with `-e` (`crate::pi`).

pub mod install;
pub mod register;
pub mod server;
pub mod swarm_tools;

use std::path::Path;

/// The MCP server name every harness sees.
pub const SERVER_NAME: &str = "agent-mux";

pub const USAGE: &str = "agent-mux mcp <command>

  serve --stdio [--db PATH] [--workspace DIR | --all-workspaces | --workspace-from-env]
                                serve the ten read-only trace tools over stdio
  install agy|grok              register this binary with `agy|grok mcp add` (no per-launch flag exists)
  uninstall agy|grok            remove it with `agy|grok mcp remove`
  status [claude|codex|agy|opencode|grok|pi|muse]
                                how each harness reaches the server

Claude Code, Codex, OpenCode, Pi and Muse are registered per launch by the Agents sidebar;
nothing under ~/.claude, ~/.codex, ~/.config/opencode, ~/.pi or ~/.config/muse is modified.";

pub fn run(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("serve") => server::serve_cli(&args[1..]),
        Some("install") | Some("uninstall") | Some("status") => install::cli(args),
        Some("help") | Some("--help") | Some("-h") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => {
            println!("unknown mcp command: {other}\n\n{USAGE}");
            Ok(())
        }
    }
}

/// `trace doctor`'s MCP section: (ok, label, detail) lines.
pub fn doctor_lines(db: &Path, home: &Path) -> Vec<(bool, String, String)> {
    let mut out = Vec::new();
    let exe = crate::tracing::hooks::register::current_exe();
    match &exe {
        Some(e) => out.push((
            true,
            "binary".into(),
            format!(
                "{} (absolute; per-launch registration possible)",
                e.display()
            ),
        )),
        None => out.push((
            false,
            "binary".into(),
            "not absolute: Claude/Codex per-launch registration is skipped".into(),
        )),
    }
    match server::selftest(exe.as_deref(), db) {
        Ok(summary) => out.push((true, "serve".into(), summary)),
        Err(e) => out.push((false, "serve".into(), e)),
    }
    let agy = install::agy_status(home, exe.as_deref());
    out.push((
        agy.installed && !agy.disabled && agy.current,
        "agy".into(),
        agy.note(),
    ));
    // Grok only where it is installed: most machines have none.
    let grok = install::grok_status(home, exe.as_deref());
    if grok.installed || crate::session::install_fallback("grok", home).is_some() {
        out.push((grok.usable() && grok.current, "grok".into(), grok.note()));
    }
    out
}
