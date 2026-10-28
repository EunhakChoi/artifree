# Quality gates

Status: early development, 2026-10-28. The checks below protect the local HTML
artifact slice ([decision 0003](decisions/0003-local-html-artifact-slice.md))
and the foundation around it. Passing them implies nothing about planned
features.

## Enforced checks

| Area         | Enforcement                                                                                                                                                                                                                                                                                           |
| ------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| TypeScript   | Strict types, checked dependency declarations, exact optional properties, unchecked indexes, side-effect imports, missing returns, fallthrough, unreachable code                                                                                                                                      |
| Code lint    | Oxlint correctness, hooks, JSX accessibility, import cycles, explicit `any`, non-null assertions; zero warnings                                                                                                                                                                                       |
| UI design    | Six `@shadcn/lint` rules: component styling, semantic colors, arbitrary values, inline styles, unknown classes, readable static classes                                                                                                                                                               |
| CSS          | Stylelint recommended validity checks, duplicate selectors/properties, unknown values/properties, Tailwind-aware at-rules; zero warnings                                                                                                                                                              |
| Rust         | Unsafe forbidden, unused results denied, public documentation required, documented stdout/stderr exceptions only in CLI binaries, pedantic Clippy, rustfmt, rustdoc warnings denied                                                                                                                   |
| Architecture | Exact Rust workspace membership and allowed dependencies, including build/dev edges; TypeScript package/import/re-export/type/lazy-import boundaries; no computed imports or source symlinks                                                                                                          |
| Contracts    | Rust DTO generation, JSON Schema validity, unknown-field/type/value rejection, stable public TypeScript aliases, byte-for-byte generated drift; MCP tool schemas self-contained, standard-format, closed                                                                                              |
| Inventory    | Portable relative paths, existing sources, unique keys, no repository escape through symlinks; a named framework or runtime must be a real server dependency                                                                                                                                          |
| Artifacts    | Path traversal and portability, size/count limits, stale-base conflicts, concurrent writers, request-key replay and reuse, injected commit failure, crash and restart, owner-only access, preview sandbox, per-actor request rate and concurrency limits, change feed delivery, resumption, and reset |
| CLI          | Success and failure exit codes, machine-readable stdout, stderr diagnostics, contradictory flags, unknown commands                                                                                                                                                                                    |
| Browsers     | Chromium and Firefox against a real server: connect, create, sandboxed preview probes, save, conflict, pinned revisions, following committed changes; module tabs, keyboard, dialogs, light/dark accessibility, mobile, offline HTML                                                                  |
| Dependencies | npm advisory scan at every severity, Cargo advisories/yanks/licenses/sources/wildcards/duplicate versions for shipped native targets; no blanket advisory exceptions                                                                                                                                  |
| CI           | Actionlint syntax/expressions, immutable action commits, read-only token defaults, timeouts, no ignored failures, aggregate includes every verification job                                                                                                                                           |

The formatter owns formatting. CSS lint uses the recommended correctness preset
rather than imposing a second formatting convention. Tailwind's `@apply` needs a
specific syntax exception. The CSS specificity heuristic is disabled because it
cannot infer the standalone document's disjoint component trees; duplicate
selectors, invalid declarations, and unknown properties remain errors.

Inside UI component definitions, restyling, arbitrary-value, and static-class
rules are disabled deliberately: that directory owns component appearance and
forwards caller classes. Semantic-color, inline-style, and unknown-class checks
remain enabled there. Application call sites retain all six rules.

## Checks must be capable of failing

`npm run test:tooling` runs real negative inputs through the actual lint config,
schema, architecture policy, and workflow policy. It verifies that all six UI
rules still produce errors; a zero-file lint run is a failure. It also catches
unpinned actions, ignored failures, and a new job omitted from the aggregate.
The Rust tests execute Serde rejection and the native descriptor CLI directly.
No domain test exists merely to repeat a field declaration.

Artifact behavior is tested at the boundary that owns it:

- `crates/artifree-domain`: path, identifier, and package validation.
- `crates/artifree-providers/tests/sqlite_store.rs`: the store contract on a
  real database — reopen durability, replay, key reuse, conflicts, eight
  concurrent writers, a trigger-injected failure that must leave no partial
  revision, content sharing, and newer-schema refusal; one feed entry per
  commit and none for replays, conflicts, reused keys, or a failed commit,
  a commit that fails with its feed entry, owner-only pages that end at the
  head, a feed identity kept on reopening and different in another
  database, a version 1 database that gains the feed and keeps its
  data, and commit times that follow commit order after the clock is set
  back.
- `crates/artifree-server/tests/`: HTTP status and error codes, including
  unsupported methods; the bearer token on every route and unmatched path, with
  a case-insensitive scheme; unsafe paths, body limits, pinned reads, preview
  tickets and CSP, expiry, owner-only access in the core, and (on Unix) an
  owner-only data directory, refusing one other users can access.
- `scripts/tests/server.test.mjs`: the CLI against a spawned server, exact
  bytes through upload and `cat`, uploads that JSON escaping would inflate past
  the body limit, refused dot segments, a base newer than current reported as
  the same conflict for every save form, every JSON response validated against
  the API schema root (which must reject an empty object) with its expected
  status, and a SIGKILL after commit
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
  reads exact pinned bytes (BOM, CRLF, binary), and opens a sandboxed preview;
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
  bodies being read.
- `crates/artifree-server/tests/change_feed.rs`: a stream starts with `ready`
  at the head and sends each commit once and nothing for a replay or
  conflict; a stream resumes after its cursor; a cursor of another feed or
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
- `scripts/tests/limits.test.mjs`: the server binary with small limits. The
  official MCP client and the CLI share the budget, a refused create commits
  nothing and its key works after the delay, a CLI retry of a committed
  create refused by the rate limit says the attempt was not processed and
  replays after the delay, a
  refused 30 MiB upload still receives the `429`, a refused body that never
  ends is answered within the drain bound and immediately once 32 are being
  read, a request held mid-body
  keeps its concurrency slot until it finishes, and invalid limit flags stop
  the server before it listens.
- `scripts/tests/client.test.mjs`: the API client with an injected `fetch`: a
  body cut off mid-stream is a network error, a complete foreign body is an
  invalid response, and uploads pick the smaller exact encoding. For the
  change feed: the event stream rules (line ends split across chunks or
  ended by a final CR, a byte order mark, a character split across chunks,
  comments, unknown fields and events, joined data lines, an event without
  data or without its blank line); an unknown change kind arriving as
  `unknown` while broken data is refused; reconnecting from the last cursor
  after a lost connection, an empty `502`, an HTML `504`, `503`, and `429`
  with their delays; stopping on `401`, `400`, an HTML `404`, a successful
  page that is not an event stream, and broken event data; a proxy's
  `Retry-After` honored with a non-API body; a handler error that ends
  following instead of a retry; and an abort that ends a retry wait at once,
  cancels an open stream, and stops events already received in the same
  chunk. A proxy's `429` without an API body is still `rate_limited`, and a
  read's signal reaches `fetch`.
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
- `scripts/smoke-workspace.mjs`: a probe inside the preview iframe must fail to
  read the parent page, use storage, open a popup, or reach the API; create,
  save, and reload requests held in flight keep the forms read-only and the
  dialog open; a response lost after commit is retried with the same key and
  replays; a failed load offers a retry; a failed list refresh keeps the open
  draft; unsaved changes lock navigation;
  non-UTF-8 files are not editable; an edit keeps a byte order mark and CRLF
  line breaks; a rejected token also stops live updates.
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

Important new behavior needs rejection and boundary tests and an observed
failure when its implementation is intentionally broken. Restore every mutation
before completing the change. Test assertions must describe observable behavior;
schema rejection tests do not assume whether an enum is emitted as `enum` or
`oneOf`/`const`.

## CI and local use

`npm run check` performs formatting, code/CSS lint, generated drift, architecture,
type checking, tooling tests, and the Rust gate. `npm run build` builds packages,
CLI, web assets, and the standalone architecture document.

CI runs the gate/build on Ubuntu 24.04 with Node 22.12.0 and Windows Server 2025
with Node 22.22.2. Runner OS labels are fixed instead of following a moving
`latest` image. GitHub actions run on their supported Node 24 runtime; that is
separate from the application Node versions under test.
Another job runs Cargo tests on Rust 1.88.0; regular checks use the pinned Rust
toolchain. Rust/CLI JSON equivalence and all CLI rejection cases run in the
general gate. Browser jobs use the official Playwright image, pinned by version
and OCI digest, with browsers and system libraries already installed. They save
screenshots, a JSON result, and a trace under `.artifree/qa/<browser>/`. The image is for trusted scaffold UI tests, not product artifact isolation. Local
checks do not require Docker. PDF output
is checked in Chromium; Playwright's Firefox does not implement PDF printing.

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
missing from Node 22. Rust needs no such rule: the Rust 1.88.0 job rejects
crates that raise the minimum version.

## Limits and the next implementation

There are no arbitrary coverage-percentage gates for empty modules. The
scaffold's automated accessibility checks cannot establish every accessibility
requirement. Npm/Cargo advisory scans detect known reports, not all vulnerabilities.

Not yet covered, because the behavior does not exist: multi-user roles and
revocation, runtime capabilities, publishing, and resource limits for artifact
scripts. Agent runs with Codex and Pi are manual checks, not CI: they need an
agent account and model calls. The single-owner token is not a multi-user authentication test. The
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
documents, jobs, handoff, models, tools, build, and export modules are still
placeholders. The `ArtifactStore` port also deliberately exposes request
fingerprints, because replay must commit atomically with the revision. Reach
10/10 by giving each placeholder a narrow behavioral interface only when its
first behavior is implemented. This score describes design, not production
readiness.
