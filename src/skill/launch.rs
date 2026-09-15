//! Turning a skill and a harness into a spawnable profile: the harness's
//! own command line, the skill invocation as the opening prompt, and the
//! environment the skill's instructions rely on.

use super::SkillDefinition;
use super::render::invocation;
use crate::config::Profile;
use crate::harness::Harness;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SkillLaunch {
    pub profile: Profile,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

#[derive(Debug)]
pub enum LaunchError {
    UnsupportedHarness(Harness),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::UnsupportedHarness(h) => {
                write!(f, "harness '{h}' is not supported by this skill")
            }
        }
    }
}

impl std::error::Error for LaunchError {}

/// The first message of the session: the harness's invocation syntax for
/// the skill, followed by the package's startup prompt.
pub fn opening_prompt(def: &SkillDefinition, harness: Harness) -> String {
    match &def.startup_prompt {
        Some(p) => format!("{} {}", invocation(&def.id, harness), p.trim()),
        None => invocation(&def.id, harness),
    }
}

/// Session name shown in the sidebar and recorded on the trace launch.
pub fn session_name(def: &SkillDefinition, harness: Harness) -> String {
    format!("{} ({})", def.name, harness.as_str())
}

/// Composes the argv for `harness`, verified against the installed CLIs:
/// Claude Code and Codex take the prompt as the last positional argument,
/// Antigravity through `--prompt-interactive`. Model and permission
/// settings come from the base profile.
pub fn build_skill_launch(
    def: &SkillDefinition,
    harness: Harness,
    base: &Profile,
    cwd: &Path,
) -> Result<SkillLaunch, LaunchError> {
    build_skill_launch_with_db(def, harness, base, cwd, None)
}

/// Like [`build_skill_launch`], but hands the skill the trace store at
/// `trace_db` (the one the running TUI writes, which honours a configured
/// `[tracing] db_path`). `None` falls back to the environment/home default.
pub fn build_skill_launch_with_db(
    def: &SkillDefinition,
    harness: Harness,
    base: &Profile,
    cwd: &Path,
    trace_db: Option<&Path>,
) -> Result<SkillLaunch, LaunchError> {
    if !def.harnesses.contains(&harness) {
        return Err(LaunchError::UnsupportedHarness(harness));
    }
    let prompt = opening_prompt(def, harness);
    let mut args: Vec<String> = Vec::new();
    if let Some(m) = base
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        args.push("--model".into());
        args.push(m.to_string());
    }
    let bypass = base.bypass_approvals.unwrap_or(false);
    match harness {
        Harness::Claude => {
            if bypass {
                args.push("--dangerously-skip-permissions".into());
            }
            args.push(prompt);
        }
        Harness::Codex => {
            if bypass {
                args.push("--yolo".into());
            }
            args.push(prompt);
        }
        Harness::Antigravity => {
            if bypass {
                args.push("--dangerously-skip-permissions".into());
            }
            args.push("--prompt-interactive".into());
            args.push(prompt);
        }
    }
    let mut profile = base.clone();
    profile.name = session_name(def, harness);
    profile.command = harness.as_str().to_string();
    profile.args = args;

    let executable = std::env::var("AGENT_MUX_BIN")
        .map(PathBuf::from)
        .or_else(|_| std::env::current_exe())
        .unwrap_or_else(|_| PathBuf::from("agent-mux"));
    let env = vec![
        ("AGENT_MUX_SKILL_ID".to_string(), def.id.clone()),
        (
            "AGENT_MUX_BIN".to_string(),
            executable.to_string_lossy().into_owned(),
        ),
        (
            "AGENT_MUX_TRACE_DB".to_string(),
            trace_db
                .map(Path::to_path_buf)
                .unwrap_or_else(crate::tracing::analysis::default_trace_db_path)
                .to_string_lossy()
                .into_owned(),
        ),
    ];
    Ok(SkillLaunch {
        profile,
        cwd: base
            .default_dir
            .as_ref()
            .map(PathBuf::from)
            .filter(|d| d.is_dir())
            .unwrap_or_else(|| cwd.to_path_buf()),
        env,
    })
}
