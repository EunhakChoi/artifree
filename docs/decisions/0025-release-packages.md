# 0025: Release packages

Date: 2026-11-01. Status: accepted. Builds on
[0022](0022-production-web-and-api-serving.md) and
[0024](0024-https-proxy-and-remote-access.md). Roadmap row: O03a.

## Context

Running Artifree outside a checkout required `npm ci`, Rust, and the repository
layout (decision 0022). A personal server operator needs files to unpack and
run on Linux and Windows. The packaging step must not weaken the quality gates,
and "it installs" must be shown from the unpacked files, not from the checkout.

## Decision

1. **Platforms.** Two native targets: `linux-x64` (a `.tar.gz`, built on
   Ubuntu 24.04, dynamic glibc) and `windows-x64` (a `.zip`, MSVC, static C
   runtime through `-C target-feature=+crt-static`). Each is built on its own
   platform; the packager refuses a target that differs from the host, and
   refuses a binary that is not an x86-64 ELF (Linux) or PE (Windows) image or
   whose `--version` differs from the repository version. No cross-compiling,
   musl, ARM, macOS, installer, service unit, or signing. The glibc floor is
   whatever the build host produces, measured at about 2.34 and not enforced.
2. **Layout.** One top-level `artifree-<version>-<target>/` holding
   `bin/artifree-server[.exe]`, `web/` (the built workspace), `cli/artifree.mjs`,
   `MANIFEST.json`, `docs/release-packages.md`, `LICENSE`,
   `THIRD_PARTY_NOTICES.md`, and `RUST_DEPENDENCIES.json`. The server is started
   with an explicit `--web-dir`; no server code changed.
3. **The CLI ships as one bundled script that needs a system Node.js 22.12+.**
   esbuild (already a dev dependency) bundles the compiled CLI and its
   workspace packages into `cli/artifree.mjs`. Alternatives rejected: a Node
   single-executable application or embedded runtime would add a
   per-platform runtime download, a larger archive, and a new trust and update
   surface for a client whose only job is HTTP calls; leaving the CLI out
   would leave a package with no scripted way to create or save, though the
   web workspace and MCP endpoint would still work. The server and web
   workspace need no Node; only the CLI does, and the documentation says so.
4. **Manifest and checksum.** `MANIFEST.json` lists every other file with size
   and SHA-256; a `.sha256` file names the archive. Both detect corruption, not
   a malicious publisher: there is no signature. Archives are written by an
   in-repository deterministic tar/gzip and ZIP writer (fixed timestamps and
   owners, sorted paths), so the same inputs give the same bytes on the same
   Node/zlib build; this is tested for the host only.
5. **Dependency inventory, not license texts.** `RUST_DEPENDENCIES.json` lists
   the crates linked into the server for the target (from `cargo metadata
--locked`) with their declared license expressions. Full license texts of
   Rust crates and the npm packages in the web and CLI bundles are not
   included; this is a known compliance gap to close before any public
   release page, not a claim of completeness.
6. **Verification (CI).** The `package` job builds each archive; the `install`
   job downloads it on a fresh runner of the same OS, checks out only
   `scripts/` (Node built-ins only), installs nothing, unpacks with the
   platform extractor (`tar`, `Expand-Archive`), and runs `verify-release.mjs`
   (see `docs/release-packages.md`). The same function is exercised by
   `scripts/tests/release-package.test.mjs` in `npm run check`, including tamper
   detection. Windows has no graceful signal for a child process, so the check
   there ends the process and relies on the restart step to show the committed
   revision survived; exit status 0 on stop is asserted on Linux only.
7. **No release publication.** CI uploads archives as short-lived workflow
   artifacts. Tags, GitHub Releases, and signing are not part of this change.

## Dependencies and boundaries

No new npm or Rust dependency, no lint exception, and no boundary change.
`scripts/lib/server-process.mjs` gains optional `release`, `env`, and `cwd`
parameters. CI gains two jobs (`package`, `install`), both required through the
quality gate. The Windows release build uses an extra `RUSTFLAGS` value for the
package job only; the existing checks keep `-D warnings` unchanged.

## Not verified

Not run against a real tunnel, reverse proxy, other machines, older glibc
systems, clean Windows installs without the Visual C++ runtime, or Windows
Defender/SmartScreen. Nothing here enables unattended upgrades. Backup and restore are O04 ([0027](0027-backup-restore-and-schema-upgrade.md)).
Codex and Pi connection status is unchanged by this decision.
