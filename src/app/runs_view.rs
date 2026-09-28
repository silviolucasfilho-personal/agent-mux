//! The runs view (`E` / `W`, spec `docs/superpowers/specs/2026-09-26-agent-first-design.md`,
//! section 4): every run of every agent in one list, **Running**, **Needs
//! you** (the inbox's items) and **Earlier**, with the selected run's
//! Report, History or Steps, Change, Result, and Setup or Document. Loop
//! runs come from `loop_runs`, flow runs from `workflow_runs` and the live
//! runs from the App. The detail tabs are built by the loop and workflow
//! detail builders (`loops_view::LoopsViewState`,
//! `workflows_view::WorkflowsViewState`), which have no screen of their
//! own any more.

use super::inbox::InboxItem;
use super::workflows_view::RunRow;
use super::{App, Mode, Notice};
use crate::keymap::{Verb, verb};
use crate::loops::store::{self as lstore, LoopRun};
use crate::workflows::store::{self as wstore, WorkflowRun, WorkflowStep};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use std::time::{Duration, Instant};

/// How often an open view re-reads the runs.
const REFRESH: Duration = Duration::from_millis(2000);

/// How many stored runs of each kind the Earlier group reads.
const EARLIER: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Running,
    NeedsYou,
    /// The scheduled agents' next runs.
    Next,
    Earlier,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::Running => "Running",
            Group::NeedsYou => "Needs you",
            Group::Next => "Next",
            Group::Earlier => "Earlier",
        }
    }
}

#[derive(Debug, Clone)]
pub enum RunRef {
    /// A loop run in flight (run id, loop id, session id).
    LoopLive {
        run_id: String,
        loop_id: String,
        session_id: usize,
    },
    /// A flow run in flight (run id).
    FlowLive(String),
    /// Something the inbox holds.
    Inbox(InboxItem),
    Loop(Box<LoopRun>),
    Flow(Box<WorkflowRun>),
    /// A scheduled agent's next run (its loop id).
    Next(String),
}

#[derive(Debug, Clone)]
pub struct RunItem {
    pub group: Group,
    /// The agent: a loop's pattern, a flow's name.
    pub agent: String,
    /// When, in words (`12m ago`, `running 3m`).
    pub when: String,
    /// What came of it, in a word or two.
    pub word: String,
    pub glyph: &'static str,
    pub run: RunRef,
    /// For ordering the Earlier group, newest first.
    pub at_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTab {
    Report,
    /// A loop's runs over time (History), a flow's steps (Steps).
    Sessions,
    Change,
    Result,
    /// A loop's setup (readiness, budget, files), a flow's document.
    Setup,
}

impl RunTab {
    pub const ALL: [RunTab; 5] = [
        RunTab::Report,
        RunTab::Sessions,
        RunTab::Change,
        RunTab::Result,
        RunTab::Setup,
    ];

    /// The tab's name for a loop run or a flow run.
    pub fn label(self, flow: bool) -> &'static str {
        match (self, flow) {
            (RunTab::Report, _) => "Report",
            (RunTab::Sessions, false) => "History",
            (RunTab::Sessions, true) => "Steps",
            (RunTab::Change, _) => "Change",
            (RunTab::Result, _) => "Result",
            (RunTab::Setup, false) => "Setup",
            (RunTab::Setup, true) => "Document",
        }
    }
}

impl RunRef {
    /// Stable across reloads, to keep the selection.
    pub fn key(&self) -> String {
        match self {
            RunRef::LoopLive { run_id, .. } => format!("loop:{run_id}"),
            RunRef::FlowLive(id) => format!("flow:{id}"),
            RunRef::Inbox(InboxItem::Loop(r)) => format!("loop:{}", r.id),
            RunRef::Inbox(InboxItem::Plan { id, .. }) => format!("plan:{id}"),
            RunRef::Inbox(InboxItem::Run { run_id, .. }) => format!("flow:{run_id}"),
            RunRef::Loop(r) => format!("loop:{}", r.id),
            RunRef::Flow(r) => format!("flow:{}", r.id),
            RunRef::Next(id) => format!("next:{id}"),
        }
    }

    /// A flow's run or plan, rather than a loop's run.
    pub fn is_flow(&self) -> bool {
        matches!(
            self,
            RunRef::FlowLive(_)
                | RunRef::Flow(_)
                | RunRef::Inbox(InboxItem::Plan { .. } | InboxItem::Run { .. })
        )
    }

    /// The loop a loop run belongs to.
    pub fn loop_id(&self) -> Option<&str> {
        match self {
            RunRef::LoopLive { loop_id, .. } | RunRef::Next(loop_id) => Some(loop_id),
            RunRef::Inbox(InboxItem::Loop(r)) | RunRef::Loop(r) => Some(&r.loop_id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunsViewState {
    pub items: Vec<RunItem>,
    pub selected: usize,
    pub tab: RunTab,
    pub scroll: usize,
    /// Why stored runs could not be read (tracing off, store error).
    pub error: Option<String>,
    /// The selected flow run's steps, read when it is selected.
    pub steps: Vec<WorkflowStep>,
    /// Today's runs, tokens and cost, for the header.
    pub today: (usize, i64, f64),
    /// The selected run's tab as the loop or workflow detail builder
    /// renders it; `None` when this view writes the tab itself.
    pub detail: Option<Vec<Line<'static>>>,
    /// `s`: the library name being typed for a flow's document.
    pub save_name: Option<String>,
    /// `d` on a plan asks first: `y` discards it.
    pub confirm_discard: bool,
    /// How far the detail can scroll, as the renderer last measured it.
    pub max_scroll: std::cell::Cell<usize>,
    pub last_refresh: Instant,
}

impl RunsViewState {
    pub fn selected_item(&self) -> Option<&RunItem> {
        self.items.get(self.selected)
    }
}

/// Consecutive quiet runs of one loop fold into their newest, so the runs
/// that found something stay on the screen: `quiet ×3`.
fn fold_quiet(items: Vec<RunItem>) -> Vec<RunItem> {
    let quiet = |i: &RunItem| match &i.run {
        RunRef::Loop(r) => (r.outcome == crate::loops::Outcome::NoOp && r.decision.is_none())
            .then(|| r.loop_id.clone()),
        _ => None,
    };
    let mut out: Vec<RunItem> = Vec::new();
    let mut count = 0usize;
    for item in items {
        let q = quiet(&item);
        if let (Some(l), Some(last)) = (&q, out.last_mut())
            && quiet(last).as_ref() == Some(l)
        {
            count += 1;
            last.word = format!("quiet ×{}", count + 1);
            continue;
        }
        count = 0;
        out.push(item);
    }
    out
}

/// `12m ago`, `3h ago`, `2d ago`.
pub fn ago(ns: i64, now_ns: i64) -> String {
    let s = (now_ns - ns).max(0) / 1_000_000_000;
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", s / 60),
        3600..86_400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

fn now_ns() -> i64 {
    (crate::loops::now().unix_timestamp_nanos()) as i64
}

fn loop_glyph(r: &LoopRun) -> &'static str {
    use crate::loops::Outcome;
    match r.outcome {
        Outcome::Failed => "✗",
        Outcome::Blocked => "·",
        o if o.needs_human() && r.decision.is_none() => "!",
        _ => "✓",
    }
}

fn flow_glyph(status: &str) -> &'static str {
    match status {
        "done" | "completed" | "succeeded" | "ok" => "✓",
        "running" => "▶",
        _ => "✗",
    }
}

impl App {
    /// Every run the view lists, in its groups.
    pub fn runs_view_items(&self) -> (Vec<RunItem>, Option<String>) {
        let now = now_ns();
        let mut items = Vec::new();
        for r in self.live_loop_runs.iter().filter(|r| r.exited_at.is_none()) {
            items.push(RunItem {
                group: Group::Running,
                agent: r.pattern.clone(),
                when: format!(
                    "running {}",
                    super::loops::short_duration(r.started.elapsed().as_secs())
                ),
                word: r.effective_level.label().to_string(),
                glyph: "▶",
                run: RunRef::LoopLive {
                    run_id: r.run_id.clone(),
                    loop_id: r.loop_id.clone(),
                    session_id: r.session_id,
                },
                at_ns: now,
            });
        }
        for r in &self.live_workflow_runs {
            items.push(RunItem {
                group: Group::Running,
                agent: r.name.clone(),
                when: format!(
                    "running {}",
                    super::loops::short_duration(r.started.elapsed().as_secs())
                ),
                word: format!("{} session(s)", r.sessions.len()),
                glyph: "▶",
                run: RunRef::FlowLive(r.run_id.clone()),
                at_ns: now,
            });
        }
        let (inbox, mut error) = self.inbox_items();
        let mut waiting: Vec<String> = Vec::new();
        for item in inbox {
            let (agent, when, id) = match &item {
                InboxItem::Loop(r) => (r.pattern.clone(), ago(r.scheduled_ns, now), r.id.clone()),
                InboxItem::Plan { name, id, .. } => (name.clone(), "planned".into(), id.clone()),
                InboxItem::Run { name, run_id, .. } => {
                    (name.clone(), "since startup".into(), run_id.clone())
                }
            };
            waiting.push(id);
            let at_ns = match &item {
                InboxItem::Loop(r) => r.scheduled_ns,
                _ => now,
            };
            items.push(RunItem {
                group: Group::NeedsYou,
                agent,
                when,
                word: item.word().to_string(),
                glyph: "!",
                run: RunRef::Inbox(item),
                at_ns,
            });
        }
        let live: Vec<String> = items
            .iter()
            .filter_map(|i| match &i.run {
                RunRef::LoopLive { run_id, .. } => Some(run_id.clone()),
                RunRef::FlowLive(id) => Some(id.clone()),
                _ => None,
            })
            .collect();
        // what runs next, soonest first; a loop running now is under Running
        let mut next: Vec<RunItem> = self
            .loop_registry
            .loops
            .iter()
            .filter(|l| {
                !self
                    .live_loop_runs
                    .iter()
                    .any(|r| r.loop_id == l.id && r.exited_at.is_none())
            })
            .map(|l| {
                let at = l.next_run().map(|t| t.unix_timestamp_nanos() as i64);
                let word = if self.loop_registry.pause_all || l.paused() {
                    "paused".to_string()
                } else {
                    match at {
                        Some(t) if t > now => format!(
                            "in {}",
                            super::loops::short_duration(((t - now) / 1_000_000_000) as u64)
                        ),
                        Some(_) => "due now".into(),
                        None => "not scheduled".into(),
                    }
                };
                RunItem {
                    group: Group::Next,
                    agent: l.pattern.clone(),
                    when: l.workspace_name(),
                    glyph: if word == "paused" { "‖" } else { "⟳" },
                    word,
                    run: RunRef::Next(l.id.clone()),
                    at_ns: at.unwrap_or(i64::MAX),
                }
            })
            .collect();
        next.sort_by_key(|i| i.at_ns);
        items.extend(next);
        let mut earlier: Vec<RunItem> = Vec::new();
        match self.trace_db_path.as_deref() {
            None => {}
            Some(p) => match crate::tracing::store::open_ro(p) {
                Ok(c) => {
                    for r in lstore::all_recent_runs(&c, EARLIER).unwrap_or_default() {
                        if waiting.contains(&r.id) || live.contains(&r.id) {
                            continue;
                        }
                        earlier.push(RunItem {
                            group: Group::Earlier,
                            agent: r.pattern.clone(),
                            when: ago(r.scheduled_ns, now),
                            word: match &r.decision {
                                Some(d) => d.clone(),
                                None => r.outcome.word().to_string(),
                            },
                            glyph: loop_glyph(&r),
                            at_ns: r.scheduled_ns,
                            run: RunRef::Loop(Box::new(r)),
                        });
                    }
                    for r in wstore::recent_runs(&c, None, EARLIER).unwrap_or_default() {
                        if waiting.contains(&r.id) || live.contains(&r.id) {
                            continue;
                        }
                        earlier.push(RunItem {
                            group: Group::Earlier,
                            agent: r.workflow.clone(),
                            when: ago(r.started_ns, now),
                            word: r.status.clone(),
                            glyph: flow_glyph(&r.status),
                            at_ns: r.started_ns,
                            run: RunRef::Flow(Box::new(r)),
                        });
                    }
                }
                Err(e) => error = error.or(Some(e)),
            },
        }
        earlier.sort_by_key(|i| std::cmp::Reverse(i.at_ns));
        earlier.truncate(EARLIER);
        items.extend(fold_quiet(earlier));
        (items, error)
    }

    /// Runs, tokens and cost since midnight UTC, loops and flows.
    fn runs_today(&self, items: &[RunItem]) -> (usize, i64, f64) {
        let now = crate::loops::now();
        let midnight = now
            .replace_time(time::Time::MIDNIGHT)
            .unix_timestamp_nanos() as i64;
        let mut out = (0, 0, 0.0);
        for i in items
            .iter()
            .filter(|i| i.at_ns >= midnight && i.group != Group::Next)
        {
            let (t, c) = match &i.run {
                RunRef::Loop(r) => (r.tokens.unwrap_or(0), r.cost_usd.unwrap_or(0.0)),
                RunRef::Flow(r) => (r.tokens.unwrap_or(0), r.cost_usd.unwrap_or(0.0)),
                RunRef::Inbox(InboxItem::Loop(r)) => {
                    (r.tokens.unwrap_or(0), r.cost_usd.unwrap_or(0.0))
                }
                _ => (0, 0.0),
            };
            out.0 += 1;
            out.1 += t;
            out.2 += c;
        }
        out
    }

    pub fn open_runs_view(&mut self) {
        let (items, error) = self.runs_view_items();
        let today = self.runs_today(&items);
        // start on what needs the user, else on the first run
        let selected = items
            .iter()
            .position(|i| i.group == Group::NeedsYou)
            .unwrap_or(0);
        let mut st = RunsViewState {
            items,
            selected,
            tab: RunTab::Report,
            scroll: 0,
            error,
            steps: Vec::new(),
            today,
            detail: None,
            save_name: None,
            confirm_discard: false,
            max_scroll: std::cell::Cell::new(usize::MAX),
            last_refresh: Instant::now(),
        };
        self.load_run_steps(&mut st);
        self.mode = Mode::RunsView(Box::new(st));
    }

    /// The runs view with the run whose key (`RunRef::key`) is `key`
    /// selected, on `tab`.
    pub fn open_runs_view_on(&mut self, key: &str, tab: RunTab) {
        self.open_runs_view();
        if let Mode::RunsView(mut st) = std::mem::replace(&mut self.mode, Mode::Control) {
            if let Some(i) = st.items.iter().position(|i| i.run.key() == key) {
                st.selected = i;
            }
            st.tab = tab;
            self.load_run_steps(&mut st);
            self.mode = Mode::RunsView(st);
        }
    }

    /// The runs view on the newest run of loop `loop_id`.
    pub fn open_runs_view_on_loop(&mut self, loop_id: &str) {
        self.open_runs_view();
        if let Mode::RunsView(mut st) = std::mem::replace(&mut self.mode, Mode::Control) {
            // its newest run, else its next one
            let of = |i: &RunItem| i.run.loop_id() == Some(loop_id);
            let at = st
                .items
                .iter()
                .position(|i| of(i) && !matches!(i.run, RunRef::Next(_)))
                .or_else(|| st.items.iter().position(of));
            if let Some(i) = at {
                st.selected = i;
            }
            self.load_run_steps(&mut st);
            self.mode = Mode::RunsView(st);
        }
    }

    fn reload_runs_view(&mut self, st: &mut RunsViewState) {
        let keep = st.selected_item().map(|i| i.run.key());
        let (items, error) = self.runs_view_items();
        st.today = self.runs_today(&items);
        st.items = items;
        st.error = error;
        st.selected = keep
            .and_then(|k| st.items.iter().position(|i| i.run.key() == k))
            .unwrap_or(st.selected)
            .min(st.items.len().saturating_sub(1));
        st.last_refresh = Instant::now();
        self.load_run_steps(st);
    }

    /// Ticks the open view: live runs move, finished ones land.
    pub fn refresh_runs_view(&mut self, now: Instant) {
        let due = matches!(&self.mode, Mode::RunsView(st)
            if now.saturating_duration_since(st.last_refresh) >= REFRESH && st.save_name.is_none());
        if !due {
            return;
        }
        if let Mode::RunsView(mut st) = std::mem::replace(&mut self.mode, Mode::Control) {
            self.reload_runs_view(&mut st);
            self.mode = Mode::RunsView(st);
        }
    }

    /// The selected run's tab from the loop or workflow detail builder.
    fn build_run_detail(&self, st: &mut RunsViewState) {
        st.detail = None;
        let Some(item) = st.selected_item() else {
            return;
        };
        if st.tab == RunTab::Change {
            return;
        }
        if item.run.is_flow() {
            use super::workflows_view::{ViewTab, WorkflowsViewState};
            let row = match &item.run {
                RunRef::FlowLive(id) => RunRow::Live(id.clone()),
                RunRef::Flow(r) => RunRow::Stored(r.id.clone()),
                RunRef::Inbox(InboxItem::Run { run_id, .. }) => RunRow::Stored(run_id.clone()),
                RunRef::Inbox(InboxItem::Plan { agent: true, .. }) => return,
                RunRef::Inbox(InboxItem::Plan { id, .. }) => RunRow::Planned(id.clone()),
                _ => return,
            };
            let facts = self.view_facts();
            let mut v = WorkflowsViewState::new(
                self.trace_db_path.as_deref(),
                self.runtime_dir.as_deref(),
                &facts,
            );
            v.select(&row, &facts);
            if v.selected_row() != Some(&row) {
                return;
            }
            let tab = match st.tab {
                RunTab::Report => ViewTab::Report,
                RunTab::Sessions => ViewTab::Steps,
                RunTab::Result => ViewTab::Result,
                _ => ViewTab::Document,
            };
            v.set_tab(tab, &facts);
            st.detail = Some(v.detail_lines.clone());
        } else {
            use super::loops_view::{LoopsTab, LoopsViewState};
            if st.tab == RunTab::Result {
                return;
            }
            let Some(loop_id) = item.run.loop_id() else {
                return;
            };
            if self.loop_registry.find(loop_id).is_none() {
                return;
            }
            let run_id = item.run.key().trim_start_matches("loop:").to_string();
            let runtime = self.loops_runtime_dir();
            let mut v = LoopsViewState::new(
                self.trace_db_path.as_deref(),
                Some(runtime.as_path()),
                &self.loop_registry,
                Some(loop_id),
                &self.loops.worktrees_dir,
            );
            v.cards = self.loop_cards.clone();
            v.live = self
                .live_loop_runs
                .iter()
                .filter(|r| r.exited_at.is_none())
                .map(|r| (r.loop_id.clone(), r.session_id))
                .collect();
            if let Some(i) = v.runs.iter().position(|r| r.id == run_id) {
                v.selected_run = i;
            }
            v.tab = match st.tab {
                RunTab::Sessions => LoopsTab::History,
                RunTab::Setup => LoopsTab::Setup,
                _ => LoopsTab::Report,
            };
            v.rebuild_detail();
            st.detail = Some(v.detail_lines.clone());
        }
    }

    /// The selected flow run's steps, from the store, and the selected
    /// tab's detail.
    fn load_run_steps(&self, st: &mut RunsViewState) {
        self.build_run_detail(st);
        st.steps.clear();
        let id = match st.selected_item().map(|i| &i.run) {
            Some(RunRef::Flow(r)) => r.id.clone(),
            Some(RunRef::FlowLive(id)) => id.clone(),
            Some(RunRef::Inbox(InboxItem::Run { run_id, .. })) => run_id.clone(),
            _ => return,
        };
        if let Some(p) = self.trace_db_path.as_deref()
            && let Ok(c) = crate::tracing::store::open_ro(p)
        {
            st.steps = wstore::steps_of(&c, &id).unwrap_or_default();
        }
    }

    pub fn handle_runs_view_key(&mut self, key: &KeyEvent) {
        let Mode::RunsView(mut st) = std::mem::replace(&mut self.mode, Mode::Control) else {
            return;
        };
        let n = st.items.len();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let before = (st.selected, st.tab);
        let mut stay = true;
        if st.confirm_discard {
            st.confirm_discard = false;
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) {
                self.runs_view_reject(&mut st);
            }
            self.mode = Mode::RunsView(st);
            return;
        }
        if let Some(name) = st.save_name.as_mut() {
            match key.code {
                KeyCode::Esc => st.save_name = None,
                KeyCode::Backspace => {
                    name.pop();
                }
                KeyCode::Enter => {
                    let name = st.save_name.take().unwrap_or_default();
                    self.runs_view_save(&st, name.trim());
                    self.reload_runs_view(&mut st);
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => name.push(c),
                _ => {}
            }
            self.mode = Mode::RunsView(st);
            return;
        }
        match key.code {
            KeyCode::Char(c @ '1'..='5') => {
                st.tab = RunTab::ALL[c as usize - '1' as usize];
                st.scroll = 0;
            }
            KeyCode::Tab => {
                let at = RunTab::ALL.iter().position(|t| *t == st.tab).unwrap_or(0);
                st.tab = RunTab::ALL[(at + 1) % RunTab::ALL.len()];
                st.scroll = 0;
            }
            KeyCode::Char('p') if !ctrl => self.runs_view_pause(&mut st),
            // a plan's document in $EDITOR
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(RunRef::Inbox(InboxItem::Plan { id, .. })) =
                    st.selected_item().map(|i| &i.run)
                    && let Some(p) = self.planned_workflows.iter().find(|p| &p.id == id).cloned()
                {
                    let path = self
                        .workflows_runtime_dir()
                        .join("workflows")
                        .join("plans")
                        .join(format!("{}.toml", p.id));
                    let _ = std::fs::write(&path, &p.document);
                    self.editor_request = Some(super::EditorRequest {
                        path,
                        asset_id: format!("plan:{}", p.id),
                        command: crate::assets::editor_command(self.editor.as_deref()),
                    });
                }
            }
            KeyCode::Char('s') if !ctrl => {
                st.save_name = st
                    .selected_item()
                    .filter(|i| i.run.is_flow())
                    .map(|i| i.agent.clone());
                if st.save_name.is_none() {
                    self.notice = Some(Notice::info("s saves a flow's document into the library"));
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                st.selected = (st.selected + 1).min(n.saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => st.selected = st.selected.saturating_sub(1),
            KeyCode::Char('E') | KeyCode::Char('W') => stay = false,
            KeyCode::Enter => stay = self.runs_view_open(&mut st),
            KeyCode::Char('d') if !ctrl => {
                if matches!(
                    st.selected_item().map(|i| &i.run),
                    Some(RunRef::Inbox(InboxItem::Plan { .. }))
                ) {
                    st.confirm_discard = true;
                } else {
                    self.runs_view_reject(&mut st);
                }
            }
            KeyCode::Char('r') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.runs_view_again(&mut st)
            }
            KeyCode::Char('x') if !ctrl => self.runs_view_stop(&mut st),
            KeyCode::Char('e') if !ctrl => stay = !self.runs_view_edit(&st),
            KeyCode::Char('T') if !ctrl => self.runs_view_traces(&st),
            _ => match verb(&crate::keymap::list_alias(key)) {
                Some(Verb::Back) => stay = false,
                Some(Verb::Top) => st.selected = 0,
                Some(Verb::Bottom) => st.selected = n.saturating_sub(1),
                Some(Verb::PageDown) => st.scroll = (st.scroll + 10).min(st.max_scroll.get()),
                Some(Verb::PageUp) => st.scroll = st.scroll.saturating_sub(10),
                Some(Verb::Reload) => self.reload_runs_view(&mut st),
                _ => {}
            },
        }
        if (st.selected, st.tab) != before {
            if st.selected != before.0 {
                st.scroll = 0;
            }
            self.load_run_steps(&mut st);
        }
        // an action that opened another screen keeps it
        if stay && matches!(self.mode, Mode::Control) {
            self.mode = Mode::RunsView(st);
        }
    }

    /// `Enter`: apply a change, run a plan, attach to a running session.
    /// `false` when the view closes.
    fn runs_view_open(&mut self, st: &mut RunsViewState) -> bool {
        let Some(item) = st.selected_item().cloned() else {
            return true;
        };
        match item.run {
            RunRef::Inbox(InboxItem::Loop(r)) if r.branch.is_some() => {
                match self.decide_loop_run(&r.id, true) {
                    Ok(()) => self.notice = Some(Notice::info(format!("{}: applied", r.pattern))),
                    Err(e) => self.notice = Some(Notice::error(format!("apply: {e}"))),
                }
                self.reload_runs_view(st);
                true
            }
            RunRef::Inbox(InboxItem::Plan {
                id, agent: true, ..
            }) => {
                self.open_agent_draft(&id);
                !matches!(self.mode, Mode::AgentEditor(_))
            }
            RunRef::Inbox(InboxItem::Plan { id, .. }) => {
                match self.run_planned_workflow(&id) {
                    Ok(rid) => {
                        self.notice = Some(Notice::info(format!("run {} started", &rid[..8])))
                    }
                    Err(e) => self.notice = Some(Notice::warn(e)),
                }
                self.reload_runs_view(st);
                true
            }
            RunRef::LoopLive { session_id, .. } => self.runs_view_attach(session_id),
            RunRef::FlowLive(id) => {
                let sid = self
                    .live_workflow_runs
                    .iter()
                    .find(|r| r.run_id == id)
                    .and_then(|r| r.sessions.iter().find(|s| s.exited_at.is_none()))
                    .map(|s| s.session_id);
                match sid {
                    Some(sid) => self.runs_view_attach(sid),
                    None => {
                        self.notice = Some(Notice::info("no session of the run is running now"));
                        true
                    }
                }
            }
            RunRef::Inbox(InboxItem::Loop(_)) => {
                self.notice = Some(Notice::info("d dismisses it; e edits the agent; T traces"));
                true
            }
            RunRef::Loop(_) => {
                self.notice = Some(Notice::info("r runs it again; T its traces; 2 its history"));
                true
            }
            RunRef::Flow(_) | RunRef::Inbox(InboxItem::Run { .. }) => {
                self.notice = Some(Notice::info("r resumes the run; s saves its document"));
                true
            }
            RunRef::Next(_) => {
                self.notice = Some(Notice::info("r runs it now; p pauses it; 5 its setup"));
                true
            }
        }
    }

    /// Attaches to session `session_id`; `false` (the view closes) when it
    /// is there.
    fn runs_view_attach(&mut self, session_id: usize) -> bool {
        match self.sessions.iter().position(|s| s.id == session_id) {
            Some(i) => {
                self.selected = i;
                self.selection = None;
                self.mode = Mode::Attached;
                if let Some(s) = self.sessions.get_mut(i) {
                    s.tracker.on_attach();
                }
                false
            }
            None => {
                self.notice = Some(Notice::info("the run's session is gone"));
                true
            }
        }
    }

    /// `p`: pauses or resumes the loop of a loop run.
    fn runs_view_pause(&mut self, st: &mut RunsViewState) {
        let at = st
            .selected_item()
            .and_then(|i| i.run.loop_id())
            .and_then(|id| self.loop_registry.loops.iter().position(|l| l.id == id));
        match at {
            Some(i) => {
                self.selected_loop = i;
                self.toggle_selected_loop_pause();
                self.load_run_steps(st);
            }
            None => self.notice = Some(Notice::info("p pauses the scheduled agent of a loop run")),
        }
    }

    /// `s`, then a name: the flow's document saved into the library.
    fn runs_view_save(&mut self, st: &RunsViewState, name: &str) {
        if name.is_empty() {
            return;
        }
        let doc = match st.selected_item().map(|i| &i.run) {
            Some(RunRef::Inbox(InboxItem::Plan {
                id, agent: true, ..
            })) => {
                let id = id.clone();
                self.notice = Some(match self.save_agent_draft(&id, name) {
                    Ok(p) => Notice::info(format!("saved {}", p.display())),
                    Err(e) => Notice::warn(e),
                });
                return;
            }
            Some(RunRef::Inbox(InboxItem::Plan { id, .. })) => {
                let id = id.clone();
                self.notice = Some(match self.save_planned_workflow(&id, name) {
                    Ok(p) => {
                        self.reload_workflow_list();
                        Notice::info(format!("saved {}", p.display()))
                    }
                    Err(e) => Notice::warn(e),
                });
                return;
            }
            Some(RunRef::FlowLive(id)) => self
                .live_workflow_runs
                .iter()
                .find(|r| &r.run_id == id)
                .map(|r| r.document.clone()),
            Some(RunRef::Flow(r)) => Some(r.document.clone()),
            Some(RunRef::Inbox(InboxItem::Run { run_id, .. })) => self
                .trace_db_path
                .as_deref()
                .and_then(|p| crate::tracing::store::open_ro(p).ok())
                .and_then(|c| wstore::get_run(&c, run_id).ok().flatten())
                .map(|r| r.document),
            _ => None,
        };
        let Some(doc) = doc else { return };
        self.notice = Some(
            match super::workflows::save_document(&self.library_root(), name, &doc) {
                Ok(path) => {
                    self.reload_workflow_list();
                    Notice::info(format!("saved {}", path.display()))
                }
                Err(e) => Notice::warn(e),
            },
        );
    }

    /// `d`: reject a change, discard a plan, dismiss a failed run.
    fn runs_view_reject(&mut self, st: &mut RunsViewState) {
        let Some(RunRef::Inbox(item)) = st.selected_item().map(|i| i.run.clone()) else {
            self.notice = Some(Notice::info("d rejects what needs you: a change, a plan"));
            return;
        };
        match item {
            InboxItem::Loop(r) => {
                if let Err(e) = self.decide_loop_run(&r.id, false) {
                    self.notice = Some(Notice::error(format!("reject: {e}")));
                } else {
                    self.notice = Some(Notice::info(format!("{}: rejected", r.pattern)));
                }
            }
            InboxItem::Plan { id, .. } => {
                self.planned_workflows.retain(|p| p.id != id);
                self.notice = Some(Notice::info("planned document discarded"));
            }
            item @ InboxItem::Run { .. } => {
                self.inbox_dismissed.insert(item.key());
                self.notice = Some(Notice::info("dismissed; the run stays under Earlier"));
            }
        }
        self.reload_runs_view(st);
    }

    /// `r`: a scheduled agent runs again now; a stored flow run resumes.
    fn runs_view_again(&mut self, st: &mut RunsViewState) {
        let stored = match st.selected_item().map(|i| &i.run) {
            Some(RunRef::Flow(r)) => Some((**r).clone()),
            Some(RunRef::Inbox(InboxItem::Run { run_id, .. })) => self
                .trace_db_path
                .as_deref()
                .and_then(|p| crate::tracing::store::open_ro(p).ok())
                .and_then(|c| wstore::get_run(&c, run_id).ok().flatten()),
            _ => None,
        };
        if let Some(r) = stored {
            self.notice = Some(match self.resume_workflow_run(&r) {
                Ok(rid) => Notice::info(format!("resumed as {}", &rid[..8])),
                Err(e) => Notice::warn(e),
            });
            self.reload_runs_view(st);
            return;
        }
        let loop_id = st
            .selected_item()
            .and_then(|i| i.run.loop_id())
            .map(str::to_string);
        match loop_id.and_then(|id| self.loop_registry.loops.iter().position(|l| l.id == id)) {
            Some(i) => {
                self.selected_loop = i;
                self.run_selected_loop_now();
            }
            None => {
                self.notice = Some(Notice::info(
                    "r runs a scheduled agent again or resumes a flow run",
                ))
            }
        }
    }

    /// `x`: stops a running run.
    fn runs_view_stop(&mut self, st: &mut RunsViewState) {
        match st.selected_item().map(|i| i.run.clone()) {
            Some(RunRef::FlowLive(id)) => {
                if self.cancel_workflow_run(&id) {
                    self.notice = Some(Notice::info("stopping the run"));
                }
            }
            Some(RunRef::LoopLive { session_id, .. }) => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == session_id) {
                    s.kill();
                    self.notice = Some(Notice::info("stopping the run"));
                }
            }
            _ => self.notice = Some(Notice::info("x stops a running run")),
        }
        self.reload_runs_view(st);
    }

    /// `e`: the run's agent in the agent editor; `true` when it opened.
    fn runs_view_edit(&mut self, st: &RunsViewState) -> bool {
        if let Some(RunRef::Inbox(InboxItem::Plan { id, agent, .. })) =
            st.selected_item().map(|i| &i.run)
        {
            let (id, agent) = (id.clone(), *agent);
            if agent {
                self.open_agent_draft(&id);
            } else {
                self.open_flow_builder_plan(&id);
            }
            return matches!(self.mode, Mode::AgentEditor(_));
        }
        let (loop_id, flow) = match st.selected_item().map(|i| &i.run) {
            Some(RunRef::Loop(r)) => (Some(r.loop_id.clone()), None),
            Some(RunRef::Inbox(InboxItem::Loop(r))) => (Some(r.loop_id.clone()), None),
            Some(RunRef::LoopLive { loop_id, .. } | RunRef::Next(loop_id)) => {
                (Some(loop_id.clone()), None)
            }
            Some(RunRef::Flow(r)) => (None, Some(r.workflow.clone())),
            Some(i) => (
                None,
                st.selected_item().map(|x| x.agent.clone()).filter(|_| {
                    matches!(
                        i,
                        RunRef::FlowLive(_) | RunRef::Inbox(InboxItem::Run { .. })
                    )
                }),
            ),
            None => (None, None),
        };
        if let Some(id) = loop_id.filter(|id| self.loop_registry.find(id).is_some()) {
            self.open_scheduled_editor(Some(id), None);
            return true;
        }
        if let Some(name) = flow
            && let Some(e) = self.workflow_entries().into_iter().find(|e| e.name == name)
        {
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
            return true;
        }
        self.notice = Some(Notice::info("the agent of this run is no longer saved"));
        false
    }

    /// Writes a planner's agent draft into the library as agent `name`.
    fn save_agent_draft(
        &mut self,
        plan_id: &str,
        name: &str,
    ) -> Result<std::path::PathBuf, String> {
        let text = self
            .planned_workflows
            .iter()
            .find(|p| p.id == plan_id)
            .and_then(|p| p.agent.clone())
            .ok_or("no such draft")?;
        let mut t: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        t.insert("name".into(), toml::Value::String(name.to_string()));
        let text = crate::agents::schedule::ordered(t)?;
        let spec = crate::agents::AgentSpec::parse(&text).map_err(|p| p.join("; "))?;
        let catalog = crate::agents::Catalog::load(&self.library_root(), None);
        if catalog.entry(name).is_some() {
            return Err(format!("an agent called {name} exists: pick another name"));
        }
        let path = crate::agents::schedule::file_of(&self.library_root(), name);
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, text))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.planned_workflows.retain(|p| p.id != plan_id);
        self.after_agent_saved(name, spec.schedule.is_some());
        Ok(path)
    }

    /// `T`: the traces of a loop run's session.
    fn runs_view_traces(&mut self, st: &RunsViewState) {
        let launch = match st.selected_item().map(|i| &i.run) {
            Some(RunRef::Loop(r)) => r.launch_id.clone(),
            Some(RunRef::Inbox(InboxItem::Loop(r))) => r.launch_id.clone(),
            _ => None,
        };
        match launch {
            Some(l) => self.open_trace_browser_for_launch(&l),
            None => self.notice = Some(Notice::info("T opens the traces of a loop run")),
        }
    }
}

/// The text of a tab for the selected run: `(heading, lines)`.
pub fn tab_lines(app: &App, st: &RunsViewState) -> Vec<String> {
    let Some(item) = st.selected_item() else {
        return vec!["no runs yet: r on a scheduled agent, Enter on a flow".into()];
    };
    match (&item.run, st.tab) {
        (RunRef::Next(id), _) => match app.loop_registry.find(id) {
            Some(l) => vec![
                format!(
                    "{} has not run yet in {}.",
                    l.pattern,
                    l.workspace.display()
                ),
                String::new(),
                "r runs it now; p pauses it; e edits it; 5 shows its setup.".into(),
            ],
            None => vec!["it is no longer scheduled: Ctrl+R reloads".into()],
        },
        (RunRef::Inbox(InboxItem::Loop(r)) | RunRef::Loop(r), tab) => loop_tab(r, tab),
        (RunRef::Flow(r), tab) => flow_tab(r, &st.steps, tab),
        (
            RunRef::Inbox(InboxItem::Plan {
                id,
                agent: true,
                task,
                problems,
                ..
            }),
            _,
        ) => {
            let mut v = vec![
                "A planner drafted an agent for this task:".into(),
                String::new(),
                format!("  {task}"),
                String::new(),
            ];
            v.extend(problems.iter().map(|p| format!("! {p}")));
            if let Some(text) = app
                .planned_workflows
                .iter()
                .find(|p| &p.id == id)
                .and_then(|p| p.agent.as_ref())
            {
                v.extend(text.lines().map(|l| format!("  {l}")));
            }
            v.push(String::new());
            v.push("Enter opens it in the agent editor; s saves it as is; d discards it.".into());
            v
        }
        (RunRef::Inbox(InboxItem::Plan { task, problems, .. }), _) => {
            let mut v = vec![
                "A planner wrote a flow for this task:".into(),
                String::new(),
                format!("  {task}"),
                String::new(),
            ];
            if problems.is_empty() {
                v.push("It checks clean. Enter reviews it; d discards it.".into());
            } else {
                v.push("It has problems to fix first:".into());
                v.extend(problems.iter().map(|p| format!("  · {p}")));
            }
            v
        }
        (
            RunRef::Inbox(InboxItem::Run {
                status,
                error,
                notes,
                verdict,
                ..
            }),
            _,
        ) => {
            let mut v = vec![format!("The run ended {status}.")];
            if let Some(e) = error {
                v.push(String::new());
                v.push(e.clone());
            }
            if let Some(vd) = verdict {
                v.push(String::new());
                v.push(format!("Its verdict: {vd}"));
            }
            v.extend(notes.iter().map(|n| format!("  · {n}")));
            v.push(String::new());
            v.push("Enter opens the run; d dismisses it.".into());
            v
        }
        (RunRef::LoopLive { run_id, .. }, _) => {
            let live = app.live_loop_runs.iter().find(|r| &r.run_id == run_id);
            match live {
                Some(r) => vec![
                    format!("{} is running in {}", r.pattern, r.workspace.display()),
                    format!("on {} · it {}", r.harness, r.effective_level.label()),
                    String::new(),
                    "Enter attaches to its session; x stops it.".into(),
                ],
                None => vec!["finished: Ctrl+R reloads".into()],
            }
        }
        (RunRef::FlowLive(id), tab) => {
            let Some(r) = app.live_workflow_runs.iter().find(|r| &r.run_id == id) else {
                return vec!["finished: Ctrl+R reloads".into()];
            };
            match tab {
                RunTab::Sessions => r
                    .sessions
                    .iter()
                    .map(|s| {
                        format!(
                            "{} {}  {}",
                            if s.exited_at.is_some() { "✓" } else { "▶" },
                            s.key.label(),
                            s.harness.as_str()
                        )
                    })
                    .collect(),
                _ => vec![
                    format!("{} is running in {}", r.name, r.workspace.display()),
                    format!("{} session(s) so far", r.sessions.len()),
                    String::new(),
                    "Enter attaches to a running session; x stops it.".into(),
                ],
            }
        }
    }
}

fn loop_tab(r: &LoopRun, tab: RunTab) -> Vec<String> {
    let files: Vec<String> = r
        .detail
        .get("files")
        .and_then(|f| f.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|f| f.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    match tab {
        RunTab::Report => {
            let mut v = Vec::new();
            let pending = r.outcome.needs_human() && r.decision.is_none();
            v.push(if pending && r.branch.is_some() {
                "! A change is ready for you".to_string()
            } else if pending {
                "! It needs your decision".into()
            } else {
                r.outcome.word().to_string()
            });
            v.push(String::new());
            if let Some(s) = r.detail_str("summary") {
                v.push(s.to_string());
                v.push(String::new());
            }
            if let Some(s) = r.detail_str("reason") {
                v.push(format!("why         {s}"));
            }
            v.push(format!(
                "files       {}",
                if files.is_empty() {
                    "none".into()
                } else {
                    files.join(" · ")
                }
            ));
            v.push(format!(
                "checked by  {}",
                super::loops_view::verifier_text(r)
            ));
            v.push(format!(
                "cost        {} tokens · ${:.2}{}",
                r.tokens.unwrap_or(0),
                r.cost_usd.unwrap_or(0.0),
                r.duration_s()
                    .map(|s| format!(" · {}", super::loops::short_duration(s.max(0) as u64)))
                    .unwrap_or_default()
            ));
            if let Some(d) = &r.decision {
                v.push(format!("decision    {d}"));
            }
            if pending {
                v.push(String::new());
                v.push(if r.branch.is_some() {
                    "Enter applies the change · d rejects it · e edits the agent".into()
                } else {
                    "Enter opens its record · d dismisses it · e edits the agent".into()
                });
            }
            v
        }
        RunTab::Sessions => vec![
            format!("{} on {}", r.pattern, r.harness),
            format!(
                "launch      {}",
                r.launch_id.clone().unwrap_or_else(|| "none".into())
            ),
            format!("workspace   {}", r.workspace),
            String::new(),
            "T opens its traces.".into(),
        ],
        RunTab::Change => {
            let mut v = Vec::new();
            match (&r.branch, &r.worktree) {
                (Some(b), wt) => {
                    v.push(format!("branch      {b}"));
                    if let Some(w) = wt {
                        v.push(format!("worktree    {w}"));
                    }
                }
                _ => v.push("No change: this run reported only.".into()),
            }
            if let Some(d) = r.detail_str("diff_stat") {
                v.push(String::new());
                v.extend(d.lines().map(str::to_string));
            } else if !files.is_empty() {
                v.push(String::new());
                v.extend(files.iter().map(|f| format!("  {f}")));
            }
            v
        }
        RunTab::Setup => vec![format!(
            "{} is no longer a scheduled agent; its setup went with it.",
            r.pattern
        )],
        RunTab::Result => match r.detail_str("final_message") {
            Some(m) => m.lines().map(str::to_string).collect(),
            None => vec!["The run left no final message.".into()],
        },
    }
}

fn flow_tab(r: &WorkflowRun, steps: &[WorkflowStep], tab: RunTab) -> Vec<String> {
    match tab {
        RunTab::Setup => r.document.lines().map(str::to_string).collect(),
        RunTab::Report => {
            let mut v = vec![
                format!("{} ended {}", r.workflow, r.status),
                String::new(),
                format!("workspace   {}", r.workspace),
                format!("on          {} ({})", r.harness, r.profile),
                format!(
                    "cost        {} tokens · ${:.2} · {} session(s)",
                    r.tokens.unwrap_or(0),
                    r.cost_usd.unwrap_or(0.0),
                    r.sessions
                ),
            ];
            if let Some(e) = &r.error {
                v.push(String::new());
                v.push(e.clone());
            }
            v
        }
        RunTab::Sessions if steps.is_empty() => vec!["No sessions recorded.".into()],
        RunTab::Sessions => steps
            .iter()
            .map(|s| {
                format!(
                    "{} {}  {} · {} tokens",
                    if s.ended_ns.is_some() { "✓" } else { "·" },
                    s.session,
                    s.harness,
                    s.tokens.unwrap_or(0)
                )
            })
            .collect(),
        RunTab::Change => {
            let mut v: Vec<String> = Vec::new();
            for s in steps {
                let files: Vec<&str> = s
                    .changed_files
                    .as_array()
                    .map(|a| a.iter().filter_map(|f| f.as_str()).collect())
                    .unwrap_or_default();
                if !files.is_empty() {
                    v.push(format!(
                        "{}{}",
                        s.session,
                        s.worktree
                            .as_deref()
                            .map(|w| format!("  ({w})"))
                            .unwrap_or_default()
                    ));
                    v.extend(files.iter().map(|f| format!("  {f}")));
                }
            }
            if v.is_empty() {
                v.push("No step changed files.".into());
            }
            v
        }
        RunTab::Result => {
            if r.result.is_null() {
                vec!["The run gave no result.".into()]
            } else {
                serde_json::to_string_pretty(&r.result)
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_string)
                    .collect()
            }
        }
    }
}
