//! `agent-mux web`: the read-only web explorer. `support` starts a server
//! on a free port over a temporary store; each area has its own file.

pub mod support;

mod core;
mod library;
mod runs;
mod session;
mod swarm;
mod tools;
mod usage;
