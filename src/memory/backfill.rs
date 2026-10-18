//! `agent-mux memory backfill`: ai-memory's own `backfill` run in every
//! folder the trace store has seen a session in, so the history from
//! before ai-memory was installed becomes searchable memory.
//!
//! As probed on ai-memory 2.6.7 (`commands/backfill.rs`):
//!
//! - `ai-memory backfill` imports the native transcripts (Claude Code,
//!   Codex, Antigravity, …) whose recorded working directory is exactly
//!   the folder it runs in, newest first up to `--max-sessions` (and 200
//!   per harness), replaying each through `/hook/batch` with the
//!   transcript's own times. The project is named from the folder, so the
//!   folders of one repository land in one project.
//! - It imports only into a project with no captured sessions unless
//!   `--force`, and an import stores every session again: a second run
//!   doubles what the first stored (probed: 4 imports of one session, 16
//!   observations stored, none replayed), as it does a session the hooks
//!   already captured.
//! - `--dry-run --json` prints `{workspace, project, selected,
//!   skipped_non_empty, …}` without importing; a run prints the same with
//!   `imported_sessions`, `imported_events` and `failed_sessions`.
//!
//! So the plan asks each folder once without `--force` (is its project
//! empty?) and, when it is not, once with it (how many sessions). A
//! project that was empty imports all its folders with `--force`, since
//! after the first folder it no longer is; one that already has captured
//! sessions is left alone unless the user passes `--force`. A folder
//! agent-mux has imported is listed in `ai-memory-feed/backfilled` and
//! never imported again, `--force` or not.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// All the sessions ai-memory finds (it scans at most 200 per harness).
pub const ALL: usize = 10_000;

/// `ai-memory backfill --json`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Report {
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub selected: usize,
    #[serde(default)]
    pub imported_sessions: usize,
    #[serde(default)]
    pub imported_events: usize,
    #[serde(default)]
    pub failed_sessions: usize,
    #[serde(default)]
    pub skipped_for_cap: usize,
    #[serde(default)]
    pub skipped_non_empty: bool,
}

pub fn parse_report(text: &str) -> Result<Report, String> {
    // the report is the last JSON object on stdout
    let start = text.find('{').ok_or("ai-memory backfill: no report")?;
    serde_json::from_str(text[start..].trim()).map_err(|e| format!("ai-memory backfill: {e}"))
}

/// One folder of the plan.
#[derive(Debug, Clone, PartialEq)]
pub struct Folder {
    pub dir: PathBuf,
    pub workspace: String,
    pub project: String,
    /// Its local sessions ai-memory would import.
    pub sessions: usize,
    /// Its project already has captured sessions.
    pub captured: bool,
}

impl Folder {
    pub fn scope(&self) -> String {
        format!("{}/{}", self.workspace, self.project)
    }

    /// Whether it is imported: an empty project, or `--force`.
    pub fn imported(&self, force: bool) -> bool {
        self.sessions > 0 && (force || !self.captured)
    }
}

/// The list of folders agent-mux has imported.
pub fn done_file(agent_mux_home: &Path) -> PathBuf {
    super::feed::dir(agent_mux_home).join("backfilled")
}

pub fn done(agent_mux_home: &Path) -> BTreeSet<PathBuf> {
    std::fs::read_to_string(done_file(agent_mux_home))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

pub fn mark_done(agent_mux_home: &Path, dir: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let f = done_file(agent_mux_home);
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(f)?;
    writeln!(file, "{}", dir.display())
}

/// The folders the trace store has seen a session in that still exist,
/// leaving out agent-mux's own files and loop worktrees (headless runs,
/// which the live feed leaves out too).
pub fn folders(db: &Path, agent_mux_home: &Path) -> Result<Vec<PathBuf>, String> {
    let conn = crate::tracing::store::open_ro(db)?;
    let mut st = conn
        .prepare(
            "SELECT cwd FROM sessions WHERE cwd IS NOT NULL AND cwd <> ''
             UNION SELECT cwd FROM hook_events WHERE cwd IS NOT NULL AND cwd <> ''",
        )
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = BTreeSet::new();
    for cwd in rows.flatten() {
        let p = PathBuf::from(cwd.trim());
        let loop_run = p.components().any(|c| c.as_os_str() == ".loop-worktrees");
        if p.is_absolute() && p.is_dir() && !p.starts_with(agent_mux_home) && !loop_run {
            out.insert(p);
        }
    }
    Ok(out.into_iter().collect())
}

/// `ai-memory backfill <args> --json` in `dir`.
pub fn run(bin: &Path, dir: &Path, args: &[&str]) -> Result<Report, String> {
    let out = std::process::Command::new(bin)
        .arg("backfill")
        .args(args)
        .arg("--json")
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", bin.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // a run with failed sessions exits non-zero after its report
    match parse_report(&stdout) {
        Ok(r) => Ok(r),
        Err(e) if out.status.success() => Err(e),
        Err(_) => {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(err
                .lines()
                .rev()
                .find_map(super::install::tidy)
                .unwrap_or_else(|| out.status.to_string()))
        }
    }
}

/// What each folder would import (dry runs, nothing is written).
pub fn plan(bin: &Path, dirs: &[PathBuf], max: usize) -> Vec<(PathBuf, Result<Folder, String>)> {
    let max = max.to_string();
    dirs.iter()
        .map(|dir| {
            let folder = (|| {
                let first = run(bin, dir, &["--dry-run", "--max-sessions", &max])?;
                let r = if first.skipped_non_empty {
                    run(bin, dir, &["--dry-run", "--force", "--max-sessions", &max])?
                } else {
                    first.clone()
                };
                Ok(Folder {
                    dir: dir.clone(),
                    workspace: r.workspace,
                    project: r.project,
                    sessions: r.selected,
                    captured: first.skipped_non_empty,
                })
            })();
            (dir.clone(), folder)
        })
        .collect()
}

/// Imports one folder of the plan.
pub fn import(bin: &Path, folder: &Folder, max: usize) -> Result<Report, String> {
    run(
        bin,
        &folder.dir,
        &["--force", "--max-sessions", &max.to_string()],
    )
}

/// The plan as lines, a project at a time.
pub fn plan_lines(folders: &[Folder], force: bool, home: &Path) -> Vec<String> {
    let scopes: BTreeSet<String> = folders.iter().map(Folder::scope).collect();
    let mut out = Vec::new();
    for scope in scopes {
        let of: Vec<&Folder> = folders.iter().filter(|f| f.scope() == scope).collect();
        let total: usize = of.iter().map(|f| f.sessions).sum();
        let captured = of.iter().any(|f| f.captured);
        let what = match (total, captured, force) {
            (0, _, _) => "nothing to import".to_string(),
            (n, false, _) => format!("{n} session(s)"),
            (n, true, true) => {
                format!("{n} session(s); the sessions it already has may be stored twice")
            }
            (n, true, false) => {
                format!(
                    "{n} session(s) left alone: it already has captured sessions (--force imports them)"
                )
            }
        };
        out.push(format!("{scope}  {what}"));
        for f in of {
            let shown = f
                .dir
                .strip_prefix(home)
                .map(|p| format!("~/{}", p.display()))
                .unwrap_or_else(|_| f.dir.display().to_string());
            out.push(format!("    {:>4}  {shown}", f.sessions));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(dir: &str, project: &str, sessions: usize, captured: bool) -> Folder {
        Folder {
            dir: PathBuf::from(dir),
            workspace: "default".into(),
            project: project.into(),
            sessions,
            captured,
        }
    }

    #[test]
    fn the_report_parses_past_log_lines() {
        let r = parse_report(
            "note\n{\n  \"workspace\": \"default\",\n  \"project\": \"app\",\n  \"selected\": 3,\n  \"imported_sessions\": 2,\n  \"imported_events\": 40,\n  \"skipped_for_cap\": 0,\n  \"failed_sessions\": 1,\n  \"skipped_non_empty\": false,\n  \"dry_run\": false\n}\n",
        )
        .unwrap();
        assert_eq!((r.project.as_str(), r.selected), ("app", 3));
        assert_eq!((r.imported_sessions, r.failed_sessions), (2, 1));
        assert!(parse_report("").is_err());
    }

    #[test]
    fn a_project_with_captured_sessions_waits_for_force() {
        let a = folder("/h/app", "app", 5, true);
        let b = folder("/h/new", "new", 2, false);
        let c = folder("/h/empty", "empty", 0, false);
        assert!(!a.imported(false) && a.imported(true));
        assert!(b.imported(false));
        assert!(!c.imported(true));
    }

    #[test]
    fn the_plan_groups_folders_by_project() {
        let fs = vec![
            folder("/h/w/app", "app", 5, true),
            folder("/h/w/app/sub", "app", 1, true),
            folder("/h/w/new", "new", 2, false),
        ];
        let lines = plan_lines(&fs, false, Path::new("/h"));
        assert_eq!(
            lines,
            vec![
                "default/app  6 session(s) left alone: it already has captured sessions (--force imports them)",
                "       5  ~/w/app",
                "       1  ~/w/app/sub",
                "default/new  2 session(s)",
                "       2  ~/w/new",
            ]
        );
        assert!(plan_lines(&fs, true, Path::new("/h"))[0].contains("may be stored twice"));
    }

    #[test]
    fn imported_folders_are_remembered() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(done(tmp.path()).is_empty());
        mark_done(tmp.path(), Path::new("/w/app")).unwrap();
        mark_done(tmp.path(), Path::new("/w/new")).unwrap();
        assert_eq!(
            done(tmp.path()).into_iter().collect::<Vec<_>>(),
            vec![PathBuf::from("/w/app"), PathBuf::from("/w/new")]
        );
    }

    #[test]
    fn folders_come_from_the_trace_store() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("traces.db");
        let real = tmp.path().join("proj");
        let own = tmp.path().join("amx/loops/wt");
        let wt = real.join(".loop-worktrees/2026-09-16T09-41-33Z");
        for d in [&real, &own, &wt] {
            std::fs::create_dir_all(d).unwrap();
        }
        drop(crate::tracing::store::open_rw(&db, Default::default()).unwrap());
        let conn = rusqlite::Connection::open(&db).unwrap();
        for (i, (t, cwd)) in [
            ("sessions", real.display().to_string()),
            ("hook_events", real.display().to_string()),
            ("hook_events", own.display().to_string()),
            ("hook_events", wt.display().to_string()),
            ("sessions", "/nonexistent/gone".to_string()),
            ("sessions", String::new()),
        ]
        .into_iter()
        .enumerate()
        {
            let sql = if t == "sessions" {
                "INSERT INTO sessions (key, provider, session_id, cwd, first_seen_ns, last_seen_ns)
                 VALUES (?1, 'claude', ?1, ?2, 0, 0)"
            } else {
                "INSERT INTO hook_events (key, provider, session_id, event, ts_ns, cwd)
                 VALUES (?1, 'claude', ?1, 'SessionStart', 0, ?2)"
            };
            conn.execute(sql, rusqlite::params![format!("k{i}"), cwd])
                .unwrap();
        }
        drop(conn);
        assert_eq!(folders(&db, &tmp.path().join("amx")).unwrap(), vec![real]);
    }
}
