//! The Workflows section's dialog and the Workflows view (`W`): state and
//! detail text only. Starting, cancelling and saving are `App` methods in
//! `crate::app::workflows`.

use crate::app::dir_picker::DirPicker;
use crate::app::text_area::TextArea;
use crate::config::Profile;
use crate::harness::Harness;
use crate::workflows::document::Isolation;
use crate::workflows::interp::RunStatus;
use crate::workflows::library::Entry;
use crate::workflows::report::{self, Block, Report, RunView, Status};
use crate::workflows::store::{self as wstore, WorkflowRun, WorkflowStep};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

// ---- the view ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewPane {
    Runs,
    Detail,
}

/// The four tabs of the detail pane. `Report` is what the run answered,
/// `Steps` the ledger it answered from; the journal is part of `Steps`,
/// because a session row and a journal entry are the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewTab {
    Report,
    Steps,
    Result,
    Document,
}

impl ViewTab {
    pub const ALL: [ViewTab; 4] = [
        ViewTab::Report,
        ViewTab::Steps,
        ViewTab::Result,
        ViewTab::Document,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ViewTab::Report => "Report",
            ViewTab::Steps => "Steps",
            ViewTab::Result => "Result",
            ViewTab::Document => "Document",
        }
    }
}

/// One row of the left list.
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

/// The footer question of the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewPending {
    None,
    Cancel,
    /// `s`: the library name for the selected document.
    SaveName(String),
}

const REFRESH: Duration = Duration::from_millis(1000);

pub struct WorkflowsViewState {
    conn: Option<rusqlite::Connection>,
    pub error: Option<String>,
    pub stored: Vec<WorkflowRun>,
    pub rows: Vec<RunRow>,
    pub selected: usize,
    pub focus: ViewPane,
    pub tab: ViewTab,
    pub scroll_offset: usize,
    pub viewport_rows: Cell<usize>,
    pub detail_lines: Vec<Line<'static>>,
    /// Width of the detail pane, set by the renderer. A result is often one
    /// very long line, so the pane wraps rather than truncates and the
    /// scroll counts the wrapped rows.
    pub wrap_width: Cell<u16>,
    wrapped: RefCell<WrapCache>,
    detail_rev: u64,
    pub pending: ViewPending,
    last_refresh: Instant,
    /// `<runtime>/workflows/<run>/` holds the notes and the null reasons a
    /// finished run wrote about itself.
    runtime: Option<PathBuf>,
    cache: Option<RunFacts>,
}

/// One stored run's report, its step rows and the reason each null answer
/// gave, built once per selection.
struct RunFacts {
    run_id: String,
    status: String,
    report: Report,
    steps: Vec<WorkflowStep>,
    reasons: BTreeMap<String, String>,
    /// The agent each session ran as, by session label.
    agents: BTreeMap<String, String>,
}

/// The detail pane's lines wrapped to one width; invalidated by a rebuild
/// (`rev`) or a resize (`width`).
#[derive(Default)]
struct WrapCache {
    width: u16,
    rev: u64,
    filled: bool,
    rows: Vec<Line<'static>>,
}

/// What the detail builder needs from the App.
pub struct ViewFacts<'a> {
    pub live: &'a [crate::app::workflows::LiveWorkflowRun],
    pub recent: &'a [crate::app::workflows::RecentWorkflowRun],
    pub planned: &'a [crate::app::workflows::PlannedWorkflow],
    pub plans_in_flight: usize,
}

impl WorkflowsViewState {
    pub fn new(db_path: Option<&Path>, runtime: Option<&Path>, facts: &ViewFacts<'_>) -> Self {
        let (conn, error) = match db_path.map(crate::tracing::store::open_ro) {
            Some(Ok(c)) => (Some(c), None),
            Some(Err(e)) => (None, Some(e)),
            None => (None, Some("no trace store".into())),
        };
        let mut v = WorkflowsViewState {
            conn,
            error,
            stored: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            focus: ViewPane::Runs,
            tab: ViewTab::Report,
            scroll_offset: 0,
            viewport_rows: Cell::new(0),
            detail_lines: Vec::new(),
            wrap_width: Cell::new(0),
            wrapped: RefCell::new(WrapCache::default()),
            detail_rev: 0,
            pending: ViewPending::None,
            last_refresh: Instant::now(),
            runtime: runtime.map(Path::to_path_buf),
            cache: None,
        };
        v.reload(facts);
        v
    }

    pub fn reload(&mut self, facts: &ViewFacts<'_>) {
        let keep = self.rows.get(self.selected).cloned();
        self.stored = self
            .conn
            .as_ref()
            .and_then(|c| wstore::recent_runs(c, None, 50).ok())
            .unwrap_or_default();
        self.rows.clear();
        if !facts.planned.is_empty() || facts.plans_in_flight > 0 {
            self.rows.push(RunRow::Header("Planned"));
            for p in facts.planned {
                self.rows.push(RunRow::Planned(p.id.clone()));
            }
        }
        if !facts.live.is_empty() {
            self.rows.push(RunRow::Header("Running"));
            for r in facts.live {
                self.rows.push(RunRow::Live(r.run_id.clone()));
            }
        }
        if !self.stored.is_empty() {
            self.rows.push(RunRow::Header("Runs"));
            let live_ids: Vec<&str> = facts.live.iter().map(|r| r.run_id.as_str()).collect();
            for r in &self.stored {
                if live_ids.contains(&r.id.as_str()) {
                    continue;
                }
                self.rows.push(RunRow::Stored(r.id.clone()));
            }
        }
        self.selected = keep
            .and_then(|k| self.rows.iter().position(|r| *r == k))
            .unwrap_or(0);
        self.ensure_selectable();
        if let (Some(c), Some(RunRow::Stored(id))) = (&self.cache, self.rows.get(self.selected))
            && c.run_id != *id
        {
            self.cache = None;
        }
        self.rebuild_detail(facts);
    }

    /// Puts the selection on `row` (a run or a plan the section chose),
    /// when the list has it.
    pub fn select(&mut self, row: &RunRow, facts: &ViewFacts<'_>) {
        if let Some(i) = self.rows.iter().position(|r| r == row) {
            self.selected = i;
            self.cache = None;
            self.scroll_offset = 0;
            self.rebuild_detail(facts);
        }
    }

    fn ensure_selectable(&mut self) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(self.rows.len() - 1);
        if matches!(self.rows[self.selected], RunRow::Header(_))
            && let Some(p) = self.rows[self.selected..]
                .iter()
                .position(|r| !matches!(r, RunRow::Header(_)))
        {
            self.selected += p;
        }
    }

    pub fn refresh_if_due(&mut self, now: Instant, facts: &ViewFacts<'_>) {
        if now.saturating_duration_since(self.last_refresh) >= REFRESH {
            self.last_refresh = now;
            self.reload(facts);
        }
    }

    pub fn selected_row(&self) -> Option<&RunRow> {
        self.rows.get(self.selected)
    }

    pub fn step(&mut self, delta: isize, facts: &ViewFacts<'_>) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as isize;
        let mut i = self.selected as isize;
        let dir = delta.signum();
        let mut remaining = delta.abs();
        while remaining > 0 {
            let mut j = i + dir;
            while j >= 0 && j < len && matches!(self.rows[j as usize], RunRow::Header(_)) {
                j += dir;
            }
            if j < 0 || j >= len {
                break;
            }
            i = j;
            remaining -= 1;
        }
        if i as usize != self.selected {
            self.selected = i as usize;
            self.scroll_offset = 0;
            self.rebuild_detail(facts);
        }
    }

    pub fn next_tab(&mut self, facts: &ViewFacts<'_>) {
        let at = ViewTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0);
        self.tab = ViewTab::ALL[(at + 1) % ViewTab::ALL.len()];
        self.scroll_offset = 0;
        self.rebuild_detail(facts);
    }

    /// `1`-`4`: straight to a tab, as in the Loops view.
    pub fn set_tab(&mut self, tab: ViewTab, facts: &ViewFacts<'_>) {
        self.tab = tab;
        self.scroll_offset = 0;
        self.rebuild_detail(facts);
    }

    pub fn prev_tab(&mut self, facts: &ViewFacts<'_>) {
        let at = ViewTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0);
        self.tab = ViewTab::ALL[(at + ViewTab::ALL.len() - 1) % ViewTab::ALL.len()];
        self.scroll_offset = 0;
        self.rebuild_detail(facts);
    }

    /// Wraps `detail_lines` to `width`, unless that is already what the
    /// cache holds.
    fn ensure_wrapped(&self, width: u16) {
        let mut cache = self.wrapped.borrow_mut();
        if cache.filled && cache.width == width && cache.rev == self.detail_rev {
            return;
        }
        cache.width = width;
        cache.rev = self.detail_rev;
        cache.filled = true;
        cache.rows = self
            .detail_lines
            .iter()
            .flat_map(|l| wrap_line(l, usize::from(width)))
            .collect();
    }

    /// Records the detail pane's size. The renderer calls this as soon as
    /// it knows it, so the pane's title and `max_scroll` agree with what is
    /// about to be drawn.
    pub fn measure(&self, width: u16, height: u16) {
        self.wrap_width.set(width);
        self.viewport_rows.set(usize::from(height));
    }

    /// The rows the detail pane shows at `width`, from the scroll offset.
    pub fn visible_rows(&self, width: u16, height: u16) -> Vec<Line<'static>> {
        self.measure(width, height);
        self.ensure_wrapped(width);
        self.wrapped
            .borrow()
            .rows
            .iter()
            .skip(self.scroll_offset)
            .take(usize::from(height))
            .cloned()
            .collect()
    }

    /// Scrollable rows: the wrapped count once the pane has been drawn.
    pub fn total_rows(&self) -> usize {
        self.ensure_wrapped(self.wrap_width.get());
        self.wrapped.borrow().rows.len()
    }

    pub fn max_scroll(&self) -> usize {
        self.total_rows()
            .saturating_sub(self.viewport_rows.get().max(1))
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.max_scroll();
    }

    /// Where the detail pane sits, for the pane title: `None` when it all
    /// fits on screen.
    pub fn scroll_position(&self) -> Option<String> {
        let total = self.total_rows();
        let rows = self.viewport_rows.get().max(1);
        (total > rows).then(|| {
            let last = (self.scroll_offset + rows).min(total);
            format!("{}–{last}/{total}", self.scroll_offset + 1)
        })
    }

    pub fn scroll(&mut self, delta: isize) {
        self.scroll_offset = if delta < 0 {
            self.scroll_offset.saturating_sub(delta.unsigned_abs())
        } else {
            self.scroll_offset
                .saturating_add(delta as usize)
                .min(self.max_scroll())
        };
    }

    /// The title of the selected row.
    pub fn selected_title(&self, facts: &ViewFacts<'_>) -> String {
        match self.selected_row() {
            Some(RunRow::Planned(id)) => facts
                .planned
                .iter()
                .find(|p| p.id == *id)
                .map(|p| format!("planned · {} · {}", p.name, short(&p.task, 40)))
                .unwrap_or_default(),
            Some(RunRow::Live(id)) => facts
                .live
                .iter()
                .find(|r| r.run_id == *id)
                .map(|r| format!("{} · running · {}", r.name, r.progress()))
                .unwrap_or_default(),
            Some(RunRow::Stored(id)) => self
                .stored
                .iter()
                .find(|r| r.id == *id)
                .map(|r| format!("{} · {} · {}", r.workflow, r.status, &r.id[..8]))
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    pub fn rebuild_detail(&mut self, facts: &ViewFacts<'_>) {
        let tab = self.tab;
        let dim = Style::default().fg(Color::DarkGray);
        let key = Style::default().fg(Color::Yellow);
        let red = Style::default().fg(Color::Red);
        let green = Style::default().fg(Color::Green);
        let row = |k: &str, v: String| {
            Line::from(vec![Span::styled(format!("  {k:<10}"), key), Span::raw(v)])
        };
        let mut lines: Vec<Line<'static>> = Vec::new();
        match self.selected_row().cloned() {
            None => lines.push(Line::styled(
                "  no workflow runs yet: Enter on a workflow in the Workflows section, or c to compose one",
                dim,
            )),
            Some(RunRow::Header(_)) => {}
            Some(RunRow::Planned(id)) => {
                if let Some(p) = facts.planned.iter().find(|p| p.id == id) {
                    lines.push(row("Task", p.task.clone()));
                    lines.push(row("Workflow", p.name.clone()));
                    lines.push(row("Harness", p.harness.display_name().to_string()));
                    lines.push(row("Workspace", p.workspace.display().to_string()));
                    match &p.run_id {
                        Some(r) => lines.push(row("Run", format!("started {}", &r[..8]))),
                        None if p.valid() => lines.push(Line::from(vec![
                            Span::styled("  Status    ", key),
                            Span::styled("valid · Enter runs it, e edits, s saves, x discards", green),
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
                    lines.push(rule());
                    match tab {
                        ViewTab::Document | ViewTab::Report => {
                            let text = if p.document.is_empty() {
                                p.raw.clone().unwrap_or_default()
                            } else {
                                p.document.clone()
                            };
                            for l in text.lines() {
                                lines.push(Line::raw(format!("  {l}")));
                            }
                        }
                        _ => lines.push(Line::styled("  (not run yet)", dim)),
                    }
                }
            }
            Some(RunRow::Live(id)) => {
                if let Some(r) = facts.live.iter().find(|r| r.run_id == id) {
                    let status = r.state.status();
                    let (status_word, result) = match &status {
                        RunStatus::Running => ("running".to_string(), serde_json::Value::Null),
                        RunStatus::Finished(v) => ("finished".to_string(), v.clone()),
                        RunStatus::BudgetExhausted(v) => ("budget-exhausted".to_string(), v.clone()),
                        RunStatus::Failed(e) => (format!("failed: {e}"), serde_json::Value::Null),
                        RunStatus::Cancelled => ("cancelled".to_string(), serde_json::Value::Null),
                    };
                    let view = RunView {
                        workflow: &r.name,
                        status: status_word.split(':').next().unwrap_or("running"),
                        harness: r.harness.as_str(),
                        workspace: &r.workspace.display().to_string(),
                        sessions: r.state.records.len() as i64,
                        tokens: r.state.tokens_spent,
                        cost_usd: Some(r.state.cost_spent),
                        duration_s: None,
                        result: &result,
                        error: None,
                        notes: &r.state.notes,
                        doc: Some(&r.state.doc),
                        steps: &[],
                    };
                    let rep = report::build(view);
                    match tab {
                        ViewTab::Report => {
                            lines.extend(headline_lines(&rep));
                            lines.push(Line::styled(
                                format!(
                                    "  {} · {}",
                                    r.progress(),
                                    match r.state.doc.budget_tokens {
                                        Some(b) => format!(
                                            "{} of {} tokens",
                                            crate::loops::format_tokens(r.state.tokens_spent),
                                            crate::loops::format_tokens(b)
                                        ),
                                        None => "no budget".into(),
                                    }
                                ),
                                dim,
                            ));
                            lines.push(rule());
                            // Per-step progress: the bar fills as sessions settle.
                            let mut done: BTreeMap<String, (usize, usize, u64)> = BTreeMap::new();
                            for rec in &r.state.records {
                                let e = done.entry(rec.key.step.clone()).or_default();
                                e.0 += 1;
                                if rec.outcome.kind() != "null" {
                                    e.1 += 1;
                                }
                                e.2 += rec.tokens;
                            }
                            for (step, state) in r.state.step_states() {
                                let (started, ok, tokens) =
                                    done.get(&step).copied().unwrap_or((0, 0, 0));
                                let style = match state {
                                    "done" => green,
                                    "running" => Style::default().fg(Color::Cyan),
                                    "skipped" => dim,
                                    _ => Style::default(),
                                };
                                lines.push(Line::from(vec![
                                    Span::raw(format!("  {step:<18} ")),
                                    Span::styled(format!("{state:<8} "), style),
                                    Span::raw(format!(
                                        "{:<16} {}",
                                        if started == 0 {
                                            String::new()
                                        } else {
                                            format!("{ok}/{started} answered")
                                        },
                                        if tokens == 0 {
                                            String::new()
                                        } else {
                                            format!("{} tokens", crate::loops::format_tokens(tokens))
                                        }
                                    )),
                                ]));
                            }
                            if !r.sessions.is_empty() {
                                lines.push(Line::raw(""));
                                for s in &r.sessions {
                                    let state = if s.exited_at.is_some() { "settling" } else { "running" };
                                    lines.push(Line::from(vec![
                                        Span::raw(format!("  {:<28} ", s.key.label())),
                                        Span::styled(
                                            format!("{state} on {} · Enter attaches", s.harness.as_str()),
                                            Style::default().fg(Color::Cyan),
                                        ),
                                    ]));
                                }
                            }
                            if !r.state.notes.is_empty() {
                                lines.push(Line::raw(""));
                                lines.push(Line::styled("  Notes", key));
                                for n in r.state.notes.iter().rev().take(20) {
                                    lines.push(Line::styled(format!("    · {n}"), dim));
                                }
                            }
                        }
                        ViewTab::Steps => {
                            lines.extend(headline_lines(&rep));
                            lines.push(rule());
                            let mut by_step: Vec<(String, Vec<&crate::workflows::interp::SessionRecord>)> =
                                Vec::new();
                            for rec in &r.state.records {
                                match by_step.iter_mut().find(|(s, _)| *s == rec.key.step) {
                                    Some((_, v)) => v.push(rec),
                                    None => by_step.push((rec.key.step.clone(), vec![rec])),
                                }
                            }
                            for (step, recs) in by_step {
                                let ok = recs.iter().filter(|r| r.outcome.kind() != "null").count();
                                let tokens: u64 = recs.iter().map(|r| r.tokens).sum();
                                lines.push(Line::styled(
                                    format!(
                                        "  {step} · {ok}/{} answered · {} tokens",
                                        recs.len(),
                                        crate::loops::format_tokens(tokens)
                                    ),
                                    key,
                                ));
                                for rec in recs {
                                    let null = rec.outcome.kind() == "null";
                                    lines.push(Line::from(vec![
                                        Span::raw(format!("    {:<28} ", rec.key.label())),
                                        Span::styled(
                                            format!(
                                                "{:<8} {} tokens",
                                                rec.outcome.kind(),
                                                crate::loops::format_tokens(rec.tokens)
                                            ),
                                            if null { red } else { dim },
                                        ),
                                    ]));
                                }
                            }
                        }
                        ViewTab::Document => {
                            for l in r.document.lines() {
                                lines.push(Line::raw(format!("  {l}")));
                            }
                        }
                        ViewTab::Result => match status {
                            RunStatus::Running => {
                                lines.push(Line::styled(
                                    "  still running · the steps that have finished so far",
                                    dim,
                                ));
                                for (step, state) in r.state.step_states() {
                                    if state != "done" {
                                        continue;
                                    }
                                    lines.push(Line::raw(""));
                                    lines.push(Line::styled(format!("  ── {step} ──"), key));
                                    lines
                                        .extend(value_lines(&pretty(&r.state.step_result(&step))));
                                }
                            }
                            RunStatus::Finished(v) => lines.extend(value_lines(&pretty(&v))),
                            RunStatus::BudgetExhausted(v) => {
                                lines.push(Line::styled("  budget exhausted · partial", red));
                                lines.extend(value_lines(&pretty(&v)));
                            }
                            RunStatus::Failed(e) => {
                                lines.push(Line::styled(format!("  failed · {e}"), red))
                            }
                            RunStatus::Cancelled => {
                                lines.push(Line::styled("  cancelled", dim))
                            }
                        },
                    }
                }
            }
            Some(RunRow::Stored(id)) => {
                if let Some(r) = self.stored.iter().find(|r| r.id == id).cloned() {
                    let facts_for_run = self.run_facts(&r, facts);
                    match tab {
                        ViewTab::Report => {
                            lines.extend(headline_lines(&facts_for_run.report));
                            lines.push(Line::styled(
                                format!(
                                    "  {} · {} · {}",
                                    started_range(&r),
                                    r.harness,
                                    r.workspace
                                ),
                                dim,
                            ));
                            if let Some(e) = &r.error {
                                lines.push(Line::styled(format!("  Error     {e}"), red));
                            }
                            lines.push(rule());
                            lines.extend(report_lines(&facts_for_run.report));
                        }
                        ViewTab::Steps => {
                            lines.extend(headline_lines(&facts_for_run.report));
                            lines.push(rule());
                            lines.extend(steps_lines(
                                &facts_for_run.steps,
                                &facts_for_run.reasons,
                                &facts_for_run.agents,
                            ));
                            if !facts_for_run.report.headline.counts.is_empty() {
                                lines.push(Line::raw(""));
                            }
                            lines.push(Line::styled(
                                "  r resumes this run from its journal",
                                dim,
                            ));
                        }
                        ViewTab::Document => {
                            for l in r.document.lines() {
                                lines.push(Line::raw(format!("  {l}")));
                            }
                            lines.push(Line::styled("  s saves this document into the library", dim));
                        }
                        ViewTab::Result => lines.extend(value_lines(&pretty(&r.result))),
                    }
                }
            }
        }
        self.detail_lines = lines;
        self.detail_rev = self.detail_rev.wrapping_add(1);
        self.scroll_offset = self.scroll_offset.min(self.max_scroll());
    }

    /// The report of a stored run, with the step rows and the per-session
    /// reasons behind it. Cached: the view rebuilds every second and a
    /// report costs one query and two small file reads.
    fn run_facts(&mut self, r: &WorkflowRun, facts: &ViewFacts<'_>) -> &RunFacts {
        if self
            .cache
            .as_ref()
            .is_some_and(|c| c.run_id == r.id && c.status == r.status)
        {
            return self.cache.as_ref().unwrap();
        }
        let steps: Vec<WorkflowStep> = self
            .conn
            .as_ref()
            .and_then(|c| wstore::steps_of(c, &r.id).ok())
            .unwrap_or_default();
        let doc = crate::workflows::document::parse(&r.document).ok();
        let (mut notes, reasons, agents) = self.run_files(&r.id);
        if notes.is_empty()
            && let Some(rec) = facts.recent.iter().find(|x| x.run_id == r.id)
        {
            notes = rec.notes.clone();
        }
        let report = report::build(RunView {
            workflow: &r.workflow,
            status: &r.status,
            harness: &r.harness,
            workspace: &r.workspace,
            sessions: r.sessions,
            tokens: r.tokens.unwrap_or(0).max(0) as u64,
            cost_usd: r.cost_usd,
            duration_s: duration_s(r),
            result: &r.result,
            error: r.error.as_deref(),
            notes: &notes,
            doc: doc.as_ref(),
            steps: &steps,
        });
        self.cache = Some(RunFacts {
            run_id: r.id.clone(),
            status: r.status.clone(),
            report,
            steps,
            reasons,
            agents,
        });
        self.cache.as_ref().unwrap()
    }

    /// `result.json` for the notes, `journal.jsonl` for the reason a
    /// session answered `null`. Both are written by the run itself, so a
    /// finished run explains its own gaps without a schema change.
    #[allow(clippy::type_complexity)]
    fn run_files(
        &self,
        run_id: &str,
    ) -> (
        Vec<String>,
        BTreeMap<String, String>,
        BTreeMap<String, String>,
    ) {
        let mut notes = Vec::new();
        let mut reasons = BTreeMap::new();
        let mut agents = BTreeMap::new();
        let Some(dir) = self
            .runtime
            .as_ref()
            .map(|r| r.join("workflows").join(run_id))
        else {
            return (notes, reasons, agents);
        };
        if let Ok(text) = std::fs::read_to_string(dir.join("result.json"))
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
            && let Some(a) = v.get("notes").and_then(|n| n.as_array())
        {
            notes = a
                .iter()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect();
        }
        if let Ok(text) = std::fs::read_to_string(dir.join("journal.jsonl")) {
            for line in text.lines() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if let (Some(k), Some(reason)) = (
                    v.get("key").and_then(|k| k.as_str()),
                    v.get("reason").and_then(|r| r.as_str()),
                ) {
                    reasons.insert(k.to_string(), reason.to_string());
                }
                if let (Some(k), Some(agent)) = (
                    v.get("key").and_then(|k| k.as_str()),
                    v.get("agent").and_then(|a| a.as_str()),
                ) {
                    agents.insert(k.to_string(), agent.to_string());
                }
            }
        }
        (notes, reasons, agents)
    }

    pub fn footer(&self) -> String {
        match &self.pending {
            ViewPending::Cancel => " Cancel this run? [y/n]".into(),
            ViewPending::SaveName(n) => {
                format!(" Save to the library as: {n}_   [Enter] save  [Esc] cancel")
            }
            ViewPending::None => " [Tab/1-4] tab  [←/→] pane  [↑/↓] select  [PgDn/End] scroll  [Enter] run/attach  [r] resume  [s] save  [x] cancel  [I] inbox  [Esc] close".into(),
        }
    }
}

fn pretty(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "(no result)".into(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn value_lines(text: &str) -> Vec<Line<'static>> {
    text.lines().map(|l| Line::raw(format!("  {l}"))).collect()
}

/// Splits one detail line into the rows it takes at `width`, breaking on a
/// space where there is one and mid-word where there is not (a result is
/// often a single long JSON line). Each piece keeps its span's style, and
/// continuations sit under the line's own indent.
fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    let indent = line
        .spans
        .iter()
        .flat_map(|s| s.content.chars())
        .take_while(|c| *c == ' ')
        .count()
        .min(width / 4);
    wrap_line_hanging(line, width, indent)
}

/// [`wrap_line`] with the continuation indent given: a `key  value` row
/// hangs its continuations under the value, not under the key.
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

fn short(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

impl std::fmt::Debug for WorkflowsViewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowsViewState")
            .field("rows", &self.rows.len())
            .field("selected", &self.selected)
            .field("tab", &self.tab)
            .finish()
    }
}

fn rule() -> Line<'static> {
    Line::styled(
        "  ────────────────────────────────────────",
        Style::default().fg(Color::DarkGray),
    )
}

fn duration_s(r: &WorkflowRun) -> Option<i64> {
    let end = r.ended_ns?;
    Some((end - r.started_ns) / 1_000_000_000)
}

fn started_range(r: &WorkflowRun) -> String {
    report::format_range(r.started_ns, r.ended_ns)
}

fn status_style(s: Status) -> Style {
    Style::default().fg(match s {
        Status::Ok => Color::Green,
        Status::Running => Color::Cyan,
        Status::Attention => Color::Yellow,
        Status::Failed => Color::Red,
    })
}

fn badge_style(badge: &str) -> Style {
    match badge.to_ascii_lowercase().as_str() {
        "high" | "critical" | "blocker" | "bug" => Style::default().fg(Color::Red),
        "medium" | "low" | "question" => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(Color::Cyan),
    }
}

/// The three lines every surface leads with: verdict, counts, cost.
fn headline_lines(rep: &Report) -> Vec<Line<'static>> {
    let h = &rep.headline;
    let dim = Style::default().fg(Color::DarkGray);
    vec![
        Line::from(vec![
            Span::styled(format!("  {} ", h.status.glyph()), status_style(h.status)),
            Span::styled(
                format!("{:<12} ", h.status_word),
                status_style(h.status).add_modifier(Modifier::BOLD),
            ),
            Span::raw(h.verdict.clone()),
        ]),
        Line::styled(format!("  {}", h.one_line()), dim),
    ]
}

/// The report's blocks as rows of the detail pane.
fn report_lines(rep: &Report) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let key = Style::default().fg(Color::Yellow);
    let mut out: Vec<Line<'static>> = Vec::new();
    for block in &rep.blocks {
        if !out.is_empty() {
            out.push(Line::raw(""));
        }
        match block {
            Block::Outline {
                title,
                bytes: _,
                headings,
                lead,
            } => {
                out.push(Line::styled(format!("  {}", cap_first(title)), key));
                if headings.is_empty() && !lead.is_empty() {
                    out.push(Line::raw(format!("    {lead}")));
                }
                // The first heading is the verdict the headline already
                // shows; repeating it under the title says nothing new.
                let skip = usize::from(
                    headings
                        .first()
                        .is_some_and(|h| h.text.trim() == rep.headline.verdict.trim()),
                );
                for h in headings.iter().skip(skip).take(24) {
                    let indent = "  ".repeat(h.level.saturating_sub(1));
                    out.push(Line::raw(format!("    {indent}{}", h.text)));
                }
            }
            Block::Table {
                title,
                columns: _,
                rows,
            } => {
                out.push(Line::styled(format!("  {title} ({})", rows.len()), key));
                for r in rows.iter().take(60) {
                    out.extend(row_lines(r, false));
                    if let Some(d) = &r.detail {
                        out.push(Line::styled(
                            format!("      {}", report::short(d, 100)),
                            dim,
                        ));
                    }
                }
            }
            Block::Dropped { title, rows } => {
                out.push(Line::styled(format!("  {title}"), key));
                for r in rows.iter().take(40) {
                    out.extend(row_lines(r, true));
                    for reason in r.reasons.iter().take(3) {
                        out.push(Line::styled(
                            format!("      refuted: {}", report::short(reason, 96)),
                            dim,
                        ));
                    }
                }
            }
            Block::Evidence {
                step,
                badge,
                fields,
            } => {
                let mut head = vec![Span::styled(format!("  {} ", cap_first(step)), key)];
                if let Some(b) = badge {
                    head.push(Span::styled(b.to_uppercase(), badge_style(b)));
                }
                out.push(Line::from(head));
                for (name, values) in fields {
                    if values.is_empty() {
                        continue;
                    }
                    out.push(Line::styled(format!("    {name}"), dim));
                    for v in values.iter().take(6) {
                        out.push(Line::raw(format!("      · {}", report::short(v, 96))));
                    }
                    if values.len() > 6 {
                        out.push(Line::styled(
                            format!("      + {} more", values.len() - 6),
                            dim,
                        ));
                    }
                }
            }
            Block::List { title, rows } => {
                out.push(Line::styled(format!("  {title}"), key));
                for (name, said) in rows.iter().take(40) {
                    out.push(Line::from(vec![
                        Span::raw(format!("    {:<44} ", report::short(name, 44))),
                        Span::styled(report::short(said, 52), dim),
                    ]));
                }
            }
            Block::Notes(notes) => {
                out.push(Line::styled("  Notes", key));
                for n in notes.iter().take(20) {
                    out.push(Line::styled(format!("    · {n}"), dim));
                }
            }
            Block::Text(t) => {
                for l in t.lines().take(200) {
                    out.push(Line::raw(format!("  {l}")));
                }
            }
        }
    }
    out
}

/// One finding: the badge, the location and the votes on a short first
/// line, the title under it, so the row fits a narrow pane instead of
/// wrapping mid-column. A row with no location keeps the title on the
/// first line.
fn row_lines(r: &report::Row, dropped: bool) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans = vec![Span::raw("    ")];
    if let Some(b) = &r.badge {
        spans.push(Span::styled(
            format!("{:<8} ", b.to_uppercase()),
            if dropped { dim } else { badge_style(b) },
        ));
    }
    let title_style = if dropped { dim } else { Style::default() };
    let title = report::short(&r.title, 96);
    let mut under = None;
    match &r.location {
        Some(l) => {
            spans.push(Span::styled(
                format!("{:<30}", report::short(l, 30)),
                if dropped {
                    dim
                } else {
                    Style::default().fg(Color::Cyan)
                },
            ));
            under = Some(Line::styled(format!("      {title}"), title_style));
        }
        None => spans.push(Span::styled(format!("{title:<30}"), title_style)),
    }
    if let Some(votes) = votes_text(r.votes) {
        spans.push(Span::styled(format!("  {votes}"), dim));
    }
    let mut out = vec![Line::from(spans)];
    out.extend(under);
    out
}

/// `refuted 1/3`: the votes against out of the votes cast, named, so the
/// fraction cannot be read as the votes in favour.
fn votes_text(votes: Option<(u64, u64)>) -> Option<String> {
    votes.map(|(against, cast)| format!("refuted {against}/{cast}"))
}

/// The ledger: one group per step, one row per session, with the reason a
/// null answer gave.
/// `reviewer@codex` when the session ran as an agent, the harness otherwise.
fn runs_on(s: &WorkflowStep, agents: &BTreeMap<String, String>) -> String {
    match agents.get(&s.session) {
        Some(a) => format!("{a}@{}", s.harness),
        None => s.harness.clone(),
    }
}

fn steps_lines(
    steps: &[WorkflowStep],
    reasons: &BTreeMap<String, String>,
    agents: &BTreeMap<String, String>,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let key = Style::default().fg(Color::Yellow);
    let red = Style::default().fg(Color::Red);
    let mut out: Vec<Line<'static>> = Vec::new();
    if steps.is_empty() {
        out.push(Line::styled("  (no sessions recorded)", dim));
        return out;
    }
    let mut order: Vec<String> = Vec::new();
    for s in steps {
        if !order.contains(&s.step_id) {
            order.push(s.step_id.clone());
        }
    }
    for step_id in order {
        let group: Vec<&WorkflowStep> = steps.iter().filter(|s| s.step_id == step_id).collect();
        let ok = group.iter().filter(|s| s.kind != "null").count();
        let tokens: u64 = group
            .iter()
            .map(|s| s.tokens.unwrap_or(0).max(0) as u64)
            .sum();
        let phase = group.first().map(|s| s.phase.clone()).unwrap_or_default();
        let mut on: Vec<String> = Vec::new();
        for g in &group {
            let r = runs_on(g, agents);
            if !on.contains(&r) {
                on.push(r);
            }
        }
        out.push(Line::from(vec![
            Span::styled(format!("  {phase} · {step_id}"), key),
            Span::styled(
                format!(
                    " · {} session(s) · {ok} answered · {} tokens · {}",
                    group.len(),
                    crate::loops::format_tokens(tokens),
                    on.join(", ")
                ),
                dim,
            ),
        ]));

        for s in group {
            let null = s.kind == "null";
            let dur = match (s.started_ns, s.ended_ns) {
                (Some(a), Some(b)) if b > a => report::format_duration((b - a) / 1_000_000_000),
                _ => "-".into(),
            };
            out.push(Line::from(vec![
                Span::styled(
                    format!("    {} ", if null { "✗" } else { "✓" }),
                    if null {
                        red
                    } else {
                        Style::default().fg(Color::Green)
                    },
                ),
                Span::raw(format!("{:<26} ", s.session)),
                Span::styled(
                    format!(
                        "{:<7} {:<8} {:>8} {:>8}",
                        s.kind,
                        runs_on(s, agents),
                        crate::loops::format_tokens(s.tokens.unwrap_or(0).max(0) as u64),
                        dur
                    ),
                    dim,
                ),
            ]));
            if let Some(r) = reasons.get(&s.session) {
                out.push(Line::styled(
                    format!("        reason  {}", report::short(r, 96)),
                    red,
                ));
            }
            if let Some(files) = s.changed_files.as_array()
                && !files.is_empty()
            {
                let list: Vec<String> = files
                    .iter()
                    .filter_map(|f| f.as_str().map(str::to_string))
                    .take(6)
                    .collect();
                out.push(Line::styled(
                    format!("        changed {}", list.join(", ")),
                    dim,
                ));
            }
        }
    }
    out
}

fn cap_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
