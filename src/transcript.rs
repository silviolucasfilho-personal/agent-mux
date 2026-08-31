//! Shared per-line transcript parsers for the three agent CLIs.
//!
//! Two consumers with different needs sit on top of this module:
//! - the history viewer (`history.rs`), which adapts `TranscriptEvent`s into
//!   its display-oriented `LogEntry`s — bit-for-bit compatible with the
//!   pre-refactor parsers (pinned by the existing history tests);
//! - the Langfuse exporter (`langfuse::map`), which needs the fields the
//!   viewer historically dropped: token usage, structured tool args/results,
//!   thinking, and Codex turn boundaries.
//!
//! Parsing is lenient `serde_json::Value`-based, matching the house style:
//! malformed lines and unknown event types yield no events, never errors.

use serde_json::Value;

/// Which coding agent wrote the transcript line format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Claude,
    Codex,
    Antigravity,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Antigravity => "antigravity",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnBoundaryKind {
    Start,
    Complete,
    Aborted,
}

/// One normalized transcript event. `ts` is the raw ISO-8601 string from the
/// file (`timestamp` / `created_at` / rollout `timestamp`), if present.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptEvent {
    User {
        text: String,
        ts: Option<String>,
    },
    Assistant {
        text: String,
        model: Option<String>,
        thinking: Option<String>,
        /// Integer-valued usage keys only (e.g. input_tokens); non-integer
        /// members of the source usage object are dropped.
        usage: Vec<(String, i64)>,
        /// The API message id (Claude `message.id`). A multi-line message
        /// repeats identical usage on every line; the assembler charges each
        /// id once.
        msg_id: Option<String>,
        ts: Option<String>,
    },
    ToolUse {
        /// Claude tool_use_id / Codex call_id; empty for Antigravity.
        id: String,
        name: String,
        /// Raw structured arguments (Null when the source had none).
        args: Value,
        ts: Option<String>,
    },
    ToolResult {
        id: String,
        /// Raw content value — string, array, or Null.
        content: Value,
        is_error: bool,
        ts: Option<String>,
    },
    /// Standalone reasoning/thinking not attached to an assistant message
    /// (Codex `reasoning` items; Claude thinking-only messages).
    Thinking {
        text: String,
        ts: Option<String>,
    },
    /// Usage reported separately from any assistant message (Codex
    /// `token_count`; Claude assistant messages with usage but no text).
    TokenCount {
        usage: Vec<(String, i64)>,
        /// See `Assistant::msg_id` — used to dedup repeated usage lines.
        msg_id: Option<String>,
        ts: Option<String>,
    },
    /// Codex explicit turn boundary (`task_started` / `task_complete` /
    /// `turn_aborted`).
    TurnBoundary {
        kind: TurnBoundaryKind,
        ts: Option<String>,
    },
    /// Codex rollout `session_meta` (always the first line of a rollout).
    SessionMeta {
        session_id: Option<String>,
        cwd: Option<String>,
        /// Selected metadata (cli_version, model_provider, git...) as found.
        extra: Value,
        ts: Option<String>,
    },
}

/// Sniffs the provider from a transcript's first line.
pub fn detect_provider(first_line: &str) -> Provider {
    if first_line.contains("\"step_index\"") || first_line.contains("USER_EXPLICIT") {
        Provider::Antigravity
    } else if first_line.contains("\"session_meta\"") {
        Provider::Codex
    } else {
        Provider::Claude
    }
}

pub fn parse_line(provider: Provider, line: &str) -> Vec<TranscriptEvent> {
    match provider {
        Provider::Claude => parse_claude_line(line),
        Provider::Codex => parse_codex_line(line),
        Provider::Antigravity => parse_antigravity_line(line),
    }
}

/// RFC3339 (offsets and fractional seconds included) -> unix epoch
/// nanoseconds. `None` on anything unparseable.
pub fn parse_rfc3339_nanos(ts: &str) -> Option<i128> {
    time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|t| t.unix_timestamp_nanos())
}

/// Extracts inner text from `<USER_REQUEST>...</USER_REQUEST>` if present.
pub fn extract_user_request(raw: &str) -> String {
    if let Some(start) = raw.find("<USER_REQUEST>") {
        let after = &raw[start + "<USER_REQUEST>".len()..];
        if let Some(end) = after.find("</USER_REQUEST>") {
            return after[..end].trim().to_string();
        }
    }
    raw.trim().to_string()
}

fn parse_json_line(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

/// Integer-valued entries of a JSON object (key-sorted, serde_json's map
/// order). Non-integer members (nested objects, strings) are dropped —
/// Langfuse usageDetails is a string->int map, and keys like Claude's
/// `cache_creation` object or `service_tier` string must never reach it.
fn integer_usage(usage: Option<&Value>) -> Vec<(String, i64)> {
    let Some(obj) = usage.and_then(|u| u.as_object()) else {
        return Vec::new();
    };
    obj.iter()
        .filter_map(|(k, v)| v.as_i64().map(|n| (k.clone(), n)))
        .collect()
}

// ---------------------------------------------------------------------------
// Claude Code: ~/.claude/projects/<slug>/<uuid>.jsonl
// ---------------------------------------------------------------------------

pub fn parse_claude_line(line: &str) -> Vec<TranscriptEvent> {
    let Some(v) = parse_json_line(line) else {
        return Vec::new();
    };
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string());
    let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let mut events = Vec::new();

    match event_type {
        "user" => {
            if let Some(msg) = v.get("message") {
                if let Some(content_str) = msg.get("content").and_then(|c| c.as_str()) {
                    events.push(TranscriptEvent::User {
                        text: content_str.to_string(),
                        ts: ts.clone(),
                    });
                } else if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                    for item in arr {
                        let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if item_type == "text" {
                            if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                                events.push(TranscriptEvent::User {
                                    text: txt.to_string(),
                                    ts: ts.clone(),
                                });
                            }
                        } else if item_type == "tool_result" {
                            events.push(TranscriptEvent::ToolResult {
                                id: item
                                    .get("tool_use_id")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                content: item.get("content").cloned().unwrap_or(Value::Null),
                                is_error: item
                                    .get("is_error")
                                    .and_then(|e| e.as_bool())
                                    .unwrap_or(false),
                                ts: ts.clone(),
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
                let msg_id = msg
                    .get("id")
                    .and_then(|m| m.as_str())
                    .map(|m| m.to_string());
                let mut usage = integer_usage(msg.get("usage"));
                // Thinking content blocks precede text in a message; gather
                // them so the first text item carries the message's thinking.
                let mut pending_thinking: Vec<String> = Vec::new();
                let mut saw_text = false;
                if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                    for item in arr {
                        let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match item_type {
                            "thinking" => {
                                if let Some(th) = item.get("thinking").and_then(|t| t.as_str()) {
                                    pending_thinking.push(th.to_string());
                                }
                            }
                            "text" => {
                                if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                                    let thinking = if pending_thinking.is_empty() {
                                        None
                                    } else {
                                        Some(std::mem::take(&mut pending_thinking).join("\n"))
                                    };
                                    events.push(TranscriptEvent::Assistant {
                                        text: txt.to_string(),
                                        model: model.clone(),
                                        thinking,
                                        // usage attaches to the message's
                                        // first text item only
                                        usage: std::mem::take(&mut usage),
                                        msg_id: msg_id.clone(),
                                        ts: ts.clone(),
                                    });
                                    saw_text = true;
                                }
                            }
                            "tool_use" => {
                                events.push(TranscriptEvent::ToolUse {
                                    id: item
                                        .get("id")
                                        .and_then(|t| t.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    name: item
                                        .get("name")
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("unknown")
                                        .to_string(),
                                    args: item.get("input").cloned().unwrap_or(Value::Null),
                                    ts: ts.clone(),
                                });
                            }
                            _ => {}
                        }
                    }
                }
                // Message-level leftovers the viewer ignores but the
                // exporter wants: thinking-only messages, and usage on
                // messages without a text item (e.g. tool_use-only).
                if !saw_text {
                    if !pending_thinking.is_empty() {
                        events.push(TranscriptEvent::Thinking {
                            text: pending_thinking.join("\n"),
                            ts: ts.clone(),
                        });
                    }
                    if !usage.is_empty() {
                        events.push(TranscriptEvent::TokenCount {
                            usage,
                            msg_id: msg_id.clone(),
                            ts: ts.clone(),
                        });
                    }
                }
            }
        }
        _ => {}
    }
    events
}

// ---------------------------------------------------------------------------
// Antigravity: ~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/
//              transcript_full.jsonl (or transcript.jsonl)
// ---------------------------------------------------------------------------

/// Extracts model name from `<USER_SETTINGS_CHANGE>` if present.
fn extract_antigravity_model(raw: &str) -> Option<String> {
    if let Some(start) = raw.find("Model Selection` from None to ") {
        let rest = &raw[start + "Model Selection` from None to ".len()..];
        if let Some(end) = rest.find('.') {
            let m = rest[..end].trim();
            if !m.is_empty() {
                return Some(m.to_string());
            }
        }
    }
    if let Some(start) = raw.find("Model Selection` to ") {
        let rest = &raw[start + "Model Selection` to ".len()..];
        if let Some(end) = rest.find('.') {
            let m = rest[..end].trim();
            if !m.is_empty() {
                return Some(m.to_string());
            }
        }
    }
    None
}

pub fn parse_antigravity_line(line: &str) -> Vec<TranscriptEvent> {
    let Some(v) = parse_json_line(line) else {
        return Vec::new();
    };
    let ts = v
        .get("created_at")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string());
    let step_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let mut events = Vec::new();

    match step_type {
        "USER_INPUT" => {
            if let Some(content) = v.get("content").and_then(|c| c.as_str()) {
                let clean = extract_user_request(content);
                let model = extract_antigravity_model(content);
                if !clean.is_empty() {
                    events.push(TranscriptEvent::User {
                        text: clean,
                        ts: ts.clone(),
                    });
                }
                if let Some(m) = model {
                    events.push(TranscriptEvent::SessionMeta {
                        session_id: None,
                        cwd: None,
                        extra: serde_json::json!({ "model": m }),
                        ts: ts.clone(),
                    });
                }
            }
        }
        "PLANNER_RESPONSE" => {
            let model = v.get("model").and_then(|m| m.as_str()).map(|s| s.to_string());
            let thinking = v
                .get("thinking")
                .and_then(|t| t.as_str())
                .map(|t| t.to_string());
            let text = v
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            if !text.is_empty() || thinking.is_some() {
                events.push(TranscriptEvent::Assistant {
                    text,
                    model,
                    thinking,
                    usage: Vec::new(),
                    msg_id: None,
                    ts: ts.clone(),
                });
            }
            if let Some(tool_calls) = v.get("tool_calls").and_then(|tc| tc.as_array()) {
                for tc in tool_calls {
                    events.push(TranscriptEvent::ToolUse {
                        id: String::new(),
                        name: tc
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        args: tc.get("args").cloned().unwrap_or(Value::Null),
                        ts: ts.clone(),
                    });
                }
            }
        }
        "GENERIC" => {
            if let Some(content) = v.get("content").and_then(|c| c.as_str()) {
                let is_error = content.contains("error")
                    || content.contains("Exit code") && !content.contains("Exit code 0");
                events.push(TranscriptEvent::ToolResult {
                    id: String::new(),
                    content: Value::String(content.to_string()),
                    is_error,
                    ts: ts.clone(),
                });
            }
        }
        _ => {}
    }
    events
}

// ---------------------------------------------------------------------------
// Codex: ~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl
// Line shape: {"timestamp": "...", "ordinal": n?, "type": "...", "payload": {...}}
// ---------------------------------------------------------------------------

/// Codex writes environment/instruction prologues as user-role messages at
/// session start (and per turn); they are context, not conversation.
fn is_codex_session_prefix(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("<user_instructions>")
        || trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<turn_context>")
}

/// Concatenated `text` fields of a Responses-API content array.
fn codex_content_text(content: Option<&Value>) -> String {
    let Some(arr) = content.and_then(|c| c.as_array()) else {
        return String::new();
    };
    arr.iter()
        .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join("")
}

pub fn parse_codex_line(line: &str) -> Vec<TranscriptEvent> {
    let Some(v) = parse_json_line(line) else {
        return Vec::new();
    };
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string());
    let line_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let Some(payload) = v.get("payload") else {
        return Vec::new();
    };
    let mut events = Vec::new();

    match line_type {
        "session_meta" => {
            let mut extra = serde_json::Map::new();
            for key in ["cli_version", "model_provider", "originator", "git"] {
                if let Some(val) = payload.get(key) {
                    extra.insert(key.to_string(), val.clone());
                }
            }
            events.push(TranscriptEvent::SessionMeta {
                // codex before May 2026 wrote only `id`; upstream's own
                // deserializer backfills session_id from it — mirror that
                session_id: payload
                    .get("session_id")
                    .or_else(|| payload.get("id"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string()),
                cwd: payload
                    .get("cwd")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string()),
                extra: Value::Object(extra),
                ts,
            });
        }
        "response_item" => {
            let item_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_type {
                "message" => {
                    let role = payload.get("role").and_then(|r| r.as_str()).unwrap_or("");
                    let text = codex_content_text(payload.get("content"));
                    match role {
                        // codex session-prologue items arrive as user-role
                        // messages; they are context, not conversation (the
                        // official Langfuse codex plugin filters the same)
                        "user" if !text.is_empty() && !is_codex_session_prefix(&text) => {
                            events.push(TranscriptEvent::User { text, ts });
                        }
                        "assistant" if !text.is_empty() => {
                            events.push(TranscriptEvent::Assistant {
                                text,
                                model: None,
                                thinking: None,
                                usage: Vec::new(),
                                msg_id: None,
                                ts,
                            });
                        }
                        // system / developer messages are not conversation
                        _ => {}
                    }
                }
                "reasoning" => {
                    let text = codex_content_text(payload.get("summary"));
                    if !text.is_empty() {
                        events.push(TranscriptEvent::Thinking { text, ts });
                    }
                }
                "function_call" => {
                    let raw_args = payload
                        .get("arguments")
                        .and_then(|a| a.as_str())
                        .unwrap_or("");
                    // arguments is a JSON *string*; parse when possible
                    let args = serde_json::from_str(raw_args)
                        .unwrap_or_else(|_| Value::String(raw_args.to_string()));
                    events.push(TranscriptEvent::ToolUse {
                        id: payload
                            .get("call_id")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: payload
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        args,
                        ts,
                    });
                }
                "custom_tool_call" => {
                    events.push(TranscriptEvent::ToolUse {
                        id: payload
                            .get("call_id")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: payload
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        args: payload.get("input").cloned().unwrap_or(Value::Null),
                        ts,
                    });
                }
                "local_shell_call" => {
                    events.push(TranscriptEvent::ToolUse {
                        id: payload
                            .get("call_id")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: "local_shell".to_string(),
                        args: payload.get("action").cloned().unwrap_or(Value::Null),
                        ts,
                    });
                }
                "web_search_call" => {
                    events.push(TranscriptEvent::ToolUse {
                        id: payload
                            .get("call_id")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: "web_search".to_string(),
                        args: payload.get("action").cloned().unwrap_or(Value::Null),
                        ts,
                    });
                }
                "function_call_output" | "custom_tool_call_output" => {
                    events.push(TranscriptEvent::ToolResult {
                        id: payload
                            .get("call_id")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string(),
                        content: payload.get("output").cloned().unwrap_or(Value::Null),
                        is_error: false,
                        ts,
                    });
                }
                _ => {}
            }
        }
        "event_msg" => {
            let event_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match event_type {
                "token_count" => {
                    let usage = integer_usage(
                        payload
                            .get("info")
                            .and_then(|i| i.get("last_token_usage")),
                    );
                    if !usage.is_empty() {
                        events.push(TranscriptEvent::TokenCount {
                            usage,
                            msg_id: None,
                            ts,
                        });
                    }
                }
                "task_started" => events.push(TranscriptEvent::TurnBoundary {
                    kind: TurnBoundaryKind::Start,
                    ts,
                }),
                "task_complete" => events.push(TranscriptEvent::TurnBoundary {
                    kind: TurnBoundaryKind::Complete,
                    ts,
                }),
                "turn_aborted" => events.push(TranscriptEvent::TurnBoundary {
                    kind: TurnBoundaryKind::Aborted,
                    ts,
                }),
                _ => {}
            }
        }
        _ => {}
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_all_three_providers() {
        assert_eq!(
            detect_provider(r#"{"step_index":0,"source":"USER_EXPLICIT"}"#),
            Provider::Antigravity
        );
        assert_eq!(
            detect_provider(r#"{"timestamp":"t","type":"session_meta","payload":{}}"#),
            Provider::Codex
        );
        assert_eq!(
            detect_provider(r#"{"type":"user","message":{}}"#),
            Provider::Claude
        );
    }

    #[test]
    fn rfc3339_parsing_covers_z_offset_and_fraction() {
        let z = parse_rfc3339_nanos("2026-08-30T19:28:18.806Z").unwrap();
        assert_eq!(z, 1_788_118_098_806_000_000);
        let plain = parse_rfc3339_nanos("2026-08-30T22:40:25Z").unwrap();
        assert!(plain > z);
        let offset = parse_rfc3339_nanos("2026-08-30T16:01:40-03:00").unwrap();
        assert_eq!(offset, parse_rfc3339_nanos("2026-08-30T19:01:40Z").unwrap());
        assert!(parse_rfc3339_nanos("not a time").is_none());
    }

    #[test]
    fn malformed_and_empty_lines_yield_nothing() {
        assert!(parse_claude_line("").is_empty());
        assert!(parse_claude_line("not json").is_empty());
        assert!(parse_codex_line("{\"type\":\"response_item\"}").is_empty()); // no payload
        assert!(parse_antigravity_line("{}").is_empty());
    }

    #[test]
    fn claude_assistant_with_usage_thinking_and_synthetic_model() {
        let line = r#"{"type":"assistant","timestamp":"2026-08-30T10:00:00Z","message":{"model":"claude-fable-5","usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":5,"cache_creation":{"x":1},"service_tier":"standard"},"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"hello"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#;
        let events = parse_claude_line(line);
        assert_eq!(events.len(), 2);
        match &events[0] {
            TranscriptEvent::Assistant {
                text,
                model,
                thinking,
                usage,
                msg_id: _,
                ts,
            } => {
                assert_eq!(text, "hello");
                assert_eq!(model.as_deref(), Some("claude-fable-5"));
                assert_eq!(thinking.as_deref(), Some("hmm"));
                // integer keys only; objects and strings dropped
                let mut sorted = usage.clone();
                sorted.sort();
                assert_eq!(
                    sorted,
                    vec![
                        ("cache_read_input_tokens".to_string(), 5),
                        ("input_tokens".to_string(), 100),
                        ("output_tokens".to_string(), 20),
                    ]
                );
                assert_eq!(ts.as_deref(), Some("2026-08-30T10:00:00Z"));
            }
            other => panic!("expected Assistant, got {other:?}"),
        }
        match &events[1] {
            TranscriptEvent::ToolUse { id, name, args, .. } => {
                assert_eq!(id, "t1");
                assert_eq!(name, "Bash");
                assert_eq!(args["command"], "ls");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn claude_thinking_only_message_yields_thinking_and_usage_events() {
        let line = r#"{"type":"assistant","message":{"model":"m","usage":{"output_tokens":7},"content":[{"type":"thinking","thinking":"only thoughts"}]}}"#;
        let events = parse_claude_line(line);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], TranscriptEvent::Thinking { text, .. } if text == "only thoughts"));
        assert!(
            matches!(&events[1], TranscriptEvent::TokenCount { usage, .. } if usage == &vec![("output_tokens".to_string(), 7)])
        );
    }

    #[test]
    fn claude_array_tool_result_content_is_preserved_structurally() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"file contents"}],"is_error":false}]}}"#;
        let events = parse_claude_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            TranscriptEvent::ToolResult { id, content, .. } => {
                assert_eq!(id, "t1");
                assert!(content.is_array(), "array content must survive: {content:?}");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn antigravity_planner_response_keeps_structured_args() {
        let line = r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-08-30T19:01:38Z","thinking":"think","content":"reply","tool_calls":[{"name":"run_command","args":{"CommandLine":"\"cargo test\"","Cwd":"\"/tmp\""}}]}"#;
        let events = parse_antigravity_line(line);
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            TranscriptEvent::Assistant { text, thinking, model: None, .. }
                if text == "reply" && thinking.as_deref() == Some("think")
        ));
        match &events[1] {
            TranscriptEvent::ToolUse { id, name, args, .. } => {
                assert_eq!(id, "");
                assert_eq!(name, "run_command");
                assert_eq!(args["CommandLine"], "\"cargo test\"");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn antigravity_user_request_extraction_and_generic_error() {
        let user = r#"{"type":"USER_INPUT","source":"USER_EXPLICIT","created_at":"2026-08-30T19:01:37Z","content":"<USER_REQUEST>\ndo the thing\n</USER_REQUEST>"}"#;
        let events = parse_antigravity_line(user);
        assert!(matches!(&events[0], TranscriptEvent::User { text, .. } if text == "do the thing"));
        let generic = r#"{"type":"GENERIC","created_at":"2026-08-30T19:01:40Z","content":"Exit code 1"}"#;
        let events = parse_antigravity_line(generic);
        assert!(matches!(&events[0], TranscriptEvent::ToolResult { is_error: true, .. }));
    }

    // -- Codex fixtures (source-derived from openai/codex rollout schema) --

    #[test]
    fn codex_session_meta_carries_id_and_cwd() {
        let line = r#"{"timestamp":"2026-08-30T10:00:00.000Z","type":"session_meta","payload":{"session_id":"abc-123","id":"thread-1","cwd":"/home/user/proj","originator":"codex_cli_rs","cli_version":"0.151.0","model_provider":"openai"}}"#;
        let events = parse_codex_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            TranscriptEvent::SessionMeta {
                session_id,
                cwd,
                extra,
                ..
            } => {
                assert_eq!(session_id.as_deref(), Some("abc-123"));
                assert_eq!(cwd.as_deref(), Some("/home/user/proj"));
                assert_eq!(extra["cli_version"], "0.151.0");
            }
            other => panic!("expected SessionMeta, got {other:?}"),
        }
    }

    #[test]
    fn codex_session_meta_id_only_backfills_session_id() {
        // codex before May 2026 wrote only `id` (no `session_id`)
        let line = r#"{"timestamp":"2026-08-30T10:00:00.000Z","type":"session_meta","payload":{"id":"019dbf7d-old","cwd":"/home/user/proj","cli_version":"0.124.0"}}"#;
        match &parse_codex_line(line)[0] {
            TranscriptEvent::SessionMeta { session_id, .. } => {
                assert_eq!(session_id.as_deref(), Some("019dbf7d-old"));
            }
            other => panic!("expected SessionMeta, got {other:?}"),
        }
    }

    #[test]
    fn codex_session_prologue_user_messages_are_not_conversation() {
        for prefix in ["<user_instructions>", "<environment_context>", "<turn_context>"] {
            let line = format!(
                r#"{{"timestamp":"t","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{prefix} lots of context {}"}}]}}}}"#,
                prefix.replace('<', "</")
            );
            assert!(
                parse_codex_line(&line).is_empty(),
                "prologue {prefix} must be filtered"
            );
        }
    }

    #[test]
    fn codex_messages_reasoning_and_function_calls() {
        let user = r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fix the bug"}]}}"#;
        assert!(
            matches!(&parse_codex_line(user)[0], TranscriptEvent::User { text, .. } if text == "fix the bug")
        );
        let asst = r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}"#;
        assert!(
            matches!(&parse_codex_line(asst)[0], TranscriptEvent::Assistant { text, .. } if text == "done")
        );
        let system = r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"system","content":[{"type":"input_text","text":"instructions"}]}}"#;
        assert!(parse_codex_line(system).is_empty());
        let reasoning = r#"{"timestamp":"t","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"thinking..."}],"encrypted_content":"xxx"}}"#;
        assert!(
            matches!(&parse_codex_line(reasoning)[0], TranscriptEvent::Thinking { text, .. } if text == "thinking...")
        );
        // arguments is a JSON string and must be parsed to structure
        let call = r#"{"timestamp":"t","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"ls\"]}","call_id":"c1"}}"#;
        match &parse_codex_line(call)[0] {
            TranscriptEvent::ToolUse { id, name, args, .. } => {
                assert_eq!(id, "c1");
                assert_eq!(name, "shell");
                assert_eq!(args["command"][0], "ls");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
        let unparseable = r#"{"timestamp":"t","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"not json","call_id":"c2"}}"#;
        match &parse_codex_line(unparseable)[0] {
            TranscriptEvent::ToolUse { args, .. } => assert_eq!(args, &Value::String("not json".into())),
            other => panic!("expected ToolUse, got {other:?}"),
        }
        let output = r#"{"timestamp":"t","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"ok"}}"#;
        assert!(
            matches!(&parse_codex_line(output)[0], TranscriptEvent::ToolResult { id, content, .. } if id == "c1" && content == &Value::String("ok".into()))
        );
        let shell = r#"{"timestamp":"t","type":"response_item","payload":{"type":"local_shell_call","call_id":"c3","status":"completed","action":{"type":"exec","command":["echo","hi"]}}}"#;
        assert!(
            matches!(&parse_codex_line(shell)[0], TranscriptEvent::ToolUse { name, .. } if name == "local_shell")
        );
    }

    #[test]
    fn codex_token_count_and_turn_boundaries() {
        let tc = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":500,"output_tokens":50,"cached_input_tokens":100,"cache_write_input_tokens":0,"reasoning_output_tokens":10,"total_tokens":550},"last_token_usage":{"input_tokens":200,"cached_input_tokens":50,"cache_write_input_tokens":0,"output_tokens":20,"reasoning_output_tokens":5,"total_tokens":220}},"rate_limits":null}}"#;
        match &parse_codex_line(tc)[0] {
            TranscriptEvent::TokenCount { usage, .. } => {
                let get = |k: &str| usage.iter().find(|(key, _)| key == k).map(|(_, v)| *v);
                assert_eq!(get("input_tokens"), Some(200));
                assert_eq!(get("output_tokens"), Some(20));
                assert_eq!(get("reasoning_output_tokens"), Some(5));
            }
            other => panic!("expected TokenCount, got {other:?}"),
        }
        for (wire, kind) in [
            ("task_started", TurnBoundaryKind::Start),
            ("task_complete", TurnBoundaryKind::Complete),
            ("turn_aborted", TurnBoundaryKind::Aborted),
        ] {
            let line = format!(r#"{{"timestamp":"t","type":"event_msg","payload":{{"type":"{wire}"}}}}"#);
            assert!(
                matches!(&parse_codex_line(&line)[0], TranscriptEvent::TurnBoundary { kind: k, .. } if *k == kind),
                "boundary {wire}"
            );
        }
        // unknown event types are skipped
        let unknown = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"something_new"}}"#;
        assert!(parse_codex_line(unknown).is_empty());
        let unknown_line = r#"{"timestamp":"t","type":"world_state","payload":{}}"#;
        assert!(parse_codex_line(unknown_line).is_empty());
    }
}
