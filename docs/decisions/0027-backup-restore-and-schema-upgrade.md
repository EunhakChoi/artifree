# 0027: Backup, restore, and schema upgrade

Date: 2026-11-01. Status: accepted. Builds on
[0003](0003-local-html-artifact-slice.md) (SQLite storage),
[0023](0023-single-owner-token-management.md) (token records),
[0025](0025-release-packages.md), and [0026](0026-container-image.md).
Roadmap row: O04. Independent Artifree design: the private evidence index lists
no original-product constraint on backup formats or migration, so the choices
below are Artifree's own.

## Context

Until now the only safeguard for a personal server was copying a stopped
server's data directory. Operators need a backup taken while the server runs, a
way to bring it up in a new place with every revision, file byte, and
idempotency key, and a clear statement of what an upgrade does to existing data.
O05 (retention) depends on this, because it deletes data and needs a way back.

## What persists

Only two things are written by the server: the SQLite database
`artifree.sqlite3` and the bootstrap `access-token` file. The database holds
artifacts, revisions, file bytes, request keys, the change feed, and the token
records of decision 0023 (hashes only). Preview tickets, runtime sessions and
grants (R05), and agent handoffs (A08) live in memory and are lost on every
restart, so there is nothing of them to back up. **A new persisted domain must
join the same database (or extend the backup in the same change) and add its
data to the backup tests.**

## Decision

1. **Backup uses `VACUUM INTO`, not the online backup API.** One statement reads
   a single snapshot inside one read transaction. In WAL mode writers are not
   blocked and the snapshot holds exactly the commits complete when it began, so
   a revision, its blobs, its request key, and its feed row are all in or all out.
   The SQLite backup API needs rusqlite's `backup` feature and a step loop that
   restarts when another connection writes. Avoiding a new feature keeps
   `Cargo.toml`, `Cargo.lock`, and `cargo deny` unchanged. The cost is that the
   copy is rebuilt rather than page-copied, which also compacts it. A source of
   a newer schema is refused; one of an older schema is copied as it is and
   restore upgrades it.
2. **The file is written privately and published without replacing anything.**
   The snapshot is written into a freshly created `0700` scratch directory next
   to the destination, so other users cannot read a partly written file whatever
   the umask. It is then verified (`PRAGMA integrity_check`, `foreign_key_check`,
   schema version) and given mode `0600`, and finally hard-linked to the
   destination. A hard link fails if the destination exists, also if it
   appeared after the early check, so an existing file is never replaced and a
   reader never sees a partial file. The scratch directory is removed on every
   path. Consequence: the destination's file system must support hard links
   (ext4, XFS, APFS, NTFS do; some network and FAT file systems do not, and then
   the command fails rather than falls back to a replacing rename).
3. **The access token file is not in the backup; token records are.** The
   database contains the SHA-256 hashes of managed tokens (decision 0023),
   their names, prefixes, and revocation state. Hashes of high-entropy secrets
   cannot be used to authenticate, but a backup still contains **every artifact
   byte and all request history unencrypted**, so it deserves the same
   protection as the data directory. The bootstrap `access-token` is plaintext in
   its own file and is left out on purpose: a backup that leaks must not hand out
   a working owner credential. Restore therefore creates no token; the restored
   directory generates a new one the first time it serves, and managed tokens
   from the backup keep working (unless revoked). Backups are not encrypted
   (out of scope); encrypt the file with the operator's own tool.
4. **Restore only creates.** The target data directory must not exist or must be
   empty. The backup is copied (never opened in place, never modified) into a
   scratch directory inside the target, checked, upgraded with the normal store
   open (which also readies the token table), checked again, and hard-linked to
   `artifree.sqlite3`. A damaged file, a schema newer than the build, or an
   existing database leaves no database behind; a directory the command created
   is removed again. The restored database keeps the feed identity, so change
   cursors from before the backup stay meaningful (0006); a client that was
   ahead of the backup's head sees the existing reset behavior.
5. **Schema upgrade policy.** `PRAGMA user_version` is the schema version
   (currently 5). Opening a database of version 0 to 4 runs all missing steps in
   one `BEGIN IMMEDIATE` transaction; any failure rolls back everything and
   reports the failing SQL error. A newer version is refused without a write.
   There is no downgrade: to go back, restore a backup taken before the upgrade
   into a directory used by the older build. **The open now switches to WAL after
   the migration commits, not before.** Switching rewrites the file header, so the
   earlier order changed a refused or failed database's bytes. Now a refused or
   failed open leaves the file byte-identical, which the tests check.
6. **The token table stays unversioned.** `tokens` is created with
   `CREATE TABLE IF NOT EXISTS` and has had one shape; it is not part of
   `user_version`. Giving it a version now would bump the schema for no change
   in behavior and make every existing database run a migration. Tests show a
   database that predates the table and one that held only tokens both open.
   The first change to that table must put it under `user_version` and add a
   fixture; this is a recorded obligation, not a silent gap.
7. **CLI.** `artifree-server backup --data-dir <dir> --output <file>` and
   `artifree-server restore --input <file> --data-dir <new-dir>` print one JSON
   line on stdout (`event`, `schema_version`, `bytes`, plus `output` or
   `data_dir`) and diagnostics on stderr; failures exit 1 with nothing on stdout.
   The logic lives in `artifree-providers::backup` (next to the SQLite code) and
   `artifree-server::data_backup`, so tests exercise it without the binary.

## Tests

- Backup during writes: a writer commits 40 revisions while a backup starts after
  the tenth. Restore holds contiguous revisions 1..=N with the exact bytes and
  digests of each, request keys of kept revisions replay to the original receipt
  with the same body and conflict with another, and the key just past the
  snapshot is unused. Renames, pins, the trash state, the feed identity and
  earlier changes, and token records (including a revoked one) survive.
- Refusals: existing output, missing database, newer-schema source, existing
  restore target, newer-schema backup (backup file untouched), non-database
  file; owner-only modes on Unix.
- Upgrade fixtures, written out as released (not derived from current code) for
  versions 1 to 4, with an artifact, two revisions, blobs, a request key, and (from 2) a feed with head 5 and (from 3) metadata revisions: each upgrades to the same
  normalized `sqlite_master` as a fresh database, keeps bytes, digests, the pin,
  the key replay, and the feed head, and passes integrity and foreign key checks.
  Missing, zero-byte, and version-0 files become the fresh schema.
- Failure injection: a pre-existing `changes_v5` table makes the last step fail
  after earlier steps ran, from each starting version. The file bytes, directory
  listing, and `user_version` are unchanged and the error names the table; the
  same file upgrades once the obstacle is gone.
- Newer schema (rollback-journal and WAL files): refused, bytes and directory
  listing unchanged.
- CLI: a real `serve` process, a backup taken while it runs, a restore, and a
  second `serve` that replays the first request key and rejects another body.
  `scripts/verify-release.mjs` repeats this on the packaged binary in CI.

## Not decided or verified

Backup retention, scheduling, encryption, and pruning of old request keys (O05);
PostgreSQL; backups of a database on a network file system; concurrent
`restore` into the same directory (two processes could race on the final link,
and the loser fails with an existing-file error); downgrade. Restore of a
snapshot taken during a hard crash relies on SQLite's own recovery on the source.
Throughput and time of a backup of a very large database were not measured; the
copy holds a read snapshot (and so keeps WAL growth from being checkpointed past
it) for as long as it runs.
