//! Tools: tool-call metrics over the trace store (`/api/tools/`,
//! `web/tools.js`). An empty slot until the tools feature lands; `router`
//! already merges it.

use super::WebState;
use axum::Router;
use std::sync::Arc;

/// The tools routes (none yet).
pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
}
