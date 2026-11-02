// Which API server the page talks to (decision 0028). No DOM or React imports:
// the Vite config and the tooling tests use this file as it ships.

/** An accepted server address, or why the text was refused. */
export type ServerUrlResult =
  | { readonly ok: true; readonly url: string }
  | { readonly ok: false; readonly reason: string };

const NOT_ABSOLUTE = 'it is not a valid absolute http or https address';
const CREDENTIALS = 'it contains a user name or password';
const QUERY_OR_FRAGMENT = 'it contains a query or fragment';

/**
 * Checks a server address from any source: the `?server=` link, the connect
 * form, a value kept for the tab, or the build default. It must be an
 * absolute `http` or `https` address without credentials, query, or fragment
 * (an empty `?` or `#` counts). On success `url` is `origin + path` without a
 * trailing slash. The reason never repeats the text, which may hold a secret.
 */
export function parseServerUrl(text: string): ServerUrlResult {
  const trimmed = text.trim();
  // Browsers read a backslash as a slash, so what follows one is not where it looks.
  if (!/^https?:\/\/[^/]/i.test(trimmed) || trimmed.includes('\\'))
    return refused(NOT_ABSOLUTE);
  if (/[?#]/.test(trimmed)) return refused(QUERY_OR_FRAGMENT);
  const authority = trimmed.slice(trimmed.indexOf('//') + 2).split('/')[0];
  if (authority?.includes('@') === true) return refused(CREDENTIALS);
  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    // The URL parser's message quotes the text, so it is not passed on.
    return refused(NOT_ABSOLUTE);
  }
  if (parsed.username !== '' || parsed.password !== '')
    return refused(CREDENTIALS);
  return {
    ok: true,
    url: `${parsed.origin}${parsed.pathname}`.replace(/\/+$/, ''),
  };
}

function refused(reason: string): ServerUrlResult {
  return { ok: false, reason };
}

/** The server a page starts with, and what to tell the person about it. */
export interface InitialServer {
  /** Address of the API server; empty means the page's own origin. */
  readonly url: string;
  readonly source: 'link' | 'tab' | 'build' | 'page';
  /**
   * Whether a token kept for this server may be restored without asking. It
   * is not when the link named a server, or when any address was ignored: the
   * connect form is the only place that explains this, so the person must
   * reach it.
   */
  readonly restoreToken: boolean;
  /** One sentence for each address that was ignored; none repeats it. */
  readonly notices: readonly string[];
}

/**
 * Chooses the starting server: the `?server=` link, then the value kept for
 * this tab, then the build default, then the page's own origin. An invalid
 * source is skipped with a notice and never requested.
 */
export function resolveInitialServer(input: {
  readonly search: string;
  readonly stored: string | undefined;
  readonly buildDefault: string | undefined;
}): InitialServer {
  const notices: string[] = [];
  const candidates: Array<{
    source: 'link' | 'tab' | 'build';
    text: string;
    ignored: string;
  }> = [
    {
      source: 'link',
      text: new URLSearchParams(input.search).get('server')?.trim() ?? '',
      ignored: 'The server address in the link was ignored because',
    },
    {
      source: 'tab',
      text: input.stored?.trim() ?? '',
      ignored: 'The server address kept for this tab was ignored because',
    },
    {
      source: 'build',
      text: input.buildDefault?.trim() ?? '',
      ignored: 'The server address built into this page was ignored because',
    },
  ];
  const linkNamedServer = candidates[0]?.text !== '';
  const finish = (
    url: string,
    source: InitialServer['source'],
  ): InitialServer => ({
    url,
    source,
    restoreToken: !linkNamedServer && notices.length === 0,
    notices,
  });
  for (const { source, text, ignored } of candidates) {
    if (text === '') continue;
    const result = parseServerUrl(text);
    if (result.ok) return finish(result.url, source);
    notices.push(`${ignored} ${result.reason}.`);
  }
  return finish('', 'page');
}

/**
 * The server address worth keeping for the tab, or undefined. The page's own
 * origin and the build default are not kept: they are what a new tab starts
 * with anyway, and keeping them would pin an old default.
 */
export function serverToKeep(
  url: string,
  buildDefault: string | undefined,
): string | undefined {
  if (url === '') return undefined;
  const built = defaultServer(buildDefault);
  return built !== '' && built === url ? undefined : url;
}

/** The build default as an address, or empty (the page's own origin). */
export function defaultServer(buildDefault: string | undefined): string {
  const result = parseServerUrl(buildDefault ?? '');
  return result.ok ? result.url : '';
}
