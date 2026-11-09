//! What the coordinator, the CLI and the screen read of a swarm without
//! being the TUI: the board as the engine shows it, from the rows and the
//! live sessions the running TUIs publish, and a wait on the swarm's log
//! (coordinator-first spec §6: `agent_mux_swarm_board`, `agent_mux_swarm_wait`,
//! `agent-mux swarm status|wait|events`).

use super::engine::{self, Input, Shown, Worker, WorkerState};
use super::record::{self, BoardTask, SwarmEvent, SwarmRun};
use crate::bus::store as bstore;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

/// The longest one `swarm_wait` blocks (10 minutes): under any harness's
/// own tool-call timeout this is the ceiling, not the swarm's.
pub const MAX_SWARM_WAIT_MS: u64 = 600_000;
/// What a `swarm_wait` waits when the caller says nothing (5 minutes): the
/// coordinator then reports to the operator and asks whether to keep
/// waiting (§5.5).
pub const DEFAULT_SWARM_WAIT_MS: u64 = 300_000;

/// Live sessions as a process other than the TUI sees them: the published
/// snapshots of running agent-mux TUIs, by launch id.
pub fn snapshot_workers() -> HashMap<String, Worker> {
    use crate::tracing::analysis::RuntimeState;
    let root = crate::tracing::analysis::default_snapshot_dir();
    crate::tracing::analysis::read_snapshots(&root, crate::tracing::store::now_ns())
        .unwrap_or_default()
        .into_iter()
        .flat_map(|s| s.sessions)
        .map(|s| {
            let state = match s.state {
                RuntimeState::Working => WorkerState::Working,
                RuntimeState::WaitingForUser => WorkerState::Prompt,
                RuntimeState::Idle | RuntimeState::Unknown => WorkerState::Idle,
                RuntimeState::Exited | RuntimeState::Disconnected => WorkerState::Exited,
            };
            (s.launch_id, Worker { state, quiet_ns: 0 })
        })
        .collect()
}

/// One task as the board shows it.
#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub key: String,
    pub id: String,
    pub title: String,
    /// The shown state (`engine::Shown`): draft, blocked, ready, working,
    /// asking, stalled, prompt, done, review, failed, lost, cancelled,
    /// proposed.
    pub state: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub isolation: String,
    pub depends_on: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<String>,
    pub files: Vec<String>,
    /// The worker's launch id, once one started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    pub attempts: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offered_ns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launched_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// `merged` or `discarded`, for a reviewed branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<String>,
    /// The unanswered question it asked, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// Automatic retries so far (v26); `held` when the operator stopped
    /// its worker, so it waits for them.
    #[serde(skip_serializing_if = "is_zero")]
    pub auto_retries: i64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// Its branch's merge behind the check (v26): `queued`, `running`,
    /// `passed` (merged), `failed`, with the reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_gate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_gate_detail: Option<String>,
    pub updated_ns: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_ns: Option<i64>,
}

/// The swarm and its board, as `agent_mux_swarm_board` answers.
#[derive(Debug, Clone, Serialize)]
pub struct BoardView {
    pub swarm: SwarmRun,
    pub gate_mode: String,
    pub coordinator: Option<String>,
    pub worker_pool: Vec<String>,
    /// How finished tasks are reviewed: `off`, `auto` or a harness.
    pub review: String,
    /// Each finished task's review (v25), in board order.
    pub reviews: Vec<ReviewView>,
    /// The decision that approved the plan, if it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved: Option<record::SwarmDecision>,
    pub tasks: Vec<TaskView>,
    pub done: usize,
    pub total: usize,
    /// Workers holding a task now, against `swarm.max_workers`.
    pub busy: usize,
    /// Ready tasks nobody asked for yet: the coordinator launches them.
    pub ready: Vec<String>,
    /// Open questions, newest last: who asked (launch), to whom, the text.
    pub questions: Vec<QuestionView>,
    pub last_event_id: i64,
}

/// Where one finished task's review stands.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewView {
    /// The reviewed task's key.
    pub task: String,
    /// `pending`, `running`, `approved`, `changes`, `unclear`, `failed`,
    /// `skipped`.
    pub state: String,
    /// The review's own task on the board, once there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// The review's result: the verdict line, then the findings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// What merging it now would go against, if anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuestionView {
    pub id: String,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub body: String,
    pub created_ns: i64,
}

/// The board as the engine shows it, with `workers` as the live sessions
/// (the TUI's own, or [`snapshot_workers`]).
pub fn board_view(
    conn: &Connection,
    swarm: &SwarmRun,
    workers: &HashMap<String, Worker>,
    now_ns: i64,
) -> rusqlite::Result<BoardView> {
    let board = record::board(conn, &swarm.id)?;
    let messages = bstore::list_workspace_messages(conn, &swarm.workspace, 500)?;
    let open = super::open_questions(&messages);
    let asking: HashSet<String> = open.iter().map(|m| m.from_launch.clone()).collect();
    let settings = super::review::settings(conn, &swarm.id);
    let input = Input {
        swarm,
        board: &board,
        workers,
        asking: &asking,
        now_ns,
        dispatch_grace_ns: engine::DISPATCH_GRACE_NS,
        review: settings.enabled,
        merge_check: None,
    };
    let task_of = |launch: &str| -> Option<String> {
        board
            .iter()
            .find(|t| t.task.assigned_to.as_deref() == Some(launch))
            .map(|t| t.ext.plan_key.clone())
    };
    let tasks: Vec<TaskView> = board
        .iter()
        .map(|t| {
            let shown = engine::shown(t, &input);
            let question = t.task.assigned_to.as_deref().and_then(|l| {
                open.iter()
                    .rev()
                    .find(|m| m.from_launch == l)
                    .map(|m| m.body.clone())
            });
            task_view(t, shown, question)
        })
        .collect();
    let ready = board
        .iter()
        .filter(|t| {
            t.task.status == crate::bus::TaskStatus::Open
                && !t.ext.proposed
                && t.ext.launched_by.is_none()
                && swarm.status.approved()
                && engine::unmet_dependencies(t, &board).is_empty()
        })
        .map(|t| t.ext.plan_key.clone())
        .collect();
    let coordinator = record::coordinator(conn, &swarm.id)?.and_then(|c| c.launch);
    Ok(BoardView {
        gate_mode: swarm.gate_mode.as_str().to_string(),
        coordinator,
        worker_pool: record::worker_pool(conn, swarm),
        review: settings.as_choice(),
        reviews: board
            .iter()
            .filter_map(|t| {
                let state = super::review::state(t, &board, settings.enabled)?;
                let by = t
                    .ext
                    .review_task
                    .as_deref()
                    .and_then(|id| board.iter().find(|o| o.task.id == id));
                Some(ReviewView {
                    task: t.ext.plan_key.clone(),
                    state: state.as_str().to_string(),
                    review_key: by.map(|r| r.ext.plan_key.clone()),
                    harness: by.and_then(|r| r.task.target_harness.clone()),
                    verdict: by.and_then(super::review::result_text),
                    merge_warning: super::review::merge_warning(Some(state)).map(str::to_string),
                })
            })
            .collect(),
        approved: record::launch_decision(conn, &swarm.id),
        done: tasks.iter().filter(|t| t.status == "done").count(),
        total: tasks.len(),
        busy: engine::busy_workers(&input),
        ready,
        questions: open
            .iter()
            .map(|m| QuestionView {
                id: m.id.clone(),
                from: m.from_launch.clone(),
                to: m.to_launch.clone(),
                task: task_of(&m.from_launch),
                body: m.body.clone(),
                created_ns: m.created_ns,
            })
            .collect(),
        last_event_id: record::last_event_id(conn, &swarm.id)?,
        tasks,
        swarm: swarm.clone(),
    })
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

fn task_view(t: &BoardTask, shown: Shown, question: Option<String>) -> TaskView {
    TaskView {
        auto_retries: t.ext.recovery.retries,
        held: t.ext.recovery.held_ns.is_some(),
        merge_gate: t.ext.gate.as_ref().map(|g| g.state.as_str().to_string()),
        merge_gate_detail: t.ext.gate.as_ref().and_then(|g| g.detail.clone()),
        key: t.ext.plan_key.clone(),
        id: t.task.id.clone(),
        title: t.task.title.clone(),
        state: shown.as_str().to_string(),
        status: t.task.status.as_str().to_string(),
        harness: t.task.target_harness.clone(),
        agent: t.task.target_agent.clone(),
        isolation: t.task.isolation.as_str().to_string(),
        depends_on: t.ext.depends_on.clone(),
        acceptance: t.ext.acceptance.clone(),
        files: t.task.files.clone(),
        worker: t.task.assigned_to.clone(),
        attempts: t.ext.attempts,
        offered_ns: t.ext.offered_ns,
        launched_by: t.ext.launched_by.map(|b| b.as_str().to_string()),
        result: t.task.result.clone(),
        error: t.task.error.clone(),
        branch: t.task.branch.clone(),
        review: t.review.clone(),
        question,
        updated_ns: t.task.updated_ns,
        ended_ns: t.task.ended_ns,
    }
}

/// What a wait answered.
#[derive(Debug, Clone, Serialize)]
pub struct Waited {
    pub events: Vec<SwarmEvent>,
    /// Where the next wait starts.
    pub last_event_id: i64,
    /// Nothing happened within the wait: the coordinator reports to the
    /// operator and asks whether to keep waiting. Nothing was stopped.
    pub timed_out: bool,
    /// The swarm ended while waiting.
    pub ended: bool,
}

/// Blocks until the swarm logs an event after `after_id` (or the swarm
/// ends), or `wait_ms` pass (clamped to [`MAX_SWARM_WAIT_MS`]). Polls the
/// store every 250 ms; a timeout is an answer, never an error.
pub fn wait_events(
    db_path: &Path,
    swarm_id: &str,
    after_id: i64,
    wait_ms: u64,
) -> Result<Waited, String> {
    let deadline = Duration::from_millis(wait_ms.min(MAX_SWARM_WAIT_MS));
    let start = Instant::now();
    loop {
        let conn = crate::tracing::store::open_aux(db_path)?;
        let events =
            record::events_after(&conn, swarm_id, after_id, 200).map_err(|e| e.to_string())?;
        let ended = record::get_swarm(&conn, swarm_id)
            .map_err(|e| e.to_string())?
            .is_none_or(|s| s.ended_ns.is_some());
        if !events.is_empty() || ended || start.elapsed() >= deadline {
            let last = events.last().map(|e| e.id).unwrap_or(after_id);
            return Ok(Waited {
                timed_out: events.is_empty() && !ended,
                events,
                last_event_id: last,
                ended,
            });
        }
        drop(conn);
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// One event as a line of the log: time, actor, kind, what.
pub fn event_line(e: &SwarmEvent) -> String {
    let when = crate::tracing::cli::fmt_time(e.created_ns);
    let actor = e.actor.as_deref().unwrap_or("-");
    let detail = match &e.detail {
        Some(serde_json::Value::Object(m)) => {
            let mut parts: Vec<String> = Vec::new();
            for (k, v) in m {
                let v = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => continue,
                    other => other.to_string(),
                };
                if !v.is_empty() {
                    parts.push(format!("{k}={v}"));
                }
            }
            parts.join(" ")
        }
        Some(v) => v.to_string(),
        None => String::new(),
    };
    format!(
        "{when}  {:<22} {:<12} {}",
        e.kind,
        crate::bus::short_id(actor),
        detail.chars().take(160).collect::<String>()
    )
}
