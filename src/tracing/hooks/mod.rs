//! Hook channel: the CLIs' lifecycle hooks as a second trace source.
//!
//! Spec: `docs/superpowers/specs/2026-09-03-hook-channel-design.md`.
//!
//! `agent-mux trace hook <source>` receives one payload from a CLI hook,
//! normalizes it into a `HookEvent`, applies the content policy, and appends
//! it to `hook_events`. Parsers are lenient: a payload that does not carry
//! the fields an event needs yields `None`, never an error. Unknown events
//! are not stored.

pub mod feed;
pub mod install;
pub mod register;

use crate::config::{ContentMode, ResolvedTracing};
use crate::tracing::ids;
use crate::tracing::map::{mask_secrets, truncate_content};
use crate::transcript::Provider;
use serde_json::{Map, Value};

/// Which registration delivered the payload. Codex has two shapes: hook
/// events on stdin and the `notify` program's single JSON argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookSource {
    Claude,
    Codex,
    CodexNotify,
    Antigravity,
}

impl HookSource {
    pub fn parse(s: &str) -> Option<HookSource> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" => Some(HookSource::Claude),
            "codex" => Some(HookSource::Codex),
            "codex-notify" | "codex_notify" | "notify" => Some(HookSource::CodexNotify),
            "agy" | "antigravity" => Some(HookSource::Antigravity),
            _ => None,
        }
    }

    pub fn provider(&self) -> Provider {
        match self {
            HookSource::Claude => Provider::Claude,
            HookSource::Codex | HookSource::CodexNotify => Provider::Codex,
            HookSource::Antigravity => Provider::Antigravity,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            HookSource::Claude => "claude",
            HookSource::Codex => "codex",
            HookSource::CodexNotify => "codex-notify",
            HookSource::Antigravity => "agy",
        }
    }
}

/// One normalized hook event, ready for `hook_events`.
#[derive(Debug, Clone, PartialEq)]
pub struct HookEvent {
    pub provider: Provider,
    pub session_id: String,
    pub launch_id: Option<String>,
    /// Normalized event name: `SessionStart`, `UserPromptSubmit`,
    /// `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `SubagentStart`,
    /// `SubagentStop`, `Stop`, `StopFailure`, `PreCompact`, `PostCompact`,
    /// `PostModelSwitch`, `SessionEnd`, `Interrupt`, `TurnComplete`,
    /// `PreInvocation`, `PostInvocation`.
    pub event: String,
    /// Hook process wall clock.
    pub ts_ns: i64,
    pub cwd: Option<String>,
    pub transcript_path: Option<String>,
    /// Claude `prompt_id`, Codex `turn_id`, agy `invocationNum`.
    pub turn_key: Option<String>,
    pub tool_use_id: Option<String>,
    pub tool_name: Option<String>,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    /// agy `stepIdx`.
    pub step_index: Option<i64>,
    pub model: Option<String>,
    pub is_error: bool,
    /// Policy-applied, whitelisted fields only.
    pub payload: Map<String, Value>,
}

impl HookEvent {
    fn new(
        provider: Provider,
        session_id: String,
        event: &str,
        ts_ns: i64,
        launch_id: Option<String>,
    ) -> Self {
        HookEvent {
            provider,
            session_id,
            launch_id,
            event: event.to_string(),
            ts_ns,
            cwd: None,
            transcript_path: None,
            turn_key: None,
            tool_use_id: None,
            tool_name: None,
            agent_id: None,
            agent_type: None,
            step_index: None,
            model: None,
            is_error: false,
            payload: Map::new(),
        }
    }

    /// Identity used for deduplication: the tool call, the subagent, the
    /// turn, or the step when the event has one; the timestamp otherwise.
    fn identity(&self) -> String {
        if let Some(id) = &self.tool_use_id {
            return format!("tool:{id}");
        }
        if self.event.starts_with("Subagent")
            && let Some(id) = &self.agent_id
        {
            return format!("agent:{id}");
        }
        if let Some(k) = &self.turn_key {
            return format!("turn:{k}");
        }
        if let Some(s) = self.step_index {
            return format!("step:{s}");
        }
        format!("ts:{}", self.ts_ns)
    }

    /// Deterministic 32-hex key; re-delivered payloads converge on one row.
    pub fn key(&self) -> String {
        ids::trace_id_hex(&format!(
            "amx1|hook|{}|{}|{}|{}",
            self.provider.as_str(),
            self.session_id,
            self.event,
            self.identity()
        ))
    }

    pub fn payload_json(&self) -> String {
        Value::Object(self.payload.clone()).to_string()
    }
}

/// The same masking, truncation, and content-mode rules the transcript
/// path applies, for hook payload fields.
#[derive(Debug, Clone)]
pub struct ContentPolicy {
    pub mode: ContentMode,
    pub max_bytes: usize,
    pub redact_literals: Vec<String>,
}

impl ContentPolicy {
    pub fn from_resolved(r: &ResolvedTracing, mode_override: Option<ContentMode>) -> ContentPolicy {
        ContentPolicy {
            mode: mode_override.unwrap_or(r.content_mode),
            max_bytes: r.content_max_bytes,
            redact_literals: r.redact_literals.clone(),
        }
    }

    pub fn metadata() -> ContentPolicy {
        ContentPolicy {
            mode: ContentMode::Metadata,
            max_bytes: 0,
            redact_literals: Vec::new(),
        }
    }

    pub fn full() -> ContentPolicy {
        ContentPolicy {
            mode: ContentMode::Full,
            max_bytes: 65536,
            redact_literals: Vec::new(),
        }
    }

    /// Content: absent in metadata mode, masked and truncated in full mode.
    fn content(&self, v: &Value) -> Option<String> {
        if self.mode == ContentMode::Metadata || v.is_null() {
            return None;
        }
        let text = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if text.is_empty() {
            return None;
        }
        Some(truncate_content(
            &mask_secrets(&text, &self.redact_literals),
            self.max_bytes,
        ))
    }

    fn put_content(&self, payload: &mut Map<String, Value>, key: &str, v: Option<&Value>) {
        if let Some(text) = v.and_then(|v| self.content(v)) {
            payload.insert(key.to_string(), Value::String(text));
        }
    }
}

/// Structure (small scalars: reasons, sources, counts) flows in both modes.
fn put_structural(payload: &mut Map<String, Value>, key: &str, v: Option<&Value>) {
    match v {
        Some(Value::String(s)) if !s.is_empty() => {
            let short: String = s.chars().take(200).collect();
            payload.insert(key.to_string(), Value::String(short));
        }
        Some(Value::Number(n)) => {
            payload.insert(key.to_string(), Value::Number(n.clone()));
        }
        Some(Value::Bool(b)) => {
            payload.insert(key.to_string(), Value::Bool(*b));
        }
        _ => {}
    }
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn first_str(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| str_field(v, k))
}

fn first_value<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(*k)).filter(|x| !x.is_null())
}

/// Parses one payload. `event_override` names the event for sources whose
/// payload does not (agy). `launch_id` comes from `AGENT_MUX_SESSION_ID`
/// or the registration.
pub fn parse(
    source: HookSource,
    event_override: Option<&str>,
    payload: &Value,
    policy: &ContentPolicy,
    ts_ns: i64,
    launch_id: Option<String>,
) -> Option<HookEvent> {
    match source {
        HookSource::Claude => parse_claude(payload, policy, ts_ns, launch_id),
        HookSource::Codex => parse_codex(payload, policy, ts_ns, launch_id),
        HookSource::CodexNotify => parse_codex_notify(payload, policy, ts_ns, launch_id),
        HookSource::Antigravity => parse_agy(event_override?, payload, policy, ts_ns, launch_id),
    }
}

fn parse_claude(
    v: &Value,
    policy: &ContentPolicy,
    ts_ns: i64,
    launch_id: Option<String>,
) -> Option<HookEvent> {
    let event = str_field(v, "hook_event_name")?;
    let session_id = str_field(v, "session_id")?;
    let mut ev = HookEvent::new(Provider::Claude, session_id, &event, ts_ns, launch_id);
    ev.cwd = str_field(v, "cwd");
    ev.transcript_path = str_field(v, "transcript_path");
    ev.turn_key = str_field(v, "prompt_id");
    ev.agent_id = str_field(v, "agent_id");
    ev.agent_type = str_field(v, "agent_type");
    let p = &mut ev.payload;
    match event.as_str() {
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => {
            ev.tool_name = str_field(v, "tool_name");
            ev.tool_use_id = str_field(v, "tool_use_id");
            policy.put_content(p, "tool_input", v.get("tool_input"));
            policy.put_content(
                p,
                "tool_result",
                first_value(v, &["tool_response", "tool_result"]),
            );
            if event == "PostToolUseFailure" {
                ev.is_error = true;
                policy.put_content(p, "tool_error", first_value(v, &["tool_error", "error"]));
            }
        }
        "UserPromptSubmit" => {
            policy.put_content(p, "prompt", first_value(v, &["prompt", "user_message"]));
        }
        "Stop" => {
            policy.put_content(p, "last_assistant_message", v.get("last_assistant_message"));
            put_structural(p, "turn_number", v.get("turn_number"));
            put_structural(p, "stop_hook_active", v.get("stop_hook_active"));
        }
        "StopFailure" => {
            ev.is_error = true;
            put_structural(p, "error_type", v.get("error_type"));
        }
        "SubagentStart" => {}
        "SubagentStop" => {
            policy.put_content(p, "result", v.get("result"));
        }
        "SessionStart" => put_structural(p, "source", v.get("source")),
        "SessionEnd" => put_structural(p, "reason", v.get("reason")),
        "PreCompact" | "PostCompact" => {
            put_structural(
                p,
                "compaction_trigger",
                first_value(v, &["compaction_trigger", "trigger"]),
            );
        }
        "PostModelSwitch" => {
            ev.model = str_field(v, "new_model");
            put_structural(p, "old_model", v.get("old_model"));
        }
        _ => return None,
    }
    Some(ev)
}

fn parse_codex(
    v: &Value,
    policy: &ContentPolicy,
    ts_ns: i64,
    launch_id: Option<String>,
) -> Option<HookEvent> {
    let event = str_field(v, "hook_event_name")?;
    let session_id = str_field(v, "session_id")?;
    let mut ev = HookEvent::new(Provider::Codex, session_id, &event, ts_ns, launch_id);
    ev.cwd = str_field(v, "cwd");
    ev.transcript_path = str_field(v, "transcript_path");
    ev.model = str_field(v, "model");
    ev.turn_key = str_field(v, "turn_id");
    ev.agent_id = str_field(v, "agent_id");
    ev.agent_type = str_field(v, "agent_type");
    let p = &mut ev.payload;
    match event.as_str() {
        "PreToolUse" | "PostToolUse" => {
            ev.tool_name = str_field(v, "tool_name");
            ev.tool_use_id = str_field(v, "tool_use_id");
            policy.put_content(p, "tool_input", v.get("tool_input"));
            policy.put_content(
                p,
                "tool_result",
                first_value(v, &["tool_response", "tool_result"]),
            );
        }
        "UserPromptSubmit" => policy.put_content(p, "prompt", v.get("prompt")),
        "Stop" => {
            policy.put_content(p, "last_assistant_message", v.get("last_assistant_message"));
            put_structural(p, "stop_hook_active", v.get("stop_hook_active"));
        }
        "SubagentStart" => {}
        "SubagentStop" => {
            policy.put_content(p, "last_assistant_message", v.get("last_assistant_message"));
            put_structural(p, "agent_transcript_path", v.get("agent_transcript_path"));
        }
        "SessionStart" => put_structural(p, "source", v.get("source")),
        "SessionEnd" => put_structural(p, "reason", v.get("reason")),
        "PreCompact" | "PostCompact" => {
            put_structural(
                p,
                "compaction_trigger",
                first_value(v, &["trigger", "compaction_trigger"]),
            );
        }
        "Interrupt" => {}
        _ => return None,
    }
    Some(ev)
}

fn parse_codex_notify(
    v: &Value,
    policy: &ContentPolicy,
    ts_ns: i64,
    launch_id: Option<String>,
) -> Option<HookEvent> {
    if str_field(v, "type").as_deref() != Some("agent-turn-complete") {
        return None;
    }
    let session_id = first_str(v, &["thread-id", "thread_id"])?;
    let mut ev = HookEvent::new(
        Provider::Codex,
        session_id,
        "TurnComplete",
        ts_ns,
        launch_id,
    );
    ev.cwd = str_field(v, "cwd");
    ev.turn_key = first_str(v, &["turn-id", "turn_id"]);
    let p = &mut ev.payload;
    policy.put_content(
        p,
        "input_messages",
        first_value(v, &["input-messages", "input_messages"]),
    );
    policy.put_content(
        p,
        "last_assistant_message",
        first_value(v, &["last-assistant-message", "last_assistant_message"]),
    );
    put_structural(p, "client", v.get("client"));
    Some(ev)
}

fn parse_agy(
    event: &str,
    v: &Value,
    policy: &ContentPolicy,
    ts_ns: i64,
    launch_id: Option<String>,
) -> Option<HookEvent> {
    let session_id = str_field(v, "conversationId")?;
    let mut ev = HookEvent::new(Provider::Antigravity, session_id, event, ts_ns, launch_id);
    ev.transcript_path = str_field(v, "transcriptPath");
    ev.cwd = v
        .get("workspacePaths")
        .and_then(|w| w.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.as_str())
        .map(str::to_string);
    ev.model = str_field(v, "modelName");
    ev.step_index = v.get("stepIdx").and_then(|s| s.as_i64());
    let p = &mut ev.payload;
    match event {
        "PreToolUse" => {
            let call = v.get("toolCall")?;
            ev.tool_name = str_field(call, "name");
            policy.put_content(p, "tool_input", call.get("args"));
        }
        "PostToolUse" => {
            if let Some(err) = str_field(v, "error") {
                ev.is_error = true;
                policy.put_content(p, "tool_error", Some(&Value::String(err)));
            }
        }
        "PreInvocation" | "PostInvocation" => {
            ev.turn_key = v
                .get("invocationNum")
                .and_then(|n| n.as_i64())
                .map(|n| n.to_string());
            put_structural(p, "initial_num_steps", v.get("initialNumSteps"));
        }
        "Stop" => {
            put_structural(p, "termination_reason", v.get("terminationReason"));
            put_structural(p, "fully_idle", v.get("fullyIdle"));
            put_structural(p, "execution_num", v.get("executionNum"));
            if let Some(err) = str_field(v, "error") {
                ev.is_error = true;
                policy.put_content(p, "error", Some(&Value::String(err)));
            }
        }
        _ => return None,
    }
    Some(ev)
}

/// What the agy hook must print: agy requires a JSON object on stdout, and
/// `Stop` requires a `decision`, where any value other than `"continue"`
/// lets the agent stop.
pub fn agy_response(event: &str) -> &'static str {
    match event {
        "Stop" => r#"{"decision":"proceed"}"#,
        _ => "{}",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_post_tool() -> Value {
        serde_json::json!({
            "session_id": "sess-c1",
            "prompt_id": "prompt-7",
            "transcript_path": "/home/me/.claude/projects/-proj/sess-c1.jsonl",
            "cwd": "/proj",
            "permission_mode": "default",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_use_id": "toolu_01",
            "tool_input": {"command": "export KEY=sk-live-topsecret && cargo test"},
            "tool_response": {"stdout": "ok", "stderr": ""}
        })
    }

    #[test]
    fn claude_tool_event_normalizes_and_masks() {
        let ev = parse(
            HookSource::Claude,
            None,
            &claude_post_tool(),
            &ContentPolicy::full(),
            1_000,
            Some("launch-1".into()),
        )
        .unwrap();
        assert_eq!(ev.provider, Provider::Claude);
        assert_eq!(ev.session_id, "sess-c1");
        assert_eq!(ev.launch_id.as_deref(), Some("launch-1"));
        assert_eq!(ev.event, "PostToolUse");
        assert_eq!(ev.tool_name.as_deref(), Some("Bash"));
        assert_eq!(ev.tool_use_id.as_deref(), Some("toolu_01"));
        assert_eq!(ev.turn_key.as_deref(), Some("prompt-7"));
        assert_eq!(
            ev.transcript_path.as_deref(),
            Some("/home/me/.claude/projects/-proj/sess-c1.jsonl")
        );
        assert!(!ev.is_error);
        let input = ev.payload["tool_input"].as_str().unwrap();
        assert!(
            input.contains("[REDACTED]") && !input.contains("topsecret"),
            "{input}"
        );
        assert!(
            ev.payload["tool_result"]
                .as_str()
                .unwrap()
                .contains("\"stdout\":\"ok\"")
        );
        // metadata mode: names and ids only
        let meta = parse(
            HookSource::Claude,
            None,
            &claude_post_tool(),
            &ContentPolicy::metadata(),
            1_000,
            None,
        )
        .unwrap();
        assert!(meta.payload.is_empty(), "{:?}", meta.payload);
        assert_eq!(meta.tool_use_id.as_deref(), Some("toolu_01"));
    }

    #[test]
    fn claude_turn_session_and_subagent_events() {
        let policy = ContentPolicy::full();
        let stop = parse(
            HookSource::Claude,
            None,
            &serde_json::json!({"session_id":"s","hook_event_name":"Stop","prompt_id":"p1","last_assistant_message":"done","turn_number":3,"stop_hook_active":false}),
            &policy,
            5,
            None,
        )
        .unwrap();
        assert_eq!(stop.payload["last_assistant_message"], "done");
        assert_eq!(stop.payload["turn_number"], 3);
        let failure = parse(
            HookSource::Claude,
            None,
            &serde_json::json!({"session_id":"s","hook_event_name":"StopFailure","prompt_id":"p1","error_type":"rate_limit"}),
            &policy,
            6,
            None,
        )
        .unwrap();
        assert!(failure.is_error);
        assert_eq!(failure.payload["error_type"], "rate_limit");
        let tool_failure = parse(
            HookSource::Claude,
            None,
            &serde_json::json!({"session_id":"s","hook_event_name":"PostToolUseFailure","tool_name":"Bash","tool_use_id":"t9","tool_error":"exit 1"}),
            &policy,
            7,
            None,
        )
        .unwrap();
        assert!(tool_failure.is_error);
        assert_eq!(tool_failure.payload["tool_error"], "exit 1");
        let start = parse(
            HookSource::Claude,
            None,
            &serde_json::json!({"session_id":"s","hook_event_name":"SessionStart","source":"resume","transcript_path":"/t.jsonl"}),
            &policy,
            8,
            None,
        )
        .unwrap();
        assert_eq!(start.payload["source"], "resume");
        let sub = parse(
            HookSource::Claude,
            None,
            &serde_json::json!({"session_id":"s","hook_event_name":"SubagentStop","agent_id":"a1","agent_type":"Explore","result":"found it"}),
            &policy,
            9,
            None,
        )
        .unwrap();
        assert_eq!(sub.agent_id.as_deref(), Some("a1"));
        assert_eq!(sub.agent_type.as_deref(), Some("Explore"));
        let switch = parse(
            HookSource::Claude,
            None,
            &serde_json::json!({"session_id":"s","hook_event_name":"PostModelSwitch","new_model":"claude-opus-5","old_model":"claude-sonnet-5"}),
            &policy,
            10,
            None,
        )
        .unwrap();
        assert_eq!(switch.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(switch.payload["old_model"], "claude-sonnet-5");
        // out-of-scope and unknown events are not stored
        for name in [
            "Notification",
            "PermissionRequest",
            "MessageDisplay",
            "Whatever",
        ] {
            assert!(
                parse(
                    HookSource::Claude,
                    None,
                    &serde_json::json!({"session_id":"s","hook_event_name":name}),
                    &policy,
                    11,
                    None
                )
                .is_none(),
                "{name}"
            );
        }
        // missing session id: nothing
        assert!(
            parse(
                HookSource::Claude,
                None,
                &serde_json::json!({"hook_event_name":"Stop"}),
                &policy,
                12,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn codex_hook_and_notify_payloads() {
        let policy = ContentPolicy::full();
        let pre = parse(
            HookSource::Codex,
            None,
            &serde_json::json!({
                "session_id":"thread-9","cwd":"/proj","hook_event_name":"PreToolUse",
                "transcript_path":"/home/me/.codex/sessions/2026/09/03/rollout-2026-09-03T10-00-00-thread-9.jsonl",
                "model":"gpt-5.3-codex","permission_mode":"default","turn_id":"turn-2",
                "tool_name":"Bash","tool_use_id":"call_1","tool_input":{"command":"ls"}
            }),
            &policy,
            1,
            None,
        )
        .unwrap();
        assert_eq!(pre.provider, Provider::Codex);
        assert_eq!(pre.model.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(pre.turn_key.as_deref(), Some("turn-2"));
        assert_eq!(pre.tool_use_id.as_deref(), Some("call_1"));
        assert!(
            pre.transcript_path
                .as_deref()
                .unwrap()
                .ends_with("thread-9.jsonl")
        );
        let notify = parse(
            HookSource::CodexNotify,
            None,
            &serde_json::json!({
                "type":"agent-turn-complete","thread-id":"thread-9","turn-id":"turn-2","cwd":"/proj",
                "client":"codex-tui","input-messages":["do it"],"last-assistant-message":"done"
            }),
            &policy,
            2,
            Some("launch-2".into()),
        )
        .unwrap();
        assert_eq!(notify.event, "TurnComplete");
        assert_eq!(notify.session_id, "thread-9");
        assert_eq!(notify.turn_key.as_deref(), Some("turn-2"));
        assert_eq!(notify.payload["last_assistant_message"], "done");
        assert!(
            notify.payload["input_messages"]
                .as_str()
                .unwrap()
                .contains("do it")
        );
        assert!(
            parse(
                HookSource::CodexNotify,
                None,
                &serde_json::json!({"type":"something-else"}),
                &policy,
                3,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn agy_payloads_need_the_event_name_and_answer_the_contract() {
        let policy = ContentPolicy::full();
        let common = serde_json::json!({
            "conversationId":"conv-1","workspacePaths":["/proj"],
            "transcriptPath":"/home/me/.gemini/antigravity-cli/brain/conv-1/.system_generated/logs/transcript_full.jsonl",
            "modelName":"auto"
        });
        let mut post = common.clone();
        post["stepIdx"] = Value::from(5);
        post["error"] = Value::from("exit status 1");
        let ev = parse(
            HookSource::Antigravity,
            Some("PostToolUse"),
            &post,
            &policy,
            1,
            None,
        )
        .unwrap();
        assert_eq!(ev.provider, Provider::Antigravity);
        assert_eq!(ev.step_index, Some(5));
        assert!(ev.is_error);
        assert_eq!(ev.cwd.as_deref(), Some("/proj"));
        assert_eq!(ev.model.as_deref(), Some("auto"));
        let mut pre = common.clone();
        pre["stepIdx"] = Value::from(19);
        pre["toolCall"] =
            serde_json::json!({"name":"run_command","args":{"CommandLine":"npm test"}});
        let ev = parse(
            HookSource::Antigravity,
            Some("PreToolUse"),
            &pre,
            &policy,
            2,
            None,
        )
        .unwrap();
        assert_eq!(ev.tool_name.as_deref(), Some("run_command"));
        assert!(
            ev.payload["tool_input"]
                .as_str()
                .unwrap()
                .contains("npm test")
        );
        let mut inv = common.clone();
        inv["invocationNum"] = Value::from(3);
        let ev = parse(
            HookSource::Antigravity,
            Some("PreInvocation"),
            &inv,
            &policy,
            3,
            None,
        )
        .unwrap();
        assert_eq!(ev.turn_key.as_deref(), Some("3"));
        let mut stop = common.clone();
        stop["terminationReason"] = Value::from("model_stop");
        stop["fullyIdle"] = Value::from(true);
        let ev = parse(
            HookSource::Antigravity,
            Some("Stop"),
            &stop,
            &policy,
            4,
            None,
        )
        .unwrap();
        assert_eq!(ev.payload["termination_reason"], "model_stop");
        // no event name, no row
        assert!(parse(HookSource::Antigravity, None, &post, &policy, 5, None).is_none());
        assert_eq!(agy_response("Stop"), r#"{"decision":"proceed"}"#);
        assert_eq!(agy_response("PostToolUse"), "{}");
    }

    #[test]
    fn dedupe_keys_follow_the_event_identity() {
        let policy = ContentPolicy::full();
        let a = parse(
            HookSource::Claude,
            None,
            &claude_post_tool(),
            &policy,
            1,
            None,
        )
        .unwrap();
        let b = parse(
            HookSource::Claude,
            None,
            &claude_post_tool(),
            &policy,
            999,
            None,
        )
        .unwrap();
        assert_eq!(
            a.key(),
            b.key(),
            "same tool call re-delivered later converges"
        );
        let mut pre = claude_post_tool();
        pre["hook_event_name"] = Value::from("PreToolUse");
        let c = parse(HookSource::Claude, None, &pre, &policy, 1, None).unwrap();
        assert_ne!(a.key(), c.key(), "pre and post are different rows");
        let end1 = parse(
            HookSource::Claude,
            None,
            &serde_json::json!({"session_id":"s","hook_event_name":"SessionEnd","reason":"other"}),
            &policy,
            1,
            None,
        )
        .unwrap();
        let end2 = parse(
            HookSource::Claude,
            None,
            &serde_json::json!({"session_id":"s","hook_event_name":"SessionEnd","reason":"other"}),
            &policy,
            2,
            None,
        )
        .unwrap();
        assert_ne!(end1.key(), end2.key(), "identity-less events key on time");
        assert_eq!(a.key().len(), 32);
        assert_eq!(
            HookSource::parse("Codex-Notify"),
            Some(HookSource::CodexNotify)
        );
        assert_eq!(
            HookSource::parse("antigravity"),
            Some(HookSource::Antigravity)
        );
        assert_eq!(HookSource::parse("x"), None);
    }
}
