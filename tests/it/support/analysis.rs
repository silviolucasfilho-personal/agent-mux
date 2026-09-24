use rusqlite::Connection;

#[allow(dead_code)]
pub fn setup_test_analysis_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE launches (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL,
            agent_mux_session INTEGER,
            session_key TEXT,
            started_ns INTEGER NOT NULL,
            cwd TEXT,
            provider TEXT
        );
        CREATE TABLE traces (
            id TEXT PRIMARY KEY,
            session_key TEXT,
            run_id TEXT,
            launch_id TEXT,
            first_seen_ns INTEGER,
            last_seen_ns INTEGER
        );
        CREATE TABLE observations (
            id TEXT PRIMARY KEY,
            trace_id TEXT NOT NULL,
            type TEXT NOT NULL,
            name TEXT,
            input TEXT,
            output TEXT,
            start_ns INTEGER NOT NULL,
            end_ns INTEGER
        );
        "#,
    )
    .unwrap();
    conn
}
