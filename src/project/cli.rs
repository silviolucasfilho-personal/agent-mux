//! `agent-mux project …`: a repository configured with spec-wave, read as
//! a Project. Phase 1: every command reads.

use super::config::{self, ProjectConfig};
use super::contract::stage_plain;
use super::next::{self, Decision};
use super::run::{self as steps, Planned};
use super::{DocState, Item, ItemType, Snapshot, doctor, ops, order, snapshot, stage, store};
use crate::factory::deliver::Tools;
use crate::harness::Harness;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const USAGE: &str = "agent-mux project — a spec-wave repository's work items, read from GitHub

  agent-mux project doctor [--json]          what can be read, and what to fix
  agent-mux project sync                     read the Project from GitHub into the cache
  agent-mux project ls [--type T] [--stage S] [--milestone M] [--all] [--json]
  agent-mux project show <n> [--json]        one item: stage, documents, pull requests, next step
  agent-mux project next <n> [--json]        the item's next step and why; runs nothing
  agent-mux project order [<feature>] [--milestone M] [--json]
        the Stories of a Feature in execution order; with no Feature, every one with work

  agent-mux project new --type T --title \"…\" [--parent N] [--priority P] [--area A] [--body \"…\"]
        create a work item, put it on the board at its type's first stage
  agent-mux project move <n> <stage> [--status S]
        advance an item; a stage never goes back
  agent-mux project repair-stage <n[,m…]> <stage> --yes --reason \"…\" [--status S]
        put items at a stage whatever the order says, with a note on each issue
  agent-mux project task start|done <n>      a Task: in progress, or done
  agent-mux project story review <n>         a Story to code review
  agent-mux project triage accept <n> [--severity P]
  agent-mux project triage reject <n> --reason \"…\"
  agent-mux project triage duplicate <n> --of <original>
  agent-mux project advance [--yes]
        the board moves the pull requests already justify; without --yes, only the plan

  agent-mux project run <n> [--step spec|plan|bug|critique|validate] [--harness H] [--critic H] [--dry-run]
        run the item's next document step: a read-only session writes the document,
        agent-mux publishes it by pull request and comments on the issue; a plan and
        a bug document are then audited by a second session

  every command: [--dir DIR] [--refresh] [--cached]

The folder (or one above it) must hold .spec-wave.json. In a code repository
the Project shown is its hub's. A read is cached for `cache.ttlSec` of that
file (600 s when unset): --refresh reads again, --cached never does.
The commands of the first block only read. The others write to GitHub (issues,
labels, comments, the board) and never to .spec-wave.json. docs/project.md has
the rest.";

const VALUE_FLAGS: &[&str] = &[
    "--dir",
    "--type",
    "--stage",
    "--milestone",
    "--title",
    "--parent",
    "--priority",
    "--area",
    "--body",
    "--status",
    "--reason",
    "--severity",
    "--of",
    "--step",
    "--harness",
    "--critic",
];

struct Args {
    values: Vec<(String, String)>,
    flags: Vec<String>,
    positional: Vec<String>,
}

impl Args {
    fn parse(raw: &[String]) -> Args {
        let mut a = Args {
            values: Vec::new(),
            flags: Vec::new(),
            positional: Vec::new(),
        };
        let mut i = 0;
        while i < raw.len() {
            let t = &raw[i];
            if VALUE_FLAGS.contains(&t.as_str())
                && let Some(v) = raw.get(i + 1)
            {
                a.values.push((t.clone(), v.clone()));
                i += 2;
                continue;
            }
            if let Some((k, v)) = t.split_once('=')
                && VALUE_FLAGS.contains(&k)
            {
                a.values.push((k.to_string(), v.to_string()));
            } else if t.starts_with("--") {
                a.flags.push(t.clone());
            } else {
                a.positional.push(t.clone());
            }
            i += 1;
        }
        a
    }

    fn value(&self, k: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(f, _)| f == k)
            .map(|(_, v)| v.as_str())
    }

    fn flag(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f == k)
    }
}

/// How fresh a snapshot must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fresh {
    /// The cache when it is younger than the Project's TTL, else a read.
    Auto,
    /// Always read GitHub.
    Refresh,
    /// Never read GitHub.
    Cached,
}

/// The trace store's path, or why there is none.
pub fn db_path() -> Result<PathBuf, String> {
    let cfg = crate::config::load().map_err(|e| e.to_string())?;
    crate::config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())
        .map(|r| r.db_path)
        .ok_or_else(|| "tracing is disabled; a Project is cached in the trace store".to_string())
}

/// A read-write connection to the store, created when it is not there.
pub fn open_store(db: &Path) -> Result<rusqlite::Connection, String> {
    if !db.is_file() {
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        crate::tracing::store::open_rw(db, crate::tracing::store::OpenOptions::default())?;
    }
    crate::tracing::store::open_aux(db)
}

/// The cache's lifetime for a Project, in seconds: `cache.ttlSec`, 600
/// when unset, 0 turns it off.
pub fn ttl_secs(cfg: &ProjectConfig) -> i64 {
    cfg.pointer("/cache/ttlSec")
        .and_then(Value::as_i64)
        .unwrap_or(600)
        .max(0)
}

/// A snapshot of the Project: from the cache when `fresh` allows it,
/// else from GitHub, saving what was read.
pub fn obtain(
    conn: &rusqlite::Connection,
    tools: &Tools,
    cfg: &ProjectConfig,
    fresh: Fresh,
) -> Result<Snapshot, String> {
    let hub = cfg.hub_repo().slug();
    let cached = store::load(conn, &hub)?;
    let age_ok = |s: &Snapshot| {
        let age = (super::now_ns() - s.synced_ns) / 1_000_000_000;
        age >= 0 && age < ttl_secs(cfg)
    };
    match (fresh, cached) {
        (Fresh::Cached, Some(s)) => Ok(s),
        (Fresh::Cached, None) => Err(format!(
            "{hub} was never read: run `agent-mux project sync`"
        )),
        (Fresh::Auto, Some(s)) if age_ok(&s) => Ok(s),
        _ => {
            let snap = snapshot::load(tools, cfg)?;
            store::save(conn, &snap)?;
            Ok(snap)
        }
    }
}

struct Env {
    cfg: ProjectConfig,
    conn: rusqlite::Connection,
    tools: Tools,
    fresh: Fresh,
}

fn env(a: &Args) -> anyhow::Result<Env> {
    let dir = match a.value("--dir") {
        Some(d) => PathBuf::from(d),
        None => std::env::current_dir()?,
    };
    let dir = dir
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?;
    let cfg = config::load(&dir).map_err(|e| anyhow::anyhow!(e))?;
    let db = db_path().map_err(|e| anyhow::anyhow!(e))?;
    let conn = open_store(&db).map_err(|e| anyhow::anyhow!(e))?;
    let fresh = if a.flag("--refresh") {
        Fresh::Refresh
    } else if a.flag("--cached") {
        Fresh::Cached
    } else {
        Fresh::Auto
    };
    Ok(Env {
        cfg,
        conn,
        tools: Tools::default(),
        fresh,
    })
}

fn snap(e: &Env) -> anyhow::Result<Snapshot> {
    obtain(&e.conn, &e.tools, &e.cfg, e.fresh).map_err(|e| anyhow::anyhow!(e))
}

fn number(a: &Args) -> anyhow::Result<u64> {
    let raw = a
        .positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("which issue? give its number, as 12 or #12"))?;
    raw.trim_start_matches('#')
        .parse()
        .map_err(|_| anyhow::anyhow!("\"{raw}\" is not an issue number"))
}

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let rest = args.get(1..).unwrap_or_default();
    let a = Args::parse(rest);
    match args.first().map(String::as_str) {
        Some("doctor") => cmd_doctor(&a),
        Some("sync") => cmd_sync(&a),
        Some("ls") | Some("list") => cmd_ls(&a),
        Some("show") => cmd_show(&a),
        Some("next") => cmd_next(&a),
        Some("order") => cmd_order(&a),
        Some("new") => write_cmd(&a, |ops, a| {
            let item_type = match a.value("--type") {
                Some(t) => Some(
                    ItemType::parse(t).ok_or_else(|| format!("\"{t}\" is not a work item type"))?,
                ),
                None => None,
            };
            ops.new_item(&ops::NewItem {
                item_type,
                title: a.value("--title").unwrap_or_default().to_string(),
                parent: a.value("--parent").map(issue_number).transpose()?,
                priority: a.value("--priority").map(str::to_string),
                area: a.value("--area").map(str::to_string),
                body: a.value("--body").unwrap_or_default().to_string(),
            })
        }),
        Some("move") => write_cmd(&a, |ops, a| {
            let (n, stage) = two(a, "an issue and a stage: project move 12 \"code review\"")?;
            ops.move_item(issue_number(n)?, stage, a.value("--status"))
        }),
        Some("repair-stage") => write_cmd(&a, |ops, a| {
            let (list, stage) = two(
                a,
                "issues and a stage: project repair-stage 529,530 ready --yes --reason \"…\"",
            )?;
            let numbers = list
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(issue_number)
                .collect::<Result<Vec<u64>, String>>()?;
            ops.repair_stage(
                &numbers,
                stage,
                a.value("--status"),
                a.flag("--yes"),
                a.value("--reason").unwrap_or_default(),
            )
        }),
        Some("task") => write_cmd(&a, |ops, a| {
            let (action, n) = two(a, "an action and a Task: project task start 12")?;
            ops.task(action, issue_number(n)?)
        }),
        Some("story") => write_cmd(&a, |ops, a| {
            let (action, n) = two(a, "an action and a Story: project story review 12")?;
            if action != "review" {
                return Err(format!("\"{action}\" is not a story action: review"));
            }
            ops.story_review(issue_number(n)?)
        }),
        Some("triage") => write_cmd(&a, |ops, a| {
            let (action, n) = two(a, "an outcome and a Bug: project triage accept 42")?;
            ops.triage(
                action,
                issue_number(n)?,
                a.value("--severity"),
                a.value("--reason"),
                a.value("--of").map(issue_number).transpose()?,
            )
        }),
        Some("advance") => cmd_advance(&a),
        Some("run") => cmd_run(&a).await,
        None | Some("help") | Some("--help") | Some("-h") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown subcommand: project {other}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn issue_number(raw: &str) -> Result<u64, String> {
    raw.trim()
        .trim_start_matches('#')
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("\"{raw}\" is not an issue number"))
}

fn two<'a>(a: &'a Args, what: &str) -> Result<(&'a str, &'a str), String> {
    match (a.positional.first(), a.positional.get(1)) {
        (Some(x), Some(y)) => Ok((x.as_str(), y.as_str())),
        _ => Err(format!("this needs {what}")),
    }
}

/// Brings the cached copies of `touched` up to what GitHub has now, and
/// answers the snapshot when there is one. A cache that was never read is
/// left alone: the next read fills it.
pub fn refresh_touched(
    conn: &rusqlite::Connection,
    tools: &Tools,
    cfg: &ProjectConfig,
    touched: &[u64],
) -> Result<Option<Snapshot>, String> {
    if touched.is_empty() {
        return Ok(None);
    }
    let Some(mut snap) = store::load(conn, &cfg.hub_repo().slug())? else {
        return Ok(None);
    };
    let nodes = super::github::issue_nodes(tools, cfg, touched)?;
    snapshot::merge(&mut snap, &nodes, cfg);
    store::save(conn, &snap)?;
    Ok(Some(snap))
}

/// Runs one verb that writes, prints what it did, and refreshes the cache
/// for what it changed.
fn write_cmd(
    a: &Args,
    verb: impl FnOnce(&ops::Ops, &Args) -> Result<ops::Report, String>,
) -> anyhow::Result<()> {
    let e = env(a)?;
    let seen_board = store::load(&e.conn, &e.cfg.hub_repo().slug())
        .ok()
        .flatten()
        .and_then(|s| s.board_id);
    let ops = ops::Ops {
        tools: &e.tools,
        cfg: &e.cfg,
        seen_board,
    };
    let report = verb(&ops, a).map_err(|err| anyhow::anyhow!(err))?;
    if a.flag("--json") {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for line in &report.lines {
            println!("{line}");
        }
    }
    if let Err(err) = refresh_touched(&e.conn, &e.tools, &e.cfg, &report.touched) {
        eprintln!("the cache was not refreshed ({err}); `agent-mux project sync` reads it again");
    }
    Ok(())
}

fn cmd_advance(a: &Args) -> anyhow::Result<()> {
    let mut e = env(a)?;
    // the plan is made from what GitHub says now
    e.fresh = Fresh::Refresh;
    let s = snap(&e)?;
    let moves = ops::auto_moves(&s);
    if moves.is_empty() {
        println!("the board is up to date with the pull requests");
        return Ok(());
    }
    if !a.flag("--yes") {
        for m in &moves {
            println!("#{}: {} → {} ({})", m.number, m.from, m.to, m.why);
        }
        println!("{} move(s); run again with --yes to make them", moves.len());
        return Ok(());
    }
    let ops = ops::Ops {
        tools: &e.tools,
        cfg: &e.cfg,
        seen_board: s.board_id.clone(),
    };
    let report = ops
        .apply_moves(&moves)
        .map_err(|err| anyhow::anyhow!(err))?;
    for line in &report.lines {
        println!("{line}");
    }
    let _ = refresh_touched(&e.conn, &e.tools, &e.cfg, &report.touched);
    Ok(())
}

/// The harnesses a Project step may run on: the ones agent-mux traces.
pub const HARNESSES: [Harness; 6] = [
    Harness::Claude,
    Harness::Codex,
    Harness::Antigravity,
    Harness::OpenCode,
    Harness::Muse,
    Harness::Cursor,
];

fn harness_of(raw: Option<&str>, default: Harness) -> Result<Harness, String> {
    let h = match raw {
        Some(name) => Harness::detect(name).ok_or_else(|| format!("unknown harness {name:?}"))?,
        None => default,
    };
    if !HARNESSES.contains(&h) {
        return Err(format!(
            "{} is not traced by agent-mux, and a Project step must leave a trace: use one of {}",
            h.as_str(),
            HARNESSES.map(|h| h.as_str()).join(", ")
        ));
    }
    Ok(h)
}

/// Runs one built-in flow headless in `workspace` and answers its
/// result. `model`, when given, is the model of its one step.
async fn run_flow(
    flow: &str,
    workspace: &Path,
    harness: Harness,
    args: Value,
    model: Option<String>,
) -> Result<Value, String> {
    use crate::app::workflows::WorkflowRunRequest;
    let entries = crate::workflows::cli::entries();
    let entry = crate::workflows::library::find(&entries, flow)
        .ok_or_else(|| format!("the {flow} workflow is missing from the library"))?;
    if !entry.valid() {
        return Err(format!("{flow}: {}", entry.problems.join("; ")));
    }
    let req = WorkflowRunRequest {
        name: entry.name.clone(),
        source: entry.source.label(),
        document: entry.text.clone(),
        workspace: workspace.to_path_buf(),
        profile: None,
        harness,
        args,
        budget_tokens: None,
        usd_cap: None,
        isolation: None,
        resume_from: None,
        worktree_base: None,
        step_overrides: model
            .map(|m| vec![("work".to_string(), "model".to_string(), m)])
            .unwrap_or_default(),
    };
    let mut h = crate::workflows::cli::headless().map_err(|e| e.to_string())?;
    let run_id = h.app.start_workflow_run(req)?;
    eprintln!("  {flow} on {} · run {run_id}", harness.as_str());
    let timeout = std::time::Duration::from_secs(h.app.workflows.run_timeout_s + 60);
    let finished =
        crate::workflows::cli::pump(&mut h, timeout, |a| a.live_workflow_runs.is_empty()).await;
    let recent = h.app.recent_workflow_runs.first().cloned();
    crate::workflows::cli::shutdown(h).await;
    let Some(recent) = recent.filter(|_| finished) else {
        return Err(format!("the run {run_id} did not finish"));
    };
    if recent.status != "finished" {
        return Err(recent
            .error
            .unwrap_or_else(|| format!("the run {run_id} ended {}", recent.status)));
    }
    Ok(recent.result)
}

/// The model a step runs with: the one spec-wave's file asks for, when
/// the harness takes that kind of id; else the harness's own.
fn step_model(
    cfg: &ProjectConfig,
    harness: Harness,
    action: &str,
    labels: &[String],
    attempt: u32,
) -> Option<String> {
    steps::model_for(cfg, action, labels, attempt)
        .filter(|m| harness == Harness::Claude && m.starts_with("claude"))
}

async fn cmd_run(a: &Args) -> anyhow::Result<()> {
    let mut e = env(a)?;
    let n = number(a)?;
    let harness =
        harness_of(a.value("--harness"), Harness::Claude).map_err(|x| anyhow::anyhow!(x))?;
    let critic = harness_of(a.value("--critic"), harness).map_err(|x| anyhow::anyhow!(x))?;
    // a step is decided on what GitHub says now, documents included
    e.fresh = Fresh::Refresh;
    let s = snap(&e)?;
    let item = s
        .item(n)
        .ok_or_else(|| anyhow::anyhow!("issue #{n} is not in the Project"))?
        .clone();
    let forced = match a.value("--step") {
        Some(step) => {
            Some(steps::forced_action(step, item.item_type).map_err(|x| anyhow::anyhow!(x))?)
        }
        None => None,
    };
    let detail =
        super::github::issue_detail(&e.tools, &e.cfg, n).map_err(|x| anyhow::anyhow!(x))?;
    let hub_root = steps::hub_root(&e.cfg).map_err(|x| anyhow::anyhow!(x))?;
    let ctx = steps::Ctx {
        tools: &e.tools,
        cfg: &e.cfg,
        scratch: steps::scratch_dir(&crate::tracing::analysis::default_snapshot_dir()),
        hub_root: hub_root.clone(),
    };
    println!("#{n} {}", item.title);
    let mut planned = ctx
        .plan(&item, &detail, forced)
        .map_err(|x| anyhow::anyhow!(x))?;
    let mut touched: Vec<u64> = Vec::new();
    let mut published: Option<(&'static str, String)> = None;
    loop {
        let report = match planned {
            Planned::Stopped(d) => {
                for line in decision_lines(&d) {
                    println!("  {line}");
                }
                if d.blocked.is_some() {
                    std::process::exit(1);
                }
                break;
            }
            Planned::NotBuilt(action) => {
                println!("  next: {}", action.as_str());
                println!("  agent-mux does not run this step yet");
                break;
            }
            Planned::Validate if a.flag("--dry-run") => {
                println!(
                    "  next: validate (checks the merged documents and, if they pass, adds the approval label)"
                );
                break;
            }
            Planned::Generate(ref g) if a.flag("--dry-run") => {
                println!(
                    "  next: write {} with the flow {} on {}",
                    g.path,
                    g.flow,
                    harness.as_str()
                );
                println!("  then: publish it on a pull request and comment on the issue");
                if let Some(k) = g.then_critique {
                    println!("  then: audit it ({}) on {}", k.as_str(), critic.as_str());
                }
                break;
            }
            Planned::Critique(ref job) if a.flag("--dry-run") => {
                println!(
                    "  next: audit {} (attempt {}/{}) on {}",
                    job.kind.document(),
                    job.attempt,
                    job.max,
                    critic.as_str()
                );
                break;
            }
            Planned::NeedsHuman { kind, attempt, max } if a.flag("--dry-run") => {
                println!(
                    "  next: stop for a person: the critique of {} failed {}/{max} times",
                    kind.document(),
                    attempt.saturating_sub(1)
                );
                break;
            }
            Planned::Validate => {
                let r = ctx.validate(&item).map_err(|x| anyhow::anyhow!(x))?;
                finish_report(&r, &mut touched);
                break;
            }
            Planned::NeedsHuman { kind, attempt, max } => {
                let r = ctx
                    .stop_for_human(&item, kind, attempt, max)
                    .map_err(|x| anyhow::anyhow!(x))?;
                finish_report(&r, &mut touched);
                break;
            }
            Planned::Generate(g) => {
                let model = step_model(&e.cfg, harness, g.doc, &item.labels, 1);
                let result = run_flow(g.flow, &hub_root, harness, g.args.clone(), model)
                    .await
                    .map_err(|x| anyhow::anyhow!(x))?;
                let (r, follow) = ctx
                    .finish_generate(&item, &detail, &g, &result)
                    .map_err(|x| anyhow::anyhow!(x))?;
                published = Some((g.doc, g.path.clone()));
                finish_report(&r, &mut touched);
                match follow {
                    Some(next) => {
                        planned = next;
                        continue;
                    }
                    None => break,
                }
            }
            Planned::Critique(job) => {
                let model = step_model(&e.cfg, critic, "critique", &item.labels, job.attempt);
                let said = model.clone().unwrap_or_else(|| critic.as_str().to_string());
                let result = run_flow(
                    steps::FLOW_CRITIQUE,
                    &hub_root,
                    critic,
                    job.args.clone(),
                    model,
                )
                .await
                .map_err(|x| anyhow::anyhow!(x))?;
                ctx.finish_critique(&item, &job, &result, &said)
                    .map_err(|x| anyhow::anyhow!(x))?
            }
        };
        finish_report(&report, &mut touched);
        break;
    }
    // the cache follows: labels from GitHub, and the document now in review
    if let Ok(Some(mut cached)) = refresh_touched(&e.conn, &e.tools, &e.cfg, &touched)
        && let Some((doc, path)) = published
        && let Some(it) = cached.items.iter_mut().find(|i| i.number == n)
    {
        it.docs.insert(
            doc.to_string(),
            super::Doc {
                state: DocState::Branch,
                path,
                pr: None,
                branch: Some(super::contract::artifact_branch(n, doc)),
            },
        );
        let _ = store::save(&e.conn, &cached);
    }
    Ok(())
}

fn finish_report(r: &ops::Report, touched: &mut Vec<u64>) {
    for line in &r.lines {
        println!("  {line}");
    }
    for n in &r.touched {
        if !touched.contains(n) {
            touched.push(*n);
        }
    }
}

fn cmd_doctor(a: &Args) -> anyhow::Result<()> {
    let e = env(a)?;
    // the doctor always reads: a cache would hide what it is asked about
    let read = snapshot::load(&e.tools, &e.cfg);
    if let Ok(s) = &read {
        let _ = store::save(&e.conn, s);
    }
    let probe = doctor::probe(&e.tools, &e.cfg);
    let checks = doctor::checks(&e.cfg, &read, &probe);
    if a.flag("--json") {
        println!("{}", serde_json::to_string_pretty(&checks)?);
    } else {
        println!("Project {}", e.cfg.hub_repo().slug());
        for c in &checks {
            println!("  {} {:<20} {}", c.status.mark(), c.name, c.detail);
        }
    }
    if doctor::failed(&checks) {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_sync(a: &Args) -> anyhow::Result<()> {
    let mut e = env(a)?;
    e.fresh = Fresh::Refresh;
    let s = snap(&e)?;
    let open = s.items.iter().filter(|i| i.open).count();
    println!(
        "{}: {} issues ({open} open), {} milestones{}",
        s.hub,
        s.items.len(),
        s.milestones.len(),
        if s.board_error.is_some() {
            ", board not read"
        } else {
            ""
        }
    );
    for w in &s.warnings {
        println!("  ! {w}");
    }
    Ok(())
}

fn cmd_ls(a: &Args) -> anyhow::Result<()> {
    let e = env(a)?;
    let s = snap(&e)?;
    let want_type = match a.value("--type") {
        Some(t) => Some(
            ItemType::parse(t).ok_or_else(|| anyhow::anyhow!("\"{t}\" is not a work item type"))?,
        ),
        None => None,
    };
    let want_stage = match a.value("--stage") {
        Some(st) => Some(stage::resolve(st).map_err(|e| anyhow::anyhow!(e))?),
        None => None,
    };
    let milestone = a.value("--milestone");
    let rows: Vec<&Item> = s
        .items
        .iter()
        .filter(|i| a.flag("--all") || i.open)
        .filter(|i| want_type.is_none_or(|t| i.item_type == Some(t)))
        .filter(|i| want_stage.is_none_or(|st| stage::shown(i).stage == st))
        .filter(|i| milestone.is_none_or(|m| i.milestone.as_deref() == Some(m)))
        .collect();
    if a.flag("--json") {
        let list: Vec<Value> = rows.iter().map(|i| item_json(i, &s)).collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(());
    }
    for i in &rows {
        println!("{}", row_line(i));
    }
    println!("{} item(s) of {}", rows.len(), s.hub);
    Ok(())
}

/// One list row: number, type, stage, priority, milestone, title.
pub fn row_line(i: &Item) -> String {
    let shown = stage::shown(i);
    format!(
        "#{:<5} {} {:<16}{} {:<3} {:<10} {}",
        i.number,
        i.item_type.map_or("?", ItemType::glyph),
        clip(stage_plain(&shown.stage), 16),
        if shown.derived { "~" } else { " " },
        i.priority.as_deref().unwrap_or("-"),
        clip(i.milestone.as_deref().unwrap_or("-"), 10),
        i.name(),
    )
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn decision_json(d: &Decision) -> Value {
    json!({
        "action": d.action.as_str(),
        "reason": d.reason,
        "runnable": d.runnable(),
        "blocked": d.blocked.as_ref().map(|b| json!({
            "code": b.code, "message": b.message, "unblock": b.unblock })),
    })
}

fn item_json(i: &Item, s: &Snapshot) -> Value {
    let shown = stage::shown(i);
    let mut v = serde_json::to_value(i).unwrap_or(Value::Null);
    v["shown_stage"] = json!(shown.stage);
    v["stage_derived"] = json!(shown.derived);
    v["next"] = decision_json(&next::describe(i, s));
    v
}

fn cmd_next(a: &Args) -> anyhow::Result<()> {
    let e = env(a)?;
    let n = number(a)?;
    let s = snap(&e)?;
    let item = s.item(n).ok_or_else(|| {
        anyhow::anyhow!(
            "issue #{n} is not in the Project (closed long ago, or not in {})",
            s.hub
        )
    })?;
    let d = next::describe(item, &s);
    if a.flag("--json") {
        println!("{}", serde_json::to_string_pretty(&decision_json(&d))?);
        return Ok(());
    }
    println!("#{n} {}", item.title);
    for line in decision_lines(&d) {
        println!("  {line}");
    }
    Ok(())
}

/// A decision as lines: the step, why, what stops it and how to go on.
pub fn decision_lines(d: &Decision) -> Vec<String> {
    let mut out = Vec::new();
    match d.action {
        next::Action::None => out.push(format!("next: {}", d.reason)),
        action => {
            out.push(format!("next: {}", action.as_str()));
            out.push(format!("why:  {}", d.reason));
        }
    }
    if let Some(b) = &d.blocked {
        out.push(format!("blocked ({}): {}", b.code, b.message));
        out.push(format!("then: {}", b.unblock));
    }
    out
}

/// The card of an item, as lines.
pub fn card_lines(i: &Item, s: &Snapshot) -> Vec<String> {
    let mut out = Vec::new();
    let shown = stage::shown(i);
    let t = i.item_type;
    out.push(format!(
        "{} #{}  {}",
        t.map_or("(no type)", ItemType::as_str),
        i.number,
        i.name()
    ));
    out.push(format!(
        "stage     {}{}{}",
        shown.stage,
        if shown.derived {
            "  (derived: not on the board)"
        } else {
            ""
        },
        i.status
            .as_deref()
            .map(|st| format!("  · {st}"))
            .unwrap_or_default()
    ));
    if !i.open {
        out.push("state     closed".to_string());
    }
    if let Some(t) = t
        && !shown.derived
        && !stage::in_track(t, &shown.stage)
    {
        out.push(format!(
            "          ! {} is not a stage a {} goes through",
            stage_plain(&shown.stage),
            t.as_str()
        ));
    }
    if stage::kind(&shown.stage) != stage::Kind::Canonical {
        out.push(
            "          ! this column is not a stage of the flow; the item is never moved from it"
                .to_string(),
        );
    }
    out.push(format!(
        "priority  {}",
        i.priority.as_deref().unwrap_or("-")
    ));
    if let Some(m) = &i.milestone {
        out.push(format!("milestone {m}"));
    }
    for (k, v) in &i.fields {
        if k != super::contract::FIELD_TYPE && k != super::contract::FIELD_PRIORITY {
            out.push(format!("{:<9} {v}", k.to_lowercase()));
        }
    }
    if !i.assignees.is_empty() {
        out.push(format!("assignees {}", i.assignees.join(", ")));
    }
    if !i.repos.is_empty() {
        out.push(format!("repos     {}", i.repos.join(", ")));
    }
    // the chain of parents, nearest first
    let mut chain = Vec::new();
    let mut cur = i.parent;
    while let Some(p) = cur {
        if chain.len() >= 8 {
            break;
        }
        match s.item(p) {
            Some(parent) => {
                chain.push(format!(
                    "#{} {} {}",
                    parent.number,
                    parent.item_type.map_or("?", ItemType::as_str),
                    parent.name()
                ));
                cur = parent.parent;
            }
            None => {
                chain.push(format!("#{p}"));
                cur = None;
            }
        }
    }
    for (k, line) in chain.iter().enumerate() {
        out.push(format!(
            "{} {line}",
            if k == 0 { "parent   " } else { "         " }
        ));
    }
    for (k, d) in i.depends_on.iter().enumerate() {
        let state = match s.item(*d) {
            Some(dep) if !dep.open => "done".to_string(),
            Some(dep) => stage_plain(&stage::shown(dep).stage).to_string(),
            None => "closed or outside the Project".to_string(),
        };
        out.push(format!(
            "{} #{d}  {state}",
            if k == 0 { "depends  " } else { "         " }
        ));
    }
    let labels: Vec<&str> = i
        .labels
        .iter()
        .filter(|l| l.starts_with("spec-wave:"))
        .map(String::as_str)
        .collect();
    if !labels.is_empty() {
        out.push(format!("labels    {}", labels.join(", ")));
    }
    if !i.docs.is_empty() {
        out.push(String::new());
        out.push("documents".to_string());
        for (name, d) in &i.docs {
            let extra = match (d.state, d.pr, &d.branch) {
                (DocState::Pr, Some(pr), _) => format!("  pull request #{pr}"),
                (DocState::Branch, _, Some(b)) => format!("  branch {b}"),
                _ => String::new(),
            };
            out.push(format!(
                "  {:<14} {:<8} {}{extra}",
                name,
                d.state.as_str(),
                d.path
            ));
        }
    }
    if !i.pulls.is_empty() {
        out.push(String::new());
        out.push("pull requests".to_string());
        for p in &i.pulls {
            out.push(format!(
                "  {}#{}  {}{}{}  {} → {}",
                p.repo,
                p.number,
                p.state.to_lowercase(),
                if p.draft { " (draft)" } else { "" },
                if p.review.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", p.review.to_lowercase().replace('_', " "))
                },
                p.head,
                p.base
            ));
        }
    }
    let kids = s.children_of(i.number);
    if !kids.is_empty() {
        out.push(String::new());
        out.push(format!("children ({})", kids.len()));
        for k in kids {
            out.push(format!("  {}", row_line(k)));
        }
    }
    out.push(String::new());
    out.extend(decision_lines(&next::describe(i, s)));
    out
}

fn cmd_show(a: &Args) -> anyhow::Result<()> {
    let e = env(a)?;
    let n = number(a)?;
    let s = snap(&e)?;
    let item = s.item(n).ok_or_else(|| {
        anyhow::anyhow!(
            "issue #{n} is not in the Project (closed long ago, or not in {})",
            s.hub
        )
    })?;
    if a.flag("--json") {
        println!("{}", serde_json::to_string_pretty(&item_json(item, &s))?);
        return Ok(());
    }
    for line in card_lines(item, &s) {
        println!("{line}");
    }
    Ok(())
}

/// A Feature's order, as lines.
pub fn order_lines(o: &order::FeatureOrder) -> Vec<String> {
    let mut out = vec![format!(
        "Feature #{} {}{}",
        o.feature,
        o.title,
        o.milestone
            .as_deref()
            .map(|m| format!("  [{m}]"))
            .unwrap_or_default()
    )];
    for (k, r) in o.rows.iter().enumerate() {
        let deps = if r.depends_on.is_empty() {
            String::new()
        } else {
            format!(
                "  ← {}",
                r.depends_on
                    .iter()
                    .map(|d| format!("#{d}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        out.push(format!(
            "  {:>2}. #{:<5} {:<16} {}/{} tasks  {}{deps}",
            k + 1,
            r.number,
            clip(stage_plain(&r.stage), 16),
            r.tasks_done,
            r.tasks,
            r.title
        ));
    }
    if o.rows.is_empty() {
        out.push("  (no Stories)".to_string());
    }
    for x in &o.external {
        out.push(format!(
            "  blocked from outside: #{} waits on #{} ({})",
            x.story,
            x.on,
            match (&x.stage, x.open) {
                (_, Some(false)) => "done".to_string(),
                (Some(st), _) => stage_plain(st).to_string(),
                _ => "closed or outside the Project".to_string(),
            }
        ));
    }
    for w in &o.warnings {
        out.push(format!("  ! {w}"));
    }
    out
}

fn cmd_order(a: &Args) -> anyhow::Result<()> {
    let e = env(a)?;
    let s = snap(&e)?;
    let orders = match a.positional.first() {
        Some(_) => vec![order::feature_order(&s, number(a)?).map_err(|e| anyhow::anyhow!(e))?],
        None => order::map(&s, a.value("--milestone")),
    };
    if a.flag("--json") {
        println!("{}", serde_json::to_string_pretty(&orders)?);
        return Ok(());
    }
    if orders.is_empty() {
        println!("no open Feature has Stories");
    }
    for (k, o) in orders.iter().enumerate() {
        if k > 0 {
            println!();
        }
        for line in order_lines(o) {
            println!("{line}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::snapshot::tests as fixture;

    #[test]
    fn flags_values_and_numbers_are_parsed() {
        let raw: Vec<String> = ["#12", "--type", "story", "--stage=ready", "--json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let a = Args::parse(&raw);
        assert_eq!(number(&a).unwrap(), 12);
        assert_eq!(a.value("--type"), Some("story"));
        assert_eq!(a.value("--stage"), Some("ready"));
        assert!(a.flag("--json"));
        assert!(number(&Args::parse(&["x".to_string()])).is_err());
    }

    #[test]
    fn a_card_says_stage_documents_and_the_next_step() {
        let s = snapshot::build(&fixture::raw(), &fixture::cfg(), 0);
        let card = card_lines(s.item(1).unwrap(), &s).join("\n");
        assert!(card.contains("Feature #1  Cadastro de Pedidos"));
        assert!(card.contains("stage     ✅ Ready  · Todo"));
        assert!(card.contains("spec           base     docs/features/cadastro-de-pedidos/spec.md"));
        assert!(card.contains("decomposition  branch"));
        assert!(card.contains("children (2)"));
        assert!(card.contains("blocked (pr-pending)"));
        let bug = card_lines(s.item(4).unwrap(), &s).join("\n");
        assert!(bug.contains("(derived: not on the board)"));
        assert!(bug.contains("pull request #40"));
    }

    #[test]
    fn the_cache_is_used_while_it_is_young() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_store(&dir.path().join("t.db")).unwrap();
        let cfg = fixture::cfg();
        // a `gh` that cannot run: any read of GitHub fails
        let tools = Tools {
            gh: "/nonexistent/gh".into(),
        };
        assert!(
            obtain(&conn, &tools, &cfg, Fresh::Cached)
                .unwrap_err()
                .contains("never read")
        );
        let mut s = snapshot::build(&fixture::raw(), &cfg, crate::project::now_ns());
        store::save(&conn, &s).unwrap();
        assert_eq!(obtain(&conn, &tools, &cfg, Fresh::Auto).unwrap(), s);
        assert!(obtain(&conn, &tools, &cfg, Fresh::Refresh).is_err());
        // an old cache is not served by Auto, and still is by Cached
        s.synced_ns -= 3600 * 1_000_000_000;
        store::save(&conn, &s).unwrap();
        assert!(obtain(&conn, &tools, &cfg, Fresh::Auto).is_err());
        assert_eq!(obtain(&conn, &tools, &cfg, Fresh::Cached).unwrap(), s);
    }
}
