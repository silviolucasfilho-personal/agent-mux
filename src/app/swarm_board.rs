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

/// Which part of the screen has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The tree.
    Board,
    /// A card in the pane (scroll, or choose a message).
    Panel,
}

/// The panel's tabs, in the order `1`–`4` choose them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Task,
    Live,
    Thread,
    Changes,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Task, Tab::Live, Tab::Thread, Tab::Changes];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Task => "Task",
            Tab::Live => "Live",
            Tab::Thread => "Thread",
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
            NeedKind::Finished => "finished",
        }
    }

    /// The keys the item takes besides `Enter` and `d`.
    pub fn verbs(self) -> &'static str {
        match self {
            NeedKind::Question => "Enter answer",
            NeedKind::Prompt | NeedKind::Stalled => "Enter attach · r retry",
            NeedKind::Plan => "y approve · p re-plan",
            NeedKind::PlannerFailed => "p re-plan",
            NeedKind::Failed | NeedKind::Lost => "r retry",
            NeedKind::Review => "y merge · x set aside",
            NeedKind::Proposed => "y accept · x drop",
            NeedKind::CheckFailed => "Enter read it · r retry a task",
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
    Goal,
    Planner,
    WorkerHarness,
    Workers,
    Isolation,
    Gate,
    Check,
}

impl LaunchField {
    const ORDER: [LaunchField; 7] = [
        LaunchField::Goal,
        LaunchField::Planner,
        LaunchField::WorkerHarness,
        LaunchField::Workers,
        LaunchField::Isolation,
        LaunchField::Gate,
        LaunchField::Check,
    ];
    fn step(self, d: isize) -> Self {
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0) as isize;
        Self::ORDER[(i + d).rem_euclid(Self::ORDER.len() as isize) as usize]
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
        field: LaunchField,
        /// Why the last Ctrl+S did not start the swarm, shown in the form.
        error: Option<String>,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    Approve { tasks: usize },
    Merge { task_id: String, branch: String },
    SetAside { task_id: String, branch: String },
    Close,
    Delete { task_id: String, key: String },
    Cancel { task_id: String, key: String },
    Stop { session_idx: usize, key: String },
}

impl Confirm {
    pub fn question(&self) -> String {
        match self {
            Confirm::Approve { tasks } => {
                format!("Approve the plan? {tasks} task(s); workers start now.")
            }
            Confirm::Merge { branch, .. } => {
                format!("Merge {branch} into the workspace's branch (a merge commit)?")
            }
            Confirm::SetAside { branch, .. } => {
                format!("Set {branch} aside without merging? The branch is kept.")
            }
            Confirm::Close => {
                "Close the swarm? Its sessions stop, open tasks are cancelled, leases released."
                    .into()
            }
            Confirm::Delete { key, .. } => format!("Delete `{key}` from the plan?"),
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
    AddTask(PlannedTask),
    EditTask {
        task_id: String,
        task: PlannedTask,
    },
    /// Save the task's card, then put it back on the board, stopping its
    /// worker first when it still has one.
    RetryTask {
        task_id: String,
        task: PlannedTask,
        stop: Option<usize>,
    },
    DeleteTask(String),
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
}

/// A row by what it stands for, so the cursor survives a refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowKey {
    Swarm(String),
    Coordinator(String),
    Task(String),
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
    /// The swarm the next refresh puts the cursor on (one just started).
    select_next: Option<String>,
    /// Every swarm the tree lists.
    pub swarms: Vec<SwarmData>,
    pub rows: Vec<Row>,
    pub cursor: usize,
    folded: HashSet<String>,
    seen: HashSet<String>,
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
    pub live: HashMap<String, LiveWorker>,
    pub workers: HashMap<String, Worker>,
    pub asking: HashSet<String>,
    pub selected: usize,
    pub focus: Focus,
    chosen_tab: Option<(String, Tab)>,
    pub activity: bool,
    pub panel_scroll: usize,
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
    conn: Option<Connection>,
    last_change_seq: Option<i64>,
    live_fingerprint: u64,
    last_check: Option<std::time::Instant>,
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
            select_next: None,
            workspace,
            swarms: Vec::new(),
            rows: Vec::new(),
            cursor: 0,
            folded: HashSet::new(),
            seen: HashSet::new(),
            pane_focused: false,
            on_task: false,
            swarm: None,
            board: Vec::new(),
            messages: Vec::new(),
            problems: Problems::default(),
            live: HashMap::new(),
            workers: HashMap::new(),
            asking: HashSet::new(),
            selected: 0,
            focus: Focus::Board,
            chosen_tab: None,
            activity: false,
            panel_scroll: 0,
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
        })
    }

    /// Re-reads every swarm, its board and messages, keeping the cursor on
    /// the same row.
    pub fn refresh(&mut self, live: &[LiveSwarmSession], env: Option<&CheckEnv>, now_ns: i64) {
        let key = self
            .select_next
            .take()
            .map(RowKey::Swarm)
            .or_else(|| self.rows.get(self.cursor).and_then(|r| self.row_key(*r)));
        let prev_len = self.list_len();
        let at_bottom = prev_len == 0 || self.selected_message + 1 >= prev_len;
        let current = self.swarm.as_ref().map(|s| s.id.clone());
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
        if let Some(conn) = &self.conn {
            self.last_change_seq = crate::tracing::store::latest_change_seq(conn).ok();
            let here = self.norm_workspace();
            let mut runs = record::live_swarms(conn).unwrap_or_default();
            // The opened folder's swarm first, then the other live ones.
            runs.sort_by_key(|r| r.workspace != here);
            runs.extend(record::ended_swarms_since(conn, now_ns - RECENT_NS).unwrap_or_default());
            let mut swarms = Vec::with_capacity(runs.len());
            for run in runs {
                let board = record::board(conn, &run.id).unwrap_or_default();
                let messages =
                    crate::bus::store::list_workspace_messages(conn, &run.workspace, 500)
                        .unwrap_or_default();
                let asking = open_questions(&messages)
                    .into_iter()
                    .map(|m| m.from_launch.clone())
                    .collect();
                let problems = match env {
                    Some(env)
                        if Some(&run.id) == current.as_ref()
                            && matches!(
                                run.status,
                                SwarmStatus::Planning | SwarmStatus::Review
                            )
                            && !board.is_empty() =>
                    {
                        crate::bus::swarm::plan::check(&record::board_plan(&run, &board), env)
                    }
                    _ => Problems::default(),
                };
                if !self.seen.contains(&run.id) {
                    self.seen.insert(run.id.clone());
                    if run.ended_ns.is_some() {
                        self.folded.insert(run.id.clone());
                    }
                }
                let mut d = SwarmData {
                    run,
                    board,
                    messages,
                    asking,
                    problems,
                    needs: Vec::new(),
                };
                d.needs = needs_for(&d, &self.workers, now_ns);
                swarms.push(d);
            }
            self.swarms = swarms;
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
        if self.on_task && self.tab() == Tab::Changes {
            self.diff_for = None;
            self.load_diff();
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
            for j in 0..d.board.len() {
                self.rows.push(Row::Task(i, j));
            }
        }
    }

    /// Points the selected-swarm fields at the cursor's swarm.
    fn sync_current(&mut self) {
        let (si, task) = match self.rows.get(self.cursor) {
            Some(Row::Swarm(i)) | Some(Row::Coordinator(i)) => (Some(*i), None),
            Some(Row::Task(i, j)) => (Some(*i), Some(*j)),
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
            }
            None => {
                self.swarm = None;
                self.board.clear();
                self.messages.clear();
                self.asking.clear();
                self.problems = Problems::default();
                self.needs.clear();
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
            Some(Row::Task(..)) => {
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
            Row::Coordinator(_) => 0,
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
        let si = match self.rows.get(self.cursor) {
            Some(Row::Swarm(i)) => *i,
            Some(Row::Coordinator(i)) | Some(Row::Task(i, _)) => {
                if fold {
                    // ← on a child climbs to its swarm
                    let at = self
                        .rows
                        .iter()
                        .position(|r| *r == Row::Swarm(*i))
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
        self.modal.is_some() || self.command.is_some() || self.pane_focused
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
        Some(engine::Input {
            swarm: self.swarm.as_ref()?,
            board: &self.board,
            workers: &self.workers,
            asking: &self.asking,
            now_ns: self.now_ns,
            dispatch_grace_ns: engine::DISPATCH_GRACE_NS,
        })
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
    }

    fn after_selection_change(&mut self) {
        self.panel_scroll = 0;
        self.selected_message = self.list_len().saturating_sub(1);
        if self.tab() == Tab::Changes {
            self.load_diff();
        }
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
        if self.activity || self.tab() == Tab::Thread {
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
            self.diff_lines.clear();
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
            field: LaunchField::Goal,
            error: None,
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

    /// Puts the cursor on swarm `id` at the next refresh.
    pub fn select_swarm(&mut self, id: &str) {
        self.select_next = Some(id.to_string());
    }

    fn open_task_modal(&mut self, editing: Option<&BoardTask>) {
        let runs = editing.is_none_or(|t| t.ext.attempts == 0 && t.task.status == TaskStatus::Open);
        self.open_task_form(editing, false, None, runs);
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

    fn open_notes(&mut self) {
        self.command = Some(CommandLine {
            text: TextArea::new(""),
            reply_to: None,
            purpose: LinePurpose::ReplanNotes,
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
        if line.purpose == LinePurpose::ReplanNotes {
            return None;
        }
        Some(parse_command(&line.text.text, self, line.reply_to.as_ref()))
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(line) = &mut self.command {
            line.text.insert_str(text);
        } else if let Some(Modal::Launch {
            goal, field, check, ..
        }) = &mut self.modal
        {
            match field {
                LaunchField::Check => check.insert_str(text),
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
        let in_list = (self.focus == Focus::Panel)
            && (self.activity || (self.on_task && self.tab() == Tab::Thread));
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
            KeyCode::Char(c @ '1'..='4') => {
                if self.on_task {
                    self.activity = false;
                    self.set_tab(Tab::ALL[(c as u8 - b'1') as usize]);
                } else {
                    self.status_msg = Some("1-4 choose a task's view: select a task.".into());
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
            KeyCode::Char('a') => {
                self.activity = !self.activity;
                self.selected_message = self.list_len().saturating_sub(1);
                Outcome::None
            }
            KeyCode::Char('>') => {
                self.open_command("", None);
                Outcome::None
            }
            KeyCode::Char('b') => {
                self.open_command("@all ", None);
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
            KeyCode::Char('p') => {
                if self.swarm.as_ref().is_some_and(|s| {
                    matches!(s.status, SwarmStatus::Review | SwarmStatus::Planning)
                }) {
                    self.open_notes();
                } else {
                    self.status_msg = Some("Re-plan from review: the plan is not open now.".into());
                }
                Outcome::None
            }
            KeyCode::Char('n') => {
                // a new swarm on a swarm row or with none; a new task inside one
                let inside = matches!(
                    self.rows.get(self.cursor),
                    Some(Row::Task(..)) | Some(Row::Coordinator(_))
                );
                if inside && self.swarm.as_ref().is_some_and(|s| s.ended_ns.is_none()) {
                    self.open_task_modal(None);
                } else {
                    self.open_launch();
                }
                Outcome::None
            }
            KeyCode::Char('e') => {
                match self.selected_task().cloned() {
                    Some(t) if !t.task.status.is_terminal() => self.open_task_modal(Some(&t)),
                    Some(_) => self.status_msg = Some("A finished task is not edited.".into()),
                    None => {}
                }
                Outcome::None
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                if let Some(t) = self.selected_task() {
                    if t.ext.attempts == 0 && t.task.status == TaskStatus::Open {
                        self.confirm = Some(Confirm::Delete {
                            task_id: t.task.id.clone(),
                            key: t.ext.plan_key.clone(),
                        });
                    } else {
                        self.status_msg = Some(
                            "Only a task that never started is deleted (x cancels one).".into(),
                        );
                    }
                }
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
        if in_list {
            if let Some(m) = self.selected_message().cloned() {
                self.modal = Some(Modal::Message {
                    message: m,
                    scroll: 0,
                });
            }
            return Outcome::None;
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
            PaneView::SwarmCard => {
                if let Some(Row::Swarm(i)) = self.rows.get(self.cursor).copied() {
                    let folded = self.is_folded(i);
                    self.fold(!folded);
                }
                Outcome::None
            }
            PaneView::TaskCard => {
                let editable = self
                    .selected_task()
                    .cloned()
                    .filter(|t| t.task.status == TaskStatus::Open && t.task.assigned_to.is_none());
                match editable {
                    Some(t) => self.open_task_modal(Some(&t)),
                    None if self
                        .selected_task()
                        .is_some_and(|t| self.worker_of(t).is_none()) =>
                    {
                        self.status_msg = Some("No live worker on this task.".into())
                    }
                    None => self.set_tab(Tab::Live),
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
                    self.confirm = Some(Confirm::Merge {
                        task_id: t.task.id.clone(),
                        branch: t.task.branch.clone().unwrap_or_default(),
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
            Confirm::SetAside { task_id, .. } => self.verb(SwarmVerb::SetAside, Some(task_id)),
            Confirm::Close => self.verb(SwarmVerb::Close, None),
            Confirm::Delete { task_id, .. } => Outcome::DeleteTask(task_id),
            Confirm::Cancel { task_id, .. } => Outcome::CancelTask(task_id),
            Confirm::Stop { session_idx, .. } => Outcome::StopSession(session_idx),
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
        if (key.code == KeyCode::Char('s') && ctrl)
            || (key.code == KeyCode::Enter && ctrl)
            || (key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE && !multiline)
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
                field,
                ..
            }) => match key.code {
                KeyCode::Tab => *field = field.step(1),
                KeyCode::BackTab => *field = field.step(-1),
                _ => match field {
                    LaunchField::Goal => {
                        crate::keymap::apply_text(goal, &key);
                    }
                    LaunchField::Check => {
                        crate::keymap::apply_text(check, &key);
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
                field,
                ..
            }) => {
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
                let why = if goal.text.trim().is_empty() {
                    Some("Write the goal first.")
                } else if chosen.is_empty() {
                    Some("Tick at least one harness for the workers (Space).")
                } else {
                    None
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
                    field,
                    error: why.map(str::to_string),
                };
                if why.is_some() {
                    self.modal = Some(form);
                    return Outcome::None;
                }
                let Modal::Launch { goal, check, .. } = &form else {
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
                match editing {
                    Some(task_id) if retry => Outcome::RetryTask {
                        task_id,
                        task: planned,
                        stop,
                    },
                    Some(task_id) => Outcome::EditTask {
                        task_id,
                        task: planned,
                    },
                    None => Outcome::AddTask(planned),
                }
            }
            other => {
                self.modal = other;
                Outcome::None
            }
        }
    }

    fn move_cursor(&mut self, delta: isize, in_list: bool) {
        if in_list {
            let len = self.list_len();
            self.selected_message = step(self.selected_message, len, delta);
        } else if self.focus == Focus::Panel {
            self.panel_scroll = step(self.panel_scroll, usize::MAX / 4, delta);
        } else {
            let at = step(self.cursor, self.rows.len(), delta);
            self.set_cursor(at);
        }
    }
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
                "the plan waits for you: {} task(s){}",
                v.board.len(),
                if v.problems.errors.is_empty() {
                    String::new()
                } else {
                    format!(", {} error(s) to fix first", v.problems.errors.len())
                }
            ),
        }),
        _ => {}
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
    fn y_approves_a_clean_plan_and_refuses_one_with_errors() {
        let mut v = board(
            SwarmStatus::Review,
            vec![task("a", TaskStatus::Open, None)],
            &[],
        );
        assert_eq!(v.needs[0].kind, NeedKind::Plan);
        assert_eq!(v.handle_key(key(KeyCode::Char('y'))), Outcome::None);
        assert_eq!(v.confirm, Some(Confirm::Approve { tasks: 1 }));
        // Enter does not confirm; only y does.
        assert_eq!(v.handle_key(key(KeyCode::Enter)), Outcome::None);
        assert_eq!(v.confirm, None);
        v.handle_key(key(KeyCode::Char('y')));
        assert_eq!(
            v.handle_key(key(KeyCode::Char('y'))),
            Outcome::Verb {
                verb: SwarmVerb::Approve,
                task: None,
                notes: None
            }
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
    fn p_takes_notes_for_a_re_plan() {
        let mut v = board(
            SwarmStatus::Review,
            vec![task("a", TaskStatus::Open, None)],
            &[],
        );
        v.handle_key(key(KeyCode::Char('p')));
        assert!(v.text_entry_active());
        for c in "split a".chars() {
            v.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            v.handle_key(key(KeyCode::Enter)),
            Outcome::Verb {
                verb: SwarmVerb::Replan,
                task: None,
                notes: Some("split a".into())
            }
        );
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
                branch: "loop/bus-a".into()
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
    fn n_adds_a_task_and_the_form_names_what_is_missing() {
        let mut v = board(
            SwarmStatus::Review,
            vec![task("a", TaskStatus::Open, None)],
            &[],
        );
        v.handle_key(key(KeyCode::Char('n')));
        for c in "b".chars() {
            v.handle_key(key(KeyCode::Char(c)));
        }
        v.handle_key(key(KeyCode::Tab));
        for c in "Second".chars() {
            v.handle_key(key(KeyCode::Char(c)));
        }
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(v.handle_key(ctrl_s), Outcome::None);
        assert_eq!(v.status_msg.as_deref(), Some("Instructions is required."));
        for c in "do it".chars() {
            v.handle_key(key(KeyCode::Char(c)));
        }
        v.handle_key(key(KeyCode::Tab));
        v.handle_key(key(KeyCode::Tab));
        v.handle_key(key(KeyCode::Tab));
        for c in "a".chars() {
            v.handle_key(key(KeyCode::Char(c)));
        }
        match v.handle_key(ctrl_s) {
            Outcome::AddTask(t) => {
                assert_eq!((t.key.as_str(), t.title.as_str()), ("b", "Second"));
                assert_eq!(t.instructions, "do it");
                assert_eq!(t.depends_on, ["a"]);
            }
            other => panic!("expected AddTask, got {other:?}"),
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
        assert!(matches!(v.modal, Some(Modal::Launch { planner: 1, .. })));
        for c in "Ship it".chars() {
            v.handle_key(key(KeyCode::Char(c)));
        }
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(
            v.handle_key(ctrl_s),
            Outcome::Start {
                goal: "Ship it".into(),
                planner: Harness::Codex,
                workers: vec![Harness::Codex],
                max_workers: 3,
                isolation: TaskIsolation::Shared,
                gate: true,
                check: None
            }
        );
    }

    #[test]
    fn the_task_form_refuses_agy_with_a_writing_agent() {
        let mut v = board(SwarmStatus::Running, vec![], &[]);
        v.harnesses = vec![Harness::Claude, Harness::Antigravity];
        v.writing_agents = vec!["doc-writer".into()];
        v.open_task_form(None, false, None, true);
        if let Some(Modal::Task {
            key,
            title,
            instructions,
            harness,
            agent,
            ..
        }) = &mut v.modal
        {
            *key = TextArea::new("spec");
            *title = TextArea::new("Write the spec");
            *instructions = TextArea::new("Write it");
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
        assert!(matches!(v.handle_key(ctrl_s), Outcome::AddTask(_)));
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
        for c in "Ship it".chars() {
            v.handle_key(key(KeyCode::Char(c)));
        }
        v.handle_key(key(KeyCode::Tab)); // coordinator
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
