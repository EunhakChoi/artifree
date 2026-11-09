# Private upload content policy

New uploads admit UTF-8 `text/plain`, `text/csv` and complete static `image/png`
(decision 0045, rule 3). **`image/jpeg` is still `asset_type_unsupported`, valid
files included**, so R09a is not complete: no JPEG decoder satisfies rule 3
without an Artifree-owned entropy-segment check, and a weaker check is not
enabled. SVG, GIF, WebP and every other type are `asset_type_unsupported`. There
is no SVG sanitizer, thumbnail, re-encoding or metadata stripping: admitted bytes
are stored exactly as sent.

## PNG admission

Order: declared type allowlist, then the decoded size (5 MiB,
`asset_limit_exceeded`), then empty content, then the signature, then the
checks below. Every failure after the size check is `invalid_asset`, and a
refused upload writes nothing and keeps its key free.

1. **Container scan** (before any data-dependent allocation). The 8-byte
   signature, then every chunk: at least 12 bytes left, length at most 2^31-1, a
   type of four ASCII letters with the reserved bit clear, data inside the file
   and a matching CRC-32. IHDR is first, 13 bytes and unique. Width and height are
   non-zero and at most 4,096 pixels each, at most 16,777,216 pixels in all, with a
   legal bit-depth and color-type pair, compression and filter method 0, and
   interlace 0 or 1. `acTL`, `fcTL` and `fdAT` (animation) are refused, as is any
   unknown critical chunk. IDAT chunks form one run and none is missing; a palette
   image needs a PLTE of 1 to 256 entries before its first IDAT. IEND is empty and
   ends the file exactly: a trailing byte is refused.
2. **Exact zlib check** (`miniz_oxide`, independent of the decoder). The IDAT
   payloads are inflated through a 32 KiB window as one stream with the header and
   Adler-32 checked. Output is counted and discarded, and inflating stops as soon
   as it passes the length the IHDR implies (a decompression bomb is refused
   within one window). The stream must end exactly, with no bytes after it and
   exactly the implied number of bytes.
3. **Maintained decoder** (`png`, CRC and Adler-32 checks on, ancillary CRC
   failures not skipped, a 16 MiB buffering limit for ancillary chunks). Every
   row is decoded and the end of the file is read; animation is refused again.
   Compressed `zTXt` and `iTXt` text is inflated against one shared 16 MiB budget.
4. **Compressed ancillary streams** (`zTXt`, compressed `iTXt`, `iCCP`). The
   decoder ignores errors in some of these and stops reading a stream when it
   ends, so each is also checked as an exact, complete zlib stream (no byte
   after it, Adler-32 correct) inside the same shared budget, and at most one
   `iCCP` chunk before IDAT is allowed.

**Known limit.** The decoder treats a malformed ancillary chunk it does not
need (for example `pHYs`, `gAMA`, a `tEXt` keyword) as ignorable, as the PNG
specification permits, and the container scan checks only its framing and CRC.
Such a chunk is admitted and stored unchanged; Artifree does not validate the
content of ancillary chunks beyond the compressed streams above.

Decoding is serialized process-wide by one lock, so concurrent uploads cannot
multiply the decoder's memory. Rows are discarded as they are decoded; the
largest admitted image (4,096 by 4,096) takes about 0.2 s in a debug build.
Worst case measured (R09a): a 5 MiB file of about 227,000 minimal `zTXt` chunks
is admitted and takes about 0.45 s in a release build (5.5 s in debug), all of it
under the decode lock. No chunk-count cap is applied, because one would refuse
files rule 3 admits; the per-actor request limits of the server bound how often
one actor can start this, and a limit specific to asset validation is an open
question for multi-user sharing (S02).
This validation is for admission only. It does not make a PNG safe to render, and
the management UI downloads assets rather than showing them.

## Dependency review (R09a, PNG)

| Crate (version)     | Purpose                                        | Result on 2026-11-09                                   |
| ------------------- | ---------------------------------------------- | ------------------------------------------------------ |
| `png` 0.18.1        | Maintained decoder with complete format checks | `cargo deny` advisories, bans, licenses and sources ok |
| `miniz_oxide` 0.8.9 | Independent exact zlib/Adler-32 check          | same                                                   |
| `crc32fast` 1.5     | Chunk CRC-32 in the container scan             | same                                                   |

`flate2` is locked at 1.1.9 (a transitive dependency of `png`) in `Cargo.lock`
only: 1.1.11 pulls a second `miniz_oxide` (0.9.1) and `multiple-versions =
"deny"` refuses it, so a careless `cargo update` fails `cargo deny` instead of
changing the decoder silently. No unsafe
code is added to first-party crates; the dependencies are reviewed through
`cargo deny` and the lockfile. Fixtures are generated in test code; no third-party
image files are in the repository.

## Text and replay

Text bytes are preserved exactly. Validation requires nonempty UTF-8, permits
tab, CR and LF, and refuses other Unicode control characters. The decoded
byte range is 1 byte through 5 MiB inclusive. Markup and CSV formulas remain
opaque content; there is no sanitization, formula neutralization or
malware-free guarantee. The authenticated content response is an attachment
with no-store, nosniff and a sandbox CSP.

The stable v1 envelope is separate from current admission. MIME syntax,
bounded strict base64 and the decoded size bound precede replay lookup.
Historical image MIME spellings remain accepted by the envelope and by stored
metadata/receipt decoding. After current authority checks, a matching historical
upload replays its metadata-only receipt before current admission is consulted,
even after the asset was deleted. Replay never recreates its bytes. A new refused
upload (an invalid PNG, a JPEG, another type) reserves no key, blob, association or quota.

Content validation still runs after read-only authority/key/base preflight and
outside all database transactions. Final immediate settlement rechecks authority,
key and base before using the saved success or rejection. The persistence,
quota, retry and lifecycle tests apply to every admitted type.
Generated HTTP DTOs and read-only TypeScript client calls (`listAssets`,
`getAsset`, `readAsset`) exist. The client checks bounded metadata and content
responses, MIME, size and digest; it does not decode images or decide admission:
the server does. The upload/delete client calls, the MCP asset tools, copying and the management
UI exist, as do the CLI asset commands; frame capabilities remain R09d work.
