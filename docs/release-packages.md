# Release packages

Status: O03a, implemented ([decision 0025](decisions/0025-release-packages.md)).
A container image, the access-path guides (tunnels, private networks, a
Vercel-hosted static UI), and backup are not implemented (O03b, O04).

> **Warning.** CI archives are for testing and your own use. Do not
> redistribute them: the full license texts of the Rust crates and npm
> packages inside are not bundled yet (O03b).

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
package changes them.

Startup refuses, with a message on stderr and a nonzero exit, a missing web
directory or one without `index.html`, and an `--api-origin` equal to
`--content-origin`. It does not create a service, daemon, or autostart entry;
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

1. Stop the server.
2. Copy the whole `--data-dir` somewhere else. A consistent online backup,
   restore verification, and migration checks across versions are O04 and do
   not exist yet, so a copy of the stopped server's directory is the only
   safeguard.
3. Extract the new archive into a new directory (do not overlay the old one) and
   start it with the same `--data-dir`.

There is no tested guarantee that data written by a newer version opens in an
older one, so keep the previous package and the copied directory until you are
satisfied. Release notes do not exist yet; check the decision records for a
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
