import type { Revision, RevisionFile } from '@artifree/contracts';

/** How a file differs between two revisions. */
export type FileChangeKind = 'added' | 'removed' | 'changed';

/** A file that differs between two revisions. */
export interface FileDifference {
  readonly path: string;
  readonly kind: FileChangeKind;
  /** The file in the earlier revision, unless it was added. */
  readonly before?: RevisionFile;
  /** The file in the later revision, unless it was removed. */
  readonly after?: RevisionFile;
}

/**
 * The files that differ between two manifests, in path order, and how many
 * are the same. Equal digests mean equal bytes (decision 0010), so no file
 * is read to compare them.
 */
export function compareManifests(
  before: Revision,
  after: Revision,
): { readonly differences: FileDifference[]; readonly unchanged: number } {
  const earlier = new Map(before.files.map((file) => [file.path, file]));
  const later = new Map(after.files.map((file) => [file.path, file]));
  const paths = [...new Set([...earlier.keys(), ...later.keys()])].sort();
  const differences: FileDifference[] = [];
  let unchanged = 0;
  for (const path of paths) {
    const old = earlier.get(path);
    const now = later.get(path);
    if (old === undefined && now !== undefined)
      differences.push({ path, kind: 'added', after: now });
    else if (now === undefined && old !== undefined)
      differences.push({ path, kind: 'removed', before: old });
    else if (old !== undefined && now !== undefined) {
      if (old.sha256 === now.sha256) unchanged += 1;
      else differences.push({ path, kind: 'changed', before: old, after: now });
    }
  }
  return { differences, unchanged };
}

/** One line of a comparison, with its line numbers on each side. */
export type DiffLine =
  | {
      readonly kind: 'same';
      readonly text: string;
      readonly before: number;
      readonly after: number;
    }
  | { readonly kind: 'removed'; readonly text: string; readonly before: number }
  | { readonly kind: 'added'; readonly text: string; readonly after: number };

/** Consecutive lines around changes, as a unified diff shows them. */
export interface DiffHunk {
  readonly lines: readonly DiffLine[];
}

/** Largest number of lines compared on each side. */
export const MAX_LINES = 20_000;
/** Largest number of added and removed lines a comparison looks for. */
export const MAX_EDITS = 2_000;

/**
 * Splits text into lines at `\n`. A final line ending ends the last line
 * instead of starting an empty one; a carriage return stays in its line, so
 * a change of line endings is a change of lines.
 */
export function splitLines(text: string): string[] {
  if (text === '') return [];
  const lines = text.split('\n');
  if (lines.at(-1) === '') lines.pop();
  return lines;
}

/**
 * The shortest sequence of removed and added lines that turns `before` into
 * `after` (Myers' O(ND) algorithm), or `undefined` when either side has more
 * than `MAX_LINES` lines or more than `MAX_EDITS` lines differ.
 */
export function diffLines(
  before: readonly string[],
  after: readonly string[],
): DiffLine[] | undefined {
  if (before.length > MAX_LINES || after.length > MAX_LINES) return undefined;
  const total = before.length + after.length;
  const limit = Math.min(total, MAX_EDITS);
  const offset = limit + 1;
  // furthest[k + offset]: the furthest position in `before` reached on
  // diagonal k (position in `before` minus position in `after`).
  let furthest = new Int32Array(2 * limit + 3);
  const trace: Int32Array[] = [];
  for (let edits = 0; edits <= limit; edits += 1) {
    trace.push(furthest);
    furthest = furthest.slice();
    for (let k = -edits; k <= edits; k += 2) {
      let x = fromAbove(furthest, k, edits, offset)
        ? (furthest[k + 1 + offset] ?? 0)
        : (furthest[k - 1 + offset] ?? 0) + 1;
      let y = x - k;
      while (x < before.length && y < after.length && before[x] === after[y]) {
        x += 1;
        y += 1;
      }
      furthest[k + offset] = x;
      if (x >= before.length && y >= after.length)
        return path(before, after, trace, offset);
    }
  }
  return undefined;
}

/** Whether diagonal k is reached from k + 1 (an added line). */
function fromAbove(
  furthest: Int32Array,
  k: number,
  edits: number,
  offset: number,
): boolean {
  return (
    k === -edits ||
    (k !== edits &&
      (furthest[k - 1 + offset] ?? 0) < (furthest[k + 1 + offset] ?? 0))
  );
}

/** Walks the trace back from the end into lines. */
function path(
  before: readonly string[],
  after: readonly string[],
  trace: readonly Int32Array[],
  offset: number,
): DiffLine[] {
  const lines: DiffLine[] = [];
  let x = before.length;
  let y = after.length;
  for (let edits = trace.length - 1; edits >= 0; edits -= 1) {
    const furthest = trace[edits] ?? new Int32Array(0);
    const k = x - y;
    const previousK = fromAbove(furthest, k, edits, offset) ? k + 1 : k - 1;
    const previousX = furthest[previousK + offset] ?? 0;
    const previousY = previousX - previousK;
    while (x > previousX && y > previousY) {
      x -= 1;
      y -= 1;
      lines.push({
        kind: 'same',
        text: before[x] ?? '',
        before: x + 1,
        after: y + 1,
      });
    }
    if (edits === 0) break;
    if (x === previousX) {
      y -= 1;
      lines.push({ kind: 'added', text: after[y] ?? '', after: y + 1 });
    } else {
      x -= 1;
      lines.push({ kind: 'removed', text: before[x] ?? '', before: x + 1 });
    }
  }
  return lines.reverse();
}

/**
 * Groups changed lines with up to `context` unchanged lines around them;
 * changes closer than twice the context share a hunk.
 */
export function hunks(lines: readonly DiffLine[], context = 3): DiffHunk[] {
  const changed = lines.flatMap((line, index) =>
    line.kind === 'same' ? [] : [index],
  );
  const groups: DiffHunk[] = [];
  let start = -1;
  let end = -1;
  for (const index of changed) {
    if (start !== -1 && index - context <= end + context + 1) {
      end = index;
      continue;
    }
    if (start !== -1)
      groups.push({ lines: lines.slice(start, end + context + 1) });
    start = Math.max(0, index - context);
    end = index;
  }
  if (start !== -1)
    groups.push({ lines: lines.slice(start, end + context + 1) });
  return groups;
}
