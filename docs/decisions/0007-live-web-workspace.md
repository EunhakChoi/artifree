# 0007: The web workspace follows committed changes

Date: 2026-10-28. Status: accepted. Builds on
[0005](0005-request-limits.md) and [0006](0006-change-feed.md).

## Context

The change feed (decision 0006) tells clients what was committed, but the
web workspace showed what it read when it opened: a save by an agent or in
another tab appeared only after a manual reload. Reading again on every
notice would be wrong in three places: a preview pinned to an older
revision, a draft the user has not saved, and the request budget the page
shares with every other client of the same actor.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                                    | Basis                                                                                                                                                                                                                       |
| --- | ---------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| W1  | A commit elsewhere updates the list, the open artifact, and a preview that follows the latest. | Observed: after a publication the open view received an invalidation with the new version and reloaded its state.                                                                                                           |
| W2  | A preview pinned to a revision stays on it.                                                    | Source-only: the analyzed client ignores invalidations while a preview version is pinned.                                                                                                                                   |
| W3  | A notice for what is already shown, or older, reads nothing.                                   | Source-only: the analyzed client ignores an invalidation for the version it shows. Decision: the same for older revisions, which a resumed stream can repeat.                                                               |
| W4  | Only the user ends an unsaved draft.                                                           | Decision; the analysis found no editor with drafts to compare. Nothing the server sends, including a failed read, a reset, or an empty list, discards a draft; the page says what changed and acts only when the user asks. |
| W5  | Changes made while the page was not following are not missed.                                  | Decision, from 0006: subscribe, then read; resume from the cursor; read everything again after `reset`.                                                                                                                     |
| W6  | A hidden page neither holds a stream nor sends reads.                                          | Source-only: the analyzed client parks its live connection after a while hidden and defers reloads until shown. Decision: Artifree pauses at once.                                                                          |
| W7  | Following changes stays within the actor's request budget.                                     | Decision, from 0005: one read at a time per page, notices coalesce, and every `429` holds the page's reads for its `Retry-After`.                                                                                           |

## Decisions

1. **A gate under the client decides when each request is sent.** Each
   connected page creates one `RequestGate` and gives its `fetch` to the
   API client, so every HTTP request of the page passes through it,
   whichever view asked and however many requests a view's load makes. It
   checks its conditions just before sending, by what the request is:
   - **Reads** (every `GET` except the feed, and issuing a preview link)
     are sent one at a time, in the order asked for, only while the page is
     visible and no `Retry-After` delay is pending. A read holds its turn
     from sending until its answer has been read or cancelled. One that has
     run for ten seconds stops holding back the next, so a read that never
     answers cannot stop the page; this bound never ends a delay or sends a
     read from a hidden page.
   - **The stream** (`GET /v1/changes`) is sent once no delay is pending;
     the page decides when to open it (decision 3), and the client
     reconnects it on its own schedule, waiting out each refusal's
     `Retry-After`.
   - **Writes** are sent at once: the user started each one and sees its
     outcome.

   Any `429`, whatever its body (a proxy's included) and whichever kind of
   request it answered, delays every read of the page for its
   `Retry-After`, at least one second, because the limits of 0005 apply
   to the actor. A refused read was not processed, so the gate sends it
   again, first, after the delay; its caller sees only the final answer. A
   read whose view no longer wants it is withdrawn if it has not been sent,
   also when its turn has just come;
   a sent read is not cancelled, since the server is working on it, and
   keeps its turn until it ends while its caller stops waiting. The API
   client reports every `429` as `rate_limited`, also without an API body.

2. **When the page may read first.** The first list read waits until the
   stream starts with `ready`, so that no commit falls between the read and
   the subscription. It waits at most two seconds of visibility, because a
   browser out of HTTP/1.1 connections (about six per server, shared by all
   tabs) holds the stream's request back without failing it, and not at all
   once the stream was refused or lost.
3. **One subscription per visible, connected page.** Disconnecting aborts
   the subscription, which then delivers nothing more. A hidden page aborts
   its stream; shown again, it resumes after its last cursor, so changes
   made meanwhile arrive then. A failure that reconnecting cannot fix, such
   as a rejected token, stops following until the user connects again.
4. **What a notice changes.** `ready` and `reset` start a new epoch: the
   list and the open artifact are read again, and the revisions known so far
   are forgotten, since after a reset to another database they may be
   lower. A `change` counts only if its revision is newer than the page
   knows for that artifact, from a notice, a list answered for the current
   request, or the page's own save. It reads the list again, and the open
   artifact if it is newer than shown; the open artifact does not read
   again while its own read is pending, since that read may bring the
   revision. A new epoch also forgets the revision the open artifact last
   read for, so after a reset to older data the same number is news again,
   and what it read in an earlier epoch is behind whatever the new one
   knows, also when its first read after a reset failed.
   A change of a kind the client does not know counts whatever its
   revision: the list and, if it is open, the artifact are read again.
5. **Content follows its metadata.** A revision number read before a reset
   may name other content afterwards, for example in a restored database.
   File content and preview links are keyed by the epoch of the metadata
   they belong to: after a reset they are read once, for the revision the
   new metadata names.
6. **Failures keep what is shown.** A failed refresh of the list or of the
   open artifact keeps the earlier result and any draft, and shows the
   failure with "Try again". A first read that failed is retried by the
   user, not by later notices.
7. **The open artifact stays while selected.** (Amended by
   [0009](0009-web-list-and-metadata.md) decision 3: only a list of every
   artifact can close it.) Selection changes only when
   the user chooses another artifact, which is disabled while a draft
   exists, or, without a draft, when a list answered for the current request
   no longer contains it; then the first listed artifact opens. An artifact
   opened because it was listed first stays open in the same way, also when
   a later list puts another artifact first.
8. **Drafts.** While the editor holds unsaved changes it keeps the revision
   the draft was written against, with its file list, even when that
   revision is no longer listed. When a newer revision arrives, the editor
   says "Revision N was saved elsewhere", explains that saving will be
   refused, and offers "Load latest", which replaces the draft. When the
   server's data may have been replaced since the draft's revision was read
   (a `reset`, or a `ready` after the page read without a stream), it says
   so and disables saving: a save against a revision of the same number
   would succeed and overwrite content the user never saw. While this
   page's own save is in flight, or after it failed in a way that may have
   committed it (its answer was lost or unreadable, or the server failed), a
   newer revision may be that save, so no notice is shown then. A refused
   save committed nothing, so the notice stays.
9. **Previews and status.** The preview menu's default entry, "Latest (N)",
   follows each new revision; a numbered revision stays chosen until the
   user chooses again, also across the source tab. (Amended by
   [0011](0011-web-history-and-restore.md) decision 8: a possible
   replacement of the server's data forgets the choice.) A status line says
   whether live updates are connecting, on, reconnecting (changes made
   meanwhile follow), paused while the tab is hidden, or stopped (reconnect
   to try again).

## Rewrite after review

Three earlier implementations were closed after three review rounds each.
The first (pull request #14) added rules one finding at a time. The second
(#15) stated them, but decided in three places when a read could start. The
third (#16) moved every condition into one scheduler, yet its unit was a
view's load function rather than an HTTP request, and it recognized rate
limits by API error code: a load that made two requests sent the second
from a hidden page, a refused stream's `Retry-After` did not hold reads, and
a proxy's `429` was not recognized at all. Decision 1 places the gate where
requests are actually sent, classifies `429` by status in the client, and
unit tests drive the gate with a controlled clock, visibility, and server.

## Costs and limits

- A notice reloads the whole list and the open artifact's metadata and
  history; nothing is patched in place.
- One read at a time makes a page's loads sequential; on a local server
  each takes milliseconds. A large file read holds back the others until it
  has downloaded.
- Each tab has its own gate. Tabs of one actor share its limits but not
  their delays: each learns of a `429` from its own requests.
- More than about six visible tabs on one server still exhaust the
  browser's connections; the later ones read after two seconds and show
  "Connecting" until a connection frees up. The local server speaks
  HTTP/1.1.
- The browser checks stand in for a replaced database by rewriting read
  answers after a forged `reset`; no real backup is restored.
- The draft notice does not merge or compare the draft with the newer
  revision; comparing revisions is roadmap A04.
