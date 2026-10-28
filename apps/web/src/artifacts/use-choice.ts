import { useState } from 'react';

/**
 * A choice among what the page shows, such as a revision number; undefined
 * until made. It belongs to the read it was made from: once the page shows
 * a read made after a possible replacement of the server's data, it is
 * forgotten (decision 0011, item 8), since the same number can then name
 * other content.
 */
export function useChoice<T>(
  /** The count of possible replacements when what is shown was read. */
  shownResets: number,
): [T | undefined, (value: T | undefined) => void] {
  const [choice, setChoice] = useState<{
    readonly value: T | undefined;
    readonly resets: number;
  }>();
  return [
    choice?.resets === shownResets ? choice.value : undefined,
    (value) => setChoice({ value, resets: shownResets }),
  ];
}
