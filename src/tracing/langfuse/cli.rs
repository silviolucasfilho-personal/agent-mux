//! CLI commands for Langfuse: `agent-mux langfuse web|status|traces`.

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
          traces    List remote traces from Langfuse REST API\n\n\
        Options:\n  \
          --session <id>   Target a session (e.g. claude:s1)\n  \
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
    if let Some(proj) = &lf.project_id {
        println!("  project_id: {proj}");
    } else {
        println!("  project_id: (auto-detected from traces)");
    }

    print!("Checking connectivity and credentials... ");
    match check_health(lf) {
        Ok(()) => {
            println!("● OK (connected & authenticated)");
            Ok(())
        }
        Err(e) => {
            println!("✗ Failed\n  Error: {e}");
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
        order_by: Some("timestamp.desc".into()),
        ..Default::default()
    };

    let resp = list_traces(lf, &params)?;
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
