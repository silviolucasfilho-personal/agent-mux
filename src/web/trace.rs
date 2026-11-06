//! `/api/trace/`: the trace store through `TraceService`.
//!
//! `POST /api/trace/{tool}` with a JSON body of the tool's arguments, where
//! `tool` is one of `briefing`, `list_sessions`, `get_session`,
//! `get_timeline`, `search_traces`, `analyze_skills`, `compare_runs`,
//! `get_health`; the answer is the service envelope, the same JSON the MCP
//! server returns as `structuredContent`. An unknown tool is a 404 JSON
//! error; a service error keeps its `[CODE] message` text.

use super::{ApiError, ApiResult, WebState};
use crate::tracing::analysis::Request;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::post;
use serde_json::Value;
use std::sync::Arc;

/// The tools `/api/trace/{tool}` accepts, without their `agent_mux_` prefix.
pub const TOOLS: &[&str] = &[
    "briefing",
    "list_sessions",
    "get_session",
    "get_timeline",
    "search_traces",
    "analyze_skills",
    "compare_runs",
    "get_health",
];

/// The MCP tool name of a web tool name.
pub fn tool_name(tool: &str) -> Option<String> {
    if !TOOLS.contains(&tool) {
        return None;
    }
    Some(match tool {
        "briefing" => "agent_mux_get_briefing".to_string(),
        other => format!("agent_mux_{other}"),
    })
}

pub fn routes() -> Router<Arc<WebState>> {
    Router::new().route("/api/trace/{tool}", post(call))
}

/// The JSON body, if any; an empty body counts as `{}`.
async fn body_json(body: axum::body::Bytes) -> Result<Value, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
}

async fn call(
    State(state): State<Arc<WebState>>,
    Path(tool): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Value> {
    let name =
        tool_name(&tool).ok_or_else(|| ApiError::not_found(format!("unknown tool: {tool}")))?;
    let arguments = body_json(body).await?;
    let request = Request::from_tool_call(&name, arguments)?;
    let envelope = tokio::task::spawn_blocking(move || state.trace.execute(request))
        .await
        .map_err(|e| ApiError::internal(format!("trace query panicked: {e}")))??;
    let value = serde_json::to_value(&envelope).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(axum::Json(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_tool_names_map_to_mcp_tool_names() {
        assert_eq!(
            tool_name("briefing").as_deref(),
            Some("agent_mux_get_briefing")
        );
        assert_eq!(
            tool_name("get_health").as_deref(),
            Some("agent_mux_get_health")
        );
        assert_eq!(
            tool_name("list_sessions").as_deref(),
            Some("agent_mux_list_sessions")
        );
        assert_eq!(tool_name("delegate"), None);
        for t in TOOLS {
            let name = tool_name(t).unwrap();
            // every web tool is a service tool: the service may reject the
            // empty arguments, but never the name
            match Request::from_tool_call(&name, serde_json::json!({})) {
                Ok(_) => {}
                Err(e) => assert!(!e.to_string().contains("unknown tool"), "{t}: {e}"),
            }
        }
    }
}
