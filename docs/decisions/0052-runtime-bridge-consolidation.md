# 0052: Consolidating the runtime bridge without changing its behavior

> Browser engines: Firefox is suspended in CI until the planned feature set is
> complete. A Firefox acceptance below is met in Chromium and reported as pending
> re-verification ([quality](../quality.md#browser-engines)).

Date: 2026-11-10. Status: **partially implemented**: R13a (rule 5) is
implemented; R13b to R13e (rules 6 to 11) are proposed and have no code.
Roadmap: R13. Builds on [0019](0019-runtime-metadata-bridge.md),
[0020](0020-runtime-grants.md),
[0032](0032-runtime-source-read-and-download.md),
[0033](0033-runtime-source-write.md), [0045](0045-uploaded-assets.md) and
[0051](0051-roadmap-and-architecture-review.md).

> Amended 2026-11-10 (documentation pull request): rule 5 puts the byte
> comparison in its own on-demand module, and rule 12 records the client's
> on-demand chunk and guards that module's importers.

## Context

The runtime bridge carries a call from a sandboxed frame to the store through
four places: the frame's SDK, the workspace page that hosts the frame, the
TypeScript client, and the server. Seven capabilities, ten frame calls and
twelve frame routes exist. They were added one slice at a time, each copying
the shape of the one before, and the review in 0051 found that one more
capability touches about twelve hand-written files in five packages. The next
rows add more (R14, R15, then the P rows), so the repetition is removed first.

What repeats today, read from the code on `main`:

- **Client.** `runtime-source.ts` defines `FrameTransport`; `runtime-assets.ts`
  defines `RuntimeAssetsFrame` with the same four members. Each file has its
  own `frameRoute` and `frameBody` and its own comparison of received bytes
  with a stated SHA-256, and `client.ts` builds the metadata route and body a
  third time.
- **Host.** `frame-rpc.ts` names every call type three times: in the `SHAPES`
  table of allowed keys, in the `parseCall` switch, and in the `#serve`
  switch. `#download` and `#assetDownload` are the same sequence (offer, ask
  the user, fetch, hand to the browser, receipt) written twice.
  `download-prompt.tsx` branches on the offer's kind in five places. The
  request gate spells every frame route tail in one regular expression, apart
  from the client functions that build those routes.
- **Limits.** The retry id pattern, the asset id pattern and the part, write
  and upload sizes are written out in the host, in the SDK bootstrap and, in
  part, in the client. Nothing compares them with each other or with the
  bounds the server enforces.
- **Server.** Eleven of the twelve frame handlers in `http.rs` rebuild the
  same `FrameCall` from the path and the body's `initialization`; the metadata
  handler passes the same values to its operation one by one. The checks
  themselves (`authorize`, `HeldCall`, `still_permitted`) are already shared
  between `runtime_source.rs` and `runtime_assets.rs`.

The three independent checks are deliberate and stay: the client, the host and
the frame each verify what they receive against what they sent, because none
of them trusts the next (0032). What is removed is the repetition around those
checks.

This is an exception to the roadmap's entry rule 6 (one observable result per
change); 0051 records why it is accepted and on what conditions. Rule 12
carries those conditions, and rule 13 states the result that can be checked:
the next call has fewer places to touch.

## Decision

### What must not change

1. **No behavior changes.** For every existing call the following stay exactly
   as they are: the frame message shapes that are accepted and refused, the
   reply shapes, the failure codes and when each is produced, the route paths
   and methods, the request and response bodies, the HTTP statuses, the order
   of the server's checks (binding `404`, grant `403`, head `409`, argument
   checks after them, the re-check before returning) and the final synchronous
   grant check, every limit's value, the prompt's text and the order in which
   an offer is asked for and shown, the object the host hands to the prompt
   desk, the permission controls' text, the frame budget and pending caps,
   timeouts, and the absence of automatic retries for writes.
2. **The three checks stay independent.** No layer stops checking a value
   because another layer checked it. The SDK bootstrap stays a self-contained
   bundle and imports nothing from another package at run time, as
   `package-path.ts` already explains for the path rules.
3. **No contract changes at all**: no frame message, route, body, generated
   schema, contract version, database schema, dependency, CSP, sandbox flag or
   token handling changes.
4. **Out of scope**: widening the capability set (R14 does it with the ninth
   capability), any new capability or call, the by-id asset lookup and the
   capped download read (follow-ups of 0045), moving the runtime modules from
   the server crate into the core, the error taxonomy, and Firefox.

### Client

5. One frame transport, in two modules.
   - `packages/client/src/runtime-frame.ts` holds `FrameTransport`, one route
     builder, one body builder and the name of each of the twelve frame route
     tails. `runtime-source.ts`, `runtime-assets.ts` and the metadata read in
     `client.ts` build their routes and bodies with `runtime-frame.ts` and no
     longer write them out. The client class imports this module, so it is in
     the web entry bundle. The tails are there on purpose: the request gate
     of rule 9 reads them in the entry bundle. Until R13b removes the gate's
     own pattern, the entry bundle carries both; that growth is the explained
     kind of rule 12.
   - `packages/client/src/runtime-bytes.ts` holds one function that compares
     received bytes with a stated size and SHA-256, using the client's
     existing `sha256Hex`. Only `runtime-source.ts` and `runtime-assets.ts`
     import it; `client.ts` does not, and the package index does not export
     it, so the comparison stays out of the entry bundle.
   - `RuntimeAssetsFrame` is removed. Everything the package index exports
     keeps its name, signature and behavior, including
     `MAX_DOWNLOAD_NAME_CHARS` (128, looser than the server's 100 on purpose).
     A digest that cannot be computed still refuses the bytes in both
     families, with the errors each family produces today.

### Host

6. One registry of frame calls. Each of the ten calls has one descriptor that
   holds its type name, the exact keys a message may carry, the parser that
   turns a message into a bounded call, and the function that serves it.
   Descriptors are grouped by family in their own modules (`metadata`,
   `source`, `assets`). `frame-rpc.ts` keeps the envelope only: the lease, the
   budget, the pending map, cancellation, replies and the mapping of errors to
   failure codes. `cancel` is an envelope message, not a call, and stays in
   the envelope. The table of keys, the parser and the dispatch are built from
   the registry, so that on the path from a message to a reply a call type is
   named once, in its descriptor. The module keeps exporting `FrameRpc`,
   `parseCall`, `callId`, `failureCode`, `FrameCall` and `FrameReply` with the
   behavior they have today; `FrameCall` may be derived from the descriptors.
   Capability names in the permission control are not call types and are not
   touched.
7. One download flow. A single function runs offer, ask, fetch, save and
   receipt for both the source and the asset download. It asks the server for
   the offer before it asks the user, hands the prompt desk the same object as
   today, returns nothing when the call was aborted, maps a declined or busy
   prompt to the same failure codes, fetches only after an accept, and replies
   with the receipt shape of its family (`{started, kind, bytes}` for source,
   `{started, assetId, bytes}` for an asset).
8. One description of an offer for the prompt. One function maps an offer to
   the strings the prompt shows, and `download-prompt.tsx` renders those
   strings without looking at the offer's kind. The offer objects, the object
   passed to the desk and the desk's snapshot are unchanged. The rendered text
   for a file, an archive and an asset is identical to today's, character for
   character. `OfferDesk`'s rules (one prompt at a time, the arming delay, the
   cooldown after a decline) are untouched.
9. The request gate builds its frame route pattern from the client's list of
   route tails (rule 5). It does not import the host registry, which is loaded
   on demand, so no on-demand code moves into the entry bundle. The gate
   classifies every path as it does today: the twelve tails as frame calls,
   and any other tail as it is classified now.

### Limits

10. Shared limits are pinned, not copied by hand. The bounds that more than
    one layer writes out are: the retry id pattern (8 to 36 characters of
    `A-Z a-z 0-9 _ -`), the asset id pattern (64 lowercase hexadecimal
    digits), the source part size (1,048,576 bytes), the source write size
    (1,048,576), the asset part size (262,144), the frame asset upload size
    (1,048,576), the asset size (5,242,880), the listed file count (256) and
    the listed asset count (64). Their values stay today's.
    - The client defines each once in `packages/client/src/runtime-limits.ts`
      and exports it; the host imports it instead of repeating the literal.
      The host still applies the bound itself (rule 2).
    - The SDK bootstrap keeps its literals (rule 2).
    - A test compares every client constant and every SDK literal with the
      bound in the generated API schema where the schema states it (a
      `maximum`, a `maxLength`, a `maxItems`), and with each other where it
      does not; the pull request lists which is which. A changed value in any
      one place fails the test.
    - Not shared, because they differ by design: the two download name bounds
      (128 in the source client, 100 in the asset client and the server), the
      media type length the host applies, and each layer's pending cap and
      timeouts.

### Server

11. One way to build a frame call. The twelve frame handlers move from
    `http.rs` to `http/runtime.rs`. The eleven that build a `FrameCall` do so
    through one helper that takes the path and the body's `initialization`,
    with today's rejections for a bad path or body. The metadata handler moves
    with them and keeps calling its operation as it does now. The routes,
    their methods, the middleware in front of them and its order are
    unchanged. `runtime_source.rs`, `runtime_assets.rs` and `operations.rs`
    change only in what they import or in visibility that the move requires.

### Acceptance

12. For every slice:
    - `npm run check`, `npm run build` and the Chromium smoke pass.
    - No existing assertion is removed or weakened. An existing test changes
      only where a moved symbol or a new import path requires it, and each
      such change is listed in the pull request. New tests may be added.
    - Each new guard is proved by mutation, in a copy and never in the
      worktree: a descriptor whose keys or parser differ from today's fails
      the existing host tests; the registry's set of call types equals the
      recorded ten; the gate accepts exactly the twelve tails and classifies a
      thirteenth as before; the prompt strings equal the recorded ones for the
      three kinds; each limit equals its pinned value; and `runtime-bytes.ts`
      is imported only by the two modules rule 5 names and is not exported
      from the package index.
    - The slice removes repetition and adds no empty port and no general
      wrapper with a single user. The pull request reports the lines removed
      and added outside tests.
    - The size of the web entry bundle, of the on-demand host chunk, of the
      client's on-demand runtime chunk and of the generated bootstrap is
      recorded before and after, as built file sizes in bytes. The entry bundle does
      not grow because of on-demand code; any growth is explained.
13. When R13 is complete, this record states how many hand-written files a new
    call in an existing family and a new family touch, measured on the code,
    beside the count from 0051.

### Slices

Each is one pull request unless a review finds it too large. They are listed in
the order they are expected to land; none depends on another except where
noted.

| Slice | Scope                                                                       | Rules   |
| ----- | --------------------------------------------------------------------------- | ------- |
| R13a  | Client: one frame transport, route tails and digest comparison              | 5       |
| R13b  | Host: the call registry, one download flow, the gate's pattern (after R13a) | 6, 7, 9 |
| R13c  | Host: one offer description for the prompt                                  | 8       |
| R13d  | Limits defined once in the client, imported by the host, pinned by a test   | 10      |
| R13e  | Server: frame handlers in their own module with one call builder            | 11      |

## Alternatives considered

- **Do it inside R14.** Rejected: a change that adds a capability and
  restructures the dispatch at once cannot show that the existing calls kept
  their behavior.
- **Generate the host and SDK code from the contract.** Rejected for now: the
  frame messages are not wire DTOs, and a generator would be a larger new
  mechanism than the repetition it removes.
- **Publish the limits in the build inventory.** Rejected: the service
  description refuses unknown fields, so a new field is a contract change that
  older strict readers reject, and it would publish again a value the package
  rules already carry. The generated API schema already states most of the
  bounds, which is enough to pin them.
- **Let the host trust the client's checks.** Rejected by rule 2.
- **Import the limits into the SDK bootstrap.** Rejected: the bootstrap is
  embedded in every opted-in preview and stays self-contained; a test can pin
  its literals.
- **Keep the byte comparison in `runtime-frame.ts`.** Rejected: that module
  is in the entry bundle, and the comparison is used only when a runtime view
  downloads something.
- **Derive the gate's pattern from the host registry.** Rejected: the gate is
  in the entry bundle and the registry is loaded on demand.

## Not decided here

The shape of R14's calls, the capability set's width, and whether the frame
modules of the server move into the core. The last is guidance for new code in
0051 and not a task of this record.
