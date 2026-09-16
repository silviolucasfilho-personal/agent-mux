//! Worktrees for L2+ runs: `git worktree add -b loop/<run_id>
//! <workspace>/<worktrees_dir>/<run_id> <base>`, a manifest in the
//! conventional shape (`{ "version": 1, "worktrees": [...] }`) written
//! atomically under a `.manifest.mutex` lock file, and the cleanup rules
//! of the spec (section 8.3).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub id: String,
    pub path: String,
    pub branch: String,
    #[serde(rename = "baseBranch")]
    pub base_branch: String,
    pub pattern: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// `active | rejected | escalated | merged | stale`
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    #[serde(default)]
    pub worktrees: Vec<ManifestEntry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Manifest {
            version: 1,
            worktrees: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    pub base: String,
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

pub fn is_git_repo(workspace: &Path) -> bool {
    git(workspace, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|s| s == "true")
}

/// The branch `HEAD` is on; `main` when detached or unknown.
pub fn base_branch(workspace: &Path) -> String {
    match git(workspace, &["symbolic-ref", "--short", "-q", "HEAD"]) {
        Ok(b) if !b.is_empty() => b,
        _ => "main".to_string(),
    }
}

pub fn manifest_path(worktrees_root: &Path) -> PathBuf {
    worktrees_root.join("manifest.json")
}

pub fn load_manifest(worktrees_root: &Path) -> Manifest {
    std::fs::read_to_string(manifest_path(worktrees_root))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Runs `f` on the manifest under the `.manifest.mutex` lock and writes
/// it back atomically. A lock older than 30 s is taken over.
pub fn update_manifest(
    worktrees_root: &Path,
    f: impl FnOnce(&mut Manifest),
) -> std::io::Result<()> {
    std::fs::create_dir_all(worktrees_root)?;
    let lock = worktrees_root.join(".manifest.mutex");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = std::fs::metadata(&lock)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|m| m.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(30));
                if stale {
                    let _ = std::fs::remove_file(&lock);
                    continue;
                }
                if std::time::Instant::now() > deadline {
                    return Err(std::io::Error::other("manifest lock held for 10 s"));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(e),
        }
    }
    let result = (|| {
        let mut m = load_manifest(worktrees_root);
        f(&mut m);
        let json = serde_json::to_string_pretty(&m)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let path = manifest_path(worktrees_root);
        let temp = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&temp, json)?;
        std::fs::rename(&temp, &path)
    })();
    let _ = std::fs::remove_file(&lock);
    result
}

/// Creates `<workspace>/<worktrees_dir>/<run_id>` on `loop/<run_id>` from
/// `base` and records it as `active`. The run id's `:` become `-` in the
/// directory and branch names.
pub fn create(
    workspace: &Path,
    worktrees_dir: &str,
    run_id: &str,
    pattern: &str,
    now: &str,
) -> Result<Worktree, String> {
    let root = workspace.join(worktrees_dir);
    std::fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    let safe = run_id.replace(':', "-");
    let path = root.join(&safe);
    let branch = format!("loop/{safe}");
    let base = base_branch(workspace);
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    git(
        workspace,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            &path.to_string_lossy(),
            &base,
        ],
    )?;
    let entry = ManifestEntry {
        id: run_id.to_string(),
        path: path.to_string_lossy().into_owned(),
        branch: branch.clone(),
        base_branch: base.clone(),
        pattern: pattern.to_string(),
        created_at: now.to_string(),
        status: "active".into(),
    };
    if let Err(e) = update_manifest(&root, |m| {
        m.worktrees.retain(|w| w.id != run_id);
        m.worktrees.push(entry);
    }) {
        let _ = git(
            workspace,
            &["worktree", "remove", "--force", &path.to_string_lossy()],
        );
        let _ = git(workspace, &["branch", "-D", &branch]);
        return Err(format!("manifest: {e}"));
    }
    Ok(Worktree { path, branch, base })
}

/// The loop assets a run needs inside its worktree. They are usually
/// untracked in the workspace (the scaffolder wrote them, nobody
/// committed them yet), so a fresh worktree lacks them; `seed_loop_files`
/// copies them in and the change detectors ignore them.
pub const SEEDED_PREFIXES: [&str; 4] = [
    ".claude/skills/loop-",
    ".codex/skills/loop-",
    ".claude/agents/loop-verifier.md",
    ".codex/agents/verifier.toml",
];

fn is_seeded_path(rel: &str) -> bool {
    let rel = rel.trim_start_matches("./");
    SEEDED_PREFIXES.iter().any(|p| rel.starts_with(p))
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
    } else if !to.exists() {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to)?;
    }
    Ok(())
}

/// Copies the workspace's loop skills and verifier into the worktree when
/// the worktree does not have them (untracked files are not part of a
/// fresh worktree). Returns the relative paths seeded.
pub fn seed_loop_files(workspace: &Path, wt: &Path) -> Vec<String> {
    let mut seeded = Vec::new();
    for dir in [".claude/skills", ".codex/skills"] {
        let Ok(entries) = std::fs::read_dir(workspace.join(dir)) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with("loop-") {
                continue;
            }
            let rel = format!("{dir}/{name}");
            let dest = wt.join(&rel);
            if dest.exists() {
                continue;
            }
            if copy_tree(&e.path(), &dest).is_ok() {
                seeded.push(rel);
            }
        }
    }
    for rel in [
        ".claude/agents/loop-verifier.md",
        ".codex/agents/verifier.toml",
    ] {
        let src = workspace.join(rel);
        let dest = wt.join(rel);
        if src.is_file() && !dest.exists() && copy_tree(&src, &dest).is_ok() {
            seeded.push(rel.to_string());
        }
    }
    seeded
}

/// `git status --porcelain` paths of the worktree, without the seeded
/// loop assets.
fn dirty_paths(wt: &Worktree) -> Vec<String> {
    git(
        &wt.path,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .unwrap_or_default()
    .lines()
    .filter(|l| l.len() > 3)
    .map(|l| l[3..].trim().trim_matches('"').to_string())
    .filter(|p| !is_seeded_path(p))
    .collect()
}

/// True when the worktree has uncommitted or committed changes against
/// its base, the seeded loop assets aside.
pub fn has_changes(wt: &Worktree) -> bool {
    let dirty = !dirty_paths(wt).is_empty();
    let ahead = git(
        &wt.path,
        &["rev-list", "--count", &format!("{}..HEAD", wt.base)],
    )
    .ok()
    .and_then(|s| s.trim().parse::<u64>().ok())
    .is_some_and(|n| n > 0);
    dirty || ahead
}

/// `git diff --stat` of everything the run changed (working tree and
/// commits) against the base, for the inbox.
pub fn diff_stat(wt: &Worktree) -> String {
    git(&wt.path, &["diff", "--stat", &wt.base]).unwrap_or_default()
}

/// Files changed in the worktree (working tree and commits) against the
/// base.
pub fn changed_files(wt: &Worktree) -> Vec<String> {
    let mut files: Vec<String> = git(&wt.path, &["diff", "--name-only", &wt.base])
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    files.extend(dirty_paths(wt));
    files.retain(|p| !is_seeded_path(p));
    files.sort();
    files.dedup();
    files
}

/// Removes the worktree directory, optionally the branch, and updates the
/// manifest entry to `status` (or drops it when `status` is `None`).
pub fn remove(
    workspace: &Path,
    worktrees_dir: &str,
    wt: &Worktree,
    delete_branch: bool,
    status: Option<&str>,
    run_id: &str,
) -> Result<(), String> {
    let root = workspace.join(worktrees_dir);
    git(
        workspace,
        &["worktree", "remove", "--force", &wt.path.to_string_lossy()],
    )?;
    if delete_branch {
        let _ = git(workspace, &["branch", "-D", &wt.branch]);
    }
    let _ = git(workspace, &["worktree", "prune"]);
    update_manifest(&root, |m| match status {
        Some(s) => {
            for w in m.worktrees.iter_mut().filter(|w| w.id == run_id) {
                w.status = s.to_string();
            }
        }
        None => m.worktrees.retain(|w| w.id != run_id),
    })
    .map_err(|e| e.to_string())
}

/// Manifest entry of a run, as a `Worktree`.
pub fn lookup(workspace: &Path, worktrees_dir: &str, run_id: &str) -> Option<Worktree> {
    load_manifest(&workspace.join(worktrees_dir))
        .worktrees
        .into_iter()
        .find(|w| w.id == run_id)
        .map(|w| Worktree {
            path: PathBuf::from(w.path),
            branch: w.branch,
            base: w.base_branch,
        })
}

/// Removes `active` worktrees older than `max_age` whose run is not in
/// `keep` (the inbox), marking them `stale`. Returns how many went.
pub fn sweep_stale(
    workspace: &Path,
    worktrees_dir: &str,
    keep: &[String],
    max_age: Duration,
    now: time::OffsetDateTime,
) -> usize {
    let root = workspace.join(worktrees_dir);
    let manifest = load_manifest(&root);
    let mut removed = 0;
    for w in manifest.worktrees.iter().filter(|w| w.status == "active") {
        if keep.contains(&w.id) {
            continue;
        }
        let Some(created) = crate::loops::parse_timestamp(&w.created_at) else {
            continue;
        };
        if now - created < time::Duration::try_from(max_age).unwrap_or(time::Duration::ZERO) {
            continue;
        }
        let wt = Worktree {
            path: PathBuf::from(&w.path),
            branch: w.branch.clone(),
            base: w.base_branch.clone(),
        };
        if remove(workspace, worktrees_dir, &wt, true, Some("stale"), &w.id).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let p = temp.path();
        git(p, &["init", "-q", "-b", "main"]).unwrap();
        git(p, &["config", "user.email", "t@example.com"]).unwrap();
        git(p, &["config", "user.name", "t"]).unwrap();
        std::fs::write(p.join("README.md"), "hi\n").unwrap();
        git(p, &["add", "."]).unwrap();
        git(p, &["commit", "-q", "-m", "init"]).unwrap();
        temp
    }

    #[test]
    fn create_change_and_remove_a_worktree() {
        let temp = repo();
        let ws = temp.path();
        assert!(is_git_repo(ws));
        assert_eq!(base_branch(ws), "main");
        let wt = create(
            ws,
            ".loop-worktrees",
            "2026-09-15T10:00:00Z",
            "ci-sweeper",
            "2026-09-15T10:00:00Z",
        )
        .unwrap();
        assert!(wt.path.is_dir());
        assert_eq!(wt.branch, "loop/2026-09-15T10-00-00Z");
        assert!(!has_changes(&wt));
        let m = load_manifest(&ws.join(".loop-worktrees"));
        assert_eq!(m.worktrees.len(), 1);
        assert_eq!(m.worktrees[0].status, "active");
        assert_eq!(m.worktrees[0].base_branch, "main");
        std::fs::write(wt.path.join("fix.txt"), "x\n").unwrap();
        assert!(has_changes(&wt));
        assert_eq!(changed_files(&wt), vec!["fix.txt"]);
        assert_eq!(
            lookup(ws, ".loop-worktrees", "2026-09-15T10:00:00Z").unwrap(),
            wt
        );
        remove(
            ws,
            ".loop-worktrees",
            &wt,
            true,
            Some("rejected"),
            "2026-09-15T10:00:00Z",
        )
        .unwrap();
        assert!(!wt.path.exists());
        assert!(git(ws, &["rev-parse", "--verify", &wt.branch]).is_err());
        let m = load_manifest(&ws.join(".loop-worktrees"));
        assert_eq!(m.worktrees[0].status, "rejected");
        assert!(!ws.join(".loop-worktrees/.manifest.mutex").exists());
    }

    #[test]
    fn untracked_loop_skills_are_seeded_and_ignored_by_change_detection() {
        let temp = repo();
        let ws = temp.path();
        std::fs::create_dir_all(ws.join(".claude/skills/loop-triage")).unwrap();
        std::fs::write(ws.join(".claude/skills/loop-triage/SKILL.md"), "# s\n").unwrap();
        std::fs::create_dir_all(ws.join(".claude/agents")).unwrap();
        std::fs::write(ws.join(".claude/agents/loop-verifier.md"), "# v\n").unwrap();
        std::fs::create_dir_all(ws.join(".claude/skills/other")).unwrap();
        std::fs::write(ws.join(".claude/skills/other/SKILL.md"), "# o\n").unwrap();
        let wt = create(
            ws,
            ".loop-worktrees",
            "r1",
            "daily-triage",
            "2026-09-16T00:00:00Z",
        )
        .unwrap();
        assert!(
            !wt.path.join(".claude/skills/loop-triage/SKILL.md").exists(),
            "untracked"
        );
        let seeded = seed_loop_files(ws, &wt.path);
        assert_eq!(
            seeded,
            vec![
                ".claude/skills/loop-triage",
                ".claude/agents/loop-verifier.md"
            ],
            "only loop assets, not other skills"
        );
        assert!(
            wt.path
                .join(".claude/skills/loop-triage/SKILL.md")
                .is_file()
        );
        assert!(!has_changes(&wt), "seeded files are not a change");
        assert!(changed_files(&wt).is_empty());
        assert!(seed_loop_files(ws, &wt.path).is_empty(), "idempotent");
        std::fs::write(wt.path.join("fix.txt"), "x\n").unwrap();
        assert!(has_changes(&wt));
        assert_eq!(changed_files(&wt), vec!["fix.txt"]);
    }

    #[test]
    fn stale_sweep_keeps_the_inbox() {
        let temp = repo();
        let ws = temp.path();
        let old = "2026-09-01T00:00:00Z";
        let a = create(ws, ".loop-worktrees", "a", "ci-sweeper", old).unwrap();
        let _b = create(ws, ".loop-worktrees", "b", "ci-sweeper", old).unwrap();
        let now = crate::loops::parse_timestamp("2026-09-15T00:00:00Z").unwrap();
        let gone = sweep_stale(
            ws,
            ".loop-worktrees",
            &["b".to_string()],
            Duration::from_secs(86_400),
            now,
        );
        assert_eq!(gone, 1);
        assert!(!a.path.exists());
        let m = load_manifest(&ws.join(".loop-worktrees"));
        let status = |id: &str| {
            m.worktrees
                .iter()
                .find(|w| w.id == id)
                .unwrap()
                .status
                .clone()
        };
        assert_eq!(status("a"), "stale");
        assert_eq!(status("b"), "active");
    }
}
