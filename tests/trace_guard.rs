//! The budget guard end to end through the hook command: a launch whose
//! row carries a guard, spend in the store, and a `PreToolUse` registered
//! with `--guard` that refuses past the limit — and permits everywhere
//! else (no guard, under the limit, another event, no launch, no store).

use agent_mux::config;
use agent_mux::tracing::cli::hook_run;
use agent_mux::tracing::hooks::guard::{Guard, Verdict, check};
use agent_mux::tracing::pricing::PriceTable;
use agent_mux::tracing::store::model::{
    LaunchRow, Level, ObservationRow, ObservationType, StoreOp, TraceRow, TraceStatus,
};
use agent_mux::tracing::store::writer::{WriterConfig, spawn_writer};
use agent_mux::tracing::store::{OpenOptions, open_ro, open_rw};
use agent_mux::tracing::usage::NormalizedUsage;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn launch(id: &str, guard: Option<Guard>) -> LaunchRow {
    LaunchRow {
        id: id.into(),
        run_id: "run-g".into(),
        agent_mux_session: 1,
        profile: "Claude Code".into(),
        provider: "claude".into(),
        cwd: "/proj".into(),
        project_slug: "-proj".into(),
        content_mode: "full".into(),
        correlation_plan: "deterministic".into(),
        correlation: Some("deterministic".into()),
        session_key: Some(format!("claude:{id}")),
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
        metadata: guard.map(|g| serde_json::json!({ "guard": g.to_json() })),
    }
}

fn trace(id: &str, ordinal: i64, launch_id: &str) -> TraceRow {
    TraceRow {
        id: id.into(),
        session_key: format!("claude:{launch_id}"),
        provider: "claude".into(),
        session_id: launch_id.into(),
        launch_id: Some(launch_id.into()),
        ordinal,
        name: format!("turn {ordinal}"),
        status: TraceStatus::Closed,
        start_ns: 1_000_000_000 * ordinal,
        end_ns: Some(1_000_000_000 * ordinal + 500),
        input: Some("go".into()),
        output: Some("ok".into()),
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

/// A priced generation: five million haiku input tokens is dollars.
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
        model: Some("claude-haiku-4-5".into()),
        input: None,
        output: Some("ok".into()),
        thinking: None,
        usage_raw: Some(vec![("input_tokens".into(), 5_000_000)]),
        usage: Some(NormalizedUsage {
            input: Some(5_000_000),
            output: Some(0),
            total: Some(5_000_000),
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

/// Three launches: `costly` guarded at $0.50 with dollars spent, `turny`
/// guarded at one turn with two, `free` unguarded with the same spend.
fn seeded_store(dir: &Path) -> PathBuf {
    let db = dir.join("traces.db");
    let store = open_rw(
        &db,
        OpenOptions {
            prices: PriceTable::builtin(),
            run_id: "run-g".into(),
            retention_days: 0,
            agent_mux_version: "test".into(),
        },
    )
    .unwrap();
    let handle = spawn_writer(store, WriterConfig::new(30), Box::new(|_, _| {}), None);
    let send = |op: StoreOp| handle.tx.try_send(op).unwrap();
    send(StoreOp::Launch(launch(
        "costly",
        Some(Guard {
            max_cost_usd: Some(0.5),
            max_turns: None,
        }),
    )));
    send(StoreOp::Trace(trace("c1", 1, "costly")));
    send(StoreOp::Observation(generation("cg1", "c1")));
    send(StoreOp::Launch(launch(
        "turny",
        Some(Guard {
            max_cost_usd: None,
            max_turns: Some(1),
        }),
    )));
    send(StoreOp::Trace(trace("t1", 1, "turny")));
    send(StoreOp::Trace(trace("t2", 2, "turny")));
    send(StoreOp::Launch(launch("free", None)));
    send(StoreOp::Trace(trace("f1", 1, "free")));
    send(StoreOp::Observation(generation("fg1", "f1")));
    assert!(handle.finish(Duration::from_secs(5)));
    db
}

#[test]
fn the_guard_reads_the_launch_row_and_its_spend() {
    let temp = tempfile::tempdir().unwrap();
    let db = seeded_store(temp.path());
    let budget = Duration::from_millis(150);
    let conn = open_ro(&db).unwrap();
    let spent: f64 = conn
        .query_row(
            "SELECT SUM(o.total_cost_usd) FROM observations o JOIN traces t ON t.id = o.trace_id WHERE t.launch_id = 'costly'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(spent > 0.5, "haiku tokens are priced: {spent}");

    match check(&db, "costly", budget) {
        Verdict::Block(reason) => {
            assert!(reason.contains("over the $0.50 limit"), "{reason}");
            assert!(reason.starts_with("agent-mux budget: $"), "{reason}");
        }
        other => panic!("expected a block, got {other:?}"),
    }
    match check(&db, "turny", budget) {
        Verdict::Block(reason) => assert_eq!(
            reason,
            "agent-mux budget: turn 2, over the 1-turn limit for this launch"
        ),
        other => panic!("expected a block, got {other:?}"),
    }
    assert_eq!(check(&db, "free", budget), Verdict::Permit, "no guard");
    assert_eq!(check(&db, "nope", budget), Verdict::Permit, "no launch");
}

const PRE: &str = r#"{"session_id":"sess-g","prompt_id":"p1","transcript_path":"/tmp/t.jsonl","cwd":"/proj","hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":"TOOL","tool_input":{"command":"ls"}}"#;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_hook_refuses_only_a_guarded_pre_tool_use_past_the_limit() {
    let temp = tempfile::tempdir().unwrap();
    let db = seeded_store(temp.path());
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".agent-mux")).unwrap();
    std::fs::write(
        home.join(".agent-mux").join("profiles.toml"),
        format!("[tracing]\ndb_path = \"{}\"\n", db.display()),
    )
    .unwrap();
    let db_s = db.to_string_lossy().into_owned();
    let home_s = home.to_string_lossy().into_owned();
    let payload = |id: &str| PRE.replace("TOOL", id);

    // over budget, registered with --guard: refused, and the model sees why
    let out = hook_run(
        &args(&[
            "claude", "--db", &db_s, "--home", &home_s, "--launch", "costly", "--guard",
        ]),
        Some(&payload("toolu_1")),
    );
    assert!(out.inserted, "{:?}", out.error);
    let reason = out.blocked.clone().expect("blocked");
    assert!(reason.contains("over the $0.50 limit"), "{reason}");
    let reply: serde_json::Value = serde_json::from_str(out.response.as_deref().unwrap()).unwrap();
    assert_eq!(reply["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(
        reply["hookSpecificOutput"]["permissionDecisionReason"],
        reason
    );
    // the stored event says so, for the pipeline's notice and the turn flag
    let conn = open_ro(&db).unwrap();
    let stored: String = conn
        .query_row(
            "SELECT payload FROM hook_events WHERE tool_use_id = 'toolu_1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["agent_mux_guard"]["blocked"], true);
    assert_eq!(stored["agent_mux_guard"]["reason"], reason);

    // the same launch without --guard: recorded, never refused
    let out = hook_run(
        &args(&[
            "claude", "--db", &db_s, "--home", &home_s, "--launch", "costly",
        ]),
        Some(&payload("toolu_2")),
    );
    assert!(out.inserted);
    assert!(out.blocked.is_none());
    assert!(out.response.is_none());
    // an unguarded launch with --guard: permitted
    let out = hook_run(
        &args(&[
            "claude", "--db", &db_s, "--home", &home_s, "--launch", "free", "--guard",
        ]),
        Some(&payload("toolu_3")),
    );
    assert!(out.inserted);
    assert!(out.blocked.is_none());
    // a PostToolUse on the guarded launch: never refused (nothing to refuse)
    let post = payload("toolu_4").replace("PreToolUse", "PostToolUse");
    let out = hook_run(
        &args(&[
            "claude", "--db", &db_s, "--home", &home_s, "--launch", "costly", "--guard",
        ]),
        Some(&post),
    );
    assert!(out.inserted);
    assert!(out.blocked.is_none());
    // the launch id can come from the environment the CLI inherited
    let out = hook_run(
        &args(&["claude", "--db", &db_s, "--home", &home_s, "--guard"]),
        Some(&payload("toolu_5")),
    );
    assert!(out.blocked.is_none(), "no launch id anywhere: permit");
}

#[test]
fn guard_limits_and_loop_thresholds_come_from_the_config() {
    let toml = r#"
        [tracing]
        db_path = "/tmp/x.db"

        [tracing.loops]
        tool_storm = 5
        no_progress = 0

        [[profiles]]
        name = "Claude Code"
        command = "claude"

        [profiles.tracing]
        max_cost_usd = 2.5
        max_turns = 40

        [[profiles]]
        name = "Codex"
        command = "codex"
    "#;
    let cfg = config::parse(toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    assert_eq!(resolved.loops.tool_storm, 5);
    assert_eq!(resolved.loops.ping_pong, 6, "default kept");
    assert_eq!(resolved.loops.no_progress, 0, "disabled");
    let guard = Guard::from_tracing(cfg.profiles[0].tracing.as_ref()).unwrap();
    assert_eq!(guard.max_cost_usd, Some(2.5));
    assert_eq!(guard.max_turns, Some(40));
    assert_eq!(guard.describe(), "max $2.50, 40 turns");
    assert_eq!(Guard::from_tracing(cfg.profiles[1].tracing.as_ref()), None);
    // no section at all: defaults
    let plain = config::parse("[tracing]\ndb_path = \"/tmp/x.db\"\n").unwrap();
    let resolved = config::resolve_tracing(plain.tracing.as_ref(), &|_| None).unwrap();
    assert_eq!(
        resolved.loops,
        agent_mux::tracing::loops::LoopThresholds::default()
    );
}
