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
  show <session|trace> [--full] [--json]
                               turns of a session, or observations of a trace
  search <fts5 query> [--limit N] [--json]
                               full-text search over content (full mode data)
  import <path>... | --discover [--provider claude|codex|antigravity] [--content-mode full|metadata]
                               backfill transcripts (idempotent)
  export [--session ID] [--since 30d] [--out FILE]
                               JSON Lines dump of every table
  prune [--older-than 90d] [--vacuum] [--dry-run]
                               delete old rows; --vacuum reclaims space
  recost                       recompute cost from usage and the current price table
  sql <select ...> [--json]    read-only query";

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
                    "all" | "json" | "full" | "discover" | "vacuum" | "dry-run"
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

fn observation_json(o: &query::ObservationView) -> serde_json::Value {
    serde_json::json!({
        "id": o.id, "trace_id": o.trace_id, "type": o.obs_type, "name": o.name, "kind": o.kind,
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
                o.name,
                o.status_message
                    .as_ref()
                    .map(|m| format!(" ({m})"))
                    .unwrap_or_default()
            );
            if full {
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

#[cfg(test)]
mod tests {
    use super::*;

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
