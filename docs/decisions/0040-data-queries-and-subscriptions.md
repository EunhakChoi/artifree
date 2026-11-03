# 0040: Data member queries and data subscriptions

Date: 2026-11-04. Status: accepted. Builds on
[0031](0031-app-data.md) (decisions 1, 2, 3, 4, and 8),
[0006](0006-change-feed.md) (the stream style, the heartbeat, shutdown),
[0036](0036-multi-user-identity.md) (decision 11: registrations and
`ensure_current`), and [0005](0005-request-limits.md). Roadmap row: R07.
Independent Artifree design. The roadmap row asks for a validated, normalized,
bounded query and a change subscription, and for source revisions and data
versions never to be mixed; every name, bound, and rule below is Artifree's own
decision.

## Context

R06 (0031) stores one JSON object per actor and artifact with its own
`data_version`, readable only by a path. An app that keeps a collection (todos
as members of one object) can read only the whole collection or one member it
already knows the name of, and learns about a change only by polling. R07 adds
one query that lists the members of a collection path in pages, and one
subscription that follows the value at one path.

**Server and TypeScript client only.** No page can use either: the frame
bridge, the runtime SDK, the web workspace, the CLI, and the MCP tools are not
changed. The outbox (R08) and any other subscription kind are not implemented.

## Decisions

1. **Two version spaces, two names.** `data_version` is the version of the
   data space (0031). A source revision is `revision`, a metadata revision is
   `metadata_revision`. The new responses and events carry only
   `data_version`; no new field can hold a source revision.
2. **Query `GET /v1/artifacts/{id}/data/members`.** A strict query (an unknown
   or repeated parameter is `400`):
   - `path`: the grammar of 0031 decision 2 (0 to 16 segments of 1 to 64
     `[A-Za-z0-9_-]`; absent or empty is the root). A breach is `422
invalid_request`.
   - `select`: `names` (default) or `values`.
   - `limit`: a whole number from 1 to 100 (default 50), else `422`.
   - `after`: a member name, 1 to 256 bytes (0031 decision 3), the cursor.
     Members after it in order are returned. Empty or longer is `422`.

   The answer is `DataMembers {artifact_id, path, select, limit, after?,
data_version, is_collection, members, next_after?}`. The first five fields
   echo the normalized query. `is_collection` is true when an object is at the
   path; for a missing path or any other value it is false, `members` is
   empty, and the answer is still `200`. Members are in **UTF-8 byte order of
   their names** (not UTF-16 order), strictly after `after`, at most `limit`.
   `next_after` is present exactly when more members follow, and is the name
   of the last member returned; a page that ends early (below) always has it.
   Reading the whole collection is following `next_after` until it is absent;
   a write between two pages is seen by the page after it, and
   `data_version` of each page says which version that page was read at.

3. **Bounded pages.** Under `select=names` a member is `{name}`. Under
   `select=values` it is **exactly one of** `{name, value}` or `{name,
too_large: true}`: a member whose canonical value is larger than 32 KiB
   (`MAX_MEMBER_VALUE_BYTES`) has no value and is marked `too_large`, so a
   caller reads it by `GET data?path=`. The values of one page add up to at
   most 256 KiB (`MAX_PAGE_VALUE_BYTES`); the page ends before the member that
   would pass it, with `next_after`. A page always holds at least one member
   when one exists, because one value is at most 32 KiB. The size is measured
   on the canonical text, in bytes.
4. **Subscription `GET /v1/artifacts/{id}/data/events`** with `path` (the same
   grammar), as server-sent events in the style of 0006: the bearer header
   only, `Accept: text/event-stream`, a comment line after 15 idle seconds.
   There is **no replay, no retained history, and no resume cursor**:
   `Last-Event-ID` is refused with `400 invalid_request`, and a client that
   reconnects receives a fresh snapshot. Events:
   - `snapshot`: the first event, once. Data is `AppData` (0031): `{artifact_id,
path, data_version, exists, value?}`.
   - `change`: the **full current value** at the path, as `AppData`, sent when
     the data version read is greater than the last one sent on this stream.
     A write elsewhere in the space moves the version and so is also sent; a
     reader that wants only its path compares values.
   - `too_large`: `{artifact_id, path, data_version, max_bytes}` instead of a
     `snapshot` or a `change` whose canonical value is larger than 128 KiB
     (`EVENT_VALUE_BYTES`). The stream ends after it. A reconnect would meet
     it again, so the client treats it as permanent (`data_too_large`).

   `data_version` is strictly increasing on one stream. Events carry no `id`.

5. **Reads, not queues.** The server holds, per stream, the version last sent
   and nothing else about the data: no value, no history, no per-artifact
   state. A process-wide notification (a counter in a `watch` channel, no
   payload) is sent **after** a data write commits in this process; sending it
   cannot fail or wait, so a committed write is never refused or delayed by a
   subscriber. A stream that is woken, or whose 15 s heartbeat is due, reads
   the space from storage. Writes by another process arrive at the next
   heartbeat. A committed metadata update (trashing is one) also sends the
   notification, so that a stream ends at once rather than at the heartbeat.
   A purge needs the artifact in the trash, so no stream is open by then.
6. **Pacing.** A stream reads at most once per `min interval` (250 ms): wakes
   within it are coalesced into one read at its end, so a busy writer costs
   at most four reads per second per stream. The loop decides in this order:
   the stream's lifetime (10 minutes), then whether a read is due, then
   waiting; **the deadline checks come before any wake is looked at** and every
   wait is bounded by the lifetime end, so a writer that never stops cannot
   keep a stream open past its lifetime, and a throttle wait cannot pass it.
   The lifetime ends the stream cleanly (the body ends); the client reconnects
   and receives a fresh snapshot. The decision is a pure function of
   instants (`Pace::step`) so that tests give it any times.
7. **Every read re-checks access.** The read is the data read of 0031: an
   artifact that is missing, in the trash, purged, or another actor's ends
   the stream (`404` at the start). A stored space that cannot be parsed is a
   typed storage error (`500 internal` at the start, a closed stream
   afterwards), and a value that cannot be written as JSON is the same error:
   it is never dropped or sent as absent. Shutdown (0006 decision 8), a
   revoked token, and a disabled user end the stream through the registrations
   of 0006/0036: it registers with the change feed's registry **after** it
   holds a place, and calls `AppState::ensure_current` after registering.
   Nothing the stream held outlives it (the place, the registration, and the
   subscription are dropped with the task).
8. **Limits.** Per actor 8 streams (`429 rate_limited`, `Retry-After: 5`) and
   per process 128 (`503 unavailable`, `Retry-After: 5`), both counted in one
   mutex-guarded table with at most 128 entries. The order of an open is
   fixed so storage work stays bounded: (1) validate the request without
   storage; (2) a **non-recording** check of both counts, refusing with zero
   storage reads; (3) the first read, which is the access check, so a missing
   or foreign artifact is the usual `404` and **holds no place and registers
   nothing**; (4) take the place, re-checking both counts under the lock
   (a request that lost a race is refused here after one read); (5) register
   for revocation and `ensure_current`. Opening a stream is also a request
   under the limits of 0005, as the change feed is. No lock is held across an
   await or across a storage call.
9. **Isolation (0036).** Both routes are artifact-scoped: another user's
   artifact is the same `404` as a missing one, an issued token of the owner
   profile works, and the route table test of the multi-user suite classifies
   both routes.
10. **TypeScript client.** `listDataMembers` and `followData` are functions in
    their own modules, not methods of `ArtifreeClient`, so the web entry
    chunk does not grow.
    - Both validate every parameter locally against the server grammar before
      anything is sent (a path with a leading, trailing, or doubled `/`, a
      bad character, a 65-byte segment, 17 segments, non-ASCII; `limit`;
      `after` by UTF-8 bytes and well-formedness) and compare the answer's
      echoed values with what they sent. The constants are pinned to the
      Rust source by a test.
    - `listDataMembers` reads at most 1 MiB of body and checks the fields
      exactly: no unknown field, the order by UTF-8 bytes, the count,
      `after` and `next_after`, one of `value` and `too_large` per member.
    - `followData` parses the event stream as bytes, not strings: the
      limits on a line, on the data of an event, and on the unfinished
      line are in **UTF-8 bytes of the stream**, newlines are searched only
      in each new chunk, and invalid UTF-8 is `invalid_response`. Every event
      is validated: its name (anything but the three is `invalid_response`),
      exactly its documented fields, `artifact_id` and `path` equal to what
      was requested, `exists` and `value` consistent, a `data_version` that
      is a safe integer and, on one connection, strictly greater than the one
      before; a `snapshot` is first and only first, a `change` never first, a
      `too_large` first or later and last. A version above 2^53 - 1
      (`Number.isSafeInteger` is false after parsing) is rejected with a
      message that says so, never compared. A reconnect's first event must not
      be below a version already delivered (a database restored from an older
      backup needs a new `followData`).
    - Errors: a lost connection, `429`, any `5xx`, and a stream that ended
      cleanly are retried; every other failure, `invalid_response`,
      `data_too_large`, and an error thrown by the handler, rejects.
      The wait is the backoff (from `min` doubling to `max`, restarted only
      after a connection that lasted `stableMs`), or `Retry-After` when
      larger, never below `min` and never above one hour. A clean close waits
      too, so a server that closes at once is not hit in a loop. An aborted
      signal resolves before any request, during a wait, and during a read,
      and no event is delivered after it. Timers and the clock are injected in
      tests.
11. **Contract 0.15.0:** `DataSelect`, `ListDataMembersQuery`, `DataMember`,
    `DataMembers`, `WatchDataQuery`, `DataTooLarge`. No new error code.
    `Settings` gains `data_streams` (limits and timings) so a test can change
    them; the command line does not.

## Not implemented

- No use from a frame, the runtime SDK, the web workspace, the CLI, or MCP.
- No filter, sort other than byte order of names, count, or aggregate; no
  query over several paths or into arrays (0031 decision 2).
- No replay, resume, `Last-Event-ID`, history, or delivery guarantee: a
  subscriber sees at most the states it read, not every write.
- No push from other processes: they arrive at the heartbeat (15 s).
- No outbox or pending/acknowledged distinction (R08).
- No persistent or cross-process sharing of stream state (X04).
- No network isolation claim: the limits bound this server's memory and
  storage reads, not what a client does with its network.

## Limits and costs

- Every read parses the whole space (at most 1 MiB), at most four per second
  per stream and 128 streams per process.
- An idle stream costs a task, a place, a registration, and a timer; it holds
  no data.
- A subscriber that does not read holds its place until it is disconnected,
  revoked, or reaches its lifetime of 10 minutes.
- A burst of writes is seen as one `change` with the latest value.

## Tests

Server: `crates/artifree-domain/src/members.rs` (order by UTF-8 bytes against
UTF-16 order, cursor, limit, both bounds and one byte over each, `too_large`,
a non-collection), `crates/artifree-server/src/data_events/tests.rs` (the pace
decision for any instants; the table of places; every refusal reads no
storage; snapshot then change, coalescing and strictly increasing versions,
`too_large`, lifetime and heartbeat with a busy writer, trash, a corrupt
stored space, disabled user, revoked token, shutdown, the limits, that a write
succeeds while subscribers hold their places, and that opens on a one-thread
runtime with one blocking thread do not deadlock),
`crates/artifree-server/tests/data_members.rs` (the members route), and the
route table of `tests/multi_user.rs`. Client: `scripts/tests/data-queries.test.mjs`.

## Alternatives

- **A change log with replay and a resume cursor.** It needs a retained
  window, a reset event, and a place to keep it; a fresh snapshot costs one
  read and cannot be wrong about what it skipped.
- **Pushing every write to every stream.** It makes the commit path depend on
  its subscribers; a notification that carries nothing cannot.
- **One shared per-artifact cache.** State shared between streams needs
  invalidation and can outlive purge; per-stream reads cannot.
- **`Last-Event-ID` ignored.** A client that believes it resumed would be told
  nothing; a refusal says that it did not.

## Costs

Two routes, six wire types, one more registry of places, a client parser of
its own (the byte-limited reader is not shared with 0006's `openChanges`,
which counts characters), and a promise that the grammar constants of the
client and the server stay equal.
