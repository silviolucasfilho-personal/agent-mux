//! Turn assembler: a stream of `TranscriptEvent`s in, typed store rows
//! (`StoreOp`) out. One trace per user turn, grouped into a session by the
//! CLI's own conversation id; generations and tool calls as observations;
//! the launch row replaces the old lifecycle trace.
//!
//! Content policy lives here too: in `Metadata` mode no prompt, response,
//! thinking, or tool bodies are ever attached; in `Full` mode every content
//! string passes through the secret-pattern masker and UTF-8-safe
//! truncation.

use crate::config::ContentMode;
use crate::tracing::agy_usage::GenUsage;
use crate::tracing::hooks::HookEvent;
use crate::tracing::ids;
use crate::tracing::store::model::{
    LaunchRow, Level, ObservationRow, ObservationType, SessionRow, StoreOp, TraceRow, TraceStatus,
};
use crate::tracing::usage;
use crate::transcript::{Provider, TranscriptEvent, TurnBoundaryKind, parse_rfc3339_nanos};
use serde_json::Value;
use std::collections::HashMap;

/// Everything the assembler needs to know about one launch's mapping.
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
    pub run_id: String,
    /// "deterministic" | "watched" | "none"
    pub correlation_plan: String,
    pub injected: bool,
    pub attached: bool,
    pub started_ns: i64,
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
    while let Some(start) = out.find("-----BEGIN") {
        let rest = &out[start..];
        let end = rest
            .find("-----END")
            .map(|e| {
                rest[e..]
                    .find('\n')
                    .map(|nl| e + nl + 1)
                    .unwrap_or(rest.len())
            })
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
                .find(|c: char| {
                    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | ')' | ']' | '}' | ',' | ';')
                })
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
    format!("{}…[truncated {} bytes]", &text[..cut], text.len() - cut)
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

fn clamp_ns(nanos: i128) -> i64 {
    nanos.clamp(0, i64::MAX as i128) as i64
}

struct OpenTool {
    id: String,
    name: String,
    args: Value,
    skill: Option<String>,
    start_nanos: i128,
    ts_approx: bool,
    /// The event index at ToolUse time — the tool's identity in id
    /// derivation (the emission-time counter would collide).
    event_index: u64,
}

#[derive(Clone)]
struct PendingGen {
    text: String,
    model: Option<String>,
    thinking: Option<String>,
    usage: Vec<(String, i64)>,
    skill: Option<String>,
    tool_calls: Vec<String>,
    tool_only: bool,
    start_nanos: i128,
    end_nanos: i128,
    ts_approx: bool,
}

struct OpenTurn {
    ordinal: u64,
    trace_key: String,
    trace_id: String,
    start_nanos: i128,
    last_nanos: i128,
    user_text: Option<String>,
    last_assistant_text: Option<String>,
    open_tools: Vec<OpenTool>,
    /// Codex only: generations buffered until turn close so the late
    /// per-turn `token_count` usage can attach to the final one.
    pending_generations: Vec<PendingGen>,
    /// The last generation emitted in this turn, by index. A model-
    /// attributed token count that follows it belongs to *that* API call,
    /// so the usage joins the row instead of minting a second one.
    last_gen: Option<(usize, PendingGen)>,
    pending_turn_usage: Vec<(String, i64)>,
    pending_thinking: Vec<String>,
    seen_usage_msg_ids: std::collections::HashSet<String>,
    event_index: u64,
    gen_count: usize,
    skills: Vec<String>,
    tools_since_gen: Vec<String>,
    reported_duration_ms: Option<i64>,
    reported_message_count: Option<i64>,
    any_ts_approx: bool,
    aborted: bool,
    /// True for the turn left open when the prime pass ended: it is history
    /// and must not be emitted, and live events never join it.
    stale: bool,
    /// Hook-delivered facts: the prompt key, the exact end, the last
    /// message when the transcript gave none, and extra metadata
    /// (compaction, API errors).
    hook_turn_key: Option<String>,
    hook_end_ns: Option<i64>,
    hook_last_message: Option<String>,
    extra_metadata: serde_json::Map<String, Value>,
}

impl OpenTurn {
    fn note_skill(&mut self, skill: &Option<String>) {
        if let Some(s) = skill
            && !self.skills.iter().any(|k| k == s)
        {
            self.skills.push(s.clone());
        }
    }
}

/// An emitted generation, remembered by transcript step so usage that
/// arrives from a side channel (agy's conversation db) can be attached.
#[derive(Debug, Clone)]
struct StepGen {
    trace_id: String,
    obs_id: String,
    name: String,
    end_ns: i64,
}

/// Timing and outcome a hook reported for one tool call, by tool use id.
#[derive(Debug, Clone, Default, PartialEq)]
struct ToolHookState {
    start_ns: Option<i64>,
    end_ns: Option<i64>,
    is_error: bool,
    error: Option<String>,
}

/// A subagent known only from `SubagentStart`/`SubagentStop` hooks: its
/// own observation row, plus the turn it belongs to for eviction.
#[derive(Debug, Clone)]
struct HookAgent {
    row: ObservationRow,
    ordinal: u64,
    trace_key: String,
}

pub struct TurnAssembler {
    settings: MapSettings,
    session_id: Option<String>,
    /// "deterministic" | "watched" | "heuristic" | "none"
    correlation: String,
    transcript_path: Option<String>,
    ordinal: u64,
    ordinal_salted: bool,
    emitting: bool,
    explicit_boundaries: bool,
    turn: Option<OpenTurn>,
    last_event_nanos: i128,
    session_extra: Vec<(String, String)>,
    last_known_model: Option<String>,
    cost: Option<CostSnapshot>,
    /// Emitted generations by transcript step index (Antigravity).
    step_gens: std::collections::HashMap<u64, StepGen>,
    /// Usage that arrived before its generation was emitted.
    pending_step_usage: std::collections::HashMap<u64, GenUsage>,
    /// Hook pins by tool use id, applied to rows whenever they are built.
    hook_tools: HashMap<String, ToolHookState>,
    /// The last emitted row per tool use id (with its turn ordinal), so a
    /// hook arriving afterwards can re-emit it pinned.
    emitted_tools: std::collections::HashMap<String, (u64, ObservationRow)>,
    /// The last closed turns' rows, so a late `Stop` can still pin them.
    recent_traces: std::collections::HashMap<u64, TraceRow>,
    /// A `UserPromptSubmit` seen before its turn opened.
    pending_prompt: Option<(Option<String>, i64)>,
    /// Subagents announced by hooks, by agent id.
    hook_agents: HashMap<String, HookAgent>,
    /// Tool calls made *inside* a subagent (hook-only rows), by tool id.
    hook_agent_tools: HashMap<String, (u64, ObservationRow)>,
}

/// The CLI's own running session totals (Claude `cost-state`), surfaced on
/// the launch row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostSnapshot {
    pub total_cost_usd: Option<f64>,
    pub total_lines_added: Option<i64>,
    pub total_lines_removed: Option<i64>,
}

pub fn session_key(provider: Provider, session_id: &str) -> String {
    format!("{}:{session_id}", provider.as_str())
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
            cost: None,
            step_gens: std::collections::HashMap::new(),
            pending_step_usage: std::collections::HashMap::new(),
            hook_tools: std::collections::HashMap::new(),
            emitted_tools: std::collections::HashMap::new(),
            recent_traces: std::collections::HashMap::new(),
            pending_prompt: None,
            hook_agents: HashMap::new(),
            hook_agent_tools: HashMap::new(),
        }
    }

    pub fn cost_snapshot(&self) -> Option<CostSnapshot> {
        self.cost
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

    pub fn correlation(&self) -> &str {
        &self.correlation
    }

    /// Prime mode: state updates only, nothing emitted (resumed transcripts
    /// must never re-export history). Turning emitting back on marks any
    /// still-open primed turn stale so its eventual close stays silent.
    pub fn set_emitting(&mut self, emitting: bool) {
        if emitting
            && !self.emitting
            && let Some(turn) = self.turn.as_mut()
        {
            turn.stale = true;
        }
        self.emitting = emitting;
    }

    /// Marks the prime pass as byte-truncated: ordinals no longer count from
    /// file start, so trace ids get the launch salt.
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

    fn session_id_or_launch(&self) -> &str {
        self.session_id
            .as_deref()
            .unwrap_or(&self.settings.launch_id)
    }

    /// `"{provider}:{session_id}"`, the claim-registry key.
    pub fn session_key(&self) -> String {
        session_key(self.settings.provider, self.session_id_or_launch())
    }

    fn trace_key_for_ordinal(&self, ordinal: u64) -> String {
        let base = format!(
            "amx1|{}|{}|turn|{ordinal}",
            self.settings.provider.as_str(),
            self.session_id_or_launch()
        );
        if self.ordinal_salted {
            format!("{base}|{}", self.settings.launch_id)
        } else {
            base
        }
    }

    /// The session row as this assembler knows it (adoption facts plus
    /// whatever provider metadata has accumulated).
    pub fn session_row(&self, seen_ns: i64) -> SessionRow {
        let extra = if self.session_extra.is_empty() {
            None
        } else {
            let map: serde_json::Map<String, Value> = self
                .session_extra
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            Some(Value::Object(map))
        };
        SessionRow {
            key: self.session_key(),
            provider: self.settings.provider.as_str().to_string(),
            session_id: self.session_id_or_launch().to_string(),
            user_id: self.settings.user_id.clone(),
            cwd: Some(self.settings.cwd.clone()),
            project_slug: Some(self.settings.project_slug.clone()),
            transcript_path: self.transcript_path.clone(),
            title: None,
            seen_ns,
            extra,
        }
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
        let trace_id = ids::trace_id_hex(&trace_key);
        self.turn = Some(OpenTurn {
            ordinal: self.ordinal,
            trace_key,
            trace_id,
            start_nanos,
            last_nanos: start_nanos,
            user_text: None,
            last_assistant_text: None,
            open_tools: Vec::new(),
            pending_generations: Vec::new(),
            last_gen: None,
            pending_turn_usage: Vec::new(),
            pending_thinking: Vec::new(),
            seen_usage_msg_ids: std::collections::HashSet::new(),
            event_index: 0,
            gen_count: 0,
            skills: Vec::new(),
            tools_since_gen: Vec::new(),
            reported_duration_ms: None,
            reported_message_count: None,
            any_ts_approx: false,
            aborted: false,
            stale: false,
            hook_turn_key: None,
            hook_end_ns: None,
            hook_last_message: None,
            extra_metadata: serde_json::Map::new(),
        });
        // a prompt hook that preceded this turn's transcript line pins its start
        if let Some((key, ts)) = self.pending_prompt.take()
            && let Some(turn) = self.turn.as_mut()
        {
            let start = clamp_ns(turn.start_nanos);
            if prompt_pins(ts, start) {
                turn.start_nanos = i128::from(ts.min(start));
                turn.hook_turn_key = key;
            }
        }
        let current = self.ordinal;
        self.emitted_tools
            .retain(|_, (ordinal, _)| *ordinal + 1 >= current);
        self.recent_traces
            .retain(|ordinal, _| *ordinal + 2 >= current);
        self.hook_agents.retain(|_, a| a.ordinal + 2 >= current);
        self.hook_agent_tools
            .retain(|_, (ordinal, _)| *ordinal + 2 >= current);
    }

    /// A stale (primed-history) turn never receives live events: it is
    /// dropped silently and a fresh turn opens. Pushes the open trace row
    /// when a turn was actually opened.
    fn ensure_live_turn(&mut self, nanos: i128, ops: &mut Vec<StoreOp>) {
        if self.turn.as_ref().is_some_and(|t| t.stale) {
            self.turn = None;
        }
        if self.turn.is_none() {
            self.open_turn(nanos);
            self.push_trace_op(ops, TraceStatus::Open);
        }
    }

    fn turn_name(&self, turn: &OpenTurn) -> String {
        match (self.settings.content_mode, &turn.user_text) {
            // Mask BEFORE slicing: the trace name is the most visible string
            // in any viewer and must never carry a secret.
            (ContentMode::Full, Some(text)) => {
                let masked = mask_secrets(text, &self.settings.redact_literals);
                let first: String = masked.chars().take(80).collect();
                format!("{}: {first}", self.settings.profile_name)
            }
            _ => format!("turn {}", turn.ordinal),
        }
    }

    fn trace_row(&self, turn: &OpenTurn, status: TraceStatus) -> TraceRow {
        let closed = status != TraceStatus::Open;
        let mut metadata = serde_json::Map::new();
        if self.correlation == "heuristic" {
            metadata.insert("correlation".into(), Value::from("heuristic"));
        }
        if let Some(key) = &turn.hook_turn_key {
            metadata.insert("turn_key".into(), Value::from(key.clone()));
        }
        for (k, v) in &turn.extra_metadata {
            metadata.insert(k.clone(), v.clone());
        }
        let end_ns = closed.then(|| {
            let last = clamp_ns(turn.last_nanos);
            turn.hook_end_ns.map_or(last, |h| h.max(last))
        });
        TraceRow {
            id: turn.trace_id.clone(),
            session_key: self.session_key(),
            provider: self.settings.provider.as_str().to_string(),
            session_id: self.session_id_or_launch().to_string(),
            launch_id: Some(self.settings.launch_id.clone()),
            ordinal: turn.ordinal as i64,
            name: self.turn_name(turn),
            status,
            start_ns: clamp_ns(turn.start_nanos),
            end_ns,
            input: turn.user_text.as_ref().and_then(|t| self.full_content(t)),
            output: turn
                .last_assistant_text
                .as_ref()
                .and_then(|t| self.full_content(t))
                .or_else(|| {
                    if closed {
                        turn.hook_last_message.clone()
                    } else {
                        None
                    }
                }),
            thinking: if closed && !turn.pending_thinking.is_empty() {
                self.full_content(&turn.pending_thinking.join("\n"))
            } else {
                None
            },
            skills: (!turn.skills.is_empty()).then(|| turn.skills.clone()),
            reported_duration_ms: turn.reported_duration_ms,
            reported_message_count: turn.reported_message_count,
            session_cost_usd: if closed {
                self.cost.and_then(|c| c.total_cost_usd)
            } else {
                None
            },
            // Generation start times are always approximated from the
            // previous event's timestamp (transcript lines are written at
            // completion), so any turn containing a generation is flagged.
            timing_approx: turn.any_ts_approx || (closed && turn.gen_count > 0),
            ordinal_salted: self.ordinal_salted,
            metadata: (!metadata.is_empty()).then_some(Value::Object(metadata)),
        }
    }

    fn push_trace_op(&self, ops: &mut Vec<StoreOp>, status: TraceStatus) {
        if !self.emitting {
            return;
        }
        let Some(turn) = self.turn.as_ref() else {
            return;
        };
        if turn.stale {
            return;
        }
        ops.push(StoreOp::Trace(self.trace_row(turn, status)));
    }

    /// Full mode: the first prompt seen becomes the session title (the
    /// store keeps the first write).
    fn title_op(&self, turn: &OpenTurn) -> Option<StoreOp> {
        let text = turn.user_text.as_ref()?;
        if self.settings.content_mode != ContentMode::Full || !self.emitting {
            return None;
        }
        let masked = mask_secrets(text, &self.settings.redact_literals);
        let title: String = masked
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        if title.trim().is_empty() {
            return None;
        }
        let mut row = self.session_row(clamp_ns(turn.start_nanos));
        row.title = Some(title);
        Some(StoreOp::Session(row))
    }

    fn gen_row(&self, turn: &OpenTurn, index: usize, generation: &PendingGen) -> ObservationRow {
        let span_key = format!("{}|gen|{index}", turn.trace_key);
        let mut metadata = serde_json::Map::new();
        if !generation.tool_calls.is_empty() {
            metadata.insert(
                "tool_calls".into(),
                Value::from(generation.tool_calls.clone()),
            );
        }
        let (usage_raw, usage_norm) = if generation.usage.is_empty() {
            (None, None)
        } else {
            (
                Some(generation.usage.clone()),
                Some(usage::normalize(self.settings.provider, &generation.usage)),
            )
        };
        ObservationRow {
            id: ids::span_id_hex(&span_key),
            trace_id: turn.trace_id.clone(),
            parent_id: None,
            obs_type: ObservationType::Generation,
            name: if generation.tool_only {
                "assistant (tool use)".into()
            } else {
                "assistant".into()
            },
            kind: None,
            start_ns: clamp_ns(generation.start_nanos),
            end_ns: Some(clamp_ns(generation.end_nanos)),
            level: Level::Default,
            status_message: None,
            model: generation.model.clone(),
            input: turn.user_text.as_ref().and_then(|t| self.full_content(t)),
            output: if generation.text.is_empty() {
                None
            } else {
                self.full_content(&generation.text)
            },
            thinking: generation
                .thinking
                .as_ref()
                .and_then(|t| self.full_content(t)),
            usage_raw,
            usage: usage_norm,
            tool_id: None,
            tool_name: None,
            skill: generation.skill.clone(),
            mcp_server: None,
            path: None,
            is_error: false,
            ts_approx: generation.ts_approx,
            metadata,
        }
    }

    /// A generation row for usage no message-level generation took (a
    /// tool-only turn, Codex per-turn counts): usage never sits on a trace.
    fn usage_only_row(
        &self,
        turn: &OpenTurn,
        index: usize,
        usage: &[(String, i64)],
    ) -> ObservationRow {
        let generation = PendingGen {
            text: String::new(),
            model: self.last_known_model.clone(),
            thinking: None,
            usage: usage.to_vec(),
            skill: None,
            tool_calls: Vec::new(),
            tool_only: true,
            start_nanos: turn.start_nanos,
            end_nanos: turn.last_nanos,
            ts_approx: turn.any_ts_approx,
        };
        let mut row = self.gen_row(turn, index, &generation);
        row.name = "assistant (usage only)".into();
        row.kind = Some("usage_only".into());
        row.input = None;
        row
    }

    fn close_turn(&mut self) -> Vec<StoreOp> {
        let Some(mut turn) = self.turn.take() else {
            return Vec::new();
        };
        let mut ops = Vec::new();
        if !self.emitting || turn.stale {
            return ops;
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
            ops.push(StoreOp::Observation(self.gen_row(&turn, index, generation)));
        }
        if !turn.pending_turn_usage.is_empty() {
            let index = turn.gen_count;
            turn.gen_count += 1;
            let usage = std::mem::take(&mut turn.pending_turn_usage);
            ops.push(StoreOp::Observation(
                self.usage_only_row(&turn, index, &usage),
            ));
        }

        // Unpaired tools close with the turn.
        let unpaired: Vec<OpenTool> = std::mem::take(&mut turn.open_tools);
        for tool in &unpaired {
            let mut row = self.tool_row(&turn, tool, Some(end_nanos), None, None, false, true);
            let relinked = self.finish_tool_row(&mut row, turn.ordinal);
            ops.push(StoreOp::Observation(row));
            ops.extend(relinked.map(StoreOp::Observation));
        }

        let status = if turn.aborted {
            TraceStatus::Aborted
        } else {
            TraceStatus::Closed
        };
        let row = self.trace_row(&turn, status);
        self.recent_traces.insert(turn.ordinal, row.clone());
        ops.push(StoreOp::Trace(row));
        ops
    }

    /// Applies hook pins (exact start/end, failure) to a freshly built tool
    /// row and remembers it so a later hook can re-emit it.
    /// Applies any hook pins to a transcript tool row and remembers it.
    /// When the row is the transcript's view of a subagent the hooks
    /// already announced, the hook-created agent row is re-parented under
    /// it and returned so the caller can emit that update too.
    fn finish_tool_row(
        &mut self,
        row: &mut ObservationRow,
        ordinal: u64,
    ) -> Option<ObservationRow> {
        let id = row.tool_id.clone()?;
        if let Some(state) = self.hook_tools.get(&id) {
            apply_tool_pins(row, state);
        }
        self.emitted_tools.insert(id, (ordinal, row.clone()));
        let agent_id = row.metadata.get("agent_id")?.as_str()?;
        let agent = self.hook_agents.get_mut(agent_id)?;
        if agent.row.trace_id != row.trace_id || agent.row.parent_id.as_deref() == Some(&row.id) {
            return None;
        }
        agent.row.parent_id = Some(row.id.clone());
        Some(agent.row.clone())
    }

    /// The turn a hook at `ts_ns` belongs to, as (ordinal, trace_key,
    /// trace_id).
    fn hook_turn_ids(&self, ts_ns: i64) -> Option<(u64, String, String)> {
        match self.turn_for_ts(ts_ns) {
            HookTarget::Open => {
                let turn = self.turn.as_ref()?;
                Some((turn.ordinal, turn.trace_key.clone(), turn.trace_id.clone()))
            }
            HookTarget::Closed(ordinal) => {
                let key = self.trace_key_for_ordinal(ordinal);
                let id = ids::trace_id_hex(&key);
                Some((ordinal, key, id))
            }
            HookTarget::None => None,
        }
    }

    /// True when a transcript row already names this agent, so a hook
    /// row for it has something to attach to.
    fn knows_agent(&self, agent_id: &str) -> bool {
        self.emitted_tools
            .values()
            .any(|(_, row)| row.metadata.get("agent_id").and_then(|v| v.as_str()) == Some(agent_id))
    }

    /// `SubagentStart` / `SubagentStop`: an agent row of the hooks' own,
    /// parented under the transcript's Task row once that is known.
    fn subagent_hook(&mut self, ev: &HookEvent, ops: &mut Vec<StoreOp>) {
        let Some(agent_id) = ev.agent_id.clone() else {
            return;
        };
        let stopping = ev.event == "SubagentStop";
        // Claude fires SubagentStop but never SubagentStart, and its
        // payload can be empty: no type, no result. A stop for an agent
        // nothing else mentions — no Task row, no child tool calls — has
        // nothing to show, and inventing a zero-length nameless span for
        // it only adds noise to the turn it happened to land in. Count it
        // on the turn instead.
        if stopping && !self.hook_agents.contains_key(&agent_id) && !self.knows_agent(&agent_id) {
            let bare = ev.agent_type.is_none()
                && !ev.payload.contains_key("result")
                && !ev.payload.contains_key("last_assistant_message");
            if bare {
                if let Some(turn) = self.turn.as_mut() {
                    let seen = turn
                        .extra_metadata
                        .get("subagent_stops")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    turn.extra_metadata
                        .insert("subagent_stops".into(), Value::from(seen + 1));
                    self.push_trace_op(ops, TraceStatus::Open);
                }
                return;
            }
        }
        if !self.hook_agents.contains_key(&agent_id) {
            let Some((ordinal, trace_key, trace_id)) = self.hook_turn_ids(ev.ts_ns) else {
                return;
            };
            let parent_id = self
                .emitted_tools
                .values()
                .find(|(_, row)| {
                    row.trace_id == trace_id
                        && row.metadata.get("agent_id").and_then(|v| v.as_str()) == Some(&agent_id)
                })
                .map(|(_, row)| row.id.clone());
            let agent_type = ev.agent_type.clone();
            let mut metadata = serde_json::Map::new();
            metadata.insert("agent_id".into(), Value::from(agent_id.clone()));
            if let Some(t) = &agent_type {
                metadata.insert("agent_type".into(), Value::from(t.clone()));
            }
            metadata.insert("source".into(), Value::from("hook"));
            metadata.insert("hook_timed".into(), Value::Bool(true));
            let row = ObservationRow {
                id: ids::span_id_hex(&format!("{trace_key}|agent|{agent_id}")),
                trace_id,
                parent_id,
                obs_type: ObservationType::Agent,
                name: format!("agent: {}", agent_type.as_deref().unwrap_or("subagent")),
                kind: Some("agent_invocation".into()),
                start_ns: ev.ts_ns,
                end_ns: None,
                level: Level::Default,
                status_message: None,
                model: None,
                input: None,
                output: None,
                thinking: None,
                usage_raw: None,
                usage: None,
                tool_id: None,
                tool_name: None,
                skill: None,
                mcp_server: None,
                path: None,
                is_error: false,
                ts_approx: false,
                metadata,
            };
            self.hook_agents.insert(
                agent_id.clone(),
                HookAgent {
                    row,
                    ordinal,
                    trace_key,
                },
            );
        }
        let agent = self.hook_agents.get_mut(&agent_id).expect("just inserted");
        if let Some(path) = ev
            .payload
            .get("agent_transcript_path")
            .and_then(|v| v.as_str())
        {
            agent
                .row
                .metadata
                .insert("agent_transcript_path".into(), Value::from(path));
        }
        if stopping {
            agent.row.end_ns = Some(ev.ts_ns.max(agent.row.start_ns));
            if agent.row.output.is_none() {
                agent.row.output = ev
                    .payload
                    .get("last_assistant_message")
                    .or_else(|| ev.payload.get("result"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
            if let Some(reason) = ev.payload.get("stop_reason").and_then(|v| v.as_str()) {
                agent
                    .row
                    .metadata
                    .insert("stop_reason".into(), Value::from(reason));
            }
        }
        if self.emitting {
            ops.push(StoreOp::Observation(agent.row.clone()));
        }
    }

    /// A tool event carrying an `agent_id`: a call made inside a subagent,
    /// which the parent transcript never shows. Hook-only row under the
    /// agent row.
    fn subagent_tool_hook(&mut self, ev: &HookEvent, ops: &mut Vec<StoreOp>) {
        let (Some(agent_id), Some(tool_id)) = (ev.agent_id.clone(), ev.tool_use_id.clone()) else {
            return;
        };
        if !self.hook_agents.contains_key(&agent_id) {
            // a tool ran inside an agent we have not heard of: the child
            // is real, so give it a parent rather than dropping it
            let mut start = ev.clone();
            start.event = "SubagentStart".into();
            let mut ignored = Vec::new();
            self.subagent_hook(&start, &mut ignored);
            ops.append(&mut ignored);
        }
        let Some(agent) = self.hook_agents.get(&agent_id) else {
            return;
        };
        let (ordinal, parent_row) = (agent.ordinal, agent.row.clone());
        let trace_key = agent.trace_key.clone();
        let full = self.settings.content_mode == ContentMode::Full;
        let entry = self
            .hook_agent_tools
            .entry(tool_id.clone())
            .or_insert_with(|| {
                let name = ev.tool_name.clone().unwrap_or_else(|| "tool".into());
                let mut metadata = serde_json::Map::new();
                metadata.insert("agent_id".into(), Value::from(agent_id.clone()));
                metadata.insert("source".into(), Value::from("hook"));
                metadata.insert("hook_timed".into(), Value::Bool(true));
                let (display, mcp_server) = match split_mcp_name(&name) {
                    Some((server, tool)) => (format!("mcp: {server}/{tool}"), Some(server)),
                    None => (name.clone(), None),
                };
                (
                    ordinal,
                    ObservationRow {
                        id: ids::span_id_hex(&format!(
                            "{trace_key}|agent|{agent_id}|tool|{tool_id}"
                        )),
                        trace_id: parent_row.trace_id.clone(),
                        parent_id: Some(parent_row.id.clone()),
                        obs_type: ObservationType::Tool,
                        name: display,
                        kind: None,
                        start_ns: ev.ts_ns,
                        end_ns: None,
                        level: Level::Default,
                        status_message: None,
                        model: None,
                        input: None,
                        output: None,
                        thinking: None,
                        usage_raw: None,
                        usage: None,
                        tool_id: Some(tool_id.clone()),
                        tool_name: Some(name),
                        skill: None,
                        mcp_server,
                        path: None,
                        is_error: false,
                        ts_approx: false,
                        metadata,
                    },
                )
            });
        let row = &mut entry.1;
        let as_text = |v: &Value| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        match ev.event.as_str() {
            "PreToolUse" => {
                row.start_ns = ev.ts_ns;
                if let Some(end) = row.end_ns
                    && end < row.start_ns
                {
                    row.end_ns = Some(row.start_ns);
                }
            }
            _ => {
                row.end_ns = Some(ev.ts_ns.max(row.start_ns));
                if ev.is_error {
                    row.is_error = true;
                    row.level = Level::Error;
                    row.status_message = Some("tool error".into());
                    if let Some(err) = ev.payload.get("tool_error").and_then(|v| v.as_str()) {
                        row.metadata.insert("tool_error".into(), Value::from(err));
                    }
                }
                if full && row.output.is_none() {
                    row.output = ev.payload.get("tool_response").map(as_text);
                }
            }
        }
        if full && row.input.is_none() {
            row.input = ev.payload.get("tool_input").map(as_text);
        }
        if self.emitting {
            ops.push(StoreOp::Observation(row.clone()));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tool_row(
        &self,
        turn: &OpenTurn,
        tool: &OpenTool,
        end_nanos: Option<i128>,
        content: Option<&Value>,
        structured: Option<&Value>,
        is_error: bool,
        unpaired: bool,
    ) -> ObservationRow {
        // Keyed to the ToolUse event's own index (+ call id): stable for the
        // open/closed writes of one call, distinct for same-name tools.
        let span_key = format!(
            "{}|tool|{}|{}|{}",
            turn.trace_key, tool.name, tool.id, tool.event_index
        );
        let mut metadata = serde_json::Map::new();
        let mut name = tool.name.clone();
        let mut obs_type = ObservationType::Tool;
        let mut kind: Option<String> = None;
        let mut mcp_server = None;
        let full = self.settings.content_mode == ContentMode::Full;

        if let Some((server, mcp_tool)) = split_mcp_name(&tool.name) {
            // MCP server tools are never subagents/skills, whatever their
            // short name matches
            name = format!("mcp: {server}/{mcp_tool}");
            metadata.insert("mcp_tool".into(), Value::from(mcp_tool));
            mcp_server = Some(server);
            kind = Some("mcp_tool".into());
        } else if let Some(agent) = extract_agent_info(&tool.name, &tool.args) {
            obs_type = ObservationType::Agent;
            name = agent.name;
            if let Some(role) = agent.role {
                metadata.insert("agent_role".into(), Value::from(role));
            }
            if let Some(type_name) = agent.type_name {
                metadata.insert("agent_type".into(), Value::from(type_name));
            }
            if let Some(model) = agent.model {
                metadata.insert("agent_model".into(), Value::from(model));
            }
            if full
                && let Some(prompt) = &agent.prompt
                && let Some(p) = self.full_content(prompt)
            {
                metadata.insert("agent_prompt".into(), Value::from(p));
            }
            kind = Some("agent_invocation".into());
        } else if let Some((skill_name, skill_path)) = extract_skill_info(&tool.name, &tool.args) {
            name = format!("skill: {skill_name}");
            metadata.insert("skill_name".into(), Value::from(skill_name));
            if !skill_path.is_empty() {
                metadata.insert("skill_path".into(), Value::from(skill_path));
            }
            kind = Some("skill_load".into());
        }

        // Content-bearing argument extracts (summaries, commands, queries)
        // are full-mode only and pass the masker like every other content
        // string; a bare path is structure (cwd already ships on the launch).
        if full {
            let extract = |keys: &[&str]| {
                keys.iter()
                    .find_map(|k| tool.args.get(*k))
                    .and_then(clean_json_str)
                    .and_then(|s| self.full_content(&s))
            };
            if let Some(summary) = extract(&["toolSummary", "summary"]) {
                metadata.insert("summary".into(), Value::from(summary));
            }
            if let Some(action) = extract(&["toolAction", "action", "Description"]) {
                metadata.insert("action".into(), Value::from(action));
            }
            if let Some(cmd) = extract(&["CommandLine", "command", "cmd"]) {
                metadata.insert("command".into(), Value::from(cmd));
            }
            if let Some(query) = extract(&["Query", "query", "pattern"]) {
                metadata.insert("query".into(), Value::from(query));
            }
        }
        let path = [
            "AbsolutePath",
            "TargetFile",
            "DirectoryPath",
            "path",
            "file_path",
        ]
        .iter()
        .find_map(|k| tool.args.get(*k))
        .and_then(clean_json_str);

        if let Some(s) = structured {
            self.push_structured_result(&mut metadata, s);
        }

        let (input, output) = if full {
            let args_str = match &tool.args {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            let input = if args_str.is_empty() {
                None
            } else {
                self.full_content(&args_str)
            };
            let output = content.and_then(|c| {
                let out_str = match c {
                    Value::String(s) => s.clone(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };
                self.full_content(&out_str)
            });
            (input, output)
        } else {
            (None, None)
        };
        // a tool the user declined is a decision, not a failure: Claude
        // marks the result `is_error`, but showing it red beside real
        // crashes (and counting it in the turn's error total) misreads
        // what happened
        let declined = is_error && content.is_some_and(user_declined);
        let is_error = is_error && !declined;
        let status_message = if unpaired {
            Some("no result observed".to_string())
        } else if declined {
            Some("declined by the user".to_string())
        } else if is_error {
            // say what failed, not just that something did — a red row
            // whose message is "tool error" explains nothing, and in
            // metadata mode the body that would have explained it is not
            // stored at all
            Some(error_summary(content).unwrap_or_else(|| "tool error".to_string()))
        } else {
            None
        };
        ObservationRow {
            id: ids::span_id_hex(&span_key),
            trace_id: turn.trace_id.clone(),
            parent_id: None,
            obs_type,
            name,
            kind,
            start_ns: clamp_ns(tool.start_nanos),
            end_ns: end_nanos.map(clamp_ns),
            level: if is_error {
                Level::Error
            } else if declined {
                Level::Warning
            } else {
                Level::Default
            },
            status_message,
            model: None,
            input,
            output,
            thinking: None,
            usage_raw: None,
            usage: None,
            tool_id: (!tool.id.is_empty()).then(|| tool.id.clone()),
            tool_name: Some(tool.name.clone()),
            skill: tool.skill.clone(),
            mcp_server,
            path,
            is_error,
            ts_approx: tool.ts_approx,
            metadata,
        }
    }

    /// Facts distilled from Claude's structured `toolUseResult`. Execution
    /// facts (sizes, interruption, patch stats, agent identity) are
    /// structure and flow in both content modes; free-text fields are
    /// full-mode only and masked.
    fn push_structured_result(&self, meta: &mut serde_json::Map<String, Value>, s: &Value) {
        fn put(meta: &mut serde_json::Map<String, Value>, key: &str, value: Value) {
            meta.entry(key.to_string()).or_insert(value);
        }
        if let Some(stdout) = s.get("stdout").and_then(|v| v.as_str()) {
            put(meta, "stdout_bytes", Value::from(stdout.len() as i64));
        }
        if let Some(stderr) = s.get("stderr").and_then(|v| v.as_str())
            && !stderr.is_empty()
        {
            put(meta, "stderr_bytes", Value::from(stderr.len() as i64));
        }
        if s.get("interrupted").and_then(|v| v.as_bool()) == Some(true) {
            put(meta, "interrupted", Value::Bool(true));
        }
        if let Some(rci) = s.get("returnCodeInterpretation").and_then(|v| v.as_str()) {
            put(meta, "return_code", Value::from(rci));
        }
        if let Some(hunks) = s.get("structuredPatch").and_then(|v| v.as_array()) {
            let (mut added, mut removed) = (0i64, 0i64);
            for hunk in hunks {
                if let Some(lines) = hunk.get("lines").and_then(|l| l.as_array()) {
                    for line in lines.iter().filter_map(|l| l.as_str()) {
                        if line.starts_with('+') {
                            added += 1;
                        } else if line.starts_with('-') {
                            removed += 1;
                        }
                    }
                }
            }
            if added > 0 || removed > 0 {
                put(meta, "lines_added", Value::from(added));
                put(meta, "lines_removed", Value::from(removed));
            }
            if s.get("userModified").and_then(|v| v.as_bool()) == Some(true) {
                put(meta, "user_modified", Value::Bool(true));
            }
        }
        if let Some(agent_id) = s.get("agentId").and_then(|v| v.as_str()) {
            put(meta, "agent_id", Value::from(agent_id));
            if let Some(model) = s.get("resolvedModel").and_then(|v| v.as_str()) {
                put(meta, "agent_model", Value::from(model));
            }
            if let Some(status) = s.get("status").and_then(|v| v.as_str()) {
                put(meta, "agent_status", Value::from(status));
            }
            if s.get("isAsync").and_then(|v| v.as_bool()) == Some(true) {
                put(meta, "agent_async", Value::Bool(true));
            }
            if let Some(out) = s.get("outputFile").and_then(|v| v.as_str()) {
                put(meta, "agent_output_file", Value::from(out));
            }
            if self.settings.content_mode == ContentMode::Full
                && let Some(desc) = s.get("description").and_then(|v| v.as_str())
                && let Some(d) = self.full_content(desc)
            {
                put(meta, "summary", Value::from(d));
            }
        }
        if let Some(wf) = s.get("workflowName").and_then(|v| v.as_str()) {
            put(meta, "workflow_name", Value::from(wf));
            if let Some(run) = s.get("runId").and_then(|v| v.as_str()) {
                put(meta, "workflow_run_id", Value::from(run));
            }
            if let Some(status) = s.get("status").and_then(|v| v.as_str()) {
                put(meta, "workflow_status", Value::from(status));
            }
        }
    }

    /// Feeds one event. `recv_nanos` is the wall-clock fallback for events
    /// with a missing/unparseable timestamp.
    pub fn feed(&mut self, event: TranscriptEvent, recv_nanos: i128) -> Vec<StoreOp> {
        let mut ops = Vec::new();
        match event {
            TranscriptEvent::SessionMeta {
                session_id,
                cwd: _,
                extra,
                ts,
            } => {
                let (nanos, _) = self.event_nanos(&ts, recv_nanos);
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
                            Value::String(s) => s.clone(),
                            other => serde_json::to_string(other).unwrap_or_default(),
                        };
                        if let Some(entry) = self.session_extra.iter_mut().find(|(ek, _)| ek == k) {
                            entry.1 = val;
                        } else {
                            self.session_extra.push((k.clone(), val));
                        }
                    }
                }
                if self.emitting && self.session_id.is_some() && !self.session_extra.is_empty() {
                    ops.push(StoreOp::Session(self.session_row(clamp_ns(nanos))));
                }
            }
            TranscriptEvent::TurnBoundary { kind, ts } => {
                let (nanos, _) = self.event_nanos(&ts, recv_nanos);
                self.explicit_boundaries = true;
                match kind {
                    TurnBoundaryKind::Start => {
                        ops.extend(self.close_turn());
                        self.open_turn(nanos);
                        self.push_trace_op(&mut ops, TraceStatus::Open);
                    }
                    TurnBoundaryKind::Complete => {
                        if let Some(turn) = self.turn.as_mut() {
                            turn.last_nanos = nanos;
                        }
                        ops.extend(self.close_turn());
                    }
                    TurnBoundaryKind::Aborted => {
                        if let Some(turn) = self.turn.as_mut() {
                            turn.last_nanos = nanos;
                            turn.aborted = true;
                        }
                        ops.extend(self.close_turn());
                    }
                }
            }
            TranscriptEvent::User { text, meta, ts } => {
                let (nanos, approx) = self.event_nanos(&ts, recv_nanos);
                if meta {
                    // Harness chatter written as a user line belongs to
                    // whatever turn is running and never opens/closes one.
                    if self.turn.as_ref().is_some_and(|t| t.stale) {
                        self.turn = None;
                    }
                    if let Some(turn) = self.turn.as_mut() {
                        turn.last_nanos = nanos;
                        turn.any_ts_approx |= approx;
                        turn.event_index += 1;
                    }
                    return ops;
                }
                if self.explicit_boundaries {
                    if self.turn.as_ref().is_some_and(|t| t.stale) {
                        self.turn = None;
                    }
                    if self.turn.is_none() {
                        self.open_turn(nanos);
                    }
                } else {
                    ops.extend(self.close_turn());
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
                self.push_trace_op(&mut ops, TraceStatus::Open);
                if let Some(op) = self.turn.as_ref().and_then(|t| self.title_op(t)) {
                    ops.push(op);
                }
            }
            TranscriptEvent::Assistant {
                text,
                model,
                thinking,
                mut usage,
                msg_id,
                skill,
                step_index,
                ts,
            } => {
                let prev_nanos = self.last_event_nanos;
                let (nanos, approx) = self.event_nanos(&ts, recv_nanos);
                self.ensure_live_turn(nanos, &mut ops);
                let mut emit_now: Option<(usize, PendingGen)> = None;
                if let Some(turn) = self.turn.as_mut() {
                    turn.note_skill(&skill);
                    turn.tools_since_gen.clear();
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
                    let tool_only = text.is_empty();
                    let generation = PendingGen {
                        text: text.clone(),
                        model: model.or_else(|| self.last_known_model.clone()),
                        thinking,
                        usage,
                        skill,
                        tool_calls: Vec::new(),
                        tool_only,
                        start_nanos: start,
                        end_nanos: nanos,
                        ts_approx: approx,
                    };
                    if self.settings.provider == Provider::Codex {
                        turn.pending_generations.push(generation);
                    } else {
                        let index = turn.gen_count;
                        turn.gen_count += 1;
                        turn.last_gen = Some((index, generation.clone()));
                        emit_now = Some((index, generation));
                    }
                    if !text.is_empty() {
                        turn.last_assistant_text = Some(text);
                    }
                    turn.last_nanos = nanos;
                    turn.any_ts_approx |= approx;
                    turn.event_index += 1;
                }
                if let (Some((index, generation)), Some(turn)) = (emit_now, self.turn.as_ref())
                    && self.emitting
                    && !turn.stale
                {
                    let row = self.gen_row(turn, index, &generation);
                    if let Some(step) = step_index {
                        self.step_gens.insert(
                            step,
                            StepGen {
                                trace_id: row.trace_id.clone(),
                                obs_id: row.id.clone(),
                                name: row.name.clone(),
                                end_ns: row.end_ns.unwrap_or(row.start_ns),
                            },
                        );
                    }
                    ops.push(StoreOp::Observation(row));
                    // usage that arrived ahead of its transcript line
                    if let Some(step) = step_index
                        && let Some(record) = self.pending_step_usage.remove(&step)
                    {
                        ops.extend(self.attach_step_usage(&record));
                    }
                }
            }
            TranscriptEvent::ToolUse {
                id,
                name,
                args,
                skill,
                ts,
            } => {
                let (nanos, approx) = self.event_nanos(&ts, recv_nanos);
                self.ensure_live_turn(nanos, &mut ops);
                if let Some(turn) = self.turn.as_mut() {
                    turn.note_skill(&skill);
                    turn.tools_since_gen.push(name.clone());
                    turn.event_index += 1;
                    turn.open_tools.push(OpenTool {
                        id,
                        name,
                        args,
                        skill,
                        start_nanos: nanos,
                        ts_approx: approx,
                        event_index: turn.event_index,
                    });
                    turn.last_nanos = nanos;
                    turn.any_ts_approx |= approx;
                }
                // the in-flight row: visible while the tool runs
                let built = match self.turn.as_ref() {
                    Some(turn) if self.emitting && !turn.stale => {
                        turn.open_tools.last().map(|tool| {
                            (
                                turn.ordinal,
                                self.tool_row(turn, tool, None, None, None, false, false),
                            )
                        })
                    }
                    _ => None,
                };
                if let Some((ordinal, mut row)) = built {
                    let relinked = self.finish_tool_row(&mut row, ordinal);
                    ops.push(StoreOp::Observation(row));
                    ops.extend(relinked.map(StoreOp::Observation));
                }
            }
            TranscriptEvent::ToolResult {
                id,
                content,
                is_error,
                structured,
                ts,
            } => {
                let (nanos, approx) = self.event_nanos(&ts, recv_nanos);
                self.ensure_live_turn(nanos, &mut ops);
                let mut popped: Option<(OpenTool, bool)> = None; // (tool, orphan)
                if let Some(turn_ref) = self.turn.as_mut() {
                    turn_ref.last_nanos = nanos;
                    turn_ref.any_ts_approx |= approx;
                    turn_ref.event_index += 1;
                    // pair by id when the provider has ids, else FIFO
                    let idx = if id.is_empty() {
                        if turn_ref.open_tools.is_empty() {
                            None
                        } else {
                            Some(0)
                        }
                    } else {
                        turn_ref.open_tools.iter().position(|t| t.id == id)
                    };
                    popped = Some(match idx {
                        Some(i) => (turn_ref.open_tools.remove(i), false),
                        // orphan result: synthesize a zero-duration row
                        // rather than dropping the data
                        None => (
                            OpenTool {
                                id,
                                name: "unknown".into(),
                                args: Value::Null,
                                skill: None,
                                start_nanos: nanos,
                                ts_approx: approx,
                                event_index: turn_ref.event_index,
                            },
                            true,
                        ),
                    });
                }
                let built = match (popped.as_ref(), self.turn.as_ref()) {
                    (Some((tool, orphan)), Some(turn)) if self.emitting && !turn.stale => {
                        let mut row = self.tool_row(
                            turn,
                            tool,
                            Some(nanos),
                            Some(&content),
                            structured.as_ref(),
                            is_error,
                            false,
                        );
                        if *orphan {
                            row.status_message = Some("unpaired result".to_string());
                        }
                        Some((turn.ordinal, row))
                    }
                    _ => None,
                };
                if let Some((ordinal, mut row)) = built {
                    let relinked = self.finish_tool_row(&mut row, ordinal);
                    ops.push(StoreOp::Observation(row));
                    ops.extend(relinked.map(StoreOp::Observation));
                }
            }
            TranscriptEvent::Thinking { text, ts } => {
                let (nanos, approx) = self.event_nanos(&ts, recv_nanos);
                self.ensure_live_turn(nanos, &mut ops);
                if let Some(turn) = self.turn.as_mut() {
                    turn.pending_thinking.push(text);
                    turn.last_nanos = nanos;
                    turn.any_ts_approx |= approx;
                }
            }
            TranscriptEvent::TokenCount {
                usage,
                msg_id,
                model,
                ts,
            } => {
                let prev_nanos = self.last_event_nanos;
                let (nanos, approx) = self.event_nanos(&ts, recv_nanos);
                self.ensure_live_turn(nanos, &mut ops);
                let mut emit_now: Option<(usize, PendingGen)> = None;
                if let Some(turn) = self.turn.as_mut() {
                    let already_charged = msg_id
                        .as_ref()
                        .is_some_and(|id| !turn.seen_usage_msg_ids.insert(id.clone()));
                    let tool_calls = std::mem::take(&mut turn.tools_since_gen);
                    if !already_charged {
                        // A model-attributed count is a real API call (a
                        // Claude tool_use-only message): mint a generation
                        // so it is priced. Codex's per-turn counts keep the
                        // buffered path.
                        // Claude reports an API call's usage on the line
                        // after the message it belongs to. If the last
                        // generation is still waiting for its usage, this
                        // is it: one call stays one row, carrying both its
                        // text and its tokens. Minting a second row here
                        // is what left text rows looking empty and their
                        // twins textless.
                        let joins_last = self.settings.provider != Provider::Codex
                            && model.is_some()
                            && turn
                                .last_gen
                                .as_ref()
                                .is_some_and(|(_, g)| g.usage.is_empty());
                        if joins_last {
                            let (index, mut generation) =
                                turn.last_gen.take().expect("checked above");
                            generation.usage = usage;
                            if generation.model.is_none() {
                                generation.model = model;
                            }
                            if generation.thinking.is_none() && !turn.pending_thinking.is_empty() {
                                generation.thinking = Some(turn.pending_thinking.join("\n"));
                                turn.pending_thinking.clear();
                            }
                            generation.tool_calls.extend(tool_calls);
                            // the call finished when its count arrived
                            generation.end_nanos = generation.end_nanos.max(nanos);
                            turn.last_gen = Some((index, generation.clone()));
                            emit_now = Some((index, generation));
                        } else if self.settings.provider != Provider::Codex
                            && let Some(model_name) = model
                        {
                            let start = if prev_nanos > 0 && prev_nanos <= nanos {
                                prev_nanos
                            } else {
                                nanos
                            };
                            let mut thinking = None;
                            if !turn.pending_thinking.is_empty() {
                                thinking = Some(turn.pending_thinking.join("\n"));
                                turn.pending_thinking.clear();
                            }
                            let generation = PendingGen {
                                text: String::new(),
                                model: Some(model_name),
                                thinking,
                                usage,
                                skill: None,
                                tool_calls,
                                tool_only: true,
                                start_nanos: start,
                                end_nanos: nanos,
                                ts_approx: approx,
                            };
                            let index = turn.gen_count;
                            turn.gen_count += 1;
                            turn.last_gen = Some((index, generation.clone()));
                            emit_now = Some((index, generation));
                        } else {
                            accumulate_usage(&mut turn.pending_turn_usage, &usage);
                        }
                    }
                    turn.last_nanos = nanos;
                }
                if let (Some((index, generation)), Some(turn)) = (emit_now, self.turn.as_ref())
                    && self.emitting
                    && !turn.stale
                {
                    ops.push(StoreOp::Observation(self.gen_row(turn, index, &generation)));
                }
            }
            TranscriptEvent::TurnDuration {
                duration_ms,
                message_count,
                ts,
            } => {
                let (nanos, _) = self.event_nanos(&ts, recv_nanos);
                if let Some(turn) = self.turn.as_mut() {
                    turn.reported_duration_ms = Some(duration_ms);
                    turn.reported_message_count = message_count;
                    turn.last_nanos = turn.last_nanos.max(nanos);
                }
            }
            TranscriptEvent::CostState {
                total_cost_usd,
                total_lines_added,
                total_lines_removed,
                ts,
            } => {
                let _ = self.event_nanos(&ts, recv_nanos);
                self.cost = Some(CostSnapshot {
                    total_cost_usd,
                    total_lines_added,
                    total_lines_removed,
                });
            }
        }
        ops
    }

    /// Attaches a per-request usage record (agy's conversation db) to the
    /// generation emitted for its transcript step: an upsert of the same
    /// observation id carrying the real model id, the token buckets, and
    /// timing facts. Records whose generation has not been emitted yet are
    /// held until it is; records for steps that never produce a generation
    /// (history primed on resume) stay parked and cost nothing.
    pub fn attach_step_usage(&mut self, record: &GenUsage) -> Vec<StoreOp> {
        if !record.has_usage() && record.model.is_none() {
            return Vec::new();
        }
        let hit = record
            .steps
            .iter()
            .find_map(|step| self.step_gens.get(step).cloned());
        match hit {
            Some(sg) if self.emitting => {
                vec![StoreOp::Observation(self.usage_update_row(&sg, record))]
            }
            Some(_) => Vec::new(),
            None => {
                if let Some(first) = record.steps.first() {
                    self.pending_step_usage.insert(*first, record.clone());
                }
                Vec::new()
            }
        }
    }

    fn usage_update_row(&self, sg: &StepGen, record: &GenUsage) -> ObservationRow {
        let raw = record.raw_usage();
        let normalized = if raw.is_empty() {
            None
        } else {
            Some(usage::normalize(self.settings.provider, &raw))
        };
        let mut metadata = serde_json::Map::new();
        metadata.insert("usage_source".into(), Value::from("agy_conversation_db"));
        if let Some(ns) = record.latency_ns {
            metadata.insert("latency_ms".into(), Value::from(ns / 1_000_000));
        }
        if let Some(ns) = record.first_token_ns {
            metadata.insert("first_token_ms".into(), Value::from(ns / 1_000_000));
        }
        if let Some(w) = record.context_window {
            metadata.insert("context_window".into(), Value::from(w));
        }
        if record.context_tokens.is_some() && record.prompt_tokens.is_some() {
            metadata.insert("cache_read_derived".into(), Value::Bool(true));
        }
        // the transcript line is written at completion; agy's latency gives
        // the real start
        let start_ns = match record.latency_ns {
            Some(l) if l > 0 && l < sg.end_ns => sg.end_ns - l,
            _ => sg.end_ns,
        };
        ObservationRow {
            id: sg.obs_id.clone(),
            trace_id: sg.trace_id.clone(),
            parent_id: None,
            obs_type: ObservationType::Generation,
            name: sg.name.clone(),
            kind: None,
            start_ns,
            end_ns: Some(sg.end_ns),
            level: Level::Default,
            status_message: None,
            model: record.model.clone(),
            input: None,
            output: None,
            thinking: None,
            usage_raw: (!raw.is_empty()).then_some(raw),
            usage: normalized,
            tool_id: None,
            tool_name: None,
            skill: None,
            mcp_server: None,
            path: None,
            is_error: false,
            ts_approx: false,
            metadata,
        }
    }

    /// Which turn a turn-level hook belongs to: the open turn when it
    /// started before the hook fired, else the latest recently closed turn
    /// that did. Hooks are polled after the transcript is tailed, so a
    /// `Stop` for turn N can arrive after the transcript opened turn N+1;
    /// the timestamp keeps it on N.
    fn turn_for_ts(&self, ts_ns: i64) -> HookTarget {
        if let Some(turn) = self.turn.as_ref().filter(|t| !t.stale)
            && clamp_ns(turn.start_nanos) <= ts_ns
        {
            return HookTarget::Open;
        }
        self.recent_traces
            .iter()
            .filter(|(_, row)| row.start_ns <= ts_ns)
            .map(|(ordinal, _)| *ordinal)
            .max()
            .map_or(HookTarget::None, HookTarget::Closed)
    }

    /// Attaches one hook event: exact tool start/end and failures by tool
    /// use id, prompt and turn-end times by turn, API failures and
    /// interrupts as aborted turns, compaction and model switches as
    /// metadata. Rows the transcript has not produced yet are pinned when
    /// they appear; rows already emitted are re-emitted with the pin.
    pub fn attach_hook_event(&mut self, ev: &HookEvent) -> Vec<StoreOp> {
        let mut ops = Vec::new();
        match ev.event.as_str() {
            "SubagentStart" | "SubagentStop" => self.subagent_hook(ev, &mut ops),
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" if ev.agent_id.is_some() => {
                self.subagent_tool_hook(ev, &mut ops)
            }
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => {
                let Some(id) = ev.tool_use_id.clone() else {
                    return ops;
                };
                let state = self.hook_tools.entry(id.clone()).or_default();
                match ev.event.as_str() {
                    "PreToolUse" => state.start_ns = Some(ev.ts_ns),
                    _ => {
                        state.end_ns = Some(ev.ts_ns);
                        if ev.is_error {
                            state.is_error = true;
                            state.error = ev
                                .payload
                                .get("tool_error")
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                        }
                    }
                }
                let state = state.clone();
                if self.emitting
                    && let Some((_, row)) = self.emitted_tools.get(&id)
                {
                    let mut pinned = row.clone();
                    apply_tool_pins(&mut pinned, &state);
                    if let Some(entry) = self.emitted_tools.get_mut(&id) {
                        entry.1 = pinned.clone();
                    }
                    ops.push(StoreOp::Observation(pinned));
                }
            }
            "UserPromptSubmit" => {
                let key = ev.turn_key.clone();
                let ts = ev.ts_ns;
                let mut pinned_open = false;
                if let Some(turn) = self.turn.as_mut()
                    && !turn.stale
                    && turn.user_text.is_some()
                    && turn.hook_turn_key.is_none()
                {
                    let start = clamp_ns(turn.start_nanos);
                    if prompt_pins(ts, start) {
                        turn.start_nanos = i128::from(ts.min(start));
                        turn.hook_turn_key = key.clone();
                        pinned_open = true;
                    }
                }
                if pinned_open {
                    self.push_trace_op(&mut ops, TraceStatus::Open);
                } else {
                    self.pending_prompt = Some((key, ts));
                }
            }
            "Stop" | "TurnComplete" => {
                let turn_number = ev.payload.get("turn_number").and_then(|v| v.as_i64());
                let last_message = ev
                    .payload
                    .get("last_assistant_message")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                match self.turn_for_ts(ev.ts_ns) {
                    HookTarget::Open => {
                        let turn = self.turn.as_mut().expect("open turn");
                        turn.hook_end_ns =
                            Some(turn.hook_end_ns.map_or(ev.ts_ns, |e| e.max(ev.ts_ns)));
                        if turn.hook_last_message.is_none() {
                            turn.hook_last_message = last_message;
                        }
                        if let Some(n) = turn_number
                            && n != turn.ordinal as i64
                        {
                            turn.extra_metadata
                                .insert("turn_number_hook".into(), Value::from(n));
                        }
                    }
                    HookTarget::Closed(ordinal) if self.emitting => {
                        // the transcript closed the turn first: pin its end
                        let row = self.recent_traces.get_mut(&ordinal).expect("recent trace");
                        row.end_ns = Some(row.end_ns.map_or(ev.ts_ns, |e| e.max(ev.ts_ns)));
                        if row.output.is_none() {
                            row.output = last_message;
                        }
                        if let Some(n) = turn_number
                            && n != row.ordinal
                        {
                            let mut meta = row
                                .metadata
                                .as_ref()
                                .and_then(|m| m.as_object().cloned())
                                .unwrap_or_default();
                            meta.insert("turn_number_hook".into(), Value::from(n));
                            row.metadata = Some(Value::Object(meta));
                        }
                        ops.push(StoreOp::Trace(row.clone()));
                    }
                    _ => {}
                }
            }
            "StopFailure" | "Interrupt" => {
                let key = if ev.event == "Interrupt" {
                    "interrupted"
                } else {
                    "api_error"
                };
                let value = if ev.event == "Interrupt" {
                    Value::Bool(true)
                } else {
                    Value::from(
                        ev.payload
                            .get("error_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown"),
                    )
                };
                match self.turn_for_ts(ev.ts_ns) {
                    HookTarget::Open => {
                        let turn = self.turn.as_mut().expect("open turn");
                        turn.aborted = true;
                        turn.hook_end_ns = Some(ev.ts_ns);
                        turn.extra_metadata.insert(key.into(), value);
                        ops.extend(self.close_turn());
                    }
                    HookTarget::Closed(ordinal) if self.emitting => {
                        let row = self.recent_traces.get_mut(&ordinal).expect("recent trace");
                        row.status = TraceStatus::Aborted;
                        let mut meta = row
                            .metadata
                            .as_ref()
                            .and_then(|m| m.as_object().cloned())
                            .unwrap_or_default();
                        meta.insert(key.into(), value);
                        row.metadata = Some(Value::Object(meta));
                        ops.push(StoreOp::Trace(row.clone()));
                    }
                    _ => {}
                }
            }
            "PostCompact" => {
                if let Some(turn) = self.turn.as_mut().filter(|t| !t.stale) {
                    turn.extra_metadata
                        .insert("compacted".into(), Value::Bool(true));
                    self.push_trace_op(&mut ops, TraceStatus::Open);
                }
            }
            "PostModelSwitch" => {
                if let Some(model) = &ev.model {
                    self.last_known_model = Some(model.clone());
                    if let Some(entry) = self.session_extra.iter_mut().find(|(k, _)| k == "model") {
                        entry.1 = model.clone();
                    } else {
                        self.session_extra.push(("model".into(), model.clone()));
                    }
                    if self.emitting && self.session_id.is_some() {
                        ops.push(StoreOp::Session(self.session_row(ev.ts_ns)));
                    }
                }
            }
            // SessionStart / SessionEnd are launch-level (pipeline);
            // subagent and agy invocation events land in later tasks
            _ => {}
        }
        ops
    }

    /// Closes any open turn (session over).
    pub fn finalize(&mut self) -> Vec<StoreOp> {
        self.close_turn()
    }
}

/// A prompt hook may precede its transcript line by at most this much.
const PROMPT_PIN_WINDOW_NS: i64 = 60 * 1_000_000_000;
/// A prompt hook normally precedes the transcript's user line, but a
/// CLI that stamps the line first (or a coarse clock) lands it a moment
/// later; the pin still applies within this slack.
const PROMPT_PIN_SLACK_NS: i64 = 2_000_000_000;

/// True when a `UserPromptSubmit` at `ts` belongs to a turn starting at
/// `start`: at most a minute before it, or within the slack after it.
fn prompt_pins(ts: i64, start: i64) -> bool {
    ts <= start + PROMPT_PIN_SLACK_NS && start.saturating_sub(ts) <= PROMPT_PIN_WINDOW_NS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookTarget {
    Open,
    Closed(u64),
    None,
}

/// Did the user decline this tool call rather than the tool failing?
///
/// Claude reports a rejection as a tool result with `is_error` set, whose
/// body is one of its own fixed sentences. Matching those at the start of
/// the body — not anywhere within it — keeps this from catching a tool
/// whose output merely quotes one.
fn user_declined(content: &Value) -> bool {
    const DECLINED: &[&str] = &[
        "The user doesn't want to proceed with this tool use",
        "The user doesn't want to take this action",
        "[Request interrupted by user",
        "Tool use was rejected",
    ];
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .find_map(|i| i.get("text").and_then(|t| t.as_str()))
            .unwrap_or_default()
            .to_string(),
        _ => return false,
    };
    let text = text.trim_start();
    DECLINED.iter().any(|p| text.starts_with(p))
}

/// A short reason for a failed tool call, taken from the result's own
/// exit line. Only derived facts travel — never a slice of the body — so
/// this is safe to store in metadata mode, where content is dropped.
fn error_summary(content: Option<&Value>) -> Option<String> {
    let text = match content? {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    text.lines()
        .take(20)
        .find_map(|l| crate::transcript::exit_code_of(l.trim()))
        .map(|code| format!("exit code {code}"))
}

fn apply_tool_pins(row: &mut ObservationRow, state: &ToolHookState) {
    if let Some(start) = state.start_ns {
        row.start_ns = start;
        if let Some(end) = row.end_ns
            && end < start
        {
            row.end_ns = Some(start);
        }
    }
    if let Some(end) = state.end_ns {
        row.end_ns = Some(row.end_ns.map_or(end, |e| e.max(end)).max(row.start_ns));
    }
    if state.is_error {
        row.is_error = true;
        row.level = Level::Error;
        if row.status_message.is_none() {
            row.status_message = Some("tool error".into());
        }
        if let Some(err) = &state.error {
            row.metadata
                .insert("tool_error".into(), Value::from(err.clone()));
        }
    }
    row.metadata.insert("hook_timed".into(), Value::Bool(true));
}

fn clean_json_str(val: &Value) -> Option<String> {
    match val {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
                if let Ok(Value::String(unescaped)) = serde_json::from_str(trimmed) {
                    return Some(unescaped.trim().to_string());
                }
                return Some(trimmed[1..trimmed.len() - 1].trim().to_string());
            }
            Some(trimmed.to_string())
        }
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
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

/// Splits Claude's MCP tool convention `mcp__<server>__<tool>`.
fn split_mcp_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("mcp__")?;
    match rest.split_once("__") {
        Some((server, tool)) if !server.is_empty() && !tool.is_empty() => {
            Some((server.to_string(), tool.to_string()))
        }
        _ => None,
    }
}

struct AgentInfo {
    name: String,
    role: Option<String>,
    type_name: Option<String>,
    model: Option<String>,
    prompt: Option<String>,
}

fn extract_skill_info(raw_tool_name: &str, args: &Value) -> Option<(String, String)> {
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
        let skill_name = if p
            .file_name()
            .is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case("skill.md"))
        {
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

fn extract_agent_info(raw_tool_name: &str, args: &Value) -> Option<AgentInfo> {
    let tool_name = normalize_tool_name(raw_tool_name);
    if tool_name == "invoke_subagent"
        || tool_name == "launch_subagent"
        || tool_name == "spawn_agent"
    {
        let subagents_val = args.get("Subagents").or_else(|| args.get("subagents"));
        let parsed_arr: Option<Vec<Value>> = match subagents_val {
            Some(Value::Array(arr)) => Some(arr.clone()),
            Some(Value::String(s)) => serde_json::from_str(s.trim()).ok(),
            _ => None,
        };
        if let Some(subagents) = parsed_arr
            && let Some(first) = subagents.first()
        {
            let role = first
                .get("Role")
                .or_else(|| first.get("role"))
                .and_then(clean_json_str);
            let type_name = first
                .get("TypeName")
                .or_else(|| first.get("type_name"))
                .or_else(|| first.get("type"))
                .and_then(clean_json_str);
            let model = first
                .get("Model")
                .or_else(|| first.get("model"))
                .and_then(clean_json_str);
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
        let name = args
            .get("name")
            .and_then(clean_json_str)
            .unwrap_or_else(|| "custom_agent".into());
        let desc = args.get("description").and_then(clean_json_str);
        let prompt = args
            .get("system_prompt")
            .or_else(|| args.get("prompt"))
            .and_then(clean_json_str);
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

/// Outcome recorded on the launch row at session end.
pub struct SessionEnd {
    /// "exit" | "stopped" | "app_quit"
    pub termination: &'static str,
    pub exit_code: Option<u32>,
    pub correlation: String,
    pub session_id: Option<String>,
    pub parse_errors: u64,
    /// App-wide dropped-op count at session end.
    pub dropped_ops: u64,
    pub cost: Option<CostSnapshot>,
}

/// Built-in tags plus the user's configured ones, deduped.
pub fn launch_tags(settings: &MapSettings) -> Vec<String> {
    let mut tags = vec![
        "agent-mux".to_string(),
        settings.provider.as_str().to_string(),
    ];
    for t in &settings.tags {
        if !tags.contains(t) {
            tags.push(t.clone());
        }
    }
    tags
}

/// The launch row as known at spawn: every start-time fact, no outcome.
pub fn launch_started(settings: &MapSettings, known_session_id: Option<&str>) -> LaunchRow {
    LaunchRow {
        id: settings.launch_id.clone(),
        run_id: settings.run_id.clone(),
        agent_mux_session: settings.agent_mux_session as i64,
        profile: settings.profile_name.clone(),
        provider: settings.provider.as_str().to_string(),
        cwd: settings.cwd.clone(),
        project_slug: settings.project_slug.clone(),
        content_mode: settings.content_mode.as_str().to_string(),
        correlation_plan: settings.correlation_plan.clone(),
        correlation: None,
        session_key: known_session_id.map(|id| session_key(settings.provider, id)),
        injected_session_id: settings.injected,
        attached: settings.attached,
        started_ns: settings.started_ns,
        ended_ns: None,
        termination: None,
        exit_code: None,
        parse_errors: None,
        dropped_ops: None,
        reported_cost_usd: None,
        reported_lines_added: None,
        reported_lines_removed: None,
        agent_mux_version: env!("CARGO_PKG_VERSION").to_string(),
        user_id: settings.user_id.clone(),
        release: settings.release.clone(),
        environment: settings.environment.clone(),
        tags: launch_tags(settings),
        metadata: None,
    }
}

/// The launch row carrying hook-delivered facts only (merged into
/// `launches.metadata`).
pub fn launch_metadata(
    settings: &MapSettings,
    session_id: Option<&str>,
    metadata: Value,
) -> LaunchRow {
    let mut row = launch_started(settings, session_id);
    row.metadata = Some(metadata);
    row
}

/// The launch row at adoption: the outcome of correlation and the session.
pub fn launch_adopted(settings: &MapSettings, session_id: &str, correlation: &str) -> LaunchRow {
    let mut row = launch_started(settings, Some(session_id));
    row.correlation = Some(correlation.to_string());
    row
}

/// The launch row at session end (or once, with `termination = "app_quit"`,
/// during app shutdown).
pub fn launch_ended(settings: &MapSettings, end: &SessionEnd, now_ns: i64) -> LaunchRow {
    let mut row = launch_started(settings, end.session_id.as_deref());
    row.correlation = Some(end.correlation.clone());
    row.ended_ns = Some(now_ns);
    row.termination = Some(end.termination.to_string());
    row.exit_code = end.exit_code.map(i64::from);
    row.parse_errors = Some(end.parse_errors as i64);
    row.dropped_ops = Some(end.dropped_ops as i64);
    if let Some(cost) = &end.cost {
        row.reported_cost_usd = cost.total_cost_usd;
        row.reported_lines_added = cost.total_lines_added;
        row.reported_lines_removed = cost.total_lines_removed;
    }
    row
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
            run_id: "run-1".into(),
            correlation_plan: "deterministic".into(),
            injected: true,
            attached: false,
            started_ns: 500,
        }
    }

    fn user(text: &str, ts: &str) -> TranscriptEvent {
        TranscriptEvent::User {
            meta: false,
            text: text.into(),
            ts: Some(ts.into()),
        }
    }

    fn assistant(text: &str, ts: &str, usage: Vec<(String, i64)>) -> TranscriptEvent {
        TranscriptEvent::Assistant {
            skill: None,
            text: text.into(),
            model: Some("claude-fable-5".into()),
            thinking: None,
            usage,
            msg_id: None,
            step_index: None,
            ts: Some(ts.into()),
        }
    }

    fn tool_use(id: &str, name: &str, args: Value, ts: &str) -> TranscriptEvent {
        TranscriptEvent::ToolUse {
            skill: None,
            id: id.into(),
            name: name.into(),
            args,
            ts: Some(ts.into()),
        }
    }

    fn tool_result(id: &str, content: Value, is_error: bool, ts: &str) -> TranscriptEvent {
        TranscriptEvent::ToolResult {
            structured: None,
            id: id.into(),
            content,
            is_error,
            ts: Some(ts.into()),
        }
    }

    fn traces(ops: &[StoreOp]) -> Vec<&TraceRow> {
        ops.iter()
            .filter_map(|op| match op {
                StoreOp::Trace(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    fn observations(ops: &[StoreOp]) -> Vec<&ObservationRow> {
        ops.iter()
            .filter_map(|op| match op {
                StoreOp::Observation(o) => Some(o),
                _ => None,
            })
            .collect()
    }

    fn closed(ops: &[StoreOp]) -> Vec<&TraceRow> {
        traces(ops)
            .into_iter()
            .filter(|t| t.status != TraceStatus::Open)
            .collect()
    }

    fn find_obs<'a>(ops: &'a [StoreOp], name: &str) -> &'a ObservationRow {
        observations(ops)
            .into_iter()
            .rev()
            .find(|o| o.name == name)
            .unwrap_or_else(|| panic!("no observation named {name}: {ops:#?}"))
    }

    fn meta_str<'a>(o: &'a ObservationRow, key: &str) -> Option<&'a str> {
        o.metadata.get(key).and_then(|v| v.as_str())
    }

    fn meta_int(o: &ObservationRow, key: &str) -> Option<i64> {
        o.metadata.get(key).and_then(|v| v.as_i64())
    }

    fn ns(ts: &str) -> i64 {
        clamp_ns(parse_rfc3339_nanos(ts).unwrap())
    }

    #[test]
    fn basic_turn_shape_open_trace_generation_tool_and_close() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("sess-1".into()),
            "deterministic",
        );
        let mut ops = Vec::new();
        ops.extend(asm.feed(user("fix it", "2026-08-30T10:00:00Z"), 0));
        // the turn is visible as soon as it opens, with name + input
        let open = traces(&ops);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].status, TraceStatus::Open);
        assert_eq!(open[0].name, "Claude Code: fix it");
        assert_eq!(open[0].input.as_deref(), Some("fix it"));
        assert_eq!(open[0].end_ns, None);
        assert_eq!(open[0].session_key, "claude:sess-1");
        assert_eq!(open[0].launch_id.as_deref(), Some("launch-1"));
        // full mode: the first prompt becomes the session title
        assert!(
            ops.iter().any(
                |op| matches!(op, StoreOp::Session(s) if s.title.as_deref() == Some("fix it"))
            )
        );

        ops.extend(asm.feed(
            assistant(
                "on it",
                "2026-08-30T10:00:05Z",
                vec![("input_tokens".into(), 10)],
            ),
            0,
        ));
        ops.extend(asm.feed(
            tool_use(
                "t1",
                "Bash",
                serde_json::json!({"command": "ls"}),
                "2026-08-30T10:00:06Z",
            ),
            0,
        ));
        // in-flight tool row: no end yet
        let in_flight = find_obs(&ops, "Bash");
        assert_eq!(in_flight.end_ns, None);
        assert_eq!(in_flight.tool_id.as_deref(), Some("t1"));
        ops.extend(asm.feed(
            tool_result(
                "t1",
                Value::String("ok".into()),
                false,
                "2026-08-30T10:00:07Z",
            ),
            0,
        ));
        let generation = find_obs(&ops, "assistant");
        assert_eq!(generation.obs_type, ObservationType::Generation);
        assert_eq!(generation.model.as_deref(), Some("claude-fable-5"));
        assert_eq!(generation.usage.as_ref().unwrap().input, Some(10));
        assert_eq!(generation.output.as_deref(), Some("on it"));
        assert_eq!(generation.input.as_deref(), Some("fix it"));
        let tool = find_obs(&ops, "Bash");
        assert_eq!(tool.obs_type, ObservationType::Tool);
        assert_eq!(tool.input.as_deref(), Some(r#"{"command":"ls"}"#));
        assert_eq!(tool.output.as_deref(), Some("ok"));
        assert_eq!(tool.end_ns, Some(ns("2026-08-30T10:00:07Z")));
        // same id for the open and closed writes of one call
        let bash_ids: std::collections::HashSet<_> = observations(&ops)
            .iter()
            .filter(|o| o.name == "Bash")
            .map(|o| o.id.clone())
            .collect();
        assert_eq!(bash_ids.len(), 1);

        // next user closes the turn: the closed row + the new open row
        let close = asm.feed(user("next", "2026-08-30T10:01:00Z"), 0);
        let closed_rows = closed(&close);
        assert_eq!(closed_rows.len(), 1, "{close:#?}");
        let root = closed_rows[0];
        assert_eq!(root.status, TraceStatus::Closed);
        assert_eq!(root.ordinal, 1);
        assert_eq!(root.id, generation.trace_id);
        assert_eq!(root.output.as_deref(), Some("on it"));
        assert_eq!(root.start_ns, ns("2026-08-30T10:00:00Z"));
        assert_eq!(root.end_ns, Some(ns("2026-08-30T10:00:07Z")));
        assert!(
            root.timing_approx,
            "any turn with a generation is approximate"
        );
        assert_eq!(generation.start_ns, ns("2026-08-30T10:00:00Z"));
        assert_eq!(generation.end_ns, Some(ns("2026-08-30T10:00:05Z")));
        // second turn has a distinct deterministic id
        let t2 = traces(&close)
            .into_iter()
            .find(|t| t.status == TraceStatus::Open)
            .unwrap();
        assert_ne!(t2.id, root.id);
        assert_eq!(t2.id, ids::trace_id_hex("amx1|claude|sess-1|turn|2"));
    }

    #[test]
    fn metadata_mode_exports_no_content_columns() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Metadata),
            Some("sess-1".into()),
            "deterministic",
        );
        let mut ops = Vec::new();
        ops.extend(asm.feed(user("secret prompt", "2026-08-30T10:00:00Z"), 0));
        ops.extend(asm.feed(
            TranscriptEvent::Assistant {
                skill: None,
                text: "secret reply".into(),
                model: Some("m".into()),
                thinking: Some("secret thoughts".into()),
                usage: vec![("output_tokens".into(), 5)],
                msg_id: None,
                step_index: None,
                ts: Some("2026-08-30T10:00:01Z".into()),
            },
            0,
        ));
        ops.extend(asm.feed(
            tool_use(
                "t1",
                "Bash",
                serde_json::json!({"command": "cat /etc/passwd"}),
                "2026-08-30T10:00:02Z",
            ),
            0,
        ));
        ops.extend(asm.feed(
            tool_result(
                "t1",
                Value::String("secret output".into()),
                true,
                "2026-08-30T10:00:03Z",
            ),
            0,
        ));
        ops.extend(asm.finalize());
        assert!(!ops.is_empty());
        for op in &ops {
            match op {
                StoreOp::Trace(t) => {
                    assert!(
                        t.input.is_none() && t.output.is_none() && t.thinking.is_none(),
                        "{t:?}"
                    );
                    assert!(!t.name.contains("secret"));
                }
                StoreOp::Observation(o) => {
                    assert!(
                        o.input.is_none() && o.output.is_none() && o.thinking.is_none(),
                        "{o:?}"
                    );
                    let dumped = serde_json::to_string(&o.metadata).unwrap();
                    assert!(!dumped.contains("secret"), "{dumped}");
                }
                StoreOp::Session(s) => assert!(s.title.is_none(), "no title in metadata mode"),
                StoreOp::Launch(_) => {}
            }
        }
        let root = closed(&ops)[0];
        assert_eq!(root.name, "turn 1");
        let generation = find_obs(&ops, "assistant");
        assert_eq!(generation.usage.as_ref().unwrap().output, Some(5));
        let tool = find_obs(&ops, "Bash");
        assert_eq!(tool.level, Level::Error);
        assert!(tool.is_error);
        assert_eq!(
            tool.status_message.as_deref(),
            Some("tool error"),
            "no exit line in the body: the generic label stands"
        );

        // when the result reports one, the row says which code it was
        let mut asm = TurnAssembler::new(settings(ContentMode::Full), None, "test");
        let _ = asm.feed(user("build it", "2026-08-30T10:00:00Z"), 0);
        let _ = asm.feed(
            tool_use(
                "t9",
                "Bash",
                serde_json::json!({"command": "cargo test"}),
                "2026-08-30T10:00:01Z",
            ),
            0,
        );
        let ops = asm.feed(
            tool_result(
                "t9",
                Value::from("Exit code 101\nthread panicked"),
                true,
                "2026-08-30T10:00:02Z",
            ),
            0,
        );
        let failed = find_obs(&ops, "Bash");
        assert!(failed.is_error);
        assert_eq!(failed.status_message.as_deref(), Some("exit code 101"));
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
        let mut ops = Vec::new();
        ops.extend(asm.feed(boundary(TurnBoundaryKind::Start, "2026-08-30T10:00:00Z"), 0));
        assert_eq!(traces(&ops).len(), 1, "explicit start opens the trace row");
        ops.extend(asm.feed(user("do it", "2026-08-30T10:00:01Z"), 0));
        ops.extend(asm.feed(assistant("working", "2026-08-30T10:00:02Z", vec![]), 0));
        ops.extend(asm.feed(
            TranscriptEvent::TokenCount {
                model: None,
                usage: vec![
                    ("input_tokens".into(), 200),
                    ("cached_input_tokens".into(), 50),
                    ("output_tokens".into(), 20),
                ],
                msg_id: None,
                ts: Some("2026-08-30T10:00:03Z".into()),
            },
            0,
        ));
        ops.extend(asm.feed(
            boundary(TurnBoundaryKind::Aborted, "2026-08-30T10:00:04Z"),
            0,
        ));
        let turn1 = closed(&ops)[0];
        assert_eq!(turn1.status, TraceStatus::Aborted);
        assert_eq!(turn1.ordinal, 1);
        // usage attached to the turn's final generation, Codex-normalized
        let generation = find_obs(&ops, "assistant");
        let usage = generation.usage.as_ref().unwrap();
        assert_eq!(usage.input, Some(150));
        assert_eq!(usage.cache_read, Some(50));

        let mut ops2 = Vec::new();
        ops2.extend(asm.feed(boundary(TurnBoundaryKind::Start, "2026-08-30T10:00:05Z"), 0));
        ops2.extend(asm.feed(assistant("done", "2026-08-30T10:00:06Z", vec![]), 0));
        ops2.extend(asm.feed(
            boundary(TurnBoundaryKind::Complete, "2026-08-30T10:00:07Z"),
            0,
        ));
        let turn2 = closed(&ops2)[0];
        assert_eq!(turn2.ordinal, 2);
        assert_ne!(turn2.id, turn1.id);
        // a user message mid-turn must NOT open turn 3 when boundaries are explicit
        let mut ops3 = Vec::new();
        ops3.extend(asm.feed(boundary(TurnBoundaryKind::Start, "2026-08-30T10:01:00Z"), 0));
        ops3.extend(asm.feed(user("steering", "2026-08-30T10:01:01Z"), 0));
        ops3.extend(asm.feed(
            boundary(TurnBoundaryKind::Complete, "2026-08-30T10:01:02Z"),
            0,
        ));
        let ordinals: std::collections::HashSet<i64> =
            traces(&ops3).iter().map(|t| t.ordinal).collect();
        assert_eq!(ordinals, [3].into_iter().collect());
    }

    #[test]
    fn codex_turn_without_generation_gets_a_usage_only_generation() {
        let mut asm = TurnAssembler::new(
            MapSettings {
                provider: Provider::Codex,
                ..settings(ContentMode::Metadata)
            },
            Some("codex-sess".into()),
            "watched",
        );
        let _ = asm.feed(
            TranscriptEvent::SessionMeta {
                session_id: None,
                cwd: None,
                extra: serde_json::json!({"model": "gpt-5.3-codex"}),
                ts: Some("2026-08-30T09:59:59Z".into()),
            },
            0,
        );
        let mut ops = Vec::new();
        ops.extend(asm.feed(user("go", "2026-08-30T10:00:00Z"), 0));
        ops.extend(asm.feed(
            TranscriptEvent::TokenCount {
                model: None,
                usage: vec![("input_tokens".into(), 100), ("output_tokens".into(), 10)],
                msg_id: None,
                ts: Some("2026-08-30T10:00:01Z".into()),
            },
            0,
        ));
        ops.extend(asm.finalize());
        let usage_only = find_obs(&ops, "assistant (usage only)");
        assert_eq!(usage_only.kind.as_deref(), Some("usage_only"));
        assert_eq!(
            usage_only.model.as_deref(),
            Some("gpt-5.3-codex"),
            "priced via the session model"
        );
        assert_eq!(usage_only.usage.as_ref().unwrap().input, Some(100));
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
        let mut ops = Vec::new();
        ops.extend(asm.feed(user("go", "2026-08-30T10:00:00Z"), 0));
        for name in ["list_dir", "view_file"] {
            ops.extend(asm.feed(tool_use("", name, Value::Null, "2026-08-30T10:00:01Z"), 0));
        }
        let paired = asm.feed(
            tool_result(
                "",
                Value::String("dir contents".into()),
                false,
                "2026-08-30T10:00:02Z",
            ),
            0,
        );
        assert_eq!(paired.len(), 1);
        let list_dir = find_obs(&paired, "list_dir");
        assert!(list_dir.end_ns.is_some());
        assert_eq!(list_dir.tool_id, None);
        let close = asm.finalize();
        let unpaired = find_obs(&close, "view_file");
        assert_eq!(
            unpaired.status_message.as_deref(),
            Some("no result observed")
        );
        assert!(unpaired.end_ns.is_some(), "closes with the turn");
        let root = closed(&close)[0];
        assert_eq!(
            root.metadata
                .as_ref()
                .and_then(|m| m.get("correlation"))
                .and_then(|v| v.as_str()),
            Some("heuristic")
        );
    }

    #[test]
    fn prime_pass_counts_ordinals_without_emitting() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("sess-1".into()),
            "deterministic",
        );
        asm.set_emitting(false);
        assert!(
            asm.feed(user("old turn 1", "2026-08-30T09:00:00Z"), 0)
                .is_empty()
        );
        assert!(
            asm.feed(assistant("old reply", "2026-08-30T09:00:01Z", vec![]), 0)
                .is_empty()
        );
        assert!(
            asm.feed(user("old turn 2", "2026-08-30T09:01:00Z"), 0)
                .is_empty()
        );
        asm.set_emitting(true);
        let ops = asm.feed(user("new turn", "2026-08-30T10:00:00Z"), 0);
        // the open primed turn was never emitted; the new turn opens as 3
        assert!(closed(&ops).is_empty(), "{ops:#?}");
        let open = traces(&ops);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].ordinal, 3);
        assert_eq!(open[0].id, ids::trace_id_hex("amx1|claude|sess-1|turn|3"));
        let close = asm.finalize();
        assert_eq!(closed(&close)[0].ordinal, 3);
    }

    #[test]
    fn backfill_truncation_salts_trace_ids_with_launch() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("sess-1".into()),
            "deterministic",
        );
        asm.mark_backfill_truncated();
        let _ = asm.feed(user("turn", "2026-08-30T10:00:00Z"), 0);
        let close = asm.finalize();
        let root = closed(&close)[0];
        assert!(root.ordinal_salted);
        assert_eq!(
            root.id,
            ids::trace_id_hex("amx1|claude|sess-1|turn|1|launch-1")
        );
    }

    #[test]
    fn missing_timestamp_falls_back_to_receive_time_with_flags() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Metadata),
            Some("s".into()),
            "deterministic",
        );
        let recv = 1_788_118_098_000_000_000_i128;
        let _ = asm.feed(
            TranscriptEvent::User {
                meta: false,
                text: "no ts".into(),
                ts: None,
            },
            recv,
        );
        let close = asm.finalize();
        let root = closed(&close)[0];
        assert_eq!(root.start_ns, recv as i64);
        assert!(root.timing_approx);
    }

    #[test]
    fn synthetic_model_is_stored_verbatim_for_the_matcher_to_skip() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Metadata),
            Some("s".into()),
            "deterministic",
        );
        let mut ops = Vec::new();
        ops.extend(asm.feed(user("u", "2026-08-30T10:00:00Z"), 0));
        ops.extend(asm.feed(
            TranscriptEvent::Assistant {
                skill: None,
                text: "synthetic".into(),
                model: Some("<synthetic>".into()),
                thinking: None,
                usage: vec![],
                msg_id: None,
                step_index: None,
                ts: Some("2026-08-30T10:00:01Z".into()),
            },
            0,
        ));
        let generation = find_obs(&ops, "assistant");
        assert_eq!(generation.model.as_deref(), Some("<synthetic>"));
        assert!(
            crate::tracing::pricing::PriceTable::builtin()
                .find("<synthetic>")
                .is_none()
        );
    }

    #[test]
    fn full_mode_trace_name_and_title_are_masked_like_all_content() {
        let mut asm = TurnAssembler::new(
            MapSettings {
                redact_literals: vec!["hunter2".into()],
                ..settings(ContentMode::Full)
            },
            Some("s".into()),
            "deterministic",
        );
        let ops = asm.feed(
            user(
                "sk-live-topsecret rotate this hunter2 now",
                "2026-08-30T10:00:00Z",
            ),
            0,
        );
        let open = traces(&ops)[0];
        assert!(!open.name.contains("topsecret"), "{}", open.name);
        assert!(!open.name.contains("hunter2"), "{}", open.name);
        assert!(open.name.contains("[REDACTED]"));
        let title = ops
            .iter()
            .find_map(|op| match op {
                StoreOp::Session(s) => s.title.clone(),
                _ => None,
            })
            .unwrap();
        assert!(
            !title.contains("topsecret") && !title.contains("hunter2"),
            "{title}"
        );
    }

    #[test]
    fn repeated_message_usage_is_charged_once_and_token_counts_accumulate() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Metadata),
            Some("s".into()),
            "deterministic",
        );
        let mut ops = Vec::new();
        ops.extend(asm.feed(user("go", "2026-08-30T10:00:00Z"), 0));
        for (i, text) in ["part one", "part two"].iter().enumerate() {
            ops.extend(asm.feed(
                TranscriptEvent::Assistant {
                    skill: None,
                    text: text.to_string(),
                    model: Some("m".into()),
                    thinking: None,
                    usage: vec![("input_tokens".into(), 100), ("output_tokens".into(), 10)],
                    msg_id: Some("msg_1".into()),
                    step_index: None,
                    ts: Some(format!("2026-08-30T10:00:0{}Z", i + 1)),
                },
                0,
            ));
        }
        for (msg, tokens) in [("msg_2", 50), ("msg_3", 25), ("msg_1", 999)] {
            ops.extend(asm.feed(
                TranscriptEvent::TokenCount {
                    model: None,
                    usage: vec![("input_tokens".into(), tokens)],
                    msg_id: Some(msg.into()),
                    ts: Some("2026-08-30T10:00:05Z".into()),
                },
                0,
            ));
        }
        ops.extend(asm.finalize());
        let total: i64 = observations(&ops)
            .iter()
            .filter_map(|o| o.usage.as_ref().and_then(|u| u.input))
            .sum();
        // 100 (msg_1, once) + 50 + 25 (usage-only generation); never 200, never +999
        assert_eq!(total, 175, "usage misrouted: {ops:#?}");
        assert!(
            observations(&ops)
                .iter()
                .any(|o| o.kind.as_deref() == Some("usage_only"))
        );
    }

    #[test]
    fn same_name_tool_rows_get_distinct_deterministic_ids() {
        let mut asm = TurnAssembler::new(
            MapSettings {
                provider: Provider::Antigravity,
                ..settings(ContentMode::Metadata)
            },
            Some("s".into()),
            "heuristic",
        );
        let mut ops = Vec::new();
        ops.extend(asm.feed(user("go", "2026-08-30T10:00:00Z"), 0));
        for _ in 0..2 {
            ops.extend(asm.feed(
                tool_use("", "run_command", Value::Null, "2026-08-30T10:00:01Z"),
                0,
            ));
        }
        ops.extend(asm.finalize());
        let ids: std::collections::HashSet<_> = observations(&ops)
            .iter()
            .filter(|o| o.name == "run_command")
            .map(|o| o.id.clone())
            .collect();
        assert_eq!(ids.len(), 2, "id collision on same-name tools");
    }

    #[test]
    fn orphan_tool_result_yields_unpaired_row_not_silence() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Metadata),
            Some("s".into()),
            "deterministic",
        );
        let _ = asm.feed(user("go", "2026-08-30T10:00:00Z"), 0);
        let ops = asm.feed(
            tool_result(
                "never-seen",
                Value::String("late output".into()),
                false,
                "2026-08-30T10:00:02Z",
            ),
            0,
        );
        let rows = observations(&ops);
        assert_eq!(rows.len(), 1, "orphan result must not vanish");
        assert_eq!(rows[0].status_message.as_deref(), Some("unpaired result"));
        assert_eq!(Some(rows[0].start_ns), rows[0].end_ns);
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
        for input in [
            "key sk-live-abc123 end",
            "lf pk-lf-xyz end",
            "aws AKIAIOSFODNN7EXAMPLE end",
            "gh ghp_abcdef end",
            "slack xoxb-123 end",
            "auth Bearer abc.def end",
            "pw hunter2 end",
        ] {
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
        assert_eq!(mask_secrets("task-123 is fine", &[]), "task-123 is fine");
        let key = "-----BEGIN RSA PRIVATE KEY-----\nMIIabc\n-----END RSA PRIVATE KEY-----\nafter";
        assert!(!mask_secrets(key, &[]).contains("MIIabc"));
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
    fn launch_rows_carry_start_and_end_facts() {
        let s = settings(ContentMode::Metadata);
        let started = launch_started(&s, None);
        assert_eq!(started.id, "launch-1");
        assert_eq!(started.run_id, "run-1");
        assert_eq!(
            started.session_key, None,
            "watched provider: no session at spawn"
        );
        assert_eq!(started.correlation_plan, "deterministic");
        assert!(started.injected_session_id);
        assert_eq!(started.started_ns, 500);
        assert_eq!(started.tags, vec!["agent-mux", "claude", "extra"]);
        assert!(started.termination.is_none());
        let known = launch_started(&s, Some("known-id"));
        assert_eq!(known.session_key.as_deref(), Some("claude:known-id"));
        let adopted = launch_adopted(&s, "adopted-id", "watched");
        assert_eq!(adopted.correlation.as_deref(), Some("watched"));
        assert_eq!(adopted.session_key.as_deref(), Some("claude:adopted-id"));
        let ended = launch_ended(
            &s,
            &SessionEnd {
                cost: Some(CostSnapshot {
                    total_cost_usd: Some(1.5),
                    total_lines_added: Some(30),
                    total_lines_removed: Some(4),
                }),
                termination: "exit",
                exit_code: Some(0),
                correlation: "watched".into(),
                session_id: Some("adopted-id".into()),
                parse_errors: 2,
                dropped_ops: 0,
            },
            2_000,
        );
        assert_eq!(ended.ended_ns, Some(2_000));
        assert_eq!(ended.termination.as_deref(), Some("exit"));
        assert_eq!(ended.exit_code, Some(0));
        assert_eq!(ended.parse_errors, Some(2));
        assert_eq!(ended.reported_cost_usd, Some(1.5));
        assert_eq!(ended.reported_lines_added, Some(30));
        assert_eq!(ended.reported_lines_removed, Some(4));
        let quit = launch_ended(
            &s,
            &SessionEnd {
                cost: None,
                termination: "app_quit",
                exit_code: None,
                correlation: "none".into(),
                session_id: None,
                parse_errors: 0,
                dropped_ops: 0,
            },
            3_000,
        );
        assert_eq!(quit.termination.as_deref(), Some("app_quit"));
        assert_eq!(quit.exit_code, None);
        assert_eq!(quit.session_key, None);
    }

    #[test]
    fn skill_and_agent_observations_and_details() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("sess-1".into()),
            "deterministic",
        );
        let mut ops = Vec::new();
        ops.extend(asm.feed(user("build something", "2026-08-30T10:00:00Z"), 0));
        ops.extend(asm.feed(
            tool_use(
                "t1",
                "view_file",
                serde_json::json!({
                    "AbsolutePath": "/home/user/.gemini/skills/agy-customizations/SKILL.md",
                    "toolAction": "Viewing skill file",
                    "toolSummary": "View agy-customizations"
                }),
                "2026-08-30T10:00:01Z",
            ),
            0,
        ));
        ops.extend(asm.feed(
            tool_result(
                "t1",
                Value::String("# Skill content".into()),
                false,
                "2026-08-30T10:00:02Z",
            ),
            0,
        ));
        ops.extend(asm.feed(
            tool_use(
                "t2",
                "invoke_subagent",
                serde_json::json!({"Subagents": [{"Role": "Codebase Researcher", "TypeName": "research", "Model": "flash", "Prompt": "explore files"}]}),
                "2026-08-30T10:00:03Z",
            ),
            0,
        ));
        ops.extend(asm.feed(
            tool_result(
                "t2",
                Value::String("subagent finished".into()),
                false,
                "2026-08-30T10:00:04Z",
            ),
            0,
        ));
        ops.extend(asm.feed(
            TranscriptEvent::Assistant {
                skill: None,
                text: "all done".into(),
                model: Some("gemini-3.7-flash".into()),
                thinking: Some("reasoning details".into()),
                usage: vec![],
                msg_id: None,
                step_index: None,
                ts: Some("2026-08-30T10:00:05Z".into()),
            },
            0,
        ));
        let skill = find_obs(&ops, "skill: agy-customizations");
        assert_eq!(skill.kind.as_deref(), Some("skill_load"));
        assert_eq!(meta_str(skill, "skill_name"), Some("agy-customizations"));
        assert_eq!(meta_str(skill, "action"), Some("Viewing skill file"));
        assert_eq!(skill.output.as_deref(), Some("# Skill content"));
        assert_eq!(
            skill.path.as_deref(),
            Some("/home/user/.gemini/skills/agy-customizations/SKILL.md")
        );
        let agent = find_obs(&ops, "agent: Codebase Researcher (research)");
        assert_eq!(agent.obs_type, ObservationType::Agent);
        assert_eq!(agent.kind.as_deref(), Some("agent_invocation"));
        assert_eq!(meta_str(agent, "agent_role"), Some("Codebase Researcher"));
        assert_eq!(meta_str(agent, "agent_type"), Some("research"));
        assert_eq!(meta_str(agent, "agent_prompt"), Some("explore files"));
        let generation = find_obs(&ops, "assistant");
        assert_eq!(generation.input.as_deref(), Some("build something"));
        assert_eq!(generation.output.as_deref(), Some("all done"));
        assert_eq!(generation.thinking.as_deref(), Some("reasoning details"));
        assert_eq!(generation.model.as_deref(), Some("gemini-3.7-flash"));
    }

    #[test]
    fn meta_user_lines_do_not_fragment_turns() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("s".into()),
            "deterministic",
        );
        let mut ops = Vec::new();
        ops.extend(asm.feed(user("real prompt", "2026-08-30T10:00:00Z"), 0));
        ops.extend(asm.feed(assistant("working", "2026-08-30T10:00:01Z", vec![]), 0));
        ops.extend(asm.feed(
            TranscriptEvent::User {
                text: "<task-notification>agent finished</task-notification>".into(),
                meta: true,
                ts: Some("2026-08-30T10:00:02Z".into()),
            },
            0,
        ));
        ops.extend(asm.feed(assistant("done", "2026-08-30T10:00:03Z", vec![]), 0));
        ops.extend(asm.feed(user("next prompt", "2026-08-30T10:01:00Z"), 0));
        ops.extend(asm.finalize());
        let roots = closed(&ops);
        assert_eq!(
            roots.len(),
            2,
            "meta line must not split the turn: {ops:#?}"
        );
        assert_eq!(roots[0].input.as_deref(), Some("real prompt"));
        assert_eq!(roots[0].output.as_deref(), Some("done"));
        assert_eq!(roots[1].input.as_deref(), Some("next prompt"));
        let dumped = format!("{ops:?}");
        assert!(!dumped.contains("task-notification"), "meta text leaked");
    }

    #[test]
    fn mcp_tools_get_server_and_tool_columns() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Metadata),
            Some("s".into()),
            "deterministic",
        );
        let _ = asm.feed(user("go", "2026-08-30T10:00:00Z"), 0);
        let _ = asm.feed(
            tool_use(
                "t1",
                "mcp__claude_ai_Google_Drive__search_files",
                serde_json::json!({"query": "quarterly report"}),
                "2026-08-30T10:00:01Z",
            ),
            0,
        );
        let ops = asm.feed(
            tool_result(
                "t1",
                Value::String("3 files".into()),
                false,
                "2026-08-30T10:00:02Z",
            ),
            0,
        );
        let tool = observations(&ops)[0];
        assert_eq!(tool.name, "mcp: claude_ai_Google_Drive/search_files");
        assert_eq!(tool.obs_type, ObservationType::Tool);
        assert_eq!(tool.mcp_server.as_deref(), Some("claude_ai_Google_Drive"));
        assert_eq!(meta_str(tool, "mcp_tool"), Some("search_files"));
        assert_eq!(tool.kind.as_deref(), Some("mcp_tool"));
        assert_eq!(
            tool.tool_name.as_deref(),
            Some("mcp__claude_ai_Google_Drive__search_files")
        );
        assert_eq!(
            meta_str(tool, "query"),
            None,
            "metadata mode keeps the query home"
        );
    }

    #[test]
    fn structured_results_enrich_bash_edit_and_agent_rows() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("s".into()),
            "deterministic",
        );
        let _ = asm.feed(user("go", "2026-08-30T10:00:00Z"), 0);
        let result = |id: &str, structured: Value| TranscriptEvent::ToolResult {
            id: id.into(),
            content: Value::String("out".into()),
            is_error: false,
            structured: Some(structured),
            ts: Some("2026-08-30T10:00:02Z".into()),
        };
        let _ = asm.feed(
            tool_use(
                "b1",
                "Bash",
                serde_json::json!({"command": "cargo test"}),
                "2026-08-30T10:00:01Z",
            ),
            0,
        );
        let ops = asm.feed(
            result("b1", serde_json::json!({"stdout": "ok!!", "stderr": "warn", "interrupted": true, "returnCodeInterpretation": "failure"})),
            0,
        );
        let bash = observations(&ops)[0];
        assert_eq!(meta_int(bash, "stdout_bytes"), Some(4));
        assert_eq!(meta_int(bash, "stderr_bytes"), Some(4));
        assert_eq!(meta_str(bash, "return_code"), Some("failure"));
        assert_eq!(bash.metadata.get("interrupted"), Some(&Value::Bool(true)));
        assert_eq!(meta_str(bash, "command"), Some("cargo test"));

        let _ = asm.feed(
            tool_use(
                "e1",
                "Edit",
                serde_json::json!({"file_path": "/proj/a.rs"}),
                "2026-08-30T10:00:01Z",
            ),
            0,
        );
        let ops = asm.feed(
            result("e1", serde_json::json!({"filePath": "/proj/a.rs", "userModified": true, "structuredPatch": [{"lines": ["+new line", "+another", "-old", " ctx"]}]})),
            0,
        );
        let edit = observations(&ops)[0];
        assert_eq!(meta_int(edit, "lines_added"), Some(2));
        assert_eq!(meta_int(edit, "lines_removed"), Some(1));
        assert_eq!(edit.metadata.get("user_modified"), Some(&Value::Bool(true)));
        assert_eq!(edit.path.as_deref(), Some("/proj/a.rs"));

        let _ = asm.feed(
            tool_use(
                "a1",
                "Agent",
                serde_json::json!({"subagent_type": "Explore", "prompt": "look around"}),
                "2026-08-30T10:00:01Z",
            ),
            0,
        );
        let ops = asm.feed(
            result("a1", serde_json::json!({"agentId": "abc123", "resolvedModel": "claude-opus-5[1m]", "status": "async_launched", "isAsync": true, "outputFile": "/tmp/tasks/abc123.output", "description": "Audit the UI"})),
            0,
        );
        let agent = observations(&ops)[0];
        assert_eq!(agent.name, "agent: Explore");
        assert_eq!(agent.obs_type, ObservationType::Agent);
        assert_eq!(meta_str(agent, "agent_id"), Some("abc123"));
        assert_eq!(meta_str(agent, "agent_model"), Some("claude-opus-5[1m]"));
        assert_eq!(meta_str(agent, "agent_status"), Some("async_launched"));
        assert_eq!(
            meta_str(agent, "agent_output_file"),
            Some("/tmp/tasks/abc123.output")
        );
        assert_eq!(meta_str(agent, "summary"), Some("Audit the UI"));
    }

    /// The pairing that made half the store's generations look empty: a
    /// text message and its own token count are one API call.
    #[test]
    fn a_token_count_joins_the_message_it_belongs_to() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("s".into()),
            "deterministic",
        );
        let _ = asm.feed(user("go", "2026-09-04T10:00:00Z"), 0);
        let ops = asm.feed(
            assistant("here is the plan", "2026-09-04T10:00:05Z", vec![]),
            0,
        );
        let first = observations(&ops)[0].clone();
        assert_eq!(first.name, "assistant");
        assert!(
            first.usage.is_none(),
            "the message line carries no usage yet"
        );

        // the count that follows belongs to that same call
        let ops = asm.feed(
            TranscriptEvent::TokenCount {
                usage: vec![("input_tokens".into(), 12), ("output_tokens".into(), 35)],
                msg_id: Some("msg_1".into()),
                model: Some("claude-opus-5".into()),
                ts: Some("2026-09-04T10:00:06Z".into()),
            },
            0,
        );
        let rows = observations(&ops);
        assert_eq!(rows.len(), 1, "one call stays one row: {ops:?}");
        let joined = rows[0];
        assert_eq!(joined.id, first.id, "the same row, updated");
        assert_eq!(joined.name, "assistant", "it keeps its text identity");
        assert_eq!(joined.output.as_deref(), Some("here is the plan"));
        assert_eq!(joined.usage.as_ref().unwrap().input, Some(12));
        assert_eq!(joined.usage.as_ref().unwrap().output, Some(35));
        assert_eq!(
            joined.model.as_deref(),
            Some("claude-fable-5"),
            "the message's own model wins; the count does not override it"
        );
        assert_eq!(
            joined.end_ns,
            Some(ns("2026-09-04T10:00:06Z")),
            "the call ended when its count arrived"
        );

        // a second count is a separate call and still gets its own row
        let ops = asm.feed(
            TranscriptEvent::TokenCount {
                usage: vec![("input_tokens".into(), 900), ("output_tokens".into(), 5)],
                msg_id: Some("msg_2".into()),
                model: Some("claude-opus-5".into()),
                ts: Some("2026-09-04T10:00:09Z".into()),
            },
            0,
        );
        let rows = observations(&ops);
        assert_eq!(rows.len(), 1);
        assert_ne!(
            rows[0].id, first.id,
            "a genuine tool-only call is its own row"
        );
        assert_eq!(rows[0].name, "assistant (tool use)");
        assert_eq!(rows[0].usage.as_ref().unwrap().input, Some(900));
    }

    #[test]
    fn token_count_with_model_mints_a_costed_generation() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Metadata),
            Some("s".into()),
            "deterministic",
        );
        let _ = asm.feed(user("go", "2026-08-30T10:00:00Z"), 0);
        let _ = asm.feed(
            tool_use("t1", "Bash", Value::Null, "2026-08-30T10:00:01Z"),
            0,
        );
        let ops = asm.feed(
            TranscriptEvent::TokenCount {
                usage: vec![("input_tokens".into(), 120), ("output_tokens".into(), 30)],
                msg_id: Some("msg_9".into()),
                model: Some("claude-fable-5".into()),
                ts: Some("2026-08-30T10:00:01Z".into()),
            },
            0,
        );
        let rows = observations(&ops);
        assert_eq!(
            rows.len(),
            1,
            "a model-attributed count becomes a generation"
        );
        let generation = rows[0];
        assert_eq!(generation.name, "assistant (tool use)");
        assert_eq!(generation.obs_type, ObservationType::Generation);
        assert_eq!(generation.model.as_deref(), Some("claude-fable-5"));
        assert_eq!(generation.usage.as_ref().unwrap().input, Some(120));
        assert_eq!(
            generation.metadata.get("tool_calls"),
            Some(&serde_json::json!(["Bash"]))
        );
        let repeat = asm.feed(
            TranscriptEvent::TokenCount {
                usage: vec![("input_tokens".into(), 120)],
                msg_id: Some("msg_9".into()),
                model: Some("claude-fable-5".into()),
                ts: Some("2026-08-30T10:00:02Z".into()),
            },
            0,
        );
        assert!(
            observations(&repeat).is_empty(),
            "duplicate usage must not mint a second generation"
        );
        let close = asm.finalize();
        assert!(
            !observations(&close)
                .iter()
                .any(|o| o.kind.as_deref() == Some("usage_only"))
        );
    }

    #[test]
    fn skill_attribution_reaches_generation_tool_and_trace() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Metadata),
            Some("s".into()),
            "deterministic",
        );
        let mut ops = Vec::new();
        ops.extend(asm.feed(user("go", "2026-08-30T10:00:00Z"), 0));
        ops.extend(asm.feed(
            TranscriptEvent::Assistant {
                text: "per the skill".into(),
                model: Some("m".into()),
                thinking: None,
                usage: vec![],
                msg_id: None,
                step_index: None,
                skill: Some("spec-wave".into()),
                ts: Some("2026-08-30T10:00:01Z".into()),
            },
            0,
        ));
        ops.extend(asm.feed(
            TranscriptEvent::ToolUse {
                id: "t1".into(),
                name: "Edit".into(),
                args: Value::Null,
                skill: Some("artifact-design".into()),
                ts: Some("2026-08-30T10:00:02Z".into()),
            },
            0,
        ));
        ops.extend(asm.feed(
            tool_result("t1", Value::Null, false, "2026-08-30T10:00:03Z"),
            0,
        ));
        ops.extend(asm.finalize());
        assert_eq!(
            find_obs(&ops, "assistant").skill.as_deref(),
            Some("spec-wave")
        );
        assert_eq!(
            find_obs(&ops, "Edit").skill.as_deref(),
            Some("artifact-design")
        );
        let root = closed(&ops)[0];
        assert_eq!(
            root.skills,
            Some(vec!["spec-wave".to_string(), "artifact-design".to_string()])
        );
    }

    #[test]
    fn turn_duration_and_cost_state_land_on_trace_and_launch() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Metadata),
            Some("s".into()),
            "deterministic",
        );
        let _ = asm.feed(user("go", "2026-08-30T10:00:00Z"), 0);
        let _ = asm.feed(
            TranscriptEvent::TurnDuration {
                duration_ms: 5400,
                message_count: Some(12),
                ts: Some("2026-08-30T10:00:06Z".into()),
            },
            0,
        );
        let _ = asm.feed(
            TranscriptEvent::CostState {
                total_cost_usd: Some(1.5),
                total_lines_added: Some(30),
                total_lines_removed: Some(4),
                ts: Some("2026-08-30T10:00:07Z".into()),
            },
            0,
        );
        let close = asm.finalize();
        let root = closed(&close)[0];
        assert_eq!(root.reported_duration_ms, Some(5400));
        assert_eq!(root.reported_message_count, Some(12));
        assert_eq!(root.session_cost_usd, Some(1.5));
        let snapshot = asm.cost_snapshot().expect("cost snapshot recorded");
        assert_eq!(snapshot.total_lines_added, Some(30));
    }

    #[test]
    fn metadata_mode_withholds_command_query_and_summaries() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Metadata),
            Some("s".into()),
            "deterministic",
        );
        let _ = asm.feed(user("go", "2026-08-30T10:00:00Z"), 0);
        let _ = asm.feed(
            tool_use("t1", "Bash", serde_json::json!({"command": "cat /etc/passwd", "toolSummary": "read the shadow file", "query": "root"}), "2026-08-30T10:00:01Z"),
            0,
        );
        let ops = asm.feed(
            TranscriptEvent::ToolResult {
                id: "t1".into(),
                content: Value::String("secret output".into()),
                is_error: false,
                structured: Some(serde_json::json!({"stdout": "secret output", "stderr": ""})),
                ts: Some("2026-08-30T10:00:02Z".into()),
            },
            0,
        );
        let tool = observations(&ops)[0];
        for gone in ["command", "summary", "query", "action"] {
            assert!(
                tool.metadata.get(gone).is_none(),
                "{gone} leaked in metadata mode"
            );
        }
        assert_eq!(meta_int(tool, "stdout_bytes"), Some(13));
    }

    #[test]
    fn full_mode_masks_command_metadata() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("s".into()),
            "deterministic",
        );
        let _ = asm.feed(user("go", "2026-08-30T10:00:00Z"), 0);
        let _ = asm.feed(
            tool_use(
                "t1",
                "Bash",
                serde_json::json!({"command": "export KEY=sk-live-topsecret && run"}),
                "2026-08-30T10:00:01Z",
            ),
            0,
        );
        let ops = asm.feed(
            tool_result("t1", Value::Null, false, "2026-08-30T10:00:02Z"),
            0,
        );
        let cmd = meta_str(observations(&ops)[0], "command").unwrap();
        assert!(
            !cmd.contains("topsecret"),
            "secret leaked via command metadata: {cmd}"
        );
        assert!(cmd.contains("[REDACTED]"), "{cmd}");
    }

    #[test]
    fn antigravity_escaped_quotes_and_namespaced_tools() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("sess-1".into()),
            "deterministic",
        );
        let mut ops = Vec::new();
        ops.extend(asm.feed(user("build something", "2026-08-30T10:00:00Z"), 0));
        ops.extend(asm.feed(
            tool_use(
                "t1",
                "default_api:view_file",
                serde_json::json!({
                    "AbsolutePath": "\"/home/silvio/.gemini/antigravity-cli/builtin/skills/agy-customizations/SKILL.md\"",
                    "toolAction": "\"Viewing skill file\"",
                    "toolSummary": "\"View agy-customizations\""
                }),
                "2026-08-30T10:00:01Z",
            ),
            0,
        ));
        ops.extend(asm.feed(
            tool_result(
                "t1",
                Value::String("# Skill content".into()),
                false,
                "2026-08-30T10:00:02Z",
            ),
            0,
        ));
        ops.extend(asm.feed(
            tool_use(
                "t2",
                "default_api:invoke_subagent",
                serde_json::json!({"Subagents": "[{\"Role\":\"Codebase Researcher\",\"TypeName\":\"research\",\"Model\":\"flash\",\"Prompt\":\"explore files\"}]"}),
                "2026-08-30T10:00:03Z",
            ),
            0,
        ));
        ops.extend(asm.feed(
            tool_result(
                "t2",
                Value::String("subagent finished".into()),
                false,
                "2026-08-30T10:00:04Z",
            ),
            0,
        ));
        let skill = find_obs(&ops, "skill: agy-customizations");
        assert_eq!(meta_str(skill, "skill_name"), Some("agy-customizations"));
        assert_eq!(meta_str(skill, "action"), Some("Viewing skill file"));
        let agent = find_obs(&ops, "agent: Codebase Researcher (research)");
        assert_eq!(agent.obs_type, ObservationType::Agent);
        assert_eq!(meta_str(agent, "agent_role"), Some("Codebase Researcher"));
        assert_eq!(meta_str(agent, "agent_prompt"), Some("explore files"));
    }

    #[test]
    fn antigravity_usage_attaches_by_step_in_either_order() {
        let mut asm = TurnAssembler::new(
            MapSettings {
                provider: Provider::Antigravity,
                content_mode: ContentMode::Metadata,
                ..settings(ContentMode::Metadata)
            },
            Some("agy-1".into()),
            "deterministic",
        );
        let planner = |step: u64, ts: &str| TranscriptEvent::Assistant {
            text: format!("step {step}"),
            model: Some("Gemini 3".into()),
            thinking: None,
            usage: vec![],
            msg_id: None,
            skill: None,
            step_index: Some(step),
            ts: Some(ts.into()),
        };
        let record = |idx: i64, steps: &[u64]| GenUsage {
            idx,
            steps: steps.to_vec(),
            model: Some("gemini-3.8-flash".into()),
            prompt_tokens: Some(5716),
            output_tokens: Some(88),
            thoughts_tokens: Some(26),
            text_tokens: Some(62),
            context_tokens: Some(28339),
            context_window: Some(256000),
            latency_ns: Some(7_000_000_000),
            first_token_ns: Some(30_000_000),
        };
        let _ = asm.feed(user("go", "2026-08-30T10:00:00Z"), 0);
        // usage first, transcript line later: held, then applied
        assert!(asm.attach_step_usage(&record(0, &[1, 2])).is_empty());
        let ops = asm.feed(planner(1, "2026-08-30T10:00:10Z"), 0);
        let rows = observations(&ops);
        assert_eq!(rows.len(), 2, "generation + its usage update: {ops:#?}");
        assert_eq!(rows[0].id, rows[1].id, "same observation, upserted");
        assert_eq!(rows[0].model.as_deref(), Some("Gemini 3"));
        assert_eq!(rows[1].model.as_deref(), Some("gemini-3.8-flash"));
        let usage = rows[1].usage.as_ref().unwrap();
        assert_eq!(usage.input, Some(5716));
        assert_eq!(usage.cache_read, Some(28339 - 5716));
        assert_eq!(usage.output, Some(88));
        assert_eq!(usage.reasoning, Some(26));
        assert_eq!(
            rows[1].start_ns,
            rows[1].end_ns.unwrap() - 7_000_000_000,
            "latency backdates the start"
        );
        assert_eq!(rows[1].metadata.get("latency_ms"), Some(&Value::from(7000)));
        assert_eq!(
            rows[1].metadata.get("cache_read_derived"),
            Some(&Value::Bool(true))
        );
        // transcript line first, usage later: applied directly
        let ops = asm.feed(planner(3, "2026-08-30T10:00:20Z"), 0);
        assert_eq!(observations(&ops).len(), 1);
        let late = asm.attach_step_usage(&record(1, &[3, 4]));
        assert_eq!(observations(&late).len(), 1);
        assert_eq!(observations(&late)[0].id, observations(&ops)[0].id);
        // a record for a step that never produced a generation stays parked
        assert!(asm.attach_step_usage(&record(2, &[99])).is_empty());
        // nothing to attach: nothing emitted
        assert!(
            asm.attach_step_usage(&GenUsage {
                idx: 3,
                steps: vec![3],
                ..Default::default()
            })
            .is_empty()
        );
    }

    fn hook(event: &str, ts_ns: i64, tool_use_id: Option<&str>, payload: Value) -> HookEvent {
        HookEvent {
            provider: Provider::Claude,
            session_id: "sess-1".into(),
            launch_id: Some("launch-1".into()),
            event: event.into(),
            ts_ns,
            cwd: None,
            transcript_path: None,
            turn_key: Some("prompt-1".into()),
            tool_use_id: tool_use_id.map(str::to_string),
            tool_name: tool_use_id.map(|_| "Bash".to_string()),
            agent_id: None,
            agent_type: None,
            step_index: None,
            model: None,
            is_error: event == "PostToolUseFailure" || event == "StopFailure",
            payload: payload.as_object().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn a_declined_tool_call_is_a_warning_not_a_failure() {
        let mut asm = TurnAssembler::new(settings(ContentMode::Full), None, "test");
        let _ = asm.feed(user("ask me", "2026-09-04T15:43:00Z"), 0);
        let _ = asm.feed(
            tool_use(
                "q1",
                "AskUserQuestion",
                serde_json::json!({"questions": []}),
                "2026-09-04T15:43:10Z",
            ),
            0,
        );
        let ops = asm.feed(
            tool_result(
                "q1",
                Value::from(
                    "The user doesn't want to proceed with this tool use. The tool use was rejected",
                ),
                true,
                "2026-09-04T15:43:17Z",
            ),
            0,
        );
        let declined = find_obs(&ops, "AskUserQuestion");
        assert!(!declined.is_error, "a decision, not a failure");
        assert_eq!(declined.level, Level::Warning);
        assert_eq!(
            declined.status_message.as_deref(),
            Some("declined by the user")
        );

        // an interrupt reads the same way
        let mut asm = TurnAssembler::new(settings(ContentMode::Full), None, "test");
        let _ = asm.feed(user("go", "2026-09-04T15:44:00Z"), 0);
        let _ = asm.feed(
            tool_use("b1", "Bash", serde_json::json!({}), "2026-09-04T15:44:01Z"),
            0,
        );
        let ops = asm.feed(
            tool_result(
                "b1",
                Value::from("[Request interrupted by user for tool use]"),
                true,
                "2026-09-04T15:44:02Z",
            ),
            0,
        );
        assert!(!find_obs(&ops, "Bash").is_error);

        // a genuine failure still is one, and a body that merely quotes
        // the sentence is not a decline
        let mut asm = TurnAssembler::new(settings(ContentMode::Full), None, "test");
        let _ = asm.feed(user("go", "2026-09-04T15:45:00Z"), 0);
        let _ = asm.feed(
            tool_use("b2", "Bash", serde_json::json!({}), "2026-09-04T15:45:01Z"),
            0,
        );
        let ops = asm.feed(
            tool_result(
                "b2",
                Value::from("Exit code 1\nlog said: The user doesn't want to proceed"),
                true,
                "2026-09-04T15:45:02Z",
            ),
            0,
        );
        let failed = find_obs(&ops, "Bash");
        assert!(failed.is_error);
        assert_eq!(failed.status_message.as_deref(), Some("exit code 1"));
    }

    #[test]
    fn a_subagent_stop_with_nothing_to_show_is_counted_not_drawn() {
        let mut asm = TurnAssembler::new(settings(ContentMode::Full), None, "test");
        let _ = asm.feed(user("nao faca mais nada", "2026-09-04T15:44:00Z"), 0);
        // Claude sends no SubagentStart and an empty SubagentStop payload
        let mut stop = hook(
            "SubagentStop",
            ns("2026-09-04T15:44:15Z"),
            None,
            serde_json::json!({}),
        );
        stop.agent_id = Some("a6d905f43532ba3f7".into());
        stop.agent_type = None;
        let ops = asm.attach_hook_event(&stop);
        assert!(
            observations(&ops).is_empty(),
            "no nameless empty span: {ops:?}"
        );
        let rows = traces(&ops);
        let turn = rows.last().expect("the turn is updated");
        assert_eq!(
            turn.metadata
                .as_ref()
                .and_then(|m| m.get("subagent_stops"))
                .and_then(|v| v.as_i64()),
            Some(1),
            "the fact is kept on the turn"
        );
        // a second one counts up rather than drawing again
        let mut other = stop.clone();
        other.agent_id = Some("ad595df9150e2c055".into());
        other.ts_ns = ns("2026-09-04T15:44:20Z");
        let ops = asm.attach_hook_event(&other);
        assert!(observations(&ops).is_empty());
        let rows = traces(&ops);
        assert_eq!(
            rows.last()
                .and_then(|t| t.metadata.as_ref())
                .and_then(|m| m.get("subagent_stops"))
                .and_then(|v| v.as_i64()),
            Some(2)
        );

        // but a stop that carries a result still gets its span
        let mut told = stop.clone();
        told.agent_id = Some("informative".into());
        told.agent_type = Some("Explore".into());
        told.ts_ns = ns("2026-09-04T15:44:25Z");
        let ops = asm.attach_hook_event(&told);
        assert_eq!(observations(&ops).len(), 1, "{ops:?}");
        assert_eq!(observations(&ops)[0].name, "agent: Explore");
    }

    #[test]
    fn a_child_tool_call_gives_its_agent_a_row_even_without_a_start() {
        let mut asm = TurnAssembler::new(settings(ContentMode::Full), None, "test");
        let _ = asm.feed(user("audit", "2026-09-04T16:00:00Z"), 0);
        // the first thing heard about this agent is a tool running inside it
        let mut child = hook(
            "PostToolUse",
            ns("2026-09-04T16:00:05Z"),
            Some("toolu_c9"),
            serde_json::json!({"tool_response": "3 hits"}),
        );
        child.agent_id = Some("a1".into());
        child.tool_name = Some("Grep".into());
        let ops = asm.attach_hook_event(&child);
        let rows = observations(&ops);
        assert_eq!(rows.len(), 2, "the agent and its child: {ops:?}");
        let agent = rows
            .iter()
            .find(|r| r.obs_type == ObservationType::Agent)
            .unwrap();
        let tool = rows
            .iter()
            .find(|r| r.obs_type == ObservationType::Tool)
            .unwrap();
        assert_eq!(tool.parent_id.as_deref(), Some(agent.id.as_str()));
        assert_eq!(tool.name, "Grep");
    }

    #[test]
    fn subagent_hooks_nest_child_tools_under_the_agent_row() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("sess-1".into()),
            "announced",
        );
        let _ = asm.feed(user("audit the ui", "2026-08-30T10:00:00Z"), 0);
        let _ = asm.feed(
            tool_use(
                "task-1",
                "Task",
                serde_json::json!({"subagent_type": "Explore", "description": "Audit the UI", "prompt": "look"}),
                "2026-08-30T10:00:01Z",
            ),
            0,
        );
        let trace_key = asm.turn.as_ref().unwrap().trace_key.clone();
        let trace_id = asm.turn.as_ref().unwrap().trace_id.clone();
        let agent_hook = |event: &str, ts: i64, payload: Value| {
            let mut ev = hook(event, ts, None, payload);
            ev.agent_id = Some("a1".into());
            ev.agent_type = Some("Explore".into());
            ev
        };
        let t_start = ns("2026-08-30T10:00:01.200Z");
        let ops =
            asm.attach_hook_event(&agent_hook("SubagentStart", t_start, serde_json::json!({})));
        let agent_row = match &ops[..] {
            [StoreOp::Observation(row)] => row.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            agent_row.id,
            ids::span_id_hex(&format!("{trace_key}|agent|a1"))
        );
        assert_eq!(agent_row.trace_id, trace_id);
        assert_eq!(agent_row.obs_type, ObservationType::Agent);
        assert_eq!(agent_row.name, "agent: Explore");
        assert_eq!(agent_row.start_ns, t_start);
        assert!(
            agent_row.parent_id.is_none(),
            "the Task row has no agent id yet"
        );

        // a tool call inside the subagent: Post before Pre, still one row
        let mut post = hook(
            "PostToolUse",
            ns("2026-08-30T10:00:03Z"),
            Some("toolu_c1"),
            serde_json::json!({"tool_input": {"pattern": "fn draw"}, "tool_response": "3 hits"}),
        );
        post.agent_id = Some("a1".into());
        post.tool_name = Some("Grep".into());
        let mut pre = post.clone();
        pre.event = "PreToolUse".into();
        pre.ts_ns = ns("2026-08-30T10:00:02Z");
        let ops = asm.attach_hook_event(&post);
        let child = match &ops[..] {
            [StoreOp::Observation(row)] => row.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            child.id,
            ids::span_id_hex(&format!("{trace_key}|agent|a1|tool|toolu_c1"))
        );
        assert_eq!(child.parent_id.as_deref(), Some(agent_row.id.as_str()));
        assert_eq!(child.name, "Grep");
        assert_eq!(child.output.as_deref(), Some("3 hits"));
        assert_eq!(child.input.as_deref(), Some(r#"{"pattern":"fn draw"}"#));
        assert_eq!(child.end_ns, Some(ns("2026-08-30T10:00:03Z")));
        let ops = asm.attach_hook_event(&pre);
        let child = match &ops[..] {
            [StoreOp::Observation(row)] => row.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(child.start_ns, ns("2026-08-30T10:00:02Z"));
        assert_eq!(child.end_ns, Some(ns("2026-08-30T10:00:03Z")));

        let ops = asm.attach_hook_event(&agent_hook(
            "SubagentStop",
            ns("2026-08-30T10:00:04Z"),
            serde_json::json!({"last_assistant_message": "found 3 draw fns", "agent_transcript_path": "/tmp/agent-a1.jsonl"}),
        ));
        let stopped = match &ops[..] {
            [StoreOp::Observation(row)] => row.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(stopped.id, agent_row.id);
        assert_eq!(stopped.end_ns, Some(ns("2026-08-30T10:00:04Z")));
        assert_eq!(stopped.output.as_deref(), Some("found 3 draw fns"));
        assert_eq!(
            stopped
                .metadata
                .get("agent_transcript_path")
                .and_then(|v| v.as_str()),
            Some("/tmp/agent-a1.jsonl")
        );

        // the transcript's Task result names the agent: the hook row is
        // re-parented under it
        let ops = asm.feed(
            TranscriptEvent::ToolResult {
                structured: Some(serde_json::json!({"agentId": "a1", "status": "completed"})),
                id: "task-1".into(),
                content: Value::from("done"),
                is_error: false,
                ts: Some("2026-08-30T10:00:05Z".into()),
            },
            0,
        );
        let rows: Vec<&ObservationRow> = ops
            .iter()
            .filter_map(|op| match op {
                StoreOp::Observation(row) => Some(row),
                _ => None,
            })
            .collect();
        let task = rows
            .iter()
            .find(|r| r.tool_id.as_deref() == Some("task-1"))
            .expect("task row");
        let relinked = rows
            .iter()
            .find(|r| r.id == agent_row.id)
            .expect("re-parented agent row");
        assert_eq!(relinked.parent_id.as_deref(), Some(task.id.as_str()));
        assert_eq!(relinked.end_ns, Some(ns("2026-08-30T10:00:04Z")));
        // a stray agent id without a known turn is ignored
        assert!(asm.hook_agent_tools.contains_key("toolu_c1"));
    }

    #[test]
    fn hook_tool_pins_apply_in_either_order() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("sess-1".into()),
            "announced",
        );
        let _ = asm.feed(user("go", "2026-08-30T10:00:00Z"), 0);
        let pre = ns("2026-08-30T10:00:00.900Z");
        let post = ns("2026-08-30T10:00:06.500Z");
        // hook first: parked, applied when the transcript row appears
        assert!(
            asm.attach_hook_event(&hook("PreToolUse", pre, Some("t1"), Value::Null))
                .is_empty()
        );
        let ops = asm.feed(
            tool_use(
                "t1",
                "Bash",
                serde_json::json!({"command": "ls"}),
                "2026-08-30T10:00:01Z",
            ),
            0,
        );
        let open = observations(&ops)[0];
        assert_eq!(open.start_ns, pre, "in-flight row pinned to the hook start");
        assert_eq!(open.metadata.get("hook_timed"), Some(&Value::Bool(true)));
        let ops = asm.feed(
            tool_result(
                "t1",
                Value::String("ok".into()),
                false,
                "2026-08-30T10:00:05Z",
            ),
            0,
        );
        let closed_row = observations(&ops)[0];
        assert_eq!(closed_row.start_ns, pre);
        assert_eq!(closed_row.end_ns, Some(ns("2026-08-30T10:00:05Z")));
        // hook after: the emitted row is re-emitted with the exact end
        let ops = asm.attach_hook_event(&hook("PostToolUse", post, Some("t1"), Value::Null));
        let pinned = observations(&ops);
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0].id, closed_row.id);
        assert_eq!(pinned[0].end_ns, Some(post));
        assert_eq!(
            pinned[0].output.as_deref(),
            Some("ok"),
            "content survives the re-emit"
        );
        // transcript first, then a failure hook: level flips, error kept
        let _ = asm.feed(
            tool_use("t2", "Bash", Value::Null, "2026-08-30T10:00:07Z"),
            0,
        );
        let _ = asm.feed(
            tool_result("t2", Value::Null, false, "2026-08-30T10:00:08Z"),
            0,
        );
        let ops = asm.attach_hook_event(&hook(
            "PostToolUseFailure",
            ns("2026-08-30T10:00:08.200Z"),
            Some("t2"),
            serde_json::json!({"tool_error": "exit status 2"}),
        ));
        let failed = observations(&ops)[0];
        assert!(failed.is_error);
        assert_eq!(failed.level, Level::Error);
        assert_eq!(failed.status_message.as_deref(), Some("tool error"));
        assert_eq!(
            failed.metadata.get("tool_error"),
            Some(&Value::from("exit status 2"))
        );
        // an id the transcript never produces stays parked
        assert!(
            asm.attach_hook_event(&hook("PreToolUse", 1, Some("never"), Value::Null))
                .is_empty()
        );
        assert!(asm.finalize().iter().all(
            |op| !matches!(op, StoreOp::Observation(o) if o.tool_id.as_deref() == Some("never"))
        ));
    }

    #[test]
    fn prompt_hook_pins_turn_start_in_either_order() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("sess-1".into()),
            "announced",
        );
        // hook first
        let early = ns("2026-08-30T09:59:59Z");
        assert!(
            asm.attach_hook_event(&hook("UserPromptSubmit", early, None, Value::Null))
                .is_empty()
        );
        let ops = asm.feed(user("first", "2026-08-30T10:00:00Z"), 0);
        let open = traces(&ops)[0];
        assert_eq!(open.start_ns, early);
        assert_eq!(
            open.metadata.as_ref().and_then(|m| m.get("turn_key")),
            Some(&Value::from("prompt-1"))
        );
        // transcript first, hook later
        let ops = asm.feed(user("second", "2026-08-30T10:05:00Z"), 0);
        let open2 = traces(&ops)
            .into_iter()
            .find(|t| t.status == TraceStatus::Open)
            .unwrap();
        assert_eq!(open2.start_ns, ns("2026-08-30T10:05:00Z"));
        let ops = asm.attach_hook_event(&hook(
            "UserPromptSubmit",
            ns("2026-08-30T10:04:58Z"),
            None,
            Value::Null,
        ));
        let pinned = traces(&ops)[0];
        assert_eq!(pinned.id, open2.id);
        assert_eq!(pinned.start_ns, ns("2026-08-30T10:04:58Z"));
        // a hook far outside the window does not move the turn
        let ops = asm.feed(user("third", "2026-08-30T11:00:00Z"), 0);
        let open3 = traces(&ops)
            .into_iter()
            .find(|t| t.status == TraceStatus::Open)
            .unwrap();
        assert!(
            asm.attach_hook_event(&hook(
                "UserPromptSubmit",
                ns("2026-08-30T10:30:00Z"),
                None,
                Value::Null
            ))
            .is_empty()
        );
        let close = asm.finalize();
        assert_eq!(closed(&close)[0].start_ns, open3.start_ns);
    }

    #[test]
    fn stop_hooks_pin_turn_end_cross_check_and_abort() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("sess-1".into()),
            "announced",
        );
        let _ = asm.feed(user("go", "2026-08-30T10:00:00Z"), 0);
        let _ = asm.feed(assistant("working", "2026-08-30T10:00:05Z", vec![]), 0);
        let stop = ns("2026-08-30T10:00:05.800Z");
        assert!(
            asm.attach_hook_event(&hook(
                "Stop",
                stop,
                None,
                serde_json::json!({"turn_number": 1, "last_assistant_message": "working"})
            ))
            .is_empty()
        );
        let ops = asm.feed(user("next", "2026-08-30T10:01:00Z"), 0);
        let first = closed(&ops)[0];
        assert_eq!(
            first.end_ns,
            Some(stop),
            "hook end beats the transcript's last line"
        );
        assert!(
            first
                .metadata
                .as_ref()
                .is_none_or(|m| m.get("turn_number_hook").is_none())
        );
        // late Stop for the already-closed second turn, with a mismatching number
        let _ = asm.feed(assistant("done", "2026-08-30T10:01:04Z", vec![]), 0);
        let ops = asm.feed(user("third", "2026-08-30T10:02:00Z"), 0);
        let second = closed(&ops)[0];
        assert_eq!(second.ordinal, 2);
        let late = ns("2026-08-30T10:01:04.700Z");
        let ops = asm.attach_hook_event(&hook(
            "Stop",
            late,
            None,
            serde_json::json!({"turn_number": 7}),
        ));
        let pinned = traces(&ops)[0];
        assert_eq!(pinned.id, second.id);
        assert_eq!(pinned.end_ns, Some(late));
        assert_eq!(
            pinned
                .metadata
                .as_ref()
                .and_then(|m| m.get("turn_number_hook")),
            Some(&Value::from(7))
        );
        // StopFailure aborts the open third turn right away
        let ops = asm.attach_hook_event(&hook(
            "StopFailure",
            ns("2026-08-30T10:02:03Z"),
            None,
            serde_json::json!({"error_type": "rate_limit"}),
        ));
        let aborted = closed(&ops)[0];
        assert_eq!(aborted.ordinal, 3);
        assert_eq!(aborted.status, TraceStatus::Aborted);
        assert_eq!(
            aborted.metadata.as_ref().and_then(|m| m.get("api_error")),
            Some(&Value::from("rate_limit"))
        );
        assert!(asm.finalize().is_empty(), "already closed");
    }

    #[test]
    fn compaction_model_switch_and_primed_turns() {
        let mut asm = TurnAssembler::new(
            settings(ContentMode::Metadata),
            Some("sess-1".into()),
            "announced",
        );
        let _ = asm.feed(user("go", "2026-08-30T10:00:00Z"), 0);
        let ops = asm.attach_hook_event(&hook(
            "PostCompact",
            ns("2026-08-30T10:00:02Z"),
            None,
            serde_json::json!({"compaction_trigger": "auto"}),
        ));
        assert_eq!(
            traces(&ops)[0]
                .metadata
                .as_ref()
                .and_then(|m| m.get("compacted")),
            Some(&Value::Bool(true))
        );
        let mut switch = hook(
            "PostModelSwitch",
            ns("2026-08-30T10:00:03Z"),
            None,
            Value::Null,
        );
        switch.model = Some("claude-opus-5".into());
        let ops = asm.attach_hook_event(&switch);
        assert!(ops.iter().any(|op| matches!(op, StoreOp::Session(s) if s.extra.as_ref().unwrap()["model"] == "claude-opus-5")));
        let ops = asm.feed(
            TranscriptEvent::Assistant {
                skill: None,
                text: "hi".into(),
                model: None,
                thinking: None,
                usage: vec![],
                msg_id: None,
                step_index: None,
                ts: Some("2026-08-30T10:00:04Z".into()),
            },
            0,
        );
        assert_eq!(
            find_obs(&ops, "assistant").model.as_deref(),
            Some("claude-opus-5")
        );
        // primed history never gets hook rows
        let mut primed = TurnAssembler::new(
            settings(ContentMode::Full),
            Some("sess-1".into()),
            "announced",
        );
        primed.set_emitting(false);
        let _ = primed.feed(user("old", "2026-08-30T09:00:00Z"), 0);
        primed.set_emitting(true);
        assert!(
            primed
                .attach_hook_event(&hook("Stop", ns("2026-08-30T09:00:05Z"), None, Value::Null))
                .is_empty()
        );
        assert!(
            primed
                .attach_hook_event(&hook(
                    "PostCompact",
                    ns("2026-08-30T09:00:06Z"),
                    None,
                    Value::Null
                ))
                .is_empty()
        );
    }

    #[test]
    fn session_meta_emits_a_session_row_with_extra() {
        let mut asm = TurnAssembler::new(
            MapSettings {
                provider: Provider::Codex,
                ..settings(ContentMode::Metadata)
            },
            None,
            "watched",
        );
        let ops = asm.feed(
            TranscriptEvent::SessionMeta {
                session_id: Some("rollout-1".into()),
                cwd: Some("/proj".into()),
                extra: serde_json::json!({"cli_version": "0.9", "model": "gpt-5.3-codex"}),
                ts: Some("2026-08-30T10:00:00Z".into()),
            },
            0,
        );
        let row = ops
            .iter()
            .find_map(|op| match op {
                StoreOp::Session(s) => Some(s),
                _ => None,
            })
            .expect("session row");
        assert_eq!(row.key, "codex:rollout-1");
        assert_eq!(row.extra.as_ref().unwrap()["cli_version"], "0.9");
        assert_eq!(asm.session_id(), Some("rollout-1"));
    }
}
