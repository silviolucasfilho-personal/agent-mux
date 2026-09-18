//! Skills: `SKILL.md` packages agent-mux lists in the sidebar, installs into
//! each harness's own skill directory in the syntax that harness reads, and
//! launches a session around.
//!
//! A package is a directory:
//!
//! ```text
//! <id>/
//! ├── SKILL.md          harness-neutral skill (frontmatter `name`, `description`)
//! ├── skill.toml        agent-mux metadata: icon, harnesses, default, startup prompt
//! └── reference/*.md    files installed next to SKILL.md, read on demand
//! ```
//!
//! Heimdall ships compiled into the binary; a package with the same id under
//! `~/.agent-mux/skills/` shadows it.

pub mod cli;
pub mod install;
pub mod launch;
pub mod render;

use crate::harness::Harness;
use crate::tracing::inventory::parse_frontmatter;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The compiled-in Heimdall package, byte for byte the repository's `skills/heimdall`.
pub const BUILTIN_HEIMDALL_SKILL: &str = include_str!("../../skills/heimdall/SKILL.md");
pub const BUILTIN_HEIMDALL_TOML: &str = include_str!("../../skills/heimdall/skill.toml");
const BUILTIN_HEIMDALL_FILES: &[(&str, &str)] = &[
    (
        "reference/sessions.md",
        include_str!("../../skills/heimdall/reference/sessions.md"),
    ),
    (
        "reference/skills.md",
        include_str!("../../skills/heimdall/reference/skills.md"),
    ),
    (
        "reference/agents.md",
        include_str!("../../skills/heimdall/reference/agents.md"),
    ),
];

/// One compiled-in package: its id, where it lives in the repository,
/// and every file as (path inside the package, text).
#[derive(Debug, Clone, Copy)]
pub struct BuiltinPackage {
    pub id: &'static str,
    pub repo_dir: &'static str,
    pub skill_md: &'static str,
    pub skill_toml: Option<&'static str>,
    pub files: &'static [(&'static str, &'static str)],
}

impl BuiltinPackage {
    /// SKILL.md, skill.toml and the reference files, in that order.
    pub fn all_files(&self) -> Vec<(&'static str, &'static str)> {
        let mut v = vec![("SKILL.md", self.skill_md)];
        if let Some(t) = self.skill_toml {
            v.push(("skill.toml", t));
        }
        v.extend(self.files.iter().copied());
        v
    }
}

macro_rules! wf_skill {
    ($name:literal) => {
        BuiltinPackage {
            id: $name,
            repo_dir: concat!("workflows/skills/", $name, "/"),
            skill_md: include_str!(concat!("../../workflows/skills/", $name, "/SKILL.md")),
            skill_toml: Some(include_str!(concat!(
                "../../workflows/skills/",
                $name,
                "/skill.toml"
            ))),
            files: &[],
        }
    };
}

/// Every compiled-in package: Heimdall, the workflow planner and the
/// workflow step skills.
pub fn builtin_packages() -> Vec<BuiltinPackage> {
    vec![
        BuiltinPackage {
            id: "heimdall",
            repo_dir: "skills/heimdall/",
            skill_md: BUILTIN_HEIMDALL_SKILL,
            skill_toml: Some(BUILTIN_HEIMDALL_TOML),
            files: BUILTIN_HEIMDALL_FILES,
        },
        BuiltinPackage {
            id: "workflow-author",
            repo_dir: "skills/workflow-author/",
            skill_md: include_str!("../../skills/workflow-author/SKILL.md"),
            skill_toml: Some(include_str!("../../skills/workflow-author/skill.toml")),
            files: &[
                (
                    "reference/document.md",
                    include_str!("../../skills/workflow-author/reference/document.md"),
                ),
                (
                    "reference/patterns.md",
                    include_str!("../../skills/workflow-author/reference/patterns.md"),
                ),
            ],
        },
        wf_skill!("wf-review-find"),
        wf_skill!("wf-refute"),
        wf_skill!("wf-synthesize"),
        wf_skill!("wf-read-map"),
        wf_skill!("wf-search"),
        wf_skill!("wf-deep-read"),
        wf_skill!("wf-critic"),
        wf_skill!("wf-find"),
        wf_skill!("wf-attempt"),
        wf_skill!("wf-judge"),
        wf_skill!("wf-discover-sites"),
        wf_skill!("wf-transform"),
        wf_skill!("wf-verify-site"),
        wf_skill!("wf-classify"),
        wf_skill!("wf-triage-bug"),
        wf_skill!("wf-triage-feature"),
    ]
}

/// Every compiled-in Heimdall file as (path inside the package, text).
pub fn builtin_files() -> Vec<(&'static str, &'static str)> {
    builtin_packages()[0].all_files()
}

/// Checks a `skill.toml` text alone (shape and harness names).
pub fn parse_skill_toml(text: &str) -> Result<(), String> {
    let meta: SkillMeta = toml::from_str(text).map_err(|e| e.to_string())?;
    for h in meta.harnesses.iter().chain(meta.default_harness.iter()) {
        let _: Harness = h.parse()?;
    }
    Ok(())
}

/// A snapshot agent-mux computes in Rust and hands to the agent at launch
/// (`[agent] hydrate` in skill.toml).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hydration {
    /// `Request::Briefing` scoped to the launch workspace, last 24 hours.
    Briefing,
    /// Schema-v2 dossier containing sessions, skills, agents, and health.
    Dossier,
}

impl Hydration {
    pub const ALL: [Hydration; 2] = [Hydration::Briefing, Hydration::Dossier];

    pub fn as_str(self) -> &'static str {
        match self {
            Hydration::Briefing => "briefing",
            Hydration::Dossier => "dossier",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "briefing" => Some(Hydration::Briefing),
            "dossier" => Some(Hydration::Dossier),
            _ => None,
        }
    }
}

/// Whether a launch registers the agent-mux MCP server for the agent
/// (`[agent] mcp` in skill.toml).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpMode {
    /// Register per launch where the harness allows, else use an installed
    /// entry, else run without.
    Auto,
    #[default]
    Off,
}

impl McpMode {
    pub fn as_str(self) -> &'static str {
        match self {
            McpMode::Auto => "auto",
            McpMode::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "auto" => Some(McpMode::Auto),
            "off" => Some(McpMode::Off),
            _ => None,
        }
    }
}

/// A skill package as agent-mux sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDefinition {
    /// Frontmatter `name`; also the directory name every harness expects.
    pub id: String,
    /// Display name in the sidebar (metadata `name`, else the id capitalized).
    pub name: String,
    pub description: String,
    pub icon: Option<String>,
    pub harnesses: Vec<Harness>,
    pub default_harness: Harness,
    /// `trace.read` switches the sidebar preview to the telemetry dashboard.
    pub capabilities: Vec<String>,
    /// Text appended to the skill invocation when a session is launched.
    pub startup_prompt: Option<String>,
    /// Snapshots written before launch; requires `trace.read`.
    pub hydrate: Vec<Hydration>,
    /// MCP registration policy; `Auto` by default when `trace.read` is
    /// declared, `Off` otherwise.
    pub mcp: McpMode,
    /// Non-fatal package problems (reported by `skill list`).
    pub warnings: Vec<String>,
    /// Markdown after the frontmatter.
    pub body: String,
    /// Extra files installed next to SKILL.md, as (relative path, content).
    pub files: Vec<(String, String)>,
    /// SHA-256 over SKILL.md, skill.toml and every extra file.
    pub source_hash: String,
    /// Package directory on disk; `None` for the compiled-in package.
    pub dir: Option<PathBuf>,
    pub is_builtin: bool,
    /// `hidden = true`: not listed in the Agents sidebar (workflow step
    /// skills and the planner); still installable and launchable.
    pub hidden: bool,
    /// `writes = true`: the skill edits files; a workflow step running it
    /// must be isolated.
    pub writes: bool,
    /// `auto_approve = true`: the session launches with every tool
    /// permission pre-granted, whichever harness runs it. The package asks
    /// for this, so a base profile that does not bypass approvals cannot
    /// leave the skill waiting on a prompt nobody is watching.
    pub auto_approve: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillError {
    pub path: PathBuf,
    pub message: String,
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for SkillError {}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillMeta {
    name: Option<String>,
    icon: Option<String>,
    #[serde(default)]
    harnesses: Vec<String>,
    default_harness: Option<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    startup_prompt: Option<String>,
    agent: Option<AgentMeta>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    writes: bool,
    #[serde(default)]
    auto_approve: bool,
}

/// `[agent]` in skill.toml: what Rust prepares for the agent at launch.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentMeta {
    #[serde(default)]
    hydrate: Vec<String>,
    mcp: Option<String>,
}

/// Skill ids double as directory names in every harness, so they stay
/// lowercase and simple: `^[a-z0-9][a-z0-9_-]*$`.
pub fn is_valid_skill_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn capitalize(s: &str) -> String {
    s.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Splits a SKILL.md into its frontmatter fields and body.
fn split_frontmatter(source: &str) -> Option<(Vec<(String, String)>, String)> {
    let fields = parse_frontmatter(source)?;
    let text = source.trim_start_matches('\u{feff}');
    let mut lines = text.lines();
    lines.next()?; // opening ---
    let mut body_start = 0;
    let mut offset = text.lines().next().map(|l| l.len() + 1).unwrap_or(0);
    for line in lines {
        let len = line.len() + 1;
        if line.trim_end() == "---" {
            body_start = offset + len;
            break;
        }
        offset += len;
    }
    let body = text
        .get(body_start..)
        .unwrap_or("")
        .trim_start_matches('\n');
    Some((fields, body.to_string()))
}

/// Parses one package from its pieces. `dir` names the package on disk; the
/// compiled-in package passes `None`.
pub fn parse_skill(
    source: &str,
    meta_toml: Option<&str>,
    files: Vec<(String, String)>,
    dir: Option<&Path>,
) -> Result<SkillDefinition, SkillError> {
    let path = dir
        .map(|d| d.join("SKILL.md"))
        .unwrap_or_else(|| PathBuf::from("<builtin>/SKILL.md"));
    let err = |m: String| SkillError {
        path: path.clone(),
        message: m,
    };

    let (fields, body) = split_frontmatter(source)
        .ok_or_else(|| err("missing `---` frontmatter block with name and description".into()))?;
    let field = |k: &str| {
        fields
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.trim().to_string())
    };
    let fallback_id = dir
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .map(str::to_string);
    let id = field("name")
        .filter(|s| !s.is_empty())
        .or(fallback_id)
        .ok_or_else(|| err("frontmatter `name` is required".into()))?;
    if !is_valid_skill_id(&id) {
        return Err(err(format!(
            "invalid skill name {id:?}: must match ^[a-z0-9][a-z0-9_-]*$"
        )));
    }
    if let Some(d) = dir
        && d.file_name().and_then(|n| n.to_str()) != Some(id.as_str())
    {
        return Err(err(format!(
            "frontmatter name {id:?} must equal the package directory name; every harness matches the two"
        )));
    }
    let description = field("description").unwrap_or_default();
    if description.is_empty() {
        return Err(err("frontmatter `description` must not be empty; it is what tells the model when to use the skill".into()));
    }
    if description.contains(": ") || description.contains(" #") {
        return Err(err("description must not contain `: ` or ` #`; keep it a plain YAML scalar so every harness parses it".into()));
    }
    if body.trim().is_empty() {
        return Err(err("SKILL.md body must not be empty".into()));
    }

    let meta: SkillMeta = match meta_toml {
        Some(t) => toml::from_str(t).map_err(|e| SkillError {
            path: dir
                .map(|d| d.join("skill.toml"))
                .unwrap_or_else(|| PathBuf::from("<builtin>/skill.toml")),
            message: format!("invalid skill.toml: {e}"),
        })?,
        None => SkillMeta::default(),
    };
    let harnesses: Vec<Harness> = if meta.harnesses.is_empty() {
        Harness::ALL.to_vec()
    } else {
        let mut out = Vec::new();
        for h in &meta.harnesses {
            let parsed: Harness = h.parse().map_err(|e: String| err(e))?;
            if !out.contains(&parsed) {
                out.push(parsed);
            }
        }
        out
    };
    let default_harness = match meta.default_harness {
        Some(h) => {
            let parsed: Harness = h.parse().map_err(|e: String| err(e))?;
            if !harnesses.contains(&parsed) {
                return Err(err(format!(
                    "default_harness {parsed} is not listed in harnesses"
                )));
            }
            parsed
        }
        None => harnesses[0],
    };

    let reads_traces = meta.capabilities.iter().any(|c| c == "trace.read");
    let agent = meta.agent.unwrap_or_default();
    let mut hydrate = Vec::new();
    for name in &agent.hydrate {
        let Some(h) = Hydration::parse(name) else {
            return Err(err(format!(
                "unknown [agent] hydrate entry {name:?}; known: {}",
                Hydration::ALL
                    .iter()
                    .map(|h| h.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        };
        if !hydrate.contains(&h) {
            hydrate.push(h);
        }
    }
    if hydrate.contains(&Hydration::Briefing) && hydrate.contains(&Hydration::Dossier) {
        return Err(err(
            "`briefing` and `dossier` hydration are mutually exclusive; choose one".into(),
        ));
    }
    let mcp = match agent.mcp.as_deref() {
        Some(m) => McpMode::parse(m).ok_or_else(|| {
            err(format!(
                "[agent] mcp must be \"auto\" or \"off\", not {m:?}"
            ))
        })?,
        None if reads_traces => McpMode::Auto,
        None => McpMode::Off,
    };
    let mut warnings = Vec::new();
    let (hydrate, mcp) = if !reads_traces && (!hydrate.is_empty() || agent.mcp.is_some()) {
        warnings.push(
            "[agent] hydrate/mcp need the trace.read capability; both are disabled".to_string(),
        );
        (Vec::new(), McpMode::Off)
    } else {
        (hydrate, mcp)
    };

    let mut files = files;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    hasher.update(meta_toml.unwrap_or("").as_bytes());
    for (rel, content) in &files {
        hasher.update(rel.as_bytes());
        hasher.update(content.as_bytes());
    }

    Ok(SkillDefinition {
        name: meta.name.unwrap_or_else(|| capitalize(&id)),
        id,
        description,
        icon: meta.icon,
        harnesses,
        default_harness,
        capabilities: meta.capabilities,
        startup_prompt: meta.startup_prompt.filter(|s| !s.trim().is_empty()),
        hydrate,
        mcp,
        warnings,
        body,
        files,
        source_hash: format!("{:x}", hasher.finalize()),
        dir: dir.map(Path::to_path_buf),
        is_builtin: false,
        hidden: meta.hidden,
        writes: meta.writes,
        auto_approve: meta.auto_approve,
    })
}

/// The compiled-in packages, parsed.
pub fn builtin_skills() -> Vec<SkillDefinition> {
    let mut out = Vec::new();
    for pkg in builtin_packages() {
        let files = pkg
            .files
            .iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect();
        match parse_skill(pkg.skill_md, pkg.skill_toml, files, None) {
            Ok(mut s) => {
                if s.id != pkg.id {
                    eprintln!(
                        "Warning: compiled-in package {} declares name {:?}",
                        pkg.id, s.id
                    );
                }
                s.is_builtin = true;
                out.push(s);
            }
            Err(e) => eprintln!(
                "Warning: compiled-in skill package {} failed to parse: {e}",
                pkg.id
            ),
        }
    }
    out
}

/// `AGENT_MUX_SKILLS_DIR`, else `~/.agent-mux/skills`.
pub fn default_skills_dir() -> PathBuf {
    if let Ok(p) = std::env::var("AGENT_MUX_SKILLS_DIR")
        && !p.trim().is_empty()
    {
        return PathBuf::from(p.trim());
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".agent-mux").join("skills"))
        .unwrap_or_else(|| PathBuf::from("skills"))
}

/// Loads one package directory: SKILL.md, optional skill.toml, and every
/// `.md` under `reference/`.
pub fn load_skill_dir(dir: &Path) -> Result<SkillDefinition, SkillError> {
    let skill_md = dir.join("SKILL.md");
    let source = std::fs::read_to_string(&skill_md).map_err(|e| SkillError {
        path: skill_md.clone(),
        message: format!("cannot read: {e}"),
    })?;
    let toml_path = dir.join("skill.toml");
    let meta = if toml_path.is_file() {
        Some(std::fs::read_to_string(&toml_path).map_err(|e| SkillError {
            path: toml_path.clone(),
            message: format!("cannot read: {e}"),
        })?)
    } else {
        None
    };
    let mut files = Vec::new();
    let reference = dir.join("reference");
    if reference.is_dir()
        && let Ok(entries) = std::fs::read_dir(&reference)
    {
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("md") {
                let rel = format!(
                    "reference/{}",
                    p.file_name().and_then(|n| n.to_str()).unwrap_or_default()
                );
                let content = std::fs::read_to_string(&p).map_err(|e| SkillError {
                    path: p.clone(),
                    message: format!("cannot read: {e}"),
                })?;
                files.push((rel, content));
            }
        }
    }
    parse_skill(&source, meta.as_deref(), files, Some(dir))
}

/// Every skill agent-mux can list: packages under the user directory first,
/// then compiled-in ones not shadowed by them. Sorted by display name.
/// Diagnostics describe packages that failed to load.
pub fn load_skills(custom_dir: Option<&Path>) -> (Vec<SkillDefinition>, Vec<String>) {
    let root = custom_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(default_skills_dir);
    let mut skills: Vec<SkillDefinition> = Vec::new();
    let mut diagnostics = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir() && p.join("SKILL.md").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            match load_skill_dir(&dir) {
                Ok(s) if skills.iter().any(|k| k.id == s.id) => diagnostics.push(format!(
                    "duplicate skill id {:?} at {} ignored",
                    s.id,
                    dir.display()
                )),
                Ok(s) => {
                    for w in &s.warnings {
                        diagnostics.push(format!("{}: {w}", dir.display()));
                    }
                    skills.push(s);
                }
                Err(e) => diagnostics.push(e.to_string()),
            }
        }
    }
    for b in builtin_skills() {
        if !skills.iter().any(|s| s.id == b.id) {
            skills.push(b);
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    (skills, diagnostics)
}

/// Index of `default_harness` within `harnesses` (0 when absent).
pub fn default_harness_index(def: &SkillDefinition) -> usize {
    def.harnesses
        .iter()
        .position(|h| *h == def.default_harness)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_heimdall_parses() {
        let s = builtin_skills();
        assert_eq!(
            s.len(),
            builtin_packages().len(),
            "every compiled-in package parses"
        );
        assert_eq!(s[0].id, "heimdall");
        assert!(
            s.iter().filter(|p| p.hidden).count() >= 17,
            "step skills and the planner are hidden"
        );
        assert!(s.iter().any(|p| p.id == "wf-transform" && p.writes));
        assert!(s[0].is_builtin);
        assert_eq!(s[0].files.len(), 3);
        assert!(s[0].capabilities.iter().any(|c| c == "trace.read"));
    }

    #[test]
    fn frontmatter_split_keeps_body() {
        let (fields, body) =
            split_frontmatter("---\nname: x\ndescription: Use it\n---\n\n# Title\nbody\n").unwrap();
        assert_eq!(fields.len(), 2);
        assert!(body.starts_with("# Title"));
    }
}
