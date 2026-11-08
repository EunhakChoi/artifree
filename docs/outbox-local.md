# Browser-local application-data outbox

Status: R08b local-storage implementation ([0044](decisions/0044-browser-outbox-local.md)).
R08 as a whole remains partially implemented; public delivery and native-process recovery remain proposed.
The browser entry point `@artifree/client/outbox` is opt-in. It does not prepare,
transmit, replay, acknowledge, trim, retry delivery, or activate management UI
or runtime-frame access. Existing direct clients are unchanged. R08a supplies
the authenticated binding endpoint; R08c/R08d retain public delivery and native-process
recovery work from decision 0041.
Internal preparation/ACK persistence, a single-attempt native kernel and a bounded
fixed-authority scheduler are described in [0041](decisions/0041-browser-data-outbox.md).
They are not connected to this entry point or its page registry.

## Local API

`openBrowserOutbox({baseUrl, accessToken})` normalizes the URL before accessing
storage or transport, performs one authenticated binding GET, then opens a
page-lifetime coordinator. It uses browser Fetch with `redirect: 'error'` and
accepts no Fetch adapter. Binding reads have a 4 KiB bound, a 15 second pre-header deadline, a 60 second
whole-read deadline and the shared 45 second body-stall bound. The header timer
is cancelled when headers arrive; the whole-read timer covers the body too. Repeated opens for the same immutable connection share one
coordinator; close it before opening a changed URL or credential.

Binding failures reject with the existing `ArtifreeApiError` (`status`, `code`,
`retryAfterSeconds`), rather than a generic local storage error. Fetch network,
redirect rejection and absent-header timeout report `network_error` with status
0; malformed/oversized successful tuples report `invalid_response` with status 200. After headers, the actual HTTP status survives body cancellation, timeout,
malformed JSON and size rejection. Bounded recognized API errors keep their code;
429 keeps `rate_limited`, and a numeric `Retry-After` is clamped to 3600 seconds.
Diagnostics contain no response body, server message or redirect destination.
These reads never retry automatically. Every revalidation failure revokes local
cleanup proof and retains rows; 401 also terminally invalidates the handle,
even with a malformed or stalled body. A later valid response cannot reopen
that handle: close it and explicitly open a newly authenticated coordinator.
Other failures require deliberate revalidation before any fresh local authority;
offline cleanup never falls back from a failed read.

`enqueue({artifactId, path, base, operation, ...})` accepts `set` with `value`,
`update` with `patch`, or `delete`. A base is `{version: number}` from a
canonical read or `{after: sequence}` naming the lane's immediate predecessor.
Ordinary values use the direct client's serialization. `JsonText.text` and
path text remain verbatim, including inputs Rust may later reject. The result
`{kind: 'durably_enqueued', sequence, idempotencyKey}` means only that the
IndexedDB transaction completed; browser eviction and loss of origin/session
storage can still lose the work. No operating-system power-loss guarantee is
made.

Each page holds an exclusive Web Lock for its cryptographically random session
owner. A session-inherited owner with retained work returns
`recovery_required`; obtaining the lock proves exclusion, not that this is the
original physical tab. `confirmRecovery(state.selection)` explicitly authorizes
local access to the matching logical queue. A reload repeats this choice and
never enables automatic delivery. A changed actor generation returns
`generation_changed` without rewriting authority; reauthorization is R08c work.
A mismatched URL, actor or epoch returns `owner_binding_quarantined`, with no
foreign identity/payload diagnostic and no owner rotation or new enqueue.
If a later explicit action's binding check fails, `binding_paused` revokes local
admission authority. A newly learned identity/epoch/generation mismatch also
revokes that authority without changing retained rows. Close and reopen to
establish a new authenticated context; retained work repeats the recovery gate.

A busy inherited owner returns `owner_busy` after retaining its ID in the single
candidate slot and allocating a separate fresh owner for independent new work.
A distinct occupied candidate slot or failed persistence prevents replacement.
Saved candidates are never automatically inspected. The explicit
`retryRecoveryCandidate()` attempts their lock and returns `owner_busy`,
`recovery_empty`, `owner_binding_quarantined`, `generation_changed`, or a fresh
`recovery_required` choice. Confirmation remains separate. Only explicit
`clearRecoveryCandidate()` under its lock, a current binding, and owner-wide
emptiness clears the reference. Existence probes include intents, receipt
anchors and retained checkpoints across every binding, without reading foreign
payloads. There is no orphan adoption.

`listUnsentTargets(recoveredCandidate?, options?)` lists bounded original-target
metadata only for the held owner's rows at this handle's authenticated actor and
normalized API URL. It includes artifact ID, original storage epoch/admission
generation and head sequence/version, never payloads or paths. Another actor or
URL yields no targets. A saved candidate must first be deliberately locked by
`retryRecoveryCandidate()`; this list never discovers/adopts orphans.

`selectUnsent(artifactId, recoveredCandidate?, options?)` creates an opaque,
page-local `UnsentDiscardSelection` for a never-prepared head. Options are
`{mode?: 'online' | 'offline', storageEpoch?: string}`. The default mode is online;
the default epoch is the current binding. To clean a retained same-actor/URL
queue after restore, deliberately choose its original `storageEpoch` from the
target list. `selection.target` presents that original target and
`selection.mode` records the chosen mode; both are immutable. Neither can be
copied/edited into a valid action for another profile or epoch.

`discardUnsent(selection)` rejects stale/consumed/wrong-kind
selections, and atomically removes that head and its complete unsent dependent
closure while preserving independent rows. It reports `discarded_unsent` only
after commit. A failed deletion transaction leaves rows and accounting intact,
returns `storage_failed`, and pauses new enqueue in that lane until the page is
closed/recovered; a new explicit disposal choice remains possible. No server
key is released and no server receipt or rollback is claimed. Recovery tokens
cannot authorize disposal.

An inherited or saved queue does not need `confirmRecovery` before this explicit
local cleanup. Online selection and deletion each revalidate the binding and
require the owner's lock. A changed epoch/generation permits only local removal
of the selected original rows, without rewriting them, adopting their binding,
or granting enqueue/recovery/delivery authority.

Offline cleanup requires an explicit `{mode: 'offline'}` caller action, never an
automatic fallback after a failed read. Selection and deletion make no HTTP
request and require this still-open page's successful server binding proof,
unchanged immutable profile/action authority and held owner lock. A new page
must authenticate first; a remembered actor label/session ID is insufficient.
Every outbox binding failure, including401, redirects, network or malformed-body
failures, conservatively revokes that proof and all earlier/queued choices.
Offline work then fails closed until a deliberate successful online action
reestablishes the same binding, or a new authenticated handle is opened. No old
selection is revived, and enqueue/recovery authority is not silently restored.

Integrators **must call `invalidateAuthentication()` before logout or replacing
the profile/credentials, and when a related401 or unresolved authentication
invalidation is observed outside this outbox**. It terminally detaches that
handle, invalidates its proof/selections, aborts its binding requests and active
cleanup transaction, and retains its owner locks and rows. An old detached
handle cannot reauthenticate itself or clean up offline. To use a new profile,
explicitly close it and open a new authenticated handle. This opt-in library
cannot observe another client's logout/401 unless the integrator forwards it.

`close()` deliberately disposes the coordinator. It fences queued actions and
late helper continuations before they can start a new transaction, settles
already-started local transactions, and releases locks. Disconnect, network
failure or logout does not automatically call it or release the owner lock.
Closing the shared handle closes that document coordinator for all its callers.
Unlike authentication revocation, close permits an already-started cleanup
transaction to settle. Call `invalidateAuthentication()` first when disposing
because of logout/profile replacement; close alone is not authentication
revocation. No later response can start a new transaction after either fence.
Declining recovery means leaving the retained rows untouched. Missing Web Locks,
session storage, or IndexedDB refuses initialization; there is no volatile
fallback. Losing session references leaves retained rows dormant.

## Storage limits and verification scope

Schema 1 uses the dedicated `artifree-data-outbox` IndexedDB database. Admission
serializes the origin-wide usage ledger, lane checkpoints, sequence/key and
immutable intent in one transaction. Caps are 256 rows/8 MiB per origin,
64 rows/2 MiB per lane, 272 KiB per row, and 256 KiB raw value text. Accounted
UTF-8 payload and envelope bytes include a 4 KiB per-row reserve. In addition,
admission reserves two copies of the JSON-escaped path (prepared body and full
receipt) and two variable lane/owner anchors (receipt and checkpoint). Thus
long paths and URLs cannot silently use the fixed reserve. Payload strings are
counted once rather than as a second JSON-escaped copy. No automatic eviction occurs. Counter/index
metadata and retained envelopes are checked; recovery reads at most 32 records
per IndexedDB page, within origin/lane caps. A newer database or unknown format
fails closed without deletion. No earlier experimental format is recognized;
missing generation metadata is retained as corruption, never inferred as
current authority or migrated automatically.

The accounting bound is `P + M + 2E + 2L + 4096`: `P` is raw UTF-8 payload,
`M` is the UTF-8 JSON envelope without payload or the accounting field, `E` is
the UTF-8 JSON string of the supplied path, and `L` is the UTF-8 JSON envelope
`{lane, owner}`. This conservatively charges both shared lane anchors per row.
The internal prepared record replaces payload text with one exact body; its
numeric-base wrapper outside `P`/`E` is under 128 bytes. The unchanged identity
metadata, a full receipt (including its path), and the lane checkpoint use the
reserved variable copies. Their bounded keys, decimal generations/versions,
flags and field names fit the remaining 4 KiB. The browser suite constructs
these envelopes with maximum integer/generation spellings and measures
their complete UTF-8 footprint against admitted accounting. These are reservation
proof fixtures; the internal preparation/ACK store and transport checks separately
exercise their actual formats within this bound. They do not activate public delivery.

`npm run test:outbox` exercises real browser Fetch, IndexedDB and Web Locks with
independently authored owned HTTP fixtures. `ARTIFREE_BROWSER=firefox`
selects Firefox; Chromium is the default. The same local suite runs inside the
existing browser smoke command. The local public-API checks cover local portions
of B2, B3/B18, B8/B12, B13/B21 and B22/B23. Internal suites also exercise
preparation/ACK/trim, native mutation redirects and receipt rejection, bounded
retry scheduling and distinct preparation/delivery actions. The full smoke and
`scripts/test-outbox-delivery-live.mjs` add real Rust replay/dependency cases.
The internal scheduler suites also cover R08c-3a `discardRejected` and R08c-3b
`abandonUnresolved` in both engines (closure deletion and accounting, survivor pause, offline authority,
abort and fence cases, 256-gate recovery), and the full smoke adds its real Rust
rejected case and its lost-response-then-purge unresolved case. These do not establish complete B8/B15/B19, public lifecycle
integration or the end-to-end R08 matrix. Injected transaction aborts and page-local reopen checks
are not native-process crash restoration. Public recovery continuation,
reauthorization, access recovery and native-process recovery remain R08c/R08d
work; rejected disposal and unresolved abandonment exist only inside the
internal scheduler, with no public handle.
