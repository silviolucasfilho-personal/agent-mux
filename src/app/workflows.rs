//! The App side of workflows: starting a run, spawning its sessions
//! through the one traced spawn path, capturing their stdout, settling
//! them after exit, feeding the interpreter, and recording every row.
//! The interpreter (`crate::workflows::interp`) decides what runs; this
//! module only executes and accounts.

use crate::app::{App, Notice};
use crate::config::Profile;
use crate::harness::{Harness, LaunchOptions, Resume, compose};
use crate::status::Status;
use crate::workflows::context::{self, Budget, Context, RunInfo, StepInfo};
use crate::workflows::document::{Actor, Isolation, Workflow};
use crate::workflows::harness as wfh;
use crate::workflows::interp::{Caps, RunState, RunStatus, SessionDone, SessionKey, SessionSpec};
use crate::workflows::result::{self, Outcome};
use crate::workflows::{journal, library, store as wstore};
use serde_json::Value;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use time::OffsetDateTime;

const SETTLE: Duration = Duration::from_millis(1200);
const PASS_EVERY: Duration = Duration::from_millis(500);
/// Raw stdout kept per session; envelopes are small, tool output is not.
const CAPTURE_CAP: usize = 4 * 1024 * 1024;

/// What starts a run.
#[derive(Debug, Clone)]
pub struct WorkflowRunRequest {
    pub name: String,
    pub source: String,
    pub document: String,
    pub workspace: PathBuf,
    pub profile: Option<String>,
    pub harness: Harness,
    pub args: Value,
    pub budget_tokens: Option<u64>,
    pub usd_cap: Option<f64>,
    pub isolation: Option<Isolation>,
    pub resume_from: Option<String>,
    /// One-off per-step overrides for this run, `(step id, key, value)` with
    /// `key` one of `harness`, `profile`, `model`, `effort`, `agent`. What a document
    /// states permanently, these state for one run.
    #[allow(clippy::type_complexity)]
    pub step_overrides: Vec<(String, String, String)>,
}

/// A session of a live run.
#[derive(Debug)]
pub struct LiveWorkflowSession {
    pub session_id: usize,
    pub key: SessionKey,
    pub spec: SessionSpec,
    pub harness: Harness,
    pub launch_id: Option<String>,
    pub started: Instant,
    pub started_ns: i64,
    pub timeout: Duration,
    pub exited_at: Option<Instant>,
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub out_file: PathBuf,
    pub context_path: PathBuf,
    pub retried: bool,
    /// The worktree an isolated session runs in.
    pub worktree: Option<crate::loops::worktree::Worktree>,
}

/// A run with sessions in flight or about to start.
pub struct LiveWorkflowRun {
    pub run_id: String,
    pub name: String,
    pub source: String,
    pub document: String,
    pub state: RunState,
    pub workspace: PathBuf,
    pub harness: Harness,
    pub profile: Profile,
    pub usd_cap: Option<f64>,
    pub started: Instant,
    pub started_at: OffsetDateTime,
    pub started_ns: i64,
    pub run_dir: PathBuf,
    pub sessions: Vec<LiveWorkflowSession>,
    pub session_count: usize,
    pub run_timeout: Duration,
    pub resumed_from: Option<String>,
    /// The agents the document names, as loaded when the run started.
    pub agents: std::collections::BTreeMap<String, crate::agents::AgentSpec>,
}

impl LiveWorkflowRun {
    pub fn running_sessions(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.exited_at.is_none())
            .count()
    }

    /// One line for the sidebar: sessions done / started, tokens.
    pub fn progress(&self) -> String {
        format!(
            "{}/{} sessions · {}k tokens",
            self.state.records.len(),
            self.state.sessions_started,
            self.state.tokens_spent / 1000
        )
    }
}

/// A finished run, kept in memory for the sidebar until restart (the
/// store has the durable row).
/// Finished runs the Workflows section lists above the library.
pub const RECENT_IN_SECTION: usize = 3;

/// One selectable row of the Workflows section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowRow {
    /// A live run (run id).
    Live(String),
    /// A planner's document awaiting a decision (plan id).
    Planned(String),
    /// A run finished since startup (run id).
    Recent(String),
    /// A library document (index into `workflow_list`).
    Doc(usize),
}

impl WorkflowRow {
    /// The heading the row sits under.
    pub fn group(&self) -> &'static str {
        match self {
            WorkflowRow::Live(_) => "Running",
            WorkflowRow::Planned(_) => "Plans to review",
            WorkflowRow::Recent(_) => "Recent",
            WorkflowRow::Doc(_) => "Library",
        }
    }
}

/// A drawn line of the Workflows section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionLine {
    Header(&'static str),
    /// Index into `App::workflow_rows`.
    Row(usize),
}

#[derive(Debug, Clone)]
pub struct RecentWorkflowRun {
    pub run_id: String,
    pub name: String,
    pub harness: String,
    pub status: String,
    pub sessions: usize,
    pub tokens: u64,
    pub cost_usd: f64,
    pub ended_at: OffsetDateTime,
    pub result: Value,
    pub error: Option<String>,
    pub notes: Vec<String>,
    /// A finished run whose verdict its document does not `accept`: the
    /// verdict (or "no verdict"), so the run waits in the inbox.
    pub awaiting: Option<String>,
}

impl RecentWorkflowRun {
    /// Waits on a human: it did not finish cleanly, or it finished with a
    /// verdict its document does not accept.
    pub fn needs_human(&self) -> bool {
        self.status != "finished" || self.error.is_some() || self.awaiting.is_some()
    }
}

/// The verdict a finished run waits on, when its document names the
/// verdicts it `accept`s and the answer's is not one of them.
fn awaiting_verdict(accept: &[String], status: &str, result: &Value) -> Option<String> {
    if accept.is_empty() || status != "finished" {
        return None;
    }
    let Some(v) = (match result {
        Value::String(text) => crate::workflows::report::verdict_line(text),
        _ => None,
    }) else {
        return Some("no verdict".into());
    };
    // an accepted word that its own header contradicts (blocking findings,
    // a human asked for) is not accepted
    let accepted = accept.iter().any(|a| a.eq_ignore_ascii_case(&v.value))
        && v.blocking.unwrap_or(0) == 0
        && !v.human_required;
    (!accepted).then_some(v.value)
}

fn hash_text(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

impl App {
    /// The runtime directory for workflow runs.
    pub fn workflows_runtime_dir(&self) -> PathBuf {
        self.loops_runtime_dir()
    }

    /// Every package the validator and the installer know.
    pub fn all_skill_infos(&self) -> Vec<crate::workflows::document::SkillInfo> {
        let mut all: Vec<crate::skill::SkillDefinition> = self.skills.clone();
        all.extend(self.hidden_skills.iter().cloned());
        library::skill_infos(&all)
    }

    /// The workflows the catalog knows: built-in, library, skill-distributed.
    pub fn workflow_entries(&self) -> Vec<library::Entry> {
        let mut all: Vec<crate::skill::SkillDefinition> = self.skills.clone();
        all.extend(self.hidden_skills.iter().cloned());
        library::load(&self.library_root(), &all)
    }

    /// The profile a run uses: the named one, else the first whose
    /// command is the harness, else the bare harness command.
    pub fn workflow_profile(&self, name: Option<&str>, harness: Harness) -> Profile {
        if let Some(n) = name
            && let Some(p) = self.profiles.iter().find(|p| p.name == n)
        {
            return p.clone();
        }
        if let Some(p) = self
            .profiles
            .iter()
            .find(|p| Harness::detect(&p.command) == Some(harness))
        {
            return p.clone();
        }
        Profile {
            name: harness.display_name().to_string(),
            command: harness.as_str().to_string(),
            args: Vec::new(),
            default_dir: None,
            tracing: None,
            model: None,
            bypass_approvals: None,
        }
    }

    /// Installs every step skill the document names into `harness`'s
    /// user-level skill directory.
    /// Installs every step skill for every harness the run can use: the
    /// run's own, plus each harness a step, a verify block or a judge names.
    /// A step that switches harness would otherwise invoke a skill that was
    /// never installed for it.
    fn install_workflow_skills(&self, doc: &Workflow, harness: Harness) -> Result<(), String> {
        let home = self.skill_home();
        let mut harnesses = vec![harness];
        for h in doc.harnesses_used() {
            if let Some(h) = Harness::detect(&h)
                && !harnesses.contains(&h)
            {
                harnesses.push(h);
            }
        }
        for id in doc.skills() {
            let def = self
                .find_skill(&id)
                .ok_or_else(|| format!("step skill {id:?} is not a known package"))?;
            for h in &harnesses {
                crate::skill::install::install(def, *h, &home, false)
                    .map_err(|e| format!("installing {id} for {}: {e}", h.as_str()))?;
            }
        }
        Ok(())
    }

    /// Fills defaults and checks required args.
    fn resolve_args(doc: &Workflow, given: &Value) -> Result<Value, String> {
        let mut out = serde_json::Map::new();
        let given_obj = match given {
            Value::Object(o) => o.clone(),
            Value::Null => serde_json::Map::new(),
            Value::String(s) => {
                // a bare string fills the single required arg
                let mut m = serde_json::Map::new();
                let required: Vec<&String> = doc
                    .args
                    .iter()
                    .filter(|(_, a)| a.required)
                    .map(|(k, _)| k)
                    .collect();
                match required.as_slice() {
                    [one] => {
                        m.insert((*one).clone(), Value::String(s.clone()));
                    }
                    _ if doc.args.len() == 1 => {
                        m.insert(
                            doc.args.keys().next().unwrap().clone(),
                            Value::String(s.clone()),
                        );
                    }
                    _ => return Err("args must be a JSON object for this workflow".into()),
                }
                m
            }
            _ => return Err("args must be a JSON object".into()),
        };
        for (k, spec) in &doc.args {
            match given_obj.get(k) {
                Some(v) if !v.is_null() => {
                    out.insert(k.clone(), v.clone());
                }
                _ => match &spec.default {
                    Some(d) => {
                        out.insert(k.clone(), d.clone());
                    }
                    None if spec.required => return Err(format!("arg {k:?} is required")),
                    None => {}
                },
            }
        }
        for (k, v) in given_obj {
            if !doc.args.contains_key(&k) {
                return Err(format!("unknown arg {k:?}"));
            }
            out.entry(k).or_insert(v);
        }
        Ok(Value::Object(out))
    }

    /// Starts a run: validates, installs skills, writes the run
    /// directory and the store row, and launches the first sessions.
    pub fn start_workflow_run(&mut self, req: WorkflowRunRequest) -> Result<String, String> {
        if !self.workflows.enabled {
            return Err("workflows are disabled ([workflows] enabled = false)".into());
        }
        let mut doc = crate::workflows::parse(&req.document).map_err(|p| p.join("; "))?;
        let problems = crate::workflows::validate(&doc, Some(&self.all_skill_infos()));
        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        if !doc.harness.allows(req.harness.as_str()) {
            return Err(format!(
                "{} does not allow {}",
                doc.name,
                req.harness.display_name()
            ));
        }
        if !req.workspace.is_dir() {
            return Err(format!(
                "workspace {} does not exist",
                req.workspace.display()
            ));
        }
        if let Some(iso) = req.isolation {
            doc.default_isolation = Some(doc.default_isolation.unwrap_or(iso));
        }
        // One-off overrides: `--step find.model=…` for this run only.
        for (id, key, value) in &req.step_overrides {
            let step = doc
                .steps
                .iter_mut()
                .find(|s| s.id == *id)
                .ok_or_else(|| format!("--step {id}: no step with that id"))?;
            let v = Some(value.clone()).filter(|v| !v.trim().is_empty());
            match key.as_str() {
                "harness" => {
                    if let Some(h) = &v {
                        let Some(to) = Harness::detect(h) else {
                            return Err(format!("--step {id}.harness: unknown harness {h:?}"));
                        };
                        if !doc.harness.allows(to.as_str()) {
                            return Err(format!(
                                "step {id}: harness {h:?} is not allowed by workflow.harness"
                            ));
                        }
                        // a profile the document named for another CLI
                        // would launch that CLI; the new harness picks its own
                        if let Some(p) = &step.profile
                            && self
                                .profiles
                                .iter()
                                .find(|q| q.name == *p)
                                .and_then(|q| Harness::detect(&q.command))
                                .is_some_and(|ph| ph != to)
                        {
                            step.profile = None;
                        }
                    }
                    step.harness = v;
                }
                "profile" => step.profile = v,
                "model" => step.model = v,
                "effort" => step.effort = v,
                "agent" => step.agent = v,
                other => {
                    return Err(format!(
                        "--step {id}.{other}: expected harness, profile, model, effort or agent"
                    ));
                }
            }
        }
        let args = Self::resolve_args(&doc, &req.args)?;
        // Agents: each one the document names must load and fit its steps;
        // warnings (what a harness cannot enforce) become run notes.
        let catalog = crate::agents::Catalog::load(&self.library_root(), Some(&req.workspace));
        let diagnostics = crate::agents::fit::check(&doc, &catalog, &self.all_skill_infos());
        let errors = crate::agents::fit::errors(&diagnostics);
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        let agents: std::collections::BTreeMap<String, crate::agents::AgentSpec> = doc
            .agents()
            .into_iter()
            .filter_map(|a| catalog.get(&a).cloned().map(|s| (a, s)))
            .collect();
        self.install_workflow_skills(&doc, req.harness)?;

        let run_id = uuid::Uuid::new_v4().to_string();
        let runtime = self.workflows_runtime_dir();
        let run_dir = context::run_dir(&runtime, &run_id);
        std::fs::create_dir_all(&run_dir).map_err(|e| format!("{}: {e}", run_dir.display()))?;
        let _ = std::fs::write(run_dir.join("workflow.toml"), &req.document);
        let _ = std::fs::write(
            run_dir.join("args.json"),
            serde_json::to_string_pretty(&args).unwrap_or_default(),
        );
        let caps = Caps {
            max_sessions: self.workflows.max_sessions,
            budget_tokens: req.budget_tokens.or(doc.budget_tokens).or((self
                .workflows
                .default_budget_tokens
                > 0)
            .then_some(self.workflows.default_budget_tokens)),
        };
        let mut state = RunState::new(doc, args.clone(), caps);
        let mut resume_note: Option<String> = None;
        if let Some(prev) = &req.resume_from {
            let prev_journal = context::run_dir(&runtime, prev).join("journal.jsonl");
            let entries = journal::load(&prev_journal);
            if entries.is_empty() {
                return Err(format!("run {prev} has no journal to resume from"));
            }
            let (entries, stale) = journal::replayable(entries, &state.doc, |a| {
                agents.get(a).map(|s| s.hash.clone())
            });
            if let Some(step) = stale {
                resume_note = Some(format!(
                    "an agent changed since run {prev}: step {step} and the steps after it run again"
                ));
            }
            for e in &entries {
                let _ = journal::append(&run_dir.join("journal.jsonl"), e);
            }
            state = state.with_replay(journal::to_replay(&entries));
        }
        state.notes.extend(resume_note);
        for d in &diagnostics {
            state.notes.push(d.message.clone());
        }
        let profile = self.workflow_profile(req.profile.as_deref(), req.harness);
        let started_at = crate::loops::now();
        let started_ns = crate::tracing::store::now_ns();
        let live = LiveWorkflowRun {
            run_id: run_id.clone(),
            name: req.name.clone(),
            source: req.source.clone(),
            document: req.document.clone(),
            state,
            workspace: req.workspace.clone(),
            harness: req.harness,
            profile: profile.clone(),
            usd_cap: req.usd_cap,
            started: Instant::now(),
            started_at,
            started_ns,
            run_dir,
            sessions: Vec::new(),
            session_count: 0,
            run_timeout: Duration::from_secs(self.workflows.run_timeout_s),
            resumed_from: req.resume_from.clone(),
            agents,
        };
        self.write_workflow_run_row(&live, "running", None);
        self.live_workflow_runs.push(live);
        self.pump_workflows();
        Ok(run_id)
    }

    fn write_workflow_run_row(&self, live: &LiveWorkflowRun, status: &str, ended_ns: Option<i64>) {
        let Some(db) = self.trace_db_path.as_deref() else {
            return;
        };
        let Ok(conn) = crate::tracing::store::open_aux(db) else {
            return;
        };
        let (result, error) = match live.state.status() {
            RunStatus::Finished(v) | RunStatus::BudgetExhausted(v) => (v, None),
            RunStatus::Failed(e) => (Value::Null, Some(e)),
            _ => (Value::Null, None),
        };
        let row = wstore::WorkflowRun {
            id: live.run_id.clone(),
            workflow: live.name.clone(),
            source: live.source.clone(),
            document_hash: hash_text(&live.document),
            document: live.document.clone(),
            workspace: live.workspace.to_string_lossy().into_owned(),
            harness: live.harness.as_str().to_string(),
            profile: live.profile.name.clone(),
            args: live.state.args.clone(),
            budget_tokens: live.state.doc.budget_tokens.map(|b| b as i64),
            started_ns: live.started_ns,
            ended_ns,
            status: status.to_string(),
            sessions: live.state.records.len() as i64,
            tokens: Some(live.state.tokens_spent as i64),
            cost_usd: Some(live.state.cost_spent),
            result,
            error,
            resumed_from: live.resumed_from.clone(),
        };
        let _ = wstore::upsert_run(&conn, &row);
    }

    // ---- the tick -----------------------------------------------------------

    /// Timeouts, settled sessions, new sessions, finished runs.
    pub fn workflow_pass(&mut self, now: Instant) {
        if self.live_workflow_runs.is_empty() {
            return;
        }
        if self
            .last_workflow_pass
            .is_some_and(|t| now.saturating_duration_since(t) < PASS_EVERY)
        {
            return;
        }
        self.last_workflow_pass = Some(now);
        // session and run timeouts
        let mut to_kill: Vec<usize> = Vec::new();
        for run in &mut self.live_workflow_runs {
            let run_over = now.saturating_duration_since(run.started) > run.run_timeout;
            if run_over && !matches!(run.state.status(), RunStatus::Running) {
                continue;
            }
            if run_over {
                run.state.cancel();
                run.state.notes.push("run timeout".into());
            }
            for s in &mut run.sessions {
                if s.exited_at.is_none()
                    && (run_over || now.saturating_duration_since(s.started) > s.timeout)
                {
                    s.timed_out = true;
                    s.exited_at = Some(now);
                    to_kill.push(s.session_id);
                }
            }
        }
        for sid in to_kill {
            if let Some(i) = self.sessions.iter().position(|s| s.id == sid) {
                self.sessions[i].kill();
            }
        }
        // settled sessions
        let mut ready: Vec<(usize, usize)> = Vec::new();
        for (ri, run) in self.live_workflow_runs.iter().enumerate() {
            for (si, s) in run.sessions.iter().enumerate() {
                if s.exited_at
                    .is_some_and(|t| now.saturating_duration_since(t) >= SETTLE)
                {
                    ready.push((ri, si));
                }
            }
        }
        // highest indexes first so removals keep earlier indexes valid
        ready.sort_by(|a, b| b.cmp(a));
        for (ri, si) in ready {
            self.complete_workflow_session(ri, si);
        }
        self.pump_workflows();
        // finished runs
        let done: Vec<usize> = self
            .live_workflow_runs
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                !matches!(r.state.status(), RunStatus::Running) && r.sessions.is_empty()
            })
            .map(|(i, _)| i)
            .collect();
        for i in done.into_iter().rev() {
            self.finish_workflow_run(i);
        }
    }

    fn workflow_slots(&self) -> usize {
        let running: usize = self
            .live_workflow_runs
            .iter()
            .map(LiveWorkflowRun::running_sessions)
            .sum();
        let loops = self
            .live_loop_runs
            .iter()
            .filter(|r| r.exited_at.is_none())
            .count();
        (self.workflows.max_concurrent as usize).saturating_sub(running + loops)
    }

    /// Launches the sessions the interpreter allows, across runs.
    fn pump_workflows(&mut self) {
        for ri in 0..self.live_workflow_runs.len() {
            let mut slots = self.workflow_slots();
            let per_run = self.live_workflow_runs[ri]
                .state
                .doc
                .max_concurrent
                .unwrap_or(usize::MAX);
            let in_run = self.live_workflow_runs[ri].running_sessions();
            slots = slots.min(per_run.saturating_sub(in_run));
            if slots == 0 {
                continue;
            }
            if self.loop_registry.pause_all {
                continue;
            }
            let specs = self.live_workflow_runs[ri].state.next(slots);
            for spec in specs {
                if let Err(e) = self.spawn_workflow_session(ri, spec.clone(), None) {
                    let run = &mut self.live_workflow_runs[ri];
                    run.state.notes.push(format!("{}: {e}", spec.label()));
                    run.state.complete(
                        &spec.key,
                        SessionDone {
                            outcome: Outcome::Null(e),
                            tokens: 0,
                            cost_usd: 0.0,
                        },
                    );
                }
            }
        }
    }

    /// One session: context file, prompt, command line, spawn.
    fn spawn_workflow_session(
        &mut self,
        ri: usize,
        spec: SessionSpec,
        retry_hint: Option<String>,
    ) -> Result<(), String> {
        let run = &mut self.live_workflow_runs[ri];
        let harness = spec
            .overrides
            .harness
            .as_deref()
            .and_then(Harness::detect)
            .unwrap_or(run.harness);
        let mut profile = match &spec.overrides.profile {
            Some(name) => self.workflow_profile(Some(name), harness),
            None if harness == run.harness => run.profile.clone(),
            None => self.workflow_profile(None, harness),
        };
        let run = &mut self.live_workflow_runs[ri];
        if harness == Harness::Antigravity && spec.overrides.isolation == Some(Isolation::Worktree)
        {
            run.state.notes.push(format!(
                "{}: worktree isolation is not available on Antigravity",
                spec.label()
            ));
        }
        run.session_count += 1;
        let n = run.session_count;
        let label = spec.label();
        // isolation: a worktree per session, on a git workspace, never on
        // Antigravity (no path guard, no per-launch registration)
        let mut worktree: Option<crate::loops::worktree::Worktree> = None;
        if spec.overrides.isolation == Some(Isolation::Worktree)
            && harness != Harness::Antigravity
            && crate::loops::worktree::is_git_repo(&run.workspace)
        {
            let slug: String = label
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            let wt_id = format!("wf-{}-{slug}", &run.run_id[..8]);
            let worktrees_dir = self.loops.worktrees_dir.clone();
            let run = &self.live_workflow_runs[ri];
            match crate::loops::worktree::create(
                &run.workspace,
                &worktrees_dir,
                &wt_id,
                &run.name,
                &crate::loops::format_timestamp(crate::loops::now()),
            ) {
                Ok(wt) => worktree = Some(wt),
                Err(e) => return Err(format!("worktree: {e}")),
            }
        }
        let run = &mut self.live_workflow_runs[ri];
        let base_dir = worktree
            .as_ref()
            .map(|w| w.path.clone())
            .unwrap_or_else(|| run.workspace.clone());
        let cwd = match &spec.overrides.cwd {
            Some(sub) => base_dir.join(sub),
            None => base_dir,
        };
        let doc = &run.state.doc;
        let step = doc.step(&spec.key.step).cloned();
        let result_schema = spec.result_schema.as_ref().and_then(|name| {
            doc.schemas
                .get(name)
                .map(|s| s.to_json_schema(&doc.schemas))
        });
        let of = match &step {
            Some(s) if s.kind.per_item() => match &s.over {
                Some(crate::workflows::document::Over::Inline(v)) => Some(v.len()),
                _ => None,
            },
            _ => None,
        };
        let ctx = Context {
            schema_version: context::SCHEMA_VERSION,
            run: RunInfo {
                id: run.run_id.clone(),
                workflow: run.name.clone(),
                harness: harness.as_str().to_string(),
                workspace: run.workspace.to_string_lossy().into_owned(),
                started_at: crate::loops::format_timestamp(run.started_at),
            },
            step: StepInfo {
                id: spec.key.step.clone(),
                kind: step
                    .as_ref()
                    .map(|s| s.kind.label().to_string())
                    .unwrap_or_default(),
                phase: spec.phase.clone(),
                label: label.clone(),
                role: match &spec.key.role {
                    crate::workflows::interp::Role::Main => "main".into(),
                    crate::workflows::interp::Role::Vote(_) => "vote".into(),
                    crate::workflows::interp::Role::Generate(_) => "generate".into(),
                    crate::workflows::interp::Role::Judge { .. } => "judge".into(),
                },
                index: spec.index,
                of,
            },
            args: spec.args.clone(),
            item: spec.item.clone(),
            inputs: spec.inputs.clone(),
            result_schema,
            budget: Budget {
                tokens_total: run.state.doc.budget_tokens,
                tokens_spent: run.state.tokens_spent,
                mode: "normal".into(),
            },
            worktree: worktree
                .as_ref()
                .map(|w| w.path.to_string_lossy().into_owned()),
            gate: crate::loops::gate::load(&run.workspace.join(crate::loops::GATE_YAML))
                .ok()
                .map(|g| serde_json::json!({ "denylist": g.denylist, "max_files": g.max_files })),
            previous_seen: spec.previous_seen.clone(),
        };
        let context_path =
            context::write(&run.run_dir, n, &ctx).map_err(|e| format!("context file: {e}"))?;
        let out_file = run.run_dir.join("sessions").join(format!("{n}.last.md"));
        let _ = std::fs::create_dir_all(out_file.parent().unwrap());

        // the prompt
        let prompts = crate::prompts::Prompts::current();
        let mut prompt = match &spec.actor {
            Actor::Skill(id) => crate::prompts::render_workflow(
                &prompts.workflow_run,
                &crate::skill::render::invocation(id, harness),
                &run.name,
                &label,
                &context_path.display().to_string(),
                "",
            ),
            Actor::Prompt(text) => {
                let pre = crate::prompts::render_workflow(
                    &prompts.workflow_inline,
                    "",
                    &run.name,
                    &label,
                    &context_path.display().to_string(),
                    "",
                );
                format!("{pre}\n\n{text}")
            }
        };
        if let Some(hint) = retry_hint {
            prompt.push_str("\n\n");
            prompt.push_str(&hint);
        }
        // The agent the session runs as: its launch plan on this harness.
        // A step's own model and effort win over the agent's.
        let agent = spec
            .overrides
            .agent
            .as_ref()
            .and_then(|a| run.agents.get(a))
            .cloned();
        let agent_plan = agent
            .as_ref()
            .map(|a| crate::agents::launch::plan(a, harness))
            .unwrap_or_default();
        if harness == Harness::Antigravity
            && let Some(a) = &agent
        {
            crate::agents::launch::install_agy(a, &self.skill_home())
                .map_err(|e| format!("agent {}: {e}", a.name))?;
        }
        let run = &mut self.live_workflow_runs[ri];
        let options = LaunchOptions {
            model: spec
                .overrides
                .model
                .clone()
                .or_else(|| agent_plan.model.clone())
                .or_else(|| profile.model.clone()),
            bypass_approvals: true,
            resume: Resume::Off,
            one_shot: Some(prompt),
        };
        let mut args = compose(&profile.args, &options.render(harness));
        args.retain(|a| !agent_plan.remove.contains(a));
        let timeout_s = spec
            .overrides
            .timeout_s
            .unwrap_or(self.workflows.session_timeout_s);
        let mut extra = wfh::extra_args(harness, run.usd_cap, timeout_s, &out_file);
        extra.extend(agent_plan.args.iter().cloned());
        // Reasoning effort, where the harness has one to set.
        if let Some(effort) = spec
            .overrides
            .effort
            .as_deref()
            .or(agent_plan.effort.as_deref())
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            match wfh::effort_args(harness, effort) {
                Some(mut a) => extra.append(&mut a),
                None => run.state.notes.push(format!(
                    "{}: {} takes no reasoning effort {effort:?}; ignored",
                    spec.label(),
                    harness.as_str()
                )),
            }
        }
        wfh::insert_before_prompt(&mut args, harness, extra);
        if Harness::detect(&profile.command) != Some(harness) {
            profile.command = harness.as_str().to_string();
        }
        profile.args = args;
        profile.name = format!("{} ▸ {label}", run.name);
        if let Some(cap) = run.usd_cap {
            let t = profile.tracing.get_or_insert_with(Default::default);
            t.max_cost_usd = Some(cap);
        }
        let run_id = run.run_id.clone();
        let workspace = run.workspace.clone();
        let skill_id = spec.actor.skill().map(str::to_string);
        let phase = spec.phase.clone();

        // MCP registration and environment
        let mut extra_args: Vec<String> = Vec::new();
        let registration = self.ensure_mcp(harness.as_str(), &workspace);
        if let crate::mcp::register::Registration::PerLaunch { args } = &registration {
            extra_args.extend(args.iter().cloned());
        }
        let env: Vec<(String, String)> = vec![
            (
                "AGENT_MUX_WORKFLOW_CONTEXT".into(),
                context_path.to_string_lossy().into_owned(),
            ),
            ("AGENT_MUX_WORKFLOW_RUN_ID".into(), run_id.clone()),
            (
                "AGENT_MUX_WORKFLOW".into(),
                self.live_workflow_runs[ri].name.clone(),
            ),
            ("AGENT_MUX_WORKFLOW_STEP".into(), label.clone()),
            ("AGENT_MUX_MCP".into(), registration.env_value().to_string()),
            (
                "AGENT_MUX_WORKSPACE".into(),
                workspace.to_string_lossy().into_owned(),
            ),
        ];
        let launch = crate::workflows::WorkflowLaunch {
            run_id: run_id.clone(),
            workflow: self.live_workflow_runs[ri].name.clone(),
            step: label.clone(),
            phase: phase.clone(),
            agent: agent.as_ref().map(|a| a.name.clone()),
        };
        let id = self.next_id;
        let session = self
            .spawn_traced_full(
                id,
                profile,
                cwd,
                &env,
                skill_id.as_deref(),
                None,
                Some(launch),
                &extra_args,
            )
            .map_err(|e| format!("spawn: {e}"))?;
        self.next_id += 1;
        let launch_id = session.trace.as_ref().map(|t| t.launch_id.clone());
        self.sessions.push(session);
        let started_ns = crate::tracing::store::now_ns();
        self.live_workflow_runs[ri]
            .sessions
            .push(LiveWorkflowSession {
                session_id: id,
                key: spec.key.clone(),
                spec: spec.clone(),
                harness,
                launch_id: launch_id.clone(),
                started: Instant::now(),
                started_ns,
                timeout: Duration::from_secs(timeout_s),
                exited_at: None,
                timed_out: false,
                stdout: Vec::new(),
                out_file,
                context_path,
                retried: false,
                worktree,
            });
        let si = self.live_workflow_runs[ri].sessions.len() - 1;
        self.write_workflow_step_row(ri, si, None);
        Ok(())
    }

    fn write_workflow_step_row(&self, ri: usize, si: usize, outcome: Option<(&Outcome, u64, f64)>) {
        let Some(db) = self.trace_db_path.as_deref() else {
            return;
        };
        let Ok(conn) = crate::tracing::store::open_aux(db) else {
            return;
        };
        let run = &self.live_workflow_runs[ri];
        let s = &run.sessions[si];
        let row = wstore::WorkflowStep {
            run_id: run.run_id.clone(),
            session: s.key.label(),
            step_id: s.key.step.clone(),
            item: s.spec.item.clone(),
            launch_id: s.launch_id.clone(),
            phase: s.spec.phase.clone(),
            harness: s.harness.as_str().to_string(),
            kind: outcome
                .map(|(o, _, _)| o.kind())
                .unwrap_or("running")
                .to_string(),
            started_ns: Some(s.started_ns),
            ended_ns: outcome.map(|_| crate::tracing::store::now_ns()),
            tokens: outcome.map(|(_, t, _)| t as i64),
            cost_usd: outcome.map(|(_, _, c)| c),
            worktree: s
                .worktree
                .as_ref()
                .map(|w| w.path.to_string_lossy().into_owned()),
            changed_files: s
                .worktree
                .as_ref()
                .filter(|_| outcome.is_some())
                .map(|w| serde_json::json!(crate::loops::worktree::changed_files(w)))
                .unwrap_or(Value::Null),
            result: outcome.map(|(o, _, _)| o.value()).unwrap_or(Value::Null),
        };
        let _ = wstore::upsert_step(&conn, &row);
    }

    /// Appends the child's raw stdout for a workflow session.
    pub fn capture_workflow_output(&mut self, session_id: usize, bytes: &[u8]) {
        for run in &mut self.live_workflow_runs {
            if let Some(s) = run.sessions.iter_mut().find(|s| s.session_id == session_id) {
                if s.stdout.len() < CAPTURE_CAP {
                    let room = CAPTURE_CAP - s.stdout.len();
                    s.stdout.extend_from_slice(&bytes[..bytes.len().min(room)]);
                }
                return;
            }
        }
    }

    /// Called from `handle_pty_exit`: the accounting waits for the writer.
    pub fn finish_workflow_session_for_session(&mut self, session_id: usize) {
        for run in &mut self.live_workflow_runs {
            if let Some(s) = run
                .sessions
                .iter_mut()
                .find(|s| s.session_id == session_id && s.exited_at.is_none())
            {
                s.exited_at = Some(Instant::now());
                return;
            }
        }
    }

    /// Reads a settled session's result and hands it to the interpreter;
    /// a structured answer that fails its schema is retried once.
    fn complete_workflow_session(&mut self, ri: usize, si: usize) {
        let (key, spec, harness, launch_id, stdout, out_file, retried, timed_out, session_id) = {
            let s = &self.live_workflow_runs[ri].sessions[si];
            (
                s.key.clone(),
                s.spec.clone(),
                s.harness,
                s.launch_id.clone(),
                s.stdout.clone(),
                s.out_file.clone(),
                s.retried,
                s.timed_out,
                s.session_id,
            )
        };
        let conn_ro = self
            .trace_db_path
            .as_deref()
            .and_then(|p| crate::tracing::store::open_ro(p).ok());
        let facts = match (&conn_ro, &launch_id) {
            (Some(c), Some(l)) => crate::loops::store::run_facts(c, l).ok(),
            _ => None,
        };
        let exit_code = self
            .sessions
            .iter()
            .find(|s| s.id == session_id)
            .map(|s| s.status(Instant::now()))
            .and_then(|st| match st {
                Status::Exited(code) => code,
                _ => None,
            });
        let screen = self
            .sessions
            .iter_mut()
            .find(|s| s.id == session_id)
            .map(|s| s.text_dump())
            .unwrap_or_default();
        let env = wfh::envelope(harness, &stdout, &out_file);
        let final_text: Option<String> = env
            .text
            .clone()
            .or_else(|| facts.as_ref().and_then(|f| f.final_message.clone()))
            .or_else(|| {
                let t = screen.trim().to_string();
                (!t.is_empty()).then_some(t)
            });
        let doc = &self.live_workflow_runs[ri].state.doc;
        let schema = spec
            .result_schema
            .as_ref()
            .and_then(|n| doc.schemas.get(n).map(|s| (n.as_str(), s)));
        let interpreted = if timed_out {
            Err("session timed out".to_string())
        } else {
            result::interpret(final_text.as_deref(), schema, &doc.schemas)
        };
        let tokens = facts
            .as_ref()
            .and_then(|f| f.tokens)
            .map(|t| t.max(0) as u64)
            .or(env.tokens)
            .unwrap_or(0);
        let cost = facts
            .as_ref()
            .and_then(|f| f.cost_usd)
            .or(env.cost_usd)
            .unwrap_or(0.0);
        let outcome = match interpreted {
            Ok(o) => o,
            Err(e) if schema.is_some() && !retried && !timed_out => {
                // retry once with the validation errors
                self.live_workflow_runs[ri].state.tokens_spent += tokens;
                self.live_workflow_runs[ri].state.cost_spent += cost;
                self.live_workflow_runs[ri].sessions.remove(si);
                let hint = format!(
                    "Your previous answer did not match the expected result: {e}. Answer again, ending with exactly one fenced workflow-result block that matches result_schema in the context file, and nothing after it."
                );
                match self.spawn_workflow_session(ri, spec.clone(), Some(hint)) {
                    Ok(()) => {
                        let run = &mut self.live_workflow_runs[ri];
                        if let Some(s) = run.sessions.last_mut() {
                            s.retried = true;
                        }
                        run.state.notes.push(format!(
                            "{}: retrying after a schema mismatch",
                            spec.label()
                        ));
                        return;
                    }
                    Err(spawn_err) => Outcome::Null(format!("{e}; retry failed: {spawn_err}")),
                }
            }
            Err(e) => {
                let mut why = e;
                if let Some(code) = exit_code
                    && code != 0
                {
                    why = format!("{why} (exit code {code})");
                }
                Outcome::Null(why)
            }
        };
        let done = SessionDone {
            outcome: outcome.clone(),
            tokens,
            cost_usd: cost,
        };
        // the worktree: removed when nothing changed, kept (and noted) otherwise
        let wt_note = {
            let run = &self.live_workflow_runs[ri];
            let s = &run.sessions[si];
            match &s.worktree {
                Some(wt) if crate::loops::worktree::has_changes(wt) => Some(format!(
                    "{}: kept worktree {} on branch {} ({})",
                    key.label(),
                    wt.path.display(),
                    wt.branch,
                    crate::loops::worktree::diff_stat(wt)
                        .lines()
                        .last()
                        .unwrap_or("changes")
                        .trim()
                )),
                Some(wt) => {
                    let _ = crate::loops::worktree::remove(
                        &run.workspace,
                        &self.loops.worktrees_dir,
                        wt,
                        true,
                        Some("clean"),
                        &run.run_id,
                    );
                    None
                }
                None => None,
            }
        };
        // journal, store, interpreter
        let run = &self.live_workflow_runs[ri];
        let agent = spec
            .overrides
            .agent
            .as_ref()
            .and_then(|a| run.agents.get(a))
            .map(|a| (a.name.clone(), a.hash.clone()));
        let entry = journal::Entry::from_outcome(
            &key.label(),
            &key.step,
            &spec.phase,
            &outcome,
            launch_id.clone(),
            tokens,
            cost,
        )
        .with_agent(agent);
        let _ = journal::append(&run.run_dir.join("journal.jsonl"), &entry);
        self.write_workflow_step_row(ri, si, Some((&outcome, tokens, cost)));
        let run = &mut self.live_workflow_runs[ri];
        run.sessions.remove(si);
        run.state.complete(&key, done);
        if let Outcome::Null(why) = &outcome {
            run.state
                .notes
                .push(format!("{}: no answer ({why})", key.label()));
        }
        if let Some(n) = wt_note {
            run.state.notes.push(n);
        }
    }

    /// Records a finished run and moves it to the recent list.
    fn finish_workflow_run(&mut self, ri: usize) {
        let status = self.live_workflow_runs[ri].state.status();
        let status_label = match &status {
            RunStatus::Finished(_) => "finished",
            RunStatus::Failed(_) => "failed",
            RunStatus::Cancelled => "cancelled",
            RunStatus::BudgetExhausted(_) => "budget-exhausted",
            RunStatus::Running => "running",
        };
        let ended_ns = crate::tracing::store::now_ns();
        let live = &self.live_workflow_runs[ri];
        self.write_workflow_run_row(live, status_label, Some(ended_ns));
        let (result, error) = match &status {
            RunStatus::Finished(v) | RunStatus::BudgetExhausted(v) => (v.clone(), None),
            RunStatus::Failed(e) => (Value::Null, Some(e.clone())),
            _ => (Value::Null, None),
        };
        let _ = std::fs::write(
            live.run_dir.join("result.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "run_id": live.run_id,
                "workflow": live.name,
                "status": status_label,
                "result": result,
                "error": error,
                "sessions": live.state.records.len(),
                "tokens": live.state.tokens_spent,
                "cost_usd": live.state.cost_spent,
                "notes": live.state.notes,
            }))
            .unwrap_or_default(),
        );
        let live = self.live_workflow_runs.remove(ri);
        let awaiting = awaiting_verdict(&live.state.doc.accept, status_label, &result);
        let recent = RecentWorkflowRun {
            run_id: live.run_id.clone(),
            name: live.name.clone(),
            harness: live.harness.as_str().to_string(),
            status: status_label.to_string(),
            sessions: live.state.records.len(),
            tokens: live.state.tokens_spent,
            cost_usd: live.state.cost_spent,
            ended_at: crate::loops::now(),
            result,
            error: error.clone(),
            notes: live.state.notes.clone(),
            awaiting,
        };
        // The notice leads with what the run answered, not with how many
        // sessions it took.
        let answered = live
            .state
            .records
            .iter()
            .filter(|r| r.outcome.kind() != "null")
            .count();
        let verdict = crate::workflows::report::build(crate::workflows::report::RunView {
            workflow: &live.name,
            status: status_label,
            harness: live.harness.as_str(),
            workspace: "",
            sessions: live.state.records.len() as i64,
            tokens: live.state.tokens_spent,
            cost_usd: Some(live.state.cost_spent),
            duration_s: None,
            result: &recent.result,
            error: error.as_deref(),
            notes: &live.state.notes,
            doc: Some(&live.state.doc),
            steps: &[],
        })
        .headline
        .verdict;
        self.notice = Some(match status_label {
            "finished" => Notice::info(format!(
                "{} finished · {verdict} · {answered}/{} answered · {} tokens · W to read",
                live.name,
                live.state.records.len(),
                crate::loops::format_tokens(live.state.tokens_spent)
            )),
            "budget-exhausted" => Notice::warn(format!(
                "workflow {} stopped at the token budget after {} sessions",
                live.name,
                live.state.records.len()
            )),
            "cancelled" => Notice::info(format!("workflow {} cancelled", live.name)),
            _ => Notice::error(format!(
                "workflow {} failed: {}",
                live.name,
                error.unwrap_or_default()
            )),
        });
        self.recent_workflow_runs.insert(0, recent);
        self.recent_workflow_runs.truncate(50);
    }

    /// Cancels a live run: the interpreter stops issuing, running
    /// sessions are killed and settle as null answers.
    pub fn cancel_workflow_run(&mut self, run_id: &str) -> bool {
        let Some(ri) = self
            .live_workflow_runs
            .iter()
            .position(|r| r.run_id == run_id || r.run_id.starts_with(run_id))
        else {
            return false;
        };
        self.live_workflow_runs[ri].state.cancel();
        let ids: Vec<usize> = self.live_workflow_runs[ri]
            .sessions
            .iter()
            .filter(|s| s.exited_at.is_none())
            .map(|s| s.session_id)
            .collect();
        for sid in ids {
            if let Some(i) = self.sessions.iter().position(|s| s.id == sid) {
                self.sessions[i].kill();
            }
        }
        true
    }

    /// The live run a session belongs to, for the sidebar.
    pub fn workflow_run_of_session(&self, session_id: usize) -> Option<&LiveWorkflowRun> {
        self.live_workflow_runs
            .iter()
            .find(|r| r.sessions.iter().any(|s| s.session_id == session_id))
    }
}

// ---- the planner ---------------------------------------------------------------

/// A planner session in flight.
#[derive(Debug)]
pub struct LivePlan {
    pub id: String,
    pub session_id: usize,
    pub task: String,
    pub workspace: PathBuf,
    pub harness: Harness,
    pub profile: Option<String>,
    pub budget_tokens: Option<u64>,
    pub started: Instant,
    pub timeout: Duration,
    pub exited_at: Option<Instant>,
    pub stdout: Vec<u8>,
    pub out_file: PathBuf,
    pub launch_id: Option<String>,
    /// Start the run as soon as the document validates.
    pub auto_run: bool,
}

/// A document the planner produced, waiting on the user (or already run).
#[derive(Debug, Clone)]
pub struct PlannedWorkflow {
    pub id: String,
    pub task: String,
    pub workspace: PathBuf,
    pub harness: Harness,
    pub profile: Option<String>,
    pub budget_tokens: Option<u64>,
    pub name: String,
    pub document: String,
    pub problems: Vec<String>,
    /// The planner's answer when no document could be extracted.
    pub raw: Option<String>,
    pub run_id: Option<String>,
    /// Agents the planner defined for the document (`agent-toml` blocks),
    /// written into the library when the plan runs or is saved.
    pub agents: Vec<String>,
}

impl PlannedWorkflow {
    pub fn valid(&self) -> bool {
        self.problems.is_empty() && !self.document.is_empty()
    }
}

/// What starts a planner session.
#[derive(Debug, Clone)]
pub struct PlanRequest {
    pub task: String,
    pub workspace: PathBuf,
    pub harness: Harness,
    pub profile: Option<String>,
    pub budget_tokens: Option<u64>,
    pub auto_run: bool,
}

impl App {
    /// Launches the `workflow-author` skill for a task.
    pub fn start_workflow_plan(&mut self, req: PlanRequest) -> Result<String, String> {
        if !self.workflows.enabled {
            return Err("workflows are disabled ([workflows] enabled = false)".into());
        }
        if req.task.trim().is_empty() {
            return Err("the task is empty".into());
        }
        if !req.workspace.is_dir() {
            return Err(format!(
                "workspace {} does not exist",
                req.workspace.display()
            ));
        }
        let def = self
            .find_skill("workflow-author")
            .cloned()
            .ok_or_else(|| "the workflow-author skill is not available".to_string())?;
        let home = self.skill_home();
        crate::skill::install::install(&def, req.harness, &home, false).map_err(|e| {
            format!(
                "installing workflow-author for {}: {e}",
                req.harness.as_str()
            )
        })?;
        let id = uuid::Uuid::new_v4().to_string();
        let mut all: Vec<crate::skill::SkillDefinition> = self.skills.clone();
        all.extend(self.hidden_skills.iter().cloned());
        let entries = self.workflow_entries();
        let ctx = crate::workflows::planner::plan_context(
            &req.task,
            req.harness.as_str(),
            &req.workspace,
            &all,
            &entries,
            &crate::agents::Catalog::load(&self.library_root(), Some(&req.workspace)),
            req.budget_tokens,
        );
        let runtime = self.workflows_runtime_dir();
        let plan_path = crate::workflows::planner::write_context(&runtime, &id, &ctx)
            .map_err(|e| format!("planner context: {e}"))?;
        let out_file = runtime
            .join("workflows")
            .join("plans")
            .join(format!("{id}.last.md"));
        let mut profile = self.workflow_profile(req.profile.as_deref(), req.harness);
        let prompts = crate::prompts::Prompts::current();
        let prompt = crate::prompts::render_workflow(
            &prompts.workflow_plan,
            &crate::skill::render::invocation("workflow-author", req.harness),
            "",
            "plan",
            "",
            &plan_path.display().to_string(),
        );
        let options = LaunchOptions {
            model: profile.model.clone(),
            bypass_approvals: true,
            resume: Resume::Off,
            one_shot: Some(prompt),
        };
        let mut args = compose(&profile.args, &options.render(req.harness));
        let timeout_s = self.workflows.session_timeout_s;
        wfh::insert_before_prompt(
            &mut args,
            req.harness,
            wfh::extra_args(req.harness, None, timeout_s, &out_file),
        );
        if Harness::detect(&profile.command) != Some(req.harness) {
            profile.command = req.harness.as_str().to_string();
        }
        profile.args = args;
        profile.name = format!("workflow plan ▸ {}", short_task(&req.task));
        let env: Vec<(String, String)> = vec![
            (
                "AGENT_MUX_WORKFLOW_PLAN".into(),
                plan_path.to_string_lossy().into_owned(),
            ),
            ("AGENT_MUX_WORKFLOW_STEP".into(), "plan".into()),
            (
                "AGENT_MUX_WORKSPACE".into(),
                req.workspace.to_string_lossy().into_owned(),
            ),
        ];
        let launch = crate::workflows::WorkflowLaunch {
            run_id: id.clone(),
            workflow: "plan".into(),
            step: "plan".into(),
            phase: "Plan".into(),
            agent: None,
        };
        let sid = self.next_id;
        let session = self
            .spawn_traced_full(
                sid,
                profile,
                req.workspace.clone(),
                &env,
                Some("workflow-author"),
                None,
                Some(launch),
                &[],
            )
            .map_err(|e| format!("spawn: {e}"))?;
        self.next_id += 1;
        let launch_id = session.trace.as_ref().map(|t| t.launch_id.clone());
        self.sessions.push(session);
        self.live_workflow_plans.push(LivePlan {
            id: id.clone(),
            session_id: sid,
            task: req.task,
            workspace: req.workspace,
            harness: req.harness,
            profile: req.profile,
            budget_tokens: req.budget_tokens,
            started: Instant::now(),
            timeout: Duration::from_secs(timeout_s),
            exited_at: None,
            stdout: Vec::new(),
            out_file,
            launch_id,
            auto_run: req.auto_run,
        });
        Ok(id)
    }

    /// Planner sessions: timeouts and settled answers.
    pub fn workflow_plan_pass(&mut self, now: Instant) {
        let mut to_kill = Vec::new();
        for p in &mut self.live_workflow_plans {
            if p.exited_at.is_none() && now.saturating_duration_since(p.started) > p.timeout {
                p.exited_at = Some(now);
                to_kill.push(p.session_id);
            }
        }
        for sid in to_kill {
            if let Some(i) = self.sessions.iter().position(|s| s.id == sid) {
                self.sessions[i].kill();
            }
        }
        let ready: Vec<usize> = self
            .live_workflow_plans
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.exited_at
                    .is_some_and(|t| now.saturating_duration_since(t) >= SETTLE)
            })
            .map(|(i, _)| i)
            .collect();
        for i in ready.into_iter().rev() {
            self.complete_workflow_plan(i);
        }
    }

    fn complete_workflow_plan(&mut self, i: usize) {
        let plan = self.live_workflow_plans.remove(i);
        let conn_ro = self
            .trace_db_path
            .as_deref()
            .and_then(|p| crate::tracing::store::open_ro(p).ok());
        let stored = match (&conn_ro, &plan.launch_id) {
            (Some(c), Some(l)) => crate::tracing::experiments::final_message(c, l)
                .ok()
                .flatten(),
            _ => None,
        };
        let screen = self
            .sessions
            .iter_mut()
            .find(|s| s.id == plan.session_id)
            .map(|s| s.text_dump())
            .unwrap_or_default();
        let env = wfh::envelope(plan.harness, &plan.stdout, &plan.out_file);
        let text = env
            .text
            .or(stored)
            .unwrap_or_else(|| screen.trim().to_string());
        let infos = self.all_skill_infos();
        let new_agents = crate::workflows::planner::extract_agents(&text);
        let planned = match crate::workflows::planner::extract_document(&text) {
            Ok(doc) => {
                let mut c = crate::workflows::planner::check(&doc, &infos);
                if c.problems.is_empty() {
                    let catalog =
                        crate::agents::Catalog::load(&self.library_root(), Some(&plan.workspace));
                    c.problems = crate::workflows::planner::check_agents(
                        &doc,
                        &new_agents,
                        &catalog,
                        &infos,
                    );
                }
                PlannedWorkflow {
                    id: plan.id.clone(),
                    task: plan.task.clone(),
                    workspace: plan.workspace.clone(),
                    harness: plan.harness,
                    profile: plan.profile.clone(),
                    budget_tokens: plan.budget_tokens,
                    name: c.name,
                    document: c.document,
                    problems: c.problems,
                    raw: None,
                    run_id: None,
                    agents: new_agents,
                }
            }
            Err(e) => PlannedWorkflow {
                id: plan.id.clone(),
                task: plan.task.clone(),
                workspace: plan.workspace.clone(),
                harness: plan.harness,
                profile: plan.profile.clone(),
                budget_tokens: plan.budget_tokens,
                name: "dynamic".into(),
                document: String::new(),
                problems: vec![e],
                raw: Some(text),
                run_id: None,
                agents: new_agents,
            },
        };
        let runtime = self.workflows_runtime_dir();
        let _ = std::fs::write(
            runtime
                .join("workflows")
                .join("plans")
                .join(format!("{}.toml", plan.id)),
            &planned.document,
        );
        let valid = planned.valid();
        self.notice = Some(if valid {
            Notice::info(format!(
                "planned workflow {} for: {}",
                planned.name,
                short_task(&planned.task)
            ))
        } else {
            Notice::warn(format!(
                "the planner's document has problems: {}",
                planned.problems.first().cloned().unwrap_or_default()
            ))
        });
        self.planned_workflows.insert(0, planned);
        self.planned_workflows.truncate(20);
        if valid
            && (plan.auto_run || !self.workflows.dynamic_approval)
            && let Err(e) = self.run_planned_workflow(&plan.id)
        {
            self.notice = Some(Notice::error(format!("planned workflow: {e}")));
        }
    }

    /// Starts the run of a planned document.
    pub fn run_planned_workflow(&mut self, plan_id: &str) -> Result<String, String> {
        let Some(p) = self
            .planned_workflows
            .iter()
            .find(|p| p.id == plan_id)
            .cloned()
        else {
            return Err(format!("no planned workflow {plan_id}"));
        };
        if !p.valid() {
            return Err(p.problems.join("; "));
        }
        crate::workflows::planner::save_agents(&self.library_root(), &p.agents)?;
        let req = WorkflowRunRequest {
            name: p.name.clone(),
            source: "dynamic".into(),
            document: p.document.clone(),
            workspace: p.workspace.clone(),
            profile: p.profile.clone(),
            harness: p.harness,
            args: Value::Null,
            budget_tokens: p.budget_tokens,
            usd_cap: None,
            isolation: None,
            resume_from: None,
            step_overrides: Vec::new(),
        };
        let run_id = self.start_workflow_run(req)?;
        if let Some(p) = self.planned_workflows.iter_mut().find(|p| p.id == plan_id) {
            p.run_id = Some(run_id.clone());
        }
        Ok(run_id)
    }

    /// Saves a planned document into the library under `name`.
    pub fn save_planned_workflow(&self, plan_id: &str, name: &str) -> Result<PathBuf, String> {
        let p = self
            .planned_workflows
            .iter()
            .find(|p| p.id == plan_id)
            .ok_or_else(|| format!("no planned workflow {plan_id}"))?;
        crate::workflows::planner::save_agents(&self.library_root(), &p.agents)?;
        save_document(&self.library_root(), name, &p.document)
    }

    pub fn capture_plan_output(&mut self, session_id: usize, bytes: &[u8]) {
        if let Some(p) = self
            .live_workflow_plans
            .iter_mut()
            .find(|p| p.session_id == session_id)
            && p.stdout.len() < CAPTURE_CAP
        {
            let room = CAPTURE_CAP - p.stdout.len();
            p.stdout.extend_from_slice(&bytes[..bytes.len().min(room)]);
        }
    }

    pub fn finish_plan_for_session(&mut self, session_id: usize) {
        if let Some(p) = self
            .live_workflow_plans
            .iter_mut()
            .find(|p| p.session_id == session_id && p.exited_at.is_none())
        {
            p.exited_at = Some(Instant::now());
        }
    }
}

/// Writes a document into the library as `<name>.toml`, renaming the
/// document to match.
pub fn save_document(
    root: &std::path::Path,
    name: &str,
    document: &str,
) -> Result<PathBuf, String> {
    if !crate::skill::is_valid_skill_id(name) {
        return Err(format!("{name:?} is not a valid workflow name"));
    }
    let renamed = rename_document(document, name);
    crate::workflows::parse(&renamed).map_err(|p| p.join("; "))?;
    let dir = library::dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{name}.toml"));
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    std::fs::write(&path, renamed).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Replaces the `name = "…"` of `[workflow]` with `name`.
pub fn rename_document(document: &str, name: &str) -> String {
    let mut out = String::new();
    let mut in_workflow = false;
    let mut done = false;
    for line in document.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_workflow = t == "[workflow]";
        }
        if in_workflow && !done && t.starts_with("name") && t[4..].trim_start().starts_with('=') {
            out.push_str(&format!("name = \"{name}\"\n"));
            done = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn short_task(task: &str) -> String {
    let t: String = task.chars().take(48).collect();
    if task.chars().count() > 48 {
        format!("{t}…")
    } else {
        t
    }
}

// ---- the section, the dialog and the view -------------------------------------

use crate::app::dir_picker::PickerEvent;
use crate::app::workflows_view::{
    DialogField, DialogPurpose, RunRow, ViewFacts, ViewPane, ViewPending, WorkflowDialogState,
    WorkflowsViewState,
};
use crate::app::{Action, Mode};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The glyph and colour of a workflow row in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowGlyph {
    Idle,
    Running,
    Finished,
    Attention,
    Paused,
}

impl WorkflowGlyph {
    pub fn glyph(self) -> &'static str {
        match self {
            WorkflowGlyph::Idle => "○",
            WorkflowGlyph::Running => "▶",
            WorkflowGlyph::Finished => "✓",
            WorkflowGlyph::Attention => "!",
            WorkflowGlyph::Paused => "‖",
        }
    }

    pub fn color(self) -> ratatui::style::Color {
        use ratatui::style::Color;
        match self {
            WorkflowGlyph::Idle => Color::DarkGray,
            WorkflowGlyph::Running => Color::Cyan,
            WorkflowGlyph::Finished => Color::Green,
            WorkflowGlyph::Attention => Color::Red,
            WorkflowGlyph::Paused => Color::Yellow,
        }
    }
}

impl App {
    /// Rescans the documents; keeps the selection on the same row.
    pub fn reload_workflow_list(&mut self) {
        let keep = self
            .workflow_anchor
            .clone()
            .or_else(|| self.workflow_rows().get(self.selected_workflow).cloned());
        let doc = match &keep {
            Some(WorkflowRow::Doc(i)) => self.workflow_list.get(*i).map(|e| e.name.clone()),
            _ => None,
        };
        self.workflow_list = self.workflow_entries();
        let keep = match doc {
            Some(name) => self
                .workflow_list
                .iter()
                .position(|e| e.name == name)
                .map(WorkflowRow::Doc),
            None => keep,
        };
        self.workflow_anchor = keep;
        self.resync_workflow_selection();
    }

    /// The section's selectable rows, in the order they are drawn: live
    /// runs, plans awaiting a decision, runs finished since startup (the
    /// last few), then the library.
    pub fn workflow_rows(&self) -> Vec<WorkflowRow> {
        let mut rows: Vec<WorkflowRow> = self
            .live_workflow_runs
            .iter()
            .map(|r| WorkflowRow::Live(r.run_id.clone()))
            .collect();
        rows.extend(
            self.planned_workflows
                .iter()
                .filter(|p| p.run_id.is_none())
                .map(|p| WorkflowRow::Planned(p.id.clone())),
        );
        rows.extend(
            self.recent_workflow_runs
                .iter()
                .filter(|r| !self.live_workflow_runs.iter().any(|l| l.run_id == r.run_id))
                .take(RECENT_IN_SECTION)
                .map(|r| WorkflowRow::Recent(r.run_id.clone())),
        );
        rows.extend((0..self.workflow_list.len()).map(WorkflowRow::Doc));
        rows
    }

    /// The section as drawn: a heading before each group once anything
    /// but the library is listed, and the selectable rows.
    pub fn workflow_section_lines(&self) -> Vec<SectionLine> {
        let rows = self.workflow_rows();
        let grouped = rows.iter().any(|r| !matches!(r, WorkflowRow::Doc(_)));
        let mut out = Vec::new();
        let mut last: Option<&'static str> = None;
        for (i, r) in rows.iter().enumerate() {
            let group = r.group();
            if grouped && last != Some(group) {
                out.push(SectionLine::Header(group));
                last = Some(group);
            }
            out.push(SectionLine::Row(i));
        }
        out
    }

    /// The first drawn line of the section at `visible` rows, keeping the
    /// selection in view; the renderer and the mouse share it.
    pub fn workflow_section_start(&self, lines: &[SectionLine], visible: usize) -> usize {
        let pos = lines
            .iter()
            .position(|l| *l == SectionLine::Row(self.selected_workflow))
            .unwrap_or(0);
        crate::ui::sidebar_window(pos, lines.len(), visible)
    }

    /// Selects row `i` and remembers which row it is, so a run starting or
    /// ending does not move the selection onto another one.
    pub fn select_workflow_row(&mut self, i: usize) {
        let rows = self.workflow_rows();
        self.selected_workflow = i.min(rows.len().saturating_sub(1));
        self.workflow_anchor = rows.get(self.selected_workflow).cloned();
    }

    /// Puts the selection back on its row after the lists changed; a run
    /// that finished is followed to its Recent row.
    pub fn resync_workflow_selection(&mut self) {
        let rows = self.workflow_rows();
        let Some(anchor) = self.workflow_anchor.clone() else {
            self.selected_workflow = self.selected_workflow.min(rows.len().saturating_sub(1));
            return;
        };
        let found = rows
            .iter()
            .position(|r| *r == anchor)
            .or_else(|| match &anchor {
                WorkflowRow::Live(id) => rows
                    .iter()
                    .position(|r| matches!(r, WorkflowRow::Recent(x) if x == id)),
                _ => None,
            });
        match found {
            Some(i) => {
                self.selected_workflow = i;
                self.workflow_anchor = rows.get(i).cloned();
            }
            None => {
                self.selected_workflow = self.selected_workflow.min(rows.len().saturating_sub(1));
                self.workflow_anchor = rows.get(self.selected_workflow).cloned();
            }
        }
    }

    pub fn selected_workflow_row(&self) -> Option<WorkflowRow> {
        self.workflow_rows().get(self.selected_workflow).cloned()
    }

    /// The document behind the selected row: the library entry, or the one
    /// a live or recent run was started from. A plan has none yet.
    pub fn selected_workflow_entry(&self) -> Option<&library::Entry> {
        let by_name = |name: &str| self.workflow_list.iter().find(|e| e.name == name);
        match self.selected_workflow_row()? {
            WorkflowRow::Doc(i) => self.workflow_list.get(i),
            WorkflowRow::Live(id) => self
                .live_workflow_runs
                .iter()
                .find(|r| r.run_id == id)
                .and_then(|r| by_name(&r.name)),
            WorkflowRow::Recent(id) => self
                .recent_workflow_runs
                .iter()
                .find(|r| r.run_id == id)
                .and_then(|r| by_name(&r.name)),
            WorkflowRow::Planned(_) => None,
        }
    }

    /// The live run of a workflow name, if any.
    pub fn live_run_of(&self, name: &str) -> Option<&LiveWorkflowRun> {
        self.live_workflow_runs.iter().find(|r| r.name == name)
    }

    /// The row of a workflow: glyph and the right-hand text.
    pub fn workflow_row(&self, entry: &library::Entry) -> (WorkflowGlyph, String) {
        if let Some(r) = self.live_run_of(&entry.name) {
            return (
                WorkflowGlyph::Running,
                format!("{}/{}", r.state.records.len(), r.state.sessions_started),
            );
        }
        if self.loop_registry.pause_all {
            return (WorkflowGlyph::Paused, String::new());
        }
        if !entry.valid() {
            return (WorkflowGlyph::Attention, "invalid".into());
        }
        match self
            .recent_workflow_runs
            .iter()
            .find(|r| r.name == entry.name)
        {
            Some(r) if r.status == "finished" => {
                (WorkflowGlyph::Finished, format!("{}", r.sessions))
            }
            Some(_) => (WorkflowGlyph::Attention, String::new()),
            None => (WorkflowGlyph::Idle, String::new()),
        }
    }

    /// Where the dialog's workspace starts: the selected session's
    /// directory, the current directory, or a profile's default.
    pub(crate) fn dialog_workspaces(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        if let Some(s) = self.sessions.get(self.selected) {
            v.push(s.dir.to_string_lossy().into_owned());
        }
        if let Ok(cwd) = std::env::current_dir() {
            v.push(cwd.to_string_lossy().into_owned());
        }
        for p in &self.profiles {
            if let Some(d) = &p.default_dir {
                v.push(d.clone());
            }
        }
        v
    }

    /// `Enter` in the section: a run or a plan opens in the view on that
    /// row; a library document opens the run dialog.
    pub fn open_workflow_run(&mut self) {
        let target = match self.selected_workflow_row() {
            None => {
                self.notice = Some(Notice::info(
                    "no workflows; f builds one step by step, c composes one for a task",
                ));
                return;
            }
            Some(WorkflowRow::Live(id)) => Some(RunRow::Live(id)),
            Some(WorkflowRow::Recent(id)) => Some(RunRow::Stored(id)),
            Some(WorkflowRow::Planned(id)) => Some(RunRow::Planned(id)),
            Some(WorkflowRow::Doc(_)) => None,
        };
        if let Some(row) = target {
            self.open_workflows_view_on(row);
            return;
        }
        let Some(entry) = self.selected_workflow_entry().cloned() else {
            return;
        };
        if !entry.valid() {
            self.notice = Some(Notice::warn(format!(
                "{}: {}",
                entry.name,
                entry.problems.first().cloned().unwrap_or_default()
            )));
            return;
        }
        let d = WorkflowDialogState::for_run(&entry, &self.profiles, self.dialog_workspaces());
        self.mode = Mode::WorkflowDialog(Box::new(d));
    }

    /// `d` in the section: a plan is discarded after a y/n in the view; a
    /// library document is removed from the Configuration view.
    pub fn delete_selected_workflow_row(&mut self) {
        match self.selected_workflow_row() {
            Some(WorkflowRow::Planned(id)) => {
                self.open_workflows_view_on(RunRow::Planned(id.clone()));
                if let Mode::WorkflowsView(v) = &mut self.mode {
                    v.pending = crate::app::workflows_view::ViewPending::Discard(id);
                }
            }
            Some(WorkflowRow::Live(_)) => {
                self.notice = Some(Notice::info("x stops a running workflow"));
            }
            _ => {
                self.notice = Some(Notice::info(
                    "d discards a plan; a library workflow is removed with R in the Configuration view (C)",
                ));
            }
        }
    }

    /// `c` in the section: the planner dialog.
    pub fn open_workflow_plan(&mut self) {
        let d = WorkflowDialogState::for_plan(&self.profiles, self.dialog_workspaces());
        self.mode = Mode::WorkflowDialog(Box::new(d));
    }

    /// `e` in the section: the document in the editor (a library copy is
    /// created for a built-in).
    pub fn edit_selected_workflow(&mut self) {
        let Some(entry) = self.selected_workflow_entry().cloned() else {
            return;
        };
        let path = match &entry.source {
            library::Source::Library(p) => p.clone(),
            library::Source::Builtin => {
                let p = library::dir(&self.library_root()).join(format!("{}.toml", entry.name));
                if let Some(parent) = p.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if !p.exists()
                    && let Err(e) = std::fs::write(&p, &entry.text)
                {
                    self.notice = Some(Notice::error(format!("{}: {e}", p.display())));
                    return;
                }
                p
            }
            library::Source::Skill(id) => {
                self.notice = Some(Notice::info(format!(
                    "{} is distributed by the {id} skill; edit that package in the Configuration view",
                    entry.name
                )));
                return;
            }
        };
        let id = format!("workflows/{}.toml", entry.name);
        let command = crate::assets::editor_command(self.editor.as_deref());
        self.editor_request = Some(crate::app::EditorRequest {
            path,
            asset_id: id,
            command,
        });
    }

    /// `x` in the section: cancel the live run of the selected workflow.
    pub fn cancel_selected_workflow(&mut self) {
        let Some(name) = self.selected_workflow_entry().map(|e| e.name.clone()) else {
            return;
        };
        let id = self.live_run_of(&name).map(|r| r.run_id.clone());
        match id {
            Some(id) => {
                self.cancel_workflow_run(&id);
                self.notice = Some(Notice::info(format!("cancelling {name}")));
            }
            None => self.notice = Some(Notice::info(format!("{name} is not running"))),
        }
    }

    pub fn handle_workflow_dialog_key(&mut self, key: &KeyEvent) {
        let Mode::WorkflowDialog(dialog) = &mut self.mode else {
            return;
        };
        if dialog.field == DialogField::Workspace {
            let WorkflowDialogState {
                dir_picker,
                workspace,
                ..
            } = &mut **dialog;
            match dir_picker.handle_key(key, workspace) {
                PickerEvent::Submit => {
                    let d = (**dialog).clone();
                    self.confirm_workflow_dialog(d);
                    return;
                }
                PickerEvent::Consumed { .. } => {
                    dialog.error = None;
                    return;
                }
                PickerEvent::Ignored => {}
            }
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let on_text = dialog.text().is_some();
        // Enter submits in every agent-mux dialog, so a newline is
        // Alt+Enter (Shift+Enter and Ctrl+J where the terminal sends them)
        let wants_newline = matches!(key.code, KeyCode::Enter) && (alt || shift)
            || (ctrl && matches!(key.code, KeyCode::Char('j')));
        if wants_newline && on_text {
            if let Some(t) = dialog.text_mut() {
                t.newline();
            }
            dialog.error = None;
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Control;
            }
            KeyCode::Enter => {
                let d = (**dialog).clone();
                self.confirm_workflow_dialog(d);
            }
            KeyCode::Tab => {
                dialog.step_field(1);
                dialog.dir_picker.leave();
            }
            KeyCode::BackTab => {
                dialog.step_field(-1);
                dialog.dir_picker.leave();
            }
            // inside a text field the arrows move the cursor; at its edges
            // they move to the next field, the way a form behaves
            KeyCode::Down => {
                let moved = dialog.text_mut().is_some_and(|t| t.down());
                if !moved {
                    dialog.step_field(1);
                    dialog.dir_picker.leave();
                }
            }
            KeyCode::Up => {
                let moved = dialog.text_mut().is_some_and(|t| t.up());
                if !moved {
                    dialog.step_field(-1);
                    dialog.dir_picker.leave();
                }
            }
            KeyCode::Left => {
                if let Some(t) = dialog.text_mut() {
                    t.left();
                } else {
                    dialog.cycle(-1);
                }
            }
            KeyCode::Right => {
                if let Some(t) = dialog.text_mut() {
                    t.right();
                } else {
                    dialog.cycle(1);
                }
            }
            KeyCode::Home => {
                if let Some(t) = dialog.text_mut() {
                    t.home();
                }
            }
            KeyCode::End => {
                if let Some(t) = dialog.text_mut() {
                    t.end();
                }
            }
            KeyCode::Char(' ')
                if matches!(
                    dialog.field,
                    DialogField::Profile
                        | DialogField::Isolation
                        | DialogField::StepHarness(_)
                        | DialogField::More
                ) =>
            {
                dialog.cycle(1)
            }
            KeyCode::Backspace => {
                if let Some(t) = dialog.text_mut() {
                    if ctrl || alt {
                        t.delete_word();
                    } else {
                        t.backspace();
                    }
                }
                dialog.error = None;
            }
            KeyCode::Delete => {
                if let Some(t) = dialog.text_mut() {
                    t.delete();
                }
                dialog.error = None;
            }
            // Ctrl+W deletes a word, Ctrl+U clears the field
            KeyCode::Char('w') if ctrl => {
                if let Some(t) = dialog.text_mut() {
                    t.delete_word();
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(t) = dialog.text_mut() {
                    t.set("");
                }
            }
            KeyCode::Char('v') if ctrl && on_text => {
                if let Some(text) = self.clipboard_text()
                    && let Mode::WorkflowDialog(d) = &mut self.mode
                    && let Some(t) = d.text_mut()
                {
                    t.insert_str(&text);
                    d.error = None;
                }
            }
            // Ctrl+O writes the field to a file and opens the editor; the
            // text comes back in `editor_finished`
            KeyCode::Char('o') if ctrl && on_text => self.open_dialog_editor(),
            // start and end of the line, as in macOS text fields
            KeyCode::Char('a') if ctrl => {
                if let Some(t) = dialog.text_mut() {
                    t.home();
                }
            }
            KeyCode::Char('e') if ctrl => {
                if let Some(t) = dialog.text_mut() {
                    t.end();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(t) = dialog.text_mut() {
                    t.insert(c);
                }
                dialog.error = None;
            }
            _ => {}
        }
    }

    /// The clipboard's text, or a notice saying why there is none.
    pub(crate) fn clipboard_text(&mut self) -> Option<String> {
        if !self.clipboard_enabled {
            self.notice = Some(Notice::info("the clipboard is disabled in this session"));
            return None;
        }
        match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) => Some(t),
            Err(e) => {
                self.notice = Some(Notice::error(format!("clipboard: {e}")));
                None
            }
        }
    }

    /// `Ctrl+E` in a dialog text field: compose it in `$EDITOR`. The
    /// dialog stays open while the editor runs, so the text goes back
    /// into the same field on return.
    fn open_dialog_editor(&mut self) {
        let Mode::WorkflowDialog(dialog) = &self.mode else {
            return;
        };
        let Some(t) = dialog.text() else {
            return;
        };
        let dir = self.workflows_runtime_dir().join("workflows");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.notice = Some(Notice::error(format!("{}: {e}", dir.display())));
            return;
        }
        let path = dir.join("compose.md");
        if let Err(e) = std::fs::write(&path, &t.text) {
            self.notice = Some(Notice::error(format!("{}: {e}", path.display())));
            return;
        }
        let command = crate::assets::editor_command(self.editor.as_deref());
        self.editor_request = Some(crate::app::EditorRequest {
            path,
            asset_id: "dialog:text".into(),
            command,
        });
    }

    /// After `Ctrl+E`: the edited text returns to the focused field.
    pub fn dialog_editor_finished(&mut self, path: &std::path::Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                self.notice = Some(Notice::error(format!("{}: {e}", path.display())));
                return;
            }
        };
        if let Mode::WorkflowDialog(d) = &mut self.mode
            && let Some(t) = d.text_mut()
        {
            t.set(text.trim_end_matches('\n'));
            d.error = None;
        }
    }

    fn confirm_workflow_dialog(&mut self, d: WorkflowDialogState) {
        let fail = |app: &mut App, e: String| {
            if let Mode::WorkflowDialog(dialog) = &mut app.mode {
                dialog.error = Some(e);
            }
        };
        let Some(harness) = d.harness() else {
            fail(
                self,
                "no profile for this workflow's harnesses in profiles.toml".into(),
            );
            return;
        };
        let workspace = PathBuf::from(d.workspace.trim());
        if d.workspace.trim().is_empty() || !workspace.is_dir() {
            fail(
                self,
                format!("workspace {} does not exist", d.workspace.trim()),
            );
            return;
        }
        let budget = match d.budget_tokens() {
            Ok(b) => b,
            Err(e) => {
                fail(self, e);
                return;
            }
        };
        match &d.purpose {
            DialogPurpose::Plan => {
                let req = PlanRequest {
                    task: d.task.text.trim().to_string(),
                    workspace,
                    harness,
                    profile: d.profile_name(),
                    budget_tokens: budget,
                    auto_run: false,
                };
                match self.start_workflow_plan(req) {
                    Ok(_) => {
                        self.mode = Mode::Control;
                        self.notice = Some(Notice::info(
                            "planning; the document appears in the Workflows view (W) when the planner answers",
                        ));
                    }
                    Err(e) => fail(self, e),
                }
            }
            DialogPurpose::Run { name } => {
                let usd_cap = match d.usd_cap() {
                    Ok(c) => c,
                    Err(e) => {
                        fail(self, e);
                        return;
                    }
                };
                let source = self
                    .workflow_list
                    .iter()
                    .find(|e| e.name == *name)
                    .map(|e| e.source.label())
                    .unwrap_or_else(|| "library".into());
                let req = WorkflowRunRequest {
                    name: name.clone(),
                    source,
                    document: d.document.clone(),
                    workspace,
                    profile: d.profile_name(),
                    harness,
                    args: d.args_value(),
                    budget_tokens: budget,
                    usd_cap,
                    isolation: Some(d.isolation),
                    resume_from: None,
                    step_overrides: d.step_overrides(),
                };
                match self.start_workflow_run(req) {
                    Ok(id) => {
                        self.mode = Mode::Control;
                        self.notice = Some(Notice::info(format!(
                            "workflow {name} started ({}); W follows it",
                            &id[..8]
                        )));
                    }
                    Err(e) => fail(self, e),
                }
            }
        }
    }

    pub fn view_facts(&self) -> ViewFacts<'_> {
        ViewFacts {
            live: &self.live_workflow_runs,
            recent: &self.recent_workflow_runs,
            planned: &self.planned_workflows,
            plans_in_flight: self.live_workflow_plans.len(),
        }
    }

    pub fn open_workflows_view(&mut self) {
        let facts = self.view_facts();
        let view = WorkflowsViewState::new(
            self.trace_db_path.as_deref(),
            self.runtime_dir.as_deref(),
            &facts,
        );
        self.mode = Mode::WorkflowsView(Box::new(view));
    }

    /// Ticks the open view.
    pub fn refresh_workflows_view(&mut self, now: Instant) {
        let live = std::mem::take(&mut self.live_workflow_runs);
        let recent = std::mem::take(&mut self.recent_workflow_runs);
        let planned = std::mem::take(&mut self.planned_workflows);
        let plans = self.live_workflow_plans.len();
        if let Mode::WorkflowsView(view) = &mut self.mode {
            let facts = ViewFacts {
                live: &live,
                recent: &recent,
                planned: &planned,
                plans_in_flight: plans,
            };
            view.refresh_if_due(now, &facts);
        }
        self.live_workflow_runs = live;
        self.recent_workflow_runs = recent;
        self.planned_workflows = planned;
    }

    /// Runs `f` on the view with the facts borrowed out of `self`.
    pub(crate) fn with_view<R>(
        &mut self,
        f: impl FnOnce(&mut WorkflowsViewState, &ViewFacts<'_>) -> R,
    ) -> Option<R> {
        let live = std::mem::take(&mut self.live_workflow_runs);
        let recent = std::mem::take(&mut self.recent_workflow_runs);
        let planned = std::mem::take(&mut self.planned_workflows);
        let plans = self.live_workflow_plans.len();
        let out = if let Mode::WorkflowsView(view) = &mut self.mode {
            let facts = ViewFacts {
                live: &live,
                recent: &recent,
                planned: &planned,
                plans_in_flight: plans,
            };
            Some(f(view, &facts))
        } else {
            None
        };
        self.live_workflow_runs = live;
        self.recent_workflow_runs = recent;
        self.planned_workflows = planned;
        out
    }

    /// The wheel over the Workflows view: the focused pane moves, so the
    /// runs list steps and the detail pane scrolls.
    pub fn scroll_workflows_view(&mut self, delta: isize) {
        self.with_view(|view, facts| match view.focus {
            ViewPane::Runs => view.step(delta, facts),
            ViewPane::Detail => view.scroll(delta),
        });
    }

    pub fn handle_workflows_view_key(&mut self, key: &KeyEvent) {
        let Mode::WorkflowsView(view) = &mut self.mode else {
            return;
        };
        let page = view.viewport_rows.get().max(1) as isize;
        // footer questions first
        match view.pending.clone() {
            ViewPending::Cancel => {
                let yes = matches!(
                    key.code,
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
                );
                let no = matches!(
                    key.code,
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc
                );
                if !yes && !no {
                    return;
                }
                view.pending = ViewPending::None;
                if yes && let Some(RunRow::Live(id)) = view.selected_row().cloned() {
                    self.cancel_workflow_run(&id);
                    self.notice = Some(Notice::info("cancelling the run"));
                }
                return;
            }
            ViewPending::Discard(id) => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        view.pending = ViewPending::None;
                        self.planned_workflows.retain(|p| p.id != id);
                        self.with_view(|v, f| v.reload(f));
                        self.notice = Some(Notice::info("planned document discarded"));
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        view.pending = ViewPending::None
                    }
                    _ => {}
                }
                return;
            }
            ViewPending::SaveName(mut name) => {
                match key.code {
                    KeyCode::Esc => view.pending = ViewPending::None,
                    KeyCode::Backspace => {
                        name.pop();
                        view.pending = ViewPending::SaveName(name);
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        name.push(c);
                        view.pending = ViewPending::SaveName(name);
                    }
                    KeyCode::Enter => {
                        view.pending = ViewPending::None;
                        let row = view.selected_row().cloned();
                        let result = match row {
                            Some(RunRow::Planned(id)) => self.save_planned_workflow(&id, &name),
                            Some(RunRow::Live(id)) => {
                                let doc = self
                                    .live_workflow_runs
                                    .iter()
                                    .find(|r| r.run_id == id)
                                    .map(|r| r.document.clone())
                                    .unwrap_or_default();
                                save_document(&self.library_root(), &name, &doc)
                            }
                            Some(RunRow::Stored(id)) => {
                                let doc = if let Mode::WorkflowsView(v) = &self.mode {
                                    v.stored
                                        .iter()
                                        .find(|r| r.id == id)
                                        .map(|r| r.document.clone())
                                } else {
                                    None
                                };
                                save_document(&self.library_root(), &name, &doc.unwrap_or_default())
                            }
                            _ => Err("select a run or a planned document".into()),
                        };
                        self.notice = Some(match result {
                            Ok(p) => {
                                self.reload_workflow_list();
                                Notice::info(format!("saved {}", p.display()))
                            }
                            Err(e) => Notice::warn(e),
                        });
                    }
                    _ => {}
                }
                return;
            }
            ViewPending::None => {}
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if view.focus == ViewPane::Detail {
                    view.focus = ViewPane::Runs;
                } else {
                    self.mode = Mode::Control;
                }
            }
            KeyCode::Tab => {
                self.with_view(|v, f| v.next_tab(f));
            }
            KeyCode::BackTab => {
                self.with_view(|v, f| v.prev_tab(f));
            }
            KeyCode::Char(c @ '1'..='4') => {
                let tab = super::workflows_view::ViewTab::ALL[c as usize - '1' as usize];
                self.with_view(|v, f| v.set_tab(tab, f));
            }
            KeyCode::Right => view.focus = ViewPane::Detail,
            KeyCode::Left => view.focus = ViewPane::Runs,
            KeyCode::Down | KeyCode::Char('j') => match view.focus {
                ViewPane::Runs => {
                    self.with_view(|v, f| v.step(1, f));
                }
                ViewPane::Detail => view.scroll(1),
            },
            KeyCode::Up | KeyCode::Char('k') => match view.focus {
                ViewPane::Runs => {
                    self.with_view(|v, f| v.step(-1, f));
                }
                ViewPane::Detail => view.scroll(-1),
            },
            // Scrolling the detail pane never asks for focus first: a long
            // result is the usual reason the view is open.
            KeyCode::PageDown | KeyCode::Char(' ') => view.scroll(page),
            KeyCode::PageUp => view.scroll(-page),
            KeyCode::Home | KeyCode::Char('g') => view.scroll_to_top(),
            KeyCode::End | KeyCode::Char('G') => view.scroll_to_bottom(),
            KeyCode::Enter => {
                let row = view.selected_row().cloned();
                match row {
                    Some(RunRow::Planned(id)) => match self.run_planned_workflow(&id) {
                        Ok(rid) => {
                            self.notice = Some(Notice::info(format!("run {} started", &rid[..8])));
                            self.with_view(|v, f| v.reload(f));
                        }
                        Err(e) => self.notice = Some(Notice::warn(e)),
                    },
                    Some(RunRow::Live(id)) => {
                        // attach to the first running session of the run
                        let sid = self
                            .live_workflow_runs
                            .iter()
                            .find(|r| r.run_id == id)
                            .and_then(|r| r.sessions.iter().find(|s| s.exited_at.is_none()))
                            .map(|s| s.session_id);
                        match sid.and_then(|sid| self.sessions.iter().position(|s| s.id == sid)) {
                            Some(idx) => self.attach_to_session(idx),
                            None => {
                                self.notice = Some(Notice::info("no session is running right now"))
                            }
                        }
                    }
                    Some(RunRow::Stored(_)) => {
                        self.notice =
                            Some(Notice::info("r resumes a stored run; s saves its document"));
                    }
                    _ => {}
                }
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.with_view(|v, f| v.reload(f));
            }
            KeyCode::Char('r') => {
                let row = view.selected_row().cloned();
                if let Some(RunRow::Stored(id)) = row {
                    let stored = if let Mode::WorkflowsView(v) = &self.mode {
                        v.stored.iter().find(|r| r.id == id).cloned()
                    } else {
                        None
                    };
                    if let Some(r) = stored {
                        let req = WorkflowRunRequest {
                            name: r.workflow.clone(),
                            source: r.source.clone(),
                            document: r.document.clone(),
                            workspace: PathBuf::from(&r.workspace),
                            profile: Some(r.profile.clone()).filter(|p| !p.is_empty()),
                            harness: Harness::detect(&r.harness).unwrap_or(Harness::Claude),
                            args: r.args.clone(),
                            budget_tokens: r.budget_tokens.map(|b| b as u64),
                            usd_cap: None,
                            isolation: None,
                            resume_from: Some(r.id.clone()),
                            step_overrides: Vec::new(),
                        };
                        self.notice = Some(match self.start_workflow_run(req) {
                            Ok(rid) => Notice::info(format!("resumed as {}", &rid[..8])),
                            Err(e) => Notice::warn(e),
                        });
                        self.with_view(|v, f| v.reload(f));
                    }
                } else {
                    self.notice = Some(Notice::info(
                        "r resumes a stored run; Ctrl+R reloads the list",
                    ));
                }
            }
            KeyCode::Char('s') => {
                let default = match view.selected_row() {
                    Some(RunRow::Planned(id)) => self
                        .planned_workflows
                        .iter()
                        .find(|p| p.id == *id)
                        .map(|p| p.name.clone()),
                    Some(RunRow::Live(id)) => self
                        .live_workflow_runs
                        .iter()
                        .find(|r| r.run_id == *id)
                        .map(|r| r.name.clone()),
                    Some(RunRow::Stored(id)) => view
                        .stored
                        .iter()
                        .find(|r| r.id == *id)
                        .map(|r| r.workflow.clone()),
                    _ => None,
                };
                if let Some(d) = default {
                    view.pending = ViewPending::SaveName(d);
                }
            }
            // x stops a live run; d discards a plan, asking first
            KeyCode::Char('x') => {
                if let Some(RunRow::Live(_)) = view.selected_row() {
                    view.pending = ViewPending::Cancel;
                }
            }
            KeyCode::Char('d') => {
                if let Some(RunRow::Planned(id)) = view.selected_row().cloned() {
                    view.pending = ViewPending::Discard(id);
                }
            }
            KeyCode::Char('e') => {
                if let Some(RunRow::Planned(id)) = view.selected_row().cloned() {
                    self.open_flow_builder_plan(&id);
                }
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let row = view.selected_row().cloned();
                if let Some(RunRow::Planned(id)) = row {
                    let doc = self.planned_workflows.iter().find(|p| p.id == id).cloned();
                    if let Some(p) = doc {
                        let path = self
                            .workflows_runtime_dir()
                            .join("workflows")
                            .join("plans")
                            .join(format!("{}.toml", p.id));
                        let _ = std::fs::write(&path, &p.document);
                        let command = crate::assets::editor_command(self.editor.as_deref());
                        self.editor_request = Some(crate::app::EditorRequest {
                            path,
                            asset_id: format!("plan:{}", p.id),
                            command,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// After the editor closed a planned document: re-read and re-check it.
    pub fn reload_planned_after_edit(&mut self, plan_id: &str) {
        let path = self
            .workflows_runtime_dir()
            .join("workflows")
            .join("plans")
            .join(format!("{plan_id}.toml"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let infos = self.all_skill_infos();
        let mut checked = crate::workflows::planner::check(&text, &infos);
        if let Some(p) = self.planned_workflows.iter().find(|p| p.id == plan_id)
            && checked.problems.is_empty()
        {
            let catalog = crate::agents::Catalog::load(&self.library_root(), Some(&p.workspace));
            checked.problems =
                crate::workflows::planner::check_agents(&text, &p.agents, &catalog, &infos);
        }
        if let Some(p) = self.planned_workflows.iter_mut().find(|p| p.id == plan_id) {
            p.document = checked.document;
            p.name = checked.name;
            p.problems = checked.problems;
            p.raw = None;
        }
        self.with_view(|v, f| v.reload(f));
    }

    /// The action for a key in the Workflows section (Control mode).
    pub fn workflows_section_action(key: &KeyEvent) -> Option<Action> {
        match key.code {
            // the shared keymap (`crate::keymap`): n new, e edit, x stop,
            // d delete, r run, Ctrl+O the file in $EDITOR; c composes
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Action::EditWorkflow)
            }
            KeyCode::Enter => Some(Action::OpenWorkflowRun),
            KeyCode::Char('c') => Some(Action::OpenWorkflowPlan),
            KeyCode::Char('n') => Some(Action::OpenFlowBuilder),
            KeyCode::Char('e') => Some(Action::OpenFlowBuilderSelected),
            KeyCode::Char('x') => Some(Action::CancelWorkflow),
            KeyCode::Char('d') => Some(Action::DeleteWorkflowRow),
            KeyCode::Char('r') => Some(Action::OpenWorkflowRun),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::awaiting_verdict;
    use serde_json::json;

    #[test]
    fn a_verdict_waits_unless_accepted_and_uncontradicted() {
        let accept = vec!["APPROVED".to_string()];
        let ok =
            json!("ORACLE_VERDICT: APPROVED\nBLOCKING_COUNT: 0\nHUMAN_REVIEW_REQUIRED: false\n");
        assert_eq!(awaiting_verdict(&accept, "finished", &ok), None);
        assert_eq!(
            awaiting_verdict(&accept, "finished", &json!("ORACLE_VERDICT: approved")),
            None,
            "case does not matter"
        );
        assert_eq!(
            awaiting_verdict(&accept, "finished", &json!("ORACLE_VERDICT: NEEDS_CHANGES"))
                .as_deref(),
            Some("NEEDS_CHANGES")
        );
        // the word says APPROVED but its own header disagrees
        let contradicted = json!("ORACLE_VERDICT: APPROVED\nBLOCKING_COUNT: 2\n");
        assert!(awaiting_verdict(&accept, "finished", &contradicted).is_some());
        let human = json!("ORACLE_VERDICT: APPROVED\nHUMAN_REVIEW_REQUIRED: true\n");
        assert!(awaiting_verdict(&accept, "finished", &human).is_some());
        assert_eq!(
            awaiting_verdict(&accept, "finished", &json!("## Report")).as_deref(),
            Some("no verdict")
        );
        // no accept list, or a run that did not finish: the old rules apply
        assert_eq!(awaiting_verdict(&[], "finished", &json!("x")), None);
        assert_eq!(awaiting_verdict(&accept, "failed", &json!(null)), None);
    }
}
