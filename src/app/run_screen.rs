//! The Run screen (`Mode::Run`): one flow run in the Swarm screen's
//! layout. The tree on the left has the run, every step of its document
//! (also those not reached yet) and the sessions of a step that has
//! several; the main pane shows the selected row in tabs. Design:
//! docs/superpowers/specs/2026-11-13-run-screen-and-flow-editor-ux-design.md
//! (section 2).
//!
//! The rows come from `workflows::run_view`: `from_live` while the run is
//! in memory, `from_stored` once it is only in the store, so a run that
//! ends under the screen stays on it. This file holds the screen's state,
//! its keys and the text of the tabs; `ui::run_screen` draws it.

use super::{App, Mode, Notice};
use crate::harness::Harness;
use crate::keymap::{Verb, verb};
use crate::workflows::document::{Actor, Step, Workflow};
use crate::workflows::interp::{Role, RunStatus, SessionKey};
use crate::workflows::live::{self, LiveLine};
use crate::workflows::report::{self, RunView};
use crate::workflows::run_view::{
    self, LiveRun, LiveSession, Row, RowKind, RunTree, State, Status, StoredRun, tokens_short,
};
use crate::workflows::store::{self as wstore, WorkflowRun, WorkflowStep};
use crate::workflows::{context, journal};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How often the screen re-reads a run in memory.
const REFRESH_LIVE: Duration = Duration::from_millis(1000);
/// And a run another process is running, which only the store knows.
const REFRESH_STORED: Duration = Duration::from_millis(2000);

/// Where the screen was opened from, and `Esc` returns to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Agents,
    Runs,
}

/// The run row's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTab {
    Board,
    Live,
    Timeline,
    Result,
    Document,
}

impl RunTab {
    pub const ALL: [RunTab; 5] = [
        RunTab::Board,
        RunTab::Live,
        RunTab::Timeline,
        RunTab::Result,
        RunTab::Document,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RunTab::Board => "Board",
            RunTab::Live => "Live",
            RunTab::Timeline => "Timeline",
            RunTab::Result => "Result",
            RunTab::Document => "Document",
        }
    }
}

/// A step's and a session's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepTab {
    Step,
    Live,
    Trace,
    Changes,
}

impl StepTab {
    pub const ALL: [StepTab; 4] = [
        StepTab::Step,
        StepTab::Live,
        StepTab::Trace,
        StepTab::Changes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StepTab::Step => "Step",
            StepTab::Live => "Live",
            StepTab::Trace => "Trace",
            StepTab::Changes => "Changes",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Pane,
}

/// What `x` asks before it stops anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    StopRun,
    /// One session: its id in the App and its label.
    StopSession {
        session_id: usize,
        label: String,
    },
}

impl Confirm {
    pub fn question(&self) -> String {
        match self {
            Confirm::StopRun => "Stop this run? [y/n]".into(),
            Confirm::StopSession { label, .. } => {
                format!("Stop the session {label}? The run goes on. [y/n]")
            }
        }
    }
}

/// What a session printed, decoded into lines (`workflows::live`). It is
/// decoded again only when the session has printed more.
#[derive(Debug, Clone, Default)]
pub struct Tail {
    /// The bytes the session had printed when this was decoded.
    pub total: u64,
    /// Read from the running session; else from the output the run kept.
    pub running: bool,
    pub lines: Vec<LiveLine>,
}

/// One run the picker (`o`) offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickItem {
    pub run_id: String,
    pub name: String,
    /// `running 3m`, `finished · 2h ago`.
    pub word: String,
    pub live: bool,
}

/// The run picker: live runs first, this folder's first among them, then
/// the runs that ended lately.
#[derive(Debug, Clone, Default)]
pub struct Picker {
    pub items: Vec<PickItem>,
    pub cursor: usize,
}

/// What a session received and left, by its label.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionFacts {
    /// The item of a fan-out it ran for; null otherwise.
    pub item: Value,
    pub answer: Option<Value>,
    pub worktree: Option<String>,
    pub changed_files: Vec<String>,
}

/// One reading of the run: the tree and what the tabs say.
#[derive(Debug, Clone)]
pub struct RunData {
    pub tree: RunTree,
    /// The run is in this App's memory; else it is read from the store.
    pub live: bool,
    pub workspace: String,
    pub profile: String,
    pub document: String,
    pub doc: Option<Workflow>,
    /// Why the run failed.
    pub error: Option<String>,
    /// The run this one resumed.
    pub resumed_from: Option<String>,
    pub sessions: BTreeMap<String, SessionFacts>,
    /// The labels of the sessions running now, for the Live mosaic.
    pub running: Vec<String>,
    /// The run's directory, where the output of ended sessions is kept.
    pub run_dir: std::path::PathBuf,
    /// The Result tab, built with the run in hand.
    pub result: Vec<Line<'static>>,
    /// The stored run, which `r` resumes.
    pub stored: Option<WorkflowRun>,
    pub now_ns: i64,
}

pub struct RunScreen {
    pub run_id: String,
    pub origin: Origin,
    pub data: RunData,
    /// The cursor, as an index into `visible()`.
    pub cursor: usize,
    /// The steps the user folded or opened, by id; the others fold once
    /// they have ended.
    folds: BTreeMap<String, bool>,
    pub run_tab: RunTab,
    pub step_tab: StepTab,
    pub focus: Focus,
    pub scroll: usize,
    /// How far the pane can scroll, as the renderer last measured it.
    pub max_scroll: Cell<usize>,
    pub confirm: Option<Confirm>,
    /// What each session printed, by label, as far as it was asked for.
    pub tails: BTreeMap<String, Tail>,
    /// The Trace tab's lines and the launch they are of.
    pub trace: Option<(String, Vec<String>)>,
    /// The Live tab stays on the newest line until the user scrolls up.
    pub follow: bool,
    pub picker: Option<Picker>,
    pub last_refresh: Instant,
}

impl std::fmt::Debug for RunScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunScreen")
            .field("run_id", &self.run_id)
            .field("cursor", &self.cursor)
            .finish()
    }
}

/// What a row is, across two readings of the run.
fn row_key(row: &Row) -> (RowKind, String, Option<String>) {
    match row.kind {
        RowKind::Run => (RowKind::Run, String::new(), None),
        RowKind::Step => (RowKind::Step, row.step_id.clone(), None),
        RowKind::Session => (RowKind::Session, row.step_id.clone(), row.session.clone()),
    }
}

impl RunScreen {
    pub fn new(run_id: &str, origin: Origin, data: RunData) -> RunScreen {
        RunScreen {
            run_id: run_id.to_string(),
            origin,
            data,
            cursor: 0,
            folds: BTreeMap::new(),
            run_tab: RunTab::Board,
            step_tab: StepTab::Step,
            focus: Focus::Tree,
            scroll: 0,
            max_scroll: Cell::new(usize::MAX),
            confirm: None,
            tails: BTreeMap::new(),
            trace: None,
            follow: true,
            picker: None,
            last_refresh: Instant::now(),
        }
    }

    /// The step has session rows under it.
    pub fn has_children(&self, step_id: &str) -> bool {
        self.data.tree.sessions_of(step_id).next().is_some()
    }

    /// A finished step folds, the running one opens, until the user says
    /// otherwise.
    pub fn folded(&self, step: &Row) -> bool {
        self.folds
            .get(&step.step_id)
            .copied()
            .unwrap_or_else(|| step.state.ended())
    }

    /// The rows drawn, as indexes into the tree's rows.
    pub fn visible(&self) -> Vec<usize> {
        let rows = &self.data.tree.rows;
        let mut out = Vec::with_capacity(rows.len());
        let mut hidden = false;
        for (i, row) in rows.iter().enumerate() {
            match row.kind {
                RowKind::Run => out.push(i),
                RowKind::Step => {
                    hidden = self.folded(row);
                    out.push(i);
                }
                RowKind::Session if !hidden => out.push(i),
                RowKind::Session => {}
            }
        }
        out
    }

    /// The row under the cursor.
    pub fn selected(&self) -> &Row {
        let visible = self.visible();
        let i = visible
            .get(self.cursor.min(visible.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0);
        &self.data.tree.rows[i]
    }

    pub fn on_run_row(&self) -> bool {
        self.selected().kind == RowKind::Run
    }

    /// The names of the selected row's tabs, and which one is open.
    pub fn tabs(&self) -> (Vec<&'static str>, usize) {
        if self.on_run_row() {
            (
                RunTab::ALL.iter().map(|t| t.label()).collect(),
                RunTab::ALL
                    .iter()
                    .position(|t| *t == self.run_tab)
                    .unwrap_or(0),
            )
        } else {
            (
                StepTab::ALL.iter().map(|t| t.label()).collect(),
                StepTab::ALL
                    .iter()
                    .position(|t| *t == self.step_tab)
                    .unwrap_or(0),
            )
        }
    }

    /// `1`-`9`: the tab with that number, when the row has one.
    fn set_tab(&mut self, n: usize) {
        if self.on_run_row() {
            if let Some(t) = RunTab::ALL.get(n) {
                self.run_tab = *t;
                self.scroll = 0;
                self.follow = true;
            }
        } else if let Some(t) = StepTab::ALL.get(n) {
            self.step_tab = *t;
            self.scroll = 0;
            self.follow = true;
        }
    }

    fn put_cursor(&mut self, at: usize) {
        let n = self.visible().len();
        let at = at.min(n.saturating_sub(1));
        if at != self.cursor {
            self.cursor = at;
            self.scroll = 0;
            self.follow = true;
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let at = self.cursor.saturating_add_signed(delta);
        self.put_cursor(at);
    }

    /// Puts the cursor on the row with `key`, else on its step, else
    /// leaves it where it is.
    fn seek(&mut self, key: &(RowKind, String, Option<String>)) {
        let visible = self.visible();
        let rows = &self.data.tree.rows;
        let at = visible
            .iter()
            .position(|i| row_key(&rows[*i]) == *key)
            .or_else(|| {
                visible
                    .iter()
                    .position(|i| rows[*i].kind == RowKind::Step && rows[*i].step_id == key.1)
            });
        match at {
            Some(i) => self.cursor = i,
            None => self.cursor = self.cursor.min(visible.len().saturating_sub(1)),
        }
    }

    /// A new reading of the run; the cursor stays on its row.
    pub fn set_data(&mut self, data: RunData) {
        let key = row_key(self.selected());
        self.data = data;
        self.seek(&key);
        self.last_refresh = Instant::now();
    }

    /// `←`: folds the step under the cursor; from a session, goes to its
    /// step.
    fn fold(&mut self) {
        let row = self.selected().clone();
        match row.kind {
            RowKind::Run => {}
            RowKind::Step => {
                if self.has_children(&row.step_id) {
                    self.folds.insert(row.step_id, true);
                }
            }
            RowKind::Session => {
                self.seek(&(RowKind::Step, row.step_id.clone(), None));
                self.scroll = 0;
            }
        }
    }

    /// `→`: opens the step under the cursor; an open one moves the cursor
    /// onto its first session.
    fn unfold(&mut self) {
        let row = self.selected().clone();
        if row.kind != RowKind::Step || !self.has_children(&row.step_id) {
            return;
        }
        if self.folded(&row) {
            self.folds.insert(row.step_id, false);
        } else {
            self.move_cursor(1);
        }
    }

    /// `Enter`: a step with sessions opens or folds; any other row hands
    /// the keyboard to its pane.
    fn enter(&mut self) {
        let row = self.selected().clone();
        if row.kind == RowKind::Step && self.has_children(&row.step_id) {
            let folded = self.folded(&row);
            self.folds.insert(row.step_id, !folded);
        } else {
            self.focus = Focus::Pane;
        }
    }

    /// The pane shows a session's output as a tail.
    pub fn on_live_tail(&self) -> bool {
        !self.on_run_row() && self.step_tab == StepTab::Live
    }

    fn scroll_by(&mut self, delta: isize) {
        // a tail that follows is at its end: scrolling starts from there
        if self.on_live_tail() && self.follow {
            self.scroll = self.max_scroll.get();
        }
        self.scroll = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll.get());
        // up leaves the newest line; reaching the end follows it again
        self.follow = delta > 0 && self.scroll >= self.max_scroll.get();
    }

    /// The session whose output the selected row's Live tab shows: its
    /// label, when the row is one session.
    pub fn tail(&self) -> Option<&Tail> {
        self.selected()
            .session
            .as_ref()
            .and_then(|l| self.tails.get(l))
    }

    /// The pane's title: what the row is and where the run stands.
    pub fn pane_title(&self) -> String {
        let tree = &self.data.tree;
        let row = self.selected();
        let at = format!(
            "step {}/{}",
            tree.totals.steps_done, tree.totals.steps_total
        );
        match row.kind {
            RowKind::Run => format!(
                "{} · {}{} · {at}",
                row.label,
                tree.status.label(),
                elapsed(row, self.data.now_ns)
                    .map(|d| format!(" {d}"))
                    .unwrap_or_default()
            ),
            RowKind::Step => {
                let shape = row
                    .shape
                    .as_ref()
                    .map(|s| s.words.clone())
                    .unwrap_or_default();
                format!("{} · {shape} · {}", row.label, row.state.label())
            }
            // the step and the session's place in it: `find [1]/vote1`
            RowKind::Session => format!("{} {} · {}", row.step_id, row.label, row.state.label()),
        }
    }
}

/// How long a row ran, or has been running: `42s`, `3m 12s`.
pub fn elapsed(row: &Row, now_ns: i64) -> Option<String> {
    let start = row.started_ns?;
    let end = match row.ended_ns {
        Some(e) => e,
        None if row.state.ended() => return None,
        None => now_ns,
    };
    Some(report::format_duration(
        (end - start).max(0) / 1_000_000_000,
    ))
}

// ---- the text of the tabs -------------------------------------------------

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn key_style() -> Style {
    Style::default().fg(Color::Yellow)
}

fn field(k: &str, v: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {k:<10} "), key_style()),
        Span::raw(v),
    ])
}

fn pretty(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// A heading, then the value's lines under it.
fn block(lines: &mut Vec<Line<'static>>, heading: &str, text: &str) {
    lines.push(Line::raw(""));
    lines.push(Line::styled(format!("  {heading}"), key_style()));
    for l in text.lines() {
        lines.push(Line::raw(format!("    {l}")));
    }
}

/// What a session of `step` was asked: the step's own skill or prompt,
/// the refuters' or the judge's.
fn asked(step: &Step, role: &Role) -> Option<String> {
    let actor = match role {
        Role::Main | Role::Generate(_) => step.actor.as_ref(),
        Role::Vote(_) => step.verify.as_ref().map(|v| &v.actor),
        Role::Judge { .. } => step.judge.as_ref(),
    }?;
    Some(match actor {
        Actor::Skill(s) => format!("skill {s}"),
        Actor::Prompt(p) => format!("prompt\n{p}"),
    })
}

fn who_lines(lines: &mut Vec<Line<'static>>, row: &Row, profile: &str) {
    let who = &row.who;
    lines.push(field(
        "harness",
        if who.inherited {
            format!("{} (the flow's)", who.harness)
        } else {
            who.harness.clone()
        },
    ));
    if !profile.is_empty() {
        lines.push(field("profile", profile.to_string()));
    }
    lines.push(field(
        "agent",
        who.agent.clone().unwrap_or_else(|| "none".into()),
    ));
    if let Some(m) = &who.model {
        lines.push(field("model", m.clone()));
    }
    if let Some(e) = &who.effort {
        lines.push(field("effort", e.clone()));
    }
}

fn state_line(row: &Row, now_ns: i64) -> Line<'static> {
    let mut parts = vec![format!("{} {}", row.state.glyph(), row.state.label())];
    if let Some(d) = elapsed(row, now_ns) {
        parts.push(d);
    }
    if row.tokens > 0 {
        parts.push(format!("{} tokens", tokens_short(row.tokens)));
    }
    if row.cost_usd > 0.0 {
        parts.push(format!("${:.2}", row.cost_usd));
    }
    field("state", parts.join(" · "))
}

/// The Step tab of a step or session row.
pub fn step_lines(data: &RunData, row: &Row) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let step = data.doc.as_ref().and_then(|d| d.step(&row.step_id));
    // the step's own profile, else the run's when it runs on the run's
    // harness; a step on another harness takes that harness's default
    let profile = step
        .and_then(|s| s.profile.clone())
        .or_else(|| row.who.inherited.then(|| data.profile.clone()))
        .unwrap_or_default();
    who_lines(&mut lines, row, &profile);
    lines.push(state_line(row, data.now_ns));
    if let Some(shape) = row.shape.as_ref().filter(|s| !s.words.is_empty()) {
        lines.push(field("runs", shape.words.clone()));
    }
    if let Some(l) = &row.launch_id {
        lines.push(field("launch", l.clone()));
    }
    let Some(label) = &row.session else {
        // a step with several sessions: what each role is asked, and them
        if let Some(step) = step {
            for (name, role) in [
                ("asked", Role::Main),
                ("refuters", Role::Vote(0)),
                ("judge", Role::Judge { round: 0, pair: 0 }),
            ] {
                if let Some(a) = asked(step, &role) {
                    block(&mut lines, name, &a);
                }
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("  sessions   {} of {} ended", row.done, row.planned),
            key_style(),
        ));
        for s in data.tree.sessions_of(&row.step_id) {
            lines.push(Line::from(vec![
                Span::raw(format!("    {} {:<14} ", s.state.glyph(), s.label)),
                Span::styled(
                    format!(
                        "{}{}",
                        s.who.short(),
                        if s.tokens > 0 {
                            format!(" · {} tokens", tokens_short(s.tokens))
                        } else {
                            String::new()
                        }
                    ),
                    dim(),
                ),
            ]));
        }
        if row.planned > 0 && data.tree.sessions_of(&row.step_id).next().is_none() {
            lines.push(Line::styled("    none started yet", dim()));
        }
        return lines;
    };
    let role = SessionKey::parse(&row.step_id, label)
        .map(|k| k.role)
        .unwrap_or(Role::Main);
    if let Some(a) = step.and_then(|s| asked(s, &role)) {
        block(&mut lines, "asked", &a);
    }
    let facts = data.sessions.get(label);
    if let Some(item) = facts.map(|f| &f.item).filter(|i| !i.is_null()) {
        block(&mut lines, "received", &pretty(item));
    }
    match (facts.and_then(|f| f.answer.as_ref()), &row.null_reason) {
        (Some(a), _) => block(&mut lines, "answered", &pretty(a)),
        (None, Some(why)) => block(&mut lines, "no answer", why),
        (None, None) if row.state == State::NullAnswer => {
            block(&mut lines, "no answer", "it gave no reason")
        }
        (None, None) if row.state.ended() => {}
        (None, None) => {
            lines.push(Line::raw(""));
            lines.push(Line::styled("  it has not answered yet", dim()));
        }
    }
    lines
}

/// The Changes tab: the files the row's sessions changed.
pub fn changes_lines(data: &RunData, row: &Row) -> Vec<Line<'static>> {
    let labels: Vec<String> = match &row.session {
        Some(l) => vec![l.clone()],
        None => data
            .tree
            .sessions_of(&row.step_id)
            .filter_map(|s| s.session.clone())
            .collect(),
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    for label in labels {
        let Some(f) = data.sessions.get(&label) else {
            continue;
        };
        if f.worktree.is_none() && f.changed_files.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(format!("  {label}"), key_style()));
        if let Some(w) = &f.worktree {
            lines.push(Line::styled(format!("    worktree {w}"), dim()));
        }
        if f.changed_files.is_empty() {
            lines.push(Line::styled("    no files changed", dim()));
        }
        for file in &f.changed_files {
            lines.push(Line::raw(format!("    {file}")));
        }
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "  No files changed: it did not edit in a worktree.",
            dim(),
        ));
    }
    lines
}

/// The Document tab: the TOML the run started from.
pub fn document_lines(data: &RunData) -> Vec<Line<'static>> {
    data.document
        .lines()
        .map(|l| Line::raw(format!("  {l}")))
        .collect()
}

// ---- reading a run ---------------------------------------------------------

fn files_of(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn facts_of_rows(rows: &[WorkflowStep]) -> BTreeMap<String, SessionFacts> {
    rows.iter()
        .map(|r| {
            (
                r.session.clone(),
                SessionFacts {
                    item: r.item.clone(),
                    answer: (r.kind != "running" && r.kind != "null").then(|| r.result.clone()),
                    worktree: r.worktree.clone(),
                    changed_files: files_of(&r.changed_files),
                },
            )
        })
        .collect()
}

fn notes_lines(lines: &mut Vec<Line<'static>>, notes: &[String]) {
    if notes.is_empty() {
        return;
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("  Notes", key_style()));
    for n in notes {
        lines.push(Line::styled(format!("    · {n}"), dim()));
    }
}

impl App {
    /// The run as it stands now: from memory while it runs here, else
    /// from the store.
    pub fn run_screen_data(&self, run_id: &str) -> Result<RunData, String> {
        use super::run_report::{headline_lines, report_lines, rule, value_lines};
        let conn = self
            .trace_db_path
            .as_deref()
            .and_then(|p| crate::tracing::store::open_ro(p).ok());
        let rows: Vec<WorkflowStep> = conn
            .as_ref()
            .and_then(|c| wstore::steps_of(c, run_id).ok())
            .unwrap_or_default();
        let now_ns = crate::tracing::store::now_ns();
        let red = Style::default().fg(Color::Red);
        if let Some(r) = self.live_workflow_runs.iter().find(|r| r.run_id == run_id) {
            let journal = journal::load(&r.run_dir.join("journal.jsonl"));
            let mut sessions: Vec<LiveSession<'_>> = r
                .ended
                .iter()
                .map(|s| LiveSession {
                    spec: &s.spec,
                    harness: s.harness.as_str(),
                    exited: true,
                    launch_id: s.launch_id.as_deref(),
                    started_ns: Some(s.started_ns),
                    ended_ns: Some(s.ended_ns),
                })
                .collect();
            sessions.extend(r.sessions.iter().map(|s| LiveSession {
                spec: &s.spec,
                harness: s.harness.as_str(),
                exited: s.exited_at.is_some(),
                launch_id: s.launch_id.as_deref(),
                started_ns: Some(s.started_ns),
                ended_ns: None,
            }));
            let tree = run_view::from_live(&LiveRun {
                run_id: &r.run_id,
                name: &r.name,
                state: &r.state,
                harness: r.harness.as_str(),
                sessions: &sessions,
                journal: &journal,
                usd_cap: r.usd_cap,
                started_ns: Some(r.started_ns),
            });
            // the store's rows, then what memory knows that they may not
            let mut facts = facts_of_rows(&rows);
            for s in r
                .ended
                .iter()
                .map(|s| &s.spec)
                .chain(r.sessions.iter().map(|s| &s.spec))
            {
                facts.entry(s.label()).or_default().item = s.item.clone();
            }
            for rec in &r.state.records {
                let f = facts.entry(rec.key.label()).or_default();
                f.answer = (rec.outcome.kind() != "null").then(|| rec.outcome.value());
            }
            let status = r.state.status();
            let (word, value) = match &status {
                RunStatus::Running => ("running", Value::Null),
                RunStatus::Finished(v) => ("finished", v.clone()),
                RunStatus::BudgetExhausted(v) => ("budget-exhausted", v.clone()),
                RunStatus::Failed(_) => ("failed", Value::Null),
                RunStatus::Cancelled => ("cancelled", Value::Null),
            };
            let workspace = r.workspace.display().to_string();
            let rep = report::build(RunView {
                workflow: &r.name,
                status: word,
                harness: r.harness.as_str(),
                workspace: &workspace,
                sessions: r.state.records.len() as i64,
                tokens: r.state.tokens_spent,
                cost_usd: Some(r.state.cost_spent),
                duration_s: None,
                result: &value,
                error: None,
                notes: &r.state.notes,
                doc: Some(&r.state.doc),
                steps: &[],
            });
            let mut result = headline_lines(&rep);
            result.push(rule());
            match &status {
                RunStatus::Running => {
                    result.push(Line::styled(
                        "  still running · the steps that have finished so far",
                        dim(),
                    ));
                    for (step, state) in r.state.step_states() {
                        if state != "done" {
                            continue;
                        }
                        block(&mut result, &step, &pretty(&r.state.step_result(&step)));
                    }
                }
                RunStatus::Finished(v) | RunStatus::BudgetExhausted(v) => {
                    result.extend(value_lines(&pretty(v)))
                }
                RunStatus::Failed(e) => result.push(Line::styled(format!("  failed · {e}"), red)),
                RunStatus::Cancelled => result.push(Line::styled("  cancelled", dim())),
            }
            notes_lines(&mut result, &r.state.notes);
            return Ok(RunData {
                tree,
                live: true,
                workspace,
                profile: r.profile.name.clone(),
                document: r.document.clone(),
                doc: Some(r.state.doc.clone()),
                error: match &status {
                    RunStatus::Failed(e) => Some(e.clone()),
                    _ => None,
                },
                resumed_from: r.resumed_from.clone(),
                sessions: facts,
                running: r
                    .sessions
                    .iter()
                    .filter(|s| s.exited_at.is_none())
                    .map(|s| s.key.label())
                    .collect(),
                run_dir: r.run_dir.clone(),
                result,
                stored: None,
                now_ns,
            });
        }
        let run = conn
            .as_ref()
            .ok_or("no trace store: a run that has ended is read from it")?;
        let run = wstore::get_run(run, run_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("run {run_id} is not in the store"))?;
        let dir = context::run_dir(&self.workflows_runtime_dir(), run_id);
        let entries = journal::load(&dir.join("journal.jsonl"));
        // what the run recorded it replayed; an older run recorded none
        let replayed = context::load_replayed(&dir);
        let tree = run_view::from_stored(&StoredRun {
            run: &run,
            steps: &rows,
            journal: &entries,
            usd_cap: None,
            replayed: replayed.as_deref(),
        });
        let mut facts = facts_of_rows(&rows);
        // a replayed session has no row of its own: its answer is the
        // journal's
        for e in &entries {
            facts.entry(e.key.clone()).or_insert_with(|| SessionFacts {
                answer: (e.kind != "null").then(|| e.result.clone()),
                ..SessionFacts::default()
            });
        }
        let mut notes: Vec<String> = std::fs::read_to_string(dir.join("result.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("notes").and_then(|n| n.as_array()).cloned())
            .map(|a| {
                a.iter()
                    .filter_map(|n| n.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if notes.is_empty()
            && let Some(rec) = self
                .recent_workflow_runs
                .iter()
                .find(|x| x.run_id == run_id)
        {
            notes = rec.notes.clone();
        }
        let doc = crate::workflows::document::parse(&run.document).ok();
        let rep = report::build(RunView {
            workflow: &run.workflow,
            status: &run.status,
            harness: &run.harness,
            workspace: &run.workspace,
            sessions: run.sessions,
            tokens: run.tokens.unwrap_or(0).max(0) as u64,
            cost_usd: run.cost_usd,
            duration_s: run.ended_ns.map(|e| (e - run.started_ns) / 1_000_000_000),
            result: &run.result,
            error: run.error.as_deref(),
            notes: &notes,
            doc: doc.as_ref(),
            steps: &rows,
        });
        let mut result = headline_lines(&rep);
        result.push(Line::styled(
            format!(
                "  {} · {} · {}",
                report::format_range(run.started_ns, run.ended_ns),
                run.harness,
                run.workspace
            ),
            dim(),
        ));
        if let Some(e) = &run.error {
            result.push(Line::styled(format!("  Error     {e}"), red));
        }
        result.push(rule());
        result.extend(report_lines(&rep));
        if !run.result.is_null() {
            // the report above is an outline; this is every word of it
            block(&mut result, "The answer in full", &pretty(&run.result));
        }
        Ok(RunData {
            tree,
            live: false,
            workspace: run.workspace.clone(),
            profile: run.profile.clone(),
            document: run.document.clone(),
            doc,
            error: run.error.clone(),
            resumed_from: run.resumed_from.clone(),
            sessions: facts,
            running: Vec::new(),
            run_dir: dir,
            result,
            stored: Some(run),
            now_ns,
        })
    }

    /// Opens the Run screen on run `run_id`; `false`, with the reason as
    /// a notice, when the run cannot be read.
    pub fn open_run_screen(&mut self, run_id: &str, origin: Origin) -> bool {
        match self.run_screen_data(run_id) {
            Ok(data) => {
                let mut st = RunScreen::new(run_id, origin, data);
                self.fill_run_screen(&mut st, true);
                self.mode = Mode::Run(Box::new(st));
                true
            }
            Err(e) => {
                self.notice = Some(Notice::warn(e));
                false
            }
        }
    }

    fn reload_run_screen(&mut self, st: &mut RunScreen) {
        match self.run_screen_data(&st.run_id) {
            Ok(data) => {
                st.set_data(data);
                self.fill_run_screen(st, true);
            }
            Err(e) => {
                // what was read stays on the screen, as the run's last word
                st.data.live = false;
                st.last_refresh = Instant::now();
                self.notice = Some(Notice::warn(e));
            }
        }
    }

    /// What the Live and Trace tabs show, as far as the screen needs it
    /// now: the output of the sessions running (decoded again only when
    /// one printed more), the kept output of the selected session once it
    /// has ended, and the selected session's trace. `refresh`: the run was
    /// just read again, so a running session's trace is read again too.
    fn fill_run_screen(&self, st: &mut RunScreen, refresh: bool) {
        let run = self
            .live_workflow_runs
            .iter()
            .find(|r| r.run_id == st.run_id);
        if let Some(r) = run {
            for s in &r.sessions {
                let label = s.key.label();
                let total = s.stdout.total();
                if st
                    .tails
                    .get(&label)
                    .is_none_or(|t| t.total != total || !t.running)
                {
                    // an OpenCode stream may carry a second session's
                    // events: the decoder keeps the first id it sees,
                    // which is this session's own
                    let lines = live::lines_for(s.harness, s.stdout.snapshot(), None);
                    st.tails.insert(
                        label,
                        Tail {
                            total,
                            running: true,
                            lines,
                        },
                    );
                }
            }
        }
        if st.on_run_row() {
            return;
        }
        let row = st.selected().clone();
        if st.step_tab == StepTab::Live
            && let Some(label) = &row.session
        {
            let running = run.is_some_and(|r| r.sessions.iter().any(|s| s.key.label() == *label));
            if !running && st.tails.get(label).is_none_or(|t| t.running) {
                let bytes = std::fs::read(context::output_file(&st.data.run_dir, label))
                    .unwrap_or_default();
                let lines = match Harness::detect(&row.who.harness) {
                    Some(h) => live::lines_for(h, &bytes, None),
                    None => Vec::new(),
                };
                st.tails.insert(
                    label.clone(),
                    Tail {
                        total: bytes.len() as u64,
                        running: false,
                        lines,
                    },
                );
            }
        }
        if st.step_tab == StepTab::Trace {
            let Some(launch) = &row.launch_id else {
                st.trace = None;
                return;
            };
            let cached = st.trace.as_ref().is_some_and(|(l, _)| l == launch);
            if cached && (!refresh || row.state.ended()) {
                return;
            }
            let lines = self
                .trace_db_path
                .as_deref()
                .and_then(|p| crate::tracing::store::open_ro(p).ok())
                .and_then(|c| {
                    crate::tracing::swarm_report::launch_trace_lines(&c, launch, None).ok()
                })
                .unwrap_or_else(|| vec!["  The trace store could not be read.".into()]);
            st.trace = Some((launch.clone(), lines));
        }
    }

    /// The runs `o` offers: live ones first, this folder's first among
    /// them, then the runs that ended lately.
    fn run_picker(&self, st: &RunScreen) -> Picker {
        let now = crate::tracing::store::now_ns();
        let mut live: Vec<&super::workflows::LiveWorkflowRun> =
            self.live_workflow_runs.iter().collect();
        let here = |r: &&super::workflows::LiveWorkflowRun| {
            r.workspace.display().to_string() != st.data.workspace
        };
        live.sort_by_key(here);
        let mut items: Vec<PickItem> = live
            .iter()
            .map(|r| PickItem {
                run_id: r.run_id.clone(),
                name: r.name.clone(),
                word: format!(
                    "running {}",
                    super::loops::short_duration(r.started.elapsed().as_secs())
                ),
                live: true,
            })
            .collect();
        let stored = self
            .trace_db_path
            .as_deref()
            .and_then(|p| crate::tracing::store::open_ro(p).ok())
            .and_then(|c| wstore::recent_runs(&c, None, 20).ok())
            .unwrap_or_default();
        for r in stored {
            if items.iter().any(|i| i.run_id == r.id) {
                continue;
            }
            items.push(PickItem {
                word: format!(
                    "{} · {}",
                    r.status,
                    super::runs_view::ago(r.ended_ns.unwrap_or(r.started_ns), now)
                ),
                run_id: r.id,
                name: r.workflow,
                live: false,
            });
        }
        let cursor = items
            .iter()
            .position(|i| i.run_id == st.run_id)
            .unwrap_or(0);
        Picker { items, cursor }
    }

    /// Ticks the open screen: a run that is going moves, a finished one
    /// is read once more and then left alone.
    pub fn refresh_run_screen(&mut self, now: Instant) {
        let due = match &self.mode {
            Mode::Run(st) => {
                let every = if st.data.live {
                    REFRESH_LIVE
                } else if st.data.tree.status == Status::Running {
                    REFRESH_STORED
                } else {
                    return;
                };
                now.saturating_duration_since(st.last_refresh) >= every
            }
            _ => false,
        };
        if !due {
            return;
        }
        if let Mode::Run(mut st) = std::mem::replace(&mut self.mode, Mode::Main) {
            self.reload_run_screen(&mut st);
            self.mode = Mode::Run(st);
        }
    }

    pub fn handle_run_screen_key(&mut self, key: &KeyEvent) {
        let Mode::Run(mut st) = std::mem::replace(&mut self.mode, Mode::Main) else {
            return;
        };
        if let Some(confirm) = st.confirm.take() {
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                self.run_screen_stop_confirmed(&mut st, confirm);
            }
            self.mode = Mode::Run(st);
            return;
        }
        if let Some(mut picker) = st.picker.take() {
            let n = picker.items.len();
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    picker.cursor = (picker.cursor + 1).min(n.saturating_sub(1))
                }
                KeyCode::Up | KeyCode::Char('k') => picker.cursor = picker.cursor.saturating_sub(1),
                KeyCode::Enter => {
                    // the other run takes the screen, opened from the same place
                    if let Some(item) = picker.items.get(picker.cursor)
                        && self.open_run_screen(&item.run_id, st.origin)
                    {
                        return;
                    }
                    self.mode = Mode::Run(st);
                    return;
                }
                KeyCode::Esc | KeyCode::Char('o' | 'q') => {
                    self.mode = Mode::Run(st);
                    return;
                }
                _ => {}
            }
            st.picker = Some(picker);
            self.mode = Mode::Run(st);
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let pane = st.focus == Focus::Pane;
        let mut stay = true;
        match key.code {
            KeyCode::Char('o') if !ctrl => st.picker = Some(self.run_picker(&st)),
            KeyCode::Char(c @ '1'..='9') => st.set_tab(c as usize - '1' as usize),
            KeyCode::Tab | KeyCode::BackTab => {
                st.focus = if pane { Focus::Tree } else { Focus::Pane }
            }
            KeyCode::Down | KeyCode::Char('j') if pane => st.scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') if pane => st.scroll_by(-1),
            KeyCode::Down | KeyCode::Char('j') => st.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => st.move_cursor(-1),
            KeyCode::Left | KeyCode::Char('h') if !pane => st.fold(),
            KeyCode::Right | KeyCode::Char('l') if !pane => st.unfold(),
            KeyCode::Enter if !pane => st.enter(),
            KeyCode::Char('x') if !ctrl => self.run_screen_stop(&mut st),
            KeyCode::Char('r') if !ctrl => stay = !self.run_screen_resume(&st),
            KeyCode::Char('e') if !ctrl => stay = !self.run_screen_edit(&st),
            _ => match verb(&crate::keymap::list_alias(key)) {
                Some(Verb::Back) if pane => st.focus = Focus::Tree,
                Some(Verb::Back) => {
                    stay = false;
                    if st.origin == Origin::Runs {
                        self.open_runs_view_on(
                            &format!("flow:{}", st.run_id),
                            super::runs_view::RunTab::Report,
                        );
                    }
                }
                Some(Verb::Top) if pane => {
                    st.scroll = 0;
                    st.follow = false;
                }
                Some(Verb::Bottom) if pane => {
                    st.scroll = st.max_scroll.get();
                    st.follow = true;
                }
                Some(Verb::Top) => st.put_cursor(0),
                Some(Verb::Bottom) => st.put_cursor(usize::MAX),
                Some(Verb::PageDown) => st.scroll_by(10),
                Some(Verb::PageUp) => st.scroll_by(-10),
                Some(Verb::Reload) => self.reload_run_screen(&mut st),
                _ => {}
            },
        }
        // an action that opened another screen keeps it
        if stay && matches!(self.mode, Mode::Main) {
            // the row or the tab may have changed: what it shows is read
            self.fill_run_screen(&mut st, false);
            self.mode = Mode::Run(st);
        }
    }

    /// `x`: asks before it stops the run (the run row) or one session (a
    /// step that is one session, or a session row).
    fn run_screen_stop(&mut self, st: &mut RunScreen) {
        let Some(run) = self
            .live_workflow_runs
            .iter()
            .find(|r| r.run_id == st.run_id)
        else {
            self.notice = Some(Notice::info("x stops a run that is running here"));
            return;
        };
        let row = st.selected();
        if row.kind == RowKind::Run {
            st.confirm = Some(Confirm::StopRun);
            return;
        }
        // what the row is called on the screen: `fan [1]`, or the step's id
        let name = match row.kind {
            RowKind::Session => format!("{} {}", row.step_id, row.label),
            _ => row.label.clone(),
        };
        let Some(label) = row.session.clone() else {
            self.notice = Some(Notice::info(
                "x stops one session on its own row, the whole run on the run's row",
            ));
            return;
        };
        match run
            .sessions
            .iter()
            .find(|s| s.key.label() == label && s.exited_at.is_none())
        {
            Some(s) => {
                st.confirm = Some(Confirm::StopSession {
                    session_id: s.session_id,
                    label: name,
                })
            }
            None => self.notice = Some(Notice::info(format!("{name} is not running"))),
        }
    }

    fn run_screen_stop_confirmed(&mut self, st: &mut RunScreen, confirm: Confirm) {
        match confirm {
            Confirm::StopRun => {
                if self.cancel_workflow_run(&st.run_id) {
                    self.notice = Some(Notice::info("stopping the run"));
                }
            }
            Confirm::StopSession { session_id, label } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == session_id) {
                    s.kill();
                    self.notice = Some(Notice::info(format!("stopping {label}; the run goes on")));
                }
            }
        }
        self.reload_run_screen(st);
    }

    /// `r`: resumes a run that has ended, and follows the new run; `true`
    /// when it started.
    fn run_screen_resume(&mut self, st: &RunScreen) -> bool {
        let Some(run) = st.data.stored.clone() else {
            self.notice = Some(Notice::info("r resumes a run that has ended"));
            return false;
        };
        match self.resume_workflow_run(&run) {
            Ok(id) => {
                self.notice = Some(Notice::info(format!(
                    "resumed as {}",
                    &id[..8.min(id.len())]
                )));
                self.open_run_screen(&id, st.origin)
            }
            Err(e) => {
                self.notice = Some(Notice::warn(e));
                false
            }
        }
    }

    /// `e`: the run's flow in the agent editor; `true` when it opened.
    fn run_screen_edit(&mut self, st: &RunScreen) -> bool {
        let name = st.data.tree.run().label.clone();
        let Some(e) = self.workflow_entries().into_iter().find(|e| e.name == name) else {
            self.notice = Some(Notice::info("the flow of this run is no longer saved"));
            return false;
        };
        let ws = self
            .dialog_workspaces()
            .into_iter()
            .next()
            .unwrap_or_default();
        self.open_flow_builder_text(
            &e.text,
            super::flow_builder::Origin::Document(e.name.clone()),
            Vec::new(),
            ws,
        );
        matches!(self.mode, Mode::AgentEditor(_))
    }
}
