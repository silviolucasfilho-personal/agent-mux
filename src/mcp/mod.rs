//! Model Context Protocol (MCP) server exposing agent-mux trace analytics.

pub mod schema;
pub mod server;
pub mod tools;

pub use server::run;
pub use tools::tool_names;
