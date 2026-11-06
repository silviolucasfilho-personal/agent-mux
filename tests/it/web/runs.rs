//! The Runs area: `/api/runs/` over a store seeded with one loop run (with
//! a launch, a write tool and a verifier sub-agent) and one workflow run
//! with two steps, read back through every route; plus the shell smoke
//! test and the pages the script registers.

use super::support::{get, seeded_store, start};
use agent_mux::loops::store::{self as lstore, LoopRun};
use agent_mux::loops::{Level, Outcome};
use agent_mux::tracing::store::model::{
    LaunchRow, Level as ObsLevel, ObservationRow, ObservationType, StoreOp, TraceRow, TraceStatus,
};
use agent_mux::tracing::store::{OpenOptions, open_aux, open_rw};
use agent_mux::workflows::store::{self as wstore, WorkflowRun, WorkflowStep};
use serde_json::{Value, json};
use std::path::Path;

const LOOP_RUN: &str = "2026-11-06T08:00:00Z";
const LAUNCH: &str = "launch-loop-1";
const WF_RUN: &str = "wf-run-0001";

fn get_json(base: &str, path: &str) -> (u16, Value) {
    let (status, ctype, body) = get(base, path);
    assert!(
        ctype.starts_with("application/json"),
        "{path}: content-type {ctype}"
    );
    let json: Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("{path}: body is not JSON ({e}): {body}"));
    (status, json)
}

/// Seeds the runs on top of `seeded_store`: a loop run of `claude:s1`'s
/// launch (fix proposed, undecided: in the inbox), one blocked loop run,
/// a finished workflow run with two steps and a failed one (in the inbox).
fn seed_runs(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let (db, ws) = seeded_store(dir);
    let now = agent_mux::tracing::store::now_ns();
    let base = now - 60 * 1_000_000_000;

    // the launch the loop run points at, with a trace, a write tool and a
    // verifier sub-agent on it
    {
        let mut store = open_rw(
            &db,
            OpenOptions {
                prices: agent_mux::tracing::pricing::PriceTable::builtin(),
                run_id: "run-web".into(),
                retention_days: 0,
                agent_mux_version: "test".into(),
            },
        )
        .unwrap();
        let launch = LaunchRow {
            id: LAUNCH.into(),
            run_id: "run-web".into(),
            agent_mux_session: 7,
            profile: "Claude Code".into(),
            provider: "claude".into(),
            cwd: ws.to_string_lossy().into_owned(),
            project_slug: "ws".into(),
            content_mode: "full".into(),
            correlation_plan: "{}".into(),
            correlation: None,
            session_key: Some("claude:s1".into()),
            injected_session_id: false,
            attached: false,
            started_ns: base,
            ended_ns: Some(base + 30_000_000_000),
            termination: Some("exited".into()),
            exit_code: Some(0),
            parse_errors: None,
            dropped_ops: None,
            reported_cost_usd: None,
            reported_lines_added: None,
            reported_lines_removed: None,
            agent_mux_version: "test".into(),
            user_id: None,
            release: None,
            environment: None,
            tags: Vec::new(),
            metadata: None,
        };
        let trace = TraceRow {
            id: "t-loop".into(),
            session_key: "claude:s1".into(),
            provider: "claude".into(),
            session_id: "s1".into(),
            launch_id: Some(LAUNCH.into()),
            ordinal: 3,
            name: "turn 3".into(),
            status: TraceStatus::Closed,
            start_ns: base,
            end_ns: Some(base + 20_000_000_000),
            input: Some("triage the failing build".into()),
            output: Some("Proposed a fix in src/lib.rs".into()),
            thinking: None,
            skills: None,
            reported_duration_ms: None,
            reported_message_count: None,
            session_cost_usd: None,
            timing_approx: false,
            metadata: None,
        };
        let edit = ObservationRow {
            id: "o-edit".into(),
            trace_id: "t-loop".into(),
            parent_id: None,
            obs_type: ObservationType::Tool,
            name: "Edit".into(),
            kind: None,
            start_ns: base + 1_000_000_000,
            end_ns: Some(base + 1_200_000_000),
            level: ObsLevel::Default,
            status_message: None,
            model: None,
            input: Some(json!({"file_path": "src/lib.rs"}).to_string()),
            output: None,
            thinking: None,
            usage_raw: None,
            usage: None,
            tool_id: None,
            tool_name: Some("Edit".into()),
            skill: None,
            mcp_server: None,
            path: None,
            ts_approx: false,
            metadata: serde_json::Map::new(),
        };
        let verifier = ObservationRow {
            id: "o-verifier".into(),
            trace_id: "t-loop".into(),
            parent_id: None,
            obs_type: ObservationType::Agent,
            name: "loop-verifier".into(),
            kind: None,
            start_ns: base + 2_000_000_000,
            end_ns: Some(base + 4_000_000_000),
            level: ObsLevel::Default,
            status_message: None,
            model: None,
            input: None,
            output: Some("## Verdict: APPROVE\nlooks right".into()),
            thinking: None,
            usage_raw: None,
            usage: None,
            tool_id: None,
            tool_name: Some("loop-verifier".into()),
            skill: None,
            mcp_server: None,
            path: None,
            ts_approx: false,
            metadata: serde_json::Map::new(),
        };
        store
            .apply(&[
                StoreOp::Launch(launch),
                StoreOp::Trace(trace),
                StoreOp::Observation(edit),
                StoreOp::Observation(verifier),
            ])
            .unwrap();
    }

    let conn = open_aux(&db).unwrap();
    let mut run = LoopRun::new(
        LOOP_RUN,
        "nightly-triage",
        ws.to_string_lossy().into_owned(),
        "daily-triage",
        "claude",
        Level::L2,
        base - 1_000_000_000,
    );
    run.launch_id = Some(LAUNCH.into());
    run.started_ns = Some(base);
    run.ended_ns = Some(base + 30_000_000_000);
    run.outcome = Outcome::FixProposed;
    run.items_found = Some(2);
    run.actions_taken = Some(1);
    run.tokens = Some(12_345);
    run.cost_usd = Some(0.42);
    run.readiness_score = Some(80);
    run.worktree = Some("/tmp/wt".into());
    run.branch = Some("loop/daily-triage-1".into());
    run.set_detail("reason", json!("fix proposed"));
    run.pattern_hash = Some("abc123".into());
    lstore::upsert_run(&conn, &run).unwrap();
    let mut blocked = LoopRun::new(
        "2026-11-05T08:00:00Z",
        "nightly-triage",
        ws.to_string_lossy().into_owned(),
        "daily-triage",
        "claude",
        Level::L1,
        base - 86_400_000_000_000,
    );
    blocked.outcome = Outcome::Blocked;
    blocked.set_detail("reason", json!("budget"));
    lstore::upsert_run(&conn, &blocked).unwrap();

    let wf = WorkflowRun {
        id: WF_RUN.into(),
        workflow: "review-changes".into(),
        source: "builtin".into(),
        document_hash: "h".into(),
        document: "[workflow]\nname = \"review-changes\"\n".into(),
        workspace: ws.to_string_lossy().into_owned(),
        harness: "claude".into(),
        profile: "Claude Code".into(),
        args: json!({"scope": "HEAD~1"}),
        budget_tokens: Some(100_000),
        started_ns: base,
        ended_ns: Some(base + 40_000_000_000),
        status: "finished".into(),
        sessions: 2,
        tokens: Some(2_000),
        cost_usd: Some(0.05),
        result: json!("## Report\nnothing to fix"),
        error: None,
        resumed_from: None,
    };
    wstore::upsert_run(&conn, &wf).unwrap();
    let mut failed = wf.clone();
    failed.id = "wf-run-0002".into();
    failed.started_ns = base + 50_000_000_000;
    failed.ended_ns = Some(base + 51_000_000_000);
    failed.status = "failed".into();
    failed.error = Some("step find[1] exited 1".into());
    failed.result = Value::Null;
    wstore::upsert_run(&conn, &failed).unwrap();
    let step = |session: &str, item: Value, start: i64, result: Value| WorkflowStep {
        run_id: WF_RUN.into(),
        session: session.into(),
        step_id: "find".into(),
        item,
        launch_id: Some(LAUNCH.into()),
        phase: "Review".into(),
        harness: "claude".into(),
        kind: "object".into(),
        started_ns: Some(start),
        ended_ns: Some(start + 5_000_000_000),
        tokens: Some(1_000),
        cost_usd: Some(0.025),
        worktree: None,
        changed_files: Value::Null,
        result,
    };
    wstore::upsert_step(
        &conn,
        &step(
            "find[1]",
            json!("correctness"),
            base + 1_000_000_000,
            json!({"findings": []}),
        ),
    )
    .unwrap();
    wstore::upsert_step(
        &conn,
        &step(
            "find[2]",
            json!("security"),
            base + 2_000_000_000,
            json!("no findings"),
        ),
    )
    .unwrap();
    (db, ws)
}

#[test]
fn the_runs_area_serves_the_shell_and_its_pages() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seeded_store(temp.path());
    let (base, server) = start(&db);
    let (status, _, body) = get(&base, "/");
    assert_eq!(status, 200);
    assert!(body.contains("/static/runs.js"));
    let (status, _, script) = get(&base, "/static/runs.js");
    assert_eq!(status, 200);
    for path in [
        "'/runs'",
        "'/runs/loop/:id'",
        "'/runs/workflow/:id'",
        "'/inbox'",
    ] {
        assert!(script.contains(path), "runs.js registers {path}");
    }
    for route in ["/api/runs/loops", "/api/runs/workflows", "/api/runs/inbox"] {
        assert!(script.contains(route), "runs.js calls {route}");
    }
    assert!(
        script.contains("'/sessions/'"),
        "the launch links to its session"
    );
    server.abort();
}

#[test]
fn loop_runs_are_listed_and_read_with_their_launch_facts() {
    let temp = tempfile::tempdir().unwrap();
    let (db, ws) = seed_runs(temp.path());
    let (base, server) = start(&db);

    let (status, list) = get_json(&base, "/api/runs/loops");
    assert_eq!(status, 200, "{list}");
    assert_eq!(list["ok"], true);
    let runs = list["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["id"], LOOP_RUN, "newest scheduled first");
    assert_eq!(runs[0]["pattern"], "daily-triage");
    assert_eq!(runs[0]["loop_id"], "nightly-triage");
    assert_eq!(runs[0]["workspace"], ws.to_string_lossy().as_ref());
    assert_eq!(runs[0]["outcome"], "fix-proposed");
    assert_eq!(runs[0]["outcome_word"], "fix ready");
    assert_eq!(runs[0]["level"], "L2");
    assert_eq!(runs[0]["duration_s"], 30);
    assert_eq!(runs[0]["cost_usd"], 0.42);
    assert_eq!(runs[0]["tokens"], 12_345);
    assert_eq!(runs[0]["in_inbox"], true);
    assert_eq!(runs[1]["outcome"], "blocked");
    assert_eq!(runs[1]["in_inbox"], false);
    assert_eq!(runs[1]["detail"]["reason"], "budget");

    let (status, one) = get_json(&base, "/api/runs/loops?limit=1");
    assert_eq!(status, 200);
    assert_eq!(one["runs"].as_array().unwrap().len(), 1);

    let (status, detail) = get_json(
        &base,
        &format!("/api/runs/loops/{}", LOOP_RUN.replace(':', "%3A")),
    );
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["run"]["id"], LOOP_RUN);
    assert_eq!(detail["run"]["branch"], "loop/daily-triage-1");
    assert_eq!(detail["run"]["pattern_hash"], "abc123");
    assert_eq!(detail["run"]["detail"]["reason"], "fix proposed");
    assert_eq!(detail["launch"]["id"], LAUNCH);
    assert_eq!(detail["launch"]["session_key"], "claude:s1");
    assert_eq!(detail["launch"]["provider"], "claude");
    assert_eq!(detail["facts"]["verifier_ran"], true);
    assert_eq!(detail["facts"]["verifier_verdict"], "APPROVE");
    assert_eq!(detail["facts"]["verifier_verdicts"], json!(["APPROVE"]));
    assert_eq!(
        detail["facts"]["final_message"],
        "Proposed a fix in src/lib.rs"
    );
    assert_eq!(detail["files_touched"], json!(["src/lib.rs"]));
    assert_eq!(detail["facts"]["files_touched"], json!(["src/lib.rs"]));

    // a run without a launch has no facts and no files
    let (status, bare) = get_json(&base, "/api/runs/loops/2026-11-05T08%3A00%3A00Z");
    assert_eq!(status, 200, "{bare}");
    assert!(bare["launch"].is_null());
    assert!(bare["facts"].is_null());
    assert_eq!(bare["files_touched"], json!([]));

    let (status, missing) = get_json(&base, "/api/runs/loops/nope");
    assert_eq!(status, 404);
    assert_eq!(missing["ok"], false);
    assert!(missing["error"].as_str().unwrap().contains("nope"));
    server.abort();
}

#[test]
fn workflow_runs_are_listed_and_read_with_their_steps() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seed_runs(temp.path());
    let (base, server) = start(&db);

    let (status, list) = get_json(&base, "/api/runs/workflows");
    assert_eq!(status, 200, "{list}");
    let runs = list["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["id"], "wf-run-0002", "newest first");
    assert_eq!(runs[0]["status"], "failed");
    assert_eq!(runs[0]["needs_human"], true);
    assert_eq!(runs[1]["id"], WF_RUN);
    assert_eq!(runs[1]["workflow"], "review-changes");
    assert_eq!(runs[1]["status"], "finished");
    assert_eq!(runs[1]["sessions"], 2);
    assert_eq!(runs[1]["tokens"], 2_000);
    assert_eq!(runs[1]["cost_usd"], 0.05);
    assert_eq!(runs[1]["duration_s"], 40);
    assert_eq!(runs[1]["args"]["scope"], "HEAD~1");
    assert_eq!(runs[1]["needs_human"], false);
    assert!(
        runs[1].get("document").is_none(),
        "the list leaves the document text out"
    );

    let (status, one) = get_json(&base, "/api/runs/workflows?limit=1");
    assert_eq!(status, 200);
    assert_eq!(one["runs"].as_array().unwrap().len(), 1);

    let (status, detail) = get_json(&base, &format!("/api/runs/workflows/{WF_RUN}"));
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["run"]["id"], WF_RUN);
    assert_eq!(
        detail["run"]["document"],
        "[workflow]\nname = \"review-changes\"\n"
    );
    assert_eq!(detail["run"]["result_text"], "## Report\nnothing to fix");
    let steps = detail["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["session"], "find[1]");
    assert_eq!(steps[0]["step_id"], "find");
    assert_eq!(steps[0]["item"], "correctness");
    assert_eq!(steps[0]["kind"], "object");
    assert_eq!(steps[0]["harness"], "claude");
    assert_eq!(steps[0]["phase"], "Review");
    assert_eq!(steps[0]["tokens"], 1_000);
    assert_eq!(steps[0]["cost_usd"], 0.025);
    assert_eq!(steps[0]["duration_s"], 5);
    assert_eq!(steps[0]["launch_id"], LAUNCH);
    assert_eq!(steps[0]["session_key"], "claude:s1");
    assert!(
        steps[0]["result_text"]
            .as_str()
            .unwrap()
            .contains("\"findings\""),
        "an object result is pretty JSON"
    );
    assert_eq!(steps[1]["session"], "find[2]");
    assert_eq!(steps[1]["result_text"], "no findings");

    // a unique id prefix resolves, like the CLI
    let (status, by_prefix) = get_json(&base, "/api/runs/workflows/wf-run-0001");
    assert_eq!(status, 200);
    assert_eq!(by_prefix["run"]["id"], WF_RUN);
    let (status, ambiguous) = get_json(&base, "/api/runs/workflows/wf-run");
    assert_eq!(status, 404, "{ambiguous}");
    assert_eq!(ambiguous["ok"], false);
    server.abort();
}

#[test]
fn the_inbox_lists_undecided_loop_runs_and_workflow_runs_that_need_a_human() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = seed_runs(temp.path());
    let (base, server) = start(&db);
    let (status, inbox) = get_json(&base, "/api/runs/inbox");
    assert_eq!(status, 200, "{inbox}");
    assert_eq!(inbox["ok"], true);
    let loops = inbox["loops"].as_array().unwrap();
    assert_eq!(loops.len(), 1, "the blocked run is not waiting on anyone");
    assert_eq!(loops[0]["id"], LOOP_RUN);
    assert_eq!(loops[0]["outcome_word"], "fix ready");
    let workflows = inbox["workflows"].as_array().unwrap();
    assert_eq!(workflows.len(), 1, "only the failed run waits");
    assert_eq!(workflows[0]["id"], "wf-run-0002");
    assert_eq!(workflows[0]["error"], "step find[1] exited 1");

    // the inbox never decides: the rows are unchanged after reading
    let conn = agent_mux::tracing::store::open_ro(&db).unwrap();
    let run = lstore::get_run(&conn, LOOP_RUN).unwrap().unwrap();
    assert!(run.decision.is_none());
    assert!(run.in_inbox());
    server.abort();
}

#[test]
fn a_missing_store_answers_503() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("absent.db");
    let (base, server) = start(&db);
    for path in [
        "/api/runs/loops",
        "/api/runs/workflows",
        "/api/runs/inbox",
        "/api/runs/loops/x",
        "/api/runs/workflows/x",
    ] {
        let (status, body) = get_json(&base, path);
        assert_eq!(status, 503, "{path}: {body}");
        assert_eq!(body["ok"], false);
    }
    server.abort();
}
