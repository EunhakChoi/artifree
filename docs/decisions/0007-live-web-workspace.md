# 0007: The web workspace follows committed changes

Date: 2026-10-28. Status: accepted. Builds on
[0005](0005-request-limits.md) and [0006](0006-change-feed.md).

## Context

The change feed (decision 0006) tells clients what was committed. The web
workspace still showed what it read when it opened, so a save by an agent
or in another tab appeared only after a manual reload. Reloading on every
notice would be wrong in three places: a preview pinned to an older
revision, a draft the user has not saved, and the request budget the page
shares with every other client of the same actor.

The requirements below use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                                    | Basis                                                                                                                                                                                                                          |
| --- | ---------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| W1  | A commit elsewhere updates the list, the open artifact, and a preview that follows the latest. | Observed: after a publication the open view received an invalidation with the new version and reloaded its state.                                                                                                              |
| W2  | A preview pinned to a revision stays on it.                                                    | Source-only: the analyzed client ignores invalidations while a preview version is pinned.                                                                                                                                      |
| W3  | A notice for what is already shown, or older, reads nothing.                                   | Source-only: the analyzed client ignores an invalidation for the version it already shows. Decision: the same for older revisions, which a resumed stream can repeat.                                                          |
| W4  | Only the user ends an unsaved draft.                                                           | Decision: the analysis found no editor with drafts to compare. Nothing the server sends, including a failed read, a reset, or an empty list, may discard a draft; the page says what changed and acts only when the user asks. |
| W5  | Changes made while the page was not following are not missed.                                  | Decision, from 0006: subscribe, then read; resume from the cursor; read everything again after `reset`.                                                                                                                        |
| W6  | A hidden page neither holds a stream nor starts reads.                                         | Source-only: the analyzed client parks its live connection after a while hidden and defers reloads until shown. Decision: Artifree pauses at once.                                                                             |
| W7  | Following changes stays within the actor's request budget.                                     | Decision, from 0005: a burst of notices costs at most one read per view, and a `429` is waited out.                                                                                                                            |

## Decisions

1. **One read policy, in one hook.** `useLiveWorkspace` follows the feed and
   decides when the page may read; components only say what they show. A
   hidden page starts no reads: notices stop with its stream, and a page
   opened in the background reads first when shown. Reads already asked
   for, including one waiting out a `429`, still complete. The first list
   read waits until the stream starts with `ready`, so no commit falls
   between the read and the subscription and the page does not read twice;
   it waits at most two seconds of visibility, because a browser out of HTTP/1.1 connections
   (about six per server, shared by all tabs) holds the stream's request
   back without failing it. A stream that fails or is refused does not
   delay the read. When a stream starts later, the page reads again.
2. **One subscription per visible, connected page.** Disconnecting, or
   connecting with another token, aborts the subscription; the old one
   delivers nothing more. A hidden page aborts its stream and, when shown,
   resumes from its last cursor, so changes made meanwhile arrive then.
3. **What a notice changes.** `ready` and `reset` mean that earlier changes
   are unknown: the list and the open artifact are read again, and the
   revisions known so far are forgotten, since after a reset to another
   database they may be lower. A `change` counts only if its revision is
   newer than the page knows for that artifact: from a notice, its own
   save, or a list read answered for the current request (a list shown
   while a newer read is pending may predate a reset). It then reloads the
   list, and the open artifact if the revision is newer than the one shown.
   A kind unknown to the client counts as a change.
4. **Request budget.** Each view (the list, the open artifact, the preview,
   the editor's file) sends one read at a time. A read asked for while
   another is in flight waits for it, at most ten seconds; reads replaced
   meanwhile are never sent. A read answered `429` was not processed: the
   view sends it again after `Retry-After` and keeps showing what it had.
5. **Failures keep what is shown.** A failed refresh of the list or of the
   open artifact keeps the earlier result and its draft, and shows the
   failure with "Try again" above it.
6. **The open artifact stays while selected.** The workspace shows the open
   artifact whenever one is selected, whatever the list holds. Selection
   changes only when the user chooses another artifact, which is disabled
   while a draft exists, or, without a draft, when a list read for the
   current request no longer contains the selected artifact; then the first
   listed artifact opens.
7. **Content and drafts.** A revision number read before a reset may name
   other content afterwards, for example in a restored database, so file
   content and previews are read again in each epoch. While the editor
   holds unsaved changes it keeps the revision the draft was written
   against, with its file list, even when that revision no longer exists.
   When a newer revision arrives, the editor says "Revision N was saved
   elsewhere", explains that saving will be refused, and offers "Load
   latest", which discards the draft. After a reset, or after a stream
   that starts with `ready` only when the page had already read without
   one (nothing ties those reads to the feed), it says that the server's
   data may have been replaced and disables saving, because a save against
   a revision of the same number would succeed and overwrite content the
   user never saw. While this page's own save is in flight, or after its
   response was lost, a newer revision may be that save, so no notice is
   shown then.
8. **Previews.** The revision menu has a "Latest (N)" entry, the default,
   which follows each new revision. Choosing a numbered revision pins the
   preview until the user chooses again, also across the source tab.
9. **Status.** A status line says whether live updates are connecting, on,
   reconnecting (changes made meanwhile follow), paused while the tab is
   hidden, or stopped (reconnect to try again). Following stops only on
   failures that retrying cannot fix (0006, decision 10), such as a
   rejected token, which the page also reports in place of the list.

## Rewrite after review

The first implementation (closed pull request #14) added these rules one
review finding at a time. Its reviews found that a failed list refresh
dropped the list, that a reset to older data or to an empty list could
unmount a draft, that notices could spend the request budget, and that a
page opened in the background still read. Two rules were implicit:
whether a draft survived depended on what the list and history happened
to contain, and when the page read was decided in several places.
Decisions 1, 4, and 6 state those rules, and the code keeps each in one
place. The rewrite's first review found that file content and previews were
still reused after a reset; decision 7 now reads them again and disables
saving a draft written before the reset.

## Costs and limits

- A notice reloads the whole list and the open artifact's metadata and
  history; nothing is patched in place.
- More than about six visible tabs on one server still exhaust the
  browser's connections; the later ones read after two seconds and show
  "Connecting" until a connection frees up. The local server speaks
  HTTP/1.1.
- The smoke check stands in for a replaced database by rewriting read
  responses after a forged `reset`; no real backup is restored.
- A read that keeps being refused with `429` keeps the page on what it
  showed; the ten-second bound on waiting for an earlier read is not
  exercised by a test.
- The draft notice does not merge or compare the draft with the newer
  revision; revision comparison is roadmap A04.
