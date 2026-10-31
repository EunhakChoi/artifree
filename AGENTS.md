# Working on Artifree

Artifree is in early development: local HTML artifacts work end to end
(decision 0003), including MCP tools for agents (decision 0004), and an
opted-in preview can read its own metadata with a connection grant
(decisions 0019 and 0020). Other runtime
capabilities remain proposed. Keep implemented and proposed behavior
explicitly separate in code, UI, docs, and completion reports.

Read `docs/roadmap.md` before starting the next feature. It records PR-sized
scope, dependencies, acceptance checks, and the requirement to trace relevant
analysis back to original evidence. Keep private evidence outside public files.

Follow [the contribution guide](.github/CONTRIBUTING.md) for the verification
workflow and PR review rules, and use `.github/PULL_REQUEST_TEMPLATE.md`.

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
  Runtime metadata requires an explicit connection grant (decision 0020).
  Other capabilities and network isolation remain unimplemented.
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

## Provenance

Write original code and assets. Never copy vendor bundles, decompiled code,
private captures, account data, secrets, cookies, or proprietary implementation
snippets into this public repository. Use independently expressed requirements
and original examples. Do not claim a formal clean-room separation has occurred.
Open-source UI components are allowed with their license and provenance retained;
see `THIRD_PARTY_NOTICES.md` for the shadcn/ui source attribution.
