import { useEffect, useRef, useState } from 'react';
import { ArtifreeApiError, deleteAsset, uploadAsset } from '@artifree/client';
import type { ArtifreeClient } from '@artifree/client';
import {
  afterOutcome,
  classify,
  conflictVersion,
  freezeDelete,
  freezeUpload,
  localUploadRefusal,
  refusalText,
} from './asset-attempt';
import type { AssetAttempt, AttemptOutcome } from './asset-attempt';
import { refusedFloor, useWriteGate } from './conditional-write';
import type { Wait } from './write-gate';

/** What the assets panel shows about the last upload or delete. */
export type AssetWriteState =
  | { readonly status: 'idle' }
  | { readonly status: 'sending'; readonly kind: AssetAttempt['kind'] }
  | {
      readonly status: 'done';
      readonly kind: 'upload';
      /** The receipt is the original result of an earlier send. */
      readonly replayed: boolean;
    }
  | {
      readonly status: 'done';
      readonly kind: 'delete';
      readonly replayed: boolean;
      /** False when the asset was already absent: still a success. */
      readonly deleted: boolean;
    }
  | {
      readonly status: 'conflict';
      /** The current asset version the refusal named, if it named one. */
      readonly current: number | undefined;
    }
  | { readonly status: 'refused'; readonly message: string }
  | { readonly status: 'unknown'; readonly message: string }
  | {
      /**
       * A retry of the held attempt was refused. The attempt stays held: the
       * refusal does not prove the earlier send was not made.
       */
      readonly status: 'retry_refused';
      readonly message: string;
      /** The current asset version a conflict named, if it named one. */
      readonly current: number | undefined;
    };

/** The attempt whose outcome is unknown, with when it was made. */
interface Held {
  readonly attempt: AssetAttempt;
  /** The page's reset count when it was first sent. */
  readonly resets: number;
}

/**
 * The asset writes of one artifact (decision 0045, rule 11): uploads and
 * deletes against the asset space's own version, with their own write gate,
 * since source, metadata and data writes neither advance nor rewind it.
 *
 * Every send is one request with no automatic retry. An attempt whose answer
 * was lost is held in memory, never persisted, until it succeeds or the user
 * forgets it; while it is held no other upload or delete is sent, and
 * `retry` sends that same frozen request under its key. A later refusal does
 * not release it, since it does not prove the earlier attempt was not made.
 * Each settled attempt bumps `listEpoch`, so the panel reads the space again
 * instead of trusting a receipt. No send is aborted when the view goes away.
 */
export function useAssetWrites(
  client: ArtifreeClient,
  artifactId: string,
  /** How often the server's data may have been replaced so far. */
  resets: number,
) {
  const gate = useWriteGate(resets);
  const [state, setState] = useState<AssetWriteState>({ status: 'idle' });
  const [held, setHeld] = useState<Held>();
  const [listEpoch, setListEpoch] = useState(0);
  const [uploads, setUploads] = useState(0);
  // State updates land after the event, so a second click in the same frame
  // would still see the first as not started.
  const busy = useRef(false);
  // The reset count now, for a check after an awaited file read.
  const latestResets = useRef(resets);
  useEffect(() => {
    latestResets.current = resets;
  }, [resets]);

  async function deliver(attempt: AssetAttempt): Promise<AttemptOutcome> {
    if (attempt.kind === 'upload') {
      const receipt = await uploadAsset(
        client,
        attempt.artifactId,
        attempt.request,
        {
          idempotencyKey: attempt.key,
        },
      );
      setState({ status: 'done', kind: 'upload', replayed: receipt.replayed });
      setUploads((value) => value + 1);
    } else {
      const receipt = await deleteAsset(
        client,
        attempt.artifactId,
        attempt.assetId,
        attempt.request,
        { idempotencyKey: attempt.key },
      );
      setState({
        status: 'done',
        kind: 'delete',
        replayed: receipt.replayed,
        deleted: receipt.deleted,
      });
    }
    return 'done';
  }

  /**
   * Sends `attempt` once; the caller has set `busy`. `retrying` marks a
   * retry of the held attempt, whose refusal is reported with it.
   */
  async function send(
    attempt: AssetAttempt,
    sentResets: number,
    retrying: boolean,
  ): Promise<AttemptOutcome> {
    setState({ status: 'sending', kind: attempt.kind });
    gate.setSending(true);
    let outcome: AttemptOutcome;
    try {
      outcome = await deliver(attempt);
    } catch (error: unknown) {
      outcome = classify(error);
      const current =
        outcome === 'conflict' && error instanceof ArtifreeApiError
          ? conflictVersion(error)
          : undefined;
      if (outcome === 'conflict') {
        const floor = refusedFloor(attempt.request.base_asset_version, current);
        if (floor !== undefined) gate.refuse(floor);
      }
      if (outcome === 'unknown') {
        setState({ status: 'unknown', message: refusalText(error) });
      } else if (retrying) {
        setState({
          status: 'retry_refused',
          // Never the definitive "nothing was changed" of a first send.
          message:
            outcome === 'conflict'
              ? 'The assets changed elsewhere.'
              : outcome === 'key_reused'
                ? 'Its request key was already used for a different request.'
                : refusalText(error),
          current,
        });
      } else if (outcome === 'conflict') {
        setState({ status: 'conflict', current });
      } else {
        setState({ status: 'refused', message: refusalText(error) });
      }
    } finally {
      busy.current = false;
      gate.setSending(false);
    }
    setHeld((current) => {
      const next = afterOutcome(current?.attempt, attempt, outcome);
      if (next === undefined) return undefined;
      return next === current?.attempt
        ? current
        : { attempt: next, resets: sentResets };
    });
    setListEpoch((value) => value + 1);
    return outcome;
  }

  /**
   * Uploads `file` as `contentType` against `base`. A file over 5 MiB is
   * refused before it is read. Refused while an attempt is held or sending.
   * Resolves to the outcome, or undefined when nothing was sent.
   */
  async function upload(
    file: File,
    contentType: string,
    base: number,
  ): Promise<AttemptOutcome | undefined> {
    if (held !== undefined || busy.current) return undefined;
    const refusal = localUploadRefusal(file.size);
    if (refusal !== undefined) {
      setState({ status: 'refused', message: refusal });
      return undefined;
    }
    busy.current = true;
    setState({ status: 'sending', kind: 'upload' });
    let bytes: Uint8Array;
    try {
      bytes = new Uint8Array(await file.arrayBuffer());
    } catch {
      // The browser gives no detail worth showing; nothing was sent.
      busy.current = false;
      setState({
        status: 'refused',
        message: 'The file could not be read, so nothing was sent.',
      });
      return undefined;
    }
    if (latestResets.current !== resets) {
      // The server's data may have been replaced while the file was read.
      busy.current = false;
      setState({
        status: 'refused',
        message:
          'The data may have been replaced while the file was read, so nothing was sent. Upload again after the list is read again.',
      });
      return undefined;
    }
    const attempt = frozen(() =>
      freezeUpload(artifactId, base, contentType, bytes),
    );
    return attempt === undefined ? undefined : send(attempt, resets, false);
  }

  /** Builds an attempt; on failure releases `busy` and reports nothing sent. */
  function frozen(build: () => AssetAttempt): AssetAttempt | undefined {
    try {
      return build();
    } catch {
      busy.current = false;
      setState({
        status: 'refused',
        message: 'The request could not be prepared, so nothing was sent.',
      });
      return undefined;
    }
  }

  /**
   * Deletes `assetId` against `base`. Refused while an attempt is held or
   * sending. Resolves to the outcome, or undefined when nothing was sent.
   */
  async function remove(
    assetId: string,
    base: number,
  ): Promise<AttemptOutcome | undefined> {
    if (held !== undefined || busy.current) return undefined;
    busy.current = true;
    const attempt = frozen(() => freezeDelete(artifactId, assetId, base));
    return attempt === undefined ? undefined : send(attempt, resets, false);
  }

  /**
   * Sends the held attempt again exactly as it was, under its key. Not
   * after the server's data may have been replaced since it was sent.
   */
  async function retry(): Promise<AttemptOutcome | undefined> {
    if (held === undefined || held.resets !== resets || busy.current)
      return undefined;
    busy.current = true;
    return send(held.attempt, held.resets, true);
  }

  /**
   * Drops the held attempt from this page. It sends nothing and says
   * nothing about whether the server made the change.
   */
  function forget(): void {
    if (busy.current) return;
    setHeld(undefined);
    setState({ status: 'idle' });
  }

  return {
    state,
    /** The attempt whose outcome is unknown, if any. */
    held: held?.attempt,
    /** Whether the held attempt predates a possible data replacement. */
    heldReplaced: held !== undefined && held.resets !== resets,
    /** Whether a send, or the file read before it, is under way. */
    sending: state.status === 'sending',
    /** Changes after every settled attempt; the panel reads the list again. */
    listEpoch,
    /**
     * Counts uploads that succeeded, first time or by retry; the upload form
     * clears when it changes, so the same file is not sent under a new key.
     */
    uploads,
    /**
     * Why a write against `base`, read when the reset count was
     * `shownResets`, waits, or undefined when it may be sent.
     */
    wait: (base: number, shownResets: number): Wait | undefined =>
      gate.wait(base, undefined, shownResets),
    upload,
    remove,
    retry,
    forget,
  };
}

/** The asset writes of one artifact, as {@link useAssetWrites} returns them. */
export type AssetWrites = ReturnType<typeof useAssetWrites>;
