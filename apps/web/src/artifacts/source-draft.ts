import { packageRules } from '@artifree/contracts';
import type { RevisionFile } from '@artifree/contracts';
import { fromEditable } from './editable-text';
import { explainPathError, mediaTypeOf, pathError } from './package-path';

/**
 * What a draft puts at a path (decision 0017, items 2 and 3).
 * - `text`: text from the editor, saved with the line breaks it was read
 *   with. `original` is the text of the path's own base file when the edit
 *   began from it; text that came from anywhere else has none.
 * - `bytes`: the exact bytes of base file `from`, which a rename moved
 *   here. The draft holds them, so saving reads nothing.
 */
export type Content =
  | {
      readonly kind: 'text';
      readonly text: string;
      readonly crlf: boolean;
      readonly original?: string;
    }
  | {
      readonly kind: 'bytes';
      readonly bytes: Uint8Array;
      readonly from: string;
    };

/** What a draft does to one path. */
export type Change =
  | { readonly kind: 'put'; readonly content: Content }
  | { readonly kind: 'delete' };

/**
 * Unsaved changes to the files of revision `base`. They are saved together,
 * as one revision against `base`, or not at all. Paths without a change
 * are as the base revision has them.
 */
export interface SourceDraft {
  readonly base: number;
  /** The base revision's manifest. */
  readonly files: readonly RevisionFile[];
  readonly changes: ReadonlyMap<string, Change>;
}

/** A path as the draft shows it. */
export interface DraftEntry {
  readonly path: string;
  readonly status: 'unchanged' | 'edited' | 'added' | 'deleted';
  /** The base file a rename brought here, if one did. */
  readonly movedFrom?: string;
  /** The `Content-Type` the file is served with. */
  readonly mediaType: string | undefined;
}

/** A reason the server would refuse to save the draft. */
export interface Problem {
  /** The path it concerns, if one. */
  readonly path?: string;
  readonly message: string;
}

/** A draft of `files` without changes. */
export function emptyDraft(
  base: number,
  files: readonly RevisionFile[],
): SourceDraft {
  return { base, files, changes: new Map() };
}

function inBase(draft: SourceDraft, path: string): RevisionFile | undefined {
  return draft.files.find((file) => file.path === path);
}

function withChange(
  draft: SourceDraft,
  path: string,
  change: Change | undefined,
): SourceDraft {
  const changes = new Map(draft.changes);
  if (change === undefined) changes.delete(path);
  else changes.set(path, change);
  return { ...draft, changes };
}

/** Whether the draft changes nothing. */
export function isEmpty(draft: SourceDraft): boolean {
  return draft.changes.size === 0;
}

/** Whether a file is at `path` in the draft as it stands. */
export function exists(draft: SourceDraft, path: string): boolean {
  const change = draft.changes.get(path);
  return change === undefined
    ? inBase(draft, path) !== undefined
    : change.kind === 'put';
}

/**
 * The editor holds `text` for `path`, to be saved with `crlf` line breaks.
 * `original` is the text of the path's own base file when the edit began
 * from it; text equal to it again is no change.
 */
export function edit(
  draft: SourceDraft,
  path: string,
  text: string,
  crlf: boolean,
  original: string | undefined,
): SourceDraft {
  if (original !== undefined && text === original)
    return withChange(draft, path, undefined);
  const content: Content =
    original === undefined
      ? { kind: 'text', text, crlf }
      : { kind: 'text', text, crlf, original };
  return withChange(draft, path, { kind: 'put', content });
}

/** Adds an empty text file at `path`. */
export function add(draft: SourceDraft, path: string): SourceDraft {
  return withChange(draft, path, {
    kind: 'put',
    content: { kind: 'text', text: '', crlf: false },
  });
}

/** Deletes a base file at `path`, or forgets a file the draft put there. */
export function remove(draft: SourceDraft, path: string): SourceDraft {
  return withChange(
    draft,
    path,
    inBase(draft, path) === undefined ? undefined : { kind: 'delete' },
  );
}

/** Undoes whatever the draft does to `path`. */
export function restore(draft: SourceDraft, path: string): SourceDraft {
  return withChange(draft, path, undefined);
}

/**
 * Whether moving `path` needs the bytes of its base file: it is unchanged,
 * so the draft does not hold its content yet.
 */
export function movingReads(draft: SourceDraft, path: string): boolean {
  return !draft.changes.has(path) && inBase(draft, path) !== undefined;
}

/**
 * Moves `from` to `to`, a path where no file is: `from` is removed, and
 * `to` holds the content `from` has in the draft. When `movingReads` says
 * so, `baseBytes` must be the bytes of `from` in the base revision. Moving
 * a base file's bytes back to its own path is no change; text moved
 * anywhere stays changed.
 */
export function rename(
  draft: SourceDraft,
  from: string,
  to: string,
  baseBytes: Uint8Array | undefined,
): SourceDraft {
  const change = draft.changes.get(from);
  let content: Content;
  if (change?.kind === 'put')
    content =
      change.content.kind === 'text'
        ? // Moved text is no longer its own base file's text.
          { kind: 'text', text: change.content.text, crlf: change.content.crlf }
        : change.content;
  else if (baseBytes === undefined)
    throw new Error(`Moving ${from} needs its bytes.`);
  else content = { kind: 'bytes', bytes: baseBytes, from };
  const moved = remove(draft, from);
  const home = content.kind === 'bytes' && content.from === to;
  return withChange(moved, to, home ? undefined : { kind: 'put', content });
}

/** Every path of the draft as it stands, deleted ones included, in order. */
export function entries(draft: SourceDraft): readonly DraftEntry[] {
  const paths = new Set([
    ...draft.files.map((file) => file.path),
    ...draft.changes.keys(),
  ]);
  return [...paths].sort(comparePaths).map((path): DraftEntry => {
    const change = draft.changes.get(path);
    const mediaType = mediaTypeOf(path);
    if (change === undefined) return { path, status: 'unchanged', mediaType };
    if (change.kind === 'delete') return { path, status: 'deleted', mediaType };
    const status = inBase(draft, path) === undefined ? 'added' : 'edited';
    return change.content.kind === 'bytes'
      ? { path, status, mediaType, movedFrom: change.content.from }
      : { path, status, mediaType };
  });
}

/** Orders paths as the server lists them: by their UTF-16 code units. */
function comparePaths(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

const encoder = new TextEncoder();

/** The bytes `content` is saved as. */
export function bytesOf(content: Content): Uint8Array {
  return content.kind === 'bytes'
    ? content.bytes
    : encoder.encode(fromEditable(content.text, content.crlf));
}

/**
 * Why the server would refuse to save the draft, as far as its rules can be
 * checked here (decision 0017, item 5). Paths the base revision has are not
 * judged again. The server checks every request regardless.
 */
export function problems(draft: SourceDraft): readonly Problem[] {
  const found: Problem[] = [];
  const present = entries(draft).filter(({ status }) => status !== 'deleted');
  const byFoldedPath = new Map<string, string>();
  let total = 0;
  for (const { path } of present) {
    if (inBase(draft, path) === undefined) {
      const error = pathError(path);
      if (error !== undefined)
        found.push({ path, message: `${path}: ${explainPathError(error)}` });
    }
    const folded = path.toLowerCase();
    const other = byFoldedPath.get(folded);
    if (other === undefined) byFoldedPath.set(folded, path);
    else
      found.push({
        path,
        message: `${other} and ${path} differ only by case.`,
      });
    const change = draft.changes.get(path);
    const size =
      change?.kind === 'put'
        ? bytesOf(change.content).length
        : (inBase(draft, path)?.size ?? 0);
    total += size;
    if (size > packageRules.max_file_bytes)
      found.push({
        path,
        message: `${path} is larger than ${packageRules.max_file_bytes} bytes.`,
      });
  }
  for (const { path } of present) {
    const inside = [...byFoldedPath.keys()].find((folded) =>
      folded.startsWith(`${path.toLowerCase()}/`),
    );
    if (inside !== undefined)
      found.push({
        path,
        message: `${path} is a file, so ${byFoldedPath.get(inside)} cannot be inside it.`,
      });
  }
  if (!present.some(({ path }) => path === packageRules.entry_path))
    found.push({
      message: `Every artifact needs ${packageRules.entry_path}, the page its preview opens.`,
    });
  if (present.length > packageRules.max_files)
    found.push({
      message: `An artifact has at most ${packageRules.max_files} files.`,
    });
  if (total > packageRules.max_package_bytes)
    found.push({
      message: `The files together are larger than ${packageRules.max_package_bytes} bytes.`,
    });
  return found;
}

/** One change of a save, with the bytes it puts. */
export type PlannedChange =
  | { readonly op: 'put'; readonly path: string; readonly bytes: Uint8Array }
  | { readonly op: 'delete'; readonly path: string };

/** The changes saving the draft sends, in path order. */
export function plan(draft: SourceDraft): readonly PlannedChange[] {
  return [...draft.changes.entries()]
    .sort(([a], [b]) => comparePaths(a, b))
    .map(([path, change]): PlannedChange =>
      change.kind === 'delete'
        ? { op: 'delete', path }
        : { op: 'put', path, bytes: bytesOf(change.content) },
    );
}
