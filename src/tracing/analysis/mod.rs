//! Shared trace analysis, evidence models, and correlation services.

pub mod correlation;
pub mod evidence;
pub mod metrics;
pub mod model;
pub mod query;
pub mod scope;
pub mod service;

pub use correlation::resolve_binding;
pub use evidence::{extract_command, extract_target_file, snippet};
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
