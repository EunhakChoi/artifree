# 0045: Uploaded assets scoped to a user and artifact

Date: 2026-11-06. Status: partially implemented; **R09a is incomplete; R09b1 supplies read-only client calls and R09b2 the bounded upload/delete client calls; the remainder of R09b–R09d is proposed**.
The current HTTP/storage slice accepts only UTF-8 plain text/CSV.
PNG and JPEG are refused as `asset_type_unsupported` until complete image
validation satisfies rule 3. A narrower PNG subset does not satisfy that gate.
The four-type target below remains unchanged. The read-only TypeScript client
lists assets, reads metadata and verifies bounded content; `uploadAsset` and
`deleteAsset` perform one explicit mutation each (R09b2). There is no CLI, MCP
tool, copy extension, management panel, grant or frame access yet. Builds on [0003](0003-local-html-artifact-slice.md),
[0012](0012-duplicate-artifacts.md), [0020](0020-runtime-grants.md),
[0027](0027-backup-restore-and-schema-upgrade.md),
[0029](0029-retention-and-cleanup.md), [0031](0031-app-data.md),
[0032](0032-runtime-source-read-and-download.md),
[0033](0033-runtime-source-write.md), and [0036](0036-multi-user-identity.md).

## Context and evidence

Source files, application data, and uploaded files have different lifetimes.
R09 gives an application a durable reference to an uploaded file without
adding it to source history or making a content digest an access credential.
The agreed initial scope is the current authenticated user and artifact.
Sharing roles and cross-user uploader rules belong to S02.

A private trace follows the latest analysis to original controlled records.
The observations below describe that product's captured behavior, not behavior
implemented in Artifree or a specification to reproduce its internals.

| Requirement                   | Evidence and limits                                                                                                                                                                                                                          | Independent proposal                                                                                                                                                                 |
| ----------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| A1 Durable bytes and metadata | Observed: an uploaded CSV returned the input bytes; a separate 40-byte upload survived a new view and an owned-browser restart. Indefinite retention was not established.                                                                    | Immutable bytes in the existing SQLite database; authenticated reads and bounded metadata.                                                                                           |
| A2 Type and resource limits   | Observed: empty, HTML, invalid UTF-8 text, and a script beginning with markup were refused. One fixture reported 5,000 files/1 GiB; that is not a universal quota. Source-only guide limits differ by type.                                  | A smaller initial allowlist and independent quotas, specified below.                                                                                                                 |
| A3 Sanitization               | Observed: one SVG lost script, event handlers and foreign content while retaining a rectangle. This does not prove a general sanitizer's safety.                                                                                             | Reject SVG initially; do not implement sanitization with regular expressions or assume an image loading context makes all SVG safe.                                                  |
| A4 Scoped access              | Observed: the old asset ID worked inside an authorized duplicate. Source-only roles described uploader restrictions. Another authenticated uploader was unavailable.                                                                         | Check the actor, artifact and scoped association on every read and write, tested with Artifree's real users. No sharing role is introduced.                                          |
| A5 Copy and lifecycle         | Observed: a seeded duplicate copied data and assets, including an unreferenced upload. An optional skipped-assets response exists in source only. A warm cached read after deletion returned 200, while a cache-bypassing read returned 404. | Explicit `copy_assets` opt-in preserves Artifree's existing files-only default; copied associations are independent. No-store reads and server-side removal have a defined boundary. |
| A6 Retry, storage and grants  | Original backend transactions, quota races, key retention and the precise asset revocation protocol are not established.                                                                                                                     | Existing application service, immediate transactions, actor-scoped replay, S01 identity and R05 connection grants determine these rules.                                             |

All numeric limits, errors, routes, state versions and implementation slices
below are Artifree decisions. This is not a formal clean-room separation.

## Current implementation boundary

The current slice provides the five authenticated HTTP operations in rule 7,
through `ArtifactService`, synchronous ports and SQLite schema 12. Asset bytes
share `blobs` with source files; both reference types protect reclamation.
Asset versions, metadata-only receipts, actor-wide keys, atomic logical quotas,
trash/recovery/purge and backup/restore implement the storage behavior of rules 1–9;
image admission remains disabled under rule 3. Upload preflight closes
its read snapshot before content validation, and final immediate settlement
rechecks authority, key and base before using the saved validation result.

Only `text/plain` and `text/csv` currently pass new-upload admission. All PNG
and JPEG uploads, valid or malformed, are `asset_type_unsupported`. No image
decoder is linked. R09a remains incomplete until PNG and JPEG full-format
validation satisfies rule 3; the target allowlist and bounds below are unchanged.
See the [content policy](../asset-validation.md).
Stable MIME envelope parsing and historical metadata/receipt decoding still
accept image type spellings. A currently authorized matching historical upload
replays before the current text-only admission policy, including after deletion,
and never recreates the bytes. A fresh image key is refused without a receipt.
Generated request/response DTOs exist for HTTP. R09b1 supplies read-only
`listAssets`, `getAsset` and `readAsset` functions using those DTOs, strict
runtime metadata validation and bounded streaming responses (64 KiB JSON,
4 KiB errors, content no larger than its validated metadata and 5 MiB).
Received chunks are copied immediately into one owned growable buffer bounded
by that byte limit; retained buffers do not grow with the number of chunks.
Calls refuse redirects and bypass caches; content must match its metadata
MIME, size and SHA-256. Web Crypto is required for digest verification;
its absence or failure rejects the read. These are authenticated management
client calls, not URLs or frame capabilities. A content read first fetches
metadata, then bytes: deletion or authority loss between them refuses the
read, while unrelated changes may advance the asset space. The returned
`asset_version` belongs to the metadata snapshot, not a combined snapshot.
No automatic retry, pixel decoding or content execution is introduced.
R09b1 depends only on the already verified text HTTP/storage behavior and
adds no new admission or authority semantics. Full R09a image validation and
the remainder of R09b remain incomplete.
R09b2 implements the TypeScript upload/delete calls (see its section below):
each makes one request with the caller's key, requires exactly 201 or 200,
checks the receipt against the captured request and never retries.
Direct callers, and callers of those functions, must freeze the identical
envelope and key themselves after an unknown outcome. CLI/MCP upload and
delete, `copy_assets` and the unknown-outcome handling of other adapters remain
R09b.
No asset change subscription is emitted. Asset copying remains absent and
existing duplicate requests retain their files/data behavior.

## Target rules (partially implemented)

1. **A space and its version.** Each `(actor, artifact)` has an asset space.
   It is readable or mutable only while that actor may access the live artifact
   under 0036. Missing, trashed, purged and another user's artifacts are `404
not_found`, including on a retry. No actor identifier is accepted from a body,
   query or frame. `asset_version` starts at 0 without a stored space and
   increases once per changed upload or deletion; an empty space keeps its
   version. Source save/restore, metadata changes and application-data writes
   neither rewind nor advance it. Upload and delete require
   `base_asset_version`, the base revision of this independent state, and an
   idempotency key. They do not change or require a matching source revision.
   A runtime call additionally requires the current source binding (rule 12).

2. **Identity and bytes.** An upload issues a cryptographically random 256-bit
   opaque asset ID, encoded as 64 lowercase hexadecimal characters, unrelated
   to its SHA-256 digest, name or owner. IDs are immutable and never reused;
   collision detection and retry happen before committing. Uploading equal
   bytes twice with different keys creates two associations and consumes two
   quota entries. The durable pointer is the asset ID interpreted within its
   actor/artifact space, never a global blob URL. Metadata contains the ID,
   normalized media type, byte size, SHA-256 and creation time. There is no
   user-supplied filename in this first version. Accepted bytes are preserved
   exactly, including permitted image metadata; no metadata stripping or
   malware-free guarantee is claimed.

3. **Initial type policy.** The complete allowlist is `text/plain`, `text/csv`,
   `image/png` and `image/jpeg`. The request declares exactly one lowercase
   type without parameters. Text must be valid UTF-8; reject NUL and control
   characters except tab, CR and LF. Text is opaque content, never inserted
   into management DOM or executed, and CSV formula neutralization is not
   promised. Images must pass a maintained decoder's complete format
   validation within bounded allocation: PNG or JPEG matching the declared
   type, nonzero dimensions, at most 4,096 pixels per axis and 16,777,216 pixels
   total. Refuse animation, malformed/truncated files and trailing bytes after
   the format end; a signature match alone is insufficient. PNG animation
   chunks are refused. Do not decode image pixels in the TypeScript client.
   The R09a dependency review must record the chosen Rust decoder, version,
   allocation limits, supported format features, license and advisory results;
   only these two format decoders are enabled. Failure to meet these bounds
   blocks that implementation rather than silently relaxing validation.

   SVG/XML, HTML, JavaScript, CSS, JSON, Markdown, PDF, fonts, archives,
   audio/video, GIF/WebP and unknown types are refused, regardless of suffix.
   Upload accepts bytes, not remote URLs, filesystem paths or `data:` URLs;
   there is no server-side fetching. A caller can still hold markup as plain
   text: the protection is the fixed type, attachment disposition, isolation
   and non-execution, not a claim that content detection proves benign text.
   SVG support requires a separate specification amendment with a reviewed
   Rust sanitizer or rasterizer, license provenance and adversarial fixtures
   for namespaces, entities, external resources, CSS, events and foreign
   content. If it transforms bytes it must define pre/post-transform bounds,
   fingerprints, stored digests and clone compatibility before enabling SVG.

4. **Quotas and request bounds.** One asset is 1 byte–5 MiB inclusive; a space
   has at most 64 assets and 25 MiB; an actor has at most 256 assets and
   100 MiB across all its spaces, trash included. MiB means 1,048,576 bytes.
   Counts and byte usage charge each live association at full logical size,
   including copies and equal-byte uploads, regardless of physical deduplication.
   These deliberately small initial limits bound reads and lists and fit the
   existing 5 MiB source-file scale. The actor cap prevents creating new
   artifacts to evade the per-artifact quota. No global disk quota or promise
   about database/WAL/backup size follows from these limits.
   A JSON upload body is bounded at 8 MiB before parsing, with strict bounded
   base64 decoding and the decoded 5 MiB check before image validation.
   An empty decoded payload passes this envelope step; a new upload rejects
   it as `422 invalid_asset` at the content-validation step of rule 5.
   Overflow-safe counters and allocation bounds apply before allocation.
   Authentication, route identity and body bounds precede expensive decoding;
   shared request admission limits still apply. Rate rejection does not prove
   that an earlier attempt failed to commit.

5. **One application transaction.** HTTP, MCP, CLI, management UI and future
   runtime calls use `ArtifactService`; the synchronous core and port remain
   independent of Axum, Tokio and the concrete SQLite provider. Validation is
   Rust logic; wire types alone do not authorize a request. SQLite remains the
   only asset-byte store. Reuse the `blobs` table for digest deduplication,
   adding indexed asset associations and state, opaque-ID reservations, and
   durable operation receipts through the schema upgrade owner.
   Upload admission has three phases. First parse the stable, bounded v1
   envelope: field/key/base types, syntactically valid lowercase parameter-free
   MIME spelling, strict base64 and the 8 MiB body/5 MiB decoded bounds. Compute
   the fingerprint from the original declared type and exact decoded bytes,
   without invoking a content decoder, sanitizer or current admission allowlist.
   These parsing rules and historical type spellings stay accepted for v1
   replay if a later release tightens content policy; a policy rejection is
   not an envelope parse failure. Malformed or transport-oversized requests
   never reach replay lookup. Validator versions and validation outcomes are
   not fingerprint inputs.
   Retiring an upload type does not remove its historical spelling from the
   generated request/receipt wire contract or stored-receipt decoding.

   Next perform a short, read-only snapshot of live ownership, the optional
   runtime source-head fence (rule 12), and the actor-wide persistent key space.
   In that order, an authority refusal wins over replay, a matching key returns
   its original receipt, a reused key is refused, and a new key with a stale
   base is refused. A new admissible key reserves nothing. Close this snapshot
   before checking the current type allowlist, nonempty content, UTF-8 or image
   format/allocation policy. Do this potentially expensive validation outside
   any database transaction or write lock, keeping its typed success candidate
   or rejection in memory. A matching replay never invokes that validation,
   even when the current decoder or allowlist would now refuse the bytes.

   Finally settle that candidate **or rejection** in one `BEGIN IMMEDIATE`
   transaction: recheck live ownership and any runtime head fence, resolve the
   same shared key space again, replay a matching receipt or refuse a reused
   key, then compare the base. Only for a still-new key at the matching base
   report the saved validation rejection, or, for valid content, recompute
   resulting space and actor quota, insert associations and blobs, advance the
   version and save the receipt. Delete uses this same transactional authority,
   key and base order without content validation. The key space includes
   artifact, cleanup, job and data operations. Preflight is an optimization,
   not a key reservation or proof of commit authority. If two first attempts
   both saw an unused key, only the final transaction decides: the matching
   loser replays the winner even if its validation outcome was rejection; a
   different fingerprint loses with `request_key_reused`. No persistent pending
   receipt, staging blob or quota reservation is written during validation.
   Every rejection and transaction rollback leaves no new key, counter, blob
   or association; concurrent distinct keys cannot both consume the last quota
   slot. S01's existing authenticated-before-disable stored-write semantics
   remain in force; no stronger mid-request cancellation is claimed. These
   asset-specific phases do not change existing operations' validation order.

6. **Retry and deletion.** Fingerprints cover operation, artifact, base,
   declared type and exact decoded bytes for upload, or asset ID for delete,
   using the existing unambiguous length-prefixed hashing scheme. A matching
   key replays the original metadata-only receipt before the now-stale base,
   changed quota or mutable content policy is considered, after the stable
   envelope and current authority checks in rule 5. It creates no extra association, charge or
   version. The receipt reports its original `asset_version` and `replayed`;
   it is not a statement of current availability. After a later asset deletion,
   replaying its original upload returns that original receipt without
   recreating the asset; a content read is 404. Deleting an absent or foreign
   asset ID within an accessible space succeeds with `deleted: false` and
   records the key without advancing the version, after checking its base.
   A delete replay retains its first `deleted` result. A reused key is
   `422 request_key_reused`; a stale new request is `409 asset_conflict` with
   `current_asset_version`. Lost replies are unknown outcomes: retry the
   identical frozen request and key. No R08 data-outbox reuse or automatic
   upload retry is introduced. Receipts persist indefinitely as other request
   keys do, containing no bytes or filename. Their request fingerprints can
   confirm a guessed input to someone holding the database, as in 0029.

7. **HTTP surface and bounded clients.** Proposed API routes:

   | Method and route                                   | Result                                                                                                                                                                      |
   | -------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
   | `GET /v1/artifacts/{id}/assets`                    | `200`: asset metadata, `asset_version`, and this space's usage/limits, all from one snapshot. At most 64 entries, ordered by asset ID; no pagination needed under that cap. |
   | `POST /v1/artifacts/{id}/assets`                   | Body `{base_asset_version, content_type, content_base64}` plus `Idempotency-Key`; `201` with metadata, version, replay flag and an authenticated resource `Location`.       |
   | `GET /v1/artifacts/{id}/assets/{asset_id}`         | `200`: immutable metadata and current space version after a scoped lookup.                                                                                                  |
   | `GET /v1/artifacts/{id}/assets/{asset_id}/content` | `200`: raw bounded bytes with the stored type, never an external redirect.                                                                                                  |
   | `DELETE /v1/artifacts/{id}/assets/{asset_id}`      | Body `{base_asset_version}` plus key; `200` with ID, version, `deleted`, replay flag.                                                                                       |

   No global digest/asset route or unauthenticated serving URL is added. Invalid
   route identifiers are 404; unknown fields, malformed JSON/base64/MIME spelling or missing
   key are 400; outer-body overflow is `413 payload_too_large`. For a new key,
   current type/content policy is evaluated only after rule 5's preflight;
   a concurrent committed receipt takes precedence at final settlement.

   | New-upload rejection                                             | HTTP status and code         |
   | ---------------------------------------------------------------- | ---------------------------- |
   | Declared MIME type outside the current allowlist                 | `422 asset_type_unsupported` |
   | Zero decoded bytes under a supported type                        | `422 invalid_asset`          |
   | Invalid UTF-8, prohibited text controls or invalid image content | `422 invalid_asset`          |
   | More than 5 MiB decoded, or count/space/actor quota overflow     | `422 asset_limit_exceeded`   |

   Exactly 1 byte meets the byte-size minimum but must still pass its type's
   content validator; exactly 5 MiB meets the per-asset byte maximum. An authenticated foreign artifact with a
   well-formed request is indistinguishable from a missing artifact. Syntactic
   validation need not conceal whether that request itself is malformed.
   Existing 401, 429/`Retry-After` and storage-unavailable errors keep their
   semantics; runtime authority refusals follow rule 12.

   All success and failure responses, including content, use `Cache-Control:
no-store`, `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`.
   Content also uses `Content-Disposition: attachment` with a server-generated
   safe name and `Content-Security-Policy: sandbox; default-src 'none'`.
   No 304, range, public CORS, inline management-origin rendering, URL token or
   hash-addressed content endpoint is introduced. Raw content reads are at
   most 5 MiB; client metadata/error reads are bounded at 64 KiB/4 KiB.
   The client rejects redirects and checks generated response shapes, echoed
   IDs, safe version/size numbers, MIME enum, received byte count and digest.
   It does not reimplement the Rust image or text validator. Contracts are
   generated with `npm run generate:contracts`, never edited by hand.

8. **Trash, purge and shared blob references ship with storage.** Trash retains
   assets, quota charges and versions but denies access; recovery restores
   access without rolling the asset state back. Asset delete removes only its
   space association. Purge removes every asset association and space for the
   artifact, within the existing erasing transaction. It scrubs asset receipt
   metadata and blanks request fingerprints, retaining the actor/key
   reservations permanently, as 0031 does for data requests. A blanked
   fingerprint cannot match any request. Non-content asset ID reservations
   also remain to prevent ID reuse. It still replays the purge under 0029;
   asset calls naming the purged artifact are 404, whether the key is the same
   or different. Reusing that old key for another accessible artifact or any
   other operation is `422 request_key_reused`, never a new write or a replay
   of scrubbed metadata. A retried old key cannot resurrect content or become
   available again after purge.
   Every blob-reclamation predicate must count **both** `revision_files` and
   live asset associations, not just the new asset path. This includes
   `remove_files`, asset deletion, purge, unreferenced dry run/apply and job
   cleanup paths that can release source-backed blobs. A blob survives as
   long as either source history or an asset/copy refers to it. Reference
   removal and reclamation are in the same transaction. No secure erasure of
   WAL, filesystem pages, previous backups, downloads, browser memory or
   caller-retained bytes is promised. Deletion/revocation prevents subsequent
   authorized reads; it cannot take back bytes already returned.

9. **Backup and schema compatibility ship with storage.** Assets, logical
   ownership, versions, quotas derivable from associations, ID reservations
   and replay receipts live in the same SQLite snapshot as source and data.
   O04 backup/restore and its structural validation must include them; an
   upload cannot commit one half outside that snapshot. The next available
   schema upgrade is allocated when R09a starts, not reserved by this proposal.
   Every previously supported schema upgrades to empty asset spaces without
   changing existing requests, source or data. A refused upgrade changes
   nothing. Restore preserves asset bytes, scope, digest, version, retry and
   quota behavior; its new feed identity resets clients as today. No new
   filesystem blob directory, object store or backup format is introduced.
   O06's temporary database naturally contains these bytes and leaves none
   in persistent mode's data directory; its existing secure-erasure limits
   remain. A07 source exports and source history do not include assets.

10. **Explicit independent copies.** R09b adds `copy_assets: boolean = false`
    to the existing duplicate request, independently of `copy_data`. Omission
    and explicit false preserve the old fingerprint and files-only default;
    true adds a distinct fingerprint component. This is an Artifree
    compatibility decision: the observed seeded original copy included assets
    by default, and that observation is not rewritten as an opt-in result.
    When true, the duplicate copies the requesting actor's entire current
    asset space, including uploads not referenced in source or data, in the
    same transaction as its selected source revision and optional current data.
    Historical source selection does not imply historical assets or data.
    It retains asset IDs in the new scoped associations, so byte-preserved
    source/data references continue to resolve; it copies no absolute URL or
    account-wide right. New uploads still receive new random IDs.
    The copy starts `asset_version` 1 if nonempty, otherwise 0. It charges
    full destination quotas and actor quota, even when blobs are shared;
    overflow refuses the entire duplicate, recording no key or partial copy.
    No partial-success/skipped-assets mode is added. `copied` reports
    `assets` when requested, including an empty space. The original receipt
    replays after the source changes or is purged, provided the copy remains
    accessible, following 0012/0029. A default duplicate, a requested asset
    copy, and data copying are described distinctly in every caller.
    Subsequent upload/delete/purge in either artifact cannot affect the
    other's associations. Grants, tokens, other actors' spaces, outbox rows,
    connector credentials and secrets are never copied.

11. **Management adapters before frames.** R09a supplies core/provider/HTTP
    behavior and lifecycle tests. R09b adds generated TypeScript client calls,
    CLI `asset` list/upload/read/delete, MCP `list_assets`, `read_asset`,
    `upload_asset`, `delete_asset`, and the copy extension. All use the same
    application service and limits; mutating MCP tools are destructive and
    uploads use encoded bytes, not arbitrary host paths. CLI upload/read may
    access only the path explicitly supplied by its operator, never by a
    remote MCP caller; stdout remains machine-readable, diagnostics on stderr.
    R09c adds a management assets panel with explicit copy controls, usage,
    bounded uploads, downloads, deletion and unknown-outcome retry. Text is
    escaped, assets are downloaded as attachments, and no asset is executed
    or rendered inline in the management origin. An upload button or SDK type
    is not evidence of implemented frame access.

12. **Separate runtime authority in R09d.** Only after the preceding behavior
    is verified, add `assets.read` (list/metadata/read bytes), `assets.write`
    (upload/delete), and `assets.download` (a user-confirmed device download).
    They have separate declaration, support, consent and connection grants;
    none implies another. Do not add a frame asset URL or give it an account
    token. The existing bound host bridge carries calls to the same service.
    Upload is capped at 1 MiB of decoded bytes per frame call, within the
    ordinary store limit; bytes use the existing bounded bridge encoding.
    Read parts are at most 256 KiB with offset/length, immutable asset ID,
    total size and digest. Every side checks the echoed binding, byte count,
    offsets and final digest before reporting assembled content. The server
    checks the actor/artifact/connection/view/generation, declared grant,
    pinned revision and observed current source head before work and again
    before returning an asynchronous read. Check live association too: deletion
    while a read is pending must refuse returning that read's bytes. Source
    replacement, grant clear/regrant, connection close/expiry, logout or S01
    user disable cannot revive an old binding. A failed authority check occurs
    before an idempotency receipt is exposed; no old runtime key bypasses it.

    Runtime mutations use the independent asset base and the established
    server-only runtime-key construction with a new disjoint operation tag,
    including artifact, connection/view generation and frame retry ID;
    frame keys cannot collide with public HTTP/MCP/CLI keys or source writes.
    Validate authority again immediately before starting the synchronous
    mutation, using the existing per-view coordination. A runtime mutation
    also passes a typed expected-source-head fence into the store; live
    artifact access and that head are checked in its write transaction before
    key replay or mutation. A changed head is `409 revision_conflict`, so a
    source save between dispatch and storage cannot admit a stale frame write.
    This runtime-only fence does not change direct asset callers' independent
    base-version rule. A grant change that
    wins first prevents dispatch, while a commit already admitted cannot be
    undone by later revocation. No claim of cancelling an in-flight database
    transaction or erasing an already returned result is made. An unknown
    frame outcome is retained; replay through the same still-authorized view
    resolves it, otherwise the host reports uncertainty without forwarding an
    old frame request under a new view. No R08 offline asset outbox is added.
    Download uses a separate user offer/confirmation as 0032 does; `started`
    means handed to the browser. Enforce existing host/server frame budgets.
    The prompt states that granting reads gives the app private uploaded bytes
    and that preview network isolation is still absent. A grant is permission
    to access bytes, not evidence that the bytes are safe code. No CSP
    expansion, `allow-same-origin`, `allow-downloads` or new network-isolation
    claim is part of this proposal.

13. **Verification is part of each slice.** R09a must ship the S01 route-table
    inventory entries and actual owner/Alice/Bob/disabled-user isolation for
    every new route, equal-byte/different-owner denial, hash/ID guessing and
    malformed-body order, plus store reopen and exact-byte reads. Test empty,
    boundary and oversized input; false MIME, malformed/truncated and
    decompression-heavy images; dimension, animation, trailing-byte and text
    validation; quota races; base conflicts; same-key cross-operation reuse;
    lost replies; upload replay after deletion; and no-op deletion. Assert a
    supported-type zero-byte upload returns `invalid_asset`, a valid 1-byte
    text upload succeeds, valid text at exactly 5 MiB succeeds, and one byte
    over returns `asset_limit_exceeded`, with no key or storage left by either
    rejection. After committing an upload, change the validator/allowlist to
    refuse that input: the same original envelope/key must replay its stored
    receipt without invoking that validator, while a fresh key must receive
    the new policy rejection. Cover a stale asset base on that replay, deleted
    asset replay, purged/trashed/foreign artifact refusal and a stale runtime
    head winning over a matching key. Hold two first attempts after both see
    an unused key: after one commits, the identical loser replays even if its
    validation failed, whereas a different-byte loser gets `request_key_reused`;
    exactly one association, version increment, quota charge and receipt remain.
    Hold content validation and show an unrelated database write can commit,
    proving no transaction/write lock spans the decoder. Inject a
    failure before commit to prove no partial blob, association, counter or
    receipt survives. Cover trash/recover/purge, a purged asset operation's
    still-reserved key refused on another artifact and on another operation,
    the purged artifact's own 404 and the purge's original replay,
    all shared-source/asset/copy
    blob-reference combinations, cleanup dry-run/apply races, backup/restore,
    supported old-schema upgrades, rejected upgrades and one-shot disposal.
    R09b proves independent copy/replay and all-or-nothing quota refusal across
    real adapters, including default duplicate compatibility. R09c exercises
    both browser engines, error recovery and unknown outcomes. R09d tests
    delayed reads across delete/disable/revoke/head replacement, stale
    clear/regrant and close/create races, runtime-key separation, bounded part
    assembly, absent grants and device confirmation against the real server.
    Hold a runtime mutation after dispatch but before `BEGIN IMMEDIATE`,
    commit a source save, then release it: require `revision_conflict` and no
    new asset association, version or receipt.
    Break important authorization, quota, reclamation, retry and binding guards
    and prove the corresponding behavior tests fail. Do not claim observed
    cross-uploader roles or original backend internals from these own tests.
    Each PR runs `npm run check`, `npm run build` and its relevant integrations;
    UI/runtime changes run Chromium and Firefox smokes. No placeholder-only
    ports, empty adapters or declaration-mirroring tests constitute a slice.

## Technical basis and deferred work

SQLite permits one writer at a time; an immediate transaction obtains write
admission before quota checks ([SQLite transaction documentation](https://sqlite.org/lang_transaction.html)).
The same-database design preserves O04's existing snapshot model.
`nosniff` supplies specific MIME checks, not content sanitization or general
execution isolation ([Fetch Standard](https://fetch.spec.whatwg.org/#should-response-to-request-be-blocked-due-to-nosniff?)).
SVG has different processing modes and external-resource behavior, so one
sanitized example or one image embedding context cannot justify a general
SVG service ([W3C SVG integration](https://www.w3.org/TR/svg-integration/)).

Decoder dependency selection is a routine implementation decision subject to
rule 3's review, license provenance and existing dependency gates. No vendor
sanitizer code, captured assets or proprietary implementation is imported.
The initial subset intentionally omits richer types and sanitization rather
than advertising support that has not been specified. S02 sharing roles,
public publishing, asset change subscriptions, automatic offline replay,
remote URL import, transforms, asset history, global disk enforcement,
resource-closed export and external blob storage remain future work. Expanding
those product boundaries requires a separate specification decision.

## R09b2: bounded TypeScript mutation client

Status: contract accepted by the merged documentation PR; **implemented** by the
code PR that follows it for `uploadAsset` and `deleteAsset` in
`packages/client` (`asset-mutations.ts`), exported from the client index and
tested in `scripts/tests/asset-mutation-client.test.mjs` against the real Rust
server and bounded HTTP-boundary faults. The existing numbered rules above are
unchanged. R09a image validation and full R09b (CLI, MCP, `copy_assets`)
remain incomplete; PNG/JPEG upload is still refused by the server. The
implementation notes that follow the contract text are marked as such.

### Dependency and scope

The proposed exception follows R09b1's already implemented text HTTP/storage
boundary instead of waiting for full PNG/JPEG admission. It adds only named
`uploadAsset` and `deleteAsset` management TypeScript functions over the existing
HTTP routes and generated `UploadAssetRequest`, `DeleteAssetRequest` and receipt
DTOs. Their options require the caller's explicit `idempotencyKey`, using the
existing mutation convention. No new endpoint, database format or server
application behavior is proposed. Reusing generated DTOs does not authorize a
request or remove runtime checks.

This is not permission to enable any image upload. New PNG/JPEG uploads remain
unsupported; historical image envelopes and matching receipts must remain
replayable under rules 5–7. The client must not substitute today's text-only
allowlist for the server's stable envelope and replay ordering. It performs no
pixel decoding, content sanitization, MIME sniffing, normalization, remote fetch
or filesystem access. CLI, MCP, copying, management UI and runtime grants remain
separate R09b–d work. No R08 outbox, persistent pending request or background
sender is introduced.

### Explicit request and receipt boundary

The caller supplies the original artifact, independent `base_asset_version`,
lowercase parameter-free MIME declaration and base64 string for upload, or the
asset ID and base for deletion. The caller also supplies the key. The method
captures its own plain envelope and key before its first await; later changes
to caller-owned objects must not change that attempt or its receipt checks.
Stable envelope checks, including identifiers, key syntax and nonnegative safe
integer bases, and size bounds apply before large allocations. They do
not duplicate Rust text/image admission. The generated request continues to use
a MIME string rather than narrowing it to the currently admitted types.

An upload's decoded bytes are bounded by 5 MiB and the serialized envelope by
8 MiB. The empty decoded envelope remains distinct from the server's current
nonempty-content policy. Before dispatch, Web Crypto must compute SHA-256 from
the bounded original decoded bytes; absence or failure refuses without HTTP.
No weaker hash or image transformation is a fallback. This requirement makes an
upload receipt independently checkable, as bounded asset reads already require
Web Crypto. No decoded bytes are added to persistent client state.

Both operations use authenticated original-origin requests with redirect
refusal and no-store. They reuse the management transport's 45-second
response-body stall limit and cancellation. The body reader starts that timer
after headers while waiting for the first byte; afterwards it runs only while
a read is pending. A consumer holding a chunk is not a stalled read. This does not introduce or
claim a timeout for waiting for response headers. Success JSON is at most
64 KiB and error bodies 4 KiB. Existing bounded error vocabulary is preserved;
no raw response body, credential or URL becomes a diagnostic. A response cannot
grant authority to another artifact or account.

Upload requires exactly HTTP `201 Created`; delete requires exactly HTTP
`200 OK`, for both initial and replayed receipts, matching rule 9. Any other
2xx status is an `invalid_response`, even with an otherwise valid receipt. It
does not prove rollback or permit changing the original envelope or key.

Upload success validates the opaque asset ID, MIME equal to the original
declaration, byte count and SHA-256 equal to the original decoded request,
creation time, safe integer version and boolean replay flag. Its resulting
version must match the original upload's base plus one. Delete validates the
exact requested asset ID, boolean deleted/replayed flags and safe integer
version equal to its base plus one when deleted, otherwise its base. Invalid,
oversize, malformed or stalled receipts are failures, never success evidence.
A replay's version is its historical receipt, not a replacement for a newer
canonical asset-space version; metadata/content are separate current reads.

### Unknown outcomes and explicit retries

Each call makes one mutation request. It does not generate or rotate a key,
refresh the base, rebase, automatically retry, recreate a deleted upload or
interpret a transport/receipt failure as rollback. The caller must retain the
same immutable envelope and key for a deliberate retry after an unknown
outcome. Rebuilding an upload from a mutable byte buffer, rereading a file,
reading a newer base or silently choosing a new key is not that retry.
Per-call capture does not freeze a caller's separate later call: callers remain
responsible for supplying the original values. The service's durable matching
receipt, authority checks and shared actor-wide key space remain authoritative.

A current 409, 422, 429 or access refusal does not establish that an earlier
unknown attempt did not commit. This client slice adds no cleanup authority or
asset-specific recovery state machine. Historical upload replay after deletion
returns only its original receipt and cannot restore bytes. Delete replay keeps
its original deleted result even when the association is already absent.

### Acceptance before implementation completion

Use the real Rust server and original independently created text/CSV inputs,
plus bounded HTTP-boundary faults; do not mock the service or receipt oracle.
Verify upload/list/metadata/exact read/delete; independent asset versions and
unchanged source/data; foreign-user denial; exact key replay after a committed
response is lost; delete replay; upload replay after deletion without
resurrection; key reuse, stale base and current type/quota refusals.

Hold a request and mutate caller-owned request/options objects to prove the
attempt and its validation use the original captured values. Independently
replace upload's `201` and delete's `200` with other 2xx statuses, for both
initial and replayed receipts, and require `invalid_response` despite an
otherwise valid body. Corrupt ID, MIME, size, digest, version and replay/deleted
fields independently, and exercise oversize, malformed, stalled and redirect
responses without accepting
a receipt or following credentials/body to a collector. Absence/failure of Web
Crypto must cause zero mutation HTTP. Explicit same-key replay after a rejected
receipt must observe one server association/version change, not a second write.

Each important rejection needs a runtime guard mutation that fails by its
specific assertion. Compile failures, timeouts, unselected tests and cleanup
failures are not guard kills. Preserve unsuccessful batches separately. Run
formatter/check/build and relevant real-server client tests under the repository
sandbox and cleanup rules; use both browser engines for any new browser Fetch
or Web Crypto fixture.

### Implementation notes (R09b2 code PR)

These record choices the contract left open; the contract above wins on any
conflict.

- `uploadAsset(client, artifactId, request, { idempotencyKey, signal? })` and
  `deleteAsset(client, artifactId, assetId, request, { idempotencyKey, signal? })`
  take the generated request DTOs and return plain, checked copies of the
  generated receipt DTOs. Receipt validation does not reuse R09b1's four-type
  MIME allowlist: the receipt MIME must equal the declared spelling.
- Before dispatch the client checks the key (8–128 of `A-Za-z0-9._:-`, the
  server's rule), the identifiers, the MIME spelling (lowercase,
  parameter-free, at most 127 characters) and strict canonical base64, bounded
  by character count and by the decoded size computed from the text, before
  the output is allocated. The 8 MiB serialized bound follows from the 5 MiB
  decoded bound (at most 6,990,508 base64 characters plus a short envelope).
  A local refusal is `invalid_request` with status 0, and nothing is sent. A
  base of 2^53 - 1 is also refused locally, because its successor version
  could not be checked as a safe integer.
- Web Crypto failure, absence or an abort while the digest is computed ends
  the call with nothing sent. A signal aborted before dispatch likewise.
- The exact-status rule is a `expectStatus` option of the shared
  `ArtifreeClient.json` helper; a different 2xx is `invalid_response` and its
  body is cancelled unread.
- A 409 `asset_conflict` is reported with the existing error vocabulary; its
  `current_asset_version` is available only in `ArtifreeApiError.body`, not as
  a typed field. No typed accessor was added.
- Tests: `scripts/tests/asset-mutation-client.test.mjs` runs the real Rust
  server (lifecycle, foreign user, lost response and replay, delete replay,
  upload replay after deletion, key reuse, stale base, type and quota
  refusals, rejected receipt then same-key replay) plus fake-fetch and
  fake-clock tests of the envelope, receipt, status, size, stall, abort and
  redirect rules. No browser fixture was added, so no browser smoke applies.
