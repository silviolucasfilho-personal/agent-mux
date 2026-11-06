//! `/api/runs/`: loop runs and workflow runs (`web/runs.js`).
//!
//! Filled by its own task; the prefix is `/api/runs/`.

use super::WebState;
use axum::Router;
use std::sync::Arc;

/// The runs routes, under `/api/runs/`.
pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
}
