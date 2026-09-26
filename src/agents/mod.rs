//! Agents: a harness-neutral definition of *who* runs a workflow session,
//! so a step can say `agent = "reviewer"` and get the same persona on
//! Claude Code, Codex CLI or Antigravity. An agent is a TOML file
//! (`name`, `description`, `instructions`, canonical `tools`, `model`,
//! `effort`, `[backends.<harness>]` overrides) found in the workspace's
//! `.agent-mux/agents/`, then in the library's `agents/`.
//!
//! The workflow keeps owning order, context, budgets and isolation; the
//! agent only changes how each session is launched (`launch`) and what
//! `workflow check` says about the steps that name it (`fit`). The design
//! follows agent-builder's plan (`~/workspace/agent-builder/docs/plan.md`,
//! section 6), implemented natively so agent-mux depends on nothing new.
//! Guide: docs/agents.md.

pub mod cli;
pub mod fit;
pub mod launch;

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// A capability an agent may use, named the same on every harness and
/// mapped per harness by `launch`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tool {
    Read,
    Edit,
    Shell,
    Web,
    /// `mcp:<server>`: the tools of one MCP server.
    Mcp(String),
}

impl Tool {
    pub fn parse(text: &str) -> Result<Tool, String> {
        match text.trim() {
            "read" => Ok(Tool::Read),
            "edit" => Ok(Tool::Edit),
            "shell" => Ok(Tool::Shell),
            "web" => Ok(Tool::Web),
            other => match other.strip_prefix("mcp:") {
                Some(server) if is_valid_name(server) => Ok(Tool::Mcp(server.to_string())),
                Some(server) => Err(format!(
                    "mcp:{server}: the server name must match ^[a-z][a-z0-9_-]*$"
                )),
                None => Err(format!(
                    "unknown tool {other:?}; use read, edit, shell, web or mcp:<server>"
                )),
            },
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tool::Read => f.write_str("read"),
            Tool::Edit => f.write_str("edit"),
            Tool::Shell => f.write_str("shell"),
            Tool::Web => f.write_str("web"),
            Tool::Mcp(s) => write!(f, "mcp:{s}"),
        }
    }
}

/// `[backends.<harness>]`: what differs on one harness.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendOverride {
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgent {
    name: String,
    description: String,
    instructions: String,
    tools: Option<Vec<String>>,
    model: Option<String>,
    effort: Option<String>,
    #[serde(default)]
    backends: BTreeMap<String, BackendOverride>,
}

/// A parsed, validated agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpec {
    pub name: String,
    /// What the agent is for; shown in listings and the planner's catalog.
    pub description: String,
    /// The system prompt (Claude Code), developer instructions (Codex) or
    /// system prompt of the agent file (Antigravity).
    pub instructions: String,
    /// `None`: the harness's own tool set. `Some`: only these.
    pub tools: Option<Vec<Tool>>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub backends: BTreeMap<String, BackendOverride>,
    /// sha256 of the file's text: a changed agent reruns its sessions on
    /// resume.
    pub hash: String,
}

pub const HARNESSES: &[&str] = &["claude", "codex", "agy"];

pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

impl AgentSpec {
    /// Parses and validates; every problem is reported with how to fix it.
    pub fn parse(text: &str) -> Result<AgentSpec, Vec<String>> {
        let raw: RawAgent = toml::from_str(text).map_err(|e| vec![format!("agent: {e}")])?;
        let mut problems = Vec::new();
        if !is_valid_name(&raw.name) {
            problems.push(format!(
                "name {:?} must match ^[a-z][a-z0-9_-]*$ (a kebab-case word works on every harness)",
                raw.name
            ));
        }
        if raw.description.trim().is_empty() {
            problems.push("description is empty: say in one line what the agent is for".into());
        }
        if raw.instructions.trim().is_empty() {
            problems.push("instructions are empty: they become the agent's system prompt".into());
        }
        let tools = match raw.tools {
            None => None,
            Some(list) => {
                let mut out: Vec<Tool> = Vec::new();
                for t in list {
                    match Tool::parse(&t) {
                        Ok(tool) if !out.contains(&tool) => out.push(tool),
                        Ok(_) => {}
                        Err(e) => problems.push(format!("tools: {e}")),
                    }
                }
                Some(out)
            }
        };
        for h in raw.backends.keys() {
            if !HARNESSES.contains(&h.as_str()) {
                problems.push(format!(
                    "[backends.{h}]: unknown harness; use claude, codex or agy"
                ));
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        Ok(AgentSpec {
            name: raw.name,
            description: raw.description.trim().to_string(),
            instructions: raw.instructions.trim().to_string(),
            tools,
            model: raw.model.filter(|m| !m.trim().is_empty()),
            effort: raw.effort.filter(|e| !e.trim().is_empty()),
            backends: raw.backends,
            hash: hash_text(text),
        })
    }

    pub fn can(&self, tool: &Tool) -> bool {
        self.tools.as_ref().is_none_or(|t| t.contains(tool))
    }

    /// The model on `harness`: its `[backends]` entry, else `model`.
    pub fn model_for(&self, harness: &str) -> Option<String> {
        self.backends
            .get(harness)
            .and_then(|b| b.model.clone())
            .or_else(|| self.model.clone())
    }

    /// The reasoning effort on `harness`: its `[backends]` entry, else `effort`.
    pub fn effort_for(&self, harness: &str) -> Option<String> {
        self.backends
            .get(harness)
            .and_then(|b| b.effort.clone())
            .or_else(|| self.effort.clone())
    }

    /// `read, edit` or `all (the harness's own)`.
    pub fn tools_label(&self) -> String {
        match &self.tools {
            None => "all (the harness's own)".into(),
            Some(t) if t.is_empty() => "none".into(),
            Some(t) => t.iter().map(Tool::to_string).collect::<Vec<_>>().join(", "),
        }
    }

    /// The short hash shown in listings and journals.
    pub fn short_hash(&self) -> &str {
        &self.hash[..12.min(self.hash.len())]
    }
}

pub fn hash_text(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    crate::tracing::ids::hex(&h.finalize())
}

/// Where an agent file was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `<workspace>/.agent-mux/agents/<name>.toml`: travels with the repository.
    Workspace(PathBuf),
    /// `<library>/agents/<name>.toml`: the user's own, or their copy of a
    /// built-in.
    Library(PathBuf),
    /// Compiled in (`agents/builtin/<name>.toml`); the path is where an
    /// edited copy goes.
    Builtin(PathBuf),
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::Workspace(_) => "workspace",
            Source::Library(_) => "library",
            Source::Builtin(_) => "built-in",
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Source::Workspace(p) | Source::Library(p) | Source::Builtin(p) => p,
        }
    }
}

/// One agent file: parsed, or the problems that stop it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub source: Source,
    pub spec: Option<AgentSpec>,
    pub problems: Vec<String>,
}

/// `<library>/agents`.
pub fn library_dir(root: &Path) -> PathBuf {
    root.join("agents")
}

/// `<workspace>/.agent-mux/agents`.
pub fn workspace_dir(workspace: &Path) -> PathBuf {
    workspace.join(".agent-mux").join("agents")
}

fn read_dir(dir: &Path, source: fn(PathBuf) -> Source) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (spec, mut problems) = match std::fs::read_to_string(&path) {
                Ok(text) => match AgentSpec::parse(&text) {
                    Ok(s) => (Some(s), Vec::new()),
                    Err(p) => (None, p),
                },
                Err(e) => (None, vec![format!("{}: {e}", path.display())]),
            };
            if let Some(s) = &spec
                && s.name != name
            {
                problems.push(format!(
                    "name {:?} must equal the file name {name:?}",
                    s.name
                ));
            }
            Entry {
                name,
                source: source(path),
                spec: if problems.is_empty() { spec } else { None },
                problems,
            }
        })
        .collect()
}

/// Every agent: the workspace's first, then the library's, then the
/// built-in ones; an agent hides any later one of the same name, so a
/// library file named like a built-in is the user's edit of it.
pub fn load(root: &Path, workspace: Option<&Path>) -> Vec<Entry> {
    let mut out = match workspace {
        Some(w) => read_dir(&workspace_dir(w), Source::Workspace),
        None => Vec::new(),
    };
    for e in read_dir(&library_dir(root), Source::Library) {
        if !out.iter().any(|o| o.name == e.name) {
            out.push(e);
        }
    }
    for (name, text) in BUILTIN {
        if out.iter().any(|o| o.name == *name) {
            continue;
        }
        let (spec, problems) = match AgentSpec::parse(text) {
            Ok(s) => (Some(s), Vec::new()),
            Err(p) => (None, p),
        };
        out.push(Entry {
            name: name.to_string(),
            source: Source::Builtin(library_dir(root).join(format!("{name}.toml"))),
            spec,
            problems,
        });
    }
    out
}

/// The built-in agents, compiled in: every one is editable (a library
/// copy replaces it) and resettable (deleting the copy restores it).
pub const BUILTIN: &[(&str, &str)] = &[
    (
        "reviewer",
        include_str!("../../agents/builtin/reviewer.toml"),
    ),
    ("skeptic", include_str!("../../agents/builtin/skeptic.toml")),
    ("planner", include_str!("../../agents/builtin/planner.toml")),
    (
        "doc-writer",
        include_str!("../../agents/builtin/doc-writer.toml"),
    ),
    ("judge", include_str!("../../agents/builtin/judge.toml")),
];

pub fn builtin(name: &str) -> Option<&'static str> {
    BUILTIN.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// The text an edit of `entry` starts from: its file, or the built-in text.
pub fn text_of(entry: &Entry) -> Option<String> {
    match &entry.source {
        Source::Builtin(_) => builtin(&entry.name).map(str::to_string),
        other => std::fs::read_to_string(other.path()).ok(),
    }
}

/// The agents a run can use, by name.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub entries: Vec<Entry>,
}

impl Catalog {
    pub fn load(root: &Path, workspace: Option<&Path>) -> Catalog {
        Catalog {
            entries: load(root, workspace),
        }
    }

    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    pub fn get(&self, name: &str) -> Option<&AgentSpec> {
        self.entry(name).and_then(|e| e.spec.as_ref())
    }
}

// ---- templates -----------------------------------------------------------------

/// Starting points for `agent-mux agent new --template <name>`.
/// The blank one, then every built-in agent.
pub fn templates() -> Vec<(&'static str, &'static str)> {
    let mut v = vec![("blank", include_str!("../../agents/templates/blank.toml"))];
    v.extend(BUILTIN.iter().copied());
    v
}

/// A template's text with its `name` replaced by `name`; the built-in
/// header comment is dropped.
pub fn from_template(template: &str, name: &str) -> Option<String> {
    let text = templates().into_iter().find(|(t, _)| *t == template)?.1;
    let text = text
        .split_once("\nname = ")
        .filter(|(head, _)| head.contains("A built-in agent"))
        .map(|(_, rest)| format!("name = {rest}"))
        .unwrap_or_else(|| text.to_string());
    let text = text.as_str();
    let mut out = String::with_capacity(text.len());
    let mut done = false;
    for line in text.lines() {
        if !done && line.starts_with("name = ") {
            out.push_str(&format!("name = {:?}", name));
            done = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVIEWER: &str = r#"
name = "reviewer"
description = "Reviews a change for defects"
instructions = "You review code."
tools = ["read", "shell", "mcp:github"]
model = "claude-opus-5"
[backends.codex]
model = "gpt-5"
effort = "high"
"#;

    #[test]
    fn parses_and_resolves_per_harness() {
        let a = AgentSpec::parse(REVIEWER).unwrap();
        assert_eq!(
            a.tools,
            Some(vec![Tool::Read, Tool::Shell, Tool::Mcp("github".into())])
        );
        assert_eq!(a.model_for("claude").as_deref(), Some("claude-opus-5"));
        assert_eq!(a.model_for("codex").as_deref(), Some("gpt-5"));
        assert_eq!(a.effort_for("codex").as_deref(), Some("high"));
        assert_eq!(a.effort_for("claude"), None);
        assert!(a.can(&Tool::Read) && !a.can(&Tool::Edit));
        assert_eq!(a.tools_label(), "read, shell, mcp:github");
        assert_eq!(a.hash, hash_text(REVIEWER));
    }

    #[test]
    fn problems_say_how_to_fix_them() {
        let bad = r#"
name = "Bad Name"
description = ""
instructions = "x"
tools = ["read", "grep"]
[backends.gemini]
model = "pro"
"#;
        let p = AgentSpec::parse(bad).unwrap_err();
        assert_eq!(p.len(), 4, "{p:?}");
        assert!(p.iter().any(|x| x.contains("kebab-case")));
        assert!(
            p.iter()
                .any(|x| x.contains("read, edit, shell, web or mcp:"))
        );
        assert!(p.iter().any(|x| x.contains("[backends.gemini]")));
        assert!(AgentSpec::parse("name = \"x\"").is_err());
    }

    #[test]
    fn no_tools_means_the_harness_default() {
        let a =
            AgentSpec::parse("name = \"a\"\ndescription = \"d\"\ninstructions = \"i\"").unwrap();
        assert!(a.tools.is_none() && a.can(&Tool::Edit));
    }

    #[test]
    fn workspace_agents_hide_library_agents() {
        let root = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(library_dir(root.path())).unwrap();
        std::fs::create_dir_all(workspace_dir(ws.path())).unwrap();
        let write = |dir: PathBuf, name: &str, desc: &str| {
            std::fs::write(
                dir.join(format!("{name}.toml")),
                format!("name = \"{name}\"\ndescription = \"{desc}\"\ninstructions = \"i\""),
            )
            .unwrap();
        };
        write(library_dir(root.path()), "reviewer", "library");
        write(library_dir(root.path()), "planner", "library");
        write(workspace_dir(ws.path()), "reviewer", "workspace");
        std::fs::write(
            library_dir(root.path()).join("wrong.toml"),
            "name = \"other\"\ndescription = \"d\"\ninstructions = \"i\"",
        )
        .unwrap();
        let c = Catalog::load(root.path(), Some(ws.path()));
        assert_eq!(c.get("reviewer").unwrap().description, "workspace");
        assert_eq!(c.get("planner").unwrap().description, "library");
        assert!(c.get("wrong").is_none());
        assert!(c.entry("wrong").unwrap().problems[0].contains("file name"));
        let lib_only = Catalog::load(root.path(), None);
        assert_eq!(lib_only.get("reviewer").unwrap().description, "library");
    }

    #[test]
    fn every_template_parses_once_named() {
        for (t, _) in templates() {
            let text = from_template(t, "my-agent").unwrap();
            let a = AgentSpec::parse(&text).unwrap_or_else(|p| panic!("{t}: {p:?}"));
            assert_eq!(a.name, "my-agent");
        }
        assert!(from_template("nope", "x").is_none());
    }
}
