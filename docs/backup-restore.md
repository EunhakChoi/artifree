# Backup, restore, and upgrade

Status: O04, implemented ([decision 0027](decisions/0027-backup-restore-and-schema-upgrade.md)).
Everything the server persists is in one SQLite file, `artifree.sqlite3`, in
the `--data-dir`: artifacts, revisions and file bytes, request keys, the change
feed, titles, pins and the trash, and the access token records. `backup` copies
that file consistently; `restore` builds a new data directory from the copy.

Not part of a backup, by design:

- the `access-token` file. A restored directory has none; the first
  `serve` creates a new one. Clients that kept the old token must read the new
  file. (The token _records_ of the token management page are in the database
  and are restored.)
- preview links, runtime grants and sessions, and agent handoffs, which live in
  server memory and are lost on every restart anyway.

## Back up

```sh
artifree-server backup --data-dir /path/to/data --output /path/to/backups/2026-11-02.sqlite3
```

The server may be running. The command takes a consistent snapshot, then proves
that the snapshot can be restored (it runs the same check, upgrade and schema
comparison that `restore` does) before it reports success. On success stdout is
one JSON line, `{"event":"backup","output":...,"schema_version":...,"bytes":...}`;
failures are one line on stderr and exit status 1, and leave no output file.
`restore` prints `{"event":"restore","data_dir":...,"schema_version":...,"bytes":...}`.

- **Paths that are not UTF-8.** Then `output` (`data_dir` for `restore`) is the
  path converted lossily, so it does not name a real file, and the line also
  has `"path_utf8":false` and the exact path: `"path_bytes"`, an array of the
  path's bytes, on Unix, or `"path_wide"`, an array of UTF-16 code units, on
  Windows. A UTF-8 path has neither extra field. A script that needs the path
  should use the value it passed in.
- **Closed or failing stdout.** The result is written once the backup is
  published and proven, and nothing written afterwards can remove it. A closed
  stdout (a pipe whose reader went away) is ignored and the exit status stays 0.
  Any other failure to write the line (for example a full disk) exits 1 with a
  message on stderr that says the backup succeeded and was kept.

- `--output` must not exist; nothing is ever replaced. Its directory must exist,
  be writable and support hard links, and have space for the snapshot (plus
  one more copy when the database is older than this build).
- The file has mode 0600 on Unix and keeps the schema version of the database
  it was taken from; `restore` upgrades it.
- A backup is refused, with nothing written, for an empty, truncated or
  foreign database; a newer schema than this build supports; a failed
  integrity or foreign key check; a schema that differs from the one this
  build creates; and an upgrade that does not succeed.
- Run it as **the operating-system user that runs the server**. The command
  opens the database for reading and writing (it never creates one). Opening a
  database that a crashed server left with a WAL performs SQLite's normal
  recovery, which writes to it; a cleanly closed database is left as it was.
  On Unix the command refuses a database file that belongs to another user
  (root included) before it opens it, so it cannot leave recovery files with
  the wrong owner in the data directory. Paths that are not UTF-8 work on
  Unix; on Windows they must be valid Unicode.
- The command works in a directory named `.artifree-scratch-<random>` beside the
  output (inside the new data directory for `restore`) and removes it on every
  failure the process survives. If the process is killed (SIGKILL, power loss)
  that directory stays; delete such leftovers when no backup or restore is
  running.
- If the scratch directory cannot be removed after the backup was published
  and flushed (a permission or file-system error), the backup is kept and the
  command still succeeds and prints its JSON line. The removal is retried once;
  if it fails again stderr gets one `warning:` line that names the leftover
  `.artifree-scratch-<random>` directory and the reason, and the exit status
  stays zero. Check stderr for it, and delete the directory when no backup or
  restore is running. For `restore` the leftover is inside the new data
  directory. The removal is not flushed, so a crash right after a successful
  run can leave the directory without a warning.
- On Unix the finished file and the directory entry are flushed to disk before
  success is reported. Windows has no standard-library directory flush, so
  there the entry is as durable as the file system makes it. On a file system
  that cannot flush a directory (some FUSE, SMB, and NFS mounts) the backup
  still succeeds, since the file itself was flushed, and the entry is only as
  durable as that mount.
- The file is in the rollback-journal mode: one self-contained file with no
  `-wal` beside it.

Copy the file off the machine yourself. Backups are not scheduled, encrypted
or rotated.

## Restore

```sh
artifree-server restore --input /path/to/backups/2026-11-02.sqlite3 --data-dir /path/to/new-data
artifree-server serve --data-dir /path/to/new-data ...
```

- `--data-dir` must **not exist**. Restore creates it (mode 700 on Unix, like
  `serve`) and refuses an existing path. To replace an instance, restore into a
  new directory, start the server on it, and keep the old directory until you
  are satisfied.
- The input is only read, so a backup kept read-only (mode 0400) works. The
  restored file is in the rollback-journal mode; the server switches it to WAL
  when it starts. Files named `<input>-wal`, `<input>-shm` or
  `<input>-journal` beside it are refused.
- The database is upgraded to the current schema, checked, and given a new
  change-feed identity: open web workspaces and clients that followed the
  original feed receive a `reset` and read their state again. Sequence numbers
  continue; they never go back.
- Request keys are restored, so a client retrying a request that committed
  before the backup gets its original result, and a key reused for different
  content is still a conflict. Commits made after the backup are not there.
- If anything fails, everything restore created is removed, the new directory
  included. Missing parent directories that it created remain.
- Run it as the user that will run the server, so the files belong to it.
- `serve` creates a new `access-token` on first start.

## Upgrade between versions

1. Back up the running server with the **new** build's `backup` command. It
   reads schema versions 1 to 5 and 6, so it works on a directory written by a
   release that has no `backup` command. If you cannot run the new binary
   against the old directory, stop the old server and copy its whole data
   directory instead.
2. Stop the server, install the new archive in a new directory, and start it
   with the same `--data-dir`. The first start upgrades the database in one
   transaction before it switches the file to WAL; a failed upgrade leaves the
   file as it was.
3. A database with a newer schema than the binary is refused with a message
   that names both versions, and the file is not touched. There is no downgrade:
   go back to the previous binary only with a backup taken before the upgrade,
   restored into a new directory.

Schema 6 adds the token table to the versioned schema. A server of an older
release that is started again on a directory this build upgraded will refuse it.

## Containers

The image runs as uid 10001 with the volume at `/data`
([container image](container.md)). The binary is `/app/bin/artifree-server`.

```sh
# Backup: write inside the volume, then copy out (docker exec runs as uid 10001).
docker exec artifree /app/bin/artifree-server backup --data-dir /data --output /data/backup-2026-11-02.sqlite3
docker cp artifree:/data/backup-2026-11-02.sqlite3 .
docker exec artifree rm /data/backup-2026-11-02.sqlite3

# Restore into a NEW volume with a throwaway container run as the same user.
# The target must not exist, and a mount point does, so use a subdirectory.
# A new volume at /data inherits the owner and mode 700 the image gives /data.
docker volume create artifree-data-restored
docker run --rm --platform linux/amd64 --entrypoint /app/bin/artifree-server \
  -v artifree-data-restored:/data -v "$PWD":/in:ro artifree:local \
  restore --input /in/backup-2026-11-02.sqlite3 --data-dir /data/instance

# Serve it: the later --data-dir wins over the image's /data.
docker run -d --name artifree-restored --platform linux/amd64 \
  -v artifree-data-restored:/data -p 127.0.0.1:7410:7410 -p 127.0.0.1:7411:7411 \
  artifree:local --data-dir /data/instance --content-origin http://127.0.0.1:7411
```

Notes:

- The image's user is uid 10001, and `docker run` without `--user` uses it, so
  the restored files belong to the server's user. `docker cp` creates files
  owned by root: if you restore from a bind mount, make the backup file
  readable by uid 10001 (`chown 10001:10001` or `chmod o+r`) first.
- `--output` is refused if the path exists, and the root file system is
  read-only in the documented `docker run`, so write the backup inside the
  volume (as above) and delete it after copying it out.
- A bind mount used as a data directory must be owned by 10001 and mode 700.
- The commands above were not run: Docker was not available where this was
  written, and the CI container job does not exercise them.

## Not verified

- Windows execution outside CI, and the Windows file-system behaviors it relies
  on (hard links in the output directory, no directory flush).
- The container commands above, bind mounts, and `docker cp` ownership.
- File systems without hard links (the backup then fails with an error that says so),
  network file systems, and databases larger than a few hundred megabytes.
- A power loss or kill during a backup or restore. Only the error paths were
  exercised: a leftover scratch directory is the expected result.
- That a backup and a running server never delay each other. Under WAL SQLite
  readers do not block the writer; commit latency during a backup was not measured.
- Restoring a backup made by a different build than this one, other than the
  schema versions 1 to 5 fixtures in the tests.
