//! The one text a session is given at start: the hint, a hand-off brief
//! when one is addressed to it, and the index, each memory block inside
//! the trust fence. The `SessionStart` hook, Codex's developer
//! instructions and anything else that delivers memory use this text, so
//! every harness reads the same words.

use super::{fence, index, note, paths};
use crate::prompts::{Prompts, render_memory_hint};
use std::path::Path;

/// Claude Code takes 10,000 characters of `additionalContext` before it
/// spills to a file; stay under it.
pub const LIMIT: usize = 9_000;

/// The share of `LIMIT` a brief may take.
pub const BRIEF_LIMIT: usize = 4_000;

pub struct Inputs<'a> {
    /// The workspace's memory folder.
    pub dir: &'a Path,
    /// The user-level folder, listed after the workspace's notes.
    pub user_dir: Option<&'a Path>,
    /// A brief addressed to this session.
    pub brief: Option<&'a Path>,
    /// `/mux-memory` or `$mux-memory`.
    pub invocation: &'a str,
}

/// The hint alone, with paths: for a channel that must not carry the
/// memory text (Claude's system prompt is frozen per session).
pub fn hint(dir: &Path, invocation: &str, prompts: &Prompts) -> String {
    render_memory_hint(
        &prompts.memory_hint,
        &dir.to_string_lossy(),
        &paths::index_path(dir).to_string_lossy(),
        invocation,
    )
}

/// Expires old notes and rebuilds stale indexes, for the workspace and
/// the user folder. Errors are ignored: a start never fails on memory.
pub fn refresh(dir: &Path, user_dir: Option<&Path>, cap: usize, prompts: &Prompts) {
    let today = super::today();
    for d in std::iter::once(dir).chain(user_dir) {
        if d.is_dir() {
            index::expire(d, &today);
            let _ = index::ensure_fresh(d, cap, &prompts.memory_index_footer);
        }
    }
}

fn read_index(dir: &Path) -> Option<String> {
    std::fs::read_to_string(paths::index_path(dir)).ok()
}

/// Cuts `text` to at most `max` bytes on a line boundary; says so.
fn cut_lines(text: &str, max: usize, rest_at: &Path) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let tail = format!("… cut here; the rest is in {}\n", rest_at.display());
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        if out.len() + line.len() + tail.len() > max {
            break;
        }
        out.push_str(line);
    }
    out.push_str(&tail);
    out
}

/// The start text.
pub fn session_context(i: &Inputs<'_>, prompts: &Prompts) -> String {
    let mut out = hint(i.dir, i.invocation, prompts);
    out.push_str("\n\n");
    if let Some(brief) = i.brief
        && let Ok(text) = std::fs::read_to_string(brief)
    {
        let body = cut_lines(&text, BRIEF_LIMIT, brief);
        out.push_str(&fence::fence(&prompts.memory_notice, &body));
        out.push_str(
            &prompts
                .memory_brief_footer
                .replace("{brief}", &brief.to_string_lossy()),
        );
        out.push_str("\n\n");
    }
    let mut body = format!("Workspace memory, {}:\n\n", i.dir.display());
    body.push_str(
        &read_index(i.dir).unwrap_or_else(|| format!("{}\n\nNo notes yet.\n", index::TITLE)),
    );
    if let Some(user) = i.user_dir
        && !note::files(user).is_empty()
        && let Some(text) = read_index(user)
    {
        body.push_str(&format!(
            "\nWhat holds in every workspace, {}:\n\n{text}",
            user.display()
        ));
    }
    // the fence's own lines take the notice plus the two markers
    let overhead = prompts.memory_notice.len() + fence::START.len() + fence::END.len() + 8;
    let budget = LIMIT.saturating_sub(out.len() + overhead);
    let body = cut_lines(&body, budget, &paths::index_path(i.dir));
    out.push_str(&fence::fence(&prompts.memory_notice, &body));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::note::{Confidence, Kind, Note, Source};

    fn add(dir: &Path, name: &str, desc: &str) {
        note::write(
            dir,
            &Note {
                name: name.into(),
                description: desc.into(),
                kind: Kind::Project,
                source: Source {
                    harness: "claude".into(),
                    ..Default::default()
                },
                confidence: Confidence::Stated,
                pinned: false,
                expires_at: None,
                body: "b".into(),
            },
        )
        .unwrap();
    }

    #[test]
    fn the_start_text_has_the_hint_the_fence_and_every_index_line() {
        let d = tempfile::tempdir().unwrap();
        let ws = d.path().join("ws");
        let user = d.path().join("_user");
        paths::ensure(&ws).unwrap();
        paths::ensure(&user).unwrap();
        add(&ws, "one", "first fact");
        add(&user, "me", "the user likes tables");
        let p = Prompts::builtin();
        refresh(&ws, Some(&user), 60, &p);
        let brief = d.path().join("brief.md");
        std::fs::write(&brief, "# Brief\nNext step: ship").unwrap();
        let text = session_context(
            &Inputs {
                dir: &ws,
                user_dir: Some(&user),
                brief: Some(&brief),
                invocation: "/mux-memory",
            },
            &p,
        );
        assert!(
            text.starts_with("You have a memory for this workspace at "),
            "{text}"
        );
        assert!(text.contains("/mux-memory"));
        assert_eq!(text.matches(fence::START).count(), 2);
        assert!(text.contains("[one](notes/one.md) — first fact"));
        assert!(text.contains("[me](notes/me.md) — the user likes tables"));
        assert!(text.contains("Next step: ship"));
        assert!(text.find("Next step").unwrap() < text.find("[one]").unwrap());
    }

    #[test]
    fn a_huge_index_is_cut_under_the_limit() {
        let d = tempfile::tempdir().unwrap();
        paths::ensure(d.path()).unwrap();
        for i in 0..500 {
            add(
                d.path(),
                &format!("note-{i:03}"),
                &"a long description ".repeat(5),
            );
        }
        let p = Prompts::builtin();
        refresh(d.path(), None, 1000, &p);
        let text = session_context(
            &Inputs {
                dir: d.path(),
                user_dir: None,
                brief: None,
                invocation: "$mux-memory",
            },
            &p,
        );
        assert!(text.len() <= LIMIT, "{}", text.len());
        assert!(text.contains("… cut here; the rest is in"));
        assert!(text.trim_end().ends_with(fence::END));
    }
}
