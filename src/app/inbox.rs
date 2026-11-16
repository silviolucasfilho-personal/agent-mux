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
    /// A factory work item that waits on the operator: triage's
    /// questions, a review that did not pass, a failure.
    Work(Box<crate::factory::Item>),
    /// A fact a station suggests for a repository's brain.
    BrainNote(crate::factory::store::PendingNote),
}

impl InboxItem {
    /// The group the item is listed under.
    pub fn group(&self) -> &'static str {
        match self {
            InboxItem::Loop(_) => "Loops",
            InboxItem::Plan { .. } | InboxItem::Run { .. } => "Workflows",
            InboxItem::Work(_) | InboxItem::BrainNote(_) => "Factory",
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
            InboxItem::Work(it) => match it.state {
                crate::factory::State::NeedsAnswer => "needs an answer",
                crate::factory::State::Unresolved => "review open",
                crate::factory::State::Rejected => "rejected",
                _ => "failed",
            },
            InboxItem::BrainNote(_) => "brain note",
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
            InboxItem::Work(_) => {
                " [↑/↓] select  [Enter] open it in the factory  [d] dismiss  [Esc] close"
            }
            InboxItem::BrainNote(_) => {
                " [↑/↓] select  [Enter] add to the brain  [e] add, then edit  [d] discard  [Esc] close"
            }
        }
    }

    pub(crate) fn key(&self) -> String {
        match self {
            InboxItem::Loop(r) => format!("loop:{}", r.id),
            InboxItem::Plan { id, .. } => format!("plan:{id}"),
            InboxItem::Run { run_id, .. } => format!("run:{run_id}"),
            // by state: an item that needs you again after a retry is listed again
            InboxItem::Work(it) => format!("work:{}:{}", it.id, it.state.as_str()),
            InboxItem::BrainNote(n) => format!("brain:{}:{}", n.item, n.note),
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
        loops + plans + runs + self.factory_needs
    }

    /// The inbox's rows: what the runs view also lists, then the factory's.
    fn inbox_rows(&self) -> (Vec<InboxItem>, Option<String>) {
        let (mut items, error) = self.inbox_items();
        items.extend(self.factory_inbox_items());
        (items, error)
    }

    pub fn open_inbox(&mut self) {
        let (items, error) = self.inbox_rows();
        self.mode = Mode::Inbox(Box::new(InboxState {
            items,
            selected: 0,
            error,
        }));
    }

    /// Re-reads the items, keeping the selection's place.
    fn reload_inbox(&mut self) {
        let (items, error) = self.inbox_rows();
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
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('I') => self.mode = Mode::Main,
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
                Some(item @ InboxItem::Work(_)) if c == 'd' => {
                    self.inbox_dismissed.insert(item.key());
                    self.last_factory_needs = None;
                    self.notice = Some(Notice::info("dismissed; the item stays in the factory"));
                    self.reload_inbox();
                }
                Some(InboxItem::BrainNote(note)) if c == 'd' => {
                    self.notice = Some(match self.decide_brain_note(&note, false) {
                        Ok(()) => Notice::info("suggestion discarded; the brain is unchanged"),
                        Err(e) => Notice::error(format!("inbox: {e}")),
                    });
                    self.reload_inbox();
                }
                _ => {}
            },
            // add the note, then open the brain to reword it
            KeyCode::Char('e') => {
                if let Some(InboxItem::BrainNote(note)) = selected {
                    match self.decide_brain_note(&note, true) {
                        Ok(()) => {
                            self.reload_inbox();
                            self.edit_factory_brain(std::path::Path::new(&note.workspace));
                        }
                        Err(e) => self.notice = Some(Notice::error(format!("inbox: {e}"))),
                    }
                }
            }
            // the runs view holds everything about a run
            KeyCode::Enter => match selected {
                Some(InboxItem::Loop(r)) => {
                    self.open_runs_view_on(&format!("loop:{}", r.id), RunTab::Report)
                }
                Some(InboxItem::Plan { id, .. }) => {
                    self.open_workflows_view_on(RunRow::Planned(id))
                }
                // a flow's run has a screen of its own
                Some(InboxItem::Run { run_id, .. })
                    if !self.open_run_screen(&run_id, super::run_screen::Origin::Agents) =>
                {
                    self.open_workflows_view_on(RunRow::Stored(run_id))
                }
                Some(InboxItem::Run { .. }) => {}
                Some(InboxItem::Work(it)) => self.open_factory_on(&it.id, &it.workspace),
                Some(InboxItem::BrainNote(note)) => {
                    self.notice = Some(match self.decide_brain_note(&note, true) {
                        Ok(()) => Notice::info(format!(
                            "added to the brain of {}",
                            crate::factory::brain::repo_name(std::path::Path::new(&note.workspace))
                        )),
                        Err(e) => Notice::error(format!("inbox: {e}")),
                    });
                    self.reload_inbox();
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
