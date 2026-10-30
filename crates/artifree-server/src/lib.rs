//! Composition root: wires the application core to `SQLite` storage, the
//! Axum HTTP adapters, and the MCP adapter.

pub mod auth;
pub mod changes;
pub mod http;
mod inventory;
pub mod limits;
pub mod mcp;
mod operations;
pub mod preview;
mod ranges;
pub mod serve;

#[cfg(test)]
mod runtime_tests;

use std::collections::BTreeMap;

use artifree_contracts::{Component, PackageRules, ServiceDescription};
use artifree_domain::package::{self, MediaType};

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
        mcp_api: true,
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
        package_rules: package_rules(),
    }
}

/// The domain's package rules, as a client can check them.
fn package_rules() -> PackageRules {
    let mut media_types = BTreeMap::new();
    for media_type in MediaType::ALL {
        for extension in media_type.extensions() {
            media_types.insert((*extension).to_owned(), media_type.essence().to_owned());
        }
    }
    // Every target this builds for has pointers of at most 64 bits.
    let wide = |limit: usize| limit as u64;
    PackageRules {
        entry_path: package::ENTRY_PATH.into(),
        max_files: wide(package::MAX_FILES),
        max_file_bytes: wide(package::MAX_FILE_BYTES),
        max_package_bytes: wide(package::MAX_PACKAGE_BYTES),
        max_path_bytes: wide(package::MAX_PATH_BYTES),
        max_path_segments: wide(package::MAX_PATH_SEGMENTS),
        max_segment_bytes: wide(package::MAX_SEGMENT_BYTES),
        media_types,
    }
}
