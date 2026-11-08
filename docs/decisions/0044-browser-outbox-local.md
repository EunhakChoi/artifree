# Browser-local outbox ownership and durable admission

> Browser engines: Firefox is suspended in CI until the planned feature set is
> complete. A Firefox acceptance below is met in Chromium and reported as pending
> re-verification ([quality](../quality.md#browser-engines)).

Status: implemented R08b local-only slice of [0041](0041-browser-data-outbox.md).
R08a is [0043](0043-data-outbox-binding.md). R08 as a whole remains partial:
R08c delivery and R08d native-process recovery are not implemented here. This
record implements the merged specification; it changes no numbered rule.

## Entry point and ownership

`@artifree/client/outbox` is an explicit browser entry point. It shares the direct
client's faithful application-data serialization, generated binding contract and
bounded body reader. It does not prepare, send, replay or acknowledge writes,
trim payloads, retry delivery, or activate management UI/runtime-frame data
access. Existing direct HTTP, MCP, CLI and web writes keep their behavior.

One document registry, shared through a `Symbol.for` key across bundles, owns
one immutable authenticated connection. A second opener for that connection
shares the coordinator. Changing credentials or the configured URL requires
explicitly disposing that coordinator and opening another one.

Session storage retains a cryptographically random active owner and at most one
saved candidate. The coordinator acquires nonblocking exclusive Web Locks before
owner-row access. A busy inherited owner is retained in the candidate slot before
a fresh owner is allocated. A distinct occupied slot or failed reference write
fails without replacement. Owner-wide existence-only probes cover intent,
receipt-anchor and checkpoint indexes before any empty-owner rotation or explicit
candidate clear. Foreign retained bindings are quarantined with a fixed state,
without exposing their identities, URLs, counts or payloads.

A free inherited owner with matching retained work always requires an explicit
recovery choice. A lock proves exclusion, not physical-original-tab identity.
Saved candidates are probed only by `retryRecoveryCandidate()`. Confirmation is
page-local and never persisted as an automatic-resume flag. Lost session
references leave rows dormant; there is no orphan adoption.

Disconnect, failed authentication and network loss do not implicitly dispose a
coordinator or release its locks. `close()` is the deliberate coordinator-disposal
operation permitted by 0041 rule 6, not an automatic logout hook. It fences both
shared handles and queued actions immediately. Every asynchronous binding or
metadata step is followed by a synchronous open check at its caller's storage
boundary: a close microtask between a helper's check and its caller's continuation
cannot start another transaction. Already-started IDB transactions settle before
the database and locks are released. Closing never deletes retained rows or
session references; a later opener repeats the applicable recovery gate.

Binding GETs use independent 15-second pre-header and 60-second whole-read
deadlines, alongside the shared 45-second body-stall limit and 4 KiB body cap.
Headers cancel only the pre-header timer. Completion, failure and close release
all timer/listener resources. Native Fetch redirect rejection is `network_error`
with status 0; other binding failures preserve `ArtifreeApiError` status and
classification, including HTTP metadata after an invalid or timed-out body.
Recognized bounded error bodies retain their code without exposing server text;
429 retains `rate_limited` and bounded `Retry-After`. No read failure retries,
consumes a mutation budget or authorizes storage mutation. Every revalidation
failure fences local cleanup proof and stale selections; observed 401 terminally
invalidates the page handle, including when its body is invalid or stalls.

## Durable local admission and cleanup

Schema 1 uses the dedicated `artifree-data-outbox` IndexedDB database. Admission
commits the immutable intent, unique key, monotonic sequence, generation metadata,
lane checkpoint and origin-wide counters together. Success follows the
transaction's `complete` event, never an individual request's success. Aborts
preserve accepted rows and counters. Browser eviction and session/origin-storage
loss remain possible; no OS power-loss guarantee is made.

Caller values are snapped synchronously using existing client serialization.
Raw `JsonText.text` and path text remain verbatim, even when Rust may later reject
them. Local admission checks envelope, ownership, dependencies and capacity;
it adds no application-data path grammar, JSON parsing or numeric normalization.

Caps are 256 retained intents/8 MiB per origin, 64 rows/2 MiB per lane and
272 KiB accounted bytes per row, with the existing 256 KiB raw value limit.
[The reservation bound](../outbox-local.md#storage-limits-and-verification-scope)
counts the payload once, the current envelope, future body/receipt path copies,
receipt/checkpoint lane anchors and a fixed 4 KiB reserve. It introduces no
arbitrary path/URL-length cap. A conservative reservation fixture constructs the
planned prepared/full-receipt/checkpoint envelopes independently and verifies their
UTF-8 footprint; those future lifecycle formats are not enabled by this record.
Origin admission is one IDB write transaction across owners, so competing pages
cannot both consume the final row or byte reservation. There is no eviction or
volatile fallback.

Recovery validates envelope fields, enums, versions, identities, schema/index
shape, dependencies and counters. Payload text is not domain-parsed. Native
materialized IDB pages contain at most 32 rows; owner probes return only existence.
The origin/lane caps bound scans and local deletion. There is no earlier recognized
experimental format: missing generation metadata and unknown/newer formats are
retained and fail closed, never inferred as generation 1 or silently migrated.
The conditional migration policy in 0041 remains applicable if a supported older
format is ever explicitly recognized.

`selectUnsent` returns a distinct opaque one-action selection for the head and
its never-prepared dependency closure. For an inherited or saved candidate, this
is the explicit local-cleanup exception in 0041 rule 17: it does not require
`confirmRecovery`, adopt the candidate, authorize new enqueue into it or grant
delivery. Online selection/deletion use a fresh binding. Explicit offline mode
uses this page's retained successful binding proof, unchanged immutable profile,
action authority and held owner lock, and sends no HTTP. Failed binding reads
conservatively revoke this proof, including401/network/redirect/body failures;
there is no automatic offline fallback. Integrators must forward external401,
logout/profile/credential invalidation through `invalidateAuthentication()`.
That terminally detaches the handle while retaining its locks, aborts active
cleanup transactions and invalidates selections; only explicit close/reopen can
replace the detached profile. A fresh deliberate successful online action can
renew proof after a nonterminal failed read, without renewing old selections or
enqueue/delivery authority.

`listUnsentTargets` uses a binding-index prefix restricted to the same owner,
authenticated actor and normalized URL. It exposes only bounded original-target
metadata, not foreign actor/URL rows or payloads. A selection's immutable target
identifies its original epoch/generation and exact head. Deliberate old-epoch or
old-generation cleanup deletes those never-prepared rows only; it never rebinds
them or reauthorizes delivery. Unknown/prepared abandonment remains R08c work.

Page-local action-authority tokens fence queued operations and old selections
after authentication failure, binding/generation mismatch or explicit close. They
are not server actor generations and are never persisted. Selection kinds use
separate WeakMaps. Consumed/wrong-kind/context/stale tokens reject without changes.
Deletion rechecks the complete lane snapshot and phase in one transaction, removes
only the selected head and dependent closure, releases their actual accounting
once, and preserves independent rows and owner references. A storage abort pauses
that lane; a new deliberate choice is needed. Cleanup grants neither recovery nor
delivery authority, and does not release server keys or claim a server rollback.
Authentication invalidation is connected to the entire active IDB transaction
lifetime: even after a delete request's success, revocation aborts that transaction
and retains rows/checkpoints/counters. Explicit coordinator close is different:
it prevents a new transaction but lets an already-started one settle. Previously
committed cleanup cannot be undone by later revocation; terminal events establish
the actual result.

## Local acceptance and exclusions

| 0041 local subcase    | Implementation and executable evidence                                                                                                                                                                                                                                                                                               |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| B2 enqueue            | Native IDB request-success followed by abort rejects; complete precedes durable acceptance; immutable ordinary/raw snapshots survive actual page reload with original owner/key.                                                                                                                                                     |
| B3/B18 owner          | Real tabs and shared/duplicate-bundle handles cover exclusion, retained candidate/full slot/write/probe failure, owner-wide anchors, quarantine, explicit recovery/clear, and lock retention through auth/network pause versus explicit close.                                                                                       |
| B8/B12 store          | Row/byte races across owners, lane/origin/row/value caps, variable/fixed reservations, bounded recovery reads, malformed enum/version/generation/counter/index and newer schema all exercise retained-state rejection.                                                                                                               |
| B13/B21 metadata      | URL validation precedes owner/storage/network; authenticated native Fetch rejects same/cross-origin redirects, invalid status/tuple and oversized/stalled reads without retry or authority. The full browser smoke also connects this entry point to the real R08a server before admission/reload/local cleanup.                     |
| B22/B23 local cleanup | Typed unconfirmed inherited/saved-candidate cleanup, changed-generation cleanup without reauthorization, closure/stale selection/abort accounting, shared-handle close races including the helper/caller microtask gap, and already-started transaction settlement use real browser boundaries. Existing serialization stays shared. |

The executable matrix is in `scripts/smoke-outbox.mjs`; `npm run test:outbox`
runs its owned metadata-fixture cases, and the existing full smoke adds the real
server binding case. Chromium and Firefox are required. Important guards have
negative mutation proofs, including the two distinct close races and the local
cleanup/recovery-authority separation. Actual run results and review dispositions
belong in the PR/verification record, not inferred from this table.

B15 and B19 are excluded in full. Every mixed-case mutation/send, preparation,
preparation retry, definitive rejection, replay, ACK/checkpoint advancement/trim,
HTTP retry budget, generation reauthorization and unresolved/prepared abandonment
clause remains R08c. Never-prepared old-epoch/offline cleanup is implemented here;
unknown-outcome abandonment is not. Actual native-process termination/session restoration, B10,
the complete browser/server recovery matrix and its public support guide remain
R08d. Reload and injected IDB faults are not native-process evidence. Required
full checks and both browser smokes remain intact.

Ownership arbitration, quotas, wire/state shapes and cleanup fencing are Artifree
design decisions implementing the approved specification, not claims about an
original provider's internals. Original-to-requirement evidence remains private;
no private captures, credentials or paths are included here.
