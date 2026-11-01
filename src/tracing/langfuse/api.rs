//! Langfuse public REST API client.
//!
//! Reads traces back from a Langfuse deployment for the Langfuse screen and
//! `agent-mux langfuse traces`, and checks credentials for
//! `agent-mux langfuse status`.
//!
//! Endpoints (checked against the Cloud OpenAPI spec,
//! `https://cloud.langfuse.com/generated/api/openapi.yml`, on 2026-11-01):
//!
//! - `GET /api/public/projects` checks the key pair: it needs Basic auth and
//!   answers with the key's project. `GET /api/public/health` is not a
//!   credential check: it answers 200 to any caller.
//! - `GET /api/public/v2/observations` is the only read path for traces. The
//!   v1 `GET /api/public/traces` and `GET /api/public/traces/{id}` are gone
//!   from the spec and are removed from Langfuse Cloud on 2026-11-16. v2 has
//!   no trace objects: a trace is listed through its root observation
//!   (`isRootObservation=true`, with the `trace_context` field group for the
//!   trace's name and tags), and read as every observation with its
//!   `traceId`. Paging is by an opaque `meta.cursor`, newest `startTime`
//!   first; `input`/`output` arrive as raw strings; `usageDetails` is a map
//!   keyed `input`/`output`/`total`.
//!
//! Not available from v2 without more calls: a trace's total cost in the
//! list (the root observation's cost is not the trace's, so the list shows
//! none; the detail sums its observations) and trace scores.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::basic_auth;
use crate::config::ResolvedLangfuse;

/// How far back the trace list looks when no `from_timestamp` is given.
/// Langfuse asks for every v2 read to be bounded by time.
pub const DEFAULT_LOOKBACK_DAYS: i64 = 30;

/// The largest page `GET /api/public/v2/observations` serves.
const MAX_PAGE: u64 = 1000;

/// A trace with more pages of observations than this is shown truncated.
const MAX_DETAIL_PAGES: usize = 10;

/// Query parameters for listing traces. Results are newest first.
#[derive(Debug, Clone, Default)]
pub struct ListTracesParams {
    /// The `next_cursor` of the previous page; `None` for the first page.
    pub cursor: Option<String>,
    pub limit: Option<u64>,
    pub session_id: Option<String>,
    /// The trace's root observation name (normally the trace name).
    pub name: Option<String>,
    pub user_id: Option<String>,
    /// RFC 3339; defaults to [`DEFAULT_LOOKBACK_DAYS`] ago.
    pub from_timestamp: Option<String>,
    /// RFC 3339; defaults to now (unbounded).
    pub to_timestamp: Option<String>,
}

/// One page of traces.
#[derive(Debug, Clone, PartialEq)]
pub struct TracesPage {
    pub data: Vec<LangfuseTraceSummary>,
    /// Pass back as [`ListTracesParams::cursor`] for the next page; `None`
    /// when this is the last page.
    pub next_cursor: Option<String>,
}

/// The project a key pair belongs to, from `GET /api/public/projects`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInfo {
    pub id: String,
    pub name: Option<String>,
}

/// Summary of a trace in the trace list.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct LangfuseTraceSummary {
    pub id: String,
    pub timestamp: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "sessionId", default)]
    pub session_id: Option<String>,
    #[serde(rename = "userId", default)]
    pub user_id: Option<String>,
    #[serde(rename = "projectId", default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub public: Option<bool>,
    #[serde(default)]
    pub bookmarked: Option<bool>,
    #[serde(default)]
    pub latency: Option<f64>,
    #[serde(rename = "totalCost", default)]
    pub total_cost: Option<f64>,
    #[serde(default)]
    pub observations: Vec<String>,
    #[serde(default)]
    pub scores: Vec<LangfuseScoreSummary>,
}

/// Score / feedback attached to a trace or observation.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct LangfuseScoreSummary {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(rename = "stringValue", default)]
    pub string_value: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
}

/// A trace with all its observations, in start order.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct LangfuseTraceDetail {
    pub id: String,
    pub timestamp: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "sessionId", default)]
    pub session_id: Option<String>,
    #[serde(rename = "projectId", default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    #[serde(default)]
    pub output: Option<serde_json::Value>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub latency: Option<f64>,
    #[serde(rename = "totalCost", default)]
    pub total_cost: Option<f64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub observations: Vec<LangfuseObservation>,
    #[serde(default)]
    pub scores: Vec<LangfuseScoreSummary>,
}

/// An observation in a trace (generation, tool span, subagent span, event).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct LangfuseObservation {
    pub id: String,
    #[serde(rename = "traceId", default)]
    pub trace_id: Option<String>,
    #[serde(rename = "type", default = "default_obs_type")]
    pub observation_type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "startTime", default)]
    pub start_time: String,
    #[serde(rename = "endTime", default)]
    pub end_time: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(rename = "modelParameters", default)]
    pub model_parameters: Option<serde_json::Value>,
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    #[serde(default)]
    pub output: Option<serde_json::Value>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(rename = "parentObservationId", default)]
    pub parent_observation_id: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(rename = "statusMessage", default)]
    pub status_message: Option<String>,
    #[serde(default)]
    pub usage: Option<LangfuseUsage>,
    #[serde(rename = "calculatedTotalCost", default)]
    pub calculated_total_cost: Option<f64>,
    #[serde(default)]
    pub latency: Option<f64>,
}

fn default_obs_type() -> String {
    "SPAN".into()
}

/// Token usage details for a generation.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
pub struct LangfuseUsage {
    #[serde(rename = "promptTokens", default)]
    pub prompt_tokens: Option<u64>,
    #[serde(rename = "completionTokens", default)]
    pub completion_tokens: Option<u64>,
    #[serde(rename = "totalTokens", default)]
    pub total_tokens: Option<u64>,
}

fn client(connect: Duration, request: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(connect))
        .timeout_global(Some(request))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

/// Resolves the URL to open in a web browser for a Langfuse trace or session.
pub fn resolve_web_url(
    host: &str,
    project_id: Option<&str>,
    trace_id: Option<&str>,
    session_id: Option<&str>,
) -> String {
    let clean_host = host.trim().trim_end_matches('/');
    match (
        project_id.filter(|p| !p.trim().is_empty()),
        trace_id,
        session_id,
    ) {
        (Some(proj), Some(trace), _) => {
            format!("{clean_host}/project/{proj}/traces/{trace}")
        }
        (Some(proj), None, Some(session)) => {
            format!("{clean_host}/project/{proj}/sessions/{session}")
        }
        (Some(proj), None, None) => {
            format!("{clean_host}/project/{proj}")
        }
        (None, _, _) => clean_host.to_string(),
    }
}

/// Checks that Langfuse is reachable and accepts the key pair, and returns
/// the key's project. Calls `GET /api/public/projects`, which needs auth
/// (`/api/public/health` does not, so it cannot tell a bad key from a good one).
pub fn check_health(lf: &ResolvedLangfuse) -> Result<ProjectInfo, String> {
    let body = get_json(
        lf,
        &format!("{}/api/public/projects", lf.host),
        Duration::from_secs(10),
    )?;
    #[derive(Deserialize)]
    struct Projects {
        #[serde(default)]
        data: Vec<Project>,
    }
    #[derive(Deserialize)]
    struct Project {
        id: String,
        #[serde(default)]
        name: Option<String>,
    }
    let projects: Projects =
        serde_json::from_str(&body).map_err(|e| format!("parsing projects JSON failed: {e}"))?;
    projects
        .data
        .into_iter()
        .next()
        .map(|p| ProjectInfo {
            id: p.id,
            name: p.name,
        })
        .ok_or_else(|| "Langfuse accepted the keys but returned no project".into())
}

/// Fetches one page of traces matching `params`, newest first.
pub fn list_traces(lf: &ResolvedLangfuse, params: &ListTracesParams) -> Result<TracesPage, String> {
    let from = params
        .from_timestamp
        .clone()
        .unwrap_or_else(|| rfc3339_days_ago(DEFAULT_LOOKBACK_DAYS));
    let mut query = vec![
        ("fields", "core,basic,metrics,trace_context".to_string()),
        ("isRootObservation", "true".to_string()),
        ("fromStartTime", from),
        (
            "limit",
            params.limit.unwrap_or(50).clamp(1, MAX_PAGE).to_string(),
        ),
    ];
    let optional = [
        ("toStartTime", &params.to_timestamp),
        ("sessionId", &params.session_id),
        ("name", &params.name),
        ("userId", &params.user_id),
        ("cursor", &params.cursor),
    ];
    for (key, value) in optional {
        if let Some(v) = value {
            query.push((key, v.clone()));
        }
    }
    let resp = get_observations(lf, &query)?;
    Ok(TracesPage {
        data: traces_from_roots(resp.data),
        next_cursor: resp.meta.cursor.filter(|c| !c.is_empty()),
    })
}

/// Fetches a trace and all its observations.
pub fn get_trace(lf: &ResolvedLangfuse, trace_id: &str) -> Result<LangfuseTraceDetail, String> {
    let mut observations = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_DETAIL_PAGES {
        let mut query = vec![
            (
                "fields",
                "core,basic,time,io,metadata,model,usage,metrics,trace_context".to_string(),
            ),
            ("traceId", trace_id.to_string()),
            ("limit", MAX_PAGE.to_string()),
        ];
        if let Some(c) = &cursor {
            query.push(("cursor", c.clone()));
        }
        let resp = get_observations(lf, &query)?;
        observations.extend(resp.data);
        cursor = resp.meta.cursor.filter(|c| !c.is_empty());
        if cursor.is_none() {
            break;
        }
    }
    if observations.is_empty() {
        return Err(format!("trace {trace_id} not found (no observations)"));
    }
    Ok(trace_from_observations(trace_id, observations))
}

/// The v2 observation as served; only the requested field groups are present.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObservationV2 {
    id: String,
    #[serde(default)]
    trace_id: Option<String>,
    #[serde(default)]
    start_time: Option<String>,
    #[serde(default)]
    end_time: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    parent_observation_id: Option<String>,
    #[serde(rename = "type", default)]
    observation_type: Option<String>,
    #[serde(default)]
    is_root_observation: Option<bool>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    status_message: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    public: Option<bool>,
    #[serde(default)]
    bookmarked: Option<bool>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
    #[serde(default)]
    output: Option<serde_json::Value>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    model_parameters: Option<serde_json::Value>,
    #[serde(default)]
    usage_details: Option<HashMap<String, serde_json::Value>>,
    #[serde(default)]
    total_cost: Option<f64>,
    #[serde(default)]
    latency: Option<f64>,
    #[serde(default)]
    trace_name: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    release: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ObservationsV2Response {
    #[serde(default)]
    data: Vec<ObservationV2>,
    #[serde(default)]
    meta: CursorMeta,
}

#[derive(Debug, Default, Deserialize)]
struct CursorMeta {
    #[serde(default)]
    cursor: Option<String>,
}

fn get_observations(
    lf: &ResolvedLangfuse,
    query: &[(&str, String)],
) -> Result<ObservationsV2Response, String> {
    let qs: Vec<String> = query
        .iter()
        .map(|(k, v)| format!("{k}={}", percent_encode(v)))
        .collect();
    let url = format!("{}/api/public/v2/observations?{}", lf.host, qs.join("&"));
    let body = get_json(lf, &url, Duration::from_secs(20))?;
    serde_json::from_str(&body).map_err(|e| {
        format!(
            "parsing observations JSON failed: {e} (body: {})",
            truncate(&body, 300)
        )
    })
}

/// GETs `url` with the key pair and returns the body of a 2xx answer.
fn get_json(lf: &ResolvedLangfuse, url: &str, timeout: Duration) -> Result<String, String> {
    let mut resp = client(Duration::from_secs(5), timeout)
        .get(url)
        .header("Authorization", &basic_auth(&lf.public_key, &lf.secret_key))
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("cannot reach Langfuse at {}: {e}", lf.host))?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("reading Langfuse response failed: {e}"))?;
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(http_error(&lf.host, status, &body))
    }
}

/// A short error for a non-2xx answer: Langfuse's own `message` when the body
/// has one (never its `_deprecation` notice), else the start of the body.
fn http_error(host: &str, status: u16, body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
        .unwrap_or_else(|| truncate(body.trim(), 300));
    match status {
        401 | 403 => format!(
            "authentication failed (HTTP {status}): {message}\n  \
             Langfuse at {host} rejected the key pair. Check that the public and secret \
             keys were created together, in a project on this host."
        ),
        _ => format!("Langfuse returned HTTP {status}: {message}"),
    }
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn rfc3339_days_ago(days: i64) -> String {
    (OffsetDateTime::now_utc() - time::Duration::days(days))
        .format(&Rfc3339)
        .unwrap_or_default()
}

fn parse_ts(ts: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(ts, &Rfc3339).ok()
}

/// v2 serves `input`/`output` as raw strings: show JSON as JSON.
fn decode_io(value: Option<serde_json::Value>) -> Option<serde_json::Value> {
    match value? {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(
            serde_json::from_str::<serde_json::Value>(&s)
                .ok()
                .filter(|v| v.is_object() || v.is_array())
                .unwrap_or(serde_json::Value::String(s)),
        ),
        other => Some(other),
    }
}

fn usage_from_details(details: &HashMap<String, serde_json::Value>) -> Option<LangfuseUsage> {
    let get = |k: &str| details.get(k).and_then(|v| v.as_f64()).map(|n| n as u64);
    let prompt = get("input");
    let completion = get("output");
    let total = get("total").or(match (prompt, completion) {
        (None, None) => None,
        (p, c) => Some(p.unwrap_or(0) + c.unwrap_or(0)),
    });
    (prompt.is_some() || completion.is_some() || total.is_some()).then_some(LangfuseUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
    })
}

/// One trace per root observation, first (newest) root of a trace wins.
fn traces_from_roots(roots: Vec<ObservationV2>) -> Vec<LangfuseTraceSummary> {
    let mut seen = std::collections::HashSet::new();
    roots
        .into_iter()
        .filter_map(|o| {
            let id = o.trace_id.clone().unwrap_or_else(|| o.id.clone());
            seen.insert(id.clone()).then(|| LangfuseTraceSummary {
                id,
                timestamp: o.start_time.unwrap_or_default(),
                name: o.trace_name.or(o.name),
                session_id: o.session_id,
                user_id: o.user_id,
                project_id: o.project_id,
                release: o.release,
                version: o.version,
                tags: o.tags.unwrap_or_default(),
                public: o.public,
                bookmarked: o.bookmarked,
                latency: o.latency,
                total_cost: None,
                observations: Vec::new(),
                scores: Vec::new(),
            })
        })
        .collect()
}

fn trace_from_observations(trace_id: &str, mut all: Vec<ObservationV2>) -> LangfuseTraceDetail {
    all.sort_by(|a, b| a.start_time.cmp(&b.start_time));
    let root_idx = all
        .iter()
        .position(|o| o.is_root_observation == Some(true))
        .or_else(|| all.iter().position(|o| o.parent_observation_id.is_none()))
        .unwrap_or(0);
    let first = |f: fn(&ObservationV2) -> Option<String>| -> Option<String> {
        f(&all[root_idx]).or_else(|| all.iter().find_map(f))
    };
    let name = first(|o| o.trace_name.clone()).or_else(|| all[root_idx].name.clone());
    let session_id = first(|o| o.session_id.clone());
    let project_id = first(|o| o.project_id.clone());
    let tags = all
        .iter()
        .find_map(|o| o.tags.clone().filter(|t| !t.is_empty()))
        .unwrap_or_default();
    let timestamp = all[root_idx].start_time.clone().unwrap_or_default();

    let start = all
        .iter()
        .filter_map(|o| o.start_time.as_deref().and_then(parse_ts))
        .min();
    let end = all
        .iter()
        .filter_map(|o| o.end_time.as_deref().and_then(parse_ts))
        .max();
    let latency = match (start, end) {
        (Some(s), Some(e)) if e >= s => Some((e - s).as_seconds_f64()),
        _ => all[root_idx].latency,
    };
    let costs: Vec<f64> = all.iter().filter_map(|o| o.total_cost).collect();
    let total_cost = (!costs.is_empty()).then(|| costs.iter().sum());

    let mut input = None;
    let mut output = None;
    let mut metadata = None;
    let observations = all
        .into_iter()
        .enumerate()
        .map(|(i, o)| {
            let obs = LangfuseObservation {
                id: o.id,
                trace_id: o.trace_id,
                observation_type: o.observation_type.unwrap_or_else(default_obs_type),
                name: o.name,
                start_time: o.start_time.unwrap_or_default(),
                end_time: o.end_time,
                model: o.model,
                model_parameters: o.model_parameters,
                input: decode_io(o.input),
                output: decode_io(o.output),
                metadata: o.metadata,
                parent_observation_id: o.parent_observation_id,
                level: o.level,
                status_message: o.status_message,
                usage: o.usage_details.as_ref().and_then(usage_from_details),
                calculated_total_cost: o.total_cost,
                latency: o.latency,
            };
            if i == root_idx {
                input = obs.input.clone();
                output = obs.output.clone();
                metadata = obs.metadata.clone();
            }
            obs
        })
        .collect();

    LangfuseTraceDetail {
        id: trace_id.to_string(),
        timestamp,
        name,
        session_id,
        project_id,
        input,
        output,
        metadata,
        latency,
        total_cost,
        tags,
        observations,
        scores: Vec::new(),
    }
}

fn percent_encode(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", b));
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_web_url() {
        assert_eq!(
            resolve_web_url(
                "https://cloud.langfuse.com",
                Some("proj_123"),
                Some("trace_abc"),
                None
            ),
            "https://cloud.langfuse.com/project/proj_123/traces/trace_abc"
        );
        assert_eq!(
            resolve_web_url(
                "https://cloud.langfuse.com/",
                Some("proj_123"),
                None,
                Some("sess_xyz")
            ),
            "https://cloud.langfuse.com/project/proj_123/sessions/sess_xyz"
        );
        assert_eq!(
            resolve_web_url("http://localhost:3000", Some("proj_123"), None, None),
            "http://localhost:3000/project/proj_123"
        );
        assert_eq!(
            resolve_web_url("https://cloud.langfuse.com", None, Some("trace_abc"), None),
            "https://cloud.langfuse.com"
        );
    }

    fn parse(json: &str) -> ObservationsV2Response {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn roots_become_trace_summaries_one_per_trace() {
        let resp = parse(
            r#"{
            "data": [
                { "id": "o-1", "traceId": "t-1", "startTime": "2026-11-01T10:00:00Z",
                  "projectId": "proj-99", "type": "SPAN", "isRootObservation": true,
                  "name": "root span", "sessionId": "claude:s1", "latency": 2.5,
                  "traceName": "run test", "tags": ["test"], "totalCost": 0.5 },
                { "id": "o-2", "traceId": "t-1", "startTime": "2026-11-01T09:59:00Z",
                  "projectId": "proj-99", "type": "SPAN", "isRootObservation": true },
                { "id": "o-3", "traceId": "t-2", "startTime": "2026-11-01T09:00:00Z",
                  "projectId": "proj-99", "type": "SPAN", "name": "only name" }
            ],
            "meta": { "cursor": "abc==" }
        }"#,
        );
        assert_eq!(resp.meta.cursor.as_deref(), Some("abc=="));
        let traces = traces_from_roots(resp.data);
        assert_eq!(traces.len(), 2, "a trace's second root is dropped");
        assert_eq!(traces[0].id, "t-1");
        assert_eq!(traces[0].name.as_deref(), Some("run test"));
        assert_eq!(traces[0].session_id.as_deref(), Some("claude:s1"));
        assert_eq!(traces[0].project_id.as_deref(), Some("proj-99"));
        assert_eq!(traces[0].tags, vec!["test".to_string()]);
        assert_eq!(traces[0].latency, Some(2.5));
        assert_eq!(
            traces[0].total_cost, None,
            "a root's cost is not the trace's"
        );
        assert_eq!(traces[1].name.as_deref(), Some("only name"));
    }

    #[test]
    fn last_page_has_no_cursor() {
        let resp = parse(r#"{ "data": [], "meta": {} }"#);
        assert!(resp.meta.cursor.is_none());
    }

    #[test]
    fn observations_build_the_trace_detail() {
        let resp = parse(
            r#"{
            "data": [
                { "id": "gen", "traceId": "t-1", "startTime": "2026-11-01T10:00:01Z",
                  "endTime": "2026-11-01T10:00:04Z", "projectId": "p", "type": "GENERATION",
                  "parentObservationId": "root", "model": "claude-opus-5-5",
                  "usageDetails": { "input": 100, "output": 200 }, "totalCost": 0.015,
                  "input": "{\"messages\":[1]}", "output": "plain text" },
                { "id": "root", "traceId": "t-1", "startTime": "2026-11-01T10:00:00Z",
                  "endTime": "2026-11-01T10:00:05Z", "projectId": "p", "type": "SPAN",
                  "isRootObservation": true, "name": "turn", "traceName": "run test",
                  "sessionId": "claude:s1", "input": "{\"task\":\"run\"}", "totalCost": 0.005 }
            ],
            "meta": {}
        }"#,
        );
        let d = trace_from_observations("t-1", resp.data);
        assert_eq!(d.id, "t-1");
        assert_eq!(d.name.as_deref(), Some("run test"));
        assert_eq!(d.session_id.as_deref(), Some("claude:s1"));
        assert_eq!(d.timestamp, "2026-11-01T10:00:00Z");
        assert_eq!(d.latency, Some(5.0));
        assert!((d.total_cost.unwrap() - 0.02).abs() < 1e-9);
        assert_eq!(d.input, Some(serde_json::json!({ "task": "run" })));
        assert_eq!(d.observations.len(), 2);
        assert_eq!(d.observations[0].id, "root", "sorted by start time");
        let generation = &d.observations[1];
        assert_eq!(generation.observation_type, "GENERATION");
        assert_eq!(generation.parent_observation_id.as_deref(), Some("root"));
        let usage = generation.usage.as_ref().unwrap();
        assert_eq!(usage.prompt_tokens, Some(100));
        assert_eq!(usage.completion_tokens, Some(200));
        assert_eq!(usage.total_tokens, Some(300));
        assert_eq!(generation.calculated_total_cost, Some(0.015));
        assert_eq!(
            generation.input,
            Some(serde_json::json!({ "messages": [1] }))
        );
        assert_eq!(generation.output, Some(serde_json::json!("plain text")));
    }

    #[test]
    fn errors_show_langfuse_message_not_deprecation_notice() {
        let body = r#"{"_deprecation":{"message":"Removed on 2026-11-16."},"message":"Invalid credentials.","error":"UnauthorizedError"}"#;
        let e = http_error("https://us.cloud.langfuse.com", 401, body);
        assert!(e.starts_with("authentication failed (HTTP 401): Invalid credentials."));
        assert!(e.contains("https://us.cloud.langfuse.com"));
        assert!(!e.contains("2026-11-16"));
        assert_eq!(
            http_error("h", 500, "oops"),
            "Langfuse returned HTTP 500: oops"
        );
    }
}
