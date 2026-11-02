# 0032: Runtime source, edit, and download capabilities

Date: 2026-11-02. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) (request keys, sandbox),
[0004](0004-inbound-mcp-adapter.md), [0015](0015-source-export.md) (export),
[0019](0019-runtime-metadata-bridge.md) (view binding),
[0020](0020-runtime-grants.md) (grants), and
[0031](0031-app-data.md) (a frame cannot reach data). Roadmap row: R11.
Independent Artifree design: the private evidence index gives this row only
the idea that an app can read and save its own source and offer a download
under an explicit edit permission, separate from the permissions an agent
has. The capability names, the error codes, the limits, and every rule below
are Artifree's own decisions. Nothing here is an observation of the original
product.

## Context

R04 and R05 gave an opted-in preview one capability, `metadata.read`, with a
per-connection grant the user controls. An app that is a small editor, a
notebook, or a generator needs more: to read the files it was built from, to
save a changed copy of them, and to hand the user a file. Each of those is a
different amount of trust. Reading a file reveals source the user may not
expect an app to read. Saving makes a revision under the user's name, and
downloading puts bytes on the user's disk. Roadmap R11 asks for them with an
explicit edit permission, with agent tools kept apart from the runtime's
permissions, and with a viewer's and an old generation's writes refused.

## Decisions

1. **Four capabilities, three of them new.** `source.read`, `source.write`,
   and `source.download` join `metadata.read`. The supported list, in order, is
   `metadata.read`, `source.read`, `source.write`, `source.download`. They
   follow ADR 0020 unchanged: a capability must be declared in
   `artifree-runtime.json` (a marker without a manifest still declares only
   `metadata.read`), a grant must be a distinct, supported, declared subset,
   the empty set clears, and a grant belongs to one connection (process-local
   and in memory, so a restart revokes every grant) and to the scope of
   artifact, revision, and head. **`source.write` is its own grant. Reading
   does not imply it and writing does not imply reading.** A frame that
   needs both asks for both.
2. **Every call checks the full binding, then the current grant.** A source
   call first matches the view exactly as a metadata read does (actor,
   artifact, revision, view, generation, initialization, an open connection,
   the head), and only then asks whether the connection currently grants the
   capability that call needs. A read and a download check before the work and
   again before the answer, so a revoke that arrives while the bytes are being
   read means the frame does not get them. A save checks before it commits
   (decision 7). Revocation, logout, and a newer generation take effect on the
   next call.
3. **Agent tools and runtime grants are separate.** No MCP tool was added or
   changed. The frame never holds an account token or an agent token: it
   speaks to the workspace page over a message port, and the page calls the
   server with the user's token and the view's initialization secret. A
   grant is session state of the web connection: an agent token does not carry
   it, cannot widen it, and cannot use it.
4. **A frame's save is the viewing user's save.** It goes through
   `ArtifactService::save`, like every mutation (base revision and
   idempotency key). Its `created_by` is the viewing user's actor, the same
   value as the owner's own saves, so revision metadata and history cannot
   tell a frame's save from the user's; the change feed shows the ordinary
   commit kinds. There is no frame identity yet (a later security row). The
   web consent text says so: the app saves "under your name".
5. **The base is the view's pinned revision, never the frame's.** A frame
   cannot name a base. The service sees the revision the view was created for.
   If the source has moved on, the save fails with `revision_conflict` and the
   new head in `current_revision` and writes nothing. A frame that wants to
   build on the new head needs a new view, and a head change changes the grant
   scope, so the user consents again (ADR 0020). Two frames pinned to the same
   revision racing to save produce one revision and one conflict.
6. **Retries.** The frame supplies a key (8 to 100 characters from
   `A-Za-z0-9._:-`) and the workspace sends it as `Idempotency-Key`. The
   server accepts a header up to 122 characters and namespaces it as
   `frame:` plus the key, so a frame can neither reuse nor probe the owner's
   own keys (a longer header is a 400). The same key with the same changes
   returns the first result with `replayed: true`, also after the head moved;
   the same key with different changes is a 422 `request_key_reused`.
7. **Known window.** The save checks the grant, then commits. A request that
   passed the check can commit once even if the user revokes a moment later
   or the view is replaced. Nothing after that check can stop it, and the
   commit is not undone. It is one revision, attributed to the user, and the
   user can restore the earlier revision from History. The tests prove the
   next call is refused, not that an in-flight save is.
8. **`source.read` is bounded.** One answer holds at most 1 MiB
   (`MAX_RUNTIME_READ_BYTES`). `offset` and `length` select a range; `length` 0
   is a 400, an offset past the end is a 416
   (`range_not_satisfiable`), and an answer carries the base64 `content`,
   the whole file's `sha256` and `size`, and `next_offset` (null at the end).
   The listing route returns the pinned revision's files with size, SHA-256,
   and media type. A frame reads the pinned revision, never the head.
9. **Downloads are exact bytes under a closed type, and the workspace, not
   the frame, starts them.** `POST .../download` returns the stored bytes of
   one file (`application/octet-stream`, whatever the file is) or, with no
   `path`, an archive (`application/zip`) equal byte for byte to the A07 export
   of the pinned revision, not the head. The headers are
   `Content-Disposition: attachment; filename="..."` (the last path segment
   with unsafe characters replaced, or `{id}-r{revision}.zip`),
   `X-Content-Type-Options: nosniff`, and `Content-Security-Policy: sandbox;
default-src 'none'`. The client refuses a response whose type or name is
   not in that closed set. In the web workspace a frame's request only shows
   an alert ("Download request", with Download and Decline) naming the file or
   the archive and the revision. The bytes are fetched (the server checks the
   grant again) and saved through a `Blob` of the closed type and a download
   link only after the user presses Download. The frame gets a receipt
   (`status: "started"`, the kind, name, size, and revision), `declined`, or
   `busy`. One offer is open at a time, and an offer disappears with its frame.
   A receipt is not proof that the user kept the file.
10. **The frame sees a closed set of error codes.** `invalid_request`,
    `not_granted`, `view_unavailable`, `not_found`, `revision_conflict`
    (with `currentRevision`), `key_reused`, `too_large`, `range`,
    `rate_limited`, `busy`, `declined`, `unavailable`, `refused`. The
    workspace maps server errors onto them and never forwards a server message.
    On the server, the four frame routes reject malformed bodies with a fixed
    message instead of the JSON library's text, which can quote what the frame
    sent. The runtime script checks arguments before posting and checks each
    answer's shape; the workspace checks every message again (hostile input:
    keys, paths, sizes, counts, duplicated paths, lone surrogates, extra
    members) and the server checks everything once more.
11. **Status codes.** A request that fails the view binding is a 404 (an old or
    closed generation, an unknown or forged view, a wrong initialization, a
    trashed or purged artifact). A request that matches the binding but
    lacks the capability is a 403 `forbidden`, so the split cannot be used to
    learn whether a view exists. A missing file is a 404. Size, count, and
    path limits are the service's and the domain's (decision 0003). Frame
    routes share the router's rate limit (429).
12. **The sandbox flag is unrelated.** The preview sandbox is `allow-scripts
allow-forms allow-modals allow-downloads` (decision 0003). `allow-downloads`
    lets a frame offer its own generated file to the browser, with the
    browser's own prompts, whether or not it has any capability here.
    `source.download` is different: it lets the frame ask the workspace to
    offer the stored source of this artifact. The sandbox is not changed, and
    nothing here makes the frame network-isolated.
13. **The runtime SDK package ships a script and types.** The frame's side
    (`bootstrap.ts`, since R04) is real code that the server embeds in an
    opted-in document; the package's index stays types only, for consumers
    that check `window.artifree`. The `window.artifree` object gained `listSource`, `readSource`, `saveSource`, and `requestDownload`.

## Limits and what is not implemented

- Frames cannot read or write app data (ADR 0031; R06 data access), subscribe
  (R07), upload (R09), have their view replaced while running (R10), queue
  work offline (R08), or share a room (R12). `model.generate`, `data.*`,
  `files.read`, and `tools.call` remain names with no grant.
- A grant is lost on a head change; the user must consent again.
- A save that passed its check can commit once after a revoke (decision 7).
- `not_found` is ambiguous on purpose: a missing file, a missing view, or an
  unavailable artifact. A frame cannot tell a trashed artifact from a deleted
  one.
- The 403 for a missing capability differs from the metadata read, which says
  404 for every refusal; metadata is unchanged.
- The commit is attributed to the user, not to the app, until a frame identity
  exists.
- The browser smoke does not drive a stale pinned base through the UI; the
  Rust tests do. The smoke also does not run a save past a revoke.

## Tests

`crates/artifree-server/tests/runtime_source.rs` (read grant, bytes and
digests; bounded and joined reads; a read-only frame cannot write; non-ASCII
and binary saves; replay and key reuse; frame and owner keys separate; a stale
pinned base; two frames racing; a replaced or closed generation; revoke and
regrant; logout; a restart; forged bindings; a trashed artifact; size, count,
and path rules; hostile messages not echoed; the rate limit; download bytes,
headers, closed type and name; the archive equals the export of the pinned
revision; a capability that is not declared cannot be granted),
`runtime_policy.rs` and `runtime_sessions.rs` unit tests (capabilities are
distinct names, a grant permits only what it names), `runtime_tests.rs` (a
grant revoked while a listing, a read, or a download is held in the service
rejects its late result), the client tests in
`scripts/tests/client.test.mjs`, `scripts/tests/runtime-requests.test.mjs` (the
workspace's message parser), `scripts/tests/runtime-bootstrap.test.mjs` (the
shipped frame script), and `scripts/smoke-source.mjs` in Chromium and Firefox
(real server, sandboxed frame, the user's confirmation, and the bytes compared
with the stored revision and its export).

## Alternatives

- **One `source` capability.** Reading source and replacing it are different
  trust. A separate edit grant makes the consent text honest and keeps a
  read-only app read-only.
- **A frame-supplied base revision.** It would let an app overwrite changes it
  never saw; the pinned base makes a stale save a conflict.
- **A frame that triggers the download itself.** The sandbox's
  `allow-downloads` would permit a file the frame made up, but not the stored
  source, and a silent save of stored bytes is what the user must decide.
- **A separate frame identity for attribution.** Right in the end, but it
  needs the identity model of the security rows; this record attributes to the
  user and says so.
- **Forwarding server error messages.** They can contain the frame's own
  input and make a probe; a closed code set cannot.

## Costs

Three more names the policy, the sessions, the consent screen, and the SDK
must keep in step; four routes; a workspace that holds a download question
and the save state of a frame; a save that is indistinguishable from the
user's; and a grant that the user must renew after the source moves.
