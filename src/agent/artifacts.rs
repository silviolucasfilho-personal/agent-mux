//! Deterministic multi-harness artifact generation and management.

use crate::agent::adapters::agy::AgyAdapter;
use crate::agent::adapters::claude::ClaudeAdapter;
use crate::agent::adapters::codex::CodexAdapter;
use crate::agent::adapters::HarnessAdapter;
use crate::agent::definition::AgentDefinition;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Error encountered during artifact rendering or writing.
#[derive(Debug)]
pub enum ArtifactError {
    Io(std::io::Error),
    Serialization(serde_json::Error),
    Conflict(String),
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArtifactError::Io(err) => write!(f, "I/O error: {err}"),
            ArtifactError::Serialization(err) => write!(f, "Serialization error: {err}"),
            ArtifactError::Conflict(msg) => write!(f, "Artifact conflict: {msg}"),
        }
    }
}

impl std::error::Error for ArtifactError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ArtifactError::Io(err) => Some(err),
            ArtifactError::Serialization(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ArtifactError {
    fn from(err: std::io::Error) -> Self {
        ArtifactError::Io(err)
    }
}

impl From<serde_json::Error> for ArtifactError {
    fn from(err: serde_json::Error) -> Self {
        ArtifactError::Serialization(err)
    }
}

/// A complete generated set of harness artifacts and their manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSet {
    pub files: BTreeMap<PathBuf, Vec<u8>>,
    pub source_hash: String,
}

fn compute_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Builds the MCP server executable and arguments vector.
pub fn mcp_command(executable: &Path, db: &Path, workspace: &Path) -> (PathBuf, Vec<String>) {
    (
        executable.to_path_buf(),
        vec![
            "mcp".to_string(),
            "serve".to_string(),
            "--stdio".to_string(),
            "--db".to_string(),
            db.to_string_lossy().to_string(),
            "--workspace".to_string(),
            workspace.to_string_lossy().to_string(),
        ],
    )
}

/// Rendering context for resolving paths and external tools in harness artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderContext {
    pub executable: PathBuf,
    pub db: PathBuf,
    pub workspace: PathBuf,
}

impl Default for RenderContext {
    fn default() -> Self {
        let executable = std::env::var("AGENT_MUX_BIN")
            .map(PathBuf::from)
            .or_else(|_| std::env::current_exe())
            .unwrap_or_else(|_| PathBuf::from("agent-mux"));
        let db = crate::tracing::analysis::default_trace_db_path();
        let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            executable,
            db,
            workspace,
        }
    }
}

/// Deterministically renders the complete artifact set for Claude, Codex, and AGY with context.
pub fn render_artifacts_with_context(
    definition: &AgentDefinition,
    source: &Path,
    ctx: &RenderContext,
) -> Result<ArtifactSet, ArtifactError> {
    let mut files = BTreeMap::new();

    let adapters: Vec<Box<dyn HarnessAdapter>> = vec![
        Box::new(ClaudeAdapter),
        Box::new(CodexAdapter),
        Box::new(AgyAdapter),
    ];

    let mut enabled_harnesses = Vec::new();
    let mut disabled_harnesses = Vec::new();
    let mut adapter_versions = BTreeMap::new();

    for adapter in &adapters {
        let h = adapter.harness();
        let is_enabled = definition.harnesses.contains(&h);
        if is_enabled {
            enabled_harnesses.push(h.as_str().to_string());
        } else {
            disabled_harnesses.push(h.as_str().to_string());
        }
        adapter_versions.insert(h.as_str().to_string(), adapter.version());

        let rendered = adapter.render(definition, is_enabled, ctx);
        for (rel_path, content) in rendered {
            files.insert(rel_path, content);
        }
    }

    // Compute hashes for all rendered files
    let mut file_hashes = BTreeMap::new();
    for (rel_path, content) in &files {
        let path_str = rel_path.to_string_lossy().to_string();
        file_hashes.insert(path_str, compute_sha256(content));
    }

    // Build deterministic manifest
    let manifest_val = serde_json::json!({
        "schema_version": 1,
        "generator_version": 1,
        "agent_id": definition.id,
        "source_path": source.to_string_lossy(),
        "source_hash": definition.source_hash,
        "enabled_harnesses": enabled_harnesses,
        "disabled_harnesses": disabled_harnesses,
        "adapter_versions": adapter_versions,
        "file_hashes": file_hashes,
    });

    let manifest_bytes = (serde_json::to_string_pretty(&manifest_val)? + "\n").into_bytes();
    files.insert(PathBuf::from("manifest.json"), manifest_bytes);

    Ok(ArtifactSet {
        files,
        source_hash: definition.source_hash.clone(),
    })
}

/// Deterministically renders the complete artifact set for Claude, Codex, and AGY.
pub fn render_artifacts(
    definition: &AgentDefinition,
    source: &Path,
) -> Result<ArtifactSet, ArtifactError> {
    let mut ctx = RenderContext::default();
    if let Some(parent) = source.parent() {
        if parent.is_absolute() {
            ctx.workspace = parent.to_path_buf();
        }
    }
    render_artifacts_with_context(definition, source, &ctx)
}

/// Atomically writes an ArtifactSet into the target `root` directory.
///
/// Refuses to overwrite owned files that were directly modified by the user.
/// Preserves any unowned files already in the root directory.
pub fn write_artifacts(root: &Path, set: &ArtifactSet) -> Result<(), ArtifactError> {
    if !root.exists() {
        fs::create_dir_all(root)?;
    }

    // Check for conflicts with existing manifest
    let existing_manifest_path = root.join("manifest.json");
    if existing_manifest_path.is_file() {
        if let Ok(manifest_content) = fs::read_to_string(&existing_manifest_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&manifest_content) {
                if let Some(hashes) = val.get("file_hashes").and_then(|h| h.as_object()) {
                    for (file_rel_str, old_hash_val) in hashes {
                        if let Some(old_hash) = old_hash_val.as_str() {
                            let disk_path = root.join(file_rel_str);
                            if disk_path.is_file() {
                                if let Ok(disk_bytes) = fs::read(&disk_path) {
                                    let current_disk_hash = compute_sha256(&disk_bytes);
                                    let new_hash = set
                                        .files
                                        .get(Path::new(file_rel_str))
                                        .map(|b| compute_sha256(b));

                                    // If disk modified and doesn't match new target
                                    if current_disk_hash != old_hash
                                        && Some(&current_disk_hash) != new_hash.as_ref()
                                    {
                                        return Err(ArtifactError::Conflict(format!(
                                            "Owned artifact '{}' was modified directly on disk",
                                            disk_path.display()
                                        )));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Stage writes in temporary directory
    let parent = root.parent().unwrap_or(root);
    let staging_dir = tempfile::tempdir_in(parent)?;

    for (rel_path, content) in &set.files {
        let staging_file = staging_dir.path().join(rel_path);
        if let Some(p) = staging_file.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(&staging_file, content)?;
    }

    // Copy staged files into root, preserving unowned files
    for (rel_path, _) in &set.files {
        let src = staging_dir.path().join(rel_path);
        let dst = root.join(rel_path);
        if let Some(p) = dst.parent() {
            fs::create_dir_all(p)?;
        }
        fs::copy(&src, &dst)?;
    }

    Ok(())
}
