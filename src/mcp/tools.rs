//! Tool names and catalog enumeration for agent-mux MCP.

/// Enumerates the exact eight read-only trace analysis tools supported by agent-mux.
pub fn tool_names() -> Vec<&'static str> {
    vec![
        "agent_mux_get_briefing",
        "agent_mux_list_sessions",
        "agent_mux_get_session",
        "agent_mux_get_timeline",
        "agent_mux_search_traces",
        "agent_mux_analyze_skills",
        "agent_mux_compare_runs",
        "agent_mux_get_health",
    ]
}
