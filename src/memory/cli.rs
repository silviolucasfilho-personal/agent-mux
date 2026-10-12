//! `agent-mux memory …`: the workspace's memory from the shell, and the
//! command the `mux-memory` skill runs after it saves a note.

use super::{Settings, context, index, log, note, paths};
use crate::prompts::Prompts;
use std::io::IsTerminal;
use std::path::PathBuf;

pub const USAGE: &str = "agent-mux memory <command>

  ls [--all] [--json]          the notes in index order (--all: past the index cap too)
  show <name>                  a note's file
  add <name> --description D [--kind K] [--body TEXT] [--pinned] [--expires YYYY-MM-DD]
         [--confidence stated|observed] [--harness H] [--launch ID] [--force]
                               write a note (the body from stdin when --body is absent)
  rm <name>                    move a note to notes/.removed/ (nothing is deleted)
  check                        validate every note, expire old ones, rebuild the index;
                               exit 1 when a note has problems
  path                         which folder is in use, and why
  save-session <job>           run one save on close (agent-mux starts it; docs/memory.md)
  install [claude|codex|agy]   give sessions started outside agent-mux the memory: a start
                               hook in each harness's own settings (default: all three)
  uninstall [claude|codex|agy] remove those hooks
  status                       which harnesses have the hook
  hook <claude|codex|agy>      the hook itself: a harness's payload on stdin

Every command takes --workspace DIR (default: $AGENT_MUX_MEMORY inside a session,
else the current folder) or --user (the facts that hold in every workspace).
Kinds: user, feedback, decision, project, reference, handoff. docs/memory.md
explains the folder.";

const BOOL_FLAGS: &[&str] = &["all", "json", "user", "pinned", "force", "help"];

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(raw: &[String]) -> Args {
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let a = &raw[i];
            if let Some(name) = a.strip_prefix("--") {
                if let Some((k, v)) = name.split_once('=') {
                    flags.push((k.to_string(), Some(v.to_string())));
                } else if BOOL_FLAGS.contains(&name) {
                    flags.push((name.to_string(), None));
                } else {
                    flags.push((name.to_string(), raw.get(i + 1).cloned()));
                    i += 1;
                }
            } else if a == "-h" {
                flags.push(("help".into(), None));
            } else {
                positional.push(a.clone());
            }
            i += 1;
        }
        Args { positional, flags }
    }

    fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|(f, _)| f == k)
    }

    fn value(&self, k: &str) -> Option<&str> {
        self.flags
            .iter()
            .find(|(f, _)| f == k)
            .and_then(|(_, v)| v.as_deref())
    }
}

/// The folder a command works on, and why.
struct Target {
    dir: PathBuf,
    why: String,
    user_dir: PathBuf,
    is_user: bool,
    settings: Settings,
    problems: Vec<String>,
}

fn target(args: &Args) -> anyhow::Result<Target> {
    let cfg = crate::config::load().ok();
    let mem_cfg = cfg.as_ref().and_then(|c| c.memory.as_ref());
    let settings = Settings::from_config(mem_cfg);
    let problems = Settings::problems(mem_cfg);
    let library = crate::assets::root();
    let user_dir = paths::user_dir(&library);
    let env_dir = std::env::var_os(super::ENV_DIR)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let (dir, why, is_user) = if args.has("user") {
        (user_dir.clone(), "--user".to_string(), true)
    } else if let Some(ws) = args.value("workspace") {
        let ws = PathBuf::from(ws);
        if !ws.is_dir() {
            anyhow::bail!("workspace {} is not a folder", ws.display());
        }
        (
            paths::dir_for(&library, &settings, &ws),
            format!("--workspace, location = {}", settings.location.as_str()),
            false,
        )
    } else if let Some(d) = env_dir {
        (
            d,
            format!("${} (this session's memory)", super::ENV_DIR),
            false,
        )
    } else {
        let cwd = std::env::current_dir()?;
        (
            paths::dir_for(&library, &settings, &cwd),
            format!(
                "the current folder, location = {}",
                settings.location.as_str()
            ),
            false,
        )
    };
    Ok(Target {
        dir,
        why,
        user_dir,
        is_user,
        settings,
        problems,
    })
}

fn rebuild(t: &Target, prompts: &Prompts) -> std::io::Result<()> {
    paths::ensure(&t.dir)?;
    index::rebuild(&t.dir, t.settings.index_lines, &prompts.memory_index_footer)?;
    Ok(())
}

fn launch_id(args: &Args) -> Option<String> {
    args.value("launch")
        .map(str::to_string)
        .or_else(|| std::env::var("AGENT_MUX_SESSION_ID").ok())
        .filter(|s| !s.is_empty())
}

pub fn run(raw: &[String]) -> anyhow::Result<()> {
    let args = Args::parse(raw.get(1..).unwrap_or_default());
    let cmd = raw.first().map(String::as_str);
    if matches!(cmd, None | Some("help") | Some("--help") | Some("-h")) || args.has("help") {
        println!("{USAGE}");
        return Ok(());
    }
    match cmd {
        Some("hook") => {
            return super::hook::run(args.positional.first().map(String::as_str).unwrap_or(""));
        }
        Some("install") | Some("uninstall") | Some("status") => {
            return hooks_cmd(cmd.unwrap_or_default(), &args);
        }
        _ => {}
    }
    if cmd == Some("save-session") {
        let job = args
            .positional
            .first()
            .ok_or_else(|| anyhow::anyhow!("a job file is needed\n\n{USAGE}"))?;
        return super::save::run(std::path::Path::new(job));
    }
    let t = target(&args)?;
    let prompts = Prompts::current();
    let name_arg = || {
        args.positional
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("a note name is needed\n\n{USAGE}"))
    };
    match cmd {
        Some("ls") | Some("list") => ls(&t, &args),
        Some("show") => {
            let name = name_arg()?;
            let path = paths::note_path(&t.dir, &name);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            print!("{text}");
            Ok(())
        }
        Some("add") => add(&t, &args, &prompts, &name_arg()?),
        Some("rm") | Some("remove") => {
            let name = name_arg()?;
            let moved = note::shelve(&t.dir, &name, paths::REMOVED)
                .map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
            rebuild(&t, &prompts)?;
            log::append(
                &t.dir,
                "user",
                launch_id(&args).as_deref(),
                "rm",
                &format!("notes/{name}.md"),
            );
            println!("moved to {}", moved.display());
            Ok(())
        }
        Some("check") => check(&t, &prompts),
        Some("path") => {
            println!("memory   {}", t.dir.display());
            println!("why      {}", t.why);
            println!("index    {}", paths::index_path(&t.dir).display());
            if !t.is_user {
                println!("user     {}", t.user_dir.display());
            }
            for p in &t.problems {
                println!("! {p}");
            }
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown memory command: {other}\n\n{USAGE}"),
        None => unreachable!(),
    }
}

fn ls(t: &Target, args: &Args) -> anyhow::Result<()> {
    let (notes, problems) = note::load_all(&t.dir);
    let total = notes.len();
    let cap = if args.has("all") {
        usize::MAX
    } else {
        t.settings.index_lines
    };
    let (listed, left_out) = index::select(notes.clone(), cap);
    if args.has("json") {
        let listed_names: Vec<&str> = listed.iter().map(|n| n.name.as_str()).collect();
        let (all, _) = index::select(notes, usize::MAX);
        let items: Vec<serde_json::Value> = all
            .iter()
            .map(|n| {
                serde_json::json!({
                    "name": n.name,
                    "description": n.description,
                    "kind": n.kind.as_str(),
                    "confidence": n.confidence.as_str(),
                    "pinned": n.pinned,
                    "expires_at": n.expires_at,
                    "source": {
                        "harness": n.source.harness,
                        "launch": n.source.launch,
                        "session": n.source.session,
                        "when": n.source.when,
                    },
                    "path": paths::note_path(&t.dir, &n.name),
                    "in_index": listed_names.contains(&n.name.as_str()),
                })
            })
            .collect();
        let bad: Vec<serde_json::Value> = problems
            .iter()
            .map(|(p, e)| serde_json::json!({"path": p, "problem": e}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "memory": t.dir,
                "notes": items,
                "problems": bad,
            }))?
        );
        return Ok(());
    }
    println!("memory  {}  ({total} notes)", t.dir.display());
    for n in &listed {
        println!(
            "  {:<9} {:<9} {:<32} {}",
            n.kind.as_str(),
            n.confidence.as_str(),
            n.name,
            n.description
        );
    }
    if left_out > 0 {
        println!("  {left_out} more past the index cap: --all lists them");
    }
    for (p, e) in &problems {
        println!("  ! {}: {e}", p.display());
    }
    Ok(())
}

fn add(t: &Target, args: &Args, prompts: &Prompts, name: &str) -> anyhow::Result<()> {
    if !note::valid_name(name) {
        anyhow::bail!(
            "{name:?}: a note name is lower case letters, digits, - and _ (try {:?})",
            note::slug(name)
        );
    }
    let description = args
        .value("description")
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("--description is needed: one line saying what the note holds")
        })?;
    let kind = match args.value("kind") {
        None => note::Kind::Project,
        Some(k) => note::Kind::parse(k).ok_or_else(|| {
            anyhow::anyhow!(
                "--kind {k:?}: one of user, feedback, decision, project, reference, handoff"
            )
        })?,
    };
    let confidence = match args.value("confidence") {
        None => note::Confidence::Stated,
        Some(c) => note::Confidence::parse(c)
            .ok_or_else(|| anyhow::anyhow!("--confidence {c:?}: stated or observed"))?,
    };
    let expires_at = args.value("expires").map(str::to_string);
    let body = match args.value("body") {
        Some(b) => b.to_string(),
        None if !std::io::stdin().is_terminal() => {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
            s
        }
        None => anyhow::bail!("give the note's text with --body TEXT or on stdin"),
    };
    let path = paths::note_path(&t.dir, name);
    if path.exists() && !args.has("force") {
        anyhow::bail!("{} exists; --force replaces it", path.display());
    }
    let n = note::Note {
        name: name.to_string(),
        description: description.to_string(),
        kind,
        source: note::Source {
            harness: args.value("harness").unwrap_or("user").to_string(),
            launch: launch_id(args),
            session: None,
            when: Some(super::now_rfc3339()),
        },
        confidence,
        pinned: args.has("pinned"),
        expires_at,
        body,
    };
    // the written text must read back: an invalid date is refused here
    note::parse(&note::render(&n), name).map_err(|e| anyhow::anyhow!(e))?;
    paths::ensure(&t.dir)?;
    let path = note::write(&t.dir, &n)?;
    rebuild(t, prompts)?;
    log::append(
        &t.dir,
        &n.source.harness,
        n.source.launch.as_deref(),
        "write",
        &format!("notes/{name}.md"),
    );
    println!("{}", path.display());
    Ok(())
}

fn check(t: &Target, prompts: &Prompts) -> anyhow::Result<()> {
    let mut bad = t.problems.len();
    for p in &t.problems {
        println!("! {p}");
    }
    let dirs: Vec<PathBuf> = if t.is_user {
        vec![t.dir.clone()]
    } else {
        vec![t.dir.clone(), t.user_dir.clone()]
    };
    let today = super::today();
    for dir in dirs.iter().filter(|d| d.is_dir() || **d == t.dir) {
        paths::ensure(dir)?;
        let expired = index::expire(dir, &today);
        let (notes, problems) = note::load_all(dir);
        for (p, e) in &problems {
            println!("{}: {e}", p.display());
        }
        bad += problems.len();
        index::rebuild(dir, t.settings.index_lines, &prompts.memory_index_footer)?;
        let (listed, _) = index::select(notes.clone(), t.settings.index_lines);
        println!(
            "{}: {} notes, {} in the index, {} expired, {}",
            dir.display(),
            notes.len(),
            listed.len(),
            expired.len(),
            if problems.is_empty() {
                "no problems".to_string()
            } else {
                format!("{} with problems", problems.len())
            }
        );
    }
    // the start text a session would be given now, as a size check
    let text = context::session_context(
        &context::Inputs {
            dir: &t.dir,
            user_dir: (!t.is_user).then_some(t.user_dir.as_path()),
            brief: None,
            invocation: "/mux-memory",
        },
        prompts,
    );
    println!(
        "start text {} characters (limit {})",
        text.chars().count(),
        context::LIMIT
    );
    if bad > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// `install`, `uninstall` and `status` of the user-level start hooks.
fn hooks_cmd(cmd: &str, args: &Args) -> anyhow::Result<()> {
    use super::hook;
    use crate::harness::Harness;
    let home = crate::skill::install::home_dir();
    let all = [Harness::Claude, Harness::Codex, Harness::Antigravity];
    let picked: Vec<Harness> = match args.positional.first().map(String::as_str) {
        None | Some("all") => all.to_vec(),
        Some(w) => vec![
            hook::parse_harness(w)
                .ok_or_else(|| anyhow::anyhow!("{w}: claude, codex, agy or all\n\n{USAGE}"))?,
        ],
    };
    if cmd == "status" {
        let exe = crate::tracing::hooks::register::current_exe();
        for h in all {
            match hook::installed(&home, h) {
                Some(p) if Some(&p) == exe.as_ref() => println!("{:<7} installed", h.as_str()),
                Some(p) => println!(
                    "{:<7} installed, runs {} (not this binary: install again)",
                    h.as_str(),
                    p.display()
                ),
                None => println!("{:<7} not installed", h.as_str()),
            }
        }
        return Ok(());
    }
    let exe = if cmd == "install" {
        Some(
            crate::tracing::hooks::register::current_exe()
                .ok_or_else(|| anyhow::anyhow!("cannot find this binary's path"))?,
        )
    } else {
        None
    };
    for h in picked {
        let (path, changed) =
            hook::set(&home, h, exe.as_deref()).map_err(|e| anyhow::anyhow!(e))?;
        let what = match (cmd, changed) {
            ("install", true) => "installed",
            ("install", false) => "already installed",
            (_, true) => "removed",
            _ => "not installed",
        };
        println!("{:<7} {what}: {}", h.as_str(), path.display());
        if cmd == "install" && h == Harness::Codex {
            println!(
                "        Codex runs it only once you trust it: open Codex, run /hooks and trust the agent-mux memory entry."
            );
        }
    }
    Ok(())
}
