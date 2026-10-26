# Working on Artifree

Artifree is at the scaffolding stage. Keep implemented behavior and proposed
behavior explicitly separate in code, UI, docs, and completion reports.

## Decisions and boundaries

- Presentation code is TypeScript. Logic code is Rust.
- The HTTP framework and async runtime are intentionally undecided. Tokio and
  Axum are candidates, not requirements.
- Provider tools, databases, queues, editors, bundlers, and worker languages are
  candidates until their first implemented behavior and decision record.
- Keep `artifree-domain` independent of I/O and wire contracts. Keep
  `artifree-core` independent of concrete providers and server frameworks.
- HTTP, MCP, web, and CLI must share application behavior rather than invent
  separate storage, permission, or retry semantics.
- Generate wire contracts with `npm run generate:contracts`. Do not hand-edit
  `contracts/generated` or `packages/contracts/src/generated`.
- JSON Schema and generated TypeScript do not replace runtime input validation
  or authorization. The current inventory is not a live health response.
- The runtime SDK currently contains types only. Do not advertise a sandbox,
  capability enforcement, or a working iframe bridge until it is implemented.

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
