# Application-data paths

R06b-1 implements the path and transport slice of [0046](decisions/0046-utf8-data-paths.md).
Its verification and merge status is recorded in that decision; cross-process
upgrade and backup/recovery evidence remains the separate R06b-2 slice.

A path addresses object members, with `/` between segments and the empty string
for the root. Each segment is 1–64 UTF-8 bytes; there are at most 16 segments.
ASCII letters, digits, `_` and `-` retain their meaning. Non-ASCII letters,
marks, numbers and symbols use Unicode 16.0.0, excluding whitespace, default
ignorables, noncharacters and the explicit slash/backslash lookalikes of 0046.
Joiners and variation selectors are excluded, so many emoji sequences are not
valid names. The grammar does not cover every JSON member name.

Text is exact: `é` and `e` followed by U+0301 address different existing members.
There is no Unicode normalization, case folding or confusable folding. Korean
`한글/값` and a plain supplementary symbol such as `𝄞` are addressable. These
rules are Artifree's design, not observations of another product.

JSON requests, CLI arguments and MCP arguments contain paths as text. A GET
query encodes UTF-8 and the server decodes it once: `%2F` becomes a separator;
`%252F` becomes the invalid literal `%2F`. Malformed percent escapes and invalid
UTF-8 receive `400 invalid_request`; well-formed text outside the grammar gets
`422 invalid_request`. GET clients reject unpaired surrogates before form
serialization could replace them. Writes and retained browser outbox requests
keep the supplied path and raw JSON text for the server to accept or reject.

Existing JSON, request keys, ASCII fingerprints and listing tokens need no
migration. A newly addressable member becomes listable without a data write or
version/feed change. Names sort by exact UTF-8 bytes. Restart a paged traversal
after upgrading: `data_version` does not identify the grammar version. Upgrade
server and client together before using non-ASCII paths; mixed-version Unicode
interoperability is not promised. Source paths and artifact IDs are unchanged.

## Reproducing the character table

Ordinary builds require no network or Unicode dependency. The committed Rust
table is generated from these public files at
<https://www.unicode.org/Public/16.0.0/ucd/>:

| File                      | SHA-256                                                            |
| ------------------------- | ------------------------------------------------------------------ |
| UnicodeData.txt           | `ff58e5823bd095166564a006e47d111130813dcf8bf234ef79fa51a870edb48f` |
| PropList.txt              | `53d614508e2a0b2305a8aa21cd60d993de9326cdf65993660dfcce4503548583` |
| DerivedCoreProperties.txt | `39d35161f2954497f69e08bdb9e701493f476a3d30222de20028feda36c1dabd` |

Download those files into a temporary directory, then run
`node scripts/generate-data-unicode.mjs /path/to/Unicode-16.0.0-directory`.
The offline generator verifies all source hashes before writing and verifies
the generated table's pinned hash. Keep the Unicode License v3 attribution in
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

`npm run check:data-unicode`, also part of `npm run check`, verifies the committed
table's exact generation hash without downloaded files. This additional check
protects the reviewed Unicode release and exclusion rules from accidental edits
or compiler upgrades. A Unicode release change requires a reviewed specification
amendment and a regenerated table with independently checked fixtures.
