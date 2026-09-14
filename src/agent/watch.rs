//! Generic background watcher and event-triggered investigation scheduler.

use crate::agent::budgets::Budget;
use crate::agent::definition::AgentDefinition;
use crate::agent::managed::{ManagedAdapter, ManagedError, ManagedEvent, ManagedRequest};
use crate::agent::state::{AgentScope, JobStatus, StateError, StateStore};
use crate::agent::triggers::evaluate_changes;
use crate::tracing::store::read_changes;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug)]
pub enum WatchError {
    State(StateError),
    TraceStore(rusqlite::Error),
    Managed(ManagedError),
    Io(std::io::Error),
    LeaseAcquisitionFailed(String),
    Other(String),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::State(e) => write!(f, "State error: {e}"),
            Self::TraceStore(e) => write!(f, "Trace store error: {e}"),
            Self::Managed(e) => write!(f, "Managed adapter error: {e}"),
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::LeaseAcquisitionFailed(msg) => write!(f, "Lease acquisition failed: {msg}"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for WatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::State(e) => Some(e),
            Self::TraceStore(e) => Some(e),
            Self::Managed(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<StateError> for WatchError {
    fn from(e: StateError) -> Self {
        Self::State(e)
    }
}

impl From<rusqlite::Error> for WatchError {
    fn from(e: rusqlite::Error) -> Self {
        Self::TraceStore(e)
    }
}

impl From<ManagedError> for WatchError {
    fn from(e: ManagedError) -> Self {
        Self::Managed(e)
    }
}

impl From<std::io::Error> for WatchError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Debug, Clone)]
pub struct WatcherConfig {
    pub poll_interval_ms: u64,
    pub batch_limit: usize,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 1000,
            batch_limit: 100,
        }
    }
}

pub struct Watcher {
    pub scope: AgentScope,
    pub definition: AgentDefinition,
    pub trace_db_path: PathBuf,
    pub state_db_path: PathBuf,
    pub adapter: Arc<dyn ManagedAdapter>,
    pub config: WatcherConfig,
    executed_jobs: usize,
    job_launches_this_hour: Vec<i64>,
}

impl Watcher {
    pub fn new(
        scope: AgentScope,
        definition: AgentDefinition,
        trace_db_path: PathBuf,
        state_db_path: PathBuf,
        adapter: Arc<dyn ManagedAdapter>,
        config: WatcherConfig,
    ) -> Result<Self, WatchError> {
        Ok(Self {
            scope,
            definition,
            trace_db_path,
            state_db_path,
            adapter,
            config,
            executed_jobs: 0,
            job_launches_this_hour: Vec::new(),
        })
    }

    pub fn executed_jobs_count(&self) -> usize {
        self.executed_jobs
    }

    /// Performs one tick of the watcher loop: reads committed changes,
    /// evaluates source triggers, and executes due bounded jobs if automatic mode is on.
    pub async fn tick(&mut self, now_ns: i64) -> Result<(), WatchError> {
        let trace_conn = match Connection::open(&self.trace_db_path) {
            Ok(c) => c,
            Err(e) => return Err(WatchError::TraceStore(e)),
        };

        let mut state = StateStore::open(&self.state_db_path)?;
        let cursor = state.consumption_cursor(&self.scope)?;

        let changes = match read_changes(&trace_conn, cursor, self.config.batch_limit) {
            Ok(c) => c,
            Err(e) => return Err(WatchError::TraceStore(e)),
        };

        if changes.is_empty() {
            return Ok(());
        }

        let new_cursor = changes.last().unwrap().seq;
        state.set_consumption_cursor(&self.scope, new_cursor)?;

        let events = evaluate_changes(&changes, &self.definition.triggers);
        if events.is_empty() || !self.definition.monitoring.automatic {
            return Ok(());
        }

        // Sliding one-hour rate limit prune
        let one_hour_ns = 3_600 * 1_000_000_000i64;
        self.job_launches_this_hour
            .retain(|&ts| now_ns.saturating_sub(ts) < one_hour_ns);

        let max_jobs = self.definition.monitoring.max_jobs_per_hour as usize;
        let max_concurrent = self.definition.monitoring.max_concurrent_jobs as usize;
        let mut concurrent = 0;

        for ev in events {
            if self.job_launches_this_hour.len() >= max_jobs || concurrent >= max_concurrent {
                break;
            }

            let dedupe_key = format!(
                "{}:{}:{}",
                self.scope.scope_key(),
                ev.event.as_str(),
                ev.evidence_seq
            );

            // Check if job already exists
            if state.get_job_status(&dedupe_key).is_ok() {
                continue;
            }

            let budget = Budget {
                max_turns: self.definition.monitoring.max_turns,
                timeout_seconds: self.definition.monitoring.timeout_seconds,
                max_tokens: self.definition.monitoring.max_tokens,
                max_cost_usd: self.definition.monitoring.max_cost_usd,
            };

            let job_id = state.create_job(
                &self.scope,
                &dedupe_key,
                ev.evidence_seq,
                JobStatus::Running,
            )?;

            let req = ManagedRequest {
                agent_id: self.scope.agent_id.clone(),
                prompt: ev.description.clone(),
                source_hash: self.definition.source_hash.clone(),
                budget,
                native_session_id: None,
            };

            let mut session = match self.adapter.start(req).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = state.update_job_status(&job_id, JobStatus::Failed);
                    return Err(WatchError::Managed(e));
                }
            };

            let mut final_output = String::new();
            let mut failed = false;

            while let Some(mev) = session.next_event().await? {
                match mev {
                    ManagedEvent::Completed { output } => {
                        final_output = output;
                    }
                    ManagedEvent::Failed { error } => {
                        final_output = error;
                        failed = true;
                    }
                    _ => {}
                }
            }

            if failed {
                state.update_job_status(&job_id, JobStatus::Failed)?;
            } else {
                state.update_job_status(&job_id, JobStatus::Completed)?;
                state.record_briefing(
                    &self.scope,
                    ev.evidence_seq,
                    &final_output,
                    &ev.evidence_ids,
                )?;
            }

            self.executed_jobs += 1;
            self.job_launches_this_hour.push(now_ns);
            concurrent += 1;
        }

        Ok(())
    }
}
