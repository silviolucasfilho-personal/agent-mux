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

  agent-mux project merge <n> [--yes] [--keep-branches] [--dry-run]
        merge the pull requests of a Feature's Stories in the order of their
        dependencies (or of one Story or Bug): without --yes, only the plan. A draft
        blocks it: marking a pull request ready is your review. Each merge is a
        merge commit, moves the item to QA, and the branches go at the end

  agent-mux project workspace map|prepare --issue <n> [--json]
        the folders of the code repositories an item changes (a Feature gathers its
        Stories'): `map` only says where each is; `prepare` clones the ones missing
        under the hub's .spec-wave/repos and writes .spec-wave/workspace-<n>.json
  agent-mux project dispatch <n> [--harness H]
        hand an item to the fleet: its card goes to WIP and a run starts when a
        slot is free and `project fleet run` is going
  agent-mux project cancel <n>
        take a dispatched item back to Backlog; a run in progress is stopped
  agent-mux project fleet status [--json]
        the slots, each dispatched item's card, and who holds which item (the
        claims on the hub's remote, this machine's and anyone else's)
  agent-mux project fleet run [--once] [--dispatch <n>] [--max-runs N] [--max-failures N] [--poll SECONDS]
        run the dispatched items, at most N at a time (3), each as `project run <n>`
        under a lease on the hub's remote; --once ends when nothing is left to run

  agent-mux project estimate <n> [--yes] [--discard] [--harness H] [--dry-run]
        a read-only session proposes the item's effort in story points, fitted to
        the board's `Story Points` options; --yes writes the proposal that is
        waiting (or runs and writes at once), --discard drops it
  agent-mux project refine <n> [--yes] [--discard] [--harness H] [--dry-run]
        a read-only session proposes a clearer description, keeping what is asked
        and every line tools read; --yes replaces the issue's description with the
        proposal that is waiting, unless the description changed since

  agent-mux project run <n> [--step spec|plan|bug|critique|validate|build|fix|fix-pr] [--harness H] [--critic H] [--dry-run] [--apply] [--ignore-dependencies] [--keep-drafts]
        run the item's next document step: a read-only session writes the document,
        agent-mux publishes it by pull request and comments on the issue; a plan and
        a bug document are then audited by a second session; a decomposition is
        drafted the same way, and --apply creates its Stories and Tasks once you
        reviewed and merged the draft
        on a Story that is Ready or a Bug that was triaged, it builds: a session
        writes and commits the code in a worktree of the code repository, a second
        one reviews the diff, and agent-mux pushes the branch, opens a draft pull
        request and moves the board to Code Review. Nothing is merged
        on a Feature whose Stories are Ready, it builds them as a stack: one Story
        at a time in dependency order, each forked from the pull request of the
        Story before it in the same repository, every pull request a draft. When
        the last Story is delivered the Feature's pull requests are marked ready
        together (--keep-drafts leaves them). A stack that stops resumes when the
        Feature is run again
        --step fix-pr on a Story or Bug with an open pull request: a session fixes
        what the open review comments ask for on the pull request's branch, a
        second one reviews the fixes, and agent-mux pushes them, answers each
        comment with its commit and leaves a summary

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
    "--issue",
    "--max-runs",
    "--max-failures",
    "--dispatch",
    "--poll",
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

/// After a development step: the item's summary trace to Langfuse, when
/// Langfuse is configured (spec §9.3). Best effort: a failure is a line.
fn langfuse_summary(hub: &str, item: u64) {
    let Ok(cfg) = crate::config::load() else {
        return;
    };
    let Some(settings) =
        crate::config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())
    else {
        return;
    };
    if let Some(line) =
        crate::tracing::langfuse::send_project_summary(&settings.db_path, &settings, hub, item)
    {
        println!("  {line}");
    }
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
        Some("merge") => cmd_merge(&a),
        Some("run") => cmd_run(&a).await,
        Some("workspace") => cmd_workspace(&a),
        Some("dispatch") => cmd_dispatch(&a),
        Some("cancel") => cmd_cancel(&a),
        Some("fleet") => cmd_fleet(&a),
        Some("estimate") => cmd_assist(&a, super::assist::Kind::Estimate).await,
        Some("refine") => cmd_assist(&a, super::assist::Kind::Refine).await,
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

/// `project merge <n> [--yes] [--keep-branches]`: the plan, and with
/// `--yes` the merges.
fn cmd_merge(a: &Args) -> anyhow::Result<()> {
    use super::merge;
    let mut e = env(a)?;
    let n = number(a)?;
    // a merge is planned on what GitHub says now
    e.fresh = Fresh::Refresh;
    let s = snap(&e)?;
    let item = s
        .item(n)
        .ok_or_else(|| anyhow::anyhow!("issue #{n} is not in the Project"))?;
    println!("#{n} {}", item.title);
    let sorted = merge::scope(&s, n).map_err(|x| anyhow::anyhow!(x))?;
    let cwd = steps::hub_root(&e.cfg).unwrap_or_else(|_| e.cfg.root().to_path_buf());
    let ops = ops::Ops {
        tools: &e.tools,
        cfg: &e.cfg,
        seen_board: s.board_id.clone(),
    };
    let (defaults, unread) = merge::default_branches(&ops, &cwd, &s, &sorted);
    let mut plan = merge::plan(&sorted, &s, &defaults);
    plan.warnings.extend(unread);
    if a.flag("--json") {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        for line in merge::plan_lines(&plan, &s.hub) {
            println!("  {line}");
        }
    }
    if !plan.blocks.is_empty() {
        anyhow::bail!(
            "blocked: half of a stack merged is the worst state; resolve the above and run again"
        );
    }
    if plan.queue.is_empty() {
        println!(
            "  {}",
            if plan.done.is_empty() {
                "no open pull request to merge"
            } else {
                "everything is merged already"
            }
        );
        return Ok(());
    }
    if a.flag("--dry-run") || !a.flag("--yes") {
        println!(
            "  nothing was merged: run again with --yes to merge in this order{}",
            if a.flag("--keep-branches") {
                ""
            } else {
                " and delete the branches at the end (--keep-branches keeps them)"
            }
        );
        return Ok(());
    }
    let (report, stopped) = match merge::execute(&ops, &cwd, &s, &plan, a.flag("--keep-branches")) {
        Ok(r) => (r, None),
        Err((r, why)) => (r, Some(why)),
    };
    for line in &report.lines {
        println!("  {line}");
    }
    if let Err(err) = refresh_touched(&e.conn, &e.tools, &e.cfg, &report.touched) {
        eprintln!("the cache was not refreshed ({err}); `agent-mux project sync` reads it again");
    }
    if let Some(why) = stopped {
        anyhow::bail!(why);
    }
    Ok(())
}

/// `project workspace map|prepare --issue <n> [--json]`.
fn cmd_workspace(a: &Args) -> anyhow::Result<()> {
    use super::workspace;
    let action = a.positional.first().map(String::as_str).unwrap_or("");
    if !matches!(action, "map" | "prepare") {
        anyhow::bail!("usage: agent-mux project workspace map|prepare --issue <n> [--json]");
    }
    let raw = a
        .value("--issue")
        .or_else(|| a.positional.get(1).map(String::as_str))
        .ok_or_else(|| anyhow::anyhow!("which issue? `--issue <n>`"))?;
    let n: u64 = raw
        .trim_start_matches('#')
        .parse()
        .map_err(|_| anyhow::anyhow!("\"{raw}\" is not an issue number"))?;
    let e = env(a)?;
    let s = snap(&e)?;
    let item = s
        .item(n)
        .ok_or_else(|| anyhow::anyhow!("issue #{n} is not in the Project"))?;
    let hub_root = steps::hub_root(&e.cfg).map_err(|x| anyhow::anyhow!(x))?;
    let mut layout = workspace::layout(&hub_root, &e.cfg, &workspace::slugs(item, &s));
    let mut cloned = Vec::new();
    if action == "prepare" {
        if !layout.unknown.is_empty() {
            anyhow::bail!(
                "#{n} names repositories the hub does not list: {}. Add them to `repositories[]` of its .spec-wave.json (spec-wave's CLI does it)",
                layout.unknown.join(", ")
            );
        }
        let (done, missing) = workspace::prepare(&mut layout, &workspace::git_clone)
            .map_err(|x| anyhow::anyhow!(x))?;
        cloned = done;
        let at = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let file = workspace::write_manifest(&layout, n, &at).map_err(|x| anyhow::anyhow!(x))?;
        if !a.flag("--json") {
            println!("wrote {}", file.display());
            if !missing.is_empty() {
                println!(
                    "! missing and not cloned (your own folders): {}",
                    missing.join(", ")
                );
            }
        }
    }
    if a.flag("--json") {
        let mut v = serde_json::to_value(&layout)?;
        v["issue"] = json!(n);
        v["cloned"] = json!(cloned);
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("#{n} {}", item.title);
    for line in workspace::lines(&layout, &cloned) {
        println!("  {line}");
    }
    Ok(())
}

/// The runtime folder of the fleet's runner: its pid file and the runs'
/// output.
fn fleet_runtime() -> PathBuf {
    crate::tracing::analysis::default_snapshot_dir()
}

fn fleet_name(hub: &str) -> String {
    let slug: String = hub
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("project-fleet-{slug}")
}

/// `project dispatch <n> [--harness H]`.
fn cmd_dispatch(a: &Args) -> anyhow::Result<()> {
    use super::fleet;
    let e = env(a)?;
    let n = number(a)?;
    let harness = match a.value("--harness") {
        Some(h) => Some(harness_of(Some(h), Harness::Claude).map_err(|x| anyhow::anyhow!(x))?),
        None => None,
    };
    let s = snap(&e)?;
    let d = fleet::dispatch(&e.conn, &s, n, harness.map(|h| h.as_str()), super::now_ns())
        .map_err(|x| anyhow::anyhow!(x))?;
    println!(
        "#{n}: dispatched to the fleet ({})",
        d.line(super::now_ns())
    );
    if crate::factory::intake::holder(&fleet_runtime(), &fleet_name(&s.hub)).is_none() {
        println!(
            "  no runner is going for {}: start one with `agent-mux project fleet run`",
            s.hub
        );
    }
    Ok(())
}

/// `project cancel <n>`.
fn cmd_cancel(a: &Args) -> anyhow::Result<()> {
    use super::fleet;
    let e = env(a)?;
    let n = number(a)?;
    let hub = e.cfg.hub_repo().slug();
    let running =
        fleet::cancel(&e.conn, &hub, n, super::now_ns()).map_err(|x| anyhow::anyhow!(x))?;
    if !running {
        println!("#{n}: back in Backlog");
        return Ok(());
    }
    if crate::factory::intake::holder(&fleet_runtime(), &fleet_name(&hub)).is_some() {
        println!("#{n}: its run is being stopped; the card returns to Backlog when it has");
    } else {
        // no runner is there to stop it: the row is all that is left
        fleet::remove(&e.conn, &hub, n).map_err(|x| anyhow::anyhow!(x))?;
        println!(
            "#{n}: back in Backlog (no runner was going; a claim it left on the remote expires on its own)"
        );
    }
    Ok(())
}

/// The repository that holds the claims of `cfg`'s hub.
fn lease_repo(cfg: &ProjectConfig) -> Result<super::lease::LeaseRepo, String> {
    let hub_root = steps::hub_root(cfg)?;
    let url = super::build::git(&hub_root, &["remote", "get-url", "origin"])?;
    super::lease::LeaseRepo::open(
        fleet_runtime()
            .join("project-leases")
            .join(fleet_name(&cfg.hub_repo().slug())),
        url.trim(),
        super::lease::DEFAULT_PREFIX,
    )
}

/// `project fleet status|run`.
fn cmd_fleet(a: &Args) -> anyhow::Result<()> {
    use super::fleet;
    let max_runs: usize = match a.value("--max-runs") {
        Some(v) => v
            .parse()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| anyhow::anyhow!("--max-runs takes a number above zero"))?,
        None => fleet::MAX_RUNS,
    };
    match a.positional.first().map(String::as_str) {
        Some("status") => {
            let e = env(a)?;
            let hub = e.cfg.hub_repo().slug();
            let s = store::load(&e.conn, &hub).ok().flatten();
            let ds = fleet::list(&e.conn, &hub).map_err(|x| anyhow::anyhow!(x))?;
            let now = super::now_ns();
            // who holds what on the remote: this machine's runs and anyone else's
            let claims = lease_repo(&e.cfg).and_then(|r| r.list());
            if a.flag("--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "hub": hub,
                        "max_runs": max_runs,
                        "runner": crate::factory::intake::holder(&fleet_runtime(), &fleet_name(&hub)),
                        "dispatches": ds,
                        "claims": claims.as_ref().ok(),
                        "claims_error": claims.as_ref().err(),
                    }))?
                );
                return Ok(());
            }
            println!("fleet of {hub}");
            println!(
                "  runner: {}",
                match crate::factory::intake::holder(&fleet_runtime(), &fleet_name(&hub)) {
                    Some(pid) => format!("going (pid {pid})"),
                    None => "not going: `agent-mux project fleet run` starts it".to_string(),
                }
            );
            for line in fleet::status_lines(&ds, s.as_ref(), max_runs, now) {
                println!("  {line}");
            }
            match claims {
                Ok(list) if list.is_empty() => println!("  claims on the remote: none"),
                Ok(list) => {
                    println!("  claims on the remote:");
                    for l in list {
                        let beat = l.heartbeat_ns();
                        println!(
                            "    #{} held by {} (generation {}){}",
                            l.issue,
                            l.owner,
                            l.generation,
                            if super::lease::is_expired(beat, now, super::lease::TTL_SECS) {
                                " · no heartbeat past the TTL: it may be taken"
                            } else {
                                ""
                            }
                        );
                    }
                }
                Err(err) => println!("  ! the claims were not read: {err}"),
            }
            Ok(())
        }
        Some("run") => {
            let max_failures: u32 =
                match a.value("--max-failures") {
                    Some(v) => v.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                        anyhow::anyhow!("--max-failures takes a number above zero")
                    })?,
                    None => fleet::MAX_FAILURES,
                };
            let poll: u64 = match a.value("--poll") {
                Some(v) => v
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| anyhow::anyhow!("--poll takes seconds above zero"))?,
                None => 5,
            };
            fleet_run(a, max_runs, max_failures, poll)
        }
        _ => anyhow::bail!("usage: agent-mux project fleet status|run"),
    }
}

/// One run the runner holds.
struct FleetRun {
    item: u64,
    child: std::process::Child,
    lease: super::lease::Lease,
    renewed: std::time::Instant,
    log: PathBuf,
}

/// The last line of a run's output that says why it ended.
fn last_word(log: &Path) -> String {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(300).collect())
        .unwrap_or_default()
}

/// The runner: starts what the dispatcher says, keeps each run's lease
/// beating, and records how each ended. One runner per hub on a machine.
fn fleet_run(a: &Args, max_runs: usize, max_failures: u32, poll: u64) -> anyhow::Result<()> {
    use super::{fleet, lease};
    use crate::factory::intake;
    let mut e = env(a)?;
    let hub = e.cfg.hub_repo().slug();
    let root = e.cfg.root().to_path_buf();
    let runtime = fleet_runtime();
    // a dispatch given with the command is queued first, runner or not
    if let Some(raw) = a.value("--dispatch") {
        let n: u64 = raw
            .trim_start_matches('#')
            .parse()
            .map_err(|_| anyhow::anyhow!("\"{raw}\" is not an issue number"))?;
        let s = snap(&e)?;
        let d = fleet::dispatch(&e.conn, &s, n, a.value("--harness"), super::now_ns())
            .map_err(|x| anyhow::anyhow!(x))?;
        println!(
            "#{n}: dispatched to the fleet ({})",
            d.line(super::now_ns())
        );
    }
    let Some(_held) = intake::hold(&runtime, &fleet_name(&hub)) else {
        println!("a runner is already going for {hub}: it picks up what is queued");
        return Ok(());
    };
    let leases = lease_repo(&e.cfg).map_err(|x| anyhow::anyhow!(x))?;
    let login = super::github::login(&e.tools, &root).map_err(|x| anyhow::anyhow!(x))?;
    let owner = lease::owner(login.trim());
    // `AGENT_MUX_PROJECT_EXE` names a stand-in for this binary, for tests
    let exe = match std::env::var_os("AGENT_MUX_PROJECT_EXE") {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe()?,
    };
    let logs = runtime.join("project-fleet");
    std::fs::create_dir_all(&logs)?;
    println!("fleet of {hub}: up to {max_runs} run(s) at once, as {owner}");
    let once = a.flag("--once");
    let mut runs: Vec<FleetRun> = Vec::new();
    loop {
        let now = super::now_ns();
        // 1. runs that ended, and runs the operator cancelled
        let mut i = 0;
        while i < runs.len() {
            let cancelled = fleet::get(&e.conn, &hub, runs[i].item)
                .ok()
                .flatten()
                .is_none_or(|d| d.state == fleet::State::Cancelled);
            if cancelled {
                let mut r = runs.remove(i);
                let _ = r.child.kill();
                let _ = r.child.wait();
                leases.release(r.item);
                let _ = fleet::remove(&e.conn, &hub, r.item);
                println!("#{}: cancelled; its run was stopped", r.item);
                continue;
            }
            match runs[i].child.try_wait() {
                Ok(Some(status)) => {
                    let r = runs.remove(i);
                    leases.release(r.item);
                    // what GitHub says now decides whether it delivered
                    let fresh = refresh_touched(&e.conn, &e.tools, &e.cfg, &[r.item])
                        .ok()
                        .flatten();
                    let pr = fresh
                        .as_ref()
                        .and_then(|s| s.item(r.item))
                        .and_then(|it| it.pulls.iter().find(|p| p.open()).map(|p| p.url.clone()));
                    if let Ok(Some(mut d)) = fleet::get(&e.conn, &hub, r.item) {
                        fleet::settle(
                            &mut d,
                            status.success(),
                            pr,
                            &last_word(&r.log),
                            max_failures,
                            now,
                        );
                        let _ = fleet::save(&e.conn, &d);
                        println!("#{}: {}", r.item, d.line(now));
                    }
                }
                Ok(None) => i += 1,
                Err(err) => {
                    eprintln!("#{}: its run could not be watched: {err}", runs[i].item);
                    i += 1;
                }
            }
        }
        // 2. the leases: a heartbeat, or fencing when one was lost
        let mut i = 0;
        while i < runs.len() {
            if runs[i].renewed.elapsed().as_secs() < lease::HEARTBEAT_SECS as u64 {
                i += 1;
                continue;
            }
            let mut held = runs[i].lease.clone();
            match leases.renew(&mut held, now) {
                Ok(()) => {
                    runs[i].lease = held;
                    runs[i].renewed = std::time::Instant::now();
                    if let Ok(Some(mut d)) = fleet::get(&e.conn, &hub, runs[i].item) {
                        d.heartbeat_ns = Some(now);
                        d.updated_ns = now;
                        let _ = fleet::save(&e.conn, &d);
                    }
                    i += 1;
                }
                Err(lease::RenewError::Lost(why)) => {
                    // fencing: the run stops at once and nothing is released or pushed
                    let mut r = runs.remove(i);
                    let _ = r.child.kill();
                    let _ = r.child.wait();
                    if let Ok(Some(mut d)) = fleet::get(&e.conn, &hub, r.item) {
                        fleet::block(
                            &mut d,
                            &format!("the lease was lost ({why}): the run was stopped"),
                            now,
                        );
                        let _ = fleet::save(&e.conn, &d);
                    }
                    println!(
                        "#{}: the lease was lost ({why}); its run was stopped",
                        r.item
                    );
                }
                Err(lease::RenewError::Transient(why)) => {
                    eprintln!(
                        "#{}: the lease was not renewed, trying again: {why}",
                        runs[i].item
                    );
                    // past the TTL with no renewal another machine may hold it: stop
                    let stale =
                        lease::is_expired(runs[i].lease.heartbeat_ns(), now, lease::TTL_SECS);
                    if stale {
                        let mut r = runs.remove(i);
                        let _ = r.child.kill();
                        let _ = r.child.wait();
                        if let Ok(Some(mut d)) = fleet::get(&e.conn, &hub, r.item) {
                            fleet::block(
                                &mut d,
                                "the lease could not be renewed past its TTL: the run was stopped",
                                now,
                            );
                            let _ = fleet::save(&e.conn, &d);
                        }
                    } else {
                        i += 1;
                    }
                }
            }
        }
        // 3. what the dispatcher says
        e.fresh = Fresh::Auto;
        let s = snap(&e)?;
        let ds = fleet::list(&e.conn, &hub).map_err(|x| anyhow::anyhow!(x))?;
        let mine: Vec<u64> = runs.iter().map(|r| r.item).collect();
        let cards = fleet::cards(&ds, &s, |_| false)
            .into_iter()
            .map(|mut c| {
                c.alive = mine.contains(&c.item);
                c
            })
            .collect::<Vec<_>>();
        let mut started = false;
        for action in fleet::next_actions(&cards, max_runs) {
            let now = super::now_ns();
            match action {
                fleet::Action::Forget(n) | fleet::Action::Cancelled(n) => {
                    let _ = fleet::remove(&e.conn, &hub, n);
                    println!("#{n}: back in Backlog");
                }
                fleet::Action::Review { item, url } => {
                    if let Ok(Some(mut d)) = fleet::get(&e.conn, &hub, item) {
                        fleet::settle(&mut d, true, Some(url), "", max_failures, now);
                        let _ = fleet::save(&e.conn, &d);
                        println!("#{item}: {}", d.line(now));
                    }
                }
                fleet::Action::Lost(n) => {
                    if let Ok(Some(mut d)) = fleet::get(&e.conn, &hub, n) {
                        fleet::settle(
                            &mut d,
                            false,
                            None,
                            "its runner stopped before the run ended",
                            max_failures,
                            now,
                        );
                        let _ = fleet::save(&e.conn, &d);
                        println!("#{n}: {}", d.line(now));
                    }
                }
                fleet::Action::Start(n) => {
                    let Ok(Some(mut d)) = fleet::get(&e.conn, &hub, n) else {
                        continue;
                    };
                    let held = match leases.try_acquire(n, &owner, lease::TTL_SECS, now) {
                        Ok(Some(l)) => l,
                        Ok(None) => {
                            let who = leases
                                .fetch(n)
                                .ok()
                                .flatten()
                                .map(|(_, l)| l.owner)
                                .unwrap_or_else(|| "another machine".into());
                            d.reason = Some(format!("held by {who}"));
                            d.updated_ns = now;
                            let _ = fleet::save(&e.conn, &d);
                            continue;
                        }
                        Err(why) => {
                            fleet::block(
                                &mut d,
                                &format!("the lease could not be taken: {why}"),
                                now,
                            );
                            let _ = fleet::save(&e.conn, &d);
                            println!("#{n}: {}", d.line(now));
                            continue;
                        }
                    };
                    let log = logs.join(format!("{}-{n}-{now}.log", fleet_name(&hub)));
                    let out = std::fs::File::create(&log)?;
                    let err = out.try_clone()?;
                    let mut cmd = std::process::Command::new(&exe);
                    cmd.args(["project", "run", &n.to_string(), "--dir"])
                        .arg(&root)
                        .stdin(std::process::Stdio::null())
                        .stdout(out)
                        .stderr(err);
                    if let Some(h) = &d.harness {
                        cmd.args(["--harness", h]);
                    }
                    match cmd.spawn() {
                        Ok(child) => {
                            d.state = fleet::State::Running;
                            d.pid = Some(child.id());
                            d.started_ns = Some(now);
                            d.ended_ns = None;
                            d.heartbeat_ns = Some(now);
                            d.owner = Some(held.owner.clone());
                            d.generation = Some(held.generation);
                            d.reason = None;
                            d.log = Some(log.to_string_lossy().into_owned());
                            d.updated_ns = now;
                            let _ = fleet::save(&e.conn, &d);
                            println!("#{n}: started (output in {})", log.display());
                            runs.push(FleetRun {
                                item: n,
                                child,
                                lease: held,
                                renewed: std::time::Instant::now(),
                                log,
                            });
                            started = true;
                        }
                        Err(why) => {
                            leases.release(n);
                            fleet::block(
                                &mut d,
                                &format!("the run could not be started: {why}"),
                                now,
                            );
                            let _ = fleet::save(&e.conn, &d);
                            println!("#{n}: {}", d.line(now));
                        }
                    }
                }
            }
        }
        if once && runs.is_empty() && !started {
            println!("nothing is left to run");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(if runs.is_empty() {
            poll
        } else {
            poll.min(2)
        }));
    }
}

/// `project estimate|refine <n> [--yes] [--discard] [--harness H] [--json]`:
/// a read-only session proposes; the proposal waits in a file and is
/// written only with `--yes`. With `--yes` and a proposal waiting, no
/// session runs: what the operator read is what is written.
async fn cmd_assist(a: &Args, kind: super::assist::Kind) -> anyhow::Result<()> {
    use super::assist::{self, Kind};
    let mut e = env(a)?;
    let n = number(a)?;
    let harness =
        harness_of(a.value("--harness"), Harness::Claude).map_err(|x| anyhow::anyhow!(x))?;
    let hub = e.cfg.hub_repo().slug();
    let hub_root = steps::hub_root(&e.cfg).map_err(|x| anyhow::anyhow!(x))?;
    let ctx = steps::Ctx {
        tools: &e.tools,
        cfg: &e.cfg,
        scratch: steps::scratch_dir(&crate::tracing::analysis::default_snapshot_dir()),
        hub_root: hub_root.clone(),
    };
    let what = kind.as_str();
    if a.flag("--discard") {
        println!(
            "#{n}: {}",
            if assist::discard(&ctx.scratch, &hub, n, kind) {
                format!("the proposed {what} was discarded")
            } else {
                format!("no proposed {what} was waiting")
            }
        );
        return Ok(());
    }
    let write = |p: &assist::Proposal, s: &Snapshot| -> anyhow::Result<()> {
        let report = ctx.apply_proposal(p, s).map_err(|x| anyhow::anyhow!(x))?;
        for line in &report.lines {
            println!("  {line}");
        }
        if let Err(err) = refresh_touched(&e.conn, &e.tools, &e.cfg, &report.touched) {
            eprintln!(
                "the cache was not refreshed ({err}); `agent-mux project sync` reads it again"
            );
        }
        Ok(())
    };
    // a proposal the operator already read: written as it is, no session
    if a.flag("--yes")
        && let Some(p) = assist::load(&ctx.scratch, &hub, n, kind)
    {
        let s = snap(&e)?;
        println!("#{n} {}", p.title);
        for line in assist::lines(&p, false) {
            println!("  {line}");
        }
        return write(&p, &s);
    }
    e.fresh = Fresh::Refresh;
    let s = snap(&e)?;
    let item = s
        .item(n)
        .ok_or_else(|| anyhow::anyhow!("issue #{n} is not in the Project"))?
        .clone();
    println!("#{n} {}", item.title);
    let detail =
        super::github::issue_detail(&e.tools, &e.cfg, n).map_err(|x| anyhow::anyhow!(x))?;
    let args = match kind {
        Kind::Estimate => ctx.estimate_args(&item, &detail, &s),
        Kind::Refine => ctx
            .refine_args(&item, &detail, &s)
            .map_err(|x| anyhow::anyhow!(x))?,
    };
    println!(
        "  next: {} with the flow {} on {} (a read-only session; nothing is written)",
        match kind {
            Kind::Estimate => "estimate its effort",
            Kind::Refine => "propose a clearer description",
        },
        kind.flow(),
        harness.as_str()
    );
    if a.flag("--dry-run") {
        return Ok(());
    }
    let tag = super::ProjectLaunch {
        hub: s.hub.clone(),
        item: n,
        item_type: item.item_type.map(|t| t.as_str().to_string()),
        feature: s
            .ancestor(n, ItemType::Feature)
            .map(|f| f.number)
            .filter(|f| *f != n),
        milestone: item.milestone.clone(),
        step: what.to_string(),
        attempt: 1,
        executor: "operator".to_string(),
        ..Default::default()
    };
    let result = run_flow(kind.flow(), &hub_root, harness, args, None, tag)
        .await
        .map_err(|x| anyhow::anyhow!(x))?;
    let now = super::now_ns();
    let p = match kind {
        Kind::Estimate => assist::estimate_of(
            &item,
            &s.hub,
            &result,
            &assist::scale(&e.cfg),
            harness.as_str(),
            now,
        ),
        Kind::Refine => assist::refined_of(&item, &s.hub, &detail, &result, harness.as_str(), now),
    }
    .map_err(|x| anyhow::anyhow!("#{n}: the proposal was not kept: {x}"))?;
    if a.flag("--json") {
        println!("{}", serde_json::to_string_pretty(&p)?);
    } else {
        for line in assist::lines(&p, true) {
            println!("  {line}");
        }
    }
    if a.flag("--yes") {
        return write(&p, &s);
    }
    assist::save(&ctx.scratch, &p).map_err(|x| anyhow::anyhow!(x))?;
    println!(
        "  nothing was written: `agent-mux project {what} {n} --yes` writes this proposal, `--discard` drops it"
    );
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
    launch: super::ProjectLaunch,
) -> Result<Value, String> {
    let overrides = model
        .map(|m| vec![("work".to_string(), "model".to_string(), m)])
        .unwrap_or_default();
    run_flow_in(flow, workspace, harness, args, overrides, None, launch)
        .await
        .map(|(result, _)| result)
}

/// Runs a built-in flow headless and answers its result and the run's
/// folder. `worktree_base` is the ref a `revise` step's worktree forks
/// from.
async fn run_flow_in(
    flow: &str,
    workspace: &Path,
    harness: Harness,
    args: Value,
    step_overrides: Vec<(String, String, String)>,
    worktree_base: Option<String>,
    launch: super::ProjectLaunch,
) -> Result<(Value, PathBuf), String> {
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
        worktree_base,
        step_overrides,
    };
    let mut h = crate::workflows::cli::headless().map_err(|e| e.to_string())?;
    // every session of the run records the work item it runs for
    h.app.project_launch = Some(launch);
    let run_id = h.app.start_workflow_run(req)?;
    let run_dir = crate::workflows::context::run_dir(&h.app.workflows_runtime_dir(), &run_id);
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
    Ok((recent.result, run_dir))
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
    let fix_pr = matches!(a.value("--step"), Some("fix-pr" | "fix_pr" | "fixpr"));
    let forced = match a.value("--step") {
        Some(_) if fix_pr => None,
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
    let hub = s.hub.clone();
    let feature = s
        .ancestor(n, ItemType::Feature)
        .map(|f| f.number)
        .filter(|f| *f != n);
    let tagged = item.clone();
    let launch = move |step: &str| super::ProjectLaunch {
        hub: hub.clone(),
        item: n,
        item_type: tagged.item_type.map(|t| t.as_str().to_string()),
        feature,
        milestone: tagged.milestone.clone(),
        step: step.to_string(),
        attempt: 1,
        executor: "operator".to_string(),
        ..Default::default()
    };
    // development is its own step: a Story, a triaged Bug, or `--step build`
    let develop = match forced {
        Some(f) => f == next::Action::Implement,
        None => super::build::decision(&item, &s, false).is_some(),
    };
    let mut planned = if fix_pr {
        ctx.fix_plan(&item, &detail, &s)
    } else if develop {
        ctx.build_plan(&item, &detail, &s, a.flag("--ignore-dependencies"))
    } else {
        ctx.plan(&item, &detail, forced)
    }
    .map_err(|x| anyhow::anyhow!(x))?;
    let mut failed: Option<String> = None;
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
            Planned::Build(b) => {
                if a.flag("--dry-run") {
                    print_build(&b, harness);
                    break;
                }
                let tag = launch(if b.kind == super::build::BuildKind::Bug {
                    "fix"
                } else {
                    "build"
                });
                let mut seen = s.clone();
                match build_all(&ctx, b, &detail, &mut seen, harness, a, tag, &mut touched).await {
                    Ok(()) => langfuse_summary(&s.hub, item.number),
                    // the board already says development: the cache must too
                    Err(e) => failed = Some(e),
                }
                break;
            }
            Planned::Stack(p) => {
                for line in super::stack::plan_lines(&p) {
                    println!("  {line}");
                }
                if a.flag("--dry-run") {
                    break;
                }
                // the snapshot follows the stack: each delivered Story is in
                // code review with its pull request, for the next one's base
                let mut seen = s.clone();
                let total = p.pending.len();
                for (i, st) in p.pending.iter().enumerate() {
                    println!("\n  [{}/{total}] Story #{} {}", i + 1, st.number, st.title);
                    let stopped = |why: String| {
                        format!(
                            "the stack stopped at Story #{} ({i} of {total} delivered): {why}\n  run #{n} again to resume from it",
                            st.number
                        )
                    };
                    let Some(story) = seen.item(st.number).cloned() else {
                        failed = Some(stopped("it is no longer in the Project".into()));
                        break;
                    };
                    let planned = super::github::issue_detail(&e.tools, &e.cfg, st.number)
                        .and_then(|d| {
                            ctx.build_plan(&story, &d, &seen, a.flag("--ignore-dependencies"))
                                .map(|p| (d, p))
                        });
                    let (detail, b) = match planned {
                        Ok((d, Planned::Build(b))) => (d, b),
                        Ok((_, Planned::Stopped(d))) => {
                            for line in decision_lines(&d) {
                                println!("  {line}");
                            }
                            failed = Some(stopped(d.reason.clone()));
                            break;
                        }
                        Ok(_) => {
                            failed = Some(stopped("it has no build step".into()));
                            break;
                        }
                        Err(x) => {
                            failed = Some(stopped(x));
                            break;
                        }
                    };
                    let tag = super::ProjectLaunch {
                        hub: s.hub.clone(),
                        item: st.number,
                        item_type: Some(ItemType::Story.as_str().to_string()),
                        feature: Some(n),
                        milestone: story.milestone.clone(),
                        step: "build".to_string(),
                        attempt: 1,
                        executor: "operator".to_string(),
                        ..Default::default()
                    };
                    match build_all(&ctx, b, &detail, &mut seen, harness, a, tag, &mut touched)
                        .await
                    {
                        Ok(()) => langfuse_summary(&s.hub, st.number),
                        Err(x) => {
                            failed = Some(stopped(x));
                            break;
                        }
                    }
                }
                if failed.is_none() {
                    println!(
                        "\n  the stack of Feature #{n} is delivered: {total} Stor{}",
                        if total == 1 { "y" } else { "ies" }
                    );
                    println!(
                        "  next: review the pull requests, then `agent-mux project merge {n}`"
                    );
                }
                langfuse_summary(&s.hub, n);
                break;
            }
            Planned::FixPr(f) => {
                let name = super::merge::pr_ref(&f.pull, &s.hub);
                println!(
                    "  next: fix what the review of pull request {name} asked for, with the flow {} on {}",
                    super::fix_pr::FLOW_FIX_PR,
                    harness.as_str()
                );
                println!(
                    "  where: {} ({}), a worktree forked from origin/{}",
                    f.target.dir.display(),
                    f.target.repo.slug(),
                    f.pull.head
                );
                for c in &f.comments {
                    let first: String = c
                        .body
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(90)
                        .collect();
                    println!(
                        "    {} @{}{}: {first}",
                        c.key,
                        c.author,
                        if c.place.is_empty() {
                            String::new()
                        } else {
                            format!(" {}", c.place)
                        }
                    );
                }
                for line in &f.notes {
                    println!("  {line}");
                }
                println!("  then: an independent review of the committed fixes");
                println!(
                    "  then: push to `{}`, answer each comment with its commit, leave a summary on the pull request",
                    f.pull.head
                );
                if a.flag("--dry-run") {
                    break;
                }
                crate::factory::deliver::fetch(&f.target.dir, &f.pull.head)
                    .map_err(|x| anyhow::anyhow!(x))?;
                let ran = run_flow_in(
                    super::fix_pr::FLOW_FIX_PR,
                    &f.target.dir,
                    harness,
                    f.args.clone(),
                    Vec::new(),
                    Some(format!("origin/{}", f.pull.head)),
                    super::ProjectLaunch {
                        code_repo: Some(f.target.repo.slug()),
                        ..launch("fix-pr")
                    },
                )
                .await;
                let done = ran.and_then(|(result, run_dir)| {
                    let wt = crate::app::workflows::load_step_worktrees(&run_dir).remove("build");
                    ctx.fix_finish(&f, &result, wt.as_ref())
                });
                match done {
                    Ok(r) => {
                        finish_report(&r, &mut touched);
                        langfuse_summary(&s.hub, item.number);
                    }
                    Err(e) => failed = Some(e),
                }
                break;
            }
            Planned::Apply => {
                // the draft is read and checked before anything is asked
                let (doc, path) = ctx.apply_plan(&item, &s).map_err(|x| anyhow::anyhow!(x))?;
                println!("  next: apply the reviewed draft `{path}`");
                for line in super::apply::preview(&doc) {
                    println!("  {line}");
                }
                if a.flag("--dry-run") || !a.flag("--apply") {
                    println!(
                        "  applying creates these issues and is your approval of the draft: run again with --apply"
                    );
                    break;
                }
                let r = ctx
                    .apply(&item, &detail, &s, doc, &path)
                    .map_err(|x| anyhow::anyhow!(x))?;
                finish_report(&r, &mut touched);
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
                let result = run_flow(
                    g.flow,
                    &hub_root,
                    harness,
                    g.args.clone(),
                    model,
                    launch(g.doc),
                )
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
                    super::ProjectLaunch {
                        attempt: job.attempt,
                        ..launch("critique")
                    },
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
    if let Some(e) = failed {
        anyhow::bail!("#{n}: {e}");
    }
    Ok(())
}

/// What a planned build will do, before it starts.
fn print_build(b: &super::build::Build, harness: Harness) {
    println!(
        "  next: {} #{} with the flow {} on {}",
        match b.kind {
            super::build::BuildKind::Bug => "fix Bug",
            super::build::BuildKind::Story => "build Story",
            super::build::BuildKind::Task => "build Task",
        },
        b.number,
        b.flow,
        harness.as_str()
    );
    println!(
        "  where: {} ({}), a worktree forked from origin/{}",
        b.target.dir.display(),
        b.target.repo.slug(),
        b.base
    );
    for t in &b.tasks {
        println!(
            "    Task #{} {}",
            t.number,
            super::contract::strip_tag(&t.title)
        );
    }
    for line in &b.notes {
        println!("  {line}");
    }
    println!(
        "  then: an independent review of the committed diff, on another harness when one is installed"
    );
    println!(
        "  then: push `{}`, open a draft pull request, move {}",
        b.branch,
        if b.kind == super::build::BuildKind::Task {
            "the Task to Done"
        } else {
            "the board to Code Review"
        }
    );
}

/// Runs a planned build to its end, one repository after another when the
/// Story changes several. `seen` follows what was delivered: the pull
/// request of each run, and the item in code review after the last, so
/// the next plan (the next repository, the next Story of a stack) reads
/// it.
#[allow(clippy::too_many_arguments)]
async fn build_all(
    ctx: &steps::Ctx<'_>,
    mut b: Box<super::build::Build>,
    detail: &super::github::Detail,
    seen: &mut Snapshot,
    harness: Harness,
    a: &Args,
    tag: super::ProjectLaunch,
    touched: &mut Vec<u64>,
) -> Result<(), String> {
    loop {
        b.keep_drafts = a.flag("--keep-drafts");
        print_build(&b, harness);
        let t = super::ProjectLaunch {
            code_repo: Some(b.target.repo.slug()),
            ..tag.clone()
        };
        let url = run_build(ctx, &b, seen, harness, t, touched).await?;
        let more = b.part.as_ref().is_some_and(|p| !p.last());
        if let Some(it) = seen.items.iter_mut().find(|x| x.number == b.number) {
            if !more && b.kind != super::build::BuildKind::Task {
                it.stage = Some(super::contract::STAGE_CODE_REVIEW.to_string());
            }
            if let Some(url) = url {
                it.pulls.push(super::Pull {
                    repo: b.target.repo.slug(),
                    number: url
                        .rsplit('/')
                        .next()
                        .and_then(|x| x.parse().ok())
                        .unwrap_or(0),
                    url,
                    state: "OPEN".into(),
                    draft: true,
                    head: b.branch.clone(),
                    base: b.base.clone(),
                    review: String::new(),
                });
            }
        }
        if !more {
            return Ok(());
        }
        let item = seen
            .item(b.number)
            .cloned()
            .ok_or_else(|| format!("#{} is no longer in the Project", b.number))?;
        b = match ctx.build_plan(&item, detail, seen, a.flag("--ignore-dependencies"))? {
            Planned::Build(next) => next,
            Planned::Stopped(d) => return Err(d.reason),
            _ => return Err(format!("#{} has no build step left", b.number)),
        };
        println!();
    }
}

/// Runs a planned build: the base fetched and the stack checked (what
/// can fail cheaply fails before the board says development), the board,
/// the flow, the delivery. Answers the pull request's URL.
async fn run_build(
    ctx: &steps::Ctx<'_>,
    b: &super::build::Build,
    s: &Snapshot,
    harness: Harness,
    tag: super::ProjectLaunch,
    touched: &mut Vec<u64>,
) -> Result<Option<String>, String> {
    crate::factory::deliver::fetch(&b.target.dir, &b.base)?;
    super::build::check_stack(b)?;
    let started = ctx.build_start(b, s);
    finish_report(&started, touched);
    let (result, run_dir) = run_flow_in(
        b.flow,
        &b.target.dir,
        harness,
        b.args.clone(),
        Vec::new(),
        Some(format!("origin/{}", b.base)),
        tag,
    )
    .await?;
    let wt = crate::app::workflows::load_step_worktrees(&run_dir).remove("build");
    let r = ctx.build_finish(b, &result, wt.as_ref(), s)?;
    finish_report(&r, touched);
    Ok(r.lines
        .iter()
        .find_map(|l| l.strip_prefix(super::build::OPENED_PR))
        .map(|u| u.trim().to_string()))
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

pub(crate) fn decision_json(d: &Decision) -> Value {
    json!({
        "action": d.action.as_str(),
        "reason": d.reason,
        "runnable": d.runnable(),
        "blocked": d.blocked.as_ref().map(|b| json!({
            "code": b.code, "message": b.message, "unblock": b.unblock })),
    })
}

pub(crate) fn item_json(i: &Item, s: &Snapshot) -> Value {
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
