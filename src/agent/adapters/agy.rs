use super::HarnessAdapter;
use crate::agent::definition::AgentDefinition;
use crate::harness::Harness;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct AgyAdapter;

impl HarnessAdapter for AgyAdapter {
    fn harness(&self) -> Harness {
        Harness::Antigravity
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

        // 1. agy/agent.md
        let mut agent_md = String::new();
        agent_md.push_str("---\n");
        agent_md.push_str(&format!("name: {}\n", definition.name));
        if !definition.description.is_empty() {
            agent_md.push_str(&format!("description: {}\n", definition.description));
        }
        agent_md.push_str("---\n\n");
        agent_md.push_str(&definition.instructions);
        if !definition.instructions.ends_with('\n') {
            agent_md.push('\n');
        }
        files.insert(PathBuf::from("agy/agent.md"), agent_md.into_bytes());

        // 2. agy/launch.json
        let launch_val = serde_json::json!({
            "agent_id": definition.id,
            "enabled": enabled,
            "startup_task": definition.startup_task,
            "mcp_servers": definition.mcp_servers,
        });
        let launch_json = serde_json::to_string_pretty(&launch_val).unwrap_or_default() + "\n";
        files.insert(PathBuf::from("agy/launch.json"), launch_json.into_bytes());

        // 3. agy/mcp.json
        let mut mcp_servers_map = serde_json::Map::new();
        if definition.mcp_servers.iter().any(|s| s == "agent-mux") {
            let (cmd, args) =
                crate::agent::artifacts::mcp_command(&ctx.executable, &ctx.db, &ctx.workspace);
            mcp_servers_map.insert(
                "agent-mux".to_string(),
                serde_json::json!({
                    "command": cmd.to_string_lossy().to_string(),
                    "args": args
                }),
            );
        }
        let mcp_val = serde_json::json!({
            "mcpServers": mcp_servers_map
        });
        let mcp_json = serde_json::to_string_pretty(&mcp_val).unwrap_or_default() + "\n";
        files.insert(PathBuf::from("agy/mcp.json"), mcp_json.into_bytes());

        files
    }
}
