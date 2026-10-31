# Contributing

Artifree welcomes contributions from people and coding agents. Work is accepted
on the strength of its behavior, design, and verification. Keep shipped features
and future plans clearly distinguished.

## Choose a bounded change

Begin with [AGENTS.md](../AGENTS.md), [the roadmap](../docs/roadmap.md),
[the architecture](../docs/architecture.md), and [the quality policy](../docs/quality.md).
Read the relevant [decisions](../docs/decisions/) and callers before extending a
contract. Record a significant new decision before spreading its assumptions
across modules.

Use a fresh branch based on current `main`; an isolated worktree is useful when
other work is present. Preserve existing local changes. A PR should deliver one
reviewable result together with its tests, compatibility considerations, and
documentation. Apply relevant installed development skills where helpful. The
repository must remain usable without any contributor's optional local skills.

## Set up and verify

Run commands from the repository root. Install Node.js 22.12 or newer, npm 10 or
newer, and rustup. [rust-toolchain.toml](../rust-toolchain.toml) selects the normal
Rust compiler, formatter, and Clippy; it does not install the separate minimum
supported compiler used below.

```sh
npm ci
npm run check
npm run build
```

`check` and `build` are required for every PR, including documentation changes.
Run focused tests while developing, then the applicable full checks. See the
[README](../README.md#quick-start) for starting the local workspace.

For web UI or standalone HTML changes, run the real-server smoke suite in both
engines. If browser binaries are absent, install them through the locked local
Playwright package:

```sh
npx --no-install playwright install chromium firefox
npm run test:smoke
ARTIFREE_BROWSER=firefox npm run test:smoke
```

In PowerShell, replace the last line with `$env:ARTIFREE_BROWSER = 'firefox'`
followed by `npm run test:smoke`. Afterwards, use
`Remove-Item Env:ARTIFREE_BROWSER` to return to the default engine.

Rust and Rust dependency changes must also pass the minimum-version check.
These are the same installation and test commands as the `msrv` CI job:

```sh
rustup toolchain install 1.88.0 --profile minimal --no-self-update
cargo +1.88.0 test --workspace --all-features --locked
```

For dependency or workflow changes, run the corresponding CI tools. The installer
below supports Linux x64 and checks download digests:

```sh
node scripts/install-ci-tools.mjs
.artifree/tools/actionlint
.artifree/tools/cargo-deny --locked check
npm audit --audit-level=low
```

Other platforms should install the versions specified by
[`scripts/install-ci-tools.mjs`](../scripts/install-ci-tools.mjs) using those
tools' upstream installation instructions. Advisory checks need network access;
report any check that could not run. CI's authoritative commands and platforms
are in [the workflow](workflows/ci.yml); keep these instructions aligned with it.

Regenerate outputs when their inputs change:

| Input                                           | Command                      |
| ----------------------------------------------- | ---------------------------- |
| Rust wire DTOs, MCP tool definitions, inventory | `npm run generate:contracts` |
| Runtime bootstrap source                        | `npm run generate:runtime`   |
| Standalone architecture source                  | `npm run build:architecture` |

Review and commit generated changes with their source and lockfiles. Do not
edit generated outputs to conceal drift. Execute heavy checks one at a time
on resource-constrained hosts and enforce an appropriate memory limit.

## Preserve the important boundaries

- TypeScript owns presentation; Rust owns application logic. Domain types are
  independent of I/O and wire contracts. The core depends on ports, with
  concrete providers and server frameworks composed outside it.
- HTTP, MCP, CLI, and web share application semantics. Source writes use
  `ArtifactService`, base-revision checks, and idempotency keys. Lost responses
  require recovery of the original outcome, not an assumption that nothing ran.
- Authorization and runtime validation remain necessary even with generated
  types. Keep source revisions, metadata versions, runtime grants, app data,
  and job state separate. Recheck authority where the operation needs it.
- Put state and resource ownership at the lifetime that actually owns them.
  Define errors, cancellation, concurrent updates, and late replies there.
  Prefer a useful, narrow interface to an abstraction introduced for a possible
  future feature.
- Reuse the existing shadcn/ui Base UI components, variants, and semantic tokens.
  Keep component appearance in `apps/web/src/components/ui`; callers arrange
  layout. Verify keyboard access and accessibility along with visual behavior.

## Make failures observable

A bug fix starts with a reproduction that fails for the reported reason. Tests
should exercise public behavior and the boundary responsible for it. Include
meaningful rejection, conflict, retry, interruption, and recovery scenarios;
coordinate asynchronous work explicitly instead of relying on arbitrary sleeps.

For important guards, deliberately break the guard, confirm the expected
assertion fails, restore the original code, and verify the passing baseline.
Build errors, a timed-out runner, unrelated failures, and exhausted resources
are inconclusive mutation results. Preserve enough version and execution
evidence to tell which code was checked. A mock does not verify a real browser,
database, protocol peer, or model provider.

Keep the strict lint, formatting, type, compiler, dependency-boundary, and
generated-drift checks. Never skip a check, dilute an assertion, or ignore a
failure just to obtain green CI. If a rule or test is demonstrably wrong, show
the evidence and make a narrow correction that retains its protection. Explain
new dependencies, compatibility changes, migrations, and any necessary exception.
Avoid tests that merely restate the implementation.

## Explain evidence without publishing private material

Reconstruction work must distinguish observations, source-only findings,
inferences, and Artifree design decisions. Public decisions and synthetic
examples should explain the behavior to contributors. Maintainers with private
investigation evidence keep their original-to-requirement-to-code/test mapping
outside the repository; access to that material is not required to contribute.

Submit independently written code and assets. Exclude proprietary bundles,
decompiled snippets, private captures, account data, secrets, and private evidence
paths from commits and PRs. Preserve third-party licenses and attribution in
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md). Do not claim formal clean-room
separation or successful execution of an untested integration. Local data and
verification artifacts belong under the ignored `.artifree/` directory.

## Submit and review

Use [PULL_REQUEST_TEMPLATE.md](PULL_REQUEST_TEMPLATE.md). Explain the problem and
result first, then only the design choices needed to assess the final diff.
Report actual commands, outcomes, relevant regression evidence, and limitations.
Identify checks that were unrun or inapplicable. Update the description when the
implementation changes; session transcripts are not a PR description.

The maintainer's Codex review procedure is:

1. Open a ready PR after local verification. Its automatic review is round one;
   do not duplicate it with a comment. Further rounds are requested with the
   exact comment `@codex review`.
2. Allow 15 minutes per round, measured from PR creation or the subsequent
   request comment. Errors and reviews unfinished at the deadline consume a
   failed round. A later result does not undo that failure.
3. Permit at most three rounds on a PR, including the initial review and service
   errors or timeouts. Track its HEAD, trigger/completion timestamps, outcome,
   and findings. Moving the work between agents does not reset the count.
4. Evaluate findings and address the shared cause across affected paths. Settle
   design choices before another review. Local or subagent review is additional
   evidence and cannot substitute for the Codex PR review.
5. After three rounds without a pass, close the PR and explain the cause and
   revised approach. Create a new branch from latest `main` and independently
   reimplement the change with the findings and regression requirements
   incorporated. Do not reopen the failed branch or merge, cherry-pick, or copy
   its implementation into the replacement. The replacement PR starts at one.
6. Merge only when the current HEAD has a completed, finding-free Codex review,
   all required CI passes, review conversations are resolved, and conflicts are
   cleared. A status label, silence, old-HEAD review, or CI alone is insufficient.
   Changes after review require checks and review of the new HEAD within the
   remaining rounds. Branch protection must not be bypassed.

The required aggregate is **Quality gate**. Include every added verification job
in its `needs`; dependency updates use the same gates. Keep decisions and roadmap
status accurate after delivery. Passing checks establishes only what those
checks exercised, not completion of the entire product roadmap.
