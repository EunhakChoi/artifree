# Outbox disposal and empty preparation-gate retirement

Status: **Accepted contract; implementation pending.** Acceptance takes effect
when this separate documentation-only approval change is reviewed and merged.
It accepts the additive contract below for a subsequent implementation PR;
it introduces no executable behavior and does not claim the implementation is
complete. The numbered rules of [0041](0041-browser-data-outbox.md) and rules 1–7
below remain unchanged. R08 remains partially implemented; the public browser
handle remains local-only and public delivery remains disabled.

The earlier proposal PR merged the specification with status Proposed. That
merge did not record this explicit contract acceptance. This approval must merge
before the implementation PR can merge; an implementation PR cannot supply its
own specification approval or treat passing tests as approval. Separate review
and required gates remain mandatory for both changes.

> Supplemented by [0049](0049-outbox-rejected-unresolved-disposal.md)
> (accepted by the R08c-3a code PR; implemented in the internal scheduler only
> by R08c-3a and R08c-3b): rejected disposal and
> unresolved abandonment, which this record excludes, and metadata retirement
> after those committed disposals. Rules 1–7 below, including rule 5's
> empty-gate eligibility, are unchanged.

## Problem and evidence

0041 rule 17 already requires typed never-prepared disposal, callback fencing and
atomic local deletion. Its R08c-2b implementation note records a distinct gap:
256 retained preparation pauses can occupy the scheduler even after low-level
store deletion removes their rows. An unrelated durably admitted lane then waits
behind the bounded metadata limit. Empty scans are deliberately insufficient to
erase an existing pause or its history.

The repository fixture `capacity-retained-gates` in
`scripts/smoke-outbox-scheduler.mjs` creates 256 preparation failures, directly
calls `LocalStore.discard`, and expects zero HTTP mutations and a pending
reconciliation. This is a native-browser fixture definition using owned HTTP
fixtures and injected IndexedDB failures, not evidence of native-process crash
restoration. This proposal was prepared by source inspection; it records no new
execution result. The existing live-server replay/dependency fixtures do not
establish this proposed disposal integration or the complete R08 matrix.

The original observations cited by 0041 distinguish durable rows, logical tab
ownership and explicit recovery. They do not specify scheduler metadata disposal.
The actions and limits below are Artifree-designed contract decisions accepted
by this documentation-only change, not facts observed in the original product.
Private evidence and its paths remain outside this repository; implementation
must maintain the
private requirement-to-original-to-test trace required by the roadmap.

## Accepted additive contract

The following additive rules supplement rule 17 and its R08c-2b
note. Rules 5–6, 11, 15–18 and their existing authority, recovery and budget
constraints retain their force. This is not generation reauthorization, access
recovery, orphan adoption, rejected disposal or unknown-outcome abandonment.

1. **Scheduler-owned unsent disposal.** For a fixed authorized owner/binding,
   an explicit internal selection identifies a never-prepared head, its complete
   dependency closure, the complete observed lane snapshot, scheduler gate
   identity, page/authority context and epoch. Use a distinct opaque one-action
   token with runtime kind/context validation. A preparation retry, delivery
   resume, cleanup retry or recovery choice cannot authorize disposal. Apply
   0041 rule 17's authenticated same-actor/URL online/offline cleanup proof and
   owner-lock conditions; an ordinary delivery revalidation callback alone is
   not a replacement for cleanup authority. Selection grants no delivery.
2. **Fence before mutation, publish after commit.** Serialize the action with
   admission, preparation, ACK and trim through the owning coordinator/scheduler
   boundary. Before deletion, fence that lane's callbacks and pending local
   preparation, cancel its timer, and settle any older lane operation. Preserve
   other lanes' authority. In the same bounded IDB transaction as deletion,
   recheck the exact snapshot, head/closure phases and versions, lock and cleanup
   authority; delete only the selected rows and unneeded checkpoints and release
   their actual accounting once. Report `discarded_unsent` only after transaction
   completion. Only then retire obsolete selections and metadata for a fully
   removed lane, and request bounded reconciliation for capacity waiters.
   The IDB terminal event determines deletion's result: logout, authentication
   invalidation or close after commit cannot undo deletion or relabel it as
   rollback/storage failure. Settle the initiating action with its committed
   `discarded_unsent` result, while suppressing stale observations, metadata
   publication, reconciliation and dispatch under the invalidated context.
   Authentication invalidation and explicit close both follow the internal
   scheduler's existing policy: synchronously fence work and attempt to abort
   its active native transaction, then await that transaction's terminal event.
   This does not adopt the public local coordinator's different close policy
   of allowing an already-started transaction to settle without requesting an
   abort. A terminal abort retains rows/accounting and yields failure; a terminal
   complete preserves the committed deletion result even if the abort request
   arrived too late. A close signal or abort request alone proves neither
   rollback nor commit. Neither outcome grants post-close/authentication authority.
3. **Survivors and failure.** Independent survivors remain paused and retain
   their accounting and applicable history until their existing explicit action.
   A newly admitted dependent or changed phase/version rejects the entire stale
   choice without expansion. Abort retains rows/counters and the lane pause;
   another deliberate choice is required. Neither committed disposal nor failed
   disposal refills failure budgets, changes an epoch, authorizes a successor,
   releases a server key or claims rollback. Late callbacks cannot prepare, ACK,
   resurrect deleted rows or dispatch survivors. A stale callback cannot affect a
   later lane instance with the same key.
4. **Already-empty gates require a different action.** Add the proposed internal
   state `empty_preparation_gate` with an opaque `EmptyPreparationGateSelection`
   and explicit `retireEmptyPreparationGate(selection)` action. It is not an
   `UnsentDiscardSelection` and returns `retired_empty_preparation_gate`, never
   `discarded_unsent`, ACK, recovery confirmation or delivery resume. No empty
   scan, wake, enqueue, timer, preparation retry or successful unrelated action
   implicitly invokes it. Bounded reconciliation may publish this explicit
   choice for an eligible retained gate, including when capacity is saturated.
   Publish at most one current choice per retained gate, within the existing
   256 metadata slots; do not add an unbounded key or tombstone collection.
5. **Narrow eligibility and identity.** An empty-gate choice is available only
   for this still-open scheduler's retained preparation-paused lane, with zero
   lane delivery failures, no in-flight attempt, retry timer or unresolved
   delivery history, and no durable intent, receipt or checkpoint for that lane.
   A blocked, access/retry/cleanup-paused, unknown/prepared or durable-ACK lane
   cannot use it. Bind the choice to the original lane/gate incarnation,
   owner/binding/generation, page/cleanup authority, observed wake/version marker
   and epoch. Require the same cleanup proof and exclusive owner lock as rule 1.
   It cannot inspect foreign bindings or release an owner reference; owner-wide
   emptiness remains separately required by 0041 rule 6.
6. **Transactional emptiness and publication.** At explicit action entry retire
   the token, fence this gate and serialize its native storage check with all
   lane writes. Recheck complete lane emptiness, including receipt/checkpoint
   anchors, and cleanup authority in one bounded IDB transaction. A stale gate,
   new row/anchor, concurrent admission, changed authority or storage failure
   leaves the gate intact and paused, with no capacity release or budget change.
   Only successful transaction completion plus unchanged captured gate context
   permits synchronous in-memory retirement. The host must serialize admission
   through this retirement publication; a post-commit enqueue or wake cannot be
   lost or mistaken for the retired incarnation. Then reconciliation can discover
   already-authorized durable lanes at its existing FIFO position. This supplies
   no authority to recovered work and does not reset aggregate failures or epoch.
   Failure can supply a fresh choice after a new successful bounded capture;
   unknown state never means empty. Do not erase other gates or close the entire
   scheduler merely to reclaim this lane's metadata.
7. **Pending action identity.** For both proposed action kinds, install the
   pending entry before any asynchronous work or reentrant callback. Repeated
   calls with the same token and same kind during that action return exactly
   the same Promise until settlement, including after token consumption.
   Runtime kind validation precedes pending lookup: passing that token to a
   different action rejects without borrowing its pending Promise or authority.
   Remove the pending entry at settlement; subsequent use of the consumed token
   rejects. Coalescing does not permit an extra transaction, binding read,
   publication, budget change or disposal attempt.

No persistent format, new server endpoint, dependency, management UI or public
entry-point method is proposed here. The first implementation is internal only,
for the fixed fresh-owner host already supported by R08c-2b. A later public
integration must implement the same cleanup proof and page registry ownership;
this document does not establish that integration. Inherited logical owners still
require explicit recovery for delivery; physical original-tab identity remains
ambiguous. Public activation also waits for the other blockers in 0041.

## First implementation PR and minimum acceptance

The proposed R08c-2c implementation changes the internal scheduler/store and its
native-browser fixture, retaining the public local-only boundary. Its observable
result is capacity recovery after explicit committed cleanup without erasing
retained gates automatically or renewing delivery authority.

- Drive 256 preparation pauses through typed scheduler disposal; after committed
  deletion, an already-authorized new durable lane is reconciled and dispatched.
  Retain a separate low-level deletion fixture: no automatic progress, followed
  by explicit eligible empty-gate retirement and bounded progress.
- Reject wrong-kind, forged, consumed, stale-context and stale-incarnation
  choices without row/counter/gate changes. A new dependent, preparation/ACK
  transition, checkpoint or wake/admission race cannot broaden the selection or
  erase newly admitted work. Ineligible delivery/ACK/history gates stay intact.
- Inject deletion-request success followed by transaction abort; rows, reserved
  accounting and gates remain. Exercise auth invalidation and close at the
  transaction/publication boundary, plus a late callback from the old incarnation.
  Prove unrelated lanes and paused independent survivors retain their meaning.
- Separate post-commit logout/authentication invalidation and close from
  pre-commit abort: committed disposal settles as `discarded_unsent`, rows and
  counters remain deleted, and no stale publication/reconciliation/dispatch
  follows. Both authentication invalidation and explicit scheduler close must
  synchronously fence and attempt native transaction abort. Test a terminal
  abort retaining rows/accounting and a too-late abort request followed by
  terminal complete preserving committed deletion; neither the abort request
  nor close signal alone determines the result. Do not import the public local
  coordinator's close settlement policy. These outcomes preserve 0041 rule 17's local-only result
  and existing authority model, without rollback or delivery claims.
- For each new action kind, call the same token twice while pending and assert
  Promise identity and one actual operation. Cross-kind calls reject while the
  original Promise remains pending, and consumed-token calls reject after it
  settles. Verify this at transaction and post-commit publication boundaries.
- With nonzero aggregate failures, compare epoch and retry allowance before and
  after both actions. Neither action performs an HTTP mutation/binding read of
  its own in explicit offline mode, retries automatically, or resets the 8/32
  budgets. Eligible new work may use only the existing remaining authority.
- Break closure validation, commit-before-retirement, authority fencing and the
  no-budget-reset guards separately; require the targeted checks to fail for
  those behavioral reasons, restore each guard and verify the baseline.

Use native IndexedDB/Web Locks/Fetch fixtures in Chromium and Firefox, and run
`npm run check`, `npm run build` and applicable smoke checks. Treat a fixture,
reload, injected abort and actual native-process termination as different
verification evidence. This spec-only PR also requires check/build and the two
reviews required by `.github/CONTRIBUTING.md`; it adds no executable behavior.
Future real-server fixtures must track all created artifacts/users, clean up
artifacts before disabling those users, always close owned contexts/listeners,
and retain cleanup failures alongside the original failure. No users or artifacts
are created by preparing this documentation proposal.
