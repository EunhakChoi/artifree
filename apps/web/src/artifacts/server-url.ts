/** The outcome of checking a server address typed, linked, or configured. */
export type ServerUrlResult =
  | { readonly ok: true; readonly url: string }
  | { readonly ok: false; readonly reason: string };

/**
 * Checks an API server address and returns its canonical form, without a
 * trailing slash. Only absolute `http:` and `https:` addresses are accepted.
 * Credentials, a query, and a fragment are refused: the page sends the access
 * token to this address, and `fetch` itself rejects an address with
 * credentials, so a clear message beats a failed request.
 */
export function parseServerUrl(raw: string): ServerUrlResult {
  const text = raw.trim();
  if (text === '') return { ok: false, reason: 'The address is empty.' };
  let parsed: URL;
  try {
    parsed = new URL(text);
  } catch {
    return {
      ok: false,
      reason: 'The address must be absolute, such as https://api.example.com.',
    };
  }
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
    return {
      ok: false,
      reason: 'The address must start with http:// or https://.',
    };
  }
  if (parsed.username !== '' || parsed.password !== '') {
    return { ok: false, reason: 'The address must not contain credentials.' };
  }
  if (parsed.search !== '' || parsed.hash !== '') {
    return {
      ok: false,
      reason: 'The address must not have a query string or fragment.',
    };
  }
  const path = parsed.pathname.replace(/\/+$/, '');
  return { ok: true, url: `${parsed.origin}${path}` };
}
