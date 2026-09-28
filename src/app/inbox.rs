//! The inbox (`I`): everything waiting on a human, across loops and
//! workflows, in one list. A loop run that proposed a fix or asked for a
//! decision is applied or rejected here; a planner's document is reviewed
//! in the Workflows view or discarded; a workflow run that did not finish
//! cleanly is opened or dismissed.

use super::runs_view::RunTab;
use super::workflows_view::RunRow;
use super::{App, Mode, Notice};
use crate::loops::store::{self as lstore, LoopRun};
use crossterm::event::{KeyCode, KeyEvent};

/// One thing waiting on a human.
#[derive(Debug, Clone)]
pub enum InboxItem {
    /// A loop run waiting on apply or reject.
    Loop(Box<LoopRun>),
    /// A planner's document waiting to be run, edited, saved or discarded.
    Plan {
        id: String,
        name: String,
        task: String,
        problems: Vec<String>,
        /// The planner drafted one agent rather than a flow.
        agent: bool,
    },
    /// A workflow run since startup that did not finish cleanly, or that
    /// finished with a verdict its document does not accept.
    Run {
        run_id: String,
        name: String,
        status: String,
        error: Option<String>,
        notes: Vec<String>,
        verdict: Option<String>,
    },
}

impl InboxItem {
    /// The group the item is listed under.
    pub fn group(&self) -> &'static str {
        match self {
            InboxItem::Loop(_) => "Loops",
            InboxItem::Plan { .. } | InboxItem::Run { .. } => "Workflows",
        }
    }

    /// The word that says what the item wants.
    pub fn word(&self) -> &'static str {
        match self {
            InboxItem::Loop(r) => r.outcome.word(),
            InboxItem::Plan { agent: true, .. } => "agent draft",
            InboxItem::Plan { problems, .. } if problems.is_empty() => "plan ready",
            InboxItem::Plan { .. } => "plan has problems",
            InboxItem::Run {
                verdict: Some(_), ..
            } => "verdict to read",
            InboxItem::Run { .. } => "did not finish",
        }
    }

    /// The key hints for the item, most useful first.
    pub fn hints(&self) -> &'static str {
        match self {
            InboxItem::Loop(r) if r.branch.is_some() => {
                " [↑/↓] select  [a] applied  [d] rejected  [Enter] its loop  [T] traces  [Esc] close"
            }
            InboxItem::Loop(_) => {
                " [↑/↓] select  [a] done  [d] dismiss  [Enter] its loop  [T] traces  [Esc] close"
            }
            InboxItem::Plan { .. } => " [↑/↓] select  [Enter] review it  [d] discard  [Esc] close",
            InboxItem::Run { .. } => {
                " [↑/↓] select  [Enter] open the run  [d] dismiss  [Esc] close"
            }
        }
    }

    pub(crate) fn key(&self) -> String {
        match self {
            InboxItem::Loop(r) => format!("loop:{}", r.id),
            InboxItem::Plan { id, .. } => format!("plan:{id}"),
            InboxItem::Run { run_id, .. } => format!("run:{run_id}"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct InboxState {
    pub items: Vec<InboxItem>,
    pub selected: usize,
    /// Why the loop runs could not be read (tracing off, store error).
    pub error: Option<String>,
}

impl InboxState {
    pub fn selected_item(&self) -> Option<&InboxItem> {
        self.items.get(self.selected)
    }
}

impl App {
    /// Everything waiting on a human, loops first, and why the loop runs
    /// could not be read when they could not.
    pub fn inbox_items(&self) -> (Vec<InboxItem>, Option<String>) {
        let mut items: Vec<InboxItem> = Vec::new();
        let mut error = None;
        match self.trace_db_path.as_deref() {
            None => error = Some("tracing is off: loop runs are not recorded".to_string()),
            Some(p) => match crate::tracing::store::open_ro(p) {
                Ok(c) => match lstore::inbox(&c) {
                    Ok(runs) => {
                        items.extend(runs.into_iter().map(|r| InboxItem::Loop(Box::new(r))))
                    }
                    Err(e) => error = Some(e.to_string()),
                },
                Err(e) => error = Some(e),
            },
        }
        items.extend(
            self.planned_workflows
                .iter()
                .filter(|p| p.run_id.is_none())
                .map(|p| InboxItem::Plan {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    task: p.task.clone(),
                    problems: p.problems.clone(),
                    agent: p.agent.is_some(),
                }),
        );
        items.extend(
            self.recent_workflow_runs
                .iter()
                .filter(|r| r.needs_human())
                .map(|r| InboxItem::Run {
                    run_id: r.run_id.clone(),
                    name: r.name.clone(),
                    status: r.status.clone(),
                    error: r.error.clone(),
                    notes: r.notes.clone(),
                    verdict: r.awaiting.clone(),
                }),
        );
        items.retain(|i| !self.inbox_dismissed.contains(&i.key()));
        (items, error)
    }

    /// How many things wait on a human, for the status bar: the loop
    /// cards' counts, plans and runs that did not finish. Cheap enough to
    /// draw every frame.
    pub fn inbox_count(&self) -> usize {
        let loops: usize = self.loop_cards.values().map(|c| c.inbox).sum();
        let plans = self
            .planned_workflows
            .iter()
            .filter(|p| p.run_id.is_none())
            .filter(|p| !self.inbox_dismissed.contains(&format!("plan:{}", p.id)))
            .count();
        let runs = self
            .recent_workflow_runs
            .iter()
            .filter(|r| r.needs_human())
            .filter(|r| !self.inbox_dismissed.contains(&format!("run:{}", r.run_id)))
            .count();
        loops + plans + runs
    }

    pub fn open_inbox(&mut self) {
        let (items, error) = self.inbox_items();
        self.mode = Mode::Inbox(Box::new(InboxState {
            items,
            selected: 0,
            error,
        }));
    }

    /// Re-reads the items, keeping the selection's place.
    fn reload_inbox(&mut self) {
        let (items, error) = self.inbox_items();
        if let Mode::Inbox(state) = &mut self.mode {
            state.items = items;
            state.error = error;
            state.selected = state.selected.min(state.items.len().saturating_sub(1));
        }
    }

    pub fn handle_inbox_key(&mut self, key: &KeyEvent) {
        let Mode::Inbox(state) = &mut self.mode else {
            return;
        };
        let selected = state.selected_item().cloned();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('I') => self.mode = Mode::Control,
            KeyCode::Down | KeyCode::Char('j') => {
                state.selected = (state.selected + 1).min(state.items.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => state.selected = state.selected.saturating_sub(1),
            KeyCode::Home | KeyCode::Char('g') => state.selected = 0,
            KeyCode::End | KeyCode::Char('G') => {
                state.selected = state.items.len().saturating_sub(1);
            }
            KeyCode::Char(c @ ('a' | 'd')) => match selected {
                Some(InboxItem::Loop(r)) => {
                    if let Err(e) = self.decide_loop_run(&r.id, c == 'a') {
                        self.notice = Some(Notice::error(format!("inbox: {e}")));
                    }
                    self.reload_inbox();
                }
                Some(InboxItem::Plan { id, .. }) if c == 'd' => {
                    self.planned_workflows.retain(|p| p.id != id);
                    self.notice = Some(Notice::info("planned document discarded"));
                    self.reload_inbox();
                }
                Some(item @ InboxItem::Run { .. }) if c == 'd' => {
                    self.inbox_dismissed.insert(item.key());
                    self.notice = Some(Notice::info("dismissed; the run stays in the runs view"));
                    self.reload_inbox();
                }
                _ => {}
            },
            // the runs view holds everything about a run
            KeyCode::Enter => match selected {
                Some(InboxItem::Loop(r)) => {
                    self.open_runs_view_on(&format!("loop:{}", r.id), RunTab::Report)
                }
                Some(InboxItem::Plan { id, .. }) => {
                    self.open_workflows_view_on(RunRow::Planned(id))
                }
                Some(InboxItem::Run { run_id, .. }) => {
                    self.open_workflows_view_on(RunRow::Stored(run_id))
                }
                None => {}
            },
            KeyCode::Char('T') => match selected.as_ref() {
                Some(InboxItem::Loop(r)) if r.launch_id.is_some() => {
                    let launch = r.launch_id.clone().unwrap_or_default();
                    self.open_trace_browser_for_launch(&launch);
                }
                _ => {
                    self.notice = Some(Notice::info(
                        "select a loop run with a launch to open its traces",
                    ))
                }
            },
            _ => {}
        }
    }

    /// The runs view on a flow's run or plan.
    pub fn open_workflows_view_on(&mut self, row: RunRow) {
        let key = match row {
            RunRow::Planned(id) => format!("plan:{id}"),
            RunRow::Live(id) | RunRow::Stored(id) => format!("flow:{id}"),
            RunRow::Header(_) => String::new(),
        };
        self.open_runs_view_on(&key, RunTab::Report);
    }
}
