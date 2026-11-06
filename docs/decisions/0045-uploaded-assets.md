# 0045: Uploaded assets scoped to a user and artifact

Date: 2026-11-06. Status: proposed; **R09a–R09d are not implemented**.
This documentation specifies future behavior and adds no endpoint, storage,
grant, SDK implementation, or UI. Builds on [0003](0003-local-html-artifact-slice.md),
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

## Proposed rules

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
   In one `BEGIN IMMEDIATE` transaction: check live ownership, resolve the
   actor-wide shared key space, replay a matching receipt, compare the base,
   recompute resulting space and actor quota, insert/remove associations and
   blobs, advance the version if changed, and save the receipt. The key space
   includes artifact, cleanup, job and data operations. A rejected operation
   reserves no key, changes no counter and leaves no blob. Concurrent requests
   cannot both consume the last quota slot. Validation may precede this
   transaction but ownership, base and quota decisions may not rely on that
   earlier read. S01's existing authenticated-before-disable stored-write
   semantics remain in force; no stronger mid-request cancellation is claimed.

6. **Retry and deletion.** Fingerprints cover operation, artifact, base,
   declared type and exact decoded bytes for upload, or asset ID for delete,
   using the existing unambiguous length-prefixed hashing scheme. A matching
   key replays the original metadata-only receipt before the now-stale base
   or changed quota is considered. It creates no extra association, charge or
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
   route identifiers are 404; unknown fields, malformed JSON/base64 or missing
   key are 400; outer-body overflow is `413 payload_too_large`; unsupported
   declared type is `422 asset_type_unsupported`; invalid content is
   `422 invalid_asset`; decoded size, count, space or actor quota overflow is
   `422 asset_limit_exceeded`. An authenticated foreign artifact with a
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
    lost replies; upload replay after deletion; and no-op deletion. Inject a
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
