use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Canonical runtime states for an agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

/// Provenance sources for inferred or observed telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Observed,
    Derived,
    Heuristic,
}

/// A qualified telemetry value carrying source and confidence metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

/// Live process and session state captured from active mux instances.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LiveSession {
    pub run_id: String,
    pub launch_id: String,
    pub session_id: usize,
    pub session_key: Option<String>,
    pub provider: Option<String>,
    pub cwd: std::path::PathBuf,
    pub state: RuntimeState,
    pub updated_at_ns: i64,
    pub active_tools: Vec<String>,
}

/// Breakdown of tool invocation counts by tool name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolCountSummary {
    pub name: String,
    pub count: i64,
}

/// Factual evidence-based summary card for an individual session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionCard {
    pub session_key: Option<String>,
    pub launch_id: Option<String>,
    pub provider: Option<String>,
    pub cwd: std::path::PathBuf,
    pub runtime_state: RuntimeState,
    pub task_outcome: TaskOutcome,

    // Initial goal
    pub initial_goal: Evidence<String>,
    // In-flight clue / current activity
    pub current_activity: Evidence<String>,

    // Turns & tools
    pub completed_turns: i64,
    pub open_turns: i64,
    pub total_tools: i64,
    pub tool_counts: Vec<ToolCountSummary>,

    // Accomplishments
    pub files_modified: Vec<Evidence<String>>,
    pub recent_commands: Vec<Evidence<String>>,
    pub last_assistant_output: Option<Evidence<String>>,

    // Timing & Resources
    pub duration_ms: u64,
    pub last_active_ns: i64,
    pub total_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,

    // Concurrency & Active Tools
    pub active_tools: Vec<String>,
}

/// Executive briefing across sessions in a time window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Briefing {
    pub cards: Vec<SessionCard>,
    pub scope_workspace: std::path::PathBuf,
    pub since_ns: i64,
    pub until_ns: i64,
    pub total_sessions: usize,
    pub total_turns: i64,
    pub total_tools: i64,
    pub total_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
    pub warnings: Vec<String>,
}

/// Attribution and performance telemetry for an individual skill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SkillMetricRow {
    pub skill_name: String,
    pub turns_loaded: i64,
    pub attributed_calls: i64,
    pub attributed_tokens: Option<i64>,
    pub attributed_cost_usd: Option<f64>,
    pub error_count: i64,
    pub schema_error_count: i64,
    pub sample_size: usize,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub max_ms: Option<u64>,
    pub slow_calls_above_4s: usize,
    pub ongoing_count: usize,
    pub limitations: Vec<String>,
}
