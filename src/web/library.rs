//! `/api/library/`: the configuration library (`web/library.js`).
//!
//! Filled by its own task; the prefix is `/api/library/`.

use super::WebState;
use axum::Router;
use std::sync::Arc;

/// The library routes, under `/api/library/`.
pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
}
