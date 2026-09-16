//! The loop context snapshot: `<runtime>/loops/<run_id>.json`, the facts a
//! run reads on turn one (`$AGENT_MUX_LOOP_CONTEXT`). Built by the App
//! from the store and the workspace files; written before launch; swept
//! with the briefings after 24 h.

use crate::loops::Level;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunInfo {
    pub id: String,
    pub pattern: String,
    pub level_configured: Level,
    pub level_effective: Level,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
    pub base: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Files {
    pub state: String,
    pub run_log: String,
    pub constraints: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<String>,
    pub gate: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Budget {
    pub runs_today: i64,
    pub max_runs_per_day: u32,
    pub tokens_today: i64,
    pub max_tokens_per_day: u64,
    pub percent: u32,
    /// `normal` | `report-only`
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Breaker {
    pub applicable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub iterations: usize,
    #[serde(default)]
    pub consecutive_failures: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Gate {
    pub denylist: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_files: Option<u32>,
    #[serde(default)]
    pub auto_merge_allowlist: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Readiness {
    pub score: u32,
    pub level: String,
    #[serde(default)]
    pub findings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RunSummary {
    pub id: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items_found: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions_taken: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalations: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ContextDoc {
    pub schema_version: u32,
    pub as_of: String,
    pub run: RunInfo,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeInfo>,
    pub files: Files,
    pub budget: Budget,
    pub breaker: Breaker,
    pub gate: Gate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<Readiness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_run: Option<RunSummary>,
    #[serde(default)]
    pub recent_runs: Vec<RunSummary>,
    pub inbox_waiting: usize,
    pub kill_switch: bool,
    /// The human gates of the pattern, for the skill's escalation rule.
    #[serde(default)]
    pub human_gates: Vec<String>,
}

pub fn contexts_dir(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("loops")
}

/// The snapshot path for a run id (`:` becomes `-`).
pub fn context_path(runtime_dir: &Path, run_id: &str) -> PathBuf {
    contexts_dir(runtime_dir).join(format!("{}.json", run_id.replace(':', "-")))
}

/// Writes the snapshot privately (0600 on Unix) and returns its path.
pub fn write(runtime_dir: &Path, run_id: &str, doc: &ContextDoc) -> std::io::Result<PathBuf> {
    let dir = contexts_dir(runtime_dir);
    std::fs::create_dir_all(&dir)?;
    let path = context_path(runtime_dir, run_id);
    let json = serde_json::to_string_pretty(doc)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&temp, &path)?;
    Ok(path)
}

pub fn read(path: &Path) -> Option<ContextDoc> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Removes snapshots older than `max_age`; returns how many.
pub fn sweep(runtime_dir: &Path, max_age: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(contexts_dir(runtime_dir)) else {
        return 0;
    };
    let mut n = 0;
    for e in entries.flatten() {
        let p = e.path();
        let old = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > max_age);
        if old && std::fs::remove_file(&p).is_ok() {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_and_sweep() {
        let temp = tempfile::tempdir().unwrap();
        let doc = ContextDoc {
            schema_version: SCHEMA_VERSION,
            as_of: "2026-09-16T08:00:00Z".into(),
            run: RunInfo {
                id: "2026-09-16T08:00:00Z".into(),
                pattern: "daily-triage".into(),
                level_configured: Level::L2,
                level_effective: Level::L1,
                level_reason: Some("tokens today at 84% of the cap".into()),
            },
            workspace: "/w".into(),
            budget: Budget {
                percent: 84,
                mode: "report-only".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let path = write(temp.path(), "2026-09-16T08:00:00Z", &doc).unwrap();
        assert!(path.ends_with("loops/2026-09-16T08-00-00Z.json"));
        let back = read(&path).unwrap();
        assert_eq!(back, doc);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["run"]["level_effective"], "L1");
        assert!(v.get("worktree").is_none(), "absent, not null");
        assert_eq!(sweep(temp.path(), std::time::Duration::from_secs(3600)), 0);
        assert_eq!(sweep(temp.path(), std::time::Duration::ZERO), 1);
    }
}
