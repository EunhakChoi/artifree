# 0033: Runtime source writing

Date: 2026-11-03. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) (the preview sandbox),
[0007](0007-live-web-workspace.md) (the workspace follows commits),
[0017](0017-code-workspace.md) (package paths),
[0020](0020-runtime-grants.md) (the grant model), and
[0032](0032-runtime-source-read-and-download.md) (frame source calls and their
budget). Roadmap row: R11b. Independent Artifree design: the roadmap row gives
the capability name and the requirement that the saved bytes be compared with
the stored ones. The route, the bounds, the key namespace, and every error below
are Artifree's own decisions. Nothing below is an observation of the original
product.

## Context

R11a let a preview read its own source and ask for a download. An app that edits
(a note, a small document, a saved setting kept as a file) also needs to save.
This record is R11b: **one sandboxed frame saves one file of its own artifact,
under an explicit edit grant**, through the same service every other writer
uses. It reuses the 0020 model (declaration, support, consent, a connection
grant in memory), the server's re-check of binding, grant and head on every
frame call, and the frame budgets of 0032. It adds no persistent state and no
new way to write that skips a base revision or an idempotency key.

## Decisions

1. **One capability with its own consent.** `source.write` is declared in
   `artifree-runtime.json`, supported by the server, consented by the user in
   the workspace, and granted per connection in memory. `RuntimePolicy::supported`
   is now `metadata.read`, `source.download`, `source.read`, `source.write`.
   The workspace shows its own control, "Allow source editing" (and "Revoke
   source editing"), beside a sentence that says what the grant lets the app do:
   _change files of this artifact; a save creates a new revision without asking
   again; earlier revisions stay and can be restored; after a save the preview
   is out of date and editing must be allowed again for the next one._ The
   sentence is tied to the button as its accessible description. A grant never
   implies another: reading does not allow writing, writing does not allow
   reading, and the existing controls are unchanged.
2. **One route.** `POST /v1/artifacts/{id}/revisions/{n}/runtime-views/{view}/{generation}/source/write`
   with `{ initialization, retry_id, path, content }`; `content` is base64 of at
   most 1,048,576 bytes (at most 1,398,104 characters). It saves **one file per
   call**: a new path adds a file, an existing path replaces it. The answer is
   `{ path, revision, size, sha256 }`. This route, and only this route, has its
   own body limit of 1,572,864 bytes; every other frame route keeps 16 KiB. The
   limits are published in the generated schema (`content` has `maxLength
1398104`, `retry_id` has `minLength 8` and `maxLength 36`), and a test holds
   the constants and the schema together.
3. **Authorization first, then the arguments.** Order: the shape of the route
   names (`400`), the binding (`404`, one answer for every mismatch, as in 0032),
   the connection's current grant of `source.write` (`403`), and the live
   artifact (`404` for trash). Only then the path (package rules, shared fixture
   `contracts/fixtures/package-paths.json`), the retry identity, the base64
   (canonical form only) and the size, each `400`/`413`/`422`. A caller without a
   binding or the grant learns nothing from how its arguments were judged. The
   binding and grant are checked again **after** the commit.
4. **A conditional save through `ArtifactService`.** The server calls
   `ArtifactService::save` with the view's revision as the **base revision**,
   one `put` of the file, and a request key (decision 5). The head is not
   checked before; the commit is conditional on the base, so the fence is
   atomic with the write. A view that is not the head gets `409
revision_conflict` with `current_revision`; bytes equal to the file's
   current bytes give `422` (nothing is saved: a revision that changes nothing is
   refused by the package rules); a path or package that breaks the rules gives
   `422`. A viewer is not a case here (decision 9).
5. **The idempotency key is derived, in a namespace no text can produce.**
   `rtw~{artifact}:{view}:{generation}:{retry_id}`, built only by
   `RequestKey::runtime_write` in `artifree-domain`. The `~` is a character the
   public key grammar (8 to 128 ASCII letters, digits and `-_.:`, which
   `FromStr` parses for the HTTP header, the MCP tools and the CLI) does not
   accept, so no text input can parse to a frame's key and the grammar itself
   is unchanged: every key that was valid before this decision is still valid,
   `rtw:...` ones included, and none of them is a frame's key. An owner or an
   agent therefore cannot send a key that lands in a frame's namespace or
   reuse the key of a frame's retry. Reserving a prefix inside the grammar was
   rejected because it would refuse keys that are already valid. The stored key is read back only as a bound SQL value, never
   parsed again, so a frame's key written before a restart is found by its
   retry after the store is reopened. The frame supplies only `retry_id`: 8 to 36 characters of
   `A-Za-z0-9_-`. The parts that make the scope (artifact, view, generation) must
   be what they claim, so that no choice of `retry_id` can blur one scope into
   another; the longest key is 127 characters. The store still scopes keys per
   actor. Consequences, each a test: the same retry identity in another
   artifact, view or generation is another request; a retry with the same
   bytes gets the **first outcome** back (the store replays the receipt by key
   even though its own commit moved the head); the same identity for other
   bytes or another path is `422` (`request_key_reused`); another identity from
   a view that is no longer the head is `409`.
6. **The revision is read back and compared.** In the same task as the commit
   the server reads the new revision and checks, in a pure function (`judge`),
   that: its number is the one after the base; every other file is as it was
   (path, size, digest); the written file is listed with the size and the digest
   of the bytes sent; and its stored bytes equal the bytes sent. A mismatch is
   `500 internal` with a fixed message and does not say which check failed; the
   revision exists, and the message says to check the history. This is the
   server's own check of its store, not of the frame.
7. **Three more checks of the receipt, none believing a number.** Each side
   compares the receipt with what **it** sent or received:
   - the **client** (`writeSource`) computes the SHA-256 of the bytes before it
     sends them, with `crypto.subtle`; a platform that cannot refuses the save
     (`unavailable`) instead of sending a write it could not verify. It accepts a
     receipt only if the path is the one asked for, the revision is the view's
     plus one, the size is the number of bytes sent and the digest is its own;
   - the **host** (the workspace) sends the frame only what the client accepted,
     and never a server error text;
   - the **frame script** hashes its own bytes with a small pure-JavaScript
     SHA-256 (the sandboxed page has no guarantee of `crypto.subtle`) and
     accepts a receipt only for the path, size and digest it sent and a
     revision that is an integer of at least 2. It cannot know the view's
     revision, so it cannot check "plus one"; the client did.
8. **A grant is for the head it was given at, so a view saves once.** The
   grant is scoped to `(artifact, revision, head)` (0020). A save moves the head.
   The workspace follows the new revision (0007) in a **new view that holds no
   grant**; the user must allow editing again. A second save with another retry
   identity from the old view is `409`. This is a chosen behavior, not an
   accident of the model: one consent covers one revision made, never an open
   ended editing session. The retry of the first save is still answered with
   its receipt while that view and connection live (decision 5), so a frame that
   lost the answer can recover it without making a second revision.

   Amended 2026-11-11 by [0054](0054-runtime-view-adoption.md): for a page
   whose manifest says `"lifecycle": "adopt"`, the workspace does not load a
   new view after a save. It replaces the frame's view in place with a
   generation that pins the saved revision and holds no grant (0054, rules 8
   and 12). The consent rule is unchanged: the user must allow editing again.
   - **Rejected: re-scoping the grant to the new head after each save.** One
     click would then let a frame rewrite the artifact repeatedly and
     unobserved.
   - **Rejected: a confirmation prompt for each save.** The prompt of 0032 fits
     a rare download of a named file; a save loop would train the user to
     accept, and the bytes the frame chose cannot be shown in a form the user
     can judge. The permission control and its sentence are the consent; this is
     the cost of decision 1's wording, so it must stay exact.
   - **Rejected: a count or a time limit on one grant.** It adds a rule the user
     is not shown.

9. **Who may write: the owner, and that is all there is.** Artifree has one
   actor; agent tokens act as it. There is no viewer or editor role, so
   "viewer saves are refused" cannot be enforced and **is not claimed**. What is
   enforced: a view is bound to the actor that opened it, so any other actor's
   token gets `404` and nothing is saved (a test with a second actor). When
   roles exist (S02) the write route must check them in addition to the
   grant, and the permission copy must change.
10. **A grant revoked after the write was authorized does not stop it.** The
    commit is a conditional write that does not look at the grant again, and it
    is durable once made. If a revoke, a close or a replacement landed meanwhile,
    the server withholds the receipt (`403`) and the frame is told `denied`
    although the revision exists; the owner sees it in the history. The change
    feed is told of the commit either way. A test holds the store's worker
    thread so that the revoke lands after the write was authorized and before it
    finished.
11. **Budgets and failures are those of 0032.** The write is a frame call: it
    costs a token of the host's bucket and a waiting slot (at most 16), the
    server's separate frame limiter admits it, and the web request gate classes
    it as a frame call, not an owner write (a frame flood does not hold the
    owner's saves). The frame waits 30 seconds, then the script cancels and the
    workspace aborts the request; a commit already made is not undone, and the
    retry of decision 5 recovers its receipt. The frame sees only the closed
    set: `409 revision_conflict` is `stale_view`, `403` `denied`, `404`
    `not_found`, `429` `rate_limited`, `400/413/422` `invalid_request`,
    a replaced connection `expired`, everything else `unavailable`.
12. **The frame API.** `window.artifree.source.write({ path, content, retryId })`
    with `content` a string (saved as UTF-8) or a `Uint8Array`, at most 1 MiB,
    resolves with `{ path, revision, size, sha256 }`. It never throws
    synchronously; arguments are copied once, property by property, with
    errors caught; the bytes are copied and transferred, so the frame's own
    array is not shared with the page. A frame must make its `retryId` unique
    to what it means to save and reuse it only to retry the same save.
13. **Surface.** Contract version 0.11.0: `RuntimeSourceWriteRequest`,
    `RuntimeSourceWriteReceipt`. `ArtifreeClient.runtimeCall` is public so that
    the workspace's save lives in the on-demand module (`runtime-source`) and
    not in the client class, which is in the main bundle and has no room. **No
    MCP tool was added or changed**: a frame's capability is a connection
    grant to the page, an agent's tools are the owner's or a token's, and they
    do not share a permission.

## Limits and what is not implemented

- **Implemented:** the above, in the web workspace with a bound frame. The main
  bundle stays under Vite's 500 kB warning (499.99 kB, the same as before this
  change; the margin is nil and the limit was not raised): the save, its
  digest, and the host's rules are in on-demand chunks, and the host does not
  import a value from the client, because a module shared by two on-demand
  chunks would put its preload list in the main bundle. A test holds the host's
  retry rule and the client's to the same answers.
- **Not implemented:** subscriptions to
  changes (R07: a frame cannot learn of a commit except by its own receipt);
  frame identity or per-frame permissions beyond the connection (S01); viewer
  or editor roles (S02); deleting or renaming a file, saving several files in
  one revision, and streaming or resuming a large write; a prompt for each save;
  continuing to save after one consent; a mark in the history that a revision
  came from a frame; data access from frames and uploaded assets (R06 stays
  server-side, R09).
- **The sandbox is not network isolation.** A frame that may read and write can
  still navigate itself or load resources; this record makes no claim that it
  cannot send what it read elsewhere. `source.write` lets the frame put any
  bytes into any package path the rules allow, including `index.html`; the
  control says so, and earlier revisions can be restored.
- A frame can make at most one revision per consent, but a user can consent
  repeatedly; nothing limits how many revisions a frame causes over time except
  the host and server budgets of 0032 and the store's own limits.
- The frame cannot tell whether the answer it gets is the first or a replay.
- The grant controls are in the preview panel. There is no log of what a frame
  wrote besides the revision itself.

## Tests

Rust: `artifree-domain` (keys that start `rtw:` parse as before, no text parses to
a frame's key, scopes differ, the longest key, parts that blur a scope are
refused);
`crates/artifree-server/tests/runtime_source.rs` (the new revision has exactly
the bytes sent; a new path adds a file and the largest binary file survives;
writing and reading are separate consents in both directions; a missing binding
or grant changes nothing; authorization before the judgement of arguments;
arguments that break the rules save nothing; the shared path fixture; a view
that is not the head conflicts; a retry gets the first outcome and the view
commits once; the same retry identity elsewhere is another request; no key an
owner or agent can send reaches the frame namespace, and `rtw:` keys, the old
spelling of a frame's key included, are ordinary keys; a frame's retry is found
again after the store is reopened; text over the base64 limit is refused before
it is decoded; the write body limit is
this route's only; a trashed artifact), `runtime_tests.rs` (a revoke during the
write withholds the receipt and not the revision; another actor cannot write
through the owner's view), the pure `judge` cases of `runtime_source.rs`
(accepted, and each mismatch), `runtime_policy.rs`, and
`crates/artifree-contracts/tests/wire.rs` (the schema bounds equal the
constants).

TypeScript (`npm run test:tooling`): `runtime-source-client.test.mjs` (the
request, a receipt for other bytes refused in each field, invalid input never
sent, no digest means no send, `409` with the current revision, a replaced
connection, the bounds equal the schema's), `runtime-bootstrap.test.mjs` (the
digest equals the platform's, a private transferred copy, a receipt judged by
the bytes sent, hostile arguments, the path fixture, a closed failure set with
timeout and cancel), `runtime-host.test.mjs` (the call parser, the retry rule
equal to the client's, the rpc over the real client with a fake network: the
bytes and the receipt, a wrong receipt, each refusal as its word, cancel, the
budget), and `request-gate.test.mjs` (the route is a frame call, not an owner
write, and look-alikes are not).

Browser: `scripts/smoke-source.mjs` in Chromium and Firefox: a save without the
grant is refused; arguments that break the rules are refused and make no
revision; with the grant a save makes revision 2 whose bytes and digest equal
what the frame sent while revision 1 and the other files are unchanged; the
workspace shows the next view without the grant, and a second consent saves
revision 3; the permission sentence is the button's description. Over the
protocol with the real client: a view issued before the grant is refused, a
retry replays, the same identity for other bytes is refused, another identity
loses to the moved head with the current revision, and an old generation is
gone.

## Alternatives

- **Writing through the agent path.** It would give the frame an account token
  or an agent's key space; the frame holds neither.
- **A key chosen by the frame.** It would let a frame replay or collide with
  the owner's keys and an agent's. The derived namespace is closed to the
  constructor that owns it.
- **Several files or a delete in one call.** Each adds a rule to judge and to
  read back; one file per call is enough for the first version.
- **Trusting the server's receipt.** A lie about a size or digest would be
  taken as proof. Each side hashes what it sent.

## Costs

One more route with its own limit, a pure-JavaScript SHA-256 in the frame
script (the size of the bootstrap grows), a read-back after every save, a
consent that covers one revision and must be repeated, a new view after every
save, and the promise that the key namespace, the receipt checks and the
permission sentence stay exact.
