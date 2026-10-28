//! Application services. HTTP and MCP adapters must call the same commands.
//! This crate must remain independent of server frameworks and concrete providers.

pub mod agent_handoff;
pub mod artifacts;
pub mod documents;
pub mod jobs;
pub mod listing;
pub mod policy;
