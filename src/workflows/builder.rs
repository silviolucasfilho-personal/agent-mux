//! The flow builder's model: a workflow document edited one field at a
//! time. The draft is the document's own TOML table, so a document opened
//! from the library keeps every key the builder has no field for, and what
//! the builder writes is an ordinary workflow (`document::parse`). Steps
//! are described the way the builder shows them: how a step runs
//! (`Runs`), what it reads (`sources`), and the shape of its answer
//! (`FieldKind`), in words rather than in TOML.

use super::document::{self, Workflow};
use toml::{Table, Value};

/// How a step runs, in the builder's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runs {
    Once,
    Parallel,
    EachThenCheck,
    PickPath,
    BestOf,
    UntilQuiet,
}

impl Runs {
    pub const ALL: [Runs; 6] = [
        Runs::Once,
        Runs::Parallel,
        Runs::EachThenCheck,
        Runs::PickPath,
        Runs::BestOf,
        Runs::UntilQuiet,
    ];

    pub fn kind(self) -> &'static str {
        match self {
            Runs::Once => "single",
            Runs::Parallel => "fanout",
            Runs::EachThenCheck => "pipeline",
            Runs::PickPath => "route",
            Runs::BestOf => "tournament",
            Runs::UntilQuiet => "until",
        }
    }

    pub fn from_kind(kind: &str) -> Runs {
        match kind {
            "fanout" => Runs::Parallel,
            "pipeline" => Runs::EachThenCheck,
            "route" => Runs::PickPath,
            "tournament" => Runs::BestOf,
            "until" => Runs::UntilQuiet,
            _ => Runs::Once,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Runs::Once => "Once",
            Runs::Parallel => "In parallel, for each",
            Runs::EachThenCheck => "For each, then check",
            Runs::PickPath => "Pick a path",
            Runs::BestOf => "Best of N",
            Runs::UntilQuiet => "Until nothing new",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Runs::Once => "One session. A brief, a plan, a final report.",
            Runs::Parallel => {
                "One session per item, all at once. Reviewers per lens, readers per folder."
            }
            Runs::EachThenCheck => {
                "Each item on its own: optional work, then independent votes that keep or drop it."
            }
            Runs::PickPath => "One session classifies; only the branch it names runs.",
            Runs::BestOf => {
                "N attempts, judged in pairs until one wins. Designs, names, approaches."
            }
            Runs::UntilQuiet => {
                "Repeat rounds, dropping repeats, until two rounds find nothing new."
            }
        }
    }

    /// A small drawing of the shape, one string per row.
    pub fn diagram(self) -> &'static [&'static str] {
        match self {
            Runs::Once => &["──●──"],
            Runs::Parallel => &["─┬●", " ├●", " └●"],
            Runs::EachThenCheck => &["●─✓─●", "●─✗", "●─✓─●"],
            Runs::PickPath => &["──◆─ a", "   └─ b"],
            Runs::BestOf => &["●┐", "●┴◆┐", "●┐ ├★", "●┴◆┘"],
            Runs::UntilQuiet => &["●→●→●", " ↺ quiet"],
        }
    }

    /// The step runs once per item of `over`.
    pub fn has_items(self) -> bool {
        matches!(
            self,
            Runs::Parallel | Runs::EachThenCheck | Runs::BestOf | Runs::UntilQuiet
        )
    }
}

/// What a field of an answer holds, in the builder's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    WholeNumber,
    Number,
    YesNo,
    OneOf(Vec<String>),
    ListOfText,
    /// A list of groups of fields: the named schema.
    ListOf(String),
    /// Anything else the builder shows but does not edit.
    Other(String),
}

impl FieldKind {
    pub fn label(&self) -> String {
        match self {
            FieldKind::Text => "text".into(),
            FieldKind::WholeNumber => "whole number".into(),
            FieldKind::Number => "number".into(),
            FieldKind::YesNo => "yes/no".into(),
            FieldKind::OneOf(v) => format!("one of {}", v.len()),
            FieldKind::ListOfText => "list of text".into(),
            FieldKind::ListOf(s) => format!("list of {s}"),
            FieldKind::Other(t) => t.clone(),
        }
    }

    /// The order ←/→ cycles through.
    const CYCLE: [&'static str; 7] = [
        "text",
        "whole number",
        "number",
        "yes/no",
        "one of",
        "list of text",
        "list of items",
    ];

    fn cycle_index(&self) -> usize {
        match self {
            FieldKind::Text | FieldKind::Other(_) => 0,
            FieldKind::WholeNumber => 1,
            FieldKind::Number => 2,
            FieldKind::YesNo => 3,
            FieldKind::OneOf(_) => 4,
            FieldKind::ListOfText => 5,
            FieldKind::ListOf(_) => 6,
        }
    }
}

/// One field of an answer shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldView {
    pub name: String,
    pub kind: FieldKind,
    pub required: bool,
}

/// Something a step can read: a path and how it reads in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: String,
    pub words: String,
    /// The path names many items (it ends in `[*]`).
    pub many: bool,
}

/// A problem or a warning about the draft, for the builder's checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub ok: bool,
    pub warning: bool,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub doc: Table,
}

fn s(v: &str) -> Value {
    Value::String(v.to_string())
}

impl Draft {
    /// A new, empty flow.
    pub fn blank(name: &str) -> Draft {
        let mut wf = Table::new();
        wf.insert("name".into(), s(name));
        wf.insert("description".into(), s(""));
        let mut doc = Table::new();
        doc.insert("workflow".into(), Value::Table(wf));
        doc.insert("steps".into(), Value::Array(Vec::new()));
        Draft { doc }
    }

    /// A draft of an existing document; every key is kept.
    pub fn from_text(text: &str) -> Result<Draft, String> {
        let doc: Table = toml::from_str(text).map_err(|e| format!("workflow.toml: {e}"))?;
        let mut d = Draft { doc };
        if !d.doc.contains_key("steps") {
            d.doc.insert("steps".into(), Value::Array(Vec::new()));
        }
        if !d.doc.contains_key("workflow") {
            d.doc.insert("workflow".into(), Value::Table(Table::new()));
        }
        Ok(d)
    }

    // ---- the flow ------------------------------------------------------------

    fn wf(&self) -> &Table {
        self.doc
            .get("workflow")
            .and_then(Value::as_table)
            .expect("workflow table")
    }

    fn wf_mut(&mut self) -> &mut Table {
        self.doc
            .entry("workflow")
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
            .expect("workflow table")
    }

    pub fn flow_str(&self, key: &str) -> String {
        self.wf().get(key).map(value_text).unwrap_or_default()
    }

    /// Sets a `[workflow]` key; empty text removes it (but `name` and
    /// `description` stay, they are required).
    pub fn set_flow_str(&mut self, key: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() && !matches!(key, "name" | "description") {
            self.wf_mut().remove(key);
        } else {
            self.wf_mut().insert(key.into(), s(text));
        }
    }

    pub fn name(&self) -> String {
        self.flow_str("name")
    }

    pub fn budget(&self) -> Option<i64> {
        self.wf().get("budget_tokens").and_then(Value::as_integer)
    }

    pub fn set_budget(&mut self, text: &str) -> Result<(), String> {
        let t = text.trim().replace(['_', ','], "");
        if t.is_empty() {
            self.wf_mut().remove("budget_tokens");
            return Ok(());
        }
        let n: i64 = t
            .strip_suffix('k')
            .map(|k| k.parse::<i64>().map(|v| v * 1000))
            .unwrap_or_else(|| t.parse::<i64>())
            .map_err(|_| format!("{text:?} is not a number of tokens"))?;
        self.wf_mut()
            .insert("budget_tokens".into(), Value::Integer(n));
        Ok(())
    }

    /// The step whose answer is the run's answer: `output`, else the last.
    pub fn output(&self) -> Option<String> {
        self.wf()
            .get("output")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| self.step_ids().last().cloned())
    }

    /// The arguments as `name=default` pairs, `*` marking a required one.
    pub fn args_text(&self) -> String {
        let Some(args) = self.doc.get("args").and_then(Value::as_table) else {
            return String::new();
        };
        args.iter()
            .map(|(k, v)| {
                let t = v.as_table();
                let req = t
                    .and_then(|t| t.get("required"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let def = t.and_then(|t| t.get("default")).map(value_text);
                match (req, def) {
                    (true, _) => format!("{k}*"),
                    (false, Some(d)) if !d.is_empty() => format!("{k}={d}"),
                    _ => k.clone(),
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Sets the arguments from `name=default, other*, third`: `*` is
    /// required; a description the document had is kept.
    pub fn set_args_text(&mut self, text: &str) -> Result<(), String> {
        let old = self
            .doc
            .get("args")
            .and_then(Value::as_table)
            .cloned()
            .unwrap_or_default();
        let mut out = Table::new();
        for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (name, default) = match part.split_once('=') {
                Some((n, d)) => (n.trim(), Some(d.trim())),
                None => (part, None),
            };
            let (name, required) = match name.strip_suffix('*') {
                Some(n) => (n.trim(), true),
                None => (name, false),
            };
            if !is_id(name) {
                return Err(format!("argument {name:?} must match ^[a-z][a-z0-9_-]*$"));
            }
            let mut spec = old
                .get(name)
                .and_then(Value::as_table)
                .cloned()
                .unwrap_or_default();
            spec.remove("required");
            spec.remove("default");
            if required {
                spec.insert("required".into(), Value::Boolean(true));
            } else {
                spec.insert("default".into(), s(default.unwrap_or("")));
            }
            out.insert(name.to_string(), Value::Table(spec));
        }
        if out.is_empty() {
            self.doc.remove("args");
        } else {
            self.doc.insert("args".into(), Value::Table(out));
        }
        Ok(())
    }

    pub fn arg_names(&self) -> Vec<String> {
        self.doc
            .get("args")
            .and_then(Value::as_table)
            .map(|t| t.keys().cloned().collect())
            .unwrap_or_default()
    }

    // ---- steps ---------------------------------------------------------------

    fn steps(&self) -> &Vec<Value> {
        static EMPTY: Vec<Value> = Vec::new();
        self.doc
            .get("steps")
            .and_then(Value::as_array)
            .unwrap_or(&EMPTY)
    }

    fn steps_mut(&mut self) -> &mut Vec<Value> {
        self.doc
            .entry("steps")
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("steps array")
    }

    pub fn len(&self) -> usize {
        self.steps().len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps().is_empty()
    }

    pub fn step(&self, i: usize) -> &Table {
        self.steps()[i].as_table().expect("step table")
    }

    pub fn step_mut(&mut self, i: usize) -> &mut Table {
        self.steps_mut()[i].as_table_mut().expect("step table")
    }

    pub fn step_ids(&self) -> Vec<String> {
        self.steps()
            .iter()
            .map(|v| {
                v.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    pub fn id(&self, i: usize) -> String {
        self.step_str(i, "id")
    }

    pub fn step_str(&self, i: usize, key: &str) -> String {
        self.step(i).get(key).map(value_text).unwrap_or_default()
    }

    /// Sets a step key from text; empty text removes it.
    pub fn set_step_str(&mut self, i: usize, key: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            self.step_mut(i).remove(key);
        } else {
            self.step_mut(i).insert(key.into(), s(text));
        }
    }

    pub fn runs(&self, i: usize) -> Runs {
        Runs::from_kind(
            self.step(i)
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("single"),
        )
    }

    /// A step id not yet used, from `base`.
    pub fn fresh_id(&self, base: &str) -> String {
        let ids = self.step_ids();
        let base = if is_id(base) { base } else { "step" };
        if !ids.iter().any(|i| i == base) {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base}{n}"))
            .find(|c| !ids.contains(c))
            .expect("an unused id")
    }

    /// Inserts a new step at `at` running as `runs`, with the defaults
    /// that shape needs to be valid, and returns its index.
    pub fn add_step(&mut self, at: usize, id: &str, runs: Runs) -> usize {
        let id = self.fresh_id(id);
        let mut t = Table::new();
        t.insert("id".into(), s(&id));
        let at = at.min(self.len());
        self.steps_mut().insert(at, Value::Table(t));
        self.set_runs(at, runs);
        at
    }

    pub fn remove_step(&mut self, i: usize) {
        let id = self.id(i);
        self.steps_mut().remove(i);
        if self.flow_str("output") == id {
            self.wf_mut().remove("output");
        }
        // a branch that named it
        for j in 0..self.len() {
            if let Some(Value::Table(b)) = self.step_mut(j).get_mut("branches") {
                for (_, v) in b.iter_mut() {
                    if let Value::Array(a) = v {
                        a.retain(|x| x.as_str() != Some(id.as_str()));
                    }
                }
            }
        }
    }

    /// Moves step `i` by `delta` places; returns its new index.
    pub fn move_step(&mut self, i: usize, delta: isize) -> usize {
        let j = (i as isize + delta).clamp(0, self.len() as isize - 1) as usize;
        if i != j {
            let v = self.steps_mut().remove(i);
            self.steps_mut().insert(j, v);
        }
        j
    }

    /// Renames a step and every reference to it (paths, branches, output).
    pub fn rename_step(&mut self, i: usize, new: &str) -> Result<(), String> {
        let new = new.trim();
        let old = self.id(i);
        if new == old {
            return Ok(());
        }
        if !is_id(new) {
            return Err(format!(
                "{new:?}: a step name must match ^[a-z][a-z0-9_-]*$"
            ));
        }
        if self.step_ids().iter().any(|x| x == new) {
            return Err(format!("a step is already called {new:?}"));
        }
        self.step_mut(i).insert("id".into(), s(new));
        let rename = |text: &str| -> String {
            if text == old {
                new.to_string()
            } else if let Some(rest) = text.strip_prefix(&old)
                && (rest.starts_with('[') || rest.starts_with('.'))
            {
                format!("{new}{rest}")
            } else {
                text.to_string()
            }
        };
        for j in 0..self.len() {
            let st = self.step_mut(j);
            for key in ["over", "input"] {
                if let Some(Value::String(p)) = st.get(key).cloned() {
                    st.insert(key.into(), Value::String(rename(&p)));
                }
            }
            if let Some(Value::Table(b)) = st.get_mut("branches") {
                for (_, v) in b.iter_mut() {
                    if let Value::Array(a) = v {
                        for x in a.iter_mut() {
                            if x.as_str() == Some(old.as_str()) {
                                *x = s(new);
                            }
                        }
                    }
                }
            }
        }
        if self.flow_str("output") == old {
            self.wf_mut().insert("output".into(), s(new));
        }
        Ok(())
    }

    /// Changes how a step runs, adding what the new shape needs and
    /// dropping what it cannot use.
    pub fn set_runs(&mut self, i: usize, runs: Runs) {
        // the nearest list of items, a list inside answers before the answers
        let many: Vec<Source> = self.sources(i).into_iter().filter(|x| x.many).collect();
        let default_over = many
            .iter()
            .rev()
            .find(|x| x.path.contains('.') && !x.path.starts_with("args."))
            .or(many.last())
            .map(|x| x.path.clone());
        {
            let st = self.step_mut(i);
            if runs == Runs::Once {
                st.remove("kind");
            } else {
                st.insert("kind".into(), s(runs.kind()));
            }
            let drop: &[&str] = match runs {
                Runs::Once | Runs::PickPath => &[
                    "over",
                    "verify",
                    "judge",
                    "judge_prompt",
                    "n",
                    "rounds_without_new",
                    "max_rounds",
                    "dedupe_by",
                    "take",
                    "keep",
                ],
                Runs::Parallel | Runs::EachThenCheck => &[
                    "judge",
                    "judge_prompt",
                    "n",
                    "rounds_without_new",
                    "max_rounds",
                ],
                Runs::BestOf => &["verify", "rounds_without_new", "max_rounds", "dedupe_by"],
                Runs::UntilQuiet => &["verify", "judge", "judge_prompt", "n"],
            };
            for k in drop {
                st.remove(*k);
            }
            if runs != Runs::PickPath {
                st.remove("branches");
            }
            match runs {
                Runs::Parallel if !st.contains_key("over") => {
                    st.insert("over".into(), Value::Array(Vec::new()));
                }
                Runs::EachThenCheck => {
                    if !st.contains_key("over") {
                        st.insert(
                            "over".into(),
                            default_over
                                .clone()
                                .map(Value::String)
                                .unwrap_or_else(|| Value::Array(Vec::new())),
                        );
                    }
                    if !st.contains_key("verify") {
                        let mut v = Table::new();
                        v.insert("skill".into(), s("wf-refute"));
                        v.insert("votes".into(), Value::Integer(3));
                        v.insert("result".into(), s("verdict"));
                        v.insert("keep".into(), s("refuted < 2"));
                        st.insert("verify".into(), Value::Table(v));
                    }
                }
                Runs::PickPath => {
                    if !st.contains_key("branches") {
                        st.insert("branches".into(), Value::Table(Table::new()));
                    }
                }
                Runs::BestOf => {
                    if !st.contains_key("over") && !st.contains_key("n") {
                        st.insert("n".into(), Value::Integer(3));
                    }
                    if !st.contains_key("judge") && !st.contains_key("judge_prompt") {
                        st.insert("judge".into(), s("wf-judge"));
                    }
                }
                Runs::UntilQuiet if !st.contains_key("dedupe_by") => {
                    st.insert("dedupe_by".into(), Value::Array(vec![s("title")]));
                }
                _ => {}
            }
        }
        match runs {
            Runs::EachThenCheck => self.ensure_verdict_schema(),
            Runs::PickPath => {
                let schema = self.ensure_result_schema(i);
                if !self.fields(&schema).iter().any(|f| f.name == "label") {
                    let _ = self.add_field(&schema, "label");
                    self.toggle_required(&schema, "label");
                }
            }
            _ => {}
        }
    }

    fn ensure_verdict_schema(&mut self) {
        if self.schema_exists("verdict") {
            return;
        }
        self.create_schema("verdict");
        let _ = self.add_field("verdict", "refuted");
        self.set_kind("verdict", "refuted", FieldKind::YesNo);
        self.toggle_required("verdict", "refuted");
        let _ = self.add_field("verdict", "reason");
    }

    /// The items a step runs over, as text: `a, b, c` for a list, else the
    /// source it reads them from.
    pub fn items(&self, i: usize) -> Items {
        match self.step(i).get("over") {
            Some(Value::Array(a)) => Items::List(a.iter().map(value_text).collect()),
            Some(Value::String(p)) => Items::From(p.clone()),
            _ => Items::None,
        }
    }

    pub fn set_items_list(&mut self, i: usize, text: &str) {
        let list: Vec<Value> = split_list(text).into_iter().map(Value::String).collect();
        self.step_mut(i).insert("over".into(), Value::Array(list));
    }

    pub fn set_items_from(&mut self, i: usize, path: &str) {
        self.step_mut(i).insert("over".into(), s(path));
    }

    /// What the step runs: a skill, a prompt, or nothing yet.
    pub fn does(&self, i: usize) -> Does {
        let st = self.step(i);
        if let Some(k) = st.get("skill").and_then(Value::as_str) {
            Does::Skill(k.to_string())
        } else if let Some(p) = st.get("prompt").and_then(Value::as_str) {
            Does::Prompt(p.to_string())
        } else {
            Does::Nothing
        }
    }

    pub fn set_skill(&mut self, i: usize, skill: &str) {
        let st = self.step_mut(i);
        st.remove("prompt");
        st.insert("skill".into(), s(skill));
    }

    pub fn set_prompt(&mut self, i: usize, prompt: &str) {
        let st = self.step_mut(i);
        st.remove("skill");
        if prompt.trim().is_empty() {
            st.remove("prompt");
        } else {
            st.insert("prompt".into(), s(prompt.trim_end()));
        }
    }

    // ---- roles: the step, its voters, its judge ------------------------------

    pub fn agent(&self, i: usize, role: Role) -> Option<String> {
        let st = self.step(i);
        let t = match role {
            Role::Step => Some(st),
            Role::Voters => st.get("verify").and_then(Value::as_table),
            Role::Judge => st.get("judge").and_then(Value::as_table),
        };
        t.and_then(|t| t.get("agent"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    pub fn set_agent(&mut self, i: usize, role: Role, agent: Option<&str>) {
        let st = self.step_mut(i);
        let t: &mut Table = match role {
            Role::Step => st,
            Role::Voters => match st.get_mut("verify").and_then(Value::as_table_mut) {
                Some(t) => t,
                None => return,
            },
            Role::Judge => {
                // `judge = "wf-judge"` becomes a table to carry an agent
                if let Some(Value::String(name)) = st.get("judge").cloned() {
                    let mut t = Table::new();
                    t.insert("skill".into(), s(&name));
                    st.insert("judge".into(), Value::Table(t));
                }
                if !st.contains_key("judge") {
                    let mut t = Table::new();
                    t.insert("skill".into(), s("wf-judge"));
                    st.insert("judge".into(), Value::Table(t));
                }
                st.get_mut("judge")
                    .and_then(Value::as_table_mut)
                    .expect("judge table")
            }
        };
        match agent {
            Some(a) => {
                t.insert("agent".into(), s(a));
            }
            None => {
                t.remove("agent");
            }
        }
    }

    /// `(votes, keep_below)` of a check: `refuted < keep_below` keeps.
    pub fn check_votes(&self, i: usize) -> Option<(i64, i64)> {
        let v = self.step(i).get("verify")?.as_table()?;
        let votes = v.get("votes").and_then(Value::as_integer).unwrap_or(1);
        let keep = v
            .get("keep")
            .and_then(Value::as_str)
            .unwrap_or("")
            .replace(' ', "");
        let num = |t: &str| t.parse::<i64>().ok();
        let below = keep
            .strip_prefix("refuted<=")
            .and_then(num)
            .map(|n| n + 1)
            .or_else(|| keep.strip_prefix("refuted<").and_then(num))
            .or_else(|| {
                keep.strip_prefix("refuted==")
                    .and_then(num)
                    .filter(|n| *n == 0)
                    .map(|_| 1)
            })
            .unwrap_or(votes);
        Some((votes, below))
    }

    /// "fewer than 2 refute it", or "no voter refutes it".
    pub fn keep_words(below: i64) -> String {
        if below <= 1 {
            "no voter refutes it".into()
        } else {
            format!("fewer than {below} refute it")
        }
    }

    /// Sets the number of votes and the keep threshold (`refuted < below`).
    pub fn set_check_votes(&mut self, i: usize, votes: i64, below: i64) {
        let votes = votes.clamp(1, 9);
        let below = below.clamp(1, votes);
        if let Some(Value::Table(v)) = self.step_mut(i).get_mut("verify") {
            v.insert("votes".into(), Value::Integer(votes));
            v.insert("keep".into(), s(&format!("refuted < {below}")));
        }
    }

    /// Adds or removes the independent check of each item.
    pub fn set_checked(&mut self, i: usize, on: bool) {
        if on {
            if !self.step(i).contains_key("verify") {
                let mut v = Table::new();
                v.insert("skill".into(), s("wf-refute"));
                v.insert("votes".into(), Value::Integer(3));
                v.insert("result".into(), s("verdict"));
                v.insert("keep".into(), s("refuted < 2"));
                self.step_mut(i).insert("verify".into(), Value::Table(v));
                self.ensure_verdict_schema();
            }
        } else {
            self.step_mut(i).remove("verify");
        }
    }

    pub fn attempts(&self, i: usize) -> i64 {
        self.step(i)
            .get("n")
            .and_then(Value::as_integer)
            .unwrap_or(3)
    }

    pub fn set_attempts(&mut self, i: usize, n: i64) {
        self.step_mut(i)
            .insert("n".into(), Value::Integer(n.clamp(2, 8)));
    }

    /// The branches of a route as `label: step step; label: step`.
    pub fn branches_text(&self, i: usize) -> String {
        let Some(b) = self.step(i).get("branches").and_then(Value::as_table) else {
            return String::new();
        };
        b.iter()
            .map(|(k, v)| {
                let steps: Vec<String> = v
                    .as_array()
                    .map(|a| a.iter().map(value_text).collect())
                    .unwrap_or_default();
                format!("{k}: {}", steps.join(" "))
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn set_branches_text(&mut self, i: usize, text: &str) -> Result<(), String> {
        let mut out = Table::new();
        for part in text.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let (label, steps) = part
                .split_once(':')
                .ok_or_else(|| format!("{part:?}: write label: step step"))?;
            let steps: Vec<Value> = steps
                .split([' ', ','])
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(s)
                .collect();
            out.insert(label.trim().to_string(), Value::Array(steps));
        }
        self.step_mut(i)
            .insert("branches".into(), Value::Table(out));
        Ok(())
    }

    /// A list key (`dedupe_by`) as `a, b`.
    pub fn list_text(&self, i: usize, key: &str) -> String {
        self.step(i)
            .get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().map(value_text).collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    }

    pub fn set_list(&mut self, i: usize, key: &str, text: &str) {
        let list = split_list(text);
        if list.is_empty() {
            self.step_mut(i).remove(key);
        } else {
            self.step_mut(i).insert(
                key.into(),
                Value::Array(list.into_iter().map(Value::String).collect()),
            );
        }
    }

    // ---- what flows between steps --------------------------------------------

    /// Everything step `i` may read: the flow's arguments, and each earlier
    /// step's answer, its items and the lists inside them.
    pub fn sources(&self, i: usize) -> Vec<Source> {
        let mut out = Vec::new();
        for a in self.arg_names() {
            out.push(Source {
                path: format!("args.{a}"),
                words: format!("the argument {a}"),
                many: false,
            });
        }
        for j in 0..i.min(self.len()) {
            let id = self.id(j);
            let runs = self.runs(j);
            let per_item = matches!(
                runs,
                Runs::Parallel | Runs::EachThenCheck | Runs::UntilQuiet
            );
            let lists: Vec<String> = self
                .step(j)
                .get("result")
                .and_then(Value::as_str)
                .map(|schema| {
                    self.fields(schema)
                        .into_iter()
                        .filter(|f| matches!(f.kind, FieldKind::ListOf(_) | FieldKind::ListOfText))
                        .map(|f| f.name)
                        .collect()
                })
                .unwrap_or_default();
            if per_item {
                let what = if runs == Runs::EachThenCheck {
                    "every item that survived"
                } else {
                    "every answer"
                };
                out.push(Source {
                    path: format!("{id}[*]"),
                    words: format!("{what} from {id}, one at a time"),
                    many: true,
                });
                for f in &lists {
                    out.push(Source {
                        path: format!("{id}[*].{f}[*]"),
                        words: format!("every {} from every {id} session", singular(f)),
                        many: true,
                    });
                }
                out.push(Source {
                    path: id.clone(),
                    words: format!("all of {id}'s answers at once"),
                    many: false,
                });
            } else {
                out.push(Source {
                    path: id.clone(),
                    words: format!("{id}'s answer"),
                    many: false,
                });
                for f in &lists {
                    out.push(Source {
                        path: format!("{id}.{f}[*]"),
                        words: format!("every {} in {id}'s answer", singular(f)),
                        many: true,
                    });
                }
            }
        }
        out
    }

    /// A path in words, when it is one of `sources(i)`.
    pub fn source_words(&self, i: usize, path: &str) -> String {
        self.sources(i)
            .into_iter()
            .find(|x| x.path == path)
            .map(|x| x.words)
            .unwrap_or_else(|| path.to_string())
    }

    // ---- answer shapes ---------------------------------------------------------

    fn schemas_mut(&mut self) -> &mut Table {
        self.doc
            .entry("schemas")
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
            .expect("schemas table")
    }

    pub fn schema_exists(&self, name: &str) -> bool {
        self.doc
            .get("schemas")
            .and_then(Value::as_table)
            .is_some_and(|t| t.contains_key(name))
    }

    fn create_schema(&mut self, name: &str) {
        let mut t = Table::new();
        t.insert("fields".into(), Value::Table(Table::new()));
        self.schemas_mut().insert(name.to_string(), Value::Table(t));
    }

    /// The step's answer shape, created (named after the step) when it has
    /// none.
    pub fn ensure_result_schema(&mut self, i: usize) -> String {
        if let Some(r) = self.step(i).get("result").and_then(Value::as_str) {
            let r = r.to_string();
            if !self.schema_exists(&r) {
                self.create_schema(&r);
            }
            return r;
        }
        let base = self.id(i).replace('-', "_");
        let mut name = base.clone();
        let mut n = 2;
        while self.schema_exists(&name) {
            name = format!("{base}{n}");
            n += 1;
        }
        self.create_schema(&name);
        self.step_mut(i).insert("result".into(), s(&name));
        name
    }

    fn fields_table(&self, schema: &str) -> Option<&Table> {
        self.doc
            .get("schemas")?
            .get(schema)?
            .get("fields")?
            .as_table()
    }

    fn fields_table_mut(&mut self, schema: &str) -> Option<&mut Table> {
        self.doc
            .get_mut("schemas")?
            .get_mut(schema)?
            .as_table_mut()?
            .entry("fields")
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
    }

    pub fn fields(&self, schema: &str) -> Vec<FieldView> {
        let Some(t) = self.fields_table(schema) else {
            return Vec::new();
        };
        t.iter()
            .map(|(name, v)| {
                let f = v.as_table();
                let get = |k: &str| f.and_then(|f| f.get(k)).and_then(Value::as_str);
                let ty = get("type").unwrap_or("string");
                let items = get("items");
                let values: Option<Vec<String>> = f
                    .and_then(|f| f.get("enum"))
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(value_text).collect());
                let kind = match (ty, items, values) {
                    ("string", _, Some(v)) => FieldKind::OneOf(v),
                    ("string", _, None) => FieldKind::Text,
                    ("integer", _, _) => FieldKind::WholeNumber,
                    ("number", _, _) => FieldKind::Number,
                    ("boolean", _, _) => FieldKind::YesNo,
                    ("array", Some("string"), _) | ("array", None, _) => FieldKind::ListOfText,
                    ("array", Some(s), _) if document::PRIMITIVE_TYPES.contains(&s) => {
                        FieldKind::Other(format!("list of {s}"))
                    }
                    ("array", Some(s), _) => FieldKind::ListOf(s.to_string()),
                    (other, _, _) => FieldKind::Other(other.to_string()),
                };
                FieldView {
                    name: name.clone(),
                    kind,
                    required: f
                        .and_then(|f| f.get("required"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                }
            })
            .collect()
    }

    pub fn add_field(&mut self, schema: &str, name: &str) -> Result<(), String> {
        let name = name.trim();
        if !is_field(name) {
            return Err(format!("{name:?}: a field name is letters, digits and _"));
        }
        if !self.schema_exists(schema) {
            self.create_schema(schema);
        }
        let t = self.fields_table_mut(schema).expect("fields");
        if t.contains_key(name) {
            return Err(format!("{schema} already has {name}"));
        }
        let mut f = Table::new();
        f.insert("type".into(), s("string"));
        t.insert(name.to_string(), Value::Table(f));
        Ok(())
    }

    pub fn remove_field(&mut self, schema: &str, name: &str) {
        if let Some(t) = self.fields_table_mut(schema) {
            t.remove(name);
        }
    }

    pub fn toggle_required(&mut self, schema: &str, name: &str) {
        if let Some(Value::Table(f)) = self.fields_table_mut(schema).and_then(|t| t.get_mut(name)) {
            let now = f.get("required").and_then(Value::as_bool).unwrap_or(false);
            if now {
                f.remove("required");
            } else {
                f.insert("required".into(), Value::Boolean(true));
            }
        }
    }

    pub fn set_kind(&mut self, schema: &str, name: &str, kind: FieldKind) {
        // a list of items gets a group of its own, named after the field
        let group = match &kind {
            FieldKind::ListOf(g) if g.is_empty() => {
                let base = singular(name);
                let mut g = base.clone();
                let mut n = 2;
                while self.schema_exists(&g) {
                    g = format!("{base}{n}");
                    n += 1;
                }
                self.create_schema(&g);
                Some(g)
            }
            FieldKind::ListOf(g) => Some(g.clone()),
            _ => None,
        };
        let Some(Value::Table(f)) = self.fields_table_mut(schema).and_then(|t| t.get_mut(name))
        else {
            return;
        };
        let required = f.get("required").cloned();
        let description = f.get("description").cloned();
        f.clear();
        let (ty, items, values): (&str, Option<String>, Option<Vec<String>>) = match &kind {
            FieldKind::Text | FieldKind::Other(_) => ("string", None, None),
            FieldKind::WholeNumber => ("integer", None, None),
            FieldKind::Number => ("number", None, None),
            FieldKind::YesNo => ("boolean", None, None),
            FieldKind::OneOf(v) => ("string", None, Some(v.clone())),
            FieldKind::ListOfText => ("array", Some("string".into()), None),
            FieldKind::ListOf(_) => ("array", group, None),
        };
        f.insert("type".into(), s(ty));
        if let Some(i) = items {
            f.insert("items".into(), Value::String(i));
        }
        if let Some(v) = values {
            f.insert(
                "enum".into(),
                Value::Array(v.into_iter().map(Value::String).collect()),
            );
        }
        if let Some(r) = required {
            f.insert("required".into(), r);
        }
        if let Some(d) = description {
            f.insert("description".into(), d);
        }
    }

    /// The next (`+1`) or previous (`-1`) kind in the cycle.
    pub fn cycle_kind(&mut self, schema: &str, name: &str, delta: isize) {
        let Some(cur) = self.fields(schema).into_iter().find(|f| f.name == name) else {
            return;
        };
        let n = FieldKind::CYCLE.len() as isize;
        let next = (cur.kind.cycle_index() as isize + delta).rem_euclid(n) as usize;
        let kind = match FieldKind::CYCLE[next] {
            "whole number" => FieldKind::WholeNumber,
            "number" => FieldKind::Number,
            "yes/no" => FieldKind::YesNo,
            "one of" => FieldKind::OneOf(match cur.kind {
                FieldKind::OneOf(v) => v,
                _ => vec!["a".into(), "b".into()],
            }),
            "list of text" => FieldKind::ListOfText,
            "list of items" => FieldKind::ListOf(String::new()),
            _ => FieldKind::Text,
        };
        self.set_kind(schema, name, kind);
    }

    /// Renames a field, keeping its place in the shape.
    pub fn rename_field(&mut self, schema: &str, old: &str, new: &str) -> Result<(), String> {
        let new = new.trim();
        if new == old {
            return Ok(());
        }
        if !is_field(new) {
            return Err(format!("{new:?}: a field name is letters, digits and _"));
        }
        let t = self
            .fields_table_mut(schema)
            .ok_or_else(|| format!("no shape {schema}"))?;
        if t.contains_key(new) {
            return Err(format!("{schema} already has {new}"));
        }
        if let Some(v) = t.remove(old) {
            t.insert(new.to_string(), v);
        }
        Ok(())
    }

    // ---- the document ----------------------------------------------------------

    /// The workflow document, keys in the order a reader expects.
    pub fn to_toml(&self) -> String {
        emit(&self.doc)
    }

    /// Parses what `to_toml` writes.
    pub fn parsed(&self) -> Result<Workflow, Vec<String>> {
        document::parse(&self.to_toml())
    }

    /// The flow retold, one sentence per step.
    pub fn sentences(&self) -> Vec<String> {
        (0..self.len())
            .map(|i| {
                let id = self.id(i);
                let who = self
                    .agent(i, Role::Step)
                    .map(|a| format!("{a} "))
                    .unwrap_or_default();
                let what = match self.does(i) {
                    Does::Skill(k) => format!("runs {k}"),
                    Does::Prompt(p) => format!("answers \"{}\"", short(&p, 48)),
                    Does::Nothing => "does nothing yet".into(),
                };
                let reads = self
                    .step(i)
                    .get("input")
                    .and_then(Value::as_str)
                    .map(|p| format!(", reading {}", self.source_words(i, p)))
                    .unwrap_or_default();
                let items = match self.items(i) {
                    Items::List(v) if !v.is_empty() => v.join(", "),
                    Items::From(p) => self.source_words(i, &p),
                    _ => "its items".into(),
                };
                match self.runs(i) {
                    Runs::Once => format!("{id}: {who}{what} once{reads}."),
                    Runs::Parallel => {
                        format!("{id}: {who}{what} for each of {items}, all at once{reads}.")
                    }
                    Runs::EachThenCheck => {
                        let check = self
                            .check_votes(i)
                            .map(|(v, b)| {
                                let voters = self
                                    .agent(i, Role::Voters)
                                    .map(|a| format!(" ({a})"))
                                    .unwrap_or_default();
                                format!(
                                    ", then {v} independent vote(s){voters}; it stays when {}",
                                    Draft::keep_words(b)
                                )
                            })
                            .unwrap_or_default();
                        format!("{id}: for each of {items}{check}.")
                    }
                    Runs::PickPath => {
                        format!(
                            "{id}: {who}{what} and picks a path: {}.",
                            self.branches_text(i)
                        )
                    }
                    Runs::BestOf => format!(
                        "{id}: {who}{what} {} times; a judge picks the best, pair by pair.",
                        self.attempts(i)
                    ),
                    Runs::UntilQuiet => format!(
                        "{id}: {who}{what} in rounds until nothing new turns up (same {}).",
                        self.list_text(i, "dedupe_by")
                    ),
                }
            })
            .collect()
    }
}

/// Whose agent: the step's own, its voters' or its judge's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Step,
    Voters,
    Judge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Items {
    None,
    List(Vec<String>),
    From(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Does {
    Nothing,
    Skill(String),
    Prompt(String),
}

pub fn is_id(id: &str) -> bool {
    let mut c = id.chars();
    matches!(c.next(), Some(x) if x.is_ascii_lowercase())
        && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '-' || x == '_')
}

fn is_field(name: &str) -> bool {
    let mut c = name.chars();
    matches!(c.next(), Some(x) if x.is_ascii_alphabetic() || x == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

fn split_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}

/// `findings` → `finding`, for words and group names.
pub fn singular(word: &str) -> String {
    word.strip_suffix("ies")
        .map(|w| format!("{w}y"))
        .or_else(|| {
            word.strip_suffix('s')
                .filter(|w| !w.ends_with('s'))
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("{word}_item"))
}

fn short(text: &str, max: usize) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > max {
        format!("{}…", one.chars().take(max).collect::<String>())
    } else {
        one
    }
}

/// A value as a person reads it: strings bare, others as TOML.
pub fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---- emitting -------------------------------------------------------------------

const WORKFLOW_ORDER: &[&str] = &[
    "name",
    "description",
    "when_to_use",
    "harness",
    "output",
    "default_isolation",
    "budget_tokens",
    "max_concurrent",
    "accept",
];

const STEP_ORDER: &[&str] = &[
    "id",
    "kind",
    "phase",
    "over",
    "skill",
    "prompt",
    "agent",
    "args",
    "input",
    "result",
    "keep",
    "dedupe_by",
    "take",
    "verify",
    "n",
    "judge",
    "judge_prompt",
    "branches",
    "rounds_without_new",
    "max_rounds",
    "harness",
    "profile",
    "model",
    "effort",
    "isolation",
    "cwd",
    "timeout_s",
    "concurrency",
];

fn ordered<'a>(t: &'a Table, order: &[&str]) -> Vec<(&'a String, &'a Value)> {
    let mut out: Vec<(&String, &Value)> = Vec::new();
    for k in order {
        if let Some((key, v)) = t.get_key_value(*k) {
            out.push((key, v));
        }
    }
    for (k, v) in t {
        if !order.contains(&k.as_str()) {
            out.push((k, v));
        }
    }
    out
}

fn key(k: &str) -> String {
    if k.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !k.is_empty()
    {
        k.to_string()
    } else {
        Value::String(k.to_string()).to_string()
    }
}

/// A value on one line; a multi-line string as a `"""` block.
fn inline(v: &Value) -> String {
    match v {
        Value::String(s) if s.contains('\n') => {
            let body = s.replace('\\', "\\\\").replace("\"\"\"", "\\\"\\\"\\\"");
            format!("\"\"\"\n{body}\"\"\"")
        }
        Value::Table(t) => {
            let parts: Vec<String> = t
                .iter()
                .map(|(k, v)| format!("{} = {}", key(k), inline(v)))
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(inline).collect();
            format!("[{}]", parts.join(", "))
        }
        other => other.to_string(),
    }
}

fn emit(doc: &Table) -> String {
    let mut out = String::new();
    // bare top-level keys come before the first table header
    for (k, v) in doc {
        if !matches!(k.as_str(), "workflow" | "args" | "schemas" | "steps") {
            out.push_str(&format!("{} = {}\n", key(k), inline(v)));
        }
    }
    if let Some(Value::Table(wf)) = doc.get("workflow") {
        out.push_str("[workflow]\n");
        for (k, v) in ordered(wf, WORKFLOW_ORDER) {
            out.push_str(&format!("{} = {}\n", key(k), inline(v)));
        }
    }
    if let Some(Value::Table(args)) = doc.get("args") {
        for (name, spec) in args {
            out.push_str(&format!("\n[args.{}]\n", key(name)));
            if let Value::Table(t) = spec {
                for (k, v) in t {
                    out.push_str(&format!("{} = {}\n", key(k), inline(v)));
                }
            }
        }
    }
    if let Some(Value::Table(schemas)) = doc.get("schemas") {
        for (name, schema) in schemas {
            out.push_str(&format!("\n[schemas.{}]\n", key(name)));
            if let Some(Value::Table(fields)) = schema.get("fields") {
                for (f, v) in fields {
                    out.push_str(&format!("fields.{} = {}\n", key(f), inline(v)));
                }
            }
        }
    }
    if let Some(Value::Array(steps)) = doc.get("steps") {
        for st in steps {
            out.push_str("\n[[steps]]\n");
            if let Value::Table(t) = st {
                for (k, v) in ordered(t, STEP_ORDER) {
                    out.push_str(&format!("{} = {}\n", key(k), inline(v)));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review_flow() -> Draft {
        let mut d = Draft::blank("security-review");
        d.set_flow_str("description", "Review a change");
        d.set_args_text("scope=working tree").unwrap();
        let b = d.add_step(0, "brief", Runs::Once);
        d.set_prompt(b, "Write a brief of {args.scope}.\nKeep it short.");
        d.set_agent(b, Role::Step, Some("sage"));
        let r = d.add_step(1, "review", Runs::Parallel);
        d.set_items_list(r, "security, bugs and edge cases, simplification");
        d.set_skill(r, "wf-review-find");
        d.set_step_str(r, "input", "brief");
        let schema = d.ensure_result_schema(r);
        d.add_field(&schema, "findings").unwrap();
        d.set_kind(&schema, "findings", FieldKind::ListOf(String::new()));
        for f in ["file", "severity", "title"] {
            d.add_field("finding", f).unwrap();
            d.toggle_required("finding", f);
        }
        d.set_kind(
            "finding",
            "severity",
            FieldKind::OneOf(vec!["high".into(), "medium".into(), "low".into()]),
        );
        let c = d.add_step(2, "check", Runs::EachThenCheck);
        d.set_agent(c, Role::Voters, Some("skeptic"));
        let v = d.add_step(3, "verdict", Runs::Once);
        d.set_prompt(v, "Weigh the findings.");
        d.set_step_str(v, "input", "check");
        d
    }

    #[test]
    fn a_built_flow_is_a_valid_workflow() {
        let d = review_flow();
        let text = d.to_toml();
        let doc = document::parse(&text).unwrap_or_else(|p| panic!("{p:?}\n{text}"));
        let problems = document::validate(&doc, None);
        assert!(problems.is_empty(), "{problems:?}\n{text}");
        // each-then-check picked the only list an earlier step gives
        let check = doc.step("check").unwrap();
        assert_eq!(
            check.over,
            Some(document::Over::Path(
                document::PathExpr::parse("review[*].findings[*]").unwrap()
            ))
        );
        assert_eq!(check.verify.as_ref().unwrap().votes, 3);
        assert_eq!(
            check.verify.as_ref().unwrap().runner.agent.as_deref(),
            Some("skeptic")
        );
        assert!(doc.schemas.contains_key("verdict"));
        assert!(text.starts_with("[workflow]\nname = \"security-review\"\n"));
        assert!(text.contains("prompt = \"\"\"\nWrite a brief"), "{text}");
        assert!(text.contains("[[steps]]\nid = \"review\"\nkind = \"fanout\"\nover = "));
    }

    #[test]
    fn sources_say_in_words_what_a_step_can_read() {
        let d = review_flow();
        let words: Vec<(String, String)> = d
            .sources(2)
            .into_iter()
            .map(|x| (x.path, x.words))
            .collect();
        assert!(words.contains(&("args.scope".into(), "the argument scope".into())));
        assert!(words.contains(&("brief".into(), "brief's answer".into())));
        assert!(words.contains(&(
            "review[*].findings[*]".into(),
            "every finding from every review session".into()
        )));
        assert!(words.contains(&("review".into(), "all of review's answers at once".into())));
        assert!(d.sources(0).iter().all(|x| x.path.starts_with("args.")));
    }

    #[test]
    fn renaming_a_step_follows_every_reference() {
        let mut d = review_flow();
        d.rename_step(1, "lenses").unwrap();
        let t = d.to_toml();
        assert!(t.contains("over = \"lenses[*].findings[*]\""), "{t}");
        assert!(d.rename_step(1, "brief").is_err());
        assert!(d.rename_step(1, "Bad").is_err());
        let doc = document::parse(&t).unwrap();
        assert!(document::validate(&doc, None).is_empty());
    }

    #[test]
    fn changing_how_a_step_runs_keeps_it_valid() {
        let mut d = review_flow();
        for runs in Runs::ALL {
            d.set_runs(1, runs);
            if runs == Runs::PickPath {
                d.set_branches_text(1, "big: check; small: verdict")
                    .unwrap();
            }
            if runs == Runs::Once || runs == Runs::PickPath {
                // later steps that read its items now fail; that is the
                // checks' job to say, the document still parses
                assert!(d.parsed().is_ok(), "{runs:?}");
            }
            assert_eq!(d.runs(1), runs);
        }
        d.set_runs(1, Runs::Parallel);
        d.set_items_list(1, "a, b");
        assert_eq!(d.items(1), Items::List(vec!["a".into(), "b".into()]));
        assert!(d.step(1).get("branches").is_none());
    }

    #[test]
    fn answer_shapes_are_edited_field_by_field() {
        let mut d = review_flow();
        let f = d.fields("finding");
        assert_eq!(f.len(), 3);
        assert_eq!(
            f[1].kind,
            FieldKind::OneOf(vec!["high".into(), "medium".into(), "low".into()])
        );
        d.cycle_kind("finding", "file", 1);
        assert_eq!(d.fields("finding")[0].kind, FieldKind::WholeNumber);
        d.cycle_kind("finding", "file", -1);
        assert_eq!(d.fields("finding")[0].kind, FieldKind::Text);
        assert!(
            d.fields("finding")[0].required,
            "a kind change keeps required"
        );
        d.rename_field("finding", "file", "path").unwrap();
        assert!(d.fields("finding").iter().any(|x| x.name == "path"));
        assert!(d.add_field("finding", "path").is_err());
        d.remove_field("finding", "path");
        assert_eq!(d.fields("finding").len(), 2);
    }

    #[test]
    fn opening_a_builtin_keeps_every_key() {
        for (name, text) in super::super::library::BUILTIN {
            let d = Draft::from_text(text).unwrap();
            let again = d.to_toml();
            let a = document::parse(text).unwrap();
            let b = document::parse(&again).unwrap_or_else(|p| panic!("{name}: {p:?}\n{again}"));
            assert_eq!(a, b, "{name} changed on the way through the builder");
        }
    }

    #[test]
    fn arguments_budget_and_votes() {
        let mut d = review_flow();
        d.set_args_text("scope*, language=English").unwrap();
        assert_eq!(d.args_text(), "language=English, scope*");
        assert!(d.set_args_text("Bad").is_err());
        d.set_budget("400k").unwrap();
        assert_eq!(d.budget(), Some(400_000));
        assert!(d.set_budget("lots").is_err());
        d.set_check_votes(2, 5, 3);
        assert_eq!(d.check_votes(2), Some((5, 3)));
        d.set_check_votes(2, 2, 9);
        assert_eq!(d.check_votes(2), Some((2, 2)));
        let s = d.sentences();
        assert!(s[2].contains("2 independent vote(s) (skeptic)"), "{}", s[2]);
        assert!(s[0].starts_with("brief: sage answers"), "{}", s[0]);
    }
}
