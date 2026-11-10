//! The Antigravity and Grok installers and the status command. `agy` keeps
//! its MCP servers in `~/.gemini/config/mcp_config.json`
//! (`{"mcpServers":{"<name>":{"command","args","disabled"}}}`, probed with
//! agy 1.2.3) and edits it through `agy mcp add|remove`; agent-mux calls
//! those rather than editing the file, and reads the file for status.
//!
//! Grok 1.0.70 (probed 2026-11-10) likewise has no per-launch way to add a
//! stdio server to the interactive TUI: `grok --help` has no MCP flag;
//! the `GROK_CONFIG` / `GROK_CONFIG_PATH` overlay is confined to an
//! allowlist (`models`, `features`, part of `toolset`) and drops
//! `mcp_servers`, since it "cannot spawn commands"; `--plugin-dir` exists
//! only on `grok agent stdio` (ACP) and is ignored in leader mode; an
//! `--agent` definition's `mcpServers` frontmatter would replace the
//! session's primary agent. So agent-mux writes one user-level entry with
//! `grok mcp add agent-mux --scope user -- <exe> mcp serve --stdio
//! --workspace-from-env`, which lands in `~/.grok/config.toml` as
//! `[mcp_servers.agent-mux]` `command`, `args`, `enabled` (probed in a
//! temporary `HOME`). The server inherits the launch's environment (probed
//! with `grok mcp doctor`: a variable set on `grok` reached the server), so
//! `AGENT_MUX_WORKSPACE` scopes it, as for agy. A `GROK_HOME` other than
//! `~/.grok` is not followed by the status reader.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The `agy` executable: `AGENT_MUX_AGY_BIN` (tests) or `agy` on PATH.
pub fn agy_bin() -> String {
    std::env::var("AGENT_MUX_AGY_BIN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "agy".to_string())
}

pub fn agy_config_path(home: &Path) -> PathBuf {
    home.join(".gemini").join("config").join("mcp_config.json")
}

/// The server arguments the installed entry carries: the workspace comes
/// from `AGENT_MUX_WORKSPACE` at session time because the entry is global.
pub fn installed_argv() -> Vec<String> {
    vec![
        "mcp".into(),
        "serve".into(),
        "--stdio".into(),
        "--workspace-from-env".into(),
    ]
}

/// The `grok` executable: `AGENT_MUX_GROK_BIN` (tests), `grok` on `PATH`,
/// else the installer's `~/.grok/bin/grok`.
pub fn grok_bin(home: &Path) -> String {
    if let Some(b) = std::env::var("AGENT_MUX_GROK_BIN")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        return b;
    }
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    if std::env::split_paths(&path_var).any(|d| crate::session::is_runnable(&d.join("grok"))) {
        return "grok".into();
    }
    crate::session::install_fallback("grok", home)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "grok".into())
}

pub fn grok_config_path(home: &Path) -> PathBuf {
    home.join(".grok").join("config.toml")
}

/// An installed entry's state, for agy or Grok.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpEntryStatus {
    /// The harness command (`agy`, `grok`), for the advice in `note`.
    pub harness: &'static str,
    pub path: PathBuf,
    pub installed: bool,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub disabled: bool,
    /// The entry points at this binary with the expected arguments.
    pub current: bool,
}

pub type AgyMcpStatus = McpEntryStatus;

impl McpEntryStatus {
    fn empty(harness: &'static str, path: PathBuf) -> Self {
        McpEntryStatus {
            harness,
            path,
            installed: false,
            command: None,
            args: Vec::new(),
            disabled: false,
            current: false,
        }
    }

    fn judge(&mut self, exe: Option<&Path>) {
        self.current = exe.is_some_and(|e| self.command.as_deref() == Some(&*e.to_string_lossy()))
            && self.args == installed_argv();
    }

    /// Installed, enabled and pointing at this binary.
    pub fn usable(&self) -> bool {
        self.installed && !self.disabled
    }

    pub fn note(&self) -> String {
        let h = self.harness;
        if !self.installed {
            return format!("not installed — `agent-mux mcp install {h}`");
        }
        let mut parts = Vec::new();
        if self.disabled {
            parts.push(format!("disabled ({h} mcp enable agent-mux)"));
        }
        if !self.current {
            parts.push(format!(
                "points at {} — rerun `agent-mux mcp install {h}`",
                self.command.as_deref().unwrap_or("?")
            ));
        }
        if parts.is_empty() {
            format!("installed at {}", self.path.display())
        } else {
            parts.join("; ")
        }
    }
}

pub fn agy_status(home: &Path, exe: Option<&Path>) -> AgyMcpStatus {
    let path = agy_config_path(home);
    let mut status = McpEntryStatus::empty("agy", path.clone());
    let Ok(text) = std::fs::read_to_string(&path) else {
        return status;
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
        return status;
    };
    let Some(entry) = doc
        .get("mcpServers")
        .and_then(|s| s.get(super::SERVER_NAME))
    else {
        return status;
    };
    status.installed = true;
    status.command = entry
        .get("command")
        .and_then(|c| c.as_str())
        .map(str::to_string);
    status.args = entry
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    status.disabled = entry
        .get("disabled")
        .and_then(|d| d.as_bool())
        .unwrap_or(false);
    status.judge(exe);
    status
}

/// The `[mcp_servers.agent-mux]` entry of `~/.grok/config.toml`.
pub fn grok_status(home: &Path, exe: Option<&Path>) -> McpEntryStatus {
    let path = grok_config_path(home);
    let mut status = McpEntryStatus::empty("grok", path.clone());
    let Ok(text) = std::fs::read_to_string(&path) else {
        return status;
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&text) else {
        return status;
    };
    let Some(entry) = doc
        .get("mcp_servers")
        .and_then(|s| s.get(super::SERVER_NAME))
    else {
        return status;
    };
    status.installed = true;
    status.command = entry
        .get("command")
        .and_then(|c| c.as_str())
        .map(str::to_string);
    status.args = entry
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    status.disabled = entry.get("enabled").and_then(|d| d.as_bool()) == Some(false);
    status.judge(exe);
    status
}

fn run_agy(args: &[&str]) -> Result<String, String> {
    run_tool(&agy_bin(), args)
}

fn run_tool(bin: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        Ok(if stdout.is_empty() { stderr } else { stdout })
    } else {
        Err(format!(
            "{bin} {} failed: {}",
            args.join(" "),
            if stderr.is_empty() { stdout } else { stderr }
        ))
    }
}

/// `agy mcp add agent-mux <exe> -- mcp serve --stdio --workspace-from-env`.
pub fn install_agy(exe: &Path) -> Result<String, String> {
    let exe = exe.to_string_lossy().into_owned();
    let argv = installed_argv();
    let mut args = vec!["mcp", "add", super::SERVER_NAME, exe.as_str(), "--"];
    args.extend(argv.iter().map(String::as_str));
    run_agy(&args)
}

/// `agy mcp remove agent-mux`.
pub fn uninstall_agy() -> Result<String, String> {
    run_agy(&["mcp", "remove", super::SERVER_NAME])
}

/// The `grok mcp add` arguments for this binary.
pub fn grok_add_args(exe: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["mcp", "add", "--scope", "user", super::SERVER_NAME, "--"]
        .map(String::from)
        .to_vec();
    args.push(exe.to_string_lossy().into_owned());
    args.extend(installed_argv());
    args
}

/// `grok mcp add agent-mux --scope user -- <exe> mcp serve --stdio
/// --workspace-from-env` (an existing entry is updated in place).
pub fn install_grok(exe: &Path, home: &Path) -> Result<String, String> {
    let args = grok_add_args(exe);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_tool(&grok_bin(home), &args)
}

/// `grok mcp remove agent-mux`.
pub fn uninstall_grok(home: &Path) -> Result<String, String> {
    run_tool(&grok_bin(home), &["mcp", "remove", super::SERVER_NAME])
}

/// `agent-mux mcp install|uninstall|status [claude|codex|agy|opencode|grok|pi]`.
pub fn cli(args: &[String]) -> anyhow::Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("status");
    let provider = args.get(1).map(String::as_str).map(|p| match p {
        "antigravity" => "agy",
        other => other,
    });
    let home = crate::skill::install::home_dir();
    let exe = crate::tracing::hooks::register::current_exe();
    match (action, provider) {
        ("install", Some("agy")) => {
            let exe = exe.ok_or_else(|| anyhow::anyhow!("cannot locate this binary"))?;
            let message = install_agy(&exe).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{message}");
            println!(
                "agy loads MCP servers from {} on its next start.",
                agy_config_path(&home).display()
            );
            Ok(())
        }
        ("uninstall", Some("agy")) => {
            let message = uninstall_agy().map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{message}");
            Ok(())
        }
        ("install", Some("grok")) => {
            let exe = exe.ok_or_else(|| anyhow::anyhow!("cannot locate this binary"))?;
            let message = install_grok(&exe, &home).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{message}");
            println!(
                "grok loads MCP servers from {} on its next start.",
                grok_config_path(&home).display()
            );
            Ok(())
        }
        ("uninstall", Some("grok")) => {
            let message = uninstall_grok(&home).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{message}");
            Ok(())
        }
        ("install", _) | ("uninstall", _) => anyhow::bail!(
            "only agy and grok need an installed entry; Claude Code, Codex, OpenCode and Pi are registered per launch\n\n{}",
            super::USAGE
        ),
        ("status", provider) => {
            let all = ["claude", "codex", "agy", "opencode", "grok", "pi"];
            let providers: Vec<&str> = match provider {
                Some(p) if all.contains(&p) => vec![p],
                Some(p) => {
                    anyhow::bail!(
                        "unknown provider {p:?}: claude, codex, agy, opencode, grok or pi"
                    )
                }
                None => all.to_vec(),
            };
            let per_launch = match &exe {
                Some(e) => format!("per launch, every session ({})", e.display()),
                None => "unavailable: the agent-mux binary path is not absolute".to_string(),
            };
            for p in providers {
                match p {
                    "claude" => println!("claude: {per_launch} through --mcp-config"),
                    "codex" => println!("codex: {per_launch} through -c mcp_servers.agent-mux.*"),
                    "opencode" => println!(
                        "opencode: {per_launch} through {} (with --standalone)",
                        crate::opencode::CONFIG_ENV
                    ),
                    "pi" => println!(
                        "pi: {per_launch} through agent-mux's extension (-e {}, {})",
                        crate::pi::extension_path(&home).display(),
                        crate::pi::MCP_ENV
                    ),
                    "grok" => {
                        let st = grok_status(&home, exe.as_deref());
                        println!("grok: {}", st.note());
                    }
                    _ => {
                        let st = agy_status(&home, exe.as_deref());
                        println!("agy: {}", st.note());
                    }
                }
            }
            Ok(())
        }
        (other, _) => anyhow::bail!("unknown mcp command: {other}\n\n{}", super::USAGE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reads_the_agy_file_shape() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        assert!(!agy_status(home, None).installed);
        let path = agy_config_path(home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"mcpServers":{"agent-mux":{"args":["mcp","serve","--stdio","--workspace-from-env"],"command":"/x/agent-mux","disabled":false}}}"#,
        )
        .unwrap();
        let st = agy_status(home, Some(Path::new("/x/agent-mux")));
        assert!(st.installed && st.current && !st.disabled);
        assert!(st.note().starts_with("installed at"));
        let stale = agy_status(home, Some(Path::new("/y/agent-mux")));
        assert!(stale.installed && !stale.current);
        assert!(stale.note().contains("points at /x/agent-mux"));
        assert!(stale.note().contains("mcp install agy"));
    }

    #[test]
    fn status_reads_the_grok_file_shape() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let missing = grok_status(home, None);
        assert!(!missing.installed);
        assert!(missing.note().contains("mcp install grok"));
        let path = grok_config_path(home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // the shape `grok mcp add` writes (probed on 1.0.70)
        std::fs::write(
            &path,
            "[cli]\ninstaller = \"internal\"\n\n[mcp_servers.agent-mux]\ncommand = \"/x/agent-mux\"\nargs = [\"mcp\", \"serve\", \"--stdio\", \"--workspace-from-env\"]\nenabled = true\n",
        )
        .unwrap();
        let st = grok_status(home, Some(Path::new("/x/agent-mux")));
        assert!(st.installed && st.current && st.usable());
        assert!(st.note().starts_with("installed at"));
        std::fs::write(
            &path,
            "[mcp_servers.agent-mux]\ncommand = \"/y/agent-mux\"\nargs = []\nenabled = false\n",
        )
        .unwrap();
        let off = grok_status(home, Some(Path::new("/x/agent-mux")));
        assert!(off.installed && off.disabled && !off.current && !off.usable());
        assert!(off.note().contains("grok mcp enable agent-mux"));
    }

    #[test]
    fn grok_add_puts_the_server_after_the_separator() {
        let args = grok_add_args(Path::new("/opt/a b/agent-mux"));
        assert_eq!(
            args,
            [
                "mcp",
                "add",
                "--scope",
                "user",
                "agent-mux",
                "--",
                "/opt/a b/agent-mux",
                "mcp",
                "serve",
                "--stdio",
                "--workspace-from-env"
            ]
        );
    }
}
