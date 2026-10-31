# 0020: Runtime declarations and connection grants

Date: 2026-10-31. Status: accepted. Builds on
[0019](0019-runtime-metadata-bridge.md).

## Context

The first runtime view could read its own metadata, but did not distinguish
what an artifact requested from what the host supports or the user allowed.
The observed producer interface retained a declaration when a patch omitted
it, replaced it with a nonempty subset, and cleared it with an explicit empty
declaration. That is one account's publisher behavior, not evidence of
another actor's grant or of this implementation. The private trace records
the original evidence and the limits of that observation.

## Decision

1. **Declaration and pin.** An opted-in HTML entry may have an
   `artifree-runtime.json` file in its source revision. It is a complete
   snapshot: `sdk` must be `artifree.app.v1`, and `capabilities` is a list of
   distinct capability names. An omitted list and an empty list both declare
   none; a nonempty list replaces the whole declaration. An unsupported pin,
   malformed file or invalid name is refused. A capability can be declared
   without being supported; it cannot thereby be granted. For an existing
   0019 entry without this file, the marker declares `metadata.read` at the
   `artifree.app.v1` pin, preserving its source opt-in. An entry without the
   marker requests no runtime capabilities. Omitting the manifest file from
   an ordinary revision change is different: the existing file remains in the
   source package under the existing save rules. Deleting the file returns to
   the marker's legacy declaration.
2. **Support and consent.** R05 supports `metadata.read` only. The workspace
   shows the declaration, supported capabilities and current grant separately.
   The user explicitly grants a supported, declared subset; a blank grant
   removes all capabilities. A declaration or SDK type is never itself a
   grant. The artifact frame does not receive the account token.
3. **Connection and grant.** Each connected page uses an actor-bound,
   process-local runtime connection. The server issues a fresh random ID for
   each connection and never offers an operation to reopen an old ID. An
   abandoned creation whose reply was lost has no grant; a late grant or view
   request after close or server restart cannot create its missing connection.
   The server binds a grant to the
   connection, artifact, pinned revision and observed current source head.
   Grant changes advance a version; a view is bound to the version it was
   issued with. Removing and regranting permission cannot revive an old view.
   A grant replacement includes the source head and grant version the user
   observed. A late request or a retry after a lost response cannot overwrite a
   newer choice; it fails with a conflict and the workspace reads the grant
   again before another choice. Every accepted replacement advances the
   version, including a request to clear an already empty grant. That empty
   replacement is a barrier against an older, delayed request. The connection
   retains pending and unknown outcomes across view changes; when an answer is
   uncertain, the workspace pauses the frame and offers an explicit clear.
   It resumes only after the server confirms that clear with an advancing
   version. A malformed or non-advancing response remains uncertain.
   These bounded, ephemeral authorization records are not source mutations;
   source file writes continue through `ArtifactService` with their normal
   base revision and request key.
4. **Current authority.** Every runtime RPC checks the live connection,
   declared and granted capability, view binding and current artifact access.
   It checks them again after an asynchronous read. Revocation, a source head
   change, deletion, view replacement, expiry or logout refuses an old
   binding. The connection and pending issuance are coordinated so logout
   cannot be followed by a late successful grant or view create. Closing a
   connection is retryable. The workspace reports a failed or unknown close
   and pauses the workspace while retaining the connection until the server
   acknowledges it with the specified 204 response; it does not claim
   server-side logout on a local token removal alone. If close fails, the user
   may explicitly forget the token locally after being told that server grants
   may remain until expiry; this is not a confirmed server-side disconnect.
5. **Lifetime.** Connections and grants are held only in the running server,
   expire after a bounded lifetime and disappear on restart. A disconnected
   or expired connection cannot be reopened with the same ID. When the server
   specifically reports a missing or expired connection, the client shares
   one replacement attempt across concurrent views. It does not treat an
   unrelated missing artifact or view as a missing connection. The new
   connection starts without grants and asks for consent again; a permission
   change prepared on an old view is never replayed on the replacement.
   Every view names its connection so late results and old controls cannot
   affect the new one. Logout fences a pending replacement, and an
   unconfirmed logout prevents another replacement until the close is retried.
   The workspace keeps its source editor mounted during recovery. A user can
   also refresh the preview when an existing frame encounters an unavailable
   view; this lets it detect an expired connection without re-entering a token.

## Limits

The current server has one local bearer-token actor; registry tests cover
actor scoping but do not claim multi-user authentication. This decision does
not implement app data, models, agent handoff or other provider capabilities.
The preview sandbox and its network limitations remain those of 0003 and 0019.
