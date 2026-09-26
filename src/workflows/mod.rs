//! Workflows: multi-agent workflows composed from a TOML document and
//! step skills, interpreted in Rust, with every session a headless traced
//! run of Claude Code, Codex CLI or Antigravity. The document model is
//! `document`, the pure state machine `interp`, result extraction
//! `result`; the App side (spawning, accounting) lives in
//! `crate::app::workflows`. Design: docs/superpowers/specs/2026-09-17-workflows-design.md.

pub mod builder;
pub mod cli;
pub mod context;
pub mod document;
pub mod harness;
pub mod interp;
pub mod journal;
pub mod library;
pub mod planner;
pub mod report;
pub mod result;
pub mod store;

pub use document::{Workflow, parse, validate};

/// The keys a workflow session carries on its launch row
/// (`launches.metadata.workflow_*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowLaunch {
    pub run_id: String,
    pub workflow: String,
    pub step: String,
    pub phase: String,
    /// The agent the session runs as (`launches.metadata.workflow_agent`).
    pub agent: Option<String>,
}
