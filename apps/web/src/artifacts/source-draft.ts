import type { RevisionFile } from '@artifree/contracts';
import { fromEditable } from './editable-text';
import {
  explainPathError,
  mediaTypeOf,
  pathError,
  rules,
} from './package-path';

/** What a draft holds at a path it changes (decision 0017, item 2). */
export type Change =
  | {
      readonly kind: 'text';
      /** The text as the editor shows it, with LF line breaks. */
      readonly text: string;
      /** Whether it is saved with CRLF line breaks. */
      readonly crlf: boolean;
      /**
       * The base file the text was read from, and its text then. At that
       * path, equal text is no change; a new file has none.
       */
      readonly source?: Source;
    }
  | {
      readonly kind: 'bytes';
      /** Bytes a rename moved here, read from the base revision. */
      readonly bytes: Uint8Array;
      /** The base path they were read from. */
      readonly from: string;
    }
  | { readonly kind: 'deleted' };

/** A base file and the text it had in the base revision. */
export interface Source {
  readonly path: string;
  readonly text: string;
}

/** Changes to the files of one revision, saved together or not at all. */
export interface SourceDraft {
  /** The revision the draft was written against. */
  readonly base: number;
  /** That revision's files. */
  readonly files: readonly RevisionFile[];
  /** What the draft does to each path it changes. */
  readonly changes: ReadonlyMap<string, Change>;
}

/** How a listed path differs from the base revision. */
export type PathState = 'unchanged' | 'edited' | 'added' | 'deleted';

/** A path the draft lists, with what it does to it. */
export interface DraftPath {
  readonly path: string;
  readonly state: PathState;
  /** What the draft holds there, or none for an unchanged base file. */
  readonly change: Change | undefined;
}

/** A reason the server would refuse the draft. */
export interface Problem {
  readonly path: string | undefined;
  readonly message: string;
}

/** One change of a save request, in the draft's terms. */
export type Planned =
  | { readonly op: 'put'; readonly path: string; readonly bytes: Uint8Array }
  | { readonly op: 'delete'; readonly path: string };

/** A draft of no changes to `files` of revision `base`. */
export function startDraft(
  base: number,
  files: readonly RevisionFile[],
): SourceDraft {
  return { base, files, changes: new Map() };
}

export function isEmpty(draft: SourceDraft): boolean {
  return draft.changes.size === 0;
}

function baseFile(draft: SourceDraft, path: string): RevisionFile | undefined {
  return draft.files.find((file) => file.path === path);
}

/** Whether the draft has a file at `path`. */
export function exists(draft: SourceDraft, path: string): boolean {
  const change = draft.changes.get(path);
  return change === undefined
    ? baseFile(draft, path) !== undefined
    : change.kind !== 'deleted';
}

/**
 * Whether a rename of `path` needs its bytes from the base revision: the
 * draft holds nothing for it yet (decision 0017, item 3).
 */
export function needsBaseBytes(draft: SourceDraft, path: string): boolean {
  return !draft.changes.has(path);
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

/**
 * Sets the text at `path`. `source` names the base file the shown text
 * was read from, unless the draft already holds text there, which keeps
 * its own. Text equal to its source at the source's own path is no change.
 */
export function setText(
  draft: SourceDraft,
  path: string,
  text: string,
  crlf: boolean,
  source: Source | undefined,
): SourceDraft {
  const held = draft.changes.get(path);
  const kept = held?.kind === 'text' ? held.source : source;
  const change: Change =
    kept === undefined
      ? { kind: 'text', text, crlf }
      : { kind: 'text', text, crlf, source: kept };
  return withChange(draft, path, isHome(change, path) ? undefined : change);
}

/** Whether `change` at `path` is what the base revision has there. */
function isHome(change: Change, path: string): boolean {
  if (change.kind === 'bytes') return change.from === path;
  return (
    change.kind === 'text' &&
    change.source?.path === path &&
    change.text === change.source.text
  );
}

/** Adds an empty text file at `path`, which must not exist. */
export function addFile(draft: SourceDraft, path: string): SourceDraft {
  return withChange(draft, path, { kind: 'text', text: '', crlf: false });
}

/** Removes the file at `path`; a base file is deleted, a new one dropped. */
export function deleteFile(draft: SourceDraft, path: string): SourceDraft {
  return withChange(
    draft,
    path,
    baseFile(draft, path) === undefined ? undefined : { kind: 'deleted' },
  );
}

/** Undoes the deletion of the base file at `path`. */
export function restoreFile(draft: SourceDraft, path: string): SourceDraft {
  return draft.changes.get(path)?.kind === 'deleted'
    ? withChange(draft, path, undefined)
    : draft;
}

/**
 * Moves what the draft holds at `from` to `to`, which must not exist.
 * `baseBytes` are `from`'s bytes in the base revision, needed when the
 * draft holds nothing for it. What moves keeps its source, so bytes or
 * unedited text moved back to their own path are no change.
 */
export function renameFile(
  draft: SourceDraft,
  from: string,
  to: string,
  baseBytes: Uint8Array | undefined,
): SourceDraft {
  const held = draft.changes.get(from);
  let moved: Change;
  if (held === undefined) {
    if (baseBytes === undefined)
      throw new Error(`The bytes of ${from} are needed to move it.`);
    moved = { kind: 'bytes', bytes: baseBytes, from };
  } else if (held.kind === 'deleted')
    throw new Error(`${from} is deleted in this draft.`);
  else moved = held;
  return withChange(
    deleteFile(draft, from),
    to,
    isHome(moved, to) ? undefined : moved,
  );
}

/** Every path the draft has or deletes, in path order. */
export function listPaths(draft: SourceDraft): DraftPath[] {
  const paths = new Set([
    ...draft.files.map((file) => file.path),
    ...draft.changes.keys(),
  ]);
  return [...paths].sort(byPath).map((path) => {
    const change = draft.changes.get(path);
    const inBase = baseFile(draft, path) !== undefined;
    const state: PathState =
      change === undefined
        ? 'unchanged'
        : change.kind === 'deleted'
          ? 'deleted'
          : inBase
            ? 'edited'
            : 'added';
    return { path, state, change };
  });
}

/** Paths in the order the server lists them: by UTF-16 code units. */
function byPath(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

const encoder = new TextEncoder();

function bytesOf(change: Change & { kind: 'text' | 'bytes' }): Uint8Array {
  return change.kind === 'bytes'
    ? change.bytes
    : encoder.encode(fromEditable(change.text, change.crlf));
}

/**
 * Every reason the server would refuse to save the draft (decision 0017,
 * item 6). Paths the base revision has are not judged again.
 */
export function problems(draft: SourceDraft): Problem[] {
  const found: Problem[] = [];
  const present: { path: string; size: number }[] = [];
  for (const { path, change } of listPaths(draft)) {
    if (change?.kind === 'deleted') continue;
    if (change === undefined) {
      present.push({ path, size: baseFile(draft, path)?.size ?? 0 });
      continue;
    }
    present.push({ path, size: bytesOf(change).length });
    const error =
      baseFile(draft, path) === undefined ? pathError(path) : undefined;
    if (error !== undefined)
      found.push({ path, message: `${path}: ${explainPathError(error)}` });
  }
  const folded = new Map<string, string>();
  for (const { path, size } of present) {
    const key = path.toLowerCase();
    const twin = folded.get(key);
    if (twin !== undefined)
      found.push({ path, message: `${path} and ${twin} differ only by case.` });
    folded.set(key, path);
    if (size > rules.max_file_bytes)
      found.push({
        path,
        message: `${path} is larger than ${mib(rules.max_file_bytes)}.`,
      });
  }
  for (const { path } of present) {
    const prefix = `${path.toLowerCase()}/`;
    const under = present.find((other) =>
      other.path.toLowerCase().startsWith(prefix),
    );
    if (under !== undefined)
      found.push({ path, message: `${under.path} is under the file ${path}.` });
  }
  if (!present.some(({ path }) => path === rules.entry_path))
    found.push({
      path: undefined,
      message: `A revision needs ${rules.entry_path}.`,
    });
  if (present.length > rules.max_files)
    found.push({
      path: undefined,
      message: `A revision has at most ${rules.max_files} files.`,
    });
  const total = present.reduce((sum, { size }) => sum + size, 0);
  if (total > rules.max_package_bytes)
    found.push({
      path: undefined,
      message: `The files together are larger than ${mib(rules.max_package_bytes)}.`,
    });
  return found;
}

function mib(bytes: number): string {
  return `${bytes / (1024 * 1024)} MiB`;
}

/** The changes a save sends, in path order (decision 0017, item 5). */
export function plan(draft: SourceDraft): Planned[] {
  return listPaths(draft).flatMap(({ path, change }): Planned[] => {
    if (change === undefined) return [];
    if (change.kind === 'deleted') return [{ op: 'delete', path }];
    return [{ op: 'put', path, bytes: bytesOf(change) }];
  });
}

/** The `Content-Type` of a listed path. */
export function mediaTypeAt(
  draft: SourceDraft,
  path: string,
): string | undefined {
  return baseFile(draft, path)?.media_type ?? mediaTypeOf(path);
}
