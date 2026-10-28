/**
 * Comparison of two revisions (decision 0011): files by digest, and the
 * lines of one text file by the shortest edit script. Plain functions, so
 * they are tested without a browser.
 */
import type { RevisionFile } from '@artifree/contracts';

/** How one path differs between two revisions. */
export interface FileDifference {
  readonly path: string;
  readonly kind: 'added' | 'removed' | 'changed';
  /** The file in the earlier revision, unless it was added. */
  readonly before: RevisionFile | undefined;
  /** The file in the later revision, unless it was removed. */
  readonly after: RevisionFile | undefined;
}

/** The files that differ, in path order, and how many do not. */
export interface ManifestComparison {
  readonly differences: readonly FileDifference[];
  readonly unchanged: number;
}

/** Compares two manifests by path and digest; no file is read. */
export function compareManifests(
  before: readonly RevisionFile[],
  after: readonly RevisionFile[],
): ManifestComparison {
  const earlier = new Map(before.map((file) => [file.path, file]));
  const later = new Map(after.map((file) => [file.path, file]));
  const paths = [...new Set([...earlier.keys(), ...later.keys()])].sort(
    (left, right) => (left < right ? -1 : left > right ? 1 : 0),
  );
  const differences: FileDifference[] = [];
  let unchanged = 0;
  for (const path of paths) {
    const old = earlier.get(path);
    const next = later.get(path);
    if (old !== undefined && next !== undefined && old.sha256 === next.sha256)
      unchanged += 1;
    else
      differences.push({
        path,
        kind:
          old === undefined
            ? 'added'
            : next === undefined
              ? 'removed'
              : 'changed',
        before: old,
        after: next,
      });
  }
  return { differences, unchanged };
}

/**
 * Splits text into lines, each with its line feed, so that a missing final
 * line feed is a difference too. A carriage return stays in its line.
 */
export function splitLines(text: string): string[] {
  const lines: string[] = [];
  let start = 0;
  for (;;) {
    const end = text.indexOf('\n', start);
    if (end === -1) break;
    lines.push(text.slice(start, end + 1));
    start = end + 1;
  }
  if (start < text.length) lines.push(text.slice(start));
  return lines;
}

/** One line of a comparison, with its 1-based number on each side. */
export interface LineChange {
  readonly kind: 'same' | 'removed' | 'added';
  readonly text: string;
  readonly before: number | undefined;
  readonly after: number | undefined;
}

/** Beyond this many lines on a side, only sizes are compared. */
export const MAX_LINES = 20_000;
/** Beyond this many changed lines, only sizes are compared. */
export const MAX_EDITS = 2_000;

/**
 * The lines of `before` and `after` as a shortest edit script (Myers, "An
 * O(ND) difference algorithm"), or undefined past {@link MAX_LINES} or
 * {@link MAX_EDITS}. Lines common to both ends are matched first.
 */
export function diffLines(
  before: readonly string[],
  after: readonly string[],
): LineChange[] | undefined {
  if (before.length > MAX_LINES || after.length > MAX_LINES) return undefined;
  let head = 0;
  while (
    head < before.length &&
    head < after.length &&
    before[head] === after[head]
  )
    head += 1;
  let tail = 0;
  while (
    tail < before.length - head &&
    tail < after.length - head &&
    before[before.length - 1 - tail] === after[after.length - 1 - tail]
  )
    tail += 1;
  const middle = shortestEdits(
    before.slice(head, before.length - tail),
    after.slice(head, after.length - tail),
  );
  if (middle === undefined) return undefined;
  const steps: Step[] = [
    ...Array.from({ length: head }, (): Step => 'same'),
    ...middle,
    ...Array.from({ length: tail }, (): Step => 'same'),
  ];
  const lines: LineChange[] = [];
  let old = 0;
  let next = 0;
  for (const step of steps) {
    if (step === 'same') {
      lines.push({
        kind: 'same',
        text: after[next] ?? '',
        before: old + 1,
        after: next + 1,
      });
      old += 1;
      next += 1;
    } else if (step === 'removed') {
      lines.push({
        kind: 'removed',
        text: before[old] ?? '',
        before: old + 1,
        after: undefined,
      });
      old += 1;
    } else {
      lines.push({
        kind: 'added',
        text: after[next] ?? '',
        before: undefined,
        after: next + 1,
      });
      next += 1;
    }
  }
  return lines;
}

type Step = LineChange['kind'];

/**
 * The steps that turn `a` into `b` with the fewest removed and added lines,
 * or undefined when that takes more than {@link MAX_EDITS}. Each round `d`
 * keeps, per diagonal `k = x - y`, the furthest `x` reached with `d` edits;
 * only the band `-d..d` is stored, so memory grows with the edits, not
 * with the lines.
 */
function shortestEdits(
  a: readonly string[],
  b: readonly string[],
): Step[] | undefined {
  const n = a.length;
  const m = b.length;
  const limit = Math.min(n + m, MAX_EDITS);
  const offset = limit + 1;
  const furthest = new Int32Array(2 * limit + 3);
  const rounds: Int32Array[] = [];
  for (let d = 0; d <= limit; d += 1) {
    for (let k = -d; k <= d; k += 2) {
      const left = entry(furthest, offset + k - 1);
      const right = entry(furthest, offset + k + 1);
      const down = k === -d || (k !== d && left < right);
      let x = down ? right : left + 1;
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x += 1;
        y += 1;
      }
      furthest[offset + k] = x;
      if (x >= n && y >= m) {
        rounds.push(furthest.slice(offset - d, offset + d + 1));
        return backtrack(rounds, n, m);
      }
    }
    rounds.push(furthest.slice(offset - d, offset + d + 1));
  }
  return undefined;
}

/**
 * An entry of `values`. Every index read here is in range; the typed
 * arrays start at 0, the furthest point before any edit.
 */
function entry(values: Int32Array, index: number): number {
  return values[index] ?? 0;
}

/** Walks the stored rounds back from the end to the start. */
function backtrack(
  rounds: readonly Int32Array[],
  n: number,
  m: number,
): Step[] {
  const steps: Step[] = [];
  let x = n;
  let y = m;
  for (let d = rounds.length - 1; d > 0; d -= 1) {
    const previous = rounds[d - 1] ?? new Int32Array();
    // Round d - 1 stores diagonals -(d - 1)..(d - 1) from index 0.
    const at = (diagonal: number) => entry(previous, diagonal + d - 1);
    const k = x - y;
    const down = k === -d || (k !== d && at(k - 1) < at(k + 1));
    const startK = down ? k + 1 : k - 1;
    const startX = at(startK);
    const startY = startX - startK;
    while (x > startX + (down ? 0 : 1) && y > startY + (down ? 1 : 0)) {
      steps.push('same');
      x -= 1;
      y -= 1;
    }
    steps.push(down ? 'added' : 'removed');
    x = startX;
    y = startY;
  }
  while (x > 0 && y > 0) {
    steps.push('same');
    x -= 1;
    y -= 1;
  }
  return steps.reverse();
}

/** A run of changed lines with up to `context` unchanged lines around it. */
export type Hunk = readonly LineChange[];

/** Groups a comparison into hunks; unchanged lines between them are left out. */
export function hunks(lines: readonly LineChange[], context = 3): Hunk[] {
  const shown = new Uint8Array(lines.length);
  lines.forEach((line, index) => {
    if (line.kind === 'same') return;
    const from = Math.max(0, index - context);
    const to = Math.min(lines.length - 1, index + context);
    shown.fill(1, from, to + 1);
  });
  const groups: Hunk[] = [];
  let current: LineChange[] = [];
  lines.forEach((line, index) => {
    if (shown[index] === 1) current.push(line);
    else if (current.length > 0) {
      groups.push(current);
      current = [];
    }
  });
  if (current.length > 0) groups.push(current);
  return groups;
}
