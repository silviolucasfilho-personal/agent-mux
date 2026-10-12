//! Imports: what each harness remembers on its own, mirrored into the
//! shared memory so the other harnesses see it too. A mirror is a file
//! under `<memory>/imports/`, rebuilt when its source is newer, never
//! edited by a session; `MEMORY.md` lists one line per mirror under
//! `## Imported` (`index_lines`), so a session opens a mirror when it
//! matters instead of carrying it in every start text.
//!
//! Sources, as found on 2026-10-11:
//!
//! - Claude Code 2.1.296: `~/.claude/projects/<slug>/memory/MEMORY.md`,
//!   one `- [Title](file.md) — hook` line per note, and the notes beside
//!   it. The mirror keeps the index lines with their links made absolute,
//!   so they open Claude's own files: Claude's edits stay the truth.
//! - codex-cli 0.162.1: `~/.codex/memories_1.sqlite`, table
//!   `stage1_outputs(thread_id, raw_memory, rollout_summary,
//!   source_updated_at, …)`. The `memories` feature was off on the probed
//!   machine and the table empty, so the reader was checked against the
//!   schema only. A thread belongs to the folder its rollout's
//!   `session_meta` names. The database is opened `immutable`: Codex may
//!   hold it, and a read-only open of a database without its `-shm` file
//!   fails.
//! - Antigravity 1.3.3: `knowledge/` was empty and
//!   `conversation_summaries.db` would not open; nothing is imported.

use super::paths;
use std::path::{Path, PathBuf};

/// The most Codex threads one mirror lists, newest first.
const CODEX_THREADS: usize = 20;
/// A Codex thread's text past this many characters is cut.
const CODEX_CHARS: usize = 2_000;

/// The first line of every mirror: what its index line says.
const SUMMARY: &str = "<!-- summary: ";

/// Which harnesses are imported: `[memory] imports`, every one when it is
/// empty, none with `["none"]`.
pub fn wanted(cfg: Option<&crate::config::MemoryConfig>) -> (bool, bool) {
    let list = cfg.map(|c| c.imports.as_slice()).unwrap_or(&[]);
    if list.is_empty() {
        return (true, true);
    }
    let has = |w: &str| list.iter().any(|i| i.eq_ignore_ascii_case(w));
    (has("claude"), has("codex"))
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Writes `text` to `imports/<name>.md`, or removes it when `None`.
fn put(dir: &Path, name: &str, text: Option<String>) {
    let path = dir.join(paths::IMPORTS).join(format!("{name}.md"));
    match text {
        Some(t) => {
            if std::fs::read_to_string(&path).ok().as_deref() != Some(t.as_str()) {
                let _ = paths::write_atomic(&path, &t);
            }
        }
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Whether `mirror` is older than `source` (or missing).
fn stale(mirror: &Path, source: &Path) -> bool {
    match (mtime(mirror), mtime(source)) {
        (Some(m), Some(s)) => s > m,
        (None, Some(_)) => true,
        _ => false,
    }
}

// ------------------------------------------------------------ Claude Code

/// The mirror of Claude Code's memory folder `src` (its `MEMORY.md`).
pub fn claude_mirror(src: &Path, workspace: &Path) -> Option<String> {
    let index = std::fs::read_to_string(src.join("MEMORY.md")).ok()?;
    let mut lines = Vec::new();
    for line in index.lines() {
        let t = line.trim_start();
        if !t.starts_with("- ") {
            continue;
        }
        // `](file.md)` → `](/abs/…/file.md)`: the link opens Claude's file
        let fixed = match (t.find("]("), t.find(".md)")) {
            (Some(a), Some(b)) if b > a + 2 => {
                let target = &t[a + 2..b + 3];
                if target.starts_with('/') {
                    t.to_string()
                } else {
                    format!(
                        "{}]({}){}",
                        &t[..a],
                        src.join(target).display(),
                        &t[b + 4..]
                    )
                }
            }
            _ => t.to_string(),
        };
        lines.push(fixed);
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "{SUMMARY}Claude Code's own memory of this folder: {} notes -->\n# Imported: Claude Code's memory\n\nWhat Claude Code saved for {} in its own memory ({}). Each line links to Claude's file; Claude edits them, not you.\n\n{}\n",
        lines.len(),
        workspace.display(),
        src.display(),
        lines.join("\n")
    ))
}

fn claude_source(workspace: &Path) -> Option<PathBuf> {
    let base = crate::history::default_claude_dir()?;
    Some(
        base.join("projects")
            .join(crate::history::project_slug(workspace))
            .join("memory"),
    )
}

// ------------------------------------------------------------ Codex

/// One Codex thread's memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexThread {
    pub id: String,
    pub updated_at: i64,
    pub memory: String,
    pub summary: String,
}

/// The threads in `db`, newest first.
pub fn codex_threads(db: &Path) -> Vec<CodexThread> {
    let uri = format!("file:{}?immutable=1", db.display());
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT thread_id, source_updated_at, raw_memory, rollout_summary
         FROM stage1_outputs ORDER BY source_updated_at DESC, thread_id DESC",
    ) else {
        return Vec::new();
    };
    stmt.query_map([], |r| {
        Ok(CodexThread {
            id: r.get(0)?,
            updated_at: r.get(1)?,
            memory: r.get(2)?,
            summary: r.get(3)?,
        })
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

fn cut(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// The mirror of the Codex threads whose rollout ran in `workspace`;
/// `cwd_of` names a thread's folder.
pub fn codex_mirror(
    threads: &[CodexThread],
    workspace: &Path,
    cwd_of: &dyn Fn(&str) -> Option<PathBuf>,
) -> Option<String> {
    let mine: Vec<&CodexThread> = threads
        .iter()
        .filter(|t| cwd_of(&t.id).as_deref() == Some(workspace))
        .take(CODEX_THREADS)
        .collect();
    if mine.is_empty() {
        return None;
    }
    let mut out = format!(
        "{SUMMARY}Codex's own memories of this folder: {} threads -->\n# Imported: Codex's memories\n\nWhat Codex's memory feature kept from its threads in {}, newest first.\n",
        mine.len(),
        workspace.display()
    );
    for t in mine {
        out.push_str(&format!("\n## Thread {}\n\n", t.id));
        if !t.summary.trim().is_empty() {
            out.push_str(&cut(&t.summary, CODEX_CHARS / 2));
            out.push_str("\n\n");
        }
        out.push_str(&cut(&t.memory, CODEX_CHARS));
        out.push('\n');
    }
    Some(out)
}

fn codex_db() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::skill::install::home_dir().join(".codex"))
        .join("memories_1.sqlite")
}

/// The folder of each Codex thread id, from its rollout's file name and
/// `session_meta`, read only for the ids asked.
fn codex_cwds(ids: &[&str]) -> std::collections::HashMap<String, PathBuf> {
    let mut out = std::collections::HashMap::new();
    let Some(root) = crate::handoff::codex_sessions_dir() else {
        return out;
    };
    for p in crate::handoff::codex_rollouts(&root) {
        let name = p.to_string_lossy();
        let Some(id) = ids
            .iter()
            .find(|id| name.ends_with(&format!("-{id}.jsonl")))
        else {
            continue;
        };
        if let Some(cwd) = crate::handoff::codex_meta(&p).and_then(|m| m.cwd) {
            out.insert(id.to_string(), cwd);
        }
    }
    out
}

// ------------------------------------------------------------ refresh

/// Rebuilds the mirrors of `dir` (the memory of `workspace`) whose
/// source changed. Cheap when nothing did: one `stat` per source.
pub fn refresh(dir: &Path, workspace: &Path, cfg: Option<&crate::config::MemoryConfig>) {
    let (claude, codex) = wanted(cfg);
    let imports = dir.join(paths::IMPORTS);
    let claude_out = imports.join("claude.md");
    match claude_source(workspace).filter(|_| claude) {
        Some(src) => {
            let index = src.join("MEMORY.md");
            if stale(&claude_out, &index) || (!index.is_file() && claude_out.is_file()) {
                put(dir, "claude", claude_mirror(&src, workspace));
            }
        }
        None => put(dir, "claude", None),
    }
    let codex_out = imports.join("codex.md");
    let db = codex_db();
    if !codex {
        put(dir, "codex", None);
    } else if stale(&codex_out, &db) {
        let threads = codex_threads(&db);
        let ids: Vec<&str> = threads.iter().map(|t| t.id.as_str()).collect();
        let cwds = if ids.is_empty() {
            Default::default()
        } else {
            codex_cwds(&ids)
        };
        put(
            dir,
            "codex",
            codex_mirror(&threads, workspace, &|id| cwds.get(id).cloned()),
        );
    }
}

/// The `## Imported` lines for `MEMORY.md`, one per mirror.
pub fn index_lines(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir.join(paths::IMPORTS)) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            let first = text.lines().next()?;
            let summary = first.strip_prefix(SUMMARY)?.strip_suffix(" -->")?;
            let stem = p.file_stem()?.to_string_lossy().into_owned();
            Some(format!(
                "- [{stem}]({}/{stem}.md) — {summary} (imported)",
                paths::IMPORTS
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_lines_point_at_claudes_own_files() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("memory");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("MEMORY.md"),
            "- [Never push to master](no-push.md) — work on a branch\n- [Abs](/x/y.md) — kept\nnot a line\n",
        )
        .unwrap();
        let m = claude_mirror(&src, Path::new("/ws")).unwrap();
        assert!(
            m.starts_with("<!-- summary: Claude Code's own memory of this folder: 2 notes -->")
        );
        assert!(
            m.contains(&format!(
                "- [Never push to master]({}) — work on a branch",
                src.join("no-push.md").display()
            )),
            "{m}"
        );
        assert!(m.contains("- [Abs](/x/y.md) — kept"));
        assert!(!m.contains("not a line"));
        std::fs::write(src.join("MEMORY.md"), "# empty\n").unwrap();
        assert!(claude_mirror(&src, Path::new("/ws")).is_none());
    }

    #[test]
    fn codex_threads_are_read_and_kept_to_their_folder() {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("memories_1.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE stage1_outputs (thread_id TEXT PRIMARY KEY, source_updated_at INTEGER NOT NULL,
               raw_memory TEXT NOT NULL, rollout_summary TEXT NOT NULL, rollout_slug TEXT,
               generated_at INTEGER NOT NULL, usage_count INTEGER, last_usage INTEGER,
               selected_for_phase2 INTEGER NOT NULL DEFAULT 0, selected_for_phase2_source_updated_at INTEGER);
             INSERT INTO stage1_outputs (thread_id, source_updated_at, raw_memory, rollout_summary, generated_at)
               VALUES ('t-old', 1, 'uses pnpm, not npm', 'set up the build', 1),
                      ('t-new', 2, 'tests need FAKE_S3=1', 'fixed the tests', 2),
                      ('t-other', 3, 'unrelated', 'elsewhere', 3);",
        )
        .unwrap();
        drop(conn);
        let threads = codex_threads(&db);
        assert_eq!(
            threads.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["t-other", "t-new", "t-old"]
        );
        let cwd = |id: &str| match id {
            "t-other" => Some(PathBuf::from("/elsewhere")),
            _ => Some(PathBuf::from("/ws")),
        };
        let m = codex_mirror(&threads, Path::new("/ws"), &cwd).unwrap();
        assert!(m.starts_with("<!-- summary: Codex's own memories of this folder: 2 threads -->"));
        assert!(
            m.find("t-new").unwrap() < m.find("t-old").unwrap(),
            "newest first"
        );
        assert!(m.contains("FAKE_S3=1") && !m.contains("unrelated"));
        assert!(codex_mirror(&threads, Path::new("/none"), &cwd).is_none());
        assert!(codex_threads(&d.path().join("missing.sqlite")).is_empty());
    }

    #[test]
    fn the_index_lists_one_line_per_mirror() {
        let d = tempfile::tempdir().unwrap();
        put(
            d.path(),
            "claude",
            Some(
                "<!-- summary: Claude Code's own memory of this folder: 3 notes -->\nbody\n".into(),
            ),
        );
        std::fs::write(
            d.path().join(paths::IMPORTS).join("stray.md"),
            "no summary\n",
        )
        .unwrap();
        assert_eq!(
            index_lines(d.path()),
            [
                "- [claude](imports/claude.md) — Claude Code's own memory of this folder: 3 notes (imported)"
            ]
        );
        put(d.path(), "claude", None);
        assert!(index_lines(d.path()).is_empty());
    }

    #[test]
    fn imports_follow_the_setting() {
        use crate::config::MemoryConfig;
        assert_eq!(wanted(None), (true, true));
        let only = MemoryConfig {
            imports: vec!["codex".into()],
            ..Default::default()
        };
        assert_eq!(wanted(Some(&only)), (false, true));
        let none = MemoryConfig {
            imports: vec!["none".into()],
            ..Default::default()
        };
        assert_eq!(wanted(Some(&none)), (false, false));
    }
}
