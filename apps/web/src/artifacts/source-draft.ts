import { packageRules } from '@artifree/contracts';
import type { RevisionFile } from '@artifree/contracts';
import { fromEditable } from './editable-text';
import { describePathError, mediaTypeOf, pathError } from './package-path';

/**
 * New content of a path: text from the editor, saved with the line breaks
 * the file was read with, or the unchanged bytes of a file of the base
 * revision, moved by a rename and read only when the draft is saved
 * (decision 0017, item 3).
 */
export type Content =
  | { readonly kind: 'text'; readonly text: string; readonly crlf: boolean }
  | { readonly kind: 'copy'; readonly from: string };

/** What a draft does to one path. */
export type Change =
  | { readonly kind: 'put'; readonly content: Content }
  | { readonly kind: 'delete' };

/**
 * Unsaved changes to the files of one revision (decision 0017). Every
 * change is saved together, against `base`, or not at all.
 */
export interface SourceDraft {
  readonly base: number;
  /** The base revision's files, as its manifest lists them. */
  readonly files: readonly RevisionFile[];
  readonly changes: ReadonlyMap<string, Change>;
}

/** A path as the draft shows it. */
export interface DraftEntry {
  readonly path: string;
  readonly status: 'unchanged' | 'edited' | 'added' | 'deleted';
  /** For a file a rename brought here, where its bytes came from. */
  readonly movedFrom?: string;
  /** The type the file is served with. */
  readonly mediaType: string | undefined;
}

/** A reason the server would refuse the draft. */
export interface Problem {
  /** The path it concerns, if one. */
  readonly path?: string;
  readonly message: string;
}

export function emptyDraft(
  base: number,
  files: readonly RevisionFile[],
): SourceDraft {
  return { base, files, changes: new Map() };
}

function baseFile(draft: SourceDraft, path: string): RevisionFile | undefined {
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

/** Whether the draft changes anything. */
export function isEmpty(draft: SourceDraft): boolean {
  return draft.changes.size === 0;
}

/** Whether `path` names a file in the draft as it stands. */
export function exists(draft: SourceDraft, path: string): boolean {
  const change = draft.changes.get(path);
  return change === undefined
    ? baseFile(draft, path) !== undefined
    : change.kind === 'put';
}

/**
 * The text of `path` in the editor is now `text`. `original` is the text and
 * line breaks the file had in the base revision, if it was there; the
 * change disappears when the text is the original again.
 */
export function edit(
  draft: SourceDraft,
  path: string,
  text: string,
  original: { readonly text: string; readonly crlf: boolean } | undefined,
  crlf: boolean,
): SourceDraft {
  const unchanged =
    original !== undefined &&
    original.text === text &&
    baseFile(draft, path) !== undefined;
  return withChange(
    draft,
    path,
    unchanged
      ? undefined
      : { kind: 'put', content: { kind: 'text', text, crlf } },
  );
}

/** Adds an empty text file at `path`. */
export function add(draft: SourceDraft, path: string): SourceDraft {
  return withChange(draft, path, {
    kind: 'put',
    content: { kind: 'text', text: '', crlf: false },
  });
}

/**
 * Removes `path`: a file of the base revision is deleted, and a file the
 * draft added is forgotten.
 */
export function remove(draft: SourceDraft, path: string): SourceDraft {
  return withChange(
    draft,
    path,
    baseFile(draft, path) === undefined ? undefined : { kind: 'delete' },
  );
}

/** Undoes every change to `path`. */
export function restore(draft: SourceDraft, path: string): SourceDraft {
  return withChange(draft, path, undefined);
}

/**
 * Moves `from` to `to`, which must not exist: `from` is removed and `to`
 * gets its content as it stands, edited or not. A file moved back to where
 * the base revision had it is unchanged again.
 */
export function rename(
  draft: SourceDraft,
  from: string,
  to: string,
): SourceDraft {
  const change = draft.changes.get(from);
  const content: Content =
    change?.kind === 'put' ? change.content : { kind: 'copy', from };
  const moved = remove(draft, from);
  const back = content.kind === 'copy' && content.from === to;
  return withChange(moved, to, back ? undefined : { kind: 'put', content });
}

/** Every path of the draft as it stands, deleted ones included, in order. */
export function entries(draft: SourceDraft): readonly DraftEntry[] {
  const paths = new Set([
    ...draft.files.map((file) => file.path),
    ...draft.changes.keys(),
  ]);
  return [...paths].sort().map((path): DraftEntry => {
    const change = draft.changes.get(path);
    const inBase = baseFile(draft, path) !== undefined;
    const mediaType = mediaTypeOf(path);
    if (change === undefined) return { path, status: 'unchanged', mediaType };
    if (change.kind === 'delete') return { path, status: 'deleted', mediaType };
    const movedFrom =
      change.content.kind === 'copy' ? change.content.from : undefined;
    return {
      path,
      status: inBase ? 'edited' : 'added',
      mediaType,
      ...(movedFrom === undefined ? {} : { movedFrom }),
    };
  });
}

const encoder = new TextEncoder();

/** The bytes `content` saves as, when known without reading. */
function textBytes(content: Content): Uint8Array | undefined {
  return content.kind === 'text'
    ? encoder.encode(fromEditable(content.text, content.crlf))
    : undefined;
}

function size(draft: SourceDraft, path: string, change: Change | undefined) {
  if (change === undefined) return baseFile(draft, path)?.size ?? 0;
  if (change.kind === 'delete') return 0;
  const { content } = change;
  return content.kind === 'copy'
    ? (baseFile(draft, content.from)?.size ?? 0)
    : (textBytes(content)?.length ?? 0);
}

/**
 * Why the server would refuse to save the draft, as far as the browser can
 * tell (decision 0017, item 5). The server checks every request again.
 */
export function problems(draft: SourceDraft): readonly Problem[] {
  const found: Problem[] = [];
  const present = entries(draft).filter(({ status }) => status !== 'deleted');
  for (const { path } of present) {
    if (baseFile(draft, path) !== undefined) continue;
    const error = pathError(path);
    if (error !== undefined)
      found.push({ path, message: describePathError(error) });
  }
  const folded = new Map<string, string>();
  for (const { path } of present) {
    const other = folded.get(path.toLowerCase());
    if (other !== undefined)
      found.push({
        path,
        message: `${path} and ${other} differ only by case.`,
      });
    else folded.set(path.toLowerCase(), path);
  }
  for (const { path } of present) {
    const directory = `${path.toLowerCase()}/`;
    const inside = present.find(({ path: other }) =>
      other.toLowerCase().startsWith(directory),
    );
    if (inside !== undefined)
      found.push({
        path,
        message: `${path} is a file, so ${inside.path} cannot be inside it.`,
      });
  }
  if (!present.some(({ path }) => path === packageRules.entry_path))
    found.push({
      message: `Every artifact needs ${packageRules.entry_path}, the page a preview opens.`,
    });
  if (present.length > packageRules.max_files)
    found.push({
      message: `An artifact has at most ${packageRules.max_files} files.`,
    });
  let total = 0;
  for (const { path } of present) {
    const bytes = size(draft, path, draft.changes.get(path));
    total += bytes;
    if (bytes > packageRules.max_file_bytes)
      found.push({
        path,
        message: `${path} is larger than ${packageRules.max_file_bytes} bytes.`,
      });
  }
  if (total > packageRules.max_package_bytes)
    found.push({
      message: `The files together are larger than ${packageRules.max_package_bytes} bytes.`,
    });
  return found;
}

/** One change of a save request, before moved bytes are read. */
export type PlannedChange =
  | { readonly op: 'put'; readonly path: string; readonly bytes: Uint8Array }
  | { readonly op: 'put'; readonly path: string; readonly copyOf: string }
  | { readonly op: 'delete'; readonly path: string };

/** The changes the draft saves, in path order. */
export function plan(draft: SourceDraft): readonly PlannedChange[] {
  return [...draft.changes.entries()]
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([path, change]): PlannedChange => {
      if (change.kind === 'delete') return { op: 'delete', path };
      const { content } = change;
      return content.kind === 'copy'
        ? { op: 'put', path, copyOf: content.from }
        : {
            op: 'put',
            path,
            bytes: encoder.encode(fromEditable(content.text, content.crlf)),
          };
    });
}
