# 0007: The web workspace follows committed changes

Date: 2026-10-28. Status: accepted. Builds on
[0005](0005-request-limits.md) and [0006](0006-change-feed.md).

## Context

The change feed (decision 0006) tells clients what was committed, but the
web workspace showed what it read when it opened: a save by an agent or in
another tab appeared only after a manual reload. Reloading on every notice
would be wrong in three places: a preview pinned to an older revision, a
draft the user has not saved, and the request budget the page shares with
every other client of the same actor.

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
| W6  | A hidden page neither holds a stream nor starts reads.                                         | Source-only: the analyzed client parks its live connection after a while hidden and defers reloads until shown. Decision: Artifree pauses at once.                                                                          |
| W7  | Following changes stays within the actor's request budget.                                     | Decision, from 0005: one read at a time per page, notices coalesce, and a `429` holds the page's reads for its `Retry-After`.                                                                                               |

## Decisions

1. **One scheduler starts every read.** Each connected page has one
   `ReadScheduler`, and all of its reads go through it: lists, artifacts,
   revisions, files, and preview links. It starts a read only when three
   conditions hold, and it checks them at the moment the read would start,
   not when it was asked for:
   - the page is visible;
   - no `Retry-After` delay is pending. A `429` on any read holds every read
     of the page, because the limits of 0005 apply to the actor, and the
     refused read is sent again afterwards;
   - no other read is running, unless that one has run for ten seconds, so a
     read that never answers cannot stop the page. This bound never ends a
     `Retry-After` delay or starts a read on a hidden page.

   Reads start in the order they were asked for. A read whose view no
   longer wants it, because its inputs changed or the view went away, is
   cancelled and never starts; one that started runs to its end and its
   result is ignored. Saves do not pass through the scheduler: the user
   starts each one and sees its outcome.

2. **When the page may read first.** The first list read waits until the
   stream starts with `ready`, so that no commit falls between the read and
   the subscription. It waits at most two seconds of visibility, because a
   browser out of HTTP/1.1 connections (about six per server, shared by all
   tabs) holds the stream's request back without failing it, and not at all
   once the stream was refused or lost.
3. **One subscription per visible, connected page.** Disconnecting aborts the
   subscription, which then delivers nothing more. A hidden page aborts its
   stream; shown again, it resumes from its last cursor, so changes made
   meanwhile arrive then.
4. **What a notice changes.** `ready` and `reset` mean that earlier reads are
   not tied to the feed: the list and the open artifact are read again, and
   the revisions known so far are forgotten, since after a reset to another
   database they may be lower. A `change` counts only if its revision is
   newer than the page knows for that artifact: from a notice, its own save,
   or a list answered for the current request. It reloads the list, and the
   open artifact if newer than shown. The open artifact does not read again
   while its own read is pending, since that read may bring the revision.
   A kind unknown to the client counts as a change.
5. **Content follows its metadata.** A revision number read before a reset
   may name other content afterwards, for example in a restored database.
   File content and preview links are read again in each epoch, keyed by the
   epoch of the metadata they belong to: after a reset they are read once,
   for the revision the new metadata names.
6. **Failures keep what is shown.** A failed refresh of the list or of the
   open artifact keeps the earlier result and any draft, and shows the
   failure with "Try again".
7. **The open artifact stays while selected.** Selection changes only when
   the user chooses another artifact, which is disabled while a draft
   exists, or, without a draft, when a list answered for the current request
   no longer contains it; then the first listed artifact opens.
8. **Drafts.** While the editor holds unsaved changes it keeps the revision
   the draft was written against, with its file list, even when that
   revision is no longer listed. When a newer revision arrives, the editor
   says "Revision N was saved elsewhere", explains that saving will be
   refused, and offers "Load latest", which replaces the draft. When the
   server's data may have been replaced since the draft's revision was read
   (a reset, or a `ready` after the page already read without a stream), it
   says so and disables saving: a save against a revision of the same
   number would succeed and overwrite content the user never saw. While this
   page's own save is in flight, or after its response was lost, a newer
   revision may be that save, so no notice is shown then.
9. **Previews and status.** The preview menu's default entry, "Latest (N)",
   follows each new revision; a numbered revision stays chosen until the
   user chooses again, also across the source tab. A status line says
   whether live updates are connecting, on, reconnecting (changes made
   meanwhile follow), paused while the tab is hidden, or stopped (reconnect
   to try again).

## Rewrite after review

The first implementation (closed pull request #14) added rules one review
finding at a time, and whether a draft survived depended on what the list
and history happened to contain. Its rewrite (closed pull request #15)
stated those rules, but when a read could start was still decided in three
places: visibility when a view asked, a `Retry-After` wait inside each
view's chain of reads, and a timeout on that chain. Its reviews found the
gaps between them: a read for a new key that ignored a pending
`Retry-After`, follow-on reads that started while hidden, a read queued
before the page was hidden that started while hidden, and a `Retry-After`
longer than the chain's timeout that was cut short. Decision 1 puts all
three conditions in one place, and unit tests drive it with a controlled
clock and visibility.

## Costs and limits

- A notice reloads the whole list and the open artifact's metadata and
  history; nothing is patched in place.
- One read at a time makes a page's loads sequential; on a local server
  each takes milliseconds.
- More than about six visible tabs on one server still exhaust the
  browser's connections; the later ones read after two seconds and show
  "Connecting" until a connection frees up. The local server speaks
  HTTP/1.1.
- The browser checks stand in for a replaced database by rewriting read
  responses after a forged `reset`; no real backup is restored.
- A save made on this page can read the list and the artifact twice when its
  own notice arrives before its response.
- The draft notice does not merge or compare the draft with the newer
  revision; comparing revisions is roadmap A04.
