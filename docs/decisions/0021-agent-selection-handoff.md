# 0021: A selected source range for an agent session

Date: 2026-10-31. Status: accepted. Builds on
[0017](0017-code-workspace.md), [0018](0018-source-draft-recovery.md), and
[0020](0020-runtime-grants.md).

## Context

A source editor can identify a saved file and the text selected by its user.
An inbound MCP tool does not itself give the web page a reverse call to a
particular external agent. A hosted example distinguished staging, explicit
send, cancellation and a routing acknowledgement; the acknowledgement was
not evidence that model work finished. A shipped type declaration described
page content as data, not the user's instructions. The unpublished trace
separates those observations and source-only descriptions from the choices
below; they do not establish an external agent transport for Artifree.

## Decision

1. **Addressable agent.** An authenticated MCP client explicitly registers a
   named, expiring inbox and receives a random session identity and secret.
   It polls and acknowledges that inbox through MCP. The workspace lists the
   actor's registered names and identities, not their secrets or a claim that a
   model is running. A client must actually poll; an ordinary inbound MCP
   connection does not subscribe or receive a page callback by itself.
2. **Pinned selection.** The workspace stages a nonempty selection from a
   saved, editable text file, with artifact, source revision, path, file
   digest and UTF-16 offsets in the editor's LF-normalized text. The server
   reads the pinned file under the authenticated actor, verifies its digest
   and UTF-8 boundaries, normalizes its line endings as the editor does, and
   constructs the selected text itself. Unsaved or replaced source is not
   presented as the saved revision. User intent and selected source are
   separate fields; source text is untrusted data, not an instruction. The
   intent is retained as entered, with whitespace-only requests refused.
3. **Consent and state.** Stage records the proposed delivery without making
   it available to the agent. The workspace shows the selected session,
   revision, path, range, source text and intent before a separate trusted
   user send. The first send rechecks the pinned source before changing the
   stage to queued. A retry with the same server-issued ID returns the recorded
   delivery state without requiring the source to remain available. A recorded
   offer, block or acknowledgement can still be read after that workspace
   closes; an unoffered item becomes expired and cannot be sent. If another
   send or cancellation commits while that first source check fails, the
   transition is checked again before reporting an error. A staged or queued
   delivery can be cancelled; after it is offered to an agent, cancel cannot
   claim to recall it. These transitions are atomic with polling. A successful
   HTTP stage returns a Location that includes the workspace session ID needed
   to retrieve that handoff; access still requires the actor's bearer token.
4. **Delivery and receipt.** Polling offers a queued item to its registered
   session and repeats the same item until that session acknowledges receipt.
   Agents must deduplicate by delivery ID. If a source lookup proves the pinned
   file is missing or its digest changed, the offered item becomes blocked and
   is skipped by later polls, so it cannot starve other deliveries. It may
   already have been received: it cannot be recalled and the agent may still
   acknowledge it. Restoring the source does not silently resume it; the user
   can stage a new request. A temporary storage failure leaves the offer
   retryable. Receipt is not model completion; this slice does not run a model
   or report its success. An expired inbox, an unavailable server or a lost
   answer is reported rather than treated as successful delivery.
5. **Connection and lifetime.** A staged item is bound to the actor and the
   current workspace connection. Logout or connection expiry stops new offers
   and further replays. It cannot recall an item already offered; the agent
   may still acknowledge that item. The server rechecks the connection and
   offer after a source lookup, so a lookup delayed across logout cannot
   produce a later delivery. A status read reports an unoffered stage or
   queued item as expired when its connection or agent session has ended,
   without waiting for an agent to poll.
   The inbox and handoff states are bounded and process-local, expire, and
   disappear on server restart. They are not source mutations or a durable
   job queue; artifact writes still use `ArtifactService`, a base revision,
   and an idempotency key.

## Limits

Only the local bearer-token actor is implemented. The selection comes from the
first-party source editor, not an arbitrary preview frame. The account token
and agent inbox secret are never passed into a preview frame. A working MCP
polling client proves transport and acknowledgement, not that a live model
completed an edit. External agents need to register, poll and acknowledge;
provider-specific background execution is outside this change.
