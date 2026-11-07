//! `/api/runs/`: loop runs and workflow runs (`web/runs.js`).
//!
//! Read-only JSON over `loop_runs`, `workflow_runs` and `workflow_steps`
//! (`loops::store`, `workflows::store`), opened with
//! `tracing::store::open_ro` per request. Nothing here writes a row or
//! decides an inbox item; the inbox is the same set the TUI's `I` shows
//! (`app::inbox`), minus what that screen keeps in memory only (planner
//! documents and dismissals).
//!
//! | Route | Answer |
//! |---|---|
//! | `GET /api/runs/loops?limit=N` | `{ok, runs: [LoopRun…]}`, newest scheduled first |
//! | `GET /api/runs/loops/{id}` | `{ok, run, launch, facts, files_touched}` |
//! | `GET /api/runs/workflows?limit=N` | `{ok, runs: [WorkflowRun…]}`, newest first |
//! | `GET /api/runs/workflows/{id}` | `{ok, run (with `document`), steps: [WorkflowStep…]}`; `id` may be a unique prefix |
//! | `GET /api/runs/inbox` | `{ok, loops: [LoopRun…], workflows: [WorkflowRun…]}` |

use super::{ApiError, ApiResult, WebState};
use crate::loops::store::{self as lstore, LoopRun, RunFacts};
use crate::workflows::store::{self as wstore, WorkflowRun, WorkflowStep};
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::routing::get;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Runs listed when `limit` is absent.
pub const DEFAULT_LIMIT: usize = 100;
/// The largest `limit` a list answers.
pub const MAX_LIMIT: usize = 1_000;

/// The runs routes, under `/api/runs/`.
pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
        .route("/api/runs/loops", get(list_loops))
        .route("/api/runs/loops/{id}", get(get_loop))
        .route("/api/runs/workflows", get(list_workflows))
        .route("/api/runs/workflows/{id}", get(get_workflow))
        .route("/api/runs/inbox", get(inbox))
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    limit: Option<usize>,
}

impl ListQuery {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }
}

fn open(state: &WebState) -> Result<Connection, ApiError> {
    crate::tracing::store::open_ro(&state.db)
        .map_err(|e| ApiError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, e))
}

fn db_err(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}

async fn list_loops(
    State(state): State<Arc<WebState>>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Value> {
    let conn = open(&state)?;
    let runs = lstore::all_recent_runs(&conn, q.limit()).map_err(db_err)?;
    let runs: Vec<Value> = runs.iter().map(loop_run_json).collect();
    Ok(axum::Json(json!({ "ok": true, "runs": runs })))
}

async fn get_loop(State(state): State<Arc<WebState>>, Path(id): Path<String>) -> ApiResult<Value> {
    let conn = open(&state)?;
    let run = lstore::get_run(&conn, &id)
        .map_err(db_err)?
        .ok_or_else(|| ApiError::not_found(format!("no loop run {id}")))?;
    let mut doc = json!({ "ok": true, "run": loop_run_json(&run) });
    let (launch, facts, files) = match run.launch_id.as_deref() {
        Some(launch_id) => {
            let launch = launch_json(&conn, launch_id).map_err(db_err)?;
            let facts = lstore::run_facts(&conn, launch_id).map_err(db_err)?;
            let files = facts.files_touched.clone();
            (launch, Some(facts_json(&facts)), files)
        }
        None => (Value::Null, None, Vec::new()),
    };
    doc["launch"] = launch;
    doc["facts"] = facts.unwrap_or(Value::Null);
    doc["files_touched"] = json!(files);
    Ok(axum::Json(doc))
}

async fn list_workflows(
    State(state): State<Arc<WebState>>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Value> {
    let conn = open(&state)?;
    let runs = wstore::recent_runs(&conn, None, q.limit()).map_err(db_err)?;
    let runs: Vec<Value> = runs.iter().map(|r| workflow_run_json(r, false)).collect();
    Ok(axum::Json(json!({ "ok": true, "runs": runs })))
}

async fn get_workflow(
    State(state): State<Arc<WebState>>,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    let conn = open(&state)?;
    let run = wstore::resolve_run(&conn, &id)
        .map_err(db_err)?
        .ok_or_else(|| ApiError::not_found(format!("no workflow run {id}")))?;
    let steps = wstore::steps_of(&conn, &run.id).map_err(db_err)?;
    let mut steps_json = Vec::with_capacity(steps.len());
    for step in &steps {
        let mut v = step_json(step);
        v["session_key"] = match step.launch_id.as_deref() {
            Some(l) => launch_session_key(&conn, l).map_err(db_err)?.into(),
            None => Value::Null,
        };
        steps_json.push(v);
    }
    Ok(axum::Json(json!({
        "ok": true,
        "run": workflow_run_json(&run, true),
        "steps": steps_json,
    })))
}

async fn inbox(State(state): State<Arc<WebState>>) -> ApiResult<Value> {
    let conn = open(&state)?;
    let loops = lstore::inbox(&conn).map_err(db_err)?;
    let loops: Vec<Value> = loops.iter().map(loop_run_json).collect();
    let workflows = wstore::recent_runs(&conn, None, MAX_LIMIT).map_err(db_err)?;
    let workflows: Vec<Value> = workflows
        .iter()
        .filter(|r| workflow_needs_human(r))
        .map(|r| workflow_run_json(r, false))
        .collect();
    Ok(axum::Json(json!({
        "ok": true,
        "loops": loops,
        "workflows": workflows,
    })))
}

// ---- shapes ----------------------------------------------------------------

/// A `loop_runs` row as JSON (`LoopRun` has no serde derive).
pub fn loop_run_json(run: &LoopRun) -> Value {
    json!({
        "id": run.id,
        "loop_id": run.loop_id,
        "workspace": run.workspace,
        "pattern": run.pattern,
        "harness": run.harness,
        "level": run.level.as_str(),
        "effective_level": run.effective_level.as_str(),
        "launch_id": run.launch_id,
        "scheduled_ns": run.scheduled_ns,
        "started_ns": run.started_ns,
        "ended_ns": run.ended_ns,
        "outcome": run.outcome.as_str(),
        "outcome_word": run.outcome.word(),
        "items_found": run.items_found,
        "actions_taken": run.actions_taken,
        "escalations": run.escalations,
        "tokens": run.tokens,
        "cost_usd": run.cost_usd,
        "readiness_score": run.readiness_score,
        "worktree": run.worktree,
        "branch": run.branch,
        "decision": run.decision,
        "decided_ns": run.decided_ns,
        "detail": run.detail,
        "pattern_hash": run.pattern_hash,
        "duration_s": run.duration_s(),
        "in_inbox": run.in_inbox(),
    })
}

/// The facts of a run's launch (`loops::store::run_facts`).
pub fn facts_json(facts: &RunFacts) -> Value {
    json!({
        "tokens": facts.tokens,
        "cost_usd": facts.cost_usd,
        "verifier_ran": facts.verifier_ran,
        "verifier_verdict": facts.verifier_verdict,
        "verifier_verdicts": facts.verifier_verdicts,
        "files_touched": facts.files_touched,
        "final_message": facts.final_message,
    })
}

/// A `workflow_runs` row as JSON; `with_document` adds the document text
/// (the list leaves it out).
pub fn workflow_run_json(run: &WorkflowRun, with_document: bool) -> Value {
    let mut v = json!({
        "id": run.id,
        "workflow": run.workflow,
        "source": run.source,
        "document_hash": run.document_hash,
        "workspace": run.workspace,
        "harness": run.harness,
        "profile": run.profile,
        "args": run.args,
        "budget_tokens": run.budget_tokens,
        "started_ns": run.started_ns,
        "ended_ns": run.ended_ns,
        "status": run.status,
        "sessions": run.sessions,
        "tokens": run.tokens,
        "cost_usd": run.cost_usd,
        "result": run.result,
        "result_text": result_text(&run.result),
        "error": run.error,
        "resumed_from": run.resumed_from,
        "duration_s": run.ended_ns.map(|e| (e - run.started_ns).max(0) / 1_000_000_000),
        "awaiting": awaiting_verdict(run),
        "needs_human": workflow_needs_human(run),
    });
    if with_document {
        v["document"] = Value::String(run.document.clone());
    }
    v
}

/// A `workflow_steps` row as JSON.
pub fn step_json(step: &WorkflowStep) -> Value {
    json!({
        "run_id": step.run_id,
        "session": step.session,
        "step_id": step.step_id,
        "item": step.item,
        "launch_id": step.launch_id,
        "phase": step.phase,
        "harness": step.harness,
        "kind": step.kind,
        "started_ns": step.started_ns,
        "ended_ns": step.ended_ns,
        "tokens": step.tokens,
        "cost_usd": step.cost_usd,
        "worktree": step.worktree,
        "changed_files": step.changed_files,
        "result": step.result,
        "result_text": result_text(&step.result),
        "duration_s": match (step.started_ns, step.ended_ns) {
            (Some(s), Some(e)) => Some((e - s).max(0) / 1_000_000_000),
            _ => None,
        },
    })
}

/// A result as text: a string as is, nothing for null, else pretty JSON.
fn result_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => serde_json::to_string_pretty(other).ok(),
    }
}

/// The TUI inbox's rule for a workflow run (`RecentWorkflowRun::needs_human`):
/// it did not finish cleanly, or it finished with a verdict its document
/// does not accept. A run still running is not waiting on anyone.
pub fn workflow_needs_human(run: &WorkflowRun) -> bool {
    if run.status == "running" {
        return false;
    }
    run.status != "finished" || run.error.is_some() || awaiting_verdict(run).is_some()
}

/// The verdict a finished run waits on, when its document names the
/// verdicts it `accept`s and the answer's is not one of them (the rule of
/// `app::workflows::awaiting_verdict`, read from the stored document).
fn awaiting_verdict(run: &WorkflowRun) -> Option<String> {
    if run.status != "finished" {
        return None;
    }
    let accept = crate::workflows::document::parse(&run.document)
        .ok()
        .map(|w| w.accept)
        .unwrap_or_default();
    if accept.is_empty() {
        return None;
    }
    let Some(v) = (match &run.result {
        Value::String(text) => crate::workflows::report::verdict_line(text),
        _ => None,
    }) else {
        return Some("no verdict".into());
    };
    let accepted = accept.iter().any(|a| a.eq_ignore_ascii_case(&v.value))
        && v.blocking.unwrap_or(0) == 0
        && !v.human_required;
    (!accepted).then_some(v.value)
}

fn launch_session_key(conn: &Connection, launch_id: &str) -> rusqlite::Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT session_key FROM launches WHERE id = ?1",
            params![launch_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// The launch of a loop run: `{id, session_key, provider, started_ns,
/// ended_ns}`, or null when the store has no such launch.
fn launch_json(conn: &Connection, launch_id: &str) -> rusqlite::Result<Value> {
    let row = conn
        .query_row(
            "SELECT session_key, provider, started_ns, ended_ns FROM launches WHERE id = ?1",
            params![launch_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()?;
    Ok(match row {
        Some((session_key, provider, started_ns, ended_ns)) => json!({
            "id": launch_id,
            "session_key": session_key,
            "provider": provider,
            "started_ns": started_ns,
            "ended_ns": ended_ns,
        }),
        None => Value::Null,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wf_run(status: &str, document: &str, result: Value) -> WorkflowRun {
        WorkflowRun {
            id: "r1".into(),
            workflow: "w".into(),
            source: "builtin".into(),
            document_hash: "h".into(),
            document: document.into(),
            workspace: "/ws".into(),
            harness: "claude".into(),
            profile: "Claude Code".into(),
            args: Value::Null,
            budget_tokens: None,
            started_ns: 10,
            ended_ns: Some(20),
            status: status.into(),
            sessions: 1,
            tokens: None,
            cost_usd: None,
            result,
            error: None,
            resumed_from: None,
        }
    }

    #[test]
    fn a_workflow_run_waits_on_a_human_like_the_tui_inbox() {
        assert!(!workflow_needs_human(&wf_run("running", "", Value::Null)));
        assert!(!workflow_needs_human(&wf_run("finished", "", json!("ok"))));
        assert!(workflow_needs_human(&wf_run("failed", "", Value::Null)));
        assert!(workflow_needs_human(&wf_run("cancelled", "", Value::Null)));
        let mut errored = wf_run("finished", "", Value::Null);
        errored.error = Some("boom".into());
        assert!(workflow_needs_human(&errored));
        // a document that accepts APPROVED: anything else waits
        let doc = "[workflow]\nname = \"w\"\ndescription = \"d\"\noutput = \"verdict\"\naccept = [\"APPROVED\"]\n\n[[steps]]\nid = \"s\"\nkind = \"single\"\nphase = \"Judge\"\nskill = \"wf-synthesize\"\n";
        let ok = wf_run("finished", doc, json!("ORACLE_VERDICT: APPROVED\nfine"));
        let needs = wf_run("finished", doc, json!("ORACLE_VERDICT: NEEDS_CHANGES\nno"));
        let none = wf_run("finished", doc, json!("## Report"));
        let parsed = crate::workflows::document::parse(doc).expect("the document parses");
        assert_eq!(parsed.accept, vec!["APPROVED".to_string()]);
        assert_eq!(awaiting_verdict(&ok), None);
        assert_eq!(awaiting_verdict(&needs).as_deref(), Some("NEEDS_CHANGES"));
        assert_eq!(awaiting_verdict(&none).as_deref(), Some("no verdict"));
        assert!(!workflow_needs_human(&ok));
        assert!(workflow_needs_human(&needs));
        // a document that does not parse keeps the status rule
        assert!(!workflow_needs_human(&wf_run(
            "finished",
            "not toml [",
            json!("x")
        )));
    }

    #[test]
    fn json_shapes_carry_the_derived_fields() {
        let mut run = LoopRun::new(
            "2026-09-16T08:00:00Z",
            "L1",
            "/ws",
            "daily-triage",
            "claude",
            crate::loops::Level::L2,
            1_000,
        );
        run.outcome = crate::loops::Outcome::FixProposed;
        run.started_ns = Some(1_000);
        run.ended_ns = Some(1_000 + 7_000_000_000);
        let v = loop_run_json(&run);
        assert_eq!(v["outcome"], "fix-proposed");
        assert_eq!(v["outcome_word"], "fix ready");
        assert_eq!(v["level"], "L2");
        assert_eq!(v["duration_s"], 7);
        assert_eq!(v["in_inbox"], true);

        let wf = wf_run("finished", "", json!({"confirmed": 2}));
        let v = workflow_run_json(&wf, false);
        assert!(v.get("document").is_none());
        assert_eq!(v["duration_s"], 0);
        assert_eq!(v["needs_human"], false);
        assert!(v["result_text"].as_str().unwrap().contains("confirmed"));
        assert_eq!(workflow_run_json(&wf, true)["document"], "");
        assert_eq!(result_text(&json!("text")).as_deref(), Some("text"));
        assert_eq!(result_text(&Value::Null), None);
    }
}
