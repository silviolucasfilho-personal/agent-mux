//! Typed rows — the writer's input. Each mirrors its table one to one;
//! `Option` fields are "unknown at this write" and never erase a stored
//! value (the upserts use `COALESCE(excluded.col, col)`).

use crate::tracing::usage::NormalizedUsage;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct LaunchRow {
    pub id: String,
    pub run_id: String,
    pub agent_mux_session: i64,
    pub profile: String,
    pub provider: String,
    pub cwd: String,
    pub project_slug: String,
    pub content_mode: String,
    pub correlation_plan: String,
    pub correlation: Option<String>,
    pub session_key: Option<String>,
    pub injected_session_id: bool,
    pub attached: bool,
    pub started_ns: i64,
    pub ended_ns: Option<i64>,
    pub termination: Option<String>,
    pub exit_code: Option<i64>,
    pub parse_errors: Option<i64>,
    pub dropped_ops: Option<i64>,
    pub reported_cost_usd: Option<f64>,
    pub reported_lines_added: Option<i64>,
    pub reported_lines_removed: Option<i64>,
    pub agent_mux_version: String,
    pub user_id: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub tags: Vec<String>,
    /// Merged with `json_patch`: hook-delivered facts (session start
    /// source, end reason, …).
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    /// `"{provider}:{session_id}"` — the claim-registry key.
    pub key: String,
    pub provider: String,
    pub session_id: String,
    pub user_id: Option<String>,
    pub cwd: Option<String>,
    pub project_slug: Option<String>,
    pub transcript_path: Option<String>,
    /// Set once (first write wins).
    pub title: Option<String>,
    /// Bumps `last_seen_ns` (and seeds `first_seen_ns`).
    pub seen_ns: i64,
    /// Merged into `extra` with `json_patch`.
    pub extra: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceStatus {
    Open,
    Closed,
    Aborted,
}

impl TraceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TraceStatus::Open => "open",
            TraceStatus::Closed => "closed",
            TraceStatus::Aborted => "aborted",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceRow {
    pub id: String,
    pub session_key: String,
    pub provider: String,
    pub session_id: String,
    pub launch_id: Option<String>,
    pub ordinal: i64,
    pub name: String,
    pub status: TraceStatus,
    pub start_ns: i64,
    pub end_ns: Option<i64>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub thinking: Option<String>,
    pub skills: Option<Vec<String>>,
    pub reported_duration_ms: Option<i64>,
    pub reported_message_count: Option<i64>,
    pub session_cost_usd: Option<f64>,
    pub timing_approx: bool,
    pub ordinal_salted: bool,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationType {
    Generation,
    Tool,
    Agent,
    Event,
    Span,
}

impl ObservationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ObservationType::Generation => "generation",
            ObservationType::Tool => "tool",
            ObservationType::Agent => "agent",
            ObservationType::Event => "event",
            ObservationType::Span => "span",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Default,
    Warning,
    Error,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Default => "DEFAULT",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservationRow {
    pub id: String,
    pub trace_id: String,
    pub parent_id: Option<String>,
    pub obs_type: ObservationType,
    pub name: String,
    pub kind: Option<String>,
    pub start_ns: i64,
    /// `None` while a tool call is in flight.
    pub end_ns: Option<i64>,
    pub level: Level,
    pub status_message: Option<String>,
    pub model: Option<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub thinking: Option<String>,
    /// Raw integer usage keys exactly as observed.
    pub usage_raw: Option<Vec<(String, i64)>>,
    /// Provider-normalized buckets; the writer prices these.
    pub usage: Option<NormalizedUsage>,
    pub tool_id: Option<String>,
    pub tool_name: Option<String>,
    pub skill: Option<String>,
    pub mcp_server: Option<String>,
    pub path: Option<String>,
    pub is_error: bool,
    pub ts_approx: bool,
    pub metadata: serde_json::Map<String, Value>,
}

/// One idempotent upsert.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreOp {
    Launch(LaunchRow),
    Session(SessionRow),
    Trace(TraceRow),
    Observation(ObservationRow),
}

impl StoreOp {
    /// Launch id an op belongs to, when it carries one (live-stats routing).
    pub fn launch_id(&self) -> Option<&str> {
        match self {
            StoreOp::Launch(l) => Some(&l.id),
            StoreOp::Trace(t) => t.launch_id.as_deref(),
            _ => None,
        }
    }
}

pub fn usage_raw_json(raw: &[(String, i64)]) -> String {
    let map: serde_json::Map<String, Value> = raw
        .iter()
        .map(|(k, v)| (k.clone(), Value::from(*v)))
        .collect();
    Value::Object(map).to_string()
}
