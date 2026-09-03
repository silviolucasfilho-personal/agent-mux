//! Shared per-line transcript parsers for the three agent CLIs.
//!
//! Two consumers with different needs sit on top of this module:
//! - the history viewer (`history.rs`), which adapts `TranscriptEvent`s into
//!   its display-oriented `LogEntry`s — bit-for-bit compatible with the
//!   pre-refactor parsers (pinned by the existing history tests);
//! - the trace store assembler (`tracing::map`), which needs the fields the
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
        /// Harness/system chatter written as a user line (Claude
        /// `isMeta: true`, `<task-notification>`, `<command-name>`,
        /// `<bash-stdout>` and friends). Not a real prompt: it must never
        /// open a new turn or become a trace name/input.
        meta: bool,
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
        /// Claude `attributionSkill`: the skill whose instructions this
        /// message was produced under.
        skill: Option<String>,
        /// Antigravity `step_index`: the join key for the per-request
        /// usage agy keeps outside the transcript.
        step_index: Option<u64>,
        ts: Option<String>,
    },
    ToolUse {
        /// Claude tool_use_id / Codex call_id; empty for Antigravity.
        id: String,
        name: String,
        /// Raw structured arguments (Null when the source had none).
        args: Value,
        /// Claude `attributionSkill` of the message issuing the call.
        skill: Option<String>,
        ts: Option<String>,
    },
    ToolResult {
        id: String,
        /// Raw content value — string, array, or Null.
        content: Value,
        is_error: bool,
        /// Claude top-level `toolUseResult` object: the rich structured
        /// result (Bash stdout/stderr/interrupted, Edit structuredPatch,
        /// Agent agentId/resolvedModel/outputFile, ...), when present.
        structured: Option<Value>,
        ts: Option<String>,
    },
    /// Standalone reasoning/thinking not attached to an assistant message
    /// (Codex `reasoning` items; Claude thinking-only messages).
    Thinking { text: String, ts: Option<String> },
    /// Usage reported separately from any assistant message (Codex
    /// `token_count`; Claude assistant messages with usage but no text).
    TokenCount {
        usage: Vec<(String, i64)>,
        /// See `Assistant::msg_id` — used to dedup repeated usage lines.
        msg_id: Option<String>,
        /// The model that produced the usage (Claude tool_use-only
        /// messages carry it) — lets the assembler mint a real generation
        /// so Langfuse cost inference applies.
        model: Option<String>,
        ts: Option<String>,
    },
    /// Claude `system`/`turn_duration` line: the CLI's own measured turn
    /// latency.
    TurnDuration {
        duration_ms: i64,
        message_count: Option<i64>,
        ts: Option<String>,
    },
    /// Claude `cost-state` line: the CLI's running session totals.
    CostState {
        total_cost_usd: Option<f64>,
        total_lines_added: Option<i64>,
        total_lines_removed: Option<i64>,
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
/// order). Langfuse usageDetails is a string->int map: strings like
/// Claude's `service_tier` are dropped, and nested objects of integers
/// (e.g. `cache_creation: {ephemeral_5m_input_tokens: n}`) are flattened
/// one level to `parent.child` keys instead of being lost.
fn integer_usage(usage: Option<&Value>) -> Vec<(String, i64)> {
    let Some(obj) = usage.and_then(|u| u.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (k, v) in obj {
        if let Some(n) = v.as_i64() {
            out.push((k.clone(), n));
        } else if let Some(sub) = v.as_object() {
            for (k2, v2) in sub {
                if let Some(n) = v2.as_i64() {
                    out.push((format!("{k}.{k2}"), n));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Claude Code: ~/.claude/projects/<slug>/<uuid>.jsonl
// ---------------------------------------------------------------------------

/// Harness-generated user-line prefixes: local command echoes, bash tool
/// output, async task notifications, injected reminders. They share the
/// user role on disk but are not prompts.
const CLAUDE_META_TAGS: [&str; 10] = [
    "<local-command-caveat>",
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<command-name>",
    "<command-message>",
    "<bash-input>",
    "<bash-stdout>",
    "<bash-stderr>",
    "<task-notification>",
    "<system-reminder>",
];

fn is_claude_meta_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    CLAUDE_META_TAGS.iter().any(|tag| trimmed.starts_with(tag))
}

fn tag_inner<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(&text[start..end])
}

/// A `<command-name>/spec-wave</command-name>` (+ optional
/// `<command-args>`) line is the user's actual action — a session driven
/// entirely by slash commands would otherwise have no prompt at all, and
/// its traces no name or input. Returns the command as prompt text
/// ("/spec-wave build the thing").
fn extract_claude_command(text: &str) -> Option<String> {
    let name = tag_inner(text, "command-name")?.trim();
    if name.is_empty() {
        return None;
    }
    let args = tag_inner(text, "command-args")
        .map(str::trim)
        .unwrap_or_default();
    Some(if args.is_empty() {
        name.to_string()
    } else {
        format!("{name} {args}")
    })
}

pub fn parse_claude_line(line: &str) -> Vec<TranscriptEvent> {
    let Some(v) = parse_json_line(line) else {
        return Vec::new();
    };
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string());
    let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let is_meta_line = v.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false);
    let skill = v
        .get("attributionSkill")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    let mut events = Vec::new();

    // Session context rides on every conversation line; surface it once per
    // thread root (parentUuid null) — the assembler dedups by key.
    if matches!(event_type, "user" | "assistant") && v.get("parentUuid").is_none_or(|p| p.is_null())
    {
        let mut extra = serde_json::Map::new();
        if let Some(branch) = v.get("gitBranch").and_then(|b| b.as_str())
            && !branch.is_empty()
        {
            extra.insert("git_branch".into(), Value::String(branch.to_string()));
        }
        if let Some(ver) = v.get("version").and_then(|b| b.as_str()) {
            extra.insert("cli_version".into(), Value::String(ver.to_string()));
        }
        if !extra.is_empty() {
            events.push(TranscriptEvent::SessionMeta {
                session_id: v
                    .get("sessionId")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string()),
                cwd: v.get("cwd").and_then(|c| c.as_str()).map(|c| c.to_string()),
                extra: Value::Object(extra),
                ts: ts.clone(),
            });
        }
    }

    match event_type {
        "user" => {
            if let Some(msg) = v.get("message") {
                // The rich structured result (top-level, beside `message`);
                // it describes this line's single tool_result item.
                let mut structured = v.get("toolUseResult").filter(|t| t.is_object()).cloned();
                if let Some(content_str) = msg.get("content").and_then(|c| c.as_str()) {
                    if let Some(cmd) = extract_claude_command(content_str) {
                        events.push(TranscriptEvent::User {
                            text: cmd,
                            meta: false,
                            ts: ts.clone(),
                        });
                    } else {
                        events.push(TranscriptEvent::User {
                            text: content_str.to_string(),
                            meta: is_meta_line || is_claude_meta_text(content_str),
                            ts: ts.clone(),
                        });
                    }
                } else if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                    for item in arr {
                        let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if item_type == "text" {
                            if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                                if let Some(cmd) = extract_claude_command(txt) {
                                    events.push(TranscriptEvent::User {
                                        text: cmd,
                                        meta: false,
                                        ts: ts.clone(),
                                    });
                                } else {
                                    events.push(TranscriptEvent::User {
                                        text: txt.to_string(),
                                        meta: is_meta_line || is_claude_meta_text(txt),
                                        ts: ts.clone(),
                                    });
                                }
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
                                structured: structured.take(),
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
                                        skill: skill.clone(),
                                        step_index: None,
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
                                    skill: skill.clone(),
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
                            model: model.clone(),
                            ts: ts.clone(),
                        });
                    }
                }
            }
        }
        "system" => {
            if v.get("subtype").and_then(|s| s.as_str()) == Some("turn_duration")
                && let Some(duration_ms) = v.get("durationMs").and_then(|d| d.as_i64())
            {
                events.push(TranscriptEvent::TurnDuration {
                    duration_ms,
                    message_count: v.get("messageCount").and_then(|m| m.as_i64()),
                    ts: ts.clone(),
                });
            }
        }
        "cost-state" => {
            events.push(TranscriptEvent::CostState {
                total_cost_usd: v.get("totalCostUSD").and_then(|c| c.as_f64()),
                total_lines_added: v.get("totalLinesAdded").and_then(|n| n.as_i64()),
                total_lines_removed: v.get("totalLinesRemoved").and_then(|n| n.as_i64()),
                ts: ts.clone(),
            });
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
                        meta: false,
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
            let model = v
                .get("model")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string());
            let thinking = v
                .get("thinking")
                .and_then(|t| t.as_str())
                .map(|t| t.to_string());
            let text = v
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let has_tool_calls = v
                .get("tool_calls")
                .and_then(|tc| tc.as_array())
                .is_some_and(|tc| !tc.is_empty());
            // a tool-call-only step is still one model request: it gets an
            // (empty-text) assistant event so its usage has a generation to
            // land on; the history viewer skips such entries
            if !text.is_empty() || thinking.is_some() || has_tool_calls {
                events.push(TranscriptEvent::Assistant {
                    text,
                    model,
                    thinking,
                    usage: Vec::new(),
                    msg_id: None,
                    skill: None,
                    step_index: v.get("step_index").and_then(|i| i.as_u64()),
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
                        skill: None,
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
                    structured: None,
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
        // per-turn context: the only place a Codex rollout names its model
        "turn_context" => {
            let mut extra = serde_json::Map::new();
            if let Some(model) = payload.get("model").and_then(|m| m.as_str()) {
                extra.insert("model".into(), Value::String(model.to_string()));
            }
            if !extra.is_empty() {
                events.push(TranscriptEvent::SessionMeta {
                    session_id: None,
                    cwd: payload
                        .get("cwd")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                    extra: Value::Object(extra),
                    ts,
                });
            }
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
                            events.push(TranscriptEvent::User {
                                text,
                                meta: false,
                                ts,
                            });
                        }
                        "assistant" if !text.is_empty() => {
                            events.push(TranscriptEvent::Assistant {
                                text,
                                model: None,
                                thinking: None,
                                usage: Vec::new(),
                                msg_id: None,
                                skill: None,
                                step_index: None,
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
                        skill: None,
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
                        skill: None,
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
                        skill: None,
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
                        skill: None,
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
                        structured: None,
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
                    let usage =
                        integer_usage(payload.get("info").and_then(|i| i.get("last_token_usage")));
                    if !usage.is_empty() {
                        events.push(TranscriptEvent::TokenCount {
                            usage,
                            msg_id: None,
                            model: None,
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
                skill,
                step_index: None,
                ts,
            } => {
                assert_eq!(text, "hello");
                assert_eq!(model.as_deref(), Some("claude-fable-5"));
                assert_eq!(thinking.as_deref(), Some("hmm"));
                assert_eq!(skill.as_deref(), None);
                // integer keys kept, nested integer objects flattened to
                // parent.child, strings dropped
                let mut sorted = usage.clone();
                sorted.sort();
                assert_eq!(
                    sorted,
                    vec![
                        ("cache_creation.x".to_string(), 1),
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
        assert!(
            matches!(&events[0], TranscriptEvent::Thinking { text, .. } if text == "only thoughts")
        );
        assert!(
            matches!(&events[1], TranscriptEvent::TokenCount { usage, .. } if usage == &vec![("output_tokens".to_string(), 7)])
        );
    }

    #[test]
    fn claude_meta_user_lines_are_flagged() {
        // isMeta flag
        let caveat = r#"{"type":"user","isMeta":true,"message":{"content":"<local-command-caveat>Caveat: local commands</local-command-caveat>"}}"#;
        assert!(matches!(
            &parse_claude_line(caveat)[0],
            TranscriptEvent::User { meta: true, .. }
        ));
        // tag-prefixed content without the flag
        for tag in [
            "<task-notification>\n<task-id>x</task-id>",
            "<local-command-stdout>Set model to Opus</local-command-stdout>",
            "  <bash-stdout>ok</bash-stdout>",
            "<system-reminder>recalled</system-reminder>",
        ] {
            let line = format!(
                r#"{{"type":"user","message":{{"content":{}}}}}"#,
                serde_json::to_string(tag).unwrap()
            );
            assert!(
                matches!(
                    &parse_claude_line(&line)[0],
                    TranscriptEvent::User { meta: true, .. }
                ),
                "tag {tag} must flag meta"
            );
        }
        // a real prompt stays non-meta
        let real = r#"{"type":"user","message":{"content":"fix the bug"}}"#;
        assert!(matches!(
            &parse_claude_line(real)[0],
            TranscriptEvent::User { meta: false, .. }
        ));
    }

    #[test]
    fn claude_slash_commands_become_real_prompts() {
        // tag order varies between CLI versions; both must extract
        let with_args = r#"{"type":"user","message":{"content":"<command-message>spec-wave</command-message>\n<command-name>/spec-wave</command-name>\n<command-args>plan the login epic</command-args>"}}"#;
        assert!(matches!(
            &parse_claude_line(with_args)[0],
            TranscriptEvent::User { text, meta: false, .. } if text == "/spec-wave plan the login epic"
        ));
        let no_args = r#"{"type":"user","message":{"content":"<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args></command-args>"}}"#;
        assert!(matches!(
            &parse_claude_line(no_args)[0],
            TranscriptEvent::User { text, meta: false, .. } if text == "/model"
        ));
        // its stdout echo stays meta
        let stdout = r#"{"type":"user","message":{"content":"<local-command-stdout>Set model</local-command-stdout>"}}"#;
        assert!(matches!(
            &parse_claude_line(stdout)[0],
            TranscriptEvent::User { meta: true, .. }
        ));
    }

    #[test]
    fn claude_tool_use_result_rides_the_tool_result_event() {
        let line = r#"{"type":"user","timestamp":"2026-08-30T10:00:00Z","toolUseResult":{"stdout":"ok\n","stderr":"","interrupted":false,"isImage":false},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#;
        match &parse_claude_line(line)[0] {
            TranscriptEvent::ToolResult { id, structured, .. } => {
                assert_eq!(id, "t1");
                let s = structured.as_ref().expect("structured result missing");
                assert_eq!(s["stdout"], "ok\n");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn claude_attribution_skill_lands_on_assistant_and_tool_use() {
        let line = r#"{"type":"assistant","attributionSkill":"spec-wave","message":{"model":"m","content":[{"type":"text","text":"per the spec"},{"type":"tool_use","id":"t1","name":"Edit","input":{}}]}}"#;
        let events = parse_claude_line(line);
        assert!(
            matches!(&events[0], TranscriptEvent::Assistant { skill: Some(s), .. } if s == "spec-wave")
        );
        assert!(
            matches!(&events[1], TranscriptEvent::ToolUse { skill: Some(s), .. } if s == "spec-wave")
        );
    }

    #[test]
    fn claude_tool_only_token_count_carries_model() {
        let line = r#"{"type":"assistant","message":{"model":"claude-fable-5","id":"msg_1","usage":{"input_tokens":9},"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#;
        let events = parse_claude_line(line);
        assert!(
            matches!(&events[1], TranscriptEvent::TokenCount { model: Some(m), .. } if m == "claude-fable-5")
        );
    }

    #[test]
    fn claude_turn_duration_and_cost_state_lines() {
        let dur = r#"{"type":"system","subtype":"turn_duration","durationMs":3644107,"messageCount":214,"timestamp":"2026-08-30T23:34:39Z"}"#;
        assert!(matches!(
            &parse_claude_line(dur)[0],
            TranscriptEvent::TurnDuration {
                duration_ms: 3644107,
                message_count: Some(214),
                ..
            }
        ));
        // other system subtypes stay invisible
        let away = r#"{"type":"system","subtype":"away_summary","timestamp":"t"}"#;
        assert!(parse_claude_line(away).is_empty());
        let cost = r#"{"type":"cost-state","totalCostUSD":1.25,"totalLinesAdded":10,"totalLinesRemoved":2}"#;
        match &parse_claude_line(cost)[0] {
            TranscriptEvent::CostState {
                total_cost_usd,
                total_lines_added,
                total_lines_removed,
                ..
            } => {
                assert_eq!(*total_cost_usd, Some(1.25));
                assert_eq!(*total_lines_added, Some(10));
                assert_eq!(*total_lines_removed, Some(2));
            }
            other => panic!("expected CostState, got {other:?}"),
        }
    }

    #[test]
    fn claude_thread_root_line_surfaces_git_branch_and_version() {
        let line = r#"{"type":"user","parentUuid":null,"sessionId":"sess-1","cwd":"/proj","gitBranch":"fix/thing","version":"2.1.251","message":{"content":"go"}}"#;
        let events = parse_claude_line(line);
        match &events[0] {
            TranscriptEvent::SessionMeta {
                session_id,
                cwd,
                extra,
                ..
            } => {
                assert_eq!(session_id.as_deref(), Some("sess-1"));
                assert_eq!(cwd.as_deref(), Some("/proj"));
                assert_eq!(extra["git_branch"], "fix/thing");
                assert_eq!(extra["cli_version"], "2.1.251");
            }
            other => panic!("expected SessionMeta, got {other:?}"),
        }
        assert!(matches!(&events[1], TranscriptEvent::User { .. }));
        // non-root lines don't repeat it
        let child = r#"{"type":"user","parentUuid":"u1","gitBranch":"fix/thing","version":"2.1.251","message":{"content":"more"}}"#;
        assert_eq!(parse_claude_line(child).len(), 1);
    }

    #[test]
    fn codex_turn_context_names_the_model() {
        let line = r#"{"timestamp":"t","type":"turn_context","payload":{"cwd":"/proj","model":"gpt-5.3-codex","approval_policy":"never"}}"#;
        match &parse_codex_line(line)[0] {
            TranscriptEvent::SessionMeta { extra, cwd, .. } => {
                assert_eq!(extra["model"], "gpt-5.3-codex");
                assert_eq!(cwd.as_deref(), Some("/proj"));
            }
            other => panic!("expected SessionMeta, got {other:?}"),
        }
    }

    #[test]
    fn claude_array_tool_result_content_is_preserved_structurally() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"file contents"}],"is_error":false}]}}"#;
        let events = parse_claude_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            TranscriptEvent::ToolResult { id, content, .. } => {
                assert_eq!(id, "t1");
                assert!(
                    content.is_array(),
                    "array content must survive: {content:?}"
                );
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
        let generic =
            r#"{"type":"GENERIC","created_at":"2026-08-30T19:01:40Z","content":"Exit code 1"}"#;
        let events = parse_antigravity_line(generic);
        assert!(matches!(
            &events[0],
            TranscriptEvent::ToolResult { is_error: true, .. }
        ));
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
        for prefix in [
            "<user_instructions>",
            "<environment_context>",
            "<turn_context>",
        ] {
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
            TranscriptEvent::ToolUse { args, .. } => {
                assert_eq!(args, &Value::String("not json".into()))
            }
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
            let line =
                format!(r#"{{"timestamp":"t","type":"event_msg","payload":{{"type":"{wire}"}}}}"#);
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
