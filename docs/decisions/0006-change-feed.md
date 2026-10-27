# 0006: Committed change feed

Date: 2026-10-28. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) and
[0005](0005-request-limits.md); corrects the CLI message of 0005.

## Context

An agent can save an artifact while the web workspace shows it, but nothing
tells the workspace or another client that a commit happened. Polling finds
changes only by reading everything again, and a notification that is sent
but not recorded is lost when nobody listens or the process restarts. The
web workspace's automatic refresh is the next change (roadmap A02); this
one provides the server feed and the client contract it uses.

The requirements below were written for Artifree after the private
behavioral analysis described in [0004](0004-inbound-mcp-adapter.md), with
the same labels: **observed**, **source-only**, **inference**, and
**decision**. A local, unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                                                             | Basis                                                                                                                                                                                                                                                                                                                |
| --- | ----------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C1  | A committed change is announced with the new revision; clients read the artifact again rather than receive its content. | Observed: after a successful file publication the open view's live connection received an invalidation carrying the new version, followed by a reload of its state. Decision: the event names the artifact, revision, and kind only.                                                                                 |
| C2  | Only a commit that wrote something is announced, once.                                                                  | Observed: stale or conflicting publications created no version. Decision: the change is recorded in the commit's transaction; replays, conflicts, and failed commits record nothing.                                                                                                                                 |
| C3  | Live delivery has keepalives and reconnects; the credential is not part of the URL.                                     | Observed: ping/pong on the live connection. Source-only: the client reconnects with doubling delays, honors a server-given delay, stops when access is denied, and passes its token outside the URL. Decision: server-sent events over `fetch` with the bearer header, comment keepalives, and the retry rule below. |
| C4  | A client that was away catches up from a position, or learns that it must reload.                                       | Source-only: a catch-up read by sequence position returns rows, the head, and whether more follow. Decision: a durable cursor that names its database, resumption with `Last-Event-ID`, and a `reset` event when the cursor cannot be continued.                                                                     |
| C5  | The feed position is not a source revision.                                                                             | Observed: the state carried a sequence number separate from the version. Decision: `ChangeSequence` is its own type, global to the database.                                                                                                                                                                         |

The analysis did not establish the original server's queue, retention, or
delivery guarantees; everything below about them is an Artifree decision.

## Decisions

1. **The change is part of the commit.** The store appends one row to a
   `changes` table in the transaction that commits a revision (schema
   version 2). The row's `AUTOINCREMENT` key is the feed sequence. SQLite
   serializes writers, so sequences become visible in increasing order: a
   reader never later sees a smaller sequence than one it saw. Another
   store must keep that property. A replay returns before writing and
   records nothing; a rolled-back commit leaves no row. The store reads a
   page of an owner's changes and the feed's head in one snapshot.
2. **The cursor names its database.** A database gets a random 128-bit feed
   identity when it is created, or when a version 1 database is opened and
   gains the feed (its earlier commits have no rows). A cursor is that
   identity and a sequence, `<32 hex digits>.<decimal>`, and clients treat
   it as opaque. A cursor kept from another database is therefore detected
   whatever its sequence, rather than only when it lies beyond the head.
3. **Transport: server-sent events at `GET /v1/changes`** on the API
   listener, using Axum's event stream response. Delivery is
   one-directional, so a WebSocket adds a dependency and a protocol without
   a use. Browsers' `EventSource` cannot send an `Authorization` header, so
   clients read the stream with `fetch`. MCP is stateless request/response
   (decision 0004) and gets no stream.
4. **Protocol.** Each event's `id` is its cursor and its `data` one JSON
   object from the contract. Without `Last-Event-ID`, a stream starts at the
   head with `ready`: a client subscribes, then reads its state, and every
   later commit follows. With `Last-Event-ID` it continues after that cursor
   with `change` events in order. A cursor of another feed, or beyond this
   feed's head, gets `reset` with the head; the client then reads all state
   again. A malformed cursor is `400 invalid_request`. A comment line after
   15 idle seconds keeps the connection open.
5. **Wake-up.** A stream rereads storage after this process commits and
   every 15 seconds, so a commit by another process on the same database,
   or a missed wake-up, arrives within that time. The in-memory signal
   carries no data, and a stream subscribes to it before its first read.
6. **Authorization.** The stream authenticates like every route, with the
   bearer header only; a token in the query string is not accepted. It
   carries changes of artifacts the actor owns. The head counts every
   actor's commits, which reveals how many commits others made; with one
   local owner nobody else exists, and multi-user identity (S01) must
   revisit this together with the owner filter in storage.
7. **Limits.** Opening a stream is a request under the rate and concurrency
   limits of 0005. An open stream then holds no request slot but one of the
   actor's `--change-streams` slots (default 16); one more is refused with
   `429 rate_limited` and `Retry-After: 5`. A stream reads at most 100
   changes at a time and hands the connection one event at a time, so a
   subscriber that stops reading keeps its slot and connection but no
   growing queue. A storage failure while opening is `503 unavailable`;
   after opening it ends the stream, and the client reconnects.
8. **Shutdown.** Graceful shutdown waits for open responses, so the server
   ends every open stream, and every stream opened later, when it starts
   shutting down.
9. **Client parsing.** `ArtifreeClient.openChanges` resolves when the server
   accepted a stream. A successful response that is not `text/event-stream`
   is `invalid_response`. The body is read as the WHATWG HTML standard
   specifies for event streams: UTF-8 without a leading byte order mark;
   CRLF, LF, or CR line ends, also when split across chunks; comment lines
   and unknown fields ignored; one leading space of a value removed; `data`
   lines joined with LF; an event dispatched at a blank line only when it
   has data; and an event the body ends before its blank line discarded.
   `id` and `retry` are not used: the data carries the cursor, and delays
   follow decision 10. Unknown event types are skipped. A known event whose
   data breaks the contract is `invalid_response`. A change kind added after
   the client was built arrives as `unknown`, so an older client still reads
   the artifact again. Aborting cancels the body.
10. **Client retry rule.** `followChanges` reconnects after a stream ends,
    and after a failure when the connection failed, the server answered
    `429`, or the server or a proxy before it answered any `5xx`, whatever
    the body. It waits `Retry-After` when given, also on a proxy's error
    page, otherwise from 0.5 seconds
    doubling to 30 seconds after each failed attempt, and starts again after
    a stream is accepted. Every other failure, including a `4xx` and a
    successful response that is not the feed, rejects: retrying cannot fix
    it, and so does an error thrown by the event handler. Aborting resolves
    at once, also during a wait, and no event is delivered after it, also
    from a chunk that held several. The analyzed client
    adds random jitter to its delays; one local owner gains nothing from it,
    and several instances (X04) should revisit it.
11. **Retention.** Change rows are kept indefinitely, like request keys. The
    head comes from SQLite's record of the largest sequence issued, so
    pruning old rows will not move it back. Pruning belongs to the retention
    work (roadmap O05), which must answer a cursor older than the oldest
    kept row with `reset`. A restore from backup (O04) must give the
    restored database a new feed identity, because a copy keeps the
    original's.
12. **CLI correction.** A refused request was described as leaving its key
    "still unused". That is false when an earlier attempt with the key
    committed. The CLI now says that this attempt was not processed and to
    retry with the same key; such a retry replays the earlier result. The
    `rate_limited` contract description says the same.

## Rewrite after review

The first implementation (closed pull request #12) used integer cursors, a
list of fatal error codes in the client, and a line parser fixed case by
case. Its reviews found an aborted wait that still slept, an unknown change
kind that stopped following, a CRLF split across chunks, a proxy's `5xx`
that stopped following, and a successful non-stream response retried
forever, plus a restored or replaced database that went undetected below
its head. Decisions 2, 9, and 10 replace those parts with a cursor that
names its database, a parser written against the standard, and a retry
rule decided by status.

## Costs and limits

- The web workspace does not use the feed yet (A02).
- Commits by another process on the same database arrive within 15 seconds,
  not at once.
- Events say what to read again; a client that needs content makes another
  request, which counts against its request limits.
- Change rows grow with every commit until retention is implemented.
- One server process holds the wake-up signal and the stream slots in
  memory; several instances sharing state (X04) need another design.
