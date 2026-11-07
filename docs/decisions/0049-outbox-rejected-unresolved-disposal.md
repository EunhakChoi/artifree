# 0049: Scheduler-owned rejected disposal and unresolved abandonment

Date: 2026-11-07. Status: **Accepted contract; implementation pending.**
Acceptance takes effect when this separate documentation-only change is
reviewed and merged. It accepts the additive contract below for the later
implementation PRs R08c-3a and R08c-3b; it introduces no executable behavior
and does not claim either implementation exists. Builds on
[0041](0041-browser-data-outbox.md) (rules 11, 12, 16–18 and B11/B22),
[0044](0044-browser-outbox-local.md) and
[0047](0047-outbox-disposal-retirement-proposal.md). The numbered rules of 0041
and 0047 remain unchanged. Roadmap rows: R08c-3s (this record), R08c-3a,
R08c-3b.

An implementation PR cannot supply its own specification approval or treat
passing tests as approval. A code PR may not change a numbered rule below; an
amendment is a separate documentation PR. R08 remains partially implemented,
the public browser handle remains local-only and public delivery remains
disabled. Nothing here adds a public registry, a public sender, a server
endpoint, a persistent format or a dependency.

Independent Artifree design: no private evidence was consulted for this
record, and nothing below is presented as an observation of the original
product. The private requirement-to-original-to-test trace required by the
roadmap still applies to the implementation PRs.

## Context

Labels follow [0040](0040-data-changes-and-member-listing.md) and
[0048](0048-drain-over-limit-bodies.md): observed (seen running), source-only
(read in this repository's source), inferred, and decision.

- **Observed:** none by this change. It ran only `npm run check` and
  `npm run build`. The native-browser results recorded in the roadmap for
  R08c-2c and for the internal resume after a later rejection (#128) are cited
  as recorded results of those PRs, not re-run here.
- **Source-only** (`packages/client/src/outbox`, `main` at 5986180):
  - `DeliveryScheduler` holds at most 256 lane metadata entries (`MAX_LANES`).
    At that limit, `wake` keeps one reconciliation marker instead of a new lane.
  - A definitive `data_conflict` or `rejected` failure (codes
    `data_path_conflict`, `invalid_request`, `data_too_large`,
    `request_key_reused`, `payload_too_large`) with `priorDelivery: 'none'`
    sets the pause `{ kind: 'blocked', failure }`. That pause has no action
    token and no captured snapshot. The same failure with
    `priorDelivery: 'unknown'` sets `retry_paused` with reason
    `unresolved_after_rejection` and a `DeliveryResume` token.
    `access_unavailable` 403/404 is also `blocked`. A 401 or `binding_changed`
    fences the whole scheduler.
  - The kernel computes `priorDelivery` per attempt. It is `'none'` only when
    the durable head was unprepared at entry, or when it exactly equals this
    instance's one-use `preparedWithoutDispatch` proof. Nothing is persisted
    about dispatch.
  - The R08c-2c cleanup actions (`selectUnsent`, `discardUnsent`,
    `selectEmptyPreparationGate`, `retireEmptyPreparationGate`) require a
    `preparation_paused` lane with zero lane failures, no timer and no active
    attempt. They capture a `SchedulerCleanupAuthority` and run in the
    `LocalOperationGate` cleanup boundary.
  - `removeUnsent` deletes the head plus every row transitively naming a
    removed row in `base.after`, rejects a non-`unprepared` member, then
    updates or deletes the lane checkpoint and decrements `usage` in the same
    transaction. The low-level `LocalStore.discard` accepts only an
    `unprepared` head. No supported path deletes a `prepared` row.
  - An epoch renewal (`resumeDelivery`) resets every lane counter and the
    aggregate counter, leaves `blocked` lanes paused and refreshes lane-scope
    `retry_paused` tokens.
- **Inferred:** a `blocked` lane has no action at all, so it occupies a
  metadata slot until the scheduler closes. An `unresolved_after_rejection` or
  exhausted lane can leave only by a delivery resume that may be refused
  again. 256 such lanes reproduce the saturation that 0047 fixed for
  preparation gates: a new durably admitted lane waits behind the marker with
  no explicit cleanup. 0047 rule 5 deliberately excludes these lanes from
  empty-gate retirement, and its preamble excludes rejected disposal and
  unknown-outcome abandonment.
- **Decision:** the rules below. They are Artifree contract choices.

## Terms

- **Lane incarnation:** one `Lane` object in the scheduler's map. A later
  `wake` for the same key after retirement creates a new incarnation with zero
  failures, no pause, no proof and no tokens.
- **Settled lane:** a lane that is not the active lane, has no retry timer, has
  no pending action of any kind and has a pause.
- **Closure:** the stored head plus every row that transitively names a row of
  the closure in `base.after`. Rows with a numeric base are not in the closure;
  rows outside the closure are **survivors**.
- **Rejection proof:** the in-memory record defined in rule 2.

## Accepted additive contract

These rules supplement 0041 rule 17 and 0047 rules 1–7. 0041 rules 5–6, 11–12
and 15–18, and 0047's authority, fencing, pending-identity and terminal-event
rules, keep their force wherever this record does not narrow them. This is not
generation reauthorization, access recovery, inherited-owner recovery, orphan
adoption or cancellation of an in-flight request.

1. **Scope and authority.** Both actions belong to the internal
   `DeliveryScheduler` of the fixed, already authorized fresh owner and
   binding that R08c-2b/2c support. A lane qualifies only when the scheduler's
   own key check accepts it. The actions use the R08c-2c cleanup host
   unchanged: the explicit `online` or `offline` mode, one
   `SchedulerCleanupAuthority` captured at selection and one at action entry
   (same `identity` and `signal`), and the shared `LocalOperationGate`. Online
   mode makes exactly one bound binding read at selection and one at action
   entry. It makes no mutation, artifact read or purge probe. Offline mode
   makes no network request. A failed online read yields no selection or
   deletion and never falls back to offline. A 401 or `outbox_binding_changed`
   closes the scheduler as R08c-2c does. Neither selection grants delivery,
   renews an epoch or authorizes another action kind.
2. **Rejection proof.** The pump creates a rejection proof only while it
   processes one attempt result. That result must be `delivery_failed` with
   reason `data_conflict` or `rejected`, `priorDelivery: 'none'`, and still
   current lane and authority. The proof holds the lane incarnation, the
   `blocked` pause object it creates, the bounded failure and a frozen copy of
   the exact prepared row that this attempt dispatched. The kernel supplies
   that row through an internal channel. It is never included in the published
   `attempt` observation. The proof is memory-only and never persisted,
   reconstructed from stored state, inferred from failure counts, status
   history or a canonical read, accepted from a caller, or transferred to
   another scheduler instance. It ends when its lane incarnation is retired
   or the scheduler closes or is fenced. It survives an epoch renewal and a
   failed disposal action, because neither re-dispatches the head. A lane holds
   at most one proof.
3. **`discardRejected` eligibility.** `selectRejected(key, mode)` and
   `discardRejected(selection)` accept a lane only if all of these hold. The
   lane is settled. Its pause is the `blocked` pause object recorded in its
   current proof. The bounded cleanup capture is of kind `intents` (no
   `receipts` anchor). The stored head is `prepared` and equal, in canonical
   JSON, to the proof's dispatched row. Every other closure member is
   `unprepared`, and the lane contains no `acknowledged` row. Every other state
   is ineligible and returns `invalid_selection` without changes:
   `preparation_paused`, `cleanup_paused`, any `retry_paused` (including
   `unresolved_after_rejection`, `ack_storage_failed`, network, invalid
   response, rate limit, server error, `unclassified_error_response` and
   `aggregate_exhausted`), `blocked` `access_unavailable`, a healthy, queued,
   timer-pending or active lane, and a lane without a current proof. That last
   case covers any lane after reload, crash or scheduler recreation. If a
   stored head differs from its proof, the action fails closed and keeps the
   gate and the proof. **An unknown head is never downgraded to rejected.**
4. **`abandonUnresolved` eligibility.** `selectUnresolved(key, mode)` and
   `abandonUnresolved(selection)` accept a lane only if all of these hold. The
   lane is settled and has no rejection proof. Its pause is either
   `retry_paused` (any reason except rule 11's `survivors_after_disposal`,
   lane or coordinator scope) or `blocked` with
   `access_unavailable` (403 or 404). The capture is of kind `intents`. The
   stored head is `prepared`, which means no durable ACK. Every other closure
   member is `unprepared`, and the lane contains no `acknowledged` row. An
   `unprepared` head (use 0047's path where it applies), an acknowledged head
   (trim), a `cleanup_paused` lane, and a lane holding a rejection proof
   (rule 3 applies) are ineligible. A head that this instance prepared but has
   not dispatched remains eligible and is reported as unknown, because the
   stored row cannot prove non-dispatch. `ack_storage_failed` is eligible: its
   observed receipt stays informational and is never turned into an ACK.
5. **Selection.** A selection is a distinct frozen opaque object
   (`RejectedDiscardSelection` or `UnresolvedAbandonSelection`) branded for its
   kind. Its context binds the lane incarnation, the pause object, the proof
   (for rejected selections), the complete capture signature (rows and
   checkpoint), the epoch, the lane failure count, the wake marker, the mode
   and the captured authority. Selection is caller-initiated only. No wake,
   scan, reconciliation, timer, enqueue, observation or other action creates
   or publishes one. A lane holds at most one current disposal selection of
   any kind, including 0047's. A new selection, a wake, a pause replacement
   (including token refresh), an epoch change, lane retirement and close all
   retire the previous one.
6. **Action entry.** 0047 rule 7 applies to both kinds without change. Runtime
   brand validation comes before the pending lookup. The same token returns
   the same Promise while pending. The token is consumed at entry, and a
   consumed token rejects after settlement. The action runs in the cleanup
   boundary after any active attempt settles and rechecks every bound context
   field. The lane's existing pause, proof and `DeliveryResume` token are not
   retired at entry. Another action on them during this one rejects with
   `invalid_selection` without consuming them.
7. **One transaction.** One readwrite IndexedDB transaction performs the
   whole disposal. It rechecks the authority callback on every request, the
   capture signature and the phase rules of rule 3 or 4. It deletes exactly
   the closure rows. It deletes the lane checkpoint when no survivor remains,
   or otherwise rewrites the checkpoint's row and byte counts for the
   survivors. It decrements origin `usage` rows and bytes by exactly the sum
   of the deleted rows' recorded `bytes`, which already include the 4 KiB
   reserve and the prepared-body wrapper. A changed signature (a new dependent
   or survivor, or a preparation, ACK or trim transition by any path), a
   changed phase, lost authority, a mismatched owner, actor, binding or key,
   or storage inconsistency aborts it. Only the transaction's terminal
   `complete` event commits; a request success does not. An abort, including
   one after the deletes succeed and before `usage` is written, leaves every
   row, checkpoint and counter intact. It also leaves the lane's pause, proof
   and resume token unchanged, makes no HTTP request and settles with
   `storage_failed`, or with `invalid_selection` for a stale check. A fresh
   selection is then required. 0047 rule 2's close and authentication
   invalidation policy applies: a synchronous fence plus an abort attempt,
   where the terminal event decides. A completed transaction settles with its
   committed result even if close or invalidation follows.
8. **Results and counts.** A committed `discardRejected` settles with
   `{ kind: 'discarded_rejected', removedDependents: n }`. A committed
   `abandonUnresolved` settles with
   `{ kind: 'abandoned_unknown', removedDependents: n }`. Here `n` is the
   number of rows the committed transaction deleted, minus the head. It is
   computed once, inside that transaction. Neither result is an ACK, a
   rejection receipt, a rollback, non-delivery or a statement about the key.
9. **Budgets.** Disposal recovers only local storage accounting: origin and
   lane rows and bytes, and the reserve, released once by the committed
   transaction. It does not change the aggregate failure count, the epoch,
   backoff, other lanes' counters or tokens, or a preparation, cleanup or
   delivery choice of another lane. It releases no server key and makes no key
   reusable. A replacement intent gets a fresh key and a caller-chosen base. A
   survivor lane keeps its consecutive-failure count. A fully removed lane's
   count leaves with its metadata, and a later incarnation starts at zero like
   any newly created lane. This is not an epoch renewal: the aggregate count,
   which every disposed failure already consumed, still caps the epoch at 32.
10. **Post-commit retirement, full removal.** After the terminal `complete`,
    if the scheduler is open, authority is current and the lane incarnation is
    still current, retire synchronously and without awaiting. Remove the lane
    from the map, retire its pause, its `DeliveryResume` token if any, its
    proof and every cleanup selection, and keep no tombstone. Request one
    bounded reconciliation if capacity is waiting or if the lane's wake marker
    changed during the action. Unlike 0047's unsent path, a wake that arrives
    during the action does not suppress this retirement: retirement depends on
    the committed rows and the lane identity, and the wake is honored by the
    reconciliation. If authority is lost or the scheduler closed, return the
    committed result with no retirement, publication, reconciliation or
    dispatch. That metadata dies with the fenced scheduler. This extends
    metadata retirement to the `blocked` and `retry_paused` lanes that 0047
    rule 5 excludes, and only through a committed disposal of that lane's own
    rows. 0047's empty-gate eligibility is unchanged.
11. **Post-commit, survivors.** If survivors remain, retire the old pause, its
    token, the proof and every cleanup selection, then publish one new
    lane-scope `retry_paused` pause with the new internal reason
    `survivors_after_disposal` and a fresh `DeliveryResume` token. The token's
    snapshot is the survivor row set that the committed transaction wrote and
    returned, in the form that existing resume validation compares. No second
    read is made, so publication cannot fail after commit. Emit one `paused`
    observation if publication is still allowed. The survivors keep their
    keys, bases and accounting. Wakes, enqueues, timers, reconciliation,
    another lane's ACK and another lane's epoch renewal never unpause them. An
    epoch renewal only refreshes their token, as for other lane-scope pauses.
    Only a valid explicit `resumeDelivery` with that token unpauses them,
    under the existing fresh binding read, snapshot check and epoch renewal.
    The survivor head then prepares and sends normally. This pause is
    ineligible for either action in this record and for 0047's actions.
12. **Fencing and late events.** A retired incarnation cannot be queued,
    paused, ACKed or dispatched. Every scheduler path compares lane identity
    before acting, as the existing `queue` and retire paths do. Late kernel,
    transaction, timer or observer callbacks from before the action find
    either a fenced generation or a non-current lane and change nothing. They
    cannot recreate deleted rows, write an ACK, release a successor or affect a
    later incarnation with the same key. Old selections and old resume tokens
    of the retired incarnation reject with `invalid_selection`. A request whose
    attempt ended by deadline or abort stays aborted. The server may still
    commit it, and that commit is not observed or recorded locally.
13. **Offline authority for `abandonUnresolved`** (and the same for
    `discardRejected`, as 0041 rule 17 requires for both). Before offline
    mode, the host must have established all of these. This page obtained the
    scheduler's actor and URL from a successful server binding response. It
    still holds that owner's exclusive Web Lock. The handle's captured
    page, profile and credential generation equals the current one. The handle
    is open and locally authenticated, with no logout, profile or credential
    replacement, 401 or unresolved authentication invalidation since that
    confirmation. The authority's `check()` verifies this at selection, at
    action entry, immediately before the transaction and on each request
    inside it. Its `signal` aborts on any loss. A caller label, saved token
    fingerprint, old handle, inherited session ID or a `404` is not this
    proof. Inability to establish it keeps the rows and fails closed. The
    contract does **not** prove current server access, that the request did
    not commit, that it will not still commit, rollback, purge, key release
    or that the artifact exists. An unknown outcome is never evidence of
    non-delivery.
14. **Bounded metadata.** Proofs and selections live on their lane (at most
    one each). A survivor pause reuses its lane's slot. No collection outside
    the existing 256 lane entries, the reconciliation marker and the
    WeakMap-held tokens is added. Disposal never closes the whole scheduler to
    reclaim metadata, and never erases another lane.

## Alternatives

- **Persist a "rejected" marker so `discardRejected` survives reload.**
  Rejected. Stored state would claim knowledge that a reload cannot verify,
  which risks labeling an unknown commit as rejected. It would also require a
  new persistent format. After reload, the head is unknown (0041 rules 11
  and 17).
- **Infer rejection from failure counters, an epoch reset or a canonical
  read.** Rejected by 0041 rules 11 and 17 and by the #128 behavior.
- **Allow `abandonUnresolved` for a head with a valid rejection proof.**
  Rejected for the internal slices, because each head then has exactly one
  honest result label and one action kind. Abandoning a known rejection would
  be safe but would discard evidence. The public form is an open question.
- **Keep a lane-counter tombstone after full removal, or refill the aggregate
  budget on disposal.** Rejected. A tombstone is unbounded metadata outside
  the 256 slots. A refill is a budget reset that 0041 rule 17 forbids. The
  retained aggregate count bounds the epoch.
- **Leave survivors in the old `blocked` pause.** Rejected: that pause has no
  action, so the slot would stay occupied. **Auto-resume survivors:**
  rejected by 0041 rule 17.
- **Capture the survivor snapshot with a second read after commit.** Rejected:
  that adds a post-commit failure point that would force a close. The
  transaction's own output is exact.
- **Suppress retirement when a wake arrives during the action (0047's unsent
  behavior).** Rejected for these lanes. A stale `blocked` gate over deleted
  rows could never be selected again and would reintroduce saturation.
  Admission is already serialized by the shared gate, and the reconciliation
  request covers a hint.
- **Publish a disposal choice automatically on `blocked`.** Rejected: a choice
  requires cleanup authority (a binding read online), and 0041 rule 1 requires
  explicit choices.
- **Abort an in-flight attempt or cancel a pending retry timer to abandon.**
  Deferred. These slices accept only settled lanes. A retrying lane reaches a
  pause within the bounded 8/32 policy. See the open questions.
- **Retire empty `blocked` or `retry_paused` gates whose rows disappeared
  outside the scheduler.** Not added. No supported path deletes a `prepared`
  row outside the scheduler. Such a gate stays retained and fails closed
  (an unknown state never means empty), and 0047 rule 5 keeps it ineligible.

## Open questions for the owner

These would change the public contract of 0041. They are not decided here and
are not part of the numbered rules.

1. **Abandoning while an attempt is in flight or a timer is pending.** 0041
   rule 17 and B11 describe fencing and aborting the request first. Options:
   (a) a later specification adds abort-then-abandon before public activation;
   (b) public activation offers abandonment only for settled pauses, and 0041
   B11 is amended to say so; (c) a public "stop delivery" action first settles
   the lane into a pause, then this record's action applies.
2. **Disposing of survivors without resuming delivery.** Options: (a)
   resume-only, the internal behavior here; (b) extend unsent discard to the
   `survivors_after_disposal` pause; (c) one lane-wide choice that removes the
   head and all survivors.
3. **Public label for a known rejection.** Options: (a) the public API mirrors
   rule 4's exclusivity; (b) the public API also lets a user abandon a known
   rejection as `abandoned_unknown`.

## Out of scope

Public registry, page-global coordinator, public sender and their methods;
inherited-owner recovery and `RecoveryContinuation`; disposal under a
mismatched storage epoch or a changed actor generation (0041 rules 17–18 still
require it for the public form); generation reauthorization and access
recovery; in-flight or timer-pending abandonment; survivor disposal; the
management UI and runtime frames; native-process crash restoration (R08d);
server changes. The public browser handle and direct writes are unchanged.

## Slices and acceptance

**R08c-3a (internal `discardRejected`)** implements rules 1–3 and 5–14 for the
rejected kind. **R08c-3b (internal `abandonUnresolved`)** follows and adds
rule 4 and the unresolved kind. Each is one code PR that changes the internal
scheduler, store and kernel channel and their native fixtures, and may not
change a numbered rule here. Each uses native IndexedDB, Web Locks and Fetch
in Chromium and Firefox, runs `npm run check`, `npm run build` and the
applicable smoke checks, and receives both required reviews. Fixtures,
reloads, injected aborts and native-process termination are different
evidence. Neither slice claims native-process restoration. Real-server
fixtures track every artifact and user they create, clean up artifacts before
disabling users, always close owned contexts, and retain cleanup failures
beside the original failure.

R08c-3a acceptance:

- **B22, rejected half.** With a real Rust server in both engines: real
  `data_conflict` (from a competing direct write), `data_path_conflict` and
  `invalid_request` heads with dependent and numeric-base successors yield
  `discarded_rejected` with the correct `removedDependents`. The closure is
  deleted, survivors and foreign-binding rows and anchors stay, and survivors
  stay paused until `resumeDelivery`. That resume then sends the survivor
  with its original key and base. `data_too_large`, `request_key_reused` and
  `payload_too_large` use owned HTTP fixtures, reported as fixtures.
- **No downgrade of an unknown head.** `selectRejected` and `discardRejected`
  reject for `unresolved_after_rejection` (real lost response then conflict),
  for the same head after scheduler recreation and after reload, for
  `access_unavailable`, network, invalid response, `unclassified_error_response`,
  `ack_storage_failed`, `aggregate_exhausted`, preparation-paused and
  cleanup-paused lanes, and for a timer-pending or active lane.
- **Atomic rejection matrix.** Each case must leave rows, checkpoints and
  `usage` byte-identical, keep the pause, proof and resume token, and send
  zero HTTP mutations:
  - wrong-kind tokens: every other selection or action token passed to
    `discardRejected`, and this token passed to every other action;
  - forged or unbranded objects;
  - a consumed token;
  - stale context: a wake, another lane's epoch renewal, a refreshed pause;
  - another scheduler's token, or a token for another owner, actor or binding;
  - a newly enqueued dependent and a newly enqueued survivor;
  - a preparation, ACK or trim written into the lane through the store between
    selection and action.
- **Abort before counters.** Inject an abort after the intent deletes succeed
  and before `usage` is written. Rows, checkpoints and reserves stay intact,
  the gate stays, no HTTP is sent, and the result is `storage_failed`. A fresh
  selection then commits once. A terminal abort and a too-late abort followed
  by `complete` are tested separately at close and at authentication
  invalidation.
- **Accounting once.** The origin and lane deltas equal the deleted rows'
  recorded bytes, including the reserve, exactly once. The aggregate count,
  epoch, other lanes' counters and tokens are unchanged before and after.
  Online mode makes exactly one binding read per selection and per action;
  offline mode makes no network request.
- **No resurrection.** After commit, old selections, old resume tokens and
  stale transaction or observer callbacks change nothing. A wake for the same
  key creates a new incarnation without failures, proof or pause. Post-commit
  close or authentication invalidation settles `discarded_rejected` with no
  publication, reconciliation or dispatch.
- **Saturation.** With owned HTTP fixtures and explicit resumes between
  epochs, fill 256 slots with definitively rejected `blocked` lanes, then
  start a fresh epoch with budget remaining. A newly admitted lane waits with
  zero HTTP. One `discardRejected` lets reconciliation dispatch and ACK it.
  The other gates stay intact, and nothing is retired automatically.
- **Pending identity.** The same token called twice while pending returns the
  same Promise and one operation. A cross-kind call rejects while it is
  pending. This holds at the transaction and post-commit boundaries.

R08c-3b acceptance, in addition, for `abandonUnresolved`:

- **B11.** With a real Rust server: the server commits, the response is lost,
  and the artifact is purged. The replay gets a 404 access pause. Abandoning
  online and offline removes the closure and recovers quota with no replay,
  purge probe or ACK, and ordinary data routes still return 404. Abandoning
  after network exhaustion without a purge yields `abandoned_unknown`, and a
  later canonical read can show the committed value; the result claimed
  nothing about it. `unresolved_after_rejection`, `unclassified_error_response`,
  `ack_storage_failed` (the receipt is never promoted),
  coordinator-scope `aggregate_exhausted` and recreated-scheduler heads are
  each covered.
- **Exclusivity and phase.** A lane with a rejection proof rejects
  `abandonUnresolved`. Unprepared, acknowledged, cleanup-paused, healthy,
  timer-pending and active lanes reject. The rejection matrix, abort before
  counters, accounting, no-resurrection, saturation and pending-identity cases
  above repeat for this kind.
- **Late response.** Hold a committed server response past the attempt
  deadline, exhaust the lane into a pause, abandon, then release the held
  response. No row is recreated, no ACK is written and no successor is
  released.
- **Offline authority.** Offline abandonment succeeds only with rule 13's
  conditions. It fails closed with rows kept after logout, a 401, profile or
  credential replacement, lock loss, a changed generation, a detached handle
  or an aborted authority signal, including when the change happens between
  selection and the transaction. An online read failure performs no deletion
  and no offline fallback.

Guard mutations for each slice. Break each guard separately, show the named
assertion fails for that reason, then restore it and verify the baseline:

| Broken guard                                                                 | Intended failing assertion                                             |
| ---------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| Proof created without `priorDelivery: 'none'`, or accepted without a proof   | "No downgrade": `unresolved_after_rejection` or recreated head rejects |
| Proof's dispatched row not compared with the stored head                     | A head changed through the store before selection rejects              |
| Closure takes only direct dependents, or includes numeric-base rows          | B22: dependent-of-dependent removed and survivor kept                  |
| Signature recheck removed from the transaction                               | A newly enqueued dependent rejects with bytes unchanged                |
| `usage` written in a second transaction                                      | Abort before counters leaves `usage` unchanged                         |
| Metadata retired before `complete`                                           | After an abort the gate and resume token remain                        |
| Lane identity not checked after commit, or a wake suppresses retirement      | No resurrection and saturation reconciliation                          |
| `access_unavailable` or a `retry_paused` lane eligible for `discardRejected` | Matrix: ineligible pauses reject                                       |
| Aggregate count or epoch reset by disposal                                   | Accounting once: before and after counters are equal                   |
| Reserve not released, or released twice                                      | Accounting once: exact byte delta                                      |
| Survivors unpaused without `resumeDelivery`                                  | B22 or B11: zero survivor HTTP before resume                           |
| (3b) Offline `check()` skipped before the transaction                        | Offline authority: change between selection and transaction rejects    |

## Implementation status

| Part                                 | Status                                     |
| ------------------------------------ | ------------------------------------------ |
| This contract (R08c-3s)              | Accepted when this documentation PR merges |
| R08c-3a internal `discardRejected`   | Proposed; not implemented                  |
| R08c-3b internal `abandonUnresolved` | Proposed; not implemented                  |
| Public handle, registry, sender      | Not implemented; outside this record       |
| Native-process recovery (R08d)       | Not implemented; outside this record       |

This documentation change ran no acceptance case and created no users or
artifacts.
