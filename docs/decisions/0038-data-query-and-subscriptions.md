# 0038: Application data queries and subscriptions

Date: 2026-11-04. Status: accepted. Builds on
[0006](0006-change-feed.md) (server-sent events, resumption),
[0031](0031-app-data.md) (the data space and its version),
[0034](0034-room-and-presence.md) (a bounded window with an explicit
`missed`), and [0036](0036-multi-user-identity.md) (per-user isolation and
revocation). Roadmap row: R07. Independent Artifree design: the private
evidence index gives this row only the idea of a validated, bounded query and
of a subscription that can resume after a reconnect; the query language, the
event vocabulary, the window, and every limit below are Artifree's own
decisions. Nothing below is an observation of the original product.

## Context

Decision 0031 stores one JSON object per actor and artifact with a data
version of its own and says that a client polls `GET data` to notice a change.
An app that shows a list of records must read a collection page by page
without pulling the whole space, and an app that stays open must learn about a
write without polling. This record adds both, on the server, the HTTP API, and
the TypeScript client.

**Still no page can use them.** A frame has no data capability (0020, 0031):
this is HTTP and the client library with an owner or issued token. The web
workspace does not use them.

## Decisions

### Query

1. **One query form: list the members of an object.** `GET
/v1/artifacts/{id}/data/members` lists the members of the object at a path
   (a collection) in the caller's own space. `get` of one path is the existing
   `GET data`. Parameters (all optional; an unknown or repeated parameter is
   `400`, a value that breaks a rule is `422 invalid_request`):
   - `path`: the collection, by the grammar of 0031; the root by default.
   - `prefix`: only members whose name starts with it; 1 to 64 characters of
     `[A-Za-z0-9_-]`.
   - `select`: `names` (the default) or `values`.
   - `limit`: 1 to 100 members, 50 by default.
   - `after`: a member name, 1 to 256 bytes; members strictly after it are
     listed. It is not limited to the path grammar: member names of a value are
     any strings, and a name the grammar refuses can still be a cursor.
2. **Order and cursor.** Members are ordered by the UTF-8 bytes of their
   names, ascending, and always so. The cursor is the last name of the
   previous page (`next_after`), not an opaque token and not a position in a
   snapshot: the space keeps no history, so a page cannot be pinned to the
   version it was read at. The consequence is stated, not hidden: every page
   carries the `data_version` it was read at; a member that exists in all of
   the reads appears exactly once and in order, whatever was written between
   pages; a member added or removed between pages may or may not appear;
   writes before the cursor are never seen. Nothing repeats, because the next
   page starts strictly after the last name sent.
3. **Bounds.** A page has at most `limit` members and, with `select=values`,
   at most 512 KiB of member names and canonical values (the first member is
   always included, so a page always makes progress: a value is at most 256
   KiB). `next_after` is present exactly when a further matching member
   exists. The query reads only the caller's space of an artifact the caller
   may read; its cost is bounded by the space (1 MiB).
4. **Normal form.** The response repeats the query as parsed and completed
   (`path` canonical, `prefix` and `after` as given, `select` and `limit`
   with defaults filled in). Two requests that mean the same thing have the
   same `query`. A path with nothing at it, or a value there that is not an
   object, is a successful answer with `exists: false` and no members.
5. **Canonical and pending are different things.** Every field of a response
   is committed state. The result nests it under `canonical`, and the server
   never mixes an unacknowledged write into it: the server has no such
   state. A client that queues writes (R08) keeps them in a `pending` field
   beside `canonical` and shows them as pending until the write's receipt
   (`data_version`, `replayed`) arrives; it never edits `canonical`. R07
   provides the `canonical` half only.

### Source revisions and data versions

6. **They are never mixed.** A data version is `data_version` and a position
   in a data stream is a _data cursor_. Neither is named `revision`, and a data
   cursor has a different text form from a change feed cursor (`d.<feed>.<n>`
   against `<feed>.<n>`), so passing one where the other is wanted is a `400`,
   not a silent mismatch. No source revision appears in a data response, no
   data version in a change feed event, and a data write is still not a
   change feed entry (0031). A source restore does not touch a stream or its
   window.

### Subscription

7. **Mechanism: server-sent events, as the change feed.** `GET
/v1/artifacts/{id}/data/events?path=` (only `path`; strict) streams the
   value at a path and every committed change to it. SSE is chosen for the
   reasons of 0006: delivery is one-directional, the bearer header is read with
   `fetch`, and `Last-Event-ID` resumes. Long polling was rejected: it needs a
   cursor and a held request per caller anyway and loses ordering between two
   waiting requests.
8. **Events.** Each event's `id` is its data cursor, repeated as `cursor` in
   the data. All values are committed (decision 5).
   - `snapshot` opens a stream that has no `Last-Event-ID`: `{cursor,
data_version, path, exists, value?}`, the value at the path as of that
     version.
   - `change`: after the write at `data_version`, the value at `path` is
     `value`, or is gone (`exists: false`). Writes that do not touch the
     watched path are not sent. `path` is the watched path when the write
     replaced it or something above it, and the written path when the write
     was below it.
   - `position`: `{cursor, data_version}`; everything up to that version has
     been sent. It follows a resumption, and any run of writes that none of
     the sent `change` events covers, so the cursor of a client watching a quiet
     path keeps up with a busy space and does not fall out of the window.
   - `reset`: the same data as `snapshot`. The cursor cannot be continued
     (decision 10). The client discards what it holds for the path and takes
     this value; later events follow it.
     A `change` carries the state after a write, not the write: for a merge
     patch it is the merged value at the patch's path, so a client needs no
     merge algorithm. Applying `snapshot` or `reset`, then each `change` in
     order, gives the value that `GET data` reports at the same `data_version`.
9. **Snapshot, then replay.** A stream with no `Last-Event-ID` sends `snapshot`
   then follows. A stream with a data cursor continues after its version:
   it replays the `change` events of the writes after it, in version order,
   then `position`, then follows live. Replay and live delivery read the same
   window, so a write is sent once: no gap, no duplicate, in the order of the
   data versions. A client that applies each event once and stores the last
   cursor resumes without loss.
10. **The retained window and `reset`.** The server keeps, per actor and
    artifact, the last writes in process memory: at most 64 writes and 256 KiB
    of values (a write larger than the byte bound is not retained and also
    breaks the window). At most 256 windows are kept in the process and 32 per
    actor, least recently written first; a window with an open stream is never
    dropped. A cursor is continued only when the window holds every write after
    it. Otherwise the server sends `reset` with a current snapshot: the cursor
    is older than the window, is ahead of the data, names another database
    (a restore; the cursor carries the feed identity of 0006), or the window
    was lost. The window is **not persisted**: a restart empties it, so a
    cursor from before a restart is reset unless nothing was written since. A
    write that this process did not make (another process on the same database)
    is noticed at the next check and resets the streams behind it. A check
    runs every 15 seconds and after any commit of this process, and also ends a
    stream whose artifact went to the trash or was purged.
11. **Order inside the process.** A write and the recording of its event
    happen under one lock per space, so the window lists the writes in the
    order they committed. A data write over HTTP and over MCP both pass
    through that one place.

### Access and limits

12. **Isolation.** Both routes read the caller's own space through
    `ArtifactService`: an artifact that is missing, in the trash, purged, or
    another user's is `404 not_found` with the body of an artifact that does
    not exist, for the owner token, a user token, and an owner agent token
    alike. A window is keyed by actor and artifact; a stream sees only its
    caller's writes. Mutations are unchanged: only `ArtifactService` writes,
    with a base data version and an idempotency key.
13. **Revocation.** An open stream registers where the change streams
    register, with its token, and confirms the user generation after
    registering (`ensure_current`, 0036). Disabling a user, revoking or
    rotating a token ends the streams of that user or token at once, and a
    stream that raced the disable is ended by the confirmation. A disabled
    user's streams therefore close, and its windows hold nothing a new stream
    can read, because a new stream is `401`. A purge also drops the windows of
    the artifact, so a purged value does not stay in memory for later streams.
14. **Limits.**
    - **Streams:** 8 open per actor; the next is `429 rate_limited` with
      `Retry-After: 5`. A stream holds no request slot while it is open; opening
      one is a request under the limits of 0005, so a client that reconnects
      in a loop is limited by the request rate.
    - **Bytes:** a stream holds one event in flight (a channel of one), so a
      reader that stops reading holds back only its own producer and its
      events are not queued; the window is bounded by decision 10; one event
      is at most one space (1 MiB); a snapshot or a page is at most one space.
      A reader that falls behind the window is reset, not queued.
    - **Rate:** an event exists only for a committed write, and a write is a
      request under the limits of 0005; an actor's own writes cannot reach
      the stream faster than they are admitted. There is no separate event rate
      limit. One write is sent to each of the actor's open streams (8 at
      most).
15. **Shutdown.** `close_streams` ends data streams with the change streams.

### Surface

16. **Contract 0.15.0:** `ListDataMembersQuery`, `DataMembers` (with
    `DataMembersQuery`, `CanonicalMembers`, `DataMember`), the event data
    `DataSnapshotEvent`, `DataChangeEvent`, `DataPosition`, `ReadDataEventsQuery`.
    The TypeScript client: `listDataMembers`, `openData`, `followData` (the
    retry rule of 0006 decision 10, restarting from the last cursor), and
    `applyDataEvent`, a small reducer used by the tests and available to apps.
    Their module is imported only by callers that use it: the web entry does
    not grow.

## Not implemented

- **No frame or runtime-bridge capability**, no runtime SDK function, no web
  UI. Frames still cannot read or write data (0031).
- **No MCP tool and no CLI command.** MCP is request/response (0004) and a
  `list_data` tool is possible later; neither was added to keep this change
  small.
- **No outbox, no pending writes, no acknowledgements beyond the write
  receipt** (R08). Decision 5 only fixes where they will go.
- **No persistence of the window.** After a restart every cursor is reset
  unless nothing was written since; there is no replay across a restart.
- **No cross-process fan-out.** A second server process on the same database
  is noticed by a periodic check and resets streams, it does not replay.
- **No data in the change feed**, no value predicates, sort other than by
  name, aggregates, totals, or queries across artifacts or actors; no
  traversal into arrays.
- **No pinned pages**: a page is not tied to the version of the page before
  it (decision 2).
- **No event rate limit** beyond the request rate of the writer (decision 14).
- No claim of network isolation or of frame capability enforcement.

## Tests

Domain: query validation and normalization (every rule, offsets, no echo of
input), ordering, `limit`, the byte bound, `after`, `prefix`, and the
projection of a write onto a watched path. Server: the window (contiguity,
trimming, oversize, anchoring, eviction without an open stream), and through
the real router the behavior named above: ordering, cursor stability under
concurrent writes, snapshot then replay with no gap and no duplicate across a
reconnect, `reset` past the window and for another database, concurrent
writers against a mirrored state, the 404 for another user, disabling closing
streams, the stream limit, and a client that goes away freeing its place.
TypeScript: the client against the real server binary, including a restart.
Important tests were checked to fail when the guarded behavior is broken
(mutation notes are in the pull request).

## Alternatives

- **Stream the whole value after each write.** Simple and idempotent, but it
  sends up to 1 MiB for every small write and cannot say what changed;
  coalescing also hides that `change` events are ordered and complete.
- **Persist a change log.** Replay across restarts and processes, at the cost
  of a schema step, a log that holds written values (erasure on purge, backup
  contents), and its own retention. The window can be persisted later; clients
  already handle `reset`.
- **Send the write (set, patch, remove) to subscribers.** Smaller events but
  every client would implement merge patch and path creation exactly as the
  server does.
- **An opaque, signed page cursor.** Without history it could pin nothing; the
  member name states honestly what it is.

## Costs

An in-memory structure with its own lock per space, bounded at about 64 MiB in
the worst case; a write that merges also reads the space once more to learn the
merged value; a reset after every restart; a periodic read of the space per
open stream (every 15 seconds); and a promise that the event vocabulary and the
cursor text stay stable.
