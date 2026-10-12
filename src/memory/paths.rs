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

/// `<library>/memory/`, for a configuration library root
/// (`assets::root()`, `~/.agent-mux`).
pub fn memory_root(library: &Path) -> PathBuf {
    library.join("memory")
}

/// The user-level memory folder.
pub fn user_dir(library: &Path) -> PathBuf {
    memory_root(library).join(USER)
}

/// A workspace's memory folder under the settings. The library folder is
/// named by `history::project_slug`, which since 2026-10-11 turns every
/// character but a letter or digit into `-`, as Claude Code does; a
/// folder made earlier under the old name (only `/`, `\` and `:`
/// turned) keeps being used, so its notes are not left behind.
pub fn dir_for(library: &Path, settings: &Settings, workspace: &Path) -> PathBuf {
    match settings.location {
        Location::Library => {
            let root = memory_root(library);
            let dir = root.join(crate::history::project_slug(workspace));
            let legacy = root.join(legacy_slug(workspace));
            if !dir.exists() && legacy != dir && legacy.is_dir() {
                return legacy;
            }
            dir
        }
        Location::Workspace => workspace.join(".agent-mux").join("memory"),
    }
}

/// The slug memory folders were named by before 2026-10-11.
fn legacy_slug(workspace: &Path) -> String {
    let new = crate::history::project_slug(workspace);
    // same canonical path, only `/`, `\` and `:` turned into `-`
    let abs = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut slug: String = abs
        .to_string_lossy()
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    if !slug.starts_with('-') && !slug.is_empty() {
        slug.insert(0, '-');
    }
    if slug.is_empty() { new } else { slug }
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
        let root = Path::new("/lib");
        assert_eq!(dir_for(root, &s, ws), ws.join(".agent-mux/memory"));
        let lib = dir_for(root, &Settings::default(), ws);
        assert!(lib.starts_with("/lib/memory/"), "{}", lib.display());
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

    #[test]
    fn a_memory_made_under_the_old_folder_name_is_kept() {
        let lib = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let ws = ws.path().join("my.app");
        std::fs::create_dir_all(&ws).unwrap();
        let s = Settings::default();
        let new = dir_for(lib.path(), &s, &ws);
        assert!(
            new.to_string_lossy().ends_with("-my-app"),
            "{}",
            new.display()
        );
        // a memory from before the rename: still found
        let legacy = memory_root(lib.path()).join(legacy_slug(&ws));
        assert!(legacy.to_string_lossy().ends_with("-my.app"));
        std::fs::create_dir_all(&legacy).unwrap();
        assert_eq!(dir_for(lib.path(), &s, &ws), legacy);
    }
}
