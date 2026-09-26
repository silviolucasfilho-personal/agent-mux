//! `agent-mux agent …`: create, list, inspect and check the agents that
//! workflow steps run as.

use super::launch::{self, Level};
use super::{AgentSpec, Catalog, Tool};
use crate::harness::Harness;
use std::path::PathBuf;

pub const USAGE: &str = "agent-mux agent <command>

  ls [--workspace DIR] [--json]    agents: the workspace's, then the library's
  show <name> [--workspace DIR]    the definition, and what it becomes on each harness
  new <name> [--template T] [--workspace DIR]
      [--description TEXT] [--instructions TEXT] [--tools read,edit,shell,web,mcp:<s>]
      [--model M] [--effort E]     write an agent; --workspace writes it into
                                   DIR/.agent-mux/agents (committed with the repo),
                                   otherwise into the library
  check [<name>] [--workspace DIR] validate agents; exit 1 on problems
  templates                        the starting points for `new`
  import <file> [--force]          copy a Claude-shaped agent file into the library
                                   as a loop agent (loops/agents/, docs/loops.md)

Agents live in ~/.agent-mux/agents/<name>.toml (AGENT_MUX_LIBRARY_DIR overrides)
and <workspace>/.agent-mux/agents/<name>.toml; a workflow step runs as one with
`agent = \"<name>\"` (also on verify and judge). docs/agents.md explains the format.";

struct Args {
    values: Vec<(String, String)>,
    flags: Vec<String>,
    positional: Vec<String>,
}

const VALUE_FLAGS: &[&str] = &[
    "--workspace",
    "--template",
    "--description",
    "--instructions",
    "--tools",
    "--model",
    "--effort",
];

impl Args {
    fn parse(raw: &[String]) -> Args {
        let mut a = Args {
            values: Vec::new(),
            flags: Vec::new(),
            positional: Vec::new(),
        };
        let mut i = 0;
        while i < raw.len() {
            let t = &raw[i];
            if VALUE_FLAGS.contains(&t.as_str())
                && let Some(v) = raw.get(i + 1)
            {
                a.values.push((t.clone(), v.clone()));
                i += 2;
                continue;
            }
            if let Some((k, v)) = t.split_once('=')
                && VALUE_FLAGS.contains(&k)
            {
                a.values.push((k.to_string(), v.to_string()));
            } else if t.starts_with("--") {
                a.flags.push(t.clone());
            } else {
                a.positional.push(t.clone());
            }
            i += 1;
        }
        a
    }

    fn value(&self, k: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    }

    fn flag(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f == k)
    }

    fn workspace(&self) -> Option<PathBuf> {
        self.value("--workspace").map(PathBuf::from)
    }
}

pub fn run(raw: &[String]) -> anyhow::Result<()> {
    let (cmd, rest) = match raw.split_first() {
        Some((c, r)) => (Some(c.as_str()), r),
        None => (None, &[][..]),
    };
    let args = Args::parse(rest);
    match cmd {
        Some("ls") => ls(&args),
        Some("show") => show(&args),
        Some("new") => new(&args),
        Some("check") => check(&args),
        Some("import") => crate::loops::agents::run_cli(raw).map_err(|e| anyhow::anyhow!(e)),
        Some("templates") => {
            for (name, text) in super::templates() {
                let desc = AgentSpec::parse(text)
                    .map(|a| a.description)
                    .unwrap_or_default();
                println!("{name:<12} {desc}");
            }
            Ok(())
        }
        Some("help") | Some("--help") | Some("-h") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown agent command {other:?}\n\n{USAGE}"),
    }
}

fn catalog(args: &Args) -> Catalog {
    Catalog::load(&crate::assets::root(), args.workspace().as_deref())
}

fn ls(args: &Args) -> anyhow::Result<()> {
    let cat = catalog(args);
    if args.flag("--json") {
        let items: Vec<serde_json::Value> = cat
            .entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "name": e.name,
                    "source": e.source.label(),
                    "path": e.source.path(),
                    "description": e.spec.as_ref().map(|s| s.description.clone()),
                    "tools": e.spec.as_ref().and_then(|s| s.tools.as_ref().map(|t| t.iter().map(Tool::to_string).collect::<Vec<_>>())),
                    "hash": e.spec.as_ref().map(|s| s.short_hash().to_string()),
                    "problems": e.problems,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    if cat.entries.is_empty() {
        println!(
            "no workflow agents yet; `agent-mux agent new <name> --template reviewer` writes one into {}",
            super::library_dir(&crate::assets::root()).display()
        );
    }
    for e in &cat.entries {
        match &e.spec {
            Some(s) => println!(
                "{:<18} {:<9} {}  [{}]",
                e.name,
                e.source.label(),
                s.description,
                s.tools_label()
            ),
            None => println!(
                "{:<18} {:<9} ! {}",
                e.name,
                e.source.label(),
                e.problems.first().cloned().unwrap_or_default()
            ),
        }
    }
    let loop_agents = crate::assets::loop_agents(&crate::assets::root());
    if !loop_agents.is_empty() {
        let names: Vec<String> = loop_agents.into_iter().map(|(n, _)| n).collect();
        println!(
            "\nloop agents (installed by the loop scaffolder): {}",
            names.join(", ")
        );
    }
    Ok(())
}

fn show(args: &Args) -> anyhow::Result<()> {
    let Some(name) = args.positional.first() else {
        anyhow::bail!("usage: agent-mux agent show <name>");
    };
    let cat = catalog(args);
    let Some(entry) = cat.entry(name) else {
        anyhow::bail!("no agent {name:?}; `agent-mux agent ls` lists them");
    };
    println!("{}  ({})", entry.name, entry.source.path().display());
    let Some(spec) = &entry.spec else {
        for p in &entry.problems {
            println!("  ! {p}");
        }
        std::process::exit(1);
    };
    println!("  {}", spec.description);
    println!("  tools   {}", spec.tools_label());
    println!("  hash    {}", spec.short_hash());
    println!();
    for h in [Harness::Claude, Harness::Codex, Harness::Antigravity] {
        let plan = launch::plan(spec, h);
        let mut line: Vec<String> = Vec::new();
        if let Some(m) = &plan.model {
            line.push(format!("--model {m}"));
        }
        if let Some(e) = &plan.effort {
            line.push(format!("effort {e}"));
        }
        for a in &plan.args {
            line.push(if a.len() > 60 {
                format!("'{}…'", &a[..a.floor_char_boundary(57)])
            } else {
                a.clone()
            });
        }
        if !plan.remove.is_empty() {
            line.push(format!("(replaces {})", plan.remove[0]));
        }
        println!("  {:<7} {}", h.as_str(), line.join(" "));
        if h == Harness::Antigravity {
            println!(
                "          writes {}",
                launch::agy_dir(&crate::skill::install::home_dir(), spec)
                    .join("agent.md")
                    .display()
            );
        }
        for (level, note) in launch::tool_notes(spec, h) {
            let mark = if level == Level::Error { "!" } else { "·" };
            println!("          {mark} {note}");
        }
    }
    Ok(())
}

/// A new agent's TOML from explicit fields.
pub fn render(
    name: &str,
    description: &str,
    instructions: &str,
    tools: Option<&[String]>,
    model: Option<&str>,
    effort: Option<&str>,
) -> String {
    let q = |s: &str| toml::Value::String(s.to_string()).to_string();
    // a literal block reads as written; one holding ''' is quoted instead
    let body = instructions.trim();
    let instructions = if body.contains("'''") {
        q(body)
    } else {
        format!("'''\n{body}\n'''")
    };
    let mut out = format!(
        "name = {}\ndescription = {}\ninstructions = {instructions}\n",
        q(name),
        q(description.trim()),
    );
    if let Some(t) = tools {
        let list: Vec<String> = t.iter().map(|x| q(x.trim())).collect();
        out.push_str(&format!("tools = [{}]\n", list.join(", ")));
    }
    if let Some(m) = model {
        out.push_str(&format!("model = {}\n", q(m)));
    }
    if let Some(e) = effort {
        out.push_str(&format!("effort = {}\n", q(e)));
    }
    out
}

fn new(args: &Args) -> anyhow::Result<()> {
    let Some(name) = args.positional.first() else {
        anyhow::bail!("usage: agent-mux agent new <name> [--template T]");
    };
    if !super::is_valid_name(name) {
        anyhow::bail!("{name:?}: an agent name must match ^[a-z][a-z0-9_-]*$");
    }
    let explicit = args.value("--description").is_some() || args.value("--instructions").is_some();
    let text = if explicit {
        let base = args
            .value("--template")
            .and_then(|t| super::from_template(t, name))
            .and_then(|t| AgentSpec::parse(&t).ok());
        let tools: Option<Vec<String>> = args
            .value("--tools")
            .map(|t| t.split(',').map(str::to_string).collect())
            .or_else(|| {
                base.as_ref()
                    .and_then(|b| b.tools.as_ref())
                    .map(|t| t.iter().map(Tool::to_string).collect())
            });
        render(
            name,
            args.value("--description")
                .or(base.as_ref().map(|b| b.description.as_str()))
                .unwrap_or(""),
            args.value("--instructions")
                .or(base.as_ref().map(|b| b.instructions.as_str()))
                .unwrap_or(""),
            tools.as_deref(),
            args.value("--model"),
            args.value("--effort"),
        )
    } else {
        let t = args.value("--template").unwrap_or("blank");
        super::from_template(t, name).ok_or_else(|| {
            anyhow::anyhow!("unknown template {t:?}; `agent-mux agent templates` lists them")
        })?
    };
    if let Err(p) = AgentSpec::parse(&text) {
        anyhow::bail!("the agent would not load:\n  {}", p.join("\n  "));
    }
    let dir = match args.workspace() {
        Some(w) => super::workspace_dir(&w),
        None => super::library_dir(&crate::assets::root()),
    };
    let path = dir.join(format!("{name}.toml"));
    if path.exists() {
        anyhow::bail!("{} already exists", path.display());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, text)?;
    println!("{}", path.display());
    if !explicit {
        println!("edit it, then `agent-mux agent check {name}`");
    }
    Ok(())
}

fn check(args: &Args) -> anyhow::Result<()> {
    let cat = catalog(args);
    let only = args.positional.first();
    let mut bad = 0;
    let mut seen = 0;
    for e in &cat.entries {
        if only.is_some_and(|n| n != &e.name) {
            continue;
        }
        seen += 1;
        if e.problems.is_empty() {
            println!("{}: ok", e.name);
        } else {
            for p in &e.problems {
                println!("{}: {p}", e.name);
                bad += 1;
            }
        }
    }
    if let Some(n) = only
        && seen == 0
    {
        anyhow::bail!("no agent {n:?}");
    }
    if bad > 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_agents_parse_back() {
        let text = render(
            "sec",
            "Security \"reviewer\"",
            "Look for \\ and \"\"\" injections.\nBe precise.",
            Some(&["read".into(), " shell".into()]),
            Some("gpt-5"),
            None,
        );
        let a = AgentSpec::parse(&text).unwrap();
        assert_eq!(a.description, "Security \"reviewer\"");
        assert_eq!(
            a.instructions,
            "Look for \\ and \"\"\" injections.\nBe precise."
        );
        let quoted = render("q", "d", "has \'\'\' inside", None, None, None);
        assert_eq!(
            AgentSpec::parse(&quoted).unwrap().instructions,
            "has \'\'\' inside"
        );
        assert_eq!(a.tools, Some(vec![Tool::Read, Tool::Shell]));
        assert_eq!(a.model.as_deref(), Some("gpt-5"));
    }
}
