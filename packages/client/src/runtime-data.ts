import type { RuntimeDataValue, RuntimeView } from '@artifree/contracts';
import { isCount, isRecord } from './responses.js';
import { FRAME_ROUTES, frameBody, frameRoute } from './runtime-frame.js';
import type { FrameTransport } from './runtime-frame.js';
import { MAX_DATA_READ_BYTES } from './runtime-limits.js';

/**
 * The application data read of a runtime view (decision 0053). Like
 * runtime-assets, this module is reached only from the on-demand runtime-source
 * module, so the main client bundle does not grow.
 */

/**
 * Whether `text` is JSON text within the read bound (decision 0053, rule 11),
 * and for the root, an object. A text longer than the bound in UTF-16 units is
 * refused before it is encoded, since each unit takes at least one byte.
 */
function isDataText(text: string, root: boolean): boolean {
  if (
    text.length > MAX_DATA_READ_BYTES ||
    new TextEncoder().encode(text).byteLength > MAX_DATA_READ_BYTES
  )
    return false;
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
 * nothing is at the path there is no value, so an answer that carries one is
 * refused; the root always exists, so an answer that says it does not is
 * refused too.
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
  if (!exists)
    return path === '' || Object.hasOwn(value, 'value') ? undefined : base;
  const text: unknown = value.value;
  if (typeof text !== 'string' || !isDataText(text, path === ''))
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
