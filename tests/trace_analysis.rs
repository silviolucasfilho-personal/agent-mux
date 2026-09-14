#[path = "support/analysis.rs"]
mod support;

use agent_mux::tracing::analysis::correlation::resolve_binding;
use agent_mux::tracing::analysis::model::*;

#[test]
fn absence_of_exact_identity_stays_uncorrelated() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE launches(id TEXT, run_id TEXT, agent_mux_session INTEGER, session_key TEXT, started_ns INTEGER);
         INSERT INTO launches VALUES('old','previous-run',1,'claude:old',1);",
    )
    .unwrap();
    assert!(resolve_binding(&db, "current-run", 1, None, None).unwrap().is_none());
}

#[test]
fn exact_launch_id_binds_immediately() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE launches(id TEXT, run_id TEXT, agent_mux_session INTEGER, session_key TEXT, started_ns INTEGER);
         INSERT INTO launches VALUES('launch-1','run-1',1,'claude:s1',100);",
    )
    .unwrap();

    let binding = resolve_binding(&db, "run-1", 1, Some("launch-1"), None).unwrap().unwrap();
    assert_eq!(binding.launch_id, "launch-1");
    assert_eq!(binding.session_key.as_deref(), Some("claude:s1"));
}

#[test]
fn native_key_resolves_when_present() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE launches(id TEXT, run_id TEXT, agent_mux_session INTEGER, session_key TEXT, started_ns INTEGER);
         INSERT INTO launches VALUES('launch-1','run-1',1,'claude:s1',100);",
    )
    .unwrap();

    let binding = resolve_binding(&db, "run-1", 1, None, Some("claude:s1")).unwrap().unwrap();
    assert_eq!(binding.launch_id, "launch-1");
    assert_eq!(binding.session_key.as_deref(), Some("claude:s1"));
}

#[test]
fn conflicting_exact_identities_fail_with_correlation_error() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE launches(id TEXT, run_id TEXT, agent_mux_session INTEGER, session_key TEXT, started_ns INTEGER);
         INSERT INTO launches VALUES('launch-1','run-1',1,'claude:s1',100);",
    )
    .unwrap();

    // Contradictory identities: launch-1 has claude:s1, but caller claims claude:different
    let res = resolve_binding(&db, "run-1", 1, Some("launch-1"), Some("claude:different"));
    assert!(matches!(res, Err(AnalysisError::Correlation(_))));
}

#[test]
fn run_id_and_local_id_picks_latest_launch_of_that_run() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE launches(id TEXT, run_id TEXT, agent_mux_session INTEGER, session_key TEXT, started_ns INTEGER);
         INSERT INTO launches VALUES('old-launch','run-1',1,'claude:s1',100);
         INSERT INTO launches VALUES('new-launch','run-1',1,'claude:s2',200);",
    )
    .unwrap();

    let binding = resolve_binding(&db, "run-1", 1, None, None).unwrap().unwrap();
    assert_eq!(binding.launch_id, "new-launch");
    assert_eq!(binding.session_key.as_deref(), Some("claude:s2"));
}

#[test]
fn two_runs_sharing_local_id_do_not_collide() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE launches(id TEXT, run_id TEXT, agent_mux_session INTEGER, session_key TEXT, started_ns INTEGER);
         INSERT INTO launches VALUES('run1-l1','run-1',1,'claude:r1',100);
         INSERT INTO launches VALUES('run2-l1','run-2',1,'claude:r2',200);",
    )
    .unwrap();

    let b1 = resolve_binding(&db, "run-1", 1, None, None).unwrap().unwrap();
    assert_eq!(b1.launch_id, "run1-l1");
    assert_eq!(b1.session_key.as_deref(), Some("claude:r1"));

    let b2 = resolve_binding(&db, "run-2", 1, None, None).unwrap().unwrap();
    assert_eq!(b2.launch_id, "run2-l1");
    assert_eq!(b2.session_key.as_deref(), Some("claude:r2"));
}
