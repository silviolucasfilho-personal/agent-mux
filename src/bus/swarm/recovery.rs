//! Automatic recovery of a swarm's tasks (schema v26): a task whose worker
//! failed, or whose worker went away before it finished, is started again
//! once without waiting for the operator, and gives up after that with a
//! note naming the tasks it blocks. Independent tasks keep flowing all the
//! while; nothing here cancels or skips a task the operator did not decide on.
//!
//! The engine decides (`engine::next_actions`: `MarkLost`, `ClearLost`,
//! `Recover`, `GiveUp`); this module holds the bookkeeping
//! (`swarm_task_recovery`), the harness a retry runs on and the words the
//! next worker and the coordinator read. A worker the operator stopped
//! (`x` on its session, the retry form) is held: it waits for them.

use super::record::{self, BoardTask, SwarmRun};
use crate::bus::TaskStatus;
use crate::bus::store as bstore;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// How long a task's worker may be gone (exited, or not among the live
/// sessions) before the task is started again: a restart of agent-mux
/// reconnects its workers well within it.
pub const LOST_GRACE_NS: i64 = 60 * 1_000_000_000;

/// Automatic retries per task, until the operator retries it themselves.
pub const MAX_AUTO_RETRIES: i64 = 1;

/// A task's recovery bookkeeping (`swarm_task_recovery`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovery {
    /// When a pass first saw the task's worker gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lost_ns: Option<i64>,
    /// Automatic retries so far (reset by the operator's `retry`).
    #[serde(default)]
    pub retries: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retried_ns: Option<i64>,
    /// Why the last automatic retry happened, for the next worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The operator stopped its worker: no automatic retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_ns: Option<i64>,
}

/// Why a task is started again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cause {
    /// Its worker failed it, or no worker could start.
    Failed,
    /// Its worker went away before finishing.
    Lost,
}

impl Cause {
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Failed => "failed",
            Cause::Lost => "lost",
        }
    }
}

/// Whether an automatic retry is still allowed for `t`.
pub fn may_retry(t: &BoardTask) -> bool {
    !t.ext.proposed && t.ext.recovery.held_ns.is_none() && t.ext.recovery.retries < MAX_AUTO_RETRIES
}

/// The harness a retried task runs on. A lost worker goes again on its own
/// harness (the session died, not the model's approach); a failed one on
/// another harness of the pool, never agy for a task with an agent (agy
/// runs a custom agent read-only), else on its own again.
pub fn retry_harness(t: &BoardTask, cause: Cause, pool: &[String]) -> Option<String> {
    let current = t
        .task
        .target_harness
        .clone()
        .or_else(|| pool.first().cloned());
    if cause == Cause::Lost {
        return current;
    }
    let fits = |h: &&String| {
        Some(h.as_str()) != current.as_deref()
            && !(h.as_str() == "agy" && t.task.target_agent.is_some())
    };
    pool.iter()
        .filter(fits)
        .find(|h| h.as_str() != "agy")
        .or_else(|| pool.iter().find(fits))
        .cloned()
        .or(current)
}

/// The plan keys of the open tasks that wait on `t`, directly or through
/// other waiting tasks.
pub fn blocked_by<'a>(t: &BoardTask, board: &'a [BoardTask]) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    let mut frontier = vec![t.ext.plan_key.as_str()];
    while let Some(k) = frontier.pop() {
        for o in board {
            if !o.task.status.is_terminal()
                && o.ext.depends_on.iter().any(|d| d == k)
                && !out.contains(&o.ext.plan_key.as_str())
            {
                out.push(o.ext.plan_key.as_str());
                frontier.push(o.ext.plan_key.as_str());
            }
        }
    }
    out
}

/// What the next worker of a retried task reads first.
pub fn reason_text(t: &BoardTask, cause: Cause, from: Option<&str>) -> String {
    let on = from.map(|h| format!(" (on {h})")).unwrap_or_default();
    match cause {
        Cause::Lost => format!(
            "The previous worker of this task{on} stopped before it finished. Whatever it \
             committed or left in this folder is still here: look at `git status` and `git log` \
             first and continue from there."
        ),
        Cause::Failed => format!(
            "The previous worker of this task{on} failed it: {}. Try another approach; if the \
             task cannot be done as written, fail it again with the reason.",
            first_line(t.task.error.as_deref().unwrap_or("no reason given"))
        ),
    }
}

/// The coordinator's line for a task that failed for good: the automatic
/// retry is spent, and what it blocks.
pub fn gave_up_note(t: &BoardTask, board: &[BoardTask]) -> Option<String> {
    if t.task.status != TaskStatus::Failed || t.ext.recovery.retries == 0 {
        return None;
    }
    let blocked = blocked_by(t, board);
    Some(format!(
        "it failed again after {} automatic retr{}{}; ask the operator: retry it (perhaps changed), cancel it, or re-plan",
        t.ext.recovery.retries,
        if t.ext.recovery.retries == 1 {
            "y"
        } else {
            "ies"
        },
        if blocked.is_empty() {
            String::new()
        } else {
            format!(", it blocks {}", blocked.join(", "))
        }
    ))
}

fn first_line(s: &str) -> String {
    let l = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out: String = l.chars().take(200).collect();
    if l.chars().count() > 200 {
        out.push('…');
    }
    out
}

fn touch(conn: &Connection, task_id: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO swarm_task_recovery (task_id, updated_ns) VALUES (?1, ?2)",
        params![task_id, now],
    )?;
    Ok(())
}

/// A pass saw the task's worker gone; the first sighting is kept.
pub fn mark_lost(conn: &Connection, task_id: &str, now: i64) -> rusqlite::Result<()> {
    touch(conn, task_id, now)?;
    conn.execute(
        "UPDATE swarm_task_recovery SET lost_ns = COALESCE(lost_ns, ?1), updated_ns = ?1
         WHERE task_id = ?2",
        params![now, task_id],
    )?;
    Ok(())
}

/// Its worker is back (reconnected): forget the sighting.
pub fn clear_lost(conn: &Connection, task_id: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE swarm_task_recovery SET lost_ns = NULL, updated_ns = ?1 WHERE task_id = ?2",
        params![now, task_id],
    )?;
    Ok(())
}

/// The operator stopped the task's worker: it waits for them.
pub fn hold(conn: &Connection, task_id: &str, now: i64) -> rusqlite::Result<()> {
    touch(conn, task_id, now)?;
    conn.execute(
        "UPDATE swarm_task_recovery SET held_ns = ?1, updated_ns = ?1 WHERE task_id = ?2",
        params![now, task_id],
    )?;
    Ok(())
}

/// The operator retried the task: a fresh start, with its own retry again.
pub fn reset(conn: &Connection, task_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM swarm_task_recovery WHERE task_id = ?1",
        params![task_id],
    )?;
    Ok(())
}

/// What [`retry`] did, for the log and the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retried {
    pub key: String,
    pub cause: Cause,
    pub from: Option<String>,
    pub harness: Option<String>,
}

/// Starts `t` again: back on the board, asked for by the dispatcher (the
/// plan was approved; the engine spawns it within the cap), on
/// [`retry_harness`], with the reason the next worker reads. Its worktree
/// and branch stay (the spawn reuses them).
pub fn retry(
    conn: &Connection,
    db: Option<&Path>,
    swarm: &SwarmRun,
    t: &BoardTask,
    cause: Cause,
    now: i64,
) -> Result<Retried, String> {
    let pool = record::worker_pool(conn, swarm);
    let from = t.task.target_harness.clone();
    let harness = retry_harness(t, cause, &pool);
    let reason = reason_text(t, cause, from.as_deref());
    bstore::reopen_task(conn, db, &t.task.id, now)?;
    record::reset_dispatch(conn, &t.task.id).map_err(|e| e.to_string())?;
    if harness != from {
        conn.execute(
            "UPDATE agent_tasks SET target_harness = ?1, updated_ns = ?2 WHERE id = ?3",
            params![harness, now, t.task.id],
        )
        .map_err(|e| e.to_string())?;
    }
    touch(conn, &t.task.id, now).map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE swarm_task_recovery SET retries = retries + 1, retried_ns = ?1, reason = ?2,
             lost_ns = NULL, updated_ns = ?1
         WHERE task_id = ?3",
        params![now, reason, t.task.id],
    )
    .map_err(|e| e.to_string())?;
    record::request_launch(conn, db, &t.task.id, record::LaunchedBy::Dispatcher, now)
        .map_err(|e| e.to_string())?;
    record::clear_check(conn, &swarm.id).map_err(|e| e.to_string())?;
    let _ = record::event(
        conn,
        &swarm.id,
        Some(&t.task.id),
        "task.retried",
        Some("engine"),
        Some(serde_json::json!({
            "key": t.ext.plan_key,
            "cause": cause.as_str(),
            "error": t.task.error,
            "from": from,
            "harness": harness,
            "retry": t.ext.recovery.retries + 1,
        })),
        now,
    );
    Ok(Retried {
        key: t.ext.plan_key.clone(),
        cause,
        from,
        harness,
    })
}

/// A lost task whose automatic retry is spent: failed, so the board says
/// what happened and the swarm can settle around it.
pub fn give_up(
    conn: &Connection,
    db: Option<&Path>,
    swarm: &SwarmRun,
    t: &BoardTask,
    now: i64,
) -> Result<(), String> {
    let why = "its worker went away again after an automatic retry";
    bstore::update_task(
        conn,
        db,
        &t.task.id,
        &record::creator(&swarm.id),
        TaskStatus::Failed,
        None,
        Some(why.to_string()),
        None,
        None,
        now,
    )?;
    let _ = record::event(
        conn,
        &swarm.id,
        Some(&t.task.id),
        "task.lost",
        Some("engine"),
        Some(serde_json::json!({ "key": t.ext.plan_key, "why": why })),
        now,
    );
    Ok(())
}

/// Reads one task's row (tests and the board query share the shape).
pub fn get(conn: &Connection, task_id: &str) -> rusqlite::Result<Recovery> {
    conn.query_row(
        "SELECT lost_ns, retries, retried_ns, reason, held_ns FROM swarm_task_recovery
         WHERE task_id = ?1",
        params![task_id],
        |r| {
            Ok(Recovery {
                lost_ns: r.get(0)?,
                retries: r.get(1)?,
                retried_ns: r.get(2)?,
                reason: r.get(3)?,
                held_ns: r.get(4)?,
            })
        },
    )
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(Recovery::default()),
        e => Err(e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::TaskIsolation;
    use crate::bus::swarm::plan::{CheckEnv, Plan, PlannedTask};
    use crate::bus::swarm::record::{Decider, LaunchedBy, NewSwarm, SwarmStatus, SwarmVerb};
    use crate::tracing::store::{OpenOptions, open_aux, open_rw};

    fn board_task(key: &str, harness: Option<&str>, agent: Option<&str>) -> BoardTask {
        let mut t: BoardTask = serde_json::from_value(serde_json::json!({
            "task": {
                "id": format!("task-{key}"), "workspace": "/ws", "created_by": "x",
                "title": key, "instructions": "x", "files": [], "isolation": "shared",
                "status": "failed", "created_ns": 0, "updated_ns": 0
            },
            "ext": { "task_id": format!("task-{key}"), "plan_key": key, "position": 0,
                     "depends_on": [], "proposed": false, "attempts": 1 },
            "review": null
        }))
        .unwrap();
        t.task.target_harness = harness.map(str::to_string);
        t.task.target_agent = agent.map(str::to_string);
        t
    }

    #[test]
    fn a_failure_moves_to_another_harness_and_a_lost_worker_stays() {
        let pool: Vec<String> = ["claude", "agy", "codex"].map(String::from).to_vec();
        let t = board_task("a", Some("claude"), None);
        assert_eq!(
            retry_harness(&t, Cause::Failed, &pool).as_deref(),
            Some("codex"),
            "agy last"
        );
        assert_eq!(
            retry_harness(&t, Cause::Lost, &pool).as_deref(),
            Some("claude")
        );
        let only_agy: Vec<String> = ["claude", "agy"].map(String::from).to_vec();
        assert_eq!(
            retry_harness(&t, Cause::Failed, &only_agy).as_deref(),
            Some("agy")
        );
        // agy runs a custom agent read-only: not for a task with one
        let with_agent = board_task("a", Some("claude"), Some("doc-writer"));
        assert_eq!(
            retry_harness(&with_agent, Cause::Failed, &only_agy).as_deref(),
            Some("claude")
        );
        // no harness named: the pool's first is the worker's
        let default = board_task("a", None, None);
        assert_eq!(
            retry_harness(&default, Cause::Failed, &pool).as_deref(),
            Some("codex")
        );
    }

    #[test]
    fn blocked_tasks_are_found_through_the_dependency_chain() {
        let a = board_task("a", None, None);
        let mut b = board_task("b", None, None);
        b.task.status = TaskStatus::Open;
        b.ext.depends_on = vec!["a".into()];
        let mut c = board_task("c", None, None);
        c.task.status = TaskStatus::Open;
        c.ext.depends_on = vec!["b".into()];
        let mut d = board_task("d", None, None);
        d.task.status = TaskStatus::Open;
        let board = [a.clone(), b, c, d];
        assert_eq!(blocked_by(&a, &board), ["b", "c"]);
        let mut spent = a.clone();
        spent.ext.recovery.retries = 1;
        let note = gave_up_note(&spent, &board).unwrap();
        assert!(
            note.contains("after 1 automatic retry, it blocks b, c;"),
            "{note}"
        );
        assert_eq!(gave_up_note(&a, &board), None, "not retried yet");
    }

    #[test]
    fn a_retry_reopens_the_task_for_the_dispatcher_and_the_operator_resets_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("traces.db");
        let _ = open_rw(&db, OpenOptions::default()).unwrap();
        let conn = open_aux(&db).unwrap();
        let n = NewSwarm {
            workspace: dir.path().to_string_lossy().into_owned(),
            goal: "ship it".into(),
            planner_harness: "claude".into(),
            max_workers: 2,
            isolation: TaskIsolation::Shared,
            gate: true,
            check_cmd: None,
            owner_run: None,
            worker_pool: vec!["claude".into(), "codex".into()],
            headless: false,
            review: crate::bus::swarm::review::ReviewSettings::off(),
        };
        let s = record::create_swarm(&conn, None, &n, 1).unwrap();
        let plan = Plan {
            summary: "one".into(),
            tasks: vec![PlannedTask {
                key: "a".into(),
                title: "Task a".into(),
                instructions: "do it".into(),
                harness: Some("claude".into()),
                ..Default::default()
            }],
            review: None,
        };
        let env = CheckEnv {
            harnesses: vec!["claude".into(), "codex".into()],
            ..Default::default()
        };
        record::submit_plan(&conn, None, &s, &plan, &env, 2).unwrap();
        record::set_status(&conn, None, &s.id, SwarmStatus::Running, 3).unwrap();
        let id = record::board(&conn, &s.id).unwrap()[0].task.id.clone();
        conn.execute(
            "UPDATE agent_tasks SET status = 'failed', error = 'tests do not compile', ended_ns = 4 WHERE id = ?1",
            params![id],
        )
        .unwrap();
        let t = record::board(&conn, &s.id).unwrap().remove(0);
        let r = retry(&conn, None, &s, &t, Cause::Failed, 5).unwrap();
        assert_eq!(
            (r.from.as_deref(), r.harness.as_deref()),
            (Some("claude"), Some("codex"))
        );
        let t = record::board(&conn, &s.id).unwrap().remove(0);
        assert_eq!(t.task.status, TaskStatus::Open);
        assert_eq!(t.task.target_harness.as_deref(), Some("codex"));
        assert_eq!(
            t.ext.launched_by,
            Some(LaunchedBy::Dispatcher),
            "spawned without a new offer"
        );
        assert_eq!(t.ext.recovery.retries, 1);
        assert!(!may_retry(&t), "one automatic retry");
        let prompt = crate::bus::swarm::prompt::worker_prompt(&s, &t, std::slice::from_ref(&t));
        assert!(
            prompt.contains("### This task is running again")
                && prompt.contains("(on claude) failed it: tests do not compile"),
            "{prompt}"
        );
        let events = record::recent_events(&conn, &s.id, 10).unwrap();
        assert!(
            events.iter().any(|e| e.kind == "task.retried"),
            "{events:?}"
        );
        // the operator's retry starts afresh, with its automatic retry again
        hold(&conn, &id, 6).unwrap();
        record::apply(
            &conn,
            None,
            &s,
            SwarmVerb::Retry,
            Some("a"),
            &env,
            &Decider::TUI,
            7,
        )
        .unwrap();
        assert_eq!(get(&conn, &id).unwrap(), Recovery::default());
    }
}
