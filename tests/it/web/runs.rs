//! Smoke test for the Runs area; its own task fills the rest.

use super::support::{get, seeded_store, start};

#[test]
fn the_runs_area_serves_the_shell() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);
    let (status, _, body) = get(&base, "/");
    assert_eq!(status, 200);
    assert!(body.contains("/static/runs.js"));
    let (status, _, script) = get(&base, "/static/runs.js");
    assert_eq!(status, 200);
    assert!(script.contains("'Runs'"), "the Runs page is registered");
    server.abort();
}
