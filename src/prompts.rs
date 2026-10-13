//! The prompts agent-mux composes itself: the opening prompt of a loop run,
//! the hydration hint of a skill launch, the workflow session prompts and
//! the baseline section of every installed loop agent. The built-in text is
//! `src/prompts.toml`; a `prompts.toml` in the configuration library
//! (`assets::root()`) replaces the keys it sets. Read at launch time, so an
//! edit takes effect on the next run without a restart.

use serde::Deserialize;
use std::path::Path;

/// The compiled-in `prompts.toml`.
pub const BUILTIN: &str = include_str!("prompts.toml");

/// File name inside the library.
pub const FILE: &str = "prompts.toml";

/// Placeholders `loop.run` accepts.
pub const LOOP_PLACEHOLDERS: &[&str] = &[
    "invocation",
    "pattern",
    "state_file",
    "workspace",
    "level",
    "harness",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompts {
    /// `[loop] run`.
    pub loop_run: String,
    /// `[skill] hydration_hint`.
    pub hydration_hint: String,
    /// `[workflow] run`: a workflow session running a step skill.
    pub workflow_run: String,
    /// `[workflow] inline`: the preamble of an inline prompt step.
    pub workflow_inline: String,
    /// `[workflow] plan`: the planner's opening prompt.
    pub workflow_plan: String,
    /// `[agent] preamble`: the baseline section the scaffolder inserts
    /// into every installed loop agent.
    pub agent_preamble: String,
}

#[derive(Debug, Default, Deserialize)]
struct Doc {
    #[serde(default, rename = "loop")]
    loop_: LoopDoc,
    #[serde(default)]
    skill: SkillDoc,
    #[serde(default)]
    workflow: WorkflowDoc,
    #[serde(default)]
    agent: AgentDoc,
}

#[derive(Debug, Default, Deserialize)]
struct LoopDoc {
    run: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SkillDoc {
    hydration_hint: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct WorkflowDoc {
    run: Option<String>,
    inline: Option<String>,
    plan: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AgentDoc {
    preamble: Option<String>,
}

/// The values filled into `loop.run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopVars<'a> {
    pub invocation: &'a str,
    pub pattern: &'a str,
    pub state_file: &'a str,
    pub workspace: &'a str,
    pub level: &'a str,
    pub harness: &'a str,
}

impl Prompts {
    /// The compiled-in prompts.
    pub fn builtin() -> Prompts {
        let doc: Doc = toml::from_str(BUILTIN).expect("src/prompts.toml is valid at build time");
        Prompts {
            loop_run: doc.loop_.run.unwrap_or_default(),
            hydration_hint: doc.skill.hydration_hint.unwrap_or_default(),
            workflow_run: doc.workflow.run.unwrap_or_default(),
            workflow_inline: doc.workflow.inline.unwrap_or_default(),
            workflow_plan: doc.workflow.plan.unwrap_or_default(),
            agent_preamble: doc.agent.preamble.unwrap_or_default(),
        }
    }

    /// The built-in prompts with the keys `text` sets applied.
    pub fn parse_over_builtin(text: &str) -> Result<Prompts, String> {
        let doc: Doc = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut p = Prompts::builtin();
        if let Some(run) = doc.loop_.run {
            p.loop_run = run;
        }
        if let Some(h) = doc.skill.hydration_hint {
            p.hydration_hint = h;
        }
        if let Some(r) = doc.workflow.run {
            p.workflow_run = r;
        }
        if let Some(i) = doc.workflow.inline {
            p.workflow_inline = i;
        }
        if let Some(pl) = doc.workflow.plan {
            p.workflow_plan = pl;
        }
        if let Some(a) = doc.agent.preamble {
            p.agent_preamble = a;
        }
        Ok(p)
    }

    /// The effective prompts for a library root: the built-in ones when the
    /// library has no `prompts.toml` or it does not parse (the Configuration
    /// view reports the parse error; a launch must never fail on it).
    pub fn load(root: &Path) -> Prompts {
        match std::fs::read_to_string(root.join(FILE)) {
            Ok(text) => Prompts::parse_over_builtin(&text).unwrap_or_else(|_| Prompts::builtin()),
            Err(_) => Prompts::builtin(),
        }
    }

    /// The effective prompts for the default library root.
    pub fn current() -> Prompts {
        Prompts::load(&crate::assets::root())
    }
}

/// Fills the placeholders of a `loop.run` template.
pub fn render_loop_run(template: &str, vars: &LoopVars<'_>) -> String {
    template
        .replace("{invocation}", vars.invocation)
        .replace("{pattern}", vars.pattern)
        .replace("{state_file}", vars.state_file)
        .replace("{workspace}", vars.workspace)
        .replace("{level}", vars.level)
        .replace("{harness}", vars.harness)
}

/// Every `{name}` placeholder in `text`.
pub fn placeholders(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                if !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    && !out.iter().any(|n| n == name)
                {
                    out.push(name.to_string());
                }
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    out
}

/// Problems with a `prompts.toml` text: parse errors, unknown placeholders,
/// a loop prompt that never invokes the skill.
pub fn validate(text: &str) -> Vec<String> {
    let doc: Doc = match toml::from_str(text) {
        Ok(d) => d,
        Err(e) => return vec![format!("prompts.toml: {e}")],
    };
    let mut problems = Vec::new();
    if let Some(run) = &doc.loop_.run {
        problems.extend(validate_loop_run(run));
    }
    if let Some(h) = &doc.skill.hydration_hint
        && h.trim().is_empty()
    {
        problems.push("skill.hydration_hint is empty".into());
    }
    for (key, text, needs_invocation) in [
        ("workflow.run", &doc.workflow.run, true),
        ("workflow.inline", &doc.workflow.inline, false),
        ("workflow.plan", &doc.workflow.plan, true),
    ] {
        if let Some(t) = text {
            if t.trim().is_empty() {
                problems.push(format!("{key} is empty"));
            } else if needs_invocation && !t.contains("{invocation}") {
                problems.push(format!("{key} does not name {{invocation}}"));
            }
            for p in placeholders(t) {
                if !WORKFLOW_PLACEHOLDERS.contains(&p.as_str()) {
                    problems.push(format!("{key}: unknown placeholder {{{p}}}"));
                }
            }
        }
    }
    problems
}

/// Placeholders the `[workflow]` prompts accept.
pub const WORKFLOW_PLACEHOLDERS: &[&str] = &["invocation", "workflow", "step", "context", "plan"];

/// Fills a `[workflow]` prompt.
pub fn render_workflow(
    template: &str,
    invocation: &str,
    workflow: &str,
    step: &str,
    context: &str,
    plan: &str,
) -> String {
    template
        .replace("{invocation}", invocation)
        .replace("{workflow}", workflow)
        .replace("{step}", step)
        .replace("{context}", context)
        .replace("{plan}", plan)
}

/// Problems with one loop run template (also used for a pattern's `prompt`).
pub fn validate_loop_run(run: &str) -> Vec<String> {
    let mut problems = Vec::new();
    if run.trim().is_empty() {
        problems.push("loop.run is empty".into());
        return problems;
    }
    if !run.contains("{invocation}") {
        problems.push(
            "loop.run does not name {invocation}, so the triage skill is never invoked".into(),
        );
    }
    for p in placeholders(run) {
        if !LOOP_PLACEHOLDERS.contains(&p.as_str()) {
            problems.push(format!("loop.run: unknown placeholder {{{p}}}"));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_matches_the_former_constants() {
        let p = Prompts::builtin();
        assert!(
            p.loop_run
                .starts_with("{invocation} Run the {pattern} loop")
        );
        assert!(
            p.hydration_hint
                .starts_with("Read the briefing snapshot at $AGENT_MUX_BRIEFING")
        );
        assert!(validate(BUILTIN).is_empty());
    }

    #[test]
    fn library_keys_override_individually() {
        let p = Prompts::parse_over_builtin("[loop]\nrun = \"{invocation} go\"\n").unwrap();
        assert_eq!(p.loop_run, "{invocation} go");
        assert_eq!(p.hydration_hint, Prompts::builtin().hydration_hint);
        assert!(Prompts::parse_over_builtin("[loop\n").is_err());
    }

    #[test]
    fn load_falls_back_to_builtin() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Prompts::load(dir.path()), Prompts::builtin());
        std::fs::write(dir.path().join(FILE), "not toml [").unwrap();
        assert_eq!(Prompts::load(dir.path()), Prompts::builtin());
        std::fs::write(dir.path().join(FILE), "[skill]\nhydration_hint = \"hi\"\n").unwrap();
        assert_eq!(Prompts::load(dir.path()).hydration_hint, "hi");
    }

    #[test]
    fn render_and_placeholders() {
        let vars = LoopVars {
            invocation: "/loop-triage",
            pattern: "daily-triage",
            state_file: "/ws/STATE.md",
            workspace: "/ws",
            level: "L1",
            harness: "claude",
        };
        let out = render_loop_run(
            "{invocation} {pattern} {state_file} {workspace} {level} {harness}",
            &vars,
        );
        assert_eq!(out, "/loop-triage daily-triage /ws/STATE.md /ws L1 claude");
        assert_eq!(placeholders("a {x} b {y_z} {x} {NO} {}"), vec!["x", "y_z"]);
    }

    #[test]
    fn validation_reports_missing_invocation_and_unknown_placeholders() {
        let problems = validate("[loop]\nrun = \"run {pattern} with {thing}\"\n");
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("{invocation}"));
        assert!(problems[1].contains("{thing}"));
        assert_eq!(validate("[loop]\nrun = 5\n").len(), 1);
        assert!(validate_loop_run("  ")[0].contains("empty"));
    }
}
