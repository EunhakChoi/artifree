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

| Tool                       | Arguments                                                                                                    | Result                                                    |
| -------------------------- | ------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------- |
| `list_artifacts`           | optional `q`, `sort`, `pinned`, `deleted`, `limit`, `page_token`                                             | `{ artifacts, next_page_token? }`                         |
| `get_artifact`             | `artifact_id`                                                                                                | artifact with `current_revision`                          |
| `create_artifact`          | `title`, `files`, `idempotency_key`                                                                          | `{ artifact, revision, replayed }`                        |
| `save_revision`            | `artifact_id`, `base_revision`, `changes`, `idempotency_key`                                                 | `{ artifact, revision, replayed }`                        |
| `edit_files`               | `artifact_id`, `base_revision`, `edits` (each with `expect`), `idempotency_key`                              | `{ artifact, revision, replayed }`                        |
| `restore_revision`         | `artifact_id`, `revision`, `base_revision`, `idempotency_key`                                                | `{ artifact, revision, replayed }`                        |
| `duplicate_artifact`       | `artifact_id`, `revision`, optional `title`, optional `copy_data`, optional `copy_assets`, `idempotency_key` | `{ artifact, revision, copied, replayed }`                |
| `update_artifact`          | `artifact_id`, `base_metadata_revision`, `title`, `pinned`, `deleted` (at least one), `idempotency_key`      | `{ artifact, replayed }`                                  |
| `list_revisions`           | `artifact_id`                                                                                                | `{ revisions }`, newest first                             |
| `get_revision`             | `artifact_id`, `revision`                                                                                    | one revision manifest                                     |
| `read_file`                | `artifact_id`, `revision`, `path`, optional `offset` and `length`                                            | exact bytes as `utf8` or `base64`                         |
| `create_preview`           | `artifact_id`, `revision`                                                                                    | `{ url, revision, expires_at_ms }`                        |
| `register_handoff_session` | `name`                                                                                                       | `{ session_id, session_secret }`                          |
| `poll_handoffs`            | `session_id`, `session_secret`                                                                               | `{ deliveries }`                                          |
| `acknowledge_handoff`      | `session_id`, `session_secret`, `delivery_id`                                                                | receipt state                                             |
| `enqueue_job`              | `artifact_id`, `revision`, `kind` (`verify_revision`), `idempotency_key`                                     | `{ job, replayed }`                                       |
| `get_job`                  | `job_id`                                                                                                     | one job                                                   |
| `list_jobs`                | optional `artifact_id`, `limit`, `page_token`                                                                | `{ jobs, next_page_token? }`                              |
| `cancel_job`               | `job_id`, `idempotency_key`                                                                                  | `{ job, replayed }`                                       |
| `get_data`                 | `artifact_id`, optional `path`                                                                               | `{ data_version, exists, value? }`                        |
| `set_data`                 | `artifact_id`, `path`, `base_data_version`, `value` (JSON text), `idempotency_key`                           | `{ data_version, changed, replayed }`                     |
| `update_data`              | `artifact_id`, `path`, `base_data_version`, `patch` (JSON text), `idempotency_key`                           | `{ data_version, changed, replayed }`                     |
| `delete_data`              | `artifact_id`, `path`, `base_data_version`, `idempotency_key`                                                | `{ data_version, changed, replayed }`                     |
| `list_assets`              | `artifact_id`                                                                                                | `{ asset_version, assets, bytes, max_assets, max_bytes }` |
| `read_asset`               | `artifact_id`, `asset_id`                                                                                    | `{ asset_version, asset, content_base64 }`                |
| `upload_asset`             | `artifact_id`, `base_asset_version`, `content_type`, `content_base64`, `idempotency_key`                     | `{ asset_version, asset, replayed }`                      |
| `delete_asset`             | `artifact_id`, `asset_id`, `base_asset_version`, `idempotency_key`                                           | `{ asset_id, asset_version, deleted, replayed }`          |

The full schemas are in
[`contracts/generated/mcp-tools.json`](../contracts/generated/mcp-tools.json).

A file is `{ "path", "encoding": "utf8" | "base64", "content" }`. A change is
`{ "op": "put", "path", "encoding", "content" }` or
`{ "op": "delete", "path" }`; unchanged files carry over.

`list_artifacts` returns the most recently updated first, or the newest first
with `sort: "created"`, in pages of up to 500 (`limit`, default 500). Pass a
returned `next_page_token` with the same `q`, `sort`, `pinned`, and `deleted`
to read the next page. `update_artifact` renames or pins without creating a
source revision; it fails with `metadata_conflict` and
`current_metadata_revision` when `base_metadata_revision` is stale (decision
[0008](decisions/0008-artifact-metadata-and-lists.md)). With `deleted: true`
it moves the artifact to the trash: every other call then answers exactly
as for an unknown artifact (`not_found`, or the same error for arguments
invalid in themselves), except the retry of a call made before the delete,
which replays. `list_artifacts` with `deleted: true`
lists the trash, and `update_artifact` with `deleted: false` recovers an
artifact from it, under the same base and key rules. Nothing is removed;
each artifact carries `deleted_at_ms`, `null` outside the trash (decision
[0013](decisions/0013-deleting-artifacts-and-the-trash.md)).
An MCP token acts as the one user it was issued to (decision
[0036](decisions/0036-multi-user-identity.md)): it sees only that user's
artifacts, jobs, data and changes, another user's artifact or job answers
`not_found`, and there is no MCP tool to create users or tokens.
There is no MCP tool to purge an artifact for good or to clean up unreferenced
content: a purge cannot be undone, so only the HTTP API and the CLI offer it,
and over HTTP a token issued to the owner profile is refused with `403 forbidden`;
only the owner's access token may clean up, and purge is open to it and to a
token issued to another user, for that user's own trashed artifacts (decisions
[0029](decisions/0029-retention-and-cleanup.md) and
[0036](decisions/0036-multi-user-identity.md)). The `not_in_trash`
and `content_conflict` error codes of the shared error schema are therefore
never returned over MCP, and
a purged artifact answers every MCP call as an unknown one (`not_found`).
`edit_files` changes some files even if the artifact moved on since
`base_revision`, the revision you read (decision
[0035](decisions/0035-per-file-conditional-edit.md)). An edit is a change
(`put` with its content, or `delete`) plus `expect`: the lowercase SHA-256 the
file has now, as `get_revision` lists it, or `absent` for a put of a file that
must not exist (a delete takes a digest only). Each `expect` is judged against
the file in the **current** revision, not in `base_revision`; files no edit
names carry over as they are now. If any one does not hold, nothing is written
and the call fails with `file_conflict`, `current_revision`, and
`conflicting_files` (at most 32, in path order, each with its `path` and
`current_sha256`, which is `null` when the file is gone) with
`conflicting_files_total`; read those files again and edit again. It never
merges: a file is replaced whole. `save_revision` is unchanged and still fails
with `revision_conflict` whenever the head moved. The same key and edits
replay the first result, also after the head moved; a rejected call does not
use its key. Editing a file to the bytes it has fails with `invalid_request`.
`restore_revision` commits an earlier revision's files, byte for byte, as the
next revision under the same `base_revision` and key rules as a save; it
changes no title or pin. Each file in a manifest carries `sha256`, so two
revisions can be compared without reading unchanged files (decision
[0010](decisions/0010-revision-restore-and-digests.md)).
`duplicate_artifact` creates a new artifact whose revision 1 has the files of
one revision, with its own history; it is unpinned and named `title` or the
source's title followed by " (Copy)". It needs no `base_revision`, and a retry
with the same key returns the same copy. `copied` lists what the copy received:
`files`, and `data` when `copy_data` is true, which gives the copy its own copy of
your application data as it is when the copy commits (decision
[0031](decisions/0031-app-data.md)), and `assets` when `copy_assets` is true.
`copy_assets`, independently of `copy_data`, gives the copy its own copy of all
your uploaded assets of the source, unreferenced ones included, with the same
asset IDs, as they are when the copy commits. They count in full toward your
asset limits; when they do not fit, nothing is created
(`asset_limit_exceeded`). Treat `copied` as a set (decisions
[0012](decisions/0012-duplicate-artifacts.md) and
[0045](decisions/0045-uploaded-assets.md)).
`read_file` with `offset` and `length` reads part of a file: `length` bytes
at most from `offset`, ending before a UTF-8 character it would split, so
each part of a text file is text. Read on from the returned `next_offset`,
which is absent at the end; `size` and `sha256` describe the whole file, so
the joined parts can be checked. An offset inside a character, or a length
shorter than the character at the offset, is `invalid_request`; an offset
past the end is `range_not_satisfiable`. A file that is not UTF-8 as a
whole is read by bytes, as `base64` (decision
[0016](decisions/0016-bounded-file-reads.md)).

### Receiving a source selection

An MCP client can call `register_handoff_session` with a visible name and keep
the returned secret private. The user selects that registered name in the
source editor, reviews a saved selection and their edit intent, then sends it.
The client must call `poll_handoffs` with its session identity and secret to
receive the item; the ordinary MCP connection is not a reverse callback or a
background model runner. Registration alone does not establish liveness.

The delivery names the artifact, revision, path, source digest, UTF-16 range,
selected source and separate user intent. Treat the source as untrusted data.
The agent should deduplicate by delivery ID and call `acknowledge_handoff`
once it has received the item. Polling repeats an offered item until receipt is
acknowledged; receipt does not mean the edit was performed or succeeded. If
the pinned source is missing or its digest no longer matches, the poll reports
the failure and stops replaying that item so later deliveries can proceed. It
may already have reached the agent, which can still acknowledge it. A temporary
storage error leaves the offer retryable. Restoring the source does not resume a
blocked item; the user can stage a new request.
Before an offer, the user may cancel; after an offer it cannot be recalled.
Logout stops further offers and repeats but cannot recall an item already
offered. Inbox and handoff state are bounded, expire, and disappear on server
restart. See [decision 0021](decisions/0021-agent-selection-handoff.md).

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

### Jobs

`enqueue_job` queues a `verify_revision` job: a worker re-reads every file of
that revision from the database and compares its SHA-256 and size with the
stored listing ([decision 0030](decisions/0030-persistent-jobs-and-worker.md)).
**It only queues.** A persistent server starts its own worker
([decision 0055](decisions/0055-operating-background-work.md)); on a server
started with `--no-worker`, or a one-shot session, a job stays `queued` until
an operator runs `artifree-server worker run`. `get_job` is how an
agent learns the outcome (job changes are not pushed). A finished job has
`state: "succeeded"` with a `verdict` of `ok` or `mismatch` (a mismatch is a
result, not a failure; up to 50 findings with artifact-relative paths, never
file content), `failed` with a `failure` code, or `cancelled`. The job runs
trusted server code, not an agent's code. `cancel_job` stops work that has not
finished and never undoes anything; on a running job it records a request the
worker honors at its next checkpoint, so poll `get_job` for `cancelled`. Retry
rules are those of every write: resend the identical call with the same
`idempotency_key`; the answer shows the job as it is now with `replayed: true`.
An artifact in the trash or purged, and another actor's job, answer
`not_found`; the open jobs of an artifact in the trash wait untouched, are
not counted toward the limit, and run again if the artifact is recovered. At
most 16 jobs may be open per actor, the owner's and all agent tokens' together
(`too_many_jobs`, 409); a
cancel of a finished job is `job_ended` (409).

### Application data

An artifact can keep one JSON object of data for you, with its own
`data_version` (0 until written) that has nothing to do with its revisions: saving
or restoring a revision does not change it, and it is not in a revision, a
preview, or an export. **A page running in a preview can read it only after the user grants it
`data.read` and change it only after the user grants it `data.write`**
([decision 0053](decisions/0053-runtime-app-data.md)), so a value you saved
may be replaced by the page; compare `data_version` before you write. Otherwise only these tools, HTTP,
and the CLI reach it ([decision 0031](decisions/0031-app-data.md)).

`get_data` reads the value at a `path`: up to 16 segments of 1-64 UTF-8 bytes,
using the Unicode 16.0.0 [data path grammar](data-paths.md), separated by `/`,
such as `todos/first` or `한글/값`. Paths preserve exact text, with no Unicode
normalization or URL decoding. No path, or the empty one, is
the root. A path addresses object members and never enters an array. A path with
nothing at it succeeds with `exists: false`. Read `data_version` and send it as
`base_data_version` in the next write; if another write won, the call fails with
`data_conflict` and `current_data_version`, and nothing is written. A write that
changes nothing keeps the version (`changed: false`).

`set_data` replaces the value at the path and makes the objects above it that are
missing; the root must be given an object, and `null` can be stored. `update_data`
applies a JSON Merge Patch (RFC 7396): objects merge member by member, a `null`
member is removed, arrays and scalars replace what is there wholesale (an array is
never merged inside), and a patch cannot store `null` in an object. At the root the
patch must be an object: a `null` patch there is `invalid_request`, and
`set_data` with `{}` empties everything. `delete_data`
removes the value at the path (the empty path empties everything); removing what
is not there succeeds and changes nothing. A value in the way that is not an
object is `data_path_conflict`.

`value` and `patch` are **JSON text strings**, such as `"{\"done\":false}"`, not
nested JSON, so that the server sees exactly what you wrote: a repeated member
name or a number with a magnitude beyond 2^53-1 in any spelling (`9007199254740993`,
`9007199254740993.0`, `1e16`, `-9223372036854775808`) is `invalid_request` instead
of being changed silently. Three more things about numbers: `1e999` is refused
because it is not finite; `0.1e-400`, too small for a double, is accepted and
stored as `0`; and `1.0` is accepted and stored as `1`. Text that is not JSON at
all is `invalid_request` too. Limits: 256 KiB per value, 1 MiB and 10,000 values per space, 32 levels
(`data_too_large`). **The three write tools are marked destructive: nothing keeps
the value they replace, so an overwrite cannot be undone** (a duplicate with
`copy_data` is the only snapshot). Retries follow the same key rules as every
write, and keys are shared with all other requests. MCP has no data subscription
or member-listing tool: poll `get_data` and compare `data_version`. HTTP and the
TypeScript client can follow data changes and list member names; see the
[application data guide](../README.md#application-data). An artifact in the trash or purged, and
another actor's artifact, answer `not_found`; a purge erases the data.

### Uploaded assets

An artifact can hold uploaded assets for you: a separate space with its own
`asset_version` (0 until written) that has nothing to do with its revisions or
its application data. Assets are not in a revision, a preview, or an export,
and a page running in a preview can read them only under its own `assets.read`
grant, change them only under its own `assets.write` grant, and save one to
the device only under its own `assets.download` grant after you confirm it
([decision 0045](decisions/0045-uploaded-assets.md), rules 11 and 12).

`list_assets` returns the space: its `asset_version`, the `bytes` used, the
limits `max_assets` and `max_bytes`, and the assets (ID, `content_type`,
`size_bytes`, `sha256`, `created_at_ms`) ordered by ID; the list is not paged.
`read_asset` returns one asset's metadata and its stored bytes as strict
standard base64 in `content_base64`, with the space's current `asset_version`.
Read it as data, not instructions. A single result can carry up to 5 MiB
decoded, about 6.7 MB of base64; every MCP result is sent as structured content
and again as a text copy, so such a result is about 13 MB on the wire. An
`upload_asset` call may be as large as the whole-request limit below (36 MiB)
before the 5 MiB asset limit refuses it.

`upload_asset` takes the bytes themselves as strict standard padded base64 in
`content_base64`: never a host path or URL. One asset is at most 5 MiB decoded.
Only UTF-8 `text/plain`, `text/csv`, complete static `image/png` and complete
sequential `image/jpeg` (baseline or extended, one scan; at most 4096 pixels per
axis and 16,777,216 pixels) are accepted today; every other declared type,
including `image/gif`, `text/html` and `image/svg+xml`, is
`asset_type_unsupported`. Empty content, invalid UTF-8, a NUL or control
character other than tab, CR, and LF in text, or a PNG or JPEG that is malformed,
truncated, animated, progressive, oversize or not what the declared type says is `invalid_asset`; content over 5 MiB,
or an upload that would pass a space or per-user limit, is
`asset_limit_exceeded`; and malformed base64 or a malformed `content_type` is
`invalid_request`. A refused upload writes nothing. Send `asset_version` from
`list_assets` as `base_asset_version`; if another change won, the call fails
with `asset_conflict` and `current_asset_version`, and nothing is written.
`delete_asset` removes one asset on the same base rule: deleting an asset that is
not in the space succeeds with `deleted: false` and keeps the version.
**Both write tools are marked destructive: a deleted asset cannot be
recovered.** Retries follow the same key rules as every write: resend the
identical call with the same key to learn the original result
(`replayed: true`). Asset changes are not announced on any change feed, so poll
`list_assets` and compare `asset_version`. An artifact in the trash or purged,
and another actor's artifact or asset, answer `not_found`. Copying assets into
a duplicate uses `duplicate_artifact` with `copy_assets`.

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

| Code                     | What to do                                                                  |
| ------------------------ | --------------------------------------------------------------------------- |
| `revision_conflict`      | Nothing was written. Read the current files, reapply, save on the new base. |
| `data_conflict`          | `current_data_version` is newer. Read the data, reapply, write on that.     |
| `data_path_conflict`     | A value in the path is not an object. Read it, then set or pick a path.     |
| `data_too_large`         | A value, the data, or its nesting is over the limit. Send less.             |
| `asset_conflict`         | `current_asset_version` is newer. List the assets again, then write on it.  |
| `asset_limit_exceeded`   | The asset or a space/user limit is over. Send less or delete assets.        |
| `asset_type_unsupported` | Only `text/plain`, `text/csv`, `image/png` and `image/jpeg` are accepted.   |
| `invalid_asset`          | The bytes break the content rules (not UTF-8, bad PNG, unsupported JPEG).   |
| `request_key_reused`     | The key was used for different content. Use a new key for a new change.     |
| `invalid_request`        | Fix the arguments named in the message; the key is not reserved.            |
| `not_found`              | The artifact, revision, file, asset, or job does not exist or is not yours. |
| `too_many_jobs`          | Sixteen jobs are open. Wait for or cancel one, then enqueue again.          |
| `job_ended`              | The job already finished; nothing to cancel.                                |
| `unavailable`            | Retry the identical call with the same key.                                 |
| `internal`               | Retry the identical call with the same key to learn whether it committed.   |

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
agent sees a transport error rather than a tool result. This attempt was not
processed: wait for the delay and resend the identical call with the same
`idempotency_key`, which returns the original result if an earlier attempt
committed. Change the limits with `--request-burst`,
`--requests-per-minute`, and `--concurrent-requests` on
`artifree-server serve`.

## What the tools do not do

The MCP preview URL opens a sandboxed page without the workspace runtime
bridge or app-data, model, or file capabilities. The URL works for anyone
who holds it until it expires. A separate, opted-in workspace preview can
read its own artifact metadata (decision [0019](decisions/0019-runtime-metadata-bridge.md))
and, after the user grants each, list and read its own source and ask the
user to save a file (decision [0032](decisions/0032-runtime-source-read-and-download.md)),
and, after a separate edit grant, save one file of its own artifact as a new
revision (decision [0033](decisions/0033-runtime-source-write.md)).
The room and presence of decision [0034](decisions/0034-room-and-presence.md)
is HTTP and client only: an agent has no runtime connection (tab), so **no MCP
tool was added** for it.
That bridge is a connection grant to the web page. **No MCP tool was added or
changed for it**, and an agent token gives a preview nothing. An open web
workspace shows an agent's saves as they are committed (decision
[0007](decisions/0007-live-web-workspace.md)); an unsaved draft there is kept
and marked as outdated. Sharing, publishing, structured documents, and builds
are not available through MCP or any other interface yet. The only job is a
read-only integrity check; no job runs an agent's code.
