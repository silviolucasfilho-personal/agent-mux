//! `agent-mux trace …` — one-shot subcommands over the store. Hand-rolled
//! argument parsing (the repo has no clap). Every read opens the store
//! read-only so a CLI never takes the writer's lock.

use crate::config::{self, ContentMode, ResolvedTracing};
use crate::tracing::map::{self, MapSettings, TurnAssembler};
use crate::tracing::store::query::{self, SessionFilter};
use crate::tracing::store::{self, Store};
use crate::tracing::{ids, price_table};
use crate::transcript::{self, Provider};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: agent-mux trace <command> [options]

commands:
  doctor                       store health, schema, unpriced models, provider readiness
  path                         print the resolved database path
  ls [--all] [--project DIR] [--since 7d] [--limit N] [--json]
                               sessions with turns, tokens, cost
  show <session|trace> [--full] [--json] [--tree | --timeline]
                               turns of a session, or observations of a trace
                               (--tree: nesting; --timeline: bars over the turn)
  search <fts5 query> [--limit N] [--json]
                               full-text search over content (full mode data)
  import <path>... | --discover [--provider claude|codex|antigravity] [--content-mode full|metadata]
                               backfill transcripts (idempotent)
  export [--session ID] [--since 30d] [--out FILE]
                               JSON Lines dump of every table
  export --langfuse [--session ID] [--since 30d] [--dry-run]
                               replay stored sessions to Langfuse ([tracing.langfuse] / $LANGFUSE_*)
  prune [--older-than 90d] [--vacuum] [--dry-run]
                               delete old rows; --vacuum reclaims space
  recost                       recompute cost from usage and the current price table
  sql <select ...> [--json]    read-only query
  loops [session] [--json]     per-turn loop metrics: calls, retries, where the time went, context
  skills [--json]              what each skill did across the store (loaded vs. attributed)
  agents [--json]              what each subagent type did: invocations, latency, cost, failures
  compare <a> <b>              two turns or sessions side by side: loop metrics and the tool path
  experiments [name]           the experiment registry, or one experiment's variants
  (see also: agent-mux run --experiment <name> --variant <label> --prompt <text> …)
  hook <claude|codex|codex-notify|agy> [--event E] [--home DIR] [--launch ID] [--content-mode M] [--db PATH] [payload]
                               hook entry point invoked by the CLIs (reads the payload from stdin or the last argument)
  hooks install|uninstall|status [codex|agy]
                               the opt-in hook installers (Claude and Codex notify need none: they register per launch)";

struct Args {
    positional: Vec<String>,
    flags: std::collections::HashMap<String, Option<String>>,
}

impl Args {
    fn parse(raw: &[String]) -> Args {
        let mut positional = Vec::new();
        let mut flags = std::collections::HashMap::new();
        let mut i = 0;
        while i < raw.len() {
            let a = &raw[i];
            if let Some(name) = a.strip_prefix("--") {
                if let Some((k, v)) = name.split_once('=') {
                    flags.insert(k.to_string(), Some(v.to_string()));
                } else if matches!(
                    name,
                    "all"
                        | "json"
                        | "full"
                        | "discover"
                        | "vacuum"
                        | "dry-run"
                        | "langfuse"
                        | "tree"
                        | "timeline"
                ) {
                    flags.insert(name.to_string(), None);
                } else {
                    let value = raw.get(i + 1).cloned();
                    if value.is_some() {
                        i += 1;
                    }
                    flags.insert(name.to_string(), value);
                }
            } else {
                positional.push(a.clone());
            }
            i += 1;
        }
        Args { positional, flags }
    }

    fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.flags.get(name).and_then(|v| v.as_deref())
    }
}

/// Parses durations like `7d`, `12h`, `30m`, `90` (days).
pub fn parse_duration_days(s: &str) -> Option<f64> {
    let s = s.trim();
    let (num, unit) = match s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit() && *c != '.')
    {
        Some((i, _)) => (&s[..i], &s[i..]),
        None => (s, "d"),
    };
    let n: f64 = num.parse().ok()?;
    match unit.trim() {
        "d" | "day" | "days" => Some(n),
        "h" | "hour" | "hours" => Some(n / 24.0),
        "m" | "min" | "minutes" => Some(n / 1440.0),
        "w" | "week" | "weeks" => Some(n * 7.0),
        _ => None,
    }
}

fn since_ns(args: &Args, key: &str) -> anyhow::Result<Option<i64>> {
    match args.value(key) {
        None => Ok(None),
        Some(s) => {
            let days = parse_duration_days(s)
                .ok_or_else(|| anyhow::anyhow!("bad duration {s:?} (try 7d, 12h, 2w)"))?;
            Ok(Some(store::now_ns() - (days * 86_400.0 * 1e9) as i64))
        }
    }
}

pub fn fmt_time(ns: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ns)) {
        Ok(t) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        Err(_) => ns.to_string(),
    }
}

pub fn fmt_tokens(n: Option<i64>) -> String {
    match n {
        None => "-".into(),
        Some(n) if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        Some(n) if n >= 10_000 => format!("{:.0}k", n as f64 / 1e3),
        Some(n) if n >= 1_000 => format!("{:.1}k", n as f64 / 1e3),
        Some(n) => n.to_string(),
    }
}

pub fn fmt_cost(c: Option<f64>) -> String {
    match c {
        None => "-".into(),
        Some(c) if c < 0.01 && c > 0.0 => format!("${c:.4}"),
        Some(c) => format!("${c:.2}"),
    }
}

pub fn fmt_ms(ms: i64) -> String {
    if ms >= 60_000 {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    } else if ms >= 1_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

fn resolved() -> anyhow::Result<(config::Config, ResolvedTracing)> {
    let cfg = config::load()?;
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())
        .or_else(|| {
            // even with tracing disabled, the CLI can still read the store
            let mut enabled = cfg.tracing.clone().unwrap_or_default();
            enabled.enabled = Some(true);
            config::resolve_tracing(Some(&enabled), &|k| std::env::var(k).ok())
        })
        .ok_or_else(|| anyhow::anyhow!("could not resolve tracing configuration"))?;
    Ok((cfg, resolved))
}

fn open_ro(resolved: &ResolvedTracing) -> anyhow::Result<Connection> {
    // a store last written by an older binary lacks the newer views; bring
    // it up first so every read-only command sees the same schema
    if let Ok(true) = store::migrate_in_place(&resolved.db_path) {
        eprintln!(
            "trace store migrated to schema v{}",
            store::schema::SCHEMA_VERSION
        );
    }
    store::open_ro(&resolved.db_path).map_err(|e| anyhow::anyhow!("{e}"))
}

fn open_rw(resolved: &ResolvedTracing) -> anyhow::Result<Store> {
    store::open_rw(
        &resolved.db_path,
        store::OpenOptions {
            prices: price_table(resolved),
            run_id: uuid::Uuid::new_v4().to_string(),
            retention_days: 0,
            agent_mux_version: env!("CARGO_PKG_VERSION").to_string(),
        },
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

pub fn run(raw: &[String]) -> anyhow::Result<()> {
    let Some(command) = raw.first().map(String::as_str) else {
        println!("{USAGE}");
        return Ok(());
    };
    let args = Args::parse(&raw[1..]);
    match command {
        "doctor" => doctor(),
        "path" => {
            let (_, resolved) = resolved()?;
            println!("{}", resolved.db_path.display());
            Ok(())
        }
        "ls" | "list" => ls(&args),
        "show" => show(&args),
        "search" => search(&args),
        "import" => import(&args),
        "export" => export(&args),
        "prune" => prune(&args),
        "recost" => recost(),
        "sql" => sql(&args),
        "loops" => loops(&args),
        "skills" => skills(&args),
        "agents" => agents(&args),
        "compare" => compare(&args),
        "experiments" => experiments(&args),
        "hooks" => hooks_cmd(&args),
        "hook" => {
            let outcome = hook_run(&raw[1..], None);
            if let Some(response) = &outcome.response {
                println!("{response}");
            }
            if std::env::var_os("AGENT_MUX_HOOK_DEBUG").is_some() {
                eprintln!(
                    "agent-mux hook: inserted={} {}",
                    outcome.inserted,
                    outcome.error.as_deref().unwrap_or("")
                );
            }
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => {
            eprintln!("unknown trace command: {other}\n\n{USAGE}");
            Ok(())
        }
    }
}

fn check(label: &str, ok: bool, detail: &str) {
    let mark = if ok { "ok " } else { "!! " };
    println!("  [{mark}] {label}: {detail}");
}

fn on_path(command: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    #[cfg(unix)]
    fn runnable(p: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    fn runnable(_p: &Path) -> bool {
        true
    }
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(command);
        if candidate.is_file() && runnable(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{command}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

fn dir_status(path: Option<&Path>) -> (bool, String) {
    match path {
        Some(p) if p.is_dir() => (true, p.display().to_string()),
        Some(p) => (false, format!("{} (missing)", p.display())),
        None => (false, "unresolvable (no HOME?)".to_string()),
    }
}

/// `trace loops [session]`: one row per turn with its loop metrics.
fn loops(args: &Args) -> anyhow::Result<()> {
    use super::loops::loop_metrics;
    let (_, resolved) = resolved()?;
    let conn = open_ro(&resolved)?;
    let json = args.has("json");
    let session = match args.positional.first() {
        Some(needle) => query::find_session(&conn, needle)?
            .ok_or_else(|| anyhow::anyhow!("no session matches {needle:?}"))?,
        None => query::list_sessions(&conn, &SessionFilter::default())?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no sessions in the store"))?,
    };
    let turns = query::list_traces(&conn, &session.key)?;
    let mut rows = Vec::new();
    for t in &turns {
        let obs = query::list_observations(&conn, &t.id)?;
        rows.push((t, loop_metrics(t, &obs)));
    }
    if json {
        let v: Vec<serde_json::Value> = rows
            .iter()
            .map(|(t, m)| {
                serde_json::json!({
                    "trace_id": t.id, "ordinal": t.ordinal, "name": t.name,
                    "tool_calls": m.tool_calls, "distinct_tools": m.distinct_tools,
                    "retries": m.retries, "retried_tools": m.retried_tools,
                    "tool_errors": m.tool_errors, "declined": m.declined,
                    "model_ms": m.model_ms, "tool_ms": m.tool_ms, "idle_ms": m.idle_ms,
                    "context_first": m.context_first, "context_last": m.context_last,
                    "cache_ratio": m.cache_ratio, "compactions": m.compactions,
                    "subagents": m.subagents, "subagent_tokens": m.subagent_tokens,
                    "subagent_cost_usd": m.subagent_cost, "total_cost_usd": t.total_cost_usd,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(v));
        return Ok(());
    }
    println!("session {}  {} turn(s)", session.session_id, turns.len());
    println!(
        "{:>4} {:>5} {:>4} {:>3} {:>4} {:>8} {:>8} {:>8} {:>15} {:>5} {:>4} {:>8}  name",
        "turn",
        "calls",
        "rtry",
        "err",
        "decl",
        "model",
        "tools",
        "idle",
        "context",
        "cache",
        "sub",
        "cost"
    );
    for (t, m) in &rows {
        let context = match (m.context_first, m.context_last) {
            (Some(a), Some(b)) => format!("{}→{}", fmt_tokens(Some(a)), fmt_tokens(Some(b))),
            _ => "-".to_string(),
        };
        let name = t
            .name
            .split_once(": ")
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_else(|| t.name.clone());
        println!(
            "{:>4} {:>5} {:>4} {:>3} {:>4} {:>8} {:>8} {:>8} {:>15} {:>5} {:>4} {:>8}  {}",
            t.ordinal,
            m.tool_calls,
            m.retries,
            m.tool_errors,
            m.declined,
            fmt_ms(m.model_ms),
            fmt_ms(m.tool_ms),
            fmt_ms(m.idle_ms),
            context,
            m.cache_ratio
                .map(|r| format!("{:.0}%", r * 100.0))
                .unwrap_or_else(|| "-".into()),
            m.subagents,
            fmt_cost(t.total_cost_usd),
            truncate_display(&name, 40)
        );
    }
    Ok(())
}

/// `trace skills`: what each skill did across the store.
fn skills(args: &Args) -> anyhow::Result<()> {
    let (_, resolved) = resolved()?;
    let conn = open_ro(&resolved)?;
    let stats = query::skill_stats(&conn)?;
    if args.has("json") {
        let v: Vec<serde_json::Value> = stats
            .iter()
            .map(|s| {
                serde_json::json!({
                    "skill": s.skill, "turns_loaded": s.turns_loaded, "turns_unused": s.turns_unused,
                    "generations": s.generations, "tools": s.tools, "tokens": s.tokens,
                    "cost_usd": s.cost, "first_ns": s.first_ns, "last_ns": s.last_ns,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(v));
        return Ok(());
    }
    if stats.is_empty() {
        println!("no skills recorded — skill loads are attributed from Claude transcripts");
        return Ok(());
    }
    println!(
        "{:<32} {:>6} {:>6} {:>5} {:>5} {:>8} {:>9}  last used",
        "skill", "loaded", "unused", "gens", "tools", "tokens", "cost"
    );
    for s in &stats {
        println!(
            "{:<32} {:>6} {:>6} {:>5} {:>5} {:>8} {:>9}  {}",
            truncate_display(&s.skill, 32),
            s.turns_loaded,
            s.turns_unused,
            s.generations,
            s.tools,
            fmt_tokens(s.tokens),
            fmt_cost(s.cost),
            fmt_time(s.last_ns)
        );
    }
    Ok(())
}

/// `trace agents`: what each subagent type did.
fn agents(args: &Args) -> anyhow::Result<()> {
    let (_, resolved) = resolved()?;
    let conn = open_ro(&resolved)?;
    let stats = query::agent_stats(&conn)?;
    if args.has("json") {
        let v: Vec<serde_json::Value> = stats
            .iter()
            .map(|a| {
                serde_json::json!({
                    "agent_type": a.agent_type, "invocations": a.invocations, "mean_ms": a.mean_ms,
                    "p90_ms": a.p90_ms, "max_ms": a.max_ms, "tokens": a.tokens,
                    "cost_usd": a.cost, "failures": a.failures,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(v));
        return Ok(());
    }
    if stats.is_empty() {
        println!("no subagent invocations recorded");
        return Ok(());
    }
    println!(
        "{:<24} {:>5} {:>8} {:>8} {:>8} {:>8} {:>9} {:>5}",
        "agent", "runs", "mean", "p90", "max", "tokens", "cost", "fail"
    );
    for a in &stats {
        println!(
            "{:<24} {:>5} {:>8} {:>8} {:>8} {:>8} {:>9} {:>5}",
            truncate_display(&a.agent_type, 24),
            a.invocations,
            fmt_ms(a.mean_ms as i64),
            fmt_ms(a.p90_ms),
            fmt_ms(a.max_ms),
            fmt_tokens(Some(a.tokens).filter(|t| *t > 0)),
            fmt_cost(Some(a.cost).filter(|c| *c > 0.0)),
            a.failures
        );
    }
    Ok(())
}

/// `trace compare <a> <b>`: two turns or sessions side by side.
fn compare(args: &Args) -> anyhow::Result<()> {
    use super::experiments::{diff, resolve_side};
    let (Some(a), Some(b)) = (args.positional.first(), args.positional.get(1)) else {
        anyhow::bail!("usage: agent-mux trace compare <turn|session> <turn|session>");
    };
    let (_, resolved) = resolved()?;
    let conn = open_ro(&resolved)?;
    let left = resolve_side(&conn, a)?.ok_or_else(|| anyhow::anyhow!("nothing matches {a:?}"))?;
    let right = resolve_side(&conn, b)?.ok_or_else(|| anyhow::anyhow!("nothing matches {b:?}"))?;
    println!(
        "{:<16} {:>22} {:>22} {:>10}",
        "", left.label, right.label, "delta"
    );
    let row = |name: &str, l: String, r: String, d: String| {
        println!("{name:<16} {l:>22} {r:>22} {d:>10}");
    };
    let signed = |d: i64| {
        if d > 0 {
            format!("+{d}")
        } else {
            d.to_string()
        }
    };
    let (lm, rm) = (&left.metrics, &right.metrics);
    row(
        "turns",
        left.turns.to_string(),
        right.turns.to_string(),
        signed(right.turns - left.turns),
    );
    row(
        "tool calls",
        lm.tool_calls.to_string(),
        rm.tool_calls.to_string(),
        signed(rm.tool_calls - lm.tool_calls),
    );
    row(
        "distinct tools",
        lm.distinct_tools.to_string(),
        rm.distinct_tools.to_string(),
        signed(rm.distinct_tools - lm.distinct_tools),
    );
    row(
        "retries",
        lm.retries.to_string(),
        rm.retries.to_string(),
        signed(rm.retries - lm.retries),
    );
    row(
        "errors",
        lm.tool_errors.to_string(),
        rm.tool_errors.to_string(),
        signed(rm.tool_errors - lm.tool_errors),
    );
    row(
        "declined",
        lm.declined.to_string(),
        rm.declined.to_string(),
        signed(rm.declined - lm.declined),
    );
    row(
        "model time",
        fmt_ms(lm.model_ms),
        fmt_ms(rm.model_ms),
        signed(rm.model_ms - lm.model_ms) + "ms",
    );
    row(
        "tool time",
        fmt_ms(lm.tool_ms),
        fmt_ms(rm.tool_ms),
        signed(rm.tool_ms - lm.tool_ms) + "ms",
    );
    row(
        "idle time",
        fmt_ms(lm.idle_ms),
        fmt_ms(rm.idle_ms),
        signed(rm.idle_ms - lm.idle_ms) + "ms",
    );
    let ctx = |m: &super::loops::LoopMetrics| match (m.context_first, m.context_last) {
        (Some(a), Some(b)) => format!("{}→{}", fmt_tokens(Some(a)), fmt_tokens(Some(b))),
        _ => "-".to_string(),
    };
    row("context", ctx(lm), ctx(rm), String::new());
    row(
        "subagents",
        lm.subagents.to_string(),
        rm.subagents.to_string(),
        signed(rm.subagents - lm.subagents),
    );
    row(
        "tokens",
        fmt_tokens(left.tokens),
        fmt_tokens(right.tokens),
        match (left.tokens, right.tokens) {
            (Some(a), Some(b)) => signed(b - a),
            _ => String::new(),
        },
    );
    row(
        "cost",
        fmt_cost(left.cost),
        fmt_cost(right.cost),
        match (left.cost, right.cost) {
            (Some(a), Some(b)) => format!("{}{:.4}", if b >= a { "+" } else { "" }, b - a),
            _ => String::new(),
        },
    );
    println!(
        "\ntool sequence ('-' only in {}, '+' only in {}):",
        left.label, right.label
    );
    for (mark, name) in diff(&left.tools, &right.tools) {
        println!("  {mark} {name}");
    }
    Ok(())
}

/// `trace experiments [name]`: the registry, or one experiment's variants.
fn experiments(args: &Args) -> anyhow::Result<()> {
    use super::experiments::{experiment_summary, list_experiments, summary_lines};
    let (_, resolved) = resolved()?;
    let conn = open_ro(&resolved)?;
    if let Some(name) = args.positional.first() {
        let rows = experiment_summary(&conn, name)?;
        if rows.is_empty() {
            anyhow::bail!("no runs recorded for experiment {name:?}");
        }
        for line in summary_lines(&rows) {
            println!("{line}");
        }
        return Ok(());
    }
    let list = list_experiments(&conn)?;
    if list.is_empty() {
        println!(
            "no experiments yet — `agent-mux run --experiment <name> --variant <label> --prompt …`"
        );
        return Ok(());
    }
    println!(
        "{:<24} {:>5} {:>8}  {:<19}  prompt",
        "experiment", "runs", "variants", "created"
    );
    for e in &list {
        println!(
            "{:<24} {:>5} {:>8}  {}  {}",
            truncate_display(&e.name, 24),
            e.runs,
            e.variants,
            fmt_time(e.created_ns),
            if e.prompt.is_empty() {
                "(interactive)".to_string()
            } else {
                truncate_display(&e.prompt.replace('\n', " "), 60)
            }
        );
    }
    Ok(())
}

/// `trace hooks install|uninstall|status [codex|agy]`.
fn hooks_cmd(args: &Args) -> anyhow::Result<()> {
    use super::hooks::install;
    let action = args
        .positional
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    let provider = args.positional.get(1).map(String::as_str);
    let (_, resolved) = resolved()?;
    let home = resolved.home.clone();
    let exe = super::hooks::register::current_exe();
    let providers: Vec<&str> = match provider {
        Some(p @ ("codex" | "agy")) => vec![p],
        Some("antigravity") => vec!["agy"],
        Some(other) => anyhow::bail!("unknown provider {other:?}: expected codex or agy"),
        None if action == "status" => vec!["codex", "agy"],
        None => anyhow::bail!("usage: agent-mux trace hooks {action} <codex|agy>"),
    };
    for p in providers {
        match action {
            "install" => {
                let Some(exe) = exe.as_deref() else {
                    anyhow::bail!("cannot resolve the path of this binary");
                };
                let report = match p {
                    "codex" => install::install_codex(exe, &home),
                    _ => install::install_agy(exe, &home),
                }
                .map_err(|e| anyhow::anyhow!(e))?;
                println!(
                    "{p}: {} {}",
                    if report.changed {
                        "installed at"
                    } else {
                        "already current at"
                    },
                    report.path.display()
                );
                println!("  {}", report.note);
            }
            "uninstall" => {
                let report = match p {
                    "codex" => install::uninstall_codex(&home),
                    _ => install::uninstall_agy(&home),
                }
                .map_err(|e| anyhow::anyhow!(e))?;
                println!("{p}: {} ({})", report.note, report.path.display());
            }
            "status" => print_hook_status(&hook_status(p, &home, exe.as_deref())),
            other => {
                anyhow::bail!("unknown action {other:?}: expected install, uninstall or status")
            }
        }
    }
    if action == "status" {
        println!(
            "claude: per-launch --settings registration (hooks = {}); codex notify: per-launch -c override",
            resolved.hooks.as_str()
        );
    }
    Ok(())
}

fn hook_status(provider: &str, home: &Path, exe: Option<&Path>) -> super::hooks::install::Status {
    match provider {
        "codex" => super::hooks::install::codex_status(home, exe),
        _ => super::hooks::install::agy_status(home, exe),
    }
}

fn print_hook_status(status: &super::hooks::install::Status) {
    if !status.installed {
        // opt-in: absent is a state, not a failure
        println!("  [-- ] {} hooks: {}", status.provider, status.note);
        return;
    }
    check(
        &format!("{} hooks", status.provider),
        !status.stale,
        &format!("{} — {}", status.path.display(), status.note),
    );
}

/// Hook rows per provider in the last 24 h, with the age of the newest.
fn hook_activity(conn: &rusqlite::Connection) -> Vec<(String, i64, i64)> {
    let since = now_ns() - 24 * 3600 * 1_000_000_000;
    let Ok(mut stmt) = conn.prepare(
        "SELECT provider, count(*), max(ts_ns) FROM hook_events WHERE ts_ns >= ?1 GROUP BY provider ORDER BY provider",
    ) else {
        return Vec::new();
    };
    stmt.query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn fmt_age(ns: i64) -> String {
    let secs = (now_ns() - ns).max(0) / 1_000_000_000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}

/// `trace export --langfuse`: replays stored sessions through the
/// Langfuse sink.
fn export_langfuse(
    conn: &Connection,
    resolved: &ResolvedTracing,
    session_key: Option<String>,
    since: Option<i64>,
    dry_run: bool,
) -> anyhow::Result<()> {
    use super::langfuse::{ExporterConfig, map::MapCtx, replay_sessions};
    let Some(lf) = &resolved.langfuse else {
        anyhow::bail!(
            "Langfuse is not configured: set [tracing.langfuse] host/public_key/secret_key or the \
             LANGFUSE_PUBLIC_KEY / LANGFUSE_SECRET_KEY environment variables"
        );
    };
    let keys: Vec<String> = match session_key {
        Some(key) => vec![key],
        None => {
            let mut stmt = conn.prepare(
                "SELECT key FROM sessions WHERE (?1 IS NULL OR last_seen_ns >= ?1) ORDER BY last_seen_ns",
            )?;
            stmt.query_map(rusqlite::params![since], |r| r.get::<_, String>(0))?
                .collect::<Result<_, _>>()?
        }
    };
    if keys.is_empty() {
        println!("nothing to export");
        return Ok(());
    }
    let mut cfg = ExporterConfig::new(lf);
    cfg.flush_interval = std::time::Duration::from_millis(200);
    let report = replay_sessions(
        conn,
        cfg,
        MapCtx::from_settings(resolved),
        &keys,
        dry_run,
        std::time::Duration::from_secs(120),
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    println!(
        "{} {} session(s), {} row(s), {} event(s) to {}",
        if dry_run { "would send" } else { "sent" },
        report.sessions,
        report.ops,
        report.events,
        lf.host
    );
    if dry_run && let Some(first) = &report.first_event {
        println!("first event: {first}");
    }
    if report.dropped > 0 {
        println!("[!!] {} event(s) were not accepted", report.dropped);
    }
    for note in &report.notes {
        println!("  {note}");
    }
    if report.notes.iter().any(|n| n.contains("authentication")) {
        anyhow::bail!("Langfuse rejected the credentials");
    }
    Ok(())
}

/// The doctor's Langfuse section.
fn doctor_langfuse(resolved: &ResolvedTracing, cfg: &config::Config) {
    println!(
        "\nlangfuse (default backend: {}):",
        resolved.backend.as_str()
    );
    let Some(lf) = &resolved.langfuse else {
        println!(
            "  [-- ] credentials: none resolved — the Langfuse backend is unavailable; launches run local\n        (set [tracing.langfuse] host/public_key/secret_key, or $LANGFUSE_PUBLIC_KEY / $LANGFUSE_SECRET_KEY)"
        );
        return;
    };
    let masked: String = lf.public_key.chars().take(8).collect::<String>() + "…";
    let source = if lf.legacy_keys {
        "legacy [langfuse] keys (move them to [tracing.langfuse])"
    } else if lf.secret_from_file {
        "config file"
    } else {
        "environment"
    };
    check(
        "credentials",
        !lf.legacy_keys,
        &format!("{source}; host {}; public key {masked}", lf.host),
    );
    if lf.secret_from_file
        && let Some(path) = &cfg.loaded_from
        && !path.is_absolute()
    {
        println!(
            "  [!!] the secret key sits in {} — prefer $LANGFUSE_SECRET_KEY (easy to commit)",
            path.display()
        );
    }
    match super::langfuse::probe(lf) {
        Ok(()) => check(
            "probe",
            true,
            "OTLP endpoint reachable, credentials accepted",
        ),
        Err(e) => check("probe", false, &e),
    }
    if resolved.backend == config::Backend::Local {
        println!("  launches go to Langfuse only when a profile or the launch dialog picks it");
    }
    if let Ok(conn) = store::open_ro(&resolved.db_path) {
        let since = now_ns() - 7 * 24 * 3600 * 1_000_000_000;
        if let Ok(mut stmt) = conn.prepare(
            "SELECT COALESCE(json_extract(metadata, '$.backend'), 'local'), count(*) FROM launches \
             WHERE started_ns >= ?1 GROUP BY 1 ORDER BY 1",
        ) {
            let rows: Vec<(String, i64)> = stmt
                .query_map([since], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|rows| rows.filter_map(Result::ok).collect())
                .unwrap_or_default();
            if !rows.is_empty() {
                let parts: Vec<String> = rows.iter().map(|(b, n)| format!("{b} {n}")).collect();
                println!("  launches (7d): {}", parts.join(", "));
            }
        }
    }
}

fn doctor() -> anyhow::Result<()> {
    println!("agent-mux trace doctor\n");
    let cfg = config::load()?;
    match &cfg.loaded_from {
        Some(path) => println!("config file: {}", path.display()),
        None => println!("config file: none found (built-in defaults; tracing is on)"),
    }
    if cfg.legacy_langfuse_section {
        println!(
            "  [!!] the config still uses [langfuse] — rename it to [tracing]; host/keys are ignored"
        );
    }
    let env = |k: &str| std::env::var(k).ok();
    let Some(resolved) = config::resolve_tracing(cfg.tracing.as_ref(), &env) else {
        println!(
            "\n[tracing] enabled = false — tracing is OFF. Remove the key or set enabled = true."
        );
        return Ok(());
    };
    println!("\nresolved configuration:");
    println!("  db_path:      {}", resolved.db_path.display());
    println!("  content_mode: {}", resolved.content_mode.as_str());
    println!(
        "  retention:    {}",
        if resolved.retention_days == 0 {
            "keep forever".to_string()
        } else {
            format!("{} days", resolved.retention_days)
        }
    );
    if config::is_wsl_drive_mount(&resolved.db_path) {
        println!(
            "  [!!] db_path is on a Windows drive mount (/mnt/*) — SQLite locking is unreliable there; prefer a path under $HOME"
        );
    }

    doctor_langfuse(&resolved, &cfg);

    println!("\nstore:");
    match store::open_ro(&resolved.db_path) {
        Err(e) => {
            check("open", false, &e);
            println!("      (the store is created on the first traced session)");
        }
        Ok(conn) => {
            let size = std::fs::metadata(&resolved.db_path)
                .map(|m| m.len())
                .unwrap_or(0);
            check(
                "open",
                true,
                &format!(
                    "{} ({:.1} MB)",
                    resolved.db_path.display(),
                    size as f64 / 1e6
                ),
            );
            let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            check(
                "schema",
                version == store::schema::SCHEMA_VERSION,
                &format!("v{version} (binary v{})", store::schema::SCHEMA_VERSION),
            );
            let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
            check("journal_mode", mode == "wal", &mode);
            let quick: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
            check("quick_check", quick == "ok", &quick);
            let opts: Vec<String> = conn
                .prepare("PRAGMA compile_options")?
                .query_map([], |r| r.get::<_, String>(0))?
                .filter_map(Result::ok)
                .collect();
            check(
                "fts5",
                opts.iter().any(|o| o == "ENABLE_FTS5"),
                if opts.iter().any(|o| o == "ENABLE_FTS5") {
                    "compiled in"
                } else {
                    "missing — `trace search` unavailable"
                },
            );
            let counts = query::counts(&conn)?;
            println!(
                "  rows: {} sessions, {} launches, {} traces, {} observations",
                counts.sessions, counts.launches, counts.traces, counts.observations
            );
            if counts.open_traces > 0 || counts.live_runs > 0 {
                println!(
                    "  live: {} open turn(s), {} run(s) without an end",
                    counts.open_traces, counts.live_runs
                );
            }
            let unpriced = query::unpriced_models(&conn)?;
            if unpriced.is_empty() {
                check("prices", true, "every model with usage has a price row");
            } else {
                let list: Vec<String> = unpriced
                    .iter()
                    .map(|(m, n)| format!("{m} ({n} generations)"))
                    .collect();
                check(
                    "prices",
                    false,
                    &format!(
                        "unpriced: {} — add [[tracing.models]] rows, then `agent-mux trace recost`",
                        list.join(", ")
                    ),
                );
            }
            let overlaps = price_table(&resolved).overlapping_patterns();
            if !overlaps.is_empty() {
                let list: Vec<String> = overlaps
                    .iter()
                    .map(|(p, a, b)| format!("{p} ({a} vs {b})"))
                    .collect();
                check(
                    "price patterns",
                    false,
                    &format!("overlapping: {}", list.join(", ")),
                );
            }
        }
    }

    println!("\nper-provider correlation readiness:");
    let claude_bin = on_path("claude");
    check(
        "claude on PATH",
        claude_bin.is_some(),
        &claude_bin
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not found".into()),
    );
    if claude_bin.is_some() {
        let supports = std::process::Command::new("claude")
            .arg("--help")
            .output()
            .ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).contains("--session-id"))
            .unwrap_or(false);
        check(
            "claude supports --session-id",
            supports,
            if supports {
                "yes — deterministic correlation"
            } else {
                "NO — set [profiles.tracing] inject_session_id = false, or update claude"
            },
        );
    }
    let claude_dir = resolved
        .claude_dir
        .clone()
        .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".claude"))
        });
    let (ok, detail) = dir_status(claude_dir.as_deref());
    check("claude data dir", ok, &detail);
    check(
        "codex on PATH",
        on_path("codex").is_some(),
        &on_path("codex")
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not found".into()),
    );
    let codex_sessions = resolved
        .codex_dir
        .clone()
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".codex"))
        })
        .map(|d| d.join("sessions"));
    let (ok, detail) = dir_status(codex_sessions.as_deref());
    check("codex sessions dir", ok, &detail);
    check(
        "agy on PATH",
        on_path("agy").is_some(),
        &on_path("agy")
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not found".into()),
    );
    let brain = resolved
        .antigravity_dir
        .clone()
        .or_else(crate::history::default_antigravity_root)
        .map(|d| d.join("brain"));
    let (ok, detail) = dir_status(brain.as_deref());
    check("antigravity brain dir", ok, &detail);

    println!("\nhooks (mode: {}):", resolved.hooks.as_str());
    if resolved.hooks.registers() {
        println!(
            "  claude: registered per launch through --settings; codex notify: per launch through -c"
        );
    } else {
        println!("  per-launch registrations are off; installed hooks still write rows");
    }
    let exe = super::hooks::register::current_exe();
    for p in ["codex", "agy"] {
        print_hook_status(&hook_status(p, &resolved.home, exe.as_deref()));
    }
    if let Ok(conn) = store::open_ro(&resolved.db_path) {
        let activity = hook_activity(&conn);
        if activity.is_empty() {
            println!("  rows (24h): none");
        } else {
            let parts: Vec<String> = activity
                .iter()
                .map(|(p, n, last)| format!("{p} {n} (last {})", fmt_age(*last)))
                .collect();
            println!("  rows (24h): {}", parts.join(", "));
        }
    }
    println!(
        "\nA provider marked [!!] still gets a launch row per session; turn traces need\nits transcript machinery above to be in place."
    );
    Ok(())
}

fn print_sessions(rows: &[query::SessionStat]) {
    if rows.is_empty() {
        println!("no sessions");
        return;
    }
    println!(
        "{:<19} {:<11} {:<12} {:>5} {:>8} {:>9} {:>9}  title",
        "last seen", "provider", "session", "turns", "tokens", "cost", "reported"
    );
    for s in rows {
        let short: String = s.session_id.chars().take(12).collect();
        let title = s
            .title
            .clone()
            .or_else(|| s.cwd.clone())
            .unwrap_or_default();
        let title: String = title.chars().take(60).collect();
        println!(
            "{:<19} {:<11} {:<12} {:>5} {:>8} {:>9} {:>9}  {}",
            fmt_time(s.last_seen_ns),
            s.provider,
            short,
            s.turn_count,
            fmt_tokens(s.total_tokens),
            fmt_cost(s.total_cost_usd),
            fmt_cost(s.reported_cost_usd),
            title
        );
    }
}

fn session_json(s: &query::SessionStat) -> serde_json::Value {
    serde_json::json!({
        "key": s.key, "provider": s.provider, "session_id": s.session_id, "title": s.title,
        "cwd": s.cwd, "project_slug": s.project_slug, "transcript_path": s.transcript_path,
        "first_seen_ns": s.first_seen_ns, "last_seen_ns": s.last_seen_ns,
        "turn_count": s.turn_count, "open_turns": s.open_turns, "duration_ms": s.duration_ms,
        "observation_count": s.observation_count, "tool_count": s.tool_count, "error_count": s.error_count,
        "input_tokens": s.input_tokens, "output_tokens": s.output_tokens,
        "cache_read_tokens": s.cache_read_tokens, "cache_write_tokens": s.cache_write_tokens,
        "total_tokens": s.total_tokens, "total_cost_usd": s.total_cost_usd,
        "reported_cost_usd": s.reported_cost_usd, "unpriced_generations": s.unpriced_generations,
    })
}

fn ls(args: &Args) -> anyhow::Result<()> {
    let (_, resolved) = resolved()?;
    let conn = open_ro(&resolved)?;
    let project_slug = if args.has("all") {
        None
    } else {
        let dir = match args.value("project") {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir()?,
        };
        Some(crate::history::project_slug(&dir))
    };
    let filter = SessionFilter {
        project_slug,
        since_ns: since_ns(args, "since")?,
        limit: args
            .value("limit")
            .and_then(|s| s.parse().ok())
            .unwrap_or(50),
    };
    let rows = query::list_sessions(&conn, &filter)?;
    if args.has("json") {
        for s in &rows {
            println!("{}", session_json(s));
        }
    } else {
        print_sessions(&rows);
        if rows.is_empty() && filter.project_slug.is_some() {
            println!("(this project only — try --all)");
        }
    }
    Ok(())
}

fn trace_json(t: &query::TraceStat) -> serde_json::Value {
    serde_json::json!({
        "id": t.id, "session_key": t.session_key, "launch_id": t.launch_id, "ordinal": t.ordinal,
        "name": t.name, "status": t.status, "start_ns": t.start_ns, "end_ns": t.end_ns,
        "latency_ms": t.latency_ms, "input": t.input, "output": t.output, "thinking": t.thinking,
        "skills": t.skills, "reported_duration_ms": t.reported_duration_ms,
        "session_cost_usd": t.session_cost_usd, "closed_by": t.closed_by,
        "observation_count": t.observation_count, "generation_count": t.generation_count,
        "tool_count": t.tool_count, "error_count": t.error_count, "open_count": t.open_count,
        "input_tokens": t.input_tokens, "output_tokens": t.output_tokens,
        "cache_read_tokens": t.cache_read_tokens, "cache_write_tokens": t.cache_write_tokens,
        "total_tokens": t.total_tokens, "total_cost_usd": t.total_cost_usd,
        "unpriced_generations": t.unpriced_generations, "models": t.models,
    })
}

fn print_observation_bodies(o: &query::ObservationView) {
    if let Some(input) = &o.input {
        print_body("input", input);
    }
    if let Some(output) = &o.output {
        print_body("output", output);
    }
    if let Some(thinking) = &o.thinking {
        print_body("thinking", thinking);
    }
    if o.metadata != "{}" {
        println!("    metadata: {}", o.metadata);
    }
}

/// `show --tree`: the turn's hierarchy, connectors and all.
fn tree_lines(observations: &[query::ObservationView]) -> Vec<String> {
    let rows = super::view::tree_rows(observations, &std::collections::HashSet::new());
    let width = rows
        .iter()
        .map(|r| r.prefix.chars().count() + r.obs.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(20, 70);
    rows.iter()
        .map(|r| {
            let label = format!("{}{}", r.prefix, r.obs.name);
            let duration = r
                .obs
                .end_ns
                .map(|e| fmt_ms((e - r.obs.start_ns) / 1_000_000))
                .unwrap_or_else(|| "running".into());
            format!(
                "{:<width$}  {:<10} {:>8} {:>8} {:>9}{}",
                truncate_display(&label, width),
                r.obs.obs_type,
                duration,
                fmt_tokens(r.obs.total_tokens),
                fmt_cost(r.obs.total_cost_usd),
                if r.obs.is_error { "  ERROR" } else { "" }
            )
        })
        .collect()
}

/// `show --timeline`: bars over the turn's own window.
fn timeline_lines(
    trace: &query::TraceStat,
    observations: &[query::ObservationView],
    now_ns: i64,
) -> Vec<String> {
    const TRACK: usize = 60;
    const LABEL: usize = 28;
    let window = super::view::window(trace.start_ns, trace.end_ns, observations, now_ns);
    let mut lines = vec![format!(
        "{:<LABEL$} {}",
        "",
        super::view::axis(&window, TRACK)
    )];
    for o in observations {
        let bar = super::view::bar(o.start_ns, o.end_ns, &window, TRACK);
        let body = if bar.instant {
            "▏".to_string()
        } else if bar.running {
            format!("{}▶", "█".repeat(bar.width.saturating_sub(1)))
        } else {
            "█".repeat(bar.width)
        };
        let label = format!("{}{}", "  ".repeat(o.depth.min(3)), o.name);
        lines.push(format!(
            "{:<LABEL$} {}{}",
            truncate_display(&label, LABEL),
            " ".repeat(bar.offset),
            body
        ));
    }
    lines
}

/// Truncates on char boundaries for the fixed-width columns above.
fn truncate_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

/// A nested observation's name, indented under its parent.
pub fn indent(depth: usize, name: &str) -> String {
    if depth == 0 {
        name.to_string()
    } else {
        format!("{}└ {name}", "  ".repeat(depth - 1))
    }
}

fn observation_json(o: &query::ObservationView) -> serde_json::Value {
    serde_json::json!({
        "id": o.id, "trace_id": o.trace_id, "parent_id": o.parent_id, "depth": o.depth,
        "type": o.obs_type, "name": o.name, "kind": o.kind,
        "start_ns": o.start_ns, "end_ns": o.end_ns, "level": o.level, "status_message": o.status_message,
        "model": o.model, "model_id": o.model_id, "input": o.input, "output": o.output, "thinking": o.thinking,
        "usage": o.usage.as_deref().and_then(|u| serde_json::from_str::<serde_json::Value>(u).ok()),
        "input_tokens": o.input_tokens, "output_tokens": o.output_tokens,
        "cache_read_tokens": o.cache_read_tokens, "cache_write_tokens": o.cache_write_tokens,
        "reasoning_tokens": o.reasoning_tokens, "total_tokens": o.total_tokens,
        "total_cost_usd": o.total_cost_usd, "tool_id": o.tool_id, "tool_name": o.tool_name,
        "skill": o.skill, "mcp_server": o.mcp_server, "path": o.path, "is_error": o.is_error,
        "metadata": serde_json::from_str::<serde_json::Value>(&o.metadata).unwrap_or(serde_json::Value::Null),
    })
}

fn print_body(label: &str, body: &str) {
    println!("    {label}:");
    for line in body.lines().take(200) {
        println!("      {line}");
    }
}

fn show(args: &Args) -> anyhow::Result<()> {
    let (_, resolved) = resolved()?;
    let conn = open_ro(&resolved)?;
    let Some(needle) = args.positional.first() else {
        anyhow::bail!("usage: agent-mux trace show <session-id | trace-id>");
    };
    let json = args.has("json");
    let full = args.has("full");
    if let Some(session) = query::find_session(&conn, needle)? {
        let turns = query::list_traces(&conn, &session.key)?;
        if json {
            let mut v = session_json(&session);
            v["turns"] = serde_json::Value::Array(turns.iter().map(trace_json).collect());
            println!("{v}");
            return Ok(());
        }
        println!(
            "session {} [{}]  {}  turns={} tokens={} cost={} reported={}",
            session.session_id,
            session.provider,
            session.title.clone().unwrap_or_default(),
            session.turn_count,
            fmt_tokens(session.total_tokens),
            fmt_cost(session.total_cost_usd),
            fmt_cost(session.reported_cost_usd)
        );
        if let Some(cwd) = &session.cwd {
            println!("cwd: {cwd}");
        }
        println!();
        println!(
            "{:>4} {:<8} {:<19} {:>8} {:>8} {:>9} {:>5} {:>4}  name",
            "turn", "status", "started", "latency", "tokens", "cost", "tools", "err"
        );
        for t in &turns {
            println!(
                "{:>4} {:<8} {:<19} {:>8} {:>8} {:>9} {:>5} {:>4}  {}  ({})",
                t.ordinal,
                t.status,
                fmt_time(t.start_ns),
                fmt_ms(t.latency_ms),
                fmt_tokens(t.total_tokens),
                fmt_cost(t.total_cost_usd),
                t.tool_count,
                t.error_count,
                t.name.chars().take(70).collect::<String>(),
                &t.id[..12]
            );
            if full {
                if let Some(input) = &t.input {
                    print_body("input", input);
                }
                if let Some(output) = &t.output {
                    print_body("output", output);
                }
            }
        }
        return Ok(());
    }
    if let Some(trace) = query::find_trace(&conn, needle)? {
        let observations = query::list_observations(&conn, &trace.id)?;
        if json {
            let mut v = trace_json(&trace);
            v["observations"] =
                serde_json::Value::Array(observations.iter().map(observation_json).collect());
            println!("{v}");
            return Ok(());
        }
        println!(
            "trace {} turn {} [{}]  {}  latency={} tokens={} cost={}",
            trace.id,
            trace.ordinal,
            trace.status,
            trace.name,
            fmt_ms(trace.latency_ms),
            fmt_tokens(trace.total_tokens),
            fmt_cost(trace.total_cost_usd)
        );
        if let Some(input) = &trace.input {
            print_body("input", input);
        }
        println!();
        let tree = args.flags.contains_key("tree");
        let timeline = args.flags.contains_key("timeline");
        if tree && timeline {
            anyhow::bail!("--tree and --timeline are two views of the same turn: pick one");
        }
        if tree {
            for line in tree_lines(&observations) {
                println!("{line}");
            }
        }
        if timeline {
            for line in timeline_lines(&trace, &observations, now_ns()) {
                println!("{line}");
            }
        }
        if tree || timeline {
            if full {
                for o in &observations {
                    print_observation_bodies(o);
                }
            }
            if let Some(output) = &trace.output {
                println!();
                print_body("output", output);
            }
            return Ok(());
        }
        println!(
            "{:<12} {:<10} {:<19} {:>8} {:>8} {:>9} {:<7}  name",
            "time", "type", "model", "duration", "tokens", "cost", "level"
        );
        for o in &observations {
            let duration = o
                .end_ns
                .map(|e| fmt_ms((e - o.start_ns) / 1_000_000))
                .unwrap_or_else(|| "running".into());
            println!(
                "{:<12} {:<10} {:<19} {:>8} {:>8} {:>9} {:<7}  {}{}",
                &fmt_time(o.start_ns)[11..],
                o.obs_type,
                o.model
                    .clone()
                    .unwrap_or_default()
                    .chars()
                    .take(19)
                    .collect::<String>(),
                duration,
                fmt_tokens(o.total_tokens),
                fmt_cost(o.total_cost_usd),
                o.level,
                indent(o.depth, &o.name),
                o.status_message
                    .as_ref()
                    .map(|m| format!(" ({m})"))
                    .unwrap_or_default()
            );
            if full {
                print_observation_bodies(o);
            }
        }
        if let Some(output) = &trace.output {
            println!();
            print_body("output", output);
        }
        return Ok(());
    }
    anyhow::bail!("no session or trace matches {needle:?}");
}

fn search(args: &Args) -> anyhow::Result<()> {
    let (_, resolved) = resolved()?;
    let conn = open_ro(&resolved)?;
    let query_text = args.positional.join(" ");
    if query_text.trim().is_empty() {
        anyhow::bail!("usage: agent-mux trace search <query>");
    }
    let limit = args
        .value("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let hits = query::search(&conn, &query_text, limit).map_err(|e| {
        anyhow::anyhow!("search failed ({e}); FTS5 syntax: words, \"phrases\", AND/OR/NOT")
    })?;
    if args.has("json") {
        for h in &hits {
            println!(
                "{}",
                serde_json::json!({"trace_id": h.trace_id, "observation_id": h.observation_id, "name": h.name, "start_ns": h.start_ns, "snippet": h.snippet})
            );
        }
        return Ok(());
    }
    if hits.is_empty() {
        println!("no matches (content is only indexed in full mode)");
    }
    for h in &hits {
        println!(
            "{}  {}  {}  {}",
            fmt_time(h.start_ns),
            &h.trace_id[..12],
            h.name,
            h.snippet.replace('\n', " ")
        );
    }
    Ok(())
}

/// Files `--discover` would import: every Claude and Antigravity transcript
/// the history viewer knows plus every Codex rollout.
pub fn discover_transcripts(resolved: &ResolvedTracing) -> Vec<(PathBuf, Provider)> {
    let mut out = Vec::new();
    let claude_dir = resolved
        .claude_dir
        .clone()
        .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
        .or_else(crate::history::default_claude_dir);
    let brain = resolved
        .antigravity_dir
        .clone()
        .map(|root| root.join("brain"))
        .or_else(crate::history::default_antigravity_dir);
    for summary in
        crate::history::discover_sessions(claude_dir.as_deref(), brain.as_deref(), None, true)
    {
        let provider = match summary.provider {
            crate::history::AgentProvider::Claude => Provider::Claude,
            crate::history::AgentProvider::Antigravity => Provider::Antigravity,
        };
        out.push((summary.file_path, provider));
    }
    let codex_sessions = resolved
        .codex_dir
        .clone()
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".codex"))
        })
        .map(|d| d.join("sessions"));
    if let Some(dir) = codex_sessions
        && dir.is_dir()
    {
        walk_rollouts(&dir, &mut out, 0);
    }
    out
}

fn walk_rollouts(dir: &Path, out: &mut Vec<(PathBuf, Provider)>, depth: usize) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rollouts(&path, out, depth + 1);
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with("rollout-")
            && name.ends_with(".jsonl")
        {
            out.push((path, Provider::Codex));
        }
    }
}

pub struct ImportSummary {
    pub session_id: String,
    pub turns: usize,
    pub ops: usize,
    pub rejected: usize,
}

/// Runs one transcript through the assembler into the store. Idempotent:
/// the launch id derives from the path, the trace ids from the session.
pub fn import_transcript(
    store: &mut Store,
    resolved: &ResolvedTracing,
    path: &Path,
    provider_override: Option<Provider>,
    content_mode: ContentMode,
) -> anyhow::Result<ImportSummary> {
    let text = std::fs::read_to_string(path)?;
    let first = text.lines().next().unwrap_or("");
    let provider = provider_override.unwrap_or_else(|| transcript::detect_provider(first));
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (session_id, cwd) = identify_transcript(&text, &abs, provider);
    let cwd = cwd.unwrap_or_else(|| ".".into());
    let launch_id = uuid::Uuid::new_v5(
        &ids::AMX_NS,
        format!("amx1|import|{}", abs.display()).as_bytes(),
    )
    .to_string();
    let started_ns = store::now_ns();
    let settings = MapSettings {
        provider,
        content_mode,
        content_max_bytes: resolved.content_max_bytes,
        redact_literals: resolved.redact_literals.clone(),
        user_id: resolved.user_id.clone(),
        release: resolved.release.clone(),
        tags: {
            let mut t = resolved.tags.clone();
            t.push("import".into());
            t
        },
        environment: resolved.environment.clone(),
        profile_name: match provider {
            Provider::Claude => "Claude Code".into(),
            Provider::Codex => "Codex".into(),
            Provider::Antigravity => "Antigravity".into(),
        },
        cwd: cwd.clone(),
        project_slug: crate::history::project_slug(Path::new(&cwd)),
        agent_mux_session: 0,
        launch_id,
        run_id: store.run_id().to_string(),
        correlation_plan: "deterministic".into(),
        injected: false,
        attached: false,
        started_ns,
    };
    let mut assembler =
        TurnAssembler::new(settings.clone(), Some(session_id.clone()), "deterministic");
    assembler.set_transcript_path(&abs.to_string_lossy());
    let mut ops = vec![
        crate::tracing::store::model::StoreOp::Launch(map::launch_adopted(
            &settings,
            &session_id,
            "deterministic",
        )),
        crate::tracing::store::model::StoreOp::Session(assembler.session_row(started_ns)),
    ];
    let recv = i128::from(started_ns);
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        for event in transcript::parse_line(provider, line) {
            ops.extend(assembler.feed(event, recv));
        }
    }
    ops.extend(assembler.finalize());
    if provider == Provider::Antigravity
        && let Some(db) = crate::tracing::agy_usage::conversation_db_for(&abs, &session_id)
    {
        for record in crate::tracing::agy_usage::AgyUsageReader::read_all(&db) {
            ops.extend(assembler.attach_step_usage(&record));
        }
    }
    let turns = ops
        .iter()
        .filter(|op| matches!(op, crate::tracing::store::model::StoreOp::Trace(t) if t.status != crate::tracing::store::model::TraceStatus::Open))
        .count();
    let end = map::SessionEnd {
        termination: "import",
        exit_code: None,
        correlation: "deterministic".into(),
        session_id: Some(session_id.clone()),
        parse_errors: 0,
        dropped_ops: 0,
        cost: assembler.cost_snapshot(),
    };
    ops.push(crate::tracing::store::model::StoreOp::Launch(
        map::launch_ended(&settings, &end, store::now_ns()),
    ));
    let mut rejected = 0usize;
    let total = ops.len();
    for chunk in ops.chunks(512) {
        rejected += store.apply(chunk)?;
    }
    Ok(ImportSummary {
        session_id,
        turns,
        ops: total,
        rejected,
    })
}

fn identify_transcript(text: &str, path: &Path, provider: Provider) -> (String, Option<String>) {
    match provider {
        Provider::Codex => {
            let mut id = None;
            let mut cwd = None;
            for line in text.lines().take(5) {
                for event in transcript::parse_codex_line(line) {
                    if let transcript::TranscriptEvent::SessionMeta {
                        session_id, cwd: c, ..
                    } = event
                    {
                        id = id.or(session_id);
                        cwd = cwd.or(c);
                    }
                }
            }
            let id = id.unwrap_or_else(|| stem(path));
            (id, cwd)
        }
        Provider::Antigravity => {
            // brain/<uuid>/transcript.jsonl or
            // brain/<uuid>/.system_generated/logs/transcript_full.jsonl
            let mut dir = path.parent();
            let mut id = None;
            while let Some(d) = dir {
                if let Some(name) = d.file_name().and_then(|n| n.to_str())
                    && uuid::Uuid::parse_str(name).is_ok()
                {
                    id = Some(name.to_string());
                    break;
                }
                dir = d.parent();
            }
            (id.unwrap_or_else(|| stem(path)), None)
        }
        Provider::Claude => {
            let cwd = text.lines().take(50).find_map(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|v| v.get("cwd").and_then(|c| c.as_str()).map(str::to_string))
            });
            (stem(path), cwd)
        }
    }
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into())
}

fn import(args: &Args) -> anyhow::Result<()> {
    let (_, resolved) = resolved()?;
    let provider = match args.value("provider") {
        Some("claude") => Some(Provider::Claude),
        Some("codex") => Some(Provider::Codex),
        Some("antigravity") | Some("agy") => Some(Provider::Antigravity),
        Some(other) => anyhow::bail!("unknown provider {other:?}"),
        None => None,
    };
    let content_mode = match args.value("content-mode") {
        Some("metadata") => ContentMode::Metadata,
        Some("full") => ContentMode::Full,
        Some(other) => anyhow::bail!("unknown content mode {other:?}"),
        None => resolved.content_mode,
    };
    let files: Vec<(PathBuf, Option<Provider>)> = if args.has("discover") {
        discover_transcripts(&resolved)
            .into_iter()
            .map(|(p, prov)| (p, Some(prov)))
            .collect()
    } else {
        args.positional
            .iter()
            .map(|p| (PathBuf::from(p), provider))
            .collect()
    };
    if files.is_empty() {
        anyhow::bail!("usage: agent-mux trace import <path>... | --discover");
    }
    let mut store = open_rw(&resolved)?;
    let mut ok = 0usize;
    for (path, prov) in &files {
        match import_transcript(&mut store, &resolved, path, *prov, content_mode) {
            Ok(summary) => {
                ok += 1;
                println!(
                    "imported {}: session {} — {} turn(s), {} row(s){}",
                    path.display(),
                    summary.session_id,
                    summary.turns,
                    summary.ops,
                    if summary.rejected > 0 {
                        format!(", {} rejected", summary.rejected)
                    } else {
                        String::new()
                    }
                );
            }
            Err(e) => println!("skipped {}: {e}", path.display()),
        }
    }
    let _ = store.end_run();
    println!(
        "{ok}/{} file(s) imported into {}",
        files.len(),
        resolved.db_path.display()
    );
    Ok(())
}

fn value_to_json(v: rusqlite::types::Value) -> serde_json::Value {
    match v {
        rusqlite::types::Value::Null => serde_json::Value::Null,
        rusqlite::types::Value::Integer(i) => serde_json::Value::from(i),
        rusqlite::types::Value::Real(f) => serde_json::Value::from(f),
        rusqlite::types::Value::Text(s) => serde_json::Value::String(s),
        rusqlite::types::Value::Blob(b) => serde_json::Value::String(ids::hex(&b)),
    }
}

fn rows_as_json(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut obj = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            obj.insert(
                name.clone(),
                value_to_json(row.get::<_, rusqlite::types::Value>(i)?),
            );
        }
        out.push(serde_json::Value::Object(obj));
    }
    Ok(out)
}

fn export(args: &Args) -> anyhow::Result<()> {
    let (_, resolved) = resolved()?;
    let conn = open_ro(&resolved)?;
    let since = since_ns(args, "since")?;
    let session_key = match args.value("session") {
        Some(needle) => Some(
            query::find_session(&conn, needle)?
                .ok_or_else(|| anyhow::anyhow!("no session matches {needle:?}"))?
                .key,
        ),
        None => None,
    };
    if args.flags.contains_key("langfuse") {
        return export_langfuse(
            &conn,
            &resolved,
            session_key,
            since,
            args.flags.contains_key("dry-run"),
        );
    }
    let mut out: Box<dyn std::io::Write> = match args.value("out") {
        Some(path) => Box::new(std::io::BufWriter::new(std::fs::File::create(path)?)),
        None => Box::new(std::io::stdout().lock()),
    };
    let tables = [
        (
            "sessions",
            "SELECT * FROM sessions WHERE (?1 IS NULL OR key = ?1) AND (?2 IS NULL OR last_seen_ns >= ?2)",
        ),
        (
            "launches",
            "SELECT * FROM launches WHERE (?1 IS NULL OR session_key = ?1) AND (?2 IS NULL OR started_ns >= ?2)",
        ),
        (
            "traces",
            "SELECT * FROM traces WHERE (?1 IS NULL OR session_key = ?1) AND (?2 IS NULL OR start_ns >= ?2)",
        ),
        (
            "observations",
            "SELECT o.* FROM observations o JOIN traces t ON t.id = o.trace_id WHERE (?1 IS NULL OR t.session_key = ?1) AND (?2 IS NULL OR t.start_ns >= ?2)",
        ),
        (
            "hook_events",
            "SELECT h.* FROM hook_events h WHERE (?1 IS NULL OR h.session_id = (SELECT session_id FROM sessions WHERE key = ?1)) AND (?2 IS NULL OR h.ts_ns >= ?2)",
        ),
    ];
    let mut n = 0usize;
    for (table, sql) in tables {
        for mut row in rows_as_json(&conn, sql, &[&session_key, &since])? {
            row["table"] = serde_json::Value::from(table);
            writeln!(out, "{row}")?;
            n += 1;
        }
    }
    out.flush()?;
    if args.value("out").is_some() {
        println!("exported {n} row(s)");
    }
    Ok(())
}

fn prune(args: &Args) -> anyhow::Result<()> {
    let (_, resolved) = resolved()?;
    let days = parse_duration_days(args.value("older-than").unwrap_or("90d"))
        .ok_or_else(|| anyhow::anyhow!("bad --older-than (try 90d)"))?;
    let cutoff = store::now_ns() - (days * 86_400.0 * 1e9) as i64;
    let store = open_rw(&resolved)?;
    let dry = args.has("dry-run");
    let counts = store.prune(cutoff, dry)?;
    println!(
        "{} {} traces, {} observations, {} launches, {} sessions, {} runs older than {}",
        if dry { "would delete" } else { "deleted" },
        counts.traces,
        counts.observations,
        counts.launches,
        counts.sessions,
        counts.runs,
        fmt_time(cutoff)
    );
    if args.has("vacuum") && !dry {
        store
            .conn()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        let size = std::fs::metadata(&resolved.db_path)
            .map(|m| m.len())
            .unwrap_or(0);
        println!("vacuumed: {:.1} MB", size as f64 / 1e6);
    }
    let _ = store.end_run();
    Ok(())
}

fn recost() -> anyhow::Result<()> {
    let (_, resolved) = resolved()?;
    let mut store = open_rw(&resolved)?;
    let (changed, unpriced) = store.recost()?;
    println!("recomputed costs: {changed} observation(s) changed, {unpriced} still unpriced");
    let _ = store.end_run();
    Ok(())
}

fn sql(args: &Args) -> anyhow::Result<()> {
    let (_, resolved) = resolved()?;
    let conn = open_ro(&resolved)?;
    let text = args.positional.join(" ");
    let head = text.trim_start().to_ascii_lowercase();
    if !(head.starts_with("select")
        || head.starts_with("with")
        || head.starts_with("explain")
        || head.starts_with("pragma"))
    {
        anyhow::bail!("only a single SELECT / WITH / EXPLAIN / PRAGMA statement is allowed");
    }
    if text.trim_end().trim_end_matches(';').contains(';') {
        anyhow::bail!("one statement at a time");
    }
    let rows = rows_as_json(&conn, &text, &[])?;
    if args.has("json") {
        for r in &rows {
            println!("{r}");
        }
        return Ok(());
    }
    if let Some(first) = rows.first().and_then(|r| r.as_object()) {
        let names: Vec<&String> = first.keys().collect();
        println!(
            "{}",
            names
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\t")
        );
        for r in &rows {
            let obj = r.as_object().unwrap();
            let cells: Vec<String> = names
                .iter()
                .map(|n| match &obj[*n] {
                    serde_json::Value::Null => String::new(),
                    serde_json::Value::String(s) => s.replace(['\t', '\n'], " "),
                    other => other.to_string(),
                })
                .collect();
            println!("{}", cells.join("\t"));
        }
    }
    println!("({} row(s))", rows.len());
    Ok(())
}

/// What one hook invocation did. The command itself always exits 0; this is
/// for tests and the `AGENT_MUX_HOOK_DEBUG` line.
#[derive(Debug, Default)]
pub struct HookOutcome {
    pub inserted: bool,
    /// What agy expects on stdout (`None` for the other sources).
    pub response: Option<String>,
    pub error: Option<String>,
}

/// Longest the hook waits for the store's write lock before giving up.
pub const HOOK_BUSY_CAP: std::time::Duration = std::time::Duration::from_millis(150);

/// `trace hook <source> [--event E] [--home DIR] [--launch ID] [--content-mode M] [--db PATH] [payload]`.
/// `stdin_override` lets tests supply the payload without a pipe.
pub fn hook_run(raw: &[String], stdin_override: Option<&str>) -> HookOutcome {
    use crate::tracing::hooks::{self, ContentPolicy, HookSource};
    let mut outcome = HookOutcome::default();
    let Some(source) = raw.first().and_then(|s| HookSource::parse(s)) else {
        outcome.error = Some("unknown hook source".into());
        return outcome;
    };
    let args = Args::parse(&raw[1..]);
    let event = args.value("event").map(str::to_string);
    if source == HookSource::Antigravity {
        outcome.response = Some(hooks::agy_response(event.as_deref().unwrap_or("")).to_string());
    }
    // payload: the last JSON-looking positional (Codex notify), else stdin
    let payload_text = match args
        .positional
        .iter()
        .rev()
        .find(|p| p.trim_start().starts_with('{'))
    {
        Some(p) => p.clone(),
        None => match stdin_override {
            Some(s) => s.to_string(),
            None => {
                let mut buf = String::new();
                let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf);
                buf
            }
        },
    };
    let payload: serde_json::Value = match serde_json::from_str(payload_text.trim()) {
        Ok(v) => v,
        Err(e) => {
            outcome.error = Some(format!("payload: {e}"));
            return outcome;
        }
    };
    // configuration: the registering TUI's home, never the CLI's cwd
    let home: PathBuf = args
        .value("home")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    let cfg = match config::load_from_home(&home) {
        Ok(c) => c,
        Err(e) => {
            outcome.error = Some(format!("config: {e}"));
            return outcome;
        }
    };
    let home_str = home.to_string_lossy().into_owned();
    let env = |k: &str| match k {
        "HOME" | "USERPROFILE" => Some(home_str.clone()),
        other => std::env::var(other).ok(),
    };
    let Some(mut resolved) = config::resolve_tracing(cfg.tracing.as_ref(), &env) else {
        outcome.error = Some("tracing disabled".into());
        return outcome;
    };
    if let Some(db) = args.value("db") {
        resolved.db_path = PathBuf::from(db);
    }
    let mode_override = match args.value("content-mode") {
        Some("metadata") => Some(ContentMode::Metadata),
        Some("full") => Some(ContentMode::Full),
        _ => None,
    };
    let policy = ContentPolicy::from_resolved(&resolved, mode_override);
    let launch_id = args
        .value("launch")
        .map(str::to_string)
        .or_else(|| std::env::var("AGENT_MUX_SESSION_ID").ok())
        .filter(|s| !s.is_empty());
    let Some(ev) = hooks::parse(
        source,
        event.as_deref(),
        &payload,
        &policy,
        store::now_ns(),
        launch_id,
    ) else {
        outcome.error = Some("event not stored".into());
        return outcome;
    };
    match store::open_hook_sink(&resolved.db_path, HOOK_BUSY_CAP) {
        Ok(conn) => match store::insert_hook_event(&conn, &ev) {
            Ok(inserted) => outcome.inserted = inserted,
            Err(e) => outcome.error = Some(format!("insert: {e}")),
        },
        Err(e) => outcome.error = Some(e),
    }
    // a user's own notify program, displaced by our per-launch override,
    // still gets the payload
    if let Some(chain) = args.value("chain")
        && let Ok(argv) = serde_json::from_str::<Vec<String>>(chain)
        && let Some((program, rest)) = argv.split_first()
    {
        let _ = std::process::Command::new(program)
            .args(rest)
            .arg(&payload_text)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_of(
        id: &str,
        name: &str,
        depth: usize,
        start_ns: i64,
        end_ns: Option<i64>,
    ) -> query::ObservationView {
        query::ObservationView {
            id: id.into(),
            trace_id: "t".into(),
            parent_id: None,
            depth,
            obs_type: "tool".into(),
            name: name.into(),
            kind: None,
            start_ns,
            end_ns,
            level: "DEFAULT".into(),
            status_message: None,
            model: None,
            model_id: None,
            input: None,
            output: None,
            thinking: None,
            usage: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            total_cost_usd: None,
            tool_id: None,
            tool_name: None,
            skill: None,
            mcp_server: None,
            path: None,
            is_error: false,
            metadata: "{}".into(),
        }
    }

    fn trace_of(start_ns: i64, end_ns: Option<i64>) -> query::TraceStat {
        query::TraceStat {
            id: "t".into(),
            session_key: "claude:s".into(),
            launch_id: None,
            ordinal: 1,
            name: "turn".into(),
            status: "closed".into(),
            start_ns,
            end_ns,
            latency_ms: 0,
            input: None,
            output: None,
            thinking: None,
            skills: "[]".into(),
            reported_duration_ms: None,
            session_cost_usd: None,
            closed_by: None,
            observation_count: 0,
            generation_count: 0,
            tool_count: 0,
            error_count: 0,
            open_count: 0,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            total_tokens: None,
            total_cost_usd: None,
            unpriced_generations: 0,
            models: None,
            metadata: "{}".into(),
            retries: 0,
            declined: 0,
        }
    }

    #[test]
    fn show_tree_prints_the_hierarchy() {
        let obs = vec![
            view_of("a", "agent: Explore", 0, 0, Some(2_000_000_000)),
            view_of("g", "Grep", 1, 200_000_000, Some(900_000_000)),
            view_of("r", "Read", 1, 1_000_000_000, None),
        ];
        let lines = tree_lines(&obs);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("agent: Explore"), "{:?}", lines[0]);
        assert!(lines[1].starts_with("├─ Grep"), "{:?}", lines[1]);
        assert!(lines[2].starts_with("└─ Read"), "{:?}", lines[2]);
        assert!(lines[0].contains("2.0s"), "{:?}", lines[0]);
        assert!(lines[2].contains("running"), "{:?}", lines[2]);
    }

    #[test]
    fn show_timeline_prints_an_axis_and_proportional_bars() {
        let obs = vec![
            view_of("a", "first", 0, 0, Some(1_000_000_000)),
            view_of("b", "second", 1, 3_000_000_000, Some(4_000_000_000)),
            view_of("c", "open", 0, 3_500_000_000, None),
        ];
        let lines = timeline_lines(&trace_of(0, Some(4_000_000_000)), &obs, 0);
        assert_eq!(lines.len(), 4, "the axis plus one row each");
        assert!(
            lines[0].contains("0ms") && lines[0].contains("4.0s"),
            "{:?}",
            lines[0]
        );
        // the first bar starts flush left, the second is pushed right
        let bar_col = |l: &str| l.find('█').unwrap_or(usize::MAX);
        assert!(bar_col(&lines[1]) < bar_col(&lines[2]), "{lines:?}");
        assert!(
            lines[2].starts_with("  second"),
            "nesting is indented: {:?}",
            lines[2]
        );
        assert!(
            lines[3].ends_with('▶'),
            "the open row is capped: {:?}",
            lines[3]
        );
        // a closed turn holding an open row is not stretched to now
        let far_future = 9_000_000_000_000;
        let same = timeline_lines(&trace_of(0, Some(4_000_000_000)), &obs, far_future);
        assert_eq!(same, lines, "now only matters while the turn is open");
    }

    #[test]
    fn duration_parsing() {
        assert_eq!(parse_duration_days("7d"), Some(7.0));
        assert_eq!(parse_duration_days("90"), Some(90.0));
        assert_eq!(parse_duration_days("12h"), Some(0.5));
        assert_eq!(parse_duration_days("2w"), Some(14.0));
        assert_eq!(parse_duration_days("x"), None);
    }

    #[test]
    fn arg_parsing_handles_flags_and_positionals() {
        let raw: Vec<String> = [
            "show",
            "abc",
            "--full",
            "--limit",
            "5",
            "--since=2d",
            "--json",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let a = Args::parse(&raw);
        assert_eq!(a.positional, vec!["show", "abc"]);
        assert!(a.has("full") && a.has("json"));
        assert_eq!(a.value("limit"), Some("5"));
        assert_eq!(a.value("since"), Some("2d"));
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(fmt_tokens(Some(999)), "999");
        assert_eq!(fmt_tokens(Some(1_500)), "1.5k");
        assert_eq!(fmt_tokens(Some(25_000)), "25k");
        assert_eq!(fmt_tokens(Some(2_500_000)), "2.5M");
        assert_eq!(fmt_tokens(None), "-");
        assert_eq!(fmt_cost(Some(1.234)), "$1.23");
        assert_eq!(fmt_cost(Some(0.0012)), "$0.0012");
        assert_eq!(fmt_ms(75_000), "1m15s");
        assert_eq!(fmt_ms(1_500), "1.5s");
        assert_eq!(fmt_time(0), "1970-01-01 00:00:00");
    }
}
