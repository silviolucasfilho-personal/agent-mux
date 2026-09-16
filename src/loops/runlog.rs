//! `loop-run-log.md`: one JSON line per completed run after the marker,
//! pruned to thirty days. Rust appends it after every run with the
//! store's token count; a line the skill already wrote for the same run is
//! replaced.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use time::OffsetDateTime;

pub const MARKER: &str = "<!-- Loop appends below this line -->";

const MAX_AGE: time::Duration = time::Duration::days(30);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub run_id: String,
    pub pattern: String,
    #[serde(default)]
    pub duration_s: u64,
    #[serde(default)]
    pub items_found: u64,
    #[serde(default)]
    pub actions_taken: u64,
    #[serde(default)]
    pub escalations: u64,
    #[serde(default)]
    pub tokens_estimate: u64,
    #[serde(default)]
    pub outcome: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl Entry {
    /// The compact one-line form written to the file.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

fn header(project: &str) -> String {
    format!(
        "# Loop Run Log — {project}\n\nOne JSON line per run, appended by agent-mux after the run. Entries older than 30 days are pruned.\n\n## Recent Runs\n\n{MARKER}\n"
    )
}

fn parse_line(line: &str) -> Option<Entry> {
    let t = line.trim();
    if !(t.starts_with('{') && t.ends_with('}')) {
        return None;
    }
    let e: Entry = serde_json::from_str(t).ok()?;
    (!e.run_id.is_empty() && !e.pattern.is_empty()).then_some(e)
}

/// Entries in file order. Only lines after the marker count when the
/// marker is present, so the fenced format example above it is skipped.
pub fn read(text: &str) -> Vec<Entry> {
    let body = match text.find(MARKER) {
        Some(i) => &text[i + MARKER.len()..],
        None => text,
    };
    body.lines().filter_map(parse_line).collect()
}

/// Appends `entry`, replacing a line with the same `run_id`, dropping
/// lines whose `run_id` is a timestamp older than thirty days before
/// `now` (other ids are kept), and returns how many entries remain.
pub fn append(path: &Path, entry: &Entry, now: OffsetDateTime) -> std::io::Result<usize> {
    let existing = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let project = path
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "workspace".into());
            header(&project)
        }
        Err(e) => return Err(e),
    };
    let (before, after) = match existing.find(MARKER) {
        Some(i) => (
            existing[..i + MARKER.len()].to_string(),
            existing[i + MARKER.len()..].to_string(),
        ),
        None => (format!("{}\n{MARKER}", existing.trim_end()), String::new()),
    };
    let cutoff = now - MAX_AGE;
    let mut kept: Vec<Entry> = read(&after)
        .into_iter()
        .filter(|e| e.run_id != entry.run_id)
        .filter(|e| match super::parse_timestamp(&e.run_id) {
            Some(t) => t >= cutoff,
            None => true,
        })
        .collect();
    kept.push(entry.clone());
    let lines: Vec<String> = kept.iter().map(Entry::to_line).collect();
    let text = format!("{}\n\n{}\n", before.trim_end(), lines.join("\n"));
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(kept.len())
}

/// The newest `limit` entries, newest first; empty when the file is
/// missing or unreadable.
pub fn recent(path: &Path, limit: usize) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut all = read(&text);
    all.reverse();
    all.truncate(limit);
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loops::{format_timestamp, parse_timestamp};

    fn entry(run_id: &str, pattern: &str) -> Entry {
        let mut extra = serde_json::Map::new();
        extra.insert("source".into(), Value::from("agent-mux"));
        Entry {
            run_id: run_id.into(),
            pattern: pattern.into(),
            duration_s: 41,
            items_found: 9,
            actions_taken: 1,
            escalations: 2,
            tokens_estimate: 48_210,
            outcome: "report-only".into(),
            extra,
        }
    }

    const TEMPLATE: &str = "# Loop Run Log — X\n\n## Format\n\n```json\n{\n  \"run_id\": \"2026-06-09T08:15:00Z\",\n  \"pattern\": \"daily-triage\"\n}\n```\n\n## Recent Runs\n\n<!-- Loop appends below this line -->\n";

    #[test]
    fn read_skips_the_fenced_example_and_takes_lines_after_the_marker() {
        let text = format!(
            "{TEMPLATE}{}\nnot json\n{{\"run_id\":\"\",\"pattern\":\"x\"}}\n",
            entry("2026-09-15T08:00:39Z", "daily-triage").to_line()
        );
        let entries = read(&text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].run_id, "2026-09-15T08:00:39Z");
        assert_eq!(entries[0].extra["source"], "agent-mux");
        // without a marker every JSON line counts
        assert_eq!(read(&entry("a", "b").to_line()).len(), 1);
    }

    #[test]
    fn line_shape_keeps_field_order() {
        let line = entry("2026-09-16T08:00:00Z", "daily-triage").to_line();
        assert!(line.starts_with(
            "{\"run_id\":\"2026-09-16T08:00:00Z\",\"pattern\":\"daily-triage\",\"duration_s\":41,"
        ));
        assert!(line.ends_with(",\"outcome\":\"report-only\",\"source\":\"agent-mux\"}"));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn append_creates_replaces_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loop-run-log.md");
        let now = parse_timestamp("2026-09-16T08:00:00Z").unwrap();
        assert_eq!(
            append(&path, &entry("2026-09-16T08:00:00Z", "daily-triage"), now).unwrap(),
            1
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Loop Run Log — "), "{text}");
        assert!(text.contains(MARKER));
        assert!(text.ends_with("\"source\":\"agent-mux\"}\n"));

        // an older ISO id within 30 days is kept, one past 30 days is dropped
        let old = format_timestamp(now - time::Duration::days(31));
        let fresh = format_timestamp(now - time::Duration::days(29));
        append(&path, &entry(&old, "daily-triage"), now).unwrap();
        append(&path, &entry(&fresh, "daily-triage"), now).unwrap();
        append(&path, &entry("run-1", "daily-triage"), now).unwrap();
        let kept = append(&path, &entry("2026-09-16T09:00:00Z", "daily-triage"), now).unwrap();
        let entries = read(&std::fs::read_to_string(&path).unwrap());
        let ids: Vec<&str> = entries.iter().map(|e| e.run_id.as_str()).collect();
        assert_eq!(kept, 4);
        assert!(!ids.contains(&old.as_str()), "{ids:?}");
        assert!(ids.contains(&fresh.as_str()));
        assert!(ids.contains(&"run-1"), "non-ISO ids survive");

        // the same run id is replaced, not duplicated
        let mut replaced = entry("2026-09-16T09:00:00Z", "daily-triage");
        replaced.tokens_estimate = 1;
        append(&path, &replaced, now).unwrap();
        let entries = read(&std::fs::read_to_string(&path).unwrap());
        let same: Vec<&Entry> = entries
            .iter()
            .filter(|e| e.run_id == "2026-09-16T09:00:00Z")
            .collect();
        assert_eq!(same.len(), 1);
        assert_eq!(same[0].tokens_estimate, 1);
        assert_eq!(recent(&path, 2)[0].run_id, "2026-09-16T09:00:00Z");
        assert_eq!(recent(&path, 2).len(), 2);

        // the header above the marker survives every rewrite
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(MARKER).count(), 1);
        assert!(text.starts_with("# Loop Run Log — "));
        // a file without a marker gets one
        let bare = dir.path().join("bare.md");
        std::fs::write(&bare, "# mine\n").unwrap();
        append(&bare, &entry("x", "y"), now).unwrap();
        let text = std::fs::read_to_string(&bare).unwrap();
        assert!(text.starts_with("# mine\n") && text.contains(MARKER));
        assert!(recent(&dir.path().join("none.md"), 5).is_empty());
    }
}
