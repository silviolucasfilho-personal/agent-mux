//! `/api/swarm/`: swarms and the inter-agent bus (`web/swarm.js`).
//!
//! Filled by its own task; the prefix is `/api/swarm/`.

use super::WebState;
use axum::Router;
use std::sync::Arc;

/// The swarm routes, under `/api/swarm/`.
pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
}
