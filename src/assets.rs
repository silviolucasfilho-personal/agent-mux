//! The configuration library: every prompt, skill, loop pattern, loop
//! skill, loop agent and template agent-mux ships, and the files under
//! `~/.agent-mux/` that replace them one by one.
//!
//! ```text
//! ~/.agent-mux/                       ($AGENT_MUX_LIBRARY_DIR overrides the root)
//! ├── prompts.toml                    the prompts agent-mux composes (crate::prompts)
//! ├── profiles.toml                   settings (crate::config)
//! ├── skills/<id>/…                   skill packages ($AGENT_MUX_SKILLS_DIR overrides)
//! └── loops/
//!     ├── registry.toml               patterns, merged by id over the built-in registry
//!     ├── skills/<name>/SKILL.md      loop skills, by name
//!     ├── agents/<name>.md            loop agents, by name
//!     └── templates/<file>            workspace templates, by file name
//! ```
//!
//! Resolution is per file: a library file is the effective text, otherwise
//! the compiled-in one is. `prompts.toml` and `registry.toml` are keyed
//! documents merged key by key (see `prompts` and `loops::patterns`).
//! The `Catalog` enumerates every item with its source and validation
//! problems, creates overrides, resets them, scaffolds new items and pushes
//! loop skills and agents into the workspaces of registered loops.

use crate::harness::Harness;
use crate::loops::registry::Registry;
use crate::loops::{patterns, scaffold};
use crate::tracing::inventory::parse_frontmatter;
use std::fmt;
use std::path::{Path, PathBuf};

/// `$AGENT_MUX_LIBRARY_DIR`, else `~/.agent-mux`.
pub fn root() -> PathBuf {
    if let Ok(p) = std::env::var("AGENT_MUX_LIBRARY_DIR")
        && !p.trim().is_empty()
    {
        return PathBuf::from(p.trim());
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".agent-mux"))
        .unwrap_or_else(|| PathBuf::from(".agent-mux"))
}

/// The skill packages subtree: `$AGENT_MUX_SKILLS_DIR` keeps its meaning.
pub fn skills_dir(root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("AGENT_MUX_SKILLS_DIR")
        && !p.trim().is_empty()
    {
        return PathBuf::from(p.trim());
    }
    root.join("skills")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Prompts,
    Settings,
    Skill,
    LoopPattern,
    LoopSkill,
    LoopAgent,
    LoopTemplate,
    Workflow,
    Agent,
}

impl Kind {
    pub const ALL: [Kind; 9] = [
        Kind::Prompts,
        Kind::Settings,
        Kind::Skill,
        Kind::Workflow,
        Kind::Agent,
        Kind::LoopPattern,
        Kind::LoopSkill,
        Kind::LoopAgent,
        Kind::LoopTemplate,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Prompts => "prompts",
            Kind::Settings => "settings",
            Kind::Skill => "skill",
            Kind::LoopPattern => "loop patterns",
            Kind::LoopSkill => "loop skill",
            Kind::LoopAgent => "loop agent",
            Kind::LoopTemplate => "loop template",
            Kind::Workflow => "workflow",
            Kind::Agent => "agent",
        }
    }

    /// The group header in the Configuration view.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Prompts => "Prompts",
            Kind::Settings => "Settings",
            Kind::Skill => "Skills",
            Kind::LoopPattern => "Loop patterns",
            Kind::LoopSkill => "Loop skills",
            Kind::LoopAgent => "Loop agents",
            Kind::LoopTemplate => "Loop templates",
            Kind::Workflow => "Workflows",
            Kind::Agent => "Agents",
        }
    }

    /// Kinds `Catalog::new_item` can create.
    pub fn creatable(self) -> bool {
        matches!(
            self,
            Kind::Skill | Kind::LoopSkill | Kind::LoopAgent | Kind::Workflow | Kind::Agent
        )
    }

    /// The word `agent-mux config new` takes.
    pub fn new_word(self) -> Option<&'static str> {
        match self {
            Kind::Skill => Some("skill"),
            Kind::LoopSkill => Some("loop-skill"),
            Kind::LoopAgent => Some("loop-agent"),
            Kind::Workflow => Some("workflow"),
            Kind::Agent => Some("agent"),
            _ => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Compiled in; no library file.
    Builtin,
    /// A library file shadows the compiled-in text.
    Override,
    /// A library file with no compiled-in counterpart.
    User,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Builtin => "built-in",
            Source::Override => "override",
            Source::User => "user",
        }
    }
}

/// One configurable item.
#[derive(Debug, Clone)]
pub struct Asset {
    pub kind: Kind,
    /// Path under the library root, also the id (`loops/skills/loop-triage/SKILL.md`).
    pub id: String,
    /// Where the library file is or would be.
    pub path: PathBuf,
    /// The compiled-in text, when there is one.
    pub builtin: Option<&'static str>,
    /// The repository path of the compiled-in text.
    pub repo_path: Option<&'static str>,
    pub source: Source,
    /// The short name: skill id, loop skill name, agent name, file name.
    pub name: String,
    pub problems: Vec<String>,
}

impl Asset {
    /// The effective text: the library file, else the compiled-in text.
    pub fn effective(&self) -> String {
        if self.source != Source::Builtin
            && let Ok(t) = std::fs::read_to_string(&self.path)
        {
            return t;
        }
        self.builtin.unwrap_or_default().to_string()
    }

    pub fn valid(&self) -> bool {
        self.problems.is_empty()
    }

    /// Placeholders this item's text accepts, for the detail pane.
    pub fn placeholders(&self) -> Vec<String> {
        match self.kind {
            Kind::Prompts => crate::prompts::LOOP_PLACEHOLDERS
                .iter()
                .chain(crate::prompts::WORKFLOW_PLACEHOLDERS.iter())
                .map(|p| format!("{{{p}}}"))
                .collect::<Vec<_>>()
                .into_iter()
                .fold(Vec::new(), |mut v, p| {
                    if !v.contains(&p) {
                        v.push(p);
                    }
                    v
                }),
            Kind::LoopTemplate => TEMPLATE_PLACEHOLDERS
                .iter()
                .map(|p| format!("{{{{{p}}}}}"))
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// `{{X}}` markers the scaffolder fills.
pub const TEMPLATE_PLACEHOLDERS: &[&str] = &[
    "PROJECT",
    "PATTERN",
    "CADENCE",
    "LEVEL",
    "STATE_FILE",
    "HARNESS",
    "GATES",
    "ROW",
    "GOAL",
];

/// A copy of a loop skill or agent in a registered loop's workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceCopy {
    pub workspace: PathBuf,
    pub path: PathBuf,
    pub harness: Harness,
    /// The copy is present and equals the effective text.
    pub same: bool,
    pub present: bool,
}

/// What `Catalog::push` did or would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushReport {
    pub written: Vec<PathBuf>,
    pub unchanged: Vec<PathBuf>,
    pub errors: Vec<String>,
    /// Registered loops whose workspace is missing or whose harness has no
    /// project skills directory.
    pub skipped_loops: Vec<String>,
}

pub struct Catalog {
    pub root: PathBuf,
    /// `profiles.toml`: the file the configuration was loaded from, else
    /// `<root>/profiles.toml`.
    pub settings_path: PathBuf,
    pub assets: Vec<Asset>,
    /// A `loops/registry.toml` in the library that does not parse; the
    /// built-in patterns are used until it is fixed.
    pub registry_error: Option<String>,
}

impl Catalog {
    /// Enumerates the built-in items and every file the library adds, and
    /// validates each one.
    pub fn load(root: &Path, settings_path: Option<&Path>) -> Catalog {
        let settings_path = settings_path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.join("profiles.toml"));
        let mut cat = Catalog {
            root: root.to_path_buf(),
            settings_path,
            assets: Vec::new(),
            registry_error: None,
        };
        cat.push_builtin(
            Kind::Prompts,
            crate::prompts::FILE,
            crate::prompts::BUILTIN,
            "src/prompts.toml",
        );
        cat.assets.push(Asset {
            kind: Kind::Settings,
            id: "profiles.toml".into(),
            path: cat.settings_path.clone(),
            builtin: None,
            repo_path: Some("profiles.example.toml"),
            source: if cat.settings_path.is_file() {
                Source::User
            } else {
                Source::Builtin
            },
            name: "profiles.toml".into(),
            problems: Vec::new(),
        });
        cat.scan_skills();
        cat.scan_workflows();
        cat.scan_agents();
        cat.push_builtin(
            Kind::LoopPattern,
            "loops/registry.toml",
            patterns::BUILTIN_REGISTRY,
            "loops/registry.toml",
        );
        cat.scan_loop_skills();
        cat.scan_loop_agents();
        for (name, text) in scaffold::TEMPLATES {
            cat.push_builtin(
                Kind::LoopTemplate,
                &format!("loops/templates/{name}"),
                text,
                "loops/templates/",
            );
        }
        cat.validate_all();
        cat
    }

    /// The catalog for the default root and the default settings file.
    pub fn current(settings_path: Option<&Path>) -> Catalog {
        Catalog::load(&root(), settings_path)
    }

    fn push_builtin(&mut self, kind: Kind, id: &str, builtin: &'static str, repo: &'static str) {
        let path = self.root.join(id);
        let source = if path.is_file() {
            Source::Override
        } else {
            Source::Builtin
        };
        let name = match kind {
            Kind::LoopSkill => id
                .trim_end_matches("/SKILL.md")
                .rsplit('/')
                .next()
                .unwrap_or(id)
                .to_string(),
            Kind::LoopAgent => id
                .rsplit('/')
                .next()
                .unwrap_or(id)
                .trim_end_matches(".md")
                .to_string(),
            _ => id.rsplit('/').next().unwrap_or(id).to_string(),
        };
        self.assets.push(Asset {
            kind,
            id: id.to_string(),
            path,
            builtin: Some(builtin),
            repo_path: Some(repo),
            source,
            name,
            problems: Vec::new(),
        });
    }

    fn push_user(&mut self, kind: Kind, id: String, path: PathBuf, name: String) {
        if self.assets.iter().any(|a| a.id == id) {
            return;
        }
        self.assets.push(Asset {
            kind,
            id,
            path,
            builtin: None,
            repo_path: None,
            source: Source::User,
            name,
            problems: Vec::new(),
        });
    }

    /// Heimdall's files, then every package directory under the skills
    /// subtree, file by file.
    fn scan_skills(&mut self) {
        let dir = skills_dir(&self.root);
        for pkg in crate::skill::builtin_packages() {
            for (rel, text) in pkg.all_files() {
                let id = format!("skills/{}/{rel}", pkg.id);
                let path = dir.join(pkg.id).join(rel);
                let source = if path.is_file() {
                    Source::Override
                } else {
                    Source::Builtin
                };
                self.assets.push(Asset {
                    kind: Kind::Skill,
                    id,
                    path,
                    builtin: Some(text),
                    repo_path: Some(pkg.repo_dir),
                    source,
                    name: pkg.id.to_string(),
                    problems: Vec::new(),
                });
            }
        }
        for pkg in sorted_dirs(&dir) {
            let Some(pkg_name) = pkg.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !pkg.join("SKILL.md").is_file() {
                continue;
            }
            let mut files = vec![pkg.join("SKILL.md")];
            if pkg.join("skill.toml").is_file() {
                files.push(pkg.join("skill.toml"));
            }
            for f in sorted_files(&pkg.join("reference"), "md") {
                files.push(f);
            }
            for f in files {
                let rel = f
                    .strip_prefix(&pkg)
                    .ok()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                self.push_user(
                    Kind::Skill,
                    format!("skills/{pkg_name}/{rel}"),
                    f,
                    pkg_name.to_string(),
                );
            }
        }
    }

    fn scan_workflows(&mut self) {
        for (name, text) in crate::workflows::library::BUILTIN {
            self.push_builtin(
                Kind::Workflow,
                &format!("workflows/{name}.toml"),
                text,
                "workflows/",
            );
        }
        for f in sorted_files(&self.root.join("workflows"), "toml") {
            let Some(stem) = f.file_stem().and_then(|n| n.to_str()).map(str::to_string) else {
                continue;
            };
            self.push_user(Kind::Workflow, format!("workflows/{stem}.toml"), f, stem);
        }
    }

    /// Workflow agents (`src/agents`): the built-in ones, which a library
    /// file of the same name replaces, then the user's own.
    fn scan_agents(&mut self) {
        for (name, text) in crate::agents::BUILTIN {
            self.push_builtin(
                Kind::Agent,
                &format!("agents/{name}.toml"),
                text,
                "agents/builtin/",
            );
        }
        for f in sorted_files(&crate::agents::library_dir(&self.root), "toml") {
            let Some(stem) = f.file_stem().and_then(|n| n.to_str()).map(str::to_string) else {
                continue;
            };
            self.push_user(Kind::Agent, format!("agents/{stem}.toml"), f, stem);
        }
    }

    fn scan_loop_skills(&mut self) {
        for (name, text) in scaffold::embedded_skills() {
            self.push_builtin(
                Kind::LoopSkill,
                &format!("loops/skills/{name}/SKILL.md"),
                text,
                "loops/skills/",
            );
        }
        for d in sorted_dirs(&self.root.join("loops").join("skills")) {
            let Some(name) = d.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let f = d.join("SKILL.md");
            if f.is_file() {
                self.push_user(
                    Kind::LoopSkill,
                    format!("loops/skills/{name}/SKILL.md"),
                    f,
                    name.to_string(),
                );
            }
        }
    }

    fn scan_loop_agents(&mut self) {
        for (name, text) in scaffold::embedded_agents() {
            self.push_builtin(
                Kind::LoopAgent,
                &format!("loops/agents/{name}.md"),
                text,
                match name {
                    "loop-verifier" => "loops/agents/loop-verifier.md",
                    _ => "loops/agents/loop-reviewer.md",
                },
            );
        }
        for f in sorted_files(&self.root.join("loops").join("agents"), "md") {
            let Some(stem) = f.file_stem().and_then(|n| n.to_str()).map(str::to_string) else {
                continue;
            };
            self.push_user(Kind::LoopAgent, format!("loops/agents/{stem}.md"), f, stem);
        }
    }

    fn validate_all(&mut self) {
        let skill_infos: Vec<crate::workflows::document::SkillInfo> = {
            let (skills, _) = crate::skill::load_skills(Some(&skills_dir(&self.root)));
            crate::workflows::library::skill_infos(&skills)
        };
        let loop_skill_names: Vec<String> = self
            .assets
            .iter()
            .filter(|a| a.kind == Kind::LoopSkill)
            .map(|a| a.name.clone())
            .collect();
        let loop_agent_names: Vec<String> = self
            .assets
            .iter()
            .filter(|a| a.kind == Kind::LoopAgent)
            .map(|a| a.name.clone())
            .collect();
        let mut registry_error = None;
        for i in 0..self.assets.len() {
            let a = &self.assets[i];
            let problems = match a.kind {
                Kind::Prompts => crate::prompts::validate(&a.effective()),
                Kind::Settings => match std::fs::read_to_string(&a.path) {
                    Ok(t) => crate::config::parse(&t)
                        .err()
                        .map(|e| vec![format!("profiles.toml: {e}")])
                        .unwrap_or_default(),
                    Err(_) => Vec::new(),
                },
                Kind::Skill => validate_skill_file(a),
                Kind::LoopPattern => {
                    let p = patterns::validate_registry(
                        &a.effective(),
                        &loop_skill_names,
                        &loop_agent_names,
                    );
                    if a.source != Source::Builtin
                        && let Some(first) = p.first()
                        && first.contains("does not parse")
                    {
                        registry_error = Some(first.clone());
                    }
                    p
                }
                Kind::LoopSkill => validate_loop_skill(&a.name, &a.effective()),
                Kind::LoopAgent => validate_loop_agent(&a.name, &a.effective()),
                Kind::LoopTemplate => validate_template(&a.name, &a.effective()),
                Kind::Workflow => validate_workflow(&a.name, &a.effective(), &skill_infos),
                Kind::Agent => validate_agent(&a.name, &a.effective()),
            };
            self.assets[i].problems = problems;
        }
        self.registry_error = registry_error;
    }

    pub fn by_kind(&self, kind: Kind) -> impl Iterator<Item = &Asset> {
        self.assets.iter().filter(move |a| a.kind == kind)
    }

    pub fn get(&self, id: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.id == id)
    }

    /// The item an id, a unique suffix or a unique substring names.
    pub fn find(&self, query: &str) -> Result<&Asset, String> {
        let q = query.trim().trim_start_matches("./");
        if let Some(a) = self.get(q) {
            return Ok(a);
        }
        let by_suffix: Vec<&Asset> = self
            .assets
            .iter()
            .filter(|a| a.id.ends_with(q) || a.id.ends_with(&format!("{q}/SKILL.md")))
            .collect();
        if by_suffix.len() == 1 {
            return Ok(by_suffix[0]);
        }
        let by_name: Vec<&Asset> = self.assets.iter().filter(|a| a.name == q).collect();
        if by_name.len() == 1 {
            return Ok(by_name[0]);
        }
        let by_sub: Vec<&Asset> = self.assets.iter().filter(|a| a.id.contains(q)).collect();
        match by_sub.len() {
            0 => Err(format!("no configuration item matches {query:?}")),
            1 => Ok(by_sub[0]),
            _ => Err(format!(
                "{query:?} is ambiguous: {}",
                by_sub
                    .iter()
                    .map(|a| a.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// Writes the compiled-in text to the library so it can be edited.
    /// Returns the path; an existing file is left alone.
    pub fn create_override(&self, asset: &Asset) -> Result<PathBuf, String> {
        if asset.path.is_file() {
            return Ok(asset.path.clone());
        }
        let text = match asset.kind {
            Kind::Settings => std::fs::read_to_string("profiles.example.toml")
                .ok()
                .unwrap_or_else(|| SETTINGS_SKELETON.to_string()),
            _ => asset
                .builtin
                .ok_or_else(|| format!("{} has no built-in text", asset.id))?
                .to_string(),
        };
        write_new(&asset.path, &text)?;
        Ok(asset.path.clone())
    }

    /// Deletes the library file: an override falls back to the compiled-in
    /// text, a user item is removed. Settings are never deleted.
    pub fn reset(&self, asset: &Asset) -> Result<(), String> {
        if asset.kind == Kind::Settings {
            return Err("profiles.toml is your settings file; edit it instead".into());
        }
        if asset.source == Source::Builtin {
            return Err(format!("{} already uses the built-in text", asset.id));
        }
        std::fs::remove_file(&asset.path).map_err(|e| format!("{}: {e}", asset.path.display()))?;
        // an emptied loop skill directory is removed too
        if asset.kind == Kind::LoopSkill
            && let Some(dir) = asset.path.parent()
            && std::fs::read_dir(dir)
                .map(|mut d| d.next().is_none())
                .unwrap_or(false)
        {
            let _ = std::fs::remove_dir(dir);
        }
        Ok(())
    }

    /// Creates a new skill package, loop skill or loop agent from a
    /// skeleton and returns the file to edit.
    pub fn new_item(&self, kind: Kind, name: &str) -> Result<PathBuf, String> {
        let name = name.trim();
        if !crate::skill::is_valid_skill_id(name) {
            return Err(format!(
                "{name:?} is not a valid name: lowercase letters, digits, `-` and `_`, starting with a letter or digit"
            ));
        }
        let (path, text) = match kind {
            Kind::Skill => {
                let dir = skills_dir(&self.root).join(name);
                if dir.exists() {
                    return Err(format!("{} already exists", dir.display()));
                }
                write_new(&dir.join("skill.toml"), &skill_toml_skeleton(name))?;
                (dir.join("SKILL.md"), skill_md_skeleton(name))
            }
            Kind::LoopSkill => {
                let path = self
                    .root
                    .join("loops")
                    .join("skills")
                    .join(name)
                    .join("SKILL.md");
                (path, loop_skill_skeleton(name))
            }
            Kind::LoopAgent => {
                let path = self
                    .root
                    .join("loops")
                    .join("agents")
                    .join(format!("{name}.md"));
                (path, loop_agent_skeleton(name))
            }
            Kind::Workflow => {
                let path = self.root.join("workflows").join(format!("{name}.toml"));
                (path, crate::workflows::library::skeleton(name))
            }
            Kind::Agent => {
                let path = crate::agents::library_dir(&self.root).join(format!("{name}.toml"));
                (
                    path,
                    crate::agents::from_template("blank", name).unwrap_or_default(),
                )
            }
            other => {
                return Err(format!(
                    "new {} items are not created this way: {}",
                    other.label(),
                    match other {
                        Kind::LoopPattern => "add a [[patterns]] table to loops/registry.toml",
                        Kind::LoopTemplate => "the template set is fixed; edit a template instead",
                        _ => "edit the file instead",
                    }
                ));
            }
        };
        if path.exists() {
            return Err(format!("{} already exists", path.display()));
        }
        write_new(&path, &text)?;
        Ok(path)
    }

    /// The patterns that list a loop skill, by id.
    pub fn patterns_using(&self, skill_name: &str) -> Vec<String> {
        patterns::load(&self.root)
            .0
            .iter()
            .filter(|p| p.skills.iter().any(|s| s == skill_name))
            .map(|p| p.id.clone())
            .collect()
    }

    /// Every registered loop workspace that holds (or should hold) a copy
    /// of a loop skill or agent, and whether the copy matches.
    pub fn workspace_copies(&self, asset: &Asset, registry: &Registry) -> Vec<WorkspaceCopy> {
        let mut out = Vec::new();
        let effective = asset.effective();
        let (pats, _) = patterns::load(&self.root);
        let preamble = crate::prompts::Prompts::load(&self.root).agent_preamble;
        for entry in &registry.loops {
            let Some(harness) = Harness::detect(&entry.harness) else {
                continue;
            };
            let Some(pattern) = pats.iter().find(|p| p.id == entry.pattern) else {
                continue;
            };
            let path = match asset.kind {
                Kind::LoopSkill => {
                    if !pattern.skills.contains(&asset.name) {
                        continue;
                    }
                    scaffold::project_skills_dir(harness, &entry.workspace)
                        .map(|d| d.join(&asset.name).join("SKILL.md"))
                }
                Kind::LoopAgent => {
                    if !pattern.effective_agents().contains(&asset.name) {
                        continue;
                    }
                    scaffold::agent_path(harness, &entry.workspace, &asset.name)
                }
                _ => None,
            };
            let Some(path) = path else {
                continue;
            };
            if out.iter().any(|c: &WorkspaceCopy| c.path == path) {
                continue;
            }
            let expected = match asset.kind {
                Kind::LoopAgent => {
                    scaffold::agent_file(harness, &asset.name, &effective, &preamble)
                }
                _ => effective.clone(),
            };
            let current = std::fs::read_to_string(&path).ok();
            out.push(WorkspaceCopy {
                workspace: entry.workspace.clone(),
                path,
                harness,
                same: current.as_deref() == Some(expected.as_str()),
                present: current.is_some(),
            });
        }
        out
    }

    /// Rewrites the loop skills and agents of every registered loop with
    /// the effective text. Contract files are never touched.
    pub fn push(&self, registry: &Registry, dry_run: bool) -> PushReport {
        let mut report = PushReport::default();
        let (pats, _) = patterns::load(&self.root);
        let preamble = crate::prompts::Prompts::load(&self.root).agent_preamble;
        for entry in &registry.loops {
            let label = format!("{}@{}", entry.pattern, entry.workspace_name());
            let Some(harness) = Harness::detect(&entry.harness) else {
                report.skipped_loops.push(label);
                continue;
            };
            let Some(pattern) = pats.iter().find(|p| p.id == entry.pattern) else {
                report.skipped_loops.push(label);
                continue;
            };
            if !entry.workspace.is_dir() {
                report.skipped_loops.push(label);
                continue;
            }
            let Some(skills_dir) = scaffold::project_skills_dir(harness, &entry.workspace) else {
                report.skipped_loops.push(label);
                continue;
            };
            let mut files: Vec<(PathBuf, String)> = Vec::new();
            for name in &pattern.skills {
                if let Some(text) = loop_skill(&self.root, name) {
                    files.push((skills_dir.join(name).join("SKILL.md"), text));
                }
            }
            let available = loop_agents(&self.root);
            for name in pattern.effective_agents() {
                let Some((_, body)) = available.iter().find(|(n, _)| *n == name) else {
                    continue;
                };
                if let Some(path) = scaffold::agent_path(harness, &entry.workspace, &name) {
                    files.push((path, scaffold::agent_file(harness, &name, body, &preamble)));
                }
            }
            for (path, content) in files {
                if report.written.contains(&path) || report.unchanged.contains(&path) {
                    continue;
                }
                if std::fs::read_to_string(&path).ok().as_deref() == Some(content.as_str()) {
                    report.unchanged.push(path);
                    continue;
                }
                if dry_run {
                    report.written.push(path);
                    continue;
                }
                match write_file(&path, &content) {
                    Ok(()) => report.written.push(path),
                    Err(e) => report.errors.push(format!("{}: {e}", path.display())),
                }
            }
        }
        report
    }
}

// ---- effective text for the scaffolder and the launches --------------------

/// A loop skill's effective `SKILL.md`: the library's, else the built-in.
pub fn loop_skill(root: &Path, name: &str) -> Option<String> {
    let lib = root
        .join("loops")
        .join("skills")
        .join(name)
        .join("SKILL.md");
    if let Ok(t) = std::fs::read_to_string(&lib) {
        return Some(t);
    }
    scaffold::embedded_skill(name).map(str::to_string)
}

/// Every loop skill name available: built-in and library.
pub fn loop_skill_names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = scaffold::embedded_skills()
        .iter()
        .map(|(n, _)| n.to_string())
        .collect();
    for d in sorted_dirs(&root.join("loops").join("skills")) {
        if d.join("SKILL.md").is_file()
            && let Some(n) = d.file_name().and_then(|n| n.to_str())
            && !names.iter().any(|k| k == n)
        {
            names.push(n.to_string());
        }
    }
    names
}

/// A loop agent's effective markdown (Claude's file shape).
pub fn loop_agent(root: &Path, name: &str) -> Option<String> {
    let lib = root.join("loops").join("agents").join(format!("{name}.md"));
    if let Ok(t) = std::fs::read_to_string(&lib) {
        return Some(t);
    }
    scaffold::embedded_agent(name).map(str::to_string)
}

/// Every loop agent as (name, markdown): the built-in checkers first
/// (verifier, reviewer), then the library's additions.
pub fn loop_agents(root: &Path) -> Vec<(String, String)> {
    let builtin = scaffold::embedded_agents();
    let mut out: Vec<(String, String)> = builtin
        .iter()
        .map(|(name, _)| (name.to_string(), loop_agent(root, name).unwrap_or_default()))
        .collect();
    for f in sorted_files(&root.join("loops").join("agents"), "md") {
        if let Some(stem) = f.file_stem().and_then(|n| n.to_str())
            && !builtin.iter().any(|(n, _)| *n == stem)
            && let Ok(t) = std::fs::read_to_string(&f)
        {
            out.push((stem.to_string(), t));
        }
    }
    out
}

/// A workspace template's effective text.
pub fn template(root: &Path, name: &str) -> Option<String> {
    let lib = root.join("loops").join("templates").join(name);
    if let Ok(t) = std::fs::read_to_string(&lib) {
        return Some(t);
    }
    scaffold::template(name).map(str::to_string)
}

// ---- the editor -------------------------------------------------------------

/// The editor command line: `editor` from the settings, else `$VISUAL`,
/// else `$EDITOR`, else `vi`.
pub fn editor_command(configured: Option<&str>) -> Vec<String> {
    let pick = |s: Option<String>| {
        s.map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|s| s.split_whitespace().map(str::to_string).collect::<Vec<_>>())
    };
    pick(configured.map(str::to_string))
        .or_else(|| pick(std::env::var("VISUAL").ok()))
        .or_else(|| pick(std::env::var("EDITOR").ok()))
        .unwrap_or_else(|| vec!["vi".to_string()])
}

// ---- validation -------------------------------------------------------------

fn validate_skill_file(a: &Asset) -> Vec<String> {
    let text = a.effective();
    let file = a.id.rsplit('/').next().unwrap_or_default();
    if file == "SKILL.md" {
        return match crate::skill::parse_skill(&text, None, Vec::new(), None) {
            Ok(def) if def.id != a.name => vec![format!(
                "frontmatter name {:?} must equal the directory name {:?}",
                def.id, a.name
            )],
            Ok(_) => Vec::new(),
            Err(e) => vec![e.to_string()],
        };
    }
    if file == "skill.toml" {
        return crate::skill::parse_skill_toml(&text)
            .err()
            .map(|e| vec![format!("skill.toml: {e}")])
            .unwrap_or_default();
    }
    Vec::new()
}

fn frontmatter_problems(text: &str, expected_name: &str, what: &str) -> Vec<String> {
    let Some(fields) = parse_frontmatter(text) else {
        return vec![format!(
            "{what}: missing `---` frontmatter with name and description"
        )];
    };
    let mut problems = Vec::new();
    match fields.iter().find(|(k, _)| k == "name") {
        Some((_, v)) if v.trim() == expected_name => {}
        Some((_, v)) => problems.push(format!(
            "{what}: frontmatter name {:?} must equal {expected_name:?}",
            v.trim()
        )),
        None => problems.push(format!("{what}: frontmatter has no name")),
    }
    match fields.iter().find(|(k, _)| k == "description") {
        Some((_, v)) if !v.trim().is_empty() => {}
        _ => problems.push(format!("{what}: frontmatter has no description")),
    }
    problems
}

fn validate_loop_skill(name: &str, text: &str) -> Vec<String> {
    frontmatter_problems(text, name, "SKILL.md")
}

fn validate_loop_agent(name: &str, text: &str) -> Vec<String> {
    frontmatter_problems(text, name, &format!("{name}.md"))
}

fn validate_workflow(
    name: &str,
    text: &str,
    skills: &[crate::workflows::document::SkillInfo],
) -> Vec<String> {
    match crate::workflows::parse(text) {
        Ok(doc) => {
            let mut p = crate::workflows::validate(&doc, Some(skills));
            let stem = name.trim_end_matches(".toml");
            if doc.name != stem {
                p.push(format!(
                    "workflow.name {:?} must equal the file name {stem:?}",
                    doc.name
                ));
            }
            p
        }
        Err(p) => p,
    }
}

fn validate_agent(name: &str, text: &str) -> Vec<String> {
    match crate::agents::AgentSpec::parse(text) {
        Ok(a) => {
            let stem = name.trim_end_matches(".toml");
            if a.name == stem {
                Vec::new()
            } else {
                vec![format!(
                    "name {:?} must equal the file name {stem:?}",
                    a.name
                )]
            }
        }
        Err(p) => p,
    }
}

fn validate_template(name: &str, text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let marker = &after[..end];
        if !TEMPLATE_PLACEHOLDERS.contains(&marker) {
            let msg = format!("unknown placeholder {{{{{marker}}}}}");
            if !problems.contains(&msg) {
                problems.push(msg);
            }
        }
        rest = &after[end + 2..];
    }
    match name {
        "gate.yaml" => {
            if let Err(e) = crate::loops::gate::parse(text) {
                problems.push(format!("gate.yaml: {e}"));
            }
        }
        "loop-ledger.json" => {
            // placeholders are substituted before the file is written
            let filled = text
                .replace("{{GOAL}}", "goal")
                .replace("{{PATTERN}}", "pattern")
                .replace("{{LEVEL}}", "L1");
            if let Err(e) = serde_json::from_str::<serde_json::Value>(&filled) {
                problems.push(format!("loop-ledger.json: {e}"));
            }
        }
        _ => {}
    }
    problems
}

// ---- skeletons --------------------------------------------------------------

const SETTINGS_SKELETON: &str = "# agent-mux settings. See profiles.example.toml in the repository\n# for every key.\n\n# editor = \"code --wait\"\n\n[[profiles]]\nname = \"Claude Code\"\ncommand = \"claude\"\nargs = []\n\n[[profiles]]\nname = \"Codex CLI\"\ncommand = \"codex\"\nargs = []\n";

fn skill_md_skeleton(name: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: Use when the user asks for \"{name}\". Describe what the skill does and quote the phrases that trigger it.\n---\n\n# {name}\n\nWhat this skill does, step by step.\n"
    )
}

fn skill_toml_skeleton(name: &str) -> String {
    let title = name
        .split(['-', '_'])
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "# agent-mux metadata for the {name} skill.\nname = \"{title}\"\nicon = \"⚡\"\nharnesses = [\"claude\", \"codex\", \"agy\"]\n# startup_prompt = \"\"\n"
    )
}

fn loop_skill_skeleton(name: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: Use when agent-mux runs a loop that lists {name}. Reads the loop context, does one bounded thing, rewrites the state file and reports.\nallowed-tools: Read, Grep, Glob, Bash, Write, Edit\n---\n\n# {name}\n\nYou are one run of a scheduled loop. Read the file named by `$AGENT_MUX_LOOP_CONTEXT` before running any command; if it is missing, print `no loop context` and stop.\n\n## Procedure\n\n1. Take `files.state`, `run.level_effective`, `budget.mode` and `gate` from the context.\n2. Make one listing call, compare its fingerprint with the `Fingerprint:` line of the state file, and exit as `no-op` when nothing changed.\n3. Otherwise triage within the budget, edit only the state file at L1, and hand at most one fix to `loop-fix` at L2 or above.\n\n## State file\n\nRewrite it with `Last run:`, the High Priority, Watch List and Recent Noise sections, a `Fingerprint:` line and the `Run log:` footer.\n\n## Result\n\nEnd with a fenced `loop-result` block with `outcome`, `items_found`, `actions_taken`, `escalations` and `summary`.\n"
    )
}

fn loop_agent_skeleton(name: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: What this agent checks when a loop run hands it work.\ntools: Read, Grep, Glob, Bash\nmodel: inherit\n---\n\n# {name}\n\nRead the file named by `$AGENT_MUX_LOOP_CONTEXT` if it is set. State what you were asked to do, do only that, and answer with one verdict line.\n"
    )
}

// ---- files ------------------------------------------------------------------

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn sorted_files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some(ext))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn write_new(path: &Path, text: &str) -> Result<(), String> {
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    write_file(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn write_file(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn the_catalog_lists_every_builtin_item_and_validates_it() {
        let dir = lib();
        let cat = Catalog::load(dir.path(), None);
        let count = |k: Kind| cat.by_kind(k).count();
        assert_eq!(count(Kind::Prompts), 1);
        assert_eq!(count(Kind::Settings), 1);
        assert_eq!(
            count(Kind::Skill),
            49,
            "Heimdall, the planner and twenty step skills"
        );
        assert_eq!(count(Kind::LoopPattern), 1);
        assert_eq!(count(Kind::LoopSkill), 11);
        assert_eq!(count(Kind::LoopAgent), 2);
        assert_eq!(count(Kind::LoopTemplate), 8);
        for a in &cat.assets {
            assert_eq!(a.source, Source::Builtin, "{}", a.id);
            assert!(a.valid(), "{}: {:?}", a.id, a.problems);
        }
        assert_eq!(
            cat.find("loop-triage").unwrap().id,
            "loops/skills/loop-triage/SKILL.md"
        );
        assert_eq!(cat.find("registry.toml").unwrap().kind, Kind::LoopPattern);
        assert_eq!(cat.find("loop-verifier").unwrap().kind, Kind::LoopAgent);
        assert!(cat.find("SKILL.md").is_err(), "ambiguous");
        assert!(cat.find("nothing-like-this").is_err());
        assert_eq!(cat.get("prompts.toml").unwrap().placeholders().len(), 10);
    }

    #[test]
    fn override_reset_and_effective_text() {
        let dir = lib();
        let cat = Catalog::load(dir.path(), None);
        let a = cat.find("loop-rules").unwrap().clone();
        let path = cat.create_override(&a).unwrap();
        assert_eq!(path, dir.path().join("loops/skills/loop-rules/SKILL.md"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), a.builtin.unwrap());
        std::fs::write(
            &path,
            "---\nname: loop-rules\ndescription: mine\n---\nbody\n",
        )
        .unwrap();

        let cat = Catalog::load(dir.path(), None);
        let a = cat.find("loop-rules").unwrap().clone();
        assert_eq!(a.source, Source::Override);
        assert!(a.effective().contains("mine"));
        assert_eq!(loop_skill(dir.path(), "loop-rules").unwrap(), a.effective());
        assert!(a.valid());
        cat.reset(&a).unwrap();
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists(), "empty directory removed");
        let cat = Catalog::load(dir.path(), None);
        assert_eq!(cat.find("loop-rules").unwrap().source, Source::Builtin);
        assert!(cat.reset(cat.find("loop-rules").unwrap()).is_err());
        assert!(cat.reset(cat.find("profiles.toml").unwrap()).is_err());
    }

    #[test]
    fn invalid_overrides_are_reported_not_used_blindly() {
        let dir = lib();
        let cat = Catalog::load(dir.path(), None);
        let a = cat.find("loop-fix").unwrap().clone();
        cat.create_override(&a).unwrap();
        std::fs::write(&a.path, "---\nname: other\n---\n").unwrap();
        std::fs::write(
            dir.path().join("prompts.toml"),
            "[loop]\nrun = \"no invocation\"\n",
        )
        .unwrap();
        let cat = Catalog::load(dir.path(), None);
        let a = cat.find("loop-fix").unwrap();
        assert_eq!(a.problems.len(), 2, "{:?}", a.problems);
        let p = cat.get("prompts.toml").unwrap();
        assert_eq!(p.source, Source::Override);
        assert_eq!(p.problems.len(), 1);
    }

    #[test]
    fn new_items_and_user_additions() {
        let dir = lib();
        let cat = Catalog::load(dir.path(), None);
        assert!(cat.new_item(Kind::LoopSkill, "Bad Name").is_err());
        assert!(cat.new_item(Kind::LoopTemplate, "x").is_err());
        let p = cat.new_item(Kind::LoopSkill, "loop-docs").unwrap();
        assert_eq!(p, dir.path().join("loops/skills/loop-docs/SKILL.md"));
        assert!(
            cat.new_item(Kind::LoopSkill, "loop-docs").is_err(),
            "exists"
        );
        let a = cat.new_item(Kind::LoopAgent, "auditor").unwrap();
        let s = cat.new_item(Kind::Skill, "my-notes").unwrap();
        assert!(s.parent().unwrap().join("skill.toml").is_file());

        let cat = Catalog::load(dir.path(), None);
        let docs = cat.find("loop-docs").unwrap();
        assert_eq!(docs.source, Source::User);
        assert!(docs.valid(), "{:?}", docs.problems);
        assert!(cat.find("auditor").unwrap().valid());
        assert_eq!(cat.by_kind(Kind::Skill).count(), 51);
        let notes = cat.find("skills/my-notes/SKILL.md").unwrap();
        assert!(notes.valid(), "{:?}", notes.problems);
        assert_eq!(
            loop_agents(dir.path())
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            vec!["loop-verifier", "loop-reviewer", "auditor"]
        );
        assert!(loop_skill_names(dir.path()).contains(&"loop-docs".to_string()));
        assert_eq!(
            std::fs::read_to_string(a).unwrap().lines().next(),
            Some("---")
        );
    }

    #[test]
    fn templates_and_registry_validate() {
        let dir = lib();
        std::fs::create_dir_all(dir.path().join("loops/templates")).unwrap();
        std::fs::write(
            dir.path().join("loops/templates/LOOP.md"),
            "# {{PROJECT}} {{NOPE}}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("loops/templates/gate.yaml"),
            "denylist: [\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("loops/registry.toml"),
            "[[patterns]]\nid = \"x\"\n",
        )
        .unwrap();
        let cat = Catalog::load(dir.path(), None);
        assert_eq!(
            cat.find("LOOP.md").unwrap().problems,
            vec!["unknown placeholder {{NOPE}}"]
        );
        assert!(!cat.find("gate.yaml").unwrap().valid());
        assert!(cat.registry_error.is_some());
        assert_eq!(
            template(dir.path(), "LOOP.md").unwrap(),
            "# {{PROJECT}} {{NOPE}}\n"
        );
        assert_eq!(
            template(dir.path(), "STATE.md").unwrap(),
            scaffold::template("STATE.md").unwrap()
        );
    }

    #[test]
    fn editor_resolution_order() {
        assert_eq!(editor_command(Some("code --wait")), vec!["code", "--wait"]);
        assert_eq!(editor_command(Some("  ")).len(), 1);
    }
}
