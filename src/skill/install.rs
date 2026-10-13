//! Installing a package into each harness's skill directory.
//!
//! Roots, as the trace inventory verified against the installed CLIs:
//! Claude Code `~/.claude/skills/`, Codex `~/.codex/skills/`, Antigravity
//! `~/.gemini/config/skills/`. agent-mux marks the directories it writes
//! with a manifest and never overwrites a directory it did not write.

use super::SkillDefinition;
use super::render::render_skill_md;
use crate::harness::Harness;
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = ".agent-mux.json";

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The directory a harness scans for user skills.
pub fn skills_root(harness: Harness, home: &Path) -> PathBuf {
    match harness {
        Harness::Claude => home.join(".claude").join("skills"),
        Harness::Codex => home.join(".codex").join("skills"),
        Harness::Antigravity => home.join(".gemini").join("config").join("skills"),
    }
}

/// The directory a harness scans for project-level skills under a
/// workspace. Antigravity reads skills at user level only (probed with
/// agy 1.2.3), so it has none.
pub fn project_skills_root(harness: Harness, workspace: &Path) -> Option<PathBuf> {
    match harness {
        Harness::Claude => Some(workspace.join(".claude").join("skills")),
        Harness::Codex => Some(workspace.join(".codex").join("skills")),
        Harness::Antigravity => None,
    }
}

#[derive(Debug)]
pub enum InstallError {
    /// The directory exists and was not written by agent-mux.
    Foreign(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Foreign(p) => write!(
                f,
                "{} exists and was not installed by agent-mux; remove it or pass --force",
                p.display()
            ),
            InstallError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<std::io::Error> for InstallError {
    fn from(e: std::io::Error) -> Self {
        InstallError::Io(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub dir: PathBuf,
    pub files: Vec<PathBuf>,
    /// True when the installed copy already matched this source hash.
    pub unchanged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallStatus {
    pub harness: Harness,
    pub dir: PathBuf,
    pub installed: bool,
    /// Written by agent-mux (manifest present).
    pub managed: bool,
    /// Manifest hash equals the package's current hash.
    pub current: bool,
}

fn manifest_hash(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join(MANIFEST)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("source_hash")?.as_str().map(str::to_string)
}

pub fn status(def: &SkillDefinition, harness: Harness, home: &Path) -> InstallStatus {
    let dir = skills_root(harness, home).join(&def.id);
    let installed = dir.join("SKILL.md").is_file();
    let hash = manifest_hash(&dir);
    InstallStatus {
        harness,
        dir,
        installed,
        managed: hash.is_some(),
        current: hash.as_deref() == Some(def.source_hash.as_str()),
    }
}

/// Writes SKILL.md, the reference files and the manifest. Refuses a
/// directory without a manifest unless `force`.
pub fn install(
    def: &SkillDefinition,
    harness: Harness,
    home: &Path,
    force: bool,
) -> Result<InstallReport, InstallError> {
    install_into(
        def,
        harness,
        &skills_root(harness, home).join(&def.id),
        force,
    )
}

/// Writes the package under the workspace's project-level skill
/// directory, the way `install` writes it under the home. Refuses a
/// directory without a manifest unless `force`; Antigravity is refused.
pub fn install_project(
    def: &SkillDefinition,
    harness: Harness,
    workspace: &Path,
    force: bool,
) -> Result<InstallReport, InstallError> {
    let Some(root) = project_skills_root(harness, workspace) else {
        return Err(InstallError::Io(std::io::Error::other(
            "Antigravity reads skills at user level only; no project install",
        )));
    };
    install_into(def, harness, &root.join(&def.id), force)
}

fn install_into(
    def: &SkillDefinition,
    harness: Harness,
    dir: &Path,
    force: bool,
) -> Result<InstallReport, InstallError> {
    let dir = dir.to_path_buf();
    let existing = manifest_hash(&dir);
    if dir.exists() && existing.is_none() && !force {
        return Err(InstallError::Foreign(dir));
    }
    if existing.as_deref() == Some(def.source_hash.as_str()) {
        return Ok(InstallReport {
            dir,
            files: Vec::new(),
            unchanged: true,
        });
    }
    std::fs::create_dir_all(&dir)?;
    let mut files = Vec::new();
    let skill_md = dir.join("SKILL.md");
    std::fs::write(&skill_md, render_skill_md(def, harness))?;
    files.push(skill_md);
    for (rel, content) in &def.files {
        let target = dir.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, content)?;
        files.push(target);
    }
    let manifest = serde_json::json!({
        "installed_by": "agent-mux",
        "id": def.id,
        "harness": harness.as_str(),
        "source_hash": def.source_hash,
        "files": files.iter().map(|p| p.strip_prefix(&dir).unwrap_or(p).to_string_lossy().into_owned()).collect::<Vec<_>>(),
    });
    let manifest_path = dir.join(MANIFEST);
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    files.push(manifest_path);
    Ok(InstallReport {
        dir,
        files,
        unchanged: false,
    })
}

/// Removes an installed copy. Only directories carrying the manifest are
/// removed; returns `Ok(false)` when there was nothing managed to remove.
pub fn uninstall(id: &str, harness: Harness, home: &Path) -> Result<bool, InstallError> {
    let dir = skills_root(harness, home).join(id);
    if !dir.is_dir() {
        return Ok(false);
    }
    if manifest_hash(&dir).is_none() {
        return Err(InstallError::Foreign(dir));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(true)
}

impl From<serde_json::Error> for InstallError {
    fn from(e: serde_json::Error) -> Self {
        InstallError::Io(std::io::Error::other(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::parse_skill;

    fn def() -> SkillDefinition {
        parse_skill(
            "---\nname: loop-rules\ndescription: the rules of a run\n---\n\n# body\n",
            None,
            Vec::new(),
            None,
        )
        .unwrap()
    }

    #[test]
    fn project_install_lands_under_the_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path();
        let d = def();
        let report = install_project(&d, Harness::Claude, ws, false).unwrap();
        assert_eq!(report.dir, ws.join(".claude/skills/loop-rules"));
        assert!(ws.join(".claude/skills/loop-rules/SKILL.md").is_file());
        assert!(
            ws.join(".claude/skills/loop-rules")
                .join(MANIFEST)
                .is_file()
        );
        assert!(
            install_project(&d, Harness::Claude, ws, false)
                .unwrap()
                .unchanged
        );
        let codex = install_project(&d, Harness::Codex, ws, false).unwrap();
        assert_eq!(codex.dir, ws.join(".codex/skills/loop-rules"));
        assert!(
            std::fs::read_to_string(codex.dir.join("SKILL.md"))
                .unwrap()
                .contains("Invoke with $loop-rules.")
        );
        assert!(install_project(&d, Harness::Antigravity, ws, false).is_err());
        assert!(project_skills_root(Harness::Antigravity, ws).is_none());
        // a foreign directory is refused without force
        let foreign = ws.join(".claude/skills/other");
        std::fs::create_dir_all(&foreign).unwrap();
        let mut other = def();
        other.id = "other".into();
        assert!(matches!(
            install_project(&other, Harness::Claude, ws, false),
            Err(InstallError::Foreign(_))
        ));
        assert!(install_project(&other, Harness::Claude, ws, true).is_ok());
    }
}
