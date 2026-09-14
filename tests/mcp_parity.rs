use agent_mux::tracing::analysis::live::{LiveSnapshot, publish_snapshot};
use agent_mux::tracing::analysis::model::{LiveSession, RuntimeState};
use agent_mux::tracing::analysis::scope::Scope;
use agent_mux::tracing::analysis::service::{
    AnalyzeSkillsArgs, BriefingArgs, CompareRunsArgs, GetSessionArgs, HealthArgs, ListSessionsArgs,
    Request, SearchArgs, ServiceConfig, ServiceError, TimelineArgs, TraceService,
};
use rusqlite::Connection;
use std::path::Path;
use tempfile::tempdir;
use time::OffsetDateTime;

fn setup_test_db(db_path: &Path) {
    let conn = Connection::open(db_path).unwrap();
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
            t.id,
            t.session_key,
            t.launch_id,
            ROW_NUMBER() OVER (PARTITION BY t.session_key ORDER BY t.start_ns) AS ordinal,
            'completed' AS status,
            t.start_ns,
            t.end_ns,
            (COALESCE(t.end_ns, t.start_ns) - t.start_ns) / 1000000 AS latency_ms,
            t.input,
            t.output,
            '' AS thinking,
            t.total_tokens,
            t.total_cost_usd,
            0 AS is_error,
            NULL AS status_message,
            0 AS tool_call_count,
            0 AS subagent_call_count
        FROM traces t;
        CREATE VIEW skill_stats AS
        SELECT
            skill,
            COUNT(DISTINCT trace_id) AS turns_loaded,
            COUNT(id) AS tools,
            SUM(total_tokens) AS tokens,
            SUM(total_cost_usd) AS cost,
            0 AS turns_unused
        FROM observations
        WHERE skill IS NOT NULL AND skill != ''
        GROUP BY skill;",
    )
    .unwrap();
}

#[test]
fn health_arguments_are_closed() {
    assert!(serde_json::from_str::<HealthArgs>("{}").is_ok());
    assert!(serde_json::from_str::<HealthArgs>(r#"{"all_workspaces":true}"#).is_err());
}

#[test]
fn all_tool_arguments_are_closed_schemas() {
    assert!(serde_json::from_str::<BriefingArgs>(r#"{"extra":123}"#).is_err());
    assert!(serde_json::from_str::<ListSessionsArgs>(r#"{"unknown":"field"}"#).is_err());
    assert!(serde_json::from_str::<GetSessionArgs>(r#"{"session_key":"s1","foo":"bar"}"#).is_err());
    assert!(serde_json::from_str::<TimelineArgs>(r#"{"session_key":"s1","bad":true}"#).is_err());
    assert!(serde_json::from_str::<SearchArgs>(r#"{"query":"q","invalid":null}"#).is_err());
    assert!(serde_json::from_str::<AnalyzeSkillsArgs>(r#"{"skill":"s","extra":false}"#).is_err());
    assert!(serde_json::from_str::<CompareRunsArgs>(r#"{"a":"1","b":"2","c":"3"}"#).is_err());
}

#[test]
fn eight_tools_parity_and_execution() {
    let root = tempdir().unwrap();
    let db_path = root.path().join("traces.db");
    let ws_path = root
        .path()
        .join("project")
        .canonicalize()
        .unwrap_or_else(|_| {
            std::fs::create_dir_all(root.path().join("project")).unwrap();
            root.path().join("project").canonicalize().unwrap()
        });

    setup_test_db(&db_path);

    let now_ns = OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 - 5_000_000_000;
    let conn = Connection::open(&db_path).unwrap();

    // Seed session 1 & 2
    conn.execute(
        "INSERT INTO sessions (key, provider, cwd, first_seen_ns, last_seen_ns)
         VALUES ('s1', 'claude', ?1, ?2, ?3), ('s2', 'codex', ?1, ?2, ?3)",
        rusqlite::params![ws_path.to_string_lossy().to_string(), now_ns, now_ns + 1000],
    )
    .unwrap();

    // Seed traces
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, provider, cwd, start_ns, end_ns, input, output, total_tokens, total_cost_usd)
         VALUES ('t1', 's1', 'l1', 'claude', ?1, ?2, ?3, 'fix tests', 'tests fixed', 100, 0.002),
                ('t2', 's2', 'l2', 'codex', ?1, ?2, ?3, 'build binary', 'binary built', 200, 0.004)",
        rusqlite::params![ws_path.to_string_lossy().to_string(), now_ns, now_ns + 1000],
    )
    .unwrap();

    // Seed observations
    conn.execute(
        "INSERT INTO observations (id, trace_id, name, type, start_ns, end_ns, is_error, status_message, input, output, total_tokens, total_cost_usd, skill)
         VALUES ('o1', 't1', 'cargo_test', 'tool', ?1, ?2, 0, 'ok', 'cargo test', 'pass', 50, 0.001, 'rust-dev')",
        rusqlite::params![now_ns, now_ns + 500],
    )
    .unwrap();

    let service = TraceService::new(ServiceConfig::new(
        db_path.clone(),
        Scope::workspace(&ws_path).unwrap(),
    ))
    .unwrap();

    // 1. Briefing
    let briefing_res = service
        .execute(Request::Briefing(BriefingArgs::default()))
        .unwrap();
    assert_eq!(briefing_res.schema_version, 1);
    assert_eq!(
        briefing_res.scope.workspace.as_deref(),
        Some(ws_path.to_str().unwrap())
    );

    // 2. ListSessions
    let list_res = service
        .execute(Request::ListSessions(ListSessionsArgs::default()))
        .unwrap();
    assert_eq!(list_res.schema_version, 1);

    // 3. GetSession
    let get_res = service
        .execute(Request::GetSession(GetSessionArgs {
            session_key: "s1".to_string(),
            launch_id: None,
        }))
        .unwrap();
    assert_eq!(get_res.schema_version, 1);

    // 4. Timeline
    let timeline_res = service
        .execute(Request::Timeline(TimelineArgs {
            session_key: "s1".to_string(),
            launch_id: None,
            cursor: None,
            limit: None,
        }))
        .unwrap();
    assert_eq!(timeline_res.schema_version, 1);

    // 5. Search
    let search_res = service
        .execute(Request::Search(SearchArgs {
            query: "fix".to_string(),
            session_key: None,
            provider: None,
            since: None,
            until: None,
            cursor: None,
            limit: None,
        }))
        .unwrap();
    assert_eq!(search_res.schema_version, 1);

    // 6. AnalyzeSkills
    let skills_res = service
        .execute(Request::AnalyzeSkills(AnalyzeSkillsArgs::default()))
        .unwrap();
    assert_eq!(skills_res.schema_version, 1);

    // 7. CompareRuns
    let compare_res = service
        .execute(Request::CompareRuns(CompareRunsArgs {
            a: "l1".to_string(),
            b: "l2".to_string(),
        }))
        .unwrap();
    assert_eq!(compare_res.schema_version, 1);

    // 8. Health
    let health_res = service.execute(Request::Health(HealthArgs {})).unwrap();
    assert_eq!(health_res.schema_version, 1);
}

#[test]
fn scope_attacks_are_strictly_denied() {
    let root = tempdir().unwrap();
    let db_path = root.path().join("traces.db");
    let ws_path = root.path().join("project");
    std::fs::create_dir_all(&ws_path).unwrap();
    let ws_path = ws_path.canonicalize().unwrap();

    let outside_path = root.path().join("other_project");
    std::fs::create_dir_all(&outside_path).unwrap();
    let outside_path = outside_path.canonicalize().unwrap();

    setup_test_db(&db_path);

    let now_ns = OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 - 10_000_000_000;
    let conn = Connection::open(&db_path).unwrap();

    // Session belongs to outside_path
    conn.execute(
        "INSERT INTO sessions (key, provider, cwd, first_seen_ns, last_seen_ns)
         VALUES ('secret_session', 'claude', ?1, ?2, ?3)",
        rusqlite::params![
            outside_path.to_string_lossy().to_string(),
            now_ns,
            now_ns + 1000
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, provider, cwd, start_ns, end_ns, input, output, total_tokens, total_cost_usd)
         VALUES ('t_secret', 'secret_session', 'l_secret', 'claude', ?1, ?2, ?3, 'secret input', 'secret output', 10, 0.001)",
        rusqlite::params![outside_path.to_string_lossy().to_string(), now_ns, now_ns + 1000],
    )
    .unwrap();

    let service = TraceService::new(ServiceConfig::new(
        db_path,
        Scope::workspace(&ws_path).unwrap(),
    ))
    .unwrap();

    // Direct access to secret_session must be denied
    let res = service.execute(Request::GetSession(GetSessionArgs {
        session_key: "secret_session".to_string(),
        launch_id: None,
    }));
    assert!(matches!(res, Err(ServiceError::ScopeDenied(_))));

    // Timeline access to secret_session must be denied
    let timeline_res = service.execute(Request::Timeline(TimelineArgs {
        session_key: "secret_session".to_string(),
        launch_id: None,
        cursor: None,
        limit: None,
    }));
    assert!(matches!(timeline_res, Err(ServiceError::ScopeDenied(_))));
}

#[test]
fn stale_live_publisher_is_discarded() {
    let root = tempdir().unwrap();
    let db_path = root.path().join("traces.db");
    let snap_dir = root.path().join("snapshots");
    let ws_path = root.path().join("project");
    std::fs::create_dir_all(&ws_path).unwrap();
    let ws_path = ws_path.canonicalize().unwrap();

    setup_test_db(&db_path);

    let now_ns = OffsetDateTime::now_utc().unix_timestamp_nanos() as i64;
    // Heartbeat from 10 seconds ago (> 5s threshold -> stale)
    let stale_snap = LiveSnapshot {
        run_id: "run-stale".to_string(),
        revision: 1,
        heartbeat_ns: now_ns - 10_000_000_000,
        sessions: vec![LiveSession {
            run_id: "run-stale".to_string(),
            session_id: 1,
            session_key: Some("live_s1".into()),
            launch_id: "live_l1".into(),
            provider: Some("claude".into()),
            cwd: ws_path.clone(),
            state: RuntimeState::Working,
            updated_at_ns: now_ns - 10_000_000_000,
            active_tools: vec!["cargo test".into()],
        }],
    };

    publish_snapshot(&snap_dir, &stale_snap).unwrap();

    let mut config = ServiceConfig::new(db_path, Scope::workspace(&ws_path).unwrap());
    config.snapshot_dir = Some(snap_dir);

    let service = TraceService::new(config).unwrap();
    let (live_sessions, available) = service.load_live_sessions(now_ns);
    // Stale snapshot must NOT be reported as active live sessions
    assert!(live_sessions.is_empty());
    assert!(!available);
}

#[test]
fn incomplete_usage_and_unknown_provider_handled_gracefully() {
    let root = tempdir().unwrap();
    let db_path = root.path().join("traces.db");
    let ws_path = root.path().join("project");
    std::fs::create_dir_all(&ws_path).unwrap();
    let ws_path = ws_path.canonicalize().unwrap();

    setup_test_db(&db_path);

    let now_ns = OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 - 10_000_000_000;
    let conn = Connection::open(&db_path).unwrap();

    // Provider is unknown, total_tokens & total_cost_usd are NULL
    conn.execute(
        "INSERT INTO sessions (key, provider, cwd, first_seen_ns, last_seen_ns)
         VALUES ('s_unknown', 'future_ai_harness', ?1, ?2, ?3)",
        rusqlite::params![ws_path.to_string_lossy().to_string(), now_ns, now_ns + 1000],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, provider, cwd, start_ns, end_ns, input, output, total_tokens, total_cost_usd)
         VALUES ('t_unknown', 's_unknown', 'l_unknown', 'future_ai_harness', ?1, ?2, ?3, 'hello', 'world', NULL, NULL)",
        rusqlite::params![ws_path.to_string_lossy().to_string(), now_ns, now_ns + 1000],
    )
    .unwrap();

    let service = TraceService::new(ServiceConfig::new(
        db_path,
        Scope::workspace(&ws_path).unwrap(),
    ))
    .unwrap();

    let briefing_res = service
        .execute(Request::Briefing(BriefingArgs::default()))
        .unwrap();
    assert_eq!(briefing_res.schema_version, 1);
    let briefing_data: agent_mux::tracing::analysis::BriefingData =
        serde_json::from_value(briefing_res.data).unwrap();
    assert_eq!(briefing_data.total_sessions, 1);
    let session = &briefing_data.sessions[0];
    assert_eq!(session.provider.as_deref(), Some("future_ai_harness"));
    assert_eq!(session.total_tokens, None);
    assert_eq!(session.total_cost_usd, None);
}
