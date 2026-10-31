//! Swarm Mission Control (`Mode::Swarm`).
//!
//! One workspace's swarm as lanes, an agent and the task it holds
//! (`bus::swarm::Lane`), beside a focus panel on the selected lane with four
//! tabs: Live (its terminal), Thread (its messages), Changes (its diff) and
//! Task (its task card), under the Needs-you strip (`bus::swarm::Need`:
//! what only the operator can resolve, `!` to act). `a` swaps the panel for
//! the whole swarm's Activity (messages and task and lease events).
//! Design: `docs/superpowers/specs/2026-10-30-swarm-mission-control-ux-design.md`.

use crate::app::text_area::TextArea;
use crate::bus::swarm::{
    ActivityItem, Lane, LaneKind, LaneState, LiveSwarmSession, Need, NeedKind, SwarmSnapshot,
    load_snapshot,
};
use crate::bus::{BusMessage, BusTask, TaskIsolation, short_id};
use crate::harness::Harness;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const HARNESS_CYCLE: [Harness; 3] = [Harness::Claude, Harness::Codex, Harness::Antigravity];

/// Which side of the screen has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Lanes,
    Panel,
    /// The Needs-you strip (`!`).
    Needs,
}

/// The focus panel's tabs, `1`–`4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelTab {
    Live,
    Thread,
    Changes,
    Task,
}

impl PanelTab {
    pub const ALL: [PanelTab; 4] = [
        PanelTab::Live,
        PanelTab::Thread,
        PanelTab::Changes,
        PanelTab::Task,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PanelTab::Live => "Live",
            PanelTab::Thread => "Thread",
            PanelTab::Changes => "Changes",
            PanelTab::Task => "Task",
        }
    }

    /// The tab a lane opens on: what its state calls for.
    pub fn for_lane(lane: &Lane) -> PanelTab {
        match lane.state {
            LaneState::Waiting => PanelTab::Thread,
            LaneState::Failed | LaneState::Lost | LaneState::Queued => PanelTab::Task,
            LaneState::Done
                if lane
                    .task
                    .as_ref()
                    .is_some_and(|t| t.isolation == TaskIsolation::Worktree) =>
            {
                PanelTab::Changes
            }
            _ if lane.session_idx.is_none() => PanelTab::Thread,
            _ => PanelTab::Live,
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
        /// The message this answers, when it is a reply.
        reply_to: Option<String>,
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
    ReleaseLeases {
        lane: String,
        patterns: Vec<String>,
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
    /// Re-open a failed or abandoned task and delegate it again.
    RetryTask {
        task_id: String,
    },
    WakeAgent {
        launch_id: String,
    },
    UpdateTask {
        task_id: String,
        title: String,
        instructions: String,
        files: Vec<String>,
    },
    CancelTask(String),
    ReleaseLeases(Vec<String>),
    StopSession(usize),
    SendMessage {
        to_launch: Option<String>,
        reply_to: Option<String>,
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
    /// The lanes as of the last read (`SwarmSnapshot::lanes`).
    pub lanes: Vec<Lane>,
    pub selected_lane: usize,
    pub focus: Focus,
    /// A tab the operator chose, for the lane it was chosen on; any other
    /// lane opens on its own default (`PanelTab::for_lane`).
    pub chosen_tab: Option<(String, PanelTab)>,
    /// The panel shows the whole swarm's messages instead of one lane.
    pub activity: bool,
    /// The chosen message in Thread, or line in Activity.
    pub selected_message: usize,
    /// What needs the operator, less what they dismissed (`d`).
    pub needs: Vec<Need>,
    pub selected_need: usize,
    /// Dismissed needs by key, for this screen's lifetime: a dismissed
    /// failure comes back if the task fails again (a new key).
    dismissed: std::collections::HashSet<String>,
    pub panel_scroll: usize,
    pub coordinator_override: Option<String>,
    pub diff_lines: Vec<String>,
    /// The lane `diff_lines` was read for.
    diff_for: Option<String>,
    pub modal: Option<Modal>,
    pub confirm: Option<Confirm>,
    pub status_msg: Option<String>,
    /// The time of the last read, in nanoseconds (lane ages).
    pub now_ns: i64,
    /// The store, held for the screen's lifetime: one connection, not one
    /// per tick.
    conn: Option<Connection>,
    /// The `trace_changes` sequence the snapshot was read at, so a quiet
    /// store costs one indexed `MAX(seq)` per tick instead of the full
    /// read. `None` when the feed could not be read: every tick re-reads.
    last_change_seq: Option<i64>,
    /// What the live sessions looked like at the last read (state, turns,
    /// tokens, running tool), so a session turning idle or finishing a turn
    /// refreshes the lanes without a store write.
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
        // Crossing the stuck threshold is a change worth a refresh; the
        // quiet time itself moves every tick and is not.
        s.quiet_ns
            .is_some_and(|q| q >= crate::bus::swarm::STUCK_AFTER_NS)
            .hash(&mut h);
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
        let lanes = snapshot.lanes(now);
        let mut view = Self {
            workspace,
            snapshot,
            lanes,
            selected_lane: 0,
            focus: Focus::Lanes,
            chosen_tab: None,
            activity: false,
            selected_message: 0,
            needs: Vec::new(),
            selected_need: 0,
            dismissed: std::collections::HashSet::new(),
            panel_scroll: 0,
            coordinator_override: None,
            diff_lines: Vec::new(),
            diff_for: None,
            modal: None,
            confirm: None,
            status_msg: None,
            now_ns: now,
            conn,
            last_change_seq,
            live_fingerprint: live_fingerprint(live_sessions),
            last_check: None,
        };
        view.recompute_needs();
        view.selected_message = view.list_len().saturating_sub(1);
        view.after_lane_change();
        view
    }

    fn recompute_needs(&mut self) {
        self.needs = self
            .snapshot
            .needs(&self.lanes, self.now_ns)
            .into_iter()
            .filter(|n| !self.dismissed.contains(&n.key))
            .collect();
        self.selected_need = self.selected_need.min(self.needs.len().saturating_sub(1));
        if self.focus == Focus::Needs && self.needs.is_empty() {
            self.focus = Focus::Lanes;
        }
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

    /// Re-reads the store and the live sessions, keeping the selected lane
    /// by its key and the message cursor on the newest message when it was
    /// there. `now` is in nanoseconds.
    pub fn refresh(&mut self, live_sessions: &[LiveSwarmSession], now: i64) {
        let key = self.selected().map(|l| l.key.clone());
        let prev_len = self.list_len();
        let was_at_bottom = prev_len == 0 || self.selected_message + 1 >= prev_len;
        self.snapshot = load_snapshot(
            self.conn.as_ref(),
            &self.workspace,
            live_sessions,
            self.coordinator_override.as_deref(),
            now,
        );
        self.now_ns = now;
        self.lanes = self.snapshot.lanes(now);
        self.last_change_seq = self.conn.as_ref().and_then(Self::change_seq);
        self.live_fingerprint = live_fingerprint(live_sessions);
        if let Some(k) = key
            && let Some(i) = self.lanes.iter().position(|l| l.key == k)
        {
            self.selected_lane = i;
        }
        self.selected_lane = self.selected_lane.min(self.lanes.len().saturating_sub(1));
        self.recompute_needs();
        let len = self.list_len();
        if was_at_bottom || self.selected_message >= len {
            self.selected_message = len.saturating_sub(1);
        }
        if self.tab() == PanelTab::Changes {
            self.diff_for = None;
            self.load_diff();
        }
    }

    pub fn text_entry_active(&self) -> bool {
        self.modal.is_some()
    }

    pub fn confirmation_active(&self) -> bool {
        self.confirm.is_some()
    }

    pub fn selected(&self) -> Option<&Lane> {
        self.lanes.get(self.selected_lane)
    }

    /// The panel's tab: the one the operator chose on this lane, else the
    /// lane's default.
    pub fn tab(&self) -> PanelTab {
        let Some(lane) = self.selected() else {
            return PanelTab::Live;
        };
        match &self.chosen_tab {
            Some((key, tab)) if *key == lane.key => *tab,
            _ => PanelTab::for_lane(lane),
        }
    }

    /// The selected lane's thread: its messages, those to it, and broadcasts.
    pub fn panel_messages(&self) -> Vec<&BusMessage> {
        let Some(launch) = self.selected().and_then(|l| l.launch_id.as_deref()) else {
            return Vec::new();
        };
        self.snapshot
            .messages
            .iter()
            .filter(|m| {
                m.from_launch == launch
                    || m.to_launch.as_deref() == Some(launch)
                    || m.to_launch.is_none()
            })
            .collect()
    }

    /// The swarm's Activity: messages with task and lease events.
    pub fn activity_items(&self) -> Vec<ActivityItem<'_>> {
        self.snapshot.timeline()
    }

    /// How many lines the panel's list has: Activity's, else the thread's.
    fn list_len(&self) -> usize {
        if self.activity {
            self.activity_items().len()
        } else {
            self.panel_messages().len()
        }
    }

    /// The chosen message, in the thread or in Activity (an event is none).
    pub fn selected_message(&self) -> Option<&BusMessage> {
        if self.activity {
            return match self.activity_items().get(self.selected_message) {
                Some(ActivityItem::Message(m)) => Some(*m),
                _ => None,
            };
        }
        self.panel_messages().get(self.selected_message).copied()
    }

    pub fn selected_need(&self) -> Option<&Need> {
        self.needs.get(self.selected_need)
    }

    /// Selects the lane with `key`, if it is on screen.
    fn select_lane(&mut self, key: &str) -> bool {
        match self.lanes.iter().position(|l| l.key == key) {
            Some(i) => {
                if i != self.selected_lane {
                    self.selected_lane = i;
                    self.after_lane_change();
                }
                true
            }
            None => false,
        }
    }

    /// The lane changed (or the screen opened): back to the lane's default
    /// tab, the newest message and the top of the panel.
    fn after_lane_change(&mut self) {
        self.panel_scroll = 0;
        self.selected_message = self.list_len().saturating_sub(1);
        if self.tab() == PanelTab::Changes {
            self.load_diff();
        }
    }

    fn set_tab(&mut self, tab: PanelTab) {
        let Some(key) = self.selected().map(|l| l.key.clone()) else {
            return;
        };
        self.activity = false;
        self.chosen_tab = Some((key, tab));
        self.panel_scroll = 0;
        self.selected_message = self.list_len().saturating_sub(1);
        if tab == PanelTab::Changes {
            self.load_diff();
        }
    }

    /// Reads the selected lane's diff, once per lane: its worktree, or the
    /// workspace for a shared lane.
    fn load_diff(&mut self) {
        let Some(lane) = self.selected() else {
            return;
        };
        if self.diff_for.as_deref() == Some(lane.key.as_str()) {
            return;
        }
        let dir = lane
            .task
            .as_ref()
            .and_then(|t| t.worktree.as_deref())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.workspace.clone());
        self.diff_for = Some(lane.key.clone());
        self.diff_lines = compute_git_diff(&dir);
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
        self.open_compose(to_launch, to_label, None, topic_idx);
    }

    fn open_compose(
        &mut self,
        to_launch: Option<String>,
        to_label: String,
        reply_to: Option<String>,
        topic_idx: usize,
    ) {
        self.modal = Some(Modal::ComposeMessage {
            to_launch,
            to_label,
            reply_to,
            topic_idx,
            body: TextArea::new(""),
            urgent: false,
            wake: true,
            field: ComposeField::Body,
        });
    }

    /// Opens a message in full.
    pub fn open_view_message(&mut self, message: BusMessage) {
        self.modal = Some(Modal::ViewMessage { message, scroll: 0 });
    }

    /// The composer addressed to a message's sender, answering it, with a
    /// fitting topic.
    fn open_reply(&mut self, message: &BusMessage) {
        let sender = message.from_launch.clone();
        let label = self.snapshot.member_label(&sender);
        let topic_idx = if message.topic == "question" { 1 } else { 0 };
        self.open_compose(Some(sender), label, Some(message.id.clone()), topic_idx);
    }

    /// The launch form, or a confirmation first when a coordinator is live
    /// (a second launch would replace it).
    fn start_swarm(&mut self) {
        if let Some(c) = self.snapshot.coordinator.as_ref().filter(|c| c.is_active()) {
            self.confirm = Some(Confirm::RelaunchSwarm {
                coordinator: c.label.clone(),
            });
        } else {
            self.open_launch_modal();
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        if self.confirm.is_some() {
            return self.handle_confirm_key(key);
        }
        if self.modal.is_some() {
            return self.handle_modal_key(key);
        }

        self.status_msg = None;
        if self.focus == Focus::Needs
            && let Some(out) = self.handle_needs_key(key)
        {
            return out;
        }
        let lanes_focused = self.focus == Focus::Lanes;
        let lists_messages = self.activity || self.tab() == PanelTab::Thread;

        match (key.code, key.modifiers) {
            (KeyCode::Esc, KeyModifiers::NONE) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
                if lanes_focused {
                    return Outcome::Close;
                }
                self.focus = Focus::Lanes;
            }
            (KeyCode::Tab, KeyModifiers::NONE) | (KeyCode::BackTab, _) => {
                self.focus = match self.focus {
                    Focus::Lanes => Focus::Panel,
                    Focus::Panel | Focus::Needs => Focus::Lanes,
                };
            }
            (KeyCode::Char('!'), _) => {
                if self.needs.is_empty() {
                    self.status_msg = Some("Nothing needs you.".into());
                } else {
                    self.focus = Focus::Needs;
                    self.selected_need = 0;
                }
            }
            (KeyCode::Char(c @ '1'..='4'), KeyModifiers::NONE) => {
                let tab = PanelTab::ALL[(c as usize) - ('1' as usize)];
                self.set_tab(tab);
            }
            (KeyCode::Char('a'), KeyModifiers::NONE) => {
                self.activity = !self.activity;
                self.panel_scroll = 0;
                self.selected_message = self.list_len().saturating_sub(1);
            }
            (KeyCode::Char('r'), m) if m.contains(KeyModifiers::CONTROL) => {
                return Outcome::Reload;
            }
            (KeyCode::Down, KeyModifiers::NONE) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                self.move_cursor(1);
            }
            (KeyCode::Up, KeyModifiers::NONE) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                self.move_cursor(-1);
            }
            (KeyCode::Char('d'), m) if m.contains(KeyModifiers::CONTROL) => self.move_cursor(8),
            (KeyCode::Char('u'), m) if m.contains(KeyModifiers::CONTROL) => self.move_cursor(-8),
            (KeyCode::Char('g'), KeyModifiers::NONE) | (KeyCode::Home, KeyModifiers::NONE) => {
                self.move_cursor(isize::MIN / 2);
            }
            (KeyCode::Char('G'), _) | (KeyCode::End, KeyModifiers::NONE) => {
                self.move_cursor(isize::MAX / 2);
            }

            (KeyCode::Enter, KeyModifiers::NONE) => {
                if !lanes_focused && lists_messages {
                    if let Some(msg) = self.selected_message().cloned() {
                        self.open_view_message(msg);
                    } else if let Some(ActivityItem::Event {
                        lane_key: Some(k), ..
                    }) = self.activity_items().get(self.selected_message).cloned()
                        && self.select_lane(&k)
                    {
                        self.activity = false;
                        self.focus = Focus::Lanes;
                    }
                } else if let Some(idx) = self.selected().and_then(|l| l.session_idx) {
                    return Outcome::AttachSession(idx);
                } else if self.lanes.is_empty() {
                    self.start_swarm();
                } else {
                    self.status_msg =
                        Some("This lane has no session in this window to attach to.".into());
                }
            }

            (KeyCode::Char('m'), KeyModifiers::NONE) => {
                if !lanes_focused
                    && lists_messages
                    && let Some(msg) = self.selected_message().cloned()
                {
                    self.open_reply(&msg);
                } else if let Some(lane) = self.selected()
                    && let Some(launch) = lane.launch_id.clone()
                {
                    let label = lane.name.clone();
                    self.open_compose_message(Some(launch), label, 0);
                } else {
                    self.open_compose_message(None, "everyone".into(), 0);
                }
            }
            (KeyCode::Char('b'), KeyModifiers::NONE) => {
                self.open_compose_message(None, "everyone".into(), 0);
            }

            (KeyCode::Char('n'), KeyModifiers::NONE) => {
                if self.lanes.is_empty() {
                    self.open_launch_modal();
                } else {
                    self.open_new_worker_modal();
                }
            }
            (KeyCode::Char('e'), KeyModifiers::NONE) => {
                match self.selected().and_then(|l| l.task.clone()) {
                    Some(task) => self.open_edit_task_modal(&task),
                    None => self.status_msg = Some("This lane holds no task to edit.".into()),
                }
            }
            (KeyCode::Char('r'), KeyModifiers::NONE) => {
                if let Some(lane) = self.selected()
                    && let Some(task) = &lane.task
                {
                    if lane.kind == LaneKind::Queued {
                        return Outcome::DelegateTask {
                            task_id: task.id.clone(),
                        };
                    }
                    if matches!(lane.state, LaneState::Failed | LaneState::Lost) {
                        return Outcome::RetryTask {
                            task_id: task.id.clone(),
                        };
                    }
                }
                self.start_swarm();
            }
            (KeyCode::Char('x'), KeyModifiers::NONE) => {
                let Some(lane) = self.selected() else {
                    return Outcome::None;
                };
                if let Some(idx) = lane.session_idx
                    && !matches!(lane.state, LaneState::Exited | LaneState::Lost)
                {
                    self.confirm = Some(Confirm::StopSession {
                        session_idx: idx,
                        label: lane.name.clone(),
                    });
                } else if let Some(task) = lane.task.as_ref().filter(|t| !t.status.is_terminal()) {
                    self.confirm = Some(Confirm::CancelTask {
                        task_id: task.id.clone(),
                        title: task.title.clone(),
                    });
                } else {
                    self.status_msg = Some("Nothing to stop on this lane.".into());
                }
            }
            (KeyCode::Char('d'), KeyModifiers::NONE) => match self.selected() {
                Some(lane) if !lane.leases.is_empty() => {
                    self.confirm = Some(Confirm::ReleaseLeases {
                        lane: lane.name.clone(),
                        patterns: lane.leases.clone(),
                    });
                }
                _ => self.status_msg = Some("This lane holds no leases.".into()),
            },

            // Rare: not in the footer, still here.
            (KeyCode::Char('c'), KeyModifiers::NONE) => {
                if let Some(launch) = self.selected().and_then(|l| l.launch_id.clone()) {
                    self.coordinator_override = Some(launch.clone());
                    return Outcome::PromoteCoordinator { launch_id: launch };
                }
            }
            (KeyCode::Char('w'), KeyModifiers::NONE) => {
                if let Some(launch) = self.selected().and_then(|l| l.launch_id.clone()) {
                    return Outcome::WakeAgent { launch_id: launch };
                }
            }
            _ => {}
        }
        Outcome::None
    }

    /// Keys while the Needs-you strip has focus; `None` lets the screen's
    /// own keys (Tab, Esc, `a`, `1`–`4`, …) handle the rest.
    fn handle_needs_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        let need = self.selected_need().cloned()?;
        let lane_session = need
            .lane_key
            .as_deref()
            .and_then(|k| self.lanes.iter().find(|l| l.key == k))
            .and_then(|l| l.session_idx);
        let out = match (key.code, key.modifiers) {
            (KeyCode::Down, KeyModifiers::NONE) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                self.move_cursor(1);
                Outcome::None
            }
            (KeyCode::Up, KeyModifiers::NONE) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                self.move_cursor(-1);
                Outcome::None
            }
            (KeyCode::Enter, KeyModifiers::NONE) => self.act_on(&need, lane_session),
            (KeyCode::Char('m'), KeyModifiers::NONE) if need.kind == NeedKind::Question => {
                self.act_on(&need, lane_session)
            }
            (KeyCode::Char('r'), KeyModifiers::NONE) => match &need.task_id {
                Some(id) if need.kind.retryable() => Outcome::RetryTask {
                    task_id: id.clone(),
                },
                _ => {
                    self.status_msg = Some("Only a failed or lost task can be retried.".into());
                    Outcome::None
                }
            },
            (KeyCode::Char('d'), KeyModifiers::NONE) => {
                self.dismissed.insert(need.key.clone());
                self.recompute_needs();
                Outcome::None
            }
            _ => return None,
        };
        Some(out)
    }

    /// A need's primary action: answer a question, attach to a session that
    /// waits or idles, or show the lane on the tab that explains it.
    fn act_on(&mut self, need: &Need, lane_session: Option<usize>) -> Outcome {
        match need.kind {
            NeedKind::Question => {
                let msg = need
                    .message_id
                    .as_deref()
                    .and_then(|id| self.snapshot.messages.iter().find(|m| m.id == id).cloned());
                if let Some(msg) = msg {
                    self.open_reply(&msg);
                }
                return Outcome::None;
            }
            NeedKind::Attention | NeedKind::Stuck => {
                if let Some(idx) = lane_session {
                    return Outcome::AttachSession(idx);
                }
            }
            _ => {}
        }
        if let Some(k) = &need.lane_key
            && self.select_lane(k)
        {
            self.focus = Focus::Lanes;
            self.activity = false;
            match need.kind {
                NeedKind::Failed | NeedKind::Lost => self.set_tab(PanelTab::Task),
                NeedKind::Review => self.set_tab(PanelTab::Changes),
                _ => {}
            }
        } else {
            self.status_msg = Some("That agent is no longer on screen.".into());
        }
        Outcome::None
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
                    Confirm::ReleaseLeases { patterns, .. } => Outcome::ReleaseLeases(patterns),
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
                reply_to,
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
                    reply_to,
                    topic: MESSAGE_TOPICS[topic_idx % MESSAGE_TOPICS.len()].to_string(),
                    body: b,
                    urgent,
                    wake,
                }
            }
        }
    }

    /// Moves within the focused side: the lanes, the panel's message list,
    /// or the panel's scroll.
    fn move_cursor(&mut self, delta: isize) {
        match self.focus {
            Focus::Lanes => {
                let before = self.selected_lane;
                self.selected_lane = step_index(self.selected_lane, self.lanes.len(), delta);
                if self.selected_lane != before {
                    self.after_lane_change();
                }
            }
            Focus::Panel if self.activity || self.tab() == PanelTab::Thread => {
                let len = self.list_len();
                self.selected_message = step_index(self.selected_message, len, delta);
            }
            Focus::Needs => {
                self.selected_need = step_index(self.selected_need, self.needs.len(), delta);
            }
            Focus::Panel => {
                self.panel_scroll = if delta < 0 {
                    self.panel_scroll.saturating_sub(delta.unsigned_abs())
                } else {
                    self.panel_scroll.saturating_add(delta as usize).min(10_000)
                };
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
    fn the_lanes_and_the_panel_share_the_keyboard_and_tabs_stick_per_lane() {
        let (dir, db) = store();
        let conn = open_aux(&db).unwrap();
        let ws = bstore::normalize_workspace(dir.path());
        bstore::split_tasks(
            &conn,
            Some(&db),
            &ws,
            "coord-1",
            &[
                NewTaskSpec {
                    title: "Wire bus".into(),
                    instructions: "…".into(),
                    assigned_to: Some("w-1".into()),
                    ..Default::default()
                },
                NewTaskSpec {
                    title: "Write API".into(),
                    instructions: "Build endpoints".into(),
                    files: vec!["src/api.rs".into()],
                    ..Default::default()
                },
            ],
            1_000,
        )
        .unwrap();
        let mut worker = coordinator_live(&ws, "working");
        worker.session_idx = 1;
        worker.launch_id = "w-1".into();
        worker.agent_persona = Some("implementer".into());
        let live = vec![coordinator_live(&ws, "working"), worker];
        let mut view = SwarmView::open(
            dir.path().to_path_buf(),
            Some(open_aux(&db).unwrap()),
            &live,
            2_000,
        );
        let names: Vec<_> = view.lanes.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            ["coordinator·claude", "implementer·claude", "queued"]
        );
        assert_eq!(view.focus, Focus::Lanes);
        assert_eq!(
            view.tab(),
            PanelTab::Live,
            "a live session opens on its terminal"
        );

        // Enter attaches; Tab moves to the panel and Esc comes back.
        assert_eq!(
            view.handle_key(key(KeyCode::Enter)),
            Outcome::AttachSession(0)
        );
        view.handle_key(key(KeyCode::Tab));
        assert_eq!(view.focus, Focus::Panel);
        assert_eq!(view.handle_key(key(KeyCode::Esc)), Outcome::None);
        assert_eq!(view.focus, Focus::Lanes);

        // A chosen tab sticks to its lane; another lane opens on its own.
        view.handle_key(key(KeyCode::Char('4')));
        assert_eq!(view.tab(), PanelTab::Task);
        view.handle_key(key(KeyCode::Down));
        assert_eq!(view.selected().unwrap().name, "implementer·claude");
        assert_eq!(view.tab(), PanelTab::Live);
        view.handle_key(key(KeyCode::Up));
        assert_eq!(view.tab(), PanelTab::Task);

        // The queued lane opens on its task; r delegates it, x cancels it (y).
        view.handle_key(key(KeyCode::Char('G')));
        let lane = view.selected().unwrap().clone();
        assert_eq!(lane.kind, LaneKind::Queued);
        assert_eq!(view.tab(), PanelTab::Task);
        let task_id = lane.task.unwrap().id;
        assert_eq!(
            view.handle_key(key(KeyCode::Char('r'))),
            Outcome::DelegateTask {
                task_id: task_id.clone()
            }
        );
        view.handle_key(key(KeyCode::Char('x')));
        assert!(matches!(view.confirm, Some(Confirm::CancelTask { .. })));
        assert_eq!(view.handle_key(key(KeyCode::Enter)), Outcome::None);
        assert_eq!(
            view.handle_key(key(KeyCode::Char('y'))),
            Outcome::CancelTask(task_id)
        );

        // `a` swaps the panel for the swarm's Activity and back.
        view.handle_key(key(KeyCode::Char('a')));
        assert!(view.activity);
        view.handle_key(key(KeyCode::Char('a')));
        assert!(!view.activity);
    }

    #[test]
    fn a_reply_from_the_thread_answers_the_question_it_was_written_for() {
        let (dir, db) = store();
        let conn = open_aux(&db).unwrap();
        let ws = bstore::normalize_workspace(dir.path());
        let q = bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "coord-1",
            Some("user"),
            "question",
            "Ship on Friday?",
            None,
            1_000,
        )
        .unwrap();
        let live = vec![coordinator_live(&ws, "idle")];
        let mut view = SwarmView::open(
            dir.path().to_path_buf(),
            Some(open_aux(&db).unwrap()),
            &live,
            2_000,
        );
        assert_eq!(view.lanes[0].state, LaneState::Waiting);
        assert_eq!(
            view.tab(),
            PanelTab::Thread,
            "a waiting lane opens on its thread"
        );
        view.handle_key(key(KeyCode::Tab));
        view.handle_key(key(KeyCode::Char('m')));
        for c in "yes".chars() {
            view.handle_key(key(KeyCode::Char(c)));
        }
        let out = view.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(
            out,
            Outcome::SendMessage {
                to_launch: Some("coord-1".into()),
                reply_to: Some(q.id),
                topic: "answer".into(),
                body: "yes".into(),
                urgent: false,
                wake: true,
            }
        );
    }

    #[test]
    fn r_retries_a_failed_lane_and_bang_says_when_nothing_needs_you() {
        let (dir, db) = store();
        let conn = open_aux(&db).unwrap();
        let ws = bstore::normalize_workspace(dir.path());
        let mut view = SwarmView::open(
            dir.path().to_path_buf(),
            Some(open_aux(&db).unwrap()),
            &[],
            2_000,
        );
        view.handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::SHIFT));
        assert_eq!(view.focus, Focus::Lanes);
        assert_eq!(view.status_msg.as_deref(), Some("Nothing needs you."));

        let id = bstore::split_tasks(
            &conn,
            Some(&db),
            &ws,
            "coord-1",
            &[NewTaskSpec {
                title: "ui tests".into(),
                instructions: "…".into(),
                assigned_to: Some("coord-1".into()),
                ..Default::default()
            }],
            1_000,
        )
        .unwrap()
        .remove(0)
        .id;
        bstore::update_task(
            &conn,
            Some(&db),
            &id,
            "coord-1",
            crate::bus::TaskStatus::Failed,
            None,
            Some("boom".into()),
            None,
            None,
            1_500,
        )
        .unwrap();
        let live = vec![coordinator_live(&ws, "idle")];
        view.refresh(&live, 3_000);
        assert_eq!(view.selected().unwrap().state, LaneState::Failed);
        assert_eq!(view.tab(), PanelTab::Task);
        assert_eq!(
            view.handle_key(key(KeyCode::Char('r'))),
            Outcome::RetryTask { task_id: id }
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
            quiet_ns: None,
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
        assert_eq!(view.selected().unwrap().kind, LaneKind::Queued);
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
                reply_to: None,
                topic: "answer".into(),
                body: "one \nx".into(),
                urgent: true,
                wake: false,
            }
        );
    }
}
