# 0040: Data changes in the feed and a member listing

Date: 2026-11-04. Status: **proposed**. Builds on [0006](0006-change-feed.md),
[0007](0007-live-web-workspace.md),
[0027](0027-backup-restore-and-schema-upgrade.md), [0031](0031-app-data.md),
and [0036](0036-multi-user-identity.md). Roadmap rows: R07, R07a–R07e.

> **Implementation is tracked per slice** in "Implementation status" at the
> end. This record fixed the design of R07 before any code. The Context
> describes `main` before R07a-1; every other statement is a rule, or what a
> slice builds until its row there says it is implemented. A code pull request
> may not change a numbered rule; an amendment is a separate documentation pull
> request.

Independent Artifree design: no private evidence was consulted, and nothing
below is presented as an observation of the original product.

## Context: what existed before R07a-1

- Before R07a-1, data writes (0031) recorded no change and woke no stream;
  clients poll `GET data` (0031 decision 14). The store's `record_change` puts a row in the
  feed of the artifact's owner (0036 decision 8). Data ownership is checked in
  the core (`ArtifactService::authorized`); the store's data write checks only
  that the artifact is neither in the trash nor purged.
- `changes` (schema 10, then current) admits six kinds by a `CHECK` and refers to
  the revision and metadata revision by foreign keys. Contract: 0.14.0 (0.15.0 with R07a-1). API
  responses carry `Cache-Control: no-store`.
- Before R07a-1, the TypeScript client reports an unknown kind as `unknown`
  and ignores unknown fields; the web workspace then reads its list again (0007 decision
  4). The event-stream parser and the JSON and error body reads have no size
  bound. `parseRetryAfter` takes 1 to 6 digits as seconds (no date, no other
  clamp, so up to 999,999 s); reconnecting waits that instead of its doubling
  delay, without jitter, and resets the doubling as soon as a stream is
  accepted. An aborted request rejects as `network_error` unless the method
  calls `signal.throwIfAborted()`.

## Scope

R07 is (a) notifications of application data changes and (b) a paged listing
of the member names of one object. **Not R07:** filters, sorting, aggregates
(T11); pending writes, acknowledgements, client queues (R08); data access from
frames or previews; MCP tools for listing or following (MCP has no stream,
0006 decision 3); a new streaming endpoint, server timer, or server limit.

## Decisions

### Data changes in the feed (R07a)

1. **Kind and shape.** `ChangeKind` gains `data_changed`. `ArtifactChange`
   gains `data_version`, present exactly for that kind: the version the write
   left the space at. `revision` and `metadata_revision` carry the artifact's
   current positions, never a data version. An event carries no path and no
   value. The contract gets a minor version bump, regenerated.
2. **When.** A set, update, or delete appends one `data_changed` row in its
   own write transaction, after the data row and the request key, only when
   it changed the space. A replay, a write with `changed: false`, a refused
   write, and a rolled-back one record nothing (0006 C2). A duplicate with
   `copy_data` records only `created`, a purge only `purged`, and a restored
   backup already gets a new feed identity (`reset`, 0027).
3. **Whose feed.** The row belongs to the feed of the actor whose space
   changed, passed to the recording function explicitly. Today that actor is
   always the owner (0031 decision 8), so the owner lookup would pick the same
   feed by coincidence; sharing (S02) would break it. A stream still carries
   only its own actor's rows. This amends 0006 decision 6 for this kind.
4. **Wake-up.** A data write that recorded a row wakes this process's streams
   as a commit does (0006 decision 5), over HTTP and MCP alike; other
   processes' writes arrive within the 15-second reread. Every limit and timer
   of 0006 applies unchanged.
5. **Schema.** The version after the current one rebuilds `changes` with
   `data_changed` in the kind `CHECK`, a nullable `data_version`,
   `CHECK ((kind = 'data_changed') = (data_version IS NOT NULL))`, and
   `CHECK (data_version IS NULL OR data_version >= 1)`. Rows are kept by value
   with the global sequence and every actor's head; the step fails, changing
   nothing, if a row would be lost (as 0036 decision 8). Fixtures of every
   earlier version upgrade, and an older backup restores through it (0027).
6. **Consumers.** A client built before R07a reports the kind as `unknown`
   and reads again: wasteful, not wrong. In R07a the client's record of known
   kinds gains `data_changed` (its type requires every contract kind), and the
   web workspace ignores it, never as unknown or as a new revision; this
   amends 0007 decision 4. The amended records (0006, 0007, 0031) already
   carry notes marked "proposed; takes effect when the slice merges".

### Member listing (R07b)

7. **Route.** `GET /v1/artifacts/{id}/data/members`, with the authentication,
   agent-token, rate-limit, `404`, and check-order rules of `GET data` (0031
   decisions 8 and 9). No MCP tool.
8. **Query**, strict as for `GET data` (an unknown or repeated parameter is
   `400 invalid_request`). `path`: optional, the root when absent or empty, a
   breach of 0031 decision 2 is `422 invalid_request`. `limit`: 1 to 100,
   default 50. `page_token`: an opaque `next_page_token` (rule 10).
   `at_data_version`: optional; a space at another version, older or newer,
   answers `409 data_conflict` with `current_data_version`, and `0` matches a
   space never written. Numbers are digits without sign or leading zero, at
   most 2^53 - 1; any other `limit` or `at_data_version` is `400`.
9. **Response** `DataMembers`, from one read of the space: `artifact_id`,
   `path` (canonical), `data_version` (of that read), and:
   - `exists`: false when nothing is at the path or the path passes through a
     value that is not an object, as `GET data` reads it; `names` is then
     empty and `next_page_token` absent. The root always exists. A value at
     the path that is not an object (`null` and arrays included) is
     `409 data_path_conflict`.
   - `names`: member names after the page token's name, ascending by UTF-8
     bytes, at most `limit`. Only names matching the segment grammar of 0031
     decision 2 are listed; others (valid up to 256 bytes, 0031 decision 3)
     are skipped, do not count toward `limit`, and do not make
     `next_page_token` present (`GET data` returns them). Values, sizes, and
     types are not listed.
   - `next_page_token`: present exactly when more listable names follow at
     `data_version`. `data_path_conflict`'s contract description gains this
     read case.
   - Checks: identifier (`404`), query (`400`), path (`422`), token (`400`),
     access (`404`), then `at_data_version` and the value at the path (`409`).
10. **Page token.** Opaque to clients, issued by the server: base64url
    without padding of a 16-byte tag followed by the last listed name in
    clear (1 to 64 bytes, so 17 to 80 bytes, 23 to 107 characters). The tag
    is the first 16 bytes of SHA-256 over the artifact identifier, a zero
    byte, and the canonical path. A token that does not decode this way (a
    padded or longer one included), whose name breaks the segment grammar,
    or whose tag does not match the request's artifact and path is
    `400 invalid_request`; the check reads no storage. The token names a
    position, not a permission: every page checks access. A page lists names
    greater than the token's name, so a removed name does not break paging.
11. **Consistency.** A page agrees with its own `data_version`; pages agree
    with each other only when each sends the same `at_data_version`. Without
    it, a name present at every page's version is listed exactly once; names
    added or removed meanwhile may or may not be. When R06b widens the path
    grammar, the listing follows it, and the token layout of rule 10 and the
    bound of rule 19 are recomputed.

### TypeScript client (R07c)

12. **Feed changes.** `FeedChange` carries `data_version` for `data_changed`;
    one without a safe integer of at least 1 there is `invalid_response`.
13. **`followData`** (artifact, optional path, `signal`, `onData`) is a
    client routine; the server keeps no follower state. It opens
    `GET /v1/changes` without `Last-Event-ID` and, after `ready`, reads
    `GET data` at the path: the snapshot. `onData` receives `snapshot` or
    `update` with the `AppData` read, or `unavailable`. Like `followChanges`'s
    `onEvent`, it is synchronous: it is called once per delivery, in order,
    and the follower does not await what it returns. The follower keeps
    `seen` (the version last delivered), `named` (the highest version any
    `data_changed` of the artifact named since this connection's last `ready`
    or `reset`, where it becomes 0; every such event is recorded in S, F, and
    R), and `asked` (`named` when the read in flight was sent). A read
    answering version `v`:
    - `v < asked` is `invalid_response`: a read sent after an event cannot
      miss its change.
    - The snapshot read, or `v < seen` (only a replaced database does this),
      delivers `snapshot` and starts an epoch; `v > seen` delivers `update`;
      `v = seen` delivers nothing. `seen` becomes `v`.
    - If `named > v`, one more read is sent; otherwise the follower waits.
14. **Ending.** A `404` on any read (the snapshot included), or a `deleted` or
    `purged` change of the artifact after `ready`, ends the follower: the read
    in flight is aborted and its answer discarded, the stream closed, one
    `unavailable` delivered, and the promise resolves (unless `onData` throws,
    rule 15). It never waits for a recovery (an application starts a new
    follower), which removes the races of trash, recovery, and purge against a
    read. Like the server's `404`, `unavailable` does not tell missing,
    trashed, purged, or foreign apart. Other changes of the artifact
    (`created`, `revised`, `metadata_updated`, `recovered`, `unknown`) and of
    other artifacts trigger nothing.
15. **States.** Connecting (C, including the wait before reconnecting),
    snapshotting (S), following (F), reading (R), and ended, which ignores
    everything. The follower starts in C by opening the stream. _Abandon_
    aborts the read in flight; answers of abandoned reads are discarded in
    every state. _Retry_ closes the stream, waits (rule 17), and opens one
    (C). _End_ is rule 14. _Reject_ closes everything and rejects. _Record_
    sets `named` to the larger value: the buffer of events that arrive during
    a read, applied when it answers. A change of any kind before `ready`
    breaks 0006 decision 4 and rejects. "—" cannot occur: only C has a
    pending stream request, only S and R a read, and the reset timer of rule
    17 starts with the connection's first read answered.

    | Event                                                   | C       | S                | F                              | R                |
    | ------------------------------------------------------- | ------- | ---------------- | ------------------------------ | ---------------- |
    | stream accepted                                         | wait    | —                | —                              | —                |
    | `ready` or `reset`                                      | read; S | abandon; read; S | read; S                        | abandon; read; S |
    | `data_changed` of the artifact, `n`                     | reject  | record           | record; if `n > seen`: read; R | record           |
    | `deleted` or `purged` of the artifact                   | reject  | end              | end                            | end              |
    | any other change                                        | reject  | nothing          | nothing                        | nothing          |
    | read answers `200`                                      | —       | rule 13; F or R  | —                              | rule 13; F or R  |
    | read answers `404`                                      | —       | end              | —                              | end              |
    | stream fails, ends, or stalls (rule 18)                 | retry   | abandon; retry   | retry                          | abandon; retry   |
    | read fails: network, `429`, `5xx`, body stall (rule 18) | —       | abandon; retry   | —                              | abandon; retry   |
    | other `4xx`, `invalid_response`                         | reject  | reject           | reject                         | reject           |
    | open `max` after its start (rule 17)                    | —       | reset `n`        | reset `n`                      | reset `n`        |
    | `signal` aborts                                         | resolve | resolve          | resolve                        | resolve          |
    | `onData` throws                                         | —       | reject           | reject                         | reject           |

    An abort wins over everything, an error `onData` throws included.

16. **Guarantees**, about which reads happen, not how soon:
    - a. A change of the space committed while the follower runs is not dropped.
      Until `seen` covers it, the follower, unless it ends, rejects, or is
      aborted, is in one of these situations, each of which leads to a read sent
      after the commit once the requests before it are answered: such a read is
      in flight; the change's event is still to arrive on the open stream, or
      was recorded while an earlier read is in flight (rule 13 then reads
      again); or it is in C, whose snapshot is read after the next `ready`. A
      read sent after the commit that answers `200` observes it. Timeliness is
      not promised: failures retry with backoff (rule 17), and a request that
      gets no headers is not timed (rule 18); the application's signal bounds
      it.
    - b. Within an epoch, delivered versions strictly increase.
    - c. A delivery is the state one read returned; intermediate versions may
      never be delivered. An update follows any increase of the space's
      version, also when the value at the path is unchanged.
    - d. At most one read is in flight. Events received during one read cause
      at most one further read, none when its version is at least every
      version they named; several events are not always one read.
    - e. A read sent after an event named `n` that answers `200` with a
      version below `n` makes the follower reject with `invalid_response`.
17. **Retry** (amends 0006 decision 10, for `followChanges` and `followData`;
    what is retried is today's set, and rule 15 for reads). Attempt `n` counts
    retries since its last reset. `base = min(max, min × 2^n)`, defaults `min`
    0.5 s and `max` 30 s; `delay = base × (0.5 + 0.5u)`, `u` in [0, 1) from an
    injectable random source, so the delay is in [base/2, base). A
    `Retry-After`, parsed as today, is a floor clamped to one hour:
    `max(min(Retry-After, 3600 s), delay)`. Today it is not clamped (up to
    999,999 s) and replaces the delay. **`n` resets to 0 only when a connection
    stays open for `max` after its start**: the stream's acceptance for
    `followChanges`, the connection's first read answered `200` for
    `followData`; the timer stops when the stream ends, fails, or stalls. A
    connection dropped sooner never resets `n`, so one accepted and dropped
    every second backs off to `max`. (Today `followChanges` resets on acceptance
    alone.) A stall (rule 18) needs 45 s of silence, so stalls cause at most one
    reconnect per 45 s even when `n` was reset. A clean end of a stream carries
    no reason and is retried; a revoked token answers the next open `401`, which
    rejects. Revocation across processes stays X04.
18. **Waits and stalls.** Once a response's headers have arrived, 45 seconds
    without a byte of its body (after the headers or the previous byte) is a
    stall: the body is cancelled and the request fails as a network failure.
    Stream comments count; the server sends one every 15 idle seconds. **No
    timer runs on a request before its headers**: the page's request gate and
    the browser may hold a request without failing it (0007 decisions 1 and
    2), so such a request waits until it is answered, fails, or the caller
    aborts. Every wait of R07c and its bound:

    | Wait                                       | Bound                                                      |
    | ------------------------------------------ | ---------------------------------------------------------- |
    | stream, `GET data`, or listing: no headers | none; the caller's `signal`                                |
    | stream body, after headers                 | 45 s between bytes; then a drop (retry), in both functions |
    | `GET data` body, after headers             | 45 s between bytes; then a failed read (abandon; retry)    |
    | listing body, after headers                | 45 s between bytes; then `listDataMembers` rejects         |
    | retry wait (rule 17)                       | at most 3,600 s; an abort ends it at once                  |
    | `onData` (rule 13)                         | not awaited; a synchronous call the application owns       |

    A body that trickles a byte at least every 45 s is bounded only by its
    size (rule 19) and the caller's `signal`.

19. **Transport bounds**, checked as data arrives; the body is cancelled at
    once.
    - Event streams (the parser both functions share): 65,536 UTF-16 code
      units for the unfinished line, and for one event's data and type
      together; past either, `invalid_response`. A change event today is
      under 400.
    - Success bodies of reads by `followData` and `listDataMembers`: 1,064,960
      bytes (1 MiB + 16 KiB); past it, `invalid_response`. A `GET data` body is
      a space of at most 1,048,576 bytes and under 1,200 more. A page is at
      most 7,991 bytes: 100 unescaped names of up to 64 bytes (6,699 quoted
      with commas), a quoted path up to 1,041, a token of 109, and the rest.
    - Error bodies of their requests, stream opens included: 65,536 bytes.
      Past it, the response counts as one without an API body, and its status
      decides, as for a proxy's page today.
    - Other client calls keep their current reads.
20. **Abort and errors.** An aborted `listDataMembers` rejects with
    `signal.reason` (an `AbortError` by default), as `throwIfAborted()` does.

    | Situation                                 | `followData`                        | `listDataMembers`       |
    | ----------------------------------------- | ----------------------------------- | ----------------------- |
    | aborted before the call                   | resolves; sends nothing             | rejects; sends nothing  |
    | abort before headers, in a body or stream | cancels; resolves; delivers no more | cancels; rejects        |
    | abort while waiting to reconnect          | resolves at once                    | never waits             |
    | no headers, however long                  | waits; no timer (rule 18)           | waits; no timer         |
    | 45 s without a body byte after headers    | retry (stream or read)              | rejects `network_error` |
    | `onData` aborts                           | delivers no more; resolves          | —                       |
    | `onData` throws                           | rejects with it; never retried      | —                       |
    | `onData` aborts and throws                | resolves (abort wins)               | —                       |
    | error body over its bound                 | its status decides                  | rejects; status decides |
    | network failure, `429`, `5xx`             | retry, then a new snapshot          | rejects; no retry       |
    | `404`                                     | read: end; stream open: rejects     | rejects                 |
    | other `4xx`; body breaking rules 19, 22   | rejects                             | rejects                 |

21. **`listDataMembers`** takes an artifact and optional `path`, `limit`,
    `pageToken`, `atDataVersion`, and `signal`, and reads one page. A caller
    pages by passing `next_page_token` as `pageToken` until it is absent.
22. **Client trust rules** (copied to [quality](../quality.md)): (1) the
    server owns the data rules; the client does not re-check storability
    (surrogates, duplicates, value count, nesting, name length, canonical
    size, number spelling) and never computes canonical sizes. (2) Shape, not
    meaning: fields have their TypeScript type, integers are safe, enums are
    closed with an `unknown` fallback where the contract may grow. (3) The
    client checks what its own state depends on: the echoed `artifact_id`
    equals the one asked; rules 13 and 16e; known event types;
    `next_page_token`, if present, is a non-empty string; the failure's class
    decides retry or stop (rules 14 and 17). (4) Memory bounds are transport
    bounds (rule 19). (5) A violation of 2 to 4 is `invalid_response`, decided
    once per message and never retried. (6) Wire shapes come from the
    generated contract; no rule lives only in TypeScript. (7) A review finding
    asking for a check outside 1 to 6 is a follow-up, not a blocker.

### Web data panel (R07e)

23. A read-only panel shows the open artifact's root data and `data_version`,
    kept current by `followData` without polling, only while shown and the
    page visible (0007 W6). It stops when another artifact opens, shows a
    notice on `unavailable`, and reads through the page's request gate (0007).
    Frames and previews get no data.

## Slices and acceptance checks

Each code pull request aims at under 3,000 lines without generated files and
may be split further. A code pull request only marks the amendment notes in
0006, 0007, and 0031 effective and corrects sentences its change makes false;
it changes no numbered rule of this record. Each check names the wrong
implementation it fails.

- **R07a** store, feed, contract, schema, web reducer (rules 1–6).
  - Each changing set, update, and delete adds one row with the new version
    and the current revisions (fails a data version in `revision`); a replay,
    no-op, stale base, path conflict, and oversized value add none, and a
    trigger failing the `changes` insert fails the whole write with space and
    key unchanged (fails recording early or in its own transaction).
  - In the store, which does not check ownership, a write as actor B on A's
    artifact adds the row to B's feed and leaves A's head (fails the owner
    lookup). `copy_data` adds only `created`; a purge only `purged`.
  - A version-10 fixture with every kind upgrades with rows equal by value and
    heads unchanged, an older cursor resumes without `reset`, each `CHECK`
    refuses its inconsistent row, and an older backup restores.
  - HTTP and MCP writes each reach a stream within the feed tests' 5-second
    wait (fails a missing wake-up: the reread takes 15 s). Two users: A's
    write reaches only A's stream; B's write on A's artifact is `404` and
    adds nothing.
  - Other kinds serialize without `data_version`. The web workspace reads
    nothing for `data_changed` of an open or unlisted artifact (fails
    treating it as `unknown` or as a revision).
- **R07b** listing, server and contract (rules 7–11).
  - Order `-y, 0, B, _x, a` (fails case folding or locale order). Seven names
    at limit 3 page 3, 3, 1; six give no `next_page_token` on the second page
    (fails a token for a full page). Names `a`, `a b`, `b`, `é`: limit 2
    gives `a, b` without a token, limit 1 `a` with one then `b` without
    (fails counting or paging past skipped names).
  - `exists: false` for a missing path and one through a scalar or array;
    `409 data_path_conflict` for a scalar, `null`, or array at the path.
  - `at_data_version` equal, older, newer, and `0` unwritten; the earlier
    page token with the earlier version after a write is `409`.
  - A core test whose fake store answers each read with a newer space gets a
    page whose names and version come from the same answer (fails two reads).
  - Refused queries (`limit` 0, 101, `05`, `+5`; unknown; repeated); 60 names
    without `limit` give 50 and a token. Undecodable, padded, 108-character,
    bad-name, foreign-path, and foreign-artifact tokens are `400`; the deepest
    path with a 64-byte name gives a 107-character token that decodes to the
    tag and the name (fails a digest of the name or an unbounded layout).
  - The route joins the 0036 route table (foreign, trashed, purged answer as
    unknown; an agent token works); an unclassified route fails that test.
- **R07c** client (rules 12–22), with a fake server, timers, and random.
  - Every cell of rule 15, each failing the opposite behavior (a read per
    event, events dropped during the snapshot). `v < asked` rejects, while
    the same `v` from a read sent before the event is accepted and followed
    by one more read. In F, an event naming `n <= seen` reads nothing, and a
    later read answering below `n` rejects (fails not recording in F).
  - Delays with `u` = 0 and near 1, the 30-second cap, and `Retry-After`
    above and below the delay (fails no jitter, or `Retry-After` replacing
    it); `Retry-After: 999999` waits 3,600 s (fails an unclamped floor). 45 s
    of silence reconnects; a comment every 15 s prevents it.
  - A `GET data` whose headers arrived and whose body then sends nothing for
    45 s (fake clock) is abandoned and followed by a new connection and
    snapshot, while the stream keeps its comments (fails a stall timer on the
    stream only); a listing body silent 45 s rejects `network_error`. A
    stream open, a `GET data`, and a listing that get no headers for an hour
    of fake time are still pending, cancelled by no timer, and abort then
    ends them (fails any timer before headers).
  - An `onData` that returns a promise that never settles does not delay the
    next read or delivery (fails awaiting `onData`).
  - A stream accepted and dropped every second, whose reads succeed, backs
    off to the 30-second cap in both functions (fails resetting `n` on
    acceptance or on a `200`); one open 30 s resets it.
  - An endless line, a 1,064,961-byte success body, and a 65,537-byte error
    body stop at the bound with the body cancelled; the bounds themselves
    pass (fails reading whole and checking afterwards).
  - Every row of rule 20, abort racing an error body included; a wrong echoed
    `artifact_id`, an empty `next_page_token`, and unsafe integers are
    refused.
- **R07d** guides: README and `docs/mcp.md` describe following and listing
  (MCP still polls). Each code slice corrects the sentences it makes false.
- **R07e** panel (rule 23), after R07c, may split into panel and live updates;
  needs the shadcn skill and Chromium and Firefox smoke checks: an agent's
  write appears without reload, no `GET data` is sent while nothing changes
  for 20 s, longer than the 15-second reread of 0006 (fails polling), and
  trashing shows the notice.

## Implementation status

One row per slice, updated by the pull request that implements it. R07a of the
slice list above was planned as two pull requests; #87 carries both slices.

| Slice  | What                                            | Status          | Pull request |
| ------ | ----------------------------------------------- | --------------- | ------------ |
| R07a-1 | Rust: store, feed, contract, schema (rules 1-5) | implemented     | #87          |
| R07a-2 | TypeScript kind record and web reducer (rule 6) | implemented     | #87          |
| R07b   | member listing (rules 7-11)                     | implemented     | #86          |
| R07c-1 | client transport bounds and retry (17-19)       | implemented     | #89          |
| R07c-2 | `listDataMembers` (rules 20, 21, 22)            | not implemented | -            |
| R07c-3 | `followData` (rules 13-16, 20)                  | not implemented | -            |
| R07d   | user guides                                     | not implemented | -            |
| R07e   | web data panel (rule 23)                        | not implemented | -            |

R07a-1 and R07a-2 both landed in #87. The client's record of known kinds gained
`data_changed`, which its type requires, and the web workspace's reducer ignores
it: it changes nothing the page knows and reads nothing, for an open artifact, an
unlisted one, and while the first read is pending (rule 6; 0007 decision 4).
#87 also implements rule 12 in the client: `FeedChange` carries `data_version`
for `data_changed` only, and an event that breaks that is `invalid_response`.
The published schema and Rust deserialization state the same rule. What R07
excludes is listed under Scope.

R07c is split into three pull requests. R07c-1 builds the transport that
`followChanges` uses now and the other two will use: rule 17 (jitter, a
`Retry-After` floor clamped to one hour, the count restarted only after a
stream stayed open) for `followChanges`; rule 18 (a stall after 45 seconds
without a byte once the headers arrived, no timer before them) for the change
stream and for the error body of its open; and rule 19 for the event-stream
parser (65,536 UTF-16 code units for a line and for one event's type and data
joined with LF) and the error body of the stream open (65,536 bytes, past it the
status decides). A bounded read of a JSON success body (1,064,960 bytes) exists
in the client, with the same stall limit, but no call uses it yet; every other
client call keeps its current read. The count restarts after 30 seconds, a
constant, which is also the default `max`: a smaller configured `max` does not
shorten it. Timers and the jitter come from the client's optional `clock` and
`random`. The rest of rules 13-16 and 20-22 (`followData`, `listDataMembers`,
the unified abort table) is not implemented; the rows above say which slice
carries it.

R07b landed in #86 (contract 0.16.0): `GET /v1/artifacts/{id}/data/members`
with the query, response, check order, page token, and consistency of rules
7-11, over HTTP only (no MCP tool, no client method). Where rule 9 lists
`at_data_version` and the value at the path together, a stale version is
reported first, then a value that is not an object. The listing's worst-case
page measured 7,987 bytes against the 7,991 of rule 19.

## Alternatives

A data stream of its own adds an endpoint, a limit, and a timer for what the
feed already orders and authorizes per actor. Values in events are large and
stale behind a read; paths in events cannot spare a read, because the version
is per space (0031 decision 1). A follower that waits through the trash needs
a "gone, maybe back" state that races recovery and purge; ending is simpler.

## Costs

A schema step and a contract change. A row per changing data write, kept until
retention (O05), at the pace the limits of 0005 allow. A follower holds a
stream and reads after every change of the space, other paths included; the
panel adds a second stream per page while shown, against the actor's 16 and
the browser's six or so connections. A request that gets no headers holds the
follower or the listing until the caller aborts or the stream drops (rule 18);
a body that trickles bytes is bounded only by its size. A kind added later
that alters data would not wake older followers, so such a change must also
record `data_changed`. Names outside the path grammar are not listed until
R06b.
