//! Every integration test in one binary: each test binary is linked and,
//! on macOS, scanned on its first launch, so one binary instead of forty
//! keeps a full run short. Each file is a module; fixtures stay in
//! tests/fixtures.

mod app_flow;
mod build_info;
mod config_library;
mod config_ui;
mod heimdall_dossier;
mod live_snapshots;
mod loop_cli;
mod loop_runs;
mod loop_skills;
mod loop_ui;
mod mcp_protocol;
mod persistent_sessions;
mod pty_session;
mod scroll_ux;
mod session_history;
mod session_tree_ui;
mod skill_hydrate;
mod skill_package;
mod skill_ui;
mod trace_analysis;
mod trace_analysis_benchmark;
mod trace_capture;
mod trace_changes;
mod trace_correlate;
mod trace_experiments;
mod trace_guard;
mod trace_hooks;
mod trace_inventory;
mod trace_langfuse;
mod trace_provider_matrix;
mod trace_scores;
mod trace_service;
mod trace_session;
mod trace_store;
mod trace_tail;
mod vt100_degenerate_grid;
mod workflow_cli;
mod workflow_runs;
mod workflow_ui;
mod zz_sub;
