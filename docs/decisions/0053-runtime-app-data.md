# 0053: A runtime frame's access to its application data

> Browser engines: Firefox is suspended in CI until the planned feature set is
> complete. A Firefox acceptance below is met in Chromium and reported as pending
> re-verification ([quality](../quality.md#browser-engines)).

Date: 2026-11-11. Status: **implemented** in Chromium, Firefox pending re-verification: R14a
(`data.read` and `data.get`), R14b (`data.write`, `data.set`, `data.update`
and `data.remove`) and R14c (the change hint and `data.onChange`).
Amends rule 6 of [0040](0040-data-changes-and-member-listing.md) (rule 17).
Rules 1, 11, 14, 17 and 23 were reworded after the implementation, where the
reviews found them open to two readings; the code did not change with them.
Roadmap: R14. Builds on [0020](0020-runtime-grants.md),
[0031](0031-app-data.md), [0033](0033-runtime-source-write.md),
[0040](0040-data-changes-and-member-listing.md),
[0045](0045-uploaded-assets.md) (rule 12, the frame's asset writes),
[0051](0051-roadmap-and-architecture-review.md) (Q3 and Q4) and
[0052](0052-runtime-bridge-consolidation.md).

## Context

An artifact keeps application data: one JSON object per user and artifact with
its own `data_version` (0031). The owner, agents and scripts read and write it
over HTTP and MCP, and the web workspace shows it. The page running in the
preview cannot reach it: the runtime bridge has seven capabilities and none
for data. An agent-made HTML artifact therefore cannot keep its own state,
which is what separates a page from an app.

The frame's asset writes (0045, rule 12) already solved the same problem for
another state: a declared capability with its own consent and grant, a check
order that reveals nothing to a caller without a binding or a grant, a
conditional write on the frame's own base version, a key only a frame can
produce, and a check of the source head inside the write transaction. This
record applies that model to application data and adds nothing to it except
where data differs.

Decided before this record (0051): the calls address the calling user's own
data space, as the HTTP and MCP routes do (Q4); a frame's write is a direct
conditional write and does not use the browser outbox (Q3).

The policies below are independent Artifree design. No observation of another
product is claimed for them.

## Decision

### Capabilities and consent

1. Two capabilities: `data.read` and `data.write`. Each has its own
   declaration in the manifest, its own support, its own consent in the
   workspace and its own connection grant. Neither implies the other, and
   neither implies or is implied by any `metadata`, `source` or `assets`
   capability. `data.write` does not return a stored value, but it is not
   blind: a conditional write tells its caller the current `data_version` on a
   stale base, whether the write changed anything (so a `set` of a guessed
   value reveals whether that value was already stored), and, for a `set` or
   a patch that is not `null`, whether a path runs through a value that is
   not an object. The consent text of rule 3 names the second of these, the
   one that reveals a stored value; a page that must learn nothing about the
   data must not be granted `data.write`.
2. Seven capabilities exist. `data.read` is the eighth and still fits the
   set's current eight bits; `data.write` is the ninth, so the slice that adds
   it (R14b) widens the set to hold at least sixteen. This is internal: the
   wire carries capability names.
3. The consent control for `data.read` says, beside the control, that the page
   can read the application data saved for this artifact and that a preview is
   not isolated from the network, so a page can send what it reads elsewhere.
   The consent control for `data.write` says, beside the control, that changes
   replace saved application data, cannot be undone, can be read by the user's
   agents, and that the page can tell whether something it writes was already
   stored. A grant is not spent by a write: it lasts until it is revoked, the
   view's source changes, or the connection ends. A data write does not move
   the source head, so it does not end the view.

### Calls

4. Four frame calls, each a route below a view
   (`POST {VIEW}/data/read`, `data/set`, `data/update`, `data/delete`):
   - `data.get({ path })` needs `data.read` and answers the path, the
     `data_version`, whether a value exists there, and the value.
   - `data.set({ path, value, baseDataVersion, retryId })`,
     `data.update({ path, patch, baseDataVersion, retryId })` and
     `data.remove({ path, baseDataVersion, retryId })` need `data.write` and
     answer the path, the new `data_version` and whether anything changed.
     Their meaning is that of the owner's routes (0031): `set` replaces a value
     and can store `null`, `update` is a JSON Merge Patch, `remove` of a missing
     path changes nothing, and a write that changes nothing keeps the version.
5. A value and a patch travel as JSON text, in both directions and through
   every layer. The SDK turns a JavaScript value into text with the same
   refusals as the client (a value `JSON.stringify` would change silently is
   refused before anything is sent) and parses the text it receives. The SDK
   bootstrap imports nothing, so it carries its own copy of those refusals;
   one test runs one table of values through the client's function and the
   SDK's and requires the same answer for each. The host passes text through
   and never parses a value: it checks that the value is a string within the
   bound of rule 15 and nothing more, so values are judged by the SDK, the
   client and the server. The server judges the text by the rules of 0031 and
   stores its canonical form.
6. A path follows the data path grammar (0046). The empty path is the root.
7. No member listing, no query and no history for a frame. They are not
   implemented for a frame by this record.

### Server checks

8. A request body that is not the route's shape (not JSON, a missing or
   unknown field, a field of the wrong type) is refused with `400` by the
   shared frame call builder before anything else, as on every frame route
   (0052, rule 11). After that, every call is checked in this order, and a
   later check never runs for a caller an earlier one refuses: the binding
   (`404` when the view, generation or initialization does not match), the
   grant of that one capability (`403`), the artifact's current head against
   the view's revision (`409` `revision_conflict`), then the arguments that
   need judging: the path grammar, the retry identity, the JSON text and its
   limits. A read checks the binding, the grant and the head again before it
   returns the value.
9. A write goes through the data service with the frame's
   `base_data_version` and a key only a frame can produce:
   `RequestKey::runtime_data_write`, with a prefix (`rtd~`) outside the public
   key grammar and the artifact, view, generation and the frame's retry
   identity inside, as `rtw~` and `rta~` are built. An owner or agent key can
   never equal it, and a retry with the same identity returns the first
   receipt.
10. The write transaction checks the view's revision against the artifact's
    head before it looks at the key, so a write from a view that is no longer
    current is refused inside the transaction and a retry after the head moved
    is refused too, not replayed. The grant is checked again on the blocking
    thread immediately before the store is called. A grant that is revoked
    after the commit withholds the receipt; the commit stands.
11. A stale base is `409 data_conflict` with the current `data_version`. The
    limits of 0031 apply unchanged (256 KiB a value, 1 MiB a space, 10,000
    values, 32 levels). The server is what enforces them: the client and the
    SDK refuse before sending only what their serializer refuses (a cycle, a
    value JSON would change, nesting deeper than 64), so a value nested 33 to
    64 deep is sent and refused by the server with the same `invalid_request`.
    The request body limit of a write route, `data/delete` included, is the
    one the frame's source write has. A read answers at most one space, so its value
    text is at most 1 MiB (1,048,576 bytes), in one reply; no parts.
12. A write that changes the space records `data_changed` in the user's change
    feed exactly as an owner's write does (0040). Nothing else is recorded,
    and no schema changes.

### Host, client and SDK

13. The host adds the four calls as descriptors in a `data` family
    (0052, rule 6), the client adds a `runtime-data` module on the shared
    frame transport and four route names (rule 5), and the SDK adds
    `window.artifree.data` with `get`, `set`, `update` and `remove`. Each
    layer checks what it receives: the path it sent and a `data_version` that
    is a safe integer; for a write, exactly the base plus one when `changed`
    is true and exactly the base when it is false, a replay included; for a
    read, a value text within the read bound, which the client and the SDK
    also require to parse as JSON (the host does not parse, rule 5). Writes
    are not retried automatically.
14. Frame failures use the existing closed vocabulary. A stale base is
    `conflict` and carries the current `data_version`. A value the rules of a
    space refuse, a `set` or a patch that is not `null` whose path runs
    through a value that is not an object, and a text that is not JSON are
    `invalid_request`. A `remove`, or a `null` patch, at such a path changes
    nothing and is not a failure, as 0031 defines for a path that is not
    there.
15. The bounds the layers share follow 0052 rule 10: defined once in the
    client, literals in the host and the SDK, pinned by the limits test. They
    are the write value size (262,144 bytes of text), the read value size
    (1,048,576 bytes) and the retry identity.

### Learning of a change (R14c)

16. A frame with `data.read` can be told when its data changes:
    `window.artifree.data.onChange(listener)` registers a listener inside the
    SDK and sends nothing. The workspace already follows the user's change
    feed. When it receives `data_changed` for the artifact of a view whose
    connection holds `data.read`, it sends the frame one message with no call
    id, `{ type: 'event', event: 'data.changed', dataVersion }`, the first
    message the host sends that is not a reply. The SDK hands `dataVersion` to
    its listeners and ignores an event it does not know. `onChange` returns a
    function that removes that listener; registering the same function twice
    registers it twice; a listener that throws does not stop the others, and
    its error is not passed to the host. Listeners are called with one
    argument, `{ dataVersion }`. The frame reads the
    value with `data.get`, which the server checks as always. The message is a
    hint: it can be missed (a reload, a lost connection) or arrive for a
    version the frame already has, and the frame must not treat its absence as
    "nothing changed".
17. The workspace sends the hint only while its own record says the grant is
    held and the view is current, and never to a frame without `data.read`.
    The grant is held when the view the frame connected with was issued with
    `data.read`; a grant change gives a new view. The view is current while
    that frame's connection is open, its lease has not ended, and the change's
    revision is the head the view was issued for: a view whose source moved
    gets no hint. The frame that made a write is told of it like any other.
    It sends a view only versions higher than the last one it sent that view,
    and at most one hint in 250 ms, keeping the highest version that arrived
    meanwhile, so a replayed stretch of the feed is one hint. No new server
    route or stream is added. This amends rule 6 of
    [0040](0040-data-changes-and-member-listing.md) ("the web workspace
    ignores it"): the workspace still does not change its own state for
    `data_changed`, and now also forwards this hint.

### Write rate

23. A frame's writes are bounded apart from its other calls: the host admits
    at most 60 data writes a minute for one view, with a burst of 10, and
    answers `rate_limited` beyond that. A changing write rewrites the space
    row, adds a change row that stays until retention, and wakes the user's
    streams, and unlike a source write it does not end the view, so the
    general frame budget (600 calls a minute) is too wide for it. A data write
    takes from both budgets. The server's frame budget is unchanged.

### What does not change

18. The owner's and agents' data routes, the MCP tools, the CLI, the data
    panel of the workspace, the outbox, the database schema, the CSP, the
    sandbox flags and token handling are unchanged. No value appears in the
    change feed. A frame holds no account token.

### Acceptance

19. Against a real server: both capabilities end to end; the independence of
    the two grants from each other and from every other capability; the check
    order of rule 8 for each route, including a caller with a wrong binding
    and arguments that would be refused (it gets `404`) and a body of the
    wrong shape (`400` whatever the binding); a root read of a full space;
    the write rate of rule 23; a stale base; the same retry identity
    replayed and reused for a different change; a head that moved before the
    write and between the check and the commit; a grant revoked before the
    read returns and after the commit; the limits of 0031 at their boundaries;
    another user's view (`404`); a write recorded in the change feed and a
    no-op that is not. Each guard is proved by mutation.
20. Unit checks of the host, the client and the SDK for what each refuses, and
    a Chromium smoke in which a page stores a value, reloads and reads it
    back, and a second tab is told of the change (R14c).
21. One test compares the layers of the frame bridge with each other: every
    route name of the client is a route the server registers, every call type
    of the host registry has an SDK method that sends it, every event the host
    can send is one the SDK handles, and every supported capability has a
    permission control. 0052's result named this gap.
22. `npm run check`, `npm run build`, the bundle sizes of 0052 rule 12, and no
    growth of the web entry bundle from on-demand code.

### Known limit

Every frame write that is accepted, one that changes nothing included,
records its request key so that a retry can be answered (rule 9), as a
frame's source and asset writes do, and nothing removes those rows; a purge
clears their fingerprints only. At the host's rate a granted page can add about 86,000
rows a day for each open view. The rows are small and hold no value. Removing
old keys is not part of R14; it belongs with a retention rule for request keys
of every kind.

### Slices

| Slice | Scope                                                        | Rules                   |
| ----- | ------------------------------------------------------------ | ----------------------- |
| R14a  | `data.read` and `data.get`, end to end                       | 1, 4 to 8, 11, 13 to 15 |
| R14b  | The wider set, `data.write` and the three writes, end to end | 1 to 5, 8 to 15, 23     |
| R14c  | The change hint and `onChange`                               | 16, 17                  |

The cross-layer test of rule 21 landed with R14a and grew with R14b.

## Alternatives considered

- **Hide `changed` and the current version from a write-only page.** Not
  possible without giving up conditional writes; rule 1 states what such a
  page learns instead.
- **One `data` capability.** Rejected: reading and writing are different
  consents everywhere else in the bridge, and a page that shows saved state
  should not need the right to replace it.
- **Pass JavaScript values through the host.** Rejected: structured clone
  carries values JSON cannot (a `Date`, a `Map`, `undefined`), and the host
  would have to judge them. Text has one meaning in every layer.
- **Let the frame follow the change feed itself.** Rejected: the frame holds
  no token and reaches the server only through the workspace.
- **A server-side push to the frame.** Rejected for now: the workspace already
  receives the change, and a hint with a version needs no new route.
- **Writes through the outbox.** Decided against in 0051 (Q3).
- **New failure codes for data refusals.** Not taken: the vocabulary is
  closed, and a page can tell a stale base from a refused value already.

## Not decided here

Shared data spaces and roles (S02s), member listing and queries for a frame
(T11 may need them), offline writes, and what a page should do when its
change hint is missed for long.
