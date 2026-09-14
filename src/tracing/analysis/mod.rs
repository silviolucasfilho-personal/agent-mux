//! Shared trace analysis, evidence models, and correlation services.

pub mod correlation;
pub mod model;

pub use correlation::resolve_binding;
pub use model::{
    AnalysisError, Binding, Confidence, Evidence, EvidenceSource, RuntimeState, TaskOutcome,
};
