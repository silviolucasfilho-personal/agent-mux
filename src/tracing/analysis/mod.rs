//! Shared trace analysis, evidence models, and correlation services.

pub mod correlation;
pub mod evidence;
pub mod metrics;
pub mod model;
pub mod query;

pub use correlation::resolve_binding;
pub use evidence::{extract_command, extract_target_file, snippet};
pub use metrics::{analyze_skills, completed_percentiles, ttft_ms};
pub use model::{
    AnalysisError, Binding, Briefing, Confidence, Evidence, EvidenceSource, LiveSession,
    RuntimeState, SessionCard, SkillMetricRow, TaskOutcome, ToolCountSummary,
};
pub use query::briefing;
