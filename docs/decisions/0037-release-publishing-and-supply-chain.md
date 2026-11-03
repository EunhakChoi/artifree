# 0037: Release publishing and supply chain

Date: 2026-11-04. Status: accepted. Builds on
[0025](0025-release-packages.md) and [0026](0026-container-image.md). Roadmap
row: O03c. Independent Artifree design: these are repository release-engineering
choices, not product behavior.

## Context

O03a and O03b produce a Linux x64 and a Windows x64 archive and a container
image, all checked in CI, none published, none signed, and without the license
texts of the code inside. This change adds what is needed to publish them
responsibly: license texts in the archives, a stated and enforced glibc floor, a
workflow that can publish from a version tag and from nowhere else, signed
provenance, and a procedure for the pinned base image digests.

**Nothing has been published.** No tag, release, image, or attestation exists.
The workflow has not run on GitHub, and no signature or checksum of a real
release has been verified. Those belong to the first release candidate (see
"Not implemented").

## Decisions

### 1. License texts

- Each archive carries `LICENSES/INDEX.json` and `LICENSES/texts/<32 hex>.txt`
  (the name is the first 128 bits of the text's SHA-256, because the archive
  writer limits paths to 100 bytes; the index holds the full hash). Identical
  texts are stored once. The index lists every component (`cargo` or `npm`, name,
  version, declared license expression, file names and hashes), sorted by code
  point, so the same inputs give the same bytes. The files are listed in
  `MANIFEST.json`, so the clean-install and container checks cover them.
- **Rust:** the registry crates reachable through normal dependencies of
  `artifree-server` for the target (the set `RUST_DEPENDENCIES.json` already
  used, from `cargo metadata --locked`), with the license, copying, notice, and
  unlicense files at the top of each crate's source directory plus Cargo's
  `license_file`. For the Linux target that was 99 crates.
- **npm:** the closure, in `package-lock.json`, of the dependencies, optional
  dependencies, and peer dependencies of the `apps/web` and `apps/cli`
  workspaces. It over-approximates what a bundler includes (a tree-shaken
  package may be listed) and cannot miss one. Installed `node_modules` must match
  the lockfile version. The CLI bundle is built with a metafile and packaging
  fails if it contains an npm package outside that closure. The web bundle is
  produced by Vite, which this change does not touch, so only the closure
  covers it. 19 packages at the time of writing (`@types/react` is among them
  as an optional peer of a runtime dependency).
- **Failure rule:** a component with no license text fails packaging unless
  `scripts/license-exceptions.json` has an entry `ecosystem:name@version` with a
  reason (empty today). An exception for a component that does have a text, or
  for none in the package, also fails, so the list cannot go stale. An empty
  file, a license file that is a link, and a version mismatch between lockfile
  and installed package fail too.
- `verify-release.mjs` (and so the container check) re-validates the bundle in
  the unpacked tree: index format, every text present and named after its hash,
  every component having texts or a recorded exception, nothing unreferenced.
- **Not covered, and a risk to settle before publishing:** the Rust standard
  library and compiler runtime linked into the binary, the C runtime on
  Windows (statically linked, `crt-static`), and system libraries are not crates
  in `Cargo.lock`, so their notices are not bundled. Build-only tools (Vite,
  Tailwind, esbuild) are not shipped and not listed. Whether the existing
  `THIRD_PARTY_NOTICES.md` plus the bundle is sufficient for these is a legal
  question this change does not answer.

### 2. glibc floor

- **Policy:** the Linux x64 package supports glibc **2.34 or newer** on
  x86-64. The floor is the measured requirement of the binary (`objdump -T`
  showed 2.34 at O03a; the ELF reader below reports 2.34 for a release build made
  for this change, and `objdump -T` on the same file agrees).
- **Enforcement:** `scripts/lib/elf-glibc.mjs` reads the `.gnu.version_r`
  section of the ELF file with Node built-ins (no binutils on the runner or the
  developer machine). `package-release.mjs` refuses a Linux binary that
  requires a `GLIBC_*` version above `GLIBC_FLOOR` (`scripts/lib/release-layout.mjs`)
  and a binary that requires no versioned glibc symbol at all (a parse that
  finds nothing would otherwise pass any floor). `node scripts/check-glibc.mjs
<archive>` repeats the check on the uploaded archive in CI. Raising the floor
  is a decision change, not a constant edit. Only symbols of glibc are checked;
  the binary also needs `libgcc_s.so.1`, which every supported distribution
  ships.
- **Verified by running:** the new `Debian 12 (glibc 2.36)` job runs
  `verify-release.mjs` (start, health, web page, CLI create/save/preview, restart,
  backup/restore, refusals) from the archive inside the official
  `node:22.12.0-bookworm-slim` image (Debian 12, pinned by digest, no network,
  read-only root), after `check-glibc.mjs --runtime` asserted that the machine
  has the glibc recorded as `GLIBC_VERIFIED_RUNTIME` (2.36). The existing
  `container` job runs the archive in `debian:bookworm-slim` and now also asserts
  that image's glibc is 2.36. The runner (Ubuntu 24.04, glibc 2.39) runs the
  archive in the `install` job.
- **What that means:** verified by running: glibc 2.36 (Debian 12) and 2.39
  (Ubuntu 24.04). Supported only because the number is at or above the floor,
  not run: 2.34 (RHEL, Rocky, Alma 9) and 2.35 (Ubuntu 22.04). Not supported:
  anything below 2.34 (Debian 11, Ubuntu 20.04, RHEL 8), musl, Alpine, ARM.
- The choice of the Node image over plain `debian:12`: the Debian base image has
  no `libstdc++`, which Node needs, and installing it would need the network.

### 3. Release workflow (`.github/workflows/release.yml`)

- **Triggers:** a push of a tag matching `v*.*.*`, and `workflow_dispatch`
  without inputs, which is a dry run (the publishing jobs require a tag push).
  No `pull_request`, `pull_request_target`, `workflow_run`, schedule, or branch
  trigger. A merge of a pull request therefore cannot publish. Merging this
  change creates no tag.
- **Permissions:** `permissions: {}` at the top; every job declares its own.
  Only `release` (`contents: write`, `actions: read`, `id-token: write`,
  `attestations: write`) and `publish-image` (`contents: read`,
  `packages: write`, `id-token: write`, `attestations: write`) hold anything
  above read. Both require `github.event_name == 'push'`, a tag ref, and
  `github.repository == 'EunhakChoi/artifree'` (not a fork, not a dry run), and
  wait for `build`, `image-check`, and `approve`. The only secret is the job's
  `GITHUB_TOKEN`; nothing is interpolated from the tag into a shell script (the
  tag goes through `env`). Checkouts do not keep credentials.
  `scripts/lib/workflow-policy.mjs` (`checkReleaseWorkflow`) enforces all of
  this in `npm run check` with a negative test for each rule, and `checkWorkflow`
  now also refuses write tokens anywhere in `ci.yml`.
- **Jobs, in order:**
  1. `verify-tag` (tag pushes): the tag equals `package.json` and the Cargo
     workspace version (`vX.Y.Z` or `vX.Y.Z-pre`); the commit is an ancestor of
     `origin/main`; and the commit has a completed **push** run of `ci.yml` on
     `main` whose newest non-cancelled, non-skipped run succeeded (Actions runs
     API with the job token). A pull-request run of the same commit, another
     workflow, or a cancelled newer run (CI cancels superseded runs) neither
     counts nor blocks. The quality gate is required through that run instead of
     re-running the checks here.
  2. `build` (Linux and Windows, as the CI `package` job): build, package,
     `verify-release.mjs` on the same runner, upload as a workflow artifact
     that expires in 7 days.
  3. `image-check`: `verify-container.mjs` on the Linux archive.
  4. `approve`: a no-op job that is the **only** job with an environment
     (`release`). Configure required reviewers there and a tag needs exactly one
     approval, before any side effect. The publishing jobs use no environment.
  5. `release`: after the approval, repeats the tag, main-ancestry, and CI
     checks (the approval can come days after step 1), then verifies that the
     downloaded set is exactly both archives, their `.sha256` files, `SHA256SUMS`
     (listing exactly those archives at their real hashes), and the notes; attests
     the files of `SHA256SUMS`; refuses to continue if a release for the tag
     exists; and creates a **draft** GitHub Release (`--draft --verify-tag`; a
     `-pre` version is marked prerelease). The notes name the image tag and say
     it is pushed afterwards. **A person reviews the draft and publishes it, only
     after `publish-image` succeeded.**
  6. `publish-image`: runs after `release`, so the image push is the last
     irreversible step. It builds the image from the verified archive with the
     repository Dockerfile, logs in, **refuses to overwrite an existing tag**
     (`docker manifest inspect` must report the tag as unknown; any other answer
     stops the job), pushes `ghcr.io/<owner>/artifree:<version>` (no `latest`),
     reads the digest, and attests that digest. The pushed image is rebuilt from
     the verified archive, not the byte-identical image of `image-check`.
- **Recovery** (no automation): a failure after the first side effect leaves
  what was done. Delete the draft release, the image version if pushed, and the
  tag, fix the cause, and release a new version. The workflow refuses to
  overwrite an existing release or image tag, so provenance recorded for earlier
  bytes is never silently replaced. The same steps are in
  `docs/release-packages.md`.
- The workflow's helpers (`scripts/release-assets.mjs`) hold every decision the
  workflow makes, so they are unit-tested.
- Third-party action: only `actions/attest-build-provenance` v4.2.2, pinned to
  `4d101475d8b20a2381f78447822ac1eab6504dd8`. It is GitHub-owned, and creates
  GitHub artifact attestations (Sigstore-signed SLSA provenance) with the
  workflow's OIDC identity, so no signing key exists to store or rotate. All other
  actions are the ones `ci.yml` already pins. `docker login` uses the job token
  through stdin and no login action. `create-storage-record` is off to keep the
  permissions minimal.
- **Exercised before the first release:** `actionlint` (existing dependencies
  job) and the policy tests run on every pull request; the helper scripts are
  unit-tested; every build, verify, and image step is the same script the CI
  `package`, `install`, and `container` jobs run. The workflow itself,
  including the dry run, cannot run until this file is on the default branch
  (GitHub only dispatches workflows present there), and the publishing jobs
  cannot run without a tag.

### 4. Archive and image provenance

GitHub artifact attestations as above: the archives through `SHA256SUMS`, the
image by digest. **How consumers retrieve them:** `actions/attest-build-provenance`
(a wrapper of `actions/attest` v4.2.1 at the pinned commit) uploads every
attestation to the GitHub attestations API ("it will be uploaded to the GH
attestations API and associated with the repository", README of `actions/attest`
at the pinned commit), so the default `gh attestation verify <file|oci://image>
--repo EunhakChoi/artifree` finds it with no bundle file. `create-storage-record:
false` is not a switch for that upload: it only controls an "Artifact Metadata
Storage Record" (the "Linked Artifacts" page), which that README says needs an
organization-owned repository and `artifact-metadata: write`; it is off here so
the permissions stay minimal. `push-to-registry: true` additionally stores the
image's attestation in the registry (readable with `--bundle-from-oci`, which
the documents do not rely on). This was settled from those READMEs and the
`gh attestation verify` source (`--repo`, `--signer-workflow`, `--bundle`,
`--bundle-from-oci`, `oci://` form); the local GitHub CLI used here (2.45) has no
`attestation` command, so `--help` could not be run, and nothing has been
verified against a real attestation. The documents use `--signer-workflow
EunhakChoi/artifree/.github/workflows/release.yml` so that only attestations
made by this workflow pass. `checkReleaseWorkflow` and a documents test pin the
combination (archives: `subject-checksums` of `dist/SHA256SUMS`, no registry
push; image: name and digest with `push-to-registry`; both
`create-storage-record: false`; the two `gh attestation verify` lines in
`docs/release-packages.md`), so the workflow and the documents cannot drift. The
attestation shows the file was built by this workflow, not that the code is
trustworthy. There is no SBOM, no Authenticode signature for the Windows binary
(SmartScreen may warn), and no image vulnerability scan.

### 5. Base image digests

Three references are pinned by tag and digest: `debian:bookworm-slim` in the
Dockerfile, the Playwright image and `node:22.12.0-bookworm-slim` in `ci.yml`.
`node scripts/refresh-base-digests.mjs --check` (in `npm run check`, offline)
requires every `@sha256:` reference in the Dockerfile and workflows to be
`name:tag@sha256:<64 hex>`, rejects a bare digest or `latest` (it could not be
refreshed), requires a Dockerfile `FROM` and a workflow `container`/`services`
image to be pinned, and requires one reference to name one digest everywhere.
Without `--check`, a person on a networked machine runs it to read each tag's
current digest from its registry (anonymous pull) and gets the lines to edit; it
edits nothing and refuses to run when `CI` is set. The checklist is in
`docs/release-packages.md`. The Debian digest it pins today matched the
registry when this was written; the registry served another digest
(`2f05f8a7...`) for the pinned Playwright tag, which is why a recorded procedure
is needed. That pin was not changed here.

**Dependabot is not used** for these pins: it would propose a new digest
without re-running the glibc and Debian-version review, and the Playwright
image must move together with the `@playwright/test` version (a test checks the
tag), which Dependabot does not coordinate. Humans own the refresh and run it
before each release candidate and after a Debian security release.

### 6. Before publication

CI archives and images remain test artifacts: workflow artifacts that expire,
not reviewed, not attested. The documents keep the warning against
redistributing them. The license-text reason is gone; unattested, unpublished
output is the reason now.

## Limits of the checks

- The ELF check reads `GLIBC_*` versions only. It says nothing about `GLIBCXX`
  or other libraries; the Debian 12 run is the real check for those.
- The npm closure covers the runtime `dependencies` of `apps/web` and
  `apps/cli`, where the web workspace's runtime libraries are declared; build
  tools (Vite, Tailwind, esbuild) are devDependencies and are not bundled.
- The digest check covers workflow `run:` steps, `container`/`services`/compose
  images, Dockerfile `FROM` and `--from`; the operator examples in `docs/` run
  locally built images and are not checked.

## Not implemented, to be recorded at the first release candidate

- Pushing a tag, running the publishing jobs, creating any release, image, or
  attestation. Nothing has been pushed or created.
- Running `gh attestation verify` or the checksum check against a real release;
  confirming that the draft's assets and notes read correctly; the first
  `workflow_dispatch` dry run.
- Notices for the Rust standard library and the Windows C runtime; legal
  review of the bundle (see 1).
- glibc 2.34 and 2.35 runs; other CPUs, musl, macOS, Windows Authenticode.
- SBOM, image scan, reproducible builds, signing of the git tag.

## Repository settings required before the first release

These cannot be set from the repository and are not set:

1. Create the `release` environment with required reviewers and allow
   deployments only from tags `v*.*.*`. Only the `approve` job uses it, so a tag
   needs one approval, before anything is drafted or pushed.
2. A tag ruleset for `v*`: only maintainers create; no update or delete.
3. Branch protection that requires the `Quality gate` status on `main` (the
   release job checks it again for the tagged commit).
4. The registry and namespace: this change uses GHCR under the repository
   owner (`ghcr.io/eunhakchoi/artifree`). Confirm that choice. A new package
   starts private; set its visibility and let the repository's workflows write
   to it.
5. Artifact attestations must be available for the repository (public
   repository, or a plan that supports them).
6. Optionally immutable releases.

## Alternatives rejected

- Publishing from the `package` job of `ci.yml`: a write token in pull-request
  CI. Rejected; `ci.yml` is now policed to hold none.
- A reusable workflow called from `ci.yml` as the dry run: the caller must grant
  every permission the callee requests, which would widen pull-request
  permissions.
- Cosign with a key, or keyless cosign: extra tooling and a third-party action
  with no gain over GitHub attestations for this repository.
- `objdump` for the floor check: depends on binutils on every machine; the ELF
  reader is about 100 lines and tested with fixtures.
- Dependabot for Docker digests: see 5.
