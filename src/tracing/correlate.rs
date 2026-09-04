//! Correlating an agent-mux session with the CLI's own on-disk transcript.
//!
//! Claude gets a pre-assigned `--session-id` so its path is known up front
//! (with a uuid-filename glob fallback in case the slug scheme drifts).
//! Codex is adopted by watching `~/.codex/sessions/<date>/` for a fresh
//! rollout whose first-line `session_meta.cwd` matches ours exactly.
//! Antigravity has no exact signal: new `brain/` subdirs are confirmed by
//! presence-lock mtime, a cwd-substring check, or (after 15 s) being the
//! only unclaimed candidate — those adoptions are tagged "heuristic".
//!
//! A process-wide claim registry keyed `"{provider}:{id}"` prevents two
//! concurrent sessions from adopting the same transcript; claims are
//! released when the claiming pipeline exits.

use crate::transcript::{Provider, TranscriptEvent};
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

pub type ClaimRegistry = Mutex<HashSet<String>>;

pub fn claim_key(provider: Provider, id: &str) -> String {
    format!("{}:{id}", provider.as_str())
}

/// Atomically claims `id` for `provider`; false if already claimed.
pub fn try_claim(claims: &ClaimRegistry, provider: Provider, id: &str) -> bool {
    claims
        .lock()
        .map(|mut set| set.insert(claim_key(provider, id)))
        .unwrap_or(false)
}

pub fn release_claim(claims: &ClaimRegistry, provider: Provider, id: &str) {
    if let Ok(mut set) = claims.lock() {
        set.remove(&claim_key(provider, id));
    }
}

/// RAII release of a transcript claim: held by the pipeline for its
/// lifetime, so the claim is returned even when the pipeline panics or its
/// task is aborted (a leaked claim would silently block re-adoption of that
/// conversation for the rest of the run).
pub struct ClaimGuard {
    claims: std::sync::Arc<ClaimRegistry>,
    provider: Provider,
    id: String,
}

impl ClaimGuard {
    /// Wraps an ALREADY-registered claim (the atomic insert happens inside
    /// `poll` / `try_claim`).
    pub fn new(claims: std::sync::Arc<ClaimRegistry>, provider: Provider, id: &str) -> ClaimGuard {
        ClaimGuard {
            claims,
            provider,
            id: id.to_string(),
        }
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        release_claim(&self.claims, self.provider, &self.id);
    }
}

/// How long before a sole unclaimed Antigravity candidate is adopted on
/// timing alone.
const ANTIGRAVITY_SOLE_CANDIDATE: Duration = Duration::from_secs(15);
/// Slack on mtime comparisons (filesystems, WSL clock skew).
const MTIME_SLACK: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum CorrelationSpec {
    /// Claude with a known session uuid (injected `--session-id`, or an
    /// explicit id found in `--resume <id>` / `--session-id <id>` args).
    KnownClaude {
        session_id: String,
        /// `<claude_dir>/projects/<slug>/<uuid>.jsonl`
        expected_path: PathBuf,
        /// `<claude_dir>/projects`, for the glob fallback.
        projects_dir: PathBuf,
        /// True for resumes: the pre-existing content is primed, not emitted.
        resume: bool,
    },
    /// Claude watching for newly created or active session in projects_dir.
    WatchClaude {
        projects_dir: PathBuf,
        cwd: PathBuf,
        t0: SystemTime,
    },
    /// Antigravity resume via `--conversation <id>`.
    KnownAntigravity {
        conversation_id: String,
        /// The antigravity-cli root (brain/ and presence/ live under it).
        root: PathBuf,
    },
    WatchCodex {
        sessions_dir: PathBuf,
        cwd: PathBuf,
        t0: SystemTime,
    },
    WatchAntigravity {
        root: PathBuf,
        cwd: PathBuf,
        t0: SystemTime,
        /// brain/ subdirs present at the first poll — only NEW dirs are
        /// candidates. `None` until that snapshot is taken.
        initial: Option<HashSet<OsString>>,
    },
    /// Untraceable launch (e.g. bare `-r` / `--continue` with no id).
    None,
}

#[derive(Debug, PartialEq)]
pub struct Adopted {
    pub session_id: String,
    pub path: PathBuf,
    /// "deterministic" | "watched" | "heuristic"
    pub correlation: &'static str,
    /// True when the file's existing content is history (Known resume):
    /// prime it instead of exporting it.
    pub resume_prime: bool,
}

/// The transcript file for an Antigravity conversation dir: prefer
/// `transcript_full.jsonl` (what agy's own hooks point at; the condensed
/// file truncates content), fall back to `transcript.jsonl`.
fn antigravity_transcript(brain_conv_dir: &Path) -> Option<PathBuf> {
    let logs = brain_conv_dir.join(".system_generated").join("logs");
    let full = logs.join("transcript_full.jsonl");
    if full.is_file() {
        return Some(full);
    }
    let condensed = logs.join("transcript.jsonl");
    condensed.is_file().then_some(condensed)
}

/// First line of a file, capped so a corrupt/huge line can't balloon memory.
fn read_first_line(path: &Path, cap: usize) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file).take(cap as u64);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let line = line.trim_end_matches('\n');
    (!line.is_empty()).then(|| line.to_string())
}

fn mtime_in_window(path: &Path, t0: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|mtime| mtime + MTIME_SLACK >= t0)
        .unwrap_or(false)
}

/// The `sessions/YYYY/MM/DD` dirs worth scanning: UTC dates around t0 and
/// now, ±1 day, covering any local-time offset codex may use for its dirs.
fn codex_date_dirs(sessions_dir: &Path, t0: SystemTime) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    for base in [t0, SystemTime::now()] {
        for delta_days in [-1i64, 0, 1] {
            let Some(t) = (if delta_days >= 0 {
                base.checked_add(Duration::from_secs(delta_days as u64 * 86400))
            } else {
                base.checked_sub(Duration::from_secs(delta_days.unsigned_abs() * 86400))
            }) else {
                continue;
            };
            let odt = time::OffsetDateTime::from(t);
            let key = (odt.year(), odt.month() as u8, odt.day());
            if seen.insert(key) {
                dirs.push(sessions_dir.join(format!("{:04}/{:02}/{:02}", key.0, key.1, key.2)));
            }
        }
    }
    dirs
}

/// One correlation poll tick. Returns the adopted transcript once found (the
/// claim is already registered); `None` keeps polling.
pub fn poll(
    spec: &mut CorrelationSpec,
    started: Instant,
    claims: &ClaimRegistry,
) -> Option<Adopted> {
    match spec {
        CorrelationSpec::None => None,
        CorrelationSpec::KnownClaude {
            session_id,
            expected_path,
            projects_dir,
            resume,
        } => {
            if expected_path.is_file() {
                if !try_claim(claims, Provider::Claude, session_id) {
                    return None;
                }
                return Some(Adopted {
                    session_id: session_id.clone(),
                    path: expected_path.clone(),
                    correlation: "deterministic",
                    resume_prime: *resume,
                });
            }
            // The uuid is the invariant; the slug scheme is not.
            // Look for the uuid filename under any project dir.
            if let Ok(entries) = std::fs::read_dir(&*projects_dir) {
                let filename = format!("{session_id}.jsonl");
                for entry in entries.filter_map(|e| e.ok()) {
                    let candidate = entry.path().join(&filename);
                    if candidate.is_file() {
                        if !try_claim(claims, Provider::Claude, session_id) {
                            return None;
                        }
                        return Some(Adopted {
                            session_id: session_id.clone(),
                            path: candidate,
                            correlation: "deterministic",
                            resume_prime: *resume,
                        });
                    }
                }
            }
            None
        }
        CorrelationSpec::WatchClaude {
            projects_dir,
            cwd,
            t0,
        } => {
            let want_slug = crate::history::project_slug(cwd);
            let preferred_dir = projects_dir.join(&want_slug);
            if let Ok(entries) = std::fs::read_dir(&preferred_dir) {
                let mut candidates: Vec<_> = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
                    .filter(|e| mtime_in_window(&e.path(), *t0))
                    .collect();
                candidates.sort_by_key(|e| {
                    std::fs::metadata(e.path())
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH)
                });
                if let Some(newest) = candidates.last() {
                    let path = newest.path();
                    let session_id = path.file_stem().unwrap().to_string_lossy().into_owned();
                    if try_claim(claims, Provider::Claude, &session_id) {
                        return Some(Adopted {
                            session_id,
                            path,
                            correlation: "watched",
                            resume_prime: false,
                        });
                    }
                }
            }
            if let Ok(project_entries) = std::fs::read_dir(&*projects_dir) {
                let mut candidates = Vec::new();
                for proj in project_entries.filter_map(|e| e.ok()) {
                    if !proj.path().is_dir() {
                        continue;
                    }
                    if let Ok(entries) = std::fs::read_dir(proj.path()) {
                        for e in entries.filter_map(|e| e.ok()) {
                            let p = e.path();
                            if p.extension().is_some_and(|ext| ext == "jsonl")
                                && mtime_in_window(&p, *t0)
                            {
                                let mtime = std::fs::metadata(&p)
                                    .and_then(|m| m.modified())
                                    .unwrap_or(SystemTime::UNIX_EPOCH);
                                candidates.push((mtime, p));
                            }
                        }
                    }
                }
                candidates.sort_by_key(|(mtime, _)| *mtime);
                if let Some((_, path)) = candidates.last() {
                    let session_id = path.file_stem().unwrap().to_string_lossy().into_owned();
                    if try_claim(claims, Provider::Claude, &session_id) {
                        return Some(Adopted {
                            session_id,
                            path: path.clone(),
                            correlation: "heuristic",
                            resume_prime: false,
                        });
                    }
                }
            }
            None
        }
        CorrelationSpec::KnownAntigravity {
            conversation_id,
            root,
        } => {
            let conv_dir = root.join("brain").join(&*conversation_id);
            let path = antigravity_transcript(&conv_dir)?;
            if !try_claim(claims, Provider::Antigravity, conversation_id) {
                return None;
            }
            Some(Adopted {
                session_id: conversation_id.clone(),
                path,
                correlation: "deterministic",
                resume_prime: true,
            })
        }
        CorrelationSpec::WatchCodex {
            sessions_dir,
            cwd,
            t0,
        } => {
            let want_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
            for date_dir in codex_date_dirs(sessions_dir, *t0) {
                let Ok(entries) = std::fs::read_dir(&date_dir) else {
                    continue;
                };
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                        continue;
                    }
                    if !mtime_in_window(&path, *t0) {
                        continue;
                    }
                    let Some(first) = read_first_line(&path, 256 * 1024) else {
                        continue;
                    };
                    let events = crate::transcript::parse_codex_line(&first);
                    let Some(TranscriptEvent::SessionMeta {
                        session_id: Some(id),
                        cwd: Some(meta_cwd),
                        ..
                    }) = events.first()
                    else {
                        continue;
                    };
                    let meta_cwd_path = Path::new(meta_cwd);
                    let meta_canon = meta_cwd_path
                        .canonicalize()
                        .unwrap_or_else(|_| meta_cwd_path.to_path_buf());
                    if meta_canon != want_cwd {
                        continue;
                    }
                    if !try_claim(claims, Provider::Codex, id) {
                        continue; // another session already owns it
                    }
                    return Some(Adopted {
                        session_id: id.clone(),
                        path,
                        correlation: "watched",
                        resume_prime: false,
                    });
                }
            }
            None
        }
        CorrelationSpec::WatchAntigravity {
            root,
            cwd,
            t0,
            initial,
        } => {
            let brain = root.join("brain");
            let list_dirs = || -> HashSet<OsString> {
                std::fs::read_dir(&brain)
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .filter(|e| e.path().is_dir())
                            .map(|e| e.file_name())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let Some(snapshot) = initial else {
                // first poll: snapshot what already exists; only NEW dirs
                // are candidates
                *initial = Some(list_dirs());
                return None;
            };
            let current = list_dirs();
            let mut candidates: Vec<String> = Vec::new();
            for name in current.difference(snapshot) {
                let id = name.to_string_lossy().into_owned();
                if antigravity_transcript(&brain.join(&id)).is_none() {
                    continue;
                }
                if claims
                    .lock()
                    .map(|set| set.contains(&claim_key(Provider::Antigravity, &id)))
                    .unwrap_or(true)
                {
                    continue;
                }
                candidates.push(id);
            }
            candidates.sort();
            let adopt = |id: &str, correlation: &'static str| -> Option<Adopted> {
                let path = antigravity_transcript(&brain.join(id))?;
                if !try_claim(claims, Provider::Antigravity, id) {
                    return None;
                }
                Some(Adopted {
                    session_id: id.to_string(),
                    path,
                    correlation,
                    resume_prime: false,
                })
            };
            // Tier 1: presence lock created since spawn — the strongest signal.
            for id in &candidates {
                let lock = root.join("presence").join(format!("{id}.lock"));
                if lock.is_file()
                    && mtime_in_window(&lock, *t0)
                    && let Some(adopted) = adopt(id, "watched")
                {
                    return Some(adopted);
                }
            }
            // Tier 2: early transcript lines mention our cwd.
            let cwd_str = cwd.to_string_lossy();
            for id in &candidates {
                if let Some(path) = antigravity_transcript(&brain.join(id)) {
                    let head = std::fs::File::open(&path)
                        .ok()
                        .map(|f| {
                            use std::io::Read;
                            let mut buf = String::new();
                            let _ = f.take(64 * 1024).read_to_string(&mut buf);
                            buf
                        })
                        .unwrap_or_default();
                    if head.contains(cwd_str.as_ref())
                        && let Some(adopted) = adopt(id, "heuristic")
                    {
                        return Some(adopted);
                    }
                }
            }
            // Tier 3: one sole candidate after a generous wait.
            if started.elapsed() >= ANTIGRAVITY_SOLE_CANDIDATE
                && candidates.len() == 1
                && let Some(adopted) = adopt(&candidates[0], "heuristic")
            {
                return Some(adopted);
            }
            None
        }
    }
}

/// The rollout file for a Codex thread id, wherever it sits under the
/// sessions tree (`YYYY/MM/DD/rollout-<ts>-<thread-id>.jsonl`). A hook or
/// `notify` payload names the thread; this finds its transcript without
/// the cwd heuristic.
pub fn codex_rollout_by_thread(sessions_dir: &Path, thread_id: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, suffix: &str, depth: usize) -> Option<PathBuf> {
        if depth > 4 {
            return None;
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .collect();
        // newest date directories first
        entries.sort();
        entries.reverse();
        for path in entries {
            if path.is_dir() {
                if let Some(found) = walk(&path, suffix, depth + 1) {
                    return Some(found);
                }
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(suffix))
            {
                return Some(path);
            }
        }
        None
    }
    if thread_id.is_empty() {
        return None;
    }
    walk(sessions_dir, &format!("-{thread_id}.jsonl"), 0)
}

#[cfg(test)]
mod thread_lookup_tests {
    use super::*;

    #[test]
    fn finds_the_rollout_for_a_thread_id() {
        let dir = tempfile::tempdir().unwrap();
        let day = dir.path().join("2026").join("09").join("03");
        std::fs::create_dir_all(&day).unwrap();
        let wanted = day.join("rollout-2026-09-03T10-00-00-thread-abc.jsonl");
        std::fs::write(&wanted, "{}\n").unwrap();
        std::fs::write(
            day.join("rollout-2026-09-03T09-00-00-thread-other.jsonl"),
            "{}\n",
        )
        .unwrap();
        assert_eq!(
            codex_rollout_by_thread(dir.path(), "thread-abc"),
            Some(wanted)
        );
        assert_eq!(codex_rollout_by_thread(dir.path(), "nope"), None);
        assert_eq!(codex_rollout_by_thread(dir.path(), ""), None);
    }
}
