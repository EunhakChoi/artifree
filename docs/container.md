# Container image

Status: O03b, implemented as a build recipe and a CI check
([decision 0026](decisions/0026-container-image.md)); settings can be declared as
environment variables ([decision 0028](decisions/0028-static-web-defaults.md)).
**No image is published.**
There is no registry, tag, or signature; you build the image from a release
archive yourself. The image was built and run only by the `Container image` CI
job, on GitHub's `ubuntu-24.04` runner. It has not been run on any other host,
and Docker was not available where the change was written.

> **Warning.** Like the archives it is built from, the image does not contain
> the full license texts of the Rust crates and npm packages inside it
> ([release packages](release-packages.md)). Do not redistribute it.

## What the image is

The image is the Linux x64 release package (`artifree-<version>-linux-x64`,
decision 0025) copied to `/app` on `debian:bookworm-slim`, pinned by digest.
Nothing is compiled or downloaded while building. Only Linux x64 is supported (pass `--platform linux/amd64` to `docker build` and
`docker run`; on an ARM host the pinned multi-arch base would otherwise resolve
to arm64 and the x86-64 binary would not run): no ARM image, no Alpine/musl, no Windows container.

| Item        | Value                                                                                                               |
| ----------- | ------------------------------------------------------------------------------------------------------------------- |
| User        | `10001:10001`, no login shell, no home                                                                              |
| Data        | volume `/data` (database and `access-token`), mode 700, owned by the user                                           |
| Ports       | `7410` API and web workspace; `7411` preview content, a separate origin                                             |
| Command     | `artifree-server serve --data-dir /data --web-dir /app/web --api-listen 0.0.0.0:7410 --content-listen 0.0.0.0:7411` |
| Your flags  | everything after the image name is appended; server environment variables (`ARTIFREE_*`) work as usual, see below   |
| Healthcheck | `GET /healthz` on `127.0.0.1:7410` inside the container, using the shell already in the image                       |
| Not inside  | Node.js (the bundled CLI in `/app/cli` cannot run there), curl, any package manager step                            |

## Build

Download the `release-linux-x64` workflow artifact from a CI run, then:

```sh
sha256sum -c artifree-<version>-linux-x64.tar.gz.sha256
node scripts/verify-container.mjs path/to/release   # needs Docker and Node.js 22.12+
```

`verify-container.mjs` unpacks the archive, checks it against its manifest,
builds the image, and runs the checks listed below. To build by hand:

```sh
mkdir pkg && tar -xzf artifree-<version>-linux-x64.tar.gz -C pkg
docker build --platform linux/amd64 -t artifree:local -f deploy/container/Dockerfile pkg/artifree-<version>-linux-x64
```

## Run on one machine

The preview origin must be given, because the container cannot know which
address your browser uses for port 7411:

```sh
docker volume create artifree-data
docker run -d --name artifree --restart unless-stopped --platform linux/amd64 \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  -v artifree-data:/data \
  -p 127.0.0.1:7410:7410 -p 127.0.0.1:7411:7411 \
  artifree:local --content-origin http://127.0.0.1:7411
docker exec artifree cat /data/access-token      # paste into http://127.0.0.1:7410
```

- Without `--content-origin` the server reports `http://0.0.0.0:7411` as the
  preview origin, which a browser cannot use.
- Publishing on other host ports (`-p 127.0.0.1:9000:7410`) changes the `Host`
  the server sees. Add `--allowed-host 127.0.0.1:9000` and set
  `--content-origin` to the matching preview port. Keep the in-container
  ports at 7410 and 7411; the healthcheck follows 7410.
- Publish to `127.0.0.1` unless you mean to expose the port. Anything reachable
  on the published port can attempt to authenticate with the token.
- A bind mount instead of a volume must be owned by uid 10001 and mode 700;
  the server refuses a data directory that other users can enter. A named
  volume gets the right owner and mode from the image.
- The container stops on `docker stop` (SIGTERM) with status 0. Data is in the
  volume; remove the container, not the volume, to upgrade: build a new image
  and run it on the same volume. Take a backup first
  ([backup guide](backup-restore.md#containers), commands not run in CI); there
  is no downgrade.

## Settings as environment variables

Every setting the container needs can be passed with `docker run --env` (or an
env file) instead of arguments after the image name:

| Variable                                                                         | Same as              | Notes                                                                             |
| -------------------------------------------------------------------------------- | -------------------- | --------------------------------------------------------------------------------- |
| `ARTIFREE_CONTENT_ORIGIN`                                                        | `--content-origin`   | Public preview origin. Required in practice (see above).                          |
| `ARTIFREE_FRAME_ANCESTORS`                                                       | `--frame-ancestors`  | Space-separated CSP sources allowed to embed previews. Replaces the default list. |
| `ARTIFREE_API_ORIGIN`                                                            | `--api-origin`       | Public API and web origin behind a proxy.                                         |
| `ARTIFREE_ALLOWED_HOSTS`, `ARTIFREE_ALLOWED_ORIGINS`, `ARTIFREE_TRUSTED_PROXIES` | the repeated flags   | Comma-separated. Added to anything the flags give.                                |
| `ARTIFREE_ALLOW_REMOTE_MCP`                                                      | `--allow-remote-mcp` | `true` or `1`. Read the MCP section first.                                        |
| `ARTIFREE_LAN`, `ARTIFREE_LOOPBACK_LISTEN`, `ARTIFREE_WEB_DIR`                   | the matching flags   | The image's fixed `--web-dir` and listen flags win over them.                     |

```sh
docker run -d --name artifree --platform linux/amd64 --read-only \
  -v artifree-data:/data -p 127.0.0.1:7410:7410 -p 127.0.0.1:7411:7411 \
  --env ARTIFREE_CONTENT_ORIGIN=http://127.0.0.1:7411 \
  --env 'ARTIFREE_FRAME_ANCESTORS=http://127.0.0.1:* https://ui.example.app' \
  artifree:local
```

- **A flag wins over its variable.** An empty variable counts as unset. For
  `ARTIFREE_CONTENT_ORIGIN` and `ARTIFREE_FRAME_ANCESTORS`, a value that is not
  valid UTF-8 stops the server unless the flag is given; for the older
  variables, an empty or non-UTF-8 value is silently ignored.
- **A bad value stops the server** with a nonzero exit and a message on stderr
  (so in `docker logs`) that names the variable or flag and the rule, never the
  value. Origins must be `http(s)://host[:port]` with no user name, password,
  path, query or fragment, and a port from 1 to 65535. An `--allowed-origin`
  entry like `*`, a bare host or `https://ui.example:65536` is an error (it
  used to be accepted and never match). A frame-ancestors list must not be
  empty or contain `;` or `,`.
- **Changing a setting needs a new container** (or a restart): the CORS
  allow-list, allowed hosts, trusted proxies and frame ancestors are read once
  at start-up. A restart drops preview links, runtime sessions and agent
  handoffs, which are held in memory; the data volume is untouched. There is
  no reload.
- Two things are still shown in errors: the socket address when binding a port
  fails (it was parsed as an IP address and port), and file-system paths such as
  the data directory.

## Behind a reverse proxy

Use the flags of [decision 0024](decisions/0024-https-proxy-and-remote-access.md)
as arguments after the image name. A container sees the proxy at its
container-network address, not at `127.0.0.1`: `--trusted-proxy` must name the
network the proxy connects from, ideally one fixed `/32`. [Remote access](remote-access.md) has a Caddy
example with a fixed network.

## MCP

MCP is loopback-only unless `--allow-remote-mcp` is set (decision 0024). A
request that arrives through a published Docker port comes from the Docker
bridge, not loopback, so **an agent on the Docker host cannot reach `/mcp`
through the published port** (CI checks that it gets 403 on GitHub's default
rootful Docker bridge; rootless Docker and Podman use other port drivers that
may present a different peer address, which was not checked). Do not add the bridge gateway to `--trusted-proxy` to "fix" this: trusting it
lets anything that can reach the published port claim any client address,
including loopback, through `X-Forwarded-For`, and `/mcp` then passes the
loopback check with only the token left. Do so only with the port published on
`127.0.0.1` and no untrusted local process, as described in
[Tunnel daemon and the container](remote-access.md#cloudflare-tunnel-or-ngrok)
(only requests with no forwarding header, a chain of only trusted proxies, or
disagreeing headers become unknown clients, decision 0024, amendment of
2026-11-01). Do not set
`--allow-remote-mcp` unless you have decided that every client that can reach
the API port may attempt MCP. Two ways to give an agent access:

- Use the unpacked archive on the host, where the agent connects over
  `127.0.0.1`.
- Run the agent as another container on the same user-defined network, start
  Artifree with `--allow-remote-mcp --allowed-host artifree:7410` (use your
  service name and port), give the agent its own token (decision 0023), and
  point it at `http://artifree:7410/mcp`. The `--allowed-host` is needed
  twice over: the API rejects any `Host` that is not allowed with 400
  (`proxy_inspection` in `http.rs`), and the MCP endpoint builds its own
  allowed-host list from the same setting (`mcp.rs`), so without it the
  request is refused before MCP is reached. This recipe was not run.

## What CI checks

The `Container image` job downloads the `release-linux-x64` archive built by
the `Release archive` job, then `scripts/verify-container.mjs`:

- checks the archive checksum and every manifest hash, and the Dockerfile's
  rules (digest-pinned base, numeric non-root user, `/data` volume, both ports,
  health check, exec-form entrypoint, no network access while building);
- builds the image and checks the user, ports, volume, health check, and
  entrypoint in the image metadata;
- runs it with `--read-only --cap-drop ALL --no-new-privileges`, waits until
  Docker reports it healthy, confirms the process runs as uid 10001;
- from the host, through the published ports: health, the packaged web page,
  create and save with the packaged CLI, a preview on the separate origin with
  the sandbox CSP;
- confirms `/mcp` is refused from the published port;
- stops with SIGTERM and requires exit status 0, starts a new container on the
  same volume, and finds revision 2;
- starts the container with `ARTIFREE_CONTENT_ORIGIN`,
  `ARTIFREE_ALLOWED_HOSTS` and `ARTIFREE_FRAME_ANCESTORS` instead of
  arguments, and checks that the preview's CSP carries the frame-ancestors list;
- requires three refusals: equal API and preview origins declared by
  `ARTIFREE_API_ORIGIN` and `ARTIFREE_CONTENT_ORIGIN`, an invalid
  `ARTIFREE_CONTENT_ORIGIN` whose log must not contain the password inside it,
  and a bind-mounted data directory that other users can enter.

Not checked: any registry, other CPUs, older Docker versions, rootless Docker
or Podman, Kubernetes, `docker compose`, a reverse proxy in front of the
container, resource limits, log handling, upgrades from an earlier image.
