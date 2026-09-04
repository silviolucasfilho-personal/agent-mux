//! Store rows → OTLP/JSON spans for `POST {host}/api/public/otel/v1/traces`.
//!
//! Langfuse converts each span into the same ingestion events its own API
//! takes (`trace-create`, `<type>-create`), keyed by the span id, and
//! merges later writes field by field. Our stream is a sequence of
//! idempotent upserts (a turn open then closed, hook pins re-emitting tool
//! rows, agent rows re-parented), so re-sending a row updates it instead
//! of duplicating. Content policy is upstream: rows arrive already masked
//! or emptied for the launch's content mode and pass through unchanged.
//!
//! The sharp edges here (pinned by the tests below): ids are lowercase hex
//! strings, 64-bit nanos are decimal *strings*, attributes are a KeyValue
//! list of `AnyValue` wrappers (never a plain object — Langfuse rejects
//! the whole export for that), and the Langfuse-specific fields ride on
//! the attribute names in `LangfuseOtelSpanAttributes`.

use crate::config::ResolvedTracing;
use crate::tracing::ids;
use crate::tracing::pricing::{Cost, PriceTable, cost_for};
use crate::tracing::store::model::{
    Level, ObservationRow, ObservationType, StoreOp, TraceRow, TraceStatus,
};
use crate::tracing::usage::NormalizedUsage;
use serde_json::{Map, Value, json};

/// Settings shared by every span of a run.
#[derive(Debug, Clone)]
pub struct MapCtx {
    pub prices: PriceTable,
    pub version: String,
    pub user_id: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub tags: Vec<String>,
}

impl MapCtx {
    pub fn from_settings(settings: &ResolvedTracing) -> MapCtx {
        MapCtx {
            prices: crate::tracing::price_table(settings),
            version: env!("CARGO_PKG_VERSION").to_string(),
            user_id: settings.user_id.clone(),
            release: settings.release.clone(),
            environment: settings.environment.clone(),
            tags: settings.tags.clone(),
        }
    }
}

// Attribute names, from Langfuse's `LangfuseOtelSpanAttributes`.
const TRACE_NAME: &str = "langfuse.trace.name";
const TRACE_USER_ID: &str = "user.id";
const TRACE_SESSION_ID: &str = "session.id";
const TRACE_TAGS: &str = "langfuse.trace.tags";
const TRACE_METADATA: &str = "langfuse.trace.metadata";
const TRACE_INPUT: &str = "langfuse.trace.input";
const TRACE_OUTPUT: &str = "langfuse.trace.output";
const OBSERVATION_TYPE: &str = "langfuse.observation.type";
const OBSERVATION_METADATA: &str = "langfuse.observation.metadata";
const OBSERVATION_LEVEL: &str = "langfuse.observation.level";
const OBSERVATION_STATUS_MESSAGE: &str = "langfuse.observation.status_message";
const OBSERVATION_INPUT: &str = "langfuse.observation.input";
const OBSERVATION_OUTPUT: &str = "langfuse.observation.output";
const OBSERVATION_MODEL: &str = "langfuse.observation.model.name";
const OBSERVATION_USAGE_DETAILS: &str = "langfuse.observation.usage_details";
const OBSERVATION_COST_DETAILS: &str = "langfuse.observation.cost_details";
const ENVIRONMENT: &str = "langfuse.environment";
const RELEASE: &str = "langfuse.release";
const VERSION: &str = "langfuse.version";

const STATUS_UNSET: i64 = 0;
const STATUS_ERROR: i64 = 2;

/// One span, serialized once.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// The span id (or trace id for the turn root): batches report it.
    pub entity_id: String,
    /// The launch the row belongs to, when known (stats routing).
    pub launch_id: Option<String>,
    /// Langfuse observation type, for logging and tests.
    pub kind: &'static str,
    pub json: String,
}

impl Event {
    pub fn size(&self) -> usize {
        self.json.len() + 1
    }
}

/// The turn's own root span id: every turn row becomes the trace's root
/// span, and its observations hang under it.
pub fn root_span_id(trace_id: &str) -> String {
    ids::span_id_hex(&format!("amx1|otlp|root|{trace_id}"))
}

/// OTLP `AnyValue`: `{"stringValue": …}` / `{"intValue": "…"}`.
fn any_str(v: impl Into<String>) -> Value {
    json!({ "stringValue": v.into() })
}

fn attr(key: &str, value: Value) -> Value {
    json!({ "key": key, "value": value })
}

/// Adds a string attribute when the value is present and non-empty.
fn push_str_attr(attrs: &mut Vec<Value>, key: &str, value: Option<String>) {
    if let Some(v) = value.filter(|s| !s.is_empty()) {
        attrs.push(attr(key, any_str(v)));
    }
}

/// Adds a JSON-object attribute when it has any entries. Langfuse parses
/// these with `JSON.parse`, so they travel as strings.
fn push_json_attr(attrs: &mut Vec<Value>, key: &str, map: Map<String, Value>) {
    if !map.is_empty() {
        attrs.push(attr(key, any_str(Value::Object(map).to_string())));
    }
}

fn put<V: Into<Value>>(m: &mut Map<String, Value>, key: &str, v: Option<V>) {
    if let Some(v) = v {
        m.insert(key.to_string(), v.into());
    }
}

/// Langfuse's flat integer usage record.
pub fn usage_details(u: &NormalizedUsage) -> Map<String, Value> {
    let mut m = Map::new();
    put(&mut m, "input", u.input);
    put(&mut m, "output", u.output);
    put(&mut m, "total", u.total);
    put(&mut m, "input_cache_read", u.cache_read);
    put(&mut m, "input_cache_write", u.cache_write);
    put(&mut m, "input_cache_write_1h", u.cache_write_1h);
    put(&mut m, "output_reasoning", u.reasoning);
    m
}

/// USD per bucket, as the store prices it.
pub fn cost_details(c: &Cost) -> Map<String, Value> {
    let mut m = Map::new();
    put(&mut m, "input", c.input);
    put(&mut m, "output", c.output);
    put(&mut m, "input_cache_read", c.cache_read);
    put(&mut m, "input_cache_write", c.cache_write);
    put(&mut m, "total", c.total);
    m
}

/// Nanoseconds as the decimal string OTLP/JSON requires.
fn nanos(ns: i64) -> String {
    u64::try_from(ns).unwrap_or(0).to_string()
}

#[allow(clippy::too_many_arguments)]
fn span_json(
    trace_id: &str,
    span_id: &str,
    parent_span_id: Option<&str>,
    name: &str,
    start_ns: i64,
    end_ns: i64,
    attributes: Vec<Value>,
    error: Option<Option<String>>,
) -> String {
    let mut span = Map::new();
    span.insert("traceId".into(), Value::from(trace_id));
    span.insert("spanId".into(), Value::from(span_id));
    if let Some(parent) = parent_span_id {
        span.insert("parentSpanId".into(), Value::from(parent));
    }
    span.insert("name".into(), Value::from(name));
    // kind 1 = INTERNAL
    span.insert("kind".into(), Value::from(1));
    span.insert("startTimeUnixNano".into(), Value::from(nanos(start_ns)));
    span.insert(
        "endTimeUnixNano".into(),
        Value::from(nanos(end_ns.max(start_ns))),
    );
    span.insert("attributes".into(), Value::Array(attributes));
    let mut status = Map::new();
    match error {
        Some(message) => {
            status.insert("code".into(), Value::from(STATUS_ERROR));
            if let Some(m) = message {
                status.insert("message".into(), Value::from(m));
            }
        }
        None => {
            status.insert("code".into(), Value::from(STATUS_UNSET));
        }
    }
    span.insert("status".into(), Value::Object(status));
    Value::Object(span).to_string()
}

/// Attributes every span carries.
fn common_attrs(ctx: &MapCtx) -> Vec<Value> {
    let mut attrs = Vec::new();
    push_str_attr(&mut attrs, ENVIRONMENT, ctx.environment.clone());
    push_str_attr(&mut attrs, RELEASE, ctx.release.clone());
    attrs.push(attr(VERSION, any_str(ctx.version.clone())));
    attrs
}

fn trace_span(t: &TraceRow, ctx: &MapCtx) -> Event {
    let mut metadata = t
        .metadata
        .as_ref()
        .and_then(|m| m.as_object().cloned())
        .unwrap_or_default();
    metadata.insert("provider".into(), Value::from(t.provider.clone()));
    metadata.insert("ordinal".into(), Value::from(t.ordinal));
    metadata.insert("status".into(), Value::from(t.status.as_str()));
    metadata.insert("session_key".into(), Value::from(t.session_key.clone()));
    metadata.insert("start_ns".into(), Value::from(t.start_ns));
    put(&mut metadata, "launch_id", t.launch_id.clone());
    put(&mut metadata, "end_ns", t.end_ns);
    put(
        &mut metadata,
        "latency_ms",
        t.end_ns.map(|e| (e - t.start_ns).max(0) / 1_000_000),
    );
    put(&mut metadata, "thinking", t.thinking.clone());
    put(&mut metadata, "skills", t.skills.clone());
    put(
        &mut metadata,
        "reported_duration_ms",
        t.reported_duration_ms,
    );
    put(
        &mut metadata,
        "reported_message_count",
        t.reported_message_count,
    );
    put(&mut metadata, "session_cost_usd", t.session_cost_usd);
    if t.timing_approx {
        metadata.insert("timing_approx".into(), Value::Bool(true));
    }

    let mut tags = ctx.tags.clone();
    if !tags.iter().any(|x| x == &t.provider) {
        tags.push(t.provider.clone());
    }
    let name: String = t.name.chars().take(1000).collect();
    let mut attrs = common_attrs(ctx);
    attrs.push(attr(OBSERVATION_TYPE, any_str("span")));
    attrs.push(attr(TRACE_NAME, any_str(name.clone())));
    attrs.push(attr(TRACE_SESSION_ID, any_str(t.session_id.clone())));
    push_str_attr(&mut attrs, TRACE_USER_ID, ctx.user_id.clone());
    push_str_attr(&mut attrs, TRACE_INPUT, t.input.clone());
    push_str_attr(&mut attrs, TRACE_OUTPUT, t.output.clone());
    push_str_attr(&mut attrs, OBSERVATION_INPUT, t.input.clone());
    push_str_attr(&mut attrs, OBSERVATION_OUTPUT, t.output.clone());
    // a JSON array string: Langfuse parses it, and an OTLP array
    // attribute is dropped by some paths
    attrs.push(attr(TRACE_TAGS, any_str(Value::from(tags).to_string())));
    push_json_attr(&mut attrs, TRACE_METADATA, metadata.clone());
    push_json_attr(&mut attrs, OBSERVATION_METADATA, metadata);
    let aborted = t.status == TraceStatus::Aborted;
    if aborted {
        attrs.push(attr(OBSERVATION_LEVEL, any_str("ERROR")));
    }
    let span_id = root_span_id(&t.id);
    let json = span_json(
        &t.id,
        &span_id,
        None,
        &name,
        t.start_ns,
        t.end_ns.unwrap_or(t.start_ns),
        attrs,
        aborted.then_some(Some("aborted".to_string())),
    );
    Event {
        entity_id: span_id,
        launch_id: t.launch_id.clone(),
        kind: "trace",
        json,
    }
}

fn observation_span(o: &ObservationRow, ctx: &MapCtx) -> Event {
    let kind: &'static str = match o.obs_type {
        ObservationType::Generation => "generation",
        ObservationType::Tool => "tool",
        ObservationType::Agent => "agent",
        ObservationType::Event => "event",
        ObservationType::Span => "span",
    };
    let mut metadata = o.metadata.clone();
    put(&mut metadata, "kind", o.kind.clone());
    put(&mut metadata, "tool_id", o.tool_id.clone());
    put(&mut metadata, "tool_name", o.tool_name.clone());
    put(&mut metadata, "skill", o.skill.clone());
    put(&mut metadata, "mcp_server", o.mcp_server.clone());
    put(&mut metadata, "path", o.path.clone());
    put(&mut metadata, "thinking", o.thinking.clone());
    metadata.insert("start_ns".into(), Value::from(o.start_ns));
    put(&mut metadata, "end_ns", o.end_ns);
    if o.end_ns.is_none() {
        metadata.insert("running".into(), Value::Bool(true));
    }
    if o.ts_approx {
        metadata.insert("timing_approx".into(), Value::Bool(true));
    }
    if let Some(raw) = &o.usage_raw {
        let mut m = Map::new();
        for (k, v) in raw {
            m.insert(k.clone(), Value::from(*v));
        }
        metadata.insert("usage_raw".into(), Value::Object(m));
    }

    let level = if o.is_error { Level::Error } else { o.level };
    let mut attrs = common_attrs(ctx);
    attrs.push(attr(OBSERVATION_TYPE, any_str(kind)));
    attrs.push(attr(OBSERVATION_LEVEL, any_str(level.as_str())));
    push_str_attr(
        &mut attrs,
        OBSERVATION_STATUS_MESSAGE,
        o.status_message.clone(),
    );
    push_str_attr(&mut attrs, OBSERVATION_INPUT, o.input.clone());
    push_str_attr(&mut attrs, OBSERVATION_OUTPUT, o.output.clone());
    if o.obs_type == ObservationType::Generation {
        push_str_attr(&mut attrs, OBSERVATION_MODEL, o.model.clone());
        if let Some(usage) = &o.usage {
            push_json_attr(&mut attrs, OBSERVATION_USAGE_DETAILS, usage_details(usage));
            if let Some(price) = o.model.as_deref().and_then(|m| ctx.prices.find(m)) {
                push_json_attr(
                    &mut attrs,
                    OBSERVATION_COST_DETAILS,
                    cost_details(&cost_for(price, usage)),
                );
            }
        }
    }
    push_json_attr(&mut attrs, OBSERVATION_METADATA, metadata);
    let parent = o
        .parent_id
        .clone()
        .unwrap_or_else(|| root_span_id(&o.trace_id));
    let json = span_json(
        &o.trace_id,
        &o.id,
        Some(&parent),
        &o.name,
        o.start_ns,
        o.end_ns.unwrap_or(o.start_ns),
        attrs,
        o.is_error
            .then_some(o.status_message.clone().or(Some("error".to_string()))),
    );
    Event {
        entity_id: o.id.clone(),
        launch_id: None,
        kind,
        json,
    }
}

/// The span for one op; launch and session rows have no Langfuse shape.
pub fn event_for(op: &StoreOp, ctx: &MapCtx) -> Option<Event> {
    match op {
        StoreOp::Trace(t) => Some(trace_span(t, ctx)),
        StoreOp::Observation(o) => Some(observation_span(o, ctx)),
        StoreOp::Launch(_) | StoreOp::Session(_) => None,
    }
}

/// The cost the store would attach to a generation row.
pub fn row_cost(o: &ObservationRow, prices: &PriceTable) -> Option<f64> {
    let usage = o.usage.as_ref()?;
    let price = prices.find(o.model.as_deref()?)?;
    cost_for(price, usage).total
}

/// A batch under construction: closes at `max_events` or `max_bytes`.
#[derive(Debug, Default)]
pub struct Batch {
    pub events: Vec<Event>,
    pub bytes: usize,
}

impl Batch {
    pub fn push(&mut self, event: Event) {
        self.bytes += event.size();
        self.events.push(event);
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_full(&self, max_events: usize, max_bytes: usize) -> bool {
        self.events.len() >= max_events || self.bytes >= max_bytes
    }

    /// One `ExportTraceServiceRequest` body.
    pub fn body(&self) -> String {
        let mut out = String::with_capacity(self.bytes + 256);
        out.push_str(
            r#"{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"agent-mux"}}]},"scopeSpans":[{"scope":{"name":"agent-mux"},"spans":["#,
        );
        for (i, e) in self.events.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&e.json);
        }
        out.push_str("]}]}]}");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> MapCtx {
        MapCtx {
            prices: PriceTable::builtin(),
            version: "9.9.9".into(),
            user_id: Some("me".into()),
            release: Some("r1".into()),
            environment: Some("dev".into()),
            tags: vec!["agent-mux".into()],
        }
    }

    fn trace() -> TraceRow {
        TraceRow {
            id: "0123456789abcdef0123456789abcdef".into(),
            session_key: "claude:sess-1".into(),
            provider: "claude".into(),
            session_id: "sess-1".into(),
            launch_id: Some("launch-1".into()),
            ordinal: 2,
            name: "Claude Code: run the tests".into(),
            status: TraceStatus::Closed,
            start_ns: 1_756_980_000_123_456_789,
            end_ns: Some(1_756_980_004_000_000_000),
            input: Some("run the tests".into()),
            output: Some("done".into()),
            thinking: None,
            skills: Some(vec!["superpowers".into()]),
            reported_duration_ms: None,
            reported_message_count: None,
            session_cost_usd: None,
            timing_approx: false,
            ordinal_salted: false,
            metadata: Some(json!({"turn_key": "p1"})),
        }
    }

    fn observation(kind: ObservationType) -> ObservationRow {
        ObservationRow {
            id: "89abcdef01234567".into(),
            trace_id: "0123456789abcdef0123456789abcdef".into(),
            parent_id: None,
            obs_type: kind,
            name: "assistant".into(),
            kind: None,
            start_ns: 1_756_980_001_000_000_000,
            end_ns: Some(1_756_980_002_500_000_000),
            level: Level::Default,
            status_message: None,
            model: Some("claude-haiku-4-5".into()),
            input: Some("hi".into()),
            output: Some("hello".into()),
            thinking: Some("hmm".into()),
            usage_raw: Some(vec![
                ("input_tokens".into(), 10),
                ("output_tokens".into(), 5),
            ]),
            usage: Some(NormalizedUsage {
                input: Some(10),
                output: Some(5),
                cache_read: Some(100),
                cache_write: None,
                cache_write_1h: None,
                reasoning: None,
                total: Some(115),
            }),
            tool_id: None,
            tool_name: None,
            skill: None,
            mcp_server: None,
            path: None,
            is_error: false,
            ts_approx: false,
            metadata: Map::new(),
        }
    }

    fn parsed(e: &Event) -> Value {
        serde_json::from_str(&e.json).unwrap()
    }

    /// Attributes are a KeyValue list, so a lookup walks it.
    fn attr_of<'a>(span: &'a Value, key: &str) -> Option<&'a Value> {
        span["attributes"]
            .as_array()?
            .iter()
            .find(|a| a["key"] == key)
            .map(|a| &a["value"])
    }

    fn attr_str<'a>(span: &'a Value, key: &str) -> Option<&'a str> {
        attr_of(span, key)?["stringValue"].as_str()
    }

    #[test]
    fn turn_rows_become_the_traces_root_span() {
        let e = event_for(&StoreOp::Trace(trace()), &ctx()).unwrap();
        assert_eq!(e.kind, "trace");
        assert_eq!(e.launch_id.as_deref(), Some("launch-1"));
        let s = parsed(&e);
        assert_eq!(s["traceId"], "0123456789abcdef0123456789abcdef");
        assert_eq!(
            s["spanId"].as_str().unwrap(),
            root_span_id("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(s["spanId"].as_str().unwrap().len(), 16, "16 hex = 8 bytes");
        assert!(s.get("parentSpanId").is_none(), "the turn is the root");
        assert_eq!(s["name"], "Claude Code: run the tests");
        // proto3 JSON: 64-bit nanos are decimal strings
        assert_eq!(s["startTimeUnixNano"], "1756980000123456789");
        assert_eq!(s["endTimeUnixNano"], "1756980004000000000");
        assert_eq!(s["status"]["code"], 0);
        assert_eq!(
            attr_str(&s, "langfuse.trace.name"),
            Some("Claude Code: run the tests")
        );
        assert_eq!(attr_str(&s, "session.id"), Some("sess-1"));
        assert_eq!(attr_str(&s, "user.id"), Some("me"));
        assert_eq!(attr_str(&s, "langfuse.trace.input"), Some("run the tests"));
        assert_eq!(attr_str(&s, "langfuse.trace.output"), Some("done"));
        assert_eq!(attr_str(&s, "langfuse.environment"), Some("dev"));
        assert_eq!(attr_str(&s, "langfuse.release"), Some("r1"));
        assert_eq!(attr_str(&s, "langfuse.version"), Some("9.9.9"));
        assert_eq!(attr_str(&s, "langfuse.observation.type"), Some("span"));
        let tags: Value =
            serde_json::from_str(attr_str(&s, "langfuse.trace.tags").unwrap()).unwrap();
        assert_eq!(tags, json!(["agent-mux", "claude"]));
        let meta: Value =
            serde_json::from_str(attr_str(&s, "langfuse.trace.metadata").unwrap()).unwrap();
        assert_eq!(meta["provider"], "claude");
        assert_eq!(meta["ordinal"], 2);
        assert_eq!(meta["status"], "closed");
        assert_eq!(meta["latency_ms"], 3876);
        assert_eq!(meta["turn_key"], "p1");
        assert_eq!(meta["skills"], json!(["superpowers"]));

        // an aborted turn is an error span
        let mut aborted = trace();
        aborted.status = TraceStatus::Aborted;
        let s = parsed(&event_for(&StoreOp::Trace(aborted), &ctx()).unwrap());
        assert_eq!(s["status"]["code"], 2);
        assert_eq!(s["status"]["message"], "aborted");
        assert_eq!(attr_str(&s, "langfuse.observation.level"), Some("ERROR"));

        // an open turn still needs an end time; the closed row corrects it
        let mut open = trace();
        open.status = TraceStatus::Open;
        open.end_ns = None;
        let s = parsed(&event_for(&StoreOp::Trace(open), &ctx()).unwrap());
        assert_eq!(s["endTimeUnixNano"], s["startTimeUnixNano"]);
    }

    #[test]
    fn generations_carry_usage_and_cost_other_kinds_do_not() {
        let e = event_for(
            &StoreOp::Observation(observation(ObservationType::Generation)),
            &ctx(),
        )
        .unwrap();
        assert_eq!(e.kind, "generation");
        let s = parsed(&e);
        assert_eq!(s["traceId"], "0123456789abcdef0123456789abcdef");
        assert_eq!(s["spanId"], "89abcdef01234567");
        assert_eq!(
            s["parentSpanId"].as_str().unwrap(),
            root_span_id("0123456789abcdef0123456789abcdef"),
            "a parentless observation hangs under the turn root"
        );
        assert_eq!(
            attr_str(&s, "langfuse.observation.type"),
            Some("generation")
        );
        assert_eq!(
            attr_str(&s, "langfuse.observation.model.name"),
            Some("claude-haiku-4-5")
        );
        assert_eq!(attr_str(&s, "langfuse.observation.level"), Some("DEFAULT"));
        let usage: Value =
            serde_json::from_str(attr_str(&s, "langfuse.observation.usage_details").unwrap())
                .unwrap();
        assert_eq!(
            usage,
            json!({"input": 10, "output": 5, "total": 115, "input_cache_read": 100})
        );
        let cost: Value =
            serde_json::from_str(attr_str(&s, "langfuse.observation.cost_details").unwrap())
                .unwrap();
        assert!(cost["input"].as_f64().unwrap() > 0.0);
        assert!(cost["total"].as_f64().unwrap() > cost["input"].as_f64().unwrap());
        let meta: Value =
            serde_json::from_str(attr_str(&s, "langfuse.observation.metadata").unwrap()).unwrap();
        assert_eq!(meta["thinking"], "hmm");
        assert_eq!(meta["usage_raw"]["input_tokens"], 10);

        // an explicit parent is kept (subagent nesting)
        let mut child = observation(ObservationType::Tool);
        child.parent_id = Some("0011223344556677".into());
        child.name = "Bash".into();
        child.tool_id = Some("toolu_1".into());
        child.is_error = true;
        child.status_message = Some("tool error".into());
        let e = event_for(&StoreOp::Observation(child), &ctx()).unwrap();
        assert_eq!(e.kind, "tool");
        let s = parsed(&e);
        assert_eq!(s["parentSpanId"], "0011223344556677");
        assert!(attr_of(&s, "langfuse.observation.model.name").is_none());
        assert!(attr_of(&s, "langfuse.observation.usage_details").is_none());
        assert_eq!(attr_str(&s, "langfuse.observation.level"), Some("ERROR"));
        assert_eq!(
            attr_str(&s, "langfuse.observation.status_message"),
            Some("tool error")
        );
        assert_eq!(s["status"]["code"], 2);
        assert_eq!(s["status"]["message"], "tool error");

        for (kind, expected) in [
            (ObservationType::Agent, "agent"),
            (ObservationType::Event, "event"),
            (ObservationType::Span, "span"),
        ] {
            let e = event_for(&StoreOp::Observation(observation(kind)), &ctx()).unwrap();
            assert_eq!(e.kind, expected);
            assert_eq!(
                attr_str(&parsed(&e), "langfuse.observation.type"),
                Some(expected)
            );
        }

        // a running tool keeps a valid (zero-length) span until it closes
        let mut running = observation(ObservationType::Tool);
        running.end_ns = None;
        let s = parsed(&event_for(&StoreOp::Observation(running), &ctx()).unwrap());
        assert_eq!(s["endTimeUnixNano"], s["startTimeUnixNano"]);
        let meta: Value =
            serde_json::from_str(attr_str(&s, "langfuse.observation.metadata").unwrap()).unwrap();
        assert_eq!(meta["running"], true);
    }

    #[test]
    fn batches_close_on_count_or_bytes_and_serialize_as_one_export() {
        let mut b = Batch::default();
        let e = event_for(&StoreOp::Trace(trace()), &ctx()).unwrap();
        for _ in 0..3 {
            b.push(e.clone());
        }
        assert_eq!(b.len(), 3);
        assert!(!b.is_full(4, usize::MAX));
        assert!(b.is_full(3, usize::MAX));
        assert!(b.is_full(usize::MAX, 10));
        let v: Value = serde_json::from_str(&b.body()).unwrap();
        let spans = v["resourceSpans"][0]["scopeSpans"][0]["spans"]
            .as_array()
            .unwrap();
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[2]["traceId"], "0123456789abcdef0123456789abcdef");
        // resource attributes are a KeyValue list too: a map-shaped one
        // makes Langfuse reject the whole export
        let resource = &v["resourceSpans"][0]["resource"]["attributes"];
        assert!(resource.is_array());
        assert_eq!(resource[0]["key"], "service.name");
        assert_eq!(
            v["resourceSpans"][0]["scopeSpans"][0]["scope"]["name"],
            "agent-mux"
        );
        assert!(Batch::default().body().contains("\"spans\":[]"));
    }
}
