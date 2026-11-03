# 0032: Runtime source reading and download requests

Date: 2026-11-03. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) (the preview sandbox),
[0005](0005-request-limits.md) (request limits),
[0015](0015-source-exports.md) (the source export),
[0016](0016-bounded-file-reads.md) (parts of a file),
[0019](0019-runtime-metadata-bridge.md), and
[0020](0020-runtime-grants.md) (the grant model). Roadmap row: R11a. Independent
Artifree design: the roadmap row gives only the names of the two features and
the requirement that the saved bytes be compared with the stored ones. The
routes, the bounds, the prompt rules, and every error below are Artifree's own
decisions. Nothing below is an observation of the original product.

## Context

R05 let a preview read its own metadata after the user granted `metadata.read`.
An app also needs its own source: to show the files it was built from, to
offer one to its user, or to offer the whole package. R11 is split in two so
that each part can be reviewed whole. **This record is R11a: reading the source
and asking for a download, both read-only.** R11b, writing source from a
frame, is not implemented; it depends on this record and adds the editing
permission, viewer checks, and conditional saves. Nothing here lets a frame
change a revision.

## Decisions

1. **Two capabilities, each with the 0020 model.** `source.read` lists the
   files of the pinned revision and reads parts of them; `source.download`
   asks the workspace to offer one stored file, or the source export (0015) of
   the revision, for the user to save. Each is **declared** in
   `artifree-runtime.json`, **supported** by the server, **consented** by the
   user, and **granted** per connection in memory. `RuntimePolicy::supported`
   is now `metadata.read`, `source.download`, `source.read`. A grant is a set;
   the workspace shows one control for each capability ("Allow source
   reading", "Allow download requests"), and a grant of one never implies
   another: a frame that may read cannot ask for a download. An entry without
   a manifest still declares `metadata.read` only. The grant scope
   `(artifact, revision, head)` and the version rules of 0020 are unchanged,
   so a grant change, a new head, a closed connection, and a server restart
   revoke exactly as before. **No new persistent state:** nothing is written to
   the database, and the R01 BLOB allow-list is unchanged.
2. **Five routes, all `POST`, under the view.**
   `/v1/artifacts/{id}/revisions/{n}/runtime-views/{view}/{generation}/`
   `metadata`, `source/list`, `source/read`, `source/offer`, and
   `source/download`. Each takes the view's `initialization` secret in a body of
   at most 16 KiB; a `GET`, or any other method, is `405` with `Allow`. The
   frame holds no account token. Its script posts to the workspace page over
   a `MessagePort`, and the page sends the request with the owner's token. A
   request without the owner's token is `401` before anything else is looked at.
3. **The server decides on every call, from live state.** Each frame call
   checks, in this order: the shape of the route names (`400`), the **binding**
   (actor, artifact, revision, view, generation, secret, and the grant version
   the view was issued with), the connection's **current grant** of the one
   capability the call needs, the **live artifact**, and that the artifact's
   head is still the one the view was pinned to. It checks the binding and the
   grant a second time after the asynchronous work and before any byte is
   returned, so a revoke, a close, or a replacement that lands in between
   releases nothing. Answers:
   - A binding that does not match, was replaced, closed, expired, or whose
     connection is gone: `404`, one answer for all of them.
   - A matching binding without the capability: `403`.
   - A trashed or purged artifact: `404`.
   - A head that moved: `409 revision_conflict` with `current_revision`. The
     workspace starts a new view and the user consents again, because the
     grant was scoped to the old head. No new error code was added, so the
     MCP tool schemas did not change.

   **Why `404` before `403`.** The binding contains secrets the frame was
   issued. A caller who has none of them must not learn whether a view,
   an artifact, or a connection exists, so every failure to match is the same
   `404`. Only a caller that proved the binding is told the grant is what is
   missing, which it can know anyway (it is shown in the same workspace). Other
   actors get `404`, not `403`, as everywhere in the API. `metadata` keeps its
   R04 answers (`404` for every refusal); only the new routes use `403` and
   `409`.

4. **Reading.** `source/list` returns the revision number and its files (path,
   size, SHA-256 of the whole file, media type), each path once.
   `source/read` takes `path`, `offset`, and `length` and returns one part:
   `path`, `revision`, `offset`, `next_offset`, `size`, the **whole-file**
   `sha256`, and `content` in base64. `length` is from 1 to 1,048,576 and
   `offset` at most 2^53 - 1; both are checked after authorization, so a
   caller without a binding learns nothing from how its arguments were judged.
   A part is cut at **bytes** (the `Boundary::Byte` of 0016): it can end in the
   middle of a UTF-8 character, and a reader joins parts as bytes. `next_offset`
   is the offset of the following part, or `null` when the bytes of this part
   end exactly at `size`. The server returns no part with fewer bytes than
   asked for unless it reaches the end. An offset equal to `size` gives an
   empty part. There is no `Range` header here.
5. **No number a peer states is believed, in every parser.** The same rules are
   enforced by the client (`parseRuntimeSourcePart`), by the workspace host
   (`parseCall` for what a frame sends, the client for what the server sends),
   and by the frame script (`part`), each from the bytes it received:
   - every number is a safe integer from 0;
   - `offset <= size`; a part is at most 1,048,576 bytes and at most as long as
     was asked for;
   - the part echoes the requested `path` and `offset`;
   - `next_offset` is strictly greater than `offset` and strictly less than
     `size`, and the bytes number exactly `next_offset - offset` (which is the
     length asked for); a `null` `next_offset` means the bytes number exactly
     `size - offset`. A part that makes no progress is refused, so a read loop
     always ends;
   - `sha256` is 64 lowercase hex digits; a listing names each path once; a
     download receipt's size is the number of bytes actually received;
   - a path the frame supplies obeys the package path rules (decision 0017)
     before anything is sent: the frame script, the host's `parseCall` and the
     server judge `contracts/fixtures/package-paths.json` alike, each in its
     own test. The frame script holds its own copy of the rules (it is a
     self-contained bundle, so the generated inventory is not imported), and a
     test compares that copy with the build inventory.
   - a **file download** is the offered digest: the client hashes the bytes it
     received with `crypto.subtle` and refuses a mismatch, also when the length
     is right. A platform that cannot compute the digest (no `crypto.subtle`,
     as on a page that is not a secure context) refuses the download; it never
     passes it unchecked. An **archive** is built on demand and has no digest;
     it is checked for nothing but its closed content type and attachment name,
     and the receipt reports the bytes received. That is a stated limit, not a
     check.

   Base64 is read only in its canonical form (no whitespace, correct padding,
   zero padding bits). Fixture `scripts/tests/fixtures/source-part-vectors.json`
   lists 29 parts, accepted and refused; the client test and the frame-script
   test both run every one.

6. **Downloading takes two server calls and one user decision.** The frame
   calls `requestDownload({ path })` for a stored file or `requestDownload()`
   for the archive of the pinned revision.
   1. The workspace first calls `source/offer`. The server runs the full check
      of decision 3 for `source.download` and answers what it would hand over
      (kind, path, file name, size, digest; for the archive only the name). **A
      prompt is never opened from the page's own picture of the grant**
      (`view.granted` is a snapshot): a grant revoked or a head moved since
      the view was issued is refused by this call, and the frame is told
      `denied` or `stale_view` without the user seeing anything.
   2. The workspace asks the user (decision 7). On accept it calls
      `source/download`, which checks again and returns the bytes.
   3. The workspace hands the bytes to the browser (decision 8) and tells the
      frame `{ started: true, kind, bytes }`.

   The offer and the download are two calls. Between the user's accept and the
   download the grant can be revoked or the head can move, so an accepted
   prompt can end with nothing saved: the server is right to refuse, and the
   frame is told `denied` or `stale_view`. The user is never told that a
   file was saved when it was not.

   **`started` means "handed to the browser".** The browser or the user can
   still cancel the save, and the workspace cannot tell. `bytes` is the number
   of bytes the workspace received, which the client checks against the
   offered size for a file.

7. **The prompt is the workspace's, and its rules live in `OfferDesk`.** The
   gate has no DOM and no React and takes an injectable clock; the React
   component only shows `snapshot()` and forwards presses.
   - **One offer at a time.** A second request while one is open is refused
     (`rate_limited`) and does not change the first.
   - **Activation delay.** Accept counts only 800 ms after the prompt appeared,
     and only for a press the browser itself made (`isTrusted`). The button
     is disabled until then; the gate decides regardless of the button. A
     second press in the same turn finds nothing open.
   - **Decline** counts at once, and starts a 5-second cooldown during which
     the frame's requests are refused (`rate_limited`) without a prompt.
     Dismissing the dialog is a decline.
   - **Withdrawal.** The offer is withdrawn when the frame gives up (its
     script sends `cancel` after a timeout), when the frame is gone, and when
     the signal was aborted before the prompt could appear (nothing is shown).
     Every exit removes its timer and its abort listener.
   - **No layout shift.** The prompt is a modal over the page; the frame does
     not move. Focus starts on Decline, so a stray key press by someone typing
     in the frame declines instead of saving.
   - **Frame text is data.** The file name is the server's sanitized name. The
     path, which a frame chose, is shown only inside a truncated code span
     (80 characters, then an ellipsis) and never as markup.

   The cooldown belongs to one frame; a new view has a new desk. The delay and
   the cooldown reduce the chance of an accidental or coerced click; they are
   not a security boundary, and the server checks again whatever the page does.

8. **The download response is hardened, and the management origin never
   renders the content.**
   - `source/download` is `POST` only. A stored file is served as
     `application/octet-stream`, the archive as `application/zip`; the set is
     closed, whatever the file's media type.
   - `Content-Disposition: attachment; filename="NAME"` where NAME is the last
     path segment reduced to ASCII letters, digits, `.`, `_` and `-`, with a
     leading dot replaced, at most 100 characters (the longest package path
     segment; a Rust test holds the two together). The archive is
     `{artifact}-r{revision}.zip`. The client accepts a name of at most 128
     such characters and exactly the header it was offered, and a test reads
     the server's bound from the generated schema and the package rules to
     keep `server <= client`.
   - `X-Content-Type-Options: nosniff`, `Content-Security-Policy: sandbox;
default-src 'none'`, and `Cache-Control: no-store`.
   - The workspace puts the bytes in a `Blob` of the same closed type and starts
     the save with a hidden anchor carrying `download`. It never navigates to
     the object URL, renders it, or opens it. The URL is revoked about 60
     seconds after it was handed over, or at once when the next one replaces
     it, and **not** when a frame goes away, because the browser may still be
     reading it.
   - No download starts without the prompt of decision 7 and a press the
     browser made.
9. **Frame traffic has its own budget on both sides.** A frame is untrusted
   script and can send as fast as it likes.
   - **Host:** a token bucket per frame (burst 30, 600 per minute, on an
     injectable clock) is charged for every message, valid or not, before
     anything is sent; at most 16 calls wait at once. Over either, the frame is
     told `rate_limited` and the server is not asked.
   - **Server:** frame routes have their own per-actor limiter (60 at once,
     1,200 per minute, 16 concurrent), separate from the owner's budget, so a
     flood by a frame cannot make the owner's reads, saves, or the revoke of
     that frame's grant wait. The web request gate classes frame calls as their
     own kind: a `429` for a frame holds no owner reads. A test floods the
     frame budget and then makes an owner request and revokes the grant.
   - **Downloads under construction:** a file is read, and an archive is built,
     whole in memory (up to the package limit). A hostile frame that holds
     `source.download` could otherwise start up to 16 builds at once, so each
     actor may have at most 2 downloads being built (`MAX_DOWNLOAD_BUILDS`).
     A build takes a slot after authorization and holds it until the bytes
     are returned or the work fails; dropping it frees it on every exit, and
     a further request is answered `429 rate_limited` with `Retry-After`. A
     test holds every slot, sees the next download refused while a list and an
     offer still work, and sees a freed slot and a failed build reuse.
   - `rate_limited` is in the frame's closed set of failures.
   - **The host answers every call it can.** A message with a known `type` and
     a positive integer `id` that does not parse (an unaccepted path, a bad
     length, an extra key) is answered `invalid_request` at once, costs a
     token, and holds no waiting slot, so a frame never waits for the timeout
     of a call that was never going to be served. A message without a usable
     id cannot be answered and is dropped. Two deliberate silences remain: a
     `cancel` that parses gets no reply, and a call whose `id` is already
     waiting is dropped, since that id will be answered once.
10. **The frame sees a closed vocabulary.** A failed call rejects with
    `RuntimeRequestFailure` whose `code` is one of `invalid_request`, `denied`,
    `stale_view`, `not_found`, `rate_limited`, `declined`, `unavailable`,
    `expired`, and whose message is a fixed sentence for that code. No server
    text, status, path, or value is forwarded, and no value the frame sent is
    echoed in an error. The workspace maps `404` to `not_found`, `403` to
    `denied`, `409 revision_conflict` to `stale_view`, `429` to `rate_limited`,
    `400/413/416/422` to `invalid_request`, a call the client refused because
    its runtime connection ended (marked by `RuntimeConnectionReplacedError`,
    not by its status and code) to `expired`,
    and everything else to `unavailable`. The server's frame routes answer
    malformed, duplicate-field, over-deep, wrongly typed, and oversized bodies
    with fixed messages and never panic. The SDK's public methods never throw
    synchronously: an argument that is `null`, an array, a primitive, a
    `Proxy`, a revoked proxy, or an object with a getter is copied once,
    property by property, with errors caught, and refused as
    `invalid_request` before anything is sent, with no pending slot used. A
    read waits 10 seconds and a download 120, then the script sends `cancel` so
    the workspace aborts its request and withdraws the prompt.
11. **Separation from agent tools.** **No MCP tool was added or changed.** A
    frame's capability is a connection grant to the page; an agent's tools are
    the owner's or an issued token's. They do not share a permission: granting
    `source.read` to a preview gives an agent nothing, and an agent token does
    not give a frame anything. The frame never holds an account token or any
    token.

    **The `allow-downloads` sandbox flag is unchanged and unrelated.** The
    preview iframe has always been sandboxed with `allow-downloads` (a page
    can start its own download, as any page can). That flag is the browser's
    permission for the frame's own script; it is not the `source.download`
    capability, it needs no grant, and it does not reach the stored source.
    The rejected alternative was to let the frame save by building a `Blob`
    link itself after a `source.read`: it would bypass the user's confirmation
    for the archive and for large files, remove the workspace's name rules,
    and make the sandbox flag the permission. Removing the flag would also
    break pages that do not use any capability, so it is left alone.

12. **Surface.** Contract version 0.10.0. `RuntimeSourceListRequest`,
    `RuntimeSourceList`, `RuntimeSourceReadRequest` (`length` has
    `maximum 1048576` in the schema), `RuntimeSourcePart`,
    `RuntimeDownloadKind`, `RuntimeDownloadRequest`, `RuntimeDownloadOffer`
    (`file_name` has `maxLength 100`). Client methods `runtimeSourceList`,
    `runtimeSourceRead`, `runtimeSourceOffer`, `runtimeSourceDownload`. The
    runtime SDK adds `window.artifree.source.list()`, `.read()`, and
    `.requestDownload()` and its types; the bootstrap script is regenerated.
    **The SDK contains the frame script and types only; it enforces nothing.**
    Every rule is the server's and the workspace's.

## Limits and what is not implemented

- **Implemented:** reading and downloading as above, in the web workspace
  with a bound frame. Code that only a page showing a runtime preview
  needs is not in the main bundle, which stays under Vite's 500 kB warning
  (499.99 kB with this change, 499.55 kB before it: the margin is thin, and
  the limit was not raised). Four chunks load on demand: the frame component
  and the permission controls (when a preview is shown), the workspace's side
  of the bridge (rpc, budget, prompt, saver; only for an enabled view, and the
  frame is not shown until it is loaded, so its connection is never missed),
  and the client's source checks (on the first source call).
- **Not implemented:** writing source from a frame, editing permission, viewer
  roles (R11b); data access from frames (the bridge still has no data
  capability, R06 stays server-side); subscriptions and queries (R07);
  uploaded assets (R09); replacing a view in place (R10: a grant change or a
  new head still reloads the frame); an outbox and crash recovery (R08);
  rooms and presence (R12); a frame identity or per-frame permissions beyond
  the connection (S01).
- **The sandbox is not network isolation.** A frame can still navigate itself
  or load resources; this record makes no claim that it cannot send what it
  read elsewhere. `source.read` hands the frame the revision's bytes; the user
  is told, by the permission control, that they are granting that.
- A file is at most 5 MiB (package rules), and the archive is built in memory
  on the server and held in memory by the workspace before it is saved; there
  is no streaming and no resume. A download that the user cancels in the
  browser is not reported to the frame.
- The prompt shows a name, a size, and a path; it does not show content. A
  hostile frame can choose which stored file to offer and when, within the
  rules of decision 7; it cannot choose the name's characters or the bytes.
- The host's budget is a pacing measure per frame in the page; a frame that
  reloads gets a new one. The server's frame limit is per actor.
- The grant controls are in the preview panel. There is no list of which frame
  read what, and no log.

## Tests

Rust: `crates/artifree-server/tests/runtime_source.rs` (listing and reading
parts joined by digest, a non-ASCII text cut in the middle of a character, a
binary file, an empty file and an offset at the end; every read refusal and that
none echoes the request; `404` before `403`, regrant, and a second generation;
an undeclared capability; a moved head with `current_revision`; a trashed
artifact; download headers, the archive equal to `GET export`, and download
needing its own consent; hostile bodies; a flood of frame calls with owner
requests and the revoke still served, and an owner flood that leaves the frame
budget alone; a restart that revokes), `runtime_tests.rs` (a revoke after the bytes were read releases none of them, for a list, a read, an offer and a download; the download slots), the shared path fixture judged by every path-taking route,
`runtime_policy.rs`, `runtime_sessions.rs`, the unit tests of the name rule (hostile strings reduced, the bound equal to the longest package segment), and `crates/artifree-contracts/tests/wire.rs` (the schema bounds equal the constants).

TypeScript (`npm run test:tooling`): `runtime-source-client.test.mjs`
(every vector, multi-part join, listings, offers, base64, the name bound,
download headers and sizes), `runtime-bootstrap.test.mjs` (the frame script
against a real `MessageChannel`: hostile arguments of every kind, no leaked
pending slot, every vector, a closed failure set, timeouts and cancel, expiry),
`runtime-host.test.mjs` (the token bucket, the desk's delay, trust, re-entrancy,
cooldown, one at a time and every withdrawal with no timer or listener left,
the call parser, the pending cap, a flood, the offer before the prompt, cancel,
close, the object-URL lifetime, a call that does not parse answered at once,
the shared path fixture, the failure mapping), and `request-gate.test.mjs` (every
route family, frame calls included, under four server address shapes with and
without a path prefix, and a frame `429` that holds no owner read). The frame
script test and the host test also run the shared path fixture, and the client
tests cover the digest of a file download, a platform without `crypto.subtle`,
and the marker of a replaced connection.

Browser: `scripts/smoke-source.mjs` in Chromium and Firefox: a frame lists
and reads parts and its bytes are compared by digest with the stored ones
(a non-ASCII text, a binary file, an empty file); a confirmed download of a file,
of a binary file, and of the archive is compared with the stored bytes and the
export; the refusals for no grant, a decline and its cooldown, and a page
whose snapshot says "granted" after the server revoked it (no prompt opens); an
old generation and a moved head over the protocol with the real client.

## Alternatives

- **One `source` capability.** Reading to show a file and saving a file to the
  disk are different asks of the user; a user who allows the first should not
  silently allow the second.
- **A download link served to the frame.** The frame would then hold an
  address that works without the workspace, outside the prompt and the
  budget.
- **Trusting `view.granted` to open the prompt.** It is a snapshot taken when
  the view was issued; a revoke or a new head makes it wrong. The extra
  `source/offer` call costs one request and removes the case.
- **A new error code for a stale view.** `revision_conflict` already carries
  `current_revision`, and a new code would have changed the MCP schemas for a
  feature MCP does not have.
- **Streaming the download to disk.** It needs a service worker or the
  File System Access API, neither of which is in this stack; the 5 MiB bound
  makes the in-memory path acceptable for now.

## Costs

Three more runtime routes and two more for downloads, a second limiter, a
larger frame script, a host module set (budget, desk, rpc, saver, prompt) that
must stay free of the DOM to stay testable, one more request before every
download prompt, and the promise that the part and name rules stay stable.
