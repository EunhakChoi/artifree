# 0027: Backup, restore, and schema upgrade

Date: 2026-11-02. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md), [0006](0006-change-feed.md) (decision
11), [0023](0023-single-owner-token-management.md),
[0025](0025-release-packages.md), and [0026](0026-container-image.md). Roadmap
row: O04. Independent Artifree design: the private evidence index lists no
original-product constraint on backup or storage migration for this row.

## Context

Every persisted table lives in one file, `artifree.sqlite3`, in the data
directory. Until now an operator could only copy the stopped server's
directory, and nothing checked that the copy, or a database from an older
release, would open. The roadmap asks for a consistent backup of a running
server, a restore on a new instance that keeps revisions, bytes and request-key
replay, a verified upgrade from the supported older schemas, and a refusal of
newer or failed schemas that does no damage.

## Decision

1. **One owner of the schema.** `artifree-providers/src/schema.rs` is the only
   code that creates or upgrades tables. The access token table, which the
   token store used to create on its own, is the version 6 step of the same
   ladder (`CREATE ... IF NOT EXISTS`, because every version 5 database from a
   server that ran already has it). `SqliteTokenStore::open` creates and alters
   nothing: it requires `user_version` to be the current version, which
   `SqliteStore::open` establishes first, and opens the file read-write
   without creating it, so on a newer, older, foreign or missing file it fails
   and leaves it as it was. Backup and verification contain no special case
   for tokens; a malformed token table is a schema difference like any other.
2. **An open that refuses changes nothing.** The ladder runs in one immediate
   transaction before `journal_mode=WAL` is set, because the switch rewrites
   the file header. A newer version, a non-empty file with `user_version` 0,
   and a failing step leave the file byte for byte as it was, in
   rollback-journal and in WAL mode, with no `-wal`, `-shm` or `-journal`
   left. An empty or missing file is a new database, which is how `serve`
   starts.
3. **Backup method: `VACUUM INTO`.** It reads the database in one read
   transaction, so under WAL the result is a consistent snapshot while commits
   continue, and it needs no extra Cargo feature or dependency. Alternatives
   rejected: copying the file (misses the WAL and can tear), and the SQLite
   online backup API (it restarts when the source changes during the copy and
   so may not finish under steady writes, and rusqlite gates it behind another
   feature). The snapshot is written into a private (0700) scratch directory
   beside the output, so that the final step is a link on the same file system,
   and is switched to the rollback journal so it is one self-contained file.
   Whether writers are delayed by a backup is SQLite WAL behaviour that was
   not measured here; no latency claim is made.
4. **A backup is proven restorable before success.** The snapshot goes through
   the restore pipeline: version in range, `integrity_check` and
   `foreign_key_check`, upgrade of a copy when the snapshot is older (the
   integrity checks run again only then), and a comparison with the schema this build
   creates, taken from an empty in-memory database upgraded by the same
   ladder. There is no on-disk reference. **The comparison is structural,
   not textual.** `ALTER TABLE ... RENAME` makes SQLite rewrite the stored
   `CREATE` text, and the rewrite may differ between SQLite versions, so a
   future `libsqlite3-sys` bump would otherwise make backup and restore refuse
   genuine databases (every released build used 0.38.2, so today the texts
   agree). The comparison uses SQLite's own introspection: objects by kind and
   table; for each table its columns (type, not-null, default, primary key,
   hidden), STRICT and WITHOUT ROWID, indexes with their columns, uniqueness
   and partiality, and foreign keys. A few clauses have no introspection:
   `CHECK` expressions, `AUTOINCREMENT` (which decides whether the change
   feed's sequence is reused), `COLLATE` names, `ON CONFLICT` algorithms, and
   the `WHERE` of a partial index. They are read from the stored text with a
   tokenizer that skips quoted names, literals and comments and works on
   characters, and compared as written; that is the one textual part, and a
   rewrite that changed such a clause itself would need the comparison
   revisited. The expression of a generated column is not read (no table has
   one). The table-valued pragma functions need SQLite 3.37; the bundled
   library is newer. The reference is built once per process. A test accepts a database whose stored text was rewritten with the same
   structure and refuses dropped checks, foreign keys, and changed defaults. The feed table must hold exactly one row. A snapshot at the
   current version is checked in place and not copied. Refused: version
   below 1 (empty, truncated, another application's database), above the
   current one, damaged, a foreign key violation, or a schema that differs
   (missing or extra table or index, a table with other columns), and an
   upgrade that fails. The output **keeps the schema version it was taken
   at**: a backup is the data as it was, a restore upgrades. A newer build can
   restore any supported older backup; an older build refuses a newer one.
5. **Opening the source.** The source is opened read-write without creating
   it. A stopped server's cleanly closed database, in WAL or rollback mode, is
   left as it was, with no leftover `-wal` or `-shm` (a read-only open of a WAL
   database does leave them). A database left with a WAL by a crash gets
   SQLite's normal recovery, which writes to it; byte-for-byte preservation
   is not claimed in that case. Backup and restore must therefore run as the
   server's operating-system user, and on Unix backup enforces it: before it
   opens the database it learns the invoking user from the owner of a file it
   creates, for a moment, beside the database (the standard library has no
   `geteuid`, and no dependency was added for one; being on the database's own
   file system, ownership means the same for both), compares it with the
   database file's owner, and refuses a database of another user, root
   included, or a directory it cannot write in, so it cannot leave recovery
   files owned by the wrong user in the server's data directory. The snapshot
   is taken only from a UTF-8 database, because the path is passed to
   `VACUUM INTO` as bytes.
6. **Publishing.** The finished file is `sync_all`ed, and given mode 0600 on
   Unix. It is published with a hard link, which fails instead of replacing
   anything, so an existing destination, a symbolic link included, is refused
   (backup also checks first, to fail before the snapshot is taken). Then the
   scratch directory is removed and the parent directory is fsynced (Unix;
   Windows has no standard-library directory flush, so there the entry is as
   durable as the file system makes it). A file system that cannot flush a
   directory (`EINVAL` or `ENOTSUP`, as some FUSE, SMB, and NFS mounts answer)
   is not a failure, because the file itself was flushed; the entry is then
   only as durable as that mount makes it. Any other flush error fails. If
   anything after the link fails, the linked destination is removed before the
   error is returned, so a failure never leaves the output behind and a retry
   works. The scratch directory is removed on every error path; a killed
   process leaves a directory named with the prefix `.artifree-scratch-`,
   which is safe to delete when no backup or restore runs. The file system
   must support hard links.
7. **Restore.** The target data directory must not exist (a restore makes a
   new instance and never merges); it is created with the policy of
   `prepare_data_dir`, which moved to the providers crate so `serve` and
   `restore` share it. The input is only read: it is copied into a scratch
   directory inside the target as a new owner read-write file (a backup kept
   at mode 0400 is a valid input), run through the shared pipeline (check,
   upgrade on the pipeline's own connection, check, compare), and given a **new random change-feed identity**
   (decision 0006, 11). Only `feed.id` changes; the `AUTOINCREMENT` head in
   `sqlite_sequence` is untouched, so sequences never go back and cursors of
   the original feed get `reset`. An input with stale `-wal`, `-shm` or
   `-journal` files beside it is refused. **Every file backup and restore
   publish is in the rollback-journal mode** (`journal_mode=DELETE`, header
   bytes 18 and 19 equal to 1), one self-contained file with no sidecar,
   whatever the input's version or journal mode was; the upgrade does not
   switch the journal mode, and the server switches to WAL when it opens the
   database. A target below a regular file is reported as that, not as an
   existing target. On any failure everything restore
   created, the target included, is removed (missing parents it created
   remain). The access token is not part of a backup; the first `serve`
   creates one. Preview tickets, runtime grants and sessions and handoffs are
   in memory and are not backed up.
8. **CLI.** `artifree-server backup --data-dir D --output F` and
   `artifree-server restore --input F --data-dir D`. Each prints one JSON line
   on stdout and diagnostics on stderr; `--help` prints usage and exits 0; an
   unknown, missing or repeated flag, or an unusable value, exits 1. The two
   commands read their arguments as operating-system strings, so on Unix a
   path that is not UTF-8 works (the snapshot path is bound to `VACUUM INTO`
   as bytes and cast to text; on Windows SQLite opens UTF-8, so a path that is
   not valid Unicode is refused when the snapshot is taken). An empty value
   for a path flag is refused at parse, and an argument of another command that is not UTF-8
   is reported instead of panicking. The database file name is the constant
   `DATABASE_FILE`, shared with `serve`.
9. **Maintenance writes outside `ArtifactService`.** AGENTS.md requires
   mutations to go through `ArtifactService` with a base revision and an
   idempotency key. Restore writes in two ways that do not: the schema upgrade
   and the renewal of `feed.id` (`UPDATE feed SET id = ...`). Neither is an
   artifact mutation: they do not create, revise, or change any artifact,
   revision, or request key, so a base revision and a key have nothing to
   guard. They cannot race with a request because they run on a private
   scratch copy in a directory created by this process (mode 0700, random
   name) before any server has the file open, and the copy is published, by a
   no-replace hard link, only after every connection to it is closed. The
   server's own start-time upgrade is the existing ladder (decision 0003) and
   is unchanged. Backup writes nothing to the live database; it only reads it
   (a crash-left WAL is recovered by SQLite itself, decision 5). The
   connections that write use the store's own settings (busy timeout,
   foreign keys on, `synchronous=FULL`, no create) through one shared
   function; the token store keeps its own 5 second timeout and no foreign
   keys (its table has none) because it is not one of those connections.
10. **Later domains extend the backup.** A new persisted table is added as a
    ladder step in `schema.rs`; the backup and the schema comparison then
    include it with no further change. Anything stored outside the database
    file (object storage, X03) needs its own entry in `docs/backup-restore.md`
    before it ships.

## Rewrite after review

The first implementation (closed pull request #57) failed review three times.
Its reviews found: a build that failed on Windows; a backup that left WAL files
and changed the source; missing fsync; databases with version 0 or an
incomplete schema accepted; a stale `-journal` copied silently; a token store
that created tables, or changed the file, before the version check; a backup
that accepted a malformed token table which a restore then rejected; and a
failed directory flush that left the published file behind. The causes were
design, not detail: the token table lived outside the schema, so backup needed
a special case for it; backup and restore ran different checks; and publishing
had no rule for failure after the link. Decisions 1 and 2 give the schema one
owner and make refusals inert, decision 4 makes backup run the restore
pipeline so that every successful backup restores, decision 6 states the
cleanup rule for publishing, and decision 7 renews the feed identity that 0006
required.

## Tests

Behavioral tests, in `artifree-providers/tests` unless noted:

- Fixtures of schema versions 1 to 5, built from frozen copies of the historical
  schema texts with data in every table, upgrade to a schema equal to a fresh
  one and keep their artifacts, bytes, request keys, feed head, trash and tokens.
- Refused opens (newer, foreign, failing first and last steps) leave all files
  byte-identical, in rollback and WAL mode; the token store alone does the same.
- Backup and restore refuse each bad database in a table of fourteen cases and
  leave no output, target or scratch behind and the input unchanged.
- A backup taken while a thread commits restores a contiguous, non-empty prefix
  with all revisions' bytes and digests, metadata, trash state and token
  records; key replay returns the original receipt and different content stays a
  conflict.
- The restored feed identity differs, the head is kept, and through the router
  (`artifree-server/tests/change_feed.rs`) a cursor of the original gets `reset`.
- The publish failure path removes the destination (unit test in `backup.rs`).
- Command-line behavior of the binary (`artifree-server/tests/backup_restore_cli.rs`),
  and `scripts/verify-release.mjs` against the packaged binary: back up the
  running server, restore, serve, read the saved revision, refuse existing paths.

## Costs and limits

- The backup is a full copy each time; there is no incremental, scheduled or
  encrypted backup, and no retention of old backups (O05 covers data
  retention, not backup files).
- Restore does not replace an existing instance; the operator switches
  `--data-dir`.
- A backup cannot be restored by an older build, and there is no downgrade.
- `VACUUM INTO` needs free space for the snapshot and, for an older snapshot,
  for the upgraded copy, beside the output.
- Not run: Windows execution outside CI, hard-link-less file systems, a
  database over 1 GiB, a crash during a backup (only error paths were
  exercised), and the container commands. See `docs/backup-restore.md`.
