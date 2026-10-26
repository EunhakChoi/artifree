# 0002: UI foundation and mandatory quality feedback

Date: 2026-10-27. Status: accepted.

## Context

External coding agents will implement and maintain Artifree. Strong compiler
feedback needs equally concrete design, contract, and integration checks. The
initial scaffold has no application feature to justify additional services,
provider frameworks, or a second frontend framework.

## Decisions

1. Keep React/Vite and introduce Tailwind CSS 4 with shadcn/ui's Base UI variant
   (`base-nova`). Add only the components used by the development shell. Own
   their source locally and retain MIT attribution. Use system fonts and local
   assets. The shell supports light/dark tokens.
2. Keep UI code in `apps/web/src/components/ui`. Extract a workspace UI package
   when a second application actually needs it. Management UI rules do not
   constrain generated artifacts; an artifact may opt into a future template.
3. Use Oxlint with `@shadcn/lint`; the current TypeScript 7 compiler is outside
   typescript-eslint's supported range. Oxlint's JS plugin API is alpha. Pin
   versions, test real negative diagnostics in CI, and revisit compatibility
   when changing versions. Prettier remains the formatter and Stylelint checks
   CSS validity.
4. Extend mandatory quality checks to Rust docs/pedantic lints, both language
   dependency boundaries, schema rejection, command-line error protocols,
   browser accessibility, minimum compiler version, and Windows portability.
   Public TypeScript type names remain stable when schema metadata changes.
5. Enforce the complete CI outcome through one stable Quality gate. Pin action
   commits and downloaded tools. Pin Ubuntu 24.04 / Windows Server 2025 runner
   labels and use Actions with a supported runtime. Run known-advisory and license checks without
   silently succeeding when the scan or its network access fails.
6. Keep product behavior unimplemented until its first vertical slice. Do not
   add an HTTP framework, async runtime, provider SDK, database, Python worker,
   or CLI-Anything harness merely to satisfy an example in a local skill.

## Costs and maintenance

Copied UI components require reviewed upstream updates. Use the shadcn CLI's
dry-run/diff support rather than overwriting local changes. Additional lint and
browser tooling adds development dependencies and CI time. Zero-warning gates
require specific, documented exceptions for verified false positives. They do
not justify blanket ignores or tests that cannot fail.

The actual commands, exceptions, scope, and limitations live in
[quality.md](../quality.md). Tokio/Axum and all provider choices remain open.
