//! The Run screen's tree, as data: a run row, one row per step of the
//! document in document order (also the steps not reached yet), and a row
//! per session under a step that has several (fan-out items, verify
//! votes, judges, rounds). Design:
//! docs/superpowers/specs/2026-11-13-run-screen-and-flow-editor-ux-design.md
//! (sections 2.2 and 4).
//!
//! Two constructors give the same rows: `from_live` reads a run in memory
//! (the interpreter's `RunState` and its live sessions, handed over as
//! plain data), `from_stored` reads `workflow_runs` and `workflow_steps`.
//! Nothing here knows the App or draws anything.
//!
//! A resumed run's journal starts as a copy of the entries it may replay,
//! and the store does not say which of them the run consumed; `from_stored`
//! works that out (`replayed_entries`), exactly for a run that has ended
//! unless its budget ran out in the middle of a step.
//!
//! A stored run has no step phases and no item counts, so `from_stored`
//! reads the run again (`resimulate`): the interpreter over the stored
//! arguments, the copied entries and the answers of the run's own rows.
//! That gives the item count of a step whose items come from the
//! arguments or an earlier step. The phases are inferred, exactly for a
//! finished run. For one that is still going, or that stopped early, two
//! things are estimates: a step that needed no session (shown as
//! waiting), and an `until` step between two rounds (shown as partly
//! done). The live tree has the interpreter's own answer for both.

use super::document::{Over, Runner, Step, StepKind, Workflow};
use super::interp::{
    Caps, Role, RunState, RunStatus, SessionDone, SessionKey, SessionSpec, StepPhase,
};
use super::journal::Entry;
use super::result::Outcome;
use super::store::{WorkflowRun, WorkflowStep};
use serde_json::Value;
use std::collections::BTreeMap;

/// What a row is doing. The glyphs are the Swarm tree's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not started.
    Waiting,
    Running,
    /// Some of its sessions answered, others have not.
    PartlyDone,
    Done,
    /// Stopped without an answer: the run failed or was cancelled under it.
    Failed,
    /// It ended with no usable answer.
    NullAnswer,
    /// Answered from the journal of the run this one resumed.
    Replayed,
    /// A route's branch not taken.
    Skipped,
}

impl State {
    pub fn glyph(self) -> &'static str {
        match self {
            State::Waiting => "⋯",
            State::Running => "▶",
            State::PartlyDone => "◷",
            State::Done => "✓",
            State::Failed => "✗",
            State::NullAnswer => "∅",
            State::Replayed => "↷",
            State::Skipped => "–",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            State::Waiting => "waiting",
            State::Running => "running",
            State::PartlyDone => "partly done",
            State::Done => "done",
            State::Failed => "failed",
            State::NullAnswer => "no answer",
            State::Replayed => "replayed",
            State::Skipped => "skipped",
        }
    }

    /// The row will not change any more.
    pub fn ended(self) -> bool {
        !matches!(self, State::Waiting | State::Running | State::PartlyDone)
    }

    /// It counts as a step or a session that is done.
    fn counts_done(self) -> bool {
        matches!(self, State::Done | State::NullAnswer | State::Replayed)
    }
}

/// How a step runs: the words of the flow builder's chain, and the few
/// characters the tree has room for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Shape {
    /// `once`, `⇉ 4 in parallel`, `↓ each · 2 votes`, `◆ pick a path`,
    /// `★ best of 3`, `↺ until quiet`.
    pub words: String,
    /// `⇉ 4`, `↓`, `◆`, `★ 3`, `↺`; empty for a step that runs once.
    pub short: String,
}

impl Shape {
    /// `items`: how many items a fan-out runs over, when known; `votes`:
    /// the votes of a verify block; `attempts`: a tournament's candidates.
    pub fn of(
        kind: StepKind,
        items: Option<usize>,
        votes: Option<usize>,
        attempts: Option<usize>,
    ) -> Shape {
        let (words, short) = match kind {
            StepKind::Single => ("once".to_string(), String::new()),
            StepKind::Fanout => match items.filter(|n| *n > 0) {
                Some(n) => (format!("⇉ {n} in parallel"), format!("⇉ {n}")),
                None => ("⇉ in parallel".into(), "⇉".into()),
            },
            StepKind::Pipeline => match votes {
                Some(v) => (format!("↓ each · {v} votes"), "↓".into()),
                None => ("↓ each".into(), "↓".into()),
            },
            StepKind::Route => ("◆ pick a path".into(), "◆".into()),
            StepKind::Tournament => match attempts {
                Some(n) => (format!("★ best of {n}"), format!("★ {n}")),
                None => ("★ best of".into(), "★".into()),
            },
            StepKind::Until => ("↺ until quiet".into(), "↺".into()),
        };
        Shape { words, short }
    }

    /// The shape of a parsed step; `items` is the item count only a run
    /// knows (a list written in the document wins).
    pub fn of_step(step: &Step, items: Option<usize>) -> Shape {
        let items = inline_items(step).or(items);
        Shape::of(
            step.kind,
            items,
            step.verify.as_ref().map(|v| v.votes),
            step.n.or(items),
        )
    }
}

/// Who runs a row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Who {
    pub harness: String,
    /// The row names no harness of its own: this is the run's.
    pub inherited: bool,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Who {
    fn of(runner: &Runner, actual: Option<&str>, run_harness: &str) -> Who {
        Who {
            harness: actual
                .or(runner.harness.as_deref())
                .unwrap_or(run_harness)
                .to_string(),
            inherited: runner.harness.is_none(),
            agent: runner.agent.clone(),
            model: runner.model.clone(),
            effort: runner.effort.clone(),
        }
    }

    /// `claude·skeptic`, `codex`.
    pub fn short(&self) -> String {
        match &self.agent {
            Some(a) => format!("{}·{a}", self.harness),
            None => self.harness.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Run,
    Step,
    Session,
}

/// One row of the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub kind: RowKind,
    /// 0 the run, 1 a step, 2 a session.
    pub depth: u8,
    pub state: State,
    /// The step's number in the document, from 1; `None` on the run row.
    pub number: Option<usize>,
    /// The step's id; empty on the run row.
    pub step_id: String,
    /// What the row is called: the flow's name, the step's id, or a
    /// session's place in its step (`[1]`, `[1]/vote1`, `judge1.1`).
    pub label: String,
    /// The session's label in the journal and the store (`find[1]`): on a
    /// session row, and on a step row that is its one session.
    pub session: Option<String>,
    /// Step rows only.
    pub shape: Option<Shape>,
    pub who: Who,
    /// The run row: steps done over steps that run. A step row: sessions
    /// ended over sessions planned. A session row: 1 or 0 over 1.
    pub done: usize,
    pub planned: usize,
    /// Sessions running now.
    pub running: usize,
    pub tokens: u64,
    pub cost_usd: f64,
    pub started_ns: Option<i64>,
    pub ended_ns: Option<i64>,
    /// The trace store's launch: on a session row, and on a step row that
    /// is its one session. A replayed session keeps the launch that
    /// answered it in the earlier run.
    pub launch_id: Option<String>,
    /// Why a session answered nothing.
    pub null_reason: Option<String>,
}

impl Row {
    /// A step row with session rows under it shows `done/planned`.
    pub fn counts(&self) -> Option<(usize, usize)> {
        (self.kind == RowKind::Step && self.planned > 1).then_some((self.done, self.planned))
    }
}

/// The run's totals, for the line under the board.
#[derive(Debug, Clone, PartialEq)]
pub struct Totals {
    pub steps_done: usize,
    /// Steps that run: every step but the branches a route skipped.
    pub steps_total: usize,
    pub sessions_running: usize,
    pub sessions_done: usize,
    pub sessions_planned: usize,
    pub tokens: u64,
    pub budget_tokens: Option<u64>,
    pub cost_usd: f64,
    pub usd_cap: Option<f64>,
}

impl Totals {
    /// `41k of 200k tokens · $0.38 of $2.00 · 2 running`.
    pub fn line(&self) -> String {
        let mut parts = vec![match self.budget_tokens {
            Some(b) => format!(
                "{} of {} tokens",
                tokens_short(self.tokens),
                tokens_short(b)
            ),
            None => format!("{} tokens", tokens_short(self.tokens)),
        }];
        parts.push(match self.usd_cap {
            Some(cap) => format!("${:.2} of ${cap:.2}", self.cost_usd),
            None => format!("${:.2}", self.cost_usd),
        });
        if self.sessions_running > 0 {
            parts.push(format!("{} running", self.sessions_running));
        }
        parts.join(" · ")
    }
}

/// `950`, `1.5k`, `41k`, `1.2M`.
pub fn tokens_short(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 if n % 1000 >= 100 => format!("{:.1}k", (n / 100) as f64 / 10.0),
        1_000..=999_999 => format!("{}k", n / 1000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// How the run stands; the words are `workflow_runs.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Finished,
    Failed,
    Cancelled,
    BudgetExhausted,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Finished => "finished",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
            Status::BudgetExhausted => "budget-exhausted",
        }
    }

    /// An unknown word reads as failed: the run is not going on.
    pub fn from_label(s: &str) -> Status {
        match s {
            "running" => Status::Running,
            "finished" => Status::Finished,
            "cancelled" => Status::Cancelled,
            "budget-exhausted" => Status::BudgetExhausted,
            _ => Status::Failed,
        }
    }

    fn of(status: &RunStatus) -> Status {
        match status {
            RunStatus::Running => Status::Running,
            RunStatus::Finished(_) => Status::Finished,
            RunStatus::Failed(_) => Status::Failed,
            RunStatus::Cancelled => Status::Cancelled,
            RunStatus::BudgetExhausted(_) => Status::BudgetExhausted,
        }
    }

    fn state(self) -> State {
        match self {
            Status::Running => State::Running,
            Status::Finished => State::Done,
            Status::Failed | Status::Cancelled => State::Failed,
            Status::BudgetExhausted => State::PartlyDone,
        }
    }
}

/// The tree of one run.
#[derive(Debug, Clone, PartialEq)]
pub struct RunTree {
    pub run_id: String,
    pub status: Status,
    /// The run row, then every step in document order, each followed by
    /// its session rows when it has several.
    pub rows: Vec<Row>,
    pub totals: Totals,
}

impl RunTree {
    pub fn run(&self) -> &Row {
        &self.rows[0]
    }

    pub fn steps(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter().filter(|r| r.kind == RowKind::Step)
    }

    /// The session rows under a step.
    pub fn sessions_of<'a>(&'a self, step_id: &'a str) -> impl Iterator<Item = &'a Row> {
        self.rows
            .iter()
            .filter(move |r| r.kind == RowKind::Session && r.step_id == step_id)
    }
}

// ---- inputs ---------------------------------------------------------------

/// One session of a live run, as the App holds it.
#[derive(Debug, Clone)]
pub struct LiveSession<'a> {
    pub spec: &'a SessionSpec,
    /// The harness it was started on.
    pub harness: &'a str,
    /// Its process has ended; the interpreter may not have its answer yet.
    pub exited: bool,
    pub launch_id: Option<&'a str>,
    pub started_ns: Option<i64>,
    pub ended_ns: Option<i64>,
}

/// A run in memory.
pub struct LiveRun<'a> {
    pub run_id: &'a str,
    pub name: &'a str,
    pub state: &'a RunState,
    /// The run's own harness, which a step that names none runs on.
    pub harness: &'a str,
    /// Every session started so far, running or ended.
    pub sessions: &'a [LiveSession<'a>],
    /// The run's `journal.jsonl`, for the launch of a replayed session;
    /// may be empty.
    pub journal: &'a [Entry],
    pub usd_cap: Option<f64>,
    pub started_ns: Option<i64>,
}

/// A run in the store.
pub struct StoredRun<'a> {
    pub run: &'a WorkflowRun,
    /// Its `workflow_steps` rows.
    pub steps: &'a [WorkflowStep],
    /// The run's `journal.jsonl`: a resumed run's replayed sessions have
    /// no row of their own and are read from here, as is the reason of a
    /// null answer. It also holds the copied entries the run never
    /// reached, which are left out. May be empty.
    pub journal: &'a [Entry],
    /// The store keeps no dollar cap; the caller passes it when it knows.
    pub usd_cap: Option<f64>,
}

// ---- the common model -----------------------------------------------------

#[derive(Debug, Clone)]
struct Sess {
    key: SessionKey,
    label: String,
    state: State,
    /// The harness it ran on, when a session was started for it.
    harness: Option<String>,
    /// Who it ran as, when the run says; else the document does.
    runner: Option<Runner>,
    exited: bool,
    tokens: u64,
    cost_usd: f64,
    started_ns: Option<i64>,
    ended_ns: Option<i64>,
    launch_id: Option<String>,
    /// What it answered; `None` while it runs and for a null answer.
    answer: Option<Value>,
    null_reason: Option<String>,
}

struct StepFacts {
    id: String,
    phase: StepPhase,
    items: Option<usize>,
    sessions: Vec<Sess>,
}

struct Facts<'a> {
    run_id: String,
    name: String,
    harness: String,
    status: Status,
    doc: Option<&'a Workflow>,
    steps: Vec<StepFacts>,
    tokens: u64,
    cost_usd: f64,
    budget_tokens: Option<u64>,
    usd_cap: Option<f64>,
    started_ns: Option<i64>,
    ended_ns: Option<i64>,
}

fn inline_items(step: &Step) -> Option<usize> {
    match &step.over {
        Some(Over::Inline(v)) => Some(v.len()),
        _ => None,
    }
}

/// What a session of `role` runs on, as the interpreter decides it: a
/// refuter or a judge takes its own runner over the step's, and its own
/// agent or none.
fn runner_for(step: &Step, role: &Role) -> Runner {
    let base = Workflow::runner_of(step);
    let own = match role {
        Role::Vote(_) => step.verify.as_ref().map(|v| &v.runner),
        Role::Judge { .. } => Some(&step.judge_runner),
        Role::Main | Role::Generate(_) => None,
    };
    match own {
        Some(own) => {
            let mut r = own.over(&base);
            r.agent = own.agent.clone();
            r
        }
        None => base,
    }
}

/// A session's place in its step: `[1]`, `[1]/vote2`, `r1[0]`, `gen2`,
/// `judge1.2`.
fn short_label(key: &SessionKey) -> String {
    let round = if key.round > 0 {
        format!("r{}", key.round)
    } else {
        String::new()
    };
    match &key.role {
        Role::Main => format!("{round}[{}]", key.item),
        Role::Vote(v) => format!("{round}[{}]/vote{}", key.item, v + 1),
        Role::Generate(g) => format!("gen{}", g + 1),
        Role::Judge { round, pair } => format!("judge{}.{}", round + 1, pair + 1),
    }
}

/// Sessions in reading order: an item, then its votes; judges last.
fn order(key: &SessionKey) -> (u8, usize, usize, usize) {
    match &key.role {
        Role::Main => (0, key.round, key.item, 0),
        Role::Generate(g) => (0, key.round, *g, 0),
        Role::Vote(v) => (0, key.round, key.item, v + 1),
        Role::Judge { round, pair } => (1, *round, *pair, 0),
    }
}

/// The sessions a step plans, from the document, the sessions seen so
/// far and, where only the run knows, its item count.
fn planned(step: Option<&Step>, sessions: &[Sess], items: Option<usize>) -> usize {
    let seen = sessions.len();
    let Some(step) = step else { return seen };
    let votes = step.verify.as_ref().map_or(0, |v| v.votes);
    let items_seen = || sessions.iter().map(|s| s.key.item + 1).max();
    // one item's chain: its main session, then its votes unless the main
    // answer was null or the step's `keep` set it aside
    let chain = |item: usize, has_main: bool| -> usize {
        let dropped = sessions
            .iter()
            .find(|s| s.key.item == item && s.key.round == 0 && s.key.role == Role::Main)
            .filter(|m| m.state.ended())
            .is_some_and(|m| match &m.answer {
                None => true,
                Some(v) => step.keep.as_ref().is_some_and(|k| !k.eval(v)),
            });
        usize::from(has_main) + if dropped { 0 } else { votes }
    };
    let n = match step.kind {
        StepKind::Single | StepKind::Route => chain(0, true),
        StepKind::Fanout | StepKind::Pipeline => {
            let n = inline_items(step)
                .or(items)
                .or_else(items_seen)
                .unwrap_or(0);
            (0..n).map(|i| chain(i, step.actor.is_some())).sum()
        }
        StepKind::Tournament => {
            let generated = |s: &&Sess| matches!(s.key.role, Role::Generate(_));
            let generates = if step.over.is_none() || sessions.iter().any(|s| generated(&s)) {
                step.n.unwrap_or(0)
            } else {
                0
            };
            let judges = sessions
                .iter()
                .filter(|s| matches!(s.key.role, Role::Judge { .. }))
                .count();
            let candidates = if generates > 0 {
                let unanswered = sessions
                    .iter()
                    .filter(generated)
                    .filter(|s| s.state.ended() && s.answer.is_none())
                    .count();
                Some(generates.saturating_sub(unanswered))
            } else {
                items.or(inline_items(step))
            };
            // every judge sends one candidate home
            generates + candidates.map_or(judges, |c| c.saturating_sub(1).max(judges))
        }
        StepKind::Until => {
            let per_round = match &step.over {
                Some(_) => inline_items(step)
                    .or(items)
                    .or_else(items_seen)
                    .unwrap_or(1),
                None => 1,
            };
            let rounds = sessions.iter().map(|s| s.key.round + 1).max().unwrap_or(1);
            per_round.max(1) * rounds
        }
    };
    n.max(seen)
}

fn step_state(phase: StepPhase, sessions: &[Sess], status: Status) -> State {
    let any_ended = sessions.iter().any(|s| s.state.ended());
    match phase {
        StepPhase::Pending => State::Waiting,
        StepPhase::Skipped => State::Skipped,
        StepPhase::Done => {
            if sessions.is_empty() {
                State::Done
            } else if sessions.iter().all(|s| s.state == State::Replayed) {
                State::Replayed
            } else if sessions.iter().all(|s| s.answer.is_none()) {
                State::NullAnswer
            } else {
                State::Done
            }
        }
        StepPhase::Active => match status {
            Status::Failed | Status::Cancelled => State::Failed,
            _ if any_ended => State::PartlyDone,
            _ if sessions.iter().any(|s| s.state == State::Running) => State::Running,
            _ => State::Waiting,
        },
    }
}

fn assemble(f: Facts<'_>) -> RunTree {
    let mut rows: Vec<Row> = Vec::new();
    let mut totals = Totals {
        steps_done: 0,
        steps_total: 0,
        sessions_running: 0,
        sessions_done: 0,
        sessions_planned: 0,
        tokens: f.tokens,
        budget_tokens: f.budget_tokens,
        cost_usd: f.cost_usd,
        usd_cap: f.usd_cap,
    };
    for (n, sf) in f.steps.iter().enumerate() {
        let step = f.doc.and_then(|d| d.step(&sf.id));
        let mut sessions: Vec<&Sess> = sf.sessions.iter().collect();
        sessions.sort_by_key(|s| order(&s.key));
        let settled = matches!(sf.phase, StepPhase::Done | StepPhase::Skipped);
        let planned = if settled {
            sessions.len()
        } else {
            planned(step, &sf.sessions, sf.items)
        };
        let done = sessions.iter().filter(|s| s.state.ended()).count();
        let running = sessions
            .iter()
            .filter(|s| s.state == State::Running && !s.exited)
            .count();
        let state = step_state(sf.phase, &sf.sessions, f.status);
        if sf.phase != StepPhase::Skipped {
            totals.steps_total += 1;
        }
        if state.counts_done() {
            totals.steps_done += 1;
        }
        totals.sessions_running += running;
        totals.sessions_done += done;
        totals.sessions_planned += planned;

        // a step with one session has no child row: the step is the session
        let only = (sessions.len() == 1 && planned <= 1).then(|| sessions[0]);
        // who ran a session: what the run says, else the document's answer
        // for its role (a refuter and a judge have their own runner)
        let runner_of_session = |s: &Sess| {
            s.runner
                .clone()
                .or_else(|| step.map(|st| runner_for(st, &s.key.role)))
                .unwrap_or_default()
        };
        // the step row is its one session's, so it shows that session's
        // runner: the judge's when a tournament over two items runs one judge
        let runner = match only {
            Some(s) => runner_of_session(s),
            None => step.map(Workflow::runner_of).unwrap_or_default(),
        };
        let items = sf.items.or_else(|| {
            settled
                .then(|| sessions.iter().map(|s| s.key.item + 1).max())
                .flatten()
        });
        rows.push(Row {
            kind: RowKind::Step,
            depth: 1,
            state,
            number: Some(n + 1),
            step_id: sf.id.clone(),
            label: sf.id.clone(),
            session: only.map(|s| s.label.clone()),
            shape: step.map(|s| Shape::of_step(s, items)),
            who: Who::of(&runner, only.and_then(|s| s.harness.as_deref()), &f.harness),
            done,
            planned,
            running,
            tokens: sessions.iter().map(|s| s.tokens).sum(),
            cost_usd: sessions.iter().map(|s| s.cost_usd).sum(),
            started_ns: sessions.iter().filter_map(|s| s.started_ns).min(),
            ended_ns: (settled && sessions.iter().all(|s| s.state.ended()))
                .then(|| sessions.iter().filter_map(|s| s.ended_ns).max())
                .flatten(),
            launch_id: only.and_then(|s| s.launch_id.clone()),
            null_reason: only.and_then(|s| s.null_reason.clone()),
        });
        if only.is_some() {
            continue;
        }
        for s in sessions {
            let runner = runner_of_session(s);
            rows.push(Row {
                kind: RowKind::Session,
                depth: 2,
                state: s.state,
                number: Some(n + 1),
                step_id: sf.id.clone(),
                label: short_label(&s.key),
                session: Some(s.label.clone()),
                shape: None,
                who: Who::of(&runner, s.harness.as_deref(), &f.harness),
                done: usize::from(s.state.ended()),
                planned: 1,
                running: usize::from(s.state == State::Running && !s.exited),
                tokens: s.tokens,
                cost_usd: s.cost_usd,
                started_ns: s.started_ns,
                ended_ns: s.ended_ns,
                launch_id: s.launch_id.clone(),
                null_reason: s.null_reason.clone(),
            });
        }
    }
    rows.insert(
        0,
        Row {
            kind: RowKind::Run,
            depth: 0,
            state: f.status.state(),
            number: None,
            step_id: String::new(),
            label: f.name,
            session: None,
            shape: None,
            who: Who {
                harness: f.harness,
                ..Who::default()
            },
            done: totals.steps_done,
            planned: totals.steps_total,
            running: totals.sessions_running,
            tokens: totals.tokens,
            cost_usd: totals.cost_usd,
            started_ns: f.started_ns,
            ended_ns: f.ended_ns,
            launch_id: None,
            null_reason: None,
        },
    );
    RunTree {
        run_id: f.run_id,
        status: f.status,
        rows,
        totals,
    }
}

/// The steps of the document, then any step the sessions name that the
/// document does not (an unreadable document, an older run).
fn step_ids<'a>(doc: Option<&Workflow>, named: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut ids: Vec<String> = doc
        .map(|d| d.steps.iter().map(|s| s.id.clone()).collect())
        .unwrap_or_default();
    for id in named {
        if !ids.iter().any(|i| i == id) {
            ids.push(id.to_string());
        }
    }
    ids
}

fn group(ids: Vec<String>, sessions: Vec<Sess>) -> Vec<StepFacts> {
    let mut by_step: BTreeMap<String, Vec<Sess>> = BTreeMap::new();
    for s in sessions {
        by_step.entry(s.key.step.clone()).or_default().push(s);
    }
    ids.into_iter()
        .map(|id| StepFacts {
            sessions: by_step.remove(&id).unwrap_or_default(),
            id,
            phase: StepPhase::Pending,
            items: None,
        })
        .collect()
}

// ---- a live run -----------------------------------------------------------

/// The tree of a run in memory.
pub fn from_live(run: &LiveRun<'_>) -> RunTree {
    let state = run.state;
    let status = Status::of(&state.status());
    let live = |key: &SessionKey| run.sessions.iter().rev().find(|s| &s.spec.key == key);
    let overrides = |s: &LiveSession<'_>| Runner {
        harness: s.spec.overrides.harness.clone(),
        profile: s.spec.overrides.profile.clone(),
        model: s.spec.overrides.model.clone(),
        effort: s.spec.overrides.effort.clone(),
        agent: s.spec.overrides.agent.clone(),
    };
    let mut sessions: Vec<Sess> = Vec::new();
    // every session the interpreter has an answer for
    for rec in &state.records {
        let label = rec.key.label();
        let session = live(&rec.key);
        let replayed = state.was_replayed(&rec.key);
        let (answer, null_reason) = match &rec.outcome {
            Outcome::Null(why) => (None, Some(why.clone())),
            other => (Some(other.value()), None),
        };
        sessions.push(Sess {
            state: if replayed {
                State::Replayed
            } else if answer.is_none() {
                State::NullAnswer
            } else {
                State::Done
            },
            harness: session.map(|s| s.harness.to_string()),
            runner: session.map(overrides),
            exited: true,
            tokens: rec.tokens,
            cost_usd: rec.cost_usd,
            started_ns: session.and_then(|s| s.started_ns),
            ended_ns: session.and_then(|s| s.ended_ns),
            launch_id: match session {
                Some(s) => s.launch_id.map(str::to_string),
                None => run
                    .journal
                    .iter()
                    .find(|e| e.key == label)
                    .and_then(|e| e.launch_id.clone()),
            },
            answer,
            null_reason,
            key: rec.key.clone(),
            label,
        });
    }
    // and those still in flight
    for (i, s) in run.sessions.iter().enumerate() {
        let key = &s.spec.key;
        let later = run.sessions[i + 1..].iter().any(|o| &o.spec.key == key);
        if later || sessions.iter().any(|d| &d.key == key) {
            continue;
        }
        sessions.push(Sess {
            key: key.clone(),
            label: key.label(),
            state: if status == Status::Running {
                State::Running
            } else {
                State::Failed
            },
            harness: Some(s.harness.to_string()),
            runner: Some(overrides(s)),
            exited: s.exited,
            tokens: 0,
            cost_usd: 0.0,
            started_ns: s.started_ns,
            ended_ns: None,
            launch_id: s.launch_id.map(str::to_string),
            answer: None,
            null_reason: None,
        });
    }
    let ids = step_ids(Some(&state.doc), std::iter::empty());
    let mut steps = group(ids, sessions);
    for (i, sf) in steps.iter_mut().enumerate() {
        sf.phase = state.step_phase(i).unwrap_or(StepPhase::Pending);
        sf.items = state.step_items(i);
    }
    assemble(Facts {
        run_id: run.run_id.to_string(),
        name: run.name.to_string(),
        harness: run.harness.to_string(),
        status,
        doc: Some(&state.doc),
        steps,
        tokens: state.tokens_spent,
        cost_usd: state.cost_spent,
        budget_tokens: state.budget_tokens(),
        usd_cap: run.usd_cap,
        started_ns: run.started_ns,
        ended_ns: None,
    })
}

// ---- a stored run ---------------------------------------------------------

/// The tree of a run in the store. The document is parsed from the run's
/// row; when it no longer parses, the steps are those the rows name.
pub fn from_stored(stored: &StoredRun<'_>) -> RunTree {
    let run = stored.run;
    let status = Status::from_label(&run.status);
    let doc = super::document::parse(&run.document).ok();
    let journaled = |label: &str| stored.journal.iter().rev().find(|e| e.key == label);
    let mut sessions: Vec<Sess> = Vec::new();
    for row in stored.steps {
        let key = SessionKey::parse(&row.step_id, &row.session).unwrap_or(SessionKey {
            step: row.step_id.clone(),
            item: 0,
            round: 0,
            role: Role::Main,
        });
        let state = match row.kind.as_str() {
            "running" if status == Status::Running => State::Running,
            "running" => State::Failed,
            "null" => State::NullAnswer,
            _ => State::Done,
        };
        sessions.push(Sess {
            key,
            label: row.session.clone(),
            state,
            harness: Some(row.harness.clone()),
            runner: None,
            exited: state != State::Running,
            tokens: row.tokens.unwrap_or(0).max(0) as u64,
            cost_usd: row.cost_usd.unwrap_or(0.0),
            started_ns: row.started_ns,
            ended_ns: row.ended_ns,
            launch_id: row.launch_id.clone(),
            answer: (state == State::Done).then(|| row.result.clone()),
            null_reason: (state == State::NullAnswer)
                .then(|| journaled(&row.session).and_then(|e| e.reason.clone()))
                .flatten(),
        });
    }
    // a journaled session with no row of its own was copied from the run
    // this one resumed; only those the interpreter met again were replayed
    let mut copied: Vec<&Entry> = Vec::new();
    for e in stored.journal.iter().rev() {
        let has_row = sessions.iter().any(|s| s.label == e.key);
        if !has_row && !copied.iter().any(|c| c.key == e.key) {
            copied.push(e);
        }
    }
    copied.reverse();
    // the interpreter read again over what the run stored: which copied
    // entries it met, and how many items each step runs over
    let sim = doc
        .as_ref()
        .map(|d| resimulate(d, run, stored.steps, &copied));
    for e in replayed_entries(sim.as_ref(), run, status, stored.steps, copied) {
        let Some(key) = SessionKey::parse(&e.step, &e.key) else {
            continue;
        };
        let null = e.kind == "null";
        sessions.push(Sess {
            key,
            label: e.key.clone(),
            state: State::Replayed,
            harness: None,
            runner: None,
            exited: true,
            tokens: e.tokens,
            cost_usd: e.cost_usd,
            started_ns: None,
            ended_ns: None,
            launch_id: e.launch_id.clone(),
            answer: (!null).then(|| e.result.clone()),
            null_reason: null.then(|| e.outcome()).and_then(|o| match o {
                Outcome::Null(why) => Some(why),
                _ => None,
            }),
        });
    }
    let tokens: u64 = sessions.iter().map(|s| s.tokens).sum();
    let cost: f64 = sessions.iter().map(|s| s.cost_usd).sum();
    let ids = step_ids(doc.as_ref(), sessions.iter().map(|s| s.key.step.as_str()));
    let mut steps = group(ids, sessions);
    // the document's steps come first and in its order, as the
    // interpreter's do
    if let Some(sim) = &sim {
        for (i, sf) in steps.iter_mut().enumerate() {
            sf.items = sim.step_items(i);
        }
    }
    let phases = infer_phases(doc.as_ref(), &steps, status);
    for (sf, phase) in steps.iter_mut().zip(phases) {
        sf.phase = phase;
    }
    assemble(Facts {
        run_id: run.id.clone(),
        name: run.workflow.clone(),
        harness: run.harness.clone(),
        status,
        doc: doc.as_ref(),
        steps,
        // the run's row is written at its start and its end; between the
        // two the sessions' own rows are ahead of it
        tokens: tokens.max(run.tokens.unwrap_or(0).max(0) as u64),
        cost_usd: cost.max(run.cost_usd.unwrap_or(0.0)),
        budget_tokens: run.budget_tokens.map(|b| b.max(0) as u64),
        usd_cap: stored.usd_cap,
        started_ns: Some(run.started_ns),
        ended_ns: run.ended_ns,
    })
}

/// The interpreter run again over what a stored run left: the copied
/// journal entries as its replay, and the answers of the run's own rows
/// fed back as they are asked for. No budget and no session limit: the
/// stored rows already say how far the run got.
///
/// It answers what the store does not keep: which copied entries the
/// run met (`replayed_entries`), and how many items a step runs over
/// when its items come from the arguments or an earlier step
/// (`RunState::step_items`), so a fan-out with one answer of four reads
/// 1/4 here as it does live.
fn resimulate(
    doc: &Workflow,
    run: &WorkflowRun,
    rows: &[WorkflowStep],
    copied: &[&Entry],
) -> RunState {
    let owned: Vec<Entry> = copied.iter().map(|e| (*e).clone()).collect();
    let caps = Caps {
        max_sessions: usize::MAX,
        budget_tokens: None,
    };
    let mut sim = RunState::new(doc.clone(), run.args.clone(), caps)
        .with_replay(super::journal::to_replay(&owned));
    // every pass answers at least one row, or is the last
    for _ in 0..=rows.len() {
        let mut fed = false;
        for spec in sim.next(usize::MAX) {
            let label = spec.label();
            let Some(row) = rows
                .iter()
                .filter(|r| r.kind != "running")
                .find(|r| r.session == label)
            else {
                continue;
            };
            let outcome = match row.kind.as_str() {
                "text" => Outcome::Text(row.result.as_str().unwrap_or_default().into()),
                "object" => Outcome::Object(row.result.clone()),
                _ => Outcome::Null("no answer".into()),
            };
            sim.complete(
                &spec.key,
                SessionDone {
                    outcome,
                    tokens: 0,
                    cost_usd: 0.0,
                },
            );
            fed = true;
        }
        if !fed {
            break;
        }
    }
    sim
}

/// The copied journal entries the run replayed, in the order it met them.
///
/// A resume copies every entry it may replay into the new run's journal
/// before the interpreter starts, and nothing records which of them the
/// interpreter then consumed: a run that stops early (its budget spent by
/// the replays themselves, a cancel, a failure) leaves the rest unread.
/// So this asks the run read again (`resimulate`), which gives the
/// entries it meets and their order. When the run has ended,
/// `workflow_runs.sessions` says how many sessions it answered, and the
/// entries are cut to that count. What is still an estimate: a run that
/// has not ended (the count is written only at its end), where an entry
/// behind a spent budget reads as replayed until the run's last row is
/// written; and a budget spent in the middle of a step that also had
/// sessions of its own, where the count is right and the entries are the
/// first the interpreter would meet. Without a document the journal's
/// own order stands in.
fn replayed_entries<'a>(
    sim: Option<&RunState>,
    run: &WorkflowRun,
    status: Status,
    rows: &[WorkflowStep],
    copied: Vec<&'a Entry>,
) -> Vec<&'a Entry> {
    if copied.is_empty() {
        return copied;
    }
    let mut met = match sim {
        None => copied,
        Some(sim) => sim
            .records
            .iter()
            .filter(|r| sim.was_replayed(&r.key))
            .filter_map(|r| {
                let label = r.key.label();
                copied.iter().copied().find(|e| e.key == label)
            })
            .collect(),
    };
    if status != Status::Running {
        let own = rows.iter().filter(|r| r.kind != "running").count();
        met.truncate((run.sessions.max(0) as usize).saturating_sub(own));
    }
    met
}

/// The phase of every step, which the store does not keep.
fn infer_phases(doc: Option<&Workflow>, steps: &[StepFacts], status: Status) -> Vec<StepPhase> {
    let mut memo: Vec<Option<StepPhase>> = vec![None; steps.len()];
    for i in 0..steps.len() {
        infer_phase(doc, steps, status, i, &mut memo, 0);
    }
    memo.into_iter()
        .map(|p| p.unwrap_or(StepPhase::Pending))
        .collect()
}

fn infer_phase(
    doc: Option<&Workflow>,
    steps: &[StepFacts],
    status: Status,
    i: usize,
    memo: &mut Vec<Option<StepPhase>>,
    depth: usize,
) -> StepPhase {
    if let Some(p) = memo[i] {
        return p;
    }
    let sf = &steps[i];
    let step = doc.and_then(|d| d.step(&sf.id));
    let phase = (|| {
        // a branch of a route: skipped unless the route chose it
        if depth < steps.len()
            && let Some((owner, label)) = doc.and_then(|d| d.branch_owner(&sf.id))
            && let Some(j) = steps.iter().position(|s| s.id == owner)
            && j != i
        {
            match infer_phase(doc, steps, status, j, memo, depth + 1) {
                StepPhase::Skipped => return StepPhase::Skipped,
                StepPhase::Done => {
                    let chosen = steps[j]
                        .sessions
                        .iter()
                        .find(|s| s.key.role == Role::Main)
                        .and_then(|s| s.answer.as_ref())
                        .and_then(|v| v.get("label"))
                        .and_then(Value::as_str);
                    if chosen != Some(label) {
                        return StepPhase::Skipped;
                    }
                }
                StepPhase::Pending | StepPhase::Active => {}
            }
        }
        if status == Status::Finished {
            return StepPhase::Done;
        }
        if sf.sessions.is_empty() {
            return StepPhase::Pending;
        }
        let ended = sf.sessions.iter().filter(|s| s.state.ended()).count();
        // an `until` step may open another round after a full one
        let open_ended = step.is_some_and(|s| s.kind == StepKind::Until);
        if !open_ended
            && ended == sf.sessions.len()
            && ended >= planned(step, &sf.sessions, sf.items)
        {
            StepPhase::Done
        } else {
            StepPhase::Active
        }
    })();
    memo[i] = Some(phase);
    phase
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::document::parse;

    /// What the App keeps of a run, driven here by a fake: the
    /// interpreter, its live sessions, and the rows and journal the App
    /// would have written.
    struct Sim {
        text: String,
        args: Value,
        state: RunState,
        /// (spec, harness, exited, launch, started, ended)
        live: Vec<(SessionSpec, String, bool, String, i64, Option<i64>)>,
        rows: Vec<WorkflowStep>,
        journal: Vec<Entry>,
        clock: i64,
    }

    impl Sim {
        fn new(text: &str) -> Sim {
            Sim::with_args(text, serde_json::json!({}))
        }

        /// A run started with arguments.
        fn with_args(text: &str, args: Value) -> Sim {
            let doc = parse(text).unwrap();
            let caps = Caps {
                max_sessions: 1000,
                budget_tokens: doc.budget_tokens,
            };
            Sim {
                text: text.to_string(),
                args: args.clone(),
                state: RunState::new(doc, args, caps),
                live: Vec::new(),
                rows: Vec::new(),
                journal: Vec::new(),
                clock: 100,
            }
        }

        /// A run that resumes `earlier`: its journal is replayed.
        fn resuming(text: &str, earlier: &Sim) -> Sim {
            let mut sim = Sim::new(text);
            sim.journal = earlier.journal.clone();
            let doc = parse(text).unwrap();
            let caps = Caps {
                max_sessions: 1000,
                budget_tokens: doc.budget_tokens,
            };
            sim.state = RunState::new(doc, serde_json::json!({}), caps)
                .with_replay(crate::workflows::journal::to_replay(&earlier.journal));
            sim
        }

        fn harness_of(spec: &SessionSpec) -> String {
            spec.overrides
                .harness
                .clone()
                .unwrap_or_else(|| "claude".into())
        }

        /// Starts up to `slots` sessions, as the App's pump does.
        fn start(&mut self, slots: usize) -> Vec<SessionSpec> {
            let specs = self.state.next(slots);
            for spec in &specs {
                self.clock += 1;
                let harness = Self::harness_of(spec);
                let launch = format!("launch-{}", spec.label());
                self.rows.push(WorkflowStep {
                    run_id: "run-1".into(),
                    session: spec.label(),
                    step_id: spec.key.step.clone(),
                    item: spec.item.clone(),
                    launch_id: Some(launch.clone()),
                    phase: spec.phase.clone(),
                    harness: harness.clone(),
                    kind: "running".into(),
                    started_ns: Some(self.clock),
                    ended_ns: None,
                    tokens: None,
                    cost_usd: None,
                    worktree: None,
                    changed_files: Value::Null,
                    result: Value::Null,
                });
                self.live
                    .push((spec.clone(), harness, false, launch, self.clock, None));
            }
            specs
        }

        fn finish(&mut self, spec: &SessionSpec, outcome: Outcome) {
            self.clock += 1;
            let label = spec.label();
            let l = self
                .live
                .iter_mut()
                .find(|l| l.0.key == spec.key)
                .expect("a started session");
            l.2 = true;
            l.5 = Some(self.clock);
            let row = self.rows.iter_mut().find(|r| r.session == label).unwrap();
            row.kind = outcome.kind().into();
            row.ended_ns = Some(self.clock);
            row.tokens = Some(1500);
            row.cost_usd = Some(0.25);
            row.result = outcome.value();
            self.journal.push(Entry::from_outcome(
                &label,
                &spec.key.step,
                &spec.phase,
                &outcome,
                row.launch_id.clone(),
                1500,
                0.25,
            ));
            self.state.complete(
                &spec.key,
                SessionDone {
                    outcome,
                    tokens: 1500,
                    cost_usd: 0.25,
                },
            );
        }

        /// Runs to the end; `answer` is each session's outcome.
        fn drive(&mut self, mut answer: impl FnMut(&SessionSpec) -> Outcome) {
            for _ in 0..1000 {
                let specs = self.start(4);
                if specs.is_empty() {
                    assert_eq!(self.state.in_flight(), 0, "stuck");
                    return;
                }
                for spec in specs {
                    let o = answer(&spec);
                    self.finish(&spec, o);
                }
            }
            panic!("the run did not end");
        }

        fn live_tree(&self) -> RunTree {
            let sessions: Vec<LiveSession<'_>> = self
                .live
                .iter()
                .map(
                    |(spec, harness, exited, launch, started, ended)| LiveSession {
                        spec,
                        harness,
                        exited: *exited,
                        launch_id: Some(launch),
                        started_ns: Some(*started),
                        ended_ns: *ended,
                    },
                )
                .collect();
            from_live(&LiveRun {
                run_id: "run-1",
                name: "flow",
                state: &self.state,
                harness: "claude",
                sessions: &sessions,
                journal: &self.journal,
                usd_cap: Some(2.0),
                started_ns: Some(100),
            })
        }

        /// The run's row as the App writes it while the run is in memory
        /// (`ended_ns` comes with the row of the finished run).
        fn stored_tree(&self) -> RunTree {
            let run = WorkflowRun {
                id: "run-1".into(),
                workflow: "flow".into(),
                source: "builtin".into(),
                document_hash: "h".into(),
                document: self.text.clone(),
                workspace: "/ws".into(),
                harness: "claude".into(),
                profile: "Claude Code".into(),
                args: self.args.clone(),
                budget_tokens: self.state.doc.budget_tokens.map(|b| b as i64),
                started_ns: 100,
                ended_ns: None,
                status: Status::of(&self.state.status()).label().into(),
                sessions: self.state.records.len() as i64,
                tokens: Some(self.state.tokens_spent as i64),
                cost_usd: Some(self.state.cost_spent),
                result: Value::Null,
                error: None,
                resumed_from: None,
            };
            from_stored(&StoredRun {
                run: &run,
                steps: &self.rows,
                journal: &self.journal,
                usd_cap: Some(2.0),
            })
        }

        /// Both trees, which must be the same.
        fn tree(&self) -> RunTree {
            let (live, stored) = (self.live_tree(), self.stored_tree());
            for (l, s) in live.rows.iter().zip(&stored.rows) {
                assert_eq!(l, s, "row {} {}", l.step_id, l.label);
            }
            assert_eq!(live, stored);
            live
        }
    }

    fn obj(v: Value) -> Outcome {
        Outcome::Object(v)
    }

    fn text(t: &str) -> Outcome {
        Outcome::Text(t.into())
    }

    /// `✓ 1 scope`, `  ✓ [0]`: glyph, number or indent, label.
    fn outline(tree: &RunTree) -> Vec<String> {
        tree.rows
            .iter()
            .skip(1)
            .map(|r| match r.kind {
                RowKind::Session => format!("  {} {}", r.state.glyph(), r.label),
                _ => format!("{} {} {}", r.state.glyph(), r.number.unwrap(), r.label),
            })
            .collect()
    }

    fn step<'a>(tree: &'a RunTree, id: &str) -> &'a Row {
        tree.steps().find(|r| r.step_id == id).unwrap()
    }

    const ONCE: &str = r#"
[workflow]
name = "flow"
description = "d"
budget_tokens = 200000
[[steps]]
id = "scope"
prompt = "p"
agent = "planner"
model = "opus"
effort = "high"
[[steps]]
id = "report"
prompt = "write {scope}"
harness = "codex"
"#;

    #[test]
    fn a_once_step_is_its_own_session_and_later_steps_wait() {
        let mut sim = Sim::new(ONCE);
        let before = sim.tree();
        assert_eq!(outline(&before), ["⋯ 1 scope", "⋯ 2 report"]);
        assert_eq!((before.run().done, before.run().planned), (0, 2));

        let specs = sim.start(4);
        assert_eq!(specs.len(), 1);
        let running = sim.tree();
        assert_eq!(outline(&running), ["▶ 1 scope", "⋯ 2 report"]);
        let scope = step(&running, "scope");
        assert_eq!(scope.shape.as_ref().unwrap().words, "once");
        assert_eq!(
            scope.who,
            Who {
                harness: "claude".into(),
                inherited: true,
                agent: Some("planner".into()),
                model: Some("opus".into()),
                effort: Some("high".into()),
            }
        );
        assert_eq!(scope.who.short(), "claude·planner");
        assert_eq!(scope.session.as_deref(), Some("scope"));
        assert_eq!(scope.launch_id.as_deref(), Some("launch-scope"));
        assert_eq!((scope.done, scope.planned, scope.running), (0, 1, 1));
        assert_eq!(scope.counts(), None, "one session: no done/planned");
        assert_eq!(scope.started_ns, Some(101));
        assert_eq!(running.totals.sessions_running, 1);
        assert_eq!(running.run().state, State::Running);

        sim.finish(&specs[0], text("the scope"));
        sim.drive(|_| text("the report"));
        let done = sim.tree();
        assert_eq!(outline(&done), ["✓ 1 scope", "✓ 2 report"]);
        assert_eq!(done.status, Status::Finished);
        assert_eq!(done.run().state, State::Done);
        let report = step(&done, "report");
        assert_eq!(report.who.harness, "codex");
        assert!(!report.who.inherited);
        assert_eq!((report.tokens, report.cost_usd), (1500, 0.25));
        assert!(report.ended_ns > report.started_ns);
        assert_eq!(
            done.totals,
            Totals {
                steps_done: 2,
                steps_total: 2,
                sessions_running: 0,
                sessions_done: 2,
                sessions_planned: 2,
                tokens: 3000,
                budget_tokens: Some(200_000),
                cost_usd: 0.5,
                usd_cap: Some(2.0),
            }
        );
        assert_eq!(done.totals.line(), "3k of 200k tokens · $0.50 of $2.00");
    }

    #[test]
    fn a_null_answer_shows_its_reason() {
        let mut sim = Sim::new(ONCE);
        sim.drive(|spec| match spec.key.step.as_str() {
            "scope" => Outcome::Null("timed out".into()),
            _ => text("r"),
        });
        let tree = sim.tree();
        assert_eq!(outline(&tree), ["∅ 1 scope", "✓ 2 report"]);
        assert_eq!(
            step(&tree, "scope").null_reason.as_deref(),
            Some("timed out")
        );
        assert_eq!(tree.run().done, 2);
    }

    const FANOUT: &str = r#"
[workflow]
name = "flow"
description = "d"
output = "find"
[[steps]]
id = "find"
kind = "fanout"
over = ["a", "b", "c", "d"]
skill = "wf-find"
harness = "codex"
model = "gpt-5"
"#;

    #[test]
    fn a_parallel_fan_out_has_a_row_per_item() {
        let mut sim = Sim::new(FANOUT);
        let before = sim.tree();
        let find = step(&before, "find");
        assert_eq!(find.shape.as_ref().unwrap().words, "⇉ 4 in parallel");
        assert_eq!(find.shape.as_ref().unwrap().short, "⇉ 4");
        assert_eq!(find.counts(), Some((0, 4)), "planned before it starts");

        let specs = sim.start(2);
        assert_eq!(outline(&sim.tree()), ["▶ 1 find", "  ▶ [0]", "  ▶ [1]"]);
        sim.finish(&specs[0], text("x"));
        let partly = sim.tree();
        assert_eq!(outline(&partly), ["◷ 1 find", "  ✓ [0]", "  ▶ [1]"]);
        let find = step(&partly, "find");
        assert_eq!((find.done, find.planned, find.running), (1, 4, 1));
        assert_eq!(find.launch_id, None, "several sessions: none is the step's");
        assert_eq!(find.ended_ns, None);
        let item = partly.sessions_of("find").nth(1).unwrap();
        assert_eq!(item.session.as_deref(), Some("find[1]"));
        assert_eq!(item.launch_id.as_deref(), Some("launch-find[1]"));
        assert_eq!(item.who.short(), "codex");
        assert_eq!(item.who.model.as_deref(), Some("gpt-5"));
        assert_eq!(
            partly.totals.line(),
            "1.5k tokens · $0.25 of $2.00 · 1 running"
        );

        sim.finish(&specs[1], Outcome::Null("no block".into()));
        sim.drive(|_| text("x"));
        let done = sim.tree();
        assert_eq!(
            outline(&done),
            ["✓ 1 find", "  ✓ [0]", "  ∅ [1]", "  ✓ [2]", "  ✓ [3]"]
        );
        assert_eq!(step(&done, "find").counts(), Some((4, 4)));
        assert_eq!(
            done.sessions_of("find")
                .nth(1)
                .unwrap()
                .null_reason
                .as_deref(),
            Some("no block")
        );
    }

    const FROM_ARGS: &str = r#"
[workflow]
name = "flow"
description = "d"
output = "find"
[args.items]
description = "what to look at"
[[steps]]
id = "find"
kind = "fanout"
over = "args.items"
prompt = "look at {{item}}"
"#;

    #[test]
    fn a_fan_out_over_the_arguments_counts_every_item_with_one_slot() {
        // four items and one slot: one answer is 1 of 4, not the whole step
        let mut sim = Sim::with_args(
            FROM_ARGS,
            serde_json::json!({"items": ["a", "b", "c", "d"]}),
        );
        let first = sim.start(1);
        assert_eq!(first.len(), 1);
        let tree = sim.tree();
        assert_eq!(step(&tree, "find").counts(), Some((0, 4)));
        assert_eq!(step(&tree, "find").state, State::Running);

        sim.finish(&first[0], text("x"));
        let tree = sim.tree();
        let find = step(&tree, "find");
        assert_eq!(
            find.counts(),
            Some((1, 4)),
            "three items are still to start"
        );
        assert_eq!(find.state, State::PartlyDone);
        assert_eq!((tree.run().done, tree.run().planned), (0, 1));

        sim.drive(|_| text("x"));
        let tree = sim.tree();
        assert_eq!(step(&tree, "find").counts(), Some((4, 4)));
        assert_eq!(step(&tree, "find").state, State::Done);
    }

    const FROM_STEP: &str = r#"
[workflow]
name = "flow"
description = "d"
output = "find"
[[steps]]
id = "list"
prompt = "list them"
[[steps]]
id = "find"
kind = "fanout"
over = "list.items"
prompt = "look at {{item}}"
"#;

    #[test]
    fn a_fan_out_over_an_earlier_answer_counts_every_item_with_one_slot() {
        let mut sim = Sim::new(FROM_STEP);
        let list = sim.start(1);
        sim.finish(&list[0], obj(serde_json::json!({"items": ["a", "b", "c"]})));
        let first = sim.start(1);
        assert_eq!(first[0].key.step, "find");
        sim.finish(&first[0], text("x"));
        let tree = sim.tree();
        let find = step(&tree, "find");
        assert_eq!(find.counts(), Some((1, 3)));
        assert_eq!(find.state, State::PartlyDone);
    }

    const CHECK: &str = r#"
[workflow]
name = "flow"
description = "d"
output = "confirmed"
[schemas.verdict]
fields.refuted = { type = "boolean", required = true }
[[steps]]
id = "confirmed"
kind = "pipeline"
over = ["one", "two"]
prompt = "look at {item}"
agent = "reviewer"
verify = { skill = "wf-refute", votes = 2, result = "verdict", keep = "refuted < 1", agent = "skeptic", harness = "codex" }
"#;

    #[test]
    fn each_then_check_lists_an_item_and_then_its_votes() {
        let mut sim = Sim::new(CHECK);
        let before = sim.tree();
        let confirmed = step(&before, "confirmed");
        assert_eq!(confirmed.shape.as_ref().unwrap().words, "↓ each · 2 votes");
        assert_eq!(
            confirmed.counts(),
            Some((0, 6)),
            "2 items, each with 2 votes"
        );

        // the first item answers nothing: its votes are not planned any more
        let specs = sim.start(4);
        assert_eq!(specs.len(), 2);
        sim.finish(&specs[0], Outcome::Null("empty".into()));
        let partly = sim.tree();
        assert_eq!(step(&partly, "confirmed").counts(), Some((1, 4)));
        sim.finish(&specs[1], text("a finding"));
        sim.drive(|spec| obj(serde_json::json!({ "refuted": spec.key.role == Role::Vote(1) })));

        let done = sim.tree();
        assert_eq!(
            outline(&done),
            [
                "✓ 1 confirmed",
                "  ∅ [0]",
                "  ✓ [1]",
                "  ✓ [1]/vote1",
                "  ✓ [1]/vote2"
            ]
        );
        let rows: Vec<&Row> = done.sessions_of("confirmed").collect();
        assert_eq!(rows[1].who.short(), "claude·reviewer");
        assert!(rows[1].who.inherited);
        // a refuter is its own role: its own harness and agent
        assert_eq!(rows[2].who.short(), "codex·skeptic");
        assert!(!rows[2].who.inherited);
        assert_eq!(rows[2].session.as_deref(), Some("confirmed[1]/vote1"));
        assert_eq!(step(&done, "confirmed").counts(), Some((4, 4)));
        assert_eq!(step(&done, "confirmed").tokens, 6000);
    }

    const BEST: &str = r#"
[workflow]
name = "flow"
description = "d"
output = "rank"
[[steps]]
id = "rank"
kind = "tournament"
n = 3
prompt = "propose"
harness = "codex"
judge = { skill = "wf-judge", agent = "judge", harness = "claude" }
"#;

    #[test]
    fn best_of_lists_the_attempts_and_then_the_judges() {
        let mut sim = Sim::new(BEST);
        let before = sim.tree();
        let rank = step(&before, "rank");
        assert_eq!(rank.shape.as_ref().unwrap().words, "★ best of 3");
        assert_eq!(rank.shape.as_ref().unwrap().short, "★ 3");
        assert_eq!(rank.counts(), Some((0, 5)), "3 attempts and 2 judges");

        sim.drive(|spec| match spec.key.role {
            Role::Generate(g) => text(&format!("candidate {g}")),
            _ => obj(serde_json::json!({ "winner": "b" })),
        });
        let done = sim.tree();
        assert_eq!(
            outline(&done),
            [
                "✓ 1 rank",
                "  ✓ gen1",
                "  ✓ gen2",
                "  ✓ gen3",
                "  ✓ judge1.1",
                "  ✓ judge2.1"
            ]
        );
        let rows: Vec<&Row> = done.sessions_of("rank").collect();
        assert_eq!(rows[0].who.short(), "codex");
        assert_eq!(rows[3].who.short(), "claude·judge");
        assert_eq!(rows[4].session.as_deref(), Some("rank/r1/judge2.1"));
        assert_eq!(step(&done, "rank").counts(), Some((5, 5)));
    }

    #[test]
    fn a_tournament_that_loses_an_attempt_plans_one_judge_less() {
        let mut sim = Sim::new(BEST);
        let specs = sim.start(4);
        sim.finish(&specs[0], Outcome::Null("empty".into()));
        assert_eq!(step(&sim.tree(), "rank").counts(), Some((1, 4)));
        sim.finish(&specs[1], text("b"));
        sim.finish(&specs[2], text("c"));
        // the one judge is now open: the run knows the candidates
        let judging = sim.live_tree();
        assert_eq!(step(&judging, "rank").counts(), Some((3, 4)));
        assert_eq!(sim.state.step_items(0), Some(2));
    }

    const ROUTE: &str = r#"
[workflow]
name = "flow"
description = "d"
output = "report"
[schemas.choice]
fields.label = { type = "string", required = true }
[[steps]]
id = "triage"
kind = "route"
prompt = "bug or feature?"
result = "choice"
branches = { bug = ["fix"], feature = ["design", "build"] }
[[steps]]
id = "fix"
prompt = "fix"
[[steps]]
id = "design"
prompt = "design"
[[steps]]
id = "build"
prompt = "build {design}"
[[steps]]
id = "report"
prompt = "report"
"#;

    #[test]
    fn a_route_shows_the_path_taken_and_skips_the_rest() {
        let mut sim = Sim::new(ROUTE);
        let before = sim.tree();
        assert_eq!(
            step(&before, "triage").shape.as_ref().unwrap().words,
            "◆ pick a path"
        );
        assert_eq!(before.run().planned, 5);

        // `report` reads no step, so it starts beside the route
        let specs = sim.start(4);
        let triage = specs.iter().find(|s| s.key.step == "triage").unwrap();
        sim.finish(triage, obj(serde_json::json!({ "label": "bug" })));
        let chosen = sim.tree();
        assert_eq!(step(&chosen, "design").state, State::Skipped);
        assert_eq!(step(&chosen, "build").state, State::Skipped);
        assert_eq!(step(&chosen, "fix").state, State::Waiting);
        assert_eq!(chosen.run().planned, 3, "the skipped branch does not count");

        for spec in specs.iter().filter(|s| s.key.step != "triage") {
            sim.finish(spec, text("ok"));
        }
        sim.drive(|_| text("ok"));
        let done = sim.tree();
        assert_eq!(
            outline(&done),
            [
                "✓ 1 triage",
                "✓ 2 fix",
                "– 3 design",
                "– 4 build",
                "✓ 5 report"
            ]
        );
        assert_eq!((done.run().done, done.run().planned), (3, 3));
        assert_eq!(done.totals.sessions_done, 3);
    }

    #[test]
    fn a_resumed_run_shows_what_the_journal_answered() {
        // the first run: one finder answers, the other does not, and the
        // run is stopped
        let mut first = Sim::new(RESUME);
        let specs = first.start(4);
        first.finish(&specs[0], text("found a"));
        first.state.cancel();
        let stopped = first.stored_tree();
        assert_eq!(stopped.status, Status::Running, "a session is in flight");

        let mut second = Sim::resuming(RESUME, &first);
        let specs = second.start(4);
        assert_eq!(specs.len(), 1, "find[0] is replayed, find[1] runs");
        let running = second.tree();
        assert_eq!(
            outline(&running),
            ["◷ 1 find", "  ↷ [0]", "  ▶ [1]", "⋯ 2 report"]
        );
        let replayed = running.sessions_of("find").next().unwrap();
        assert_eq!(replayed.state, State::Replayed);
        assert_eq!(
            replayed.launch_id.as_deref(),
            Some("launch-find"),
            "the launch that answered it in the earlier run"
        );
        assert_eq!(replayed.started_ns, None);
        assert_eq!(replayed.tokens, 1500);
        assert_eq!(replayed.who.short(), "codex·reviewer");
        assert_eq!(running.totals.tokens, 1500);

        second.finish(&specs[0], text("found b"));
        second.drive(|_| text("the report"));
        let done = second.tree();
        assert_eq!(
            outline(&done),
            ["✓ 1 find", "  ↷ [0]", "  ✓ [1]", "✓ 2 report"]
        );
        assert_eq!(done.totals.tokens, 4500);

        // a third run replays everything the second journaled
        let mut third = Sim::resuming(RESUME, &second);
        assert!(third.start(4).is_empty());
        let all = third.tree();
        assert_eq!(
            outline(&all),
            ["↷ 1 find", "  ↷ [0]", "  ↷ [1]", "↷ 2 report"]
        );
        assert_eq!(all.status, Status::Finished);
        assert_eq!((all.run().done, all.run().planned), (2, 2));
        assert_eq!(
            step(&all, "report").launch_id.as_deref(),
            Some("launch-report")
        );
    }

    #[test]
    fn a_copied_entry_the_resumed_run_never_reached_is_not_replayed() {
        let mut first = Sim::new(ONCE);
        first.drive(|_| text("x"));
        assert_eq!(first.journal.len(), 2);

        // the same flow with a budget the first replayed step spends: the
        // second entry was copied into the journal and never consumed
        let tight = ONCE.replace("budget_tokens = 200000", "budget_tokens = 1000");
        let mut second = Sim::resuming(&tight, &first);
        assert!(second.start(4).is_empty());
        assert_eq!(second.state.replayed, 1);
        assert_eq!(second.journal.len(), 2, "both entries were copied");
        let tree = second.tree();
        assert_eq!(tree.status, Status::BudgetExhausted);
        assert_eq!(outline(&tree), ["↷ 1 scope", "⋯ 2 report"]);
        let report = step(&tree, "report");
        assert_eq!((report.tokens, report.cost_usd), (0, 0.0));
        assert_eq!(report.launch_id, None);
        assert_eq!((tree.totals.tokens, tree.totals.cost_usd), (1500, 0.25));
        assert_eq!((tree.run().done, tree.run().planned), (1, 2));
        assert_eq!(tree.totals.sessions_done, 1);
    }

    #[test]
    fn a_running_resumed_run_replays_only_what_it_has_reached() {
        // `report` reads `find`, whose second item is still running: its
        // copied entry waits
        let mut first = Sim::new(RESUME);
        first.drive(|_| text("x"));
        let mut again = Sim::resuming(RESUME, &first);
        again.journal.retain(|e| e.key != "find[1]");
        again.state = RunState::new(
            parse(RESUME).unwrap(),
            serde_json::json!({}),
            Caps::default(),
        )
        .with_replay(crate::workflows::journal::to_replay(&again.journal));
        assert_eq!(again.start(4).len(), 1);
        let tree = again.tree();
        assert_eq!(
            outline(&tree),
            ["◷ 1 find", "  ↷ [0]", "  ▶ [1]", "⋯ 2 report"]
        );
        assert_eq!(tree.totals.tokens, 1500);
    }

    const JUDGE: &str = r#"
[workflow]
name = "flow"
description = "d"
output = "rank"
[[steps]]
id = "rank"
kind = "tournament"
over = ["a", "b"]
harness = "codex"
agent = "reviewer"
model = "gpt-5"
judge = { prompt = "pick", agent = "judge", model = "opus", effort = "high" }
"#;

    #[test]
    fn a_step_that_is_its_one_judge_shows_the_judges_runner() {
        let mut sim = Sim::new(JUDGE);
        let specs = sim.start(4);
        assert_eq!(specs.len(), 1);
        assert!(matches!(specs[0].key.role, Role::Judge { .. }));
        let judge = Who {
            harness: "codex".into(),
            inherited: false,
            agent: Some("judge".into()),
            model: Some("opus".into()),
            effort: Some("high".into()),
        };
        let running = sim.tree();
        assert_eq!(outline(&running), ["▶ 1 rank"]);
        assert_eq!(step(&running, "rank").who, judge);
        sim.finish(&specs[0], obj(serde_json::json!({ "winner": "a" })));
        let done = sim.tree();
        assert_eq!(outline(&done), ["✓ 1 rank"]);
        assert_eq!(step(&done, "rank").who, judge);
        assert_eq!(
            step(&done, "rank").session.as_deref(),
            Some("rank/judge1.1")
        );
    }

    const RESUME: &str = r#"
[workflow]
name = "flow"
description = "d"
output = "report"
[[steps]]
id = "find"
kind = "fanout"
over = ["a", "b"]
skill = "wf-find"
agent = "reviewer"
harness = "codex"
[[steps]]
id = "report"
prompt = "report {find}"
"#;

    #[test]
    fn a_failed_run_marks_the_step_it_stopped_in() {
        let mut sim = Sim::new(FANOUT);
        let specs = sim.start(2);
        sim.finish(&specs[0], text("x"));
        // the stored run was cancelled with a session that never answered
        let run = WorkflowRun {
            id: "run-1".into(),
            workflow: "flow".into(),
            source: "builtin".into(),
            document_hash: "h".into(),
            document: FANOUT.into(),
            workspace: "/ws".into(),
            harness: "claude".into(),
            profile: "Claude Code".into(),
            args: Value::Null,
            budget_tokens: None,
            started_ns: 100,
            ended_ns: Some(200),
            status: "cancelled".into(),
            sessions: 1,
            tokens: Some(1500),
            cost_usd: Some(0.25),
            result: Value::Null,
            error: None,
            resumed_from: None,
        };
        let tree = from_stored(&StoredRun {
            run: &run,
            steps: &sim.rows,
            journal: &[],
            usd_cap: None,
        });
        assert_eq!(outline(&tree), ["✗ 1 find", "  ✓ [0]", "  ✗ [1]"]);
        assert_eq!(tree.run().state, State::Failed);
        assert_eq!(tree.run().ended_ns, Some(200));
        assert_eq!(tree.totals.sessions_running, 0);
        assert_eq!(tree.totals.line(), "1.5k tokens · $0.25");
    }

    #[test]
    fn a_document_that_no_longer_parses_lists_the_steps_the_rows_name() {
        let mut sim = Sim::new(ONCE);
        sim.drive(|_| text("x"));
        sim.text = "not a workflow".into();
        let tree = sim.stored_tree();
        assert_eq!(outline(&tree), ["✓ 1 scope", "✓ 2 report"]);
        assert_eq!(step(&tree, "scope").shape, None);
        assert_eq!(step(&tree, "report").who.harness, "codex");
    }

    #[test]
    fn the_shapes_read_as_the_flow_builder_writes_them() {
        let words = |kind, items, votes, attempts| Shape::of(kind, items, votes, attempts).words;
        assert_eq!(words(StepKind::Single, None, None, None), "once");
        assert_eq!(words(StepKind::Fanout, None, None, None), "⇉ in parallel");
        assert_eq!(
            words(StepKind::Fanout, Some(0), None, None),
            "⇉ in parallel"
        );
        assert_eq!(words(StepKind::Pipeline, None, None, None), "↓ each");
        assert_eq!(words(StepKind::Until, None, None, None), "↺ until quiet");
        assert_eq!(tokens_short(999), "999");
        assert_eq!(tokens_short(1_000), "1k");
        assert_eq!(tokens_short(1_999), "1.9k");
        assert_eq!(tokens_short(41_900), "41k");
        assert_eq!(tokens_short(1_250_000), "1.2M");
    }
}
