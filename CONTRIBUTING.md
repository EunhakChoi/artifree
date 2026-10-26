# Contributing

Artifree is a scaffold. Keep actual behavior and future plans distinguishable.
Read [AGENTS.md](AGENTS.md), [the architecture](docs/architecture.md), and
[the quality policy](docs/quality.md) before changing module boundaries.

## Local verification

```sh
npm ci
npm run check
npm run build
npx --no-install playwright install chromium firefox
npm run test:smoke
```

Run Firefox with `ARTIFREE_BROWSER=firefox npm run test:smoke` on Unix, or set
`$env:ARTIFREE_BROWSER = 'firefox'` before the same command in PowerShell.
Unset that variable afterwards to return to Chromium.

For dependency and workflow checks on Linux x64:

```sh
node scripts/install-ci-tools.mjs
.artifree/tools/actionlint
.artifree/tools/cargo-deny --locked check
npm audit --audit-level=low
```

The installer verifies fixed archive digests. On other platforms install
actionlint 1.7.12 and cargo-deny 0.20.2 through their upstream instructions.
Online advisory checks are separate from the deterministic local gate.

After wire DTO changes, run `npm run generate:contracts`. After changes to the
standalone architecture source, run `npm run build:architecture`. Review and
commit generated changes with their source; CI checks them without rewriting
them. Lockfiles and `.gitattributes` keep builds reproducible across platforms.

## Review a change

Explain the concrete behavior, the relevant module boundary, and the tests that
would fail if that behavior broke. Run targeted checks while editing, then the
full relevant gate. A green build is evidence for the checks that ran, not proof
of product completeness.

Do not weaken assertions, add ignores, disable rules, or add unchecked tokens to
quiet a failure. A verified false positive needs a narrow exception and a reason.
Avoid unnecessary interfaces and provider dependencies before the first working
behavior requires them. Keep application behavior shared across HTTP, MCP, web,
and CLI.

Rust 1.85.0 is the supported minimum; the pinned toolchain also runs formatting,
Clippy, and rustdoc. Test new dependencies against both rather than assuming a
newer compiler proves minimum-version compatibility.

The required CI aggregate is named **Quality gate**. Branch rules should require
this status before merging. Add every new verification job to its `needs`; the
workflow policy tests reject omissions. Dependency updates arrive through
Dependabot and follow the same review and checks.
