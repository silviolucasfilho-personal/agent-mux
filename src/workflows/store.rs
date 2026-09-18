//! `workflow_runs` and `workflow_steps`: one row per run and per session,
//! written by the App after each session and at the end of a run, read
//! by the section, the view, the CLI and the MCP tool.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRun {
    pub id: String,
    pub workflow: String,
    pub source: String,
    pub document_hash: String,
    pub document: String,
    pub workspace: String,
    pub harness: String,
    pub profile: String,
    pub args: Value,
    pub budget_tokens: Option<i64>,
    pub started_ns: i64,
    pub ended_ns: Option<i64>,
    pub status: String,
    pub sessions: i64,
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub result: Value,
    pub error: Option<String>,
    pub resumed_from: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowStep {
    pub run_id: String,
    /// The session label.
    pub session: String,
    pub step_id: String,
    pub item: Value,
    pub launch_id: Option<String>,
    pub phase: String,
    pub harness: String,
    pub kind: String,
    pub started_ns: Option<i64>,
    pub ended_ns: Option<i64>,
    pub tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub worktree: Option<String>,
    pub changed_files: Value,
    pub result: Value,
}

const RUN_COLUMNS: &str = "id, workflow, source, document_hash, document, workspace, harness, profile, args, budget_tokens, started_ns, ended_ns, status, sessions, tokens, cost_usd, result, error, resumed_from";

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowRun> {
    let args: String = row.get(8)?;
    let result: String = row.get(16)?;
    Ok(WorkflowRun {
        id: row.get(0)?,
        workflow: row.get(1)?,
        source: row.get(2)?,
        document_hash: row.get(3)?,
        document: row.get(4)?,
        workspace: row.get(5)?,
        harness: row.get(6)?,
        profile: row.get(7)?,
        args: serde_json::from_str(&args).unwrap_or(Value::Null),
        budget_tokens: row.get(9)?,
        started_ns: row.get(10)?,
        ended_ns: row.get(11)?,
        status: row.get(12)?,
        sessions: row.get(13)?,
        tokens: row.get(14)?,
        cost_usd: row.get(15)?,
        result: serde_json::from_str(&result).unwrap_or(Value::Null),
        error: row.get(17)?,
        resumed_from: row.get(18)?,
    })
}

pub fn upsert_run(conn: &Connection, run: &WorkflowRun) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO workflow_runs ({RUN_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
             ON CONFLICT(id) DO UPDATE SET
               ended_ns = excluded.ended_ns, status = excluded.status, sessions = excluded.sessions,
               tokens = excluded.tokens, cost_usd = excluded.cost_usd, result = excluded.result,
               error = excluded.error"
        ),
        params![
            run.id,
            run.workflow,
            run.source,
            run.document_hash,
            run.document,
            run.workspace,
            run.harness,
            run.profile,
            run.args.to_string(),
            run.budget_tokens,
            run.started_ns,
            run.ended_ns,
            run.status,
            run.sessions,
            run.tokens,
            run.cost_usd,
            run.result.to_string(),
            run.error,
            run.resumed_from,
        ],
    )?;
    Ok(())
}

pub fn get_run(conn: &Connection, id: &str) -> rusqlite::Result<Option<WorkflowRun>> {
    conn.query_row(
        &format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id = ?1"),
        params![id],
        row_to_run,
    )
    .optional()
}

/// A run by id or unique id prefix.
pub fn resolve_run(conn: &Connection, query: &str) -> rusqlite::Result<Option<WorkflowRun>> {
    if let Some(r) = get_run(conn, query)? {
        return Ok(Some(r));
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id LIKE ?1 ORDER BY started_ns DESC"
    ))?;
    let rows: Vec<WorkflowRun> = stmt
        .query_map(params![format!("{query}%")], row_to_run)?
        .collect::<Result<_, _>>()?;
    Ok(if rows.len() == 1 {
        rows.into_iter().next()
    } else {
        None
    })
}

/// Recent runs, newest first; `workflow` narrows to one name.
pub fn recent_runs(
    conn: &Connection,
    workflow: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<WorkflowRun>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM workflow_runs
         WHERE (?1 IS NULL OR workflow = ?1)
         ORDER BY started_ns DESC LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![workflow, limit as i64], row_to_run)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

const STEP_COLUMNS: &str = "run_id, session, step_id, item, launch_id, phase, harness, kind, started_ns, ended_ns, tokens, cost_usd, worktree, changed_files, result";

fn row_to_step(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowStep> {
    let item: String = row.get(3)?;
    let changed: String = row.get(13)?;
    let result: String = row.get(14)?;
    Ok(WorkflowStep {
        run_id: row.get(0)?,
        session: row.get(1)?,
        step_id: row.get(2)?,
        item: serde_json::from_str(&item).unwrap_or(Value::Null),
        launch_id: row.get(4)?,
        phase: row.get(5)?,
        harness: row.get(6)?,
        kind: row.get(7)?,
        started_ns: row.get(8)?,
        ended_ns: row.get(9)?,
        tokens: row.get(10)?,
        cost_usd: row.get(11)?,
        worktree: row.get(12)?,
        changed_files: serde_json::from_str(&changed).unwrap_or(Value::Null),
        result: serde_json::from_str(&result).unwrap_or(Value::Null),
    })
}

pub fn upsert_step(conn: &Connection, step: &WorkflowStep) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO workflow_steps ({STEP_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(run_id, session) DO UPDATE SET
               launch_id = excluded.launch_id, kind = excluded.kind, started_ns = excluded.started_ns,
               ended_ns = excluded.ended_ns, tokens = excluded.tokens, cost_usd = excluded.cost_usd,
               worktree = excluded.worktree, changed_files = excluded.changed_files, result = excluded.result"
        ),
        params![
            step.run_id,
            step.session,
            step.step_id,
            step.item.to_string(),
            step.launch_id,
            step.phase,
            step.harness,
            step.kind,
            step.started_ns,
            step.ended_ns,
            step.tokens,
            step.cost_usd,
            step.worktree,
            step.changed_files.to_string(),
            step.result.to_string(),
        ],
    )?;
    Ok(())
}

pub fn steps_of(conn: &Connection, run_id: &str) -> rusqlite::Result<Vec<WorkflowStep>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {STEP_COLUMNS} FROM workflow_steps WHERE run_id = ?1 ORDER BY started_ns, session"
    ))?;
    let rows = stmt
        .query_map(params![run_id], row_to_step)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Runs started since `since_ns`, for the doctor section.
pub fn runs_since(conn: &Connection, since_ns: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM workflow_runs WHERE started_ns >= ?1",
        params![since_ns],
        |r| r.get(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let _ = crate::tracing::store::open_rw(&db, crate::tracing::store::OpenOptions::default())
            .unwrap();
        let conn = crate::tracing::store::open_aux(&db).unwrap();
        let run = WorkflowRun {
            id: "r1".into(),
            workflow: "review-changes".into(),
            source: "builtin".into(),
            document_hash: "h".into(),
            document: "[workflow]".into(),
            workspace: "/ws".into(),
            harness: "claude".into(),
            profile: "Claude Code".into(),
            args: serde_json::json!({"scope": ""}),
            budget_tokens: Some(1000),
            started_ns: 10,
            ended_ns: None,
            status: "running".into(),
            sessions: 0,
            tokens: None,
            cost_usd: None,
            result: Value::Null,
            error: None,
            resumed_from: None,
        };
        upsert_run(&conn, &run).unwrap();
        let mut done = run.clone();
        done.status = "finished".into();
        done.ended_ns = Some(20);
        done.sessions = 3;
        done.result = serde_json::json!({"confirmed": 2});
        upsert_run(&conn, &done).unwrap();
        assert_eq!(get_run(&conn, "r1").unwrap().unwrap(), done);
        assert_eq!(resolve_run(&conn, "r").unwrap().unwrap().id, "r1");
        assert_eq!(
            recent_runs(&conn, Some("review-changes"), 5).unwrap().len(),
            1
        );
        assert_eq!(recent_runs(&conn, Some("other"), 5).unwrap().len(), 0);
        let step = WorkflowStep {
            run_id: "r1".into(),
            session: "find[1]".into(),
            step_id: "find".into(),
            item: serde_json::json!("security"),
            launch_id: Some("l1".into()),
            phase: "Review".into(),
            harness: "claude".into(),
            kind: "object".into(),
            started_ns: Some(11),
            ended_ns: Some(15),
            tokens: Some(100),
            cost_usd: Some(0.01),
            worktree: None,
            changed_files: Value::Null,
            result: serde_json::json!({"findings": []}),
        };
        upsert_step(&conn, &step).unwrap();
        upsert_step(&conn, &step).unwrap();
        assert_eq!(steps_of(&conn, "r1").unwrap(), vec![step]);
        assert_eq!(runs_since(&conn, 0).unwrap(), 1);
    }
}
