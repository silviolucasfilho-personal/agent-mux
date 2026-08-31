//! Turn assembler: a stream of `TranscriptEvent`s in, completed OTLP spans
//! out. One Langfuse trace per user turn, grouped into a Langfuse session by
//! the CLI's own conversation id; plus a per-session lifecycle trace
//! (`session_started` / `session_ended` event spans) keyed by the launch id.
//!
//! Content policy lives here too: in `Metadata` mode no prompt, response,
//! thinking, or tool bodies are ever attached; in `Full` mode every content
//! string passes through the secret-pattern masker and UTF-8-safe
//! truncation.

use crate::config::ContentMode;
use crate::langfuse::otlp::{
    self, AnyValue, KeyValue, Span, STATUS_ERROR, STATUS_UNSET, attr, str_attr,
};
use crate::transcript::{Provider, TranscriptEvent, TurnBoundaryKind, parse_rfc3339_nanos};

/// Everything the assembler needs to know about one session's mapping.
#[derive(Debug, Clone)]
pub struct MapSettings {
    pub provider: Provider,
    pub content_mode: ContentMode,
    pub content_max_bytes: usize,
    pub redact_literals: Vec<String>,
    pub user_id: Option<String>,
    pub release: Option<String>,
    pub tags: Vec<String>,
    pub environment: Option<String>,
    pub profile_name: String,
    pub cwd: String,
    pub project_slug: String,
    pub agent_mux_session: usize,
    pub launch_id: String,
}

/// Substring prefixes whose following token is masked in full mode. Matches
/// are boundary-checked (preceding char must not be alphanumeric) so e.g.
/// "task-123" survives while "sk-live-..." is masked.
const SECRET_PREFIXES: [&str; 6] = ["sk-", "pk-lf-", "AKIA", "ghp_", "xox", "Bearer "];

fn is_boundary(prev: Option<char>) -> bool {
    match prev {
        None => true,
        Some(c) => !c.is_ascii_alphanumeric(),
    }
}

/// Masks secret-shaped substrings: known token prefixes (the token following
/// the prefix is replaced), `-----BEGIN ...` blocks (masked through the
/// matching END line or to the end), and user-supplied literals.
pub fn mask_secrets(text: &str, literals: &[String]) -> String {
    let mut out = text.to_string();
    for lit in literals {
        if !lit.is_empty() {
            out = out.replace(lit.as_str(), "[REDACTED]");
        }
    }
    // -----BEGIN ...----- blocks: mask through the end of the END line
    // (a plain '\n' scan also covers CRLF), or to the end of the string
    while let Some(start) = out.find("-----BEGIN") {
        let rest = &out[start..];
        let end = rest
            .find("-----END")
            .map(|e| rest[e..].find('\n').map(|nl| e + nl + 1).unwrap_or(rest.len()))
            .unwrap_or(rest.len());
        out.replace_range(start..start + end, "[REDACTED KEY BLOCK]");
    }
    for prefix in SECRET_PREFIXES {
        let mut search_from = 0;
        while let Some(rel) = out[search_from..].find(prefix) {
            let at = search_from + rel;
            let prev = out[..at].chars().next_back();
            if !is_boundary(prev) {
                search_from = at + prefix.len();
                continue;
            }
            let token_start = at + prefix.len();
            let token_len = out[token_start..]
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ')' | ']' | '}' | ',' | ';'))
                .unwrap_or(out.len() - token_start);
            if token_len == 0 {
                search_from = token_start;
                continue;
            }
            out.replace_range(token_start..token_start + token_len, "[REDACTED]");
            search_from = token_start + "[REDACTED]".len();
        }
    }
    out
}

/// UTF-8-boundary-safe truncation with a marker.
pub fn truncate_content(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}…[truncated {} bytes]",
        &text[..cut],
        text.len() - cut
    )
}

/// Sums `add` into `target` per key.
fn accumulate_usage(target: &mut Vec<(String, i64)>, add: &[(String, i64)]) {
    for (k, v) in add {
        if let Some(entry) = target.iter_mut().find(|(tk, _)| tk == k) {
            entry.1 += v;
        } else {
            target.push((k.clone(), *v));
        }
    }
}

struct OpenTool {
    id: String,
    name: String,
    args: serde_json::Value,
    start_nanos: i128,
    ts_approx: bool,
    /// The event index at ToolUse time — the tool's identity in span-id
    /// derivation (the emission-time counter would collide).
    event_index: u64,
}

struct PendingGen {
    text: String,
    model: Option<String>,
    thinking: Option<String>,
    usage: Vec<(String, i64)>,
    start_nanos: i128,
    end_nanos: i128,
    ts_approx: bool,
}

struct OpenTurn {
    ordinal: u64,
    trace_key: String,
    trace_id: [u8; 16],
    root_span_id: [u8; 8],
    start_nanos: i128,
    last_nanos: i128,
    user_text: Option<String>,
    last_assistant_text: Option<String>,
    open_tools: Vec<OpenTool>,
    /// Codex only: generations buffered until turn close so the late
    /// per-turn `token_count` usage can attach to the final one. Other
    /// providers emit generations as they complete.
    pending_generations: Vec<PendingGen>,
    pending_turn_usage: Vec<(String, i64)>,
    pending_thinking: Vec<String>,
    /// API message ids whose usage has already been charged in this turn —
    /// a multi-line Claude message repeats identical usage on every line.
    seen_usage_msg_ids: std::collections::HashSet<String>,
    event_index: u64,
    /// Emitted-generation cursor: keeps `{trace_key}|gen|{i}` span keys
    /// deterministic across the immediate and buffered emission paths.
    gen_count: usize,
    any_ts_approx: bool,
    aborted: bool,
    /// True for the turn left open when the prime pass ended: it is history
    /// and must not be emitted, and live events never join it — they drop
    /// it and open a fresh turn instead.
    stale: bool,
}

pub struct TurnAssembler {
    settings: MapSettings,
    /// The CLI's own conversation/session id, once known.
    session_id: Option<String>,
    /// "deterministic" | "watched" | "heuristic" | "none"
    correlation: String,
    /// The adopted transcript path, for trace metadata.
    transcript_path: Option<String>,
    ordinal: u64,
    /// Set when the prime pass was truncated by backfill_max_bytes: trace-id
    /// keys gain a launch salt since the since-file-start count is unknowable.
    ordinal_salted: bool,
    emitting: bool,
    explicit_boundaries: bool,
    turn: Option<OpenTurn>,
    last_event_nanos: i128,
    session_extra: Vec<(String, String)>,
    last_known_model: Option<String>,
}

impl TurnAssembler {
    pub fn new(settings: MapSettings, session_id: Option<String>, correlation: &str) -> Self {
        TurnAssembler {
            settings,
            session_id,
            correlation: correlation.to_string(),
            transcript_path: None,
            ordinal: 0,
            ordinal_salted: false,
            emitting: true,
            explicit_boundaries: false,
            turn: None,
            last_event_nanos: 0,
            session_extra: Vec::new(),
            last_known_model: None,
        }
    }

    pub fn set_session_id(&mut self, id: &str, correlation: &str) {
        self.session_id = Some(id.to_string());
        self.correlation = correlation.to_string();
    }

    pub fn set_transcript_path(&mut self, path: &str) {
        self.transcript_path = Some(path.to_string());
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Prime mode: state updates only, nothing emitted (resumed transcripts
    /// must never re-export history). Turning emitting back on marks any
    /// still-open primed turn stale so its eventual close stays silent.
    pub fn set_emitting(&mut self, emitting: bool) {
        if emitting && !self.emitting
            && let Some(turn) = self.turn.as_mut()
        {
            turn.stale = true;
        }
        self.emitting = emitting;
    }

    /// Marks the prime pass as byte-truncated: ordinals no longer count from
    /// file start, so trace-id keys get the launch salt.
    pub fn mark_backfill_truncated(&mut self) {
        self.ordinal_salted = true;
    }

    fn full_content(&self, text: &str) -> Option<String> {
        match self.settings.content_mode {
            ContentMode::Metadata => None,
            ContentMode::Full => Some(truncate_content(
                &mask_secrets(text, &self.settings.redact_literals),
                self.settings.content_max_bytes,
            )),
        }
    }

    fn session_key(&self) -> &str {
        self.session_id.as_deref().unwrap_or(&self.settings.launch_id)
    }

    fn trace_key_for_ordinal(&self, ordinal: u64) -> String {
        let base = format!(
            "amx1|{}|{}|turn|{ordinal}",
            self.settings.provider.as_str(),
            self.session_key()
        );
        if self.ordinal_salted {
            format!("{base}|{}", self.settings.launch_id)
        } else {
            base
        }
    }

    /// Attributes every emitted span carries — session/user identity AND the
    /// trace metadata — so Langfuse grouping and filtering survive a lost
    /// turn root.
    fn common_attrs(&self) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(id) = &self.session_id {
            attrs.push(str_attr("langfuse.session.id", id.clone()));
        }
        if let Some(user) = &self.settings.user_id {
            attrs.push(str_attr("langfuse.user.id", user.clone()));
        }
        attrs.extend(self.trace_metadata_attrs());
        attrs
    }

    fn trace_metadata_attrs(&self) -> Vec<KeyValue> {
        let s = &self.settings;
        let mut attrs = vec![
            attr(
                "langfuse.trace.metadata.agent_mux_session",
                AnyValue::Int(s.agent_mux_session as i64),
            ),
            str_attr("langfuse.trace.metadata.launch_id", s.launch_id.clone()),
            str_attr("langfuse.trace.metadata.profile", s.profile_name.clone()),
            str_attr("langfuse.trace.metadata.cwd", s.cwd.clone()),
            str_attr("langfuse.trace.metadata.project_slug", s.project_slug.clone()),
            str_attr("langfuse.trace.metadata.provider", s.provider.as_str()),
            str_attr(
                "langfuse.trace.metadata.agent_mux_version",
                env!("CARGO_PKG_VERSION"),
            ),
            str_attr("langfuse.trace.metadata.correlation", self.correlation.clone()),
        ];
        if let Some(path) = &self.transcript_path {
            attrs.push(str_attr("langfuse.trace.metadata.transcript_path", path.clone()));
        }
        for (k, v) in &self.session_extra {
            attrs.push(str_attr(&format!("langfuse.trace.metadata.{k}"), v.clone()));
        }
        attrs
    }

    fn tag_attrs(&self) -> KeyValue {
        let mut tags = vec![
            AnyValue::Str("agent-mux".into()),
            AnyValue::Str(self.settings.provider.as_str().into()),
        ];
        tags.extend(self.settings.tags.iter().map(|t| AnyValue::Str(t.clone())));
        attr("langfuse.trace.tags", AnyValue::Array(tags))
    }

    fn event_nanos(&mut self, ts: &Option<String>, recv_nanos: i128) -> (i128, bool) {
        match ts.as_deref().and_then(parse_rfc3339_nanos) {
            Some(n) => {
                self.last_event_nanos = n;
                (n, false)
            }
            None => (recv_nanos, true),
        }
    }

    fn open_turn(&mut self, start_nanos: i128) {
        self.ordinal += 1;
        let trace_key = self.trace_key_for_ordinal(self.ordinal);
        let trace_id = otlp::trace_id_for(&trace_key);
        let root_span_id = otlp::span_id_for(&format!("{trace_key}|root"));
        self.turn = Some(OpenTurn {
            ordinal: self.ordinal,
            trace_key,
            trace_id,
            root_span_id,
            start_nanos,
            last_nanos: start_nanos,
            user_text: None,
            last_assistant_text: None,
            open_tools: Vec::new(),
            pending_generations: Vec::new(),
            pending_turn_usage: Vec::new(),
            pending_thinking: Vec::new(),
            seen_usage_msg_ids: std::collections::HashSet::new(),
            event_index: 0,
            gen_count: 0,
            any_ts_approx: false,
            aborted: false,
            stale: false,
        });
    }

    /// A stale (primed-history) turn never receives live events: it is
    /// dropped silently and a fresh turn opens. Used by every non-User arm
    /// so post-resume tails always land in a live turn.
    fn ensure_live_turn(&mut self, nanos: i128) {
        if self.turn.as_ref().is_some_and(|t| t.stale) {
            self.turn = None;
        }
        if self.turn.is_none() {
            self.open_turn(nanos);
        }
    }

    fn gen_span(&self, turn: &OpenTurn, index: usize, generation: &PendingGen) -> Span {
        let span_key = format!("{}|gen|{index}", turn.trace_key);
        let mut attrs = self.common_attrs();
        attrs.push(str_attr("langfuse.observation.type", "generation"));
        match &generation.model {
            Some(m) if m != "<synthetic>" => {
                attrs.push(str_attr("gen_ai.request.model", m.clone()));
            }
            Some(m) => {
                // "<synthetic>" would mint bogus generations and defeat
                // Langfuse's model-based cost inference
                attrs.push(str_attr("langfuse.observation.metadata.model", m.clone()));
            }
            None => {}
        }
        for (k, v) in &generation.usage {
            attrs.push(attr(&format!("gen_ai.usage.{k}"), AnyValue::Int(*v)));
        }
        if let Some(inp) = turn.user_text.as_ref().and_then(|t| self.full_content(t)) {
            attrs.push(str_attr("langfuse.observation.input", inp));
        }
        if let Some(out) = self.full_content(&generation.text) {
            attrs.push(str_attr("langfuse.observation.output", out));
        }
        if let Some(th) = generation
            .thinking
            .as_ref()
            .and_then(|t| self.full_content(t))
        {
            attrs.push(str_attr("langfuse.observation.metadata.thinking", th));
        }
        if generation.ts_approx {
            attrs.push(str_attr("langfuse.observation.metadata.ts", "approx"));
        }
        Span {
            trace_id: turn.trace_id,
            span_id: otlp::span_id_for(&span_key),
            parent_span_id: Some(turn.root_span_id),
            name: "assistant".into(),
            start_nanos: generation.start_nanos,
            end_nanos: generation.end_nanos,
            attributes: attrs,
            status_code: STATUS_UNSET,
            status_message: None,
        }
    }

    fn close_turn(&mut self) -> Vec<Span> {
        let Some(mut turn) = self.turn.take() else {
            return Vec::new();
        };
        let mut spans = Vec::new();
        if !self.emitting || turn.stale {
            return spans;
        }
        let end_nanos = turn.last_nanos;

        // Buffered (Codex) generations: the late per-turn usage attaches to
        // the final one.
        let buffered = turn.pending_generations.len();
        if buffered > 0 && !turn.pending_turn_usage.is_empty() {
            let last = &mut turn.pending_generations[buffered - 1];
            if last.usage.is_empty() {
                last.usage = std::mem::take(&mut turn.pending_turn_usage);
            }
        }
        let pending: Vec<PendingGen> = std::mem::take(&mut turn.pending_generations);
        for generation in &pending {
            let index = turn.gen_count;
            turn.gen_count += 1;
            spans.push(self.gen_span(&turn, index, generation));
        }
        let had_generations = turn.gen_count > 0;

        // Unpaired tools close with the turn.
        let unpaired: Vec<OpenTool> = std::mem::take(&mut turn.open_tools);
        for tool in &unpaired {
            spans.push(self.tool_span(&turn, tool, end_nanos, None, false, true));
        }

        // Root span.
        let mut attrs = self.common_attrs();
        let name = match self.settings.content_mode {
            ContentMode::Full => match &turn.user_text {
                // Mask BEFORE slicing: the trace name is the most visible
                // string in the Langfuse UI and must never carry a secret.
                Some(text) => {
                    let masked = mask_secrets(text, &self.settings.redact_literals);
                    let first: String = masked.chars().take(80).collect();
                    format!("{}: {first}", self.settings.profile_name)
                }
                None => format!("turn {}", turn.ordinal),
            },
            ContentMode::Metadata => format!("turn {}", turn.ordinal),
        };
        attrs.push(str_attr("langfuse.trace.name", name.clone()));
        attrs.push(self.tag_attrs());
        if let Some(release) = &self.settings.release {
            attrs.push(str_attr("langfuse.release", release.clone()));
        }
        if let Some(env) = &self.settings.environment {
            attrs.push(str_attr("langfuse.environment", env.clone()));
        }
        if !turn.pending_turn_usage.is_empty() {
            // no generation took it (e.g. tool-only turn): keep it on the root
            for (k, v) in &turn.pending_turn_usage {
                attrs.push(attr(&format!("gen_ai.usage.{k}"), AnyValue::Int(*v)));
            }
        }
        if let Some(input) = turn.user_text.as_ref().and_then(|t| self.full_content(t)) {
            attrs.push(str_attr("langfuse.trace.input", input));
        }
        if let Some(output) = turn
            .last_assistant_text
            .as_ref()
            .and_then(|t| self.full_content(t))
        {
            attrs.push(str_attr("langfuse.trace.output", output));
        }
        if !turn.pending_thinking.is_empty()
            && let Some(th) = self.full_content(&turn.pending_thinking.join("\n"))
        {
            attrs.push(str_attr("langfuse.trace.metadata.thinking", th));
        }
        // Generation start times are always approximated from the previous
        // event's timestamp (transcript lines are written at completion), so
        // any turn containing a generation is flagged — not just missing-ts.
        if turn.any_ts_approx || had_generations {
            attrs.push(str_attr("langfuse.trace.metadata.timing", "approximate"));
        }
        if turn.aborted {
            attrs.push(str_attr("langfuse.trace.metadata.terminated", "aborted"));
        }
        spans.push(Span {
            trace_id: turn.trace_id,
            span_id: turn.root_span_id,
            parent_span_id: None,
            name: format!("turn {}", turn.ordinal),
            start_nanos: turn.start_nanos,
            end_nanos,
            attributes: attrs,
            status_code: STATUS_UNSET,
            status_message: None,
        });
        spans
    }

    #[allow(clippy::too_many_arguments)]
    fn tool_span(
        &self,
        turn: &OpenTurn,
        tool: &OpenTool,
        end_nanos: i128,
        content: Option<&serde_json::Value>,
        is_error: bool,
        unpaired: bool,
    ) -> Span {
        // Keyed to the ToolUse event's own index (+ call id): stable for a
        // paired-vs-unpaired close, and distinct for same-name tools.
        let span_key = format!(
            "{}|tool|{}|{}|{}",
            turn.trace_key, tool.name, tool.id, tool.event_index
        );
        let mut attrs = self.common_attrs();
        let mut span_name = tool.name.clone();
        let mut obs_type = "tool";

        if let Some(agent) = extract_agent_info(&tool.name, &tool.args) {
            obs_type = "agent";
            span_name = agent.name;
            if let Some(role) = agent.role {
                attrs.push(str_attr("langfuse.observation.metadata.agent_role", role));
            }
            if let Some(type_name) = agent.type_name {
                attrs.push(str_attr("langfuse.observation.metadata.agent_type", type_name));
            }
            if let Some(model) = agent.model {
                attrs.push(str_attr("langfuse.observation.metadata.agent_model", model));
            }
            if self.settings.content_mode == ContentMode::Full
                && let Some(prompt) = &agent.prompt
                && let Some(p) = self.full_content(prompt)
            {
                attrs.push(str_attr("langfuse.observation.metadata.agent_prompt", p));
            }
            attrs.push(str_attr("langfuse.observation.metadata.kind", "agent_invocation"));
        } else if let Some((skill_name, skill_path)) = extract_skill_info(&tool.name, &tool.args) {
            span_name = format!("skill: {skill_name}");
            attrs.push(str_attr("langfuse.observation.metadata.skill_name", skill_name));
            if !skill_path.is_empty() {
                attrs.push(str_attr("langfuse.observation.metadata.skill_path", skill_path));
            }
            attrs.push(str_attr("langfuse.observation.metadata.kind", "skill_load"));
        }

        if let Some(summary) = tool.args.get("toolSummary").or_else(|| tool.args.get("summary")).and_then(clean_json_str) {
            attrs.push(str_attr("langfuse.observation.metadata.summary", summary));
        }
        if let Some(action) = tool.args.get("toolAction").or_else(|| tool.args.get("action")).or_else(|| tool.args.get("Description")).and_then(clean_json_str) {
            attrs.push(str_attr("langfuse.observation.metadata.action", action));
        }
        if let Some(cmd) = tool.args.get("CommandLine").or_else(|| tool.args.get("command")).or_else(|| tool.args.get("cmd")).and_then(clean_json_str) {
            attrs.push(str_attr("langfuse.observation.metadata.command", cmd));
        }
        if let Some(query) = tool.args.get("Query").or_else(|| tool.args.get("query")).or_else(|| tool.args.get("pattern")).and_then(clean_json_str) {
            attrs.push(str_attr("langfuse.observation.metadata.query", query));
        }
        if let Some(path) = tool.args.get("AbsolutePath").or_else(|| tool.args.get("TargetFile")).or_else(|| tool.args.get("DirectoryPath")).or_else(|| tool.args.get("path")).or_else(|| tool.args.get("file_path")).and_then(clean_json_str) {
            attrs.push(str_attr("langfuse.observation.metadata.path", path));
        }

        attrs.push(str_attr("langfuse.observation.type", obs_type));
        if self.settings.content_mode == ContentMode::Full {
            let args_str = match &tool.args {
                serde_json::Value::Null => String::new(),
                serde_json::Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            if !args_str.is_empty()
                && let Some(input) = self.full_content(&args_str)
            {
                attrs.push(str_attr("langfuse.observation.input", input));
            }
            if let Some(c) = content {
                let out_str = match c {
                    serde_json::Value::String(s) => s.clone(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };
                if let Some(output) = self.full_content(&out_str) {
                    attrs.push(str_attr("langfuse.observation.output", output));
                }
            }
        }
        if tool.ts_approx {
            attrs.push(str_attr("langfuse.observation.metadata.ts", "approx"));
        }
        let status_message = if unpaired {
            Some("no result observed".to_string())
        } else if is_error {
            Some("tool error".to_string())
        } else {
            None
        };
        Span {
            trace_id: turn.trace_id,
            span_id: otlp::span_id_for(&span_key),
            parent_span_id: Some(turn.root_span_id),
            name: span_name,
            start_nanos: tool.start_nanos,
            end_nanos,
            attributes: attrs,
            status_code: if is_error { STATUS_ERROR } else { STATUS_UNSET },
            status_message,
        }
    }

    /// Feeds one event. `recv_nanos` is the wall-clock fallback for events
    /// with a missing/unparseable timestamp.
    pub fn feed(&mut self, event: TranscriptEvent, recv_nanos: i128) -> Vec<Span> {
        let mut spans = Vec::new();
        match event {
            TranscriptEvent::SessionMeta {
                session_id,
                cwd: _,
                extra,
                ts,
            } => {
                let _ = self.event_nanos(&ts, recv_nanos);
                if self.session_id.is_none()
                    && let Some(id) = session_id
                {
                    self.session_id = Some(id);
                }
                if let Some(obj) = extra.as_object() {
                    if let Some(m) = obj.get("model").and_then(|v| v.as_str()) {
                        self.last_known_model = Some(m.to_string());
                    }
                    for (k, v) in obj {
                        let val = match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => serde_json::to_string(other).unwrap_or_default(),
                        };
                        self.session_extra.push((k.clone(), val));
                    }
                }
            }
            TranscriptEvent::TurnBoundary { kind, ts } => {
                let (nanos, _) = self.event_nanos(&ts, recv_nanos);
                self.explicit_boundaries = true;
                match kind {
                    TurnBoundaryKind::Start => {
                        spans.extend(self.close_turn());
                        self.open_turn(nanos);
                    }
                    TurnBoundaryKind::Complete => {
                        if let Some(turn) = self.turn.as_mut() {
                            turn.last_nanos = nanos;
                        }
                        spans.extend(self.close_turn());
                    }
                    TurnBoundaryKind::Aborted => {
                        if let Some(turn) = self.turn.as_mut() {
                            turn.last_nanos = nanos;
                            turn.aborted = true;
                        }
                        spans.extend(self.close_turn());
                    }
                }
            }
            TranscriptEvent::User { text, ts } => {
                let (nanos, approx) = self.event_nanos(&ts, recv_nanos);
                if self.explicit_boundaries {
                    if self.turn.as_ref().is_some_and(|t| t.stale) {
                        self.turn = None; // primed history: dropped silently
                    }
                    if self.turn.is_none() {
                        self.open_turn(nanos);
                    }
                } else {
                    spans.extend(self.close_turn());
                    self.open_turn(nanos);
                }
                if let Some(turn) = self.turn.as_mut() {
                    turn.last_nanos = nanos;
                    turn.any_ts_approx |= approx;
                    turn.event_index += 1;
                    if turn.user_text.is_none() {
                        turn.user_text = Some(text);
                    }
                }
            }
            TranscriptEvent::Assistant {
                text,
                model,
                thinking,
                mut usage,
                msg_id,
                ts,
            } => {
                let prev_nanos = self.last_event_nanos;
                let (nanos, approx) = self.event_nanos(&ts, recv_nanos);
                // assistant with no preceding user (e.g. resumed tail):
                // open a turn so the data isn't dropped
                self.ensure_live_turn(nanos);
                let mut emit_now: Option<(usize, PendingGen)> = None;
                if let Some(turn) = self.turn.as_mut() {
                    // A multi-line message repeats identical usage on every
                    // line; charge each API message id exactly once.
                    if !usage.is_empty()
                        && let Some(id) = &msg_id
                        && !turn.seen_usage_msg_ids.insert(id.clone())
                    {
                        usage.clear();
                    }
                    let start = if prev_nanos > 0 && prev_nanos <= nanos {
                        prev_nanos
                    } else {
                        nanos
                    };
                    let mut thinking = thinking;
                    if thinking.is_none() && !turn.pending_thinking.is_empty() {
                        thinking = Some(turn.pending_thinking.join("\n"));
                        turn.pending_thinking.clear();
                    }
                    let generation = PendingGen {
                        text: text.clone(),
                        model: model.or_else(|| self.last_known_model.clone()),
                        thinking,
                        usage,
                        start_nanos: start,
                        end_nanos: nanos,
                        ts_approx: approx,
                    };
                    // Codex buffers generations until close (its per-turn
                    // token_count must attach to the final one); the other
                    // providers carry usage on the message itself, so their
                    // generations export as soon as they complete.
                    if self.settings.provider == Provider::Codex {
                        turn.pending_generations.push(generation);
                    } else {
                        let index = turn.gen_count;
                        turn.gen_count += 1;
                        emit_now = Some((index, generation));
                    }
                    turn.last_assistant_text = Some(text);
                    turn.last_nanos = nanos;
                    turn.any_ts_approx |= approx;
                    turn.event_index += 1;
                }
                if let (Some((index, generation)), Some(turn)) = (emit_now, self.turn.as_ref())
                    && self.emitting
                {
                    spans.push(self.gen_span(turn, index, &generation));
                }
            }
            TranscriptEvent::ToolUse { id, name, args, ts } => {
                let (nanos, approx) = self.event_nanos(&ts, recv_nanos);
                self.ensure_live_turn(nanos);
                if let Some(turn) = self.turn.as_mut() {
                    turn.event_index += 1;
                    turn.open_tools.push(OpenTool {
                        id,
                        name,
                        args,
                        start_nanos: nanos,
                        ts_approx: approx,
                        event_index: turn.event_index,
                    });
                    turn.last_nanos = nanos;
                    turn.any_ts_approx |= approx;
                }
            }
            TranscriptEvent::ToolResult {
                id,
                content,
                is_error,
                ts,
            } => {
                let (nanos, approx) = self.event_nanos(&ts, recv_nanos);
                self.ensure_live_turn(nanos);
                let mut popped: Option<(OpenTool, bool)> = None; // (tool, orphan)
                if let Some(turn_ref) = self.turn.as_mut() {
                    turn_ref.last_nanos = nanos;
                    turn_ref.any_ts_approx |= approx;
                    turn_ref.event_index += 1;
                    // pair by id when the provider has ids, else FIFO
                    let idx = if id.is_empty() {
                        if turn_ref.open_tools.is_empty() { None } else { Some(0) }
                    } else {
                        turn_ref.open_tools.iter().position(|t| t.id == id)
                    };
                    popped = Some(match idx {
                        Some(i) => (turn_ref.open_tools.remove(i), false),
                        // orphan result (no matching ToolUse observed, e.g.
                        // its use predated a truncation reset): synthesize a
                        // zero-duration span rather than dropping the data
                        None => (
                            OpenTool {
                                id,
                                name: "unknown".into(),
                                args: serde_json::Value::Null,
                                start_nanos: nanos,
                                ts_approx: approx,
                                event_index: turn_ref.event_index,
                            },
                            true,
                        ),
                    });
                }
                if let (Some((tool, orphan)), Some(turn)) = (popped.as_ref(), self.turn.as_ref())
                    && self.emitting
                {
                    let mut span =
                        self.tool_span(turn, tool, nanos, Some(&content), is_error, false);
                    if *orphan {
                        span.status_message = Some("unpaired result".to_string());
                    }
                    spans.push(span);
                }
            }
            TranscriptEvent::Thinking { text, ts } => {
                let (nanos, approx) = self.event_nanos(&ts, recv_nanos);
                self.ensure_live_turn(nanos);
                if let Some(turn) = self.turn.as_mut() {
                    turn.pending_thinking.push(text);
                    turn.last_nanos = nanos;
                    turn.any_ts_approx |= approx;
                }
            }
            TranscriptEvent::TokenCount { usage, msg_id, ts } => {
                let (nanos, _) = self.event_nanos(&ts, recv_nanos);
                self.ensure_live_turn(nanos);
                if let Some(turn) = self.turn.as_mut() {
                    // dedup repeated usage lines of one API message; then
                    // ACCUMULATE — a turn can span several API calls, and
                    // overwriting would keep only the last one
                    let already_charged = msg_id
                        .as_ref()
                        .is_some_and(|id| !turn.seen_usage_msg_ids.insert(id.clone()));
                    if !already_charged {
                        accumulate_usage(&mut turn.pending_turn_usage, &usage);
                    }
                    turn.last_nanos = nanos;
                }
            }
        }
        spans
    }

    /// Closes any open turn (session over).
    pub fn finalize(&mut self) -> Vec<Span> {
        self.close_turn()
    }
}

fn clean_json_str(val: &serde_json::Value) -> Option<String> {
    match val {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
                if let Ok(serde_json::Value::String(unescaped)) = serde_json::from_str(trimmed) {
                    return Some(unescaped.trim().to_string());
                }
                return Some(trimmed[1..trimmed.len() - 1].trim().to_string());
            }
            Some(trimmed.to_string())
        }
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn normalize_tool_name(name: &str) -> &str {
    if let Some(pos) = name.rfind(':') {
        &name[pos + 1..]
    } else {
        name
    }
}

struct AgentInfo {
    name: String,
    role: Option<String>,
    type_name: Option<String>,
    model: Option<String>,
    prompt: Option<String>,
}

fn extract_skill_info(raw_tool_name: &str, args: &serde_json::Value) -> Option<(String, String)> {
    let tool_name = normalize_tool_name(raw_tool_name);
    if tool_name.eq_ignore_ascii_case("skill")
        || tool_name == "load_skill"
        || tool_name == "use_skill"
        || tool_name == "activate_skill"
    {
        let name = args
            .get("skill")
            .or_else(|| args.get("skill_name"))
            .or_else(|| args.get("name"))
            .and_then(clean_json_str)
            .unwrap_or_else(|| "skill".into());
        let path = args
            .get("path")
            .or_else(|| args.get("file_path"))
            .and_then(clean_json_str)
            .unwrap_or_default();
        return Some((name, path));
    }
    
    // Tools that read skill files
    let path = args
        .get("AbsolutePath")
        .or_else(|| args.get("file_path"))
        .or_else(|| args.get("path"))
        .or_else(|| args.get("filePath"))
        .or_else(|| args.get("target_file"))
        .and_then(clean_json_str)?;

    let path_lower = path.to_lowercase();
    if path_lower.contains("/skills/")
        || path_lower.contains("\\skills\\")
        || path_lower.ends_with("skill.md")
        || path_lower.contains(".claude/skills")
        || path_lower.contains(".gemini/skills")
        || path_lower.contains(".agent/skills")
    {
        let p = std::path::Path::new(&path);
        let skill_name = if p.file_name().is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case("skill.md")) {
            p.parent()
                .and_then(|parent| parent.file_name())
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "custom_skill".into())
        } else {
            p.file_stem()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "custom_skill".into())
        };
        return Some((skill_name, path));
    }
    None
}

fn extract_agent_info(raw_tool_name: &str, args: &serde_json::Value) -> Option<AgentInfo> {
    let tool_name = normalize_tool_name(raw_tool_name);
    if tool_name == "invoke_subagent" || tool_name == "launch_subagent" || tool_name == "spawn_agent" {
        let subagents_val = args.get("Subagents").or_else(|| args.get("subagents"));
        let parsed_arr: Option<Vec<serde_json::Value>> = match subagents_val {
            Some(serde_json::Value::Array(arr)) => Some(arr.clone()),
            Some(serde_json::Value::String(s)) => {
                let trimmed = s.trim();
                serde_json::from_str(trimmed).ok()
            }
            _ => None,
        };

        if let Some(subagents) = parsed_arr && let Some(first) = subagents.first() {
            let role = first.get("Role").or_else(|| first.get("role")).and_then(clean_json_str);
            let type_name = first
                .get("TypeName")
                .or_else(|| first.get("type_name"))
                .or_else(|| first.get("type"))
                .and_then(clean_json_str);
            let model = first.get("Model").or_else(|| first.get("model")).and_then(clean_json_str);
            let prompt = first
                .get("Prompt")
                .or_else(|| first.get("prompt"))
                .or_else(|| first.get("instruction"))
                .and_then(clean_json_str);
            let display_name = match (&role, &type_name) {
                (Some(r), Some(t)) => format!("agent: {r} ({t})"),
                (Some(r), None) => format!("agent: {r}"),
                (None, Some(t)) => format!("agent: {t}"),
                (None, None) => "agent: subagent".into(),
            };
            return Some(AgentInfo {
                name: display_name,
                role,
                type_name,
                model,
                prompt,
            });
        }
        return Some(AgentInfo {
            name: "agent: invoke_subagent".into(),
            role: None,
            type_name: None,
            model: None,
            prompt: None,
        });
    }
    if tool_name == "define_subagent" || tool_name == "create_agent" {
        let name = args.get("name").and_then(clean_json_str).unwrap_or_else(|| "custom_agent".into());
        let desc = args.get("description").and_then(clean_json_str);
        let prompt = args.get("system_prompt").or_else(|| args.get("prompt")).and_then(clean_json_str);
        return Some(AgentInfo {
            name: format!("define_agent: {name}"),
            role: Some(name),
            type_name: desc,
            model: None,
            prompt,
        });
    }
    if tool_name.eq_ignore_ascii_case("agent")
        || tool_name.eq_ignore_ascii_case("task")
        || tool_name == "dispatch_agent"
        || tool_name == "subagent"
    {
        let sub_type = args
            .get("subagent_type")
            .or_else(|| args.get("type"))
            .or_else(|| args.get("role"))
            .and_then(clean_json_str);
        let prompt = args
            .get("prompt")
            .or_else(|| args.get("instruction"))
            .or_else(|| args.get("description"))
            .and_then(clean_json_str);
        let display_name = sub_type
            .as_ref()
            .map(|t| format!("agent: {t}"))
            .unwrap_or_else(|| "agent: subagent".into());
        return Some(AgentInfo {
            name: display_name,
            role: None,
            type_name: sub_type,
            model: None,
            prompt,
        });
    }
    None
}

fn format_tool_span_name(raw_name: &str, args: &serde_json::Value) -> String {
    let name = normalize_tool_name(raw_name);
    match name {
        "run_command" | "Bash" | "bash" | "execute_command" => {
            if let Some(cmd) = args.get("CommandLine").or_else(|| args.get("command")).and_then(clean_json_str) {
                let short: String = cmd.chars().take(40).collect();
                format!("bash: {short}")
            } else {
                "bash".into()
            }
        }
        "grep_search" | "Grep" | "grep" => {
            if let Some(q) = args.get("Query").or_else(|| args.get("query")).or_else(|| args.get("pattern")).and_then(clean_json_str) {
                format!("grep: {q}")
            } else {
                "grep".into()
            }
        }
        "find_by_name" | "Glob" | "find" | "glob" => {
            if let Some(p) = args.get("Pattern").or_else(|| args.get("pattern")).and_then(clean_json_str) {
                format!("find: {p}")
            } else {
                "find".into()
            }
        }
        "list_dir" | "LS" | "ls" | "list_directory" => {
            if let Some(p) = args.get("DirectoryPath").or_else(|| args.get("path")).and_then(clean_json_str) {
                let file = std::path::Path::new(&p).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or(p);
                format!("ls: {file}")
            } else {
                "ls".into()
            }
        }
        "view_file" | "read_file" | "Read" | "View" | "view" => {
            if let Some(p) = args.get("AbsolutePath").or_else(|| args.get("file_path")).or_else(|| args.get("path")).and_then(clean_json_str) {
                let file = std::path::Path::new(&p).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or(p);
                format!("read: {file}")
            } else {
                "read_file".into()
            }
        }
        "replace_file_content" | "Edit" | "edit" | "str_replace" => {
            if let Some(p) = args.get("TargetFile").or_else(|| args.get("file_path")).or_else(|| args.get("path")).and_then(clean_json_str) {
                let file = std::path::Path::new(&p).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or(p);
                format!("edit: {file}")
            } else {
                "edit_file".into()
            }
        }
        "write_to_file" | "Write" | "write" | "create_file" => {
            if let Some(p) = args.get("TargetFile").or_else(|| args.get("file_path")).or_else(|| args.get("path")).and_then(clean_json_str) {
                let file = std::path::Path::new(&p).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or(p);
                format!("write: {file}")
            } else {
                "write_file".into()
            }
        }
        "search_web" | "WebSearch" | "search" => {
            if let Some(q) = args.get("query").or_else(|| args.get("Query")).and_then(clean_json_str) {
                format!("web_search: {q}")
            } else {
                "web_search".into()
            }
        }
        "read_url_content" | "WebFetch" | "fetch" => {
            if let Some(u) = args.get("Url").or_else(|| args.get("url")).and_then(clean_json_str) {
                let short: String = u.chars().take(40).collect();
                format!("fetch: {short}")
            } else {
                "fetch_url".into()
            }
        }
        "ask_question" | "Ask" | "ask" => "ask_question".into(),
        "manage_subagents" => "manage_agents".into(),
        "send_message" => "send_message".into(),
        _ => normalize_tool_name(raw_name).to_string(),
    }
}

/// Outcome recorded on the lifecycle `session_ended` span.
pub struct SessionEnd {
    /// "exit" | "app_quit"
    pub termination: &'static str,
    pub exit_code: Option<u32>,
    pub correlation: String,
    pub session_id: Option<String>,
    pub parse_errors: u64,
    /// App-wide dropped-span count at session end (backpressure/network).
    pub dropped_spans: u64,
}

fn lifecycle_common(settings: &MapSettings, correlation: &str) -> Vec<KeyValue> {
    let mut attrs = vec![
        str_attr("langfuse.observation.type", "event"),
        str_attr(
            "langfuse.trace.name",
            format!("{} session", settings.profile_name),
        ),
        str_attr("langfuse.trace.metadata.launch_id", settings.launch_id.clone()),
        str_attr("langfuse.trace.metadata.profile", settings.profile_name.clone()),
        str_attr("langfuse.trace.metadata.cwd", settings.cwd.clone()),
        str_attr(
            "langfuse.trace.metadata.project_slug",
            settings.project_slug.clone(),
        ),
        str_attr("langfuse.trace.metadata.provider", settings.provider.as_str()),
        str_attr(
            "langfuse.trace.metadata.agent_mux_version",
            env!("CARGO_PKG_VERSION"),
        ),
        str_attr("langfuse.trace.metadata.correlation", correlation.to_string()),
    ];
    // lifecycle traces must land in the same environment/release as the
    // turn traces they accompany
    if let Some(release) = &settings.release {
        attrs.push(str_attr("langfuse.release", release.clone()));
    }
    if let Some(env) = &settings.environment {
        attrs.push(str_attr("langfuse.environment", env.clone()));
    }
    attrs
}

fn lifecycle_trace_id(settings: &MapSettings) -> [u8; 16] {
    otlp::trace_id_for(&format!("amx1|{}|lifecycle", settings.launch_id))
}

/// Point-in-time EVENT span emitted at spawn. Carries `langfuse.session.id`
/// only when the id is already Known (Claude pre-assignment / resume) — for
/// watched providers the id doesn't exist yet, and this span is complete on
/// arrival; `launch_id` metadata is the lifecycle join key regardless.
pub fn session_started_span(
    settings: &MapSettings,
    session_id: Option<&str>,
    correlation: &str,
    now_nanos: i128,
) -> Span {
    let mut attrs = lifecycle_common(settings, correlation);
    if let Some(id) = session_id {
        attrs.push(str_attr("langfuse.session.id", id.to_string()));
    }
    if let Some(user) = &settings.user_id {
        attrs.push(str_attr("langfuse.user.id", user.clone()));
    }
    let mut tags = vec![
        AnyValue::Str("agent-mux".into()),
        AnyValue::Str(settings.provider.as_str().into()),
    ];
    tags.extend(settings.tags.iter().map(|t| AnyValue::Str(t.clone())));
    attrs.push(attr("langfuse.trace.tags", AnyValue::Array(tags)));
    Span {
        trace_id: lifecycle_trace_id(settings),
        span_id: otlp::span_id_for(&format!("amx1|{}|lifecycle|started", settings.launch_id)),
        parent_span_id: None,
        name: "session_started".into(),
        start_nanos: now_nanos,
        end_nanos: now_nanos,
        attributes: attrs,
        status_code: STATUS_UNSET,
        status_message: None,
    }
}

/// Point-in-time EVENT span in the same lifecycle trace, emitted at session
/// end (or once, with `termination: "app_quit"`, during app shutdown).
pub fn session_ended_span(settings: &MapSettings, end: &SessionEnd, now_nanos: i128) -> Span {
    let mut attrs = lifecycle_common(settings, &end.correlation);
    if let Some(id) = &end.session_id {
        attrs.push(str_attr("langfuse.session.id", id.clone()));
    }
    if let Some(user) = &settings.user_id {
        attrs.push(str_attr("langfuse.user.id", user.clone()));
    }
    attrs.push(str_attr("langfuse.trace.metadata.termination", end.termination));
    if let Some(code) = end.exit_code {
        attrs.push(attr(
            "langfuse.trace.metadata.exit_code",
            AnyValue::Int(i64::from(code)),
        ));
    }
    if end.parse_errors > 0 {
        attrs.push(attr(
            "langfuse.trace.metadata.parse_errors",
            AnyValue::Int(end.parse_errors as i64),
        ));
    }
    if end.dropped_spans > 0 {
        attrs.push(attr(
            "langfuse.trace.metadata.dropped_spans",
            AnyValue::Int(end.dropped_spans as i64),
        ));
    }
    Span {
        trace_id: lifecycle_trace_id(settings),
        span_id: otlp::span_id_for(&format!("amx1|{}|lifecycle|ended", settings.launch_id)),
        parent_span_id: None,
        name: "session_ended".into(),
        start_nanos: now_nanos,
        end_nanos: now_nanos,
        attributes: attrs,
        status_code: STATUS_UNSET,
        status_message: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(mode: ContentMode) -> MapSettings {
        MapSettings {
            provider: Provider::Claude,
            content_mode: mode,
            content_max_bytes: 10_000,
            redact_literals: vec![],
            user_id: Some("tester".into()),
            release: Some("r1".into()),
            tags: vec!["extra".into()],
            environment: Some("dev".into()),
            profile_name: "Claude Code".into(),
            cwd: "/tmp/proj".into(),
            project_slug: "-tmp-proj".into(),
            agent_mux_session: 7,
            launch_id: "launch-1".into(),
        }
    }

    fn user(text: &str, ts: &str) -> TranscriptEvent {
        TranscriptEvent::User {
            text: text.into(),
            ts: Some(ts.into()),
        }
    }

    fn assistant(text: &str, ts: &str, usage: Vec<(String, i64)>) -> TranscriptEvent {
        TranscriptEvent::Assistant {
            text: text.into(),
            model: Some("claude-fable-5".into()),
            thinking: None,
            usage,
            msg_id: None,
            ts: Some(ts.into()),
        }
    }

    fn attr_str<'a>(span: &'a Span, key: &str) -> Option<&'a str> {
        span.attributes.iter().find(|a| a.key == key).and_then(|a| match &a.value {
            AnyValue::Str(s) => Some(s.as_str()),
            _ => None,
        })
    }

    fn attr_int(span: &Span, key: &str) -> Option<i64> {
        span.attributes.iter().find(|a| a.key == key).and_then(|a| match &a.value {
            AnyValue::Int(n) => Some(*n),
            _ => None,
        })
    }

    #[test]
    fn basic_turn_shape_root_generation_and_tool() {
        let mut asm = TurnAssembler::new(settings(ContentMode::Full), Some("sess-1".into()), "deterministic");
        let mut spans = Vec::new();
        spans.extend(asm.feed(user("fix it", "2026-08-30T10:00:00Z"), 0));
        spans.extend(asm.feed(assistant("on it", "2026-08-30T10:00:05Z", vec![("input_tokens".into(), 10)]), 0));
        spans.extend(asm.feed(
            TranscriptEvent::ToolUse {
                id: "t1".into(),
                name: "Bash".into(),
                args: serde_json::json!({"command": "ls"}),
                ts: Some("2026-08-30T10:00:06Z".into()),
            },
            0,
        ));
        spans.extend(asm.feed(
            TranscriptEvent::ToolResult {
                id: "t1".into(),
                content: serde_json::Value::String("ok".into()),
                is_error: false,
                ts: Some("2026-08-30T10:00:07Z".into()),
            },
            0,
        ));
        // non-Codex providers export children as they complete: the
        // generation (Claude usage rides the message) then the tool span
        assert_eq!(spans.len(), 2, "generation + tool: {spans:#?}");
        let generation = spans[0].clone();
        assert_eq!(generation.name, "assistant");
        assert_eq!(attr_str(&generation, "gen_ai.request.model"), Some("claude-fable-5"));
        assert_eq!(attr_int(&generation, "gen_ai.usage.input_tokens"), Some(10));
        assert_eq!(attr_str(&generation, "langfuse.observation.output"), Some("on it"));
        assert_eq!(spans[1].name, "Bash");
        assert_eq!(attr_str(&spans[1], "langfuse.observation.type"), Some("tool"));
        assert_eq!(attr_str(&spans[1], "langfuse.observation.input"), Some(r#"{"command":"ls"}"#));
        assert_eq!(attr_str(&spans[1], "langfuse.observation.output"), Some("ok"));
        assert_eq!(attr_str(&spans[1], "langfuse.session.id"), Some("sess-1"));

        // next user closes the turn: root only (children already shipped)
        let close = asm.feed(user("next", "2026-08-30T10:01:00Z"), 0);
        assert_eq!(close.len(), 1, "root only: {close:#?}");
        let root = &close[0];
        assert_eq!(root.name, "turn 1");
        assert!(root.parent_span_id.is_none());
        assert_eq!(generation.parent_span_id, Some(root.span_id));
        assert_eq!(generation.trace_id, root.trace_id);
        assert_eq!(attr_str(root, "langfuse.trace.name"), Some("Claude Code: fix it"));
        assert_eq!(attr_str(root, "langfuse.trace.input"), Some("fix it"));
        assert_eq!(attr_str(root, "langfuse.trace.output"), Some("on it"));
        assert_eq!(attr_str(root, "langfuse.release"), Some("r1"));
        assert_eq!(attr_str(root, "langfuse.environment"), Some("dev"));
        assert_eq!(attr_str(root, "langfuse.trace.metadata.correlation"), Some("deterministic"));
        assert_eq!(attr_int(root, "langfuse.trace.metadata.agent_mux_session"), Some(7));
        // times: root spans user->last event; generation start approximates
        // from the previous event
        assert_eq!(root.start_nanos, parse_rfc3339_nanos("2026-08-30T10:00:00Z").unwrap());
        assert_eq!(generation.start_nanos, parse_rfc3339_nanos("2026-08-30T10:00:00Z").unwrap());
        assert_eq!(generation.end_nanos, parse_rfc3339_nanos("2026-08-30T10:00:05Z").unwrap());

        // second turn has a distinct deterministic trace id
        let t2 = asm.feed(user("third", "2026-08-30T10:02:00Z"), 0);
        assert_eq!(t2.len(), 1); // root only (empty turn 2)
        assert_ne!(t2[0].trace_id, root.trace_id);
        assert_eq!(t2[0].trace_id, otlp::trace_id_for("amx1|claude|sess-1|turn|2"));
    }

    #[test]
    fn metadata_mode_exports_no_content_attributes() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Metadata),
            Some("sess-1".into()),
            "deterministic",
        );
        let mut spans = Vec::new();
        spans.extend(asm.feed(user("secret prompt", "2026-08-30T10:00:00Z"), 0));
        spans.extend(asm.feed(
            TranscriptEvent::Assistant {
                text: "secret reply".into(),
                model: Some("m".into()),
                thinking: Some("secret thoughts".into()),
                usage: vec![("output_tokens".into(), 5)],
                msg_id: None,
                ts: Some("2026-08-30T10:00:01Z".into()),
            },
            0,
        ));
        spans.extend(asm.feed(
            TranscriptEvent::ToolUse {
                id: "t1".into(),
                name: "Bash".into(),
                args: serde_json::json!({"command": "cat /etc/passwd"}),
                ts: Some("2026-08-30T10:00:02Z".into()),
            },
            0,
        ));
        spans.extend(asm.feed(
            TranscriptEvent::ToolResult {
                id: "t1".into(),
                content: serde_json::Value::String("secret output".into()),
                is_error: true,
                ts: Some("2026-08-30T10:00:03Z".into()),
            },
            0,
        ));
        spans.extend(asm.finalize());
        assert!(!spans.is_empty());
        const CONTENT_KEYS: [&str; 6] = [
            "langfuse.trace.input",
            "langfuse.trace.output",
            "langfuse.observation.input",
            "langfuse.observation.output",
            "langfuse.observation.metadata.thinking",
            "langfuse.trace.metadata.thinking",
        ];
        for span in &spans {
            for kv in &span.attributes {
                assert!(
                    !CONTENT_KEYS.contains(&kv.key.as_str()),
                    "content attribute {} leaked in metadata mode (span {})",
                    kv.key,
                    span.name
                );
                if let AnyValue::Str(s) = &kv.value {
                    assert!(!s.contains("secret"), "content leaked via {}: {s}", kv.key);
                }
            }
        }
        // structure still flows: usage, model, error level, trace name
        let root = spans.iter().find(|s| s.name.starts_with("turn")).unwrap();
        assert_eq!(attr_str(root, "langfuse.trace.name"), Some("turn 1"));
        let generation = spans.iter().find(|s| s.name == "assistant").unwrap();
        assert_eq!(attr_int(generation, "gen_ai.usage.output_tokens"), Some(5));
        let tool = spans.iter().find(|s| s.name == "Bash").unwrap();
        assert_eq!(tool.status_code, STATUS_ERROR);
    }

    #[test]
    fn codex_boundaries_attach_usage_and_survive_aborted_rerun() {
        let mut asm = TurnAssembler::new(
            MapSettings {
                provider: Provider::Codex,
                ..settings(ContentMode::Metadata)
            },
            Some("codex-sess".into()),
            "watched",
        );
        let boundary = |kind, ts: &str| TranscriptEvent::TurnBoundary {
            kind,
            ts: Some(ts.into()),
        };
        let mut spans = Vec::new();
        spans.extend(asm.feed(boundary(TurnBoundaryKind::Start, "2026-08-30T10:00:00Z"), 0));
        spans.extend(asm.feed(user("do it", "2026-08-30T10:00:01Z"), 0));
        spans.extend(asm.feed(assistant("working", "2026-08-30T10:00:02Z", vec![]), 0));
        spans.extend(asm.feed(
            TranscriptEvent::TokenCount {
                usage: vec![("input_tokens".into(), 200), ("output_tokens".into(), 20)],
                msg_id: None,
                ts: Some("2026-08-30T10:00:03Z".into()),
            },
            0,
        ));
        // an aborted rerun: same user, no new user message
        spans.extend(asm.feed(boundary(TurnBoundaryKind::Aborted, "2026-08-30T10:00:04Z"), 0));
        let after_abort = spans.len();
        assert!(after_abort >= 2, "aborted turn still closed: {spans:#?}");
        let turn1_root = spans.iter().find(|s| s.name == "turn 1").unwrap();
        assert_eq!(attr_str(turn1_root, "langfuse.trace.metadata.terminated"), Some("aborted"));
        // usage attached to the turn's final generation
        let generation = spans.iter().find(|s| s.name == "assistant").unwrap();
        assert_eq!(attr_int(generation, "gen_ai.usage.input_tokens"), Some(200));

        // rerun opens turn 2 via Start without a user message — distinct ordinal/trace id
        let mut spans2 = Vec::new();
        spans2.extend(asm.feed(boundary(TurnBoundaryKind::Start, "2026-08-30T10:00:05Z"), 0));
        spans2.extend(asm.feed(assistant("done", "2026-08-30T10:00:06Z", vec![]), 0));
        spans2.extend(asm.feed(boundary(TurnBoundaryKind::Complete, "2026-08-30T10:00:07Z"), 0));
        let turn2_root = spans2.iter().find(|s| s.name == "turn 2").unwrap();
        assert_ne!(turn2_root.trace_id, turn1_root.trace_id);
        // a user message mid-turn must NOT open turn 3 when boundaries are explicit
        let mut spans3 = Vec::new();
        spans3.extend(asm.feed(boundary(TurnBoundaryKind::Start, "2026-08-30T10:01:00Z"), 0));
        spans3.extend(asm.feed(user("steering", "2026-08-30T10:01:01Z"), 0));
        spans3.extend(asm.feed(boundary(TurnBoundaryKind::Complete, "2026-08-30T10:01:02Z"), 0));
        let roots: Vec<_> = spans3.iter().filter(|s| s.name.starts_with("turn")).collect();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "turn 3");
    }

    #[test]
    fn antigravity_positional_pairing_and_unpaired_close() {
        let mut asm = TurnAssembler::new(
            MapSettings {
                provider: Provider::Antigravity,
                ..settings(ContentMode::Metadata)
            },
            Some("agy-sess".into()),
            "heuristic",
        );
        let mut spans = Vec::new();
        spans.extend(asm.feed(user("go", "2026-08-30T10:00:00Z"), 0));
        for name in ["list_dir", "view_file"] {
            spans.extend(asm.feed(
                TranscriptEvent::ToolUse {
                    id: String::new(),
                    name: name.into(),
                    args: serde_json::Value::Null,
                    ts: Some("2026-08-30T10:00:01Z".into()),
                },
                0,
            ));
        }
        // one positional result: pairs FIFO with list_dir
        spans.extend(asm.feed(
            TranscriptEvent::ToolResult {
                id: String::new(),
                content: serde_json::Value::String("dir contents".into()),
                is_error: false,
                ts: Some("2026-08-30T10:00:02Z".into()),
            },
            0,
        ));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "list_dir");
        // view_file never gets a result; closes with the turn
        let close = asm.finalize();
        let unpaired = close.iter().find(|s| s.name == "view_file").unwrap();
        assert_eq!(unpaired.status_message.as_deref(), Some("no result observed"));
        let root = close.iter().find(|s| s.name == "turn 1").unwrap();
        assert_eq!(attr_str(root, "langfuse.trace.metadata.correlation"), Some("heuristic"));
    }

    #[test]
    fn prime_pass_counts_ordinals_without_emitting() {
        let mut asm = TurnAssembler::new(settings(ContentMode::Full), Some("sess-1".into()), "deterministic");
        asm.set_emitting(false);
        assert!(asm.feed(user("old turn 1", "2026-08-30T09:00:00Z"), 0).is_empty());
        assert!(asm.feed(assistant("old reply", "2026-08-30T09:00:01Z", vec![]), 0).is_empty());
        assert!(asm.feed(user("old turn 2", "2026-08-30T09:01:00Z"), 0).is_empty());
        asm.set_emitting(true);
        // the next real turn continues the ordinal sequence: turn 3
        let spans = asm.feed(user("new turn", "2026-08-30T10:00:00Z"), 0);
        assert!(spans.is_empty(), "open primed turn was never emitted: {spans:#?}");
        let close = asm.finalize();
        let root = close.iter().find(|s| s.name.starts_with("turn")).unwrap();
        assert_eq!(root.name, "turn 3");
        assert_eq!(root.trace_id, otlp::trace_id_for("amx1|claude|sess-1|turn|3"));
    }

    #[test]
    fn backfill_truncation_salts_trace_ids_with_launch() {
        let mut asm = TurnAssembler::new(settings(ContentMode::Full), Some("sess-1".into()), "deterministic");
        asm.mark_backfill_truncated();
        let _ = asm.feed(user("turn", "2026-08-30T10:00:00Z"), 0);
        let close = asm.finalize();
        let root = close.iter().find(|s| s.name.starts_with("turn")).unwrap();
        assert_eq!(
            root.trace_id,
            otlp::trace_id_for("amx1|claude|sess-1|turn|1|launch-1")
        );
    }

    #[test]
    fn missing_timestamp_falls_back_to_receive_time_with_flags() {
        let mut asm = TurnAssembler::new(settings(ContentMode::Metadata), Some("s".into()), "deterministic");
        let recv = 1_788_118_098_000_000_000_i128;
        let _ = asm.feed(
            TranscriptEvent::User {
                text: "no ts".into(),
                ts: None,
            },
            recv,
        );
        let close = asm.finalize();
        let root = close.iter().find(|s| s.name.starts_with("turn")).unwrap();
        assert_eq!(root.start_nanos, recv);
        assert_eq!(attr_str(root, "langfuse.trace.metadata.timing"), Some("approximate"));
    }

    #[test]
    fn synthetic_model_goes_to_metadata_not_gen_ai() {
        let mut asm = TurnAssembler::new(settings(ContentMode::Metadata), Some("s".into()), "deterministic");
        let mut spans = Vec::new();
        spans.extend(asm.feed(user("u", "2026-08-30T10:00:00Z"), 0));
        spans.extend(asm.feed(
            TranscriptEvent::Assistant {
                text: "synthetic".into(),
                model: Some("<synthetic>".into()),
                thinking: None,
                usage: vec![],
                msg_id: None,
                ts: Some("2026-08-30T10:00:01Z".into()),
            },
            0,
        ));
        spans.extend(asm.finalize());
        let generation = spans.iter().find(|s| s.name == "assistant").unwrap();
        assert_eq!(attr_str(generation, "gen_ai.request.model"), None);
        assert_eq!(
            attr_str(generation, "langfuse.observation.metadata.model"),
            Some("<synthetic>")
        );
    }

    #[test]
    fn full_mode_trace_name_is_masked_like_all_content() {
        let mut asm = TurnAssembler::new(
            MapSettings {
                redact_literals: vec!["hunter2".into()],
                ..settings(ContentMode::Full)
            },
            Some("s".into()),
            "deterministic",
        );
        let _ = asm.feed(user("sk-live-topsecret rotate this hunter2 now", "2026-08-30T10:00:00Z"), 0);
        let close = asm.finalize();
        let root = close.iter().find(|s| s.name.starts_with("turn")).unwrap();
        let name = attr_str(root, "langfuse.trace.name").unwrap();
        assert!(!name.contains("topsecret"), "secret leaked via trace name: {name}");
        assert!(!name.contains("hunter2"), "redact literal leaked via trace name: {name}");
        assert!(name.contains("[REDACTED]"), "{name}");
    }

    #[test]
    fn repeated_message_usage_is_charged_once_and_token_counts_accumulate() {
        let mut asm = TurnAssembler::new(settings(ContentMode::Metadata), Some("s".into()), "deterministic");
        let mut spans = Vec::new();
        spans.extend(asm.feed(user("go", "2026-08-30T10:00:00Z"), 0));
        // one API message streamed as two JSONL lines, identical usage on both
        for (i, text) in ["part one", "part two"].iter().enumerate() {
            spans.extend(asm.feed(
                TranscriptEvent::Assistant {
                    text: text.to_string(),
                    model: Some("m".into()),
                    thinking: None,
                    usage: vec![("input_tokens".into(), 100), ("output_tokens".into(), 10)],
                    msg_id: Some("msg_1".into()),
                    ts: Some(format!("2026-08-30T10:00:0{}Z", i + 1)),
                },
                0,
            ));
        }
        // tool-only message usage arrives as TokenCount events; two distinct
        // messages accumulate, a repeat of a charged id is dropped
        for (msg, tokens) in [("msg_2", 50), ("msg_3", 25), ("msg_1", 999)] {
            spans.extend(asm.feed(
                TranscriptEvent::TokenCount {
                    usage: vec![("input_tokens".into(), tokens)],
                    msg_id: Some(msg.into()),
                    ts: Some("2026-08-30T10:00:05Z".into()),
                },
                0,
            ));
        }
        spans.extend(asm.finalize());
        let total: i64 = spans
            .iter()
            .filter_map(|s| attr_int(s, "gen_ai.usage.input_tokens"))
            .sum();
        // 100 (msg_1, once) + 50 + 25; never 200, never +999
        assert_eq!(total, 175, "usage misrouted: {spans:#?}");
    }

    #[test]
    fn same_name_tool_spans_get_distinct_deterministic_ids() {
        let mut asm = TurnAssembler::new(
            MapSettings {
                provider: Provider::Antigravity,
                ..settings(ContentMode::Metadata)
            },
            Some("s".into()),
            "heuristic",
        );
        let mut spans = Vec::new();
        spans.extend(asm.feed(user("go", "2026-08-30T10:00:00Z"), 0));
        // two same-name tools, neither ever resolved: both close unpaired
        for _ in 0..2 {
            spans.extend(asm.feed(
                TranscriptEvent::ToolUse {
                    id: String::new(),
                    name: "run_command".into(),
                    args: serde_json::Value::Null,
                    ts: Some("2026-08-30T10:00:01Z".into()),
                },
                0,
            ));
        }
        spans.extend(asm.finalize());
        let tool_ids: Vec<_> = spans
            .iter()
            .filter(|s| s.name == "run_command")
            .map(|s| s.span_id)
            .collect();
        assert_eq!(tool_ids.len(), 2);
        assert_ne!(tool_ids[0], tool_ids[1], "span-id collision on same-name tools");
    }

    #[test]
    fn orphan_tool_result_yields_unpaired_span_not_silence() {
        let mut asm = TurnAssembler::new(settings(ContentMode::Metadata), Some("s".into()), "deterministic");
        let _ = asm.feed(user("go", "2026-08-30T10:00:00Z"), 0);
        let spans = asm.feed(
            TranscriptEvent::ToolResult {
                id: "never-seen".into(),
                content: serde_json::Value::String("late output".into()),
                is_error: false,
                ts: Some("2026-08-30T10:00:02Z".into()),
            },
            0,
        );
        assert_eq!(spans.len(), 1, "orphan result must not vanish");
        assert_eq!(spans[0].status_message.as_deref(), Some("unpaired result"));
        assert_eq!(spans[0].start_nanos, spans[0].end_nanos);
    }

    #[test]
    fn crlf_pem_block_is_masked_without_eating_the_rest() {
        let text = "before\n-----BEGIN RSA PRIVATE KEY-----\r\nMIIsecret\r\n-----END RSA PRIVATE KEY-----\r\nafter survives";
        let masked = mask_secrets(text, &[]);
        assert!(!masked.contains("MIIsecret"), "{masked}");
        assert!(masked.contains("after survives"), "over-masked: {masked}");
    }

    #[test]
    fn masking_hits_every_builtin_pattern_and_literals() {
        let literals = vec!["hunter2".to_string()];
        let cases = [
            ("key sk-live-abc123 end", "key sk-[REDACTED] end"),
            ("lf pk-lf-xyz end", "lf pk-[REDACTED] end"), // pk-lf- prefix masks via sk-? no: pk-lf- itself
            ("aws AKIAIOSFODNN7EXAMPLE end", "aws AKIA[REDACTED] end"),
            ("gh ghp_abcdef end", "gh ghp_[REDACTED] end"),
            ("slack xoxb-123 end", "slack xox[REDACTED] end"),
            ("auth Bearer abc.def end", "auth Bearer [REDACTED] end"),
            ("pw hunter2 end", "pw [REDACTED] end"),
        ];
        for (input, _) in &cases {
            let masked = mask_secrets(input, &literals);
            assert!(
                !masked.contains("abc123")
                    && !masked.contains("IOSFODNN")
                    && !masked.contains("ghp_abcdef")
                    && !masked.contains("xoxb-123")
                    && !masked.contains("abc.def")
                    && !masked.contains("hunter2"),
                "unmasked secret in {masked:?} (from {input:?})"
            );
            assert!(masked.contains("[REDACTED]"), "no mask in {masked:?}");
        }
        // word-boundary guard: ordinary words containing "sk-" survive
        assert_eq!(mask_secrets("task-123 is fine", &[]), "task-123 is fine");
        // BEGIN blocks masked wholesale
        let key = "-----BEGIN RSA PRIVATE KEY-----\nMIIabc\n-----END RSA PRIVATE KEY-----\nafter";
        let masked = mask_secrets(key, &[]);
        assert!(!masked.contains("MIIabc"), "{masked:?}");
    }

    #[test]
    fn truncation_is_utf8_safe_with_marker() {
        let text = "héllo wörld, this is long";
        let out = truncate_content(text, 8);
        assert!(out.starts_with("héllo w") || out.starts_with("héllo "));
        assert!(out.contains("[truncated"));
        assert_eq!(truncate_content("short", 100), "short");
    }

    #[test]
    fn lifecycle_spans_share_trace_and_carry_session_caveat() {
        let s = settings(ContentMode::Metadata);
        let started = session_started_span(&s, None, "watched", 1_000);
        // watched provider: no session id at spawn
        assert!(attr_str(&started, "langfuse.session.id").is_none());
        assert_eq!(attr_str(&started, "langfuse.trace.metadata.launch_id"), Some("launch-1"));
        assert_eq!(started.start_nanos, started.end_nanos);
        assert_eq!(started.name, "session_started");
        assert_eq!(attr_str(&started, "langfuse.observation.type"), Some("event"));
        let ended = session_ended_span(
            &s,
            &SessionEnd {
                termination: "exit",
                exit_code: Some(0),
                correlation: "watched".into(),
                session_id: Some("adopted-id".into()),
                parse_errors: 2,
                dropped_spans: 0,
            },
            2_000,
        );
        assert_eq!(ended.trace_id, started.trace_id, "same lifecycle trace");
        assert_ne!(ended.span_id, started.span_id);
        assert_eq!(attr_str(&ended, "langfuse.session.id"), Some("adopted-id"));
        assert_eq!(attr_int(&ended, "langfuse.trace.metadata.exit_code"), Some(0));
        assert_eq!(attr_str(&ended, "langfuse.trace.metadata.termination"), Some("exit"));
        assert_eq!(attr_int(&ended, "langfuse.trace.metadata.parse_errors"), Some(2));
        // known-at-spawn (Claude) carries the id on started too
        let started_known = session_started_span(&s, Some("known-id"), "deterministic", 1_000);
        assert_eq!(attr_str(&started_known, "langfuse.session.id"), Some("known-id"));
        // app_quit shape
        let quit = session_ended_span(
            &s,
            &SessionEnd {
                termination: "app_quit",
                exit_code: None,
                correlation: "none".into(),
                session_id: None,
                parse_errors: 0,
                dropped_spans: 0,
            },
            3_000,
        );
        assert_eq!(attr_str(&quit, "langfuse.trace.metadata.termination"), Some("app_quit"));
        assert!(attr_int(&quit, "langfuse.trace.metadata.exit_code").is_none());
    }

    #[test]
    fn skill_and_agent_observations_and_details() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("sess-1".into()),
            "deterministic",
        );
        let mut spans = Vec::new();
        spans.extend(asm.feed(user("build something", "2026-08-30T10:00:00Z"), 0));
        
        // Skill loading tool call
        spans.extend(asm.feed(
            TranscriptEvent::ToolUse {
                id: "t1".into(),
                name: "view_file".into(),
                args: serde_json::json!({
                    "AbsolutePath": "/home/user/.gemini/skills/agy-customizations/SKILL.md",
                    "toolAction": "Viewing skill file",
                    "toolSummary": "View agy-customizations"
                }),
                ts: Some("2026-08-30T10:00:01Z".into()),
            },
            0,
        ));
        spans.extend(asm.feed(
            TranscriptEvent::ToolResult {
                id: "t1".into(),
                content: serde_json::Value::String("# Skill content".into()),
                is_error: false,
                ts: Some("2026-08-30T10:00:02Z".into()),
            },
            0,
        ));

        // Subagent invocation tool call
        spans.extend(asm.feed(
            TranscriptEvent::ToolUse {
                id: "t2".into(),
                name: "invoke_subagent".into(),
                args: serde_json::json!({
                    "Subagents": [{
                        "Role": "Codebase Researcher",
                        "TypeName": "research",
                        "Model": "flash",
                        "Prompt": "explore files"
                    }]
                }),
                ts: Some("2026-08-30T10:00:03Z".into()),
            },
            0,
        ));
        spans.extend(asm.feed(
            TranscriptEvent::ToolResult {
                id: "t2".into(),
                content: serde_json::Value::String("subagent finished".into()),
                is_error: false,
                ts: Some("2026-08-30T10:00:04Z".into()),
            },
            0,
        ));

        // Assistant generation
        spans.extend(asm.feed(
            TranscriptEvent::Assistant {
                text: "all done".into(),
                model: Some("gemini-3.7-flash".into()),
                thinking: Some("reasoning details".into()),
                usage: vec![],
                msg_id: None,
                ts: Some("2026-08-30T10:00:05Z".into()),
            },
            0,
        ));

        // Skill span checks
        let skill_span = spans.iter().find(|s| s.name.contains("skill:")).expect("skill span missing");
        assert_eq!(skill_span.name, "skill: agy-customizations");
        assert_eq!(attr_str(skill_span, "langfuse.observation.metadata.skill_name"), Some("agy-customizations"));
        assert_eq!(attr_str(skill_span, "langfuse.observation.metadata.kind"), Some("skill_load"));
        assert_eq!(attr_str(skill_span, "langfuse.observation.metadata.action"), Some("Viewing skill file"));
        assert_eq!(attr_str(skill_span, "langfuse.observation.output"), Some("# Skill content"));

        // Agent span checks
        let agent_span = spans.iter().find(|s| s.name.contains("agent:")).expect("agent span missing");
        assert_eq!(agent_span.name, "agent: Codebase Researcher (research)");
        assert_eq!(attr_str(agent_span, "langfuse.observation.type"), Some("agent"));
        assert_eq!(attr_str(agent_span, "langfuse.observation.metadata.agent_role"), Some("Codebase Researcher"));
        assert_eq!(attr_str(agent_span, "langfuse.observation.metadata.agent_type"), Some("research"));
        assert_eq!(attr_str(agent_span, "langfuse.observation.metadata.agent_prompt"), Some("explore files"));

        // Generation span checks (includes user input prompt)
        let gen_span = spans.iter().find(|s| s.name == "assistant").expect("assistant gen missing");
        assert_eq!(attr_str(gen_span, "langfuse.observation.input"), Some("build something"));
        assert_eq!(attr_str(gen_span, "langfuse.observation.output"), Some("all done"));
        assert_eq!(attr_str(gen_span, "langfuse.observation.metadata.thinking"), Some("reasoning details"));
        assert_eq!(attr_str(gen_span, "gen_ai.request.model"), Some("gemini-3.7-flash"));
    }

    #[test]
    fn antigravity_escaped_quotes_and_namespaced_tools() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("sess-1".into()),
            "deterministic",
        );
        let mut spans = Vec::new();
        spans.extend(asm.feed(user("build something", "2026-08-30T10:00:00Z"), 0));
        
        // Namespaced tool with escaped string quotes in JSON args
        spans.extend(asm.feed(
            TranscriptEvent::ToolUse {
                id: "t1".into(),
                name: "default_api:view_file".into(),
                args: serde_json::json!({
                    "AbsolutePath": "\"/home/silvio/.gemini/antigravity-cli/builtin/skills/agy-customizations/SKILL.md\"",
                    "toolAction": "\"Viewing skill file\"",
                    "toolSummary": "\"View agy-customizations\""
                }),
                ts: Some("2026-08-30T10:00:01Z".into()),
            },
            0,
        ));
        spans.extend(asm.feed(
            TranscriptEvent::ToolResult {
                id: "t1".into(),
                content: serde_json::Value::String("# Skill content".into()),
                is_error: false,
                ts: Some("2026-08-30T10:00:02Z".into()),
            },
            0,
        ));

        // Namespaced subagent with stringified JSON array
        spans.extend(asm.feed(
            TranscriptEvent::ToolUse {
                id: "t2".into(),
                name: "default_api:invoke_subagent".into(),
                args: serde_json::json!({
                    "Subagents": "[{\"Role\":\"Codebase Researcher\",\"TypeName\":\"research\",\"Model\":\"flash\",\"Prompt\":\"explore files\"}]"
                }),
                ts: Some("2026-08-30T10:00:03Z".into()),
            },
            0,
        ));
        spans.extend(asm.feed(
            TranscriptEvent::ToolResult {
                id: "t2".into(),
                content: serde_json::Value::String("subagent finished".into()),
                is_error: false,
                ts: Some("2026-08-30T10:00:04Z".into()),
            },
            0,
        ));

        let skill_span = spans.iter().find(|s| s.name.contains("skill:")).expect("skill span missing");
        assert_eq!(skill_span.name, "skill: agy-customizations");
        assert_eq!(attr_str(skill_span, "langfuse.observation.metadata.skill_name"), Some("agy-customizations"));
        assert_eq!(attr_str(skill_span, "langfuse.observation.metadata.action"), Some("Viewing skill file"));

        let agent_span = spans.iter().find(|s| s.name.contains("agent:")).expect("agent span missing");
        assert_eq!(agent_span.name, "agent: Codebase Researcher (research)");
        assert_eq!(attr_str(agent_span, "langfuse.observation.type"), Some("agent"));
        assert_eq!(attr_str(agent_span, "langfuse.observation.metadata.agent_role"), Some("Codebase Researcher"));
        assert_eq!(attr_str(agent_span, "langfuse.observation.metadata.agent_prompt"), Some("explore files"));
    }
}
