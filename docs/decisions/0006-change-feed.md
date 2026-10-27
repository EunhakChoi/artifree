# 0006: Committed change feed

Date: 2026-10-28. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) and
[0005](0005-request-limits.md); corrects the CLI message of 0005.

## Context

An agent can now save an artifact while the web workspace shows it, but
nothing tells the workspace or another client that a commit happened. A
client that polls may miss nothing only by reading everything again, and a
notification that is sent but not recorded is lost when nobody listens or
the process restarts. The web workspace's automatic refresh is the next
change (roadmap A02); this one provides the server feed and the client
contract it will use.

The requirements below were written for Artifree after the private
behavioral analysis described in [0004](0004-inbound-mcp-adapter.md), with
the same labels: **observed**, **source-only**, **inference**, and
**decision**. A local, unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                                                             | Basis                                                                                                                                                                                                                                                                      |
| --- | ----------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C1  | A committed change is announced with the new revision; clients read the artifact again rather than receive its content. | Observed: after a successful file publication the open view's live connection received an invalidation carrying the new version, followed by a reload of its state. Decision: the event names the artifact, revision, and kind only.                                       |
| C2  | Only a commit that wrote something is announced, once.                                                                  | Observed: stale or conflicting publications created no version. Decision: the change is recorded in the commit's transaction; replays, conflicts, and failed commits record nothing.                                                                                       |
| C3  | Live delivery has keepalives and reconnects; the credential is not part of the URL.                                     | Observed: ping/pong on the live connection. Source-only: the client reconnects with backoff and passes its token outside the URL, which names only the artifact. Decision: server-sent events over `fetch` with the bearer header, comments as keepalives, client backoff. |
| C4  | A client that was away catches up from a position, or learns that it must reload.                                       | Source-only: a catch-up read by sequence position returns rows, the head, and whether more follow. Decision: a durable cursor, resumption with `Last-Event-ID`, and a `reset` event when the cursor cannot be served.                                                      |
| C5  | The feed position is not a source revision.                                                                             | Observed: the state carried a sequence number separate from the version. Decision: `ChangeCursor` is its own type, global to the database.                                                                                                                                 |

The analysis did not establish the original server's queue, retention, or
delivery guarantees; everything below about them is an Artifree decision.

## Decisions

1. **The change is part of the commit.** The store appends one row to a
   `changes` table in the transaction that commits a revision (schema
   version 2; a version 1 database gains the table when opened, and its
   earlier commits have no rows). The row's `AUTOINCREMENT` key is the
   cursor. SQLite serializes writers, so cursors become visible in
   increasing order and a reader can never later see a smaller cursor than
   one it saw; a store for another database must keep that property. A
   replay returns before writing and records nothing; a rolled-back commit
   leaves no row. The store reads a page of an owner's changes and the
   global head in one snapshot.
2. **Transport: server-sent events at `GET /v1/changes`** on the API
   listener. Delivery is one-directional, so a WebSocket adds a dependency
   and a protocol without a use. Browsers' `EventSource` cannot send an
   `Authorization` header, so clients read the stream with `fetch`. MCP is
   stateless request/response (decision 0004) and gets no stream.
3. **Protocol.** Each event's `id` is its cursor and its `data` one JSON
   object from the contract. Without `Last-Event-ID`, a stream starts at the
   head with `ready`; a client subscribes, then reads its state, and every
   later commit follows. With `Last-Event-ID` it continues after that
   cursor with `change` events in order. A cursor beyond the head comes from
   another or a restored database: the server sends `reset` with its head,
   and the client reads all state again. A cursor at or below the head is
   trusted, so a replaced or restored database that has already passed the
   client's cursor is not detected; telling databases apart needs an
   identity in the cursor, which backup and restore (O04) must add before it
   restores a database under running clients. An invalid cursor is
   `400 invalid_request`. A comment line every 15 seconds keeps idle
   connections open.
4. **Wake-up.** A stream rereads storage after this process commits and
   every 15 seconds, so a commit by another process on the same database,
   or a missed wake-up, still arrives within that time. The in-memory
   signal carries no data.
5. **Authorization.** The stream authenticates like every route, with the
   bearer header only; a token in the query string is not accepted. It
   carries changes of artifacts the actor owns. The cursor and head count
   every actor's commits, which reveals how many commits others made; with
   one local owner nobody else exists, and multi-user identity (S01) must
   revisit this.
6. **Limits.** Opening or reopening a stream is a request under the rate
   and concurrency limits of 0005. An open stream then holds no request
   slot but one of the actor's `--change-streams` slots (default 16); one
   more is refused with `429 rate_limited` and `Retry-After: 5`. A stream
   reads at most 100 changes at a time and hands the connection one event
   at a time, so a subscriber that stops reading keeps its slot and
   connection but no growing queue.
7. **Shutdown.** Graceful shutdown waits for open responses, so the server
   ends every open stream when it starts shutting down.
8. **Client.** `ArtifreeClient.openChanges` reads one stream and validates
   each known event against the contract, skipping comments and unknown
   event types. A change kind added after the client was built arrives as
   `unknown`, so an older client still reads the artifact again instead of
   stopping. `followChanges` reconnects from the last cursor after an
   ended or failed connection, waiting `Retry-After` or doubling from 0.5 to
   30 seconds, and rejects on failures a retry cannot fix (`unauthenticated`,
   `invalid_request`, `not_found`, `invalid_response`).
9. **Retention.** Change rows are kept indefinitely, like request keys.
   Pruning belongs to the retention work (roadmap O05), which must answer a
   cursor older than the oldest kept row with `reset`.
10. **CLI correction.** A refused request was described as leaving its key
    "still unused". That is false when an earlier attempt with the key
    committed. The CLI now says that this attempt was not processed and to
    retry with the same key; such a retry replays the earlier result. The
    `rate_limited` contract description says the same.

## Costs and limits

- The web workspace does not use the feed yet (A02).
- Commits by another process on the same database arrive within 15 seconds,
  not at once.
- Events say what to read again; a client that needs content makes another
  request, which counts against its request limits.
- Change rows grow with every commit until retention is implemented.
- One server process holds the wake-up signal and the stream slots in
  memory; several instances sharing state (X04) need another design.
