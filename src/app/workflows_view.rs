//! The Workflows section's run dialog, and a planned document's detail in
//! the runs view (`app::runs_view`). A flow's run is read on the Run
//! screen (`app::run_screen`), whose Result tab is built by
//! `app::run_report`. Starting, cancelling and saving are `App` methods in
//! `crate::app::workflows`.

use crate::app::dir_picker::DirPicker;
use crate::app::text_area::TextArea;
use crate::config::Profile;
use crate::harness::Harness;
use crate::workflows::document::Isolation;
use crate::workflows::library::Entry;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// What the dialog starts: a run of a document, or the planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogPurpose {
    Run { name: String },
    Plan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogField {
    Task,
    Workspace,
    Profile,
    /// Index into the document's args.
    Arg(usize),
    Budget,
    MaxCost,
    Isolation,
    /// Index into the document's steps: the harness that step runs on.
    StepHarness(usize),
    /// The row that shows or hides budget, cost cap, isolation and the
    /// per-step harness rows.
    More,
}

#[derive(Debug, Clone)]
pub struct WorkflowDialogState {
    pub purpose: DialogPurpose,
    pub document: String,
    pub description: String,
    pub arg_names: Vec<String>,
    pub arg_help: Vec<String>,
    /// One multi-line field per declared argument: an argument is often a
    /// whole prompt, not a word.
    pub args: Vec<TextArea>,
    /// The planner's task: what the composed workflow should do.
    pub task: TextArea,
    pub field: DialogField,
    pub workspace: String,
    pub dir_picker: DirPicker,
    /// `(profile name, harness)` of the eligible profiles.
    pub profiles: Vec<(String, Harness)>,
    pub profile_idx: usize,
    pub budget: TextArea,
    pub max_cost: TextArea,
    pub isolation: Isolation,
    /// The document's step ids, in order: one harness row each.
    pub step_ids: Vec<String>,
    /// The harness picked for each step for this run; `None` keeps the
    /// step's default (`step_defaults`, else the run's profile).
    pub step_harness: Vec<Option<Harness>>,
    /// The harness each step's document names, if any.
    pub step_defaults: Vec<Option<Harness>>,
    pub error: Option<String>,
    pub estimate: String,
    /// The `More` rows are shown.
    pub more: bool,
}

impl WorkflowDialogState {
    pub fn for_run(entry: &Entry, profiles: &[Profile], workspaces: Vec<String>) -> Self {
        let doc = entry.doc.as_ref();
        let allowed = |h: Harness| doc.is_none_or(|d| d.harness.allows(h.as_str()));
        let eligible: Vec<(String, Harness)> = profiles
            .iter()
            .filter_map(|p| Harness::detect(&p.command).map(|h| (p.name.clone(), h)))
            .filter(|(_, h)| allowed(*h))
            .collect();
        let workspace = workspaces.into_iter().next().unwrap_or_default();
        let (arg_names, arg_help, args): (Vec<String>, Vec<String>, Vec<TextArea>) = doc
            .map(|d| {
                let mut n = Vec::new();
                let mut h = Vec::new();
                let mut v = Vec::new();
                for (k, spec) in &d.args {
                    n.push(k.clone());
                    h.push(format!(
                        "{}{}",
                        spec.description.clone().unwrap_or_default(),
                        if spec.required { " (required)" } else { "" }
                    ));
                    v.push(TextArea::new(match &spec.default {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(other) => other.to_string(),
                        None => String::new(),
                    }));
                }
                (n, h, v)
            })
            .unwrap_or_default();
        let estimate = doc.map(estimate_text).unwrap_or_default();
        let first_field = if arg_names.is_empty() {
            DialogField::Workspace
        } else {
            DialogField::Arg(0)
        };
        let step_ids: Vec<String> = doc
            .map(|d| d.steps.iter().map(|s| s.id.clone()).collect())
            .unwrap_or_default();
        let step_defaults: Vec<Option<Harness>> = doc
            .map(|d| {
                d.steps
                    .iter()
                    .map(|s| s.harness.as_deref().and_then(Harness::detect))
                    .collect()
            })
            .unwrap_or_default();
        WorkflowDialogState {
            purpose: DialogPurpose::Run {
                name: entry.name.clone(),
            },
            document: entry.text.clone(),
            description: doc.map(|d| d.description.clone()).unwrap_or_default(),
            arg_names,
            arg_help,
            args,
            task: TextArea::default(),
            // what the run is for comes first: the arguments, if any
            field: first_field,
            dir_picker: DirPicker::for_path(&workspace),
            workspace,
            profiles: eligible,
            profile_idx: 0,
            budget: TextArea::new(
                doc.and_then(|d| d.budget_tokens)
                    .map(|b| b.to_string())
                    .unwrap_or_default(),
            ),
            max_cost: TextArea::default(),
            isolation: doc.and_then(|d| d.default_isolation).unwrap_or_default(),
            step_harness: vec![None; step_ids.len()],
            step_ids,
            step_defaults,
            error: None,
            estimate,
            more: false,
        }
    }

    pub fn for_plan(profiles: &[Profile], workspaces: Vec<String>) -> Self {
        let eligible: Vec<(String, Harness)> = profiles
            .iter()
            .filter_map(|p| Harness::detect(&p.command).map(|h| (p.name.clone(), h)))
            .collect();
        let workspace = workspaces.into_iter().next().unwrap_or_default();
        WorkflowDialogState {
            purpose: DialogPurpose::Plan,
            document: String::new(),
            description:
                "The planner composes a workflow for the task and shows it before it runs.".into(),
            arg_names: Vec::new(),
            arg_help: Vec::new(),
            args: Vec::new(),
            task: TextArea::default(),
            field: DialogField::Task,
            dir_picker: DirPicker::for_path(&workspace),
            workspace,
            profiles: eligible,
            profile_idx: 0,
            budget: TextArea::default(),
            max_cost: TextArea::default(),
            isolation: Isolation::None,
            step_ids: Vec::new(),
            step_harness: Vec::new(),
            step_defaults: Vec::new(),
            error: None,
            estimate: String::new(),
            more: false,
        }
    }

    /// The fields in the order the dialog draws them: for a run, the
    /// arguments, where and on what, then `More` and, when it is open,
    /// the limits and the per-step harness rows.
    pub fn fields(&self) -> Vec<DialogField> {
        let mut v = Vec::new();
        if self.purpose == DialogPurpose::Plan {
            v.push(DialogField::Task);
            v.push(DialogField::Workspace);
            v.push(DialogField::Profile);
            v.push(DialogField::Budget);
            return v;
        }
        for i in 0..self.arg_names.len() {
            v.push(DialogField::Arg(i));
        }
        v.push(DialogField::Workspace);
        v.push(DialogField::Profile);
        v.push(DialogField::More);
        if self.more {
            v.push(DialogField::Budget);
            v.push(DialogField::MaxCost);
            v.push(DialogField::Isolation);
            for i in 0..self.step_ids.len() {
                v.push(DialogField::StepHarness(i));
            }
        }
        v
    }

    /// The `More` row's summary of what it hides.
    pub fn more_summary(&self) -> String {
        let mut parts = vec![
            match self.budget.text.trim() {
                "" => "no token budget".to_string(),
                t => format!("{t} tokens"),
            },
            match self.max_cost.text.trim() {
                "" => "no cost cap".to_string(),
                t => format!("${t} cap"),
            },
            match self.isolation {
                Isolation::None => "in place".to_string(),
                Isolation::Worktree => "in worktrees".to_string(),
            },
        ];
        let changed = self.step_harness.iter().filter(|h| h.is_some()).count();
        if changed > 0 {
            parts.push(format!("{changed} step(s) on another CLI"));
        }
        parts.join(" · ")
    }

    pub fn step_field(&mut self, delta: isize) {
        let fields = self.fields();
        let at = fields.iter().position(|f| *f == self.field).unwrap_or(0) as isize;
        let len = fields.len() as isize;
        self.field = fields[((at + delta).rem_euclid(len)) as usize];
    }

    pub fn cycle(&mut self, delta: isize) {
        match self.field {
            DialogField::Profile => {
                let len = self.profiles.len() as isize;
                if len > 0 {
                    self.profile_idx =
                        ((self.profile_idx as isize + delta).rem_euclid(len)) as usize;
                }
            }
            DialogField::More => self.more = !self.more,
            DialogField::Isolation => {
                self.isolation = match self.isolation {
                    Isolation::None => Isolation::Worktree,
                    Isolation::Worktree => Isolation::None,
                };
            }
            DialogField::StepHarness(i) => {
                // the default, then each harness a profile can launch
                let mut choices: Vec<Option<Harness>> = vec![None];
                choices.extend(
                    Harness::ALL
                        .into_iter()
                        .filter(|h| self.profiles.iter().any(|(_, p)| p == h))
                        .map(Some),
                );
                if let Some(cur) = self.step_harness.get_mut(i) {
                    let at = choices.iter().position(|c| c == cur).unwrap_or(0) as isize;
                    let len = choices.len() as isize;
                    *cur = choices[((at + delta).rem_euclid(len)) as usize];
                }
            }
            _ => {}
        }
    }

    /// What step `i` runs on when its row is left on the default: the
    /// document's harness for it, else the run's profile.
    pub fn step_default_label(&self, i: usize) -> String {
        match self.step_defaults.get(i).copied().flatten() {
            Some(h) => format!("{} · set by the document", h.as_str()),
            None => match self.harness() {
                Some(h) => format!("{} · the run's profile", h.as_str()),
                None => "the run's profile".into(),
            },
        }
    }

    /// The harness rows the user changed, as this run's step overrides
    /// `(step id, "harness", harness)`; rows on their default add none.
    pub fn step_overrides(&self) -> Vec<(String, String, String)> {
        self.step_ids
            .iter()
            .zip(&self.step_harness)
            .filter_map(|(id, h)| {
                h.map(|h| (id.clone(), "harness".to_string(), h.as_str().to_string()))
            })
            .collect()
    }

    /// The multi-line field under the cursor, when the cursor is on one.
    pub fn text_mut(&mut self) -> Option<&mut TextArea> {
        match self.field {
            DialogField::Task => Some(&mut self.task),
            DialogField::Arg(i) => self.args.get_mut(i),
            DialogField::Budget => Some(&mut self.budget),
            DialogField::MaxCost => Some(&mut self.max_cost),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&TextArea> {
        match self.field {
            DialogField::Task => Some(&self.task),
            DialogField::Arg(i) => self.args.get(i),
            DialogField::Budget => Some(&self.budget),
            DialogField::MaxCost => Some(&self.max_cost),
            _ => None,
        }
    }

    /// Rows the focused field gets on screen; the others show one.
    pub fn rows_for(&self, field: DialogField) -> usize {
        if field != self.field {
            return 1;
        }
        match field {
            DialogField::Task => 8,
            DialogField::Arg(_) => 6,
            _ => 1,
        }
    }

    pub fn harness(&self) -> Option<Harness> {
        self.profiles.get(self.profile_idx).map(|(_, h)| *h)
    }

    pub fn profile_name(&self) -> Option<String> {
        self.profiles.get(self.profile_idx).map(|(n, _)| n.clone())
    }

    /// The args as JSON: numbers and JSON literals typed as such.
    pub fn args_value(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        for (k, v) in self.arg_names.iter().zip(&self.args) {
            let v = v.text.trim();
            if v.is_empty() {
                continue;
            }
            // a bare number or JSON literal keeps its type; prose stays a
            // string, newlines and all
            let value = serde_json::from_str::<serde_json::Value>(v)
                .ok()
                .filter(|j| !j.is_string() && !j.is_object())
                .unwrap_or_else(|| serde_json::Value::String(v.to_string()));
            m.insert(k.clone(), value);
        }
        if m.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::Object(m)
        }
    }

    pub fn budget_tokens(&self) -> Result<Option<u64>, String> {
        parse_num(&self.budget.text, "Budget")
    }

    pub fn usd_cap(&self) -> Result<Option<f64>, String> {
        let t = self.max_cost.text.trim();
        if t.is_empty() {
            return Ok(None);
        }
        t.parse::<f64>()
            .map(Some)
            .map_err(|_| format!("Max cost {t:?} is not a number"))
    }
}

fn parse_num(text: &str, what: &str) -> Result<Option<u64>, String> {
    let t = text.trim().replace(['_', ','], "");
    if t.is_empty() {
        return Ok(None);
    }
    let (num, mult) = match t.chars().last() {
        Some('k') | Some('K') => (&t[..t.len() - 1], 1_000),
        Some('m') | Some('M') => (&t[..t.len() - 1], 1_000_000),
        _ => (t.as_str(), 1),
    };
    num.parse::<u64>()
        .map(|n| Some(n * mult))
        .map_err(|_| format!("{what} {text:?} is not a number (try 400k)"))
}

/// What a step does, in words for the preview: how many sessions it
/// starts and over what, then what it filters or checks.
pub fn describe_step(s: &crate::workflows::document::Step) -> String {
    use crate::workflows::document::{Over, Root, StepKind};
    let over = match &s.over {
        Some(Over::Inline(v)) => {
            let words: Vec<&str> = v.iter().filter_map(|x| x.as_str()).collect();
            if words.len() == v.len() && (1..=4).contains(&v.len()) {
                format!("each of: {}", words.join(", "))
            } else {
                format!("each of {} items", v.len())
            }
        }
        Some(Over::Path(p)) => match &p.root {
            Root::Args(n) => format!("each value of the {n} argument"),
            Root::Step(id) => format!("each item from {id}"),
            _ => "each item".into(),
        },
        None => String::new(),
    };
    let mut text = match s.kind {
        StepKind::Single if s.actor.is_none() => "no session: reshapes earlier results".into(),
        StepKind::Single => "one session".into(),
        StepKind::Route => {
            let branches: Vec<&str> = s.branches.keys().map(String::as_str).collect();
            format!(
                "one session sorts the task, then one branch runs ({})",
                branches.join(" | ")
            )
        }
        StepKind::Fanout => format!("one session for {over}"),
        StepKind::Pipeline if s.actor.is_none() => over.clone(),
        StepKind::Pipeline => format!("one session for {over}, each on its own"),
        StepKind::Tournament => format!(
            "{} attempts, judged in pairs; the best one wins",
            s.n.unwrap_or(0)
        ),
        StepKind::Until => format!(
            "repeats until {} rounds find nothing new (at most {})",
            s.rounds_without_new, s.max_rounds
        ),
    };
    if !s.dedupe_by.is_empty() {
        text.push_str("; drops duplicates");
    }
    if s.keep.is_some() {
        text.push_str("; keeps only the items that pass a filter");
    }
    if let Some(t) = s.take {
        text.push_str(&format!("; keeps at most {t}"));
    }
    if let Some(v) = &s.verify {
        text.push_str(&format!(
            "; each checked {}× by independent sessions",
            v.votes
        ));
        if let Some(k) = &v.keep_text {
            text.push_str(&format!(", kept when {k}"));
        }
    }
    text
}

/// About how many sessions a run starts, in words: known counts added up,
/// and the part that depends on what earlier steps find.
pub fn estimate_text(doc: &crate::workflows::document::Workflow) -> String {
    use crate::workflows::document::{Over, StepKind};
    let mut fixed = 0usize;
    let mut per_item = 0usize;
    let mut rounds = false;
    for s in &doc.steps {
        let votes = s.verify.as_ref().map(|v| v.votes).unwrap_or(0);
        let one = usize::from(s.actor.is_some());
        match s.kind {
            StepKind::Single | StepKind::Route => fixed += one + votes,
            StepKind::Fanout | StepKind::Pipeline => match &s.over {
                Some(Over::Inline(v)) => fixed += v.len() * (one + votes),
                _ => per_item += one + votes,
            },
            StepKind::Tournament => {
                let n = s.n.unwrap_or(0);
                fixed += n + n.saturating_sub(1);
            }
            StepKind::Until => rounds = true,
        }
    }
    let mut text = format!("about {fixed} session(s)");
    if per_item > 0 {
        text.push_str(&format!(" + {per_item} for each item found while it runs"));
    }
    if rounds {
        text.push_str(" + one per search round");
    }
    text
}

// ---- runs and plans ---------------------------------------------------------

/// A flow's run or plan, as the Agents list and the inbox name it when
/// they open the runs view on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunRow {
    Header(&'static str),
    /// A planned document awaiting a decision (plan id).
    Planned(String),
    /// A live run (run id).
    Live(String),
    /// A stored run (run id).
    Stored(String),
}

/// A planned document in the runs view's detail pane: the task, where
/// it would run, whether it checks clean, and its text. `document`: the
/// tab shows the document; the others say it has not run.
pub fn plan_lines(
    p: &crate::app::workflows::PlannedWorkflow,
    document: bool,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let key = Style::default().fg(Color::Yellow);
    let red = Style::default().fg(Color::Red);
    let green = Style::default().fg(Color::Green);
    let row = |k: &str, v: String| {
        Line::from(vec![Span::styled(format!("  {k:<10}"), key), Span::raw(v)])
    };
    let mut lines = vec![
        row("Task", p.task.clone()),
        row("Workflow", p.name.clone()),
        row("Harness", p.harness.display_name().to_string()),
        row("Workspace", p.workspace.display().to_string()),
    ];
    match &p.run_id {
        Some(r) => lines.push(row("Run", format!("started {}", &r[..8.min(r.len())]))),
        None if p.valid() => lines.push(Line::from(vec![
            Span::styled("  Status    ", key),
            Span::styled("valid · Enter runs it, e edits, s saves, d discards", green),
        ])),
        None => {
            lines.push(Line::from(vec![
                Span::styled("  Status    ", key),
                Span::styled(format!("{} problem(s)", p.problems.len()), red),
            ]));
            for pr in &p.problems {
                lines.push(Line::styled(format!("            {pr}"), red));
            }
        }
    }
    lines.push(super::run_report::rule());
    if document {
        let text = if p.document.is_empty() {
            p.raw.clone().unwrap_or_default()
        } else {
            p.document.clone()
        };
        for l in text.lines() {
            lines.push(Line::raw(format!("  {l}")));
        }
    } else {
        lines.push(Line::styled("  (not run yet)", dim));
    }
    lines
}

/// Splits one line into the rows it takes at `width`, breaking on a space
/// where there is one and mid-word where there is not. Each piece keeps
/// its span's style; continuations start at `indent`, so a `key  value`
/// row hangs them under the value, not under the key.
pub(crate) fn wrap_line_hanging(
    line: &Line<'static>,
    width: usize,
    indent: usize,
) -> Vec<Line<'static>> {
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
        .collect();
    if width < 8 || chars.len() <= width {
        return vec![line.clone()];
    }
    let indent = indent.min(width / 2);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let pad = if out.is_empty() { 0 } else { indent };
        let avail = width.saturating_sub(pad).max(1);
        let mut end = (start + avail).min(chars.len());
        if end < chars.len()
            && let Some(p) = chars[start..end].iter().rposition(|(c, _)| *c == ' ')
            && p > 0
        {
            end = start + p;
        }
        let mut spans: Vec<Span<'static>> = Vec::new();
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }
        let mut buf = String::new();
        let mut style = chars[start].1;
        for (c, st) in &chars[start..end] {
            if *st != style && !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), style));
            }
            style = *st;
            buf.push(*c);
        }
        if !buf.is_empty() {
            spans.push(Span::styled(buf, style));
        }
        out.push(Line::from(spans));
        start = end;
        while start < chars.len() && chars[start].0 == ' ' {
            start += 1;
        }
    }
    out
}
