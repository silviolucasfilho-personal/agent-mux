//! `agent-mux factory …`: start a work item, follow it, answer its
//! questions, retry it, and keep the repository's brain.
//!
//! A run is headless: this process holds a private App, runs the
//! `factory` flow to its end, reads the stations' answers from the run's
//! journal, and delivers an approved branch itself.

use super::deliver::{self, Tools};
use super::{
    FLOW, Item, Settlement, Source, State, brain, ci, comment, intake, settle, store,
    title_from_summary, title_of,
};
use crate::app::workflows::WorkflowRunRequest;
use crate::config::FactorySettings;
use crate::harness::Harness;
use crate::loops::worktree::Worktree;
use crate::workflows::{context, journal, library};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

const USAGE: &str = "agent-mux factory — one work item through the line, to a draft pull request

  agent-mux factory run \"<what is wanted>\"      start an item from text
  agent-mux factory run --issue <N>              start an item from a GitHub issue
        [--workspace DIR] [--harness H] [--profile P] [--budget TOKENS]
        [--step <step>.<harness|profile|model|effort|agent>=<value>]… [--json]
  agent-mux factory ls [--state S] [--all] [--json]
  agent-mux factory show <item> [--json]
  agent-mux factory answer <item> \"<answer>\"     answer triage's questions and run again
  agent-mux factory retry <item> [--force]       run again, or deliver again after a failed delivery
  agent-mux factory cancel <item> [--force]        stops the process that runs it, too
  agent-mux factory poll [--dry-run] [--no-run] [--ci] [--workspace DIR]
        issues labeled for the factory become items, then run one by one;
        with [factory] ci_fix or --ci, so do red checks on its pull requests
  agent-mux factory eval [--case NAME]… [--harness H] [--keep] [--json]
        the line's five checks with real harnesses on a throwaway repository
  agent-mux factory brain show|path|edit [--workspace DIR]
  agent-mux factory brain add \"<note>\" [--workspace DIR]

An item is named by its id (wi_…), a unique prefix of it, or #<issue>.
The line is the `factory` workflow (classify, research, analyze, build with
an independent review). An approved branch is pushed under [factory]
branch_prefix and opened as a draft pull request; nothing merges it.
docs/factory.md explains the rest.";

const VALUE_FLAGS: &[&str] = &[
    "--workspace",
    "--harness",
    "--profile",
    "--budget",
    "--issue",
    "--state",
    "--step",
    "--case",
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

    fn all(&self, k: &str) -> Vec<&str> {
        self.values
            .iter()
            .filter(|(f, _)| f == k)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    fn flag(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f == k)
    }
}

fn workspace_of(args: &Args) -> anyhow::Result<PathBuf> {
    let dir = match args.value("--workspace") {
        Some(d) => PathBuf::from(d),
        None => std::env::current_dir()?,
    };
    let dir = dir
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("workspace {}: {e}", dir.display()))?;
    if !crate::loops::worktree::is_git_repo(&dir) {
        anyhow::bail!(
            "{} is not a git repository: the factory works on a branch of one",
            dir.display()
        );
    }
    Ok(dir)
}

struct Env {
    db: PathBuf,
    settings: FactorySettings,
    tools: Tools,
}

fn env() -> anyhow::Result<Env> {
    let cfg = crate::config::load()?;
    let resolved = crate::config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())
        .ok_or_else(|| anyhow::anyhow!("tracing is disabled; the factory needs the store"))?;
    Ok(Env {
        db: resolved.db_path,
        settings: crate::config::resolve_factory(cfg.factory.as_ref()),
        tools: Tools::default(),
    })
}

fn open(env: &Env) -> anyhow::Result<rusqlite::Connection> {
    // a first use before any traced session: the store does not exist yet
    if !env.db.is_file() {
        if let Some(dir) = env.db.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::tracing::store::open_rw(&env.db, crate::tracing::store::OpenOptions::default())
            .map_err(|e| anyhow::anyhow!(e))?;
    }
    crate::tracing::store::open_aux(&env.db).map_err(|e| anyhow::anyhow!(e))
}

fn today() -> String {
    crate::loops::format_timestamp(crate::loops::now())
        .chars()
        .take(10)
        .collect()
}

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let rest = args.get(1..).unwrap_or_default();
    let a = Args::parse(rest);
    match args.first().map(String::as_str) {
        Some("run") => start(&a).await,
        Some("ls") | Some("list") => ls(&a),
        Some("show") => show(&a),
        Some("answer") => answer(&a).await,
        Some("retry") => retry(&a).await,
        Some("cancel") => cancel(&a),
        Some("poll") => poll(&a).await,
        Some("eval") => eval_cmd(&a),
        Some("brain") => brain_cmd(&a),
        Some("help") | Some("--help") | Some("-h") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown factory command {other:?}\n\n{USAGE}"),
    }
}

// ---- reading --------------------------------------------------------------

fn item_json(it: &Item) -> Value {
    serde_json::json!({
        "id": it.id, "workspace": it.workspace, "repo": it.repo,
        "source": it.source.as_str(), "source_ref": it.source_ref,
        "title": it.title, "requester": it.requester, "attended": it.attended,
        "state": it.state.as_str(), "kind": it.kind, "priority": it.priority,
        "complexity": it.complexity, "run_id": it.run_id, "cycles": it.cycles,
        "worktree": it.worktree, "branch": it.branch,
        "pr_number": it.pr_number, "pr_url": it.pr_url, "error": it.error,
    })
}

fn find(conn: &rusqlite::Connection, args: &Args) -> anyhow::Result<Item> {
    let query = args
        .positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("name the item: its id, a prefix of it, or #<issue>"))?;
    let repo = if query.starts_with('#') {
        brain::github_repo(&workspace_of(args)?)
    } else {
        String::new()
    };
    store::resolve(conn, query, &repo)?
        .ok_or_else(|| anyhow::anyhow!("no work item {query:?} (agent-mux factory ls --all)"))
}

fn ls(args: &Args) -> anyhow::Result<()> {
    let env = env()?;
    let conn = open(&env)?;
    let state = match args.value("--state") {
        Some(s) => Some(State::parse(s).ok_or_else(|| {
            anyhow::anyhow!(
                "--state takes one of: {}",
                State::ALL.map(State::as_str).join(", ")
            )
        })?),
        None => None,
    };
    let workspace = if args.flag("--all") {
        None
    } else {
        Some(workspace_of(args)?.to_string_lossy().into_owned())
    };
    let items = store::list(&conn, state, workspace.as_deref(), 200)?;
    if args.flag("--json") {
        let rows: Vec<Value> = items.iter().map(item_json).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if items.is_empty() {
        println!("no work items (agent-mux factory run \"<what is wanted>\")");
        return Ok(());
    }
    for it in &items {
        let tail = match it.state {
            State::Delivered => it.pr_url.clone().unwrap_or_default(),
            _ => it.error.clone().unwrap_or_default(),
        };
        println!(
            "{:<13} {:<12} {:<8} {}{}",
            it.id,
            it.state.as_str(),
            it.kind.as_deref().unwrap_or("-"),
            match it.source {
                Source::Manual => String::new(),
                _ => format!("#{} ", it.source_ref),
            },
            title_of(&it.title, 60),
        );
        if !tail.is_empty() {
            println!("{:<13} {}", "", title_of(&tail, 100));
        }
    }
    Ok(())
}

fn print_item(it: &Item) {
    println!("{} · {} · {}", it.handle(), it.title, it.state.as_str());
    let mut facts: Vec<String> = [&it.kind, &it.priority, &it.complexity]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    if it.cycles > 0 {
        facts.push(format!("{} revision(s)", it.cycles));
    }
    if !facts.is_empty() {
        println!("  {}", facts.join(" · "));
    }
    let field = |name: &str, v: &Option<String>| {
        if let Some(v) = v {
            println!("  {name:<9} {v}");
        }
    };
    field("id", &Some(it.id.clone()));
    field("branch", &it.branch);
    field("pull", &it.pr_url);
    field("worktree", &it.worktree);
    field("run", &it.run_id);
    field("why", &it.error);
}

fn show(args: &Args) -> anyhow::Result<()> {
    let env = env()?;
    let conn = open(&env)?;
    let it = find(&conn, args)?;
    let events = store::events_of(&conn, &it.id)?;
    if args.flag("--json") {
        let mut v = item_json(&it);
        v["body"] = Value::String(it.body.clone());
        v["events"] = events
            .iter()
            .map(|e| serde_json::json!({ "ts_ns": e.ts_ns, "kind": e.kind, "detail": e.detail }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    print_item(&it);
    if !it.body.trim().is_empty() {
        println!("\n{}", it.body.trim());
    }
    if !events.is_empty() {
        println!();
        for e in &events {
            let when = time::OffsetDateTime::from_unix_timestamp_nanos(e.ts_ns as i128)
                .map(crate::loops::format_timestamp)
                .unwrap_or_default();
            let detail = match &e.detail {
                Value::Null => String::new(),
                Value::String(s) => format!("  {}", title_of(s, 90)),
                other => format!("  {}", title_of(&other.to_string(), 90)),
            };
            println!("  {when}  {}{detail}", e.kind);
        }
    }
    Ok(())
}

// ---- running --------------------------------------------------------------

/// The latest journaled answer of `step` (its main session).
fn answer_of(entries: &[journal::Entry], step: &str) -> Value {
    entries
        .iter()
        .rev()
        .find(|e| e.step == step && e.kind != "null" && !e.key.ends_with("/review"))
        .map(|e| e.result.clone())
        .unwrap_or(Value::Null)
}

fn set_state(conn: &rusqlite::Connection, item: &mut Item, state: State, detail: Value) {
    item.state = state;
    let _ = store::update(conn, item);
    let _ = store::add_event(conn, &item.id, &format!("state.{}", state.as_str()), detail);
}

/// Pushes the reviewed branch and opens its draft pull request. What is
/// done is recorded as it is done, so a retry after a failure repeats
/// only what is missing.
fn deliver_item(
    conn: &rusqlite::Connection,
    env: &Env,
    item: &mut Item,
    wt: &Worktree,
    analysis: &Value,
    build: &Value,
) -> Result<(), String> {
    let workspace = PathBuf::from(&item.workspace);
    if !deliver::has_origin(&workspace) {
        return Err(format!(
            "the repository has no origin remote; the reviewed work is on {} in {}",
            wt.branch,
            wt.path.display()
        ));
    }
    let default = deliver::default_branch(&workspace)?;
    let prefix = &env.settings.branch_prefix;
    let branch = match &item.branch {
        Some(b) => b.clone(),
        None => deliver::free_branch(
            &workspace,
            &deliver::branch_name(prefix, item.kind.as_deref(), &item.title),
        )?,
    };
    deliver::push(wt, prefix, &branch, &default)?;
    item.branch = Some(branch.clone());
    let _ = store::update(conn, item);
    let _ = store::add_event(conn, &item.id, "pushed", serde_json::json!(branch));
    if item.pr_url.is_none() {
        let url = deliver::open_pr(
            &env.tools,
            &workspace,
            &default,
            &branch,
            &item.title,
            &deliver::pr_body(item, analysis, build),
        )?;
        item.pr_number = deliver::pr_number(&url);
        item.pr_url = Some(url);
    }
    Ok(())
}

/// Applies a settlement to its item, delivering an approved branch.
fn apply(
    conn: &rusqlite::Connection,
    env: &Env,
    item: &mut Item,
    s: &Settlement,
    wt: Option<&Worktree>,
    analysis: &Value,
    build: &Value,
) {
    // a typed request is a paragraph: triage's summary names it better
    // (not while triage is still asking: its summary then describes the gap)
    if item.source == Source::Manual
        && !matches!(
            s.state,
            State::NeedsAnswer | State::Failed | State::Cancelled
        )
        && let Some(title) = s.summary.as_deref().and_then(|t| title_from_summary(t, 72))
    {
        item.title = title;
    }
    item.kind = s.kind.clone().or(item.kind.take());
    item.priority = s.priority.clone().or(item.priority.take());
    item.complexity = s.complexity.clone().or(item.complexity.take());
    item.cycles = s.cycles;
    item.error = s.error.clone();
    if let Some(wt) = wt {
        item.worktree = Some(wt.path.to_string_lossy().into_owned());
    }
    for note in &s.brain_notes {
        let _ = store::add_event(conn, &item.id, "brain.note", serde_json::json!(note));
    }
    if !s.deliver {
        let detail = match s.state {
            State::NeedsAnswer => serde_json::json!(s.questions),
            State::Answered => serde_json::json!(s.answer),
            _ => serde_json::json!(s.error),
        };
        set_state(conn, item, s.state, detail);
        return;
    }
    let delivered = match wt {
        Some(wt) => deliver_item(conn, env, item, wt, analysis, build),
        None => Err("the run left no worktree to deliver".into()),
    };
    match delivered {
        Ok(()) => {
            item.error = None;
            let pr = serde_json::json!(item.pr_url);
            set_state(conn, item, State::Delivered, pr);
        }
        Err(e) => {
            // `delivery:` marks a failure a retry repairs without a new run
            item.error = Some(format!("delivery: {e}"));
            let why = serde_json::json!(item.error);
            set_state(conn, item, State::Failed, why);
        }
    }
}

struct RunOpts<'a> {
    args: &'a Args,
}

/// Runs the line for `item` to its end and settles it.
async fn work(env: &Env, item: &mut Item, opts: &RunOpts<'_>) -> anyhow::Result<()> {
    let args = opts.args;
    let workspace = PathBuf::from(&item.workspace);
    let entries = crate::workflows::cli::entries();
    // failing checks run the short line, on the pull request's branch
    let is_ci = item.source == Source::Ci;
    let flow = if is_ci { ci::FLOW } else { FLOW };
    let entry = library::find(&entries, flow)
        .ok_or_else(|| anyhow::anyhow!("the {flow} workflow is missing from the library"))?;
    if !entry.valid() {
        anyhow::bail!("{flow}: {}", entry.problems.join("; "));
    }
    let harness = match args.value("--harness") {
        Some(h) => Harness::detect(h).ok_or_else(|| anyhow::anyhow!("unknown harness {h:?}"))?,
        None => Harness::Claude,
    };
    // nobody watches an unattended item: its writing stations run only
    // where agent-mux's guard reaches the session
    if !item.attended && !matches!(harness, Harness::Claude | Harness::Codex | Harness::Muse) {
        anyhow::bail!(
            "{} was started by the poll and runs unattended: its harness must be claude, codex or muse, where the guard refuses a push; {} is not",
            item.handle(),
            harness.as_str()
        );
    }
    // the work forks from the remote's default branch, not from whatever
    // the workspace happens to have checked out
    let worktree_base = if is_ci {
        let branch = item
            .branch
            .clone()
            .ok_or_else(|| anyhow::anyhow!("{} has no branch to fix", item.id))?;
        deliver::fetch(&workspace, &branch).map_err(|e| anyhow::anyhow!(e))?;
        Some(format!("origin/{branch}"))
    } else if deliver::has_origin(&workspace) {
        let default = deliver::default_branch(&workspace).map_err(|e| anyhow::anyhow!(e))?;
        deliver::fetch(&workspace, &default).map_err(|e| anyhow::anyhow!(e))?;
        Some(format!("origin/{default}"))
    } else {
        None
    };
    let mut step_overrides = Vec::new();
    for raw in args.all("--step") {
        let parsed = raw
            .split_once('=')
            .and_then(|(target, value)| target.rsplit_once('.').map(|(s, k)| (s, k, value)));
        let Some((step, key, value)) = parsed else {
            anyhow::bail!(
                "--step takes <step>.<harness|profile|model|effort|agent>=<value>, got {raw:?}"
            );
        };
        step_overrides.push((
            step.trim().to_string(),
            key.trim().to_string(),
            value.trim().to_string(),
        ));
    }
    let budget = match args.value("--budget") {
        Some(b) => Some(b.parse::<u64>()?),
        None => (env.settings.budget_tokens > 0).then_some(env.settings.budget_tokens),
    };
    let req = WorkflowRunRequest {
        name: entry.name.clone(),
        source: entry.source.label(),
        document: entry.text.clone(),
        workspace: workspace.clone(),
        profile: args.value("--profile").map(str::to_string),
        harness,
        args: {
            let mut a = serde_json::json!({
                "item": item.text(),
                "brain": brain::read(&crate::assets::root(), &workspace),
                "setup": env.settings.setup_command,
            });
            if is_ci {
                a["classification"] = serde_json::json!(ci::CLASSIFICATION);
            }
            a
        },
        budget_tokens: budget,
        usd_cap: None,
        isolation: None,
        resume_from: None,
        worktree_base,
        step_overrides,
    };
    let quiet = args.flag("--json");
    let factory_runtime = crate::tracing::analysis::default_snapshot_dir();
    let Some(_held) = intake::hold(&factory_runtime, &item.id) else {
        anyhow::bail!("{} is being worked by another process", item.id);
    };
    let mut h = crate::workflows::cli::headless()?;
    let run_id = h
        .app
        .start_workflow_run(req)
        .map_err(|e| anyhow::anyhow!(e))?;
    let runtime = h.app.workflows_runtime_dir();
    {
        let conn = open(env)?;
        item.run_id = Some(run_id.clone());
        item.error = None;
        set_state(&conn, item, State::Running, serde_json::json!(run_id));
    }
    if !quiet {
        eprintln!("{} · run {run_id} started", item.handle());
    }
    let timeout = Duration::from_secs(h.app.workflows.run_timeout_s + 60);
    let mut seen = 0;
    let target = comment_target(env, item);
    let mut progress = target
        .as_deref()
        .map(|n| comment::Progress::new(&env.tools, item, n));
    let journal_file = context::run_dir(&runtime, &run_id).join("journal.jsonl");
    let finished = crate::workflows::cli::pump(&mut h, timeout, |a| {
        if let Some(p) = progress.as_mut() {
            p.tick(&journal_file);
        }
        if !quiet && let Some(r) = a.live_workflow_runs.first() {
            for n in &r.state.notes[seen..] {
                eprintln!("  {n}");
            }
            seen = r.state.notes.len();
        }
        a.live_workflow_runs.is_empty()
    })
    .await;
    let recent = h.app.recent_workflow_runs.first().cloned();
    crate::workflows::cli::shutdown(h).await;

    let conn = open(env)?;
    let Some(recent) = recent.filter(|_| finished) else {
        item.error = Some("the run did not finish".into());
        set_state(&conn, item, State::Failed, serde_json::json!(item.error));
        anyhow::bail!("the run did not finish");
    };
    let run_dir = context::run_dir(&runtime, &run_id);
    let entries = journal::load(&run_dir.join("journal.jsonl"));
    let wt = crate::app::workflows::load_step_worktrees(&run_dir).remove("build");
    let analysis = answer_of(&entries, "analyze");
    let s = settle(
        &recent.status,
        recent.error.as_deref(),
        &classification_of(item, &entries),
        &recent.result,
        &answer_of(&entries, "answer"),
    );
    apply(&conn, env, item, &s, wt.as_ref(), &analysis, &recent.result);
    if let Some(mut p) = progress {
        p.tick(&journal_file);
        if let Some(url) = &p.url {
            let _ = store::add_event(&conn, &item.id, "comment.progress", serde_json::json!(url));
        }
        let sent = match (p.failed.take(), comment::outcome(item, &s, &recent.result)) {
            (Some(e), _) => Err(e),
            (None, Some(body)) => comment::post(
                &env.tools,
                &workspace,
                target.as_deref().unwrap_or_default(),
                &body,
            )
            .map(Some),
            (None, None) => Ok(None),
        };
        match sent {
            Ok(Some(url)) => {
                let _ = store::add_event(&conn, &item.id, "comment", serde_json::json!(url));
            }
            Ok(None) => {}
            Err(e) => {
                let _ = store::add_event(&conn, &item.id, "comment.failed", serde_json::json!(e));
                if !quiet {
                    eprintln!("  the issue was not commented: {e}");
                }
            }
        }
    }
    report(item, &s, &recent.result, quiet);
    Ok(())
}

/// The issue or pull request agent-mux comments on for this item: an
/// issue item's issue, a `ci` item's pull request.
fn comment_target(env: &Env, item: &Item) -> Option<String> {
    if !env.settings.comments || item.repo.is_empty() {
        return None;
    }
    match item.source {
        Source::Issue if !item.source_ref.is_empty() => Some(item.source_ref.clone()),
        Source::Ci => item.pr_number.map(|n| n.to_string()),
        _ => None,
    }
}

/// Triage's answer for the item's run. A `ci` item has no triage: failing
/// checks are work to build.
fn classification_of(item: &Item, entries: &[journal::Entry]) -> Value {
    if item.source == Source::Ci {
        return serde_json::from_str(ci::CLASSIFICATION).unwrap_or(Value::Null);
    }
    answer_of(entries, "classify")
}

/// What the operator reads when a run ends.
fn report(item: &Item, s: &Settlement, build: &Value, json: bool) {
    if json {
        let mut v = item_json(item);
        v["questions"] = serde_json::json!(s.questions);
        v["answer"] = serde_json::json!(s.answer);
        v["brain_notes"] = serde_json::json!(s.brain_notes);
        v["review"] = build.get("review").cloned().unwrap_or(Value::Null);
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return;
    }
    print_item(item);
    match item.state {
        State::NeedsAnswer => {
            println!("\nTriage needs an answer before anything is built:");
            for q in &s.questions {
                println!("  - {q}");
            }
            println!("\n  agent-mux factory answer {} \"<answer>\"", item.id);
        }
        State::Answered => {
            if let Some(a) = &s.answer {
                println!("\n{a}");
            }
        }
        State::Unresolved | State::Rejected => {
            let review = build.get("review").cloned().unwrap_or(Value::Null);
            if let Some(sum) = review.get("summary").and_then(Value::as_str) {
                println!("\n{sum}");
            }
            if let Some(found) = review.get("blocking_findings").and_then(Value::as_array) {
                for f in found.iter().filter_map(Value::as_str) {
                    println!("  - {f}");
                }
            }
            println!("\nNo pull request was opened. The work is in the worktree above.");
        }
        State::Delivered if item.source == Source::Ci => {
            println!("\nThe reviewed fix was pushed to the pull request's branch.");
        }
        State::Delivered => {
            println!("\nA draft pull request is open. Marking it ready and merging are yours.");
        }
        _ => {}
    }
    if !s.brain_notes.is_empty() {
        println!("\nSuggested for the brain (nothing was added):");
        for n in &s.brain_notes {
            println!("  agent-mux factory brain add {n:?}");
        }
    }
}

fn exit_for(state: State) -> i32 {
    match state {
        State::Delivered | State::Answered => 0,
        State::Cancelled => 2,
        State::NeedsAnswer | State::Unresolved | State::Rejected => 4,
        _ => 1,
    }
}

async fn start(args: &Args) -> anyhow::Result<()> {
    let env = env()?;
    let workspace = workspace_of(args)?;
    let repo = brain::github_repo(&workspace);
    let mut item = match args.value("--issue") {
        Some(n) => {
            let n = n.trim_start_matches('#');
            if repo.is_empty() {
                anyhow::bail!(
                    "--issue needs a GitHub origin remote in {}",
                    workspace.display()
                );
            }
            let issue =
                deliver::issue(&env.tools, &workspace, n).map_err(|e| anyhow::anyhow!(e))?;
            let mut it = Item::new(
                &workspace,
                &repo,
                Source::Issue,
                &issue.number,
                &issue.title,
            );
            it.body = issue.body;
            it.requester = issue.author;
            it
        }
        None => {
            let text = args.positional.join(" ");
            if text.trim().is_empty() {
                anyhow::bail!("say what is wanted, or name an issue with --issue <N>\n\n{USAGE}");
            }
            let mut it = Item::new(&workspace, &repo, Source::Manual, "", &title_of(&text, 72));
            it.body = text.trim().to_string();
            it
        }
    };
    {
        let conn = open(&env)?;
        if let store::Inserted::Claimed(existing) = store::insert(&conn, &item)? {
            let next = if existing.state.may_retry() {
                format!(
                    "run it again with `agent-mux factory retry {}`",
                    existing.id
                )
            } else if existing.state == State::NeedsAnswer {
                format!(
                    "answer it with `agent-mux factory answer {} \"<answer>\"`",
                    existing.id
                )
            } else {
                "an issue becomes an item once".to_string()
            };
            anyhow::bail!(
                "issue #{} is already work item {} ({}); see `agent-mux factory show {}`; {next}",
                existing.source_ref,
                existing.id,
                existing.state.as_str(),
                existing.id
            );
        }
        let _ = store::add_event(
            &conn,
            &item.id,
            "created",
            serde_json::json!(item.source.as_str()),
        );
    }
    work(&env, &mut item, &RunOpts { args }).await?;
    std::process::exit(exit_for(item.state))
}

async fn answer(args: &Args) -> anyhow::Result<()> {
    let env = env()?;
    let mut item = {
        let conn = open(&env)?;
        find(&conn, args)?
    };
    if item.state != State::NeedsAnswer {
        anyhow::bail!(
            "{} is {}, not waiting for an answer",
            item.id,
            item.state.as_str()
        );
    }
    let text = args.positional.get(1..).unwrap_or_default().join(" ");
    if text.trim().is_empty() {
        anyhow::bail!(
            "give the answer: agent-mux factory answer {} \"<answer>\"",
            item.id
        );
    }
    item.body = format!(
        "{}\n\nAnswer from the requester to triage's questions:\n{}",
        item.body.trim_end(),
        text.trim()
    );
    {
        let conn = open(&env)?;
        store::update(&conn, &mut item)?;
        let _ = store::add_event(&conn, &item.id, "answer", serde_json::json!(text.trim()));
    }
    work(&env, &mut item, &RunOpts { args }).await?;
    std::process::exit(exit_for(item.state))
}

async fn retry(args: &Args) -> anyhow::Result<()> {
    let env = env()?;
    let conn = open(&env)?;
    let mut item = find(&conn, args)?;
    let forced = args.flag("--force") && item.state == State::Running;
    if !item.state.may_retry() && !forced {
        anyhow::bail!(
            "{} is {}{}",
            item.id,
            item.state.as_str(),
            match item.state {
                State::Running =>
                    "; if the process that ran it is gone, `retry --force` starts it again",
                State::NeedsAnswer => "; answer it with `agent-mux factory answer`",
                _ => " and is not run again",
            }
        );
    }
    // a delivery that failed after an approved review: deliver, do not rebuild
    let failed_delivery = item.state == State::Failed
        && item
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("delivery:"));
    if failed_delivery && let Some(run_id) = item.run_id.clone() {
        let run_dir = context::run_dir(&crate::tracing::analysis::default_snapshot_dir(), &run_id);
        if let Some(wt) = crate::app::workflows::load_step_worktrees(&run_dir).remove("build") {
            let entries = journal::load(&run_dir.join("journal.jsonl"));
            let build: Value = std::fs::read_to_string(run_dir.join("result.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|v| v.get("result").cloned())
                .unwrap_or(Value::Null);
            let analysis = answer_of(&entries, "analyze");
            let s = settle(
                "finished",
                None,
                &classification_of(&item, &entries),
                &build,
                &Value::Null,
            );
            if s.deliver {
                apply(&conn, &env, &mut item, &s, Some(&wt), &analysis, &build);
                report(&item, &s, &build, args.flag("--json"));
                std::process::exit(exit_for(item.state));
            }
        }
    }
    // a fresh run: a branch and a pull request of an earlier one stay as
    // they are. A `ci` item keeps its own: they are what it fixes.
    if item.source != Source::Ci {
        item.branch = None;
        item.pr_number = None;
        item.pr_url = None;
    }
    item.worktree = None;
    let _ = store::add_event(&conn, &item.id, "retry", Value::Null);
    drop(conn);
    work(&env, &mut item, &RunOpts { args }).await?;
    std::process::exit(exit_for(item.state))
}

fn cancel(args: &Args) -> anyhow::Result<()> {
    let env = env()?;
    let conn = open(&env)?;
    let mut item = find(&conn, args)?;
    if matches!(
        item.state,
        State::Delivered | State::Answered | State::Cancelled
    ) {
        anyhow::bail!("{} is {} already", item.id, item.state.as_str());
    }
    // the process that runs it, when there is one, is told to stop first
    let runtime = crate::tracing::analysis::default_snapshot_dir();
    let stopped = intake::cancel(&conn, &runtime, &mut item);
    println!(
        "{} cancelled{}",
        item.id,
        if stopped {
            "; its process was told to stop"
        } else {
            ""
        }
    );
    Ok(())
}

// ---- unattended intake ------------------------------------------------------

/// Reads the labeled issues into items, then works the queued unattended
/// ones of this workspace one after another.
async fn poll(args: &Args) -> anyhow::Result<()> {
    let env = env()?;
    let workspace = workspace_of(args)?;
    let repo = brain::github_repo(&workspace);
    if repo.is_empty() {
        anyhow::bail!(
            "the poll needs a GitHub origin remote in {}",
            workspace.display()
        );
    }
    let dry = args.flag("--dry-run");
    let runtime = crate::tracing::analysis::default_snapshot_dir();
    let lock = format!("poll-{}", brain::key(&repo));
    // one poll per repository at a time: a second one would start the
    // same queued item
    let held = if dry {
        None
    } else {
        match intake::hold(&runtime, &lock) {
            Some(h) => Some(h),
            None => {
                println!("another poll of {repo} is running");
                return Ok(());
            }
        }
    };
    // by hand the labels are always read; started by the TUI (`--auto`),
    // only what the settings turn on
    let auto = args.flag("--auto");
    let labels = !auto || env.settings.intake_label;
    let checks = env.settings.ci_fix || args.flag("--ci");
    let label = &env.settings.label;
    if labels {
        let seen = {
            let conn = open(&env)?;
            intake::poll(&conn, &env.tools, &workspace, &repo, label, dry)
                .map_err(|e| anyhow::anyhow!(e))?
        };
        if seen.is_empty() {
            println!("no open issue of {repo} carries the label {label:?}");
        }
        for s in &seen {
            match s {
                intake::Seen::New(it) => println!(
                    "#{} {} {}",
                    it.source_ref,
                    if dry {
                        "would become an item:"
                    } else {
                        "queued:"
                    },
                    title_of(&it.title, 70)
                ),
                intake::Seen::Known { number, item } => println!("#{number} is already {item}"),
                intake::Seen::Refused {
                    number,
                    login,
                    role,
                } => println!(
                    "#{number} ignored: labeled by {} ({role}), who may not triage {repo}",
                    login.as_deref().unwrap_or("nobody the events name")
                ),
                intake::Seen::Error { number, why } => println!("#{number} skipped: {why}"),
            }
        }
    }
    if checks {
        let seen = {
            let conn = open(&env)?;
            ci::poll(&conn, &env.tools, &workspace, env.settings.ci_fix_max, dry)
                .map_err(|e| anyhow::anyhow!(e))?
        };
        if seen.is_empty() {
            println!("no open factory pull request of {repo} has failing checks");
        }
        for s in &seen {
            match s {
                ci::Seen::New(it) => println!(
                    "{} {}",
                    if dry { "would be queued:" } else { "queued:" },
                    title_of(&it.title, 90)
                ),
                ci::Seen::Known { pr, item } => {
                    println!("pull request #{pr}: this failure is already {item}")
                }
                ci::Seen::Skipped { pr, why } => println!("pull request #{pr}: {why}"),
                ci::Seen::Error { pr, why } => println!("pull request #{pr} skipped: {why}"),
            }
        }
    }
    if dry || args.flag("--no-run") {
        return Ok(());
    }
    let ws = workspace.to_string_lossy().into_owned();
    loop {
        let next = {
            let conn = open(&env)?;
            store::list(&conn, Some(State::Queued), Some(&ws), 200)?
                .into_iter()
                .filter(|it| !it.attended)
                .min_by_key(|it| it.created_ns)
        };
        let Some(mut item) = next else { break };
        if let Err(e) = work(&env, &mut item, &RunOpts { args }).await {
            eprintln!("{}: {e}", item.handle());
            // an item that could not start must not be picked again
            if item.state == State::Queued {
                let conn = open(&env)?;
                item.error = Some(e.to_string());
                let why = serde_json::json!(item.error);
                set_state(&conn, &mut item, State::Failed, why);
            }
        }
    }
    drop(held);
    Ok(())
}

// ---- evals ----------------------------------------------------------------

/// Runs the line's checks with real harnesses on a throwaway repository.
fn eval_cmd(args: &Args) -> anyhow::Result<()> {
    use super::eval::{self, Case};
    let mut cases = Vec::new();
    for name in args.all("--case") {
        cases.push(Case::parse(name).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown case {name:?}: {}",
                Case::ALL.map(Case::name).join(", ")
            )
        })?);
    }
    if cases.is_empty() {
        cases = Case::ALL.to_vec();
    }
    let json = args.flag("--json");
    let exe = std::env::current_exe()?;
    let temp = tempfile::Builder::new()
        .prefix("agent-mux-factory-eval-")
        .tempdir()?;
    // macOS: the temp dir is a symlink; git reports the real path
    let root = temp.path().canonicalize()?;
    let mut runs: Vec<_> = cases.iter().map(|c| c.run()).collect();
    runs.sort();
    runs.dedup();
    if !json {
        eprintln!(
            "{} case(s) over {} run(s) of the line with real harnesses, in parallel, in {}. This takes a few minutes and costs what {} small item(s) cost.",
            cases.len(),
            runs.len(),
            root.display(),
            runs.len()
        );
    }
    let verdicts = eval::run_cases(&exe, &root, &cases, args.value("--harness"));
    let failed = verdicts.iter().filter(|v| !v.pass).count();
    if json {
        let rows: Vec<Value> = verdicts
            .iter()
            .map(|v| {
                serde_json::json!({ "case": v.case.name(), "checks": v.case.what(),
                    "pass": v.pass, "detail": v.detail })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        for v in &verdicts {
            println!(
                "{} {:<11} {}",
                if v.pass { "PASS" } else { "FAIL" },
                v.case.name(),
                v.case.what()
            );
            println!("     {}", v.detail);
        }
        println!("\n{} of {} passed", verdicts.len() - failed, verdicts.len());
    }
    if args.flag("--keep") {
        let kept = temp.keep();
        eprintln!("kept: {}", kept.display());
    }
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

// ---- the brain ------------------------------------------------------------

fn brain_cmd(args: &Args) -> anyhow::Result<()> {
    let env = env()?;
    let workspace = workspace_of(args)?;
    let root = crate::assets::root();
    let file: PathBuf = brain::path(&root, &workspace);
    match args.positional.first().map(String::as_str) {
        Some("path") => println!("{}", file.display()),
        Some("show") | None => {
            let text = brain::read(&root, &workspace);
            if text.trim().is_empty() {
                println!(
                    "no brain yet for {} (agent-mux factory brain add \"<a fact about this repository>\")",
                    brain::repo_name(&workspace)
                );
            } else {
                print!("{text}");
            }
        }
        Some("add") => {
            let note = args.positional.get(1..).unwrap_or_default().join(" ");
            let path = brain::append(
                &root,
                &workspace,
                &note,
                &today(),
                env.settings.brain_max_chars,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("added to {}", path.display());
        }
        Some("edit") => edit_brain(&file, &workspace, env.settings.brain_max_chars)?,
        Some(other) => anyhow::bail!("unknown brain command {other:?}: show, path, add or edit"),
    }
    Ok(())
}

fn edit_brain(file: &Path, workspace: &Path, max_chars: usize) -> anyhow::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::write(
            dir.join("repo"),
            format!("{}\n", brain::repo_name(workspace)),
        );
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let mut words = editor.split_whitespace();
    let program = words.next().unwrap_or("vi");
    let status = std::process::Command::new(program)
        .args(words)
        .arg(file)
        .status()
        .map_err(|e| anyhow::anyhow!("{editor}: {e}"))?;
    if !status.success() {
        anyhow::bail!("{editor} exited with {status}");
    }
    let chars = std::fs::read_to_string(file)
        .map(|t| t.chars().count())
        .unwrap_or(0);
    if chars > max_chars {
        eprintln!(
            "warning: the brain is {chars} characters, past its bound of {max_chars}; every station reads it whole, and nothing more can be added until it is shorter"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Args {
        Args::parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn flags_values_and_words_are_told_apart() {
        let a = args(&[
            "fix",
            "the",
            "--issue",
            "12",
            "--step=build.model=x",
            "--step",
            "analyze.harness=codex",
            "--json",
            "thing",
        ]);
        assert_eq!(a.positional, vec!["fix", "the", "thing"]);
        assert_eq!(a.value("--issue"), Some("12"));
        assert_eq!(
            a.all("--step"),
            vec!["build.model=x", "analyze.harness=codex"]
        );
        assert!(a.flag("--json"));
        assert!(!a.flag("--force"));
    }

    #[test]
    fn a_review_is_not_taken_for_its_steps_answer() {
        let e = |key: &str, step: &str, kind: &str, v: Value| journal::Entry {
            key: key.into(),
            step: step.into(),
            phase: String::new(),
            kind: kind.into(),
            result: v,
            reason: None,
            launch_id: None,
            tokens: 0,
            cost_usd: 0.0,
            agent: None,
            agent_hash: None,
        };
        let entries = vec![
            e(
                "classify",
                "classify",
                "object",
                serde_json::json!({ "label": "build" }),
            ),
            e(
                "build",
                "build",
                "object",
                serde_json::json!({ "base": "main" }),
            ),
            e(
                "build/review",
                "build",
                "object",
                serde_json::json!({ "verdict": "approve" }),
            ),
            e("analyze", "analyze", "null", Value::Null),
        ];
        assert_eq!(answer_of(&entries, "classify")["label"], "build");
        assert_eq!(answer_of(&entries, "build")["base"], "main");
        assert!(answer_of(&entries, "analyze").is_null());
        assert!(answer_of(&entries, "missing").is_null());
    }

    #[test]
    fn the_exit_status_says_whether_someone_is_needed() {
        assert_eq!(exit_for(State::Delivered), 0);
        assert_eq!(exit_for(State::Answered), 0);
        assert_eq!(exit_for(State::NeedsAnswer), 4);
        assert_eq!(exit_for(State::Unresolved), 4);
        assert_eq!(exit_for(State::Failed), 1);
        assert_eq!(exit_for(State::Cancelled), 2);
    }
}
