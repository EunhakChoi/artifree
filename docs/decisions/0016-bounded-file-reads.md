# 0016: Reading part of a file

Date: 2026-10-28. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md),
[0004](0004-inbound-mcp-adapter.md),
[0005](0005-request-limits.md), and
[0013](0013-deleting-artifacts-and-the-trash.md).

## Context

A file of a pinned revision could only be read whole. An agent that edits
one function of a 5 MiB script had to take all of it into its context.
Roadmap A11 adds reads of part of a file to the API, the TypeScript
client, the CLI, and MCP, without changing what a whole read returns.

An earlier version of this slice did not pass review. Every finding came
from two rules it left implicit:

- **Which `Range` fields the API serves.** It parsed the field as each
  finding showed a new form: several field lines, an inverted range, a
  suffix of zero bytes, and positions that do not fit in 64 bits.
- **Which answers the client accepts.** It checked the answer against the
  request one property at a time; a shorter part, and then an inverted
  `Content-Range` at the end of the file, still passed.

This record states both rules (decisions 3 and 5) and carries every case
those reviews found as a requirement.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                                       | Basis                                                                                                                                                                                                                                                                                                                   |
| --- | ------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| B1  | A file can be read in parts of a chosen size, from a chosen offset, until its end.                | Observed: a hosted agent read 13–16 thousand characters of a file whole. Source-only: the hosted runtime read files by byte offset and length, returned fewer bytes at the end and the file's size, and advanced the offset until it reached the size; a model-facing tool returned the next offset, absent at the end. |
| B2  | Parts of a text file are text, and the parts read in order join into the file.                    | Source-only: the model-facing tool skipped continuation bytes at the start of a part and cut an incomplete sequence at its end, so a part that started inside a character lost bytes. Decision: a start inside a character is refused, and an end inside one moves back before it.                                      |
| B3  | The parts can be shown to come from one file.                                                     | Source-only: the hosted runtime said a recently edited file could return its previous bytes. Decision: every part is read from a pinned revision and carries the whole file's size and SHA-256.                                                                                                                         |
| B4  | Any file can be read in parts, with its exact bytes.                                              | Source-only: the desktop tool decoded leniently and refused files that were not text. Inference: invalid bytes became U+FFFD. Decision: a file that is not UTF-8 as a whole is read by bytes, as base64.                                                                                                                |
| B5  | Reading at and past the end of a file has one answer each.                                        | Source-only: the end was an absent next offset, and an offset out of range was an error. Decision: decisions 3 and 4.                                                                                                                                                                                                   |
| B6  | A part is refused wherever the whole file is, and a whole read is unchanged.                      | Decision: the access policy of every read (0003) and the trash rule of 0013.                                                                                                                                                                                                                                            |
| B7  | Over HTTP, `Range` has one meaning for every well-formed field, and every other field is refused. | Decision: RFC 9110, section 14, for one byte range (decision 3). The earlier version's review found several field lines, `bytes=3-1`, `bytes=-0`, and positions past 64 bits answered inconsistently.                                                                                                                   |
| B8  | A client accepts only the part it asked for.                                                      | Decision: decision 5. The earlier version's review found a shorter part, and an inverted `Content-Range` at the end of the file, accepted.                                                                                                                                                                              |

## Decisions

1. **A part is a range of a pinned file's bytes.**
   `ArtifactService::read_file_part` reads the file as `read_file` does,
   with the same access, trash, and revision checks, and then cuts the
   range from it (`artifree_core::parts`). A range is `length` bytes from
   `offset`, or to the end without a length, or the last `length` bytes.
   One that ends past the end of the file stops there. One that starts at
   the end selects an empty part; one that starts past it is refused as
   `range_not_satisfiable`, with the file's size. Every part carries its
   offset, the whole file's size and SHA-256, and whether the whole file
   is UTF-8. Reading the whole file is the range from 0 to the end, with
   the same bytes as before.
2. **Two boundaries.** Over HTTP a part is cut at bytes, as `Range`
   means. Over MCP a part of a UTF-8 file is cut between characters: a
   range that starts inside a character is refused as `invalid_request`,
   one that ends inside one ends before it, and one shorter than the
   character it starts with is refused, since its part would be empty
   before the end of the file. Parts of four bytes or more always make
   progress. A file that is not UTF-8 as a whole is cut at bytes either
   way.
3. **One parser of the `Range` field.** `GET
/v1/artifacts/{id}/revisions/{revision}/files/{path}` serves one byte
   range (RFC 9110, section 14):
   - The field's lines form one list. Without `Range`, with a unit other
     than `bytes`, or with `If-Range`, which no validator of this API can
     match, the whole file is read as before: `200`, now with
     `Accept-Ranges: bytes`.
   - The unit is matched without regard to case; whitespace around it,
     `=`, and the range, and empty list elements, are ignored.
   - `bytes=first-last`, `bytes=first-`, and `bytes=-suffix` are served.
     A position too large for 64 bits is past the end of any file: as a
     last position it means the end, and as a suffix the whole file.
   - More than one range, in one line or several, a last position before
     the first, and anything else are refused as `400 invalid_request`
     before anything is read.
   - A range that selects no bytes is `416 range_not_satisfiable` with
     `Content-Range: bytes */{size}`: it starts at or past the end of the
     file (so an empty file has no parts), or it is `bytes=-0`. The
     refusals of a read come first: a missing or trashed artifact is
     `404`, as for a whole read, and says nothing of the file's size.
   - A served range is `206 Partial Content` with `Content-Range:
bytes {first}-{last}/{size}` and the headers of a whole read.
4. **MCP.** `read_file` takes optional `offset` (default 0) and `length`
   (at least 1; default to the end), cut between characters. Its result
   adds `offset`, `next_offset` (where the next part starts, absent once
   the part reaches the end), and `sha256`, and `size` is the whole
   file's. A part of a UTF-8 file is `utf8`; a part of any other file is
   `base64`, also where its bytes alone would be text. An offset at the
   end returns an empty part without `next_offset`, so a whole read of an
   empty file is unchanged; an offset past it is `range_not_satisfiable`,
   with the size in the message.
5. **One rule for accepting a part.** `ArtifreeClient.readFilePart(id,
revision, path, { offset, length })` asks for `bytes=offset-last` or
   `bytes=offset-`, and resolves only with a `206` whose `Content-Range`
   is `bytes {first}-{last}/{size}` and whose body has exactly
   `last - first + 1` bytes, where `first` is the offset asked for and
   `last` is one before the end of the range or of the stated size,
   whichever comes first. Every other answer is `invalid_response`,
   including a `200` with the whole file: the status and `Content-Range`
   are checked before the body is read, so a server or proxy that ignored
   `Range` does not make the client read a whole file it did not ask
   for. An offset or length that is not a whole number, a length of 0, or
   a last position past `Number.MAX_SAFE_INTEGER` is refused before
   sending.
   `artifree artifact cat` takes `--offset` and `--length` and writes
   that part; `artifact history` lists each file's size and SHA-256.
6. **A new error code.** `range_not_satisfiable` joins the error codes of
   the API and MCP; the client accepts it.

## Alternatives

- **Lines instead of bytes**, as the desktop tool read. Lines have no
  fixed size, so a part could not be bounded, and they do not apply to
  files that are not text.
- **Skipping continuation bytes at the start of a part**, as the
  model-facing tool did. A part that starts inside a character then
  silently loses bytes; refusing it says to use `next_offset`.
- **Several ranges in a `multipart/byteranges` answer.** No client needs
  them, and one range per request keeps both rules small.
- **An `ETag` of the file's SHA-256, matched by `If-Range`.** A pinned
  revision never changes, so a validator adds no safety; `If-Range` reads
  the whole file.
- **Character boundaries over HTTP.** `Range` is defined in bytes, and
  proxies and caches rely on that.

## Costs and limits

- A part is cut from the whole file, which is read and hashed for every
  part: up to the package's file limit per request.
- The parts of a file are consistent because they name a pinned revision;
  reading the current revision again may name another file.
- Over HTTP a part may end inside a UTF-8 character; joining the parts in
  order restores the bytes.
