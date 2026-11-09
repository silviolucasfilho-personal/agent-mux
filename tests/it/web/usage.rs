//! Usage: `/api/usage/` and `web/usage.js`. A slot: the usage feature adds
//! its tests here.

use super::support::{get, seeded_store, start};

#[test]
fn the_usage_slot_is_served() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);
    let (status, ctype, _) = get(&base, "/static/usage.js");
    assert_eq!(status, 200);
    assert!(ctype.starts_with("text/javascript"), "{ctype}");
    server.abort();
}
