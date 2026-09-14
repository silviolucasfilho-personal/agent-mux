use agent_mux::tracing::analysis::live::{
    LiveSnapshot, MAX_SNAPSHOT_BYTES, clean_up_snapshot, is_stale, publish_snapshot, read_snapshots,
};
use agent_mux::tracing::analysis::model::{LiveSession, RuntimeState};
use std::fs;
use std::path::PathBuf;

fn make_dummy_session(run_id: &str, session_id: usize, launch_id: &str) -> LiveSession {
    LiveSession {
        run_id: run_id.to_string(),
        launch_id: launch_id.to_string(),
        session_id,
        session_key: Some(format!("key-{session_id}")),
        provider: Some("claude".to_string()),
        cwd: PathBuf::from("/workspace/app"),
        state: RuntimeState::Working,
        updated_at_ns: 1_000_000_000,
        active_tools: vec!["cargo test".to_string()],
    }
}

#[test]
fn missed_heartbeat_means_stale_not_exited() {
    assert!(!is_stale(1_000_000_000, 6_000_000_000));
    assert!(is_stale(1_000_000_000, 6_000_000_001));
}

#[test]
fn atomic_publish_and_read_roundtrip() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    let snapshot = LiveSnapshot {
        run_id: "run-1".to_string(),
        revision: 1,
        heartbeat_ns: 2_000_000_000,
        sessions: vec![make_dummy_session("run-1", 1, "launch-1")],
    };

    publish_snapshot(root, &snapshot).unwrap();

    let snapshots = read_snapshots(root, 3_000_000_000).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].run_id, "run-1");
    assert_eq!(snapshots[0].revision, 1);
    assert_eq!(snapshots[0].sessions.len(), 1);
    assert_eq!(snapshots[0].sessions[0].launch_id, "launch-1");
}

#[test]
fn clean_exit_removes_current_run_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    let snapshot = LiveSnapshot {
        run_id: "run-1".to_string(),
        revision: 1,
        heartbeat_ns: 2_000_000_000,
        sessions: vec![make_dummy_session("run-1", 1, "launch-1")],
    };

    publish_snapshot(root, &snapshot).unwrap();
    assert_eq!(read_snapshots(root, 3_000_000_000).unwrap().len(), 1);

    clean_up_snapshot(root, "run-1").unwrap();
    assert_eq!(read_snapshots(root, 3_000_000_000).unwrap().len(), 0);
}

#[test]
fn crash_leftovers_and_stale_snapshots_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    let stale_snapshot = LiveSnapshot {
        run_id: "run-crashed".to_string(),
        revision: 10,
        heartbeat_ns: 1_000_000_000,
        sessions: vec![make_dummy_session("run-crashed", 1, "launch-old")],
    };

    publish_snapshot(root, &stale_snapshot).unwrap();

    // Reading at now_ns = 7s (diff = 6s > 5s stale threshold)
    let snapshots = read_snapshots(root, 7_000_000_000).unwrap();
    assert!(
        snapshots.is_empty(),
        "Stale snapshots (> 5s since heartbeat) must be ignored"
    );
}

#[test]
fn two_mux_runs_read_and_resolved() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    let snap1 = LiveSnapshot {
        run_id: "run-alpha".to_string(),
        revision: 3,
        heartbeat_ns: 5_000_000_000,
        sessions: vec![make_dummy_session("run-alpha", 1, "l-1")],
    };
    let snap2 = LiveSnapshot {
        run_id: "run-beta".to_string(),
        revision: 5,
        heartbeat_ns: 5_500_000_000,
        sessions: vec![make_dummy_session("run-beta", 2, "l-2")],
    };

    publish_snapshot(root, &snap1).unwrap();
    publish_snapshot(root, &snap2).unwrap();

    let snapshots = read_snapshots(root, 6_000_000_000).unwrap();
    assert_eq!(snapshots.len(), 2);
    let run_ids: Vec<_> = snapshots.iter().map(|s| s.run_id.as_str()).collect();
    assert!(run_ids.contains(&"run-alpha"));
    assert!(run_ids.contains(&"run-beta"));
}

#[test]
fn oversized_and_malformed_snapshots_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Malformed JSON file
    fs::write(root.join("bad.json"), "{ invalid json").unwrap();

    // Oversized file (> 1 MiB)
    let huge_data = vec![b'a'; MAX_SNAPSHOT_BYTES + 1024];
    fs::write(root.join("huge.json"), huge_data).unwrap();

    let valid_snap = LiveSnapshot {
        run_id: "run-good".to_string(),
        revision: 1,
        heartbeat_ns: 5_000_000_000,
        sessions: vec![make_dummy_session("run-good", 1, "l-good")],
    };
    publish_snapshot(root, &valid_snap).unwrap();

    let snapshots = read_snapshots(root, 5_500_000_000).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].run_id, "run-good");
}

#[test]
fn missing_publisher_directory_returns_empty_without_error() {
    let temp = tempfile::tempdir().unwrap();
    let nonexistent = temp.path().join("does_not_exist");

    let snapshots = read_snapshots(&nonexistent, 1_000_000_000).unwrap();
    assert!(snapshots.is_empty());
}

#[test]
fn symlinks_and_unowned_targets_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    let valid_snap = LiveSnapshot {
        run_id: "run-target".to_string(),
        revision: 1,
        heartbeat_ns: 5_000_000_000,
        sessions: vec![make_dummy_session("run-target", 1, "l-1")],
    };
    let target_file = root.join("target.json");
    fs::write(&target_file, serde_json::to_vec(&valid_snap).unwrap()).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link_file = root.join("link.json");
        symlink(&target_file, &link_file).unwrap();

        let snapshots = read_snapshots(root, 5_500_000_000).unwrap();
        // Symlink should not produce a separate snapshot or be accepted as symlink
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].run_id, "run-target");
    }
}

#[test]
fn briefing_reflects_live_snapshots_and_full_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("traces.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
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
        FROM traces;"
    ).unwrap();

    let snap_dir = temp.path().join("snapshots");
    let scope = agent_mux::tracing::analysis::scope::Scope::workspace(std::path::Path::new(
        "/workspace/app",
    ))
    .unwrap();
    let service = agent_mux::tracing::analysis::service::TraceService::new(
        agent_mux::tracing::analysis::service::ServiceConfig {
            db_path,
            scope,
            snapshot_dir: Some(snap_dir.clone()),
            limits: agent_mux::tracing::analysis::service::Limits::default(),
            admission_hook: None,
        },
    )
    .unwrap();

    // 1. Without live snapshot -> CoverageStatus::Partial
    let res_no_snap = service
        .execute(agent_mux::tracing::analysis::service::Request::Briefing(
            agent_mux::tracing::analysis::service::BriefingArgs::default(),
        ))
        .unwrap();
    assert_eq!(
        res_no_snap.coverage.status,
        agent_mux::tracing::analysis::service::CoverageStatus::Partial
    );
    assert!(
        res_no_snap
            .coverage
            .reasons
            .contains(&"live_snapshot_unavailable".to_string())
    );

    // 2. Publish live snapshot -> CoverageStatus::Full
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    let live_snap = LiveSnapshot {
        run_id: "run-live".to_string(),
        revision: 1,
        heartbeat_ns: now_ns,
        sessions: vec![make_dummy_session("run-live", 1, "l-live")],
    };
    publish_snapshot(&snap_dir, &live_snap).unwrap();

    let res_with_snap = service
        .execute(agent_mux::tracing::analysis::service::Request::Briefing(
            agent_mux::tracing::analysis::service::BriefingArgs::default(),
        ))
        .unwrap();
    assert_eq!(
        res_with_snap.coverage.status,
        agent_mux::tracing::analysis::service::CoverageStatus::Full
    );
    assert!(res_with_snap.coverage.reasons.is_empty());
}
