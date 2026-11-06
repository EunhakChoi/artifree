# Browser application-data outbox

Status: **Partially implemented.** R08a provides the server binding fence;
R08b provides local browser admission and owner recovery. R08c-1 adds internal
prepared/ACK/checkpoint/trim persistence. Network delivery and the remaining
numbered sender/lifecycle contract are proposed.

Scope: R08, an opt-in browser TypeScript client for R06 application-data
writes. This specification does not enable it in the management UI or runtime
frames. Numbered rules define the complete target contract; changes to
them require a separate specification amendment before implementation.

## Existing behavior and evidence

[0031](0031-app-data.md) implements direct conditional, keyed writes through
`ArtifactService`. Keys are actor-scoped; replay returns the original receipt,
not the current value. Access is checked before replay, including for trash.
[0040](0040-data-changes-and-member-listing.md) specifies notifications and
canonical reads; its client follower is present on the specification's base
branch. It does not provide pending writes or acknowledgements.
[0018](0018-source-draft-recovery.md) concerns source drafts, a separate store
and recovery policy. [0036](0036-multi-user-identity.md) defines authenticated
actors; [0027](0027-backup-restore-and-schema-upgrade.md) renews the feed
identity on restore.

The roadmap's private evidence index groups R08 with E3 and E5. Original
single-account observations distinguish durable app-data rows owned by one
tab, recovery in the original restored tab, and a fresh tab that did not adopt
them. A lifecycle follow-up observed a held write fail during declaration
clear and later succeed with the same key after access returned. These do not
prove another principal's authorization, unlimited retention, or arbitrary
tab adoption. Docs staging, operation WAL and synthetic quota/trim faults are
separate evidence; they do not establish the general app-data implementation.
The private requirement trace records the inspected report, original file,
hash and JSON anchor. No original code, captures or private paths belong here.

All protocol shapes, limits, ownership arbitration, failure policy and PR
slices below are **Artifree design proposals**, not claims about original
internals. Browser durability is conditional on retained origin storage and
retained session identity; enqueue success is not an OS power-loss guarantee.
An inherited session owner is not proof of the physical original tab. Whenever
that distinction cannot be established, recovery pauses for an explicit choice;
acquiring a lock or acknowledging a prompt does not establish physical origin.

## Proposed contract

### Surface and binding

1. Add a browser-only outbox entry point in `@artifree/client`, opened
   explicitly by a caller. It owns a page-lifetime coordinator outside React
   component lifetimes. Existing `setData`, `updateData`, `deleteData`, CLI,
   MCP and management UI keep their direct-write behavior. There is no service
   worker, background delivery after tab closure, runtime SDK data capability,
   frame bridge, optimistic canonical value, or required user-facing UI.
   Callers receive typed state/receipt callbacks and explicit recovery
   commands, including `recovery_required`, `retryRecoveryCandidate()`,
   `confirmRecovery(selection)`,
   `discardUnsent(selection: UnsentDiscardSelection)`,
   `discardRejected(selection: RejectedDiscardSelection)`,
   `abandonUnresolved(selection: UnresolvedAbandonSelection)`,
   `continueRecovery(continuation: RecoveryContinuation)` and
   `resumeDelivery(selection: DeliveryResumeSelection)`, plus
   `retryPreparation(selection: PreparationRetrySelection)`. `RecoveryContinuation`,
   `DeliveryResumeSelection`, `PreparationRetrySelection`, `UnsentDiscardSelection`,
   `RejectedDiscardSelection` and `UnresolvedAbandonSelection` are distinct opaque action types with runtime
   kind/context validation, not interchangeable strings or a generic `resume`.
   Confirmation must represent an explicit recovery choice by the caller/user,
   never a default startup, reconnect or online-event action. A consumer that
   supplies no recovery interaction leaves recovered work paused; R08 requires
   no new management UI. Source drafts, room events, Docs operations and comments never
   enter this store.
2. Binding must be established by the server before an outbox first accepts
   writes, and checked again before recovery sends. Add authenticated
   `GET /v1/data-outbox/binding` returning exactly the authenticated
   `actor_id`, `actor_generation` and opaque `storage_epoch` in a `200` response. `actor_id` is
   `local` or `u-` followed by 32 lowercase hexadecimal digits, the registered
   actor spellings of 0036. `storage_epoch` is the existing `FeedId` display
   form: exactly 32 lowercase hexadecimal digits representing its 16 bytes.
   `actor_generation` is the existing `users.state_generation`, encoded as a
   canonical positive decimal **string** in the current SQLite integer range
   1–9223372036854775807 (no sign, whitespace or leading zero). It is not a
   JavaScript number or the client's credential-change counter. The owner is
   always generation `"1"`. Read the feed identity and enabled user generation
   in one storage snapshot, require the generation to match `AuthContext`, and
   perform current-user confirmation before returning. A snapshot generation
   mismatch, missing/disabled current user, or failed confirmation is the same
   `401` and fixed unauthenticated response as 0036, with no binding fields
   returned; it is never `409 outbox_binding_changed` on this read. Keep 0036's
   existing confirmation/cleanup semantics. The read takes no query or body:
   after ordinary authentication/rate checks, reject any present URI query
   component, including a bare `?` with an empty query, or nonempty request body
   with `400 invalid_request` before reading the binding snapshot. Check query
   presence, not whether decoded parameters or query text are nonempty. Require exactly
   those three response fields and bound
   its success body to 4 KiB. Use the existing feed identity
   rather than inventing a second database identity. It uses ordinary HTTP
   authentication/rate limits, `Cache-Control: no-store`, no administrator
   requirement, no other user's information, and the existing current-user
   confirmation before returning. It is not a health/inventory response.
   Support `HEAD /v1/data-outbox/binding` as the GET route's implicit HEAD:
   apply the same authentication, rate, query/body validation, snapshot and
   current-user confirmation, status and `Cache-Control: no-store`, but return
   no response body on success or failure. HEAD exposes no binding tuple and
   cannot initialize/resume a client binding; the outbox client uses GET. Other
   method and CORS/preflight handling retain the existing API behavior.
   Token rotation for the same actor, actor generation and epoch may resume only after all
   applicable recovery/access choices; another actor may
   not. A caller label, token fingerprint or API URL alone is insufficient.
3. Persist the normalized API base URL (including any base path), authenticated
   actor, storage epoch, artifact and owner with every lane. Persist each
   intent's admitted actor generation and the lane's separately authorized
   generation checkpoint. No bearer token,
   cookie, runtime grant or secret is persisted by the outbox. The coordinator
   validates the raw base URL before any outbox storage access, owner/session
   persistence, or network call. Accept absolute HTTP(S), or empty/root-relative
   bases resolved against the browser's origin; reject other relative forms,
   non-HTTP(S), nonempty parsed URL `username` or `password`, and any query or
   fragment marker (including empty `?`/`#`). Use the standard URL parser, canonical origin/path and removal of
   trailing slashes, and use this same normalized absolute base for persistence
   and transport. Credential-bearing input is rejected as
   `invalid_outbox_configuration`, not silently stripped into another binding.
   Its fixed diagnostic/callback contains no original URL, userinfo or parser
   error cause; rejected input is never written to storage or logs. Recovery
   strictly validates stored URLs too; an unsafe legacy URL is quarantined
   without sending, echoing credentials or automatically rewriting its binding.
   Existing direct-client URL behavior is unchanged. The coordinator
   pins one immutable credential generation for each send; profile/credential
   changes fence its callbacks and require a new binding check. URL, actor or
   epoch mismatch pauses the old lane; never silently rebind or replay into
   the new target, even if its artifact ID and data version happen to match.
   A changed actor generation also pauses delivery, invalidates prior recovery
   choices and requires rule 18's explicit reauthorization; never silently copy
   the newly observed generation into a retained request's authority.
   Every request initiated by outbox machinery must set Fetch
   `redirect: 'error'` at the final dispatch boundary: initial binding,
   revalidation/access reads, any canonical reads it initiates, mutations and
   every retry/replay. Reject all redirects, including same-origin/path-only,
   HTTP-to-HTTPS and `301`/`302`/`303`/`307`/`308`; no automatic follow, manual
   `Location` request, fallback to `follow`/`manual`, or rewriting the stored
   URL/method/body/key/fence from a redirect. Caller options and internal helper
   retries cannot override this mode; a transport adapter must honor it or be
   refused for outbox use. Checking `Response.redirected`/the final URL after
   following is not a substitute: sensitive bytes could already have reached
   another path/origin. Direct-client calls outside this opt-in machinery keep
   their existing behavior. The policy prevents following a redirect, not the
   original configured destination from receiving or committing its request.
4. A handshake alone leaves a race with restore or a different actor's token.
   Outbox writes therefore include the all-or-none optional header triple
   `Artifree-Outbox-Actor`, `Artifree-Outbox-Epoch` and
   `Artifree-Outbox-Actor-Generation`. A partial triple,
   repeated headers, or syntax outside rule 2's actor/epoch/generation spellings is
   `400 invalid_request`. After normal
   authentication and input validation, preserve ordinary header/body/path
   syntax errors; then authorize the target using the **authenticated actor**
   and confirm that it is in place. A foreign, missing, trashed or purged
   target returns the existing indistinguishable `404 not_found`, even when a
   well-formed outbox fence would mismatch. Do not compare expected actor,
   epoch or generation early and turn that target denial into `409`. Invalid
   fence syntax remains the input error above, not a semantic fence mismatch.
   Only an accessible in-place target reaches the semantic fence: in the same
   `BEGIN IMMEDIATE` as key
   lookup/base comparison/write, require an enabled current user and
   `expected actor_generation == AuthContext.generation == users.state_generation`,
   `expected actor == AuthContext.actor` and `expected storage_epoch == current
feed identity`, **after target-access/in-place checks
   but before key lookup or replay too**. The target's in-place check is within
   that write transaction as in 0031; retain core ownership authorization.
   Thus the order is validated input, target access/in-place, semantic fence,
   key replay/reuse, base comparison, mutation and receipt/change record. The
   current user row is read inside this transaction, not through a separate
   preflight `ensure_current` call. User state changes use this same database
   and write serialization, so disable/re-enable cannot slip between the
   generation check and mutation/replay. Authentication may already return
   `401` for a disabled user; a fence mismatch after authentication is the
   binding error below. A write committed before disable remains committed;
   the fence does not retroactively cancel it or change direct-write semantics
   that 0036 explicitly leaves outside its registration guard.
   Mismatch is `409 outbox_binding_changed`, without a key or mutation and
   without disclosing the actual binding. Existing access checks remain
   mandatory. The optional precondition is an explicit core input, not HTTP
   logic in the store or domain; HTTP delegates through `ArtifactService`.
   Without these headers the existing rules are unchanged. Regenerate wire
   contracts; do not hand-edit generated files. Add the headers to the allowed
   CORS request headers and expose the read route to the existing route
   inventory/authorization tests. No new MCP outbox command is needed.
   Supported restore renews the epoch; copying an identical database identity
   outside supported restore is outside this fence's guarantee. Use the
   existing state-generation column: no new SQL schema or counter is needed.
   Regenerate the additive binding contract with a minor version bump.

### Ownership and storage

5. Use a dedicated versioned IndexedDB database, `artifree-data-outbox`, with
   indexes by binding, owner, artifact and monotonically increasing sequence.
   Session storage holds a cryptographically random tab owner ID; acquire a
   Web Lock for it before enumerating, modifying or sending any row and hold
   it for the coordinator's lifetime, including disconnect/logout until page
   disposal. A newly generated owner can send only work accepted in its current
   page lifetime. An inherited session owner is a **recovery candidate** even
   after its lock is acquired: a clone can acquire a copied identity before
   the original starts or after a crash. Lock ownership proves exclusion, not
   physical tab continuity. If the candidate's lock is held, never steal it:
   report `owner_busy`, preserve its queue and save its inherited owner ID in
   one separate session-storage candidate slot before allocating a fresh owner
   for independent new work. This slot contains only a bounded owner identifier,
   no payload, token or recovery authority. Cap it at one candidate beside the
   active owner (two owner identities, at most two held owner locks). If a
   distinct unresolved candidate already occupies the slot, or retaining it
   fails, return `recovery_reference_full`/storage failure without overwriting
   either identifier, accepting new work or touching either queue. No silent
   replacement or candidate eviction. That fresh owner cannot inspect, confirm or drain the
   busy candidate's rows. This also applies to a reload overlapping an old live
   page. Coordinate all handles in one document through one registry;
   a second opener cannot bypass the coordinator or make a second sender.
   If Web Locks, session storage or IndexedDB is unavailable, refuse outbox
   initialization explicitly. Once a free inherited candidate lock is acquired,
   first determine whether that owner has **any retained records across all
   bindings**, using bounded existence-only owner-index reads over intents,
   receipts and all retained checkpoints. Do not decode or expose other bindings'
   payloads, identities, URLs, counts or paths through this probe. A failed or
   uncertain probe is not empty: preserve references and report a storage error.
   Only owner-wide emptiness permits the empty-case fresh-owner allocation;
   an empty current-binding query alone never does.
   If records match the authenticated URL/actor/storage epoch, read only that
   binding's bounded recovery metadata and return `recovery_required`; send
   nothing, prepare nothing and perform no trim, enqueue into that candidate
   or deletion until an explicit choice. Apply rule 18 when the actor generation
   changed. Other bindings' rows remain anchored by the same owner reference.
   If no delivery-binding records match but the owner is occupied, return the
   explicit non-delivering state `owner_binding_quarantined`: keep the active
   owner ID and any saved candidate unchanged, hold its lock under the normal
   page-lifetime rules, and refuse new enqueues or a fresh owner allocation for
   this inherited context. Use a fixed generic diagnostic with no previous
   actor/binding identity or payload, and do not treat it as an empty queue or
   let `resumeDelivery`/paging clear it. A different current actor/URL receives
   no recovery/abandonment selection for the old rows. Same-actor/URL records
   with an epoch mismatch may offer rule 17's explicit local
   cleanup metadata/choice, never delivery or automatic removal. The caller may
   restore the original authenticated connection and reopen/retry recovery as
   rule 6 describes; quarantine itself conveys no authority. This deliberately
   limits this inherited context's availability while preserving its data;
   existing direct writes remain available under their ordinary permissions.
   If the owner is globally empty, create a fresh owner for new work instead
   of claiming recovered physical identity. Automatic reload/native-restore delivery is
   not provided where original and copied session identities are ambiguous.
   Do not fall back to timestamps, localStorage
   leases, volatile success, or automatic foreign-owner adoption. This limits
   availability on unsupported/insecure contexts; direct writes still exist.
6. A fresh owner never takes over another owner's queue, including after its
   lock disappears. R08 provides no orphan adoption command. An
   inherited session owner can resume only through explicit recovery under its
   exclusive lock and matching authenticated binding. `confirmRecovery` takes
   the opaque selection returned with `recovery_required`, bound to owner,
   binding, page generation and observed record versions. Recheck all of them
   transactionally; stale selections fail without changes. Confirmation
   authorizes recovery of that logical queue, **not proof that this is the
   original physical tab**, and does not transfer rows to a new owner. Its
   authority is held only in the current page's memory, never persisted as an
   auto-resume flag. Every new page lifetime repeats this gate. A decline or
   cancellation leaves rows intact and sends nothing; it is not disposal.
   For delivery, matching binding includes the authorized actor generation;
   only rule 18's explicit reauthorization can change that generation. Rule 17
   separately permits explicit local abandonment without delivery authority.
   An explicit `resumeDelivery` for retry/access cannot bypass this recovery gate.
   A saved candidate is not automatically probed, adopted, drained or deleted.
   Only `retryRecoveryCandidate()` deliberately attempts its lock again; if
   still held it reports `owner_busy` without waiting or changing rows. If
   acquired, apply rule 5's owner-wide occupancy/binding classification:
   matching records return a fresh `recovery_required` selection after binding
   and record-version validation, mismatched retained records preserve the
   saved owner ID in `owner_binding_quarantined`, and globally empty candidates
   return `recovery_empty` without automatically deleting the reference.
   Return no selection for a different actor/URL. A permitted selection is returned without sending, preparing,
   trimming or deleting. `confirmRecovery` remains a separate explicit choice.
   The recovered queue keeps its original logical owner; it is not moved into
   the fresh owner's queue. Retry confers no proof of physical tab identity.
   Clear the saved reference only by an explicit caller action after holding
   its lock, validating binding and verifying that its queue has no retained
   records **under any binding**, not just the current visible subset; it
   cannot discard payloads or evict an unresolved candidate. Serialize owner
   reference changes with local enqueue/receipt/cleanup transitions. Dropping
   or replacing the last reference to a nonempty owner is forbidden. The
   existing busy-owner branch may replace an active ID only after successfully
   retaining that same ID in the candidate slot; it does not establish emptiness
   or permit later clearing it while any binding still has records.
   Restoring the original configured URL and authenticating as the original
   actor lets a newly opened/revalidated handle use the preserved owner ID and
   find only its matching records. If the ID is in the saved slot, deliberately
   call `retryRecoveryCandidate()` after that authentication. In either case
   return `recovery_required` and require the normal explicit confirmation;
   never switch bindings, infer an actor from a label or send on quarantine
   exit automatically. Successful recovery/trim of one binding does not release
   other bindings' records or their owner anchor. If the original storage epoch
   cannot be restored, same-actor/URL local abandonment remains rule 17's path;
   another actor cannot inspect or clear those rows.
   If a browser loses session storage, retained rows stay dormant; there is no
   orphan discovery/adoption. Explicit disposal is restricted to a current-page
   queue or an explicitly confirmed recovery queue, with its owner lock held
   and binding revalidated. Closing releases the lock only after fencing callbacks
   and settling local transactions; an unresolved HTTP write can still commit
   and is recovered by its saved key. Lock release is not cancellation proof.
7. Enqueue is asynchronous and resolves only after the IndexedDB transaction's
   `complete` event. It validates the operation/path/value using the existing
   client serialization/input rules, snapshots caller data so later mutation cannot change
   it, allocates a sequence and unique request key, stores the full immutable
   intent and updates counters in one transaction. Failure/abort rejects with
   `not_enqueued` and sends nothing. A callback before transaction completion,
   a local overlay or an `IDBRequest` success is not durable acceptance.
   Enqueue acknowledgement and server acknowledgement are distinct results.
   Preserve the existing direct client's `DataInput`/`JsonText` contract:
   ordinary values use its existing serialization checks; `JsonText.text` is
   retained verbatim without parsing/reformatting, and supplied path text is
   not normalized or newly domain-validated in TypeScript. Local admission adds
   only outbox envelope/ownership/dependency/capacity checks. Rust remains the
   authority for path grammar and application-data JSON/domain rejection; a
   deliberately invalid `JsonText` or path is not storage corruption merely
   because the server will reject it. Do not duplicate all Rust validation or
   change existing direct-client rejection behavior to implement the outbox.
8. Persist operation, original supplied path text, immutable value/patch text (none for
   delete), sequence, key, caller-observed base, admitted actor generation,
   authorized-generation checkpoint, state and record version.
   Records are strictly decoded at recovery: wrong version, invalid enums,
   invalid sizes, duplicate sequence/key, inconsistent counters or malformed
   storage-envelope data pause that lane with `storage_corrupt`; do not skip or delete
   the bad head and send its successors. Read at most one bounded page at a
   time. A database schema newer than the client fails closed without writes.
   The local-only v1 schema requires admission-generation metadata. The v2
   schema upgrade checks layout metadata only, without reading or changing
   retained payloads. Under the owner lock and matching binding, exact legacy
   rows/checkpoints decode read-only with authorized generation from their original
   admission generation and an empty ACK checkpoint. Owner-scoped transitions
   persist format 2 atomically, preserving keys, payloads, bases and reservations.
   Upgrading or reading grants no recovery confirmation or delivery authority. If a recognized experimental older
   format lacks generation metadata, preserve the
   rows as `generation_unbound` and require a fresh binding plus explicit
   recovery before setting delivery authority. Never infer generation 1 or the
   current generation, discard rows, or regenerate operation keys during an
   upgrade. Unrecognized formats remain a closed storage error.
   Validate stored field types, frozen-request identity/serialization and
   size/structural invariants, not the semantic validity of raw `JsonText` or
   the server's path/domain rules. Preserve those payload bytes for the server.

### Serialized intent and request identity

9. A lane is `(URL, actor, epoch, owner, artifact)`; all its writes, even at
   disjoint paths, serialize because `base_data_version` covers the whole
   space. At most one HTTP mutation is in flight per coordinator, scheduled
   round-robin across ready lanes. Enqueue order is transaction sequence, not
   completion of application callbacks. Other tabs and direct writes do not
   share this queue and can legitimately produce a data conflict.
10. The first item in a chain uses a base explicitly supplied from the caller's
    canonical read. Later intents may name the immediately preceding local
    sequence as their dependency instead of a numeric base; they explicitly
    mean "apply after this local intent succeeds". On the predecessor's durable
    receipt, fix the successor's base to its returned data version (including
    unchanged writes). This is not a merge with unrelated remote changes.
    Numeric-base intents keep their exact base. No implicit read-latest/rebase,
    coalescing, key reuse with altered data, or best-effort skipping is allowed.
11. Before the first HTTP attempt, commit a `prepared` record containing the
    exact method, body text, base, key and binding. Replace the enqueue-only
    value/patch field with the prepared body in that same transaction; do not
    retain two full copies of a maximum-size value. Reserve body-wrapper and
    receipt/checkpoint bytes at admission. Only after that transaction
    completes may the sender run. Once prepared, operation method/body/base/key
    and actor/URL/storage epoch are immutable. Every automatic retry uses the
    same authority generation too; only rule 18's explicit reauthorization may
    change that separate precondition header. It is not part of the existing
    operation fingerprint and cannot change the stored request identity;
    abort, timeout, response loss, invalid response, process crash and reload
    cannot regenerate its key or base. There is no persisted `sent means
committed` assumption. A prepared record without a durable receipt is an
    unknown outcome that must replay to learn its receipt, not consult a
    current value to guess whether its write happened. Rule 17 also permits
    deliberate abandonment without learning or undoing that outcome.
    A preparation transaction that definitively aborts leaves the already
    accepted intent unprepared, with its original sequence/key/operation and
    accounting intact. It is not `not_enqueued` or an uncertain HTTP outcome:
    no request was dispatched. Enter lane-scoped `preparation_paused` with
    reason `preparation_storage_failed`, `durablyEnqueued: true`, `prepared:
false`, and an opaque `PreparationRetrySelection`. Block the head and
    dependent suffix; schedule neither HTTP nor an automatic local retry.
    Resolve the transaction's terminal event once and fence failed preparation
    callbacks before reporting the pause; late/request-success/duplicate events
    cannot mark an aborted preparation ready, dispatch, or downgrade a later
    committed preparation.
    Only deliberate `retryPreparation(selection)` attempts that one local
    preparation transaction again. Validate token kind, page/owner/binding/
    authority context and current unprepared row/version; stale or consumed
    selections fail and duplicate pending actions coalesce. Preserve the
    original numeric base or resolve the original dependency from its durable
    predecessor receipt, never a latest read/new key or changed intent.
    One action permits one preparation attempt; another abort returns the same
    pause with a fresh selection and needs another deliberate action. Complete
    must be observed before dispatch. Success clears only the preparation
    pause; ordinary sending still requires remaining delivery budget and every
    authority/recovery/conflict gate. Preparation attempts consume no HTTP
    failure budget and reset neither its epoch/counters/backoff nor local
    recovery frontier. `resumeDelivery`, paging, timers, storage/online events
    and callbacks cannot substitute for this local retry action. Its selection
    cannot retry an ACK write or an already prepared request.
    After a crash, explicit owner recovery reads storage truth: an unprepared
    intent may receive its first bounded preparation attempt in that recovery;
    a prepared record keeps its frozen request and replays normally. A further
    preparation failure pauses again. Do not infer either state from an old
    callback or an individual IDB request-success event. Explicit disposal of
    a never-prepared accepted intent remains the guarded unsent-discard path;
    it is not unresolved-delivery abandonment.

### ACK, canonical state and trim

12. Accept an ACK only for an expected HTTP `200` and validated `DataWriteResponse` matching
    the prepared artifact/path, with a safe version: `changed: true` means
    base plus one and `changed: false` means base. `replayed` may be either.
    Any unexpected `2xx` status, or a successful response with malformed,
    oversized, contract-invalid or mismatched body, is `invalid_response` with
    unknown outcome, never an ACK. Retain the exact prepared request and use
    rule 16's bounded retry path, not a new key or an unclassified stop. Commit
    a validated receipt (version, changed, replayed) and advance
    the lane's checkpoint/dependency atomically before releasing the successor.
    Expose `server_acknowledged` only after this transaction's `complete` event.
    If the ACK transaction aborts or otherwise definitively fails to commit,
    count this delivery attempt unsuccessful exactly once and immediately enter
    lane-scoped `retry_paused` with reason `ack_storage_failed` and a typed
    `DeliveryResumeSelection`, regardless of remaining retry budget. Report the
    bounded validated `observedReceipt` and `ackDurable: false`: server success
    was observed, not a failed mutation or a durable local ACK. Retain the
    prepared request and block its successor. Fence the attempt/callback
    generation before reporting the pause; no automatic resend or local ACK
    retry is scheduled while paused.
    Choose **same-key replay only** to recover this failed checkpoint. A valid
    deliberate `resumeDelivery`, or required explicit post-crash recovery,
    revalidates the retained record and authority, then replays the original
    method/body/base/key. Commit the newly validated replay receipt before
    releasing a successor. The old observed receipt is informational; never
    cache/reuse it for a later local-persistence retry or accept a late old
    callback as the resumed attempt's receipt. If checkpointing fails again,
    pause again after that one attempt. Access loss/purge may prevent replay;
    explicit abandonment and its offline-identity limits still apply.
    Resolve a storage transaction's terminal event only once: request success
    is not commit, and stale/double completion/error callbacks cannot promote
    an aborted ACK or downgrade an already durable ACK. A crash before the
    terminal event is observed is resolved by reading storage during explicit
    recovery: a durable ACK follows rule 14's trim-only path; a still-prepared
    row replays. The same distinction applies when revalidating a stale resume
    selection; stored durable ACK state wins, never a volatile receipt guess.
13. Canonical snapshots remain server reads via `getData`/`followData`.
    A receipt states the historical result of one request; its replay version
    can be older than a current snapshot. A notification or snapshot, even an
    equal value/version, cannot acknowledge a particular key. Pending counts,
    unknown outcomes, durable ACKs and optional caller overlays are separate
    from canonical values. A snapshot at or above an ACK version proves that
    read's position, not that the acknowledged value is still present. Do not
    replace a newer canonical snapshot with an ACK or optimistic local data.
    A feed reset fences reads and pauses sends for a new binding check; only
    equal actor/epoch, the authorized actor generation, and applicable
    recovery/access choices may resume, then
    replay prepared work before successors.
14. Trimming is a separate local transaction that removes acknowledged payloads
    and updates storage counters; it never sends HTTP or changes a receipt's
    meaning. Keep a bounded lane checkpoint while dependent intents remain.
    Trim failure is `acknowledged_cleanup_pending`: the receipt is still
    durable, recovery retries deletion, and that row is never resent. Successors
    may proceed using the checkpoint while storage capacity permits. If the
    ACK transaction aborted instead, rule 12's immediate pause and explicit
    same-key replay apply; no local receipt-persistence retry. Distinguish these two
    faults in callbacks and tests. Do not retain an unbounded receipt history.

### Limits, errors and lifecycle

15. Per origin, cap the dedicated database at 256 retained intent rows and
    8 MiB of accounted UTF-8 payload plus metadata; per lane, 64 rows and
    2 MiB. Each row is at most 272 KiB including a metadata budget; the
    existing value limit still applies. Count prepared, blocked and
    acknowledged-but-untrimmed rows. Admission/counters are atomic across
    owners, with a 4 KiB reserve per accepted row for bounded receipt/checkpoint
    metadata included in its accounted size. Admission must prove this reserve
    also covers the prepared-body wrapper without double payload copies.
    Exceeding a cap or browser quota rejects the new enqueue without disturbing
    accepted rows. No eviction, silent volatile mode, or automatic old-item
    deletion. Preparation failure follows rule 11's local-only typed retry;
    ACK failure follows rule 12's explicit same-key replay. Neither is an
    automatic storage/HTTP retry loop.
    Empty lane metadata is deleted after trim. Browser eviction/clear/private
    mode may lose data: report missing storage honestly, without reconstructing
    delivery from a remembered in-memory promise.
16. Network errors, timeouts, `429`, `5xx`, and rule 12's invalid successful
    responses retain the exact prepared request and are retryable unknown
    outcomes. While budget remains, emit `delivery_retrying` with a typed reason (including
    `invalid_response`), `outcome: unknown`, remaining budgets and next delay;
    never echo an invalid response body or mismatched identifiers. Repeated
    failures use the same bounded policy below and enter `retry_paused` with
    that reason/scope and a `DeliveryResumeSelection` on budget exhaustion,
    without scheduling a retry timer or another `delivery_retrying` state then.
    A valid explicit `resumeDelivery` then renews the bounded epoch and retries
    the same frozen request, subject to every existing authority/conflict gate.
    There is no permanent unactionable invalid-response state or unbounded
    replay loop. ACK persistence failure is different: rule 12 pauses it
    immediately without automatic retry, even when budget remains.
    A redirect rejected by Fetch `redirect: 'error'` normally exposes no HTTP
    status or `Location`; do not inspect exception text to invent a redirect
    code or destination. For a dispatched mutation this is the existing
    `network_error`/unknown-outcome path: count one failure, preserve the exact
    request and use the same 8/32 epoch/backoff limits and explicit exhaustion
    resume. The original destination may have committed before returning a
    redirect; no ACK or non-delivery claim is inferred. Every allowed retry
    addresses only the original bound URL with redirect rejection still set.
    If a conforming test/transport exposes a non-followed `3xx`, classify it as
    `unclassified_error_response` below and pause; never use its `Location`.
    Binding and revalidation reads are not mutation attempts. Each explicit
    open/recovery/revalidation action permits one such read; redirect rejection
    or another read failure establishes no new binding/authority and schedules
    no automatic binding retry. A redirect-induced initial binding failure
    rejects initialization with `network_error` and accepts no intent; other
    read failures retain their actual status/error classification. Existing work remains paused
    on revalidation failure, with the applicable recovery/delivery selection
    refreshed for a deliberate retry of the same bound read. Failure neither
    consumes nor refills the mutation budget, opens a new delivery epoch, nor
    authorizes a mutation. A deliberate delivery-resume/recovery action renews
    its epoch only after required binding/authority checks succeed. Paging,
    preparation retry, timers and callbacks cannot turn metadata-read failures
    into an automatic binding retry or bypass existing pauses. No redirected
    response can authorize local cleanup; rule 17's existing locally fenced
    offline-cleanup conditions remain unchanged.
    Classify non-success responses from retained HTTP metadata, not just the
    transport exception's `invalid_response`/`network_error` label. Capture
    status and parsed `Retry-After` before reading the bounded error body, and
    preserve them if parsing, size enforcement, cancellation or stall/whole-
    attempt timeout fails after headers. HTTP `429` and `5xx` remain the bounded
    retry cases above, including malformed/oversized/stalled bodies; retain the
    clamped `Retry-After` floor. HTTP `401`/`403`/`404` enter rule 18's access
    pause regardless of body validity; a `401` invalidates the local auth handle
    for offline cleanup as already required. Count each dispatched failed
    attempt once, with no retry timer for these access pauses.
    Other non-2xx responses use rules 17–18 only when the bounded body contains
    a recognized error with a contract-compatible status and valid required
    fields. If a `409` cannot distinguish conflict from binding loss, or an
    unexpected `4xx`/other non-success status or invalid/unrecognized error body
    supplies no safe classification, immediately enter lane-scoped
    `retry_paused` with reason `unclassified_error_response`, the observed
    status and a `DeliveryResumeSelection`. Count one failed HTTP attempt,
    retain the exact prepared request and block successors; do not automatically
    replay, infer a conflict/current version/permission, rebase, adopt authority
    or expose the raw error body/message. Generic transport failure without
    usable response headers remains the network retry case, not guessed `409`.
    For the unclassified pause, a deliberate `resumeDelivery` must first obtain
    a fresh server binding and satisfy the existing owner/actor/URL/epoch and
    generation-confirmation gates. It then permits an exact-request probe,
    without changing method/body/base/key. An unclassified response pauses
    again after that one attempt; a valid classified response takes its normal
    ACK, bounded retry, conflict or access path. A changed binding cannot be
    silently authorized by this probe or action. No canonical read is used to
    guess whether an unknown request committed. Guarded explicit abandonment
    remains available when replay is unavailable, with the same offline limits.
    Use 0040 rule 17's bounded retry policy with the client's injected clock
    and random source: 0.5 s doubling to 30 s, jitter multiplier in [0.5, 1),
    with parsed Retry-After as a floor clamped to one hour. A **delivery-budget
    epoch** permits at most eight consecutive unsuccessful HTTP mutation
    attempts per lane and 32 unsuccessful attempts total across the coordinator.
    Each unsuccessful delivery attempt counts once, including an invalid
    successful response, any failed non-2xx attempt, a definitive ACK-transaction abort after a valid
    receipt, or a timeout/abort whose request may have reached the server.
    Parser failure followed by transport cancellation, or duplicate storage
    error callbacks, never count the same attempt twice. HTTP `2xx` or a
    volatile valid receipt does not reset any counter. A durable ACK resets that lane's
    consecutive-failure count and backoff only; it never resets the aggregate
    32-failure count within the epoch. Successes consume no failure budget, so
    more than eight healthy writes can drain. Eight consecutive failures pause
    that lane; aggregate exhaustion pauses all sending. Report `retry_paused`
    with the applicable scope and exhausted budget.
    Opening an explicitly opted-in coordinator for a fresh owner starts its
    initial epoch. A deliberate caller/user
    `resumeDelivery(selection: DeliveryResumeSelection)`, or required explicit
    recovery/reauthorization confirmation, starts a new bounded epoch with both
    failure counters reset and fresh backoff; this is an intentional new budget,
    not a lifetime 32-attempt guarantee. Serialize the action with the active
    attempt: settle or abort it under the existing deadline before starting the
    new epoch. The delivery-resume selection identifies the current paused
    delivery state/epoch and page/owner/binding/authority context; runtime validation
    rejects another action's token, stale context or consumed selection.
    Duplicate/pending delivery-resume actions coalesce without replenishing
    an active epoch repeatedly. Authorization, conflict, preparation-storage and inherited-owner
    recovery gates remain in force; resetting budget grants no authority.
    Reconnect, online events, new enqueues, ACK progress, timers and status/error
    callbacks never open a new epoch, reset the aggregate counter or invoke
    `resumeDelivery` automatically. A `RecoveryContinuation` only advances local
    recovery processing under rule 19: it cannot reset/refill this epoch or
    bypass an exhausted budget, even when the caller requests many pages.
    A new page with retained work still needs explicit
    recovery, so reload is not an automatic budget reset. Further deliberate
    actions may authorize further bounded epochs, but there is no automatic
    reset loop after exhaustion. No busy
    offline polling and no online-event retry storm. Start/reconnect uses one
    coalesced wake-up; abort and close cancel timers immediately. HTTP header,
    body size and stall budgets use the existing bounded transport primitives.
    Add a 15-second pre-header deadline and a 60-second whole-attempt deadline
    covering request upload, headers and response body (also for the binding
    read); abort at the earlier applicable deadline and retain prepared work
    as unknown. 0040's body/stall limits alone do not bound absent headers.
    Late completion after the attempt generation changed cannot ACK, mutate
    storage or start a successor.
17. `data_conflict` blocks the head and its dependent suffix, retaining the
    original key/request and reporting the server's current version.
    `data_path_conflict`, invalid input, size rejection and key reuse are also
    blocked failures, not infinite retries. Resolution is explicit: inspect
    retained intent and canonical data, then deliberately dispose through the
    phase-specific actions below, or enqueue an independently chosen new
    intent with a new key/base. No automatic merge or mutation of prepared
    payloads. Independent lanes continue.
    `discardUnsent(UnsentDiscardSelection)` accepts only a never-prepared head
    and its never-dispatched dependent suffix, including a confirmed preparation
    abort. It reports `discarded_unsent`; a prepared row, durable ACK or
    uncertain/in-flight mutation is ineligible, even if a caller believes the
    prepared request probably was not sent. `discardRejected(RejectedDiscardSelection)`
    accepts only a blocked head with a validated, definitive known-rejection
    observation bound to this request/current attempt, and its eligible unsent
    dependent suffix; it reports `discarded_rejected`, never an ACK. A network,
    invalid/unclassified response, access pause, ACK-storage failure or any
    unresolved earlier delivery is not that proof. Do not downgrade an unknown
    head just because a later attempt was refused; its exact-key resume/recovery
    path remains available via `retry_paused` reason `unresolved_after_rejection`
    and a `DeliveryResumeSelection`, without automatic retry. Choosing local removal instead requires
    `abandonUnresolved`. A rejection describes that attempt, not global key
    non-use or rollback. Do not reconstruct missing rejection evidence from a
    local payload/snapshot or caller-provided error; after a crash a prepared
    row without durable resolution is unknown under rule 11.
    Both discard selections are distinct, opaque, one-action choices identifying
    the owner, original binding, page/profile/authority context, exact head,
    complete transitive dependent set and observed record versions/phases.
    Use rule 6's owner lock and the same authenticated actor/URL and locally
    fenced online/offline cleanup safeguards as the local-only rule 17 path
    below; neither selection grants delivery or permits another actor's rows
    to be inspected/deleted. For recovered candidates a deliberate disposal
    choice authorizes only local cleanup, not implicit delivery confirmation.
    This is the corresponding local-only exception to rule 6's confirmed-queue
    disposal restriction, with the same cleanup identity limitations below.
    Serialize disposal with enqueue/preparation/receipt transitions, fence the
    lane's callbacks and cancel its timers/pending local preparation before
    committing. Inside one bounded IDB transaction (at most 64 rows/2 MiB),
    revalidate kind/context, head, full dependency closure, versions and phase
    eligibility, then remove exactly the selected rows and unneeded checkpoints
    and update current lane/origin row/byte/reserved accounting atomically.
    A new dependent, changed phase/version, lost lock or stale/wrong-kind token
    rejects the whole selection; never silently expand it or partially delete.
    A transaction abort leaves all rows/counters intact and the lane paused;
    another deliberate fresh choice is needed. Success is reported only on
    transaction completion. Late callbacks cannot resurrect rows, ACK, prepare
    or dispatch successors. Independent surviving intents remain intact and
    paused until the appropriate existing explicit action; no budget/authority
    is renewed by disposal. Discard has no HTTP/cancellation/key-release side
    effect, never rewrites retained requests or makes an old key reusable; any
    replacement intent has a fresh key and caller-chosen base. Durable ACKs use
    trim, not either discard or unknown abandonment. Owner-reference cleanup
    still requires rule 6's all-binding emptiness; removing one subset does not
    orphan another binding's records.
    For an unknown outcome, replay is the
    way to resolve the receipt, but is not a prerequisite for explicit local
    abandonment: purge makes replay permanently unavailable under 0031.
    `abandonUnresolved(UnresolvedAbandonSelection)` requires a deliberate caller/user choice
    identifying the unresolved head and its entire dependent suffix, with rule
    6's owner-lock, binding and version-selection safeguards. For a recovered
    candidate, this command itself is the explicit local-cleanup choice;
    it does not require a prior delivery-confirming `confirmRecovery` or grant
    delivery authority. A same-authenticated-actor/URL handle, the original
    owner's exclusive lock, and a current selection matching the retained
    records' original binding and versions are required. A different current
    storage epoch or actor generation is presented explicitly with the original
    target: it may authorize local removal only, never rebinding or delivery.
    Expose bounded cleanup metadata, not payloads across a mismatched binding.
    This is the local-abandonment exception to rule 6's confirmed-queue disposal
    restriction. Offline cleanup requires a still-current locally fenced
    authenticated handle: this page obtained its actor/URL from a successful
    server binding response, still holds that owner's lock, and the handle's
    captured profile/credential generation equals the coordinator's current
    generation. It must remain open and locally authenticated, with no logout,
    profile/credential replacement, `401`, or other unresolved authentication
    invalidation since that confirmation. A caller-supplied actor label, saved
    token fingerprint, old handle or inherited session ID is not this proof.
    Check these conditions again immediately before the deletion transaction;
    inability to establish them offline retains the rows and fails closed.
    Only the local identity/cleanup choice is established: offline operation
    cannot prove current server access or authorize delivery. A new page,
    detached handle or unauthenticated opener cannot guess
    an actor or recover/abandon somebody else's rows: reauthenticate the same
    actor first. Never weaken the owner lock or let a new profile expose old
    payloads. No successful
    artifact read, replay or proof of purge is required. A `404` alone never
    selects abandonment or reveals whether an artifact was purged. Fence this
    lane's attempt/callback generation and abort its request first, then remove
    the selected rows, unneeded checkpoints and accounting in one bounded
    IndexedDB transaction (at most the lane's 64 rows/2 MiB); report success
    only on transaction completion. Failure leaves rows/counters intact and
    the lane paused. Survivors stay paused until explicit `resumeDelivery`. Late replies
    cannot recreate deleted rows, ACK them or release successors. This is
    local cleanup only: no server endpoint, key cancellation/reuse, fabricated
    ACK, or server-data access. Report `abandoned_unknown`: the request may
    already have committed or may still commit after abort; abandonment proves
    neither non-delivery nor rollback and cannot undo it. Do not keep an
    unbounded payload/tombstone history after successful cleanup.
    Known durable ACKs are not unresolved: preserve their acknowledged meaning,
    never replay or label them `abandoned_unknown`; their payload cleanup uses
    normal trim under the same applicable owner/selection safeguards. Count
    and report selected unresolved descendants separately from acknowledged
    cleanup, and retain unrelated rows/checkpoints needed by surviving intents.
18. 401, 403, 404 and binding mismatch pause, retain and report access/binding
    uncertainty; never interpret them as proof an earlier attempt did not
    commit. For `401`/`403`/`404`, return `retry_paused` with reason
    `access_unavailable`, the observed HTTP status and a `DeliveryResumeSelection`;
    recognized `outbox_binding_changed` uses reason `binding_changed`. A
    selection is not proof access returned and cannot skip the checks below.
    A fresh binding/access check and explicit `resumeDelivery` are needed after
    credentials/access return. Even if disable/re-enable was never observed as
    a `401`, the stored generation fence rejects the old authority on the next
    send. A fresh binding may reveal a different generation for the same
    actor/URL/storage epoch; pause all affected lanes, invalidate prior
    selections and bump the local authority/callback generation before any
    reauthorization. Return `recovery_required` with reason
    `actor_generation_changed`; retry or `resumeDelivery` cannot adopt it. A new
    explicit `confirmRecovery(selection)` must authorize that current enabled
    generation for the retained intents, under the owner lock and observed
    record-version checks. Persist the new authority checkpoint atomically,
    keeping original admission generations and immutable operation identities.
    Replaying a lost-ACK request then uses its original method/body/base/key
    with only the new explicitly authorized generation precondition; it may
    return the historical receipt without committing again. Neither a receipt
    from the previous generation nor a late old callback grants authority for
    successors. Once a changed generation is observed, callbacks captured under
    the old authority cannot update rows, ACK, or start successors even after
    reauthorization. A response received before the transition is observed can
    record a historical ACK, but the next request's transactional fence still
    prevents old-authority delivery. Logout/profile replacement fences old callbacks,
    aborts work and detaches that coordinator, without deleting its rows.
    Artifact trash/purge and future runtime capability clear do not clear all
    pending intents. R08 does not implement grant-aware runtime delivery or
    claim S02 sharing/role behavior. An authorization pause must not expose
    another actor's payloads through a newly bound handle.
19. Recovery holds the owner lock, validates binding, obtains rule 6's explicit
    choice for inherited/ambiguous ownership, and loads at most 32 rows
    per page and the configured row/byte caps. It preserves sequence, trims
    durable receipts first, replays unknown prepared requests and prepares only
    eligible heads. Invalid storage halts its lane. A recovery-processing slice
    performs at most 256 local state transitions, yielding between its pages;
    if work remains, return `recovery_paused` and a `RecoveryContinuation` for
    the saved durable frontier. Only
    `continueRecovery(continuation: RecoveryContinuation)` advances that frontier
    into another bounded local slice, without rescanning from the beginning.
    Runtime-check the token kind, page/owner/binding/authority context and
    frontier version; stale or consumed tokens fail without changing work or
    delivery budgets. This action replenishes only the local slice's work
    allowance. It does not create a delivery-budget epoch, reset failure
    counters/backoff, grant authority or clear delivery/conflict/preparation pauses. Normal
    eligible attempts/ACKs during the slice still obey and consume/update the
    existing rule 16 epoch; paging itself never refills its budget.
    `recovery_paused` and `retry_paused` may coexist: continuing recovery can do
    eligible local reads/cleanup, but cannot send while delivery is paused.
    Conversely, deliberate `resumeDelivery` may renew its retry epoch but does
    not reset the recovery frontier or bypass the 256-transition slice limit;
    a paused recovery still needs its continuation. Neither action can be
    substituted for the other's typed token or explicit authority confirmation.
    Issue these action tokens with their current typed pause states; refresh
    continuation metadata when its durable frontier changes, without resetting
    delivery budgets or treating refreshed metadata as a delivery-resume choice.
    Recovery never waits for canonical follower notifications to identify ACKs.
    Claims of native browser-process recovery require an actual owned-profile
    termination and native session restoration, followed by explicit recovery
    where identity is ambiguous; reload or injected session IDs must be reported
    as their narrower tests. Neither test success nor confirmation proves a
    physical-tab identity guarantee unavailable from the browser primitives.

## Acceptance and rejection tests (sender portions remain proposed)

| ID  | Required behavior and failure proof                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| B1  | Binding read returns only the caller’s actor, actor_generation string and existing epoch; disabled/revoked credentials fail. Reject partial header triples, malformed or overflow generation strings and inconsistent binding responses. With well-formed headers, foreign/missing/trash/purged target remains identical404 before semantic fence checks, including a replayable key or wrong expected actor; an accessible target with stale actor/epoch/generation gives409 before key replay/base checks and consumes no key. Malformed fence input still follows ordinary input-error precedence. Swap actor or supported-restore epoch between handshake and mutation: no write/key/replay. Generation cases are B12. Remove transaction fence and show rejection tests fail. Unfenced direct writes remain compatible.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| B2  | Real IndexedDB enqueue transaction complete survives reload. Hold request-success before transaction abort: enqueue rejects and zero HTTP writes. Quota/transaction exceptions preserve older rows and counters. Mutate caller objects after enqueue: transmitted request stays fixed.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| B3  | Real tabs with copied session storage: original holds lock, clone wins before original starts, clone wins after crash, and reload overlaps an old live page. Busy candidate is preserved in the bounded saved slot before a fresh owner; explicit retry after lock release revalidates but never automatically sends. Classify each free inherited owner: globally empty across all bindings permits fresh-owner allocation; matching retained URL/actor/epoch records yield recovery_required with no mutation/preparation/trim/delete before an explicit valid choice; retained records without a matching delivery binding yield owner_binding_quarantined and preserve IDs. A saved globally empty candidate returns recovery_empty and requires explicit reference clear. Cover slot full, persistence/existence-probe failure and stale/wrong-binding selection; no false-empty or overwritten anchor. Only current matching confirmation can authorize replay, while disposal uses its separate typed choice. No persisted auto-resume flag, physical-origin proof, boot/online/resumeDelivery bypass or second same-document sender; unsupported locks/storage fail closed.                                                                                                                                                                                                                                                                    |
| B4  | Two writes at different paths serialize across one data version; dependency uses predecessor receipt including no-op/replay. Another tab or direct write in the same actor space produces a blocked conflict without automatic rebase. Another actor is rejected by binding/access checks without changing either actor’s data or the retained queue; shared-writer permission is not implemented before S02. Head conflict leaves dependent suffix untouched and other lane progresses.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| B5  | Kill/reload at enqueue completion, preparation completion, server commit before response, validated response before receipt commit, receipt commit before trim, and trim abort. Recovery first pauses with preserved records and no automatic HTTP; after explicit recovery assert original key/body/base replay, exactly one server mutation/change record, and no duplicate dependent write. Remove each relevant guard and observe its test fail.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| B6  | ACK persistence abort is server success observed but no durable ACK: count once, immediately retry_paused with reason ack_storage_failed, no automatic resend or local-persistence retry; only valid explicit resumeDelivery or post-crash recovery performs exact same-key replay and persists its fresh validated receipt; ACK success plus trim failure is retained acknowledgement. The latter is not retransmitted after reload and can advance dependencies. Malformed/oversize/stalled/mismatched success responses never advance.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| B7  | Canonical follower can observe a write before ACK, and a replay can ACK below current canonical version; neither falsely ACKs another key nor rolls back canonical state. A no-op with no event still ACKs. Reset pauses/fences old reads and requires matching binding to resume.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| B8  | Atomic admission at each row/byte boundary under competing owners; counted blocked/untrimmed rows and reserved metadata. Maximum-size intent prepares and ACKs without double-payload accounting failure. Malformed/new-schema records halt safely without deletion or successor send. More than eight healthy writes drain. Eight consecutive lane failures pause that lane; 32 total failures pause the coordinator even with interleaved ACKs. Only deliberate resumeDelivery with its delivery-resume selection or required recovery confirmation starts a new bounded epoch; duplicate concurrent actions coalesce and settle the old attempt before reset. Reconnect/online/enqueue/ACK/callback/timer cannot reset budget or send after exhaustion; reload of retained work remains recovery-gated. New epoch does not bypass authorization or conflicts. Bounded reads, transitions, retry count, timer cancellation, header silence, upload/body deadlines and late completion under deterministic clock/network faults.                                                                                                                                                                                                                                                                                                                                                                                                                      |
| B9  | Lost response followed by 401/403/404 retains unknown request. Same binding and authorized generation plus explicit resumeDelivery permits exact replay; changed generation requires B12 reauthorization and cannot be silently copied. Different actor/URL/epoch stays paused. Logout and late callbacks cannot update/send the new handle or erase intents. Unresolved retention ends only through a validated replay receipt and normal trim, or an explicit valid B11 abandonment choice; never an access error alone.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| B10 | Terminate a real owned browser profile after durable enqueue while writes cannot reach the server, then native session restore without injecting owner data. Inherited owner remains recovery_required with zero sends until explicit choice; after confirmation under its lock and matching binding, drain and verify independent canonical commit. Confirmation authorizes the logical queue and does not prove original physical-tab identity. Missing owner leaves dormant rows; report OS/browser limitations and do not call reload a native crash success.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| B11 | Server commit with lost ACK, then purge: ordinary data routes still give the same 404, no receipt is inferred, and explicit abandonUnresolved removes head/dependent suffix and recovers quota without replay or a purge probe. Also allow deliberate abandonment while delivery remains uncertain without claiming purge. Assert no HTTP mutation or delivery authority from abandonment, no fabricated ACK/key changes/undo claim, no automatic removal after access loss, and no survivor send until explicit resumeDelivery. Held response released after abandonment cannot resurrect rows or release successors. Selection/lock/binding mismatch or injected IDB abort preserves rows and counters; successful deletion is bounded to the 64-row/2 MiB lane and releases reserved accounting. A recovered candidate can explicitly abandon without first granting delivery. Supported restore with epoch mismatch allows warned same-actor/URL local cleanup without rebinding; offline cleanup requires the same page’s still-open locally authenticated handle, a held owner lock and unchanged current profile/credential generation; no 401 or unresolved auth invalidation may have occurred, and uncertain local authority fails closed, while detached/new unauthenticated or different-actor handles cannot inspect or remove old payloads. Known durable ACKs keep their receipt meaning and use trim, not unknown-outcome abandonment. |
| B12 | Disable→enable between binding and first send, between chained writes, or while a request authenticated in the old generation waits before its transaction: current actor/epoch are equal but the old generation cannot commit or replay/consume a key. No observed 401 is required. Unknown lost-ACK work stays pending through the transition; only fresh binding plus explicit generation-change confirmation permits the original method/body/base/key to replay with a new authority header and exactly one historical mutation. Old callbacks cannot ACK or release successors after change detection/reauthorization. A write committed before disable is not undone; its receipt is historical authority only. Repeated same-state administration does not invent a generation change, same-generation token rotation does not bypass other gates, generation above JS safe integer round-trips as a decimal string, and missing-generation recognized legacy rows stay generation_unbound until explicit recovery. Break the transactional check or automatic-adoption guard and prove these tests fail.                                                                                                                                                                                                                                                                                                                                      |
| B13 | Reject credential-bearing base URL (username-only, password, percent-encoded userinfo), query/fragment, unsupported scheme or relative form before any owner/IDB write or network call. Diagnostics and callbacks never echo the raw URL or parser cause. Empty/root-relative and absolute credential-free HTTP(S) normalize to one base used by both storage and transport; test host/default-port/trailing-slash/base-path equivalence. Unsafe stored binding is quarantined without delivery or automatic credential stripping/rebinding. Existing direct client behavior is unchanged.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| B14 | Binding read snapshot generation unequal to AuthContext, or disable/enable before final current-user confirmation, returns the same401 as invalid authentication with no actor/epoch/generation fields; no409 on binding reads. A valid snapshot plus confirmation returns200. Send raw URI binding?, binding?x=1 and binding?& without client URL normalization: authenticated GET/HEAD reject400, while unauthenticated requests retain401 precedence. HEAD applies identical checks, no-store and statuses for success, stale authentication and invalid query/body, always with an empty body; client must obtain binding through GET, not HEAD. Verify route inventory/method coverage and retain existing CORS/other-method behavior.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| B15 | Drive recovery across multiple 32-row pages and the 256-local-transition pause while retry counters are nonzero; each continueRecovery advances only its durable frontier and preserves delivery epoch and remaining retry budget, apart from ordinary attempts/ACKs under rule 16. Exhaust aggregate 32 failures, repeatedly continue local recovery, and assert no new HTTP mutation, counter reset, backoff reset or cleared retry pause. Runtime rejection of a RecoveryContinuation passed to resumeDelivery, a delivery-resume selection passed to continueRecovery, and stale/consumed/wrong-context tokens leaves both budgets and authority unchanged. Only a valid deliberate resumeDelivery starts exactly one fresh bounded epoch; duplicate pending actions coalesce, recovery frontier remains stable and recovery pause still needs its continuation. Break paging’s no-refill guard and prove the test fails.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| B16 | Commit the first server mutation but replace success with malformed JSON, oversized body, wrong artifact/path/version, or unexpected 2xx status. Each attempt remains prepared/unknown and consumes exactly one existing 8/32 failure unit, including body cancellation callbacks. If budget remains, emit delivery_retrying reason invalid_response; exhaustion emits only retry_paused with no retry timer. Replays preserve method/body/base/key and cause no second mutation/feed row. Exhaustion yields typed retry_paused, no sends until valid explicit resumeDelivery; a valid replay receipt plus ACK transaction complete can then advance. Raw bad body/foreign identifiers never leak to callbacks. Mutate the no-ACK validation or failure-count/budget guard and prove these cases fail.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| B17 | Observe a valid server receipt, inject real ACK-transaction abort, and assert one failure counted, bounded observedReceipt with ackDurable: false, immediate ack_storage_failed pause, zero further network or ACK-persistence attempts and blocked successor. Explicit resumeDelivery replays exact original operation and only its newly validated receipt plus committed ACK advances once; a continuing disk fault pauses again after one replay. Release stale/duplicate response and transaction callbacks before/after resume: none can ACK, double-count, downgrade a durable ACK or release successors. Crash before terminal storage event: persisted ACK trims without HTTP; prepared state requires explicit recovery and same-key replay. Purge/access loss after the observed receipt still permits guarded explicit abandonment without claiming rollback or replaying automatically. Break the abort pause, terminal-event or callback-generation guard and show the corresponding fault test fails.                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| B18 | Under one free inherited owner, enqueue actorA’s work, then open after logout/reload as actorB at the same URL: current-binding lookup is empty but owner-wide presence is not. Assert owner_binding_quarantined, unchanged active/saved owner IDs, zero new enqueue/HTTP/trim/delete/fresh-owner allocation and a generic diagnostic with no actorA identity, URL, count, path, value or selection. Restore authenticated actorA’s original binding: same owner yields recovery_required, no send before explicit confirmation, then exact original operation replay. Repeat with another URL, saved candidate, occupied candidate slot and storage-probe failure; no reference is overwritten or treated as empty. Mixed matching/foreign bindings may recover and trim only the matching subset; empty-case owner rotation/candidate clearing must fail while any other binding’s intent/receipt/retained checkpoint remains. Same-actor/URL epoch mismatch permits only guarded explicit local abandonment; full owner emptiness is required for empty-case fresh-owner allocation or explicit saved-reference clear; the busy-owner branch still must save its reference before replacing the active ID. Break owner-wide existence or reference-preservation guard and prove these cases fail.                                                                                                                                                   |
| B19 | Durably enqueue, expose IDB request success but abort preparation transaction: original unprepared intent/key/sequence/dependency and counters remain, preparation_paused reports durablyEnqueued true/prepared false, no HTTP or automatic local retry. One valid retryPreparation permits one local attempt; repeated abort requires a new deliberate action, stale/duplicate/wrong-phase tokens cannot retry an ACK or prepared record, and late callbacks cannot dispatch or downgrade state. Successful preparation uses the original numeric base/durable predecessor receipt, sends only after commit and only if existing delivery/authority gates allow. Neither local retry nor recovery paging refills 8/32 budget or clears retry/auth/conflict pauses. Crash with unprepared versus committed-prepared storage follows its respective explicit recovery path; unsent discard remains local. Break commit-before-dispatch, pause or no-budget-reset guards and prove these cases fail.                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| B20 | Supply malformed, oversized and stalled 409 bodies and unexpected 4xx responses, including incompatible status/error-code pairs: exactly one failed mutation attempt, immediate unclassified_error_response pause, no automatic mutation retry, guessed conflict/version, authority update or raw-body leak. Only fresh binding plus valid explicit resumeDelivery permits the same frozen-request probe; another unclassified response pauses again after one. A changed actor/URL/epoch/generation cannot be silently adopted, and classified conflict/access responses use their existing gates. Invalid-body 401/403/404 still access-pause; 401 invalidates offline identity proof. Invalid-body 429/5xx retain status/bounded Retry-After and use 8/32 backoff despite body timeout/cancellation; no headers uses network policy. Assert one count per attempt, successor blocked, exact key/body/base preserved, and abandonment/offline authority rules unchanged. Break status retention or unclassified-response no-autoretry guard and prove these cases fail.                                                                                                                                                                                                                                                                                                                                                                              |
| B21 | Use real browser Fetch and owned HTTP redirect servers: binding/revalidation GET and mutation PUT/PATCH/DELETE endpoints induce 301/302/303/307/308 to both another path on the same origin and an allowed cross-origin collector. Every outbox dispatch, retry and probe carries redirect:error; target receives no followed HTTP request, body marker, idempotency key, outbox fence or credential, and no binding/ACK is accepted. Configure target/CORS to accept follow attempts, then weaken the guard to follow and prove 307/308 deliver the actual body/key/fence to that target so rejection is not a false CORS pass. Have original destination commit then 307/308: rejected Fetch counts one network-error unknown failure per mutation, retries only original bound URL within 8/32 budget, pauses on exhaustion and never duplicates commit; after explicit resume and redirect removal, exact same-key replay can ACK. Binding/revalidation redirect failure makes no mutation-budget change/authority update, permits no mutation or automatic read retry, and a fresh explicit action still uses redirect:error on the original URL. Exposed non-followed 3xx pauses without consuming Location. Break final-dispatch or retry-helper redirect mode and prove these cases fail.                                                                                                                                                      |
| B22 | For a never-prepared accepted head, discardUnsent removes only the selected head and complete unsent dependent closure and reports discarded_unsent. For a validated definitive known-rejection head (data_conflict/path conflict/input/size/key-reuse cases), discardRejected reports discarded_rejected; unknown prior delivery, prepared-uncertain, ACK-storage-failed, invalid/unclassified response or access pause cannot obtain/use that selection and retains exact-key resume/recovery or explicit abandonUnresolved. Verify wrong-kind/stale/consumed/wrong-owner/actor/binding/phase tokens, a newly added dependent and a concurrent preparation/ACK transition reject atomically without row/quota change. Inject IDB abort after deletes but before counters: all rows/checkpoints/reserved accounting remain, lane pauses, no HTTP. On success count actual deleted rows/bytes/reserves once, preserve independent rows/foreign binding anchors and paused survivors, block late callback resurrection/ACK/dispatch, never release server keys or reset delivery budgets. Break closure, fencing, eligibility or atomic-accounting guard and prove rejection/fault tests fail.                                                                                                                                                                                                                                                          |
| B23 | Enqueue ordinary values through existing direct-client serialization and preserve raw JsonText.text plus supplied path exactly across reload/preparation/retry. Include malformed JSON, duplicate-member or unsafe-number JsonText and invalid path strings that fit local caps: outbox must not parse/round/reformat/normalize or call these storage_corrupt; the real Rust server supplies its existing rejection. Ordinary getter/class/unsupported-value serialization failures stay the existing client behavior; local envelope corruption is still rejected. A first definitive rejection can expose a RejectedDiscardSelection, while unresolved prior delivery retains unknown cleanup semantics. Prove newly added TS domain validation or payload normalization breaks compatibility tests.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |

Use independently authored fixtures and the repository's isolated verification
workflow. Unit fault seams prove state transitions; real-browser IndexedDB,
Web Locks and real-server tests prove boundaries. Rejection tests must fail
when their guarded behavior is broken. Run `npm run check`, `npm run build`
and relevant browser smokes for each code slice, with existing gates intact.
This documentation change has not executed these future acceptance cases.

## Proposed PR slices

| Slice | Deliverable                                                                                                                                                                                                                                                                   | Dependencies and acceptance                                                                                                                                 |
| ----- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R08a  | Authenticated binding read and optional transactional actor/epoch/state-generation fence, generated contracts and route/authorization coverage. No automatic queue.                                                                                                           | R07 server basis, 0031/0036/0027; B1/B12/B14 server cases, direct-write regressions.                                                                        |
| R08b  | Dedicated browser store, page owner coordinator, ambiguity gate, bounded retained candidate and explicit local owner recovery, durable admission, caps, strict recovery decoder and typed never-prepared disposal. No sender or delivery/preparation-retry lifecycle enabled. | R08a; only the local-storage/owner/metadata-read subcases listed below. B15 and B19 are excluded.                                                           |
| R08c  | Opt-in serialized sender, immutable preparation, ACK/checkpoint/trim, explicit conflicts, typed rejection disposal, abandonment and access-generation pauses, bounded retries and typed lifecycle API.                                                                        | R08a–b, R07c; sender/fault portions of B2–B9, B11–B13 and B15–B23, including B15 and B19 in full and fail-first proofs. Native-process B10 belongs to R08d. |
| R08d  | Real-browser explicit crash recovery and interleaving/fault verification, public API guide and precise supported-browser/recovery limits.                                                                                                                                     | R08c; B10 native-process restoration, end-to-end B2–B23, full gates and both browser engines where supported.                                               |

R08b's referenced cases are explicitly limited to behavior it implements:

- **B2:** durable enqueue completion/abort, reload retention and immutable local
  intent snapshots. Transmitted-request checks start in R08c.
- **B3/B18:** owner locks, duplicate/inherited owner selection, binding
  quarantine, candidate/reference preservation and local recovery choices.
  Actual mutation dispatch, same-key replay and ACK advancement start in R08c.
- **B8/B12:** local row/byte quotas, accounting, strict storage decoding,
  bounded local reads/transitions and legacy generation-unbound retention.
  HTTP failure budgets, backoff, response validation and generation-fenced
  delivery are not R08b acceptance requirements.
- **B13/B21:** URL admission/normalization and authenticated binding-read-only
  transport, including redirect rejection before establishing identity or
  accepting an intent. Mutation redirect, retry-budget and receipt checks
  start in R08c.

- **B22/B23:** typed never-prepared disposal, local dependency-closure/atomic
  accounting faults, existing-client admission and verbatim local snapshots.
  Prepared/rejected-head disposal, actual server rejection, transmitted bytes
  and sender/callback interleavings start in R08c.

These are named local/metadata subcases, not a claim that the whole composite
case passes in R08b. **B15 and B19 belong to R08c in full**, including their
paging-versus-delivery-budget, preparation/dispatch and typed-action checks.
Every actual sender/replay/ACK clause of a mixed case also belongs to R08c.
R08d supplies B10's actual native-process evidence and the complete browser/
server recovery matrix; earlier local or injected-fault tests cannot be reported
as native-process restoration. Each slice still runs its full required quality
gates and applicable smoke checks; this scope assignment does not weaken them.

No slice may advertise R08 complete before the end-to-end recovery acceptance
has passed. Runtime-frame data access, UI adoption, orphan transfer, automatic
merge, S02 roles and Docs staging remain separate proposed work. Implementation
reports must identify which slices actually shipped and which tests actually ran.

## R08c-1: internal storage lifecycle

The dedicated store strictly decodes unprepared, prepared and acknowledged rows.
Admission and authorized actor generations are distinct required fields. A
versionchange transaction checks schema layout only; it never scans or updates
retained payloads. Exact R08b v1 rows normalize read-only after the owner lock and
matching binding are established. Owner-scoped transitions persist format 2 while
retaining keys, payloads, bases, owner references and original reservations. Abort
or quota failure preserves the original rows and accounting. Invalid old envelopes
remain untouched and fail closed when their owner-held lane is read. A local
preparation transaction replaces the enqueue payload with the exact method/body/
base request, preserving supplied path and raw JSON text without interpreting it.
Only its terminal completion returns a prepared row. A validated receipt and the
bounded predecessor checkpoint commit atomically; a dependent request resolves
its base from that durable receipt, including unchanged writes. A separate trim
transaction removes the durable-ACK prefix and releases its row/byte reservation;
failed trim retains the ACK and never changes its historical meaning.

Native IndexedDB checks in `scripts/smoke-outbox-store.mjs` cover request success
followed by preparation/ACK/trim abort, quota denial, stale identities/phases,
strict corrupted envelopes, duplicate keys, dependency and numeric bases, raw
payload compatibility, maximum payload reservations and close/reopen retention.
These are storage-boundary tests with independently supplied receipt fixtures;
they do not prove HTTP delivery, same-key server replay, native process crash
restoration, selection actions or bounded retry budgets. R08c's remaining sender
work must test those boundaries before activation. The public browser handle
continues to expose local admission/recovery/disposal only, with no sender.
