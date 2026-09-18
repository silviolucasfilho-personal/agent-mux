//! The context file a workflow session reads first
//! (`$AGENT_MUX_WORKFLOW_CONTEXT`), mirroring the loop context: facts
//! computed in Rust, one JSON document per session under the runtime dir.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct RunInfo {
    pub id: String,
    pub workflow: String,
    pub harness: String,
    pub workspace: String,
    pub started_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepInfo {
    pub id: String,
    pub kind: String,
    pub phase: String,
    pub label: String,
    pub role: String,
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub of: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Budget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_total: Option<u64>,
    pub tokens_spent: u64,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Context {
    pub schema_version: u32,
    pub run: RunInfo,
    pub step: StepInfo,
    pub args: Value,
    pub item: Value,
    pub inputs: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<Value>,
    pub budget: Budget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_seen: Option<Value>,
}

/// `<runtime>/workflows/<run_id>`.
pub fn run_dir(runtime_dir: &Path, run_id: &str) -> PathBuf {
    runtime_dir.join("workflows").join(run_id)
}

/// Writes `contexts/<n>.json` under the run directory, owner-only where
/// the platform supports it.
pub fn write(run_dir: &Path, n: usize, ctx: &Context) -> std::io::Result<PathBuf> {
    let dir = run_dir.join("contexts");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{n}.json"));
    let json = serde_json::to_string_pretty(ctx)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

/// Removes run directories older than `max_age`.
pub fn sweep(runtime_dir: &Path, max_age: std::time::Duration) -> usize {
    let root = runtime_dir.join("workflows");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    for e in entries.flatten() {
        let p = e.path();
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > max_age);
        if old && p.is_dir() && !p.join("keep").exists() && std::fs::remove_dir_all(&p).is_ok() {
            removed += 1;
        }
    }
    removed
}
