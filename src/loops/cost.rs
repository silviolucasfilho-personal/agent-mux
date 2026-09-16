//! Token estimate of a loop at a cadence and level, for the add-loop
//! dialog and the Budget tab. Tokens only, never dollars: per-run costs
//! come from the store once a run happened.

use super::{Level, Pattern, format_tokens};

#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    pub pattern_id: String,
    pub level: Level,
    pub interval_s: u64,
    pub runs_per_day: u64,
    /// Orchestration multiplier on the action path: 2 with a verifier.
    pub multiplier: u32,
    pub noop_per_day: u64,
    pub report_per_day: u64,
    pub action_per_day: u64,
    pub realistic_per_run: u64,
    pub realistic_per_day: u64,
    pub assumptions: String,
    pub cached_per_run: Option<u64>,
    pub cached_per_day: Option<u64>,
    pub savings_percent: Option<u32>,
    pub suggested_daily_cap: u64,
    pub warnings: Vec<String>,
}

/// Cache reads bill at roughly a tenth of base input.
const CACHE_READ_DISCOUNT: f64 = 0.1;

fn mix(level: Level, early_exit: bool) -> (f64, f64, f64, &'static str) {
    match (level, early_exit) {
        (Level::L1, true) => (0.9, 0.1, 0.0, "L1: 90% early exit, 10% full triage"),
        (Level::L1, false) => (0.6, 0.4, 0.0, "L1: 60% no-op, 40% full triage"),
        (Level::L2, true) => (
            0.85,
            0.1,
            0.05,
            "L2: 85% early exit, 10% triage, 5% fix and verify",
        ),
        (Level::L2, false) => (0.5, 0.3, 0.2, "L2: 50% no-op, 30% triage, 20% action"),
        (Level::L3, _) => (
            0.4,
            0.35,
            0.25,
            "L3: 40% no-op, 35% triage, 25% action (unattended: watch it)",
        ),
    }
}

pub fn estimate(pattern: &Pattern, interval_s: u64, level: Level, with_caching: bool) -> Estimate {
    let runs_per_day = 86_400u64.checked_div(interval_s).unwrap_or(0);
    let multiplier: u32 = if pattern.verifier { 2 } else { 1 };
    let c = &pattern.cost;
    let action_per_run = c.tokens_action * u64::from(multiplier);
    let (mn, mr, ma, assumptions) = mix(level, c.early_exit_required);
    let realistic_per_run =
        (c.tokens_noop as f64 * mn + c.tokens_report as f64 * mr + action_per_run as f64 * ma)
            .round() as u64;
    let realistic_per_day = realistic_per_run * runs_per_day;
    let (cached_per_run, cached_per_day, savings_percent) =
        if with_caching && realistic_per_run > 0 && c.stable_fraction > 0.0 {
            let stable = realistic_per_run as f64 * c.stable_fraction;
            let variable = realistic_per_run as f64 - stable;
            let cached = (variable + stable * CACHE_READ_DISCOUNT).round() as u64;
            let savings = ((1.0 - cached as f64 / realistic_per_run as f64) * 100.0).round() as u32;
            (Some(cached), Some(cached * runs_per_day), Some(savings))
        } else {
            (None, None, None)
        };
    let cap = pattern.max_tokens_per_day;
    let action_per_day = action_per_run * runs_per_day;
    let mut warnings = Vec::new();
    if c.early_exit_required {
        warnings.push(
            "Early-exit triage is required — an empty watch list should exit in under 5k tokens."
                .to_string(),
        );
    }
    if action_per_day > cap {
        warnings.push(format!(
            "Worst case (action every run) exceeds the daily cap ({}/day).",
            format_tokens(cap)
        ));
    }
    if realistic_per_day > cap {
        warnings.push(
            "Realistic estimate exceeds the daily cap — slow the cadence or tighten scope."
                .to_string(),
        );
    }
    if runs_per_day >= 96 {
        warnings.push(format!(
            "High cadence ({runs_per_day} runs/day) — verify early exit is working."
        ));
    }
    if multiplier > 2 {
        warnings.push("The verifier doubles the action cost — confirm it is needed.".to_string());
    }
    Estimate {
        pattern_id: pattern.id.clone(),
        level,
        interval_s,
        runs_per_day,
        multiplier,
        noop_per_day: c.tokens_noop * runs_per_day,
        report_per_day: c.tokens_report * runs_per_day,
        action_per_day,
        realistic_per_run,
        realistic_per_day,
        assumptions: assumptions.to_string(),
        cached_per_run,
        cached_per_day,
        savings_percent,
        suggested_daily_cap: cap,
        warnings,
    }
}

/// The estimate as text, one line per item.
pub fn human_lines(e: &Estimate, pattern_name: &str) -> Vec<String> {
    let mut lines = vec![
        format!("Loop cost estimate — {pattern_name} ({})", e.pattern_id),
        format!(
            "Every {}  →  {} runs/day",
            super::format_interval(e.interval_s),
            e.runs_per_day
        ),
        format!(
            "Level {}  ·  action ×{}  ·  daily cap {} tokens",
            e.level.as_str(),
            e.multiplier,
            format_tokens(e.suggested_daily_cap)
        ),
        String::new(),
        "Daily token estimates:".to_string(),
        format!(
            "  Early exit / no-op:  {}  ({}/run)",
            format_tokens(e.noop_per_day),
            format_tokens(e.noop_per_day.checked_div(e.runs_per_day).unwrap_or(0))
        ),
        format!(
            "  Full triage:         {}  ({}/run)",
            format_tokens(e.report_per_day),
            format_tokens(e.report_per_day.checked_div(e.runs_per_day).unwrap_or(0))
        ),
        format!(
            "  Action every run:    {}  ({}/run)",
            format_tokens(e.action_per_day),
            format_tokens(e.action_per_day.checked_div(e.runs_per_day).unwrap_or(0))
        ),
        format!(
            "  Realistic blend:     {}  ({})",
            format_tokens(e.realistic_per_day),
            e.assumptions
        ),
    ];
    if let (Some(day), Some(pct)) = (e.cached_per_day, e.savings_percent) {
        lines.push(format!(
            "  With prompt caching: {}  ({pct}% less than the realistic blend)",
            format_tokens(day)
        ));
    }
    if !e.warnings.is_empty() {
        lines.push(String::new());
        lines.push("Warnings:".to_string());
        for w in &e.warnings {
            lines.push(format!("  ! {w}"));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loops::PatternCost;

    fn ci_sweeper() -> Pattern {
        Pattern {
            id: "ci-sweeper".into(),
            name: "CI Sweeper".into(),
            goal: "keep CI green".into(),
            default_interval_s: 900,
            week_one_level: Level::L2,
            state_file: "ci-sweeper-state.md".into(),
            skills: vec!["loop-ci-triage".into()],
            verifier: true,
            breaker: true,
            human_gates: vec![],
            risk: "medium".into(),
            token_cost: "very-high".into(),
            max_runs_per_day: 96,
            max_tokens_per_day: 1_000_000,
            priority: 1,
            cost: PatternCost {
                tokens_noop: 5_000,
                tokens_report: 50_000,
                tokens_action: 200_000,
                stable_fraction: 0.35,
                early_exit_required: true,
            },
        }
    }

    #[test]
    fn ci_sweeper_at_fifteen_minutes_l2_with_verifier() {
        let e = estimate(&ci_sweeper(), 900, Level::L2, true);
        assert_eq!(e.runs_per_day, 96);
        assert_eq!(e.multiplier, 2);
        assert_eq!(e.realistic_per_run, 29_250);
        assert_eq!(e.realistic_per_day, 2_808_000);
        assert_eq!(e.action_per_day, 400_000 * 96);
        assert_eq!(e.savings_percent, Some(32));
        assert_eq!(e.cached_per_run, Some(20_036));
        assert!(e.warnings.iter().any(|w| w.starts_with("Early-exit")));
        assert!(e.warnings.iter().any(|w| w.starts_with("Worst case")));
        assert!(
            e.warnings
                .iter()
                .any(|w| w.starts_with("Realistic estimate"))
        );
        assert!(e.warnings.iter().any(|w| w.starts_with("High cadence (96")));
        assert!(!e.warnings.iter().any(|w| w.contains("verifier doubles")));
        let lines = human_lines(&e, "CI Sweeper");
        assert!(lines[1].contains("Every 15m"));
        assert!(lines.iter().any(|l| l.contains("32% less")));
    }

    #[test]
    fn no_caching_and_a_quiet_daily_pattern() {
        let mut p = ci_sweeper();
        p.verifier = false;
        p.cost.early_exit_required = false;
        p.max_tokens_per_day = 100_000;
        let e = estimate(&p, 86_400, Level::L1, false);
        assert_eq!(e.runs_per_day, 1);
        assert_eq!(e.multiplier, 1);
        assert_eq!(e.realistic_per_run, 23_000, "0.6*5000 + 0.4*50000");
        assert!(e.cached_per_run.is_none() && e.savings_percent.is_none());
        assert!(e.warnings.iter().any(|w| w.starts_with("Worst case")));
        assert!(!e.warnings.iter().any(|w| w.starts_with("Realistic")));
        assert!(!e.warnings.iter().any(|w| w.starts_with("High cadence")));
        assert_eq!(estimate(&p, 0, Level::L3, true).runs_per_day, 0);
    }
}
