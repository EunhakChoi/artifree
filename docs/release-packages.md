# Release packages

Status: O03a, implemented ([decision 0025](decisions/0025-release-packages.md)).
The [container image](container.md) and [remote access guides](remote-access.md)
are O03b ([decision 0026](decisions/0026-container-image.md)). Archive
signing, publishing, and license texts (O03c) are not implemented.
Backup, restore, and schema upgrade are O04
([decision 0027](decisions/0027-backup-restore-and-schema-upgrade.md),
[guide](backup-restore.md)).

> **Warning.** CI archives are for testing and your own use. Do not
> redistribute them: the full license texts of the Rust crates and npm
> packages inside are not bundled yet (O03c).

A release package is one archive per platform that runs without a repository
checkout, Rust, or `npm install`:

| Platform    | Archive                               | Built and checked on |
| ----------- | ------------------------------------- | -------------------- |
| Linux x64   | `artifree-<version>-linux-x64.tar.gz` | Ubuntu 24.04 (glibc) |
| Windows x64 | `artifree-<version>-windows-x64.zip`  | Windows Server 2025  |

No other platform (macOS, ARM, musl/Alpine) is packaged. The Linux binary was
measured, with `objdump`, to need glibc 2.34 or newer; CI does not enforce that
number. The Windows binary links the C runtime statically.

## Obtain and check

CI uploads each archive and its `.sha256` file as a workflow artifact
(`release-linux-x64`, `release-windows-x64`) for every change. Publishing to a
releases page, signing, and notarization do not exist yet, so the checksum shows
that the file is intact, not who produced it.

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
```

`RUST_DEPENDENCIES.json` lists registry crates reachable through normal
dependencies of the server, with declared license expressions only. Artifree's
own workspace crates (covered by `LICENSE`) and build-only or dev-only crates
(not linked into the binary) are intentionally left out. It does not
contain the license texts of those crates or of the npm packages bundled into
`web/` and `cli/`.

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

A newer schema is refused by an older build, and there is no downgrade: keep
the previous package and the backup until you are satisfied. Release notes do not exist yet; check the decision records for a
changed data format.

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
