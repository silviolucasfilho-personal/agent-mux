//! Dynamic discovery and migration of agent packages and legacy definition files.

use crate::agent::definition::{AgentDefinition, parse_definition, parse_legacy_definition};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Error returned when migrating a legacy agent file to a canonical package.
#[derive(Debug)]
pub enum MigrationError {
    SourceNotFound(PathBuf),
    DestinationExists(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationError::SourceNotFound(p) => {
                write!(f, "source file not found: {}", p.display())
            }
            MigrationError::DestinationExists(p) => {
                write!(f, "destination already exists: {}", p.display())
            }
            MigrationError::Io(err) => write!(f, "I/O error: {err}"),
        }
    }
}

impl std::error::Error for MigrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MigrationError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for MigrationError {
    fn from(err: std::io::Error) -> Self {
        MigrationError::Io(err)
    }
}

/// Result of an agent discovery run across workspace, global, and bundled roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryReport {
    pub agents: Vec<AgentDefinition>,
    pub diagnostics: Vec<String>,
}

/// Copies a legacy agent markdown file to a canonical `AGENTS.md` package path.
///
/// Preserves the original legacy source file and refuses to overwrite an existing
/// destination.
pub fn migrate_legacy(source: &Path, destination: &Path) -> Result<(), MigrationError> {
    if destination.exists() {
        return Err(MigrationError::DestinationExists(destination.to_path_buf()));
    }
    if !source.exists() {
        return Err(MigrationError::SourceNotFound(source.to_path_buf()));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)?;
    Ok(())
}

/// Locates the bundled agents root directory if present.
///
/// Precedence:
/// 1. `AGENT_MUX_BUNDLED_AGENTS_DIR` env var
/// 2. `AGENT_MUX_SHARE_DIR/agents` env var
/// 3. Executable-relative share dir (`../share/agent-mux/agents`)
/// 4. System share locations (`/usr/local/share/agent-mux/agents`, `/usr/share/agent-mux/agents`)
/// 5. Development repo paths (`CARGO_MANIFEST_DIR/agents`, `./agents`)
pub fn bundled_agents_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("AGENT_MUX_BUNDLED_AGENTS_DIR") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Ok(share) = std::env::var("AGENT_MUX_SHARE_DIR") {
        let p = PathBuf::from(share).join("agents");
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(prefix) = exe.parent().and_then(|bin| bin.parent()) {
            let share = prefix.join("share").join("agent-mux").join("agents");
            if share.is_dir() {
                return Some(share);
            }
            if let Some(repo_dir) = prefix.parent() {
                let repo_agents = repo_dir.join("agents");
                if repo_agents.is_dir() {
                    return Some(repo_agents);
                }
            }
        }
    }
    for sys_share in [
        "/usr/local/share/agent-mux/agents",
        "/usr/share/agent-mux/agents",
    ] {
        let p = PathBuf::from(sys_share);
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let p = PathBuf::from(manifest_dir).join("agents");
        if p.is_dir() {
            return Some(p);
        }
    }
    let dev = PathBuf::from("agents");
    if dev.is_dir() {
        return Some(dev);
    }
    None
}

/// Scans canonical packages and legacy files across workspace, global, and bundled roots.
///
/// Precedence:
/// `workspace canonical > global canonical > bundled canonical > workspace legacy > global legacy`
///
/// Canonical package sources take precedence over legacy definitions of the same ID.
/// This function is strictly read-only and never mutates the filesystem.
pub fn discover_agents(workspace: &Path, global: &Path, bundled: Option<&Path>) -> DiscoveryReport {
    let mut diagnostics = Vec::new();
    let mut agents = Vec::new();
    let mut seen_ids = HashSet::new();

    // 1. Workspace canonical packages
    let ws_canonical = scan_canonical_packages(workspace, &mut diagnostics);
    for agent in ws_canonical {
        if seen_ids.insert(agent.id.clone()) {
            agents.push(agent);
        }
    }

    // 2. Global canonical packages
    let global_canonical = scan_canonical_packages(global, &mut diagnostics);
    for agent in global_canonical {
        if seen_ids.insert(agent.id.clone()) {
            agents.push(agent);
        }
    }

    // 3. Bundled canonical packages
    if let Some(bundled_root) = bundled {
        if bundled_root.is_dir() {
            let bundled_canonical = scan_canonical_packages(bundled_root, &mut diagnostics);
            for mut agent in bundled_canonical {
                if seen_ids.insert(agent.id.clone()) {
                    agent.is_builtin = true;
                    agents.push(agent);
                }
            }
        } else {
            diagnostics.push(format!(
                "Bundled agents directory not found or not a directory: {}",
                bundled_root.display()
            ));
        }
    }

    // 4. Workspace legacy files
    let ws_legacy = scan_legacy_files(workspace, &mut diagnostics);
    for agent in ws_legacy {
        if seen_ids.insert(agent.id.clone()) {
            agents.push(agent);
        }
    }

    // 5. Global legacy files
    let global_legacy = scan_legacy_files(global, &mut diagnostics);
    for agent in global_legacy {
        if seen_ids.insert(agent.id.clone()) {
            agents.push(agent);
        }
    }

    DiscoveryReport {
        agents,
        diagnostics,
    }
}

/// Scans direct subdirectories under `root` for canonical packages (`<id>/AGENTS.md`).
/// Skips any directory named `generated`.
/// Rejects duplicate IDs within the same root with deterministic diagnostics.
fn scan_canonical_packages(root: &Path, diagnostics: &mut Vec<String>) -> Vec<AgentDefinition> {
    let mut packages = Vec::new();
    let mut seen_ids_in_root: HashMap<String, PathBuf> = HashMap::new();

    if !root.is_dir() {
        return packages;
    }

    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(err) => {
            diagnostics.push(format!(
                "Failed to read directory '{}': {err}",
                root.display()
            ));
            return packages;
        }
    };

    let mut entry_paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entry_paths.sort();

    for dir_path in entry_paths {
        if !dir_path.is_dir() {
            continue;
        }

        // Never scan `generated` directories as packages
        if dir_path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == "generated")
        {
            continue;
        }

        // Canonical package definition file
        let candidate_file = ["AGENTS.md", "ANGENTS.md", "agents.md"]
            .iter()
            .map(|name| dir_path.join(name))
            .find(|p| p.is_file());

        let agents_md_path = match candidate_file {
            Some(p) => p,
            None => continue,
        };

        let content = match fs::read_to_string(&agents_md_path) {
            Ok(c) => c,
            Err(err) => {
                diagnostics.push(format!(
                    "Failed to read canonical agent file '{}': {err}",
                    agents_md_path.display()
                ));
                continue;
            }
        };

        match parse_definition(&content, &agents_md_path) {
            Ok(agent) => {
                if let Some(earlier_path) = seen_ids_in_root.get(&agent.id) {
                    diagnostics.push(format!(
                        "Duplicate canonical agent ID '{}' in '{}' at '{}' rejected; already defined at '{}'",
                        agent.id,
                        root.display(),
                        agents_md_path.display(),
                        earlier_path.display()
                    ));
                } else {
                    seen_ids_in_root.insert(agent.id.clone(), agents_md_path);
                    packages.push(agent);
                }
            }
            Err(err) => {
                diagnostics.push(format!("{err}"));
            }
        }
    }

    packages
}

/// Scans direct files under `root` for legacy `.md` agent files.
/// Skips `AGENTS.md` and `ANGENTS.md` so repository-root instructions are never
/// parsed as agent packages.
fn scan_legacy_files(root: &Path, diagnostics: &mut Vec<String>) -> Vec<AgentDefinition> {
    let mut files = Vec::new();
    let mut seen_ids_in_root: HashMap<String, PathBuf> = HashMap::new();

    if !root.is_dir() {
        return files;
    }

    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(err) => {
            diagnostics.push(format!(
                "Failed to read directory '{}': {err}",
                root.display()
            ));
            return files;
        }
    };

    let mut entry_paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entry_paths.sort();

    for file_path in entry_paths {
        if !file_path.is_file() {
            continue;
        }

        let is_md = file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));

        if !is_md {
            continue;
        }

        // Skip root AGENTS.md / ANGENTS.md repository guidance files
        if let Some(file_name) = file_path.file_name().and_then(|n| n.to_str()) {
            if file_name.eq_ignore_ascii_case("agents.md")
                || file_name.eq_ignore_ascii_case("angents.md")
            {
                continue;
            }
        }

        let content = match fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(err) => {
                diagnostics.push(format!(
                    "Failed to read legacy agent file '{}': {err}",
                    file_path.display()
                ));
                continue;
            }
        };

        match parse_legacy_definition(&content, Some(&file_path)) {
            Ok(agent) => {
                if let Some(earlier_path) = seen_ids_in_root.get(&agent.id) {
                    diagnostics.push(format!(
                        "Duplicate legacy agent ID '{}' in '{}' at '{}' rejected; already defined at '{}'",
                        agent.id,
                        root.display(),
                        file_path.display(),
                        earlier_path.display()
                    ));
                } else {
                    seen_ids_in_root.insert(agent.id.clone(), file_path);
                    files.push(agent);
                }
            }
            Err(err) => {
                diagnostics.push(format!("{err}"));
            }
        }
    }

    files
}
