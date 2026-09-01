use crate::transcript::TranscriptEvent;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Which coding agent recorded the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentProvider {
    Claude,
    Antigravity,
}

/// Converts a directory path into Claude Code's project folder slug format.
/// E.g. `/home/user/project` -> `-home-user-project`
pub fn project_slug(path: &Path) -> String {
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };
    let normalized = abs_path.canonicalize().unwrap_or(abs_path);
    let s = normalized.to_string_lossy();
    let mut slug = String::with_capacity(s.len() + 1);
    for c in s.chars() {
        if c == '/' || c == '\\' || c == ':' {
            slug.push('-');
        } else {
            slug.push(c);
        }
    }
    if !slug.starts_with('-') && !slug.is_empty() {
        slug.insert(0, '-');
    }
    slug
}

/// Metadata summary of a discovered past session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub session_id: String,
    pub title: String,
    pub modified: SystemTime,
    pub file_path: PathBuf,
    pub turn_count: usize,
    pub project_slug: String,
    pub timestamp_str: String,
    pub provider: AgentProvider,
}

/// Structured turn or event in a session log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEntry {
    User {
        text: String,
        timestamp: Option<String>,
    },
    Assistant {
        provider: AgentProvider,
        text: String,
        model: Option<String>,
        thinking: Option<String>,
        timestamp: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: String,
        timestamp: Option<String>,
    },
    ToolResult {
        id: String,
        content: String,
        is_error: bool,
        timestamp: Option<String>,
    },
}

/// Returns the base `.claude` directory, resolving `~/.claude`.
pub fn default_claude_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let p = PathBuf::from(home).join(".claude");
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

/// The antigravity-cli ROOT (`~/.gemini/antigravity-cli`) — `brain/` and
/// `presence/` live under it. Shared by the history viewer, the Langfuse
/// correlator, and the doctor so they can never disagree on composition.
pub fn default_antigravity_root() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".gemini").join("antigravity-cli"))
}

/// Returns the base Antigravity brain directory, resolving `~/.gemini/antigravity-cli/brain`.
pub fn default_antigravity_dir() -> Option<PathBuf> {
    default_antigravity_root()
        .map(|root| root.join("brain"))
        .filter(|p| p.is_dir())
}

/// Scans both Claude and Antigravity storage for past session files.
pub fn discover_sessions(
    claude_dir: Option<&Path>,
    antigravity_dir: Option<&Path>,
    current_dir: Option<&Path>,
    all_projects: bool,
) -> Vec<SessionSummary> {
    let mut summaries = Vec::new();
    summaries.extend(discover_claude_sessions(claude_dir, current_dir, all_projects));
    summaries.extend(discover_antigravity_sessions(antigravity_dir, current_dir, all_projects));
    summaries.sort_by_key(|a| std::cmp::Reverse(a.modified));
    summaries
}

/// Scans Claude project folders to discover session JSONL files.
pub fn discover_claude_sessions(
    claude_dir: Option<&Path>,
    current_dir: Option<&Path>,
    all_projects: bool,
) -> Vec<SessionSummary> {
    let base_claude = match claude_dir {
        Some(d) => d.to_path_buf(),
        None => match default_claude_dir() {
            Some(d) => d,
            None => return Vec::new(),
        },
    };

    let projects_dir = base_claude.join("projects");
    if !projects_dir.is_dir() {
        return Vec::new();
    }

    let target_slug = current_dir.map(project_slug);
    let mut summaries = Vec::new();

    let project_entries = match std::fs::read_dir(&projects_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    for project_entry in project_entries.filter_map(|e| e.ok()) {
        let path = project_entry.path();
        if !path.is_dir() {
            continue;
        }

        let folder_name = project_entry.file_name().to_string_lossy().into_owned();
        if !all_projects
            && let Some(ref slug) = target_slug
            && &folder_name != slug
        {
            continue;
        }

        let session_files = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for file_entry in session_files.filter_map(|e| e.ok()) {
            let file_path = file_entry.path();
            if file_path.extension().is_some_and(|ext| ext == "jsonl")
                && let Some(summary) = summarize_claude_file(&file_path, &folder_name)
            {
                summaries.push(summary);
            }
        }
    }

    summaries
}

/// Reads a Claude session JSONL file and creates a high-level summary.
pub fn summarize_claude_file(file_path: &Path, project_slug: &str) -> Option<SessionSummary> {
    let file_stem = file_path.file_stem()?.to_string_lossy().into_owned();
    let metadata = std::fs::metadata(file_path).ok()?;
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

    let file = File::open(file_path).ok()?;
    let reader = BufReader::new(file);

    let mut title: Option<String> = None;
    let mut first_user_prompt: Option<String> = None;
    let mut first_timestamp: Option<String> = None;
    let mut turn_count = 0;

    for line_res in reader.lines() {
        let line = match line_res {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let v: Value = match serde_json::from_str(&line) {
            Ok(val) => val,
            Err(_) => continue,
        };

        if first_timestamp.is_none()
            && let Some(ts) = v.get("timestamp").and_then(|t| t.as_str())
        {
            first_timestamp = Some(ts.to_string());
        }

        if let Some(event_type) = v.get("type").and_then(|t| t.as_str()) {
            if event_type == "ai-title" {
                if let Some(t) = v.get("aiTitle").and_then(|t| t.as_str()) {
                    title = Some(t.to_string());
                }
            } else if event_type == "user" {
                turn_count += 1;
                if first_user_prompt.is_none()
                    && let Some(msg) = v.get("message")
                {
                    if let Some(content_str) = msg.get("content").and_then(|c| c.as_str()) {
                        first_user_prompt = Some(content_str.to_string());
                    } else if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                        for item in arr {
                            if item.get("type").and_then(|t| t.as_str()) == Some("text")
                                && let Some(txt) = item.get("text").and_then(|t| t.as_str())
                            {
                                first_user_prompt = Some(txt.to_string());
                                break;
                            }
                        }
                    }
                }
            } else if event_type == "assistant" {
                turn_count += 1;
            }
        }
    }

    let final_title = title
        .or(first_user_prompt.map(|p| {
            let first_line = p.lines().next().unwrap_or("").trim();
            if first_line.chars().count() > 40 {
                format!("{}...", first_line.chars().take(37).collect::<String>())
            } else {
                first_line.to_string()
            }
        }))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| format!("Session {}", &file_stem[..file_stem.len().min(8)]));

    let timestamp_str = first_timestamp
        .as_deref()
        .and_then(format_iso_time)
        .unwrap_or_else(|| format_system_time(modified));

    Some(SessionSummary {
        session_id: file_stem,
        title: final_title,
        modified,
        file_path: file_path.to_path_buf(),
        turn_count,
        project_slug: project_slug.to_string(),
        timestamp_str,
        provider: AgentProvider::Claude,
    })
}

pub use crate::transcript::extract_user_request;

/// Scans Antigravity brain directories for session transcripts.
pub fn discover_antigravity_sessions(
    brain_dir: Option<&Path>,
    current_dir: Option<&Path>,
    all_projects: bool,
) -> Vec<SessionSummary> {
    let base = match brain_dir {
        Some(d) => d.to_path_buf(),
        None => match default_antigravity_dir() {
            Some(d) => d,
            None => return Vec::new(),
        },
    };

    let entries = match std::fs::read_dir(&base) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let target_dir_str = current_dir.map(|p| p.to_string_lossy().into_owned());
    let mut summaries = Vec::new();

    for entry in entries.filter_map(|e| e.ok()) {
        let conv_dir = entry.path();
        if !conv_dir.is_dir() {
            continue;
        }

        let transcript_file = conv_dir
            .join(".system_generated")
            .join("logs")
            .join("transcript.jsonl");
        if transcript_file.is_file() {
            let session_id = entry.file_name().to_string_lossy().into_owned();
            if let Some(summary) = summarize_antigravity_file(
                &transcript_file,
                &session_id,
                target_dir_str.as_deref(),
                all_projects,
            ) {
                summaries.push(summary);
            }
        }
    }

    summaries
}

/// Reads an Antigravity transcript.jsonl file and creates a high-level summary.
pub fn summarize_antigravity_file(
    file_path: &Path,
    session_id: &str,
    target_dir_str: Option<&str>,
    all_projects: bool,
) -> Option<SessionSummary> {
    let metadata = std::fs::metadata(file_path).ok()?;
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

    let file = File::open(file_path).ok()?;
    let reader = BufReader::new(file);

    let mut first_user_prompt: Option<String> = None;
    let mut first_timestamp: Option<String> = None;
    let mut turn_count = 0;
    let mut matches_project = all_projects;
    let mut detected_project_name: Option<String> = None;

    for line_res in reader.lines() {
        let line = match line_res {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let v: Value = match serde_json::from_str(&line) {
            Ok(val) => val,
            Err(_) => continue,
        };

        if first_timestamp.is_none()
            && let Some(ts) = v.get("created_at").and_then(|t| t.as_str())
        {
            first_timestamp = Some(ts.to_string());
        }

        let step_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if step_type == "USER_INPUT" {
            turn_count += 1;
            if first_user_prompt.is_none()
                && let Some(content) = v.get("content").and_then(|c| c.as_str())
            {
                let clean = extract_user_request(content);
                if !clean.is_empty() {
                    first_user_prompt = Some(clean);
                }
            }
        } else if step_type == "PLANNER_RESPONSE" {
            turn_count += 1;
        }

        if !matches_project
            && let Some(target) = target_dir_str
            && line.contains(target)
        {
            matches_project = true;
        }

        if detected_project_name.is_none()
            && let Some(tool_calls) = v.get("tool_calls").and_then(|tc| tc.as_array())
        {
            for tc in tool_calls {
                if let Some(args) = tc.get("args") {
                    for key in &[
                        "DirectoryPath",
                        "AbsolutePath",
                        "Cwd",
                        "SearchDirectory",
                        "SearchPath",
                    ] {
                        if let Some(val) = args.get(key).and_then(|v| v.as_str()) {
                            let unquoted = val.trim_matches('"');
                            if let Some(name) = Path::new(unquoted).file_name() {
                                detected_project_name =
                                    Some(name.to_string_lossy().into_owned());
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    if !all_projects && target_dir_str.is_some() && !matches_project {
        return None;
    }

    let final_title = first_user_prompt
        .map(|p| {
            let first_line = p.lines().next().unwrap_or("").trim();
            if first_line.chars().count() > 40 {
                format!("{}...", first_line.chars().take(37).collect::<String>())
            } else {
                first_line.to_string()
            }
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| format!("Session {}", &session_id[..session_id.len().min(8)]));

    let timestamp_str = first_timestamp
        .as_deref()
        .and_then(format_iso_time)
        .unwrap_or_else(|| format_system_time(modified));

    let project_slug = detected_project_name.unwrap_or_else(|| "antigravity".to_string());

    Some(SessionSummary {
        session_id: session_id.to_string(),
        title: final_title,
        modified,
        file_path: file_path.to_path_buf(),
        turn_count,
        project_slug,
        timestamp_str,
        provider: AgentProvider::Antigravity,
    })
}

/// Parses an entire session JSONL file (Claude or Antigravity) into a list of structured log entries.
pub fn load_session_log(file_path: &Path) -> std::io::Result<Vec<LogEntry>> {
    let file = File::open(file_path)?;
    let mut first_line = String::new();
    let mut reader = BufReader::new(file);
    let _ = reader.read_line(&mut first_line);

    if first_line.contains("\"step_index\"") || first_line.contains("USER_EXPLICIT") {
        load_antigravity_log(file_path)
    } else {
        load_claude_log(file_path)
    }
}

/// Flattens Claude structured tool input to the viewer's display string:
/// prefer `command`, then `file_path`, else pretty-printed JSON.
fn format_claude_tool_args(args: &Value) -> String {
    if let Some(cmd) = args.get("command").and_then(|c| c.as_str()) {
        cmd.to_string()
    } else if let Some(file_path) = args.get("file_path").and_then(|c| c.as_str()) {
        file_path.to_string()
    } else if args.is_object() || args.is_array() {
        serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string())
    } else {
        args.to_string()
    }
}

/// Parses a Claude Code session JSONL file. Thin display adapter over
/// `transcript::parse_claude_line`: thinking and usage (which the viewer
/// never showed) are dropped, tool args are flattened to strings, and
/// array-form tool_result content collapses to "" — exactly the
/// pre-refactor behavior, pinned by the existing tests.
pub fn load_claude_log(file_path: &Path) -> std::io::Result<Vec<LogEntry>> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line_res in reader.lines() {
        let line = match line_res {
            Ok(l) => l,
            Err(_) => continue,
        };
        for ev in crate::transcript::parse_claude_line(&line) {
            match ev {
                TranscriptEvent::User { text, ts } => {
                    entries.push(LogEntry::User {
                        text,
                        timestamp: ts,
                    });
                }
                TranscriptEvent::Assistant {
                    text, model, ts, ..
                } => {
                    entries.push(LogEntry::Assistant {
                        provider: AgentProvider::Claude,
                        text,
                        model,
                        thinking: None,
                        timestamp: ts,
                    });
                }
                TranscriptEvent::ToolUse { id, name, args, ts } => {
                    entries.push(LogEntry::ToolUse {
                        id,
                        name,
                        input: format_claude_tool_args(&args),
                        timestamp: ts,
                    });
                }
                TranscriptEvent::ToolResult {
                    id,
                    content,
                    is_error,
                    ts,
                } => {
                    entries.push(LogEntry::ToolResult {
                        id,
                        content: content.as_str().unwrap_or("").to_string(),
                        is_error,
                        timestamp: ts,
                    });
                }
                // export-only events the viewer never displayed
                _ => {}
            }
        }
    }

    Ok(entries)
}

/// Formats tool call arguments for Antigravity tools.
fn format_antigravity_tool_args(tool_name: &str, args: &Value) -> String {
    if let Some(obj) = args.as_object() {
        if tool_name == "run_command"
            && let Some(cmd) = obj.get("CommandLine").and_then(|c| c.as_str())
        {
            return cmd.trim().trim_matches('"').to_string();
        } else if tool_name == "view_file"
            && let Some(path) = obj.get("AbsolutePath").and_then(|c| c.as_str())
        {
            return path.trim().trim_matches('"').to_string();
        } else if tool_name == "list_dir"
            && let Some(path) = obj.get("DirectoryPath").and_then(|c| c.as_str())
        {
            return path.trim().trim_matches('"').to_string();
        }
        let mut parts = Vec::new();
        for (k, v) in obj {
            let val_str = v.as_str().unwrap_or("");
            let clean_val = val_str.trim().trim_matches('"');
            parts.push(format!("{k}: {clean_val}"));
        }
        parts.join(", ")
    } else {
        args.to_string()
    }
}

/// Parses an Antigravity transcript.jsonl file into structured log entries.
/// Thin display adapter over `transcript::parse_antigravity_line` — same
/// output as the pre-refactor parser (tool args flattened for display,
/// thinking kept).
pub fn load_antigravity_log(file_path: &Path) -> std::io::Result<Vec<LogEntry>> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line_res in reader.lines() {
        let line = match line_res {
            Ok(l) => l,
            Err(_) => continue,
        };
        for ev in crate::transcript::parse_antigravity_line(&line) {
            match ev {
                TranscriptEvent::User { text, ts } => {
                    entries.push(LogEntry::User {
                        text,
                        timestamp: ts,
                    });
                }
                TranscriptEvent::Assistant {
                    text, thinking, ts, ..
                } => {
                    entries.push(LogEntry::Assistant {
                        provider: AgentProvider::Antigravity,
                        text,
                        model: None,
                        thinking,
                        timestamp: ts,
                    });
                }
                TranscriptEvent::ToolUse { name, args, ts, .. } => {
                    let input = format_antigravity_tool_args(&name, &args);
                    entries.push(LogEntry::ToolUse {
                        id: String::new(),
                        name,
                        input,
                        timestamp: ts,
                    });
                }
                TranscriptEvent::ToolResult {
                    content,
                    is_error,
                    ts,
                    ..
                } => {
                    entries.push(LogEntry::ToolResult {
                        id: String::new(),
                        content: content.as_str().unwrap_or("").to_string(),
                        is_error,
                        timestamp: ts,
                    });
                }
                _ => {}
            }
        }
    }

    Ok(entries)
}

/// Renders structured log entries into styled Ratatui Lines for display.
pub fn render_log_lines(entries: &[LogEntry]) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    if entries.is_empty() {
        lines.push(Line::styled(
            "  (No log entries found in this session transcript)",
            Style::default().fg(Color::DarkGray),
        ));
        return lines;
    }

    for (idx, entry) in entries.iter().enumerate() {
        if idx > 0 {
            lines.push(Line::raw(""));
        }

        match entry {
            LogEntry::User { text, timestamp } => {
                let ts_label = timestamp
                    .as_deref()
                    .and_then(format_iso_time)
                    .map(|t| format!(" ({t})"))
                    .unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled(
                        "👤 USER",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(ts_label, Style::default().fg(Color::DarkGray)),
                ]));
                for line in text.lines() {
                    lines.push(Line::styled(
                        format!("  {line}"),
                        Style::default().fg(Color::Cyan),
                    ));
                }
            }
            LogEntry::Assistant {
                provider,
                text,
                model,
                thinking,
                timestamp,
            } => {
                let (agent_name, agent_style) = match provider {
                    AgentProvider::Claude => (
                        "🤖 CLAUDE",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    AgentProvider::Antigravity => (
                        "✨ ANTIGRAVITY",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                };
                let model_label = model
                    .as_ref()
                    .map(|m| format!(" [{m}]"))
                    .unwrap_or_default();
                let ts_label = timestamp
                    .as_deref()
                    .and_then(format_iso_time)
                    .map(|t| format!(" ({t})"))
                    .unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled(agent_name, agent_style),
                    Span::styled(model_label, Style::default().fg(Color::LightGreen)),
                    Span::styled(ts_label, Style::default().fg(Color::DarkGray)),
                ]));

                if let Some(th) = thinking {
                    lines.push(Line::styled(
                        "  💭 Thinking:",
                        Style::default().fg(Color::DarkGray),
                    ));
                    for line in th.lines() {
                        lines.push(Line::styled(
                            format!("    {line}"),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }

                for line in text.lines() {
                    lines.push(Line::styled(
                        format!("  {line}"),
                        Style::default().fg(Color::White),
                    ));
                }
            }
            LogEntry::ToolUse {
                name,
                input,
                timestamp,
                ..
            } => {
                let ts_label = timestamp
                    .as_deref()
                    .and_then(format_iso_time)
                    .map(|t| format!(" ({t})"))
                    .unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("⚙️ TOOL: {name}"),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(ts_label, Style::default().fg(Color::DarkGray)),
                ]));
                for line in input.lines() {
                    lines.push(Line::styled(
                        format!("  $ {line}"),
                        Style::default().fg(Color::LightYellow),
                    ));
                }
            }
            LogEntry::ToolResult {
                content,
                is_error,
                ..
            } => {
                let (status_label, style) = if *is_error {
                    ("  ── Result (Error) ──", Style::default().fg(Color::Red))
                } else {
                    ("  ── Result ──", Style::default().fg(Color::DarkGray))
                };
                lines.push(Line::styled(status_label, style));
                for line in content.lines().take(50) {
                    lines.push(Line::styled(
                        format!("    {line}"),
                        if *is_error {
                            Style::default().fg(Color::LightRed)
                        } else {
                            Style::default().fg(Color::Gray)
                        },
                    ));
                }
                if content.lines().count() > 50 {
                    lines.push(Line::styled(
                        format!("    ... ({} lines truncated)", content.lines().count() - 50),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
        }
    }

    lines
}

fn format_iso_time(iso: &str) -> Option<String> {
    // Expects e.g. "2026-08-30T19:28:18.806Z" -> "2026-08-30 19:28"
    if iso.len() >= 16 {
        let date = &iso[0..10];
        let time = &iso[11..16];
        Some(format!("{date} {time}"))
    } else {
        None
    }
}

fn format_system_time(time: SystemTime) -> String {
    let dur = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let days = secs / 86400;
    let rem = secs % 86400;
    let hours = rem / 3600;
    let minutes = (rem % 3600) / 60;
    format!("day {days} {hours:02}:{minutes:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_slug() {
        assert_eq!(
            project_slug(Path::new("/home/silvio/workspace/agent-mux")),
            "-home-silvio-workspace-agent-mux"
        );
        let win_path = Path::new("C:\\Users\\silvio\\workspace\\app");
        let slug = project_slug(win_path);
        assert!(slug.contains("Users-silvio-workspace-app"));
        assert!(slug.starts_with('-'));
    }

    #[test]
    fn test_summarize_and_load_claude_session() {
        let temp_dir = tempfile::tempdir().unwrap();
        let proj_dir = temp_dir.path().join("projects").join("-test-project");
        std::fs::create_dir_all(&proj_dir).unwrap();

        let session_file = proj_dir.join("test-session-uuid.jsonl");
        let jsonl_content = r#"{"type":"ai-title","aiTitle":"My Test Session","sessionId":"test-session-uuid"}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Fix the build error"}]},"timestamp":"2026-08-30T19:28:18.806Z"}
{"type":"assistant","message":{"role":"assistant","model":"claude-opus-5","content":[{"type":"text","text":"Sure, running cargo check."},{"type":"tool_use","id":"tool_1","name":"Bash","input":{"command":"cargo check"}}]},"timestamp":"2026-08-30T19:28:20.590Z"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool_1","content":"error[E0425]: cannot find value","is_error":true}]},"timestamp":"2026-08-30T19:28:22.000Z"}
"#;
        std::fs::write(&session_file, jsonl_content).unwrap();

        let summaries = discover_claude_sessions(Some(temp_dir.path()), Some(Path::new("/test/project")), false);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].session_id, "test-session-uuid");
        assert_eq!(summaries[0].title, "My Test Session");
        assert_eq!(summaries[0].provider, AgentProvider::Claude);
        assert_eq!(summaries[0].turn_count, 3);

        let entries = load_session_log(&session_file).unwrap();
        assert_eq!(entries.len(), 4);
        assert!(matches!(&entries[0], LogEntry::User { text, .. } if text == "Fix the build error"));
        assert!(matches!(&entries[1], LogEntry::Assistant { text, model, provider, .. } if text == "Sure, running cargo check." && model.as_deref() == Some("claude-opus-5") && *provider == AgentProvider::Claude));
        assert!(matches!(&entries[2], LogEntry::ToolUse { name, input, .. } if name == "Bash" && input == "cargo check"));
        assert!(matches!(&entries[3], LogEntry::ToolResult { is_error, .. } if *is_error));

        let lines = render_log_lines(&entries);
        assert!(!lines.is_empty());
        let text = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("USER"));
        assert!(text.contains("CLAUDE"));
        assert!(text.contains("TOOL: Bash"));
        assert!(text.contains("Result (Error)"));
    }

    #[test]
    fn test_summarize_and_load_antigravity_session() {
        let temp_dir = tempfile::tempdir().unwrap();
        let brain_dir = temp_dir.path().join("brain").join("agy-session-uuid");
        let logs_dir = brain_dir.join(".system_generated").join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();

        let transcript_file = logs_dir.join("transcript.jsonl");
        let transcript_content = r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-08-30T19:01:37Z","content":"<USER_REQUEST>\ninstall rust and build this app\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\ninfo\n</ADDITIONAL_METADATA>"}
{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-08-30T19:01:38Z","thinking":"I need to list directory first","tool_calls":[{"name":"list_dir","args":{"DirectoryPath":"\"/test/my-project\"","toolAction":"\"Listing workspace directory\"","toolSummary":"\"List workspace directory\""}}]}
{"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-08-30T19:01:40Z","content":"Created At: 2026-08-30T16:01:40-03:00\nCargo.toml\nsrc"}
"#;
        std::fs::write(&transcript_file, transcript_content).unwrap();

        let summaries = discover_antigravity_sessions(Some(&temp_dir.path().join("brain")), Some(Path::new("/test/my-project")), false);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].session_id, "agy-session-uuid");
        assert_eq!(summaries[0].title, "install rust and build this app");
        assert_eq!(summaries[0].provider, AgentProvider::Antigravity);

        let entries = load_session_log(&transcript_file).unwrap();
        assert_eq!(entries.len(), 4);
        assert!(matches!(&entries[0], LogEntry::User { text, .. } if text == "install rust and build this app"));
        assert!(matches!(&entries[1], LogEntry::Assistant { thinking, provider, .. } if thinking.as_deref() == Some("I need to list directory first") && *provider == AgentProvider::Antigravity));
        assert!(matches!(&entries[2], LogEntry::ToolUse { name, input, .. } if name == "list_dir" && input == "/test/my-project"));
        assert!(matches!(&entries[3], LogEntry::ToolResult { content, .. } if content.contains("Cargo.toml")));

        let lines = render_log_lines(&entries);
        let text = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("USER"));
        assert!(text.contains("install rust and build this app"));
        assert!(text.contains("TOOL: list_dir"));
        assert!(text.contains("Result"));
    }
}
