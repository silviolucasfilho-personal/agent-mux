//! How a session becomes an agent on each harness. Probed on 2026-09-25:
//!
//! - Claude Code 2.1.282: `--agents '<json>' --agent <name>` in print mode
//!   runs the main session as that agent: its `prompt` is the system
//!   prompt and its `tools` the only tools, and a `/skill` in the prompt
//!   still loads with `tools = ["Read"]`. No file is written. The model
//!   goes through `--model` like any step's, not the JSON, so the step's
//!   own model still wins.
//! - Codex CLI 0.155.1: `codex exec -c developer_instructions="…"` applies
//!   to the main session (a probe answered from them), and `-s read-only`
//!   runs it read-only. Codex has no per-tool list: an agent without
//!   `edit` gets `-s read-only` in place of `--yolo`; the rest of its tool
//!   list is advisory (`notes`).
//! - Antigravity: `--agent <name>` runs an agent file from
//!   `~/.gemini/config/agents/<dir>/agent.md` (frontmatter `name`,
//!   `mainAgent: true`, body under `# System Prompt`; probed on agy 1.2.2,
//!   not re-probed on 1.2.10). An unknown name is ignored silently, so the
//!   file is written before the run. Tool lists are not written: a
//!   misspelled agy tool name can hang the session.
//! - OpenCode 2.0.32 (probed 2026-10-31): an agent defined inline in
//!   `OPENCODE_CONFIG_CONTENT` (`agent.<name>` with `description`,
//!   `mode: "primary"`, `prompt` and a `tools` map of booleans) is run by
//!   `opencode run --agent <name>`, or by `default_agent` in the same
//!   document for the interactive command, which rejects `--agent`
//!   (`crate::opencode::adapt_launch` makes that move). Both the `tools`
//!   map and a `permission` map took the shell away from a probe agent;
//!   the `tools` map is written. No file is written. The document counts
//!   only with `--standalone`, which the spawn step adds.

use super::{AgentSpec, Tool};
use crate::harness::Harness;
use std::path::{Path, PathBuf};

/// A note's weight: a warning is shown; an error stops the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Warning,
    Error,
}

/// What an agent adds to a session's command line on one harness.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LaunchPlan {
    /// Inserted before the prompt, with the workflow's own arguments.
    pub args: Vec<String>,
    /// Set in the session's environment (OpenCode's inline agent).
    pub env: Vec<(String, String)>,
    /// Arguments of the composed command line this plan replaces.
    pub remove: Vec<String>,
    /// The agent's model and effort on this harness; a step's own win.
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Claude Code's tool names for a canonical tool.
pub fn claude_tools(tool: &Tool) -> Vec<String> {
    match tool {
        Tool::Read => vec!["Read".into(), "Glob".into(), "Grep".into()],
        Tool::Edit => vec!["Edit".into(), "Write".into(), "NotebookEdit".into()],
        Tool::Shell => vec!["Bash".into()],
        Tool::Web => vec!["WebFetch".into(), "WebSearch".into()],
        Tool::Mcp(server) => vec![format!("mcp__{server}")],
    }
}

/// The name an agent carries on Antigravity and OpenCode: prefixed, so it
/// never collides with an agent of the user's own.
pub fn agy_name(spec: &AgentSpec) -> String {
    format!("agent-mux-{}", spec.name)
}

/// OpenCode's tool names for a canonical tool, as its `tools` map keys
/// them (from its documentation; `bash` was probed).
pub fn opencode_tools(tool: &Tool) -> Vec<&'static str> {
    match tool {
        Tool::Read => vec!["read", "glob", "grep", "list"],
        Tool::Edit => vec!["edit", "write", "patch"],
        Tool::Shell => vec!["bash"],
        Tool::Web => vec!["webfetch", "websearch"],
        Tool::Mcp(_) => vec![],
    }
}

/// The `OPENCODE_CONFIG_CONTENT` fragment defining the agent. A tool
/// list becomes a `tools` map that switches off every canonical tool the
/// agent lacks; MCP tools are not mapped (`tool_notes`).
pub fn opencode_fragment(spec: &AgentSpec) -> serde_json::Value {
    let mut def = serde_json::Map::new();
    def.insert(
        "description".into(),
        spec.description.replace('\n', " ").into(),
    );
    def.insert("mode".into(), "primary".into());
    def.insert("prompt".into(), spec.instructions.clone().into());
    if let Some(tools) = &spec.tools {
        let mut map = serde_json::Map::new();
        for canonical in [Tool::Read, Tool::Edit, Tool::Shell, Tool::Web] {
            let on = tools.contains(&canonical);
            for name in opencode_tools(&canonical) {
                map.insert(name.into(), on.into());
            }
        }
        def.insert("tools".into(), serde_json::Value::Object(map));
    }
    serde_json::json!({ "agent": { agy_name(spec): def } })
}

/// `~/.gemini/config/agents/agent-mux-<name>/`.
pub fn agy_dir(home: &Path, spec: &AgentSpec) -> PathBuf {
    home.join(".gemini")
        .join("config")
        .join("agents")
        .join(agy_name(spec))
}

const MARKER: &str = ".agent-mux.json";

/// The Antigravity agent file.
pub fn agy_file(spec: &AgentSpec) -> String {
    let description = spec.description.replace('\n', " ");
    format!(
        "---\nname: {}\ndescription: {}\nmainAgent: true\n---\n\n# System Prompt\n\n{}\n",
        agy_name(spec),
        serde_json::to_string(&description).unwrap_or_default(),
        spec.instructions
    )
}

/// Writes the Antigravity agent file when it is missing or stale. A
/// directory agent-mux did not write is left alone and is an error.
pub fn install_agy(spec: &AgentSpec, home: &Path) -> Result<PathBuf, String> {
    let dir = agy_dir(home, spec);
    let file = dir.join("agent.md");
    if dir.exists() && !dir.join(MARKER).exists() {
        return Err(format!(
            "{} exists and was not written by agent-mux; rename it or remove it",
            dir.display()
        ));
    }
    let text = agy_file(spec);
    if std::fs::read_to_string(&file).ok().as_deref() == Some(text.as_str()) {
        return Ok(file);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(&file, &text).map_err(|e| format!("{}: {e}", file.display()))?;
    let marker = serde_json::json!({ "agent": spec.name, "hash": spec.hash });
    std::fs::write(dir.join(MARKER), marker.to_string())
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(file)
}

/// What an agent's tool list becomes on `harness`, where it cannot be
/// enforced exactly: advisory lines for `workflow check` and run notes.
pub fn tool_notes(spec: &AgentSpec, harness: Harness) -> Vec<(Level, String)> {
    let Some(tools) = &spec.tools else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match harness {
        Harness::Claude => {}
        Harness::Codex => {
            if !tools.contains(&Tool::Shell) {
                out.push((
                    Level::Warning,
                    "codex always has a shell; without `shell` the agent is only asked not to use it"
                        .to_string(),
                ));
            }
            if tools.contains(&Tool::Web) {
                out.push((
                    Level::Warning,
                    "codex: `web` is not mapped; the session has whatever web access ~/.codex/config.toml enables".to_string(),
                ));
            }
            for t in tools {
                if let Tool::Mcp(s) = t {
                    out.push((
                        Level::Warning,
                        format!("codex: mcp:{s} must be configured in ~/.codex/config.toml ([mcp_servers.{s}])"),
                    ));
                }
            }
        }
        Harness::Antigravity => {
            out.push((
                Level::Warning,
                "agy: the tool list is not enforced (agy tool names are unverified); the session has agy's own tools".to_string(),
            ));
        }
        Harness::OpenCode => {
            for t in tools {
                if let Tool::Mcp(s) = t {
                    out.push((
                        Level::Warning,
                        format!("opencode: mcp:{s} is not mapped; the session has the MCP servers opencode.json configures"),
                    ));
                }
            }
        }
    }
    out
}

/// The command-line changes that make a session `spec` on `harness`.
pub fn plan(spec: &AgentSpec, harness: Harness) -> LaunchPlan {
    let h = harness.as_str();
    let mut out = LaunchPlan {
        model: spec.model_for(h),
        effort: spec.effort_for(h),
        ..Default::default()
    };
    match harness {
        Harness::Claude => {
            let mut def = serde_json::Map::new();
            def.insert("description".into(), spec.description.clone().into());
            def.insert("prompt".into(), spec.instructions.clone().into());
            if let Some(tools) = &spec.tools {
                let names: Vec<String> = tools.iter().flat_map(claude_tools).collect();
                def.insert("tools".into(), names.into());
            }
            let mut agents = serde_json::Map::new();
            agents.insert(spec.name.clone(), serde_json::Value::Object(def));
            out.args.push("--agents".into());
            out.args.push(serde_json::Value::Object(agents).to_string());
            out.args.push("--agent".into());
            out.args.push(spec.name.clone());
        }
        Harness::Codex => {
            out.args.push("-c".into());
            out.args.push(format!(
                "developer_instructions={}",
                toml::Value::String(spec.instructions.clone())
            ));
            if !spec.can(&Tool::Edit) {
                out.args.push("-s".into());
                out.args.push("read-only".into());
                out.remove.push("--yolo".into());
                out.remove
                    .push("--dangerously-bypass-approvals-and-sandbox".into());
            }
        }
        Harness::Antigravity => {
            out.args.push("--agent".into());
            out.args.push(agy_name(spec));
        }
        Harness::OpenCode => {
            out.args.push("--agent".into());
            out.args.push(agy_name(spec));
            out.env
                .push(crate::opencode::config_env(&opencode_fragment(spec)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(tools: &str) -> AgentSpec {
        AgentSpec::parse(&format!(
            "name = \"reviewer\"\ndescription = \"Reviews\"\ninstructions = \"Say \\\"hi\\\".\\nThen stop.\"\n{tools}\nmodel = \"m\"\n[backends.codex]\nmodel = \"gpt-5\"\neffort = \"high\""
        ))
        .unwrap()
    }

    #[test]
    fn claude_defines_and_selects_the_agent() {
        let p = plan(&spec("tools = [\"read\", \"mcp:github\"]"), Harness::Claude);
        assert_eq!(p.args[0], "--agents");
        let v: serde_json::Value = serde_json::from_str(&p.args[1]).unwrap();
        assert_eq!(v["reviewer"]["prompt"], "Say \"hi\".\nThen stop.");
        assert_eq!(
            v["reviewer"]["tools"],
            serde_json::json!(["Read", "Glob", "Grep", "mcp__github"])
        );
        assert_eq!(&p.args[2..], ["--agent", "reviewer"]);
        assert_eq!(p.model.as_deref(), Some("m"));
        assert!(p.remove.is_empty());
        // no tool list: the harness's own
        let all = plan(&spec(""), Harness::Claude);
        let v: serde_json::Value = serde_json::from_str(&all.args[1]).unwrap();
        assert!(v["reviewer"].get("tools").is_none());
    }

    #[test]
    fn codex_gets_developer_instructions_and_a_sandbox() {
        let p = plan(&spec("tools = [\"read\", \"shell\"]"), Harness::Codex);
        assert_eq!(p.args[0], "-c");
        let (key, value) = p.args[1].split_once('=').unwrap();
        assert_eq!(key, "developer_instructions");
        let parsed: toml::Value = toml::from_str(&format!("v = {value}")).unwrap();
        assert_eq!(parsed["v"].as_str(), Some("Say \"hi\".\nThen stop."));
        assert_eq!(&p.args[2..], ["-s", "read-only"]);
        assert!(p.remove.contains(&"--yolo".to_string()));
        assert_eq!(p.model.as_deref(), Some("gpt-5"));
        assert_eq!(p.effort.as_deref(), Some("high"));
        let editor = plan(&spec("tools = [\"read\", \"edit\"]"), Harness::Codex);
        assert_eq!(editor.args.len(), 2);
        assert!(editor.remove.is_empty());
    }

    #[test]
    fn agy_selects_the_installed_file() {
        let s = spec("tools = [\"read\"]");
        let p = plan(&s, Harness::Antigravity);
        assert_eq!(p.args, ["--agent", "agent-mux-reviewer"]);
        let home = tempfile::tempdir().unwrap();
        let file = install_agy(&s, home.path()).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("---\nname: agent-mux-reviewer\n"));
        assert!(text.contains("mainAgent: true\n---\n\n# System Prompt\n\nSay"));
        // idempotent, and a directory of the user's own is left alone
        install_agy(&s, home.path()).unwrap();
        std::fs::remove_file(agy_dir(home.path(), &s).join(MARKER)).unwrap();
        assert!(
            install_agy(&s, home.path())
                .unwrap_err()
                .contains("not written by agent-mux")
        );
    }

    #[test]
    fn opencode_defines_the_agent_inline_and_selects_it() {
        let s = spec("tools = [\"read\", \"shell\"]");
        let p = plan(&s, Harness::OpenCode);
        assert_eq!(p.args, ["--agent", "agent-mux-reviewer"]);
        assert_eq!(p.env.len(), 1);
        assert_eq!(p.env[0].0, crate::opencode::CONFIG_ENV);
        let v: serde_json::Value = serde_json::from_str(&p.env[0].1).unwrap();
        let a = &v["agent"]["agent-mux-reviewer"];
        assert_eq!(a["mode"], "primary");
        assert_eq!(a["prompt"], "Say \"hi\".\nThen stop.");
        assert_eq!(a["tools"]["bash"], true);
        assert_eq!(a["tools"]["read"], true);
        assert_eq!(a["tools"]["edit"], false);
        assert_eq!(a["tools"]["webfetch"], false);
        assert!(p.remove.is_empty());
        // no tool list: no map, the harness's own tools
        let all = plan(&spec(""), Harness::OpenCode);
        let v: serde_json::Value = serde_json::from_str(&all.env[0].1).unwrap();
        assert!(v["agent"]["agent-mux-reviewer"].get("tools").is_none());
    }

    #[test]
    fn notes_say_what_is_not_enforced() {
        let s = spec("tools = [\"read\", \"web\", \"mcp:gh\"]");
        assert!(tool_notes(&s, Harness::Claude).is_empty());
        let codex = tool_notes(&s, Harness::Codex);
        assert_eq!(codex.len(), 3);
        assert_eq!(tool_notes(&s, Harness::Antigravity).len(), 1);
        assert!(tool_notes(&spec(""), Harness::Antigravity).is_empty());
        assert_eq!(tool_notes(&s, Harness::OpenCode).len(), 1);
        assert!(tool_notes(&spec("tools = [\"read\"]"), Harness::OpenCode).is_empty());
    }
}
