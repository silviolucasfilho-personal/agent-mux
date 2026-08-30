use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Converts a directory path into Claude Code's project folder slug format.
/// E.g. `/home/user/project` -> `-home-user-project`
pub fn project_slug(path: &Path) -> String {
    let s = path.to_string_lossy();
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
}

/// Structured turn or event in a session log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEntry {
    User {
        text: String,
        timestamp: Option<String>,
    },
    Assistant {
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

/// Scans Claude project folders to discover session JSONL files.
pub fn discover_sessions(
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
                && let Some(summary) = summarize_session_file(&file_path, &folder_name)
            {
                summaries.push(summary);
            }
        }
    }

    summaries.sort_by_key(|a| std::cmp::Reverse(a.modified));
    summaries
}

/// Reads a session JSONL file and creates a high-level summary.
pub fn summarize_session_file(file_path: &Path, project_slug: &str) -> Option<SessionSummary> {
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
    })
}

/// Parses an entire session JSONL file into a list of structured log entries.
pub fn load_session_log(file_path: &Path) -> std::io::Result<Vec<LogEntry>> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line_res in reader.lines() {
        let line = match line_res {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }

        let v: Value = match serde_json::from_str(&line) {
            Ok(val) => val,
            Err(_) => continue,
        };

        let timestamp = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .map(|t| t.to_string());
        let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match event_type {
            "user" => {
                if let Some(msg) = v.get("message") {
                    if let Some(content_str) = msg.get("content").and_then(|c| c.as_str()) {
                        entries.push(LogEntry::User {
                            text: content_str.to_string(),
                            timestamp: timestamp.clone(),
                        });
                    } else if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                        for item in arr {
                            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            if item_type == "text" {
                                if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                                    entries.push(LogEntry::User {
                                        text: txt.to_string(),
                                        timestamp: timestamp.clone(),
                                    });
                                }
                            } else if item_type == "tool_result" {
                                let id = item
                                    .get("tool_use_id")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let content = item
                                    .get("content")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let is_error = item
                                    .get("is_error")
                                    .and_then(|e| e.as_bool())
                                    .unwrap_or(false);
                                entries.push(LogEntry::ToolResult {
                                    id,
                                    content,
                                    is_error,
                                    timestamp: timestamp.clone(),
                                });
                            }
                        }
                    }
                }
            }
            "assistant" => {
                if let Some(msg) = v.get("message") {
                    let model = msg
                        .get("model")
                        .and_then(|m| m.as_str())
                        .map(|m| m.to_string());
                    if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                        for item in arr {
                            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            if item_type == "text" {
                                if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                                    entries.push(LogEntry::Assistant {
                                        text: txt.to_string(),
                                        model: model.clone(),
                                        thinking: None,
                                        timestamp: timestamp.clone(),
                                    });
                                }
                            } else if item_type == "tool_use" {
                                let id = item
                                    .get("id")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let name = item
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let input_val = item.get("input").unwrap_or(&Value::Null);
                                let input_str = if let Some(cmd) =
                                    input_val.get("command").and_then(|c| c.as_str())
                                {
                                    cmd.to_string()
                                } else if let Some(file_path) =
                                    input_val.get("file_path").and_then(|c| c.as_str())
                                {
                                    file_path.to_string()
                                } else if input_val.is_object() || input_val.is_array() {
                                    serde_json::to_string_pretty(input_val)
                                        .unwrap_or_else(|_| input_val.to_string())
                                } else {
                                    input_val.to_string()
                                };
                                entries.push(LogEntry::ToolUse {
                                    id,
                                    name,
                                    input: input_str,
                                    timestamp: timestamp.clone(),
                                });
                            }
                        }
                    }
                }
            }
            _ => {}
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
                text,
                model,
                thinking,
                timestamp,
            } => {
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
                    Span::styled(
                        "🤖 CLAUDE",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
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
    // Rough fallback timestamp
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
    fn test_summarize_and_load_session() {
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

        let summaries = discover_sessions(Some(temp_dir.path()), Some(Path::new("/test/project")), false);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].session_id, "test-session-uuid");
        assert_eq!(summaries[0].title, "My Test Session");
        assert_eq!(summaries[0].turn_count, 3); // 2 user + 1 assistant

        let entries = load_session_log(&session_file).unwrap();
        assert_eq!(entries.len(), 4);
        assert!(matches!(&entries[0], LogEntry::User { text, .. } if text == "Fix the build error"));
        assert!(matches!(&entries[1], LogEntry::Assistant { text, model, .. } if text == "Sure, running cargo check." && model.as_deref() == Some("claude-opus-5")));
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
}
