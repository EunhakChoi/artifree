# 0052: Consolidating the runtime bridge without changing its behavior

> Browser engines: Firefox is suspended in CI until the planned feature set is
> complete. A Firefox acceptance below is met in Chromium and reported as pending
> re-verification ([quality](../quality.md#browser-engines)).

Date: 2026-11-10. Status: **proposed** (specification only; no code).
Roadmap: R13. Builds on [0019](0019-runtime-metadata-bridge.md),
[0020](0020-runtime-grants.md),
[0032](0032-runtime-source-read-and-download.md),
[0033](0033-runtime-source-write.md), [0045](0045-uploaded-assets.md) and
[0051](0051-roadmap-and-architecture-review.md).

## Context

The runtime bridge carries a call from a sandboxed frame to the store through
four places: the frame's SDK, the workspace page that hosts the frame, the
TypeScript client, and the server. Seven capabilities, ten frame calls and
twelve frame routes exist. They were added one slice at a time, each copying the shape of the one
before, and the review in 0051 found that one more capability touches about
twelve hand-written files in five packages. The next rows add more (R14, R15,
then the P rows), so the repetition is removed first.

What repeats today, read from the code on `main`:

- **Client.** `runtime-source.ts` defines `FrameTransport`; `runtime-assets.ts`
  defines `RuntimeAssetsFrame` with the same four members. Each file has its
  own `frameRoute` and `frameBody`, and its own comparison of received bytes
  with a stated SHA-256.
- **Host.** `frame-rpc.ts` names every call type three times: in the `SHAPES`
  table of allowed keys, in the `parseCall` switch, and in the `#serve`
  switch. `#download` and `#assetDownload` are the same sequence (offer, ask
  the user, fetch, hand to the browser, receipt) written twice.
  `download-prompt.tsx` branches on the offer's kind in five places. The
  request gate spells every frame route tail in one regular expression.
- **Limits.** The retry id pattern, the asset id pattern and the part, write
  and upload sizes are written out in the host, in the SDK bootstrap and, in
  part, in the client. The server holds the values that are enforced.
- **Server.** The twelve frame handlers in `http.rs` each rebuild the same
  `FrameCall` from the path and the body's `initialization`. The checks
  themselves (`authorize`, `HeldCall`, `still_permitted`) are already shared
  between `runtime_source.rs` and `runtime_assets.rs`.

The three independent checks are deliberate and stay: the client, the host and
the frame each verify what they receive against what they sent, because none
of them trusts the next (0032). What is removed is the repetition around those
checks.

This is an exception to the roadmap's entry rule 6 (one observable result per
change); 0051 records why it is accepted. Its result is checkable: the
existing checks pass unchanged and the next call has fewer places to touch
(rule 12).

## Decision

### What must not change

1. **No behavior changes.** For every existing call the following stay exactly
   as they are: the frame message shapes that are accepted and refused, the
   reply shapes, the failure codes and when each is produced, the route paths
   and methods, the request and response bodies, the HTTP statuses, the order
   of the server's checks (binding `404`, grant `403`, head `409`, argument
   checks after them, the re-check before returning) and the final synchronous
   grant check, every limit's value, the prompt's text and the order in which
   an offer is asked for and shown, the permission controls' text, the frame
   budget and pending cap, timeouts, and the absence of automatic retries for
   writes.
2. **The three checks stay independent.** A layer may share definitions with
   code in its own package. No layer stops checking a value because another
   layer checked it. The SDK bootstrap stays a self-contained bundle and
   imports nothing from the contracts package at run time, as
   `package-path.ts` already explains for the path rules.
3. **No contract of a frame or a route changes**, and no schema, dependency,
   CSP, sandbox flag or token handling changes. The single contract change is
   the additive inventory field of rule 9, with a minor contract version.
4. **Out of scope**: widening the capability set (R14 does it with the ninth
   capability), any new capability or call, the by-id asset lookup and the
   capped download read (follow-ups of 0045), moving the runtime modules from
   the server crate into the core, the error taxonomy, and Firefox.

### Client

5. One frame transport. `FrameTransport`, the route builder, the body builder
   and one function that compares received bytes with a stated size and
   SHA-256 live in a new `packages/client/src/runtime-frame.ts`.
   `runtime-source.ts` and `runtime-assets.ts` import them and no longer define
   their own; `RuntimeAssetsFrame` is removed. Everything the package index
   exports keeps its name, signature and behavior. A digest that cannot be
   computed still refuses the bytes in both families, with the errors each
   family produces today.

### Host

6. One registry of frame calls. Each call has one descriptor that holds its
   type name, the exact keys a message may carry, the parser that turns a
   message into a bounded call, and the function that serves it. Descriptors
   are grouped by family in their own modules (`metadata`, `source`, `assets`).
   `frame-rpc.ts` keeps the envelope only: the lease, the budget, the pending
   map, cancellation, replies and the mapping of errors to failure codes. The
   table of keys, the parser and the dispatch are derived from the registry;
   no call type is named anywhere else in the host. The module keeps exporting
   `FrameRpc`, `parseCall`, `callId`, `failureCode`, `FrameCall` and
   `FrameReply` with the behavior they have today.
7. One download flow. A single function runs offer, ask, fetch, save and
   receipt for both the source and the asset download. It asks the server for
   the offer before it asks the user, returns nothing when the call was
   aborted, maps a declined or busy prompt to the same failure codes, fetches
   only after an accept, and replies with the receipt shape of its family
   (`{started, kind, bytes}` for source, `{started, assetId, bytes}` for an
   asset).
8. One description of an offer for the prompt. Each family turns its offer
   into the strings the prompt shows; `download-prompt.tsx` renders them and no
   longer branches on the kind. The rendered text for a file, an archive and an
   asset is identical to today's, character for character. `OfferDesk`'s rules
   (one prompt at a time, the arming delay, the cooldown after a decline) are
   untouched.
9. The request gate classifies a frame call from the registry's route tails.
   The set of tails is the same twelve as today; a path with any other tail is
   classified as it is today.

### Limits

10. One source for the limits the layers share. The server's enforced values
    are published in the build inventory as an additive `runtime_limits`
    object beside `package_rules`, filled from the constants the domain and
    the contract already define: the source part size (1,048,576 bytes), the
    source write size (1,048,576), the asset part size (262,144), the frame
    asset upload size (1,048,576), the asset size (5,242,880), the listed file
    count (256), the listed asset count (64), the retry id length range (8 to
    36), the media type length (127) and the offer name length (100). The
    values are today's. The host and the client read them from the generated
    inventory. The SDK bootstrap keeps its literals, and a test compares each
    with the inventory so that they cannot drift, as one already does for the
    path rules. The character classes of the retry id and asset id patterns
    stay written where they are used; only their length bounds come from the
    inventory. The inventory is a build-time snapshot and no route serves a
    different value.

### Server

11. One way to build a frame call. The frame handlers move from `http.rs` to
    `http/runtime.rs`, and a single helper builds the `FrameCall` from the
    path and the body's `initialization`, with today's rejections for a bad
    path or body. The routes, their methods, the middleware in front of them
    and its order are unchanged. `runtime_source.rs` and `runtime_assets.rs`
    change only in what they import.

### Acceptance

12. For every slice:
    - `npm run check`, `npm run build` and the Chromium smoke pass.
    - No existing assertion is removed or weakened. A test file changes only
      where a moved symbol or a new import path requires it, and the change is
      listed in the pull request.
    - Each new guard is proved by mutation: the registry is the only place a
      call type is named (a call added to the dispatch but not to the registry
      fails a test), the gate's tails equal the registry's, the prompt strings
      equal the recorded ones, and each SDK literal equals the inventory's
      value.
    - The size of the web bundle and of the generated bootstrap is recorded
      before and after; a growth is explained.
13. When R13 is complete, this record states how many hand-written files a new
    call in an existing family and a new family touch, measured on the code,
    beside the count from 0051.

### Slices

Each is one pull request unless a review finds it too large. They are listed in
the order they are expected to land; none depends on another except where
noted.

| Slice | Scope                                                                 | Rules   |
| ----- | --------------------------------------------------------------------- | ------- |
| R13a  | Client: one frame transport and digest comparison                     | 5       |
| R13b  | Host: the call registry, the derived gate tails and one download flow | 6, 7, 9 |
| R13c  | Host: one offer description for the prompt (after R13b)               | 8       |
| R13d  | Limits in the build inventory, read by the host and client, SDK test  | 10      |
| R13e  | Server: frame handlers in their own module with one call builder      | 11      |

## Alternatives considered

- **Do it inside R14.** Rejected: a change that adds a capability and
  restructures the dispatch at once cannot show that the existing calls kept
  their behavior.
- **Generate the host and SDK code from the contract.** Rejected for now: the
  frame messages are not wire DTOs, and a generator would be a larger new
  mechanism than the repetition it removes.
- **Let the host trust the client's checks.** Rejected by rule 2.
- **Import the limits into the SDK bootstrap.** Rejected: the bootstrap is
  embedded in every opted-in preview, and bundling the inventory into it would
  grow it for values a test can pin.

## Not decided here

The shape of R14's calls, the capability set's width, and whether the frame
modules of the server move into the core. The last is guidance for new code in
0051 and not a task of this record.
