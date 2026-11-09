# Quality gates

Status: early development, 2026-10-31. The checks below protect the local HTML
artifact slice ([decision 0003](decisions/0003-local-html-artifact-slice.md))
and the foundation around it. Passing them implies nothing about planned
features.

## Enforced checks

| Area         | Enforcement                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| ------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| TypeScript   | Strict types, checked dependency declarations, exact optional properties, unchecked indexes, side-effect imports, missing returns, fallthrough, unreachable code                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| Code lint    | Oxlint correctness, hooks, JSX accessibility, import cycles, explicit `any`, non-null assertions; zero warnings                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| UI design    | Six `@shadcn/lint` rules: component styling, semantic colors, arbitrary values, inline styles, unknown classes, readable static classes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| CSS          | Stylelint recommended validity checks, duplicate selectors/properties, unknown values/properties, Tailwind-aware at-rules; zero warnings                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| Rust         | Unsafe forbidden, unused results denied, public documentation required, documented stdout/stderr exceptions only in CLI binaries, pedantic Clippy, rustfmt, rustdoc warnings denied                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| Architecture | Exact Rust workspace membership and allowed dependencies, including build/dev edges; TypeScript package/import/re-export/type/lazy-import boundaries; no computed imports or source symlinks                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| Contracts    | Rust DTO generation, JSON Schema validity, unknown-field/type/value rejection, stable public TypeScript aliases, byte-for-byte generated drift; MCP tool schemas self-contained, standard-format, closed                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| Inventory    | Portable relative paths, existing sources, unique keys, no repository escape through symlinks; a named framework or runtime must be a real server dependency                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| Artifacts    | Path traversal and portability, size/count limits, stale-base conflicts, concurrent writers, request-key replay and reuse, injected commit failure, crash and restart, owner-only access, preview sandbox, per-actor request rate and concurrency limits, change feed delivery, resumption, and reset                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| CLI          | Success and failure exit codes, machine-readable stdout, stderr diagnostics, contradictory flags, unknown commands                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| Browsers     | Chromium (Firefox suspended, see below) against a real server: connect, create, sandboxed preview probes, save, conflict, pinned revisions, following committed changes, search, pages, rename, pin, compare, restore, duplicate; module tabs, keyboard, dialogs, light/dark accessibility, mobile, offline HTML                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| Dependencies | npm advisory scan at every severity, Cargo advisories/yanks/licenses/sources/wildcards/duplicate versions for shipped native targets; no blanket advisory exceptions                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| Packaging    | Release archives built on Linux and Windows are unpacked into an empty directory on a fresh runner and started with an empty PATH: manifest hashes, no unlisted files, health, packaged web page, create/save/preview with the bundled CLI, restart persistence, refusal of a bad web directory and of equal API and preview origins; wrong-platform binaries are refused at packaging. The container image built from the Linux archive (decision 0026): digest-pinned base, non-root user, volume, ports, health check, read-only run, SIGTERM exit 0, restart on the same volume, MCP refused on a published port, and two refused configurations (Docker runs in CI only). Backup and restore (decision 0027): schema 1–5 fixtures upgrade to a schema equal to a fresh one, refused opens leave files byte-identical, every bad database is refused by both commands with nothing left behind, a backup under concurrent writes restores a contiguous prefix with bytes, key replay and tokens, the restored feed identity is new, the CLI protocol, and in the packaged binary a running server is backed up, restored, served, and a restore onto an existing path is refused Release supply chain (decision 0037): every archive carries `LICENSES/` (texts of the Rust crates and npm packages, deduplicated by hash, indexed) and packaging fails for a component without a text or a recorded exception, with tamper checks in the manifest verifier; the Linux binary's required `GLIBC_*` versions are read from the ELF file and may not exceed the 2.34 floor (fixture tests for pass, fail, non-ELF, truncated, and no-glibc files); the `Debian 12 (glibc 2.36)` job runs the verifier on the archive inside Debian 12 with no network; base image pins must be `name:tag@sha256:<digest>` and consistent (`npm run check:digests`, offline; the resolving mode is human-only and refuses to run in CI); the release helpers (tag equals version, latest Quality gate run, checksum list) are unit-tested; `release.yml` publishes only from a version tag push of this repository and is checked by the policy in `workflow-policy.mjs` (no pull-request or branch trigger, explicit least permissions with signing and write rights only in the two publishing jobs, no repository secret, no ref data in scripts). The release workflow itself has never run |
| CI           | Actionlint syntax/expressions (both workflows), immutable action commits, read-only token defaults, no write token in `ci.yml`, timeouts, no ignored failures, aggregate includes every verification job                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |

The formatter owns formatting. CSS lint uses the recommended correctness preset
rather than imposing a second formatting convention. Tailwind's `@apply` needs a
specific syntax exception. The CSS specificity heuristic is disabled because it
cannot infer the standalone document's disjoint component trees; duplicate
selectors, invalid declarations, and unknown properties remain errors.

Inside UI component definitions, restyling, arbitrary-value, and static-class
rules are disabled deliberately: that directory owns component appearance and
forwards caller classes. Semantic-color, inline-style, and unknown-class checks
remain enabled there. Application call sites retain all six rules.

### Browser engines

CI runs the browser jobs in Chromium only. Firefox is suspended until the
planned feature set is complete, because the time cost is high and current use
is almost entirely Chromium. Before the first public release it returns to the
CI matrix and every Firefox-specific acceptance is re-run at once. Until then:

- A specification or roadmap acceptance that names "Chromium and Firefox" is
  met in Chromium. The completion report says Firefox is pending re-verification.
- Slices merged before the suspension ran in both engines; their records stay
  as written.
- The scripts keep `ARTIFREE_BROWSER=firefox`, so a change that touches
  engine-specific APIs (IndexedDB, Web Locks, downloads, printing) can still
  be run in Firefox locally when its author wants to.
- Re-adding Firefox to the CI matrix and passing it is a precondition of the
  first public release (O03c). The release workflow refuses a tag while
  `ci.yml` has no Firefox entry in its browser matrix.

The Chromium smoke runs as two shards, chosen with `ARTIFREE_SMOKE_SHARD`:
`outbox` runs the outbox checks and `ui` runs everything else. CI runs them as
separate jobs, and each shard has its own server, data directory, and evidence
folder (`.artifree/qa/<browser>-<shard>/`). `npm run test:smoke:parallel` runs
both shards at once on one machine and fails if either fails. The whole suite
still runs in one process with `npm run test:smoke`, which is what happens when
the variable is unset or `all`. CI runs only the shards, so the `ui` checks start
on a server the outbox checks have not used; a check must not depend on running
after another shard.

## Checks must be capable of failing

`npm run test:tooling` runs real negative inputs through the actual lint config,
schema, architecture policy, and workflow policy. It verifies that all six UI
rules still produce errors; a zero-file lint run is a failure. It also catches
unpinned actions, ignored failures, and a new job omitted from the aggregate.
The Rust tests execute Serde rejection and the native descriptor CLI directly,
and check that each response schema is published as the server writes it,
with every field it always writes required.
No domain test exists merely to repeat a field declaration.

How a new test is shown to fail, and how heavy runs are isolated, is described
in [testing practice](testing-practice.md).

Artifact behavior is tested at the boundary that owns it:

- `crates/artifree-domain`: path, identifier, and package validation.
- `crates/artifree-core/src/parts.rs`: parts of a file cut exactly at bytes
  with the whole file's size and digest; the end as an empty part and past
  it refused; for every offset and length over a text of 1- to 4-byte
  characters, a byte order mark, and CRLF, a part that ends between
  characters, a start inside one refused, and a length too short for one
  refused, with the parts read from each next offset joining into the file;
  a file that is not UTF-8 cut at bytes.
- `crates/artifree-server/src/ranges.rs`: every form of one byte range,
  with case, whitespace, empty list elements, `-0`, and positions past 64
  bits; the whole file without `Range`, for another unit, and with
  `If-Range`; several ranges in one line or several, an inverted range,
  and malformed fields refused.
- `crates/artifree-providers/src/archive.rs`: the ZIP writer's CRC-32
  against the standard check value, entry dates in UTC within the format's
  range (a leap day, odd seconds, before 1980, past 2107), entries stored in
  order at the offsets the directory lists, the same bytes for the same
  entries, and names, counts, and sizes the classic format cannot hold
  refused.
- `crates/artifree-providers/tests/sqlite_store.rs`: the store contract on a
  real database — reopen durability, one revision read alone exactly as the
  history lists it, replay, key reuse, conflicts, eight
  concurrent writers, a trigger-injected failure that must leave no partial
  revision, content sharing, and newer-schema refusal; in the trash, a write
  that is its replay or not found, a key used for another request and a copy
  of the artifact included, a listing of the trash alone, and a recovery as
  the one update that reaches it, conditional and keyed, that keeps every
  byte, with `deleted` and `recovered` feed entries; one feed entry per
  commit and none for replays, conflicts, reused keys, or a failed commit,
  a commit that fails with its feed entry, owner-only pages that end at the
  head, a feed identity kept on reopening and different in another
  database, a version 1 database that gains the feed and keeps its
  data, and commit times that follow commit order after the clock is set
  back, in migrated version 1 and 2 databases whose stored times are ahead
  of the clock and out of order.
- `crates/artifree-server/tests/`: HTTP status and error codes, including
  unsupported methods; the bearer token on every route and unmatched path, with
  a case-insensitive scheme; unsafe paths, body limits, pinned reads, preview
  tickets and CSP, expiry, owner-only access in the core, and (on Unix) an
  owner-only data directory, refusing one other users can access; a restore
  that commits an earlier revision's exact files and digests and keeps the
  title, replays after later saves, and refuses a reused key, a stale base
  (also one whose files are those restored), a missing revision, an
  unchanged restore, and a missing key without writing; a duplicate of an
  earlier revision as revision 1 of an unpinned copy named "… (Copy)" with
  the same digests, later saves to either artifact that leave the other
  as it was, a title of the caller's, a replay after the source was
  renamed and revised, a reused key, and a missing revision, invalid
  title, unknown field, or missing key that create nothing; an artifact in
  the trash answers every read and write exactly as the same request to an
  unknown artifact, checks that fail before the store, reused keys, and
  requests invalid in themselves included, also
  through a preview link issued before, and is listed only in the trash;
  requests made before the delete replay as of their request, and a
  recovery is conditional, keyed apart from the delete, and brings back the
  artifact and its link; non-owners cannot delete; an export is a ZIP of
  the pinned revision, manifest first, with the same bytes each time, named
  after the revision, and missing for revision 0, a later revision, a
  trashed artifact, and other actors; a byte range of a pinned file as
  `206` with its `Content-Range` and the headers of a whole read, also
  inside a UTF-8 character; ranges that select no bytes as `416` with the
  size, malformed and several ranges as `400`, the whole file without a
  byte range or with `If-Range`, and a missing file, revision, or trashed
  artifact refused before any range, telling nothing of the size.
- `crates/artifree-server/tests/serve_shutdown.rs`: one real Unix stop signal
  sent inside the readiness callback gracefully stops all three listeners,
  even though their shutdown futures start later. The shared latch also has
  a unit test for waiters created after the stop request.
- `scripts/tests/server.test.mjs`: the CLI against a spawned server, exact
  bytes through upload and `cat`, uploads that JSON escaping would inflate past
  the body limit, refused dot segments, a base newer than current reported as
  the same conflict for every save form, every JSON response validated against
  the API schema root (which must reject an empty object) with its expected
  status, a restore through the CLI with exact bytes, its replay, and its
  conflict and usage errors, a duplicate through the CLI with exact bytes,
  its replay, a title, a missing revision, and usage errors, a delete,
  `list --deleted`, and a recovery through the CLI with a replay, a conflict,
  and usage errors, an export through the CLI read back with Node's own
  CRC-32 (`scripts/lib/zip-entries.mjs`): every file at its path with its
  exact bytes and mode, a manifest valid against its schema that equals the
  revision's listing, entries dated at the commit, the same bytes on a
  second export, an existing file never replaced, and a missing revision
  that writes nothing, `cat --offset/--length` writing exact parts, a
  range past the end refused with nothing written, and invalid range
  flags as usage errors, and a SIGKILL after commit
  followed by a restart and a replayed retry.
- `crates/artifree-server/tests/mcp_adapter.rs`: the MCP endpoint
  authenticates before anything else, refuses a request with an `Origin` or a
  foreign `Host` and a server stream, reports another owner's artifact as not
  found through every tool without leaking content, scopes retry keys to the
  actor, and answers an unknown tool with a protocol error.
- `scripts/tests/mcp.test.mjs`: the official MCP TypeScript SDK against a
  spawned server: the 2.x client in the legacy and 2026-07-28 protocol eras,
  and the 1.x client, which validates error results against output schemas. `tools/list`
  equals the generated catalog; an agent creates, reads, saves, lists history,
  reads exact pinned bytes (BOM, CRLF, binary), opens a sandboxed preview,
  restores an earlier revision byte for byte, replayed over HTTP, and
  duplicates a revision with binary bytes intact, replayed over HTTP;
  deletes an artifact that every other tool and HTTP then report as unknown,
  lists the trash, replays the delete, and recovers it with its bytes;
  reads a text file in parts that end between characters and join into it,
  with the whole file's size and SHA-256, the end as an empty part, a
  binary file's parts as base64, and offsets inside a character, lengths
  too short for one, offsets past the end, and invalid numbers refused;
  results validate against the API schema; retries replay, reordered changes
  are the same request, a reused key is refused, and keys are shared with
  HTTP; stale bases conflict without partial writes; invalid arguments are
  readable tool errors that do not reserve their key; missing or wrong tokens
  and page origins are refused; the largest file fits and an oversized body is
  refused.
- `crates/artifree-server/src/limits.rs` and
  `crates/artifree-server/tests/request_limits.rs`: the burst, the rate, and
  recovery after time passes; concurrency slots released after a response;
  separate actors; zero limits rejected; HTTP and MCP drawing on one budget;
  unauthenticated requests refused first without spending it; `429
rate_limited` with `Retry-After`; a refused create whose key later commits
  with `replayed: false`; a request abandoned after admission that keeps
  its slot until its work ends and still commits; and refused requests
  cancelled while their body is read, which release their place among the
  bodies being read. Refusals with `413` that drain the unread
  body (decision 0048) are covered there too.
- `crates/artifree-server/tests/change_feed.rs`: a stream starts with `ready`
  at the head and sends each commit once and nothing for a replay or
  conflict; deletes and recoveries arrive as `deleted` and `recovered`, and
  a write refused in the trash sends nothing; a stream resumes after its cursor; a cursor of another feed or
  beyond the head is reset to the head; a malformed cursor and a token in the
  query string are refused without taking a stream slot; streams beyond the
  per-actor cap get `429` with `Retry-After: 5` until a reader leaves; and
  closing the server ends open streams and those opened later.
- `scripts/tests/changes.test.mjs`: the TypeScript client against the server
  binary: an MCP agent's commits reach a follower once, with event data
  validated against the schema; a follower crosses a SIGKILL and restart and
  receives the commit made while it was away; a cursor of another database
  is reset although that database is past it; a commit by a second process
  on the same database arrives within the poll interval; shutdown ends open
  streams (on Unix: Node cannot send a graceful stop signal to a child on
  Windows); and an invalid `--change-streams` stops the server.
- `scripts/tests/refused-body-drain.test.mjs`: a raw socket sends a body over
  the request limit to `/mcp` and `/v1/artifacts` and must read the `413`
  (decision 0048).
- `scripts/tests/limits.test.mjs`: the server binary with small limits. The
  official MCP client and the CLI share the budget, a refused create commits
  nothing and its key works after the delay. A fixed HTTP `429` fixture checks
  the CLI's exact retry delay and key diagnostic independently of process
  startup time; the first commit and the replay after that injected refusal
  still use the real server. Separate fixture cases check `Retry-After` values
  of 1 and 7 seconds without waiting. The real server tests also check that a
  refused 30 MiB upload still receives the `429`, a refused body that never
  ends is answered within the drain bound and immediately once 32 are being
  read, a request held mid-body
  keeps its concurrency slot until it finishes, and invalid limit flags stop
  the server before it listens.
- `scripts/tests/client.test.mjs`: the API client with an injected `fetch`: a
  body cut off mid-stream is a network error, a complete foreign body is an
  invalid response, uploads pick the smaller exact encoding, a restore sends
  its revision, base, and key, a manifest without a valid lowercase
  `sha256` is an invalid response, and a duplicate whose `copied` is missing
  or names a part the client does not know is an invalid response, as is an
  artifact whose `deleted_at_ms` is missing or not a time, and an export
  that is not `application/zip`; an export cut off mid-body is a network
  error. For parts of files: every range within 5 bytes answered with
  every `Content-Range` of positions up to 5 and a body of its length or
  one byte off, where only the exact part asked for resolves, with the
  `Range` it sent; a `200`, a missing, unknown-size, multiple, or
  malformed `Content-Range` as invalid responses; `416` as
  `range_not_satisfiable`; and ranges that cannot be asked for refused
  before sending. For the
  change feed: the event stream rules (line ends split across chunks or
  ended by a final CR, a byte order mark, a character split across chunks,
  comments, unknown fields and events, joined data lines, an event without
  data or without its blank line); deletes and recoveries arriving as
  themselves and an unknown change kind arriving as
  `unknown` while broken data is refused; reconnecting from the last cursor
  after a lost connection, an empty `502`, an HTML `504`, `503`, and `429`
  with their delays; stopping on `401`, `400`, an HTML `404`, a successful
  page that is not an event stream, and broken event data; a proxy's
  `Retry-After` honored with a non-API body; a handler error that ends
  following instead of a retry; and an abort that ends a retry wait at once,
  cancels an open stream, and stops events already received in the same
  chunk. A proxy's `429` without an API body is still `rate_limited`, and a
  read's signal reaches `fetch`.
- `scripts/tests/client-transport.test.mjs`: the client's transport against a
  fake server, clock, and random source (decision 0040 rules 17 to 19). The
  event-stream parser at its bounds (a line of 65,536 units passes and 65,537
  fails, an event likewise, an endless line, a chunk refused before it is
  split, one-unit chunks in linear time); reads of a success and an error body
  at their limits, cancelled as the data arrives; the 45-second stall limit (a
  comment every 15 seconds prevents it, an empty chunk does not count, no timer
  before the headers for an hour, a consumer that holds a chunk is not a stall,
  an unread stream releases its timer); an error body that stalls or is lost
  counts as no API body, so a `429` keeps its `Retry-After` and a `401` or
  `404` stops; the delays with the random source at 0 and near 1, the cap, the
  `Retry-After` floor and its one-hour clamp; the count reset only after 30
  seconds open, apart from the configured maximum; abort at every point with no
  timer or listener left; and a body whose cancel or read never settles, a
  clock that calls back at once, and one that calls back late. Each guard was
  checked by breaking it.
- `scripts/tests/request-gate.test.mjs`: bundles the web workspace's request
  gate with esbuild and drives it with mock timers, a fake visibility source,
  and a server the test answers by hand: reads go one at a time in order
  while writes are not held; a read keeps its turn until its answer's body
  was read, and an abandoned read keeps it until it ends without being
  cancelled; a withdrawn read is never sent; a hidden page sends nothing,
  also a read asked for before it was hidden; a `429` on a read, the stream,
  or a write, with or without an API body or `Retry-After`, delays every
  read, and the refused read is sent again first; a delay is never shortened,
  ended by the stall bound, or ended while hidden; a stalled read frees its
  turn once; and the gate listens to visibility only while reads wait.
- Runtime metadata: the real HTTP routers and the ticket registry tests check
  the view's actor, artifact,
  source revision, generation, initialization, expiry, current access, and
  replacement. A close before issuance fences the generation, including an
  actual storage lookup still in flight and a browser that leaves its preview
  while the create is held. A fence remains while its create is pending even
  after its ordinary lifetime. An ordinary entry retains its bytes and reloads normally; an
  opted-in entry receives its bootstrap once and its source export declares
  the workspace dependency. A real storage read held across a generation
  replacement must refuse its late result. The client refuses malformed or mismatched view
  and metadata responses. The current server has one local actor.
  `scripts/smoke-runtime.mjs` runs Chromium (and
  Firefox, suspended in CI; see Browser engines) against a real server and sandboxed frame: metadata reaches the
  frame without the account token, a wrong initialization and a different
  opaque window are refused, an expired host message sends no RPC, and a
  successful response held across frame navigation is not delivered afterwards.
- Runtime grants: tests exercise declarations and SDK pins, unsupported and
  undeclared grants, a user choice before metadata access, compare-and-set
  conflicts, revocation and regrant, source head changes, session expiry and
  logout. The real service tests hold grant, view and metadata reads across
  revocation or close, and HTTP tests exercise the same authorization through
  the router. Browser checks cover separate connections, a lost close
  acknowledgement, and an unknown grant carried across a view change until a
  confirmed clear fences its delayed request. A connection created after server restart has a fresh ID;
  the tests do not claim multi-user authentication.
- Runtime source reading and download requests (decision 0032): Rust tests
  assert, for each capability separately, that a binding failure is `404`, a
  missing grant after a matching binding is `403`, a moved head is `409`, and a
  trashed artifact is `404`, and that a revoke landing after the bytes were
  read releases none of them. Parts are joined by digest (a non-ASCII text cut
  inside a character, a binary file, an empty file), hostile, oversized,
  nested and duplicate-field bodies get fixed messages, a flood of frame calls
  leaves owner requests and the revoke served, a holder of `source.download`
  cannot have more than two downloads built at once, every path-taking route
  judges the shared path fixture, and a restart revokes. Node
  tests run one fixture of accepted and refused parts through the client and the
  frame script and the shared path fixture through the frame script and the host; check that a file download is the offered digest
  (or is refused, also without `crypto.subtle`); classify every request kind
  under server addresses with and without a path prefix; answer a call the host
  cannot parse at once; hit the frame script's public methods with `null`,
  arrays, primitives, `Proxy` objects, revoked proxies and throwing getters
  without a leaked pending slot; and drive the host's token bucket, offer gate
  (delay, trust, one at a time, cooldown, every withdrawal leaving no timer or
  listener), call parser, pending cap and object-URL lifetime on an injected
  clock. `scripts/smoke-source.mjs` runs Chromium (Firefox suspended in CI): the bytes a frame
  read and the files the browser saved (a non-ASCII text, a binary file, the
  archive) are compared by digest with the stored bytes; no grant, a decline's
  cooldown, and a page whose snapshot still says "granted" after the server
  revoked it open no prompt; an old generation and a moved head are refused.
  The view recorder copies the real fetch response before the workspace receives
  it and matches its URL to the shown frame; body-read failures retain their
  request path and status. Its tests check ordering, preservation of the
  application's body, and visible stream/JSON failures.
  Each guard was checked by breaking it and watching a test fail. These tests
  do not show that a preview cannot use the network, and no MCP tool is involved.
- Runtime source writing (decision 0033): Rust tests read the new revision
  back from the store and compare its bytes with the bytes the frame sent (a
  non-ASCII text, a new path, the largest binary file), check that the other
  files and the earlier revision are unchanged, and that no revision exists after
  a missing binding, a missing edit grant, a bad path (the shared fixture), bad
  base64, an over-limit file, a view that is not the head (`409` with the
  current revision), or another actor's token. Authorization is judged before
  the arguments. A retry of a save returns its first receipt and makes no
  second revision; the same retry identity in another artifact, view or
  generation is another request; no key an owner or an agent can send parses into
  the frame's namespace; a revoke after the write was authorized withholds the
  receipt and not the revision. The decision-making function that compares the
  new revision with the request has a case for each way it can differ. Node
  tests give the client, the frame script and the host a receipt that differs in
  each field, and one for other bytes with a correct digest of its own; run
  hostile save arguments, a platform without `crypto.subtle`, the retry rule held
  to the client's, the schema bounds, and the route's class. `scripts/smoke-source.mjs`
  saves from a frame in Chromium (Firefox suspended in CI): the stored bytes and digest of the
  new revision equal what the frame sent, the refusals make no revision, and the
  next view starts without the grant. These tests do not show roles (there are
  none), that a preview cannot use the network, or anything about MCP.
- Rooms and presence (decision 0034): Rust tests run the room on an injected
  clock (no sleeps) and over HTTP with a manual clock: the `isMe`/`sameTab` truth
  table for a page, its frame and another tab; the exact lease boundary; a
  reconnect that resumes one participant while the old epoch is refused; no
  replay of earlier events to a late joiner whatever position it asks for; the
  caps (64 rooms, 16 per room, 4 per tab, sixteen concurrent joins succeeding
  and the rest refused); the event pace with `Retry-After`; a flood that leaves
  another participant's events and the owner's saves unaffected; bounded memory;
  another actor and a wrong tab getting `404`; a join racing a close; closing a
  connection; a trashed artifact; strict route numbers; and no row in the
  database after a restart; a resumed join that delivers, or reports as missed,
  what came while its participant was away; and a malformed, unknown-field or
  oversized body that answers a dead or another actor's connection exactly as a
  valid body does. Node tests give the client parser about thirty
  inconsistent responses, check local validation without a request and the
  follower's backoff (a Retry-After kept between the minimum and the maximum), rejoin, repeated-loss wait and abort (a first join or a leave that never answers, or a signal aborted by `state()` before the join, cannot keep it from settling; the join and the leave share one bound), a join whose answer was lost and resumed without skipping what came meanwhile, and run the real binary with every room
  response validated against the published schema, including that lost first join. The follower does not rejoin after a restart; it rejects with `runtime_session_expired`. Each guard was checked by
  breaking it and watching a named test fail. These tests do not show multiple
  users (those are in the next item), that a frame can use a room, or anything
  about MCP; no browser smoke covers rooms because no web code uses them.
- Multi-user identity (decision 0036), in process with the owner and two
  registered users: a table that sends every artifact-scoped HTTP route and
  MCP tool the other users' real names and names nobody holds and requires the
  same status and body (and the holder's own call not to be `404`), a test that
  reads the server's route declarations and fails on a route that is in no
  table, listings/trash/jobs/feeds that hold only the caller's own with
  per-user numbering, the same request key from two users, the administrator
  boundary (`403` for user and owner-agent tokens, `401` first, decided before
  the body), token ownership and rotation, the owner profile unchanged,
  no MCP tool for users or tokens, disabling (every token `401` on HTTP and
  MCP, streams, previews, connections and rooms closed, another user
  untouched, enabling restores tokens only), a request that authenticated
  before a disable, or before a disable and an enable, leaving nothing registered (stream, link, view, connection, inbox, a tool call, and a room join already in flight), a state generation that rises on each change and never falls, a user at a bound having its places back after a disable and an enable, a disabled user's token still revocable and staying revoked once enabled, and each per-user bound with one
  user at the limit and another unaffected. Provider tests upgrade a database
  from every earlier schema version and keep a feed's cursors. Node tests check
  the client and the CLI against a stand-in server. Each guard was checked by
  breaking it and watching a named test fail. These tests do not show roles,
  sharing, OAuth, or a web screen for users, because none exist.
- Source handoff: the real server and official MCP client exercise stage,
  cancel, send replay, session secrets, selection digest and UTF-16 boundaries,
  polling, acknowledgement, a missing source followed by another delivery,
  the created resource's Location, and retrying a confirmed send after source
  deletion. Rust tests check concurrent cancel and poll, a source lookup held
  across logout, and send or cancellation while a source check fails. Chromium (Firefox suspended in CI) exercises the source editor, trusted
  user actions, an unconfirmed send retried with the same ID, cancellation,
  receipt-only status, and the dialog's accessibility. A closed connection
  while listing inboxes is replaced in Source; a close before stage requires
  the user to select source again, with no automatic stage or send. Client
  tests cover the exact expiry error, one read retry, unrelated failures,
  shared replacement, cancellation, late replies and logout across a new
  connection. The blocked-status
  presentation uses an injected HTTP response; the missing-source transition
  and later delivery use the real MCP/HTTP service. These checks use an MCP
  client, not a live model performing the requested edit.
- `scripts/smoke-workspace.mjs`: a probe inside the preview iframe must fail to
  read the parent page, use storage, open a popup, or reach the API; create,
  save, and reload requests held in flight keep the forms read-only and the
  dialog open; a response lost after commit is retried with the same key and
  replays; a failed load offers a retry; a failed list refresh keeps the open
  draft; unsaved changes lock navigation, and a save in flight locks the
  file tree; non-UTF-8 files are not shown; an edit keeps a byte order mark and CRLF
  line breaks; a rejected token also stops live updates; an empty workspace
  says so and still offers the trash.
- `scripts/smoke-live.mjs`: pages in separate browser contexts follow commits
  made through the API. Exact read counts on load, on a commit elsewhere, on
  a change of unknown kind, and on each kind of reset; a pinned preview
  across tabs; duplicate and older changes read nothing; a draft survives a
  newer revision, a failed refresh, a reset to older data (saving disabled),
  and a reset to an empty server; a hidden page holds no stream, sends no
  read (also the second request of a view's load, or one queued before
  hiding), and resumes after its cursor; reads coalesce during a read; a
  `429` with `Retry-After: 12`, a proxy's HTML `429`, and a `429` on the
  stream each hold every read for the delay; a stream that starts late marks
  a draft as possibly replaced; a background page neither reads nor
  subscribes. An observer installed in each page records any read started
  while another was outstanding.
- `scripts/smoke-list.mjs`: a page lists 50 artifacts and shows more on
  request; a commit elsewhere reads the list once, as long as it was; typing
  a search reads once after the pause and keeps the open artifact open;
  pinning, the pin filter, and both orders; a rename reads the list, the
  artifact, and its history once and keeps a draft; a rename whose answer was
  lost is retried with its key and applies once; a forged conflict changes
  nothing, reads the artifact again, and keeps pinning and renaming waiting
  until the page has read the metadata revision it named. The rename dialog
  is checked for accessibility.
- `scripts/tests/compare.test.mjs` and `scripts/tests/write-gate.test.mjs`:
  bundle web code with esbuild. The comparison: 3,000 seeded random pairs of
  line lists, each with the fewest changed lines (checked against a longest
  common subsequence), both sides rebuilt, and consecutive line numbers; the
  line and change limits; line endings, byte order marks, and missing final
  line feeds as differences; runs left out before, between, and after
  changes; files by digest in path order. The gate: a write waits for every
  version learned from a notice, its own write, or a conflict, for a write
  in flight, and after a possible replacement, when what was learned
  before lapses; a dialog prepares from what it opened on, moves to a newer
  version only once the page shows it, waits on one it does not show yet,
  and sends a request whose answer was lost again as it was.
- `scripts/tests/navigation.test.mjs`: bundles the workspace's navigation
  reducer and list rules. A recovery answered in the trash opens the
  artifact with the search shown then; one answered after the page left the
  trash keeps what it shows; why an open artifact closed is said once, the
  page's own move is not reported again, and an earlier recovery notice is
  replaced; a choice made in the trash opens outside it; an artifact known
  to be in the trash closes unless a draft holds it, and the first listed
  one outside the trash opens; an empty list says the workspace is empty
  only when it read every artifact outside the trash.
- `scripts/tests/source-draft.test.mjs` and `package_rules.rs`: the web's
  path check and the domain judge every path of
  `contracts/fixtures/package-paths.json` alike, and the build inventory's
  package rules are the domain's. A rename of an unchanged file needs and
  carries its base bytes, and moving them home is no change; moved text
  keeps its source, so it is a change elsewhere and none back home with
  the source's text; text equal to its source is no change, and CRLF and a byte order mark are saved as read; each reason the
  server would refuse a draft is named until it is fixed, with sizes
  counting moved bytes.
- `scripts/smoke-editor.mjs`, in its own browser context: a draft spans
  files while choosing another artifact stays locked; the path dialog
  refuses an unserved extension and a case-only twin; a failed read of the
  file to move moves nothing and says why; a binary file moves with its
  exact bytes, and `index.html` moved away disables saving until it is
  back; nothing is read between the click and the save, which makes one
  revision that keeps CRLF; a lost save stays possibly committed after a
  refused retry, shows no "saved elsewhere" notice, and replays; a save
  elsewhere leaves the draft and "Load latest" replaces it; a rename read
  answered before the artifact moved to the trash and delivered after it
  moves nothing, an open path dialog cannot submit in the trash, the open
  file stays editable, also back to its read text, files the draft holds
  still open, and nothing is read there; a save, a file read, and a rename read held across a forged
  `reset` change nothing, and paths cannot change then while text can.
  The draft and the replaced state are checked for accessibility.
- `scripts/tests/page-drafts.test.mjs`: the page's kept drafts against a
  store that releases writes one by one. Only the last state asked for an
  artifact is written, one write at a time, so a late write never brings
  back an ended draft; artifacts do not wait for each other; a failed
  write is reported until a later one succeeds; another tab's draft this
  one continues stays until a write succeeds; a discard runs after the
  writes asked for before it, and one that fails says so; a new list and a
  later write wait for another tab's pending discard, including a failed
  discard that can be retried; discarding after a failed adoption lets a
  fresh draft be kept; the list waits for those writes and offers the page's
  own drafts first.
- `scripts/smoke-drafts.mjs`, in its own browser context, against the
  browser's IndexedDB: a draft with moved bytes outlives a reload, is
  named above the list, is recovered by choice, and saves with its exact
  bytes; a saved, discarded, or reloaded ("Load latest") draft is no
  longer offered; after a reconnect the page's draft is still offered; a
  page disconnected and reloaded keeps its draft from another tab; a save
  in flight when the page reloads is offered as possibly committed, shows
  no "saved elsewhere" notice, and replays without a second revision, even
  after a different request is refused and the original edits are restored;
  different request bodies use different keys; an
  open tab's draft is not offered to its duplicate, which takes an id of
  its own, nor to another tab until it closes; while writes are refused,
  continuing a closed tab's draft leaves its record; the page's own draft
  is offered before a newer one of a closed tab; a draft whose revision
  the server lists with other digests is text only; a refused write warns
  and the next change is kept; drafts that cannot be read hold the editor
  until "Try again", including after a failed owner claim. A save whose
  preflight write is held across a feed reset sends no request. A returned
  owner or a changed record rejects stale adoption and deletion. Pending
  discards prevent competing choices. Failed deletes during save, discard,
  undo, or "Load latest" leave a retry or an explicit continue choice; a
  reset while a saved record is being removed leaves the draft available.
  A view replaced during another owner's pending discard waits for its
  result: a failed discard leaves the choice, and a successful one opens
  the editor without offering the removed record. A discarded record is not
  used again after an earlier failed adoption when a new draft is written.
  A failed list of other artifacts' drafts is shown and can be retried.
  Before each reload or close the smoke waits for the
  store to hold what the next page relies on. The notice and the choice
  are checked for accessibility.
- `scripts/smoke-draft-guards.mjs`: a live draft survives two copied-owner
  pages without Web Locks, including their save and discard choices; the
  pages see the lookup and keep failures. Incomplete lock snapshots fail the
  lookup and cannot expose an active owner's draft; explicitly proceeding
  uses a distinct owner. A real Vite development page also recovers and
  discards a draft under React Strict Mode, retaining then releasing the
  workspace lock. The lookup and recovery choices are checked for
  accessibility.
- `scripts/smoke-history.mjs`: the history tab compares revisions by digest
  (a same-size binary file; a 215-byte path and a 100-byte name inside a
  360-pixel window) and shows text lines with visible carriage returns,
  byte order marks, missing final line feeds, and counts of lines left
  out; a restore commits the earlier digests; a lost restore is retried
  with its key and applies once; a restore waits for a save it learned of
  from a notice, for a forged conflict's revision, and, with the feed
  stalled, for the page's own restore and editor save, and for a save the
  server refused; pinning and renaming wait for the page's own pin; after a
  forged `reset`, while the artifact read is held, nothing is read for
  previews, the source, or comparisons, also when other content is chosen,
  writes wait with the reason shown, and choices made before and meanwhile
  are forgotten once the read arrives; dialogs open across a reset send
  nothing, and one whose request was sent says its outcome is unknown; a
  draft disables restoring. The tab and the dialog are checked for
  accessibility.
- `scripts/smoke-duplicate.mjs`: the duplicate dialog offers the revision
  the preview shows; a duplicate whose answer was lost is retried with its
  key and creates one copy, which opens unpinned at revision 1 with the
  source revision's digests; the source keeps its revisions, pin, and
  title; a dialog open during a forged `reset` sends nothing; a draft
  disables duplicating. The "Copy uploaded assets" box starts unchecked at
  every opening, and an unchecked duplicate sends no `copy_assets` and its
  copy's Assets tab is empty; checked, the copy has the source's asset IDs
  (from an older revision), and a cut answer is resent with the same body
  under the same key, which differs from the default duplicate's; with the
  actor's quota filled through the API, a checked duplicate is refused with
  the asset limit wording and no copy exists (R09c). The dialog is checked
  for accessibility.
- `scripts/smoke-export.mjs`: a failed download says nothing changed and is
  checked for accessibility; tried again, the file the browser saves for
  the revision the preview shows is named after it and holds that
  revision's exact bytes and manifest, read with Node's own CRC-32; a
  download in flight across a forged `reset` is not saved, and the button
  then waits until the artifact is read again.
- `scripts/smoke-trash.mjs`, in its own browser context, with moves and
  recoveries made elsewhere through the API: a draft disables the move and
  the trash; with the feed cut, a move that committed and lost its answer
  is retried with the same key and moves the artifact once, which closes
  and leaves the list before the list is read again; the trash keeps the
  search, opens nothing, and keeps its fields and the focus while another
  search's list loads; a recovery answered after the order changed opens
  the artifact with the order shown then; an open artifact listed first and
  moved elsewhere closes with a notice, and the next one outside the trash
  opens, before the list is read again; with a draft it stays open, cannot
  be saved, and neither previews nor reads its history; with the feed cut,
  a refused recovery reads the artifact once and is sent again from the
  revision the refusal named, the draft editable throughout, or finds it
  recovered elsewhere and the draft savable again; recovered, it previews
  again; a forged conflict holds back only its own artifact in the trash;
  after a forged `reset` nothing in the trash is recovered until it is read
  again, and a recovery or move answered after it neither moves the page
  nor says what it did; a recovery answered after the page left the trash
  keeps a draft written meanwhile. The dialog, the trash, and the draft
  notice are checked for accessibility.

Important new behavior needs rejection and boundary tests and an observed
failure when its implementation is intentionally broken. Restore every mutation
before completing the change. Test assertions must describe observable behavior;
schema rejection tests do not assume whether an enum is emitted as `enum` or
`oneOf`/`const`.

- One-shot sessions (decision 0039): unit tests drive the startup runner with
  a fake stage list (a stop latched during or before each stage means no later
  stage runs; a failed stage ends the run), parse the lifetime (digits only),
  check the marker format and the sweep's fail-closed rules with a proc-shaped
  directory (dead, live, zombie, reused pid, unparsable, a non-directory entry,
  a mode-000 file that is skipped with a message when the user can read it,
  a missing or foreign table, look-alikes, a latched stop), and show a panic
  unwinding removes the directory. Real-process tests (`one_shot_session`) check
  modes and the marker, each stop signal, a stalled client that cannot keep the
  data, TTL expiry on every platform, a stop held at every startup stage and at
  the first sweep entry (debug builds only; skipped with a message otherwise),
  a live owner kept while a dead one is swept, look-alikes kept, no token,
  job, preview or artifact carried over, a failed readiness write (an error
  in one-shot, logged and ignored in persistent mode), a non-UTF-8 temporary
  directory, and refused flag combinations that create nothing. Guards were
  checked by breaking them. Not covered: the same-user check of the sweep
  (no second user in CI), the sweep on macOS and Windows (it removes nothing
  there), secure erase and quotas (not implemented), and signal handling on
  Windows beyond the TTL path.

The outbox delivery smoke keeps owned HTTP response and native IndexedDB fixtures
separate from real-server requests. Its real Rust ACK authority interleavings and
replay/conflict/receipt/rejection scenarios use two independently registered actors;
each artifact is trashed and purged using its creating actor before both actors are
disabled. This leaves the workspace smoke's actor budget independent. The standalone
`scripts/test-outbox-delivery-live.mjs` additionally runs with an 80-request burst
and one request per minute, below the production 120-request burst, so passing does
not rely on browser execution time replenishing a shared fixture budget. It runs
the same real-server assertions and cleanup as the full browser smoke. The held
request check also runs against Rust: committed preparation and frozen bytes are
observed before forwarding, a concurrent run is refused, and releasing that same
request produces a durable ACK. Cleanup attempts every context, artifact step and
actor even after a failure, always closes owned listeners, and reports the first
scenario or cleanup error as the cause of any combined failures. Native HTTP
cleanup tests inject multiple failures and verify the remaining requests and closed
listeners; each cleanup guard was checked by mutation.
Native callback-fault checks throw host errors before and after mutation dispatch
and during 401 authentication invalidation. They verify fixed diagnostics without
the host message or cause, the actual dispatch flag, retained rows, and a permanent
fence even after the host callback is repaired.

## Client trust rules

Proposed by [decision 0040](decisions/0040-data-changes-and-member-listing.md)
rule 22 for the TypeScript client of slice R07c. `listDataMembers` and
`followData` apply these rules (`scripts/tests/data-members.test.mjs` and
`scripts/tests/follow-data.test.mjs`); the change stream and the error body of
its open have the R07c-1 transport bounds (`scripts/tests/client-transport.test.mjs`).
Other client calls keep their current reads.

1. The server owns the data-domain rules. The client does not re-check
   storability (surrogates, duplicate names, value count, nesting, name
   length, canonical size, number spelling) and never computes canonical sizes.
2. Shape, not meaning: fields have the TypeScript type claimed, integers are
   safe, and enums are closed with an `unknown` fallback where the contract may
   grow.
3. The client checks what its own state machine depends on: an echoed artifact
   identifier equals the one asked, the version ordering of decision 0040, known
   event types, a present `next_page_token` is a non-empty string, and the
   failure's class decides between retrying and stopping.
4. Memory bounds are transport bounds: event-stream lines and events, and the
   success and error bodies of the requests decision 0040 names, are read up to
   a fixed size and cancelled past it.
5. A violation of rules 2 to 4 is `invalid_response`, decided once per message
   and never retried.
6. Wire shapes come from the generated contracts; no rule exists only in
   TypeScript.
7. A review finding that asks for a check outside rules 1 to 6 is a follow-up,
   not a blocker. Such a finding is recorded (as a GitHub issue or a roadmap
   row) with a one-line reproduction and is never dropped, so no finding is
   hidden.

## CI and local use

`npm run check` performs formatting, code/CSS lint, generated drift, architecture,
type checking, tooling tests, and the Rust gate. `npm run build` builds packages,
CLI, web assets, and the standalone architecture document.

CI runs the gate/build on Ubuntu 24.04 with Node 22.12.0 and Windows Server 2025
with Node 22.22.2. Runner OS labels are fixed instead of following a moving
`latest` image. GitHub actions run on their supported Node 24 runtime; that is
separate from the application Node versions under test.
Another job runs Cargo tests on Rust 1.89.0 with plain `cargo test`; regular
checks use the pinned Rust toolchain. Rust/CLI JSON equivalence and all CLI
rejection cases run in the general
general gate. Browser jobs use the official Playwright image, pinned by version
and OCI digest, with browsers and system libraries already installed. They save
screenshots, a JSON result, and a trace under `.artifree/qa/<browser>-<shard>/`
(`.artifree/qa/<browser>/` for a local whole-suite run). The image is for trusted scaffold UI tests, not product artifact isolation. Local
checks do not require Docker. PDF output
is checked in Chromium; Playwright's Firefox does not implement PDF printing.

### Rust test runner

`npm run check:rust` runs the Rust tests with cargo-nextest and then the
doctests, because nextest does not run them. The `checks` job installs nextest
on Linux and Windows from a release archive whose SHA-256 is verified, and
`.config/nextest.toml` names the version. That version is a minimum for local
runs, while CI installs exactly it, so the two must be changed together (a
workflow test checks the agreement). nextest runs the test binaries in
parallel, which cut the Rust test step from about 59 s to 16 s on a 16-CPU
sandbox. A test with a wall-clock budget that fails only when the runner is
busy gets `threads-required` in that file, after it is shown to fail or slow
down under load. None needed it in three runs on that sandbox; the CI runners
have fewer CPUs and were not measured, so a flaky test there is handled the
same way. The Rust 1.89.0 job keeps plain `cargo test` because it verifies the
minimum toolchain.

Dependency and workflow checks run on pushes, pull requests, manual runs, and a
weekly schedule. Downloads and advisory database access fail the job if they
fail. All jobs feed the stable **Quality gate** status; failed, skipped, or
cancelled dependencies cannot pass it. Repository branch rules must require
that status to enforce merging restrictions.

Dependabot opens weekly update pull requests for npm, Cargo, and GitHub
Actions. Minor and patch updates are grouped per ecosystem; majors arrive one
at a time. Every update must pass the full gate before it is merged.
`@types/node` majors are ignored because Node types must match the oldest
supported runtime in `engines`; nothing else would catch code that uses APIs
missing from Node 22. Rust needs no such rule: the Rust 1.89.0 job rejects
crates that raise the minimum version.

## Cleanup after verification and merge

Delete test environments when their tests finish, and delete branches when their
changes have merged, including their worktrees. Cleanup is part of completing
the work, including failed or interrupted verification; a failed check is not a
reason to leave an entire environment running or on disk.

- Stop the test servers, workers, browsers, containers, and other processes owned
  by the completed run. Remove its temporary source copies, data directories,
  profiles, volumes, and disposable build outputs. When the task's checks finish,
  remove its dependency installations and build caches, including worktree-local
  `node_modules` and `target` directories and any external cache owned by that
  worktree. Reusable test caches may remain between checks of an ongoing task,
  but must be purged when that task finishes and before removing its worktree.
- Before deleting evidence, record the checked commit or diff, commands, exit
  results, relevant failures, and cleanup outcome. Retain only the logs, traces,
  screenshots, or minimal reproductions needed to explain the result or resolve
  a failure, outside tracked files. Before removing an environment or worktree,
  move that retained evidence outside every directory scheduled for deletion
  and verify it is readable at its retained location. Redact public reports and
  keep private evidence and credentials private. A retained evidence file does not require
  retaining the disposable environment that produced it.
- Before deleting a branch, refresh the target branch and verify the intended
  changes are merged. Check both local and remote branch heads; a merged PR does
  not prove that commits pushed afterwards are merged. For squash or rebase
  merges, use the PR's merged state and reviewed head plus the target's changes;
  ancestry alone may not establish the result. Preserve unmerged branches and
  any additional unmerged work. Delete the merged local and remote topic branch
  once confirmed; preserve the repository's main and other maintained base
  branches.
- Inspect every associated worktree for tracked edits, untracked files, stashes,
  and active use before removal. Preserve unrelated user changes and ongoing
  work, even on a merged branch. Purge that worktree's completed test caches,
  remove the worktree with Git, then remove its branch. Do not force-remove a
  dirty or active worktree or force-delete a branch to bypass unresolved merge
  or ownership checks.
- Confirm exact ownership and paths before stopping processes or deleting files.
  Never use broad process-name kills, wildcard deletion, or global cache pruning
  that can affect another user, repository, or active run. Preserve shared tools,
  system package stores, credentials, private source evidence, and persistent
  application data; they are not disposable test environments. Committed build
  artifacts and generated contracts remain part of the source tree.
- Verify that the completed run's processes, temporary environments, caches, and
  merged topic branches/worktrees are gone. Report what was removed. If cleanup
  is blocked by active use, uncertain ownership or merge status, necessary
  reproduction evidence, or an access failure, report the specific resource,
  reason, and condition or next action for cleanup. Preserve the affected
  resource until that condition is resolved and complete its cleanup afterwards;
  do not silently treat deferred cleanup as complete.

This is a contributor workflow requirement. It does not implement product data
retention, automatic artifact cleanup, or a new sandbox capability.

## Limits and the next implementation

There are no arbitrary coverage-percentage gates for empty modules. The
scaffold's automated accessibility checks cannot establish every accessibility
requirement. Npm/Cargo advisory scans detect known reports, not all vulnerabilities.

Not yet covered, because the behavior does not exist: multi-user roles and
revocation (a viewer's refused save included), broader runtime capabilities, publishing, and resource limits for artifact
scripts. Agent runs with Codex and Pi are manual checks, not CI: they need an
agent account and model calls. Multi-user isolation is tested with the owner and two registered users in process (decision 0036); OAuth, roles, sharing, and an identity provider are not tested because they do not exist. The
crash test kills the process after a commit; it does not simulate power loss
or a storage device that ignores flushes.

## Design review

Applying the local software-design-philosophy diagnostic after the first slice
gives **8.75/10: seven of eight diagnostics pass**. The working modules now have
interfaces much smaller than their implementations: `FilePackage` hides path
and limit rules, `ArtifactService` hides authorization, fingerprinting, and
change application behind six commands, `SqliteStore` hides transactions,
replay, and content sharing, and the preview module hides tickets and CSP.
HTTP, CLI, and web share one client contract and one application service.

The remaining diagnostic is that every module hides a design decision: the
documents, models, tools, build, and export modules are still
placeholders; jobs and application data have their first behavior (decisions 0030 and 0031). The `ArtifactStore` port also deliberately exposes request
fingerprints, because replay must commit atomically with the revision. Reach
10/10 by giving each placeholder a narrow behavioral interface only when its
first behavior is implemented. This score describes design, not production
readiness.

Private upload coverage (partial R09a, 0045): provider asset tests exercise
exact-byte reopening, equal-byte logical charges, independent versions,
authority/key/base precedence, concurrent last-slot admission, metadata-only
replay after deletion, no-op deletion, late receipt rollback, trash/recover/purge,
shared source/asset blob retention, actor quotas including trash, schema-11
upgrade and rejection, and backup/restore. HTTP tests cover stable envelopes,
security headers, 1-byte and exact 5-MiB text, oversized/empty/invalid content,
unknown outcomes and asset route isolation. Content policy validation happens
outside database transactions and replay skips changed policy. Only UTF-8
plain text/CSV, complete static PNG and complete sequential JPEG uploads are admitted. PNG validation has
unit tests per layer (`artifree-core` `asset_validation/png/tests.rs`: accept and
refusal tables, every strict prefix, decompression bombs, window-boundary sizes,
embedded zlib streams) with guard mutations, and server tests for exact stored
bytes and refusals that keep the key free. JPEG validation has unit tests per
layer (`artifree-core` `asset_validation/jpeg/tests.rs` for the header parser and
accept/refusal tables, `jpeg/scan_tests.rs` for the entropy walk: truncation at
every byte, restart sequence, symbol limits, a byte-flip sweep and the worst
case) with guard mutations, and the same server tests; the dispatcher tests in
`asset_validation.rs` cover ordering and the decode lock. Historical image
metadata and matching receipts remain decodable and replay before current policy
without recreating deleted bytes. R09a is complete only for the named JPEG subset
(progressive and other variants are refused as `invalid_asset`). The
TypeScript client has real-server tests (R09b2), the panel's frozen-attempt
rules have Node tests (`scripts/tests/asset-attempt.test.mjs`) and the
management assets panel has a real-server Chromium smoke
(`scripts/smoke-assets.mjs`, R09c) inside the full browser smoke and
standalone through `scripts/test-assets-live.mjs`. It covers upload, attachment
download with no inline rendering, local oversize, type and quota refusals,
stale base, delete confirmation and same-key retries after lost upload and
delete replies, with the trash locked while an attempt is held and the upload
form cleared after a retry succeeds. The duplicate
`copy_assets` extension has provider tests
(`crates/artifree-providers/tests/asset_copy.rs`: full copy, empty space,
space/actor overflow with no key or partial copy, actor scoping, independence,
purge of either side) and HTTP, MCP, client and CLI tests of `copied`, the
fingerprint, overflow and replay after source change or purge. There are no MCP
asset tool or runtime tests here: the MCP tools (R09b3) and the CLI commands
(R09b4) have their own tests, and runtime grants remain R09d work.
