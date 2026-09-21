//! Loop Engineering: scheduled, gated, bounded agent runs against one
//! workspace, driven from the Loops sidebar section. See
//! `docs/superpowers/specs/2026-09-15-loop-engineering-design.md`.
//!
//! Facts are Rust (spend, breaker state, readiness, tokens, files touched);
//! judgment is the skill (`loops/skills`); the files on disk (`STATE.md`,
//! `loop-run-log.md`, `gate.yaml`, …) are the contract a workspace keeps.
//!
//! Not to be confused with `tracing::loops`, the per-turn agentic-loop
//! diagnostics behind `trace loops`.

pub mod agents;
pub mod breaker;
pub mod cli;
pub mod context;
pub mod cost;
pub mod gate;
pub mod patterns;
pub mod readiness;
pub mod registry;
pub mod run;
pub mod runlog;
pub mod scaffold;
pub mod schedule;
pub mod state;
pub mod store;
pub mod worktree;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Readiness ladder level of a loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum Level {
    #[default]
    L1,
    L2,
    L3,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::L1 => "L1",
            Level::L2 => "L2",
            Level::L3 => "L3",
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        match s.trim().to_ascii_uppercase().as_str() {
            "L1" => Some(Level::L1),
            "L2" => Some(Level::L2),
            "L3" => Some(Level::L3),
            _ => None,
        }
    }

    /// What the level means, for the dialog and the preview.
    pub fn label(self) -> &'static str {
        match self {
            Level::L1 => "report-only",
            Level::L2 => "assisted",
            Level::L3 => "unattended",
        }
    }
}

/// Outcome of one loop run, as stored in `loop_runs.outcome` and written
/// to `loop-run-log.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    #[serde(rename = "report-only")]
    ReportOnly,
    #[serde(rename = "fix-proposed")]
    FixProposed,
    #[serde(rename = "escalated")]
    Escalated,
    #[serde(rename = "no-op")]
    NoOp,
    #[serde(rename = "blocked")]
    Blocked,
    #[serde(rename = "failed")]
    Failed,
}

impl Outcome {
    /// What the outcome means for the reader. `escalated` says what the
    /// loop did; `needs you` says what the user must do, which is the only
    /// reason to look at a run list at all. The stored value never changes.
    pub fn word(self) -> &'static str {
        match self {
            Outcome::ReportOnly => "reported",
            Outcome::FixProposed => "fix ready",
            Outcome::Escalated => "needs you",
            Outcome::NoOp => "quiet",
            Outcome::Blocked => "skipped",
            Outcome::Failed => "failed",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::ReportOnly => "report-only",
            Outcome::FixProposed => "fix-proposed",
            Outcome::Escalated => "escalated",
            Outcome::NoOp => "no-op",
            Outcome::Blocked => "blocked",
            Outcome::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Outcome> {
        match s.trim() {
            "report-only" => Some(Outcome::ReportOnly),
            "fix-proposed" => Some(Outcome::FixProposed),
            "escalated" => Some(Outcome::Escalated),
            "no-op" | "noop" => Some(Outcome::NoOp),
            "blocked" => Some(Outcome::Blocked),
            "failed" => Some(Outcome::Failed),
            _ => None,
        }
    }

    /// A run that waits on a human decision.
    pub fn needs_human(self) -> bool {
        matches!(self, Outcome::FixProposed | Outcome::Escalated)
    }
}

/// Token profile of a pattern for the cost estimate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternCost {
    pub tokens_noop: u64,
    pub tokens_report: u64,
    pub tokens_action: u64,
    /// Fraction of a run's prompt that is stable across runs (cacheable).
    pub stable_fraction: f64,
    /// An empty watch list must exit early (under 5k tokens).
    pub early_exit_required: bool,
}

/// One entry of `loops/registry.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pattern {
    pub id: String,
    pub name: String,
    pub goal: String,
    /// Default interval between runs, seconds.
    pub default_interval_s: u64,
    pub week_one_level: Level,
    /// The state file this pattern keeps in the workspace.
    pub state_file: String,
    /// agent-mux skills the scaffolder installs, triage skill first.
    pub skills: Vec<String>,
    /// The pattern hands changes to the `loop-verifier` agent.
    pub verifier: bool,
    /// The pattern can propose code fixes repeatedly: seeds and maintains
    /// `loop-ledger.json` and runs the circuit breaker.
    pub breaker: bool,
    pub human_gates: Vec<String>,
    pub risk: String,
    pub token_cost: String,
    pub max_runs_per_day: u32,
    pub max_tokens_per_day: u64,
    /// Scheduler order: lower runs first when several loops are due.
    pub priority: u8,
    pub cost: PatternCost,
    /// The opening prompt of a run, replacing `[loop] run` of prompts.toml;
    /// same placeholders (`crate::prompts::LOOP_PLACEHOLDERS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The loop agents the scaffolder installs for this pattern, by name
    /// (`loops/agents/<name>.md`). Empty means the verifier alone when
    /// `verifier` is true, else no agent; see `effective_agents`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    /// The model this pattern suggests for a run (`--model`). A loop copies
    /// it when it is registered and can change it afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The model this pattern suggests for the `loop-verifier` sub-agent:
    /// the checker of the maker/checker pair can be a different, usually
    /// stronger or cheaper, model than the run that calls it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_model: Option<String>,
}

impl Pattern {
    /// The agents a workspace of this pattern receives: the listed ones,
    /// or `loop-verifier` alone when the list is empty and the pattern
    /// uses a verifier, else none.
    pub fn effective_agents(&self) -> Vec<String> {
        if !self.agents.is_empty() {
            return self.agents.clone();
        }
        if self.verifier {
            vec!["loop-verifier".to_string()]
        } else {
            Vec::new()
        }
    }

    /// The skill whose invocation opens a run.
    pub fn triage_skill(&self) -> &str {
        self.skills
            .first()
            .map(String::as_str)
            .unwrap_or("loop-triage")
    }
}

/// The policy the `PreToolUse` guard enforces on a loop launch, stored as
/// `launches.metadata.loop_policy`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LoopPolicy {
    /// The run may only write the state file, the run log and
    /// `.loop-context/`.
    pub report_only: bool,
    /// Why the run is report-only, if it is (shown to the model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub state_file: String,
    pub run_log: String,
    #[serde(default)]
    pub denylist: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_files: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
}

/// What a loop launch adds to the launch row: the keys of
/// `launches.metadata.loop_*` and the policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoopLaunch {
    pub loop_id: String,
    pub run_id: String,
    pub pattern: String,
    pub level: Level,
    pub workspace: String,
    pub policy: LoopPolicy,
}

/// The nine state file names the method recognizes, one per built-in
/// pattern (`loops/registry.toml`); the guard permits writes to these.
pub const STATE_FILES: [&str; 9] = [
    "STATE.md",
    "pr-babysitter-state.md",
    "ci-sweeper-state.md",
    "post-merge-state.md",
    "dependency-sweeper-state.md",
    "changelog-drafter-state.md",
    "issue-triage-state.md",
    "continuous-pr-state.md",
    "harness-audit-state.md",
];

pub const LOOP_MD: &str = "LOOP.md";
pub const BUDGET_MD: &str = "loop-budget.md";
pub const RUN_LOG_MD: &str = "loop-run-log.md";
pub const CONSTRAINTS_MD: &str = "loop-constraints.md";
pub const GATE_YAML: &str = "gate.yaml";
pub const LEDGER_JSON: &str = "loop-ledger.json";
/// The literal that pauses every loop of a workspace when it appears in
/// the state file or `LOOP.md`.
pub const KILL_SWITCH: &str = "loop-pause-all";

/// Parses `2026-09-15T08:00:39Z`, `2026-09-15 08:00:39+02:00` or a bare
/// `2026-09-15` (taken as midnight UTC).
pub fn parse_timestamp(s: &str) -> Option<OffsetDateTime> {
    let s = s.trim();
    if s.len() == 10 {
        return OffsetDateTime::parse(&format!("{s}T00:00:00Z"), &Rfc3339).ok();
    }
    let normalized = if s.len() > 10 && s.as_bytes()[10] == b' ' {
        let mut t = s.to_string();
        t.replace_range(10..11, "T");
        t
    } else {
        s.to_string()
    };
    OffsetDateTime::parse(&normalized, &Rfc3339).ok()
}

/// RFC 3339 in UTC with second precision: the run id format.
pub fn format_timestamp(t: OffsetDateTime) -> String {
    t.to_offset(time::UtcOffset::UTC)
        .replace_nanosecond(0)
        .unwrap_or(t)
        .format(&Rfc3339)
        .unwrap_or_default()
}

pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

/// Nanoseconds since the epoch, the store's clock.
pub fn to_ns(t: OffsetDateTime) -> i64 {
    (t.unix_timestamp_nanos()).clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

pub fn from_ns(ns: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(ns as i128).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// Midnight UTC of the day `t` falls in.
pub fn utc_midnight(t: OffsetDateTime) -> OffsetDateTime {
    let t = t.to_offset(time::UtcOffset::UTC);
    t.replace_time(time::Time::MIDNIGHT)
}

/// `5m`, `2h`, `1d` → seconds. Minimum 5 minutes.
pub fn parse_interval(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: u64 = num.parse().ok()?;
    if n == 0 {
        return None;
    }
    let secs = match unit {
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => return None,
    };
    (secs >= 300).then_some(secs)
}

/// The shortest of `1d`, `2h`, `15m` that spells `secs` exactly, else `<n>s`.
pub fn format_interval(secs: u64) -> String {
    if secs.is_multiple_of(86_400) {
        format!("{}d", secs / 86_400)
    } else if secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// A run with nothing to read: a timeline folds consecutive ones into a
/// single row. Runs fold together only when they say the same thing, so a
/// blocked run's reason is part of the key.
pub fn run_fold_key(r: &store::LoopRun) -> Option<String> {
    match r.outcome {
        Outcome::Blocked => Some(format!(
            "skipped · {}",
            r.detail_str("reason").unwrap_or("blocked")
        )),
        Outcome::NoOp => Some("quiet".into()),
        _ if r.detail.get("quiet").is_some() => Some("quiet".into()),
        _ => None,
    }
}

/// `1.2M`, `52k`, `900`.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", (n as f64 / 1000.0).round() as u64)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_and_intervals_round_trip() {
        let t = parse_timestamp("2026-09-15T08:00:39Z").unwrap();
        assert_eq!(format_timestamp(t), "2026-09-15T08:00:39Z");
        assert_eq!(
            format_timestamp(parse_timestamp("2026-09-15").unwrap()),
            "2026-09-15T00:00:00Z"
        );
        assert!(parse_timestamp("2026-09-15 08:00:39+02:00").is_some());
        assert!(parse_timestamp("yesterday").is_none());
        assert_eq!(parse_interval("15m"), Some(900));
        assert_eq!(parse_interval("1d"), Some(86_400));
        assert_eq!(parse_interval("1m"), None, "below the five-minute floor");
        assert_eq!(parse_interval("3x"), None);
        assert_eq!(format_interval(900), "15m");
        assert_eq!(format_interval(7200), "2h");
        assert_eq!(format_interval(86_400), "1d");
        assert_eq!(format_tokens(52_000), "52k");
        assert_eq!(format_tokens(2_000_000), "2.0M");
        assert_eq!(Level::parse("l2"), Some(Level::L2));
        assert_eq!(Outcome::parse("fix-proposed"), Some(Outcome::FixProposed));
        assert!(Level::L1 < Level::L3);
    }
}
