# 0031: Application data per actor and artifact

Date: 2026-11-02. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) (request keys),
[0004](0004-inbound-mcp-adapter.md), [0012](0012-duplicate-artifacts.md),
[0013](0013-deleting-artifacts-and-the-trash.md),
[0020](0020-runtime-grants.md),
[0023](0023-single-owner-token-management.md),
[0027](0027-backup-restore-and-schema-upgrade.md),
[0029](0029-retention-and-cleanup.md), and
[0030](0030-persistent-jobs-and-worker.md) (decisions 3 and 4). Roadmap row: R06.
Independent Artifree design: the private evidence index gives this row only
the idea of a data store that is separate from an app's source and versioned on
its own, and names the path grammar, the patch rules, the limits, and every
error as Artifree's own decisions. Nothing below is an observation of the
original product.

> Amended by [0040](0040-data-changes-and-member-listing.md) (proposed; takes
> effect when the slice merges): decision 14, as a data write that changes the
> space records a `data_changed` change in the feed (R07a).

## Context

An artifact is a package of files, and each revision of it is immutable. An app
also needs somewhere to keep what its user does: a list of todos, a score, a
setting. That state has a different life from the source: it changes far more
often, it must not be rewound by restoring a revision, and it must not leak
into an export. Roadmap R06 asks for it with its own version, per actor and
artifact, and for the pieces that make writes safe to retry and safe to
repeat: conditional writes, key replay, a defined merge, and a copy that is
independent.

This record adds the server side only. **No page can read or write this data:
ADR 0020 grants a frame exactly `metadata.read`, the runtime SDK is types only,
and previews run with no capability. Reaching data from a frame is a runtime
bridge and belongs to a later row.** What exists is HTTP, MCP, the client, and
the CLI, with the owner's token or an issued agent token.

## Decisions

1. **One data space per actor and artifact, with its own version.** A space is
   one JSON object. `data_version` is 0 for a space nobody wrote, with no row
   behind it, and goes up by 1 for every write that changes the space. It never
   resets, not even when the space is emptied. It is unrelated to the source
   revision and to the metadata revision: saving, restoring (A04), renaming,
   pinning, trashing, and recovering neither change nor read it.
2. **Paths.** A path is `/`-separated segments, each `[A-Za-z0-9_-]{1,64}`,
   at most 16 segments. The empty path is the root. There is no leading,
   trailing, or doubled `/`, no `.` or `..`, no whitespace, no percent
   encoding, no non-ASCII. A path addresses object members only: it never
   enters an array, so `list/0` does not exist as a path. A path error names
   the rule and the byte offset and never echoes the path.
3. **Values are strict JSON, canonicalized.** The domain parses the exact text
   it is sent (RFC 8259, no extensions) and writes one canonical form: compact,
   members sorted by their UTF-8 bytes, escapes `\" \\ \b \f \n \r \t` and
   `\u00xx` for other control characters. Member order is therefore not
   preserved. A value is refused when it has a repeated member name at any
   depth (also when spelled with escapes), a member name that is empty or over
   256 bytes, a lone surrogate escape, or a raw control character in a string.
   **Numbers: every number must be finite and have a magnitude of at most
   2^53 - 1, in whatever spelling.** An integer literal beyond that bound
   (including `-9223372036854775808` and `18446744073709551615`) is refused,
   and so is a float spelling of such a number: `9007199254740993.0`,
   `9.007199254740993e15`, and `1e16` are refused as `10000000000000000` is
   (every double beyond the bound is a whole number, so the rule has no gap).
   `1e999` is refused as not finite. A fraction too small for a double reads as
   `0` (`0.1e-400` is stored as `0`). A number that is whole and in range is
   stored as an integer (`1.0`, `1e2`, and `-0` become `1`, `100`, and `0`),
   and any other as the shortest double text that reads back the same. The
   bound is a range check, not `abs`, so the extremes of the integer types are
   refused and cannot overflow. The text, not a parsed value, is what is
   checked, so that a duplicate or an integer a JavaScript parser would
   silently change is seen. The domain has no serde and no I/O.
4. **Limits.** A value or patch is at most 256 KiB of text (`MAX_VALUE_BYTES`).
   A space is at most 1 MiB canonical, 10,000 values below the root (every
   array element and member counts), and 32 levels of nesting with the root
   as level 1. The nesting limit also bounds the parser's recursion, so
   100,000 opening brackets are refused, not recursed into. All four fail with
   `422 data_too_large`.
5. **Four operations.**
   - **get** reads the value at a path. A path with nothing at it, or one that
     goes through a value that is not an object, is a successful `200` with
     `exists: false` and no `value`. The root always exists. A stored `null`
     reads as `exists: true, value: null`.
   - **set** replaces the subtree at the path and makes the missing objects
     above it. If a value above is not an object it fails with `409
data_path_conflict` and nothing is written. The root can be set only to an
     object. `null` can be stored by a set.
   - **update** is a JSON Merge Patch (RFC 7396) at the path, which is a merge
     of the nested patch at the root. A `null` member removes that member;
     arrays and scalars replace what is there wholesale; nothing inside an
     array is merged and a `null` in an array stays. An object patch over a
     value that is not an object starts from `{}` and drops its `null`
     members. A `null` patch at a path removes the target and never creates
     parents or conflicts. The root patch must be an object. **A merge patch
     cannot store `null` in an object; a set can.**
   - **delete** removes the subtree. A path that is not there is a no-op that
     keeps the version. Deleting the root empties the space.
6. **Every write is conditional and keyed.** A write carries
   `base_data_version` and an `Idempotency-Key`. In one `BEGIN IMMEDIATE`
   transaction, in this order: the artifact is in place; the key is looked up
   (a replay answers with the **original** version and `changed`, and
   `replayed: true`; a different request, or a key another kind of request
   used, is `422 request_key_reused`); the base is compared (a stale or a
   future base is `409 data_conflict` and the error body carries
   `current_data_version`); the change is applied; the row and the key are
   written. A refused request records no key. **A write that changes nothing
   keeps the version** (`changed: false`) and still records its key, and its
   base is checked first, so a stale no-op is a conflict. The fingerprint is
   over the operation, the artifact, the base, the canonical path, and the
   canonical value or patch. The write time comes from the injectable clock.
7. **The key space is one per actor across five tables:** `request_keys`,
   `cleanup_runs`, `job_requests` (0030), and the new `data_requests`. A key
   used by one cannot be used by another, in either direction, checked in the
   same transaction. A single list (`KEY_TABLES`) is used by all four writers
   and a unit test compares it with every table that has a `request_key`
   column, so a table added later without joining the space fails.
8. **Access follows reads.** The artifact must exist, be outside the trash and
   not purged, and belong to the actor; otherwise every data route is `404
not_found`, **a retry of a request made before the artifact went to the
   trash included** (as in 0030, unlike 0013). **Issued agent tokens may read
   and write data over HTTP and MCP.** They authenticate as the owner (0023),
   every write is bounded, conditional, and keyed, and an agent that keeps
   application state needs this. The cost is stated: data has no history, so an
   overwrite or delete is not undoable. An owner who wants a snapshot can
   duplicate with `copy_data` (decision 10). Each new route is tested with an
   issued token.
9. **HTTP.** `/v1/artifacts/{id}/data`:
   - `GET ?path=` (optional; strict query, an unknown or repeated parameter is
     `400`) answers `AppData {artifact_id, path, data_version, exists, value?}`.
   - `PUT {path, base_data_version, value}`, `PATCH {path, base_data_version,
patch}`, and `DELETE {path, base_data_version}` (a body, as the cleanup
     route has one) answer `200 DataWriteResponse {artifact_id, path,
data_version, changed, replayed}`. `path` is required in a write; the
     explicit empty string is the root.

   `value` and `patch` have the contract type `JsonValue`, held by the server
   as raw text so that decision 3 applies to what was sent. The checks run in
   this order: artifact identifier (`404`), `Idempotency-Key` (`400`), body
   shape (`400`), path and value (`422 invalid_request`, or `422
data_too_large`), then artifact, key, and base. **Text inside the body that
   is not JSON at all** (`"value": [1,}`) breaks the body and is `400
invalid_request`, like any malformed body; text that is JSON but breaks the
   rules of decision 3 or 4 is `422`. MCP gives the same codes: a `value` or
   `patch` string that is not JSON is `invalid_request` (internally the same
   `400`), and one that breaks the rules is `invalid_request` or
   `data_too_large` as over HTTP. MCP results carry the code and not the HTTP
   status, so the code is what the two share. These routes have their own
   body bound, the value limit plus 8 KiB, answered `413 payload_too_large`;
   the request rate and concurrency limits apply as everywhere else. New error
   codes: `data_conflict` (`409`), `data_path_conflict` (`409`), and
   `data_too_large` (`422`); `ErrorBody` gains `current_data_version`.

   **`DELETE` carries its request in a JSON body**, as the content cleanup does.
   The client and the CLI always send one. Some proxies, tunnels, and HTTP
   clients drop the body of a `DELETE`; the server then sees a request with
   no body and answers `400 invalid_request` (a malformed body), **never a
   deletion**: a delete is only ever performed for a body that names the path and
   the version. Where bodies cannot be sent, `PATCH` with a `null` patch at
   the same path removes the same value, **for a path below the root only**:
   a `null` patch at the root is `RootNotObject` (`422 invalid_request`) because
   the root must stay an object (decision 5). The root is emptied with `PUT`
   of `{}` at the empty path, which carries a body (a CLI test asserts both),
   and the CLI says
   so in `artifree data --help`. A deletion has no query-string form: a
   version and a destructive path in a URL would be logged and cached.

10. **MCP.** `get_data` (read), and `set_data`, `update_data`, `delete_data`
    (a new effect `data`, all requiring `idempotency_key`). **The three write
    tools set `destructiveHint: true`**, because nothing keeps what they
    replace; every other tool keeps `false`. Their `value` and `patch`
    arguments are **JSON text strings**, not nested JSON: an MCP client or SDK
    parses a nested value before the server sees it and would merge a repeated
    name. `duplicate_artifact` gains `copy_data`.
11. **Duplicate (A05, 0012).** `DuplicateArtifactRequest.copy_data` (default
    `false`, also an MCP argument). When true, the copy receives **a copy of
    the copying actor's space**, as of the moment the copy commits, whichever
    revision the files come from, in the same transaction that creates the copy.
    The copy's space starts at version 1 and is independent in both
    directions. An empty source space copies nothing and leaves the copy at
    version 0, and `copied` still says `["files", "data"]` because it was
    asked for. A replay does not copy again. The fingerprint adds `copy data`
    only when it is true, so keys of requests made before this change keep
    their fingerprints. Another actor's space is never copied. The web
    duplicate dialog is unchanged and copies files only.
12. **Trash, purge, backup.** The data of an artifact in the trash is hidden
    (`404`) and kept; recovery brings back access, the data, and the version.
    **A purge deletes the artifact's `app_data` rows and blanks the
    `data_requests.fingerprint` of its keys** (the fingerprint hashes the
    written value) inside the purge transaction; the key rows stay, so keys
    stay reserved (0029). Data writes and the purge run with `secure_delete`
    on, so a replaced, removed, or purged value is overwritten in the free
    pages and is not left readable in the database file. **Caveats, stated:**
    the write-ahead log is not truncated by this; a backup (0027) taken before
    a purge still holds the data and restores it; and the filesystem or a
    snapshot below SQLite may keep copies. Backups contain the data and keys of
    the live database; a restore refuses a backup whose `app_data` or
    `data_requests` table is missing or without its checks.
13. **Schema version 9.** `app_data(artifact_id, actor, data_version >= 1,
document TEXT CHECK(json_valid(document) AND json_type(document) =
'object'), PRIMARY KEY (artifact_id, actor)) STRICT` and `data_requests(actor,
request_key, fingerprint BLOB of 32 bytes, artifact_id, data_version >= 0,
changed IN (0, 1), created_at, PRIMARY KEY (actor, request_key)) STRICT,
WITHOUT ROWID`. Fixtures of versions 1 to 8 upgrade, a failed step rolls
    back, and an older backup restores and upgrades (0027). The binary-column
    guard of the store tests lists `data_requests.fingerprint` and nothing else
    new.
14. **Not in the change feed.** A data change adds no change kind to
    `/v1/changes`, and the web reducer is unchanged. A client polls `GET data`
    and compares `data_version`. Subscriptions belong to R07. Data is not in a
    revision, an export (A07), the `package`, or the content-cleanup
    references (0029): it holds no content digest.
15. **Surface.** Contract version 0.9.0: `AppData`, `JsonValue`, `ReadDataQuery`,
    `SetDataRequest`, `UpdateDataRequest`, `DeleteDataRequest`,
    `DataWriteResponse`, `DuplicateArtifactRequest.copy_data`,
    `CopiedPart` `data`, the three error codes and `current_data_version`, the
    four MCP tools, the client methods `getData`, `setData`, `updateData`,
    `deleteData` (`JsonText` sends text exactly as written; a JavaScript value
    is serialized), `duplicateArtifact` with `copy_data`, and the CLI
    `artifree data get|set|update|delete` and `artifree artifact duplicate
--copy-data`. The CLI embeds the text it is given in the request body
    unparsed. The client rejects a response whose shape or `copied` part it
    does not know as an invalid response. `AppData.value` reads a stored
    `null` as present (`exists: true` with the text `null`) and a value that
    is not there as absent, in Rust as in TypeScript (a round-trip test of the
    wire contract). The client refuses, before sending, a JavaScript value that
    `JSON.stringify` would change silently or fail on (`NaN`, `Infinity`,
    `undefined`, functions including a `toJSON` function, symbols, `bigint`, a
    `Date`, a `Map`, a class instance, an array with holes or extra properties,
    a getter, a non-enumerable or symbol-keyed property, a cycle, more than 64
    levels). It checks a copy that reads every property once and sends that
    copy, so a getter or proxy cannot change the value after the check. Plain
    objects and arrays of another realm are accepted (an object is plain when
    its prototype is `null` or has none), `-0` is sent as `0`, and a `toJSON`
    member that is not a function is data. It also refuses a `base_data_version` that is not a whole number from
    0; `JsonText` is the path for exact text. A CLI value that starts with `-`
    is written `--value=-5`.

## Limits and what is not implemented

- **Frames and previews cannot read or write data.** There is no runtime
  bridge, no capability, and no data panel in the web workspace.
- No subscription or change notification (R07), no query (R07), no uploaded
  assets (R09), no sharing between actors, and no per-collection or array
  operations. A path never enters an array, so an app that needs to change one
  element sets the array whole or keeps its items as members.
- No history of data, no undo, and no export of it. A duplicate with
  `copy_data` is the only snapshot.
- Erasure is best effort at the SQLite level (decision 12); it is not a
  statement about backups, the WAL before it is truncated, or the disk.
- Limits are per space and fixed; an actor can have a space for each of its
  artifacts, and nothing bounds their number in total.
- A write replaces the space's row whole, so a write costs the size of the
  space (at most 1 MiB).

## Tests

`crates/artifree-domain/src/data.rs` (path grammar, the parser, canonical form,
numbers, duplicates, the merge patch rules of RFC 7396, limits, a bound on
recursion), `crates/artifree-providers/tests/data.rs` (versions, independence
from source and metadata, stale and future base, no-op, refused change leaves
the key free, lost-response replay, key reuse, the shared key space in both
directions, per-actor spaces, trash and recovery, purge with a raw-file erasure
check after a replace, a remove, and a purge, a two-writer race, copy with and
without data and its independence, row constraints), `schema_upgrade.rs` and
`backup_restore.rs` (version 8 fixture, a failed step, data across a restore,
a backup older than a purge), `crates/artifree-server/tests/data_api.rs`
(every route requires the token and works with an agent token; reads, sets,
updates, deletes, canonical form; conflicts with `current_data_version`;
replay and key reuse; the shared key space against artifact and job requests;
concurrent writers; path, value, body, and query refusals; limits including
absurd nesting; trash, purge, recovery, and restore; duplicate with and without
`copy_data`), `authorization.rs` (another actor), `mcp_adapter.rs` (every data
tool, the 404 rule, annotations), and the TypeScript client, CLI, MCP, and
server tests, including a real server restart.

## Alternatives

- **JSON Pointer paths or dotted paths.** Pointer escapes (`~1`) and array
  indexes bring cases this row does not need; the plain segment grammar can be
  extended, a looser one cannot be narrowed.
- **Parsing the value with serde and re-serializing.** It merges repeated
  names, accepts integers beyond 2^53 into a float, and loses what a client
  meant; the domain parser takes the text.
- **Writing a subtree with its own version.** One version per space makes a
  conditional write simple and a conflict meaningful; finer versions are a
  later, additive change.
- **Last-writer-wins without a base.** The same app opened twice would lose
  writes silently. The base costs one extra read.
- **Admin-only agent tokens for writes (as purge).** Purge and cleanup cannot
  be undone for the whole store; a data write is bounded to one space and
  keyed. The cost is decision 8.
- **Data in the change feed.** It would add a change kind and a reducer to
  every client for state that R07 will subscribe to with a better contract.
- **Copying data on every duplicate.** Data is personal state, and a copy that
  starts with it is surprising; asking is explicit.

## Costs

A schema step, two tables that a purge and a restore must handle, a third
vocabulary of conflicts that clients must tell apart, a write that rewrites
the space, a caution that agents can overwrite without undo, and a promise
that the path grammar and the canonical form stay stable.
