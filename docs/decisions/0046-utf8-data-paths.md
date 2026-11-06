# 0046: UTF-8 names in application data paths

Date: 2026-11-06. Status: proposed. Roadmap: R06b.
Builds on [0031](0031-app-data.md),
[0040](0040-data-changes-and-member-listing.md), and
[0041](0041-browser-data-outbox.md).

This is an independent Artifree design, not an observation of another product.
No private evidence was consulted for this proposal. R06b's grammar, limits,
transport handling, and compatibility rules below are our own decisions.
Before an implementation PR, follow the roadmap's evidence entry conditions;
record any original-source comparison privately and distinguish it from these
choices. This document does not claim that comparison has been completed.

**Nothing in this proposal is implemented.** The accepted ASCII grammar of
0031 and the listing rules of 0040 remain in effect. Each numbered rule below
becomes effective only when the corresponding implementation slice is merged
and its status is recorded here. A rule amendment requires a separate
specification PR, as required by the contribution policy.

## Context

Application JSON already permits object member names of 1 to 256 UTF-8 bytes.
Only some names can be addressed by a path or returned by a member listing:
today each path segment must match `[A-Za-z0-9_-]{1,64}`. Thus a root write can
store a Korean member that a subsequent path read cannot address. Widening the
path grammar should expose such names without rewriting existing JSON, merging
members, changing ASCII request identities, or creating a second path syntax.

## Proposed rules

1. **Identity is exact Unicode text.** Preserve the sequence of Unicode scalar
   values and its UTF-8 bytes. Do not apply NFC, NFD, NFKC, NFKD, case folding,
   locale rules, transliteration, or confusable skeletons when validating,
   storing, comparing, echoing, or fingerprinting a path. Both composed `é`
   and `e` followed by U+0301 may be addressed and remain different members.
   JSON escape spelling still does not affect identity: decoding `\u00e9`
   names the same member as literal `é`. Existing JSON canonicalization and
   duplicate-name checks remain unchanged. "Canonical path" in existing
   decisions means the accepted segments joined by `/`, not Unicode
   normalization. This preserves previously stored keys that differ only in
   normalization form; normalizing paths alone could select a different key.
2. **Segment grammar.** Keep ASCII exactly as before: letters, digits, `_`,
   and `-`. Additionally allow non-ASCII scalars whose Unicode 16.0.0 General
   Category is Letter (`L*`), Mark (`M*`), Number (`N*`), or Symbol (`S*`),
   subject to rule 3. A segment is still 1 to 64 UTF-8 bytes, not characters,
   UTF-16 code units, or grapheme clusters. A path has at most 16 segments;
   its maximum is 1,039 UTF-8 bytes including 15 separators. The empty path
   alone names the root; leading, trailing, or repeated `/` is invalid.
   Paths still enter objects only, never array indexes. No Unicode names are
   newly permitted in artifact IDs or source file paths by this decision.
3. **Excluded characters and confusables.** Even in rule 2's categories,
   reject Unicode 16.0.0 `White_Space`, `Default_Ignorable_Code_Point`, and
   `Noncharacter_Code_Point`, plus U+2044, U+2215, U+29F8, U+FF0F, U+FF3C,
   and U+FE68 (selected slash/backslash lookalikes). The category allowlist
   already rejects controls (`Cc`), format characters (`Cf`), separators
   (`Z*`), private-use (`Co`), unassigned scalars (`Cn`), and punctuation
   (`P*`). ASCII `%`, `.`, `~`, backslash, and whitespace remain invalid.
   Bidi controls, BOM, zero-width joiners, variation selectors, and other
   default-ignorables are refused, including within an emoji sequence. Plain
   symbols and ordinary combining marks are allowed; no first-letter rule
   or single-script restriction is added. Other visual lookalikes, including
   Latin and Cyrillic letters, remain distinct names: this is not a claim
   to detect all confusing names or to support every language or emoji name.
   Name display must treat names as text, not HTML. No new display UI is in
   this scope.
4. **Pinned character properties.** The Unicode 16.0.0 data in
   [UnicodeData](https://www.unicode.org/Public/16.0.0/ucd/UnicodeData.txt),
   [PropList](https://www.unicode.org/Public/16.0.0/ucd/PropList.txt), and
   [DerivedCoreProperties](https://www.unicode.org/Public/16.0.0/ucd/DerivedCoreProperties.txt)
   defines rules 2 and 3, independently of the host's locale or Unicode
   library version. This fixed release gives Rust and TypeScript a stable
   agreement; it is not a claim that it is the newest release. Implementation
   must document its dependency or reproducible table generation, retain
   applicable licenses, and test public fixtures through Rust validation and
   client transport/response boundaries. This does not require TypeScript to
   duplicate domain validation of supplied requests (rule 7).
   A later Unicode version is a reviewed specification amendment, not a
   silent behavior change during a compiler or dependency upgrade.
5. **Transport decoding happens once.** JSON bodies, MCP arguments, and CLI
   path arguments carry the path itself; never URL-decode those strings.
   HTTP GET uses its existing query parameter, encoded by the caller as
   UTF-8 percent escapes and decoded once by the query parser. An encoded
   `/` in that query becomes a path separator, never a literal slash in a
   member name. `%252F` decodes to `%2F`, which rule 3 rejects; a JSON/MCP
   `%2F` is also invalid. Raw `+` in a form-style query decodes as a space
   and is invalid; `%2B` is an invalid ASCII plus as well. Malformed percent
   escapes or invalid UTF-8 in the encoded path are `400 invalid_request`,
   without replacement characters or lossy decoding. A validly decoded path
   outside the grammar is `422 invalid_request`. Unknown/repeated query
   parameters and existing check ordering remain unchanged. For GET/query
   serialization only, reject a supplied unpaired surrogate before encoding:
   `URLSearchParams` must not silently turn it into U+FFFD. This is a query
   transport check, not a domain path validator. JSON body serialization and
   outbox enqueue/recovery retain the supplied path, including an unpaired
   surrogate represented by its JSON escape, under 0041 rule 7 and B23. Do
   not add a pre-send domain rejection for those writes. Rust refuses such
   malformed Unicode reaching the server with the existing body error
   behavior. There is no JSON Pointer escape or literal-slash path.
6. **No stored-data migration.** Keep every stored JSON member, including
   names outside the new path grammar, byte-for-byte equivalent under the
   existing JSON canonicalization. Root reads and writes can still contain
   such names subject to 0031's existing JSON rules. Do not reject an entire
   previously valid document because some keys are not addressable. A name
   newly matching rules 2 and 3 becomes addressable/listable without a write,
   data version increment, or change-feed event. Names outside the grammar
   remain accessible through an ancestor read or replacement. Do not claim
   that this expansion makes every valid JSON member addressable.
7. **One behavior across entry points.** `DataPath` and `MemberName` share
   the same Rust segment validator. HTTP, MCP, and core use that authority;
   client/CLI transport and response handling must preserve accepted names.
   Preserve 0041 rule 7 and B23: TypeScript does not newly domain-validate
   supplied paths or `JsonText`, at enqueue or recovery. An invalid supplied
   path may be durably enqueued and later receive the server's rejection;
   this is not `storage_corrupt`. Retain it under the existing rejection and
   explicit-disposal rules. Recovery validates the storage envelope, and
   receipt checks compare the prepared path exactly, without introducing
   domain path validation. Existing authorization, versions, conditional
   writes, JSON merge patch,
   `ArtifactService` mutation paths, idempotency keys, retries, and error
   confidentiality remain unchanged. Preserve the exact path bytes used by
   fingerprints: an ASCII request and an already persisted ASCII outbox row
   have the same request identity and replay result after upgrade. Never
   normalize or rewrite an enqueued request. No data runtime bridge, new MCP
   tools, or editable data UI is added.
8. **Listing and tokens.** List only addressable member names, ordered by
   exact UTF-8 bytes, with 0040's existing version and pagination semantics.
   Keep the existing token layout: a 16-byte artifact/path tag and the last
   name's 1 to 64 UTF-8 bytes, base64url without padding (23 to 107 ASCII
   characters). Decode names strictly as UTF-8 and validate the same grammar;
   preserve every existing ASCII token and tag. Tokens remain positions,
   not authority, and every page still checks access. A previously hidden
   name becoming listable across a server upgrade is a grammar change, not
   a data change; consumers needing one uninterrupted traversal must restart
   after upgrading. An unchanged data version cannot identify a grammar
   upgrade.
9. **Bounds.** Accepted names require no JSON string escapes: ASCII is the
   previous allowlist, and the newly allowed characters exclude quote,
   backslash, and controls. With literal UTF-8 response serialization, the
   conservative existing 7,991-byte listing bound and the 1,064,960-byte
   bounded-read limit therefore still apply: at most 100 names of 64 bytes,
   a 1,039-byte path, and a 107-character token. The maximum percent-encoded
   path alone is 3,117 ASCII characters; query keys and the remaining query
   fields are additional bytes. Existing body limits do not change. Tests
   must verify the actual serializer's output at these maxima, not count
   JavaScript string length as UTF-8 bytes. A serializer change that escapes
   non-ASCII must recalculate these bounds before adoption.
10. **Compatibility surface.** Regenerate the wire descriptions and bump
    the contract minor version when the implementation changes the accepted
    grammar; generated files are not edited by hand. Shapes, status codes,
    schema version, JSON member limit, and page-token layout do not change.
    New clients work with old servers for ASCII paths; Unicode requests to
    old servers remain rejected. Old strict clients may reject new Unicode
    responses or lists. Do not silently filter those responses, promise
    arbitrary mixed-version Unicode interoperability, or add fallback
    normalization. Document upgrading server and client together before
    using non-ASCII paths.

## Implementation slices and acceptance

| Slice  | Observable result and checks                                                                                                                                                                  | Status                 |
| ------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------- |
| R06b-0 | This proposed decision and roadmap, without implementation or changed accepted behavior.                                                                                                      | Proposed specification |
| R06b-1 | Shared domain grammar and strict transport handling; HTTP/MCP, client/CLI, outbox, contracts and listing accept the same names. Verify replay and old rows/tokens as well as rejection paths. | Not started            |
| R06b-2 | Cross-process upgrade/restart and backup/restore evidence, independent guide examples, full required gates and final status update. Depends on R06b-1.                                        | Not started            |

R06b-1 must not ship a server-only expansion whose accepted response names the
bundled client cannot handle. This requires transport/response compatibility,
not new client-side request grammar validation. R06b-2 can be combined with R06b-1 if
that gives one reviewable PR; it does not defer the behavioral tests required
for any persisted behavior introduced by R06b-1.

Use independently authored fixtures covering:

- Korean, CJK, composed/decomposed accents, a supplementary-plane symbol,
  mixed-script lookalikes as distinct keys, and combining marks.
- Every excluded property and explicit scalar, including joiner/variation
  selector sequences; ASCII punctuation still rejected; invalid UTF-8,
  unpaired surrogate, malformed escapes, encoded separators, double encoding,
  plus, empty segment, maximum depth, and 64/65-byte multibyte boundaries.
- Existing JSON holding both composed and decomposed keys, forbidden or
  over-64-byte keys; exact read/set/update/delete and listing without an
  implicit migration or unrelated version/feed change.
- Byte order, all page sizes, continuation after a missing name, invalid
  UTF-8 token names, artifact/path tag mismatch, prior ASCII tokens, maximum
  serialized page/read sizes and percent-encoded query round trips.
- Persisted ASCII request keys and outbox rows surviving upgrade and restart,
  same-key replay after the head advances, different byte paths refusing key
  reuse; an invalid path durably enqueued, recovered unchanged and rejected
  by Rust with 0041 B23's existing explicit disposal; stale bases, no-ops,
  actor isolation, trash/recovery/purge, independent
  clone, source restore independence, and backup reopening.

Important rejection guards must fail when deliberately broken. Run the
repository's required `npm run check`, `npm run build`, relevant integration
and real browser checks, including outbox persistence when affected. Report
fixture, real process, browser, and original evidence separately. Do not call
this implemented until the recorded slices and their gates actually pass.

## Alternatives

NFC-only paths leave some existing keys unreachable and automatic normalization
can redirect access to a different existing key. NFKC or confusable folding can
merge even more names. Exact identity accepts that visually similar names can
coexist and preserves existing JSON meaning. See
[Unicode normalization](https://www.unicode.org/reports/tr15/) for the
normalization forms; Artifree deliberately does not apply one to these keys.

A 256-byte segment would address more existing names, but changes token and
query bounds and is unnecessary for the first Unicode expansion. Retaining
64 bytes preserves those bounds and every ASCII path. JSON Pointer, escaped
slashes, arbitrary punctuation, full emoji sequences, normalization assistance,
and confusable-warning UI can be proposed separately without being claimed
here.
