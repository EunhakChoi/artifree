# Application-data paths

R06b-1 implements the path and transport slice of [0046](decisions/0046-utf8-data-paths.md).
R06b-2 adds the process upgrade/restart and backup/restore checks described here.
Those focused checks passed; the follow-up's final repository gates and review
remain pending in that decision. Neither slice adds browser delivery or native
session recovery.

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

## Independent examples

These examples use Artifree's HTTP API directly. Set `ARTIFREE_URL`,
`ARTIFREE_TOKEN`, and `ARTIFREE_ARTIFACT` to your server, current token and a live
artifact you own. Read the current version before each write; replace the example
base `0` if this artifact already has data. Use a fresh key for a new operation
and retain the same body and key when retrying an uncertain result.

```sh
curl --fail-with-body "$ARTIFREE_URL/v1/artifacts/$ARTIFREE_ARTIFACT/data" \
  -H "Authorization: Bearer $ARTIFREE_TOKEN"

curl --fail-with-body -X PUT \
  "$ARTIFREE_URL/v1/artifacts/$ARTIFREE_ARTIFACT/data" \
  -H "Authorization: Bearer $ARTIFREE_TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: independent-unicode-example-1' \
  --data-raw '{"path":"記録/温度","base_data_version":0,"value":21}'

curl --fail-with-body --get \
  "$ARTIFREE_URL/v1/artifacts/$ARTIFREE_ARTIFACT/data" \
  -H "Authorization: Bearer $ARTIFREE_TOKEN" \
  --data-urlencode 'path=記録/温度'
```

The write increments the data version once. Retrying that exact write with the
same key replays its original receipt even after another write advances the head.
The query encodes the supplied path once. Do not pre-encode its text.

Root JSON can hold both `"é"` and `"e\u0301"` as distinct members. Path reads of
those decoded strings select their respective values. A root can also retain
`"bad/name"`, but no path can select that single member: `bad/name` means two
segments. Read or replace its ancestor instead. None of these names is normalized.

## Verifying restart, upgrade and restore

`npm run test:tooling` includes a real current-server process restart and
backup/restore test. It verifies canonical UTF-8 response bytes, distinct NFC/NFD
members, unchanged versions/feed on reopen, historical ASCII/Unicode receipts,
listing token bytes, stale-base rejection and restore's outbox fence. It kills
the server after a committed write; it does not simulate power loss.

The separate two-binary check requires an actual pre-0046 executable and the
current executable. It refuses missing arguments and fails if the old executable
already accepts Unicode paths; it never silently skips an unavailable baseline.
The baseline below is the last main commit before #124, with the same schema 12
and Cargo lockfile as the path expansion. Build it in a separate checkout:

```sh
git worktree add --detach ../artifree-before-utf8 2e6a3753db07a8f853911eaca35902dec154e187
cargo build --manifest-path ../artifree-before-utf8/Cargo.toml --locked -p artifree-server --bin artifree-server
cargo build --locked -p artifree-server --bin artifree-server
npm run test:data-upgrade -- \
  --previous-server ../artifree-before-utf8/target/debug/artifree-server \
  --server target/debug/artifree-server
```

On Windows use the `.exe` paths. Use the repository's resource-limited verification
environment when configured locally; offline builds also require preinstalled
locked dependencies. The check starts both supplied executables sequentially on
loopback, writes its own temporary database through HTTP, invokes the current
backup/restore CLI, and stops its processes/removes its data in `finally`.
Startup errors are normal rejected operations. Shutdown waits five seconds for
TERM, then sends KILL and waits five seconds for actual child closure; an
unconfirmed shutdown fails the check. Original verification and cleanup failures
are reported together. If child closure cannot be confirmed, the check retains
the temporary database and reports its absolute directory, PID and reason.
Confirm that the owned child has exited before removing that directory; the
check does not report cleanup success. Other failures still remove the temporary
data. The process helper and this preservation decision have boundary tests.
It does not download or build a historical release itself. Keep the two build
outputs separate, then remove the temporary checkout and build caches after use
as required by [the cleanup policy](quality.md#cleanup-after-verification-and-merge).

This checks server process and persisted-request compatibility. It does not run
an old client bundle or prove mixed-version browser/native-session recovery.
Restored outbox bindings require explicit reconciliation; the test's fresh-binding
replay is an authenticated HTTP operation, not automatic queue reauthorization.

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
