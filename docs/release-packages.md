# Release packages

Status: O03a, implemented ([decision 0025](decisions/0025-release-packages.md)).
The [container image](container.md) and [remote access guides](remote-access.md)
are O03b ([decision 0026](decisions/0026-container-image.md)). Release
publishing, signing, license texts, and the glibc floor (O03c) are implemented
as a workflow and checks, **but nothing has been published and no signature has
been verified** ([decision 0037](decisions/0037-release-publishing-and-supply-chain.md)).
Backup, restore, and schema upgrade are O04
([decision 0027](decisions/0027-backup-restore-and-schema-upgrade.md),
[guide](backup-restore.md)).

> **Warning.** Archives and images that CI builds are for testing and your own
> use. Do not redistribute them: they are unreviewed, unattested workflow
> artifacts that expire. Redistribute only assets of a published release
> (see [Verify a release](#verify-a-release)). No release exists yet.

A release package is one archive per platform that runs without a repository
checkout, Rust, or `npm install`:

| Platform    | Archive                               | Built and checked on |
| ----------- | ------------------------------------- | -------------------- |
| Linux x64   | `artifree-<version>-linux-x64.tar.gz` | Ubuntu 24.04 (glibc) |
| Windows x64 | `artifree-<version>-windows-x64.zip`  | Windows Server 2025  |

No other platform (macOS, ARM, musl/Alpine) is packaged. The Windows binary
links the C runtime statically.

**Linux glibc floor: 2.34.** The binary needs glibc 2.34 or newer (x86-64).
Packaging refuses a binary whose `GLIBC_*` symbol versions exceed that, and CI
repeats the check on the archive. Checked by running the package: Debian 12
(glibc 2.36, the `Debian 12` and `Container image` jobs) and Ubuntu 24.04
(2.39, the `Clean install` job). Supported only by the number, never run:
glibc 2.34 and 2.35 (RHEL, Rocky, and Alma 9; Ubuntu 22.04). Below 2.34 (Debian 11,
Ubuntu 20.04, RHEL 8) is not supported.

## Obtain and check

CI uploads each archive and its `.sha256` file as a workflow artifact
(`release-linux-x64`, `release-windows-x64`) for every change. The
checksum shows that the file is intact, not who produced it; for that, see
[Verify a release](#verify-a-release). Windows notarization or Authenticode
signing does not exist.

```sh
sha256sum -c artifree-<version>-linux-x64.tar.gz.sha256
tar -xzf artifree-<version>-linux-x64.tar.gz
```

```powershell
(Get-FileHash artifree-<version>-windows-x64.zip -Algorithm SHA256).Hash
Expand-Archive artifree-<version>-windows-x64.zip -DestinationPath .
```

The Windows binary is unsigned, so SmartScreen may warn on first run.

The archive has one top-level directory:

```text
artifree-<version>-<target>/
  MANIFEST.json            every file with its size and SHA-256
  bin/artifree-server[.exe]
  web/                     built web workspace (index.html, assets/)
  cli/artifree.mjs         the command-line client, one bundled script
  docs/release-packages.md this page
  docs/backup-restore.md   backup, restore, and upgrade (O04)
  LICENSE  THIRD_PARTY_NOTICES.md
  RUST_DEPENDENCIES.json   crates linked into the server: name, version, license expression
  LICENSES/INDEX.json      every third-party component with its license files (hashes)
  LICENSES/texts/<hash>.txt  the license texts, each stored once
```

`RUST_DEPENDENCIES.json` lists registry crates reachable through normal
dependencies of the server, with declared license expressions. Artifree's own
workspace crates (covered by `LICENSE`) and build-only or dev-only crates (not
linked into the binary) are intentionally left out. `LICENSES/` holds the
license, copying, and notice files of those crates and of the npm packages that
may be bundled into `web/` and `cli/` (the lockfile closure of the two
workspaces, which can include a package that was tree-shaken away). Packaging
fails if a component has no text and no exception recorded in
`scripts/license-exceptions.json` (none today). Not covered: the notices of the
Rust standard library and the Windows C runtime linked into the binary.

## Start

From the extracted directory:

```sh
./bin/artifree-server serve --data-dir ./data --web-dir ./web
```

```powershell
.\bin\artifree-server.exe serve --data-dir .\data --web-dir .\web
```

The server prints one JSON readiness line (`api_url`, `web_url`,
`content_origin`, `token_file`). It listens on loopback only (`127.0.0.1:7410`
and, for previews, `127.0.0.1:7411`). Open the `web_url` and paste the contents
of `token_file` (`<data-dir>/access-token`). `GET /healthz` answers `ok` without
a token. LAN and HTTPS reverse-proxy operation use the flags of decisions
[0022](decisions/0022-production-web-and-api-serving.md) and
[0024](decisions/0024-https-proxy-and-remote-access.md) (`--lan`, `--api-origin`,
`--content-origin`, `--trusted-proxy`, `--allowed-host`); nothing in the
package changes them. The same settings can be given as `ARTIFREE_*` variables
([decision 0028](decisions/0028-static-web-defaults.md), `artifree-server --help`);
a flag wins, and an invalid value stops the server with a message that names the
flag or variable and not the value.

Startup refuses, with a message on stderr and a nonzero exit, a missing web
directory or one without `index.html`, an `--api-origin` equal to
`--content-origin`, and an origin that is not `http(s)://host[:port]` with a
port from 1 to 65535. The packaged web workspace is built without a default
server; to bake one in, build the web app yourself with `VITE_ARTIFREE_SERVER`
([remote access](remote-access.md)). It does not create a service, daemon, or autostart entry;
use your platform's supervisor (systemd, a Windows service wrapper) if you want
one.

## Command-line client

`cli/artifree.mjs` needs Node.js 22.12 or newer on the machine; the server does
not. It reads the same connection settings as the repository CLI:

```sh
export ARTIFREE_TOKEN_FILE=./data/access-token
node cli/artifree.mjs artifact create --title "Demo" --dir ./site
node cli/artifree.mjs artifact preview <id>
```

## Stop

On Linux send `SIGTERM` or press Ctrl+C; the server finishes in-flight requests
and exits with status 0. On Windows press Ctrl+C in the console that runs it.
Ending the process any other way (Task Manager, `Stop-Process`, a killed
service) is an abrupt stop; SQLite recovers committed revisions after a crash,
and CI checks that a saved revision is present after the process is ended and
restarted, but treat it as a last resort.

## Upgrade in place

1. Take a backup of the running server with the new package's
   `bin/artifree-server backup --data-dir <dir> --output <file>`: it reads
   the data of older releases, proves the file restores, and needs no
   `backup` command in the old release ([backup guide](backup-restore.md)).
   Alternatively stop the server and copy the whole `--data-dir`.
2. Stop the server.
3. Extract the new archive into a new directory (do not overlay the old one) and
   start it with the same `--data-dir`. The first start upgrades the database
   in one transaction; a refused or failed upgrade leaves the file unchanged.
   `artifree-server worker run` upgrades a database the same way when it opens
   it first, so the backup comes before either starts.

A newer schema is refused by an older build, and there is no downgrade: keep
the previous package and the backup until you are satisfied. Release notes do not exist yet; check the decision records for a
changed data format.

## Verify a release

> **Status: not exercised.** No release exists. The steps below describe what
> the release workflow ([decision 0037](decisions/0037-release-publishing-and-supply-chain.md))
> is built to support. They have not been run against a real release, so treat a
> first failure as possibly a documentation or workflow defect. The workflow
> creates a **draft** release; a maintainer reviews and publishes it by hand.

Download the archive, its `.sha256` file, and `SHA256SUMS` from the release.

```sh
sha256sum -c artifree-<version>-linux-x64.tar.gz.sha256   # integrity
gh attestation verify artifree-<version>-linux-x64.tar.gz --repo EunhakChoi/artifree
gh attestation verify oci://ghcr.io/eunhakchoi/artifree:<version> --repo EunhakChoi/artifree
docker pull ghcr.io/eunhakchoi/artifree:<version>
docker image inspect --format '{{index .RepoDigests 0}}' ghcr.io/eunhakchoi/artifree:<version>
```

The last command prints the image digest; it must equal the digest in the
release notes and in the attestation output. Run the container by that digest
(`ghcr.io/eunhakchoi/artifree@sha256:...`), not by the tag. An attestation shows
the file was built by this repository's release workflow from a version tag; it
does not show the code is safe, and a Windows `.zip` still has no Authenticode
signature.

## Base image digests

Three image references are pinned by tag and digest (the Dockerfile base, the
Playwright image, and the Debian 12 verifier image in `ci.yml`). Dependabot does
not track them (decision 0037, section 5). `npm run check` verifies the
format offline. To refresh them, a maintainer does the following before each
release candidate and after a Debian security release:

1. On a networked machine, not in CI, run `node scripts/refresh-base-digests.mjs`.
   It lists each pin as `current` or `CHANGED` with the lines to edit and edits
   nothing.
2. Edit those lines. Keep the tag. Update the resolution date in the Dockerfile
   comment. The Playwright tag must keep matching `@playwright/test` in
   `package.json`.
3. Read what changed upstream (Debian security notices; the image's release
   notes) and decide whether the change is wanted.
4. Run `npm run check`, open a pull request, and wait for all CI jobs: the
   `Container image` job asserts the base's glibc and runs the package, and the
   `Debian 12` job runs the verifier on it. If the glibc of the new base differs
   from `GLIBC_VERIFIED_RUNTIME` (a new Debian major), change that constant and
   decision 0037 in the same pull request, because the support policy changes.
5. Merge, then rebuild any pending release.

## What CI checks

The `Release archive` job builds the archive on each platform, and the `Clean
install` job unpacks it into an empty directory on a fresh runner without
installing npm packages or building Rust. The check runs only files from the
unpacked directory, with an empty `PATH` and a working directory outside any
checkout, and verifies: archive checksum, every manifest hash, only regular files
and no unlisted file, link, or directory, entrypoints equal to the target's
layout, the manifest matching the archive name and the runner's platform, start and health, the packaged web page, creating and saving an artifact
with the packaged CLI, fetching its preview, stopping, restarting on the same
data and finding revision 2, and the two startup refusals above. It ran only on
those two runner images; it did not run against a real reverse proxy, tunnel, or
other machine.

Further checks (decision 0037): the `Debian 12` job checks the archive's glibc
requirement against the floor and runs the same verifier inside Debian 12
(glibc 2.36) with no network and a read-only root; the license bundle is
validated as part of the manifest check; `release.yml` is checked for syntax and
for the publishing rules in `npm run check` and in CI. The release workflow
itself has never run.
