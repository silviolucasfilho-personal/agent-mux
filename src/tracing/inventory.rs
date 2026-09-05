//! What could trigger in a session — skills, agents, commands — read from
//! disk for a working directory and a home, per harness, with the
//! frontmatter each CLI reads; a conservative lint; and the join to what
//! the store says actually ran.
//!
//! Roots, verified on 2026-09-05 against the installed CLIs (Claude Code,
//! codex-cli 0.153.2, agy) and their plugin caches:
//!
//! | harness | project | home | plugins |
//! |---|---|---|---|
//! | claude | `.claude/skills/*/SKILL.md`, `.claude/agents/*.md`, `.claude/commands/*.md` | the same under `~/.claude/` | `~/.claude/plugins/installed_plugins.json` → each install path's `skills/`, `agents/`, `commands/` |
//! | codex | `AGENTS.md` | `~/.codex/skills/*/SKILL.md` | `~/.codex/plugins/cache/<marketplace>/<plugin>/<version>/skills/*/SKILL.md`, newest version |
//! | agy | `.agents/skills/*/SKILL.md`, `.agents/workflows/*.md`, `.agents/plugins/*/` | `~/.gemini/config/skills/*/SKILL.md` | `~/.gemini/config/plugins/*/` (`skills/`, `workflows/`) |
//!
//! Within a harness a project definition shadows a home one of the same
//! name and kind, which shadows a plugin's — the order the roots are
//! listed in.

use crate::harness::Harness;
use crate::tracing::pricing::PriceTable;
use crate::tracing::store::query::{PromptRow, SkillStat};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Skill,
    Agent,
    Command,
    /// A repository's `AGENTS.md`: instructions, not a trigger.
    Instructions,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Skill => "skill",
            Kind::Agent => "agent",
            Kind::Command => "command",
            Kind::Instructions => "instructions",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Project,
    Home,
    Plugin(String),
}

impl Scope {
    pub fn label(&self) -> String {
        match self {
            Scope::Project => "project".into(),
            Scope::Home => "home".into(),
            Scope::Plugin(p) => format!("plugin:{p}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub harness: Harness,
    pub kind: Kind,
    /// The name the harness knows it by: frontmatter `name`, else the
    /// directory (skills) or file stem (agents, commands).
    pub name: String,
    pub path: PathBuf,
    pub scope: Scope,
    pub description: String,
    pub tools: Vec<String>,
    pub model: Option<String>,
    /// Quoted phrases in the description — how Claude's skill format asks
    /// authors to say when a skill should trigger.
    pub triggers: Vec<String>,
    /// False when the file has no `---` frontmatter block at its head.
    pub frontmatter: bool,
    /// The frontmatter's own `name`, when it has one.
    pub declared_name: Option<String>,
}

impl Definition {
    /// The names the store may record this definition under: bare, and
    /// `plugin:name` for a plugin's.
    pub fn store_names(&self) -> Vec<String> {
        let mut names = vec![self.name.clone()];
        if let Scope::Plugin(p) = &self.scope {
            names.push(format!("{p}:{}", self.name));
        }
        names
    }
}

// ---------------------------------------------------------------- frontmatter

/// A tolerant reading of a YAML frontmatter block: scalars, `|`/`>` block
/// scalars, `- item` lists and `[a, b]` inline lists (across lines), each
/// flattened to one string; lists join with ", ". `None` when the text
/// does not open with `---` or the block never closes.
pub fn parse_frontmatter(text: &str) -> Option<Vec<(String, String)>> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    // (key, lines so far, inside an unclosed `[`)
    let mut current: Option<(String, Vec<String>, bool)> = None;
    let mut closed = false;
    for line in lines {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        let continuation =
            line.starts_with(' ') || line.starts_with('\t') || line.trim().is_empty();
        if let Some((_, buf, inline_open)) = current.as_mut()
            && (continuation || *inline_open)
        {
            if *inline_open && line.contains(']') {
                *inline_open = false;
            }
            buf.push(line.to_string());
            continue;
        }
        if let Some((k, buf, _)) = current.take() {
            fields.push((k, flatten(&buf)));
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            let inline_open = value.starts_with('[') && !value.contains(']');
            current = Some((key.trim().to_string(), vec![value.to_string()], inline_open));
        }
        // a line without a colon at column 0 is not YAML we understand
    }
    if let Some((k, buf, _)) = current.take() {
        fields.push((k, flatten(&buf)));
    }
    closed.then_some(fields)
}

fn flatten(buf: &[String]) -> String {
    let head = buf.first().map(|h| h.trim()).unwrap_or("");
    let rest: Vec<&str> = buf
        .iter()
        .skip(1)
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let block = matches!(head, "|" | ">" | "|-" | ">-" | "|+" | ">+");
    if block || head.is_empty() {
        if rest.iter().all(|l| l.starts_with("- ") || *l == "-") && !rest.is_empty() {
            return rest
                .iter()
                .map(|l| unquote(l.trim_start_matches('-').trim()))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
        }
        if rest.first().is_some_and(|l| l.starts_with('[')) {
            return inline_list(&rest.join(" "));
        }
        return rest.join(if head.starts_with('|') { "\n" } else { " " });
    }
    if head.starts_with('[') {
        let joined = std::iter::once(head)
            .chain(rest.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        return inline_list(&joined);
    }
    // a plain scalar, possibly folded over indented lines
    let mut s = unquote(head);
    for r in rest {
        s.push(' ');
        s.push_str(r);
    }
    s
}

fn inline_list(s: &str) -> String {
    let inner = s.trim().trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(|t| unquote(t.trim()))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// The quoted phrases in a description, lowercased, in order, deduped.
pub fn triggers_of(description: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (open, close) in [('"', '"'), ('\u{201c}', '\u{201d}')] {
        let mut rest = description;
        while let Some(start) = rest.find(open) {
            let after = &rest[start + open.len_utf8()..];
            let Some(end) = after.find(close) else {
                break;
            };
            let phrase = after[..end]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let phrase = phrase.to_lowercase();
            let n = phrase.chars().count();
            if (3..=80).contains(&n) && !out.contains(&phrase) {
                out.push(phrase);
            }
            rest = &after[end + close.len_utf8()..];
        }
    }
    out
}

// ---------------------------------------------------------------- roots

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// `<dir>/<name>/SKILL.md`
    SkillDirs,
    /// `<dir>/<name>.md`
    MdFiles,
    /// `<dir>` is itself the file
    File,
}

#[derive(Debug, Clone)]
struct Root {
    dir: PathBuf,
    kind: Kind,
    layout: Layout,
    scope: Scope,
}

fn std_dirs(base: &Path, scope: Scope, out: &mut Vec<Root>) {
    out.push(Root {
        dir: base.join("skills"),
        kind: Kind::Skill,
        layout: Layout::SkillDirs,
        scope: scope.clone(),
    });
    out.push(Root {
        dir: base.join("agents"),
        kind: Kind::Agent,
        layout: Layout::MdFiles,
        scope: scope.clone(),
    });
    out.push(Root {
        dir: base.join("commands"),
        kind: Kind::Command,
        layout: Layout::MdFiles,
        scope,
    });
}

fn agy_plugin(name: String, dir: &Path, out: &mut Vec<Root>) {
    out.push(Root {
        dir: dir.join("skills"),
        kind: Kind::Skill,
        layout: Layout::SkillDirs,
        scope: Scope::Plugin(name.clone()),
    });
    out.push(Root {
        dir: dir.join("workflows"),
        kind: Kind::Command,
        layout: Layout::MdFiles,
        scope: Scope::Plugin(name),
    });
}

fn roots(harness: Harness, cwd: &Path, home: &Path) -> Vec<Root> {
    let mut r = Vec::new();
    match harness {
        Harness::Claude => {
            std_dirs(&cwd.join(".claude"), Scope::Project, &mut r);
            std_dirs(&home.join(".claude"), Scope::Home, &mut r);
            for (name, path) in claude_plugins(&home.join(".claude").join("plugins")) {
                std_dirs(&path, Scope::Plugin(name), &mut r);
            }
        }
        Harness::Codex => {
            r.push(Root {
                dir: cwd.join("AGENTS.md"),
                kind: Kind::Instructions,
                layout: Layout::File,
                scope: Scope::Project,
            });
            r.push(Root {
                dir: home.join(".codex").join("skills"),
                kind: Kind::Skill,
                layout: Layout::SkillDirs,
                scope: Scope::Home,
            });
            for (name, path) in codex_plugins(&home.join(".codex").join("plugins").join("cache")) {
                r.push(Root {
                    dir: path.join("skills"),
                    kind: Kind::Skill,
                    layout: Layout::SkillDirs,
                    scope: Scope::Plugin(name),
                });
            }
        }
        Harness::Antigravity => {
            let agents = cwd.join(".agents");
            r.push(Root {
                dir: agents.join("skills"),
                kind: Kind::Skill,
                layout: Layout::SkillDirs,
                scope: Scope::Project,
            });
            r.push(Root {
                dir: agents.join("workflows"),
                kind: Kind::Command,
                layout: Layout::MdFiles,
                scope: Scope::Project,
            });
            for (name, path) in subdirs(&agents.join("plugins")) {
                agy_plugin(name, &path, &mut r);
            }
            let cfg = home.join(".gemini").join("config");
            r.push(Root {
                dir: cfg.join("skills"),
                kind: Kind::Skill,
                layout: Layout::SkillDirs,
                scope: Scope::Home,
            });
            for (name, path) in subdirs(&cfg.join("plugins")) {
                agy_plugin(name, &path, &mut r);
            }
        }
    }
    r
}

/// Sorted `(name, path)` of a directory's subdirectories; empty when it
/// is missing.
fn subdirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(n, _)| !n.starts_with('.'))
        .collect();
    out.sort();
    out
}

/// Claude's installed plugins: `installed_plugins.json` maps
/// `name@marketplace` to install records with an `installPath`.
fn claude_plugins(plugins_dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(text) = std::fs::read_to_string(plugins_dir.join("installed_plugins.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(map) = v.get("plugins").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, records) in map {
        let name = key.split('@').next().unwrap_or(key).to_string();
        let paths: Vec<&serde_json::Value> = match records {
            serde_json::Value::Array(a) => a.iter().collect(),
            other => vec![other],
        };
        for rec in paths {
            if let Some(p) = rec.get("installPath").and_then(|p| p.as_str()) {
                out.push((name.clone(), PathBuf::from(p)));
            }
        }
    }
    out.sort();
    out
}

/// Codex's plugin cache: `<marketplace>/<plugin>/<version>/`; the newest
/// version directory (by modification time) stands for the plugin.
fn codex_plugins(cache: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for (_, marketplace) in subdirs(cache) {
        for (plugin, dir) in subdirs(&marketplace) {
            let newest = subdirs(&dir)
                .into_iter()
                .filter(|(_, v)| v.join("skills").is_dir() || v.join(".codex-plugin").is_dir())
                .max_by_key(|(_, v)| std::fs::metadata(v).and_then(|m| m.modified()).ok());
            if let Some((_, version)) = newest {
                out.push((plugin, version));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- scan

/// Everything one harness could trigger from `cwd` for a user at `home`.
pub fn inventory(harness: Harness, cwd: &Path, home: &Path) -> Vec<Definition> {
    let mut out: Vec<Definition> = Vec::new();
    for root in roots(harness, cwd, home) {
        for def in scan(harness, &root) {
            if !out.iter().any(|d| d.kind == def.kind && d.name == def.name) {
                out.push(def);
            }
        }
    }
    out
}

/// All three harnesses.
pub fn inventory_all(cwd: &Path, home: &Path) -> Vec<Definition> {
    [Harness::Claude, Harness::Codex, Harness::Antigravity]
        .into_iter()
        .flat_map(|h| inventory(h, cwd, home))
        .collect()
}

fn scan(harness: Harness, root: &Root) -> Vec<Definition> {
    let mut out = Vec::new();
    match root.layout {
        Layout::SkillDirs => {
            for (name, dir) in subdirs(&root.dir) {
                let file = dir.join("SKILL.md");
                if file.is_file()
                    && let Some(d) = read_definition(harness, root, &file, name)
                {
                    out.push(d);
                }
            }
        }
        Layout::MdFiles => {
            let Ok(rd) = std::fs::read_dir(&root.dir) else {
                return out;
            };
            let mut files: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
                .collect();
            files.sort();
            for file in files {
                let stem = file
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if let Some(d) = read_definition(harness, root, &file, stem) {
                    out.push(d);
                }
            }
        }
        Layout::File => {
            if root.dir.is_file() {
                let name = root
                    .dir
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if let Some(d) = read_definition(harness, root, &root.dir, name) {
                    out.push(d);
                }
            }
        }
    }
    out
}

fn read_definition(
    harness: Harness,
    root: &Root,
    path: &Path,
    default_name: String,
) -> Option<Definition> {
    let text = std::fs::read_to_string(path).ok()?;
    let fm = parse_frontmatter(&text);
    let get = |k: &str| -> Option<String> {
        fm.as_ref().and_then(|f| {
            f.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(k))
                .map(|(_, v)| v.clone())
        })
    };
    let declared_name = get("name")
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    let name = match root.kind {
        Kind::Skill | Kind::Agent => declared_name.clone().unwrap_or(default_name),
        Kind::Command | Kind::Instructions => default_name,
    };
    let description = get("description").unwrap_or_default();
    let tools = ["tools", "allowed-tools", "allowed_tools", "allowedTools"]
        .iter()
        .find_map(|k| get(k))
        .map(|t| {
            t.split(',')
                .map(|s| unquote(s.trim()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some(Definition {
        harness,
        kind: root.kind,
        name,
        path: path.to_path_buf(),
        scope: root.scope.clone(),
        triggers: triggers_of(&description),
        description: description.split_whitespace().collect::<Vec<_>>().join(" "),
        tools,
        model: get("model")
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty()),
        frontmatter: fm.is_some(),
        declared_name,
    })
}

// ---------------------------------------------------------------- lint

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub rule: &'static str,
    pub message: String,
}

pub struct LintContext<'a> {
    /// Tool names the harness has — a built-in floor plus every tool the
    /// store has seen it call. Empty means unknown: the rule is skipped.
    pub known_tools: &'a HashSet<String>,
    pub prices: Option<&'a PriceTable>,
}

/// Claude Code's own tools as they appear in transcripts — the floor;
/// the store adds whatever else it has seen the CLI call.
pub const CLAUDE_TOOLS: &[&str] = &[
    "Agent",
    "AskUserQuestion",
    "Bash",
    "BashOutput",
    "Edit",
    "EnterPlanMode",
    "ExitPlanMode",
    "Glob",
    "Grep",
    "KillShell",
    "LS",
    "MultiEdit",
    "NotebookEdit",
    "NotebookRead",
    "Read",
    "Skill",
    "SlashCommand",
    "Task",
    "TodoWrite",
    "ToolSearch",
    "WebFetch",
    "WebSearch",
    "Write",
];

/// The known tool set for a harness: its floor plus what the store saw.
pub fn known_tools(harness: Harness, seen: impl IntoIterator<Item = String>) -> HashSet<String> {
    let mut set: HashSet<String> = match harness {
        Harness::Claude => CLAUDE_TOOLS.iter().map(|t| t.to_string()).collect(),
        // no verified floor for these: only what was measured
        Harness::Codex | Harness::Antigravity => HashSet::new(),
    };
    set.extend(seen);
    set
}

/// Claude Code's cap on a skill description.
pub const DESCRIPTION_MAX: usize = 1024;

/// Model names a Claude agent may use without naming a priced model.
const MODEL_ALIASES: &[&str] = &["inherit", "default", "sonnet", "opus", "haiku"];

pub fn lint(def: &Definition, ctx: &LintContext) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut push = |level: Level, rule: &'static str, message: String| {
        out.push(Finding {
            level,
            rule,
            message,
        })
    };
    if def.kind == Kind::Instructions {
        return out;
    }
    if !def.frontmatter {
        if def.kind == Kind::Command {
            push(
                Level::Warn,
                "no-frontmatter",
                "no frontmatter: the command runs, but has no description for the CLI to show"
                    .into(),
            );
        } else {
            push(
                Level::Error,
                "no-frontmatter",
                "no `---` frontmatter block: the CLI cannot read a name or description".into(),
            );
        }
        return out;
    }
    if def.kind == Kind::Skill
        && let Some(declared) = &def.declared_name
        && let Some(dir) = def.path.parent().and_then(|p| p.file_name())
        && dir.to_string_lossy() != declared.as_str()
    {
        push(
            Level::Warn,
            "name-mismatch",
            format!(
                "frontmatter name {declared:?} differs from directory {:?}",
                dir.to_string_lossy()
            ),
        );
    }
    let desc = def.description.trim();
    if desc.is_empty() {
        let level = if def.kind == Kind::Command {
            Level::Warn
        } else {
            Level::Error
        };
        push(
            level,
            "empty-description",
            "empty description: nothing tells the model when to use this".into(),
        );
    } else if desc.split_whitespace().count() == 1 {
        push(
            Level::Warn,
            "one-word-description",
            format!("one-word description {desc:?}"),
        );
    }
    if def.kind == Kind::Skill && desc.chars().count() > DESCRIPTION_MAX {
        push(
            Level::Warn,
            "long-description",
            format!(
                "description is {} characters; Claude Code reads at most {DESCRIPTION_MAX}",
                desc.chars().count()
            ),
        );
    }
    if def.kind == Kind::Skill
        && def.harness == Harness::Claude
        && def.triggers.is_empty()
        && !desc.is_empty()
    {
        push(
            Level::Warn,
            "no-triggers",
            "the description quotes no trigger phrases (\"…\"), so missed triggers cannot be measured".into(),
        );
    }
    if !ctx.known_tools.is_empty() {
        for tool in &def.tools {
            let base = tool.split('(').next().unwrap_or(tool).trim();
            if base.is_empty() || base.starts_with("mcp__") || base == "*" {
                continue;
            }
            if !ctx.known_tools.contains(base) {
                push(
                    Level::Warn,
                    "unknown-tool",
                    format!(
                        "tool {base:?} is not one the {} CLI has been seen to call",
                        def.harness.as_str()
                    ),
                );
            }
        }
    }
    if def.kind == Kind::Agent
        && let Some(model) = &def.model
        && let Some(prices) = ctx.prices
        && !MODEL_ALIASES.contains(&model.to_ascii_lowercase().as_str())
        && prices.find(model).is_none()
    {
        push(
            Level::Warn,
            "unknown-model",
            format!("model {model:?} is not in the price table: its runs will be unpriced"),
        );
    }
    out
}

// ---------------------------------------------------------------- join

/// A skill as the author sees it: on disk, in the store, or both.
#[derive(Debug, Clone)]
pub struct SkillReport {
    pub name: String,
    pub def: Option<Definition>,
    pub stat: Option<SkillStat>,
    /// Prompts that contained one of the skill's trigger phrases in a turn
    /// that did not load it. A hint, measured on the stored prompts.
    pub missed: i64,
}

impl SkillReport {
    pub fn note(&self) -> &'static str {
        match (&self.def, &self.stat) {
            (None, _) => "not on disk",
            (Some(_), None) => "never triggered",
            _ => "",
        }
    }
}

pub fn skill_reports(
    defs: &[Definition],
    stats: &[SkillStat],
    prompts: &[PromptRow],
) -> Vec<SkillReport> {
    let mut out = Vec::new();
    let mut used: HashSet<usize> = HashSet::new();
    for def in defs.iter().filter(|d| d.kind == Kind::Skill) {
        let names = def.store_names();
        let stat_idx = stats.iter().position(|s| names.contains(&s.skill));
        if let Some(i) = stat_idx {
            used.insert(i);
        }
        let missed = if def.triggers.is_empty() {
            0
        } else {
            prompts
                .iter()
                .filter(|p| {
                    let text = p.input.to_lowercase();
                    def.triggers.iter().any(|t| text.contains(t.as_str()))
                        && !p.skills.iter().any(|s| names.contains(s))
                })
                .count() as i64
        };
        out.push(SkillReport {
            name: def.name.clone(),
            def: Some(def.clone()),
            stat: stat_idx.map(|i| stats[i].clone()),
            missed,
        });
    }
    for (i, s) in stats.iter().enumerate() {
        if !used.contains(&i) {
            out.push(SkillReport {
                name: s.skill.clone(),
                def: None,
                stat: Some(s.clone()),
                missed: 0,
            });
        }
    }
    out.sort_by(|a, b| {
        let loaded = |r: &SkillReport| r.stat.as_ref().map(|s| s.turns_loaded).unwrap_or(0);
        loaded(b).cmp(&loaded(a)).then_with(|| a.name.cmp(&b.name))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn frontmatter_shapes_flatten_to_strings() {
        let text = "---\nname: demo\ndescription: |\n  Use when the user says \"make a demo\"\n  or \"demo it\".\nallowed-tools:\n  [\n    \"Read\",\n    \"Bash(git:*)\"\n  ]\ntools:\n  - Grep\n  - Glob\nmodel: haiku\ninline: [a, b]\n---\n# body\n";
        let fm = parse_frontmatter(text).unwrap();
        let get = |k: &str| fm.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("name"), Some("demo"));
        assert_eq!(
            get("description"),
            Some("Use when the user says \"make a demo\"\nor \"demo it\".")
        );
        assert_eq!(get("allowed-tools"), Some("Read, Bash(git:*)"));
        assert_eq!(get("tools"), Some("Grep, Glob"));
        assert_eq!(get("model"), Some("haiku"));
        assert_eq!(get("inline"), Some("a, b"));
        assert_eq!(parse_frontmatter("# no frontmatter\n"), None);
        assert_eq!(parse_frontmatter("---\nname: x\n"), None, "never closed");
        assert_eq!(
            triggers_of(
                "Say \"make a demo\", “demo it”, or \"x\" (too short) — \"Make A Demo\" again"
            ),
            vec!["make a demo", "demo it"]
        );
    }

    #[test]
    fn claude_roots_shadow_project_over_home_over_plugins() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("proj");
        let home = temp.path().join("home");
        write(
            &cwd.join(".claude/skills/deploy/SKILL.md"),
            "---\nname: deploy\ndescription: Use when asked to \"deploy the app\".\n---\n",
        );
        write(&cwd.join(".claude/skills/bare/SKILL.md"), "# just prose\n");
        write(
            &home.join(".claude/skills/deploy/SKILL.md"),
            "---\nname: deploy\ndescription: the home one\n---\n",
        );
        write(
            &home.join(".claude/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews code\ntools: Read, Grep\nmodel: sonnet\n---\n",
        );
        write(
            &home.join(".claude/commands/ship.md"),
            "---\ndescription: ship it\nallowed-tools: [\"Bash\"]\n---\nDo the thing\n",
        );
        let plugin = temp.path().join("cache/market/tools/abc123");
        write(
            &plugin.join("skills/plugin-skill/SKILL.md"),
            "---\nname: plugin-skill\ndescription: from a plugin, \"plug it in\"\n---\n",
        );
        write(
            &plugin.join("skills/deploy/SKILL.md"),
            "---\nname: deploy\ndescription: shadowed by both\n---\n",
        );
        write(
            &home.join(".claude/plugins/installed_plugins.json"),
            &format!(
                "{{\"version\":2,\"plugins\":{{\"tools@market\":[{{\"installPath\":\"{}\"}}]}}}}",
                plugin.display()
            ),
        );
        let defs = inventory(Harness::Claude, &cwd, &home);
        let names: Vec<(Kind, &str, String)> = defs
            .iter()
            .map(|d| (d.kind, d.name.as_str(), d.scope.label()))
            .collect();
        assert_eq!(
            names,
            vec![
                (Kind::Skill, "bare", "project".to_string()),
                (Kind::Skill, "deploy", "project".to_string()),
                (Kind::Agent, "reviewer", "home".to_string()),
                (Kind::Command, "ship", "home".to_string()),
                (Kind::Skill, "plugin-skill", "plugin:tools".to_string()),
            ]
        );
        let deploy = defs.iter().find(|d| d.name == "deploy").unwrap();
        assert_eq!(deploy.triggers, vec!["deploy the app"]);
        let bare = defs.iter().find(|d| d.name == "bare").unwrap();
        assert!(
            !bare.frontmatter,
            "listed by directory name without frontmatter"
        );
        let reviewer = defs.iter().find(|d| d.name == "reviewer").unwrap();
        assert_eq!(reviewer.tools, vec!["Read", "Grep"]);
        assert_eq!(reviewer.model.as_deref(), Some("sonnet"));
        let ship = defs.iter().find(|d| d.name == "ship").unwrap();
        assert_eq!(ship.tools, vec!["Bash"]);
        let plugged = defs.iter().find(|d| d.name == "plugin-skill").unwrap();
        assert_eq!(
            plugged.store_names(),
            vec!["plugin-skill".to_string(), "tools:plugin-skill".to_string()]
        );
    }

    #[test]
    fn codex_and_agy_roots_are_read() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("proj");
        let home = temp.path().join("home");
        write(&cwd.join("AGENTS.md"), "# Repo rules\n");
        write(
            &home.join(".codex/skills/research/SKILL.md"),
            "---\nname: research\ndescription: Conduct research when asked for \"deep research\".\n---\n",
        );
        let old = home.join(".codex/plugins/cache/market/deep/0.1.9");
        let new = home.join(".codex/plugins/cache/market/deep/0.1.14");
        write(
            &old.join("skills/deep/SKILL.md"),
            "---\nname: deep\ndescription: old\n---\n",
        );
        write(
            &new.join("skills/deep/SKILL.md"),
            "---\nname: deep\ndescription: new\n---\n",
        );
        // the newer version is the more recently modified directory
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::open(&new)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let codex = inventory(Harness::Codex, &cwd, &home);
        let names: Vec<(Kind, &str, String)> = codex
            .iter()
            .map(|d| (d.kind, d.name.as_str(), d.scope.label()))
            .collect();
        assert_eq!(
            names,
            vec![
                (Kind::Instructions, "AGENTS.md", "project".to_string()),
                (Kind::Skill, "research", "home".to_string()),
                (Kind::Skill, "deep", "plugin:deep".to_string()),
            ]
        );
        assert_eq!(codex[2].description, "new");

        write(
            &cwd.join(".agents/skills/local/SKILL.md"),
            "---\nname: local\ndescription: project skill\n---\n",
        );
        write(
            &cwd.join(".agents/workflows/release.md"),
            "---\ndescription: release flow\n---\n",
        );
        write(
            &home.join(".gemini/config/skills/agy-customizations/SKILL.md"),
            "---\nname: agy-customizations\ndescription: home skill\n---\n",
        );
        write(
            &home.join(".gemini/config/plugins/extra/skills/bonus/SKILL.md"),
            "---\nname: bonus\ndescription: plugin skill\n---\n",
        );
        let agy = inventory(Harness::Antigravity, &cwd, &home);
        let names: Vec<(Kind, &str, String)> = agy
            .iter()
            .map(|d| (d.kind, d.name.as_str(), d.scope.label()))
            .collect();
        assert_eq!(
            names,
            vec![
                (Kind::Skill, "local", "project".to_string()),
                (Kind::Command, "release", "project".to_string()),
                (Kind::Skill, "agy-customizations", "home".to_string()),
                (Kind::Skill, "bonus", "plugin:extra".to_string()),
            ]
        );
        // an empty tree is simply empty
        assert!(
            inventory_all(&temp.path().join("nowhere"), &temp.path().join("nohome")).is_empty()
        );
    }

    fn def(kind: Kind, name: &str, text: &str) -> Definition {
        let temp = tempfile::tempdir().unwrap();
        let path = match kind {
            Kind::Skill => temp.path().join(name).join("SKILL.md"),
            _ => temp.path().join(format!("{name}.md")),
        };
        write(&path, text);
        let root = Root {
            dir: temp.path().to_path_buf(),
            kind,
            layout: Layout::File,
            scope: Scope::Project,
        };
        let mut d = read_definition(Harness::Claude, &root, &path, name.to_string()).unwrap();
        // keep the temp dir's path text alive in the definition only
        d.path = path;
        d
    }

    #[test]
    fn lint_flags_what_stops_a_definition_working() {
        let known = known_tools(Harness::Claude, ["Artifact".to_string()]);
        let prices = PriceTable::builtin();
        let ctx = LintContext {
            known_tools: &known,
            prices: Some(&prices),
        };
        let rules = |d: &Definition| -> Vec<(Level, &'static str)> {
            lint(d, &ctx)
                .into_iter()
                .map(|f| (f.level, f.rule))
                .collect()
        };
        // well-formed: nothing
        let good = def(
            Kind::Skill,
            "good",
            "---\nname: good\ndescription: Use when the user asks to \"ship the release\".\nallowed-tools: Read, Bash(git:*), Artifact, mcp__x__y\n---\n",
        );
        assert!(rules(&good).is_empty(), "{:?}", lint(&good, &ctx));
        // no frontmatter at all
        let bare = def(Kind::Skill, "bare", "# prose only\n");
        assert_eq!(rules(&bare), vec![(Level::Error, "no-frontmatter")]);
        let bare_cmd = def(Kind::Command, "cmd", "just do it\n");
        assert_eq!(rules(&bare_cmd), vec![(Level::Warn, "no-frontmatter")]);
        // empty and one-word descriptions
        let empty = def(
            Kind::Skill,
            "empty",
            "---\nname: empty\ndescription:\n---\n",
        );
        assert!(rules(&empty).contains(&(Level::Error, "empty-description")));
        let terse = def(
            Kind::Agent,
            "terse",
            "---\nname: terse\ndescription: Reviews\n---\n",
        );
        assert_eq!(rules(&terse), vec![(Level::Warn, "one-word-description")]);
        // an unknown tool and an unknown model
        let odd = def(
            Kind::Agent,
            "odd",
            "---\nname: odd\ndescription: Does odd things when asked\ntools: Read, Telepathy\nmodel: gpt-9-ultra\n---\n",
        );
        assert_eq!(
            rules(&odd),
            vec![
                (Level::Warn, "unknown-tool"),
                (Level::Warn, "unknown-model")
            ]
        );
        let aliased = def(
            Kind::Agent,
            "aliased",
            "---\nname: aliased\ndescription: Uses an alias model\nmodel: inherit\n---\n",
        );
        assert!(rules(&aliased).is_empty());
        // no trigger phrases on a claude skill, name mismatch, over-long
        let quiet = def(
            Kind::Skill,
            "quiet",
            "---\nname: loud\ndescription: Helps with things sometimes\n---\n",
        );
        assert_eq!(
            rules(&quiet),
            vec![(Level::Warn, "name-mismatch"), (Level::Warn, "no-triggers")]
        );
        let long = def(
            Kind::Skill,
            "long",
            &format!(
                "---\nname: long\ndescription: \"go long\" {}\n---\n",
                "x ".repeat(600)
            ),
        );
        assert!(rules(&long).contains(&(Level::Warn, "long-description")));
        // with no known tool list the tool rule stays quiet
        let none = HashSet::new();
        let quiet_ctx = LintContext {
            known_tools: &none,
            prices: None,
        };
        assert!(lint(&odd, &quiet_ctx).is_empty());
    }

    #[test]
    fn reports_join_disk_to_store_and_count_missed_triggers() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("proj");
        write(
            &cwd.join(".claude/skills/deploy/SKILL.md"),
            "---\nname: deploy\ndescription: Use when asked to \"deploy the app\" or \"ship it\".\n---\n",
        );
        write(
            &cwd.join(".claude/skills/silent/SKILL.md"),
            "---\nname: silent\ndescription: never mentioned\n---\n",
        );
        let defs = inventory(Harness::Claude, &cwd, &temp.path().join("nohome"));
        let stat = |skill: &str, loaded: i64| SkillStat {
            skill: skill.into(),
            turns_loaded: loaded,
            generations: 0,
            tools: 0,
            tokens: None,
            cost: None,
            turns_unused: 0,
            first_ns: 0,
            last_ns: 0,
        };
        let stats = vec![stat("deploy", 2), stat("ghost", 5)];
        let prompt = |input: &str, skills: &[&str]| PromptRow {
            trace_id: input.to_string(),
            input: input.to_string(),
            skills: skills.iter().map(|s| s.to_string()).collect(),
        };
        let prompts = vec![
            prompt("please Deploy the App now", &["deploy"]), // loaded: fine
            prompt("can you ship it today", &[]),             // missed
            prompt("SHIP IT and deploy the app", &["other"]), // missed once
            prompt("unrelated", &[]),
        ];
        let reports = skill_reports(&defs, &stats, &prompts);
        let names: Vec<&str> = reports.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["ghost", "deploy", "silent"],
            "by turns loaded, then name"
        );
        let deploy = &reports[1];
        assert_eq!(deploy.missed, 2);
        assert_eq!(deploy.stat.as_ref().unwrap().turns_loaded, 2);
        assert_eq!(deploy.note(), "");
        assert_eq!(reports[0].note(), "not on disk");
        assert_eq!(reports[2].note(), "never triggered");
        assert_eq!(reports[2].missed, 0, "no triggers, nothing to miss");
    }
}
