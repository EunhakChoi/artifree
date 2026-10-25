//! Versioned wire contracts. Generate JSON Schema and TypeScript from these DTOs.
//! Domain models remain independent; transport DTOs do not grant authorization.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: &str = "0.1.0";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    Scaffold,
    Planned,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Presentation,
    Logic,
    Provider,
    Shared,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub id: String,
    pub name: String,
    pub layer: Layer,
    pub language: String,
    pub path: String,
    pub status: ComponentStatus,
    pub summary: String,
}

/// A build-time scaffold inventory, not a live server health response.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceDescription {
    pub name: String,
    pub version: String,
    pub contract_version: String,
    pub phase: String,
    pub server_framework: Option<String>,
    pub async_runtime: Option<String>,
    pub http_listening: bool,
    pub components: Vec<Component>,
}
