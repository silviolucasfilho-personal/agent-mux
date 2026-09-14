//! Dynamic agent discovery and loading from `~/.agent-mux/agents/`.
//!
//! Agents are defined as individual Markdown (`.md`) files with optional YAML frontmatter.
//! `agent-mux` automatically scans `~/.agent-mux/agents/` (and `./.agent-mux/agents/`),
//! loading each discovered agent into the sidebar's Agents menu.

pub mod adapters;
pub mod artifacts;
pub mod definition;
pub mod discovery;

pub use artifacts::{render_artifacts, write_artifacts, ArtifactError, ArtifactSet};
pub use crate::harness::Harness as HeimdallHarness;
pub use definition::{
    parse_definition, parse_legacy_definition, AgentDefinition, DefinitionError,
};
pub use discovery::{
    bundled_agents_dir, discover_agents, migrate_legacy, DiscoveryReport, MigrationError,
};
use std::path::{Path, PathBuf};

/// Default built-in markdown instructions for Heimdall (legacy compatibility constant).
pub const DEFAULT_HEIMDALL_MD: &str = r#"---
id: heimdall
name: Heimdall
icon: ⚡
description: Omniscient monitor, executive morning briefings, and skill optimizer
harnesses: [claude, codex, agy]
default_harness: agy
---

# Agent: Heimdall (The Omniscient Watcher)

You are Heimdall, the omniscient watcher and autonomous monitoring agent of `agent-mux`.
Everything you inspect is backed by the local agent-mux SQLite store located at `~/.agent-mux/traces.db`.

## Primary Missions

### 1. Executive Morning Briefing
When a user returns to their workstation after sessions have run overnight (or while away), deliver a clear, high-signal briefing for each active or historical session:
- **Initial Goal**: What the user requested in turn 1 (cleaned of harness/system preambles).
- **Right Now (In-flight Clue)**: The active tool currently executing (with target file or command and elapsed time), or the prompt/step currently in progress.
- **Work Accomplished**: Turns completed, total tool calls broken down by tool type, files modified, and recent shell commands executed.
- **Last Assistant Output**: A clean snippet of the latest response or summary delivered by the assistant.
- **Timeline & Resources**: Duration, when last active, tokens consumed, and USD cost.

### 2. Skills Performance & Token Optimization
Inspect SQLite metadata to detect:
- Unused skill loads (skills loaded into context that were never invoked, wasting tokens).
- Slow tool executions (>4000ms latency).
- LLM generation bottlenecks (high TTFT).
- Recurring tool errors.

### 3. Interactive Investigation
Directly query SQLite (`~/.agent-mux/traces.db`) using sqlite3 commands whenever the user asks to drill down into specific turns, logs, or observations.
"#;


/// Fallback location for the global agents directory (`~/.agent-mux/agents`).
pub fn default_agents_dir() -> PathBuf {
    if let Ok(p) = std::env::var("AGENT_MUX_AGENTS_DIR") {
        if !p.trim().is_empty() {
            return PathBuf::from(p.trim());
        }
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".agent-mux").join("agents"))
        .unwrap_or_else(|| PathBuf::from("agents"))
}

/// Seeds the default Heimdall agent file into the target directory if missing.
pub fn seed_default_agents(dir: &Path) {
    if !dir.exists() {
        let _ = std::fs::create_dir_all(dir);
    }
    let heimdall_file = dir.join("heimdall.md");
    if !heimdall_file.exists() {
        let _ = std::fs::write(&heimdall_file, DEFAULT_HEIMDALL_MD);
    }
}

/// Discovers and loads all agents from workspace and global directories.
pub fn load_agents(custom_dir: Option<&Path>) -> Vec<AgentDefinition> {
    let global_dir = custom_dir.map(Path::to_path_buf).unwrap_or_else(default_agents_dir);
    seed_default_agents(&global_dir);

    let ws_dir = std::env::current_dir()
        .map(|cwd| cwd.join(".agent-mux").join("agents"))
        .unwrap_or_else(|_| PathBuf::from(".agent-mux/agents"));

    let bundled = bundled_agents_dir();
    let report = discover_agents(&ws_dir, &global_dir, bundled.as_deref());
    let mut agents = report.agents;

    // Guarantee built-in Heimdall if not found on disk or bundled
    if !agents.iter().any(|a| a.id == "heimdall") {
        let mut builtin = AgentDefinition::default();
        builtin.instructions = DEFAULT_HEIMDALL_MD.to_string();
        agents.insert(0, builtin);
    }

    // Sort: Heimdall first, then alphabetically by name
    agents.sort_by(|a, b| {
        if a.id == "heimdall" {
            std::cmp::Ordering::Less
        } else if b.id == "heimdall" {
            std::cmp::Ordering::Greater
        } else {
            a.name.cmp(&b.name)
        }
    });

    agents
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_agent_markdown_with_frontmatter() {
        let content = r#"---
id: code-reviewer
name: Senior Reviewer
icon: 🔍
description: Audits code diffs and enforces style
harnesses: [claude, codex]
default_harness: claude
---

# Code Reviewer Instructions
Review all code changes thoroughly.
"#;
        let agent = AgentDefinition::parse_markdown(content, Some(Path::new("/tmp/reviewer.md")));
        assert_eq!(agent.id, "code-reviewer");
        assert_eq!(agent.name, "Senior Reviewer");
        assert_eq!(agent.icon.as_deref(), Some("🔍"));
        assert_eq!(agent.description, "Audits code diffs and enforces style");
        assert_eq!(agent.harnesses, vec![HeimdallHarness::Claude, HeimdallHarness::Codex]);
        assert_eq!(agent.default_harness, HeimdallHarness::Claude);
        assert!(agent.instructions.contains("Review all code changes thoroughly"));
    }

    #[test]
    fn test_parse_agent_markdown_without_frontmatter() {
        let content = "# Tester\nAutomated testing agent.";
        let agent = AgentDefinition::parse_markdown(content, Some(Path::new("/tmp/auto-tester.md")));
        assert_eq!(agent.id, "auto-tester");
        assert_eq!(agent.name, "Auto Tester");
        assert_eq!(agent.description, "Automated testing agent.");
    }

    #[test]
    fn test_load_and_seed_agents() {
        let temp = TempDir::new().unwrap();
        let agents_dir = temp.path().join("agents");

        // First load should seed heimdall.md
        let agents = load_agents(Some(&agents_dir));
        assert!(!agents.is_empty());
        assert_eq!(agents[0].id, "heimdall");
        assert!(agents_dir.join("heimdall.md").exists());

        // Add a second agent
        let reviewer_content = r#"---
id: reviewer
name: Code Reviewer
icon: 🔍
description: Reviews pull requests
---
Review instructions.
"#;
        std::fs::write(agents_dir.join("reviewer.md"), reviewer_content).unwrap();

        let agents_updated = load_agents(Some(&agents_dir));
        assert_eq!(agents_updated.len(), 2);
        assert_eq!(agents_updated[0].id, "heimdall");
        assert_eq!(agents_updated[1].id, "reviewer");
        assert_eq!(agents_updated[1].name, "Code Reviewer");
        assert_eq!(agents_updated[1].icon.as_deref(), Some("🔍"));
    }
}
