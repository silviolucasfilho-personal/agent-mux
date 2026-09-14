//! Declarative agent triggers and event matching.

use crate::agent::budgets::Budget;
use crate::agent::state::AgentScope;
use crate::tracing::store::Change;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    ProcessExit,
    WaitingForUser,
    RepeatedError,
    CollectorStale,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProcessExit => "process_exit",
            Self::WaitingForUser => "waiting_for_user",
            Self::RepeatedError => "repeated_error",
            Self::CollectorStale => "collector_stale",
        }
    }
}

fn default_debounce_ms() -> u64 {
    5000
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TriggerDefinition {
    pub event: EventKind,
    pub prompt: String,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
}

fn default_max_jobs_per_hour() -> u32 {
    3
}
fn default_max_concurrent_jobs() -> u32 {
    1
}
fn default_timeout_seconds() -> u64 {
    120
}
fn default_max_turns() -> u32 {
    3
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MonitoringConfig {
    #[serde(default)]
    pub automatic: bool,
    #[serde(default = "default_max_jobs_per_hour")]
    pub max_jobs_per_hour: u32,
    #[serde(default = "default_max_concurrent_jobs")]
    pub max_concurrent_jobs: u32,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub max_cost_usd: Option<f64>,
}

impl PartialEq for MonitoringConfig {
    fn eq(&self, other: &Self) -> bool {
        self.automatic == other.automatic
            && self.max_jobs_per_hour == other.max_jobs_per_hour
            && self.max_concurrent_jobs == other.max_concurrent_jobs
            && self.timeout_seconds == other.timeout_seconds
            && self.max_turns == other.max_turns
            && self.max_tokens == other.max_tokens
            && self.max_cost_usd.map(|f| f.to_bits()) == other.max_cost_usd.map(|f| f.to_bits())
    }
}

impl Eq for MonitoringConfig {}

impl Default for MonitoringConfig {
    fn default() -> Self {
        Self {
            automatic: false,
            max_jobs_per_hour: 3,
            max_concurrent_jobs: 1,
            timeout_seconds: 120,
            max_turns: 3,
            max_tokens: None,
            max_cost_usd: None,
        }
    }
}

/// Prevents an agent from triggering on its own actions or those of its subagents.
pub fn should_trigger(
    observer: &str,
    origin_agent: Option<&str>,
    ancestor_agents: &[String],
) -> bool {
    if let Some(origin) = origin_agent {
        if origin == observer {
            return false;
        }
    }
    if ancestor_agents.iter().any(|a| a == observer) {
        return false;
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerEvent {
    pub event: EventKind,
    pub session_key: Option<String>,
    pub launch_id: Option<String>,
    pub evidence_seq: i64,
    pub description: String,
    pub evidence_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct JobRequest {
    pub scope: AgentScope,
    pub dedupe_key: String,
    pub prompt: String,
    pub evidence_revision: i64,
    pub budget: Budget,
}

/// Evaluates committed changes against source-defined triggers to detect trigger events.
pub fn evaluate_changes(changes: &[Change], triggers: &[TriggerDefinition]) -> Vec<TriggerEvent> {
    let mut events = Vec::new();

    for trg in triggers {
        match trg.event {
            EventKind::ProcessExit => {
                for c in changes {
                    if (c.entity_kind == "session" || c.entity_kind == "launch")
                        && (c.operation == "update" || c.operation == "delete")
                    {
                        events.push(TriggerEvent {
                            event: EventKind::ProcessExit,
                            session_key: c.session_key.clone(),
                            launch_id: c.launch_id.clone(),
                            evidence_seq: c.seq,
                            description: format!(
                                "Process exited for {} {}",
                                c.entity_kind, c.entity_id
                            ),
                            evidence_ids: vec![c.entity_id.clone()],
                        });
                    }
                }
            }
            EventKind::RepeatedError => {
                let error_obs: Vec<_> = changes
                    .iter()
                    .filter(|c| c.entity_kind == "observation" && c.operation == "update")
                    .collect();
                if error_obs.len() >= 3 {
                    let last = error_obs.last().unwrap();
                    events.push(TriggerEvent {
                        event: EventKind::RepeatedError,
                        session_key: last.session_key.clone(),
                        launch_id: last.launch_id.clone(),
                        evidence_seq: last.seq,
                        description: format!(
                            "Repeated errors detected (count: {})",
                            error_obs.len()
                        ),
                        evidence_ids: error_obs.iter().map(|c| c.entity_id.clone()).collect(),
                    });
                }
            }
            EventKind::WaitingForUser => {
                for c in changes {
                    if c.entity_kind == "trace" && c.operation == "update" {
                        events.push(TriggerEvent {
                            event: EventKind::WaitingForUser,
                            session_key: c.session_key.clone(),
                            launch_id: c.launch_id.clone(),
                            evidence_seq: c.seq,
                            description: format!(
                                "Session waiting for user in trace {}",
                                c.entity_id
                            ),
                            evidence_ids: vec![c.entity_id.clone()],
                        });
                    }
                }
            }
            EventKind::CollectorStale => {
                // Emitted if collector drops or stalls
            }
        }
    }

    events
}
