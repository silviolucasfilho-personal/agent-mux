//! The factory brain: one short Markdown file per repository with the
//! durable facts earlier runs learned (a build quirk, a check that is not
//! obvious, a convention a station missed).
//!
//! agent-mux reads it when a run starts and hands it to every station in
//! its context, so it reaches all seven harnesses alike and no station
//! needs a memory tool. Only agent-mux writes it: a station can at most
//! suggest a note, and the operator decides. It lives in the library
//! (`<library>/factory/<key>/brain.md`), not in the repository, where
//! worktrees and branches would fork it and a station could edit it.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The default bound of a brain, in characters: it is loaded whole into
/// every station's context, so it must stay a curated page, not a log.
pub const MAX_CHARS: usize = 40_000;

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `host/owner/name` of a git remote URL, the same for its SSH and HTTPS
/// forms: `git@github.com:Owner/Repo.git` and
/// `https://github.com/owner/repo` both give `github.com/owner/repo`.
pub fn normalize_remote(url: &str) -> Option<String> {
    let mut u = url.trim();
    if u.is_empty() {
        return None;
    }
    for scheme in ["https://", "http://", "ssh://", "git://"] {
        if let Some(rest) = u.strip_prefix(scheme) {
            u = rest;
            break;
        }
    }
    // `user@host…`
    if let Some((user, rest)) = u.split_once('@')
        && !user.contains('/')
    {
        u = rest;
    }
    // scp form `host:owner/name`; a URL's `host:port/owner/name` keeps its path
    let (host, path) = match u.split_once(':') {
        Some((h, p)) if !h.contains('/') => {
            let p = match p.split_once('/') {
                Some((port, rest)) if port.chars().all(|c| c.is_ascii_digit()) => rest,
                _ => p,
            };
            (h, p)
        }
        _ => u.split_once('/')?,
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host, path).to_lowercase())
}

/// `owner/name` of the workspace's `origin` when it is on GitHub, else
/// empty: what `gh` calls the repository.
pub fn github_repo(workspace: &Path) -> String {
    git(workspace, &["remote", "get-url", "origin"])
        .and_then(|u| normalize_remote(&u))
        .and_then(|n| n.strip_prefix("github.com/").map(str::to_string))
        .unwrap_or_default()
}

/// What names a repository's brain: its normalized `origin`, or the
/// workspace's own path without a remote.
pub fn repo_name(workspace: &Path) -> String {
    git(workspace, &["remote", "get-url", "origin"])
        .and_then(|u| normalize_remote(&u))
        .unwrap_or_else(|| {
            let top = git(workspace, &["rev-parse", "--show-toplevel"])
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace.to_path_buf());
            format!("local{}", top.to_string_lossy())
        })
}

/// The folder name of a repository's brain: a hash, so the library holds
/// no path made of a remote's words.
pub fn key(repo_name: &str) -> String {
    let digest = Sha256::digest(repo_name.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// `<library>/factory/<key>/brain.md`.
pub fn path(library_root: &Path, workspace: &Path) -> PathBuf {
    library_root
        .join("factory")
        .join(key(&repo_name(workspace)))
        .join("brain.md")
}

/// The brain's text; empty when the repository has none yet.
pub fn read(library_root: &Path, workspace: &Path) -> String {
    std::fs::read_to_string(path(library_root, workspace)).unwrap_or_default()
}

/// Appends `note` under `date` (`2026-11-16`), creating the file with its
/// heading when it is new. Refuses a note that would take the brain past
/// `max_chars`: the page is to be edited down, not truncated.
pub fn append(
    library_root: &Path,
    workspace: &Path,
    note: &str,
    date: &str,
    max_chars: usize,
) -> Result<PathBuf, String> {
    let note = note.trim();
    if note.is_empty() {
        return Err("the note is empty".into());
    }
    let file = path(library_root, workspace);
    let name = repo_name(workspace);
    let mut text = std::fs::read_to_string(&file).unwrap_or_default();
    if text.trim().is_empty() {
        text = format!(
            "# Factory brain: {name}\n\nDurable facts about this repository that every factory run starts from. Keep it short and current: it is read whole by every station.\n\n"
        );
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    // one line per note; a note's own line breaks become spaces
    let line: String = note.split_whitespace().collect::<Vec<_>>().join(" ");
    text.push_str(&format!("- {date}: {line}\n"));
    let chars = text.chars().count();
    if chars > max_chars {
        return Err(format!(
            "the brain would be {chars} characters, past its bound of {max_chars}; edit {} down first",
            file.display()
        ));
    }
    let dir = file.parent().expect("the brain has a folder");
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // beside it, the repository's readable name: the folder is a hash
    let _ = std::fs::write(dir.join("repo"), format!("{name}\n"));
    std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_and_https_forms_of_a_remote_are_one_repository() {
        let forms = [
            "git@github.com:Owner/Repo.git",
            "https://github.com/owner/repo",
            "https://github.com/owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
            "https://token@github.com/owner/repo/",
            "ssh://git@github.com:22/owner/repo.git",
        ];
        for f in forms {
            assert_eq!(
                normalize_remote(f).as_deref(),
                Some("github.com/owner/repo"),
                "{f}"
            );
        }
        assert_eq!(key("github.com/owner/repo"), key("github.com/owner/repo"));
        assert_ne!(key("github.com/owner/repo"), key("github.com/owner/other"));
        assert_eq!(key("x").len(), 16);
        assert_eq!(normalize_remote(""), None);
        assert_eq!(
            normalize_remote("git@gitlab.example.com:team/sub/proj.git").as_deref(),
            Some("gitlab.example.com/team/sub/proj")
        );
    }

    #[test]
    fn a_note_is_appended_under_its_date_and_the_bound_refuses() {
        let lib = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        assert_eq!(read(lib.path(), ws.path()), "");
        let file = append(
            lib.path(),
            ws.path(),
            "tests need\n  --all-features",
            "2026-11-16",
            400,
        )
        .unwrap();
        let text = read(lib.path(), ws.path());
        assert!(text.starts_with("# Factory brain: local"));
        assert!(text.ends_with("- 2026-11-16: tests need --all-features\n"));
        assert!(file.parent().unwrap().join("repo").is_file());
        assert!(file.starts_with(lib.path().join("factory")));
        // past the bound: refused, and the file is as it was
        let long = "x".repeat(400);
        let err = append(lib.path(), ws.path(), &long, "2026-11-16", 400).unwrap_err();
        assert!(err.contains("past its bound"), "{err}");
        assert_eq!(read(lib.path(), ws.path()), text);
        assert!(append(lib.path(), ws.path(), "  ", "2026-11-16", 400).is_err());
    }
}
