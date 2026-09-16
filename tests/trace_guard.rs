//! The budget guard end to end through the hook command: a launch whose
//! row carries a guard, spend in the store, and a `PreToolUse` registered
//! with `--guard` that refuses past the limit — and permits everywhere
//! else (no guard, under the limit, another event, no launch, no store).

use agent_mux::config;
use agent_mux::loops::{Level as LoopLevel, LoopLaunch, LoopPolicy};
use agent_mux::tracing::cli::hook_run;
use agent_mux::tracing::hooks::guard::{Guard, Verdict, check, check_tool};
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

/// A loop launch row: the policy the guard enforces, next to a budget.
fn loop_launch(id: &str, policy: &LoopPolicy, guard: Option<Guard>) -> LaunchRow {
    let mut row = launch(id, guard);
    let mut meta = row
        .metadata
        .take()
        .and_then(|m| m.as_object().cloned())
        .unwrap_or_default();
    meta.insert("loop_id".into(), serde_json::json!("L1"));
    meta.insert(
        "loop_run_id".into(),
        serde_json::json!("2026-09-16T08:00:00Z"),
    );
    meta.insert("loop_pattern".into(), serde_json::json!("ci-sweeper"));
    meta.insert("loop_level".into(), serde_json::json!("L2"));
    meta.insert("loop_workspace".into(), serde_json::json!("/ws"));
    meta.insert("loop_policy".into(), serde_json::to_value(policy).unwrap());
    row.metadata = Some(serde_json::Value::Object(meta));
    row
}

fn write_obs(id: &str, trace_id: &str, tool: &str, path: &str) -> ObservationRow {
    ObservationRow {
        id: id.into(),
        trace_id: trace_id.into(),
        parent_id: None,
        obs_type: ObservationType::Tool,
        name: tool.into(),
        kind: None,
        start_ns: 1_100,
        end_ns: Some(1_200),
        level: Level::Default,
        status_message: None,
        model: None,
        input: Some(serde_json::json!({ "file_path": path }).to_string()),
        output: None,
        thinking: None,
        usage_raw: None,
        usage: None,
        tool_id: None,
        tool_name: Some(tool.into()),
        skill: None,
        mcp_server: None,
        path: None,
        ts_approx: false,
        metadata: serde_json::Map::new(),
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

fn assisted_policy() -> LoopPolicy {
    LoopPolicy {
        report_only: false,
        reason: None,
        state_file: "ci-sweeper-state.md".into(),
        run_log: "loop-run-log.md".into(),
        denylist: vec![
            "**/.env".into(),
            "**/secrets/**".into(),
            "**/migrations/**".into(),
        ],
        max_files: Some(2),
        worktree: Some("/ws/.loop-worktrees/r1".into()),
    }
}

fn tool(name: &str, input: serde_json::Value) -> (String, serde_json::Value) {
    (name.to_string(), input)
}

/// One store with an assisted loop launch (two files already written), a
/// report-only launch, and a loop launch that is also over its budget.
fn loop_store(dir: &Path) -> PathBuf {
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
    send(StoreOp::Launch(loop_launch(
        "assisted",
        &assisted_policy(),
        None,
    )));
    send(StoreOp::Trace(trace("a1", 1, "assisted")));
    send(StoreOp::Observation(write_obs(
        "aw1",
        "a1",
        "Write",
        "src/one.rs",
    )));
    send(StoreOp::Observation(write_obs(
        "aw2",
        "a1",
        "Edit",
        "/ws/.loop-worktrees/r1/src/two.rs",
    )));
    let report = LoopPolicy {
        report_only: true,
        reason: Some("tokens today at 84% of cap".into()),
        max_files: None,
        worktree: None,
        ..assisted_policy()
    };
    send(StoreOp::Launch(loop_launch("report", &report, None)));
    send(StoreOp::Launch(loop_launch(
        "looped-costly",
        &assisted_policy(),
        Some(Guard {
            max_cost_usd: Some(0.5),
            max_turns: None,
        }),
    )));
    send(StoreOp::Trace(trace("lc1", 1, "looped-costly")));
    send(StoreOp::Observation(generation("lcg1", "lc1")));
    assert!(handle.finish(Duration::from_secs(5)));
    db
}

#[test]
fn the_loop_policy_on_the_launch_row_gates_every_write() {
    let temp = tempfile::tempdir().unwrap();
    let db = loop_store(temp.path());
    let b = Duration::from_millis(150);
    let verdict = |launch: &str, (name, input): (String, serde_json::Value)| {
        check_tool(&db, launch, b, Some(&name), Some(&input), true)
    };
    let block = |v: Verdict| match v {
        Verdict::Block(r) => r,
        Verdict::Permit => panic!("expected a block"),
    };
    // 1. pushing and merging are human gates, whatever the level
    for cmd in [
        "git push origin HEAD",
        "gh pr merge 3",
        "git merge main",
        "git rebase -i main",
    ] {
        let r = block(verdict(
            "assisted",
            tool("Bash", serde_json::json!({"command": cmd})),
        ));
        assert_eq!(
            r, "agent-mux loop: pushing and merging are human gates",
            "{cmd}"
        );
    }
    // the Codex shell tool spells the command as argv
    let r = block(verdict(
        "assisted",
        tool("shell", serde_json::json!({"command": ["git", "push"]})),
    ));
    assert!(r.contains("human gates"));
    assert_eq!(
        verdict(
            "assisted",
            tool("Bash", serde_json::json!({"command": "cargo test"}))
        ),
        Verdict::Permit
    );
    // 2. the denylist, dotfiles and nested directories included
    for path in [
        ".env",
        "config/.env",
        ".secrets/prod.json",
        "db/migrations/001.sql",
    ] {
        let r = block(verdict(
            "assisted",
            tool("Write", serde_json::json!({"file_path": path})),
        ));
        assert!(r.contains("gate.yaml denylist"), "{path}: {r}");
    }
    let r = block(verdict(
        "assisted",
        tool(
            "apply_patch",
            serde_json::json!({"input": "*** Begin Patch\n*** Update File: app/secrets/key.txt\n*** End Patch"}),
        ),
    ));
    assert!(r.contains("app/secrets/key.txt"), "{r}");
    // 3. report-only: only the state file and the run log may change
    let r = block(verdict(
        "report",
        tool("Edit", serde_json::json!({"file_path": "src/one.rs"})),
    ));
    assert!(
        r.contains("report-only (tokens today at 84% of cap)"),
        "{r}"
    );
    assert_eq!(
        verdict(
            "report",
            tool(
                "Write",
                serde_json::json!({"file_path": "ci-sweeper-state.md"})
            )
        ),
        Verdict::Permit
    );
    assert_eq!(
        verdict(
            "report",
            tool(
                "Edit",
                serde_json::json!({"file_path": "./loop-run-log.md"})
            )
        ),
        Verdict::Permit
    );
    assert_eq!(
        verdict(
            "report",
            tool(
                "Write",
                serde_json::json!({"file_path": ".loop-context/notes.md"})
            )
        ),
        Verdict::Permit
    );
    // 4. maxFiles: two files already written, a third is refused, the same
    //    two (worktree-relative) are not
    let r = block(verdict(
        "assisted",
        tool("Write", serde_json::json!({"file_path": "src/three.rs"})),
    ));
    assert_eq!(
        r,
        "agent-mux loop: 2 files changed, gate.yaml maxFiles is 2"
    );
    assert_eq!(
        verdict(
            "assisted",
            tool("Edit", serde_json::json!({"file_path": "src/one.rs"}))
        ),
        Verdict::Permit
    );
    assert_eq!(
        verdict(
            "assisted",
            tool(
                "Edit",
                serde_json::json!({"file_path": "/ws/.loop-worktrees/r1/src/two.rs"})
            )
        ),
        Verdict::Permit
    );
    // read tools are never refused by the loop rules
    assert_eq!(
        verdict(
            "assisted",
            tool("Read", serde_json::json!({"file_path": ".env"}))
        ),
        Verdict::Permit
    );
    // 5. the budget guard still runs after the loop rules
    let r = block(verdict(
        "looped-costly",
        tool("Read", serde_json::json!({"file_path": "x"})),
    ));
    assert!(r.starts_with("agent-mux budget: $"), "{r}");
    // a launch without a loop policy under --loop: fail closed for writes
    let r = block(verdict(
        "nope",
        tool("Write", serde_json::json!({"file_path": "x"})),
    ));
    assert!(r.starts_with("agent-mux loop guard unavailable"), "{r}");
    assert_eq!(
        verdict("nope", tool("Read", serde_json::json!({}))),
        Verdict::Permit
    );
    // without --loop the same missing launch permits (fail-open budget guard)
    assert_eq!(
        check_tool(&db, "nope", b, Some("Write"), None, false),
        Verdict::Permit
    );
    // no store at all: closed with --loop, open without
    let gone = temp.path().join("absent.db");
    assert!(matches!(
        check_tool(&gone, "assisted", b, Some("Write"), None, true),
        Verdict::Block(_)
    ));
    assert_eq!(
        check_tool(&gone, "assisted", b, Some("Write"), None, false),
        Verdict::Permit
    );
}

#[test]
fn the_hook_command_applies_the_loop_policy_with_the_loop_flag() {
    let temp = tempfile::tempdir().unwrap();
    let db = loop_store(temp.path());
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".agent-mux")).unwrap();
    std::fs::write(
        home.join(".agent-mux").join("profiles.toml"),
        format!("[tracing]\ndb_path = \"{}\"\n", db.display()),
    )
    .unwrap();
    let db_s = db.to_string_lossy().into_owned();
    let home_s = home.to_string_lossy().into_owned();
    let pre = |id: &str, tool: &str, input: &str| {
        format!(
            r#"{{"session_id":"sess-l","prompt_id":"p1","transcript_path":"/tmp/t.jsonl","cwd":"/ws","hook_event_name":"PreToolUse","tool_name":"{tool}","tool_use_id":"{id}","tool_input":{input}}}"#
        )
    };
    // a denylisted write on a loop launch, registered with --loop: refused
    let out = hook_run(
        &args(&[
            "claude", "--db", &db_s, "--home", &home_s, "--launch", "assisted", "--loop",
        ]),
        Some(&pre("lt1", "Write", r#"{"file_path":"secrets/k.pem"}"#)),
    );
    assert!(out.inserted, "{:?}", out.error);
    let reason = out.blocked.clone().expect("blocked");
    assert!(reason.contains("gate.yaml denylist"), "{reason}");
    let reply: serde_json::Value = serde_json::from_str(out.response.as_deref().unwrap()).unwrap();
    assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(
        reply["hookSpecificOutput"]["permissionDecisionReason"],
        reason
    );
    // the state file is fine
    let out = hook_run(
        &args(&[
            "claude", "--db", &db_s, "--home", &home_s, "--launch", "report", "--loop",
        ]),
        Some(&pre(
            "lt2",
            "Write",
            r#"{"file_path":"ci-sweeper-state.md"}"#,
        )),
    );
    assert!(out.inserted && out.blocked.is_none(), "{:?}", out.blocked);
    // a push is a human gate
    let out = hook_run(
        &args(&[
            "claude", "--db", &db_s, "--home", &home_s, "--launch", "report", "--loop",
        ]),
        Some(&pre("lt3", "Bash", r#"{"command":"git push"}"#)),
    );
    assert!(
        out.blocked
            .as_deref()
            .is_some_and(|r| r.contains("human gates"))
    );
    // --guard alone also applies the loop policy (fail-open on errors)
    let out = hook_run(
        &args(&[
            "claude", "--db", &db_s, "--home", &home_s, "--launch", "assisted", "--guard",
        ]),
        Some(&pre("lt4", "Write", r#"{"file_path":".env"}"#)),
    );
    assert!(
        out.blocked
            .as_deref()
            .is_some_and(|r| r.contains("denylist"))
    );
    // a store that is not there: --loop refuses the write, --guard permits
    let out = hook_run(
        &args(&[
            "claude",
            "--db",
            "/nonexistent/x.db",
            "--home",
            &home_s,
            "--launch",
            "assisted",
            "--loop",
        ]),
        Some(&pre("lt5", "Edit", r#"{"file_path":"src/a.rs"}"#)),
    );
    assert!(
        out.blocked
            .as_deref()
            .is_some_and(|r| r.contains("guard unavailable"))
    );
    let out = hook_run(
        &args(&[
            "claude",
            "--db",
            "/nonexistent/x.db",
            "--home",
            &home_s,
            "--launch",
            "assisted",
            "--guard",
        ]),
        Some(&pre("lt6", "Edit", r#"{"file_path":"src/a.rs"}"#)),
    );
    assert!(out.blocked.is_none());
}

#[test]
fn attaching_a_loop_to_a_plan_registers_the_loop_flag_and_the_metadata_keys() {
    use agent_mux::tracing::TraceRuntime;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let toml = format!(
        "[tracing]\ndb_path = \"{}\"\nclaude_dir = \"{}\"\n",
        temp.path().join("t.db").display(),
        home.join(".claude").display()
    );
    let cfg = config::parse(&toml).unwrap();
    let mut resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    resolved.home = home.clone();
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let rt = TraceRuntime::new(resolved, tx).unwrap();
    let profile = config::Profile {
        name: "Claude Code".into(),
        command: "claude".into(),
        args: vec![],
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
    };
    let mut plan = rt
        .plan_launch(&profile, temp.path())
        .expect("a traced plan");
    let settings_of = |plan: &agent_mux::tracing::LaunchPlan| -> serde_json::Value {
        let i = plan
            .extra_args
            .iter()
            .position(|a| a == "--settings")
            .unwrap();
        serde_json::from_str(&plan.extra_args[i + 1]).unwrap()
    };
    let before = settings_of(&plan);
    let pre = &before["hooks"]["PreToolUse"][0]["hooks"][0];
    assert_eq!(pre["async"], true, "plain launch: async, no flags");
    plan.attach_loop(
        LoopLaunch {
            loop_id: "L1".into(),
            run_id: "2026-09-16T08:00:00Z".into(),
            pattern: "daily-triage".into(),
            level: LoopLevel::L1,
            workspace: temp.path().to_string_lossy().into_owned(),
            policy: LoopPolicy {
                report_only: true,
                reason: Some("level L1".into()),
                state_file: "STATE.md".into(),
                run_log: "loop-run-log.md".into(),
                denylist: vec!["**/.env".into()],
                max_files: Some(10),
                worktree: None,
            },
        },
        &home,
    );
    assert!(plan.is_loop());
    let after = settings_of(&plan);
    let pre = &after["hooks"]["PreToolUse"][0]["hooks"][0];
    assert!(pre.get("async").is_none(), "waits for the answer: {pre}");
    assert!(
        pre["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "--loop")
    );
    assert_eq!(
        plan.extra_args
            .iter()
            .filter(|a| *a == "--settings")
            .count(),
        1,
        "replaced in place"
    );
}
