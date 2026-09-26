//! Whether each agent a workflow names fits the steps that use it: it
//! exists and parses, it can `read` (every session reads its context
//! file), it can `edit` where the step writes (a `writes = true` skill or
//! worktree isolation), and what its tool list becomes on every harness
//! the step may run on. Errors stop `workflow check` and a run; warnings
//! are printed and noted.

use super::launch::{Level, tool_notes};
use super::{Catalog, Tool};
use crate::harness::Harness;
use crate::workflows::document::{Actor, HarnessFilter, Isolation, SkillInfo, Workflow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub level: Level,
    pub message: String,
}

impl Diagnostic {
    fn error(message: String) -> Diagnostic {
        Diagnostic {
            level: Level::Error,
            message,
        }
    }
}

/// The harnesses a session may run on: the one it names, else every one
/// the document allows.
fn candidates(named: Option<&str>, filter: &HarnessFilter) -> Vec<Harness> {
    match named.and_then(Harness::detect) {
        Some(h) => vec![h],
        None => [Harness::Claude, Harness::Codex, Harness::Antigravity]
            .into_iter()
            .filter(|h| filter.allows(h.as_str()))
            .collect(),
    }
}

pub fn check(doc: &Workflow, catalog: &Catalog, skills: &[SkillInfo]) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = Vec::new();
    for s in &doc.steps {
        let mut roles: Vec<(&str, &str, Option<&str>, Option<&Actor>)> = Vec::new();
        if let Some(a) = &s.agent {
            roles.push(("agent", a, s.harness.as_deref(), s.actor.as_ref()));
        }
        if let Some(v) = &s.verify
            && let Some(a) = &v.runner.agent
        {
            roles.push((
                "verify.agent",
                a,
                v.runner.harness.as_deref().or(s.harness.as_deref()),
                Some(&v.actor),
            ));
        }
        if let Some(a) = &s.judge_runner.agent {
            roles.push((
                "judge.agent",
                a,
                s.judge_runner.harness.as_deref().or(s.harness.as_deref()),
                s.judge.as_ref(),
            ));
        }
        for (what, name, harness, actor) in roles {
            let ctx = format!("step {}: {what} {name:?}", s.id);
            let Some(entry) = catalog.entry(name) else {
                out.push(Diagnostic::error(format!(
                    "{ctx} not found; create it with `agent-mux agent new {name}`"
                )));
                continue;
            };
            let Some(spec) = &entry.spec else {
                out.push(Diagnostic::error(format!(
                    "{ctx} does not load ({}): {}",
                    entry.source.path().display(),
                    entry.problems.join("; ")
                )));
                continue;
            };
            if !spec.can(&Tool::Read) {
                out.push(Diagnostic::error(format!(
                    "{ctx} cannot read; add \"read\" to its tools, every session reads its context file"
                )));
            }
            if what == "agent" && !spec.can(&Tool::Edit) {
                let writes_skill = actor
                    .and_then(Actor::skill)
                    .and_then(|k| skills.iter().find(|i| i.name == k))
                    .is_some_and(|i| i.writes);
                let isolated = s.isolation.or(doc.default_isolation) == Some(Isolation::Worktree);
                if writes_skill || isolated {
                    out.push(Diagnostic::error(format!(
                        "{ctx} cannot edit, but the step {}; add \"edit\" to its tools or pick another agent",
                        if writes_skill {
                            "runs a skill that edits files"
                        } else {
                            "runs in a worktree to edit files"
                        }
                    )));
                }
            }
            for h in candidates(harness, &doc.harness) {
                for (level, note) in tool_notes(spec, h) {
                    let d = Diagnostic {
                        level,
                        message: format!("{ctx}: {note}"),
                    };
                    if !out.contains(&d) {
                        out.push(d);
                    }
                }
            }
        }
    }
    out
}

pub fn errors(diags: &[Diagnostic]) -> Vec<String> {
    diags
        .iter()
        .filter(|d| d.level == Level::Error)
        .map(|d| d.message.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentSpec, Entry, Source};

    fn catalog(agents: &[(&str, &str)]) -> Catalog {
        Catalog {
            entries: agents
                .iter()
                .map(|(name, tools)| Entry {
                    name: name.to_string(),
                    source: Source::Library(format!("/lib/{name}.toml").into()),
                    spec: Some(
                        AgentSpec::parse(&format!(
                            "name = \"{name}\"\ndescription = \"d\"\ninstructions = \"i\"\n{tools}"
                        ))
                        .unwrap(),
                    ),
                    problems: Vec::new(),
                })
                .collect(),
        }
    }

    const DOC: &str = r#"
[workflow]
name = "w"
description = "d"
harness = ["claude", "codex"]

[[steps]]
id = "find"
skill = "wf-find"
agent = "reviewer"
verify = { skill = "wf-refute", votes = 2, agent = "skeptic", harness = "claude" }

[[steps]]
id = "fix"
skill = "wf-transform"
agent = "reviewer"
isolation = "worktree"

[[steps]]
id = "report"
prompt = "Write it up."
agent = "ghost"
"#;

    #[test]
    fn unknown_unreadable_and_read_only_agents_are_errors() {
        let doc = crate::workflows::parse(DOC).unwrap();
        let skills = vec![
            SkillInfo {
                name: "wf-find".into(),
                writes: false,
            },
            SkillInfo {
                name: "wf-transform".into(),
                writes: true,
            },
        ];
        let c = catalog(&[
            ("reviewer", "tools = [\"read\", \"shell\", \"web\"]"),
            ("skeptic", "tools = [\"shell\"]"),
        ]);
        let d = check(&doc, &c, &skills);
        let errs = errors(&d);
        assert_eq!(errs.len(), 3, "{errs:?}");
        assert!(errs[0].contains("verify.agent \"skeptic\" cannot read"));
        assert!(errs[1].contains("step fix: agent \"reviewer\" cannot edit"));
        assert!(
            errs[2].contains("\"ghost\" not found; create it with `agent-mux agent new ghost`")
        );
        // codex notes for the reviewer, once per message; none for the
        // claude-only skeptic; agy is not allowed by the document
        let warns: Vec<&Diagnostic> = d.iter().filter(|x| x.level == Level::Warning).collect();
        assert!(warns.iter().any(|w| {
            w.message
                .contains("step find: agent \"reviewer\": codex: `web`")
        }));
        assert!(!warns.iter().any(|w| w.message.contains("agy")));
        assert!(!warns.iter().any(|w| w.message.contains("skeptic")));
    }

    #[test]
    fn an_editor_fits_a_writing_step() {
        let doc = crate::workflows::parse(DOC).unwrap();
        let c = catalog(&[("reviewer", ""), ("skeptic", ""), ("ghost", "")]);
        assert!(errors(&check(&doc, &c, &[])).is_empty());
    }
}
