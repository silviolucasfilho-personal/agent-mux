use agent_mux::tracing::store::{self, latest_change_seq, read_changes};
use rusqlite::Connection;
use tempfile::TempDir;

fn seed_store(_suffix: &str) -> TempDir {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("traces.db");
    let mut opts = store::OpenOptions::default();
    opts.run_id = "test-run".to_string();
    let _ = store::open_rw(&db_path, opts).unwrap();
    temp
}

#[test]
fn rolled_back_changes_do_not_advance_review_sequence() {
    let root = seed_store("");
    let conn = Connection::open(root.path().join("traces.db")).unwrap();
    let before = latest_change_seq(&conn).unwrap();
    conn.execute_batch(
        "BEGIN;
        INSERT INTO sessions(key,provider,session_id,first_seen_ns,last_seen_ns)
        VALUES('claude:rollback','claude','rollback',1,1);
        ROLLBACK;",
    )
    .unwrap();
    assert_eq!(latest_change_seq(&conn).unwrap(), before);
}

#[test]
fn committed_changes_are_journaled_and_readable() {
    let root = seed_store("");
    let conn = Connection::open(root.path().join("traces.db")).unwrap();
    let before = latest_change_seq(&conn).unwrap();

    conn.execute_batch(
        "BEGIN;
        INSERT INTO runs(id, agent_mux_version, started_ns, heartbeat_ns)
        VALUES('r1', '0.1.0', 1000, 1000);
        INSERT INTO sessions(key,provider,session_id,first_seen_ns,last_seen_ns)
        VALUES('claude:sess1','claude','sess1',1000,2000);
        INSERT INTO launches(id, run_id, agent_mux_session, profile, provider, cwd, project_slug, content_mode, correlation_plan, started_ns, session_key, agent_mux_version)
        VALUES('l1', 'r1', 1, 'default', 'claude', '/app', 'test', 'metadata', 'none', 1000, 'claude:sess1', '0.1.0');
        INSERT INTO traces(id,session_key,launch_id,ordinal,name,status,start_ns,end_ns,input,output)
        VALUES('t1','claude:sess1','l1',1,'turn1','closed',1000,2000,'hello','world');
        INSERT INTO observations(id,trace_id,name,type,start_ns,end_ns,level,status_message,input,output)
        VALUES('o1','t1','tool_read','tool',1000,1500,'DEFAULT','ok','read file','contents');
        COMMIT;",
    )
    .unwrap();

    let after = latest_change_seq(&conn).unwrap();
    assert!(after > before);

    let changes = read_changes(&conn, before, 10).unwrap();
    assert_eq!(changes.len(), 4);
    assert_eq!(changes[0].entity_kind, "session");
    assert_eq!(changes[0].entity_id, "claude:sess1");
    assert_eq!(changes[0].operation, "insert");

    assert_eq!(changes[1].entity_kind, "launch");
    assert_eq!(changes[1].entity_id, "l1");
    assert_eq!(changes[1].session_key.as_deref(), Some("claude:sess1"));
    assert_eq!(changes[1].launch_id.as_deref(), Some("l1"));
    assert_eq!(changes[1].operation, "insert");

    assert_eq!(changes[2].entity_kind, "trace");
    assert_eq!(changes[2].entity_id, "t1");
    assert_eq!(changes[2].session_key.as_deref(), Some("claude:sess1"));
    assert_eq!(changes[2].operation, "insert");

    assert_eq!(changes[3].entity_kind, "observation");
    assert_eq!(changes[3].entity_id, "o1");
    assert_eq!(changes[3].session_key.as_deref(), Some("claude:sess1"));
    assert_eq!(changes[3].launch_id.as_deref(), Some("l1"));
    assert_eq!(changes[3].operation, "insert");
}

#[test]
fn update_and_delete_operations_journaled() {
    let root = seed_store("");
    let conn = Connection::open(root.path().join("traces.db")).unwrap();

    conn.execute_batch(
        "INSERT INTO sessions(key,provider,session_id,first_seen_ns,last_seen_ns)
        VALUES('claude:sess2','claude','sess2',1000,2000);",
    )
    .unwrap();
    let after_insert = latest_change_seq(&conn).unwrap();

    // Meaningful update
    conn.execute(
        "UPDATE sessions SET last_seen_ns = 3000 WHERE key = 'claude:sess2'",
        [],
    )
    .unwrap();
    let after_update = latest_change_seq(&conn).unwrap();
    assert_eq!(after_update, after_insert + 1);

    let changes = read_changes(&conn, after_insert, 10).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].entity_kind, "session");
    assert_eq!(changes[0].operation, "update");

    // Delete
    conn.execute("DELETE FROM sessions WHERE key = 'claude:sess2'", [])
        .unwrap();
    let after_delete = latest_change_seq(&conn).unwrap();
    assert_eq!(after_delete, after_update + 1);

    let del_changes = read_changes(&conn, after_update, 10).unwrap();
    assert_eq!(del_changes.len(), 1);
    assert_eq!(del_changes[0].entity_kind, "session");
    assert_eq!(del_changes[0].operation, "delete");
}

#[test]
fn store_uuid_is_persistent() {
    let root = seed_store("");
    let conn = Connection::open(root.path().join("traces.db")).unwrap();
    let uuid1 = store::store_uuid(&conn).unwrap();
    assert!(!uuid1.is_empty());
    let uuid2 = store::store_uuid(&conn).unwrap();
    assert_eq!(uuid1, uuid2);
}
