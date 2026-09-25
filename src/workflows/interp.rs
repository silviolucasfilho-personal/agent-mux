//! The interpreter: a pure state machine over a workflow document. The
//! caller (the App's tick, the headless CLI, a test) asks `next()` for
//! sessions that may start, runs them however it likes, and reports each
//! one with `complete()`. Per-item chains (main session, then verify
//! votes) proceed independently; a step waits only for the steps it
//! references. Nothing here touches a harness, a file or a clock, which
//! is what makes journal replay exact.

use super::document::{
    Actor, Env, Isolation, Over, Step, StepKind, Workflow, interpolate, resolve,
};
use super::result::Outcome;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Limits the caller enforces through the interpreter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
    pub max_sessions: usize,
    pub budget_tokens: Option<u64>,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            max_sessions: 1000,
            budget_tokens: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Role {
    Main,
    Vote(usize),
    Generate(usize),
    Judge { round: usize, pair: usize },
}

/// Identifies one session of a run; stable across resumes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionKey {
    pub step: String,
    pub item: usize,
    pub round: usize,
    pub role: Role,
}

impl SessionKey {
    /// `find[2]`, `confirmed[0]/vote1`, `best/r1/judge2.1`, `audit/r2`.
    pub fn label(&self) -> String {
        let mut s = self.step.clone();
        if self.round > 0 {
            s.push_str(&format!("/r{}", self.round));
        }
        match &self.role {
            Role::Main => {
                if self.item > 0 {
                    s.push_str(&format!("[{}]", self.item));
                }
            }
            Role::Vote(i) => s.push_str(&format!("[{}]/vote{}", self.item, i + 1)),
            Role::Generate(i) => s.push_str(&format!("/gen{}", i + 1)),
            Role::Judge { round, pair } => s.push_str(&format!("/judge{}.{}", round + 1, pair + 1)),
        }
        s
    }

    /// The journal key: the label plus nothing else, since labels are unique.
    pub fn journal_key(&self) -> String {
        self.label()
    }
}

/// Per-step overrides the executor applies.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Overrides {
    pub harness: Option<String>,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub isolation: Option<Isolation>,
    pub cwd: Option<String>,
    pub timeout_s: Option<u64>,
}

/// One session to run.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSpec {
    pub key: SessionKey,
    pub actor: Actor,
    pub args: Value,
    pub item: Value,
    pub index: usize,
    pub inputs: Value,
    pub result_schema: Option<String>,
    pub phase: String,
    pub overrides: Overrides,
    /// `until` rounds: what earlier rounds already found.
    pub previous_seen: Option<Value>,
}

impl SessionSpec {
    pub fn label(&self) -> String {
        self.key.label()
    }
}

/// What the executor reports for a session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionDone {
    pub outcome: Outcome,
    pub tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus {
    Running,
    Finished(Value),
    Failed(String),
    Cancelled,
    /// The token ceiling was reached; the partial result of `output`.
    BudgetExhausted(Value),
}

/// A finished session, for the journal and the store.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub key: SessionKey,
    pub phase: String,
    pub outcome: Outcome,
    pub tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone)]
struct Slot {
    issued: bool,
    done: Option<Outcome>,
}

impl Slot {
    fn new() -> Self {
        Slot {
            issued: false,
            done: None,
        }
    }
    fn finished(&self) -> bool {
        self.done.is_some()
    }
}

#[derive(Debug, Clone)]
struct ItemWork {
    item: Value,
    index: usize,
    main: Option<Slot>,
    /// The value `keep`, `verify` and the result see: the main result, or
    /// the item itself when the step has no actor.
    subject: Option<Value>,
    votes: Vec<Slot>,
    aggregated: Option<Value>,
    dropped: Option<String>,
}

#[derive(Debug, Clone)]
enum Work {
    Single {
        item: ItemWork,
    },
    Items {
        items: Vec<ItemWork>,
        dedupe_dropped: usize,
    },
    Route {
        item: ItemWork,
        chosen: Option<String>,
    },
    Tournament {
        generate: Vec<Slot>,
        candidates: Vec<Value>,
        round: usize,
        /// Pairs of the current round and their judge slots.
        pairs: Vec<(usize, usize, Slot)>,
        bye: Option<usize>,
        winners: Vec<Value>,
    },
    Until {
        round: usize,
        rounds_without_new: usize,
        seen: Vec<Value>,
        keys: BTreeSet<String>,
        /// This round's sessions.
        current: Vec<ItemWork>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Pending,
    Active,
    Done,
    Skipped,
}

#[derive(Debug, Clone)]
struct StepState {
    phase: Phase,
    work: Option<Work>,
    result: Value,
    /// A finished route step's chosen branch.
    route_choice: Option<String>,
}

pub struct RunState {
    pub doc: Workflow,
    pub args: Value,
    caps: Caps,
    steps: Vec<StepState>,
    /// Sessions issued and not yet completed.
    in_flight: BTreeMap<SessionKey, (usize, String)>,
    pub records: Vec<SessionRecord>,
    pub notes: Vec<String>,
    pub sessions_started: usize,
    pub tokens_spent: u64,
    pub cost_spent: f64,
    cancelled: bool,
    failed: Option<String>,
    /// Journaled outcomes replayed instead of run.
    replay: BTreeMap<String, SessionDone>,
    pub replayed: usize,
}

impl RunState {
    pub fn new(doc: Workflow, args: Value, caps: Caps) -> RunState {
        let steps = doc
            .steps
            .iter()
            .map(|_| StepState {
                phase: Phase::Pending,
                work: None,
                result: Value::Null,
                route_choice: None,
            })
            .collect();
        RunState {
            doc,
            args,
            caps,
            steps,
            in_flight: BTreeMap::new(),
            records: Vec::new(),
            notes: Vec::new(),
            sessions_started: 0,
            tokens_spent: 0,
            cost_spent: 0.0,
            cancelled: false,
            failed: None,
            replay: BTreeMap::new(),
            replayed: 0,
        }
    }

    /// Journaled sessions to replay: a session whose key is present is
    /// completed from the journal the moment it would start.
    pub fn with_replay(mut self, entries: BTreeMap<String, SessionDone>) -> RunState {
        self.replay = entries;
        self
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    pub fn in_flight_keys(&self) -> Vec<SessionKey> {
        self.in_flight.keys().cloned().collect()
    }

    pub fn budget_exhausted(&self) -> bool {
        self.caps
            .budget_tokens
            .is_some_and(|b| self.tokens_spent >= b)
    }

    fn step_results(&self) -> BTreeMap<String, Value> {
        self.doc
            .steps
            .iter()
            .zip(&self.steps)
            .filter(|(_, st)| st.phase == Phase::Done || st.phase == Phase::Skipped)
            .map(|(s, st)| (s.id.clone(), st.result.clone()))
            .collect()
    }

    pub fn status(&self) -> RunStatus {
        if let Some(e) = &self.failed {
            return RunStatus::Failed(e.clone());
        }
        if self.cancelled && self.in_flight.is_empty() {
            return RunStatus::Cancelled;
        }
        let output = self
            .doc
            .step_index(&self.doc.output)
            .map(|i| &self.steps[i]);
        if let Some(out) = output
            && (out.phase == Phase::Done || out.phase == Phase::Skipped)
            && self
                .steps
                .iter()
                .all(|s| matches!(s.phase, Phase::Done | Phase::Skipped))
        {
            return RunStatus::Finished(out.result.clone());
        }
        if self.budget_exhausted() && self.in_flight.is_empty() {
            return RunStatus::BudgetExhausted(
                output.map(|o| o.result.clone()).unwrap_or(Value::Null),
            );
        }
        if self.in_flight.is_empty()
            && !self.cancelled
            && self.steps.iter().all(|s| s.phase != Phase::Active)
            && self.next_eligible().is_none()
            && !self
                .steps
                .iter()
                .all(|s| matches!(s.phase, Phase::Done | Phase::Skipped))
        {
            return RunStatus::Failed("no step can make progress".into());
        }
        RunStatus::Running
    }

    /// Result of a step (`Null` while pending).
    pub fn step_result(&self, id: &str) -> Value {
        self.doc
            .step_index(id)
            .map(|i| self.steps[i].result.clone())
            .unwrap_or(Value::Null)
    }

    /// One line per step for the progress display.
    pub fn step_states(&self) -> Vec<(String, &'static str)> {
        self.doc
            .steps
            .iter()
            .zip(&self.steps)
            .map(|(s, st)| {
                (
                    s.id.clone(),
                    match st.phase {
                        Phase::Pending => "pending",
                        Phase::Active => "running",
                        Phase::Done => "done",
                        Phase::Skipped => "skipped",
                    },
                )
            })
            .collect()
    }

    // ---- scheduling -------------------------------------------------------

    /// Ids of the steps a step reads.
    fn dependencies(&self, step: &Step) -> Vec<String> {
        let mut deps = Vec::new();
        let mut add_path = |p: &super::document::PathExpr| {
            if let super::document::Root::Step(id) = &p.root
                && !deps.contains(id)
            {
                deps.push(id.clone());
            }
        };
        if let Some(Over::Path(p)) = &step.over {
            add_path(p);
        }
        if let Some(p) = &step.input {
            add_path(p);
        }
        for v in step.args.values() {
            if let Value::String(s) = v {
                for ph in super::document::placeholders(s) {
                    if let Ok(p) = super::document::PathExpr::parse(&ph) {
                        add_path(&p);
                    }
                }
            }
        }
        if let Some(Actor::Prompt(t)) = &step.actor {
            for ph in super::document::placeholders(t) {
                if let Ok(p) = super::document::PathExpr::parse(&ph) {
                    add_path(&p);
                }
            }
        }
        if let Some((owner, _)) = self.doc.branch_owner(&step.id) {
            let owner = owner.to_string();
            if !deps.contains(&owner) {
                deps.push(owner);
            }
        }
        deps
    }

    fn ready(&self, i: usize) -> bool {
        let step = &self.doc.steps[i];
        self.dependencies(step).iter().all(|d| {
            self.doc
                .step_index(d)
                .is_some_and(|j| matches!(self.steps[j].phase, Phase::Done | Phase::Skipped))
        })
    }

    fn next_eligible(&self) -> Option<usize> {
        (0..self.doc.steps.len()).find(|&i| self.steps[i].phase == Phase::Pending && self.ready(i))
    }

    /// Sessions that may start now, at most `slots`. Replayed sessions are
    /// completed inline and do not count.
    pub fn next(&mut self, slots: usize) -> Vec<SessionSpec> {
        let mut out = Vec::new();
        if self.cancelled || self.failed.is_some() {
            return out;
        }
        loop {
            // activate every pending step whose dependencies are done
            let mut progressed = false;
            for i in 0..self.doc.steps.len() {
                if self.steps[i].phase == Phase::Pending && self.ready(i) {
                    self.activate(i);
                    progressed = true;
                }
            }
            if self.budget_exhausted() {
                return out;
            }
            // issue sessions from active steps in document order
            let mut replayed_any = false;
            for i in 0..self.doc.steps.len() {
                if self.steps[i].phase != Phase::Active {
                    continue;
                }
                let step_cap = self.doc.steps[i].concurrency.unwrap_or(usize::MAX);
                let mut issued: Vec<SessionSpec> = Vec::new();
                loop {
                    let in_step =
                        self.in_flight.values().filter(|(j, _)| *j == i).count() + issued.len();
                    if out.len() + issued.len() >= slots || in_step >= step_cap {
                        break;
                    }
                    if self.sessions_started + issued.len() >= self.caps.max_sessions {
                        self.failed =
                            Some(format!("session cap of {} reached", self.caps.max_sessions));
                        return out;
                    }
                    let Some(spec) = self.issue_one(i) else { break };
                    issued.push(spec);
                }
                for spec in issued {
                    let key = spec.key.clone();
                    self.in_flight.insert(key.clone(), (i, spec.phase.clone()));
                    self.sessions_started += 1;
                    if let Some(done) = self.replay.remove(&key.journal_key()) {
                        self.replayed += 1;
                        self.complete(&key, done);
                        replayed_any = true;
                    } else {
                        out.push(spec);
                    }
                }
            }
            if !replayed_any && !progressed {
                break;
            }
            if !replayed_any && out.len() >= slots {
                break;
            }
            if !replayed_any {
                // a second pass only helps when something completed
                let any_pending_ready = (0..self.doc.steps.len())
                    .any(|i| self.steps[i].phase == Phase::Pending && self.ready(i));
                if !any_pending_ready {
                    break;
                }
            }
        }
        out
    }

    fn env_for<'a>(
        &'a self,
        results: &'a BTreeMap<String, Value>,
        item: Option<&'a Value>,
        index: Option<usize>,
    ) -> Env<'a> {
        Env {
            args: &self.args,
            steps: results,
            item,
            index,
        }
    }

    fn items_of(&self, step: &Step, results: &BTreeMap<String, Value>) -> Vec<Value> {
        match &step.over {
            Some(Over::Inline(v)) => v.clone(),
            Some(Over::Path(p)) => match resolve(p, &self.env_for(results, None, None)) {
                Value::Array(a) => a,
                Value::Null => Vec::new(),
                other => vec![other],
            },
            None => Vec::new(),
        }
    }

    fn dedupe(items: Vec<Value>, by: &[String]) -> (Vec<Value>, usize) {
        if by.is_empty() {
            return (items, 0);
        }
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        let mut dropped = 0;
        for it in items {
            let key = dedupe_key(&it, by);
            if seen.insert(key) {
                out.push(it);
            } else {
                dropped += 1;
            }
        }
        (out, dropped)
    }

    fn activate(&mut self, i: usize) {
        let step = self.doc.steps[i].clone();
        let results = self.step_results();
        // a branch not taken
        if let Some((owner, label)) = self.doc.branch_owner(&step.id) {
            let chosen = self
                .doc
                .step_index(owner)
                .and_then(|j| self.steps[j].route_choice.clone());
            if chosen.as_deref() != Some(label) {
                self.steps[i].phase = Phase::Skipped;
                self.steps[i].result = Value::Null;
                self.notes
                    .push(format!("{}: skipped (branch {label:?} not taken)", step.id));
                return;
            }
        }
        let work = match step.kind {
            StepKind::Single | StepKind::Route => {
                let item = ItemWork {
                    item: Value::Null,
                    index: 0,
                    main: Some(Slot::new()),
                    subject: None,
                    votes: Vec::new(),
                    aggregated: None,
                    dropped: None,
                };
                if step.kind == StepKind::Route {
                    Work::Route { item, chosen: None }
                } else {
                    Work::Single { item }
                }
            }
            StepKind::Fanout | StepKind::Pipeline => {
                let raw = self.items_of(&step, &results);
                let (items, dropped) = Self::dedupe(raw, &step.dedupe_by);
                if dropped > 0 {
                    self.notes
                        .push(format!("{}: dropped {dropped} duplicate item(s)", step.id));
                }
                let items = items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| ItemWork {
                        subject: if step.actor.is_none() {
                            Some(item.clone())
                        } else {
                            None
                        },
                        item,
                        index,
                        main: step.actor.as_ref().map(|_| Slot::new()),
                        votes: Vec::new(),
                        aggregated: None,
                        dropped: None,
                    })
                    .collect();
                Work::Items {
                    items,
                    dedupe_dropped: dropped,
                }
            }
            StepKind::Tournament => {
                let candidates = self.items_of(&step, &results);
                let generate = if candidates.is_empty() {
                    (0..step.n.unwrap_or(0)).map(|_| Slot::new()).collect()
                } else {
                    Vec::new()
                };
                Work::Tournament {
                    generate,
                    candidates,
                    round: 0,
                    pairs: Vec::new(),
                    bye: None,
                    winners: Vec::new(),
                }
            }
            StepKind::Until => Work::Until {
                round: 0,
                rounds_without_new: 0,
                seen: Vec::new(),
                keys: BTreeSet::new(),
                current: Vec::new(),
            },
        };
        self.steps[i].work = Some(work);
        self.steps[i].phase = Phase::Active;
        self.advance(i);
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn spec_for(
        &self,
        i: usize,
        key: SessionKey,
        actor: &Actor,
        item: &Value,
        index: usize,
        inputs: Value,
        result_schema: Option<String>,
        previous_seen: Option<Value>,
        // `role`: the session's own harness and model, over the step's.
        role: &crate::workflows::document::Runner,
    ) -> SessionSpec {
        let step = &self.doc.steps[i];
        // The role's runner wins over the step's: a refuter or a judge may
        // be a different, cheaper or stronger, model than the work it reads.
        let runner = role.over(&crate::workflows::document::Workflow::runner_of(step));
        let results = self.step_results();
        let env = self.env_for(&results, Some(item), Some(index));
        let args: serde_json::Map<String, Value> = step
            .args
            .iter()
            .map(|(k, v)| {
                let v = match v {
                    Value::String(s) => Value::String(interpolate(s, &env)),
                    other => other.clone(),
                };
                (k.clone(), v)
            })
            .collect();
        let actor = match actor {
            Actor::Skill(s) => Actor::Skill(s.clone()),
            Actor::Prompt(p) => Actor::Prompt(interpolate(p, &env)),
        };
        SessionSpec {
            key,
            actor,
            args: Value::Object(args),
            item: item.clone(),
            index,
            inputs,
            result_schema,
            phase: step.phase.clone().unwrap_or_else(|| step.id.clone()),
            overrides: Overrides {
                harness: runner.harness,
                profile: runner.profile,
                model: runner.model,
                effort: runner.effort,
                isolation: step.isolation.or(self.doc.default_isolation),
                cwd: step.cwd.clone(),
                timeout_s: step.timeout_s,
            },
            previous_seen,
        }
    }

    fn main_inputs(&self, step: &Step) -> Value {
        match &step.input {
            Some(p) => {
                let results = self.step_results();
                resolve(p, &self.env_for(&results, None, None))
            }
            None => Value::Null,
        }
    }

    /// The next unissued session of an active step, if any.
    fn issue_one(&mut self, i: usize) -> Option<SessionSpec> {
        let step = self.doc.steps[i].clone();
        let work = self.steps[i].work.clone()?;
        let sid = step.id.clone();
        let mut issued: Option<(SessionSpec, Work)> = None;
        match work {
            Work::Single { mut item } | Work::Route { mut item, .. } => {
                if let Some((spec, new_item)) = self.issue_item(i, &step, &mut item, 0) {
                    let w = if step.kind == StepKind::Route {
                        Work::Route {
                            item: new_item,
                            chosen: None,
                        }
                    } else {
                        Work::Single { item: new_item }
                    };
                    issued = Some((spec, w));
                }
            }
            Work::Items {
                mut items,
                dedupe_dropped,
            } => {
                for idx in 0..items.len() {
                    let mut it = items[idx].clone();
                    if let Some((spec, new_item)) = self.issue_item(i, &step, &mut it, 0) {
                        items[idx] = new_item;
                        issued = Some((
                            spec,
                            Work::Items {
                                items,
                                dedupe_dropped,
                            },
                        ));
                        break;
                    }
                    items[idx] = it;
                }
            }
            Work::Tournament {
                mut generate,
                candidates,
                round,
                mut pairs,
                bye,
                winners,
            } => {
                if let Some(g) = generate.iter().position(|s| !s.issued) {
                    generate[g].issued = true;
                    let key = SessionKey {
                        step: sid.clone(),
                        item: g,
                        round: 0,
                        role: Role::Generate(g),
                    };
                    let actor = step.actor.clone().expect("validated");
                    let spec = self.spec_for(
                        i,
                        key,
                        &actor,
                        &Value::from(g),
                        g,
                        self.main_inputs(&step),
                        step.result.clone(),
                        None,
                        &crate::workflows::document::Runner::default(),
                    );
                    issued = Some((
                        spec,
                        Work::Tournament {
                            generate,
                            candidates,
                            round,
                            pairs,
                            bye,
                            winners,
                        },
                    ));
                } else if let Some(p) = pairs.iter().position(|(_, _, s)| !s.issued) {
                    pairs[p].2.issued = true;
                    let (a, b, _) = &pairs[p];
                    let key = SessionKey {
                        step: sid.clone(),
                        item: p,
                        round,
                        role: Role::Judge { round, pair: p },
                    };
                    let judge = step.judge.clone().expect("validated");
                    let inputs = serde_json::json!({ "a": candidates[*a], "b": candidates[*b] });
                    let spec = self.spec_for(
                        i,
                        key,
                        &judge,
                        &Value::Null,
                        p,
                        inputs,
                        None,
                        None,
                        &step.judge_runner,
                    );
                    issued = Some((
                        spec,
                        Work::Tournament {
                            generate,
                            candidates,
                            round,
                            pairs,
                            bye,
                            winners,
                        },
                    ));
                }
            }
            Work::Until {
                round,
                rounds_without_new,
                seen,
                keys,
                mut current,
            } => {
                for idx in 0..current.len() {
                    let mut it = current[idx].clone();
                    let previous = Some(Value::Array(seen.clone()));
                    if let Some((spec, new_item)) =
                        self.issue_item_round(i, &step, &mut it, round, previous)
                    {
                        current[idx] = new_item;
                        issued = Some((
                            spec,
                            Work::Until {
                                round,
                                rounds_without_new,
                                seen,
                                keys,
                                current,
                            },
                        ));
                        break;
                    }
                    current[idx] = it;
                }
            }
        }
        match issued {
            Some((spec, work)) => {
                self.steps[i].work = Some(work);
                Some(spec)
            }
            None => None,
        }
    }

    fn issue_item(
        &self,
        i: usize,
        step: &Step,
        it: &mut ItemWork,
        round: usize,
    ) -> Option<(SessionSpec, ItemWork)> {
        self.issue_item_round(i, step, it, round, None)
    }

    fn issue_item_round(
        &self,
        i: usize,
        step: &Step,
        it: &mut ItemWork,
        round: usize,
        previous: Option<Value>,
    ) -> Option<(SessionSpec, ItemWork)> {
        if it.dropped.is_some() {
            return None;
        }
        let sid = step.id.clone();
        if let Some(main) = &mut it.main
            && !main.issued
        {
            main.issued = true;
            let key = SessionKey {
                step: sid,
                item: it.index,
                round,
                role: Role::Main,
            };
            let actor = step.actor.clone().expect("a main slot implies an actor");
            let spec = self.spec_for(
                i,
                key,
                &actor,
                &it.item,
                it.index,
                self.main_inputs(step),
                step.result.clone(),
                previous,
                &crate::workflows::document::Runner::default(),
            );
            return Some((spec, it.clone()));
        }
        if let Some(v) = &step.verify
            && let Some(subject) = &it.subject
            && let Some(vi) = it.votes.iter().position(|s| !s.issued)
        {
            it.votes[vi].issued = true;
            let key = SessionKey {
                step: sid,
                item: it.index,
                round,
                role: Role::Vote(vi),
            };
            let inputs = serde_json::json!({ "item": subject, "vote": vi + 1, "votes": v.votes });
            let spec = self.spec_for(
                i,
                key,
                &v.actor,
                &it.item,
                it.index,
                inputs,
                v.result.clone(),
                None,
                &v.runner,
            );
            return Some((spec, it.clone()));
        }
        None
    }

    // ---- completion -------------------------------------------------------

    /// Records a finished session and advances its step.
    pub fn complete(&mut self, key: &SessionKey, done: SessionDone) {
        let Some((i, phase)) = self.in_flight.remove(key) else {
            return;
        };
        self.tokens_spent += done.tokens;
        self.cost_spent += done.cost_usd;
        self.records.push(SessionRecord {
            key: key.clone(),
            phase,
            outcome: done.outcome.clone(),
            tokens: done.tokens,
            cost_usd: done.cost_usd,
        });
        let step = self.doc.steps[i].clone();
        let Some(mut work) = self.steps[i].work.take() else {
            return;
        };
        match &mut work {
            Work::Single { item } | Work::Route { item, .. } => {
                Self::record_on_item(&step, item, key, &done.outcome);
            }
            Work::Items { items, .. } => {
                if let Some(it) = items.iter_mut().find(|it| it.index == key.item) {
                    Self::record_on_item(&step, it, key, &done.outcome);
                }
            }
            Work::Tournament {
                generate,
                candidates,
                pairs,
                ..
            } => match &key.role {
                Role::Generate(g) => {
                    generate[*g].done = Some(done.outcome.clone());
                    if generate.iter().all(Slot::finished) {
                        *candidates = generate
                            .iter()
                            .filter_map(|s| s.done.as_ref())
                            .filter(|o| !matches!(o, Outcome::Null(_)))
                            .map(Outcome::value)
                            .collect();
                    }
                }
                Role::Judge { pair, .. } => {
                    if let Some(p) = pairs.get_mut(*pair) {
                        p.2.done = Some(done.outcome.clone());
                    }
                }
                _ => {}
            },
            Work::Until { current, .. } => {
                if let Some(it) = current.iter_mut().find(|it| it.index == key.item) {
                    Self::record_on_item(&step, it, key, &done.outcome);
                }
            }
        }
        self.steps[i].work = Some(work);
        self.advance(i);
    }

    fn record_on_item(step: &Step, it: &mut ItemWork, key: &SessionKey, outcome: &Outcome) {
        match &key.role {
            Role::Main => {
                if let Some(m) = &mut it.main {
                    m.done = Some(outcome.clone());
                }
                match outcome {
                    Outcome::Null(why) => it.dropped = Some(format!("no answer: {why}")),
                    other => {
                        let v = other.value();
                        if let Some(k) = &step.keep
                            && !k.eval(&v)
                        {
                            it.dropped = Some("keep".into());
                        }
                        it.subject = Some(v);
                    }
                }
                if it.dropped.is_none()
                    && let Some(v) = &step.verify
                    && it.votes.is_empty()
                {
                    it.votes = (0..v.votes).map(|_| Slot::new()).collect();
                }
            }
            Role::Vote(vi) => {
                if let Some(s) = it.votes.get_mut(*vi) {
                    s.done = Some(outcome.clone());
                }
            }
            _ => {}
        }
    }

    /// Finishes items whose chains are complete and closes the step when
    /// nothing is left; may open a new round or bracket.
    fn advance(&mut self, i: usize) {
        let step = self.doc.steps[i].clone();
        let Some(mut work) = self.steps[i].work.take() else {
            return;
        };
        let mut finished: Option<Value> = None;
        match &mut work {
            Work::Single { item } => {
                Self::settle_item(&step, item);
                if Self::item_settled(&step, item) {
                    finished = Some(match &item.dropped {
                        Some(why) => {
                            self.notes.push(format!("{}: dropped ({why})", step.id));
                            Value::Null
                        }
                        None => item.subject.clone().unwrap_or(Value::Null),
                    });
                }
            }
            Work::Route { item, chosen } => {
                Self::settle_item(&step, item);
                if Self::item_settled(&step, item) {
                    let label = item
                        .subject
                        .as_ref()
                        .and_then(|v| v.get("label"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    match label {
                        Some(l) if step.branches.contains_key(&l) => {
                            self.notes.push(format!("{}: branch {l:?}", step.id));
                            *chosen = Some(l.clone());
                        }
                        Some(l) => {
                            self.notes.push(format!(
                                "{}: label {l:?} matches no branch; every branch skipped",
                                step.id
                            ));
                            *chosen = Some(l);
                        }
                        None => {
                            self.notes.push(format!(
                                "{}: no label in the answer; every branch skipped",
                                step.id
                            ));
                            *chosen = Some(String::new());
                        }
                    }
                    finished = Some(item.subject.clone().unwrap_or(Value::Null));
                }
            }
            Work::Items { items, .. } => {
                // items with no actor and no verify settle at once
                for it in items.iter_mut() {
                    if it.main.is_none() && it.votes.is_empty() && it.dropped.is_none() {
                        if let Some(k) = &step.keep
                            && let Some(s) = &it.subject
                            && !k.eval(s)
                        {
                            it.dropped = Some("keep".into());
                        }
                        if it.dropped.is_none()
                            && let Some(v) = &step.verify
                        {
                            it.votes = (0..v.votes).map(|_| Slot::new()).collect();
                        }
                    }
                    Self::settle_item(&step, it);
                }
                if items.iter().all(|it| Self::item_settled(&step, it)) {
                    let mut kept: Vec<Value> = Vec::new();
                    // why each item went: a reader must tell findings the
                    // keep predicate set aside from findings the votes refuted
                    let (mut below_keep, mut refuted, mut unanswered) = (0, 0, 0);
                    for it in items.iter() {
                        match it.dropped.as_deref() {
                            Some("keep") => below_keep += 1,
                            Some("verify") => refuted += 1,
                            Some(_) => unanswered += 1,
                            None => {
                                if let Some(s) = &it.subject {
                                    kept.push(s.clone());
                                }
                            }
                        }
                    }
                    let dropped = below_keep + refuted + unanswered;
                    if dropped > 0 {
                        let why: Vec<String> = [
                            (below_keep, "below keep"),
                            (refuted, "refuted"),
                            (unanswered, "without an answer"),
                        ]
                        .iter()
                        .filter(|(n, _)| *n > 0)
                        .map(|(n, w)| format!("{n} {w}"))
                        .collect();
                        self.notes.push(format!(
                            "{}: {dropped} item(s) dropped ({})",
                            step.id,
                            why.join(", ")
                        ));
                    }
                    if let Some(n) = step.take
                        && kept.len() > n
                    {
                        self.notes.push(format!(
                            "{}: take {n} kept {n} of {} item(s)",
                            step.id,
                            kept.len()
                        ));
                        kept.truncate(n);
                    }
                    finished = Some(Value::Array(kept));
                }
            }
            Work::Tournament {
                generate,
                candidates,
                round,
                pairs,
                bye,
                winners,
            } => loop {
                if generate.iter().any(|s| !s.finished()) {
                    break;
                }
                if candidates.len() <= 1 && pairs.is_empty() {
                    finished = Some(candidates.first().cloned().unwrap_or(Value::Null));
                    if candidates.is_empty() {
                        self.notes.push(format!("{}: no candidates", step.id));
                    }
                    break;
                }
                if pairs.is_empty() {
                    // open a round
                    let mut idx = 0;
                    while idx + 1 < candidates.len() {
                        pairs.push((idx, idx + 1, Slot::new()));
                        idx += 2;
                    }
                    *bye = (idx < candidates.len()).then_some(idx);
                    winners.clear();
                    self.notes.push(format!(
                        "{}: round {} with {} candidate(s)",
                        step.id,
                        *round + 1,
                        candidates.len()
                    ));
                    break;
                }
                if pairs.iter().all(|(_, _, s)| s.finished()) {
                    let mut next: Vec<Value> = Vec::new();
                    for (a, b, slot) in pairs.iter() {
                        let winner = slot.done.as_ref().map(Outcome::value).and_then(|v| {
                            v.get("winner").and_then(Value::as_str).map(str::to_string)
                        });
                        match winner.as_deref() {
                            Some("b") => next.push(candidates[*b].clone()),
                            _ => next.push(candidates[*a].clone()),
                        }
                    }
                    if let Some(b) = bye {
                        next.push(candidates[*b].clone());
                    }
                    *candidates = next;
                    pairs.clear();
                    *bye = None;
                    *round += 1;
                    continue;
                }
                break;
            },
            Work::Until {
                round,
                rounds_without_new,
                seen,
                keys,
                current,
            } => {
                if current.is_empty() {
                    // open a round
                    let results = self.step_results();
                    let items = match &step.over {
                        Some(_) => self.items_of(&step, &results),
                        None => vec![Value::Null],
                    };
                    *current = items
                        .into_iter()
                        .enumerate()
                        .map(|(index, item)| ItemWork {
                            item,
                            index,
                            main: Some(Slot::new()),
                            subject: None,
                            votes: Vec::new(),
                            aggregated: None,
                            dropped: None,
                        })
                        .collect();
                } else if current
                    .iter()
                    .all(|it| it.main.as_ref().is_some_and(Slot::finished))
                {
                    let mut new_items = 0;
                    for it in current.iter() {
                        let Some(v) = &it.subject else { continue };
                        for found in collect_items(v) {
                            let k = dedupe_key(&found, &step.dedupe_by);
                            if keys.insert(k) {
                                seen.push(found);
                                new_items += 1;
                            }
                        }
                    }
                    *round += 1;
                    if new_items == 0 {
                        *rounds_without_new += 1;
                    } else {
                        *rounds_without_new = 0;
                    }
                    self.notes.push(format!(
                        "{}: round {} found {new_items} new item(s), {} total",
                        step.id,
                        *round,
                        seen.len()
                    ));
                    current.clear();
                    if *rounds_without_new >= step.rounds_without_new || *round >= step.max_rounds {
                        finished = Some(Value::Array(seen.clone()));
                    } else {
                        // next round
                        let results = self.step_results();
                        let items = match &step.over {
                            Some(_) => self.items_of(&step, &results),
                            None => vec![Value::Null],
                        };
                        *current = items
                            .into_iter()
                            .enumerate()
                            .map(|(index, item)| ItemWork {
                                item,
                                index,
                                main: Some(Slot::new()),
                                subject: None,
                                votes: Vec::new(),
                                aggregated: None,
                                dropped: None,
                            })
                            .collect();
                    }
                }
            }
        }
        match finished {
            Some(v) => {
                self.steps[i].result = v;
                self.steps[i].phase = Phase::Done;
                self.steps[i].work = None;
                // a route decided: skip the branches not taken now
                if let Work::Route {
                    chosen: Some(c), ..
                } = &work
                {
                    self.steps[i].route_choice = Some(c.clone());
                    for (label, ids) in &step.branches {
                        if label != c {
                            for id in ids {
                                if let Some(j) = self.doc.step_index(id)
                                    && self.steps[j].phase == Phase::Pending
                                {
                                    self.steps[j].phase = Phase::Skipped;
                                    self.steps[j].result = Value::Null;
                                }
                            }
                        }
                    }
                }
            }
            None => self.steps[i].work = Some(work),
        }
    }

    /// Aggregates finished votes and applies `verify.keep`.
    fn settle_item(step: &Step, it: &mut ItemWork) {
        if it.dropped.is_some() || it.aggregated.is_some() || it.votes.is_empty() {
            return;
        }
        if !it.votes.iter().all(Slot::finished) {
            return;
        }
        let outcomes: Vec<Value> = it
            .votes
            .iter()
            .filter_map(|s| s.done.as_ref())
            .filter(|o| !matches!(o, Outcome::Null(_)))
            .map(Outcome::value)
            .collect();
        let agg = aggregate_votes(&outcomes, it.votes.len());
        if let Some(v) = &step.verify
            && let Some(k) = &v.keep
            && !k.eval(&agg)
        {
            it.dropped = Some("verify".into());
        }
        it.aggregated = Some(agg);
    }

    fn item_settled(step: &Step, it: &ItemWork) -> bool {
        if it.dropped.is_some() {
            return true;
        }
        if let Some(m) = &it.main
            && !m.finished()
        {
            return false;
        }
        if step.verify.is_some() {
            return it.aggregated.is_some();
        }
        true
    }
}

/// The key `dedupe_by` builds for an item: the named fields, or the
/// whole value for scalars.
pub fn dedupe_key(item: &Value, by: &[String]) -> String {
    if by.is_empty() || !item.is_object() {
        return item.to_string();
    }
    by.iter()
        .map(|f| item.get(f).map(Value::to_string).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

/// The items a session of an `until` step reported: an array as is, an
/// object's single array field, else the object itself.
pub fn collect_items(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => {
            let arrays: Vec<&Value> = o.values().filter(|v| v.is_array()).collect();
            if arrays.len() == 1 {
                arrays[0].as_array().cloned().unwrap_or_default()
            } else {
                vec![v.clone()]
            }
        }
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    }
}

/// Aggregates vote results: boolean fields become the count of votes
/// where they are true, `votes` and `answers` count the ballots, other
/// fields keep the last vote's value.
pub fn aggregate_votes(outcomes: &[Value], votes: usize) -> Value {
    let mut agg = serde_json::Map::new();
    agg.insert("votes".into(), Value::from(votes));
    agg.insert("answers".into(), Value::from(outcomes.len()));
    for o in outcomes {
        if let Some(obj) = o.as_object() {
            for (k, v) in obj {
                match v {
                    Value::Bool(b) => {
                        let cur = agg.get(k).and_then(Value::as_u64).unwrap_or(0);
                        agg.insert(k.clone(), Value::from(cur + u64::from(*b)));
                    }
                    other => {
                        agg.insert(k.clone(), other.clone());
                    }
                }
            }
        }
    }
    Value::Object(agg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::document::parse;

    /// Runs a workflow to completion with a synchronous fake: `answer` maps
    /// a session to its outcome.
    fn drive(
        doc: &str,
        args: Value,
        caps: Caps,
        mut answer: impl FnMut(&SessionSpec) -> Outcome,
    ) -> (RunState, Vec<String>) {
        let doc = parse(doc).unwrap();
        let mut st = RunState::new(doc, args, caps);
        let mut labels = Vec::new();
        for _ in 0..10_000 {
            let specs = st.next(4);
            if specs.is_empty() {
                if st.in_flight() == 0 {
                    break;
                }
                panic!("stuck with {} in flight", st.in_flight());
            }
            for spec in specs {
                labels.push(spec.label());
                let o = answer(&spec);
                st.complete(
                    &spec.key,
                    SessionDone {
                        outcome: o,
                        tokens: 10,
                        cost_usd: 0.01,
                    },
                );
            }
        }
        (st, labels)
    }

    fn obj(v: Value) -> Outcome {
        Outcome::Object(v)
    }

    const REVIEW: &str = r#"
[workflow]
name = "review"
description = "d"
output = "report"
[args.scope]
default = "src"
[schemas.findings]
fields.findings = { type = "array", items = "finding", required = true }
[schemas.finding]
fields.file = { type = "string", required = true }
fields.line = { type = "integer" }
[schemas.verdict]
fields.refuted = { type = "boolean", required = true }

[[steps]]
id = "find"
kind = "fanout"
phase = "Review"
over = ["a", "b"]
skill = "wf-find"
args = { dimension = "{item}", scope = "{args.scope}" }
result = "findings"

[[steps]]
id = "confirmed"
kind = "pipeline"
over = "find[*].findings[*]"
dedupe_by = ["file", "line"]
verify = { skill = "wf-refute", votes = 3, result = "verdict", keep = "refuted < 2" }

[[steps]]
id = "report"
prompt = "Summarize {confirmed}"
input = "confirmed"
"#;

    #[test]
    fn fanout_verify_and_synthesis_run_in_order() {
        let (st, labels) = drive(
            REVIEW,
            serde_json::json!({ "scope": "src" }),
            Caps::default(),
            |spec| {
                match (spec.key.step.as_str(), &spec.key.role) {
                    ("find", Role::Main) => {
                        assert_eq!(spec.args["scope"], "src");
                        assert_eq!(spec.result_schema.as_deref(), Some("findings"));
                        let dim = spec.args["dimension"].as_str().unwrap().to_string();
                        obj(serde_json::json!({ "findings": [
                            { "file": format!("{dim}.rs"), "line": 1 },
                            { "file": "shared.rs", "line": 2 }
                        ]}))
                    }
                    ("confirmed", Role::Vote(i)) => {
                        let file = spec.inputs["item"]["file"].as_str().unwrap();
                        // shared.rs is refuted by everyone; a.rs by one vote
                        let refuted = file == "shared.rs" || (file == "a.rs" && *i == 0);
                        obj(serde_json::json!({ "refuted": refuted }))
                    }
                    ("report", Role::Main) => {
                        assert!(spec.inputs.as_array().unwrap().len() == 2);
                        assert!(matches!(&spec.actor, Actor::Prompt(p) if p.contains("a.rs")));
                        Outcome::Text("done".into())
                    }
                    other => panic!("unexpected {other:?}"),
                }
            },
        );
        assert_eq!(
            st.status(),
            RunStatus::Finished(Value::String("done".into()))
        );
        // 2 finders, 3 unique findings × 3 votes, 1 report
        assert_eq!(labels.len(), 2 + 9 + 1, "{labels:?}");
        assert!(labels.contains(&"find[1]".to_string()));
        assert!(labels.contains(&"confirmed[2]/vote3".to_string()));
        assert!(
            st.notes.iter().any(|n| n.contains("dropped 1 duplicate")),
            "{:?}",
            st.notes
        );
        let confirmed = st.step_result("confirmed");
        assert_eq!(confirmed.as_array().unwrap().len(), 2, "{confirmed}");
        assert_eq!(st.tokens_spent, 120);
        assert_eq!(st.records.len(), 12);
        assert_eq!(st.records[0].phase, "Review");
    }

    #[test]
    fn concurrency_slots_and_step_caps_are_respected() {
        let doc = parse(REVIEW).unwrap();
        let mut st = RunState::new(doc, serde_json::json!({}), Caps::default());
        let first = st.next(1);
        assert_eq!(first.len(), 1);
        let more = st.next(5);
        assert_eq!(more.len(), 1, "one finder left");
        assert!(st.next(5).is_empty(), "confirmed waits for find");
        assert_eq!(st.status(), RunStatus::Running);
    }

    #[test]
    fn budget_and_session_caps_stop_the_run() {
        let (st, labels) = drive(
            REVIEW,
            serde_json::json!({}),
            Caps {
                max_sessions: 1000,
                budget_tokens: Some(15),
            },
            |_| obj(serde_json::json!({ "findings": [] })),
        );
        assert!(
            matches!(st.status(), RunStatus::BudgetExhausted(_)),
            "{:?}",
            st.status()
        );
        assert!(labels.len() <= 2, "{labels:?}");

        let (st, _) = drive(
            REVIEW,
            serde_json::json!({}),
            Caps {
                max_sessions: 1,
                budget_tokens: None,
            },
            |_| obj(serde_json::json!({ "findings": [] })),
        );
        assert!(matches!(st.status(), RunStatus::Failed(e) if e.contains("session cap")));
    }

    #[test]
    fn cancel_and_null_answers() {
        let doc = parse(REVIEW).unwrap();
        let mut st = RunState::new(doc, serde_json::json!({}), Caps::default());
        let specs = st.next(4);
        st.cancel();
        assert_eq!(st.status(), RunStatus::Running, "sessions still in flight");
        for s in specs {
            st.complete(
                &s.key,
                SessionDone {
                    outcome: Outcome::Null("timeout".into()),
                    tokens: 0,
                    cost_usd: 0.0,
                },
            );
        }
        assert_eq!(st.status(), RunStatus::Cancelled);
        assert!(st.next(4).is_empty());

        // null main answers drop the item; an empty fanout still finishes
        let (st, _) = drive(
            REVIEW,
            serde_json::json!({}),
            Caps::default(),
            |spec| match spec.key.step.as_str() {
                "find" => Outcome::Null("no block".into()),
                _ => Outcome::Text("empty".into()),
            },
        );
        assert_eq!(
            st.status(),
            RunStatus::Finished(Value::String("empty".into()))
        );
        assert_eq!(st.step_result("find"), serde_json::json!([]));
        assert!(
            st.notes
                .iter()
                .any(|n| n.contains("2 item(s) dropped (2 without an answer)")),
            "{:?}",
            st.notes
        );
    }

    #[test]
    fn replay_completes_journaled_sessions_without_running_them() {
        let doc = parse(REVIEW).unwrap();
        let mut entries = BTreeMap::new();
        for label in ["find", "find[1]"] {
            entries.insert(
                label.to_string(),
                SessionDone {
                    outcome: obj(serde_json::json!({ "findings": [] })),
                    tokens: 7,
                    cost_usd: 0.0,
                },
            );
        }
        let mut st =
            RunState::new(doc, serde_json::json!({}), Caps::default()).with_replay(entries);
        let specs = st.next(4);
        assert_eq!(st.replayed, 2);
        assert_eq!(
            specs.len(),
            1,
            "only the report runs: {:?}",
            specs.iter().map(|s| s.label()).collect::<Vec<_>>()
        );
        assert_eq!(specs[0].key.step, "report");
        assert_eq!(st.tokens_spent, 14);
    }

    const ROUTE: &str = r#"
[workflow]
name = "route"
description = "d"
[schemas.label]
fields.label = { type = "string", required = true }
[[steps]]
id = "classify"
kind = "route"
skill = "wf-classify"
result = "label"
branches = { bug = ["fix"], feature = ["design"] }
[[steps]]
id = "fix"
prompt = "fix it"
[[steps]]
id = "design"
prompt = "design it"
[[steps]]
id = "wrap"
prompt = "wrap {fix} {design}"
"#;

    #[test]
    fn route_runs_one_branch_and_skips_the_other() {
        let (st, labels) = drive(ROUTE, Value::Null, Caps::default(), |spec| {
            match spec.key.step.as_str() {
                "classify" => obj(serde_json::json!({ "label": "bug" })),
                s => Outcome::Text(format!("{s} done")),
            }
        });
        assert_eq!(labels, vec!["classify", "fix", "wrap"]);
        assert_eq!(
            st.status(),
            RunStatus::Finished(Value::String("wrap done".into()))
        );
        let states = st.step_states();
        assert_eq!(states[2], ("design".to_string(), "skipped"));
        assert!(st.notes.iter().any(|n| n.contains("branch \"bug\"")));

        let (st, labels) = drive(ROUTE, Value::Null, Caps::default(), |spec| {
            match spec.key.step.as_str() {
                "classify" => obj(serde_json::json!({ "label": "other" })),
                s => Outcome::Text(format!("{s} done")),
            }
        });
        assert_eq!(labels, vec!["classify", "wrap"]);
        assert!(matches!(st.status(), RunStatus::Finished(_)));
    }

    const TOURNAMENT: &str = r#"
[workflow]
name = "judge-panel"
description = "d"
[schemas.winner]
fields.winner = { type = "string", required = true, enum = ["a", "b"] }
[[steps]]
id = "best"
kind = "tournament"
n = 3
prompt = "attempt {index}"
judge_prompt = "pick"
"#;

    #[test]
    fn tournament_generates_then_judges_pairwise_with_byes() {
        let (st, labels) = drive(TOURNAMENT, Value::Null, Caps::default(), |spec| match &spec
            .key
            .role
        {
            Role::Generate(i) => Outcome::Text(format!("candidate {i}")),
            Role::Judge { .. } => {
                assert!(spec.inputs["a"].is_string() && spec.inputs["b"].is_string());
                obj(serde_json::json!({ "winner": "b" }))
            }
            _ => panic!(),
        });
        assert_eq!(
            labels,
            vec![
                "best/gen1",
                "best/gen2",
                "best/gen3",
                "best/judge1.1",
                "best/r1/judge2.1"
            ]
        );
        // round 1: (0,1) -> 1 wins, 2 has a bye; round 2: (1,2) -> 2 wins
        assert_eq!(
            st.status(),
            RunStatus::Finished(Value::String("candidate 2".into()))
        );
    }

    const UNTIL: &str = r#"
[workflow]
name = "audit"
description = "d"
[[steps]]
id = "sweep"
kind = "until"
prompt = "find more"
dedupe_by = ["id"]
rounds_without_new = 2
max_rounds = 10
"#;

    #[test]
    fn until_stops_after_quiet_rounds_and_dedupes_across_rounds() {
        let mut round = 0;
        let (st, labels) = drive(UNTIL, Value::Null, Caps::default(), |spec| {
            round += 1;
            if round == 1 {
                assert_eq!(spec.previous_seen, Some(serde_json::json!([])));
            }
            let items = match round {
                1 => serde_json::json!({ "bugs": [ { "id": 1 }, { "id": 2 } ] }),
                2 => serde_json::json!({ "bugs": [ { "id": 2 }, { "id": 3 } ] }),
                _ => serde_json::json!({ "bugs": [ { "id": 3 } ] }),
            };
            obj(items)
        });
        assert_eq!(labels, vec!["sweep", "sweep/r1", "sweep/r2", "sweep/r3"]);
        let RunStatus::Finished(v) = st.status() else {
            panic!()
        };
        assert_eq!(v.as_array().unwrap().len(), 3);
        assert!(st.notes.last().unwrap().contains("found 0 new"));
    }

    #[test]
    fn helpers() {
        assert_eq!(
            dedupe_key(&serde_json::json!({"a":1,"b":2}), &["a".into()]),
            "1"
        );
        assert_eq!(dedupe_key(&serde_json::json!("x"), &["a".into()]), "\"x\"");
        assert_eq!(
            collect_items(&serde_json::json!({"items":[1,2],"n":2})).len(),
            2
        );
        assert_eq!(
            collect_items(&serde_json::json!({"a":[1],"b":[2]})).len(),
            1
        );
        let agg = aggregate_votes(
            &[
                serde_json::json!({"refuted": true, "why": "x"}),
                serde_json::json!({"refuted": false}),
            ],
            3,
        );
        assert_eq!(agg["refuted"], 1);
        assert_eq!(agg["votes"], 3);
        assert_eq!(agg["answers"], 2);
        assert_eq!(agg["why"], "x");
        let k = SessionKey {
            step: "s".into(),
            item: 0,
            round: 0,
            role: Role::Main,
        };
        assert_eq!(k.label(), "s");
    }
}
