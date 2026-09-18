//! Heimdall startup dossier builder (schema version 2).
//!
//! One single read-only SQLite connection, single definition scan, single
//! live-snapshot read, isolated section errors with typed coverage statuses,
//! and bounded 64 KiB response.

use crate::tracing::analysis::agents::{AgentDossier, analyze_agents};
use crate::tracing::analysis::model::{AnalysisError, LiveSession, SessionCard};
use crate::tracing::analysis::query::briefing;
use crate::tracing::analysis::skills::{SkillDossier, analyze_skills_for_dossier};
use crate::tracing::inventory;
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Canonical schema-v2 startup dossier for Heimdall.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HeimdallDossier {
    pub schema_version: u32,
    pub as_of: String,
    pub scope: DossierScope,
    pub windows: DossierWindows,
    pub health: DossierHealth,
    pub sessions: DossierSection<SessionDossier>,
    pub skills: DossierSection<SkillDossier>,
    pub agents: DossierSection<AgentDossier>,
}

/// Scope definition of the dossier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct DossierScope {
    pub workspace: PathBuf,
}

/// Time window pair for sessions and evaluations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct DossierWindows {
    pub sessions: DossierWindow,
    pub evaluations: DossierWindow,
}

/// Single time window bounds in RFC 3339 format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct DossierWindow {
    pub since: String,
    pub until: String,
}

/// Store health, coverage, content mode, and timing telemetry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct DossierHealth {
    pub reader_schema_version: i32,
    pub store_schema_version: i32,
    pub db_path: String,
    pub content_mode: String,
    pub live_snapshot_available: bool,
    pub provider_coverage: Vec<String>,
    pub priced_generations: Option<i64>,
    pub unpriced_generations: Option<i64>,
    pub total_build_ms: u64,
    pub sessions_build_ms: u64,
    pub skills_build_ms: u64,
    pub agents_build_ms: u64,
    pub bytes: usize,
    pub max_session_cards: usize,
    pub max_skill_rows: usize,
    pub max_agent_rows: usize,
    pub examples_per_category: usize,
}

/// Coverage status of a dossier section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    #[default]
    Full,
    Partial,
    Unavailable,
}

/// Typed section wrapper with independent coverage, warnings, errors, and truncation metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct DossierSection<T> {
    pub status: CoverageStatus,
    pub warnings: Vec<String>,
    pub errors: Vec<DossierError>,
    pub truncated: bool,
    pub total_matching: usize,
    pub data: T,
}

impl<T: Default> DossierSection<T> {
    pub fn unavailable(code: &str, message: impl Into<String>) -> Self {
        Self {
            status: CoverageStatus::Unavailable,
            warnings: Vec::new(),
            errors: vec![DossierError::new(code, message)],
            truncated: false,
            total_matching: 0,
            data: T::default(),
        }
    }
}

/// Section-level error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DossierError {
    pub code: String,
    pub message: String,
}

impl DossierError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// 24-hour session briefing data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct SessionDossier {
    pub sessions: Vec<SessionCard>,
    pub total_sessions: usize,
    pub total_turns: i64,
    pub total_tools: i64,
    pub total_tokens: Option<i64>,
    pub total_cost_usd: Option<f64>,
}

/// Limits and timing configuration for dossier building.
#[derive(Debug, Clone)]
pub struct DossierConfig {
    pub session_window: Duration,
    pub evaluation_window: Duration,
    pub max_bytes: usize,
    pub max_session_cards: usize,
    pub max_skill_rows: usize,
    pub max_agent_rows: usize,
    pub examples_per_category: usize,
    pub deadline: Duration,
}

impl Default for DossierConfig {
    fn default() -> Self {
        Self {
            session_window: Duration::from_secs(24 * 3600),
            evaluation_window: Duration::from_secs(30 * 24 * 3600),
            max_bytes: 64 * 1024,
            max_session_cards: 20,
            max_skill_rows: 30,
            max_agent_rows: 30,
            examples_per_category: 3,
            deadline: Duration::from_secs(5),
        }
    }
}

/// Inputs provided to the dossier builder.
pub struct DossierInputs<'a> {
    pub conn: &'a Connection,
    pub db_path: &'a Path,
    pub workspace: &'a Path,
    pub home: &'a Path,
    pub live_sessions: &'a [LiveSession],
    pub live_snapshot_available: bool,
    pub as_of: OffsetDateTime,
}

/// Top-level builder errors.
#[derive(Debug)]
pub enum DossierBuildError {
    InvalidScope(String),
    SchemaUnsupported(String),
    TooLarge(usize),
    Sqlite(rusqlite::Error),
    Analysis(AnalysisError),
}

impl std::fmt::Display for DossierBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DossierBuildError::InvalidScope(s) => write!(f, "invalid scope: {s}"),
            DossierBuildError::SchemaUnsupported(s) => write!(f, "schema unsupported: {s}"),
            DossierBuildError::TooLarge(sz) => write!(f, "dossier exceeds byte limit ({sz} bytes)"),
            DossierBuildError::Sqlite(e) => write!(f, "database error: {e}"),
            DossierBuildError::Analysis(e) => write!(f, "analysis error: {e}"),
        }
    }
}

impl std::error::Error for DossierBuildError {}

impl From<rusqlite::Error> for DossierBuildError {
    fn from(e: rusqlite::Error) -> Self {
        DossierBuildError::Sqlite(e)
    }
}

impl From<AnalysisError> for DossierBuildError {
    fn from(e: AnalysisError) -> Self {
        DossierBuildError::Analysis(e)
    }
}

fn format_rfc3339(dt: OffsetDateTime) -> String {
    dt.format(&Rfc3339).unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

/// Executes one section closure, isolating any failure to that section.
pub(crate) fn orchestrate_section<T: Default, F>(
    f: F,
) -> (DossierSection<T>, u64)
where
    F: FnOnce() -> Result<(T, usize, Vec<String>, bool), AnalysisError>,
{
    let start = Instant::now();
    let result = f();
    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok((data, total_matching, warnings, truncated)) => {
            let status = if warnings.is_empty() {
                CoverageStatus::Full
            } else {
                CoverageStatus::Partial
            };
            (
                DossierSection {
                    status,
                    warnings,
                    errors: Vec::new(),
                    truncated,
                    total_matching,
                    data,
                },
                elapsed_ms,
            )
        }
        Err(err) => {
            let code = match &err {
                AnalysisError::Sqlite(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::OperationInterrupted =>
                {
                    "QUERY_TIMEOUT"
                }
                _ => "SECTION_QUERY_FAILED",
            };
            (
                DossierSection {
                    status: CoverageStatus::Unavailable,
                    warnings: Vec::new(),
                    errors: vec![DossierError::new(code, err.to_string())],
                    truncated: false,
                    total_matching: 0,
                    data: T::default(),
                },
                elapsed_ms,
            )
        }
    }
}

/// Builds the schema-v2 Heimdall startup dossier from the provided inputs.
pub fn build_dossier(
    inputs: DossierInputs<'_>,
    config: DossierConfig,
) -> Result<HeimdallDossier, DossierBuildError> {
    let build_start = Instant::now();

    // 1. Validate scope
    if inputs.workspace.as_os_str().is_empty() {
        return Err(DossierBuildError::InvalidScope(
            "workspace path cannot be empty".into(),
        ));
    }

    // 2. Check store schema version
    let user_version: i32 = inputs
        .conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap_or(0);

    if user_version > crate::tracing::store::schema::SCHEMA_VERSION {
        return Err(DossierBuildError::SchemaUnsupported(format!(
            "store schema version {user_version} is newer than supported version {}",
            crate::tracing::store::schema::SCHEMA_VERSION
        )));
    }

    // 3. Compute window bounds
    let as_of_str = format_rfc3339(inputs.as_of);
    let session_since = inputs.as_of - config.session_window;
    let eval_since = inputs.as_of - config.evaluation_window;

    let windows = DossierWindows {
        sessions: DossierWindow {
            since: format_rfc3339(session_since),
            until: as_of_str.clone(),
        },
        evaluations: DossierWindow {
            since: format_rfc3339(eval_since),
            until: as_of_str.clone(),
        },
    };

    let as_of_ns = inputs.as_of.unix_timestamp_nanos() as i64;
    let session_since_ns = session_since.unix_timestamp_nanos() as i64;
    let eval_since_ns = eval_since.unix_timestamp_nanos() as i64;

    // 4. Scan inventory definitions once
    let definitions = inventory::inventory_all(inputs.workspace, inputs.home);

    // 5. Build Sessions section
    let (sessions_section, sessions_ms) = orchestrate_section(|| {
        let b = briefing(
            inputs.conn,
            inputs.workspace,
            session_since_ns,
            as_of_ns,
            inputs.live_sessions,
        )?;
        let total_matching = b.cards.len();
        let truncated = b.cards.len() > config.max_session_cards;
        let mut cards = b.cards;
        cards.truncate(config.max_session_cards);

        let data = SessionDossier {
            sessions: cards,
            total_sessions: b.total_sessions,
            total_turns: b.total_turns,
            total_tools: b.total_tools,
            total_tokens: b.total_tokens,
            total_cost_usd: b.total_cost_usd,
        };
        Ok((data, total_matching, b.warnings, truncated))
    });

    // 6. Build Skills section
    let (skills_section, skills_ms) = orchestrate_section(|| {
        let mut d = analyze_skills_for_dossier(
            inputs.conn,
            inputs.workspace,
            eval_since_ns,
            as_of_ns,
            &definitions,
            config.examples_per_category,
        )?;
        let total_matching = d.skills.len();
        let truncated = d.skills.len() > config.max_skill_rows;
        d.skills.truncate(config.max_skill_rows);
        Ok((d, total_matching, Vec::new(), truncated))
    });

    // 7. Build Agents section
    let (agents_section, agents_ms) = orchestrate_section(|| {
        let mut d = analyze_agents(
            inputs.conn,
            inputs.workspace,
            eval_since_ns,
            as_of_ns,
            &definitions,
            config.examples_per_category,
        )?;
        let total_matching = d.agents.len();
        let truncated = d.agents.len() > config.max_agent_rows;
        d.agents.truncate(config.max_agent_rows);
        Ok((d, total_matching, Vec::new(), truncated))
    });

    // 8. Health telemetry
    let content_mode = inputs
        .conn
        .query_row(
            "SELECT content_mode FROM launches WHERE started_ns >= ?1 ORDER BY started_ns DESC LIMIT 1",
            [session_since_ns],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "full".into());

    let provider_coverage: Vec<String> = {
        let stmt = inputs
            .conn
            .prepare("SELECT DISTINCT provider FROM sessions WHERE last_seen_ns >= ?1 AND provider IS NOT NULL")
            .ok();
        if let Some(mut s) = stmt {
            s.query_map([session_since_ns], |r| r.get::<_, String>(0))
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        }
    };

    let total_build_ms = build_start.elapsed().as_millis() as u64;

    let health = DossierHealth {
        reader_schema_version: crate::tracing::store::schema::SCHEMA_VERSION,
        store_schema_version: user_version,
        db_path: inputs.db_path.display().to_string(),
        content_mode,
        live_snapshot_available: inputs.live_snapshot_available,
        provider_coverage,
        priced_generations: None,
        unpriced_generations: None,
        total_build_ms,
        sessions_build_ms: sessions_ms,
        skills_build_ms: skills_ms,
        agents_build_ms: agents_ms,
        bytes: 0,
        max_session_cards: config.max_session_cards,
        max_skill_rows: config.max_skill_rows,
        max_agent_rows: config.max_agent_rows,
        examples_per_category: config.examples_per_category,
    };

    let mut dossier = HeimdallDossier {
        schema_version: 2,
        as_of: as_of_str,
        scope: DossierScope {
            workspace: inputs.workspace.to_path_buf(),
        },
        windows,
        health,
        sessions: sessions_section,
        skills: skills_section,
        agents: agents_section,
    };

    // Calculate preliminary serialized size
    if let Ok(bytes) = serde_json::to_vec(&dossier) {
        dossier.health.bytes = bytes.len();
    }

    Ok(dossier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_isolation_marks_failing_section_unavailable_and_preserves_others() {
        let (sess_sec, _): (DossierSection<SessionDossier>, _) = orchestrate_section(|| {
            Ok((
                SessionDossier {
                    total_sessions: 1,
                    ..Default::default()
                },
                1,
                vec![],
                false,
            ))
        });
        assert_eq!(sess_sec.status, CoverageStatus::Full);
        assert_eq!(sess_sec.data.total_sessions, 1);

        let (skill_sec, _): (DossierSection<SkillDossier>, _) = orchestrate_section(|| {
            Ok((
                SkillDossier {
                    total_observed: 2,
                    ..Default::default()
                },
                2,
                vec![],
                false,
            ))
        });
        assert_eq!(skill_sec.status, CoverageStatus::Full);
        assert_eq!(skill_sec.data.total_observed, 2);

        let (agent_sec, _): (DossierSection<AgentDossier>, _) = orchestrate_section(|| {
            Err(AnalysisError::Correlation("simulated failure".into()))
        });
        assert_eq!(agent_sec.status, CoverageStatus::Unavailable);
        assert_eq!(agent_sec.errors.len(), 1);
        assert_eq!(agent_sec.errors[0].code, "SECTION_QUERY_FAILED");
        assert!(agent_sec.errors[0].message.contains("simulated failure"));
        assert_eq!(agent_sec.data, AgentDossier::default());
    }
}
