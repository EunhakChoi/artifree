import type { Offer, Outcome } from '../offer-desk';
import { type CallDeps, type FailureCode, type FrameReply, ok } from './call';

const OUTCOME_CODE: Record<Exclude<Outcome, 'accepted'>, FailureCode | null> = {
  declined: 'declined',
  // The frame is told nothing about a withdrawn offer: it is gone or gave up.
  withdrawn: null,
  busy: 'rate_limited',
  cooling: 'rate_limited',
};

/** What one family's download does at each step (decision 0052, rule 7). */
export interface DownloadSteps<O> {
  /** Asks the server for the offer. */
  readonly offer: () => Promise<O>;
  /** The object the prompt desk is handed for the offer. */
  readonly prompt: (offer: O) => Offer;
  /** Fetches the offered bytes; called only after an accept. */
  readonly fetch: (offer: O) => Promise<Uint8Array<ArrayBuffer>>;
  /** The file name and media type the bytes are saved under. */
  readonly saveAs: (offer: O) => {
    readonly fileName: string;
    readonly type: string;
  };
  /** The receipt the frame is told. */
  readonly receipt: (
    offer: O,
    bytes: Uint8Array<ArrayBuffer>,
  ) => Record<string, unknown>;
}

/**
 * The server is asked first, so a prompt never shows an offer the server
 * would refuse: no grant, an old head or an outdated view, a missing file or
 * asset. Only the user's accept makes the workspace fetch the bytes, and the
 * server checks again.
 */
export async function download<O>(
  id: number,
  deps: CallDeps,
  signal: AbortSignal,
  steps: DownloadSteps<O>,
): Promise<FrameReply | undefined> {
  const offer = await steps.offer();
  const outcome = await deps.ask(steps.prompt(offer), signal);
  if (signal.aborted) return undefined;
  if (outcome !== 'accepted') {
    const code = OUTCOME_CODE[outcome];
    return code === null
      ? undefined
      : { message: { id, ok: false, code }, transfer: [] };
  }
  const bytes = await steps.fetch(offer);
  if (signal.aborted) return undefined;
  const { fileName, type } = steps.saveAs(offer);
  deps.save(bytes, fileName, type);
  return ok(id, { receipt: steps.receipt(offer, bytes) });
}
