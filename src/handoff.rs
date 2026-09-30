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
/// command it ran can be read or run again, and outputs would otherwise
/// crowd out the conversation itself. Measured on a real 5.5 MB Claude
/// transcript: prompts and answers took 35 kB of the rendering, tool
/// arguments 520 kB and tool outputs 340 kB.
pub const TOOL_OUTPUT_CAP: usize = 4_000;

/// A tool call's arguments past this many bytes are cut (a Write or an
/// Edit carries whole files; the files are in the workspace).
pub const TOOL_ARGS_CAP: usize = 2_000;

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
    cap_at(text, TOOL_OUTPUT_CAP)
}

fn cap_at(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
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

/// The rendering stays near this size (about 50k tokens) so the new
/// session can read all of it: prompts and answers always go in whole;
/// the newest tool calls and outputs go in while the budget lasts, and the
/// older ones shrink to one line each.
pub const BUDGET: usize = 200_000;

/// One piece of the conversation, with its one-line form for when the
/// budget has run out.
struct Block {
    full: String,
    /// `None` for prompts and answers, which are never shortened.
    short: Option<String>,
}

fn one_line(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

/// The whole conversation of a transcript as Markdown: every prompt and
/// answer as written, and the tool calls with their arguments and
/// outputs, each cut past `TOOL_ARGS_CAP` / `TOOL_OUTPUT_CAP`, the older
/// ones down to a line when the rendering would pass `BUDGET`. Harness
/// chatter written as user lines, private reasoning and bookkeeping
/// records are left out.
pub fn render(provider: Provider, transcript_text: &str) -> String {
    let mut blocks: Vec<Block> = Vec::new();
    let mut turns = 0usize;
    for line in transcript_text.lines() {
        for ev in transcript::parse_line(provider, line) {
            match ev {
                TranscriptEvent::User {
                    text, meta: false, ..
                } if !text.trim().is_empty() => {
                    turns += 1;
                    blocks.push(Block {
                        full: format!("\n## User\n\n{}\n", text.trim_end()),
                        short: None,
                    });
                }
                TranscriptEvent::Assistant { text, .. } if !text.trim().is_empty() => {
                    blocks.push(Block {
                        full: format!("\n## Assistant\n\n{}\n", text.trim_end()),
                        short: None,
                    });
                }
                TranscriptEvent::ToolUse { name, args, .. } => {
                    let raw = args_text(&args);
                    let a = cap_at(&raw, TOOL_ARGS_CAP);
                    let f = fence(&a);
                    blocks.push(Block {
                        full: format!("\n### Tool call: {name}\n\n{f}\n{a}\n{f}\n"),
                        short: Some(format!("\n- tool call {name}: {}\n", one_line(&raw, 100))),
                    });
                }
                TranscriptEvent::ToolResult {
                    content, is_error, ..
                } => {
                    let raw = content_text(&content);
                    let t = cap(&raw);
                    let f = fence(&t);
                    let head = if is_error {
                        "Tool error"
                    } else {
                        "Tool output"
                    };
                    blocks.push(Block {
                        full: format!("\n### {head}\n\n{f}\n{t}\n{f}\n"),
                        short: Some(format!(
                            "\n- {} ({} bytes, left out)\n",
                            head.to_lowercase(),
                            raw.len()
                        )),
                    });
                }
                _ => {}
            }
        }
    }
    // prompts and answers first; then the newest tool blocks while they fit
    let fixed: usize = blocks
        .iter()
        .map(|b| if b.short.is_none() { b.full.len() } else { 0 })
        .sum();
    let mut room = BUDGET.saturating_sub(fixed);
    let mut keep = vec![true; blocks.len()];
    for (i, b) in blocks.iter().enumerate().rev() {
        if b.short.is_some() {
            if b.full.len() <= room {
                room -= b.full.len();
            } else {
                keep[i] = false;
                room = 0;
            }
        }
    }
    let shortened = keep.iter().filter(|k| !**k).count();
    let mut out = String::new();
    for (b, k) in blocks.iter().zip(keep) {
        match (&b.short, k) {
            (Some(s), false) => out.push_str(s),
            _ => out.push_str(&b.full),
        }
    }
    let note = if shortened > 0 {
        format!(
            " To keep it readable in one go, {shortened} older tool call(s) and output(s) are one line each."
        )
    } else {
        String::new()
    };
    format!(
        "# Conversation handed over from {}\n\n{turns} prompt(s). Every prompt and answer is here as written; tool arguments over {} bytes and tool outputs over {} bytes are cut (the files and commands they name are in the workspace).{note}\n{out}",
        provider_label(provider),
        TOOL_ARGS_CAP,
        TOOL_OUTPUT_CAP
    )
}

/// Codex's rollouts: `$CODEX_HOME/sessions`, else `~/.codex/sessions`.
pub fn codex_sessions_dir() -> Option<PathBuf> {
    match std::env::var_os("CODEX_HOME") {
        Some(h) => Some(PathBuf::from(h).join("sessions")),
        None => Some(
            crate::skill::install::home_dir()
                .join(".codex")
                .join("sessions"),
        ),
    }
}

/// Every rollout file under `root` (`YYYY/MM/DD/rollout-*.jsonl`).
fn codex_rollouts(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![(root.to_path_buf(), 0)];
    while let Some((d, depth)) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && depth < 4 {
                dirs.push((p, depth + 1));
            } else if p.extension().is_some_and(|x| x == "jsonl")
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("rollout-"))
            {
                out.push(p);
            }
        }
    }
    out
}

/// A rollout's `session_meta`: its id, its folder and the conversation it
/// was forked from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexMeta {
    pub id: String,
    pub cwd: Option<PathBuf>,
    pub forked_from: Option<String>,
}

pub fn codex_meta(path: &Path) -> Option<CodexMeta> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(f)
        .lines()
        .take(5)
        .map_while(Result::ok)
    {
        let v: Value = serde_json::from_str(&line).ok()?;
        if v.get("type").and_then(Value::as_str) == Some("session_meta") {
            let p = v.get("payload")?;
            let s = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
            return Some(CodexMeta {
                id: s("id").or_else(|| s("session_id"))?,
                cwd: s("cwd").map(PathBuf::from),
                forked_from: s("forked_from_id"),
            });
        }
    }
    None
}

/// The rollout of conversation `id` under `root`.
pub fn codex_rollout(root: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("-{id}.jsonl");
    codex_rollouts(root)
        .into_iter()
        .find(|p| p.to_string_lossy().ends_with(&suffix))
}

/// A Codex conversation's whole text: `codex fork` starts a new rollout
/// that holds only what came after the fork and names its parent in
/// `forked_from_id` (probed on codex-cli 0.158.0), so the parents come
/// first, oldest first.
pub fn codex_history(root: &Path, path: &Path) -> std::io::Result<String> {
    let mut chain = vec![path.to_path_buf()];
    let mut at = path.to_path_buf();
    while chain.len() < 16 {
        let Some(parent) = codex_meta(&at)
            .and_then(|m| m.forked_from)
            .and_then(|id| codex_rollout(root, &id))
        else {
            break;
        };
        chain.push(parent.clone());
        at = parent;
    }
    let mut text = String::new();
    for p in chain.iter().rev() {
        text.push_str(&std::fs::read_to_string(p)?);
        text.push('\n');
    }
    Ok(text)
}

/// The transcript file of conversation `id` of `provider`, looked up in
/// the harness's own folders (no trace store needed).
pub fn transcript_by_id(provider: Provider, id: &str) -> Option<PathBuf> {
    match provider {
        Provider::Claude => {
            let projects = crate::history::default_claude_dir()?.join("projects");
            std::fs::read_dir(projects)
                .ok()?
                .flatten()
                .map(|e| e.path().join(format!("{id}.jsonl")))
                .find(|p| p.is_file())
        }
        Provider::Codex => codex_rollout(&codex_sessions_dir()?, id),
        Provider::Antigravity => {
            let p = crate::history::default_antigravity_dir()?
                .join(id)
                .join(".system_generated")
                .join("logs")
                .join("transcript.jsonl");
            p.is_file().then_some(p)
        }
    }
}

/// The conversation a session with no trace had: the one transcript of
/// `provider` in `dir` written since `since`. Two or more are ambiguous
/// (another session in the same folder), and none means it has not had a
/// turn yet; both are errors that say so.
pub fn find_on_disk(
    provider: Provider,
    dir: &Path,
    since: std::time::SystemTime,
) -> Result<Source, String> {
    find_in(
        &Roots {
            claude: None,
            antigravity: None,
            codex: codex_sessions_dir(),
        },
        provider,
        dir,
        since,
    )
}

/// Where each harness keeps its transcripts; `None` is the default.
#[derive(Debug, Default)]
pub struct Roots {
    /// The `.claude` folder.
    pub claude: Option<PathBuf>,
    /// Antigravity's `brain` folder.
    pub antigravity: Option<PathBuf>,
    /// Codex's `sessions` folder.
    pub codex: Option<PathBuf>,
}

/// `find_on_disk` over explicit folders.
pub fn find_in(
    roots: &Roots,
    provider: Provider,
    dir: &Path,
    since: std::time::SystemTime,
) -> Result<Source, String> {
    let recent = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= since)
    };
    let found: Vec<(String, PathBuf)> = match provider {
        Provider::Claude => {
            crate::history::discover_claude_sessions(roots.claude.as_deref(), Some(dir), false)
                .into_iter()
                .filter(|s| recent(&s.file_path))
                .map(|s| (s.session_id, s.file_path))
                .collect()
        }
        Provider::Antigravity => crate::history::discover_antigravity_sessions(
            roots.antigravity.as_deref(),
            Some(dir),
            false,
        )
        .into_iter()
        .filter(|s| recent(&s.file_path))
        .map(|s| (s.session_id, s.file_path))
        .collect(),
        Provider::Codex => roots
            .codex
            .as_deref()
            .map(codex_rollouts)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| recent(p))
            .filter_map(|p| {
                let m = codex_meta(&p)?;
                (m.cwd.as_deref() == Some(dir)).then_some((m.id, p))
            })
            .collect(),
    };
    match found.len() {
        0 => Err(format!(
            "no {} conversation in {} since this session started: has it had a turn?",
            provider_label(provider),
            dir.display()
        )),
        1 => {
            let (id, path) = found.into_iter().next().unwrap_or_default();
            Ok(Source {
                provider,
                conversation: id,
                transcript: Some(path),
            })
        }
        n => Err(format!(
            "{n} {} conversations in {} since this session started, and tracing is off: turn it on (t) so agent-mux knows which is this one",
            provider_label(provider),
            dir.display()
        )),
    }
}

/// The text to render for `source`: a Codex conversation with the
/// rollouts it was forked from.
pub fn source_text(source: &Source) -> Result<String, String> {
    let path = source
        .transcript
        .as_ref()
        .ok_or("no transcript file for this conversation")?;
    let text = match (source.provider, codex_sessions_dir()) {
        (Provider::Codex, Some(root)) => codex_history(&root, path),
        _ => std::fs::read_to_string(path),
    };
    text.map_err(|e| format!("{}: {e}", path.display()))
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
    fn past_the_budget_older_tool_blocks_shrink_and_prompts_stay() {
        let mut lines = Vec::new();
        for i in 0..200 {
            lines.push(format!(
                r#"{{"type":"user","message":{{"role":"user","content":"prompt {i}"}}}}"#
            ));
            lines.push(format!(
                r#"{{"type":"assistant","message":{{"id":"m{i}","role":"assistant","content":[{{"type":"tool_use","id":"t{i}","name":"Read","input":{{"file_path":"/f{i}"}}}}]}}}}"#
            ));
            lines.push(format!(
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t{i}","content":"{}"}}]}}}}"#,
                "y".repeat(3_000)
            ));
        }
        let md = render(Provider::Claude, &lines.join("\n"));
        assert!(md.len() <= BUDGET + 20_000, "{}", md.len());
        assert!(
            md.contains("prompt 0") && md.contains("prompt 199"),
            "every prompt stays"
        );
        assert!(
            md.contains("- tool output (3000 bytes, left out)"),
            "old outputs shrink"
        );
        let last = md.rfind("### Tool output").unwrap();
        assert!(
            last > md.find("prompt 199").unwrap(),
            "the newest output stays whole"
        );
        assert!(md.contains("older tool call(s) and output(s) are one line each"));
    }

    #[test]
    fn a_forked_codex_rollout_brings_its_parents() {
        let t = tempfile::tempdir().unwrap();
        let day = t.path().join("2026/09/30");
        std::fs::create_dir_all(&day).unwrap();
        let meta = |id: &str, from: Option<&str>| {
            let f = from
                .map(|f| format!(r#","forked_from_id":"{f}""#))
                .unwrap_or_default();
            format!(r#"{{"type":"session_meta","payload":{{"id":"{id}","cwd":"/w"{f}}}}}"#)
        };
        std::fs::write(
            day.join("rollout-a-p1.jsonl"),
            format!("{}\nPARENT\n", meta("p1", None)),
        )
        .unwrap();
        let child = day.join("rollout-b-c1.jsonl");
        std::fs::write(&child, format!("{}\nCHILD\n", meta("c1", Some("p1")))).unwrap();
        let m = codex_meta(&child).unwrap();
        assert_eq!(
            (m.id.as_str(), m.forked_from.as_deref()),
            ("c1", Some("p1"))
        );
        assert_eq!(m.cwd.as_deref(), Some(Path::new("/w")));
        let text = codex_history(t.path(), &child).unwrap();
        let (p, c) = (text.find("PARENT").unwrap(), text.find("CHILD").unwrap());
        assert!(p < c, "the parent comes first");
        assert_eq!(
            codex_rollout(t.path(), "p1").unwrap(),
            day.join("rollout-a-p1.jsonl")
        );
    }

    #[test]
    fn with_tracing_off_the_one_new_conversation_in_the_folder_is_found() {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let before = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
        let roots = Roots {
            claude: Some(t.path().join(".claude")),
            antigravity: Some(t.path().join("brain")),
            codex: Some(t.path().join("codex")),
        };
        // nothing yet: it has not had a turn
        let e = find_in(&roots, Provider::Claude, &ws, before).unwrap_err();
        assert!(e.contains("has it had a turn"), "{e}");
        // one Claude transcript in the folder's project
        let project = t
            .path()
            .join(".claude/projects")
            .join(crate::history::project_slug(&ws));
        std::fs::create_dir_all(&project).unwrap();
        let line = r#"{"type":"user","message":{"role":"user","content":"hello"},"timestamp":"2026-09-30T10:00:00Z"}"#;
        std::fs::write(project.join("abc-1.jsonl"), format!("{line}\n")).unwrap();
        let src = find_in(&roots, Provider::Claude, &ws, before).unwrap();
        assert_eq!(src.conversation, "abc-1");
        // a second one in the same folder: ambiguous without tracing
        std::fs::write(project.join("abc-2.jsonl"), format!("{line}\n")).unwrap();
        let e = find_in(&roots, Provider::Claude, &ws, before).unwrap_err();
        assert!(e.contains("2 Claude Code conversations"), "{e}");
        // a Codex rollout in another folder is not this session's
        let day = t.path().join("codex/2026/09/30");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            day.join("rollout-x-c9.jsonl"),
            r#"{"type":"session_meta","payload":{"id":"c9","cwd":"/elsewhere"}}"#,
        )
        .unwrap();
        assert!(find_in(&roots, Provider::Codex, &ws, before).is_err());
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
