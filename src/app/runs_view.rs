//! The runs view (`E` / `W`, spec `docs/superpowers/specs/2026-09-26-agent-first-design.md`,
//! section 4): every run of every agent in one list, **Running**, **Needs
//! you** (the inbox's items) and **Earlier**, with the selected run's
//! Report, Sessions, Change and Result. Loop runs come from `loop_runs`,
//! flow runs from `workflow_runs` and the live runs from the App; the
//! Loops and Workflows views stay one `Enter` away for everything else
//! about a run.

use super::inbox::InboxItem;
use super::workflows_view::RunRow;
use super::{App, Mode, Notice};
use crate::keymap::{Verb, verb};
use crate::loops::store::{self as lstore, LoopRun};
use crate::workflows::store::{self as wstore, WorkflowRun, WorkflowStep};
use crossterm::event::{KeyCode, KeyEvent};

/// How many stored runs of each kind the Earlier group reads.
const EARLIER: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Running,
    NeedsYou,
    Earlier,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::Running => "Running",
            Group::NeedsYou => "Needs you",
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
    Sessions,
    Change,
    Result,
}

impl RunTab {
    pub const ALL: [RunTab; 4] = [
        RunTab::Report,
        RunTab::Sessions,
        RunTab::Change,
        RunTab::Result,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RunTab::Report => "Report",
            RunTab::Sessions => "Sessions",
            RunTab::Change => "Change",
            RunTab::Result => "Result",
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
}

impl RunsViewState {
    pub fn selected_item(&self) -> Option<&RunItem> {
        self.items.get(self.selected)
    }
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
        items.extend(earlier);
        (items, error)
    }

    /// Runs, tokens and cost since midnight UTC, loops and flows.
    fn runs_today(&self, items: &[RunItem]) -> (usize, i64, f64) {
        let now = crate::loops::now();
        let midnight = now
            .replace_time(time::Time::MIDNIGHT)
            .unix_timestamp_nanos() as i64;
        let mut out = (0, 0, 0.0);
        for i in items.iter().filter(|i| i.at_ns >= midnight) {
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
        };
        self.load_run_steps(&mut st);
        self.mode = Mode::RunsView(Box::new(st));
    }

    fn reload_runs_view(&mut self, st: &mut RunsViewState) {
        let (items, error) = self.runs_view_items();
        st.today = self.runs_today(&items);
        st.items = items;
        st.error = error;
        st.selected = st.selected.min(st.items.len().saturating_sub(1));
        self.load_run_steps(st);
    }

    /// The selected flow run's steps, from the store.
    fn load_run_steps(&self, st: &mut RunsViewState) {
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
        let before = st.selected;
        let mut stay = true;
        match key.code {
            KeyCode::Char(c @ '1'..='4') => {
                st.tab = RunTab::ALL[c as usize - '1' as usize];
                st.scroll = 0;
            }
            KeyCode::Tab => {
                let at = RunTab::ALL.iter().position(|t| *t == st.tab).unwrap_or(0);
                st.tab = RunTab::ALL[(at + 1) % 4];
                st.scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                st.selected = (st.selected + 1).min(n.saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => st.selected = st.selected.saturating_sub(1),
            KeyCode::Char('E') | KeyCode::Char('W') => stay = false,
            KeyCode::Enter => stay = self.runs_view_open(&mut st),
            KeyCode::Char('d') => self.runs_view_reject(&mut st),
            KeyCode::Char('r') => self.runs_view_again(&st),
            KeyCode::Char('x') => self.runs_view_stop(&mut st),
            KeyCode::Char('e') => stay = !self.runs_view_edit(&st),
            KeyCode::Char('T') => self.runs_view_traces(&st),
            _ => match verb(&crate::keymap::list_alias(key)) {
                Some(Verb::Back) => stay = false,
                Some(Verb::Top) => st.selected = 0,
                Some(Verb::Bottom) => st.selected = n.saturating_sub(1),
                Some(Verb::PageDown) => st.scroll += 10,
                Some(Verb::PageUp) => st.scroll = st.scroll.saturating_sub(10),
                Some(Verb::Reload) => self.reload_runs_view(&mut st),
                _ => {}
            },
        }
        if st.selected != before {
            st.scroll = 0;
            self.load_run_steps(&mut st);
        }
        // an action that opened another screen keeps it
        if stay && matches!(self.mode, Mode::Control) {
            self.mode = Mode::RunsView(st);
        }
    }

    /// `Enter`: apply a change, review a plan, attach to a running
    /// session, or open the run's full record. `false` when the view
    /// closes.
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
            RunRef::Inbox(InboxItem::Loop(r)) => {
                self.open_loop_run_record(&r.loop_id);
                false
            }
            RunRef::Inbox(InboxItem::Plan { id, .. }) => {
                self.open_workflows_view_on(RunRow::Planned(id));
                false
            }
            RunRef::Inbox(InboxItem::Run { run_id, .. }) => {
                self.open_workflows_view_on(RunRow::Stored(run_id));
                false
            }
            RunRef::LoopLive { session_id, .. } => {
                match self.sessions.iter().position(|s| s.id == session_id) {
                    Some(i) => {
                        self.selected = i;
                        self.selection = None;
                        self.mode = Mode::Attached;
                        if let Some(s) = self.sessions.get_mut(i) {
                            s.tracker.on_attach();
                        }
                    }
                    None => self.notice = Some(Notice::info("the run's session is gone")),
                }
                false
            }
            RunRef::FlowLive(id) => {
                self.open_workflows_view_on(RunRow::Live(id));
                false
            }
            RunRef::Loop(r) => {
                self.open_loop_run_record(&r.loop_id);
                false
            }
            RunRef::Flow(r) => {
                self.open_workflows_view_on(RunRow::Stored(r.id.clone()));
                false
            }
        }
    }

    /// The Loops view on `loop_id`'s runs.
    fn open_loop_run_record(&mut self, loop_id: &str) {
        if let Some(i) = self
            .loop_registry
            .loops
            .iter()
            .position(|l| l.id == loop_id)
        {
            self.selected_loop = i;
        }
        self.open_loops_view();
        if let Mode::LoopsView(view) = &mut self.mode {
            view.tab = super::loops_view::LoopsTab::History;
            view.rebuild_detail();
        }
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

    /// `r`: a scheduled agent runs again now.
    fn runs_view_again(&mut self, st: &RunsViewState) {
        let loop_id = match st.selected_item().map(|i| &i.run) {
            Some(RunRef::Loop(r)) => Some(r.loop_id.clone()),
            Some(RunRef::Inbox(InboxItem::Loop(r))) => Some(r.loop_id.clone()),
            Some(RunRef::LoopLive { loop_id, .. }) => Some(loop_id.clone()),
            _ => None,
        };
        match loop_id.and_then(|id| self.loop_registry.loops.iter().position(|l| l.id == id)) {
            Some(i) => {
                self.selected_loop = i;
                self.run_selected_loop_now();
            }
            None => {
                self.notice = Some(Notice::info(
                    "r runs a scheduled agent again; a flow runs from its row (Enter)",
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
        let (loop_id, flow) = match st.selected_item().map(|i| &i.run) {
            Some(RunRef::Loop(r)) => (Some(r.loop_id.clone()), None),
            Some(RunRef::Inbox(InboxItem::Loop(r))) => (Some(r.loop_id.clone()), None),
            Some(RunRef::LoopLive { loop_id, .. }) => (Some(loop_id.clone()), None),
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
        (RunRef::Inbox(InboxItem::Loop(r)) | RunRef::Loop(r), tab) => loop_tab(r, tab),
        (RunRef::Flow(r), tab) => flow_tab(r, &st.steps, tab),
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
                    "Enter opens it in the Workflows view; x stops it.".into(),
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
        RunTab::Result => match r.detail_str("final_message") {
            Some(m) => m.lines().map(str::to_string).collect(),
            None => vec!["The run left no final message.".into()],
        },
    }
}

fn flow_tab(r: &WorkflowRun, steps: &[WorkflowStep], tab: RunTab) -> Vec<String> {
    match tab {
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
