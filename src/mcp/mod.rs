//! The agent-mux MCP surface: a read-only stdio server over
//! `TraceService` (`agent-mux mcp serve --stdio`), the per-launch
//! registration fragments for Claude Code and Codex, and the opt-in
//! installer for Antigravity (`agent-mux mcp install agy`).
//!
//! Probed on 2026-09-15: Claude Code 2.1.273 `--mcp-config <configs...>`
//! (JSON strings or files); Codex 0.154.0 `-c mcp_servers.<name>.command=…`
//! / `.args=[…]`; agy 1.2.3 `agy mcp add <name> <cmd> -- <args…>` writing
//! `~/.gemini/config/mcp_config.json`, no per-launch flag.

pub mod install;
pub mod register;
pub mod server;

use std::path::Path;

/// The MCP server name every harness sees.
pub const SERVER_NAME: &str = "agent-mux";

pub const USAGE: &str = "agent-mux mcp <command>

  serve --stdio [--db PATH] [--workspace DIR | --all-workspaces | --workspace-from-env]
                                serve the ten read-only trace tools over stdio
  install agy                   register this binary with `agy mcp add` (no per-launch flag exists)
  uninstall agy                 remove it with `agy mcp remove`
  status [claude|codex|agy]     how each harness reaches the server

Claude Code and Codex are registered per launch by the Agents sidebar; nothing
under ~/.claude or ~/.codex is modified.";

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
    out
}
