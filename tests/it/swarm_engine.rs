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

const PLAN: &str = r#"Here is the plan.
```swarm-plan
{"summary": "schema first, then the queries",
 "tasks": [
  {"key": "schema", "title": "Add the table", "instructions": "Create it.", "acceptance": "tests pass", "files": ["src/a.rs"]},
  {"key": "queries", "title": "Query the table", "instructions": "Read it.", "files": ["src/b.rs"], "depends_on": ["schema"]}
 ]}
```"#;

/// A fake `claude`: as the planner (`$AGENT_MUX_SWARM_PLAN` set) it prints
/// a print-mode envelope whose result is `plan_text`; as a worker
/// (`$AGENT_MUX_TASK_ID` set) it records its arguments, reports its task
/// done through `agent-mux bus done` and stays open.
fn fake_claude(bin: &Path, db: &Path, ws: &Path, plan_text: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin.join("calls")).unwrap();
    let plan_file = bin.join("plan.txt");
    std::fs::write(&plan_file, plan_text).unwrap();
    let script = bin.join("claude");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
r='{calls}'
if [ -n "$AGENT_MUX_SWARM_PLAN" ]; then
  cp "$AGENT_MUX_SWARM_PLAN" "$r/planner-context.json"
  esc=$(sed 's/\\/\\\\/g; s/"/\\"/g' '{plan}' | awk 'BEGIN{{ORS="\\n"}} {{print}}' | sed 's/\\n$//')
  printf '{{"type":"result","result":"%s","total_cost_usd":0.01,"usage":{{"input_tokens":10,"output_tokens":5}}}}\n' "$esc"
  exit 0
fi
if [ -n "$AGENT_MUX_TASK_ID" ]; then
  printf '%s\n' "$@" > "$r/worker-$AGENT_MUX_TASK_ID.txt"
  '{mux}' bus done "$AGENT_MUX_TASK_ID" --workspace '{ws}' --db '{db}' --result "did $AGENT_MUX_TASK_ID" > "$r/done-$AGENT_MUX_TASK_ID.txt" 2>&1
  echo "worker finished"
  sleep 600
fi
exit 0
"#,
            calls = bin.join("calls").display(),
            plan = plan_file.display(),
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

fn fixture(plan_text: &str) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join("skills")).unwrap();
    let db = temp.path().join("store").join("traces.db");
    let ws = temp.path().join("proj");
    std::fs::create_dir_all(&ws).unwrap();
    let ws = ws.canonicalize().unwrap();
    std::fs::write(ws.join("README.md"), "hi\n").unwrap();
    let bin = temp.path().join("bin");
    let script = fake_claude(&bin, &db, &ws, plan_text);
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
    f.app
        .start_orchestrated_swarm(StartSwarm {
            workspace: f.ws.clone(),
            goal: "Add the table and its queries".into(),
            planner: Some("claude".into()),
            max_workers: 2,
            isolation: TaskIsolation::Shared,
            gate,
            check: None,
        })
        .unwrap_or_else(|e| panic!("the swarm did not start: {e}"));
}

#[tokio::test]
async fn a_swarm_plans_waits_for_approval_and_runs_its_tasks_in_order() {
    let mut f = fixture(PLAN);
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
    assert!(agent_mux::bus::store::plan_blocks(&conn, &ws).is_some());

    let done = f
        .app
        .swarm_action(&f.ws, SwarmVerb::Approve, None, None)
        .unwrap();
    assert!(done.starts_with("plan approved: 2 task(s)"), "{done}");

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
    assert_eq!(live_workers, 2);
    let closed = f
        .app
        .swarm_action(&f.ws, SwarmVerb::Close, None, None)
        .unwrap();
    assert!(
        closed.starts_with("closed: stopped 2 session(s)"),
        "{closed}"
    );
    assert!(swarm(&f).is_none(), "the swarm ended");
    f.app.kill_all();
}

#[tokio::test]
async fn a_broken_plan_stays_in_review_with_the_planners_error() {
    let mut f = fixture("I could not decide on a plan.");
    start(&mut f, false);
    assert!(
        pump_until(&mut f, Duration::from_secs(60), |f| {
            swarm(f).is_some_and(|s| s.status == SwarmStatus::Review)
        })
        .await,
        "{:?}",
        swarm(&f)
    );
    let s = swarm(&f).unwrap();
    assert!(
        s.plan_error
            .as_deref()
            .is_some_and(|e| e.contains("no `swarm-plan` block")),
        "{s:?}"
    );
    assert!(board(&f).is_empty());
    let err = f
        .app
        .swarm_action(&f.ws, SwarmVerb::Approve, None, None)
        .unwrap_err();
    assert!(err.contains("the plan has no tasks"), "{err}");
    f.app.kill_all();
}
