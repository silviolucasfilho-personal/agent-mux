//! Smoke test for the Library area; its own task fills the rest.

use super::support::{get, seeded_store, start};

#[test]
fn the_library_area_serves_the_shell() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);
    let (status, _, body) = get(&base, "/");
    assert_eq!(status, 200);
    assert!(body.contains("/static/library.js"));
    let (status, _, script) = get(&base, "/static/library.js");
    assert_eq!(status, 200);
    assert!(
        script.contains("'Library'"),
        "the Library page is registered"
    );
    server.abort();
}
