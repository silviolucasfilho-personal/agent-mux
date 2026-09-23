//! The Loop Ready audit: a 0–100 score over the files a loop-ready
//! workspace carries, the L0–L3 level the score and its gates allow, and
//! the findings that explain both. Spec section 10.
//!
//! Every signal is a boolean with a fixed weight; the sum is clamped to
//! 100, so several partial combinations reach it. Levels need more than a
//! score: L1 a state file, L2 a triage skill, L3 a verifier, a state file,
//! cost observability and loop activity within the last fourteen days.
//! Activity is a hard window, not a decay: a run an hour old and one
//! thirteen days old are worth the same fourteen points.

use crate::loops::{Level, STATE_FILES, parse_timestamp};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use time::OffsetDateTime;

/// Activity older than this is "files on disk", not a running loop.
pub const ACTIVITY_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);
/// A `Last run` this far in the future is still accepted (clock skew);
/// anything later is rejected as a forgery.
const FUTURE_TOLERANCE: Duration = Duration::from_secs(60);
/// How long the `git log` activity probe may take.
const GIT_TIMEOUT: Duration = Duration::from_millis(1500);

const THRESHOLD_L1: u32 = 38;
const THRESHOLD_L2: u32 = 58;
const THRESHOLD_L3: u32 = 78;

// Weights. The order and values are the method's published table.
const W_BASE: u32 = 7;
const W_STATE_FILE: u32 = 18;
const W_TRIAGE: u32 = 14;
const W_LOOP_CONFIG: u32 = 9;
const W_AGENTS_MD: u32 = 9;
const W_SKILLS_TWO_PLUS: u32 = 14;
const W_SKILLS_ONE: u32 = 7;
const W_VERIFIER: u32 = 14;
const W_SAFETY_LOOP_MD: u32 = 4;
const W_SAFETY_DOC: u32 = 4;
const W_GITHUB: u32 = 6;
const W_GITHUB_WORKFLOWS: u32 = 4;
const W_MCP: u32 = 3;
const W_WORKTREE: u32 = 3;
const W_REGISTRY: u32 = 2;
const W_BUDGET_DOC: u32 = 3;
const W_RUN_LOG: u32 = 3;
const W_LOOP_MD_BUDGET: u32 = 2;
const W_BUDGET_SKILL: u32 = 2;
const W_TOOL_SCOPE: u32 = 3;
const W_STALL_DETECTION: u32 = 3;
const W_ESCALATION: u32 = 3;
const W_GATE_YAML: u32 = 3;
const W_CONSTRAINTS_FILE: u32 = 4;
const W_CONSTRAINTS_SKILL: u32 = 2;
const W_LOOP_ACTIVITY: u32 = 14;
const W_HARNESS_STACK: u32 = 4;
const W_HARNESS_LOCK: u32 = 1;
const W_HARNESS_SESSIONS: u32 = 2;
const W_HARNESS_EMIT: u32 = 1;
const W_HARNESS_HOST: u32 = 1;
const W_MEMORY_TIERS: u32 = 4;
const W_MEMORY_BUDGET: u32 = 2;
const W_FLEET_REGISTRY: u32 = 4;
const W_FLEET_INBOX: u32 = 2;

/// Loop skill names: the method's and agent-mux's own.
const LOOP_SKILL_NAMES: &[&str] = &[
    // the method's names
    "loop-triage",
    "minimal-fix",
    "loop-verifier",
    "pr-review-triage",
    "ci-triage",
    "post-merge-scan",
    "dependency-triage",
    "rebase-and-clean",
    "changelog-scan",
    "loop-constraints",
    "draft-release-notes",
    "issue-triage",
    // agent-mux's names
    "loop-pr-triage",
    "loop-ci-triage",
    "loop-post-merge",
    "loop-dependency-triage",
    "loop-changelog",
    "loop-issue-triage",
    "loop-fix",
    "loop-rules",
];

/// Skills whose presence means the workspace can triage.
const TRIAGE_SKILL_NAMES: &[&str] = &[
    "loop-triage",
    "pr-review-triage",
    "ci-triage",
    "dependency-triage",
    "post-merge-scan",
    "changelog-scan",
    "issue-triage",
    "loop-pr-triage",
    "loop-ci-triage",
    "loop-post-merge",
    "loop-dependency-triage",
    "loop-changelog",
    "loop-issue-triage",
];

const CONSTRAINTS_SKILL_NAMES: &[&str] = &["loop-constraints", "loop-rules"];
const BUDGET_SKILL_NAMES: &[&str] = &["loop-budget"];
const STALL_SKILL_NAMES: &[&str] = &["loop-context", "loop-guard"];

const SKILL_DIRS: &[&str] = &[".grok/skills", ".claude/skills", ".codex/skills", "skills"];
const AGENT_DIRS: &[&str] = &[".claude/agents", ".codex/agents"];

const BUDGET_HINTS: &[&str] = &[
    "budget",
    "max tokens",
    "token cap",
    "kill switch",
    "loop-pause-all",
];
const SAFETY_HINTS: &[&str] = &["gate", "denylist", "auto-merge", "safety"];
const MCP_HINTS: &[&str] = &["mcp", "mcp server", "plugins & connectors"];
const WORKTREE_HINTS: &[&str] = &["worktree", "worktrees", "git worktree"];
const TOOL_SCOPE_HINTS: &[&str] = &[
    "least-privilege",
    "least privilege",
    "tool scope",
    "scoped tool",
    "allow-list",
    "allowlist",
    "read-only tool",
    "permission scope",
];
const STALL_HINTS: &[&str] = &[
    "loop-context",
    "circuit breaker",
    "max attempts",
    "no-progress",
    "no progress",
    "same error",
];
const STALL_WORDS: &[&str] = &["stall", "stalled", "stalls", "stalling", "stuck"];
const ESCALATION_HINTS: &[&str] = &[
    "escalat",
    "handoff",
    "hand-off",
    "hand off",
    "human-in-the-loop",
    "human in the loop",
    "hitl",
    "human review",
    "need human",
    "needs human",
    "stop and ask",
    "exit code 2",
    "exit 2",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingLevel {
    Ok,
    Warn,
    Fail,
}

impl FindingLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            FindingLevel::Ok => "ok",
            FindingLevel::Warn => "warn",
            FindingLevel::Fail => "fail",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            FindingLevel::Ok => "✓",
            FindingLevel::Warn => "!",
            FindingLevel::Fail => "✗",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: FindingLevel,
    pub message: String,
}

/// The audit of one workspace.
#[derive(Debug, Clone)]
pub struct Audit {
    pub target: PathBuf,
    pub score: u32,
    /// `None` is L0.
    pub level: Option<Level>,
    pub assessment: String,
    pub signals: Value,
    pub findings: Vec<Finding>,
    pub recommendations: Vec<String>,
    /// The state file found, first of `STATE_FILES` present.
    pub state_file: Option<String>,
    /// The state file's `Last run` parses and is older than the window.
    pub state_stale: bool,
    /// `state:<file>:fresh`, `log:loop-run-log.md:fresh`, `git:<subject>`,
    /// `store:<n> runs`; at most four.
    pub activity_evidence: Vec<String>,
}

/// Everything the detectors found, before scoring.
#[derive(Debug, Default)]
struct Signals {
    state_paths: Vec<String>,
    loop_md: bool,
    agents_md: bool,
    loop_skills: Vec<String>,
    verifier: bool,
    triage: bool,
    safety_loop_md: bool,
    safety_doc: bool,
    github: bool,
    github_workflows: bool,
    mcp: bool,
    worktree: bool,
    registry: bool,
    budget_doc: bool,
    run_log: bool,
    loop_md_budget: bool,
    budget_skill: bool,
    tool_scope: bool,
    stall_detection: bool,
    escalation: bool,
    gate_yaml: bool,
    constraints_file: bool,
    constraints_skill: bool,
    activity: Vec<String>,
    harness_stack: bool,
    harness_lock: bool,
    harness_sessions: bool,
    harness_emit: bool,
    harness_host: bool,
    memory_tiers: bool,
    memory_budget: bool,
    fleet_registry: bool,
    fleet_inbox: bool,
    /// A state file exists whose `Last run` is older than the window.
    stale_state: Option<String>,
}

impl Signals {
    fn state_present(&self) -> bool {
        !self.state_paths.is_empty()
    }

    fn cost_ready(&self) -> bool {
        self.budget_doc && self.run_log && self.loop_md_budget
    }

    fn activity_present(&self) -> bool {
        !self.activity.is_empty()
    }

    fn score(&self) -> u32 {
        let mut s = W_BASE;
        let mut add = |on: bool, w: u32| {
            if on {
                s += w;
            }
        };
        add(self.state_present(), W_STATE_FILE);
        add(self.triage, W_TRIAGE);
        add(self.loop_md, W_LOOP_CONFIG);
        add(self.agents_md, W_AGENTS_MD);
        match self.loop_skills.len() {
            0 => {}
            1 => add(true, W_SKILLS_ONE),
            _ => add(true, W_SKILLS_TWO_PLUS),
        }
        add(self.verifier, W_VERIFIER);
        add(self.safety_loop_md, W_SAFETY_LOOP_MD);
        add(self.safety_doc, W_SAFETY_DOC);
        add(self.github, W_GITHUB);
        add(self.github_workflows, W_GITHUB_WORKFLOWS);
        add(self.mcp, W_MCP);
        add(self.worktree, W_WORKTREE);
        add(self.registry, W_REGISTRY);
        add(self.budget_doc, W_BUDGET_DOC);
        add(self.run_log, W_RUN_LOG);
        add(self.loop_md_budget, W_LOOP_MD_BUDGET);
        add(self.budget_skill, W_BUDGET_SKILL);
        add(self.tool_scope, W_TOOL_SCOPE);
        add(self.stall_detection, W_STALL_DETECTION);
        add(self.escalation, W_ESCALATION);
        add(self.gate_yaml, W_GATE_YAML);
        add(self.constraints_file, W_CONSTRAINTS_FILE);
        add(self.constraints_skill, W_CONSTRAINTS_SKILL);
        add(self.activity_present(), W_LOOP_ACTIVITY);
        add(self.harness_stack, W_HARNESS_STACK);
        add(self.harness_lock, W_HARNESS_LOCK);
        add(self.harness_sessions, W_HARNESS_SESSIONS);
        add(self.harness_emit, W_HARNESS_EMIT);
        add(self.harness_host, W_HARNESS_HOST);
        add(self.memory_tiers, W_MEMORY_TIERS);
        add(self.memory_budget, W_MEMORY_BUDGET);
        add(self.fleet_registry, W_FLEET_REGISTRY);
        add(self.fleet_inbox, W_FLEET_INBOX);
        s.min(100)
    }

    fn to_json(&self) -> Value {
        json!({
            "stateFile": { "present": self.state_present(), "paths": self.state_paths },
            "loopConfig": { "present": self.loop_md, "path": if self.loop_md { Value::from("LOOP.md") } else { Value::Null } },
            "skills": { "count": self.loop_skills.len(), "loopSkills": self.loop_skills },
            "verifier": { "present": self.verifier },
            "triage": { "present": self.triage },
            "agentsMd": { "present": self.agents_md },
            "safety": { "loopMdMentionsSafety": self.safety_loop_md, "safetyDocPresent": self.safety_doc },
            "github": { "present": self.github, "workflows": self.github_workflows },
            "mcp": { "present": self.mcp },
            "constraints": { "present": self.constraints_file, "hasConstraintsSkill": self.constraints_skill },
            "worktreeEvidence": { "present": self.worktree },
            "registry": { "present": self.registry },
            "cost": { "budgetDoc": self.budget_doc, "runLog": self.run_log, "loopMdBudget": self.loop_md_budget, "budgetSkill": self.budget_skill },
            "governance": { "toolScope": self.tool_scope, "stallDetection": self.stall_detection, "escalation": self.escalation, "gateYaml": self.gate_yaml },
            "loopActivity": { "present": self.activity_present(), "evidence": self.activity },
            "harness": { "stack": self.harness_stack, "lock": self.harness_lock, "sessions": self.harness_sessions, "emit": self.harness_emit, "host": self.harness_host },
            "memory": { "tiers": self.memory_tiers, "budget": self.memory_budget },
            "fleet": { "registry": self.fleet_registry, "inbox": self.fleet_inbox },
        })
    }
}

impl Audit {
    pub fn level_str(&self) -> &'static str {
        match self.level {
            None => "L0",
            Some(l) => l.as_str(),
        }
    }

    /// The signals the level gates read: state file, triage skill,
    /// verifier, cost observability, fresh activity.
    fn gates(&self) -> (bool, bool, bool, bool, bool) {
        let sig = &self.signals;
        let flag = |path: &[&str]| -> bool {
            let mut v = sig;
            for p in path {
                v = v.get(p).unwrap_or(&Value::Null);
            }
            v.as_bool().unwrap_or(false)
        };
        (
            flag(&["stateFile", "present"]),
            flag(&["triage", "present"]),
            flag(&["verifier", "present"]),
            flag(&["cost", "budgetDoc"])
                && flag(&["cost", "runLog"])
                && flag(&["cost", "loopMdBudget"]),
            flag(&["loopActivity", "present"]),
        )
    }

    /// What `level` still needs in this workspace, in plain words for the
    /// screen; empty exactly when [`Audit::allows`] is `Ok`.
    pub fn missing_for(&self, level: Level) -> Vec<String> {
        self.missing_for_with(level, false)
    }

    /// [`Audit::missing_for`] for a loop that may bypass the score: at L1
    /// and L2 `bypass_score` drops the score threshold and keeps the rest
    /// (a state file, a triage skill). L3 always needs its score.
    pub fn missing_for_with(&self, level: Level, bypass_score: bool) -> Vec<String> {
        let (state, triage, verifier, cost_ready, activity) = self.gates();
        let score = self.score;
        let mut out = Vec::new();
        let threshold = match level {
            Level::L1 => THRESHOLD_L1,
            Level::L2 => THRESHOLD_L2,
            Level::L3 => THRESHOLD_L3,
        };
        let score_counts = !(bypass_score && level < Level::L3);
        if score_counts && score < threshold {
            out.push(format!("a readiness score of {threshold} (now {score})"));
        }
        match level {
            Level::L1 => {
                if !state {
                    out.push("a state file".into());
                }
            }
            Level::L2 => {
                if !triage {
                    out.push("a triage skill".into());
                }
            }
            Level::L3 => {
                if !verifier {
                    out.push("a verifier".into());
                }
                if !state {
                    out.push("a state file".into());
                }
                if !cost_ready {
                    out.push("cost observability (budget doc, run log, LOOP.md budget)".into());
                }
                if !activity {
                    out.push("loop activity in the last 14 days".into());
                }
            }
        }
        out
    }

    /// Whether the workspace may run at `level`, else why not.
    pub fn allows(&self, level: Level) -> Result<(), String> {
        self.allows_with(level, false)
    }

    /// [`Audit::allows`] for a loop that may bypass the score at L1 and L2
    /// (`LoopEntry::bypass_score`): the other gates still hold.
    pub fn allows_with(&self, level: Level, bypass_score: bool) -> Result<(), String> {
        let (state, triage, verifier, cost_ready, activity) = self.gates();
        let score = self.score;
        let over = |t: u32| score >= t || bypass_score;
        match level {
            Level::L1 => {
                if over(THRESHOLD_L1) && state {
                    Ok(())
                } else {
                    Err(format!(
                        "L1 needs score ≥ {THRESHOLD_L1} and a state file (score {score}{})",
                        if state { "" } else { ", no state file" }
                    ))
                }
            }
            Level::L2 => {
                if over(THRESHOLD_L2) && triage {
                    Ok(())
                } else {
                    Err(format!(
                        "L2 needs score ≥ {THRESHOLD_L2} and a triage skill (score {score}{})",
                        if triage { "" } else { ", no triage skill" }
                    ))
                }
            }
            Level::L3 => {
                let mut missing = Vec::new();
                if score < THRESHOLD_L3 {
                    missing.push(format!("score {score}"));
                }
                if !verifier {
                    missing.push("no verifier".into());
                }
                if !state {
                    missing.push("no state file".into());
                }
                if !cost_ready {
                    missing.push("missing cost observability".into());
                }
                if !activity {
                    missing.push("no fresh activity".into());
                }
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "L3 needs score ≥ {THRESHOLD_L3}, a verifier, a state file, cost observability and fresh activity ({})",
                        missing.join(", ")
                    ))
                }
            }
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "target": self.target.to_string_lossy(),
            "score": self.score,
            "level": self.level_str(),
            "assessment": self.assessment,
            "signals": self.signals,
            "findings": self.findings.iter().map(|f| json!({ "level": f.level.as_str(), "message": f.message })).collect::<Vec<_>>(),
            "recommendations": self.recommendations,
        })
    }

    /// Twenty cells of `█`/`░` followed by the score.
    pub fn score_bar(&self) -> String {
        let filled = ((self.score as f64 / 100.0) * 20.0).round() as usize;
        let filled = filled.min(20);
        format!(
            "{}{}  {}/100",
            "█".repeat(filled),
            "░".repeat(20 - filled),
            self.score
        )
    }

    pub fn human_lines(&self) -> Vec<String> {
        let mut out = vec![
            format!("Loop readiness · {}", self.target.display()),
            format!("Score: {}/100  Level: {}", self.score, self.level_str()),
            self.score_bar(),
            self.assessment.clone(),
            String::new(),
            "Findings:".to_string(),
        ];
        for f in &self.findings {
            out.push(format!("  {} {}", f.level.glyph(), f.message));
        }
        if !self.recommendations.is_empty() {
            out.push(String::new());
            out.push("Recommendations:".to_string());
            for r in &self.recommendations {
                out.push(format!("  → {r}"));
            }
        }
        out
    }
}

/// Audits `root`. `store_runs_within_14_days` is the number of completed
/// loop runs the trace store holds for this workspace inside the window;
/// the caller has the store, this module does not.
pub fn audit(root: &Path, store_runs_within_14_days: usize) -> Audit {
    let root = root.to_path_buf();
    let sig = detect(&root, store_runs_within_14_days, OffsetDateTime::now_utc());
    build(root, sig)
}

fn build(root: PathBuf, sig: Signals) -> Audit {
    let score = sig.score();
    let cost_ready = sig.cost_ready();
    let activity = sig.activity_present();
    let l3_ready = cost_ready && activity;
    let level = if score >= THRESHOLD_L3 && sig.verifier && sig.state_present() && l3_ready {
        Some(Level::L3)
    } else if score >= THRESHOLD_L2 && sig.triage {
        Some(Level::L2)
    } else if score >= THRESHOLD_L1 && sig.state_present() {
        Some(Level::L1)
    } else {
        None
    };
    let assessment = if score >= 82 && l3_ready {
        "Strong loop readiness: a candidate for L3 with explicit gates."
    } else if score >= 82 && !cost_ready {
        "Strong signals, but cost observability is incomplete (loop-budget.md, loop-run-log.md, a budget section in LOOP.md): add it before L3."
    } else if score >= 82 {
        "Strong structure, but no proven loop run yet: run one report-only cycle before L3."
    } else if score >= 62 {
        "Good foundation: add the verifier and safety documents for L3."
    } else if score >= 42 {
        "Early setup: focus on the L1 state file and triage before enabling actions."
    } else {
        "Not loop-ready: scaffold a pattern from the Loops sidebar."
    }
    .to_string();

    let mut findings = Vec::new();
    let mut recs = Vec::new();
    let mut add = |level: FindingLevel, msg: String, rec: Option<&str>| {
        findings.push(Finding {
            level,
            message: msg,
        });
        if let Some(r) = rec {
            recs.push(r.to_string());
        }
    };

    match sig.state_paths.first() {
        Some(p) => add(FindingLevel::Ok, format!("State file: {p}"), None),
        None => add(
            FindingLevel::Fail,
            "No state file (STATE.md or <pattern>-state.md)".into(),
            Some(
                "Add a state file with a `Last run:` line and the High Priority / Watch List / Recent Noise sections",
            ),
        ),
    }
    if let Some(p) = &sig.stale_state {
        add(
            FindingLevel::Warn,
            format!("{p} Last run is older than 14 days — files on disk are not loop activity"),
            Some("Run one report-only cycle and let the loop update `Last run:`"),
        );
    }
    if sig.loop_md {
        add(FindingLevel::Ok, "LOOP.md documents the loop".into(), None);
    } else {
        add(
            FindingLevel::Fail,
            "No LOOP.md".into(),
            Some("Add LOOP.md with the active loops, human gates, budget and kill switch"),
        );
    }
    if sig.triage {
        add(FindingLevel::Ok, "Triage skill present".into(), None);
    } else {
        add(
            FindingLevel::Warn,
            "No triage skill".into(),
            Some("Scaffold a pattern so its triage skill lands in the harness skill directory"),
        );
    }
    match sig.loop_skills.len() {
        0 => add(
            FindingLevel::Warn,
            "No loop skills found".into(),
            Some("Install the pattern's skills (triage, fix, rules)"),
        ),
        n => add(
            FindingLevel::Ok,
            format!("{n} loop skill(s): {}", sig.loop_skills.join(", ")),
            None,
        ),
    }
    if sig.verifier {
        add(FindingLevel::Ok, "Verifier agent present".into(), None);
    } else {
        add(
            FindingLevel::Warn,
            "No verifier agent".into(),
            Some("Add the loop-verifier agent so proposed fixes are checked before the inbox"),
        );
    }
    if sig.agents_md {
        add(
            FindingLevel::Ok,
            "AGENTS.md or CLAUDE.md present".into(),
            None,
        );
    } else {
        add(
            FindingLevel::Warn,
            "No AGENTS.md".into(),
            Some("Add AGENTS.md with the test and lint commands and the week-one rule"),
        );
    }
    if sig.safety_loop_md {
        add(
            FindingLevel::Ok,
            "LOOP.md mentions gates or safety".into(),
            None,
        );
    } else {
        add(
            FindingLevel::Warn,
            "LOOP.md does not mention gates, denylist or safety".into(),
            None,
        );
    }
    if !sig.safety_doc {
        add(
            FindingLevel::Warn,
            "No safety document (docs/safety.md or SECURITY.md)".into(),
            None,
        );
    }
    if sig.budget_doc {
        add(FindingLevel::Ok, "loop-budget.md present".into(), None);
    } else {
        add(
            FindingLevel::Warn,
            "No loop-budget.md".into(),
            Some("Add loop-budget.md with daily caps and the kill switch"),
        );
    }
    if sig.run_log {
        add(FindingLevel::Ok, "loop-run-log.md present".into(), None);
    } else {
        add(
            FindingLevel::Warn,
            "No loop-run-log.md".into(),
            Some("Add loop-run-log.md so runs are observable"),
        );
    }
    if !sig.loop_md_budget {
        add(
            FindingLevel::Warn,
            "LOOP.md has no budget section".into(),
            Some("Add a Budget section to LOOP.md (daily caps, kill switch)"),
        );
    }
    if sig.gate_yaml {
        add(FindingLevel::Ok, "gate.yaml present".into(), None);
    } else {
        add(
            FindingLevel::Warn,
            "No gate.yaml".into(),
            Some("Add gate.yaml with a path denylist and a file cap"),
        );
    }
    if sig.constraints_file {
        add(FindingLevel::Ok, "loop-constraints.md present".into(), None);
    } else {
        add(FindingLevel::Warn, "No loop-constraints.md".into(), None);
    }
    if activity {
        add(
            FindingLevel::Ok,
            format!("Loop activity within 14 days: {}", sig.activity.join(", ")),
            None,
        );
    } else {
        add(
            FindingLevel::Warn,
            "No loop activity in the last 14 days".into(),
            Some("Schedule a report-only run; the run log and the store then count as activity"),
        );
    }
    if score >= THRESHOLD_L3 && sig.verifier && sig.state_present() && !cost_ready {
        add(
            FindingLevel::Warn,
            "Score qualifies for L3 but cost observability is incomplete — capped at L2 until budget + run log + LOOP.md budget exist".into(),
            None,
        );
    }
    if score >= THRESHOLD_L3 && sig.verifier && sig.state_present() && cost_ready && !activity {
        add(
            FindingLevel::Warn,
            "Score qualifies for L3 but no proven loop activity yet — capped at L2 until one loop cycle has run".into(),
            None,
        );
    }

    Audit {
        target: root,
        score,
        level,
        assessment,
        signals: sig.to_json(),
        findings,
        recommendations: recs,
        state_file: sig.state_paths.first().cloned(),
        state_stale: sig.stale_state.is_some(),
        activity_evidence: sig.activity.clone(),
    }
}

// ---------------------------------------------------------------------
// detectors

fn detect(root: &Path, store_runs: usize, now: OffsetDateTime) -> Signals {
    let mut sig = Signals::default();

    // state files, freshness and staleness
    for name in STATE_FILES {
        let path = root.join(name);
        if !path.is_file() {
            continue;
        }
        sig.state_paths.push(name.to_string());
        if let Some(text) = read(&path)
            && let Some(t) = last_run_timestamp(&text)
        {
            if is_fresh(t, now) {
                sig.activity.push(format!("state:{name}:fresh"));
            } else if t < now && sig.stale_state.is_none() {
                sig.stale_state = Some(name.to_string());
            }
        }
    }

    let loop_md = read(&root.join("LOOP.md")).unwrap_or_default();
    sig.loop_md = root.join("LOOP.md").is_file();
    sig.agents_md = root.join("AGENTS.md").is_file() || root.join("CLAUDE.md").is_file();

    // skills and the verifier
    let (found, any_skill_has_tool_scope) = find_skills(root);
    sig.loop_skills = found
        .iter()
        .filter(|s| LOOP_SKILL_NAMES.contains(&s.as_str()))
        .cloned()
        .collect();
    sig.loop_skills.sort();
    sig.loop_skills.dedup();
    sig.verifier = found.iter().any(|s| s == "loop-verifier");
    sig.triage = found
        .iter()
        .any(|s| TRIAGE_SKILL_NAMES.contains(&s.as_str()));
    sig.constraints_skill = found
        .iter()
        .any(|s| CONSTRAINTS_SKILL_NAMES.contains(&s.as_str()));
    sig.budget_skill = found
        .iter()
        .any(|s| BUDGET_SKILL_NAMES.contains(&s.as_str()));
    let stall_skill = found
        .iter()
        .any(|s| STALL_SKILL_NAMES.contains(&s.as_str()));

    // safety
    sig.safety_loop_md = contains_any_ci(&loop_md, SAFETY_HINTS);
    let safety_doc = read(&root.join("docs").join("safety.md"))
        .or_else(|| read(&root.join("safety.md")))
        .or_else(|| read(&root.join("SECURITY.md")));
    sig.safety_doc = safety_doc.is_some();

    // github
    sig.github = root.join(".github").is_dir();
    sig.github_workflows = dir_has(&root.join(".github").join("workflows"), |n| {
        n.ends_with(".yml") || n.ends_with(".yaml")
    });

    // mcp, worktrees, registry
    sig.mcp = root.join(".mcp.json").is_file()
        || root.join("mcp.json").is_file()
        || root.join(".mcp").join("config.json").is_file()
        || contains_any_ci(&loop_md, MCP_HINTS);
    let operating = read(&root.join("docs").join("operating-loops.md")).unwrap_or_default();
    sig.worktree =
        contains_any_ci(&loop_md, WORKTREE_HINTS) || contains_any_ci(&operating, WORKTREE_HINTS);
    sig.registry = [
        "registry.yaml",
        "registry.yml",
        "registry.toml",
        "registry.json",
    ]
    .iter()
    .any(|n| root.join("patterns").join(n).is_file());

    // cost observability
    sig.budget_doc = root.join("loop-budget.md").is_file();
    sig.run_log = root.join("loop-run-log.md").is_file();
    sig.loop_md_budget = contains_any_ci(&loop_md, BUDGET_HINTS);

    // governance corpus
    let constraints = read(&root.join("loop-constraints.md"));
    sig.constraints_file = constraints.is_some();
    let mut corpus = String::new();
    corpus.push_str(&loop_md);
    corpus.push('\n');
    if let Some(s) = &safety_doc {
        corpus.push_str(s);
        corpus.push('\n');
    }
    if let Some(s) = read(&root.join("SECURITY.md")) {
        corpus.push_str(&s);
        corpus.push('\n');
    }
    if let Some(s) = &constraints {
        corpus.push_str(s);
    }
    sig.tool_scope = any_skill_has_tool_scope || contains_any_ci(&corpus, TOOL_SCOPE_HINTS);
    sig.stall_detection = stall_skill
        || dir_has(root, |n| n.to_ascii_lowercase().contains("ledger"))
        || contains_any_ci(&corpus, STALL_HINTS)
        || contains_any_word_ci(&corpus, STALL_WORDS);
    sig.escalation = contains_any_ci(&corpus, ESCALATION_HINTS);
    sig.gate_yaml = root.join("gate.yaml").is_file();

    // activity: run log, git, store
    if let Some(log) = read(&root.join("loop-run-log.md"))
        && run_log_is_fresh(&log, now)
    {
        sig.activity.push("log:loop-run-log.md:fresh".into());
    }
    if let Some(subject) = git_recent_activity(root) {
        sig.activity.push(format!("git:{subject}"));
    }
    if store_runs > 0 {
        sig.activity.push(format!("store:{store_runs} runs"));
    }
    sig.activity.dedup();
    sig.activity.truncate(4);

    // harness, memory, fleet (recognized for parity)
    let foundry = root.join(".foundry");
    let stack = read(&foundry.join("stack.yaml"));
    sig.harness_stack = stack.is_some();
    sig.harness_lock =
        foundry.join("stack.lock").is_file() || root.join(".stack.lock.yaml").is_file();
    sig.harness_sessions = dir_has(&foundry.join("sessions"), |n| n != ".gitkeep");
    sig.harness_emit = stack
        .as_deref()
        .is_some_and(|s| contains_any_ci(s, &["emit/outerloop-evidence", "outerloop"]))
        || foundry.join("hooks").join("outerloop.yaml").is_file();
    sig.harness_host = foundry.join("host").join("cursor").exists()
        || foundry.join("host").join("claude-code").exists()
        || root
            .join(".cursor")
            .join("rules")
            .join("foundry.mdc")
            .is_file()
        || root.join(".claude").join("foundry.md").is_file()
        || contains_any_ci(&loop_md, &["foundry host integrate", "harness-foundry"]);
    sig.memory_tiers = root.join("memory-tiers.md").is_file();
    sig.memory_budget = root.join("memory-budget.md").is_file();
    sig.fleet_registry = root.join("fleet-registry.md").is_file();
    sig.fleet_inbox = root.join("fleet-inbox.md").is_file();

    sig
}

/// Skill names found in the harness skill directories plus verifier
/// agents; and whether any `SKILL.md` declares `allowed-tools:`.
fn find_skills(root: &Path) -> (Vec<String>, bool) {
    let mut names = Vec::new();
    let mut tool_scope = false;
    for dir in SKILL_DIRS {
        let base = root.join(dir);
        if base.join("SKILL.md").is_file() {
            names.push("root-skill".to_string());
            if has_allowed_tools(&base.join("SKILL.md")) {
                tool_scope = true;
            }
        }
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if has_allowed_tools(&entry.path().join("SKILL.md")) {
                tool_scope = true;
            }
            names.push(name);
        }
    }
    for dir in AGENT_DIRS {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name
                .strip_suffix(".md")
                .or_else(|| name.strip_suffix(".toml"))
                .unwrap_or(&name);
            if stem.to_ascii_lowercase().contains("verifier") {
                names.push("loop-verifier".to_string());
            }
        }
    }
    for name in ["opencode.json", "opencode.json.example"] {
        if let Some(text) = read(&root.join(name))
            && text.to_ascii_lowercase().contains("verifier")
        {
            names.push("loop-verifier".to_string());
        }
    }
    (names, tool_scope)
}

fn has_allowed_tools(skill_md: &Path) -> bool {
    let Some(text) = read(skill_md) else {
        return false;
    };
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("allowed-tools") {
        let after = &lower[from + i + "allowed-tools".len()..];
        if after.trim_start().starts_with(':') {
            return true;
        }
        from += i + 1;
    }
    false
}

/// The `Last run: <timestamp>` line of a state file. The timestamp must
/// start with `YYYY-MM-DD`; an optional time follows after `T` or a space,
/// with an optional `Z` or `±HH:MM` offset. Anything else (the template's
/// placeholder, prose) is no timestamp.
pub fn last_run_timestamp(state_text: &str) -> Option<OffsetDateTime> {
    let lower = state_text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("last run:") {
        let start = from + i + "last run:".len();
        let rest = &state_text[start..];
        let rest = rest.trim_start_matches([' ', '\t']);
        if let Some(raw) = leading_timestamp(rest)
            && let Some(t) = parse_timestamp(raw)
        {
            return Some(t);
        }
        from = start;
    }
    None
}

/// The longest prefix of `s` shaped like a timestamp.
fn leading_timestamp(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let date_ok = b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit);
    if !date_ok {
        return None;
    }
    let mut end = 10;
    if b.len() > 11 && (b[10] == b'T' || b[10] == b' ') {
        let mut j = 11;
        while j < b.len() && (b[j].is_ascii_digit() || b[j] == b':' || b[j] == b'.') {
            j += 1;
        }
        if j > 11 {
            end = j;
            if j < b.len() && b[j] == b'Z' {
                end = j + 1;
            } else if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                let mut k = j + 1;
                while k < b.len() && (b[k].is_ascii_digit() || b[k] == b':') {
                    k += 1;
                }
                if k > j + 1 {
                    end = k;
                }
            }
        }
    }
    Some(&s[..end])
}

fn is_fresh(t: OffsetDateTime, now: OffsetDateTime) -> bool {
    let age = now - t;
    age <= ACTIVITY_MAX_AGE && age >= -time::Duration::try_from(FUTURE_TOLERANCE).unwrap()
}

/// Any JSON line whose `run_id` is a fresh timestamp.
fn run_log_is_fresh(text: &str, now: OffsetDateTime) -> bool {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{') && l.ends_with('}'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v.get("run_id").and_then(Value::as_str).map(str::to_string))
        .filter_map(|id| parse_timestamp(&id))
        .any(|t| is_fresh(t, now))
}

/// The subject of the newest commit within fourteen days that touched a
/// state or run-log file, cut to sixty characters. `None` when `git` is
/// missing, the directory is not a repository, or the probe outlasts its
/// budget.
fn git_recent_activity(root: &Path) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("git")
        .args([
            "log",
            "--since=14.days",
            "--oneline",
            "--max-count=1",
            "--",
            "STATE.md",
            "loop-run-log.md",
            "*state.md",
            "*-state.md",
        ])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {
                if started.elapsed() > GIT_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next()?.trim();
    if first.is_empty() {
        return None;
    }
    Some(first.chars().take(60).collect())
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn dir_has(dir: &Path, pred: impl Fn(&str) -> bool) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|e| pred(&e.file_name().to_string_lossy()))
}

fn contains_any_ci(text: &str, needles: &[&str]) -> bool {
    let lower = text.to_ascii_lowercase();
    needles.iter().any(|n| lower.contains(n))
}

/// Whole-word, case-insensitive match of any needle.
fn contains_any_word_ci(text: &str, words: &[&str]) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .any(|w| {
            let w = w.to_ascii_lowercase();
            words.contains(&w.as_str())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loops::format_timestamp;
    use std::fs;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/loops")
    }

    /// Copies a fixture into a fresh temp dir (outside any git repository,
    /// so the git probe never sees this repository's own history).
    fn copy_fixture(name: &str) -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        copy_tree(&fixtures().join(name), temp.path());
        temp
    }

    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap().flatten() {
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn set_last_run(root: &Path, file: &str, when: OffsetDateTime) {
        let path = root.join(file);
        let text = fs::read_to_string(&path).unwrap();
        let replaced: Vec<String> = text
            .lines()
            .map(|l| {
                if l.starts_with("Last run:") {
                    format!("Last run: {}", format_timestamp(when))
                } else {
                    l.to_string()
                }
            })
            .collect();
        fs::write(&path, replaced.join("\n") + "\n").unwrap();
    }

    fn flag(a: &Audit, path: &[&str]) -> bool {
        let mut v = &a.signals;
        for p in path {
            v = v.get(p).unwrap();
        }
        v.as_bool().unwrap()
    }

    #[test]
    fn last_run_line_is_parsed_strictly() {
        assert!(last_run_timestamp("Last run: (set by loop on each run)").is_none());
        assert!(last_run_timestamp("nothing here").is_none());
        let t =
            last_run_timestamp("# State\n\nLast run: 2026-09-15T08:00:39Z (automated)\n").unwrap();
        assert_eq!(format_timestamp(t), "2026-09-15T08:00:39Z");
        let bare = last_run_timestamp("last run: 2026-09-15\n").unwrap();
        assert_eq!(format_timestamp(bare), "2026-09-15T00:00:00Z");
        let spaced = last_run_timestamp("Last run: 2026-09-15 08:00:39+02:00").unwrap();
        assert_eq!(format_timestamp(spaced), "2026-09-15T06:00:39Z");
        // a first, unparseable line does not hide a later good one
        let later = last_run_timestamp("Last run: soon\nLast run: 2026-01-02").unwrap();
        assert_eq!(format_timestamp(later), "2026-01-02T00:00:00Z");
    }

    #[test]
    fn freshness_is_a_fourteen_day_window_with_a_minute_of_tolerance() {
        let now = OffsetDateTime::now_utc();
        assert!(is_fresh(now - time::Duration::hours(1), now));
        assert!(is_fresh(now - time::Duration::days(13), now));
        assert!(!is_fresh(now - time::Duration::days(15), now));
        assert!(is_fresh(now + time::Duration::seconds(30), now));
        assert!(
            !is_fresh(now + time::Duration::minutes(2), now),
            "forged future"
        );
    }

    #[test]
    fn missing_for_agrees_with_allows() {
        for fixture in ["empty", "minimal"] {
            let temp = copy_fixture(fixture);
            let a = audit(temp.path(), 0);
            for level in [Level::L1, Level::L2, Level::L3] {
                for bypass in [false, true] {
                    assert_eq!(
                        a.allows_with(level, bypass).is_ok(),
                        a.missing_for_with(level, bypass).is_empty(),
                        "{fixture} {level:?} bypass {bypass}: {:?}",
                        a.missing_for_with(level, bypass)
                    );
                }
            }
        }
        let temp = copy_fixture("empty");
        let a = audit(temp.path(), 0);
        assert_eq!(
            a.missing_for(Level::L1),
            vec!["a readiness score of 38 (now 7)", "a state file"]
        );
    }

    #[test]
    fn bypassing_the_score_keeps_the_other_gates_and_never_reaches_l3() {
        let temp = copy_fixture("empty");
        let a = audit(temp.path(), 0);
        assert_eq!(a.missing_for_with(Level::L1, true), vec!["a state file"]);
        assert_eq!(a.missing_for_with(Level::L2, true), vec!["a triage skill"]);
        assert!(
            a.missing_for_with(Level::L3, true)
                .contains(&"a readiness score of 78 (now 7)".to_string()),
            "L3 keeps its score"
        );
        assert!(
            a.allows_with(Level::L1, true).is_err(),
            "still no state file"
        );
        // a triage skill alone: L2 needs no state file, only the score
        let skill = temp.path().join(".claude/skills/loop-triage");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: loop-triage\n---\n").unwrap();
        let a = audit(temp.path(), 0);
        assert!(a.score < THRESHOLD_L2, "score {}", a.score);
        assert!(a.allows(Level::L2).is_err());
        assert!(
            a.allows_with(Level::L2, true).is_ok(),
            "{:?}",
            a.missing_for_with(Level::L2, true)
        );
        // a stale state file alone: present, but under the L1 score
        let temp = copy_fixture("empty");
        std::fs::write(
            temp.path().join("STATE.md"),
            "Last run: 2020-01-01T00:00:00Z\n",
        )
        .unwrap();
        let a = audit(temp.path(), 0);
        assert!(a.score < THRESHOLD_L1, "score {}", a.score);
        assert!(a.allows(Level::L1).is_err());
        assert!(a.allows_with(Level::L1, true).is_ok());
        assert!(a.allows_with(Level::L3, true).is_err());
    }

    #[test]
    fn the_empty_fixture_is_l0_with_the_base_score() {
        // base 7 only
        let temp = copy_fixture("empty");
        let a = audit(temp.path(), 0);
        assert_eq!(a.score, 7);
        assert_eq!(a.level, None);
        assert_eq!(a.level_str(), "L0");
        assert!(a.state_file.is_none());
        assert!(a.allows(Level::L1).is_err());
        assert!(
            a.findings
                .iter()
                .any(|f| f.level == FindingLevel::Fail && f.message.starts_with("No state file"))
        );
        assert!(a.assessment.starts_with("Not loop-ready"));
        let json = a.to_json();
        for key in [
            "target",
            "score",
            "level",
            "assessment",
            "signals",
            "findings",
            "recommendations",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert_eq!(json["signals"]["stateFile"]["present"], false);
        assert_eq!(json["findings"][0]["level"], "fail");
    }

    #[test]
    fn the_minimal_fixture_is_l2_without_activity() {
        // base 7 + state 18 + triage 14 + LOOP.md 9 + skills≥2 (loop-triage +
        // loop-verifier from .claude/agents) 14 + verifier 14 = 76
        let temp = copy_fixture("minimal");
        let a = audit(temp.path(), 0);
        assert_eq!(a.score, 76);
        assert_eq!(a.level, Some(Level::L2));
        assert_eq!(a.state_file.as_deref(), Some("STATE.md"));
        assert!(!a.state_stale, "a placeholder Last run is not stale");
        assert!(a.activity_evidence.is_empty());
        assert!(flag(&a, &["triage", "present"]));
        assert!(flag(&a, &["verifier", "present"]));
        assert_eq!(a.signals["skills"]["count"], 2);
        assert!(a.allows(Level::L1).is_ok());
        assert!(a.allows(Level::L2).is_ok());
        let why = a.allows(Level::L3).unwrap_err();
        assert!(
            why.contains("score 76") && why.contains("no fresh activity"),
            "{why}"
        );
        // a fresh state file adds activity: 90, still not L3 (no cost observability)
        set_last_run(temp.path(), "STATE.md", OffsetDateTime::now_utc());
        let fresh = audit(temp.path(), 0);
        assert_eq!(fresh.score, 90);
        assert_eq!(fresh.level, Some(Level::L2));
        assert_eq!(fresh.activity_evidence, vec!["state:STATE.md:fresh"]);
        assert!(
            fresh
                .findings
                .iter()
                .any(|f| f.message.contains("cost observability is incomplete")),
            "the L3 cap is explained"
        );
        assert!(fresh.assessment.contains("cost observability"));
    }

    #[test]
    fn the_full_fixture_reaches_100_and_l3_once_a_run_is_fresh() {
        // base 7 + state 18 + triage 14 + LOOP.md 9 + AGENTS.md 9 + skills 14
        // + verifier 14 + safety 4+4 + github 6+4 + mcp 3 + worktree 3
        // + registry 2 + budget 3 + run log 3 + LOOP.md budget 2 + budget skill 2
        // + tool scope 3 + stall 3 + escalation 3 + gate 3 + constraints 4+2
        // = 139 → clamped to 100 even before activity.
        let temp = copy_fixture("full");
        let a = audit(temp.path(), 0);
        assert_eq!(a.score, 100);
        assert_eq!(a.level, Some(Level::L2), "capped without activity");
        assert!(flag(&a, &["cost", "budgetDoc"]));
        assert!(flag(&a, &["cost", "loopMdBudget"]));
        assert!(flag(&a, &["governance", "toolScope"]));
        assert!(flag(&a, &["governance", "stallDetection"]));
        assert!(flag(&a, &["governance", "escalation"]));
        assert!(flag(&a, &["governance", "gateYaml"]));
        assert!(flag(&a, &["worktreeEvidence", "present"]));
        assert!(flag(&a, &["mcp", "present"]));
        assert!(
            a.findings
                .iter()
                .any(|f| f.message.contains("no proven loop activity"))
        );
        assert!(a.allows(Level::L3).is_err());

        // store evidence alone unlocks L3
        let with_store = audit(temp.path(), 2);
        assert_eq!(with_store.level, Some(Level::L3));
        assert_eq!(with_store.activity_evidence, vec!["store:2 runs"]);
        assert!(with_store.allows(Level::L3).is_ok());
        assert!(with_store.assessment.starts_with("Strong loop readiness"));

        // so does a fresh run-log line
        let log = temp.path().join("loop-run-log.md");
        let mut text = fs::read_to_string(&log).unwrap();
        text.push_str(&format!(
            "{{\"run_id\":\"{}\",\"pattern\":\"daily-triage\",\"outcome\":\"report-only\"}}\n",
            format_timestamp(OffsetDateTime::now_utc())
        ));
        fs::write(&log, text).unwrap();
        let with_log = audit(temp.path(), 0);
        assert_eq!(with_log.level, Some(Level::L3));
        assert_eq!(
            with_log.activity_evidence,
            vec!["log:loop-run-log.md:fresh"]
        );
        assert_eq!(with_log.score_bar(), format!("{}  100/100", "█".repeat(20)));
        let lines = with_log.human_lines();
        assert!(lines[1].contains("Score: 100/100  Level: L3"));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("  ✓ State file: STATE.md"))
        );
    }

    #[test]
    fn a_stale_state_file_warns_and_a_forged_future_one_is_ignored() {
        // base 7 + state 18 + triage 14 + LOOP.md 9 + one skill 7 = 55 → L1
        let temp = copy_fixture("stale-state");
        let a = audit(temp.path(), 0);
        assert_eq!(a.score, 55);
        assert_eq!(a.level, Some(Level::L1));
        assert!(a.state_stale);
        assert!(
            a.findings
                .iter()
                .any(|f| f.level == FindingLevel::Warn && f.message.contains("older than 14 days")),
            "{:?}",
            a.findings
        );
        assert!(a.allows(Level::L2).unwrap_err().contains("score 55"));

        set_last_run(
            temp.path(),
            "STATE.md",
            OffsetDateTime::now_utc() + time::Duration::hours(2),
        );
        let forged = audit(temp.path(), 0);
        assert!(
            forged.activity_evidence.is_empty(),
            "future timestamps are not activity"
        );
        assert!(!forged.state_stale, "nor are they stale");
        assert_eq!(forged.score, 55);
    }

    #[test]
    fn skills_are_found_in_every_harness_directory_and_scoped_tools_count() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".codex/skills/loop-ci-triage")).unwrap();
        fs::write(
            root.join(".codex/skills/loop-ci-triage/SKILL.md"),
            "---\nname: loop-ci-triage\nallowed-tools: Read, Grep\n---\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("skills/loop-rules")).unwrap();
        fs::write(root.join("skills/loop-rules/SKILL.md"), "# rules").unwrap();
        fs::create_dir_all(root.join(".codex/agents")).unwrap();
        fs::write(
            root.join(".codex/agents/verifier.toml"),
            "name = \"loop-verifier\"",
        )
        .unwrap();
        let (names, scoped) = find_skills(root);
        assert!(names.contains(&"loop-ci-triage".to_string()));
        assert!(names.contains(&"loop-rules".to_string()));
        assert!(names.contains(&"loop-verifier".to_string()));
        assert!(scoped);
        let a = audit(root, 0);
        // base 7 + triage 14 + skills≥2 14 + verifier 14 + constraints skill 2
        // + tool scope 3 = 54, no state file → L0
        assert_eq!(a.score, 54);
        assert_eq!(a.level, None);
        assert!(flag(&a, &["constraints", "hasConstraintsSkill"]));
    }

    #[test]
    fn hint_matching_is_case_insensitive_and_word_bounded() {
        assert!(contains_any_ci("A Circuit Breaker stops it", STALL_HINTS));
        assert!(contains_any_word_ci("the loop is STUCK.", STALL_WORDS));
        assert!(!contains_any_word_ci("installed", STALL_WORDS));
        assert!(contains_any_ci("Escalation: exit 2", ESCALATION_HINTS));
        assert_eq!(
            leading_timestamp("2026-09-15T08:00:39Z tail"),
            Some("2026-09-15T08:00:39Z")
        );
        assert_eq!(
            leading_timestamp("2026-09-15 08:00+02:00"),
            Some("2026-09-15 08:00+02:00")
        );
        assert_eq!(leading_timestamp("2026-09-15Tsoon"), Some("2026-09-15"));
        assert_eq!(leading_timestamp("soon"), None);
    }
}
