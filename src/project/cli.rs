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
  agent-mux project dispatch <n> [--harness H] [--to <login>]
        hand an item to the fleet: its card goes to WIP and a run starts when a
        slot is free and `project fleet run` is going. With --to, it is handed to
        that developer by a comment on the issue, and their fleet runs it
  agent-mux project cancel <n>
        take a dispatched item back to Backlog; a run in progress is stopped
  agent-mux project fleet status [--json]
        the slots, each dispatched item's card, and who holds which item (the
        claims on the hub's remote, this machine's and anyone else's)
  agent-mux project fleet run [--once] [--dispatch <n>] [--max-runs N] [--max-failures N] [--poll SECONDS] [--label] [--delivery pr-stack|agent]
        run the dispatched items, at most N at a time (3), each as `project run <n>`
        under a lease on the hub's remote; --once ends when nothing is left to run

  agent-mux project qa <n> [--only a,b] [--severity P] [--harness H] [--dry-run]
        run the QA scenarios of a Story, of a Feature's pending Stories, or a Bug's
        regression test, on an item that is in QA and whose Feature's plan is
        qa-ready. A session executes them in a temporary worktree at the default
        branch and fixes nothing. Green: qa-approved and the item moves on. Red: one
        Bug per failed scenario under the Story, and the item stays in QA. Blocked
        with no failure: nothing moves

  agent-mux project qa-trail plan <milestone> [--harness H] [--critic H] [--dry-run]
        the QA of a milestone, by its number or title: write and audit the QA plan
        of each Feature that has none (the qa-plan step), and say which are ready
  agent-mux project qa-trail run <milestone> [--only f1,f2] [--max-cycles N] [--parallel N] [--harness H] [--dry-run]
        one cycle: `project qa` on every Feature of the milestone that is in QA,
        each a process of its own, only when all of them are qa-ready. Writes the
        cycle's report (a pull request under docs/qa/) and the block in the
        milestone's description. A later cycle tests only what may have changed
        (a Bug closed, a blocked scenario) and refuses to start when nothing did
  agent-mux project qa-trail report <milestone> [--cycle N] [--json]
        print a cycle's report, the last one by default

  agent-mux project preflight --milestone <M> [--harness H] [--json]
        before generating a milestone's specs: what could make every generation
        fail (gh, the execution mode, the push, the harness) and where each
        Feature's spec.md is: to generate, ready, waiting for its merge (never
        generated again) or not read. Read-only; exits 1 on a block
  agent-mux project audit --milestone <M> [--critique] [--critic H] [--json] [--dry-run]
        the milestone's specs as a set, before the plans: a dependency cycle, a
        blocking Feature due in a later milestone, a reference to an issue that
        does not exist (these exit 1), and what is only worth a look. --critique
        adds one read-only session that reads every spec against the others.
        Nothing is written to GitHub

  agent-mux project insights [<n>] [--scope progress|tech|daily|brainstorm] [--topic \"…\"] [--harness H] [--dry-run]
        a read-only session reads where the Project stands (or one item and what is
        under it) and says what matters; the answer is shown and kept as a file.
        Nothing is written to GitHub

  agent-mux project estimate <n> [--yes] [--discard] [--harness H] [--dry-run]
        a read-only session proposes the item's effort in story points, fitted to
        the board's `Story Points` options; --yes writes the proposal that is
        waiting (or runs and writes at once), --discard drops it
  agent-mux project refine <n> [--yes] [--discard] [--harness H] [--dry-run]
        a read-only session proposes a clearer description, keeping what is asked
        and every line tools read; --yes replaces the issue's description with the
        proposal that is waiting, unless the description changed since

  agent-mux project run <n> [--step spec|plan|bug|critique|validate|build|fix|fix-pr|qa-plan] [--harness H] [--critic H] [--dry-run] [--apply] [--ignore-dependencies] [--keep-drafts] [--delivery pr-stack|agent]
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
        --step qa-plan on a Feature: a session writes qa-plan.md (scenarios per
        Story) from the merged spec and plan, agent-mux validates it and publishes it
        by pull request, and a second session audits it; a clean audit adds
        spec-wave:qa-ready. A plan that is already there is audited as it is
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
    "--to",
    "--delivery",
    "--scope",
    "--topic",
    "--only",
    "--max-cycles",
    "--cycle",
    "--parallel",
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
        Some("insights") => cmd_insights(&a).await,
        Some("qa") => cmd_qa(&a).await,
        Some("qa-trail") => cmd_qa_trail(&a).await,
        Some("preflight") => cmd_preflight(&a),
        Some("audit") => cmd_audit(&a).await,
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
    if let Some(to) = a
        .value("--to")
        .map(|t| t.trim_start_matches('@').to_string())
    {
        // handed to a named developer: the request is a comment on the
        // issue, which their agent-mux reads; nothing runs here
        if to.is_empty() || !to.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            fleet::remove(&e.conn, &s.hub, n).map_err(|x| anyhow::anyhow!(x))?;
            anyhow::bail!("\"{to}\" is not a login");
        }
        let mut d = d;
        d.target = to.clone();
        let asked = super::write::comment(
            &e.tools,
            &e.cfg,
            n,
            &fleet::request_comment(&to, d.harness.as_deref(), false),
        );
        if let Err(why) = asked {
            fleet::remove(&e.conn, &s.hub, n).map_err(|x| anyhow::anyhow!(x))?;
            anyhow::bail!(
                "#{n} was not dispatched: the request was not written on the issue: {why}"
            );
        }
        fleet::save(&e.conn, &d).map_err(|x| anyhow::anyhow!(x))?;
        println!(
            "#{n}: dispatched to @{to} by a comment on the issue ({})",
            d.line(super::now_ns())
        );
        println!(
            "  it starts when their `agent-mux project fleet run` next looks; `project fleet status` shows the claim once it has"
        );
        return Ok(());
    }
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
    // a request to a named developer is taken back where it was made
    if let Some(d) = fleet::get(&e.conn, &hub, n)
        .map_err(|x| anyhow::anyhow!(x))?
        .filter(|d| !d.for_fleet())
    {
        super::write::comment(
            &e.tools,
            &e.cfg,
            n,
            &fleet::request_comment(&d.target, None, true),
        )
        .map_err(|x| anyhow::anyhow!("#{n}: the request was not taken back on the issue: {x}"))?;
        fleet::remove(&e.conn, &hub, n).map_err(|x| anyhow::anyhow!(x))?;
        println!(
            "#{n}: the request to @{} was taken back; the card is in Backlog",
            d.target
        );
        return Ok(());
    }
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
    // `[project.fleet]` of the settings file; a flag wins over it
    let mut settings = crate::config::load()
        .map_err(|e| e.to_string())
        .and_then(|c| crate::config::resolve_fleet(c.project.as_ref()))
        .map_err(|x| anyhow::anyhow!(x))?;
    if let Some(v) = a.value("--max-runs") {
        settings.max_runs = v
            .parse()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| anyhow::anyhow!("--max-runs takes a number above zero"))?;
    }
    match a.value("--delivery") {
        None => {}
        Some("pr-stack") => settings.agent_delivery = false,
        Some("agent") => settings.agent_delivery = true,
        Some(other) => anyhow::bail!("--delivery {other:?}: use pr-stack or agent"),
    }
    if a.flag("--label") {
        settings.label = true;
    }
    let max_runs = settings.max_runs;
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
            if let Some(v) = a.value("--max-failures") {
                settings.max_failures =
                    v.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                        anyhow::anyhow!("--max-failures takes a number above zero")
                    })?;
            }
            let poll: u64 = match a.value("--poll") {
                Some(v) => v
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| anyhow::anyhow!("--poll takes seconds above zero"))?,
                None => 5,
            };
            fleet_run(a, &settings, poll)
        }
        _ => anyhow::bail!("usage: agent-mux project fleet status|run"),
    }
}

/// Stops a run and everything in its process group: the group is asked to
/// end, then the run's own process is killed and reaped.
fn stop_run(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", "--", &format!("-{}", child.id())])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = std::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// The `request` source: dispatches addressed to `me` in comments on the
/// hub's issues since the last look, by someone who may triage the hub.
/// Best effort: a read that fails is a line, and the next look asks again
/// from the same point.
fn take_requests(e: &Env, s: &Snapshot, hub: &str, root: &Path, me: &str, cursor: &Path) {
    use super::fleet;
    use crate::factory::intake;
    let rfc = |ns: i64| {
        time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ns))
            .ok()
            .and_then(|t| {
                t.replace_nanosecond(0)
                    .ok()?
                    .format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .unwrap_or_default()
    };
    // `{since, seen}`: where the last look ended and the comments it took
    let saved: Value = std::fs::read_to_string(cursor)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let started = super::now_ns();
    let since = saved
        .get("since")
        .and_then(Value::as_str)
        .map(str::to_string)
        // a first look reads the last day, not the repository's history
        .unwrap_or_else(|| rfc(started - 86_400 * 1_000_000_000));
    let mut seen: Vec<u64> = saved
        .get("seen")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    let comments = match super::github::comments_since(&e.tools, root, hub, &since) {
        Ok(c) => c,
        Err(why) => {
            eprintln!("requests were not read: {why}");
            return;
        }
    };
    let mut taken: Vec<u64> = Vec::new();
    for c in &comments {
        let (Some(id), Some(n)) = (
            c.get("id").and_then(Value::as_u64),
            c.get("issue").and_then(Value::as_u64),
        ) else {
            continue;
        };
        taken.push(id);
        if seen.contains(&id) {
            continue;
        }
        let Some(req) = fleet::parse_request(c.get("body").and_then(Value::as_str).unwrap_or(""))
        else {
            continue;
        };
        if !req.to.eq_ignore_ascii_case(me) {
            continue;
        }
        let author = c.get("author").and_then(Value::as_str).unwrap_or("");
        let may = !author.is_empty()
            && intake::permission(&e.tools, root, hub, author)
                .is_ok_and(|role| intake::may_request(&role));
        if !may {
            println!("#{n}: a dispatch to you by {author}, who may not triage {hub}: not taken");
            continue;
        }
        let now = super::now_ns();
        if req.cancel {
            if fleet::cancel(&e.conn, hub, n, now).is_ok() {
                println!("#{n}: {author} took the dispatch back");
            }
            continue;
        }
        match fleet::dispatch(&e.conn, s, n, req.harness.as_deref(), now) {
            Ok(_) => println!("#{n}: dispatched to you by {author}"),
            Err(why) => println!("#{n}: a dispatch by {author} was not taken: {why}"),
        }
    }
    seen = taken;
    let _ = std::fs::write(
        cursor,
        json!({ "since": rfc(started), "seen": seen }).to_string(),
    );
}

/// One run the runner holds.
struct FleetRun {
    item: u64,
    child: std::process::Child,
    lease: super::lease::Lease,
    renewed: std::time::Instant,
    log: PathBuf,
}

/// The last pull request a run's output says it opened.
fn opened_pr(log: &Path) -> Option<String> {
    std::fs::read_to_string(log)
        .ok()?
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix(super::build::OPENED_PR))
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
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
fn fleet_run(a: &Args, settings: &crate::config::FleetSettings, poll: u64) -> anyhow::Result<()> {
    use super::{fleet, lease};
    use crate::factory::intake;
    let (max_runs, max_failures) = (settings.max_runs, settings.max_failures);
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
    if settings.label {
        println!("  taking issues labelled `{}`", settings.queue_label);
    }
    let me = login.trim().to_string();
    let cursor = logs.join(format!("{}.requests", fleet_name(&hub)));
    // issues whose label was added by someone who may not hand out work
    let mut refused: Vec<u64> = Vec::new();
    let mut looked: Option<std::time::Instant> = None;
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
                stop_run(&mut r.child);
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
                        .and_then(|it| it.pulls.iter().find(|p| p.open()).map(|p| p.url.clone()))
                        // a Feature's one pull request closes nothing, so
                        // GitHub links it to no issue: the run says it
                        .or_else(|| opened_pr(&r.log));
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
                    stop_run(&mut r.child);
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
                        stop_run(&mut r.child);
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
        // 3. where work comes from besides the screen: at most once a minute,
        // since each asks GitHub
        e.fresh = Fresh::Auto;
        let s = snap(&e)?;
        if (settings.label || settings.request)
            && looked.is_none_or(|t| t.elapsed().as_secs() >= 60)
        {
            looked = Some(std::time::Instant::now());
            if settings.label {
                let ds = fleet::list(&e.conn, &hub).map_err(|x| anyhow::anyhow!(x))?;
                for n in fleet::labelled(&s, &ds, &settings.queue_label) {
                    if refused.contains(&n) {
                        continue;
                    }
                    let who = intake::labeler(
                        &e.tools,
                        &root,
                        &hub,
                        &n.to_string(),
                        &settings.queue_label,
                    )
                    .ok()
                    .flatten();
                    let may = who.as_deref().is_some_and(|login| {
                        intake::permission(&e.tools, &root, &hub, login)
                            .is_ok_and(|role| intake::may_request(&role))
                    });
                    if !may {
                        refused.push(n);
                        println!(
                            "#{n}: labelled `{}` by {}, who may not triage {hub}: not dispatched",
                            settings.queue_label,
                            who.as_deref().unwrap_or("nobody its events name")
                        );
                        continue;
                    }
                    if fleet::dispatch(&e.conn, &s, n, None, super::now_ns()).is_ok() {
                        println!(
                            "#{n}: dispatched by the label `{}` ({} added it)",
                            settings.queue_label,
                            who.as_deref().unwrap_or("?")
                        );
                    }
                }
            }
            if settings.request {
                take_requests(&e, &s, &hub, &root, &me, &cursor);
            }
        }
        // 4. what the dispatcher says
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
                    // a group of its own, so stopping the run stops what it started
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::CommandExt;
                        cmd.process_group(0);
                    }
                    let feature = s
                        .item(n)
                        .is_some_and(|i| i.item_type == Some(ItemType::Feature));
                    if settings.agent_delivery && feature {
                        cmd.args(["--delivery", "agent"]);
                    }
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

/// `project qa <n> [--only a,b] [--severity P] [--harness H] [--dry-run]`.
async fn cmd_qa(a: &Args) -> anyhow::Result<()> {
    use super::qa::{self, QaPlanned, Verdict};
    let mut e = env(a)?;
    let n = number(a)?;
    let harness =
        harness_of(a.value("--harness"), Harness::Claude).map_err(|x| anyhow::anyhow!(x))?;
    let only: Option<Vec<u32>> = match a.value("--only") {
        Some(raw) => Some(
            raw.split(',')
                .map(|x| x.trim().parse::<u32>().ok().filter(|v| *v > 0))
                .collect::<Option<Vec<u32>>>()
                .ok_or_else(|| {
                    anyhow::anyhow!("--only takes scenario numbers, as 2 or 1,3: got \"{raw}\"")
                })?,
        ),
        None => None,
    };
    let (_, _, default_severity) = qa::settings(&e.cfg);
    let severity = a
        .value("--severity")
        .unwrap_or(&default_severity)
        .to_string();
    if !super::contract::PRIORITIES.contains(&severity.as_str()) {
        anyhow::bail!(
            "\"{severity}\" is not a severity; one of: {}",
            super::contract::PRIORITIES.join(", ")
        );
    }
    // a verdict moves the board: it is decided on what GitHub says now
    e.fresh = Fresh::Refresh;
    let s = snap(&e)?;
    let item = s
        .item(n)
        .ok_or_else(|| anyhow::anyhow!("issue #{n} is not in the Project"))?
        .clone();
    println!("#{n} {}", item.title);
    let detail =
        super::github::issue_detail(&e.tools, &e.cfg, n).map_err(|x| anyhow::anyhow!(x))?;
    let hub_root = steps::hub_root(&e.cfg).map_err(|x| anyhow::anyhow!(x))?;
    let ctx = steps::Ctx {
        tools: &e.tools,
        cfg: &e.cfg,
        scratch: steps::scratch_dir(&crate::tracing::analysis::default_snapshot_dir()),
        hub_root: hub_root.clone(),
    };
    let job = match ctx
        .qa_plan(&item, &detail, &s, only.as_deref())
        .map_err(|x| anyhow::anyhow!("#{n}: {x}"))?
    {
        QaPlanned::Nothing(why) => {
            println!("  {why}");
            return Ok(());
        }
        QaPlanned::Job(j) => j,
    };
    // where the code is: the repository of the item, or of the first Story in scope
    let owner = match item.item_type {
        Some(ItemType::Feature) => job
            .targets
            .first()
            .and_then(|sc| s.item(sc.story))
            .unwrap_or(&item),
        _ => &item,
    };
    let repos = super::build::targets(&hub_root, &e.cfg, owner).map_err(|x| anyhow::anyhow!(x))?;
    let target = repos
        .first()
        .ok_or_else(|| anyhow::anyhow!("#{n} names no repository to test"))?;
    println!(
        "  next: run {} scenario(s) with the flow {} on {} (run {} of this item)",
        job.targets.len(),
        qa::FLOW_QA,
        harness.as_str(),
        job.run
    );
    for sc in &job.targets {
        println!(
            "    {} · Story #{}{}",
            sc.anchor,
            sc.story,
            if sc.criterio.is_empty() {
                String::new()
            } else {
                format!(": {}", sc.criterio)
            }
        );
    }
    println!(
        "  where: a temporary worktree of {} ({}) at its default branch; the session fixes nothing",
        target.dir.display(),
        target.repo.slug()
    );
    if repos.len() > 1 {
        println!(
            "  ! the item changes {} repositories: the scenarios run in the first, {}",
            repos.len(),
            target.repo.slug()
        );
    }
    println!(
        "  then: the report on the issue; green moves the item on, red opens a Bug ({severity}) per failed scenario"
    );
    if a.flag("--dry-run") {
        return Ok(());
    }
    super::build::check_checkout(target).map_err(|x| anyhow::anyhow!(x))?;
    let base =
        crate::factory::deliver::default_branch(&target.dir).map_err(|x| anyhow::anyhow!(x))?;
    crate::factory::deliver::fetch(&target.dir, &base).map_err(|x| anyhow::anyhow!(x))?;
    let wt = ctx.scratch.join(format!("qa-{n}-{}", super::now_ns()));
    std::fs::create_dir_all(&ctx.scratch)?;
    super::build::git(
        &target.dir,
        &[
            "worktree",
            "add",
            "--detach",
            &wt.to_string_lossy(),
            &format!("origin/{base}"),
        ],
    )
    .map_err(|x| anyhow::anyhow!("the QA worktree was not created: {x}"))?;
    let head = super::build::git(&wt, &["rev-parse", "--short=7", "HEAD"]).unwrap_or_default();
    let tag = super::ProjectLaunch {
        hub: s.hub.clone(),
        item: n,
        item_type: item.item_type.map(|t| t.as_str().to_string()),
        feature: job.feature.filter(|f| *f != n),
        milestone: item.milestone.clone(),
        step: "qa".to_string(),
        attempt: job.run,
        role: "qa".to_string(),
        executor: "operator".to_string(),
        code_repo: Some(target.repo.slug()),
        ..Default::default()
    };
    let ran = run_flow(qa::FLOW_QA, &wt, harness, job.args.clone(), None, tag).await;
    // the worktree goes whatever happened: what the session changed dies with it
    let _ = super::build::git(
        &target.dir,
        &["worktree", "remove", "--force", &wt.to_string_lossy()],
    );
    let result = ran.map_err(|x| {
        anyhow::anyhow!(
            "#{n}: the QA session did not finish ({x}): nothing was moved and no Bug was created"
        )
    })?;
    let (report, verdict) = ctx
        .qa_finish(&job, &result, &s, &severity, head.trim())
        .map_err(|x| anyhow::anyhow!("#{n}: {x}"))?;
    for line in &report.lines {
        println!("  {line}");
    }
    if let Err(err) = refresh_touched(&e.conn, &e.tools, &e.cfg, &report.touched) {
        eprintln!("the cache was not refreshed ({err}); `agent-mux project sync` reads it again");
    }
    langfuse_summary(&s.hub, n);
    println!("  verdict: {}", verdict.as_str());
    if verdict != Verdict::Pass {
        anyhow::bail!("#{n}: QA did not pass ({})", verdict.as_str());
    }
    Ok(())
}
/// `agent-mux project qa-trail plan|run|report <milestone>`: the QA of a
/// milestone (spec §34), the port of spec-wave's `qa-lead`. `plan` writes
/// the QA plans that are missing; `run` is one cycle of `project qa` over
/// the Features in QA; `report` prints a cycle's report.
async fn cmd_qa_trail(a: &Args) -> anyhow::Result<()> {
    use super::trail;
    use std::collections::BTreeMap;
    let action = a.positional.first().map(String::as_str).unwrap_or("");
    if !matches!(action, "plan" | "run" | "report") {
        anyhow::bail!(
            "usage: agent-mux project qa-trail plan|run|report <milestone> (its number, or its title)"
        );
    }
    let reference = a.positional.get(1..).unwrap_or_default().join(" ");
    if reference.is_empty() {
        anyhow::bail!(
            "which milestone? give its number or its title: `agent-mux project qa-trail {action} <milestone>`"
        );
    }
    let mut e = env(a)?;
    if action != "report" && !a.flag("--cached") {
        // a cycle is decided on what GitHub says now
        e.fresh = Fresh::Refresh;
    }
    let s = snap(&e)?;
    let milestone = trail::resolve_milestone(&s.milestones, &reference)
        .map_err(|x| anyhow::anyhow!(x))?
        .clone();
    let hub_root = steps::hub_root(&e.cfg).map_err(|x| anyhow::anyhow!(x))?;
    let scratch = steps::scratch_dir(&crate::tracing::analysis::default_snapshot_dir());

    if action == "report" {
        let all = trail::cycles(&scratch, &s.hub, &hub_root, &milestone.title);
        let wanted =
            match a.value("--cycle") {
                Some(raw) => Some(raw.parse::<u32>().ok().filter(|c| *c > 0).ok_or_else(|| {
                    anyhow::anyhow!("--cycle takes a cycle number: got \"{raw}\"")
                })?),
                None => None,
            };
        let report = match wanted {
            Some(c) => all.get(&c).ok_or_else(|| {
                anyhow::anyhow!("cycle {c} of milestone \"{}\" was not found", milestone.title)
            })?,
            None => all.values().next_back().ok_or_else(|| {
                anyhow::anyhow!(
                    "no cycle has run for milestone \"{}\": start one with `agent-mux project qa-trail run`",
                    milestone.title
                )
            })?,
        };
        if a.flag("--json") {
            print!("{}", trail::render_json(report));
        } else {
            println!("{}", trail::render_md(report));
        }
        return Ok(());
    }

    // the trail: the milestone's Features, in the order of their Stories
    let features: Vec<&Item> = s
        .items
        .iter()
        .filter(|i| {
            i.item_type == Some(ItemType::Feature)
                && i.milestone.as_deref() == Some(milestone.title.as_str())
        })
        .collect();
    if features.is_empty() {
        println!(
            "milestone \"{}\" has no Feature: nothing to do",
            milestone.title
        );
        return Ok(());
    }
    let mut numbers: Vec<u64> = features.iter().map(|f| f.number).collect();
    numbers.sort_unstable();
    let stories: Vec<(u64, u64, Vec<u64>)> = numbers
        .iter()
        .flat_map(|f| {
            s.children_of(*f)
                .into_iter()
                .filter(|c| c.item_type == Some(ItemType::Story))
                .map(|c| (c.number, *f, c.depends_on.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    let ordered: Vec<&Item> = trail::feature_order(&numbers, &stories)
        .into_iter()
        .filter_map(|n| s.item(n))
        .collect();
    let exe = match std::env::var_os("AGENT_MUX_PROJECT_EXE") {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe()?,
    };
    let root = e.cfg.root().to_path_buf();
    let dry = a.flag("--dry-run");

    if action == "plan" {
        use super::DocState;
        let classify = |i: &Item| {
            trail::classify_for_plan(
                &i.labels,
                i.doc_state("spec") == DocState::Base && i.doc_state("plan") == DocState::Base,
            )
        };
        let mut states: Vec<(u64, String, trail::PlanState, Option<String>)> = ordered
            .iter()
            .map(|i| (i.number, i.title.clone(), classify(i), None))
            .collect();
        let mut ran = false;
        for (n, _, state, note) in states.iter_mut() {
            if *state != trail::PlanState::Generate {
                continue;
            }
            if dry {
                println!("dry-run: would run `agent-mux project run {n} --step qa-plan`");
                continue;
            }
            println!("Feature #{n}: writing and auditing its QA plan");
            let mut cmd = std::process::Command::new(&exe);
            cmd.args([
                "project",
                "run",
                &n.to_string(),
                "--step",
                "qa-plan",
                "--dir",
            ])
            .arg(&root)
            .stdin(std::process::Stdio::null());
            for flag in ["--harness", "--critic"] {
                if let Some(v) = a.value(flag) {
                    cmd.args([flag, v]);
                }
            }
            ran = true;
            match cmd.status() {
                Ok(st) if st.success() => {}
                Ok(st) => *note = Some(format!("the step ended with {st}")),
                Err(x) => *note = Some(format!("the step did not start: {x}")),
            }
        }
        if ran {
            // what the steps left on the issues
            let after = obtain(&e.conn, &e.tools, &e.cfg, Fresh::Refresh)
                .map_err(|x| anyhow::anyhow!(x))?;
            for (n, _, state, _) in states.iter_mut() {
                if let Some(i) = after.item(*n) {
                    *state = classify(i);
                }
            }
        }
        println!(
            "\nreadiness of the trail: milestone \"{}\" ({} Feature(s), in the trail's order)\n",
            milestone.title,
            states.len()
        );
        for (n, title, state, note) in &states {
            println!("  #{n} {title}");
            // a plan that was written and not released says why above
            let label = if *state == trail::PlanState::Generate && !dry && ran {
                "✗ not released: see the step's output above"
            } else {
                state.label()
            };
            println!("    {label}");
            if let Some(note) = note {
                println!("    ! {note}");
            }
        }
        let gated: Vec<String> = states
            .iter()
            .filter(|x| x.2 == trail::PlanState::HumanGate)
            .map(|x| format!("#{}", x.0))
            .collect();
        if !gated.is_empty() {
            println!(
                "\n⛔ a person must act on {}: correct the document the 🔎 comment names and remove the label",
                gated.join(", ")
            );
        }
        let ready = states
            .iter()
            .filter(|x| x.2 == trail::PlanState::Ready)
            .count();
        println!(
            "\n{ready}/{} ready. {}",
            states.len(),
            if ready == states.len() {
                "Review each qa-plan.md, merge its pull request, then `agent-mux project qa-trail run`."
            } else {
                "A cycle starts only when every Feature in QA has `spec-wave:qa-ready`."
            }
        );
        if !dry && ready != states.len() {
            std::process::exit(1);
        }
        return Ok(());
    }

    // ---- run: one cycle
    let settings = trail::settings(&e.cfg);
    let max_cycles = match a.value("--max-cycles") {
        Some(raw) => raw.parse::<u32>().ok().filter(|c| *c > 0).ok_or_else(|| {
            anyhow::anyhow!("--max-cycles takes a number above zero: got \"{raw}\"")
        })?,
        None => settings.max_cycles,
    };
    let parallel = match a.value("--parallel") {
        Some(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|c| *c > 0)
            .ok_or_else(|| {
                anyhow::anyhow!("--parallel takes a number above zero: got \"{raw}\"")
            })?,
        None => settings.max_parallel,
    };
    let harness =
        harness_of(a.value("--harness"), Harness::Claude).map_err(|x| anyhow::anyhow!(x))?;
    if let Some(why) = &s.board_error {
        println!("! the board was not read ({why}): every Feature counts as outside the cycle");
    }
    let mut the_trail: Vec<trail::TrailFeature> = ordered
        .iter()
        .map(|i| trail::TrailFeature {
            number: i.number,
            title: i.title.clone(),
            labels: i.labels.clone(),
            scope: trail::classify_scope(i.stage.as_deref()),
        })
        .collect();
    if let Some(raw) = a.value("--only") {
        let wanted: Vec<u64> = raw
            .split(',')
            .map(|x| x.trim().trim_start_matches('#').parse::<u64>())
            .collect::<Result<_, _>>()
            .map_err(|_| {
                anyhow::anyhow!("--only takes Feature numbers, as 12 or 12,15: got \"{raw}\"")
            })?;
        let unknown: Vec<String> = wanted
            .iter()
            .filter(|n| !the_trail.iter().any(|t| t.number == **n))
            .map(|n| format!("#{n}"))
            .collect();
        if !unknown.is_empty() {
            anyhow::bail!(
                "--only names Feature(s) that are not of the milestone: {}",
                unknown.join(", ")
            );
        }
        the_trail.retain(|t| wanted.contains(&t.number));
    }
    let missing = trail::run_gate(&the_trail);
    if !missing.is_empty() {
        anyhow::bail!(
            "Feature(s) in QA without `{}`: {}. A cycle starts only with the whole trail reviewed: run `agent-mux project qa-trail plan` and review the plans",
            super::contract::LABEL_QA_READY,
            missing
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let in_cycle: Vec<&trail::TrailFeature> = the_trail
        .iter()
        .filter(|t| t.scope == trail::Scope::InCycle)
        .collect();
    let previous = trail::cycles(&scratch, &s.hub, &hub_root, &milestone.title)
        .into_values()
        .next_back();
    let cycle = previous.as_ref().map_or(0, |p| p.cycle) + 1;

    // the Bugs of the cycle before: the closed ones are what changed
    let mut previous_bugs: Vec<u64> = previous
        .iter()
        .flat_map(|p| {
            p.features
                .iter()
                .flat_map(|f| f.bugs_opened.clone())
                .chain(p.bugs.open_blocking.clone())
        })
        .collect();
    previous_bugs.sort_unstable();
    previous_bugs.dedup();
    let mut closed_bodies: Vec<String> = Vec::new();
    let mut still_open: Vec<u64> = Vec::new();
    for n in &previous_bugs {
        match super::github::issue_state(&e.tools, &e.cfg, *n) {
            Ok((true, _)) => still_open.push(*n),
            Ok((false, body)) => closed_bodies.push(body),
            // a Bug that cannot be read is neither closed nor a blocker
            Err(x) => println!("! Bug #{n} was not read: {x}"),
        }
    }
    let closed = closed_bodies.len() as u32;
    trail::next_cycle_gate(previous.as_ref(), closed, max_cycles)
        .map_err(|why| anyhow::anyhow!("cycle {cycle} does not start: {why}"))?;

    let comments_of = |n: u64| -> super::qa::LastReport {
        super::github::issue_detail(&e.tools, &e.cfg, n)
            .map(|d| super::qa::parse_last_report(&d.comments))
            .unwrap_or_default()
    };
    let targets: Vec<trail::Target> = match &previous {
        None => in_cycle
            .iter()
            .map(|t| trail::Target {
                issue: t.number,
                feature: Some(t.number),
                only: None,
                motivo: "ciclo-1",
            })
            .collect(),
        Some(p) => {
            let mut blocked: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
            for t in &in_cycle {
                blocked.insert(
                    t.number,
                    comments_of(t.number)
                        .results
                        .values()
                        .filter(|r| r.verdict == super::qa::Verdict::Blocked)
                        .map(|r| r.numero)
                        .collect(),
                );
            }
            let tested: Vec<trail::FeatureEntry> = p
                .features
                .iter()
                .filter(|f| in_cycle.iter().any(|t| t.number == f.issue))
                .cloned()
                .collect();
            trail::retest_plan(&tested, &blocked, &closed_bodies)
        }
    };
    let feature_of = |t: &trail::Target| -> Option<u64> {
        t.feature
            .or_else(|| s.ancestor(t.issue, ItemType::Feature).map(|f| f.number))
    };

    println!(
        "qa-trail run: milestone \"{}\", cycle {cycle}/{max_cycles}, on {}",
        milestone.title,
        harness.as_str()
    );
    println!(
        "  Features in the cycle: {}",
        if in_cycle.is_empty() {
            "none".to_string()
        } else {
            in_cycle
                .iter()
                .map(|t| format!("#{}", t.number))
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    for t in the_trail
        .iter()
        .filter(|t| t.scope != trail::Scope::InCycle)
    {
        println!("    #{}: {}", t.number, t.scope.as_str());
    }
    println!(
        "  {} target(s), up to {parallel} at once, each in a worktree of its own:",
        targets.len()
    );
    for t in &targets {
        println!("    - {}", t.line());
    }
    if dry {
        println!("  dry-run: nothing ran and nothing was written");
        return Ok(());
    }
    if targets.is_empty() && in_cycle.is_empty() {
        println!("  no Feature of this milestone is in QA: nothing to run");
        return Ok(());
    }

    let now_iso = || {
        time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default()
    };
    let started_at = now_iso();
    let planned = |title: &str| -> u32 {
        let path = format!(
            "{}/qa-plan.md",
            super::contract::doc_dir(ItemType::Feature, title)
        );
        super::deliver::read_base_file(&hub_root, &path)
            .ok()
            .flatten()
            .and_then(|c| super::qa_plan::parse(&c).ok())
            .map_or(0, |p| p.scenarios.len() as u32)
    };
    let outside: Vec<trail::FeatureEntry> = the_trail
        .iter()
        .filter(|t| t.scope != trail::Scope::InCycle)
        .map(|t| trail::out_of_cycle_entry(t.number, &t.title, t.scope))
        .collect();
    let publish = |report: &trail::Report| {
        match trail::keep(&scratch, &s.hub, report) {
            Ok(file) => println!("  cycle {} kept in {}", report.cycle, file.display()),
            Err(x) => println!("  ! the report was not kept: {x}"),
        }
        let dir = trail::trail_dir(&milestone.title, report.cycle);
        let md = format!("{dir}/report.md");
        let published = super::deliver::publish(
            &e.tools,
            &hub_root,
            &scratch,
            &super::deliver::Artifact {
                doc: &format!("qa-trail-c{}", report.cycle),
                number: milestone.number,
                issue_title: &format!("milestone \"{}\"", milestone.title),
                issue_url: &format!(
                    "https://github.com/{}/milestone/{}",
                    s.hub, milestone.number
                ),
                path: &md,
                content: &trail::render_md(report),
                next_step: None,
                extra: &[(format!("{dir}/report.json"), trail::render_json(report))],
            },
        );
        match published {
            Ok(p) => println!(
                "  report published on {}{}",
                p.branch,
                p.pr_url
                    .map(|u| format!(": {u}"))
                    .or(p.pr_error.map(|x| format!(" (! no pull request: {x})")))
                    .unwrap_or_default()
            ),
            Err(x) => println!("  ! the report was not published: {x}"),
        }
        let block = trail::milestone_block(report, &md);
        let written = super::github::milestone_description(&e.tools, &e.cfg, milestone.number)
            .and_then(|now| {
                super::write::set_milestone_description(
                    &e.tools,
                    &e.cfg,
                    milestone.number,
                    &trail::upsert_block(&now, &block),
                )
            });
        match written {
            Ok(()) => println!("  the milestone's description has the cycle's block"),
            Err(x) => println!("  ! the milestone's description was not updated: {x}"),
        }
    };

    // preflight: what would abort every target alike
    let mut checks = vec![trail::Check {
        name: format!("harness {}", harness.as_str()),
        ok: crate::session::command_available(harness.command()),
        detail: String::new(),
    }];
    if !checks[0].ok {
        checks[0].detail = format!("`{}` is not on PATH", harness.command());
    }
    let free_mb = std::process::Command::new("df")
        .arg("-Pk")
        .arg(scratch.parent().unwrap_or(&root))
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .last()?
                .split_whitespace()
                .nth(3)?
                .parse::<u64>()
                .ok()
        })
        .map(|kb| kb / 1024);
    checks.push(trail::Check {
        name: "espaço em disco".into(),
        ok: free_mb.is_none_or(|mb| mb > 1024),
        detail: free_mb.map_or("não verificável".into(), |mb| format!("{mb}MB livres")),
    });
    let preflight = trail::Preflight::of(checks);
    if !preflight.ok {
        let mut entries: Vec<trail::FeatureEntry> = in_cycle
            .iter()
            .map(|t| trail::aborted_entry(t.number, &t.title, planned(&t.title), None))
            .collect();
        entries.extend(outside);
        let report = trail::build_report(trail::ReportInput {
            milestone_number: milestone.number,
            milestone_title: milestone.title.clone(),
            cycle,
            started_at: Some(started_at),
            finished_at: Some(now_iso()),
            preflight: preflight.clone(),
            features: entries,
            previous_totals: previous.as_ref().map(|p| p.totals),
            closed_since_previous_cycle: closed,
            open_blocking: still_open,
            termination_reason: Some("preflight-falhou".into()),
        });
        publish(&report);
        for c in preflight.checks.iter().filter(|c| !c.ok) {
            eprintln!("  ✗ {} — {}", c.name, c.detail);
        }
        eprintln!("the preflight failed: the cycle was aborted, and its report written");
        std::process::exit(1);
    }

    // dispatch: each target is `agent-mux project qa`, a process of its own
    struct Outcome {
        report: Option<super::qa::LastReport>,
        duration_sec: u64,
    }
    let logs = crate::tracing::analysis::default_snapshot_dir().join("project-qa-trail");
    std::fs::create_dir_all(&logs)?;
    let queue = std::sync::Mutex::new(targets.iter().collect::<std::collections::VecDeque<_>>());
    let outcomes: std::sync::Mutex<BTreeMap<u64, Outcome>> = std::sync::Mutex::new(BTreeMap::new());
    let timeout = std::time::Duration::from_secs(settings.feature_timeout_min * 60);
    std::thread::scope(|scope| {
        for _ in 0..parallel.min(targets.len()) {
            scope.spawn(|| {
                // the lock is let go before the target runs
                let next = || queue.lock().ok().and_then(|mut q| q.pop_front());
                while let Some(t) = next() {
                    let before = comments_of(t.issue).run;
                    println!("▶ {}", t.line());
                    let started = std::time::Instant::now();
                    let log = logs.join(format!("{}-c{cycle}-{}.log", fleet_name(&s.hub), t.issue));
                    let mut timed_out = false;
                    let spawned = std::fs::File::create(&log).and_then(|out| {
                        let err = out.try_clone()?;
                        let mut cmd = std::process::Command::new(&exe);
                        cmd.args(["project", "qa", &t.issue.to_string(), "--dir"])
                            .arg(&root)
                            .args(["--harness", harness.as_str()])
                            .stdin(std::process::Stdio::null())
                            .stdout(out)
                            .stderr(err);
                        if let Some(only) = &t.only {
                            cmd.args([
                                "--only",
                                &only
                                    .iter()
                                    .map(u32::to_string)
                                    .collect::<Vec<_>>()
                                    .join(","),
                            ]);
                        }
                        #[cfg(unix)]
                        {
                            use std::os::unix::process::CommandExt;
                            cmd.process_group(0);
                        }
                        cmd.spawn()
                    });
                    match spawned {
                        Ok(mut child) => loop {
                            match child.try_wait() {
                                Ok(Some(_)) | Err(_) => break,
                                Ok(None) if started.elapsed() > timeout => {
                                    timed_out = true;
                                    stop_run(&mut child);
                                    break;
                                }
                                Ok(None) => {
                                    std::thread::sleep(std::time::Duration::from_millis(500))
                                }
                            }
                        },
                        Err(x) => println!("  ! qa {} did not start: {x}", t.issue),
                    }
                    let after = comments_of(t.issue);
                    let fresh = (after.run > before).then_some(after);
                    let secs = started.elapsed().as_secs();
                    println!(
                        "◀ qa {}: {} ({secs}s; log {})",
                        t.issue,
                        match (&fresh, timed_out) {
                            (Some(r), _) => r.verdict.map_or("report", |v| v.as_str()),
                            (None, true) => "timeout",
                            (None, false) => "no report",
                        },
                        log.display()
                    );
                    if let Ok(mut o) = outcomes.lock() {
                        o.insert(
                            t.issue,
                            Outcome {
                                report: fresh,
                                duration_sec: secs,
                            },
                        );
                    }
                }
            });
        }
    });
    let outcomes = outcomes.into_inner().unwrap_or_default();

    let mut entries: Vec<trail::FeatureEntry> = Vec::new();
    for t in &in_cycle {
        let mine: Vec<&trail::Target> = targets
            .iter()
            .filter(|x| feature_of(x) == Some(t.number))
            .collect();
        if mine.is_empty() {
            // not tested again: what the last cycle said stands
            if let Some(prev) = previous
                .as_ref()
                .and_then(|p| p.features.iter().find(|f| f.issue == t.number))
            {
                entries.push(trail::FeatureEntry {
                    duration_sec: None,
                    ..prev.clone()
                });
            }
            continue;
        }
        let mut counts = trail::Counts::default();
        let mut reasons: BTreeMap<String, u32> = BTreeMap::new();
        let mut bugs: Vec<u64> = Vec::new();
        let mut duration = 0u64;
        let mut aborted = false;
        let mut worst = trail::VERDE;
        for target in mine {
            let Some(o) = outcomes.get(&target.issue) else {
                aborted = true;
                continue;
            };
            duration += o.duration_sec;
            let Some(r) = &o.report else {
                aborted = true;
                continue;
            };
            let (c, why) = trail::scenario_counts(r.results.values());
            counts.pass += c.pass;
            counts.fail += c.fail;
            counts.blocked += c.blocked;
            counts.total += c.total;
            for (k, n) in why {
                *reasons.entry(k).or_default() += n;
            }
            bugs.extend(r.bugs.iter().filter(|b| !b.existing).map(|b| b.number));
            let verdict = r.verdict.unwrap_or_else(|| {
                super::qa::aggregate(&r.results.values().cloned().collect::<Vec<_>>())
            });
            worst = trail::worst_of(worst, trail::feature_verdict(Some(verdict)));
        }
        if aborted {
            let known = counts.pass + counts.fail + counts.blocked;
            let plan = match planned(&t.title) {
                0 => counts.total,
                n => n,
            };
            let lost = plan.saturating_sub(known);
            let mut entry = trail::aborted_entry(t.number, &t.title, lost, Some(duration));
            entry.scenarios.pass = counts.pass;
            entry.scenarios.fail = counts.fail;
            entry.scenarios.blocked += counts.blocked;
            entry.scenarios.total =
                entry.scenarios.pass + entry.scenarios.fail + entry.scenarios.blocked;
            if lost > 0 {
                *reasons.entry("ambiente".into()).or_default() += lost;
            }
            entry.blocked_reasons = reasons;
            entry.bugs_opened = bugs;
            entries.push(entry);
            continue;
        }
        entries.push(trail::FeatureEntry {
            issue: t.number,
            title: t.title.clone(),
            verdict: worst.to_string(),
            scenarios: counts,
            bugs_opened: bugs,
            duration_sec: Some(duration),
            blocked_reasons: reasons,
        });
    }
    entries.extend(outside);
    let mut open_blocking = still_open;
    open_blocking.extend(entries.iter().flat_map(|f| f.bugs_opened.clone()));
    open_blocking.sort_unstable();
    open_blocking.dedup();
    let mut report = trail::build_report(trail::ReportInput {
        milestone_number: milestone.number,
        milestone_title: milestone.title.clone(),
        cycle,
        started_at: Some(started_at),
        finished_at: Some(now_iso()),
        preflight,
        features: entries,
        previous_totals: previous.as_ref().map(|p| p.totals),
        closed_since_previous_cycle: closed,
        open_blocking,
        termination_reason: None,
    });
    let changed = trail::changed_since_previous(previous.as_ref(), &report, closed);
    report.termination_reason =
        trail::termination_of(true, &report.verdict, cycle, max_cycles, changed)
            .map(str::to_string);
    publish(&report);
    println!(
        "\nthe trail's verdict: {}{}",
        report.verdict,
        report
            .termination_reason
            .as_deref()
            .map(|r| format!(" (ended: {r})"))
            .unwrap_or_else(|| " (another cycle may follow)".into())
    );
    if report.verdict != trail::LIBERADA {
        std::process::exit(1);
    }
    Ok(())
}

/// `project insights [<n>] [--scope S] [--topic T] [--harness H]`.
async fn cmd_insights(a: &Args) -> anyhow::Result<()> {
    use super::insights::{self, Scope};
    let e = env(a)?;
    let scope = match a.value("--scope") {
        Some(raw) => Scope::parse(raw).ok_or_else(|| {
            anyhow::anyhow!("\"{raw}\" is not a scope: use progress, tech, daily or brainstorm")
        })?,
        None => Scope::Progress,
    };
    let root = match a.positional.first() {
        Some(_) => Some(number(a)?),
        None => None,
    };
    let harness =
        harness_of(a.value("--harness"), Harness::Claude).map_err(|x| anyhow::anyhow!(x))?;
    let s = snap(&e)?;
    if let Some(n) = root
        && s.item(n).is_none()
    {
        anyhow::bail!("issue #{n} is not in the Project");
    }
    let digest = insights::digest(&s, root, insights::MAX_ITEMS);
    println!(
        "{}: {} with the flow {} on {} (a read-only session; nothing is written to GitHub)",
        s.hub,
        scope.label(),
        insights::FLOW_INSIGHTS,
        harness.as_str()
    );
    if a.flag("--dry-run") {
        println!("{digest}");
        return Ok(());
    }
    let hub_root = steps::hub_root(&e.cfg).map_err(|x| anyhow::anyhow!(x))?;
    let tag = super::ProjectLaunch {
        hub: s.hub.clone(),
        // a reading of the whole Project belongs to no item
        item: root.unwrap_or(0),
        item_type: root
            .and_then(|n| s.item(n))
            .and_then(|i| i.item_type)
            .map(|t| t.as_str().to_string()),
        step: format!("insights:{}", scope.as_str()),
        attempt: 1,
        executor: "operator".to_string(),
        ..Default::default()
    };
    let result = run_flow(
        insights::FLOW_INSIGHTS,
        &hub_root,
        harness,
        json!({
            "scope": scope.as_str(),
            "digest": digest,
            "topic": a.value("--topic").unwrap_or(""),
        }),
        None,
        tag,
    )
    .await
    .map_err(|x| anyhow::anyhow!(x))?;
    let text = result
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        anyhow::bail!("the session answered nothing");
    }
    let actions: Vec<String> = result
        .get("actions")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(Value::as_str)
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let doc = insights::document(&s.hub, scope, root, text, &actions, &at);
    println!("\n{doc}");
    let scratch = steps::scratch_dir(&crate::tracing::analysis::default_snapshot_dir());
    match insights::save(&scratch, &s.hub, scope, &doc) {
        Ok(file) => println!("kept in {}", file.display()),
        Err(why) => eprintln!("the answer was not kept: {why}"),
    }
    Ok(())
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
    let qa_plan = matches!(a.value("--step"), Some("qa-plan" | "qa_plan" | "qaplan"));
    let forced = match a.value("--step") {
        Some(_) if fix_pr || qa_plan => None,
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
    let develop = !qa_plan
        && match forced {
            Some(f) => f == next::Action::Implement,
            None => super::build::decision(&item, &s, false).is_some(),
        };
    let mut planned = if fix_pr {
        ctx.fix_plan(&item, &detail, &s)
    } else if qa_plan {
        ctx.qa_plan_step(&item, &detail, &s)
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
                // the fleet's other delivery: one branch, one pull request
                let agent = match a.value("--delivery") {
                    None | Some("pr-stack") => false,
                    Some("agent") => true,
                    Some(other) => anyhow::bail!("--delivery {other:?}: use pr-stack or agent"),
                };
                if agent {
                    println!(
                        "  delivery: agent. Every Story is a checkpoint on `{}`, and the Feature gets one pull request after the last",
                        super::build::agent_branch(n)
                    );
                }
                if a.flag("--dry-run") {
                    break;
                }
                // the snapshot follows the stack: each delivered Story is in
                // code review with its pull request, for the next one's base
                let mut seen = s.clone();
                let total = p.pending.len();
                if agent {
                    // Stories checkpointed by an earlier run have no pull
                    // request of their own: their code is on the one branch
                    let branch = super::build::agent_branch(n);
                    for st in &p.skipped {
                        let Some(it) = seen.items.iter_mut().find(|x| x.number == st.number) else {
                            continue;
                        };
                        if it.open
                            && !it.pulls.iter().any(|p| p.open())
                            && let Ok(target) = super::build::target_repo(&hub_root, &e.cfg, it)
                        {
                            it.pulls.push(checkpoint_pull(&target.repo.slug(), &branch));
                        }
                    }
                }
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
                        Ok((d, Planned::Build(mut b))) if agent => {
                            let branch = super::build::agent_branch(n);
                            let made = crate::factory::deliver::free_branch(&b.target.dir, &branch)
                                .map(|free| free != branch)
                                .and_then(|on_remote| {
                                    super::build::as_checkpoint(
                                        &mut b,
                                        super::build::AgentDelivery {
                                            feature: n,
                                            feature_title: item.title.clone(),
                                            stories: p
                                                .order
                                                .iter()
                                                .copied()
                                                .filter(|x| seen.item(*x).is_some_and(|i| i.open))
                                                .collect(),
                                            last: i + 1 == total,
                                        },
                                        on_remote,
                                    )
                                });
                            if let Err(x) = made {
                                failed = Some(stopped(x));
                                break;
                            }
                            (d, b)
                        }
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
            if url.is_none() && b.agent.is_some() {
                // a checkpoint: the Story's code is on the Feature's branch
                it.pulls
                    .push(checkpoint_pull(&b.target.repo.slug(), &b.branch));
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

/// What stands in the snapshot for a Story that was checkpointed on its
/// Feature's one branch: its code is there, with no pull request of its
/// own yet.
fn checkpoint_pull(repo: &str, branch: &str) -> super::Pull {
    super::Pull {
        repo: repo.to_string(),
        number: 0,
        url: String::new(),
        state: "OPEN".into(),
        draft: true,
        head: branch.to_string(),
        base: String::new(),
        review: String::new(),
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

/// The milestone a command is about: `--milestone M`, or the words after
/// the verb.
fn milestone_arg(a: &Args, verb: &str) -> anyhow::Result<String> {
    let reference = match a.value("--milestone") {
        Some(m) => m.trim().to_string(),
        None => a.positional.join(" ").trim().to_string(),
    };
    if reference.is_empty() {
        anyhow::bail!(
            "which milestone? give its number or its title: `agent-mux project {verb} --milestone <M>`"
        );
    }
    Ok(reference)
}

/// The Features of a milestone, in number order.
fn milestone_features<'a>(s: &'a Snapshot, title: &str) -> Vec<&'a Item> {
    let mut out: Vec<&Item> = s
        .items
        .iter()
        .filter(|i| i.item_type == Some(ItemType::Feature) && i.milestone.as_deref() == Some(title))
        .collect();
    out.sort_by_key(|i| i.number);
    out
}

/// What would make every generation of a round fail alike: gh's login,
/// who runs a step, the harness, the hub's checkout and the push a
/// document is published with.
fn preflight_environment(
    tools: &Tools,
    cfg: &ProjectConfig,
    s: &Snapshot,
    harness: Harness,
) -> Vec<doctor::Check> {
    use doctor::{Check, Status};
    let hub = cfg.hub_repo().slug();
    let cwd = cfg.root();
    let probe = doctor::Probe {
        auth: Some(super::github::auth_status(tools, cwd)),
        execution_var: Some(super::github::variable(
            tools,
            cwd,
            &hub,
            "SPEC_WAVE_EXECUTION",
        )),
        workflows: Some(super::github::workflow_files(tools, cwd, &hub)),
        labels: None,
    };
    let mut out: Vec<Check> = doctor::checks(cfg, &Ok(s.clone()), &probe)
        .into_iter()
        .filter(|c| c.name == "gh" || c.name == "execution mode")
        .collect();
    let check = |status, name: &str, detail: String| Check {
        status,
        name: name.to_string(),
        detail,
    };
    out.push(if crate::session::command_available(harness.command()) {
        check(
            Status::Ok,
            "harness",
            format!("{} is installed", harness.as_str()),
        )
    } else {
        check(
            Status::Fail,
            "harness",
            format!(
                "`{}` is not on PATH: no session could write a document",
                harness.command()
            ),
        )
    });
    out.push(match steps::hub_root(cfg) {
        Ok(root) => check(Status::Ok, "hub checkout", root.display().to_string()),
        Err(why) => check(Status::Fail, "hub checkout", why),
    });
    out.push(match super::github::can_push(tools, cwd, &hub) {
        Ok(Some(true)) => check(
            Status::Ok,
            "publishing",
            format!("gh's login may push to {hub}: a document is published by pull request"),
        ),
        Ok(Some(false)) => check(
            Status::Fail,
            "publishing",
            format!(
                "gh's login may not push to {hub}: a document would be generated and never published"
            ),
        ),
        Ok(None) => check(
            Status::Skip,
            "publishing",
            format!("GitHub did not say whether gh's login may push to {hub}"),
        ),
        Err(why) => check(
            Status::Skip,
            "publishing",
            why.lines().next().unwrap_or("").to_string(),
        ),
    });
    out
}

/// A milestone's preflight, as read: what `project preflight` prints and
/// what the Milestones screen shows.
pub struct PreflightRead {
    pub environment: Vec<doctor::Check>,
    pub inventory: super::audit::Inventory,
    pub verdict: super::audit::PreflightVerdict,
}

impl PreflightRead {
    pub fn blocked(&self) -> bool {
        self.verdict.status == "bloqueio"
    }
}

/// Reads a milestone's preflight: the environment from `gh` and this
/// machine, the inventory from the snapshot. Writes nothing.
pub fn preflight_read(
    tools: &Tools,
    cfg: &ProjectConfig,
    s: &Snapshot,
    milestone: &super::Milestone,
    harness: Harness,
) -> PreflightRead {
    use super::audit;
    let inventory = audit::inspect_milestone(&milestone_features(s, &milestone.title));
    let environment = preflight_environment(tools, cfg, s, harness);
    let verdict = audit::preflight_verdict(&environment, &inventory);
    PreflightRead {
        environment,
        inventory,
        verdict,
    }
}

/// The preflight for a person, line by line.
pub fn preflight_lines(
    s: &Snapshot,
    milestone: &super::Milestone,
    r: &PreflightRead,
) -> Vec<String> {
    use super::audit;
    let (environment, inventory, verdict) = (&r.environment, &r.inventory, &r.verdict);
    let mut out = vec![format!(
        "Preflight of {} (base: {})",
        s.hub, s.default_branch
    )];
    for c in environment {
        out.push(format!("  {} {:<16} {}", c.status.mark(), c.name, c.detail));
    }
    out.push(String::new());
    out.push(format!(
        "Milestone \"{}\": {} Feature(s), {} to generate",
        milestone.title,
        inventory.features.len(),
        inventory.gerar.len()
    ));
    for f in &inventory.features {
        out.push(format!(
            "  {:<22} #{} {}{}",
            audit::spec_mark(f),
            f.number,
            f.title.chars().take(60).collect::<String>(),
            f.pr.as_ref()
                .map(|p| format!("  PR #{}", p.number))
                .unwrap_or_default()
        ));
    }
    if !inventory.gatilhos_pendentes.is_empty() {
        out.push(String::new());
        out.push("  Trigger labels left behind (remove them before generating):".to_string());
        for g in &inventory.gatilhos_pendentes {
            out.push(format!("    #{}: {}", g.number, g.labels.join(", ")));
        }
    }
    if !verdict.avisos.is_empty() || !verdict.bloqueios.is_empty() {
        out.push(String::new());
    }
    // an environment line is in the checklist above already
    let shown: Vec<String> = environment
        .iter()
        .map(|c| format!("{}: {}", c.name, c.detail))
        .collect();
    for w in verdict.avisos.iter().filter(|w| !shown.contains(w)) {
        out.push(format!("  ! {w}"));
    }
    for b in verdict.bloqueios.iter().filter(|b| !shown.contains(b)) {
        out.push(format!("  ✗ {b}"));
    }
    out.push(String::new());
    out.push(if r.blocked() {
        format!(
            "{} block(s). Fix them before generating.",
            verdict.bloqueios.len()
        )
    } else if inventory.gerar.is_empty() {
        "Preflight ok. Nothing to generate.".to_string()
    } else {
        format!(
            "Preflight ok. {} spec(s) to generate: `agent-mux project run <n>` on {}.",
            inventory.gerar.len(),
            inventory
                .gerar
                .iter()
                .map(|f| format!("#{}", f.number))
                .collect::<Vec<_>>()
                .join(", ")
        )
    });
    out
}

/// `project preflight --milestone M [--harness H] [--json]`: the port of
/// spec-wave's `preflight`. Reads only.
fn cmd_preflight(a: &Args) -> anyhow::Result<()> {
    use super::trail;
    let reference = milestone_arg(a, "preflight")?;
    let harness =
        harness_of(a.value("--harness"), Harness::Claude).map_err(|x| anyhow::anyhow!(x))?;
    let mut e = env(a)?;
    if !a.flag("--cached") {
        // a round is decided on what GitHub says now
        e.fresh = Fresh::Refresh;
    }
    let s = snap(&e)?;
    let milestone = trail::resolve_milestone(&s.milestones, &reference)
        .map_err(|x| anyhow::anyhow!(x))?
        .clone();
    let read = preflight_read(&e.tools, &e.cfg, &s, &milestone, harness);

    if a.flag("--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "milestone": {
                    "title": milestone.title,
                    "number": milestone.number,
                    "base": s.default_branch,
                },
                "ambiente": read.environment,
                "inventario": read.inventory,
                "veredito": read.verdict,
            }))?
        );
    } else {
        for line in preflight_lines(&s, &milestone, &read) {
            println!("{line}");
        }
    }
    if read.blocked() {
        std::process::exit(1);
    }
    Ok(())
}

/// A Feature's spec as it is now, wherever it is: on the default branch
/// or on its artifact branch. A read that fails is `Unknown`, never
/// "there is none".
fn read_spec(hub_root: &Path, item: &Item) -> (Option<String>, DocState) {
    let Some(doc) = item.docs.get("spec") else {
        return (None, DocState::Missing);
    };
    let read = match doc.state {
        DocState::Base => super::deliver::read_base_file(hub_root, &doc.path),
        DocState::Pr | DocState::Branch => {
            let branch = doc
                .branch
                .clone()
                .unwrap_or_else(|| super::contract::artifact_branch(item.number, "spec"));
            super::deliver::read_branch_file(hub_root, &branch, &doc.path)
        }
        other => return (None, other),
    };
    match read {
        Ok(Some(content)) if !content.trim().is_empty() => (Some(content), doc.state),
        _ => (None, DocState::Unknown),
    }
}

/// A milestone's audit, as read: what `project audit` prints and what
/// the Milestones screen shows.
pub struct AuditRead {
    pub features: Vec<super::audit::Target>,
    pub result: super::audit::Audit,
    pub verdict: super::audit::AuditVerdict,
}

/// Reads a milestone's audit: each open Feature's spec where it is, the
/// `#N` the snapshot does not know from GitHub, and the files of the hub
/// and of the code repositories beside it. Writes nothing.
pub fn audit_read(
    tools: &Tools,
    cfg: &ProjectConfig,
    s: &Snapshot,
    milestone: &super::Milestone,
) -> Result<AuditRead, String> {
    use super::audit;
    let hub_root = steps::hub_root(cfg)?;
    let milestone_ref = |title: &str| {
        s.milestones
            .iter()
            .find(|m| m.title == title)
            .map(|m| audit::MilestoneRef {
                number: m.number,
                title: m.title.clone(),
                due_on: m.due.clone(),
            })
    };
    let entry = |i: &Item| audit::CatalogEntry {
        number: i.number,
        title: i.title.clone(),
        closed: !i.open,
        milestone: i.milestone.as_deref().and_then(milestone_ref),
    };

    // what a spec may name: the Features of every milestone
    let mut catalog: Vec<audit::CatalogEntry> = s
        .items
        .iter()
        .filter(|i| i.item_type == Some(ItemType::Feature))
        .map(entry)
        .collect();
    let this = audit::MilestoneRef {
        number: milestone.number,
        title: milestone.title.clone(),
        due_on: milestone.due.clone(),
    };
    let features: Vec<audit::Target> = milestone_features(s, &milestone.title)
        .into_iter()
        .filter(|i| i.open)
        .map(|i| {
            let (spec, spec_state) = read_spec(&hub_root, i);
            audit::Target {
                number: i.number,
                title: i.title.clone(),
                milestone: this.clone(),
                spec,
                spec_state,
            }
        })
        .collect();
    if features.is_empty() {
        return Err(format!(
            "no open Feature in milestone \"{}\"",
            milestone.title
        ));
    }

    // `#N` the catalog does not know: an issue in no milestone, a Story of
    // another Feature, or a number that does not exist. GitHub says which
    let mut unknown: Vec<u64> = Vec::new();
    for f in &features {
        let section = f.spec.as_deref().and_then(audit::dependency_section);
        for n in audit::issue_refs(section.as_deref().unwrap_or("")) {
            if !catalog.iter().any(|c| c.number == n) && !unknown.contains(&n) {
                unknown.push(n);
            }
        }
    }
    let mut missing: Vec<u64> = Vec::new();
    if !unknown.is_empty() {
        // a read that fails stops here: it must not turn into "does not exist"
        let found = ops::read_items(tools, cfg, &unknown)
            .map_err(|x| format!("the issues the specs name could not be read: {x}"))?;
        for n in &unknown {
            match found.iter().find(|i| i.number == *n) {
                Some(i) => catalog.push(entry(i)),
                None => missing.push(*n),
            }
        }
    }

    // the code: the hub's own, and each code repository beside it
    let mut repos = vec![audit::RepoFiles {
        prefix: String::new(),
        files: audit::list_repo_files(&hub_root),
    }];
    for r in &cfg.repositories {
        if let Some(dir) = hub_root
            .parent()
            .map(|p| p.join(&r.repo.repo))
            .filter(|d| d.is_dir() && *d != hub_root)
        {
            repos.push(audit::RepoFiles {
                prefix: r.repo.repo.clone(),
                files: audit::list_repo_files(&dir),
            });
        }
    }

    let result = audit::audit_milestone(&features, &catalog, &missing, &repos);
    let verdict = audit::audit_verdict(&result);
    Ok(AuditRead {
        features,
        result,
        verdict,
    })
}

/// The audit's first line.
pub fn audit_head(s: &Snapshot, milestone: &super::Milestone, features: usize) -> String {
    format!(
        "Milestone \"{}\" of {}: {} Feature(s) audited",
        milestone.title, s.hub, features
    )
}

/// What the audit found, line by line: the graph, then the warnings and
/// the material findings.
pub fn audit_lines(
    result: &super::audit::Audit,
    verdict: &super::audit::AuditVerdict,
) -> Vec<String> {
    let mut out = Vec::new();
    if !result.grafo.is_empty() {
        out.push(String::new());
        out.push("Dependencies among Features (read from the specs):".to_string());
        for g in &result.grafo {
            out.push(format!("  {}", super::audit::edge_line(g)));
        }
    }
    if !verdict.avisos.is_empty() || !verdict.materiais.is_empty() {
        out.push(String::new());
    }
    for w in &verdict.avisos {
        out.push(format!("  ! {w}"));
    }
    for m in &verdict.materiais {
        out.push(format!("  ✗ {m}"));
    }
    out
}

/// The audit's last line.
pub fn audit_tail(verdict: &super::audit::AuditVerdict) -> String {
    match verdict.status {
        "problema" => format!(
            "{} material finding(s). Decide before generating the plans.",
            verdict.materiais.len()
        ),
        "aviso" => "Nothing material: look at the warnings before going on.".to_string(),
        _ => "The milestone's dependencies close. Go on to the plans.".to_string(),
    }
}

/// `project audit --milestone M [--critique] [--critic H] [--json]
/// [--dry-run]`: the port of spec-wave's `audit`. Writes nothing.
async fn cmd_audit(a: &Args) -> anyhow::Result<()> {
    use super::{audit, trail};
    let reference = milestone_arg(a, "audit")?;
    let as_json = a.flag("--json");
    let critic = harness_of(
        a.value("--critic").or(a.value("--harness")),
        Harness::Claude,
    )
    .map_err(|x| anyhow::anyhow!(x))?;
    let mut e = env(a)?;
    if !a.flag("--cached") {
        e.fresh = Fresh::Refresh;
    }
    let s = snap(&e)?;
    let milestone = trail::resolve_milestone(&s.milestones, &reference)
        .map_err(|x| anyhow::anyhow!(x))?
        .clone();
    let hub_root = steps::hub_root(&e.cfg).map_err(|x| anyhow::anyhow!(x))?;
    let AuditRead {
        features,
        result,
        mut verdict,
    } = audit_read(&e.tools, &e.cfg, &s, &milestone).map_err(|x| anyhow::anyhow!(x))?;
    if !as_json {
        println!("{}", audit_head(&s, &milestone, features.len()));
    }

    // the set critique: one session over every spec, after what a grep can do
    let mut critique: Option<Value> = None;
    if a.flag("--critique") {
        let with_spec: Vec<(u64, &str, &str)> = features
            .iter()
            .filter_map(|f| Some((f.number, f.title.as_str(), f.spec.as_deref()?)))
            .collect();
        if with_spec.len() < 2 {
            verdict.avisos.push(format!(
                "The set critique did not run: {} spec(s) available, and a set of one has no pair.",
                with_spec.len()
            ));
            if verdict.status == "ok" {
                verdict.status = "aviso";
            }
        } else if a.flag("--dry-run") {
            if !as_json {
                println!(
                    "  would run the set critique over {} specs with the flow {} on {} (a read-only session)",
                    with_spec.len(),
                    steps::FLOW_CRITIQUE,
                    critic.as_str()
                );
            }
        } else {
            if !as_json {
                println!(
                    "  set critique over {} specs with the flow {} on {} (a read-only session)…",
                    with_spec.len(),
                    steps::FLOW_CRITIQUE,
                    critic.as_str()
                );
            }
            let model = step_model(&e.cfg, critic, "critique", &[], 1);
            let said = model.clone().unwrap_or_else(|| critic.as_str().to_string());
            let answer = run_flow(
                steps::FLOW_CRITIQUE,
                &hub_root,
                critic,
                json!({
                    "kind": "conjunto",
                    "document": audit::set_document(&with_spec),
                    "issue": format!(
                        "Milestone \"{}\" de {}: {} specs, uma por Feature.",
                        milestone.title,
                        s.hub,
                        with_spec.len()
                    ),
                }),
                model,
                super::ProjectLaunch {
                    hub: s.hub.clone(),
                    // the audit of a milestone belongs to no item
                    item: 0,
                    milestone: Some(milestone.title.clone()),
                    step: "audit".to_string(),
                    attempt: 1,
                    role: "critique".to_string(),
                    executor: "operator".to_string(),
                    ..Default::default()
                },
            )
            .await
            // who asked for the critique must not get an "ok" that covered
            // only the cheap half
            .map_err(|x| anyhow::anyhow!("the set critique failed: {x}"))?;
            let known: Vec<u64> = with_spec.iter().map(|(n, _, _)| *n).collect();
            let findings = audit::set_findings(&answer, &known)
                .map_err(|x| anyhow::anyhow!("the set critique's answer was refused: {x}"))?;
            audit::add_set_findings(&mut verdict, &findings);
            critique = Some(json!({
                "model": said,
                "findings": findings,
                // the comment for the issues of both ends of each finding:
                // whoever drives the command posts it, never agent-mux
                "markdown": audit::render_set_markdown(&findings, &said),
            }));
        }
    }
    let problem = verdict.status == "problema";

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "milestone": {"title": milestone.title, "number": milestone.number},
                "features": features
                    .iter()
                    .map(|f| json!({
                        "number": f.number,
                        "title": f.title,
                        "temSpec": f.spec.is_some(),
                    }))
                    .collect::<Vec<_>>(),
                "auditoria": result,
                "critica": critique,
                "veredito": verdict,
            }))?
        );
        if problem {
            std::process::exit(1);
        }
        return Ok(());
    }

    for line in audit_lines(&result, &verdict) {
        println!("{line}");
    }
    if let Some(md) = critique
        .as_ref()
        .and_then(|c| c.get("markdown"))
        .and_then(Value::as_str)
    {
        println!(
            "\nThe set critique, as a comment for the issues it names (nothing was posted):\n\n{md}"
        );
    }
    println!("\n{}", audit_tail(&verdict));
    if problem {
        std::process::exit(1);
    }
    Ok(())
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
    // what its steps' sessions cost, from the trace store
    {
        use crate::tracing::project_report as pr;
        if let Ok(all) = pr::hub_launches(&e.conn, &s.hub, 0)
            && let Some(line) = pr::item_totals(&all, Some(&s), super::now_ns())
                .get(&n)
                .and_then(pr::card_line)
        {
            println!("{line}");
        }
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
