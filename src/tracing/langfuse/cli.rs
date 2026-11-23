//! CLI commands for Langfuse: `agent-mux langfuse
//! web|status|traces|export-swarm|export-project`.

use crate::config::{self, resolve_langfuse};
use crate::tracing::langfuse::api::{ListTracesParams, check_health, list_traces, resolve_web_url};

pub fn run(args: &[String]) -> Result<(), String> {
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        print_usage();
        return Ok(());
    }

    let cfg = config::load().ok();
    let env_getter = |k: &str| std::env::var(k).ok();
    let default_tracing = crate::config::TracingConfig::default();
    let tracing_cfg = cfg
        .as_ref()
        .and_then(|c| c.tracing.as_ref())
        .unwrap_or(&default_tracing);
    let lf = resolve_langfuse(tracing_cfg, &env_getter);

    match args[0].as_str() {
        "web" | "open" => run_web(&args[1..], lf.as_ref()),
        "status" | "health" => run_status(lf.as_ref()),
        "traces" | "ls" => run_traces(&args[1..], lf.as_ref()),
        "export-swarm" => run_export_swarm(&args[1..], tracing_cfg),
        "export-project" => run_export_project(&args[1..], tracing_cfg),
        cmd => {
            eprintln!("unknown langfuse command: {cmd}\n");
            print_usage();
            Err(format!("unknown command: {cmd}"))
        }
    }
}

fn print_usage() {
    println!(
        "Usage: agent-mux langfuse <command> [options]\n\n\
        Commands:\n  \
          web       Open Langfuse Web in your default browser\n  \
          status    Verify credentials and reachability\n  \
          traces    List recent traces (last 30 days, newest first)\n  \
          export-swarm <id> [--dry-run]\n            \
                    Send one swarm to Langfuse: every session of its launches in the\n            \
                    swarm's Langfuse session, then its summary trace (also sent when\n            \
                    a swarm closes). `langfuse web -s <swarm id>` opens it.\n  \
          export-project <n> [--hub owner/repo] [--dry-run]\n            \
                    Send one work item of a Project to Langfuse: every session its\n            \
                    steps started in the item's Langfuse session (<hub>#<n>), then\n            \
                    its summary trace (also sent when a build or a fix is delivered).\n\n\
        Options:\n  \
          --session <id>   Target a session (e.g. claude:s1, or a swarm id)\n  \
          --trace <id>     Target a trace ID\n  \
          --project <id>   Override Langfuse project ID\n  \
          --limit <n>      Number of traces to fetch (default: 20)\n"
    );
}

fn run_web(args: &[String], lf: Option<&crate::config::ResolvedLangfuse>) -> Result<(), String> {
    let Some(lf) = lf else {
        return Err(
            "Langfuse is not configured. Set LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY.".into(),
        );
    };

    let mut session_id = None;
    let mut trace_id = None;
    let mut project_id = lf.project_id.clone();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--session" | "-s" if i + 1 < args.len() => {
                session_id = Some(args[i + 1].clone());
                i += 2;
            }
            "--trace" | "-t" if i + 1 < args.len() => {
                trace_id = Some(args[i + 1].clone());
                i += 2;
            }
            "--project" | "-p" if i + 1 < args.len() => {
                project_id = Some(args[i + 1].clone());
                i += 2;
            }
            _ => i += 1,
        }
    }

    let url = resolve_web_url(
        &lf.host,
        project_id.as_deref(),
        trace_id.as_deref(),
        session_id.as_deref(),
    );
    println!("Opening Langfuse Web: {url}");
    crate::links::open(&url).map_err(|e| format!("could not open browser: {e}"))
}

fn run_status(lf: Option<&crate::config::ResolvedLangfuse>) -> Result<(), String> {
    let Some(lf) = lf else {
        println!("Langfuse: not configured");
        println!(
            "Set LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY, or configure [tracing.langfuse] in profiles.toml"
        );
        return Ok(());
    };

    println!("Langfuse configuration:");
    println!("  host:       {}", lf.host);
    println!(
        "  public_key: {}...",
        &lf.public_key[..lf.public_key.len().min(8)]
    );
    match &lf.project_id {
        Some(proj) => println!("  project_id: {proj}"),
        None => println!("  project_id: (from the key)"),
    }

    print!("Checking connectivity and credentials... ");
    match check_health(lf) {
        Ok(project) => {
            let name = project.name.as_deref().unwrap_or("unnamed");
            println!("● OK (authenticated to project {name}, {})", project.id);
            if let Some(configured) = lf.project_id.as_deref().filter(|p| *p != project.id) {
                println!(
                    "  warning: project_id {configured} is not the key's project ({}); web links will point at the wrong project",
                    project.id
                );
            }
            Ok(())
        }
        Err(e) => {
            println!("✗ Failed"); // main prints the error
            Err(e)
        }
    }
}

fn run_traces(args: &[String], lf: Option<&crate::config::ResolvedLangfuse>) -> Result<(), String> {
    let Some(lf) = lf else {
        return Err(
            "Langfuse is not configured. Set LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY.".into(),
        );
    };

    let mut session_id = None;
    let mut limit = 20;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--session" | "-s" if i + 1 < args.len() => {
                session_id = Some(args[i + 1].clone());
                i += 2;
            }
            "--limit" | "-n" if i + 1 < args.len() => {
                if let Ok(l) = args[i + 1].parse::<u64>() {
                    limit = l;
                }
                i += 2;
            }
            _ => i += 1,
        }
    }

    let params = ListTracesParams {
        limit: Some(limit),
        session_id,
        ..Default::default()
    };

    let resp = list_traces(lf, &params)?;
    if resp.data.is_empty() {
        println!(
            "No traces in the last {} days.",
            crate::tracing::langfuse::api::DEFAULT_LOOKBACK_DAYS
        );
        return Ok(());
    }
    println!(
        "{:<20} {:<24} {:<16} {:<8} {:<8}",
        "TIMESTAMP", "NAME", "SESSION", "LATENCY", "COST"
    );
    println!("{}", "-".repeat(80));

    for t in resp.data {
        let name = t.name.as_deref().unwrap_or("trace");
        let session = t.session_id.as_deref().unwrap_or("-");
        let lat = t
            .latency
            .map(|l| format!("{l:.2}s"))
            .unwrap_or_else(|| "-".into());
        let cost = t
            .total_cost
            .map(|c| format!("${c:.4}"))
            .unwrap_or_else(|| "-".into());
        let ts = if t.timestamp.len() >= 19 {
            &t.timestamp[..19]
        } else {
            &t.timestamp
        };

        println!(
            "{:<20} {:<24} {:<16} {:<8} {:<8}",
            ts, name, session, lat, cost
        );
    }

    Ok(())
}

/// `langfuse export-project <n> [--hub owner/repo] [--dry-run]`:
/// `tracing::langfuse::export_project`.
fn run_export_project(
    args: &[String],
    tracing_cfg: &crate::config::TracingConfig,
) -> Result<(), String> {
    use crate::tracing::langfuse::{ExporterConfig, export_project, map};
    const USAGE: &str =
        "usage: agent-mux langfuse export-project <issue number> [--hub owner/repo] [--dry-run]";
    let env_getter = |k: &str| std::env::var(k).ok();
    let resolved = crate::config::resolve_tracing(Some(tracing_cfg), &env_getter)
        .or_else(|| {
            let mut enabled = tracing_cfg.clone();
            enabled.enabled = Some(true);
            crate::config::resolve_tracing(Some(&enabled), &env_getter)
        })
        .ok_or("could not resolve tracing configuration")?;
    let Some(lf) = resolved.langfuse.as_ref() else {
        return Err(
            "Langfuse is not configured. Set LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY.".into(),
        );
    };
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let mut hub = None;
    let mut number = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--hub" if i + 1 < args.len() => {
                hub = Some(args[i + 1].clone());
                i += 2;
            }
            a if a.starts_with("--") => i += 1,
            a => {
                number = Some(a.to_string());
                i += 1;
            }
        }
    }
    let raw = number.ok_or(USAGE)?;
    let item: u64 = raw
        .trim_start_matches('#')
        .parse()
        .map_err(|_| format!("\"{raw}\" is not an issue number\n{USAGE}"))?;
    let hub = match hub {
        Some(h) => h,
        None => {
            let dir = std::env::current_dir().map_err(|e| e.to_string())?;
            crate::project::config::load(&dir)
                .map(|c| c.hub_repo().slug())
                .map_err(|e| format!("{e}; or name the hub with --hub owner/repo"))?
        }
    };
    let conn = crate::tracing::store::open_ro(&resolved.db_path)?;
    let snap = crate::project::store::load(&conn, &hub).ok().flatten();
    let item_report = crate::tracing::project_report::report(&conn, &hub, item, snap.as_ref())
        .map_err(|e| e.to_string())?;
    if item_report.launches.is_empty() {
        return Err(format!(
            "no session was recorded for #{item} of {hub}: nothing to send"
        ));
    }
    let mut cfg = ExporterConfig::new(lf);
    cfg.flush_interval = std::time::Duration::from_millis(200);
    let report = export_project(
        &conn,
        cfg,
        map::MapCtx::from_settings(&resolved),
        &item_report,
        dry_run,
        std::time::Duration::from_secs(120),
    )?;
    println!(
        "{} #{item}: {} session(s), {} row(s), {} event(s) to {} (Langfuse session {})",
        if dry_run { "would send" } else { "sent" },
        report.sessions,
        report.ops,
        report.events,
        lf.host,
        map::project_session_id(&hub, item)
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
        return Err("Langfuse rejected the credentials".into());
    }
    Ok(())
}

/// `langfuse export-swarm <id> [--dry-run]`: `tracing::langfuse::export_swarm`.
fn run_export_swarm(
    args: &[String],
    tracing_cfg: &crate::config::TracingConfig,
) -> Result<(), String> {
    use crate::tracing::langfuse::{ExporterConfig, export_swarm, map::MapCtx};
    let env_getter = |k: &str| std::env::var(k).ok();
    let resolved = crate::config::resolve_tracing(Some(tracing_cfg), &env_getter)
        .or_else(|| {
            let mut enabled = tracing_cfg.clone();
            enabled.enabled = Some(true);
            crate::config::resolve_tracing(Some(&enabled), &env_getter)
        })
        .ok_or("could not resolve tracing configuration")?;
    let Some(lf) = resolved.langfuse.as_ref() else {
        return Err(
            "Langfuse is not configured. Set LANGFUSE_PUBLIC_KEY and LANGFUSE_SECRET_KEY.".into(),
        );
    };
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let Some(id) = args.iter().find(|a| !a.starts_with("--")) else {
        return Err("usage: agent-mux langfuse export-swarm <swarm id> [--dry-run]".into());
    };
    let conn = crate::tracing::store::open_ro(&resolved.db_path)?;
    let ws = std::env::current_dir()
        .map(|d| crate::bus::store::normalize_workspace(&d))
        .unwrap_or_default();
    let swarm = crate::bus::swarm::record::resolve_swarm(&conn, &ws, Some(id))?;
    let mut cfg = ExporterConfig::new(lf);
    cfg.flush_interval = std::time::Duration::from_millis(200);
    let report = export_swarm(
        &conn,
        cfg,
        MapCtx::from_settings(&resolved),
        &swarm,
        dry_run,
        std::time::Duration::from_secs(120),
    )?;
    println!(
        "{} {}: {} session(s), {} row(s), {} event(s) to {} (Langfuse session {})",
        if dry_run { "would send" } else { "sent" },
        swarm.id,
        report.sessions,
        report.ops,
        report.events,
        lf.host,
        swarm.id
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
        return Err("Langfuse rejected the credentials".into());
    }
    Ok(())
}
