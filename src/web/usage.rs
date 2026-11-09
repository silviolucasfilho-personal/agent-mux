//! Usage: token, cost and activity aggregates over the trace store
//! (`/api/usage/`, `web/usage.js`). An empty slot until the usage feature
//! lands; `router` already merges it.

use super::WebState;
use axum::Router;
use std::sync::Arc;

/// The usage routes (none yet).
pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
}
