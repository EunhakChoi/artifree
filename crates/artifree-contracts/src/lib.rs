//! Versioned wire contracts. Generate JSON Schema and TypeScript from these DTOs.
//! Domain models remain independent; transport DTOs do not grant authorization.

pub mod api;
pub mod mcp;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Wire schema version, independent from a future API transport version.
pub const CONTRACT_VERSION: &str = "0.5.0";

/// How much of a module's responsibility exists in this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    /// Some product behavior works; the summary states its scope and limits.
    Implemented,
    /// Source scaffolding exists; the summary states its actual behavior.
    Scaffold,
    /// The module has no implemented product behavior.
    Planned,
}

/// Architectural ownership of a component, not its deployment location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// User and agent facing clients.
    Presentation,
    /// Application policy and domain logic.
    Logic,
    /// Concrete outbound integrations and workers.
    Provider,
    /// Contracts consumed across layers.
    Shared,
}

/// A repository component and an honest description of its implementation stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Component {
    /// Stable inventory key, unique within the descriptor.
    pub id: String,
    /// Human-readable module label.
    pub name: String,
    /// Layer that owns the module's responsibility.
    pub layer: Layer,
    /// Implemented language or an explicitly open integration choice.
    pub language: String,
    /// Repository-relative path to the source or module boundary.
    pub path: String,
    /// Implementation stage; this is not runtime health.
    pub status: ComponentStatus,
    /// Actual scaffold behavior or the proposed responsibility.
    pub summary: String,
}

/// A build-time module inventory, not a live server health response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceDescription {
    /// Display name of the application.
    pub name: String,
    /// Version of the compiled descriptor binary.
    pub version: String,
    /// Version of this wire contract.
    pub contract_version: String,
    /// Product implementation stage.
    pub phase: String,
    /// Selected HTTP framework, or null while undecided.
    pub server_framework: Option<String>,
    /// Selected async runtime, or null while undecided.
    pub async_runtime: Option<String>,
    /// Whether this build can serve the HTTP API (`artifree-server serve`).
    /// The inventory itself is static and does not probe a running server.
    pub http_api: bool,
    /// Whether `artifree-server serve` also answers MCP over Streamable HTTP
    /// at `/mcp` on the API listener.
    pub mcp_api: bool,
    /// Modules included in this build-time inventory.
    pub components: Vec<Component>,
}

/// The package rules this build enforces, for clients that check a package
/// before sending it. The server checks every request again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageRules {
    /// The file a preview opens; every package has it.
    pub entry_path: String,
    /// Most files in one package.
    pub max_files: u64,
    /// Largest file, in bytes.
    pub max_file_bytes: u64,
    /// Largest package, all files together, in bytes.
    pub max_package_bytes: u64,
    /// Longest path, in UTF-8 bytes.
    pub max_path_bytes: u64,
    /// Most `/`-separated segments in a path.
    pub max_path_segments: u64,
    /// Longest segment, in bytes.
    pub max_segment_bytes: u64,
    /// Each lowercase file extension a path may end with, and the
    /// `Content-Type` its file is served with. Extensions match without
    /// regard to ASCII case.
    pub media_types: BTreeMap<String, String>,
}
