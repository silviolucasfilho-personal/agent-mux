use agent_mux::langfuse::tail::{TailPoll, Tailer};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

fn append(path: &Path, text: &str) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(text.as_bytes()).unwrap();
    f.flush().unwrap();
}

#[test]
fn appends_across_polls_and_partial_lines_are_held() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    append(&path, "line one\nline two\n");
    let mut tailer = Tailer::new(&path);
    assert_eq!(
        tailer.poll(),
        TailPoll::Lines(vec!["line one".into(), "line two".into()])
    );
    assert_eq!(tailer.poll(), TailPoll::NoChange);
    // torn write: partial line held in the remainder buffer...
    append(&path, "line th");
    assert_eq!(tailer.poll(), TailPoll::NoChange);
    // ...surfaced once the newline lands
    append(&path, "ree\nline four\n");
    assert_eq!(
        tailer.poll(),
        TailPoll::Lines(vec!["line three".into(), "line four".into()])
    );
}

#[test]
fn late_appearing_file_is_a_normal_tick() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-yet.jsonl");
    let mut tailer = Tailer::new(&path);
    assert_eq!(tailer.poll(), TailPoll::NoChange);
    append(&path, "first\n");
    assert_eq!(tailer.poll(), TailPoll::Lines(vec!["first".into()]));
}

#[test]
fn watch_adopted_file_first_lines_are_emitted() {
    // A watch-adopted transcript pre-exists with its head already written;
    // Tailer::new reads from byte 0, so nothing is swallowed.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    append(&path, "head line 1\nhead line 2\n");
    let mut tailer = Tailer::new(&path);
    assert_eq!(
        tailer.poll(),
        TailPoll::Lines(vec!["head line 1".into(), "head line 2".into()])
    );
}

#[test]
fn prime_reads_history_and_tails_from_eof() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    append(&path, "old 1\nold 2\n");
    let (mut tailer, lines, truncated) = Tailer::prime(&path, 1024 * 1024).unwrap();
    assert_eq!(lines, vec!["old 1".to_string(), "old 2".to_string()]);
    assert!(!truncated);
    // nothing new yet
    assert_eq!(tailer.poll(), TailPoll::NoChange);
    append(&path, "new 1\n");
    assert_eq!(tailer.poll(), TailPoll::Lines(vec!["new 1".into()]));
}

#[test]
fn prime_backfill_cap_is_tail_biased_and_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    let mut content = String::new();
    for i in 0..100 {
        content.push_str(&format!("line number {i:04}\n"));
    }
    append(&path, &content);
    // cap smaller than the file: read only the tail, skip the cut partial
    // line, and report truncation
    let (mut tailer, lines, truncated) = Tailer::prime(&path, 100).unwrap();
    assert!(truncated);
    assert!(!lines.is_empty());
    assert!(lines.len() < 100);
    // every surfaced line is complete
    for line in &lines {
        assert!(line.starts_with("line number "), "torn line surfaced: {line:?}");
    }
    // and the newest line made it
    assert_eq!(lines.last().unwrap(), "line number 0099");
    append(&path, "after\n");
    assert_eq!(tailer.poll(), TailPoll::Lines(vec!["after".into()]));
}

#[test]
fn truncation_resets_the_tailer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    append(&path, "a\nb\nc\n");
    let mut tailer = Tailer::new(&path);
    assert!(matches!(tailer.poll(), TailPoll::Lines(_)));
    // rewrite the file shorter
    std::fs::write(&path, "x\n").unwrap();
    assert_eq!(tailer.poll(), TailPoll::Truncated);
    // after the reset, the next poll re-reads from byte 0
    assert_eq!(tailer.poll(), TailPoll::Lines(vec!["x".into()]));
}

#[test]
fn remainder_is_taken_once_at_finalize() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    append(&path, "complete\npartial without newline");
    let mut tailer = Tailer::new(&path);
    assert_eq!(tailer.poll(), TailPoll::Lines(vec!["complete".into()]));
    assert_eq!(
        tailer.take_remainder().as_deref(),
        Some("partial without newline")
    );
    assert_eq!(tailer.take_remainder(), None);
}
