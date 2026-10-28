# 0017: Editing an artifact's files in the web

Date: 2026-10-29. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md),
[0007](0007-live-web-workspace.md),
[0011](0011-web-history-and-restore.md), and
[0014](0014-web-trash.md). Amends 0007 decision 8.

## Context

The web edited one text file at a time and locked the file menu while a
draft existed. Adding, renaming, or changing two files in one revision
needed the CLI or an agent, although the API already commits a set of
`put` and `delete` changes against one base revision, all or nothing
(0003). Roadmap A09 adds a file tree, edits across files, adding,
renaming, and deleting files, preview placement, and checks of paths
before anything is sent, and chooses the editor.

The original product has no source editor for HTML artifacts. Its code
editors belong to other surfaces, so their behavior is evidence of
expectations, not a design to follow.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                           | Basis                                                                                                                                                                                                                                                   |
| --- | ------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C1  | Every file of a revision can be opened, and files can be added, renamed, and deleted. | Observed: the original's artifact viewer and its menu offer no source view or edit. Source-only: a desktop session editor listed a folder's files as a tree. Decision: decisions 2 and 3.                                                               |
| C2  | Changes to several files are saved as one revision against one base, or not at all.   | Observed: a two-file save was one version; a stale base, or one failed file condition, refused the whole save and created no version. Decision: the whole-revision condition of 0003, without per-file conditions (decision 5).                         |
| C3  | A renamed file keeps its bytes.                                                       | Source-only: file changes were `put` and `delete` only; there is no rename operation. Inference: a rename is a `put` of the new path and a `delete` of the old one in one save. Decision: decision 3.                                                   |
| C4  | Bytes the user did not change are saved unchanged.                                    | Source-only: the desktop editor kept CRLF line breaks; text was strict UTF-8 with a byte order mark kept. Decision: decision 7.                                                                                                                         |
| C5  | A path the server would refuse is reported before anything is sent.                   | Source-only: the original client checked path lengths but not case or `..`, and a desktop worker refused malformed paths. Decision: decision 6, with the server's rules as the single source.                                                           |
| C6  | A revision saved elsewhere does not overwrite a draft.                                | Source-only: the desktop editor refused a save over a changed file and offered to discard or override; a widget code panel kept unsaved edits when a build or remote update arrived, and warned. Not observed for HTML artifacts. Decision: decision 8. |
| C7  | A save whose answer was lost cannot commit twice, and is not mistaken for another's.  | Source-only: the original's file saves carried no idempotency key. Decision: decision 5.                                                                                                                                                                |
| C8  | A saved revision can be seen beside the editor.                                       | Observed: the original's viewer is one full frame. Source-only: a widget code panel sat beside its rendered widget, with a draft/published switch. Decision: decision 9.                                                                                |
| C9  | The editor is chosen in this change.                                                  | Source-only: the desktop session editor and the widget panel used CodeMirror 6; no code editor ships in the original's web viewer. Decision: decision 1.                                                                                                |

## Decisions

1. **The editor is the existing code textarea.** It adds no dependency,
   works with the browser's own undo, find, and assistive technology, and
   already round-trips bytes (decision 7). CodeMirror 6 would add
   highlighting and folding; it waits for a feature that needs a document
   model, such as inline comments or build diagnostics.
2. **A draft is a set of changes to its base revision.** Each path the
   draft touches holds edited text, bytes a rename moved there, or a
   deletion; every other path is as the base revision has it. The tree
   lists the draft's paths with what it does to each, and any of them can
   be opened while the draft grows. This amends 0007 decision 8, which
   locked the file menu; choosing another artifact and disconnecting stay
   locked. Text equal to its own base file's text is no change, and a
   draft without changes is no draft.
3. **A rename carries the bytes it moves.** Renaming `a` to `b` deletes
   `a` and puts at `b` what `a` holds in the draft. For a file the draft
   has not changed, the rename reads its bytes from the base revision
   when it is made, and the draft holds them; if they cannot be read,
   nothing moves and the dialog says why. Moving bytes back to their own
   path is no change. Edited text that moves keeps its text but not its
   original, since the file it replaces, if any, is another one.
4. **One reader per draft.** Every read for a draft, to show a file or to
   move one, names the draft's base revision. Once the page learns that
   the server's data may have been replaced since that revision was read
   (0011), the reader reads nothing, and an answer that arrives after it
   is dropped: the number may now name other content. The draft stays,
   saving is disabled, and what the draft holds can still be read and
   copied. Text the draft holds is shown without reading; a file the
   draft has not changed is read at its own path.
5. **One save, one request, nothing read on the way.** Saving sends every
   change, in path order, as one `POST /v1/artifacts/{id}/revisions` with
   the draft's base revision, through `ArtifactService` like every other
   write. The draft holds every byte it sends, so nothing is awaited
   between the click and the request. The same request sent again reuses
   its idempotency key (0007), so a retry after a lost answer replays.
   There are no per-file conditions: a draft written on an older revision
   is refused whole.
6. **Paths are checked with the server's rules.** `artifree-server
--package-rules` prints the domain's package limits and the media type
   of each extension; `npm run generate:contracts` writes them to
   `packageRules` in `@artifree/contracts`. The web checks a new path with
   them in the domain's order, and `contracts/fixtures/package-paths.json`
   holds cases a Rust test and a Node test must judge alike. A dialog
   refuses a path that is invalid, taken, or differs from a present path
   only by case. The draft lists every reason the server would refuse it,
   which disables saving: such paths, a file under a file, a missing
   `index.html`, too many files, and files or a package too large. Paths
   the base revision has are not judged again; the server checks every
   request regardless.
7. **Bytes.** A text file is shown when its bytes round-trip through a
   textarea: valid UTF-8, a byte order mark kept, and CRLF line breaks
   throughout or none. Text is saved with the line breaks it was read
   with. Other files are not shown but can be renamed and deleted, and
   moved bytes are saved exactly as read.
8. **Saves elsewhere leave the draft.** The rules of 0007 decision 8
   apply to the whole draft: a newer revision shows "Revision N was saved
   elsewhere", saving is refused as a conflict, and "Load latest"
   replaces the draft. A save whose answer was lost, or that failed in a
   way that may have committed it, may have made that newer revision; the
   draft remembers so until a save of it succeeds or the draft ends, also
   when a later attempt fails for another reason, and shows no such
   notice meanwhile. In the trash, where every read is refused (0014),
   the open file's text stays editable, but no file can be opened, added,
   moved, deleted, or restored, and no preview is shown. Carrying a draft
   onto a newer revision is A10's.
9. **The saved revision is previewed beside the editor.** "Show saved
   preview" puts the draft's base revision, in its sandboxed frame,
   beside the files. A draft is not previewed: previews are of pinned
   revisions (0003), and a temporary revision would show in history and
   the change feed.

## Rewrite after review

An earlier implementation (pull request #35) was closed after three
review rounds. Its four findings came from two rules it left implicit:

- **Who reads for a draft.** It showed files through one read and, to
  save renamed files cheaply, read their bytes again just before sending.
  Each read had its own lifetime and its own check for a replacement, or
  none. A replacement during the save's reads still sent the save; text
  moved onto a deleted path reread the file it replaced; and a held
  draft kept reading after a replacement. Decisions 3, 4, and 5 replace
  this: the draft holds what it moves, one reader serves every read and
  stops after a replacement, and saving reads nothing.
- **What a lost answer means later.** Whether a save may have committed
  was a property of one attempt, so a later failure erased it. Decision 8
  keeps it for the draft.

## Alternatives

- **CodeMirror 6.** Highlighting and folding, at the cost of a large
  dependency and its own undo, selection, and line-break handling, which
  would have to keep decision 7. No current feature needs it.
- **Reading moved bytes when the draft is saved**, as the earlier
  implementation did. It holds less in the page, but puts reads between
  the click and the request, where each needs the replacement rule and a
  failure confuses the save's outcome. A package is at most 25 MiB.
- **Per-file conditions**, as the original's optional file conditions.
  A save could then apply over another client's changes to other files,
  which history would not show as a conflict.
- **A rename operation in the API.** A rename is one `delete` and one
  `put` in a revision committed whole, so it would add no guarantee.
- **Checking paths only on the server.** A refused save would report one
  reason, after the draft was finished.

## Costs and limits

- No syntax highlighting, line numbers, or search across files.
- A draft lives in the page's memory, including the bytes it moved.
  Reloading the page or closing the tab loses it; keeping it is A10.
- The browser's checks mirror the domain's rules; the shared fixture and
  the generated rules keep them aligned, and the server remains the
  authority.
