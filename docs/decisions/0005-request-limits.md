# 0005: Per-actor request limits for HTTP and MCP

Date: 2026-10-27. Status: accepted. Amends
[0004](0004-inbound-mcp-adapter.md) and applies to the HTTP API of
[0003](0003-local-html-artifact-slice.md).

## Context

The MCP specification (2026-07-28, server tools, security considerations)
says servers MUST rate limit tool invocations. The MCP endpoint had
authentication and a per-request size limit, but no limit on how often or how
many requests ran at once, and the SDK provides none. With the token, 500
concurrent tool calls and 300 concurrent creates were all accepted locally.
The realistic risk is an agent stuck in a loop: unbounded commits grow the
database, and many concurrent 36 MiB requests are each buffered in memory.
The HTTP API, which the CLI and web workspace use, had the same gap, and an
agent can switch between the two.

The analyzed hosted product also answered some writes with `429` rate caps
(observed); that supports the behavior, not the numbers chosen here.

## Decisions

1. **One budget per actor for HTTP and MCP together.** A single middleware on
   the API router authenticates every request, including unmatched ones, and
   then admits it under the actor's limits before routing. HTTP handlers and
   MCP tools read the actor that middleware attached; nothing else
   authenticates. Switching interfaces does not reset the budget.
2. **Two limits.** A rate limit (generic cell rate algorithm: `burst`
   requests at once, then `per_minute` per minute, evenly spaced) and a
   concurrency limit (`concurrent` requests of one actor running at once).
   An admitted request runs in its own task that keeps its slot until the
   handler finishes, so a client that disconnects cannot free the slot while
   storage work continues. Defaults: 120, 1200 per minute (20 per second),
   and 16, set with `--request-burst`, `--requests-per-minute`, and
   `--concurrent-requests` (1-100000 each; anything else stops the server
   before it listens). The rate is per minute so that small test limits can
   leave room of whole seconds.
3. **Refusal before any processing.** A refused request gets `429` with the
   contract error `rate_limited` and `Retry-After` in whole seconds, rounded
   up. It never reaches the application, so it commits nothing and its
   idempotency key stays unused: the same request with the same key succeeds
   after the delay with `replayed: false`. Its body is read and dropped
   without being kept, so a client still uploading reads the `429` instead of
   a reset connection. Refused requests hold no permit, so this reading is
   bounded: it stops at the API body limit or after 2 seconds, and when 32
   refused bodies are already being read, a new refusal is sent at once. A
   slow or endless sender therefore still gets its `429`. A refusal cancelled
   while its body is read, for example when its connection is dropped, gives
   up its place among the 32.
4. **Authentication first.** Requests without a valid token are answered
   `401` before the limiter and spend nothing, so they cannot exhaust the
   owner's budget.
5. **MCP framing.** The limit is applied before the MCP SDK, so a refused MCP
   call is an HTTP `429` with the JSON error body, not a tool result.
   Returning a tool error would mean buffering and parsing the very requests
   the limit exists to refuse.
6. **Clients.** `rate_limited` is a contract error code (contract 0.4.0). The
   TypeScript client exposes `retryAfterSeconds`, and its list of known codes
   is now a record typed by the contract, so a new code fails type checking
   instead of becoming `invalid_response`. The CLI says that this attempt was
   not processed and to retry with the same key. (Corrected in
   [0006](0006-change-feed.md): it first said the key was still unused,
   which is false when an earlier attempt with that key committed.)
7. **Fail closed.** If the limiter's lock is poisoned, requests are refused
   with `503 unavailable` rather than admitted.

## Costs and limits

- Limits are per server process and kept in memory; a restart resets them.
  There is one entry per actor that sent a request, which today is the single
  local owner.
- Preview content requests (`/p/<ticket>/...`) are not counted: they carry no
  actor token and are bounded by ticket count and lifetime.
- The defaults suit one person's workspace and agents; the browser smoke and
  test suites run under them. They are not tuned for multi-user servers.
- A refused MCP call surfaces in the agent as a transport error; agents and
  clients should wait for `Retry-After` and resend the same call.
