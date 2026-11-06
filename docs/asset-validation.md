# Private upload content validation

R09a content admission currently implements UTF-8 `text/plain`, UTF-8
`text/csv`, and a **restricted static PNG subset**. JPEG admission is disabled
with `asset_type_unsupported`. Decision 0045's complete R09a acceptance is not
met. This document records the validator module and dependency review, rather
than claiming that an isolated validator implements the upload/storage/HTTP
slice. The service must call it after authority/key/base preflight, outside
every database transaction. Replays bypass current admission.

Bytes are preserved exactly. Text allows tab, CR and LF but refuses other
Unicode control characters and invalid UTF-8. Markup and CSV formulas remain
opaque text; no sanitizer or malware-free guarantee is supplied. PNG dimensions
must be nonzero, at most 4,096 on either axis and 16,777,216 pixels overall.
The common input cap is 5 MiB; zero-byte supported uploads are invalid.

## Accepted PNG subset and limits

Only 8-bit RGB (color type 2) and 8-bit RGBA (color type 6) are admitted, with
standard compression/filter methods, no interlace, and exactly this structure:
one initial IHDR, one or more consecutive IDAT chunks, and one final empty IEND.
RGBA's alpha channel supports transparency. All other chunks and PNG features
are refused as `invalid_asset`, even when a broader PNG reader considers them
valid. This includes palettes, grayscale, other bit depths, Adam7, animation,
color profiles, gamma, text, EXIF and `tRNS`. Ordinary PNG files carrying these
features or metadata therefore cannot currently be uploaded. No bytes are
stripped, converted or silently normalized to make them fit the subset.

Framing validates these fixed fields, checked chunk extents and exact IEND end
position before passing the bytes to the maintained decoder. Compressed IDAT
bytes are collected under the existing 5 MiB input cap. Width, height and the
RGB/RGBA channel count determine the exact inflated scanline length, including
one filter byte per row. A streaming zlib check uses a 16 KiB output scratch
buffer, requires successful completion, Adler-32 verification and consumption
of every compressed byte, and refuses output beyond the expected length.
Output is discarded rather than retained. The maximum inflated length is
67,112,960 bytes (4,096 RGBA8 rows including filter bytes).

The maintained PNG pixel decoder then verifies all chunk CRCs and consumes
every filtered pixel row before finishing the image. The zlib check does not
substitute for pixel/filter decoding. No full decoded frame is allocated; the
largest raw row is 16,385 bytes. The library has a 64 MiB internal allocation
budget. Encoded allocations are bounded by the 5 MiB input, and pixel/inflation
work is bounded by the dimensions and exact row count. The library describes
its allocation limit as best effort; these are algorithm/input bounds, not an
operating-system RSS limit or deadline. Admission limits bound concurrency
separately. No palette/ancillary/Adam7 semantic validation is claimed.

## Dependency review (2026-11-06)

| Component   | Locked version and enabled use                                                                                  | License / safety review                                                       |
| ----------- | --------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| `png`       | 0.18.1; row decoder for the restricted static RGB/RGBA8 subset above; no other format decoder                   | MIT OR Apache-2.0; library forbids unsafe code                                |
| `flate2`    | 1.1.9; direct default features disabled, `rust_backend` enabled; complete zlib stream validation                | MIT OR Apache-2.0; miniz_oxide Rust backend, no C or zlib-rs backend selected |
| `crc32fast` | 1.5.4; direct dev dependency to recompute adversarial fixture CRCs; also a PNG transitive production dependency | MIT OR Apache-2.0; runtime CPU-selected SIMD contains unsafe intrinsics       |

Locked dependencies include `fdeflate` 0.3.7 and `miniz_oxide` 0.8.9 (both
forbid unsafe), `simd-adler32` 0.3.10 (CPU-selected unsafe SIMD), `adler2` 2.0.1,
and already-used `bitflags`/`cfg-if`. Dependency unsafe code is not covered by
the first-party prohibition. No formal safety audit is claimed. Selected
source/configuration was inspected; `cargo deny check advisories bans licenses
sources` passed with the current RustSec database. The focused suite passed on
Rust 1.88.0. The lock keeps flate2 1.1.9 to share miniz_oxide 0.8.9 with png;
1.1.10 would introduce a second version and violate the existing dependency
gate. No advisory exceptions were added. The validator test module explicitly
expects `clippy::unwrap_used`: invalid fixture construction must fail the test
immediately. This test-only expectation does not exempt production code.

Upstream: [PNG](https://github.com/image-rs/image-png),
[flate2](https://github.com/rust-lang/flate2-rs),
[CRC implementation](https://github.com/srijs/rust-crc32fast).

## Decoder discrepancies and remaining acceptance

Independently authored test fixtures showed that PNG 0.18.1 accepts IDAT truncated after
all required pixel bytes, including a missing checksum. The added strict zlib
check refuses it. Independent review also produced passing validator calls for
an indexed image missing its required palette and a grayscale image containing
a malformed four-byte palette. Regression tests failed for both cases before
the subset restriction. This is why unrestricted PNG acceptance was removed;
full PNG validation remains unfinished, rather than being inferred from a
successful generic decoder call.

JPEG candidates zune-jpeg 0.5.15, zune-jpeg 0.5.16-rc3 with strict mode, and
jpeg-decoder 0.3.2 accepted an independently constructed malformed 32-by-32 grayscale baseline
fixture containing only one zero entropy byte before EOI. Missing coefficient
data was effectively padded. A different fixture with no entropy bytes was
also accepted by zune-jpeg 0.5.15. Candidate dependencies were removed; no
prerelease decoder ships. Strict JPEG and broader PNG support require further
work before decision 0045's acceptance can be claimed. Historical envelope MIME
parsing stays independent of this current admission policy.
