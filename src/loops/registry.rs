//! The per-user loop registry: `~/.agent-mux/loops.json`
//! (`AGENT_MUX_LOOPS_FILE` overrides, as the sessions file does). The
//! workspace's `LOOP.md` stays a human document; this file is what the
//! scheduler reads and writes.

use crate::loops::{Level, format_timestamp, parse_timestamp};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

/// One scheduled loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopEntry {
    pub id: String,
    pub workspace: PathBuf,
    pub pattern: String,
    /// Profile name the run launches with (empty: the first profile for
    /// the harness).
    #[serde(default)]
    pub profile: String,
    /// `claude` | `codex`.
    pub harness: String,
    /// Model for the run's session (`--model`). Empty leaves the profile's
    /// own model, and then the CLI's default. The pattern may suggest one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Model for the `loop-verifier` sub-agent, written into the agent file
    /// the scaffolder installs. Empty means `inherit`: the verifier runs on
    /// the same model as the run that calls it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub verifier_model: String,
    pub interval_s: u64,
    pub level: Level,
    pub enabled: bool,
    pub max_runs_per_day: u32,
    pub max_tokens_per_day: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd_per_run: Option<f64>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_id: Option<String>,
    /// Set by auto-pause (failure, breaker trip, gate violation); cleared
    /// on resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_reason: Option<String>,
}

impl LoopEntry {
    pub fn next_run(&self) -> Option<OffsetDateTime> {
        self.next_run_at.as_deref().and_then(parse_timestamp)
    }

    pub fn set_next_run(&mut self, t: OffsetDateTime) {
        self.next_run_at = Some(format_timestamp(t));
    }

    /// A short label for the sidebar: the pattern id.
    pub fn label(&self) -> &str {
        &self.pattern
    }

    pub fn workspace_name(&self) -> String {
        self.workspace
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.workspace.to_string_lossy().into_owned())
    }

    pub fn paused(&self) -> bool {
        !self.enabled || self.paused_reason.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
    #[serde(default)]
    pub pause_all: bool,
    #[serde(default)]
    pub loops: Vec<LoopEntry>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry {
            version: 1,
            pause_all: false,
            loops: Vec::new(),
        }
    }
}

impl Registry {
    pub fn find(&self, id: &str) -> Option<&LoopEntry> {
        self.loops.iter().find(|l| l.id == id)
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut LoopEntry> {
        self.loops.iter_mut().find(|l| l.id == id)
    }

    /// Resolves `<id>`, a unique id prefix, or `<pattern>@<workspace dir
    /// name or path>`.
    pub fn resolve(&self, needle: &str) -> Option<&LoopEntry> {
        if let Some(l) = self.find(needle) {
            return Some(l);
        }
        if let Some((pattern, ws)) = needle.split_once('@') {
            return self.loops.iter().find(|l| {
                l.pattern == pattern
                    && (l.workspace_name() == ws || l.workspace.to_string_lossy() == ws)
            });
        }
        let mut by_prefix = self.loops.iter().filter(|l| l.id.starts_with(needle));
        let first = by_prefix.next()?;
        by_prefix.next().is_none().then_some(first)
    }

    pub fn add(&mut self, entry: LoopEntry) {
        self.loops.retain(|l| l.id != entry.id);
        self.loops.push(entry);
    }

    pub fn remove(&mut self, id: &str) -> Option<LoopEntry> {
        let idx = self.loops.iter().position(|l| l.id == id)?;
        Some(self.loops.remove(idx))
    }

    /// Pauses one loop (`p`), keeping any auto-pause reason.
    pub fn pause(&mut self, id: &str) -> bool {
        match self.find_mut(id) {
            Some(l) => {
                l.enabled = false;
                true
            }
            None => false,
        }
    }

    /// Resumes one loop: enabled, auto-pause reason cleared. Returns the
    /// reason that was cleared, if any (the breaker reset needs it).
    pub fn resume(&mut self, id: &str) -> Option<Option<String>> {
        let l = self.find_mut(id)?;
        l.enabled = true;
        Some(l.paused_reason.take())
    }

    /// Loops of one workspace.
    pub fn for_workspace(&self, workspace: &Path) -> Vec<&LoopEntry> {
        self.loops
            .iter()
            .filter(|l| l.workspace == workspace)
            .collect()
    }
}

/// `AGENT_MUX_LOOPS_FILE`, else `~/.agent-mux/loops.json`.
pub fn registry_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AGENT_MUX_LOOPS_FILE") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".agent-mux").join("loops.json"))
}

pub fn load(path: &Path) -> Registry {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Registry::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Atomic write (temp file and rename), like the sessions file.
pub fn save(path: &Path, registry: &Registry) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(registry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let temp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&temp, json)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}

/// A new entry with the pattern's caps and the first run one interval
/// from `now`.
pub fn new_entry(
    workspace: &Path,
    pattern: &crate::loops::Pattern,
    harness: &str,
    profile: &str,
    interval_s: u64,
    level: Level,
    now: OffsetDateTime,
) -> LoopEntry {
    let mut entry = LoopEntry {
        id: uuid::Uuid::new_v4().to_string(),
        workspace: workspace.to_path_buf(),
        pattern: pattern.id.clone(),
        profile: profile.to_string(),
        harness: harness.to_string(),
        model: pattern.model.clone().unwrap_or_default(),
        verifier_model: pattern.verifier_model.clone().unwrap_or_default(),
        interval_s,
        level,
        enabled: true,
        max_runs_per_day: pattern.max_runs_per_day,
        max_tokens_per_day: pattern.max_tokens_per_day,
        max_cost_usd_per_run: None,
        created_at: format_timestamp(now),
        next_run_at: None,
        last_run_id: None,
        paused_reason: None,
    };
    entry.set_next_run(now + time::Duration::seconds(interval_s as i64));
    entry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loops::PatternCost;

    fn pattern() -> crate::loops::Pattern {
        crate::loops::Pattern {
            id: "daily-triage".into(),
            name: "Daily Triage".into(),
            goal: "g".into(),
            default_interval_s: 86_400,
            week_one_level: Level::L1,
            state_file: "STATE.md".into(),
            skills: vec!["loop-triage".into()],
            verifier: false,
            breaker: false,
            human_gates: vec![],
            risk: "low".into(),
            token_cost: "low".into(),
            max_runs_per_day: 2,
            max_tokens_per_day: 100_000,
            priority: 6,
            cost: PatternCost {
                tokens_noop: 5000,
                tokens_report: 50_000,
                tokens_action: 200_000,
                stable_fraction: 0.35,
                early_exit_required: false,
            },
            model: None,
            verifier_model: None,
            prompt: None,
            agents: Vec::new(),
        }
    }

    #[test]
    fn round_trip_resolve_pause_resume() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("loops.json");
        assert_eq!(load(&path), Registry::default());
        let now = parse_timestamp("2026-09-15T09:00:00Z").unwrap();
        let ws = temp.path().join("proj");
        let entry = new_entry(
            &ws,
            &pattern(),
            "claude",
            "Claude Code",
            3600,
            Level::L1,
            now,
        );
        assert_eq!(entry.next_run_at.as_deref(), Some("2026-09-15T10:00:00Z"));
        let mut reg = Registry::default();
        reg.add(entry.clone());
        save(&path, &reg).unwrap();
        let back = load(&path);
        assert_eq!(back, reg);
        assert!(back.resolve(&entry.id[..8]).is_some(), "unique prefix");
        assert!(back.resolve("daily-triage@proj").is_some());
        assert!(back.resolve("nope@proj").is_none());
        let mut reg = back;
        reg.find_mut(&entry.id).unwrap().paused_reason = Some("breaker".into());
        assert!(reg.find(&entry.id).unwrap().paused());
        assert_eq!(reg.resume(&entry.id), Some(Some("breaker".into())));
        assert!(!reg.find(&entry.id).unwrap().paused());
        assert!(reg.pause(&entry.id));
        assert!(reg.find(&entry.id).unwrap().paused());
        assert!(reg.remove(&entry.id).is_some());
        assert!(reg.loops.is_empty());
    }
}
