//! `agent_mux_analyze_agents` against a real-schema store: launches tagged
//! with an agent id are grouped per agent and per definition version, scoped
//! by workspace, and summarized with drift between versions.

use agent_mux::tracing::analysis::Scope;
use agent_mux::tracing::analysis::service::{
    AnalyzeAgentsArgs, AnalyzeAgentsData, Request, ServiceConfig, TraceService,
};
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::{OpenOptions, open_rw};
use rusqlite::{Connection, params};
use std::path::Path;
use tempfile::tempdir;
use time::OffsetDateTime;

fn real_schema_db(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("traces.db");
    let store = open_rw(
        &path,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run-1".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    drop(store);
    path
}

#[allow(clippy::too_many_arguments)]
fn seed_launch(
    conn: &Connection,
    id: &str,
    provider: &str,
    cwd: &str,
    started_ns: i64,
    ended_ns: Option<i64>,
    agent_id: &str,
    hash: &str,
) {
    conn.execute(
        "INSERT INTO runs (id, agent_mux_version, started_ns, heartbeat_ns)
         VALUES ('run-1', 'test', ?1, ?1) ON CONFLICT(id) DO NOTHING",
        params![started_ns],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug,
             content_mode, correlation_plan, started_ns, ended_ns, agent_mux_version, metadata)
         VALUES (?1, 'run-1', 1, 'Heimdall', ?2, ?3, 'slug', 'full', 'deterministic', ?4, ?5, 'test',
             json_object('agent_id', ?6, 'agent_source_hash', ?7))",
        params![id, provider, cwd, started_ns, ended_ns, agent_id, hash],
    )
    .unwrap();
}

fn seed_turn(conn: &Connection, launch_id: &str, trace_id: &str, ns: i64, tools: &[(bool, i64)]) {
    let key = format!("claude:{launch_id}");
    conn.execute(
        "INSERT OR IGNORE INTO sessions (key, provider, session_id, first_seen_ns, last_seen_ns)
         VALUES (?1, 'claude', ?2, ?3, ?3)",
        params![key, launch_id, ns],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns)
         VALUES (?1, ?2, ?3, 1, 'turn', 'closed', ?4, ?4 + 1000)",
        params![trace_id, key, launch_id, ns],
    )
    .unwrap();
    for (i, (is_error, tokens)) in tools.iter().enumerate() {
        conn.execute(
            "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, level, total_tokens, total_cost_usd)
             VALUES (?1, ?2, 'tool', 'Bash', ?3, ?3 + 500, ?4, ?5, 0.01)",
            params![
                format!("{trace_id}-o{i}"),
                trace_id,
                ns + i as i64,
                if *is_error { "ERROR" } else { "DEFAULT" },
                tokens
            ],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns)
         VALUES (?1, ?2, 'generation', 'claude-opus-5', ?3, ?3 + 2000000)",
        params![format!("{trace_id}-gen"), trace_id, ns],
    )
    .unwrap();
}

#[test]
fn analyze_agents_groups_by_agent_and_version_with_drift() {
    let root = tempdir().unwrap();
    let db = real_schema_db(root.path());
    let ws = root.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let ws = ws.canonicalize().unwrap();
    let ws_s = ws.to_string_lossy().to_string();
    let other = root.path().join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();
    let other_s = other.canonicalize().unwrap().to_string_lossy().to_string();

    let conn = Connection::open(&db).unwrap();
    let t0 = OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 - 60_000_000_000;

    // Heimdall v1: two launches, noisy tools.
    seed_launch(
        &conn,
        "l1",
        "claude",
        &ws_s,
        t0,
        Some(t0 + 10_000_000_000),
        "heimdall",
        "aaaa1111",
    );
    seed_turn(
        &conn,
        "l1",
        "t1",
        t0 + 1_000,
        &[(false, 10), (true, 10), (true, 10), (false, 10)],
    );
    seed_launch(
        &conn,
        "l2",
        "codex",
        &ws_s,
        t0 + 1_000_000,
        Some(t0 + 5_000_000_000),
        "heimdall",
        "aaaa1111",
    );
    seed_turn(&conn, "l2", "t2", t0 + 1_001_000, &[(false, 5), (true, 5)]);
    // Heimdall v2: one launch, cleaner.
    seed_launch(
        &conn,
        "l3",
        "agy",
        &ws_s,
        t0 + 2_000_000,
        None,
        "heimdall",
        "bbbb2222",
    );
    seed_turn(&conn, "l3", "t3", t0 + 2_001_000, &[(false, 7), (false, 7)]);
    // Another agent in the same workspace, and Heimdall in another workspace.
    seed_launch(
        &conn,
        "l4",
        "claude",
        &ws_s,
        t0 + 3_000_000,
        Some(t0 + 4_000_000),
        "audit",
        "cccc3333",
    );
    seed_launch(
        &conn,
        "l5",
        "claude",
        &other_s,
        t0 + 4_000_000,
        None,
        "heimdall",
        "bbbb2222",
    );
    // A plain launch without agent metadata is ignored.
    conn.execute(
        "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug,
             content_mode, correlation_plan, started_ns, agent_mux_version)
         VALUES ('plain', 'run-1', 2, 'Claude Code', 'claude', ?1, 'slug', 'full', 'deterministic', ?2, 'test')",
        params![ws_s, t0],
    )
    .unwrap();
    drop(conn);

    // Workspace scope: only launches from `ws`.
    let service = TraceService::new(ServiceConfig::new(
        db.clone(),
        Scope::workspace(&ws).unwrap(),
    ))
    .unwrap();
    let env = service
        .execute(Request::AnalyzeAgents(AnalyzeAgentsArgs::default()))
        .unwrap();
    assert!(env.warnings.is_empty(), "{:?}", env.warnings);
    let data: AnalyzeAgentsData = serde_json::from_value(env.data).unwrap();
    assert_eq!(data.total_agents, 2);
    let heimdall = data
        .agents
        .iter()
        .find(|a| a.agent_id == "heimdall")
        .unwrap();
    assert_eq!(
        heimdall.launches, 3,
        "other workspace excluded, plain launch ignored"
    );
    assert_eq!(heimdall.providers, vec!["claude", "codex", "agy"]);
    assert_eq!(heimdall.turns, 3);
    assert_eq!(heimdall.tools, 8);
    assert_eq!(heimdall.tool_errors, 3);
    assert_eq!(heimdall.tool_error_rate, Some(3.0 / 8.0));
    assert_eq!(heimdall.tokens, Some(64));
    assert_eq!(heimdall.generation_sample_size, 3);
    assert_eq!(heimdall.generation_p50_ms, Some(2));
    assert_eq!(heimdall.avg_launch_duration_ms, Some(7_499));
    assert!(heimdall.last_launch.is_some());

    assert_eq!(heimdall.versions.len(), 2);
    assert_eq!(heimdall.versions[0].source_hash, "aaaa1111");
    assert_eq!(heimdall.versions[0].launches, 2);
    assert_eq!(heimdall.versions[0].tool_errors, 3);
    assert_eq!(heimdall.versions[1].source_hash, "bbbb2222");
    assert_eq!(heimdall.versions[1].launches, 1);
    assert_eq!(heimdall.versions[1].tool_errors, 0);
    assert!(
        heimdall
            .drift
            .iter()
            .any(|d| d.contains("tool error rate 50.0% → 0.0% (aaaa1111→bbbb2222)")),
        "{:?}",
        heimdall.drift
    );

    let audit = data.agents.iter().find(|a| a.agent_id == "audit").unwrap();
    assert_eq!(audit.launches, 1);
    assert_eq!(audit.turns, 0);
    assert!(
        audit
            .limitations
            .iter()
            .any(|l| l.contains("no token usage"))
    );

    // Filters and all-workspaces scope.
    let service_all = TraceService::new(ServiceConfig::new(db, Scope::all_workspaces())).unwrap();
    let env = service_all
        .execute(Request::AnalyzeAgents(AnalyzeAgentsArgs {
            agent: Some("heimdall".into()),
            provider: Some("claude".into()),
            ..Default::default()
        }))
        .unwrap();
    let data: AnalyzeAgentsData = serde_json::from_value(env.data).unwrap();
    assert_eq!(data.total_agents, 1);
    assert_eq!(
        data.agents[0].launches, 2,
        "claude launches from both workspaces"
    );
}

#[test]
fn analyze_agents_reports_partial_coverage_without_launch_metadata() {
    let conn_dir = tempdir().unwrap();
    let db = conn_dir.path().join("old.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE launches (id TEXT PRIMARY KEY, started_ns INTEGER);")
        .unwrap();
    drop(conn);
    let service = TraceService::new(ServiceConfig::new(db, Scope::all_workspaces())).unwrap();
    let env = service
        .execute(Request::AnalyzeAgents(AnalyzeAgentsArgs::default()))
        .unwrap();
    assert_eq!(env.data["total_agents"], 0);
    assert!(env.warnings.iter().any(|w| w.contains("no agent metadata")));
}
