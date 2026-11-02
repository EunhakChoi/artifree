//! Consistent backups of the data directory's database, and restores of them
//! into a new data directory.
//!
//! A backup is one `SQLite` file made with `VACUUM INTO`, which reads the
//! database in a single read transaction. Under WAL, as the server runs, that
//! is a consistent snapshot while commits continue, and a compact one. The
//! snapshot is written into a private scratch directory beside the output, on
//! the same file system, and is published by a no-replace hard link only
//! after it was proven restorable: the same pipeline a restore runs (check,
//! upgrade a copy, check again, compare the schema with this build's) is
//! applied before success is reported.
//!
//! The backup keeps the schema version it was taken at; a restore upgrades.
//! A restore reads the input only, builds the new database inside the new
//! data directory, gives it a new change-feed identity (decision 0006), and
//! publishes it the same way.
//!
//! Both commands open the source or input as the user who runs them, so they
//! must run as the server's operating-system user. Opening a database left
//! with a WAL by a crash performs `SQLite`'s normal recovery, which writes to
//! it; a cleanly closed database is left as it was.
//!
//! The data directory's access token is not part of a backup. Tickets, grants,
//! sessions, and handoffs live in server memory.
//!
//! Scratch directories are named with [`SCRATCH_PREFIX`]. They are removed on
//! every error path the process survives. A scratch directory that cannot be
//! removed after the result was published does not undo that result: the
//! output stays, the removal is retried once, and if it still fails the
//! directory is reported as [`Report::leftover`]. A killed process also leaves
//! its directory. A leftover is safe to delete when no backup or restore is
//! running.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::data_dir::{self, parent_dir};
use crate::schema::{self, SCHEMA_VERSION};
use crate::storage::{self, DATABASE_FILE, random_bytes};

/// Name prefix of the scratch directories backup and restore create. A
/// directory with this prefix that no running command owns is a leftover of a
/// killed process.
pub const SCRATCH_PREFIX: &str = ".artifree-scratch-";

const SNAPSHOT_FILE: &str = "snapshot.sqlite3";
const COPY_FILE: &str = "copy.sqlite3";
const SIBLING_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

/// Why a backup or restore did not finish. Nothing it created remains.
#[derive(Debug)]
#[non_exhaustive]
pub enum BackupError {
    /// The output file or the new data directory already exists. Nothing is replaced.
    Exists(PathBuf),
    /// The database is not one this build can safely back up or restore:
    /// newer, foreign, damaged, or with a schema that differs from the one
    /// this build creates.
    Refused(String),
    /// A file or database operation failed.
    Failed(String),
}

impl fmt::Display for BackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exists(path) => write!(
                formatter,
                "{} already exists; nothing was replaced",
                path.display()
            ),
            Self::Refused(reason) | Self::Failed(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for BackupError {}

/// A scratch directory that a successful backup or restore could not remove.
/// The result was published and proven before the removal was tried, so it is
/// kept; the directory only wastes space and is safe to delete when no backup
/// or restore is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    /// The directory, named with [`SCRATCH_PREFIX`].
    pub path: PathBuf,
    /// Why it could not be removed.
    pub reason: String,
}

/// What a finished backup or restore produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Schema version of the written database file: the one the backup was
    /// taken at, and always the current one after a restore.
    pub schema_version: i64,
    /// Size of the written database file in bytes.
    pub bytes: u64,
    /// The scratch directory that could not be removed after the result was
    /// published, if any. The backup or restore itself succeeded.
    pub leftover: Option<Leftover>,
}

fn failed(context: impl fmt::Display, error: impl fmt::Display) -> BackupError {
    BackupError::Failed(format!("{context}: {error}"))
}

fn damaged(path: &Path, error: impl fmt::Display) -> BackupError {
    BackupError::Refused(format!(
        "{} is not a usable Artifree database: {error}",
        path.display()
    ))
}

/// Writes a consistent, restorable copy of the database in `data_dir` to the
/// new file `output`, while the server may be running.
///
/// The output is the snapshot as taken, at the database's schema version, and
/// has owner-only permissions on Unix. Its parent directory must exist, be
/// writable, and support hard links.
///
/// # Errors
///
/// [`BackupError::Exists`] when `output` exists, also as a symbolic link.
/// [`BackupError::Refused`] for a database that is empty, foreign, newer than
/// this build, damaged, or different from this build's schema.
/// [`BackupError::Failed`] for other failures. On any error `output` is gone
/// and the scratch directory is removed (see the module documentation for a
/// scratch directory that cannot be).
///
/// A scratch directory that cannot be removed once `output` is published is
/// not an error: it is returned as [`Report::leftover`].
pub fn backup(data_dir: &Path, output: &Path) -> Result<Report, BackupError> {
    #[cfg(unix)]
    let owner_check = refuse_foreign_database;
    #[cfg(not(unix))]
    let owner_check = |_: &Path| Ok(());
    backup_with(data_dir, output, owner_check, remove_tree)
}

/// How a scratch directory is removed; [`remove_tree`] outside tests.
type RemoveDir = fn(&Path) -> io::Result<()>;

fn remove_tree(directory: &Path) -> io::Result<()> {
    fs::remove_dir_all(directory)
}

/// [`backup`] with the check of who owns the database made by `owner_check`,
/// which runs before anything is opened or created, and the scratch
/// directory removed by `remove_dir`.
fn backup_with(
    data_dir: &Path,
    output: &Path,
    owner_check: impl FnOnce(&Path) -> Result<(), BackupError>,
    remove_dir: RemoveDir,
) -> Result<Report, BackupError> {
    let database = data_dir.join(DATABASE_FILE);
    refuse_existing(output)?;
    if !database.is_file() {
        return Err(BackupError::Failed(format!(
            "no database at {}",
            database.display()
        )));
    }
    owner_check(&database)?;
    let parent = parent_dir(output);
    let scratch = Scratch::create_in(parent, remove_dir)?;
    let snapshot = scratch.path(SNAPSHOT_FILE);
    take_snapshot(&database, &snapshot)?;
    let schema_version = prove_restorable(&snapshot, &scratch)?;
    refuse_siblings(&snapshot)?;
    seal(&snapshot)?;
    let bytes = size_of(&snapshot)?;
    publish(&snapshot, output, || sync_directory(parent))?;
    Ok(Report {
        schema_version,
        bytes,
        leftover: scratch.discard(),
    })
}

/// Restores `input`, a file made by [`backup`] of this or an older build,
/// into the new data directory `data_dir`. The directory is created with the
/// server's access policy and must not exist. The input is only read.
///
/// The restored database is at the current schema version and has a new
/// random change-feed identity, so clients that followed the original feed
/// take the reset path; its sequence head is unchanged. The access token is
/// created by the first `serve`.
///
/// # Errors
///
/// As for [`backup`], with `data_dir` in place of `output`; an input with
/// stale `-wal`, `-shm`, or `-journal` files beside it is refused. On any
/// error everything restore created, `data_dir` included, is removed. Missing
/// parent directories it created stay. A scratch directory inside `data_dir`
/// that cannot be removed once the database is published is not an error: it
/// is returned as [`Report::leftover`].
pub fn restore(input: &Path, data_dir: &Path) -> Result<Report, BackupError> {
    restore_with(input, data_dir, remove_tree)
}

/// [`restore`] with the scratch directory removed by `remove_dir`.
fn restore_with(
    input: &Path,
    data_dir: &Path,
    remove_dir: RemoveDir,
) -> Result<Report, BackupError> {
    if !input.is_file() {
        return Err(BackupError::Failed(format!(
            "{} is not a file",
            input.display()
        )));
    }
    refuse_siblings(input)?;
    data_dir::create_new_data_dir(data_dir).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            BackupError::Exists(data_dir.to_owned())
        } else {
            failed(format!("cannot create {}", data_dir.display()), error)
        }
    })?;
    restore_into(input, data_dir, remove_dir).map_err(|error| match fs::remove_dir_all(data_dir) {
        Ok(()) => error,
        Err(cleanup) => BackupError::Failed(format!(
            "{error}; additionally {} could not be removed: {cleanup}",
            data_dir.display()
        )),
    })
}

fn restore_into(
    input: &Path,
    data_dir: &Path,
    remove_dir: RemoveDir,
) -> Result<Report, BackupError> {
    let scratch = Scratch::create_in(data_dir, remove_dir)?;
    let copy = scratch.path(COPY_FILE);
    copy_private(input, &copy)?;
    let mut connection = open_existing(&copy)?;
    verify_and_upgrade(&mut connection, &copy)?;
    schema::renew_feed_identity(&connection)
        .map_err(|error| failed("cannot renew the change feed identity", error))?;
    use_rollback_journal(&connection, &copy)?;
    drop(connection);
    refuse_siblings(&copy)?;
    seal(&copy)?;
    let database = data_dir.join(DATABASE_FILE);
    let bytes = size_of(&copy)?;
    publish(&copy, &database, || {
        sync_directory(data_dir)?;
        sync_directory(parent_dir(data_dir))
    })?;
    Ok(Report {
        schema_version: SCHEMA_VERSION,
        bytes,
        leftover: scratch.discard(),
    })
}

/// Fails when anything, a symbolic link included, is at `path`.
fn refuse_existing(path: &Path) -> Result<(), BackupError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(BackupError::Exists(path.to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(failed(format!("cannot inspect {}", path.display()), error)),
    }
}

/// A database file is only complete with its WAL or journal. A copy of the
/// file alone would silently lose what they hold.
fn refuse_siblings(database: &Path) -> Result<(), BackupError> {
    for suffix in SIBLING_SUFFIXES {
        let mut sibling = database.as_os_str().to_owned();
        sibling.push(suffix);
        let sibling = PathBuf::from(sibling);
        match fs::symlink_metadata(&sibling) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(failed(
                    format!("cannot inspect {}", sibling.display()),
                    error,
                ));
            }
            Ok(_) => {
                return Err(BackupError::Refused(format!(
                    "{} exists beside {}; it belongs to a database that was not closed cleanly. Open the original with the server's user and stop it first",
                    sibling.display(),
                    database.display()
                )));
            }
        }
    }
    Ok(())
}

/// Opens an existing database for reading and writing, never creating it.
fn open_existing(path: &Path) -> Result<Connection, BackupError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| match error.sqlite_error_code() {
        // Permissions, a missing file or a wrong user, not a damaged database.
        Some(rusqlite::ErrorCode::CannotOpen | rusqlite::ErrorCode::PermissionDenied) => {
            failed(format!("cannot open {}", path.display()), error)
        }
        _ => damaged(path, error),
    })?;
    storage::configure(&connection).map_err(|error| damaged(path, error))?;
    Ok(connection)
}

/// The schema version, when this build can read it.
fn supported_version(connection: &Connection, path: &Path) -> Result<i64, BackupError> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| damaged(path, error))?;
    if version < 1 {
        Err(BackupError::Refused(format!(
            "{} has no Artifree schema version (it is empty, truncated, or another application's database)",
            path.display()
        )))
    } else if version > SCHEMA_VERSION {
        Err(BackupError::Refused(format!(
            "{} uses schema version {version}, newer than the version {SCHEMA_VERSION} this build supports; use a newer build",
            path.display()
        )))
    } else {
        Ok(version)
    }
}

fn check_integrity(connection: &Connection, path: &Path) -> Result<(), BackupError> {
    let mut statement = connection
        .prepare("PRAGMA integrity_check")
        .map_err(|error| damaged(path, error))?;
    let findings: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .and_then(Iterator::collect)
        .map_err(|error| damaged(path, error))?;
    if findings != ["ok"] {
        return Err(damaged(
            path,
            format!("integrity check: {}", findings.join("; ")),
        ));
    }
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(|error| damaged(path, error))?;
    let violations = statement
        .query_map([], |row| row.get::<_, String>(0))
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .map_err(|error| damaged(path, error))?
        .len();
    if violations > 0 {
        return Err(damaged(
            path,
            format!("{violations} foreign key violations"),
        ));
    }
    Ok(())
}

/// Takes the snapshot of the live database into the new file `snapshot`.
fn take_snapshot(database: &Path, snapshot: &Path) -> Result<(), BackupError> {
    let source = open_existing(database)?;
    supported_version(&source, database)?;
    // The path is cast to text in the database's encoding, so only UTF-8 files
    // are snapshotted; every database this project creates is UTF-8.
    let encoding: String = source
        .pragma_query_value(None, "encoding", |row| row.get(0))
        .map_err(|error| damaged(database, error))?;
    if encoding != "UTF-8" {
        return Err(damaged(
            database,
            format!("its text encoding is {encoding}, not UTF-8"),
        ));
    }
    // The path is bound as a blob and cast to text, which keeps its bytes, so
    // a directory name that is not UTF-8 works.
    source
        .execute("VACUUM INTO CAST(?1 AS TEXT)", [path_bytes(snapshot)?])
        .map_err(|error| damaged(database, format!("cannot snapshot: {error}")))?;
    drop(source);
    use_rollback_journal(&open_existing(snapshot)?, snapshot)
}

/// A path as the bytes `SQLite` opens. On Unix that is the path itself. Windows
/// paths are opened as UTF-8, so one that is not valid Unicode is refused.
#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Same signature as the Windows function, which can fail."
)]
fn path_bytes(path: &Path) -> Result<Vec<u8>, BackupError> {
    use std::os::unix::ffi::OsStrExt;
    Ok(path.as_os_str().as_bytes().to_vec())
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Result<Vec<u8>, BackupError> {
    path.to_str()
        .map(|text| text.as_bytes().to_vec())
        .ok_or_else(|| BackupError::Failed(format!("{} is not valid Unicode", path.display())))
}

/// Every file backup and restore publish is in the rollback-journal mode, one
/// self-contained file, whatever mode the source was in. The server switches
/// its database to WAL when it opens it.
fn use_rollback_journal(connection: &Connection, path: &Path) -> Result<(), BackupError> {
    connection
        .pragma_update(None, "journal_mode", "DELETE")
        .map_err(|error| damaged(path, error))
}

/// The restore pipeline on a file the caller owns and may change: check it,
/// upgrade it when older, check again after an upgrade, and compare its
/// schema with this build's. A file already at the current version is not
/// written. The upgrade runs on this pipeline's own connection and does not
/// touch the journal mode. It returns the version the file had.
fn upgrade_and_verify(copy: &Path) -> Result<i64, BackupError> {
    verify_and_upgrade(&mut open_existing(copy)?, copy)
}

/// [`upgrade_and_verify`] on a connection to `copy` the caller keeps using.
/// The connection must come from [`open_existing`], whose settings (foreign
/// keys enforced) the upgrade ladder has always run with.
fn verify_and_upgrade(connection: &mut Connection, copy: &Path) -> Result<i64, BackupError> {
    let version = supported_version(connection, copy)?;
    check_integrity(connection, copy)?;
    if version < SCHEMA_VERSION {
        schema::upgrade(connection).map_err(|error| {
            BackupError::Refused(format!(
                "upgrading {} from schema version {version} failed ({error}); nothing was published",
                copy.display()
            ))
        })?;
        check_integrity(connection, copy)?;
    }
    schema::check_current(connection).map_err(|difference| {
        BackupError::Refused(format!(
            "{} does not match the schema this build creates: {difference}",
            copy.display()
        ))
    })?;
    Ok(version)
}

/// Proves that a restore of `snapshot` would succeed, by running the restore
/// pipeline on it, or on a copy when it needs an upgrade. Returns its version.
fn prove_restorable(snapshot: &Path, scratch: &Scratch) -> Result<i64, BackupError> {
    let version = supported_version(&open_existing(snapshot)?, snapshot)?;
    if version == SCHEMA_VERSION {
        return upgrade_and_verify(snapshot);
    }
    let copy = scratch.path(COPY_FILE);
    copy_private(snapshot, &copy)?;
    upgrade_and_verify(&copy)
}

/// Copies `from` to the new file `to`, which the current user can read and
/// write whatever the permissions of `from` are (a backup kept read-only is
/// still a valid input), and flushes it.
fn copy_private(from: &Path, to: &Path) -> Result<(), BackupError> {
    let context = || format!("cannot copy {} to {}", from.display(), to.display());
    let mut source = fs::File::open(from).map_err(|error| failed(context(), error))?;
    let mut target = create_private_file(to).map_err(|error| failed(context(), error))?;
    io::copy(&mut source, &mut target)
        .and_then(|_| target.sync_all())
        .map_err(|error| failed(context(), error))
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> io::Result<fs::File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Refuses a database owned by another user. Opening it read-write could run
/// `SQLite`'s WAL recovery and leave `-wal` and `-shm` files this user owns in
/// the server's data directory. The standard library has no `geteuid`, so the
/// invoking user is the owner of a file this command creates, for a moment,
/// beside the database: on the same file system, so ownership means the same
/// for both. No write access there is refused too, since recovery needs it.
#[cfg(unix)]
fn refuse_foreign_database(database: &Path) -> Result<(), BackupError> {
    use std::os::unix::fs::MetadataExt;
    let directory = parent_dir(database);
    let random = random_bytes().map_err(|error| failed("no system randomness", error))?;
    let probe = directory.join(format!(
        "{SCRATCH_PREFIX}owner-{:032x}",
        u128::from_be_bytes(random)
    ));
    let user_uid = create_private_file(&probe)
        .and_then(|file| file.metadata())
        .map(|metadata| metadata.uid());
    drop(fs::remove_file(&probe));
    let user_uid = user_uid.map_err(|error| {
        failed(
            format!(
                "cannot write in {}; run backup as the user that runs the server",
                directory.display()
            ),
            error,
        )
    })?;
    let database_uid = fs::metadata(database)
        .map(|metadata| metadata.uid())
        .map_err(|error| failed(format!("cannot inspect {}", database.display()), error))?;
    check_owner(database_uid, user_uid, database)
}

#[cfg(unix)]
fn check_owner(database_uid: u32, user_uid: u32, database: &Path) -> Result<(), BackupError> {
    if database_uid == user_uid {
        Ok(())
    } else {
        Err(BackupError::Refused(format!(
            "{} is owned by user id {database_uid}, not by the user running this command ({user_uid}); run backup as the user that runs the server",
            database.display()
        )))
    }
}

/// Gives the finished file its final permissions and makes it durable.
fn seal(file: &Path) -> Result<(), BackupError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(file, fs::Permissions::from_mode(0o600))
            .map_err(|error| failed(format!("cannot restrict {}", file.display()), error))?;
    }
    // Write access is what Windows needs to flush a file.
    OpenOptions::new()
        .write(true)
        .open(file)
        .and_then(|handle| handle.sync_all())
        .map_err(|error| failed(format!("cannot flush {}", file.display()), error))
}

/// Publishes `source` at `destination` by a hard link, which fails instead of
/// replacing anything, then runs `finish`, which makes the entry durable.
/// When `finish` fails the link is removed again: a reported failure never
/// leaves the destination behind, so a retry works. Once this returns `Ok`
/// the destination stays; nothing after it may remove it.
fn publish(
    source: &Path,
    destination: &Path,
    finish: impl FnOnce() -> Result<(), BackupError>,
) -> Result<(), BackupError> {
    fs::hard_link(source, destination).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            BackupError::Exists(destination.to_owned())
        } else {
            failed(
                format!(
                    "cannot create {} (the file system must support hard links)",
                    destination.display()
                ),
                error,
            )
        }
    })?;
    finish().map_err(|error| match fs::remove_file(destination) {
        Ok(()) => error,
        Err(cleanup) => BackupError::Failed(format!(
            "{error}; additionally {} could not be removed: {cleanup}",
            destination.display()
        )),
    })
}

/// Flushes a directory's entries. Windows has no standard-library way to
/// flush a directory, so there a created file is as durable as the file
/// system makes it. Some file systems (certain FUSE, SMB, and NFS mounts)
/// cannot flush a directory; the file was already flushed, so that is not a
/// failure there, and the entry is only as durable as the mount makes it.
#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), BackupError> {
    let handle = fs::File::open(directory)
        .map_err(|error| failed(format!("cannot open {}", directory.display()), error))?;
    match handle.sync_all() {
        Err(error) if !directory_flush_unsupported(&error) => Err(failed(
            format!("cannot flush {}", directory.display()),
            error,
        )),
        _ => Ok(()),
    }
}

/// Whether `error` is the operating system saying that this file system
/// cannot flush a directory (`EINVAL` or `ENOTSUP`), not that flushing failed.
#[cfg(unix)]
fn directory_flush_unsupported(error: &io::Error) -> bool {
    error.raw_os_error().is_some()
        && matches!(
            error.kind(),
            io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
        )
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Same signature as the Unix function, which can fail."
)]
fn sync_directory(_directory: &Path) -> Result<(), BackupError> {
    Ok(())
}

fn size_of(file: &Path) -> Result<u64, BackupError> {
    fs::metadata(file)
        .map(|metadata| metadata.len())
        .map_err(|error| failed(format!("cannot inspect {}", file.display()), error))
}

/// A private directory for unfinished files, removed by [`Scratch::discard`]
/// or, on an error path, when dropped.
struct Scratch {
    directory: PathBuf,
    remove_dir: RemoveDir,
    removed: bool,
}

impl Scratch {
    fn create_in(parent: &Path, remove_dir: RemoveDir) -> Result<Self, BackupError> {
        let random = random_bytes().map_err(|error| failed("no system randomness", error))?;
        let suffix = format!("{:032x}", u128::from_be_bytes(random));
        let directory = parent.join(format!("{SCRATCH_PREFIX}{suffix}"));
        data_dir::create_private_dir(&directory).map_err(|error| {
            failed(
                format!("cannot create a scratch directory in {}", parent.display()),
                error,
            )
        })?;
        Ok(Self {
            directory,
            remove_dir,
            removed: false,
        })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }

    /// Removes the directory after the result it served was published. A
    /// failure is not an error: the removal is retried once, by dropping,
    /// and a directory that is still there is returned as a leftover.
    fn discard(mut self) -> Option<Leftover> {
        let error = match (self.remove_dir)(&self.directory) {
            Ok(()) => {
                self.removed = true;
                return None;
            }
            Err(error) => error,
        };
        let path = self.directory.clone();
        drop(self);
        let gone = matches!(
            fs::symlink_metadata(&path),
            Err(ref error) if error.kind() == io::ErrorKind::NotFound
        );
        (!gone).then(|| Leftover {
            path,
            reason: error.to_string(),
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.removed {
            // A leftover is named by SCRATCH_PREFIX. On an error path the
            // command reports its own error; after `discard` the leftover is
            // reported.
            drop((self.remove_dir)(&self.directory));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn directory(name: &str) -> io::Result<PathBuf> {
        let path =
            std::env::temp_dir().join(format!("artifree-publish-{}-{name}", std::process::id()));
        drop(fs::remove_dir_all(&path));
        fs::create_dir_all(&path)?;
        Ok(path)
    }

    #[test]
    fn a_failure_after_the_link_removes_the_destination_so_a_retry_works() -> TestResult {
        let dir = directory("retry")?;
        let source = dir.join("source");
        let destination = dir.join("destination");
        fs::write(&source, b"data")?;

        let result = publish(&source, &destination, || {
            assert!(destination.exists(), "the link exists while finishing");
            Err(BackupError::Failed("flush failed".to_owned()))
        });

        assert!(
            matches!(&result, Err(BackupError::Failed(reason)) if reason == "flush failed"),
            "{result:?}"
        );
        assert!(
            !destination.exists(),
            "a failed publish left the destination"
        );
        assert_eq!(fs::read(&source)?, b"data");
        publish(&source, &destination, || Ok(()))?;
        assert_eq!(fs::read(&destination)?, b"data");
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn publishing_never_replaces_an_existing_destination() -> TestResult {
        let dir = directory("exists")?;
        let source = dir.join("source");
        let destination = dir.join("destination");
        fs::write(&source, b"new")?;
        fs::write(&destination, b"old")?;

        let result = publish(&source, &destination, || Ok(()));

        assert!(matches!(result, Err(BackupError::Exists(_))), "{result:?}");
        assert_eq!(fs::read(&destination)?, b"old");
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_database_of_another_user_is_refused_before_it_is_opened() {
        let database = Path::new("/data/artifree.sqlite3");
        assert!(check_owner(1000, 1000, database).is_ok());
        let refused = check_owner(999, 1000, database);
        assert!(
            matches!(&refused, Err(BackupError::Refused(reason)) if reason.contains("999") && reason.contains("1000")),
            "{refused:?}"
        );
        // Root is held to the same rule: the files must belong to the server's user.
        assert!(check_owner(1000, 0, database).is_err());
    }

    /// The owner check is wired into `backup`: with another owner reported it
    /// stops before anything is created, and with the real one it proceeds.
    #[cfg(unix)]
    #[test]
    fn backup_stops_for_a_database_of_another_user_and_leaves_nothing() -> TestResult {
        use std::os::unix::fs::MetadataExt;
        let dir = directory("owner")?;
        let data = dir.join("data");
        fs::create_dir(&data)?;
        crate::storage::SqliteStore::open(&data.join(DATABASE_FILE))?;
        let out = dir.join("out");
        fs::create_dir(&out)?;
        let before: Vec<_> = fs::read_dir(&data)?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<Result<_, _>>()?;
        let uid = fs::metadata(data.join(DATABASE_FILE))?.uid();

        let refused = backup_with(
            &data,
            &out.join("b.sqlite3"),
            |database| check_owner(uid.wrapping_add(1), uid, database),
            remove_tree,
        );

        assert!(
            matches!(refused, Err(BackupError::Refused(_))),
            "{refused:?}"
        );
        assert_eq!(fs::read_dir(&out)?.count(), 0, "nothing was created");
        let after: Vec<_> = fs::read_dir(&data)?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<Result<_, _>>()?;
        assert_eq!(before.len(), after.len());
        // The real check passes for a database of this user and leaves no probe.
        refuse_foreign_database(&data.join(DATABASE_FILE))?;
        assert_eq!(fs::read_dir(&data)?.count(), before.len(), "probe removed");
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn only_an_unsupported_directory_flush_is_tolerated() {
        // EINVAL is 22 on every Unix.
        assert!(directory_flush_unsupported(&io::Error::from_raw_os_error(
            22
        )));
        #[cfg(target_os = "linux")]
        assert!(directory_flush_unsupported(&io::Error::from_raw_os_error(
            95
        )));
        // Real failures still fail the publish: EIO, EACCES, and an error that
        // did not come from the operating system.
        assert!(!directory_flush_unsupported(&io::Error::from_raw_os_error(
            5
        )));
        assert!(!directory_flush_unsupported(&io::Error::from_raw_os_error(
            13
        )));
        assert!(!directory_flush_unsupported(
            &io::ErrorKind::InvalidInput.into()
        ));
        assert!(!directory_flush_unsupported(
            &io::ErrorKind::Unsupported.into()
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_flush_succeeds_on_an_ordinary_directory() -> TestResult {
        let dir = directory("flush")?;
        sync_directory(&dir)?;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    fn refuse_removal(_: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected: cannot remove",
        ))
    }

    fn scratch_names(directory: &Path) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(directory)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.starts_with(SCRATCH_PREFIX) {
                names.push(name);
            }
        }
        Ok(names)
    }

    /// A data directory with a database, and an empty `out` directory.
    fn data_and_out(dir: &Path) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
        let data = dir.join("data");
        fs::create_dir(&data)?;
        crate::storage::SqliteStore::open(&data.join(DATABASE_FILE))?;
        let out = dir.join("out");
        fs::create_dir(&out)?;
        Ok((data, out))
    }

    /// The backup was published and proven before the scratch directory was
    /// touched, so a scratch directory that cannot be removed is a warning,
    /// not a failure that deletes the backup.
    #[test]
    fn a_scratch_directory_that_cannot_be_removed_keeps_the_published_backup() -> TestResult {
        let dir = directory("leftover-backup")?;
        let (data, out) = data_and_out(&dir)?;
        let output = out.join("b.sqlite3");

        let report = backup_with(&data, &output, |_| Ok(()), refuse_removal)?;

        let Some(leftover) = report.leftover.as_ref() else {
            panic!("a leftover is reported: {report:?}");
        };
        assert_eq!(leftover.path.parent(), Some(out.as_path()));
        assert!(
            leftover
                .path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(SCRATCH_PREFIX)),
            "{leftover:?}"
        );
        assert!(leftover.path.is_dir(), "the leftover is really there");
        assert!(leftover.reason.contains("injected"), "{leftover:?}");
        // The proven backup stays, complete and restorable.
        assert_eq!(report.bytes, fs::metadata(&output)?.len());
        let target = dir.join("restored");
        restore(&output, &target)?;
        assert_eq!(scratch_names(&out)?.len(), 1, "only the reported leftover");
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn a_scratch_directory_that_cannot_be_removed_keeps_the_restored_database() -> TestResult {
        let dir = directory("leftover-restore")?;
        let (data, out) = data_and_out(&dir)?;
        let input = out.join("b.sqlite3");
        backup(&data, &input)?;
        let target = dir.join("restored");

        let report = restore_with(&input, &target, refuse_removal)?;

        let Some(leftover) = report.leftover.as_ref() else {
            panic!("a leftover is reported: {report:?}");
        };
        assert_eq!(leftover.path.parent(), Some(target.as_path()));
        assert!(leftover.path.is_dir());
        assert!(target.join(DATABASE_FILE).is_file(), "the database stays");
        crate::storage::SqliteStore::open(&target.join(DATABASE_FILE))?;
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    /// The removal is retried when the scratch is dropped, so a failure that
    /// goes away is not reported.
    #[test]
    fn a_removal_that_succeeds_on_the_retry_is_not_reported() -> TestResult {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FAILED_ONCE: AtomicBool = AtomicBool::new(false);
        fn fail_once(path: &Path) -> io::Result<()> {
            if FAILED_ONCE.swap(true, Ordering::SeqCst) {
                fs::remove_dir_all(path)
            } else {
                Err(io::Error::other("injected: transient"))
            }
        }
        let dir = directory("leftover-retry")?;
        let (data, out) = data_and_out(&dir)?;

        let report = backup_with(&data, &out.join("b.sqlite3"), |_| Ok(()), fail_once)?;

        assert_eq!(report.leftover, None);
        assert!(FAILED_ONCE.load(Ordering::SeqCst));
        assert_eq!(scratch_names(&out)?, Vec::<String>::new());
        assert!(out.join("b.sqlite3").is_file());
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
