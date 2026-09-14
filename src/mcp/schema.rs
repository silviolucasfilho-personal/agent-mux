//! Schemas and tool definitions for agent-mux MCP tools.

use crate::tracing::analysis::service::{
    AnalyzeSkillsArgs, BriefingArgs, CompareRunsArgs, GetSessionArgs, HealthArgs, ListSessionsArgs,
    SearchArgs, TimelineArgs,
};
use schemars::schema_for;
use serde_json::{Value, json};

/// Generates the standard MCP tool definitions for all eight tools.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "agent_mux_get_briefing",
            "description": "Session cards, exact scope/window totals, prioritized evidence-based findings",
            "inputSchema": schema_for!(BriefingArgs),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }),
        json!({
            "name": "agent_mux_list_sessions",
            "description": "Session identities, launch identities, states, correlation quality, usage coverage",
            "inputSchema": schema_for!(ListSessionsArgs),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }),
        json!({
            "name": "agent_mux_get_session",
            "description": "Detailed recap, source references, active tools, coverage and correlation warnings",
            "inputSchema": schema_for!(GetSessionArgs),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }),
        json!({
            "name": "agent_mux_get_timeline",
            "description": "Ordered turns and observations with IDs, nesting, duration and error classification",
            "inputSchema": schema_for!(TimelineArgs),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }),
        json!({
            "name": "agent_mux_search_traces",
            "description": "Scoped FTS matches with bounded excerpts and source IDs",
            "inputSchema": schema_for!(SearchArgs),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }),
        json!({
            "name": "agent_mux_analyze_skills",
            "description": "Attribution, latency, error and usage metrics with sample sizes and limitations",
            "inputSchema": schema_for!(AnalyzeSkillsArgs),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }),
        json!({
            "name": "agent_mux_compare_runs",
            "description": "Metric deltas, tool-path differences, task/model/config comparability warnings",
            "inputSchema": schema_for!(CompareRunsArgs),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }),
        json!({
            "name": "agent_mux_get_health",
            "description": "Reader schema compatibility, collector freshness, content mode, provider coverage, available features",
            "inputSchema": schema_for!(HealthArgs),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }),
    ]
}
