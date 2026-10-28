# 0016: Reading part of a file

Date: 2026-10-28. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md),
[0004](0004-inbound-mcp-adapter.md),
[0010](0010-revision-restore-and-digests.md), and
[0013](0013-deleting-artifacts-and-the-trash.md).

## Context

A file of a pinned revision could only be read whole. A file may hold up to
5 MiB, so an agent that needed a few lines of a large file had to take all
of it into the model's context. Roadmap A11 adds bounded reads to the API,
the TypeScript client, the CLI, and MCP, without changing what a whole read
returns.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                         | Basis                                                                                                                                                                                                                                                                                                                                                                      |
| --- | ----------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| B1  | A part of a file can be read by offset and length, with where the next part starts. | Source-only: the analyzed artifact runtime read files by byte offset and length and returned the file's size; a model-facing file tool read fixed byte parts and returned the next offset, which it left out at the end. A desktop tool read by lines instead. Observed: an agent read files of 13 to 16 thousand characters whole. Decision: bytes, which every file has. |
| B2  | The parts of a text file are text and join into the file exactly.                   | Source-only: the model-facing tool skipped continuation bytes at the start of a part and cut an incomplete character at its end. Decision: the end is cut the same way, but a start inside a character is refused, because skipping bytes would lose them from the joined parts.                                                                                           |
| B3  | Parts read one after another belong to the same file.                               | Source-only: the analyzed runtime advised starting over when the revision changed during a read. Decision: a read names its revision, which never changes (0003), and returns the whole file's size and SHA-256, so the joined parts can be checked.                                                                                                                       |
| B4  | A file that is not text comes back exact.                                           | Source-only: a desktop handler decoded files leniently, which would turn invalid bytes into replacement characters (inference). Decision: as for whole reads, only a file that is UTF-8 as a whole is returned as text; any other is read by bytes.                                                                                                                        |
| B5  | The end of the file, and a range past it, have one answer each.                     | Source-only: the end was the absence of a next offset, and an offset out of range was an error. Decision: a part at the end is empty without a next offset; a part past the end is refused.                                                                                                                                                                                |
| B6  | A whole read is unchanged, and a part is subject to every rule of a read.           | Decision: one read path in `ArtifactService` for whole files and parts, with the access policy of 0003 and the trash rule of 0013.                                                                                                                                                                                                                                         |

## Decisions

1. **One read in the core.** `ArtifactService::read_file_part` reads the
   file as `read_file` does, so access, missing targets, and the trash are
   answered exactly as for a whole read, and then cuts a part from it
   (`artifree-core/src/parts.rs`). A range is an offset with an optional
   length, or the last N bytes. The part carries its offset, the whole
   file's size and SHA-256, and whether the whole file is UTF-8 text. The
   file is read whole from storage; files are at most 5 MiB, and SQLite
   reads a blob whole.
2. **Two boundaries.** By bytes, a part is exactly the range. By
   characters, used when the whole file is UTF-8 text, a part ends before a
   character the range would split; a range that starts inside a character,
   or is shorter than the character it starts with, is refused. A part read
   by characters is therefore always text, never empty before the end, and
   the parts read from each `next_offset` join into the file. A file that is
   not UTF-8 is read by bytes either way. Line-based reads are not offered:
   they need a decoded file, and they do not exist for binary files.
3. **HTTP uses standard ranges.** `GET
/v1/artifacts/{id}/revisions/{n}/files/{path}` honors one `Range:
bytes=first-last`, `first-`, or `-suffix` (RFC 9110). It answers `206`
   with `Content-Range: bytes first-last/size` and the exact bytes. A range
   that starts at or after the end of the file is `416
range_not_satisfiable` with `Content-Range: bytes */size`; a malformed
   range or several ranges are `400 invalid_request`. A unit other than
   bytes, and a request with `If-Range`, read the whole file, since the API
   sends no validator to match. Every answer says `Accept-Ranges: bytes`.
   HTTP reads by bytes: its clients store bytes, and a cut character is
   theirs to join.
4. **MCP reads by characters.** `read_file` takes optional `offset` (from 0) and `length` (at least 1, in bytes). Its result adds `offset`,
   `size`, `sha256` of the whole file, and `next_offset` when the file
   continues. Content is `utf8` when the whole file is UTF-8 and `base64`
   otherwise, as for whole reads, so a part of a text file is text the model
   can read. A refused range is `invalid_request`, or `range_not_satisfiable`
   past the end, with a message that says what to send instead.
5. **Clients.** `ArtifreeClient.readFilePart(id, revision, path, range)`
   sends a `Range` and accepts only a `206` whose `Content-Range` names the
   range it asked for; anything else is `invalid_response`. `artifree
artifact cat` takes `--offset` and `--length` and writes the part's
   bytes to stdout, unchanged.
6. **Errors.** `range_not_satisfiable` is a new error code. It is only
   used for a range past the end of the file.

## Costs and limits

- A part is cut from the whole file in memory, and its digest is computed
  from the whole file on every read; both are bounded by the 5 MiB file
  limit.
- HTTP sends no `ETag`, so `If-Range` always reads the whole file.
- A part read by characters may be shorter than asked; the next one starts
  where it ended.
- The web workspace does not read parts; the editor works on whole files.
