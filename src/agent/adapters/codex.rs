use super::HarnessAdapter;
use crate::agent::definition::AgentDefinition;
use crate::harness::Harness;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct CodexAdapter;

impl HarnessAdapter for CodexAdapter {
    fn harness(&self) -> Harness {
        Harness::Codex
    }

    fn version(&self) -> u32 {
        1
    }

    fn render(
        &self,
        definition: &AgentDefinition,
        enabled: bool,
        ctx: &crate::agent::artifacts::RenderContext,
    ) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();

        // 1. codex/AGENTS.md
        let mut agents_md = String::new();
        agents_md.push_str(&format!("# {}\n\n", definition.name));
        agents_md.push_str(&definition.instructions);
        if !definition.instructions.ends_with('\n') {
            agents_md.push('\n');
        }
        files.insert(PathBuf::from("codex/AGENTS.md"), agents_md.into_bytes());

        // 2. codex/launch.json
        let launch_val = serde_json::json!({
            "agent_id": definition.id,
            "enabled": enabled,
            "startup_task": definition.startup_task,
            "mcp_servers": definition.mcp_servers,
        });
        let launch_json = serde_json::to_string_pretty(&launch_val).unwrap_or_default() + "\n";
        files.insert(PathBuf::from("codex/launch.json"), launch_json.into_bytes());

        // 3. codex/mcp.toml
        let mut mcp_toml = String::new();
        if definition.mcp_servers.iter().any(|s| s == "agent-mux") {
            let (cmd, args) =
                crate::agent::artifacts::mcp_command(&ctx.executable, &ctx.db, &ctx.workspace);
            mcp_toml.push_str("[mcp_servers.agent-mux]\n");
            mcp_toml.push_str(&format!("command = \"{}\"\n", cmd.to_string_lossy()));
            let formatted_args = args
                .iter()
                .map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")))
                .collect::<Vec<_>>()
                .join(", ");
            mcp_toml.push_str(&format!("args = [{}]\n", formatted_args));
        } else {
            mcp_toml.push_str("[mcp_servers]\n");
        }
        files.insert(PathBuf::from("codex/mcp.toml"), mcp_toml.into_bytes());

        files
    }
}
