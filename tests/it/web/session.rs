//! The session viewer (`#/sessions/:key`). A slot: the session-viewer
//! feature adds its tests here.

use super::support::{post, seeded_store, start};
use serde_json::json;

#[test]
fn the_session_viewer_reads_a_session() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);
    let (status, env) = post(
        &base,
        "/api/trace/get_session",
        &json!({"session_key": "claude:s1"}),
    );
    assert_eq!(status, 200, "{env}");
    server.abort();
}
