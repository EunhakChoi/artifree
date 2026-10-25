/* Generated build-time inventory; this is not live server health. */
import type { ServiceDescription } from './schema.js';

export const scaffoldDescription: ServiceDescription = {
  "name": "Artifree",
  "version": "0.1.0",
  "contract_version": "0.1.0",
  "phase": "scaffolding",
  "server_framework": null,
  "async_runtime": null,
  "http_listening": false,
  "components": [
    {
      "id": "web",
      "name": "Web workspace",
      "layer": "presentation",
      "language": "TypeScript",
      "path": "apps/web",
      "status": "scaffold",
      "summary": "React/Vite development shell; no backend connection."
    },
    {
      "id": "cli",
      "name": "CLI",
      "layer": "presentation",
      "language": "TypeScript",
      "path": "apps/cli",
      "status": "scaffold",
      "summary": "Help, version, and build-time scaffold inventory."
    },
    {
      "id": "runtime",
      "name": "Content runtime SDK",
      "layer": "presentation",
      "language": "TypeScript",
      "path": "packages/runtime-sdk",
      "status": "scaffold",
      "summary": "Session and capability types; no iframe execution bridge."
    },
    {
      "id": "api",
      "name": "Application API",
      "layer": "logic",
      "language": "Rust",
      "path": "crates/artifree-server",
      "status": "scaffold",
      "summary": "Composition root and descriptor binary; HTTP/MCP are not implemented."
    },
    {
      "id": "domain",
      "name": "Domain vocabulary",
      "layer": "logic",
      "language": "Rust",
      "path": "crates/artifree-domain",
      "status": "scaffold",
      "summary": "Six content kinds and separate source/document/data counters."
    },
    {
      "id": "artifacts",
      "name": "Artifact core",
      "layer": "logic",
      "language": "Rust",
      "path": "crates/artifree-core/src/artifacts.rs",
      "status": "planned",
      "summary": "Drafts, versioned files, conditional saves, publish, restore, and fork."
    },
    {
      "id": "policy",
      "name": "Policy & sessions",
      "layer": "logic",
      "language": "Rust",
      "path": "crates/artifree-core/src/policy.rs",
      "status": "planned",
      "summary": "Actors, access rules, scoped sessions, and capability checks."
    },
    {
      "id": "documents",
      "name": "Data & documents",
      "layer": "logic",
      "language": "Rust",
      "path": "crates/artifree-core/src/documents.rs",
      "status": "planned",
      "summary": "Structured edits and separately versioned application data."
    },
    {
      "id": "jobs",
      "name": "Sync & jobs",
      "layer": "logic",
      "language": "Rust",
      "path": "crates/artifree-core/src/jobs.rs",
      "status": "planned",
      "summary": "Commit events, retries, worker lifecycle, and reconnect."
    },
    {
      "id": "handoff",
      "name": "Agent handoff",
      "layer": "logic",
      "language": "Rust",
      "path": "crates/artifree-core/src/agent_handoff.rs",
      "status": "planned",
      "summary": "Selection and edit intent delivered to external agent sessions."
    },
    {
      "id": "storage",
      "name": "Storage",
      "layer": "provider",
      "language": "Rust adapters; backend open",
      "path": "crates/artifree-providers/src/storage.rs",
      "status": "planned",
      "summary": "Local SQLite/files and server PostgreSQL/objects are candidates."
    },
    {
      "id": "models",
      "name": "Models",
      "layer": "provider",
      "language": "Rust adapters; worker language open",
      "path": "crates/artifree-providers/src/models.rs",
      "status": "planned",
      "summary": "Optional providers for artifact AI capabilities."
    },
    {
      "id": "tools",
      "name": "Tools & files",
      "layer": "provider",
      "language": "Rust adapters; worker language open",
      "path": "crates/artifree-providers/src/tools.rs",
      "status": "planned",
      "summary": "Outbound MCP and explicitly allowed project files."
    },
    {
      "id": "build",
      "name": "Build workers",
      "layer": "provider",
      "language": "Worker language open",
      "path": "crates/artifree-providers/src/build.rs",
      "status": "planned",
      "summary": "Isolated HTML/JS/TSX build and preview generation."
    },
    {
      "id": "export",
      "name": "Export workers",
      "layer": "provider",
      "language": "Worker language open",
      "path": "crates/artifree-providers/src/export.rs",
      "status": "planned",
      "summary": "Document, image, slide, and video conversion."
    },
    {
      "id": "contracts",
      "name": "Shared contracts",
      "layer": "shared",
      "language": "Rust → JSON Schema → TypeScript",
      "path": "crates/artifree-contracts",
      "status": "scaffold",
      "summary": "Generated wire types and validated build-time inventory."
    },
    {
      "id": "client",
      "name": "Client contract",
      "layer": "shared",
      "language": "TypeScript",
      "path": "packages/client",
      "status": "scaffold",
      "summary": "Connection configuration types; no HTTP methods yet."
    }
  ]
};
