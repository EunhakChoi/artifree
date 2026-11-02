/**
 * What the workspace tells the user about each runtime capability it can
 * grant (decisions 0020 and 0032). The words are what the capability allows,
 * not what its name suggests.
 */
export interface CapabilityCopy {
  readonly name: string;
  readonly allow: string;
  readonly revoke: string;
  readonly description: string;
}

/** In the order they are offered; metadata keeps its original wording. */
export const CAPABILITIES: readonly CapabilityCopy[] = [
  {
    name: 'metadata.read',
    allow: 'Allow metadata access',
    revoke: 'Revoke metadata access',
    description:
      "Lets the app read this artifact's title and revision numbers.",
  },
  {
    name: 'source.read',
    allow: 'Allow reading source',
    revoke: 'Revoke reading source',
    description:
      "Lets the app read the files of the revision shown here, which is this artifact's own source code and content.",
  },
  {
    name: 'source.write',
    allow: 'Allow editing source',
    revoke: 'Revoke editing source',
    description:
      "Lets the app save changes to this artifact's files as a new revision under your name. It can replace or delete any file. Nothing is overwritten: a save is refused if the source has moved on, and you can restore an earlier revision from History.",
  },
  {
    name: 'source.download',
    allow: 'Allow downloads',
    revoke: 'Revoke downloads',
    description:
      'Lets the app ask you to download one file of this revision, or the whole revision as a ZIP. You are asked each time and nothing is saved until you confirm.',
  },
];

/** The grant after switching one capability on or off. */
export function nextGrant(
  granted: readonly string[],
  name: string,
  allowed: boolean,
): string[] {
  const others = granted.filter((entry) => entry !== name);
  return allowed ? [...others, name] : others;
}
