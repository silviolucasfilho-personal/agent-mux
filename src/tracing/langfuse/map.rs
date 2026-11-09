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

use crate::bus::swarm::SwarmLaunch;
use crate::config::ResolvedTracing;
use crate::tracing::ids;
use crate::tracing::pricing::{Cost, PriceTable, cost_for};
use crate::tracing::store::model::{
    LaunchRow, Level, ObservationRow, ObservationType, StoreOp, TraceRow, TraceStatus,
};
use crate::tracing::swarm_report::SwarmReport;
use crate::tracing::usage::NormalizedUsage;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

/// Settings shared by every span of a run.
#[derive(Debug, Clone)]
pub struct MapCtx {
    pub prices: PriceTable,
    pub version: String,
    pub user_id: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub tags: Vec<String>,
    /// The launches that belong to a swarm (coordinator-first spec §7.3),
    /// by launch id: their turns get `session.id = <swarm id>` and the
    /// swarm tags. Learned from `StoreOp::Launch` rows as they pass
    /// ([`MapCtx::note_launch`]) or seeded from the store for a replay.
    pub swarms: HashMap<String, SwarmLaunch>,
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
            swarms: HashMap::new(),
        }
    }

    /// Remembers a launch's swarm, from its row's metadata.
    pub fn note_launch(&mut self, l: &LaunchRow) {
        if let Some(sw) = l.metadata.as_ref().and_then(SwarmLaunch::from_metadata) {
            self.swarms.insert(l.id.clone(), sw);
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
    put(&mut m, "cache_read_input_tokens", u.cache_read);
    put(&mut m, "input_cache_creation_5m", u.cache_write);
    put(&mut m, "input_cache_creation_1h", u.cache_write_1h);
    put(&mut m, "output_reasoning_tokens", u.reasoning);
    m
}

/// USD per bucket, as the store prices it.
pub fn cost_details(c: &Cost) -> Map<String, Value> {
    let mut m = Map::new();
    put(&mut m, "input", c.input);
    put(&mut m, "output", c.output);
    put(&mut m, "cache_read_input_tokens", c.cache_read);
    put(&mut m, "input_cache_creation_5m", c.cache_write);
    put(&mut m, "total", c.total);
    m
}

/// Provider-reported cost is authoritative.  Keep it distinct from local
/// pricing, but serialize it through the same Langfuse cost-details field so
/// an export faithfully represents the source rather than silently replacing
/// it with the current local price table.
fn provided_cost_details(metadata: &Map<String, Value>) -> Option<Map<String, Value>> {
    let source = metadata.get("provided_cost")?.as_object()?;
    let mut details = Map::new();
    for (from, to) in [
        ("input", "input"),
        ("output", "output"),
        ("cache_read_input_tokens", "cache_read_input_tokens"),
        ("input_cache_creation_5m", "input_cache_creation_5m"),
        ("total", "total"),
    ] {
        if let Some(value) = source.get(from).filter(|v| v.is_number()) {
            details.insert(to.into(), value.clone());
        }
    }
    (!details.is_empty()).then_some(details)
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
    // a swarm's turn: one Langfuse session per swarm (§7.3), so the
    // coordinator's and the workers' turns list together in order
    let swarm = t.launch_id.as_deref().and_then(|l| ctx.swarms.get(l));
    let session_id = match swarm {
        Some(sw) => {
            tags.push("swarm".into());
            tags.push(format!("swarm:{}", sw.role.as_str()));
            metadata.insert("swarm_id".into(), Value::from(sw.swarm_id.clone()));
            metadata.insert("swarm_role".into(), Value::from(sw.role.as_str()));
            put(&mut metadata, "swarm_task", sw.task_id.clone());
            put(&mut metadata, "swarm_task_key", sw.task_key.clone());
            put(&mut metadata, "parent_launch", sw.parent_launch.clone());
            metadata.insert("harness_session".into(), Value::from(t.session_id.clone()));
            sw.swarm_id.clone()
        }
        None => t.session_id.clone(),
    };
    let name: String = t.name.chars().take(1000).collect();
    let mut attrs = common_attrs(ctx);
    attrs.push(attr(OBSERVATION_TYPE, any_str("span")));
    attrs.push(attr(TRACE_NAME, any_str(name.clone())));
    attrs.push(attr(TRACE_SESSION_ID, any_str(session_id)));
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

    let level = o.level;
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
        if let Some(cost) = provided_cost_details(&o.metadata) {
            push_json_attr(&mut attrs, OBSERVATION_COST_DETAILS, cost);
        } else if let Some(usage) = &o.usage {
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
        matches!(o.level, Level::Error)
            .then_some(o.status_message.clone().or(Some("error".to_string()))),
    );
    Event {
        entity_id: o.id.clone(),
        launch_id: None,
        kind,
        json,
    }
}

/// The swarm's trace id: one deterministic trace per swarm.
pub fn swarm_trace_id(swarm_id: &str) -> String {
    ids::trace_id_hex(&format!("amx1|swarm|{swarm_id}"))
}

fn text(v: &str, max: usize) -> String {
    v.chars().take(max).collect()
}

/// The summary trace of a swarm (coordinator-first spec §7.3): one trace
/// named `swarm <goal>` in the swarm's Langfuse session, with a root span
/// for the swarm, a child span per task (its timing, state, worker launch,
/// tokens, cost, result or error, and the worker's trace ids: OTLP cannot
/// parent a span across traces, so the link is metadata) and an event per
/// decision. Sent when the swarm closes and by `langfuse export-swarm`;
/// the ids are deterministic, so sending it again updates it.
pub fn swarm_summary(r: &SwarmReport, ctx: &MapCtx) -> Vec<Event> {
    let s = &r.swarm;
    let trace_id = swarm_trace_id(&s.id);
    let root_id = ids::span_id_hex(&format!("amx1|swarm|root|{}", s.id));
    let goal = text(r.swarm.goal.lines().next().unwrap_or(""), 120);
    let name = format!("swarm {goal}");
    let now = crate::tracing::store::now_ns();
    let end = s.ended_ns.unwrap_or(now);
    let launch_of = |id: Option<&str>| r.launches.iter().find(|l| Some(l.launch_id.as_str()) == id);
    let mut events = Vec::with_capacity(1 + r.tasks.len() + r.decisions.len());

    let mut tags = ctx.tags.clone();
    tags.push("swarm".into());
    tags.push("swarm:summary".into());
    for l in &r.launches {
        if !tags.contains(&l.provider) {
            tags.push(l.provider.clone());
        }
    }
    let mut metadata = Map::new();
    metadata.insert("swarm_id".into(), Value::from(s.id.clone()));
    metadata.insert("workspace".into(), Value::from(s.workspace.clone()));
    metadata.insert(
        "status".into(),
        Value::from(if s.ended_ns.is_some() {
            "closed"
        } else {
            s.status.as_str()
        }),
    );
    metadata.insert("gate_mode".into(), Value::from(r.gate_mode.clone()));
    metadata.insert("max_workers".into(), Value::from(s.max_workers));
    metadata.insert("isolation".into(), Value::from(s.isolation.as_str()));
    metadata.insert("done".into(), Value::from(r.done));
    metadata.insert("total".into(), Value::from(r.total));
    put(&mut metadata, "tokens", r.tokens);
    put(&mut metadata, "cost_usd", r.cost_usd);
    metadata.insert("unpriced".into(), Value::Bool(r.unpriced));
    put(&mut metadata, "coordinator", r.coordinator.clone());
    metadata.insert(
        "launches".into(),
        Value::Array(
            r.launches
                .iter()
                .map(|l| Value::from(l.launch_id.clone()))
                .collect(),
        ),
    );
    put(&mut metadata, "check_cmd", s.check_cmd.clone());
    put(&mut metadata, "check_result", s.check_result.clone());
    metadata.insert("started_ns".into(), Value::from(s.started_ns));
    put(&mut metadata, "ended_ns", s.ended_ns);
    metadata.insert("decisions".into(), Value::from(r.decisions.len()));
    metadata.insert("events".into(), Value::from(r.events.len()));
    let output = format!(
        "{} · {}/{} done · {} launch(es) · {} tok · {}{}{}",
        if s.ended_ns.is_some() {
            "closed"
        } else {
            s.status.as_str()
        },
        r.done,
        r.total,
        r.launches.len(),
        r.tokens
            .map(|t| t.to_string())
            .unwrap_or_else(|| "—".into()),
        r.cost_usd
            .map(|c| format!("${c:.2}"))
            .unwrap_or_else(|| "—".into()),
        s.plan_summary
            .as_deref()
            .map(|p| format!("\nplan: {p}"))
            .unwrap_or_default(),
        s.check_result
            .as_deref()
            .map(|c| format!("\ncheck: {c}"))
            .unwrap_or_default(),
    );
    let mut attrs = common_attrs(ctx);
    attrs.push(attr(OBSERVATION_TYPE, any_str("span")));
    attrs.push(attr(TRACE_NAME, any_str(name.clone())));
    attrs.push(attr(TRACE_SESSION_ID, any_str(s.id.clone())));
    push_str_attr(&mut attrs, TRACE_USER_ID, ctx.user_id.clone());
    attrs.push(attr(TRACE_INPUT, any_str(text(&s.goal, 4000))));
    attrs.push(attr(TRACE_OUTPUT, any_str(output.clone())));
    attrs.push(attr(OBSERVATION_INPUT, any_str(text(&s.goal, 4000))));
    attrs.push(attr(OBSERVATION_OUTPUT, any_str(output)));
    attrs.push(attr(TRACE_TAGS, any_str(Value::from(tags).to_string())));
    push_json_attr(&mut attrs, TRACE_METADATA, metadata.clone());
    push_json_attr(&mut attrs, OBSERVATION_METADATA, metadata);
    let failed = r.tasks.iter().filter(|t| t.state == "failed").count();
    let error = (failed > 0).then(|| Some(format!("{failed} task(s) failed")));
    if error.is_some() {
        attrs.push(attr(OBSERVATION_LEVEL, any_str("ERROR")));
    }
    events.push(Event {
        entity_id: root_id.clone(),
        launch_id: None,
        kind: "swarm",
        json: span_json(
            &trace_id,
            &root_id,
            None,
            &name,
            s.started_ns,
            end,
            attrs,
            error,
        ),
    });

    for t in &r.tasks {
        let span_id = ids::span_id_hex(&format!("amx1|swarm|task|{}", t.id));
        let worker = launch_of(t.worker.as_deref());
        let start = worker.map(|l| l.started_ns).unwrap_or(s.started_ns);
        let task_end = t
            .ended_ns
            .or_else(|| worker.and_then(|l| l.ended_ns))
            .unwrap_or(if t.worker.is_some() { end } else { start });
        let mut m = Map::new();
        m.insert("task_id".into(), Value::from(t.id.clone()));
        m.insert("task_key".into(), Value::from(t.key.clone()));
        m.insert("state".into(), Value::from(t.state.clone()));
        m.insert("status".into(), Value::from(t.status.clone()));
        put(&mut m, "harness", t.harness.clone());
        put(&mut m, "agent", t.agent.clone());
        m.insert("isolation".into(), Value::from(t.isolation.clone()));
        m.insert(
            "depends_on".into(),
            Value::Array(
                t.depends_on
                    .iter()
                    .map(|d| Value::from(d.clone()))
                    .collect(),
            ),
        );
        m.insert(
            "files".into(),
            Value::Array(t.files.iter().map(|f| Value::from(f.clone())).collect()),
        );
        put(&mut m, "worker", t.worker.clone());
        put(&mut m, "launched_by", t.launched_by.clone());
        put(&mut m, "offered_ns", t.offered_ns);
        m.insert("attempts".into(), Value::from(t.attempts));
        put(&mut m, "branch", t.branch.clone());
        put(&mut m, "review", t.review.clone());
        put(&mut m, "error", t.error.clone());
        if let Some(res) = &t.result {
            m.insert("result".into(), res.clone());
        }
        if let Some(l) = worker {
            put(&mut m, "tokens", l.tokens);
            put(&mut m, "cost_usd", l.cost_usd);
            m.insert("turns".into(), Value::from(l.turns.len()));
            m.insert("tools".into(), Value::from(l.tools));
            m.insert("errors".into(), Value::from(l.errors));
            m.insert(
                "worker_traces".into(),
                Value::Array(l.turns.iter().map(|t| Value::from(t.id.clone())).collect()),
            );
            m.insert(
                "sub_agents".into(),
                Value::Array(
                    l.sub_agents
                        .iter()
                        .map(|a| Value::from(a.name.clone()))
                        .collect(),
                ),
            );
        }
        let mut attrs = common_attrs(ctx);
        attrs.push(attr(OBSERVATION_TYPE, any_str("span")));
        push_str_attr(&mut attrs, OBSERVATION_INPUT, t.acceptance.clone());
        let out = match (&t.result, &t.error) {
            (_, Some(e)) => Some(text(e, 4000)),
            (Some(Value::String(r)), _) => Some(text(r, 4000)),
            (Some(other), _) => Some(text(&other.to_string(), 4000)),
            _ => None,
        };
        push_str_attr(&mut attrs, OBSERVATION_OUTPUT, out);
        push_json_attr(&mut attrs, OBSERVATION_METADATA, m);
        let error = matches!(t.state.as_str(), "failed" | "lost")
            .then(|| Some(t.error.clone().unwrap_or_else(|| t.state.clone())));
        if error.is_some() {
            attrs.push(attr(OBSERVATION_LEVEL, any_str("ERROR")));
        }
        let name = format!("{} {}", t.key, text(&t.title, 200));
        events.push(Event {
            entity_id: span_id.clone(),
            launch_id: t.worker.clone(),
            kind: "swarm-task",
            json: span_json(
                &trace_id,
                &span_id,
                Some(&root_id),
                &name,
                start,
                task_end,
                attrs,
                error,
            ),
        });
    }

    for d in &r.decisions {
        let span_id = ids::span_id_hex(&format!("amx1|swarm|decision|{}|{}", s.id, d.id));
        let mut m = Map::new();
        m.insert("verb".into(), Value::from(d.verb.as_str()));
        m.insert("by".into(), Value::from(d.by.as_str()));
        put(&mut m, "said", d.said.clone());
        put(&mut m, "launch", d.launch.clone());
        put(&mut m, "task_id", d.task_id.clone());
        let mut attrs = common_attrs(ctx);
        attrs.push(attr(OBSERVATION_TYPE, any_str("event")));
        push_str_attr(&mut attrs, OBSERVATION_INPUT, d.said.clone());
        push_json_attr(&mut attrs, OBSERVATION_METADATA, m);
        let name = format!("decision {}", d.verb.as_str());
        events.push(Event {
            entity_id: span_id.clone(),
            launch_id: d.launch.clone(),
            kind: "swarm-decision",
            json: span_json(
                &trace_id,
                &span_id,
                Some(&root_id),
                &name,
                d.created_ns,
                d.created_ns,
                attrs,
                None,
            ),
        });
    }
    events
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
            swarms: HashMap::new(),
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

    fn swarm_run(id: &str) -> crate::bus::swarm::record::SwarmRun {
        use crate::bus::swarm::record::{GateMode, SwarmRun, SwarmStatus};
        SwarmRun {
            id: id.into(),
            workspace: "/ws".into(),
            goal: "Ship the follow-ups\nwith care".into(),
            status: SwarmStatus::Finished,
            planner_harness: "claude".into(),
            planner_launch: Some("L-coord".into()),
            plan_summary: Some("one task".into()),
            plan_error: None,
            max_workers: 2,
            isolation: crate::bus::TaskIsolation::Shared,
            gate: true,
            check_cmd: Some("cargo test".into()),
            check_result: Some("ok".into()),
            owner_run: None,
            started_ns: 1_000,
            updated_ns: 9_000,
            ended_ns: Some(9_000),
            gate_mode: GateMode::Chat,
            headless: false,
        }
    }

    #[test]
    fn a_swarms_turn_joins_the_swarms_langfuse_session_with_its_tags() {
        use crate::bus::swarm::SwarmLaunch;
        let mut ctx = ctx();
        let mut t = trace();
        t.launch_id = Some("L-w1".into());
        // not a swarm's launch: the harness session, as before
        let plain = parsed(&trace_span(&t, &ctx));
        let s = &plain;
        assert_eq!(attr_str(s, "session.id"), Some("sess-1"));

        let mut meta = Map::new();
        SwarmLaunch::worker("swarm-abc", "task-1", "schema", Some("L-coord".into()))
            .metadata(&mut meta);
        let mut launch = LaunchRow {
            id: "L-w1".into(),
            run_id: "run".into(),
            agent_mux_session: 1,
            profile: "default".into(),
            provider: "claude".into(),
            cwd: "/ws".into(),
            project_slug: "ws".into(),
            content_mode: "full".into(),
            correlation_plan: "known".into(),
            correlation: None,
            session_key: Some("claude:sess-1".into()),
            injected_session_id: true,
            attached: false,
            started_ns: 1,
            ended_ns: None,
            termination: None,
            exit_code: None,
            parse_errors: None,
            dropped_ops: None,
            reported_cost_usd: None,
            reported_lines_added: None,
            reported_lines_removed: None,
            agent_mux_version: "test".into(),
            user_id: None,
            release: None,
            environment: None,
            tags: Vec::new(),
            metadata: Some(Value::Object(meta)),
        };
        ctx.note_launch(&launch);
        let v = parsed(&trace_span(&t, &ctx));
        let s = &v;
        assert_eq!(attr_str(s, "session.id"), Some("swarm-abc"));
        let tags: Value =
            serde_json::from_str(attr_str(s, "langfuse.trace.tags").unwrap()).unwrap();
        assert_eq!(
            tags,
            json!(["agent-mux", "claude", "swarm", "swarm:worker"])
        );
        let md: Value =
            serde_json::from_str(attr_str(s, "langfuse.trace.metadata").unwrap()).unwrap();
        assert_eq!(md["swarm_id"], "swarm-abc");
        assert_eq!(md["swarm_role"], "worker");
        assert_eq!(md["swarm_task_key"], "schema");
        assert_eq!(md["parent_launch"], "L-coord");
        assert_eq!(md["harness_session"], "sess-1");
        // a launch of no swarm teaches nothing
        launch.id = "L-plain".into();
        launch.metadata = Some(json!({"loop_run_id": "x"}));
        ctx.note_launch(&launch);
        assert!(!ctx.swarms.contains_key("L-plain"));
    }

    #[test]
    fn the_swarm_summary_is_one_trace_with_a_span_per_task_and_an_event_per_decision() {
        use crate::bus::swarm::record::{DecidedBy, SwarmDecision, SwarmVerb};
        use crate::bus::swarm::view::TaskView;
        use crate::tracing::swarm_report::{LaunchReport, SwarmReport, TurnReport};
        let swarm = swarm_run("swarm-abc");
        let worker = LaunchReport {
            launch_id: "L-w1".into(),
            role: "worker".into(),
            task_id: Some("task-1".into()),
            task_key: Some("schema".into()),
            parent_launch: Some("L-coord".into()),
            provider: "codex".into(),
            session_key: Some("codex:w1".into()),
            started_ns: 3_000,
            ended_ns: Some(8_000),
            turns: vec![TurnReport {
                id: "T-w1".into(),
                ordinal: 1,
                name: "do it".into(),
                status: "closed".into(),
                start_ns: 3_000,
                end_ns: Some(7_000),
                tools: 4,
                errors: 0,
                tokens: Some(500),
                cost_usd: Some(0.02),
            }],
            tools: 4,
            errors: 0,
            tokens: Some(500),
            cost_usd: Some(0.02),
            unpriced: false,
            sub_agents: Vec::new(),
        };
        let task = |key: &str, id: &str, state: &str, worker: Option<&str>, error: Option<&str>| {
            TaskView {
                key: key.into(),
                id: id.into(),
                title: format!("Task {key}"),
                state: state.into(),
                status: if state == "done" { "done" } else { "failed" }.into(),
                harness: Some("codex".into()),
                agent: None,
                isolation: "shared".into(),
                depends_on: Vec::new(),
                acceptance: Some("tests pass".into()),
                files: vec!["src/a.rs".into()],
                worker: worker.map(str::to_string),
                attempts: 1,
                offered_ns: Some(2_500),
                launched_by: Some("coordinator".into()),
                result: (state == "done").then(|| json!("written")),
                error: error.map(str::to_string),
                branch: None,
                review: None,
                question: None,
                auto_retries: 0,
                held: false,
                merge_gate: None,
                merge_gate_detail: None,
                updated_ns: 8_000,
                ended_ns: Some(8_000),
            }
        };
        let report = SwarmReport {
            swarm: swarm.clone(),
            gate_mode: "chat".into(),
            coordinator: Some("L-coord".into()),
            done: 1,
            total: 2,
            tokens: Some(500),
            cost_usd: Some(0.02),
            unpriced: false,
            launches: vec![worker],
            tasks: vec![
                task("schema", "task-1", "done", Some("L-w1"), None),
                task("docs", "task-2", "failed", None, Some("no writer")),
            ],
            attention: Vec::new(),
            questions: Vec::new(),
            decisions: vec![SwarmDecision {
                id: 7,
                swarm_id: "swarm-abc".into(),
                task_id: None,
                verb: SwarmVerb::Launch,
                by: DecidedBy::Chat,
                said: Some("go".into()),
                launch: Some("L-coord".into()),
                created_ns: 2_000,
            }],
            events: Vec::new(),
            messages: Vec::new(),
        };
        let events = swarm_summary(&report, &ctx());
        assert_eq!(events.len(), 4, "root + 2 tasks + 1 decision");
        assert_eq!(events[0].kind, "swarm");
        assert_eq!(events[1].kind, "swarm-task");
        assert_eq!(events[3].kind, "swarm-decision");
        let root = parsed(&events[0]);
        let r = &root;
        assert_eq!(r["traceId"], swarm_trace_id("swarm-abc"));
        assert_eq!(r["name"], "swarm Ship the follow-ups");
        assert_eq!(attr_str(r, "session.id"), Some("swarm-abc"));
        assert_eq!(
            attr_str(r, "langfuse.trace.input"),
            Some("Ship the follow-ups\nwith care")
        );
        let tags: Value =
            serde_json::from_str(attr_str(r, "langfuse.trace.tags").unwrap()).unwrap();
        assert_eq!(
            tags,
            json!(["agent-mux", "swarm", "swarm:summary", "codex"])
        );
        let md: Value =
            serde_json::from_str(attr_str(r, "langfuse.trace.metadata").unwrap()).unwrap();
        assert_eq!(md["done"], 1);
        assert_eq!(md["total"], 2);
        assert_eq!(md["status"], "closed");
        assert_eq!(md["launches"], json!(["L-w1"]));
        // one failed task: the root reads as an error
        assert_eq!(r["status"]["code"], STATUS_ERROR);
        assert_eq!(attr_str(r, "langfuse.observation.level"), Some("ERROR"));

        let t1 = parsed(&events[1]);
        let t1 = &t1;
        assert_eq!(t1["traceId"], swarm_trace_id("swarm-abc"));
        assert_eq!(t1["parentSpanId"], r["spanId"]);
        assert_eq!(t1["name"], "schema Task schema");
        assert_eq!(t1["startTimeUnixNano"], "3000");
        assert_eq!(t1["endTimeUnixNano"], "8000");
        let md: Value =
            serde_json::from_str(attr_str(t1, "langfuse.observation.metadata").unwrap()).unwrap();
        assert_eq!(md["worker"], "L-w1");
        assert_eq!(md["worker_traces"], json!(["T-w1"]));
        assert_eq!(md["tokens"], 500);
        assert_eq!(md["state"], "done");
        assert_eq!(attr_str(t1, "langfuse.observation.output"), Some("written"));
        assert_eq!(t1["status"]["code"], STATUS_UNSET);

        let t2 = parsed(&events[2]);
        let t2 = &t2;
        assert_eq!(t2["status"]["code"], STATUS_ERROR);
        assert_eq!(t2["status"]["message"], "no writer");
        // a task with no worker starts at the swarm's start and ends when it did
        assert_eq!(t2["startTimeUnixNano"], "1000");
        assert_eq!(t2["endTimeUnixNano"], "8000");

        let d = parsed(&events[3]);
        let d = &d;
        assert_eq!(d["name"], "decision launch");
        assert_eq!(attr_str(d, "langfuse.observation.type"), Some("event"));
        assert_eq!(attr_str(d, "langfuse.observation.input"), Some("go"));
        let md: Value =
            serde_json::from_str(attr_str(d, "langfuse.observation.metadata").unwrap()).unwrap();
        assert_eq!(md["by"], "chat");
        // deterministic: sending it again updates rather than duplicates
        let again = swarm_summary(&report, &ctx());
        assert_eq!(
            again.iter().map(|e| &e.entity_id).collect::<Vec<_>>(),
            events.iter().map(|e| &e.entity_id).collect::<Vec<_>>()
        );
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
            json!({"input": 10, "output": 5, "total": 115, "cache_read_input_tokens": 100})
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
        child.level = Level::Error;
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
    fn provider_cost_wins_over_local_price_on_export() {
        let mut row = observation(ObservationType::Generation);
        row.metadata
            .insert("provided_cost".into(), json!({"total": 0.42, "input": 0.1}));
        let span = parsed(&event_for(&StoreOp::Observation(row), &ctx()).unwrap());
        let cost: Value = serde_json::from_str(
            attr_str(&span, "langfuse.observation.cost_details").expect("cost details"),
        )
        .unwrap();
        assert_eq!(cost, json!({"input": 0.1, "total": 0.42}));
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
