//! Projects in the web explorer: `/api/project/`.
//!
//! Everything is answered from what this machine last read of a Project:
//! the snapshot `project::store` caches, the fleet's dispatches and the
//! sessions of the trace store. Nothing here calls `gh`, and nothing
//! writes: a hub that was never read is a 404 that says to run
//! `agent-mux project sync`.
//!
//! - `GET /api/project/list`: every cached hub.
//! - `GET /api/project/board?hub=o/r[&all=1]`: its items, stages,
//!   milestones, dispatches and what the sessions of each item add up to.
//! - `GET /api/project/item?hub=o/r&n=12`: one item, what is under it, a
//!   Feature's order, and its sessions as one trace.
//! - `GET /api/project/activity?hub=o/r`: the Observability screen's
//!   lines, for the last 30 days and ever.

use super::{ApiError, ApiResult, WebState};
use crate::project::{
    self, Item, ItemType, Snapshot, cli, contract, fleet, next, order, stage, store as cache,
};
use crate::tracing::{project_report as report, store};
use axum::{
    Json, Router,
    extract::{Query, State, rejection::QueryRejection},
    http::StatusCode,
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// GET is public; POST aliases let the SPA use the shared API client.
pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
        .route("/api/project/list", get(list).post(list))
        .route("/api/project/board", get(board).post(board))
        .route("/api/project/item", get(item).post(item))
        .route("/api/project/activity", get(activity).post(activity))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    hub: Option<String>,
    n: Option<u64>,
    all: Option<u8>,
}
type ParamsQuery = Result<Query<Params>, QueryRejection>;

fn params(q: ParamsQuery) -> Result<Params, ApiError> {
    q.map(|Query(q)| q)
        .map_err(|e| ApiError::bad_request(e.to_string()))
}

fn connection(state: &WebState) -> Result<rusqlite::Connection, ApiError> {
    store::open_ro(&state.db).map_err(|e| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, e))
}

/// The cached snapshot the call names, whatever the case of `hub`.
fn snapshot(conn: &rusqlite::Connection, q: &Params) -> Result<Snapshot, ApiError> {
    let asked = q
        .hub
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| ApiError::bad_request("hub is required, as owner/repo"))?;
    let hub = cache::hubs(conn)
        .into_iter()
        .find(|h| h.eq_ignore_ascii_case(asked))
        .ok_or_else(|| never_read(asked))?;
    cache::load(conn, &hub)
        .map_err(|e| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, e))?
        .ok_or_else(|| never_read(asked))
}

fn never_read(hub: &str) -> ApiError {
    ApiError::not_found(format!(
        "the Project {hub} was never read on this machine: run `agent-mux project sync` in its folder"
    ))
}

/// Runs a read on a blocking thread: the store is SQLite.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
}

/// One row of a list of items.
fn row(i: &Item, s: &Snapshot) -> Value {
    let shown = stage::shown(i);
    let d = next::describe(i, s);
    json!({
        "number": i.number,
        "type": i.item_type.map(ItemType::as_str),
        "title": i.name(),
        "open": i.open,
        "stage": shown.stage,
        "stage_derived": shown.derived,
        "priority": i.priority,
        "milestone": i.milestone,
        "parent": i.parent,
        "assignees": i.assignees,
        "repos": i.repos,
        "updated": i.updated,
        "pulls": i.pulls.iter().map(|p| json!({
            "repo": p.repo, "number": p.number, "url": p.url,
            "state": p.state, "draft": p.draft, "review": p.review,
        })).collect::<Vec<_>>(),
        "next": cli::decision_json(&d),
        "next_line": d.line(),
        "needs_operator": d.needs_operator(),
    })
}

/// The stages a board is shown by: the canonical order, then whatever
/// else an item of the snapshot is at.
fn stage_order(s: &Snapshot) -> Vec<String> {
    let mut out: Vec<String> = contract::STAGE_ORDER
        .iter()
        .map(|x| x.to_string())
        .collect();
    for i in &s.items {
        let at = stage::shown(i).stage;
        if !out.contains(&at) {
            out.push(at);
        }
    }
    out
}

/// Where each of a Feature's documents is, by name.
fn docs(f: &Item) -> serde_json::Map<String, Value> {
    ["spec", "plan", "decomposition", "qa-plan"]
        .iter()
        .map(|d| (d.to_string(), json!(f.doc_state(d).as_str())))
        .collect()
}

fn milestones(s: &Snapshot) -> Vec<Value> {
    s.milestones
        .iter()
        .map(|m| {
            let mut features: Vec<&Item> = s
                .items
                .iter()
                .filter(|i| {
                    i.item_type == Some(ItemType::Feature)
                        && i.milestone.as_deref() == Some(m.title.as_str())
                })
                .collect();
            features.sort_by_key(|i| i.number);
            json!({
                "number": m.number,
                "title": m.title,
                "open": m.open,
                "due": m.due,
                "open_issues": m.open_issues,
                "closed_issues": m.closed_issues,
                "features": features.iter().map(|f| json!({
                    "number": f.number,
                    "title": f.name(),
                    "open": f.open,
                    "stage": stage::shown(f).stage,
                    "docs": docs(f),
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

async fn list(State(state): State<Arc<WebState>>) -> ApiResult<Value> {
    blocking(move || {
        let conn = connection(&state)?;
        let mut projects = Vec::new();
        for hub in cache::hubs(&conn) {
            let Ok(Some(s)) = cache::load(&conn, &hub) else {
                continue;
            };
            projects.push(json!({
                "hub": s.hub,
                "synced_ns": s.synced_ns,
                "default_branch": s.default_branch,
                "root": s.root,
                "open": s.items.iter().filter(|i| i.open).count(),
                "needs_you": crate::app::project::needs_you(&s).len(),
                "milestones": s.milestones.iter().filter(|m| m.open).count(),
                "board_error": s.board_error,
                "warnings": s.warnings.len(),
            }));
        }
        Ok(Json(json!({ "projects": projects })))
    })
    .await
}

async fn board(State(state): State<Arc<WebState>>, q: ParamsQuery) -> ApiResult<Value> {
    let q = params(q)?;
    blocking(move || {
        let conn = connection(&state)?;
        let s = snapshot(&conn, &q)?;
        let now = project::now_ns();
        let all = report::hub_launches(&conn, &s.hub, 0).unwrap_or_default();
        let totals: serde_json::Map<String, Value> = report::item_totals(&all, Some(&s), now)
            .into_iter()
            .map(|(n, t)| (n.to_string(), json!(t)))
            .collect();
        let items: Vec<Value> = s
            .items
            .iter()
            .filter(|i| q.all == Some(1) || i.open)
            .map(|i| row(i, &s))
            .collect();
        Ok(Json(json!({
            "hub": s.hub,
            "synced_ns": s.synced_ns,
            "default_branch": s.default_branch,
            "root": s.root,
            "board_error": s.board_error,
            "warnings": s.warnings,
            "stages": stage_order(&s),
            "items": items,
            "milestones": milestones(&s),
            "fleet": fleet::list(&conn, &s.hub).unwrap_or_default(),
            "totals": totals,
        })))
    })
    .await
}

async fn item(State(state): State<Arc<WebState>>, q: ParamsQuery) -> ApiResult<Value> {
    let q = params(q)?;
    let n =
        q.n.ok_or_else(|| ApiError::bad_request("n is required: the issue's number"))?;
    blocking(move || {
        let conn = connection(&state)?;
        let s = snapshot(&conn, &q)?;
        let it = s.item(n).ok_or_else(|| {
            ApiError::not_found(format!(
                "issue #{n} is not in the Project {} (closed long ago, or not read yet)",
                s.hub
            ))
        })?;
        let now = project::now_ns();
        // the path from the top down to the item's parent
        let mut ancestors = Vec::new();
        let mut at = it.parent;
        while let Some(p) = at.and_then(|p| s.item(p)).filter(|_| ancestors.len() < 16) {
            ancestors.push(json!({
                "number": p.number,
                "type": p.item_type.map(ItemType::as_str),
                "title": p.name(),
            }));
            at = p.parent;
        }
        ancestors.reverse();
        let ordered = matches!(it.item_type, Some(ItemType::Feature | ItemType::Rfc))
            .then(|| order::feature_order(&s, n).ok())
            .flatten();
        let r = report::report(&conn, &s.hub, n, Some(&s))
            .map_err(|e| ApiError::internal(e.to_string()))?;
        let all = report::hub_launches(&conn, &s.hub, 0).unwrap_or_default();
        let totals = report::item_totals(&all, Some(&s), now).remove(&n);
        Ok(Json(json!({
            "hub": s.hub,
            "synced_ns": s.synced_ns,
            "item": cli::item_json(it, &s),
            "title": it.name(),
            "card": cli::card_lines(it, &s),
            "ancestors": ancestors,
            "children": s.children_of(n).into_iter().map(|c| row(c, &s)).collect::<Vec<_>>(),
            "order": ordered.as_ref().map(|o| json!({
                "rows": o.rows,
                "cycle": o.cycle,
                "external": o.external,
                "warnings": o.warnings,
                "lines": cli::order_lines(o),
            })),
            "dispatch": fleet::get(&conn, &s.hub, n).ok().flatten(),
            "totals": totals,
            "sessions_line": totals.as_ref().and_then(report::card_line),
            "runs": report::runs(&r),
            "tree_lines": report::tree_lines(&r, now),
            "report": r,
        })))
    })
    .await
}

async fn activity(State(state): State<Arc<WebState>>, q: ParamsQuery) -> ApiResult<Value> {
    let q = params(q)?;
    blocking(move || {
        let conn = connection(&state)?;
        let s = snapshot(&conn, &q)?;
        let now = project::now_ns();
        let all = report::hub_launches(&conn, &s.hub, 0)
            .map_err(|e| ApiError::internal(e.to_string()))?;
        let month = now - 30 * 86_400 * 1_000_000_000;
        let recent: Vec<report::StepLaunch> = all
            .iter()
            .filter(|l| l.started_ns >= month)
            .cloned()
            .collect();
        Ok(Json(json!({
            "hub": s.hub,
            "sessions": all.len(),
            "recent": report::observability_lines(&recent, Some(&s), "the last 30 days", now),
            "ever": report::observability_lines(&all, Some(&s), "ever", now),
        })))
    })
    .await
}
