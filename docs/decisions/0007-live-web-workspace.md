# 0007: The web workspace follows committed changes

Date: 2026-10-28. Status: accepted. Builds on
[0006](0006-change-feed.md).

## Context

The change feed (decision 0006) tells clients what was committed. The web
workspace still showed what it read when it opened, so an agent's save, or a
save in another tab, appeared only after a manual reload. Reloading by
itself would be wrong in two places: a preview the user pinned to an older
revision, and a draft the user has not saved.

The requirements below use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                                    | Basis                                                                                                                                                                                                           |
| --- | ---------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| W1  | A commit elsewhere updates the list, the open artifact, and a preview that follows the latest. | Observed: after a publication the open view received an invalidation with the new version and reloaded its state.                                                                                               |
| W2  | A preview pinned to a revision stays on it.                                                    | Source-only: the analyzed client ignores invalidations while a preview version is pinned.                                                                                                                       |
| W3  | A notice for what is already shown, or older, changes nothing.                                 | Source-only: the analyzed client ignores an invalidation for the version it already shows. Decision: the same for older revisions, which a resumed stream can repeat.                                           |
| W4  | An unsaved draft is never replaced; its author learns that a newer revision exists.            | Decision: the analysis found no editor with drafts to compare. Replacing the draft would lose work, and saving it would be refused as stale, so the page says so and loads the latest revision only when asked. |
| W5  | Changes made while the page was not following are not missed.                                  | Decision, from 0006: subscribe, then read; resume from the cursor; read everything again after `reset`.                                                                                                         |

## Decisions

1. **One subscription per visible, connected page.** The workspace follows
   the feed with `followChanges` while it is connected. Disconnecting, or
   connecting with another token, aborts the subscription, which closes its
   stream; the old subscription delivers nothing more. A browser allows
   about six HTTP/1.1 connections per server, shared by all its tabs, and
   each open stream holds one, so a hidden page gives its stream up and
   resumes from its last cursor when shown again: changes made meanwhile
   arrive then, and nothing is read while hidden. The analyzed client parks
   its live connection after the page has been hidden for a while and
   defers reloads until it is shown (source-only); Artifree pauses at once,
   since resuming costs one request and reads only what changed.
2. **Subscribe, then read.** The first list read waits until the stream
   starts with `ready`, so no commit can fall between the read and the
   subscription and the page does not read twice. It waits at most two
   seconds: a browser out of connections holds the stream's request back
   without failing it, and the page must not stay empty meanwhile. If the
   stream cannot start in time, or at all (a refused stream, a server that
   is down, a rejected token), the page reads anyway and reads again when a
   stream starts.
3. **What a notice changes.** `ready` and `reset` mean that earlier changes
   are unknown: the list and the open artifact are read again, and the
   revisions known so far are forgotten, since after a reset to another
   database they may start lower. A `change` counts only if its revision is
   newer than what this page knows for that artifact, from a notice, its own
   save, or a list read; it then reloads the list, and the open artifact if
   the revision is newer than the one it shows. Duplicate and older notices
   read nothing. A kind unknown to the client counts as a change. If reading
   the newer revision fails, the page keeps showing what it had, including a
   draft, says that the refresh failed, and offers to try again.
4. **Previews.** The revision menu has a "Latest (N)" entry, the default,
   which follows each new revision. Choosing a numbered revision pins the
   preview to it until the user chooses again, also across a visit to the
   source tab.
5. **Drafts.** While the editor holds unsaved changes it keeps the revision
   the draft was written against. When a newer revision arrives, the editor
   says "Revision N was saved elsewhere", explains that saving will be
   refused, and offers "Load latest", which discards the draft. Navigation
   that would replace the editor stays disabled, as before. While this
   page's own save is in flight, or after it failed without an answer, a
   newer revision may be that save, so no notice is shown then.
6. **Status.** A status line says whether live updates are connecting, on,
   reconnecting (changes made meanwhile follow), paused while the tab is
   hidden, or stopped (reconnect to try again). Following stops only on failures that retrying cannot fix
   (0006, decision 10), such as a rejected token, which the page also
   reports in place of the list.

## Costs and limits

- A notice reloads the whole list and the open artifact's metadata and
  history; nothing is patched in place.
- More than about six visible tabs on one server still exhaust the
  browser's connections; the later ones read after two seconds and show
  "Connecting" until a connection frees up. HTTP/2 would lift the limit;
  the local server speaks HTTP/1.1.
- A page that becomes visible again resumes from its cursor, which reads
  only what changed; after a long time hidden that can be many events.
- Forgetting known revisions on `reset` is not exercised end to end: the
  smoke check forges a reset of the same database, whose revisions do not
  start lower.
- The draft notice does not merge or compare the draft with the newer
  revision; revision comparison is roadmap A04.
