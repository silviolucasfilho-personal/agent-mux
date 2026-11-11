//! The Swarm screen (`Mode::Swarm`), in the Agents layout
//! (`docs/superpowers/specs/2026-11-02-swarm-agents-layout-design.md` §2).
//!
//! The sidebar is a tree of every swarm (live ones, then those that ended in
//! the last week): a `◆` swarm row, its coordinator row (the planner run until
//! the coordinator agent replaces it) and one row per plan task, each in its
//! `engine::shown` state with `!` when it needs the operator and `⚑n` on its
//! swarm. The main pane shows the selected row: a live session's terminal at
//! its own size (the Agents screen's pane, `Enter` types into it), else the
//! swarm's card (Needs you, plan, check, verbs) or the task's card (Task,
//! Live, Thread, Changes). Messages are written on the `>` command line
//! (`[!] [@who] [#topic] body`, `@all` or `@<task key>`).
//!
//! The view holds no App state: it reads the store through its own
//! connection, takes the live sessions and the plan checks from the App at
//! each refresh, and answers keys with an [`Outcome`] the App carries out.

use crate::app::text_area::TextArea;
use crate::bus::swarm::engine::{self, Shown, Worker, WorkerState};
use crate::bus::swarm::plan::{CheckEnv, PlannedTask, Problems};
use crate::bus::swarm::record::{self, BoardTask, SwarmRun, SwarmStatus, SwarmVerb};
use crate::bus::swarm::{LiveSwarmSession, open_questions};
use crate::bus::{BusMessage, TaskIsolation, TaskStatus};
use crate::harness::Harness;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// How often a tick may ask the store whether anything changed.
const REFRESH_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// How often the open Changes tab reads its `git` diff again while the
/// swarm runs.
const DIFF_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// Which part of the screen has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The tree.
    Board,
    /// A card in the pane (scroll, or choose a message).
    Panel,
}

/// The panel's tabs, in the order `1`–`5` choose them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Task,
    Live,
    Thread,
    /// The worker's turns, tool calls and sub-agents from the store, with
    /// tokens and cost (coordinator-first design §9.2).
    Trace,
    Changes,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Task, Tab::Live, Tab::Thread, Tab::Trace, Tab::Changes];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Task => "Task",
            Tab::Live => "Live",
            Tab::Thread => "Thread",
            Tab::Trace => "Trace",
            Tab::Changes => "Changes",
        }
    }

    /// The tab a task's state calls for (§8.1).
    pub fn for_state(state: Shown) -> Tab {
        match state {
            Shown::Asking => Tab::Thread,
            Shown::Working | Shown::Stalled | Shown::Prompt => Tab::Live,
            Shown::Review => Tab::Changes,
            _ => Tab::Task,
        }
    }
}

/// The swarm row's tabs, in the order `1`–`6` choose them
/// (coordinator-first design §9.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwarmTab {
    Board,
    /// Every live terminal of the swarm at once (§9.5).
    Live,
    /// One lane per agent, time left to right (`tracing::swarm_report`).
    Timeline,
    /// The swarm's log, `swarm_events` newest last.
    Log,
    /// Every message of the swarm.
    Activity,
    /// The combined diff: each worktree's branch, and the workspace.
    Changes,
}

impl SwarmTab {
    pub const ALL: [SwarmTab; 6] = [
        SwarmTab::Board,
        SwarmTab::Live,
        SwarmTab::Timeline,
        SwarmTab::Log,
        SwarmTab::Activity,
        SwarmTab::Changes,
    ];
    pub fn label(self) -> &'static str {
        match self {
            SwarmTab::Board => "Board",
            SwarmTab::Live => "Live",
            SwarmTab::Timeline => "Timeline",
            SwarmTab::Log => "Log",
            SwarmTab::Activity => "Activity",
            SwarmTab::Changes => "Changes",
        }
    }
}

/// A worker's sub-agent (a Claude `Task` run, a Codex sub-agent): an
/// `observations` row of type `agent` under the worker's launch. It has no
/// terminal of its own; its row connects to its worker's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubAgent {
    /// The observation's id and turn, for its rows in the Trace tab.
    pub id: String,
    pub trace_id: String,
    pub name: String,
    pub start_ns: i64,
    pub end_ns: Option<i64>,
}

/// What only the operator can resolve, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NeedKind {
    Question,
    Prompt,
    Plan,
    PlannerFailed,
    Failed,
    Lost,
    Stalled,
    Review,
    Proposed,
    CheckFailed,
    /// agent-mux started a task itself: the coordinator did not launch it
    /// within the grace period (§9.2).
    CoordinatorQuiet,
    Finished,
}

impl NeedKind {
    pub fn label(self) -> &'static str {
        match self {
            NeedKind::Question => "question",
            NeedKind::Prompt => "prompt",
            NeedKind::Plan => "plan",
            NeedKind::PlannerFailed => "coordinator",
            NeedKind::Failed => "failed",
            NeedKind::Lost => "lost",
            NeedKind::Stalled => "stalled",
            NeedKind::Review => "review",
            NeedKind::Proposed => "proposed",
            NeedKind::CheckFailed => "check",
            NeedKind::CoordinatorQuiet => "coordinator quiet",
            NeedKind::Finished => "finished",
        }
    }

    /// The keys the item takes besides `Enter` and `d`.
    pub fn verbs(self) -> &'static str {
        match self {
            NeedKind::Question => "Enter answer",
            NeedKind::Prompt | NeedKind::Stalled => "Enter attach · r retry",
            NeedKind::Plan => "Enter on ★: confirm in the chat",
            NeedKind::PlannerFailed => "Enter on ★: talk to it · x then r on ★ restarts it",
            NeedKind::Failed | NeedKind::Lost => "r retry",
            NeedKind::Review => "y merge · Y merge past the check · x set aside",
            NeedKind::Proposed => "y accept · x drop",
            NeedKind::CheckFailed => "Enter read it · r retry a task",
            NeedKind::CoordinatorQuiet => "Enter on ★: talk to it",
            NeedKind::Finished => "y close",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Need {
    pub kind: NeedKind,
    /// Stable across refreshes, for `d` (dismiss).
    pub key: String,
    pub task_id: Option<String>,
    pub message: Option<BusMessage>,
    pub text: String,
}

/// The `>` command line: a message, or a re-plan's notes.
#[derive(Debug, Clone)]
pub struct CommandLine {
    pub text: TextArea,
    pub reply_to: Option<BusMessage>,
    pub purpose: LinePurpose,
    completion: Option<(String, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinePurpose {
    Message,
    ReplanNotes,
    /// The swarm's merge gate: `off`, `on` or a command (`M`, v26).
    MergeGate,
}

/// Who a message goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    Everyone,
    Launch { launch: String, name: String },
}

/// A command line read against the board: what would be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub to: Recipient,
    pub urgent: bool,
    pub topic: String,
    pub body: String,
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchField {
    /// The coordinator's harness: the one field the dialog asks
    /// (coordinator-first design §4.1).
    Planner,
    /// How finished tasks are reviewed: `auto`, a harness, or `off`
    /// (`bus::swarm::review`); shown folded too, so the step is never hidden.
    Review,
    /// The fold that shows the rest.
    Advanced,
    Goal,
    WorkerHarness,
    Workers,
    Isolation,
    Gate,
    Check,
    /// Whether merges wait for a check: empty or `off`, `on` (the check
    /// above), or a command of its own (`bus::swarm::gate`, v26).
    MergeGate,
}

impl LaunchField {
    const ORDER: [LaunchField; 10] = [
        LaunchField::Planner,
        LaunchField::Review,
        LaunchField::Advanced,
        LaunchField::Goal,
        LaunchField::WorkerHarness,
        LaunchField::Workers,
        LaunchField::Isolation,
        LaunchField::Gate,
        LaunchField::Check,
        LaunchField::MergeGate,
    ];
    /// The next field: folded, Tab moves between the coordinator, the
    /// review and the Advanced fold only.
    fn step(self, d: isize, advanced: bool) -> Self {
        let order: &[LaunchField] = if advanced {
            &Self::ORDER
        } else {
            &Self::ORDER[..3]
        };
        let i = order.iter().position(|f| *f == self).unwrap_or(0) as isize;
        order[(i + d).rem_euclid(order.len() as isize) as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskField {
    Key,
    Title,
    Instructions,
    Acceptance,
    Files,
    DependsOn,
    Harness,
    Agent,
    Isolation,
}

impl TaskField {
    const ORDER: [TaskField; 9] = [
        TaskField::Key,
        TaskField::Title,
        TaskField::Instructions,
        TaskField::Acceptance,
        TaskField::Files,
        TaskField::DependsOn,
        TaskField::Harness,
        TaskField::Agent,
        TaskField::Isolation,
    ];
    /// Whether the field runs the task (harness, agent, isolation): only
    /// for a task that has not started, or is being retried.
    pub fn runs(self) -> bool {
        matches!(
            self,
            TaskField::Harness | TaskField::Agent | TaskField::Isolation
        )
    }
    fn step(self, d: isize, with_key: bool, with_runs: bool) -> Self {
        let order: Vec<TaskField> = Self::ORDER
            .iter()
            .copied()
            .filter(|f| with_key || *f != TaskField::Key)
            .filter(|f| with_runs || !f.runs())
            .collect();
        let i = order.iter().position(|f| *f == self).unwrap_or(0) as isize;
        order[(i + d).rem_euclid(order.len() as isize) as usize]
    }
    pub fn label(self) -> &'static str {
        match self {
            TaskField::Key => "Key",
            TaskField::Title => "Title",
            TaskField::Instructions => "Instructions",
            TaskField::Acceptance => "Done when",
            TaskField::Files => "Files (comma separated)",
            TaskField::DependsOn => "Depends on (task keys, comma separated)",
            TaskField::Harness => "Harness",
            TaskField::Agent => "Agent (empty: none)",
            TaskField::Isolation => "Isolation",
        }
    }
}

#[derive(Clone)]
pub enum Modal {
    Launch {
        goal: TextArea,
        planner: usize,
        /// The workers' pool: one tick per entry of `harnesses`.
        pool: Vec<bool>,
        /// The pool entry `←`/`→` is on; `Space` ticks it.
        pool_cursor: usize,
        /// The pool follows the coordinator until the operator ticks one.
        pool_touched: bool,
        workers: usize,
        isolation: TaskIsolation,
        gate: bool,
        check: TextArea,
        /// The merge gate: empty or `off`, `on`, or its own command.
        merge_gate: TextArea,
        /// The review: 0 `auto`, 1..=n the harnesses, n+1 `off`
        /// ([`SwarmBoard::review_choice`]).
        review: usize,
        field: LaunchField,
        /// Why the last Ctrl+S did not start the swarm, shown in the form.
        error: Option<String>,
        /// The Advanced fold is open: goal, pool, limits, isolation, gate,
        /// check. Folded, the dialog asks for the coordinator only.
        advanced: bool,
    },
    /// Add a task (`editing` none) or edit one; `retry` puts it back on
    /// the board when saved (stopping `stop`, its worker, first).
    Task {
        editing: Option<String>,
        retry: bool,
        stop: Option<usize>,
        /// Harness, agent and isolation can change: the task has not
        /// started, or is being retried.
        runs: bool,
        /// 0: the swarm's default; else `harnesses[harness - 1]`.
        harness: usize,
        agent: TextArea,
        isolation: TaskIsolation,
        key: TextArea,
        title: TextArea,
        instructions: TextArea,
        acceptance: TextArea,
        files: TextArea,
        depends_on: TextArea,
        field: TaskField,
        /// A warning or why saving was refused, drawn at the form's top.
        note: Option<String>,
    },
    /// One message in full.
    Message { message: BusMessage, scroll: usize },
    /// `o`: the swarm picker. Every live swarm (this folder's first, then
    /// the newest), then the ones that ended within [`RECENT_NS`]; `Enter`
    /// shows the one under the cursor. With swarm sessions off the Agents
    /// sidebar (`[agents] show_swarm`), this is how another swarm is reached.
    Picker {
        swarms: Vec<SwarmRun>,
        cursor: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    Approve {
        tasks: usize,
    },
    Merge {
        task_id: String,
        branch: String,
        /// Why the merge goes against the task's review, if it does.
        warning: Option<String>,
        /// The swarm's check command: the merge is queued behind it (v26).
        check: Option<String>,
    },
    /// Merge a branch at once, past the swarm's check (`Y`, v26).
    ForceMerge {
        task_id: String,
        branch: String,
        check: String,
        /// Why the merge goes against the task's review, if it does.
        warning: Option<String>,
    },
    SetAside {
        task_id: String,
        branch: String,
    },
    Close,
    Cancel {
        task_id: String,
        key: String,
    },
    Stop {
        session_idx: usize,
        key: String,
    },
    /// Delete the swarm: a live one is closed first (`d` on any of its rows).
    Delete {
        swarm_id: String,
        goal: String,
        live: bool,
        sessions: usize,
        open_tasks: usize,
        worktrees: usize,
    },
}

impl Confirm {
    /// The delete confirmation for `swarm` with its `board` and the
    /// number of its live `sessions`.
    pub fn delete_of(swarm: &SwarmRun, board: &[BoardTask], sessions: usize) -> Confirm {
        let mut worktrees: Vec<&str> = board
            .iter()
            .filter_map(|t| t.task.worktree.as_deref())
            .filter(|p| !p.is_empty())
            .collect();
        worktrees.sort_unstable();
        worktrees.dedup();
        Confirm::Delete {
            swarm_id: swarm.id.clone(),
            goal: swarm.goal.clone(),
            live: swarm.ended_ns.is_none(),
            sessions,
            open_tasks: board
                .iter()
                .filter(|t| !t.task.status.is_terminal())
                .count(),
            worktrees: worktrees.len(),
        }
    }

    pub fn question(&self) -> String {
        match self {
            Confirm::Delete {
                swarm_id,
                goal,
                live,
                sessions,
                open_tasks,
                worktrees,
            } => {
                let id = crate::bus::short_id(swarm_id);
                let goal = goal.lines().next().unwrap_or_default();
                if *live {
                    format!(
                        "Delete swarm {id} \"{goal}\"? It is running: {sessions} session(s) stop, {open_tasks} task(s) are cancelled, {worktrees} worktree(s) are removed (branches kept). [y/n]"
                    )
                } else {
                    format!(
                        "Delete swarm {id} \"{goal}\"? Its {worktrees} worktree(s) are removed (branches kept); traces stay. [y/n]"
                    )
                }
            }
            Confirm::Approve { tasks } => {
                format!("Approve the plan? {tasks} task(s); workers start now.")
            }
            Confirm::Merge {
                branch,
                warning,
                check: Some(check),
                ..
            } => format!(
                "{}Queue {branch} for its merge? `{check}` runs on it brought up to date with the workspace, and it is merged if the check passes (Y merges now).",
                warning
                    .as_deref()
                    .map(|w| format!("⚠ {w}. "))
                    .unwrap_or_default()
            ),
            Confirm::Merge {
                branch, warning, ..
            } => match warning {
                Some(w) => format!(
                    "⚠ {w}. Merge {branch} into the workspace's branch anyway (a merge commit)?"
                ),
                None => format!("Merge {branch} into the workspace's branch (a merge commit)?"),
            },
            Confirm::ForceMerge {
                branch,
                check,
                warning,
                ..
            } => format!(
                "{}Merge {branch} now, without running `{check}` on it first (a merge commit)?",
                warning
                    .as_deref()
                    .map(|w| format!("⚠ {w}. "))
                    .unwrap_or_default()
            ),
            Confirm::SetAside { branch, .. } => {
                format!("Set {branch} aside without merging? The branch is kept.")
            }
            Confirm::Close => {
                "Close the swarm? Its sessions stop, open tasks are cancelled, leases released."
                    .into()
            }
            Confirm::Cancel { key, .. } => format!("Cancel `{key}`? It will not run."),
            Confirm::Stop { key, .. } if key == "the coordinator" => {
                "Stop the coordinator? The swarm keeps running; r on its row starts a new one."
                    .into()
            }
            Confirm::Stop { key, .. } => {
                format!("Stop the worker of `{key}`? Its task is then lost (r retries it).")
            }
        }
    }
}

/// What the App does for a key.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    None,
    Close,
    Reload,
    Attach(usize),
    /// The pane takes the keyboard: keys go to this session.
    FocusPane(usize),
    Start {
        goal: String,
        planner: Harness,
        /// The workers' pool, the harness of tasks that name none first.
        workers: Vec<Harness>,
        max_workers: usize,
        isolation: TaskIsolation,
        gate: bool,
        check: Option<String>,
        /// `auto`, a harness or `off`.
        review: String,
        /// `off` (`None`), `on` or the gate's own command.
        merge_gate: Option<String>,
    },
    Verb {
        verb: SwarmVerb,
        task: Option<String>,
        notes: Option<String>,
    },
    Send {
        to_launch: Option<String>,
        reply_to: Option<String>,
        topic: String,
        body: String,
        urgent: bool,
    },
    /// Save the task's card, then put it back on the board, stopping its
    /// worker first when it still has one.
    RetryTask {
        task_id: String,
        task: PlannedTask,
        stop: Option<usize>,
    },
    /// Resume the ended worker's conversation in a new session on the same
    /// task (coordinator-first §9.5).
    Reconnect {
        task_id: String,
        launch: String,
    },
    CancelTask(String),
    StopSession(usize),
}

/// A live session as the board needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveWorker {
    pub session_idx: usize,
    pub worker: Worker,
    pub provider: String,
    pub tokens: i64,
    pub cost_usd: f64,
    /// Whether `cost_usd` is the whole cost (a harness whose generations
    /// are all priced).
    pub priced: bool,
    pub running_tool: Option<String>,
}

/// One swarm as the tree lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct SwarmData {
    pub run: SwarmRun,
    pub board: Vec<BoardTask>,
    pub messages: Vec<BusMessage>,
    pub asking: HashSet<String>,
    /// The plan's checks (read for the selected swarm only).
    pub problems: Problems,
    pub needs: Vec<Need>,
    /// The decision that approved the plan, once there is one (who relayed
    /// it, the operator's words).
    pub launch_decision: Option<crate::bus::swarm::record::SwarmDecision>,
    /// Each worker's sub-agents, by the worker's launch id.
    pub agents: HashMap<String, Vec<SubAgent>>,
    /// The swarm's log (`swarm_events`), oldest first.
    pub events: Vec<crate::bus::swarm::record::SwarmEvent>,
    /// The Timeline tab's lanes, computed once per refresh.
    pub timeline: Option<crate::tracing::swarm_report::Timeline>,
    /// How the swarm reviews finished tasks (v25).
    pub review: crate::bus::swarm::review::ReviewSettings,
    /// Whether its merges wait for a check (v26).
    pub merge_gate: crate::bus::swarm::gate::MergeGateSettings,
}

/// A row of the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// A swarm (index into `swarms`).
    Swarm(usize),
    /// Its coordinator: the planner run (until the coordinator agent).
    Coordinator(usize),
    /// A task of a swarm (swarm index, task index on its board).
    Task(usize, usize),
    /// A sub-agent of a task's worker (swarm, task, index in its list).
    Agent(usize, usize, usize),
}

/// A row by what it stands for, so the cursor survives a refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowKey {
    Swarm(String),
    Coordinator(String),
    Task(String),
    Agent(String, usize),
}

/// What the main pane shows for the selected row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneView {
    /// A live session's terminal (index into `App::sessions`).
    Session(usize),
    SwarmCard,
    TaskCard,
    /// No swarm at all: how to start one.
    Launch,
}

/// A coordinator that has not submitted a plan this long after starting
/// (or being sent back) is listed in Needs you.
pub const PLAN_TIMEOUT_NS: i64 = 15 * 60 * 1_000_000_000;

/// Swarms that ended within this long stay in the tree, folded.
pub const RECENT_NS: i64 = 7 * 24 * 3_600 * 1_000_000_000;

/// The screen's areas in the last frame (set by `ui::swarm_board`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Drawn {
    pub tree: ratatui::layout::Rect,
    /// The tree's row at the top of its list.
    pub tree_start: usize,
    /// The main pane, where a session's terminal is drawn.
    pub pane: ratatui::layout::Rect,
}

/// What a click on the screen asks of the App.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Nothing to do (a click on a border, or with a form open).
    None,
    /// The cursor moved to a tree row; the tree has the keyboard.
    Row,
    /// The pane's terminal takes the keyboard: this session's.
    FocusPane(usize),
}

pub struct SwarmBoard {
    /// The folder the screen was opened from: its swarm comes first, and a
    /// new swarm starts there.
    pub workspace: PathBuf,
    /// The launch form as it was submitted, until the App says whether the
    /// swarm started (`launch_failed`, `launch_started`).
    pub last_launch: Option<Modal>,
    /// Agents that edit files or run commands (`plan::writing_agents`):
    /// agy runs them read-only, so a task must not pair them (the App sets
    /// it when it opens and refreshes the screen).
    pub writing_agents: Vec<String>,
    /// The one swarm this screen shows (coordinator-first design §9.1): its
    /// coordinator, the agents it started and their sub-agents. `None`
    /// until a swarm is chosen: the folder's newest live one, else the
    /// empty screen and the New swarm dialog.
    pub scope: Option<String>,
    /// The swarm the next refresh puts the cursor on (one just started).
    select_next: Option<String>,
    /// With `select_next`: the coordinator's row rather than the swarm's.
    select_next_coordinator: bool,
    /// Every swarm the tree lists.
    pub swarms: Vec<SwarmData>,
    pub rows: Vec<Row>,
    pub cursor: usize,
    folded: HashSet<String>,
    /// Tasks whose ended sub-agents are listed too (live ones always are).
    agent_unfolded: HashSet<String>,
    seen: HashSet<String>,
    /// The swarm row's tab.
    pub swarm_tab: SwarmTab,
    /// The tile `↑↓←→` are on in the Live mosaic.
    pub tile_cursor: usize,
    /// The pane has the keyboard: keys go to the selected row's session.
    pub pane_focused: bool,
    /// The cursor is on a task (`selected` indexes the board).
    on_task: bool,
    /// The live swarm, or the one that ended last (shown until a new one).
    pub swarm: Option<SwarmRun>,
    pub board: Vec<BoardTask>,
    pub messages: Vec<BusMessage>,
    /// The plan's checks, while it is drafted or reviewed.
    pub problems: Problems,
    /// How the plan was approved, once it was.
    pub launch_decision: Option<crate::bus::swarm::record::SwarmDecision>,
    /// The selected swarm's log and timeline (from its `SwarmData`).
    pub events: Vec<crate::bus::swarm::record::SwarmEvent>,
    pub timeline: Option<crate::tracing::swarm_report::Timeline>,
    /// The Timeline tab's magnification (1× is the whole swarm) and pan,
    /// in tenths of the shown span.
    pub tl_zoom: u32,
    pub tl_pan: i32,
    /// The Log tab's kind filter (`t`) and text filter (`/`).
    pub log_kind: crate::tracing::swarm_report::LogKind,
    pub log_filter: String,
    /// `/` is typing the text filter.
    pub log_editing: bool,
    /// The Trace tab's lines, for `(launch, sub-agent)`.
    pub trace_lines: Vec<String>,
    trace_for: Option<(String, Option<String>)>,
    pub live: HashMap<String, LiveWorker>,
    pub workers: HashMap<String, Worker>,
    pub asking: HashSet<String>,
    pub selected: usize,
    pub focus: Focus,
    chosen_tab: Option<(String, Tab)>,
    pub activity: bool,
    pub panel_scroll: usize,
    /// The card's furthest scroll and its visible height, from the last
    /// frame (`None` before the first: no clamp yet).
    pub panel_max: std::cell::Cell<Option<usize>>,
    pub panel_page: std::cell::Cell<usize>,
    pub selected_message: usize,
    pub needs: Vec<Need>,
    pub diff_lines: Vec<String>,
    diff_for: Option<String>,
    pub modal: Option<Modal>,
    pub confirm: Option<Confirm>,
    pub command: Option<CommandLine>,
    pub status_msg: Option<String>,
    /// Harnesses the launch form cycles through (the installed ones).
    pub harnesses: Vec<Harness>,
    pub default_harness: Harness,
    pub now_ns: i64,
    /// Where the last frame drew the tree and the pane, and the tree's first
    /// row shown: what a click lands on ([`SwarmBoard::click`]).
    pub drawn: std::cell::Cell<Drawn>,
    conn: Option<Connection>,
    last_change_seq: Option<i64>,
    live_fingerprint: u64,
    last_check: Option<std::time::Instant>,
    /// The periodic read in flight ([`SwarmBoard::refresh_in_background`]).
    loading: Option<Loading>,
    /// Bumped by every synchronous read: a background read started before
    /// it is stale.
    generation: u64,
    /// When the Changes tab's diff was last read in the background.
    diff_at: Option<std::time::Instant>,
}

impl std::fmt::Debug for SwarmBoard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwarmBoard")
            .field("workspace", &self.workspace)
            .field("swarms", &self.swarms.len())
            .field("cursor", &self.cursor)
            .field("swarm", &self.swarm.as_ref().map(|s| (&s.id, s.status)))
            .field("tasks", &self.board.len())
            .field("selected", &self.selected)
            .field("focus", &self.focus)
            .finish_non_exhaustive()
    }
}

fn fingerprint(live: &[LiveSwarmSession]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for s in live {
        s.launch_id.hash(&mut h);
        s.state.hash(&mut h);
        s.turns.hash(&mut h);
        s.tokens.hash(&mut h);
        s.running_tool.hash(&mut h);
        s.quiet_ns
            .is_some_and(|q| q >= engine::STALL_GRACE_NS)
            .hash(&mut h);
    }
    h.finish()
}

fn worker_state(state: &str) -> WorkerState {
    match state {
        "working" => WorkerState::Working,
        "attention" | "needs_attention" | "waiting" => WorkerState::Prompt,
        "exited" => WorkerState::Exited,
        _ => WorkerState::Idle,
    }
}

impl SwarmBoard {
    /// Opens the screen on `workspace`, reading `conn` (kept for later
    /// refreshes).
    pub fn open(
        workspace: PathBuf,
        conn: Option<Connection>,
        live: &[LiveSwarmSession],
        env: Option<&CheckEnv>,
        harnesses: Vec<Harness>,
        default_harness: Harness,
        now_ns: i64,
    ) -> Self {
        let mut v = SwarmBoard {
            last_launch: None,
            writing_agents: Vec::new(),
            scope: None,
            select_next: None,
            select_next_coordinator: false,
            workspace,
            swarms: Vec::new(),
            rows: Vec::new(),
            cursor: 0,
            folded: HashSet::new(),
            agent_unfolded: HashSet::new(),
            seen: HashSet::new(),
            swarm_tab: SwarmTab::Board,
            tile_cursor: 0,
            pane_focused: false,
            on_task: false,
            swarm: None,
            board: Vec::new(),
            messages: Vec::new(),
            problems: Problems::default(),
            launch_decision: None,
            events: Vec::new(),
            timeline: None,
            tl_zoom: 1,
            tl_pan: 0,
            log_kind: crate::tracing::swarm_report::LogKind::All,
            log_filter: String::new(),
            log_editing: false,
            trace_lines: Vec::new(),
            trace_for: None,
            live: HashMap::new(),
            workers: HashMap::new(),
            asking: HashSet::new(),
            selected: 0,
            focus: Focus::Board,
            chosen_tab: None,
            activity: false,
            panel_scroll: 0,
            panel_max: std::cell::Cell::new(None),
            panel_page: std::cell::Cell::new(10),
            selected_message: 0,
            needs: Vec::new(),
            diff_lines: Vec::new(),
            diff_for: None,
            modal: None,
            confirm: None,
            command: None,
            status_msg: None,
            harnesses,
            default_harness,
            now_ns,
            conn,
            last_change_seq: None,
            live_fingerprint: 0,
            last_check: None,
            loading: None,
            generation: 0,
            diff_at: None,
            drawn: std::cell::Cell::new(Drawn::default()),
        };
        v.refresh(live, env, now_ns);
        v
    }

    fn norm_workspace(&self) -> String {
        crate::bus::store::normalize_workspace(&self.workspace)
    }

    /// Whether the plan's checks are wanted at the next refresh (the App
    /// computes the [`CheckEnv`] only then).
    pub fn wants_checks(&self) -> bool {
        self.swarm
            .as_ref()
            .is_none_or(|s| matches!(s.status, SwarmStatus::Planning | SwarmStatus::Review))
    }

    /// Whether the store's change feed or a live session moved since the
    /// last read; at most one store read per [`REFRESH_CHECK_INTERVAL`].
    pub fn refresh_due(&mut self, live: &[LiveSwarmSession], tick: std::time::Instant) -> bool {
        if self.loading.is_some() {
            return false;
        }
        if self
            .last_check
            .is_some_and(|t| tick.duration_since(t) < REFRESH_CHECK_INTERVAL)
        {
            return false;
        }
        self.last_check = Some(tick);
        if fingerprint(live) != self.live_fingerprint {
            return true;
        }
        let Some(conn) = &self.conn else {
            return false;
        };
        match crate::tracing::store::latest_change_seq(conn).ok() {
            Some(seq) => self.last_change_seq != Some(seq),
            None => true,
        }
    }

    /// Re-reads the swarm, its board and messages, keeping the selection on
    /// the same task.
    /// The workspace of the selected swarm (else the one opened from): the
    /// plan's checks are read against it.
    pub fn current_workspace(&self) -> PathBuf {
        self.swarm
            .as_ref()
            .map(|s| PathBuf::from(&s.workspace))
            .unwrap_or_else(|| self.workspace.clone())
    }

    fn row_key(&self, row: Row) -> Option<RowKey> {
        Some(match row {
            Row::Swarm(i) => RowKey::Swarm(self.swarms.get(i)?.run.id.clone()),
            Row::Coordinator(i) => RowKey::Coordinator(self.swarms.get(i)?.run.id.clone()),
            Row::Task(i, j) => RowKey::Task(self.swarms.get(i)?.board.get(j)?.task.id.clone()),
            Row::Agent(i, j, k) => {
                RowKey::Agent(self.swarms.get(i)?.board.get(j)?.task.id.clone(), k)
            }
        })
    }

    /// Re-reads every swarm, its board and messages, keeping the cursor on
    /// the same row. Synchronous: the screen's opening read and the read
    /// after an operator's action. The periodic reads go through
    /// [`SwarmBoard::refresh_in_background`].
    pub fn refresh(&mut self, live: &[LiveSwarmSession], env: Option<&CheckEnv>, now_ns: i64) {
        // a read in flight would bring older rows than these
        self.generation += 1;
        self.loading = None;
        self.set_live(live, now_ns);
        let loaded = self
            .conn
            .as_ref()
            .map(|conn| load(conn, &self.load_input(env, now_ns, None, None)));
        self.apply(loaded);
        if self.on_task && self.tab() == Tab::Changes {
            self.diff_for = None;
            self.load_diff();
        }
        if self.on_task && self.tab() == Tab::Trace {
            self.trace_for = None;
            self.load_trace();
        }
    }

    /// Starts the periodic read on a thread of its own, reading the store at
    /// `db`; [`SwarmBoard::poll_refresh`] applies it. The UI thread only
    /// takes the live sessions' telemetry, which it already has: a store
    /// of hundreds of megabytes, a swarm of dozens of launches and the
    /// `git` calls of the Changes tab never hold the keyboard up.
    pub fn refresh_in_background(
        &mut self,
        live: &[LiveSwarmSession],
        env: Option<&CheckEnv>,
        now_ns: i64,
        db: PathBuf,
    ) {
        if self.loading.is_some() {
            return;
        }
        self.set_live(live, now_ns);
        // the open tab's own reads go along: the diff at most every
        // `DIFF_INTERVAL` (each is several `git` processes per worktree)
        let diff = (self.on_task
            && self.tab() == Tab::Changes
            && self.diff_at.is_none_or(|t| t.elapsed() >= DIFF_INTERVAL))
        .then(|| self.diff_job())
        .flatten();
        let trace = (self.on_task && self.tab() == Tab::Trace)
            .then(|| self.trace_job())
            .flatten();
        let input = self.load_input(env, now_ns, diff, trace);
        let (tx, rx) = std::sync::mpsc::channel();
        let generation = self.generation;
        let spawned = std::thread::Builder::new()
            .name("swarm-screen-read".into())
            .spawn(move || {
                let loaded = crate::tracing::store::open_ro(&db)
                    .ok()
                    .map(|conn| load(&conn, &input));
                let _ = tx.send(loaded);
            });
        if spawned.is_ok() {
            self.loading = Some(Loading { generation, rx });
        }
    }

    /// Whether a background read is under way.
    pub fn loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Applies a finished background read; `true` when it changed the
    /// screen. A read started before a synchronous one is dropped.
    pub fn poll_refresh(&mut self) -> bool {
        let Some(l) = &self.loading else {
            return false;
        };
        let loaded = match l.rx.try_recv() {
            Ok(loaded) => loaded,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        let current = l.generation == self.generation;
        self.loading = None;
        let Some(loaded) = loaded.filter(|_| current) else {
            return false;
        };
        let (diff, trace) = (loaded.diff.clone(), loaded.trace.clone());
        self.apply(Some(loaded));
        // the tab's reads land only if the cursor is still where they were
        // asked for
        if let Some((key, lines)) = diff {
            self.diff_at = Some(std::time::Instant::now());
            if self.on_task
                && self.tab() == Tab::Changes
                && self.diff_job().is_some_and(|j| j.key == key)
            {
                self.diff_lines = lines;
                self.diff_for = Some(key);
            }
        }
        if let Some((want, lines)) = trace
            && self.on_task
            && self.tab() == Tab::Trace
            && self.trace_job().is_some_and(|j| j.want == want)
        {
            self.trace_lines = lines;
            self.trace_for = Some(want);
        }
        true
    }

    /// The live sessions' telemetry, kept on the UI thread.
    fn set_live(&mut self, live: &[LiveSwarmSession], now_ns: i64) {
        self.now_ns = now_ns;
        self.live_fingerprint = fingerprint(live);
        self.live = live
            .iter()
            .map(|s| {
                (
                    s.launch_id.clone(),
                    LiveWorker {
                        session_idx: s.session_idx,
                        worker: Worker {
                            state: worker_state(&s.state),
                            quiet_ns: s.quiet_ns.unwrap_or(0),
                        },
                        provider: s.provider.clone(),
                        tokens: s.tokens,
                        cost_usd: s.cost_usd,
                        priced: s.priced,
                        running_tool: s.running_tool.clone(),
                    },
                )
            })
            .collect();
        self.workers = self
            .live
            .iter()
            .map(|(k, w)| (k.clone(), w.worker))
            .collect();
    }

    fn load_input(
        &self,
        env: Option<&CheckEnv>,
        now_ns: i64,
        diff: Option<DiffJob>,
        trace: Option<TraceJob>,
    ) -> LoadInput {
        LoadInput {
            scope: self.scope.clone(),
            here: self.norm_workspace(),
            current: self.swarm.as_ref().map(|s| s.id.clone()),
            workers: self.workers.clone(),
            env: env.cloned(),
            now_ns,
            diff,
            trace,
        }
    }

    /// The Changes tab's read for the task under the cursor.
    fn diff_job(&self) -> Option<DiffJob> {
        let t = self.selected_task()?;
        Some(DiffJob {
            key: format!("{}:{:?}", t.task.id, t.task.worktree),
            worktree: t.task.worktree.as_deref().map(PathBuf::from),
            workspace: self.workspace.clone(),
        })
    }

    /// The Trace tab's read for the task (or sub-agent) under the cursor.
    fn trace_job(&self) -> Option<TraceJob> {
        let launch = self.selected_task()?.task.assigned_to.clone()?;
        let agent = self
            .rows
            .get(self.cursor)
            .copied()
            .and_then(|r| self.agent_of(r))
            .map(|a| a.id.clone());
        Some(TraceJob {
            want: (launch, agent),
        })
    }

    /// Puts a read on the screen, keeping the cursor on the same row (the
    /// row it is on now: the operator may have moved while it was read).
    fn apply(&mut self, loaded: Option<Loaded>) {
        let to_coordinator = std::mem::take(&mut self.select_next_coordinator);
        let key = self
            .select_next
            .take()
            .map(|id| {
                if to_coordinator {
                    RowKey::Coordinator(id)
                } else {
                    RowKey::Swarm(id)
                }
            })
            .or_else(|| self.rows.get(self.cursor).and_then(|r| self.row_key(*r)));
        let prev_len = self.list_len();
        let at_bottom = prev_len == 0 || self.selected_message + 1 >= prev_len;
        if let Some(loaded) = loaded {
            self.last_change_seq = loaded.change_seq;
            if self.scope.is_none() {
                self.scope = loaded.scope;
            }
            for d in &loaded.swarms {
                if !self.seen.contains(&d.run.id) {
                    self.seen.insert(d.run.id.clone());
                    if d.run.ended_ns.is_some() {
                        self.folded.insert(d.run.id.clone());
                    }
                }
            }
            self.swarms = loaded.swarms;
        }
        self.rebuild_rows();
        self.cursor = key
            .and_then(|k| {
                self.rows
                    .iter()
                    .position(|r| self.row_key(*r).as_ref() == Some(&k))
            })
            .unwrap_or_else(|| self.cursor.min(self.rows.len().saturating_sub(1)));
        self.sync_current();
        let len = self.list_len();
        if at_bottom || self.selected_message >= len {
            self.selected_message = len.saturating_sub(1);
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (i, d) in self.swarms.iter().enumerate() {
            self.rows.push(Row::Swarm(i));
            if self.folded.contains(&d.run.id) {
                continue;
            }
            if d.run.planner_launch.is_some() {
                self.rows.push(Row::Coordinator(i));
            }
            for (j, t) in d.board.iter().enumerate() {
                self.rows.push(Row::Task(i, j));
                // its worker's sub-agents: the live ones always, the ended
                // ones when the task is unfolded (→)
                let unfolded = self.agent_unfolded.contains(&t.task.id);
                if let Some(agents) = t.task.assigned_to.as_deref().and_then(|l| d.agents.get(l)) {
                    for (k, a) in agents.iter().enumerate() {
                        if unfolded || a.end_ns.is_none() {
                            self.rows.push(Row::Agent(i, j, k));
                        }
                    }
                }
            }
        }
    }

    /// Whether task `j` of swarm `i` has ended sub-agents folded away.
    pub fn has_folded_agents(&self, i: usize, j: usize) -> bool {
        let Some(d) = self.swarms.get(i) else {
            return false;
        };
        let Some(t) = d.board.get(j) else {
            return false;
        };
        if self.agent_unfolded.contains(&t.task.id) {
            return false;
        }
        t.task
            .assigned_to
            .as_deref()
            .and_then(|l| d.agents.get(l))
            .is_some_and(|a| a.iter().any(|x| x.end_ns.is_some()))
    }

    /// The sub-agent a row stands for.
    pub fn agent_of(&self, r: Row) -> Option<&SubAgent> {
        let Row::Agent(i, j, k) = r else {
            return None;
        };
        let d = self.swarms.get(i)?;
        let launch = d.board.get(j)?.task.assigned_to.as_deref()?;
        d.agents.get(launch)?.get(k)
    }

    /// Points the selected-swarm fields at the cursor's swarm.
    fn sync_current(&mut self) {
        let (si, task) = match self.rows.get(self.cursor) {
            Some(Row::Swarm(i)) | Some(Row::Coordinator(i)) => (Some(*i), None),
            Some(Row::Task(i, j)) | Some(Row::Agent(i, j, _)) => (Some(*i), Some(*j)),
            None => (None, None),
        };
        match si.and_then(|i| self.swarms.get(i)) {
            Some(d) => {
                self.swarm = Some(d.run.clone());
                self.board = d.board.clone();
                self.messages = d.messages.clone();
                self.asking = d.asking.clone();
                self.problems = d.problems.clone();
                self.needs = d.needs.clone();
                self.launch_decision = d.launch_decision.clone();
                self.events = d.events.clone();
                self.timeline = d.timeline.clone();
            }
            None => {
                self.swarm = None;
                self.board.clear();
                self.messages.clear();
                self.asking.clear();
                self.problems = Problems::default();
                self.needs.clear();
                self.launch_decision = None;
                self.events.clear();
                self.timeline = None;
            }
        }
        self.on_task = task.is_some();
        self.selected = task.unwrap_or(0);
    }

    /// What the main pane shows.
    pub fn pane(&self) -> PaneView {
        match self.rows.get(self.cursor) {
            None => PaneView::Launch,
            Some(Row::Swarm(_)) => PaneView::SwarmCard,
            Some(Row::Coordinator(_)) => match self.coordinator_session() {
                Some(w) => PaneView::Session(w.session_idx),
                None => PaneView::SwarmCard,
            },
            Some(Row::Task(..)) | Some(Row::Agent(..)) => {
                match (
                    self.tab(),
                    self.selected_task().and_then(|t| self.worker_of(t)),
                ) {
                    (Tab::Live, Some(w)) => PaneView::Session(w.session_idx),
                    _ => PaneView::TaskCard,
                }
            }
        }
    }

    /// The session the pane shows, if it shows one.
    pub fn pane_session(&self) -> Option<usize> {
        match self.pane() {
            PaneView::Session(i) => Some(i),
            _ => None,
        }
    }

    /// Whether row `r` needs the operator: a task with a need, or a swarm
    /// with one of its own (plan, planner, check, finished).
    pub fn row_needs(&self, r: Row) -> usize {
        match r {
            Row::Swarm(i) => self.swarms.get(i).map_or(0, |d| d.needs.len()),
            Row::Coordinator(_) | Row::Agent(..) => 0,
            Row::Task(i, j) => self.swarms.get(i).map_or(0, |d| {
                let id = d.board.get(j).map(|t| t.task.id.as_str());
                d.needs
                    .iter()
                    .filter(|n| n.task_id.as_deref() == id && id.is_some())
                    .count()
            }),
        }
    }

    /// Every need of the live swarms.
    pub fn needs_total(&self) -> usize {
        self.swarms
            .iter()
            .filter(|d| d.run.ended_ns.is_none())
            .map(|d| d.needs.len())
            .sum()
    }

    pub fn is_folded(&self, swarm: usize) -> bool {
        self.swarms
            .get(swarm)
            .is_some_and(|d| self.folded.contains(&d.run.id))
    }

    fn set_cursor(&mut self, at: usize) {
        let at = at.min(self.rows.len().saturating_sub(1));
        if at != self.cursor || self.swarm.is_none() {
            self.cursor = at;
            self.pane_focused = false;
            self.sync_current();
            self.after_selection_change();
        }
    }

    fn fold(&mut self, fold: bool) {
        let si = match self.rows.get(self.cursor).copied() {
            Some(Row::Swarm(i)) => i,
            Some(Row::Task(i, j)) => {
                // → on a worker lists its ended sub-agents too; ← folds them,
                // or climbs to the swarm
                let id = self.swarms[i].board[j].task.id.clone();
                if !fold {
                    if self.agent_unfolded.insert(id) {
                        self.rebuild_rows();
                    } else if let Some(next) = self.rows.get(self.cursor + 1).copied()
                        && matches!(next, Row::Agent(..))
                    {
                        self.set_cursor(self.cursor + 1);
                    }
                } else if self.agent_unfolded.remove(&id) {
                    self.rebuild_rows();
                } else {
                    let at = self
                        .rows
                        .iter()
                        .position(|r| *r == Row::Swarm(i))
                        .unwrap_or(0);
                    self.set_cursor(at);
                }
                return;
            }
            Some(Row::Agent(i, j, _)) => {
                if fold {
                    // ← on a sub-agent climbs to its worker
                    let at = self
                        .rows
                        .iter()
                        .position(|r| *r == Row::Task(i, j))
                        .unwrap_or(0);
                    self.set_cursor(at);
                }
                return;
            }
            Some(Row::Coordinator(i)) => {
                if fold {
                    let at = self
                        .rows
                        .iter()
                        .position(|r| *r == Row::Swarm(i))
                        .unwrap_or(0);
                    self.set_cursor(at);
                }
                return;
            }
            None => return,
        };
        let id = self.swarms[si].run.id.clone();
        let changed = if fold {
            self.folded.insert(id)
        } else {
            self.folded.remove(&id)
        };
        if changed {
            self.rebuild_rows();
            self.cursor = self
                .rows
                .iter()
                .position(|r| *r == Row::Swarm(si))
                .unwrap_or(0);
        } else if !fold
            && let Some(next) = self.rows.get(self.cursor + 1).copied()
            && !matches!(next, Row::Swarm(_))
        {
            // → on an open swarm descends to its first child
            self.set_cursor(self.cursor + 1);
        }
    }

    /// `!`: the next row (after the cursor, wrapping) that needs you.
    fn next_need(&mut self) {
        let n = self.rows.len();
        let found = (1..=n).map(|k| (self.cursor + k) % n.max(1)).find(|&i| {
            let r = self.rows[i];
            // a swarm row counts for its own needs, not its tasks'
            match r {
                Row::Swarm(si) => self.swarms[si]
                    .needs
                    .iter()
                    .any(|need| need.task_id.is_none()),
                _ => self.row_needs(r) > 0,
            }
        });
        match found {
            Some(i) => self.set_cursor(i),
            None => self.status_msg = Some("Nothing needs you right now.".into()),
        }
    }

    /// A form, the command line, or the pane typing into a session: letters
    /// are text, not the screen's verbs.
    pub fn text_entry_active(&self) -> bool {
        self.modal.is_some() || self.command.is_some() || self.pane_focused || self.log_editing
    }

    pub fn confirmation_active(&self) -> bool {
        self.confirm.is_some()
    }

    pub fn selected_task(&self) -> Option<&BoardTask> {
        if self.on_task {
            self.board.get(self.selected)
        } else {
            None
        }
    }

    fn input(&self) -> Option<engine::Input<'_>> {
        let swarm = self.swarm.as_ref()?;
        Some(engine::Input {
            swarm,
            board: &self.board,
            workers: &self.workers,
            asking: &self.asking,
            now_ns: self.now_ns,
            dispatch_grace_ns: engine::DISPATCH_GRACE_NS,
            review: self.review_settings().enabled,
            merge_check: None,
        })
    }

    /// The current swarm's review setting.
    pub fn review_settings(&self) -> crate::bus::swarm::review::ReviewSettings {
        self.swarm
            .as_ref()
            .and_then(|s| self.swarms.iter().find(|d| d.run.id == s.id))
            .map(|d| d.review.clone())
            .unwrap_or_else(crate::bus::swarm::review::ReviewSettings::off)
    }

    /// The selected swarm's merge gate setting (v26).
    pub fn merge_gate_settings(&self) -> crate::bus::swarm::gate::MergeGateSettings {
        self.swarm
            .as_ref()
            .and_then(|s| self.swarms.iter().find(|d| d.run.id == s.id))
            .map(|d| d.merge_gate.clone())
            .unwrap_or_default()
    }

    /// `M`: the command line, filled with the merge gate as it is; `Enter`
    /// relays `merge-gate` with what it says.
    fn open_merge_gate(&mut self) {
        match &self.swarm {
            Some(s) if s.ended_ns.is_none() => {
                let mut text = TextArea::new(self.merge_gate_settings().as_choice());
                text.end();
                self.command = Some(CommandLine {
                    text,
                    reply_to: None,
                    purpose: LinePurpose::MergeGate,
                    completion: None,
                });
            }
            Some(_) => self.status_msg = Some("This swarm is closed.".into()),
            None => self.status_msg = Some("No swarm here.".into()),
        }
    }

    /// The command the selected swarm's merges wait for, when they are
    /// gated (v26).
    pub fn merge_check(&self) -> Option<String> {
        let s = self.swarm.as_ref()?;
        self.swarms
            .iter()
            .find(|d| d.run.id == s.id)
            .and_then(|d| d.merge_gate.command_for(s))
    }

    /// How the board shows `t`.
    pub fn shown(&self, t: &BoardTask) -> Shown {
        match self.input() {
            Some(i) => engine::shown(t, &i),
            None => Shown::Draft,
        }
    }

    pub fn busy_workers(&self) -> usize {
        self.input().map(|i| engine::busy_workers(&i)).unwrap_or(0)
    }

    /// The live worker holding `t`, if any.
    pub fn worker_of(&self, t: &BoardTask) -> Option<&LiveWorker> {
        t.task
            .assigned_to
            .as_deref()
            .and_then(|l| self.live.get(l))
            .filter(|w| w.worker.state != WorkerState::Exited)
    }

    /// The coordinator's session while it is live (the planner run, until
    /// the coordinator agent).
    pub fn coordinator_session(&self) -> Option<&LiveWorker> {
        let s = self.swarm.as_ref()?;
        self.live
            .get(s.planner_launch.as_deref()?)
            .filter(|w| w.worker.state != WorkerState::Exited)
    }

    /// The planner's session, while it plans.
    pub fn planner_session(&self) -> Option<&LiveWorker> {
        let s = self.swarm.as_ref()?;
        if s.status != SwarmStatus::Planning {
            return None;
        }
        self.live.get(s.planner_launch.as_deref()?)
    }

    pub fn tab(&self) -> Tab {
        let Some(t) = self.selected_task() else {
            return Tab::Live;
        };
        match &self.chosen_tab {
            Some((id, tab)) if *id == t.task.id => *tab,
            // a sub-agent row opens on its rows in the trace (§9.5)
            _ if matches!(self.rows.get(self.cursor), Some(Row::Agent(..))) => Tab::Trace,
            _ => Tab::for_state(self.shown(t)),
        }
    }

    fn set_tab(&mut self, tab: Tab) {
        if let Some(t) = self.selected_task() {
            self.chosen_tab = Some((t.task.id.clone(), tab));
        }
        self.panel_scroll = 0;
        self.selected_message = self.list_len().saturating_sub(1);
        if tab == Tab::Changes {
            self.load_diff();
        }
        if tab == Tab::Trace {
            self.load_trace();
        }
    }

    fn after_selection_change(&mut self) {
        self.panel_scroll = 0;
        self.selected_message = self.list_len().saturating_sub(1);
        if (self.on_task && self.tab() == Tab::Changes)
            || (!self.on_task && self.swarm_tab == SwarmTab::Changes)
        {
            self.load_diff();
        }
        if self.on_task && self.tab() == Tab::Trace {
            self.load_trace();
        }
    }

    /// The Trace tab's lines for the selected worker (and, on a sub-agent
    /// row, that sub-agent's rows only), read once per selection.
    fn load_trace(&mut self) {
        let Some(launch) = self
            .selected_task()
            .and_then(|t| t.task.assigned_to.clone())
        else {
            self.trace_lines = vec!["  No worker yet: nothing was traced.".into()];
            self.trace_for = None;
            return;
        };
        let agent = self
            .rows
            .get(self.cursor)
            .copied()
            .and_then(|r| self.agent_of(r))
            .map(|a| a.id.clone());
        let want = (launch.clone(), agent.clone());
        if self.trace_for.as_ref() == Some(&want) {
            return;
        }
        self.trace_lines = match &self.conn {
            Some(conn) => {
                crate::tracing::swarm_report::launch_trace_lines(conn, &launch, agent.as_deref())
                    .unwrap_or_else(|e| vec![format!("  could not read the trace: {e}")])
            }
            None => vec!["  No trace store.".into()],
        };
        self.trace_for = Some(want);
    }

    /// The Log tab's events: the kind and text filters applied.
    pub fn log_entries(&self) -> Vec<&crate::bus::swarm::record::SwarmEvent> {
        let needle = self.log_filter.trim().to_lowercase();
        self.events
            .iter()
            .filter(|e| self.log_kind.admits(e))
            .filter(|e| {
                needle.is_empty()
                    || crate::bus::swarm::view::event_line(e)
                        .to_lowercase()
                        .contains(&needle)
            })
            .collect()
    }

    /// The Log tab has the keyboard: `↑`/`↓` choose a line.
    pub fn in_log(&self) -> bool {
        !self.on_task
            && self.swarm.is_some()
            && self.swarm_tab == SwarmTab::Log
            && self.focus == Focus::Panel
            && !self.pane_focused
    }

    /// The Timeline tab has the keyboard: `←`/`→` pan, `-`/`+` zoom.
    pub fn in_timeline(&self) -> bool {
        !self.on_task
            && self.swarm.is_some()
            && self.swarm_tab == SwarmTab::Timeline
            && self.focus == Focus::Panel
            && !self.pane_focused
    }

    /// The part of the swarm the Timeline tab shows.
    pub fn timeline_window(&self) -> Option<crate::tracing::view::Window> {
        self.timeline
            .as_ref()
            .map(|tl| tl.view(self.tl_zoom, self.tl_pan))
    }

    /// `1`–`6` on the swarm row.
    fn set_swarm_tab(&mut self, tab: SwarmTab) {
        self.swarm_tab = tab;
        self.activity = tab == SwarmTab::Activity;
        self.panel_scroll = 0;
        self.selected_message = self.list_len().saturating_sub(1);
        if tab == SwarmTab::Changes {
            self.diff_for = None;
            self.load_diff();
        }
        if tab == SwarmTab::Live {
            self.tile_cursor = self
                .tile_cursor
                .min(self.live_tiles().len().saturating_sub(1));
        }
    }

    /// The Live mosaic has the keyboard: the swarm row, its Live tab, the
    /// pane focused.
    pub fn in_mosaic(&self) -> bool {
        !self.on_task
            && self.swarm.is_some()
            && self.swarm_tab == SwarmTab::Live
            && self.focus == Focus::Panel
            && !self.pane_focused
    }

    /// The swarm's live terminals, the coordinator first, then the workers
    /// in plan order: `(launch, session index)`.
    pub fn live_tiles(&self) -> Vec<(String, usize)> {
        let Some(s) = &self.swarm else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(l) = s.planner_launch.as_deref()
            && let Some(w) = self.live.get(l)
            && w.worker.state != WorkerState::Exited
        {
            out.push((l.to_string(), w.session_idx));
        }
        for t in &self.board {
            if let Some(l) = t.task.assigned_to.as_deref()
                && let Some(w) = self.live.get(l)
                && w.worker.state != WorkerState::Exited
                && !out.iter().any(|(x, _)| x == l)
            {
                out.push((l.to_string(), w.session_idx));
            }
        }
        out
    }

    /// The tree row of a launch: the coordinator's, or its task's.
    fn row_of_launch(&self, launch: &str) -> Option<usize> {
        let si = match self.rows.get(self.cursor).copied()? {
            Row::Swarm(i) | Row::Coordinator(i) | Row::Task(i, _) | Row::Agent(i, ..) => i,
        };
        let d = self.swarms.get(si)?;
        if d.run.planner_launch.as_deref() == Some(launch) {
            return self.rows.iter().position(|r| *r == Row::Coordinator(si));
        }
        let j = d
            .board
            .iter()
            .position(|t| t.task.assigned_to.as_deref() == Some(launch))?;
        self.rows.iter().position(|r| *r == Row::Task(si, j))
    }

    /// The selected task's messages: from or to its worker, and broadcasts.
    pub fn thread(&self) -> Vec<&BusMessage> {
        let Some(launch) = self
            .selected_task()
            .and_then(|t| t.task.assigned_to.as_deref())
        else {
            return Vec::new();
        };
        self.messages
            .iter()
            .filter(|m| {
                m.from_launch == launch
                    || m.to_launch.as_deref() == Some(launch)
                    || m.to_launch.is_none()
            })
            .collect()
    }

    /// The messages the panel lists: the thread, or the whole swarm's.
    pub fn panel_messages(&self) -> Vec<&BusMessage> {
        if self.activity {
            self.messages.iter().collect()
        } else {
            self.thread()
        }
    }

    fn list_len(&self) -> usize {
        if !self.on_task && self.swarm_tab == SwarmTab::Log {
            self.log_entries().len()
        } else if self.activity || self.tab() == Tab::Thread {
            self.panel_messages().len()
        } else {
            0
        }
    }

    pub fn selected_message(&self) -> Option<&BusMessage> {
        self.panel_messages().get(self.selected_message).copied()
    }

    /// The board key a launch works on, as an `@` address.
    pub fn name_of(&self, launch: &str) -> String {
        if launch == "user" {
            return "you".into();
        }
        if let Some(t) = self
            .board
            .iter()
            .find(|t| t.task.assigned_to.as_deref() == Some(launch))
        {
            return t.ext.plan_key.clone();
        }
        crate::bus::short_id(launch).to_string()
    }

    fn load_diff(&mut self) {
        let Some(t) = self.selected_task() else {
            // the swarm's Changes: every worktree task's branch, then the
            // workspace (§9.3)
            let Some(s) = self.swarm.clone() else {
                self.diff_lines.clear();
                return;
            };
            let key = format!("swarm:{}:{}", s.id, self.board.len());
            if self.diff_for.as_deref() == Some(key.as_str()) {
                return;
            }
            let ws = PathBuf::from(&s.workspace);
            let mut lines = Vec::new();
            for t in &self.board {
                if let Some(wt) = t
                    .task
                    .worktree
                    .as_deref()
                    .map(PathBuf::from)
                    .filter(|p| p.is_dir())
                {
                    lines.push(format!("── {} ──", t.ext.plan_key));
                    lines.extend(worktree_diff(&wt, &ws));
                    lines.push(String::new());
                }
            }
            lines.push("── workspace ──".to_string());
            lines.extend(workspace_diff(&ws));
            self.diff_lines = lines;
            self.diff_for = Some(key);
            return;
        };
        let key = format!("{}:{:?}", t.task.id, t.task.worktree);
        if self.diff_for.as_deref() == Some(key.as_str()) {
            return;
        }
        let dir = t
            .task
            .worktree
            .as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir());
        self.diff_lines = match dir {
            Some(wt) => worktree_diff(&wt, &self.workspace),
            None => workspace_diff(&self.workspace),
        };
        self.diff_for = Some(key);
    }

    pub fn open_launch(&mut self) {
        // a workspace may run several swarms (schema v23)
        let planner = self
            .harnesses
            .iter()
            .position(|h| *h == self.default_harness)
            .unwrap_or(0);
        let mut pool = vec![false; self.harnesses.len().max(1)];
        let last = pool.len() - 1;
        pool[planner.min(last)] = true;
        self.modal = Some(Modal::Launch {
            goal: TextArea::new(""),
            planner,
            pool,
            pool_cursor: planner,
            pool_touched: false,
            workers: 3,
            isolation: TaskIsolation::Shared,
            gate: true,
            check: TextArea::new(""),
            merge_gate: TextArea::new(""),
            review: 0,
            field: LaunchField::Planner,
            error: None,
            advanced: false,
        });
    }

    /// The launch form back, as the operator left it, with why it did not
    /// start (the App calls this when `Outcome::Start` fails).
    pub fn launch_failed(&mut self, why: String) {
        if let Some(mut m) = self.last_launch.take() {
            if let Modal::Launch { error, .. } = &mut m {
                *error = Some(why);
            }
            self.modal = Some(m);
        }
    }

    /// The swarm started: the form's copy is no longer needed.
    pub fn launch_started(&mut self) {
        self.last_launch = None;
    }

    /// `o`: the swarm picker ([`Modal::Picker`]), the cursor on the swarm
    /// shown.
    pub fn open_picker(&mut self) {
        let Some(conn) = &self.conn else {
            self.status_msg = Some("No trace store: no swarms to list.".into());
            return;
        };
        let here = self.norm_workspace();
        let mut live = record::live_swarms(conn).unwrap_or_default();
        live.sort_by_key(|s| (s.workspace != here, std::cmp::Reverse(s.started_ns)));
        let ended = record::ended_swarms_since(conn, self.now_ns - RECENT_NS).unwrap_or_default();
        let swarms: Vec<SwarmRun> = live.into_iter().chain(ended).collect();
        if swarms.is_empty() {
            self.status_msg = Some("No swarm yet: n starts one.".into());
            return;
        }
        let cursor = swarms
            .iter()
            .position(|s| self.scope.as_deref() == Some(s.id.as_str()))
            .unwrap_or(0);
        self.modal = Some(Modal::Picker { swarms, cursor });
    }

    /// Shows swarm `id` (and puts the cursor on it at the next refresh).
    pub fn select_swarm(&mut self, id: &str) {
        self.scope = Some(id.to_string());
        self.select_next = Some(id.to_string());
        self.select_next_coordinator = false;
    }

    /// Shows swarm `id` with the cursor on its coordinator at the next
    /// refresh (a swarm just started: the operator's next keystrokes are
    /// the goal).
    pub fn select_coordinator(&mut self, id: &str) {
        self.scope = Some(id.to_string());
        self.select_next = Some(id.to_string());
        self.select_next_coordinator = true;
    }

    /// The screen on swarm `id` from the start.
    /// The launch form's review choice `i` as `ReviewSettings::parse` reads
    /// it: 0 `auto`, then each harness, then `off`.
    pub fn review_choice(&self, i: usize) -> String {
        match i {
            0 => "auto".into(),
            i if i <= self.harnesses.len() => self.harnesses[i - 1].as_str().to_string(),
            _ => "off".into(),
        }
    }

    /// How the form draws choice `i`.
    pub fn review_label(&self, i: usize) -> String {
        match self.review_choice(i).as_str() {
            "auto" => "auto · another harness than the worker's".into(),
            "off" => "off · finished tasks go to you unreviewed".into(),
            h => format!("on {h}"),
        }
    }

    pub fn scoped(mut self, id: Option<String>) -> Self {
        self.scope = id;
        self
    }

    /// A left click at (`col`, `row`): on a tree row it moves the cursor
    /// there and gives the tree the keyboard; on the pane, while it shows a
    /// session, it gives that session the keyboard (as `Enter` does). A
    /// form, a confirmation or the command line keeps the click.
    pub fn click(&mut self, col: u16, row: u16) -> Click {
        if self.modal.is_some() || self.confirm.is_some() || self.command.is_some() {
            return Click::None;
        }
        let d = self.drawn.get();
        let at = ratatui::layout::Position::new(col, row);
        if d.tree.contains(at) {
            // inside the border: one list row per line
            if row <= d.tree.y || row + 1 >= d.tree.y + d.tree.height {
                return Click::None;
            }
            let n = d.tree_start + usize::from(row - d.tree.y - 1);
            if n >= self.rows.len() {
                return Click::None;
            }
            self.pane_focused = false;
            self.focus = Focus::Board;
            self.set_cursor(n);
            return Click::Row;
        }
        if d.pane.contains(at)
            && let Some(idx) = self.pane_session()
        {
            self.pane_focused = true;
            return Click::FocusPane(idx);
        }
        Click::None
    }

    /// Where the screen opens: on `launch`'s row (the session selected when
    /// it opened: the coordinator or a worker of this swarm), else on the
    /// coordinator, where the operator talks to the swarm, so `Enter` types
    /// into its chat at once.
    pub fn open_on(&mut self, launch: Option<&str>) {
        let at = launch
            .and_then(|l| self.row_of_launch(l))
            .or_else(|| self.coordinator_row());
        if let Some(at) = at {
            self.set_cursor(at);
        }
    }

    /// The current swarm's coordinator row.
    fn coordinator_row(&self) -> Option<usize> {
        let (Row::Swarm(i) | Row::Coordinator(i) | Row::Task(i, _) | Row::Agent(i, ..)) =
            *self.rows.get(self.cursor)?;
        self.rows.iter().position(|r| *r == Row::Coordinator(i))
    }

    /// Moves the cursor to the current swarm's coordinator row now.
    fn cursor_to_coordinator(&mut self) -> bool {
        let Some(Row::Swarm(i) | Row::Coordinator(i) | Row::Task(i, _) | Row::Agent(i, ..)) =
            self.rows.get(self.cursor).copied()
        else {
            return false;
        };
        match self.rows.iter().position(|r| *r == Row::Coordinator(i)) {
            Some(at) => {
                self.cursor = at;
                true
            }
            None => false,
        }
    }

    /// `r` on a failed, lost, cancelled or stalled task: the form, with its
    /// harness, agent and isolation open; saving puts it back on the board
    /// (layout spec §4.2: a stuck agy task moves to Claude in one step).
    /// The writing agent `t` names when it runs on agy, which cannot give
    /// that agent its write and shell tools.
    fn agy_trap(&self, t: &BoardTask) -> Option<String> {
        let a = t.task.target_agent.as_deref()?;
        (t.task.target_harness.as_deref() == Some(Harness::Antigravity.as_str())
            && self.writing_agents.iter().any(|w| w == a))
        .then(|| a.to_string())
    }

    fn open_retry_form(&mut self, t: &BoardTask, stop: Option<usize>) {
        self.open_task_form(Some(t), true, stop, true);
    }

    fn open_task_form(
        &mut self,
        editing: Option<&BoardTask>,
        retry: bool,
        stop: Option<usize>,
        runs: bool,
    ) {
        let (key, title, instructions, acceptance, files, deps) = match editing {
            Some(t) => (
                t.ext.plan_key.clone(),
                t.task.title.clone(),
                t.task.instructions.clone(),
                t.ext.acceptance.clone().unwrap_or_default(),
                t.task.files.join(", "),
                t.ext.depends_on.join(", "),
            ),
            None => Default::default(),
        };
        let mut harness = editing
            .and_then(|t| t.task.target_harness.as_deref())
            .and_then(|h| self.harnesses.iter().position(|x| x.as_str() == h))
            .map_or(0, |i| i + 1);
        // a retry of agy with a writing agent would fail the same way: it
        // starts on another harness, Claude first, and says why
        let mut note = None;
        if let Some(a) = editing.and_then(|t| self.agy_trap(t)) {
            let other = self
                .harnesses
                .iter()
                .position(|h| *h == Harness::Claude)
                .or_else(|| {
                    self.harnesses
                        .iter()
                        .position(|h| *h != Harness::Antigravity)
                });
            note = Some(match other {
                Some(i) if retry => {
                    harness = i + 1;
                    format!(
                        "agy runs the `{a}` agent read-only, so it could not write: moved to {}. \
                         To keep agy, clear the agent.",
                        self.harnesses[i].as_str()
                    )
                }
                _ => format!(
                    "agy runs the `{a}` agent read-only: pick another harness or clear the agent."
                ),
            });
        }
        self.modal = Some(Modal::Task {
            editing: editing.map(|t| t.task.id.clone()),
            retry,
            stop,
            runs,
            harness,
            agent: TextArea::new(
                editing
                    .and_then(|t| t.task.target_agent.clone())
                    .unwrap_or_default(),
            ),
            isolation: editing
                .map(|t| t.task.isolation)
                .or_else(|| self.swarm.as_ref().map(|s| s.isolation))
                .unwrap_or_default(),
            key: TextArea::new(key),
            title: TextArea::new(title),
            instructions: TextArea::new(instructions),
            acceptance: TextArea::new(acceptance),
            files: TextArea::new(files),
            depends_on: TextArea::new(deps),
            field: if retry {
                TaskField::Harness
            } else if editing.is_some() {
                TaskField::Title
            } else {
                TaskField::Key
            },
            note,
        });
    }

    /// Opens the command line, prefilled.
    pub fn open_command(&mut self, prefill: &str, reply_to: Option<BusMessage>) {
        let mut text = TextArea::new(prefill);
        text.end();
        self.command = Some(CommandLine {
            text,
            reply_to,
            purpose: LinePurpose::Message,
            completion: None,
        });
    }

    fn open_reply(&mut self, m: &BusMessage) {
        let to = if m.from_launch == "user" {
            m.to_launch.clone().unwrap_or_default()
        } else {
            m.from_launch.clone()
        };
        let prefill = format!("@{} ", self.name_of(&to));
        self.modal = None;
        self.open_command(&prefill, Some(m.clone()));
    }

    /// What `Enter` would send from the command line.
    pub fn command_preview(&self) -> Option<Result<Outgoing, String>> {
        let line = self.command.as_ref()?;
        if line.purpose != LinePurpose::Message {
            return None;
        }
        Some(parse_command(&line.text.text, self, line.reply_to.as_ref()))
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(line) = &mut self.command {
            line.text.insert_str(text);
        } else if let Some(Modal::Launch {
            goal,
            field,
            check,
            merge_gate,
            ..
        }) = &mut self.modal
        {
            match field {
                LaunchField::Check => check.insert_str(text),
                LaunchField::MergeGate => merge_gate.insert_str(text),
                _ => goal.insert_str(text),
            }
        } else if let Some(Modal::Task {
            field,
            key,
            title,
            instructions,
            acceptance,
            files,
            depends_on,
            agent,
            ..
        }) = &mut self.modal
        {
            if *field == TaskField::Agent {
                agent.insert_str(text);
                return;
            }
            match field {
                TaskField::Key => key.insert_str(text),
                TaskField::Title => title.insert_str(text),
                TaskField::Instructions => instructions.insert_str(text),
                TaskField::Acceptance => acceptance.insert_str(text),
                TaskField::Files => files.insert_str(text),
                TaskField::DependsOn => depends_on.insert_str(text),
                TaskField::Agent => {}
                TaskField::Harness | TaskField::Isolation => {}
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        if self.confirm.is_some() {
            return self.handle_confirm_key(key);
        }
        if self.modal.is_some() {
            return self.handle_modal_key(key);
        }
        if self.command.is_some() {
            return self.handle_command_key(key);
        }
        self.status_msg = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('r') {
            return Outcome::Reload;
        }
        if self.log_editing {
            // `/` on the Log tab types its text filter
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.log_editing = false,
                KeyCode::Backspace => {
                    self.log_filter.pop();
                }
                KeyCode::Char(c) if !ctrl => self.log_filter.push(c),
                _ => {}
            }
            self.selected_message = self.list_len().saturating_sub(1);
            return Outcome::None;
        }
        let in_list = (self.focus == Focus::Panel)
            && (self.activity || self.in_log() || (self.on_task && self.tab() == Tab::Thread));
        if self.in_timeline() {
            match key.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    self.tl_pan -= 1;
                    return Outcome::None;
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    self.tl_pan += 1;
                    return Outcome::None;
                }
                KeyCode::Char('+') | KeyCode::Char('=') => {
                    self.tl_zoom = (self.tl_zoom * 2).min(64);
                    return Outcome::None;
                }
                KeyCode::Char('-') => {
                    self.tl_zoom = (self.tl_zoom / 2).max(1);
                    if self.tl_zoom == 1 {
                        self.tl_pan = 0;
                    }
                    return Outcome::None;
                }
                KeyCode::Char('0') => {
                    self.tl_zoom = 1;
                    self.tl_pan = 0;
                    return Outcome::None;
                }
                _ => {}
            }
        }
        if self.in_log() {
            match key.code {
                KeyCode::Char('/') => {
                    self.log_editing = true;
                    return Outcome::None;
                }
                KeyCode::Char('t') => {
                    self.log_kind = self.log_kind.next();
                    self.selected_message = self.list_len().saturating_sub(1);
                    return Outcome::None;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.focus != Focus::Board {
                    self.focus = Focus::Board;
                    Outcome::None
                } else if self.activity {
                    self.activity = false;
                    Outcome::None
                } else {
                    Outcome::Close
                }
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::Panel {
                    Focus::Board
                } else {
                    Focus::Panel
                };
                Outcome::None
            }
            KeyCode::Char(c @ '1'..='6') => {
                let n = (c as u8 - b'1') as usize;
                if self.on_task {
                    if let Some(tab) = Tab::ALL.get(n) {
                        self.activity = false;
                        self.set_tab(*tab);
                    }
                } else if let Some(tab) = SwarmTab::ALL.get(n) {
                    self.set_swarm_tab(*tab);
                }
                Outcome::None
            }
            KeyCode::Left if self.focus == Focus::Board => {
                self.fold(true);
                Outcome::None
            }
            KeyCode::Right if self.focus == Focus::Board => {
                self.fold(false);
                Outcome::None
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down if self.in_mosaic() => {
                let n = self.live_tiles().len();
                let d: isize = match key.code {
                    KeyCode::Left | KeyCode::Up => -1,
                    _ => 1,
                };
                self.tile_cursor = step(self.tile_cursor, n, d);
                Outcome::None
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                // the card scrolls a page whichever side has the keyboard
                let page = self.panel_page.get().saturating_sub(1).max(1) as isize;
                self.scroll_panel(if key.code == KeyCode::PageUp {
                    -page
                } else {
                    page
                });
                Outcome::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_cursor(-1, in_list);
                Outcome::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_cursor(1, in_list);
                Outcome::None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.move_cursor(isize::MIN / 2, in_list);
                Outcome::None
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.move_cursor(isize::MAX / 2, in_list);
                Outcome::None
            }
            KeyCode::Char('!') => {
                self.next_need();
                Outcome::None
            }
            KeyCode::Char('>') => {
                self.open_command("", None);
                Outcome::None
            }
            KeyCode::Char('o') => {
                self.open_picker();
                Outcome::None
            }
            KeyCode::Char('m') => {
                let question = self.selected_task().and_then(|t| {
                    let launch = t.task.assigned_to.as_deref()?;
                    open_questions(&self.messages)
                        .into_iter()
                        .rev()
                        .find(|q| q.from_launch == launch)
                        .cloned()
                });
                if in_list && let Some(m) = self.selected_message().cloned() {
                    self.open_reply(&m);
                } else if let Some(q) = question {
                    // a worker waiting on an answer: m answers it
                    self.open_reply(&q);
                } else if let Some(t) = self.selected_task()
                    && self.worker_of(t).is_some()
                {
                    let prefill = format!("@{} ", t.ext.plan_key);
                    self.open_command(&prefill, None);
                } else {
                    self.open_command("", None);
                }
                Outcome::None
            }
            KeyCode::Enter => self.enter(in_list),
            KeyCode::Char(' ') => match self.swarm.as_ref().map(|s| s.status) {
                Some(SwarmStatus::Running) => self.verb(SwarmVerb::Pause, None),
                Some(SwarmStatus::Paused) => self.verb(SwarmVerb::Resume, None),
                _ => {
                    self.status_msg = Some("Only a running swarm pauses.".into());
                    Outcome::None
                }
            },
            KeyCode::Char('y') => self.yes(),
            KeyCode::Char('Y') => self.force_merge(),
            KeyCode::Char('M') => {
                self.open_merge_gate();
                Outcome::None
            }
            KeyCode::Char('n') => {
                // a new swarm, from any row (coordinator-first §9.4): the plan
                // itself is changed by telling the coordinator
                self.open_launch();
                Outcome::None
            }
            KeyCode::Char('d') => {
                match &self.swarm {
                    Some(sw) => {
                        let sessions = self.live_tiles().len();
                        self.confirm = Some(Confirm::delete_of(sw, &self.board, sessions));
                    }
                    None => self.status_msg = Some("No swarm to delete.".into()),
                }
                Outcome::None
            }
            KeyCode::Char('e') | KeyCode::Char('p') | KeyCode::Char('b') | KeyCode::Char('a') => {
                self.status_msg = Some(
                    "Changes to the plan are said to the coordinator (Enter on ★). > writes to workers; 5 on the swarm row is Activity."
                        .into(),
                );
                Outcome::None
            }
            KeyCode::Char('r') => self.retry_or_launch(),
            KeyCode::Char('x') => self.x_on_selected(),
            _ => Outcome::None,
        }
    }

    fn verb(&mut self, verb: SwarmVerb, task: Option<String>) -> Outcome {
        Outcome::Verb {
            verb,
            task,
            notes: None,
        }
    }

    fn enter(&mut self, in_list: bool) -> Outcome {
        if in_list && self.in_log() {
            // Enter on a log line opens the row the event is about
            let target = self
                .log_entries()
                .get(self.selected_message)
                .map(|e| (e.task_id.clone(), e.actor.clone()));
            let Some((task_id, actor)) = target else {
                return Outcome::None;
            };
            let at = match task_id {
                Some(id) => self.rows.iter().position(|r| {
                    matches!(r, Row::Task(i, j)
                        if self.swarms.get(*i).and_then(|d| d.board.get(*j)).is_some_and(|t| t.task.id == id))
                }),
                None => actor.as_deref().and_then(|a| self.row_of_launch(a)),
            };
            match at {
                Some(at) => {
                    self.focus = Focus::Board;
                    self.set_cursor(at);
                }
                None => self.status_msg = Some("That event names no row of this swarm.".into()),
            }
            return Outcome::None;
        }
        if in_list {
            if let Some(m) = self.selected_message().cloned() {
                self.modal = Some(Modal::Message {
                    message: m,
                    scroll: 0,
                });
            }
            return Outcome::None;
        }
        if self.in_mosaic() {
            // Enter on a tile connects to that session: the cursor moves to
            // its row, where the pane is its terminal at full size (§9.5)
            let tiles = self.live_tiles();
            let Some((launch, idx)) = tiles.get(self.tile_cursor).cloned() else {
                self.status_msg = Some("No live terminal in this swarm.".into());
                return Outcome::None;
            };
            if let Some(at) = self.row_of_launch(&launch) {
                self.set_cursor(at);
            }
            self.pane_focused = true;
            return Outcome::FocusPane(idx);
        }
        match self.pane() {
            PaneView::Launch => {
                self.open_launch();
                Outcome::None
            }
            PaneView::Session(idx) => {
                self.pane_focused = true;
                Outcome::FocusPane(idx)
            }
            PaneView::TaskCard
                if self.selected_task().is_some_and(|t| {
                    t.task.assigned_to.is_some() && self.worker_of(t).is_none()
                }) =>
            {
                // its session ended: reconnect to the same conversation (§9.5)
                let t = self.selected_task().expect("checked");
                Outcome::Reconnect {
                    task_id: t.task.id.clone(),
                    launch: t.task.assigned_to.clone().unwrap_or_default(),
                }
            }
            PaneView::SwarmCard
                if matches!(self.rows.get(self.cursor), Some(Row::Swarm(_)))
                    && let Some(idx) = self.coordinator_session().map(|w| w.session_idx)
                    && let Some(at) = self.coordinator_row() =>
            {
                // the swarm is steered from its coordinator's chat: Enter
                // goes there (←/→ fold the swarm)
                self.set_cursor(at);
                self.pane_focused = true;
                Outcome::FocusPane(idx)
            }
            PaneView::SwarmCard => {
                match self.rows.get(self.cursor).copied() {
                    Some(Row::Swarm(i)) => {
                        let folded = self.is_folded(i);
                        self.fold(!folded);
                    }
                    Some(Row::Coordinator(_)) => {
                        self.status_msg = Some(
                            "The coordinator has no live session here: r starts a new one.".into(),
                        );
                    }
                    _ => {}
                }
                Outcome::None
            }
            PaneView::TaskCard => {
                // a task that has not started: nothing to type into; its
                // card is changed by telling the coordinator (§9.4)
                if self
                    .selected_task()
                    .is_some_and(|t| self.worker_of(t).is_none())
                {
                    self.status_msg = Some(
                        "No worker yet: tell the coordinator (Enter on ★) to change or start it."
                            .into(),
                    );
                } else {
                    self.set_tab(Tab::Live);
                }
                Outcome::None
            }
        }
    }

    /// `y`: approve the plan, merge a branch, accept a proposal or close a
    /// finished swarm, by what is selected and where the swarm is.
    fn yes(&mut self) -> Outcome {
        let Some(swarm) = self.swarm.clone() else {
            return Outcome::None;
        };
        if let Some(t) = self.selected_task().cloned() {
            match self.shown(&t) {
                Shown::Review => {
                    let state = crate::bus::swarm::review::state(
                        &t,
                        &self.board,
                        self.review_settings().enabled,
                    );
                    self.confirm = Some(Confirm::Merge {
                        task_id: t.task.id.clone(),
                        branch: t.task.branch.clone().unwrap_or_default(),
                        warning: crate::bus::swarm::review::merge_warning(state)
                            .map(str::to_string),
                        check: self.merge_check(),
                    });
                    return Outcome::None;
                }
                Shown::Proposed => {
                    return self.verb(SwarmVerb::Accept, Some(t.task.id.clone()));
                }
                _ => {}
            }
        }
        match swarm.status {
            SwarmStatus::Review => {
                if let Some((k, m)) = self.problems.errors.first() {
                    self.status_msg = Some(format!(
                        "fix the plan first: {}{m}",
                        if k.is_empty() {
                            String::new()
                        } else {
                            format!("{k}: ")
                        }
                    ));
                } else if swarm.gate_mode == crate::bus::swarm::record::GateMode::Chat {
                    // the screen never approves a plan (coordinator-first
                    // §5.3): the operator says go in the coordinator's chat
                    let moved = self.cursor_to_coordinator();
                    self.status_msg = Some(format!(
                        "Confirm the plan in the coordinator's chat{} — or `agent-mux swarm approve` from a shell.",
                        if moved {
                            ": Enter types into it"
                        } else {
                            " (Enter on ★)"
                        }
                    ));
                } else {
                    self.confirm = Some(Confirm::Approve {
                        tasks: self.board.len(),
                    });
                }
            }
            SwarmStatus::Finished if swarm.ended_ns.is_none() => {
                self.confirm = Some(Confirm::Close)
            }
            _ => self.status_msg = Some("Nothing to approve here.".into()),
        }
        Outcome::None
    }

    /// `Y`: merge the selected branch at once, past the swarm's check
    /// command (`force-merge`, v26). Without a check, `y` merges at once.
    fn force_merge(&mut self) -> Outcome {
        let Some(t) = self.selected_task().cloned() else {
            return Outcome::None;
        };
        if self.shown(&t) != Shown::Review {
            self.status_msg =
                Some("Y merges a finished branch past the check: choose one in review.".into());
            return Outcome::None;
        }
        let Some(check) = self.merge_check() else {
            self.status_msg = Some("This swarm's merges are not gated: y merges at once.".into());
            return Outcome::None;
        };
        let state =
            crate::bus::swarm::review::state(&t, &self.board, self.review_settings().enabled);
        self.confirm = Some(Confirm::ForceMerge {
            task_id: t.task.id.clone(),
            branch: t.task.branch.clone().unwrap_or_default(),
            check,
            warning: crate::bus::swarm::review::merge_warning(state).map(str::to_string),
        });
        Outcome::None
    }

    fn retry_or_launch(&mut self) -> Outcome {
        if let Some(Row::Coordinator(_)) = self.rows.get(self.cursor) {
            if self.coordinator_session().is_some() {
                self.status_msg =
                    Some("The coordinator is running: x stops it, then r restarts it.".into());
                return Outcome::None;
            }
            return self.verb(SwarmVerb::Restart, None);
        }
        if let Some(t) = self.selected_task() {
            let st = self.shown(t);
            if matches!(
                st,
                Shown::Failed | Shown::Lost | Shown::Cancelled | Shown::Stalled
            ) {
                // a worker still open on it is stopped when the form is saved
                let stop = self.worker_of(t).map(|w| w.session_idx);
                let t = t.clone();
                self.open_retry_form(&t, stop);
                return Outcome::None;
            }
        }
        if self.swarm.as_ref().is_none_or(|s| s.ended_ns.is_some()) {
            self.open_launch();
        } else {
            self.status_msg = Some("r retries a failed, lost or stalled task.".into());
        }
        Outcome::None
    }

    fn x_on_selected(&mut self) -> Outcome {
        if let Some(Row::Coordinator(_)) = self.rows.get(self.cursor) {
            match self.coordinator_session() {
                Some(w) => {
                    self.confirm = Some(Confirm::Stop {
                        session_idx: w.session_idx,
                        key: "the coordinator".into(),
                    })
                }
                None => {
                    self.status_msg =
                        Some("The coordinator is not running: r starts a new one.".into())
                }
            }
            return Outcome::None;
        }
        let Some(t) = self.selected_task().cloned() else {
            return Outcome::None;
        };
        let key = t.ext.plan_key.clone();
        match self.shown(&t) {
            Shown::Review => {
                self.confirm = Some(Confirm::SetAside {
                    task_id: t.task.id.clone(),
                    branch: t.task.branch.clone().unwrap_or_default(),
                })
            }
            Shown::Proposed => return self.verb(SwarmVerb::Drop, Some(t.task.id.clone())),
            _ => {
                if let Some(w) = self.worker_of(&t) {
                    self.confirm = Some(Confirm::Stop {
                        session_idx: w.session_idx,
                        key,
                    });
                } else if !t.task.status.is_terminal() {
                    self.confirm = Some(Confirm::Cancel {
                        task_id: t.task.id.clone(),
                        key,
                    });
                } else {
                    self.status_msg = Some(format!("`{key}` is already settled."));
                }
            }
        }
        Outcome::None
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> Outcome {
        let Some(c) = self.confirm.take() else {
            return Outcome::None;
        };
        if key.code != KeyCode::Char('y') {
            self.status_msg = Some("Not done.".into());
            return Outcome::None;
        }
        match c {
            Confirm::Approve { .. } => self.verb(SwarmVerb::Approve, None),
            Confirm::Merge { task_id, .. } => self.verb(SwarmVerb::Merge, Some(task_id)),
            Confirm::ForceMerge { task_id, .. } => self.verb(SwarmVerb::ForceMerge, Some(task_id)),
            Confirm::SetAside { task_id, .. } => self.verb(SwarmVerb::SetAside, Some(task_id)),
            Confirm::Close => self.verb(SwarmVerb::Close, None),
            Confirm::Cancel { task_id, .. } => Outcome::CancelTask(task_id),
            Confirm::Stop { session_idx, .. } => Outcome::StopSession(session_idx),
            Confirm::Delete { .. } => self.verb(SwarmVerb::Delete, None),
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) -> Outcome {
        let Some(line) = &mut self.command else {
            return Outcome::None;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                self.command = None;
                Outcome::None
            }
            (KeyCode::Enter, KeyModifiers::NONE) if line.purpose == LinePurpose::MergeGate => {
                let value = line.text.text.trim().to_string();
                self.command = None;
                if value.is_empty() {
                    self.status_msg =
                        Some("Say off, on (the check command) or a command of its own.".into());
                    return Outcome::None;
                }
                Outcome::Verb {
                    verb: SwarmVerb::MergeGate,
                    task: None,
                    notes: Some(value),
                }
            }
            (KeyCode::Enter, KeyModifiers::NONE) if line.purpose == LinePurpose::ReplanNotes => {
                let notes = line.text.text.trim().to_string();
                self.command = None;
                if notes.is_empty() {
                    self.status_msg = Some("Say what should change in the plan.".into());
                    return Outcome::None;
                }
                Outcome::Verb {
                    verb: SwarmVerb::Replan,
                    task: None,
                    notes: Some(notes),
                }
            }
            (KeyCode::Enter, KeyModifiers::NONE) => match self.command_preview() {
                Some(Ok(cmd)) => {
                    self.command = None;
                    Outcome::Send {
                        to_launch: match cmd.to {
                            Recipient::Everyone => None,
                            Recipient::Launch { launch, .. } => Some(launch),
                        },
                        reply_to: cmd.reply_to,
                        topic: cmd.topic,
                        body: cmd.body,
                        urgent: cmd.urgent,
                    }
                }
                Some(Err(why)) => {
                    self.status_msg = Some(why);
                    Outcome::None
                }
                None => Outcome::None,
            },
            (KeyCode::Tab, KeyModifiers::NONE) => {
                let addresses = addresses(self);
                if let Some(line) = &mut self.command {
                    complete_address(line, &addresses);
                }
                Outcome::None
            }
            _ => {
                crate::keymap::apply_text(&mut line.text, &key);
                line.completion = None;
                Outcome::None
            }
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) -> Outcome {
        if key.code == KeyCode::Esc {
            self.modal = None;
            return Outcome::None;
        }
        if let Some(Modal::Picker { swarms, cursor }) = &mut self.modal {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => *cursor = step(*cursor, swarms.len(), 1),
                KeyCode::Up | KeyCode::Char('k') => *cursor = step(*cursor, swarms.len(), -1),
                KeyCode::Home | KeyCode::Char('g') => *cursor = 0,
                KeyCode::End | KeyCode::Char('G') => *cursor = swarms.len().saturating_sub(1),
                KeyCode::Enter => {
                    let id = swarms.get(*cursor).map(|s| s.id.clone());
                    self.modal = None;
                    if let Some(id) = id {
                        self.select_coordinator(&id);
                        self.focus = Focus::Board;
                        self.pane_focused = false;
                        return Outcome::Reload;
                    }
                }
                _ => {}
            }
            return Outcome::None;
        }
        if let Some(Modal::Message { message, scroll }) = &mut self.modal {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => *scroll += 1,
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                KeyCode::Char('m') | KeyCode::Char('r') | KeyCode::Enter => {
                    let m = message.clone();
                    self.open_reply(&m);
                }
                _ => {}
            }
            return Outcome::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let multiline = matches!(
            self.modal,
            Some(Modal::Launch {
                field: LaunchField::Goal,
                ..
            }) | Some(Modal::Task {
                field: TaskField::Instructions,
                ..
            })
        );
        let on_fold = matches!(
            self.modal,
            Some(Modal::Launch {
                field: LaunchField::Advanced,
                ..
            })
        );
        if (key.code == KeyCode::Char('s') && ctrl)
            || (key.code == KeyCode::Enter && ctrl)
            || (key.code == KeyCode::Enter
                && key.modifiers == KeyModifiers::NONE
                && !multiline
                && !on_fold)
        {
            return self.submit_modal();
        }
        let n_harness = self.harnesses.len().max(1);
        match &mut self.modal {
            Some(Modal::Launch {
                goal,
                planner,
                pool,
                pool_cursor,
                pool_touched,
                workers,
                isolation,
                gate,
                check,
                merge_gate,
                review,
                field,
                advanced,
                ..
            }) => match key.code {
                KeyCode::Tab => *field = field.step(1, *advanced),
                KeyCode::BackTab => *field = field.step(-1, *advanced),
                _ => match field {
                    LaunchField::Advanced => {
                        if matches!(
                            key.code,
                            KeyCode::Enter
                                | KeyCode::Char(' ')
                                | KeyCode::Left
                                | KeyCode::Right
                                | KeyCode::Char('a')
                        ) {
                            *advanced = !*advanced;
                        }
                    }
                    LaunchField::Goal => {
                        crate::keymap::apply_text(goal, &key);
                    }
                    LaunchField::Review => {
                        let n = n_harness + 2;
                        match key.code {
                            KeyCode::Left | KeyCode::Char('h') => *review = (*review + n - 1) % n,
                            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                                *review = (*review + 1) % n
                            }
                            _ => {}
                        }
                    }
                    LaunchField::Check => {
                        crate::keymap::apply_text(check, &key);
                    }
                    LaunchField::MergeGate => {
                        crate::keymap::apply_text(merge_gate, &key);
                    }
                    LaunchField::Planner => {
                        let d = match key.code {
                            KeyCode::Left | KeyCode::Char('h') => n_harness - 1,
                            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => 1,
                            _ => 0,
                        };
                        if d > 0 {
                            *planner = (*planner + d) % n_harness;
                            // the pool follows the coordinator until ticked
                            if !*pool_touched {
                                pool.iter_mut().for_each(|t| *t = false);
                                if let Some(t) = pool.get_mut(*planner) {
                                    *t = true;
                                }
                                *pool_cursor = *planner;
                            }
                        }
                    }
                    LaunchField::WorkerHarness => match key.code {
                        KeyCode::Left | KeyCode::Char('h') => {
                            *pool_cursor = (*pool_cursor + n_harness - 1) % n_harness
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            *pool_cursor = (*pool_cursor + 1) % n_harness
                        }
                        KeyCode::Char(' ') | KeyCode::Char('x') => {
                            if let Some(t) = pool.get_mut(*pool_cursor) {
                                *t = !*t;
                                *pool_touched = true;
                            }
                        }
                        _ => {}
                    },
                    LaunchField::Workers => match key.code {
                        KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('-') => {
                            *workers = workers.saturating_sub(1).max(1)
                        }
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('+') => {
                            *workers = (*workers + 1).min(8)
                        }
                        _ => {}
                    },
                    LaunchField::Isolation => {
                        if matches!(
                            key.code,
                            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                        ) {
                            *isolation = match isolation {
                                TaskIsolation::Shared => TaskIsolation::Worktree,
                                TaskIsolation::Worktree => TaskIsolation::Shared,
                            };
                        }
                    }
                    LaunchField::Gate => {
                        if matches!(
                            key.code,
                            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                        ) {
                            *gate = !*gate;
                        }
                    }
                },
            },
            Some(Modal::Task {
                editing,
                runs,
                harness,
                agent,
                isolation,
                key: k,
                title,
                instructions,
                acceptance,
                files,
                depends_on,
                field,
                ..
            }) => {
                let with_key = editing.is_none();
                let choices = n_harness + 1; // 0: the swarm's default
                match key.code {
                    KeyCode::Tab => *field = field.step(1, with_key, *runs),
                    KeyCode::BackTab => *field = field.step(-1, with_key, *runs),
                    _ => match field {
                        TaskField::Harness => match key.code {
                            KeyCode::Left | KeyCode::Char('h') => {
                                *harness = (*harness + choices - 1) % choices
                            }
                            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                                *harness = (*harness + 1) % choices
                            }
                            _ => {}
                        },
                        TaskField::Isolation => {
                            if matches!(
                                key.code,
                                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                            ) {
                                *isolation = match isolation {
                                    TaskIsolation::Shared => TaskIsolation::Worktree,
                                    TaskIsolation::Worktree => TaskIsolation::Shared,
                                };
                            }
                        }
                        _ => {
                            crate::keymap::apply_text(
                                match field {
                                    TaskField::Key => k,
                                    TaskField::Title => title,
                                    TaskField::Instructions => instructions,
                                    TaskField::Acceptance => acceptance,
                                    TaskField::Files => files,
                                    TaskField::DependsOn => depends_on,
                                    _ => agent,
                                },
                                &key,
                            );
                        }
                    },
                }
            }
            _ => {}
        }
        Outcome::None
    }

    fn submit_modal(&mut self) -> Outcome {
        match self.modal.take() {
            Some(Modal::Launch {
                goal,
                planner,
                pool,
                pool_cursor,
                pool_touched,
                workers,
                isolation,
                gate,
                check,
                merge_gate,
                review,
                field,
                advanced,
                ..
            }) => {
                let review_s = self.review_choice(review);
                let planner_h = self
                    .harnesses
                    .get(planner)
                    .copied()
                    .unwrap_or(self.default_harness);
                // the coordinator's harness first when it is in the pool
                let mut chosen: Vec<Harness> = self
                    .harnesses
                    .iter()
                    .zip(&pool)
                    .filter(|(_, t)| **t)
                    .map(|(h, _)| *h)
                    .collect();
                if let Some(i) = chosen.iter().position(|h| *h == planner_h) {
                    chosen.swap(0, i);
                }
                // the goal is optional: without one the operator says it in
                // the coordinator's chat (coordinator-first §4.1)
                let gate_s = crate::bus::swarm::gate::MergeGateSettings::parse(&merge_gate.text);
                let check_s = Some(check.text.trim()).filter(|c| !c.is_empty());
                let why = if chosen.is_empty() {
                    Some("Tick at least one harness for the workers (Space).".to_string())
                } else {
                    gate_s.check(check_s).err()
                };
                let form = Modal::Launch {
                    goal,
                    planner,
                    pool,
                    pool_cursor,
                    pool_touched,
                    workers,
                    isolation,
                    gate,
                    check,
                    merge_gate,
                    review,
                    field,
                    error: why.clone(),
                    advanced,
                };
                if why.is_some() {
                    self.modal = Some(form);
                    return Outcome::None;
                }
                let Modal::Launch {
                    goal,
                    check,
                    merge_gate,
                    ..
                } = &form
                else {
                    unreachable!()
                };
                let out = Outcome::Start {
                    goal: goal.text.trim().to_string(),
                    planner: planner_h,
                    workers: chosen,
                    max_workers: workers,
                    isolation,
                    gate,
                    check: Some(check.text.trim().to_string()).filter(|c| !c.is_empty()),
                    review: review_s,
                    merge_gate: Some(merge_gate.text.trim().to_string()).filter(|g| !g.is_empty()),
                };
                self.last_launch = Some(form);
                out
            }
            Some(Modal::Task {
                editing,
                retry,
                stop,
                runs,
                harness,
                agent,
                isolation,
                key,
                title,
                instructions,
                acceptance,
                files,
                depends_on,
                field,
                note,
            }) => {
                let existing = editing
                    .as_ref()
                    .and_then(|id| self.board.iter().find(|t| &t.task.id == id))
                    .cloned();
                // harness, agent and isolation: as chosen when they could
                // change, else as they are
                let (h, a, iso) = if runs {
                    (
                        harness
                            .checked_sub(1)
                            .and_then(|i| self.harnesses.get(i))
                            .map(|h| h.as_str().to_string()),
                        Some(agent.text.trim().to_string()).filter(|a| !a.is_empty()),
                        Some(isolation.as_str().to_string()),
                    )
                } else {
                    (
                        existing
                            .as_ref()
                            .and_then(|t| t.task.target_harness.clone()),
                        existing.as_ref().and_then(|t| t.task.target_agent.clone()),
                        existing
                            .as_ref()
                            .map(|t| t.task.isolation.as_str().to_string()),
                    )
                };
                let planned = PlannedTask {
                    review: None,
                    harness: h,
                    agent: a,
                    isolation: iso,
                    key: match &editing {
                        Some(id) => self
                            .board
                            .iter()
                            .find(|t| &t.task.id == id)
                            .map(|t| t.ext.plan_key.clone())
                            .unwrap_or_default(),
                        None => key.text.trim().to_string(),
                    },
                    title: title.text.trim().to_string(),
                    instructions: instructions.text.trim().to_string(),
                    acceptance: Some(acceptance.text.trim().to_string()).filter(|a| !a.is_empty()),
                    files: csv(&files.text),
                    depends_on: csv(&depends_on.text),
                };
                let missing = if planned.key.is_empty() {
                    Some(TaskField::Key)
                } else if planned.title.is_empty() {
                    Some(TaskField::Title)
                } else if planned.instructions.is_empty() {
                    Some(TaskField::Instructions)
                } else {
                    None
                };
                // the harness the task would run on: its own, else the swarm's
                let runs_on = planned.harness.clone().or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|t| t.task.target_harness.clone())
                });
                let trap = (runs_on.as_deref() == Some(Harness::Antigravity.as_str()))
                    .then(|| planned.agent.clone())
                    .flatten()
                    .filter(|a| self.writing_agents.iter().any(|w| w == a));
                if let Some(a) = trap {
                    self.modal = Some(Modal::Task {
                        editing,
                        retry,
                        stop,
                        runs,
                        harness,
                        agent,
                        isolation,
                        key,
                        title,
                        instructions,
                        acceptance,
                        files,
                        depends_on,
                        field: TaskField::Harness,
                        note: Some(format!(
                            "✗ agy runs the `{a}` agent read-only (no write or shell tools): \
                             pick another harness or clear the agent."
                        )),
                    });
                    let _ = (field, note);
                    return Outcome::None;
                }
                if let Some(f) = missing {
                    self.status_msg = Some(format!("{} is required.", f.label()));
                    self.modal = Some(Modal::Task {
                        editing,
                        retry,
                        stop,
                        runs,
                        harness,
                        agent,
                        isolation,
                        key,
                        title,
                        instructions,
                        acceptance,
                        files,
                        depends_on,
                        field: f,
                        note,
                    });
                    let _ = field;
                    return Outcome::None;
                }
                // the form is the retry form (coordinator-first §9.4): the
                // plan itself is edited by telling the coordinator
                let _ = retry;
                match editing {
                    Some(task_id) => Outcome::RetryTask {
                        task_id,
                        task: planned,
                        stop,
                    },
                    None => Outcome::None,
                }
            }
            other => {
                self.modal = other;
                Outcome::None
            }
        }
    }

    /// Scrolls the card the pane shows by `delta` lines (the mouse wheel,
    /// `PgUp`/`PgDn`), whichever side has the keyboard: a list (Log,
    /// Activity, Thread) moves its selection, any other card its text,
    /// within what the last frame drew.
    pub fn scroll_panel(&mut self, delta: isize) {
        if self.pane_session().is_some() {
            return;
        }
        let list = self.activity
            || (!self.on_task && self.swarm.is_some() && self.swarm_tab == SwarmTab::Log)
            || (self.on_task && self.tab() == Tab::Thread);
        if list {
            let len = self.list_len();
            self.selected_message = step(self.selected_message, len, delta);
        } else {
            self.scroll_card(delta);
        }
    }

    /// Moves the card's text by `delta` lines, clamped to its last line.
    fn scroll_card(&mut self, delta: isize) {
        let max = self.panel_max.get().unwrap_or(usize::MAX / 4);
        self.panel_scroll = step(self.panel_scroll.min(max), max + 1, delta);
    }

    fn move_cursor(&mut self, delta: isize, in_list: bool) {
        if in_list {
            let len = self.list_len();
            self.selected_message = step(self.selected_message, len, delta);
        } else if self.focus == Focus::Panel {
            self.scroll_card(delta);
        } else {
            let at = step(self.cursor, self.rows.len(), delta);
            self.set_cursor(at);
        }
    }
}

/// What a read of the screen's rows needs: built on the UI thread, read on
/// any ([`load`]).
#[derive(Debug, Clone)]
struct LoadInput {
    scope: Option<String>,
    /// The normalized workspace the screen was opened from.
    here: String,
    /// The swarm the cursor is in (the plan's checks are read for it only).
    current: Option<String>,
    workers: HashMap<String, Worker>,
    env: Option<CheckEnv>,
    now_ns: i64,
    diff: Option<DiffJob>,
    trace: Option<TraceJob>,
}

/// The Changes tab's `git` reads for one task.
#[derive(Debug, Clone)]
struct DiffJob {
    key: String,
    worktree: Option<PathBuf>,
    workspace: PathBuf,
}

/// The Trace tab's read for one launch (and sub-agent).
#[derive(Debug, Clone)]
struct TraceJob {
    want: TraceKey,
}

/// A finished read.
#[derive(Debug)]
struct Loaded {
    /// The swarm chosen when the screen had none.
    scope: Option<String>,
    change_seq: Option<i64>,
    swarms: Vec<SwarmData>,
    diff: Option<(String, Vec<String>)>,
    trace: Option<(TraceKey, Vec<String>)>,
}

/// A trace read's subject: the worker's launch, and the sub-agent if one.
type TraceKey = (String, Option<String>);

/// A background read in flight.
struct Loading {
    generation: u64,
    rx: std::sync::mpsc::Receiver<Option<Loaded>>,
}

/// Reads the screen's swarm, its board, messages, log and timeline (and the
/// open tab's diff or trace when asked). Touches no screen state, so it runs
/// on any thread.
fn load(conn: &Connection, input: &LoadInput) -> Loaded {
    let change_seq = crate::tracing::store::latest_change_seq(conn).ok();
    // One swarm per screen (§9.1): the chosen one, else the folder's
    // newest live one, which then becomes the choice. Other swarms are
    // reached with `o` (the picker).
    let scope = input.scope.clone().or_else(|| {
        record::live_swarm(conn, &input.here)
            .ok()
            .flatten()
            .map(|s| s.id)
    });
    let runs: Vec<SwarmRun> = scope
        .as_deref()
        .and_then(|id| record::get_swarm(conn, id).ok().flatten())
        .into_iter()
        .collect();
    let now_ns = input.now_ns;
    let mut swarms = Vec::with_capacity(runs.len());
    for run in runs {
        let board = record::board(conn, &run.id).unwrap_or_default();
        let messages = crate::bus::store::list_workspace_messages(conn, &run.workspace, 500)
            .unwrap_or_default();
        let asking = open_questions(&messages)
            .into_iter()
            .map(|m| m.from_launch.clone())
            .collect();
        let problems = match &input.env {
            Some(env)
                if Some(&run.id) == input.current.as_ref()
                    && matches!(run.status, SwarmStatus::Planning | SwarmStatus::Review)
                    && !board.is_empty() =>
            {
                crate::bus::swarm::plan::check(&record::board_plan(&run, &board), env)
            }
            _ => Problems::default(),
        };
        let launch_decision = record::launch_decision(conn, &run.id);
        let agents = sub_agents(conn, &board);
        // the log and the timeline (§9.3), once per refresh
        let events = record::recent_events(conn, &run.id, 1000).unwrap_or_default();
        let timeline = crate::tracing::swarm_report::report(conn, &run, &input.workers, now_ns)
            .ok()
            .map(|r| crate::tracing::swarm_report::timeline(&r, now_ns));
        let review = crate::bus::swarm::review::settings(conn, &run.id);
        let merge_gate = crate::bus::swarm::gate::settings(conn, &run.id);
        let mut d = SwarmData {
            run,
            board,
            messages,
            asking,
            problems,
            needs: Vec::new(),
            launch_decision,
            agents,
            events,
            timeline,
            review,
            merge_gate,
        };
        d.needs = needs_for(&d, &input.workers, now_ns);
        swarms.push(d);
    }
    let diff = input.diff.as_ref().map(|j| {
        let lines = match j.worktree.as_deref().filter(|p| p.is_dir()) {
            Some(wt) => worktree_diff(wt, &j.workspace),
            None => workspace_diff(&j.workspace),
        };
        (j.key.clone(), lines)
    });
    let trace = input.trace.as_ref().map(|j| {
        let lines =
            crate::tracing::swarm_report::launch_trace_lines(conn, &j.want.0, j.want.1.as_deref())
                .unwrap_or_else(|e| vec![format!("  could not read the trace: {e}")]);
        (j.want.clone(), lines)
    });
    Loaded {
        scope,
        change_seq,
        swarms,
        diff,
        trace,
    }
}

/// Each worker's sub-agents from the store: the `agent` observations under
/// the worker's launch (a Claude `Task` run, a Codex sub-agent).
fn sub_agents(conn: &Connection, board: &[BoardTask]) -> HashMap<String, Vec<SubAgent>> {
    let mut out: HashMap<String, Vec<SubAgent>> = HashMap::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT o.id, o.trace_id, o.name, o.start_ns, o.end_ns FROM observations o
         JOIN traces t ON t.id = o.trace_id
         WHERE t.launch_id = ?1 AND o.type = 'agent' ORDER BY o.start_ns ASC LIMIT 50",
    ) else {
        return out;
    };
    for launch in board.iter().filter_map(|t| t.task.assigned_to.as_deref()) {
        let rows: Vec<SubAgent> = stmt
            .query_map([launch], |r| {
                Ok(SubAgent {
                    id: r.get(0)?,
                    trace_id: r.get(1)?,
                    name: r.get(2)?,
                    start_ns: r.get(3)?,
                    end_ns: r.get(4)?,
                })
            })
            .and_then(|it| it.collect())
            .unwrap_or_default();
        if !rows.is_empty() {
            out.insert(launch.to_string(), rows);
        }
    }
    out
}

fn step(cur: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (cur as isize)
        .saturating_add(delta)
        .clamp(0, len as isize - 1) as usize
}

fn csv(raw: &str) -> Vec<String> {
    raw.split([',', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The question a message asks, for a one-line need: its first line that is
/// not a header (workers often open with "Question regarding task-…:").
pub fn question_line(body: &str) -> String {
    let lines: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .find(|l| {
            let low = l.to_lowercase();
            !(low.starts_with("question regarding")
                || low.starts_with("question about")
                || low.starts_with("i am worker")
                || l.ends_with(':'))
        })
        .or(lines.first())
        .map(|l| l.to_string())
        .unwrap_or_default()
}

/// What needs the operator in one swarm now, most urgent first.
pub fn needs_for(d: &SwarmData, workers: &HashMap<String, Worker>, now_ns: i64) -> Vec<Need> {
    let mut out = Vec::new();
    let swarm = &d.run;
    if swarm.ended_ns.is_some() {
        return out;
    }
    let input = engine::Input {
        swarm,
        board: &d.board,
        workers,
        asking: &d.asking,
        now_ns,
        dispatch_grace_ns: engine::DISPATCH_GRACE_NS,
        review: d.review.enabled,
        merge_check: None,
    };
    let name_of = |launch: &str| -> String {
        if launch == "user" {
            return "you".into();
        }
        d.board
            .iter()
            .find(|t| t.task.assigned_to.as_deref() == Some(launch))
            .map(|t| t.ext.plan_key.clone())
            .unwrap_or_else(|| crate::bus::short_id(launch).to_string())
    };
    struct V<'a> {
        board: &'a [BoardTask],
        messages: &'a [BusMessage],
        problems: &'a Problems,
    }
    let v = V {
        board: &d.board,
        messages: &d.messages,
        problems: &d.problems,
    };
    let board_launches: HashSet<&str> = v
        .board
        .iter()
        .filter_map(|t| t.task.assigned_to.as_deref())
        .collect();
    let coordinator = swarm.planner_launch.as_deref();
    for q in open_questions(v.messages) {
        let to_operator = matches!(q.to_launch.as_deref(), None | Some("user"));
        // a question to the coordinator is the operator's only once it went
        // unanswered for STUCK_AFTER (§3.2)
        let to_coordinator = q.to_launch.is_some() && q.to_launch.as_deref() == coordinator;
        let escalated =
            to_coordinator && now_ns - q.created_ns >= crate::bus::swarm::STUCK_AFTER_NS;
        if to_coordinator && !escalated {
            continue;
        }
        if !to_operator && !escalated && !board_launches.contains(q.from_launch.as_str()) {
            continue;
        }
        let task_id = v
            .board
            .iter()
            .find(|t| t.task.assigned_to.as_deref() == Some(q.from_launch.as_str()))
            .map(|t| t.task.id.clone());
        out.push(Need {
            kind: NeedKind::Question,
            key: format!("q:{}", q.id),
            task_id,
            message: Some(q.clone()),
            text: if escalated {
                format!(
                    "{} asked the coordinator {} ago: {}",
                    name_of(&q.from_launch),
                    crate::bus::swarm::short_age(now_ns - q.created_ns),
                    question_line(&q.body)
                )
            } else {
                format!(
                    "{} asks: {}",
                    name_of(&q.from_launch),
                    question_line(&q.body)
                )
            },
        });
    }
    match swarm.status {
        SwarmStatus::Planning if now_ns - swarm.updated_ns >= PLAN_TIMEOUT_NS => {
            out.push(Need {
                kind: NeedKind::PlannerFailed,
                key: format!("planning:{}", swarm.updated_ns),
                task_id: None,
                message: None,
                text: format!(
                    "the coordinator has not submitted a plan in {}: p nudges it, x then r on ★ restarts it",
                    crate::bus::swarm::short_age(now_ns - swarm.updated_ns)
                ),
            });
        }
        SwarmStatus::Review if swarm.plan_error.is_some() && v.board.is_empty() => {
            out.push(Need {
                kind: NeedKind::PlannerFailed,
                key: format!("planner:{}", swarm.updated_ns),
                task_id: None,
                message: None,
                text: format!(
                    "the planner gave no plan: {}",
                    first_line(swarm.plan_error.as_deref().unwrap_or_default())
                ),
            });
        }
        SwarmStatus::Review => out.push(Need {
            kind: NeedKind::Plan,
            key: format!("plan:{}", swarm.updated_ns),
            task_id: None,
            message: None,
            text: format!(
                "the plan waits for your go in the coordinator's chat: {} task(s){}{}",
                v.board.len(),
                if v.problems.errors.is_empty() {
                    String::new()
                } else {
                    format!(", {} error(s) to fix first", v.problems.errors.len())
                },
                if now_ns - swarm.updated_ns >= crate::bus::swarm::STUCK_AFTER_NS {
                    format!(
                        " ({} without an answer)",
                        crate::bus::swarm::short_age(now_ns - swarm.updated_ns)
                    )
                } else {
                    String::new()
                }
            ),
        }),
        _ => {}
    }
    // agent-mux started a task itself: the coordinator did not launch it in
    // time (coordinator-first §9.2), said once per such task while recent
    for t in v.board.iter().filter(|t| {
        t.ext.launched_by == Some(crate::bus::swarm::record::LaunchedBy::Dispatcher)
            && t.task.assigned_to.is_some()
            && !t.task.status.is_terminal()
            && now_ns - t.task.updated_ns < 10 * 60 * 1_000_000_000
    }) {
        out.push(Need {
            kind: NeedKind::CoordinatorQuiet,
            key: format!("quiet:{}", t.task.id),
            task_id: Some(t.task.id.clone()),
            message: None,
            text: format!(
                "agent-mux started `{}` itself: the coordinator did not launch it in time",
                t.ext.plan_key
            ),
        });
    }
    for t in v.board {
        let st = engine::shown(t, &input);
        let key = &t.ext.plan_key;
        let (kind, text) = match st {
            Shown::Prompt => (
                NeedKind::Prompt,
                format!("{key} waits on a prompt in its terminal"),
            ),
            Shown::Failed => (
                NeedKind::Failed,
                format!(
                    "{key} failed: {}",
                    first_line(t.task.error.as_deref().unwrap_or("no reason given"))
                ),
            ),
            Shown::Lost => (
                NeedKind::Lost,
                format!("{key}: its session exited mid-task"),
            ),
            Shown::Stalled if t.ext.nudged_ns.is_some() => (
                NeedKind::Stalled,
                format!("{key} stopped without finishing, and was reminded"),
            ),
            Shown::Review => (
                NeedKind::Review,
                format!(
                    "{key} finished on {}: merge it or set it aside",
                    t.task.branch.as_deref().unwrap_or("its branch")
                ),
            ),
            Shown::Proposed => (
                NeedKind::Proposed,
                format!("a worker proposes {key}: {}", t.task.title),
            ),
            _ => continue,
        };
        out.push(Need {
            kind,
            key: format!("{}:{}:{}", kind.label(), t.task.id, t.task.updated_ns),
            task_id: Some(t.task.id.clone()),
            message: None,
            text,
        });
    }
    if let Some(cmd) = &swarm.check_cmd
        && let Some(r) = swarm
            .check_result
            .as_deref()
            .and_then(crate::bus::swarm::check::CheckResult::parse)
        && !r.running
        && !r.passed
    {
        out.push(Need {
            kind: NeedKind::CheckFailed,
            key: format!("check:{}", r.started_ns),
            task_id: None,
            message: None,
            text: format!(
                "{} · {}",
                r.summary(cmd),
                r.tail.lines().last().unwrap_or_default().trim()
            ),
        });
    }
    if swarm.status == SwarmStatus::Finished {
        let done = v
            .board
            .iter()
            .filter(|t| t.task.status == TaskStatus::Done)
            .count();
        out.push(Need {
            kind: NeedKind::Finished,
            key: "finished".into(),
            task_id: None,
            message: None,
            text: format!(
                "finished: {done}/{} done · close it when you're ready",
                v.board.len()
            ),
        });
    }
    out.sort_by_key(|n| n.kind);
    out
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().trim().to_string()
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// What `@` can name: `all`, and the key of every task with a live worker.
fn addresses(v: &SwarmBoard) -> Vec<String> {
    let mut out = vec!["all".to_string()];
    out.extend(
        v.board
            .iter()
            .filter(|t| v.worker_of(t).is_some())
            .map(|t| t.ext.plan_key.clone()),
    );
    out
}

/// Reads a command line: leading `!` (urgent), `@who` and `#topic` in any
/// order, then the body. `@who` is `all` or a task key (prefix, separators
/// ignored) whose worker is live, or a launch id; with none, the reply's
/// sender, else the selected task's worker. The topic, unless named:
/// `urgent` for `!` (the hook delivers it mid-turn), `answer` for a reply to
/// a question, else `directive`.
pub fn parse_command(
    text: &str,
    v: &SwarmBoard,
    reply_to: Option<&BusMessage>,
) -> Result<Outgoing, String> {
    let mut rest = text.trim_start();
    let mut urgent = false;
    let mut who: Option<&str> = None;
    let mut topic: Option<&str> = None;
    loop {
        if let Some(r) = rest.strip_prefix('!') {
            urgent = true;
            rest = r.trim_start();
            continue;
        }
        let token_end = |r: &str| r.find(char::is_whitespace).unwrap_or(r.len());
        if let Some(r) = rest.strip_prefix('@') {
            if who.is_some() {
                return Err("one recipient per message: @all writes to everyone".into());
            }
            let end = token_end(r);
            who = Some(&r[..end]);
            rest = r[end..].trim_start();
            continue;
        }
        if let Some(r) = rest.strip_prefix('#') {
            let end = token_end(r);
            topic = Some(&r[..end]).filter(|t| !t.is_empty());
            rest = r[end..].trim_start();
            continue;
        }
        break;
    }
    let body = rest.trim().to_string();
    let live_tasks: Vec<&BoardTask> = v
        .board
        .iter()
        .filter(|t| v.worker_of(t).is_some())
        .collect();
    let launch_of = |t: &BoardTask| t.task.assigned_to.clone().unwrap_or_default();
    let to = match who {
        Some(w) => {
            let key = squash(w);
            if key.is_empty() {
                return Err("@ names someone: @all or @<task key>".into());
            }
            if matches!(key.as_str(), "all" | "everyone") {
                Recipient::Everyone
            } else if let Some(t) = live_tasks.iter().find(|t| squash(&t.ext.plan_key) == key) {
                Recipient::Launch {
                    launch: launch_of(t),
                    name: t.ext.plan_key.clone(),
                }
            } else {
                let hits: Vec<&&BoardTask> = live_tasks
                    .iter()
                    .filter(|t| {
                        squash(&t.ext.plan_key).starts_with(&key) || launch_of(t).starts_with(w)
                    })
                    .collect();
                match hits.as_slice() {
                    [one] => Recipient::Launch {
                        launch: launch_of(one),
                        name: one.ext.plan_key.clone(),
                    },
                    [] => match reply_to {
                        Some(m) if m.from_launch.starts_with(w) => Recipient::Launch {
                            launch: m.from_launch.clone(),
                            name: v.name_of(&m.from_launch),
                        },
                        _ => return Err(format!("no live worker is called @{w}")),
                    },
                    many => {
                        return Err(format!(
                            "@{w} could be {}",
                            many.iter()
                                .map(|t| t.ext.plan_key.as_str())
                                .collect::<Vec<_>>()
                                .join(" or ")
                        ));
                    }
                }
            }
        }
        None => match (
            reply_to,
            v.selected_task().filter(|t| v.worker_of(t).is_some()),
        ) {
            (Some(m), _) => Recipient::Launch {
                launch: m.from_launch.clone(),
                name: v.name_of(&m.from_launch),
            },
            (None, Some(t)) => Recipient::Launch {
                launch: launch_of(t),
                name: t.ext.plan_key.clone(),
            },
            _ => return Err("say who: @all or @<task key>".into()),
        },
    };
    if body.is_empty() {
        return Err("write the message after the address".into());
    }
    let reply_to = reply_to.filter(|m| match &to {
        Recipient::Launch { launch, .. } => *launch == m.from_launch,
        Recipient::Everyone => m.to_launch.is_none(),
    });
    let topic = if urgent {
        "urgent".to_string()
    } else if let Some(t) = topic {
        t.to_string()
    } else if reply_to.is_some_and(|m| m.topic == "question") {
        "answer".to_string()
    } else {
        "directive".to_string()
    };
    Ok(Outgoing {
        to,
        urgent,
        topic,
        body,
        reply_to: reply_to.map(|m| m.id.clone()),
    })
}

fn complete_address(line: &mut CommandLine, addresses: &[String]) {
    let text = line.text.text.clone();
    let cursor = line.text.cursor();
    let end = if line.completion.is_some() && text[..cursor].ends_with(' ') {
        cursor - 1
    } else {
        cursor
    };
    let start = text[..end].rfind(char::is_whitespace).map_or(0, |i| i + 1);
    let word = &text[start..end];
    let Some(at) = word
        .find('@')
        .filter(|&i| word[..i].chars().all(|c| c == '!'))
    else {
        return;
    };
    let at = start + at;
    let (prefix, next) = match &line.completion {
        Some((p, i)) => (p.clone(), i + 1),
        None => (squash(&text[at + 1..end]), 0),
    };
    let candidates: Vec<&String> = addresses
        .iter()
        .filter(|n| squash(n).starts_with(&prefix))
        .collect();
    if candidates.is_empty() {
        return;
    }
    let index = next % candidates.len();
    let mut new = format!("{}@{} ", &text[..at], candidates[index]);
    let caret = new.len();
    new.push_str(text[cursor..].trim_start());
    line.text.set(new);
    line.text.set_cursor(caret);
    line.completion = Some((prefix, index));
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A shared task's changes: the workspace's uncommitted diff.
fn workspace_diff(dir: &Path) -> Vec<String> {
    let mut lines = vec![format!("Workspace: {}", dir.display())];
    if let Some(s) = git(dir, &["status", "--short"]) {
        lines.extend(s.lines().take(30).map(str::to_string));
    }
    lines.push(String::new());
    match git(dir, &["diff", "--no-color"]) {
        Some(d) if d.trim().is_empty() => lines.push("(no uncommitted changes)".into()),
        Some(d) => lines.extend(d.lines().take(400).map(str::to_string)),
        None => lines.push("(not a git repository)".into()),
    }
    lines
}

/// A worktree task's changes: its commits since it branched from the
/// workspace's branch, and what it has not committed.
fn worktree_diff(wt: &Path, workspace: &Path) -> Vec<String> {
    let mut lines = vec![format!("Worktree: {}", wt.display())];
    let branch = crate::loops::worktree::base_branch(workspace);
    let base = git(wt, &["merge-base", "HEAD", &branch]).map(|s| s.trim().to_string());
    if let Some(base) = &base {
        if let Some(log) = git(wt, &["log", "--oneline", &format!("{base}..HEAD")]) {
            lines.push(format!("Commits ({}):", log.lines().count()));
            lines.extend(log.lines().take(20).map(|l| format!("  {l}")));
        }
        if let Some(stat) = git(wt, &["diff", "--shortstat", base]) {
            lines.push(stat.trim().to_string());
        }
        lines.push(String::new());
        if let Some(d) = git(wt, &["diff", "--no-color", base]) {
            if d.trim().is_empty() {
                lines.push("(no changes yet)".into());
            }
            lines.extend(d.lines().take(400).map(str::to_string));
        }
    } else {
        lines.extend(workspace_diff(wt).into_iter().skip(1));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::swarm::record::SwarmTaskExt;
    use crate::bus::{BusTask, TaskIsolation};

    fn swarm(status: SwarmStatus) -> SwarmRun {
        SwarmRun {
            id: "swarm-1".into(),
            workspace: "/ws".into(),
            goal: "ship".into(),
            status,
            planner_harness: "claude".into(),
            planner_launch: None,
            plan_summary: Some("why".into()),
            plan_error: None,
            max_workers: 2,
            isolation: TaskIsolation::Shared,
            gate: true,
            gate_mode: crate::bus::swarm::record::GateMode::Chat,
            headless: false,
            check_cmd: None,
            check_result: None,
            owner_run: None,
            started_ns: 0,
            updated_ns: 1,
            ended_ns: None,
        }
    }

    fn task(key: &str, status: TaskStatus, launch: Option<&str>) -> BoardTask {
        BoardTask {
            task: BusTask {
                id: format!("task-{key}"),
                workspace: "/ws".into(),
                created_by: "swarm:swarm-1".into(),
                parent_task_id: None,
                assigned_to: launch.map(str::to_string),
                target_harness: Some("claude".into()),
                target_agent: None,
                title: format!("Title {key}"),
                instructions: "x".into(),
                files: vec![],
                isolation: TaskIsolation::Shared,
                status,
                result: None,
                error: (status == TaskStatus::Failed).then(|| "tests failed\nmore".into()),
                worktree: None,
                branch: None,
                created_ns: 0,
                updated_ns: 0,
                ended_ns: None,
            },
            ext: SwarmTaskExt {
                task_id: format!("task-{key}"),
                plan_key: key.into(),
                attempts: launch.is_some() as i64,
                ..Default::default()
            },
            review: None,
        }
    }

    fn board(status: SwarmStatus, tasks: Vec<BoardTask>, live: &[(&str, &str)]) -> SwarmBoard {
        let mut v = SwarmBoard::open(
            PathBuf::from("/nonexistent-ws"),
            None,
            &[],
            None,
            vec![Harness::Claude, Harness::Codex],
            Harness::Claude,
            0,
        );
        for (i, (launch, state)) in live.iter().enumerate() {
            let w = LiveWorker {
                session_idx: i,
                worker: Worker {
                    state: worker_state(state),
                    quiet_ns: 0,
                },
                provider: "claude".into(),
                tokens: 0,
                cost_usd: 0.0,
                priced: false,
                running_tool: None,
            };
            v.workers.insert(launch.to_string(), w.worker);
            v.live.insert(launch.to_string(), w);
        }
        set_data(&mut v, swarm(status), tasks, vec![]);
        // the cursor on the first task, as after ↓ from the swarm row
        v.set_cursor(1.min(v.rows.len().saturating_sub(1)));
        v
    }

    /// Puts one swarm in the tree, as a refresh would.
    fn set_data(
        v: &mut SwarmBoard,
        run: SwarmRun,
        board: Vec<BoardTask>,
        messages: Vec<BusMessage>,
    ) {
        let asking = open_questions(&messages)
            .into_iter()
            .map(|m| m.from_launch.clone())
            .collect();
        let mut d = SwarmData {
            run,
            board,
            messages,
            asking,
            problems: v.problems.clone(),
            needs: Vec::new(),
            launch_decision: None,
            agents: HashMap::new(),
            events: Vec::new(),
            timeline: None,
            review: crate::bus::swarm::review::ReviewSettings::off(),
            merge_gate: Default::default(),
        };
        d.needs = needs_for(&d, &v.workers, v.now_ns);
        v.swarms = vec![d];
        v.rebuild_rows();
        v.cursor = v.cursor.min(v.rows.len().saturating_sub(1));
        v.sync_current();
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn msg(id: &str, from: &str, to: Option<&str>, topic: &str, body: &str, at: i64) -> BusMessage {
        BusMessage {
            id: id.into(),
            workspace: "/ws".into(),
            from_launch: from.into(),
            to_launch: to.map(str::to_string),
            topic: topic.into(),
            body: body.into(),
            reply_to: None,
            created_ns: at,
            delivered_ns: None,
            acked_ns: None,
        }
    }

    #[test]
    fn the_needs_are_what_only_the_operator_can_resolve_most_urgent_first() {
        let mut v = board(
            SwarmStatus::Running,
            vec![
                task("a", TaskStatus::Failed, None),
                task("b", TaskStatus::Running, Some("L2")),
                task("c", TaskStatus::Running, Some("gone")),
                task("d", TaskStatus::Running, Some("L4")),
            ],
            &[("L2", "working"), ("L4", "attention")],
        );
        let (run, board) = (v.swarm.clone().unwrap(), v.board.clone());
        set_data(
            &mut v,
            run,
            board,
            vec![msg(
                "m1",
                "L2",
                Some("user"),
                "question",
                "Question regarding task-b (b):\n\nWhich table?\nmore",
                5,
            )],
        );
        let got: Vec<(NeedKind, &str)> =
            v.needs.iter().map(|n| (n.kind, n.text.as_str())).collect();
        assert_eq!(
            got,
            [
                (NeedKind::Question, "b asks: Which table?"),
                (NeedKind::Prompt, "d waits on a prompt in its terminal"),
                (NeedKind::Failed, "a failed: tests failed"),
                (NeedKind::Lost, "c: its session exited mid-task"),
            ]
        );
        // Badges: each task that needs you, and the swarm's count.
        let badges: Vec<usize> = v.rows.iter().map(|r| v.row_needs(*r)).collect();
        assert_eq!(badges, [4, 1, 1, 1, 1], "swarm, a, b, c, d");
        // `!` walks the rows that need you; m on an asking task answers it.
        v.set_cursor(0);
        v.handle_key(key(KeyCode::Char('!')));
        assert_eq!(
            v.selected_task().map(|t| t.ext.plan_key.as_str()),
            Some("a")
        );
        v.handle_key(key(KeyCode::Char('!')));
        assert_eq!(
            v.selected_task().map(|t| t.ext.plan_key.as_str()),
            Some("b")
        );
        v.handle_key(key(KeyCode::Char('m')));
        let line = v.command.as_ref().unwrap();
        assert_eq!(line.text.text, "@b ");
        assert_eq!(line.reply_to.as_ref().map(|m| m.id.as_str()), Some("m1"));
    }

    #[test]
    fn the_tree_lists_swarm_coordinator_and_tasks_and_folds() {
        let mut v = board(
            SwarmStatus::Running,
            vec![
                task("a", TaskStatus::Running, Some("L1")),
                task("b", TaskStatus::Open, None),
            ],
            &[("L1", "working"), ("P", "idle")],
        );
        let mut run = v.swarm.clone().unwrap();
        run.planner_launch = Some("P".into());
        let board_tasks = v.board.clone();
        set_data(&mut v, run, board_tasks, vec![]);
        assert_eq!(
            v.rows,
            [
                Row::Swarm(0),
                Row::Coordinator(0),
                Row::Task(0, 0),
                Row::Task(0, 1)
            ]
        );
        // The pane: the swarm's card, the coordinator's terminal, a live
        // worker's terminal, a task card.
        let panes: Vec<PaneView> = (0..4)
            .map(|i| {
                v.set_cursor(i);
                v.pane()
            })
            .collect();
        assert_eq!(
            panes,
            [
                PaneView::SwarmCard,
                PaneView::Session(1),
                PaneView::Session(0),
                PaneView::TaskCard
            ]
        );
        // Enter on a terminal gives it the keyboard.
        v.set_cursor(2);
        assert_eq!(v.handle_key(key(KeyCode::Enter)), Outcome::FocusPane(0));
        assert!(v.pane_focused);
        // ← climbs to the swarm, ← again folds it, → unfolds.
        v.pane_focused = false;
        v.handle_key(key(KeyCode::Left));
        assert_eq!(v.cursor, 0);
        v.handle_key(key(KeyCode::Left));
        assert_eq!(v.rows, [Row::Swarm(0)]);
        v.handle_key(key(KeyCode::Right));
        assert_eq!(v.rows.len(), 4);
    }

    #[test]
    fn a_question_to_the_coordinator_is_yours_only_after_five_minutes() {
        let mut v = board(
            SwarmStatus::Running,
            vec![task("a", TaskStatus::Running, Some("L1"))],
            &[("L1", "idle"), ("C", "idle")],
        );
        let mut run = v.swarm.clone().unwrap();
        run.planner_launch = Some("C".into());
        let tasks = v.board.clone();
        let q = msg("q1", "L1", Some("C"), "question", "Which table?", 0);
        v.now_ns = 60 * 1_000_000_000;
        set_data(&mut v, run.clone(), tasks.clone(), vec![q.clone()]);
        assert!(
            v.needs.iter().all(|n| n.kind != NeedKind::Question),
            "the coordinator has it: {:?}",
            v.needs
        );
        v.now_ns = 6 * 60 * 1_000_000_000;
        set_data(&mut v, run, tasks, vec![q]);
        let n = v
            .needs
            .iter()
            .find(|n| n.kind == NeedKind::Question)
            .unwrap();
        assert_eq!(n.text, "a asked the coordinator 6m ago: Which table?");
    }

    #[test]
    fn a_question_line_skips_the_header() {
        assert_eq!(
            question_line("Question regarding task-x (k):\n\nI am worker abc.\nShould it be v2?"),
            "Should it be v2?"
        );
        assert_eq!(question_line("Plain question?"), "Plain question?");
        assert_eq!(
            question_line("Context:"),
            "Context:",
            "nothing better: the first line"
        );
    }

    #[test]
    fn y_never_approves_a_plan_from_the_screen_and_refuses_one_with_errors() {
        let mut v = board(
            SwarmStatus::Review,
            vec![task("a", TaskStatus::Open, None)],
            &[],
        );
        assert_eq!(v.needs[0].kind, NeedKind::Plan);
        // The gate is the chat: y says so and nothing is confirmed.
        assert_eq!(v.handle_key(key(KeyCode::Char('y'))), Outcome::None);
        assert_eq!(v.confirm, None);
        assert!(
            v.status_msg
                .as_deref()
                .is_some_and(|m| m.contains("Confirm the plan in the coordinator's chat")),
            "{:?}",
            v.status_msg
        );
        v.problems
            .errors
            .push(("a".into(), "no instructions".into()));
        v.handle_key(key(KeyCode::Char('y')));
        assert_eq!(v.confirm, None);
        assert_eq!(
            v.status_msg.as_deref(),
            Some("fix the plan first: a: no instructions")
        );
    }

    #[test]
    fn the_plan_is_changed_in_the_chat_not_with_keys() {
        let mut v = board(
            SwarmStatus::Review,
            vec![task("a", TaskStatus::Open, None)],
            &[],
        );
        for c in ['p', 'e', 'b', 'a'] {
            assert_eq!(v.handle_key(key(KeyCode::Char(c))), Outcome::None);
            assert!(!v.text_entry_active(), "{c}");
            assert!(
                v.status_msg
                    .as_deref()
                    .is_some_and(|m| m.contains("said to the coordinator")),
                "{c}: {:?}",
                v.status_msg
            );
        }
        // d asks to delete the whole swarm, on any row; only y does it
        v.handle_key(key(KeyCode::Down));
        assert_eq!(v.handle_key(key(KeyCode::Char('d'))), Outcome::None);
        assert!(matches!(
            v.confirm,
            Some(Confirm::Delete { live: true, .. })
        ));
        assert_eq!(
            v.handle_key(key(KeyCode::Char('y'))),
            Outcome::Verb {
                verb: SwarmVerb::Delete,
                task: None,
                notes: None
            }
        );
        // n is always a new swarm, even inside one
        v.handle_key(key(KeyCode::Char('n')));
        assert!(matches!(v.modal, Some(Modal::Launch { .. })));
    }

    #[test]
    fn the_panel_opens_on_what_the_state_calls_for_and_a_chosen_tab_sticks() {
        let mut v = board(
            SwarmStatus::Running,
            vec![
                task("a", TaskStatus::Running, Some("L1")),
                task("b", TaskStatus::Open, None),
            ],
            &[("L1", "working")],
        );
        assert_eq!(v.tab(), Tab::Live);
        v.handle_key(key(KeyCode::Char('3')));
        assert_eq!(v.tab(), Tab::Thread);
        v.handle_key(key(KeyCode::Down));
        assert_eq!(v.tab(), Tab::Task, "another task opens on its own tab");
        v.handle_key(key(KeyCode::Up));
        assert_eq!(v.tab(), Tab::Thread, "the chosen tab stuck to `a`");
        // Enter on a card brings the live terminal, a second Enter types into it.
        assert_eq!(v.handle_key(key(KeyCode::Enter)), Outcome::None);
        assert_eq!(v.tab(), Tab::Live);
        assert_eq!(v.handle_key(key(KeyCode::Enter)), Outcome::FocusPane(0));
    }

    #[test]
    fn the_command_line_addresses_workers_by_task_key() {
        let mut v = board(
            SwarmStatus::Running,
            vec![
                task("store-queries", TaskStatus::Running, Some("L1")),
                task("store-schema", TaskStatus::Running, Some("L2")),
                task("docs", TaskStatus::Open, None),
            ],
            &[("L1", "working"), ("L2", "idle")],
        );
        let to = |v: &SwarmBoard, s: &str| parse_command(s, v, None);
        assert_eq!(
            to(&v, "@store-q use the index").unwrap().to,
            Recipient::Launch {
                launch: "L1".into(),
                name: "store-queries".into()
            }
        );
        assert_eq!(
            to(&v, "@store x").unwrap_err(),
            "@store could be store-queries or store-schema"
        );
        assert_eq!(
            to(&v, "@docs x").unwrap_err(),
            "no live worker is called @docs"
        );
        let all = to(&v, "!@all stop").unwrap();
        assert_eq!(
            (all.to, all.topic.as_str()),
            (Recipient::Everyone, "urgent")
        );
        // No address: the selected task's worker.
        assert_eq!(
            to(&v, "hello").unwrap().to,
            Recipient::Launch {
                launch: "L1".into(),
                name: "store-queries".into()
            }
        );
        // A reply to a question is an answer to its asker.
        let q = msg("q1", "L2", Some("user"), "question", "?", 1);
        let a = parse_command("yes", &v, Some(&q)).unwrap();
        assert_eq!(
            (a.topic.as_str(), a.reply_to.as_deref()),
            ("answer", Some("q1"))
        );
        // Tab completes and cycles task keys.
        v.open_command("@st", None);
        v.handle_key(key(KeyCode::Tab));
        assert_eq!(v.command.as_ref().unwrap().text.text, "@store-queries ");
        v.handle_key(key(KeyCode::Tab));
        assert_eq!(v.command.as_ref().unwrap().text.text, "@store-schema ");
    }

    #[test]
    fn shift_y_merges_a_branch_past_the_gate_and_y_queues_it() {
        let mut wt = task("a", TaskStatus::Done, Some("L1"));
        wt.task.branch = Some("loop/bus-a".into());
        let tasks = vec![wt.clone()];
        // without a check: Y says y merges at once
        let mut v = board(SwarmStatus::Integrating, tasks.clone(), &[("L1", "idle")]);
        assert_eq!(v.handle_key(key(KeyCode::Char('Y'))), Outcome::None);
        assert_eq!(v.confirm, None);
        assert_eq!(
            v.status_msg.as_deref(),
            Some("This swarm's merges are not gated: y merges at once.")
        );
        // a check command alone does not gate the merges
        let mut run = swarm(SwarmStatus::Integrating);
        run.check_cmd = Some("make test".into());
        set_data(&mut v, run, tasks, vec![]);
        v.set_cursor(1);
        assert_eq!(v.merge_check(), None);
        // the gate on: y queues it behind the check, Y merges now after a y
        v.swarms[0].merge_gate = crate::bus::swarm::gate::MergeGateSettings::parse("on");
        v.handle_key(key(KeyCode::Char('y')));
        let Some(c) = v.confirm.clone() else {
            panic!("y asks first")
        };
        assert!(
            c.question()
                .starts_with("Queue loop/bus-a for its merge? `make test` runs"),
            "{}",
            c.question()
        );
        v.handle_key(key(KeyCode::Esc));
        v.handle_key(key(KeyCode::Char('Y')));
        assert_eq!(
            v.confirm,
            Some(Confirm::ForceMerge {
                task_id: "task-a".into(),
                branch: "loop/bus-a".into(),
                check: "make test".into(),
                warning: None,
            })
        );
        assert_eq!(
            v.confirm.as_ref().unwrap().question(),
            "Merge loop/bus-a now, without running `make test` on it first (a merge commit)?"
        );
        assert_eq!(
            v.handle_key(key(KeyCode::Char('y'))),
            Outcome::Verb {
                verb: SwarmVerb::ForceMerge,
                task: Some("task-a".into()),
                notes: None
            }
        );
        // only a branch in review
        let mut v = board(
            SwarmStatus::Running,
            vec![task("b", TaskStatus::Running, Some("L1"))],
            &[("L1", "working")],
        );
        v.handle_key(key(KeyCode::Char('Y')));
        assert_eq!(v.confirm, None);
    }

    #[test]
    fn shift_m_changes_the_merge_gate_from_the_command_line() {
        let mut v = board(
            SwarmStatus::Running,
            vec![task("a", TaskStatus::Running, Some("L1"))],
            &[("L1", "working")],
        );
        // filled with the gate as it is
        v.handle_key(key(KeyCode::Char('M')));
        let line = v.command.as_ref().expect("M opens the command line");
        assert_eq!(
            (line.purpose, line.text.text.as_str()),
            (LinePurpose::MergeGate, "off")
        );
        assert!(v.command_preview().is_none(), "not a message");
        // typing replaces it; Enter relays merge-gate with the value
        for _ in 0..3 {
            v.handle_key(key(KeyCode::Backspace));
        }
        for c in "cargo test --lib".chars() {
            v.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            v.handle_key(key(KeyCode::Enter)),
            Outcome::Verb {
                verb: SwarmVerb::MergeGate,
                task: None,
                notes: Some("cargo test --lib".into())
            }
        );
        assert!(v.command.is_none());
        // an empty line says what it takes, and changes nothing
        v.swarms[0].merge_gate = crate::bus::swarm::gate::MergeGateSettings::parse("on");
        v.handle_key(key(KeyCode::Char('M')));
        assert_eq!(v.command.as_ref().unwrap().text.text, "on");
        v.handle_key(key(KeyCode::Backspace));
        v.handle_key(key(KeyCode::Backspace));
        assert_eq!(v.handle_key(key(KeyCode::Enter)), Outcome::None);
        assert!(v.status_msg.as_deref().unwrap().starts_with("Say off, on"));
    }

    #[test]
    fn x_and_y_act_on_a_finished_branch_and_r_retries_a_failure() {
        let mut wt = task("a", TaskStatus::Done, Some("L1"));
        wt.task.branch = Some("loop/bus-a".into());
        let mut v = board(
            SwarmStatus::Integrating,
            vec![wt, task("f", TaskStatus::Failed, None)],
            &[("L1", "idle")],
        );
        v.handle_key(key(KeyCode::Char('y')));
        assert_eq!(
            v.confirm,
            Some(Confirm::Merge {
                task_id: "task-a".into(),
                branch: "loop/bus-a".into(),
                warning: None,
                check: None,
            })
        );
        assert_eq!(
            v.handle_key(key(KeyCode::Char('y'))),
            Outcome::Verb {
                verb: SwarmVerb::Merge,
                task: Some("task-a".into()),
                notes: None
            }
        );
        v.handle_key(key(KeyCode::Char('x')));
        assert!(matches!(v.confirm, Some(Confirm::SetAside { .. })));
        v.handle_key(key(KeyCode::Esc));
        v.handle_key(key(KeyCode::Down));
        // r opens the retry form on its harness (claude, as the task names):
        // → moves it to codex, and Ctrl+S puts it back on the board.
        assert_eq!(v.handle_key(key(KeyCode::Char('r'))), Outcome::None);
        assert!(matches!(
            v.modal,
            Some(Modal::Task {
                retry: true,
                runs: true,
                field: TaskField::Harness,
                harness: 1,
                ..
            })
        ));
        v.handle_key(key(KeyCode::Right));
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        match v.handle_key(ctrl_s) {
            Outcome::RetryTask {
                task_id,
                task,
                stop,
            } => {
                assert_eq!(task_id, "task-f");
                assert_eq!(task.harness.as_deref(), Some("codex"));
                assert_eq!(stop, None, "no worker left to stop");
            }
            other => panic!("expected RetryTask, got {other:?}"),
        }
    }

    #[test]
    fn with_no_swarm_enter_opens_the_launch_form() {
        let mut v = SwarmBoard::open(
            PathBuf::from("/nonexistent-ws"),
            None,
            &[],
            None,
            vec![Harness::Claude, Harness::Codex],
            Harness::Codex,
            0,
        );
        v.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            v.modal,
            Some(Modal::Launch {
                planner: 1,
                field: LaunchField::Planner,
                advanced: false,
                ..
            })
        ));
        // One field: Enter starts the coordinator; the goal is said in its chat.
        assert_eq!(
            v.handle_key(key(KeyCode::Enter)),
            Outcome::Start {
                goal: String::new(),
                planner: Harness::Codex,
                workers: vec![Harness::Codex],
                max_workers: 3,
                isolation: TaskIsolation::Shared,
                gate: true,
                check: None,
                review: "auto".into(),
                merge_gate: None,
            }
        );
        // Advanced unfolds the goal and the limits; Tab stays within the
        // three fields while folded: the coordinator, the review, the fold.
        v.open_launch();
        v.handle_key(key(KeyCode::Tab));
        assert!(matches!(
            v.modal,
            Some(Modal::Launch {
                field: LaunchField::Review,
                ..
            })
        ));
        v.handle_key(key(KeyCode::Tab));
        assert!(matches!(
            v.modal,
            Some(Modal::Launch {
                field: LaunchField::Advanced,
                ..
            })
        ));
        v.handle_key(key(KeyCode::Tab));
        assert!(matches!(
            v.modal,
            Some(Modal::Launch {
                field: LaunchField::Planner,
                ..
            })
        ));
        v.handle_key(key(KeyCode::Tab));
        v.handle_key(key(KeyCode::Tab));
        assert_eq!(
            v.handle_key(key(KeyCode::Enter)),
            Outcome::None,
            "Enter on the fold unfolds"
        );
        assert!(matches!(
            v.modal,
            Some(Modal::Launch { advanced: true, .. })
        ));
        v.handle_key(key(KeyCode::Tab)); // goal
        for c in "Ship it".chars() {
            v.handle_key(key(KeyCode::Char(c)));
        }
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        match v.handle_key(ctrl_s) {
            Outcome::Start { goal, planner, .. } => {
                assert_eq!(goal, "Ship it");
                assert_eq!(planner, Harness::Codex);
            }
            other => panic!("expected Start, got {other:?}"),
        }
    }

    #[test]
    fn the_task_form_refuses_agy_with_a_writing_agent() {
        let mut v = board(
            SwarmStatus::Running,
            vec![task("x", TaskStatus::Failed, None)],
            &[],
        );
        v.harnesses = vec![Harness::Claude, Harness::Antigravity];
        v.writing_agents = vec!["doc-writer".into()];
        let failed = v.board[0].clone();
        v.open_task_form(Some(&failed), true, None, true);
        if let Some(Modal::Task { harness, agent, .. }) = &mut v.modal {
            *harness = 2; // agy
            *agent = TextArea::new("doc-writer");
        }
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(v.handle_key(ctrl_s), Outcome::None);
        match &v.modal {
            Some(Modal::Task {
                note: Some(n),
                field,
                ..
            }) => {
                assert!(
                    n.starts_with("✗ agy runs the `doc-writer` agent read-only"),
                    "{n}"
                );
                assert_eq!(*field, TaskField::Harness);
            }
            _ => panic!("the form should stay open with the reason"),
        }
        // Without the agent, agy is fine.
        if let Some(Modal::Task { agent, .. }) = &mut v.modal {
            *agent = TextArea::new("");
        }
        let out = v.handle_key(ctrl_s);
        assert!(
            matches!(out, Outcome::RetryTask { .. }),
            "{out:?} {:?}",
            v.status_msg
        );
    }

    #[test]
    fn the_launch_form_ticks_a_pool_and_keeps_the_coordinators_first() {
        let mut v = SwarmBoard::open(
            PathBuf::from("/nonexistent-ws"),
            None,
            &[],
            None,
            vec![Harness::Claude, Harness::Codex, Harness::Antigravity],
            Harness::Claude,
            0,
        );
        v.open_launch();
        v.handle_key(key(KeyCode::Tab)); // the review
        v.handle_key(key(KeyCode::Tab)); // the Advanced fold
        v.handle_key(key(KeyCode::Char(' '))); // open it
        v.handle_key(key(KeyCode::Tab)); // goal
        for c in "Ship it".chars() {
            v.handle_key(key(KeyCode::Char(c)));
        }
        v.handle_key(key(KeyCode::Tab)); // workers' pool, on claude
        v.handle_key(key(KeyCode::Char(' '))); // untick claude
        v.handle_key(key(KeyCode::Right));
        v.handle_key(key(KeyCode::Char(' '))); // codex
        v.handle_key(key(KeyCode::Right));
        v.handle_key(key(KeyCode::Char(' '))); // agy
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        match v.handle_key(ctrl_enter) {
            Outcome::Start {
                planner, workers, ..
            } => {
                assert_eq!(planner, Harness::Claude);
                assert_eq!(workers, [Harness::Codex, Harness::Antigravity]);
            }
            other => panic!("expected Start, got {other:?}"),
        }
        // A failed start brings the form back, as it was, with the reason.
        v.launch_failed("✗ no trace store".into());
        match &v.modal {
            Some(Modal::Launch {
                goal, pool, error, ..
            }) => {
                assert_eq!(goal.text, "Ship it");
                assert_eq!(pool, &[false, true, true]);
                assert_eq!(error.as_deref(), Some("✗ no trace store"));
            }
            _ => panic!("the form is not back"),
        }
        // An empty pool is refused in the form (still on the pool field).
        v.handle_key(key(KeyCode::Char(' ')));
        v.handle_key(key(KeyCode::Left));
        v.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(v.handle_key(ctrl_enter), Outcome::None);
        assert!(matches!(
            &v.modal,
            Some(Modal::Launch { error: Some(e), .. }) if e.contains("Tick at least one")
        ));
    }
}
