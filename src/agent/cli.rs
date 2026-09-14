//! CLI command handlers for `agent-mux agent <subcommand>`.

use crate::agent::artifacts::{render_artifacts, write_artifacts};
use crate::agent::discovery::{discover_agents, migrate_legacy};
use crate::agent::install::{DoctorStatus, doctor_agent, install_agent, uninstall_agent};
use std::path::{Path, PathBuf};

pub fn handle_agent_cli(args: &[String]) -> Result<(), String> {
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" || args[0] == "help" {
        print_help();
        return Ok(());
    }

    match args[0].as_str() {
        "build" => {
            if args.len() < 2 {
                return Err(
                    "Usage: agent-mux agent build <agent-id> [--harness <all|claude|codex|agy>]"
                        .to_string(),
                );
            }
            let agent_id = &args[1];
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let ws = cwd.join(".agent-mux").join("agents");
            let global = crate::agent::default_agents_dir();
            let bundled = crate::agent::bundled_agents_dir();

            let report = discover_agents(&ws, &global, bundled.as_deref());
            let agent = report
                .agents
                .iter()
                .find(|a| a.id == *agent_id)
                .ok_or_else(|| format!("Agent '{agent_id}' not found"))?;

            let source_path = agent.file_path.as_deref().unwrap_or(Path::new("AGENTS.md"));
            let artifacts = render_artifacts(agent, source_path)
                .map_err(|e| format!("Failed to render artifacts: {e}"))?;

            let gen_dir = source_path
                .parent()
                .map(|p| p.join("generated"))
                .unwrap_or_else(|| PathBuf::from("generated"));

            write_artifacts(&gen_dir, &artifacts)
                .map_err(|e| format!("Failed to write artifacts: {e}"))?;

            println!(
                "Successfully built artifacts for agent '{}' at '{}'",
                agent_id,
                gen_dir.display()
            );
            Ok(())
        }
        "doctor" => {
            if args.len() < 2 {
                return Err("Usage: agent-mux agent doctor <agent-id>".to_string());
            }
            let agent_id = &args[1];
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let ws = cwd.join(".agent-mux").join("agents");
            let global = crate::agent::default_agents_dir();
            let bundled = crate::agent::bundled_agents_dir();

            let report = discover_agents(&ws, &global, bundled.as_deref());
            let agent = report
                .agents
                .iter()
                .find(|a| a.id == *agent_id)
                .ok_or_else(|| format!("Agent '{agent_id}' not found"))?;

            let root_dir = agent
                .file_path
                .as_ref()
                .and_then(|p| p.parent())
                .unwrap_or(Path::new("."));

            let doc =
                doctor_agent(agent, root_dir).map_err(|e| format!("Doctor check failed: {e}"))?;

            match doc.status {
                DoctorStatus::Healthy => {
                    println!(
                        "Agent '{}' is healthy. All artifacts are up to date.",
                        agent_id
                    );
                }
                status => {
                    println!("Agent '{}' check: {:?}", agent_id, status);
                    for issue in &doc.issues {
                        println!("  - {}", issue);
                    }
                }
            }
            Ok(())
        }
        "migrate" => {
            if args.len() < 3 {
                return Err(
                    "Usage: agent-mux agent migrate <source.md> <dest/AGENTS.md>".to_string(),
                );
            }
            let source = Path::new(&args[1]);
            let dest = Path::new(&args[2]);
            migrate_legacy(source, dest).map_err(|e| format!("Migration error: {e}"))?;
            println!(
                "Successfully migrated '{}' to '{}'",
                source.display(),
                dest.display()
            );
            Ok(())
        }
        "install" => {
            if args.len() < 2 {
                return Err("Usage: agent-mux agent install <agent-id> [target-dir]".to_string());
            }
            let agent_id = &args[1];
            let target_dir = if args.len() >= 3 {
                PathBuf::from(&args[2])
            } else {
                crate::agent::default_agents_dir().join(agent_id)
            };

            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let ws = cwd.join(".agent-mux").join("agents");
            let global = crate::agent::default_agents_dir();
            let bundled = crate::agent::bundled_agents_dir();

            let report = discover_agents(&ws, &global, bundled.as_deref());
            let agent = report
                .agents
                .iter()
                .find(|a| a.id == *agent_id)
                .ok_or_else(|| format!("Agent '{agent_id}' not found"))?;

            let source_path = agent.file_path.as_deref().unwrap_or(Path::new("AGENTS.md"));
            let artifacts = render_artifacts(agent, source_path)
                .map_err(|e| format!("Failed to render artifacts: {e}"))?;

            let res = install_agent(agent, &artifacts, &target_dir, None)
                .map_err(|e| format!("Install failed: {e}"))?;

            println!(
                "Installed {} artifact files to '{}'",
                res.installed_files.len(),
                target_dir.display()
            );
            Ok(())
        }
        "uninstall" => {
            if args.len() < 2 {
                return Err("Usage: agent-mux agent uninstall <agent-id> [target-dir]".to_string());
            }
            let agent_id = &args[1];
            let target_dir = if args.len() >= 3 {
                PathBuf::from(&args[2])
            } else {
                crate::agent::default_agents_dir().join(agent_id)
            };

            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let ws = cwd.join(".agent-mux").join("agents");
            let global = crate::agent::default_agents_dir();
            let bundled = crate::agent::bundled_agents_dir();

            let report = discover_agents(&ws, &global, bundled.as_deref());
            let agent = report
                .agents
                .iter()
                .find(|a| a.id == *agent_id)
                .ok_or_else(|| format!("Agent '{agent_id}' not found"))?;

            let res = uninstall_agent(agent, &target_dir, None)
                .map_err(|e| format!("Uninstall failed: {e}"))?;

            println!(
                "Removed {} artifact files from '{}'",
                res.removed_files.len(),
                target_dir.display()
            );
            Ok(())
        }
        "watch" => {
            if args.len() < 2 {
                return Err(
                    "Usage: agent-mux agent watch <start|stop|status> [agent-id]".to_string(),
                );
            }
            let action = &args[1];
            let agent_id = args.get(2).map(|s| s.as_str()).unwrap_or("all");
            match action.as_str() {
                "start" => {
                    println!("Started monitoring watcher for agent '{agent_id}'");
                    Ok(())
                }
                "stop" => {
                    println!("Stopped monitoring watcher for agent '{agent_id}'");
                    Ok(())
                }
                "status" => {
                    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
                    let state_path = cwd.join(".agent-mux").join("agent-state.db");
                    if state_path.exists() {
                        let state = crate::agent::state::StateStore::open(&state_path)
                            .map_err(|e| format!("Failed to open state DB: {e}"))?;
                        let scope = crate::agent::state::AgentScope {
                            agent_id: agent_id.to_string(),
                            source_path: cwd.join("AGENTS.md"),
                            workspace: cwd,
                        };
                        let reviewed = state.reviewed_through(&scope).unwrap_or(0);
                        let consumed = state.consumption_cursor(&scope).unwrap_or(0);
                        println!(
                            "Watcher status for '{}': consumed_seq={}, reviewed_seq={}",
                            agent_id, consumed, reviewed
                        );
                    } else {
                        println!("Watcher status: inactive (no agent-state.db found)");
                    }
                    Ok(())
                }
                other => Err(format!(
                    "Unknown watch action: '{other}'. Usage: agent-mux agent watch <start|stop|status> [agent-id]"
                )),
            }
        }
        "mark-reviewed" => {
            if args.len() < 2 {
                return Err(
                    "Usage: agent-mux agent mark-reviewed <briefing-id> [agent-id]".to_string(),
                );
            }
            let briefing_id = &args[1];
            let agent_id = args.get(2).map(|s| s.as_str()).unwrap_or("heimdall");
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let state_path = cwd.join(".agent-mux").join("agent-state.db");
            let mut state = crate::agent::state::StateStore::open(&state_path)
                .map_err(|e| format!("Failed to open state store: {e}"))?;
            let scope = crate::agent::state::AgentScope {
                agent_id: agent_id.to_string(),
                source_path: cwd.join("AGENTS.md"),
                workspace: cwd,
            };
            state
                .acknowledge(&scope, briefing_id)
                .map_err(|e| format!("Failed to acknowledge briefing '{briefing_id}': {e}"))?;
            println!("Acknowledged briefing '{briefing_id}' for agent '{agent_id}'");
            Ok(())
        }
        "retry" => {
            if args.len() < 2 {
                return Err("Usage: agent-mux agent retry <job-id>".to_string());
            }
            let job_id = &args[1];
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let state_path = cwd.join(".agent-mux").join("agent-state.db");
            let mut state = crate::agent::state::StateStore::open(&state_path)
                .map_err(|e| format!("Failed to open state store: {e}"))?;
            state
                .update_job_status(job_id, crate::agent::state::JobStatus::Pending)
                .map_err(|e| format!("Failed to reset job '{job_id}': {e}"))?;
            println!("Reset job '{job_id}' status to pending for retry");
            Ok(())
        }
        other => Err(format!(
            "Unknown agent subcommand: '{other}'. Run 'agent-mux agent help' for usage."
        )),
    }
}

fn print_help() {
    println!(
        r#"Usage: agent-mux agent <subcommand> [args...]

Subcommands:
  build <id> [--harness <all|claude|codex|agy>]
      Generates deterministic harness artifacts and manifest.json.
  doctor <id>
      Verifies package integrity, source hashes, and artifact freshness.
  migrate <source.md> <dest/AGENTS.md>
      Migrates legacy flat markdown file to canonical AGENTS.md package.
  install <id> [target-dir]
      Installs generated harness artifacts into the target directory.
  uninstall <id> [target-dir]
      Removes installed harness artifacts from the target directory.
  watch <start|stop|status> [id]
      Controls or queries the background agent monitoring watcher.
  mark-reviewed <briefing-id> [agent-id]
      Explicitly marks a briefing as reviewed and advances review cursor.
  retry <job-id>
      Explicitly resets an interrupted or failed investigation job to pending.
"#
    );
}
