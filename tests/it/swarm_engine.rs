//! An orchestrated swarm end to end through the App with a fake `claude`
//! (schema v21): the headless planner answers a `swarm-plan` block, the
//! plan waits for approval, the dispatcher starts the first task's worker
//! with its whole task as the opening prompt, starts the dependent task
//! once the first is done, keeps finished workers open, and the swarm ends
//! `finished`. A broken plan stays in review with the planner's error.

#![cfg(unix)]

#[path = "support/hermetic_env.rs"]
mod hermetic_env;

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
/// `agent_mux_swarm_plan` would, relays the operator's "go" when `launch`
/// exists (`swarm launch --as-coordinator --said go`, what
/// `agent_mux_swarm_launch` does), and stays open. As a worker
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
# drop the agent-mux identity `cargo test` may have inherited
{hermetic}
r='{calls}'
# The dispatcher records a worker's task (assigned_to) just after starting
# its process; a real harness boots for seconds first, this one does not:
# retry a bus call refused in that window.
retry() {{
  i=0
  until "$@"; do
    i=$((i+1))
    [ "$i" -ge 40 ] && return 1
    sleep 0.25
  done
}}
assigned() {{
  '{mux}' bus tasks --assigned-to "$AGENT_MUX_SESSION_ID" --workspace '{ws}' --db '{db}' | grep -q "^$AGENT_MUX_TASK_ID "
}}
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
  if [ -f '{bin}/launch' ]; then
    # the operator said go in the chat: relay it, through the rows (no socket)
    AGENT_MUX_CONTROL_SOCK=/nonexistent/agent-mux-test.sock '{mux}' swarm launch --as-coordinator --said "go" --workspace '{ws}' --db '{db}' > "$r/launch.txt" 2>&1
  fi
  echo "coordinator waiting"
  sleep 600
fi
if [ -n "$AGENT_MUX_TASK_ID" ]; then
  printf '%s\n' "$@" > "$r/worker-$AGENT_MUX_TASK_ID.txt"
  if [ -f '{bin}/ask' ] && [ ! -f "$r/asked-once" ]; then
    touch "$r/asked-once"
    retry '{mux}' bus send --to coordinator --topic question --workspace '{ws}' --db '{db}' "Which table name?" > "$r/ask.txt" 2>&1
  fi
  if [ -f '{bin}/propose' ] && [ ! -f "$r/proposed-once" ]; then
    touch "$r/proposed-once"
    # a split before the dispatcher assigned the task to this launch would
    # succeed as a plain task, not a proposal: wait for the assignment
    retry assigned
    retry '{mux}' bus split --title "Follow-up found by the worker" --instructions "Tidy it." --workspace '{ws}' --db '{db}' > "$r/split.txt" 2>&1
  fi
  result="did $AGENT_MUX_TASK_ID"
  # a review answers with the verdict the test left, and says where it ran
  if grep -q 'You review one change' "$r/worker-$AGENT_MUX_TASK_ID.txt"; then
    result=$(cat '{bin}/verdict' 2>/dev/null || echo 'APPROVE fine')
    pwd > "$r/review-cwd.txt"
    echo x >> "$r/reviews.txt"
  fi
  retry '{mux}' bus done "$AGENT_MUX_TASK_ID" --workspace '{ws}' --db '{db}' --result "$result" > "$r/done-$AGENT_MUX_TASK_ID.txt" 2>&1
  echo "worker finished"
  sleep 600
fi
exit 0
"#,
            hermetic = hermetic_env::shell_unset_inherited(),
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
    // the fake coordinator never launches a task itself: the dispatcher's
    // grace is short so the test sees it step in
    app.swarm_dispatch_grace_ns = 2_000_000_000;
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
    start_reviewed(f, gate, check, "off");
}

fn start_reviewed(f: &mut Fixture, gate: bool, check: Option<&str>, review: &str) {
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
            review: Some(review.into()),
            merge_gate: None,
        })
        .unwrap_or_else(|e| panic!("the swarm did not start: {e}"));
}

#[tokio::test]
async fn a_coordinator_plans_through_the_tool_and_the_swarm_runs_its_tasks_in_order() {
    let mut f = fixture(Some(PLAN), true);
    std::fs::write(f.bin.join("ask"), "").unwrap();
    std::fs::write(f.bin.join("launch"), "").unwrap();
    start(&mut f, true);
    assert_eq!(swarm(&f).unwrap().status, SwarmStatus::Planning);
    assert!(
        pump_until(&mut f, Duration::from_secs(120), |f| {
            swarm(f).is_some_and(|s| s.status.approved())
        })
        .await,
        "the plan never arrived or was not launched: {:?} {:?} launch: {:?}",
        swarm(&f),
        f.app.notice,
        std::fs::read_to_string(f.bin.join("calls/launch.txt"))
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
    // The opening prompt is three lines: the swarm, the skill, the goal.
    assert!(
        args.contains(&format!(
            "You are the coordinator of agent-mux swarm {} in",
            s.id
        )),
        "{args}"
    );
    assert!(
        args.contains("/swarm-coordinator Read the context at"),
        "{args}"
    );
    assert!(
        args.contains("The operator's goal: Add the table and its queries"),
        "{args}"
    );
    let ctx: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(f.bin.join("calls/planner-context.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ctx["goal"], "Add the table and its queries");
    assert_eq!(ctx["max_workers"], 2);
    assert_eq!(ctx["limits"]["gate"], "chat");
    assert_eq!(ctx["limits"]["headless"], true, "a goal given up front");
    assert_eq!(ctx["limits"]["max_workers"], 2);
    assert_eq!(ctx["tools"]["dispatch_grace_s"], 2);
    assert!(
        ctx["tools"]["mcp"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("agent_mux_swarm_launch"))
    );
    let keys: Vec<String> = board(&f).iter().map(|t| t.ext.plan_key.clone()).collect();
    assert_eq!(keys, ["schema", "queries"]);

    // The coordinator relayed the operator's "go" from its chat: the plan is
    // approved on their word, recorded as the chat's, and nobody on the
    // screen had to press anything.
    let conn = open_aux(&f.db).unwrap();
    let ws = agent_mux::bus::store::normalize_workspace(&f.ws);
    let launched = std::fs::read_to_string(f.bin.join("calls/launch.txt")).unwrap();
    assert!(launched.contains("plan approved"), "{launched}");
    let d = record::launch_decision(&conn, &s.id).expect("the launch decision");
    assert_eq!(d.by, record::DecidedBy::Chat);
    assert_eq!(d.said.as_deref(), Some("go"));
    assert_eq!(d.launch.as_deref(), Some(coordinator.as_str()));
    assert_eq!(s.status, SwarmStatus::Running);

    // The first task runs and finishes; the second waits for it, then runs.
    assert!(
        pump_until(&mut f, Duration::from_secs(120), |f| {
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
        pump_until(&mut f, Duration::from_secs(120), |f| {
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
    // `schema` was asked for by the operator's approval; `queries` became
    // ready later, was offered to the coordinator, which never launched it,
    // so the dispatcher started it after the grace (coordinator-first §5.4).
    assert_eq!(
        schema.ext.launched_by,
        Some(record::LaunchedBy::Coordinator),
        "{:?}",
        schema.ext
    );
    assert_eq!(
        queries.ext.launched_by,
        Some(record::LaunchedBy::Dispatcher)
    );
    assert!(queries.ext.offered_ns.is_some());
    let kinds: Vec<String> = record::recent_events(&conn, &swarm(&f).unwrap().id, 100)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    for k in [
        "started",
        "coordinator.started",
        "plan.proposed",
        "decision.launch",
        "task.requested",
        "task.spawned",
        "task.offered",
        "swarm.finished",
    ] {
        assert!(kinds.contains(&k.to_string()), "{k} missing from {kinds:?}");
    }
    // Every launch of the swarm knows its swarm (§7.1): the coordinator and
    // both workers, the workers with their task and the coordinator as parent.
    let sid = swarm(&f).unwrap().id;
    let mut rows = conn
        .prepare(
            "SELECT json_extract(metadata, '$.swarm_role'), json_extract(metadata, '$.swarm_task_key'),
                    json_extract(metadata, '$.parent_launch')
             FROM launches WHERE json_extract(metadata, '$.swarm_id') = ?1 ORDER BY started_ns",
        )
        .unwrap();
    let launches: Vec<(String, Option<String>, Option<String>)> = rows
        .query_map([&sid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(launches.len(), 3, "{launches:?}");
    assert_eq!(launches[0], ("coordinator".into(), None, None));
    assert_eq!(
        launches[1],
        (
            "worker".into(),
            Some("schema".into()),
            Some(coordinator.clone())
        )
    );
    assert_eq!(launches[2].1.as_deref(), Some("queries"));
    // Finished workers stay open for follow-ups until the swarm closes.
    let live_workers = f
        .app
        .sessions
        .iter()
        .filter(|s| {
            s.group
                .as_ref()
                .is_some_and(|g| g.kind == agent_mux::tree::GroupKind::Swarm)
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
        pump_until(&mut f, Duration::from_secs(60), |f| !told_events(f)
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
        pump_until(&mut f, Duration::from_secs(120), |f| {
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
        pump_until(&mut f, Duration::from_secs(120), |f| {
            swarm(f).is_some_and(|s| s.status == SwarmStatus::Review)
        })
        .await
    );
    f.app
        .swarm_action(&f.ws, SwarmVerb::Approve, None, None)
        .unwrap();

    // The first worker proposes a task instead of starting an agent.
    assert!(
        pump_until(&mut f, Duration::from_secs(120), |f| {
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
        pump_until(&mut f, Duration::from_secs(120), |f| {
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
        pump_until(&mut f, Duration::from_secs(120), |f| {
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

const ONE_TASK: &str = r#"{"plan": {"summary": "one task",
 "tasks": [
  {"key": "schema", "title": "Add the table", "instructions": "Create it.", "acceptance": "tests pass", "files": ["src/a.rs"]}
 ]}}"#;

fn review_state(f: &Fixture, key: &str) -> Option<agent_mux::bus::swarm::review::ReviewState> {
    let b = board(f);
    let t = b.iter().find(|t| t.ext.plan_key == key)?;
    agent_mux::bus::swarm::review::state(t, &b, true)
}

#[tokio::test]
async fn a_finished_task_is_reviewed_read_only_and_reviewed_again_after_a_fix() {
    use agent_mux::bus::swarm::review::ReviewState;
    let mut f = fixture(Some(ONE_TASK), false);
    std::fs::write(f.bin.join("launch"), "").unwrap();
    std::fs::write(
        f.bin.join("verdict"),
        "CHANGES\n- src/a.rs:1 the table has no primary key",
    )
    .unwrap();
    start_reviewed(&mut f, true, None, "auto");
    let conn = open_aux(&f.db).unwrap();
    let s = swarm(&f).unwrap();
    assert!(agent_mux::bus::swarm::review::settings(&conn, &s.id).enabled);

    // The task finishes; its review goes on the board and runs.
    assert!(
        pump_until(&mut f, Duration::from_secs(120), |f| {
            review_state(f, "schema") == Some(ReviewState::Changes)
        })
        .await,
        "no verdict: {:?}",
        board(&f)
            .iter()
            .map(|t| (
                &t.ext.plan_key,
                t.task.status,
                &t.task.result,
                &t.task.error
            ))
            .collect::<Vec<_>>()
    );
    let b = board(&f);
    let review = b
        .iter()
        .find(|t| t.ext.plan_key == "schema-review")
        .unwrap();
    let schema = b.iter().find(|t| t.ext.plan_key == "schema").unwrap();
    assert_eq!(
        review.ext.review_of.as_deref(),
        Some(schema.task.id.as_str())
    );
    assert_eq!(
        schema.ext.review_task.as_deref(),
        Some(review.task.id.as_str())
    );
    // only claude here: it reviews its own harness's work, as `reviewer`
    assert_eq!(review.task.target_harness.as_deref(), Some("claude"));
    assert_eq!(review.task.target_agent.as_deref(), Some("reviewer"));
    let args = std::fs::read_to_string(f.bin.join(format!("calls/worker-{}.txt", review.task.id)))
        .unwrap();
    assert!(args.contains("**change no file**"), "{args}");
    assert!(
        args.contains("### The change under review: `schema`"),
        "{args}"
    );
    assert!(args.contains("tests pass"), "the done-when line: {args}");
    assert!(args.contains("did "), "the worker's result: {args}");
    assert!(args.contains("reviewer"), "the reviewer agent: {args}");
    // a shared task is reviewed in the workspace itself
    let cwd = std::fs::read_to_string(f.bin.join("calls/review-cwd.txt")).unwrap();
    assert_eq!(
        std::path::Path::new(cwd.trim()).canonicalize().unwrap(),
        f.ws
    );
    // the coordinator's board says so, with the findings and the warning
    let view = agent_mux::bus::swarm::view::board_view(
        &conn,
        &swarm(&f).unwrap(),
        &Default::default(),
        agent_mux::tracing::store::now_ns(),
    )
    .unwrap();
    assert_eq!(view.review, "auto");
    let r = view.reviews.iter().find(|r| r.task == "schema").unwrap();
    assert_eq!(r.state, "changes");
    assert!(r.verdict.as_deref().unwrap().contains("no primary key"));
    assert_eq!(
        r.merge_warning.as_deref(),
        Some("its review asked for changes")
    );

    // After the fix, `review` puts a second review on the board; it approves,
    // and the first review's findings stay as they were.
    std::fs::write(f.bin.join("verdict"), "APPROVE the key is there now").unwrap();
    let done = f
        .app
        .swarm_action(&f.ws, SwarmVerb::Review, Some("schema"), None)
        .unwrap();
    assert!(
        done.contains("reviewed again by `schema-review-2`"),
        "{done}"
    );
    assert!(
        pump_until(&mut f, Duration::from_secs(120), |f| {
            review_state(f, "schema") == Some(ReviewState::Approved)
        })
        .await,
        "never approved: {:?}",
        review_state(&f, "schema")
    );
    let reviews = std::fs::read_to_string(f.bin.join("calls/reviews.txt")).unwrap();
    assert_eq!(reviews.lines().count(), 2, "two review runs");
    let first = board(&f)
        .into_iter()
        .find(|t| t.ext.plan_key == "schema-review")
        .unwrap();
    assert!(
        agent_mux::bus::swarm::review::result_text(&first)
            .unwrap()
            .starts_with("CHANGES")
    );
    assert!(
        pump_until(&mut f, Duration::from_secs(60), |f| {
            swarm(f).is_none_or(|s| s.status == SwarmStatus::Finished)
        })
        .await,
        "the swarm did not finish: {:?}",
        swarm(&f).map(|s| s.status)
    );
    f.app.kill_all();
}
