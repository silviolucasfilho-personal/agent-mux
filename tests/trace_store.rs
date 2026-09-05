//! The writer thread against a real tempdir store: batches commit and are
//! visible through a read-only connection and the views; the shutdown
//! `finish` honors its deadline even while another connection holds the
//! write lock; the commit hook sees the launches a batch touched.

use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::model::{
    LaunchRow, Level, ObservationRow, ObservationType, StoreOp, TraceRow, TraceStatus,
};
use agent_mux::tracing::store::writer::{WriterConfig, spawn_writer};
use agent_mux::tracing::store::{OpenOptions, open_ro, open_rw};
use agent_mux::tracing::usage::NormalizedUsage;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn store(dir: &Path, run: &str) -> agent_mux::tracing::store::Store {
    open_rw(
        &dir.join("traces.db"),
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: run.into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap()
}

fn launch(id: &str) -> LaunchRow {
    LaunchRow {
        id: id.into(),
        run_id: "run-1".into(),
        agent_mux_session: 1,
        profile: "Claude Code".into(),
        provider: "claude".into(),
        cwd: "/proj".into(),
        project_slug: "-proj".into(),
        content_mode: "full".into(),
        correlation_plan: "deterministic".into(),
        correlation: Some("deterministic".into()),
        session_key: Some("claude:s1".into()),
        injected_session_id: true,
        attached: false,
        started_ns: 1_000,
        ended_ns: None,
        termination: None,
        exit_code: None,
        parse_errors: None,
        dropped_ops: None,
        reported_cost_usd: None,
        reported_lines_added: None,
        reported_lines_removed: None,
        agent_mux_version: "test".into(),
        user_id: None,
        release: None,
        environment: None,
        tags: vec![],
        metadata: None,
    }
}

fn trace(id: &str, ordinal: i64) -> TraceRow {
    TraceRow {
        id: id.into(),
        session_key: "claude:s1".into(),
        provider: "claude".into(),
        session_id: "s1".into(),
        launch_id: Some("l1".into()),
        ordinal,
        name: format!("turn {ordinal}"),
        status: TraceStatus::Closed,
        start_ns: 1_000 * ordinal,
        end_ns: Some(1_000 * ordinal + 500),
        input: Some("hello there".into()),
        output: Some("general kenobi".into()),
        thinking: None,
        skills: None,
        reported_duration_ms: None,
        reported_message_count: None,
        session_cost_usd: None,
        timing_approx: false,
        ordinal_salted: false,
        metadata: None,
    }
}

fn generation(id: &str, trace_id: &str) -> ObservationRow {
    ObservationRow {
        id: id.into(),
        trace_id: trace_id.into(),
        parent_id: None,
        obs_type: ObservationType::Generation,
        name: "assistant".into(),
        kind: None,
        start_ns: 1_100,
        end_ns: Some(1_400),
        level: Level::Default,
        status_message: None,
        model: Some("claude-haiku-4-5-20251001".into()),
        input: None,
        output: Some("general kenobi".into()),
        thinking: None,
        usage_raw: Some(vec![
            ("input_tokens".into(), 1_000_000),
            ("output_tokens".into(), 200_000),
        ]),
        usage: Some(NormalizedUsage {
            input: Some(1_000_000),
            output: Some(200_000),
            total: Some(1_200_000),
            ..Default::default()
        }),
        tool_id: None,
        tool_name: None,
        skill: None,
        mcp_server: None,
        path: None,
        is_error: false,
        ts_approx: false,
        metadata: serde_json::Map::new(),
    }
}

fn wait_for<F: Fn() -> bool>(pred: F, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    pred()
}

#[test]
fn batches_commit_and_are_visible_to_readers_and_views() {
    let dir = tempfile::tempdir().unwrap();
    let touched: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let touched_hook = Arc::clone(&touched);
    let handle = spawn_writer(
        store(dir.path(), "run-1"),
        WriterConfig::new(30),
        Box::new(|_, _| {}),
        Some(Box::new(move |_store, launches| {
            touched_hook
                .lock()
                .unwrap()
                .extend(launches.iter().cloned());
        })),
    );
    handle.tx.try_send(StoreOp::Launch(launch("l1"))).unwrap();
    handle.tx.try_send(StoreOp::Trace(trace("t1", 1))).unwrap();
    handle
        .tx
        .try_send(StoreOp::Observation(generation("g1", "t1")))
        .unwrap();
    handle.tx.try_send(StoreOp::Trace(trace("t2", 2))).unwrap();
    let db = dir.path().join("traces.db");
    let committed = wait_for(
        || {
            open_ro(&db).ok().and_then(|c| {
                c.query_row("SELECT COUNT(*) FROM traces", [], |r| r.get::<_, i64>(0))
                    .ok()
            }) == Some(2)
        },
        Duration::from_secs(5),
    );
    assert!(committed, "writer never committed");
    let conn = open_ro(&db).unwrap();
    // views: session rollup with the haiku price ($1 in, $5 out per 1M)
    let (turns, cost, tokens): (i64, f64, i64) = conn
        .query_row(
            "SELECT turn_count, total_cost_usd, total_tokens FROM session_stats WHERE key = 'claude:s1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(turns, 2);
    assert!((cost - 2.0).abs() < 1e-9, "{cost}");
    assert_eq!(tokens, 1_200_000);
    // the query layer sees the same thing
    let sessions = agent_mux::tracing::store::query::list_sessions(
        &conn,
        &agent_mux::tracing::store::query::SessionFilter {
            project_slug: None,
            since_ns: None,
            limit: 10,
        },
    )
    .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "s1");
    let hits = agent_mux::tracing::store::query::search(&conn, "kenobi", 10).unwrap();
    assert!(!hits.is_empty(), "FTS should find the output");
    // the commit hook saw the launch
    assert!(wait_for(
        || touched.lock().unwrap().contains(&"l1".to_string()),
        Duration::from_secs(2)
    ));
    // clean shutdown ends the run
    assert!(handle.finish(Duration::from_secs(5)));
    let ended: Option<i64> = conn
        .query_row("SELECT ended_ns FROM runs WHERE id = 'run-1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(ended.is_some(), "end_run must stamp the run");
}

#[test]
fn finish_honors_the_deadline_while_the_write_lock_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("traces.db");
    let handle = spawn_writer(
        store(dir.path(), "run-1"),
        WriterConfig {
            busy_backoff: Duration::from_millis(10),
            ..WriterConfig::new(20)
        },
        Box::new(|_, _| {}),
        None,
    );
    // another process holding the write lock
    let blocker = rusqlite::Connection::open(&db).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
    handle.tx.try_send(StoreOp::Launch(launch("l1"))).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    let finished = handle.finish(Duration::from_millis(400));
    assert!(!finished, "cannot finish while the lock is held");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "finish overstayed its deadline: {:?}",
        started.elapsed()
    );
    blocker.execute_batch("ROLLBACK;").unwrap();
}

/// A stored session reads back as the ops that produced it, in emission
/// order, so it can be replayed through another sink.
#[test]
fn stored_sessions_read_back_as_ops_for_replay() {
    let dir = tempfile::tempdir().unwrap();
    let handle = spawn_writer(
        store(dir.path(), "run-1"),
        WriterConfig::new(30),
        Box::new(|_, _| {}),
        None,
    );
    handle.tx.try_send(StoreOp::Launch(launch("l1"))).unwrap();
    handle.tx.try_send(StoreOp::Trace(trace("t1", 1))).unwrap();
    handle
        .tx
        .try_send(StoreOp::Observation(generation("g1", "t1")))
        .unwrap();
    handle.tx.try_send(StoreOp::Trace(trace("t2", 2))).unwrap();
    assert!(handle.finish(Duration::from_secs(5)));
    let conn = open_ro(&dir.path().join("traces.db")).unwrap();
    let ops = agent_mux::tracing::store::read_session_ops(&conn, "claude:s1").unwrap();
    assert_eq!(ops.len(), 4, "{ops:?}");
    let StoreOp::Session(s) = &ops[0] else {
        panic!("session first: {:?}", ops[0]);
    };
    assert_eq!(s.key, "claude:s1");
    assert_eq!(s.provider, "claude");
    let StoreOp::Trace(t1) = &ops[1] else {
        panic!("{:?}", ops[1]);
    };
    let expected = trace("t1", 1);
    assert_eq!(t1.id, expected.id);
    assert_eq!(t1.ordinal, 1);
    assert_eq!(t1.name, expected.name);
    assert_eq!(t1.status, expected.status);
    assert_eq!(t1.start_ns, expected.start_ns);
    assert_eq!(t1.end_ns, expected.end_ns);
    assert_eq!(t1.input, expected.input);
    assert_eq!(t1.session_id, "s1");
    let StoreOp::Observation(g) = &ops[2] else {
        panic!("{:?}", ops[2]);
    };
    let expected = generation("g1", "t1");
    assert_eq!(g.id, expected.id);
    assert_eq!(g.trace_id, expected.trace_id);
    assert_eq!(g.obs_type, expected.obs_type);
    assert_eq!(g.model, expected.model);
    assert_eq!(
        g.usage, expected.usage,
        "normalized usage survives the round trip"
    );
    assert_eq!(g.output, expected.output);
    let StoreOp::Trace(t2) = &ops[3] else {
        panic!("{:?}", ops[3]);
    };
    assert_eq!(t2.ordinal, 2);
    assert!(
        agent_mux::tracing::store::read_session_ops(&conn, "claude:missing").is_err(),
        "unknown sessions are an error, not an empty replay"
    );
}
