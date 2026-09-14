//! Agent package validation (doctor) and adapter artifact installation/uninstallation.

use crate::agent::artifacts::ArtifactSet;
use crate::agent::definition::AgentDefinition;
use crate::harness::Harness;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorStatus {
    Healthy,
    MissingArtifacts,
    StaleArtifacts,
    Damaged,
}

#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub status: DoctorStatus,
    pub issues: Vec<String>,
}

#[derive(Debug)]
pub enum DoctorError {
    Io(std::io::Error),
}

impl std::fmt::Display for DoctorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DoctorError::Io(err) => write!(f, "I/O error: {err}"),
        }
    }
}

impl std::error::Error for DoctorError {}

impl From<std::io::Error> for DoctorError {
    fn from(err: std::io::Error) -> Self {
        DoctorError::Io(err)
    }
}

#[derive(Debug, Clone, Default)]
pub struct InstallReport {
    pub installed_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct UninstallReport {
    pub removed_files: Vec<PathBuf>,
}

#[derive(Debug)]
pub enum InstallError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Io(err) => write!(f, "I/O error: {err}"),
            InstallError::Json(err) => write!(f, "JSON error: {err}"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<std::io::Error> for InstallError {
    fn from(err: std::io::Error) -> Self {
        InstallError::Io(err)
    }
}

impl From<serde_json::Error> for InstallError {
    fn from(err: serde_json::Error) -> Self {
        InstallError::Json(err)
    }
}

/// Diagnoses the integrity of an agent's package and generated artifacts.
pub fn doctor_agent(
    definition: &AgentDefinition,
    root: &Path,
) -> Result<DoctorReport, DoctorError> {
    let mut issues = Vec::new();
    let gen_dir = root.join("generated");

    if !gen_dir.is_dir() {
        issues.push("Generated artifacts directory does not exist".to_string());
        return Ok(DoctorReport {
            status: DoctorStatus::MissingArtifacts,
            issues,
        });
    }

    let manifest_path = gen_dir.join("manifest.json");
    if !manifest_path.is_file() {
        issues.push("manifest.json is missing from generated directory".to_string());
        return Ok(DoctorReport {
            status: DoctorStatus::MissingArtifacts,
            issues,
        });
    }

    let manifest_bytes = fs::read(&manifest_path)?;
    let manifest_val: serde_json::Value = match serde_json::from_slice(&manifest_bytes) {
        Ok(v) => v,
        Err(e) => {
            issues.push(format!("Failed to parse manifest.json: {e}"));
            return Ok(DoctorReport {
                status: DoctorStatus::Damaged,
                issues,
            });
        }
    };

    let manifest_source_hash = manifest_val
        .get("source_hash")
        .and_then(|h| h.as_str())
        .unwrap_or_default();

    if manifest_source_hash != definition.source_hash {
        issues.push(format!(
            "Artifacts are stale: manifest hash '{}' != source hash '{}'",
            manifest_source_hash, definition.source_hash
        ));
        return Ok(DoctorReport {
            status: DoctorStatus::StaleArtifacts,
            issues,
        });
    }

    // Verify presence of all expected files
    if let Some(file_hashes) = manifest_val.get("file_hashes").and_then(|h| h.as_object()) {
        for (rel_path_str, _) in file_hashes {
            let p = gen_dir.join(rel_path_str);
            if !p.is_file() {
                issues.push(format!(
                    "Expected artifact '{}' is missing on disk",
                    p.display()
                ));
            }
        }
    }

    if !issues.is_empty() {
        return Ok(DoctorReport {
            status: DoctorStatus::MissingArtifacts,
            issues,
        });
    }

    // Verify MCP service health if agent-mux is a declared MCP server
    if definition.mcp_servers.iter().any(|s| s == "agent-mux") {
        let db_path = crate::tracing::analysis::default_trace_db_path();
        let config = crate::tracing::analysis::ServiceConfig::new(
            db_path,
            crate::tracing::analysis::Scope::all_workspaces(),
        );
        match crate::tracing::analysis::TraceService::new(config) {
            Ok(service) => {
                if let Err(e) = service.execute(crate::tracing::analysis::Request::Health(
                    crate::tracing::analysis::HealthArgs {},
                )) {
                    issues.push(format!("MCP health check failed: {e}"));
                    return Ok(DoctorReport {
                        status: DoctorStatus::Damaged,
                        issues,
                    });
                }
            }
            Err(e) => {
                issues.push(format!("Failed to initialize trace service: {e}"));
                return Ok(DoctorReport {
                    status: DoctorStatus::Damaged,
                    issues,
                });
            }
        }
    }

    Ok(DoctorReport {
        status: DoctorStatus::Healthy,
        issues,
    })
}

/// Installs generated harness artifacts into the specified directory.
pub fn install_agent(
    _definition: &AgentDefinition,
    artifacts: &ArtifactSet,
    target_dir: &Path,
    filter_harness: Option<Harness>,
) -> Result<InstallReport, InstallError> {
    let mut installed_files = Vec::new();
    fs::create_dir_all(target_dir)?;

    for (rel_path, content) in &artifacts.files {
        // If filter_harness is provided, only install files for that harness prefix
        if let Some(h) = filter_harness {
            let h_str = h.as_str();
            let matches = rel_path.starts_with(h_str) || rel_path == Path::new("manifest.json");
            if !matches {
                continue;
            }
        }

        let dest = target_dir.join(rel_path);
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(&dest, content)?;
        installed_files.push(dest);
    }

    Ok(InstallReport { installed_files })
}

/// Removes previously installed harness artifacts for this agent from the target directory.
pub fn uninstall_agent(
    _definition: &AgentDefinition,
    target_dir: &Path,
    filter_harness: Option<Harness>,
) -> Result<UninstallReport, InstallError> {
    let mut removed_files = Vec::new();

    if !target_dir.is_dir() {
        return Ok(UninstallReport::default());
    }

    let manifest_path = target_dir.join("manifest.json");
    if manifest_path.is_file() {
        if let Ok(manifest_bytes) = fs::read(&manifest_path) {
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&manifest_bytes) {
                if let Some(hashes) = val.get("file_hashes").and_then(|h| h.as_object()) {
                    for (rel_path_str, _) in hashes {
                        let rel_path = PathBuf::from(rel_path_str);
                        if let Some(h) = filter_harness {
                            let h_str = h.as_str();
                            if !rel_path.starts_with(h_str)
                                && rel_path != Path::new("manifest.json")
                            {
                                continue;
                            }
                        }
                        let dest = target_dir.join(&rel_path);
                        if dest.is_file() {
                            let _ = fs::remove_file(&dest);
                            removed_files.push(dest);
                        }
                    }
                }
            }
        }
        let _ = fs::remove_file(&manifest_path);
        if !removed_files.contains(&manifest_path) {
            removed_files.push(manifest_path);
        }
    }

    Ok(UninstallReport { removed_files })
}
