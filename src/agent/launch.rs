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
    /// Files that must exist before the harness starts, such as the custom
    /// agent definition Antigravity reads from its config directory.
    /// Written by [`prepare_launch`]; building a launch never touches disk.
    pub files: Vec<(PathBuf, Vec<u8>)>,
    /// Commands (argv) run before the harness starts, such as registering
    /// the agent-mux MCP server through the harness's own CLI.
    pub setup_commands: Vec<Vec<String>>,
}

/// Errors occurring during launch resolution.
#[derive(Debug)]
pub enum LaunchError {
    UnsupportedHarness(Harness),
    MissingArtifacts(String),
    Config(String),
    /// A file the harness needs could not be written.
    Prepare(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::UnsupportedHarness(h) => {
                write!(f, "harness '{h}' is not supported by this agent")
            }
            LaunchError::MissingArtifacts(msg) => write!(f, "missing artifacts: {msg}"),
            LaunchError::Config(msg) => write!(f, "configuration error: {msg}"),
            LaunchError::Prepare(msg) => write!(f, "launch preparation failed: {msg}"),
        }
    }
}

/// Materializes `launch.files` and runs `launch.setup_commands`.
///
/// A file that cannot be written is fatal: the harness would start without
/// its instructions. A setup command that fails is reported as a warning so
/// the session still opens and the user can see what went wrong.
pub fn prepare_launch(launch: &AgentLaunch) -> Result<Vec<String>, LaunchError> {
    for (path, bytes) in &launch.files {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| LaunchError::Prepare(format!("create {}: {e}", parent.display())))?;
        }
        std::fs::write(path, bytes)
            .map_err(|e| LaunchError::Prepare(format!("write {}: {e}", path.display())))?;
    }
    let mut warnings = Vec::new();
    for argv in &launch.setup_commands {
        let Some((program, rest)) = argv.split_first() else {
            continue;
        };
        let shown = argv.join(" ");
        match std::process::Command::new(program)
            .args(rest)
            .current_dir(&launch.cwd)
            .stdin(std::process::Stdio::null())
            .output()
        {
            Ok(out) if out.status.success() => {}
            Ok(out) => warnings.push(format!(
                "setup command failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => warnings.push(format!("setup command could not run: {shown}: {e}")),
        }
    }
    Ok(warnings)
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

    // Per-harness block from the package frontmatter, if declared.
    let block = definition.launch.get(&harness);

    let model = options
        .model
        .clone()
        .or_else(|| block.and_then(|b| b.model.clone()))
        .or_else(|| profile.model.clone());
    let bypass = options
        .bypass_approvals
        .or(block.and_then(|b| b.bypass_approvals))
        .or(profile.bypass_approvals)
        .unwrap_or(false);
    let block_args: Vec<String> = block.map(|b| b.args.clone()).unwrap_or_default();
    let block_env: Vec<(String, String)> = block
        .map(|b| b.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    let startup_task = options
        .startup_task
        .clone()
        .or_else(|| definition.startup_task.clone());

    let mut p = profile.clone();
    p.name = format!("{} ({})", definition.name, harness.as_str());
    p.command = harness.as_str().to_string();

    let mut args = Vec::new();
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut setup_commands: Vec<Vec<String>> = Vec::new();
    match harness {
        Harness::Claude => {
            args.push("--append-system-prompt".to_string());
            args.push(definition.instructions.clone());
            if definition.mcp_servers.iter().any(|s| s == "agent-mux") {
                let executable = std::env::var("AGENT_MUX_BIN")
                    .map(PathBuf::from)
                    .or_else(|_| std::env::current_exe())
                    .unwrap_or_else(|_| PathBuf::from("agent-mux"));
                let db = crate::tracing::analysis::default_trace_db_path();
                let (cmd, mcp_args) =
                    crate::agent::artifacts::mcp_command(&executable, &db, workspace);
                let mcp_json = serde_json::json!({
                    "mcpServers": {
                        "agent-mux": {
                            "command": cmd.to_string_lossy().to_string(),
                            "args": mcp_args,
                        }
                    }
                });
                args.push("--mcp-config".to_string());
                args.push(mcp_json.to_string());
            }
            if let Some(ref m) = model {
                args.push("--model".to_string());
                args.push(m.clone());
            }
            if bypass {
                args.push("--dangerously-skip-permissions".to_string());
            }
            args.extend(block_args.iter().cloned());
            for extra in &options.extra_args {
                args.push(extra.clone());
            }
            if let Some(ref task) = startup_task {
                args.push(task.clone());
            }
        }
        Harness::Codex => {
            if !definition.instructions.trim().is_empty() {
                args.push("-c".to_string());
                let escaped_instructions = serde_json::to_string(&definition.instructions)
                    .unwrap_or_else(|_| format!("\"{}\"", definition.instructions));
                args.push(format!("developer_instructions={escaped_instructions}"));
            }
            if definition.mcp_servers.iter().any(|s| s == "agent-mux") {
                let executable = std::env::var("AGENT_MUX_BIN")
                    .map(PathBuf::from)
                    .or_else(|_| std::env::current_exe())
                    .unwrap_or_else(|_| PathBuf::from("agent-mux"));
                let db = crate::tracing::analysis::default_trace_db_path();
                let (_, mcp_args) =
                    crate::agent::artifacts::mcp_command(&executable, &db, workspace);
                let formatted_args = mcp_args
                    .iter()
                    .map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")))
                    .collect::<Vec<_>>()
                    .join(", ");
                args.push("-c".to_string());
                args.push(format!(
                    "mcp_servers.agent-mux.command=\"{}\"",
                    executable.to_string_lossy()
                ));
                args.push("-c".to_string());
                args.push(format!("mcp_servers.agent-mux.args=[{}]", formatted_args));
            }
            if let Some(ref m) = model {
                args.push("--model".to_string());
                args.push(m.clone());
            }
            if bypass {
                args.push("--yolo".to_string());
            }
            args.extend(block_args.iter().cloned());
            for extra in &options.extra_args {
                args.push(extra.clone());
            }
            if let Some(ref task) = startup_task {
                args.push(task.clone());
            }
        }
        Harness::Antigravity => {
            // Instructions travel as a custom main agent in agy's config
            // directory, selected by name. See adapters::agy::agent_markdown
            // for the format verified against the real CLI.
            args.push("--agent".to_string());
            args.push(definition.name.clone());
            files.push((
                crate::agent::adapters::agy::agy_agent_file(&definition.id),
                crate::agent::adapters::agy::agent_markdown(definition).into_bytes(),
            ));
            if definition.mcp_servers.iter().any(|s| s == "agent-mux") {
                let executable = std::env::var("AGENT_MUX_BIN")
                    .map(PathBuf::from)
                    .or_else(|_| std::env::current_exe())
                    .unwrap_or_else(|_| PathBuf::from("agent-mux"));
                let db = crate::tracing::analysis::default_trace_db_path();
                let (cmd, mcp_args) =
                    crate::agent::artifacts::mcp_command(&executable, &db, workspace);
                // agy has no per-launch MCP flag; its own CLI registers the
                // server in ~/.gemini/config/mcp_config.json (add-or-update).
                let mut register = vec![
                    "agy".to_string(),
                    "mcp".to_string(),
                    "add".to_string(),
                    "agent-mux".to_string(),
                    cmd.to_string_lossy().to_string(),
                    "--".to_string(),
                ];
                register.extend(mcp_args);
                setup_commands.push(register);
            }
            if let Some(ref m) = model {
                args.push("--model".to_string());
                args.push(m.clone());
            }
            if bypass {
                args.push("--dangerously-skip-permissions".to_string());
            }
            args.extend(block_args.iter().cloned());
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

    let mut env = block_env;
    env.extend(options.extra_env.iter().cloned());
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
        files,
        setup_commands,
    })
}
