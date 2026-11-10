import type {
  DataWriteResponse,
  RuntimeDataDeleteRequest,
  RuntimeDataSetRequest,
  RuntimeDataUpdateRequest,
  RuntimeDataValue,
  RuntimeView,
} from '@artifree/contracts';
import { ArtifreeApiError } from './errors.js';
import { isCount, isRecord, parseDataWrite } from './responses.js';
import { FRAME_ROUTES, frameBody, frameRoute } from './runtime-frame.js';
import type { FrameRouteTail, FrameTransport } from './runtime-frame.js';
import { MAX_DATA_READ_BYTES, MAX_DATA_WRITE_BYTES } from './runtime-limits.js';
import { isRetryId } from './source-part.js';

/**
 * The application data of a runtime view (decision 0053): its reads and its
 * writes. Like runtime-assets, this module is reached only from the on-demand
 * runtime-source module, so the main client bundle does not grow. Writes are
 * not retried automatically: a caller that does not know whether one took
 * effect sends the same write again with the same retry identity, and the
 * server answers with the first receipt (decision 0053, rules 9 and 13).
 */

/**
 * Whether `text` takes at most `bound` UTF-8 bytes. A text longer than the
 * bound in UTF-16 units is refused before it is encoded, since each unit takes
 * at least one byte. A UTF-16 unit takes one to three bytes, so most texts are
 * settled by their length, and only one in the range between is encoded to be
 * counted.
 */
function withinBytes(text: string, bound: number): boolean {
  if (text.length > bound) return false;
  if (text.length * 3 <= bound) return true;
  return new TextEncoder().encode(text).byteLength <= bound;
}

/**
 * Whether `text` is JSON text within `bound` bytes (decision 0053, rules 5 and
 * 11), and for the root of a read, an object.
 */
function isDataText(text: string, bound: number, root: boolean): boolean {
  if (!withinBytes(text, bound)) return false;
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return false;
  }
  return !root || isRecord(parsed);
}

/**
 * Validates a data read against what was asked. The artifact and the path must
 * be echoed, the version must be a safe integer, and a value that exists must
 * be JSON text within the read bound, an object when it is the root. Where
 * nothing is at the path there is no value: the server omits it, the contract
 * also allows `null`, and an answer that carries a text there is refused. The
 * root always exists, so an answer that says it does not is refused too.
 */
export function parseRuntimeDataValue(
  value: unknown,
  artifactId: string,
  path: string,
): RuntimeDataValue | undefined {
  if (!isRecord(value)) return undefined;
  const { artifact_id, path: echoed, data_version, exists } = value;
  if (
    artifact_id !== artifactId ||
    echoed !== path ||
    !isCount(data_version) ||
    typeof exists !== 'boolean'
  )
    return undefined;
  const base = { artifact_id: artifactId, path, data_version, exists };
  if (!exists) {
    const none = value.value === undefined || value.value === null;
    return path === '' || !none ? undefined : base;
  }
  const text: unknown = value.value;
  if (
    typeof text !== 'string' ||
    !isDataText(text, MAX_DATA_READ_BYTES, path === '')
  )
    return undefined;
  return { ...base, value: text };
}

/**
 * The value at `path` of the view's artifact's application data, as JSON text.
 * The path is sent as given: the server judges its grammar (decision 0053,
 * rule 6), and this module checks only the answer.
 */
export function readRuntimeData(
  frame: FrameTransport,
  view: RuntimeView,
  path: string,
  signal: AbortSignal | undefined,
): Promise<RuntimeDataValue> {
  return frame.json(
    frameRoute(view, FRAME_ROUTES.dataRead),
    frameBody(view, { path }),
    (value) => parseRuntimeDataValue(value, view.artifact_id, path),
    signal,
  );
}

/** What a frame asked to set at one path, under its retry identity. */
export interface RuntimeDataSetInput {
  /** The path to write (decision 0046); the empty string is the root. */
  readonly path: string;
  /** The new value as JSON text, at most 262,144 bytes. */
  readonly value: string;
  /** The version of the data space the frame last observed. */
  readonly baseDataVersion: number;
  /** The retry identity: 8 to 36 ASCII letters, digits, `-` or `_`. */
  readonly retryId: string;
}

/** What a frame asked to patch at one path, under its retry identity. */
export interface RuntimeDataUpdateInput {
  /** The path to patch (decision 0046); the empty string is the root. */
  readonly path: string;
  /** The patch as JSON text, at most 262,144 bytes. */
  readonly patch: string;
  /** The version of the data space the frame last observed. */
  readonly baseDataVersion: number;
  /** The retry identity: 8 to 36 ASCII letters, digits, `-` or `_`. */
  readonly retryId: string;
}

/** What a frame asked to remove at one path, under its retry identity. */
export interface RuntimeDataRemoveInput {
  /** The path to remove (decision 0046); the empty string is the root. */
  readonly path: string;
  /** The version of the data space the frame last observed. */
  readonly baseDataVersion: number;
  /** The retry identity: 8 to 36 ASCII letters, digits, `-` or `_`. */
  readonly retryId: string;
}

/**
 * Whether a base version is one the client sends: a count that the base plus
 * one still states exactly.
 */
function isBaseVersion(value: unknown): value is number {
  return isCount(value) && value < Number.MAX_SAFE_INTEGER;
}

/** Whether a write names a string path, a retry identity and a base version. */
function isWriteRequest(
  path: unknown,
  retryId: unknown,
  baseDataVersion: unknown,
): boolean {
  return (
    typeof path === 'string' &&
    isRetryId(retryId) &&
    isBaseVersion(baseDataVersion)
  );
}

/** Whether a write's text is a string within the write bound that parses. */
function isWriteText(text: unknown): boolean {
  return (
    typeof text === 'string' && isDataText(text, MAX_DATA_WRITE_BYTES, false)
  );
}

/** The refusal of a write the client can see is not valid. Nothing is sent. */
function invalidWrite(): ArtifreeApiError {
  return new ArtifreeApiError('The data write was not valid.', {
    status: 0,
    code: 'invalid_request',
  });
}

/**
 * Validates a data write's receipt against the write it answers. The server
 * states the artifact, the path and the version, and none of it is believed:
 * the artifact and the path must be the ones sent, and the version must be the
 * base plus one when the write changed the space and the base when it did not,
 * a replay included.
 */
export function parseRuntimeDataWriteReceipt(
  value: unknown,
  artifactId: string,
  path: string,
  baseDataVersion: number,
): DataWriteResponse | undefined {
  const receipt = parseDataWrite(value);
  if (
    receipt === undefined ||
    receipt.artifact_id !== artifactId ||
    receipt.path !== path ||
    receipt.data_version !==
      (receipt.changed ? baseDataVersion + 1 : baseDataVersion)
  )
    return undefined;
  return receipt;
}

/**
 * Sends one write of the view's application data and checks its receipt
 * against the path and the base version the frame sent.
 */
function sendDataWrite(
  frame: FrameTransport,
  view: RuntimeView,
  tail: FrameRouteTail,
  body: Record<string, unknown>,
  sent: { readonly path: string; readonly baseDataVersion: number },
  signal: AbortSignal | undefined,
): Promise<DataWriteResponse> {
  return frame.json(
    frameRoute(view, tail),
    frameBody(view, body),
    (value) =>
      parseRuntimeDataWriteReceipt(
        value,
        view.artifact_id,
        sent.path,
        sent.baseDataVersion,
      ),
    signal,
  );
}

/**
 * Replaces the value at `input.path` for the frame, if the data is still at
 * `input.baseDataVersion`. It resolves only with a receipt that matches the
 * write sent. The path, the retry identity, the base version and the JSON text
 * are checked before anything is sent: a write that fails them rejects with
 * `invalid_request` and is not sent. Nothing here retries the write: a retry
 * must reuse the retry identity for the identical write.
 */
export async function setRuntimeData(
  frame: FrameTransport,
  view: RuntimeView,
  input: RuntimeDataSetInput,
  signal: AbortSignal | undefined,
): Promise<DataWriteResponse> {
  const { path, value, baseDataVersion, retryId } = input;
  if (!isWriteRequest(path, retryId, baseDataVersion) || !isWriteText(value))
    throw invalidWrite();
  const body: Omit<RuntimeDataSetRequest, 'initialization'> = {
    path,
    base_data_version: baseDataVersion,
    retry_id: retryId,
    value,
  };
  return sendDataWrite(
    frame,
    view,
    FRAME_ROUTES.dataSet,
    body,
    { path, baseDataVersion },
    signal,
  );
}

/**
 * Applies a JSON Merge Patch (RFC 7396) at `input.path` for the frame. The
 * checks and the receipt are those of {@link setRuntimeData}.
 */
export async function updateRuntimeData(
  frame: FrameTransport,
  view: RuntimeView,
  input: RuntimeDataUpdateInput,
  signal: AbortSignal | undefined,
): Promise<DataWriteResponse> {
  const { path, patch, baseDataVersion, retryId } = input;
  if (!isWriteRequest(path, retryId, baseDataVersion) || !isWriteText(patch))
    throw invalidWrite();
  const body: Omit<RuntimeDataUpdateRequest, 'initialization'> = {
    path,
    base_data_version: baseDataVersion,
    retry_id: retryId,
    patch,
  };
  return sendDataWrite(
    frame,
    view,
    FRAME_ROUTES.dataUpdate,
    body,
    { path, baseDataVersion },
    signal,
  );
}

/**
 * Removes the value at `input.path` for the frame; the empty path empties the
 * whole space. Removing what is not there changes nothing and keeps the version
 * (`changed: false`). The checks and the receipt are those of
 * {@link setRuntimeData}.
 */
export async function removeRuntimeData(
  frame: FrameTransport,
  view: RuntimeView,
  input: RuntimeDataRemoveInput,
  signal: AbortSignal | undefined,
): Promise<DataWriteResponse> {
  const { path, baseDataVersion, retryId } = input;
  if (!isWriteRequest(path, retryId, baseDataVersion)) throw invalidWrite();
  const body: Omit<RuntimeDataDeleteRequest, 'initialization'> = {
    path,
    base_data_version: baseDataVersion,
    retry_id: retryId,
  };
  return sendDataWrite(
    frame,
    view,
    FRAME_ROUTES.dataDelete,
    body,
    { path, baseDataVersion },
    signal,
  );
}
