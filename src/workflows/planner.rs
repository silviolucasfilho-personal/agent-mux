//! The dynamic-workflow planner: the facts handed to the `workflow-author`
//! skill (`$AGENT_MUX_WORKFLOW_PLAN`) and the extraction of the document
//! it answers with. Launching the planner session is `App`'s job
//! (`crate::app::workflows`).

use super::document::{SkillInfo, parse, validate};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const FENCE: &str = "workflow-toml";
/// A new agent the planner defines for its document (`src/agents`).
pub const AGENT_FENCE: &str = "agent-toml";

/// A quick inventory of a workspace, computed in Rust so the planner
/// spends its context on the task, not on `ls`.
pub fn inventory(workspace: &Path) -> Value {
    let mut entries: Vec<String> = std::fs::read_dir(workspace)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        return None;
                    }
                    let suffix = if e.path().is_dir() { "/" } else { "" };
                    Some(format!("{name}{suffix}"))
                })
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    entries.truncate(60);
    let has = |f: &str| workspace.join(f).is_file();
    let (kind, test, lint) = if has("Cargo.toml") {
        (
            "rust",
            Some("cargo test"),
            Some("cargo clippy --all-targets"),
        )
    } else if has("package.json") {
        ("node", Some("npm test"), Some("npm run lint"))
    } else if has("pyproject.toml") || has("setup.py") {
        ("python", Some("pytest -q"), Some("ruff check ."))
    } else if has("go.mod") {
        ("go", Some("go test ./..."), Some("go vet ./..."))
    } else {
        ("unknown", None, None)
    };
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(workspace)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let dirty = git(&["status", "--porcelain"]).map(|s| s.lines().count());
    let recent: Vec<String> = git(&["log", "--oneline", "-10"])
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default();
    serde_json::json!({
        "path": workspace,
        "entries": entries,
        "kind": kind,
        "test_command": test,
        "lint_command": lint,
        "git": { "branch": branch, "dirty_files": dirty, "recent_commits": recent },
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub writes: bool,
}

/// An agent a step may run as.
#[derive(Debug, Clone, Serialize)]
pub struct AgentSummary {
    pub name: String,
    pub description: String,
    /// Canonical tools, or `all` for the harness's own.
    pub tools: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowSummary {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub document: String,
}

/// What the planner reads first.
#[derive(Debug, Clone, Serialize)]
pub struct PlanContext {
    pub schema_version: u32,
    pub task: String,
    pub harness: String,
    pub workspace: Value,
    pub skills: Vec<SkillSummary>,
    pub agents: Vec<AgentSummary>,
    pub workflows: Vec<WorkflowSummary>,
    pub budget_tokens: Option<u64>,
    pub rules: Vec<String>,
}

pub fn plan_context(
    task: &str,
    harness: &str,
    workspace: &Path,
    skills: &[crate::skill::SkillDefinition],
    workflows: &[super::library::Entry],
    agents: &crate::agents::Catalog,
    budget_tokens: Option<u64>,
) -> PlanContext {
    PlanContext {
        schema_version: 2,
        task: task.to_string(),
        harness: harness.to_string(),
        workspace: inventory(workspace),
        skills: skills
            .iter()
            .filter(|s| s.id.starts_with("wf-"))
            .map(|s| SkillSummary {
                name: s.id.clone(),
                description: s.description.clone(),
                writes: s.writes,
            })
            .collect(),
        agents: agents
            .entries
            .iter()
            .filter_map(|e| {
                e.spec.as_ref().map(|s| AgentSummary {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    tools: match &s.tools {
                        None => "all".into(),
                        Some(_) => s.tools_label(),
                    },
                    source: e.source.label().into(),
                })
            })
            .collect(),
        workflows: workflows
            .iter()
            .filter(|e| e.valid())
            .map(|e| WorkflowSummary {
                name: e.name.clone(),
                description: e
                    .doc
                    .as_ref()
                    .map(|d| d.description.clone())
                    .unwrap_or_default(),
                when_to_use: e.doc.as_ref().and_then(|d| d.when_to_use.clone()),
                document: e.text.clone(),
            })
            .collect(),
        budget_tokens,
        rules: vec![
            "Use only the step skills listed here; anything else is an inline prompt step.".into(),
            "A step whose skill has writes = true needs isolation = \"worktree\".".into(),
            format!("Answer with exactly one fenced {FENCE} block and nothing after it."),
            "Regular coding tasks do not need a panel of 5 reviewers.".into(),
            "A step, a verify block or a judge may run as an agent listed here: agent = \"<name>\". Refuters and judges do not inherit the step's agent.".into(),
            format!("When a role needs a persona no listed agent has, define it in a fenced {AGENT_FENCE} block before the {FENCE} block (name, description, instructions, tools from read, edit, shell, web, mcp:<server>); it is saved into the library when the plan runs or is saved. Every agent needs read; one on a step that edits needs edit."),
        ],
    }
}

/// `<runtime>/workflows/plans/<id>.json`.
pub fn write_context(runtime_dir: &Path, id: &str, ctx: &PlanContext) -> std::io::Result<PathBuf> {
    let dir = runtime_dir.join("workflows").join("plans");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{id}.json"));
    let json = serde_json::to_string_pretty(ctx)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// The last fenced `workflow-toml` block of the planner's answer.
pub fn extract_document(text: &str) -> Result<String, String> {
    let open = format!("```{FENCE}");
    let Some(start) = text.rfind(&open) else {
        return Err(format!(
            "the planner did not answer with a fenced {FENCE} block"
        ));
    };
    let body = &text[start + open.len()..];
    let body = body.trim_start_matches(['\r', '\n']);
    let Some(end) = body.find("```") else {
        return Err(format!("the {FENCE} block is not closed"));
    };
    let doc = body[..end].trim().to_string();
    if doc.is_empty() {
        return Err(format!("the {FENCE} block is empty"));
    }
    Ok(doc + "\n")
}

/// Every fenced `agent-toml` block of the planner's answer, in order.
pub fn extract_agents(text: &str) -> Vec<String> {
    let open = format!("```{AGENT_FENCE}");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let body = rest[start + open.len()..].trim_start_matches(['\r', '\n']);
        let Some(end) = body.find("```") else {
            break;
        };
        let agent = body[..end].trim();
        if !agent.is_empty() {
            out.push(format!("{agent}\n"));
        }
        rest = &body[end + 3..];
    }
    out
}

/// A planned document with its validation.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub document: String,
    pub name: String,
    pub problems: Vec<String>,
}

/// Checks the agents a planner defined, and the document's fit with
/// them and the existing ones: a new agent must load and must not
/// replace an existing one with other content.
pub fn check_agents(
    document: &str,
    new_agents: &[String],
    catalog: &crate::agents::Catalog,
    skills: &[SkillInfo],
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut all = catalog.clone();
    for text in new_agents {
        match crate::agents::AgentSpec::parse(text) {
            Err(p) => problems.extend(p.into_iter().map(|x| format!("{AGENT_FENCE}: {x}"))),
            Ok(spec) => {
                if let Some(existing) = catalog.get(&spec.name)
                    && existing.hash != spec.hash
                {
                    problems.push(format!(
                        "{AGENT_FENCE}: agent {:?} already exists with other content; use it, or give the new one another name",
                        spec.name
                    ));
                    continue;
                }
                all.entries.retain(|e| e.name != spec.name);
                all.entries.push(crate::agents::Entry {
                    name: spec.name.clone(),
                    source: crate::agents::Source::Library(PathBuf::from(format!(
                        "(planned) {}.toml",
                        spec.name
                    ))),
                    spec: Some(spec),
                    problems: Vec::new(),
                });
            }
        }
    }
    if let Ok(doc) = parse(document) {
        problems.extend(crate::agents::fit::errors(&crate::agents::fit::check(
            &doc, &all, skills,
        )));
    }
    problems
}

/// Writes the planner's new agents into the library (`<root>/agents`).
/// One that is already there with the same content is left alone.
pub fn save_agents(root: &Path, new_agents: &[String]) -> Result<Vec<PathBuf>, String> {
    let dir = crate::agents::library_dir(root);
    let mut written = Vec::new();
    for text in new_agents {
        let spec = crate::agents::AgentSpec::parse(text).map_err(|p| p.join("; "))?;
        let path = dir.join(format!("{}.toml", spec.name));
        match std::fs::read_to_string(&path) {
            Ok(existing) if existing == *text => continue,
            Ok(_) => {
                return Err(format!(
                    "{} already exists with other content",
                    path.display()
                ));
            }
            Err(_) => {}
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

pub fn check(document: &str, skills: &[SkillInfo]) -> Planned {
    match parse(document) {
        Ok(doc) => Planned {
            name: doc.name.clone(),
            problems: validate(&doc, Some(skills)),
            document: document.to_string(),
        },
        Err(problems) => Planned {
            document: document.to_string(),
            name: "dynamic".into(),
            problems,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_reads_the_workspace_shape() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        let inv = inventory(dir.path());
        assert_eq!(inv["kind"], "rust");
        assert_eq!(inv["test_command"], "cargo test");
        assert_eq!(inv["entries"], serde_json::json!(["Cargo.toml", "src/"]));
    }

    #[test]
    fn extracts_and_checks_the_planned_document() {
        let text = "Here you go:\n```workflow-toml\n[workflow]\nname = \"quick\"\ndescription = \"d\"\n[[steps]]\nid = \"s\"\nprompt = \"hi\"\n```\n";
        let doc = extract_document(text).unwrap();
        let p = check(&doc, &[]);
        assert_eq!(p.name, "quick");
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        assert!(extract_document("no block").is_err());
        assert!(
            extract_document("```workflow-toml\n[workflow]")
                .unwrap_err()
                .contains("not closed")
        );
        let bad = check("[workflow\n", &[]);
        assert_eq!(bad.name, "dynamic");
        assert!(!bad.problems.is_empty());
    }

    #[test]
    fn planned_agents_are_extracted_checked_and_saved() {
        let text = "Two agents:\n```agent-toml\nname = \"sec\"\ndescription = \"Security\"\ninstructions = \"Look for injections.\"\ntools = [\"read\", \"shell\"]\n```\n```agent-toml\nname = \"bad\"\n```\n```workflow-toml\n[workflow]\nname = \"w\"\ndescription = \"d\"\n[[steps]]\nid = \"s\"\nprompt = \"p\"\nagent = \"sec\"\n```\n";
        let agents = extract_agents(text);
        assert_eq!(agents.len(), 2);
        let doc = extract_document(text).unwrap();
        let empty = crate::agents::Catalog::default();
        let p = check_agents(&doc, &agents, &empty, &[]);
        assert!(p.iter().all(|x| x.starts_with("agent-toml:")), "{p:?}");
        assert!(!p.is_empty());
        // the good one alone fits; an unknown agent does not
        assert!(check_agents(&doc, &agents[..1], &empty, &[]).is_empty());
        assert!(check_agents(&doc, &[], &empty, &[])[0].contains("not found"));
        // saved once, then left alone; other content is refused
        let root = tempfile::tempdir().unwrap();
        assert_eq!(save_agents(root.path(), &agents[..1]).unwrap().len(), 1);
        assert!(save_agents(root.path(), &agents[..1]).unwrap().is_empty());
        let other = agents[0].replace("Security", "Other");
        assert!(save_agents(root.path(), std::slice::from_ref(&other)).is_err());
        let lib = crate::agents::Catalog::load(root.path(), None);
        assert!(check_agents(&doc, &[other], &lib, &[])[0].contains("already exists"));
    }
}
