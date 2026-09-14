//! Shared trace analysis, evidence models, and correlation services.

pub mod correlation;
pub mod cursor;
pub mod evidence;
pub mod live;
pub mod metrics;
pub mod model;
pub mod query;
pub mod scope;
pub mod service;

pub use correlation::resolve_binding;
pub use evidence::{extract_command, extract_target_file, snippet};
pub use live::{
    clean_up_snapshot, is_stale, publish_snapshot, read_snapshots, LiveSnapshot, MAX_SNAPSHOT_BYTES,
};
pub use metrics::{analyze_skills, completed_percentiles, ttft_ms};
pub use model::{
    AnalysisError, Binding, Briefing, Confidence, Evidence, EvidenceSource, LiveSession,
    RuntimeState, SessionCard, SkillMetricRow, TaskOutcome, ToolCountSummary,
};
pub use query::briefing;
use std::path::PathBuf;

/// Fallback location for the traces database if not configured.
pub fn default_trace_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("AGENT_MUX_TRACE_DB") {
        if !p.trim().is_empty() {
            return PathBuf::from(p.trim());
        }
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".agent-mux").join("traces.db"))
        .unwrap_or_else(|| PathBuf::from("traces.db"))
}

/// Fallback location for live session snapshots if not configured.
pub fn default_snapshot_dir() -> PathBuf {
    if let Ok(p) = std::env::var("AGENT_MUX_RUNTIME_DIR") {
        if !p.trim().is_empty() {
            return PathBuf::from(p.trim());
        }
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".agent-mux").join("snapshots"))
        .unwrap_or_else(|| PathBuf::from(".agent-mux-snapshots"))
}
