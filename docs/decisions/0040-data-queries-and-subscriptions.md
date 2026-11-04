# 0040: Data member queries and a data subscription

Date: 2026-11-04. Status: accepted. Builds on
[0006](0006-change-feed.md) (server-sent events and their registrations),
[0007](0007-live-web-workspace.md), [0013](0013-deleting-artifacts-and-the-trash.md),
[0029](0029-retention-and-cleanup.md), [0031](0031-app-data.md) (the data space
and its version), and [0036](0036-multi-user-identity.md) (isolation and
revocation). Roadmap row: R07. Independent Artifree design: the private
evidence index gives this row only the idea of a validated, bounded query and
of a subscription that is repaired by a fresh snapshot instead of a replay; the
grammar, the limits, and every event below are Artifree's own decisions.
Nothing below is an observation of the original product.

## Context

Decision 0031 stores one JSON object per actor and artifact, with its own
`data_version`, and says that a client polls `GET data` and compares versions.
R07 asks for a query with a validated, normalized, bounded result, and for a
subscription, without mixing the source revision and the data version, and
with a defined repair after a reconnect.

Three earlier attempts at this row were closed in review. Their findings are
folded into the decisions as rules that are enforced and tested, not as
guidance: bounded registries, no side effect of a subscription on a commit,
deadlines that cannot be starved, revocation that wins over sending, honest
coalescing, byte-exact parsing in the client, and an explicit retry table.

## Decisions

### 1. Names: two numbers that are never mixed

Responses and events of this record carry `data_version` and never a
`revision` or `metadata_revision`. A source revision (0003) and a metadata
revision (0008) are not read, changed, or reported here. The client's
validators refuse an object that carries both kinds of field in the data
shapes.

### 2. `GET /v1/artifacts/{id}/data/members`

Lists the members of the object at a path of the caller's data space.

Query (strict: an unknown or repeated parameter, or a value that is not of the
parameter's type, is `400 invalid_request`; a well-formed value that breaks a
rule below is `422 invalid_request`, naming the rule and never the value):

| Parameter | Rule                                                                     | Default |
| --------- | ------------------------------------------------------------------------ | ------- |
| `path`    | the path grammar of 0031: 0 to 16 segments of 1 to 64 `[A-Za-z0-9_-]`    | root    |
| `select`  | `names` or `values`                                                      | `names` |
| `limit`   | a whole number from 1 to 100                                             | 50      |
| `after`   | a member name, 1 to 256 bytes: only members after it in order are listed | none    |

Response `DataMembers`: `artifact_id`; the **normalized query** as it was
applied (`path`, `select`, `limit`, and `after` only when given); the
`data_version` the page was read at; `exists` (something is at the path);
`collection` (it is an object); `members`; and `next_after` when more members
follow. The client sends every parameter explicitly and refuses an answer that
echoes anything else.

- **Order.** Members are in the order of their names as UTF-8 bytes, the order
  of the stored canonical form (0031). `after` is exclusive and need not be a
  member. A page continues from `next_after` as `after`, with the same other
  parameters.
- **Bounds.** A value is `too_large` when its canonical text is over 32 KiB. A
  page holds members until `limit`, or until the next member would take the
  sum of the name bytes and listed value bytes over 256 KiB; the first member
  of a page is always listed, so a page makes progress. Either end of a page
  with members left sets `next_after` to the last listed name.
- **Shape of a member.** `name`; under `select=values` exactly one of `value`
  (the canonical JSON, `null` included) and `too_large: true` (no `value`);
  under `select=names` neither.
- **Not a collection.** A path with nothing at it is `exists: false,
collection: false`; a scalar or an array is `exists: true, collection:
false`; neither lists members and neither has `next_after`. The root is
  always a collection.
- **Read.** One snapshot, like `GET data`: the artifact must be in place for
  the actor, and a missing, trashed, purged, or another actor's artifact is the
  same `404`. **A stored space that cannot be read back is `500 internal`**
  (also for `GET data` and for writes, which were `503` before): it is a
  fault of the store, retrying does not fix it, and an empty answer would
  hide it.

### 3. `GET /v1/artifacts/{id}/data/events`

Server-sent events about **one** watched path (`?path=`, the grammar above;
strict like the query). The credential is the bearer header only. There is no
replay, no retained window, and no resume: a request with `Last-Event-ID` is
`400 invalid_request`, and a client that lost its stream opens a new one and
receives a fresh snapshot.

| Event       | Data                                                  | When                                                        |
| ----------- | ----------------------------------------------------- | ----------------------------------------------------------- |
| `snapshot`  | `AppData` (0031): value at the path and version       | first event, once                                           |
| `change`    | `AppData`: the **new full value**                     | the data version moved since the last event sent            |
| `too_large` | `DataTooLarge`: `artifact_id`, `path`, `data_version` | the value at the path is over 128 KiB; the stream then ends |

- **Order and versions.** `snapshot` or `too_large` is first. Versions are
  strictly increasing within a stream; a read that finds a lower version ends
  the stream (it cannot happen while the version never resets, 0031). A read
  that finds the same version sends nothing.
- **Coalescing.** A stream reads the space at most once per 250 ms and sends
  the newest state, not every intermediate one. Before it starts a read the
  stream marks the process's write notification as seen (a `watch` value taken
  with `borrow_and_update`): writes before that point are covered by the read
  and cause no second one; a write after it stays pending and causes exactly
  one more read after the interval.
- **Wake-ups.** A write that changed a space, and moving an artifact to the
  trash, bump one per-process counter after they commit. (Restoring or purging
  an artifact needs none: a stream cannot be open for an artifact in the
  trash.) The counter
  carries no data and no list of streams, and bumping it cannot fail or wait
  (a `watch::Sender::send_modify`). A commit by another server process on the
  same database is seen at the next heartbeat. That is the only cross-process
  mechanism: **cross-process revocation and wake-ups are not supported**; a
  token revoked through another server process is not seen by an open data
  stream until its lifetime ends, as for every existing long-lived stream
  (X04).
- **Heartbeat read.** At least every 15 seconds a stream reads again, which
  checks that the artifact is still in place for the actor (a trashed or
  purged artifact ends the stream), and sends a change if the version moved.
  The reader sees a comment line as a keep-alive.
- **Lifetime.** A stream ends cleanly after 10 minutes. The client reconnects
  and gets a snapshot.
- **End without an event.** The artifact leaving the actor's reach, storage
  failing, an unreadable stored space, revocation, shutdown, and the reader
  leaving end the stream with no event; the client's reconnect learns the
  reason from the answer to its next request (`404`, `500`, `401`, `503`).
- **Retention.** A stream keeps only the last version sent. The value it read
  is dropped after its event is handed to the connection.
- **Deadlines first.** The lifetime and the heartbeat are checked before a wake
  is looked at, in every wait, and a throttle wait is cut at the lifetime, so
  a stream under constant writes still ends on time and rechecks access.
  Waiting to hand an event to a reader that does not read is cut at the
  lifetime too.

### 4. Admission and limits

| Limit                           | Value   | Refusal                              |
| ------------------------------- | ------- | ------------------------------------ |
| Data streams per actor          | 8       | `429 rate_limited`, `Retry-After: 5` |
| Data streams in the process     | 128     | `503 unavailable`, `Retry-After: 10` |
| Registrations (open or opening) | 256     | `503 unavailable`, `Retry-After: 10` |
| Value at the watched path       | 128 KiB | the `too_large` event                |

Opening a stream is a request under the request limits of 0005; an open stream
holds none of its slots. The order of opening is part of the contract:

1. The artifact identifier, the path, and `Last-Event-ID` are validated.
2. A **non-recording precheck** compares the actor's and the process's counts
   with the limits and refuses with **no storage read** and without taking
   anything.
3. The stream is **registered** (token streams only: the owner profile has no
   token to revoke and cannot be disabled) **before any slow read**, as the
   change feed does (0006, 0036), so a revocation that runs from now on reaches
   it. The registration is bounded (the third row).
4. `ensure_current` confirms the user after registering, so a disable that
   passed earlier is seen.
5. The first read is the **access check**: a missing, foreign, trashed, or
   purged artifact is `404` and holds nothing but its registration.
6. If revocation or shutdown became visible while the read ran, the request is
   refused (`401` or `503`) and no event is sent.
7. The **place** is taken in one step that re-checks both counts under the
   registry lock, and the producer starts with no await between taking the
   place and spawning it.

Registrations and places are RAII values owned by the request and then by the
producer; every exit drops them. The registry lock is a plain mutex that is
never held across an await, so no task waits on it while it waits for the
blocking pool.

### 5. Revocation and shutdown win over sending

Revoking a token, disabling a user (0036), and shutdown end open data streams
through the same registrations as the change feed. A producer checks, before
**every** send, whether it is ended, and its waits and sends are `biased`
selects whose first arms are shutdown and revocation, so no event is handed to
the connection after a signal that is already visible.

### 6. A commit is never touched by a subscription

The wake-up is bumped after the commit, without a lock and without an error,
and the write has already succeeded. A poisoned registry makes new streams
fail with `503`; it does not make a write fail.

### 7. Client (`@artifree/client`)

Two standalone modules, loaded only by a caller that imports them; nothing in
the web entry imports them.

- `listDataMembers(client, artifactId, query, options)` validates the query
  against the grammar above before sending, sends all four parameters, and
  checks the answer against what was sent: echoed query, artifact, order by
  bytes, bounds, member shapes, and the consistency of `exists`, `collection`,
  and `next_after`. The client measures a value with `JSON.stringify`, which
  spells a few numbers longer than the stored canonical form (`0.00001`
  against `1e-5`), so it refuses a member or a page only beyond **twice** the
  server's bound. That keeps an unbounded answer out; the exact accounting is
  the server's, and is proved by the Rust tests.
- `followData(client, options)` opens the stream and reconnects. Its parser
  reads **bytes**: a leading byte order mark is removed before anything is
  counted, however the chunks split it; lines end in LF, CRLF, or CR; every
  limit is in bytes (a line and an event's data 144 KiB, which holds a 128 KiB
  value and its envelope); text is decoded with a fatal decoder per line, so
  invalid UTF-8 anywhere, an unfinished last line at end of stream included
  (never dispatched, but checked), is `invalid_response`; `id` and `retry` are
  ignored; an event name other than `snapshot`, `change`, `too_large` is
  `invalid_response`.
- **Event rules**, checked for every event: the artifact and path are those
  requested; `snapshot` first and only once; `change` never first; `too_large`
  first or later; versions are safe integers (above 2^53 - 1 is refused) and
  strictly newer than anything delivered on the connection, and a new
  connection's first event is not older than anything delivered before;
  nothing follows `too_large`; `exists` and `value` agree (the root exists and
  is an object); the data names no revision. Any fault ends following with
  `invalid_response`.
- **Retry table.** Retried: a connection that failed (`network_error`), `429`,
  `502`, `503`, `504`, and a stream that ended cleanly. Not retried, and
  rejected: every other status (`4xx` and `500`), `invalid_response`, and
  `DataTooLargeError` (thrown after the `too_large` event was delivered and
  the server ended the stream; an event that follows `too_large` is
  `invalid_response`). An error thrown
  by the handler rejects too. Aborting resolves, before the first request, while
  waiting, and while reading; nothing is delivered after it.
- **Backoff.** The delay doubles from 500 ms to a maximum (default 30 s). A
  `Retry-After` is used instead, kept between 500 ms and one hour. The attempt
  count goes back to zero only when a connection that was **accepted**
  delivered at least one event and lasted at least `stableMs` (default 10 s)
  from the moment it was accepted; opening a stream never resets it, and a
  stream that is silent or short does not. The clock and the sleep can be
  replaced in tests.

### 8. Not implemented

- Replay, a retained window, a resume cursor, and a per-artifact shared
  buffer or cache.
- A **list**: the members query is a page of one object, not a query language
  (no filter, sort order, or projection).
- Cross-process revocation and wake-ups (above).
- CLI commands, MCP tools, a runtime bridge into frames, web workspace
  screens, a client outbox (R08), and any change to the change feed.
- Network isolation or capability enforcement for previews (0003).

## Contract

Contract 0.15.0: `ListDataMembersQuery`, `DataMemberSelect`, `DataMembers`,
`DataMember`, `DataTooLarge`; `GET .../data/events` reuses `AppData` for
`snapshot` and `change`. Generated by `npm run generate:contracts`.

## Tests

`crates/artifree-domain/src/data.rs` (page rules), `crates/artifree-server/tests/
data_members.rs` (the query), `crates/artifree-server/src/data_stream_tests.rs`
(the stream, with counters and gates instead of sleeps), the two-user route
table of `tests/multi_user.rs`, and `scripts/tests/data-client.test.mjs`
(parser, validators, retry and backoff with an injected clock).

## Alternatives

- **A replayed window with a resume cursor.** It needs a retained history per
  artifact, a position that survives restarts, and a rule for a position that
  was dropped. A fresh snapshot after a reconnect has none of that and costs a
  read.
- **Sending a patch.** A patch is only right against the version it was made
  for; a lost connection then needs the replay. The full value of a bounded
  path is simpler and always right.
- **A per-artifact watcher task.** Shared state per artifact grows with
  artifacts, needs its own lifetime, and is another place where a revocation
  can be missed. Each stream reads for itself, at most four times a second.
- **`EventSource`.** It cannot send the bearer header (0006).

## Costs

A stream under writes costs up to four reads of the whole space (at most 1
MiB) per second; 128 streams bound the total. A trashed artifact ends streams
within the coalescing interval of this process, and of another process within
15 seconds. A reconnect always costs a snapshot. A change sends the whole
value of the path, up to 128 KiB.
