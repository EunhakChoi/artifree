import type { Offer } from './offer-desk';

/** The longest stretch of frame-supplied path text shown. */
const SHOWN_PATH_CHARS = 80;

/** What each kind of source offer is called in the description. */
const SOURCE_WORDING = {
  file: 'a source file of this revision',
  archive: 'the source files of this revision as a ZIP archive',
} as const;

/** The strings the download prompt shows for one offer (decision 0052, rule 8). */
export interface OfferText {
  /** The dialog title. */
  readonly title: string;
  /** The sentence under the title. */
  readonly description: string;
  /** The name the file is saved as; absent when no offer is open. */
  readonly fileName: string | undefined;
  /** The source path, shortened; absent for an asset or a source offer without a path. */
  readonly sourcePath: string | undefined;
  /** The size as shown, such as `12 bytes`; absent when the server stated none. */
  readonly size: string | undefined;
}

/**
 * Maps an offer to the strings the prompt shows. This is the only place that
 * reads the offer's kind for the prompt. Without an offer, as while the closed
 * dialog animates out, the wording is that of a source file, as it was when
 * the prompt chose its own text.
 */
export function offerText(offer: Offer | undefined): OfferText {
  if (offer === undefined) return sourceText(SOURCE_WORDING.file);
  switch (offer.kind) {
    case 'asset':
      return {
        title: 'Save uploaded file?',
        description:
          'The preview asked to save a file uploaded to this artifact. Saving it does not make it safe to open. Nothing is saved unless you accept.',
        fileName: offer.fileName,
        sourcePath: undefined,
        size: `${offer.bytes} bytes`,
      };
    case 'file':
    case 'archive':
      return {
        ...sourceText(SOURCE_WORDING[offer.kind]),
        fileName: offer.fileName,
        sourcePath: offer.path === null ? undefined : shorten(offer.path),
        size: offer.size === null ? undefined : `${offer.size} bytes`,
      };
    default:
      // A kind added to `Offer` without wording here fails the typecheck.
      return offer satisfies never;
  }
}

/** The text every source offer shares, with no details. */
function sourceText(what: string): OfferText {
  return {
    title: 'Save a file from this preview?',
    description: `The preview asked to save ${what}. Nothing is saved unless you accept.`,
    fileName: undefined,
    sourcePath: undefined,
    size: undefined,
  };
}

/** Keeps the first SHOWN_PATH_CHARS characters and marks a cut with an ellipsis. */
function shorten(path: string): string {
  return path.length > SHOWN_PATH_CHARS
    ? `${path.slice(0, SHOWN_PATH_CHARS)}…`
    : path;
}
