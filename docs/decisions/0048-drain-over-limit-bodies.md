# 0048: Drain request bodies refused with 413

Date: 2026-11-07. Status: **proposed** (specification only; accepted by the
code pull request that implements it). Amends rule 4 of
[0004](0004-inbound-mcp-adapter.md) and rule 3 of
[0005](0005-request-limits.md). Roadmap row: O07.

> **Nothing here is implemented.** On `main`, a request refused with `413`
> leaves the unread rest of its body to the connection, as described in the
> Context. Every numbered rule below describes what the code pull request
> builds. A code pull request may not change a numbered rule; an amendment is a
> separate documentation pull request.

Independent Artifree design: no private evidence was consulted, and nothing
below is presented as an observation of the original product.

## Context

Labels follow [0004](0004-inbound-mcp-adapter.md): observed (seen running),
source-only (read in this repository's code), inferred, and decision.

- **Observed**: a Windows CI run failed once with `fetch failed` in
  `scripts/tests/mcp.test.mjs`, "Escape-heavy text over the request limit is
  refused; the same bytes fit as base64". The client uploads a body over the
  36 MiB request limit and expects HTTP `413`; its fetch failed instead. The
  undici cause was not logged.
- **Source-only**: `/mcp` is the SDK's `StreamableHttpService`, mounted behind
  the admission middleware, with `with_max_request_body_bytes(MAX_BODY_BYTES)`
  (`crates/artifree-server/src/mcp.rs`). The SDK reads frames until the next
  one would pass the limit, answers `413` and drops the rest of the body. The
  HTTP API refuses an oversized JSON body through `JsonRejection`
  (`crates/artifree-server/src/http.rs`) and likewise leaves the remainder
  unread. Rule 3 of 0005 already reads and drops the body of a `429` refusal
  (`discard`, bounded by `MAX_BODY_BYTES`, `MAX_DRAIN_TIME` of 2 seconds and
  `MAX_DRAINING` of 32); the `413` paths never got it.
- **Inferred**: the test body is about 1.2 MiB beyond the point where the SDK
  stops reading (from the sizes in the test). The server then closes the
  connection with unread data, which makes the operating system send a TCP
  reset. On Windows a reset discards data the client has received but not yet
  read, so the client sees a failed fetch instead of the `413`. Other systems
  may deliver the status line first; no system guarantees it.

## Decisions

1. **A `413` for an oversized body is sent after a bounded drain.** A request
   refused with `413` because its body exceeds the request limit gets the same
   treatment 0005 rule 3 gives a `429`: the unread remainder is read and
   dropped, never kept, before the `413` is sent, so a client still uploading
   reads the `413` instead of a reset connection. This applies to the MCP
   endpoint and to every HTTP API route that admits under the actor limits
   (the owner budget of 0005, the frame budget of
   [0032](0032-runtime-source-read-and-download.md) and the room budget of
   [0034](0034-room-and-presence.md)), whatever that route's own body limit is.
2. **The bounds are the existing ones, shared with the `429` drains.**
   - Bytes: the drain counts only what it reads itself, starting at the
     refusal, and stops with the first frame that brings that count above
     `MAX_BODY_BYTES`. It reads at most `MAX_BODY_BYTES` plus that last frame
     after the refusal, on top of whatever the framework had read before
     refusing (at most the route's limit plus one frame). The bound restarts at
     the refusal; it is not the remainder of the route's limit. For a `429`
     nothing was read before the refusal, so the 0005 wording "stops at the API
     body limit" and this one give the same number.
   - Time: the drain stops after `MAX_DRAIN_TIME` (2 seconds).
   - Count: `MAX_DRAINING` (32) drains run at once, `429` and `413` together
     and across all actors. When 32 are running, the `413` is sent at once
     without reading. This is not a new budget.
   - A drain also ends when the body ends or fails to read. A slow or endless
     sender still gets its `413` and may then be reset; that is documented and
     accepted.
   - A refusal cancelled while its body is read, for example because its
     connection was dropped, gives up its place among the 32 as in 0005.
3. **The response does not change.** The status is `413`. The HTTP API keeps
   the contract error `payload_too_large` with its existing message; the MCP
   endpoint keeps the SDK's plain-text body (0004 rule 4). There is no contract
   change, no new configuration and no new error code.
4. **The drain changes nothing that is committed.** An over-limit request is
   refused before any application code runs, so it commits nothing and its
   idempotency key stays unused: the same request with the same key, within the
   limit, succeeds with `replayed: false` (0005 rule 3 states this for `429`).
   Draining only reads bytes that are then discarded.
5. **The request keeps its permit while its body is drained (decision).** The
   `413` arises inside a request that was already admitted, and the drain runs
   there, so the request still holds its concurrency slot, and the rate limiter
   has already counted it. A client that uploads slowly spends its own budget
   for at most `MAX_DRAIN_TIME`. This differs from a `429` drain, whose request
   never held a permit, and it is why the count of rule 2 is shared: the 32
   places bound connections and tasks, the permits bound the actor.
6. **Other refusals are unchanged.** Responses decided before the body is
   admitted (`401`, and the `403` of 0004 rule 4) and routes that are not
   admitted under the actor limits keep their present handling of the body.

## Alternatives

- Closing the connection after the response, or sending `Connection: close`,
  is not enough: closing with unread data still sends the reset.
- Reading the whole body regardless of size would hold a connection for as
  long as a hostile sender likes; the shared bounds were chosen for that reason
  in 0005.
- Raising the MCP limit so that the escape-heavy test fits was rejected in
  0004 and does not remove the problem for the next larger body.

## Out of scope

- The SDK's plain-text `413` body stays; mapping it to the API error body is a
  separate change.
- No limit changes: not `MAX_BODY_BYTES`, the per-route limits, the drain time
  or the count of 32.
- A Windows reset after a drain that timed out, hit the byte bound or found 32
  drains running remains possible. This record does not claim it away.
- Refusals that never read the body (rule 6).

## Acceptance

The code pull request satisfies all of these. Test names are suggestions.

1. Rust, without TCP: a body of `MAX_BODY_BYTES` plus 4 MiB in 64 KiB frames,
   sent to `/mcp` and to `POST /v1/artifacts`, gets `413` and the counting body
   is read to its end. The test fails when the drain is removed.
2. Rust: an endless over-limit body still gets `413`, and no more than
   `MAX_BODY_BYTES` plus one frame is read after the refusal. The test fails
   when the byte bound is removed. The existing test of a refusal cancelled
   while its body is read keeps covering `MAX_DRAINING` and the time bound.
3. Node, real server: a raw `node:net` socket, on both `/mcp` and
   `/v1/artifacts`, sends `Content-Length` of the limit plus 4 MiB, the whole
   body and `Connection: close`. The status line is `413`, the body of the MCP
   answer mentions "too large", and the socket ends with `end` and without
   `error`. The pull request shows that the test fails without the fix, as a
   reset, by running it at least 20 times on Linux with the drain removed and
   reporting how many runs failed, and that it passes in every run with the
   fix.
4. The existing escape-heavy MCP test stays unchanged and passes.

## Implementation status

| Row | Slice                               | Status                    |
| --- | ----------------------------------- | ------------------------- |
| O07 | rules 1-6 and the acceptance checks | proposed, not implemented |
