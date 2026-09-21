//! The pattern library on disk: the embedded skills, verifier and
//! templates, and the scaffolder that writes a workspace's contract files
//! without ever overwriting one. The scaffolder writes the *effective*
//! text: a file in the configuration library (`crate::assets`) replaces
//! the embedded one of the same name.
//!
//! # Agents per harness
//!
//! A loop agent is kept in Claude's file shape (frontmatter `name`,
//! `description`, `tools`, `model`, then the body). Every installed copy
//! gets the `[agent] preamble` of `prompts.toml` as a `## Baseline` section
//! right after the frontmatter (`with_preamble`). Claude Code receives the
//! file as is; Codex receives `codex_agent_toml`.
//!
//! What the Codex TOML carries was checked against the installed CLI
//! (codex-cli 0.155.1, 2026-09-21, by reading the config field names the
//! binary deserialises; the CLI ships no local docs): an agent role is
//! `[agents.<role>] { description, config_file, nickname_candidates }` in
//! `config.toml`, and `config_file` names a config overlay whose keys are
//! the ordinary config keys, among them `model`, `model_reasoning_effort`,
//! `sandbox_mode` and `developer_instructions`. So the file keeps the
//! `name`, `description` and `[system_prompt] content` keys the loop
//! design chose, and adds `developer_instructions` with the same text and,
//! when the frontmatter names a model that is not a Claude alias
//! (`sonnet`, `opus`, `haiku`, `inherit`), `model`. Claude's `tools` list
//! has no Codex counterpart: the nearest key, `sandbox_mode = "read-only"`,
//! would also stop the verifier's test run from writing build output, so
//! the tool list is left out on purpose.

use super::{
    BUDGET_MD, CONSTRAINTS_MD, GATE_YAML, LEDGER_JSON, LOOP_MD, Level, Pattern, RUN_LOG_MD,
    format_interval, format_tokens, parse_timestamp,
};
use crate::harness::Harness;
use std::path::{Path, PathBuf};

macro_rules! skill {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!("../../loops/skills/", $name, "/SKILL.md")),
        )
    };
}

/// Every embedded skill as (name, SKILL.md text).
pub fn embedded_skills() -> Vec<(&'static str, &'static str)> {
    vec![
        skill!("loop-triage"),
        skill!("loop-pr-triage"),
        skill!("loop-ci-triage"),
        skill!("loop-post-merge"),
        skill!("loop-dependency-triage"),
        skill!("loop-changelog"),
        skill!("loop-issue-triage"),
        skill!("loop-issue-pick"),
        skill!("loop-harness-audit"),
        skill!("loop-fix"),
        skill!("loop-rules"),
    ]
}

pub fn embedded_skill(name: &str) -> Option<&'static str> {
    embedded_skills()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, text)| text)
}

/// The embedded verifier agent, Claude's file shape (frontmatter + body).
pub fn verifier_body() -> &'static str {
    include_str!("../../loops/agents/loop-verifier.md")
}

/// Every embedded loop agent as (name, markdown): the verifier (runs the
/// tests) and the reviewer (reads the diff), the two checkers a pattern
/// lists as `agents = ["loop-verifier", "loop-reviewer"]` to require both.
pub fn embedded_agents() -> Vec<(&'static str, &'static str)> {
    vec![
        ("loop-verifier", verifier_body()),
        (
            "loop-reviewer",
            include_str!("../../loops/agents/loop-reviewer.md"),
        ),
    ]
}

/// The embedded text of one loop agent.
pub fn embedded_agent(name: &str) -> Option<&'static str> {
    embedded_agents()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| t)
}

/// The embedded workspace templates as (file name, text).
pub const TEMPLATES: &[(&str, &str)] = &[
    ("STATE.md", include_str!("../../loops/templates/STATE.md")),
    ("LOOP.md", include_str!("../../loops/templates/LOOP.md")),
    (
        "loop-budget.md",
        include_str!("../../loops/templates/loop-budget.md"),
    ),
    (
        "loop-run-log.md",
        include_str!("../../loops/templates/loop-run-log.md"),
    ),
    (
        "loop-constraints.md",
        include_str!("../../loops/templates/loop-constraints.md"),
    ),
    ("gate.yaml", include_str!("../../loops/templates/gate.yaml")),
    (
        "loop-ledger.json",
        include_str!("../../loops/templates/loop-ledger.json"),
    ),
    ("AGENTS.md", include_str!("../../loops/templates/AGENTS.md")),
];

/// The embedded template text; `assets::template` gives the effective one.
pub fn template(name: &str) -> Option<&'static str> {
    TEMPLATES.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// The caps written into `loop-budget.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caps {
    pub max_runs_per_day: u32,
    pub max_tokens_per_day: u64,
    /// The model the scaffolded `loop-verifier` agent declares. Empty
    /// leaves the file as the library wrote it (`model: inherit`: the
    /// verifier runs on the model of the run that calls it).
    pub verifier_model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScaffoldReport {
    pub written: Vec<PathBuf>,
    pub skipped: Vec<PathBuf>,
    /// Agents the pattern lists that neither the library nor the embedded
    /// set has; nothing was written for them.
    pub missing_agents: Vec<String>,
}

impl ScaffoldReport {
    fn put(&mut self, path: PathBuf, content: &str) -> std::io::Result<()> {
        if path.exists() {
            self.skipped.push(path);
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
        self.written.push(path);
        Ok(())
    }
}

/// Wraps the verifier body into Codex's agent TOML.
pub fn codex_verifier_toml(body: &str) -> String {
    codex_agent_toml("loop-verifier", body)
}

/// Claude model aliases that are not Codex model ids; never copied into
/// the Codex TOML.
const CLAUDE_MODEL_ALIASES: [&str; 4] = ["sonnet", "opus", "haiku", "inherit"];

/// Rewrites the `model:` line of a Claude-shaped agent file, adding one
/// when the frontmatter has none. A file without frontmatter is returned
/// unchanged: agent-mux does not invent a header for someone else's file.
pub fn set_agent_model(body: &str, model: &str) -> String {
    let Some(rest) = body.strip_prefix("---\n") else {
        return body.to_string();
    };
    let Some(end) = rest.find("\n---") else {
        return body.to_string();
    };
    let (front, tail) = rest.split_at(end);
    let mut lines: Vec<String> = front.lines().map(str::to_string).collect();
    match lines.iter_mut().find(|l| l.starts_with("model:")) {
        Some(l) => *l = format!("model: {model}"),
        None => lines.push(format!("model: {model}")),
    }
    format!("---\n{}{}", lines.join("\n"), tail)
}

/// Wraps a Claude-shaped agent file (frontmatter + body) into Codex's
/// agent TOML under `name`: `name`, `description`, `model` when the
/// frontmatter names one that is not a Claude alias, `developer_instructions`
/// with the body, and the body again under `[system_prompt] content`
/// (module docs say what was probed).
pub fn codex_agent_toml(name: &str, body: &str) -> String {
    let parts = split_agent_frontmatter(body);
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let escaped = parts.body.replace("\"\"\"", "\\\"\"\"");
    let escaped = escaped.trim_end();
    let mut out = format!(
        "name = \"{}\"\ndescription = \"{}\"\n",
        quote(name),
        quote(&parts.description)
    );
    if let Some(model) = parts.model.as_deref().map(str::trim).filter(|m| {
        !m.is_empty() && !CLAUDE_MODEL_ALIASES.contains(&m.to_ascii_lowercase().as_str())
    }) {
        out.push_str(&format!("model = \"{}\"\n", quote(model)));
    }
    out.push_str(&format!(
        "developer_instructions = \"\"\"\n{escaped}\n\"\"\"\n\n[system_prompt]\ncontent = \"\"\"\n{escaped}\n\"\"\"\n"
    ));
    out
}

/// The pieces of a Claude agent file the Codex TOML uses.
struct AgentParts {
    description: String,
    model: Option<String>,
    body: String,
}

/// Splits a Claude agent file; the description falls back to a fixed
/// sentence when the frontmatter has none.
fn split_agent_frontmatter(text: &str) -> AgentParts {
    let mut parts = AgentParts {
        description: "Independent verifier for loop-produced changes.".to_string(),
        model: None,
        body: text.to_string(),
    };
    let Some((front, body)) = split_front_lines(text) else {
        return parts;
    };
    for line in front {
        if let Some(d) = line.strip_prefix("description:") {
            parts.description = d.trim().to_string();
        } else if let Some(m) = line.strip_prefix("model:") {
            parts.model = Some(m.trim().to_string());
        }
    }
    parts.body = body;
    parts
}

/// The frontmatter lines (between the `---` fences) and the body after
/// them, or `None` when the text has no frontmatter.
fn split_front_lines(text: &str) -> Option<(Vec<&str>, String)> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return None;
    }
    let mut front = Vec::new();
    let mut body_start = None;
    let mut offset = text.lines().next().map(|l| l.len() + 1).unwrap_or(0);
    for line in lines {
        let len = line.len() + 1;
        if line.trim() == "---" {
            body_start = Some(offset + len);
            break;
        }
        front.push(line);
        offset += len;
    }
    let start = body_start?;
    let body = text.get(start..).unwrap_or("").trim_start_matches('\n');
    Some((front, body.to_string()))
}

/// The agent text with `preamble` inserted as a `## Baseline` section right
/// after the frontmatter (or at the top when there is none). An empty
/// preamble leaves the text unchanged.
pub fn with_preamble(body: &str, preamble: &str) -> String {
    let preamble = preamble.trim();
    if preamble.is_empty() {
        return body.to_string();
    }
    let section = format!("## Baseline\n\n{preamble}\n\n");
    match split_front_lines(body) {
        Some((front, rest)) => {
            let mut out = String::from("---\n");
            for line in front {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("---\n\n");
            out.push_str(&section);
            out.push_str(&rest);
            out
        }
        None => format!("{section}{body}"),
    }
}

/// The file a harness receives for a loop agent: the Claude text with the
/// preamble, or its Codex TOML.
pub fn agent_file(harness: Harness, name: &str, body: &str, preamble: &str) -> String {
    let text = with_preamble(body, preamble);
    match harness {
        Harness::Codex => codex_agent_toml(name, &text),
        _ => text,
    }
}

fn project_name(workspace: &Path) -> String {
    workspace
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("project")
        .to_string()
}

/// Where a harness reads project-level skills.
pub fn project_skills_dir(harness: Harness, workspace: &Path) -> Option<PathBuf> {
    match harness {
        Harness::Claude => Some(workspace.join(".claude").join("skills")),
        Harness::Codex => Some(workspace.join(".codex").join("skills")),
        Harness::Antigravity => None,
    }
}

/// Where the verifier agent goes for a harness.
pub fn verifier_path(harness: Harness, workspace: &Path) -> Option<PathBuf> {
    match harness {
        Harness::Claude => Some(
            workspace
                .join(".claude")
                .join("agents")
                .join("loop-verifier.md"),
        ),
        Harness::Codex => Some(
            workspace
                .join(".codex")
                .join("agents")
                .join("verifier.toml"),
        ),
        Harness::Antigravity => None,
    }
}

/// Where a loop agent goes for a harness: the verifier keeps its historical
/// Codex file name, every other agent is `<name>.md` / `<name>.toml`.
pub fn agent_path(harness: Harness, workspace: &Path, name: &str) -> Option<PathBuf> {
    if name == "loop-verifier" {
        return verifier_path(harness, workspace);
    }
    match harness {
        Harness::Claude => Some(
            workspace
                .join(".claude")
                .join("agents")
                .join(format!("{name}.md")),
        ),
        Harness::Codex => Some(
            workspace
                .join(".codex")
                .join("agents")
                .join(format!("{name}.toml")),
        ),
        Harness::Antigravity => None,
    }
}

fn not_supported() -> std::io::Error {
    std::io::Error::other("Antigravity is not supported for loops")
}

/// Writes every missing contract file and skill for `pattern` into
/// `workspace`; existing files are skipped, never overwritten. Skills,
/// agents and templates come from the configuration library
/// (`assets::root()`) when it has them, else from the embedded set.
pub fn scaffold(
    workspace: &Path,
    pattern: &Pattern,
    harness: Harness,
    level: Level,
    caps: &Caps,
) -> std::io::Result<ScaffoldReport> {
    scaffold_with_library(
        &crate::assets::root(),
        workspace,
        pattern,
        harness,
        level,
        caps,
    )
}

/// `scaffold` reading skills, agents and templates from `library`.
pub fn scaffold_with_library(
    library: &Path,
    workspace: &Path,
    pattern: &Pattern,
    harness: Harness,
    level: Level,
    caps: &Caps,
) -> std::io::Result<ScaffoldReport> {
    use crate::assets;
    let skills_dir = project_skills_dir(harness, workspace).ok_or_else(not_supported)?;
    verifier_path(harness, workspace).ok_or_else(not_supported)?;
    let project = project_name(workspace);
    let mut report = ScaffoldReport::default();
    let template = |name: &str| assets::template(library, name).unwrap_or_default();

    // 1. skills
    for name in &pattern.skills {
        if let Some(text) = assets::loop_skill(library, name) {
            report.put(skills_dir.join(name).join("SKILL.md"), &text)?;
        }
    }
    // 2. agents: the ones the pattern names (the verifier alone by
    // default), each with the preamble of prompts.toml
    let preamble = crate::prompts::Prompts::load(library).agent_preamble;
    let available = assets::loop_agents(library);
    for name in pattern.effective_agents() {
        let Some((_, body)) = available.iter().find(|(n, _)| *n == name) else {
            report.missing_agents.push(name);
            continue;
        };
        let Some(path) = agent_path(harness, workspace, &name) else {
            continue;
        };
        let body = if name == "loop-verifier" && !caps.verifier_model.is_empty() {
            set_agent_model(body, &caps.verifier_model)
        } else {
            body.to_string()
        };
        report.put(path, &agent_file(harness, &name, &body, &preamble))?;
    }
    // 3. state file
    let state = template("STATE.md").replace("{{PROJECT}}", &project);
    report.put(workspace.join(&pattern.state_file), &state)?;
    // 4. LOOP.md
    let gates = pattern
        .human_gates
        .iter()
        .map(|g| format!("- {g}"))
        .collect::<Vec<_>>()
        .join("\n");
    let loop_md = template("LOOP.md")
        .replace("{{PROJECT}}", &project)
        .replace("{{PATTERN}}", &pattern.id)
        .replace("{{CADENCE}}", &format_interval(pattern.default_interval_s))
        .replace("{{LEVEL}}", level.as_str())
        .replace("{{STATE_FILE}}", &pattern.state_file)
        .replace("{{HARNESS}}", harness.as_str())
        .replace("{{GATES}}", &gates);
    report.put(workspace.join(LOOP_MD), &loop_md)?;
    // 5. budget, run log, constraints, gate, ledger
    let spawns = if pattern.verifier {
        "0 (L1) / 2 (L2)"
    } else {
        "0"
    };
    let row = format!(
        "| {} | {} | {} | {} |",
        pattern.name,
        caps.max_runs_per_day,
        format_tokens(caps.max_tokens_per_day),
        spawns
    );
    let budget = template("loop-budget.md")
        .replace("{{PROJECT}}", &project)
        .replace("{{ROW}}", &row);
    report.put(workspace.join(BUDGET_MD), &budget)?;
    let run_log = template("loop-run-log.md").replace("{{PROJECT}}", &project);
    report.put(workspace.join(RUN_LOG_MD), &run_log)?;
    let constraints = template("loop-constraints.md").replace("{{PROJECT}}", &project);
    report.put(workspace.join(CONSTRAINTS_MD), &constraints)?;
    report.put(workspace.join(GATE_YAML), &template("gate.yaml"))?;
    if pattern.breaker {
        let ledger = template("loop-ledger.json")
            .replace("{{GOAL}}", &pattern.goal.replace('"', "'"))
            .replace("{{PATTERN}}", &pattern.id)
            .replace("{{LEVEL}}", level.as_str());
        report.put(workspace.join(LEDGER_JSON), &ledger)?;
    }
    // 6. AGENTS.md
    report.put(workspace.join("AGENTS.md"), &template("AGENTS.md"))?;
    Ok(report)
}

/// One of a workspace's contract files, as the Files tab shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractFile {
    pub name: String,
    pub present: bool,
    /// The state file's `Last run:` is older than 14 days.
    pub stale: bool,
}

/// The `Last run:` timestamp of a state file's text, when it parses.
pub fn last_run(text: &str) -> Option<time::OffsetDateTime> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("Last run:"))
        .and_then(|rest| {
            let token = rest.split_whitespace().next()?;
            parse_timestamp(token)
        })
}

pub const STALE_AFTER: time::Duration = time::Duration::days(14);

/// Presence and staleness of the contract files for `pattern`.
pub fn contract_files(workspace: &Path, pattern: &Pattern) -> Vec<ContractFile> {
    let mut names = vec![
        pattern.state_file.as_str(),
        LOOP_MD,
        BUDGET_MD,
        RUN_LOG_MD,
        CONSTRAINTS_MD,
        GATE_YAML,
    ];
    if pattern.breaker {
        names.push(LEDGER_JSON);
    }
    let now = super::now();
    names
        .into_iter()
        .map(|name| {
            let path = workspace.join(name);
            let present = path.is_file();
            let stale = present
                && name == pattern.state_file
                && std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| last_run(&t))
                    .is_some_and(|t| now - t > STALE_AFTER);
            ContractFile {
                name: name.to_string(),
                present,
                stale,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loops::patterns;

    #[test]
    fn templates_and_skills_are_embedded() {
        assert_eq!(embedded_skills().len(), 11);
        for (name, text) in embedded_skills() {
            assert!(text.contains(&format!("name: {name}")), "{name}");
        }
        for name in [
            "STATE.md",
            "LOOP.md",
            "loop-budget.md",
            "loop-run-log.md",
            "loop-constraints.md",
            "gate.yaml",
            "loop-ledger.json",
            "AGENTS.md",
        ] {
            assert!(template(name).is_some(), "{name}");
        }
        assert!(template("nope").is_none());
        assert!(verifier_body().contains("## Verdict: APPROVE | REJECT | ESCALATE_HUMAN"));
    }

    #[test]
    fn the_codex_verifier_is_toml() {
        let toml_text = codex_verifier_toml(verifier_body());
        let doc: toml::Value = toml::from_str(&toml_text).unwrap();
        assert_eq!(doc["name"].as_str(), Some("loop-verifier"));
        assert!(doc["description"].as_str().unwrap().contains("verifier"));
        let content = doc["system_prompt"]["content"].as_str().unwrap();
        assert!(content.starts_with("# loop-verifier"));
        assert!(!content.contains("---\nname:"), "frontmatter stripped");
        let tricky = codex_verifier_toml(
            "---\ndescription: a \"quoted\" one\n---\nbody with \"\"\" inside\n",
        );
        let doc: toml::Value = toml::from_str(&tricky).unwrap();
        assert_eq!(doc["description"].as_str(), Some("a \"quoted\" one"));
        assert!(
            doc["system_prompt"]["content"]
                .as_str()
                .unwrap()
                .contains("\"\"\"")
        );
    }

    #[test]
    fn the_codex_toml_carries_an_explicit_model_only() {
        let doc: toml::Value = toml::from_str(&codex_agent_toml(
            "r",
            "---\nname: r\ndescription: d\nmodel: gpt-5.5\ntools: Read\n---\nbody\n",
        ))
        .unwrap();
        assert_eq!(doc["model"].as_str(), Some("gpt-5.5"));
        assert_eq!(doc["developer_instructions"].as_str(), Some("body\n"));
        assert_eq!(doc["system_prompt"]["content"].as_str(), Some("body\n"));
        assert!(doc.get("tools").is_none(), "no Codex key for a tool list");
        for alias in ["sonnet", "Opus", "inherit", ""] {
            let text = format!("---\nname: r\ndescription: d\nmodel: {alias}\n---\nbody\n");
            let doc: toml::Value = toml::from_str(&codex_agent_toml("r", &text)).unwrap();
            assert!(doc.get("model").is_none(), "{alias:?}");
        }
    }

    #[test]
    fn the_preamble_follows_the_frontmatter() {
        let body = "---\nname: a\ndescription: d\n---\n\n# a\n\nbody\n";
        assert_eq!(with_preamble(body, ""), body);
        assert_eq!(with_preamble(body, "  \n"), body);
        assert_eq!(
            with_preamble(body, "- rule one\n- rule two"),
            "---\nname: a\ndescription: d\n---\n\n## Baseline\n\n- rule one\n- rule two\n\n# a\n\nbody\n"
        );
        assert_eq!(
            with_preamble("no frontmatter\n", "- rule"),
            "## Baseline\n\n- rule\n\nno frontmatter\n"
        );
        // the Codex file carries it inside the instructions
        let toml_text = agent_file(Harness::Codex, "a", body, "- rule");
        let doc: toml::Value = toml::from_str(&toml_text).unwrap();
        assert!(
            doc["developer_instructions"]
                .as_str()
                .unwrap()
                .starts_with("## Baseline\n\n- rule\n\n# a")
        );
        assert_eq!(
            agent_file(Harness::Claude, "a", body, "- rule"),
            with_preamble(body, "- rule")
        );
        // the built-in preamble reaches an installed verifier
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let pattern = patterns::find("ci-sweeper").unwrap();
        let caps = Caps {
            max_runs_per_day: 1,
            max_tokens_per_day: 1000,
            verifier_model: String::new(),
        };
        scaffold_with_library(temp.path(), &ws, pattern, Harness::Claude, Level::L1, &caps)
            .unwrap();
        let verifier = std::fs::read_to_string(ws.join(".claude/agents/loop-verifier.md")).unwrap();
        assert!(
            verifier.contains("---\n\n## Baseline\n\n- Keep the role"),
            "{verifier}"
        );
        assert!(verifier.contains("# loop-verifier"));
    }

    #[test]
    fn scaffold_writes_once_and_reports_files() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path().join("my-proj");
        std::fs::create_dir_all(&ws).unwrap();
        let pattern = patterns::find("ci-sweeper").unwrap();
        let caps = Caps {
            max_runs_per_day: 96,
            max_tokens_per_day: 1_000_000,
            verifier_model: String::new(),
        };
        let report = scaffold(&ws, pattern, Harness::Claude, Level::L2, &caps).unwrap();
        assert!(report.skipped.is_empty());
        let rel: Vec<String> = report
            .written
            .iter()
            .map(|p| p.strip_prefix(&ws).unwrap().to_string_lossy().into_owned())
            .collect();
        for expected in [
            ".claude/skills/loop-ci-triage/SKILL.md",
            ".claude/skills/loop-fix/SKILL.md",
            ".claude/skills/loop-rules/SKILL.md",
            ".claude/agents/loop-verifier.md",
            "ci-sweeper-state.md",
            "LOOP.md",
            "loop-budget.md",
            "loop-run-log.md",
            "loop-constraints.md",
            "gate.yaml",
            "loop-ledger.json",
            "AGENTS.md",
        ] {
            assert!(rel.contains(&expected.to_string()), "{expected} in {rel:?}");
        }
        let loop_md = std::fs::read_to_string(ws.join("LOOP.md")).unwrap();
        assert!(loop_md.contains("# Loops — my-proj"));
        assert!(loop_md.contains("| ci-sweeper | 15m | L2 | ci-sweeper-state.md | claude |"));
        assert!(loop_md.contains("- infra-failures"));
        assert!(!loop_md.contains("{{"));
        let budget = std::fs::read_to_string(ws.join("loop-budget.md")).unwrap();
        assert!(budget.contains("| CI Sweeper | 96 | 1.0M | 0 (L1) / 2 (L2) |"));
        let ledger: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(ws.join("loop-ledger.json")).unwrap())
                .unwrap();
        assert_eq!(ledger["pattern"], "ci-sweeper");
        assert_eq!(ledger["level"], "L2");
        assert_eq!(ledger["attempts"].as_array().unwrap().len(), 0);
        let state = std::fs::read_to_string(ws.join("ci-sweeper-state.md")).unwrap();
        assert!(state.contains("# Loop State — my-proj"));

        // second call: everything skipped, nothing rewritten
        std::fs::write(ws.join("LOOP.md"), "mine").unwrap();
        let again = scaffold(&ws, pattern, Harness::Claude, Level::L2, &caps).unwrap();
        assert!(again.written.is_empty());
        assert_eq!(again.skipped.len(), report.written.len());
        assert_eq!(std::fs::read_to_string(ws.join("LOOP.md")).unwrap(), "mine");

        // codex lands its own roots and the TOML verifier; no ledger for a
        // pattern without a breaker
        let ws2 = temp.path().join("other");
        std::fs::create_dir_all(&ws2).unwrap();
        let daily = patterns::find("changelog-drafter").unwrap();
        let report = scaffold(&ws2, daily, Harness::Codex, Level::L1, &caps).unwrap();
        assert!(ws2.join(".codex/skills/loop-changelog/SKILL.md").is_file());
        assert!(ws2.join(".codex/agents/verifier.toml").is_file());
        assert!(!ws2.join("loop-ledger.json").exists());
        assert!(
            !report
                .written
                .iter()
                .any(|p| p.ends_with("loop-ledger.json"))
        );
        assert!(
            scaffold(&ws2, daily, Harness::Antigravity, Level::L1, &caps)
                .unwrap_err()
                .to_string()
                .contains("Antigravity")
        );
    }

    #[test]
    fn both_checkers_install_when_the_pattern_lists_them() {
        let temp = tempfile::tempdir().unwrap();
        let mut pattern = patterns::find("ci-sweeper").unwrap().clone();
        pattern.agents = vec!["loop-verifier".into(), "loop-reviewer".into()];
        let caps = Caps {
            max_runs_per_day: 1,
            max_tokens_per_day: 1000,
            verifier_model: String::new(),
        };
        assert_eq!(
            embedded_agents()
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>(),
            vec!["loop-verifier", "loop-reviewer"]
        );
        let reviewer = embedded_agent("loop-reviewer").unwrap();
        assert!(reviewer.starts_with("---\nname: loop-reviewer\n"));
        assert!(reviewer.contains("## Verdict: APPROVE | REJECT | ESCALATE_HUMAN"));
        assert!(reviewer.contains("tools: Read, Grep, Glob\n"));

        let ws = temp.path().join("claude");
        std::fs::create_dir_all(&ws).unwrap();
        let report = scaffold_with_library(
            temp.path(),
            &ws,
            &pattern,
            Harness::Claude,
            Level::L2,
            &caps,
        )
        .unwrap();
        assert!(
            report.missing_agents.is_empty(),
            "{:?}",
            report.missing_agents
        );
        let installed =
            std::fs::read_to_string(ws.join(".claude/agents/loop-reviewer.md")).unwrap();
        assert!(installed.contains("## Baseline"));
        assert!(installed.contains("# loop-reviewer"));
        assert!(ws.join(".claude/agents/loop-verifier.md").is_file());

        let ws2 = temp.path().join("codex");
        std::fs::create_dir_all(&ws2).unwrap();
        scaffold_with_library(
            temp.path(),
            &ws2,
            &pattern,
            Harness::Codex,
            Level::L2,
            &caps,
        )
        .unwrap();
        let toml_text =
            std::fs::read_to_string(ws2.join(".codex/agents/loop-reviewer.toml")).unwrap();
        let doc: toml::Value = toml::from_str(&toml_text).unwrap();
        assert_eq!(doc["name"].as_str(), Some("loop-reviewer"));
        assert!(
            doc["developer_instructions"]
                .as_str()
                .unwrap()
                .contains("## Verdict")
        );
        assert!(ws2.join(".codex/agents/verifier.toml").is_file());
    }

    #[test]
    fn contract_files_report_presence_and_staleness() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path();
        let pattern = patterns::find("daily-triage").unwrap();
        let files = contract_files(ws, pattern);
        assert_eq!(files.len(), 6, "no ledger for daily-triage");
        assert!(files.iter().all(|f| !f.present && !f.stale));
        std::fs::write(
            ws.join("STATE.md"),
            "# x\n\nLast run: 2020-01-01T00:00:00Z\n",
        )
        .unwrap();
        std::fs::write(ws.join("gate.yaml"), "version: 1\n").unwrap();
        let files = contract_files(ws, pattern);
        let state = files.iter().find(|f| f.name == "STATE.md").unwrap();
        assert!(state.present && state.stale);
        assert!(
            files
                .iter()
                .find(|f| f.name == "gate.yaml")
                .unwrap()
                .present
        );
        let fresh = format!(
            "Last run: {}\n",
            crate::loops::format_timestamp(crate::loops::now())
        );
        std::fs::write(ws.join("STATE.md"), fresh).unwrap();
        assert!(!contract_files(ws, pattern)[0].stale);
        std::fs::write(ws.join("STATE.md"), "Last run: (set by loop on each run)\n").unwrap();
        assert!(
            !contract_files(ws, pattern)[0].stale,
            "unparsed is not stale"
        );
        assert!(
            contract_files(ws, patterns::find("ci-sweeper").unwrap())
                .iter()
                .any(|f| f.name == "loop-ledger.json")
        );
    }
}
