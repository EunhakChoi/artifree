# Private upload content policy

The current partial R09a implementation admits only UTF-8 `text/plain` and
`text/csv`. **Every PNG and JPEG upload is `asset_type_unsupported`, including
valid images.** Complete image validation has not met decision 0045, rule 3.
A restricted PNG subset does not satisfy that rule and is not enabled.
R09a's full target remains unimplemented.

No image decoder or compression dependency ships in this slice. Earlier PNG
and JPEG decoder evaluations did not establish the complete validation the
specification requires. Their experimental implementation is outside this
change. Future image support must satisfy the unchanged decoder, allocation,
format, trailing-byte, animation and dependency-review requirements before it
is enabled. There is no SVG sanitizer or image-transform fallback.

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
even after the asset was deleted. Replay never recreates its bytes. A new PNG
or JPEG key is refused and reserves no key, blob, association or quota.

Content validation still runs after read-only authority/key/base preflight and
outside all database transactions. Final immediate settlement rechecks authority,
key and base before using the saved success or rejection. The persistence,
quota, retry and lifecycle tests remain part of this text-only partial slice.
Generated HTTP DTOs and read-only TypeScript client calls (`listAssets`,
`getAsset`, `readAsset`) exist. The client checks bounded metadata and content
responses, MIME, size and digest; it does not decode images or broaden upload
admission. The upload/delete client calls, the MCP asset tools, copying and the management
UI exist, as do the CLI asset commands; frame capabilities remain R09d work.
