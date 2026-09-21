//! The Antigravity installer and the status command. `agy` keeps its MCP
//! servers in `~/.gemini/config/mcp_config.json`
//! (`{"mcpServers":{"<name>":{"command","args","disabled"}}}`, probed with
//! agy 1.2.3) and edits it through `agy mcp add|remove`; agent-mux calls
//! those rather than editing the file, and reads the file for status.

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgyMcpStatus {
    pub path: PathBuf,
    pub installed: bool,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub disabled: bool,
    /// The entry points at this binary with the expected arguments.
    pub current: bool,
}

impl AgyMcpStatus {
    pub fn note(&self) -> String {
        if !self.installed {
            return "not installed — `agent-mux mcp install agy`".into();
        }
        let mut parts = Vec::new();
        if self.disabled {
            parts.push("disabled (agy mcp enable agent-mux)".to_string());
        }
        if !self.current {
            parts.push(format!(
                "points at {} — rerun `agent-mux mcp install agy`",
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
    let mut status = AgyMcpStatus {
        path: path.clone(),
        installed: false,
        command: None,
        args: Vec::new(),
        disabled: false,
        current: false,
    };
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
    status.current = exe.is_some_and(|e| status.command.as_deref() == Some(&*e.to_string_lossy()))
        && status.args == installed_argv();
    status
}

fn run_agy(args: &[&str]) -> Result<String, String> {
    let bin = agy_bin();
    let out = Command::new(&bin)
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

/// `agent-mux mcp install|uninstall|status [claude|codex|agy]`.
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
        ("install", _) | ("uninstall", _) => anyhow::bail!(
            "only agy needs an installed entry; Claude Code and Codex are registered per launch\n\n{}",
            super::USAGE
        ),
        ("status", provider) => {
            let all = ["claude", "codex", "agy"];
            let providers: Vec<&str> = match provider {
                Some(p) if all.contains(&p) => vec![p],
                Some(p) => anyhow::bail!("unknown provider {p:?}: claude, codex or agy"),
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
    }
}
