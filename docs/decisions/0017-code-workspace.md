# 0017: Editing an artifact's files in the web

Date: 2026-10-29. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md),
[0007](0007-live-web-workspace.md),
[0011](0011-web-history-and-restore.md), and
[0014](0014-web-trash.md). Amends 0007 decision 8.

## Context

The web edits one text file at a time and locks the file menu while a
draft exists. Adding a file, renaming one, or changing two in one revision
needs the CLI or an agent, although the API already commits a set of `put`
and `delete` changes against one base revision, all or nothing (0003).
Roadmap A09 adds a file tree, edits across files, adding, renaming, and
deleting files, the preview's placement, checks of paths before anything
is sent, and the choice of editor.

The original product has no source editor for HTML artifacts. Its code
editors belong to other surfaces, so their behavior is evidence of what
users expect, not a design to copy. The requirements use the labels of
[0004](0004-inbound-mcp-adapter.md): **observed**, **source-only**,
**inference**, and **decision**. A local, unpublished trace maps each to
its evidence.

## Requirements

| ID  | Requirement                                                                            | Basis                                                                                                                                                                                                                         |
| --- | -------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C1  | Every file of a revision can be opened; files can be added, renamed, and deleted.      | Observed: the original's artifact viewer and its menu offer no source view or edit. Source-only: a desktop session editor listed a folder's files as a tree. Decision: items 2 and 3.                                         |
| C2  | Changes to several files are saved as one revision against one base, or not at all.    | Observed: a two-file save was one version; a stale base, or one failed file condition, refused the whole save and created no version. Decision: 0003's whole-revision condition without per-file conditions (item 6).         |
| C3  | A renamed file keeps its bytes.                                                        | Source-only: file changes were `put` and `delete` only; there is no rename operation. Inference: a rename is a `put` and a `delete` in one save. Decision: item 3.                                                            |
| C4  | Bytes the user did not change are saved unchanged.                                     | Source-only: the desktop editor kept CRLF line breaks, and text was strict UTF-8 with a byte order mark kept. Decision: item 8.                                                                                               |
| C5  | A path the server would refuse is reported before anything is sent.                    | Source-only: the original client checked path lengths but not case or `..`, and a desktop worker refused malformed paths. Decision: item 7, with the server's rules as the one source.                                        |
| C6  | A revision saved elsewhere, or a change of the server's state, does not alter a draft. | Source-only: the desktop editor refused a save over a changed file and offered to discard or override; a widget code panel kept unsaved edits when a build or remote update arrived, and warned. Decision: items 4, 5, and 9. |
| C7  | A save whose answer was lost cannot commit twice and is not mistaken for another's.    | Source-only: the original's file saves carried no idempotency key. Decision: items 6 and 9.                                                                                                                                   |
| C8  | A saved revision can be seen beside the editor.                                        | Observed: the original's viewer is one full frame. Source-only: a widget code panel sat beside its rendered widget. Decision: item 10.                                                                                        |
| C9  | The editor is chosen in this change.                                                   | Source-only: the desktop session editor and the widget panel used CodeMirror 6; the original's web viewer ships no code editor. Decision: item 1.                                                                             |

## Decisions

1. **The editor is the existing code textarea.** It adds no dependency,
   keeps the browser's own undo, find, and assistive technology, and
   already round-trips bytes (item 8). CodeMirror 6 would add highlighting
   and folding; it waits for a feature that needs a document model, such
   as inline comments or build diagnostics.
2. **A draft is a set of changes to its base revision.** Each path the
   draft changes holds text, bytes a rename moved there, or a deletion;
   every other path is as the base revision has it. The tree lists every
   path with what the draft does to it, and any file can be opened while
   the draft grows. This amends 0007 decision 8, which locked the file
   menu; choosing another artifact and disconnecting stay locked. Text
   equal to its own base file's text is no change, and a draft without
   changes is no draft.
3. **A rename carries the bytes it moves.** Renaming `a` to `b` deletes
   `a` and puts at `b` what `a` holds in the draft. For a file the draft
   has not changed, the rename reads its bytes from the base revision when
   it is made, and the draft holds them, so saving reads nothing. Bytes
   moved back to their own path are no change. Edited text that moves
   keeps its text but not its original, since a base file at `b` is
   another file. Moved bytes that are text can be edited where they are.
4. **One write point decides every change.** Each change states what it
   touches, the text of a file or the draft's paths, and the revision it
   was made on. The write point applies it to the draft as last written,
   or refuses it with the reason, judging by the state it lands in rather
   than the one it started in:

   | The page shows                                         | Allowed                      |
   | ------------------------------------------------------ | ---------------------------- |
   | Nothing below                                          | Every change                 |
   | The artifact in the trash, or the data maybe replaced  | Text of files the page holds |
   | A save of the draft being sent, or a newer one loading | Nothing                      |

   A change made on another revision than the draft's is refused. Controls
   mirror the same state: a dialog opened earlier stays open, says why
   paths cannot change, and cannot be submitted, and a rename whose read
   answers later is refused where it lands. What the page shows is the
   last rendered state, so a change that lands before the page shows the
   trash was made before it, as for the user.

5. **Reads name a revision the number still names.** A file of the base
   revision is read only while the page shows neither the trash, where
   every read is refused (0014), nor a possible replacement of the
   server's data (0011). An answer that settles after the feed's epoch
   changed is refused: the feed changes it as soon as a `reset` arrives,
   before the page renders it. A file's text read once stays shown while
   it is open, also after edits return to it, so nothing is read again.
6. **One save, one request, nothing read on the way.** Saving sends every
   change, in path order, as one `POST /v1/artifacts/{id}/revisions` with
   the draft's base revision, through `ArtifactService` like every other
   write. The same request sent again reuses its idempotency key (0007),
   so a retry after a lost answer replays. There are no per-file
   conditions: a draft on an older revision is refused whole. A save
   allowed by the write point is sent; while it is in flight nothing
   changes the draft, and an answer that arrives in a later feed epoch
   changes nothing, as for every write of the page (0014).
7. **Paths are checked with the server's rules.** The build inventory
   (`artifree-server --describe`) carries `package_rules`: the domain's
   package limits and the media type of each extension, generated into
   `scaffoldDescription` by `npm run generate:contracts`; the contract
   version becomes 0.6.0. The web checks a new path with them in the
   domain's order, and `contracts/fixtures/package-paths.json` lists
   paths that a Rust test and a Node test must judge alike. A dialog
   refuses a path that is invalid, taken, or differs from a present path
   only by case. The draft lists every reason the server would refuse it,
   which disables saving: such paths, case twins, a file under a file, a
   missing `index.html`, too many files, and files or a package too
   large. Paths the base revision has are not judged again; the server
   checks every request regardless.
8. **Bytes.** A text file is shown when its bytes round-trip through a
   textarea: valid UTF-8, a byte order mark kept, and CRLF line breaks
   throughout or none. Text is saved with the line breaks it was read
   with. Other files are not shown but can be renamed and deleted, and
   moved bytes are saved exactly as read.
9. **Saves elsewhere leave the draft.** The rules of 0007 decision 8 apply
   to the whole draft: a newer revision shows "Revision N was saved
   elsewhere", saving is refused as a conflict, and "Load latest" replaces
   the draft. A save that failed in a way that may have committed it may
   have made that newer revision; the draft keeps that until a save of it
   succeeds or the draft ends, also when a later attempt is refused for
   another reason, and shows no such notice meanwhile. Carrying a draft
   onto a newer revision, and keeping it across reloads, is A10's.
10. **The saved revision is previewed beside the editor.** "Show saved
    preview" puts the draft's base revision, in its sandboxed frame,
    beside the files, unless the page shows the trash or a possible
    replacement. A draft is not previewed: previews are of pinned
    revisions (0003), and a temporary revision would show in history and
    the change feed.

## Rewrites after review

Two earlier implementations (pull requests #35 and #36) were closed after
three review rounds each. Six of their eight findings had one cause:
whether the draft may change was decided where each control or
continuation happened to be. A button's `disabled`, a reader's epoch
check, an abort in an effect, and a check after a save's await each held
part of the policy, so every new path, such as a dialog left open or a
rename that reads nothing, was a path the policy did not reach. Item 4
replaces them with one write point and one table; the controls only
mirror it. The other two findings, a save that read files on the way and
a lost save forgotten after a later failure, are items 3, 6, and 9.

## Alternatives

- **CodeMirror 6.** Highlighting and folding, at the cost of a large
  dependency and its own undo, selection, and line-break handling, which
  would have to keep item 8. No current feature needs it.
- **Closing dialogs when paths cannot change.** It hides why a submit did
  nothing, and a read already answered still lands; the write point has
  to refuse it anyway.
- **Reading moved bytes when the draft is saved.** It holds less in the
  page, but puts reads between the click and the request, where each needs
  the replacement rule and a failure confuses the save's outcome. A
  package is at most 25 MiB.
- **Per-file conditions**, like the original's optional file conditions.
  A save could then apply over another client's change to other files,
  which the whole-revision condition of 0003 rules out.
- **A separate command for the package rules.** The build inventory
  already describes what this build supports, is generated and checked,
  and reaches the web as `scaffoldDescription`.

## Consequences

- A draft, with the bytes it moved, lives in the page's memory until A10.
- There is no syntax highlighting.
- The browser's path checks mirror the domain; the server stays the
  authority.
