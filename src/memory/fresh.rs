//! Freshness: a running session hears about the notes saved after it
//! started. The start text gives a session the memory as it was; before
//! each prompt (`UserPromptSubmit`) the hook compares the notes with what
//! this session has already been given and injects only the difference.
//!
//! What a session has seen is `<memory>/.seen/<harness>-<session>.json`:
//! each note's modification time, written when the start text is given
//! and after each difference. A session with no record yet (one whose
//! start text came another way, like Codex's developer instructions) is
//! given nothing the first time and starts its record there.
//!
//! Probed 2026-10-11: a `UserPromptSubmit` hook answering
//! `{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":…}}`
//! reached the model on Claude Code 2.1.296 (`--settings`) and on
//! codex-cli 0.162.1 (`hooks.json`, trust bypassed); both payloads carry
//! `session_id` and `cwd`. Antigravity needs none of this: its hook gives
//! the whole index before every model call.

use super::{fence, index, note, paths};
use crate::prompts::Prompts;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const SEEN: &str = ".seen";

/// A record older than this belongs to a session long gone.
const KEEP: Duration = Duration::from_secs(14 * 24 * 3600);

/// `ws:<name>` / `user:<name>` → modification time in nanoseconds.
type Snapshot = BTreeMap<String, u128>;

fn snapshot(dir: &Path, user: Option<&Path>) -> Snapshot {
    let mut out = Snapshot::new();
    for (prefix, d) in std::iter::once(("ws", dir)).chain(user.map(|u| ("user", u))) {
        for f in note::files(d) {
            let Some(stem) = f.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            let t = std::fs::metadata(&f)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            out.insert(format!("{prefix}:{stem}"), t);
        }
    }
    out
}

pub fn seen_path(dir: &Path, harness: &str, session: &str) -> PathBuf {
    dir.join(SEEN)
        .join(format!("{harness}-{}.json", note::slug(session)))
}

fn read(path: &Path) -> Option<Snapshot> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn write(path: &Path, s: &Snapshot) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
        // drop the records of sessions long gone
        if let Ok(entries) = std::fs::read_dir(parent) {
            for e in entries.flatten() {
                let old = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > KEEP);
                if old {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    if let Ok(text) = serde_json::to_string(s) {
        let _ = paths::write_atomic(path, &text);
    }
}

/// The session was just given the memory as it is now.
pub fn record(dir: &Path, user: Option<&Path>, harness: &str, session: &str) {
    if session.is_empty() {
        return;
    }
    write(&seen_path(dir, harness, session), &snapshot(dir, user));
}

/// What changed since the session last looked, fenced, or `None`. Either
/// way the session's record is brought up to date.
pub fn delta(
    dir: &Path,
    user: Option<&Path>,
    harness: &str,
    session: &str,
    prompts: &Prompts,
) -> Option<String> {
    if session.is_empty() {
        return None;
    }
    let path = seen_path(dir, harness, session);
    let now = snapshot(dir, user);
    let Some(seen) = read(&path) else {
        write(&path, &now);
        return None;
    };
    if seen == now {
        return None;
    }
    write(&path, &now);
    let mut added = Vec::new();
    let mut changed = Vec::new();
    for (key, t) in &now {
        match seen.get(key) {
            None => added.push(key),
            Some(old) if old != t => changed.push(key),
            _ => {}
        }
    }
    let removed: Vec<&String> = seen.keys().filter(|k| !now.contains_key(*k)).collect();
    let line_of = |key: &str| -> Option<String> {
        let (prefix, name) = key.split_once(':')?;
        let d = if prefix == "user" { user? } else { dir };
        let n = note::read(d, name).ok()?;
        let line = index::line(&n);
        Some(if prefix == "user" {
            line.replacen("](notes/", &format!("]({}/notes/", d.display()), 1)
        } else {
            line
        })
    };
    let mut body = format!(
        "The memory changed since this session last read it (another session saved, or you did). Notes are in {}:\n",
        dir.display()
    );
    let new_lines: Vec<String> = added.iter().filter_map(|k| line_of(k)).collect();
    let changed_lines: Vec<String> = changed.iter().filter_map(|k| line_of(k)).collect();
    if !new_lines.is_empty() {
        body.push_str("\nNew:\n");
        body.push_str(&new_lines.join("\n"));
        body.push('\n');
    }
    if !changed_lines.is_empty() {
        body.push_str("\nUpdated (open the note for its new text):\n");
        body.push_str(&changed_lines.join("\n"));
        body.push('\n');
    }
    if !removed.is_empty() {
        let names: Vec<&str> = removed
            .iter()
            .filter_map(|k| k.split_once(':').map(|(_, n)| n))
            .collect();
        body.push_str(&format!(
            "\nNo longer in the memory: {}\n",
            names.join(", ")
        ));
    }
    if new_lines.is_empty() && changed_lines.is_empty() && removed.is_empty() {
        return None;
    }
    Some(fence::fence(&prompts.memory_notice, &body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(dir: &Path, name: &str, desc: &str) {
        paths::ensure(dir).unwrap();
        std::fs::write(
            paths::note_path(dir, name),
            format!("---\nname: {name}\ndescription: {desc}\nkind: project\n---\n\nbody\n"),
        )
        .unwrap();
    }

    #[test]
    fn a_session_hears_only_what_changed_since_it_last_looked() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("ws");
        let p = Prompts::builtin();
        put(&dir, "old", "known at start");
        put(&dir, "gone", "removed later");
        record(&dir, None, "claude", "s1");
        assert!(
            delta(&dir, None, "claude", "s1", &p).is_none(),
            "nothing new"
        );

        std::thread::sleep(Duration::from_millis(20));
        put(&dir, "fresh", "saved by another session");
        put(&dir, "old", "known at start, then corrected");
        std::fs::remove_file(paths::note_path(&dir, "gone")).unwrap();
        let text = delta(&dir, None, "claude", "s1", &p).expect("a difference");
        assert!(text.contains(fence::START), "{text}");
        assert!(text.contains("New:\n- [fresh]"), "{text}");
        assert!(text.contains("Updated") && text.contains("[old]"), "{text}");
        assert!(text.contains("No longer in the memory: gone"), "{text}");
        assert!(!text.contains("[gone]"));
        // told once
        assert!(delta(&dir, None, "claude", "s1", &p).is_none());
        // another session keeps its own record
        assert!(
            delta(&dir, None, "claude", "s2", &p).is_none(),
            "no record: a baseline"
        );
    }

    #[test]
    fn user_notes_count_and_point_at_their_folder() {
        let d = tempfile::tempdir().unwrap();
        let (dir, user) = (d.path().join("ws"), d.path().join("_user"));
        paths::ensure(&dir).unwrap();
        record(&dir, Some(&user), "codex", "s1");
        put(&user, "prefers-tabs", "the user indents with tabs");
        let text = delta(&dir, Some(&user), "codex", "s1", &Prompts::builtin()).unwrap();
        assert!(
            text.contains(&format!("({}/notes/prefers-tabs.md)", user.display())),
            "{text}"
        );
    }
}
