/**
 * Hands bytes the user confirmed to the browser to save (decision 0032). The
 * bytes are never rendered, opened or navigated to: the management origin
 * only offers them as a download under a name it has checked.
 */

/** How long a blob URL stays valid after it was handed to the browser. */
export const OBJECT_URL_LIFETIME_MS = 60_000;

/** The browser calls this saver needs; a test supplies its own. */
export interface SaveTarget {
  createObjectURL(blob: Blob): string;
  revokeObjectURL(url: string): void;
  /** Starts the browser's save of `url` under `fileName`. */
  start(url: string, fileName: string): void;
  /** Runs `run` after `ms`; the returned function cancels it. */
  after(ms: number, run: () => void): () => void;
}

/** The browser's own implementation. */
export const browserTarget: SaveTarget = {
  createObjectURL: (blob) => URL.createObjectURL(blob),
  revokeObjectURL: (url) => {
    URL.revokeObjectURL(url);
  },
  start: (url, fileName) => {
    const link = document.createElement('a');
    link.href = url;
    link.download = fileName;
    link.rel = 'noopener';
    link.hidden = true;
    document.body.append(link);
    link.click();
    link.remove();
  },
  after: (ms, run) => {
    const handle = window.setTimeout(run, ms);
    return () => window.clearTimeout(handle);
  },
};

/**
 * Saves one file at a time. An object URL is revoked about a minute after it
 * was handed over, or at once when the next one replaces it, and never when a
 * frame goes away: the browser may still be reading the first.
 */
export class BlobSaver {
  readonly #target: SaveTarget;
  #live: { readonly url: string; readonly cancel: () => void } | undefined;

  constructor(target: SaveTarget = browserTarget) {
    this.#target = target;
  }

  /** Hands `bytes` to the browser under `fileName`. */
  save(bytes: Uint8Array<ArrayBuffer>, fileName: string, type: string): void {
    this.#release();
    const url = this.#target.createObjectURL(new Blob([bytes], { type }));
    const cancel = this.#target.after(OBJECT_URL_LIFETIME_MS, () => {
      if (this.#live?.url === url) this.#release();
    });
    this.#live = { url, cancel };
    try {
      this.#target.start(url, fileName);
    } catch (error) {
      this.#release();
      throw error;
    }
  }

  #release(): void {
    const live = this.#live;
    if (live === undefined) return;
    this.#live = undefined;
    live.cancel();
    this.#target.revokeObjectURL(live.url);
  }
}

/** The saver of this page. */
export const pageSaver = new BlobSaver();
