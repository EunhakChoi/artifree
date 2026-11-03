# 0035: Per-file conditional edit

Date: 2026-11-03. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) (a mutation has a base revision and
an idempotency key), [0004](0004-inbound-mcp-adapter.md) (M3, M4 and M5),
[0005](0005-request-limits.md), [0010](0010-revision-restore-and-digests.md)
(file digests, restore) and [0012](0012-duplicate-artifacts.md). Roadmap row:
X01. The command, its route and tool names, and every rule below are Artifree's
own decisions; nothing here is an observation of another product, and M3's
"no merge of stale edits" is kept (decision 14).

## Context

`save_revision` and `restore_revision` are whole-revision compare-and-set: the
commit succeeds only if the base revision is still the head. A writer that
touched one file loses to any writer that touched another file, though their
changes do not overlap. An agent that edits `notes.txt` while a person saves
`index.html` must read everything again and resend.

This record adds **a separate command** that applies a change when each file it
names is, in the current revision, exactly what the caller expects, whatever
else changed since. If any one expectation fails, nothing is written. The two
strict commands keep their meaning; they are not given a mode.

## Decisions

1. **A separate command, in every adapter.** `ArtifactService::edit_files`;
   `POST /v1/artifacts/{id}/edits`; the MCP tool `edit_files`; the client
   function `editFiles`; the CLI command `artifree artifact edit`. `save` and
   `restore` are unchanged in every behavior and every test (a test pins this:
   decision 11). The store gets one more method, `commit_edits`; the existing
   `commit_revision` has the same body as before, with its final steps moved to
   a function the new method shares.
2. **The request.** `{ base_revision, edits }` and an `Idempotency-Key` (MCP:
   `idempotency_key`). An edit is `{ op: "put", path, encoding, content,
expect }` or `{ op: "delete", path, expect }`: the shape of a save's change
   plus `expect`, which has no default. `expect` is `"absent"` (put only: the
   path must not be a file) or the 64 lowercase hexadecimal digits of the
   SHA-256 of the bytes the file must have. A delete takes only a digest:
   deleting what is not there is no edit. Upper-case or short digests are
   `422 invalid_request`. Content, encodings and paths are those of a save
   (package path rules, 5 MiB per file, 25 MiB per package, 256 files).
3. **What is compared.** The file **in the current revision when the commit
   happens** against the edit's `expect`. Not the base revision's file: the
   caller states the digest it read, and the store judges it against the head.
   A caller that read revision 3 and finds revision 5 current is not in
   conflict by that; it conflicts only if a file it names changed (or appeared,
   or vanished) between the revision it read and the head.
4. **Files no edit names.** They are carried over from the current revision,
   as they are then, even if they changed after the base. This is the "stale
   disjoint" case, and the only way the new revision differs from a save's.
5. **The base revision.** Required, and a revision the artifact has (`1` to the
   current one); `0` or a later one is `404 not_found`. It is **not** a
   condition and does not fence the commit. It is the revision the caller read
   the digests from; it is part of the request's identity (decision 9) and the
   commit response is the same as a save's. This is how the rule "a mutation
   names a base revision and carries an idempotency key" holds: the request
   names its base and its key, and the conflict decision is made per file. An
   adapter may derive `expect` from the base's manifest (the CLI does, decision
   13).
6. **Shape of a valid request.** At least one edit (`422`), at most 512
   (`MAX_CHANGES`, `422`, judged before any edit is decoded), each to a different
   path (`422`, `DuplicatePath`). A result that is not a valid package (no
   `index.html`, two paths differing by case, a file path that is also a
   directory, over a size or count limit) is `422`, as for a save. A request
   whose every expectation holds but whose result equals the current revision
   is `422 invalid_request` ("changes do not modify the package"), the rule of a
   save; a conflict is reported before it, so bytes equal to the caller's own
   base are no no-op once their file moved on.
7. **All or nothing, in one step.** The store method runs in one `BEGIN
IMMEDIATE` transaction: the request key lookup (replay, reuse, trash), the
   read of the head's listing, every expectation, the derivation of the new
   package from the head, the new revision's files, the request key and the
   change feed entry. SQLite lets one writer hold the database at a time, so no
   commit can land between the expectations and the new revision, in this
   process or another. The revision is `head + 1` of the head the expectations
   were judged against. There is no retry loop and no second attempt; a writer
   that waits for the lock sees the head the winner left, and its own
   expectations are judged against that. One failed expectation returns before
   anything is written; the transaction is rolled back. Two writers racing on
   the same file: exactly one wins and the other is a conflict that names the
   file with the winner's digest. Two writers on different files: both apply,
   one after the other, neither overwriting the other's file.
8. **The error: `409 file_conflict`.** A new code, not `revision_conflict`: for
   the strict commands that code means "the head moved, read again and
   reapply", and an agent that handles it may not look for per-file facts. Here
   the head moving is not the failure. The body is the usual error body with
   `current_revision` and two more members: `conflicting_files`, the first 32
   failures in path order, each `{ path, current_sha256 }` with `current_sha256`
   `null` when the path is not a file now, and `conflicting_files_total`, how many
   edits failed (at least the length of the list). Every edit is judged, not
   the first only, so the list is exhaustive up to the bound; beyond 32 the
   caller reads the revision. The strict commands never answer `file_conflict`
   and this command never answers `revision_conflict` (a test each way).
9. **Idempotency is the existing machinery.** The same `RequestKey` and the
   same store key table, scoped by actor: one key space for every mutation. The
   fingerprint covers the operation (`edit_files`), the artifact, the base, and
   for each edit, sorted by path, its change (path; for a put the digest of the
   bytes) and its expectation. The same key and request returns the first
   receipt with `replayed: true`, also after the head moved and even if the
   edit's own conditions would now fail (the store settles the key before the
   expectations). The same key for other content, another base, or a save, is
   `request_key_reused`. A rejected request (a conflict, a no-op, an invalid
   package, any 4xx) does not record its key, so the caller sends the corrected
   request under the same key (a test each).
10. **Order of judgement.** Arguments that are invalid in themselves (a
    malformed id, path, digest or encoding, too many edits) are refused first,
    as for a save, whichever artifact is named; they say nothing about it.
    Then, in the service: the artifact (`404` for a missing, other actor's,
    trashed or purged one, the same body), then an empty or repeating request
    and the base (also `404` for a trashed artifact, as for a save, decision
    0013), then the store (key, expectations, package). A caller without access
    learns nothing from how its edits were judged (a test with a second actor).
    In the trash, only the replay of an edit made before the delete succeeds.
11. **The strict meaning is pinned.** Tests: the same stale base that makes
    `save_revision` fail with `revision_conflict` and the current revision lets
    `edit_files` of an untouched file commit, at the service, over HTTP and over
    MCP; an edit and a save share a key space. `commit_revision` and its tests
    are not changed.
12. **The new revision is read back.** After a new commit (not a replay),
    `ArtifactService::edit_files` reads the revision and the one before it, and
    checks: the listing is exactly the previous one with the edits applied (same
    paths, and for each its size and SHA-256), and the stored bytes of every put
    file equal the bytes sent. A mismatch is `500 internal` with a fixed message
    that does not say which check failed; the revision exists, and the message
    says to check the history. This is the server's check of its store, shared
    by HTTP and MCP; the client function checks too (decision 13). Tests use a
    store that misreports the bytes, a digest, and an extra file.
13. **Clients.** The TypeScript function `editFiles(client, id, request,
options)` is not a method of `ArtifreeClient`, because that class is in the
    web workspace's main bundle, which has no room (decision 0033); it reaches
    the client through its now public `json`. It accepts the answer only if the
    revision follows `base_revision`, every put file is listed with the size
    and digest of the bytes sent (computed here, with `crypto.subtle`; base64 is
    decoded first), and no deleted file is listed; otherwise `invalid_response`
    with a message to retry under the same key. A `file_conflict` becomes a
    `FileConflictError` with `files` and `total`, accepted only if the list has at
    most 32 files with valid digests and a total that is not smaller; a list that
    does not fit stays a plain error with code `file_conflict`. The CLI
    `artifree artifact edit <id> --base <n> (--put <path>=<local>)...
(--delete <path>)...` sets each `expect` to the file's digest in revision
    `n` (`absent` for a put of a path `n` lacks); a delete of a path `n` lacks is
    a usage error. On a conflict it prints `error[file_conflict]`,
    `current_revision=`, one `conflict <path> <sha256|absent>` line per listed
    file, and `conflicts=<total>` on stderr; stdout stays empty.
14. **Not a merge.** A file is replaced whole or removed. Two edits to one file
    are not combined, a conflict is never resolved by the server, and a
    caller that wants a three-way merge reads the current file and sends it
    with the digest it read. M3's "no merge of stale edits" holds: a stale base
    applies only where each named file is byte-identical to what the caller
    read.
15. **Contract 0.13.0.** `EditFilesRequest`, `FileEdit`, `ConflictingFile`, the
    error code `file_conflict` and the two members of the error body; MCP tool
    `edit_files` (a commit tool; 23 tools in all). The schema publishes
    `maxItems` 512 and 32 and the `expect` patterns, and a test holds them to the
    numbers above.

## Limits and what is not implemented

- **Implemented:** the above, over HTTP, MCP, the TypeScript function and the
  CLI, against the local SQLite store.
- **Not implemented:** a web workspace control (the workspace saves with the
  strict command and its draft recovery is unchanged); a runtime-frame
  capability (`source.write` of decision 0033 stays one file, strict); merging,
  line or range patches, or a conflict resolver; a rename operation (a put of
  the new path with `absent` and a delete of the old path with its digest in one
  request is an atomic rename); conditions on a directory or on files no edit
  names; a way for the CLI to send a digest other than the base's; a dry run;
  a mark in the history that a revision came from an edit.
- The base revision is validated but fences nothing: an edit can apply on top of
  any number of later revisions, by design (decisions 3 to 5). A caller that
  needs "nothing changed at all" uses `save_revision`.
- The conflict list is bounded at 32 of at most 512 failures.
- The new package is derived inside the write transaction, so a large package
  (at most 25 MiB) holds the writer lock while it is read and checked, longer
  than a save, which builds its package before the lock.
- The read-back reads two listings and every put file's bytes: at most the size
  of the request again.
- The MCP and HTTP request limits are those of decision 0005; an edit costs one
  request.
- Single owner: the command is as strong as the other mutations, no more
  (decision 0003).

## Tests

Rust: `crates/artifree-providers/tests/conditional_edits.rs` (a stale disjoint
edit applies to the current files, an unlisted file is carried over; a
changed file conflicts with its current digest; one failure rejects every edit
and lists each failure in path order, writing no revision and no feed entry;
create expects `absent`; delete needs the digest; a rejected edit keeps its key
free; a retry replays after the head moved and other content or another key use
is refused; no-op against conflict; a package that cannot form; 40 failures list
32 and count 40; eight racing edits of one file have one winner whose bytes are
stored; eight racing edits of different files all apply; the trash),
`crates/artifree-ports` (expectations per file, the digest grammar),
`crates/artifree-core` (the read-back judgement, the shape checks, the
fingerprint), `crates/artifree-server/tests/file_edits.rs` (the strict save
still conflicts where the edit of the same base applies, a conflict names the
file, a store that misreports bytes, a digest or a file makes a 500 though the
revision exists, a replay is not read back, a key is shared with saves),
`authorization.rs` (another actor, an empty request, a repeating request),
`http_api.rs` (the contrast over HTTP, the conflict body, replay after the head
moved and in another order, twelve refused requests that write nothing and
reserve no key, the trash and the purge answer as for an unknown artifact),
`mcp_adapter.rs` (the same over MCP, the tool's hints, another owner), and
`crates/artifree-contracts/tests/wire.rs` (every edit states its condition, the
bounds and patterns).

TypeScript (`npm run test:tooling`): `scripts/tests/file-edits.test.mjs` (the
request, six answers that do not match, base64, a conflict, lists whose
numbers do not fit, the client's bound of 32 listed files held to the schema's
`maxItems`, other failures; the CLI against a real server process: an
edit past a moved head, a refused edit with its stderr lines, nothing written,
replay under the same key, a create and a delete, usage errors, the bodies
against the API schema).

## Alternatives

- **A mode flag on `save_revision`.** It would change what `base_revision`
  means for the same name and the same tool. Rejected: the strict command's
  meaning must not depend on a flag an agent might set by habit.
- **A retry loop over `commit_revision`** (read the head, check, commit
  against it, repeat on a conflict). It needs no store method but cannot
  decide a replay before the expectations: after the head moved, the retry of
  a committed edit would find its own change in the way. Deciding the key,
  the expectations and the commit in one transaction is exact.
- **`revision_conflict` with an extra list.** It would blur the strict command's
  code, for clients that treat it as "reapply everything".
- **An expectation that defaults to the base's digest.** A server that reads
  the base's file to fill it in would hide the caller's facts; an HTTP or MCP
  caller states them. The CLI does the filling, visibly.
- **Per-file conflicts as warnings with a partial commit.** The roadmap
  requires that one failed condition reject all.

## Costs

One more store method and one more error code and tool for every client to
know; a read-back after every new edit; a client function and a public client
`json` to keep the main bundle where it was (499.94 kB, under the 500 kB
warning, which was not raised); a promise that the strict commands stay strict.
