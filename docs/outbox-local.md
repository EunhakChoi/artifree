# Browser-local application-data outbox

Status: R08b local-storage implementation. R08 as a whole remains proposed.
The browser entry point `@artifree/client/outbox` is opt-in. It does not prepare,
transmit, replay, acknowledge, trim, retry delivery, or activate management UI
or runtime-frame access. Existing direct clients are unchanged. R08a supplies
the authenticated binding endpoint; R08c/R08d retain delivery and native-process
recovery work from decision 0041.

## Local API

`openBrowserOutbox({baseUrl, accessToken})` normalizes the URL before accessing
storage or transport, performs one authenticated binding GET, then opens a
page-lifetime coordinator. It uses browser Fetch with `redirect: 'error'` and
accepts no Fetch adapter. Binding reads have a 4 KiB bound and a 45 second
request deadline. Repeated opens for the same immutable connection share one
coordinator; close it before opening a changed URL or credential.

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

`selectUnsent(artifactId, recoveredCandidate?)` creates an opaque, page-local
`UnsentDiscardSelection` for a never-prepared head. `discardUnsent(selection)`
revalidates the authenticated binding, rejects stale/consumed/wrong-kind
selections, and atomically removes that head and its complete unsent dependent
closure while preserving independent rows. It reports `discarded_unsent` only
after commit. A failed deletion transaction leaves rows and accounting intact,
returns `storage_failed`, and pauses new enqueue in that lane until the page is
closed/recovered; a new explicit disposal choice remains possible. No server
key is released and no server receipt or rollback is claimed. Recovery tokens
cannot authorize disposal.

`close()` fences new actions, settles local transactions, and releases locks.
Closing the shared handle closes that document coordinator for all its callers.
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
The future prepared record replaces payload text with one exact body; its
numeric-base wrapper outside `P`/`E` is under 128 bytes. The unchanged identity
metadata, a full receipt (including its path), and the lane checkpoint use the
reserved variable copies. Their bounded keys, decimal generations/versions,
flags and field names fit the remaining 4 KiB. The browser suite constructs
these planned envelopes with maximum integer/generation spellings and measures
their complete UTF-8 footprint against admitted accounting. These are reservation
proof fixtures, not an implemented preparation/ACK format or sender. R08c must
stay within this bound or change admission before accepting its requests.

`npm run test:outbox` exercises real browser Fetch, IndexedDB and Web Locks with
an independently authored owned HTTP metadata fixture. `ARTIFREE_BROWSER=firefox`
selects Firefox; Chromium is the default. The same local suite runs inside the
existing browser smoke command. It covers local portions of B2, B3/B18,
B8/B12, B13/B21 and B22/B23 only. Its injected transaction aborts and page-local
reopen checks are not native-process crash restoration. B15/B19, actual mutation
redirects, preparation, server rejection, replay, ACK, delivery retry budgets and
the end-to-end browser/server matrix remain R08c/R08d work.
