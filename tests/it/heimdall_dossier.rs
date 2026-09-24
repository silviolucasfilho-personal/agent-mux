//! Integration and contract tests for the Heimdall Rust dossier (schema v2).

use agent_mux::tracing::analysis::dossier::{
    CoverageStatus, DossierBuildError, DossierConfig, DossierInputs, build_dossier,
};
use agent_mux::tracing::analysis::model::{LiveSession, RuntimeState};
use rusqlite::Connection;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    assert!(dossier.health.live_snapshot_available);
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

    let config = DossierConfig {
        max_bytes: 4096,
        max_skill_rows: 100,
        ..Default::default()
    };

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

    let config = DossierConfig {
        max_bytes: 100,
        ..Default::default()
    };

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
            rusqlite::params![
                key,
                format!("sess-{i}"),
                ws.to_str().unwrap(),
                session_since_ns + i
            ],
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

    let config = DossierConfig {
        deadline: Duration::from_nanos(0),
        ..Default::default()
    };

    let dossier = build_dossier(inputs, config).unwrap();
    let any_timeout = dossier
        .sessions
        .errors
        .iter()
        .any(|e| e.code == "QUERY_TIMEOUT")
        || dossier
            .skills
            .errors
            .iter()
            .any(|e| e.code == "QUERY_TIMEOUT")
        || dossier
            .agents
            .errors
            .iter()
            .any(|e| e.code == "QUERY_TIMEOUT");
    assert!(
        any_timeout,
        "expected at least one section to report QUERY_TIMEOUT"
    );

    // Proving handler cleanup: subsequent query on same connection succeeds
    let val: i32 = conn.query_row("SELECT 1", [], |r| r.get(0)).unwrap();
    assert_eq!(val, 1);
}

#[test]
fn large_fixture_is_bounded_and_reports_build_time() {
    let as_of = OffsetDateTime::parse("2026-09-17T12:00:00Z", &Rfc3339).unwrap();
    let as_of_ns = as_of.unix_timestamp_nanos() as i64;
    let session_since_ns = as_of_ns - 24 * 3600 * 1_000_000_000;
    let eval_since_ns = as_of_ns - 30 * 24 * 3600 * 1_000_000_000;

    // 1. Small fixture: 2 sessions, 2 skills, 2 agents, 2 turns
    let dir_small = tempdir().unwrap();
    let db_small = dir_small.path().join("traces.db");
    let conn_small = setup_test_db(&db_small);
    let ws_small = dir_small.path().join("work");
    fs::create_dir_all(&ws_small).unwrap();
    let home_small = dir_small.path().join("home");
    fs::create_dir_all(&home_small).unwrap();

    conn_small
        .execute(
            "INSERT INTO runs (id, pid, agent_mux_version, started_ns, heartbeat_ns)
             VALUES ('r_small', 100, '0.1.0', ?1, ?1)",
            [session_since_ns],
        )
        .unwrap();

    conn_small
        .execute(
            "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug, content_mode, correlation_plan, started_ns, agent_mux_version)
             VALUES ('l_small', 'r_small', 1, 'default', 'claude', ?1, 'work', 'full', 'none', ?2, '0.1.0')",
            rusqlite::params![ws_small.to_str().unwrap(), session_since_ns],
        )
        .unwrap();

    for i in 0..2 {
        let s_dir = ws_small.join(format!(".claude/skills/skill_{i}"));
        fs::create_dir_all(&s_dir).unwrap();
        fs::write(
            s_dir.join("SKILL.md"),
            format!("---\nname: skill_{i}\ndescription: Test skill {i}\n---\nBody\n"),
        )
        .unwrap();
    }

    let agents_dir_small = ws_small.join(".claude/agents");
    fs::create_dir_all(&agents_dir_small).unwrap();
    for i in 0..2 {
        fs::write(
            agents_dir_small.join(format!("agent_{i}.md")),
            format!("---\nname: agent_{i}\ndescription: Test agent {i}\n---\nPrompt\n"),
        )
        .unwrap();
    }

    for i in 0..2 {
        let key = format!("s_small_{i}");
        let sess_time = session_since_ns + (i as i64 + 1) * 1_000_000_000;
        conn_small
            .execute(
                "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
                 VALUES (?1, 'claude', ?2, ?3, ?4, ?4)",
                rusqlite::params![
                    key,
                    format!("sess-{i}"),
                    ws_small.to_str().unwrap(),
                    sess_time
                ],
            )
            .unwrap();

        let trace_id = format!("t_small_{i}");
        conn_small
            .execute(
                "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, input, output, skills)
                 VALUES (?1, ?2, 'l_small', 1, 'turn 1', 'closed', ?3, ?3 + 500000000, 'input', 'output', ?4)",
                rusqlite::params![trace_id, key, sess_time, format!("[\"skill_{i}\"]")],
            )
            .unwrap();

        let agent_obs_id = format!("a_small_{i}");
        conn_small
            .execute(
                "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, level, input)
                 VALUES (?1, ?2, 'agent', ?3, ?4, ?4 + 10000000, 100, 0.05, 'DEFAULT', 'task')",
                rusqlite::params![agent_obs_id, trace_id, format!("agent_{i}"), sess_time],
            )
            .unwrap();

        conn_small
            .execute(
                "INSERT INTO observations (id, trace_id, parent_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, level)
                 VALUES (?1, ?2, ?3, 'tool', 'Bash', ?4, ?4 + 5000000, 10, 0.01, 'DEFAULT')",
                rusqlite::params![format!("c_small_{i}"), trace_id, agent_obs_id, sess_time],
            )
            .unwrap();
    }

    // 2. Large fixture: 100 sessions (2 in 24h window, 98 in 30d window), 100 skills, 100 agents, 1,000 turns
    let dir_large = tempdir().unwrap();
    let db_large = dir_large.path().join("traces.db");
    let conn_large = setup_test_db(&db_large);
    let ws_large = dir_large.path().join("work");
    fs::create_dir_all(&ws_large).unwrap();
    let home_large = dir_large.path().join("home");
    fs::create_dir_all(&home_large).unwrap();

    conn_large
        .execute(
            "INSERT INTO runs (id, pid, agent_mux_version, started_ns, heartbeat_ns)
             VALUES ('r_large', 101, '0.1.0', ?1, ?1)",
            [session_since_ns],
        )
        .unwrap();

    conn_large
        .execute(
            "INSERT INTO launches (id, run_id, agent_mux_session, profile, provider, cwd, project_slug, content_mode, correlation_plan, started_ns, agent_mux_version)
             VALUES ('l_large', 'r_large', 1, 'default', 'claude', ?1, 'work', 'full', 'none', ?2, '0.1.0')",
            rusqlite::params![ws_large.to_str().unwrap(), session_since_ns],
        )
        .unwrap();

    for i in 0..100 {
        let s_dir = ws_large.join(format!(".claude/skills/skill_{i}"));
        fs::create_dir_all(&s_dir).unwrap();
        fs::write(
            s_dir.join("SKILL.md"),
            format!("---\nname: skill_{i}\ndescription: Test skill {i}\n---\nBody\n"),
        )
        .unwrap();
    }

    let agents_dir_large = ws_large.join(".claude/agents");
    fs::create_dir_all(&agents_dir_large).unwrap();
    for i in 0..100 {
        fs::write(
            agents_dir_large.join(format!("agent_{i}.md")),
            format!("---\nname: agent_{i}\ndescription: Test agent {i}\n---\nPrompt\n"),
        )
        .unwrap();
    }

    conn_large.execute_batch("BEGIN TRANSACTION;").unwrap();
    for i in 0..100 {
        let key = format!("s_large_{i}");
        let sess_time = if i < 2 {
            session_since_ns + (i as i64 + 1) * 1_000_000_000
        } else {
            eval_since_ns + (i as i64) * 60_000_000_000
        };
        conn_large
            .execute(
                "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
                 VALUES (?1, 'claude', ?2, ?3, ?4, ?4)",
                rusqlite::params![
                    key,
                    format!("sess-{i}"),
                    ws_large.to_str().unwrap(),
                    sess_time
                ],
            )
            .unwrap();
    }

    for t in 0..1000 {
        let sess_idx = t % 100;
        let key = format!("s_large_{sess_idx}");
        let trace_id = format!("t_large_{t}");
        let trace_time = if sess_idx < 2 {
            session_since_ns + (t as i64) * 10_000_000
        } else {
            eval_since_ns + (t as i64) * 6_000_000_000
        };
        let skill_name = format!("skill_{}", t % 100);
        conn_large
            .execute(
                "INSERT INTO traces (id, session_key, launch_id, ordinal, name, status, start_ns, end_ns, input, output, skills)
                 VALUES (?1, ?2, 'l_large', ?3, 'turn', 'closed', ?4, ?4 + 50000000, 'input', 'output', ?5)",
                rusqlite::params![trace_id, key, t + 1, trace_time, format!("[\"{skill_name}\"]")],
            )
            .unwrap();

        if t < 100 {
            let agent_obs_id = format!("a_large_{t}");
            let agent_name = format!("agent_{t}");
            conn_large
                .execute(
                    "INSERT INTO observations (id, trace_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, level, input)
                     VALUES (?1, ?2, 'agent', ?3, ?4, ?4 + 10000000, 100, 0.05, 'DEFAULT', 'task')",
                    rusqlite::params![agent_obs_id, trace_id, agent_name, trace_time],
                )
                .unwrap();

            conn_large
                .execute(
                    "INSERT INTO observations (id, trace_id, parent_id, type, name, start_ns, end_ns, total_tokens, total_cost_usd, level)
                     VALUES (?1, ?2, ?3, 'tool', 'Bash', ?4, ?4 + 5000000, 10, 0.01, 'DEFAULT')",
                    rusqlite::params![format!("c_large_{t}"), trace_id, agent_obs_id, trace_time],
                )
                .unwrap();
        }
    }
    conn_large.execute_batch("COMMIT;").unwrap();

    // 3. Authorizer setup and small build
    let small_selects = Arc::new(AtomicUsize::new(0));
    let small_selects_clone = Arc::clone(&small_selects);
    let _ = conn_small.authorizer(Some(move |ctx: AuthContext<'_>| {
        if matches!(ctx.action, AuthAction::Select) {
            small_selects_clone.fetch_add(1, Ordering::SeqCst);
        }
        Authorization::Allow
    }));

    let inputs_small = DossierInputs {
        conn: &conn_small,
        db_path: &db_small,
        workspace: &ws_small,
        home: &home_small,
        live_sessions: &[],
        live_snapshot_available: false,
        as_of,
    };
    let _small_dossier = build_dossier(inputs_small, DossierConfig::default()).unwrap();
    let _ = conn_small.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);

    // 4. Authorizer setup and large build
    let large_selects = Arc::new(AtomicUsize::new(0));
    let large_selects_clone = Arc::clone(&large_selects);
    let _ = conn_large.authorizer(Some(move |ctx: AuthContext<'_>| {
        if matches!(ctx.action, AuthAction::Select) {
            large_selects_clone.fetch_add(1, Ordering::SeqCst);
        }
        Authorization::Allow
    }));

    let inputs_large = DossierInputs {
        conn: &conn_large,
        db_path: &db_large,
        workspace: &ws_large,
        home: &home_large,
        live_sessions: &[],
        live_snapshot_available: false,
        as_of,
    };
    let dossier = build_dossier(inputs_large, DossierConfig::default()).unwrap();
    let _ = conn_large.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);

    println!(
        "Large dossier build time: {} ms (limit target: <500ms)",
        dossier.health.total_build_ms
    );
    println!(
        "Small selects: {}, Large selects: {}",
        small_selects.load(Ordering::SeqCst),
        large_selects.load(Ordering::SeqCst)
    );

    let bytes = serde_json::to_vec(&dossier).unwrap();
    assert!(bytes.len() <= 65_536);
    assert!(dossier.sessions.data.sessions.len() <= 20);
    assert!(dossier.skills.data.skills.len() <= 30);
    assert!(dossier.agents.data.agents.len() <= 30);
    assert_eq!(dossier.skills.total_matching, 100);
    assert_eq!(dossier.agents.total_matching, 100);
    assert_eq!(
        large_selects.load(Ordering::SeqCst),
        small_selects.load(Ordering::SeqCst)
    );
}
