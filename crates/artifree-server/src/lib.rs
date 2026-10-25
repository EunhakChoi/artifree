//! Composition root. HTTP/MCP adapters and their runtime will be chosen later.

pub mod http;
pub mod mcp;

use artifree_contracts::{Component, ComponentStatus, Layer, ServiceDescription};

/// Actual scaffold inventory. It advertises no running API or configured provider.
#[must_use]
pub fn describe() -> ServiceDescription {
    let rows = [
        (
            "web",
            "Web workspace",
            Layer::Presentation,
            "TypeScript",
            "apps/web",
            ComponentStatus::Scaffold,
            "React/Vite development shell; no backend connection.",
        ),
        (
            "cli",
            "CLI",
            Layer::Presentation,
            "TypeScript",
            "apps/cli",
            ComponentStatus::Scaffold,
            "Help, version, and build-time scaffold inventory.",
        ),
        (
            "runtime",
            "Content runtime SDK",
            Layer::Presentation,
            "TypeScript",
            "packages/runtime-sdk",
            ComponentStatus::Scaffold,
            "Session and capability types; no iframe execution bridge.",
        ),
        (
            "api",
            "Application API",
            Layer::Logic,
            "Rust",
            "crates/artifree-server",
            ComponentStatus::Scaffold,
            "Composition root and descriptor binary; HTTP/MCP are not implemented.",
        ),
        (
            "domain",
            "Domain vocabulary",
            Layer::Logic,
            "Rust",
            "crates/artifree-domain",
            ComponentStatus::Scaffold,
            "Six content kinds and separate source/document/data counters.",
        ),
        (
            "artifacts",
            "Artifact core",
            Layer::Logic,
            "Rust",
            "crates/artifree-core/src/artifacts.rs",
            ComponentStatus::Planned,
            "Drafts, versioned files, conditional saves, publish, restore, and fork.",
        ),
        (
            "policy",
            "Policy & sessions",
            Layer::Logic,
            "Rust",
            "crates/artifree-core/src/policy.rs",
            ComponentStatus::Planned,
            "Actors, access rules, scoped sessions, and capability checks.",
        ),
        (
            "documents",
            "Data & documents",
            Layer::Logic,
            "Rust",
            "crates/artifree-core/src/documents.rs",
            ComponentStatus::Planned,
            "Structured edits and separately versioned application data.",
        ),
        (
            "jobs",
            "Sync & jobs",
            Layer::Logic,
            "Rust",
            "crates/artifree-core/src/jobs.rs",
            ComponentStatus::Planned,
            "Commit events, retries, worker lifecycle, and reconnect.",
        ),
        (
            "handoff",
            "Agent handoff",
            Layer::Logic,
            "Rust",
            "crates/artifree-core/src/agent_handoff.rs",
            ComponentStatus::Planned,
            "Selection and edit intent delivered to external agent sessions.",
        ),
        (
            "storage",
            "Storage",
            Layer::Provider,
            "Rust adapters; backend open",
            "crates/artifree-providers/src/storage.rs",
            ComponentStatus::Planned,
            "Local SQLite/files and server PostgreSQL/objects are candidates.",
        ),
        (
            "models",
            "Models",
            Layer::Provider,
            "Rust adapters; worker language open",
            "crates/artifree-providers/src/models.rs",
            ComponentStatus::Planned,
            "Optional providers for artifact AI capabilities.",
        ),
        (
            "tools",
            "Tools & files",
            Layer::Provider,
            "Rust adapters; worker language open",
            "crates/artifree-providers/src/tools.rs",
            ComponentStatus::Planned,
            "Outbound MCP and explicitly allowed project files.",
        ),
        (
            "build",
            "Build workers",
            Layer::Provider,
            "Worker language open",
            "crates/artifree-providers/src/build.rs",
            ComponentStatus::Planned,
            "Isolated HTML/JS/TSX build and preview generation.",
        ),
        (
            "export",
            "Export workers",
            Layer::Provider,
            "Worker language open",
            "crates/artifree-providers/src/export.rs",
            ComponentStatus::Planned,
            "Document, image, slide, and video conversion.",
        ),
        (
            "contracts",
            "Shared contracts",
            Layer::Shared,
            "Rust → JSON Schema → TypeScript",
            "crates/artifree-contracts",
            ComponentStatus::Scaffold,
            "Generated wire types and validated build-time inventory.",
        ),
        (
            "client",
            "Client contract",
            Layer::Shared,
            "TypeScript",
            "packages/client",
            ComponentStatus::Scaffold,
            "Connection configuration types; no HTTP methods yet.",
        ),
    ];
    ServiceDescription {
        name: "Artifree".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        contract_version: artifree_contracts::CONTRACT_VERSION.into(),
        phase: "scaffolding".into(),
        server_framework: None,
        async_runtime: None,
        http_listening: false,
        components: rows
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
