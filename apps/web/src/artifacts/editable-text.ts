/** A file's text as the editor shows it, or why it cannot be edited. */
export type EditableText =
  | {
      readonly editable: true;
      /** Content with LF line breaks, as a textarea reports it. */
      readonly text: string;
      /** Whether the file uses CRLF line breaks throughout. */
      readonly crlf: boolean;
    }
  | { readonly editable: false; readonly reason: string };

/**
 * Decodes file bytes for a textarea so that saving writes back exactly the
 * bytes the user did not change. Textareas report every line break as LF,
 * so a file with CRLF throughout is edited as LF and saved with CRLF again.
 * Invalid UTF-8, a lone CR, or mixed line breaks cannot round-trip, so such
 * files are not editable here. A byte order mark is kept as U+FEFF.
 */
export function toEditable(bytes: Uint8Array): EditableText {
  let text: string;
  try {
    text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(
      bytes,
    );
  } catch {
    return { editable: false, reason: 'is not valid UTF-8 text' };
  }
  const lf = text.replaceAll('\r\n', '\n');
  if (lf.includes('\r'))
    return { editable: false, reason: 'contains a carriage return on its own' };
  const crlf = lf !== text;
  if (crlf && /(?:^|[^\r])\n/u.test(text))
    return { editable: false, reason: 'mixes CRLF and LF line breaks' };
  return { editable: true, text: lf, crlf };
}

/** The content to save for edited `text` of a file read by `toEditable`. */
export function fromEditable(text: string, crlf: boolean): string {
  return crlf ? text.replaceAll('\n', '\r\n') : text;
}
