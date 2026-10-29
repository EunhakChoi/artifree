# Wire contracts

The authoritative DTOs are in `crates/artifree-contracts/src/lib.rs`.

`npm run generate:contracts` runs the Rust schema exporter and server descriptor,
validates the description using JSON Schema, then generates TypeScript. The
generated snapshot is imported by the web shell and inspection CLI. It is not a
health endpoint and performs no server discovery.

The description also carries the package rules the build enforces
(`package_rules`), so clients check paths and sizes with the domain's own
limits. `fixtures/package-paths.json` lists paths that the domain and the web's
path check must judge alike; tests on both sides read it.

`npm run check:contracts` recomputes everything without writing and rejects drift.
The current contract describes the scaffold only. HTTP routes, MCP tools, runtime
messages, and storage operations must get behavior contracts when implemented.
