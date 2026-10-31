//! Swarm snapshot and orchestration model (`src/bus/swarm.rs`).
//!
//! Aggregates the workspace's active Coordinator, Workers, Task Board,
//! Cooperative File Leases, and Inter-Agent Message Feed from `traces.db`
//! and live `App` sessions into a single [`SwarmSnapshot`], and derives the
//! Mission Control view of it: [`Lane`]s with stable names and states, open
//! questions and the swarm's [`Phase`]
//! (`docs/superpowers/specs/2026-10-30-swarm-mission-control-ux-design.md`).

use super::{
    BusMessage, BusTask, FileLease, PeerInfo, TaskIsolation, TaskStatus, short_id, store as bstore,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Role of an agent session inside a workspace swarm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SwarmRole {
    Coordinator,
    Worker,
}

impl SwarmRole {
    pub fn as_str(self) -> &'static str {
        match self {
            SwarmRole::Coordinator => "coordinator",
            SwarmRole::Worker => "worker",
        }
    }
}

/// Live session telemetry supplied directly by the running `App` (when in TUI mode).
#[derive(Debug, Clone)]
pub struct LiveSwarmSession {
    pub session_idx: usize,
    pub session_id: usize,
    pub launch_id: String,
    pub provider: String,
    pub profile_name: String,
    pub agent_persona: Option<String>,
    pub state: String,
    pub workspace: String,
    pub is_delegated_worker: bool,
    pub turns: usize,
    pub tokens: i64,
    pub cost_usd: f64,
    pub running_tool: Option<String>,
    /// Nanoseconds since the session last printed anything.
    pub quiet_ns: Option<i64>,
}

/// One member (Coordinator or Worker) of a workspace swarm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmMember {
    pub launch_id: String,
    pub session_idx: Option<usize>,
    pub session_id: Option<usize>,
    pub label: String,
    pub role: SwarmRole,
    pub provider: String,
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_persona: Option<String>,
    pub state: String,
    pub active_tasks: Vec<String>,
    pub leased_files: Vec<String>,
    pub unread_messages: usize,
    pub turns: usize,
    pub tokens: i64,
    pub cost_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_tool: Option<String>,
    /// Nanoseconds since its session last printed anything (live sessions only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_ns: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl SwarmMember {
    pub fn is_active(&self) -> bool {
        self.state != "exited" && self.state != "disconnected"
    }
}

/// Complete point-in-time snapshot of a workspace swarm.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwarmSnapshot {
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// The worker cap the swarm was started with (`swarm_state`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_workers: Option<usize>,
    /// The default task isolation the swarm was started with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<TaskIsolation>,
    /// When the swarm was started (`swarm_state.started_ns`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_ns: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator: Option<SwarmMember>,
    pub workers: Vec<SwarmMember>,
    pub tasks: Vec<BusTask>,
    pub leases: Vec<FileLease>,
    pub messages: Vec<BusMessage>,
    pub total_turns: usize,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
}

impl SwarmSnapshot {
    /// All members in roster order: Coordinator first (if present), followed by Workers.
    pub fn all_members(&self) -> Vec<&SwarmMember> {
        let mut out =
            Vec::with_capacity(self.workers.len() + usize::from(self.coordinator.is_some()));
        if let Some(c) = &self.coordinator {
            out.push(c);
        }
        out.extend(self.workers.iter());
        out
    }

    /// The name a launch goes by everywhere on the screen: a member's stable
    /// lane name (`implementer·codex`), `User` for the operator, else the
    /// launch's short id.
    pub fn member_label(&self, launch_id: &str) -> String {
        if launch_id == "user" || launch_id == "operator" || launch_id == "cli" {
            return "User".to_string();
        }
        self.all_members()
            .into_iter()
            .find(|m| m.launch_id == launch_id)
            .map(|m| m.label.clone())
            .unwrap_or_else(|| short_id(launch_id).to_string())
    }

    /// Counts tasks by status category: `(running, open_or_claimed, done, failed_or_cancelled)`.
    pub fn task_counts(&self) -> (usize, usize, usize, usize) {
        let (mut running, mut open, mut done, mut failed) = (0, 0, 0, 0);
        for t in &self.tasks {
            match t.status {
                TaskStatus::Running => running += 1,
                TaskStatus::Open | TaskStatus::Claimed => open += 1,
                TaskStatus::Done => done += 1,
                TaskStatus::Failed | TaskStatus::Cancelled => failed += 1,
            }
        }
        (running, open, done, failed)
    }
}

/// Builds a [`SwarmSnapshot`] for `workspace` by combining `traces.db` with optional
/// in-memory `live_sessions` from the running `App`. `now` is in nanoseconds,
/// like every timestamp the store compares it with (lease expiry, peer
/// recency). Reads only: expired leases are left out, not pruned.
pub fn load_snapshot(
    conn: Option<&Connection>,
    workspace_path: &Path,
    live_sessions: &[LiveSwarmSession],
    explicit_coordinator: Option<&str>,
    now: i64,
) -> SwarmSnapshot {
    let workspace = bstore::normalize_workspace(workspace_path);
    let mut tasks = Vec::new();
    let mut leases = Vec::new();
    let mut messages = Vec::new();
    let mut db_peers: Vec<PeerInfo> = Vec::new();
    let mut state: Option<bstore::SwarmState> = None;

    if let Some(c) = conn {
        tasks = bstore::list_tasks(c, &workspace, None, None, None).unwrap_or_default();
        leases = bstore::list_leases(c, &workspace, now).unwrap_or_default();
        messages = bstore::list_workspace_messages(c, &workspace, 150).unwrap_or_default();
        db_peers = bstore::list_peers(c, &workspace, None, true, now).unwrap_or_default();
        state = bstore::swarm_state(c, &workspace).ok().flatten();
    }

    // Also include worktree paths created for tasks in this workspace so workers running
    // inside isolated worktrees are recognized as members of this workspace's swarm.
    let worktree_dirs: HashSet<String> = tasks
        .iter()
        .filter_map(|t| t.worktree.as_deref())
        .map(|p| bstore::normalize_workspace(Path::new(p)))
        .collect();

    let live_by_launch: HashMap<String, &LiveSwarmSession> = live_sessions
        .iter()
        .filter(|s| {
            let norm = bstore::normalize_workspace(Path::new(&s.workspace));
            norm == workspace || worktree_dirs.contains(&norm)
        })
        .map(|s| (s.launch_id.clone(), s))
        .collect();

    // Determine which launch_id is the Coordinator:
    // 1. Explicit coordinator override from `App`
    // 2. The coordinator recorded in `swarm_state` (started or promoted)
    // 3. A live session in this workspace with `agent_persona == Some("coordinator")`
    // 4. A session that created tasks on the board (`created_by`) and is not a delegated worker
    let coord_launch_id: Option<String> = explicit_coordinator
        .map(str::to_string)
        .or_else(|| state.as_ref().map(|st| st.coordinator_launch.clone()))
        .or_else(|| {
            live_by_launch
                .values()
                .find(|s| s.agent_persona.as_deref() == Some("coordinator"))
                .map(|s| s.launch_id.clone())
        })
        .or_else(|| {
            tasks.first().and_then(|t| {
                let creator = t.created_by.as_str();
                if creator != "user" && creator != "cli" && creator != "operator" {
                    Some(creator.to_string())
                } else {
                    None
                }
            })
        });

    let mut seen_launches = HashSet::new();
    let mut raw_members: Vec<SwarmMember> = Vec::new();

    // Merge live sessions first (freshest state and direct `session_idx`)
    let mut ordered_live: Vec<&LiveSwarmSession> = live_by_launch.values().copied().collect();
    ordered_live.sort_by_key(|s| s.session_idx);

    for ls in ordered_live {
        seen_launches.insert(ls.launch_id.clone());
        let is_coord = coord_launch_id.as_deref() == Some(ls.launch_id.as_str());
        let active_tasks: Vec<String> = tasks
            .iter()
            .filter(|t| {
                t.assigned_to.as_deref() == Some(ls.launch_id.as_str()) && !t.status.is_terminal()
            })
            .map(|t| format!("{}: {}", short_id(&t.id), t.title))
            .collect();
        let leased_files: Vec<String> = leases
            .iter()
            .filter(|l| l.owner_launch == ls.launch_id)
            .map(|l| l.pattern.clone())
            .collect();
        let unread_messages = conn
            .and_then(|c| bstore::count_unread_messages(c, &workspace, &ls.launch_id).ok())
            .unwrap_or(0);
        let assigned_task = tasks
            .iter()
            .rev()
            .find(|t| t.assigned_to.as_deref() == Some(ls.launch_id.as_str()));

        raw_members.push(SwarmMember {
            launch_id: ls.launch_id.clone(),
            session_idx: Some(ls.session_idx),
            session_id: Some(ls.session_id),
            label: String::new(),
            role: if is_coord {
                SwarmRole::Coordinator
            } else {
                SwarmRole::Worker
            },
            provider: ls.provider.clone(),
            profile: ls.profile_name.clone(),
            agent_persona: ls
                .agent_persona
                .clone()
                .or_else(|| assigned_task.and_then(|t| t.target_agent.clone())),
            state: ls.state.clone(),
            active_tasks,
            leased_files,
            unread_messages,
            turns: ls.turns,
            tokens: ls.tokens,
            cost_usd: ls.cost_usd,
            running_tool: ls.running_tool.clone(),
            quiet_ns: ls.quiet_ns,
            worktree: assigned_task.and_then(|t| t.worktree.clone()),
            branch: assigned_task.and_then(|t| t.branch.clone()),
        });
    }

    // Add DB peers not already represented in `live_sessions`
    for peer in db_peers {
        if seen_launches.contains(&peer.launch_id) {
            continue;
        }
        let is_coord = coord_launch_id.as_deref() == Some(peer.launch_id.as_str());
        let has_swarm_activity = is_coord
            || !peer.active_tasks.is_empty()
            || !peer.leased_files.is_empty()
            || tasks
                .iter()
                .any(|t| t.assigned_to.as_deref() == Some(peer.launch_id.as_str()))
            || peer.state != "exited";
        if !has_swarm_activity {
            continue;
        }
        seen_launches.insert(peer.launch_id.clone());
        let (turns, tokens, cost_usd) = conn
            .and_then(|c| query_launch_totals(c, &peer.launch_id))
            .unwrap_or((0, 0, 0.0));
        let assigned_task = tasks
            .iter()
            .rev()
            .find(|t| t.assigned_to.as_deref() == Some(peer.launch_id.as_str()));

        raw_members.push(SwarmMember {
            launch_id: peer.launch_id.clone(),
            session_idx: None,
            session_id: peer.session_id,
            label: String::new(),
            role: if is_coord {
                SwarmRole::Coordinator
            } else {
                SwarmRole::Worker
            },
            provider: peer.provider,
            profile: peer.profile,
            agent_persona: assigned_task.and_then(|t| t.target_agent.clone()),
            state: peer.state,
            active_tasks: peer.active_tasks,
            leased_files: peer.leased_files,
            unread_messages: peer.unread_messages,
            turns,
            tokens,
            cost_usd,
            running_tool: None,
            quiet_ns: None,
            worktree: assigned_task.and_then(|t| t.worktree.clone()),
            branch: assigned_task.and_then(|t| t.branch.clone()),
        });
    }

    let mut coordinator: Option<SwarmMember> = None;
    let mut workers: Vec<SwarmMember> = Vec::new();
    for mut m in raw_members {
        if m.role == SwarmRole::Coordinator && coordinator.is_none() {
            coordinator = Some(m);
        } else {
            m.role = SwarmRole::Worker;
            workers.push(m);
        }
    }
    // Workers in the order they joined: their first task, then launch id.
    // Neither changes while the swarm runs, so neither does the order.
    let joined = |launch: &str| {
        tasks
            .iter()
            .filter(|t| t.assigned_to.as_deref() == Some(launch))
            .map(|t| t.created_ns)
            .min()
            .unwrap_or(i64::MAX)
    };
    workers.sort_by(|a, b| {
        joined(&a.launch_id)
            .cmp(&joined(&b.launch_id))
            .then_with(|| a.launch_id.cmp(&b.launch_id))
    });
    assign_names(coordinator.as_mut(), &mut workers);

    let mut total_turns = 0usize;
    let mut total_tokens = 0i64;
    let mut total_cost_usd = 0.0f64;
    if let Some(c) = &coordinator {
        total_turns += c.turns;
        total_tokens += c.tokens;
        total_cost_usd += c.cost_usd;
    }
    for w in &workers {
        total_turns += w.turns;
        total_tokens += w.tokens;
        total_cost_usd += w.cost_usd;
    }

    SwarmSnapshot {
        workspace,
        goal: state.as_ref().and_then(|st| st.goal.clone()),
        max_workers: state.as_ref().and_then(|st| st.max_workers),
        isolation: state.as_ref().and_then(|st| st.isolation),
        started_ns: state.as_ref().map(|st| st.started_ns),
        coordinator,
        workers,
        tasks,
        leases,
        messages,
        total_turns,
        total_tokens,
        total_cost_usd,
    }
}

/// Names every member `persona·harness` (the coordinator `coordinator·harness`),
/// adding the launch id's first four characters where two names collide. A
/// name depends on the member itself, never on its position, so it never
/// passes to another agent (as `W2` did when `W1` left); a suffix only drops
/// once its twin is gone.
fn assign_names(coordinator: Option<&mut SwarmMember>, workers: &mut [SwarmMember]) {
    let base = |m: &SwarmMember| {
        let persona = match m.role {
            SwarmRole::Coordinator => "coordinator",
            SwarmRole::Worker => m.agent_persona.as_deref().unwrap_or("worker"),
        };
        format!("{persona}·{}", m.provider)
    };
    let mut counts: HashMap<String, usize> = HashMap::new();
    for w in workers.iter() {
        *counts.entry(base(w)).or_default() += 1;
    }
    for w in workers.iter_mut() {
        let name = base(w);
        w.label = if counts.get(&name).copied().unwrap_or(0) > 1 {
            let id = short_id(&w.launch_id);
            format!("{name} {}", &id[..id.len().min(4)])
        } else {
            name
        };
    }
    if let Some(c) = coordinator {
        c.label = base(c);
    }
}

/// How long a question may go unanswered, or a lane sit idle on a task,
/// before it needs the operator (decided 2026-10-30).
pub const STUCK_AFTER_NS: i64 = 5 * 60 * 1_000_000_000;

/// Where a swarm is in its life, from its tasks and its coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    NotStarted,
    Planning,
    Executing,
    WrappingUp,
    Done,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::NotStarted => "not started",
            Phase::Planning => "planning",
            Phase::Executing => "executing",
            Phase::WrappingUp => "wrapping up",
            Phase::Done => "done",
        }
    }
}

/// What a lane is: the coordinator's, an agent's, or a task nobody holds yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneKind {
    Coordinator,
    Agent,
    Queued,
}

/// A lane's state, in the order [`SwarmSnapshot::lanes`] decides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneState {
    /// Its task failed.
    Failed,
    /// Its task is still open but its session exited.
    Lost,
    /// Its session waits for the operator (a permission prompt).
    Attention,
    /// It asked a question nobody has answered.
    Waiting,
    Working,
    Done,
    /// Idle with a task that is not finished.
    Idle,
    /// Live with no task.
    Ready,
    Exited,
    Queued,
}

impl LaneState {
    pub fn glyph(self) -> &'static str {
        match self {
            LaneState::Failed => "✗",
            LaneState::Lost => "!",
            LaneState::Attention => "⚑",
            LaneState::Waiting => "◷",
            LaneState::Working => "●",
            LaneState::Done => "✓",
            LaneState::Idle => "○",
            LaneState::Ready | LaneState::Queued => "◦",
            LaneState::Exited => "·",
        }
    }
}

/// One row of Mission Control: an agent and the task it holds, the
/// coordinator, or a queued task.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    /// The launch id, or the task id of a queued lane: what the selection
    /// follows across refreshes.
    pub key: String,
    pub kind: LaneKind,
    pub name: String,
    pub launch_id: Option<String>,
    pub session_idx: Option<usize>,
    pub provider: Option<String>,
    pub task: Option<BusTask>,
    pub state: LaneState,
    /// What it is doing now, in a few words.
    pub activity: String,
    /// Where its task works: `shared · 2 leases`, `wt <branch>`.
    pub placement: String,
    pub leases: Vec<String>,
    /// When its oldest unanswered question was asked.
    pub waiting_since_ns: Option<i64>,
    pub unread: usize,
    /// How long its session has printed nothing, for a live lane.
    pub quiet_ns: Option<i64>,
}

fn first_line(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > max {
        let cut: String = line.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        line.to_string()
    }
}

fn result_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `4m`, `2h`, `35s`: a short age for activity lines.
pub fn short_age(ns: i64) -> String {
    let s = (ns / 1_000_000_000).max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3_600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3_600)
    }
}

impl SwarmSnapshot {
    /// Questions nobody has answered: a `question` is answered by a later
    /// message that names it in `reply_to`, or by a later message to its
    /// sender from its addressee (from anyone, for a broadcast question).
    pub fn open_questions(&self) -> Vec<&BusMessage> {
        self.messages
            .iter()
            .filter(|q| q.topic == "question")
            .filter(|q| {
                !self.messages.iter().any(|m| {
                    m.created_ns >= q.created_ns
                        && m.id != q.id
                        && (m.reply_to.as_deref() == Some(q.id.as_str())
                            || (m.to_launch.as_deref() == Some(q.from_launch.as_str())
                                && q.to_launch.as_ref().is_none_or(|to| *to == m.from_launch)))
                })
            })
            .collect()
    }

    /// Where the swarm is: see [`Phase`].
    pub fn phase(&self) -> Phase {
        let coordinator_live = self.coordinator.as_ref().is_some_and(|c| c.is_active());
        if self.tasks.is_empty() {
            return if coordinator_live {
                Phase::Planning
            } else {
                Phase::NotStarted
            };
        }
        if self.tasks.iter().any(|t| !t.status.is_terminal()) {
            Phase::Executing
        } else if coordinator_live {
            Phase::WrappingUp
        } else {
            Phase::Done
        }
    }

    /// The lanes, coordinator first, then agents in the order they joined,
    /// then queued tasks oldest first. `now` is in nanoseconds.
    pub fn lanes(&self, now: i64) -> Vec<Lane> {
        let open = self.open_questions();
        let mut out = Vec::new();
        if let Some(c) = &self.coordinator {
            let tasks = self.tasks.len();
            let done = self
                .tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Done)
                .count();
            let mut lane = self.member_lane(c, LaneKind::Coordinator, &open, now);
            lane.placement = match (tasks, open.len()) {
                (0, _) => "plan: no tasks yet".into(),
                (n, 0) => format!("plan: {done}/{n} tasks done"),
                (n, q) => format!(
                    "plan: {done}/{n} tasks done · {q} open question{}",
                    if q == 1 { "" } else { "s" }
                ),
            };
            out.push(lane);
        }
        for w in &self.workers {
            out.push(self.member_lane(w, LaneKind::Agent, &open, now));
        }
        let mut queued: Vec<&BusTask> = self
            .tasks
            .iter()
            .filter(|t| !t.status.is_terminal() && t.assigned_to.is_none())
            .collect();
        queued.sort_by_key(|t| t.created_ns);
        for t in queued {
            out.push(Lane {
                key: t.id.clone(),
                kind: LaneKind::Queued,
                name: "queued".into(),
                launch_id: None,
                session_idx: None,
                provider: t.target_harness.clone(),
                task: Some(t.clone()),
                state: LaneState::Queued,
                activity: match (&t.target_agent, &t.target_harness) {
                    (Some(a), Some(h)) => format!("for {a}·{h}"),
                    (Some(a), None) => format!("for {a}"),
                    (None, Some(h)) => format!("for {h}"),
                    (None, None) => "waiting for a worker".into(),
                },
                placement: placement(t, 0),
                leases: Vec::new(),
                waiting_since_ns: None,
                unread: 0,
                quiet_ns: None,
            });
        }
        out
    }

    fn member_lane(&self, m: &SwarmMember, kind: LaneKind, open: &[&BusMessage], now: i64) -> Lane {
        let held: Vec<&BusTask> = self
            .tasks
            .iter()
            .filter(|t| t.assigned_to.as_deref() == Some(m.launch_id.as_str()))
            .collect();
        let task = held
            .iter()
            .rev()
            .find(|t| !t.status.is_terminal())
            .or_else(|| held.last())
            .copied();
        let waiting = open
            .iter()
            .filter(|q| q.from_launch == m.launch_id)
            .min_by_key(|q| q.created_ns);
        let live = m.is_active();
        let status = task.map(|t| t.status);
        let state = match status {
            Some(TaskStatus::Failed) => LaneState::Failed,
            Some(s) if !s.is_terminal() && !live => LaneState::Lost,
            _ if live && m.state == "attention" => LaneState::Attention,
            _ if waiting.is_some() => LaneState::Waiting,
            _ if live && m.state == "working" => LaneState::Working,
            Some(TaskStatus::Done) => LaneState::Done,
            Some(s) if !s.is_terminal() => LaneState::Idle,
            _ if live => LaneState::Ready,
            _ => LaneState::Exited,
        };
        let activity = match state {
            LaneState::Failed => match task.and_then(|t| t.error.as_deref()) {
                Some(e) if !e.trim().is_empty() => format!("failed · {}", first_line(e, 48)),
                _ => "failed".into(),
            },
            LaneState::Lost => "session exited mid-task".into(),
            LaneState::Attention => "needs you in its terminal".into(),
            LaneState::Waiting => {
                let q = waiting.expect("waiting has a question");
                let to = match q.to_launch.as_deref() {
                    Some(t) if is_operator(t) => "you".to_string(),
                    Some(t) => self.member_label(t),
                    None => "everyone".into(),
                };
                format!("waiting {} on {to}", short_age(now - q.created_ns))
            }
            LaneState::Working => match &m.running_tool {
                Some(tool) => format!("running {tool}"),
                None => "working".into(),
            },
            LaneState::Done => match task.and_then(|t| t.result.as_ref()).map(result_text) {
                Some(r) if !r.trim().is_empty() => format!("done · {}", first_line(&r, 48)),
                _ => "done".into(),
            },
            LaneState::Idle => "idle".into(),
            LaneState::Ready => "ready, no task".into(),
            LaneState::Exited => "session exited".into(),
            LaneState::Queued => unreachable!("an agent lane is never queued"),
        };
        let leases: Vec<String> = self
            .leases
            .iter()
            .filter(|l| l.owner_launch == m.launch_id)
            .map(|l| l.pattern.clone())
            .collect();
        Lane {
            key: m.launch_id.clone(),
            kind,
            name: m.label.clone(),
            launch_id: Some(m.launch_id.clone()),
            session_idx: m.session_idx,
            provider: Some(m.provider.clone()),
            task: task.cloned(),
            state,
            activity,
            placement: task.map(|t| placement(t, leases.len())).unwrap_or_default(),
            leases,
            waiting_since_ns: waiting.map(|q| q.created_ns),
            unread: m.unread_messages,
            quiet_ns: m.quiet_ns,
        }
    }
}

/// What a [`Need`] asks of the operator, in the order the strip lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NeedKind {
    /// A session waits on a prompt in its terminal.
    Attention,
    /// A question to the operator, or one nobody answered for `STUCK_AFTER`.
    Question,
    Failed,
    /// A task still open whose session exited.
    Lost,
    /// Idle on an unfinished task for `STUCK_AFTER`, no tool running.
    Stuck,
    /// The worker cap is reached while tasks wait.
    CapReached,
    /// A task finished in a worktree: its branch waits for review.
    Review,
}

impl NeedKind {
    pub fn label(self) -> &'static str {
        match self {
            NeedKind::Attention => "needs you",
            NeedKind::Question => "question",
            NeedKind::Failed => "failed",
            NeedKind::Lost => "lost",
            NeedKind::Stuck => "stuck",
            NeedKind::CapReached => "cap",
            NeedKind::Review => "review",
        }
    }

    /// Whether `r` can retry it (re-open the task and delegate it again).
    pub fn retryable(self) -> bool {
        matches!(self, NeedKind::Failed | NeedKind::Lost)
    }
}

/// One thing in the swarm only the operator can resolve.
#[derive(Debug, Clone, PartialEq)]
pub struct Need {
    pub kind: NeedKind,
    /// Identifies this occurrence, so a dismissal does not hide the next one
    /// (a task failing again, a later question).
    pub key: String,
    /// The lane it is about, when there is one.
    pub lane_key: Option<String>,
    pub text: String,
    pub since_ns: i64,
    pub message_id: Option<String>,
    pub task_id: Option<String>,
}

fn is_operator(launch: &str) -> bool {
    matches!(launch, "user" | "operator" | "cli")
}

/// A line of the Activity timeline: a message, or something that happened
/// to a task or a lease.
#[derive(Debug, Clone, PartialEq)]
pub enum ActivityItem<'a> {
    Message(&'a BusMessage),
    Event {
        at_ns: i64,
        /// Who did it, a launch id.
        who: Option<String>,
        text: String,
        /// The lane it concerns, for `Enter`.
        lane_key: Option<String>,
    },
}

impl ActivityItem<'_> {
    pub fn at_ns(&self) -> i64 {
        match self {
            ActivityItem::Message(m) => m.created_ns,
            ActivityItem::Event { at_ns, .. } => *at_ns,
        }
    }
}

impl SwarmSnapshot {
    /// What only the operator can resolve, most urgent first, each kind
    /// oldest first. `lanes` is `self.lanes(now)`.
    pub fn needs(&self, lanes: &[Lane], now: i64) -> Vec<Need> {
        let mut out = Vec::new();
        let lane_of = |launch: &str| {
            lanes
                .iter()
                .find(|l| l.launch_id.as_deref() == Some(launch))
        };
        for lane in lanes.iter().filter(|l| l.state == LaneState::Attention) {
            out.push(Need {
                kind: NeedKind::Attention,
                key: format!("attention:{}", lane.key),
                lane_key: Some(lane.key.clone()),
                text: format!("{} waits on a prompt in its terminal", lane.name),
                since_ns: now,
                message_id: None,
                task_id: None,
            });
        }
        for q in self.open_questions() {
            let to_operator = q.to_launch.as_deref().is_some_and(is_operator);
            if !to_operator && now - q.created_ns < STUCK_AFTER_NS {
                continue;
            }
            let from = self.member_label(&q.from_launch);
            let to = match q.to_launch.as_deref() {
                Some(t) if is_operator(t) => "you".to_string(),
                Some(t) => self.member_label(t),
                None => "everyone".into(),
            };
            out.push(Need {
                kind: NeedKind::Question,
                key: format!("question:{}", q.id),
                lane_key: lane_of(&q.from_launch).map(|l| l.key.clone()),
                text: format!("{from} asks {to}: \"{}\"", first_line(&q.body, 72)),
                since_ns: q.created_ns,
                message_id: Some(q.id.clone()),
                task_id: None,
            });
        }
        for t in self.tasks.iter().filter(|t| t.status == TaskStatus::Failed) {
            let who = t
                .assigned_to
                .as_deref()
                .map(|a| self.member_label(a))
                .unwrap_or_else(|| "a worker".into());
            let why = t
                .error
                .as_deref()
                .filter(|e| !e.trim().is_empty())
                .map(|e| format!(": {}", first_line(e, 48)))
                .unwrap_or_default();
            out.push(Need {
                kind: NeedKind::Failed,
                key: format!("failed:{}:{}", t.id, t.ended_ns.unwrap_or(t.updated_ns)),
                lane_key: t
                    .assigned_to
                    .as_deref()
                    .and_then(lane_of)
                    .map(|l| l.key.clone()),
                text: format!("{who} failed \"{}\"{why}", t.title),
                since_ns: t.ended_ns.unwrap_or(t.updated_ns),
                message_id: None,
                task_id: Some(t.id.clone()),
            });
        }
        for lane in lanes {
            let Some(task) = &lane.task else { continue };
            match lane.state {
                LaneState::Lost => out.push(Need {
                    kind: NeedKind::Lost,
                    key: format!("lost:{}", task.id),
                    lane_key: Some(lane.key.clone()),
                    text: format!(
                        "{}'s session exited with \"{}\" unfinished",
                        lane.name, task.title
                    ),
                    since_ns: task.updated_ns,
                    message_id: None,
                    task_id: Some(task.id.clone()),
                }),
                LaneState::Idle if lane.quiet_ns.is_some_and(|q| q >= STUCK_AFTER_NS) => {
                    let quiet = lane.quiet_ns.unwrap_or(0);
                    out.push(Need {
                        kind: NeedKind::Stuck,
                        key: format!("stuck:{}:{}", lane.key, task.id),
                        lane_key: Some(lane.key.clone()),
                        text: format!(
                            "{} has been quiet {} on \"{}\"",
                            lane.name,
                            short_age(quiet),
                            task.title
                        ),
                        since_ns: now - quiet,
                        message_id: None,
                        task_id: Some(task.id.clone()),
                    });
                }
                _ => {}
            }
        }
        let queued: Vec<&Lane> = lanes
            .iter()
            .filter(|l| l.kind == LaneKind::Queued)
            .collect();
        if let Some(cap) = self.max_workers
            && let Some(first) = queued.first()
        {
            let active = self.workers.iter().filter(|w| w.is_active()).count();
            if active >= cap {
                out.push(Need {
                    kind: NeedKind::CapReached,
                    key: format!("cap:{}", first.key),
                    lane_key: Some(first.key.clone()),
                    text: format!(
                        "all {cap} workers are busy and {} task{} wait{}",
                        queued.len(),
                        if queued.len() == 1 { "" } else { "s" },
                        if queued.len() == 1 { "s" } else { "" }
                    ),
                    since_ns: first.task.as_ref().map_or(now, |t| t.created_ns),
                    message_id: None,
                    task_id: None,
                });
            }
        }
        for t in self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done && t.isolation == TaskIsolation::Worktree)
        {
            let who = t
                .assigned_to
                .as_deref()
                .map(|a| self.member_label(a))
                .unwrap_or_else(|| "a worker".into());
            out.push(Need {
                kind: NeedKind::Review,
                key: format!("review:{}", t.id),
                lane_key: t
                    .assigned_to
                    .as_deref()
                    .and_then(lane_of)
                    .map(|l| l.key.clone()),
                text: format!(
                    "{who} finished \"{}\"{}: review it",
                    t.title,
                    t.branch
                        .as_deref()
                        .map(|b| format!(" on {b}"))
                        .unwrap_or_default()
                ),
                since_ns: t.ended_ns.unwrap_or(t.updated_ns),
                message_id: None,
                task_id: Some(t.id.clone()),
            });
        }
        out.sort_by_key(|n| (n.kind, n.since_ns));
        out
    }

    /// Messages and the events of tasks and leases, oldest first: a task
    /// planned or added, finished, failed or cancelled; a lease taken.
    pub fn timeline(&self) -> Vec<ActivityItem<'_>> {
        let mut out: Vec<ActivityItem<'_>> =
            self.messages.iter().map(ActivityItem::Message).collect();
        let lane_key = |launch: Option<&str>| {
            launch
                .filter(|l| self.all_members().iter().any(|m| m.launch_id == *l))
                .map(str::to_string)
        };
        for t in &self.tasks {
            let creator_is_coord = self
                .coordinator
                .as_ref()
                .is_some_and(|c| c.launch_id == t.created_by);
            out.push(ActivityItem::Event {
                at_ns: t.created_ns,
                who: Some(t.created_by.clone()),
                text: format!(
                    "{} \"{}\"",
                    if creator_is_coord { "planned" } else { "added" },
                    t.title
                ),
                lane_key: lane_key(t.assigned_to.as_deref()),
            });
            if let Some(end) = t.ended_ns {
                let verb = match t.status {
                    TaskStatus::Done => "finished",
                    TaskStatus::Failed => "failed",
                    _ => "cancelled",
                };
                out.push(ActivityItem::Event {
                    at_ns: end,
                    who: t.assigned_to.clone(),
                    text: format!("{verb} \"{}\"", t.title),
                    lane_key: lane_key(t.assigned_to.as_deref()),
                });
            }
        }
        for l in &self.leases {
            out.push(ActivityItem::Event {
                at_ns: l.acquired_ns,
                who: Some(l.owner_launch.clone()),
                text: format!("took a lease on {}", l.pattern),
                lane_key: lane_key(Some(&l.owner_launch)),
            });
        }
        out.sort_by_key(ActivityItem::at_ns);
        out
    }
}

fn placement(t: &BusTask, leases: usize) -> String {
    match (t.isolation, &t.branch) {
        (TaskIsolation::Worktree, Some(b)) => format!("wt {b}"),
        (TaskIsolation::Worktree, None) => "worktree".into(),
        (TaskIsolation::Shared, _) if leases > 0 => format!(
            "shared · {leases} lease{}",
            if leases == 1 { "" } else { "s" }
        ),
        (TaskIsolation::Shared, _) => "shared".into(),
    }
}

fn query_launch_totals(conn: &Connection, launch_id: &str) -> Option<(usize, i64, f64)> {
    conn.query_row(
        "SELECT COUNT(DISTINCT t.id),
                COALESCE(SUM(o.total_tokens), 0),
                COALESCE(SUM(o.total_cost_usd), 0.0)
         FROM traces t
         LEFT JOIN observations o ON o.trace_id = t.id
         WHERE t.launch_id = ?1",
        params![launch_id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?.max(0) as usize,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
            ))
        },
    )
    .ok()
}

/// The task a pre-warmed worker holds until the coordinator hands it real work.
pub const STANDBY_WORKER_INSTRUCTIONS: &str = "You are a standby worker of this workspace's swarm. \
Do not change any file yet. Call `agent_mux_recv_messages(wait_ms = 30000)` and `agent_mux_task_board` \
until the Swarm Coordinator assigns you a task or messages you; then claim that task with \
`agent_mux_task_update(action = \"claim\")` and do it. When the coordinator says the swarm is finished, \
mark this standby task done.";

/// Composes the opening mission prompt handed to a newly spawned Swarm Coordinator.
pub fn compose_coordinator_prompt(
    goal: &str,
    max_workers: usize,
    isolation: TaskIsolation,
) -> String {
    format!(
        "You are the **Swarm Coordinator** for this workspace.\n\n\
         ### Swarm Mission Goal\n\
         {goal}\n\n\
         ### Swarm Operating Parameters\n\
         - **Max Parallel Workers**: {max_workers}\n\
         - **Default Task Isolation**: `{}` ({})\n\n\
         ### Your Coordination Playbook\n\
         1. **Orient**: Inspect the codebase (read-only) and call `agent_mux_list_peers` and `agent_mux_task_board`.\n\
         2. **Decompose & Delegate**:\n\
            - Break the mission into independent subtasks with clear `files` globs.\n\
            - If idle worker peers are already running in the workspace, publish subtasks with `agent_mux_task_split` and wake them via `agent_mux_send_message(wake = true)`.\n\
            - Otherwise, spawn worker sessions directly via `agent_mux_delegate` (setting `isolation = \"{}\"` and `wait_ms = 0` so workers execute in parallel).\n\
         3. **Monitor & Communicate**:\n\
            - Wait for worker updates and questions using `agent_mux_recv_messages(wait_ms = 30000)` and `agent_mux_task_board`.\n\
            - Answer questions with `agent_mux_send_message(to = \"<worker_launch_id>\", wake = true)`.\n\
            - Encourage workers on coupled components to exchange `api-contract` messages directly.\n\
         4. **Verify & Synthesize**:\n\
            - Once all tasks reach `done`, review the changes/results, run final verification, and present a concise summary.",
        isolation.as_str(),
        match isolation {
            TaskIsolation::Shared => "cooperative file leases enforced by PreToolUse guard",
            TaskIsolation::Worktree => "isolated git worktree branch per worker",
        },
        isolation.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::NewTaskSpec;
    use crate::tracing::pricing::PriceTable;
    use crate::tracing::store::{OpenOptions, open_aux, open_rw};

    #[test]
    fn snapshot_identifies_coordinator_workers_tasks_and_messages() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let _ = open_rw(
            &db,
            OpenOptions {
                prices: PriceTable::builtin(),
                run_id: "run-swarm".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();
        let conn = open_aux(&db).unwrap();
        let ws = bstore::normalize_workspace(dir.path());

        bstore::set_coordinator(
            &conn,
            Some(&db),
            &ws,
            "coord-11112222",
            Some("Build auth + UI in parallel"),
            Some(4),
            Some(TaskIsolation::Shared),
            1_000,
        )
        .unwrap();

        let created = bstore::split_tasks(
            &conn,
            Some(&db),
            &ws,
            "coord-11112222",
            &[NewTaskSpec {
                title: "Implement auth".into(),
                instructions: "Add JWT validation".into(),
                files: vec!["src/auth/**".into()],
                assigned_to: Some("worker-aaaa1111".into()),
                ..Default::default()
            }],
            2_000,
        )
        .unwrap();
        assert_eq!(created.len(), 1);

        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "worker-aaaa1111",
            Some("coord-11112222"),
            "question",
            "Should tokens expire in 1h?",
            None,
            3_000,
        )
        .unwrap();

        let live = vec![
            LiveSwarmSession {
                session_idx: 0,
                session_id: 1,
                launch_id: "coord-11112222".into(),
                provider: "claude".into(),
                profile_name: "Claude Code".into(),
                agent_persona: Some("coordinator".into()),
                state: "working".into(),
                workspace: ws.clone(),
                is_delegated_worker: false,
                turns: 4,
                tokens: 12_000,
                cost_usd: 0.15,
                running_tool: Some("agent_mux_task_board".into()),
                quiet_ns: None,
            },
            LiveSwarmSession {
                session_idx: 1,
                session_id: 2,
                launch_id: "worker-aaaa1111".into(),
                provider: "codex".into(),
                profile_name: "codex · task".into(),
                agent_persona: Some("reviewer".into()),
                state: "working".into(),
                workspace: ws.clone(),
                is_delegated_worker: true,
                turns: 2,
                tokens: 5_000,
                cost_usd: 0.05,
                running_tool: None,
                quiet_ns: None,
            },
        ];

        let snap = load_snapshot(Some(&conn), dir.path(), &live, None, 4_000);
        assert_eq!(snap.goal.as_deref(), Some("Build auth + UI in parallel"));
        assert_eq!(snap.max_workers, Some(4));
        let coord = snap.coordinator.as_ref().expect("coordinator present");
        assert_eq!(coord.launch_id, "coord-11112222");
        assert_eq!(coord.unread_messages, 1);
        assert_eq!(snap.workers.len(), 1);
        assert_eq!(snap.workers[0].label, "reviewer·codex");
        assert_eq!(snap.workers[0].leased_files, vec!["src/auth/**"]);
        assert_eq!(snap.leases.len(), 1);
        // Recording the coordinator posts nothing on the bus: the feed holds
        // only the worker's question.
        assert_eq!(snap.messages.len(), 1);
        assert_eq!(snap.total_turns, 6);
        assert_eq!(snap.total_tokens, 17_000);
        assert_eq!(snap.member_label("coord-11112222"), "coordinator·claude");
        assert_eq!(snap.member_label("worker-aaaa1111"), "reviewer·codex");
    }

    fn live(
        idx: usize,
        launch: &str,
        persona: Option<&str>,
        provider: &str,
        state: &str,
        ws: &str,
    ) -> LiveSwarmSession {
        LiveSwarmSession {
            session_idx: idx,
            session_id: idx + 1,
            launch_id: launch.into(),
            provider: provider.into(),
            profile_name: provider.into(),
            agent_persona: persona.map(str::to_string),
            state: state.into(),
            workspace: ws.into(),
            is_delegated_worker: persona != Some("coordinator"),
            turns: 0,
            tokens: 0,
            cost_usd: 0.0,
            running_tool: None,
            quiet_ns: None,
        }
    }

    fn store() -> (tempfile::TempDir, std::path::PathBuf, Connection, String) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let _ = open_rw(
            &db,
            OpenOptions {
                prices: PriceTable::builtin(),
                run_id: "run-lanes".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();
        let conn = open_aux(&db).unwrap();
        let ws = bstore::normalize_workspace(dir.path());
        (dir, db, conn, ws)
    }

    fn task(
        conn: &Connection,
        db: &Path,
        ws: &str,
        title: &str,
        to: Option<&str>,
        at: i64,
    ) -> String {
        bstore::split_tasks(
            conn,
            Some(db),
            ws,
            "coord-1",
            &[NewTaskSpec {
                title: title.into(),
                instructions: "…".into(),
                assigned_to: to.map(str::to_string),
                ..Default::default()
            }],
            at,
        )
        .unwrap()
        .remove(0)
        .id
    }

    #[test]
    fn names_are_stable_and_disambiguated_not_positional() {
        let (dir, db, conn, ws) = store();
        task(&conn, &db, &ws, "a", Some("aaaa1111"), 1);
        task(&conn, &db, &ws, "b", Some("bbbb2222"), 2);
        task(&conn, &db, &ws, "c", Some("cccc3333"), 3);
        let all = vec![
            live(0, "coord-1", Some("coordinator"), "claude", "working", &ws),
            live(1, "cccc3333", Some("tester"), "agy", "working", &ws),
            live(2, "aaaa1111", Some("implementer"), "codex", "working", &ws),
            live(3, "bbbb2222", Some("implementer"), "codex", "working", &ws),
        ];
        let snap = load_snapshot(Some(&conn), dir.path(), &all, None, 10);
        let names: Vec<_> = snap.workers.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(
            names,
            [
                "implementer·codex aaaa",
                "implementer·codex bbbb",
                "tester·agy"
            ],
            "joined order, duplicates get an id"
        );
        assert_eq!(snap.member_label("coord-1"), "coordinator·claude");

        // The first implementer leaves. No name passes to another agent: the
        // tester keeps its own, the twin drops a suffix it no longer needs,
        // and the one who left is its short id.
        let fewer: Vec<_> = all
            .iter()
            .filter(|s| s.launch_id != "aaaa1111")
            .cloned()
            .collect();
        let _ = bstore::update_task(
            &conn,
            Some(&db),
            &snap.tasks[0].id,
            "aaaa1111",
            TaskStatus::Cancelled,
            None,
            None,
            None,
            None,
            11,
        );
        let snap = load_snapshot(Some(&conn), dir.path(), &fewer, None, 12);
        assert_eq!(snap.member_label("cccc3333"), "tester·agy");
        assert_eq!(snap.member_label("bbbb2222"), "implementer·codex");
        assert_eq!(snap.member_label("aaaa1111"), "aaaa1111");
    }

    #[test]
    fn a_question_is_open_until_answered_by_reply_or_by_the_addressee() {
        let (dir, db, conn, ws) = store();
        let q = bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "w-1",
            Some("coord-1"),
            "question",
            "TTL?",
            None,
            1,
        )
        .unwrap();
        let snap = load_snapshot(Some(&conn), dir.path(), &[], None, 2);
        assert_eq!(snap.open_questions().len(), 1);

        // A message from someone else to the asker does not answer it.
        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "w-2",
            Some("w-1"),
            "message",
            "hi",
            None,
            3,
        )
        .unwrap();
        let snap = load_snapshot(Some(&conn), dir.path(), &[], None, 4);
        assert_eq!(snap.open_questions().len(), 1);

        // The addressee writing back does.
        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "coord-1",
            Some("w-1"),
            "answer",
            "1h",
            None,
            5,
        )
        .unwrap();
        let snap = load_snapshot(Some(&conn), dir.path(), &[], None, 6);
        assert!(snap.open_questions().is_empty());

        // A reply naming the question answers it, whoever sends it.
        let q2 = bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "w-1",
            None,
            "question",
            "anyone?",
            None,
            7,
        )
        .unwrap();
        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "user",
            None,
            "answer",
            "me",
            Some(&q2.id),
            8,
        )
        .unwrap();
        let snap = load_snapshot(Some(&conn), dir.path(), &[], None, 9);
        assert!(snap.open_questions().is_empty(), "{:?}", q.id);
    }

    #[test]
    fn lanes_carry_state_activity_and_queued_tasks_and_the_phase_follows_the_tasks() {
        let (dir, db, conn, ws) = store();
        let min = 60 * 1_000_000_000i64;
        let snap = load_snapshot(Some(&conn), dir.path(), &[], None, 0);
        assert_eq!(snap.phase(), Phase::NotStarted);
        let coord = live(0, "coord-1", Some("coordinator"), "claude", "working", &ws);
        let snap = load_snapshot(
            Some(&conn),
            dir.path(),
            std::slice::from_ref(&coord),
            None,
            0,
        );
        assert_eq!(snap.phase(), Phase::Planning);

        task(&conn, &db, &ws, "wire bus", Some("w-1"), 1);
        task(&conn, &db, &ws, "ui tests", Some("w-2"), 2);
        task(&conn, &db, &ws, "migration", None, 3);
        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "w-1",
            Some("coord-1"),
            "question",
            "TTL?",
            None,
            min,
        )
        .unwrap();
        let mut w1 = live(1, "w-1", Some("implementer"), "codex", "idle", &ws);
        w1.running_tool = None;
        let mut w2 = live(2, "w-2", Some("tester"), "agy", "working", &ws);
        w2.running_tool = Some("Bash".into());
        let sessions = vec![coord.clone(), w1, w2];
        let snap = load_snapshot(Some(&conn), dir.path(), &sessions, None, 5 * min);
        assert_eq!(snap.phase(), Phase::Executing);

        let lanes = snap.lanes(5 * min);
        let summary: Vec<_> = lanes
            .iter()
            .map(|l| (l.kind, l.name.as_str(), l.state, l.activity.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    LaneKind::Coordinator,
                    "coordinator·claude",
                    LaneState::Working,
                    "working"
                ),
                (
                    LaneKind::Agent,
                    "implementer·codex",
                    LaneState::Waiting,
                    "waiting 4m on coordinator·claude"
                ),
                (
                    LaneKind::Agent,
                    "tester·agy",
                    LaneState::Working,
                    "running Bash"
                ),
                (
                    LaneKind::Queued,
                    "queued",
                    LaneState::Queued,
                    "waiting for a worker"
                ),
            ]
        );
        assert_eq!(lanes[0].placement, "plan: 0/3 tasks done · 1 open question");
        assert_eq!(lanes[1].waiting_since_ns, Some(min));
        assert_eq!(lanes[3].task.as_ref().unwrap().title, "migration");

        // The tester's session exits mid-task: lost. Its task fails: failed.
        let gone: Vec<_> = sessions
            .iter()
            .cloned()
            .map(|mut s| {
                if s.launch_id == "w-2" {
                    s.state = "exited".into();
                }
                s
            })
            .collect();
        let snap = load_snapshot(Some(&conn), dir.path(), &gone, None, 6 * min);
        assert_eq!(snap.lanes(6 * min)[2].state, LaneState::Lost);
        let tid = snap.lanes(6 * min)[2].task.as_ref().unwrap().id.clone();
        bstore::update_task(
            &conn,
            Some(&db),
            &tid,
            "w-2",
            TaskStatus::Failed,
            None,
            Some("cargo test exit 101".into()),
            None,
            None,
            7 * min,
        )
        .unwrap();
        let snap = load_snapshot(Some(&conn), dir.path(), &gone, None, 8 * min);
        let lane = &snap.lanes(8 * min)[2];
        assert_eq!(
            (lane.state, lane.activity.as_str()),
            (LaneState::Failed, "failed · cargo test exit 101")
        );
    }

    #[test]
    fn needs_list_what_only_the_operator_can_resolve_most_urgent_first() {
        let (dir, db, conn, ws) = store();
        let min = 60 * 1_000_000_000i64;
        bstore::set_coordinator(
            &conn,
            Some(&db),
            &ws,
            "coord-1",
            Some("g"),
            Some(3),
            None,
            0,
        )
        .unwrap();
        let failed = task(&conn, &db, &ws, "ui tests", Some("w-fail"), 1);
        bstore::update_task(
            &conn,
            Some(&db),
            &failed,
            "w-fail",
            TaskStatus::Failed,
            None,
            Some("exit 101".into()),
            None,
            None,
            2 * min,
        )
        .unwrap();
        task(&conn, &db, &ws, "wire bus", Some("w-idle"), 3);
        task(&conn, &db, &ws, "write docs", Some("w-gone"), 4);
        // A worktree task done: its branch waits for review.
        let wt = bstore::split_tasks(
            &conn,
            Some(&db),
            &ws,
            "coord-1",
            &[NewTaskSpec {
                title: "schema".into(),
                instructions: "…".into(),
                assigned_to: Some("w-done".into()),
                isolation: TaskIsolation::Worktree,
                ..Default::default()
            }],
            5,
        )
        .unwrap()
        .remove(0);
        bstore::update_task(
            &conn,
            Some(&db),
            &wt.id,
            "w-done",
            TaskStatus::Done,
            None,
            None,
            None,
            Some("bus-abc".into()),
            3 * min,
        )
        .unwrap();
        // A question to the operator counts at once; one to the coordinator only after 5 minutes.
        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "w-idle",
            Some("user"),
            "question",
            "Ship Friday?",
            None,
            9 * min,
        )
        .unwrap();
        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "w-gone",
            Some("coord-1"),
            "question",
            "TTL?",
            None,
            9 * min,
        )
        .unwrap();

        let mut idle = live(1, "w-idle", Some("implementer"), "codex", "idle", &ws);
        idle.quiet_ns = Some(6 * min);
        let sessions = vec![
            live(
                0,
                "coord-1",
                Some("coordinator"),
                "claude",
                "attention",
                &ws,
            ),
            idle,
            live(2, "w-gone", Some("writer"), "claude", "exited", &ws),
        ];
        let now = 10 * min;
        let snap = load_snapshot(Some(&conn), dir.path(), &sessions, None, now);
        let lanes = snap.lanes(now);
        let needs = snap.needs(&lanes, now);
        let kinds: Vec<_> = needs.iter().map(|n| n.kind).collect();
        assert_eq!(
            kinds,
            [
                NeedKind::Attention,
                NeedKind::Question,
                NeedKind::Failed,
                NeedKind::Lost,
                NeedKind::Review
            ],
            "{needs:#?}"
        );
        assert_eq!(
            needs[1].text,
            "implementer·codex asks you: \"Ship Friday?\""
        );
        assert_eq!(needs[2].text, "w-fail failed \"ui tests\": exit 101");
        assert!(needs[2].kind.retryable() && needs[3].kind.retryable());
        assert!(
            needs[4].text.ends_with("on bus-abc: review it"),
            "{}",
            needs[4].text
        );
        // The implementer has a question open, so it is waiting, not stuck.
        assert!(!kinds.contains(&NeedKind::Stuck));

        // Five minutes later the coordinator's question is overdue; the answered
        // one is gone, and a quiet lane with nothing to ask is stuck.
        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "user",
            Some("w-idle"),
            "answer",
            "yes",
            None,
            11 * min,
        )
        .unwrap();
        let now = 15 * min;
        let snap = load_snapshot(Some(&conn), dir.path(), &sessions, None, now);
        let lanes = snap.lanes(now);
        let needs = snap.needs(&lanes, now);
        let texts: Vec<_> = needs.iter().map(|n| (n.kind, n.text.as_str())).collect();
        assert!(
            texts.contains(&(
                NeedKind::Question,
                "writer·claude asks coordinator·claude: \"TTL?\""
            )),
            "{texts:#?}"
        );
        assert!(
            texts.contains(&(
                NeedKind::Stuck,
                "implementer·codex has been quiet 6m on \"wire bus\""
            )),
            "{texts:#?}"
        );
        assert!(!texts.iter().any(|(_, t)| t.contains("Ship Friday")));
    }

    #[test]
    fn the_cap_needs_you_only_while_tasks_wait() {
        let (dir, db, conn, ws) = store();
        bstore::set_coordinator(&conn, Some(&db), &ws, "coord-1", None, Some(1), None, 0).unwrap();
        task(&conn, &db, &ws, "a", Some("w-1"), 1);
        let sessions = vec![live(1, "w-1", Some("implementer"), "codex", "working", &ws)];
        let snap = load_snapshot(Some(&conn), dir.path(), &sessions, None, 2);
        assert!(snap.needs(&snap.lanes(2), 2).is_empty());
        task(&conn, &db, &ws, "b", None, 3);
        let snap = load_snapshot(Some(&conn), dir.path(), &sessions, None, 4);
        let needs = snap.needs(&snap.lanes(4), 4);
        assert_eq!(needs.len(), 1);
        assert_eq!(needs[0].kind, NeedKind::CapReached);
        assert_eq!(needs[0].text, "all 1 workers are busy and 1 task waits");
    }

    #[test]
    fn a_failed_task_reopens_for_another_worker_and_a_done_one_does_not() {
        let (_dir, db, conn, ws) = store();
        let id = task(&conn, &db, &ws, "ui tests", Some("w-1"), 1);
        bstore::update_task(
            &conn,
            Some(&db),
            &id,
            "w-1",
            TaskStatus::Failed,
            None,
            Some("boom".into()),
            None,
            Some("bus-x".into()),
            2,
        )
        .unwrap();
        let t = bstore::reopen_task(&conn, Some(&db), &id, 3).unwrap();
        assert_eq!(t.status, TaskStatus::Open);
        assert_eq!(t.assigned_to, None);
        assert_eq!(t.error, None);
        assert_eq!(t.ended_ns, None);
        assert_eq!(
            t.branch.as_deref(),
            Some("bus-x"),
            "the branch stays for the retry"
        );
        bstore::update_task(
            &conn,
            Some(&db),
            &id,
            "w-2",
            TaskStatus::Done,
            None,
            None,
            None,
            None,
            4,
        )
        .unwrap();
        assert!(bstore::reopen_task(&conn, Some(&db), &id, 5).is_err());
    }

    #[test]
    fn the_timeline_merges_messages_with_task_and_lease_events() {
        let (dir, db, conn, ws) = store();
        let coord = live(0, "coord-1", Some("coordinator"), "claude", "working", &ws);
        let id = bstore::split_tasks(
            &conn,
            Some(&db),
            &ws,
            "coord-1",
            &[NewTaskSpec {
                title: "wire bus".into(),
                instructions: "…".into(),
                files: vec!["src/bus/**".into()],
                ..Default::default()
            }],
            10,
        )
        .unwrap()
        .remove(0)
        .id;
        bstore::claim_task(&conn, Some(&db), &id, "w-1", 20).unwrap();
        bstore::send_message(
            &conn,
            Some(&db),
            &ws,
            "w-1",
            Some("coord-1"),
            "message",
            "on it",
            None,
            30,
        )
        .unwrap();
        let snap = load_snapshot(Some(&conn), dir.path(), &[coord], None, 40);
        let line = |i: &ActivityItem| match i {
            ActivityItem::Message(m) => format!("msg {}", m.body),
            ActivityItem::Event { text, .. } => text.clone(),
        };
        let lines: Vec<_> = snap.timeline().iter().map(line).collect();
        assert_eq!(
            lines,
            [
                "planned \"wire bus\"",
                "took a lease on src/bus/**",
                "msg on it"
            ]
        );
    }
}
