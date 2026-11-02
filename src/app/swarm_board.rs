//! The Swarm screen (`Mode::Swarm`), built around the task board
//! (`docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md` §8).
//!
//! One workspace's orchestrated swarm: the mission strip, the **Needs you**
//! strip (what only the operator can resolve, `!` to act), the **board**
//! (one row per plan task, in plan order, with its shown state from
//! `bus::swarm::engine::shown`) and a **panel** on the selected task with
//! four tabs: Task (its card, editable in review), Live (its worker's
//! terminal), Thread (its messages) and Changes (its diff). `a` swaps the
//! panel for the whole swarm's Activity. Messages are written on the `>`
//! command line (`[!] [@who] [#topic] body`, `@all` or `@<task key>`).
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
    Board,
    Panel,
    Needs,
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
            NeedKind::PlannerFailed => "planner",
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
    Workers,
    Isolation,
    Gate,
    Check,
}

impl LaunchField {
    const ORDER: [LaunchField; 6] = [
        LaunchField::Goal,
        LaunchField::Planner,
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
}

impl TaskField {
    const ORDER: [TaskField; 6] = [
        TaskField::Key,
        TaskField::Title,
        TaskField::Instructions,
        TaskField::Acceptance,
        TaskField::Files,
        TaskField::DependsOn,
    ];
    fn step(self, d: isize, with_key: bool) -> Self {
        let order: Vec<TaskField> = Self::ORDER
            .iter()
            .copied()
            .filter(|f| with_key || *f != TaskField::Key)
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
        }
    }
}

pub enum Modal {
    Launch {
        goal: TextArea,
        planner: usize,
        workers: usize,
        isolation: TaskIsolation,
        gate: bool,
        check: TextArea,
        field: LaunchField,
    },
    /// Add a task (`editing` none) or edit one.
    Task {
        editing: Option<String>,
        key: TextArea,
        title: TextArea,
        instructions: TextArea,
        acceptance: TextArea,
        files: TextArea,
        depends_on: TextArea,
        field: TaskField,
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
    Start {
        goal: String,
        planner: Harness,
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
    pub running_tool: Option<String>,
}

pub struct SwarmBoard {
    pub workspace: PathBuf,
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
    pub selected_need: usize,
    dismissed: HashSet<String>,
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
            workspace,
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
            selected_need: 0,
            dismissed: HashSet::new(),
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
    pub fn refresh(&mut self, live: &[LiveSwarmSession], env: Option<&CheckEnv>, now_ns: i64) {
        let selected_id = self.selected_task().map(|t| t.task.id.clone());
        let prev_len = self.list_len();
        let at_bottom = prev_len == 0 || self.selected_message + 1 >= prev_len;
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
            let ws = self.norm_workspace();
            let live_swarm = record::live_swarm(conn, &ws).ok().flatten();
            // Keep showing a swarm that just ended until a new one starts.
            self.swarm = match (live_swarm, self.swarm.take()) {
                (Some(s), _) => Some(s),
                (None, Some(old)) => record::get_swarm(conn, &old.id).ok().flatten(),
                (None, None) => None,
            };
            self.board = match &self.swarm {
                Some(s) => record::board(conn, &s.id).unwrap_or_default(),
                None => Vec::new(),
            };
            self.messages =
                crate::bus::store::list_workspace_messages(conn, &ws, 500).unwrap_or_default();
        }
        self.asking = open_questions(&self.messages)
            .into_iter()
            .map(|m| m.from_launch.clone())
            .collect();
        self.problems = match (&self.swarm, env) {
            (Some(s), Some(env)) if self.wants_checks() && !self.board.is_empty() => {
                crate::bus::swarm::plan::check(&record::board_plan(s, &self.board), env)
            }
            _ => Problems::default(),
        };
        if let Some(id) = selected_id
            && let Some(i) = self.board.iter().position(|t| t.task.id == id)
        {
            self.selected = i;
        }
        self.selected = self.selected.min(self.board.len().saturating_sub(1));
        self.recompute_needs();
        let len = self.list_len();
        if at_bottom || self.selected_message >= len {
            self.selected_message = len.saturating_sub(1);
        }
        if self.tab() == Tab::Changes {
            self.diff_for = None;
            self.load_diff();
        }
    }

    pub fn text_entry_active(&self) -> bool {
        self.modal.is_some() || self.command.is_some()
    }

    pub fn confirmation_active(&self) -> bool {
        self.confirm.is_some()
    }

    pub fn selected_task(&self) -> Option<&BoardTask> {
        self.board.get(self.selected)
    }

    fn input(&self) -> Option<engine::Input<'_>> {
        Some(engine::Input {
            swarm: self.swarm.as_ref()?,
            board: &self.board,
            workers: &self.workers,
            asking: &self.asking,
            now_ns: self.now_ns,
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

    pub fn selected_need(&self) -> Option<&Need> {
        self.needs.get(self.selected_need)
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

    fn recompute_needs(&mut self) {
        self.needs = compute_needs(self)
            .into_iter()
            .filter(|n| !self.dismissed.contains(&n.key))
            .collect();
        self.selected_need = self.selected_need.min(self.needs.len().saturating_sub(1));
        if self.focus == Focus::Needs && self.needs.is_empty() {
            self.focus = Focus::Board;
        }
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

    fn select_task_id(&mut self, id: &str) {
        if let Some(i) = self.board.iter().position(|t| t.task.id == id) {
            self.selected = i;
            self.after_selection_change();
        }
    }

    pub fn open_launch(&mut self) {
        if let Some(s) = &self.swarm
            && s.ended_ns.is_none()
        {
            self.status_msg = Some(format!(
                "{} is {}: one swarm per workspace (close it first)",
                s.id,
                s.status.as_str()
            ));
            return;
        }
        let planner = self
            .harnesses
            .iter()
            .position(|h| *h == self.default_harness)
            .unwrap_or(0);
        self.modal = Some(Modal::Launch {
            goal: TextArea::new(""),
            planner,
            workers: 3,
            isolation: TaskIsolation::Shared,
            gate: true,
            check: TextArea::new(""),
            field: LaunchField::Goal,
        });
    }

    fn open_task_modal(&mut self, editing: Option<&BoardTask>) {
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
        self.modal = Some(Modal::Task {
            editing: editing.map(|t| t.task.id.clone()),
            key: TextArea::new(key),
            title: TextArea::new(title),
            instructions: TextArea::new(instructions),
            acceptance: TextArea::new(acceptance),
            files: TextArea::new(files),
            depends_on: TextArea::new(deps),
            field: if editing.is_some() {
                TaskField::Title
            } else {
                TaskField::Key
            },
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
            ..
        }) = &mut self.modal
        {
            match field {
                TaskField::Key => key.insert_str(text),
                TaskField::Title => title.insert_str(text),
                TaskField::Instructions => instructions.insert_str(text),
                TaskField::Acceptance => acceptance.insert_str(text),
                TaskField::Files => files.insert_str(text),
                TaskField::DependsOn => depends_on.insert_str(text),
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
        if self.focus == Focus::Needs
            && let Some(o) = self.handle_needs_key(key)
        {
            return o;
        }
        let in_list = (self.focus == Focus::Panel) && (self.activity || self.tab() == Tab::Thread);
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
                self.activity = false;
                self.set_tab(Tab::ALL[(c as u8 - b'1') as usize]);
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
                if self.needs.is_empty() {
                    self.status_msg = Some("Nothing needs you right now.".into());
                } else {
                    self.focus = Focus::Needs;
                }
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
                if in_list && let Some(m) = self.selected_message().cloned() {
                    self.open_reply(&m);
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
                if self
                    .swarm
                    .as_ref()
                    .is_some_and(|s| s.status == SwarmStatus::Review)
                {
                    self.open_notes();
                } else {
                    self.status_msg = Some("Re-plan from review: the plan is not open now.".into());
                }
                Outcome::None
            }
            KeyCode::Char('n') => {
                if self.swarm.as_ref().is_some_and(|s| s.ended_ns.is_none()) {
                    self.open_task_modal(None);
                } else {
                    self.status_msg = Some("Start a swarm first (r).".into());
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
        let Some(swarm) = &self.swarm else {
            self.open_launch();
            return Outcome::None;
        };
        if swarm.ended_ns.is_some() {
            self.open_launch();
            return Outcome::None;
        }
        if swarm.status == SwarmStatus::Planning
            && let Some(p) = self.planner_session()
        {
            return Outcome::Attach(p.session_idx);
        }
        let Some(t) = self.selected_task().cloned() else {
            return Outcome::None;
        };
        if let Some(w) = self.worker_of(&t) {
            return Outcome::Attach(w.session_idx);
        }
        if matches!(swarm.status, SwarmStatus::Review | SwarmStatus::Planning)
            || t.task.status == TaskStatus::Open
        {
            self.open_task_modal(Some(&t));
        }
        Outcome::None
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
        if let Some(t) = self.selected_task() {
            let st = self.shown(t);
            if matches!(
                st,
                Shown::Failed | Shown::Lost | Shown::Cancelled | Shown::Stalled
            ) {
                let id = t.task.id.clone();
                if st == Shown::Stalled || st == Shown::Lost {
                    // A worker still open on it is stopped first.
                    if let Some(w) = self.worker_of(t) {
                        return Outcome::StopSession(w.session_idx);
                    }
                }
                return self.verb(SwarmVerb::Retry, Some(id));
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

    fn handle_needs_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        let need = self.selected_need().cloned()?;
        if let Some(id) = &need.task_id
            && self.selected_task().map(|t| &t.task.id) != Some(id)
        {
            self.select_task_id(id);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected_need = self.selected_need.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected_need = (self.selected_need + 1).min(self.needs.len() - 1);
            }
            KeyCode::Char('d') => {
                self.dismissed.insert(need.key.clone());
                self.recompute_needs();
            }
            KeyCode::Enter => return Some(self.act_on(&need)),
            KeyCode::Char('y') if need.kind == NeedKind::Finished => {
                self.confirm = Some(Confirm::Close);
            }
            KeyCode::Char('y') | KeyCode::Char('x') | KeyCode::Char('r') | KeyCode::Char('p') => {
                return None; // the board's verbs, on the task just selected
            }
            _ => return None,
        }
        Some(Outcome::None)
    }

    fn act_on(&mut self, need: &Need) -> Outcome {
        self.focus = Focus::Board;
        match need.kind {
            NeedKind::Question => {
                if let Some(m) = &need.message {
                    let m = m.clone();
                    self.open_reply(&m);
                }
                Outcome::None
            }
            NeedKind::Prompt | NeedKind::Stalled => {
                match self.selected_task().and_then(|t| self.worker_of(t)) {
                    Some(w) => Outcome::Attach(w.session_idx),
                    None => Outcome::None,
                }
            }
            NeedKind::Plan => {
                self.set_tab(Tab::Task);
                Outcome::None
            }
            NeedKind::PlannerFailed => {
                self.open_notes();
                Outcome::None
            }
            NeedKind::Failed | NeedKind::Lost | NeedKind::Proposed => {
                self.set_tab(Tab::Task);
                Outcome::None
            }
            NeedKind::Review => {
                self.set_tab(Tab::Changes);
                Outcome::None
            }
            NeedKind::Finished => {
                self.confirm = Some(Confirm::Close);
                Outcome::None
            }
            NeedKind::CheckFailed => {
                if let Some(s) = &self.swarm
                    && let Some(r) = s
                        .check_result
                        .as_deref()
                        .and_then(crate::bus::swarm::check::CheckResult::parse)
                {
                    self.modal = Some(Modal::Message {
                        message: BusMessage {
                            id: "check".into(),
                            workspace: s.workspace.clone(),
                            from_launch: "agent-mux".into(),
                            to_launch: Some("user".into()),
                            topic: "check".into(),
                            body: format!(
                                "{}\n\n{}",
                                r.summary(s.check_cmd.as_deref().unwrap_or_default()),
                                r.tail
                            ),
                            reply_to: None,
                            created_ns: r.started_ns,
                            delivered_ns: None,
                            acked_ns: None,
                        },
                        scroll: 0,
                    });
                }
                Outcome::None
            }
        }
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
            || (key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE && !multiline)
        {
            return self.submit_modal();
        }
        let n_harness = self.harnesses.len().max(1);
        match &mut self.modal {
            Some(Modal::Launch {
                goal,
                planner,
                workers,
                isolation,
                gate,
                check,
                field,
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
                    LaunchField::Planner => match key.code {
                        KeyCode::Left | KeyCode::Char('h') => {
                            *planner = (*planner + n_harness - 1) % n_harness
                        }
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                            *planner = (*planner + 1) % n_harness
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
                key: k,
                title,
                instructions,
                acceptance,
                files,
                depends_on,
                field,
            }) => {
                let with_key = editing.is_none();
                match key.code {
                    KeyCode::Tab => *field = field.step(1, with_key),
                    KeyCode::BackTab => *field = field.step(-1, with_key),
                    _ => {
                        crate::keymap::apply_text(
                            match field {
                                TaskField::Key => k,
                                TaskField::Title => title,
                                TaskField::Instructions => instructions,
                                TaskField::Acceptance => acceptance,
                                TaskField::Files => files,
                                TaskField::DependsOn => depends_on,
                            },
                            &key,
                        );
                    }
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
                workers,
                isolation,
                gate,
                check,
                field,
            }) => {
                if goal.text.trim().is_empty() {
                    self.status_msg = Some("Write the goal first.".into());
                    self.modal = Some(Modal::Launch {
                        goal,
                        planner,
                        workers,
                        isolation,
                        gate,
                        check,
                        field: LaunchField::Goal,
                    });
                    let _ = field;
                    return Outcome::None;
                }
                Outcome::Start {
                    goal: goal.text.trim().to_string(),
                    planner: self
                        .harnesses
                        .get(planner)
                        .copied()
                        .unwrap_or(self.default_harness),
                    max_workers: workers,
                    isolation,
                    gate,
                    check: Some(check.text.trim().to_string()).filter(|c| !c.is_empty()),
                }
            }
            Some(Modal::Task {
                editing,
                key,
                title,
                instructions,
                acceptance,
                files,
                depends_on,
                field,
            }) => {
                let planned = PlannedTask {
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
                    ..Default::default()
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
                if let Some(f) = missing {
                    self.status_msg = Some(format!("{} is required.", f.label()));
                    self.modal = Some(Modal::Task {
                        editing,
                        key,
                        title,
                        instructions,
                        acceptance,
                        files,
                        depends_on,
                        field: f,
                    });
                    let _ = field;
                    return Outcome::None;
                }
                match editing {
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
            let before = self.selected;
            self.selected = step(self.selected, self.board.len(), delta);
            if self.selected != before {
                self.after_selection_change();
            }
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

/// What needs the operator now, most urgent first.
pub fn compute_needs(v: &SwarmBoard) -> Vec<Need> {
    let mut out = Vec::new();
    let Some(swarm) = &v.swarm else {
        return out;
    };
    if swarm.ended_ns.is_some() {
        return out;
    }
    let board_launches: HashSet<&str> = v
        .board
        .iter()
        .filter_map(|t| t.task.assigned_to.as_deref())
        .collect();
    for q in open_questions(&v.messages) {
        let to_operator = matches!(q.to_launch.as_deref(), None | Some("user"));
        if !to_operator && !board_launches.contains(q.from_launch.as_str()) {
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
            text: format!(
                "{} asks: {}",
                v.name_of(&q.from_launch),
                first_line(&q.body)
            ),
        });
    }
    match swarm.status {
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
    for t in &v.board {
        let st = v.shown(t);
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
        v.swarm = Some(swarm(status));
        v.board = tasks;
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
                running_tool: None,
            };
            v.workers.insert(launch.to_string(), w.worker);
            v.live.insert(launch.to_string(), w);
        }
        v.recompute_needs();
        v
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
        v.messages = vec![msg(
            "m1",
            "L2",
            Some("user"),
            "question",
            "Which table?\nmore",
            5,
        )];
        v.asking = ["L2".to_string()].into();
        v.recompute_needs();
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
        // `d` dismisses one for the screen's lifetime.
        v.focus = Focus::Needs;
        v.selected_need = 2;
        v.handle_key(key(KeyCode::Char('d')));
        assert!(v.needs.iter().all(|n| n.kind != NeedKind::Failed));
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
        assert_eq!(v.handle_key(key(KeyCode::Enter)), Outcome::Attach(0));
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
        assert_eq!(
            v.handle_key(key(KeyCode::Char('r'))),
            Outcome::Verb {
                verb: SwarmVerb::Retry,
                task: Some("task-f".into()),
                notes: None
            }
        );
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
                max_workers: 3,
                isolation: TaskIsolation::Shared,
                gate: true,
                check: None
            }
        );
    }
}
