# Quality gates

Status: early development, 2026-10-27. The checks below protect the local HTML
artifact slice ([decision 0003](decisions/0003-local-html-artifact-slice.md))
and the foundation around it. Passing them implies nothing about planned
features.

## Enforced checks

| Area         | Enforcement                                                                                                                                                                                               |
| ------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| TypeScript   | Strict types, checked dependency declarations, exact optional properties, unchecked indexes, side-effect imports, missing returns, fallthrough, unreachable code                                          |
| Code lint    | Oxlint correctness, hooks, JSX accessibility, import cycles, explicit `any`, non-null assertions; zero warnings                                                                                           |
| UI design    | Six `@shadcn/lint` rules: component styling, semantic colors, arbitrary values, inline styles, unknown classes, readable static classes                                                                   |
| CSS          | Stylelint recommended validity checks, duplicate selectors/properties, unknown values/properties, Tailwind-aware at-rules; zero warnings                                                                  |
| Rust         | Unsafe forbidden, unused results denied, public documentation required, documented stdout/stderr exceptions only in CLI binaries, pedantic Clippy, rustfmt, rustdoc warnings denied                       |
| Architecture | Exact Rust workspace membership and allowed dependencies, including build/dev edges; TypeScript package/import/re-export/type/lazy-import boundaries; no computed imports or source symlinks              |
| Contracts    | Rust DTO generation, JSON Schema validity, unknown-field/type/value rejection, stable public TypeScript aliases, byte-for-byte generated drift                                                            |
| Inventory    | Portable relative paths, existing sources, unique keys, no repository escape through symlinks; a named framework or runtime must be a real server dependency                                              |
| Artifacts    | Path traversal and portability, size/count limits, stale-base conflicts, concurrent writers, request-key replay and reuse, injected commit failure, crash and restart, owner-only access, preview sandbox |
| CLI          | Success and failure exit codes, machine-readable stdout, stderr diagnostics, contradictory flags, unknown commands                                                                                        |
| Browsers     | Chromium and Firefox against a real server: connect, create, sandboxed preview probes, save, conflict, pinned revisions; module tabs, keyboard, dialogs, light/dark accessibility, mobile, offline HTML   |
| Dependencies | npm advisory scan at every severity, Cargo advisories/yanks/licenses/sources/wildcards/duplicate versions for shipped native targets; no blanket advisory exceptions                                      |
| CI           | Actionlint syntax/expressions, immutable action commits, read-only token defaults, timeouts, no ignored failures, aggregate includes every verification job                                               |

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
  revision, content sharing, and newer-schema refusal.
- `crates/artifree-server/tests/`: HTTP status and error codes, including
  unsupported methods; the bearer token on every route and unmatched path, with
  a case-insensitive scheme; unsafe paths, body limits, pinned reads, preview
  tickets and CSP, expiry, owner-only access in the core, and (on Unix) an
  owner-only data directory, refusing one other users can access.
- `scripts/tests/server.test.mjs`: the CLI against a spawned server, exact
  bytes through upload and `cat`, uploads that JSON escaping would inflate past
  the body limit, refused dot segments, a missing base revision as an API
  error, and a SIGKILL after commit followed by a restart and a replayed
  retry.
- `scripts/smoke-workspace.mjs`: a probe inside the preview iframe must fail to
  read the parent page, use storage, open a popup, or reach the API; create,
  save, and reload requests held in flight keep the forms read-only and the
  dialog open; a response lost after commit is retried with the same key and
  replays; a failed load offers a retry; unsaved changes lock navigation;
  non-UTF-8 files are not editable; an edit keeps a byte order mark and CRLF
  line breaks.

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
Another job runs Cargo tests on Rust 1.85.0; regular checks use the pinned Rust
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

## Limits and the next implementation

There are no arbitrary coverage-percentage gates for empty modules. The
scaffold's automated accessibility checks cannot establish every accessibility
requirement. Npm/Cargo advisory scans detect known reports, not all vulnerabilities.

Not yet covered, because the behavior does not exist: multi-user roles and
revocation, runtime capabilities, publishing, and resource limits for artifact
scripts. The single-owner token is not a multi-user authentication test. The
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
