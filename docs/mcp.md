# Using Artifree from MCP agents

`artifree-server serve` answers MCP over Streamable HTTP at `/mcp` on the API
listener (default `http://127.0.0.1:7410/mcp`). Agents use it to create,
read, edit, and preview HTML artifacts with the same rules as the HTTP API
and CLI. See [decision 0004](decisions/0004-inbound-mcp-adapter.md) for the
design and its limits.

## Start the server and export the token

```sh
cargo run -p artifree-server -- serve --data-dir .artifree/data
export ARTIFREE_TOKEN="$(cat .artifree/data/access-token)"
```

The endpoint requires `Authorization: Bearer <token>`. It accepts only
loopback host names and refuses requests from web pages (any `Origin`
header). Keep the token out of shared configuration: both clients below read
it from the environment.

## Codex

Add the server once:

```sh
codex mcp add artifree --url http://127.0.0.1:7410/mcp --bearer-token-env-var ARTIFREE_TOKEN
```

or put the same settings in `~/.codex/config.toml`:

```toml
[mcp_servers.artifree]
url = "http://127.0.0.1:7410/mcp"
bearer_token_env_var = "ARTIFREE_TOKEN"

# Optional: allow tools without a prompt, for example in `codex exec`.
[mcp_servers.artifree.tools.create_artifact]
approval_mode = "approve"

[mcp_servers.artifree.tools.save_revision]
approval_mode = "approve"
```

Codex starts the server's tools with each session. A one-off run that does
not change your configuration can pass the same keys with `-c`:

```sh
codex exec --skip-git-repo-check \
  -c 'mcp_servers.artifree.url="http://127.0.0.1:7410/mcp"' \
  -c 'mcp_servers.artifree.bearer_token_env_var="ARTIFREE_TOKEN"' \
  'Use the artifree tools to create an artifact titled "Hello" with an
   index.html that says hello, then give me a preview link.' < /dev/null
```

## Pi

Pi's built-in MCP support reads `~/.pi/agent/mcp.json`, or `.pi/mcp.json` in a
trusted project:

```json
{
  "mcpServers": {
    "artifree": {
      "url": "http://127.0.0.1:7410/mcp",
      "headers": { "Authorization": "Bearer ${ARTIFREE_TOKEN}" },
      "exposure": "direct",
      "description": "Create, edit, and preview Artifree HTML artifacts"
    }
  }
}
```

The same entry can be added with
`pi mcp add artifree --url http://127.0.0.1:7410/mcp --bearer-token-env-var ARTIFREE_TOKEN`.
Run `pi mcp list` to connect and print the tools. `exposure: "direct"`
declares the tools to the model; Pi's default exposes them to its code mode
only. Tools appear as `mcp__artifree__<tool>`. This configuration follows
Pi's documentation; it has not been run as part of Artifree's checks.

## Tools

| Tool              | Arguments                                                    | Result                             |
| ----------------- | ------------------------------------------------------------ | ---------------------------------- |
| `list_artifacts`  | none                                                         | `{ artifacts }`, newest first      |
| `get_artifact`    | `artifact_id`                                                | artifact with `current_revision`   |
| `create_artifact` | `title`, `files`, `idempotency_key`                          | `{ artifact, revision, replayed }` |
| `save_revision`   | `artifact_id`, `base_revision`, `changes`, `idempotency_key` | `{ artifact, revision, replayed }` |
| `list_revisions`  | `artifact_id`                                                | `{ revisions }`, newest first      |
| `get_revision`    | `artifact_id`, `revision`                                    | one revision manifest              |
| `read_file`       | `artifact_id`, `revision`, `path`                            | exact bytes as `utf8` or `base64`  |
| `create_preview`  | `artifact_id`, `revision`                                    | `{ url, revision, expires_at_ms }` |

The full schemas are in
[`contracts/generated/mcp-tools.json`](../contracts/generated/mcp-tools.json).
A file is `{ "path", "encoding": "utf8" | "base64", "content" }`. A change is
`{ "op": "put", "path", "encoding", "content" }` or
`{ "op": "delete", "path" }`; unchanged files carry over.

Limits: 256 files, 5 MiB per file, 25 MiB per package, and 36 MiB for the
whole JSON request. Send binary data, or text with many control characters,
as base64: JSON escapes each control character to as many as six bytes, so
such text can exceed the request limit as `utf8` while any package within the
file limits fits as `base64`. A request over the limit is refused with HTTP
`413` before any tool runs.

A typical sequence of `tools/call` parameters:

```text
{ "name": "create_artifact", "arguments": {
    "title": "Hello",
    "files": [{ "path": "index.html", "encoding": "utf8", "content": "<h1>Hello</h1>" }],
    "idempotency_key": "2f1c9a4e-hello-create" } }

{ "name": "save_revision", "arguments": {
    "artifact_id": "art_…", "base_revision": 1,
    "changes": [{ "op": "put", "path": "style.css", "encoding": "utf8", "content": "h1 { color: teal; }" }],
    "idempotency_key": "2f1c9a4e-hello-style" } }

{ "name": "create_preview", "arguments": { "artifact_id": "art_…", "revision": 2 } }
```

## Errors and retries

A failed tool call returns `isError: true` with the HTTP API error body:

```json
{
  "error": {
    "code": "revision_conflict",
    "current_revision": 2,
    "message": "artifact changed; current revision is 2"
  }
}
```

| Code                 | What to do                                                                  |
| -------------------- | --------------------------------------------------------------------------- |
| `revision_conflict`  | Nothing was written. Read the current files, reapply, save on the new base. |
| `request_key_reused` | The key was used for different content. Use a new key for a new change.     |
| `invalid_request`    | Fix the arguments named in the message; the key is not reserved.            |
| `not_found`          | The artifact, revision, or file does not exist or is not yours.             |
| `unavailable`        | Retry the identical call with the same key.                                 |
| `internal`           | Retry the identical call with the same key to learn whether it committed.   |

Use a new `idempotency_key` for each intended change. If a call times out or
its answer is lost, resend the identical call with the same key: it returns
the original result with `replayed: true` instead of committing twice, even
after later saves. Keys are shared with the HTTP API and CLI.

A wrong or missing token is an HTTP `401` before MCP starts; an unknown tool
name is a JSON-RPC error.

## Request limits

Each actor's HTTP and MCP requests share one budget: by default 120 at once,
then 1200 per minute (20 per second), with at most 16 running at the same time
([decision 0005](decisions/0005-request-limits.md)). Beyond that the server
answers HTTP `429` with `Retry-After` and
`{ "error": { "code": "rate_limited", ... } }` before the MCP layer, so the
agent sees a transport error rather than a tool result. Nothing was done:
wait for the delay and resend the identical call with the same
`idempotency_key`. Change the limits with `--request-burst`,
`--requests-per-minute`, and `--concurrent-requests` on
`artifree-server serve`.

## What the tools do not do

Previews are sandboxed pages with no data, model, or file capabilities, and a
preview URL works for anyone who holds it until it expires. The web workspace
does not refresh by itself after an agent saves; reload it. Sharing,
publishing, structured documents, and builds are not available through MCP or
any other interface yet.
