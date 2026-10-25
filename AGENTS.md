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

## Provenance

Write original code and assets. Never copy vendor bundles, decompiled code,
private captures, account data, secrets, cookies, or proprietary implementation
snippets into this public repository. Use independently expressed requirements
and original examples. Do not claim a formal clean-room separation has occurred.
