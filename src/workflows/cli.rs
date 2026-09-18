//! `agent-mux workflow …`: the Workflows section headless. `run` and
//! `plan` drive a private `App` the way `loop run --now` does, so the CLI
//! and the TUI share one runtime.

use crate::app::workflows::{PlanRequest, WorkflowRunRequest};
use crate::harness::Harness;
use crate::workflows::{library, report as wreport, store as wstore};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const USAGE: &str = "agent-mux workflow <command>

  ls [--json]                      workflows (built-in, library, skill-distributed) with their last run
  show <name>                      the document
  check [<name>]                   validate every document and step skill; exit 1 on problems
  skills                           the step skills and the harnesses they are installed for
  run <name> --workspace DIR [--harness claude|codex|agy] [--profile P]
      [--arg name=value …] [--budget N] [--max-cost USD] [--isolation none|worktree]
      [--resume RUN_ID] [--json]   run to completion and print the result;
                                   exit 0 finished, 1 failed, 2 cancelled, 3 budget exhausted
  plan \"<task>\" --workspace DIR [--harness H] [--profile P] [--budget N] [--run] [--save NAME] [--json]
                                   compose a workflow for the task (and run or save it)
  runs [--json]                    recent runs
  status <run_id> [--json]         a run's sessions and result
  cancel <run_id>                  cancel a live run of this process (headless runs only)
  save <run_id> <name>             a run's document into the library

Documents live in ~/.agent-mux/workflows (AGENT_MUX_LIBRARY_DIR overrides); the
built-in ones are review-changes, understand, research, audit-until-dry,
judge-panel, migrate and triage-route. docs/workflows.md explains the format.";

struct Args {
    values: HashMap<String, String>,
    multi: Vec<(String, String)>,
    flags: Vec<String>,
    positional: Vec<String>,
}

const VALUE_FLAGS: &[&str] = &[
    "--workspace",
    "--harness",
    "--profile",
    "--budget",
    "--max-cost",
    "--isolation",
    "--resume",
    "--save",
    "--arg",
];

impl Args {
    fn parse(raw: &[String]) -> Args {
        let mut a = Args {
            values: HashMap::new(),
            multi: Vec::new(),
            flags: Vec::new(),
            positional: Vec::new(),
        };
        let mut i = 0;
        while i < raw.len() {
            let t = &raw[i];
            if VALUE_FLAGS.contains(&t.as_str()) {
                if let Some(v) = raw.get(i + 1) {
                    if t == "--arg" {
                        a.multi.push((t.clone(), v.clone()));
                    } else {
                        a.values.insert(t.clone(), v.clone());
                    }
                    i += 2;
                    continue;
                }
            } else if let Some((k, v)) = t.split_once('=')
                && VALUE_FLAGS.contains(&k)
            {
                if k == "--arg" {
                    a.multi.push((k.to_string(), v.to_string()));
                } else {
                    a.values.insert(k.to_string(), v.to_string());
                }
                i += 1;
                continue;
            }
            if t.starts_with("--") {
                a.flags.push(t.clone());
            } else {
                a.positional.push(t.clone());
            }
            i += 1;
        }
        a
    }
    fn value(&self, k: &str) -> Option<&str> {
        self.values.get(k).map(String::as_str)
    }
    fn flag(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f == k)
    }
}

fn parse_harness(s: &str) -> anyhow::Result<Harness> {
    Harness::detect(s).ok_or_else(|| anyhow::anyhow!("unknown harness {s:?}: claude, codex or agy"))
}

fn workspace_of(args: &Args) -> anyhow::Result<PathBuf> {
    let ws = args
        .value("--workspace")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let ws = ws
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("{}: {e}", ws.display()))?;
    Ok(ws)
}

fn all_skills() -> Vec<crate::skill::SkillDefinition> {
    crate::skill::load_skills(None).0
}

fn entries() -> Vec<library::Entry> {
    library::load(&crate::assets::root(), &all_skills())
}

fn open_store_ro() -> Option<rusqlite::Connection> {
    let cfg = crate::config::load().ok()?;
    let resolved =
        crate::config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())?;
    crate::tracing::store::open_ro(&resolved.db_path).ok()
}

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("ls") | Some("list") => ls(&Args::parse(rest)),
        Some("show") => show(&Args::parse(rest)),
        Some("check") => check(&Args::parse(rest)),
        Some("skills") => skills(),
        Some("run") => run_workflow(&Args::parse(rest)).await,
        Some("plan") => plan(&Args::parse(rest)).await,
        Some("runs") => runs(&Args::parse(rest)),
        Some("status") => status(&Args::parse(rest)),
        Some("cancel") => {
            anyhow::bail!(
                "cancel applies to the TUI's live runs (x in the Workflows section); a headless run is cancelled with Ctrl-C"
            )
        }
        Some("save") => save(&Args::parse(rest)),
        Some("help") | Some("--help") | Some("-h") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown workflow command {other:?}\n\n{USAGE}"),
    }
}

fn ls(args: &Args) -> anyhow::Result<()> {
    let entries = entries();
    let conn = open_store_ro();
    let last = |name: &str| -> Option<wstore::WorkflowRun> {
        conn.as_ref()
            .and_then(|c| wstore::recent_runs(c, Some(name), 1).ok())
            .and_then(|v| v.into_iter().next())
    };
    if args.flag("--json") {
        let items: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                let l = last(&e.name);
                serde_json::json!({
                    "name": e.name,
                    "source": e.source.label(),
                    "description": e.doc.as_ref().map(|d| d.description.clone()),
                    "when_to_use": e.doc.as_ref().and_then(|d| d.when_to_use.clone()),
                    "steps": e.doc.as_ref().map(|d| d.steps.len()).unwrap_or(0),
                    "problems": e.problems,
                    "last_run": l.map(|r| serde_json::json!({
                        "id": r.id, "status": r.status, "harness": r.harness,
                        "sessions": r.sessions, "tokens": r.tokens, "cost_usd": r.cost_usd,
                    })),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    for e in &entries {
        let status = if e.valid() {
            String::new()
        } else {
            format!("  ! {}", e.problems[0])
        };
        let l = last(&e.name)
            .map(|r| {
                format!(
                    "last {} on {} ({} sessions)",
                    r.status, r.harness, r.sessions
                )
            })
            .unwrap_or_else(|| "never run".into());
        println!(
            "{:<22} {:<10} {:<40} {}{}",
            e.name,
            e.source.label(),
            l,
            e.doc
                .as_ref()
                .map(|d| d.description.clone())
                .unwrap_or_default(),
            status
        );
    }
    Ok(())
}

fn show(args: &Args) -> anyhow::Result<()> {
    let name = args
        .positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: agent-mux workflow show <name>"))?;
    let entries = entries();
    let e = library::find(&entries, name).ok_or_else(|| anyhow::anyhow!("no workflow {name:?}"))?;
    print!("{}", e.text);
    Ok(())
}

fn check(args: &Args) -> anyhow::Result<()> {
    let entries = entries();
    let only = args.positional.first();
    let mut bad = 0;
    for e in &entries {
        if let Some(n) = only
            && &e.name != n
        {
            continue;
        }
        if e.valid() {
            println!(
                "{}: ok ({} steps)",
                e.name,
                e.doc.as_ref().map(|d| d.steps.len()).unwrap_or(0)
            );
        } else {
            for p in &e.problems {
                println!("{}: {p}", e.name);
                bad += 1;
            }
        }
    }
    let skills = all_skills();
    let missing: Vec<&str> = library::BUILTIN
        .iter()
        .filter_map(|(_, t)| crate::workflows::parse(t).ok())
        .flat_map(|d| d.skills())
        .filter(|s| !skills.iter().any(|k| k.id == *s))
        .collect::<Vec<String>>()
        .leak()
        .iter()
        .map(String::as_str)
        .collect();
    for m in missing {
        println!("step skill {m}: missing");
        bad += 1;
    }
    if bad == 0 {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

fn skills() -> anyhow::Result<()> {
    let home = crate::skill::install::home_dir();
    for s in all_skills()
        .iter()
        .filter(|s| s.id.starts_with("wf-") || s.id == "workflow-author")
    {
        let mut where_: Vec<String> = Vec::new();
        for h in Harness::ALL {
            let st = crate::skill::install::status(s, h, &home);
            if st.installed {
                where_.push(format!(
                    "{}{}",
                    h.as_str(),
                    if st.current { "" } else { " (stale)" }
                ));
            }
        }
        println!(
            "{:<20} {:<9} {}",
            s.id,
            if s.writes { "writes" } else { "" },
            if where_.is_empty() {
                "not installed".to_string()
            } else {
                where_.join(", ")
            }
        );
    }
    Ok(())
}

/// A private App with the trace runtime, for headless runs.
struct Headless {
    app: crate::app::App,
    rx: tokio::sync::mpsc::Receiver<crate::events::AppEvent>,
}

fn headless() -> anyhow::Result<Headless> {
    let cfg = crate::config::load()?;
    let resolved = crate::config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())
        .ok_or_else(|| anyhow::anyhow!("tracing is disabled; a workflow run needs the store"))?;
    let (tx, rx) = tokio::sync::mpsc::channel(1024);
    let runtime = crate::tracing::TraceRuntime::new(resolved, tx.clone())
        .map_err(|e| anyhow::anyhow!("trace store: {e}"))?;
    let mut app = crate::app::App::new(cfg.profiles, Some(runtime), tx);
    app.clipboard_enabled = false;
    app.set_pane_size(40, 120);
    app.workflows = crate::config::resolve_workflows(cfg.workflows.as_ref());
    app.loops = crate::config::resolve_loops(cfg.loops.as_ref());
    app.config_path = cfg.loaded_from.clone();
    app.load_loop_registry();
    Ok(Headless { app, rx })
}

async fn pump(
    h: &mut Headless,
    deadline: Duration,
    mut done: impl FnMut(&crate::app::App) -> bool,
) -> bool {
    use crate::events::AppEvent;
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        h.app.on_tick(Instant::now());
        if done(&h.app) {
            return true;
        }
        match tokio::time::timeout(Duration::from_millis(100), h.rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                h.app.handle_pty_output(id, &bytes, Instant::now())
            }
            Ok(Some(AppEvent::PtyExit { id })) => h.app.handle_pty_exit(id),
            Ok(Some(AppEvent::TraceStats { launch_id, stats })) => {
                h.app.handle_trace_stats(&launch_id, stats)
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    h.app.on_tick(Instant::now());
    done(&h.app)
}

async fn shutdown(mut h: Headless) {
    h.app.kill_all();
    if let Some(rt) = h.app.take_tracing() {
        rt.shutdown(Duration::from_secs(5)).await;
    }
}

fn parse_args_flags(args: &Args) -> anyhow::Result<serde_json::Value> {
    let mut m = serde_json::Map::new();
    for (_, kv) in &args.multi {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--arg needs name=value, got {kv:?}"))?;
        let value = serde_json::from_str::<serde_json::Value>(v)
            .unwrap_or(serde_json::Value::String(v.to_string()));
        m.insert(k.to_string(), value);
    }
    Ok(if m.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(m)
    })
}

async fn run_workflow(args: &Args) -> anyhow::Result<()> {
    let name = args.positional.first().cloned().ok_or_else(|| {
        anyhow::anyhow!("usage: agent-mux workflow run <name> --workspace DIR [--harness H] …")
    })?;
    let entries = entries();
    let entry =
        library::find(&entries, &name).ok_or_else(|| anyhow::anyhow!("no workflow {name:?}"))?;
    if !entry.valid() {
        anyhow::bail!("{name}: {}", entry.problems.join("; "));
    }
    let workspace = workspace_of(args)?;
    let harness = match args.value("--harness") {
        Some(h) => parse_harness(h)?,
        None => Harness::Claude,
    };
    let req = WorkflowRunRequest {
        name: entry.name.clone(),
        source: entry.source.label(),
        document: entry.text.clone(),
        workspace,
        profile: args.value("--profile").map(str::to_string),
        harness,
        args: parse_args_flags(args)?,
        budget_tokens: args.value("--budget").map(|b| b.parse()).transpose()?,
        usd_cap: args.value("--max-cost").map(|b| b.parse()).transpose()?,
        isolation: match args.value("--isolation") {
            Some("worktree") => Some(crate::workflows::document::Isolation::Worktree),
            Some("none") => Some(crate::workflows::document::Isolation::None),
            Some(other) => anyhow::bail!("--isolation must be none or worktree, not {other:?}"),
            None => None,
        },
        resume_from: args.value("--resume").map(str::to_string),
    };
    let mut h = headless()?;
    let run_id = h
        .app
        .start_workflow_run(req)
        .map_err(|e| anyhow::anyhow!(e))?;
    if !args.flag("--json") {
        eprintln!("run {run_id} started");
    }
    let timeout = Duration::from_secs(h.app.workflows.run_timeout_s + 60);
    let mut seen_notes = 0;
    let finished = pump(&mut h, timeout, |a| {
        if !args.flag("--json")
            && let Some(r) = a.live_workflow_runs.first()
        {
            for n in &r.state.notes[seen_notes..] {
                eprintln!("  {n}");
            }
            seen_notes = r.state.notes.len();
        }
        a.live_workflow_runs.is_empty()
    })
    .await;
    let recent = h.app.recent_workflow_runs.first().cloned();
    shutdown(h).await;
    let Some(r) = recent.filter(|_| finished) else {
        anyhow::bail!("the run did not finish");
    };
    if args.flag("--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run_id": r.run_id, "workflow": r.name, "status": r.status, "harness": r.harness,
                "sessions": r.sessions, "tokens": r.tokens, "cost_usd": r.cost_usd,
                "result": r.result, "error": r.error, "notes": r.notes,
            }))?
        );
    } else {
        println!(
            "{} {}: {} sessions, {}k tokens, ${:.2}",
            r.name,
            r.status,
            r.sessions,
            r.tokens / 1000,
            r.cost_usd
        );
        if let Some(e) = &r.error {
            println!("error: {e}");
        }
        match &r.result {
            serde_json::Value::String(s) => println!("{s}"),
            serde_json::Value::Null => {}
            other => println!("{}", serde_json::to_string_pretty(other)?),
        }
    }
    std::process::exit(match r.status.as_str() {
        "finished" => 0,
        "cancelled" => 2,
        "budget-exhausted" => 3,
        _ => 1,
    })
}

async fn plan(args: &Args) -> anyhow::Result<()> {
    let task = args.positional.join(" ");
    if task.trim().is_empty() {
        anyhow::bail!(
            "usage: agent-mux workflow plan \"<task>\" --workspace DIR [--run] [--save NAME]"
        );
    }
    let workspace = workspace_of(args)?;
    let harness = match args.value("--harness") {
        Some(h) => parse_harness(h)?,
        None => Harness::Claude,
    };
    let mut h = headless()?;
    let req = PlanRequest {
        task: task.clone(),
        workspace,
        harness,
        profile: args.value("--profile").map(str::to_string),
        budget_tokens: args.value("--budget").map(|b| b.parse()).transpose()?,
        auto_run: false,
    };
    let plan_id = h
        .app
        .start_workflow_plan(req)
        .map_err(|e| anyhow::anyhow!(e))?;
    let timeout = Duration::from_secs(h.app.workflows.session_timeout_s + 30);
    let planned = pump(&mut h, timeout, |a| {
        a.planned_workflows.iter().any(|p| p.id == plan_id)
    })
    .await;
    if !planned {
        shutdown(h).await;
        anyhow::bail!("the planner did not answer in time");
    }
    let p = h
        .app
        .planned_workflows
        .iter()
        .find(|p| p.id == plan_id)
        .cloned()
        .expect("planned");
    if let Some(name) = args.value("--save") {
        match h.app.save_planned_workflow(&plan_id, name) {
            Ok(path) => eprintln!("saved {}", path.display()),
            Err(e) => eprintln!("save: {e}"),
        }
    }
    if args.flag("--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "plan_id": p.id, "task": p.task, "name": p.name, "harness": p.harness.as_str(),
                "document": p.document, "problems": p.problems, "raw": p.raw,
            }))?
        );
    } else {
        print!("{}", p.document);
        for pr in &p.problems {
            eprintln!("! {pr}");
        }
        if let Some(raw) = &p.raw {
            eprintln!("--- the planner answered:\n{raw}");
        }
    }
    if !p.valid() {
        shutdown(h).await;
        std::process::exit(1);
    }
    if args.flag("--run") {
        let run_id = h
            .app
            .run_planned_workflow(&plan_id)
            .map_err(|e| anyhow::anyhow!(e))?;
        eprintln!("run {run_id} started");
        let timeout = Duration::from_secs(h.app.workflows.run_timeout_s + 60);
        let finished = pump(&mut h, timeout, |a| a.live_workflow_runs.is_empty()).await;
        let recent = h.app.recent_workflow_runs.first().cloned();
        shutdown(h).await;
        let Some(r) = recent.filter(|_| finished) else {
            anyhow::bail!("the run did not finish");
        };
        println!("{} {}: {} sessions", r.name, r.status, r.sessions);
        match &r.result {
            serde_json::Value::String(s) => println!("{s}"),
            serde_json::Value::Null => {}
            other => println!("{}", serde_json::to_string_pretty(other)?),
        }
        std::process::exit(if r.status == "finished" { 0 } else { 1 });
    }
    shutdown(h).await;
    Ok(())
}

fn runs(args: &Args) -> anyhow::Result<()> {
    let Some(conn) = open_store_ro() else {
        anyhow::bail!("no trace store");
    };
    let rows = wstore::recent_runs(&conn, None, 30)?;
    if args.flag("--json") {
        let v: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id, "workflow": r.workflow, "source": r.source, "workspace": r.workspace,
                    "harness": r.harness, "status": r.status, "sessions": r.sessions,
                    "tokens": r.tokens, "cost_usd": r.cost_usd, "started_ns": r.started_ns, "ended_ns": r.ended_ns,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    for r in rows {
        println!(
            "{}  {:<18} {:<8} {:<16} {} sessions  {}",
            &r.id[..8],
            r.workflow,
            r.harness,
            r.status,
            r.sessions,
            r.workspace
        );
    }
    Ok(())
}

fn status(args: &Args) -> anyhow::Result<()> {
    let id = args
        .positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: agent-mux workflow status <run_id>"))?;
    let Some(conn) = open_store_ro() else {
        anyhow::bail!("no trace store");
    };
    let run = wstore::resolve_run(&conn, id)?.ok_or_else(|| anyhow::anyhow!("no run {id:?}"))?;
    let steps = wstore::steps_of(&conn, &run.id)?;
    if args.flag("--json") {
        let v = serde_json::json!({
            "id": run.id, "workflow": run.workflow, "status": run.status, "harness": run.harness,
            "workspace": run.workspace, "args": run.args, "sessions": run.sessions, "tokens": run.tokens,
            "cost_usd": run.cost_usd, "result": run.result, "error": run.error,
            "steps": steps.iter().map(|s| serde_json::json!({
                "session": s.session, "step": s.step_id, "phase": s.phase, "harness": s.harness,
                "kind": s.kind, "tokens": s.tokens, "cost_usd": s.cost_usd, "launch_id": s.launch_id,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let notes = run_notes(&run.id);
    let doc = crate::workflows::document::parse(&run.document).ok();
    let report = wreport::build(wreport::RunView {
        workflow: &run.workflow,
        status: &run.status,
        harness: &run.harness,
        workspace: &run.workspace,
        sessions: run.sessions,
        tokens: run.tokens.unwrap_or(0).max(0) as u64,
        cost_usd: run.cost_usd,
        duration_s: run.ended_ns.map(|e| (e - run.started_ns) / 1_000_000_000),
        result: &run.result,
        error: run.error.as_deref(),
        notes: &notes,
        doc: doc.as_ref(),
        steps: &steps,
    });
    // `--result` pipes the answer alone; `--steps` prints the ledger.
    if args.flag("--result") {
        match &run.result {
            serde_json::Value::Null => {}
            serde_json::Value::String(s) => println!("{s}"),
            other => println!("{}", serde_json::to_string_pretty(other)?),
        }
        return Ok(());
    }
    if args.flag("--steps") {
        let reasons = run_reasons(&run.id);
        for s in &steps {
            let dur = match (s.started_ns, s.ended_ns) {
                (Some(a), Some(b)) if b > a => wreport::format_duration((b - a) / 1_000_000_000),
                _ => "-".into(),
            };
            println!(
                "  {} {:<26} {:<14} {:<7} {:>8} {:>8}",
                if s.kind == "null" { "✗" } else { "✓" },
                s.session,
                s.phase,
                s.kind,
                crate::loops::format_tokens(s.tokens.unwrap_or(0).max(0) as u64),
                dur
            );
            if let Some(r) = reasons.get(&s.session) {
                println!("      reason  {r}");
            }
        }
        return Ok(());
    }
    for l in wreport::text_lines(&report) {
        println!("{l}");
    }
    println!();
    println!(
        "  result: agent-mux workflow status {} --result",
        &run.id[..8]
    );
    println!(
        "  steps:  agent-mux workflow status {} --steps",
        &run.id[..8]
    );
    Ok(())
}

/// The notes and the null reasons a finished run wrote about itself.
fn run_dir(run_id: &str) -> Option<std::path::PathBuf> {
    Some(crate::workflows::context::run_dir(
        &crate::tracing::analysis::default_snapshot_dir(),
        run_id,
    ))
}

fn run_notes(run_id: &str) -> Vec<String> {
    let Some(dir) = run_dir(run_id) else {
        return Vec::new();
    };
    std::fs::read_to_string(dir.join("result.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("notes").and_then(|n| {
                n.as_array().map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect()
                })
            })
        })
        .unwrap_or_default()
}

fn run_reasons(run_id: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(dir) = run_dir(run_id) else {
        return out;
    };
    if let Ok(text) = std::fs::read_to_string(dir.join("journal.jsonl")) {
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if let (Some(k), Some(r)) = (
                v.get("key").and_then(|k| k.as_str()),
                v.get("reason").and_then(|r| r.as_str()),
            ) {
                out.insert(k.to_string(), r.to_string());
            }
        }
    }
    out
}

fn save(args: &Args) -> anyhow::Result<()> {
    let (Some(id), Some(name)) = (args.positional.first(), args.positional.get(1)) else {
        anyhow::bail!("usage: agent-mux workflow save <run_id> <name>");
    };
    let Some(conn) = open_store_ro() else {
        anyhow::bail!("no trace store");
    };
    let run = wstore::resolve_run(&conn, id)?.ok_or_else(|| anyhow::anyhow!("no run {id:?}"))?;
    let path = crate::app::workflows::save_document(&crate::assets::root(), name, &run.document)
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("{}", path.display());
    Ok(())
}
