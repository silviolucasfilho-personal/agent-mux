//! The hand-off brief: what a session continuing the work on another
//! harness starts from, instead of reading the whole earlier transcript
//! first (spec section 6, `docs/memory.md` "Hand-off brief").
//!
//! agent-mux writes the facts it knows at once (`facts`): the goal (the
//! first prompt), turns, cost, the files touched, the last answer and the
//! path of the rendered transcript. A headless fork of the old
//! conversation (`memory::save`, a `Job` with `brief`) then completes the
//! brief below them with the `[memory] brief` prompt (`complete`). The
//! new session gets the file through `$AGENT_MUX_MEMORY_BRIEF`, inside its
//! start text; if the fork takes longer than `[memory] brief_timeout_s`,
//! it starts from the facts alone.
//!
//! The brief is addressed: only the session `f` launches is given it. It
//! is never claimed by whoever starts next in the folder.

use super::paths;
use crate::tracing::analysis::model::SessionCard;
use std::path::{Path, PathBuf};

/// Seconds `f` waits for the brief when `[memory] brief_timeout_s` is unset.
pub const DEFAULT_TIMEOUT_S: u64 = 90;

/// The heading the model's part of the brief goes under.
const FROM_SESSION: &str = "## From the session";

/// Where the brief for continuing `conversation` of `harness` goes.
pub fn path_for(dir: &Path, harness: &str, conversation: &str) -> PathBuf {
    let stamp = super::now_rfc3339().replace([':', '-'], "");
    dir.join(paths::BRIEFS).join(format!(
        "{harness}-{}-{stamp}.md",
        super::note::slug(conversation)
            .chars()
            .take(12)
            .collect::<String>()
    ))
}

/// What agent-mux knows of the session being continued.
pub struct Facts<'a> {
    /// `Claude Code · agent-mux`.
    pub from: &'a str,
    pub workspace: &'a Path,
    pub card: Option<&'a SessionCard>,
    /// The rendered transcript, for a detail the brief leaves out.
    pub transcript: Option<&'a Path>,
}

fn one_line(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// The facts header, the whole brief until the model's part arrives.
pub fn facts(f: &Facts<'_>) -> String {
    let goal = f
        .card
        .and_then(|c| c.initial_goal.value.as_deref())
        .map(|g| one_line(g, 160));
    let title = goal.clone().unwrap_or_else(|| {
        f.workspace
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let mut out = format!("# Brief: {}\n\n", one_line(&title, 80));
    out.push_str(&format!(
        "Continues: {} in {}\n",
        f.from,
        f.workspace.display()
    ));
    if let Some(c) = f.card {
        let turns = c.completed_turns + c.open_turns;
        let mut facts = vec![format!("{turns} turns")];
        if let Some(cost) = c.total_cost_usd.filter(|c| *c > 0.0) {
            facts.push(crate::tracing::cli::fmt_cost(Some(cost)));
        }
        out.push_str(&format!("Session: {}\n", facts.join(" · ")));
        if let Some(g) = &goal {
            out.push_str(&format!("Goal (its first prompt): {g}\n"));
        }
        let files: Vec<String> = c
            .files_modified
            .iter()
            .filter_map(|e| e.value.as_ref())
            .map(|p| {
                Path::new(p)
                    .strip_prefix(f.workspace)
                    .map(|r| r.display().to_string())
                    .unwrap_or_else(|_| p.clone())
            })
            .take(15)
            .collect();
        if !files.is_empty() {
            out.push_str(&format!("Files touched: {}\n", files.join(", ")));
        }
        if let Some(last) = c
            .last_assistant_output
            .as_ref()
            .and_then(|e| e.value.as_ref())
        {
            out.push_str(&format!("Its last answer: {}\n", one_line(last, 400)));
        }
    }
    if let Some(t) = f.transcript {
        out.push_str(&format!(
            "Transcript: {} (the whole conversation; read it only for a detail)\n",
            t.display()
        ));
    }
    out
}

/// Writes the facts header as the brief.
pub fn write_facts(path: &Path, f: &Facts<'_>) -> std::io::Result<()> {
    paths::write_atomic(path, &facts(f))
}

/// Puts the model's part under the facts already in `path`.
pub fn complete(path: &Path, text: &str) -> std::io::Result<()> {
    let head = std::fs::read_to_string(path).unwrap_or_default();
    let head = match head.find(FROM_SESSION) {
        Some(i) => head[..i].trim_end().to_string(),
        None => head.trim_end().to_string(),
    };
    paths::write_atomic(
        path,
        &format!("{head}\n\n{FROM_SESSION}\n\n{}\n", text.trim()),
    )
}

/// Whether the model's part is in the brief.
pub fn is_complete(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| t.contains(FROM_SESSION))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_facts_come_first_and_the_session_completes_them() {
        let d = tempfile::tempdir().unwrap();
        let path = path_for(d.path(), "claude", "abcdef-1234-5678");
        assert!(path.starts_with(d.path().join("briefs")));
        let f = Facts {
            from: "Claude Code · proj",
            workspace: Path::new("/w/proj"),
            card: None,
            transcript: Some(Path::new("/rt/handoffs/claude-abcdef12.md")),
        };
        write_facts(&path, &f).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Brief: proj\n"), "{text}");
        assert!(text.contains("Continues: Claude Code · proj in /w/proj"));
        assert!(text.contains("Transcript: /rt/handoffs/claude-abcdef12.md"));
        assert!(!is_complete(&path));
        complete(&path, "Goal: ship it\nNext step: run the tests").unwrap();
        complete(&path, "Goal: ship it\nNext step: merge").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(is_complete(&path));
        assert_eq!(text.matches(FROM_SESSION).count(), 1, "{text}");
        assert!(text.ends_with("Next step: merge\n") && text.contains("Continues:"));
    }
}
