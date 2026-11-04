# 0040: Data change notifications and member listing (specification)

Date: 2026-11-04. Status: **proposed**. Roadmap row: R07 (split into R07a to
R07e). Builds on [0006](0006-change-feed.md) (the change feed),
[0031](0031-app-data.md) (application data), and
[0036](0036-multi-user-identity.md) (per-actor feeds). Amends decision 14 of
0031 and decisions 9 and 10 of 0006 (rules 17 to 19 below).

**Nothing in this record is implemented.** It is a specification written before
any code, so that the code pull requests can be reviewed against numbered rules
instead of being redesigned in review. Every sentence below that says what a
component "does" describes the intended behavior after the slices of rules 46 to 50;
until a slice is merged, the server, the contract, the client, and the web
workspace behave as 0006 and 0031 describe. The only things that exist today
are the facts quoted from those records and from the code of `main`, and they
are marked "today". Independent Artifree design; no observation of another
product is assumed.

## Context

R06 (0031) stores one JSON object per actor and artifact with its own
`data_version`. Today a program that wants to know that the data changed polls
`GET /v1/artifacts/{id}/data` and compares `data_version` (0031 decision 14),
and it cannot list the members of an object without reading the whole value
(up to 1 MiB). Roadmap R07 asks for bounded results and for change
subscription that survives reconnects without mixing source revisions and data
versions.

Two things are in scope and the rest is not.

- In scope: (a) change notifications for application data; (b) a paged listing
  of the member names of one object in a data space.
- Not in R07: filter, sort, or aggregate queries (T11); a pending or
  acknowledged state and any client queue (R08); the frame and preview data
  bridge (no capability exists, 0020 and 0031); MCP tools for the listing (they
  may follow in a later record); a new streaming endpoint, timer, or limit.

## Decisions (proposed, not implemented)

### Scope and transport

1. **One feed, one new kind.** Data changes travel in the existing change feed
   `GET /v1/changes` of 0006, per actor (0036 item 8). The change kind
   `data_changed` is added. No endpoint, timer, stream slot, or limit is added:
   stream slots (`--change-streams`, default 16), the 15 second wake-up, the
   15 second keepalive, and the 100 changes per read of 0006 decisions 4, 5, and
   7 apply unchanged.
2. **Events carry no data values.** A `data_changed` change contains no value,
   no path, no member name, and no size. It says only which artifact's data
   changed and the new data version. A client reads `GET data` for content.
3. **Shape.** `ArtifactChange` gains one optional field, `data_version` (an
   integer, at least 1). It is present exactly when `change` is
   `data_changed` and absent for every other kind. The existing `revision` and
   `metadata_revision` fields of a `data_changed` change carry the artifact's
   current source revision and metadata revision at the time of the write, as
   every other change does. The data version is never placed in either of those
   fields, so a source revision and a data version are never mixed (R07 row).
4. **When a row is recorded.** The store appends a `data_changed` row in the
   same write transaction as a data set, update, or delete, only when that
   write changed the space (`changed = true` in `DataWriteResponse`). A replay
   (0031 decision 6), a no-op (`changed = false`), a refused request, and a
   rolled-back transaction record nothing (0006 requirement C2). The row is
   written after the `app_data` row and the request key, in the same
   transaction, so a reader of the feed never sees the row before the data
   version it names is readable.
5. **Whose feed.** The row belongs to the feed of the **actor whose data space
   changed**. A data space is per artifact and actor (0031 decision 1), and the
   write's `ActorId` is the space's actor. Today `record_change`
   (`crates/artifree-providers/src/storage.rs`) looks up `artifacts.owner` and
   writes to that actor's feed, and `actor_feeds.head` is taken in the same
   statement. Because data access today requires ownership (0031 decision 8; a
   non-owner gets `404`), the two actors are always the same, so reuse would
   work by coincidence. The rule is nonetheless stated as "the actor of the
   data write", and the implementation must pass that actor to the row
   explicitly (a new `record_change` parameter or sibling function) so that
   sharing (S02), which would let two actors have spaces for one artifact,
   cannot send A's data change to the owner's feed. Acceptance (rule 46): a
   lower-level store test that calls the row-recording function with a writer
   actor different from the artifact's owner and asserts that the row, its
   `actor_seq`, and `actor_feeds.head` land in the **writer's** feed and the
   owner's feed is unchanged; it must fail if the explicit actor is removed
   (owner lookup). An HTTP-level two-user test is an additional check, not a
   substitute: today a write to another actor's artifact is refused, so that
   test alone cannot tell the two routings apart.
6. **Schema.** A schema step (the next version after the current 10) rebuilds
   `changes` as earlier versions did: the `kind` CHECK gains `'data_changed'`;
   a nullable `data_version INTEGER` column is added with
   `CHECK ((kind = 'data_changed') = (data_version IS NOT NULL))` and
   `CHECK (data_version IS NULL OR data_version >= 1)`; every existing row, its
   `sequence`, `actor`, and `actor_seq` are kept by value, and the upgrade fails
   and changes nothing if the rebuilt table would hold fewer rows (the rule of
   0036 item 8). The head in `actor_feeds` is not touched. A failed step rolls
   back. An old-database upgrade fixture (schema 10 with rows of every existing
   kind) is required, and a restore of an older backup (0027) must upgrade
   through the step.
7. **Contract.** The contract version gets a minor bump (the next minor after
   the value at implementation time; 0.14.0 today). `contracts/generated` and
   `packages/contracts/src/generated` are regenerated with
   `npm run generate:contracts`, never edited by hand. `ChangeKind` gains
   `data_changed`; `ArtifactChange.data_version` is optional.
8. **Older clients.** A client built before the kind existed sees the change
   as `unknown` (0006 decision 9) and reads the artifact again; it does not fail.
   An older client still gets the correct, if wasteful, behavior.
9. **Web reducer.** The web workspace reducer that follows the feed (A02, 0007) ignores `data_changed`: it concerns data, not the source, and must not
   refresh the file list, the revision, or the metadata. It does not treat the
   kind as `unknown` (which today triggers a reload). A test sends a
   `data_changed` change and shows that no read happens. This is part of slice
   R07a (rule 46), so the workspace never sees the kind as `unknown` once the
   server can emit it.
10. **Duplicate.** Duplicating with `copy_data` (0031 decision 11) records **no**
    `data_changed` row for the copy. The copy's `created` change already
    announces the new artifact; nobody can follow the copy's data before its
    identifier exists, and every follower starts with a read (rule 21), which
    sees the copied space at version 1. A second row would add a feed entry
    that carries no information the first does not. The source's space is not
    changed by a copy, so it records nothing either.
11. **Purge, trash, restore of a backup.** A purge removes the data (0031
    decision 12) and the existing `purged` change covers it: no `data_changed`
    row. `deleted` (trash) hides the data and `recovered` brings it back with the
    same version (0031 decision 12); those changes are the signal (rule 25). A
    backup restore gives the database a new feed identity (0006 decision 11),
    so every cursor from before is answered with `reset`, and a follower reads
    again (rule 23). A source restore (A04) changes neither the data nor its
    version and records no `data_changed`.
12. **Cost, stated.** Each changed data write adds one row to `changes`, which
    has no retention until O05 (0006 decision 11). R07 does not coalesce rows.
    A busy app writes as many rows as it makes changed writes, bounded by the
    request limits of 0005. A reader of 100 changes per read may therefore see a
    run of `data_changed` rows ahead of a source change; the stream reads on
    without loss, and a follower that coalesces (rule 22) does not multiply
    reads. No limit on the row count per artifact is added.
13. **Authorization.** The stream delivers only the authenticated actor's rows
    (0006 decision 6, 0036 item 8), so a `data_changed` of another actor's
    space is never delivered or visible as a gap in numbering. A data change
    reveals that data changed and its new version number, to the actor who owns
    that space and nobody else.
14. **Order.** Sequence order is the commit order of the feed (0006 decision 1).
    For one artifact and actor the data versions in `data_changed` rows are
    strictly increasing in sequence order, because they are written in the
    transaction that increments the version. Consecutive rows of one artifact
    differ by exactly 1, but a follower must not rely on that, because a purge
    or a restore can interrupt the sequence.
15. **No new streaming endpoint.** `Last-Event-ID` behavior, `ready`, `reset`,
    and comments are exactly those of 0006 decision 4. The server's behavior
    for a stream does not depend on the new kind.
16. **Boundaries kept.** Frames and previews cannot read or write data and do
    not receive these events. Nothing here claims a runtime bridge, network
    isolation, or any capability (AGENTS.md).

### Amendments to earlier decisions

17. **0031 decision 14 is amended.** "A data change adds no change kind to
    `/v1/changes`, and the web reducer is unchanged" is replaced, once the
    slices of rules 46 to 50 are implemented, by rules 1 to 11. The sentence "A client
    polls `GET data`" stays true for a client that does not subscribe. The rest of
    decision 14 (data is not in a revision, export, package, or content-cleanup
    reference) is unchanged. The note at the top of 0031 says "Amended by 0040"
    and names decision 14 only.
18. **0006 decision 9 and 10 are amended for the new client work**, implemented
    in slice R07c and not before: (a) delays carry jitter as defined in rule 44 (the 30 s cap applies to the
    base before jitter), and a server `Retry-After` is a floor (the delay is the
    larger of it and the jittered delay), where 0006 decision 10 waits
    `Retry-After` exactly and adds no jitter; (b) the stall detection of rule 44; (c) a bounded SSE parser (rule 39).
    0006's other text stays. Until R07c, 0006 describes the client.
19. **Not amended.** 0006 decision 11 (retention), the stream limits, and the
    clean-end behavior (rule 45) are unchanged.

### Consumer model: snapshot and replay

20. **Reads stay `GET data`.** A follower is a client routine, not a server
    object. The server holds no per-follower state. `followData` (rule 29)
    implements the algorithm below; any client may implement it.
21. **Algorithm.** A follower of artifact `A`, path `P` (the empty path is the
    root) keeps `seen` (the last data version delivered in the current epoch,
    initially none) and `dirty` (the highest version named by an event not yet
    covered by a read, initially none).
    1. Open the feed and wait for `ready`. Events are buffered by the client
       from here on.
    2. Read `GET data` for `P` (the snapshot). Deliver it as a **snapshot**,
       start an epoch with `seen` = its `data_version`, and clear `dirty`.
       Events buffered meanwhile are then applied in order (step 3). A `404`
       on this read is handled by rule 27.
    3. For each `data_changed` for `A` with `data_version` greater than `seen`
       (and greater than `dirty`), set `dirty` to it. For any other change of
       `A` see rule 25. Changes of other artifacts and other kinds are ignored.
    4. When `dirty` is greater than `seen` and no read is in flight, read again.
       Only one read is in flight. When the read returns version `v`: if `v` is
       greater than `seen`, deliver an **update**, set `seen` to `v`, and clear
       `dirty` if `dirty <= v`; if `v` equals `seen`, deliver nothing; if `v` is
       less than `seen`, start a new epoch (rule 24). A `v` below `dirty` (below a
       version an event of this connection already named) cannot happen with one
       database and is `invalid_response` (trust rule 3), which also prevents a
       read loop. A `404` is handled by rule 27.
    5. On `reset`, on a reconnect (a new `ready`), or after a stream error that
       ended the connection, abandon any read in flight and return to step 2 (a
       new epoch, delivered as a snapshot, even if the version is the same).
22. **Guarantees.** Given an unbroken feed, (a) no lost update: every committed
    data change after the follower's subscription is either at or below the
    version of a later read or raises `dirty` and causes a read; (b) versions
    delivered to the application strictly increase within an epoch; (c) several
    events that arrive before a read completes cause one read (coalescing); (d)
    a read is never started for a version the follower already has. The
    follower does not guarantee that it delivers every intermediate version: it
    delivers the newest state it read, not each write. **An update is
    delivered whenever the space's data version increases, even when the value at
    `P` did not change**, because the version belongs to the whole space and not
    to a path; an application that cares compares values itself. It does not claim ordering
    against changes of other artifacts or kinds.
23. **Why subscribe first.** Step 1 precedes step 2, so a commit between the
    two is delivered as an event with a version greater than the snapshot, or
    is already inside the snapshot (its version is not greater than `seen`) and is
    ignored. After `reset` the feed's rows may have been lost, so the follower
    cannot know what it missed; a fresh snapshot is the only safe answer.
24. **A version that goes backwards.** A read that returns a version below
    `seen` can follow a restore of the whole database from a backup.
    The follower does not drop it: that read
    replaces the earlier state, starts a new epoch, and is delivered as a
    **snapshot**, never as an update. The application's rule is: a snapshot
    replaces everything; an update is newer than the last delivery of its
    epoch. Within an epoch no delivered version is lower than an earlier one.
25. **Other changes of the same artifact.** `deleted` (trash) makes the
    follower **unavailable**: it delivers one `unavailable` notice (reason
    `trashed`), discards the result of any read in flight, and reads no more
    (a read would be `404`, 0031 decision 8). While unavailable, events for the
    artifact other than `recovered` and `purged` are ignored. `recovered`
    triggers step 2. An observed `purged` ends the follower: it delivers an
    `unavailable` notice with reason `purged` (even if a `trashed` notice was
    delivered before, because the reason differs) and resolves quietly. While
    available, `created`, `revised`, `metadata_updated`, and `unknown` for the
    artifact trigger nothing, because source revisions and data versions are
    unrelated (0031 decision 1) and an `unknown` kind carries no `data_version`
    to compare.
26. **No `Last-Event-ID`-specific behavior.** The follower does not resume from
    a remembered cursor; it takes the head at each connect and a fresh
    snapshot (rule 21). The server's resume support is unchanged and unused
    by this routine.
27. **A read that fails, and the state machine.**
    - A `404` on the follower's **first snapshot read** (no snapshot has ever
      been delivered) rejects `followData` with the error: an unknown artifact
      is the caller's error. It is not retried.
    - A `404` on any read **after** a snapshot was delivered (in an earlier
      epoch, or an update read) is not terminal: the follower becomes
      unavailable (rule 25), delivers `unavailable` (reason `trashed`) once, not
      again while it stays unavailable, keeps the feed open, and waits. A `404`
      on a later step-2 read (after `recovered`, `reset`, or a reconnect) keeps
      waiting silently.
    - A `429`, a `5xx`, or a dropped connection on a read is retried by the
      rule of 0006 decision 10 as amended by rule 18 (the feed stays open; if
      the stream itself dropped, step 5 applies). Any other `4xx` rejects.
    - **Limitation, stated:** a purge that happens while the follower is
      disconnected looks like the trash state (`404`, no `purged` event
      observed). The follower waits until the application aborts it or the
      artifact returns. The application owns the follower's lifetime through
      its abort signal.

    The follower is a state machine. States: `connecting` (feed opening, until
    `ready`), `snapshotting` (step 2 in flight), `following` (idle), `reading`
    (an update read in flight), `unavailable`, `ended`. Abort in any state moves
    to `ended` quietly (rule 43). `drop` is a stream end or error.

    | State                 | Event                                       | Action; next state                                                                              |
    | --------------------- | ------------------------------------------- | ----------------------------------------------------------------------------------------------- |
    | connecting            | `ready`                                     | Start step 2; snapshotting                                                                      |
    | connecting            | drop, `429`, `5xx` on open                  | Back off (rule 44); connecting                                                                  |
    | connecting            | other `4xx` on open                         | Reject; ended                                                                                   |
    | snapshotting          | read `200`                                  | Deliver snapshot, apply buffered events; following (or reading, or unavailable by those events) |
    | snapshotting          | read `404`, no snapshot ever delivered      | Reject; ended                                                                                   |
    | snapshotting          | read `404`, a snapshot was delivered before | Deliver `unavailable` unless already unavailable; unavailable                                   |
    | snapshotting, reading | read `429`, `5xx`                           | Back off, retry the read; same state                                                            |
    | snapshotting, reading | read other `4xx`                            | Reject; ended                                                                                   |
    | snapshotting, reading | drop or `reset`                             | Abandon read; connecting (drop) or snapshotting (reset)                                         |
    | following             | `data_changed` with version above `seen`    | Set `dirty`; reading                                                                            |
    | following, reading    | `data_changed` not above `seen`             | Ignore                                                                                          |
    | reading               | `data_changed` above `dirty`                | Raise `dirty`; reading (coalesced)                                                              |
    | reading               | read `200`                                  | Deliver per step 4; following, or reading if `dirty` remains                                    |
    | reading               | read `404`                                  | Deliver `unavailable`; unavailable                                                              |
    | following, reading    | `deleted`                                   | Discard read in flight; deliver `unavailable`; unavailable                                      |
    | following, reading    | `purged`                                    | Deliver `unavailable` (purged); ended                                                           |
    | following             | `reset` or new `ready` after drop           | snapshotting                                                                                    |
    | unavailable           | `recovered`, `reset`, new `ready`           | snapshotting (a `404` there waits silently)                                                     |
    | unavailable           | `purged`                                    | Deliver `unavailable` (purged); ended                                                           |
    | unavailable           | drop                                        | Back off; connecting (stays unavailable on `ready` until the read)                              |
    | unavailable           | any other change                            | Ignore                                                                                          |
    | any                   | callback throws                             | Reject; ended (abort wins, rule 43)                                                             |

    The audit of rules 20 to 27 found three contradictions, all fixed here: the
    old rule 27 ended the follower on a `404` although rule 25 waits for
    `recovered`; a read returning a version below a named `dirty` could loop
    forever (now `invalid_response`); and a read in flight when `deleted`
    arrived had no defined fate (discarded).

### Member listing

28. **Endpoint.** `GET /v1/artifacts/{id}/data/members`, with the artifact
    identifier in the path as in 0031 decision 9. The query is strict (an
    unknown or repeated parameter is `400 invalid_request`) and has these
    parameters:
    - `path`: optional; the data path of 0031 decision 2, the empty string
      (or absence) meaning the root.
    - `limit`: optional; an integer from 1 to 100 inclusive, default 50. Any
      other value or spelling (empty, signed, fractional, or with a leading zero) is `400 invalid_request`.
    - `after`: optional; an opaque cursor issued by a previous response.
    - `at_data_version`: optional; an integer from 0. A precondition, not a
      base: see rule 31.

    The authentication, the order of checks (artifact identifier, then query
    shape, then path), the rate and concurrency limits, the `404` for an artifact
    outside access (trash, purge, another actor, unknown), and the use of an
    issued agent token follow the data routes of 0031 decision 8 and 9.

29. **Client entry points** (slice R07c; nothing exists today): `listDataMembers`
    in `ArtifreeClient`, and `followData` in the client package. Their abort and
    error behavior is the table of rule 43.
30. **Response.** `200 DataMembers`:
    - `artifact_id`: the artifact the page was read for.
    - `path`: the canonical path asked for.
    - `data_version`: the version of the space at which the page was read.
    - `exists`: `false` when nothing is at `path`, or `path` goes through a
      value that is not an object (the same meaning as `AppData.exists`, 0031
      decision 5); then `names` is empty and `next` is absent.
    - `names`: member names in ascending order of their **UTF-8 bytes**, the
      order of the canonical form (0031 decision 3), at most `limit` of them.
    - `next`: an opaque cursor, **present exactly when** (a server guarantee,
      tested in slice R07b; a client cannot observe it) at least one more
      member follows the last name returned, absent otherwise.

    A page is read from one snapshot of the store (one transaction, as 0006
    decision 1 reads a page and a head together), so `data_version` and `names`
    agree. The listing does not return values, sizes, or types; values come from
    `GET data` at `path/name`.

31. **Version precondition.** When `at_data_version` is given and differs from
    the space's current version (a stale one or a future one), the answer is
    `409 data_conflict` with `current_data_version`, exactly the error and body
    of 0031 decision 6. A version of 0 matches a space that was never written.
    With the precondition on every page, a client walking the pages sees one
    consistent set of members or learns that it must start over. **Without it,
    pages are not snapshot-consistent**: a member added or removed between two
    pages may be missed or not appear, and `data_version` may differ between
    pages. The cursor's position is a name, not a count, so a deleted `after`
    name is still a valid position.
32. **Cursor.** The cursor is issued by the server and the client never parses
    it, builds it, or stores it beyond the walk. It carries a digest of the
    artifact identifier and the canonical path and the last name returned,
    encoded in base64url, at most 256 bytes. A cursor that is malformed, longer
    than 256 bytes, or bound to another artifact or path is `400 invalid_request`
    (a foreign cursor is a request error, not a lookup of another's data; the
    cursor is not a secret and grants nothing, because the page is always read
    from the authenticated actor's own space). The cursor is not an
    authorization token, does not expire, and may be reused after a version
    change.
33. **A path that is not an object.** If a value exists at `path` and it is not
    an object (a number, string, boolean, `null`, or array), the answer is
    `409 data_path_conflict` (0031 decision 5 uses it for a path that cannot be
    entered) and not `exists: false`: there is a value, it has no members.
    (`exists: false` is for nothing there or an unenterable path.)
34. **Which names are listed.** The listing returns the names of the members of
    the object at `path`, as long as they can be addressed by the data path
    grammar (0031 decision 2): a name matching `[A-Za-z0-9_-]{1,64}`. A member
    whose name does not (non-ASCII, over 64 bytes, or with a character outside
    the set; 0031 decision 3 allows names up to 256 bytes) cannot be read by
    `GET data` and is skipped; it does not count toward `limit` and does not make
    `next` present. This is a consequence of 0031 decisions 2 and 3. The roadmap
    follow-up R06b extends the path grammar to UTF-8; when it lands, the listing
    follows the extended grammar and the page-size bound of rule 35 must be
    recomputed. The order for the listed names is the byte
    order of rule 30.
35. **Bounds.** With names limited to 64 ASCII bytes, a page of 100 names is at
    most 6,400 bytes of names, plus quotes and commas (at most 7,000 bytes),
    plus the cursor (at most 256 bytes) and the fixed fields: a response below
    8 KiB. The server reads the whole space row (at most 1 MiB, 0031 decision 4)
    to list, so a page costs the size of the space; this is the cost of 0031
    "a write costs the size of the space" on the read side. The existing request
    limits apply; no new limit is added.

### Client trust rule

The rules below apply to every response and stream the TypeScript client
handles in R07 and are copied into `docs/quality.md`.

36. **Rule 1: the server owns data-domain rules.** The client does not re-check
    whether something is storable: surrogates, duplicate names, node count,
    nesting depth, name length, canonical byte size, or number formatting. It
    sends what the caller gave (after the existing `plainData` check of 0031
    decision 15) and trusts the server's answer.
37. **Rule 2: shape, not meaning.** Every field the client reads has the type
    its TypeScript declaration claims; integers are safe integers; enumerations
    are closed, with an `unknown` fallback where the contract may grow (as
    `FeedChange` does for change kinds).
38. **Rule 3: what the state machine depends on.** The client checks what its
    own algorithm needs: the echoed artifact identifier equals the one asked for;
    data versions are consistent with their ordering rules (rules 21, 22, 24);
    event types are known; `next`, if present, is a non-empty string (the
    client does not parse it and applies no length policy beyond the response
    body cap), and paging continues if and only if `next` is present; the error code decides whether to retry or stop.
39. **Rule 4: memory and denial of service are transport limits.** One generic
    bound on the SSE line, event, and pending buffer size, and the existing
    response-body cap of the client. The client never computes canonical JSON
    size. Today the feed parser has no such bound (`change-feed.ts`); adding it
    is slice R07c, and until then that gap exists.
40. **Rule 5: invalid responses are decided once.** A violation of rule 2, 3,
    or 4 is `invalid_response`, decided once per message, and is never retried.
41. **Rule 6: contracts own the wire.** Wire shapes come from the generated
    contracts (`artifree-contracts`). A rule must not exist only in TypeScript.
42. **Rule 7: review scope.** A review finding that asks for a check outside
    rules 1 to 6 is a follow-up, not a blocker for the pull request.

Rules 36 to 42 are trust rules 1 to 7; cite them as "0040 rule 3x" or as
"trust rule N".

### Abort and error behavior of the client

43. **Table.** Both routines take an `AbortSignal`. "Quiet" means resolve with
    no error. Implemented in slice R07c; nothing here exists today.

    | Situation                         | `followData`                                                     | `listDataMembers`                              |
    | --------------------------------- | ---------------------------------------------------------------- | ---------------------------------------------- |
    | Abort before the request          | Resolves quietly; sends nothing                                  | Rejects with the abort error                   |
    | Abort while waiting for headers   | Resolves quietly; cancels the request                            | Rejects with the abort error                   |
    | Abort while reading an error body | Resolves quietly                                                 | Rejects with the abort error                   |
    | Abort during a stream             | Resolves quietly; cancels the body; delivers nothing after       | Not applicable (no stream)                     |
    | Abort while waiting to reconnect  | Resolves quietly at once, not after the delay                    | Not applicable                                 |
    | Abort inside a user callback      | Resolves quietly after the callback returns; no further delivery | Not applicable                                 |
    | Callback throws                   | Rejects with that error; never retried                           | Not applicable                                 |
    | Abort and callback error together | Abort wins: resolves quietly                                     | Not applicable                                 |
    | Error response body               | Read with a size cap, raced with the abort signal                | Same                                           |
    | Network failure, `429`, `5xx`     | Retried (rule 44)                                                | Rejects; the caller decides (a single request) |
    | Other `4xx`, non-stream success   | Rejects; never retried                                           | Rejects; never retried                         |

    The abort error is a `DOMException` named `AbortError` or an error whose
    `name` is `AbortError`, documented once in the client package. `followData`
    resolves quietly on abort, as `followChanges` does (0006 decision 10).

44. **Retry and stall.** For `followData`: 0006 decision 10 applies as
    amended by rule 18: reconnect after a stream ends, and after a failure only
    for a dropped connection, `429`, or `5xx`; the delay is jittered as follows.
    For attempt `n` (0-based) the base is `min(30 s, 0.5 s x 2^n)`; the delay is
    `base x (0.5 + 0.5 x u)`, where `u` is uniform in [0, 1) from an injectable
    random source, so the delay lies in [base/2, base). A server `Retry-After`
    (seconds or HTTP-date, as 0006 already parses it, itself capped at 1 hour as
    0006 clamps it) is a floor: the delay is `max(Retry-After, jittered delay)`; the sequence restarts after a stream is
    accepted. **Stall detection:** if no bytes arrive on a stream (comments
    count) for 45 seconds, the client ends the connection like a drop and
    reconnects by this rule. The server's 15 second keepalive (0006 decision 4)
    makes 45 seconds three missed keepalives.
45. **Unchanged and not decided.** A clean end of a stream carries no reason;
    the client always reconnects with backoff, and a revoked token appears as
    `401` on the next open (rejecting, 0006 decision 10). `retry:` and `id:` are
    not used (0006 decision 9). Cross-process revocation stays X04.

### Slices

46. **R07a: store, feed, contract.** Scope: rules 1 to 16 (schema step, the
    row in the write transaction, per-actor, contract bump and regeneration, and the
    web reducer ignoring the kind with its test of rule 9). Non-goals: listing,
    client, any other web change.
    Acceptance checks (each must fail when its guard is removed): a changed
    set, update, and delete each produce one row with the new `data_version` and
    the artifact's current `revision` and `metadata_revision`; a replay, a
    no-op, a conflict, and a refused write record nothing; a rolled-back
    transaction records nothing; the store test of rule 5 (writer actor differs
    from the owner; row, `actor_seq`, and head land in the writer's feed, the
    owner's feed is unchanged, and it fails without the explicit actor) and, as
    an additional check, a two-user HTTP test showing actor B's feed, head, and
    stream unchanged by A's data writes; a stream receives the row; an old
    cursor of a previous feed gets `reset`; duplicate with `copy_data` records no
    `data_changed`; purge records `purged` only; a schema 10 fixture upgrades with
    every row kept by value and a failed step rolls back; a backup older than
    the step restores and upgrades; the contract round-trips with
    `data_version` present only for the kind.
47. **R07b: member listing.** Scope: rules 28 to 35 on the server and in the
    contract. Non-goals: MCP tools, client, CLI, values in the listing.
    Acceptance: order by UTF-8 bytes (including names that differ only by case
    and by digits); default 50, bounds 1 and 100, 0 and 101 refused; the cursor
    resumes after a deleted name; a malformed, oversized, and foreign cursor are
    `400`; `next` is absent on the last page and present otherwise (a page of
    exactly `limit` names with none following has no `next`; this is a server
    guarantee tested here only); the stale and
    future precondition give `data_conflict` with `current_data_version`; a
    non-object is `data_path_conflict`; a missing path is `exists: false`; names
    outside the path grammar are skipped; strict query; an issued agent token
    works; another actor's artifact is the same `404` as an unknown one.
48. **R07c: TypeScript client.** Scope: `FeedChange` handles `data_changed`
    and `data_version`; `followData`; `listDataMembers`; the bounded SSE parser;
    stall detection; jitter and the `Retry-After` floor; the table of rule 43;
    the trust rules; the text in
    `docs/quality.md` stays in step. Non-goals: web panel, MCP, CLI. Acceptance:
    the algorithm of rule 21 against a scripted server (no lost update across
    a commit between subscribe and read; a version going backwards is delivered
    as a snapshot; coalescing; `reset` and reconnect re-read; `deleted` then
    `recovered`; and scripted-server cases for deleted, disconnect, recovered;
    deleted, disconnect, purge unobserved (keeps waiting, exactly one
    `unavailable`); purged observed (ends with reason `purged`); a first-read
    `404` rejects; `reset` while unavailable; every transition of the table of
    rule 27); every row of the table of rule 43; an event larger than the
    bound is `invalid_response`; a stall of 45 seconds reconnects (with a fake
    clock); with a random source returning 0 and a value just below 1 the delay is
    base/2 and just below base; a `Retry-After` larger than the jittered delay
    wins and a smaller one does not (fake clock throughout); an observed `next`
    that is an empty string or not a string is `invalid_response`, and paging
    continues if and only if `next` is present.
49. **R07d: user-facing documentation.** Only if a user document for application
    data exists when R07a to R07c are merged; otherwise this slice is dropped.
    Scope: that document and the roadmap status. No behavior.
50. **R07e: first consumer (owner-approved).** A read-only data panel in the web
    workspace of the open artifact, updated through `followData` without
    polling. It depends on R07a to R07c and comes after R07c. It does not give
    frames or previews access to data.
51. **Size.** Each code pull request targets under about 3,000 lines excluding
    generated files. The three review rounds of `.github/CONTRIBUTING.md` are
    counted on the code pull requests after this specification is merged, and a
    code pull request may not change a numbered rule of this record: a needed
    change is a separate amendment pull request.

## Not implemented

All of the above. Specifically, today: there is no `data_changed` kind, no
`data_version` field, no schema step, no member listing endpoint, no
`followData` or `listDataMembers`, no SSE size bound, no stall detection, no
jitter, no `Retry-After` floor, and no data panel. Also not part of R07:
filter, sort, aggregate (T11); pending and acknowledged states and a client
queue (R08); frame or preview access to data; MCP tools for the listing;
retention of change rows (O05); several server instances (X04).

## Alternatives

- **A separate data stream.** It would add a second endpoint, authentication
  path, slot limit, and parser for the same wake-up. The feed already has
  the cursor, `reset`, and limits.
- **Carry the new value or a patch in the event.** It would copy up to 1 MiB
  into the feed, bring the value rules into the feed, and need a retention
  answer for secrets; a read is one request and already authoritative.
- **Put the data version in `revision` or `metadata_revision`.** It would
  mix three unrelated counters in one field, which the R07 row forbids.
- **Resume followers from `Last-Event-ID`.** The feed has no retention
  guarantee (0006 decision 11) and a gap or `reset` forces a snapshot anyway;
  the head plus a snapshot is the only path both cases share.
- **Offset pagination.** A page after a delete would skip or repeat a name;
  a name-based cursor does not.
- **Record a row for the copy of a duplicate.** See rule 10.
- **Skip the client checks of rules 2 and 3.** Then a server bug becomes silent
  state corruption in an application; checking shape and the state machine's own
  inputs is cheap.
- **Check storability in the client.** The server must check anyway, two
  copies of a rule diverge, and 0031 decision 3 shows how subtle the rule is.

## Costs

A schema step and contract bump, a new column in `changes`, more feed rows
until retention, a cursor format the server must keep stable, a read of the
whole space per listing page. The
benefit is one subscription mechanism with one set of limits and a client
rule set that reviewers can cite.
