use super::cursor::{compute_filters_hash, CursorCodec, CursorPayload};
use super::model::{
    AnalysisError, Binding, LiveSession, RuntimeState, SessionCard, SkillMetricRow,
};
use super::query;
use super::scope::Scope;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Stable error codes and classifications for MCP and trace query operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceError {
    InvalidArgument(String),
    NotFound(String),
    ScopeDenied(String),
    DbUnavailable(String),
    SchemaUnsupported(String),
    ContentUnavailable(String),
    QueryTimeout(String),
    Busy(String),
    CursorExpired(String),
    Internal(String),
}

impl ServiceError {
    pub fn code(&self) -> &'static str {
        match self {
            ServiceError::InvalidArgument(_) => "INVALID_ARGUMENT",
            ServiceError::NotFound(_) => "NOT_FOUND",
            ServiceError::ScopeDenied(_) => "SCOPE_DENIED",
            ServiceError::DbUnavailable(_) => "DB_UNAVAILABLE",
            ServiceError::SchemaUnsupported(_) => "SCHEMA_UNSUPPORTED",
            ServiceError::ContentUnavailable(_) => "CONTENT_UNAVAILABLE",
            ServiceError::QueryTimeout(_) => "QUERY_TIMEOUT",
            ServiceError::Busy(_) => "BUSY",
            ServiceError::CursorExpired(_) => "CURSOR_EXPIRED",
            ServiceError::Internal(_) => "INTERNAL_ERROR",
        }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(self, ServiceError::Busy(_) | ServiceError::QueryTimeout(_))
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] ", self.code())?;
        match self {
            ServiceError::InvalidArgument(m)
            | ServiceError::NotFound(m)
            | ServiceError::ScopeDenied(m)
            | ServiceError::DbUnavailable(m)
            | ServiceError::SchemaUnsupported(m)
            | ServiceError::ContentUnavailable(m)
            | ServiceError::QueryTimeout(m)
            | ServiceError::Busy(m)
            | ServiceError::CursorExpired(m)
            | ServiceError::Internal(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ServiceError {}

impl From<AnalysisError> for ServiceError {
    fn from(err: AnalysisError) -> Self {
        match err {
            AnalysisError::NotFound(m) => ServiceError::NotFound(m),
            AnalysisError::InvalidParameter(m) => ServiceError::InvalidArgument(m),
            AnalysisError::Sqlite(e) => ServiceError::from(e),
            AnalysisError::Correlation(m) => ServiceError::Internal(m),
        }
    }
}

impl From<rusqlite::Error> for ServiceError {
    fn from(err: rusqlite::Error) -> Self {
        match err {
            rusqlite::Error::SqliteFailure(ref ffi_err, _)
                if ffi_err.code == rusqlite::ErrorCode::OperationInterrupted =>
            {
                ServiceError::QueryTimeout("query exceeded execution deadline".into())
            }
            _ => ServiceError::DbUnavailable(err.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool Argument Schemas (Section 8 - Closed JSON objects)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct BriefingArgs {
    pub since: Option<String>,
    pub until: Option<String>,
    pub provider: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct ListSessionsArgs {
    pub since: Option<String>,
    pub until: Option<String>,
    pub provider: Option<String>,
    pub runtime_state: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetSessionArgs {
    pub session_key: String,
    pub launch_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimelineArgs {
    pub session_key: String,
    pub launch_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    pub query: String,
    pub session_key: Option<String>,
    pub provider: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct AnalyzeSkillsArgs {
    pub skill: Option<String>,
    pub provider: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareRunsArgs {
    pub a: String,
    pub b: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct HealthArgs {}

/// Request enum dispatching to the eight tool operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool", content = "arguments")]
pub enum Request {
    Briefing(BriefingArgs),
    ListSessions(ListSessionsArgs),
    GetSession(GetSessionArgs),
    Timeline(TimelineArgs),
    Search(SearchArgs),
    AnalyzeSkills(AnalyzeSkillsArgs),
    CompareRuns(CompareRunsArgs),
    Health(HealthArgs),
}

impl Request {
    pub fn from_tool_call(name: &str, arguments: serde_json::Value) -> Result<Self, ServiceError> {
        match name {
            "agent_mux_get_briefing" => {
                let args = serde_json::from_value(arguments).map_err(|e| {
                    ServiceError::InvalidArgument(format!("invalid arguments for briefing: {e}"))
                })?;
                Ok(Request::Briefing(args))
            }
            "agent_mux_list_sessions" => {
                let args = serde_json::from_value(arguments).map_err(|e| {
                    ServiceError::InvalidArgument(format!("invalid arguments for list_sessions: {e}"))
                })?;
                Ok(Request::ListSessions(args))
            }
            "agent_mux_get_session" => {
                let args = serde_json::from_value(arguments).map_err(|e| {
                    ServiceError::InvalidArgument(format!("invalid arguments for get_session: {e}"))
                })?;
                Ok(Request::GetSession(args))
            }
            "agent_mux_get_timeline" => {
                let args = serde_json::from_value(arguments).map_err(|e| {
                    ServiceError::InvalidArgument(format!("invalid arguments for get_timeline: {e}"))
                })?;
                Ok(Request::Timeline(args))
            }
            "agent_mux_search_traces" => {
                let args = serde_json::from_value(arguments).map_err(|e| {
                    ServiceError::InvalidArgument(format!("invalid arguments for search_traces: {e}"))
                })?;
                Ok(Request::Search(args))
            }
            "agent_mux_analyze_skills" => {
                let args = serde_json::from_value(arguments).map_err(|e| {
                    ServiceError::InvalidArgument(format!("invalid arguments for analyze_skills: {e}"))
                })?;
                Ok(Request::AnalyzeSkills(args))
            }
            "agent_mux_compare_runs" => {
                let args = serde_json::from_value(arguments).map_err(|e| {
                    ServiceError::InvalidArgument(format!("invalid arguments for compare_runs: {e}"))
                })?;
                Ok(Request::CompareRuns(args))
            }
            "agent_mux_get_health" => {
                if !arguments.is_null() {
                    let _args: HealthArgs = serde_json::from_value(arguments).map_err(|e| {
                        ServiceError::InvalidArgument(format!("invalid arguments for get_health: {e}"))
                    })?;
                }
                Ok(Request::Health(HealthArgs {}))
            }
            other => Err(ServiceError::InvalidArgument(format!("unknown tool: {other}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// Envelope (Section 8 - Common Response Envelope)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScopeInfo {
    pub workspace: Option<String>,
    pub all_workspaces: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WindowInfo {
    pub since: String,
    pub until: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Full,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CoverageInfo {
    pub status: CoverageStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Envelope<T = serde_json::Value> {
    pub schema_version: u32,
    pub as_of: String,
    pub scope: ScopeInfo,
    pub window: Option<WindowInfo>,
    pub data: T,
    pub coverage: CoverageInfo,
    pub warnings: Vec<String>,
    pub next_cursor: Option<String>,
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Tool Output Schemas
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BriefingData {
    pub sessions: Vec<SessionCard>,
    pub total_sessions: usize,
    pub total_turns: i64,
    pub total_tools: i64,
    pub total_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionSummaryItem {
    pub session_key: Option<String>,
    pub launch_id: Option<String>,
    pub provider: Option<String>,
    pub cwd: String,
    pub runtime_state: RuntimeState,
    pub first_seen: Option<String>,
    pub last_active: Option<String>,
    pub turns: i64,
    pub tools: i64,
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub correlation_quality: String,
    pub usage_coverage: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListSessionsData {
    pub sessions: Vec<SessionSummaryItem>,
    pub total_matching: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetSessionData {
    pub session: SessionCard,
    pub binding: Binding,
    pub correlation_warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineObservationItem {
    pub id: String,
    pub name: String,
    pub observation_type: String,
    pub start_time: String,
    pub duration_ms: Option<u64>,
    pub is_error: bool,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineTurnItem {
    pub turn_id: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub duration_ms: Option<u64>,
    pub input_snippet: Option<String>,
    pub output_snippet: Option<String>,
    pub observations: Vec<TimelineObservationItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineData {
    pub session_key: String,
    pub launch_id: Option<String>,
    pub turns: Vec<TimelineTurnItem>,
    pub total_turns: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchMatchItem {
    pub source_id: String,
    pub source_type: String,
    pub session_key: Option<String>,
    pub launch_id: Option<String>,
    pub provider: Option<String>,
    pub timestamp: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchData {
    pub matches: Vec<SearchMatchItem>,
    pub total_matches: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalyzeSkillsData {
    pub skills: Vec<SkillMetricRow>,
    pub total_skills: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunMetrics {
    pub launch_id: String,
    pub session_key: Option<String>,
    pub provider: Option<String>,
    pub cwd: String,
    pub turns: i64,
    pub tools: i64,
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub duration_ms: u64,
    pub initial_goal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunDeltas {
    pub delta_turns: i64,
    pub delta_tools: i64,
    pub delta_tokens: Option<i64>,
    pub delta_cost_usd: Option<f64>,
    pub delta_duration_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompareRunsData {
    pub run_a: RunMetrics,
    pub run_b: RunMetrics,
    pub deltas: RunDeltas,
    pub comparability_warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HealthData {
    pub reader_version: String,
    pub schema_version: u32,
    pub db_available: bool,
    pub db_path: String,
    pub collector_freshness: Option<String>,
    pub content_mode: String,
    pub provider_coverage: Vec<String>,
    pub features: Vec<String>,
}

// ---------------------------------------------------------------------------
// Configuration & TraceService
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub concurrent: usize,
    pub queue: usize,
    pub response_bytes: usize,
    pub page_default: usize,
    pub page_max: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            concurrent: 4,
            queue: 16,
            response_bytes: 64 * 1024,
            page_default: 20,
            page_max: 100,
        }
    }
}

#[derive(Clone)]
pub struct ServiceConfig {
    pub db_path: PathBuf,
    pub scope: Scope,
    pub snapshot_dir: Option<PathBuf>,
    pub limits: Limits,
    pub admission_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for ServiceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceConfig")
            .field("db_path", &self.db_path)
            .field("scope", &self.scope)
            .field("snapshot_dir", &self.snapshot_dir)
            .field("limits", &self.limits)
            .field("admission_hook", &self.admission_hook.is_some())
            .finish()
    }
}

impl ServiceConfig {
    pub fn new(db_path: PathBuf, scope: Scope) -> Self {
        Self {
            db_path,
            scope,
            snapshot_dir: None,
            limits: Limits::default(),
            admission_hook: None,
        }
    }

    pub fn resolve(explicit_db: Option<&Path>, scope: Scope) -> Self {
        let db_path = explicit_db
            .map(|p| p.to_path_buf())
            .or_else(|| {
                std::env::var("AGENT_MUX_TRACE_DB")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .map(PathBuf::from)
            })
            .or_else(|| {
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(|h| PathBuf::from(h).join(".agent-mux").join("traces.db"))
            })
            .unwrap_or_else(|| PathBuf::from("traces.db"));

        Self {
            db_path,
            scope,
            snapshot_dir: None,
            limits: Limits::default(),
            admission_hook: None,
        }
    }
}

struct AdmissionState {
    running: usize,
    waiting: usize,
}

pub struct TraceService {
    config: ServiceConfig,
    codec: CursorCodec,
    admission: Arc<(std::sync::Mutex<AdmissionState>, std::sync::Condvar)>,
}

impl TraceService {
    pub fn new(config: ServiceConfig) -> Result<Self, ServiceError> {
        let mut key = [0u8; 32];
        let u1 = uuid::Uuid::new_v4();
        let u2 = uuid::Uuid::new_v4();
        key[0..16].copy_from_slice(u1.as_bytes());
        key[16..32].copy_from_slice(u2.as_bytes());
        let codec = CursorCodec::new(key);
        let admission = Arc::new((
            std::sync::Mutex::new(AdmissionState {
                running: 0,
                waiting: 0,
            }),
            std::sync::Condvar::new(),
        ));
        Ok(Self {
            config,
            codec,
            admission,
        })
    }

    pub fn config(&self) -> &ServiceConfig {
        &self.config
    }

    pub fn codec(&self) -> &CursorCodec {
        &self.codec
    }

    pub fn admission_counts(&self) -> (usize, usize) {
        if let Ok(state) = self.admission.0.lock() {
            (state.running, state.waiting)
        } else {
            (0, 0)
        }
    }

    pub fn load_live_sessions(&self, now_ns: i64) -> (Vec<LiveSession>, bool) {
        let dir = self
            .config
            .snapshot_dir
            .clone()
            .unwrap_or_else(crate::tracing::analysis::default_snapshot_dir);
        if !dir.exists() {
            return (Vec::new(), false);
        }
        match crate::tracing::analysis::read_snapshots(&dir, now_ns) {
            Ok(snaps) => {
                let available = !snaps.is_empty();
                let mut sessions = Vec::new();
                for snap in snaps {
                    sessions.extend(snap.sessions);
                }
                (sessions, available)
            }
            Err(_) => (Vec::new(), false),
        }
    }

    /// Synchronously executes a trace query or analysis request.
    pub fn execute(&self, request: Request) -> Result<Envelope<serde_json::Value>, ServiceError> {
        let start = std::time::Instant::now();
        let deadline_duration = match request {
            Request::AnalyzeSkills(_) | Request::CompareRuns(_) => std::time::Duration::from_secs(5),
            _ => std::time::Duration::from_secs(2),
        };

        // Admission check
        {
            let mut state = self.admission.0.lock().map_err(|_| {
                ServiceError::Internal("admission lock poisoned".into())
            })?;
            if state.running < self.config.limits.concurrent {
                state.running += 1;
            } else if state.waiting < self.config.limits.queue {
                state.waiting += 1;
                while state.running >= self.config.limits.concurrent {
                    let elapsed = start.elapsed();
                    if elapsed >= deadline_duration {
                        state.waiting = state.waiting.saturating_sub(1);
                        return Err(ServiceError::QueryTimeout(
                            "query timed out waiting for admission slot".into(),
                        ));
                    }
                    let remaining = deadline_duration - elapsed;
                    let (new_state, timeout_result) = self
                        .admission
                        .1
                        .wait_timeout(state, remaining)
                        .map_err(|_| ServiceError::Internal("admission condvar poisoned".into()))?;
                    state = new_state;
                    if timeout_result.timed_out() && state.running >= self.config.limits.concurrent {
                        state.waiting = state.waiting.saturating_sub(1);
                        return Err(ServiceError::QueryTimeout(
                            "query timed out waiting for admission slot".into(),
                        ));
                    }
                }
                state.waiting = state.waiting.saturating_sub(1);
                state.running += 1;
            } else {
                return Err(ServiceError::Busy(
                    "capacity exceeded: queue full".into(),
                ));
            }
        }

        struct SlotGuard<'a>(&'a (std::sync::Mutex<AdmissionState>, std::sync::Condvar));
        impl<'a> Drop for SlotGuard<'a> {
            fn drop(&mut self) {
                if let Ok(mut s) = self.0.0.lock() {
                    s.running = s.running.saturating_sub(1);
                    self.0.1.notify_one();
                }
            }
        }
        let _slot_guard = SlotGuard(&self.admission);

        if let Some(ref hook) = self.config.admission_hook {
            hook();
        }

        let now = OffsetDateTime::now_utc();
        let as_of = now.format(&Rfc3339).map_err(|e| ServiceError::Internal(e.to_string()))?;
        let scope_info = ScopeInfo {
            workspace: self.config.scope.workspace_path().map(|p| p.to_string_lossy().to_string()),
            all_workspaces: self.config.scope.is_all_workspaces(),
        };

        // For Health, database existence is reported as a capability flag without failing
        if let Request::Health(_) = request {
            let data = self.execute_health(&as_of)?;
            return Ok(Envelope {
                schema_version: 1,
                as_of,
                scope: scope_info,
                window: None,
                data: serde_json::to_value(data).map_err(|e| ServiceError::Internal(e.to_string()))?,
                coverage: CoverageInfo {
                    status: CoverageStatus::Full,
                    reasons: Vec::new(),
                },
                warnings: Vec::new(),
                next_cursor: None,
                truncated: false,
            });
        }

        // For all other tools, DB must exist and be readable
        if !self.config.db_path.exists() {
            return Err(ServiceError::DbUnavailable(format!(
                "trace database does not exist: {}",
                self.config.db_path.display()
            )));
        }

        let conn = Connection::open_with_flags(
            &self.config.db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| ServiceError::DbUnavailable(format!("cannot open database read-only: {e}")))?;

        // Install progress handler with deadline
        let deadline_instant = start + deadline_duration;
        let _ = conn.progress_handler(50, Some(move || {
            std::time::Instant::now() >= deadline_instant
        }));

        let mut envelope = match request {
            Request::Health(_) => unreachable!(),
            Request::Briefing(args) => self.execute_briefing(&conn, args, as_of, scope_info, now)?,
            Request::ListSessions(args) => self.execute_list_sessions(&conn, args, as_of, scope_info, now)?,
            Request::GetSession(args) => self.execute_get_session(&conn, args, as_of, scope_info)?,
            Request::Timeline(args) => self.execute_timeline(&conn, args, as_of, scope_info)?,
            Request::Search(args) => self.execute_search(&conn, args, as_of, scope_info, now)?,
            Request::AnalyzeSkills(args) => self.execute_analyze_skills(&conn, args, as_of, scope_info, now)?,
            Request::CompareRuns(args) => self.execute_compare_runs(&conn, args, as_of, scope_info)?,
        };

        let _ = conn.progress_handler(0, None::<fn() -> bool>);

        bound_envelope(&mut envelope, self.config.limits.response_bytes);

        Ok(envelope)
    }

    // --- Tool Handlers ---

    fn execute_health(&self, _as_of: &str) -> Result<HealthData, ServiceError> {
        let db_available = self.config.db_path.exists();
        let mut collector_freshness = None;

        if db_available {
            if let Ok(conn) = Connection::open_with_flags(
                &self.config.db_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ) {
                let latest: Option<i64> = conn
                    .query_row(
                        "SELECT MAX(start_ns) FROM traces",
                        [],
                        |r| r.get(0),
                    )
                    .optional()
                    .unwrap_or(None);

                if let Some(ns) = latest {
                    if let Ok(dt) = OffsetDateTime::from_unix_timestamp_nanos(ns as i128) {
                        collector_freshness = dt.format(&Rfc3339).ok();
                    }
                }
            }
        }

        let now_ns = OffsetDateTime::now_utc().unix_timestamp_nanos() as i64;
        let (_, live_available) = self.load_live_sessions(now_ns);
        let mut features = vec![
            "briefing".into(),
            "list_sessions".into(),
            "get_session".into(),
            "timeline".into(),
            "search".into(),
            "analyze_skills".into(),
            "compare_runs".into(),
            "health".into(),
        ];
        if live_available {
            features.push("live_sessions".into());
        }

        Ok(HealthData {
            reader_version: env!("CARGO_PKG_VERSION").to_string(),
            schema_version: 1,
            db_available,
            db_path: self.config.db_path.to_string_lossy().to_string(),
            collector_freshness,
            content_mode: "full".to_string(),
            provider_coverage: vec!["claude".into(), "codex".into(), "antigravity".into()],
            features,
        })
    }

    fn execute_briefing(
        &self,
        conn: &Connection,
        args: BriefingArgs,
        as_of: String,
        scope_info: ScopeInfo,
        now: OffsetDateTime,
    ) -> Result<Envelope<serde_json::Value>, ServiceError> {
        let (since_ns, until_ns, window) = parse_window(args.since.as_deref(), args.until.as_deref(), now)?;
        let limit = args.limit.unwrap_or(self.config.limits.page_default).min(self.config.limits.page_max);

        let now_ns = now.unix_timestamp_nanos() as i64;
        let ws = self.config.scope.workspace_path().unwrap_or(Path::new(""));
        let (live, live_available) = self.load_live_sessions(now_ns);

        let mut b = query::briefing(conn, ws, since_ns, until_ns, &live)?;

        // Filter by provider if specified
        if let Some(ref prov) = args.provider {
            b.cards.retain(|c| c.provider.as_deref() == Some(prov.as_str()));
        }

        let filters_detail = format!("prov:{:?}", args.provider);
        let filters_hash = compute_filters_hash("briefing", &self.config.scope, &filters_detail);

        let mut offset = 0;
        if let Some(ref c) = args.cursor {
            let payload = self.codec.decode(c, &filters_hash, now_ns)?;
            offset = payload.offset;
        }

        let total_sessions = b.cards.len();
        let mut paged_cards = if offset < b.cards.len() {
            b.cards.into_iter().skip(offset).collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        let has_more = paged_cards.len() > limit;
        if has_more {
            paged_cards.truncate(limit);
        }

        let next_cursor = if has_more {
            Some(self.codec.encode(&CursorPayload {
                schema_version: 1,
                issued_at_ns: now_ns,
                expires_at_ns: now_ns + 10 * 60 * 1_000_000_000,
                filters_hash,
                upper_bound_ns: until_ns,
                sort_key: paged_cards.last().and_then(|c| c.session_key.clone()).unwrap_or_default(),
                offset: offset + limit,
            }))
        } else {
            None
        };

        let data = BriefingData {
            sessions: paged_cards,
            total_sessions,
            total_turns: b.total_turns,
            total_tools: b.total_tools,
            total_tokens: b.total_tokens,
            total_cost_usd: b.total_cost_usd,
        };

        Ok(Envelope {
            schema_version: 1,
            as_of,
            scope: scope_info,
            window: Some(window),
            data: serde_json::to_value(data).map_err(|e| ServiceError::Internal(e.to_string()))?,
            coverage: CoverageInfo {
                status: if live_available {
                    CoverageStatus::Full
                } else {
                    CoverageStatus::Partial
                },
                reasons: if live_available {
                    Vec::new()
                } else {
                    vec!["live_snapshot_unavailable".into()]
                },
            },
            warnings: b.warnings,
            next_cursor,
            truncated: has_more,
        })
    }

    fn execute_list_sessions(
        &self,
        conn: &Connection,
        args: ListSessionsArgs,
        as_of: String,
        scope_info: ScopeInfo,
        now: OffsetDateTime,
    ) -> Result<Envelope<serde_json::Value>, ServiceError> {
        let (since_ns, until_ns, window) = parse_window(args.since.as_deref(), args.until.as_deref(), now)?;
        let limit = args.limit.unwrap_or(self.config.limits.page_default).min(self.config.limits.page_max);

        let now_ns = now.unix_timestamp_nanos() as i64;
        let (live, live_available) = self.load_live_sessions(now_ns);
        let filters_detail = format!("prov:{:?}:state:{:?}", args.provider, args.runtime_state);
        let filters_hash = compute_filters_hash("list_sessions", &self.config.scope, &filters_detail);

        let mut offset = 0;
        if let Some(ref c) = args.cursor {
            let payload = self.codec.decode(c, &filters_hash, now_ns)?;
            offset = payload.offset;
        }

        let mut items = Vec::new();

        let has_sessions: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = 'sessions'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);

        if has_sessions {
            let mut stmt = conn.prepare(
                "SELECT s.key, s.provider, s.cwd, s.first_seen_ns, s.last_seen_ns,
                        COALESCE(st.turn_count, 0),
                        COALESCE(st.total_tools, 0),
                        st.total_tokens,
                        st.total_cost_usd
                 FROM sessions s
                 LEFT JOIN session_stats st ON st.session_key = s.key
                 WHERE (s.last_seen_ns >= ?1 AND s.first_seen_ns < ?2)
                 ORDER BY s.last_seen_ns DESC",
            )?;

            let rows = stmt.query_map(params![since_ns, until_ns], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, Option<f64>>(8)?,
                ))
            })?;

            for row in rows.flatten() {
                let (key, provider, cwd_opt, first_seen_ns, last_seen_ns, turns, tools, tokens, cost) = row;
                let cwd_path = cwd_opt.as_deref().map(Path::new).unwrap_or(Path::new(""));

                if !self.config.scope.allows_session_cwd(cwd_path) {
                    continue;
                }

                if let Some(ref req_prov) = args.provider {
                    if provider.as_deref() != Some(req_prov.as_str()) {
                        continue;
                    }
                }

                let first_seen = OffsetDateTime::from_unix_timestamp_nanos(first_seen_ns as i128)
                    .ok()
                    .and_then(|dt| dt.format(&Rfc3339).ok());
                let last_active = OffsetDateTime::from_unix_timestamp_nanos(last_seen_ns as i128)
                    .ok()
                    .and_then(|dt| dt.format(&Rfc3339).ok());

                items.push(SessionSummaryItem {
                    session_key: Some(key),
                    launch_id: None,
                    provider,
                    cwd: cwd_path.to_string_lossy().to_string(),
                    runtime_state: RuntimeState::Exited,
                    first_seen,
                    last_active,
                    turns,
                    tools,
                    tokens,
                    cost_usd: cost,
                    correlation_quality: "exact".into(),
                    usage_coverage: if tokens.is_some() { "full".into() } else { "none".into() },
                });
            }
        }

        // Augment with live session states
        for item in &mut items {
            if let Some(ref key) = item.session_key {
                if let Some(ls) = live.iter().find(|s| s.session_key.as_deref() == Some(key.as_str())) {
                    item.runtime_state = ls.state;
                }
            }
        }
        if let Some(ref req_state) = args.runtime_state {
            items.retain(|item| item.runtime_state.as_str() == req_state.as_str());
        }

        let total_matching = items.len();
        let mut paged_items = if offset < items.len() {
            items[offset..].to_vec()
        } else {
            Vec::new()
        };

        let has_more = paged_items.len() > limit;
        if has_more {
            paged_items.truncate(limit);
        }

        let next_cursor = if has_more {
            Some(self.codec.encode(&CursorPayload {
                schema_version: 1,
                issued_at_ns: now_ns,
                expires_at_ns: now_ns + 10 * 60 * 1_000_000_000,
                filters_hash,
                upper_bound_ns: until_ns,
                sort_key: paged_items.last().and_then(|i| i.session_key.clone()).unwrap_or_default(),
                offset: offset + limit,
            }))
        } else {
            None
        };

        let data = ListSessionsData {
            sessions: paged_items,
            total_matching,
        };

        Ok(Envelope {
            schema_version: 1,
            as_of,
            scope: scope_info,
            window: Some(window),
            data: serde_json::to_value(data).map_err(|e| ServiceError::Internal(e.to_string()))?,
            coverage: CoverageInfo {
                status: if live_available {
                    CoverageStatus::Full
                } else {
                    CoverageStatus::Partial
                },
                reasons: if live_available {
                    Vec::new()
                } else {
                    vec!["live_snapshot_unavailable".into()]
                },
            },
            warnings: Vec::new(),
            next_cursor,
            truncated: has_more,
        })
    }

    fn execute_get_session(
        &self,
        conn: &Connection,
        args: GetSessionArgs,
        as_of: String,
        scope_info: ScopeInfo,
    ) -> Result<Envelope<serde_json::Value>, ServiceError> {
        // Verify session existence and extract metadata
        let (cwd_str, _provider): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT cwd, provider FROM sessions WHERE key = ?1",
                params![args.session_key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .or_else(|| {
                conn.query_row(
                    "SELECT cwd, provider FROM traces WHERE session_key = ?1 LIMIT 1",
                    params![args.session_key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .ok()
                .flatten()
            })
            .ok_or_else(|| ServiceError::NotFound(format!("session '{}' not found", args.session_key)))?;

        let cwd_path = cwd_str.as_deref().map(Path::new).unwrap_or(Path::new(""));

        // Scope check: must be inside authorized workspace
        if !self.config.scope.allows_session_cwd(cwd_path) {
            return Err(ServiceError::ScopeDenied(format!(
                "session '{}' is outside authorized workspace scope",
                args.session_key
            )));
        }

        // If launch_id is specified, verify it belongs to this session
        if let Some(ref lid) = args.launch_id {
            let belongs: bool = conn
                .query_row(
                    "SELECT 1 FROM traces WHERE session_key = ?1 AND launch_id = ?2 LIMIT 1",
                    params![args.session_key, lid],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);

            if !belongs {
                return Err(ServiceError::InvalidArgument(format!(
                    "launch_id '{}' does not belong to session '{}'",
                    lid, args.session_key
                )));
            }
        }

        // Build session card using query builder logic
        let card = query::briefing(
            conn,
            cwd_path,
            0,
            i64::MAX,
            &[],
        )?
        .cards
        .into_iter()
        .find(|c| c.session_key.as_deref() == Some(&args.session_key))
        .ok_or_else(|| ServiceError::NotFound(format!("session '{}' details not found", args.session_key)))?;

        let binding = Binding {
            launch_id: args.launch_id.clone().unwrap_or_default(),
            session_key: Some(args.session_key.clone()),
        };

        let data = GetSessionData {
            session: card,
            binding,
            correlation_warnings: Vec::new(),
        };

        Ok(Envelope {
            schema_version: 1,
            as_of,
            scope: scope_info,
            window: None,
            data: serde_json::to_value(data).map_err(|e| ServiceError::Internal(e.to_string()))?,
            coverage: CoverageInfo {
                status: CoverageStatus::Full,
                reasons: Vec::new(),
            },
            warnings: Vec::new(),
            next_cursor: None,
            truncated: false,
        })
    }

    fn execute_timeline(
        &self,
        conn: &Connection,
        args: TimelineArgs,
        as_of: String,
        scope_info: ScopeInfo,
    ) -> Result<Envelope<serde_json::Value>, ServiceError> {
        // Scope & Existence check
        let cwd_str: Option<String> = conn
            .query_row(
                "SELECT cwd FROM sessions WHERE key = ?1",
                params![args.session_key],
                |r| r.get(0),
            )
            .optional()?
            .or_else(|| {
                conn.query_row(
                    "SELECT cwd FROM traces WHERE session_key = ?1 LIMIT 1",
                    params![args.session_key],
                    |r| r.get(0),
                )
                .optional()
                .ok()
                .flatten()
            })
            .ok_or_else(|| ServiceError::NotFound(format!("session '{}' not found", args.session_key)))?;

        let cwd_path = cwd_str.as_deref().map(Path::new).unwrap_or(Path::new(""));
        if !self.config.scope.allows_session_cwd(cwd_path) {
            return Err(ServiceError::ScopeDenied(format!(
                "session '{}' is outside authorized workspace scope",
                args.session_key
            )));
        }

        if let Some(ref lid) = args.launch_id {
            let belongs: bool = conn
                .query_row(
                    "SELECT 1 FROM traces WHERE session_key = ?1 AND launch_id = ?2 LIMIT 1",
                    params![args.session_key, lid],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);

            if !belongs {
                return Err(ServiceError::InvalidArgument(format!(
                    "launch_id '{}' does not belong to session '{}'",
                    lid, args.session_key
                )));
            }
        }

        let lid_filter = args.launch_id.as_deref().unwrap_or("");
        let limit = args.limit.unwrap_or(self.config.limits.page_default).min(self.config.limits.page_max);

        let now_ns = OffsetDateTime::now_utc().unix_timestamp_nanos() as i64;
        let filters_detail = format!("sess:{}:launch:{:?}", args.session_key, args.launch_id);
        let filters_hash = compute_filters_hash("timeline", &self.config.scope, &filters_detail);

        let mut offset = 0;
        if let Some(ref c) = args.cursor {
            let payload = self.codec.decode(c, &filters_hash, now_ns)?;
            offset = payload.offset;
        }

        let mut stmt = conn.prepare(
            "SELECT id, start_ns, end_ns, input, output
             FROM traces
             WHERE session_key = ?1 AND (?2 = '' OR launch_id = ?2)
             ORDER BY start_ns ASC, id ASC",
        )?;

        let trace_rows = stmt.query_map(params![args.session_key, lid_filter], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;

        let mut turns = Vec::new();

        for row in trace_rows.flatten() {
            let (t_id, start_ns, end_ns, input, output) = row;
            let start_time = OffsetDateTime::from_unix_timestamp_nanos(start_ns as i128)
                .ok()
                .and_then(|dt| dt.format(&Rfc3339).ok())
                .unwrap_or_default();
            let end_time = end_ns.and_then(|ns| {
                OffsetDateTime::from_unix_timestamp_nanos(ns as i128)
                    .ok()
                    .and_then(|dt| dt.format(&Rfc3339).ok())
            });
            let duration_ms = end_ns.map(|e| ((e.saturating_sub(start_ns)) / 1_000_000) as u64);

            // Fetch observations for this trace turn
            let mut obs = Vec::new();
            if let Ok(mut obs_stmt) = conn.prepare(
                "SELECT id, name, type, start_ns, end_ns, is_error, status_message
                 FROM observations
                 WHERE trace_id = ?1
                 ORDER BY start_ns ASC, id ASC",
            ) {
                if let Ok(obs_rows) = obs_stmt.query_map(params![t_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?.unwrap_or_else(|| "unknown".into()),
                        r.get::<_, Option<String>>(2)?.unwrap_or_else(|| "tool".into()),
                        r.get::<_, i64>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                        r.get::<_, Option<i64>>(5)?.unwrap_or(0) != 0,
                        r.get::<_, Option<String>>(6)?,
                    ))
                }) {
                    for o in obs_rows.flatten() {
                        let (o_id, o_name, o_type, o_start_ns, o_end_ns, is_err, status_msg) = o;
                        let o_start_time = OffsetDateTime::from_unix_timestamp_nanos(o_start_ns as i128)
                            .ok()
                            .and_then(|dt| dt.format(&Rfc3339).ok())
                            .unwrap_or_default();
                        let o_dur_ms = o_end_ns.map(|e| ((e.saturating_sub(o_start_ns)) / 1_000_000) as u64);

                        obs.push(TimelineObservationItem {
                            id: o_id,
                            name: o_name,
                            observation_type: o_type,
                            start_time: o_start_time,
                            duration_ms: o_dur_ms,
                            is_error: is_err,
                            error_message: status_msg,
                        });
                    }
                }
            }

            turns.push(TimelineTurnItem {
                turn_id: t_id,
                start_time,
                end_time,
                duration_ms,
                input_snippet: input.as_deref().map(|s| super::evidence::snippet(s, 200)),
                output_snippet: output.as_deref().map(|s| super::evidence::snippet(s, 200)),
                observations: obs,
            });
        }

        let total_turns = turns.len();
        let mut paged_turns = if offset < turns.len() {
            turns[offset..].to_vec()
        } else {
            Vec::new()
        };

        let has_more = paged_turns.len() > limit;
        if has_more {
            paged_turns.truncate(limit);
        }

        let next_cursor = if has_more {
            Some(self.codec.encode(&CursorPayload {
                schema_version: 1,
                issued_at_ns: now_ns,
                expires_at_ns: now_ns + 10 * 60 * 1_000_000_000,
                filters_hash,
                upper_bound_ns: now_ns,
                sort_key: paged_turns.last().map(|t| t.turn_id.clone()).unwrap_or_default(),
                offset: offset + limit,
            }))
        } else {
            None
        };

        let data = TimelineData {
            session_key: args.session_key,
            launch_id: args.launch_id,
            turns: paged_turns,
            total_turns,
        };

        Ok(Envelope {
            schema_version: 1,
            as_of,
            scope: scope_info,
            window: None,
            data: serde_json::to_value(data).map_err(|e| ServiceError::Internal(e.to_string()))?,
            coverage: CoverageInfo {
                status: CoverageStatus::Full,
                reasons: Vec::new(),
            },
            warnings: Vec::new(),
            next_cursor,
            truncated: has_more,
        })
    }

    fn execute_search(
        &self,
        conn: &Connection,
        args: SearchArgs,
        as_of: String,
        scope_info: ScopeInfo,
        now: OffsetDateTime,
    ) -> Result<Envelope<serde_json::Value>, ServiceError> {
        if args.query.len() > 4096 {
            return Err(ServiceError::InvalidArgument("query exceeds 4 KiB limit".into()));
        }
        if args.query.trim().is_empty() {
            return Err(ServiceError::InvalidArgument("query cannot be empty".into()));
        }

        let (since_ns, until_ns, window) = parse_window(args.since.as_deref(), args.until.as_deref(), now)?;
        let limit = args.limit.unwrap_or(self.config.limits.page_default).min(self.config.limits.page_max);

        let now_ns = now.unix_timestamp_nanos() as i64;
        let filters_detail = format!("q:{}:sess:{:?}:prov:{:?}", args.query, args.session_key, args.provider);
        let filters_hash = compute_filters_hash("search", &self.config.scope, &filters_detail);

        let mut offset = 0;
        if let Some(ref c) = args.cursor {
            let payload = self.codec.decode(c, &filters_hash, now_ns)?;
            offset = payload.offset;
        }

        // If session_key specified, verify scope
        if let Some(ref sk) = args.session_key {
            let cwd_str: Option<String> = conn
                .query_row(
                    "SELECT cwd FROM sessions WHERE key = ?1",
                    params![sk],
                    |r| r.get(0),
                )
                .optional()?
                .or_else(|| {
                    conn.query_row(
                        "SELECT cwd FROM traces WHERE session_key = ?1 LIMIT 1",
                        params![sk],
                        |r| r.get(0),
                    )
                    .optional()
                    .ok()
                    .flatten()
                })
                .ok_or_else(|| ServiceError::NotFound(format!("session '{sk}' not found")))?;

            let cwd_path = cwd_str.as_deref().map(Path::new).unwrap_or(Path::new(""));
            if !self.config.scope.allows_session_cwd(cwd_path) {
                return Err(ServiceError::ScopeDenied(format!(
                    "session '{sk}' is outside authorized workspace scope"
                )));
            }
        }

        let mut matches = Vec::new();
        let query_pattern = format!("%{}%", args.query);
        let sk_filter = args.session_key.as_deref().unwrap_or("");
        let prov_filter = args.provider.as_deref().unwrap_or("");

        // Push scope into SQL query
        let ws_str = self
            .config
            .scope
            .workspace_path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let all_ws = self.config.scope.is_all_workspaces();

        // 1. Search in traces (inputs/outputs)
        let mut t_stmt = conn.prepare(
            "SELECT id, session_key, launch_id, provider, cwd, start_ns, input, output
             FROM traces
             WHERE start_ns >= ?1 AND start_ns < ?2
               AND (?3 = 1 OR cwd = ?4)
               AND (?5 = '' OR session_key = ?5)
               AND (?6 = '' OR provider = ?6)
               AND (input LIKE ?7 OR output LIKE ?7)
             ORDER BY start_ns DESC",
        )?;

        let t_rows = t_stmt.query_map(
            params![
                since_ns,
                until_ns,
                if all_ws { 1 } else { 0 },
                ws_str,
                sk_filter,
                prov_filter,
                query_pattern,
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            },
        )?;

        for r in t_rows.flatten() {
            let (id, s_key, l_id, prov, cwd, start_ns, in_opt, out_opt) = r;
            let cwd_path = cwd.as_deref().map(Path::new).unwrap_or(Path::new(""));
            if !self.config.scope.allows_session_cwd(cwd_path) {
                continue;
            }

            let ts = OffsetDateTime::from_unix_timestamp_nanos(start_ns as i128)
                .ok()
                .and_then(|dt| dt.format(&Rfc3339).ok())
                .unwrap_or_default();

            if let Some(ref text) = in_opt {
                if text.to_lowercase().contains(&args.query.to_lowercase()) {
                    matches.push(SearchMatchItem {
                        source_id: format!("trace:{id}:input"),
                        source_type: "trace_input".into(),
                        session_key: s_key.clone(),
                        launch_id: l_id.clone(),
                        provider: prov.clone(),
                        timestamp: ts.clone(),
                        snippet: super::evidence::snippet(text, 150),
                    });
                }
            }

            if let Some(ref text) = out_opt {
                if text.to_lowercase().contains(&args.query.to_lowercase()) {
                    matches.push(SearchMatchItem {
                        source_id: format!("trace:{id}:output"),
                        source_type: "trace_output".into(),
                        session_key: s_key,
                        launch_id: l_id,
                        provider: prov,
                        timestamp: ts,
                        snippet: super::evidence::snippet(text, 150),
                    });
                }
            }
        }

        let total_matches = matches.len();
        let mut paged_matches = if offset < matches.len() {
            matches[offset..].to_vec()
        } else {
            Vec::new()
        };

        let has_more = paged_matches.len() > limit;
        if has_more {
            paged_matches.truncate(limit);
        }

        let next_cursor = if has_more {
            Some(self.codec.encode(&CursorPayload {
                schema_version: 1,
                issued_at_ns: now_ns,
                expires_at_ns: now_ns + 10 * 60 * 1_000_000_000,
                filters_hash,
                upper_bound_ns: until_ns,
                sort_key: paged_matches.last().map(|m| m.source_id.clone()).unwrap_or_default(),
                offset: offset + limit,
            }))
        } else {
            None
        };

        let data = SearchData {
            matches: paged_matches,
            total_matches,
        };

        Ok(Envelope {
            schema_version: 1,
            as_of,
            scope: scope_info,
            window: Some(window),
            data: serde_json::to_value(data).map_err(|e| ServiceError::Internal(e.to_string()))?,
            coverage: CoverageInfo {
                status: CoverageStatus::Full,
                reasons: Vec::new(),
            },
            warnings: Vec::new(),
            next_cursor,
            truncated: has_more,
        })
    }

    fn execute_analyze_skills(
        &self,
        conn: &Connection,
        args: AnalyzeSkillsArgs,
        as_of: String,
        scope_info: ScopeInfo,
        now: OffsetDateTime,
    ) -> Result<Envelope<serde_json::Value>, ServiceError> {
        let (since_ns, until_ns, window) = parse_window(args.since.as_deref(), args.until.as_deref(), now)?;
        let limit = args.limit.unwrap_or(self.config.limits.page_default).min(self.config.limits.page_max);

        let now_ns = now.unix_timestamp_nanos() as i64;
        let filters_detail = format!("skill:{:?}:prov:{:?}", args.skill, args.provider);
        let filters_hash = compute_filters_hash("analyze_skills", &self.config.scope, &filters_detail);

        let mut offset = 0;
        if let Some(ref c) = args.cursor {
            let payload = self.codec.decode(c, &filters_hash, now_ns)?;
            offset = payload.offset;
        }

        let skills = super::metrics::analyze_skills(
            conn,
            args.skill.as_deref(),
            Some(since_ns),
            Some(until_ns),
        )?;

        let total_skills = skills.len();
        let mut paged_skills = if offset < skills.len() {
            skills[offset..].to_vec()
        } else {
            Vec::new()
        };

        let has_more = paged_skills.len() > limit;
        if has_more {
            paged_skills.truncate(limit);
        }

        let next_cursor = if has_more {
            Some(self.codec.encode(&CursorPayload {
                schema_version: 1,
                issued_at_ns: now_ns,
                expires_at_ns: now_ns + 10 * 60 * 1_000_000_000,
                filters_hash,
                upper_bound_ns: until_ns,
                sort_key: paged_skills.last().map(|s| s.skill_name.clone()).unwrap_or_default(),
                offset: offset + limit,
            }))
        } else {
            None
        };

        let data = AnalyzeSkillsData {
            skills: paged_skills,
            total_skills,
        };

        Ok(Envelope {
            schema_version: 1,
            as_of,
            scope: scope_info,
            window: Some(window),
            data: serde_json::to_value(data).map_err(|e| ServiceError::Internal(e.to_string()))?,
            coverage: CoverageInfo {
                status: CoverageStatus::Full,
                reasons: Vec::new(),
            },
            warnings: Vec::new(),
            next_cursor,
            truncated: has_more,
        })
    }

    fn execute_compare_runs(
        &self,
        conn: &Connection,
        args: CompareRunsArgs,
        as_of: String,
        scope_info: ScopeInfo,
    ) -> Result<Envelope<serde_json::Value>, ServiceError> {
        let load_run = |lid: &str| -> Result<RunMetrics, ServiceError> {
            let row = conn
                .query_row(
                    "SELECT session_key, provider, cwd FROM traces WHERE launch_id = ?1 LIMIT 1",
                    params![lid],
                    |r| {
                        Ok((
                            r.get::<_, Option<String>>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| ServiceError::NotFound(format!("launch '{lid}' not found")))?;

            let (session_key, provider, cwd_opt) = row;
            let cwd_path = cwd_opt.as_deref().map(Path::new).unwrap_or(Path::new(""));

            if !self.config.scope.allows_session_cwd(cwd_path) {
                return Err(ServiceError::ScopeDenied(format!(
                    "launch '{lid}' is outside authorized workspace scope"
                )));
            }

            let turns: i64 = conn.query_row(
                "SELECT COUNT(*) FROM traces WHERE launch_id = ?1",
                params![lid],
                |r| r.get(0),
            )?;

            let tools: i64 = conn.query_row(
                "SELECT COUNT(*) FROM observations o JOIN traces t ON t.id = o.trace_id WHERE t.launch_id = ?1 AND o.type = 'tool'",
                params![lid],
                |r| r.get(0),
            ).unwrap_or(0);

            let (tokens, cost, start_ns, end_ns): (Option<i64>, Option<f64>, Option<i64>, Option<i64>) = conn
                .query_row(
                    "SELECT SUM(total_tokens), SUM(total_cost_usd), MIN(start_ns), MAX(COALESCE(end_ns, start_ns))
                     FROM traces WHERE launch_id = ?1",
                    params![lid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap_or((None, None, None, None));

            let duration_ms = match (start_ns, end_ns) {
                (Some(s), Some(e)) => ((e.saturating_sub(s)) / 1_000_000) as u64,
                _ => 0,
            };

            let initial_goal: Option<String> = conn
                .query_row(
                    "SELECT input FROM traces WHERE launch_id = ?1 AND input IS NOT NULL AND trim(input) != '' ORDER BY start_ns ASC LIMIT 1",
                    params![lid],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .unwrap_or(None)
                .map(|s| super::evidence::snippet(&s, 100));

            Ok(RunMetrics {
                launch_id: lid.to_string(),
                session_key,
                provider,
                cwd: cwd_path.to_string_lossy().to_string(),
                turns,
                tools,
                tokens,
                cost_usd: cost,
                duration_ms,
                initial_goal,
            })
        };

        let run_a = load_run(&args.a)?;
        let run_b = load_run(&args.b)?;

        let mut warnings = Vec::new();
        if run_a.provider != run_b.provider {
            warnings.push(format!(
                "Comparing different providers: {:?} vs {:?}",
                run_a.provider, run_b.provider
            ));
        }

        let deltas = RunDeltas {
            delta_turns: run_b.turns - run_a.turns,
            delta_tools: run_b.tools - run_a.tools,
            delta_tokens: match (run_a.tokens, run_b.tokens) {
                (Some(a_tok), Some(b_tok)) => Some(b_tok - a_tok),
                _ => None,
            },
            delta_cost_usd: match (run_a.cost_usd, run_b.cost_usd) {
                (Some(a_cost), Some(b_cost)) => Some(b_cost - a_cost),
                _ => None,
            },
            delta_duration_ms: (run_b.duration_ms as i64) - (run_a.duration_ms as i64),
        };

        let data = CompareRunsData {
            run_a,
            run_b,
            deltas,
            comparability_warnings: warnings,
        };

        Ok(Envelope {
            schema_version: 1,
            as_of,
            scope: scope_info,
            window: None,
            data: serde_json::to_value(data).map_err(|e| ServiceError::Internal(e.to_string()))?,
            coverage: CoverageInfo {
                status: CoverageStatus::Full,
                reasons: Vec::new(),
            },
            warnings: Vec::new(),
            next_cursor: None,
            truncated: false,
        })
    }
}

/// Parses and validates time window parameters into nanoseconds, enforcing RFC3339.
fn parse_window(
    since: Option<&str>,
    until: Option<&str>,
    now: OffsetDateTime,
) -> Result<(i64, i64, WindowInfo), ServiceError> {
    let until_dt = if let Some(u) = until {
        OffsetDateTime::parse(u, &Rfc3339).map_err(|_| {
            ServiceError::InvalidArgument(format!("invalid RFC3339 timestamp for 'until': '{u}'"))
        })?
    } else {
        now
    };

    let since_dt = if let Some(s) = since {
        OffsetDateTime::parse(s, &Rfc3339).map_err(|_| {
            ServiceError::InvalidArgument(format!("invalid RFC3339 timestamp for 'since': '{s}'"))
        })?
    } else {
        until_dt - time::Duration::hours(24)
    };

    if since_dt > until_dt {
        return Err(ServiceError::InvalidArgument(
            "'since' must be earlier than or equal to 'until'".into(),
        ));
    }

    let since_str = since_dt
        .format(&Rfc3339)
        .map_err(|e| ServiceError::Internal(e.to_string()))?;
    let until_str = until_dt
        .format(&Rfc3339)
        .map_err(|e| ServiceError::Internal(e.to_string()))?;

    let since_ns = (since_dt.unix_timestamp_nanos() / 1) as i64;
    let until_ns = (until_dt.unix_timestamp_nanos() / 1) as i64;

    Ok((
        since_ns,
        until_ns,
        WindowInfo {
            since: since_str,
            until: until_str,
        },
    ))
}

fn bound_envelope(envelope: &mut Envelope<serde_json::Value>, max_bytes: usize) {
    let serialized_len = match serde_json::to_vec(envelope) {
        Ok(v) => v.len(),
        Err(_) => return,
    };
    if serialized_len <= max_bytes {
        return;
    }

    envelope.truncated = true;
    if !envelope.warnings.iter().any(|w| w.contains("64 KiB")) {
        envelope.warnings.push("response truncated to fit 64 KiB limit".into());
    }

    // Step 1: Truncate large string values in envelope.data down to 100 chars
    truncate_json_strings(&mut envelope.data, 100);
    if serde_json::to_vec(envelope).map(|v| v.len()).unwrap_or(0) <= max_bytes {
        return;
    }

    // Step 2: Truncate large string values down to 40 chars
    truncate_json_strings(&mut envelope.data, 40);
    if serde_json::to_vec(envelope).map(|v| v.len()).unwrap_or(0) <= max_bytes {
        return;
    }

    // Step 3: Truncate array items in envelope.data
    if let serde_json::Value::Object(map) = &mut envelope.data {
        for (_k, v) in map.iter_mut() {
            if let serde_json::Value::Array(arr) = v {
                while arr.len() > 1 {
                    arr.pop();
                }
            }
        }
    }

    // Step 4: If single huge record, heavily truncate strings down to 20 chars
    if serde_json::to_vec(envelope).map(|v| v.len()).unwrap_or(0) > max_bytes {
        truncate_json_strings(&mut envelope.data, 20);
    }
}

fn truncate_json_strings(val: &mut serde_json::Value, max_len: usize) {
    match val {
        serde_json::Value::String(s) => {
            if s.len() > max_len {
                *s = super::evidence::snippet(s, max_len);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                truncate_json_strings(item, max_len);
            }
        }
        serde_json::Value::Object(obj) => {
            for (_, v) in obj {
                truncate_json_strings(v, max_len);
            }
        }
        _ => {}
    }
}
