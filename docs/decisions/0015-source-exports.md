# 0015: Exporting a revision's source

Date: 2026-10-28. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md),
[0010](0010-revision-restore-and-digests.md),
[0011](0011-web-history-and-restore.md), and
[0013](0013-deleting-artifacts-and-the-trash.md).

## Context

A revision's files could be read one at a time, but there was no way to
take a whole revision out of Artifree to keep, inspect, or open
elsewhere. Roadmap A07 adds a source export to the API, the TypeScript
client, the CLI, and the web workspace.

An export is not a migration. It carries one revision of one artifact.
History, pin and trash state, request keys, other artifacts, and the
server's data stay behind; backups of the whole service are O04.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                       | Basis                                                                                                                                                                                                                                                                                          |
| --- | --------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| E1  | A revision's files can be downloaded together, at their paths, byte for byte.     | Observed: the hosted export of a single HTML file was the stored bytes, with no runtime added. Source-only: a package of several files was downloaded as a ZIP archive of the files at their relative paths, stored with CRC-32 and not inlined. Gap: a ZIP of several files was not executed. |
| E2  | The download says what it holds, and what it holds can be checked.                | Observed: the hosted ZIP exports of designs and motion pieces had no manifest of digests. Decision: a manifest lists every file with its size, SHA-256, and media type, as the revision does.                                                                                                  |
| E3  | It says what the files need to run, and claims nothing Artifree does not provide. | Source-only: a saved copy had no guarantee of the hosted runtime, which needs the host. Decision: Artifree previews give files no capabilities (0003), so the manifest says the files need no runtime, and that resources they load from the network are not included.                         |
| E4  | Only someone who may read the artifact may export it.                             | Decision: the access policy of every read (0003) and the trash rule of 0013.                                                                                                                                                                                                                   |
| E5  | The same revision exports the same way every time.                                | Gap: whether the hosted archive was reproducible was not captured. Decision: the archive's bytes depend only on the revision and the artifact's title.                                                                                                                                         |

## Decisions

1. **An export is a read.** `GET
/v1/artifacts/{id}/revisions/{revision}/export` answers `200` with a
   ZIP archive (`Content-Type: application/zip`) and `Content-Disposition:
attachment; filename="{id}-r{revision}.zip"`, through
   `ArtifactService::export`. It needs no key, writes nothing, records no
   change, and counts against the same request limits (0005). Refusals are
   those of a file read: a missing, inaccessible, or trashed artifact, and
   revision 0 or one after the current revision, are `404 not_found`; a
   path that cannot name a revision is `400`. In the trash it answers as
   for an unknown artifact (0013 decision 2).
2. **The archive.** The first entry is the manifest
   `.artifree-export.json`; every other entry is one file of the revision
   at its package path, in path order, with its bytes unchanged. Package
   paths never start with a dot (0003), so no file has the manifest's
   name, and they are portable ASCII names that extract without
   traversal. Entries are stored without compression: a package holds at
   most 25 MiB, and a stored entry keeps each file's bytes visible in the
   archive. Every entry is dated at the revision's commit, in UTC, and
   marked as a regular file readable by everyone, so the same revision and
   title give the same bytes. Only the classic ZIP format is written; the
   package limits keep an archive far within it.
3. **The manifest.** `SourceExportManifest` in the API schema: `format`
   (`artifree.source-export.v1`), `artifact_id`, `title` (the title when
   exported, since titles are not part of a revision), `revision`,
   `created_at_ms` (the commit time), `entry` (`index.html`), `runtime`,
   and `files`, exactly the revision's listing of path, size, SHA-256, and
   media type. It names no actor. A later version changes `format`.
4. **The runtime.** `runtime` is `none`: the files need a browser and
   nothing Artifree provides, because previews give them no capabilities.
   Resources the files load from the network are not part of the export
   and may be unavailable offline; nothing checks them. When runtime
   capabilities exist (R04, R05), the files that use them say so here with
   a new value and manifest version.
5. **Other clients.** The client method `exportRevision` returns the
   archive's bytes and rejects a successful answer that is not a ZIP
   archive as `invalid_response`. `artifree artifact export <id> --output
<file> [--revision <n>]` writes the archive to a new file, never
   replacing one that exists, and prints the artifact, revision, file,
   size, and SHA-256 as JSON; without `--revision` it exports the current
   revision. MCP has no export tool: an agent reads the files it needs
   with `read_file`, and an archive in a tool result serves no model.
6. **The web workspace.** "Download revision N" in the artifact's header
   saves the export of the revision the preview shows, named as the
   server names it. It is a read, so a failure says that nothing changed
   and the button tries again. While what is shown may predate a
   replacement of the server's data it waits until the artifact is read
   again, since the same number may then name other files (0011 decision
   6). A draft in the editor is not part of any revision and is not
   exported.

## Alternatives

- **A single HTML file when the package has only `index.html`**, as the
  hosted export did. One format with a manifest is simpler to check and
  to consume; a single file can still be read on its own.
- **Compression.** Deflate would shrink text, but needs a compressor
  dependency, hides the bytes, and makes archives depend on its version.
- **A manifest outside the archive**, as response headers or a second
  request. It would be lost when the file is saved.
- **An MCP tool returning the archive in base64.** No model needs it, and
  it would move up to 25 MiB through a tool result.

## Costs and limits

- An archive is built in memory: up to about 25 MiB per request, held
  while it is sent.
- Runtime `none` states what Artifree provides; it does not prove that
  the files work offline.
- Not a backup: history, metadata other than the title, and request keys
  are not exported (O04).
- The web page offers the revision the preview shows; other revisions are
  exported after choosing them there, or with the CLI.
