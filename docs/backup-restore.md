# Backup, restore, and upgrade

Status: implemented for the single-user SQLite server
([decision 0027](decisions/0027-backup-restore-and-schema-upgrade.md)). Not
implemented: scheduled backups, encryption, retention of old backups, downgrade,
PostgreSQL (O05 and later).

## What a backup contains

One file with the whole database: artifacts, every revision and file byte,
request keys, the change feed, and managed token records (hashes only). It does
**not** contain the `access-token` file. Treat the backup like the data
directory: it holds all your artifacts unencrypted. It is created with mode 600
on Unix; on Windows it inherits the directory's access control.

Preview links, runtime grants, and agent handoffs are kept in memory only, so
they are not backed up and do not survive a restart.

## Back up (server may be running)

```sh
artifree-server backup --data-dir /var/lib/artifree --output /backups/artifree-2026-11-01.sqlite3
# {"event":"backup","output":"...","schema_version":5,"bytes":123456}
```

- The command refuses an existing `--output`; give each backup a new name.
- The output directory must exist and its file system must support hard links.
- The snapshot is one point in time: a revision is either wholly in or wholly out.
- It reads a snapshot of the live database; leave the server running.
- Do not copy `artifree.sqlite3` with `cp` while the server runs: the WAL file
  holds recent commits and a plain copy can be inconsistent.

In a container, write into the volume or a mounted directory and copy it out:

```sh
docker exec artifree artifree-server backup --data-dir /data --output /data/backup-2026-11-01.sqlite3
docker cp artifree:/data/backup-2026-11-01.sqlite3 .
```

(`/data` is the image's data directory; the backup file then sits beside the
database, so move it out and delete it from the volume.)

## Restore into a new instance

```sh
artifree-server restore --input /backups/artifree-2026-11-01.sqlite3 --data-dir /srv/artifree-new
artifree-server serve --data-dir /srv/artifree-new --web-dir ...
```

- The target directory must not exist or must be empty; nothing is replaced.
- A backup that is damaged, or whose schema is newer than this build, is
  refused. An older schema is upgraded during the restore.
- The restore does not create an access token. The first `serve` prints a new
  `token_file`; managed tokens from the backup still work unless revoked.
- The change feed keeps its identity. A web page open on the old server reloads
  its state if it was ahead of the backup.
- In a container, restore with the image's entrypoint overridden, into an empty
  volume owned by uid 10001:
  `docker run --rm -v artifree-new:/data -v "$PWD":/in:ro --entrypoint artifree-server artifree:local restore --input /in/backup.sqlite3 --data-dir /data`

Check the result by serving the restored directory and opening the artifacts you
care about before retiring the old instance.

## Upgrade

1. Take a backup (above) with the old version, and keep it and the old package.
2. Stop the server, extract the new package into a new directory, and start it
   with the same `--data-dir`.
3. Opening the database upgrades an older schema (1 to the current) in one
   transaction. If an upgrade step fails, nothing changes and the server does
   not start; the error names the failing statement.
4. A database written by a **newer** version is refused unchanged. There is no
   downgrade: restore the backup into a new directory and run the old version
   on it.

## Not verified

- Backups of very large databases (time, disk use, WAL growth during the copy).
- Network file systems and file systems without hard links.
- Restoring on a different operating system or CPU than the source (SQLite files
  are portable, but this was not run).
- Upgrades from schema versions other than the fixtures in the tests; versions
  before 1 do not exist in a released build.
- The container commands above were not executed against a container runtime;
  the same binary is exercised by the release verification in CI.
