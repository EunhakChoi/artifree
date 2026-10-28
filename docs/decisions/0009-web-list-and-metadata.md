# 0009: The web workspace searches, pages, renames, and pins

Date: 2026-10-28. Status: accepted. Builds on
[0007](0007-live-web-workspace.md) and
[0008](0008-artifact-metadata-and-lists.md); amends decision 7 of 0007.

## Context

Decision 0008 added renaming, pins, search, order, and pages to the API,
MCP, and CLI. The web workspace still listed the first 500 artifacts in
update order and could neither rename nor pin. This record covers roadmap
A03b: the same operations in the web workspace, while it keeps following
committed changes as 0007 describes.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                  | Basis                                                                                                                                                                                |
| --- | ---------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| L1  | The list can be searched, ordered, and filtered by pin.                      | Source-only: the analyzed client searches with a server-side query, orders by edit or by last opened, and filters favorites. Decision: the parameters of 0008.                       |
| L2  | Long lists are read in parts.                                                | Source-only: the analyzed gallery reads 48 at a time and continues with the server's page token. Decision: 50 at a time, read again from the start (decision 2).                     |
| L3  | Following changes keeps the list as long as it was.                          | Decision, from 0007 W1: a notice reads the list again; here, every artifact shown.                                                                                                   |
| L4  | Renaming and pinning add no revision and leave drafts and previews alone.    | Observed (0008 M1, M2): a pin answered without a body and added no version. Decision: both stay available while the editor holds a draft.                                            |
| L5  | A rename or pin is conditional and safe to retry from the page.              | Decision, from 0008 M3: each names its base metadata revision and carries an idempotency key; a retry is the same request.                                                           |
| L6  | An update that another client won changes nothing and shows the latest data. | Source-only: the analyzed client discards cached views after a rename and updates a pin before the server answers. Decision: the page shows what the server answered, never a guess. |

## Decisions

1. **One list query per page.** Search text, order (recently updated or
   recently created), and a pin filter (all, pinned, not pinned) form the
   query. Search waits until typing pauses for 300 ms and sends the trimmed
   text, so typing a word reads the list once. Changing the query starts
   again at the first 50 artifacts.
2. **The list is a prefix, read from the start.** The page shows the first
   N artifacts of the query, 50 at first; "Show more" adds 50. Each read,
   including the one a notice causes, reads those N again from the start,
   following the server's pages when N exceeds 500. Continuing an older
   page token after changes could skip or repeat artifacts when ordering by
   update time (0008 decision 4), so the token is used only within one read.
3. **Selection (amends 0007 decision 7).** Nothing chosen, the first listed
   artifact opens and stays open, as before. The open artifact closes
   without a draft only when a fresh list of every artifact (no search, no
   filter, nothing after the last one read) no longer contains it; then the
   first listed one opens. A search, a filter, or a list shorter than all
   artifacts does not close it, since its absence there says nothing about
   whether it exists.
4. **Pin and rename on the open artifact.** "Pin" is a toggle button; its
   pressed state is the artifact's pin. "Rename" opens a dialog with the
   title. Both send `PATCH` through the API client with the base metadata
   revision and an idempotency key; nothing is shown as changed before the
   server answers. Both stay available while the editor holds a draft,
   since neither touches the source.
5. **Retries and conflicts.** The rename dialog prepares its request
   against the artifact as it opened, so after a lost answer "Rename" sends
   the same request with the same key, even if the view has meanwhile read
   that the rename took effect; the server replays it. A
   `metadata_conflict` says that the title or pin changed elsewhere and
   nothing was changed, reads the artifact again, and prepares the next
   attempt against what it reads. Pin requests are prepared at each click
   from the state shown, the same way.
6. **This page's own updates.** The answer's version is learned as known
   (0007 decision 4), so the notice for it reads nothing more; learning it
   makes the open artifact read its metadata and history once, and the list
   is read once.

## Costs and limits

- A notice reads every artifact shown, up to N; with more than 500 shown,
  the reads are sequential through the page's request gate.
- The page size is fixed at 50.
- An artifact that disappears while a search or filter is active stays
  open until its own read fails; deleting artifacts is roadmap A06.
- No optimistic update: a pin shows its new state after the server
  answers, a few milliseconds locally.
- The browser checks forge one conflict and one lost answer; the
  server-side conflict and replay are covered by the API tests of 0008.
