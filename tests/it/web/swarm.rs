//! Smoke test for the Swarms area; its own task fills the rest.

use super::support::{get, seeded_store, start};

#[test]
fn the_swarm_area_serves_the_shell() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);
    let (status, _, body) = get(&base, "/");
    assert_eq!(status, 200);
    assert!(body.contains("/static/swarm.js"));
    let (status, _, script) = get(&base, "/static/swarm.js");
    assert_eq!(status, 200);
    assert!(script.contains("'Swarms'"), "the Swarms page is registered");
    server.abort();
}
