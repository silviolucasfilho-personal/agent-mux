//! Execution budgets and capability checking for managed agents.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Budget {
    pub max_turns: u32,
    pub timeout_seconds: u64,
    pub max_tokens: Option<u64>,
    pub max_cost_usd: Option<f64>,
}

impl PartialEq for Budget {
    fn eq(&self, other: &Self) -> bool {
        self.max_turns == other.max_turns
            && self.timeout_seconds == other.timeout_seconds
            && self.max_tokens == other.max_tokens
            && self.max_cost_usd.map(|f| f.to_bits()) == other.max_cost_usd.map(|f| f.to_bits())
    }
}

impl Eq for Budget {}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_turns: 3,
            timeout_seconds: 120,
            max_tokens: None,
            max_cost_usd: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedCapabilities {
    pub cancel: bool,
    pub max_turns: bool,
    pub token_budget: bool,
    pub cost_budget: bool,
    pub resume: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetError(pub String);

impl std::fmt::Display for BudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Budget error: {}", self.0)
    }
}

impl std::error::Error for BudgetError {}

/// Validates that an adapter's capabilities satisfy the requested budget.
pub fn validate_budget(caps: &ManagedCapabilities, budget: &Budget) -> Result<(), BudgetError> {
    if budget.max_turns > 0 && !caps.max_turns {
        return Err(BudgetError(
            "provider does not support turn enforcement".into(),
        ));
    }
    if budget.max_tokens.is_some() && !caps.token_budget {
        return Err(BudgetError(
            "provider does not support token budgets".into(),
        ));
    }
    if budget.max_cost_usd.is_some() && !caps.cost_budget {
        return Err(BudgetError("provider does not support cost budgets".into()));
    }
    Ok(())
}
