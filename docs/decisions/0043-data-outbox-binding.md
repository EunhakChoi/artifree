# Transactional binding for browser application-data writes

Status: implemented in this branch, pending the R08 specification in PR #100.
This is R08a only. Browser ownership, IndexedDB admission, delivery, crash
recovery and management UI integration remain proposed (R08b–d).

## Authority and transaction

An ordinary authenticated caller can read `GET /v1/data-outbox/binding`. It
returns exactly `actor_id`, `actor_generation` (a positive SQLite integer as
canonical decimal text), and `storage_epoch` (the existing feed identity).
The store reads the enabled user and feed identity in one snapshot, requires
the generation captured at authentication, and the server confirms the current
user again before returning. Missing, disabled or stale users receive the
existing fixed unauthenticated response and actor cleanup. There is no
administrator requirement, another user's state or health information.

The route rejects every present query component, including bare `?`, and a
nonempty body after authentication and admission. GET and implicit HEAD have
the same checks, status and `Cache-Control: no-store`; HEAD has no body even
on authentication failure. Only GET returns a usable binding.

Data PUT/PATCH/DELETE accept the all-or-none optional headers
`Artifree-Outbox-Actor`, `Artifree-Outbox-Epoch` and
`Artifree-Outbox-Actor-Generation`. A partial triple, duplicates or noncanonical
syntax is `400 invalid_request`. CORS allows the three headers. The authenticated
actor remains the actor used to authorize the artifact and its data space.
An inaccessible, missing, trashed or purged artifact returns the ordinary
indistinguishable `404 not_found` before any semantic binding comparison.

`ArtifactService` receives a typed optional fence. SQLite checks the target is
in place, then reads the enabled user's generation and feed identity inside
the same `BEGIN IMMEDIATE` as replay lookup, base comparison, mutation, receipt
and change record. It requires the expected actor, epoch and generation to
match current state, including the authentication generation. A mismatch is
`409 outbox_binding_changed` with no current tuple, key or mutation. It is
checked before historical replay and before key reuse. Disable/enable writes
use the same database serialization. Supported restore renews the feed
identity and invalidates a retained fence even for an otherwise valid replay.
Copying an identical database outside supported restore is outside this
fence's guarantee.

Existing direct core calls, HTTP requests without the headers, MCP, CLI and
management UI retain their current semantics. The fence is additional
authority, not part of a request fingerprint: after explicit actor-generation reauthorization
a retained logical request can still replay its original receipt. The browser
policy authorizing that reauthorization is not implemented by R08a. There is
no SQL migration, second database identity, stored token or new dependency.
The additive generated wire contract is version 0.17.0.

## Evidence and validation

Actor/epoch fencing and the wire shapes are Artifree decisions built on the
existing application-data replay contract (0031), user state generations
(0036), and restore identity renewal (0027). They are not claims about an
original provider's server implementation. Original evidence remains in the
private R08 trace; no captures or private paths are published here.

Behavioral tests exercise GET/HEAD input and authentication, header grammar,
free keys after rejection, successful writes/replay, denial precedence,
actor replacement, token rotation, disable/enable, stale authentication inside
the write transaction, and supported restore. Browser crash recovery and
outbox delivery tests remain future acceptance checks. A deterministic test pauses
the real binding GET after its SQLite snapshot, disables and re-enables the user,
then proves the final confirmation returns the fixed unauthenticated response
and runs the existing actor cleanup. A stale fence also wins over a used key
with a different payload.
