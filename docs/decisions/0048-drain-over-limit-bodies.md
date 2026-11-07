# 0048: Drain request bodies refused with 413

Date: 2026-11-07. Status: **accepted** (implemented by roadmap row O07; the
specification was merged first as proposed). Amends rule 4 of
[0004](0004-inbound-mcp-adapter.md) and rule 3 of
[0005](0005-request-limits.md). Roadmap row: O07.

> **Implemented by O07.** The Context describes the state before O07. Rules
> 1-6 describe what the code does now. That the Windows failure recorded in the
> Context is gone is confirmed only by the Windows CI of the merge commit and
> later main runs, not by this record. A numbered rule is amended by a separate
> documentation pull request.

Independent Artifree design: no private evidence was consulted, and nothing
below is presented as an observation of the original product.

## Context

Labels follow [0004](0004-inbound-mcp-adapter.md): observed (seen running),
source-only (read in source code), inferred, and decision.

- **Observed**: a Windows CI run failed once with `fetch failed` in
  `scripts/tests/mcp.test.mjs`, "Escape-heavy text over the request limit is
  refused; the same bytes fit as base64". The client uploads a body over the
  36 MiB request limit and expects HTTP `413`; its fetch failed instead. The
  undici error cause was not logged.
- **Source-only (this repository)**: `/mcp` is the SDK's
  `StreamableHttpService` behind the admission middleware, with
  `with_max_request_body_bytes(MAX_BODY_BYTES)` (`crates/artifree-server/src/mcp.rs`).
  `admit_under` in `crates/artifree-server/src/http.rs` reads and drops a body
  only when the limiter refuses with `429` (`discard`, bounded by
  `MAX_BODY_BYTES`, `MAX_DRAIN_TIME` of 2 seconds and `MAX_DRAINING` of 32;
  0005 rule 3). A `413` is produced later, inside the admitted request's task,
  by whichever layer notices: an extractor (`JsonRejection`), the SDK, or a
  handler's own `payload_too_large` mapping. By then the body has been consumed
  or dropped, and nothing reads the rest.
- **Source-only (dependency source)**: rmcp 3.8.0 reads frames until the next
  would pass its limit, answers `413` and drops the rest of the body; hyper
  1.12 then polls the dropped body once and closes the read side with unread
  data.
- **Inferred**: closing with unread data makes the operating system send a TCP
  reset; on Windows a reset discards data the client has received but not yet
  read, so the client sees a failed fetch instead of the `413`. The test body
  is about 1.2 MiB beyond where the SDK stops reading (from the sizes in the
  test). This is inferred because the client's error cause was not logged.
  Other systems may deliver the status line first; no system guarantees it.

## Decisions

1. **A `413` sent before the body was read to its end is sent after a bounded
   drain.** The trigger is the response: any response with status `413` for a
   request that was admitted under an actor budget and whose body was not read
   to its end, whichever layer produced it (extractor, SDK, a handler's own
   `payload_too_large` mapping, an outer-body overflow on an asset route). It
   gets the treatment 0005 rule 3 gives a `429`: the unread remainder is read
   and dropped, never kept, before the `413` is sent, so a client still
   uploading reads the `413` instead of a reset connection. Examples: the MCP
   endpoint, the owner API (0005), the frame routes
   ([0032](0032-runtime-source-read-and-download.md)), the room routes
   ([0034](0034-room-and-presence.md)) and the asset routes
   ([0045](0045-uploaded-assets.md)). This holds whatever the route's own body
   limit is: a route with a small limit may read up to `MAX_BODY_BYTES` after
   its refusal, within the shared bounds of rule 2. That is deliberate, because
   the same reset hazard exists there.
2. **The bounds are the existing ones, shared with the `429` drains.**
   - Bytes: the drain counts only what it reads itself, starting at the
     refusal, and stops with the first frame that brings that count above
     `MAX_BODY_BYTES`. It reads at most `MAX_BODY_BYTES` plus that last frame
     after the refusal. The bound restarts at the refusal; it is not the
     remainder of the route's limit. The wording "stops at the API body limit"
     in 0005 rule 3 is approximate in the same way.
   - Time: the drain stops after `MAX_DRAIN_TIME` (2 seconds).
   - Count: `MAX_DRAINING` (32) drains run at once, `429` and `413` together
     and across all actors. When 32 are running, the `413` is sent at once
     without reading. This is not a new budget.
   - A drain also ends when the body ends or fails to read. A slow or endless
     sender still gets its `413` and may then be reset; that is documented and
     accepted.
   - A `429` refusal cancelled while its body is read, for example because its
     connection was dropped, gives up its place among the 32 (0005). A `413`
     drain runs in the admitted request's task, which a dropped connection does
     not cancel (0005 rule 2); its place among the 32 and its permit are
     released when the drain ends (body end, a read error such as a dropped
     connection, or the 2-second limit) and never later.
3. **The response does not change.** The status is `413`. The HTTP API keeps
   the contract error `payload_too_large` with its existing message; the MCP
   endpoint keeps the SDK's plain-text body (0004 rule 4). There is no contract
   change, no new configuration and no new error code. How the code reaches the
   remainder, which the `413` path has already consumed, is the code pull
   request's choice, within rules 1-6.
4. **The drain changes nothing that is committed.** An over-limit request is
   refused before any application code runs, so it commits nothing and its
   idempotency key stays unused: the same request with the same key, within the
   limit, succeeds with `replayed: false` (0005 rule 3 states this for `429`).
   Draining only reads bytes that are then discarded.
5. **The request keeps its permit while its body is drained (decision).** The
   `413` arises inside a request that was already admitted, and the drain runs
   there, so the request holds its concurrency slot until the drain ends, and
   the rate limiter has already counted it. A slow over-limit upload therefore
   costs its actor up to `MAX_DRAIN_TIME` of one slot. This differs from a
   `429` drain, whose request never held a permit. Sharing the 32 places
   between `429` and `413` (rule 2) is justified separately: they bound the
   connections and tasks that drains occupy across all actors.
6. **Other refusals keep their present handling of the body.** Responses
   decided before admission (`401`) and the `403` for `Host` or `Origin`
   (0004 rule 4), which the SDK or `enforce_mcp_access` decides after
   authentication and admission with the body unread, are unchanged, as are
   routes that are not admitted under the actor limits.

## Alternatives

- Closing the connection after the response, or sending `Connection: close`,
  is not enough: closing with unread data still sends the reset.
- Reading the whole body regardless of size would hold a connection for as
  long as a hostile sender likes; the shared bounds were chosen for that reason
  in 0005.
- Releasing the permit before draining was rejected: slow over-limit uploaders
  would then be bounded only by the 32 shared places, not by the actor's
  concurrency budget. Keeping the permit costs the actor up to 2 seconds of one
  slot per slow upload, which is accepted.
- Raising the MCP limit so that the escape-heavy test fits: 0004 rule 4
  rejected raising it to the worst-case escaped size (about 150 MiB, buffered
  per request). This record adds its own argument: a higher limit does not
  remove the problem for the next larger body.

## Out of scope

- The SDK's plain-text `413` body stays; mapping it to the API error body is a
  separate change.
- No limit changes: not `MAX_BODY_BYTES`, the per-route limits, the drain time
  or the count of 32.
- A Windows reset after a drain that timed out, hit the byte bound or found 32
  drains running remains possible. This record does not claim it away.
- Refusals that never read the body (rule 6). In particular, a client that
  uploads a large body with a refused `Origin` or `Host` may still see a reset
  instead of the `403`. MCP clients are not browsers and legitimate ones send
  neither header, but this record makes no claim that the residual is harmless.

## Acceptance

The code pull request satisfies all of these. Test names are suggestions. The
pull request does not claim the Windows flake solved from a Linux run: "fixed"
means that the mechanism is closed by these tests; confirmation is the Windows
CI of the merge commit and later main runs.

1. Rust, without TCP: one `413` drain check through each distinct admission
   path (MCP, the owner API with `POST /v1/artifacts`, the frame route behind
   `admit_frame`, the room route behind `admit_room`), each with a body larger
   than that route's own limit. The response is `413` and the counting body is
   read to its end. Each fails when the drain is removed.
2. Rust: an endless over-limit body still gets `413`, and no more than
   `MAX_BODY_BYTES` plus one frame is read after the refusal. It fails when
   the byte bound is removed.
3. Rust: a slow over-limit body gets its `413` after about `MAX_DRAIN_TIME`,
   not later (injected or paused tokio time, as the existing `429` tests do).
   It fails when the timeout is removed.
4. Rust: mixed saturation. With 32 drains running, some `429` and some `413`
   (for example 16 and 16, or 31 and 1 followed by one more of either kind),
   the 33rd of either kind is answered at once without reading. It fails when
   `413` uses a separate counter. The existing cancelled-`429` test keeps
   covering the release of a place.
5. Rust: permit retention (rule 5). With `concurrent` set to 1, while a `413`
   body is stalled in its drain another request of the same actor is refused
   with `429` `rate_limited`, and it is admitted after the drain ends. It fails
   when the permit is dropped before the drain.
6. Rust: a dropped connection. When the client drops during a `413` drain, the
   place among the 32 and the permit are free within the bound (or at once on
   the read error), and a later request is admitted.
7. Node, real server, a raw `node:net` socket on both `/mcp` and
   `/v1/artifacts`, sending `Connection: close`. Both body shapes are used: a
   declared `Content-Length` of the limit plus 4 MiB with the whole body, and a
   chunked sender, so that a refusal after the framework read part of the body
   is covered too. The status line is `413`, the MCP body mentions "too large",
   and the socket ends with `end` and without `error`; the failure message
   includes the socket error `code`. Evidence: run it 20 times on Linux
   without the fix and report how many runs failed. If it never fails on
   Linux, the pull request says so and relies on tests 1-6 for the guarantee
   and on Windows CI after merge. It must pass 20 of 20 with the fix.
8. The existing escape-heavy MCP test stays unchanged and passes.

## Implementation status

| Row | Slice                               | Status      |
| --- | ----------------------------------- | ----------- |
| O07 | rules 1-6 and the acceptance checks | implemented |
