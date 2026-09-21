#[path = "support/analysis.rs"]
mod support;

use agent_mux::tracing::analysis::correlation::resolve_binding;
use agent_mux::tracing::analysis::evidence::snippet;
use agent_mux::tracing::analysis::metrics::{completed_percentiles, ttft_ms};
use agent_mux::tracing::analysis::model::*;

#[test]
fn generation_duration_is_not_ttft() {
    assert_eq!(ttft_ms(Some(1_000_000), None), None);
    assert_eq!(ttft_ms(Some(1_000_000), Some(4_000_000)), Some(3));
    assert_eq!(
        completed_percentiles(&[None, Some(10), Some(30)]),
        Some((10, 30, 30, 2))
    );
}

#[test]
fn snippets_are_unicode_safe() {
    assert_eq!(snippet("á🦀日本語", 3), "á🦀日…");
    assert_eq!(snippet("ok", 3), "ok");
}

#[test]
fn absence_of_exact_identity_stays_uncorrelated() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE launches(id TEXT, run_id TEXT, agent_mux_session INTEGER, session_key TEXT, started_ns INTEGER);
         INSERT INTO launches VALUES('old','previous-run',1,'claude:old',1);",
    )
    .unwrap();
    assert!(
        resolve_binding(&db, "current-run", 1, None, None)
            .unwrap()
            .is_none()
    );
}

#[test]
fn exact_launch_id_binds_immediately() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE launches(id TEXT, run_id TEXT, agent_mux_session INTEGER, session_key TEXT, started_ns INTEGER);
         INSERT INTO launches VALUES('launch-1','run-1',1,'claude:s1',100);",
    )
    .unwrap();

    let binding = resolve_binding(&db, "run-1", 1, Some("launch-1"), None)
        .unwrap()
        .unwrap();
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

    let binding = resolve_binding(&db, "run-1", 1, None, Some("claude:s1"))
        .unwrap()
        .unwrap();
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

    let binding = resolve_binding(&db, "run-1", 1, None, None)
        .unwrap()
        .unwrap();
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

    let b1 = resolve_binding(&db, "run-1", 1, None, None)
        .unwrap()
        .unwrap();
    assert_eq!(b1.launch_id, "run1-l1");
    assert_eq!(b1.session_key.as_deref(), Some("claude:r1"));

    let b2 = resolve_binding(&db, "run-2", 1, None, None)
        .unwrap()
        .unwrap();
    assert_eq!(b2.launch_id, "run2-l1");
    assert_eq!(b2.session_key.as_deref(), Some("claude:r2"));
}

#[test]
fn briefing_preserves_distinct_relative_paths() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        r#"
        CREATE TABLE sessions (key TEXT PRIMARY KEY, provider TEXT, cwd TEXT, first_seen_ns INTEGER, last_seen_ns INTEGER);
        CREATE TABLE traces (id TEXT PRIMARY KEY, session_key TEXT, launch_id TEXT, start_ns INTEGER, end_ns INTEGER, input TEXT, output TEXT);
        CREATE TABLE observations (id TEXT PRIMARY KEY, trace_id TEXT, type TEXT, name TEXT, input TEXT, output TEXT, start_ns INTEGER, end_ns INTEGER);

        INSERT INTO sessions VALUES('s1', 'claude', '/my/workspace', 100, 200);
        INSERT INTO traces VALUES('t1', 's1', 'l1', 100, 200, 'Refactor configs', 'All done');
        INSERT INTO observations VALUES('o1', 't1', 'tool', 'write_to_file', '{"TargetFile": "src/a/config.rs"}', 'ok', 110, 120);
        INSERT INTO observations VALUES('o2', 't1', 'tool', 'write_to_file', '{"TargetFile": "src/b/config.rs"}', 'ok', 130, 140);
        INSERT INTO observations VALUES('o3', 't1', 'tool', 'execute_command', '{"CommandLine": "cargo test --workspace"}', 'ok', 150, 160);
        "#,
    ).unwrap();

    let rep = agent_mux::tracing::analysis::query::briefing(
        &db,
        std::path::Path::new("/my/workspace"),
        0,
        1000,
        &[],
        &agent_mux::tracing::analysis::BriefingScan::all(),
    )
    .unwrap();

    assert_eq!(rep.total_sessions, 1);
    let card = &rep.cards[0];
    assert_eq!(card.files_modified.len(), 2);
    let paths: Vec<_> = card
        .files_modified
        .iter()
        .map(|f| f.value.as_deref().unwrap())
        .collect();
    assert!(paths.contains(&"src/a/config.rs"));
    assert!(paths.contains(&"src/b/config.rs"));
    assert_eq!(card.recent_commands.len(), 1);
    assert_eq!(
        card.recent_commands[0].value.as_deref().unwrap(),
        "cargo test --workspace"
    );
}

#[test]
fn briefing_with_live_session_merges_activity() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    let live = vec![LiveSession {
        run_id: "run-1".into(),
        launch_id: "launch-1".into(),
        session_id: 1,
        session_key: Some("s-live".into()),
        provider: Some("claude".into()),
        cwd: std::path::PathBuf::from("/my/workspace"),
        state: RuntimeState::Working,
        updated_at_ns: 500,
        active_tools: vec!["cargo test".into()],
    }];

    let rep = agent_mux::tracing::analysis::query::briefing(
        &db,
        std::path::Path::new("/my/workspace"),
        0,
        1000,
        &live,
        &agent_mux::tracing::analysis::BriefingScan::all(),
    )
    .unwrap();

    assert_eq!(rep.total_sessions, 1);
    let card = &rep.cards[0];
    assert_eq!(card.runtime_state, RuntimeState::Working);
    assert!(
        card.current_activity
            .value
            .as_deref()
            .unwrap()
            .contains("cargo test")
    );
}

#[test]
fn analyze_skills_computes_attribution_and_percentiles() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        r#"
        CREATE TABLE observations (
            id TEXT PRIMARY KEY,
            trace_id TEXT,
            type TEXT,
            name TEXT,
            skill TEXT,
            input TEXT,
            output TEXT,
            start_ns INTEGER,
            end_ns INTEGER,
            level TEXT NOT NULL DEFAULT 'DEFAULT',
            total_tokens INTEGER,
            total_cost_usd REAL,
            status_message TEXT
        );

        INSERT INTO observations VALUES('o1', 't1', 'tool', 'cargo_test', 'rust-dev', '{}', 'ok', 100_000_000, 200_000_000, 'DEFAULT', 100, 0.01, NULL);
        INSERT INTO observations VALUES('o2', 't1', 'tool', 'cargo_test', 'rust-dev', '{}', 'err', 100_000_000, 300_000_000, 'ERROR', 150, 0.02, 'JSON Schema error: invalid input');
        "#,
    ).unwrap();

    let metrics =
        agent_mux::tracing::analysis::metrics::analyze_skills(&db, Some("rust-dev"), None, None)
            .unwrap();
    assert_eq!(metrics.len(), 1);
    let row = &metrics[0];
    assert_eq!(row.skill_name, "rust-dev");
    assert_eq!(row.attributed_calls, 2);
    assert_eq!(row.error_count, 1);
    assert_eq!(row.schema_error_count, 1);
    assert_eq!(row.attributed_tokens, Some(250));
    assert_eq!(row.sample_size, 2);
    assert_eq!(row.p50_ms, Some(100));
    assert_eq!(row.max_ms, Some(200));
}

/// The briefing caps the cards it builds and filters by provider in SQL,
/// and the rollup it reports stays exact whatever the cap.
#[test]
fn briefing_caps_the_cards_but_not_the_totals() {
    use agent_mux::tracing::analysis::BriefingScan;

    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        r#"
        CREATE TABLE sessions (key TEXT PRIMARY KEY, provider TEXT, cwd TEXT, first_seen_ns INTEGER, last_seen_ns INTEGER);
        CREATE TABLE traces (id TEXT PRIMARY KEY, session_key TEXT, launch_id TEXT, start_ns INTEGER, end_ns INTEGER, input TEXT, output TEXT);
        CREATE TABLE observations (id TEXT PRIMARY KEY, trace_id TEXT, type TEXT, name TEXT, input TEXT, output TEXT, start_ns INTEGER, end_ns INTEGER);
        CREATE VIEW session_stats AS
          SELECT s.*,
                 COUNT(t.id) AS turn_count,
                 0 AS open_turns,
                 COUNT(o.id) AS tool_count,
                 100 AS total_tokens,
                 0.5 AS total_cost_usd
          FROM sessions s
          LEFT JOIN traces t ON t.session_key = s.key
          LEFT JOIN observations o ON o.trace_id = t.id
          GROUP BY s.key;

        INSERT INTO sessions VALUES('s1', 'claude', '/w', 100, 500);
        INSERT INTO sessions VALUES('s2', 'claude', '/w', 100, 400);
        INSERT INTO sessions VALUES('s3', 'codex',  '/w', 100, 300);
        INSERT INTO sessions VALUES('s4', 'codex',  '/w', 100, 200);
        INSERT INTO sessions VALUES('s5', 'agy',    '/w', 100, 150);
        INSERT INTO traces VALUES('t1', 's1', 'l1', 100, 200, 'one', 'done');
        INSERT INTO traces VALUES('t2', 's2', 'l2', 100, 200, 'two', 'done');
        INSERT INTO traces VALUES('t3', 's3', 'l3', 100, 200, 'three', 'done');
        INSERT INTO traces VALUES('t4', 's4', 'l4', 100, 200, 'four', 'done');
        INSERT INTO traces VALUES('t5', 's5', 'l5', 100, 200, 'five', 'done');
        "#,
    )
    .unwrap();
    let ws = std::path::Path::new("/w");

    // uncapped: every session gets a card
    let all =
        agent_mux::tracing::analysis::query::briefing(&db, ws, 0, 1000, &[], &BriefingScan::all())
            .unwrap();
    assert_eq!(all.cards.len(), 5);
    assert_eq!(all.total_sessions, 5);
    assert!(all.warnings.is_empty(), "{:?}", all.warnings);

    // capped: two cards, the two most recent, and the totals still cover five
    let capped = agent_mux::tracing::analysis::query::briefing(
        &db,
        ws,
        0,
        1000,
        &[],
        &BriefingScan::with_max(2),
    )
    .unwrap();
    assert_eq!(capped.cards.len(), 2);
    let keys: Vec<&str> = capped
        .cards
        .iter()
        .filter_map(|c| c.session_key.as_deref())
        .collect();
    assert_eq!(keys, vec!["s1", "s2"], "the most recent sessions");
    assert_eq!(capped.total_sessions, 5, "the rollup is not capped");
    assert_eq!(capped.total_turns, all.total_turns);
    assert_eq!(capped.total_tools, all.total_tools);
    assert_eq!(capped.total_tokens, all.total_tokens);
    assert!(
        capped
            .warnings
            .iter()
            .any(|w| w.contains("5 sessions in scope")),
        "the cap is reported: {:?}",
        capped.warnings
    );

    // the provider filter reaches the SQL, so it also bounds the rollup
    let codex = agent_mux::tracing::analysis::query::briefing(
        &db,
        ws,
        0,
        1000,
        &[],
        &BriefingScan {
            provider: Some("codex".into()),
            ..BriefingScan::all()
        },
    )
    .unwrap();
    assert_eq!(codex.cards.len(), 2);
    assert_eq!(codex.total_sessions, 2);
    assert!(
        codex
            .cards
            .iter()
            .all(|c| c.provider.as_deref() == Some("codex"))
    );

    // one key, one card: what a session-detail request asks for
    let one = agent_mux::tracing::analysis::query::briefing(
        &db,
        ws,
        0,
        1000,
        &[],
        &BriefingScan {
            session_key: Some("s4".into()),
            max_cards: 1,
            ..BriefingScan::all()
        },
    )
    .unwrap();
    assert_eq!(one.cards.len(), 1);
    assert_eq!(one.cards[0].session_key.as_deref(), Some("s4"));
    assert_eq!(one.total_sessions, 1);
}
