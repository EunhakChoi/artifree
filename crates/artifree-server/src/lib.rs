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
    }
}

/// The package rules of the domain this build links, as clients check them
/// before sending (decision 0017).
#[must_use]
pub fn package_rules() -> PackageRules {
    // Each limit is far below `u64::MAX`; `usize` is at most 64 bits.
    let wide = |limit: usize| limit as u64;
    PackageRules {
        entry_path: package::ENTRY_PATH.into(),
        max_files: wide(package::MAX_FILES),
        max_file_bytes: wide(package::MAX_FILE_BYTES),
        max_package_bytes: wide(package::MAX_PACKAGE_BYTES),
        max_path_bytes: wide(package::MAX_PATH_BYTES),
        max_path_segments: wide(package::MAX_PATH_SEGMENTS),
        max_segment_bytes: wide(package::MAX_SEGMENT_BYTES),
        media_types: MediaType::EXTENSIONS
            .iter()
            .map(|(extension, media_type)| ((*extension).into(), media_type.essence().into()))
            .collect(),
    }
}
