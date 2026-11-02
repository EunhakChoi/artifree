# 0026: Container image and remote access guides

Date: 2026-11-01. Status: accepted. Builds on
[0022](0022-production-web-and-api-serving.md),
[0023](0023-single-owner-token-management.md),
[0024](0024-https-proxy-and-remote-access.md), and
[0025](0025-release-packages.md). Roadmap row: O03b. Independent Artifree design:
the private evidence index lists no original-product constraint on deployment
packaging for this row, so the choices below are Artifree's own.

## Context

O03a produced a verified Linux x64 package. Some operators prefer a container,
and every operator needs written instructions for reaching the server from
elsewhere. The roadmap row also bundled archive signing, publishing, and
license texts. Those are release-policy decisions with a different review
audience, so this change takes the container image and the guides only; the rest
moves to O03c. The web-workspace gaps found while writing the guides are O03d.

## Decision

1. **The image is built from the O03a package, not from the repository.** The
   Docker build context is an unpacked, manifest-verified `linux-x64` package;
   the Dockerfile only copies it to `/app`. There is no builder stage, no
   `RUN` that downloads or installs anything, and no second way to compile the
   server. The packager and manifest stay the single definition of what ships.
2. **Base image: `debian:bookworm-slim`, pinned by multi-arch index digest.** Its
   glibc 2.36 is newer than the 2.34 the binary was measured to need, and the
   `libgcc` runtime the Rust binary links is part of the base. Debian was
   chosen over `ubuntu:24.04` (the build host's glibc, so it would not show a
   floor) and over distroless or Alpine (distroless has no shell for the
   health check; musl is not the binary's target). The digest is refreshed by
   hand; no automation tracks it. If the binary ever needs a newer glibc, CI
   fails at start-up of the container, which is the useful signal.
   Whether the binary really runs on glibc 2.36 is shown only by the CI job.
3. **Runtime shape.** Fixed numeric user `10001:10001`; `/data` created with that
   owner and mode 700 before `VOLUME`, so a new named volume satisfies the
   server's data-directory check; exposed ports 7410 (API and web workspace)
   and 7411 (preview). The entrypoint is the exec form of
   `artifree-server serve` with the web, data, and listen arguments, and **there
   is no `CMD`**: operator arguments (`--content-origin`, `--api-origin`,
   `--trusted-proxy`, ...) are appended, and the server's existing flags and
   `ARTIFREE_*` variables are the only configuration. The binary is PID 1 and
   handles SIGTERM (exit 0).
4. **Health check** is `GET /healthz` over bash's `/dev/tcp` against
   `127.0.0.1:7410`, since the base image has bash but no HTTP client and
   installing one would break rule 1. The in-container API port is therefore
   fixed; operators publish other host ports with `-p`.
5. **The preview origin stays separate by configuration, and is mandatory in
   practice.** Inside a container the listeners bind `0.0.0.0`, so the server's
   own idea of its preview origin (`http://0.0.0.0:7411`) is unusable. The
   operator must pass `--content-origin` (documented in the run command).
   Equal API and preview origins are refused at startup (decision 0024) and the
   CI job requires that refusal through the image. We did not add an
   environment variable or a refusing wrapper script: that would be a new
   configuration surface for one deployment form (see O03d).
6. **MCP remains loopback-only in the image.** Docker delivers published-port
   traffic from the bridge, so the server sees a non-loopback peer and returns
   403 for `/mcp`. This is the intended default and CI asserts it. The guides
   say what choosing `--allow-remote-mcp` means and recommend a dedicated token
   per agent (decision 0023); no guide enables it by default.
7. **Trusted proxy in a container** must name the proxy's address on the
   container network. The Caddy example uses a compose network and a fixed proxy address
   (one trusted /32). A proxy on the Docker host that reaches a
   published port appears with the bridge gateway address instead.
8. **Verification.** `scripts/verify-container.mjs` (Node built-ins and the
   `docker` CLI) reuses `verifyManifest` and the shared service exercise in
   `scripts/lib/service-check.mjs`, now also used by `verify-release.mjs`. A
   new CI job `container` (needs `package`) runs it on `ubuntu-24.04`, 20-minute
   timeout, read-only token, actions pinned by SHA, and is in the quality gate.
   The job is justified because the image's behavior (user, volume ownership,
   read-only root, signals, published-port peer address, glibc) cannot be shown
   by the archive check. Static Dockerfile rules
   (`scripts/lib/dockerfile-policy.mjs`) and the image-metadata check run in
   `npm run check` without Docker, with negative tests for each rule.
9. **Guides** (`docs/container.md`, `docs/remote-access.md`) cover Caddy with TLS,
   Cloudflare Tunnel / ngrok, Tailscale / Headscale, and a static web UI on
   another host. Each says it was not executed against the real service.
   The static-UI path needs no code change: the web workspace accepts
   `?server=<api url>` (0022), the API accepts `--allowed-origin` (0024), and
   previews need `--frame-ancestors <ui origin>`. This was run once by hand in
   Chromium with host names mapped to loopback, over plain HTTP, and not in CI.

## Not changed, and gaps recorded

- No Rust, web, or contract change; no new dependency, lint exception, or
  boundary change. The only workflow change is the `container` job.
- No image is published, signed, or scanned. Bundled license texts, release
  publishing, signing/provenance, and the glibc floor policy are O03c.
- The static web workspace has no build-time default server, and
  `--frame-ancestors`/`--content-origin` have no environment variables, which
  makes container and static-UI setups argument-only. Tracked as O03d.
- Client locality behind a proxy follows the amendment of 2026-11-01 in
  [0024](0024-https-proxy-and-remote-access.md): a missing, all-trusted, or
  disagreeing forwarding chain is an unknown client and `/mcp` returns 403. The
  guides require the proxy to supply the chain and keep a post-setup `curl`
  check as a guard against misconfiguration such as trusting too broad a range.
- Not verified: other CPUs, rootless Docker, Podman, Kubernetes, compose,
  bind-mount ownership workflows other than the refusal, any named third-party
  service, the public internet. Codex and Pi were not run.

## Amendment 2026-11-02

[0028](0028-static-web-defaults.md) closes the O03d gaps recorded above:
`ARTIFREE_CONTENT_ORIGIN` and `ARTIFREE_FRAME_ANCESTORS` now exist, and a static
web build can carry a default server. Rule 5's statement that an environment
variable would be a new surface no longer stands; the image and its entrypoint
are unchanged.
