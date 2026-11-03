# 0040: Data queries and data subscriptions

Date: 2026-11-04. Status: accepted. Builds on
[0031](0031-app-data.md) (application data and its version),
[0006](0006-change-feed.md) and [0007](0007-live-web-workspace.md) (server-sent
events, wake-ups, the client retry rule), and
[0036](0036-multi-user-identity.md) (per-user isolation and what a disable
closes). Roadmap row: R07. Independent Artifree design: the private evidence
index gives this row only the idea of a bounded query and of a subscription
that carries state, and names the query grammar, the event shapes, the limits,
and every error as Artifree's own decisions. Nothing below is an observation
of the original product.

## Context

Decision 0031 left two things out on purpose: a way to read a collection a
page at a time without fetching the whole value, and a way to learn that data
changed without polling `GET data`. R07 adds both on the server and in the
TypeScript client. The goal is the smallest design that is safe to run: **no
replay window, no per-artifact registry, no resume cursor.** A subscriber is
told the current state when it connects and the current state again whenever
the data version moves; a client that was away connects again and is told the
current state.

**Not reachable from a page.** As in 0031, no preview or runtime frame can use
these routes: there is no runtime bridge capability for data, the runtime SDK
is types only, and the web workspace has no data panel. What exists is HTTP and
the TypeScript client, with the owner's token or an issued token.

## Decisions

1. **Two separate numbers.** `data_version` (0031) is the version of the data
   space. The source revision and the metadata revision are other numbers with
   other names. A query response and a stream event carry `data_version` and
   no revision; nothing in this record reads or changes a source revision.
2. **Query: `GET /v1/artifacts/{id}/data/members`.** It lists the members of
   the object at `path` (default the root). Parameters, all optional and
   strict (an unknown or repeated parameter, or one of the wrong type, is
   `400`; a value that breaks a rule below is `422 invalid_request` and never
   echoes the value):
   - `path`: a data path (0031 decision 2).
   - `prefix`: only members whose name starts with it; at most 256 bytes;
     default empty.
   - `select`: `names` (default) or `values`.
   - `limit`: 1 to 100, default 25.
   - `after`: a member name (1 to 256 bytes); only members whose name sorts
     after it are listed.

   Members are ordered by the UTF-8 bytes of their names, which is the
   canonical order of 0031, so the order is the same for every request and
   after every write. The cursor is a member name, not a position: a write
   between two pages inserts or removes members without repeating or skipping
   one that was not written. Each page is read from one snapshot of the space.

3. **Query response.** `DataMembers {artifact_id, query, data_version,
collection, members, next_after?}`. `query` echoes the normalized request
   (`path` as parsed, `prefix`, `select`, `limit`, `after` when given).
   `collection` is true when an object is at the path; a path with nothing
   at it, or a value that is not an object, is a successful answer with
   `collection: false` and no members. `next_after` is present exactly when
   more members follow this page; passing it as `after` with the same other
   parameters reads the next page.
4. **Bounded pages.** Under `select=values` a member is `{name, value}` with
   the canonical text of its value, and the values of one page total at most
   128 KiB (131,072 bytes; names are at most 256 bytes each and 100 per
   page). A page ends before the member that would pass the bound, and
   `next_after` names the last member that fit. **A member whose canonical
   value alone is over 128 KiB is listed as `{name, too_large: true}` with no
   value, and costs the page nothing.** Under `select=values` every member
   has exactly one of `value` and `too_large`; under `select=names` it has
   neither. Whoever needs a large value reads it with `GET data` at its path.
5. **Subscription: `GET /v1/artifacts/{id}/data/events?path=`.** One watched
   path (default the root; strict query as `GET data`; the grammar limits it
   to 16 segments of 1 to 64 bytes, so a path is at most 1,039 bytes). It is
   a `text/event-stream` as in 0006: bearer header only. Events:
   - `snapshot`: sent once, first. Its data is `AppData` (0031): `artifact_id`,
     `path`, `data_version`, `exists`, and `value` when it exists.
   - `change`: the same shape, carrying **the new full value** at the path
     and the new `data_version`. It is sent when the space's version is newer
     than the last one this stream sent, also when the write was elsewhere in
     the space (the value at the path is then unchanged and only the version
     moved).
   - `too_large`: `{artifact_id, path, data_version, max_bytes}`. Sent instead of a
     snapshot or change whose canonical value is over 128 KiB; the stream then
     ends. Nothing larger is ever sent.
   - A comment line every 15 seconds keeps the connection open.

   Events carry no `id`, so no client sends `Last-Event-ID`; a request that
   does is `400 invalid_request`, because honoring it would claim a resume this
   design does not have.

6. **Versions in a stream strictly increase, and bursts coalesce.** The
   stream reads storage after a wake-up and sends the current state if its
   version is newer than the last one sent. It never sends an older or equal
   version. If a read ever shows an older version than one already sent (a
   database replaced under the server), the stream ends and the client
   connects again. Writes that happen faster than the stream reads are not
   queued: the stream reads at most once every 250 ms and sends only the
   newest state, so two writes can reach a subscriber as one event whose
   version skips a number. **Replay is not implemented**: there is no memory
   of past states, no catch-up, and no way to ask for what was missed. The
   client's reducer must replace its state with each event, never apply it as
   a difference.
7. **The wake-up is the only shared state.** A write that changed the space
   sends one signal after its transaction committed (a `watch` channel that
   carries no data; sending cannot fail and its result is not used, so a
   subscription can never fail a write). The commit signal of the change feed
   (0006) is **not** awaited: it wakes on a commit by anyone to any artifact,
   so with many streams open every commit would cost every stream a read of
   its data space for nothing. Trashing or purging an artifact does not move
   its data version and so does not wake a stream either. Every 15 seconds a
   stream reads again regardless, so a trash, a purge, or a commit by another
   process on the same database reaches it within that time. A stream holds no copy of the data between reads except the one state
   it is about to send.
8. **Authorization and isolation.** Both routes are artifact-scoped under the
   rules of 0031 decision 8 and 0036: the artifact must exist, be outside the
   trash and not purged, and belong to the caller; any other case, another
   user's artifact included, is the same `404 not_found` as a missing one.
   A request is checked against the limits of item 12 first, before any
   storage is read, so a refused request costs no storage work; a request for
   an artifact the caller cannot see then holds its place only while its own
   check runs, registers nothing, and creates nothing. (At the actor's limit
   the answer is the limit's, `429`, whatever the artifact; it depends only on
   the caller's own streams, so it tells nothing about any artifact.)
   Issued agent tokens may use both routes, as they may read data.
9. **A stream ends when access ends.** The stream reads storage at every wake
   and every heartbeat, so a trashed or purged artifact ends it at the next
   heartbeat read: within 15 seconds.
   Disabling a user or revoking a token ends the streams of that user or token
   through the same registrations as change streams, and a stream that is
   opened by a request that authenticated before the disable confirms the
   user after registering (`AppState::ensure_current`, 0036 item 11). A stream
   also ends after 10 minutes in any case, and the server's shutdown ends
   every stream. A client treats every end as normal and connects again.
10. **A stream that cannot read ends.** A stored space that cannot be read
    back (it breaks the rules of 0031) is a storage failure: a query is `503
unavailable`, and a stream ends without sending the unreadable state; it
    never skips the member or the event and never continues. The same for any
    other storage failure while the stream runs.
11. **A slow reader holds a slot and nothing else.** The stream's task waits
    for room to send; while it waits it still reads at each wake-up, keeps only
    the newest state, and ends if the artifact is gone. So a reader that has
    stopped reading cannot keep a purged artifact's stream alive, and at most
    one state (the one in flight) and one event in the response channel exist
    for it. The response itself stays until the client reads or disconnects;
    the server cannot close a connection its peer does not read.
12. **Limits.** Defaults are server settings (not command-line flags):
    - 8 open data streams per user. One more is `429 rate_limited` with
      `Retry-After: 5`.
    - 128 open data streams in the process. One more is `503 unavailable`
      with `Retry-After: 10`. Both limits are checked first, before any
      storage is read or the stream is registered, and a refusal inserts
      nothing. Slots are released when the
      stream's task ends, whatever ends it.
    - Opening a stream is an ordinary request under the request limits
      (0005); an open stream holds no request slot.
    - One read of storage per stream per 250 ms, whatever wakes it.
    - 128 KiB of canonical value per event (decision 5), 128 KiB of values per
      query page (decision 4), 100 members per page.
    - 10 minutes of lifetime per stream, 15 seconds between heartbeats. The
      end of the lifetime is checked ahead of every wake-up and bounds the wait
      for the read interval, so writes that never pause cannot extend a stream
      past it.
13. **Client (`@artifree/client`).** Two functions in modules of their own,
    not methods of the client class (that class is in the web workspace's main
    bundle, which has no room):
    - `listDataMembers(client, artifactId, query, options)` validates the
      response at runtime: the echoed query equals the request, members are
      strictly ordered by their UTF-8 bytes, begin with the prefix and after
      the cursor, are at most `limit`, each has exactly the fields its
      `select` allows, and `next_after` is the page's last member when
      present. It does not measure the bytes of the values.
    - `followData(configuration, artifactId, options)` opens the stream and
      reconnects until aborted. It parses with its own server-sent-event
      reader with a limit on one line and on one event (256 KiB); the shared
      change-feed reader is unchanged. The first event of a connection must
      be `snapshot`, `data_version` must strictly increase within a
      connection, and `artifact_id` and `path` must be the ones asked for;
      anything else is `invalid_response` and rejects. A `too_large` event
      rejects with `data_too_large`. `onEvent` receives every event; the next
      snapshot replaces whatever came before, also with a lower version (a
      restored database).
    - Reconnection waits `Retry-After` when the server gave one, up to a
      ceiling of one hour, otherwise from 0.5 seconds doubling to 30 seconds;
      every wait is at least 0.5 seconds. A stream that ends cleanly waits
      too, and the doubling restarts only after a connection that stayed open
      for 30 seconds. A connection that receives nothing, comments included,
      for 45 seconds is dropped as a `network_error`. A `4xx` other than `429`
      rejects (retrying cannot fix it), `429` and `5xx` and connection
      failures retry. Aborting resolves at once, also during a wait, and no
      event follows it. Timers and the clock are injectable, so tests do not
      sleep.

## Not implemented

- **Replay, catch-up, a resume cursor, or an event log.** A subscriber that
  misses events is told the current state on its next connection and nothing
  else.
- **CLI, MCP tools, a runtime-frame capability, the runtime SDK, a web
  panel.** A page cannot query or subscribe. Agents have no stream: MCP is
  request and response (0004).
- **The client outbox, pending and acknowledged intents, and tab ownership.**
  That is R08; nothing here distinguishes a pending write from a committed one,
  because the client has none.
- Subscriptions to several paths or artifacts in one stream, filters on
  values, sorting by anything but name, offsets, counts, and queries into
  arrays.
- A stream that shares a process with others only: limits and wake-ups are in
  one process's memory (X04).
- Network isolation or any claim about what a page can reach (0003).

## Tests

`crates/artifree-domain/src/data_members.rs` (query rules, order, prefix and cursor
bounds, the byte bound, the `too_large` member, bytes versus characters in
the order), `crates/artifree-server/src/data_events.rs` (slot accounting,
version decisions, keeping the newest state of a stream that is behind), `crates/artifree-server/tests/data_queries.rs` (paging
under concurrent writes, every refusal, the response bounds, snapshot then
changes in order, coalescing, the least time between reads, a commit waking a stream without
the heartbeat, a fresh snapshot after reconnect, two users, a
trashed and a purged artifact, a disabled user and a revoked token, the stream
and process limits, a missing artifact taking nothing, `too_large`, a corrupt
space, a slow reader that does not keep a purged artifact's stream, the
lifetime, `Last-Event-ID`), the extended route table of
`crates/artifree-server/tests/multi_user.rs`, `scripts/tests/data-client.test.mjs`
(parsers, reconnection with injected timers, abort, idle, line limits), and
`scripts/tests/data-stream.test.mjs` (the real binary, end to end).

## Alternatives

- **A replay window per artifact.** It would let a client resume, at the cost
  of retained copies, an eviction policy, its own lock, and a cursor that must
  be bound to an artifact, a feed, and a path. A snapshot on every connection
  costs one read and needs none of that.
- **Sending a difference.** It needs ordering guarantees and replay to be
  correct. The full value at a bounded path is simple and self-correcting.
- **Offset paging.** It repeats or skips members when a write lands between
  two pages.
- **Putting data changes into `/v1/changes`.** The feed names artifacts and
  revisions for every client of the web workspace; data changes are per path
  and per version and would grow its reducer (0031 decision 14).
- **A WebSocket.** As in 0006, delivery is one-directional.

## Costs

A stream rereads and reparses the whole space (at most 1 MiB) at each
wake, at most four times a second. A write elsewhere in the space still sends
an event to every subscriber of the artifact. A subscriber that is slower than
the writes sees only some of the states. Several streams of one artifact
repeat the same read. The process cap is global to the process and one user can
fill their own 8 but not another's.
