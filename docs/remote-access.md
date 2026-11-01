# Remote access guides

Status: O03b ([decision 0026](decisions/0026-container-image.md)), documentation
only. These guides rest on the server flags of decisions
[0022](decisions/0022-production-web-and-api-serving.md),
[0023](decisions/0023-single-owner-token-management.md), and
[0024](decisions/0024-https-proxy-and-remote-access.md). **None of the third-party
services below (Caddy, Cloudflare, ngrok, Tailscale, Headscale, Vercel) was run
for this change.** The commands are written from the services' public
documentation as understood when this was written; their syntax changes, so
read their current documentation first. What was actually run is in
[the record at the end](#what-was-actually-run).

## Rules for every path

1. **Two origins, always.** Artifree needs one origin for the API and web
   workspace and a different one for previews. Different hostnames, or the same
   hostname on different ports, both work; identical origins are refused at
   startup. A path that gives you only one public address is not enough.
2. **The access token is the only login.** Anyone who can reach the API and
   holds the token has the whole workspace. Use HTTPS on the public internet,
   keep the token out of chat, shell history, and screenshots, and give each agent its
   own token (`artifree token create`, decision 0023) so it can be revoked or
   rotated without changing the owner token in `<data-dir>/access-token`.
3. **MCP stays loopback-only** unless you start the server with
   `--allow-remote-mcp`. Behind a proxy the server judges the client address
   from `X-Forwarded-For`, but only when the request comes from a
   `--trusted-proxy`. Do not set `--allow-remote-mcp` just to make a tunnel
   work; if an agent must connect remotely, issue it a dedicated token and
   revoke it when done. Today a remote agent token is a full-owner token:
   there are no scopes.
4. **Trusted proxy is the proxy's address as the server sees it.** Never trust a
   range that internet clients can send from. A request with forwarded headers
   from any other peer is refused with 403.
5. **Tunnels see your traffic.** A hosted tunnel terminates TLS, so its operator
   can read tokens and artifacts in transit. Treat that as a trust decision.

## Reverse proxy with TLS (Caddy)

Caddy obtains certificates automatically for two public names that point to
your server, for example `artifree.example.com` and `preview.example.com`. This
example runs Artifree from the [container](container.md) and Caddy as sibling
containers on a network with a fixed subnet, so `--trusted-proxy` can be
narrow. Artifree publishes no port to the host; only Caddy does.

`compose.yaml` (not run; `docker compose` was not available):

```yaml
services:
  artifree:
    image: artifree:local # built as in container.md
    read_only: true
    cap_drop: [ALL]
    security_opt: ['no-new-privileges:true']
    volumes: ['artifree-data:/data']
    command:
      - --api-origin=https://artifree.example.com
      - --content-origin=https://preview.example.com
      - --trusted-proxy=172.28.0.0/24
    networks: [edge]
  caddy:
    image: caddy:2 # pin a digest for real use
    ports: ['80:80', '443:443']
    volumes: ['./Caddyfile:/etc/caddy/Caddyfile:ro', 'caddy-data:/data']
    networks: [edge]
networks:
  edge:
    ipam:
      config: [{ subnet: 172.28.0.0/24 }]
volumes:
  artifree-data:
  caddy-data:
```

`Caddyfile`:

```text
artifree.example.com {
	reverse_proxy artifree:7410
}
preview.example.com {
	reverse_proxy artifree:7411
}
```

- The server derives its allowed `Host` from `--api-origin`; the preview
  listener checks forwarded headers but not `Host`.
- Without a container, run `artifree-server serve ... --api-origin ...
--content-origin ... --trusted-proxy 127.0.0.1/32` on the host and point Caddy
  at `127.0.0.1:7410` and `127.0.0.1:7411`. This matches the shape of the
  automated proxy test of decision 0024, which used a Node TLS proxy, not Caddy.
- The live change feed is a long-lived response; Caddy streams these by default
  as far as its documentation says. Check that the web workspace shows saves
  from the CLI without a reload.
- Keep MCP loopback-only: do not add `--allow-remote-mcp`. A local agent on the
  server host can use the unpacked archive on `127.0.0.1`.

## Cloudflare Tunnel or ngrok

Both give a public HTTPS name for a service that is not otherwise reachable.
You need **two** public names mapped to `127.0.0.1:7410` and `127.0.0.1:7411`
(for example `artifree.example.com` and `preview.example.com`); a plan or
configuration that gives you one address cannot run previews. Cloudflare Tunnel
uses one `cloudflared` process with two ingress rules (hostname to
`http://127.0.0.1:7410` and to `http://127.0.0.1:7411`). For ngrok, check the
current syntax for reserved domains and for running two endpoints.

Run Artifree with the public names and the address the agent connects from,
which is loopback when `cloudflared` or `ngrok` runs on the same host:

```sh
artifree-server serve --data-dir ./data --web-dir ./web \
  --api-origin https://artifree.example.com \
  --content-origin https://preview.example.com \
  --trusted-proxy 127.0.0.1/32
```

Do not use `--trusted-proxy` with a container's published ports unless you
have read the note in [the container guide](container.md): the server sees the
Docker bridge address, not `127.0.0.1`.

Points to settle before using it:

- The public name reaches your API from anywhere. The token is the only
  protection unless you add the provider's access control, and then CLI and
  agent clients need whatever credential that layer requires; this guide did not
  test any such combination.
- Confirm in the provider's documentation which forwarded headers it sends and
  whether it passes the original `Host`. The server accepts `Forwarded` or
  `X-Forwarded-*` from a trusted peer and uses the `Host` header otherwise.
- Do not start `--allow-remote-mcp`. A tunnel makes every internet client a
  "remote client" whose only barrier is the token.

## Private network (Tailscale or Headscale)

A tailnet gives each device a private address and encrypts traffic between
devices, so HTTPS is optional for the API when every client is on the tailnet.
Headscale is a self-hosted control server for the same client, so the steps are
the same except where noted. Artifree needs no tunnel software.

Simplest form, no TLS: bind to the server's tailnet address.

```sh
artifree-server serve --data-dir ./data --web-dir ./web --lan 100.64.0.10
```

(`100.64.0.10` stands for your device's tailnet IP.) This listens on
`100.64.0.10:7410` and `:7411`, allows that address as `Host`, sets the preview
origin to `http://100.64.0.10:7411`, and keeps a loopback listener for local
agents (decision 0022). Open `http://100.64.0.10:7410` from another device on the
tailnet. Do not bind `0.0.0.0` with `--lan`; it refuses unspecified addresses.

For HTTPS names, Tailscale's `serve` feature can publish local ports with a
certificate for the device's name; use one port for the API and another for the
preview so the origins differ, then set `--api-origin`, `--content-origin`, and
`--trusted-proxy 127.0.0.1/32` as in the proxy example. Headscale does not
necessarily provide those certificates; use the plain `--lan` form or your own
reverse proxy. Neither was run.

- Restrict who on the tailnet can reach ports 7410 and 7411 with the network's
  access rules; every device that can connect may try the token.
- MCP: an agent on the server itself uses `http://127.0.0.1:7410/mcp`. An agent
  on another tailnet device needs `--allow-remote-mcp` and its own token; make
  that choice per device, not by default.

## Static web workspace on another host (for example Vercel)

**This works today without code changes** when the API is served over HTTPS
(browsers block an HTTPS page from calling a plain-HTTP API). The built web
workspace is plain static files, and its API address can be chosen at run time.

1. Build: `npm ci && npm run build`. Publish the contents of `apps/web/dist`
   as a static site (on Vercel, deploy that directory as static output). The
   workspace has no client-side path routes, so no rewrite rules are needed.
2. Run the API on its own public HTTPS origin and preview origin (see the
   proxy example), and allow the UI origin to call it and to frame previews.
   With the UI at `https://ui.example.app`:

   ```sh
   artifree-server serve --data-dir ./data \
     --api-origin https://api.example.com \
     --content-origin https://preview.example.com \
     --trusted-proxy 127.0.0.1/32 \
     --allowed-origin https://ui.example.app \
     --frame-ancestors https://ui.example.app
   ```

   Both extra flags are needed. Without `--allowed-origin` the browser's CORS
   check fails; without `--frame-ancestors` previews are blocked from rendering in
   the UI (the CSP default lists only the API origin and loopback). Note that
   `--frame-ancestors` replaces the default list instead of extending it, and has
   no environment variable. `--api-origin` also adds the API's own origin to
   the allowed CORS origins.

3. Open `https://ui.example.app/?server=https://api.example.com`, paste the
   access token, and connect. A server address given in the URL suppresses restoring a
   stored token (decision 0022), so the token is entered again.

Limits and risks:

- The static host serves the code that handles your token. A compromised
  deployment or an account takeover on that host can steal it. Use a dedicated
  agent token (decision 0023) for browsers on such a host if you want to
  revoke it separately.
- There is no build-time default server address. Every new tab needs the
  `?server=` URL or the Server URL field. A default and environment variables
  for the container's remaining flags are tracked as roadmap item O03d.
- Previews are separate from the UI in origin, not from the network
  (decision 0003). Their frame can navigate elsewhere.
- Hosting platforms' preview deployments get new origins; each needs adding to
  `--allowed-origin` and `--frame-ancestors`, which cannot be changed without a
  restart.

## What was actually run

| Claim                                                                                                                                                                                                                                                                                    | Where it ran                                                                                                                                                 |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| A static copy of `apps/web/dist` on `http://ui.test:5000` talks to a server with `--api-origin http://api.test:7410 --content-origin http://content.test:7411 --allowed-origin http://ui.test:5000 --frame-ancestors http://ui.test:5000`: list loads and a preview renders in its frame | Once, by hand, in Playwright Chromium on a development machine; the three names were mapped to `127.0.0.1` by the browser. Plain HTTP on one host. Not in CI |
| The same setup without `--frame-ancestors` shows the list but the preview is blocked by CSP `frame-ancestors`; without `--allowed-origin` the list never loads (CORS)                                                                                                                    | Same manual run                                                                                                                                              |
| The container image builds, runs read-only as uid 10001, stays healthy, saves, restarts on the same volume, and refuses two misconfigurations                                                                                                                                            | Only in the CI job `Container image` ([container guide](container.md))                                                                                       |
| HTTPS proxy behavior (forwarded headers, Host, MCP, preview CSP, change feed)                                                                                                                                                                                                            | Decision 0024's automated test with a Node TLS proxy on one host                                                                                             |
| Caddy, Cloudflare Tunnel, ngrok, Tailscale, Headscale, Vercel, `docker compose`, any real certificate, any second machine, the public internet                                                                                                                                           | Not run                                                                                                                                                      |
| Codex and Pi connecting to Artifree                                                                                                                                                                                                                                                      | Not run in this change; their status is unchanged from [MCP guide](mcp.md) and the roadmap                                                                   |
