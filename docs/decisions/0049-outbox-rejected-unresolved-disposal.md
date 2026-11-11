# 0049: Scheduler-owned rejected disposal and unresolved abandonment

> Browser engines: this record was written while Firefox was suspended in CI,
> and its Firefox acceptances were first met in Chromium only. They were re-run
> on 2026-11-11 (O08): the whole smoke suite passes in Firefox, which is back in
> CI ([quality](../quality.md#browser-engines)).

Date: 2026-11-07. Status: **accepted by the R08c-3a code pull request;
implemented in the internal scheduler** (`discardRejected` by R08c-3a, the
internal `abandonUnresolved` by R08c-3b). Builds on
[0041](0041-browser-data-outbox.md) (rules 11, 12, 16–18 and B11/B22),
[0044](0044-browser-outbox-local.md) and
[0047](0047-outbox-disposal-retirement-proposal.md). Roadmap rows: R08c-3s
(this record), R08c-3a, R08c-3b.

> **Both kinds are implemented, internal scheduler only.** This record merged as
> a documentation-only specification with status proposed, following
> `.github/CONTRIBUTING.md`, and the R08c-3a code PR accepted it when it
> implemented `discardRejected`. The R08c-3b code PR implemented
> `abandonUnresolved` under the same record without changing a numbered rule.
> There is no public registry, sender or handle. (0047 was staged differently: proposed in #123 and accepted by a
> separate documentation PR, #126. This record did not follow that path.) A
> code PR may not change a numbered rule below; an amendment is a separate
> documentation PR. The numbered rules of 0041 and 0047 remain unchanged.

R08 remains partially implemented, the public browser handle remains local-only
and public delivery remains disabled. Nothing here adds a public registry, a
public sender, a server endpoint, a persistent format or a dependency.

Independent Artifree design: no private evidence was consulted for this
record, and nothing below is presented as an observation of the original
product. The private requirement-to-original-to-test trace required by the
roadmap still applies to the implementation PRs.

## Context

Labels follow [0004](0004-inbound-mcp-adapter.md), as
[0048](0048-drain-over-limit-bodies.md) does: observed (seen running),
source-only (read in this repository's source), inferred, and decision.

- **Observed:** none by this change. It ran only formatting, `npm run check`
  and `npm run build`. Earlier PRs record their own checks, cited here without
  re-running them: the roadmap's R08c-2c row records focused native checks in
  two browser engines, and 0041's section on the internal resume after a
  later rejection (merged as 5986180, PR #128, with
  `scripts/smoke-outbox-rejection.mjs`) describes its native IndexedDB and
  Rust fixtures.
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
    `access_unavailable` (401, 403 or 404) is also a token-less `blocked`
    pause; the code comment says access and generation recovery are not
    implemented by this fixed-authority host. A delivery `binding_changed`
    closes the whole scheduler. A delivery 401 makes the kernel abort its own
    fence and call the host's `invalidateAuthentication`. The scheduler closes
    only if the host then aborts `context.signal`; otherwise the 401 result
    reaches the failure path and sets a `blocked` pause. A 401 or
    `outbox_binding_changed` from a cleanup-authority read closes the
    scheduler.
  - **This differs from 0041 rule 18 on purpose.** Rule 18 requires
    `retry_paused` with reason `access_unavailable` and a
    `DeliveryResumeSelection` for 401/403/404. The merged internal host
    implements only part of R08 and leaves access recovery to a later slice.
    This record describes the merged `blocked` state. It neither amends rule
    18 nor asks R08c-3a/3b to change that state.
  - The kernel computes `priorDelivery` per attempt. It is `'none'` only when
    the durable head was unprepared at entry, or when `JSON.stringify` of the
    decoded head equals that of this instance's one-use
    `preparedWithoutDispatch` row. Nothing is persisted about dispatch.
  - The R08c-2c cleanup actions (`selectUnsent`, `discardUnsent`,
    `selectEmptyPreparationGate`, `retireEmptyPreparationGate`) require a
    `preparation_paused` lane with zero lane failures, no timer and no active
    attempt. They capture a `SchedulerCleanupAuthority`, run inside the host's
    `LocalOperationGate`, and recheck their context (including the wake marker)
    on every request of the deletion transaction.
  - `removeUnsent` deletes the head plus every row transitively naming a
    removed row in `base.after`, rejects a non-`unprepared` member, then
    updates or deletes the lane checkpoint and decrements `usage` in the same
    transaction. The low-level `LocalStore.discard` accepts only an
    `unprepared` head. No path deletes a `prepared` row today.
  - The store's `transaction` helper rejects with `storage_failed` for a
    native abort and `owner_not_ready` when its signal aborts it.
  - An epoch renewal (`resumeDelivery`) resets every lane counter and the
    aggregate counter, leaves `blocked` lanes paused and refreshes lane-scope
    `retry_paused` tokens. A delivery token runs only while its captured epoch
    equals the scheduler's epoch.
- **Inferred:** a `blocked` lane has no action at all, so it occupies a
  metadata slot until the scheduler closes. An `unresolved_after_rejection` or
  exhausted lane can leave only by a delivery resume that may be refused
  again. 256 such lanes reproduce the saturation that the 0047 contract
  addresses for preparation gates (its implementation is partial): a new durably admitted lane waits behind the marker with
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
- **Row identity comparison:** `JSON.stringify` of rows as produced by the
  store's strict decoder, the same comparison the kernel uses for
  `priorDelivery`. The decoder builds each row as a fixed object literal (the
  nested `base` has a single field), so the comparison does not depend on
  IndexedDB's stored key order. No other
  canonicalization is applied.
- **Rejection proof:** the in-memory record defined in rule 2.

## Proposed additive contract

These rules supplement 0041 rule 17 and 0047 rules 1–7. 0041 rules 5–6, 11–12
and 15–18, and 0047's authority, fencing, pending-identity and terminal-event
rules, keep their force wherever this record does not narrow them. This is not
generation reauthorization, access recovery, inherited-owner recovery, orphan
adoption or cancellation of an in-flight request.

Rules 3, 4 and 11 scope the **internal** R08c-3a/3b slices only. They do not
settle how a later public coordinator meets 0041 rule 17 and B11; those
choices stay open below and need a separate documentation PR.

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
   `blocked` pause object it creates, the bounded failure, and the frozen
   decoded prepared row that this attempt dispatched. The kernel supplies
   that row through an internal channel. It is never included in the published
   `attempt` observation. The proof is memory-only and never persisted,
   reconstructed from stored state, inferred from failure counts, status
   history or a canonical read, accepted from a caller, or transferred to
   another scheduler instance. It ends when its lane incarnation is retired
   or the scheduler closes or is fenced. It survives an epoch renewal and a
   failed disposal action, because neither re-dispatches the head. A lane holds
   at most one proof.
3. **`discardRejected` eligibility (internal).** `selectRejected(key, mode)`
   and `discardRejected(selection)` accept a lane only if all of these hold.
   The lane is settled. Its pause is the same `blocked` pause object recorded
   in its current proof. The bounded cleanup capture is of kind `intents` (no
   `receipts` anchor). The stored head is `prepared` and equal, under the row
   identity comparison, to the proof's dispatched row. Every other closure
   member is `unprepared`, and the lane contains no `acknowledged` row. Every
   other state is ineligible and rejects without changes (rule 8 gives the
   code): `preparation_paused`, `cleanup_paused`, any `retry_paused`
   (including `unresolved_after_rejection`, `ack_storage_failed`, network,
   invalid response, rate limit, server error, `unclassified_error_response`,
   `aggregate_exhausted` and `survivors_after_disposal`), `blocked`
   `access_unavailable`, a healthy, queued, timer-pending or active lane, and
   a lane without a current proof. That last case covers any lane after
   reload, crash or scheduler recreation. If a stored head differs from its
   proof, the action fails closed and keeps the gate and the proof. **An
   unknown head is never downgraded to rejected.**
4. **`abandonUnresolved` eligibility (internal).** `selectUnresolved` and
   `abandonUnresolved` accept a lane only if all of these hold. The lane is settled and has no rejection proof. Its pause is
   either `retry_paused` (any reason except `survivors_after_disposal`, lane or
   coordinator scope) or `blocked` with `access_unavailable` 403 or 404. A
   `blocked` 401 lane is ineligible: its authentication was invalidated, so
   rule 15's authority cannot hold on that scheduler. The
   capture is of kind `intents`. The stored head is `prepared`, which means no
   durable ACK. Every other closure member is `unprepared`, and the lane
   contains no `acknowledged` row. Ineligible: an `unprepared` head, an
   acknowledged head (trim), a `cleanup_paused` lane, and a lane holding a
   rejection proof (rule 3 applies). Exclusivity runs both ways: a lane has
   exactly one eligible disposal kind at a time. A head that this instance
   prepared but has not dispatched remains eligible and is reported as
   unknown, because the stored row cannot prove non-dispatch.
   `ack_storage_failed` is eligible: its observed receipt stays informational
   and is never turned into an ACK.
5. **Selection.** A selection is a distinct frozen opaque object
   (`RejectedDiscardSelection` or `UnresolvedAbandonSelection`) branded for its
   kind. Its context binds the fields in the recheck table below. Selection is
   caller-initiated only. No wake, scan, reconciliation, timer, enqueue,
   observation or other action creates or publishes one. A lane holds at most
   one current disposal selection of any kind, including 0047's. A new
   selection, a wake, a pause replacement (including token refresh), an epoch
   change, lane retirement and close all retire the previous one.
6. **Action entry.** 0047 rule 7 applies to both kinds without change. Runtime
   brand validation comes before the pending lookup. The same token returns
   the same Promise while pending. The token is consumed at entry, and a
   consumed token rejects after settlement. The action runs in the cleanup
   boundary after any active attempt settles. The lane's existing pause, proof
   and `DeliveryResume` token are not retired at entry. Another action on them
   during this one rejects with `invalid_selection` without consuming them.
7. **One transaction.** One readwrite IndexedDB transaction performs the
   whole disposal, bounded like 0041 rule 17 by the lane caps: at most 64 rows
   and 2 MiB of accounted bytes. A stored lane above those caps fails the
   strict decoder and is never partly deleted. It recaptures the lane and
   compares the capture signature (rows and checkpoint) first, then checks the
   phase rules of rule 3 or 4 and runs the recheck callback before and after
   every request. It deletes exactly the closure rows, including the
   `prepared` head. This is a new scheduler-only store operation: the
   low-level `LocalStore.discard` and `removeUnsent` keep rejecting `prepared`
   heads. It deletes the lane checkpoint when no survivor remains, or
   otherwise rewrites the checkpoint's row and byte counts to the survivors'
   sums, keeping its predecessor. It decrements origin `usage` rows and bytes
   by exactly the deleted rows' count and recorded `bytes`, which already
   include the 4 KiB reserve and the prepared-body wrapper. Only the
   transaction's terminal `complete` event commits; a request success does
   not. An abort, including one after the deletes succeed and before `usage`
   is written, leaves every row, checkpoint and counter intact. It also leaves
   the lane's pause, proof and resume token unchanged, makes no HTTP request
   and settles with the rule 8 code. A fresh selection is then required. 0047
   rule 2's close and authentication invalidation policy applies: a
   synchronous fence plus an abort attempt, where the terminal event decides.
   A completed transaction settles with its committed result even if close or
   invalidation follows.
8. **Rechecks, wakes and error codes.** The fields below are checked at these
   points (✓) with the given rejection code. The codes are the existing ones.

   | Field                                                           | Selection | Action entry | Each tx request | After `complete` | Code on failure     |
   | --------------------------------------------------------------- | --------- | ------------ | --------------- | ---------------- | ------------------- |
   | Scheduler open, host context authorized                         | ✓         | ✓            | ✓               | ✓ (publish only) | `owner_not_ready`   |
   | Cleanup authority `check()`/`signal`; same identity and signal  | ✓         | ✓            | ✓               | ✓ (publish only) | `owner_not_ready`   |
   | Lane key inside the scheduler's owner, URL, actor and epoch     | ✓         | ✓            | ✓               |                  | `owner_not_ready`   |
   | Token brand, registered, not consumed                           |           | ✓            |                 |                  | `invalid_selection` |
   | Lane incarnation current                                        | ✓         | ✓            | ✓               | ✓                | `invalid_selection` |
   | Same pause object; proof current (rejected kind)                | ✓         | ✓            | ✓               |                  | `invalid_selection` |
   | Epoch, lane failure count, wake marker unchanged                | ✓ (bound) | ✓            | ✓               |                  | `invalid_selection` |
   | Settled: not active, no timer, no other pending action          | ✓         | ✓            | ✓               |                  | `invalid_selection` |
   | Capture signature and phase rules (rule 3 or 4)                 | ✓ (bound) |              | ✓ (first)       |                  | `invalid_selection` |
   | `usage` and checkpoint counts cover the deleted rows            |           |              | ✓               |                  | `storage_corrupt`   |
   | Native transaction abort (quota, injected, browser)             |           |              | terminal event  |                  | `storage_failed`    |
   | Transaction aborted through the fence signal (close, auth loss) |           |              | terminal event  |                  | `owner_not_ready`   |
   | Online binding read failure (existing bounded code and status)  | ✓         | ✓            |                 |                  | `ArtifreeApiError`  |

   A wake for the lane between selection and the transaction's terminal event
   changes the wake marker and rejects the action with `invalid_selection`,
   with nothing deleted. This matches the R08c-2c recheck. After `complete`,
   a failed "publish only" check returns the committed result without
   retirement, publication, reconciliation or dispatch; the other checks
   cannot fail there, because the action holds the gate and the `acting`
   flag. A wake marker change after `complete` does not suppress retirement
   (rule 10).

9. **Results and counts.** A committed `discardRejected` settles with
   `{ kind: 'discarded_rejected', removedDependents: n }`. A committed
   `abandonUnresolved` settles with
   `{ kind: 'abandoned_unknown', removedDependents: n }`. Here `n` is the
   number of rows the committed transaction deleted, minus the head. It is
   computed once, inside that transaction. Neither result is an ACK, a
   rejection receipt, a rollback, non-delivery or a statement about the key.
10. **Budgets.** Disposal recovers only local storage accounting: origin and
    lane rows and bytes, and the reserve, released once by the committed
    transaction. It does not change the aggregate failure count, the epoch,
    backoff, other lanes' counters or tokens, or a preparation, cleanup or
    delivery choice of another lane. It releases no server key and makes no key
    reusable. A replacement intent gets a fresh key and a caller-chosen base. A
    survivor lane keeps its consecutive-failure count. A fully removed lane's
    count leaves with its metadata, and a later incarnation starts at zero like
    any newly created lane. This is not an epoch renewal: the aggregate count,
    which every disposed failure already consumed, still caps the epoch at 32.
11. **Post-commit retirement, full removal.** After the terminal `complete`,
    if the "publish only" checks pass and the lane incarnation is still
    current, retire synchronously and without awaiting. Remove the lane from
    the map, retire its pause, its `DeliveryResume` token if any, its proof and
    every cleanup selection, and keep no tombstone. Request one bounded
    reconciliation if capacity is waiting or if the wake marker changed after
    `complete`. Unlike 0047's unsent path, such a wake does not suppress this
    retirement: retirement depends on the committed rows and the lane identity,
    and the reconciliation honors the wake. Otherwise return the committed
    result with no retirement, publication, reconciliation or dispatch; that
    metadata dies with the fenced scheduler. This extends metadata retirement
    to the `blocked` and `retry_paused` lanes that 0047 rule 5 excludes, and
    only through a committed disposal of that lane's own rows. 0047's
    empty-gate eligibility is unchanged.
12. **Post-commit, survivors (internal).** If survivors remain, retire the old
    pause, its token, the proof and every cleanup selection. Then publish one
    new lane-scope `retry_paused` pause with the new internal reason
    `survivors_after_disposal` and a fresh `DeliveryResume` token. The token's
    context captures the scheduler's current epoch, which cannot change during
    the action. Its snapshot is the survivor row set that the committed
    transaction wrote and returned, in the form that existing resume
    validation compares. No second read is made, so publication cannot fail
    after commit. Emit one `paused` observation if publication is still
    allowed. The survivors keep their keys, bases and accounting. Wakes,
    enqueues, timers, reconciliation and another lane's ACK never unpause them.
    Another lane's epoch renewal only refreshes their token to the new epoch,
    as for other lane-scope pauses. Only a valid explicit `resumeDelivery`
    with that token unpauses them, under the existing fresh binding read,
    snapshot check and epoch renewal. The survivor head then prepares and sends
    normally. This pause is ineligible for either action in this record and
    for 0047's actions.
13. **Admission serialization.** The host's `LocalOperationGate` stays held
    from action entry until full-removal retirement or survivor publication
    completes, as 0047 rule 6 requires for empty-gate retirement. No durable
    admission can interleave between the commit and the metadata change. An
    admission and its wake after that point find either a new incarnation or
    the survivor pause. They are never mistaken for the retired incarnation.
14. **Fencing and late events.** A retired incarnation cannot be queued,
    paused, ACKed or dispatched. Every scheduler path compares lane identity
    before acting, as the existing `queue` and retire paths do. Late kernel,
    transaction, timer or observer callbacks from before the action find
    either a fenced generation or a non-current lane and change nothing. They
    cannot recreate deleted rows, write an ACK, release a successor or affect a
    later incarnation with the same key. Old selections and old resume tokens
    of the retired incarnation reject with `invalid_selection`. A request whose
    attempt ended by deadline or abort stays aborted. The server may still
    commit it, and that commit is not observed or recorded locally.
15. **Offline authority** for both kinds, as 0041 rule 17 requires. Before
    offline mode, the host must have established all of these. This page
    obtained the scheduler's actor and URL from a successful server binding
    response. It still holds that owner's exclusive Web Lock. The handle's
    captured page, profile and credential generation equals the current one.
    The handle is open and locally authenticated, with no logout, profile or
    credential replacement, 401 or unresolved authentication invalidation
    since that confirmation. The authority's `check()` verifies this at the
    rule 8 points. Its `signal` aborts on any loss. A caller label, saved token
    fingerprint, old handle, inherited session ID or a `404` is not this
    proof. Inability to establish it keeps the rows and fails closed. The
    contract does **not** prove current server access, that the request did
    not commit, that it will not still commit, rollback, purge, key release
    or that the artifact exists. An unknown outcome is never evidence of
    non-delivery.
16. **Bounded metadata and memory.** Proofs and selections live on their lane,
    at most one of each. A proof holds one decoded row: at most 272 KiB under
    0041 rule 15. A selection context holds one lane capture: at most 64 rows
    and 2 MiB of accounted bytes. A retired selection is deleted from the
    token map, so a caller still holding the token object keeps no context
    alive. The hard bound is therefore 256 lanes times one proof row plus one
    lane capture. While selections are current they mirror rows counted under
    the 8 MiB origin cap. A survivor pause reuses its lane's slot. Apart from
    the existing 256 lane entries, the reconciliation marker and the
    WeakMap-held tokens, nothing is added. Disposal never closes the whole
    scheduler to reclaim metadata, and never erases another lane.

## Alternatives

- **Persist a "rejected" marker so `discardRejected` survives reload.**
  Rejected. Stored state would claim knowledge that a reload cannot verify,
  which risks labeling an unknown commit as rejected. It would also require a
  new persistent format. After reload, the head is unknown (0041 rules 11
  and 17).
- **Infer rejection from failure counters, an epoch reset or a canonical
  read.** Rejected by 0041 rules 11 and 17 and by the #128 behavior.
- **Store only an identity of the dispatched row in the proof (sequence,
  record version and a digest).** Rejected. A digest needs asynchronous
  hashing inside the pump, and sequence plus version alone would not detect a
  rewritten request. The rule 16 bound is explicit, and the comparison is the
  one the kernel already uses.
- **Allow `abandonUnresolved` for a head with a valid rejection proof.**
  Rejected for the internal slices, because each head then has exactly one
  honest result label and one action kind. Abandoning a known rejection would
  be safe but would discard evidence. The public form is open question 3.
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
- **Let a wake during the action pass, and only suppress publication (0047's
  unsent post-commit behavior).** Rejected for these lanes in both
  directions. Before `complete`, a wake rejects the action, as the existing
  recheck does. After `complete`, it does not suppress retirement, because a
  stale `blocked` gate over deleted rows could never be selected again and
  would reintroduce saturation.
- **Publish a disposal choice automatically on `blocked`.** Rejected: a choice
  requires cleanup authority (a binding read online), and 0041 rule 1 requires
  explicit choices.
- **Abort an in-flight attempt or cancel a pending retry timer to abandon.**
  Not part of the internal slices: they accept only settled lanes. A retrying
  lane reaches a pause within the bounded 8/32 policy. The public answer is
  open question 1.
- **Retire empty `blocked` or `retry_paused` gates whose rows disappeared
  outside the scheduler.** Not added. No path deletes a `prepared` row outside
  the scheduler. Such a gate stays retained and fails closed (an unknown state
  never means empty), and 0047 rule 5 keeps it ineligible.

## Open questions for the owner

These would change how the public coordinator meets 0041 rule 17 and B11.
They are not decided here. The internal scoping in rules 3, 4 and 12 is not
an answer to them, and a later documentation PR settles them before public
activation.

1. **Abandoning while an attempt is in flight or a timer is pending.** 0041
   rule 17 requires fencing the lane's attempt and aborting its request
   before deletion. B11 allows deliberate abandonment "while delivery remains
   uncertain" and requires that a held response released after abandonment
   cannot resurrect rows or release successors. The internal slices cover only
   settled pauses. Options: (a) a later specification adds
   abort-then-abandon before public activation; (b) public activation offers
   abandonment only for settled pauses, and 0041 B11 is amended to say so;
   (c) a public "stop delivery" action first settles the lane into a pause,
   then this record's action applies.
2. **Disposing of survivors without resuming delivery.** The internal slices
   only offer resume. Options: (a) resume-only in the public form too; (b)
   extend unsent discard to the `survivors_after_disposal` pause; (c) one
   lane-wide choice that removes the head and all survivors.
3. **Public label for a known rejection.** The internal slices are exclusive.
   Options: (a) the public API mirrors that exclusivity; (b) the public API
   also lets a user abandon a known rejection as `abandoned_unknown`.

## Out of scope

- The public registry, page-global coordinator, public sender and their
  methods.
- Inherited-owner recovery and `RecoveryContinuation`.
- Disposal under a mismatched storage epoch or a changed actor generation.
  0041 rules 17–18 still require it for the public form.
- Generation reauthorization and access recovery, including 0041 rule 18's
  `retry_paused` `access_unavailable` state with a `DeliveryResumeSelection`.
  The merged host's token-less `blocked` access pause stays as it is.
- In-flight or timer-pending abandonment.
- **A never-prepared head outside a zero-failure `preparation_paused` lane
  has no disposal path.** This covers an unprepared head in a `retry_paused`
  lane (for example coordinator-scope `aggregate_exhausted`, which an epoch
  renewal clears) and the `survivors_after_disposal` pause. 0047 rule 5 and
  rule 4 here both exclude it. Its only exit is `resumeDelivery`; see open
  question 2.
- The management UI and runtime frames.
- Native-process crash restoration (R08d).
- Server changes.

The public browser handle and direct writes are unchanged.

## Slices and acceptance

**R08c-3a (internal `discardRejected`)** accepts this record and implements
every rule except rule 4, for the rejected kind, including rule 15's offline
authority. **R08c-3b (internal `abandonUnresolved`)** follows and adds rule 4
and the unresolved kind. Each slice is one code PR. It changes the internal
scheduler, the store (the new scheduler-only prepared-head deletion) and the
kernel channel, plus their native fixtures, and may not change a numbered
rule here.

Each slice uses native IndexedDB, Web Locks and Fetch in Chromium and Firefox,
runs `npm run check`, `npm run build` and the applicable smoke checks, and
receives both required reviews. Fixtures, reloads, injected aborts and
native-process termination are different evidence. Neither slice claims
native-process restoration. Real-server fixtures track every artifact and user
they create, clean up artifacts before disabling users, always close owned
contexts, and retain cleanup failures beside the original failure.

R08c-3a acceptance:

- **B22, rejected half.** With a real Rust server in both engines: real
  `data_conflict` (from a competing direct write), `data_path_conflict` and
  `invalid_request` heads with dependent, dependent-of-dependent and
  numeric-base successors yield `discarded_rejected` with the exact
  `removedDependents`. The closure is deleted. Survivors, foreign-binding
  rows and anchors stay, and the survivor checkpoint's counts equal the
  survivors' sums with its predecessor kept. Survivors stay paused until
  `resumeDelivery` with the survivor token; that resume then sends the
  survivor with its original key and base. `data_too_large`,
  `request_key_reused` and `payload_too_large` use owned HTTP fixtures,
  reported as fixtures.
- **No downgrade of an unknown head.** `selectRejected` and `discardRejected`
  reject in each of these cases:
  - `unresolved_after_rejection` (a real lost response, then a conflict);
  - the same head after scheduler recreation and after reload;
  - `access_unavailable`, network, invalid response,
    `unclassified_error_response`, `ack_storage_failed`,
    `aggregate_exhausted` and `survivors_after_disposal` pauses;
  - preparation-paused and cleanup-paused lanes;
  - a timer-pending or active lane.
- **Atomic rejection matrix.** Each case must leave rows, checkpoints and
  `usage` byte-identical, keep the pause, proof and resume token, send zero
  HTTP mutations, and reject with the rule 8 code:
  - wrong-kind tokens: every other selection or action token passed to
    `discardRejected`, and this token passed to every other action;
  - forged or unbranded objects;
  - a consumed token;
  - stale context: another lane's epoch renewal, a refreshed pause;
  - a wake between selection and entry, and a wake between two requests of
    the transaction;
  - another scheduler's token, or a key of another owner, actor or binding;
  - a newly enqueued dependent and a newly enqueued survivor;
  - a preparation, ACK or trim written into the lane through the store between
    selection and action;
  - a head rewritten through the store before selection (proof mismatch);
  - authority loss and scheduler close at each rule 8 point.
- **Bound.** A 64-row closure at the 2 MiB lane boundary is deleted in one
  transaction with exact counter deltas.
- **Abort before counters.** Inject an abort after the intent deletes succeed
  and before `usage` is written. Rows, checkpoints and reserves stay intact,
  the gate stays, no HTTP is sent, and the result is `storage_failed`. A fresh
  selection then commits once. A terminal abort (`owner_not_ready`) and a
  too-late abort followed by `complete` (committed result) are tested
  separately at close and at authentication invalidation.
- **Accounting once.** The origin and lane deltas equal the deleted rows'
  recorded bytes, including the reserve, exactly once. The aggregate count,
  epoch, other lanes' counters and tokens are unchanged before and after.
  Online mode makes exactly one binding read per selection and per action, and
  a failed read deletes nothing and does not fall back to offline. Offline
  mode makes no network request.
- **Offline authority.** Offline disposal succeeds only under rule 15's
  conditions. It fails closed with rows kept after any of: logout, a 401,
  profile or credential replacement, lock loss, a changed generation, a
  detached handle, or an aborted authority signal. This includes a change
  between selection and the transaction.
- **No resurrection and serialization.** After commit, old selections, old
  resume tokens and stale transaction or observer callbacks change nothing.
  A wake for the same key creates a new incarnation without failures, proof or
  pause. An admission attempted during the action waits for the gate and then
  sees either the new incarnation or the survivor pause. Post-commit close or
  authentication invalidation settles `discarded_rejected` with no
  publication, reconciliation or dispatch, and the lane metadata is not
  retired on the fenced scheduler.
- **Saturation.** With owned HTTP fixtures and explicit resumes between
  epochs, fill 256 slots with definitively rejected `blocked` lanes, then
  start a fresh epoch with budget remaining. A newly admitted lane waits with
  zero HTTP. One `discardRejected` lets reconciliation dispatch and ACK it.
  The other gates stay intact, and nothing is retired automatically.
- **Pending identity.** The same token called twice while pending returns the
  same Promise and one operation. A cross-kind call rejects while it is
  pending. A consumed token rejects after settlement. This holds at the
  transaction and post-commit boundaries.

R08c-3b acceptance, in addition, for `abandonUnresolved`:

- **B11 subset.** This slice covers these B11 clauses:
  - Selection, lock or binding mismatch and an injected IDB abort preserve
    rows and counters, and deletion stays within the 64-row/2 MiB lane.
  - A held response released after abandonment cannot resurrect rows or
    release successors (the late-response case below).
  - Real Rust server commit, lost response, then purge. The replay gets a 404
    access pause. Abandoning online and offline removes the closure and
    recovers quota with no replay, purge probe or ACK. Ordinary data routes
    still return 404.
  - Abandoning after network exhaustion without a purge yields
    `abandoned_unknown`. A later canonical read can show the committed value;
    the result claimed nothing about it.
  - `unresolved_after_rejection`, `unclassified_error_response`,
    `ack_storage_failed` (the receipt is never promoted), coordinator-scope
    `aggregate_exhausted` and recreated-scheduler heads are each covered.

  It does **not** cover these B11 clauses, which stay proposed:
  - abandonment while an attempt is in flight (rule 17's abort-first path);
  - a recovered candidate abandoning without a delivery grant;
  - supported-restore epoch mismatch;
  - different-actor and new-unauthenticated-handle cases, beyond the
    detached-handle case.

- **Exclusivity and phase.** A lane with a rejection proof rejects
  `abandonUnresolved`, and an unresolved lane rejects `discardRejected`.
  Unprepared, acknowledged, cleanup-paused, survivor-paused, healthy,
  timer-pending and active lanes reject. All R08c-3a cases above repeat for
  this kind: the matrix, bound, abort before counters, accounting, offline
  authority, no resurrection, saturation and pending identity.
- **Late response.** Hold a committed server response past the attempt
  deadline, exhaust the lane into a pause, abandon, then release the held
  response. No row is recreated, no ACK is written and no successor is
  released.

Guard mutations. Break each guard separately in the slice that implements it,
show the named assertion fails for that reason, then restore it and verify
the baseline:

| Broken guard                                                                        | Intended failing assertion                                                    |
| ----------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| Proof created without `priorDelivery: 'none'`, or accepted without a proof          | No downgrade: `unresolved_after_rejection` and recreated heads reject         |
| Proof's dispatched row not compared with the stored head                            | Matrix: head rewritten before selection rejects                               |
| Proof's pause object identity not compared                                          | Matrix: refreshed or replaced pause rejects                                   |
| Closure takes only direct dependents, or includes numeric-base rows                 | B22: dependent-of-dependent removed and survivor kept                         |
| Signature recheck removed from the transaction                                      | Matrix: newly enqueued dependent rejects with bytes unchanged                 |
| Wake marker not rechecked inside the transaction                                    | Matrix: wake between two requests rejects with nothing deleted                |
| `usage` written in a second transaction                                             | Abort before counters leaves `usage` unchanged                                |
| Survivor checkpoint not rewritten, or deleted while survivors remain                | B22: survivor checkpoint counts and predecessor                               |
| `removedDependents` taken from the selection instead of the transaction             | B22: exact `removedDependents` after a stale-free run with deep chains        |
| Metadata retired before `complete`                                                  | Abort before counters: the gate and resume token remain                       |
| Retirement performed after post-commit authority loss                               | No resurrection: fenced scheduler keeps the lane metadata, no publication     |
| Lane identity not checked after commit, or a post-commit wake suppresses retirement | No resurrection and saturation reconciliation                                 |
| Gate released before retirement or survivor publication                             | Serialization: admission during the action sees only the new state            |
| Survivor token snapshot read after commit, or bound to a stale epoch                | B22: survivor resume succeeds once without a refresh rejection                |
| `access_unavailable` or a `retry_paused` lane eligible for `discardRejected`        | Matrix: ineligible pauses reject                                              |
| (3b) Proof lane accepted by `abandonUnresolved`, or proof ignored by the check      | Exclusivity: both directions reject                                           |
| Aggregate count or epoch reset by disposal                                          | Accounting once: before and after counters are equal                          |
| Reserve not released, or released twice                                             | Accounting once: exact byte delta                                             |
| Survivors unpaused without `resumeDelivery`                                         | B22 or B11 subset: zero survivor HTTP before resume                           |
| Online read failure falls back to offline                                           | Accounting once: failed read deletes nothing and makes no offline transaction |
| Offline `check()` skipped before or inside the transaction                          | Offline authority: change between selection and transaction rejects           |
| Token not consumed at entry, or pending Promise not shared                          | Pending identity: one operation, consumed token rejects                       |
| (3b) Late kernel callback compared without lane identity                            | Late response: no row, ACK or successor after release                         |

## Implementation status

| Part                                 | Status                                |
| ------------------------------------ | ------------------------------------- |
| This specification (R08c-3s)         | Accepted by the R08c-3a code PR       |
| R08c-3a internal `discardRejected`   | Implemented (internal scheduler only) |
| R08c-3b internal `abandonUnresolved` | Implemented (internal scheduler only) |
| Public handle, registry, sender      | Not implemented; outside this record  |
| Native-process recovery (R08d)       | Not implemented; outside this record  |

The R08c-3a code change is covered by the internal outbox suites in both
browser engines and the real Rust server case; it adds no public registry,
sender or delivery. It ran no acceptance case for `abandonUnresolved`. The
R08c-3b code change adds the internal `abandonUnresolved` acceptance and guard
cases to the same suites; it adds no public registry, sender or delivery.
