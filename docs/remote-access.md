# Remote access guides

Status: O03b ([decision 0026](decisions/0026-container-image.md)), documentation
only; O03d ([decision 0028](decisions/0028-static-web-defaults.md)) added the
static build default and the environment variables used below. These guides rest on the server flags of decisions
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
   from the forwarding chain, but only when the request comes from a
   `--trusted-proxy` (rule 5). A local agent therefore must **not** go through
   the local proxy or tunnel: it gets 403 there. It connects directly to the
   loopback listener (`http://127.0.0.1:7410/mcp`). Do not set `--allow-remote-mcp` just to make a tunnel
   work; if an agent must connect remotely, issue it a dedicated token and
   revoke it when done. Today a remote agent token is a full-owner token:
   there are no scopes.
4. **Trusted proxy is the proxy's address as the server sees it.** Never trust a
   range that internet clients can send from. A request with forwarded headers
   from any other peer is refused with 403.
5. **The proxy or tunnel must record the client address.** Behind a trusted
   proxy, the effective client is the rightmost entry of the forwarding chain
   (`X-Forwarded-For` or `Forwarded`, all lines joined) that is not itself a
   trusted proxy ([decision 0024](decisions/0024-https-proxy-and-remote-access.md),
   amendment of 2026-11-01). The client is **unknown**, and `/mcp` returns 403
   (unless `--allow-remote-mcp`), when the proxy sent no forwarding header,
   when every entry is a trusted proxy, when `X-Forwarded-For` and `Forwarded`
   resolve to different clients, or when the peer address is missing. So the
   proxy must append (or overwrite) the real client address in the one header it
   uses and strip the other, and a setting that hides visitor IPs makes `/mcp`
   unusable rather than open. Other routes keep working for unknown clients.
   Do not trust a range wider than your proxies. Run the post-setup check
   below; it still catches misconfiguration such as trusting too broad a
   range.
6. **Tunnels see your traffic.** A hosted tunnel terminates TLS, so its operator
   can read tokens and artifacts in transit. Treat that as a trust decision.

**Post-setup check (all paths with a public name).** From a machine outside your
network, with a valid token and without `--allow-remote-mcp`:

```sh
curl -s -o /dev/null -w '%{http_code}\n' -X POST https://artifree.example.com/mcp \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{}'
```

It must print `403`. Any other code means the server treats internet clients as
local; stop and fix the proxy headers or `--trusted-proxy` before using the
service. Repeat it after changing the proxy, tunnel, or flags.

## Reverse proxy with TLS (Caddy)

Caddy obtains certificates automatically for two public names that point to
your server, for example `artifree.example.com` and `preview.example.com`. This
example runs Artifree from the [container](container.md) and Caddy as sibling
containers on a network with a fixed subnet, so `--trusted-proxy` can be
a single address. Artifree publishes no port to the host; only Caddy does.

`compose.yaml` (not run; `docker compose` was not available). The settings are
the environment variables of [the container guide](container.md#settings-by-environment-variable);
the flags of the same name work too:

```yaml
services:
  artifree:
    image: artifree:local # built as in container.md
    read_only: true
    cap_drop: [ALL]
    security_opt: ['no-new-privileges:true']
    volumes: ['artifree-data:/data']
    environment:
      ARTIFREE_API_ORIGIN: https://artifree.example.com
      ARTIFREE_CONTENT_ORIGIN: https://preview.example.com
      ARTIFREE_TRUSTED_PROXIES: 172.28.0.2/32
    networks: [edge]
  caddy:
    image: caddy:2 # pin a digest for real use
    ports: ['80:80', '443:443']
    volumes: ['./Caddyfile:/etc/caddy/Caddyfile:ro', 'caddy-data:/data']
    networks:
      edge:
        ipv4_address: 172.28.0.2 # the only trusted proxy
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
- Caddy's `reverse_proxy` sets `X-Forwarded-For` by default, per its documentation; the post-setup check above confirms it.
- Keep MCP loopback-only: do not add `--allow-remote-mcp`. A local agent on the
  server host must not go through Caddy (that is 403); it connects directly to
  `http://127.0.0.1:7410/mcp` of the unpacked archive.

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

**Tunnel daemon and the container.** A daemon on the Docker host that reaches
a published port appears to the container as the bridge gateway (for example
`172.17.0.1`), the same address as every other host process and, with the port
published on all interfaces, every network client. Trusting that gateway
(`--trusted-proxy 172.17.0.1/32`) lets anything that can reach the published
port forge `X-Forwarded-For`, so it is acceptable only if the port is published
on `127.0.0.1` alone (`-p 127.0.0.1:7410:7410`) and no untrusted local process
exists. The safer shape is to run the tunnel daemon as a sibling container on a
user-defined network with a fixed address (as in the Caddy example), publish no
port, point its ingress at `http://artifree:7410` and `http://artifree:7411`,
and trust only that `/32`. Add `--allowed-host artifree:7410` only if the
daemon forwards the internal `Host` instead of the public name (the public name
is allowed through `--api-origin`). Neither shape was run.

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
- Make sure the tunnel records the visitor address in `X-Forwarded-For` (or
  `Forwarded`). A setting that removes visitor IP headers leaves the client
  unknown and `/mcp` returns 403 (rule 5); a local agent must not use the
  tunnel for MCP either. Run the post-setup check above through the public
  name.

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

**This works when the API is served over HTTPS** (browsers block an HTTPS page
from calling a plain-HTTP API). The built web workspace is plain static files.
Which API it talks to can be set when it is built (the default) or chosen in
the page (`?server=` or the Server URL field).

1. Build with the API address as the default, and publish the contents of
   `apps/web/dist` as a static site (on Vercel, set `VITE_ARTIFREE_SERVER` in the
   project's environment variables and deploy that directory as static output):

   ```sh
   npm ci && VITE_ARTIFREE_SERVER=https://api.example.com npm run build
   ```

   The address must be an absolute `http://` or `https://` address without
   credentials, query, or fragment (a path prefix is kept); anything else fails
   the build. Without the variable the page behaves as before (it uses its own
   origin). Release archives are built without it, because `artifree-server`
   serves their web files itself. The workspace has no client-side path routes,
   so no rewrite rules are needed.

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

   In a container the same values are variables: `ARTIFREE_API_ORIGIN`,
   `ARTIFREE_CONTENT_ORIGIN`, `ARTIFREE_TRUSTED_PROXIES`,
   `ARTIFREE_ALLOWED_ORIGINS=https://ui.example.app`, and
   `ARTIFREE_FRAME_ANCESTORS=https://ui.example.app`.

   Both UI settings are needed. Without the allowed origin the browser's CORS
   check fails; without the frame ancestor previews are blocked from rendering in
   the UI (the CSP default lists only the API origin and loopback). Note that
   `--frame-ancestors` replaces the default list instead of extending it.
   `--api-origin` also adds the API's own origin to the allowed CORS origins.
   Allowed origins are normalized like the browser's (`https://UI.example.app/`
   matches `https://ui.example.app`); an entry that is not an `http(s)` origin
   stops the server at start-up.

3. Open `https://ui.example.app/`, paste the access token, and connect. The
   Server URL field already holds the default. A link with
   `?server=https://other.example.com` replaces it for that tab. A server address
   in the URL suppresses restoring a stored token (decision 0022), so the token
   is entered again; the default does not.

Limits and risks:

- A `?server=<url>` link makes whoever opens it send their token to the
  named server. Only use links you built yourself, and never open one from
  someone else. The page refuses addresses that are not `http(s)` or that carry
  credentials, but it cannot tell a friendly server from a hostile one.
- The API still needs the headers and post-setup check of rule 5 and the
  check above; the static host does not change them.
- The static host serves the code that handles your token, and the default
  address is part of that code. A compromised deployment or an account takeover
  on that host can steal the token or point the page elsewhere. Use a dedicated
  agent token (decision 0023) for browsers on such a host if you want to
  revoke it separately.
- The default is fixed at build time. To change it, rebuild and redeploy.
- Previews are separate from the UI in origin, not from the network
  (decision 0003). Their frame can navigate elsewhere.
- Hosting platforms' preview deployments get new origins; each needs adding to
  `--allowed-origin` and `--frame-ancestors`. There is no wildcard, and these
  lists are read once at start-up, so adding one means restarting the server.
  Serve the UI from a stable name (a production alias) to avoid this. See
  [decision 0028](decisions/0028-static-web-defaults.md#changing-the-cors-allow-list-needs-a-restart).

## What was actually run

| Claim                                                                                                                                                                                                                                                                                    | Where it ran                                                                                                                                                           |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A static copy of `apps/web/dist` on `http://ui.test:5000` talks to a server with `--api-origin http://api.test:7410 --content-origin http://content.test:7411 --allowed-origin http://ui.test:5000 --frame-ancestors http://ui.test:5000`: list loads and a preview renders in its frame | Once, by hand, in Playwright Chromium on a development machine; the three names were mapped to `127.0.0.1` by the browser. Plain HTTP on one host. Not in CI           |
| The same setup without `--frame-ancestors` shows the list but the preview is blocked by CSP `frame-ancestors`; without `--allowed-origin` the list never loads (CORS)                                                                                                                    | Same manual run                                                                                                                                                        |
| A static build made with `VITE_ARTIFREE_SERVER` on a plain file server (its own origin) reads the default server, a `?server=` link overrides it, unsafe links and typed addresses are refused before any request, and the build fails on a bad value                                    | The browser smoke suite (`npm run test:smoke`), Chromium and Firefox, loopback and plain HTTP, run on a development machine and in CI. Not a real static host or HTTPS |
| `ARTIFREE_CONTENT_ORIGIN` and `ARTIFREE_FRAME_ANCESTORS` reach the preview policy of the real server binary, flags win, bad values stop it                                                                                                                                               | Node tests against the built binary (`container-env.test.mjs`); the image itself only in the CI job `Container image`                                                  |
| The container image builds, runs read-only as uid 10001, stays healthy, saves, restarts on the same volume, and refuses two misconfigurations                                                                                                                                            | Only in the CI job `Container image` ([container guide](container.md))                                                                                                 |
| HTTPS proxy behavior (forwarded headers, Host, MCP, preview CSP, change feed)                                                                                                                                                                                                            | Decision 0024's automated test with a Node TLS proxy on one host                                                                                                       |
| Caddy, Cloudflare Tunnel, ngrok, Tailscale, Headscale, Vercel, `docker compose`, any real certificate, any second machine, the public internet                                                                                                                                           | Not run                                                                                                                                                                |
| Codex and Pi connecting to Artifree                                                                                                                                                                                                                                                      | Not run in this change; their status is unchanged from [MCP guide](mcp.md) and the roadmap                                                                             |
