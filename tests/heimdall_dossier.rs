//! Integration and contract tests for the Heimdall Rust dossier (schema v2).

use agent_mux::tracing::analysis::dossier::{
    CoverageStatus, DossierBuildError, DossierConfig, DossierInputs, build_dossier,
};
use agent_mux::tracing::analysis::model::{LiveSession, RuntimeState};
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use std::time::Duration;
use tempfile::tempdir;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

fn setup_test_db(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    for m in agent_mux::tracing::store::schema::MIGRATIONS {
        conn.execute_batch(m).unwrap();
    }
    conn
}

#[test]
fn dossier_contains_every_startup_section() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("traces.db");
    let conn = setup_test_db(&db);

    let ws = dir.path().join("work");
    fs::create_dir_all(&ws).unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let as_of = OffsetDateTime::parse("2026-09-17T12:00:00Z", &Rfc3339).unwrap();
    let as_of_ns = as_of.unix_timestamp_nanos() as i64;
    let session_since_ns = as_of_ns - 24 * 3600 * 1_000_000_000;
    let _eval_since_ns = as_of_ns - 30 * 24 * 3600 * 1_000_000_000;

    // Seed runs and launches
    conn.execute(
        "INSERT INTO runs (id, pid, agent_mux_version, started_ns, heartbeat_ns)
         VALUES ('r1', 1234, '0.1.0', ?1, ?1)",
        [session_since_ns],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug, content_mode, correlation_plan, started_ns, agent_mux_version)
         VALUES ('l1', 'r1', 1, 'default', 'claude', ?1, 'work', 'full', 'none', ?2, '0.1.0')",
        rusqlite::params![ws.to_str().unwrap(), session_since_ns],
    )
    .unwrap();

    // 1. Session 1: exactly at session_since_ns (must be INCLUDED in sessions)
    conn.execute(
        "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
         VALUES ('s_included', 'claude', 'sess-inc', ?1, ?2, ?2)",
        rusqlite::params![ws.to_str().unwrap(), session_since_ns],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, skills)
         VALUES ('t_included', 's_included', 'l1', 1, 'turn 1', 'closed', ?1, ?1, '[\"audit\"]')",
        [session_since_ns],
    )
    .unwrap();

    // Record exactly at until (as_of_ns): must be EXCLUDED
    conn.execute(
        "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
         VALUES ('s_excluded', 'claude', 'sess-exc', ?1, ?2, ?2)",
        rusqlite::params![ws.to_str().unwrap(), as_of_ns],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns)
         VALUES ('t_excluded', 's_excluded', 'l1', 1, 'turn 1', 'closed', ?1, ?1)",
        [as_of_ns],
    )
    .unwrap();

    // Agent invocation for 'reviewer' in evaluation window
    conn.execute(
        "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, level, input)
         VALUES ('a1', 't_included', 'agent', 'reviewer', ?1, ?1 + 10000000, 100, 0.10, 'DEFAULT', 'review')",
        [session_since_ns],
    )
    .unwrap();

    // Write definitions to disk so inventory_all finds them
    let claude_skills = ws.join(".claude/skills/audit");
    fs::create_dir_all(&claude_skills).unwrap();
    fs::write(
        claude_skills.join("SKILL.md"),
        "---\nname: audit\ndescription: Use when asked to \"audit\".\n---\nBody.\n",
    )
    .unwrap();

    let claude_agents = ws.join(".claude/agents");
    fs::create_dir_all(&claude_agents).unwrap();
    fs::write(
        claude_agents.join("reviewer.md"),
        "---\nname: reviewer\ndescription: Use to review code.\n---\nPrompt.\n",
    )
    .unwrap();

    let live_session = LiveSession {
        run_id: "r_live".into(),
        session_id: 1,
        session_key: Some("s_live".into()),
        launch_id: "l_live".into(),
        provider: Some("claude".into()),
        cwd: ws.clone(),
        state: RuntimeState::Working,
        updated_at_ns: as_of_ns,
        active_tools: vec!["Read".into()],
    };

    let inputs = DossierInputs {
        conn: &conn,
        db_path: &db,
        workspace: &ws,
        home: &home,
        live_sessions: &[live_session],
        live_snapshot_available: true,
        as_of,
    };

    let dossier = build_dossier(inputs, DossierConfig::default()).unwrap();
    assert_eq!(dossier.schema_version, 2);
    assert_eq!(dossier.windows.sessions.since, "2026-09-16T12:00:00Z");
    assert_eq!(dossier.windows.evaluations.since, "2026-08-18T12:00:00Z");
    assert_eq!(dossier.sessions.data.total_sessions, 2);
    assert_eq!(dossier.skills.data.skills[0].key, "audit");
    assert_eq!(dossier.agents.data.agents[0].agent_type, "reviewer");
    assert_eq!(dossier.health.db_path, db.display().to_string());
    assert_eq!(dossier.health.live_snapshot_available, true);
}

#[test]
fn dossier_builds_are_byte_for_byte_identical() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("traces.db");
    let conn = setup_test_db(&db);

    let ws = dir.path().join("work");
    fs::create_dir_all(&ws).unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let as_of = OffsetDateTime::parse("2026-09-17T12:00:00Z", &Rfc3339).unwrap();
    let as_of_ns = as_of.unix_timestamp_nanos() as i64;
    let session_since_ns = as_of_ns - 24 * 3600 * 1_000_000_000;

    conn.execute(
        "INSERT INTO runs (id, pid, agent_mux_version, started_ns, heartbeat_ns)
         VALUES ('r1', 1234, '0.1.0', ?1, ?1)",
        [session_since_ns],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug, content_mode, correlation_plan, started_ns, agent_mux_version)
         VALUES ('l1', 'r1', 1, 'default', 'claude', ?1, 'work', 'full', 'none', ?2, '0.1.0')",
        rusqlite::params![ws.to_str().unwrap(), session_since_ns],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
         VALUES ('s1', 'claude', 'sess-1', ?1, ?2, ?2)",
        rusqlite::params![ws.to_str().unwrap(), session_since_ns],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, skills)
         VALUES ('t1', 's1', 'l1', 1, 'turn 1', 'closed', ?1, ?1, '[\"audit\"]')",
        [session_since_ns],
    )
    .unwrap();

    let inputs1 = DossierInputs {
        conn: &conn,
        db_path: &db,
        workspace: &ws,
        home: &home,
        live_sessions: &[],
        live_snapshot_available: true,
        as_of,
    };
    let mut d1 = build_dossier(inputs1, DossierConfig::default()).unwrap();

    let inputs2 = DossierInputs {
        conn: &conn,
        db_path: &db,
        workspace: &ws,
        home: &home,
        live_sessions: &[],
        live_snapshot_available: true,
        as_of,
    };
    let mut d2 = build_dossier(inputs2, DossierConfig::default()).unwrap();

    // Timing telemetry in health may differ slightly between executions; normalize for byte comparison
    d1.health.total_build_ms = 0;
    d1.health.sessions_build_ms = 0;
    d1.health.skills_build_ms = 0;
    d1.health.agents_build_ms = 0;
    d2.health.total_build_ms = 0;
    d2.health.sessions_build_ms = 0;
    d2.health.skills_build_ms = 0;
    d2.health.agents_build_ms = 0;

    let json1 = serde_json::to_vec(&d1).unwrap();
    let json2 = serde_json::to_vec(&d2).unwrap();
    assert_eq!(json1, json2);
}

#[test]
fn dossier_small_budget_removes_examples_before_rows_and_sets_truncation() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("traces.db");
    let conn = setup_test_db(&db);

    let ws = dir.path().join("work");
    fs::create_dir_all(&ws).unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let as_of = OffsetDateTime::parse("2026-09-17T12:00:00Z", &Rfc3339).unwrap();
    let as_of_ns = as_of.unix_timestamp_nanos() as i64;
    let session_since_ns = as_of_ns - 24 * 3600 * 1_000_000_000;

    conn.execute(
        "INSERT INTO runs (id, pid, agent_mux_version, started_ns, heartbeat_ns)
         VALUES ('r1', 1234, '0.1.0', ?1, ?1)",
        [session_since_ns],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug, content_mode, correlation_plan, started_ns, agent_mux_version)
         VALUES ('l1', 'r1', 1, 'default', 'claude', ?1, 'work', 'full', 'none', ?2, '0.1.0')",
        rusqlite::params![ws.to_str().unwrap(), session_since_ns],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
         VALUES ('s1', 'claude', 'sess-1', ?1, ?2, ?2)",
        rusqlite::params![ws.to_str().unwrap(), session_since_ns],
    )
    .unwrap();

    // Create 80 skill definitions on disk
    for i in 0..80 {
        let s_dir = ws.join(format!(".claude/skills/skill_{i:02}"));
        fs::create_dir_all(&s_dir).unwrap();
        fs::write(
            s_dir.join("SKILL.md"),
            format!("---\nname: skill_{i:02}\ndescription: Test skill {i:02}.\n---\nBody text\n"),
        )
        .unwrap();
    }

    let inputs = DossierInputs {
        conn: &conn,
        db_path: &db,
        workspace: &ws,
        home: &home,
        live_sessions: &[],
        live_snapshot_available: false,
        as_of,
    };

    let mut config = DossierConfig::default();
    config.max_skill_rows = 100;
    config.max_bytes = 4096;

    let dossier = build_dossier(inputs, config.clone()).unwrap();
    let json = serde_json::to_vec(&dossier).unwrap();
    assert!(json.len() <= config.max_bytes);
    assert!(dossier.skills.truncated);
    assert_eq!(dossier.skills.total_matching, 80);
    assert!(dossier.skills.data.skills.len() < 80);
    assert_ne!(dossier.sessions.status, CoverageStatus::Unavailable);
}

#[test]
fn dossier_minimal_budget_returns_too_large_error() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("traces.db");
    let conn = setup_test_db(&db);
    let ws = dir.path().join("work");
    fs::create_dir_all(&ws).unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let as_of = OffsetDateTime::parse("2026-09-17T12:00:00Z", &Rfc3339).unwrap();

    let inputs = DossierInputs {
        conn: &conn,
        db_path: &db,
        workspace: &ws,
        home: &home,
        live_sessions: &[],
        live_snapshot_available: false,
        as_of,
    };

    let mut config = DossierConfig::default();
    config.max_bytes = 100;

    let result = build_dossier(inputs, config);
    assert!(matches!(result, Err(DossierBuildError::TooLarge(_))));
}

#[test]
fn dossier_deadline_timeout_yields_query_timeout_and_cleans_up() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("traces.db");
    let conn = setup_test_db(&db);
    let ws = dir.path().join("work");
    fs::create_dir_all(&ws).unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let as_of = OffsetDateTime::parse("2026-09-17T12:00:00Z", &Rfc3339).unwrap();
    let as_of_ns = as_of.unix_timestamp_nanos() as i64;
    let session_since_ns = as_of_ns - 24 * 3600 * 1_000_000_000;

    conn.execute(
        "INSERT INTO runs (id, pid, agent_mux_version, started_ns, heartbeat_ns)
         VALUES ('r1', 1234, '0.1.0', ?1, ?1)",
        [session_since_ns],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug, content_mode, correlation_plan, started_ns, agent_mux_version)
         VALUES ('l1', 'r1', 1, 'default', 'claude', ?1, 'work', 'full', 'none', ?2, '0.1.0')",
        rusqlite::params![ws.to_str().unwrap(), session_since_ns],
    )
    .unwrap();

    for i in 0..50 {
        let key = format!("s_{i}");
        conn.execute(
            "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
             VALUES (?1, 'claude', ?2, ?3, ?4, ?4)",
            rusqlite::params![key, format!("sess-{i}"), ws.to_str().unwrap(), session_since_ns + i],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns)
             VALUES (?1, ?2, 'l1', 1, 'turn', 'closed', ?3, ?3)",
            rusqlite::params![format!("t_{i}"), key, session_since_ns + i],
        )
        .unwrap();
    }

    let inputs = DossierInputs {
        conn: &conn,
        db_path: &db,
        workspace: &ws,
        home: &home,
        live_sessions: &[],
        live_snapshot_available: false,
        as_of,
    };

    let mut config = DossierConfig::default();
    config.deadline = Duration::from_nanos(0);

    let dossier = build_dossier(inputs, config).unwrap();
    let any_timeout = dossier.sessions.errors.iter().any(|e| e.code == "QUERY_TIMEOUT")
        || dossier.skills.errors.iter().any(|e| e.code == "QUERY_TIMEOUT")
        || dossier.agents.errors.iter().any(|e| e.code == "QUERY_TIMEOUT");
    assert!(any_timeout, "expected at least one section to report QUERY_TIMEOUT");

    // Proving handler cleanup: subsequent query on same connection succeeds
    let val: i32 = conn.query_row("SELECT 1", [], |r| r.get(0)).unwrap();
    assert_eq!(val, 1);
}
