//! `log.md`: one line per memory event, appended by agent-mux. The
//! plain-text record of who read and wrote the folder; best effort.

use super::paths;
use std::io::Write;
use std::path::Path;

/// `2026-10-10T12:30:00Z claude <launch> <op> <what>`.
pub fn append(dir: &Path, harness: &str, launch: Option<&str>, op: &str, what: &str) {
    if !dir.is_dir() {
        return;
    }
    let line = format!(
        "{} {} {} {} {}\n",
        super::now_rfc3339(),
        if harness.is_empty() { "-" } else { harness },
        launch.filter(|l| !l.is_empty()).unwrap_or("-"),
        op,
        what.replace('\n', " ")
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(paths::LOG))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lines_append() {
        let d = tempfile::tempdir().unwrap();
        super::append(d.path(), "claude", Some("l1"), "read", "MEMORY.md");
        super::append(d.path(), "", None, "rebuild", "MEMORY.md");
        let text = std::fs::read_to_string(d.path().join("log.md")).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert!(lines[0].ends_with(" claude l1 read MEMORY.md"), "{text}");
        assert!(lines[1].ends_with(" - - rebuild MEMORY.md"));
    }
}
