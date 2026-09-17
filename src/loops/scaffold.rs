//! The pattern library on disk: the embedded skills, verifier and
//! templates, and the scaffolder that writes a workspace's contract files
//! without ever overwriting one. The scaffolder writes the *effective*
//! text: a file in the configuration library (`crate::assets`) replaces
//! the embedded one of the same name.

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub max_runs_per_day: u32,
    pub max_tokens_per_day: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScaffoldReport {
    pub written: Vec<PathBuf>,
    pub skipped: Vec<PathBuf>,
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

/// Wraps a Claude-shaped agent file (frontmatter + body) into Codex's
/// agent TOML under `name`.
pub fn codex_agent_toml(name: &str, body: &str) -> String {
    let (description, text) = split_agent_frontmatter(body);
    let escaped = text.replace("\"\"\"", "\\\"\"\"");
    format!(
        "name = \"{}\"\ndescription = \"{}\"\n\n[system_prompt]\ncontent = \"\"\"\n{}\n\"\"\"\n",
        name.replace('\\', "\\\\").replace('"', "\\\""),
        description.replace('\\', "\\\\").replace('"', "\\\""),
        escaped.trim_end()
    )
}

/// (description, body) of a Claude agent file; the description falls back
/// to a fixed sentence when the frontmatter has none.
fn split_agent_frontmatter(text: &str) -> (String, String) {
    let mut description = "Independent verifier for loop-produced changes.".to_string();
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (description, text.to_string());
    }
    let mut body_start = 0;
    let mut offset = text.lines().next().map(|l| l.len() + 1).unwrap_or(0);
    for line in lines {
        let len = line.len() + 1;
        if line.trim() == "---" {
            body_start = offset + len;
            break;
        }
        if let Some(d) = line.strip_prefix("description:") {
            description = d.trim().to_string();
        }
        offset += len;
    }
    let body = text
        .get(body_start..)
        .unwrap_or("")
        .trim_start_matches('\n');
    (description, body.to_string())
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
    // 2. agents: the verifier when the pattern uses one, and every agent
    // the library adds
    for (name, body) in assets::loop_agents(library) {
        if name == "loop-verifier" && !pattern.verifier {
            continue;
        }
        let Some(path) = agent_path(harness, workspace, &name) else {
            continue;
        };
        let content = match harness {
            Harness::Codex => codex_agent_toml(&name, &body),
            _ => body,
        };
        report.put(path, &content)?;
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
        assert_eq!(embedded_skills().len(), 9);
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
    fn scaffold_writes_once_and_reports_files() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path().join("my-proj");
        std::fs::create_dir_all(&ws).unwrap();
        let pattern = patterns::find("ci-sweeper").unwrap();
        let caps = Caps {
            max_runs_per_day: 96,
            max_tokens_per_day: 1_000_000,
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
