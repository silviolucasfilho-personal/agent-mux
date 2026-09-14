use agent_mux::tracing::analysis::cursor::{CursorCodec, CursorPayload};
use agent_mux::tracing::analysis::scope::Scope;
use agent_mux::tracing::analysis::service::{
    HealthArgs, Limits, ListSessionsArgs, Request, SearchArgs, ServiceConfig, ServiceError,
    TraceService,
};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

fn setup_test_db(dir: &Path) -> PathBuf {
    let db_path = dir.join("traces.db");
    let conn = Connection::open(&db_path).unwrap();

    conn.execute_batch(
        "CREATE TABLE traces (
            id TEXT PRIMARY KEY,
            session_key TEXT,
            launch_id TEXT,
            provider TEXT,
            cwd TEXT,
            start_ns INTEGER,
            end_ns INTEGER,
            input TEXT,
            output TEXT,
            total_tokens INTEGER,
            total_cost_usd REAL
        );
        CREATE TABLE sessions (
            key TEXT PRIMARY KEY,
            provider TEXT,
            cwd TEXT,
            first_seen_ns INTEGER,
            last_seen_ns INTEGER
        );
        CREATE TABLE observations (
            id TEXT PRIMARY KEY,
            trace_id TEXT,
            name TEXT,
            type TEXT,
            start_ns INTEGER,
            end_ns INTEGER,
            is_error INTEGER,
            status_message TEXT,
            input TEXT,
            output TEXT,
            total_tokens INTEGER,
            total_cost_usd REAL,
            skill TEXT
        );
        CREATE VIEW session_stats AS
        SELECT
            session_key,
            COUNT(id) AS turn_count,
            SUM(total_tokens) AS total_tokens,
            SUM(total_cost_usd) AS total_cost_usd,
            (SELECT COUNT(o.id) FROM observations o JOIN traces t ON t.id = o.trace_id WHERE t.session_key = traces.session_key AND o.type = 'tool') AS total_tools
        FROM traces
        GROUP BY session_key;
        CREATE VIEW trace_stats AS
        SELECT
            id, session_key, launch_id, total_tokens, total_cost_usd, start_ns, end_ns
        FROM traces;",
    )
    .unwrap();

    for i in 1..=5 {
        conn.execute(
            "INSERT INTO sessions (key, provider, cwd, first_seen_ns, last_seen_ns)
             VALUES (?1, 'claude', '/workspace/app', ?2, ?3)",
            rusqlite::params![
                format!("s-{i}"),
                (i as i64) * 1_000_000_000i64,
                (i as i64 + 1) * 1_000_000_000i64
            ],
        )
        .unwrap();
    }

    db_path
}

#[test]
fn defaults_match_public_limits() {
    let limits = Limits::default();
    assert_eq!(limits.concurrent, 4);
    assert_eq!(limits.queue, 16);
    assert_eq!(limits.response_bytes, 64 * 1024);
    assert_eq!(limits.page_default, 20);
    assert_eq!(limits.page_max, 100);
}

#[test]
fn cursor_encodes_and_decodes_valid_token() {
    let codec = CursorCodec::new([42u8; 32]);
    let payload = CursorPayload {
        schema_version: 1,
        issued_at_ns: 1_000_000_000,
        expires_at_ns: 1_000_000_000 + 600 * 1_000_000_000, // 10 minutes
        filters_hash: "hash-123".to_string(),
        upper_bound_ns: 2_000_000_000,
        sort_key: "item-42".to_string(),
        offset: 20,
    };

    let token = codec.encode(&payload);
    assert!(!token.is_empty());

    let decoded = codec.decode(&token, "hash-123", 1_100_000_000).unwrap();
    assert_eq!(decoded.schema_version, 1);
    assert_eq!(decoded.filters_hash, "hash-123");
    assert_eq!(decoded.sort_key, "item-42");
    assert_eq!(decoded.offset, 20);
}

#[test]
fn cursor_rejects_mismatched_filter_hash() {
    let codec = CursorCodec::new([42u8; 32]);
    let payload = CursorPayload {
        schema_version: 1,
        issued_at_ns: 1_000_000_000,
        expires_at_ns: 1_000_000_000 + 600 * 1_000_000_000,
        filters_hash: "hash-123".to_string(),
        upper_bound_ns: 2_000_000_000,
        sort_key: "item-42".to_string(),
        offset: 20,
    };

    let token = codec.encode(&payload);
    let err = codec.decode(&token, "different-hash", 1_100_000_000);
    assert!(matches!(err, Err(ServiceError::InvalidArgument(_))));
}

#[test]
fn cursor_rejects_expired_token() {
    let codec = CursorCodec::new([42u8; 32]);
    let payload = CursorPayload {
        schema_version: 1,
        issued_at_ns: 1_000_000_000,
        expires_at_ns: 1_000_000_000 + 600 * 1_000_000_000,
        filters_hash: "hash-123".to_string(),
        upper_bound_ns: 2_000_000_000,
        sort_key: "item-42".to_string(),
        offset: 20,
    };

    let token = codec.encode(&payload);
    // Decode at 800s (past 601s expiration)
    let err = codec.decode(&token, "hash-123", 800_000_000_000);
    assert!(matches!(err, Err(ServiceError::CursorExpired(_))));
}

#[test]
fn cursor_rejects_tampered_token() {
    let codec = CursorCodec::new([42u8; 32]);
    let payload = CursorPayload {
        schema_version: 1,
        issued_at_ns: 1_000_000_000,
        expires_at_ns: 1_000_000_000 + 600 * 1_000_000_000,
        filters_hash: "hash-123".to_string(),
        upper_bound_ns: 2_000_000_000,
        sort_key: "item-42".to_string(),
        offset: 20,
    };

    let token = codec.encode(&payload);
    let tampered = format!("{token}extra");
    let err = codec.decode(&tampered, "hash-123", 1_100_000_000);
    assert!(err.is_err());
}

#[test]
fn search_query_ceiling_enforced() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = setup_test_db(temp.path());

    let scope = Scope::workspace(Path::new("/workspace/app")).unwrap();
    let service = TraceService::new(ServiceConfig {
        db_path,
        scope,
        snapshot_dir: None,
        limits: Limits::default(),
        admission_hook: None,
    })
    .unwrap();

    let oversized_query = "x".repeat(4097);
    let res = service.execute(Request::Search(SearchArgs {
        query: oversized_query,
        session_key: None,
        provider: None,
        since: None,
        until: None,
        cursor: None,
        limit: None,
    }));
    assert!(matches!(res, Err(ServiceError::InvalidArgument(_))));
}

#[test]
fn response_size_budget_bounds_large_data() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("traces.db");
    let conn = Connection::open(&db_path).unwrap();

    conn.execute_batch(
        "CREATE TABLE traces (
            id TEXT PRIMARY KEY,
            session_key TEXT,
            launch_id TEXT,
            provider TEXT,
            cwd TEXT,
            start_ns INTEGER,
            end_ns INTEGER,
            input TEXT,
            output TEXT,
            total_tokens INTEGER,
            total_cost_usd REAL
        );
        CREATE TABLE sessions (
            key TEXT PRIMARY KEY,
            provider TEXT,
            cwd TEXT,
            first_seen_ns INTEGER,
            last_seen_ns INTEGER
        );",
    )
    .unwrap();

    // Insert a huge trace with 100 KiB input
    let huge_input = "Large content to test response size bounding. ".repeat(2500);
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, provider, cwd, start_ns, end_ns, input, output, total_tokens, total_cost_usd)
         VALUES ('t-huge', 's-huge', 'l-huge', 'claude', '/workspace/app', 1000000000, 2000000000, ?1, 'output', 100, 0.01)",
        [&huge_input],
    )
    .unwrap();

    let scope = Scope::workspace(Path::new("/workspace/app")).unwrap();
    let service = TraceService::new(ServiceConfig {
        db_path,
        scope,
        snapshot_dir: None,
        limits: Limits::default(),
        admission_hook: None,
    })
    .unwrap();

    let res = service
        .execute(Request::Search(SearchArgs {
            query: "content".into(),
            session_key: None,
            provider: None,
            since: None,
            until: None,
            cursor: None,
            limit: None,
        }))
        .unwrap();

    let json_bytes = serde_json::to_vec(&res).unwrap();
    assert!(
        json_bytes.len() <= 64 * 1024,
        "Serialized response must fit within 64 KiB bound, got {}",
        json_bytes.len()
    );
}

#[test]
fn cursor_pagination_advances_through_pages() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = setup_test_db(temp.path());

    let scope = Scope::workspace(Path::new("/workspace/app")).unwrap();
    let service = TraceService::new(ServiceConfig {
        db_path,
        scope,
        snapshot_dir: None,
        limits: Limits {
            page_default: 2,
            page_max: 10,
            ..Default::default()
        },
        admission_hook: None,
    })
    .unwrap();

    // Request page 1 with limit 2
    let page1 = service
        .execute(Request::ListSessions(ListSessionsArgs {
            since: Some("1970-01-01T00:00:00Z".into()),
            until: Some("2030-01-01T00:00:00Z".into()),
            provider: None,
            runtime_state: None,
            cursor: None,
            limit: Some(2),
        }))
        .unwrap();

    assert!(page1.next_cursor.is_some());
    let sessions_page1 = page1.data["sessions"].as_array().unwrap();
    assert_eq!(sessions_page1.len(), 2);
    let first_key = sessions_page1[0]["session_key"].as_str().unwrap();

    // Request page 2 with cursor from page 1
    let page2 = service
        .execute(Request::ListSessions(ListSessionsArgs {
            since: Some("1970-01-01T00:00:00Z".into()),
            until: Some("2030-01-01T00:00:00Z".into()),
            provider: None,
            runtime_state: None,
            cursor: page1.next_cursor,
            limit: Some(2),
        }))
        .unwrap();

    let sessions_page2 = page2.data["sessions"].as_array().unwrap();
    assert_eq!(sessions_page2.len(), 2);
    let second_key = sessions_page2[0]["session_key"].as_str().unwrap();
    assert_ne!(first_key, second_key, "Page 2 must advance to new items");
}

#[test]
fn admission_queue_saturation_returns_busy() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = setup_test_db(temp.path());

    let (tx_entered, rx_entered) = std::sync::mpsc::channel();
    let (tx_release, rx_release) = std::sync::mpsc::channel();
    let rx_release = Arc::new(std::sync::Mutex::new(rx_release));

    // Limits: 1 concurrent slot, 1 queued slot
    let scope = Scope::workspace(Path::new("/workspace/app")).unwrap();
    let service = Arc::new(
        TraceService::new(ServiceConfig {
            db_path,
            scope,
            snapshot_dir: None,
            limits: Limits {
                concurrent: 1,
                queue: 1,
                ..Default::default()
            },
            admission_hook: Some(Arc::new(move || {
                let _ = tx_entered.send(());
                let _ = rx_release.lock().unwrap().recv();
            })),
        })
        .unwrap(),
    );

    // Thread 1: takes the 1 running slot and blocks inside admission_hook
    let s1 = Arc::clone(&service);
    let h1 = thread::spawn(move || s1.execute(Request::Health(HealthArgs {})));

    // Deterministically wait for Thread 1 to acquire the running slot
    rx_entered.recv().unwrap();
    assert_eq!(service.admission_counts().0, 1);

    // Thread 2: attempts execute, but concurrent is 1, so it queues in waiting slot
    let s2 = Arc::clone(&service);
    let h2 = thread::spawn(move || s2.execute(Request::Health(HealthArgs {})));

    // Deterministically wait until Thread 2 is registered in the waiting queue
    while service.admission_counts().1 < 1 {
        thread::yield_now();
    }
    assert_eq!(service.admission_counts().1, 1);

    // Request 3: running is 1, queue is 1 -> saturation!
    let res3 = service.execute(Request::Health(HealthArgs {}));
    assert!(
        matches!(res3, Err(ServiceError::Busy(_))),
        "Expected Busy when admission queue is full, got: {:?}",
        res3
    );

    // Release Thread 1
    let _ = tx_release.send(());
    let r1 = h1.join().unwrap();
    assert!(r1.is_ok());

    // Release Thread 2 (since thread 2 also hits admission_hook when it acquires the slot)
    let _ = tx_release.send(());
    let r2 = h2.join().unwrap();
    assert!(r2.is_ok());
}
