//! Core domain and evidence types for trace analysis and session state.

use serde::{Deserialize, Serialize};

/// Canonical runtime states for an agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    Working,
    WaitingForUser,
    Idle,
    Exited,
    Disconnected,
    Unknown,
}

impl RuntimeState {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuntimeState::Working => "working",
            RuntimeState::WaitingForUser => "waiting_for_user",
            RuntimeState::Idle => "idle",
            RuntimeState::Exited => "exited",
            RuntimeState::Disconnected => "disconnected",
            RuntimeState::Unknown => "unknown",
        }
    }
}

/// Task completion outcome, distinct from process exit or closed turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

/// Provenance sources for inferred or observed telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    Hook,
    Transcript,
    StoreRollup,
    LiveProcess,
    TerminalScreen,
    UserAnnotation,
}

/// Confidence classification for evidence values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Observed,
    Derived,
    Heuristic,
}

/// A qualified telemetry value carrying source and confidence metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence<T> {
    pub value: Option<T>,
    pub source: EvidenceSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

impl<T> Evidence<T> {
    pub fn observed(value: T, source: EvidenceSource, observed_at: Option<String>) -> Self {
        Self {
            value: Some(value),
            source,
            observed_at,
            confidence: Confidence::Observed,
            evidence_ids: Vec::new(),
            limitations: Vec::new(),
        }
    }

    pub fn derived(value: T, source: EvidenceSource) -> Self {
        Self {
            value: Some(value),
            source,
            observed_at: None,
            confidence: Confidence::Derived,
            evidence_ids: Vec::new(),
            limitations: Vec::new(),
        }
    }

    pub fn heuristic(value: T, source: EvidenceSource) -> Self {
        Self {
            value: Some(value),
            source,
            observed_at: None,
            confidence: Confidence::Heuristic,
            evidence_ids: Vec::new(),
            limitations: Vec::new(),
        }
    }

    pub fn missing(source: EvidenceSource, limitation: impl Into<String>) -> Self {
        Self {
            value: None,
            source,
            observed_at: None,
            confidence: Confidence::Derived,
            evidence_ids: Vec::new(),
            limitations: vec![limitation.into()],
        }
    }
}

/// Binding between an agent-mux launch and a provider-native session key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub launch_id: String,
    pub session_key: Option<String>,
}

/// Errors returned by the analysis service and query engine.
#[derive(Debug)]
pub enum AnalysisError {
    Sqlite(rusqlite::Error),
    Correlation(String),
    NotFound(String),
    InvalidParameter(String),
}

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnalysisError::Sqlite(err) => write!(f, "database error: {err}"),
            AnalysisError::Correlation(msg) => write!(f, "correlation error: {msg}"),
            AnalysisError::NotFound(msg) => write!(f, "not found: {msg}"),
            AnalysisError::InvalidParameter(msg) => write!(f, "invalid parameter: {msg}"),
        }
    }
}

impl std::error::Error for AnalysisError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AnalysisError::Sqlite(err) => Some(err),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for AnalysisError {
    fn from(err: rusqlite::Error) -> Self {
        AnalysisError::Sqlite(err)
    }
}
