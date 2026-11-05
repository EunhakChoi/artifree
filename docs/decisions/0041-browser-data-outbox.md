# Browser application-data outbox

Status: **Proposed. Nothing in this record is implemented by this change.**

Scope: R08, an opt-in browser TypeScript client for R06 application-data
writes. This specification does not enable it in the management UI or runtime
frames. Numbered rules are the proposed implementation contract; changes to
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
   `confirmRecovery(selection)` and `abandonUnresolved(selection)`.
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
   initialization explicitly. Once a free candidate lock is acquired, read
   bounded recovery metadata for the same authenticated binding and return
   `recovery_required`; send nothing, prepare nothing and perform no trim,
   enqueue into that candidate or deletion until an explicit choice. If it
   has no retained work, create a fresh owner for new work instead of claiming
   recovered physical identity. Automatic reload/native-restore delivery is
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
   An explicit `resume` for retry/access cannot bypass this recovery gate.
   A saved candidate is not automatically probed, adopted, drained or deleted.
   Only `retryRecoveryCandidate()` deliberately attempts its lock again; if
   still held it reports `owner_busy` without waiting or changing rows. If
   acquired, revalidate the authenticated binding and retained record versions,
   then return a fresh `recovery_required` selection without sending, preparing,
   trimming or deleting. `confirmRecovery` remains a separate explicit choice.
   The recovered queue keeps its original logical owner; it is not moved into
   the fresh owner's queue. Retry confers no proof of physical tab identity.
   Clear the saved reference only by an explicit caller action after holding
   its lock, validating binding and verifying that its queue has no retained
   rows; it cannot discard payloads or evict an unresolved candidate.
   If a browser loses session storage, retained rows stay dormant; there is no
   orphan discovery/adoption. Explicit disposal is restricted to a current-page
   queue or an explicitly confirmed recovery queue, with its owner lock held
   and binding revalidated. Closing releases the lock only after fencing callbacks
   and settling local transactions; an unresolved HTTP write can still commit
   and is recovered by its saved key. Lock release is not cancellation proof.
7. Enqueue is asynchronous and resolves only after the IndexedDB transaction's
   `complete` event. It validates the operation/path/value using the existing
   client input rules, snapshots caller data so later mutation cannot change
   it, allocates a sequence and unique request key, stores the full immutable
   intent and updates counters in one transaction. Failure/abort rejects with
   `not_enqueued` and sends nothing. A callback before transaction completion,
   a local overlay or an `IDBRequest` success is not durable acceptance.
   Enqueue acknowledgement and server acknowledgement are distinct results.
8. Persist operation, canonical path, immutable value/patch text (none for
   delete), sequence, key, caller-observed base, admitted actor generation,
   authorized-generation checkpoint, state and record version.
   Records are strictly decoded at recovery: wrong version, invalid enums,
   invalid sizes, duplicate sequence/key, inconsistent counters or malformed
   request data pause that lane with `storage_corrupt`; do not skip or delete
   the bad head and send its successors. Read at most one bounded page at a
   time. A database schema newer than the client fails closed without writes.
   No released R08 store exists; its first schema must require generation
   metadata. If a recognized experimental older format lacks it, preserve the
   rows as `generation_unbound` and require a fresh binding plus explicit
   recovery before setting delivery authority. Never infer generation 1 or the
   current generation, discard rows, or regenerate operation keys during an
   upgrade. Unrecognized formats remain a closed storage error.

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

### ACK, canonical state and trim

12. Accept an ACK only for a validated successful `DataWriteResponse` matching
    the prepared artifact/path, with a safe version: `changed: true` means
    base plus one and `changed: false` means base. `replayed` may be either.
    A successful HTTP status with an invalid/mismatched body is unknown outcome,
    never an ACK. Commit the receipt (version, changed, replayed) and advance
    the lane's checkpoint/dependency atomically before releasing the successor.
    Expose `server_acknowledged` only after this commit. If receipt persistence
    fails, expose the observed receipt as `ack_storage_failed`, retain the
    prepared request, stop this lane and retry local persistence explicitly;
    after a crash the original request can replay only after rule 6's explicit
    recovery choice. Do not send the successor.
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
    ACK transaction failed instead, replay is required; distinguish these two
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
    deletion. Storage failures while preparing/ACKing pause sends as above.
    Empty lane metadata is deleted after trim. Browser eviction/clear/private
    mode may lose data: report missing storage honestly, without reconstructing
    delivery from a remembered in-memory promise.
16. Network errors, timeouts, 429 and 5xx retain the exact prepared request.
    Use 0040 rule 17's bounded retry policy with the client's injected clock
    and random source: 0.5 s doubling to 30 s, jitter multiplier in [0.5, 1),
    with parsed Retry-After as a floor clamped to one hour. A **delivery-budget
    epoch** permits at most eight consecutive unsuccessful HTTP mutation
    attempts per lane and 32 unsuccessful attempts total across the coordinator.
    Each unsuccessful attempt counts once, including a timeout or abort whose
    request may have reached the server. A durable ACK resets that lane's
    consecutive-failure count and backoff only; it never resets the aggregate
    32-failure count within the epoch. Successes consume no failure budget, so
    more than eight healthy writes can drain. Eight consecutive failures pause
    that lane; aggregate exhaustion pauses all sending. Report `retry_paused`
    with the applicable scope and exhausted budget.
    Opening an explicitly opted-in coordinator for a fresh owner starts its
    initial epoch. A deliberate caller/user `resume`, or required explicit
    recovery/reauthorization confirmation, starts a new bounded epoch with both
    failure counters reset and fresh backoff; this is an intentional new budget,
    not a lifetime 32-attempt guarantee. Serialize the action with the active
    attempt: settle or abort it under the existing deadline before starting the
    new epoch. Duplicate/pending resume actions coalesce without replenishing
    an active epoch repeatedly. Authorization, conflict and inherited-owner
    recovery gates remain in force; resetting budget grants no authority.
    Reconnect, online events, new enqueues, ACK progress, timers and status/error
    callbacks never open a new epoch, reset the aggregate counter or invoke
    resume automatically. A new page with retained work still needs explicit
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
    retained intent and canonical data, then discard the never-acknowledged
    head plus its dependency suffix, or enqueue an independently chosen new
    intent with a new key/base. No automatic merge or mutation of prepared
    payloads. Independent lanes continue. For an unknown outcome, replay is the
    way to resolve the receipt, but is not a prerequisite for explicit local
    abandonment: purge makes replay permanently unavailable under 0031.
    `abandonUnresolved(selection)` requires a deliberate caller/user choice
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
    the lane paused. Survivors stay paused until explicit resume. Late replies
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
    commit. A fresh binding/access check and explicit resume are needed after
    credentials/access return. Even if disable/re-enable was never observed as
    a `401`, the stored generation fence rejects the old authority on the next
    send. A fresh binding may reveal a different generation for the same
    actor/URL/storage epoch; pause all affected lanes, invalidate prior
    selections and bump the local authority/callback generation before any
    reauthorization. Return `recovery_required` with reason
    `actor_generation_changed`; generic retry/resume cannot adopt it. A new
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
    eligible heads. Invalid storage halts its lane. Stop after at most 256
    local state transitions per cycle; yield between pages and surface
    `recovery_paused` if work remains. A caller resume continues from durable
    state rather than restarting a scan/attempt budget in a tight loop.
    Recovery never waits for canonical follower notifications to identify ACKs.
    Claims of native browser-process recovery require an actual owned-profile
    termination and native session restoration, followed by explicit recovery
    where identity is ambiguous; reload or injected session IDs must be reported
    as their narrower tests. Neither test success nor confirmation proves a
    physical-tab identity guarantee unavailable from the browser primitives.

## Acceptance and rejection tests (all future work)

| ID  | Required behavior and failure proof                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| B1  | Binding read returns only the caller’s actor, actor_generation string and existing epoch; disabled/revoked credentials fail. Reject partial header triples, malformed or overflow generation strings and inconsistent binding responses. With well-formed headers, foreign/missing/trash/purged target remains identical404 before semantic fence checks, including a replayable key or wrong expected actor; an accessible target with stale actor/epoch/generation gives409 before key replay/base checks and consumes no key. Malformed fence input still follows ordinary input-error precedence. Swap actor or supported-restore epoch between handshake and mutation: no write/key/replay. Generation cases are B12. Remove transaction fence and show rejection tests fail. Unfenced direct writes remain compatible.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| B2  | Real IndexedDB enqueue transaction complete survives reload. Hold request-success before transaction abort: enqueue rejects and zero HTTP writes. Quota/transaction exceptions preserve older rows and counters. Mutate caller objects after enqueue: transmitted request stays fixed.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| B3  | Real tabs with copied session storage: original holds lock, clone wins before original starts, clone wins after crash, and reload overlaps held old page. Busy candidate preserves rows and one bounded separate candidate reference before allocating a fresh owner. Hold old page alive, then release its lock: no automatic probe or send; explicit retry validates lock/binding/versions and returns a new recovery_required selection, and only separate confirmation permits replay. Full candidate slot or persistence failure preserves both identifiers and queues without accepting new work; stale candidate/wrong binding cannot recover; free inherited candidate always returns recovery_required with zero HTTP writes, preparation, trim or deletion. Explicit current selection plus matching binding allows exact replay; decline, stale selection, retry resume, boot or online event cannot bypass the gate. Reopening reloads the choice; no persistent auto-resume flag. No test attributes physical origin to confirmation. Missing storage/locks refuses initialization; same-document handles cannot send twice.                                                                                                                                                                                                                                                                                                      |
| B4  | Two writes at different paths serialize across one data version; dependency uses predecessor receipt including no-op/replay. Another tab or direct write in the same actor space produces a blocked conflict without automatic rebase. Another actor is rejected by binding/access checks without changing either actor’s data or the retained queue; shared-writer permission is not implemented before S02. Head conflict leaves dependent suffix untouched and other lane progresses.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| B5  | Kill/reload at enqueue completion, preparation completion, server commit before response, validated response before receipt commit, receipt commit before trim, and trim abort. Recovery first pauses with preserved records and no automatic HTTP; after explicit recovery assert original key/body/base replay, exactly one server mutation/change record, and no duplicate dependent write. Remove each relevant guard and observe its test fail.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| B6  | ACK persistence failure is unknown durable outcome; ACK success plus trim failure is retained acknowledgement. The latter is not retransmitted after reload and can advance dependencies. Malformed/oversize/stalled/mismatched success responses never advance.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| B7  | Canonical follower can observe a write before ACK, and a replay can ACK below current canonical version; neither falsely ACKs another key nor rolls back canonical state. A no-op with no event still ACKs. Reset pauses/fences old reads and requires matching binding to resume.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| B8  | Atomic admission at each row/byte boundary under competing owners; counted blocked/untrimmed rows and reserved metadata. Maximum-size intent prepares and ACKs without double-payload accounting failure. Malformed/new-schema records halt safely without deletion or successor send. More than eight healthy writes drain. Eight consecutive lane failures pause that lane; 32 total failures pause the coordinator even with interleaved ACKs. Only deliberate resume/recovery confirmation starts a new bounded epoch; duplicate concurrent actions coalesce and settle the old attempt before reset. Reconnect/online/enqueue/ACK/callback/timer cannot reset budget or send after exhaustion; reload of retained work remains recovery-gated. New epoch does not bypass authorization or conflicts. Bounded reads, transitions, retry count, timer cancellation, header silence, upload/body deadlines and late completion under deterministic clock/network faults.                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| B9  | Lost response followed by 401/403/404 retains unknown request. Same binding and authorized generation plus explicit resume permits exact replay; changed generation requires B12 reauthorization and cannot be silently copied. Different actor/URL/epoch stays paused. Logout and late callbacks cannot update/send the new handle or erase intents. Unresolved retention ends only through a validated replay receipt and normal trim, or an explicit valid B11 abandonment choice; never an access error alone.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| B10 | Terminate a real owned browser profile after durable enqueue while writes cannot reach the server, then native session restore without injecting owner data. Inherited owner remains recovery_required with zero sends until explicit choice; after confirmation under its lock and matching binding, drain and verify independent canonical commit. Confirmation authorizes the logical queue and does not prove original physical-tab identity. Missing owner leaves dormant rows; report OS/browser limitations and do not call reload a native crash success.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| B11 | Server commit with lost ACK, then purge: ordinary data routes still give the same 404, no receipt is inferred, and explicit abandonUnresolved removes head/dependent suffix and recovers quota without replay or a purge probe. Also allow deliberate abandonment while delivery remains uncertain without claiming purge. Assert no HTTP mutation or delivery authority from abandonment, no fabricated ACK/key changes/undo claim, no automatic removal after access loss, and no survivor send until explicit resume. Held response released after abandonment cannot resurrect rows or release successors. Selection/lock/binding mismatch or injected IDB abort preserves rows and counters; successful deletion is bounded to the 64-row/2 MiB lane and releases reserved accounting. A recovered candidate can explicitly abandon without first granting delivery. Supported restore with epoch mismatch allows warned same-actor/URL local cleanup without rebinding; offline cleanup requires the same page’s still-open locally authenticated handle, a held owner lock and unchanged current profile/credential generation; no 401 or unresolved auth invalidation may have occurred, and uncertain local authority fails closed, while detached/new unauthenticated or different-actor handles cannot inspect or remove old payloads. Known durable ACKs keep their receipt meaning and use trim, not unknown-outcome abandonment. |
| B12 | Disable→enable between binding and first send, between chained writes, or while a request authenticated in the old generation waits before its transaction: current actor/epoch are equal but the old generation cannot commit or replay/consume a key. No observed 401 is required. Unknown lost-ACK work stays pending through the transition; only fresh binding plus explicit generation-change confirmation permits the original method/body/base/key to replay with a new authority header and exactly one historical mutation. Old callbacks cannot ACK or release successors after change detection/reauthorization. A write committed before disable is not undone; its receipt is historical authority only. Repeated same-state administration does not invent a generation change, same-generation token rotation does not bypass other gates, generation above JS safe integer round-trips as a decimal string, and missing-generation recognized legacy rows stay generation_unbound until explicit recovery. Break the transactional check or automatic-adoption guard and prove these tests fail.                                                                                                                                                                                                                                                                                                                              |
| B13 | Reject credential-bearing base URL (username-only, password, percent-encoded userinfo), query/fragment, unsupported scheme or relative form before any owner/IDB write or network call. Diagnostics and callbacks never echo the raw URL or parser cause. Empty/root-relative and absolute credential-free HTTP(S) normalize to one base used by both storage and transport; test host/default-port/trailing-slash/base-path equivalence. Unsafe stored binding is quarantined without delivery or automatic credential stripping/rebinding. Existing direct client behavior is unchanged.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| B14 | Binding read snapshot generation unequal to AuthContext, or disable/enable before final current-user confirmation, returns the same401 as invalid authentication with no actor/epoch/generation fields; no409 on binding reads. A valid snapshot plus confirmation returns200. Send raw URI binding?, binding?x=1 and binding?& without client URL normalization: authenticated GET/HEAD reject400, while unauthenticated requests retain401 precedence. HEAD applies identical checks, no-store and statuses for success, stale authentication and invalid query/body, always with an empty body; client must obtain binding through GET, not HEAD. Verify route inventory/method coverage and retain existing CORS/other-method behavior.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |

Use independently authored fixtures and the repository's isolated verification
workflow. Unit fault seams prove state transitions; real-browser IndexedDB,
Web Locks and real-server tests prove boundaries. Rejection tests must fail
when their guarded behavior is broken. Run `npm run check`, `npm run build`
and relevant browser smokes for each code slice, with existing gates intact.
This documentation change has not executed these future acceptance cases.

## Proposed PR slices

| Slice | Deliverable                                                                                                                                                                                                                        | Dependencies and acceptance                                                          |
| ----- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| R08a  | Authenticated binding read and optional transactional actor/epoch/state-generation fence, generated contracts and route/authorization coverage. No automatic queue.                                                                | R07 server basis, 0031/0036/0027; B1/B12/B14 server cases, direct-write regressions. |
| R08b  | Dedicated browser store, page owner coordinator, ambiguity gate, bounded retained candidate and explicit retry/recovery, durable admission, caps, strict recovery decoder and explicit disposal of unsent work. No sender enabled. | R08a contract; B2/B3/B8/B13 and B12 legacy-generation storage cases.                 |
| R08c  | Opt-in serialized sender, immutable preparation, ACK/checkpoint/trim, explicit conflicts, abandonment and access-generation pauses, bounded retries and typed lifecycle API.                                                       | R08a–b, R07c follower semantics; B4–B12 including fail-first rejection proofs.       |
| R08d  | Real-browser explicit crash recovery and interleaving/fault verification, public API guide and precise supported-browser/recovery limits.                                                                                          | R08c; B2–B14, full gates and both browser engines where supported.                   |

No slice may advertise R08 complete before the end-to-end recovery acceptance
has passed. Runtime-frame data access, UI adoption, orphan transfer, automatic
merge, S02 roles and Docs staging remain separate proposed work. Implementation
reports must identify which slices actually shipped and which tests actually ran.
