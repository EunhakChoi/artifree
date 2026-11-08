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

A feature whose behavior is not already specified in a decision record or a
detailed roadmap row first gets a documentation-only specification pull
request: a decision record with status proposed and the roadmap rows, with no
code. The specification pull request and each code pull request that follows it
each receive one Codex review and one independent subagent review. A code pull
request may not change a numbered rule of the merged specification; a rule is
amended by a separate documentation pull request.

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
[README](../README.md#quick-start) for starting the local workspace. How tests
are written and how heavy commands are isolated is in
[testing practice](../docs/testing-practice.md).

For web UI or standalone HTML changes, run the real-server smoke suite in
Chromium. Firefox is suspended in CI until the planned feature set is complete
(see [quality](../docs/quality.md#browser-engines)). If browser binaries are
absent, install them through the locked local Playwright package:

```sh
npx --no-install playwright install chromium
npm run test:smoke
```

To run Firefox locally anyway, install `firefox` the same way and set
`ARTIFREE_BROWSER=firefox` (in PowerShell, `$env:ARTIFREE_BROWSER = 'firefox'`,
then `Remove-Item Env:ARTIFREE_BROWSER` afterwards).

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

## Implementation inputs and verification evidence

Do not use original application DLLs as the execution engine. This replaces
earlier implementation-input restrictions in handoffs and referenced
specification or analysis workspaces.

Reconstruction work must distinguish observations, source-only findings,
inferences, and Artifree design decisions. Public decisions and synthetic
examples should explain the behavior to contributors. Maintainers with private
investigation evidence keep their original-to-requirement-to-code/test mapping
outside the repository; access to that material is not required to contribute.

Keep credentials, local data, private evidence and its paths, generated
verification outputs, and caches out of commits and PRs. Preserve third-party
licenses and attribution in [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).
Do not claim formal clean-room
separation or successful execution of an untested integration. Local data and
verification artifacts belong under the ignored `.artifree/` directory.

## Submit and review

Use [PULL_REQUEST_TEMPLATE.md](PULL_REQUEST_TEMPLATE.md). Explain the problem and
result first, then only the design choices needed to assess the final diff.
Report actual commands, outcomes, relevant regression evidence, and limitations.
Identify checks that were unrun or inapplicable. Update the description when the
implementation changes; session transcripts are not a PR description.

Open a ready PR after local verification. Each PR receives exactly two reviews:

1. **One Codex review.** The automatic review on PR creation counts; do not send
   another request while it is pending or after it completes. If no automatic
   review was triggered, request the one review with a standalone PR comment
   containing exactly `@codex review`. Avoid the bot mention in PR descriptions
   and ordinary progress comments. Keep the 15-minute deadline, including queue
   time, measured from PR creation or the actual manual request. Record service
   errors or timeouts honestly; a reaction, silence, or a pending job is not a
   completed review. If the single review fails or remains incomplete at the
   deadline, record the outcome and leave the PR blocked from merging. Do not
   request a replacement review or count the incomplete review as completed.
2. **One independent subagent review.** Use a fresh context without the
   implementer's conversation. Give the reviewer the user request, applicable
   rules, relevant requirements and decisions, and the diff. Focus on abstract or
   undecided requirements that the implementation may have filled in arbitrarily,
   unsupported assumptions, and compliance with applicable code-quality skills.
   For documentation changes, check that the policy matches the user request and
   is consistent across the affected documents.

Evaluate findings and address the shared cause across affected paths with fixes
or an evidenced explanation. Settle design choices consistently across code,
tests, and documentation, and run the checks appropriate to those fixes. Do not
request another review solely because findings were fixed or the head changed.
The former three-round workflow, forced PR closure, and requirement to discard
and reimplement a reviewed change no longer apply.

Record the reviewer, reviewed full head SHA (or local diff for the subagent),
request/completion times, result, and finding disposition in the PR. Record the
Codex deadline and any failure without claiming a completed review. Review fixes
may follow the recorded reviewed SHA; identify those fixes and their verification
in the same record. A final PR comment can hold this evidence without a recursive
documentation commit just to describe its own review. Earlier review histories
describe the policy in force at the time, not the current workflow.

Merge after the one Codex review and one independent subagent review are
completed, their findings are addressed, and the current head has successful
required CI. Resolve review conversations and clear conflicts before merging.
Review fixes do not need a further review. Branch protection must not be bypassed;
a status label, silence, a pending review, or CI alone is insufficient. After
merging, verify CI for the actual main merge commit and address any failure
before dependent merges.

Finish by deleting completed test environments and merged branches, including
their worktrees. Follow [the cleanup policy](../docs/quality.md#cleanup-after-verification-and-merge)
to preserve verification evidence and ongoing work, verify merge status, and
report both removed resources and any resources whose cleanup is deferred.

The required aggregate is **Quality gate**. Include every added verification job
in its `needs`; dependency updates use the same gates. Keep decisions and roadmap
status accurate after delivery. Passing checks establishes only what those
checks exercised, not completion of the entire product roadmap.
