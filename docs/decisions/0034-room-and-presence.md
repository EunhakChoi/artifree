# 0034: Room and presence

Date: 2026-11-03. Status: accepted. Builds on
[0006](0006-change-feed.md) (the change feed stays the only record of commits),
[0019](0019-runtime-metadata-bridge.md) and [0020](0020-runtime-grants.md) (the
runtime connection the server issues and can close), and
[0032](0032-runtime-source-read-and-download.md) (a separate, per-actor budget
for traffic that is not the owner's). Roadmap row: R12. Independent Artifree
design: the roadmap row asks for presence and ephemeral events that tell a
participant, an actor and a tab apart, a presence that recovers after a
reconnect, an `isMe`/`sameTab` difference that is tested, and no persistent
delivery guarantee. The routes, numbers, identities and errors below are
Artifree's own decisions. Nothing below is an observation of the original
product.

## Context

Several views of one artifact can be open at once: two browser tabs, a page and
the preview frame inside it. A tab that wants to show "someone else is here" or
send a cursor position needs a small shared place for it. That place must not
become a second storage system: what it holds is true only for a moment, and a
lost message is normal.

This record is R12: **a server-side, in-memory, bounded room per artifact, with
join, heartbeat and leave leases, a presence list, and ephemeral events.** It
is not S01 (multi-user identity), R07 (subscriptions), R08 (an outbox) or R10
(view replacement), and it implements none of them.

## Implemented

1. **A tab is a runtime connection.** The server already issues a runtime
   connection (`POST /v1/runtime-sessions`, a 32-hex id, 24 hours, closed by
   `DELETE`, closed when the token that opened it is revoked) and
   authenticates every call that names it. A room's "tab" is that connection:
   the same fact the grants already depend on, no second credential, and
   closing the connection with `DELETE` ends everything it joined (decision 9). Two browser
   tabs open two connections. A page and its frame that share one connection
   are one tab with two participants.
2. **Three identities, all known to the server.**
   - _actor_: the authenticated account, from the bearer token. Rooms are keyed
     by actor and artifact, so another actor's room is a different room, not a
     room it may not read. Since decision 0036 the actor can be the owner or a
     registered user; the room cap and the numbers a room hands out are kept
     per actor, and a room is never shared between actors.
   - _tab_: the runtime connection named in the route, accepted only if the
     server issued it to this actor and has not closed it.
   - _participant_: a number the room hands out (`1` to `2^53-1`), one per
     joined slot of a tab. A client may hold several.
     `is_me` is "this member is the participant whose id and epoch the caller
     proved"; `same_tab` is "this member's tab is the caller's tab". Both are
     computed by the room for every member and every event it returns. No
     request carries either, and no field a peer sends is used to compute
     them. The wire never carries a tab's connection id: members and events
     carry a **tab label**, a number valid inside one room, so two members of
     one room can be compared and a label means nothing elsewhere.

   | viewer                                            | member in viewer's tab           | member in another tab             |
   | ------------------------------------------------- | -------------------------------- | --------------------------------- |
   | its own participant                               | `is_me: true`, `same_tab: true`  | not possible                      |
   | a sibling participant (the frame beside the page) | `is_me: false`, `same_tab: true` |                                   |
   | a participant of another tab                      |                                  | `is_me: false`, `same_tab: false` |

3. **Join, heartbeat, leave.** Four routes, all `POST`, all under
   `/v1/runtime-sessions/{session}/artifacts/{id}/room/participants`:
   - `POST` (join): body `{slot, state?}`; answer a snapshot.
   - `POST .../{participant}/{epoch}/heartbeat`: body `{after, state?}`; answer
     a snapshot. It is also the read: there is no stream.
   - `POST .../{participant}/{epoch}/events`: body `{kind, data}`; answer
     `{seq}`.
   - `POST .../{participant}/{epoch}/leave`: `204`.

   A snapshot is `{participant, epoch, tab, lease_ms, members[], events[],
head, missed}`. A **lease** is 15 s from the last join or heartbeat on a
   monotonic clock. A participant is present while `lease_until > now`; at
   exactly the end of the lease it is gone. The client library heartbeats every
   `lease / 5` (3 s), so four missed beats still keep a participant.

4. **Slot and epoch: reconnect without duplicates.** A join names a `slot`, a
   label of 1 to 32 characters of `[A-Za-z0-9._-]` (for example `page`,
   `frame`). A tab has at most one participant per slot per room. A join in a
   slot the tab already holds **resumes** that participant: same `participant`
   number, same tab label, no second entry, a **new epoch**, and the state
   replaced by the request's (or emptied). The older epoch can no longer act:
   its heartbeat, event and leave answer `404 not_found`, exactly as for a
   participant that never existed. A join after the lease ran out makes a new
   participant. Every protected call is `(tab, participant, epoch)` and all
   three must match what the room holds, so a stale connection and a tab that
   guesses another tab's numbers are refused in one way, and a refusal tells
   nothing about which part was wrong.
5. **Events are ephemeral, with a join floor.** An event is `{kind, data}`:
   `kind` is a label of the same grammar as a slot, `data` is at most 512
   bytes of UTF-8 that the server does not read. The room numbers events
   `1, 2, ...` (`head`) and keeps the newest 128. A participant reads with
   `after = the head it last saw`. Each participant has a **floor**, the room's
   head at its join, and reads from `max(after, floor)`. So:
   - an event sent before a participant joined is **never** delivered to it,
     whatever `after` it asks for, including 0;
   - the room also remembers the newest `after` each participant reported in
     a heartbeat (its **read position**, never below its floor). A join that
     **resumes** a participant returns the events after that position, up to
     the 128 kept, and sets `missed` when the room already dropped some of
     them, so what came while the participant was away is delivered or
     reported, never skipped silently. The position is the participant's own
     claim and only affects what it reads. Events handled by an earlier
     connection of the participant after its last heartbeat can come again;
     readers tell them apart by `seq`. A join that makes a **new**
     participant returns no events;
   - `missed` is true when the room dropped events the caller had not read.
     Nothing replaces them.

   `after` greater than `head` is refused with `400`. There is no
   acknowledgement, no retry that makes an event arrive, and no persistent
   delivery guarantee. When the last participant leaves or expires the room is
   deleted, and a later room starts its numbers again at 1.

6. **Limits, enforced by the server.**

   | what                                 | number                                                        | when exceeded                                                |
   | ------------------------------------ | ------------------------------------------------------------- | ------------------------------------------------------------ |
   | rooms with a participant, all actors | 64                                                            | `429 rate_limited`, join                                     |
   | participants in one room             | 16                                                            | `429`, join                                                  |
   | participants of one tab in one room  | 4                                                             | `429`, join                                                  |
   | lease                                | 15 s                                                          | participant removed                                          |
   | kept events per room                 | 128                                                           | oldest dropped, `missed` set for those who had not read them |
   | events per participant               | 4 per second sustained, burst 8                               | `429`, `Retry-After: 1`                                      |
   | event `data`                         | 512 bytes                                                     | `400`                                                        |
   | presence `state`                     | 256 bytes                                                     | `400`                                                        |
   | `slot`, `kind`                       | 1 to 32 characters of `[A-Za-z0-9._-]`                        | `400`                                                        |
   | request body                         | 8 KiB                                                         | `413`                                                        |
   | room calls per actor                 | burst 30, 900 per minute, 8 at a time (`RequestLimits::ROOM`) | `429`, `Retry-After`                                         |

   The event pace is **per participant**, so a participant at its maximum rate
   cannot take another's allowance. The caps (64 rooms, 16 participants per
   room, 128 events per room, with 256-byte states and 512-byte event data)
   bound the memory of all rooms whatever the traffic; a test fills every limit
   and checks the counts. The room budget is separate
   from the owner's budget and from the frame budget of decision 0032: a loop of
   room calls exhausts only the room budget, and a test shows an owner save
   still being accepted at that moment. Every `Retry-After` is 1 s; the
   budget refills continuously and the value is a floor, not a computed wait.

7. **Order of judgement.** Route shape (`400`), then the tab (`404
runtime_session_expired`), then the live artifact (`404`; a trashed artifact
   is as absent as for every other call), then the body. The body is read by
   the router but judged only after the tab: malformed JSON, an unknown field
   or a body over 8 KiB answer exactly as a valid body does when the
   connection is unknown, expired, closed or another actor's, so a caller
   without a live tab learns nothing from how its body was judged (a test sends
   each of the three to a dead connection and to a live one on every route with
   a body). `leave` needs the tab
   but not a live artifact, so a tab can leave the room of an artifact that was
   trashed. Route numbers are plain: ASCII digits, no sign, no leading zero,
   `1` to `2^53-1`; anything else is `400`.
8. **Errors reuse existing codes.** `not_found` for an unknown, expired,
   stale-epoch, other-tab or other-actor participant; `rate_limited` for a full
   room, a full tab, too many rooms and event pace (no new code, so MCP error
   schemas do not change); `invalid_request`; `payload_too_large`;
   `unavailable` when a counter reaches `2^53-1` or the lock is poisoned. Every
   counter (participant, epoch, tab label, event) stops at `2^53-1` so a number
   always fits a JavaScript integer, and the client parser rejects any other.
9. **No participant outlives a closed connection.** `DELETE
/v1/runtime-sessions/{id}` removes the connection's participants at once. A
   room change runs inside `RuntimeSession::while_active`, so a close that
   happens during a join either comes first (the join is refused) or comes
   after (the close removes what the join added); a test races them. Revoking
   the token that opened a connection closes the connection without removing
   its participants: they can no longer act and disappear when their lease
   ends, within 15 s (see the limits below).
10. **Nothing is persisted.** A room is a `Mutex<Rooms>` in the server process,
    read with a monotonic clock the server owns. No row, no WAL entry, no
    change record, no outbox. A restart empties every room; a test restarts the
    binary on the same data directory and checks that the database holds no
    room data. A restart also empties the runtime-connection store, so every
    call that names an old connection answers `404 runtime_session_expired`;
    `followRoom` treats that as a failure retrying cannot fix and rejects, and
    the app must open a new runtime connection and follow again. Rejoining by
    itself is limited to a participant lost while the connection stays live
    (`not_found`: the lease ran out in a long outage, or another connection of
    the tab replaced it). A connection also ends after 24 hours, with the same
    consequence.
11. **Client.** `packages/client/src/room.ts` (`joinRoom`, `pollRoom`,
    `sendRoomEvent`, `leaveRoom`, `identityOf`, `followRoom`, `parseRoomSnapshot`)
    is a separate module that reuses the client's runtime call path and is not
    imported by the web entry: the main web chunk does not grow. The parser
    cross-checks everything a server sends (counters, flags, a member list that
    holds the viewer exactly once, a viewer that is `is_me` and `same_tab`,
    `same_tab` equal to equality of tab labels, event numbers that increase, come
    after the position asked and do not pass `head`) and rejects an
    inconsistent response as a contract violation. Methods return rejected
    promises; none throws synchronously. `followRoom` joins, heartbeats at
    `lease / 5` from the head it last read, backs off from 0.5 s to 15 s on a
    transient failure (honoring `Retry-After`), joins again at once on
    `not_found`, never retries a failure that is the caller's fault or a handler
    that throws, and leaves once on abort. A participant lost again before any
    heartbeat succeeded (two followers of one slot in one tab replace each
    other's epoch) is joined again only after a wait that doubles from 0.5 s to
    15 s, reported to `onDisconnect`; the first loss still rejoins at once.
    After an abort `followRoom` settles within about 3 s: one deadline starts
    when the abort happens (the listener is installed before anything, such as
    `state()`, can abort the signal) and is shared by everything left to do. A first join
    already in flight is waited for until then and, if it made a participant,
    that participant is left; the leave gives up at the same deadline; past it
    the lease (15 s) ends the participant. The join and heartbeat rely on the
    transport honoring an `AbortSignal`; the leave is also raced against the
    deadline. A wait chosen by `Retry-After` is kept within the retry delays
    (0.5 s to 15 s), so `Retry-After: 0` is not a hot loop and a huge value
    does not stall the follower past its lease.

## Not implemented

- **Multi-user identity and roles (S01, S02).** Rooms hold one account kind. A
  second human is not a participant of another actor; there is no sharing. The
  `actor` field and the actor-keyed rooms are what S01 would extend. This
  record makes no claim that several people can use a room.
- **A frame capability.** A sandboxed frame cannot join a room: there is no
  `room` capability, declaration, consent or SDK method, and the runtime SDK
  still contains types only. A frame that shares its page's connection is a
  second slot only when the page acts for it. This is why `sameTab` is tested
  at the HTTP and client level, not in a browser.
- **A web UI and a CLI command.** No page shows presence and no command lists a
  room. The client library is the only surface.
- **An MCP tool.** None. An agent has no runtime connection and no tab, so it
  has no "same tab" to be told apart from; a room tool would invent an identity
  (an agent as a participant) that S01 has not defined. The tool schemas and
  count are unchanged. If an agent should see who is present, that is a
  design for S01 and R07.
- **A push stream (SSE, WebSocket).** Reading is a heartbeat poll. A stream
  would add a stream-count budget and a reconnect protocol for a room whose
  events are allowed to be lost; the poll reuses the request limiter and keeps
  the change small. Latency is the poll interval.
- **Subscriptions (R07), an outbox or app-data events (R08), view replacement
  (R10), file or asset events.** A room event carries no source change; the
  change feed of decision 0006 remains the only record of commits.
- **Delivery guarantees.** No acknowledgement, no ordering across rooms, no
  event kept for a participant that is not present, no replay after a restart.
- **Persistence and replication.** A room exists in one process. Two server
  processes on one data directory do not share a room.

## Honest limits

- The room budget is per actor, not per tab. A tab that floods room calls can
  use the actor's whole room budget, and that is another tab of the same actor
  that is slowed, not another account's. The per-participant event pace
  and the per-tab and per-room caps bound what one tab can hold.
- Revoking an access token closes the runtime connections it opened. Their
  participants are not removed at that moment: they can no longer act, and stay
  listed to the others until their lease ends, at most 15 s.
- A tab label is comparable only inside one room and one snapshot lineage; it is
  not stable across a room that emptied and was made again.
- A replaced runtime connection (a page reload that opens a new connection) is
  a new tab with a new participant; the old participant disappears when its
  lease ends or its connection is closed. Resuming a participant needs the same
  connection and slot, which is what a reconnect of the same page does.
- `lease_ms` is advice; the server decides presence.
- State and event text are not read, filtered or sanitized by the server. A
  consumer that renders them must treat them as untrusted text from any peer
  that holds a connection of the same actor.

## Tests

Core (`crates/artifree-core/src/room.rs`, an injected `Duration` clock, no
sleeps): the `is_me`/`same_tab` truth table for members and events, exact lease
boundary, heartbeat extension, expired members not listed, resume without a
duplicate, a replaced connection refused, wrong tab and actor separation, no
replay to a late joiner for any `after`, a resumed participant still reading
its gap, cursor ahead refused, `missed`, a full-rate sender not making another
miss, burst then sustained pace, pacer independence, per-room, per-tab and room
count caps, bounded memory under a flood, state replace and keep, leave,
`leave_tab`, counter exhaustion, and `Debug` redaction of a tab key.

Server (`crates/artifree-server/tests/rooms.rs`, a manual `RoomClock`, 19 tests,
and three in-crate tests in `runtime_tests.rs`): the same truth table over HTTP,
reconnect with the stale epoch refused, lease expiry by moving the clock, no
replay, no row in the database and an empty room after a restart, wrong-tab
impersonation, session close, a trashed artifact, a 401/404/405 matrix, no leak
through body judgement, strict route numbers, cursor and size bounds, burst and
sustained pace with `Retry-After`, flood isolation, the three caps and their
release by expiry, sixteen concurrent joins succeeding and the rest refused, a
separate budget that leaves an owner's save unaffected, another actor having no
access, a join racing a close, and token revocation.

Client (`scripts/tests/room-client.test.mjs`, 53 tests with a fake network;
`scripts/tests/room.test.mjs`, 6 tests against the real binary with every
response validated against the published schema): parser acceptance and about
thirty refusals, call shapes, local validation without a request, leave
semantics, and the follower's cadence, backoff, rejoin and abort.

Every guard above was checked by breaking it and watching a named test fail
(recorded in the pull request). These tests do not show multi-user behavior,
that a frame can reach a room, network isolation, or anything about MCP.

## Alternatives considered

- **Tab as a client-chosen id.** Simpler, but then `same_tab` would be a claim
  of the peer. The runtime connection is already server-issued and revocable.
- **One participant per tab.** Cannot tell a page from its own frame, which
  is the case `sameTab` exists for.
- **Reconnect as a new participant plus garbage collection.** Shows a ghost for
  a lease and counts twice against the caps; resume by slot does not.
- **SSE for events.** See above; a candidate for R07.
- **Persisting presence.** A stale row is worse than an absent one, and
  presence has no value after a restart.

## Costs

Reads are polls, so presence changes are seen up to one interval late. All
rooms share one lock, held only for in-memory work of at most 64 rooms by 16
participants; a sweep runs on every call. A reload makes a new tab, and its old
participant stays visible until its lease ends.
