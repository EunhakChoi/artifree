# 0045: Uploaded assets scoped to a user and artifact

> Browser engines: Firefox is suspended in CI until the planned feature set is
> complete. A Firefox acceptance below is met in Chromium and reported as pending
> re-verification ([quality](../quality.md#browser-engines)).

Date: 2026-11-06. Status: partially implemented; **R09a admits PNG and the named sequential JPEG subset under complete validation (progressive and other JPEG variants are refused as `invalid_asset`; the remaining follow-ups are recorded in the R09a image validation notes); R09b1 supplies read-only client calls, R09b2 the bounded upload/delete client calls, the duplicate `copy_assets` extension is implemented and R09c supplies the management assets panel and the duplicate dialog's copy control (Firefox re-verification pending); the MCP asset tools (R09b3) and the CLI asset commands (R09b4) are implemented; R09d1 lets a runtime frame list and read its artifact's uploaded assets under a separate `assets.read` grant (Chromium; Firefox re-verification pending); the remainder of R09d (frame upload/delete and device download) is proposed**.
The current HTTP/storage slice accepts UTF-8 plain text/CSV, complete static
PNG and complete JPEG of the sequential subset recorded in the R09a image
validation notes at the end of this record. Other JPEG variants are refused as
`invalid_asset`.
The four-type target below remains unchanged. The read-only TypeScript client
lists assets, reads metadata and verifies bounded content; `uploadAsset` and
`deleteAsset` perform one explicit mutation each (R09b2). The MCP tools `list_assets`, `read_asset`, `upload_asset` and `delete_asset`
exist (R09b3). The CLI `asset list|upload|read|delete` commands exist (R09b4). There is no grant or frame access yet. A duplicate copies the
requesting actor's current asset space only when `copy_assets` is true (rule 10).
The management web has an Assets tab (R09c: usage, upload, list,
attachment download, delete, conflict and unknown-outcome retry), and its
duplicate dialog copies assets only when asked (R09c). Builds on [0003](0003-local-html-artifact-slice.md),
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
image admission follows rule 3 as recorded in the R09a image validation notes. Upload preflight closes
its read snapshot before content validation, and final immediate settlement
rechecks authority, key and base before using the saved validation result.

Only `text/plain`, `text/csv`, `image/png` and `image/jpeg` currently pass
new-upload admission. A PNG or JPEG is admitted only after the complete
validation recorded in the R09a image validation notes at the end of this
record; every refused JPEG, valid but unsupported variants included, is
`invalid_asset`. The target allowlist and bounds below are unchanged.
See the [content policy](../asset-validation.md).
Stable MIME envelope parsing and historical metadata/receipt decoding still
accept image type spellings. A currently authorized matching historical upload
replays before the current admission policy, including after deletion,
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
envelope and key themselves after an unknown outcome. MCP has its own tools (R09b3) and the CLI its own commands (R09b4); a separate
CLI run cannot freeze the bytes, so the server's key fingerprint enforces the
identical resend there (see R09b4).
No asset change subscription is emitted. Duplicates copy assets only with the
`copy_assets` opt-in (see "Implementation notes (copy_assets)" below); requests
without it keep their files/data behavior and fingerprints.

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
remain incomplete; at the time of R09b2 the server refused PNG/JPEG uploads
(PNG and the JPEG subset are admitted since the R09a image changes recorded at the
end of this record). The implementation notes that follow the contract text are marked as such.

### Dependency and scope

The proposed exception follows R09b1's already implemented text HTTP/storage
boundary instead of waiting for full PNG/JPEG admission. It adds only named
`uploadAsset` and `deleteAsset` management TypeScript functions over the existing
HTTP routes and generated `UploadAssetRequest`, `DeleteAssetRequest` and receipt
DTOs. Their options require the caller's explicit `idempotencyKey`, using the
existing mutation convention. No new endpoint, database format or server
application behavior is proposed. Reusing generated DTOs does not authorize a
request or remove runtime checks.

This was not permission to enable any image upload: when R09b2 was written,
new PNG/JPEG uploads remained unsupported (historical constraint; see the R09a image
validation notes for the current state). Historical image envelopes and matching receipts must
remain replayable under rules 5–7. The client must not substitute any current
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
  parameter-free, at most 127 characters) and strict canonical base64; the
  decoded size is computed from the text length and padding and bounded at
  5 MiB before the output is allocated, so a text longer than 6,990,508 base64
  characters is refused (unpadded text of exactly that length decodes to more
  than 5 MiB). The 8 MiB serialized bound follows from the 5 MiB
  decoded bound (at most 6,990,508 base64 characters plus a short envelope).
  A local refusal is `invalid_request` with status 0, and nothing is sent. A
  base of 2^53 - 1 is also refused locally, because its successor version
  could not be checked as a safe integer.
- Web Crypto failure, absence or an abort while the digest is computed ends
  the call with nothing sent. A signal aborted before dispatch likewise.
- A signal is accepted structurally: `aborted` must be a boolean, and
  `addEventListener` and `removeEventListener` must be functions. So a signal
  from another realm is accepted, and an object without those members is
  refused with `invalid_request` and nothing sent. An abort between the digest
  and dispatch also sends nothing, with the same error as an abort before the
  call.
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

## Implementation notes (copy_assets)

Status: **implemented** for HTTP, the TypeScript client, the CLI
(`--copy-assets`) and the MCP `duplicate_artifact` argument. These notes record
choices rule 10 left open; the rule wins on any conflict.

- The copy runs inside the duplicate's creating transaction, after the source
  and optional data copy and before the request key is recorded, so an
  overflow leaves no artifact, key, association or blob change.
- Destination checks cover both the space limits (64 files, 25 MiB) and the
  actor limits (256 files, 100 MiB, trash included), with every copied
  association charged at full logical size. Overflow is `422
asset_limit_exceeded` with the same generic message as uploads; it does not
  say which limit was hit.
- Copied associations keep the source's asset ID, type, size, digest and
  `created_at` (metadata is immutable per ID). Only the requesting actor's
  rows are copied; upload receipts are not. A nonempty copy starts at
  `asset_version` 1, an empty one writes no space row and reads as 0.
- `copied` lists `files`, then `data` and `assets` when requested (`assets`
  also for an empty space); clients treat it as a set. `copy_assets: false`
  and omission keep the earlier fingerprint; `true` adds a distinct
  component.
- The management web duplicate dialog offers the copy as an unchecked box
  (R09c, below).

## R09c: management assets panel and copy control

Status: **implemented in Chromium; Firefox acceptance is pending
re-verification** ([quality](../quality.md#browser-engines)). The management
web has an Assets tab per artifact with usage, upload, list, attachment
download, delete, conflict handling and unknown-outcome retry, and the
duplicate dialog has the explicit copy control of rule 11.

These notes record choices rule 11 and R09b2 left open; the rules win on any
conflict.

- Usage and limits come from one list snapshot (`bytes`/`max_bytes`, file
  count/`max_assets`), each live association at full logical size. The actor
  cap (256 files, 100 MiB) has no wire field; the panel says the server checks
  it on upload.
- A file over 5 MiB is refused from its size before it is read. Empty files
  are sent and refused by the server. The declared type is chosen from the
  file's own valid MIME spelling, `text/plain` or `text/csv` and sent as given;
  there is no client allowlist, so other types reach the server's
  `asset_type_unsupported`.
- Each upload or delete is frozen once (artifact, base, declared type, encoded
  bytes and key). One attempt with an unknown outcome is held in page memory
  per open artifact; retry resends the identical request and key, forget
  drops it and sends nothing, and a later refusal never clears it. While an
  attempt is held or sending, further uploads and deletes and opening another
  artifact are locked; reloading the page forgets it. A held attempt that
  predates a possible data replacement can only be forgotten.
- `asset_conflict` shows `current_asset_version` from the error envelope and
  raises the asset write gate's floor; the list is read again after every
  settled attempt instead of trusting a receipt. A replayed receipt is shown
  as the original result.
- Downloads read verified bytes through the client and save them as
  `asset-<id>` with type `application/octet-stream`. No asset content is put
  in the management DOM, previewed or opened.
- The duplicate dialog's "Copy uploaded assets" box starts unchecked each
  time the dialog opens; `copy_assets: true` is sent only when it is checked,
  and an unchecked request omits the field. The dialog states the three cases
  apart: by default only the chosen revision's files are copied; checked, all
  of the caller's current assets of the source are copied as they are now,
  whatever revision is chosen, with their own asset version, and an overflow
  refuses the whole duplicate; application data is copied only on request
  through the CLI, MCP or API, never by this dialog. The revision and the box
  form the request whose key is kept for an unchanged resend and replaced for
  any other (0012 decision 8). `asset_limit_exceeded` is shown with the
  panel's limit wording. A held asset attempt still disables Duplicate.

## R09b3: MCP asset tools

Implements the MCP half of rule 11. It adds no numbered rule and changes none.
`list_assets`, `read_asset`, `upload_asset` and `delete_asset` call the same
`operations` functions as the HTTP asset routes (the handlers were thinned to
call them), so limits, base checks, key replay, receipts and error codes are
those of rules 1-9. What rule 11 left open and this slice fixes:

- `read_asset` returns `{ asset_version, asset, content_base64 }` in one
  result: the stored bytes as strict standard padded base64, with the space's
  `asset_version` read together with the metadata. There is no paging and no
  separate size limit beyond the 5 MiB asset limit (about 6.7 MB of base64, sent
  twice by the MCP result envelope: about 13 MB on the wire). An `upload_asset`
  request is bounded by MCP's 36 MiB request limit (decision 0005) before the
  asset limit applies, not by the 8 MiB body cap of the HTTP asset routes.
- `upload_asset` takes `base_asset_version`, `content_type`, `content_base64`
  and `idempotency_key`; `delete_asset` takes `asset_id`, `base_asset_version`
  and `idempotency_key`. Neither accepts a host path or URL. Results are the
  HTTP bodies (`replayed` included) and failures are the HTTP error bodies with
  `isError: true`. An unknown or foreign artifact or asset, or one in the trash,
  is `not_found`.
- The two write tools are destructive (`destructiveHint`) and idempotent, as
  rule 11 requires of mutating MCP tools. An upload replaces nothing, so for
  `upload_asset` the hint follows that rule, not its effect on stored data. The
  tool catalog gets the effect `asset`, whose mutating tools must require
  `idempotency_key`. The two read tools are read-only.
- Asset writes emit no change-feed event, as over HTTP; MCP has no asset
  subscription. A preview reads assets only through the R09d1 `assets.read` bridge below.

Verification: the shared operations keep the existing HTTP asset tests green;
`crates/artifree-server/tests/mcp_adapter.rs` and `scripts/tests/mcp.test.mjs`
exercise the tools against the real server (round trip, same-key replay, stale
base, rejections, isolation between users, HTTP/MCP byte parity, tool hints),
with mutation checks for the base-version guard and the returned bytes. The
The CLI `asset` commands are R09b4 below.

## R09b4: CLI asset commands

Implements the CLI half of rule 11. It adds no numbered rule and changes none;
nothing changes on the server, in the TypeScript client or in the generated
contracts. `artifree asset list|upload|read|delete` call the client functions of
R09b1 and R09b2 and print their validated results as JSON on stdout; diagnostics
and retry hints go to stderr.

- **Grammar.** `asset list <artifact-id>`; `asset upload <artifact-id> --file
<path> --content-type <mime> --base-asset-version <n> [--idempotency-key
<key>]`; `asset read <artifact-id> <asset-id> --output <file>`; `asset delete
<artifact-id> <asset-id> --base-asset-version <n> [--idempotency-key <key>]`.
  `delete` has no `--yes`: the required base is the confirmation, as for
  `data delete`.
- **Base and key.** `--base-asset-version` is required and is never read from
  the server by the CLI, because reading it again would turn a retry into a
  different request. Without `--idempotency-key` the CLI makes one and reports it
  only for an unknown or rate-limited outcome. There is one request and no
  retry (R09b2). After an unknown outcome stderr says to resend with the same
  key and the same parts: for upload the artifact, `--content-type`, base and
  file content, with the file's sha256 and size so the operator can check it did
  not change; for delete the artifact, asset ID and base. A separate CLI run
  cannot freeze the bytes, so the server's fingerprint enforces identity: the
  same key with other content is `request_key_reused` and never a second write.
- **Input.** Upload takes only a path the operator names (rule 11); there is no
  stdin. `--content-type` is required and never inferred from the file name; the
  CLI checks only its spelling, so `image/gif` and `text/html` reach the server
  and are `asset_type_unsupported`, while `image/png` and `image/jpeg` reach their
  validation and are `invalid_asset` unless the file is complete. The file must be a regular file: a symbolic
  link or special file is refused; it is opened without following links (where
  the platform has `O_NOFOLLOW`) and its device and inode must match the check
  before opening; a file over 5 MiB is refused before it is read, and a file whose size or modification time changes while it is read is
  refused (a best-effort check, bounded by the file system's timestamp
  granularity). An empty file is sent and refused by the
  server as `invalid_asset`. Local refusals send nothing.
- **Read.** `--output` is required, no raw bytes go to stdout, and the file is
  written only after the client verified type, size and SHA-256. It is created
  exclusively: an existing path, including a symbolic link, is refused and a
  partial file is removed. A digest mismatch is `invalid_response` and creates no
  file. Exit codes: 0 success, 2 usage, 1 API or local file errors.
- **Conflicts.** An `asset_conflict` prints `current_asset_version=<n>`, read from
  the error body because the client has no typed accessor.
- **Open flags and mode.** The file is opened with `O_NONBLOCK` where it exists,
  so a regular file replaced by a FIFO between the check and the open cannot block
  the open; the device, inode and type check right after the open refuses it. The
  downloaded file is created with mode `0600`, because asset content is private
  user data, whatever the umask.
- **Bounds.** `--base-asset-version` must be below 2^53-1, as the client requires,
  so an impossible value is a usage error (exit 2) before anything is sent.
- **Not in this slice:** `asset show`, stdin or `--output -`, content-type
  inference, a CLI allowlist, CLI-side retry or a pending-request file, a typed
  `currentAssetVersion`, PNG/JPEG admission (R09a), runtime write/download grants (R09d2, R09d3), change
  following, bulk upload.

Verification: real-server CLI tests in `scripts/tests/cli-assets.test.mjs`
(lifecycle, replay, exactly 5 MiB), `cli-assets-refusals.test.mjs` (server
refusals), `cli-assets-local.test.mjs` (nothing sent) and
`cli-assets-outcome.test.mjs` (lost reply through a proxy, corrupted read), with
guard mutations for each rule above.

## R09a image validation (implementation notes)

Status: **implemented** for `image/png` and for the `image/jpeg` subset below.
The numbered rules above are unchanged: this section records how rule 3 is met
and is not a new decision. The PNG notes come first, the JPEG notes after them.

- **Decoder and checks.** `artifree-core` links `png` 0.18.1, `miniz_oxide` 0.8.9
  and `crc32fast` 1.5 (boundary edges recorded in `scripts/lib/boundaries.mjs`).
  Validation runs three layers: a container scan of every chunk, an independent
  exact zlib check of the IDAT stream and of every compressed ancillary stream,
  and a full `png` decode with CRC and Adler-32 checks on. Details and the
  dependency review are in the [content policy](../asset-validation.md).
- **Known limit.** Ancillary chunks that the decoder ignores when malformed
  (such as `pHYs`, `gAMA` or a `tEXt` keyword) are checked for framing and CRC
  only; their content is not validated beyond the compressed `zTXt`, `iTXt` and
  `iCCP` streams. Whether rule 3's "complete format validation" requires more is
  an owner question recorded with the R09a PNG change.
- **Bounds.** At most 4,096 pixels per axis and 16,777,216 pixels; the decoded
  upload is at most 5 MiB; one 16 MiB budget bounds ancillary buffering and
  inflated text; decoding is serialized process-wide.
- **Refused as `invalid_asset`:** a declared type that does not match the
  signature, empty content, malformed or truncated chunks or zlib data, a CRC or
  Adler-32 mismatch, extra bytes after IEND or after a zlib stream, zero or
  oversize dimensions, and animation (`acTL`, `fcTL`, `fdAT`). Bytes are stored
  exactly as sent: nothing is re-encoded or stripped.
- **JPEG decoders reviewed.** `zune-jpeg` 0.5.15 does not check the length of
  the entropy-coded data: it decodes files whose entropy bytes were removed, and
  it accepts bytes after EOI. Complete validation therefore uses an Artifree-owned
  entropy walk.

**JPEG (R09a, second change).** Status: **implemented for the named subset only.**

- **Layers.** A header parser, an Artifree-owned entropy walk (canonical Huffman
  builder, byte-at-a-time bit reader with no zero-fill, strict padding, restart
  sequence, EOI as the last two bytes) and then `zune-jpeg` 0.5.15 (default
  features off, strict mode, 4,096 per axis, output at most 64 MiB) as a
  corroborating decode. The walk guarantees truncation is refused; the decoder is
  never relied on for it. Dependency edge and review: `scripts/lib/boundaries.mjs`
  and the [content policy](../asset-validation.md).
- **Admitted subset.** SOF0/SOF1, 8-bit, one, three or four components, exactly one
  scan with every component, optional DRI, APPn/COM and fill bytes. Bounds are the
  PNG bounds (4,096 per axis, 16,777,216 pixels, 5 MiB, one decode lock).
- **Refused as `invalid_asset`** (valid unsupported variants included, so a
  caller cannot tell "broken" from "outside the subset" by the code): progressive,
  lossless, hierarchical, arithmetic and 12-bit frames, multi-scan sequential,
  DNL, bytes before SOI, restart markers outside the scan, and any byte after EOI
  (camera trailers and MPF are refused). Stored bytes are exact; nothing is
  re-encoded or stripped.
- **Known limits.** APPn/COM contents are framed, not validated. The walk is
  stricter than libjpeg in three places (1-bit padding, no restart marker after
  the last MCU, ZRL may not land on coefficient 64). `zune-jpeg` can panic inside
  `decode_into`; the decode runs under `catch_unwind` (containing a dependency fault, not
  reporting errors) and a panic is `invalid_asset`. The decoder also refuses a few
  files the walk accepts (an Adobe APP14 after a gray frame header, luma factor 3).
- **Completeness label.** R09a is complete only for this subset. Progressive
  admission and trailer-bearing JPEGs (camera trailers, Motion Photo, SEFT, MPF) are
  recorded as non-blocking follow-ups; the validator is not relaxed, and a client that
  re-encodes or strips a trailer before upload is a suggested route that is not
  implemented. Decided 2026-11-09 (rationale: progressive and trailer files stay
  refused and the validator is not relaxed, so rule 3 is not weakened). Still open:
  whether a limit specific to asset validation belongs in S02, and whether rule 3
  requires ancillary PNG chunk content validation.
- **Decoder panic containment.** An uncontained zune-jpeg panic surfaces as a
  repeatable 503, so the decode runs under `catch_unwind` (see Known limits). The
  test uses a stand-in closure; a regression fixture from a real zune panic input is
  **still missing** and is tracked in the roadmap R09a row.
- **Not changed:** the envelope, fingerprints, replay order, schema and every
  client, MCP and CLI contract except their lists of accepted types.

## R09d1: runtime `assets.read` (implementation notes)

Rule 12's read half is implemented; `assets.write` (R09d2) and `assets.download`
(R09d3) are not. Chromium is verified; Firefox re-verification is pending.

- **Capability.** `assets.read` has its own declaration, support, consent and
  connection grant, independent of every `source.*` capability. A grant of one
  never admits the other. The consent text says the app receives the bytes of the
  artifact's private uploaded files and that the preview is not isolated from the
  network, so the app could send those bytes elsewhere.
- **Routes.** `POST {VIEW}/assets/list` and `POST {VIEW}/assets/read` on the bound
  frame route, with the same admission, no-store and body limit as the source
  routes. The list returns the artifact's current asset space (assets are not part
  of a source revision). A read takes `asset_id`, `offset` and `length`.
- **Parts.** A part is at most 262,144 bytes (`MAX_RUNTIME_ASSET_PART_BYTES`) and
  carries `asset_id`, `content_type`, `offset`, `next_offset` (null on the last
  part), the whole asset's `size` and whole-asset `sha256`, and base64 `content`.
  The digest comes from stored metadata and is not recomputed per part. An offset
  past the end is `416`; an offset equal to the size reads an empty final part.
- **Check order.** Binding (`404`), the connection's current grant (`403`) and the
  artifact's head (`409`) are checked before the arguments are judged, and the
  grant and head are checked again before the part is returned. A read also
  confirms that the asset is still associated with the artifact with the same
  digest, so a deletion that lands before that check releases no bytes (`404`).
  The residual window is a deletion or grant change that lands after that check
  and before the response is written; it is not closed, and a result already
  returned is not recalled.
- **Frame SDK.** `assets.list()` and `assets.read({ assetId })` return validated
  listings and the assembled bytes. The SDK reads parts from offset 0, checks each
  part's echoed identity, type, offsets and size against the first, and verifies
  the assembled length and whole-asset SHA-256 before returning; a mismatch is
  `unavailable`. There is no per-part public API.
- **Not changed.** No CSP widening, no `allow-same-origin`, no `allow-downloads`,
  no asset URL and no token for a frame. No new `ArtifreeClient` method: the host
  reaches the routes through the runtime call helper, so the main web bundle does
  not grow.
- **Still not supported:** frame upload or delete (R09d2), device download
  (R09d3), S02 shared roles and an offline asset outbox.
