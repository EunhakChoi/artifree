# 0029: Retention and cleanup

Date: 2026-11-02. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) (request keys),
[0006](0006-change-feed.md) (decision 11, retention),
[0012](0012-duplicate-artifacts.md), [0013](0013-deleting-artifacts-and-the-trash.md),
[0015](0015-source-exports.md), and
[0027](0027-backup-restore-and-schema-upgrade.md). Roadmap row: O05.
Independent Artifree design: the private evidence index lists no
original-product constraint on retention, purging, or storage reclamation for
this row. Nothing below is an observation of the original product.

## Context

Until now nothing was ever removed. A deleted artifact stays in the trash with
all its revisions and bytes (0013), request keys and change rows are kept for
the life of the database (0003, 0006), and file content is shared by all
revisions and copies and was never deleted (0012). Roadmap O05 asks for
cleanup of the trash, of content nothing refers to, and of expired preview and
job output, while keeping what history, publications, running jobs, and copies
refer to, and without breaking the indefinite replay of request keys.

What exists today decides the scope:

- **Previews and runtime sessions** are in memory (tickets, grants, sessions,
  handoffs). A restart drops them; nothing persistent is left to clean.
- **Jobs** are not persisted: R01 is not done, and there are no job tables.
- **Publications** do not exist yet (S03).
- **Request keys** are persisted, reference revisions, and replay indefinitely.
- **Blobs** are content-addressed and shared by revisions and by copies.

## Decisions

1. **Purge is a conditional, retryable mutation.** `POST
/v1/artifacts/{id}/purge` with `{"base_metadata_revision": n}` and an
   `Idempotency-Key` removes an artifact **that is in the trash**, through
   `ArtifactService::purge` and the store, like every other write. It answers
   `200` with `artifact_id`, the `metadata_revision` the purge recorded,
   `purged_at_ms`, and `replayed`. An artifact outside the trash is `409
not_in_trash` (new error code); a stale base is `409 metadata_conflict`; a
   missing, purged, or other owner's artifact is `404 not_found`; the checks
   are made in that order after the key (as 0013). The base is the trash
   state the caller saw: an artifact recovered and deleted again has a newer
   metadata revision, so an old purge does not remove it. The owner check is
   the one of delete and recover. **The route needs the administrative
   credential** (0023: the bootstrap owner token, `RequireAdmin`): an issued
   agent token authenticates as the same owner, so the owner check alone would
   not stop an agent. An agent token gets `403 forbidden`, before the key, the
   body, or the artifact is looked at.
2. **A purge leaves a tombstone.** In one write transaction it deletes the
   artifact's `revision_files` rows (the listing of every revision's files),
   the blobs that no other revision file refers to afterwards, and the title
   of the artifact and of each of its metadata revisions (set to the empty
   string). It records one new metadata revision (empty title, still in the
   trash), sets `artifacts.purged_at`, records the request key, and appends a
   `purged` row to the change feed. It keeps the `artifacts` row (id, owner,
   kind, times, `deleted_at`), the `revisions` and `metadata_revisions`
   headers (numbers, actors, times), every `request_keys` row, and every
   `changes` row. Why a tombstone and not dropped foreign keys: the rows kept
   hold no content, title, or file name in the clear (see below for what a
   request key row still allows); every foreign key of the schema still
   holds, so `foreign_key_check` and the structural schema comparison (0027)
   keep their meaning for live artifacts too; the identifier stays in a
   primary key, so it can never be issued again even if the random generator
   repeated; and the change feed's join to the artifact still tells whose
   change each row is. Rebuilding `request_keys` and `changes` without their
   foreign keys would have weakened all artifacts to make room for the rare
   purged one. **What the kept rows still allow:** a request key row keeps the
   fingerprint of the request that created or changed the artifact, an
   unsalted SHA-256 over the request's kind, title, file paths and content (or
   digests of it). Someone with the database or an old backup who guesses a
   purged artifact's title or content can hash the guess the same way and
   confirm it; they cannot read it from the row. The fingerprint scheme is
   unchanged, because a retry is recognized by comparing it. The headers also
   keep who made each revision and when.
3. **A purged artifact is missing.** The store's reads (`artifact`, lists,
   revisions) treat `purged_at` as absence; a revision's header and files, and
   a revision listing, are read inside one snapshot, so a purge that commits
   meanwhile is seen whole or not at all (never a header with no files). So
   every route, preview ticket, export, runtime grant, and handoff that checks
   the artifact before using it answers as for an unknown identifier (`404 not_found`). `recover` of a
   purged artifact is `404`.
4. **Key replay after a purge.** No key is removed or expired. For a key whose
   request named a purged artifact:

   | Request retried                                    | Answer after the purge                                                                                             |
   | -------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
   | the purge itself (same key and body)               | `200`, the original `metadata_revision` and `purged_at_ms`, `replayed: true`                                       |
   | the same key with another body                     | `422 request_key_reused` for a create; `404 not_found` for a request naming the artifact                           |
   | a create, save, restore, or update of the artifact | `404 not_found`, the answer for an unknown artifact (the recorded outcome describes content that no longer exists) |
   | a duplicate whose **copy** still exists            | the copy's original receipt, `replayed: true`, also when the **source** was purged                                 |
   | a duplicate whose copy was purged                  | `404 not_found`                                                                                                    |

   The key stays reserved, so a retry can never run the request a second time
   or create a new artifact. The cases follow one rule: a retry replays the
   recorded outcome unless the artifact that outcome describes was purged,
   and then it is not found, as in the trash (0013, decision 2). A source
   that is gone does not change what a copy request created, so the store can
   answer `replay_create` without the source.

5. **The change feed.** A purge appends one row of kind `purged` carrying the
   new metadata revision. Sequences are never reused, the head never moves
   back, and existing rows stay, so a client that is behind receives the
   artifact's earlier changes (reading it now is `404`) and then `purged`;
   the wire kind `purged` is new, and a client built before it reads
   it as `unknown` and reads the artifact again (0006, decision 9), which is
   also `404`. Change rows are still never pruned; 0006 decision 11 keeps its
   rule that pruning must answer older cursors with `reset`, and this record
   does not prune.
6. **Unreferenced content.** Content is stored once per digest in `blobs` and
   referenced by `revision_files`. `GET /v1/content/unreferenced` reports how
   many blobs (and bytes) no `revision_files` row refers to, read in one
   snapshot, and removes nothing (the dry run). It also returns `digest`, the
   identity of that set: SHA-256 over a fixed prefix and the sorted digests of
   its members, written as 64 lowercase hexadecimal digits. `DELETE
/v1/content/unreferenced` is a mutation like the others: it needs an
   `Idempotency-Key` and a body `{"base_digest": <digest>}`, the set the caller
   reviewed. In one write transaction it (1) looks the key up, (2) counts the
   unreferenced content again, (3) refuses with `409 content_conflict`, deleting
   nothing, if the set is not the one named, (4) deletes with the predicate
   `NOT EXISTS` a `revision_files` row with the digest, so it cannot remove
   content a revision file refers to (a commit that stored the file is wholly
   before it, which then changes the set, or wholly after, which stores it
   again; a commit is an immediate write transaction, so SQLite serializes
   them), and (5) records the key with the report in the same transaction. So
   the caller deletes only what it saw. The dry run is not a plan the apply
   trusts: the apply decides again, and the base is what makes the two agree.
   - **Why not "naturally idempotent".** A second run finds nothing, so a retry
     after a lost response would report `0` and the caller could not tell a
     successful deletion from a cleanup that found nothing. The report of the
     first run must be recoverable, and a deletion of data the caller never
     reviewed is not acceptable either.
   - **Replay.** The first run records `(actor, key, fingerprint, blobs, bytes,
time)` in the table `cleanup_runs`; the fingerprint is over the operation
     and the base. A retry with the same key and base answers `200` with the
     first run's `blobs` and `bytes` and `replayed: true`, whatever the set is
     by then, and never runs again. The same key with another base is `422
request_key_reused`. A new key with a stale base is `content_conflict`
     and records nothing, so the same key works after a new dry run.
   - **One key space.** Keys are per actor. A key an artifact request used
     cannot start a cleanup, and a key a cleanup used cannot create or change
     an artifact (`request_key_reused`); the check is in the store, in the same
     transaction. `cleanup_runs` is a table of its own because a `request_keys`
     row names an artifact and a revision, which a cleanup has none of;
     widening that table would have weakened its foreign keys for every
     artifact. It holds counts, the key, and a request fingerprint (a SHA-256
     over the operation and the base set digest), never a list of blob
     digests, a name, a title, or content, so the claim that kept rows hold
     no content stays true.
   - **Key retention.** These keys are kept for the life of the database like
     all request keys (decision 11): no TTL, and a restore brings them along
     (tested). So a retry is answered with the original report however late it
     comes.
   - **A new error code.** `content_conflict` (`409`) is new: the existing
     conflict codes, `revision_conflict` and `metadata_conflict`, each carry a
     revision number a client acts on, which this conflict does not have.
     Clients that predate it read an unknown code as the generic error of its
     status. It is a response of the HTTP API only; MCP has no cleanup tool.
   - **Roles.** Both calls need the administrative credential (decision 1).
     The dry run too, because even its report describes content shared by every
     owner and agent, and a client that may not delete has no use for it.
     Issued agent tokens exist today (0023), so the restriction is not left to
     S01; S01 still decides which of several owners may run a cleanup, which
     covers all owners' content.

   Referenced content of artifacts in the trash is referenced content and is
   kept; so is content of copies, of exports (which read through
   `revision_files`), and of every older revision. A purge deletes the blobs it
   makes unreferenced itself, in its own transaction, so on a healthy database
   the cleanup finds nothing; it exists for content left by other means (an
   interrupted external operation, future object storage, a manual edit) and as
   a check. Schema version 7 adds `revision_files_by_digest`, because both the
   check and the foreign key check of a blob delete otherwise scan every file
   row. The database file does not shrink: freed pages are reused. No `VACUUM`
   is run.

7. **Schema version 7.** Adds `artifacts.purged_at` with `CHECK (purged_at IS
NULL OR deleted_at IS NOT NULL)`, the index above, the table `cleanup_runs`
   (decision 6), and rebuilds `changes`
   with `'purged'` in its checked kinds, keeping every sequence and the feed's
   head (`keeping_feed_head`). It is the next step of the ladder in
   `schema.rs`; `check_current` compares a database with the schema this build
   creates, so fresh and upgraded databases are held to the same shape. Frozen
   fixtures of versions 1 to 6 upgrade (0027). A backup is a copy of the
   database, so tombstones, kept keys, and rows come along, and a restore
   serves the same answers (decision 4) with a new feed identity.
8. **Surface.** Purge and the two cleanup calls are HTTP routes with
   contracts (`PurgeArtifactRequest`, `PurgeArtifactResponse`,
   `UnreferencedContent`, `RemoveUnreferencedContentRequest`,
   `RemoveUnreferencedContentResponse`, the `purged` change kind,
   `not_in_trash`, `content_conflict`), client methods (`purgeArtifact`,
   `getUnreferencedContent`, `removeUnreferencedContent`), and CLI commands:
   `artifree artifact purge <id> --base-metadata <n> --yes` and `artifree
content cleanup [--apply] [--base-digest <hex>] [--idempotency-key <key>]`.
   **Purge requires `--yes`**; without it the CLI refuses before any request.
   `content cleanup` without `--apply` is the dry run and issues the read-only
   request. `--apply` is the only way to delete: it runs a dry run, prints the
   reviewed set on stderr, and deletes that set under a generated key (or the
   `--base-digest` of an earlier dry run and the `--idempotency-key` given, to
   retry after a lost response); stdout is the JSON report with `applied` and
   `replayed`. **No MCP tool, and no agent token over HTTP.** An agent can
   delete to the trash and recover (0013) because that can be undone; a purge
   cannot, so an agent that went wrong or was misled would lose data for
   good. The MCP catalog has no such tool (a test lists it and calls the
   obvious names), and the HTTP routes refuse agent tokens (`403`, tested for
   all three routes against an issued token, with the administrative
   credential succeeding). The CLI sends whichever token it was given, so a
   profile that holds an agent token gets that `403`. Agent access to purge, if
   ever wanted, needs a separate decision about confirmation by a person. The web workspace is unchanged: it shows
   trash entries from the list, which does not include purged artifacts, and
   reads the `purged` change as it reads any newer metadata revision. A web
   control to purge is a follow-up (O05b in the roadmap).
9. **Automatic purge of old trash is not implemented.** An unattended,
   irreversible deletion needs a policy that is not decided here: which actor
   and request key it acts under (a mutation needs both), what a missed base
   means, how an operator sees and stops it, and its scheduling lifetime in the
   server. The explicit purge and cleanup give an operator the capability,
   and `list --deleted` shows `deleted_at_ms` for choosing; a timer can come
   after the web UI and a policy, and then needs an injected clock. It is the
   roadmap row O05b. The default today is: nothing is removed unless asked.
10. **Expired previews and jobs.** Nothing persistent exists (see Context), so
    there is nothing to clean and nothing is implemented for them. O06 (a
    temporary data directory and session TTL) and R01 (persisted jobs) must
    define, with their first persisted state, what expires, what a purge or
    cleanup must preserve (a running job's input revisions, a publication's
    revision), and what the cleanup's reference rule then includes: `revision_files`
    is the only reference today, and each later table that holds a digest
    must be added to it in the same change, with a test that fails if it is
    not.
11. **No expiry of request keys.** Request keys are retained for the lifetime
    of the database, as before, and this record introduces no TTL. A key
    retention change needs, first and in its own record: a versioned contract
    (for example a documented minimum replay window, announced to clients), the
    meaning of a retry after expiry (it would run again as a new request, so
    the window must be longer than any client retry, and the response to an
    expired key must be defined), how an expired key and a still-present
    artifact interact, and a migration that does not make an old backup's keys
    unusable. A purge keeps keys because replaying them is the point (decision
    4).

## Alternatives

- **Drop the foreign keys and delete every row of the artifact.** Rejected
  (decision 2): all artifacts lose referential integrity, the schema
  comparison weakens, and the feed's owner join and identifier reservation
  would need a second table.
- **Keep the full recorded outcome for replay (file listings, titles).** It
  would keep the names and digests of content a user asked to remove for good,
  and the digests would pin the blobs.
- **Replay a purged artifact's create or save as `410 Gone`.** It would name
  the purged identifier to a caller; 0013 chose one answer for what is not
  there.
- **A keyless, unconditional `DELETE` of unreferenced content, "naturally
  idempotent".** Rejected (decision 6): a retry after a lost response reports
  `0` and loses the original report, and the caller deletes what it never
  reviewed.
- **Cleanup keys in `request_keys`.** Rejected (decision 6): a row there names
  an artifact and revisions.
- **Purge leaves the bytes to the cleanup.** A purge would not be permanent
  until a second command ran.
- **`DELETE /v1/artifacts/{id}`.** Rejected in 0013 for the same reason: no
  base and no key.
- **Purge through the metadata update (`purged: true`).** Rejected: an
  irreversible effect in a request that also renames, which an agent may call
  through MCP.

## Erasure and what it does not reach

The purge and the cleanup run with SQLite's `secure_delete` on, so the pages
they free are overwritten in the database file. Two tests read the raw file
after a purge and after a cleanup and look for the removed content; they fail
when `secure_delete` is off. This is **not** a guarantee that no copy exists:

- A copy can stay in the write-ahead log until it is checkpointed and
  truncated (stopping the server does that). It was not tested whether the
  server's checkpoints reach it before then.
- **Backups made before the purge keep the artifact**, and a restore of one
  brings it back in the state it had when the backup was taken: live, if the
  backup was made before it was deleted, or in the trash, if after. A backup is
  a copy; this record does not delete copies.
- The file system, snapshots, and the operating system may keep older blocks.
- The database file does not shrink (decision 6).
- **A fingerprint of the purged request remains.** The request key rows are
  kept (decision 2), and each holds an unsalted SHA-256 of the request that
  created or changed the artifact (its kind, title, file paths and content).
  The database or an old backup lets someone confirm a guess about a purged
  artifact's title or content by hashing it the same way; it does not let them
  read it. The scheme is not changed here.

`docs/backup-restore.md` says the same to operators.

## Dependency note

`artifree-server` gains `rusqlite` as a **dev**-dependency only, to plant a
stray blob in the HTTP test of the cleanup (the store offers no way to create
one, by design). The boundary checker's dev-edge table lists it with that
reason. It adds no dependency to the shipped binary.

## Tests

Behavior is tested against the real SQLite store and the real HTTP router; the
TypeScript tests run the client and CLI against stub servers and, in
`server.test.mjs`, against the built server.

- `crates/artifree-providers/tests/purge.rs`: refusal outside the trash; a
  stale base; what remains after a purge (headers only, no listing, blob, or
  title); replay and reused keys; earlier keys answer `NotFound` and stay
  reserved; feed sequences and the `purged` kind; a copy keeps its history and
  bytes; `replay_create`; the dry run removes nothing and the apply removes what
  nothing refers to; a retry of a cleanup replays the first run's non-zero
  report; the same key with another base is a reused key and deletes nothing; a
  set that is not the reviewed one is refused, deletes nothing, and keeps the
  key; cleanup keys and artifact keys share one space, both ways; a cleanup
  report survives backup and restore; content a commit refers to after the dry
  run is kept and the cleanup that reviewed it is refused; a cleanup beside an
  in-flight commit is refused and keeps the commit's content (a channel orders
  the threads; the interleaving is **not** forced by timing, so both orders
  must give a refusal or an empty cleanup, never a deletion); backup and restore of a purged artifact; the `purged_at`
  check; two `secure_delete` erasure tests on the raw file.
- `schema_upgrade.rs` and `backup_restore.rs`: fixtures of versions 1 to 6
  upgrade to 7, a failed v6 to v7 step leaves the file unchanged, a trashed
  fixture artifact that shares content with another is purged without removing
  the shared bytes.
- Roles: `token_management.rs` issues an agent token and expects `403
forbidden` on 12 requests: the purge and the apply when valid, without a key,
  with a body that is not JSON, with no body, with JSON of the wrong shape, with
  an identifier that is malformed or unknown, and the dry run; nothing is
  removed and no refused key is consumed, then the same calls succeed with the
  owner's token; `mcp_adapter.rs` lists the tool
  catalog and calls the obvious names to show that no tool purges or cleans up.
- Reads: unit tests in `storage.rs` commit a purge between a revision's header
  and its files, and between the listing of revision numbers and their reads
  (a seam that only the tests use); a control without a snapshot shows the
  torn read the snapshot prevents. The shared-content tests also check, from
  outside the store, that every file still has its content and the foreign key
  check is clean, so they fail even if SQLite did not refuse the delete.
- Server tests: `http_api.rs` (preconditions and error order, purged answers as
  unknown on every route, a surviving copy's retry replays, dry run against
  apply with a planted stray blob), `change_feed.rs`, `authorization.rs`
  (another actor cannot purge or learn of one).
- TypeScript: client contract tests (a purge response is checked against the
  contract; `not_in_trash`; dry run is `GET`, apply is `DELETE` with the key and
  the base in the body, with a checked digest and report; `content_conflict`;
  the `purged` kind), CLI tests (no request without `--yes`, no delete without
  `--apply`, the apply sends the dry run's digest under a key, a given digest
  and key are sent as given), and server end-to-end tests of both commands,
  including a restart that replays the purge and a retried cleanup that
  replays.

Each guard was removed in turn and the tests failed: the trash check, the base
check, the blob deletion, the shared-blob rule, the `Gone` replay, the purged
filter in reads, the owner check, `replay_create` in duplicate, the dry run
that deletes, an apply that does not delete, the `purged` mapping in the feed,
the `purged` kind in the table check, the v7 step, `secure_delete`, and the CLI
`--yes` check. The shared-blob mutations (deleting a blob another revision file
refers to, or all blobs) were caught by SQLite's foreign key, not by an
assertion of ours: that is defense in depth, and the dedicated tests also pin
the behavior.

The cleanup's guards were removed in turn as well, and a named test failed each
time: the digest comparison (a changed set deleted anyway), the replay lookup
(a retry re-ran and reported nothing), the fingerprint comparison (another base
under a used key replayed instead of conflicting), the report written under
another key, either side of the shared key space, a digest that ignores the
members, a delete that is not limited to the reviewed set, a fixed key in place
of the header, a lenient (uppercase) digest parse, the administrative check on
the dry run and on the apply, and, for the purge and the apply, the same check
moved after the key, after the identifier, or after the body (the 403 tests
send a garbage body, a malformed identifier, and a missing key, so a check
that runs after those extractors answers 400, 422, or 404 instead). In core,
a read that a purge overtakes answering an empty list or "unavailable" instead
of not found fails a deterministic test with a store that purges between the
check and the read.

## Costs and limits

- A purge cannot be undone and has no confirmation in the API; the CLI's
  `--yes` and the trash step are the safeguards.
- A purged artifact still occupies a row, its headers, its keys, and its
  change rows; only content, listings, and titles are freed. The database file
  does not shrink.
- The retry of a create, save, restore, or update whose artifact was purged is
  `404`; the client learns the artifact is gone, not what the request did.
- The web workspace has no code for `purged`: the feed reducer counts it as a
  newer metadata revision, so lists refresh, but its record of artifacts seen in
  the trash is not updated. A trashed artifact open in a page when a purge
  arrives is not marked gone until its next read answers `404`. This was
  reasoned from the reducer, not run in a browser; O05b owns the web behavior.
- The cleanup is global, not per owner, and administrative; its keys are kept
  with the database like all keys. It is not limited
  to one run at a time: two runs serialize on the write lock.
- Not implemented: automatic purge, a web control, an MCP tool, pruning of
  change rows, vacuum, cleanup of previews and jobs (nothing persists).
