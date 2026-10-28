//! Workspace Explorer (`Mode::Workspaces`, `agent-mux workspace …`): scans a
//! workspace directory tree, detects the technology stack, documentation lead
//! and git state of each folder and sub-folder, aggregates agent activity
//! (live sessions, resumable past sessions, scheduled loops and project-local
//! personas) directly and across sub-folders, and lets the user launch or
//! resume an agent in any folder.

pub mod activity;
pub mod cli;
pub mod scan;
pub mod summary_cache;

use crate::harness::Harness;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default scan depth below the current workspace root.
pub const SCAN_DEPTH: usize = 3;

/// Maximum directories visited in one scan so `/` or `~` stays responsive.
pub const SCAN_MAX_VISITED: usize = 4_000;

/// Detected languages, frameworks, libraries and build/infra tools in a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TechStack {
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub tools: Vec<String>,
    pub package_name: Option<String>,
    pub edition_or_version: Option<String>,
    pub manifest_description: Option<String>,
    pub workspace_members: Vec<String>,
}

impl TechStack {
    pub fn is_empty(&self) -> bool {
        self.languages.is_empty() && self.frameworks.is_empty() && self.tools.is_empty()
    }

    /// Short badge for the left tree pane (`Rust · TUI`, `TS · Next`, `Python`).
    pub fn short_badge(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for l in self.languages.iter().take(2) {
            parts.push(match l.as_str() {
                "TypeScript" => "TS",
                "JavaScript" => "JS",
                "Python" => "Py",
                other => other,
            });
        }
        if parts.len() < 2
            && let Some(fw) = self.frameworks.first()
        {
            parts.push(fw.as_str());
        }
        if parts.is_empty()
            && let Some(t) = self.tools.first()
        {
            parts.push(t.as_str());
        }
        parts.join(" · ")
    }

    /// Full one-line description for the right dossier pane.
    pub fn detail_line(&self) -> String {
        let mut items: Vec<String> = Vec::new();
        if let Some(first_lang) = self.languages.first() {
            match &self.edition_or_version {
                Some(ver) if !ver.is_empty() => items.push(format!("{first_lang} {ver}")),
                _ => items.push(first_lang.clone()),
            }
            for lang in self.languages.iter().skip(1) {
                items.push(lang.clone());
            }
        }
        for tool in &self.tools {
            if !items.iter().any(|x| x.eq_ignore_ascii_case(tool)) {
                items.push(tool.clone());
            }
        }
        for fw in &self.frameworks {
            if !items.iter().any(|x| x.eq_ignore_ascii_case(fw)) {
                items.push(fw.clone());
            }
        }
        if items.is_empty() {
            "—".to_string()
        } else {
            items.join(" · ")
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitKind {
    #[default]
    None,
    RepoRoot,
    Worktree,
    Subfolder,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitInfo {
    pub kind: GitKind,
    pub branch: Option<String>,
    pub dirty_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPersona {
    pub name: String,
    pub description: String,
    pub task_label: Option<String>,
}

/// Agent and harness configuration files present in a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomizationInfo {
    pub has_agents_md: bool,
    pub has_claude_md: bool,
    pub has_readme_md: bool,
    pub primary_doc: Option<PathBuf>,
    pub project_personas: Vec<ProjectPersona>,
    pub harness_dirs: Vec<String>,
    pub has_gate_yaml: bool,
    pub has_loop_md: bool,
}

impl CustomizationInfo {
    pub fn has_any(&self) -> bool {
        self.has_agents_md
            || self.has_claude_md
            || !self.project_personas.is_empty()
            || !self.harness_dirs.is_empty()
            || self.has_gate_yaml
            || self.has_loop_md
    }

    pub fn summary_badges(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.has_agents_md {
            out.push("AGENTS.md".into());
        }
        if self.has_claude_md {
            out.push("CLAUDE.md".into());
        }
        if !self.project_personas.is_empty() {
            out.push(format!(
                "{} project agent{}",
                self.project_personas.len(),
                if self.project_personas.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ));
        }
        for d in &self.harness_dirs {
            out.push(d.clone());
        }
        if self.has_gate_yaml {
            out.push("gate.yaml".into());
        }
        if self.has_loop_md {
            out.push("loop.md".into());
        }
        if out.is_empty() && self.has_readme_md {
            out.push("README.md".into());
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SummarySource {
    CachedLlm {
        harness: String,
        generated_at: String,
        stale: bool,
    },
    Manifest,
    DocLead {
        file: String,
    },
    Synthesized,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub text: String,
    pub source: SummarySource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry_points: Vec<String>,
}

/// An active or idle session currently open in `App::sessions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSessionItem {
    pub session_idx: usize,
    pub session_id: usize,
    pub profile_name: String,
    pub harness: Option<Harness>,
    pub status_label: String,
    pub is_running: bool,
    pub dir: PathBuf,
    pub rel_subpath: Option<String>,
}

/// A scheduled loop targeting this folder or one of its sub-folders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledAgentItem {
    pub loop_id: String,
    pub agent_name: Option<String>,
    pub pattern: String,
    pub harness: String,
    pub interval_s: u64,
    pub paused: bool,
    pub last_outcome: Option<String>,
    pub last_run_at: Option<String>,
    pub workspace: PathBuf,
    pub rel_subpath: Option<String>,
}

/// A resumable past session from `traces.db` or on-disk transcripts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentSessionItem {
    pub session_id: String,
    pub provider: String,
    pub title: String,
    pub last_seen_ns: i64,
    pub age_label: String,
    pub turn_count: i64,
    pub total_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
    pub cwd: PathBuf,
    pub rel_subpath: Option<String>,
}

/// Roll-up telemetry for a folder and its sub-folders over the last 30 days.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RollupStats {
    pub direct_sessions_30d: usize,
    pub subtree_sessions_30d: usize,
    pub total_tokens_30d: i64,
    pub total_cost_usd_30d: f64,
    pub last_active_ns: Option<i64>,
    pub by_harness: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FolderActivity {
    pub live_sessions: Vec<LiveSessionItem>,
    pub scheduled_agents: Vec<ScheduledAgentItem>,
    pub recent_sessions: Vec<RecentSessionItem>,
    pub stats: RollupStats,
}

impl FolderActivity {
    /// The newest resumable session in this folder (preferring a direct
    /// match in the folder itself, then the newest in a sub-folder).
    pub fn latest_resumable(&self) -> Option<&RecentSessionItem> {
        self.recent_sessions
            .iter()
            .find(|s| s.rel_subpath.is_none())
            .or_else(|| self.recent_sessions.first())
    }
}

/// One row in the Workspace Explorer tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceNode {
    pub path: PathBuf,
    pub name: String,
    pub rel_path: String,
    pub depth: usize,
    pub is_project: bool,
    pub child_project_count: usize,
    pub stack: TechStack,
    pub git: GitInfo,
    pub customization: CustomizationInfo,
    pub summary: ProjectSummary,
    pub activity: FolderActivity,
}

impl WorkspaceNode {
    /// Relative sub-path from `self.path` to `child`, or `None` if `child == self.path`.
    pub fn relative_subpath(&self, child: &Path) -> Option<String> {
        let rel = child.strip_prefix(&self.path).ok()?;
        if rel.as_os_str().is_empty() {
            None
        } else {
            Some(rel.to_string_lossy().into_owned())
        }
    }
}
