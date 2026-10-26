//! Composition root: wires the application core to `SQLite` storage and the
//! Axum HTTP adapters. The MCP adapter is planned.

pub mod auth;
pub mod http;
mod inventory;
pub mod mcp;
pub mod preview;
pub mod serve;

use artifree_contracts::{Component, ServiceDescription};

/// The build-time module inventory. It does not probe a running server.
#[must_use]
pub fn describe() -> ServiceDescription {
    ServiceDescription {
        name: "Artifree".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        contract_version: artifree_contracts::CONTRACT_VERSION.into(),
        phase: "early-development".into(),
        server_framework: Some("axum".into()),
        async_runtime: Some("tokio".into()),
        http_api: true,
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
