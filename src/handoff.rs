//! Continuing a session in a new one, with everything the old one knew.
//!
//! On the same harness the harness forks the conversation itself, so the
//! new session has the old one's real memory and the old one keeps
//! running: Claude Code `--resume <id> --fork-session`, Codex `codex fork
//! <id>` (probed on Claude Code 2.1.284 and codex-cli 0.158.0;
//! `harness::Resume::Fork`). Antigravity has no fork, and another harness
//! cannot read a foreign conversation, so there the old session's whole
//! transcript is written to a Markdown file (`render`) and the new
//! session's first message asks it to read that file and carry on
//! (`opening_message`).

use crate::transcript::{self, Provider, TranscriptEvent};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// A tool's output past this many bytes is cut: the file it read or the
/// command it ran can be read or run again, and one large output would
/// crowd out the conversation itself.
pub const TOOL_OUTPUT_CAP: usize = 20_000;

/// What the new session continues from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub provider: Provider,
    /// The harness's own conversation id.
    pub conversation: String,
    /// The harness's transcript file, when the trace store knows it.
    pub transcript: Option<PathBuf>,
}

/// How the new session gets the old one's memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Continue {
    /// The harness forks this conversation (`harness::Resume::Fork`).
    Fork(String),
    /// The arguments that open the new session with the handover message.
    Transcript(Vec<String>),
}

/// `claude` → Claude, and so on for the store's provider names.
pub fn provider_of(name: &str) -> Option<Provider> {
    match name {
        "claude" => Some(Provider::Claude),
        "codex" => Some(Provider::Codex),
        "antigravity" | "agy" => Some(Provider::Antigravity),
        _ => None,
    }
}

fn provider_label(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "Claude Code",
        Provider::Codex => "Codex",
        Provider::Antigravity => "Antigravity",
    }
}

/// A tool call's arguments on one line.
fn args_text(args: &Value) -> String {
    match args {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A tool result's text: strings as they are, content blocks joined.
fn content_text(content: &Value) -> String {
    match content {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|i| match i.get("text").and_then(Value::as_str) {
                Some(t) => t.to_string(),
                None => i.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn cap(text: &str) -> String {
    if text.len() <= TOOL_OUTPUT_CAP {
        return text.to_string();
    }
    let mut end = TOOL_OUTPUT_CAP;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… ({} more bytes cut)", &text[..end], text.len() - end)
}

/// A fence longer than any run of backticks in `text`.
fn fence(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// The whole conversation of a transcript as Markdown: every prompt and
/// answer as written, every tool call with its arguments and its output
/// (outputs past `TOOL_OUTPUT_CAP` cut). Harness chatter written as user
/// lines, private reasoning and bookkeeping records are left out.
pub fn render(provider: Provider, transcript_text: &str) -> String {
    let mut out = String::new();
    let mut turns = 0usize;
    for line in transcript_text.lines() {
        for ev in transcript::parse_line(provider, line) {
            match ev {
                TranscriptEvent::User {
                    text, meta: false, ..
                } if !text.trim().is_empty() => {
                    turns += 1;
                    out.push_str(&format!("\n## User\n\n{}\n", text.trim_end()));
                }
                TranscriptEvent::Assistant { text, .. } if !text.trim().is_empty() => {
                    out.push_str(&format!("\n## Assistant\n\n{}\n", text.trim_end()));
                }
                TranscriptEvent::ToolUse { name, args, .. } => {
                    let a = args_text(&args);
                    let f = fence(&a);
                    out.push_str(&format!("\n### Tool call: {name}\n\n{f}\n{a}\n{f}\n"));
                }
                TranscriptEvent::ToolResult {
                    content, is_error, ..
                } => {
                    let t = cap(&content_text(&content));
                    let f = fence(&t);
                    let head = if is_error {
                        "Tool error"
                    } else {
                        "Tool output"
                    };
                    out.push_str(&format!("\n### {head}\n\n{f}\n{t}\n{f}\n"));
                }
                _ => {}
            }
        }
    }
    format!(
        "# Conversation handed over from {}\n\n{turns} prompt(s). Every prompt and answer is here as written; tool outputs over {} bytes are cut.\n{out}",
        provider_label(provider),
        TOOL_OUTPUT_CAP
    )
}

/// Writes the rendered transcript to `<dir>/<name>.md`.
pub fn write(dir: &Path, name: &str, markdown: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{name}.md"));
    std::fs::write(&path, markdown)?;
    Ok(path)
}

/// The new session's first message: read the old conversation, then
/// carry on from where it stopped.
pub fn opening_message(from: Provider, workspace: &Path, file: &Path) -> String {
    format!(
        "You are continuing a conversation that started in {} in {}. Its whole transcript is in {} (read it all first). Carry on from where it stopped, as the same assistant, with everything it knew; then say in one line what you will do next and wait for me.",
        provider_label(from),
        workspace.display(),
        file.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claude_transcript_renders_prompts_answers_and_tools() {
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"fix the failing test"},"timestamp":"2026-09-28T10:00:00Z"}"#,
            r#"{"type":"assistant","message":{"id":"m1","role":"assistant","model":"claude-opus-5-5","content":[{"type":"text","text":"Looking at it."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]},"timestamp":"2026-09-28T10:00:01Z"}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"1 failed"}]},"timestamp":"2026-09-28T10:00:02Z"}"#,
        ]
        .join("\n");
        let md = render(Provider::Claude, &lines);
        assert!(
            md.starts_with("# Conversation handed over from Claude Code"),
            "{md}"
        );
        assert!(md.contains("1 prompt(s)"), "{md}");
        assert!(md.contains("## User\n\nfix the failing test"), "{md}");
        assert!(md.contains("## Assistant\n\nLooking at it."), "{md}");
        assert!(md.contains("### Tool call: Bash"), "{md}");
        assert!(md.contains("cargo test"), "{md}");
        assert!(md.contains("### Tool output\n\n```\n1 failed"), "{md}");
    }

    #[test]
    fn large_outputs_are_cut_and_fences_outgrow_backticks() {
        let big = "x".repeat(TOOL_OUTPUT_CAP + 10);
        assert!(cap(&big).ends_with("(10 more bytes cut)"));
        assert_eq!(fence("a ``` b"), "````");
        assert_eq!(fence("plain"), "```");
    }

    #[test]
    fn the_opening_message_names_the_file() {
        let m = opening_message(
            Provider::Codex,
            Path::new("/w"),
            Path::new("/r/handoffs/x.md"),
        );
        assert!(m.contains("started in Codex in /w") && m.contains("/r/handoffs/x.md"));
    }
}
