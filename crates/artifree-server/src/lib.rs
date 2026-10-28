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

use artifree_contracts::package::PackageRules;
use artifree_contracts::{Component, ServiceDescription};
use artifree_domain::package::{
    ENTRY_PATH, MAX_FILE_BYTES, MAX_FILES, MAX_PACKAGE_BYTES, MAX_PATH_BYTES, MAX_PATH_SEGMENTS,
    MAX_SEGMENT_BYTES, MediaType,
};

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

/// The rules a package must meet, as this build enforces them (decision
/// 0017).
#[must_use]
pub fn package_rules() -> PackageRules {
    let count = |value: usize| value as u64;
    PackageRules {
        entry_path: ENTRY_PATH.into(),
        max_files: count(MAX_FILES),
        max_file_bytes: count(MAX_FILE_BYTES),
        max_package_bytes: count(MAX_PACKAGE_BYTES),
        max_path_bytes: count(MAX_PATH_BYTES),
        max_path_segments: count(MAX_PATH_SEGMENTS),
        max_segment_bytes: count(MAX_SEGMENT_BYTES),
        media_types: MediaType::EXTENSIONS
            .iter()
            .map(|(extension, media_type)| ((*extension).into(), media_type.essence().into()))
            .collect(),
    }
}
