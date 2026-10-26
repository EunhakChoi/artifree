//! Composition root. HTTP/MCP adapters and their runtime will be chosen later.

pub mod http;
mod inventory;
pub mod mcp;

use artifree_contracts::{Component, ServiceDescription};

/// Actual scaffold inventory. It advertises no running API or configured provider.
#[must_use]
pub fn describe() -> ServiceDescription {
    ServiceDescription {
        name: "Artifree".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        contract_version: artifree_contracts::CONTRACT_VERSION.into(),
        phase: "scaffolding".into(),
        server_framework: None,
        async_runtime: None,
        http_listening: false,
        components: inventory::COMPONENTS
            .into_iter()
            .map(
                |(id, name, layer, language, path, status, summary)| Component {
                    id: id.into(),
                    name: name.into(),
                    layer,
                    language: language.into(),
                    path: path.into(),
                    status,
                    summary: summary.into(),
                },
            )
            .collect(),
    }
}
