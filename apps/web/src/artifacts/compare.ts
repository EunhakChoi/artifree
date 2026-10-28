/**
 * Comparing two revisions (decision 0011): which files differ, by digest,
 * and how one text file's lines differ. Plain code without React or the
 * DOM, so it is tested on its own.
 */
import type { RevisionFile } from '@artifree/contracts';

/** A file that differs between two revisions. */
export interface FileChange {
  readonly path: string;
  readonly kind: 'added' | 'removed' | 'changed';
  /** The file in the earlier revision, unless it was added. */
  readonly before: RevisionFile | undefined;
  /** The file in the later revision, unless it was removed. */
  readonly after: RevisionFile | undefined;
}

/**
 * The files that differ from `before` to `after`, in path order, and how
 * many are the same. Files are compared by digest, so nothing is read.
 */
export function changedFiles(
  before: readonly RevisionFile[],
  after: readonly RevisionFile[],
): { readonly changes: FileChange[]; readonly unchanged: number } {
  const earlier = new Map(before.map((file) => [file.path, file]));
  const later = new Map(after.map((file) => [file.path, file]));
  const paths = [...new Set([...earlier.keys(), ...later.keys()])].sort(
    (a, b) => (a < b ? -1 : a > b ? 1 : 0),
  );
  const changes: FileChange[] = [];
  let unchanged = 0;
  for (const path of paths) {
    const old = earlier.get(path);
    const next = later.get(path);
    if (old !== undefined && next !== undefined && old.sha256 === next.sha256)
      unchanged += 1;
    else
      changes.push({
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
  return { changes, unchanged };
}

/**
 * A text's lines, each with its line feed, so that a carriage return, a
 * byte order mark, or a missing final line feed is part of a line and
 * shows as a difference.
 */
export function toLines(text: string): string[] {
  const lines: string[] = [];
  let start = 0;
  while (start < text.length) {
    const end = text.indexOf('\n', start);
    const stop = end === -1 ? text.length : end + 1;
    lines.push(text.slice(start, stop));
    start = stop;
  }
  return lines;
}

/** One line of a comparison, with its number on each side it is on. */
export interface Line {
  readonly kind: 'same' | 'added' | 'removed';
  readonly text: string;
  /** Its number in the earlier text, unless it was added. */
  readonly old: number | undefined;
  /** Its number in the later text, unless it was removed. */
  readonly new: number | undefined;
}

/** Texts with more lines on either side are not compared line by line. */
export const MAX_LINES = 20_000;
/** Comparisons needing more added and removed lines are not shown. */
export const MAX_EDITS = 2_000;

/**
 * The lines of `after` compared with those of `before`, with the fewest
 * added and removed lines (Myers's O(ND) algorithm). Undefined when either
 * side has more than {@link MAX_LINES} lines or more than
 * {@link MAX_EDITS} lines differ, which the caller reports by size.
 */
export function compareLines(
  before: readonly string[],
  after: readonly string[],
): Line[] | undefined {
  if (before.length > MAX_LINES || after.length > MAX_LINES) return undefined;
  // Lines both ends share need no search.
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
  const middle = editScript(
    before.slice(head, before.length - tail),
    after.slice(head, after.length - tail),
  );
  if (middle === undefined) return undefined;
  const lines: Line[] = [];
  let old = 1;
  let next = 1;
  const same = (text: string): void => {
    lines.push({ kind: 'same', text, old, new: next });
    old += 1;
    next += 1;
  };
  for (const text of before.slice(0, head)) same(text);
  for (const step of middle) {
    if (step.kind === 'same') same(step.text);
    else if (step.kind === 'removed') {
      lines.push({ kind: 'removed', text: step.text, old, new: undefined });
      old += 1;
    } else {
      lines.push({ kind: 'added', text: step.text, old: undefined, new: next });
      next += 1;
    }
  }
  for (const text of before.slice(before.length - tail)) same(text);
  return lines;
}

interface Step {
  readonly kind: Line['kind'];
  readonly text: string;
}

/**
 * The shortest edit script from `a` to `b`, or undefined beyond
 * {@link MAX_EDITS}. Each round `d` keeps only the diagonals it reached,
 * `-d..d`, so memory grows with the square of the edits, not with the
 * texts.
 */
function editScript(
  a: readonly string[],
  b: readonly string[],
): Step[] | undefined {
  const n = a.length;
  const m = b.length;
  // rounds[d][k + d]: the furthest x on diagonal k = x - y after d edits.
  const rounds: Int32Array[] = [];
  let previous = Int32Array.of(0);
  for (let d = 0; d <= Math.min(n + m, MAX_EDITS); d += 1) {
    const reached = new Int32Array(2 * d + 1);
    for (let k = -d; k <= d; k += 2) {
      // Down (an added line) from diagonal k + 1, or right (a removed line)
      // from k - 1, whichever went further.
      const fromAbove =
        d > 0 &&
        (k === -d ||
          (k !== d && at(previous, d - 1, k - 1) < at(previous, d - 1, k + 1)));
      let x =
        d === 0
          ? 0
          : fromAbove
            ? at(previous, d - 1, k + 1)
            : at(previous, d - 1, k - 1) + 1;
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x += 1;
        y += 1;
      }
      reached[k + d] = x;
      if (x >= n && y >= m) {
        rounds.push(reached);
        return walkBack(a, b, rounds);
      }
    }
    rounds.push(reached);
    previous = reached;
  }
  return undefined;
}

/** The furthest x on diagonal `k` in a round of `d` edits. */
function at(round: Int32Array, d: number, k: number): number {
  const x = round[k + d];
  if (x === undefined)
    throw new RangeError(`No diagonal ${k} after ${d} edits.`);
  return x;
}

/** Rebuilds the script by walking the rounds from the end to the start. */
function walkBack(
  a: readonly string[],
  b: readonly string[],
  rounds: readonly Int32Array[],
): Step[] {
  const steps: Step[] = [];
  let x = a.length;
  let y = b.length;
  for (let d = rounds.length - 1; d > 0; d -= 1) {
    const k = x - y;
    const previous = rounds[d - 1];
    if (previous === undefined) throw new RangeError(`No round ${d - 1}.`);
    const fromAbove =
      k === -d ||
      (k !== d && at(previous, d - 1, k - 1) < at(previous, d - 1, k + 1));
    const priorK = fromAbove ? k + 1 : k - 1;
    const priorX = at(previous, d - 1, priorK);
    const priorY = priorX - priorK;
    // The run of equal lines after the edit, then the edit itself.
    while (
      x > priorX + (fromAbove ? 0 : 1) &&
      y > priorY + (fromAbove ? 1 : 0)
    ) {
      x -= 1;
      y -= 1;
      steps.push({ kind: 'same', text: line(a, x) });
    }
    if (fromAbove) {
      y -= 1;
      steps.push({ kind: 'added', text: line(b, y) });
    } else {
      x -= 1;
      steps.push({ kind: 'removed', text: line(a, x) });
    }
  }
  // Round 0 is a run of equal lines from the start.
  while (x > 0 && y > 0) {
    x -= 1;
    y -= 1;
    steps.push({ kind: 'same', text: line(a, x) });
  }
  return steps.reverse();
}

function line(lines: readonly string[], index: number): string {
  const text = lines[index];
  if (text === undefined) throw new RangeError(`No line ${index}.`);
  return text;
}

/** What a comparison shows: lines near a change, and gaps for the rest. */
export type Excerpt =
  | { readonly kind: 'line'; readonly line: Line }
  | {
      readonly kind: 'gap';
      /** How many unchanged lines are left out here. */
      readonly count: number;
    };

/**
 * The changed lines of a comparison with `context` unchanged lines around
 * each, and a gap wherever unchanged lines are left out: before the first
 * shown line, between two, and after the last.
 */
export function excerpt(lines: readonly Line[], context = 3): Excerpt[] {
  const shown = new Uint8Array(lines.length);
  lines.forEach((item, index) => {
    if (item.kind !== 'same')
      shown.fill(
        1,
        Math.max(0, index - context),
        Math.min(lines.length, index + context + 1),
      );
  });
  const result: Excerpt[] = [];
  let skipped = 0;
  lines.forEach((item, index) => {
    if (shown[index] === 1) {
      if (skipped > 0) result.push({ kind: 'gap', count: skipped });
      skipped = 0;
      result.push({ kind: 'line', line: item });
    } else skipped += 1;
  });
  if (skipped > 0) result.push({ kind: 'gap', count: skipped });
  return result;
}
