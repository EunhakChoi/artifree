# Quality gates

Status: scaffold infrastructure, 2026-10-27. The checks below protect implemented
scaffold behavior; no storage, authorization, or execution feature is implied.

## Enforced checks

| Area            | Enforcement                                                                                                                                                                                             |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| TypeScript      | Strict types, checked dependency declarations, exact optional properties, unchecked indexes, side-effect imports, missing returns, fallthrough, unreachable code                                        |
| Code lint       | Oxlint correctness, hooks, JSX accessibility, import cycles, explicit `any`, non-null assertions; zero warnings                                                                                         |
| UI design       | Six `@shadcn/lint` rules: component styling, semantic colors, arbitrary values, inline styles, unknown classes, readable static classes                                                                 |
| CSS             | Stylelint recommended validity checks, duplicate selectors/properties, unknown values/properties, Tailwind-aware at-rules; zero warnings                                                                |
| Rust            | Unsafe forbidden, unused results denied, public documentation required, documented stdout/stderr exceptions only in CLI binaries, pedantic Clippy, rustfmt, rustdoc warnings denied                     |
| Architecture    | Exact Rust workspace membership and allowed dependencies, including build/dev edges; TypeScript package/import/re-export/type/lazy-import boundaries; no computed imports or source symlinks            |
| Contracts       | Rust DTO generation, JSON Schema validity, unknown-field/type/value rejection, stable public TypeScript aliases, byte-for-byte generated drift                                                          |
| Inventory paths | Portable relative paths, existing sources, unique keys, no repository escape through symlinks; these are not user-upload path checks                                                                    |
| CLI             | Success and failure exit codes, machine-readable stdout, stderr diagnostics, contradictory flags, unknown commands                                                                                      |
| Browsers        | Chromium and Firefox; all module tabs, keyboard operation, dialog focus/escape/return, light/dark accessibility scans, mobile overflow, offline HTML download/reopen, console errors, external requests |
| Dependencies    | npm advisory scan at every severity, Cargo advisories/yanks/licenses/sources/wildcards/duplicate versions; no blanket advisory exceptions                                                               |
| CI              | Actionlint syntax/expressions, immutable action commits, read-only token defaults, timeouts, no ignored failures, aggregate includes every verification job                                             |

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
toolchain. Browser jobs install the lockfile-pinned Playwright browsers and save
screenshots, a JSON result, and a trace under `.artifree/qa/<browser>/`. PDF output
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

The first working artifact slice must add behavioral tests for authorization,
untrusted file paths and sizes, stale revisions, concurrent writes, retry keys,
crash recovery, capability revocation, and isolation. Compiler/lint success and
the current inventory-path checks do not establish those properties.

## Design review

Applying the local software-design-philosophy diagnostic gives this scaffold
**7.5/10: six of eight diagnostics pass**. Module responsibilities, replaceable
integration boundaries, interface documentation, review criteria, discoverable
dependencies, and deliberate design investment are present.

Two diagnostics remain unresolved: planned module interfaces cannot yet be
shown to hide more complexity than their implementations, and placeholder
modules do not yet encapsulate operational decisions. Reach those goals through
the first real slice and narrow behavioral ports, rather than speculative
frameworks or additional wrapper layers. This score describes scaffold design,
not production readiness.
