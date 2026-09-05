//! The budget guard: `max_cost_usd` / `max_turns` on a launch, enforced
//! through the hook channel. A synchronous `PreToolUse` hook (Claude per
//! launch; Codex through the installed `hooks.json`) looks the launch's
//! guard and spend up in the store and refuses the call past the limit,
//! in the reply shape verified against the installed Claude CLI:
//! `hookSpecificOutput.permissionDecision = "deny"` with a reason the
//! model sees. Antigravity is excluded: its `PreToolUse` must answer every
//! call, which is why it is not registered. The guard permits on any
//! error or delay — it fails open, never closed.

use crate::config::ProfileTracing;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guard {
    pub max_cost_usd: Option<f64>,
    pub max_turns: Option<u32>,
}

impl Guard {
    /// The guard a profile's tracing section asks for, if any limit is set.
    pub fn from_tracing(t: Option<&ProfileTracing>) -> Option<Guard> {
        let t = t?;
        let max_cost_usd = t.max_cost_usd.filter(|c| c.is_finite() && *c > 0.0);
        let max_turns = t.max_turns.filter(|n| *n > 0);
        (max_cost_usd.is_some() || max_turns.is_some()).then_some(Guard {
            max_cost_usd,
            max_turns,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({ "max_cost_usd": self.max_cost_usd, "max_turns": self.max_turns })
    }

    pub fn from_json(v: &Value) -> Option<Guard> {
        let max_cost_usd = v
            .get("max_cost_usd")
            .and_then(|c| c.as_f64())
            .filter(|c| *c > 0.0);
        let max_turns = v
            .get("max_turns")
            .and_then(|n| n.as_u64())
            .map(|n| n as u32)
            .filter(|n| *n > 0);
        (max_cost_usd.is_some() || max_turns.is_some()).then_some(Guard {
            max_cost_usd,
            max_turns,
        })
    }

    /// "max $2.00, 40 turns" for notices.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(c) = self.max_cost_usd {
            parts.push(format!("max ${c:.2}"));
        }
        if let Some(n) = self.max_turns {
            parts.push(format!("{n} turns"));
        }
        parts.join(", ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Permit,
    Block(String),
}

/// What the store says about the launch against its guard. Any error —
/// no store, a locked store past `budget`, no launch row — permits.
pub fn check(db: &Path, launch_id: &str, budget: Duration) -> Verdict {
    let Ok(conn) = crate::tracing::store::open_hook_sink(db, budget) else {
        return Verdict::Permit;
    };
    let meta: Option<String> = conn
        .query_row(
            "SELECT metadata FROM launches WHERE id = ?1",
            [launch_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let Some(guard) = meta
        .and_then(|m| serde_json::from_str::<Value>(&m).ok())
        .and_then(|v| v.get("guard").cloned())
        .and_then(|g| Guard::from_json(&g))
    else {
        return Verdict::Permit;
    };
    if let Some(max) = guard.max_cost_usd {
        let spent: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(o.total_cost_usd), 0)
                 FROM observations o JOIN traces t ON t.id = o.trace_id
                 WHERE t.launch_id = ?1",
                [launch_id],
                |r| r.get(0),
            )
            .unwrap_or(0.0);
        if spent > max {
            return Verdict::Block(format!(
                "agent-mux budget: ${spent:.2} spent, over the ${max:.2} limit for this launch"
            ));
        }
    }
    if let Some(max) = guard.max_turns {
        let turns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM traces WHERE launch_id = ?1",
                [launch_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if turns > i64::from(max) {
            return Verdict::Block(format!(
                "agent-mux budget: turn {turns}, over the {max}-turn limit for this launch"
            ));
        }
    }
    Verdict::Permit
}

/// The `PreToolUse` reply that refuses the call, as verified against the
/// installed Claude CLI (the model sees the reason); Codex reads the same
/// keys.
pub fn deny_json(reason: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guard_needs_at_least_one_positive_limit() {
        let t = |c: Option<f64>, n: Option<u32>| ProfileTracing {
            max_cost_usd: c,
            max_turns: n,
            ..Default::default()
        };
        assert_eq!(Guard::from_tracing(None), None);
        assert_eq!(Guard::from_tracing(Some(&t(None, None))), None);
        assert_eq!(Guard::from_tracing(Some(&t(Some(0.0), Some(0)))), None);
        let g = Guard::from_tracing(Some(&t(Some(2.5), None))).unwrap();
        assert_eq!(g.describe(), "max $2.50");
        let g = Guard::from_tracing(Some(&t(Some(2.5), Some(40)))).unwrap();
        assert_eq!(g.describe(), "max $2.50, 40 turns");
        assert_eq!(Guard::from_json(&g.to_json()), Some(g));
        assert_eq!(Guard::from_json(&json!({"max_turns": 0})), None);
        let v: Value = serde_json::from_str(&deny_json("why")).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(v["hookSpecificOutput"]["permissionDecisionReason"], "why");
        // no store at all: permit
        assert_eq!(
            check(
                Path::new("/nonexistent/traces.db"),
                "l1",
                Duration::from_millis(10)
            ),
            Verdict::Permit
        );
    }
}
