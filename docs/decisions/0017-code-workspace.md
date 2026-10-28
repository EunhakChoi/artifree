# 0017: Editing an artifact's files in the web

Date: 2026-10-29. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md),
[0007](0007-live-web-workspace.md), and
[0011](0011-web-history-and-restore.md). Amends 0007 decision 8.

## Context

The web edited one text file at a time and locked the file menu while a
draft existed. Adding a file, renaming one, or changing two files in one
revision needed the CLI or an agent, although the API already commits a
set of `put` and `delete` changes against one base revision, all or
nothing (0003). Roadmap A09 adds a file tree, edits across files, adding,
renaming, and deleting files, preview placement, and checks of paths
before anything is sent, and chooses the editor.

The original product has no source editor for HTML artifacts. Its code
editors belong to other surfaces, so their behavior is evidence of
expectations, not a design to follow.

The requirements use the labels of [0004](0004-inbound-mcp-adapter.md):
**observed**, **source-only**, **inference**, and **decision**. A local,
unpublished trace maps each to its evidence.

## Requirements

| ID  | Requirement                                                                           | Basis                                                                                                                                                                                                                                                  |
| --- | ------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| C1  | Every file of a revision can be opened, and files can be added, renamed, and deleted. | Observed: the original's artifact viewer and its menu offer no source view or edit. Source-only: a desktop session editor listed a folder's files as a tree. Decision: the tree lists the draft's paths.                                               |
| C2  | Changes to several files are saved as one revision against one base, or not at all.   | Observed: a two-file save was one version; a stale base, or one failed file condition, refused the whole save and created no version. Decision: the whole-revision condition of 0003, without per-file conditions.                                     |
| C3  | A renamed file keeps its bytes.                                                       | Source-only: file changes were `put` and `delete` only; no rename operation exists. Inference: a rename is a `put` of the new path and a `delete` of the old in one save. Decision: decision 3.                                                        |
| C4  | Bytes the user did not change are saved unchanged.                                    | Source-only: the desktop editor kept CRLF line breaks, and text was strict UTF-8 with any byte order mark kept. Decision: the round-trip rule the web already used (0007), and binary files moved without being read into the page.                    |
| C5  | A path the server would refuse is reported before anything is sent.                   | Source-only: the original client checked path lengths but not case or `..`, and a desktop worker refused malformed paths. Decision: decision 5, with the server's rules as the single source.                                                          |
| C6  | A revision saved elsewhere does not overwrite a draft.                                | Source-only: the desktop editor refused a save over a changed file and offered to discard or override; a widget code panel kept unsaved edits when a build or remote update arrived and warned. Not observed for HTML artifacts. Decision: decision 7. |
| C7  | Retrying a save whose answer was lost cannot commit twice.                            | Source-only: the original's file saves carried no idempotency key. Decision: the key reuse of 0007.                                                                                                                                                    |
| C8  | A saved revision can be seen beside the editor.                                       | Observed: the original's viewer is one full frame. Source-only: a widget code panel sat beside its rendered widget, with a draft/published switch. Decision: decision 8.                                                                               |
| C9  | The editor is chosen in this change.                                                  | Source-only: the desktop session editor and the widget panel used CodeMirror 6; no code editor ships in the original's web viewer. Decision: decision 1.                                                                                               |

## Decisions

1. **The editor is the existing code textarea.** It needs no new
   dependency, works with the browser's own undo, find, spell-check
   control, and assistive technology, and already round-trips bytes
   (decision 6). CodeMirror 6 would add highlighting and folding; it is
   deferred until a feature needs a document model, such as inline
   comments or diagnostics from a build.
2. **A draft is a set of changes to its base revision.** Each path the
   draft touches holds new text, the bytes of a base file moved there, or
   a deletion; every other path is as the base revision has it. The tree
   lists the draft's paths with what the draft does to each, and any of
   them can be opened while the draft grows: this amends 0007 decision 8,
   which locked the file menu. Choosing another artifact and
   disconnecting stay locked while a draft exists. Text back at its
   original is no change, and a draft without changes is no draft.
3. **A rename moves bytes without reading them.** Renaming `a` to `b`
   deletes `a` and puts at `b` what `a` holds in the draft: its edited
   text, or a reference to the base file, read only when the draft is
   saved. The base revision is pinned and never changes, so the bytes
   read then are those the draft was written against, unless the
   server's data was replaced meanwhile. If one cannot be read, or if the
   page learns of a possible replacement (0011) while they are read,
   nothing is sent, and the draft says why. Moving a file back to its base path is no
   change.
4. **One save, one request.** Saving sends every change, in path order,
   as one `POST /v1/artifacts/{id}/revisions` with the draft's base
   revision, through `ArtifactService` like every other write. Sending
   the same request again reuses its idempotency key (0007), so a retry
   after a lost answer replays the commit. There are no per-file
   conditions: a draft written against an older revision is refused
   whole.
5. **Paths are checked with the server's rules.** `artifree-server
--package-rules` prints the domain's package limits and the media type
   of each extension; `npm run generate:contracts` writes them to
   `packageRules` in `@artifree/contracts`. The web checks each new path
   with those rules, in the domain's order, and
   `contracts/fixtures/package-paths.json` lists cases that a Rust test
   and a Node test must both judge alike. A dialog refuses a path that is
   invalid, taken, or differs from a present path only by case. The draft
   lists the reasons the server would refuse it, which disable saving:
   such paths, a file whose path is a directory of another, a missing
   `index.html`, too many files, and files or a package too large. Paths
   the base revision already has are not judged again. The server checks
   every request regardless.
6. **Bytes.** A file of a text type is shown when its bytes round-trip
   through a textarea: valid UTF-8, any byte order mark kept, and CRLF
   line breaks throughout or none. Saved text is encoded back with the
   line breaks it was read with. Other files, and text that cannot
   round-trip, are not shown but can be renamed and deleted; moved bytes
   are sent exactly as read.
7. **Saves elsewhere leave the draft.** The rules of 0007 decision 8
   apply to the whole draft: a newer revision shows "Revision N was saved
   elsewhere", saving it is refused as a conflict, and "Load latest"
   replaces it. A possible replacement of the server's data disables
   saving (0011). In the trash, where every read is refused (0014), the
   open file's text stays editable, but no file can be opened, added,
   moved, deleted, or restored, and no preview is shown: most of these
   read, and the paths stay as they were when the artifact left. Carrying a draft onto a newer revision is A10's.
8. **The saved revision is previewed beside the editor.** "Show saved
   preview" puts the draft's base revision, in its sandboxed frame,
   beside the files. A draft is not previewed: previews are of pinned
   revisions (0003), and a temporary revision would show in history and
   the change feed.

## Alternatives

- **CodeMirror 6.** Highlighting and folding, at the cost of a large
  dependency, its own undo and selection model, and line-break handling
  that must be kept in line with decision 6. Not needed by any current
  feature.
- **Per-file conditions**, as the original's optional file conditions.
  A save could then apply over another client's changes to other files,
  which the history would not show as a conflict. The API has one
  condition per revision; adding another is outside this change.
- **A rename operation in the API.** Moving bytes server-side would avoid
  sending them again, but a rename is one `delete` and one `put` in a
  revision that is committed whole, so it adds no guarantee.
- **Reading moved bytes when the rename is made.** The page would hold
  every moved file, up to the package limit, for as long as the draft
  lives.
- **Checking paths only on the server.** A refused save would report one
  reason, after the user finished the draft.

## Costs and limits

- No syntax highlighting, line numbers, or search across files.
- A draft lives in the page's memory. Reloading the page or closing the
  tab loses it; keeping it is A10.
- A moved file is read whole when the draft is saved.
- The browser's checks mirror the domain's rules; the shared fixture and
  generated limits keep them aligned, and the server remains the
  authority.
