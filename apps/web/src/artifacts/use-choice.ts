import { useState } from 'react';

/**
 * A choice among what the page shows, such as a revision number; undefined
 * until made. It is kept with the count of possible data replacements at
 * which what is shown was read, and forgotten once the page shows a later
 * read (decision 0011, item 8), since the same number can then name other
 * content.
 */
export function useChoice<T>(
  /** The count of possible replacements when what is shown was read. */
  resets: number,
): [T | undefined, (value: T | undefined) => void] {
  const [choice, setChoice] = useState<{
    readonly value: T | undefined;
    readonly resets: number;
  }>();
  return [
    choice?.resets === resets ? choice.value : undefined,
    (value) => setChoice({ value, resets }),
  ];
}
