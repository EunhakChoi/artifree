# Wire contracts

The authoritative DTOs are in `crates/artifree-contracts/src/lib.rs`.

`npm run generate:contracts` runs the Rust schema exporter and server descriptor,
validates the description using JSON Schema, then generates TypeScript. The
generated snapshot is imported by the web shell and inspection CLI. It is not a
health endpoint and performs no server discovery.

`npm run check:contracts` recomputes everything without writing and rejects drift.
The current contract describes the scaffold only. HTTP routes, MCP tools, runtime
messages, and storage operations must get behavior contracts when implemented.
