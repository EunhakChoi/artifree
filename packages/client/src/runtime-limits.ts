/**
 * Bounds that more than one layer applies to what a runtime frame sends or
 * receives (decision 0052, rule 10). Each is defined here once and exported.
 * The host keeps its own literals rather than importing these (decision 0033),
 * and so does the frame's bootstrap. A test pins each of those literals to the
 * value here, and each value here to the generated API schema where the schema
 * states it.
 */

/** What a frame's retry identity looks like: 8 to 36 of `A-Za-z0-9_-`. */
export const RETRY_ID = /^[A-Za-z0-9_-]{8,36}$/;

/** What an asset ID looks like: 64 lowercase hexadecimal digits. */
export const ASSET_ID = /^[0-9a-f]{64}$/;

/** Most bytes in one part of a file, as the server's contract states. */
export const MAX_SOURCE_PART_BYTES = 1_048_576;

/** Most bytes in one file a frame saves, as the server's contract states. */
export const MAX_SOURCE_WRITE_BYTES = 1_048_576;

/** Most bytes in one part of an asset a read may ask for: 256 KiB. */
export const MAX_ASSET_PART_BYTES = 262_144;

/** Most bytes one asset upload may carry: 1 MiB (decision 0045, rule 12). */
export const MAX_ASSET_UPLOAD_BYTES = 1_048_576;

/** One asset's decoded bytes are at most 5 MiB (decision 0045 rule 4). */
export const MAX_ASSET_BYTES = 5_242_880;

/** Most files a runtime view's source listing may name. */
export const MAX_LISTED_FILES = 256;

/** Most assets one space holds and one listing names (decision 0045, rules 4 and 7). */
export const MAX_LISTED_ASSETS = 64;
