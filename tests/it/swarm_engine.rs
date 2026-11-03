//! An orchestrated swarm end to end through the App with a fake `claude`
//! (schema v21): the headless planner answers a `swarm-plan` block, the
//! plan waits for approval, the dispatcher starts the first task's worker
//! with its whole task as the opening prompt, starts the dependent task
//! once the first is done, keeps finished workers open, and the swarm ends
//! `finished`. A broken plan stays in review with the planner's error.

#![cfg(unix)]

use agent_mux::app::App;
use agent_mux::app::swarm_engine::StartSwarm;
use agent_mux::bus::TaskIsolation;
use agent_mux::bus::TaskStatus;
use agent_mux::bus::swarm::record::{self, SwarmStatus, SwarmVerb};
use agent_mux::config::{self, Profile};
use agent_mux::events::AppEvent;
use agent_mux::tracing::TraceRuntime;
use agent_mux::tracing::store::open_aux;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const PLAN: &str = r#"{"plan": {"summary": "schema first, then the queries",
 "tasks": [
  {"key": "schema", "title": "Add the table", "instructions": "Create it.", "acceptance": "tests pass", "files": ["src/a.rs"]},
  {"key": "queries", "title": "Query the table", "instructions": "Read it.", "files": ["src/b.rs"], "depends_on": ["schema"]}
 ]}}"#;

/// What a coordinator might submit first: a dependency on a task that is
/// not in the plan.
const BAD_PLAN: &str = r#"{"plan": {"summary": "oops",
 "tasks": [{"key": "schema", "title": "Add the table", "instructions": "Create it.", "depends_on": ["nope"]}]}}"#;

/// A fake `claude`. As the coordinator (`$AGENT_MUX_SWARM_PLAN` set) it
/// records its context and arguments, submits `bad-plan.json` then
/// `plan.json` (each if present) through `agent-mux swarm submit-plan`, as
/// `agent_mux_swarm_plan` would, and stays open. As a worker
/// (`$AGENT_MUX_TASK_ID` set) it records its arguments, may propose a task
/// or ask the coordinator (when `propose` / `ask` exist), reports its task
/// done through `agent-mux bus done` and stays open.
fn fake_claude(bin: &Path, db: &Path, ws: &Path, plan: Option<&str>, bad: bool) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin.join("calls")).unwrap();
    if let Some(p) = plan {
        std::fs::write(bin.join("plan.json"), p).unwrap();
    }
    if bad {
        std::fs::write(bin.join("bad-plan.json"), BAD_PLAN).unwrap();
    }
    let script = bin.join("claude");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
r='{calls}'
if [ -n "$AGENT_MUX_SWARM_PLAN" ]; then
  cp "$AGENT_MUX_SWARM_PLAN" "$r/planner-context.json"
  printf '%s\n' "$@" > "$r/coordinator-args.txt"
  echo "$AGENT_MUX_SESSION_ID" > "$r/coordinator-launch.txt"
  if [ -f '{bin}/bad-plan.json' ]; then
    '{mux}' swarm submit-plan --file '{bin}/bad-plan.json' --workspace '{ws}' --db '{db}' > "$r/submit-1.txt" 2>&1
  fi
  if [ -f '{bin}/plan.json' ]; then
    '{mux}' swarm submit-plan --file '{bin}/plan.json' --workspace '{ws}' --db '{db}' > "$r/submit-2.txt" 2>&1
  fi
  echo "coordinator waiting"
  sleep 600
fi
if [ -n "$AGENT_MUX_TASK_ID" ]; then
  printf '%s\n' "$@" > "$r/worker-$AGENT_MUX_TASK_ID.txt"
  if [ -f '{bin}/ask' ] && [ ! -f "$r/asked-once" ]; then
    touch "$r/asked-once"
    '{mux}' bus send --to coordinator --topic question --workspace '{ws}' --db '{db}' "Which table name?" > "$r/ask.txt" 2>&1
  fi
  if [ -f '{bin}/propose' ] && [ ! -f "$r/proposed-once" ]; then
    touch "$r/proposed-once"
    '{mux}' bus split --title "Follow-up found by the worker" --instructions "Tidy it." --workspace '{ws}' --db '{db}' > "$r/split.txt" 2>&1
  fi
  '{mux}' bus done "$AGENT_MUX_TASK_ID" --workspace '{ws}' --db '{db}' --result "did $AGENT_MUX_TASK_ID" > "$r/done-$AGENT_MUX_TASK_ID.txt" 2>&1
  echo "worker finished"
  sleep 600
fi
exit 0
"#,
            calls = bin.join("calls").display(),
            bin = bin.display(),
            mux = env!("CARGO_BIN_EXE_agent-mux"),
            ws = ws.display(),
            db = db.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

struct Fixture {
    _temp: tempfile::TempDir,
    bin: PathBuf,
    ws: PathBuf,
    db: PathBuf,
    rx: mpsc::Receiver<AppEvent>,
    app: App,
}

fn fixture(plan: Option<&str>, bad: bool) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join("skills")).unwrap();
    let db = temp.path().join("store").join("traces.db");
    let ws = temp.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let ws = ws.canonicalize().unwrap();
    std::fs::write(ws.join("README.md"), "hi\n").unwrap();
    let bin = temp.path().join("bin");
    let script = fake_claude(&bin, &db, &ws, plan, bad);
    let toml = format!(
        "[tracing]\ndb_path = \"{}\"\nclaude_dir = \"{}/.claude\"\nhooks = \"off\"\n",
        db.display(),
        home.display()
    );
    let cfg = config::parse(&toml).unwrap();
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|_| None).unwrap();
    let (tx, rx) = mpsc::channel(1024);
    let runtime = TraceRuntime::new(resolved, tx.clone()).unwrap();
    let profiles = vec![Profile {
        name: "Claude Code".into(),
        command: script.to_string_lossy().into_owned(),
        args: vec![],
        default_dir: None,
        tracing: None,
        model: None,
        bypass_approvals: None,
    }];
    let mut app = App::new(profiles, Some(runtime), tx);
    app.clipboard_enabled = false;
    app.set_pane_size(24, 100);
    app.skill_install_home = Some(home.clone());
    app.skills_dir = Some(home.join("skills"));
    app.library_root = Some(home.join(".agent-mux"));
    app.runtime_dir = Some(home.join("runtime"));
    app.loops_file = Some(temp.path().join("loops.json"));
    app.reload_skills();
    Fixture {
        _temp: temp,
        bin,
        ws,
        db,
        rx,
        app,
    }
}

async fn pump_until(
    f: &mut Fixture,
    timeout: Duration,
    mut pred: impl FnMut(&Fixture) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        f.app.on_tick(Instant::now());
        if pred(f) {
            return true;
        }
        match tokio::time::timeout(Duration::from_millis(100), f.rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                f.app.handle_pty_output(id, &bytes, Instant::now());
            }
            Ok(Some(AppEvent::PtyExit { id })) => f.app.handle_pty_exit(id),
            Ok(Some(AppEvent::TraceStats { launch_id, stats })) => {
                f.app.handle_trace_stats(&launch_id, stats)
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    f.app.on_tick(Instant::now());
    pred(f)
}

fn swarm(f: &Fixture) -> Option<record::SwarmRun> {
    let conn = open_aux(&f.db).ok()?;
    let ws = agent_mux::bus::store::normalize_workspace(&f.ws);
    record::live_swarm(&conn, &ws).ok().flatten()
}

fn board(f: &Fixture) -> Vec<record::BoardTask> {
    let conn = open_aux(&f.db).unwrap();
    match swarm(f) {
        Some(s) => record::board(&conn, &s.id).unwrap(),
        None => vec![],
    }
}

fn status_of(f: &Fixture, key: &str) -> Option<TaskStatus> {
    board(f)
        .into_iter()
        .find(|t| t.ext.plan_key == key)
        .map(|t| t.task.status)
}

fn start(f: &mut Fixture, gate: bool) {
    start_with(f, gate, None);
}

fn start_with(f: &mut Fixture, gate: bool, check: Option<&str>) {
    f.app
        .start_orchestrated_swarm(StartSwarm {
            workspace: f.ws.clone(),
            goal: "Add the table and its queries".into(),
            planner: Some("claude".into()),
            max_workers: 2,
            isolation: TaskIsolation::Shared,
            gate,
            check: check.map(str::to_string),
            workers: None,
        })
        .unwrap_or_else(|e| panic!("the swarm did not start: {e}"));
}

#[tokio::test]
async fn a_coordinator_plans_through_the_tool_and_the_swarm_runs_its_tasks_in_order() {
    let mut f = fixture(Some(PLAN), true);
    std::fs::write(f.bin.join("ask"), "").unwrap();
    start(&mut f, true);
    assert_eq!(swarm(&f).unwrap().status, SwarmStatus::Planning);
    assert!(
        pump_until(&mut f, Duration::from_secs(60), |f| {
            swarm(f).is_some_and(|s| s.status == SwarmStatus::Review)
        })
        .await,
        "the plan never arrived: {:?} {:?}",
        swarm(&f),
        f.app.notice
    );
    let s = swarm(&f).unwrap();
    assert_eq!(
        s.plan_summary.as_deref(),
        Some("schema first, then the queries")
    );
    assert_eq!(s.plan_error, None);
    // The coordinator is a live session: its first plan was answered with
    // its error, nothing changed, and its second went on the board.
    let first = std::fs::read_to_string(f.bin.join("calls/submit-1.txt")).unwrap();
    assert!(first.starts_with("Plan rejected: 1 error(s)"), "{first}");
    assert!(
        first.contains("depends on `nope`, which is not in the plan"),
        "{first}"
    );
    let second = std::fs::read_to_string(f.bin.join("calls/submit-2.txt")).unwrap();
    assert!(second.starts_with("Plan accepted: 2 task(s)"), "{second}");
    let coordinator = std::fs::read_to_string(f.bin.join("calls/coordinator-launch.txt"))
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(s.planner_launch.as_deref(), Some(coordinator.as_str()));
    let args = std::fs::read_to_string(f.bin.join("calls/coordinator-args.txt")).unwrap();
    assert!(
        args.contains("You are the coordinator of the agent-mux swarm"),
        "{args}"
    );
    assert!(args.contains("agent_mux_swarm_plan"), "{args}");
    let ctx: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(f.bin.join("calls/planner-context.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ctx["goal"], "Add the table and its queries");
    assert_eq!(ctx["max_workers"], 2);
    let keys: Vec<String> = board(&f).iter().map(|t| t.ext.plan_key.clone()).collect();
    assert_eq!(keys, ["schema", "queries"]);

    // Nothing starts before approval, and claims are refused.
    pump_until(&mut f, Duration::from_millis(1500), |_| false).await;
    assert!(board(&f).iter().all(|t| t.task.status == TaskStatus::Open));
    let conn = open_aux(&f.db).unwrap();
    let ws = agent_mux::bus::store::normalize_workspace(&f.ws);
    let first = board(&f)[0].task.id.clone();
    assert!(agent_mux::bus::store::plan_blocks(&conn, &first).is_some());

    let done = f
        .app
        .swarm_action(&f.ws, SwarmVerb::Approve, None, None)
        .unwrap();
    assert!(done.starts_with("plan approved: 2 task(s)"), "{done}");
    // The coordinator is told.
    let told = agent_mux::bus::store::list_workspace_messages(&conn, &ws, 50).unwrap();
    assert!(
        told.iter()
            .any(|m| m.topic == "plan-approved"
                && m.to_launch.as_deref() == Some(coordinator.as_str())),
        "{told:?}"
    );

    // The first task runs and finishes; the second waits for it, then runs.
    assert!(
        pump_until(&mut f, Duration::from_secs(60), |f| {
            status_of(f, "schema") == Some(TaskStatus::Done)
        })
        .await,
        "schema never finished: {:?}",
        board(&f)
            .iter()
            .map(|t| (&t.ext.plan_key, t.task.status, &t.task.error))
            .collect::<Vec<_>>()
    );
    let schema = board(&f)
        .into_iter()
        .find(|t| t.ext.plan_key == "schema")
        .unwrap();
    let args = std::fs::read_to_string(f.bin.join(format!("calls/worker-{}.txt", schema.task.id)))
        .unwrap();
    assert!(
        args.contains(&format!(
            "Your task is `{}` (schema): **Add the table**",
            schema.task.id
        )),
        "the task travels in the opening prompt: {args}"
    );
    assert!(args.contains("### Done when\ntests pass"), "{args}");
    // The first worker asked its coordinator, not the operator: the question
    // is addressed to the coordinator's launch and is not yet a need.
    let msgs = agent_mux::bus::store::list_workspace_messages(&conn, &ws, 50).unwrap();
    let q = msgs
        .iter()
        .find(|m| m.topic == "question")
        .unwrap_or_else(|| {
            panic!(
                "no question: {:?}",
                std::fs::read_to_string(f.bin.join("calls/ask.txt"))
            )
        });
    assert_eq!(q.to_launch.as_deref(), Some(coordinator.as_str()));
    assert_eq!(q.body, "Which table name?");
    assert!(
        pump_until(&mut f, Duration::from_secs(60), |f| {
            swarm(f).is_some_and(|s| s.status == SwarmStatus::Finished)
        })
        .await,
        "the swarm never finished: {:?} {:?}",
        swarm(&f),
        board(&f)
            .iter()
            .map(|t| (&t.ext.plan_key, t.task.status, &t.task.error))
            .collect::<Vec<_>>()
    );
    let queries = board(&f)
        .into_iter()
        .find(|t| t.ext.plan_key == "queries")
        .unwrap();
    let args = std::fs::read_to_string(f.bin.join(format!("calls/worker-{}.txt", queries.task.id)))
        .unwrap();
    assert!(
        args.contains(&format!("- `schema` Add the table: did {}", schema.task.id)),
        "a dependent task reads what its dependency did: {args}"
    );
    // Finished workers stay open for follow-ups until the swarm closes.
    let live_workers = f
        .app
        .sessions
        .iter()
        .filter(|s| {
            s.group
                .as_ref()
                .is_some_and(|g| g.kind == agent_mux::tree::GroupKind::Bus)
        })
        .filter(|s| {
            !matches!(
                s.status(Instant::now()),
                agent_mux::status::Status::Exited(_)
            )
        })
        .count();
    assert_eq!(live_workers, 3, "two workers and the coordinator");
    // The coordinator is told what finished, in batches, once it is idle.
    let told_events = |f: &Fixture| {
        let conn = open_aux(&f.db).unwrap();
        agent_mux::bus::store::list_workspace_messages(
            &conn,
            &agent_mux::bus::store::normalize_workspace(&f.ws),
            50,
        )
        .unwrap()
        .into_iter()
        .filter(|m| m.topic == "swarm-events")
        .collect::<Vec<_>>()
    };
    assert!(
        pump_until(&mut f, Duration::from_secs(30), |f| !told_events(f)
            .is_empty())
        .await,
        "no events told"
    );
    let events = told_events(&f);
    assert!(
        events[0].body.contains("schema done: did"),
        "{}",
        events[0].body
    );
    let closed = f
        .app
        .swarm_action(&f.ws, SwarmVerb::Close, None, None)
        .unwrap();
    assert!(
        closed.starts_with("closed: stopped 3 session(s)"),
        "{closed}"
    );
    assert!(swarm(&f).is_none(), "the swarm ended");
    f.app.kill_all();
}

#[tokio::test]
async fn a_plan_with_errors_changes_nothing_and_the_swarm_keeps_planning() {
    let mut f = fixture(None, true);
    start(&mut f, false);
    assert!(
        pump_until(&mut f, Duration::from_secs(60), |f| {
            std::fs::read_to_string(f.bin.join("calls/submit-1.txt"))
                .is_ok_and(|t| t.contains("Plan rejected"))
        })
        .await
    );
    pump_until(&mut f, Duration::from_millis(1500), |_| false).await;
    let said = std::fs::read_to_string(f.bin.join("calls/submit-1.txt")).unwrap();
    assert!(said.starts_with("Plan rejected"), "{said}");
    let s = swarm(&f).unwrap();
    assert_eq!(
        s.status,
        SwarmStatus::Planning,
        "still waiting for a sound plan"
    );
    assert!(board(&f).is_empty());
    // r on the coordinator row needs it gone first.
    let err = f
        .app
        .swarm_action(&f.ws, SwarmVerb::Restart, None, None)
        .unwrap_err();
    assert!(err.contains("still running"), "{err}");
    f.app.kill_all();
}

#[tokio::test]
async fn a_workers_proposal_waits_for_the_operator_and_the_check_runs_before_finishing() {
    let mut f = fixture(Some(PLAN), false);
    std::fs::write(f.bin.join("propose"), "").unwrap();
    start_with(&mut f, true, Some("echo checked-ok"));
    assert!(
        pump_until(&mut f, Duration::from_secs(60), |f| {
            swarm(f).is_some_and(|s| s.status == SwarmStatus::Review)
        })
        .await
    );
    f.app
        .swarm_action(&f.ws, SwarmVerb::Approve, None, None)
        .unwrap();

    // The first worker proposes a task instead of starting an agent.
    assert!(
        pump_until(&mut f, Duration::from_secs(60), |f| {
            board(f).iter().any(|t| t.ext.proposed)
        })
        .await,
        "no proposal: {:?}",
        std::fs::read_to_string(f.bin.join("calls/split.txt"))
    );
    let said = std::fs::read_to_string(f.bin.join("calls/split.txt")).unwrap();
    assert!(said.contains("as a proposal for the operator"), "{said}");
    let proposal = board(&f).into_iter().find(|t| t.ext.proposed).unwrap();
    assert_eq!(proposal.ext.plan_key, "follow-up-found-by");
    assert_eq!(
        proposal.task.created_by,
        record::creator(&swarm(&f).unwrap().id)
    );

    // Both planned tasks finish; the proposal holds the swarm until decided.
    assert!(
        pump_until(&mut f, Duration::from_secs(60), |f| {
            status_of(f, "queries") == Some(TaskStatus::Done)
        })
        .await
    );
    pump_until(&mut f, Duration::from_millis(1500), |_| false).await;
    assert_eq!(swarm(&f).unwrap().status, SwarmStatus::Running);
    assert_eq!(proposal.task.status, TaskStatus::Open);
    let accepted = f
        .app
        .swarm_action(&f.ws, SwarmVerb::Accept, Some("follow-up"), None)
        .unwrap();
    assert_eq!(accepted, "accepted `follow-up-found-by`");

    // It runs, then the check, then the swarm is finished.
    assert!(
        pump_until(&mut f, Duration::from_secs(60), |f| {
            swarm(f).is_some_and(|s| s.status == SwarmStatus::Finished)
        })
        .await,
        "{:?}",
        swarm(&f)
    );
    assert_eq!(status_of(&f, "follow-up-found-by"), Some(TaskStatus::Done));
    let r = agent_mux::bus::swarm::check::CheckResult::parse(
        swarm(&f).unwrap().check_result.as_deref().unwrap(),
    )
    .unwrap();
    assert!(r.passed && !r.running, "{r:?}");
    assert_eq!(r.tail, "checked-ok");
    f.app.kill_all();
}
