//! History for every harness: where each one keeps its past sessions, how
//! a session is summarised for the list, and how its log is read for the
//! viewer. Claude Code and Antigravity live in `history.rs`; this module
//! adds Codex, Muse, OpenCode, Grok, Pi and Cursor.
//!
//! Storage (probed on the installed CLIs):
//! - Codex: `<codex>/sessions/YYYY/MM/DD/rollout-*.jsonl` (`$CODEX_HOME`,
//!   else `~/.codex`), parsed by `transcript::parse_codex_line`.
//! - Muse: `<data>/sessions/YYYY/MM/DD/<id>/session.jsonl`
//!   (`tracing::correlate::muse_data_dir`), `transcript::parse_muse_line`.
//! - OpenCode: rows of `session_v2` / `session_message` in its SQLite
//!   database (`tracing::opencode_db`).
//! - Grok: `<grok>/sessions/<url-encoded cwd>/<id>/{summary.json,
//!   chat_history.jsonl}` (`$GROK_HOME`, else `~/.grok`).
//! - Pi: `<pi>/sessions/<slug>/<timestamp>_<id>.jsonl`
//!   (`$PI_CODING_AGENT_DIR`, else `~/.pi/agent`), a `session` header line
//!   then `message` lines.
//! - Cursor: `<cursor>/projects/<slug>/agent-transcripts/<id>/<id>.jsonl`
//!   (`~/.cursor`), `transcript::parse_cursor_line`; the folder a chat ran
//!   in is in `<cursor>/chats/<hash>/<id>/meta.json`.

use crate::history::{
    AgentProvider, LogEntry, SessionSummary, discover_antigravity_sessions,
    discover_claude_sessions, format_claude_tool_args, format_iso_time, format_system_time,
    project_slug,
};
use crate::transcript::TranscriptEvent;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The storage root of each harness. `None` skips that harness.
#[derive(Debug, Clone, Default)]
pub struct HistoryDirs {
    pub claude: Option<PathBuf>,
    /// The Antigravity `brain` directory.
    pub antigravity: Option<PathBuf>,
    /// The Codex home (the folder holding `sessions/`).
    pub codex: Option<PathBuf>,
    /// Muse's data directory (the folder holding `sessions/`).
    pub muse: Option<PathBuf>,
    /// OpenCode's SQLite database file.
    pub opencode_db: Option<PathBuf>,
    /// The Grok home (the folder holding `sessions/`).
    pub grok: Option<PathBuf>,
    /// Pi's agent directory (the folder holding `sessions/`).
    pub pi: Option<PathBuf>,
    /// Cursor's directory (the folder holding `projects/` and `chats/`).
    pub cursor: Option<PathBuf>,
}

impl HistoryDirs {
    /// The default locations, honouring each harness's own environment
    /// override.
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        let env_dir = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let under = |rel: &[&str]| {
            home.as_ref().map(|h| {
                let mut p = h.clone();
                for part in rel {
                    p.push(part);
                }
                p
            })
        };
        HistoryDirs {
            claude: crate::history::default_claude_dir(),
            antigravity: crate::history::default_antigravity_dir(),
            codex: env_dir("CODEX_HOME").or_else(|| under(&[".codex"])),
            muse: home
                .as_ref()
                .map(|h| crate::tracing::correlate::muse_data_dir(h)),
            opencode_db: home
                .as_ref()
                .map(|h| crate::tracing::opencode_db::default_db_path(h)),
            grok: env_dir("GROK_HOME").or_else(|| under(&[".grok"])),
            pi: env_dir("PI_CODING_AGENT_DIR").or_else(|| under(&[".pi", "agent"])),
            cursor: under(&[".cursor"]),
        }
    }
}

/// Past sessions of every harness, newest first. With `all_projects`
/// false only those that ran in `current_dir` are kept.
pub fn discover_all(
    dirs: &HistoryDirs,
    current_dir: Option<&Path>,
    all_projects: bool,
) -> Vec<SessionSummary> {
    // `None` here means "skip", where the two legacy scanners read
    // "default location".
    let mut out = Vec::new();
    if let Some(dir) = &dirs.claude {
        out.extend(discover_claude_sessions(
            Some(dir),
            current_dir,
            all_projects,
        ));
    }
    if let Some(dir) = &dirs.antigravity {
        out.extend(discover_antigravity_sessions(
            Some(dir),
            current_dir,
            all_projects,
        ));
    }
    let want = Want::new(current_dir, all_projects);
    if let Some(dir) = &dirs.codex {
        out.extend(discover_codex(dir, &want));
    }
    if let Some(dir) = &dirs.muse {
        out.extend(discover_muse(dir, &want));
    }
    if let Some(db) = &dirs.opencode_db {
        out.extend(discover_opencode(db, &want));
    }
    if let Some(dir) = &dirs.grok {
        out.extend(discover_grok(dir, &want));
    }
    if let Some(dir) = &dirs.pi {
        out.extend(discover_pi(dir, &want));
    }
    if let Some(dir) = &dirs.cursor {
        out.extend(discover_cursor(dir, &want));
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.modified));
    out
}

/// Reads a session's log whatever harness wrote it.
pub fn load_log(summary: &SessionSummary) -> std::io::Result<Vec<LogEntry>> {
    match summary.provider {
        AgentProvider::Claude | AgentProvider::Antigravity => {
            crate::history::load_session_log(&summary.file_path)
        }
        AgentProvider::Codex => load_jsonl_events(
            &summary.file_path,
            AgentProvider::Codex,
            crate::transcript::parse_codex_line,
        ),
        AgentProvider::Muse => load_jsonl_events(
            &summary.file_path,
            AgentProvider::Muse,
            crate::transcript::parse_muse_line,
        ),
        AgentProvider::OpenCode => Ok(load_opencode(&summary.file_path, &summary.session_id)),
        AgentProvider::Grok => load_grok(&summary.file_path),
        AgentProvider::Pi => load_pi(&summary.file_path),
        AgentProvider::Cursor => load_jsonl_events(
            &summary.file_path,
            AgentProvider::Cursor,
            crate::transcript::parse_cursor_line,
        ),
    }
}

/// Which folder's sessions are wanted.
struct Want {
    dir: Option<PathBuf>,
    all: bool,
}

impl Want {
    fn new(current_dir: Option<&Path>, all: bool) -> Self {
        Want {
            dir: current_dir.map(|d| d.canonicalize().unwrap_or_else(|_| d.to_path_buf())),
            all: all || current_dir.is_none(),
        }
    }

    fn accepts(&self, cwd: Option<&Path>) -> bool {
        if self.all {
            return true;
        }
        let (Some(want), Some(cwd)) = (&self.dir, cwd) else {
            return false;
        };
        let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        &cwd == want
    }
}

/// One line, at most 40 characters, the way the Claude list titles are.
fn short_title(prompt: &str, fallback_id: &str) -> String {
    let first_line = prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let first_line = first_line.trim();
    let title = if first_line.chars().count() > 40 {
        format!("{}...", first_line.chars().take(37).collect::<String>())
    } else {
        first_line.to_string()
    };
    if title.is_empty() {
        format!(
            "Session {}",
            fallback_id.chars().take(8).collect::<String>()
        )
    } else {
        title
    }
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn slug_of(cwd: Option<&Path>) -> String {
    cwd.map(project_slug).unwrap_or_default()
}

fn ms_to_time(ms: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64)
}

fn iso_from_ms(ms: i64) -> Option<String> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()?
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

// ---------------------------------------------------------------------------
// Codex and Muse: JSONL transcripts the shared parsers understand.
// ---------------------------------------------------------------------------

fn discover_codex(codex_home: &Path, want: &Want) -> Vec<SessionSummary> {
    let mut files = Vec::new();
    walk_files(&codex_home.join("sessions"), 0, 6, &mut |p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with("rollout-") && name.ends_with(".jsonl") {
            files.push(p.to_path_buf());
        }
    });
    files
        .iter()
        .filter_map(|f| summarize_events(f, AgentProvider::Codex, None, None, want))
        .collect()
}

fn discover_muse(data_dir: &Path, want: &Want) -> Vec<SessionSummary> {
    let mut files = Vec::new();
    walk_files(&data_dir.join("sessions"), 0, 5, &mut |p| {
        if p.file_name().and_then(|n| n.to_str()) == Some("session.jsonl")
            && !crate::tracing::correlate::is_muse_reminder_child(p)
        {
            files.push(p.to_path_buf());
        }
    });
    files
        .iter()
        .filter_map(|f| {
            let id = f
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned());
            summarize_events(f, AgentProvider::Muse, id, None, want)
        })
        .collect()
}

/// Cursor's chats: the transcript names the chat, `meta.json` its folder
/// (the transcript lines carry neither).
fn discover_cursor(cursor_dir: &Path, want: &Want) -> Vec<SessionSummary> {
    crate::tracing::correlate::cursor_transcripts(cursor_dir)
        .into_iter()
        .filter_map(|(id, path)| {
            let cwd =
                crate::tracing::correlate::cursor_chat_cwd(cursor_dir, &id).map(PathBuf::from);
            summarize_events(&path, AgentProvider::Cursor, Some(id), cwd, want)
        })
        .collect()
}

fn walk_files(dir: &Path, depth: usize, max: usize, visit: &mut dyn FnMut(&Path)) {
    if depth > max {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_files(&path, depth + 1, max, visit);
        } else {
            visit(&path);
        }
    }
}

fn parser_for(provider: AgentProvider) -> fn(&str) -> Vec<TranscriptEvent> {
    match provider {
        AgentProvider::Muse => crate::transcript::parse_muse_line,
        AgentProvider::Cursor => crate::transcript::parse_cursor_line,
        _ => crate::transcript::parse_codex_line,
    }
}

fn summarize_events(
    path: &Path,
    provider: AgentProvider,
    id_hint: Option<String>,
    cwd_hint: Option<PathBuf>,
    want: &Want,
) -> Option<SessionSummary> {
    let parse = parser_for(provider);
    let reader = BufReader::new(File::open(path).ok()?);
    let mut session_id = id_hint;
    let mut cwd: Option<PathBuf> = cwd_hint;
    let mut first_prompt: Option<String> = None;
    let mut first_ts: Option<String> = None;
    let mut turns = 0usize;
    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        for event in parse(&line) {
            match event {
                TranscriptEvent::SessionMeta {
                    session_id: sid,
                    cwd: c,
                    ts,
                    ..
                } => {
                    if let Some(sid) = sid {
                        session_id.get_or_insert(sid);
                    }
                    if cwd.is_none() {
                        cwd = c.map(PathBuf::from);
                    }
                    if first_ts.is_none() {
                        first_ts = ts;
                    }
                }
                TranscriptEvent::User {
                    text,
                    meta: false,
                    ts,
                    ..
                } => {
                    if first_prompt.is_none() && !text.trim().is_empty() {
                        first_prompt = Some(text);
                    }
                    if first_ts.is_none() {
                        first_ts = ts;
                    }
                }
                TranscriptEvent::Assistant { .. } => turns += 1,
                _ => {}
            }
        }
    }
    if !want.accepts(cwd.as_deref()) {
        return None;
    }
    let session_id =
        session_id.or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))?;
    let modified = mtime(path);
    Some(SessionSummary {
        title: short_title(first_prompt.as_deref().unwrap_or(""), &session_id),
        session_id,
        modified,
        file_path: path.to_path_buf(),
        turn_count: turns,
        project_slug: slug_of(cwd.as_deref()),
        timestamp_str: first_ts
            .as_deref()
            .and_then(format_iso_time)
            .unwrap_or_else(|| format_system_time(modified)),
        provider,
        cwd,
    })
}

fn load_jsonl_events(
    path: &Path,
    provider: AgentProvider,
    parse: fn(&str) -> Vec<TranscriptEvent>,
) -> std::io::Result<Vec<LogEntry>> {
    let reader = BufReader::new(File::open(path)?);
    let mut events = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        events.extend(parse(&line));
    }
    Ok(entries_from_events(provider, events))
}

/// The display form of a stream of transcript events: thinking that has no
/// message of its own rides the next assistant entry.
fn entries_from_events(provider: AgentProvider, events: Vec<TranscriptEvent>) -> Vec<LogEntry> {
    let mut out = Vec::new();
    let mut pending_thinking: Option<String> = None;
    for ev in events {
        match ev {
            TranscriptEvent::User {
                text,
                meta: false,
                ts,
                ..
            } => out.push(LogEntry::User {
                text,
                timestamp: ts,
            }),
            TranscriptEvent::Thinking { text, .. } => {
                if !text.trim().is_empty() {
                    pending_thinking = Some(match pending_thinking.take() {
                        Some(prev) => format!("{prev}\n{text}"),
                        None => text,
                    });
                }
            }
            TranscriptEvent::Assistant {
                text,
                model,
                thinking,
                ts,
                ..
            } => {
                let thinking = thinking
                    .filter(|t| !t.trim().is_empty())
                    .or_else(|| pending_thinking.take());
                pending_thinking = None;
                out.push(LogEntry::Assistant {
                    provider,
                    text,
                    model,
                    thinking,
                    timestamp: ts,
                });
            }
            TranscriptEvent::ToolUse {
                id, name, args, ts, ..
            } => out.push(LogEntry::ToolUse {
                id,
                name,
                input: format_claude_tool_args(&args),
                timestamp: ts,
            }),
            TranscriptEvent::ToolResult {
                id,
                content,
                is_error,
                ts,
                ..
            } => out.push(LogEntry::ToolResult {
                id,
                content: value_text(&content),
                is_error,
                timestamp: ts,
            }),
            _ => {}
        }
    }
    out
}

/// A tool result's content as text: a string, or the text parts of an array.
fn value_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| {
                i.get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| i.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// OpenCode: the SQLite database.
// ---------------------------------------------------------------------------

fn discover_opencode(db: &Path, want: &Want) -> Vec<SessionSummary> {
    crate::tracing::opencode_db::list_sessions(db)
        .into_iter()
        .filter(|s| s.parent_id.is_none())
        .filter_map(|s| {
            let cwd = PathBuf::from(&s.directory);
            if !want.accepts(Some(&cwd)) {
                return None;
            }
            let title = s
                .title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty() && !t.starts_with("New session"))
                .map(|t| short_title(t, &s.id))
                .or_else(|| first_opencode_prompt(db, &s.id))
                .unwrap_or_else(|| format!("Session {}", s.id.chars().take(8).collect::<String>()));
            Some(SessionSummary {
                title,
                modified: ms_to_time(s.time_updated),
                file_path: db.to_path_buf(),
                turn_count: usize::try_from(s.messages).unwrap_or(0),
                project_slug: slug_of(Some(&cwd)),
                timestamp_str: iso_from_ms(s.time_created)
                    .as_deref()
                    .and_then(format_iso_time)
                    .unwrap_or_else(|| format_system_time(ms_to_time(s.time_created))),
                provider: AgentProvider::OpenCode,
                cwd: Some(cwd),
                session_id: s.id,
            })
        })
        .collect()
}

fn first_opencode_prompt(db: &Path, id: &str) -> Option<String> {
    let mut reader = crate::tracing::opencode_db::OpenCodeReader::new(db.to_path_buf(), id);
    reader.poll().into_iter().find_map(|e| match e {
        TranscriptEvent::User {
            text, meta: false, ..
        } if !text.trim().is_empty() => Some(short_title(&text, id)),
        _ => None,
    })
}

fn load_opencode(db: &Path, id: &str) -> Vec<LogEntry> {
    let mut reader = crate::tracing::opencode_db::OpenCodeReader::new(db.to_path_buf(), id);
    entries_from_events(AgentProvider::OpenCode, reader.poll())
}

// ---------------------------------------------------------------------------
// Grok
// ---------------------------------------------------------------------------

fn discover_grok(grok_home: &Path, want: &Want) -> Vec<SessionSummary> {
    let Ok(projects) = std::fs::read_dir(grok_home.join("sessions")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for project in projects.flatten() {
        let Ok(sessions) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for session in sessions.flatten() {
            let dir = session.path();
            let chat = dir.join("chat_history.jsonl");
            if !chat.is_file() {
                continue;
            }
            let summary: Value = std::fs::read_to_string(dir.join("summary.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(Value::Null);
            let id = summary
                .pointer("/info/id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| session.file_name().to_string_lossy().into_owned());
            let cwd = summary
                .pointer("/info/cwd")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .or_else(|| {
                    percent_decode(&project.file_name().to_string_lossy()).map(PathBuf::from)
                });
            if !want.accepts(cwd.as_deref()) {
                continue;
            }
            let title = summary
                .get("session_summary")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(|t| short_title(t, &id))
                .or_else(|| {
                    load_grok(&chat).ok().and_then(|entries| {
                        entries.into_iter().find_map(|e| match e {
                            LogEntry::User { text, .. } => Some(short_title(&text, &id)),
                            _ => None,
                        })
                    })
                })
                .unwrap_or_else(|| format!("Session {}", id.chars().take(8).collect::<String>()));
            let modified = mtime(&chat);
            out.push(SessionSummary {
                title,
                turn_count: summary
                    .get("num_chat_messages")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize,
                project_slug: slug_of(cwd.as_deref()),
                timestamp_str: summary
                    .get("created_at")
                    .and_then(|v| v.as_str())
                    .and_then(format_iso_time)
                    .unwrap_or_else(|| format_system_time(modified)),
                provider: AgentProvider::Grok,
                cwd,
                session_id: id,
                modified,
                file_path: chat,
            });
        }
    }
    out
}

/// `%2FUsers%2Fme` → `/Users/me` (Grok names a project folder by its
/// url-encoded path).
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The prompt as Grok stored it, without its `<user_query>` wrapper.
fn grok_user_text(content: &Value) -> String {
    let text = value_text(content);
    let trimmed = text.trim();
    match (
        trimmed.strip_prefix("<user_query>"),
        trimmed.rfind("</user_query>"),
    ) {
        (Some(rest), Some(_)) => rest
            .rsplit_once("</user_query>")
            .map(|(a, _)| a)
            .unwrap_or(rest)
            .trim()
            .to_string(),
        _ => trimmed.to_string(),
    }
}

fn load_grok(path: &Path) -> std::io::Result<Vec<LogEntry>> {
    let reader = BufReader::new(File::open(path)?);
    let mut out = Vec::new();
    let mut thinking: Option<String> = None;
    for line in reader.lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            // Only the real prompts: Grok also writes its environment and
            // reminders as user rows (they carry `synthetic_reason`).
            Some("user") if v.get("prompt_index").is_some() => {
                out.push(LogEntry::User {
                    text: grok_user_text(v.get("content").unwrap_or(&Value::Null)),
                    timestamp: None,
                });
            }
            Some("reasoning") => {
                let text = v
                    .get("summary")
                    .and_then(|s| s.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                if !text.trim().is_empty() {
                    thinking = Some(text);
                }
            }
            Some("assistant") => {
                let text = value_text(v.get("content").unwrap_or(&Value::Null));
                let calls = v
                    .get("tool_calls")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default();
                if !text.trim().is_empty() || calls.is_empty() {
                    out.push(LogEntry::Assistant {
                        provider: AgentProvider::Grok,
                        text,
                        model: v
                            .get("model_id")
                            .and_then(|m| m.as_str())
                            .map(str::to_string),
                        thinking: thinking.take(),
                        timestamp: None,
                    });
                }
                for call in calls {
                    let args = match call.get("arguments") {
                        Some(Value::String(s)) => {
                            serde_json::from_str(s).unwrap_or(Value::String(s.clone()))
                        }
                        Some(other) => other.clone(),
                        None => Value::Null,
                    };
                    out.push(LogEntry::ToolUse {
                        id: call
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: call
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("tool")
                            .to_string(),
                        input: format_claude_tool_args(&args),
                        timestamp: None,
                    });
                }
            }
            Some("tool_result") => out.push(LogEntry::ToolResult {
                id: v
                    .get("tool_call_id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string(),
                content: value_text(v.get("content").unwrap_or(&Value::Null)),
                is_error: false,
                timestamp: None,
            }),
            _ => {}
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pi
// ---------------------------------------------------------------------------

fn discover_pi(pi_dir: &Path, want: &Want) -> Vec<SessionSummary> {
    let Ok(projects) = std::fs::read_dir(pi_dir.join("sessions")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for project in projects.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_some_and(|e| e == "jsonl")
                && let Some(s) = summarize_pi(&path, want)
            {
                out.push(s);
            }
        }
    }
    out
}

fn summarize_pi(path: &Path, want: &Want) -> Option<SessionSummary> {
    let reader = BufReader::new(File::open(path).ok()?);
    let mut id: Option<String> = None;
    let mut cwd: Option<PathBuf> = None;
    let mut first_ts: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut turns = 0usize;
    for line in reader.lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("session") => {
                id = v.get("id").and_then(|i| i.as_str()).map(str::to_string);
                cwd = v.get("cwd").and_then(|c| c.as_str()).map(PathBuf::from);
                first_ts = v
                    .get("timestamp")
                    .and_then(|t| t.as_str())
                    .map(str::to_string);
            }
            Some("message") => match v.pointer("/message/role").and_then(|r| r.as_str()) {
                Some("user") if prompt.is_none() => {
                    let text = value_text(v.pointer("/message/content").unwrap_or(&Value::Null));
                    if !text.trim().is_empty() {
                        prompt = Some(text);
                    }
                }
                Some("assistant") => turns += 1,
                _ => {}
            },
            _ => {}
        }
    }
    if !want.accepts(cwd.as_deref()) {
        return None;
    }
    let id = id.or_else(|| {
        // `<timestamp>_<id>.jsonl`
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.rsplit('_').next().unwrap_or(s).to_string())
    })?;
    let modified = mtime(path);
    Some(SessionSummary {
        title: short_title(prompt.as_deref().unwrap_or(""), &id),
        session_id: id,
        modified,
        file_path: path.to_path_buf(),
        turn_count: turns,
        project_slug: slug_of(cwd.as_deref()),
        timestamp_str: first_ts
            .as_deref()
            .and_then(format_iso_time)
            .unwrap_or_else(|| format_system_time(modified)),
        provider: AgentProvider::Pi,
        cwd,
    })
}

fn load_pi(path: &Path) -> std::io::Result<Vec<LogEntry>> {
    let reader = BufReader::new(File::open(path)?);
    let mut out = Vec::new();
    let mut model: Option<String> = None;
    for line in reader.lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let ts = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .map(str::to_string);
        match v.get("type").and_then(|t| t.as_str()) {
            Some("model_change") => {
                model = v
                    .get("modelId")
                    .and_then(|m| m.as_str())
                    .map(str::to_string);
            }
            Some("message") => {
                let Some(m) = v.get("message") else { continue };
                let content = m.get("content").unwrap_or(&Value::Null);
                match m.get("role").and_then(|r| r.as_str()) {
                    Some("user") => out.push(LogEntry::User {
                        text: value_text(content),
                        timestamp: ts,
                    }),
                    Some("assistant") => {
                        let parts = content.as_array().cloned().unwrap_or_default();
                        let text = parts
                            .iter()
                            .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
                            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n");
                        let thinking = parts
                            .iter()
                            .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("thinking"))
                            .filter_map(|p| p.get("thinking").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n");
                        let error = m
                            .get("errorMessage")
                            .and_then(|e| e.as_str())
                            .map(|e| format!("[error] {e}"));
                        let text = match (text.is_empty(), error) {
                            (true, Some(e)) => e,
                            (false, Some(e)) => format!("{text}\n{e}"),
                            (_, None) => text,
                        };
                        let calls: Vec<&Value> = parts
                            .iter()
                            .filter(|p| {
                                matches!(
                                    p.get("type").and_then(|t| t.as_str()),
                                    Some("toolCall" | "tool_use")
                                )
                            })
                            .collect();
                        if !text.trim().is_empty() || !thinking.is_empty() || calls.is_empty() {
                            out.push(LogEntry::Assistant {
                                provider: AgentProvider::Pi,
                                text,
                                model: m
                                    .get("model")
                                    .and_then(|x| x.as_str())
                                    .map(str::to_string)
                                    .or_else(|| model.clone()),
                                thinking: (!thinking.is_empty()).then_some(thinking),
                                timestamp: ts.clone(),
                            });
                        }
                        for call in calls {
                            let args = call
                                .get("arguments")
                                .or_else(|| call.get("input"))
                                .cloned()
                                .unwrap_or(Value::Null);
                            out.push(LogEntry::ToolUse {
                                id: call
                                    .get("id")
                                    .and_then(|i| i.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                name: call
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("tool")
                                    .to_string(),
                                input: format_claude_tool_args(&args),
                                timestamp: ts.clone(),
                            });
                        }
                    }
                    Some("toolResult") => out.push(LogEntry::ToolResult {
                        id: m
                            .get("toolCallId")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string(),
                        content: value_text(content),
                        is_error: m.get("isError").and_then(|e| e.as_bool()).unwrap_or(false),
                        timestamp: ts,
                    }),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn percent_decode_restores_a_path() {
        assert_eq!(
            percent_decode("%2FUsers%2Fme%2Fapp").as_deref(),
            Some("/Users/me/app")
        );
        assert_eq!(percent_decode("plain").as_deref(), Some("plain"));
    }

    #[test]
    fn cursor_chats_are_listed_by_folder_and_read() {
        let home = tempfile::tempdir().unwrap();
        let proj = home.path().join("work");
        let other = home.path().join("other");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let cursor = home.path().join(".cursor");
        let chat = |id: &str, cwd: &Path, prompt: &str| {
            write(
                &cursor.join(format!("projects/p-{id}/agent-transcripts/{id}/{id}.jsonl")),
                &format!(
                    concat!(
                        r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"<timestamp>Tuesday</timestamp>\n<user_query>\n{prompt}\n</user_query>"}}]}}}}"#,
                        "\n",
                        r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"Reading it."}},{{"type":"tool_use","name":"Shell","input":{{"command":"cat a.txt"}}}}]}}}}"#,
                        "\n",
                        r#"{{"role":"assistant","message":{{"content":[{{"type":"text","text":"Done."}}]}}}}"#,
                        "\n",
                        r#"{{"type":"turn_ended","status":"success"}}"#,
                        "\n",
                    ),
                    prompt = prompt
                ),
            );
            write(
                &cursor.join(format!("chats/h-{id}/{id}/meta.json")),
                &serde_json::json!({ "cwd": cwd }).to_string(),
            );
        };
        chat("c-1", &proj, "explain this repo");
        chat("c-2", &other, "something else");
        let dirs = HistoryDirs {
            cursor: Some(cursor),
            ..Default::default()
        };
        let here = discover_all(&dirs, Some(&proj), false);
        assert_eq!(here.len(), 1);
        assert_eq!(here[0].provider, AgentProvider::Cursor);
        assert_eq!(here[0].session_id, "c-1");
        assert_eq!(here[0].title, "explain this repo");
        assert_eq!(here[0].turn_count, 2);
        assert_eq!(here[0].cwd.as_deref(), Some(proj.as_path()));
        assert_eq!(discover_all(&dirs, Some(&proj), true).len(), 2);

        let log = load_log(&here[0]).unwrap();
        assert!(
            matches!(&log[0], LogEntry::User { text, .. } if text == "explain this repo"),
            "{log:?}"
        );
        assert!(log.len() >= 3, "{log:?}");
        // the harness resumes it by its chat id
        assert_eq!(
            crate::harness::resume_args(here[0].provider.harness(), &here[0].session_id),
            ["--resume", "c-1"]
        );
    }

    #[test]
    fn pi_sessions_are_listed_and_read() {
        let home = tempfile::tempdir().unwrap();
        let proj = home.path().join("work");
        std::fs::create_dir_all(&proj).unwrap();
        let file = home
            .path()
            .join("sessions/--work--/2026-11-12T13-26-50-072Z_abc.jsonl");
        write(
            &file,
            &format!(
                concat!(
                    r#"{{"type":"session","version":3,"id":"abc","timestamp":"2026-11-12T13:26:50.072Z","cwd":"{cwd}"}}"#,
                    "\n",
                    r#"{{"type":"message","timestamp":"2026-11-12T13:27:00Z","message":{{"role":"user","content":[{{"type":"text","text":"explain me this repo"}}]}}}}"#,
                    "\n",
                    r#"{{"type":"message","timestamp":"2026-11-12T13:27:05Z","message":{{"role":"assistant","model":"m1","content":[{{"type":"thinking","thinking":"hmm"}},{{"type":"text","text":"It is a mux."}},{{"type":"toolCall","id":"t1","name":"read","arguments":{{"file_path":"a.rs"}}}}]}}}}"#,
                    "\n",
                    r#"{{"type":"message","message":{{"role":"toolResult","toolCallId":"t1","content":[{{"type":"text","text":"fn main"}}],"isError":false}}}}"#,
                    "\n"
                ),
                cwd = proj.display()
            ),
        );
        let dirs = HistoryDirs {
            pi: Some(home.path().to_path_buf()),
            ..Default::default()
        };
        let all = discover_all(&dirs, None, true);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].provider, AgentProvider::Pi);
        assert_eq!(all[0].session_id, "abc");
        assert_eq!(all[0].title, "explain me this repo");
        assert_eq!(all[0].turn_count, 1);
        let here = discover_all(&dirs, Some(&proj), false);
        assert_eq!(here.len(), 1);
        let elsewhere = discover_all(&dirs, Some(home.path()), false);
        assert!(elsewhere.is_empty());

        let log = load_log(&all[0]).unwrap();
        assert!(matches!(&log[0], LogEntry::User { text, .. } if text == "explain me this repo"));
        assert!(matches!(
            &log[1],
            LogEntry::Assistant { text, thinking: Some(t), .. } if text == "It is a mux." && t == "hmm"
        ));
        assert!(
            matches!(&log[2], LogEntry::ToolUse { name, input, .. } if name == "read" && input == "a.rs")
        );
        assert!(matches!(&log[3], LogEntry::ToolResult { content, .. } if content == "fn main"));
    }

    #[test]
    fn grok_sessions_are_listed_and_read() {
        let home = tempfile::tempdir().unwrap();
        let proj = home.path().join("work");
        std::fs::create_dir_all(&proj).unwrap();
        let dir = home.path().join("sessions/%2Fwork/sess-1");
        write(
            &dir.join("summary.json"),
            &format!(
                r#"{{"info":{{"id":"sess-1","cwd":"{}"}},"session_summary":"Explain the repo","created_at":"2026-10-31T19:29:03Z","num_chat_messages":3}}"#,
                proj.display()
            ),
        );
        write(
            &dir.join("chat_history.jsonl"),
            concat!(
                r#"{"type":"system","content":"sys"}"#,
                "\n",
                r#"{"type":"user","content":[{"type":"text","text":"<env>"}],"synthetic_reason":"x"}"#,
                "\n",
                r#"{"type":"user","content":[{"type":"text","text":"<user_query>\nexplain\n</user_query>"}],"prompt_index":0}"#,
                "\n",
                r#"{"type":"reasoning","summary":[{"type":"summary_text","text":"think"}]}"#,
                "\n",
                r#"{"type":"assistant","content":"looking","model_id":"grok-4.7","tool_calls":[{"id":"c1","name":"list_dir","arguments":"{\"target\":\"/\"}"}]}"#,
                "\n",
                r#"{"type":"tool_result","tool_call_id":"c1","content":"a\nb"}"#,
                "\n",
                r#"{"type":"assistant","content":"done","model_id":"grok-4.7"}"#,
                "\n"
            ),
        );
        let dirs = HistoryDirs {
            grok: Some(home.path().to_path_buf()),
            ..Default::default()
        };
        let all = discover_all(&dirs, Some(&proj), false);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].provider, AgentProvider::Grok);
        assert_eq!(all[0].title, "Explain the repo");
        let log = load_log(&all[0]).unwrap();
        assert!(matches!(&log[0], LogEntry::User { text, .. } if text == "explain"));
        assert!(matches!(
            &log[1],
            LogEntry::Assistant { text, thinking: Some(t), .. } if text == "looking" && t == "think"
        ));
        assert!(matches!(&log[2], LogEntry::ToolUse { name, .. } if name == "list_dir"));
        assert!(matches!(&log[3], LogEntry::ToolResult { content, .. } if content == "a\nb"));
        assert!(matches!(&log[4], LogEntry::Assistant { text, .. } if text == "done"));
        assert_eq!(log.len(), 5);
    }

    #[test]
    fn codex_rollouts_are_listed_and_read() {
        let home = tempfile::tempdir().unwrap();
        let proj = home.path().join("work");
        std::fs::create_dir_all(&proj).unwrap();
        let file = home
            .path()
            .join("sessions/2026/11/12/rollout-2026-11-12T10-00-00-abc.jsonl");
        write(
            &file,
            &format!(
                concat!(
                    r#"{{"timestamp":"2026-11-12T10:00:00Z","type":"session_meta","payload":{{"id":"abc","cwd":"{cwd}"}}}}"#,
                    "\n",
                    r#"{{"timestamp":"2026-11-12T10:00:01Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"fix the bug"}}]}}}}"#,
                    "\n",
                    r#"{{"timestamp":"2026-11-12T10:00:02Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Fixed."}}]}}}}"#,
                    "\n"
                ),
                cwd = proj.display()
            ),
        );
        let dirs = HistoryDirs {
            codex: Some(home.path().to_path_buf()),
            ..Default::default()
        };
        let all = discover_all(&dirs, Some(&proj), false);
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].provider, AgentProvider::Codex);
        assert_eq!(all[0].session_id, "abc");
        assert_eq!(all[0].title, "fix the bug");
        let log = load_log(&all[0]).unwrap();
        assert!(matches!(&log[0], LogEntry::User { text, .. } if text == "fix the bug"));
        assert!(
            matches!(&log[1], LogEntry::Assistant { provider: AgentProvider::Codex, text, .. } if text == "Fixed.")
        );
    }
}
