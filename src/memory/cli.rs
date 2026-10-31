//! `agent-mux memory status|install|uninstall|backfill`: the install flow
//! from the command line (`super::install`), and the import of the
//! history from before it (`super::backfill`). `memory hook …`, what an older
//! agent-mux put into a harness's settings, does nothing and succeeds.

use super::install::{self, Options, Step};
use super::{State, home, label};
use crate::harness::Harness;

pub const USAGE: &str = "agent-mux memory <command>

Shared memory for Claude Code, Codex and Antigravity through ai-memory
(https://github.com/akitaonrails/ai-memory).

  status [--json]              the binary, the server, and each harness's wiring
  install [--yes] [--harness claude|codex|agy]...
                               download ai-memory (macOS), run its server at
                               login, and wire the harnesses whose CLI is
                               installed (or the ones named); shows each step
                               and asks first unless --yes
  uninstall [--yes] [--purge-data]
                               remove ai-memory's hooks and MCP entries from
                               every harness and stop its server; --purge-data
                               also deletes the memory. The binary stays.
  backfill [--yes] [--force] [--max-sessions N] [<folder>...]
                               import the sessions from before ai-memory was
                               installed (Claude Code, Codex and Antigravity
                               transcripts) in every folder the trace store
                               has seen, or the ones named; shows what each
                               project gets and asks first unless --yes.
                               A project that already has captured sessions
                               is left alone unless --force (those may then
                               be stored twice); a folder is imported once.";

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().map(String::as_str) {
        Some("status") => status(rest),
        Some("install") => install_cmd(rest),
        Some("uninstall") => uninstall_cmd(rest),
        Some("backfill") => backfill_cmd(rest),
        // the hook an older agent-mux installed: silent
        Some("hook") => Ok(()),
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    }
}

fn status(args: &[String]) -> anyhow::Result<()> {
    let s = State::detect();
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&to_json(&s))?);
        return Ok(());
    }
    for line in status_lines(&s) {
        println!("{line}");
    }
    println!("{}", verdict(&s, "`agent-mux memory install`"));
    Ok(())
}

/// The state as JSON, for scripts.
pub fn to_json(s: &State) -> serde_json::Value {
    serde_json::json!({
        "binary": s.binary.as_ref().map(|b| serde_json::json!({
            "path": b.path, "version": b.version,
        })),
        "initialized": s.initialized,
        "server": s.server,
        "address": super::server_addr(),
        "launchd": s.launchd,
        "ready": s.ready(),
        "feed": s.feed,
        "harnesses": s.harnesses.iter().map(|w| serde_json::json!({
            "harness": w.harness.as_str(),
            "cli": w.cli, "mcp": w.mcp, "hooks": w.hooks,
        })).collect::<Vec<_>>(),
    })
}

/// One line per part of the setup, with a mark.
pub fn status_lines(s: &State) -> Vec<String> {
    let mark = |ok: bool| if ok { "✓" } else { "✗" };
    let mut out = vec![match &s.binary {
        Some(b) => format!(
            "✓ ai-memory   {} ({})",
            super::install::shown(&b.path),
            b.version.as_deref().unwrap_or("version unknown")
        ),
        None => "✗ ai-memory   not installed".into(),
    }];
    out.push(format!(
        "{} server      {} on {}{}",
        mark(s.server),
        if s.server { "running" } else { "not running" },
        super::server_addr(),
        if s.launchd { " · starts at login" } else { "" }
    ));
    for w in &s.harnesses {
        let line = if !w.cli && !w.mcp && !w.hooks {
            format!("– {:<11} CLI not installed", label(w.harness))
        } else {
            format!(
                "{} {:<11} MCP {} · hooks {}",
                mark(w.complete()),
                label(w.harness),
                mark(w.mcp),
                mark(w.hooks)
            )
        };
        out.push(line);
    }
    out.push(if s.feed {
        "✓ feed        agent-mux sends the sessions it launches from its traces".into()
    } else {
        format!(
            "– feed        off ({}=off): ai-memory's hooks capture",
            super::feed::ENV_SWITCH
        )
    });
    out
}

/// The verdict under the rows.
pub fn verdict(s: &State, how: &str) -> String {
    if s.ready() {
        "ready: every installed harness shares the memory".into()
    } else {
        format!("not ready: {how} sets up what is missing")
    }
}

fn parse_harness(word: &str) -> anyhow::Result<Harness> {
    match word {
        "claude" | "claude-code" => Ok(Harness::Claude),
        "codex" => Ok(Harness::Codex),
        "agy" | "antigravity" | "antigravity-cli" => Ok(Harness::Antigravity),
        "opencode" | "open-code" | "opencode2" => Ok(Harness::OpenCode),
        w => anyhow::bail!("unknown harness {w:?}: claude, codex, agy or opencode"),
    }
}

fn confirm(steps: &[Step]) -> anyhow::Result<bool> {
    use std::io::{BufRead, Write};
    println!("agent-mux will:");
    for (i, s) in steps.iter().enumerate() {
        println!("  {}. {}", i + 1, s.title);
        if !s.command.is_empty() {
            println!("     {}", s.command);
        }
    }
    print!("Go ahead? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn execute(steps: &[Step]) -> anyhow::Result<()> {
    let home = home();
    for (i, step) in steps.iter().enumerate() {
        println!("[{}/{}] {}", i + 1, steps.len(), step.title);
        install::run(step, &home, &mut |line| println!("  {line}"))
            .map_err(|e| anyhow::anyhow!("{}: {e}", step.title))?;
    }
    Ok(())
}

fn install_cmd(args: &[String]) -> anyhow::Result<()> {
    let mut yes = false;
    let mut only: Vec<Harness> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--yes" | "-y" => yes = true,
            "--harness" => only
                .push(parse_harness(it.next().ok_or_else(|| {
                    anyhow::anyhow!("--harness claude|codex|agy")
                })?)?),
            other => anyhow::bail!("unknown option {other:?}\n\n{USAGE}"),
        }
    }
    let state = State::detect();
    let opts = Options {
        only: (!only.is_empty()).then_some(only),
    };
    let steps = install::plan(&state, &home(), &opts).map_err(|e| anyhow::anyhow!(e))?;
    if steps.is_empty() {
        println!("Nothing to do.");
    } else {
        if !yes && !confirm(&steps)? {
            println!("Nothing changed.");
            return Ok(());
        }
        execute(&steps)?;
    }
    let after = State::detect();
    for line in status_lines(&after) {
        println!("{line}");
    }
    println!("{}", verdict(&after, "`agent-mux memory install`"));
    print_notes(&after);
    Ok(())
}

/// What the user still has to do by hand.
pub fn notes(s: &State) -> Vec<String> {
    let mut out = Vec::new();
    if s.wiring(Harness::Codex).is_some_and(|w| w.hooks) {
        out.push("Codex asks to review the new hooks at its next start: choose \"Trust all and continue\".".into());
    }
    if s.wiring(Harness::Antigravity).is_some_and(|w| w.hooks) {
        out.push("Antigravity has no session-end event: `ai-memory finalize-session --agent antigravity-cli` closes its last session.".into());
    }
    if s.harnesses.iter().any(|w| w.mcp) {
        out.push("Restart running Claude Code, Codex, Antigravity and OpenCode sessions so they load the MCP server.".into());
    }
    if s.server {
        out.push(format!(
            "The memory's web view: http://{}/web",
            super::server_addr()
        ));
    }
    out
}

fn print_notes(s: &State) {
    for n in notes(s) {
        println!("• {n}");
    }
}

fn uninstall_cmd(args: &[String]) -> anyhow::Result<()> {
    let mut yes = false;
    let mut purge = false;
    for a in args {
        match a.as_str() {
            "--yes" | "-y" => yes = true,
            "--purge-data" => purge = true,
            other => anyhow::bail!("unknown option {other:?}\n\n{USAGE}"),
        }
    }
    let state = State::detect();
    let steps = install::plan_uninstall(&state, &home(), purge);
    if steps.is_empty() {
        println!("ai-memory is not installed; nothing to remove.");
        return Ok(());
    }
    if !yes && !confirm(&steps)? {
        println!("Nothing changed.");
        return Ok(());
    }
    execute(&steps)
}

fn backfill_cmd(args: &[String]) -> anyhow::Result<()> {
    use super::backfill;
    use std::io::{BufRead, Write};
    let (mut yes, mut force, mut max) = (false, false, backfill::ALL);
    let mut named: Vec<std::path::PathBuf> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--yes" | "-y" => yes = true,
            "--force" => force = true,
            "--max-sessions" => {
                max = it
                    .next()
                    .and_then(|n| n.parse().ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| anyhow::anyhow!("--max-sessions <N>"))?
            }
            o if o.starts_with('-') => anyhow::bail!("unknown option {o:?}\n\n{USAGE}"),
            dir => {
                named.push(std::fs::canonicalize(dir).map_err(|e| anyhow::anyhow!("{dir}: {e}"))?)
            }
        }
    }
    let home = home();
    let state = State::detect();
    let Some(bin) = state.binary.as_ref().map(|b| b.path.clone()) else {
        anyhow::bail!("ai-memory is not installed: `agent-mux memory install` first");
    };
    if !state.server {
        anyhow::bail!(
            "ai-memory's server does not answer on {}: `agent-mux memory install` starts it",
            super::server_addr()
        );
    }
    let amx = crate::assets::root();
    let dirs = if named.is_empty() {
        let db = crate::tracing::analysis::default_trace_db_path();
        backfill::folders(&db, &amx).map_err(|e| anyhow::anyhow!(e))?
    } else {
        named
    };
    // an import stores every session again: a folder goes once
    let done = backfill::done(&amx);
    let (old, dirs): (Vec<_>, Vec<_>) = dirs.into_iter().partition(|d| done.contains(d));
    for d in &old {
        println!("already imported: {}", install::shown(d));
    }
    if dirs.is_empty() {
        println!("Nothing to import.");
        return Ok(());
    }
    println!("Looking at {} folder(s)…", dirs.len());
    let mut folders = Vec::new();
    for (dir, f) in backfill::plan(&bin, &dirs, max) {
        match f {
            Ok(f) => folders.push(f),
            Err(e) => println!("  skipped {}: {e}", install::shown(&dir)),
        }
    }
    for line in backfill::plan_lines(&folders, force, &home) {
        println!("{line}");
    }
    let todo: Vec<&backfill::Folder> = folders.iter().filter(|f| f.imported(force)).collect();
    let total: usize = todo.iter().map(|f| f.sessions).sum();
    if todo.is_empty() {
        println!("Nothing to import.");
        return Ok(());
    }
    if !yes {
        print!(
            "Import {total} session(s) from {} folder(s)? [y/N] ",
            todo.len()
        );
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("Nothing changed.");
            return Ok(());
        }
    }
    let (mut sessions, mut events, mut failed) = (0, 0, 0);
    for (i, f) in todo.iter().enumerate() {
        print!(
            "[{}/{}] {} ({}) … ",
            i + 1,
            todo.len(),
            install::shown(&f.dir),
            f.scope()
        );
        std::io::stdout().flush()?;
        match backfill::import(&bin, f, max) {
            Ok(r) => {
                if let Err(e) = backfill::mark_done(&amx, &f.dir) {
                    eprintln!("{}: {e}", backfill::done_file(&amx).display());
                }
                println!(
                    "{} session(s), {} event(s){}",
                    r.imported_sessions,
                    r.imported_events,
                    if r.failed_sessions > 0 {
                        format!(", {} failed", r.failed_sessions)
                    } else {
                        String::new()
                    }
                );
                sessions += r.imported_sessions;
                events += r.imported_events;
                failed += r.failed_sessions;
            }
            Err(e) => {
                println!("failed: {e}");
                failed += f.sessions;
            }
        }
    }
    println!("Imported {sessions} session(s), {events} event(s).");
    if failed > 0 {
        anyhow::bail!("{failed} session(s) were not imported");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Binary, Wiring};

    #[test]
    fn status_marks_each_part_and_says_what_is_next() {
        let s = State {
            binary: Some(Binary {
                path: "/x/ai-memory".into(),
                version: Some("ai-memory 2.6.7".into()),
            }),
            initialized: true,
            server: true,
            launchd: true,
            feed: true,
            harnesses: vec![
                Wiring {
                    harness: Harness::Claude,
                    cli: true,
                    mcp: true,
                    hooks: true,
                },
                Wiring {
                    harness: Harness::Codex,
                    cli: true,
                    mcp: true,
                    hooks: false,
                },
                Wiring {
                    harness: Harness::Antigravity,
                    cli: false,
                    mcp: false,
                    hooks: false,
                },
            ],
        };
        let lines = status_lines(&s);
        assert_eq!(lines[0], "✓ ai-memory   /x/ai-memory (ai-memory 2.6.7)");
        assert!(lines[1].starts_with("✓ server      running on "));
        assert!(lines[1].ends_with("· starts at login"));
        assert_eq!(lines[2], "✓ Claude Code MCP ✓ · hooks ✓");
        assert_eq!(lines[3], "✗ Codex       MCP ✓ · hooks ✗");
        assert_eq!(lines[4], "– Antigravity CLI not installed");
        assert!(verdict(&s, "i").starts_with("not ready: i sets up"));
        let j = to_json(&s);
        assert_eq!(j["harnesses"][1]["hooks"], false);
        assert_eq!(j["ready"], false);
        assert_eq!(parse_harness("agy").unwrap(), Harness::Antigravity);
        assert!(parse_harness("cursor").is_err());
    }
}
