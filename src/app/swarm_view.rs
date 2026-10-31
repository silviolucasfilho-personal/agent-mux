//! Swarm Mission Control (`Mode::Swarm`).
//!
//! One workspace's swarm as lanes, an agent and the task it holds
//! (`bus::swarm::Lane`), beside a focus panel on the selected lane with four
//! tabs: Live (its terminal), Thread (its messages), Changes (its diff) and
//! Task (its task card), under the Needs-you strip (`bus::swarm::Need`:
//! what only the operator can resolve, `!` to act). `a` swaps the panel for
//! the whole swarm's Activity (messages and task and lease events). Messages
//! are written on the `>` command line: `@coord`, `@all` or `@<lane>`
//! (the selected lane when none is named), a leading `!` for urgent, a
//! `#topic` if the inferred one will not do (`parse_command`).
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
    /// Approve the coordinator's plan before any worker starts.
    PlanGate,
    PrewarmWorkers,
}

impl LaunchField {
    pub fn next(self) -> Self {
        match self {
            LaunchField::Goal => LaunchField::Harness,
            LaunchField::Harness => LaunchField::MaxWorkers,
            LaunchField::MaxWorkers => LaunchField::Isolation,
            LaunchField::Isolation => LaunchField::PlanGate,
            LaunchField::PlanGate => LaunchField::PrewarmWorkers,
            LaunchField::PrewarmWorkers => LaunchField::Goal,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            LaunchField::Goal => LaunchField::PrewarmWorkers,
            LaunchField::Harness => LaunchField::Goal,
            LaunchField::MaxWorkers => LaunchField::Harness,
            LaunchField::Isolation => LaunchField::MaxWorkers,
            LaunchField::PlanGate => LaunchField::Isolation,
            LaunchField::PrewarmWorkers => LaunchField::PlanGate,
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

/// The `>` command line: one message to one lane, the coordinator or
/// everyone.
#[derive(Debug, Clone)]
pub struct CommandLine {
    pub text: TextArea,
    /// The message this answers, when it was opened as a reply.
    pub reply_to: Option<BusMessage>,
    /// The completion `Tab` last offered, to cycle through the next one.
    completion: Option<(String, usize)>,
}

/// Who a command line message goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    Everyone,
    Launch { launch: String, name: String },
}

/// A command line read against the swarm: what would be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub to: Recipient,
    pub urgent: bool,
    pub topic: String,
    pub body: String,
    pub reply_to: Option<String>,
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Reads `text`: leading `!` (urgent), `@who` and `#topic` tokens in any
/// order, then the body. `@who` is `coord`, `all` or a lane's name, matched
/// without its separators and by prefix (`@impl` for `implementer·codex`)
/// or by launch id; with none, the reply's sender, else `fallback`. The
/// topic, unless named: `urgent` for `!` (the hook delivers it mid-turn),
/// `answer` for a reply to a question, else `directive`.
pub fn parse_command(
    text: &str,
    snapshot: &SwarmSnapshot,
    lanes: &[Lane],
    reply_to: Option<&BusMessage>,
    fallback: Option<&Lane>,
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
    let to = match who {
        Some(who) => resolve(who, snapshot, lanes, reply_to)?,
        None => match (reply_to, fallback) {
            (Some(m), _) => Recipient::Launch {
                launch: m.from_launch.clone(),
                name: snapshot.member_label(&m.from_launch),
            },
            (None, Some(lane)) if lane.launch_id.is_some() => Recipient::Launch {
                launch: lane.launch_id.clone().unwrap_or_default(),
                name: lane.name.clone(),
            },
            _ => return Err("say who: @coord, @all or @<lane>".into()),
        },
    };
    if body.is_empty() {
        return Err("write the message after the address".into());
    }
    // A reply re-addressed to someone else is a new message.
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

/// A lane's name as one `@` word: a disambiguated `implementer·codex 8f31`
/// becomes `implementer·codex·8f31`, which resolves to the same lane.
pub fn address_token(name: &str) -> String {
    name.replace(' ', "·")
}

/// The addresses `@` can name: `coord`, `all`, and every agent lane.
fn addresses(lanes: &[Lane]) -> Vec<String> {
    let mut out = vec!["coord".to_string(), "all".to_string()];
    out.extend(
        lanes
            .iter()
            .filter(|l| l.launch_id.is_some() && l.kind == LaneKind::Agent)
            .map(|l| address_token(&l.name)),
    );
    out
}

fn resolve(
    who: &str,
    snapshot: &SwarmSnapshot,
    lanes: &[Lane],
    reply_to: Option<&BusMessage>,
) -> Result<Recipient, String> {
    let key = squash(who);
    if key.is_empty() {
        return Err("@ names someone: @coord, @all or @<lane>".into());
    }
    if matches!(key.as_str(), "all" | "everyone") {
        return Ok(Recipient::Everyone);
    }
    if "coordinator".starts_with(&key) || key == "lead" {
        return match &snapshot.coordinator {
            Some(c) => Ok(Recipient::Launch {
                launch: c.launch_id.clone(),
                name: c.label.clone(),
            }),
            None => Err("this swarm has no coordinator".into()),
        };
    }
    let agents: Vec<&Lane> = lanes
        .iter()
        .filter(|l| l.launch_id.is_some() && l.kind == LaneKind::Agent)
        .collect();
    let launch_of = |l: &Lane| l.launch_id.clone().unwrap_or_default();
    if let Some(l) = agents.iter().find(|l| squash(&l.name) == key) {
        return Ok(Recipient::Launch {
            launch: launch_of(l),
            name: l.name.clone(),
        });
    }
    let matches: Vec<&&Lane> = agents
        .iter()
        .filter(|l| squash(&l.name).starts_with(&key) || launch_of(l).starts_with(who))
        .collect();
    match matches.as_slice() {
        [one] => Ok(Recipient::Launch {
            launch: launch_of(one),
            name: one.name.clone(),
        }),
        [] => match reply_to {
            // The asker may have left the swarm: its short id still answers it.
            Some(m) if m.from_launch.starts_with(who) => Ok(Recipient::Launch {
                launch: m.from_launch.clone(),
                name: snapshot.member_label(&m.from_launch),
            }),
            _ => Err(format!("nobody here is called @{who}")),
        },
        many => Err(format!(
            "@{who} could be {}",
            many.iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>()
                .join(" or ")
        )),
    }
}

#[derive(Debug, Clone)]
pub enum Modal {
    LaunchSwarm {
        goal: TextArea,
        harness_idx: usize,
        max_workers: usize,
        isolation: TaskIsolation,
        plan_gate: bool,
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
    /// One message in full: the feed shows a line of it; `Enter` opens this.
    ViewMessage { message: BusMessage, scroll: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    /// `r` while a coordinator is live: a second launch would replace it.
    RelaunchSwarm {
        coordinator: String,
    },
    /// Merge a finished worktree branch into the workspace.
    MergeBranch {
        task_id: String,
        branch: String,
    },
    /// Set a finished branch aside without merging it.
    DiscardBranch {
        task_id: String,
        branch: String,
    },
    /// End the swarm: stop its sessions, cancel what is open, forget it.
    CloseSwarm,
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
        plan_gate: bool,
        prewarm_workers: usize,
    },
    /// The operator's decision on the coordinator's plan.
    DecidePlan {
        approve: bool,
    },
    MergeBranch {
        task_id: String,
    },
    DiscardBranch {
        task_id: String,
    },
    CloseSwarm,
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
    /// The `>` command line, while it is open.
    pub command: Option<CommandLine>,
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
            command: None,
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
        self.modal.is_some() || self.command.is_some()
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
            // The coordinator opens on its plan while the plan holds work,
            // and on the summary once every task is settled.
            _ if lane.kind == LaneKind::Coordinator
                && (self.snapshot.plan_held()
                    || matches!(
                        self.snapshot.phase(),
                        crate::bus::swarm::Phase::Integrating
                            | crate::bus::swarm::Phase::WrappingUp
                            | crate::bus::swarm::Phase::Done
                    )) =>
            {
                PanelTab::Task
            }
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
            plan_gate: true,
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

    /// Opens the command line with `prefill` (an address, usually).
    pub fn open_command(&mut self, prefill: &str, reply_to: Option<BusMessage>) {
        self.modal = None;
        self.command = Some(CommandLine {
            text: TextArea::new(prefill),
            reply_to,
            completion: None,
        });
    }

    /// The `@` address for a lane, as the command line writes it.
    fn address_of(lane: &Lane) -> String {
        match lane.kind {
            LaneKind::Coordinator => "@coord ".into(),
            _ => format!("@{} ", address_token(&lane.name)),
        }
    }

    /// Opens a message in full.
    pub fn open_view_message(&mut self, message: BusMessage) {
        self.modal = Some(Modal::ViewMessage { message, scroll: 0 });
    }

    /// The command line addressed to a message's sender, answering it.
    fn open_reply(&mut self, message: &BusMessage) {
        let sender = message.from_launch.clone();
        let address = match self
            .lanes
            .iter()
            .find(|l| l.launch_id.as_deref() == Some(&sender))
        {
            Some(lane) => Self::address_of(lane),
            None => format!("@{} ", short_id(&sender)),
        };
        self.open_command(&address, Some(message.clone()));
    }

    /// What the command line would send now, or why it cannot.
    pub fn command_preview(&self) -> Option<Result<Outgoing, String>> {
        let line = self.command.as_ref()?;
        Some(parse_command(
            &line.text.text,
            &self.snapshot,
            &self.lanes,
            line.reply_to.as_ref(),
            self.selected(),
        ))
    }

    /// Text pasted while the command line is open goes into it.
    pub fn paste(&mut self, text: &str) {
        if let Some(line) = &mut self.command {
            line.text.insert_str(text);
            line.completion = None;
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
            (KeyCode::Enter, KeyModifiers::NONE) => match self.command_preview() {
                Some(Ok(cmd)) => {
                    self.command = None;
                    let to = match &cmd.to {
                        Recipient::Everyone => None,
                        Recipient::Launch { launch, .. } => Some(launch.clone()),
                    };
                    Outcome::SendMessage {
                        to_launch: to,
                        reply_to: cmd.reply_to,
                        topic: cmd.topic,
                        body: cmd.body,
                        urgent: cmd.urgent,
                        wake: true,
                    }
                }
                Some(Err(why)) => {
                    self.status_msg = Some(why);
                    Outcome::None
                }
                None => Outcome::None,
            },
            (KeyCode::Tab, KeyModifiers::NONE) => {
                complete_address(line, &self.lanes);
                Outcome::None
            }
            _ => {
                crate::keymap::apply_text(&mut line.text, &key);
                line.completion = None;
                Outcome::None
            }
        }
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
        if self.command.is_some() {
            return self.handle_command_key(key);
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
        if self.focus != Focus::Needs
            && let Some(out) = self.plan_key(key)
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
                } else {
                    let address = self.selected().map(Self::address_of).unwrap_or_default();
                    self.open_command(&address, None);
                }
            }
            (KeyCode::Char('b'), KeyModifiers::NONE) => self.open_command("@all ", None),
            (KeyCode::Char('>'), _) => self.open_command("", None),
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

    /// Asks before merging (or setting aside) a finished task's branch.
    fn confirm_review(&mut self, task_id: Option<&str>, merge: bool) {
        let Some(task) = task_id.and_then(|id| self.snapshot.tasks.iter().find(|t| t.id == id))
        else {
            return;
        };
        let Some(branch) = task.branch.clone() else {
            self.status_msg = Some("That task has no branch to review.".into());
            return;
        };
        let task_id = task.id.clone();
        self.confirm = Some(if merge {
            Confirm::MergeBranch { task_id, branch }
        } else {
            Confirm::DiscardBranch { task_id, branch }
        });
    }

    /// Sends the plan back: records the decision and opens the command line
    /// to the coordinator for what should change.
    fn send_plan_back(&mut self) -> Outcome {
        self.open_command("@coord #plan-changes ", None);
        Outcome::DecidePlan { approve: false }
    }

    /// `y` and `e` on the coordinator's lane while its plan waits.
    fn plan_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        if !self.snapshot.plan_held()
            || self
                .selected()
                .is_none_or(|l| l.kind != LaneKind::Coordinator)
        {
            return None;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('y'), KeyModifiers::NONE) => Some(Outcome::DecidePlan { approve: true }),
            (KeyCode::Char('e'), KeyModifiers::NONE) => Some(self.send_plan_back()),
            _ => None,
        }
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
            (KeyCode::Char('y'), KeyModifiers::NONE) => match need.kind {
                NeedKind::Plan => Outcome::DecidePlan { approve: true },
                NeedKind::Review => {
                    self.confirm_review(need.task_id.as_deref(), true);
                    Outcome::None
                }
                NeedKind::Finished => {
                    self.confirm = Some(Confirm::CloseSwarm);
                    Outcome::None
                }
                _ => return None,
            },
            (KeyCode::Char('e'), KeyModifiers::NONE) if need.kind == NeedKind::Plan => {
                self.send_plan_back()
            }
            (KeyCode::Char('x'), KeyModifiers::NONE) if need.kind == NeedKind::Review => {
                self.confirm_review(need.task_id.as_deref(), false);
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
                    Confirm::MergeBranch { task_id, .. } => Outcome::MergeBranch { task_id },
                    Confirm::DiscardBranch { task_id, .. } => Outcome::DiscardBranch { task_id },
                    Confirm::CloseSwarm => Outcome::CloseSwarm,
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
        // and is a newline in a multi-line one.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let submit = (key.code == KeyCode::Char('s') && ctrl)
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
                plan_gate,
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
                    LaunchField::PlanGate => {
                        if matches!(
                            key.code,
                            KeyCode::Left
                                | KeyCode::Right
                                | KeyCode::Char('h')
                                | KeyCode::Char('l')
                                | KeyCode::Char(' ')
                        ) {
                            *plan_gate = !*plan_gate;
                        }
                    }
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
                plan_gate,
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
                    plan_gate,
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

/// `Tab` on the command line: completes the `@` word under the cursor to
/// the next address it could be, cycling on repeat.
fn complete_address(line: &mut CommandLine, lanes: &[Lane]) {
    let text = line.text.text.clone();
    let cursor = line.text.cursor();
    // Right after a completion the word is the one just inserted, before
    // the space that follows it.
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
    let candidates: Vec<String> = addresses(lanes)
        .into_iter()
        .filter(|name| squash(name).starts_with(&prefix))
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
        assert_eq!(
            view.command.as_ref().map(|c| c.text.text.as_str()),
            Some("@coord yes")
        );
        let out = view.handle_key(key(KeyCode::Enter));
        assert!(view.command.is_none());
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

    /// A swarm with a coordinator, two codex implementers and a tester.
    fn crowd() -> (tempfile::TempDir, SwarmView) {
        let (dir, db) = store();
        let conn = open_aux(&db).unwrap();
        let ws = bstore::normalize_workspace(dir.path());
        for (who, at) in [("aaaa1111", 1), ("bbbb2222", 2), ("cccc3333", 3)] {
            bstore::split_tasks(
                &conn,
                Some(&db),
                &ws,
                "coord-1",
                &[NewTaskSpec {
                    title: format!("task {who}"),
                    instructions: "…".into(),
                    assigned_to: Some(who.into()),
                    ..Default::default()
                }],
                at,
            )
            .unwrap();
        }
        let worker = |idx: usize, launch: &str, persona: &str, provider: &str| {
            let mut s = coordinator_live(&ws, "working");
            s.session_idx = idx;
            s.launch_id = launch.into();
            s.agent_persona = Some(persona.into());
            s.provider = provider.into();
            s
        };
        let live = vec![
            coordinator_live(&ws, "working"),
            worker(1, "aaaa1111", "implementer", "codex"),
            worker(2, "bbbb2222", "implementer", "codex"),
            worker(3, "cccc3333", "tester", "agy"),
        ];
        let view = SwarmView::open(
            dir.path().to_path_buf(),
            Some(open_aux(&db).unwrap()),
            &live,
            10,
        );
        (dir, view)
    }

    fn parse(view: &SwarmView, text: &str) -> Result<Outgoing, String> {
        parse_command(text, &view.snapshot, &view.lanes, None, view.selected())
    }

    #[test]
    fn the_command_line_reads_who_how_and_what() {
        let (_dir, view) = crowd();
        let to = |r: Result<Outgoing, String>| match r {
            Ok(o) => (
                match o.to {
                    Recipient::Everyone => "everyone".to_string(),
                    Recipient::Launch { name, .. } => name,
                },
                o.topic,
                o.body,
                o.urgent,
            ),
            Err(e) => (format!("error: {e}"), String::new(), String::new(), false),
        };
        let row =
            |a: &str, b: &str, c: &str, d: bool| (a.to_string(), b.to_string(), c.to_string(), d);
        assert_eq!(
            to(parse(&view, "@coord ship it")),
            row("coordinator·claude", "directive", "ship it", false)
        );
        assert_eq!(
            to(parse(&view, "@all stop editing src/")),
            row("everyone", "directive", "stop editing src/", false)
        );
        assert_eq!(
            to(parse(&view, "@tester run it")),
            row("tester·agy", "directive", "run it", false)
        );
        assert_eq!(
            to(parse(&view, "@tester.agy run it")),
            row("tester·agy", "directive", "run it", false)
        );
        assert_eq!(
            to(parse(&view, "! @tester stop now")),
            row("tester·agy", "urgent", "stop now", true)
        );
        assert_eq!(
            to(parse(&view, "!@tester stop now")),
            row("tester·agy", "urgent", "stop now", true)
        );
        assert_eq!(
            to(parse(&view, "@tester #api-contract v2 is final")),
            row("tester·agy", "api-contract", "v2 is final", false)
        );
        assert_eq!(
            to(parse(&view, "@bbbb please")),
            row("implementer·codex bbbb", "directive", "please", false)
        );
        assert_eq!(
            to(parse(&view, "@implementer·codex·bbbb please")),
            row("implementer·codex bbbb", "directive", "please", false),
            "a completed address is one word"
        );
        // No address: the selected lane (the coordinator, first).
        assert_eq!(
            to(parse(&view, "hello")),
            row("coordinator·claude", "directive", "hello", false)
        );
        for (text, why) in [
            (
                "@impl go",
                "@impl could be implementer·codex aaaa or implementer·codex bbbb",
            ),
            ("@nobody go", "nobody here is called @nobody"),
            ("@tester", "write the message after the address"),
            (
                "@tester @coord hi",
                "one recipient per message: @all writes to everyone",
            ),
        ] {
            assert_eq!(parse(&view, text), Err(why.to_string()), "{text}");
        }
    }

    #[test]
    fn a_reply_answers_its_question_unless_readdressed() {
        let (_dir, view) = crowd();
        let q = BusMessage {
            id: "msg-1".into(),
            workspace: String::new(),
            from_launch: "cccc3333".into(),
            to_launch: Some("user".into()),
            topic: "question".into(),
            body: "drop the trigger?".into(),
            reply_to: None,
            created_ns: 5,
            delivered_ns: None,
            acked_ns: None,
        };
        let reply =
            parse_command("@tester yes", &view.snapshot, &view.lanes, Some(&q), None).unwrap();
        assert_eq!(
            (reply.topic.as_str(), reply.reply_to.as_deref()),
            ("answer", Some("msg-1"))
        );
        let bare = parse_command("yes", &view.snapshot, &view.lanes, Some(&q), None).unwrap();
        assert_eq!(
            bare.reply_to.as_deref(),
            Some("msg-1"),
            "no address: the asker"
        );
        let other =
            parse_command("@coord fyi", &view.snapshot, &view.lanes, Some(&q), None).unwrap();
        assert_eq!((other.topic.as_str(), other.reply_to), ("directive", None));
    }

    #[test]
    fn the_command_line_completes_addresses_and_keeps_text_keys() {
        let (_dir, mut view) = crowd();
        view.handle_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::SHIFT));
        assert!(view.text_entry_active());
        for c in "@te".chars() {
            view.handle_key(key(KeyCode::Char(c)));
        }
        view.handle_key(key(KeyCode::Tab));
        assert_eq!(view.command.as_ref().unwrap().text.text, "@tester·agy ");
        // `@i` cycles through both implementers.
        view.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        view.handle_key(key(KeyCode::Char('@')));
        view.handle_key(key(KeyCode::Char('i')));
        view.handle_key(key(KeyCode::Tab));
        assert_eq!(
            view.command.as_ref().unwrap().text.text,
            "@implementer·codex·aaaa "
        );
        view.handle_key(key(KeyCode::Tab));
        assert_eq!(
            view.command.as_ref().unwrap().text.text,
            "@implementer·codex·bbbb "
        );
        // Text keys stay text keys: Ctrl+W deletes a word, Alt+Enter a new line.
        for c in "one two".chars() {
            view.handle_key(key(KeyCode::Char(c)));
        }
        view.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        view.handle_key(key(KeyCode::Char('x')));
        assert_eq!(
            view.command.as_ref().unwrap().text.text,
            "@implementer·codex·bbbb one \nx"
        );
        view.paste(" pasted");
        let out = view.handle_key(key(KeyCode::Enter));
        assert_eq!(
            out,
            Outcome::SendMessage {
                to_launch: Some("bbbb2222".into()),
                reply_to: None,
                topic: "directive".into(),
                body: "one \nx pasted".into(),
                urgent: false,
                wake: true,
            }
        );

        // An error keeps the line open; Esc drops it.
        view.handle_key(key(KeyCode::Char('b')));
        assert_eq!(view.command.as_ref().unwrap().text.text, "@all ");
        view.handle_key(key(KeyCode::Enter));
        assert!(view.command.is_some());
        assert_eq!(
            view.command_preview(),
            Some(Err("write the message after the address".into()))
        );
        view.handle_key(key(KeyCode::Esc));
        assert!(view.command.is_none() && !view.text_entry_active());
    }

    #[test]
    fn the_launch_form_asks_for_plan_approval_by_default() {
        let (dir, db) = store();
        let mut view = SwarmView::open(
            dir.path().to_path_buf(),
            Some(open_aux(&db).unwrap()),
            &[],
            1,
        );
        view.handle_key(key(KeyCode::Char('r')));
        for c in "ship".chars() {
            view.handle_key(key(KeyCode::Char(c)));
        }
        let out = view.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(
            matches!(
                out,
                Outcome::StartSwarm {
                    plan_gate: true,
                    ..
                }
            ),
            "{out:?}"
        );
        // Tab to the gate (goal, harness, workers, isolation, gate) and turn it off.
        view.handle_key(key(KeyCode::Char('r')));
        for _ in 0..4 {
            view.handle_key(key(KeyCode::Tab));
        }
        view.handle_key(key(KeyCode::Char(' ')));
        view.handle_key(key(KeyCode::BackTab));
        view.handle_key(key(KeyCode::BackTab));
        view.handle_key(key(KeyCode::BackTab));
        view.handle_key(key(KeyCode::BackTab));
        for c in "ship".chars() {
            view.handle_key(key(KeyCode::Char(c)));
        }
        let out = view.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(
            matches!(
                out,
                Outcome::StartSwarm {
                    plan_gate: false,
                    ..
                }
            ),
            "{out:?}"
        );
    }

    #[test]
    fn e_sends_the_plan_back_and_opens_the_line_to_the_coordinator() {
        let (dir, db) = store();
        let conn = open_aux(&db).unwrap();
        let ws = bstore::normalize_workspace(dir.path());
        bstore::set_plan_gate(&conn, Some(&db), &ws, true, 0, 1).unwrap();
        let live = vec![coordinator_live(&ws, "idle")];
        let mut view = SwarmView::open(
            dir.path().to_path_buf(),
            Some(open_aux(&db).unwrap()),
            &live,
            2,
        );
        assert_eq!(view.selected().unwrap().kind, LaneKind::Coordinator);
        assert_eq!(
            view.tab(),
            PanelTab::Task,
            "the coordinator opens on its plan"
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Char('e'))),
            Outcome::DecidePlan { approve: false }
        );
        assert_eq!(
            view.command.as_ref().map(|c| c.text.text.as_str()),
            Some("@coord #plan-changes ")
        );
        view.handle_key(key(KeyCode::Esc));
        assert_eq!(
            view.handle_key(key(KeyCode::Char('y'))),
            Outcome::DecidePlan { approve: true }
        );
    }
}
