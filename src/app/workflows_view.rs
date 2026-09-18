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
use crate::workflows::store::{self as wstore, WorkflowRun, WorkflowStep};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::cell::{Cell, RefCell};
use std::path::Path;
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
    pub error: Option<String>,
    pub estimate: String,
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
        let estimate = doc.map(estimate_sessions).unwrap_or_default();
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
            field: DialogField::Workspace,
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
            error: None,
            estimate,
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
            error: None,
            estimate: String::new(),
        }
    }

    pub fn fields(&self) -> Vec<DialogField> {
        let mut v = Vec::new();
        if self.purpose == DialogPurpose::Plan {
            v.push(DialogField::Task);
        }
        v.push(DialogField::Workspace);
        v.push(DialogField::Profile);
        for i in 0..self.arg_names.len() {
            v.push(DialogField::Arg(i));
        }
        v.push(DialogField::Budget);
        if self.purpose != DialogPurpose::Plan {
            v.push(DialogField::MaxCost);
            v.push(DialogField::Isolation);
        }
        v
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
            DialogField::Isolation => {
                self.isolation = match self.isolation {
                    Isolation::None => Isolation::Worktree,
                    Isolation::Worktree => Isolation::None,
                };
            }
            _ => {}
        }
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

/// A static session estimate: known item counts, `?` for the rest.
pub fn estimate_sessions(doc: &crate::workflows::document::Workflow) -> String {
    use crate::workflows::document::{Over, StepKind};
    let mut parts: Vec<String> = Vec::new();
    for s in &doc.steps {
        let votes = s.verify.as_ref().map(|v| v.votes).unwrap_or(0);
        let part = match s.kind {
            StepKind::Single | StepKind::Route => {
                if votes > 0 {
                    format!("{}: 1+{votes}", s.id)
                } else {
                    format!("{}: 1", s.id)
                }
            }
            StepKind::Fanout | StepKind::Pipeline => {
                let n = match &s.over {
                    Some(Over::Inline(v)) => v.len().to_string(),
                    _ => "k".into(),
                };
                let per = if s.actor.is_some() { 1 } else { 0 } + votes;
                format!("{}: {n}×{per}", s.id)
            }
            StepKind::Tournament => format!("{}: {}+judges", s.id, s.n.unwrap_or(0)),
            StepKind::Until => format!("{}: ≤{} rounds", s.id, s.max_rounds),
        };
        parts.push(part);
    }
    parts.join("  ")
}

// ---- the view ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewPane {
    Runs,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewTab {
    Progress,
    Document,
    Result,
    Journal,
}

impl ViewTab {
    pub const ALL: [ViewTab; 4] = [
        ViewTab::Progress,
        ViewTab::Document,
        ViewTab::Result,
        ViewTab::Journal,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ViewTab::Progress => "Progress",
            ViewTab::Document => "Document",
            ViewTab::Result => "Result",
            ViewTab::Journal => "Journal",
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
    pub fn new(db_path: Option<&Path>, facts: &ViewFacts<'_>) -> Self {
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
            tab: ViewTab::Progress,
            scroll_offset: 0,
            viewport_rows: Cell::new(0),
            detail_lines: Vec::new(),
            wrap_width: Cell::new(0),
            wrapped: RefCell::new(WrapCache::default()),
            detail_rev: 0,
            pending: ViewPending::None,
            last_refresh: Instant::now(),
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
        self.rebuild_detail(facts);
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
                    lines.push(Line::styled("  ────────────────────────────────────────", dim));
                    match self.tab {
                        ViewTab::Document | ViewTab::Progress => {
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
                    lines.push(row("Workflow", format!("{} ({})", r.name, r.source)));
                    lines.push(row("Harness", r.harness.display_name().to_string()));
                    lines.push(row("Workspace", r.workspace.display().to_string()));
                    lines.push(row("Progress", r.progress()));
                    lines.push(row(
                        "Budget",
                        match r.state.doc.budget_tokens {
                            Some(b) => format!("{}k of {}k tokens", r.state.tokens_spent / 1000, b / 1000),
                            None => "none".into(),
                        },
                    ));
                    lines.push(Line::styled("  ────────────────────────────────────────", dim));
                    match self.tab {
                        ViewTab::Progress => {
                            for (step, state) in r.state.step_states() {
                                let style = match state {
                                    "done" => green,
                                    "running" => Style::default().fg(Color::Cyan),
                                    "skipped" => dim,
                                    _ => Style::default(),
                                };
                                lines.push(Line::from(vec![
                                    Span::raw(format!("  {step:<24} ")),
                                    Span::styled(state.to_string(), style),
                                ]));
                            }
                            lines.push(Line::raw(""));
                            for s in &r.sessions {
                                let state = if s.exited_at.is_some() { "settling" } else { "running" };
                                lines.push(Line::from(vec![
                                    Span::raw(format!("    {:<28} ", s.key.label())),
                                    Span::styled(
                                        format!("{state} on {} · Enter attaches", s.harness.as_str()),
                                        Style::default().fg(Color::Cyan),
                                    ),
                                ]));
                            }
                            for rec in r.state.records.iter().rev().take(30) {
                                lines.push(Line::from(vec![
                                    Span::raw(format!("    {:<28} ", rec.key.label())),
                                    Span::styled(
                                        format!("{} · {} tokens", rec.outcome.kind(), rec.tokens),
                                        if rec.outcome.kind() == "null" { red } else { dim },
                                    ),
                                ]));
                            }
                            lines.push(Line::raw(""));
                            for n in r.state.notes.iter().rev().take(20) {
                                lines.push(Line::styled(format!("  · {n}"), dim));
                            }
                        }
                        ViewTab::Document => {
                            for l in r.document.lines() {
                                lines.push(Line::raw(format!("  {l}")));
                            }
                        }
                        ViewTab::Result => match r.state.status() {
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
                        ViewTab::Journal => {
                            for rec in &r.state.records {
                                lines.push(Line::raw(format!(
                                    "  {:<28} {:<7} {} tokens",
                                    rec.key.label(),
                                    rec.outcome.kind(),
                                    rec.tokens
                                )));
                            }
                        }
                    }
                }
            }
            Some(RunRow::Stored(id)) => {
                if let Some(r) = self.stored.iter().find(|r| r.id == id).cloned() {
                    lines.push(row("Workflow", format!("{} ({})", r.workflow, r.source)));
                    lines.push(row("Harness", format!("{} · {}", r.harness, r.profile)));
                    lines.push(row("Workspace", r.workspace.clone()));
                    lines.push(Line::from(vec![
                        Span::styled("  Status    ", key),
                        Span::styled(
                            r.status.clone(),
                            match r.status.as_str() {
                                "finished" => green,
                                "running" => Style::default().fg(Color::Cyan),
                                _ => red,
                            },
                        ),
                    ]));
                    lines.push(row(
                        "Sessions",
                        format!(
                            "{} · {}k tokens · ${:.2}",
                            r.sessions,
                            r.tokens.unwrap_or(0) / 1000,
                            r.cost_usd.unwrap_or(0.0)
                        ),
                    ));
                    if let Some(e) = &r.error {
                        lines.push(Line::styled(format!("  Error     {e}"), red));
                    }
                    let recent = facts.recent.iter().find(|x| x.run_id == r.id);
                    lines.push(Line::styled("  ────────────────────────────────────────", dim));
                    match self.tab {
                        ViewTab::Progress | ViewTab::Journal => {
                            let steps: Vec<WorkflowStep> = self
                                .conn
                                .as_ref()
                                .and_then(|c| wstore::steps_of(c, &r.id).ok())
                                .unwrap_or_default();
                            for s in &steps {
                                lines.push(Line::from(vec![
                                    Span::raw(format!("  {:<28} {:<14} ", s.session, s.phase)),
                                    Span::styled(
                                        format!("{} · {} tokens", s.kind, s.tokens.unwrap_or(0)),
                                        if s.kind == "null" { red } else { dim },
                                    ),
                                ]));
                            }
                            if let Some(rec) = recent {
                                lines.push(Line::raw(""));
                                for n in &rec.notes {
                                    lines.push(Line::styled(format!("  · {n}"), dim));
                                }
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

    pub fn footer(&self) -> String {
        match &self.pending {
            ViewPending::Cancel => " Cancel this run? [y/n]".into(),
            ViewPending::SaveName(n) => {
                format!(" Save to the library as: {n}_   [Enter] save  [Esc] cancel")
            }
            ViewPending::None => " [Tab] tabs  [→] detail  [PgDn/End] scroll  [Enter] run/attach  [r] resume  [s] save  [x] cancel  [Esc] close".into(),
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
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
        .collect();
    if width < 8 || chars.len() <= width {
        return vec![line.clone()];
    }
    let indent = chars
        .iter()
        .take_while(|(c, _)| *c == ' ')
        .count()
        .min(width / 4);
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
