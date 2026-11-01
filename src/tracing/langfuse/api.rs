//! Langfuse public REST API client.
//!
//! Provides data structures and functions to query traces, observations,
//! and sessions from a Langfuse deployment via its public REST API
//! (`/api/public/*`), supporting the Langfuse TUI viewer and web URL launching.

use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::basic_auth;
use crate::config::ResolvedLangfuse;

/// Query parameters for listing traces.
#[derive(Debug, Clone, Default)]
pub struct ListTracesParams {
    pub page: Option<u64>,
    pub limit: Option<u64>,
    pub session_id: Option<String>,
    pub name: Option<String>,
    pub user_id: Option<String>,
    pub from_timestamp: Option<String>,
    pub to_timestamp: Option<String>,
    pub tags: Vec<String>,
    pub order_by: Option<String>,
}

/// Metadata about pagination returned by `/api/public/traces`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
pub struct PaginationMeta {
    pub page: u64,
    pub limit: u64,
    #[serde(rename = "totalItems")]
    pub total_items: Option<u64>,
    #[serde(rename = "totalPages")]
    pub total_pages: Option<u64>,
}

/// Summary of a trace returned in `/api/public/traces` list.
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

/// Paginated trace list response envelope.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct PaginatedTracesResponse {
    pub data: Vec<LangfuseTraceSummary>,
    pub meta: PaginationMeta,
}

/// Full trace detail including observations from `GET /api/public/traces/{id}`.
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

/// Checks reachability and validity of credentials against Langfuse.
pub fn check_health(lf: &ResolvedLangfuse) -> Result<(), String> {
    let endpoint = format!("{}/api/public/health", lf.host);
    let agent = client(Duration::from_secs(5), Duration::from_secs(10));
    let resp = agent
        .get(&endpoint)
        .header("Authorization", &basic_auth(&lf.public_key, &lf.secret_key))
        .call()
        .map_err(|e| format!("health check request failed: {e}"))?;

    let status = resp.status().as_u16();
    if status == 200 {
        return Ok(());
    }

    // Some Langfuse deployments return 404 for /api/public/health; fallback to GET /api/public/traces?limit=1
    let traces_endpoint = format!("{}/api/public/traces?limit=1", lf.host);
    let mut trace_resp = agent
        .get(&traces_endpoint)
        .header("Authorization", &basic_auth(&lf.public_key, &lf.secret_key))
        .call()
        .map_err(|e| format!("trace check request failed: {e}"))?;

    let trace_status = trace_resp.status().as_u16();
    if (200..300).contains(&trace_status) {
        Ok(())
    } else if trace_status == 401 || trace_status == 403 {
        Err("authentication failed (401/403) — verify public and secret keys".into())
    } else {
        let body = trace_resp.body_mut().read_to_string().unwrap_or_default();
        Err(format!("Langfuse returned HTTP {trace_status}: {body}"))
    }
}

/// Fetches a paginated list of traces matching `params`.
pub fn list_traces(
    lf: &ResolvedLangfuse,
    params: &ListTracesParams,
) -> Result<PaginatedTracesResponse, String> {
    let mut url = format!("{}/api/public/traces?", lf.host);
    let mut query_parts = Vec::new();

    if let Some(p) = params.page {
        query_parts.push(format!("page={p}"));
    }
    if let Some(l) = params.limit {
        query_parts.push(format!("limit={l}"));
    }
    if let Some(s) = &params.session_id {
        query_parts.push(format!("sessionId={}", percent_encode(s)));
    }
    if let Some(n) = &params.name {
        query_parts.push(format!("name={}", percent_encode(n)));
    }
    if let Some(u) = &params.user_id {
        query_parts.push(format!("userId={}", percent_encode(u)));
    }
    if let Some(f) = &params.from_timestamp {
        query_parts.push(format!("fromTimestamp={}", percent_encode(f)));
    }
    if let Some(t) = &params.to_timestamp {
        query_parts.push(format!("toTimestamp={}", percent_encode(t)));
    }
    if let Some(o) = &params.order_by {
        query_parts.push(format!("orderBy={}", percent_encode(o)));
    }
    for tag in &params.tags {
        query_parts.push(format!("tags={}", percent_encode(tag)));
    }

    url.push_str(&query_parts.join("&"));

    let agent = client(Duration::from_secs(5), Duration::from_secs(15));
    let mut resp = agent
        .get(&url)
        .header("Authorization", &basic_auth(&lf.public_key, &lf.secret_key))
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("traces request failed: {e}"))?;

    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("reading response body failed: {e}"))?;

    if (200..300).contains(&status) {
        serde_json::from_str::<PaginatedTracesResponse>(&body)
            .map_err(|e| format!("parsing traces JSON failed: {e} (body: {body})"))
    } else {
        Err(format!("Langfuse returned HTTP {status}: {body}"))
    }
}

/// Fetches full details and observations of a single trace by ID.
pub fn get_trace(lf: &ResolvedLangfuse, trace_id: &str) -> Result<LangfuseTraceDetail, String> {
    let endpoint = format!("{}/api/public/traces/{}", lf.host, percent_encode(trace_id));
    let agent = client(Duration::from_secs(5), Duration::from_secs(15));
    let mut resp = agent
        .get(&endpoint)
        .header("Authorization", &basic_auth(&lf.public_key, &lf.secret_key))
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("trace detail request failed: {e}"))?;

    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("reading trace detail body failed: {e}"))?;

    if (200..300).contains(&status) {
        serde_json::from_str::<LangfuseTraceDetail>(&body)
            .map_err(|e| format!("parsing trace detail JSON failed: {e} (body: {body})"))
    } else {
        Err(format!("Langfuse returned HTTP {status}: {body}"))
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

    #[test]
    fn test_parse_traces_json() {
        let json = r#"{
            "data": [
                {
                    "id": "t-1",
                    "timestamp": "2026-11-01T10:00:00Z",
                    "name": "run test",
                    "sessionId": "claude:s1",
                    "projectId": "proj-99",
                    "tags": ["test"],
                    "latency": 2.5,
                    "totalCost": 0.015,
                    "observations": ["obs-1"],
                    "scores": [
                        { "id": "sc-1", "name": "verdict", "value": 1.0, "comment": "good" }
                    ]
                }
            ],
            "meta": {
                "page": 1,
                "limit": 50,
                "totalItems": 1,
                "totalPages": 1
            }
        }"#;

        let parsed: PaginatedTracesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.data.len(), 1);
        assert_eq!(parsed.data[0].id, "t-1");
        assert_eq!(parsed.data[0].session_id.as_deref(), Some("claude:s1"));
        assert_eq!(parsed.data[0].project_id.as_deref(), Some("proj-99"));
        assert_eq!(parsed.data[0].scores.len(), 1);
        assert_eq!(parsed.data[0].scores[0].value, Some(1.0));
        assert_eq!(parsed.meta.total_items, Some(1));
    }

    #[test]
    fn test_parse_trace_detail_json() {
        let json = r#"{
            "id": "t-1",
            "timestamp": "2026-11-01T10:00:00Z",
            "name": "run test",
            "sessionId": "claude:s1",
            "projectId": "proj-99",
            "input": { "task": "run" },
            "output": { "status": "ok" },
            "latency": 3.2,
            "totalCost": 0.02,
            "observations": [
                {
                    "id": "obs-1",
                    "traceId": "t-1",
                    "type": "GENERATION",
                    "name": "claude-3-5-sonnet",
                    "startTime": "2026-11-01T10:00:01Z",
                    "endTime": "2026-11-01T10:00:04Z",
                    "model": "claude-3-5-sonnet",
                    "usage": {
                        "promptTokens": 100,
                        "completionTokens": 200,
                        "totalTokens": 300
                    },
                    "calculatedTotalCost": 0.015
                }
            ]
        }"#;

        let parsed: LangfuseTraceDetail = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.id, "t-1");
        assert_eq!(parsed.observations.len(), 1);
        assert_eq!(parsed.observations[0].observation_type, "GENERATION");
        assert_eq!(
            parsed.observations[0].usage.as_ref().unwrap().total_tokens,
            Some(300)
        );
    }
}
