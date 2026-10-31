# Contributing to Artifree

Artifree is an independently written workspace for agent-made artifacts. It is
in early development: describe implemented behavior and proposed behavior
separately. Human and agent contributions follow the same quality requirements.

## Before changing code

Read [AGENTS.md](../AGENTS.md), the [architecture](../docs/architecture.md),
the [roadmap](../docs/roadmap.md), and the [quality policy](../docs/quality.md).
Check the relevant [decision records](../docs/decisions/) and existing callers
before choosing a design. A roadmap item is a scope and acceptance contract,
not evidence that its implementation is complete.

Start a focused branch from current `main`, or use an isolated worktree. Preserve
other contributors' uncommitted files. Keep one independently reviewable outcome
per PR; include the code, tests, generated files, and documentation needed for
that outcome. Explain material design choices in a decision record rather than
leaving callers to guess. Apply relevant installed development skills when
available; local skill files are optional and must not be required to build or
verify a checkout.

## Development setup

Use Node.js 22.12 or newer, npm 10 or newer, and rustup. The tested Rust toolchain
and components are pinned in [rust-toolchain.toml](../rust-toolchain.toml).
Rust 1.88.0 is the supported minimum. Install dependencies from the lockfile:

```sh
npm ci
```

Run these commands from the repository root.

The [README](../README.md#quick-start) describes running the local server and
web workspace. Keep tokens, local databases, captures, and verification output
under the ignored `.artifree/` directory. Use synthetic fixtures in committed
tests, never account data or live credentials.

## Design and compatibility

- Presentation is TypeScript; application logic is Rust. Keep the domain free
  of I/O and wire types, and the application core independent of concrete
  providers and server frameworks. Share application behavior across adapters.
- Preserve authorization, exact file bytes, conflict handling, and retry
  semantics. Source mutations go through `ArtifactService` with a base revision
  and an idempotency key. An uncertain response does not prove a write failed.
- Keep source revisions, metadata versions, runtime grants, app data, and job
  state distinct. Types and generated schemas do not replace runtime validation
  or current authorization checks.
- Give each state and resource an explicit owner and lifetime. Handle failure,
  cancellation, concurrent work, late replies, and recovery at that boundary.
  Prefer a small interface that hides the difficult cases to speculative layers
  and wrappers that callers must coordinate themselves.
- Use explicit errors and narrow types. Retain the repository's strict TypeScript,
  Rust, and module-boundary checks. Explain new dependencies, public API changes,
  migrations, tokens, variants, and narrow lint exceptions.
- Compose the existing shadcn/ui Base UI components and semantic tokens. Component
  appearance belongs in `apps/web/src/components/ui`; callers control placement.
  Check accessibility and keyboard behavior as well as appearance.

## Verification required before review

Every change, including documentation, must pass:

```sh
npm run check
npm run build
```

UI and standalone HTML changes must also pass both browser suites against the
real server. Install the pinned package's browsers if they are missing:

```sh
npx --no-install playwright install chromium firefox
npm run test:smoke
ARTIFREE_BROWSER=firefox npm run test:smoke
```

In PowerShell, use `$env:ARTIFREE_BROWSER = 'firefox'` before
`npm run test:smoke`, then `Remove-Item Env:ARTIFREE_BROWSER` to restore the default.
Run resource-intensive checks sequentially on constrained machines and bound
their resource use. An OOM or timeout is a failed or inconclusive run.

Run additional checks appropriate to the change:

| Change                                | Additional verification                                                                  |
| ------------------------------------- | ---------------------------------------------------------------------------------------- |
| Wire DTOs, tool catalog, or inventory | `npm run generate:contracts`; review generated diffs with their source                   |
| Runtime bootstrap                     | `npm run generate:runtime`; verify generated drift and the real frame boundary           |
| Architecture HTML source              | `npm run build:architecture`; verify the standalone output in browsers                   |
| Rust or Rust dependencies             | `cargo +1.88.0 test --workspace --locked`, in addition to the pinned-toolchain gate      |
| Dependencies or workflows             | Advisory/license checks and actionlint below; include every new CI job in `Quality gate` |

On Linux x64, the repository installer verifies fixed archive digests:

```sh
node scripts/install-ci-tools.mjs
.artifree/tools/actionlint
.artifree/tools/cargo-deny --locked check
npm audit --audit-level=low
```

On other platforms, install the tool versions named in
[`scripts/install-ci-tools.mjs`](../scripts/install-ci-tools.mjs) through their
upstream instructions. Online advisory checks are separate from the local gate.
Commit lockfiles and generated outputs with the source that produced them; do
not hand-edit generated contracts or the embedded runtime.

### Tests must detect the named failure

For a bug fix, reproduce the failure through the real path before fixing it.
Test observable behavior at the boundary that owns it, with important refusal,
retry, conflict, cancellation, and recovery cases. Use deterministic control of
concurrent work and time rather than arbitrary sleeps. Distinguish a mock's
result from an actual browser, database, protocol, or external-provider result.

Prove important new guards can fail: temporarily break the guard, verify the
test fails for the intended reason, restore the source, and verify the baseline.
Compilation failures, unrelated assertions, OOMs, and timeouts do not count as
successful mutation detection. Record the tested source version and useful
failure evidence. Do not add tests that merely repeat implementation details.

Never skip checks, loosen assertions, disable rules, or ignore failures to get
a pass. If a test or rule is demonstrably wrong, explain the evidence and make
the smallest justified correction while preserving the intended protection.

## Evidence and provenance

For reconstruction work, distinguish observed behavior, source-only evidence,
inference, and independent Artifree decisions. Link the requirement to public
decision records and reproducible synthetic examples. Maintainers with private
investigation material keep the requirement-to-original-to-code/test trace
outside public files; access to that material is not a contributor prerequisite.

Write original code and assets. Never publish proprietary bundles, decompiled
snippets, private captures, private account data, credentials, or private evidence
paths in source, logs, PRs, or screenshots. Retain open-source licenses and
attribution; see [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md). Do not claim
formal clean-room separation or verification of an unavailable external service.

## Pull requests and review

Use the [PR template](PULL_REQUEST_TEMPLATE.md). Lead with the concrete problem
and resulting behavior, then the design choices a reviewer needs to assess it.
List commands actually run, their results, and relevant regression evidence.
State unrun checks and material limitations explicitly. Update the description
to the final implementation; omit session transcripts and abandoned approaches
unless they explain a current tradeoff.

Maintainers apply this Codex PR review policy:

1. Opening a PR for review starts the first Codex review automatically. Do not
   also post a duplicate request. After that, request each new round with the
   exact comment `@codex review`.
2. Each round has a hard 15-minute limit, starting at PR creation for the initial
   review, or at the request comment for a subsequent review. A review that has
   not completed in that time counts as an error/failed round. A late result
   does not erase the timeout or restore the round.
3. Allow at most three rounds per PR, including the initial automatic review
   and any errors or timeouts. Record the PR, target HEAD SHA, trigger and
   completion times, findings, and disposition. Changing agents or branches
   behind the same PR does not reset the count.
4. Review each finding for correctness and fix the underlying cause across
   affected paths. Settle unresolved design choices and update the decision
   record before requesting another round. Independent local review supplements
   the Codex PR review; it does not replace it.
5. If the PR has not passed within three rounds, close it with the root cause
   and a revised design. Start from the latest `main` on a new branch and write
   the change anew, incorporating the confirmed findings and regression cases.
   Do not reopen or continue the failed branch, or merge, cherry-pick, or copy
   its implementation into a nominal rewrite. A new PR starts at round one.
6. Merge only after the current HEAD has a completed Codex review with no
   findings, all required CI succeeds, review conversations are resolved, and
   conflicts are cleared. Silence, a running/completed label alone, a stale
   review, or successful CI alone is not a review pass. Freeze the reviewed
   HEAD; any later change needs validation and review of the new HEAD within
   the remaining rounds. Never bypass branch protection.

The required CI aggregate is **Quality gate**; every verification job must be in
its `needs`. Dependency updates follow the same gates. After merge, keep the
roadmap's completion state and relevant decisions accurate. A green gate proves
only the checks that ran, not completion of all planned product capabilities.
