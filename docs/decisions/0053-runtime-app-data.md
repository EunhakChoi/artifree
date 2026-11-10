# 0053: A runtime frame's access to its application data

> Browser engines: Firefox is suspended in CI until the planned feature set is
> complete. A Firefox acceptance below is met in Chromium and reported as pending
> re-verification ([quality](../quality.md#browser-engines)).

Date: 2026-11-11. Status: **proposed** (specification only; no code).
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
   capability. A page that only writes cannot read what it wrote.
2. The capability set holds nine capabilities with `data.read` and ten with
   `data.write`. Its representation is widened to hold at least sixteen. This
   is internal: the wire carries capability names.
3. The consent control for `data.write` says, beside the control, that changes
   replace saved application data, cannot be undone, and can be read by the
   user's agents. A grant is not spent by a write: it lasts until it is
   revoked, the view's source changes, or the connection ends. A data write
   does not move the source head, so it does not end the view.

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
   refused before anything is sent) and parses the text it receives. The host
   passes text through and never parses a value. The server judges the text
   by the rules of 0031 and stores its canonical form.
6. A path follows the data path grammar (0046). The empty path is the root.
7. No member listing, no query and no history for a frame. They are not
   implemented for a frame by this record.

### Server checks

8. Every call is checked in this order, and a later check never runs for a
   caller an earlier one refuses: the binding (`404` when the view, generation
   or initialization does not match), the grant of that one capability
   (`403`), the artifact's current head against the view's revision (`409`
   `revision_conflict`), then the arguments. A read checks the binding, the
   grant and the head again before it returns the value.
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
    values, 32 levels). The request body limit of a write route is the one the
    frame's source write has.
12. A write that changes the space records `data_changed` in the user's change
    feed exactly as an owner's write does (0040). Nothing else is recorded,
    and no schema changes.

### Host, client and SDK

13. The host adds the four calls as descriptors in a `data` family
    (0052, rule 6), the client adds a `runtime-data` module on the shared
    frame transport and four route names (rule 5), and the SDK adds
    `window.artifree.data` with `get`, `set`, `update` and `remove`. Each
    layer checks what it receives: the path it sent, a `data_version` that is
    a safe integer, a version that is not lower than the base of a write, and
    text that parses as JSON. Writes are not retried automatically.
14. Frame failures use the existing closed vocabulary. A stale base is
    `conflict` and carries the current `data_version`. A value the rules of a
    space refuse, a path through a value that is not an object, and a text
    that is not JSON are `invalid_request`.
15. The bounds the layers share (the value size and the retry identity) follow
    0052 rule 10: defined once in the client, literals in the host and the
    SDK, pinned by the limits test.

### Learning of a change (R14c)

16. A frame with `data.read` can ask to be told when its data changes:
    `window.artifree.data.onChange(listener)`. The workspace already follows
    the user's change feed; when it receives `data_changed` for the artifact
    of a view whose connection holds `data.read`, it sends the frame one
    message with the new `data_version` and nothing else. The frame reads the
    value with `data.get`, which the server checks as always. The message is a
    hint: it can be missed (a reload, a lost connection) or arrive for a
    version the frame already has, and the frame must not treat its absence
    as "nothing changed".
17. The workspace sends the hint only while its own record says the grant is
    held and the view is current, and never to a frame without `data.read`.
    No new server route, stream or timer is added.

### What does not change

18. The owner's and agents' data routes, the MCP tools, the CLI, the data
    panel of the workspace, the outbox, the database schema, the CSP, the
    sandbox flags and token handling are unchanged. No value appears in the
    change feed. A frame holds no account token.

### Acceptance

19. Against a real server: both capabilities end to end; the independence of
    the two grants from each other and from every other capability; the check
    order of rule 8 for each route, including a caller with a wrong binding
    and bad arguments (it gets `404`); a stale base; the same retry identity
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
    of the host registry has an SDK method that sends it, and every supported
    capability has a permission control. 0052's result named this gap.
22. `npm run check`, `npm run build`, the bundle sizes of 0052 rule 12, and no
    growth of the web entry bundle from on-demand code.

### Slices

| Slice | Scope                                                            | Rules                  |
| ----- | ---------------------------------------------------------------- | ---------------------- |
| R14a  | The wider capability set, `data.read` and `data.get`, end to end | 1, 2, 4 to 8, 13 to 15 |
| R14b  | `data.write` and the three writes, end to end                    | 1, 3, 4, 5, 8 to 15    |
| R14c  | The change hint and `onChange`                                   | 16, 17                 |

The cross-layer test of rule 21 lands with R14a and grows with R14b.

## Alternatives considered

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
