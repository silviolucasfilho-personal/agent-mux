//! Claude Code managed process adapter.

use super::{ManagedAdapter, ManagedCapabilities, ManagedError, ManagedRequest, ManagedSession};
use std::future::Future;
use std::pin::Pin;

pub struct ClaudeManagedAdapter;

impl ClaudeManagedAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClaudeManagedAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ManagedAdapter for ClaudeManagedAdapter {
    fn capabilities(&self) -> ManagedCapabilities {
        ManagedCapabilities {
            cancel: true,
            max_turns: true,
            token_budget: false,
            cost_budget: false,
            resume: true,
        }
    }

    fn start(
        &self,
        request: ManagedRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ManagedSession, ManagedError>> + Send>> {
        let caps = self.capabilities();
        Box::pin(async move {
            crate::agent::budgets::validate_budget(&caps, &request.budget)
                .map_err(|e| ManagedError::BudgetExceeded(e.0))?;
            Ok(ManagedSession::empty())
        })
    }
}
