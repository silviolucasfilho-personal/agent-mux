//! Dynamic agent discovery and loading from `~/.agent-mux/agents/`.
//!
//! Agents are defined as individual Markdown (`.md`) files with optional YAML frontmatter.
//! `agent-mux` automatically scans `~/.agent-mux/agents/` (and `./.agent-mux/agents/`),
//! loading each discovered agent into the sidebar's Agents menu.

pub mod adapters;
pub mod artifacts;
pub mod budgets;
pub mod cli;
pub mod definition;
pub mod discovery;
pub mod install;
pub mod launch;
pub mod managed;
pub mod state;
pub mod triggers;

pub use crate::harness::Harness;
pub use artifacts::{ArtifactError, ArtifactSet, render_artifacts, write_artifacts};
pub use budgets::{Budget, BudgetError, ManagedCapabilities, validate_budget};
pub use definition::{AgentDefinition, DefinitionError, parse_definition, parse_legacy_definition};
pub use discovery::{
    DiscoveryReport, MigrationError, bundled_agents_dir, discover_agents, migrate_legacy,
};
pub use install::{
    DoctorError, DoctorReport, DoctorStatus, InstallError, InstallReport, UninstallReport,
    doctor_agent, install_agent, uninstall_agent,
};
pub use launch::{
    AgentLaunch, LaunchError, LaunchOptions, build_agent_launch, selected_harness_index,
};
pub use managed::{
    AgyManagedAdapter, ClaudeManagedAdapter, CodexManagedAdapter, ManagedAdapter, ManagedError,
    ManagedEvent, ManagedRequest, ManagedSession, ScriptedManagedAdapter,
};
pub use state::{
    AgentScope, BriefingRecord, Finding, FindingStatus, Job, JobStatus, StateError, StateStore,
};
use std::path::{Path, PathBuf};
pub use triggers::{
    EventKind, JobRequest, MonitoringConfig, TriggerDefinition, TriggerEvent, evaluate_changes,
    should_trigger,
};

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

/// Discovers and loads all agents from workspace and global directories.
pub fn load_agents(custom_dir: Option<&Path>) -> Vec<AgentDefinition> {
    let global_dir = custom_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(default_agents_dir);
    let ws_dir = std::env::current_dir()
        .map(|cwd| cwd.join(".agent-mux").join("agents"))
        .unwrap_or_else(|_| PathBuf::from(".agent-mux/agents"));

    let bundled = bundled_agents_dir();
    let report = discover_agents(&ws_dir, &global_dir, bundled.as_deref());
    let mut agents = report.agents;

    // Sort alphabetically by name
    agents.sort_by(|a, b| a.name.cmp(&b.name));

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
        assert_eq!(agent.harnesses, vec![Harness::Claude, Harness::Codex]);
        assert_eq!(agent.default_harness, Harness::Claude);
        assert!(
            agent
                .instructions
                .contains("Review all code changes thoroughly")
        );
    }

    #[test]
    fn test_parse_agent_markdown_without_frontmatter() {
        let content = "# Tester\nAutomated testing agent.";
        let agent =
            AgentDefinition::parse_markdown(content, Some(Path::new("/tmp/auto-tester.md")));
        assert_eq!(agent.id, "auto-tester");
        assert_eq!(agent.name, "Auto Tester");
        assert_eq!(agent.description, "Automated testing agent.");
    }

    #[test]
    fn test_load_agents() {
        let temp = TempDir::new().unwrap();
        let agents_dir = temp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();

        // Add an agent
        let reviewer_content = r#"---
id: reviewer
name: Code Reviewer
icon: 🔍
description: Reviews pull requests
---
Review instructions.
"#;
        std::fs::write(agents_dir.join("reviewer.md"), reviewer_content).unwrap();

        let agents = load_agents(Some(&agents_dir));
        let reviewer = agents
            .iter()
            .find(|a| a.id == "reviewer")
            .expect("reviewer found");
        assert_eq!(reviewer.name, "Code Reviewer");
        assert_eq!(reviewer.icon.as_deref(), Some("🔍"));
    }
}
