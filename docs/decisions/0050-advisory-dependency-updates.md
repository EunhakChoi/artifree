# 0050: Raise the minimum Rust version to 1.89 for two security advisories

Date: 2026-11-09. Status: **accepted** (implemented by the change that adds
this record). Amends item 8 of [0004](0004-inbound-mcp-adapter.md): the
minimum supported Rust version is now 1.89, not 1.88.

## Context

The `cargo-deny` job began to fail on `main` after two advisories were
published for crates in `Cargo.lock`:

- RUSTSEC-2026-0459: SQLite before 3.54.0 can return uninitialized memory when
  a database is opened together with a `-wal` file whose page size differs.
  `libsqlite3-sys` 0.38.2 (the `bundled` feature of `rusqlite` 0.40) is
  affected; the fix is `libsqlite3-sys` 0.39 with `rusqlite` 0.41.
- RUSTSEC-2026-0462: `rmcp` 3.8.0 buffers an unbounded response body in its
  Streamable HTTP client. Artifree compiles only the server features; the fix
  is `rmcp` 3.9.

## Decision

1. `rusqlite` moves to 0.41 (bundled SQLite 3.54) and `rmcp` to 3.9. The
   lockfile records the resolved versions.
2. `rusqlite` 0.41 declares Rust 1.89 as its minimum, so the workspace
   `rust-version`, the minimum-version CI job, `CONTRIBUTING.md` and
   `docs/quality.md` move from 1.88 to 1.89. The pinned toolchain
   (`rust-toolchain.toml`) is unchanged.
3. No advisory is ignored in `deny.toml`. A blanket or time-limited exception
   was rejected because the repository's policy forbids silencing a finding
   to make a check pass and an upgrade is available.
4. Exposure before the change, for the record only: the database is the
   server's own store, and restore refuses input with stale `-wal`, `-shm` or
   `-journal` files beside it. A crafted database and WAL pair therefore needs
   write access to the data directory. The `rmcp` client code is not compiled
   with the features Artifree enables.

Decision 0004's record of the 1.85 to 1.88 step is a past change and is not
edited.
