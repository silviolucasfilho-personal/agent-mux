//! `agent-mux workspace …`: scan, inspect and summarize workspace folders and
//! sub-folders from the command line.

use super::activity::{ActivitySource, format_interval};
use super::scan::{inspect_folder, scan_workspace};
use super::{SCAN_DEPTH, WorkspaceNode, summary_cache};
use crate::config;
use crate::tracing::cli::{fmt_cost, fmt_tokens};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub const USAGE: &str = "agent-mux workspace <command>

  ls [DIR] [--depth N] [--json]       scan DIR (default: current directory) and list
                                      each folder and sub-folder with its tech stack,
                                      summary and agent activity roll-up
  show [DIR] [--json]                 full dossier for DIR and its sub-folders
  summarize [DIR] [--harness H]       generate and cache an LLM summary for DIR
                                      (claude, codex, or agy; default: claude)
";

pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None | Some("ls") => {
            let rest = if args.first().map(String::as_str) == Some("ls") {
                &args[1..]
            } else {
                args
            };
            cmd_ls(rest)
        }
        Some("show") => cmd_show(&args[1..]),
        Some("summarize") => cmd_summarize(&args[1..]),
        Some("help") | Some("--help") | Some("-h") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown workspace command {other:?}\n\n{USAGE}");
            std::process::exit(1);
        }
    }
}

fn resolve_db_path() -> Option<PathBuf> {
    let cfg = config::load().ok()?;
    let resolved = config::resolve_tracing(cfg.tracing.as_ref(), &|k| std::env::var(k).ok())?;
    Some(resolved.db_path)
}

fn cmd_ls(raw: &[String]) -> Result<()> {
    let mut dir: Option<PathBuf> = None;
    let mut depth = SCAN_DEPTH;
    let mut json = false;
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--json" => json = true,
            "--depth" => {
                if let Some(v) = raw.get(i + 1) {
                    depth = v.parse().unwrap_or(SCAN_DEPTH);
                    i += 1;
                }
            }
            s if !s.starts_with('-') && dir.is_none() => {
                dir = Some(PathBuf::from(s));
            }
            _ => {}
        }
        i += 1;
    }

    let root = dir
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let lib = crate::assets::root();
    let db_path = resolve_db_path();
    let source = ActivitySource::load(db_path.as_deref(), None, &lib, Vec::new());
    let mut nodes = scan_workspace(&root, depth, Some(&lib), &source.active_dirs());
    source.populate(&mut nodes);

    if json {
        println!("{}", serde_json::to_string_pretty(&nodes)?);
        return Ok(());
    }

    println!(
        "Workspaces under {} ({} folder{})",
        root.display(),
        nodes.len(),
        if nodes.len() == 1 { "" } else { "s" }
    );
    println!();
    for node in &nodes {
        let indent = "  ".repeat(node.depth);
        let stack_badge = node.stack.short_badge();
        let stack_str = if stack_badge.is_empty() {
            "—".to_string()
        } else {
            stack_badge
        };
        let sess_count = node.activity.stats.subtree_sessions_30d;
        let loops_count = node.activity.scheduled_agents.len();
        let mut activity_parts = Vec::new();
        if sess_count > 0 {
            activity_parts.push(format!("{sess_count} sess (30d)"));
        }
        if loops_count > 0 {
            activity_parts.push(format!(
                "{loops_count} loop{}",
                if loops_count == 1 { "" } else { "s" }
            ));
        }
        if !node.customization.project_personas.is_empty() {
            activity_parts.push(format!(
                "{} persona{}",
                node.customization.project_personas.len(),
                if node.customization.project_personas.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ));
        }
        let act_str = if activity_parts.is_empty() {
            String::new()
        } else {
            format!("  [{}]", activity_parts.join(" · "))
        };
        println!("{indent}{:<26} {:<18}{act_str}", node.rel_path, stack_str);
        println!("{indent}  {}", node.summary.text);
    }
    Ok(())
}

fn cmd_show(raw: &[String]) -> Result<()> {
    let mut dir: Option<PathBuf> = None;
    let mut json = false;
    for arg in raw {
        if arg == "--json" {
            json = true;
        } else if !arg.starts_with('-') && dir.is_none() {
            dir = Some(PathBuf::from(arg));
        }
    }
    let target = dir
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let lib = crate::assets::root();
    let db_path = resolve_db_path();
    let source = ActivitySource::load(db_path.as_deref(), None, &lib, Vec::new());
    let mut nodes = scan_workspace(&target, SCAN_DEPTH, Some(&lib), &source.active_dirs());
    source.populate(&mut nodes);

    let Some(root_node) = nodes.first() else {
        return Ok(());
    };
    if json {
        println!("{}", serde_json::to_string_pretty(root_node)?);
        return Ok(());
    }

    print_node_dossier(root_node, &nodes[1..]);
    Ok(())
}

fn print_node_dossier(node: &WorkspaceNode, children: &[WorkspaceNode]) {
    println!("{} — {}", node.name, node.path.display());
    println!("Stack:   {}", node.stack.detail_line());
    if let Some(branch) = &node.git.branch {
        println!("Git:     {:?} on branch {branch}", node.git.kind);
    }
    let badges = node.customization.summary_badges();
    if !badges.is_empty() {
        println!("Config:  {}", badges.join(" · "));
    }
    println!();
    println!("Summary:");
    println!("  {}", node.summary.text);
    if let Some(arch) = &node.summary.architecture {
        println!("  Architecture: {arch}");
    }
    if !node.summary.entry_points.is_empty() {
        println!("  Entry points: {}", node.summary.entry_points.join(", "));
    }
    if !children.is_empty() {
        println!();
        println!("Sub-folders ({}):", children.len());
        for c in children {
            println!(
                "  {:<24} {:<16} {}",
                c.rel_path,
                c.stack.short_badge(),
                c.summary.text
            );
        }
    }
    println!();
    let stats = &node.activity.stats;
    println!(
        "Agent Activity (30d): {} session(s) ({} direct) · {} tokens · {}",
        stats.subtree_sessions_30d,
        stats.direct_sessions_30d,
        fmt_tokens(Some(stats.total_tokens_30d)),
        fmt_cost(Some(stats.total_cost_usd_30d))
    );
    if !node.activity.scheduled_agents.is_empty() {
        println!();
        println!("Scheduled Loops:");
        for lp in &node.activity.scheduled_agents {
            let sub = lp
                .rel_subpath
                .as_deref()
                .map(|s| format!(" [in {s}]"))
                .unwrap_or_default();
            let outcome = lp.last_outcome.as_deref().unwrap_or("no runs yet");
            println!(
                "  {} ({}, every {}): {outcome}{sub}",
                lp.loop_id,
                lp.harness,
                format_interval(lp.interval_s)
            );
        }
    }
    if !node.activity.recent_sessions.is_empty() {
        println!();
        println!("Recent Sessions:");
        for s in &node.activity.recent_sessions {
            let sub = s
                .rel_subpath
                .as_deref()
                .map(|p| format!(" [in {p}]"))
                .unwrap_or_default();
            println!(
                "  {:>8}  {:<11} {:>3}t  \"{}\"{sub}",
                s.age_label, s.provider, s.turn_count, s.title
            );
        }
    }
}

fn cmd_summarize(raw: &[String]) -> Result<()> {
    let mut dir: Option<PathBuf> = None;
    let mut harness = "claude".to_string();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--harness" => {
                if let Some(v) = raw.get(i + 1) {
                    harness = v.clone();
                    i += 1;
                }
            }
            s if !s.starts_with('-') && dir.is_none() => {
                dir = Some(PathBuf::from(s));
            }
            _ => {}
        }
        i += 1;
    }

    let target = dir
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let lib = crate::assets::root();
    match run_headless_summarize(&target, &harness, &lib) {
        Ok((cached, path)) => {
            println!("Saved summary to {}", path.display());
            println!("Purpose: {}", cached.purpose);
            if let Some(arch) = &cached.architecture {
                println!("Architecture: {arch}");
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("summarize failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Runs a headless read-only harness invocation to summarize `folder` and
/// writes the result into `<library_root>/workspaces/<hash>.json`.
pub fn run_headless_summarize(
    folder: &Path,
    harness: &str,
    library_root: &Path,
) -> Result<(summary_cache::CachedSummary, PathBuf), String> {
    let canon = folder
        .canonicalize()
        .unwrap_or_else(|_| folder.to_path_buf());
    let (stack, _, _, _, _) = inspect_folder(&canon, Some(library_root));
    let prompt = summary_cache::build_prompt(&canon, &stack);

    let mut cmd = match harness {
        "codex" => {
            let mut c = Command::new("codex");
            c.args(["exec", "-s", "read-only", &prompt]);
            c
        }
        "agy" | "antigravity" => {
            let mut c = Command::new("agy");
            c.args(["--prompt", &prompt]);
            c
        }
        _ => {
            let mut c = Command::new("claude");
            c.args(["-p", &prompt]);
            c
        }
    };
    cmd.current_dir(&canon);
    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn {harness}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{harness} exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let now_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let now_str = crate::loops::format_timestamp(crate::loops::now());
    let cached = summary_cache::parse_llm_response(&canon, harness, &now_str, now_s, &stdout)?;
    let path = summary_cache::save(library_root, &canon, &cached)?;
    Ok((cached, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::SummarySource;
    use std::collections::HashSet;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn scans_projects_and_subfolders_and_detects_tech_stacks() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Root has README
        fs::write(
            root.join("README.md"),
            "# My Monorepo\n\nA multi-language workspace for testing agent-mux.\n",
        )
        .unwrap();

        // Rust subproject
        let rust_dir = root.join("backend-rs");
        fs::create_dir_all(rust_dir.join("src")).unwrap();
        fs::write(
            rust_dir.join("Cargo.toml"),
            r#"[package]
name = "backend-rs"
version = "0.1.0"
edition = "2024"
description = "Fast Rust API server"

[dependencies]
axum = "0.8"
tokio = "1.0"
rusqlite = "0.40"
"#,
        )
        .unwrap();
        fs::write(rust_dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(
            rust_dir.join("AGENTS.md"),
            "# Agent Rules\nUse cargo test.\n",
        )
        .unwrap();

        // Next.js TypeScript subproject
        let web_dir = root.join("apps/web");
        fs::create_dir_all(&web_dir).unwrap();
        fs::write(web_dir.join("tsconfig.json"), "{}").unwrap();
        fs::write(web_dir.join("pnpm-lock.yaml"), "").unwrap();
        fs::write(
            web_dir.join("package.json"),
            r#"{
  "name": "@acme/web",
  "description": "Customer dashboard built with Next.js",
  "dependencies": {
    "next": "15.0.0",
    "react": "19.0.0",
    "tailwindcss": "4.0.0"
  }
}"#,
        )
        .unwrap();

        // Nested sub-project or subfolder inside a project must NOT be explored
        let nested_in_rust = rust_dir.join("crates/subcrate");
        fs::create_dir_all(&nested_in_rust).unwrap();
        fs::write(
            nested_in_rust.join("Cargo.toml"),
            "[package]\nname = \"subcrate\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let nodes = scan_workspace(root, 3, None, &HashSet::new());
        let rels: Vec<&str> = nodes.iter().map(|n| n.rel_path.as_str()).collect();
        assert_eq!(rels, vec![".", "apps", "apps/web", "backend-rs"]);

        // Scanning a project folder directly stops at that project folder
        let direct_nodes = scan_workspace(&rust_dir, 3, None, &HashSet::new());
        assert_eq!(direct_nodes.len(), 1);
        assert_eq!(direct_nodes[0].rel_path, ".");

        let rust_node = nodes.iter().find(|n| n.rel_path == "backend-rs").unwrap();
        assert_eq!(rust_node.stack.languages, vec!["Rust"]);
        assert!(rust_node.stack.frameworks.contains(&"Axum".to_string()));
        assert!(rust_node.stack.frameworks.contains(&"tokio".to_string()));
        assert!(rust_node.stack.frameworks.contains(&"SQLite".to_string()));
        assert_eq!(rust_node.summary.text, "Fast Rust API server");
        assert_eq!(rust_node.summary.source, SummarySource::Manifest);
        assert!(rust_node.customization.has_agents_md);
        assert_eq!(rust_node.summary.entry_points, vec!["src/main.rs"]);

        let web_node = nodes.iter().find(|n| n.rel_path == "apps/web").unwrap();
        assert!(web_node.stack.languages.contains(&"TypeScript".to_string()));
        assert!(web_node.stack.frameworks.contains(&"Next.js".to_string()));
        assert!(web_node.stack.tools.contains(&"pnpm".to_string()));
        assert_eq!(
            web_node.summary.text,
            "Customer dashboard built with Next.js"
        );
    }

    #[test]
    fn rolls_up_subfolder_agent_activity_into_parent() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let sub = root.join("services/auth");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("go.mod"), "module example.com/auth\n\ngo 1.23\n").unwrap();

        let now_ns = 1_700_000_000_000_000_000i64;
        let sub_canon = sub.canonicalize().unwrap();
        let root_canon = root.canonicalize().unwrap();

        let source = ActivitySource {
            live: vec![crate::workspace::activity::LiveSessionSnapshot {
                session_idx: 0,
                session_id: 1,
                profile_name: "Claude Code".into(),
                harness: Some(crate::harness::Harness::Claude),
                status_label: "running".into(),
                is_running: true,
                dir: sub_canon.clone(),
            }],
            traced_sessions: vec![crate::tracing::store::query::SessionStat {
                key: "claude:s1".into(),
                provider: "claude".into(),
                session_id: "s1".into(),
                title: Some("Add JWT verification".into()),
                cwd: Some(sub_canon.to_string_lossy().into_owned()),
                project_slug: None,
                transcript_path: None,
                first_seen_ns: now_ns - 600_000_000_000,
                last_seen_ns: now_ns - 300_000_000_000,
                turn_count: 5,
                open_turns: 0,
                duration_ms: Some(12_000),
                observation_count: 10,
                tool_count: 4,
                error_count: 0,
                input_tokens: Some(10_000),
                output_tokens: Some(2_000),
                cache_read_tokens: None,
                cache_write_tokens: None,
                total_tokens: Some(12_000),
                total_cost_usd: Some(0.18),
                reported_cost_usd: None,
                unpriced_generations: 0,
                correlation: Some("deterministic".into()),
            }],
            history_sessions: Vec::new(),
            loops: Vec::new(),
            loop_last_outcomes: std::collections::HashMap::new(),
            now_ns,
        };

        let mut nodes = scan_workspace(&root_canon, 3, None, &source.active_dirs());
        source.populate(&mut nodes);

        let root_node = &nodes[0];
        assert_eq!(root_node.activity.live_sessions.len(), 1);
        assert_eq!(
            root_node.activity.live_sessions[0].rel_subpath.as_deref(),
            Some("services/auth")
        );
        assert_eq!(root_node.activity.stats.direct_sessions_30d, 0);
        assert_eq!(root_node.activity.stats.subtree_sessions_30d, 1);
        assert_eq!(root_node.activity.stats.total_tokens_30d, 12_000);

        let auth_node = nodes
            .iter()
            .find(|n| n.rel_path == "services/auth")
            .unwrap();
        assert_eq!(auth_node.activity.stats.direct_sessions_30d, 1);
        assert_eq!(auth_node.activity.recent_sessions[0].rel_subpath, None);
    }

    #[test]
    fn caches_and_loads_llm_summaries() {
        let tmp = TempDir::new().unwrap();
        let lib = tmp.path().join("lib");
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("README.md"), "# Fallback doc\n\nFallback.\n").unwrap();

        let raw_output = r#"```json
{
  "purpose": "Distributed task scheduler for coding agents.",
  "architecture": "Event-driven Rust worker pool backed by SQLite.",
  "entry_points": ["src/main.rs", "src/worker.rs"]
}
```"#;
        let now_s = summary_cache::newest_marker_mtime(&proj) + 10;
        let cached = summary_cache::parse_llm_response(
            &proj,
            "claude",
            "2026-10-28T11:00:00Z",
            now_s,
            raw_output,
        )
        .unwrap();
        summary_cache::save(&lib, &proj, &cached).unwrap();

        let (_, _, _, summary, _) = inspect_folder(&proj, Some(&lib));
        assert_eq!(
            summary.text,
            "Distributed task scheduler for coding agents."
        );
        assert_eq!(
            summary.architecture.as_deref(),
            Some("Event-driven Rust worker pool backed by SQLite.")
        );
        assert_eq!(summary.entry_points, vec!["src/main.rs", "src/worker.rs"]);
        assert!(matches!(
            summary.source,
            SummarySource::CachedLlm { stale: false, .. }
        ));
    }
}
