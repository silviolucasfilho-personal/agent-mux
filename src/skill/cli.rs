//! `agent-mux skill <list|show|install|uninstall|status>`.

use super::install::{home_dir, install, status, uninstall};
use super::render::render_skill_md;
use super::{SkillDefinition, load_skills};
use crate::harness::Harness;

fn harness_args(args: &[String]) -> Result<Vec<Harness>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--harness" {
            let v = args
                .get(i + 1)
                .ok_or("--harness needs a value: claude, codex, agy or all")?;
            if v == "all" {
                out.extend(Harness::ALL);
            } else {
                out.push(v.parse::<Harness>()?);
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    Ok(out)
}

fn find(id: &str) -> Result<SkillDefinition, String> {
    let (skills, diagnostics) = load_skills(None);
    for d in &diagnostics {
        eprintln!("warning: {d}");
    }
    skills
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("skill '{id}' not found (compiled-in: heimdall; user packages: ~/.agent-mux/skills/<id>/SKILL.md)"))
}

pub fn handle_skill_cli(args: &[String]) -> Result<(), String> {
    let Some(cmd) = args.first().map(String::as_str) else {
        print_help();
        return Ok(());
    };
    match cmd {
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        "list" => {
            let (skills, diagnostics) = load_skills(None);
            for d in &diagnostics {
                eprintln!("warning: {d}");
            }
            for s in &skills {
                let origin = match &s.dir {
                    Some(d) => d.display().to_string(),
                    None => "compiled-in".to_string(),
                };
                println!(
                    "{:<12} {:<20} harnesses={} default={}  {}",
                    s.id,
                    s.name,
                    s.harnesses
                        .iter()
                        .map(|h| h.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                    s.default_harness.as_str(),
                    origin
                );
            }
            Ok(())
        }
        "show" => {
            let id = args
                .get(1)
                .ok_or("usage: agent-mux skill show <id> [--harness H]")?;
            let def = find(id)?;
            let harness = harness_args(&args[2..])?
                .first()
                .copied()
                .unwrap_or(def.default_harness);
            print!("{}", render_skill_md(&def, harness));
            Ok(())
        }
        "install" => {
            let id = args.get(1).ok_or(
                "usage: agent-mux skill install <id> [--harness claude|codex|agy|all] [--force]",
            )?;
            let def = find(id)?;
            let force = args.iter().any(|a| a == "--force");
            let mut targets = harness_args(&args[2..])?;
            if targets.is_empty() {
                targets = def.harnesses.clone();
            }
            let home = home_dir();
            for h in targets {
                if !def.harnesses.contains(&h) {
                    println!(
                        "{}: skill does not declare this harness, skipped",
                        h.as_str()
                    );
                    continue;
                }
                match install(&def, h, &home, force) {
                    Ok(r) if r.unchanged => {
                        println!("{}: up to date at {}", h.as_str(), r.dir.display())
                    }
                    Ok(r) => println!(
                        "{}: installed {} files at {}",
                        h.as_str(),
                        r.files.len(),
                        r.dir.display()
                    ),
                    Err(e) => return Err(format!("{}: {e}", h.as_str())),
                }
            }
            Ok(())
        }
        "uninstall" => {
            let id = args
                .get(1)
                .ok_or("usage: agent-mux skill uninstall <id> [--harness claude|codex|agy|all]")?;
            let mut targets = harness_args(&args[2..])?;
            if targets.is_empty() {
                targets = Harness::ALL.to_vec();
            }
            let home = home_dir();
            for h in targets {
                match uninstall(id, h, &home) {
                    Ok(true) => println!("{}: removed", h.as_str()),
                    Ok(false) => println!("{}: not installed", h.as_str()),
                    Err(e) => return Err(format!("{}: {e}", h.as_str())),
                }
            }
            Ok(())
        }
        "status" => {
            let (skills, _) = load_skills(None);
            let home = home_dir();
            let filter = args.get(1);
            for def in skills.iter().filter(|s| filter.is_none_or(|f| f == &s.id)) {
                for h in &def.harnesses {
                    let st = status(def, *h, &home);
                    let state = match (st.installed, st.managed, st.current) {
                        (false, _, _) => "not installed",
                        (true, false, _) => "present, not managed by agent-mux",
                        (true, true, true) => "installed, current",
                        (true, true, false) => "installed, stale",
                    };
                    println!(
                        "{:<10} {:<7} {:<34} {}",
                        def.id,
                        h.as_str(),
                        state,
                        st.dir.display()
                    );
                }
            }
            Ok(())
        }
        other => Err(format!(
            "unknown skill subcommand '{other}'; run `agent-mux skill help`"
        )),
    }
}

fn print_help() {
    println!(
        r#"usage: agent-mux skill <command> [args]

commands:
  list                                   skills agent-mux can launch (compiled-in and ~/.agent-mux/skills)
  show <id> [--harness H]                the SKILL.md as it is written for that harness
  install <id> [--harness H|all] [--force]
                                         write the skill into the harness skill directories:
                                         claude ~/.claude/skills, codex ~/.codex/skills, agy ~/.gemini/config/skills
  uninstall <id> [--harness H|all]       remove copies agent-mux installed
  status [id]                            installed / stale / not managed, per harness

The sidebar launcher installs the chosen skill for its harness before every launch."#
    );
}
