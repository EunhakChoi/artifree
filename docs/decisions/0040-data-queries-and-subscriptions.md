# 0040: Data member queries and a data subscription

Date: 2026-11-04. Status: accepted. Builds on
[0031](0031-app-data.md) (app data), [0006](0006-change-feed.md) and
[0007](0007-live-web-workspace.md) (the change feed it takes
its stream patterns from, written again here), [0020](0020-runtime-grants.md),
[0023](0023-single-owner-token-management.md), and
[0036](0036-multi-user-identity.md) (users, token revocation). Roadmap row:
R07. Independent Artifree design; nothing here observes another product.

## Context

R06 gave an app one JSON object per actor and artifact with its own
`data_version`, readable only by path, whole. An app that keeps a list needs
two things more: to read the members of a collection in bounded pages, and to
learn that a value changed without polling. This record adds both over HTTP
and a TypeScript client. **No frame, CLI, MCP tool, or web screen uses them**
(0031 stays true: a frame has no data access).

## Decisions

1. **Two names for two counters.** `data_version` is the version of the data
   space (0031). A source revision or metadata revision is a different
   counter and never appears in these responses; nothing here reads one.
2. **Query: `GET /v1/artifacts/{id}/data/members`.** Strict query; an unknown
   or repeated parameter is `400`. `path`: 0 to 16 segments of 1 to 64 of
   `[A-Za-z0-9_-]` (the 0031 grammar; the empty path is the root). `select`:
   `names` (default) or `values`. `limit`: 1 to 100 (default 50). `after`: a
   member name of 1 to 256 bytes, exclusive. The response echoes the
   **normalized** query (`artifact_id`, `path`, `select`, `limit`, `after`
   when given) and `data_version`, then `exists` (an object is at the path;
   a missing path, a scalar, or an array reads as `exists: false` and no
   members), `members`, and `next_after` (the last member returned, present
   exactly when more members follow). Members come in the stable order of
   their UTF-8 bytes. Under `values` each member is `{name, value}` or, when
   its canonical value text is over 32 KiB, `{name, too_large: true}` with no
   value: exactly one of the two. Under `names` a member has neither. A page
   holds at most 256 KiB of member names and values; a page stops early,
   with `next_after`, before it would pass that. A stored space that does not
   parse is a typed `500 internal`, not `503`: it is a fault of the stored
   data, not a wait.
3. **Subscription: `GET /v1/artifacts/{id}/data/events`** (server-sent
   events), one watched `path`. The first event is `snapshot`; after it a
   `change` event follows whenever the space's `data_version` is seen to have
   moved, carrying the **new full value** at the path (the `AppData` shape
   of 0031). **Versions are strictly increasing within one connection.**
   Across a reconnect there is no replay: the new connection's `snapshot` is
   authoritative and may carry a version no greater than the last one the old
   connection delivered only if the data was restored from a backup; a client
   replaces what it has. Intermediate versions can be skipped (coalescing,
   item 5). `Last-Event-ID` is refused `400`: there is nothing to resume.
   A value over 128 KiB ends the stream with a typed `too_large` event
   (`artifact_id`, `path`, `data_version`, `limit_bytes`), delivered once, and
   a client must not reconnect for it.

   **One rule for faults.** Before the response is accepted (authentication,
   the token and user re-checks, the first read) a fault is an HTTP status
   with the typed body, exactly as for the members query: a stored space that
   does not parse is `500 internal`, a missing artifact `404`, failing storage
   `503`. After it is accepted the same faults are one terminal `error` event
   whose body is that same `ErrorResponse` (code `internal`, `not_found`, or
   `unavailable`), and the stream ends. A stream never ends on a fault
   without it: a stored value that cannot be written as JSON is an `internal`
   event, a version that went back (a restored database) an `unavailable`
   one, a trashed, purged, or lost artifact a `not_found` one. The only silent
   ends are the ones that are not faults: the 10 minute lifetime, server
   shutdown, and a revocation (the next open answers `401`). A client treats
   both forms alike: a fault that retrying cannot fix (`internal`,
   `not_found`, any 4xx) rejects, and `unavailable` is retried with the
   capped backoff, which keeps growing while connections fail within the
   stability window even when each opens and delivers a snapshot.

4. **Limits.** Per actor 8 open streams (`429 rate_limited`, `Retry-After: 5`),
   per process 128 (`503 unavailable`, `Retry-After: 10`). The registrations
   of token streams (the entries revocation walks) are bounded too: 16 per
   actor and 256 per process, so a request that never reaches the stream
   still cannot grow them. A stream lives 10 minutes, then ends cleanly; the
   client reconnects. The whole open (validate, register, re-check,
   `ensure_current`, first read, taking the place) is bounded by a 10 s open
   timeout that answers `503` with `Retry-After: 10` and releases everything.
   The HTTP layer does not cancel a handler whose client closed the
   connection while the response is pending (measured with a real socket in
   a test), so a reader that leaves during a stalled open keeps its
   registration until that timeout and no longer; the open cannot see the
   reader before the response exists.
5. **Wake-ups and coalescing.** A data write that committed (not a replay or
   a no-op) notifies after the commit through a per-process counter; the
   notification cannot fail and is never awaited, so a commit never fails or
   waits for a subscription. Artifact commits (trash, purge, restore) wake
   streams through the existing change feed's signal. No state is kept per
   artifact or per path, and nothing is replayed. A stream marks the
   notification seen **before** each read, reads at most once per 250 ms, and
   a burst during the wait causes exactly one more read. Every 15 s without
   a wake the stream reads anyway: that read re-checks artifact access and
   catches writes of another process (cross-process wake-ups are not built).
6. **Admission order.** validate; a precheck of the limits that touches no
   storage; register the token stream; check that the token is still valid
   (`token_is_active`, item 7); `ensure_current` (0036); mark the
   notification seen; the first read (also the access check); take the place
   again with the limits re-checked at the bound. A refusal at any step holds
   nothing beyond its request-scoped guard.
7. **Token revocation window.** A token revoked or rotated after the request
   authenticated and before it registered is not seen by the registry walk.
   The open therefore asks the identity store, **after** registering, whether
   the token id with the secret hash it authenticated with is still active and
   its user enabled (`TokenStore::token_is_active`). Either that check sees
   the revocation, or the registry walk, which runs after the store change,
   sees the entry. A failed check is the same `401` as an unknown token. The
   owner credential has no token id and is unaffected. Disabling a user ends
   its streams through the registry as for the change feed.
8. **Every wait is raced.** The producer and the open path race every await
   (reads, sends, `ensure_current`, the heartbeat, the lifetime) against
   revocation, shutdown, the open timeout or lifetime, and the reader leaving,
   signals first. Only a value sent on a signal ends a wait: a signal whose
   sender was dropped says nothing and does not end every wait (the senders
   live as long as the state and the registration). A read runs on the blocking pool and, once started, cannot
   be cancelled: the stream stops waiting for it and drops its place at once,
   but the read itself may still finish in the background (not implemented:
   cancelling it).
9. **Client.** `listDataMembers` and `followData` are standalone modules of
   the TypeScript client. The SSE parser works on bytes: all limits (line,
   event data, pending) count UTF-8 bytes, a BOM is removed at the byte level,
   invalid UTF-8 anywhere fails with `invalid_response`, and an event that the
   body ended before its blank line is never dispatched. Every event is
   validated: artifact, path, a version strictly newer than the last in the
   connection, exact shapes, and every JSON value recursively (finite
   numbers, plain objects, the 32-level nesting rule). Versions above
   2^53 - 1 are refused. The client holds received data to every bound the
   server promises, measured in the UTF-8 bytes of the **canonical JSON
   text** the server defines (no whitespace, strings escaped only for `"`,
   `\\` and controls, everything else verbatim, whole numbers as digits, other
   numbers as Rust prints a double: plain digits down to 1e-4, then `1.5e-7`
   style; member order does not change the length): every member name at any
   depth is 1 to 256 bytes; under `values` a member value is at most 32 KiB
   (larger ones arrive as `too_large`) and the names and values of a page
   together at most 256 KiB; the value of a `snapshot` or `change` is at most
   128 KiB on its own, whatever room the framing leaves. A pinned vector file
   (`scripts/tests/fixtures/data-canonical.tsv`) is checked by a Rust test
   and a client test, so the two counts cannot drift. An exception thrown by
   a callback is not a transport failure: `followData` rejects with that very
   exception, never retries it, and a `too_large` event stays terminal.
   The members query likewise fails with a typed `500` instead of listing a
   member that has neither a value nor the marker. Reconnects use capped doubling; the stability clock
   starts only after an open succeeded and an event arrived; `Retry-After` is
   clamped to 500 ms to 1 h; a `too_large` event is terminal whatever follows.
   Every callback may abort the signal and the follower checks it after each.

## Not implemented

Cross-process revocation and wake-ups (X04); cancelling a blocking read that
outlives its stream; replay or a retained window; multi-path subscriptions;
frame, CLI, MCP, and web access; the client outbox (R08).
