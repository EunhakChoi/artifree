import { parseServerUrl } from './server-url';

/** The build-time default server, checked: `url` is empty when there is none. */
export interface BuildDefault {
  readonly url: string;
  /** Set when the build was given an address that failed validation. */
  readonly notice?: string;
}

/**
 * Checks the address a static build was given in `VITE_ARTIFREE_SERVER`
 * (decision 0028). The build already refuses an invalid one, so a failure here
 * means the page was built some other way; it is reported and ignored.
 */
export function checkBuildDefault(raw: string): BuildDefault {
  if (raw.trim() === '') return { url: '' };
  const checked = parseServerUrl(raw);
  if (checked.ok) return { url: checked.url };
  return {
    url: '',
    notice: `The default server built into this page was ignored. ${checked.reason}`,
  };
}

/** The server the page starts with and why. */
export interface InitialServer {
  /** The API address, or an empty string for the page's own origin. */
  readonly url: string;
  /** True when `?server=` was present; a stored token is then not restored. */
  readonly fromQuery: boolean;
  /** Something the person should be told, such as a refused address. */
  readonly notices: readonly string[];
}

/**
 * Chooses the starting server. Order: the `?server=` address in the link, the
 * address kept for this tab, the default built into the page, then the page's
 * own origin. Every address is validated; one that fails is reported and
 * skipped, never used.
 */
export function resolveInitialServer(input: {
  readonly search: string;
  readonly stored: string | null;
  readonly buildDefault: string;
}): InitialServer {
  const buildDefault = checkBuildDefault(input.buildDefault);
  const notices: string[] =
    buildDefault.notice === undefined ? [] : [buildDefault.notice];

  const queryServer = new URLSearchParams(input.search).get('server');
  const fromQuery = queryServer !== null && queryServer.trim() !== '';
  if (fromQuery) {
    const checked = parseServerUrl(queryServer);
    if (checked.ok) return { url: checked.url, fromQuery, notices };
    notices.push(
      `The server address in the link was ignored. ${checked.reason}`,
    );
  }
  if (input.stored !== null && input.stored !== '') {
    const checked = parseServerUrl(input.stored);
    if (checked.ok) return { url: checked.url, fromQuery, notices };
    notices.push(
      `The server address kept for this tab was ignored. ${checked.reason}`,
    );
  }
  return { url: buildDefault.url, fromQuery, notices };
}
