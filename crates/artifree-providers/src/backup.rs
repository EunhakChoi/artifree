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
//! every error path, but a killed process leaves its directory, which is safe
//! to delete when no backup or restore is running.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::data_dir;
use crate::schema::{self, SCHEMA_VERSION};
use crate::storage::{DATABASE_FILE, SqliteStore, random_bytes};

/// Name prefix of the scratch directories backup and restore create. A
/// directory with this prefix that no running command owns is a leftover of a
/// killed process.
pub const SCRATCH_PREFIX: &str = ".artifree-scratch-";

const BUSY_TIMEOUT: Duration = Duration::from_secs(10);
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

/// What a finished backup or restore produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// Schema version of the written database file.
    pub schema_version: i64,
    /// Size of the written database file in bytes.
    pub bytes: u64,
}

/// The directory a new file is created in: its parent, or the current
/// directory for a bare file name.
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
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
/// [`BackupError::Failed`] for other failures. On any error `output` and the
/// scratch directory are gone.
pub fn backup(data_dir: &Path, output: &Path) -> Result<Report, BackupError> {
    let database = data_dir.join(DATABASE_FILE);
    refuse_existing(output)?;
    if !database.is_file() {
        return Err(BackupError::Failed(format!(
            "no database at {}",
            database.display()
        )));
    }
    let parent = parent_dir(output);
    let scratch = Scratch::create_in(parent)?;
    let snapshot = scratch.path(SNAPSHOT_FILE);
    take_snapshot(&database, &snapshot)?;
    let schema_version = prove_restorable(&snapshot, &scratch)?;
    refuse_siblings(&snapshot)?;
    seal(&snapshot)?;
    publish(&snapshot, output, || {
        scratch.remove()?;
        sync_directory(parent)
    })?;
    Ok(Report {
        schema_version,
        bytes: size_of(output)?,
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
/// parent directories it created stay.
pub fn restore(input: &Path, data_dir: &Path) -> Result<Report, BackupError> {
    refuse_existing(data_dir)?;
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
    restore_into(input, data_dir).map_err(|error| match fs::remove_dir_all(data_dir) {
        Ok(()) => error,
        Err(cleanup) => BackupError::Failed(format!(
            "{error}; additionally {} could not be removed: {cleanup}",
            data_dir.display()
        )),
    })
}

fn restore_into(input: &Path, data_dir: &Path) -> Result<Report, BackupError> {
    let scratch = Scratch::create_in(data_dir)?;
    let copy = scratch.path(COPY_FILE);
    fs::copy(input, &copy).map_err(|error| {
        failed(
            format!("cannot copy {} to {}", input.display(), copy.display()),
            error,
        )
    })?;
    let schema_version = upgrade_and_verify(&copy)?;
    let connection = open_existing(&copy)?;
    schema::renew_feed_identity(&connection)
        .map_err(|error| failed("cannot renew the change feed identity", error))?;
    drop(connection);
    refuse_siblings(&copy)?;
    seal(&copy)?;
    let database = data_dir.join(DATABASE_FILE);
    publish(&copy, &database, || {
        scratch.remove()?;
        sync_directory(data_dir)?;
        sync_directory(parent_dir(data_dir))
    })?;
    Ok(Report {
        schema_version,
        bytes: size_of(&database)?,
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
        if fs::symlink_metadata(&sibling).is_ok() {
            return Err(BackupError::Refused(format!(
                "{} exists beside {}; it belongs to a database that was not closed cleanly. Open the original with the server's user and stop it first",
                sibling.display(),
                database.display()
            )));
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
    .map_err(|error| damaged(path, error))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| damaged(path, error))?;
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
    let target = snapshot
        .to_str()
        .ok_or_else(|| BackupError::Failed(format!("{} is not valid UTF-8", snapshot.display())))?;
    source
        .execute("VACUUM INTO ?1", [target])
        .map_err(|error| damaged(database, format!("cannot snapshot: {error}")))?;
    drop(source);
    // A single self-contained file, whatever the source's journal mode was.
    open_existing(snapshot)?
        .pragma_update(None, "journal_mode", "DELETE")
        .map_err(|error| damaged(snapshot, error))
}

/// The restore pipeline on a file the caller owns and may change: check it,
/// upgrade it when older, check again after an upgrade, and compare its
/// schema with this build's. A file already at the current version is not
/// written. It returns the version the file had.
fn upgrade_and_verify(copy: &Path) -> Result<i64, BackupError> {
    let version = {
        let connection = open_existing(copy)?;
        let version = supported_version(&connection, copy)?;
        check_integrity(&connection, copy)?;
        version
    };
    if version < SCHEMA_VERSION {
        SqliteStore::open(copy).map_err(|error| {
            BackupError::Refused(format!(
                "upgrading from schema version {version} failed and changed nothing: {error}"
            ))
        })?;
        check_integrity(&open_existing(copy)?, copy)?;
    }
    schema::check_current(&open_existing(copy)?).map_err(|difference| {
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
    fs::copy(snapshot, &copy)
        .map_err(|error| failed(format!("cannot copy {}", snapshot.display()), error))?;
    upgrade_and_verify(&copy)
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
/// replacing anything, then runs `finish`. When `finish` fails the link is
/// removed again: a reported failure never leaves the destination behind, so
/// a retry works.
fn publish(
    source: &Path,
    destination: &Path,
    finish: impl FnOnce() -> Result<(), BackupError>,
) -> Result<(), BackupError> {
    refuse_existing(destination)?;
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
/// system makes it.
#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), BackupError> {
    fs::File::open(directory)
        .and_then(|handle| handle.sync_all())
        .map_err(|error| failed(format!("cannot flush {}", directory.display()), error))
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

/// A private directory for unfinished files, removed by [`Scratch::remove`]
/// or, on an error path, when dropped.
#[derive(Debug)]
struct Scratch {
    directory: Option<PathBuf>,
}

impl Scratch {
    fn create_in(parent: &Path) -> Result<Self, BackupError> {
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
            directory: Some(directory),
        })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory
            .as_deref()
            .map_or_else(|| PathBuf::from(name), |directory| directory.join(name))
    }

    fn remove(mut self) -> Result<(), BackupError> {
        match self.directory.take() {
            Some(directory) => fs::remove_dir_all(&directory)
                .map_err(|error| failed(format!("cannot remove {}", directory.display()), error)),
            None => Ok(()),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(directory) = self.directory.take() {
            // The command already failed; a leftover is named by SCRATCH_PREFIX.
            drop(fs::remove_dir_all(directory));
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

    #[test]
    fn a_bare_file_name_is_created_in_the_current_directory() {
        assert_eq!(parent_dir(Path::new("backup.db")), Path::new("."));
        assert_eq!(parent_dir(Path::new("a/backup.db")), Path::new("a"));
    }
}
