//! Application services. HTTP and MCP adapters must call the same commands.
//! This crate must remain independent of server frameworks and concrete providers.

pub mod agent_handoff;
pub mod artifacts;
pub mod data;
pub mod documents;
pub mod jobs;
pub mod listing;
pub mod parts;
pub mod policy;
pub mod worker;
