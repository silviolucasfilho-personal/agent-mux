//! Where a workspace's memory lives, and the files inside it.

use super::{Location, Settings};
use std::path::{Path, PathBuf};

pub const INDEX: &str = "MEMORY.md";
pub const NOTES: &str = "notes";
pub const BRIEFS: &str = "briefs";
pub const IMPORTS: &str = "imports";
pub const LOG: &str = "log.md";
/// Under `notes/`: what `memory rm` and the Configuration view remove.
pub const REMOVED: &str = ".removed";
/// Under `notes/`: notes past their `expires_at`.
pub const EXPIRED: &str = ".expired";
/// The folder of facts about the user that hold in every workspace.
pub const USER: &str = "_user";

/// `<library>/memory/`.
pub fn library_root() -> PathBuf {
    crate::assets::root().join("memory")
}

/// The user-level memory folder.
pub fn user_dir() -> PathBuf {
    library_root().join(USER)
}

/// A workspace's memory folder under the settings.
pub fn dir_for(settings: &Settings, workspace: &Path) -> PathBuf {
    match settings.location {
        Location::Library => library_root().join(crate::history::project_slug(workspace)),
        Location::Workspace => workspace.join(".agent-mux").join("memory"),
    }
}

pub fn index_path(dir: &Path) -> PathBuf {
    dir.join(INDEX)
}

pub fn notes_dir(dir: &Path) -> PathBuf {
    dir.join(NOTES)
}

pub fn note_path(dir: &Path, name: &str) -> PathBuf {
    notes_dir(dir).join(format!("{name}.md"))
}

/// Creates the folder's layout. Never rewrites anything that exists.
pub fn ensure(dir: &Path) -> std::io::Result<()> {
    for sub in [NOTES, BRIEFS, IMPORTS] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    Ok(())
}

/// Writes `text` to `path` through a temporary file and a rename, so a
/// reader never sees half a file.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_location_lives_beside_the_workspace_agents() {
        let ws = Path::new("/tmp/some-ws");
        let s = Settings {
            location: Location::Workspace,
            ..Settings::default()
        };
        assert_eq!(dir_for(&s, ws), ws.join(".agent-mux/memory"));
        let lib = dir_for(&Settings::default(), ws);
        assert!(lib.starts_with(library_root()));
    }

    #[test]
    fn ensure_creates_the_layout_and_keeps_files() {
        let d = tempfile::tempdir().unwrap();
        ensure(d.path()).unwrap();
        std::fs::write(d.path().join("notes/a.md"), "x").unwrap();
        ensure(d.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join("notes/a.md")).unwrap(),
            "x"
        );
        assert!(d.path().join("briefs").is_dir() && d.path().join("imports").is_dir());
    }
}
