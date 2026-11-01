# 0024: HTTPS proxy and remote access

Date: 2026-11-01. Status: accepted. Builds on
[0001](0001-foundation.md),
[0003](0003-local-html-artifact-slice.md),
[0004](0004-inbound-mcp-adapter.md),
[0022](0022-production-web-and-api-serving.md), and
[0023](0023-single-owner-token-management.md).

## Context

Artifree's single-owner architecture (decision 0001) supports local and LAN
environments (decision 0022) with distinct credentials per agent (decision
0023). Enabling remote web and agent access requires terminating TLS via a
reverse proxy (such as Caddy, Nginx, Traefik, or cloud load balancers).

Blindly trusting forwarded headers (`Forwarded`, `X-Forwarded-*`) introduces
critical security vulnerabilities:

1. **Header spoofing**: An untrusted caller connecting directly to the server can
   spoof client IPs or protocol schemes.
2. **Loopback proxy confusion**: When a reverse proxy runs on loopback
   (`127.0.0.1`), checking only the immediate TCP socket peer would misclassify
   remote requests forwarded through the proxy as local loopback calls,
   unintentionally exposing loopback-restricted endpoints like MCP.
3. **Origin and Host confusion**: Unchecked `Host` headers expose the server to
   DNS rebinding and cache poisoning, while conflating API and preview origins
   defeats iframe sandbox isolation (decision 0003).

## Decision

1. **Explicit trusted proxy network allowlist.**
   - The `--trusted-proxy` CLI flag and `ARTIFREE_TRUSTED_PROXIES` environment
     variable configure a list of trusted IPv4 and IPv6 addresses or CIDR
     prefixes (`IpNetwork`).
   - If a request arrives from a socket peer that is _not_ in `trusted_proxies`
     and contains any forwarded header (`Forwarded`, `X-Forwarded-For`,
     `X-Forwarded-Host`, or `X-Forwarded-Proto`), the server immediately rejects
     it with `403 Forbidden` (`ErrorCode::InvalidRequest`).
   - Only socket peers matching `trusted_proxies` have their forwarded headers
     evaluated to extract client IP, host, and protocol scheme.
2. **Effective client IP resolution.**
   - In requests from trusted proxies, the leftmost IP in `X-Forwarded-For` or the
     `for` directive in RFC 7239 `Forwarded` is extracted as the effective client
     IP (`ClientConnectionInfo.client_ip`).
   - In direct requests without trusted proxies, the socket peer IP is the
     effective client IP.
3. **Loopback MCP confinement with explicit remote opt-in.**
   - The MCP endpoint (`/mcp`) checks the effective client IP, _not_ merely the
     immediate socket peer. Even when the proxy connects via `127.0.0.1`, a remote
     client forwarded through the proxy is recognized as remote.
   - MCP access remains confined to loopback clients by default.
   - Remote MCP access is permitted only when explicitly enabled via
     `--allow-remote-mcp` or `ARTIFREE_ALLOW_REMOTE_MCP=true`.
4. **Strict Host and Origin policies.**
   - `--allowed-host` and `ARTIFREE_ALLOWED_HOSTS` configure permitted `Host` and
     `X-Forwarded-Host` values. Default ports (80 for HTTP, 443 for HTTPS) are
     normalized away before case-insensitive comparison. Requests with
     unauthorized hosts return `400 Bad Request`.
   - `--api-origin` configures the public external API and web origin.
   - `--content-origin` configures the public preview origin.
   - An origin conflict check in `AppState::new` and CLI parsing strictly rejects
     identical `--api-origin` and `--content-origin` values, maintaining iframe
     sandbox separation.
   - When `--api-origin` is active, API CORS permits only requests originating
     from `--api-origin`, explicit `--allowed-origin`, or local loopback origins.
     Requests with `Origin: null` or the preview origin are rejected with `403 Forbidden`.
   - CSP `frame-ancestors` on preview pages automatically incorporates the
     configured `--api-origin` alongside local defaults.
5. **Verification boundary.**
   - Implementation verified with automated Rust integration tests
     (`proxy_remote_access.rs`) and an end-to-end Node test suite
     (`remote-proxy-mcp.test.mjs`) driving a real TLS reverse proxy with ECDSA
     certificates and the official `@modelcontextprotocol/sdk` client.
   - Verified flows: artifact creation, revision updates, HTTPS preview retrieval
     with CSP verification, live SSE change feed streaming, agent token issuance
     and revocation, spoofed header rejection, and Host validation.
   - Testing over separate physical network hardware or public cloud infrastructure
     was not executed in this local test suite and remains categorized as unverified
     external environment.

## Consequences

- Artifree can safely run behind standard TLS reverse proxies with strict
  defense against forwarded header forgery.
- MCP's loopback security model is preserved by default behind local reverse proxies.
- Preview sandbox isolation remains fully enforced across distinct public origins.

## Amendment 2026-11-01: unknown clients fail closed

Decision 2 above misdescribed the implementation and left fail-open gaps. This
amendment replaces its resolution rules; everything else stands.

- **Effective client.** Behind a trusted proxy peer, every `X-Forwarded-For`
  line, and separately every `Forwarded` line, is read and joined in arrival
  order as one comma list (RFC 9110 field combination). The client is the
  rightmost entry that is not in `--trusted-proxy`. Entries a client prepended
  further left are never consulted. A non-ASCII line anywhere, or an invalid
  address, is `400`.
- **Unknown client.** The client is unknown (`ClientConnectionInfo.client_ip`
  is `None`; there is no placeholder address) when
  1. a trusted peer sent neither `X-Forwarded-For` nor `Forwarded`
     (`X-Forwarded-Host` and `X-Forwarded-Proto` do not name a client);
     previously the peer address was used, so a same-host tunnel or proxy that
     strips visitor addresses made every internet request look like loopback;
  2. every entry in the chain is a trusted proxy; a client-supplied
     `127.0.0.1` cannot be told apart from one a proxy recorded (tunnel to local
     proxy to Artifree, with 127.0.0.1 trusted, where an attacker sends
     `X-Forwarded-For: 127.0.0.1` and the local proxy appends the tunnel's
     `127.0.0.1`); the earlier "leftmost of an all-trusted chain" rule is
     removed;
  3. both headers are present and resolve to different clients; one of them was
     not written by the proxy. If they agree, that client is used; and
  4. the socket peer address is missing. It is never assumed to be loopback
     and is never trusted.
- **Consumers fail closed.** `/mcp` allows a request only if `--allow-remote-mcp`
  is set or the client is known and loopback (IPv4-mapped IPv6 such as
  `::ffff:127.0.0.1` is canonicalized first). Unknown is `403`. No other code
  reads `client_ip`: Host and Origin checks, rate limits, and preview use the
  peer socket, host, and proto, so other routes keep working for unknown clients.
- **Operator consequences.** The proxy must append (or overwrite) the client
  address in the header it uses and strip the other one. A local agent that goes
  through a local trusted proxy now has an unknown or all-trusted chain and gets
  `403` on `/mcp`; local agents connect directly to the loopback listener.
  A direct loopback client without a trusted proxy is its own client.
- **Known limitation.** `X-Forwarded-Host`, `X-Forwarded-Proto`, and the
  `host=`/`proto=` parameters of `Forwarded` read only the first line. They
  do not affect client locality.
