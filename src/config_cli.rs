//! `agent-mux config …`: the Configuration view headless. Same catalog as
//! the view (`crate::assets`), so the CLI and the TUI never disagree about
//! what is built in, overridden or invalid.

use crate::assets::{Catalog, Kind, Source};

pub const USAGE: &str = "agent-mux config <command>

  ls [--json]                  every prompt, skill, loop pattern, loop skill, loop agent
                               and template, with its kind, source and status
  show <id> [--builtin]        the effective text (or the compiled-in text)
  path [<id>]                  the library root, or where an item's override lives
  edit <id>                    copy the built-in text into the library when needed and
                               open it in `editor` (profiles.toml), $VISUAL, $EDITOR or vi
  reset <id>                   delete the override (or a user-added item)
  new skill|loop-skill|loop-agent <name>
                               create an item from a skeleton and print its path
  check                        validate every item; exit 1 when one has problems
  push [--dry-run]             rewrite the loop skills and agents in every registered
                               loop's workspace with the effective text

An <id> is the path under the library (loops/skills/loop-triage/SKILL.md) or any
unique suffix or name (loop-triage, registry.toml, prompts.toml). The library is
~/.agent-mux (AGENT_MUX_LIBRARY_DIR overrides); docs/configuration.md explains
the layout.";

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let settings = crate::config::load().ok().and_then(|c| c.loaded_from);
    let root = crate::assets::root();
    let catalog = Catalog::load(&root, settings.as_deref());
    let rest = args.get(1..).unwrap_or_default();
    let flag = |f: &str| rest.iter().any(|a| a == f);
    let positional = |n: usize| rest.iter().filter(|a| !a.starts_with("--")).nth(n);
    match args.first().map(String::as_str) {
        Some("ls") | Some("list") => {
            if flag("--json") {
                let items: Vec<serde_json::Value> = catalog
                    .assets
                    .iter()
                    .map(|a| {
                        serde_json::json!({
                            "id": a.id,
                            "kind": a.kind.label(),
                            "name": a.name,
                            "source": a.source.label(),
                            "path": a.path,
                            "builtin": a.repo_path,
                            "problems": a.problems,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "root": catalog.root,
                        "settings": catalog.settings_path,
                        "items": items,
                    }))?
                );
                return Ok(());
            }
            println!("library  {}", catalog.root.display());
            for kind in Kind::ALL {
                let items: Vec<_> = catalog.by_kind(kind).collect();
                if items.is_empty() {
                    continue;
                }
                println!("\n{}", kind.title());
                for a in items {
                    let status = if a.valid() {
                        String::new()
                    } else {
                        format!("  ! {}", a.problems[0])
                    };
                    println!("  {:<44} {:<9}{}", a.id, a.source.label(), status);
                }
            }
            Ok(())
        }
        Some("show") => {
            let id = positional(0)
                .ok_or_else(|| anyhow::anyhow!("usage: agent-mux config show <id> [--builtin]"))?;
            let a = catalog.find(id).map_err(|e| anyhow::anyhow!(e))?;
            if flag("--builtin") {
                match a.builtin {
                    Some(t) => print!("{t}"),
                    None => anyhow::bail!("{} has no built-in text", a.id),
                }
            } else {
                print!("{}", a.effective());
            }
            Ok(())
        }
        Some("path") => {
            match positional(0) {
                Some(id) => {
                    let a = catalog.find(id).map_err(|e| anyhow::anyhow!(e))?;
                    println!("{}", a.path.display());
                }
                None => println!("{}", catalog.root.display()),
            }
            Ok(())
        }
        Some("edit") => {
            let id = positional(0)
                .ok_or_else(|| anyhow::anyhow!("usage: agent-mux config edit <id>"))?;
            let a = catalog.find(id).map_err(|e| anyhow::anyhow!(e))?.clone();
            let path = catalog
                .create_override(&a)
                .map_err(|e| anyhow::anyhow!(e))?;
            let editor = crate::config::load().ok().and_then(|c| c.editor);
            let cmd = crate::assets::editor_command(editor.as_deref());
            let status = std::process::Command::new(&cmd[0])
                .args(&cmd[1..])
                .arg(&path)
                .status()
                .map_err(|e| anyhow::anyhow!("cannot run {}: {e}", cmd[0]))?;
            if !status.success() {
                anyhow::bail!("{} exited with {status}", cmd[0]);
            }
            report_one(&Catalog::load(&root, settings.as_deref()), &a.id)
        }
        Some("reset") => {
            let id = positional(0)
                .ok_or_else(|| anyhow::anyhow!("usage: agent-mux config reset <id>"))?;
            let a = catalog.find(id).map_err(|e| anyhow::anyhow!(e))?;
            catalog.reset(a).map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "{} {}",
                a.id,
                if a.source == Source::User {
                    "deleted"
                } else {
                    "reset to the built-in text"
                }
            );
            Ok(())
        }
        Some("new") => {
            let usage = "usage: agent-mux config new skill|loop-skill|loop-agent <name>";
            let kind = match positional(0).map(String::as_str) {
                Some("skill") => Kind::Skill,
                Some("loop-skill") => Kind::LoopSkill,
                Some("loop-agent") => Kind::LoopAgent,
                _ => anyhow::bail!("{usage}"),
            };
            let name = positional(1).ok_or_else(|| anyhow::anyhow!("{usage}"))?;
            let path = catalog
                .new_item(kind, name)
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("{}", path.display());
            Ok(())
        }
        Some("check") => {
            let mut bad = 0;
            for a in &catalog.assets {
                for p in &a.problems {
                    println!("{}: {p}", a.id);
                    bad += 1;
                }
            }
            if let Some(e) = &catalog.registry_error {
                println!("{e}");
            }
            if bad == 0 {
                println!(
                    "{} items, {} overridden or added, no problems",
                    catalog.assets.len(),
                    catalog
                        .assets
                        .iter()
                        .filter(|a| a.source != Source::Builtin)
                        .count()
                );
                Ok(())
            } else {
                std::process::exit(1)
            }
        }
        Some("push") => {
            let registry = crate::loops::registry::registry_path()
                .map(|p| crate::loops::registry::load(&p))
                .unwrap_or_default();
            let report = catalog.push(&registry, flag("--dry-run"));
            let verb = if flag("--dry-run") {
                "would write"
            } else {
                "wrote"
            };
            for p in &report.written {
                println!("{verb} {}", p.display());
            }
            println!(
                "{verb} {} file(s), {} unchanged, {} loop(s) skipped",
                report.written.len(),
                report.unchanged.len(),
                report.skipped_loops.len()
            );
            for e in &report.errors {
                eprintln!("{e}");
            }
            if report.errors.is_empty() {
                Ok(())
            } else {
                std::process::exit(2)
            }
        }
        Some("help") | Some("--help") | Some("-h") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown config command {other:?}\n\n{USAGE}"),
    }
}

/// Prints one item's source and status, for `edit`.
fn report_one(catalog: &Catalog, id: &str) -> anyhow::Result<()> {
    let Some(a) = catalog.get(id) else {
        println!("{id}: removed");
        return Ok(());
    };
    println!("{}  {}  {}", a.id, a.source.label(), a.path.display());
    if a.valid() {
        println!("valid");
        Ok(())
    } else {
        for p in &a.problems {
            println!("! {p}");
        }
        std::process::exit(1)
    }
}
