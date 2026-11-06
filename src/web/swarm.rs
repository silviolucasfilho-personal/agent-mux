//! Read-only swarm reports and workspace bus boards.
use super::{ApiError, ApiResult, WebState};
use crate::bus::{store as bus, swarm::record};
use crate::tracing::{store, swarm_report as report};
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::QueryRejection},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// GET is public; POST aliases let the SPA use the shared API client.
pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
        .route("/api/swarm/list", get(list).post(list))
        .route("/api/swarm/{id}", get(detail).post(detail))
        .route("/api/swarm/{id}/tasks", get(tasks).post(tasks))
        .route("/api/bus/peers", get(peers).post(peers))
        .route("/api/bus/messages", get(messages).post(messages))
        .route("/api/bus/tasks", get(bus_tasks).post(bus_tasks))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Filters {
    since: Option<i64>,
    workspace: Option<String>,
    all: Option<u8>,
}
type FiltersQuery = Result<Query<Filters>, QueryRejection>;

fn filters(q: FiltersQuery) -> Result<Filters, ApiError> {
    q.map(|Query(q)| q)
        .map_err(|e| ApiError::bad_request(e.to_string()))
}
fn workspace(q: &Filters) -> Result<String, ApiError> {
    let ws = q
        .workspace
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("workspace is required"))?;
    Ok(bus::normalize_workspace(std::path::Path::new(ws)))
}
fn connection(state: &WebState) -> Result<rusqlite::Connection, ApiError> {
    store::open_ro(&state.db)
        .map_err(|e| ApiError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, e))
}
fn db_error(e: rusqlite::Error) -> ApiError {
    ApiError::internal(e.to_string())
}
fn swarm(conn: &rusqlite::Connection, id: &str) -> Result<record::SwarmRun, ApiError> {
    record::get_swarm(conn, id)
        .map_err(db_error)?
        .ok_or_else(|| ApiError::not_found(format!("swarm '{id}' not found")))
}
async fn list(State(state): State<Arc<WebState>>, q: FiltersQuery) -> ApiResult<Value> {
    let q = filters(q)?;
    let conn = connection(&state)?;
    let now = store::now_ns();
    let swarms = report::swarms_since(&conn, q.workspace.as_deref(), q.since.unwrap_or(0), 500)
        .map_err(db_error)?;
    let mut rows = Vec::new();
    let workers = crate::bus::swarm::view::snapshot_workers();
    for s in swarms {
        let r = report::report(&conn, &s, &workers, now).map_err(db_error)?;
        rows.push(
            json!({"swarm": s, "workers": r.launches.iter().filter(|l| l.role == "worker").count(),
            "cost_usd": r.cost_usd, "done": r.done, "total": r.total}),
        );
    }
    Ok(Json(json!({"swarms": rows})))
}
async fn detail(State(state): State<Arc<WebState>>, Path(id): Path<String>) -> ApiResult<Value> {
    let conn = connection(&state)?;
    let s = swarm(&conn, &id)?;
    let now = store::now_ns();
    let r = report::report(&conn, &s, &crate::bus::swarm::view::snapshot_workers(), now)
        .map_err(db_error)?;
    Ok(Json(json!({"tree_lines": report::tree_lines(&r, now),
        "timeline": report::timeline(&r, now),
        "log": report::log_lines(&r.events, report::LogKind::All, ""), "report": r})))
}
async fn tasks(State(state): State<Arc<WebState>>, Path(id): Path<String>) -> ApiResult<Value> {
    let conn = connection(&state)?;
    swarm(&conn, &id)?;
    Ok(Json(
        json!({"tasks": record::board(&conn, &id).map_err(db_error)?}),
    ))
}
async fn peers(State(state): State<Arc<WebState>>, q: FiltersQuery) -> ApiResult<Value> {
    let q = filters(q)?;
    let ws = workspace(&q)?;
    let conn = connection(&state)?;
    Ok(Json(
        json!({"peers": bus::list_peers(&conn, &ws, None, q.all == Some(1), store::now_ns()).map_err(db_error)?}),
    ))
}
async fn messages(State(state): State<Arc<WebState>>, q: FiltersQuery) -> ApiResult<Value> {
    let ws = workspace(&filters(q)?)?;
    let conn = connection(&state)?;
    Ok(Json(
        json!({"messages": bus::list_workspace_messages(&conn, &ws, 500).map_err(db_error)?}),
    ))
}
async fn bus_tasks(State(state): State<Arc<WebState>>, q: FiltersQuery) -> ApiResult<Value> {
    let ws = workspace(&filters(q)?)?;
    let conn = connection(&state)?;
    Ok(Json(
        json!({"tasks": bus::list_tasks(&conn, &ws, None, None, None).map_err(db_error)?}),
    ))
}
