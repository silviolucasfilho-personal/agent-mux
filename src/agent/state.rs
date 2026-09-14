//! Review state, briefings, findings, and job records persisted in `agent-state.db`.
//!
//! State is stored independently of traces (`traces.db`). Records are namespaced by
//! agent ID, source path, and workspace.

use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

#[derive(Debug)]
pub enum StateError {
    Database(rusqlite::Error),
    NotFound(String),
    ScopeConflict { expected: String, actual: String },
    Io(std::io::Error),
    Other(String),
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(e) => write!(f, "Database error: {e}"),
            Self::NotFound(msg) => write!(f, "Record not found: {msg}"),
            Self::ScopeConflict { expected, actual } => {
                write!(f, "Scope conflict: expected {expected}, found {actual}")
            }
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for StateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for StateError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e)
    }
}

impl From<std::io::Error> for StateError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentScope {
    pub agent_id: String,
    pub source_path: PathBuf,
    pub workspace: PathBuf,
}

impl AgentScope {
    pub fn scope_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.agent_id,
            self.source_path.display(),
            self.workspace.display()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Interrupted,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "interrupted" => Some(Self::Interrupted),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Open,
    Acknowledged,
    Resolved,
}

impl FindingStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Acknowledged => "acknowledged",
            Self::Resolved => "resolved",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "acknowledged" => Some(Self::Acknowledged),
            "resolved" => Some(Self::Resolved),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BriefingRecord {
    pub id: String,
    pub scope_key: String,
    pub agent_id: String,
    pub source_path: PathBuf,
    pub workspace: PathBuf,
    pub source_hash: Option<String>,
    pub trace_store_uuid: Option<String>,
    pub through_seq: i64,
    pub text: String,
    pub evidence_ids: Vec<String>,
    pub acknowledged: bool,
    pub acknowledged_at_ns: Option<i64>,
    pub created_at_ns: i64,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub id: String,
    pub scope_key: String,
    pub agent_id: String,
    pub source_path: PathBuf,
    pub workspace: PathBuf,
    pub session_key: Option<String>,
    pub event_kind: String,
    pub classification: String,
    pub title: String,
    pub description: String,
    pub evidence_ids: Vec<String>,
    pub first_seq: i64,
    pub last_seq: i64,
    pub status: FindingStatus,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub scope_key: String,
    pub agent_id: String,
    pub source_path: PathBuf,
    pub workspace: PathBuf,
    pub dedupe_key: String,
    pub evidence_revision: i64,
    pub status: JobStatus,
    pub attempts: u32,
    pub native_session_id: Option<String>,
    pub budget_usage: Option<String>,
    pub created_at_ns: i64,
    pub started_at_ns: Option<i64>,
    pub ended_at_ns: Option<i64>,
}

pub struct StateStore {
    conn: Connection,
}

impl StateStore {
    /// Opens or creates the state database at `path`.
    pub fn open(path: &Path) -> Result<Self, StateError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;",
        )?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS review_cursors (
               scope_key TEXT PRIMARY KEY,
               agent_id TEXT NOT NULL,
               source_path TEXT NOT NULL,
               workspace TEXT NOT NULL,
               trace_store_uuid TEXT,
               reviewed_through INTEGER NOT NULL DEFAULT 0,
               consumption_cursor INTEGER NOT NULL DEFAULT 0,
               updated_at_ns INTEGER NOT NULL
             );

             CREATE TABLE IF NOT EXISTS briefings (
               id TEXT PRIMARY KEY,
               scope_key TEXT NOT NULL,
               agent_id TEXT NOT NULL,
               source_path TEXT NOT NULL,
               workspace TEXT NOT NULL,
               source_hash TEXT,
               trace_store_uuid TEXT,
               through_seq INTEGER NOT NULL,
               text TEXT NOT NULL,
               evidence_ids TEXT NOT NULL DEFAULT '[]',
               acknowledged INTEGER NOT NULL DEFAULT 0,
               acknowledged_at_ns INTEGER,
               created_at_ns INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_briefings_scope ON briefings(scope_key);

             CREATE TABLE IF NOT EXISTS findings (
               id TEXT PRIMARY KEY,
               scope_key TEXT NOT NULL,
               agent_id TEXT NOT NULL,
               source_path TEXT NOT NULL,
               workspace TEXT NOT NULL,
               source_hash TEXT,
               trace_store_uuid TEXT,
               session_key TEXT,
               event_kind TEXT NOT NULL,
               classification TEXT NOT NULL,
               title TEXT NOT NULL,
               description TEXT NOT NULL,
               evidence_ids TEXT NOT NULL DEFAULT '[]',
               first_seq INTEGER NOT NULL,
               last_seq INTEGER NOT NULL,
               status TEXT NOT NULL CHECK(status IN ('open','acknowledged','resolved')),
               created_at_ns INTEGER NOT NULL,
               updated_at_ns INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_findings_scope ON findings(scope_key);
             CREATE INDEX IF NOT EXISTS idx_findings_status ON findings(status);

             CREATE TABLE IF NOT EXISTS jobs (
               id TEXT PRIMARY KEY,
               scope_key TEXT NOT NULL,
               agent_id TEXT NOT NULL,
               source_path TEXT NOT NULL,
               workspace TEXT NOT NULL,
               source_hash TEXT,
               trace_store_uuid TEXT,
               dedupe_key TEXT NOT NULL,
               evidence_revision INTEGER NOT NULL,
               status TEXT NOT NULL CHECK(status IN ('pending','running','completed','failed','interrupted','cancelled')),
               attempts INTEGER NOT NULL DEFAULT 0,
               native_session_id TEXT,
               budget_usage TEXT,
               created_at_ns INTEGER NOT NULL,
               started_at_ns INTEGER,
               ended_at_ns INTEGER,
               updated_at_ns INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS idx_jobs_scope ON jobs(scope_key);
             CREATE INDEX IF NOT EXISTS idx_jobs_dedupe ON jobs(dedupe_key);
             CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status);",
        )?;

        // Startup recovery: transition running jobs to interrupted (never auto-replay)
        conn.execute(
            "UPDATE jobs SET status = 'interrupted' WHERE status = 'running'",
            [],
        )?;

        Ok(Self { conn })
    }

    /// Access underlying rusqlite connection.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Access mutable rusqlite connection.
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Records a new briefing. Returns the unique briefing ID.
    pub fn record_briefing(
        &mut self,
        scope: &AgentScope,
        through_seq: i64,
        text: &str,
        evidence_ids: &[String],
    ) -> Result<String, StateError> {
        let id: String =
            self.conn
                .query_row("SELECT 'brf_' || lower(hex(randomblob(12)))", [], |r| {
                    r.get(0)
                })?;
        let evidence_json =
            serde_json::to_string(evidence_ids).unwrap_or_else(|_| "[]".to_string());
        let created_ns = now_ns();
        let scope_key = scope.scope_key();

        self.conn.execute(
            "INSERT INTO briefings(id, scope_key, agent_id, source_path, workspace, through_seq, text, evidence_ids, acknowledged, created_at_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)",
            params![
                id,
                scope_key,
                scope.agent_id,
                scope.source_path.display().to_string(),
                scope.workspace.display().to_string(),
                through_seq,
                text,
                evidence_json,
                created_ns
            ],
        )?;

        Ok(id)
    }

    /// Acknowledges a briefing, advancing the review cursor to max(current, briefing.through_seq).
    pub fn acknowledge(&mut self, scope: &AgentScope, briefing_id: &str) -> Result<(), StateError> {
        let tx = self.conn.transaction()?;

        let (briefing_scope_key, through_seq): (String, i64) = tx
            .query_row(
                "SELECT scope_key, through_seq FROM briefings WHERE id = ?1",
                params![briefing_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StateError::NotFound(format!("briefing {briefing_id}")))?;

        let expected_key = scope.scope_key();
        if briefing_scope_key != expected_key {
            return Err(StateError::ScopeConflict {
                expected: expected_key,
                actual: briefing_scope_key,
            });
        }

        let now = now_ns();
        tx.execute(
            "UPDATE briefings SET acknowledged = 1, acknowledged_at_ns = ?1 WHERE id = ?2",
            params![now, briefing_id],
        )?;

        let current_reviewed: i64 = tx
            .query_row(
                "SELECT reviewed_through FROM review_cursors WHERE scope_key = ?1",
                params![expected_key],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);

        let new_reviewed = current_reviewed.max(through_seq);

        tx.execute(
            "INSERT INTO review_cursors (scope_key, agent_id, source_path, workspace, reviewed_through, updated_at_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(scope_key) DO UPDATE SET
               reviewed_through = ?5,
               updated_at_ns = ?6",
            params![
                expected_key,
                scope.agent_id,
                scope.source_path.display().to_string(),
                scope.workspace.display().to_string(),
                new_reviewed,
                now
            ],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Returns the sequence number through which reviews have been acknowledged.
    pub fn reviewed_through(&self, scope: &AgentScope) -> Result<i64, StateError> {
        let seq: Option<i64> = self
            .conn
            .query_row(
                "SELECT reviewed_through FROM review_cursors WHERE scope_key = ?1",
                params![scope.scope_key()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(seq.unwrap_or(0))
    }

    /// Returns the internal consumption cursor for watching changes.
    pub fn consumption_cursor(&self, scope: &AgentScope) -> Result<i64, StateError> {
        let seq: Option<i64> = self
            .conn
            .query_row(
                "SELECT consumption_cursor FROM review_cursors WHERE scope_key = ?1",
                params![scope.scope_key()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(seq.unwrap_or(0))
    }

    /// Sets the internal consumption cursor.
    pub fn set_consumption_cursor(
        &mut self,
        scope: &AgentScope,
        cursor: i64,
    ) -> Result<(), StateError> {
        let now = now_ns();
        let key = scope.scope_key();
        self.conn.execute(
            "INSERT INTO review_cursors (scope_key, agent_id, source_path, workspace, consumption_cursor, updated_at_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(scope_key) DO UPDATE SET
               consumption_cursor = ?5,
               updated_at_ns = ?6",
            params![
                key,
                scope.agent_id,
                scope.source_path.display().to_string(),
                scope.workspace.display().to_string(),
                cursor,
                now
            ],
        )?;
        Ok(())
    }

    /// Creates a new job record. Returns the job ID.
    pub fn create_job(
        &mut self,
        scope: &AgentScope,
        dedupe_key: &str,
        evidence_revision: i64,
        initial_status: JobStatus,
    ) -> Result<String, StateError> {
        let id: String =
            self.conn
                .query_row("SELECT 'job_' || lower(hex(randomblob(12)))", [], |r| {
                    r.get(0)
                })?;
        let now = now_ns();
        let key = scope.scope_key();

        self.conn.execute(
            "INSERT INTO jobs (id, scope_key, agent_id, source_path, workspace, dedupe_key, evidence_revision, status, attempts, created_at_ns, updated_at_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?9)",
            params![
                id,
                key,
                scope.agent_id,
                scope.source_path.display().to_string(),
                scope.workspace.display().to_string(),
                dedupe_key,
                evidence_revision,
                initial_status.as_str(),
                now
            ],
        )?;
        Ok(id)
    }

    /// Queries the status of a job by ID or dedupe key.
    pub fn get_job_status(&self, id_or_dedupe: &str) -> Result<JobStatus, StateError> {
        let status_str: String = self
            .conn
            .query_row(
                "SELECT status FROM jobs WHERE id = ?1 OR dedupe_key = ?1 ORDER BY created_at_ns DESC LIMIT 1",
                params![id_or_dedupe],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| StateError::NotFound(format!("job {id_or_dedupe}")))?;

        JobStatus::parse(&status_str)
            .ok_or_else(|| StateError::Other(format!("unknown job status: {status_str}")))
    }

    /// Updates the status of a job.
    pub fn update_job_status(&mut self, job_id: &str, status: JobStatus) -> Result<(), StateError> {
        let now = now_ns();
        let affected = self.conn.execute(
            "UPDATE jobs SET status = ?1, updated_at_ns = ?2 WHERE id = ?3",
            params![status.as_str(), now, job_id],
        )?;
        if affected == 0 {
            return Err(StateError::NotFound(format!("job {job_id}")));
        }
        Ok(())
    }
}
