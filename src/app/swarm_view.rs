//! Agent Swarm Control Room (`Mode::Swarm`).
//!
//! Provides a 4-quadrant TUI dashboard for orchestrating one Coordinator and
//! multiple Worker agents sharing a workspace over the Inter-Agent Bus:
//! - `[1] Agents Roster`: Coordinator (`★ Coord`) + Workers (`W1..Wn`) with live status.
//! - `[2] Live Preview`: Zero-copy live terminal (`PseudoTerminal`), Task Detail, or Git Diff.
//! - `[3] Task Board & File Leases`: DAG subtasks (`open`/`claimed`/`running`/`done`/`failed`) and active file globs.
//! - `[4] Inter-Agent Message Feed`: Chronological message stream (`api-contract`, `question`, `answer`, `blocker`, `handoff`) and composer.

use crate::app::text_area::TextArea;
use crate::bus::swarm::{LiveSwarmSession, SwarmMember, SwarmSnapshot, load_snapshot};
use crate::bus::{BusMessage, BusTask, FileLease, TaskIsolation, short_id};
use crate::harness::Harness;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const HARNESS_CYCLE: [Harness; 3] = [Harness::Claude, Harness::Codex, Harness::Antigravity];

/// Active quadrant focus inside `Mode::Swarm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwarmPane {
    Agents,
    Preview,
    Board,
    Messages,
}

impl SwarmPane {
    pub fn next(self) -> Self {
        match self {
            SwarmPane::Agents => SwarmPane::Preview,
            SwarmPane::Preview => SwarmPane::Board,
            SwarmPane::Board => SwarmPane::Messages,
            SwarmPane::Messages => SwarmPane::Agents,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            SwarmPane::Agents => SwarmPane::Messages,
            SwarmPane::Preview => SwarmPane::Agents,
            SwarmPane::Board => SwarmPane::Preview,
            SwarmPane::Messages => SwarmPane::Board,
        }
    }
}

/// Display mode for Quadrant `[2] Live Preview`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewMode {
    Terminal,
    TaskDetail,
    GitDiff,
}

impl PreviewMode {
    pub fn next(self) -> Self {
        match self {
            PreviewMode::Terminal => PreviewMode::TaskDetail,
            PreviewMode::TaskDetail => PreviewMode::GitDiff,
            PreviewMode::GitDiff => PreviewMode::Terminal,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PreviewMode::Terminal => "Terminal",
            PreviewMode::TaskDetail => "Task Detail",
            PreviewMode::GitDiff => "Git Diff",
        }
    }
}

/// Sub-tab inside Quadrant `[3] Task Board & File Leases`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardMode {
    Tasks,
    Leases,
}

impl BoardMode {
    pub fn toggle(self) -> Self {
        match self {
            BoardMode::Tasks => BoardMode::Leases,
            BoardMode::Leases => BoardMode::Tasks,
        }
    }
}

/// Filter applied to Quadrant `[4] Inter-Agent Message Feed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageFilter {
    All,
    SelectedAgent,
    QuestionsAndUrgent,
}

impl MessageFilter {
    pub fn next(self) -> Self {
        match self {
            MessageFilter::All => MessageFilter::SelectedAgent,
            MessageFilter::SelectedAgent => MessageFilter::QuestionsAndUrgent,
            MessageFilter::QuestionsAndUrgent => MessageFilter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MessageFilter::All => "All",
            MessageFilter::SelectedAgent => "Selected Agent",
            MessageFilter::QuestionsAndUrgent => "Questions & Urgent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchField {
    Goal,
    Harness,
    MaxWorkers,
    Isolation,
    PrewarmWorkers,
}

impl LaunchField {
    pub fn next(self) -> Self {
        match self {
            LaunchField::Goal => LaunchField::Harness,
            LaunchField::Harness => LaunchField::MaxWorkers,
            LaunchField::MaxWorkers => LaunchField::Isolation,
            LaunchField::Isolation => LaunchField::PrewarmWorkers,
            LaunchField::PrewarmWorkers => LaunchField::Goal,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            LaunchField::Goal => LaunchField::PrewarmWorkers,
            LaunchField::Harness => LaunchField::Goal,
            LaunchField::MaxWorkers => LaunchField::Harness,
            LaunchField::Isolation => LaunchField::MaxWorkers,
            LaunchField::PrewarmWorkers => LaunchField::Isolation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerField {
    Title,
    Instructions,
    Files,
    Harness,
    Agent,
    Isolation,
}

impl WorkerField {
    pub fn next(self) -> Self {
        match self {
            WorkerField::Title => WorkerField::Instructions,
            WorkerField::Instructions => WorkerField::Files,
            WorkerField::Files => WorkerField::Harness,
            WorkerField::Harness => WorkerField::Agent,
            WorkerField::Agent => WorkerField::Isolation,
            WorkerField::Isolation => WorkerField::Title,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            WorkerField::Title => WorkerField::Isolation,
            WorkerField::Instructions => WorkerField::Title,
            WorkerField::Files => WorkerField::Instructions,
            WorkerField::Harness => WorkerField::Files,
            WorkerField::Agent => WorkerField::Harness,
            WorkerField::Isolation => WorkerField::Agent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskField {
    Title,
    Instructions,
    Files,
    Isolation,
}

impl TaskField {
    pub fn next(self, has_isolation: bool) -> Self {
        match self {
            TaskField::Title => TaskField::Instructions,
            TaskField::Instructions => TaskField::Files,
            TaskField::Files => {
                if has_isolation {
                    TaskField::Isolation
                } else {
                    TaskField::Title
                }
            }
            TaskField::Isolation => TaskField::Title,
        }
    }

    pub fn prev(self, has_isolation: bool) -> Self {
        match self {
            TaskField::Title => {
                if has_isolation {
                    TaskField::Isolation
                } else {
                    TaskField::Files
                }
            }
            TaskField::Instructions => TaskField::Title,
            TaskField::Files => TaskField::Instructions,
            TaskField::Isolation => TaskField::Files,
        }
    }
}

/// The composer's fields: the body is text, the rest are options Tab
/// reaches, so no text-editing key (Ctrl+U, Ctrl+W) is taken from the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeField {
    Body,
    Topic,
    Urgent,
    Wake,
}

impl ComposeField {
    pub fn next(self) -> Self {
        match self {
            ComposeField::Body => ComposeField::Topic,
            ComposeField::Topic => ComposeField::Urgent,
            ComposeField::Urgent => ComposeField::Wake,
            ComposeField::Wake => ComposeField::Body,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            ComposeField::Body => ComposeField::Wake,
            ComposeField::Topic => ComposeField::Body,
            ComposeField::Urgent => ComposeField::Topic,
            ComposeField::Wake => ComposeField::Urgent,
        }
    }
}

pub const MESSAGE_TOPICS: [&str; 6] = [
    "directive",
    "answer",
    "question",
    "api-contract",
    "blocker",
    "handoff",
];

#[derive(Debug, Clone)]
pub enum Modal {
    LaunchSwarm {
        goal: TextArea,
        harness_idx: usize,
        max_workers: usize,
        isolation: TaskIsolation,
        prewarm_workers: usize,
        field: LaunchField,
    },
    NewWorker {
        title: TextArea,
        instructions: TextArea,
        files: TextArea,
        harness_idx: usize,
        agent: TextArea,
        isolation: TaskIsolation,
        field: WorkerField,
    },
    NewTask {
        title: TextArea,
        instructions: TextArea,
        files: TextArea,
        isolation: TaskIsolation,
        field: TaskField,
    },
    EditTask {
        task_id: String,
        title: TextArea,
        instructions: TextArea,
        files: TextArea,
        field: TaskField,
    },
    ComposeMessage {
        to_launch: Option<String>,
        to_label: String,
        topic_idx: usize,
        body: TextArea,
        urgent: bool,
        wake: bool,
        field: ComposeField,
    },
    /// One message in full: the feed shows a line of it; `Enter` opens this.
    ViewMessage { message: BusMessage, scroll: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    /// `r` while a coordinator is live: a second launch would replace it.
    RelaunchSwarm {
        coordinator: String,
    },
    ReleaseLease {
        pattern: String,
    },
    CancelTask {
        task_id: String,
        title: String,
    },
    StopSession {
        session_idx: usize,
        label: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    None,
    Close,
    Reload,
    AttachSession(usize),
    StartSwarm {
        goal: String,
        harness: Harness,
        max_workers: usize,
        isolation: TaskIsolation,
        prewarm_workers: usize,
    },
    PromoteCoordinator {
        launch_id: String,
    },
    SpawnWorker {
        title: String,
        instructions: String,
        files: Vec<String>,
        harness: Harness,
        agent: Option<String>,
        isolation: TaskIsolation,
    },
    DelegateTask {
        task_id: String,
    },
    WakeAgent {
        launch_id: String,
    },
    CreateTask {
        title: String,
        instructions: String,
        files: Vec<String>,
        isolation: TaskIsolation,
    },
    UpdateTask {
        task_id: String,
        title: String,
        instructions: String,
        files: Vec<String>,
    },
    CancelTask(String),
    ReleaseLease(String),
    StopSession(usize),
    SendMessage {
        to_launch: Option<String>,
        topic: String,
        body: String,
        urgent: bool,
        wake: bool,
    },
}

#[derive(Debug)]
pub struct SwarmView {
    pub workspace: PathBuf,
    pub snapshot: SwarmSnapshot,
    pub pane: SwarmPane,
    pub preview_mode: PreviewMode,
    pub board_mode: BoardMode,
    pub message_filter: MessageFilter,
    pub selected_agent: usize,
    pub selected_task: usize,
    pub selected_lease: usize,
    pub selected_message: usize,
    pub preview_scroll: usize,
    pub coordinator_override: Option<String>,
    pub diff_lines: Vec<String>,
    pub modal: Option<Modal>,
    pub confirm: Option<Confirm>,
    pub status_msg: Option<String>,
    /// The store, held for the screen's lifetime: one connection, not one
    /// per tick.
    conn: Option<Connection>,
    /// The `trace_changes` sequence the snapshot was read at, so a quiet
    /// store costs one indexed `MAX(seq)` per tick instead of the full
    /// read. `None` when the feed could not be read: every tick re-reads.
    last_change_seq: Option<i64>,
    /// What the live sessions looked like at the last read (state, turns,
    /// tokens, running tool), so a session turning idle or finishing a turn
    /// refreshes the roster without a store write.
    live_fingerprint: u64,
    /// When the tick last asked whether a refresh is due.
    last_check: Option<std::time::Instant>,
}

/// How often a tick may ask the store whether anything changed.
const REFRESH_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

fn live_fingerprint(live: &[LiveSwarmSession]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for s in live {
        s.launch_id.hash(&mut h);
        s.state.hash(&mut h);
        s.turns.hash(&mut h);
        s.tokens.hash(&mut h);
        s.running_tool.hash(&mut h);
    }
    h.finish()
}

impl SwarmView {
    /// Opens the screen on `workspace`, reading `conn` (kept for later
    /// refreshes). `now` is in nanoseconds.
    pub fn open(
        workspace: PathBuf,
        conn: Option<Connection>,
        live_sessions: &[LiveSwarmSession],
        now: i64,
    ) -> Self {
        let snapshot = load_snapshot(conn.as_ref(), &workspace, live_sessions, None, now);
        let last_change_seq = conn.as_ref().and_then(Self::change_seq);
        let msg_count = snapshot.messages.len();
        let mut view = Self {
            workspace,
            snapshot,
            pane: SwarmPane::Agents,
            preview_mode: PreviewMode::Terminal,
            board_mode: BoardMode::Tasks,
            message_filter: MessageFilter::All,
            selected_agent: 0,
            selected_task: 0,
            selected_lease: 0,
            selected_message: msg_count.saturating_sub(1),
            preview_scroll: 0,
            coordinator_override: None,
            diff_lines: Vec::new(),
            modal: None,
            confirm: None,
            status_msg: None,
            conn,
            last_change_seq,
            live_fingerprint: live_fingerprint(live_sessions),
            last_check: None,
        };
        view.clamp_selections();
        view
    }

    /// The change feed's sequence, `None` only when it cannot be read. A
    /// store this binary opened has the feed (and, from v18, the bus
    /// triggers), so an empty feed is a quiet store, not a missing one.
    fn change_seq(conn: &Connection) -> Option<i64> {
        crate::tracing::store::latest_change_seq(conn).ok()
    }

    /// Whether the snapshot is stale: the store's change feed advanced (a
    /// message, task, lease or the swarm record), a live session changed
    /// state or telemetry, or a lease the snapshot shows has expired. At
    /// most one store read per [`REFRESH_CHECK_INTERVAL`]; a feed that
    /// cannot be read counts as always changed. `now` is in nanoseconds.
    pub fn refresh_due(
        &mut self,
        live_sessions: &[LiveSwarmSession],
        now: i64,
        tick: std::time::Instant,
    ) -> bool {
        if self
            .last_check
            .is_some_and(|t| tick.duration_since(t) < REFRESH_CHECK_INTERVAL)
        {
            return false;
        }
        self.last_check = Some(tick);
        let fingerprint = live_fingerprint(live_sessions);
        if fingerprint != self.live_fingerprint {
            return true;
        }
        if self.snapshot.leases.iter().any(|l| l.expires_ns <= now) {
            return true;
        }
        let Some(conn) = &self.conn else {
            return false;
        };
        match Self::change_seq(conn) {
            Some(seq) => self.last_change_seq != Some(seq),
            None => true,
        }
    }

    /// Re-reads the store and the live sessions. `now` is in nanoseconds.
    pub fn refresh(&mut self, live_sessions: &[LiveSwarmSession], now: i64) {
        let prev_msg_len = self.filtered_messages().len();
        let was_at_bottom = prev_msg_len == 0 || self.selected_message + 1 >= prev_msg_len;
        self.snapshot = load_snapshot(
            self.conn.as_ref(),
            &self.workspace,
            live_sessions,
            self.coordinator_override.as_deref(),
            now,
        );
        self.last_change_seq = self.conn.as_ref().and_then(Self::change_seq);
        self.live_fingerprint = live_fingerprint(live_sessions);
        self.clamp_selections();
        let new_msg_len = self.filtered_messages().len();
        if was_at_bottom && new_msg_len > 0 {
            self.selected_message = new_msg_len - 1;
        }
        if self.preview_mode == PreviewMode::GitDiff {
            self.refresh_diff();
        }
    }

    pub fn text_entry_active(&self) -> bool {
        self.modal.is_some()
    }

    pub fn confirmation_active(&self) -> bool {
        self.confirm.is_some()
    }

    pub fn selected_member(&self) -> Option<&SwarmMember> {
        let members = self.snapshot.all_members();
        members.get(self.selected_agent).copied()
    }

    pub fn selected_task(&self) -> Option<&BusTask> {
        self.snapshot.tasks.get(self.selected_task)
    }

    pub fn selected_lease(&self) -> Option<&FileLease> {
        self.snapshot.leases.get(self.selected_lease)
    }

    pub fn filtered_messages(&self) -> Vec<&BusMessage> {
        let selected_launch = self.selected_member().map(|m| m.launch_id.as_str());
        self.snapshot
            .messages
            .iter()
            .filter(|m| match self.message_filter {
                MessageFilter::All => true,
                MessageFilter::SelectedAgent => match selected_launch {
                    Some(lid) => {
                        m.from_launch == lid
                            || m.to_launch.as_deref() == Some(lid)
                            || m.to_launch.is_none()
                    }
                    None => true,
                },
                MessageFilter::QuestionsAndUrgent => {
                    m.topic == "question"
                        || m.topic == "blocker"
                        || m.topic == "api-contract"
                        || m.acked_ns.is_none()
                }
            })
            .collect()
    }

    pub fn selected_filtered_message(&self) -> Option<&BusMessage> {
        let msgs = self.filtered_messages();
        msgs.get(self.selected_message).copied()
    }

    fn clamp_selections(&mut self) {
        let member_len = self.snapshot.all_members().len();
        if member_len == 0 {
            self.selected_agent = 0;
        } else if self.selected_agent >= member_len {
            self.selected_agent = member_len - 1;
        }

        let task_len = self.snapshot.tasks.len();
        if task_len == 0 {
            self.selected_task = 0;
        } else if self.selected_task >= task_len {
            self.selected_task = task_len - 1;
        }

        let lease_len = self.snapshot.leases.len();
        if lease_len == 0 {
            self.selected_lease = 0;
        } else if self.selected_lease >= lease_len {
            self.selected_lease = lease_len - 1;
        }

        let msg_len = self.filtered_messages().len();
        if msg_len == 0 {
            self.selected_message = 0;
        } else if self.selected_message >= msg_len {
            self.selected_message = msg_len - 1;
        }
    }

    pub fn refresh_diff(&mut self) {
        let target_dir = self
            .selected_member()
            .and_then(|m| m.worktree.as_deref())
            .map(PathBuf::from)
            .or_else(|| {
                self.selected_task()
                    .and_then(|t| t.worktree.as_deref())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| self.workspace.clone());

        self.diff_lines = compute_git_diff(&target_dir);
    }

    pub fn open_launch_modal(&mut self) {
        let mut goal = TextArea::new("");
        if let Some(existing) = &self.snapshot.goal {
            goal.set(existing);
        }
        self.modal = Some(Modal::LaunchSwarm {
            goal,
            harness_idx: 0,
            max_workers: 3,
            isolation: TaskIsolation::Shared,
            prewarm_workers: 0,
            field: LaunchField::Goal,
        });
    }

    pub fn open_new_worker_modal(&mut self) {
        self.modal = Some(Modal::NewWorker {
            title: TextArea::new(""),
            instructions: TextArea::new(""),
            files: TextArea::new(""),
            harness_idx: 0,
            agent: TextArea::new(""),
            isolation: TaskIsolation::Shared,
            field: WorkerField::Title,
        });
    }

    pub fn open_new_task_modal(&mut self) {
        self.modal = Some(Modal::NewTask {
            title: TextArea::new(""),
            instructions: TextArea::new(""),
            files: TextArea::new(""),
            isolation: TaskIsolation::Shared,
            field: TaskField::Title,
        });
    }

    pub fn open_edit_task_modal(&mut self, task: &BusTask) {
        let mut title = TextArea::new("");
        title.set(&task.title);
        let mut instructions = TextArea::new("");
        instructions.set(&task.instructions);
        let mut files = TextArea::new("");
        files.set(task.files.join(", "));
        self.modal = Some(Modal::EditTask {
            task_id: task.id.clone(),
            title,
            instructions,
            files,
            field: TaskField::Title,
        });
    }

    pub fn open_compose_message(
        &mut self,
        to_launch: Option<String>,
        to_label: String,
        topic_idx: usize,
    ) {
        self.modal = Some(Modal::ComposeMessage {
            to_launch,
            to_label,
            topic_idx,
            body: TextArea::new(""),
            urgent: false,
            wake: true,
            field: ComposeField::Body,
        });
    }

    /// Opens the selected feed message in full.
    pub fn open_view_message(&mut self, message: BusMessage) {
        self.modal = Some(Modal::ViewMessage { message, scroll: 0 });
    }

    /// The composer addressed to a message's sender, with a fitting topic.
    fn open_reply(&mut self, message: &BusMessage) {
        let sender = message.from_launch.clone();
        let label = self.snapshot.member_label(&sender);
        let topic_idx = if message.topic == "question" { 1 } else { 0 };
        self.open_compose_message(Some(sender), label, topic_idx);
    }

    /// The task the preview's Task Detail shows: the board's selection when
    /// the board has focus, else the selected agent's latest task, else the
    /// board's selection.
    pub fn detail_task(&self) -> Option<&BusTask> {
        if self.pane == SwarmPane::Board {
            return self.selected_task();
        }
        let agent_task = self.selected_member().and_then(|m| {
            self.snapshot
                .tasks
                .iter()
                .rev()
                .find(|t| t.assigned_to.as_deref() == Some(m.launch_id.as_str()))
        });
        agent_task.or_else(|| self.selected_task())
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        if self.confirm.is_some() {
            return self.handle_confirm_key(key);
        }
        if self.modal.is_some() {
            return self.handle_modal_key(key);
        }

        self.status_msg = None;

        match (key.code, key.modifiers) {
            (KeyCode::Esc, KeyModifiers::NONE) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
                Outcome::Close
            }
            (KeyCode::Tab, KeyModifiers::NONE) => {
                self.pane = self.pane.next();
                Outcome::None
            }
            (KeyCode::BackTab, _) => {
                self.pane = self.pane.prev();
                Outcome::None
            }
            (KeyCode::Char('1'), KeyModifiers::NONE) => {
                self.pane = SwarmPane::Agents;
                Outcome::None
            }
            (KeyCode::Char('2'), KeyModifiers::NONE) => {
                self.pane = SwarmPane::Preview;
                Outcome::None
            }
            (KeyCode::Char('3'), KeyModifiers::NONE) => {
                self.pane = SwarmPane::Board;
                Outcome::None
            }
            (KeyCode::Char('4'), KeyModifiers::NONE) => {
                self.pane = SwarmPane::Messages;
                Outcome::None
            }
            (KeyCode::Char('r'), m) if m.contains(KeyModifiers::CONTROL) => Outcome::Reload,

            // Vertical navigation in active quadrant
            (KeyCode::Down, KeyModifiers::NONE) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                self.move_cursor(1);
                Outcome::None
            }
            (KeyCode::Up, KeyModifiers::NONE) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                self.move_cursor(-1);
                Outcome::None
            }
            (KeyCode::Char('g'), KeyModifiers::NONE) | (KeyCode::Home, KeyModifiers::NONE) => {
                self.move_to_top();
                Outcome::None
            }
            (KeyCode::Char('G'), KeyModifiers::SHIFT)
            | (KeyCode::Char('G'), KeyModifiers::NONE)
            | (KeyCode::End, KeyModifiers::NONE) => {
                self.move_to_bottom();
                Outcome::None
            }
            (KeyCode::Char('d'), m) if m.contains(KeyModifiers::CONTROL) => {
                self.move_cursor(8);
                Outcome::None
            }
            (KeyCode::Char('u'), m) if m.contains(KeyModifiers::CONTROL) => {
                self.move_cursor(-8);
                Outcome::None
            }

            // Sub-mode toggles (`v`, `l`, `f`, `←/→`)
            (KeyCode::Char('v'), KeyModifiers::NONE) => {
                self.preview_mode = self.preview_mode.next();
                self.preview_scroll = 0;
                if self.preview_mode == PreviewMode::GitDiff {
                    self.refresh_diff();
                }
                Outcome::None
            }
            (KeyCode::Char('l'), KeyModifiers::NONE) => {
                self.board_mode = self.board_mode.toggle();
                Outcome::None
            }
            (KeyCode::Char('f'), KeyModifiers::NONE) => {
                self.message_filter = self.message_filter.next();
                self.clamp_selections();
                Outcome::None
            }
            (KeyCode::Left, KeyModifiers::NONE) | (KeyCode::Char('h'), KeyModifiers::NONE) => {
                match self.pane {
                    SwarmPane::Preview => {
                        self.preview_mode = match self.preview_mode {
                            PreviewMode::Terminal => PreviewMode::GitDiff,
                            PreviewMode::TaskDetail => PreviewMode::Terminal,
                            PreviewMode::GitDiff => PreviewMode::TaskDetail,
                        };
                        self.preview_scroll = 0;
                        if self.preview_mode == PreviewMode::GitDiff {
                            self.refresh_diff();
                        }
                    }
                    SwarmPane::Board => self.board_mode = self.board_mode.toggle(),
                    SwarmPane::Messages => {
                        self.message_filter = self.message_filter.next();
                        self.clamp_selections();
                    }
                    SwarmPane::Agents => {}
                }
                Outcome::None
            }
            (KeyCode::Right, KeyModifiers::NONE) => {
                match self.pane {
                    SwarmPane::Preview => {
                        self.preview_mode = self.preview_mode.next();
                        self.preview_scroll = 0;
                        if self.preview_mode == PreviewMode::GitDiff {
                            self.refresh_diff();
                        }
                    }
                    SwarmPane::Board => self.board_mode = self.board_mode.toggle(),
                    SwarmPane::Messages => {
                        self.message_filter = self.message_filter.next();
                        self.clamp_selections();
                    }
                    SwarmPane::Agents => {}
                }
                Outcome::None
            }

            // `Enter`: Attach to selected agent's live terminal or inspect task
            (KeyCode::Enter, KeyModifiers::NONE) => match self.pane {
                SwarmPane::Agents | SwarmPane::Preview => {
                    if let Some(member) = self.selected_member()
                        && let Some(idx) = member.session_idx
                    {
                        return Outcome::AttachSession(idx);
                    }
                    self.status_msg = Some(
                        "Selected peer does not have a live TUI session in this window.".into(),
                    );
                    Outcome::None
                }
                SwarmPane::Board => {
                    self.preview_mode = PreviewMode::TaskDetail;
                    self.preview_scroll = 0;
                    Outcome::None
                }
                SwarmPane::Messages => {
                    if let Some(msg) = self.selected_filtered_message().cloned() {
                        self.open_view_message(msg);
                    }
                    Outcome::None
                }
            },

            // `r`: Run / Launch Swarm (or delegate selected open task in Board)
            (KeyCode::Char('r'), KeyModifiers::NONE) => {
                if self.pane == SwarmPane::Board
                    && self.board_mode == BoardMode::Tasks
                    && let Some(task) = self.selected_task()
                    && !task.status.is_terminal()
                    && task.assigned_to.is_none()
                {
                    return Outcome::DelegateTask {
                        task_id: task.id.clone(),
                    };
                }
                if let Some(c) = self.snapshot.coordinator.as_ref().filter(|c| c.is_active()) {
                    self.confirm = Some(Confirm::RelaunchSwarm {
                        coordinator: format!("{} ({})", c.label, c.provider),
                    });
                    return Outcome::None;
                }
                self.open_launch_modal();
                Outcome::None
            }

            // `n`: New Worker (in Agents/Preview) or New Task (in Board) or New Message (in Messages)
            (KeyCode::Char('n'), KeyModifiers::NONE) => {
                match self.pane {
                    SwarmPane::Board => self.open_new_task_modal(),
                    SwarmPane::Messages => {
                        self.open_compose_message(None, "* (All Swarm Peers)".into(), 0);
                    }
                    SwarmPane::Agents | SwarmPane::Preview => {
                        if self.snapshot.coordinator.is_none() && self.snapshot.workers.is_empty() {
                            self.open_launch_modal();
                        } else {
                            self.open_new_worker_modal();
                        }
                    }
                }
                Outcome::None
            }

            // `e`: Edit selected task in Board
            (KeyCode::Char('e'), KeyModifiers::NONE) => {
                if let Some(task) = self.selected_task().cloned() {
                    self.open_edit_task_modal(&task);
                } else {
                    self.status_msg = Some("No task on the board to edit.".into());
                }
                Outcome::None
            }

            // `c`: Promote selected agent to Coordinator
            (KeyCode::Char('c'), KeyModifiers::NONE) => {
                if let Some(member) = self.selected_member() {
                    let lid = member.launch_id.clone();
                    self.coordinator_override = Some(lid.clone());
                    return Outcome::PromoteCoordinator { launch_id: lid };
                }
                Outcome::None
            }

            // `w`: Wake selected agent
            (KeyCode::Char('w'), KeyModifiers::NONE) => {
                if let Some(member) = self.selected_member() {
                    return Outcome::WakeAgent {
                        launch_id: member.launch_id.clone(),
                    };
                }
                Outcome::None
            }

            // `m`: Direct message to selected agent (or reply in Messages pane)
            (KeyCode::Char('m'), KeyModifiers::NONE) => {
                if self.pane == SwarmPane::Messages
                    && let Some(msg) = self.selected_filtered_message().cloned()
                {
                    self.open_reply(&msg);
                    return Outcome::None;
                }
                if let Some(member) = self.selected_member() {
                    let lid = member.launch_id.clone();
                    let label = self.snapshot.member_label(&lid);
                    self.open_compose_message(Some(lid), label, 0);
                } else {
                    self.open_compose_message(None, "* (Broadcast to All)".into(), 0);
                }
                Outcome::None
            }

            // `b`: Broadcast message to `*`
            (KeyCode::Char('b'), KeyModifiers::NONE) => {
                self.open_compose_message(None, "* (Broadcast to All)".into(), 0);
                Outcome::None
            }

            // `x` / `d`: Stop agent, cancel task, or force-release lease
            (KeyCode::Char('x'), KeyModifiers::NONE) | (KeyCode::Char('d'), KeyModifiers::NONE) => {
                match self.pane {
                    SwarmPane::Agents | SwarmPane::Preview => {
                        if let Some(member) = self.selected_member()
                            && let Some(idx) = member.session_idx
                            && member.is_active()
                        {
                            self.confirm = Some(Confirm::StopSession {
                                session_idx: idx,
                                label: format!("{} ({})", member.label, member.provider),
                            });
                        }
                        Outcome::None
                    }
                    SwarmPane::Board => match self.board_mode {
                        BoardMode::Tasks => {
                            if let Some(task) = self.selected_task()
                                && !task.status.is_terminal()
                            {
                                self.confirm = Some(Confirm::CancelTask {
                                    task_id: task.id.clone(),
                                    title: task.title.clone(),
                                });
                            }
                            Outcome::None
                        }
                        BoardMode::Leases => {
                            if let Some(lease) = self.selected_lease() {
                                self.confirm = Some(Confirm::ReleaseLease {
                                    pattern: lease.pattern.clone(),
                                });
                            }
                            Outcome::None
                        }
                    },
                    SwarmPane::Messages => Outcome::None,
                }
            }

            _ => Outcome::None,
        }
    }

    /// `y` approves; Enter does not (`docs/keyboard.md`: a destructive
    /// confirmation is never a keypress away from a habit).
    fn handle_confirm_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let Some(confirm) = self.confirm.take() else {
                    return Outcome::None;
                };
                match confirm {
                    Confirm::RelaunchSwarm { .. } => {
                        self.open_launch_modal();
                        Outcome::None
                    }
                    Confirm::ReleaseLease { pattern } => Outcome::ReleaseLease(pattern),
                    Confirm::CancelTask { task_id, .. } => Outcome::CancelTask(task_id),
                    Confirm::StopSession { session_idx, .. } => Outcome::StopSession(session_idx),
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.confirm = None;
                Outcome::None
            }
            _ => Outcome::None,
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) -> Outcome {
        if key.code == KeyCode::Esc {
            self.modal = None;
            return Outcome::None;
        }

        if let Some(Modal::ViewMessage { message, scroll }) = &mut self.modal {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => *scroll += 1,
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                KeyCode::Char('m') | KeyCode::Char('r') | KeyCode::Enter => {
                    let msg = message.clone();
                    self.open_reply(&msg);
                }
                _ => {}
            }
            return Outcome::None;
        }

        // Ctrl+S submits every form; Enter submits from a single-line field
        // and is a newline in a multi-line one (the composer's body too,
        // where Ctrl+Enter submits).
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let submit = (key.code == KeyCode::Char('s') && ctrl)
            || (key.code == KeyCode::Enter
                && ctrl
                && matches!(self.modal, Some(Modal::ComposeMessage { .. })))
            || (key.code == KeyCode::Enter
                && !ctrl
                && !matches!(
                    self.modal,
                    Some(Modal::LaunchSwarm {
                        field: LaunchField::Goal,
                        ..
                    }) | Some(Modal::NewWorker {
                        field: WorkerField::Instructions,
                        ..
                    }) | Some(Modal::NewTask {
                        field: TaskField::Instructions,
                        ..
                    }) | Some(Modal::EditTask {
                        field: TaskField::Instructions,
                        ..
                    }) | Some(Modal::ComposeMessage {
                        field: ComposeField::Body,
                        ..
                    })
                ));

        if submit {
            return self.submit_modal();
        }

        let Some(modal) = &mut self.modal else {
            return Outcome::None;
        };

        match modal {
            Modal::LaunchSwarm {
                goal,
                harness_idx,
                max_workers,
                isolation,
                prewarm_workers,
                field,
            } => match (key.code, key.modifiers) {
                (KeyCode::Tab, KeyModifiers::NONE) => {
                    *field = field.next();
                }
                (KeyCode::BackTab, _) => {
                    *field = field.prev();
                }
                _ => match field {
                    LaunchField::Goal => {
                        crate::keymap::apply_text(goal, &key);
                    }
                    LaunchField::Harness => match key.code {
                        KeyCode::Left | KeyCode::Char('h') => {
                            *harness_idx =
                                (*harness_idx + HARNESS_CYCLE.len() - 1) % HARNESS_CYCLE.len();
                        }
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                            *harness_idx = (*harness_idx + 1) % HARNESS_CYCLE.len();
                        }
                        _ => {}
                    },
                    LaunchField::MaxWorkers => match key.code {
                        KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('-') => {
                            *max_workers = (*max_workers).saturating_sub(1).max(1);
                        }
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('+') => {
                            *max_workers = (*max_workers + 1).min(8);
                        }
                        _ => {}
                    },
                    LaunchField::Isolation => match key.code {
                        KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Char('h')
                        | KeyCode::Char('l')
                        | KeyCode::Char(' ') => {
                            *isolation = match isolation {
                                TaskIsolation::Shared => TaskIsolation::Worktree,
                                TaskIsolation::Worktree => TaskIsolation::Shared,
                            };
                        }
                        _ => {}
                    },
                    LaunchField::PrewarmWorkers => match key.code {
                        KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('-') => {
                            *prewarm_workers = prewarm_workers.saturating_sub(1);
                        }
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('+') => {
                            *prewarm_workers = (*prewarm_workers + 1).min(*max_workers);
                        }
                        _ => {}
                    },
                },
            },

            Modal::NewWorker {
                title,
                instructions,
                files,
                harness_idx,
                agent,
                isolation,
                field,
            } => match (key.code, key.modifiers) {
                (KeyCode::Tab, KeyModifiers::NONE) => *field = field.next(),
                (KeyCode::BackTab, _) => *field = field.prev(),
                _ => match field {
                    WorkerField::Title => {
                        crate::keymap::apply_text(title, &key);
                    }
                    WorkerField::Instructions => {
                        crate::keymap::apply_text(instructions, &key);
                    }
                    WorkerField::Files => {
                        crate::keymap::apply_text(files, &key);
                    }
                    WorkerField::Harness => match key.code {
                        KeyCode::Left | KeyCode::Char('h') => {
                            *harness_idx =
                                (*harness_idx + HARNESS_CYCLE.len() - 1) % HARNESS_CYCLE.len();
                        }
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                            *harness_idx = (*harness_idx + 1) % HARNESS_CYCLE.len();
                        }
                        _ => {}
                    },
                    WorkerField::Agent => {
                        crate::keymap::apply_text(agent, &key);
                    }
                    WorkerField::Isolation => match key.code {
                        KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Char('h')
                        | KeyCode::Char('l')
                        | KeyCode::Char(' ') => {
                            *isolation = match isolation {
                                TaskIsolation::Shared => TaskIsolation::Worktree,
                                TaskIsolation::Worktree => TaskIsolation::Shared,
                            };
                        }
                        _ => {}
                    },
                },
            },

            Modal::NewTask {
                title,
                instructions,
                files,
                isolation,
                field,
            } => match (key.code, key.modifiers) {
                (KeyCode::Tab, KeyModifiers::NONE) => *field = field.next(true),
                (KeyCode::BackTab, _) => *field = field.prev(true),
                _ => match field {
                    TaskField::Title => {
                        crate::keymap::apply_text(title, &key);
                    }
                    TaskField::Instructions => {
                        crate::keymap::apply_text(instructions, &key);
                    }
                    TaskField::Files => {
                        crate::keymap::apply_text(files, &key);
                    }
                    TaskField::Isolation => match key.code {
                        KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Char('h')
                        | KeyCode::Char('l')
                        | KeyCode::Char(' ') => {
                            *isolation = match isolation {
                                TaskIsolation::Shared => TaskIsolation::Worktree,
                                TaskIsolation::Worktree => TaskIsolation::Shared,
                            };
                        }
                        _ => {}
                    },
                },
            },

            Modal::EditTask {
                title,
                instructions,
                files,
                field,
                ..
            } => match (key.code, key.modifiers) {
                (KeyCode::Tab, KeyModifiers::NONE) => *field = field.next(false),
                (KeyCode::BackTab, _) => *field = field.prev(false),
                _ => match field {
                    TaskField::Title => {
                        crate::keymap::apply_text(title, &key);
                    }
                    TaskField::Instructions => {
                        crate::keymap::apply_text(instructions, &key);
                    }
                    TaskField::Files | TaskField::Isolation => {
                        crate::keymap::apply_text(files, &key);
                    }
                },
            },

            Modal::ComposeMessage {
                topic_idx,
                body,
                urgent,
                wake,
                field,
                ..
            } => match (key.code, key.modifiers) {
                (KeyCode::Tab, KeyModifiers::NONE) => *field = field.next(),
                (KeyCode::BackTab, _) => *field = field.prev(),
                _ => match field {
                    ComposeField::Body => {
                        if key.code == KeyCode::Enter {
                            body.newline();
                        } else {
                            crate::keymap::apply_text(body, &key);
                        }
                    }
                    ComposeField::Topic => match key.code {
                        KeyCode::Left | KeyCode::Char('h') => {
                            *topic_idx =
                                (*topic_idx + MESSAGE_TOPICS.len() - 1) % MESSAGE_TOPICS.len();
                        }
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                            *topic_idx = (*topic_idx + 1) % MESSAGE_TOPICS.len();
                        }
                        _ => {}
                    },
                    ComposeField::Urgent => {
                        if matches!(
                            key.code,
                            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter
                        ) {
                            *urgent = !*urgent;
                        }
                    }
                    ComposeField::Wake => {
                        if matches!(
                            key.code,
                            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter
                        ) {
                            *wake = !*wake;
                        }
                    }
                },
            },
            Modal::ViewMessage { .. } => {}
        }

        Outcome::None
    }

    fn submit_modal(&mut self) -> Outcome {
        let Some(modal) = self.modal.take() else {
            return Outcome::None;
        };

        match modal {
            Modal::LaunchSwarm {
                goal,
                harness_idx,
                max_workers,
                isolation,
                prewarm_workers,
                ..
            } => {
                let goal_text = goal.text.trim().to_string();
                if goal_text.is_empty() {
                    self.status_msg = Some("Swarm goal cannot be empty.".into());
                    return Outcome::None;
                }
                Outcome::StartSwarm {
                    goal: goal_text,
                    harness: HARNESS_CYCLE[harness_idx % HARNESS_CYCLE.len()],
                    max_workers,
                    isolation,
                    prewarm_workers,
                }
            }
            Modal::NewWorker {
                title,
                instructions,
                files,
                harness_idx,
                agent,
                isolation,
                ..
            } => {
                let t = title.text.trim().to_string();
                if t.is_empty() {
                    self.status_msg = Some("Worker task title cannot be empty.".into());
                    return Outcome::None;
                }
                let file_list = parse_csv_files(&files.text);
                let agent_opt = {
                    let a = agent.text.trim().to_string();
                    if a.is_empty() { None } else { Some(a) }
                };
                Outcome::SpawnWorker {
                    title: t,
                    instructions: instructions.text.trim().to_string(),
                    files: file_list,
                    harness: HARNESS_CYCLE[harness_idx % HARNESS_CYCLE.len()],
                    agent: agent_opt,
                    isolation,
                }
            }
            Modal::NewTask {
                title,
                instructions,
                files,
                isolation,
                ..
            } => {
                let t = title.text.trim().to_string();
                if t.is_empty() {
                    self.status_msg = Some("Task title cannot be empty.".into());
                    return Outcome::None;
                }
                Outcome::CreateTask {
                    title: t,
                    instructions: instructions.text.trim().to_string(),
                    files: parse_csv_files(&files.text),
                    isolation,
                }
            }
            Modal::EditTask {
                task_id,
                title,
                instructions,
                files,
                ..
            } => {
                let t = title.text.trim().to_string();
                if t.is_empty() {
                    self.status_msg = Some("Task title cannot be empty.".into());
                    return Outcome::None;
                }
                Outcome::UpdateTask {
                    task_id,
                    title: t,
                    instructions: instructions.text.trim().to_string(),
                    files: parse_csv_files(&files.text),
                }
            }
            Modal::ViewMessage { .. } => Outcome::None,
            Modal::ComposeMessage {
                to_launch,
                topic_idx,
                body,
                urgent,
                wake,
                ..
            } => {
                let b = body.text.trim().to_string();
                if b.is_empty() {
                    self.status_msg = Some("Message body cannot be empty.".into());
                    return Outcome::None;
                }
                Outcome::SendMessage {
                    to_launch,
                    topic: MESSAGE_TOPICS[topic_idx % MESSAGE_TOPICS.len()].to_string(),
                    body: b,
                    urgent,
                    wake,
                }
            }
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        match self.pane {
            SwarmPane::Agents => {
                let len = self.snapshot.all_members().len();
                self.selected_agent = step_index(self.selected_agent, len, delta);
                if self.preview_mode == PreviewMode::GitDiff {
                    self.refresh_diff();
                }
            }
            SwarmPane::Preview => {
                if delta < 0 {
                    self.preview_scroll = self.preview_scroll.saturating_sub(delta.unsigned_abs());
                } else {
                    self.preview_scroll = self.preview_scroll.saturating_add(delta as usize);
                }
            }
            SwarmPane::Board => match self.board_mode {
                BoardMode::Tasks => {
                    let len = self.snapshot.tasks.len();
                    self.selected_task = step_index(self.selected_task, len, delta);
                }
                BoardMode::Leases => {
                    let len = self.snapshot.leases.len();
                    self.selected_lease = step_index(self.selected_lease, len, delta);
                }
            },
            SwarmPane::Messages => {
                let len = self.filtered_messages().len();
                self.selected_message = step_index(self.selected_message, len, delta);
            }
        }
    }

    fn move_to_top(&mut self) {
        match self.pane {
            SwarmPane::Agents => self.selected_agent = 0,
            SwarmPane::Preview => self.preview_scroll = 0,
            SwarmPane::Board => match self.board_mode {
                BoardMode::Tasks => self.selected_task = 0,
                BoardMode::Leases => self.selected_lease = 0,
            },
            SwarmPane::Messages => self.selected_message = 0,
        }
    }

    fn move_to_bottom(&mut self) {
        match self.pane {
            SwarmPane::Agents => {
                let len = self.snapshot.all_members().len();
                self.selected_agent = len.saturating_sub(1);
            }
            SwarmPane::Preview => {
                self.preview_scroll = 500;
            }
            SwarmPane::Board => match self.board_mode {
                BoardMode::Tasks => {
                    let len = self.snapshot.tasks.len();
                    self.selected_task = len.saturating_sub(1);
                }
                BoardMode::Leases => {
                    let len = self.snapshot.leases.len();
                    self.selected_lease = len.saturating_sub(1);
                }
            },
            SwarmPane::Messages => {
                let len = self.filtered_messages().len();
                self.selected_message = len.saturating_sub(1);
            }
        }
    }
}

fn step_index(cur: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = (cur as isize + delta).clamp(0, len as isize - 1);
    next as usize
}

fn parse_csv_files(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn compute_git_diff(dir: &Path) -> Vec<String> {
    let stat_out = Command::new("git")
        .args([
            "-C",
            &dir.to_string_lossy(),
            "status",
            "--short",
            "--branch",
        ])
        .output();
    let diff_out = Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "diff", "--no-color"])
        .output();

    let mut lines = Vec::new();
    lines.push(format!("Directory: {}", dir.display()));
    if let Ok(out) = stat_out
        && out.status.success()
    {
        for l in String::from_utf8_lossy(&out.stdout).lines().take(30) {
            lines.push(l.to_string());
        }
    }
    lines.push(String::new());
    if let Ok(out) = diff_out
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        if text.trim().is_empty() {
            lines.push("(No unstaged git diff changes in working tree)".into());
        } else {
            for l in text.lines().take(250) {
                lines.push(l.to_string());
            }
        }
    } else {
        lines.push("(Unable to read git diff in this folder)".into());
    }
    lines
}

/// Formats a task's detail card for `PreviewMode::TaskDetail`.
pub fn format_task_detail_lines(task: &BusTask, snap: &SwarmSnapshot) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!("Task [{}] — {}", short_id(&task.id), task.title));
    out.push(format!(
        "Status: {}   Isolation: {}",
        task.status.as_str(),
        task.isolation.as_str()
    ));
    out.push(format!(
        "Created by: {}   Assigned to: {}",
        snap.member_label(&task.created_by),
        task.assigned_to
            .as_deref()
            .map(|a| snap.member_label(a))
            .unwrap_or_else(|| "(unclaimed)".into())
    ));
    if !task.files.is_empty() {
        out.push(format!("Files / Leases: {}", task.files.join(", ")));
    }
    if let Some(parent) = &task.parent_task_id {
        out.push(format!("Parent task: {}", short_id(parent)));
    }
    if let Some(wt) = &task.worktree {
        out.push(format!(
            "Worktree: {} ({})",
            wt,
            task.branch.as_deref().unwrap_or("branch")
        ));
    }
    if !task.instructions.trim().is_empty() {
        out.push(String::new());
        out.push("── Instructions ──".into());
        for line in task.instructions.lines() {
            out.push(line.to_string());
        }
    }
    if let Some(err) = &task.error
        && !err.trim().is_empty()
    {
        out.push(String::new());
        out.push("── Error ──".into());
        for line in err.lines() {
            out.push(line.to_string());
        }
    }
    if let Some(res) = &task.result {
        let res_str = match res {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        if !res_str.trim().is_empty() {
            out.push(String::new());
            out.push("── Result / Output ──".into());
            for line in res_str.lines() {
                out.push(line.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{NewTaskSpec, store as bstore};
    use crate::tracing::pricing::PriceTable;
    use crate::tracing::store::{OpenOptions, open_aux, open_rw};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn swarm_view_quadrant_navigation_and_modals() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let _ = open_rw(
            &db,
            OpenOptions {
                prices: PriceTable::builtin(),
                run_id: "r1".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();
        let conn = open_aux(&db).unwrap();
        let ws = bstore::normalize_workspace(dir.path());
        bstore::split_tasks(
            &conn,
            Some(&db),
            &ws,
            "user",
            &[NewTaskSpec {
                title: "Write API".into(),
                instructions: "Build endpoints".into(),
                files: vec!["src/api.rs".into()],
                ..Default::default()
            }],
            1_000,
        )
        .unwrap();

        let mut view = SwarmView::open(
            dir.path().to_path_buf(),
            Some(open_aux(&db).unwrap()),
            &[],
            2_000,
        );
        assert_eq!(view.pane, SwarmPane::Agents);
        assert_eq!(view.snapshot.tasks.len(), 1);

        // Number keys 1..4 switch quadrants
        view.handle_key(key(KeyCode::Char('3')));
        assert_eq!(view.pane, SwarmPane::Board);
        view.handle_key(key(KeyCode::Char('4')));
        assert_eq!(view.pane, SwarmPane::Messages);
        view.handle_key(key(KeyCode::Tab));
        assert_eq!(view.pane, SwarmPane::Agents);

        // `r` opens LaunchSwarm modal
        view.handle_key(key(KeyCode::Char('r')));
        assert!(view.text_entry_active());
        assert!(matches!(view.modal, Some(Modal::LaunchSwarm { .. })));

        // Esc closes modal
        view.handle_key(key(KeyCode::Esc));
        assert!(!view.text_entry_active());

        // `b` opens broadcast message composer
        view.handle_key(key(KeyCode::Char('b')));
        assert!(matches!(
            view.modal,
            Some(Modal::ComposeMessage {
                to_launch: None,
                ..
            })
        ));
        view.handle_key(key(KeyCode::Char('H')));
        view.handle_key(key(KeyCode::Char('i')));
        let out = view.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(
            out,
            Outcome::SendMessage {
                to_launch: None,
                topic: "directive".into(),
                body: "Hi".into(),
                urgent: false,
                wake: true,
            }
        );
    }

    fn store() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let _ = open_rw(
            &db,
            OpenOptions {
                prices: PriceTable::builtin(),
                run_id: "r1".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();
        (dir, db)
    }

    fn coordinator_live(ws: &str, state: &str) -> LiveSwarmSession {
        LiveSwarmSession {
            session_idx: 0,
            session_id: 1,
            launch_id: "coord-1".into(),
            provider: "claude".into(),
            profile_name: "Claude Code".into(),
            agent_persona: Some("coordinator".into()),
            state: state.into(),
            workspace: ws.into(),
            is_delegated_worker: false,
            turns: 1,
            tokens: 10,
            cost_usd: 0.0,
            running_tool: None,
        }
    }

    #[test]
    fn a_confirmation_takes_y_not_enter() {
        let (dir, db) = store();
        let conn = open_aux(&db).unwrap();
        let ws = bstore::normalize_workspace(dir.path());
        bstore::split_tasks(
            &conn,
            Some(&db),
            &ws,
            "user",
            &[NewTaskSpec {
                title: "t".into(),
                instructions: "i".into(),
                ..Default::default()
            }],
            1_000,
        )
        .unwrap();
        let mut view = SwarmView::open(
            dir.path().to_path_buf(),
            Some(open_aux(&db).unwrap()),
            &[],
            2_000,
        );
        view.pane = SwarmPane::Board;
        view.handle_key(key(KeyCode::Char('x')));
        assert!(matches!(view.confirm, Some(Confirm::CancelTask { .. })));
        assert_eq!(view.handle_key(key(KeyCode::Enter)), Outcome::None);
        assert!(view.confirm.is_some(), "Enter does not approve");
        assert!(matches!(
            view.handle_key(key(KeyCode::Char('y'))),
            Outcome::CancelTask(_)
        ));
        assert!(view.confirm.is_none());
    }

    #[test]
    fn r_with_a_live_coordinator_asks_before_relaunching() {
        let (dir, db) = store();
        let ws = bstore::normalize_workspace(dir.path());
        let live = vec![coordinator_live(&ws, "working")];
        let mut view = SwarmView::open(
            dir.path().to_path_buf(),
            Some(open_aux(&db).unwrap()),
            &live,
            2_000,
        );
        assert!(view.snapshot.coordinator.is_some());
        view.handle_key(key(KeyCode::Char('r')));
        assert!(matches!(view.confirm, Some(Confirm::RelaunchSwarm { .. })));
        assert!(view.modal.is_none());
        view.handle_key(key(KeyCode::Esc));
        assert!(view.confirm.is_none() && view.modal.is_none());
        view.handle_key(key(KeyCode::Char('r')));
        view.handle_key(key(KeyCode::Char('y')));
        assert!(matches!(view.modal, Some(Modal::LaunchSwarm { .. })));

        // An exited coordinator is no swarm: `r` launches directly.
        let live = vec![coordinator_live(&ws, "exited")];
        let mut view = SwarmView::open(
            dir.path().to_path_buf(),
            Some(open_aux(&db).unwrap()),
            &live,
            2_000,
        );
        view.handle_key(key(KeyCode::Char('r')));
        assert!(matches!(view.modal, Some(Modal::LaunchSwarm { .. })));
    }

    #[test]
    fn the_composer_keeps_text_keys_for_the_body_and_tabs_to_its_options() {
        let (dir, db) = store();
        let mut view = SwarmView::open(
            dir.path().to_path_buf(),
            Some(open_aux(&db).unwrap()),
            &[],
            2_000,
        );
        view.handle_key(key(KeyCode::Char('b')));
        for c in "one two".chars() {
            view.handle_key(key(KeyCode::Char(c)));
        }
        // Ctrl+W deletes a word in the body, Enter adds a line.
        view.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        view.handle_key(key(KeyCode::Enter));
        view.handle_key(key(KeyCode::Char('x')));
        match &view.modal {
            Some(Modal::ComposeMessage { body, wake, .. }) => {
                assert_eq!(body.text, "one \nx");
                assert!(*wake);
            }
            other => panic!("{other:?}"),
        }
        // Tab reaches the options; Space changes them.
        view.handle_key(key(KeyCode::Tab));
        view.handle_key(key(KeyCode::Char(' ')));
        view.handle_key(key(KeyCode::Tab));
        view.handle_key(key(KeyCode::Char(' ')));
        view.handle_key(key(KeyCode::Tab));
        view.handle_key(key(KeyCode::Char(' ')));
        match &view.modal {
            Some(Modal::ComposeMessage {
                topic_idx,
                urgent,
                wake,
                field,
                ..
            }) => {
                assert_eq!(*topic_idx, 1);
                assert!(*urgent);
                assert!(!*wake);
                assert_eq!(*field, ComposeField::Wake);
            }
            other => panic!("{other:?}"),
        }
        // Ctrl+Enter sends from any field.
        let out = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        assert_eq!(
            out,
            Outcome::SendMessage {
                to_launch: None,
                topic: "answer".into(),
                body: "one \nx".into(),
                urgent: true,
                wake: false,
            }
        );
    }
}
