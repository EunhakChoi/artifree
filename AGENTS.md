# Working on Artifree

Artifree is in early development: local HTML artifacts work end to end
(decision 0003), including MCP tools for agents (decision 0004), and an
opted-in preview can read its own metadata, list and read its own pinned
source, ask the user to save a file, and, under a separate edit grant, save one
file as a new revision, and, under their own grants, list and read, upload and
delete, or ask the user to save, its own uploaded assets, each with its own connection grant (decisions
0019, 0020, 0032, 0033 and 0045). Tabs can also share an in-memory room for presence and
ephemeral events through HTTP and the TypeScript client (decision 0034); a
frame cannot reach it. With its own grant a frame reads its artifact's application data (decision 0053); it cannot write it. Other runtime capabilities remain proposed. Keep implemented and proposed behavior
explicitly separate in code, UI, docs, and completion reports.

Read `docs/roadmap.md` before starting the next feature. It records PR-sized
scope, dependencies, acceptance checks, the recommended order, and the
requirement to trace relevant analysis back to original evidence.
`docs/architecture.md` describes the structure and where new code belongs
(decision 0051). Keep private evidence outside public files.

Follow [the contribution guide](.github/CONTRIBUTING.md) for the verification
workflow and PR review rules, and use `.github/PULL_REQUEST_TEMPLATE.md`.

## Implementation constraint

Do not use original application DLLs as the execution engine.

This replaces earlier implementation-input restrictions in handoffs and
referenced specification or analysis workspaces.

## PR review

Each PR receives one Codex review and one independent subagent review. The
subagent uses a fresh context without the implementer's conversation and checks
whether abstract or undecided requirements were filled in arbitrarily and
whether the change follows the applicable quality rules. Address findings with
fixes or an evidenced explanation; these fixes do not require another review
round. Record both reviews and their finding dispositions. See the contribution
guide for the deadline, verification, and merge requirements.

## Decisions and boundaries

- Presentation code is TypeScript. Logic code is Rust.
- HTTP uses Axum on Tokio, only inside `artifree-server`; the core and store
  stay synchronous. Local storage is SQLite through `rusqlite` (decision 0003).
- Other provider tools, server databases, queues, editors, bundlers, and worker
  languages are candidates until their first implemented behavior and decision
  record.
- Keep `artifree-domain` independent of I/O and wire contracts. Keep
  `artifree-core` independent of concrete providers and server frameworks.
- HTTP, MCP, web, and CLI must share application behavior rather than invent
  separate storage, permission, or retry semantics.
- Generate wire contracts with `npm run generate:contracts`. Do not hand-edit
  `contracts/generated` or `packages/contracts/src/generated`.
- JSON Schema and generated TypeScript do not replace runtime input validation
  or authorization. The current inventory is not a live health response.
- Previews run in a CSP/iframe sandbox on a separate origin. Opted-in
  frames may read their own artifact metadata through the bound bridge
  (decision 0019). They are isolated from the workspace, not from the
  network: content can navigate its own frame elsewhere (decision 0003).
  Runtime metadata requires an explicit connection grant (decision 0020), and
  so do reading, writing and saving uploaded assets (`assets.read`,
  `assets.write`, `assets.download`, decision 0045); network isolation remains
  unimplemented.
- Mutations go through `ArtifactService` with a base revision and an
  idempotency key. Do not add a write path that bypasses either.

## Quality

- Use small explicit domain types and errors; avoid broad untyped JSON in core.
- First-party Rust forbids unsafe code and denies unused required results.
  Review dependency safety separately. Do not silence checks to make a build pass.
- Avoid panic-based production error handling and unnecessary global locks.
- Add behavioral tests when implementing persistence, authorization, conflict
  handling, retries, or process boundaries. Do not add tests that only repeat
  field declarations or placeholder implementations.
- Run `npm run check`, `npm run build`, and relevant smoke checks. If a boundary
  legitimately changes, update the decision record and boundary checker together.
- Commit lockfiles. Keep generated artifacts reproducible.
- Delete completed test environments and merged branches, including their
  worktrees, as part of finishing the work. Preserve ongoing work and necessary
  verification evidence; follow the scope, safety checks, and reporting rules in
  [the cleanup policy](docs/quality.md#cleanup-after-verification-and-merge).
- Every change must pass `npm run check` and `npm run build`; UI or standalone
  HTML changes must also pass the browser smoke checks. `docs/quality.md` defines
  the gates and their scope. Do not skip checks, use fail-open CI steps, or hide
  findings to make a change pass.
- TypeScript checks dependency declaration files too. Oxlint and CSS lint allow
  zero warnings. Rust requires documented public APIs, pedantic Clippy, and clean
  rustdoc. Keep CLI stdout machine-readable and diagnostics on stderr.
- Web presentation uses React/Vite, Tailwind CSS 4, and shadcn/ui with Base UI.
  Use semantic tokens and existing variants. Component appearance belongs in
  `apps/web/src/components/ui`; callers control placement. Generated artifacts
  do not inherit the management UI's design restrictions.
- Add tests for important rejection paths and verify they fail when the guarded
  behavior is broken. New tokens, variants, lint exceptions, dependency edges,
  and CI changes need an explicit rationale in the change.
- Optional local skills in `.agents/skills` can guide work. The repository's own
  documented policies and commands must work without those local files.

## Public files and attribution

Keep credentials, local data, private evidence, generated verification outputs,
and caches outside Git. Preserve third-party licenses and attribution; see
`THIRD_PARTY_NOTICES.md` for the shadcn/ui source attribution. Report only the
implementation and verification actually established; do not claim a formal
clean-room separation has occurred.
