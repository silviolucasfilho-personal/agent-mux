//! The pure parts of a run: pre-flight decisions, the `loop-result`
//! block, outcome derivation and the run-log entry. Everything that needs
//! the store, the files or a process lives in `app::loops`.

use crate::loops::registry::LoopEntry;
use crate::loops::store::LoopRun;
use crate::loops::{Level, Outcome};
use serde::{Deserialize, Serialize};

/// What pre-flight looks at, gathered by the caller in the spec's order.
#[derive(Debug, Clone)]
pub struct PreflightInput {
    pub pause_all: bool,
    /// `loop-pause-all` found in the state file or `LOOP.md`.
    pub kill_switch_in_files: bool,
    pub workspace_exists: bool,
    pub workspace_is_repo: bool,
    pub runs_today: i64,
    pub tokens_today: i64,
    /// The breaker tripped: its reason.
    pub breaker_trip: Option<String>,
    /// The breaker is one attempt short of tripping: the cap reason
    /// (`breaker::Verdict::near_trip_reason`). Caps the run at L1.
    pub breaker_near_trip: Option<String>,
    /// What the readiness audit allows; `Err(why)` for the configured
    /// level means a cap.
    pub audit_allows_configured: Result<(), String>,
    pub audit_allows_l2: Result<(), String>,
    pub audit_score: Option<u32>,
    pub state_stale: bool,
    /// A path guard exists for this harness (Claude per launch, Codex
    /// with installed hooks).
    pub guard_available: bool,
    pub harness_resolves: bool,
    /// The pattern's triage skill exists in the workspace for the harness
    /// (`.claude/skills/<skill>/SKILL.md` or the Codex equivalent).
    pub skill_installed: bool,
    /// Another run is live for this loop or its workspace, or no slot.
    pub concurrency_blocked: Option<String>,
}

impl Default for PreflightInput {
    fn default() -> Self {
        PreflightInput {
            pause_all: false,
            kill_switch_in_files: false,
            workspace_exists: false,
            workspace_is_repo: false,
            runs_today: 0,
            tokens_today: 0,
            breaker_trip: None,
            breaker_near_trip: None,
            audit_allows_configured: Ok(()),
            audit_allows_l2: Ok(()),
            audit_score: None,
            state_stale: false,
            guard_available: false,
            harness_resolves: false,
            skill_installed: true,
            concurrency_blocked: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preflight {
    Blocked {
        reason: String,
        /// The loop is paused afterwards (breaker trip).
        pause: bool,
    },
    Go {
        effective_level: Level,
        /// Why the effective level is below the configured one.
        level_reason: Option<String>,
        /// Budget mode for the context: `normal` | `report-only`.
        budget_percent: u32,
    },
}

/// The spec's pre-flight, section 8.2, in order.
pub fn preflight(entry: &LoopEntry, input: &PreflightInput) -> Preflight {
    if input.pause_all {
        return blocked("paused: kill switch (K)", false);
    }
    if input.kill_switch_in_files {
        return blocked("paused: loop-pause-all found in the workspace files", false);
    }
    if entry.paused() {
        return blocked(
            &format!(
                "paused{}",
                entry
                    .paused_reason
                    .as_deref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ),
            false,
        );
    }
    if !input.workspace_exists {
        return blocked("workspace does not exist", false);
    }
    if entry.level > Level::L1 && !input.workspace_is_repo {
        return blocked("L2+ needs a git repository for the worktree", false);
    }
    if input.runs_today >= i64::from(entry.max_runs_per_day) {
        return blocked(
            &format!(
                "runs today {} at the cap of {}",
                input.runs_today, entry.max_runs_per_day
            ),
            false,
        );
    }
    let percent = if entry.max_tokens_per_day == 0 {
        0
    } else {
        ((input.tokens_today.max(0) as u128 * 100) / entry.max_tokens_per_day as u128) as u32
    };
    if percent >= 100 {
        return blocked(
            &format!(
                "tokens today {} at the cap of {}",
                crate::loops::format_tokens(input.tokens_today.max(0) as u64),
                crate::loops::format_tokens(entry.max_tokens_per_day)
            ),
            false,
        );
    }
    if let Some(why) = &input.breaker_trip {
        return blocked(&format!("circuit breaker: {why}"), true);
    }
    if input.concurrency_blocked.is_some() {
        // handled by the scheduler before pre-flight; kept for `loop run`
        return blocked(input.concurrency_blocked.as_deref().unwrap_or(""), false);
    }
    if !input.harness_resolves {
        return blocked("the profile's command is not installed", false);
    }
    if !input.skill_installed {
        return blocked(
            "the loop's skill is not installed in the workspace: edit the loop with Scaffold on, or run `agent-mux loop init`",
            false,
        );
    }
    let mut level = entry.level;
    let mut reason: Option<String> = None;
    let cap = |to: Level, why: String, level: &mut Level, reason: &mut Option<String>| {
        if *level > to {
            *level = to;
            *reason = Some(why);
        }
    };
    if percent >= 80 {
        cap(
            Level::L1,
            format!("tokens today at {percent}% of the cap"),
            &mut level,
            &mut reason,
        );
    }
    if let Some(why) = &input.breaker_near_trip {
        cap(Level::L1, why.clone(), &mut level, &mut reason);
    }
    if input.state_stale {
        cap(
            Level::L1,
            "state stale: Last run older than 14 days".into(),
            &mut level,
            &mut reason,
        );
    }
    if level > Level::L1
        && let Err(why) = &input.audit_allows_configured
    {
        let to = if level == Level::L3 && input.audit_allows_l2.is_ok() {
            Level::L2
        } else {
            Level::L1
        };
        cap(to, format!("readiness: {why}"), &mut level, &mut reason);
    }
    if level > Level::L1 && !input.guard_available {
        cap(
            Level::L1,
            "no path guard for this harness".into(),
            &mut level,
            &mut reason,
        );
    }
    Preflight::Go {
        effective_level: level,
        level_reason: reason,
        budget_percent: percent,
    }
}

fn blocked(reason: &str, pause: bool) -> Preflight {
    Preflight::Blocked {
        reason: reason.to_string(),
        pause,
    }
}

/// The block the skills end with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LoopResult {
    pub outcome: String,
    #[serde(default)]
    pub items_found: u64,
    #[serde(default)]
    pub actions_taken: u64,
    #[serde(default)]
    pub escalations: u64,
    #[serde(default)]
    pub summary: String,
}

/// Finds the last fenced ```loop-result block in a message.
pub fn parse_loop_result(text: &str) -> Option<LoopResult> {
    let mut last: Option<LoopResult> = None;
    let mut rest = text;
    while let Some(start) = rest.find("```loop-result") {
        let after = &rest[start + "```loop-result".len()..];
        let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
        let body = &after[body_start..];
        let end = body.find("```").unwrap_or(body.len());
        if let Ok(r) = serde_json::from_str::<LoopResult>(body[..end].trim()) {
            last = Some(r);
        }
        rest = &body[end..];
    }
    last
}

/// What post-run knows besides the block.
#[derive(Debug, Clone, Default)]
pub struct Observed {
    pub exit_code: Option<u32>,
    pub timed_out: bool,
    pub worktree_changed: bool,
    pub state_changed: bool,
    pub high_priority_grew: bool,
    /// One verdict per checker sub-agent that answered (`loop-verifier`,
    /// `loop-reviewer`, …), in the order they ran.
    pub verifier_verdicts: Vec<String>,
    pub permission_refused: bool,
    /// The harness reported the skill as an unknown command.
    pub skill_missing: bool,
}

/// The one word several checkers add up to: any `ESCALATE_HUMAN` wins,
/// then any `REJECT`, and only unanimous `APPROVE`s approve. `None` when
/// nobody answered.
pub fn combined_verdict(verdicts: &[String]) -> Option<String> {
    if verdicts.is_empty() {
        return None;
    }
    if verdicts.iter().any(|v| v == "ESCALATE_HUMAN") {
        return Some("ESCALATE_HUMAN".into());
    }
    if verdicts.iter().any(|v| v != "APPROVE") {
        return Some("REJECT".into());
    }
    Some("APPROVE".into())
}

/// `APPROVE (2/2)`: the combined word and how many checkers said it, for
/// a run with more than one checker; the bare word otherwise.
pub fn verdict_label(verdicts: &[String]) -> Option<String> {
    let word = combined_verdict(verdicts)?;
    if verdicts.len() < 2 {
        return Some(word);
    }
    let agreeing = verdicts.iter().filter(|v| **v == word).count();
    Some(format!("{word} ({agreeing}/{})", verdicts.len()))
}

/// Spec section 8.6 step 2, plus the checkers' rule: a fix is proposed
/// only when every checker that answered said APPROVE. A REJECT or an
/// ESCALATE_HUMAN against a changed worktree (or a block claiming a fix)
/// makes the run `escalated`: the branch stays for a human, nothing is
/// proposed.
pub fn derive_outcome(result: Option<&LoopResult>, obs: &Observed) -> Outcome {
    if obs.timed_out
        || obs.exit_code.is_some_and(|c| c != 0)
        || obs.permission_refused
        || obs.skill_missing
    {
        return Outcome::Failed;
    }
    let verdict = combined_verdict(&obs.verifier_verdicts);
    let claims_fix = result
        .and_then(|r| Outcome::parse(&r.outcome))
        .is_some_and(|o| o == Outcome::FixProposed);
    if (obs.worktree_changed || claims_fix)
        && verdict
            .as_deref()
            .is_some_and(|v| v == "REJECT" || v == "ESCALATE_HUMAN")
    {
        return Outcome::Escalated;
    }
    if let Some(r) = result
        && let Some(o) = Outcome::parse(&r.outcome)
        && !matches!(o, Outcome::Blocked | Outcome::Failed)
    {
        // the block cannot claim a fix that is not there
        if o == Outcome::FixProposed && !obs.worktree_changed {
            return Outcome::ReportOnly;
        }
        return o;
    }
    if obs.worktree_changed {
        return Outcome::FixProposed;
    }
    if verdict.as_deref() == Some("ESCALATE_HUMAN") || obs.high_priority_grew {
        return Outcome::Escalated;
    }
    if obs.state_changed {
        return Outcome::ReportOnly;
    }
    Outcome::NoOp
}

/// The harness could not find the loop's skill: Claude Code prints
/// `Unknown command: /<skill>` and exits 0. Returns the offending name.
pub fn skill_missing(output: &str) -> Option<String> {
    output.lines().find_map(|l| {
        let t = l.trim();
        t.strip_prefix("Unknown command:")
            .or_else(|| t.strip_prefix("Unknown skill:"))
            .map(|rest| {
                rest.trim()
                    .trim_matches(|c: char| c == '`' || c == '"')
                    .to_string()
            })
            .filter(|s| !s.is_empty())
    })
}

/// The `## High Priority` section of a state file, for the "grew" test.
pub fn high_priority_items(state_text: &str) -> usize {
    let mut in_section = false;
    let mut n = 0;
    for line in state_text.lines() {
        let t = line.trim_start();
        if t.starts_with("## ") {
            in_section = t.to_ascii_lowercase().contains("high priority");
            continue;
        }
        if in_section && (t.starts_with("- ") || t.starts_with("* ")) {
            n += 1;
        }
    }
    n
}

/// True when the kill-switch literal appears outside a comment or a
/// budget/kill-switch description line (the template names it).
pub fn kill_switch_active(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim();
        t.contains(crate::loops::KILL_SWITCH)
            // a backticked mention (the templates explain the switch) is not a switch
            && !t.contains(&format!("`{}`", crate::loops::KILL_SWITCH))
            && !t.starts_with("<!--")
            && !t.starts_with('#')
            && !t.starts_with('-')
            && !t.starts_with('|')
            && !t.starts_with('*')
            && !t.starts_with('`')
            && !t.to_ascii_lowercase().contains("label")
            && !t.to_ascii_lowercase().contains("command")
    })
}

/// The run-log entry for a completed run (never for a blocked one).
pub fn runlog_entry(
    run: &LoopRun,
    readiness_score: Option<i64>,
    launch_id: Option<&str>,
) -> crate::loops::runlog::Entry {
    let duration_s = match (run.started_ns, run.ended_ns) {
        (Some(s), Some(e)) if e > s => ((e - s) / 1_000_000_000).max(1) as u64,
        _ => 0,
    };
    let mut extra = serde_json::Map::new();
    if let Some(s) = readiness_score {
        extra.insert("readiness_score".into(), s.into());
    }
    extra.insert("level".into(), run.effective_level.as_str().into());
    extra.insert("harness".into(), run.harness.clone().into());
    if let Some(l) = launch_id {
        extra.insert("launch_id".into(), l.into());
    }
    extra.insert("source".into(), "agent-mux".into());
    crate::loops::runlog::Entry {
        run_id: run.id.clone(),
        pattern: run.pattern.clone(),
        duration_s,
        items_found: run.items_found.unwrap_or(0).max(0) as u64,
        actions_taken: run.actions_taken.unwrap_or(0).max(0) as u64,
        escalations: run.escalations.unwrap_or(0).max(0) as u64,
        tokens_estimate: run.tokens.unwrap_or(0).max(0) as u64,
        outcome: run.outcome.as_str().into(),
        extra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(level: Level) -> LoopEntry {
        LoopEntry {
            id: "a".into(),
            workspace: PathBuf::from("/w"),
            pattern: "ci-sweeper".into(),
            profile: String::new(),
            harness: "claude".into(),
            model: String::new(),
            verifier_model: String::new(),
            interval_s: 900,
            level,
            enabled: true,
            max_runs_per_day: 2,
            max_tokens_per_day: 100_000,
            max_cost_usd_per_run: None,
            bypass_score: false,
            created_at: String::new(),
            next_run_at: None,
            last_run_id: None,
            paused_reason: None,
        }
    }

    fn ok_input() -> PreflightInput {
        PreflightInput {
            workspace_exists: true,
            workspace_is_repo: true,
            audit_allows_configured: Ok(()),
            audit_allows_l2: Ok(()),
            guard_available: true,
            harness_resolves: true,
            ..Default::default()
        }
    }

    #[test]
    fn preflight_follows_the_spec_order_and_caps_levels() {
        let e = entry(Level::L2);
        let mut i = ok_input();
        assert!(matches!(
            preflight(&e, &i),
            Preflight::Go {
                effective_level: Level::L2,
                level_reason: None,
                budget_percent: 0
            }
        ));
        i.tokens_today = 84_000;
        match preflight(&e, &i) {
            Preflight::Go {
                effective_level,
                level_reason,
                budget_percent,
            } => {
                assert_eq!(effective_level, Level::L1);
                assert_eq!(budget_percent, 84);
                assert!(level_reason.unwrap().contains("84%"));
            }
            other => panic!("{other:?}"),
        }
        i.tokens_today = 100_000;
        assert!(matches!(
            preflight(&e, &i),
            Preflight::Blocked { pause: false, .. }
        ));
        i.tokens_today = 0;
        i.runs_today = 2;
        assert!(matches!(preflight(&e, &i), Preflight::Blocked { .. }));
        i.runs_today = 0;
        i.breaker_trip = Some("stagnation".into());
        assert!(matches!(
            preflight(&e, &i),
            Preflight::Blocked { pause: true, .. }
        ));
        i.breaker_trip = None;
        i.breaker_near_trip = Some("breaker one attempt from tripping (stagnation)".into());
        match preflight(&e, &i) {
            Preflight::Go {
                effective_level,
                level_reason,
                ..
            } => {
                assert_eq!(effective_level, Level::L1);
                assert_eq!(
                    level_reason.as_deref(),
                    Some("breaker one attempt from tripping (stagnation)")
                );
            }
            other => panic!("{other:?}"),
        }
        i.breaker_near_trip = None;
        i.state_stale = true;
        assert!(matches!(
            preflight(&e, &i),
            Preflight::Go {
                effective_level: Level::L1,
                ..
            }
        ));
        i.state_stale = false;
        i.audit_allows_configured = Err("L2 needs score ≥ 58".into());
        i.audit_allows_l2 = Err("L2 needs score ≥ 58".into());
        assert!(matches!(
            preflight(&e, &i),
            Preflight::Go {
                effective_level: Level::L1,
                ..
            }
        ));
        let e3 = entry(Level::L3);
        i.audit_allows_l2 = Ok(());
        assert!(matches!(
            preflight(&e3, &i),
            Preflight::Go {
                effective_level: Level::L2,
                ..
            }
        ));
        i.audit_allows_configured = Ok(());
        i.guard_available = false;
        assert!(matches!(
            preflight(&e3, &i),
            Preflight::Go {
                effective_level: Level::L1,
                ..
            }
        ));
        i.pause_all = true;
        assert!(matches!(preflight(&e3, &i), Preflight::Blocked { .. }));
        let mut p = entry(Level::L1);
        p.paused_reason = Some("breaker".into());
        assert!(matches!(
            preflight(&p, &ok_input()),
            Preflight::Blocked { .. }
        ));
    }

    #[test]
    fn loop_result_and_outcome_derivation() {
        let msg = "done\n```loop-result\n{\"outcome\":\"report-only\",\"items_found\":9,\"actions_taken\":1,\"escalations\":2,\"summary\":\"ok\"}\n```\n";
        let r = parse_loop_result(msg).unwrap();
        assert_eq!(r.items_found, 9);
        assert!(parse_loop_result("nothing").is_none());
        let obs = Observed::default();
        assert_eq!(derive_outcome(Some(&r), &obs), Outcome::ReportOnly);
        let fix = LoopResult {
            outcome: "fix-proposed".into(),
            ..Default::default()
        };
        assert_eq!(
            derive_outcome(Some(&fix), &obs),
            Outcome::ReportOnly,
            "no change, no fix"
        );
        let changed = Observed {
            worktree_changed: true,
            ..Default::default()
        };
        assert_eq!(derive_outcome(Some(&fix), &changed), Outcome::FixProposed);
        assert_eq!(derive_outcome(None, &changed), Outcome::FixProposed);
        assert_eq!(
            derive_outcome(
                None,
                &Observed {
                    state_changed: true,
                    ..Default::default()
                }
            ),
            Outcome::ReportOnly
        );
        assert_eq!(
            derive_outcome(
                None,
                &Observed {
                    high_priority_grew: true,
                    ..Default::default()
                }
            ),
            Outcome::Escalated
        );
        assert_eq!(derive_outcome(None, &obs), Outcome::NoOp);
        assert_eq!(
            derive_outcome(
                Some(&r),
                &Observed {
                    exit_code: Some(1),
                    ..Default::default()
                }
            ),
            Outcome::Failed
        );
        assert_eq!(
            derive_outcome(
                Some(&r),
                &Observed {
                    timed_out: true,
                    ..Default::default()
                }
            ),
            Outcome::Failed
        );
        assert_eq!(
            high_priority_items("## High Priority (x)\n- [ ] a\n- [ ] b\n## Watch List\n- c\n"),
            2
        );
        assert_eq!(
            skill_missing("hi\nUnknown command: /loop-pr-triage\nbye"),
            Some("/loop-pr-triage".into())
        );
        assert!(skill_missing("all good").is_none());
        assert_eq!(
            derive_outcome(
                Some(&r),
                &Observed {
                    skill_missing: true,
                    ..Default::default()
                }
            ),
            Outcome::Failed
        );
        // the checkers' rule
        let v = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(combined_verdict(&[]), None);
        assert_eq!(
            combined_verdict(&v(&["APPROVE", "APPROVE"])).as_deref(),
            Some("APPROVE")
        );
        assert_eq!(
            combined_verdict(&v(&["APPROVE", "REJECT"])).as_deref(),
            Some("REJECT")
        );
        assert_eq!(
            combined_verdict(&v(&["REJECT", "ESCALATE_HUMAN"])).as_deref(),
            Some("ESCALATE_HUMAN")
        );
        assert_eq!(verdict_label(&v(&["APPROVE"])).as_deref(), Some("APPROVE"));
        assert_eq!(
            verdict_label(&v(&["APPROVE", "APPROVE"])).as_deref(),
            Some("APPROVE (2/2)")
        );
        assert_eq!(
            verdict_label(&v(&["APPROVE", "REJECT"])).as_deref(),
            Some("REJECT (1/2)")
        );
        let approved = Observed {
            worktree_changed: true,
            verifier_verdicts: v(&["APPROVE", "APPROVE"]),
            ..Default::default()
        };
        assert_eq!(derive_outcome(Some(&fix), &approved), Outcome::FixProposed);
        let split = Observed {
            worktree_changed: true,
            verifier_verdicts: v(&["APPROVE", "REJECT"]),
            ..Default::default()
        };
        assert_eq!(
            derive_outcome(Some(&fix), &split),
            Outcome::Escalated,
            "one REJECT keeps the fix from being proposed even when the block claims it"
        );
        assert_eq!(derive_outcome(None, &split), Outcome::Escalated);
        let escalate = Observed {
            worktree_changed: true,
            verifier_verdicts: v(&["ESCALATE_HUMAN"]),
            ..Default::default()
        };
        assert_eq!(derive_outcome(None, &escalate), Outcome::Escalated);
        let rejected_nothing = Observed {
            state_changed: true,
            verifier_verdicts: v(&["REJECT"]),
            ..Default::default()
        };
        assert_eq!(
            derive_outcome(None, &rejected_nothing),
            Outcome::ReportOnly,
            "a REJECT with no change to propose is just a report"
        );
        assert!(kill_switch_active("Last run: x\nloop-pause-all\n"));
        assert!(!kill_switch_active(
            "- Command or issue label: `loop-pause-all`\n"
        ));
        assert!(!kill_switch_active(
            "Kill switch: put the literal `loop-pause-all` in the state file.\n"
        ));
        assert!(!kill_switch_active(
            "## Kill switch\n<!-- loop-pause-all -->\n"
        ));
    }
}
