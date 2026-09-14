use agent_mux::tracing::analysis::scope::Scope;
use agent_mux::tracing::analysis::service::{
    BriefingArgs, CompareRunsArgs, GetSessionArgs, HealthArgs, Request, SearchArgs, ServiceConfig,
    ServiceError, TraceService,
};
use rusqlite::Connection;
use serde_json::json;
use std::path::{Path, PathBuf};

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

    // Insert data for Workspace A (/workspace/app)
    conn.execute(
        "INSERT INTO sessions (key, provider, cwd, first_seen_ns, last_seen_ns)
         VALUES ('s-app-1', 'claude', '/workspace/app', 1000000000, 2000000000)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, provider, cwd, start_ns, end_ns, input, output, total_tokens, total_cost_usd)
         VALUES ('t-app-1', 's-app-1', 'l-app-1', 'claude', '/workspace/app', 1000000000, 1500000000, 'Fix the build error in app', 'Fixed the build error', 300, 0.05)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, provider, cwd, start_ns, end_ns, input, output, total_tokens, total_cost_usd)
         VALUES ('t-app-2', 's-app-1', 'l-app-1', 'claude', '/workspace/app', 1500000000, 2000000000, 'Run the unit tests', 'Tests passed', 200, 0.05)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observations (id, trace_id, name, type, start_ns, end_ns, is_error, status_message, input, output, total_tokens, total_cost_usd, skill)
         VALUES ('o-app-1', 't-app-1', 'Write', 'tool', 1100000000, 1200000000, 0, NULL, '{\"TargetFile\":\"/workspace/app/src/main.rs\"}', 'ok', 100, 0.02, NULL)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observations (id, trace_id, name, type, start_ns, end_ns, is_error, status_message, input, output, total_tokens, total_cost_usd, skill)
         VALUES ('o-app-2', 't-app-2', 'run_command', 'tool', 1600000000, 1700000000, 0, NULL, '{\"CommandLine\":\"cargo test\"}', 'ok', 50, 0.01, NULL)",
        [],
    )
    .unwrap();

    // Second session in Workspace A
    conn.execute(
        "INSERT INTO sessions (key, provider, cwd, first_seen_ns, last_seen_ns)
         VALUES ('s-app-2', 'codex', '/workspace/app', 3000000000, 4000000000)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, provider, cwd, start_ns, end_ns, input, output, total_tokens, total_cost_usd)
         VALUES ('t-app-3', 's-app-2', 'l-app-2', 'codex', '/workspace/app', 3000000000, 4000000000, 'Refactor auth module', 'Auth refactored', 400, 0.08)",
        [],
    )
    .unwrap();

    // Data for Workspace B (/workspace/other)
    conn.execute(
        "INSERT INTO sessions (key, provider, cwd, first_seen_ns, last_seen_ns)
         VALUES ('s-other-1', 'antigravity', '/workspace/other', 5000000000, 6000000000)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, provider, cwd, start_ns, end_ns, input, output, total_tokens, total_cost_usd)
         VALUES ('t-other-1', 's-other-1', 'l-other-1', 'antigravity', '/workspace/other', 5000000000, 6000000000, 'Deploy app to staging', 'Deployed', 600, 0.12)",
        [],
    )
    .unwrap();

    db_path
}

#[test]
fn service_starts_with_scope() {
    let scope = Scope::workspace(Path::new("/workspace/test")).unwrap();
    let config = ServiceConfig {
        db_path: Path::new("/nonexistent/traces.db").to_path_buf(),
        scope,
        snapshot_dir: None,
    };
    let service = TraceService::new(config);
    assert!(service.is_ok());
}

#[test]
fn read_only_startup_does_not_create_db() {
    let temp = tempfile::tempdir().unwrap();
    let missing_db = temp.path().join("sub/missing.db");
    let scope = Scope::workspace(Path::new("/workspace/test")).unwrap();
    let service = TraceService::new(ServiceConfig {
        db_path: missing_db.clone(),
        scope,
        snapshot_dir: None,
    })
    .unwrap();

    let res = service.execute(Request::Briefing(BriefingArgs::default()));
    assert!(matches!(res, Err(ServiceError::DbUnavailable(_))));
    assert!(!missing_db.exists(), "Read-only query must not create missing DB");
}

#[test]
fn closed_argument_schemas_reject_unknown_fields() {
    let bad_json = json!({
        "since": null,
        "unexpected_extra_field": "disallowed"
    });

    let res = Request::from_tool_call("agent_mux_get_briefing", bad_json);
    assert!(matches!(res, Err(ServiceError::InvalidArgument(_))));
}

#[test]
fn cross_scope_briefing_totals_and_isolation() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = setup_test_db(temp.path());

    let scope = Scope::workspace(Path::new("/workspace/app")).unwrap();
    let service = TraceService::new(ServiceConfig {
        db_path,
        scope,
        snapshot_dir: None,
    })
    .unwrap();

    let res = service
        .execute(Request::Briefing(BriefingArgs {
            since: Some("1970-01-01T00:00:00Z".into()),
            until: Some("2030-01-01T00:00:00Z".into()),
            ..Default::default()
        }))
        .unwrap();

    assert_eq!(res.scope.workspace.as_deref(), Some("/workspace/app"));
    let data = res.data;
    assert_eq!(data["total_sessions"], 2);
    let sessions = data["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    for s in sessions {
        assert_eq!(s["cwd"], "/workspace/app");
        assert_ne!(s["session_key"], "s-other-1");
    }
}

#[test]
fn direct_id_nonexistent_vs_denied() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = setup_test_db(temp.path());

    let scope = Scope::workspace(Path::new("/workspace/app")).unwrap();
    let service = TraceService::new(ServiceConfig {
        db_path,
        scope,
        snapshot_dir: None,
    })
    .unwrap();

    // Nonexistent session key -> NOT_FOUND
    let res_missing = service.execute(Request::GetSession(GetSessionArgs {
        session_key: "s-missing".into(),
        launch_id: None,
    }));
    assert!(matches!(res_missing, Err(ServiceError::NotFound(_))));

    // Session key in /workspace/other -> SCOPE_DENIED (not exposed)
    let res_denied = service.execute(Request::GetSession(GetSessionArgs {
        session_key: "s-other-1".into(),
        launch_id: None,
    }));
    assert!(matches!(res_denied, Err(ServiceError::ScopeDenied(_))));

    // Authorized session key -> Ok
    let res_ok = service.execute(Request::GetSession(GetSessionArgs {
        session_key: "s-app-1".into(),
        launch_id: None,
    }));
    assert!(res_ok.is_ok());
}

#[test]
fn get_session_rejects_mismatched_launch_id() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = setup_test_db(temp.path());

    let scope = Scope::workspace(Path::new("/workspace/app")).unwrap();
    let service = TraceService::new(ServiceConfig {
        db_path,
        scope,
        snapshot_dir: None,
    })
    .unwrap();

    // l-app-2 belongs to s-app-2, not s-app-1
    let res = service.execute(Request::GetSession(GetSessionArgs {
        session_key: "s-app-1".into(),
        launch_id: Some("l-app-2".into()),
    }));
    assert!(matches!(res, Err(ServiceError::InvalidArgument(_))));
}

#[test]
fn scoped_search_isolates_workspaces() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = setup_test_db(temp.path());

    let scope = Scope::workspace(Path::new("/workspace/app")).unwrap();
    let service = TraceService::new(ServiceConfig {
        db_path,
        scope,
        snapshot_dir: None,
    })
    .unwrap();

    // Search for "error", which exists in s-app-1
    let res = service
        .execute(Request::Search(SearchArgs {
            query: "error".into(),
            session_key: None,
            provider: None,
            since: Some("1970-01-01T00:00:00Z".into()),
            until: Some("2030-01-01T00:00:00Z".into()),
            cursor: None,
            limit: None,
        }))
        .unwrap();

    let matches = res.data["matches"].as_array().unwrap();
    assert!(!matches.is_empty());
    for m in matches {
        assert_eq!(m["session_key"], "s-app-1");
    }

    // Search for "staging", which exists in s-other-1 (/workspace/other)
    let res_staging = service
        .execute(Request::Search(SearchArgs {
            query: "staging".into(),
            session_key: None,
            provider: None,
            since: Some("1970-01-01T00:00:00Z".into()),
            until: Some("2030-01-01T00:00:00Z".into()),
            cursor: None,
            limit: None,
        }))
        .unwrap();

    let matches_staging = res_staging.data["matches"].as_array().unwrap();
    assert!(
        matches_staging.is_empty(),
        "Matches outside workspace scope must be excluded"
    );
}

#[test]
fn compare_runs_scope_and_existence() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = setup_test_db(temp.path());

    let scope = Scope::workspace(Path::new("/workspace/app")).unwrap();
    let service = TraceService::new(ServiceConfig {
        db_path,
        scope,
        snapshot_dir: None,
    })
    .unwrap();

    // Compare two runs in /workspace/app
    let res = service.execute(Request::CompareRuns(CompareRunsArgs {
        a: "l-app-1".into(),
        b: "l-app-2".into(),
    }));
    assert!(res.is_ok());

    // Nonexistent run -> NOT_FOUND
    let res_missing = service.execute(Request::CompareRuns(CompareRunsArgs {
        a: "l-app-1".into(),
        b: "l-missing".into(),
    }));
    assert!(matches!(res_missing, Err(ServiceError::NotFound(_))));

    // Run in other workspace -> SCOPE_DENIED
    let res_denied = service.execute(Request::CompareRuns(CompareRunsArgs {
        a: "l-app-1".into(),
        b: "l-other-1".into(),
    }));
    assert!(matches!(res_denied, Err(ServiceError::ScopeDenied(_))));
}

#[test]
fn health_returns_capabilities_not_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = setup_test_db(temp.path());

    let scope = Scope::workspace(Path::new("/workspace/app")).unwrap();
    let service = TraceService::new(ServiceConfig {
        db_path,
        scope,
        snapshot_dir: None,
    })
    .unwrap();

    let res = service.execute(Request::Health(HealthArgs {})).unwrap();
    let data = res.data;
    assert_eq!(data["db_available"], true);
    assert!(data["features"].as_array().unwrap().len() >= 8);

    // Verify no secrets or sensitive environment variables leaked
    let json_str = serde_json::to_string(&data).unwrap();
    assert!(!json_str.contains("secret"));
    assert!(!json_str.contains("password"));
    assert!(!json_str.contains("token_key"));
}

#[test]
fn invalid_or_reversed_window_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = setup_test_db(temp.path());

    let scope = Scope::workspace(Path::new("/workspace/app")).unwrap();
    let service = TraceService::new(ServiceConfig {
        db_path,
        scope,
        snapshot_dir: None,
    })
    .unwrap();

    // Reversed window: since > until
    let res_reversed = service.execute(Request::Briefing(BriefingArgs {
        since: Some("2026-09-14T12:00:00Z".into()),
        until: Some("2026-09-13T12:00:00Z".into()),
        ..Default::default()
    }));
    assert!(matches!(res_reversed, Err(ServiceError::InvalidArgument(_))));

    // Invalid RFC3339 string
    let res_invalid = service.execute(Request::Briefing(BriefingArgs {
        since: Some("not-a-timestamp".into()),
        ..Default::default()
    }));
    assert!(matches!(res_invalid, Err(ServiceError::InvalidArgument(_))));
}
