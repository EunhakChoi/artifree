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
   commands, including `recovery_required`, `retryRecoveryCandidate()` and
   `confirmRecovery(selection)`.
   Confirmation must represent an explicit recovery choice by the caller/user,
   never a default startup, reconnect or online-event action. A consumer that
   supplies no recovery interaction leaves recovered work paused; R08 requires
   no new management UI. Source drafts, room events, Docs operations and comments never
   enter this store.
2. Binding must be established by the server before an outbox first accepts
   writes, and checked again before recovery sends. Add authenticated
   `GET /v1/data-outbox/binding` returning exactly the authenticated
   `actor_id` and opaque `storage_epoch` in a `200` response. `actor_id` is
   `local` or `u-` followed by 32 lowercase hexadecimal digits, the registered
   actor spellings of 0036. `storage_epoch` is the existing `FeedId` display
   form: exactly 32 lowercase hexadecimal digits representing its 16 bytes.
   The read takes no query or body; reject unexpected input with
   `400 invalid_request`; require exactly those two response fields and bound
   its success body to 4 KiB. Use the existing feed identity
   rather than inventing a second database identity. It uses ordinary HTTP
   authentication/rate limits, `Cache-Control: no-store`, no administrator
   requirement, no other user's information, and the existing current-user
   confirmation before returning. It is not a health/inventory response.
   Token rotation for the same actor and epoch may resume only after all
   applicable recovery/access choices; another actor may
   not. A caller label, token fingerprint or API URL alone is insufficient.
3. Persist the normalized API base URL (including any base path), authenticated
   actor, storage epoch, artifact and owner with every lane. No bearer token,
   cookie, runtime grant or secret is persisted by the outbox. The coordinator
   pins one immutable credential generation for each send; profile/credential
   changes fence its callbacks and require a new binding check. URL, actor or
   epoch mismatch pauses the old lane; never silently rebind or replay into
   the new target, even if its artifact ID and data version happen to match.
4. A handshake alone leaves a race with restore or a different actor's token.
   Outbox writes therefore include paired optional headers
   `Artifree-Outbox-Actor` and `Artifree-Outbox-Epoch`. One without the other,
   repeated headers, or syntax outside rule 2's actor/epoch spellings is
   `400 invalid_request`. After normal
   authentication and input validation, the service verifies expected actor
   against authenticated actor and expected epoch against storage in the same
   transaction as key lookup/base comparison/write, **before replay too**.
   Mismatch is `409 outbox_binding_changed`, without a key or mutation and
   without disclosing the actual binding. Existing access checks remain
   mandatory. The optional precondition is an explicit core input, not HTTP
   logic in the store or domain; HTTP delegates through `ArtifactService`.
   Without these headers the existing rules are unchanged. Regenerate wire
   contracts; do not hand-edit generated files. Add the headers to the allowed
   CORS request headers and expose the read route to the existing route
   inventory/authorization tests. No new MCP outbox command is needed.
   Supported restore renews the epoch; copying an identical database identity
   outside supported restore is outside this fence's guarantee.

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
   delete), sequence, key, caller-observed base, state and record version.
   Records are strictly decoded at recovery: wrong version, invalid enums,
   invalid sizes, duplicate sequence/key, inconsistent counters or malformed
   request data pause that lane with `storage_corrupt`; do not skip or delete
   the bad head and send its successors. Read at most one bounded page at a
   time. A database schema newer than the client fails closed without writes.

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
    completes may the sender run. Once prepared, every attempt is identical;
    abort, timeout, response loss, invalid response, process crash and reload
    cannot regenerate its key or base. There is no persisted `sent means
committed` assumption. A prepared record without a durable receipt is an
    unknown outcome that must replay, not consult a current value to guess
    whether its write happened.

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
    equal actor/epoch and applicable recovery/access choices may resume, then
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
    with parsed Retry-After as a floor clamped to one hour. Count retries from
    the first failure and reset only on durable progress or explicit resume.
    Each recovery/start/resume cycle permits at most eight consecutive
    unsuccessful HTTP mutation attempts per lane, and at most 32 unsuccessful
    attempts overall since durable progress. A durable ACK resets both
    relevant counters, so healthy backlogs do not stop after eight successes.
    Admission of new work alone never resets them;
    exhaustion enters `retry_paused` until an explicit caller resume. No busy
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
    payloads. Independent lanes continue. Unknown outcomes must be resolved by
    replay before discard; disposal cannot claim to undo a possible commit.
18. 401, 403, 404 and binding mismatch pause, retain and report access/binding
    uncertainty; never interpret them as proof an earlier attempt did not
    commit. A fresh binding/access check and explicit resume are needed after
    credentials/access return. Logout/profile replacement fences old callbacks,
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

| ID  | Required behavior and failure proof                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| B1  | Binding read returns only the caller's actor and existing epoch; disabled/revoked credentials fail. Swap token actor or supported-restore epoch between handshake and mutation: no write/key/replay, binding error. Remove transaction fence and show the rejection test fails. Unfenced direct writes remain compatible.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| B2  | Real IndexedDB enqueue transaction complete survives reload. Hold request-success before transaction abort: enqueue rejects and zero HTTP writes. Quota/transaction exceptions preserve older rows and counters. Mutate caller objects after enqueue: transmitted request stays fixed.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| B3  | Real tabs with copied session storage: original holds lock, clone wins before original starts, clone wins after crash, and reload overlaps held old page. Busy candidate preserves rows and one bounded separate candidate reference before allocating a fresh owner. Hold old page alive, then release its lock: no automatic probe or send; explicit retry validates lock/binding/versions and returns a new recovery_required selection, and only separate confirmation permits replay. Full candidate slot or persistence failure preserves both identifiers and queues without accepting new work; stale candidate/wrong binding cannot recover; free inherited candidate always returns recovery_required with zero HTTP writes, preparation, trim or deletion. Explicit current selection plus matching binding allows exact replay; decline, stale selection, retry resume, boot or online event cannot bypass the gate. Reopening reloads the choice; no persistent auto-resume flag. No test attributes physical origin to confirmation. Missing storage/locks refuses initialization; same-document handles cannot send twice. |
| B4  | Two writes at different paths serialize across one data version; dependency uses predecessor receipt including no-op/replay. Another tab or direct write in the same actor space produces a blocked conflict without automatic rebase. Another actor is rejected by binding/access checks without changing either actor’s data or the retained queue; shared-writer permission is not implemented before S02. Head conflict leaves dependent suffix untouched and other lane progresses.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| B5  | Kill/reload at enqueue completion, preparation completion, server commit before response, validated response before receipt commit, receipt commit before trim, and trim abort. Recovery first pauses with preserved records and no automatic HTTP; after explicit recovery assert original key/body/base replay, exactly one server mutation/change record, and no duplicate dependent write. Remove each relevant guard and observe its test fail.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| B6  | ACK persistence failure is unknown durable outcome; ACK success plus trim failure is retained acknowledgement. The latter is not retransmitted after reload and can advance dependencies. Malformed/oversize/stalled/mismatched success responses never advance.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| B7  | Canonical follower can observe a write before ACK, and a replay can ACK below current canonical version; neither falsely ACKs another key nor rolls back canonical state. A no-op with no event still ACKs. Reset pauses/fences old reads and requires matching binding to resume.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| B8  | Atomic admission at each row/byte boundary under competing owners; counted blocked/untrimmed rows and reserved metadata. Maximum-size intent prepares and ACKs without double-payload accounting failure. Malformed/new-schema records halt safely without deletion or successor send. More than eight healthy writes drain; failed attempts stop at their budget. Bounded reads, transitions, retry count, timer cancellation, header silence, upload/body deadlines and late completion under deterministic clock/network faults.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| B9  | Lost response followed by 401/403/404 retains unknown request. Restore credentials/access for the same binding and explicitly resume: exact replay. Different actor/URL/epoch stays paused. Logout and late callback cannot update/send the new handle or erase old intents.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| B10 | Terminate a real owned browser profile after durable enqueue while writes cannot reach the server, then native session restore without injecting owner data. Inherited owner remains recovery_required with zero sends until explicit choice; after confirmation under its lock and matching binding, drain and verify independent canonical commit. Confirmation authorizes the logical queue and does not prove original physical-tab identity. Missing owner leaves dormant rows; report OS/browser limitations and do not call reload a native crash success.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |

Use independently authored fixtures and the repository's isolated verification
workflow. Unit fault seams prove state transitions; real-browser IndexedDB,
Web Locks and real-server tests prove boundaries. Rejection tests must fail
when their guarded behavior is broken. Run `npm run check`, `npm run build`
and relevant browser smokes for each code slice, with existing gates intact.
This documentation change has not executed these future acceptance cases.

## Proposed PR slices

| Slice | Deliverable                                                                                                                                                                                                                        | Dependencies and acceptance                                                   |
| ----- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| R08a  | Authenticated binding read and optional transactional actor/epoch fence, generated contracts and route/authorization coverage. No automatic queue.                                                                                 | R07 server basis, 0031/0036/0027; B1, direct-write regressions.               |
| R08b  | Dedicated browser store, page owner coordinator, ambiguity gate, bounded retained candidate and explicit retry/recovery, durable admission, caps, strict recovery decoder and explicit disposal of unsent work. No sender enabled. | R08a contract; B2/B3/B8 storage cases.                                        |
| R08c  | Opt-in serialized sender, immutable preparation, ACK/checkpoint/trim, explicit conflicts and access pauses, bounded retries and typed lifecycle API.                                                                               | R08a–b, R07c follower semantics; B4–B9 including fail-first rejection proofs. |
| R08d  | Real-browser explicit crash recovery and interleaving/fault verification, public API guide and precise supported-browser/recovery limits.                                                                                          | R08c; B2–B10, full gates and both browser engines where supported.            |

No slice may advertise R08 complete before the end-to-end recovery acceptance
has passed. Runtime-frame data access, UI adoption, orphan transfer, automatic
merge, S02 roles and Docs staging remain separate proposed work. Implementation
reports must identify which slices actually shipped and which tests actually ran.
