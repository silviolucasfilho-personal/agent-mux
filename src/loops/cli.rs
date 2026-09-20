//! `agent-mux loop …`: the Loops section headless. Same registry, same
//! pre-flight and post-run code as the TUI (`loop run` drives a private
//! `App`), so cron and the sidebar never disagree.

use crate::config::Profile;
use crate::harness::Harness;
use crate::loops::registry::{self, LoopEntry, Registry};
use crate::loops::store as lstore;
use crate::loops::{Level, Outcome, format_interval, format_timestamp, format_tokens, patterns};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const USAGE: &str = "agent-mux loop <command>

  ls [--json]                              every registered loop with next run, spend and readiness
  add --workspace <dir> --pattern <id> [--profile <name>] [--harness claude|codex]
      [--every 1d] [--level L1] [--max-runs-per-day n] [--max-tokens-per-day n]
      [--max-cost <usd>] [--model <id>] [--verifier-model <id>] [--no-scaffold]
                                           register a loop (and scaffold its files)
  rm <id|prefix|pattern@workspace>         remove the registry entry (files stay)
  run <id|prefix|pattern@workspace> [--now]
                                           one scheduler pass for that loop, headless;
                                           exit 0 report-only/no-op, 3 fix-proposed,
                                           4 escalated, 1 blocked, 2 failed
  pause [<id>|--all], resume [<id>|--all]  pause or resume one loop or the kill switch
  init <dir> --pattern <id> --harness claude|codex [--level L1] [--verifier-model <id>]
                                           scaffold only
  audit <dir> [--json]                     the Loop Ready score of a workspace
  status [<id>] [--json]                   the preview card as text
  report <id> [--run <run_id>] [--json]    what a run found and who has to act
  runs <id> [--all] [--json]               the run timeline, quiet runs folded
  show <run_id> [--json]                   one run in full
  cost --pattern <id> [--every 15m] [--level L2] [--with-caching] [--json]
                                           token estimate per day
  inbox [--json]                           runs waiting on a decision
  decide <run_id> applied|rejected         the inbox decision

Patterns: daily-triage, pr-babysitter, ci-sweeper, post-merge-cleanup,
dependency-sweeper, changelog-drafter, issue-triage. The registry lives in
~/.agent-mux/loops.json (AGENT_MUX_LOOPS_FILE overrides). Antigravity is not
supported for loops yet (docs/loops.md).";

/// `--flag value` pairs, `--flag` switches and positionals.
struct Args {
    values: HashMap<String, String>,
    flags: Vec<String>,
    positional: Vec<String>,
}

const VALUE_FLAGS: &[&str] = &[
    "--workspace",
    "--pattern",
    "--profile",
    "--harness",
    "--every",
    "--level",
    "--max-runs-per-day",
    "--max-tokens-per-day",
    "--max-cost",
    "--run",
    "--model",
    "--verifier-model",
];

impl Args {
    fn parse(raw: &[String]) -> Args {
        let mut out = Args {
            values: HashMap::new(),
            flags: Vec::new(),
            positional: Vec::new(),
        };
        let mut i = 0;
        while i < raw.len() {
            let a = &raw[i];
            if let Some((k, v)) = a.strip_prefix("--").and_then(|s| s.split_once('=')) {
                out.values.insert(k.to_string(), v.to_string());
            } else if VALUE_FLAGS.contains(&a.as_str()) {
                i += 1;
                if let Some(v) = raw.get(i) {
                    out.values
                        .insert(a.trim_start_matches("--").to_string(), v.clone());
                }
            } else if let Some(f) = a.strip_prefix("--") {
                out.flags.push(f.to_string());
            } else {
                out.positional.push(a.clone());
            }
            i += 1;
        }
        out
    }

    fn value(&self, k: &str) -> Option<&str> {
        self.values.get(k).map(String::as_str)
    }

    fn has(&self, f: &str) -> bool {
        self.flags.iter().any(|x| x == f)
    }
}

fn registry_path() -> anyhow::Result<PathBuf> {
    registry::registry_path().ok_or_else(|| anyhow::anyhow!("no home directory for loops.json"))
}

fn load_registry() -> anyhow::Result<(PathBuf, Registry)> {
    let path = registry_path()?;
    Ok((path.clone(), registry::load(&path)))
}

fn save_registry(path: &Path, reg: &Registry) -> anyhow::Result<()> {
    registry::save(path, reg).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

fn db_path(cfg: &crate::config::Config) -> Option<PathBuf> {
    crate::config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())
        .map(|r| r.db_path)
}

fn open_ro(cfg: &crate::config::Config) -> Option<rusqlite::Connection> {
    let db = db_path(cfg)?;
    crate::tracing::store::open_ro(&db).ok()
}

fn resolve_entry<'a>(reg: &'a Registry, needle: &str) -> anyhow::Result<&'a LoopEntry> {
    reg.resolve(needle)
        .ok_or_else(|| anyhow::anyhow!("no loop matches {needle:?} (see `agent-mux loop ls`)"))
}

/// A path guard exists for the harness (spec section 9.5).
fn guard_available(harness: Harness, home: &Path) -> bool {
    let exe = crate::tracing::hooks::register::current_exe();
    match harness {
        Harness::Claude => exe.is_some(),
        Harness::Codex => {
            let st = crate::tracing::hooks::install::codex_status(home, exe.as_deref());
            st.installed && !st.stale
        }
        Harness::Antigravity => false,
    }
}

fn store_activity(conn: Option<&rusqlite::Connection>, workspace: &Path) -> usize {
    let Some(c) = conn else {
        return 0;
    };
    let since = crate::loops::to_ns(crate::loops::now() - time::Duration::days(14));
    lstore::activity_count(c, &workspace.to_string_lossy(), since).unwrap_or(0)
}

fn spend_today(conn: Option<&rusqlite::Connection>, loop_id: &str) -> lstore::Spend {
    let Some(c) = conn else {
        return lstore::Spend::default();
    };
    let since = crate::loops::to_ns(crate::loops::utc_midnight(crate::loops::now()));
    lstore::spend_since(c, loop_id, since).unwrap_or_default()
}

fn parse_harness(s: &str) -> anyhow::Result<Harness> {
    match s.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => Ok(Harness::Claude),
        "codex" => Ok(Harness::Codex),
        "agy" | "antigravity" => anyhow::bail!("Antigravity is not supported for loops"),
        other => anyhow::bail!("unknown harness {other:?}: claude or codex"),
    }
}

fn pattern_or_bail(id: &str) -> anyhow::Result<&'static crate::loops::Pattern> {
    patterns::find(id)
        .ok_or_else(|| anyhow::anyhow!("unknown pattern {id:?}: {}", patterns::ids().join(", ")))
}

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let cmd = args.first().map(String::as_str);
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match cmd {
        Some("ls") | Some("list") => ls(&Args::parse(rest)),
        Some("add") => add(&Args::parse(rest)),
        Some("rm") | Some("remove") => rm(&Args::parse(rest)),
        Some("run") => run_loop(&Args::parse(rest)).await,
        Some("pause") => pause(&Args::parse(rest), true),
        Some("resume") => pause(&Args::parse(rest), false),
        Some("init") => init(&Args::parse(rest)),
        Some("audit") => audit(&Args::parse(rest)),
        Some("status") => status(&Args::parse(rest)),
        Some("cost") => cost(&Args::parse(rest)),
        Some("report") => report(&Args::parse(rest)),
        Some("runs") => runs_cmd(&Args::parse(rest)),
        Some("show") => show(&Args::parse(rest)),
        Some("inbox") => inbox(&Args::parse(rest)),
        Some("decide") => decide(&Args::parse(rest)),
        Some("help") | Some("--help") | Some("-h") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown loop command: {other}\n\n{USAGE}"),
    }
}

fn entry_json(
    l: &LoopEntry,
    conn: Option<&rusqlite::Connection>,
    audit: Option<&crate::loops::readiness::Audit>,
) -> serde_json::Value {
    let spend = spend_today(conn, &l.id);
    serde_json::json!({
        "id": l.id,
        "pattern": l.pattern,
        "workspace": l.workspace,
        "profile": l.profile,
        "harness": l.harness,
        "model": l.model,
        "verifier_model": l.verifier_model,
        "every": format_interval(l.interval_s),
        "interval_s": l.interval_s,
        "level": l.level.as_str(),
        "enabled": l.enabled,
        "paused": l.paused(),
        "paused_reason": l.paused_reason,
        "next_run_at": l.next_run_at,
        "last_run_id": l.last_run_id,
        "max_runs_per_day": l.max_runs_per_day,
        "max_tokens_per_day": l.max_tokens_per_day,
        "max_cost_usd_per_run": l.max_cost_usd_per_run,
        "today": { "runs": spend.runs, "tokens": spend.tokens, "cost_usd": spend.cost_usd },
        "readiness": audit.map(|a| serde_json::json!({ "score": a.score, "level": a.level_str() })),
    })
}

fn ls(args: &Args) -> anyhow::Result<()> {
    let (_, reg) = load_registry()?;
    let cfg = crate::config::load()?;
    let conn = open_ro(&cfg);
    let audits: Vec<Option<crate::loops::readiness::Audit>> = reg
        .loops
        .iter()
        .map(|l| {
            l.workspace.is_dir().then(|| {
                crate::loops::readiness::audit(
                    &l.workspace,
                    store_activity(conn.as_ref(), &l.workspace),
                )
            })
        })
        .collect();
    if args.has("json") {
        let rows: Vec<serde_json::Value> = reg
            .loops
            .iter()
            .zip(audits.iter())
            .map(|(l, a)| entry_json(l, conn.as_ref(), a.as_ref()))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "pause_all": reg.pause_all,
                "loops": rows,
            }))?
        );
        return Ok(());
    }
    if reg.loops.is_empty() {
        println!("no loops registered — `agent-mux loop add --workspace <dir> --pattern <id>`");
        return Ok(());
    }
    if reg.pause_all {
        println!("LOOPS PAUSED (kill switch) — `agent-mux loop resume --all`");
    }
    println!(
        "id       pattern              workspace          harness  every lvl state      next run             today        readiness"
    );
    for (l, a) in reg.loops.iter().zip(audits.iter()) {
        let spend = spend_today(conn.as_ref(), &l.id);
        let state = if l.paused() {
            match &l.paused_reason {
                Some(r) => format!("paused: {r}"),
                None => "paused".into(),
            }
        } else {
            "scheduled".into()
        };
        println!(
            "{:<8} {:<20} {:<18} {:<8} {:<5} {:<3} {:<10} {:<20} {:<12} {}",
            &l.id[..8.min(l.id.len())],
            trunc(&l.pattern, 20),
            trunc(&l.workspace_name(), 18),
            l.harness,
            format_interval(l.interval_s),
            l.level.as_str(),
            trunc(&state, 10),
            l.next_run_at.as_deref().unwrap_or("-"),
            format!(
                "{}/{} {}",
                spend.runs,
                l.max_runs_per_day,
                format_tokens(spend.tokens.max(0) as u64)
            ),
            a.as_ref()
                .map(|a| format!("{}/100 {}", a.score, a.level_str()))
                .unwrap_or_else(|| "workspace missing".into()),
        );
    }
    Ok(())
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let keep: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{keep}…")
    } else {
        s.to_string()
    }
}

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        return crate::skill::install::home_dir().join(rest);
    }
    PathBuf::from(p)
}

fn add(args: &Args) -> anyhow::Result<()> {
    let workspace = expand_home(
        args.value("workspace")
            .ok_or_else(|| anyhow::anyhow!("--workspace <dir> is required"))?,
    );
    let workspace = workspace
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("workspace {}: {e}", workspace.display()))?;
    if !workspace.is_dir() {
        anyhow::bail!("workspace {}: not a directory", workspace.display());
    }
    let pattern = pattern_or_bail(
        args.value("pattern")
            .ok_or_else(|| anyhow::anyhow!("--pattern <id> is required"))?,
    )?;
    let cfg = crate::config::load()?;
    let (profile, harness) =
        pick_profile(&cfg.profiles, args.value("profile"), args.value("harness"))?;
    let interval_s = match args.value("every") {
        Some(e) => crate::loops::parse_interval(e)
            .ok_or_else(|| anyhow::anyhow!("--every: use <n>m, <n>h or <n>d, at least 5m"))?,
        None => pattern.default_interval_s,
    };
    let level = match args.value("level") {
        Some(l) => Level::parse(l).ok_or_else(|| anyhow::anyhow!("--level: L1, L2 or L3"))?,
        None => Level::L1,
    };
    let max_runs: u32 = match args.value("max-runs-per-day") {
        Some(v) => v
            .parse()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| anyhow::anyhow!("--max-runs-per-day: a positive number"))?,
        None => pattern.max_runs_per_day,
    };
    let max_tokens: u64 = match args.value("max-tokens-per-day") {
        Some(v) => v
            .replace(['_', ','], "")
            .parse()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| anyhow::anyhow!("--max-tokens-per-day: a positive number"))?,
        None => pattern.max_tokens_per_day,
    };
    let max_cost = match args.value("max-cost") {
        Some(v) => Some(
            v.parse::<f64>()
                .ok()
                .filter(|c| c.is_finite() && *c > 0.0)
                .ok_or_else(|| anyhow::anyhow!("--max-cost: a positive number"))?,
        ),
        None => None,
    };
    let home = crate::skill::install::home_dir();
    let ceiling = if guard_available(harness, &home) {
        Level::L3
    } else {
        Level::L1
    };
    if level > ceiling {
        anyhow::bail!(
            "level {} needs a path guard; {} has none here (ceiling {}){}",
            level.as_str(),
            harness.as_str(),
            ceiling.as_str(),
            if harness == Harness::Codex {
                " — run `agent-mux trace hooks install codex`"
            } else {
                ""
            }
        );
    }
    if level > Level::L1 && !crate::loops::worktree::is_git_repo(&workspace) {
        anyhow::bail!(
            "level {} needs a git repository (worktrees)",
            level.as_str()
        );
    }
    // The models: what was asked for, else what the pattern suggests.
    let model = args
        .value("model")
        .map(str::to_string)
        .or_else(|| pattern.model.clone())
        .unwrap_or_default();
    let verifier_model = args
        .value("verifier-model")
        .map(str::to_string)
        .or_else(|| pattern.verifier_model.clone())
        .unwrap_or_default();
    let conn = open_ro(&cfg);
    if !args.has("no-scaffold") {
        let caps = crate::loops::scaffold::Caps {
            max_runs_per_day: max_runs,
            max_tokens_per_day: max_tokens,
            verifier_model: verifier_model.clone(),
        };
        let report = crate::loops::scaffold::scaffold(&workspace, pattern, harness, level, &caps)?;
        println!(
            "scaffolded {} file(s) in {}, {} kept",
            report.written.len(),
            workspace.display(),
            report.skipped.len()
        );
    }
    let audit =
        crate::loops::readiness::audit(&workspace, store_activity(conn.as_ref(), &workspace));
    if let Err(why) = audit.allows(level)
        && level > Level::L1
    {
        anyhow::bail!("level {}: {why}", level.as_str());
    }
    let (path, mut reg) = load_registry()?;
    let entry = registry::new_entry(
        &workspace,
        pattern,
        harness.as_str(),
        &profile,
        interval_s,
        level,
        crate::loops::now(),
    );
    let mut entry = entry;
    entry.max_runs_per_day = max_runs;
    entry.max_tokens_per_day = max_tokens;
    entry.max_cost_usd_per_run = max_cost;
    entry.model = model.clone();
    entry.verifier_model = verifier_model.clone();
    let id = entry.id.clone();
    let next = entry.next_run_at.clone().unwrap_or_default();
    reg.add(entry);
    save_registry(&path, &reg)?;
    println!(
        "{id}: {} every {} at {} on {}{} · next run {next}\nreadiness {}/100 {} — {}",
        pattern.id,
        format_interval(interval_s),
        level.as_str(),
        harness.as_str(),
        match (model.is_empty(), verifier_model.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!(" · model {model}"),
            (true, false) => format!(" · verifier {verifier_model}"),
            (false, false) => format!(" · model {model} · verifier {verifier_model}"),
        },
        audit.score,
        audit.level_str(),
        audit.assessment
    );
    Ok(())
}

/// `(profile name, harness)` for `--profile` / `--harness`: a named
/// profile, else the first profile of the harness (default claude), else a
/// bare command.
fn pick_profile(
    profiles: &[Profile],
    name: Option<&str>,
    harness: Option<&str>,
) -> anyhow::Result<(String, Harness)> {
    if let Some(n) = name {
        let p = profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(n))
            .ok_or_else(|| anyhow::anyhow!("no profile named {n:?}"))?;
        let h = Harness::detect(&p.command)
            .ok_or_else(|| anyhow::anyhow!("profile {n:?} runs {}, not a harness", p.command))?;
        if h == Harness::Antigravity {
            anyhow::bail!("Antigravity is not supported for loops");
        }
        if let Some(want) = harness
            && parse_harness(want)? != h
        {
            anyhow::bail!("profile {n:?} runs {}, not {want}", h.as_str());
        }
        return Ok((p.name.clone(), h));
    }
    let h = match harness {
        Some(s) => parse_harness(s)?,
        None => Harness::Claude,
    };
    let name = profiles
        .iter()
        .find(|p| Harness::detect(&p.command) == Some(h))
        .map(|p| p.name.clone())
        .unwrap_or_default();
    Ok((name, h))
}

fn rm(args: &Args) -> anyhow::Result<()> {
    let needle = args
        .positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: agent-mux loop rm <id|prefix|pattern@workspace>"))?;
    let (path, mut reg) = load_registry()?;
    let id = resolve_entry(&reg, needle)?.id.clone();
    let gone = reg.remove(&id).unwrap();
    save_registry(&path, &reg)?;
    println!(
        "removed {} ({} in {}); the files in the workspace stay",
        gone.id,
        gone.pattern,
        gone.workspace.display()
    );
    Ok(())
}

fn pause(args: &Args, pause: bool) -> anyhow::Result<()> {
    let (path, mut reg) = load_registry()?;
    if args.has("all") {
        reg.pause_all = pause;
        save_registry(&path, &reg)?;
        println!(
            "{}",
            if pause {
                "LOOPS PAUSED — every loop stays idle until `agent-mux loop resume --all`"
            } else {
                "loops resumed"
            }
        );
        return Ok(());
    }
    let needle = args.positional.first().ok_or_else(|| {
        anyhow::anyhow!(
            "usage: agent-mux loop {} <id>|--all",
            if pause { "pause" } else { "resume" }
        )
    })?;
    let entry = resolve_entry(&reg, needle)?.clone();
    if pause {
        reg.pause(&entry.id);
        println!("{} paused", entry.pattern);
    } else {
        let reason = reg.resume(&entry.id).flatten();
        if reason
            .as_deref()
            .is_some_and(|r| r.starts_with("circuit breaker"))
        {
            let ledger_path = entry.workspace.join(crate::loops::LEDGER_JSON);
            if let Ok(mut ledger) = crate::loops::breaker::load(&ledger_path) {
                crate::loops::breaker::human_reset(&mut ledger);
                let _ = crate::loops::breaker::save(&ledger_path, &ledger);
            }
        }
        println!(
            "{} resumed{}",
            entry.pattern,
            reason.map(|r| format!(" (was: {r})")).unwrap_or_default()
        );
    }
    save_registry(&path, &reg)?;
    Ok(())
}

fn init(args: &Args) -> anyhow::Result<()> {
    let dir = expand_home(args.positional.first().ok_or_else(|| {
        anyhow::anyhow!("usage: agent-mux loop init <dir> --pattern <id> --harness claude|codex")
    })?);
    if !dir.is_dir() {
        anyhow::bail!("{}: not a directory", dir.display());
    }
    let pattern = pattern_or_bail(
        args.value("pattern")
            .ok_or_else(|| anyhow::anyhow!("--pattern <id> is required"))?,
    )?;
    let harness = parse_harness(
        args.value("harness")
            .ok_or_else(|| anyhow::anyhow!("--harness claude|codex is required"))?,
    )?;
    let level = match args.value("level") {
        Some(l) => Level::parse(l).ok_or_else(|| anyhow::anyhow!("--level: L1, L2 or L3"))?,
        None => Level::L1,
    };
    let caps = crate::loops::scaffold::Caps {
        max_runs_per_day: pattern.max_runs_per_day,
        max_tokens_per_day: pattern.max_tokens_per_day,
        verifier_model: args
            .value("verifier-model")
            .map(str::to_string)
            .or_else(|| pattern.verifier_model.clone())
            .unwrap_or_default(),
    };
    let report = crate::loops::scaffold::scaffold(&dir, pattern, harness, level, &caps)?;
    for p in &report.written {
        println!("written  {}", p.display());
    }
    for p in &report.skipped {
        println!("skipped  {} (already exists)", p.display());
    }
    let audit = crate::loops::readiness::audit(&dir, 0);
    println!(
        "\nreadiness {}/100 {} — {}",
        audit.score,
        audit.level_str(),
        audit.assessment
    );
    Ok(())
}

fn audit(args: &Args) -> anyhow::Result<()> {
    let dir = args
        .positional
        .first()
        .map(|d| expand_home(d))
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| anyhow::anyhow!("usage: agent-mux loop audit <dir> [--json]"))?;
    if !dir.is_dir() {
        anyhow::bail!("{}: not a directory", dir.display());
    }
    let cfg = crate::config::load()?;
    let conn = open_ro(&cfg);
    let audit = crate::loops::readiness::audit(&dir, store_activity(conn.as_ref(), &dir));
    if args.has("json") {
        println!("{}", serde_json::to_string_pretty(&audit.to_json())?);
    } else {
        for l in audit.human_lines() {
            println!("{l}");
        }
    }
    Ok(())
}

fn status_card(
    l: &LoopEntry,
    conn: Option<&rusqlite::Connection>,
    inbox: &[lstore::LoopRun],
) -> (Vec<String>, serde_json::Value) {
    let pattern = patterns::find(&l.pattern);
    let spend = spend_today(conn, &l.id);
    let percent = if l.max_tokens_per_day == 0 {
        0
    } else {
        (spend.tokens.max(0) as u128 * 100 / l.max_tokens_per_day as u128) as u32
    };
    let recent = conn
        .and_then(|c| lstore::recent_runs(c, &l.id, 5).ok())
        .unwrap_or_default();
    let breaker = pattern.filter(|p| p.breaker).map(|_| {
        match crate::loops::breaker::load(&l.workspace.join(crate::loops::LEDGER_JSON)) {
            Ok(ledger) => {
                let v = crate::loops::breaker::check(&ledger, &Default::default());
                if v.tripped() {
                    format!("TRIPPED: {}", v.reason)
                } else {
                    format!("ok · {} attempts", v.iterations)
                }
            }
            Err(_) => "no ledger yet".to_string(),
        }
    });
    let audit = l
        .workspace
        .is_dir()
        .then(|| crate::loops::readiness::audit(&l.workspace, store_activity(conn, &l.workspace)));
    let files = pattern
        .map(|p| crate::loops::scaffold::contract_files(&l.workspace, p))
        .unwrap_or_default();
    let waiting = inbox.iter().filter(|r| r.loop_id == l.id).count();
    let mut lines = Vec::new();
    lines.push(format!(
        "{} · {} · {}{}",
        l.pattern,
        l.workspace.display(),
        l.harness,
        if l.profile.is_empty() {
            String::new()
        } else {
            format!(" ({})", l.profile)
        }
    ));
    lines.push(format!(
        "  status     {} · next run {} · every {} · level {} ({})",
        if l.paused() {
            format!(
                "paused{}",
                l.paused_reason
                    .as_deref()
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            )
        } else {
            "scheduled".into()
        },
        l.next_run_at.as_deref().unwrap_or("-"),
        format_interval(l.interval_s),
        l.level.as_str(),
        l.level.label()
    ));
    // Which model does the work, and which one checks it.
    if !l.model.is_empty() || !l.verifier_model.is_empty() {
        lines.push(format!(
            "  models     run {}{}",
            if l.model.is_empty() {
                "the profile's".to_string()
            } else {
                l.model.clone()
            },
            if l.verifier_model.is_empty() {
                String::new()
            } else {
                format!(" · verifier {}", l.verifier_model)
            }
        ));
    }
    match recent.first() {
        Some(r) => lines.push(format!(
            "  last run   {} · {} · {} found · {} action · {} escalated · {} tokens · ${:.2}{}",
            r.id,
            r.outcome.as_str(),
            r.items_found.unwrap_or(0),
            r.actions_taken.unwrap_or(0),
            r.escalations.unwrap_or(0),
            format_tokens(r.tokens.unwrap_or(0).max(0) as u64),
            r.cost_usd.unwrap_or(0.0),
            r.detail_str("reason")
                .map(|x| format!(" · {x}"))
                .unwrap_or_default()
        )),
        None => lines.push("  last run   none yet".into()),
    }
    lines.push(format!(
        "  budget     today {}/{} runs · {}/{} tokens ({percent} %) · {}",
        spend.runs,
        l.max_runs_per_day,
        format_tokens(spend.tokens.max(0) as u64),
        format_tokens(l.max_tokens_per_day),
        if percent >= 100 {
            "blocked"
        } else if percent >= 80 {
            "report-only"
        } else {
            "normal"
        }
    ));
    lines.push(format!(
        "  breaker    {}",
        breaker
            .clone()
            .unwrap_or_else(|| "n/a (report-only pattern)".into())
    ));
    match &audit {
        Some(a) => lines.push(format!(
            "  readiness  {}  {}/100 {}",
            a.score_bar(),
            a.score,
            a.level_str()
        )),
        None => lines.push("  readiness  workspace not found".into()),
    }
    lines.push(format!("  inbox      {waiting} waiting"));
    let file_words: Vec<String> = files
        .iter()
        .map(|f| {
            format!(
                "{} {}",
                f.name,
                if !f.present {
                    "✗"
                } else if f.stale {
                    "! (stale)"
                } else {
                    "✓"
                }
            )
        })
        .collect();
    lines.push(format!("  files      {}", file_words.join("  ")));
    let json = serde_json::json!({
        "id": l.id,
        "pattern": l.pattern,
        "workspace": l.workspace,
        "harness": l.harness,
        "profile": l.profile,
        "every": format_interval(l.interval_s),
        "level": l.level.as_str(),
        "paused": l.paused(),
        "paused_reason": l.paused_reason,
        "next_run_at": l.next_run_at,
        "last_run": recent.first().map(|r| serde_json::json!({
            "id": r.id, "outcome": r.outcome.as_str(), "items_found": r.items_found,
            "actions_taken": r.actions_taken, "escalations": r.escalations,
            "tokens": r.tokens, "cost_usd": r.cost_usd, "detail": r.detail,
        })),
        "budget": { "runs_today": spend.runs, "max_runs_per_day": l.max_runs_per_day,
                    "tokens_today": spend.tokens, "max_tokens_per_day": l.max_tokens_per_day,
                    "percent": percent },
        "breaker": breaker,
        "readiness": audit.as_ref().map(|a| serde_json::json!({ "score": a.score, "level": a.level_str() })),
        "inbox_waiting": waiting,
        "files": files.iter().map(|f| serde_json::json!({ "name": f.name, "present": f.present, "stale": f.stale })).collect::<Vec<_>>(),
    });
    (lines, json)
}

fn status(args: &Args) -> anyhow::Result<()> {
    let (_, reg) = load_registry()?;
    let cfg = crate::config::load()?;
    let conn = open_ro(&cfg);
    let inbox = conn
        .as_ref()
        .and_then(|c| lstore::inbox(c).ok())
        .unwrap_or_default();
    let selected: Vec<&LoopEntry> = match args.positional.first() {
        Some(n) => vec![resolve_entry(&reg, n)?],
        None => reg.loops.iter().collect(),
    };
    if selected.is_empty() {
        println!("no loops registered");
        return Ok(());
    }
    let mut cards = Vec::new();
    for l in &selected {
        cards.push(status_card(l, conn.as_ref(), &inbox));
    }
    if args.has("json") {
        let v: Vec<serde_json::Value> = cards.into_iter().map(|(_, j)| j).collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if reg.pause_all {
        println!("LOOPS PAUSED (kill switch)\n");
    }
    for (i, (lines, _)) in cards.iter().enumerate() {
        if i > 0 {
            println!();
        }
        for l in lines {
            println!("{l}");
        }
    }
    Ok(())
}

fn cost(args: &Args) -> anyhow::Result<()> {
    let pattern = pattern_or_bail(
        args.value("pattern")
            .ok_or_else(|| anyhow::anyhow!("--pattern <id> is required"))?,
    )?;
    let interval_s = match args.value("every") {
        Some(e) => crate::loops::parse_interval(e)
            .ok_or_else(|| anyhow::anyhow!("--every: use <n>m, <n>h or <n>d, at least 5m"))?,
        None => pattern.default_interval_s,
    };
    let level = match args.value("level") {
        Some(l) => Level::parse(l).ok_or_else(|| anyhow::anyhow!("--level: L1, L2 or L3"))?,
        None => Level::L1,
    };
    let est = crate::loops::cost::estimate(pattern, interval_s, level, args.has("with-caching"));
    if args.has("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "pattern_id": est.pattern_id,
                "pattern_name": pattern.name,
                "level": est.level.as_str(),
                "interval_s": est.interval_s,
                "runs_per_day": est.runs_per_day,
                "multiplier": est.multiplier,
                "noop_per_day": est.noop_per_day,
                "report_per_day": est.report_per_day,
                "action_per_day": est.action_per_day,
                "realistic_per_run": est.realistic_per_run,
                "realistic_per_day": est.realistic_per_day,
                "assumptions": est.assumptions,
                "cached_per_run": est.cached_per_run,
                "cached_per_day": est.cached_per_day,
                "savings_percent": est.savings_percent,
                "suggested_daily_cap": est.suggested_daily_cap,
                "warnings": est.warnings,
            }))?
        );
        return Ok(());
    }
    for l in crate::loops::cost::human_lines(&est, &pattern.name) {
        println!("{l}");
    }
    Ok(())
}

fn inbox(args: &Args) -> anyhow::Result<()> {
    let cfg = crate::config::load()?;
    // no store yet (nothing ever ran) is an empty inbox, not an error
    let rows = match open_ro(&cfg) {
        Some(conn) => lstore::inbox(&conn)?,
        None => Vec::new(),
    };
    if args.has("json") {
        let v: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "run_id": r.id, "loop_id": r.loop_id, "pattern": r.pattern,
                    "workspace": r.workspace, "outcome": r.outcome.as_str(),
                    "branch": r.branch, "worktree": r.worktree, "detail": r.detail,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("nothing waiting on a decision");
        return Ok(());
    }
    for r in &rows {
        println!(
            "{} · {} · {} · {}{}",
            r.id,
            r.pattern,
            r.workspace,
            r.outcome.as_str(),
            r.branch
                .as_deref()
                .map(|b| format!(" · branch {b}"))
                .unwrap_or_default()
        );
        if let Some(s) = r.detail_str("summary") {
            println!("    {s}");
        }
        if let Some(stat) = r.detail_str("diff_stat") {
            for l in stat.lines().take(10) {
                println!("    {l}");
            }
        }
    }
    println!("\n`agent-mux loop decide <run_id> applied|rejected`");
    Ok(())
}

fn decide(args: &Args) -> anyhow::Result<()> {
    let (run_id, verdict) = match (args.positional.first(), args.positional.get(1)) {
        (Some(r), Some(v)) => (r.clone(), v.clone()),
        _ => anyhow::bail!("usage: agent-mux loop decide <run_id> applied|rejected"),
    };
    let applied = match verdict.as_str() {
        "applied" | "apply" | "accept" => true,
        "rejected" | "reject" => false,
        other => anyhow::bail!("decision {other:?}: applied or rejected"),
    };
    let cfg = crate::config::load()?;
    let (tx, _rx) = tokio::sync::mpsc::channel(32);
    let mut app = crate::app::App::new(cfg.profiles.clone(), None, tx);
    app.clipboard_enabled = false;
    app.trace_db_path = db_path(&cfg);
    app.loops = crate::config::resolve_loops(cfg.loops.as_ref());
    app.decide_loop_run(&run_id, applied)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(n) = &app.notice {
        println!("{}", n.text);
    }
    Ok(())
}

async fn run_loop(args: &Args) -> anyhow::Result<()> {
    use crate::events::AppEvent;
    let needle = args.positional.first().ok_or_else(|| {
        anyhow::anyhow!("usage: agent-mux loop run <id|prefix|pattern@workspace> [--now]")
    })?;
    let (_, reg) = load_registry()?;
    let entry = resolve_entry(&reg, needle)?.clone();
    let now = crate::loops::now();
    if !args.has("now") {
        match entry.next_run() {
            Some(t) if t > now => {
                println!(
                    "{} not due until {} (--now runs it anyway)",
                    entry.pattern,
                    format_timestamp(t)
                );
                return Ok(());
            }
            _ => {}
        }
    }
    let cfg = crate::config::load()?;
    let resolved = crate::config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())
        .ok_or_else(|| anyhow::anyhow!("tracing is disabled: a loop run needs the trace store"))?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
    let runtime = crate::tracing::TraceRuntime::new(resolved.clone(), tx.clone())
        .map_err(|e| anyhow::anyhow!("trace store: {e}"))?;
    let mut app = crate::app::App::new(cfg.profiles.clone(), Some(runtime), tx);
    app.clipboard_enabled = false;
    app.loops = crate::config::resolve_loops(cfg.loops.as_ref());
    app.loops.enabled = false; // only the loop asked for, never the others
    app.load_loop_registry();
    if args.has("now")
        && let Some(l) = app.loop_registry.find_mut(&entry.id)
    {
        l.set_next_run(now);
    }
    let run_id = match app.start_loop_run(&entry.id) {
        Some(id) => id,
        None => {
            let why = app
                .notice
                .as_ref()
                .map(|n| n.text.clone())
                .unwrap_or_else(|| "pre-flight blocked the run".into());
            println!("{why}");
            let _ = app.save_loop_registry();
            if let Some(rt) = app.take_tracing() {
                rt.shutdown(Duration::from_secs(5)).await;
            }
            std::process::exit(1);
        }
    };
    println!("{} started · run {run_id}", entry.pattern);
    let deadline = Instant::now() + Duration::from_secs(app.loops.run_timeout_s + 30);
    let mut last_tick = Instant::now();
    while !app.live_loop_runs.is_empty() {
        if Instant::now() > deadline {
            println!("gave up waiting for the run to finish");
            break;
        }
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(AppEvent::PtyOutput { id, bytes })) => {
                app.handle_pty_output(id, &bytes, Instant::now())
            }
            Ok(Some(AppEvent::PtyExit { id })) => app.handle_pty_exit(id),
            Ok(Some(AppEvent::TraceStats { launch_id, stats })) => {
                app.handle_trace_stats(&launch_id, stats)
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
        if last_tick.elapsed() >= Duration::from_millis(250) {
            last_tick = Instant::now();
            app.on_tick(Instant::now());
        }
    }
    app.kill_all();
    if let Some(rt) = app.take_tracing() {
        rt.shutdown(Duration::from_secs(5)).await;
    }
    let conn = crate::tracing::store::open_ro(&resolved.db_path).map_err(|e| anyhow::anyhow!(e))?;
    let Some(row) = lstore::get_run(&conn, &run_id)? else {
        println!("run {run_id}: no row in the store");
        std::process::exit(2);
    };
    println!(
        "run {} · {} · {} tokens · ${:.2} · {}s{}",
        row.id,
        row.outcome.as_str(),
        format_tokens(row.tokens.unwrap_or(0).max(0) as u64),
        row.cost_usd.unwrap_or(0.0),
        row.duration_s().unwrap_or(0),
        row.branch
            .as_deref()
            .map(|b| format!(" · branch {b}"))
            .unwrap_or_default()
    );
    for key in ["reason", "level_reason", "summary", "gate_violation"] {
        if let Some(v) = row.detail_str(key) {
            println!("  {key:<14} {v}");
        }
    }
    let code = match row.outcome {
        Outcome::ReportOnly | Outcome::NoOp => 0,
        Outcome::FixProposed => 3,
        Outcome::Escalated => 4,
        Outcome::Blocked => 1,
        Outcome::Failed => 2,
    };
    std::process::exit(code);
}

/// `agent-mux loop report <id>`: the report of one run — what it found,
/// who has to act, what moved since the run before it. The same reading
/// as the Loops view's Report tab.
fn report(args: &Args) -> anyhow::Result<()> {
    let (_, registry) = load_registry()?;
    let id = args
        .positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: agent-mux loop report <id> [--run <run_id>]"))?;
    let entry = resolve_entry(&registry, id)?;
    let cfg = crate::config::load()?;
    let conn = open_ro(&cfg);
    let runs = conn
        .as_ref()
        .and_then(|c| lstore::recent_runs(c, &entry.id, 50).ok())
        .unwrap_or_default();
    let run = match args.value("run") {
        Some(want) => runs.iter().find(|r| r.id.starts_with(want)),
        None => runs.first(),
    };
    let runtime = crate::tracing::analysis::default_snapshot_dir();
    let (text, from_snapshot) =
        match run.and_then(|r| crate::loops::state::read_snapshot(&runtime, &entry.id, &r.id)) {
            Some(t) => (Some(t), true),
            None => {
                let newest = runs.first().map(|r| r.id.as_str());
                let is_newest = run.is_none() || run.map(|r| r.id.as_str()) == newest;
                let p = patterns::find(&entry.pattern);
                let file = p.and_then(|p| {
                    is_newest
                        .then(|| std::fs::read_to_string(entry.workspace.join(&p.state_file)).ok())
                        .flatten()
                });
                (file, false)
            }
        };
    if let Some(r) = run {
        for l in run_lines(r) {
            println!("{l}");
        }
    } else {
        println!("{} · no runs yet", entry.pattern);
    }
    let Some(text) = text else {
        println!("  no report kept for this run");
        return Ok(());
    };
    let report = crate::loops::state::parse(&text);
    if args.has("json") {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    if report.is_empty() {
        println!("  the state file does not follow the loop shape");
        return Ok(());
    }
    use crate::loops::state::SectionKind;
    for kind in [
        SectionKind::NeedsYou,
        SectionKind::Watching,
        SectionKind::Ignored,
        SectionKind::Other,
    ] {
        let Some(section) = report.of_kind(kind) else {
            continue;
        };
        let items = report.items_of(kind);
        if items.is_empty() && section.notes.is_empty() {
            continue;
        }
        println!();
        println!("{} ({})", kind.label(), items.len());
        for item in &items {
            println!("  {}", item.headline());
            if let Some(s) = &item.status {
                println!("    {s}");
            }
            if kind != SectionKind::NeedsYou {
                continue;
            }
            if let Some(d) = &item.human_decision {
                println!("    Decide    {d}");
            }
            if let Some(a) = &item.loop_action {
                println!("    Loop did  {a}");
            }
        }
        for n in &section.notes {
            println!("    note  {n}");
        }
    }
    if let Some(m) = run.and_then(|r| r.detail_str("final_message")) {
        println!();
        println!("What the run said");
        for l in m.lines() {
            println!("  {l}");
        }
    }
    println!();
    println!(
        "{} · last run {}",
        if from_snapshot {
            "the report this run wrote"
        } else {
            "the workspace's state file"
        },
        report.last_run.unwrap_or_default()
    );
    Ok(())
}

/// `agent-mux loop runs <id>`: the timeline, newest first, quiet runs folded.
fn runs_cmd(args: &Args) -> anyhow::Result<()> {
    let (_, registry) = load_registry()?;
    let id = args
        .positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: agent-mux loop runs <id> [--all] [--json]"))?;
    let entry = resolve_entry(&registry, id)?;
    let cfg = crate::config::load()?;
    let Some(conn) = open_ro(&cfg) else {
        anyhow::bail!("tracing is off — no runs recorded");
    };
    let runs = lstore::recent_runs(&conn, &entry.id, 100)?;
    if args.has("json") {
        let v: Vec<serde_json::Value> = runs
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id, "outcome": r.outcome.as_str(), "word": r.outcome.word(),
                    "level": r.effective_level.as_str(), "items_found": r.items_found,
                    "actions_taken": r.actions_taken, "escalations": r.escalations,
                    "tokens": r.tokens, "cost_usd": r.cost_usd, "duration_s": r.duration_s(),
                    "detail": r.detail,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    if runs.is_empty() {
        println!("no runs yet — agent-mux loop run {}", &entry.id[..8]);
        return Ok(());
    }
    let all = args.has("all");
    let mut i = 0usize;
    while i < runs.len() {
        let key = crate::loops::run_fold_key(&runs[i]);
        if let Some(k) = key.filter(|_| !all) {
            let mut j = i + 1;
            while j < runs.len() && crate::loops::run_fold_key(&runs[j]).as_deref() == Some(&k) {
                j += 1;
            }
            if j - i >= 2 {
                println!(
                    "{} … {}   {k} ×{}",
                    when_of(&runs[j - 1]),
                    when_of(&runs[i]),
                    j - i
                );
                i = j;
                continue;
            }
        }
        for l in run_lines(&runs[i]) {
            println!("{l}");
        }
        i += 1;
    }
    println!();
    println!(
        "agent-mux loop report {} --run <run_id> reads one run",
        &entry.id[..8]
    );
    Ok(())
}

/// `agent-mux loop show <run_id>`: one run in full.
fn show(args: &Args) -> anyhow::Result<()> {
    let id = args
        .positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: agent-mux loop show <run_id>"))?;
    let cfg = crate::config::load()?;
    let Some(conn) = open_ro(&cfg) else {
        anyhow::bail!("tracing is off — no runs recorded");
    };
    let runs = lstore::all_recent_runs(&conn, 500)?;
    let run = runs
        .iter()
        .find(|r| r.id.starts_with(id))
        .ok_or_else(|| anyhow::anyhow!("no run {id:?}"))?;
    if args.has("json") {
        println!("{}", serde_json::to_string_pretty(&run.detail)?);
        return Ok(());
    }
    for l in run_lines(run) {
        println!("{l}");
    }
    println!("  pattern   {} · {}", run.pattern, run.workspace);
    println!(
        "  level     {} (configured {})",
        run.effective_level.as_str(),
        run.level.as_str()
    );
    if let Some(v) = run.detail_str("level_reason") {
        println!("  capped    {v}");
    }
    if let Some(v) = run.detail_str("gate_violation") {
        println!("  gate      VIOLATION {v}");
    }
    let files: Vec<String> = run
        .detail
        .get("files")
        .and_then(|f| f.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|f| f.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    println!(
        "  touched   {}",
        if files.is_empty() {
            "no files".to_string()
        } else {
            format!("{} · {}", files.len(), files.join(", "))
        }
    );
    if let Some(b) = &run.branch {
        println!("  branch    {b}  (merge it yourself; agent-mux never merges)");
    }
    if let Some(w) = &run.worktree {
        println!("  worktree  {w}");
    }
    if let Some(l) = &run.launch_id {
        println!("  launch    {l}");
    }
    if let Some(m) = run.detail_str("final_message") {
        println!("  said");
        for l in m.lines() {
            println!("    {l}");
        }
    }
    if let Some(stat) = run.detail_str("diff_stat") {
        println!("  diff --stat");
        for l in stat.lines() {
            println!("    {l}");
        }
    }
    Ok(())
}

fn when_of(r: &lstore::LoopRun) -> String {
    crate::workflows::report::format_when(r.started_ns.unwrap_or(r.scheduled_ns))
}

/// The two lines every surface leads a run with.
fn run_lines(r: &lstore::LoopRun) -> Vec<String> {
    let facts = match r.outcome {
        crate::loops::Outcome::Blocked => r
            .detail_str("reason")
            .unwrap_or("no reason recorded")
            .to_string(),
        _ => {
            let mut f: Vec<String> = Vec::new();
            if let Some(n) = r.items_found {
                f.push(format!("{n} found"));
            }
            if let Some(n) = r.escalations.filter(|n| *n > 0) {
                f.push(format!("{n} for you"));
            }
            if let Some(n) = r.actions_taken.filter(|n| *n > 0) {
                f.push(format!("{n} action"));
            }
            if let Some(t) = r.tokens {
                f.push(format!(
                    "{} tokens",
                    crate::loops::format_tokens(t.max(0) as u64)
                ));
            }
            if r.tokens.is_some_and(|t| t > 0) {
                f.push(crate::workflows::report::Cost::of(r.cost_usd, &r.harness).text());
            }
            if let Some(d) = r.duration_s() {
                f.push(crate::workflows::report::format_duration(d));
            }
            f.join(" · ")
        }
    };
    let mut out = vec![format!(
        "{}   {}   {}   {facts}",
        when_of(r),
        r.outcome.word().to_uppercase(),
        r.effective_level.as_str()
    )];
    if let Some(s) = r.detail_str("summary") {
        out.push(format!("  {s}"));
    } else if let Some(d) = r
        .detail
        .get("delta")
        .and_then(|v| serde_json::from_value::<crate::loops::state::Delta>(v.clone()).ok())
    {
        out.push(format!("  {}", d.summary()));
    }
    out
}
