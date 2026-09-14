//! Generic agent launch builder and options resolution.

use crate::agent::artifacts::ArtifactSet;
use crate::agent::definition::AgentDefinition;
use crate::config::Profile;
use crate::harness::Harness;
use std::path::{Path, PathBuf};

/// Options provided at agent launch time to override defaults.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    pub harness_override: Option<Harness>,
    pub model: Option<String>,
    pub bypass_approvals: Option<bool>,
    pub cwd: Option<PathBuf>,
    pub startup_task: Option<String>,
    pub extra_args: Vec<String>,
    pub extra_env: Vec<(String, String)>,
}

/// A resolved, ready-to-spawn agent launch configuration.
#[derive(Debug, Clone)]
pub struct AgentLaunch {
    pub profile: Profile,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub agent_id: String,
    pub source_hash: String,
    pub diagnostics: Vec<String>,
}

/// Errors occurring during launch resolution.
#[derive(Debug)]
pub enum LaunchError {
    UnsupportedHarness(Harness),
    MissingArtifacts(String),
    Config(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::UnsupportedHarness(h) => {
                write!(f, "harness '{h}' is not supported by this agent")
            }
            LaunchError::MissingArtifacts(msg) => write!(f, "missing artifacts: {msg}"),
            LaunchError::Config(msg) => write!(f, "configuration error: {msg}"),
        }
    }
}

impl std::error::Error for LaunchError {}

/// Returns the index of `definition.default_harness` within `definition.harnesses`.
/// Defaults to 0 if not found.
pub fn selected_harness_index(definition: &AgentDefinition) -> usize {
    definition
        .harnesses
        .iter()
        .position(|h| *h == definition.default_harness)
        .unwrap_or(0)
}

/// Builds a resolved `AgentLaunch` for the chosen harness and profile.
///
/// Precedence:
/// `explicit launch override > supported agent field > profile > native default`
pub fn build_agent_launch(
    definition: &AgentDefinition,
    profile: &Profile,
    options: &LaunchOptions,
    workspace: &Path,
    _artifacts: &ArtifactSet,
) -> Result<AgentLaunch, LaunchError> {
    let harness = options
        .harness_override
        .or_else(|| Harness::detect(&profile.command))
        .unwrap_or(definition.default_harness);

    if !definition.harnesses.contains(&harness) {
        return Err(LaunchError::UnsupportedHarness(harness));
    }

    let cwd = options
        .cwd
        .clone()
        .or_else(|| profile.default_dir.as_ref().map(PathBuf::from))
        .unwrap_or_else(|| workspace.to_path_buf());

    let model = options.model.clone().or_else(|| profile.model.clone());
    let bypass = options
        .bypass_approvals
        .or(profile.bypass_approvals)
        .unwrap_or(false);
    let startup_task = options
        .startup_task
        .clone()
        .or_else(|| definition.startup_task.clone());

    let mut p = profile.clone();
    p.name = format!("{} ({})", definition.name, harness.as_str());
    p.command = harness.as_str().to_string();

    let mut args = Vec::new();
    match harness {
        Harness::Claude => {
            args.push("--append-system-prompt".to_string());
            args.push(definition.instructions.clone());
            if let Some(ref m) = model {
                args.push("--model".to_string());
                args.push(m.clone());
            }
            if bypass {
                args.push("--dangerously-skip-permissions".to_string());
            }
            for extra in &options.extra_args {
                args.push(extra.clone());
            }
            if let Some(ref task) = startup_task {
                args.push(task.clone());
            }
        }
        Harness::Codex => {
            if let Some(ref m) = model {
                args.push("--model".to_string());
                args.push(m.clone());
            }
            if bypass {
                args.push("--yolo".to_string());
            }
            for extra in &options.extra_args {
                args.push(extra.clone());
            }
            if let Some(ref task) = startup_task {
                args.push(task.clone());
            }
        }
        Harness::Antigravity => {
            if let Some(ref m) = model {
                args.push("--model".to_string());
                args.push(m.clone());
            }
            if bypass {
                args.push("--dangerously-skip-permissions".to_string());
            }
            for extra in &options.extra_args {
                args.push(extra.clone());
            }
            if let Some(ref task) = startup_task {
                args.push("--prompt-interactive".to_string());
                args.push(task.clone());
            }
        }
    }
    p.args = args;

    let mut env = options.extra_env.clone();
    env.push(("AGENT_MUX_AGENT_ID".to_string(), definition.id.clone()));
    env.push((
        "AGENT_MUX_SOURCE_HASH".to_string(),
        definition.source_hash.clone(),
    ));

    Ok(AgentLaunch {
        profile: p,
        cwd,
        env,
        agent_id: definition.id.clone(),
        source_hash: definition.source_hash.clone(),
        diagnostics: Vec::new(),
    })
}
