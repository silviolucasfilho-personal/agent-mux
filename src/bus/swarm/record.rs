//! The orchestrated swarm's rows (schema v21): `swarms`, one swarm's run
//! (at most one live per workspace), and `swarm_tasks`, the plan's extension
//! of the board's `agent_tasks` rows (key, order, dependencies,
//! acceptance). See `docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md` §7.

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
    pub gate: bool,
    pub check_cmd: Option<String>,
    pub check_result: Option<String>,
    /// The TUI (`App::app_run_id`) that dispatches this swarm.
    pub owner_run: Option<String>,
    pub started_ns: i64,
    pub updated_ns: i64,
    pub ended_ns: Option<i64>,
}

/// What a new swarm starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSwarm {
    pub workspace: String,
    pub goal: String,
    pub planner_harness: String,
    pub max_workers: usize,
    pub isolation: TaskIsolation,
    pub gate: bool,
    pub check_cmd: Option<String>,
    pub owner_run: Option<String>,
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
}

/// A board task with its plan extension and, for a finished worktree
/// task, what the operator did with its branch (`merged`, `discarded`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardTask {
    pub task: BusTask,
    pub ext: SwarmTaskExt,
    pub review: Option<String>,
}

const SWARM_COLS: &str = "id, workspace, goal, status, planner_harness, planner_launch, \
    plan_summary, plan_error, max_workers, isolation, gate, check_cmd, check_result, \
    owner_run, started_ns, updated_ns, ended_ns";

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
    })
}

fn poke(db: Option<&Path>) {
    if let Some(p) = db {
        crate::tracing::store::poke_wake(p);
    }
}

/// Starts a swarm in `planning`. Refuses when the workspace has a live one.
pub fn create_swarm(
    conn: &Connection,
    db: Option<&Path>,
    new: &NewSwarm,
    now: i64,
) -> Result<SwarmRun, String> {
    let workspace = normalize_workspace(Path::new(&new.workspace));
    if let Some(live) = live_swarm(conn, &workspace).map_err(|e| e.to_string())? {
        return Err(format!(
            "this workspace already runs a swarm ({}, {}): stop it first",
            live.id,
            live.status.as_str()
        ));
    }
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
    poke(db);
    get_swarm(conn, &id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the swarm disappeared".into())
}

pub fn get_swarm(conn: &Connection, id: &str) -> rusqlite::Result<Option<SwarmRun>> {
    conn.query_row(
        &format!("SELECT {SWARM_COLS} FROM swarms WHERE id = ?1"),
        params![id],
        row_to_swarm,
    )
    .optional()
}

/// `workspace`'s live swarm (normalized path), if any.
pub fn live_swarm(conn: &Connection, workspace: &str) -> rusqlite::Result<Option<SwarmRun>> {
    conn.query_row(
        &format!("SELECT {SWARM_COLS} FROM swarms WHERE workspace = ?1 AND ended_ns IS NULL"),
        params![workspace],
        row_to_swarm,
    )
    .optional()
}

/// Swarms that ended at or after `since_ns`, newest first.
pub fn ended_swarms_since(conn: &Connection, since_ns: i64) -> rusqlite::Result<Vec<SwarmRun>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SWARM_COLS} FROM swarms WHERE ended_ns IS NOT NULL AND ended_ns >= ?1
         ORDER BY ended_ns DESC LIMIT 20"
    ))?;
    let rows = stmt.query_map(params![since_ns], row_to_swarm)?;
    rows.collect()
}

/// Every live swarm, oldest first.
pub fn live_swarms(conn: &Connection) -> rusqlite::Result<Vec<SwarmRun>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {SWARM_COLS} FROM swarms WHERE ended_ns IS NULL ORDER BY started_ns ASC"
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
    })
}

/// The swarm's board in plan order, each task with its extension and review.
pub fn board(conn: &Connection, swarm_id: &str) -> rusqlite::Result<Vec<BoardTask>> {
    let mut stmt = conn.prepare(
        "SELECT st.task_id, st.plan_key, st.position, st.depends_on_json, st.acceptance,
                st.proposed, st.attempts, st.nudged_ns, r.verdict
         FROM swarm_tasks st LEFT JOIN swarm_reviews r ON r.task_id = st.task_id
         WHERE st.swarm_id = ?1 ORDER BY st.position ASC, st.task_id ASC",
    )?;
    let rows: Vec<(SwarmTaskExt, Option<String>)> = stmt
        .query_map(params![swarm_id], |r| Ok((row_to_ext(r)?, r.get(8)?)))?
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
                .unwrap_or_else(|| swarm.planner_harness.clone()),
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
    Ok(task.id)
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

/// A key for a task a worker proposes: its title as a slug, unique on the
/// board.
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
    let (cancelled, released) = bstore::stop_swarm(conn, db, &swarm.workspace, now)?;
    conn.execute(
        "UPDATE swarms SET status = 'finished', ended_ns = ?1, updated_ns = ?1 WHERE id = ?2",
        params![now, swarm.id],
    )
    .map_err(|e| e.to_string())?;
    poke(db);
    Ok((cancelled, released))
}

/// An operator decision on a swarm (`ControlRequest::SwarmAction`, the
/// CLI's verbs, the screen's keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SwarmVerb {
    /// Approve the plan: `review` → `running`.
    Approve,
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
}

impl SwarmVerb {
    pub fn parse(s: &str) -> Option<SwarmVerb> {
        Some(match s {
            "approve" => SwarmVerb::Approve,
            "replan" | "re-plan" => SwarmVerb::Replan,
            "pause" => SwarmVerb::Pause,
            "resume" => SwarmVerb::Resume,
            "retry" => SwarmVerb::Retry,
            "merge" => SwarmVerb::Merge,
            "set-aside" | "setaside" => SwarmVerb::SetAside,
            "accept" => SwarmVerb::Accept,
            "drop" => SwarmVerb::Drop,
            "close" | "stop" => SwarmVerb::Close,
            _ => return None,
        })
    }
    /// Whether the verb names a task.
    pub fn takes_task(self) -> bool {
        matches!(
            self,
            SwarmVerb::Retry
                | SwarmVerb::Merge
                | SwarmVerb::SetAside
                | SwarmVerb::Accept
                | SwarmVerb::Drop
        )
    }
    /// Whether only the running TUI can carry it out (it starts or stops
    /// sessions).
    pub fn needs_tui(self) -> bool {
        matches!(self, SwarmVerb::Replan | SwarmVerb::Close)
    }
}

/// The board as a plan again, for re-checking it before approval.
pub fn board_plan(swarm: &SwarmRun, board: &[BoardTask]) -> Plan {
    Plan {
        summary: swarm.plan_summary.clone().unwrap_or_default(),
        tasks: board
            .iter()
            .filter(|t| t.task.status != TaskStatus::Cancelled)
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
            })
            .collect(),
    }
}

/// Carries out a verb that only changes rows (and, for `merge`, runs git in
/// the workspace). `env` re-checks the plan on approval. Answers what was
/// done, for the operator.
pub fn apply(
    conn: &Connection,
    db: Option<&Path>,
    swarm: &SwarmRun,
    verb: SwarmVerb,
    task: Option<&str>,
    env: &super::plan::CheckEnv,
    now: i64,
) -> Result<String, String> {
    if verb.needs_tui() {
        return Err(format!("{verb:?} needs the running agent-mux TUI"));
    }
    let board = board(conn, &swarm.id).map_err(|e| e.to_string())?;
    let target = match (verb.takes_task(), task) {
        (true, Some(t)) => Some(find_task(&board, t)?),
        (true, None) => return Err("name the task (its key or id)".into()),
        (false, _) => None,
    };
    let status = swarm.status;
    match verb {
        SwarmVerb::Approve => {
            if status != SwarmStatus::Review {
                return Err(format!(
                    "there is no plan to approve: the swarm is {}",
                    status.as_str()
                ));
            }
            let problems = super::plan::check(&board_plan(swarm, &board), env);
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
            set_status(conn, db, &swarm.id, SwarmStatus::Running, now)
                .map_err(|e| e.to_string())?;
            Ok(format!(
                "plan approved: {} task(s); workers start now",
                board.len()
            ))
        }
        SwarmVerb::Pause => {
            if status != SwarmStatus::Running {
                return Err(format!(
                    "only a running swarm pauses (it is {})",
                    status.as_str()
                ));
            }
            set_status(conn, db, &swarm.id, SwarmStatus::Paused, now).map_err(|e| e.to_string())?;
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
            Ok("resumed".into())
        }
        SwarmVerb::Retry => {
            let t = target.expect("takes a task");
            bstore::reopen_task(conn, db, &t.task.id, now)?;
            clear_check(conn, &swarm.id).map_err(|e| e.to_string())?;
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
            Ok(match detail {
                Some(c) => format!("merged {branch} ({c})"),
                None => format!("set {branch} aside"),
            })
        }
        SwarmVerb::Accept | SwarmVerb::Drop => {
            let t = target.expect("takes a task");
            if !t.ext.proposed {
                return Err(format!("`{}` is not a proposal", t.ext.plan_key));
            }
            if verb == SwarmVerb::Accept {
                accept_proposal(conn, db, &t.task.id).map_err(|e| e.to_string())?;
                Ok(format!("accepted `{}`", t.ext.plan_key))
            } else {
                delete_task(conn, &t.task.id).map_err(|e| e.to_string())?;
                poke(db);
                Ok(format!("dropped `{}`", t.ext.plan_key))
            }
        }
        SwarmVerb::Replan | SwarmVerb::Close => unreachable!("needs the TUI"),
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
    fn one_live_swarm_per_workspace_and_a_board_in_plan_order() {
        let (dir, conn) = store();
        let ws = dir.path().to_string_lossy().into_owned();
        let s = create_swarm(&conn, None, &new_swarm(&ws), 1).unwrap();
        assert_eq!(s.status, SwarmStatus::Planning);
        assert_eq!(s.check_cmd, None, "a blank check is none");
        assert!(
            create_swarm(&conn, None, &new_swarm(&ws), 2)
                .unwrap_err()
                .contains("already runs a swarm")
        );

        let plan = Plan {
            summary: "two".into(),
            tasks: vec![pt("b", &[]), pt("a", &["b"])],
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

        let (cancelled, _) = end_swarm(&conn, None, &s, 5).unwrap();
        assert_eq!(cancelled, 4);
        assert!(live_swarm(&conn, &s.workspace).unwrap().is_none());
        create_swarm(&conn, None, &new_swarm(&ws), 6).expect("a new swarm after the end");
    }
}
