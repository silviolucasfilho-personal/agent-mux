//! Bounded managed harness process adapters.

pub mod agy;
pub mod claude;
pub mod codex;

pub use agy::AgyManagedAdapter;
pub use claude::ClaudeManagedAdapter;
pub use codex::CodexManagedAdapter;

use crate::agent::budgets::{Budget, ManagedCapabilities};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;

#[derive(Debug)]
pub enum ManagedError {
    BudgetExceeded(String),
    ProcessError(String),
    Cancelled,
    Io(std::io::Error),
    Protocol(String),
}

impl std::fmt::Display for ManagedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BudgetExceeded(msg) => write!(f, "Budget exceeded: {msg}"),
            Self::ProcessError(msg) => write!(f, "Process error: {msg}"),
            Self::Cancelled => write!(f, "Managed session cancelled"),
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Protocol(msg) => write!(f, "Protocol error: {msg}"),
        }
    }
}

impl std::error::Error for ManagedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ManagedError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Debug, Clone)]
pub struct ManagedRequest {
    pub agent_id: String,
    pub prompt: String,
    pub source_hash: String,
    pub budget: Budget,
    pub native_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ManagedEvent {
    Started,
    TurnStarted { turn: u32 },
    Usage { tokens: u64, cost_usd: Option<f64> },
    Evidence { id: String },
    Completed { output: String },
    Failed { error: String },
}

pub struct ManagedSession {
    events: VecDeque<ManagedEvent>,
    cancelled: bool,
}

impl ManagedSession {
    pub fn new(events: Vec<ManagedEvent>) -> Self {
        Self {
            events: VecDeque::from(events),
            cancelled: false,
        }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    pub async fn next_event(&mut self) -> Result<Option<ManagedEvent>, ManagedError> {
        if self.cancelled {
            return Err(ManagedError::Cancelled);
        }
        Ok(self.events.pop_front())
    }

    pub async fn cancel(&mut self) -> Result<(), ManagedError> {
        self.cancelled = true;
        Ok(())
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

pub trait ManagedAdapter: Send + Sync {
    fn capabilities(&self) -> ManagedCapabilities;
    fn start(
        &self,
        request: ManagedRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ManagedSession, ManagedError>> + Send>>;
}

/// Scripted in-memory adapter for deterministic scheduling tests.
pub struct ScriptedManagedAdapter {
    events: Vec<ManagedEvent>,
}

impl ScriptedManagedAdapter {
    pub fn new(events: Vec<ManagedEvent>) -> Self {
        Self { events }
    }
}

impl ManagedAdapter for ScriptedManagedAdapter {
    fn capabilities(&self) -> ManagedCapabilities {
        ManagedCapabilities {
            cancel: true,
            max_turns: true,
            token_budget: true,
            cost_budget: true,
            resume: true,
        }
    }

    fn start(
        &self,
        request: ManagedRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ManagedSession, ManagedError>> + Send>> {
        let caps = self.capabilities();
        let events = self.events.clone();
        Box::pin(async move {
            crate::agent::budgets::validate_budget(&caps, &request.budget)
                .map_err(|e| ManagedError::BudgetExceeded(e.0))?;
            Ok(ManagedSession::new(events))
        })
    }
}
