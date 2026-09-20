//! The workflow document: `[workflow]`, `[args]`, `[schemas]` and
//! `[[steps]]` in TOML, parsed into a validated `Workflow`. Paths
//! (`find[*].findings[*]`, `args.scope`, `item.file`), predicates
//! (`refuted < 2 and has(file)`) and `{…}` interpolations are parsed here
//! and resolved statically against the declared steps, so a document that
//! loads is one the interpreter can run without surprises.

use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Which harnesses a workflow allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessFilter {
    Any,
    Only(Vec<String>),
}

impl HarnessFilter {
    pub fn allows(&self, harness: &str) -> bool {
        match self {
            HarnessFilter::Any => true,
            HarnessFilter::Only(list) => list.iter().any(|h| h == harness),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    Single,
    Fanout,
    Pipeline,
    Route,
    Tournament,
    Until,
}

impl StepKind {
    pub fn label(self) -> &'static str {
        match self {
            StepKind::Single => "single",
            StepKind::Fanout => "fanout",
            StepKind::Pipeline => "pipeline",
            StepKind::Route => "route",
            StepKind::Tournament => "tournament",
            StepKind::Until => "until",
        }
    }

    /// The step runs once per item of `over`.
    pub fn per_item(self) -> bool {
        matches!(self, StepKind::Fanout | StepKind::Pipeline)
    }
}

/// What a session runs: a step skill or an inline prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    Skill(String),
    Prompt(String),
}

impl Actor {
    pub fn skill(&self) -> Option<&str> {
        match self {
            Actor::Skill(s) => Some(s),
            Actor::Prompt(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Isolation {
    #[default]
    None,
    Worktree,
}

/// One field of a schema.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub required: bool,
    /// For arrays: a primitive type name or another schema's name.
    pub items: Option<String>,
    #[serde(rename = "enum")]
    pub enum_values: Option<Vec<Value>>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    #[serde(default)]
    pub fields: BTreeMap<String, Field>,
}

pub const PRIMITIVE_TYPES: &[&str] = &["string", "integer", "number", "boolean", "array", "object"];

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ArgSpec {
    pub description: Option<String>,
    pub default: Option<Value>,
    #[serde(default)]
    pub required: bool,
}

/// Which harness and model run a session. A step sets it; a `verify` block
/// and a `judge` may set their own, so the refuters of a finding or the
/// judges of a tournament can be cheaper or stronger than the work they
/// check. An unset field falls back to the step's, then to the run's.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Runner {
    pub harness: Option<String>,
    pub profile: Option<String>,
    pub model: Option<String>,
    /// Reasoning effort, where the harness takes one (Codex).
    pub effort: Option<String>,
}

impl Runner {
    pub fn is_empty(&self) -> bool {
        self.harness.is_none()
            && self.profile.is_none()
            && self.model.is_none()
            && self.effort.is_none()
    }

    /// `self` where it is set, `base` otherwise.
    pub fn over(&self, base: &Runner) -> Runner {
        Runner {
            harness: self.harness.clone().or_else(|| base.harness.clone()),
            profile: self.profile.clone().or_else(|| base.profile.clone()),
            model: self.model.clone().or_else(|| base.model.clone()),
            effort: self.effort.clone().or_else(|| base.effort.clone()),
        }
    }

    /// How it reads in a listing: `codex · gpt-5-mini`.
    pub fn label(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if let Some(h) = &self.harness {
            parts.push(h);
        }
        if let Some(p) = &self.profile {
            parts.push(p);
        }
        if let Some(m) = &self.model {
            parts.push(m);
        }
        if let Some(e) = &self.effort {
            parts.push(e);
        }
        parts.join(" · ")
    }
}

/// A `verify = { … }` table.
#[derive(Debug, Clone, PartialEq)]
pub struct Verify {
    pub actor: Actor,
    pub votes: usize,
    pub result: Option<String>,
    pub keep: Option<Predicate>,
    pub keep_text: Option<String>,
    /// The refuters' own harness and model; empty inherits the step's.
    pub runner: Runner,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Over {
    Inline(Vec<Value>),
    Path(PathExpr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub id: String,
    pub kind: StepKind,
    pub actor: Option<Actor>,
    pub args: BTreeMap<String, Value>,
    pub over: Option<Over>,
    pub input: Option<PathExpr>,
    pub result: Option<String>,
    pub verify: Option<Verify>,
    pub keep: Option<Predicate>,
    pub dedupe_by: Vec<String>,
    pub take: Option<usize>,
    pub branches: BTreeMap<String, Vec<String>>,
    pub judge: Option<Actor>,
    /// The judge's own harness and model; empty inherits the step's.
    pub judge_runner: Runner,
    pub n: Option<usize>,
    pub rounds_without_new: usize,
    pub max_rounds: usize,
    pub harness: Option<String>,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub isolation: Option<Isolation>,
    pub cwd: Option<String>,
    pub timeout_s: Option<u64>,
    pub concurrency: Option<usize>,
    pub phase: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub harness: HarnessFilter,
    pub output: String,
    pub default_isolation: Option<Isolation>,
    pub budget_tokens: Option<u64>,
    pub max_concurrent: Option<usize>,
    pub args: BTreeMap<String, ArgSpec>,
    pub schemas: BTreeMap<String, Schema>,
    pub steps: Vec<Step>,
}

impl Workflow {
    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    pub fn step_index(&self, id: &str) -> Option<usize> {
        self.steps.iter().position(|s| s.id == id)
    }

    /// The step runner of `s`: what a session of that step runs on.
    pub fn runner_of(step: &Step) -> Runner {
        Runner {
            harness: step.harness.clone(),
            profile: step.profile.clone(),
            model: step.model.clone(),
            effort: step.effort.clone(),
        }
    }

    /// Every harness named anywhere in the document: a step, a verify
    /// block or a judge. The run's own harness is not included.
    pub fn harnesses_used(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |h: &Option<String>| {
            if let Some(h) = h
                && !out.contains(h)
            {
                out.push(h.clone());
            }
        };
        for s in &self.steps {
            push(&s.harness);
            push(&s.judge_runner.harness);
            if let Some(v) = &s.verify {
                push(&v.runner.harness);
            }
        }
        out
    }

    /// Every skill the document references (steps, verify, judge).
    pub fn skills(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |a: &Option<Actor>| {
            if let Some(Actor::Skill(s)) = a
                && !out.contains(s)
            {
                out.push(s.clone());
            }
        };
        for s in &self.steps {
            push(&s.actor);
            push(&s.judge);
            if let Some(v) = &s.verify {
                push(&Some(v.actor.clone()));
            }
        }
        out
    }

    /// Which route step's branch a step belongs to, if any.
    pub fn branch_owner(&self, step_id: &str) -> Option<(&str, &str)> {
        for s in &self.steps {
            for (label, ids) in &s.branches {
                if ids.iter().any(|i| i == step_id) {
                    return Some((s.id.as_str(), label.as_str()));
                }
            }
        }
        None
    }

    /// The phases in first-seen order, for the preview.
    pub fn phases(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for s in &self.steps {
            let p = s.phase.clone().unwrap_or_else(|| s.id.clone());
            if !v.contains(&p) {
                v.push(p);
            }
        }
        v
    }
}

// ---- raw TOML shape ----------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDoc {
    workflow: RawWorkflow,
    #[serde(default)]
    args: BTreeMap<String, ArgSpec>,
    #[serde(default)]
    schemas: BTreeMap<String, Schema>,
    #[serde(default)]
    steps: Vec<RawStep>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorkflow {
    name: String,
    description: String,
    when_to_use: Option<String>,
    harness: Option<toml::Value>,
    output: Option<String>,
    default_isolation: Option<Isolation>,
    budget_tokens: Option<u64>,
    max_concurrent: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVerify {
    skill: Option<String>,
    prompt: Option<String>,
    #[serde(default = "one")]
    votes: usize,
    result: Option<String>,
    keep: Option<String>,
    harness: Option<String>,
    profile: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

/// `judge = "wf-judge"` or `judge = { skill = …, harness = …, model = … }`.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawJudge {
    Name(String),
    Table(RawJudgeTable),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJudgeTable {
    skill: Option<String>,
    prompt: Option<String>,
    harness: Option<String>,
    profile: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

fn one() -> usize {
    1
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    id: String,
    #[serde(default = "single")]
    kind: StepKind,
    skill: Option<String>,
    prompt: Option<String>,
    #[serde(default)]
    args: BTreeMap<String, toml::Value>,
    over: Option<toml::Value>,
    input: Option<String>,
    result: Option<String>,
    verify: Option<RawVerify>,
    keep: Option<String>,
    #[serde(default)]
    dedupe_by: Vec<String>,
    take: Option<usize>,
    #[serde(default)]
    branches: BTreeMap<String, Vec<String>>,
    judge: Option<RawJudge>,
    judge_prompt: Option<String>,
    n: Option<usize>,
    rounds_without_new: Option<usize>,
    max_rounds: Option<usize>,
    harness: Option<String>,
    profile: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    isolation: Option<Isolation>,
    cwd: Option<String>,
    timeout_s: Option<u64>,
    concurrency: Option<usize>,
    phase: Option<String>,
}

fn single() -> StepKind {
    StepKind::Single
}

pub fn toml_to_json(v: toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::from(i),
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            Value::Object(t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect())
        }
    }
}

fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Parses a document. Structural problems (TOML, unknown keys, malformed
/// paths and predicates) are errors; `validate` checks the semantics.
pub fn parse(text: &str) -> Result<Workflow, Vec<String>> {
    let raw: RawDoc = toml::from_str(text).map_err(|e| vec![format!("workflow.toml: {e}")])?;
    let mut problems = Vec::new();
    let harness = match raw.workflow.harness {
        None => HarnessFilter::Any,
        Some(toml::Value::String(s)) if s == "any" => HarnessFilter::Any,
        Some(toml::Value::String(s)) => HarnessFilter::Only(vec![s]),
        Some(toml::Value::Array(a)) => HarnessFilter::Only(
            a.into_iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        ),
        Some(_) => {
            problems.push("workflow.harness must be a string or an array of strings".into());
            HarnessFilter::Any
        }
    };
    let mut steps = Vec::new();
    for r in raw.steps {
        let ctx = format!("step {}", r.id);
        let actor = match (r.skill, r.prompt) {
            (Some(s), None) => Some(Actor::Skill(s)),
            (None, Some(p)) => Some(Actor::Prompt(p)),
            (None, None) => None,
            (Some(_), Some(_)) => {
                problems.push(format!("{ctx}: give either skill or prompt, not both"));
                None
            }
        };
        let over = match r.over {
            None => None,
            Some(toml::Value::Array(a)) => {
                Some(Over::Inline(a.into_iter().map(toml_to_json).collect()))
            }
            Some(toml::Value::String(s)) => match PathExpr::parse(&s) {
                Ok(p) => Some(Over::Path(p)),
                Err(e) => {
                    problems.push(format!("{ctx}: over: {e}"));
                    None
                }
            },
            Some(_) => {
                problems.push(format!("{ctx}: over must be an array or a path"));
                None
            }
        };
        let input = match r.input {
            None => None,
            Some(s) => match PathExpr::parse(&s) {
                Ok(p) => Some(p),
                Err(e) => {
                    problems.push(format!("{ctx}: input: {e}"));
                    None
                }
            },
        };
        let keep = match r.keep {
            None => None,
            Some(s) => match Predicate::parse(&s) {
                Ok(p) => Some(p),
                Err(e) => {
                    problems.push(format!("{ctx}: keep: {e}"));
                    None
                }
            },
        };
        let verify = match r.verify {
            None => None,
            Some(v) => {
                let actor = match (v.skill, v.prompt) {
                    (Some(s), None) => Some(Actor::Skill(s)),
                    (None, Some(p)) => Some(Actor::Prompt(p)),
                    _ => {
                        problems.push(format!(
                            "{ctx}: verify needs exactly one of skill or prompt"
                        ));
                        None
                    }
                };
                let keep = match &v.keep {
                    None => None,
                    Some(s) => match Predicate::parse(s) {
                        Ok(p) => Some(p),
                        Err(e) => {
                            problems.push(format!("{ctx}: verify.keep: {e}"));
                            None
                        }
                    },
                };
                if v.votes == 0 {
                    problems.push(format!("{ctx}: verify.votes must be at least 1"));
                }
                let runner = Runner {
                    harness: v.harness,
                    profile: v.profile,
                    model: v.model,
                    effort: v.effort,
                };
                actor.map(|actor| Verify {
                    actor,
                    votes: v.votes.max(1),
                    result: v.result,
                    keep,
                    keep_text: v.keep,
                    runner,
                })
            }
        };
        let mut judge_runner = Runner::default();
        let judge_from_table = match r.judge {
            Some(RawJudge::Name(name)) => Some(Actor::Skill(name)),
            Some(RawJudge::Table(t)) => {
                judge_runner = Runner {
                    harness: t.harness,
                    profile: t.profile,
                    model: t.model,
                    effort: t.effort,
                };
                match (t.skill, t.prompt) {
                    (Some(s), None) => Some(Actor::Skill(s)),
                    (None, Some(p)) => Some(Actor::Prompt(p)),
                    (None, None) => {
                        problems.push(format!("{ctx}: judge needs a skill or a prompt"));
                        None
                    }
                    (Some(_), Some(_)) => {
                        problems.push(format!("{ctx}: judge takes a skill or a prompt, not both"));
                        None
                    }
                }
            }
            None => None,
        };
        let judge = match (judge_from_table, r.judge_prompt) {
            (Some(a), None) => Some(a),
            (None, Some(p)) => Some(Actor::Prompt(p)),
            (None, None) => None,
            (Some(a), Some(_)) => {
                problems.push(format!("{ctx}: give either judge or judge_prompt"));
                Some(a)
            }
        };
        let args: BTreeMap<String, Value> = r
            .args
            .into_iter()
            .map(|(k, v)| (k, toml_to_json(v)))
            .collect();
        for (k, v) in &args {
            if let Value::String(s) = v {
                for ph in placeholders(s) {
                    if let Err(e) = PathExpr::parse(&ph) {
                        problems.push(format!("{ctx}: args.{k}: {{{ph}}}: {e}"));
                    }
                }
            }
        }
        if let Some(Actor::Prompt(p)) = &actor {
            for ph in placeholders(p) {
                if let Err(e) = PathExpr::parse(&ph) {
                    problems.push(format!("{ctx}: prompt: {{{ph}}}: {e}"));
                }
            }
        }
        steps.push(Step {
            id: r.id,
            kind: r.kind,
            actor,
            args,
            over,
            input,
            result: r.result,
            verify,
            keep,
            dedupe_by: r.dedupe_by,
            take: r.take,
            branches: r.branches,
            judge,
            judge_runner,
            n: r.n,
            rounds_without_new: r.rounds_without_new.unwrap_or(2),
            max_rounds: r.max_rounds.unwrap_or(10),
            harness: r.harness,
            profile: r.profile,
            model: r.model,
            effort: r.effort,
            isolation: r.isolation,
            cwd: r.cwd,
            timeout_s: r.timeout_s,
            concurrency: r.concurrency,
            phase: r.phase,
        });
    }
    let output = raw
        .workflow
        .output
        .or_else(|| steps.last().map(|s| s.id.clone()))
        .unwrap_or_default();
    let doc = Workflow {
        name: raw.workflow.name,
        description: raw.workflow.description,
        when_to_use: raw.workflow.when_to_use,
        harness,
        output,
        default_isolation: raw.workflow.default_isolation,
        budget_tokens: raw.workflow.budget_tokens,
        max_concurrent: raw.workflow.max_concurrent,
        args: raw.args,
        schemas: raw.schemas,
        steps,
    };
    if problems.is_empty() {
        Ok(doc)
    } else {
        Err(problems)
    }
}

/// What the validator knows about a step skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInfo {
    pub name: String,
    /// `writes = true` in its skill.toml: the skill edits files.
    pub writes: bool,
}

/// Semantic checks: ids, kinds, references, ordering, isolation. `skills`
/// is `None` to skip the skill existence check (a document alone).
pub fn validate(doc: &Workflow, skills: Option<&[SkillInfo]>) -> Vec<String> {
    let mut problems = Vec::new();
    if !is_valid_id(&doc.name) {
        problems.push(format!(
            "workflow.name {:?} must match ^[a-z][a-z0-9_-]*$",
            doc.name
        ));
    }
    if doc.description.trim().is_empty() {
        problems.push("workflow.description is empty".into());
    }
    if doc.steps.is_empty() {
        problems.push("no steps".into());
        return problems;
    }
    if let HarnessFilter::Only(list) = &doc.harness {
        for h in list {
            if !matches!(h.as_str(), "claude" | "codex" | "agy") {
                problems.push(format!("workflow.harness: unknown harness {h:?}"));
            }
        }
    }
    let ids: Vec<&str> = doc.steps.iter().map(|s| s.id.as_str()).collect();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (i, s) in doc.steps.iter().enumerate() {
        let ctx = format!("step {}", s.id);
        if !is_valid_id(&s.id) {
            problems.push(format!("{ctx}: id must match ^[a-z][a-z0-9_-]*$"));
        }
        if !seen.insert(s.id.as_str()) {
            problems.push(format!("{ctx}: duplicate id"));
        }
        if matches!(s.id.as_str(), "args" | "item" | "index" | "steps") {
            problems.push(format!("{ctx}: id is a reserved word"));
        }
        let earlier: Vec<&str> = ids[..i].to_vec();
        // actor requirements
        let needs_actor = match s.kind {
            StepKind::Single | StepKind::Fanout | StepKind::Route | StepKind::Until => true,
            StepKind::Pipeline => false,
            StepKind::Tournament => s.over.is_none(),
        };
        if needs_actor && s.actor.is_none() {
            problems.push(format!(
                "{ctx}: a {} step needs skill or prompt",
                s.kind.label()
            ));
        }
        if s.kind == StepKind::Pipeline
            && s.actor.is_none()
            && s.verify.is_none()
            && s.keep.is_none()
            && s.dedupe_by.is_empty()
            && s.take.is_none()
        {
            problems.push(format!(
                "{ctx}: a pipeline without skill, prompt, verify or transforms does nothing"
            ));
        }
        match s.kind {
            StepKind::Fanout | StepKind::Pipeline if s.over.is_none() => {
                problems.push(format!("{ctx}: a {} step needs over", s.kind.label()));
            }
            StepKind::Tournament => {
                if s.judge.is_none() {
                    problems.push(format!("{ctx}: a tournament needs judge or judge_prompt"));
                }
                if s.over.is_none() && s.n.unwrap_or(0) < 2 {
                    problems.push(format!("{ctx}: a tournament needs over or n >= 2"));
                }
            }
            StepKind::Until => {
                if s.dedupe_by.is_empty() {
                    problems.push(format!("{ctx}: an until step needs dedupe_by"));
                }
                if s.rounds_without_new == 0 || s.max_rounds == 0 {
                    problems.push(format!(
                        "{ctx}: rounds_without_new and max_rounds must be at least 1"
                    ));
                }
            }
            StepKind::Route => {
                if s.branches.is_empty() {
                    problems.push(format!("{ctx}: a route step needs branches"));
                }
                for (label, targets) in &s.branches {
                    for t in targets {
                        match doc.step_index(t) {
                            None => problems
                                .push(format!("{ctx}: branch {label:?} names unknown step {t:?}")),
                            Some(j) if j <= i => problems.push(format!(
                                "{ctx}: branch {label:?} step {t:?} must come after the route step"
                            )),
                            Some(_) => {}
                        }
                    }
                }
            }
            _ => {}
        }
        if s.kind != StepKind::Route && !s.branches.is_empty() {
            problems.push(format!("{ctx}: branches belong to a route step"));
        }
        if s.kind != StepKind::Tournament && (s.judge.is_some() || s.n.is_some()) {
            problems.push(format!("{ctx}: judge and n belong to a tournament step"));
        }
        // references
        if let Some(r) = &s.result
            && !doc.schemas.contains_key(r)
        {
            problems.push(format!("{ctx}: result names unknown schema {r:?}"));
        }
        if let Some(v) = &s.verify
            && let Some(r) = &v.result
            && !doc.schemas.contains_key(r)
        {
            problems.push(format!("{ctx}: verify.result names unknown schema {r:?}"));
        }
        let per_item =
            s.kind.per_item() || s.kind == StepKind::Tournament || s.kind == StepKind::Until;
        let check_path = |p: &PathExpr, what: &str, problems: &mut Vec<String>| match &p.root {
            Root::Args(name) => {
                if !doc.args.contains_key(name) {
                    problems.push(format!("{ctx}: {what}: unknown arg {name:?}"));
                }
            }
            Root::Step(id) => {
                if !earlier.contains(&id.as_str()) {
                    problems.push(format!("{ctx}: {what}: {id:?} is not an earlier step"));
                }
            }
            Root::Item | Root::Index => {
                if !per_item && what != "verify" {
                    problems.push(format!(
                            "{ctx}: {what}: item and index exist only in fanout, pipeline, tournament and until steps"
                        ));
                }
            }
        };
        if let Some(Over::Path(p)) = &s.over {
            if matches!(p.root, Root::Item | Root::Index) {
                problems.push(format!("{ctx}: over cannot be item or index"));
            } else {
                check_path(p, "over", &mut problems);
            }
        }
        if let Some(p) = &s.input {
            check_path(p, "input", &mut problems);
        }
        for (k, v) in &s.args {
            if let Value::String(t) = v {
                for ph in placeholders(t) {
                    if let Ok(p) = PathExpr::parse(&ph) {
                        check_path(&p, &format!("args.{k}"), &mut problems);
                    }
                }
            }
        }
        if let Some(Actor::Prompt(t)) = &s.actor {
            for ph in placeholders(t) {
                if let Ok(p) = PathExpr::parse(&ph) {
                    check_path(&p, "prompt", &mut problems);
                }
            }
        }
        if let Some(h) = &s.harness
            && !matches!(h.as_str(), "claude" | "codex" | "agy")
        {
            problems.push(format!("{ctx}: unknown harness {h:?}"));
        }
        if let Some(h) = &s.harness
            && !doc.harness.allows(h)
        {
            problems.push(format!(
                "{ctx}: harness {h:?} is not allowed by workflow.harness"
            ));
        }
        // skills
        if let Some(known) = skills {
            let mut actors: Vec<(&Actor, &str)> = Vec::new();
            if let Some(a) = &s.actor {
                actors.push((a, "skill"));
            }
            if let Some(v) = &s.verify {
                actors.push((&v.actor, "verify.skill"));
            }
            if let Some(j) = &s.judge {
                actors.push((j, "judge"));
            }
            for (a, what) in actors {
                if let Actor::Skill(name) = a {
                    match known.iter().find(|k| k.name == *name) {
                        None => {
                            problems.push(format!("{ctx}: {what}: unknown step skill {name:?}"))
                        }
                        Some(k) if k.writes && what == "skill" => {
                            let iso = s.isolation.or(doc.default_isolation).unwrap_or_default();
                            if iso != Isolation::Worktree {
                                problems.push(format!(
                                    "{ctx}: {name:?} edits files (writes = true) and must run with isolation = \"worktree\""
                                ));
                            }
                        }
                        Some(_) => {}
                    }
                }
            }
        }
    }
    if doc.step(&doc.output).is_none() {
        problems.push(format!(
            "workflow.output names unknown step {:?}",
            doc.output
        ));
    }
    for (name, schema) in &doc.schemas {
        for (field, f) in &schema.fields {
            if !PRIMITIVE_TYPES.contains(&f.ty.as_str()) {
                problems.push(format!("schema {name}.{field}: unknown type {:?}", f.ty));
            }
            if let Some(items) = &f.items
                && !PRIMITIVE_TYPES.contains(&items.as_str())
                && !doc.schemas.contains_key(items)
            {
                problems.push(format!(
                    "schema {name}.{field}: items names unknown schema {items:?}"
                ));
            }
        }
    }
    problems
}

// ---- schemas -----------------------------------------------------------------

impl Schema {
    /// The JSON Schema handed to the model in the context file.
    pub fn to_json_schema(&self, all: &BTreeMap<String, Schema>) -> Value {
        self.render(all, 0)
    }

    fn render(&self, all: &BTreeMap<String, Schema>, depth: usize) -> Value {
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for (name, f) in &self.fields {
            let mut p = serde_json::Map::new();
            p.insert("type".into(), Value::String(f.ty.clone()));
            if let Some(d) = &f.description {
                p.insert("description".into(), Value::String(d.clone()));
            }
            if let Some(e) = &f.enum_values {
                p.insert("enum".into(), Value::Array(e.clone()));
            }
            if f.ty == "array" {
                let items = match f.items.as_deref() {
                    Some(t) if PRIMITIVE_TYPES.contains(&t) => {
                        serde_json::json!({ "type": t })
                    }
                    Some(other) if depth < 8 => all
                        .get(other)
                        .map(|s| s.render(all, depth + 1))
                        .unwrap_or_else(|| serde_json::json!({})),
                    _ => serde_json::json!({}),
                };
                p.insert("items".into(), items);
            }
            if f.required {
                required.push(Value::String(name.clone()));
            }
            props.insert(name.clone(), Value::Object(p));
        }
        serde_json::json!({
            "type": "object",
            "properties": Value::Object(props),
            "required": Value::Array(required),
        })
    }

    /// Problems with `value` against this schema.
    pub fn check(&self, value: &Value, all: &BTreeMap<String, Schema>) -> Vec<String> {
        let mut problems = Vec::new();
        self.check_at(value, all, "", &mut problems, 0);
        problems
    }

    fn check_at(
        &self,
        value: &Value,
        all: &BTreeMap<String, Schema>,
        path: &str,
        problems: &mut Vec<String>,
        depth: usize,
    ) {
        let Some(obj) = value.as_object() else {
            problems.push(format!(
                "{}: expected an object",
                if path.is_empty() { "result" } else { path }
            ));
            return;
        };
        for (name, f) in &self.fields {
            let here = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}.{name}")
            };
            let Some(v) = obj.get(name) else {
                if f.required {
                    problems.push(format!("{here}: missing"));
                }
                continue;
            };
            if v.is_null() && !f.required {
                continue;
            }
            let ok = match f.ty.as_str() {
                "string" => v.is_string(),
                "integer" => v.is_i64() || v.is_u64(),
                "number" => v.is_number(),
                "boolean" => v.is_boolean(),
                "array" => v.is_array(),
                "object" => v.is_object(),
                _ => true,
            };
            if !ok {
                problems.push(format!("{here}: expected {}", f.ty));
                continue;
            }
            if let Some(e) = &f.enum_values
                && !e.contains(v)
            {
                problems.push(format!("{here}: not one of the allowed values"));
            }
            if f.ty == "array"
                && let Some(items) = &f.items
                && let Some(arr) = v.as_array()
            {
                for (i, el) in arr.iter().enumerate() {
                    let at = format!("{here}[{i}]");
                    if PRIMITIVE_TYPES.contains(&items.as_str()) {
                        let ok = match items.as_str() {
                            "string" => el.is_string(),
                            "integer" => el.is_i64() || el.is_u64(),
                            "number" => el.is_number(),
                            "boolean" => el.is_boolean(),
                            "array" => el.is_array(),
                            "object" => el.is_object(),
                            _ => true,
                        };
                        if !ok {
                            problems.push(format!("{at}: expected {items}"));
                        }
                    } else if depth < 8
                        && let Some(s) = all.get(items)
                    {
                        s.check_at(el, all, &at, problems, depth + 1);
                    }
                }
            }
        }
    }
}

// ---- paths -------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Root {
    /// `args.<name>`
    Args(String),
    /// `<step>` or `steps.<step>.result`
    Step(String),
    /// `item`
    Item,
    /// `index`
    Index,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Field(String),
    /// `[*]`: flatten one level
    Flatten,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathExpr {
    pub root: Root,
    pub segs: Vec<Seg>,
}

impl fmt::Display for PathExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.root {
            Root::Args(n) => write!(f, "args.{n}")?,
            Root::Step(s) => write!(f, "{s}")?,
            Root::Item => write!(f, "item")?,
            Root::Index => write!(f, "index")?,
        }
        for s in &self.segs {
            match s {
                Seg::Field(n) => write!(f, ".{n}")?,
                Seg::Flatten => write!(f, "[*]")?,
            }
        }
        Ok(())
    }
}

impl PathExpr {
    pub fn parse(text: &str) -> Result<PathExpr, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty path".into());
        }
        // tokens: identifiers, '.', '[*]'
        let mut parts: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '.' => {
                    if cur.is_empty() {
                        return Err(format!("{text:?}: empty segment"));
                    }
                    parts.push(std::mem::take(&mut cur));
                }
                '[' => {
                    if chars.next() != Some('*') || chars.next() != Some(']') {
                        return Err(format!("{text:?}: only [*] is allowed in brackets"));
                    }
                    if !cur.is_empty() {
                        parts.push(std::mem::take(&mut cur));
                    }
                    parts.push("[*]".into());
                    if chars.peek() == Some(&'.') {
                        chars.next();
                        if chars.peek().is_none() {
                            return Err(format!("{text:?}: trailing dot"));
                        }
                    }
                }
                c if c.is_ascii_alphanumeric() || c == '_' || c == '-' => cur.push(c),
                c => return Err(format!("{text:?}: unexpected {c:?}")),
            }
        }
        if !cur.is_empty() {
            parts.push(cur);
        }
        if parts.is_empty() || parts[0] == "[*]" {
            return Err(format!("{text:?}: a path starts with a name"));
        }
        let mut idx = 1;
        let root = match parts[0].as_str() {
            "args" => {
                let name = parts
                    .get(1)
                    .filter(|p| *p != "[*]")
                    .ok_or_else(|| format!("{text:?}: args needs a name"))?;
                idx = 2;
                Root::Args(name.clone())
            }
            "steps" => {
                let name = parts
                    .get(1)
                    .filter(|p| *p != "[*]")
                    .ok_or_else(|| format!("{text:?}: steps needs a step id"))?;
                idx = 2;
                if parts.get(2).map(String::as_str) == Some("result") {
                    idx = 3;
                }
                Root::Step(name.clone())
            }
            "item" => Root::Item,
            "index" => Root::Index,
            other => Root::Step(other.to_string()),
        };
        let segs = parts[idx..]
            .iter()
            .map(|p| {
                if p == "[*]" {
                    Seg::Flatten
                } else {
                    Seg::Field(p.clone())
                }
            })
            .collect();
        Ok(PathExpr { root, segs })
    }
}

/// The values a path resolves against.
#[derive(Debug, Clone)]
pub struct Env<'a> {
    pub args: &'a Value,
    /// Step id → result.
    pub steps: &'a BTreeMap<String, Value>,
    pub item: Option<&'a Value>,
    pub index: Option<usize>,
}

/// Resolves a path; a missing field yields `Null`, a flatten over a
/// non-array yields the value itself wrapped as needed.
pub fn resolve(path: &PathExpr, env: &Env<'_>) -> Value {
    let null = Value::Null;
    let mut cur: Value = match &path.root {
        Root::Args(n) => env.args.get(n).cloned().unwrap_or(Value::Null),
        Root::Step(s) => env.steps.get(s).cloned().unwrap_or(Value::Null),
        Root::Item => env.item.unwrap_or(&null).clone(),
        Root::Index => env.index.map(Value::from).unwrap_or(Value::Null),
    };
    for seg in &path.segs {
        cur = match seg {
            Seg::Field(n) => match cur {
                Value::Array(items) => Value::Array(
                    items
                        .into_iter()
                        .map(|v| v.get(n).cloned().unwrap_or(Value::Null))
                        .collect(),
                ),
                other => other.get(n).cloned().unwrap_or(Value::Null),
            },
            Seg::Flatten => match cur {
                Value::Array(items) => {
                    let mut out = Vec::new();
                    for v in items {
                        match v {
                            Value::Array(inner) => out.extend(inner),
                            Value::Null => {}
                            other => out.push(other),
                        }
                    }
                    Value::Array(out)
                }
                Value::Null => Value::Array(Vec::new()),
                other => Value::Array(vec![other]),
            },
        };
    }
    cur
}

/// Every `{path}` in `text`, in order, without duplicates.
pub fn placeholders(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        let inner = after[..end].trim();
        if !inner.is_empty()
            && inner.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '[' | '*' | ']')
            })
            && !out.iter().any(|o| o == inner)
        {
            out.push(inner.to_string());
        }
        rest = &after[end + 1..];
    }
    out
}

/// A value as it appears inside a prompt: scalars bare, structures as JSON.
pub fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// Replaces every `{path}` with its value.
pub fn interpolate(text: &str, env: &Env<'_>) -> String {
    let mut out = text.to_string();
    for ph in placeholders(text) {
        if let Ok(p) = PathExpr::parse(&ph) {
            let v = resolve(&p, env);
            out = out.replace(&format!("{{{ph}}}"), &value_text(&v));
        }
    }
    out
}

// ---- predicates ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    Cmp {
        field: Vec<String>,
        op: Op,
        value: Value,
    },
    Has(Vec<String>),
    Not(Box<Predicate>),
    And(Box<Predicate>, Box<Predicate>),
    Or(Box<Predicate>, Box<Predicate>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Predicate {
    pub fn parse(text: &str) -> Result<Predicate, String> {
        let tokens = tokenize(text)?;
        let mut pos = 0;
        let p = parse_or(&tokens, &mut pos)?;
        if pos != tokens.len() {
            return Err(format!("{text:?}: unexpected {:?}", tokens[pos]));
        }
        Ok(p)
    }

    /// Evaluates against an object (an item result or an aggregated vote).
    pub fn eval(&self, value: &Value) -> bool {
        match self {
            Predicate::Cmp {
                field,
                op,
                value: rhs,
            } => {
                let lhs = get_path(value, field);
                compare(lhs, *op, rhs)
            }
            Predicate::Has(field) => !get_path(value, field).is_null(),
            Predicate::Not(p) => !p.eval(value),
            Predicate::And(a, b) => a.eval(value) && b.eval(value),
            Predicate::Or(a, b) => a.eval(value) || b.eval(value),
        }
    }
}

fn get_path<'a>(value: &'a Value, field: &[String]) -> &'a Value {
    let mut cur = value;
    for f in field {
        cur = match cur.get(f) {
            Some(v) => v,
            None => return &Value::Null,
        };
    }
    cur
}

fn compare(lhs: &Value, op: Op, rhs: &Value) -> bool {
    use std::cmp::Ordering;
    let ord = match (lhs, rhs) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .zip(b.as_f64())
            .and_then(|(a, b)| a.partial_cmp(&b)),
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        (Value::Null, Value::Null) => Some(Ordering::Equal),
        _ => None,
    };
    match (op, ord) {
        (Op::Eq, Some(Ordering::Equal)) => true,
        (Op::Eq, _) => false,
        (Op::Ne, Some(Ordering::Equal)) => false,
        (Op::Ne, _) => true,
        (Op::Lt, Some(Ordering::Less)) => true,
        (Op::Le, Some(Ordering::Less | Ordering::Equal)) => true,
        (Op::Gt, Some(Ordering::Greater)) => true,
        (Op::Ge, Some(Ordering::Greater | Ordering::Equal)) => true,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Op(Op),
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
    LParen,
    RParen,
    And,
    Or,
    Not,
    Has,
}

fn tokenize(text: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' => i += 1,
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '"' | '\'' => {
                let quote = c;
                let mut s = String::new();
                i += 1;
                while i < chars.len() && chars[i] != quote {
                    s.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(format!("{text:?}: unterminated string"));
                }
                i += 1;
                out.push(Tok::Str(s));
            }
            '=' | '!' | '<' | '>' => {
                let two = chars.get(i + 1) == Some(&'=');
                let op = match (c, two) {
                    ('=', true) => Op::Eq,
                    ('!', true) => Op::Ne,
                    ('<', true) => Op::Le,
                    ('>', true) => Op::Ge,
                    ('<', false) => Op::Lt,
                    ('>', false) => Op::Gt,
                    _ => return Err(format!("{text:?}: bad operator at {i}")),
                };
                out.push(Tok::Op(op));
                i += if two { 2 } else { 1 };
            }
            c if c.is_ascii_digit()
                || (c == '-' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) =>
            {
                let start = i;
                i += 1;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                out.push(Tok::Num(
                    s.parse().map_err(|_| format!("{text:?}: bad number {s}"))?,
                ));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '-' | '.'))
                {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                out.push(match s.as_str() {
                    "and" => Tok::And,
                    "or" => Tok::Or,
                    "not" => Tok::Not,
                    "has" => Tok::Has,
                    "true" => Tok::Bool(true),
                    "false" => Tok::Bool(false),
                    "null" => Tok::Null,
                    _ => Tok::Ident(s),
                });
            }
            other => return Err(format!("{text:?}: unexpected {other:?}")),
        }
    }
    Ok(out)
}

fn parse_or(t: &[Tok], pos: &mut usize) -> Result<Predicate, String> {
    let mut left = parse_and(t, pos)?;
    while t.get(*pos) == Some(&Tok::Or) {
        *pos += 1;
        let right = parse_and(t, pos)?;
        left = Predicate::Or(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_and(t: &[Tok], pos: &mut usize) -> Result<Predicate, String> {
    let mut left = parse_term(t, pos)?;
    while t.get(*pos) == Some(&Tok::And) {
        *pos += 1;
        let right = parse_term(t, pos)?;
        left = Predicate::And(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_term(t: &[Tok], pos: &mut usize) -> Result<Predicate, String> {
    match t.get(*pos) {
        Some(Tok::Not) => {
            *pos += 1;
            Ok(Predicate::Not(Box::new(parse_term(t, pos)?)))
        }
        Some(Tok::LParen) => {
            *pos += 1;
            let p = parse_or(t, pos)?;
            if t.get(*pos) != Some(&Tok::RParen) {
                return Err("expected )".into());
            }
            *pos += 1;
            Ok(p)
        }
        Some(Tok::Has) => {
            *pos += 1;
            if t.get(*pos) != Some(&Tok::LParen) {
                return Err("has needs (field)".into());
            }
            *pos += 1;
            let Some(Tok::Ident(f)) = t.get(*pos) else {
                return Err("has needs a field name".into());
            };
            *pos += 1;
            if t.get(*pos) != Some(&Tok::RParen) {
                return Err("has needs (field)".into());
            }
            *pos += 1;
            Ok(Predicate::Has(f.split('.').map(str::to_string).collect()))
        }
        Some(Tok::Ident(f)) => {
            *pos += 1;
            let Some(Tok::Op(op)) = t.get(*pos) else {
                return Err(format!("{f}: expected a comparison operator"));
            };
            *pos += 1;
            let value = match t.get(*pos) {
                Some(Tok::Str(s)) => Value::String(s.clone()),
                Some(Tok::Num(n)) => serde_json::Number::from_f64(*n)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                Some(Tok::Bool(b)) => Value::Bool(*b),
                Some(Tok::Null) => Value::Null,
                _ => return Err(format!("{f}: expected a value")),
            };
            *pos += 1;
            Ok(Predicate::Cmp {
                field: f.split('.').map(str::to_string).collect(),
                op: *op,
                value,
            })
        }
        other => Err(format!("unexpected {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const EXAMPLE: &str = r#"
[workflow]
name = "review-changes"
description = "Find issues per dimension, refute each one, report the survivors."
output = "report"

[args.scope]
description = "A path or ref range."
default = ""

[schemas.findings]
fields.findings = { type = "array", items = "finding", required = true }
[schemas.finding]
fields.file = { type = "string", required = true }
fields.line = { type = "integer" }
fields.title = { type = "string", required = true }
[schemas.verdict]
fields.refuted = { type = "boolean", required = true }

[[steps]]
id = "find"
kind = "fanout"
phase = "Review"
over = ["correctness", "security"]
skill = "wf-review-find"
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
skill = "wf-synthesize"
input = "confirmed"
"#;

    fn skills() -> Vec<SkillInfo> {
        ["wf-review-find", "wf-refute", "wf-synthesize"]
            .iter()
            .map(|n| SkillInfo {
                name: n.to_string(),
                writes: false,
            })
            .collect()
    }

    #[test]
    fn the_example_parses_and_validates() {
        let doc = parse(EXAMPLE).unwrap();
        assert_eq!(doc.name, "review-changes");
        assert_eq!(doc.output, "report");
        assert_eq!(doc.steps.len(), 3);
        assert_eq!(doc.steps[0].kind, StepKind::Fanout);
        assert_eq!(doc.steps[1].verify.as_ref().unwrap().votes, 3);
        assert_eq!(
            doc.skills(),
            vec!["wf-review-find", "wf-refute", "wf-synthesize"]
        );
        assert_eq!(doc.phases(), vec!["Review", "confirmed", "report"]);
        assert!(
            validate(&doc, Some(&skills())).is_empty(),
            "{:?}",
            validate(&doc, Some(&skills()))
        );
        let js = doc.schemas["findings"].to_json_schema(&doc.schemas);
        assert_eq!(js["properties"]["findings"]["items"]["required"][0], "file");
    }

    #[test]
    fn validation_catches_the_error_classes() {
        let text = r#"
[workflow]
name = "Bad Name"
description = ""
harness = "gemini"
output = "nope"

[[steps]]
id = "a"
kind = "fanout"
result = "missing"
harness = "codex"

[[steps]]
id = "a"
kind = "until"
skill = "x"

[[steps]]
id = "r"
kind = "route"
skill = "wf-classify"
branches = { bug = ["a", "zzz"] }

[[steps]]
id = "t"
kind = "tournament"
input = "later"

[[steps]]
id = "later"
prompt = "look at {item} and {args.nothing}"
"#;
        let doc = parse(text).unwrap();
        let p = validate(
            &doc,
            Some(&[SkillInfo {
                name: "wf-classify".into(),
                writes: false,
            }]),
        );
        let has = |s: &str| p.iter().any(|x| x.contains(s));
        assert!(has("workflow.name"), "{p:?}");
        assert!(has("description is empty"));
        assert!(has("unknown harness \"gemini\""));
        assert!(has("output names unknown step"));
        assert!(has("fanout step needs skill or prompt"));
        assert!(has("fanout step needs over"));
        assert!(has("unknown schema \"missing\""));
        assert!(has("duplicate id"));
        assert!(has("until step needs dedupe_by"));
        assert!(has("unknown step skill \"x\""));
        assert!(has("unknown step \"zzz\""));
        assert!(has("must come after the route step"));
        assert!(has("needs judge"));
        assert!(has("needs over or n"));
        assert!(has("not an earlier step"));
        assert!(has("item and index exist only"));
        assert!(has("unknown arg \"nothing\""));
        assert!(has("not allowed by workflow.harness"), "{p:?}");
    }

    #[test]
    fn writing_skills_need_isolation() {
        let text = r#"
[workflow]
name = "m"
description = "d"
[[steps]]
id = "t"
kind = "fanout"
over = ["a"]
skill = "wf-transform"
"#;
        let doc = parse(text).unwrap();
        let skills = [SkillInfo {
            name: "wf-transform".into(),
            writes: true,
        }];
        let p = validate(&doc, Some(&skills));
        assert!(p.iter().any(|x| x.contains("isolation")), "{p:?}");
        let text2 = text.replace(
            "skill = \"wf-transform\"",
            "skill = \"wf-transform\"\nisolation = \"worktree\"",
        );
        let doc = parse(&text2).unwrap();
        assert!(validate(&doc, Some(&skills)).is_empty());
    }

    #[test]
    fn parse_errors_are_reported_not_panicked() {
        assert!(parse("[workflow\n").unwrap_err()[0].contains("workflow.toml"));
        let both = "[workflow]\nname=\"a\"\ndescription=\"d\"\n[[steps]]\nid=\"s\"\nskill=\"x\"\nprompt=\"y\"\n";
        assert!(parse(both).unwrap_err()[0].contains("not both"));
        let unknown = "[workflow]\nname=\"a\"\ndescription=\"d\"\n[[steps]]\nid=\"s\"\nbogus=1\n";
        assert!(parse(unknown).unwrap_err()[0].contains("bogus"));
        let bad_over =
            "[workflow]\nname=\"a\"\ndescription=\"d\"\n[[steps]]\nid=\"s\"\nover=\"a[1]\"\n";
        assert!(parse(bad_over).unwrap_err()[0].contains("[*]"));
    }

    #[test]
    fn paths_resolve_and_flatten() {
        let steps: BTreeMap<String, Value> = [(
            "find".to_string(),
            serde_json::json!([
                { "findings": [ { "file": "a.rs", "line": 1 }, { "file": "b.rs" } ] },
                null,
                { "findings": [ { "file": "c.rs" } ] }
            ]),
        )]
        .into_iter()
        .collect();
        let args = serde_json::json!({ "scope": "src" });
        let item = serde_json::json!({ "file": "x.rs" });
        let env = Env {
            args: &args,
            steps: &steps,
            item: Some(&item),
            index: Some(3),
        };
        let all = resolve(&PathExpr::parse("find[*].findings[*]").unwrap(), &env);
        assert_eq!(all.as_array().unwrap().len(), 3);
        assert_eq!(
            resolve(&PathExpr::parse("find[*].findings[*].file").unwrap(), &env),
            serde_json::json!(["a.rs", "b.rs", "c.rs"])
        );
        assert_eq!(
            resolve(&PathExpr::parse("args.scope").unwrap(), &env),
            "src"
        );
        assert_eq!(
            resolve(&PathExpr::parse("item.file").unwrap(), &env),
            "x.rs"
        );
        assert_eq!(resolve(&PathExpr::parse("index").unwrap(), &env), 3);
        assert_eq!(
            resolve(&PathExpr::parse("steps.find.result").unwrap(), &env)
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            resolve(&PathExpr::parse("nope.x").unwrap(), &env),
            Value::Null
        );
        assert_eq!(
            interpolate("dim {item.file} in {args.scope} #{index}", &env),
            "dim x.rs in src #3"
        );
        assert_eq!(
            PathExpr::parse("find[*].findings[*]").unwrap().to_string(),
            "find[*].findings[*]"
        );
        assert_eq!(
            placeholders("{a} {b.c} {a} {not a path!}"),
            vec!["a", "b.c"]
        );
    }

    #[test]
    fn predicates_parse_and_evaluate() {
        let v = serde_json::json!({ "refuted": 1, "file": "a.rs", "nested": { "ok": true }, "name": "x" });
        let t = |s: &str| Predicate::parse(s).unwrap().eval(&v);
        assert!(t("refuted < 2"));
        assert!(!t("refuted >= 2"));
        assert!(t("file == 'a.rs' and refuted <= 1"));
        assert!(t("file == \"b.rs\" or has(nested.ok)"));
        assert!(t("not has(missing)"));
        assert!(t("nested.ok == true"));
        assert!(t("(refuted == 1) and (name != 'y')"));
        assert!(!t("missing > 0"), "null compares false");
        assert!(Predicate::parse("refuted <").is_err());
        assert!(Predicate::parse("refuted < 2 junk").is_err());
        assert!(Predicate::parse("has refuted").is_err());
    }

    #[test]
    fn schema_check_reports_each_problem() {
        let doc = parse(EXAMPLE).unwrap();
        let s = &doc.schemas["findings"];
        let good = serde_json::json!({ "findings": [ { "file": "a", "title": "t", "line": 3 } ] });
        assert!(s.check(&good, &doc.schemas).is_empty());
        let bad = serde_json::json!({ "findings": [ { "file": 1 }, "x" ] });
        let p = s.check(&bad, &doc.schemas);
        assert!(
            p.iter().any(|x| x == "findings[0].file: expected string"),
            "{p:?}"
        );
        assert!(p.iter().any(|x| x == "findings[0].title: missing"), "{p:?}");
        assert!(
            p.iter()
                .any(|x| x.starts_with("findings[1]: expected an object")),
            "{p:?}"
        );
        assert_eq!(
            s.check(&serde_json::json!({}), &doc.schemas),
            vec!["findings: missing"]
        );
        assert_eq!(
            s.check(&serde_json::json!("text"), &doc.schemas),
            vec!["result: expected an object"]
        );
    }
}
