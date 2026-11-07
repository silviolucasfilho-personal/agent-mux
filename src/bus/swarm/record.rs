//! The orchestrated swarm's rows (schema v21): `swarms`, one swarm's run
//! (several may be live per workspace since v23), and `swarm_tasks`, the
//! plan's extension of the board's `agent_tasks` rows (key, order,
//! dependencies, acceptance). See
//! `docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md` §7.
//! Schema v24 (`2026-11-05-swarm-coordinator-first-design.md` §8) adds the
//! side tables `swarm_settings` (the gate mode), `swarm_dispatch` (when a
//! ready task was offered to the coordinator and who asked for its worker),
//! `swarm_decisions` (every operator-level decision, with who relayed it and
//! the operator's words) and `swarm_events` (the swarm's log).

use super::plan::{Plan, PlannedTask};
use crate::bus::store::{self as bstore, normalize_workspace};
use crate::bus::{BusTask, NewTaskSpec, TaskIsolation, TaskStatus};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Who created a swarm's tasks on the board: `swarm:<id>`.
pub fn creator(swarm_id: &str) -> String {
    format!("swarm:{swarm_id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SwarmStatus {
    Planning,
    Review,
    Running,
    Paused,
    Integrating,
    Finished,
}

impl SwarmStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SwarmStatus::Planning => "planning",
            SwarmStatus::Review => "review",
            SwarmStatus::Running => "running",
            SwarmStatus::Paused => "paused",
            SwarmStatus::Integrating => "integrating",
            SwarmStatus::Finished => "finished",
        }
    }
    pub fn parse(s: &str) -> Option<SwarmStatus> {
        Some(match s {
            "planning" => SwarmStatus::Planning,
            "review" => SwarmStatus::Review,
            "running" => SwarmStatus::Running,
            "paused" => SwarmStatus::Paused,
            "integrating" => SwarmStatus::Integrating,
            "finished" => SwarmStatus::Finished,
            _ => return None,
        })
    }
    /// Whether the plan is approved: work may be claimed.
    pub fn approved(self) -> bool {
        matches!(
            self,
            SwarmStatus::Running
                | SwarmStatus::Paused
                | SwarmStatus::Integrating
                | SwarmStatus::Finished
        )
    }
}

/// How a plan gets approved (`swarm_settings.gate_mode`, v24): `chat`, the
/// coordinator relays the operator's yes from its own chat
/// (`agent_mux_swarm_launch` with the operator's words); `off`, a plan
/// without errors runs at once.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum GateMode {
    #[default]
    Chat,
    Off,
}

impl GateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GateMode::Chat => "chat",
            GateMode::Off => "off",
        }
    }
    pub fn parse(s: &str) -> Option<GateMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "chat" => Some(GateMode::Chat),
            "off" | "none" => Some(GateMode::Off),
            _ => None,
        }
    }
}

/// Who relayed an operator-level decision (`swarm_decisions.by`): the
/// coordinator from its chat, the Swarm screen, or the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DecidedBy {
    Chat,
    Tui,
    Cli,
}

impl DecidedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            DecidedBy::Chat => "chat",
            DecidedBy::Tui => "tui",
            DecidedBy::Cli => "cli",
        }
    }
    pub fn parse(s: &str) -> Option<DecidedBy> {
        match s.trim() {
            "chat" => Some(DecidedBy::Chat),
            "tui" => Some(DecidedBy::Tui),
            "cli" => Some(DecidedBy::Cli),
            _ => None,
        }
    }
}

/// Who asked for a task's worker (`swarm_dispatch.launched_by`): the
/// coordinator through `agent_mux_swarm_launch`, the dispatcher once the
/// grace period after the offer ran out, or the operator (screen, CLI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum LaunchedBy {
    Coordinator,
    Dispatcher,
    Operator,
}

impl LaunchedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchedBy::Coordinator => "coordinator",
            LaunchedBy::Dispatcher => "dispatcher",
            LaunchedBy::Operator => "operator",
        }
    }
    pub fn parse(s: &str) -> Option<LaunchedBy> {
        match s.trim() {
            "coordinator" => Some(LaunchedBy::Coordinator),
            "dispatcher" => Some(LaunchedBy::Dispatcher),
            "operator" => Some(LaunchedBy::Operator),
            _ => None,
        }
    }
}

impl From<DecidedBy> for LaunchedBy {
    fn from(by: DecidedBy) -> Self {
        match by {
            DecidedBy::Chat => LaunchedBy::Coordinator,
            DecidedBy::Tui | DecidedBy::Cli => LaunchedBy::Operator,
        }
    }
}

/// Who decides, for [`apply`] and [`launch`]: the relay, the operator's
/// words when the coordinator relays them, and the relaying launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decider<'a> {
    pub by: DecidedBy,
    pub said: Option<&'a str>,
    pub launch: Option<&'a str>,
}

impl<'a> Decider<'a> {
    pub const TUI: Decider<'static> = Decider {
        by: DecidedBy::Tui,
        said: None,
        launch: None,
    };
    pub const CLI: Decider<'static> = Decider {
        by: DecidedBy::Cli,
        said: None,
        launch: None,
    };
    /// The coordinator relays what the operator said in its chat.
    pub fn chat(said: Option<&'a str>, launch: Option<&'a str>) -> Decider<'a> {
        Decider {
            by: DecidedBy::Chat,
            said: said.map(str::trim).filter(|s| !s.is_empty()),
            launch,
        }
    }
    /// The actor name the bus records for this decider.
    pub fn actor(&self) -> &'a str {
        match (self.by, self.launch) {
            (DecidedBy::Chat, Some(l)) => l,
            (DecidedBy::Chat, None) => "coordinator",
            (DecidedBy::Tui, _) => "user",
            (DecidedBy::Cli, _) => "cli",
        }
    }
}

/// One swarm's run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmRun {
    pub id: String,
    pub workspace: String,
    pub goal: String,
    pub status: SwarmStatus,
    pub planner_harness: String,
    pub planner_launch: Option<String>,
    pub plan_summary: Option<String>,
    pub plan_error: Option<String>,
    pub max_workers: usize,
    pub isolation: TaskIsolation,
    /// Whether a plan waits for the operator (`gate_mode != off`).
    pub gate: bool,
    pub check_cmd: Option<String>,
    pub check_result: Option<String>,
    /// The TUI (`App::app_run_id`) that dispatches this swarm.
    pub owner_run: Option<String>,
    pub started_ns: i64,
    pub updated_ns: i64,
    pub ended_ns: Option<i64>,
    /// How the plan is approved (v24).
    pub gate_mode: GateMode,
    /// A headless swarm: the goal came with the start, the coordinator plans
    /// without asking (v24).
    pub headless: bool,
}

/// What a new swarm starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSwarm {
    pub workspace: String,
    pub goal: String,
    pub planner_harness: String,
    pub max_workers: usize,
    pub isolation: TaskIsolation,
    /// `false` is gate mode `off`; `true` is `chat`.
    pub gate: bool,
    pub check_cmd: Option<String>,
    pub owner_run: Option<String>,
    /// The harnesses the workers may run on, the first one for tasks that
    /// name none (empty: the coordinator's).
    pub worker_pool: Vec<String>,
    /// The goal is given and the coordinator plans without a conversation.
    pub headless: bool,
    /// How finished tasks are reviewed (v25).
    pub review: super::review::ReviewSettings,
}

/// The plan's side of one board task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SwarmTaskExt {
    pub task_id: String,
    pub plan_key: String,
    pub position: i64,
    pub depends_on: Vec<String>,
    pub acceptance: Option<String>,
    pub proposed: bool,
    pub attempts: i64,
    pub nudged_ns: Option<i64>,
    /// When the engine offered the ready task to the coordinator (v24).
    pub offered_ns: Option<i64>,
    /// Who asked for its worker; set before the spawn (v24).
    pub launched_by: Option<LaunchedBy>,
    /// The plan dismissed this task's review (v25).
    #[serde(default)]
    pub review_skip: bool,
    /// The board task that reviews this one (v25).
    #[serde(default)]
    pub review_task: Option<String>,
    /// This task is the review of that one (v25).
    #[serde(default)]
    pub review_of: Option<String>,
}

/// A board task with its plan extension and, for a finished worktree
/// task, what the operator did with its branch (`merged`, `discarded`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardTask {
    pub task: BusTask,
    pub ext: SwarmTaskExt,
    pub review: Option<String>,
}

const SWARM_COLS: &str = "s.id, s.workspace, s.goal, s.status, s.planner_harness, s.planner_launch, \
    s.plan_summary, s.plan_error, s.max_workers, s.isolation, s.gate, s.check_cmd, s.check_result, \
    s.owner_run, s.started_ns, s.updated_ns, s.ended_ns, \
    COALESCE(g.gate_mode, CASE WHEN s.gate = 0 THEN 'off' ELSE 'chat' END), COALESCE(g.headless, 0)";
/// `swarms` with its v24 settings; the columns of [`SWARM_COLS`].
const SWARM_FROM: &str = "swarms s LEFT JOIN swarm_settings g ON g.swarm_id = s.id";

fn row_to_swarm(r: &rusqlite::Row<'_>) -> rusqlite::Result<SwarmRun> {
    Ok(SwarmRun {
        id: r.get(0)?,
        workspace: r.get(1)?,
        goal: r.get(2)?,
        status: SwarmStatus::parse(&r.get::<_, String>(3)?).unwrap_or(SwarmStatus::Finished),
        planner_harness: r.get(4)?,
        planner_launch: r.get(5)?,
        plan_summary: r.get(6)?,
        plan_error: r.get(7)?,
        max_workers: r.get::<_, i64>(8)?.max(1) as usize,
        isolation: TaskIsolation::parse(&r.get::<_, String>(9)?).unwrap_or_default(),
        gate: r.get::<_, i64>(10)? != 0,
        check_cmd: r.get(11)?,
        check_result: r.get(12)?,
        owner_run: r.get(13)?,
        started_ns: r.get(14)?,
        updated_ns: r.get(15)?,
        ended_ns: r.get(16)?,
        gate_mode: GateMode::parse(&r.get::<_, String>(17)?).unwrap_or_default(),
        headless: r.get::<_, i64>(18)? != 0,
    })
}

fn poke(db: Option<&Path>) {
    if let Some(p) = db {
        crate::tracing::store::poke_wake(p);
    }
}

/// Starts a swarm in `planning`. A workspace may run several (schema v23).
pub fn create_swarm(
    conn: &Connection,
    db: Option<&Path>,
    new: &NewSwarm,
    now: i64,
) -> Result<SwarmRun, String> {
    let workspace = normalize_workspace(Path::new(&new.workspace));
    let id = format!("swarm-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    conn.execute(
        "INSERT INTO swarms (id, workspace, goal, status, planner_harness, max_workers,
                             isolation, gate, check_cmd, owner_run, started_ns, updated_ns)
         VALUES (?1, ?2, ?3, 'planning', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
        params![
            id,
            workspace,
            new.goal.trim(),
            new.planner_harness,
            new.max_workers.max(1) as i64,
            new.isolation.as_str(),
            new.gate,
            new.check_cmd
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty()),
            new.owner_run,
            now,
        ],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO swarm_coordinators (swarm_id, worker_harness, updated_ns) VALUES (?1, ?2, ?3)
         ON CONFLICT (swarm_id) DO UPDATE SET worker_harness = excluded.worker_harness",
        params![id, encode_pool(&new.worker_pool), now],
    )
    .map_err(|e| e.to_string())?;
    let gate_mode = if new.gate {
        GateMode::Chat
    } else {
        GateMode::Off
    };
    conn.execute(
        "INSERT INTO swarm_settings (swarm_id, gate_mode, headless, updated_ns) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (swarm_id) DO UPDATE SET gate_mode = excluded.gate_mode,
             headless = excluded.headless, updated_ns = excluded.updated_ns",
        params![id, gate_mode.as_str(), new.headless, now],
    )
    .map_err(|e| e.to_string())?;
    super::review::set_settings(conn, &id, &new.review, now).map_err(|e| e.to_string())?;
    let _ = event(
        conn,
        &id,
        None,
        "started",
        Some("operator"),
        Some(serde_json::json!({
            "goal": new.goal.trim(),
            "coordinator": new.planner_harness,
            "gate": gate_mode.as_str(),
            "headless": new.headless,
            "review": new.review.as_choice(),
        })),
        now,
    );
    poke(db);
    get_swarm(conn, &id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the swarm disappeared".into())
}

/// `swarm_coordinators.worker_harness`: the pool, comma separated.
fn encode_pool(pool: &[String]) -> Option<String> {
    let mut seen: Vec<&str> = Vec::new();
    for h in pool.iter().map(|h| h.trim()).filter(|h| !h.is_empty()) {
        if !seen.contains(&h) {
            seen.push(h);
        }
    }
    (!seen.is_empty()).then(|| seen.join(","))
}

/// The harnesses the swarm's workers may run on, the default first: the
/// pool chosen at the start, else the coordinator's harness alone.
pub fn worker_pool(conn: &Connection, swarm: &SwarmRun) -> Vec<String> {
    let pool: Vec<String> = coordinator(conn, &swarm.id)
        .ok()
        .flatten()
        .and_then(|c| c.worker_harness)
        .map(|s| {
            s.split(',')
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if pool.is_empty() {
        vec![swarm.planner_harness.clone()]
    } else {
        pool
    }
}

/// The harness of the swarm's tasks that name none: the pool's first.
pub fn worker_harness(conn: &Connection, swarm: &SwarmRun) -> String {
    worker_pool(conn, swarm).remove(0)
}

pub fn get_swarm(conn: &Connection, id: &str) -> rusqlite::Result<Option<SwarmRun>> {
    conn.query_row(
        &format!("SELECT {SWARM_COLS} FROM {SWARM_FROM} WHERE s.id = ?1"),
        params![id],
        row_to_swarm,
    )
    .optional()
}

/// `workspace`'s newest live swarm (normalized path), if any. A workspace
/// may run several: name one by id where it matters ([`live_swarms_in`],
/// [`swarm_of_task`]).
pub fn live_swarm(conn: &Connection, workspace: &str) -> rusqlite::Result<Option<SwarmRun>> {
    conn.query_row(
        &format!(
            "SELECT {SWARM_COLS} FROM {SWARM_FROM} WHERE s.workspace = ?1 AND s.ended_ns IS NULL
             ORDER BY s.started_ns DESC LIMIT 1"
        ),
        params![workspace],
        row_to_swarm,
    )
    .optional()
}

/// `workspace`'s live swarms, oldest first.
pub fn live_swarms_in(conn: &Connection, workspace: &str) -> rusqlite::Result<Vec<SwarmRun>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SWARM_COLS} FROM {SWARM_FROM} WHERE s.workspace = ?1 AND s.ended_ns IS NULL
         ORDER BY s.started_ns ASC"
    ))?;
    let rows = stmt.query_map(params![workspace], row_to_swarm)?;
    rows.collect()
}

/// The swarm `task_id` is on the board of, live or not.
pub fn swarm_of_task(conn: &Connection, task_id: &str) -> rusqlite::Result<Option<SwarmRun>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT swarm_id FROM swarm_tasks WHERE task_id = ?1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()?;
    match id {
        Some(id) => get_swarm(conn, &id),
        None => Ok(None),
    }
}

/// The live swarm a command names: `id` (an id or its short form), else
/// `workspace`'s only live one. Several live and none named is an error
/// that lists them.
pub fn resolve_swarm(
    conn: &Connection,
    workspace: &str,
    id: Option<&str>,
) -> Result<SwarmRun, String> {
    if let Some(id) = id.map(str::trim).filter(|s| !s.is_empty()) {
        let full = if id.starts_with("swarm-") {
            id.to_string()
        } else {
            format!("swarm-{id}")
        };
        return get_swarm(conn, &full)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no swarm `{id}`"));
    }
    let mut live = live_swarms_in(conn, workspace).map_err(|e| e.to_string())?;
    match live.len() {
        0 => Err(format!("no swarm runs in {workspace}")),
        1 => Ok(live.remove(0)),
        _ => Err(format!(
            "{} swarms run in {workspace}: name one with --swarm ({})",
            live.len(),
            live.iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Swarms that ended at or after `since_ns`, newest first.
pub fn ended_swarms_since(conn: &Connection, since_ns: i64) -> rusqlite::Result<Vec<SwarmRun>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SWARM_COLS} FROM {SWARM_FROM} WHERE s.ended_ns IS NOT NULL AND s.ended_ns >= ?1
         ORDER BY s.ended_ns DESC LIMIT 20"
    ))?;
    let rows = stmt.query_map(params![since_ns], row_to_swarm)?;
    rows.collect()
}

/// Every live swarm, oldest first.
pub fn live_swarms(conn: &Connection) -> rusqlite::Result<Vec<SwarmRun>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SWARM_COLS} FROM {SWARM_FROM} WHERE s.ended_ns IS NULL ORDER BY s.started_ns ASC"
    ))?;
    let rows = stmt.query_map([], row_to_swarm)?;
    rows.collect()
}

pub fn set_status(
    conn: &Connection,
    db: Option<&Path>,
    id: &str,
    status: SwarmStatus,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE swarms SET status = ?1, updated_ns = ?2 WHERE id = ?3",
        params![status.as_str(), now, id],
    )?;
    poke(db);
    Ok(())
}

/// A planner run started: `planning`, its launch, no error yet.
pub fn set_planner(
    conn: &Connection,
    db: Option<&Path>,
    id: &str,
    launch: Option<&str>,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE swarms SET status = 'planning', planner_launch = ?1, plan_error = NULL,
                           updated_ns = ?2 WHERE id = ?3",
        params![launch, now, id],
    )?;
    poke(db);
    Ok(())
}

/// A planner run ended: its summary or its error, and the status it leads to.
pub fn set_plan_result(
    conn: &Connection,
    db: Option<&Path>,
    id: &str,
    summary: Option<&str>,
    error: Option<&str>,
    status: SwarmStatus,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE swarms SET status = ?1, plan_summary = COALESCE(?2, plan_summary),
                           plan_error = ?3, updated_ns = ?4 WHERE id = ?5",
        params![status.as_str(), summary, error, now, id],
    )?;
    poke(db);
    Ok(())
}

pub fn set_owner(conn: &Connection, id: &str, owner_run: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE swarms SET owner_run = ?1 WHERE id = ?2",
        params![owner_run, id],
    )?;
    Ok(())
}

pub fn set_check_result(
    conn: &Connection,
    db: Option<&Path>,
    id: &str,
    result: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE swarms SET check_result = ?1, updated_ns = ?2 WHERE id = ?3",
        params![result, now, id],
    )?;
    poke(db);
    Ok(())
}

fn row_to_ext(r: &rusqlite::Row<'_>) -> rusqlite::Result<SwarmTaskExt> {
    let deps: String = r.get(3)?;
    Ok(SwarmTaskExt {
        task_id: r.get(0)?,
        plan_key: r.get(1)?,
        position: r.get(2)?,
        depends_on: serde_json::from_str(&deps).unwrap_or_default(),
        acceptance: r.get(4)?,
        proposed: r.get::<_, i64>(5)? != 0,
        attempts: r.get(6)?,
        nudged_ns: r.get(7)?,
        offered_ns: r.get(8)?,
        launched_by: r
            .get::<_, Option<String>>(9)?
            .as_deref()
            .and_then(LaunchedBy::parse),
        review_skip: r.get::<_, Option<i64>>(11)?.unwrap_or(0) != 0,
        review_task: r.get(12)?,
        review_of: r.get(13)?,
    })
}

/// The swarm's board in plan order, each task with its extension and review.
pub fn board(conn: &Connection, swarm_id: &str) -> rusqlite::Result<Vec<BoardTask>> {
    let mut stmt = conn.prepare(
        "SELECT st.task_id, st.plan_key, st.position, st.depends_on_json, st.acceptance,
                st.proposed, st.attempts, st.nudged_ns, d.offered_ns, d.launched_by, r.verdict,
                rv.skip, rv.review_task, ro.task_id
         FROM swarm_tasks st LEFT JOIN swarm_reviews r ON r.task_id = st.task_id
                             LEFT JOIN swarm_dispatch d ON d.task_id = st.task_id
                             LEFT JOIN swarm_task_reviews rv ON rv.task_id = st.task_id
                             LEFT JOIN swarm_task_reviews ro ON ro.review_task = st.task_id
         WHERE st.swarm_id = ?1 ORDER BY st.position ASC, st.task_id ASC",
    )?;
    let rows: Vec<(SwarmTaskExt, Option<String>)> = stmt
        .query_map(params![swarm_id], |r| Ok((row_to_ext(r)?, r.get(10)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (ext, review) in rows {
        if let Some(task) = bstore::get_task(conn, &ext.task_id)? {
            out.push(BoardTask { task, ext, review });
        }
    }
    Ok(out)
}

/// Whether a task has started at least once (a draft has not).
fn started(t: &BoardTask) -> bool {
    t.ext.attempts > 0 || t.task.assigned_to.is_some() || t.task.status != TaskStatus::Open
}

/// Puts `plan` on the board in place of the tasks that never started (a
/// re-plan keeps everything that ran). Defaults: the swarm's isolation and
/// the planner's harness. Answers the board.
pub fn replace_draft(
    conn: &Connection,
    db: Option<&Path>,
    swarm: &SwarmRun,
    plan: &Plan,
    now: i64,
) -> Result<Vec<BoardTask>, String> {
    let before = board(conn, &swarm.id).map_err(|e| e.to_string())?;
    let kept: Vec<&BoardTask> = before.iter().filter(|t| started(t)).collect();
    for t in before.iter().filter(|t| !started(t)) {
        delete_task(conn, &t.task.id).map_err(|e| e.to_string())?;
    }
    let mut position = kept.iter().map(|t| t.ext.position).max().unwrap_or(-1) + 1;
    for pt in &plan.tasks {
        if kept.iter().any(|k| k.ext.plan_key == pt.key) {
            continue; // already ran under this key
        }
        insert_task(conn, swarm, pt, position, false, now)?;
        position += 1;
    }
    poke(db);
    board(conn, &swarm.id).map_err(|e| e.to_string())
}

/// Adds one task to the board (the operator's `n`, a worker's proposal).
pub fn add_task(
    conn: &Connection,
    db: Option<&Path>,
    swarm: &SwarmRun,
    pt: &PlannedTask,
    proposed: bool,
    now: i64,
) -> Result<String, String> {
    let position: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM swarm_tasks WHERE swarm_id = ?1",
            params![swarm.id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let id = insert_task(conn, swarm, pt, position, proposed, now)?;
    poke(db);
    Ok(id)
}

fn insert_task(
    conn: &Connection,
    swarm: &SwarmRun,
    pt: &PlannedTask,
    position: i64,
    proposed: bool,
    now: i64,
) -> Result<String, String> {
    let spec = NewTaskSpec {
        title: pt.title.clone(),
        instructions: pt.instructions.clone(),
        files: pt.files.clone(),
        parent_task_id: None,
        assigned_to: None,
        target_harness: Some(
            pt.harness
                .clone()
                .unwrap_or_else(|| worker_harness(conn, swarm)),
        ),
        target_agent: pt.agent.clone(),
        isolation: pt.isolation_or(swarm.isolation),
    };
    let created = bstore::split_tasks(
        conn,
        None,
        &swarm.workspace,
        &creator(&swarm.id),
        &[spec],
        now,
    )?;
    let task = created
        .into_iter()
        .next()
        .ok_or_else(|| "no task was created".to_string())?;
    conn.execute(
        "INSERT INTO swarm_tasks (task_id, swarm_id, plan_key, position, depends_on_json,
                                  acceptance, proposed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            task.id,
            swarm.id,
            pt.key,
            position,
            serde_json::to_string(&pt.depends_on).unwrap_or_else(|_| "[]".into()),
            pt.acceptance,
            proposed,
        ],
    )
    .map_err(|e| {
        let _ = delete_task(conn, &task.id);
        if e.to_string().contains("UNIQUE") {
            format!("the key `{}` is already on the board", pt.key)
        } else {
            e.to_string()
        }
    })?;
    if pt.review == Some(false) {
        super::review::set_skip(conn, &task.id, true, now).map_err(|e| e.to_string())?;
    }
    Ok(task.id)
}

/// Puts the review of finished task `task_id` on the board (v25): one more
/// task, on the review harness (`review::harness_for`) as the review agent
/// (none on agy, which would run it without a shell), shared: it changes no
/// file and runs in the reviewed task's folder. It is offered and started
/// like any task. Answers the review's key.
pub fn add_review(
    conn: &Connection,
    db: Option<&Path>,
    swarm: &SwarmRun,
    task_id: &str,
    now: i64,
) -> Result<String, String> {
    let board = board(conn, &swarm.id).map_err(|e| e.to_string())?;
    let t = board
        .iter()
        .find(|t| t.task.id == task_id)
        .ok_or_else(|| format!("no task {task_id} on the board"))?;
    if t.ext.review_of.is_some() {
        return Err(format!("`{}` is a review itself", t.ext.plan_key));
    }
    let settings = super::review::settings(conn, &swarm.id);
    let harness = super::review::harness_for(
        &settings,
        t.task.target_harness.as_deref(),
        &worker_pool(conn, swarm),
    );
    let key = super::review::review_key(&board, &t.ext.plan_key);
    let pt = PlannedTask {
        key: key.clone(),
        title: format!("Review: {}", t.task.title),
        instructions: super::review::instructions(t),
        acceptance: Some("the result's first line is APPROVE or CHANGES".into()),
        files: Vec::new(),
        depends_on: Vec::new(),
        agent: (harness != "agy").then(|| settings.agent.clone()),
        harness: Some(harness.clone()),
        isolation: Some(TaskIsolation::Shared.as_str().to_string()),
        review: None,
    };
    let id = add_task(conn, None, swarm, &pt, false, now)?;
    super::review::link(conn, &t.task.id, &id, now).map_err(|e| e.to_string())?;
    let _ = event(
        conn,
        &swarm.id,
        Some(&t.task.id),
        "review.added",
        Some("engine"),
        Some(serde_json::json!({
            "key": t.ext.plan_key,
            "review": key,
            "harness": harness,
        })),
        now,
    );
    poke(db);
    Ok(key)
}

/// Why merging `t` goes against its review, if it does: the swarm reviews
/// and `t`'s review did not approve it (yet).
pub fn unreviewed_warning(
    conn: &Connection,
    swarm: &SwarmRun,
    t: &BoardTask,
    board: &[BoardTask],
) -> Option<String> {
    let enabled = super::review::settings(conn, &swarm.id).enabled;
    super::review::merge_warning(super::review::state(t, board, enabled)).map(str::to_string)
}

/// A swarm's coordinator agent (schema v22).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Coordinator {
    pub swarm_id: String,
    /// Its live session, if one was started.
    pub launch: Option<String>,
    /// The workers' default harness (`None`: the coordinator's).
    pub worker_harness: Option<String>,
    /// Events up to here were told to it.
    pub events_seen_ns: i64,
    pub last_wake_ns: Option<i64>,
}

pub fn coordinator(conn: &Connection, swarm_id: &str) -> rusqlite::Result<Option<Coordinator>> {
    conn.query_row(
        "SELECT swarm_id, launch, worker_harness, events_seen_ns, last_wake_ns
         FROM swarm_coordinators WHERE swarm_id = ?1",
        params![swarm_id],
        |r| {
            Ok(Coordinator {
                swarm_id: r.get(0)?,
                launch: r.get(1)?,
                worker_harness: r.get(2)?,
                events_seen_ns: r.get(3)?,
                last_wake_ns: r.get(4)?,
            })
        },
    )
    .optional()
}

/// Records the coordinator's session (and the swarm's `planner_launch`, so
/// the tree's `★` row shows it). A new coordinator starts with no events
/// seen but those before `now`.
pub fn set_coordinator_launch(
    conn: &Connection,
    db: Option<&Path>,
    swarm_id: &str,
    launch: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO swarm_coordinators (swarm_id, launch, events_seen_ns, updated_ns)
         VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT (swarm_id) DO UPDATE SET launch = excluded.launch,
             events_seen_ns = excluded.events_seen_ns, updated_ns = excluded.updated_ns",
        params![swarm_id, launch, now],
    )?;
    conn.execute(
        "UPDATE swarms SET planner_launch = ?1, updated_ns = ?2 WHERE id = ?3",
        params![launch, now, swarm_id],
    )?;
    poke(db);
    Ok(())
}

/// The events up to `seen_ns` were told to the coordinator, at `wake_ns`.
pub fn mark_events_told(
    conn: &Connection,
    swarm_id: &str,
    seen_ns: i64,
    wake_ns: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE swarm_coordinators SET events_seen_ns = ?1, last_wake_ns = ?2, updated_ns = ?2
         WHERE swarm_id = ?3",
        params![seen_ns, wake_ns, swarm_id],
    )?;
    Ok(())
}

/// The live swarm whose coordinator is `launch`.
pub fn swarm_of_coordinator(conn: &Connection, launch: &str) -> rusqlite::Result<Option<SwarmRun>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT c.swarm_id FROM swarm_coordinators c JOIN swarms s ON s.id = c.swarm_id
             WHERE c.launch = ?1 AND s.ended_ns IS NULL",
            params![launch],
            |r| r.get(0),
        )
        .optional()?;
    match id {
        Some(id) => get_swarm(conn, &id),
        None => Ok(None),
    }
}

/// The swarm a launch belongs to, live or ended, from its launch row's
/// `swarm_id` (v24): a coordinator, a worker, even one whose task ended.
pub fn swarm_of_launch(conn: &Connection, launch: &str) -> rusqlite::Result<Option<SwarmRun>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT json_extract(metadata, '$.swarm_id') FROM launches WHERE id = ?1",
            params![launch],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    match id {
        Some(id) => get_swarm(conn, &id),
        None => Ok(None),
    }
}

/// Where a message `to = "coordinator"` from `sender` goes: the live
/// coordinator of the swarm `sender` works for.
pub fn coordinator_address(conn: &Connection, sender: &str) -> Option<String> {
    let (swarm, _) = swarm_of_worker(conn, sender).ok().flatten()?;
    coordinator(conn, &swarm.id).ok().flatten()?.launch
}

/// What submitting a plan did.
#[derive(Debug, Clone, PartialEq)]
pub enum Submitted {
    /// On the board; the swarm now waits for the operator (`review`) or,
    /// with the gate off, runs.
    Accepted {
        tasks: usize,
        status: SwarmStatus,
        warnings: Vec<(String, String)>,
        /// The plan rendered for the chat ([`plan_table`]).
        table: String,
    },
    /// Nothing changed: these errors must be fixed first.
    Rejected(super::plan::Problems),
}

/// The coordinator's `agent_mux_swarm_plan` (and `agent-mux swarm
/// submit-plan`): the plan is checked, and only a plan without errors goes
/// on the board, in place of the tasks that never started. Allowed while
/// the swarm is planning or in review.
pub fn submit_plan(
    conn: &Connection,
    db: Option<&Path>,
    swarm: &SwarmRun,
    plan: &Plan,
    env: &super::plan::CheckEnv,
    now: i64,
) -> Result<Submitted, String> {
    if !matches!(swarm.status, SwarmStatus::Planning | SwarmStatus::Review) {
        return Err(format!(
            "the plan of {} is already approved ({}): propose new tasks with agent_mux_task_split",
            swarm.id,
            swarm.status.as_str()
        ));
    }
    // Check the whole plan as it would stand: the tasks that ran stay.
    let before = board(conn, &swarm.id).map_err(|e| e.to_string())?;
    let mut merged = plan.clone();
    for t in before.iter().filter(|t| started(t)) {
        if !merged.tasks.iter().any(|p| p.key == t.ext.plan_key) {
            merged
                .tasks
                .push(board_plan(swarm, std::slice::from_ref(t)).tasks.remove(0));
        }
    }
    let problems = super::plan::check(&merged, &env.with_pool(worker_pool(conn, swarm)));
    if !problems.is_ok() {
        return Ok(Submitted::Rejected(problems));
    }
    let revised = swarm.plan_summary.is_some() || !before.is_empty();
    if let Some(r) = &plan.review {
        let mut s = super::review::settings(conn, &swarm.id);
        if let Some(on) = r.enabled {
            s.enabled = on;
        }
        if let Some(h) = r
            .harness
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
        {
            s.harness = (h != "auto").then(|| h.to_string());
        }
        super::review::set_settings(conn, &swarm.id, &s, now).map_err(|e| e.to_string())?;
    }
    let board = replace_draft(conn, db, swarm, plan, now)?;
    let status = if swarm.gate_mode == GateMode::Off {
        SwarmStatus::Running
    } else {
        SwarmStatus::Review
    };
    set_plan_result(conn, db, &swarm.id, Some(&plan.summary), None, status, now)
        .map_err(|e| e.to_string())?;
    let _ = event(
        conn,
        &swarm.id,
        None,
        if revised {
            "plan.revised"
        } else {
            "plan.proposed"
        },
        coordinator(conn, &swarm.id)
            .ok()
            .flatten()
            .and_then(|c| c.launch)
            .as_deref(),
        Some(serde_json::json!({
            "summary": plan.summary,
            "tasks": board.iter().map(|t| t.ext.plan_key.clone()).collect::<Vec<_>>(),
            "warnings": problems.warnings.len(),
        })),
        now,
    );
    Ok(Submitted::Accepted {
        tasks: board.len(),
        status,
        warnings: problems.warnings,
        table: plan_table(swarm, &board, &super::review::settings(conn, &swarm.id)),
    })
}

/// The plan as a Markdown table, for the coordinator to show the operator
/// in its chat: what the Board tab draws.
pub fn plan_table(
    swarm: &SwarmRun,
    board: &[BoardTask],
    review: &super::review::ReviewSettings,
) -> String {
    let mut out = String::from(
        "| # | key | title | harness · agent | isolation | depends on | done when | review |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for (i, t) in board
        .iter()
        .filter(|t| t.ext.review_of.is_none())
        .enumerate()
    {
        let who = match (&t.task.target_harness, &t.task.target_agent) {
            (Some(h), Some(a)) => format!("{h} · {a}"),
            (Some(h), None) => h.clone(),
            (None, Some(a)) => format!("· {a}"),
            (None, None) => "—".into(),
        };
        let reviewed = if !review.enabled || t.ext.review_skip {
            "—".to_string()
        } else {
            match &review.harness {
                Some(h) => h.clone(),
                None => "another harness".to_string(),
            }
        };
        out.push_str(&format!(
            "| {} | `{}` | {} | {} | {} | {} | {} | {} |\n",
            i + 1,
            t.ext.plan_key,
            t.task.title.replace('|', "/"),
            who,
            t.task.isolation.as_str(),
            if t.ext.depends_on.is_empty() {
                "—".to_string()
            } else {
                t.ext.depends_on.join(", ")
            },
            t.ext
                .acceptance
                .as_deref()
                .unwrap_or("—")
                .replace('|', "/")
                .replace('\n', " "),
            reviewed,
        ));
    }
    out.push_str(&format!("\nReview: {}", review.describe()));
    out.push_str(&format!(
        "\nUp to {} worker(s) at once · default isolation {}{}",
        swarm.max_workers,
        swarm.isolation.as_str(),
        swarm
            .check_cmd
            .as_deref()
            .map(|c| format!(" · check when done: `{c}`"))
            .unwrap_or_default()
    ));
    out
}

/// A submission's answer, in the coordinator's words.
pub fn submitted_note(swarm: &SwarmRun, s: &Submitted) -> String {
    match s {
        Submitted::Accepted {
            tasks,
            status,
            warnings,
            table,
        } => {
            let mut out = match status {
                SwarmStatus::Running => format!(
                    "Plan accepted: {tasks} task(s) on the board of {}; workers start now.",
                    swarm.id
                ),
                _ => format!(
                    "Plan accepted: {tasks} task(s) on the board of {}. Show this table to the \
                     operator and ask whether to start; when they say yes, call \
                     agent_mux_swarm_launch with their words in operator_said.\n\n{table}",
                    swarm.id
                ),
            };
            for (k, m) in warnings {
                out.push_str(&format!(
                    "\nwarning{}: {m}",
                    if k.is_empty() {
                        String::new()
                    } else {
                        format!(" ({k})")
                    }
                ));
            }
            out
        }
        Submitted::Rejected(p) => {
            let mut out = format!(
                "Plan rejected: {} error(s). Fix them and call agent_mux_swarm_plan again.",
                p.errors.len()
            );
            for (k, m) in &p.errors {
                out.push_str(&format!(
                    "\nerror{}: {m}",
                    if k.is_empty() {
                        String::new()
                    } else {
                        format!(" ({k})")
                    }
                ));
            }
            out
        }
    }
}

/// The live swarm `launch` works for and the task it holds, if it is one
/// of a swarm's workers (its task not yet terminal).
pub fn swarm_of_worker(
    conn: &Connection,
    launch: &str,
) -> rusqlite::Result<Option<(SwarmRun, BoardTask)>> {
    let found: Option<(String, String)> = conn
        .query_row(
            "SELECT st.swarm_id, st.task_id FROM swarm_tasks st
             JOIN agent_tasks t ON t.id = st.task_id
             JOIN swarms s ON s.id = st.swarm_id
             WHERE t.assigned_to = ?1 AND s.ended_ns IS NULL
               AND t.status IN ('open','claimed','running')
             ORDER BY t.updated_ns DESC LIMIT 1",
            params![launch],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((swarm_id, task_id)) = found else {
        return Ok(None);
    };
    let Some(swarm) = get_swarm(conn, &swarm_id)? else {
        return Ok(None);
    };
    let task = board(conn, &swarm_id)?
        .into_iter()
        .find(|t| t.task.id == task_id);
    Ok(task.map(|t| (swarm, t)))
}

/// A key for a task added outside the plan (a worker's proposal, the
/// coordinator's addition): its title as a slug, unique on the board.
pub fn key_for_title(conn: &Connection, swarm_id: &str, title: &str) -> String {
    proposal_key(conn, swarm_id, title)
}

fn proposal_key(conn: &Connection, swarm_id: &str, title: &str) -> String {
    let mut slug: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|w| !w.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        slug = "task".into();
    }
    let taken = |k: &str| -> bool {
        conn.query_row(
            "SELECT 1 FROM swarm_tasks WHERE swarm_id = ?1 AND plan_key = ?2",
            params![swarm_id, k],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    };
    if !taken(&slug) {
        return slug;
    }
    (2..)
        .map(|n| format!("{slug}-{n}"))
        .find(|k| !taken(k))
        .expect("an unused key")
}

/// What a swarm worker's `task_split` or `delegate` becomes (§5.6): new
/// tasks on its swarm's board that wait for the operator (`proposed`), or,
/// with the plan gate off, tasks the dispatcher schedules at once. Nothing
/// is spawned by the worker. Answers each new task's key and id.
pub fn propose(
    conn: &Connection,
    db: Option<&Path>,
    swarm: &SwarmRun,
    specs: &[NewTaskSpec],
    now: i64,
) -> Result<Vec<(String, String)>, String> {
    if specs.is_empty() {
        return Err("at least one task spec is required".into());
    }
    let mut out = Vec::new();
    for spec in specs {
        if spec.title.trim().is_empty() {
            return Err("task title cannot be empty".into());
        }
        let key = proposal_key(conn, &swarm.id, &spec.title);
        let pt = PlannedTask {
            key: key.clone(),
            title: spec.title.trim().to_string(),
            instructions: spec.instructions.trim().to_string(),
            acceptance: None,
            files: spec.files.clone(),
            depends_on: Vec::new(),
            harness: spec.target_harness.clone(),
            agent: spec.target_agent.clone(),
            isolation: Some(spec.isolation.as_str().to_string()),
            review: None,
        };
        let id = add_task(conn, db, swarm, &pt, swarm.gate, now)?;
        out.push((key, id));
    }
    Ok(out)
}

/// A worker's answer for [`propose`]: what happened, in its words.
pub fn proposal_note(swarm: &SwarmRun, created: &[(String, String)]) -> String {
    let keys = created
        .iter()
        .map(|(k, _)| format!("`{k}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if swarm.gate {
        format!(
            "You are a worker of swarm {}: agent-mux runs its tasks, so nothing was spawned. \
             {keys} went on the board as a proposal for the operator to accept; carry on with your own task.",
            swarm.id
        )
    } else {
        format!(
            "You are a worker of swarm {}: agent-mux runs its tasks, so nothing was spawned. \
             {keys} went on the board and will start when a worker is free; carry on with your own task.",
            swarm.id
        )
    }
}

/// Clears the final check's outcome (a retried task makes it stale).
pub fn clear_check(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE swarms SET check_result = NULL WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// Edits a task's card: title, instructions and files on the board row,
/// dependencies and acceptance on its plan extension.
pub fn update_plan_task(
    conn: &Connection,
    db: Option<&Path>,
    task_id: &str,
    pt: &PlannedTask,
    now: i64,
) -> Result<(), String> {
    bstore::update_task_spec(
        conn,
        None,
        task_id,
        &pt.title,
        &pt.instructions,
        &pt.files,
        now,
    )?;
    // the harness, agent and isolation the card names (a harness of none
    // is the swarm's workers' harness); the operator's harness joins the
    // pool, so the plan check accepts the card as it now stands
    let swarm = swarm_of_task(conn, task_id).map_err(|e| e.to_string())?;
    let harness = match (&pt.harness, &swarm) {
        (Some(h), Some(s)) => {
            let mut pool = worker_pool(conn, s);
            if !pool.contains(h) {
                pool.push(h.clone());
                conn.execute(
                    "INSERT INTO swarm_coordinators (swarm_id, worker_harness, updated_ns)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT (swarm_id) DO UPDATE SET worker_harness = excluded.worker_harness,
                                                          updated_ns = excluded.updated_ns",
                    params![s.id, encode_pool(&pool), now],
                )
                .map_err(|e| e.to_string())?;
            }
            Some(h.clone())
        }
        (Some(h), None) => Some(h.clone()),
        (None, Some(s)) => Some(worker_harness(conn, s)),
        (None, None) => None,
    };
    let isolation = pt
        .isolation
        .as_deref()
        .and_then(TaskIsolation::parse)
        .unwrap_or_default();
    conn.execute(
        "UPDATE agent_tasks SET target_harness = COALESCE(?1, target_harness), target_agent = ?2,
                                isolation = ?3 WHERE id = ?4",
        params![harness, pt.agent, isolation.as_str(), task_id],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE swarm_tasks SET depends_on_json = ?1, acceptance = ?2 WHERE task_id = ?3",
        params![
            serde_json::to_string(&pt.depends_on).unwrap_or_else(|_| "[]".into()),
            pt.acceptance,
            task_id
        ],
    )
    .map_err(|e| e.to_string())?;
    poke(db);
    Ok(())
}

/// Removes a task that never started from the board.
pub fn delete_task(conn: &Connection, task_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM swarm_tasks WHERE task_id = ?1",
        params![task_id],
    )?;
    conn.execute(
        "DELETE FROM agent_leases WHERE task_id = ?1",
        params![task_id],
    )?;
    conn.execute("DELETE FROM agent_tasks WHERE id = ?1", params![task_id])?;
    Ok(())
}

/// A worker was spawned for the task: one more attempt, no nudge yet.
pub fn mark_spawned(conn: &Connection, task_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE swarm_tasks SET attempts = attempts + 1, nudged_ns = NULL WHERE task_id = ?1",
        params![task_id],
    )?;
    Ok(())
}

pub fn mark_nudged(conn: &Connection, task_id: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE swarm_tasks SET nudged_ns = ?1 WHERE task_id = ?2",
        params![now, task_id],
    )?;
    Ok(())
}

/// The engine offered a ready task to the coordinator (v24): from now the
/// grace period runs, after which the dispatcher starts it itself. A task
/// already offered keeps its first offer.
pub fn offer_task(conn: &Connection, task_id: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO swarm_dispatch (task_id, offered_ns, updated_ns) VALUES (?1, ?2, ?2)
         ON CONFLICT (task_id) DO UPDATE SET offered_ns = COALESCE(swarm_dispatch.offered_ns, excluded.offered_ns),
             updated_ns = excluded.updated_ns",
        params![task_id, now],
    )?;
    Ok(())
}

/// Someone asked for the task's worker: the engine spawns it at its next
/// pass (within the cap). Who asked is kept on the row.
pub fn request_launch(
    conn: &Connection,
    db: Option<&Path>,
    task_id: &str,
    by: LaunchedBy,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO swarm_dispatch (task_id, launched_by, requested_ns, updated_ns) VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT (task_id) DO UPDATE SET launched_by = excluded.launched_by,
             requested_ns = excluded.requested_ns, updated_ns = excluded.updated_ns",
        params![task_id, by.as_str(), now],
    )?;
    poke(db);
    Ok(())
}

/// Forgets a task's offer and request (a retry starts the cycle again).
pub fn reset_dispatch(conn: &Connection, task_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM swarm_dispatch WHERE task_id = ?1",
        params![task_id],
    )?;
    Ok(())
}

/// One line of the swarm's log (`swarm_events`, v24).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmEvent {
    pub id: i64,
    pub swarm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// `started`, `plan.proposed`, `plan.revised`, `decision.<verb>`,
    /// `task.offered`, `task.spawned`, `task.nudged`, `task.done`,
    /// `task.failed`, `task.lost`, `check.started`, `check.done`,
    /// `swarm.<status>`, `closed`.
    pub kind: String,
    /// A launch id, `operator`, `engine`, or none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
    pub created_ns: i64,
}

/// Writes one event to the swarm's log. Answers its id.
pub fn event(
    conn: &Connection,
    swarm_id: &str,
    task_id: Option<&str>,
    kind: &str,
    actor: Option<&str>,
    detail: Option<serde_json::Value>,
    now: i64,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO swarm_events (swarm_id, task_id, kind, actor, detail, created_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            swarm_id,
            task_id,
            kind,
            actor,
            detail.map(|d| d.to_string()),
            now
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn row_to_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<SwarmEvent> {
    let detail: Option<String> = r.get(5)?;
    Ok(SwarmEvent {
        id: r.get(0)?,
        swarm_id: r.get(1)?,
        task_id: r.get(2)?,
        kind: r.get(3)?,
        actor: r.get(4)?,
        detail: detail.and_then(|d| serde_json::from_str(&d).ok()),
        created_ns: r.get(6)?,
    })
}

/// The events after `after_id`, oldest first, at most `limit`.
pub fn events_after(
    conn: &Connection,
    swarm_id: &str,
    after_id: i64,
    limit: usize,
) -> rusqlite::Result<Vec<SwarmEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, swarm_id, task_id, kind, actor, detail, created_ns FROM swarm_events
         WHERE swarm_id = ?1 AND id > ?2 ORDER BY id ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        params![swarm_id, after_id, limit.clamp(1, 1000) as i64],
        row_to_event,
    )?;
    rows.collect()
}

/// The newest `limit` events, oldest first.
pub fn recent_events(
    conn: &Connection,
    swarm_id: &str,
    limit: usize,
) -> rusqlite::Result<Vec<SwarmEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, swarm_id, task_id, kind, actor, detail, created_ns FROM (
             SELECT id, swarm_id, task_id, kind, actor, detail, created_ns FROM swarm_events
             WHERE swarm_id = ?1 ORDER BY id DESC LIMIT ?2
         ) ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![swarm_id, limit.clamp(1, 1000) as i64], row_to_event)?;
    rows.collect()
}

/// The id of the swarm's last event (0 when there is none): where a
/// `swarm_wait` starts.
pub fn last_event_id(conn: &Connection, swarm_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM swarm_events WHERE swarm_id = ?1",
        params![swarm_id],
        |r| r.get(0),
    )
}

/// One operator-level decision (`swarm_decisions`, v24).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmDecision {
    pub id: i64,
    pub swarm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub verb: SwarmVerb,
    pub by: DecidedBy,
    /// The operator's words, when the coordinator relayed them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
    /// The relaying launch (the coordinator's, for `chat`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<String>,
    pub created_ns: i64,
}

/// Records a decision and its event (`decision.<verb>`).
pub fn decide(
    conn: &Connection,
    swarm_id: &str,
    task_id: Option<&str>,
    verb: SwarmVerb,
    who: &Decider<'_>,
    now: i64,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO swarm_decisions (swarm_id, task_id, verb, by, said, launch, created_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            swarm_id,
            task_id,
            verb.as_str(),
            who.by.as_str(),
            who.said,
            who.launch,
            now
        ],
    )?;
    let id = conn.last_insert_rowid();
    let _ = event(
        conn,
        swarm_id,
        task_id,
        &format!("decision.{}", verb.as_str()),
        Some(who.actor()),
        Some(serde_json::json!({ "by": who.by.as_str(), "said": who.said })),
        now,
    );
    Ok(id)
}

/// The swarm's decisions, oldest first.
pub fn decisions(conn: &Connection, swarm_id: &str) -> rusqlite::Result<Vec<SwarmDecision>> {
    let mut stmt = conn.prepare(
        "SELECT id, swarm_id, task_id, verb, by, said, launch, created_ns FROM swarm_decisions
         WHERE swarm_id = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![swarm_id], |r| {
        Ok(SwarmDecision {
            id: r.get(0)?,
            swarm_id: r.get(1)?,
            task_id: r.get(2)?,
            verb: SwarmVerb::parse(&r.get::<_, String>(3)?).unwrap_or(SwarmVerb::Launch),
            by: DecidedBy::parse(&r.get::<_, String>(4)?).unwrap_or(DecidedBy::Cli),
            said: r.get(5)?,
            launch: r.get(6)?,
            created_ns: r.get(7)?,
        })
    })?;
    rows.collect()
}

/// The decision that approved the plan, if it was.
pub fn launch_decision(conn: &Connection, swarm_id: &str) -> Option<SwarmDecision> {
    decisions(conn, swarm_id)
        .ok()?
        .into_iter()
        .find(|d| matches!(d.verb, SwarmVerb::Launch | SwarmVerb::Approve) && d.task_id.is_none())
}

/// What [`launch`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    /// The plan was approved by this call (`review` → `running`).
    pub approved: bool,
    /// The tasks whose workers were asked for, by key.
    pub requested: Vec<String>,
}

impl Launched {
    pub fn note(&self, swarm: &SwarmRun) -> String {
        let mut out = if self.approved {
            format!("plan approved: swarm {} is running", swarm.id)
        } else {
            format!("swarm {} is {}", swarm.id, swarm.status.as_str())
        };
        match self.requested.len() {
            0 => out.push_str("; no task is ready to start right now"),
            n => out.push_str(&format!(
                "; starting {n} worker(s): {}",
                self.requested
                    .iter()
                    .map(|k| format!("`{k}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
        out
    }
}

/// The coordinator's `agent_mux_swarm_launch` (and `swarm approve`,
/// `swarm launch`): approves a plan in review, recording who relayed the
/// operator's yes, and asks for the workers of `tasks` (keys, ids or
/// prefixes), or of every ready task when none is named. The engine spawns
/// them at its next pass, within the cap. A coordinator relaying from its
/// chat must quote the operator (`Decider::said`) to approve.
pub fn launch(
    conn: &Connection,
    db: Option<&Path>,
    swarm: &SwarmRun,
    tasks: &[String],
    who: &Decider<'_>,
    env: &super::plan::CheckEnv,
    now: i64,
) -> Result<Launched, String> {
    let board = board(conn, &swarm.id).map_err(|e| e.to_string())?;
    let mut approved = false;
    match swarm.status {
        SwarmStatus::Review => {
            if who.by == DecidedBy::Chat && who.said.is_none() {
                return Err(
                    "quote what the operator said in operator_said: the plan is approved only on                      their word"
                        .into(),
                );
            }
            let problems = super::plan::check(
                &board_plan(swarm, &board),
                &env.with_pool(worker_pool(conn, swarm)),
            );
            if let Some((k, m)) = problems.errors.first() {
                return Err(format!(
                    "the plan has {} error(s); first: {}{m}",
                    problems.errors.len(),
                    if k.is_empty() {
                        String::new()
                    } else {
                        format!("{k}: ")
                    }
                ));
            }
            if board.is_empty() {
                return Err("the plan has no tasks".into());
            }
            set_status(conn, db, &swarm.id, SwarmStatus::Running, now)
                .map_err(|e| e.to_string())?;
            decide(conn, &swarm.id, None, SwarmVerb::Launch, who, now)
                .map_err(|e| e.to_string())?;
            approved = true;
        }
        SwarmStatus::Running | SwarmStatus::Paused | SwarmStatus::Integrating => {}
        SwarmStatus::Planning => {
            return Err("there is no plan yet: submit one with agent_mux_swarm_plan first".into());
        }
        SwarmStatus::Finished => return Err("the swarm is finished".into()),
    }
    let mut chosen: Vec<&BoardTask> = Vec::new();
    if tasks.is_empty() {
        chosen.extend(board.iter().filter(|t| {
            t.task.status == TaskStatus::Open
                && !t.ext.proposed
                && super::engine::unmet_dependencies(t, &board).is_empty()
        }));
    } else {
        for needle in tasks {
            let t = find_task(&board, needle)?;
            if t.task.status != TaskStatus::Open {
                return Err(format!(
                    "`{}` is {}: only an open task starts",
                    t.ext.plan_key,
                    t.task.status.as_str()
                ));
            }
            let unmet = super::engine::unmet_dependencies(t, &board);
            if !unmet.is_empty() {
                return Err(format!(
                    "`{}` waits for {}: start those first",
                    t.ext.plan_key,
                    unmet
                        .iter()
                        .map(|k| format!("`{k}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !chosen.iter().any(|c| c.task.id == t.task.id) {
                chosen.push(t);
            }
        }
    }
    let by: LaunchedBy = who.by.into();
    let mut requested = Vec::new();
    for t in chosen {
        if t.ext.proposed {
            accept_proposal(conn, None, &t.task.id).map_err(|e| e.to_string())?;
        }
        request_launch(conn, None, &t.task.id, by, now).map_err(|e| e.to_string())?;
        let _ = event(
            conn,
            &swarm.id,
            Some(&t.task.id),
            "task.requested",
            Some(who.actor()),
            Some(serde_json::json!({ "by": by.as_str(), "key": t.ext.plan_key })),
            now,
        );
        requested.push(t.ext.plan_key.clone());
    }
    poke(db);
    Ok(Launched {
        approved,
        requested,
    })
}

/// Accepts a proposed task: from now on the dispatcher schedules it.
pub fn accept_proposal(
    conn: &Connection,
    db: Option<&Path>,
    task_id: &str,
) -> rusqlite::Result<usize> {
    let n = conn.execute(
        "UPDATE swarm_tasks SET proposed = 0 WHERE task_id = ?1",
        params![task_id],
    )?;
    poke(db);
    Ok(n)
}

/// Finds a board task by id, id prefix or plan key.
pub fn find_task<'a>(board: &'a [BoardTask], needle: &str) -> Result<&'a BoardTask, String> {
    if let Some(t) = board
        .iter()
        .find(|t| t.task.id == needle || t.ext.plan_key == needle)
    {
        return Ok(t);
    }
    let hits: Vec<&BoardTask> = board
        .iter()
        .filter(|t| t.task.id.starts_with(needle) || t.ext.plan_key.starts_with(needle))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no task `{needle}` on the board")),
        _ => Err(format!(
            "`{needle}` could be {}",
            hits.iter()
                .map(|t| t.ext.plan_key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Ends a swarm: open tasks cancelled, leases released, the row ended.
/// Answers (tasks cancelled, leases released).
pub fn end_swarm(
    conn: &Connection,
    db: Option<&Path>,
    swarm: &SwarmRun,
    now: i64,
) -> Result<(usize, usize), String> {
    let (cancelled, released) = stop_swarm_tasks(conn, db, &swarm.id, now)?;
    conn.execute(
        "UPDATE swarms SET status = 'finished', ended_ns = ?1, updated_ns = ?1 WHERE id = ?2",
        params![now, swarm.id],
    )
    .map_err(|e| e.to_string())?;
    let _ = event(
        conn,
        &swarm.id,
        None,
        "closed",
        None,
        Some(serde_json::json!({ "cancelled": cancelled, "released": released })),
        now,
    );
    poke(db);
    Ok((cancelled, released))
}

/// Cancels the swarm's open tasks and releases the leases its workers
/// hold; another swarm of the workspace keeps its own.
fn stop_swarm_tasks(
    conn: &Connection,
    db: Option<&Path>,
    swarm_id: &str,
    now: i64,
) -> Result<(usize, usize), String> {
    let board = board(conn, swarm_id).map_err(|e| e.to_string())?;
    let mut cancelled = 0;
    let mut released = 0;
    for t in board.iter().filter(|t| !t.task.status.is_terminal()) {
        released += conn
            .execute(
                "DELETE FROM agent_leases WHERE task_id = ?1",
                params![t.task.id],
            )
            .map_err(|e| e.to_string())?;
        bstore::update_task(
            conn,
            db,
            &t.task.id,
            t.task.assigned_to.as_deref().unwrap_or("user"),
            TaskStatus::Cancelled,
            Some(serde_json::json!("Swarm stopped")),
            None,
            None,
            None,
            now,
        )?;
        cancelled += 1;
    }
    Ok((cancelled, released))
}

/// An operator decision on a swarm (`ControlRequest::SwarmAction`, the
/// CLI's verbs, the screen's keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SwarmVerb {
    /// Approve the plan: `review` → `running` (every ready task starts).
    Approve,
    /// Approve the plan if it waits, and start the named ready task (or
    /// every ready one): the coordinator's `agent_mux_swarm_launch`.
    Launch,
    /// Cancel a task that has not finished; a running worker is stopped.
    Cancel,
    /// Run the planner again with notes (needs the TUI).
    Replan,
    Pause,
    Resume,
    /// Put a failed, lost or cancelled task back on the board.
    Retry,
    /// Merge a finished task's branch into the workspace.
    Merge,
    /// Keep a finished task's branch without merging it.
    SetAside,
    /// Accept a task a worker proposed.
    Accept,
    /// Drop a task a worker proposed.
    Drop,
    /// End the swarm: stop its sessions (needs the TUI to stop them).
    Close,
    /// Start a new coordinator in place of one that exited (needs the TUI).
    Restart,
    /// Review a finished task (again, after a fix round): a new review goes
    /// on the board; the earlier ones stay (v25).
    Review,
}

impl SwarmVerb {
    pub fn parse(s: &str) -> Option<SwarmVerb> {
        Some(match s {
            "approve" => SwarmVerb::Approve,
            "launch" | "start-task" => SwarmVerb::Launch,
            "cancel" => SwarmVerb::Cancel,
            "replan" | "re-plan" => SwarmVerb::Replan,
            "pause" => SwarmVerb::Pause,
            "resume" => SwarmVerb::Resume,
            "retry" => SwarmVerb::Retry,
            "merge" => SwarmVerb::Merge,
            "set-aside" | "setaside" => SwarmVerb::SetAside,
            "accept" => SwarmVerb::Accept,
            "drop" => SwarmVerb::Drop,
            "close" | "stop" => SwarmVerb::Close,
            "restart" | "restart-coordinator" => SwarmVerb::Restart,
            "review" | "re-review" => SwarmVerb::Review,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            SwarmVerb::Approve => "approve",
            SwarmVerb::Launch => "launch",
            SwarmVerb::Cancel => "cancel",
            SwarmVerb::Replan => "replan",
            SwarmVerb::Pause => "pause",
            SwarmVerb::Resume => "resume",
            SwarmVerb::Retry => "retry",
            SwarmVerb::Merge => "merge",
            SwarmVerb::SetAside => "set-aside",
            SwarmVerb::Accept => "accept",
            SwarmVerb::Drop => "drop",
            SwarmVerb::Close => "close",
            SwarmVerb::Restart => "restart",
            SwarmVerb::Review => "review",
        }
    }
    /// Whether the verb names a task.
    pub fn takes_task(self) -> bool {
        matches!(
            self,
            SwarmVerb::Retry
                | SwarmVerb::Cancel
                | SwarmVerb::Merge
                | SwarmVerb::SetAside
                | SwarmVerb::Accept
                | SwarmVerb::Drop
                | SwarmVerb::Review
        )
    }
    /// Whether a coordinator relaying the verb from its chat must quote the
    /// operator (`operator_said`): what starts, merges, discards or ends
    /// work (spec §5.3).
    pub fn needs_operator_words(self) -> bool {
        matches!(
            self,
            SwarmVerb::Approve
                | SwarmVerb::Launch
                | SwarmVerb::Merge
                | SwarmVerb::SetAside
                | SwarmVerb::Cancel
                | SwarmVerb::Close
        )
    }
    /// Whether only the running TUI can carry it out (it starts or stops
    /// sessions).
    pub fn needs_tui(self) -> bool {
        matches!(
            self,
            SwarmVerb::Replan | SwarmVerb::Close | SwarmVerb::Restart
        )
    }
}

/// The board as a plan again, for re-checking it before approval.
pub fn board_plan(swarm: &SwarmRun, board: &[BoardTask]) -> Plan {
    Plan {
        summary: swarm.plan_summary.clone().unwrap_or_default(),
        tasks: board
            .iter()
            .filter(|t| t.task.status != TaskStatus::Cancelled && t.ext.review_of.is_none())
            .map(|t| PlannedTask {
                key: t.ext.plan_key.clone(),
                title: t.task.title.clone(),
                instructions: t.task.instructions.clone(),
                acceptance: t.ext.acceptance.clone(),
                files: t.task.files.clone(),
                depends_on: t.ext.depends_on.clone(),
                harness: t.task.target_harness.clone(),
                agent: t.task.target_agent.clone(),
                isolation: Some(t.task.isolation.as_str().to_string()),
                review: t.ext.review_skip.then_some(false),
            })
            .collect(),
        review: None,
    }
}

/// Carries out a verb that only changes rows (and, for `merge`, runs git in
/// the workspace). `env` re-checks the plan on approval. Answers what was
/// done, for the operator.
#[allow(clippy::too_many_arguments)]
pub fn apply(
    conn: &Connection,
    db: Option<&Path>,
    swarm: &SwarmRun,
    verb: SwarmVerb,
    task: Option<&str>,
    env: &super::plan::CheckEnv,
    who: &Decider<'_>,
    now: i64,
) -> Result<String, String> {
    if verb.needs_tui() {
        return Err(format!("{verb:?} needs the running agent-mux TUI"));
    }
    if who.by == DecidedBy::Chat && verb.needs_operator_words() && who.said.is_none() {
        return Err(format!(
            "quote what the operator said in operator_said: `{}` happens only on their word",
            verb.as_str()
        ));
    }
    let board = board(conn, &swarm.id).map_err(|e| e.to_string())?;
    let target = match (verb.takes_task(), task) {
        (true, Some(t)) => Some(find_task(&board, t)?),
        (true, None) => return Err("name the task (its key or id)".into()),
        (false, _) => None,
    };
    let status = swarm.status;
    let task_id = target.map(|t| t.task.id.as_str());
    match verb {
        SwarmVerb::Approve => {
            if status != SwarmStatus::Review {
                return Err(format!(
                    "there is no plan to approve: the swarm is {}",
                    status.as_str()
                ));
            }
            let done = launch(conn, db, swarm, &[], who, env, now)?;
            Ok(format!(
                "plan approved: {} task(s); {}",
                board.len(),
                match done.requested.len() {
                    0 => "no task is ready yet".to_string(),
                    n => format!("{n} worker(s) start now"),
                }
            ))
        }
        SwarmVerb::Launch => {
            let tasks: Vec<String> = task.map(|t| vec![t.to_string()]).unwrap_or_default();
            let done = launch(conn, db, swarm, &tasks, who, env, now)?;
            let current = get_swarm(conn, &swarm.id)
                .ok()
                .flatten()
                .unwrap_or_else(|| swarm.clone());
            Ok(done.note(&current))
        }
        SwarmVerb::Cancel => {
            let t = target.expect("takes a task");
            if t.task.status.is_terminal() {
                return Err(format!(
                    "`{}` is already {}",
                    t.ext.plan_key,
                    t.task.status.as_str()
                ));
            }
            bstore::update_task(
                conn,
                db,
                &t.task.id,
                t.task.assigned_to.as_deref().unwrap_or(who.actor()),
                TaskStatus::Cancelled,
                Some(serde_json::json!(format!(
                    "cancelled by the operator{}",
                    who.said.map(|s| format!(": {s}")).unwrap_or_default()
                ))),
                None,
                None,
                None,
                now,
            )?;
            decide(conn, &swarm.id, task_id, verb, who, now).map_err(|e| e.to_string())?;
            Ok(format!("cancelled `{}`", t.ext.plan_key))
        }
        SwarmVerb::Pause => {
            if status != SwarmStatus::Running {
                return Err(format!(
                    "only a running swarm pauses (it is {})",
                    status.as_str()
                ));
            }
            set_status(conn, db, &swarm.id, SwarmStatus::Paused, now).map_err(|e| e.to_string())?;
            decide(conn, &swarm.id, None, verb, who, now).map_err(|e| e.to_string())?;
            Ok("paused: running workers continue, nothing new starts".into())
        }
        SwarmVerb::Resume => {
            if status != SwarmStatus::Paused {
                return Err(format!(
                    "the swarm is not paused (it is {})",
                    status.as_str()
                ));
            }
            set_status(conn, db, &swarm.id, SwarmStatus::Running, now)
                .map_err(|e| e.to_string())?;
            decide(conn, &swarm.id, None, verb, who, now).map_err(|e| e.to_string())?;
            Ok("resumed".into())
        }
        SwarmVerb::Retry => {
            let t = target.expect("takes a task");
            bstore::reopen_task(conn, db, &t.task.id, now)?;
            reset_dispatch(conn, &t.task.id).map_err(|e| e.to_string())?;
            clear_check(conn, &swarm.id).map_err(|e| e.to_string())?;
            decide(conn, &swarm.id, task_id, verb, who, now).map_err(|e| e.to_string())?;
            if matches!(status, SwarmStatus::Integrating | SwarmStatus::Finished) {
                set_status(conn, db, &swarm.id, SwarmStatus::Running, now)
                    .map_err(|e| e.to_string())?;
            }
            Ok(format!("`{}` is back on the board", t.ext.plan_key))
        }
        SwarmVerb::Merge | SwarmVerb::SetAside => {
            let t = target.expect("takes a task");
            if !super::engine::awaits_review(t) {
                return Err(format!(
                    "`{}` has no branch waiting for review",
                    t.ext.plan_key
                ));
            }
            let branch = t.task.branch.clone().unwrap_or_default();
            let merged = verb == SwarmVerb::Merge;
            let detail = if merged {
                Some(super::merge_branch(Path::new(&swarm.workspace), &branch)?)
            } else {
                None
            };
            bstore::record_review(
                conn,
                db,
                &swarm.workspace,
                &t.task.id,
                merged,
                Some(&branch),
                detail.as_deref(),
                now,
            )
            .map_err(|e| e.to_string())?;
            decide(conn, &swarm.id, task_id, verb, who, now).map_err(|e| e.to_string())?;
            let _ = event(
                conn,
                &swarm.id,
                task_id,
                if merged {
                    "review.merged"
                } else {
                    "review.set_aside"
                },
                Some(who.actor()),
                Some(serde_json::json!({ "branch": branch, "commit": detail })),
                now,
            );
            let mut out = match detail {
                Some(c) => format!("merged {branch} ({c})"),
                None => format!("set {branch} aside"),
            };
            if merged && let Some(w) = unreviewed_warning(conn, swarm, t, &board) {
                out.push_str(&format!(" — {w}"));
            }
            Ok(out)
        }
        SwarmVerb::Accept | SwarmVerb::Drop => {
            let t = target.expect("takes a task");
            if !t.ext.proposed {
                return Err(format!("`{}` is not a proposal", t.ext.plan_key));
            }
            decide(conn, &swarm.id, task_id, verb, who, now).map_err(|e| e.to_string())?;
            if verb == SwarmVerb::Accept {
                accept_proposal(conn, db, &t.task.id).map_err(|e| e.to_string())?;
                Ok(format!("accepted `{}`", t.ext.plan_key))
            } else {
                delete_task(conn, &t.task.id).map_err(|e| e.to_string())?;
                poke(db);
                Ok(format!("dropped `{}`", t.ext.plan_key))
            }
        }
        SwarmVerb::Review => {
            let t = target.expect("takes a task");
            // naming the review itself means the task it reviews
            let reviewed = match &t.ext.review_of {
                Some(of) => board
                    .iter()
                    .find(|o| &o.task.id == of)
                    .ok_or_else(|| format!("the task `{}` reviews is gone", t.ext.plan_key))?,
                None => t,
            };
            if reviewed.task.status != TaskStatus::Done {
                return Err(format!(
                    "`{}` is not done yet ({}): it is reviewed when it is",
                    reviewed.ext.plan_key,
                    reviewed.task.status.as_str()
                ));
            }
            if reviewed.review.is_some() {
                return Err(format!(
                    "`{}` was already {} by the operator",
                    reviewed.ext.plan_key,
                    reviewed.review.as_deref().unwrap_or("decided")
                ));
            }
            let existing = reviewed
                .ext
                .review_task
                .as_deref()
                .and_then(|id| board.iter().find(|o| o.task.id == id));
            if existing.is_some_and(|r| !r.task.status.is_terminal()) {
                return Err(format!("`{}` is being reviewed now", reviewed.ext.plan_key));
            }
            // a new review each time: the earlier verdicts stay on the board
            let key = add_review(conn, db, swarm, &reviewed.task.id, now)?;
            let done = match existing {
                Some(r) => format!(
                    "`{}` is reviewed again by `{key}` (the last review was `{}`)",
                    reviewed.ext.plan_key, r.ext.plan_key
                ),
                None => format!("`{}` is reviewed by `{key}`", reviewed.ext.plan_key),
            };
            clear_check(conn, &swarm.id).map_err(|e| e.to_string())?;
            decide(conn, &swarm.id, Some(&reviewed.task.id), verb, who, now)
                .map_err(|e| e.to_string())?;
            if matches!(status, SwarmStatus::Integrating | SwarmStatus::Finished) {
                set_status(conn, db, &swarm.id, SwarmStatus::Running, now)
                    .map_err(|e| e.to_string())?;
            }
            Ok(done)
        }
        SwarmVerb::Replan | SwarmVerb::Close | SwarmVerb::Restart => {
            unreachable!("needs the TUI")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::store::{OpenOptions, open_aux, open_rw};

    pub(crate) fn store() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let _ = open_rw(&db, OpenOptions::default()).unwrap();
        let conn = open_aux(&db).unwrap();
        (dir, conn)
    }

    fn new_swarm(ws: &str) -> NewSwarm {
        NewSwarm {
            workspace: ws.into(),
            goal: "ship it".into(),
            planner_harness: "claude".into(),
            max_workers: 2,
            isolation: TaskIsolation::Shared,
            gate: true,
            check_cmd: Some("  ".into()),
            owner_run: Some("run-1".into()),
            worker_pool: vec![],
            headless: false,
            review: crate::bus::swarm::review::ReviewSettings::off(),
        }
    }

    fn pt(key: &str, deps: &[&str]) -> PlannedTask {
        PlannedTask {
            key: key.into(),
            title: format!("Task {key}"),
            instructions: "do it".into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn every_plan_carries_the_review_unless_the_plan_dismisses_it() {
        use super::super::review::{self, ReviewState};
        let (dir, conn) = store();
        let ws = dir.path().to_string_lossy().into_owned();
        let mut n = new_swarm(&ws);
        n.review = review::ReviewSettings::default();
        n.worker_pool = vec!["claude".into(), "codex".into()];
        let s = create_swarm(&conn, None, &n, 1).unwrap();
        let env = super::super::plan::CheckEnv {
            harnesses: vec!["claude".into(), "codex".into()],
            ..Default::default()
        };
        // a task the operator wants unreviewed, one reviewed
        let mut quiet = pt("docs", &[]);
        quiet.review = Some(false);
        let plan = Plan {
            summary: "two".into(),
            tasks: vec![pt("code", &[]), quiet],
            review: None,
        };
        let table = match submit_plan(&conn, None, &s, &plan, &env, 2).unwrap() {
            Submitted::Accepted { table, .. } => table,
            other => panic!("{other:?}"),
        };
        assert!(table.contains("| review |"), "{table}");
        assert!(table.contains("another harness |"), "{table}");
        assert!(
            table.contains("Review: each finished task is reviewed by the `reviewer` agent"),
            "{table}"
        );
        let b = board(&conn, &s.id).unwrap();
        assert!(!b[0].ext.review_skip && b[1].ext.review_skip);
        // board_plan keeps the dismissal for a re-check
        assert_eq!(board_plan(&s, &b).tasks[1].review, Some(false));

        // `code` finishes: its review goes to codex (not the worker's claude)
        let code = b[0].task.id.clone();
        bstore::update_task(
            &conn,
            None,
            &code,
            "w1",
            TaskStatus::Done,
            Some(serde_json::json!("done")),
            None,
            None,
            None,
            3,
        )
        .unwrap();
        let b = board(&conn, &s.id).unwrap();
        assert_eq!(review::state(&b[0], &b, true), Some(ReviewState::Pending));
        let key = add_review(&conn, None, &s, &code, 4).unwrap();
        assert_eq!(key, "code-review");
        let b = board(&conn, &s.id).unwrap();
        let r = b.iter().find(|t| t.ext.plan_key == "code-review").unwrap();
        assert_eq!(r.task.target_harness.as_deref(), Some("codex"));
        assert_eq!(r.task.target_agent.as_deref(), Some("reviewer"));
        assert_eq!(r.ext.review_of.as_deref(), Some(code.as_str()));
        // reviews are not plan tasks
        assert!(
            !board_plan(&s, &b)
                .tasks
                .iter()
                .any(|t| t.key == "code-review")
        );
        assert!(!plan_table(&s, &b, &review::settings(&conn, &s.id)).contains("code-review"));

        // the plan may also switch reviews off for the whole swarm
        let s2 = create_swarm(&conn, None, &n, 5).unwrap();
        let mut off = plan.clone();
        off.review = Some(super::super::plan::PlanReview {
            enabled: Some(false),
            harness: None,
        });
        submit_plan(&conn, None, &s2, &off, &env, 6).unwrap();
        assert!(!review::settings(&conn, &s2.id).enabled);
        // and a review harness must be installed
        let s3 = create_swarm(&conn, None, &n, 7).unwrap();
        let mut bad = plan.clone();
        bad.review = Some(super::super::plan::PlanReview {
            enabled: None,
            harness: Some("opencode".into()),
        });
        match submit_plan(&conn, None, &s3, &bad, &env, 8).unwrap() {
            Submitted::Rejected(p) => assert!(
                p.errors
                    .iter()
                    .any(|(_, m)| m.contains("review harness `opencode`")),
                "{p:?}"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn tasks_that_name_no_harness_run_on_the_workers_harness() {
        let (dir, conn) = store();
        let ws = dir.path().to_string_lossy().into_owned();
        let mut n = new_swarm(&ws);
        n.worker_pool = vec!["codex".into()];
        let s = create_swarm(&conn, None, &n, 1).unwrap();
        assert_eq!(worker_harness(&conn, &s), "codex");
        let mut named = pt("b", &[]);
        named.harness = Some("agy".into());
        let b = replace_draft(
            &conn,
            None,
            &s,
            &Plan {
                summary: String::new(),
                tasks: vec![pt("a", &[]), named],
                review: None,
            },
            2,
        )
        .unwrap();
        let harness: Vec<Option<&str>> =
            b.iter().map(|t| t.task.target_harness.as_deref()).collect();
        assert_eq!(harness, [Some("codex"), Some("agy")]);
        // An edit moves a task to another harness, agent and isolation.
        let mut moved = pt("b", &[]);
        moved.harness = Some("claude".into());
        moved.agent = Some("reviewer".into());
        moved.isolation = Some("worktree".into());
        update_plan_task(&conn, None, &b[1].task.id, &moved, 3).unwrap();
        let t = bstore::get_task(&conn, &b[1].task.id).unwrap().unwrap();
        assert_eq!(
            (
                t.target_harness.as_deref(),
                t.target_agent.as_deref(),
                t.isolation
            ),
            (Some("claude"), Some("reviewer"), TaskIsolation::Worktree)
        );
        // Without a harness of its own, back to the workers'.
        update_plan_task(&conn, None, &b[1].task.id, &pt("b", &[]), 4).unwrap();
        let t = bstore::get_task(&conn, &b[1].task.id).unwrap().unwrap();
        assert_eq!(
            (t.target_harness.as_deref(), t.target_agent.as_deref()),
            (Some("codex"), None)
        );
        // Without a workers' harness, the coordinator's.
        let other = tempfile::tempdir().unwrap();
        let s2 = create_swarm(&conn, None, &new_swarm(&other.path().to_string_lossy()), 5).unwrap();
        assert_eq!(worker_harness(&conn, &s2), "claude");
    }

    #[test]
    fn a_workspace_runs_several_swarms_and_each_ends_alone() {
        let (dir, conn) = store();
        let ws = normalize_workspace(dir.path());
        let a = create_swarm(&conn, None, &new_swarm(&ws), 1).unwrap();
        let b = create_swarm(&conn, None, &new_swarm(&ws), 2).unwrap();
        let ids: Vec<String> = live_swarms_in(&conn, &ws)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, [a.id.clone(), b.id.clone()]);
        assert_eq!(
            live_swarm(&conn, &ws).unwrap().unwrap().id,
            b.id,
            "the newest"
        );
        // Unnamed, a command is ambiguous; named, it is not.
        let err = resolve_swarm(&conn, &ws, None).unwrap_err();
        assert!(err.contains("2 swarms run") && err.contains(&a.id), "{err}");
        assert_eq!(
            resolve_swarm(&conn, &ws, Some(&a.id[6..])).unwrap().id,
            a.id,
            "the short id"
        );
        let plan = |k: &str| Plan {
            summary: String::new(),
            tasks: vec![pt(k, &[])],
            review: None,
        };
        let ta = replace_draft(&conn, None, &a, &plan("x"), 3).unwrap();
        let tb = replace_draft(&conn, None, &b, &plan("y"), 3).unwrap();
        assert_eq!(
            swarm_of_task(&conn, &tb[0].task.id).unwrap().unwrap().id,
            b.id
        );
        // Ending one leaves the other's tasks and its row alone.
        assert_eq!(end_swarm(&conn, None, &a, 4).unwrap().0, 1);
        let a_task = bstore::get_task(&conn, &ta[0].task.id).unwrap().unwrap();
        let b_task = bstore::get_task(&conn, &tb[0].task.id).unwrap().unwrap();
        assert_eq!(
            (a_task.status, b_task.status),
            (TaskStatus::Cancelled, TaskStatus::Open)
        );
        assert_eq!(resolve_swarm(&conn, &ws, None).unwrap().id, b.id);
        // Only a task of an unapproved swarm waits for its plan.
        assert!(bstore::plan_blocks(&conn, &tb[0].task.id).is_some());
        set_status(&conn, None, &b.id, SwarmStatus::Running, 5).unwrap();
        assert!(bstore::plan_blocks(&conn, &tb[0].task.id).is_none());
    }

    #[test]
    fn the_workers_pool_bounds_the_plan_and_the_operator_widens_it() {
        let (dir, conn) = store();
        let ws = dir.path().to_string_lossy().into_owned();
        let mut n = new_swarm(&ws);
        n.worker_pool = vec!["agy".into(), "codex".into(), "agy".into()];
        let s = create_swarm(&conn, None, &n, 1).unwrap();
        assert_eq!(
            worker_pool(&conn, &s),
            ["agy", "codex"],
            "deduplicated, in order"
        );
        assert_eq!(worker_harness(&conn, &s), "agy");
        let env = super::super::plan::CheckEnv {
            harnesses: vec!["claude".into(), "codex".into(), "agy".into()],
            git_repo: false,
            ..Default::default()
        };
        let on = |h: &str| {
            let mut t = pt("a", &[]);
            t.harness = Some(h.into());
            Plan {
                summary: String::new(),
                tasks: vec![t],
                review: None,
            }
        };
        match submit_plan(&conn, None, &s, &on("claude"), &env, 2).unwrap() {
            Submitted::Rejected(p) => assert!(
                p.errors[0]
                    .1
                    .contains("not in the swarm's workers' pool (agy, codex)"),
                "{p:?}"
            ),
            other => panic!("expected a rejection, got {other:?}"),
        }
        let accepted = submit_plan(&conn, None, &s, &on("codex"), &env, 3).unwrap();
        assert!(
            matches!(accepted, Submitted::Accepted { .. }),
            "{accepted:?}"
        );
        // The operator moves the task to claude: claude joins the pool.
        let task = board(&conn, &s.id).unwrap().remove(0);
        let mut moved = pt("a", &[]);
        moved.harness = Some("claude".into());
        update_plan_task(&conn, None, &task.task.id, &moved, 4).unwrap();
        assert_eq!(worker_pool(&conn, &s), ["agy", "codex", "claude"]);
    }

    #[test]
    fn a_board_in_plan_order() {
        let (dir, conn) = store();
        let ws = dir.path().to_string_lossy().into_owned();
        let s = create_swarm(&conn, None, &new_swarm(&ws), 1).unwrap();
        assert_eq!(s.status, SwarmStatus::Planning);
        assert_eq!(s.check_cmd, None, "a blank check is none");

        let plan = Plan {
            summary: "two".into(),
            tasks: vec![pt("b", &[]), pt("a", &["b"])],
            review: None,
        };
        let b = replace_draft(&conn, None, &s, &plan, 3).unwrap();
        let keys: Vec<&str> = b.iter().map(|t| t.ext.plan_key.as_str()).collect();
        assert_eq!(keys, ["b", "a"]);
        assert_eq!(b[1].ext.depends_on, ["b"]);
        assert_eq!(b[0].task.target_harness.as_deref(), Some("claude"));
        assert_eq!(b[0].task.created_by, creator(&s.id));
        assert_eq!(find_task(&b, "a").unwrap().ext.plan_key, "a");

        // A re-plan replaces the draft but keeps a task that started.
        mark_spawned(&conn, &b[0].task.id).unwrap();
        let again = Plan {
            summary: String::new(),
            tasks: vec![pt("b", &[]), pt("c", &[])],
            review: None,
        };
        let b2 = replace_draft(&conn, None, &s, &again, 4).unwrap();
        let keys: Vec<&str> = b2.iter().map(|t| t.ext.plan_key.as_str()).collect();
        assert_eq!(keys, ["b", "c"]);
        assert_eq!(b2[0].task.id, b[0].task.id, "the started task is kept");
        assert!(bstore::get_task(&conn, &b[1].task.id).unwrap().is_none());

        // A worker's task_split becomes a proposal on its swarm's board.
        let held = b2[1].task.id.clone();
        conn.execute(
            "UPDATE agent_tasks SET status = 'running', assigned_to = 'worker-L' WHERE id = ?1",
            params![held],
        )
        .unwrap();
        let (sw, t) = swarm_of_worker(&conn, "worker-L").unwrap().unwrap();
        assert_eq!(
            (sw.id.as_str(), t.task.id.as_str()),
            (s.id.as_str(), held.as_str())
        );
        assert!(swarm_of_worker(&conn, "someone-else").unwrap().is_none());
        let spec = |title: &str| NewTaskSpec {
            title: title.into(),
            instructions: "found while working".into(),
            ..Default::default()
        };
        let made = propose(
            &conn,
            None,
            &sw,
            &[spec("Fix the flaky test!"), spec("Fix the flaky test")],
            4,
        )
        .unwrap();
        let keys: Vec<&str> = made.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["fix-the-flaky-test", "fix-the-flaky-test-2"]);
        let b3 = board(&conn, &s.id).unwrap();
        assert!(
            b3.iter().filter(|t| t.ext.proposed).count() == 2,
            "the gate is on: proposals"
        );
        assert!(proposal_note(&sw, &made).contains("as a proposal for the operator"));

        // The coordinator: found by its launch, and the address a worker's
        // `to = "coordinator"` resolves to.
        set_coordinator_launch(&conn, None, &s.id, "coord-L", 4).unwrap();
        assert_eq!(
            swarm_of_coordinator(&conn, "coord-L").unwrap().unwrap().id,
            s.id
        );
        assert_eq!(
            coordinator_address(&conn, "worker-L").as_deref(),
            Some("coord-L")
        );
        assert_eq!(coordinator_address(&conn, "someone-else"), None);
        assert_eq!(
            get_swarm(&conn, &s.id)
                .unwrap()
                .unwrap()
                .planner_launch
                .as_deref(),
            Some("coord-L")
        );

        // A plan with errors changes nothing; a sound one replaces the draft.
        let env = super::super::plan::CheckEnv {
            harnesses: vec!["claude".into()],
            agents: vec![],
            writing_agents: vec![],
            git_repo: false,
            default_isolation: TaskIsolation::Shared,
            pool: vec![],
        };
        let live = get_swarm(&conn, &s.id).unwrap().unwrap();
        let before = board(&conn, &s.id).unwrap().len();
        let bad = Plan {
            summary: "x".into(),
            tasks: vec![pt("z", &["nope"])],
            review: None,
        };
        match submit_plan(&conn, None, &live, &bad, &env, 4).unwrap() {
            Submitted::Rejected(p) => {
                assert!(
                    p.errors
                        .iter()
                        .any(|(_, m)| m.contains("depends on `nope`")),
                    "{p:?}"
                );
                assert!(
                    submitted_note(&live, &Submitted::Rejected(p))
                        .starts_with("Plan rejected: 1 error(s)")
                );
            }
            other => panic!("expected a rejection, got {other:?}"),
        }
        assert_eq!(
            board(&conn, &s.id).unwrap().len(),
            before,
            "nothing changed"
        );
        let good = Plan {
            summary: "two".into(),
            tasks: vec![pt("y", &[]), pt("z", &["y"])],
            review: None,
        };
        match submit_plan(&conn, None, &live, &good, &env, 4).unwrap() {
            Submitted::Accepted { status, .. } => assert_eq!(status, SwarmStatus::Review),
            other => panic!("expected an acceptance, got {other:?}"),
        }
        let after = get_swarm(&conn, &s.id).unwrap().unwrap();
        assert_eq!(
            (after.status, after.plan_summary.as_deref()),
            (SwarmStatus::Review, Some("two"))
        );
        let keys: Vec<String> = board(&conn, &s.id)
            .unwrap()
            .into_iter()
            .map(|t| t.ext.plan_key)
            .collect();
        assert!(
            keys.contains(&"b".to_string()),
            "the task that ran stays: {keys:?}"
        );
        assert!(
            keys.contains(&"y".to_string()) && keys.contains(&"z".to_string()),
            "{keys:?}"
        );
        let mut running = after.clone();
        running.status = SwarmStatus::Running;
        assert!(
            submit_plan(&conn, None, &running, &good, &env, 4)
                .unwrap_err()
                .contains("already approved")
        );

        let (cancelled, _) = end_swarm(&conn, None, &s, 5).unwrap();
        assert_eq!(cancelled, 4);
        assert!(live_swarm(&conn, &s.workspace).unwrap().is_none());
        create_swarm(&conn, None, &new_swarm(&ws), 6).expect("a new swarm after the end");
    }

    fn check_env() -> super::super::plan::CheckEnv {
        super::super::plan::CheckEnv {
            harnesses: vec!["claude".into(), "codex".into()],
            agents: vec![],
            writing_agents: vec![],
            git_repo: false,
            default_isolation: TaskIsolation::Shared,
            pool: vec![],
        }
    }

    #[test]
    fn a_plan_is_approved_on_the_operators_word_and_its_ready_tasks_are_requested() {
        let (dir, conn) = store();
        let ws = dir.path().to_string_lossy().into_owned();
        let s = create_swarm(&conn, None, &new_swarm(&ws), 1).unwrap();
        assert_eq!(s.gate_mode, GateMode::Chat);
        assert!(!s.headless);
        let plan = Plan {
            summary: "two steps".into(),
            tasks: vec![pt("a", &[]), pt("b", &["a"])],
            review: None,
        };
        let done = submit_plan(&conn, None, &s, &plan, &check_env(), 2).unwrap();
        let Submitted::Accepted { table, status, .. } = &done else {
            panic!("{done:?}");
        };
        assert_eq!(*status, SwarmStatus::Review);
        assert!(table.contains("| `a` | Task a |"), "{table}");
        assert!(table.contains("Up to 2 worker(s)"), "{table}");
        let s = get_swarm(&conn, &s.id).unwrap().unwrap();

        // The coordinator must quote the operator to approve.
        let err = launch(
            &conn,
            None,
            &s,
            &[],
            &Decider::chat(None, Some("coord")),
            &check_env(),
            3,
        )
        .unwrap_err();
        assert!(err.contains("operator_said"), "{err}");
        assert_eq!(
            get_swarm(&conn, &s.id).unwrap().unwrap().status,
            SwarmStatus::Review
        );

        let done = launch(
            &conn,
            None,
            &s,
            &[],
            &Decider::chat(Some("go"), Some("coord")),
            &check_env(),
            4,
        )
        .unwrap();
        assert!(done.approved);
        assert_eq!(done.requested, ["a"], "b waits for a");
        let s = get_swarm(&conn, &s.id).unwrap().unwrap();
        assert_eq!(s.status, SwarmStatus::Running);
        let rows = board(&conn, &s.id).unwrap();
        assert_eq!(rows[0].ext.launched_by, Some(LaunchedBy::Coordinator));
        assert_eq!(rows[1].ext.launched_by, None);
        let d = launch_decision(&conn, &s.id).expect("recorded");
        assert_eq!(
            (d.by, d.said.as_deref(), d.launch.as_deref()),
            (DecidedBy::Chat, Some("go"), Some("coord"))
        );
        let kinds: Vec<String> = recent_events(&conn, &s.id, 50)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                "started",
                "plan.proposed",
                "decision.launch",
                "task.requested"
            ]
        );

        // Launching a blocked task is refused; a done one too.
        let err = launch(
            &conn,
            None,
            &s,
            &["b".into()],
            &Decider::TUI,
            &check_env(),
            5,
        )
        .unwrap_err();
        assert!(err.contains("waits for `a`"), "{err}");
        // The dispatcher's offer is kept once; a retry forgets it.
        offer_task(&conn, &rows[1].task.id, 10).unwrap();
        offer_task(&conn, &rows[1].task.id, 20).unwrap();
        assert_eq!(board(&conn, &s.id).unwrap()[1].ext.offered_ns, Some(10));
        bstore::update_task(
            &conn,
            None,
            &rows[1].task.id,
            "w",
            TaskStatus::Failed,
            None,
            Some("boom".into()),
            None,
            None,
            30,
        )
        .unwrap();
        apply(
            &conn,
            None,
            &s,
            SwarmVerb::Retry,
            Some("b"),
            &check_env(),
            &Decider::CLI,
            31,
        )
        .unwrap();
        let b = &board(&conn, &s.id).unwrap()[1];
        assert_eq!((b.ext.offered_ns, b.ext.launched_by), (None, None));
        assert_eq!(b.task.status, TaskStatus::Open);
        // The log can be waited on from an id.
        let last = last_event_id(&conn, &s.id).unwrap();
        assert!(events_after(&conn, &s.id, last, 10).unwrap().is_empty());
        assert_eq!(
            events_after(&conn, &s.id, 0, 100).unwrap().len() as i64,
            last - events_after(&conn, &s.id, 0, 1).unwrap()[0].id + 1
        );
    }

    #[test]
    fn the_gate_off_runs_the_plan_at_once_and_cancel_needs_the_operators_words_from_the_chat() {
        let (dir, conn) = store();
        let ws = dir.path().to_string_lossy().into_owned();
        let mut n = new_swarm(&ws);
        n.gate = false;
        let s = create_swarm(&conn, None, &n, 1).unwrap();
        assert_eq!(s.gate_mode, GateMode::Off);
        let plan = Plan {
            summary: String::new(),
            tasks: vec![pt("a", &[])],
            review: None,
        };
        let done = submit_plan(&conn, None, &s, &plan, &check_env(), 2).unwrap();
        assert!(matches!(
            done,
            Submitted::Accepted {
                status: SwarmStatus::Running,
                ..
            }
        ));
        let s = get_swarm(&conn, &s.id).unwrap().unwrap();
        let err = apply(
            &conn,
            None,
            &s,
            SwarmVerb::Cancel,
            Some("a"),
            &check_env(),
            &Decider::chat(None, Some("c")),
            3,
        )
        .unwrap_err();
        assert!(err.contains("operator_said"), "{err}");
        let d = apply(
            &conn,
            None,
            &s,
            SwarmVerb::Cancel,
            Some("a"),
            &check_env(),
            &Decider::chat(Some("stop a"), Some("c")),
            4,
        )
        .unwrap();
        assert_eq!(d, "cancelled `a`");
        assert_eq!(
            board(&conn, &s.id).unwrap()[0].task.status,
            TaskStatus::Cancelled
        );
        let ds = decisions(&conn, &s.id).unwrap();
        assert_eq!(ds.len(), 1);
        assert_eq!(
            (ds[0].verb, ds[0].said.as_deref()),
            (SwarmVerb::Cancel, Some("stop a"))
        );
    }
}
