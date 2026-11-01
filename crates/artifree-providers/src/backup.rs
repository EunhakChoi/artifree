//! Consistent backup and restore of the local `SQLite` database.
//!
//! One database file holds every persisted domain: artifacts, revisions, file
//! bytes, request keys, the change feed, and access token records. A backup is
//! therefore one file, and a restore creates one file.
//!
//! A backup is taken with `VACUUM INTO` while the server runs. It reads one
//! snapshot in a single read transaction: writers are not blocked, and the
//! snapshot holds only commits that were complete when it began. The copy is
//! written inside a private scratch directory next to the destination, checked
//! (`integrity_check`, `foreign_key_check`, schema version), and only then
//! linked to its final name. The final step refuses an existing destination, so
//! no backup silently replaces another file.
//!
//! A restore copies the backup into a scratch directory next to the new
//! database, checks it, upgrades it to the current schema with the normal
//! store open, checks it again, and links it into place. It never writes to the
//! backup, and it refuses to replace an existing database.
//!
//! A backup holds every artifact byte and the token hashes unencrypted; protect
//! it like the data directory. It does not hold the access
//! token file, which is not stored in the database.

use std::fmt;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::storage::{SCHEMA_VERSION, SqliteStore};
use crate::tokens::SqliteTokenStore;

const DATABASE_NAME: &str = "artifree.sqlite3";

/// What a backup or restore produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupSummary {
    /// Schema version (`PRAGMA user_version`) of the resulting file.
    pub schema_version: i64,
    /// Size of the resulting file in bytes.
    pub bytes: u64,
}

/// Why a backup or restore was refused or failed. Nothing is left behind
/// except, for a restore into a new directory, that directory.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackupError {
    /// The source database or backup file does not exist.
    Missing(PathBuf),
    /// The destination already exists and is not replaced.
    Exists(PathBuf),
    /// The file uses a newer schema version than this build supports.
    NewerSchema {
        /// Version found in the file.
        found: i64,
        /// Newest version this build supports.
        supported: i64,
    },
    /// The file failed `SQLite`'s integrity or foreign key checks, or is not
    /// a database.
    Damaged(String),
    /// A file system or database operation failed.
    Failed(String),
}

impl fmt::Display for BackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(path) => write!(formatter, "{} does not exist", path.display()),
            Self::Exists(path) => write!(
                formatter,
                "{} already exists; it is not replaced",
                path.display()
            ),
            Self::NewerSchema { found, supported } => write!(
                formatter,
                "database schema version {found} is newer than this build supports ({supported}); use a newer artifree-server"
            ),
            Self::Damaged(reason) => write!(formatter, "database is damaged: {reason}"),
            Self::Failed(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for BackupError {}

/// Writes a consistent snapshot of the database at `database` to the new file
/// `output`, which may be taken while the server is running.
///
/// The output's directory must exist. The file gets owner-only permissions on
/// Unix. A database of an older schema is copied as it is; a restore upgrades
/// it.
///
/// # Errors
///
/// [`BackupError::Missing`] when `database` does not exist,
/// [`BackupError::Exists`] when `output` does,
/// [`BackupError::NewerSchema`] and [`BackupError::Damaged`] for a source that
/// cannot be backed up faithfully, and [`BackupError::Failed`] otherwise.
pub fn create_backup(database: &Path, output: &Path) -> Result<BackupSummary, BackupError> {
    if !database.is_file() {
        return Err(BackupError::Missing(database.to_owned()));
    }
    if output.symlink_metadata().is_ok() {
        return Err(BackupError::Exists(output.to_owned()));
    }
    let scratch = Scratch::beside(output)?;
    let snapshot = scratch.path().join(DATABASE_NAME);
    {
        // Read-write but never written: a read-only connection to a stopped
        // server's WAL database would create `-wal` and `-shm` files it cannot
        // remove, while the last read-write connection to close checkpoints and
        // removes them, leaving the data directory as it was.
        let source = Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(failed)?;
        source
            .busy_timeout(std::time::Duration::from_secs(10))
            .map_err(failed)?;
        let version = schema_version(&source)?;
        refuse_newer(version)?;
        let target = snapshot.to_str().ok_or_else(|| {
            BackupError::Failed(format!("{} is not valid UTF-8", snapshot.display()))
        })?;
        source.execute("VACUUM INTO ?1", [target]).map_err(failed)?;
    }
    let summary = check(&snapshot)?;
    publish(&snapshot, output)?;
    Ok(summary)
}

/// Creates the database `database` from the backup file `backup`, upgraded to
/// the current schema.
///
/// The backup is only read. `database` and its `-wal` and `-shm` files must
/// not exist, and its directory must.
///
/// # Errors
///
/// [`BackupError::Missing`] when `backup` does not exist,
/// [`BackupError::Exists`] when `database` does,
/// [`BackupError::NewerSchema`] and [`BackupError::Damaged`] for a backup that
/// is not restored, and [`BackupError::Failed`] otherwise.
pub fn restore_backup(backup: &Path, database: &Path) -> Result<BackupSummary, BackupError> {
    if !backup.is_file() {
        return Err(BackupError::Missing(backup.to_owned()));
    }
    for suffix in ["", "-wal", "-shm"] {
        let mut name = database.as_os_str().to_owned();
        name.push(suffix);
        let existing = PathBuf::from(name);
        if existing.symlink_metadata().is_ok() {
            return Err(BackupError::Exists(existing));
        }
    }
    let scratch = Scratch::beside(database)?;
    let copy = scratch.path().join(DATABASE_NAME);
    copy_new(backup, &copy)?;
    check(&copy)?;
    // The store open upgrades older schemas in one transaction and refuses
    // newer ones; both stores must see the file before it serves.
    SqliteStore::open(&copy).map_err(|error| BackupError::Failed(error.to_string()))?;
    SqliteTokenStore::open(&copy).map_err(|error| BackupError::Failed(error.to_string()))?;
    let summary = check(&copy)?;
    publish(&copy, database)?;
    Ok(summary)
}

fn refuse_newer(version: i64) -> Result<(), BackupError> {
    if version > SCHEMA_VERSION {
        return Err(BackupError::NewerSchema {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    Ok(())
}

fn schema_version(connection: &Connection) -> Result<i64, BackupError> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| BackupError::Damaged(error.to_string()))
}

/// Opens the file read-only and verifies it, returning its summary.
fn check(path: &Path) -> Result<BackupSummary, BackupError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| BackupError::Damaged(error.to_string()))?;
    let version = schema_version(&connection)?;
    refuse_newer(version)?;
    let verdicts: Vec<String> = {
        let mut statement = connection
            .prepare("PRAGMA integrity_check")
            .map_err(|error| BackupError::Damaged(error.to_string()))?;
        let rows = statement
            .query_map([], |row| row.get(0))
            .map_err(|error| BackupError::Damaged(error.to_string()))?;
        rows.collect::<Result<_, _>>()
            .map_err(|error| BackupError::Damaged(error.to_string()))?
    };
    if verdicts != ["ok"] {
        return Err(BackupError::Damaged(verdicts.join("; ")));
    }
    let violations: i64 = connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .map_err(|error| BackupError::Damaged(error.to_string()))?;
    if violations != 0 {
        return Err(BackupError::Damaged(format!(
            "{violations} rows violate foreign keys"
        )));
    }
    drop(connection);
    let bytes = fs::metadata(path)
        .map_err(io_failure("inspect", path))?
        .len();
    Ok(BackupSummary {
        schema_version: version,
        bytes,
    })
}

/// Moves a finished file to `destination` without replacing anything: the
/// hard link fails when the destination exists, also when it appeared after
/// the early check.
fn publish(finished: &Path, destination: &Path) -> Result<(), BackupError> {
    restrict(finished)?;
    fs::hard_link(finished, destination).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            BackupError::Exists(destination.to_owned())
        } else {
            BackupError::Failed(format!(
                "cannot write {}: {error}; the destination file system must support hard links",
                destination.display()
            ))
        }
    })
}

fn copy_new(from: &Path, to: &Path) -> Result<(), BackupError> {
    let mut source = File::open(from).map_err(io_failure("read", from))?;
    let mut target = File::create_new(to).map_err(io_failure("write", to))?;
    io::copy(&mut source, &mut target).map_err(io_failure("copy", from))?;
    target.sync_all().map_err(io_failure("write", to))
}

#[cfg(unix)]
fn restrict(path: &Path) -> Result<(), BackupError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(io_failure("restrict", path))
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Same signature as the Unix variant, which can fail."
)]
fn restrict(_path: &Path) -> Result<(), BackupError> {
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().create(path)
}

fn io_failure<'p>(action: &'p str, path: &'p Path) -> impl Fn(io::Error) -> BackupError + 'p {
    move |error| BackupError::Failed(format!("cannot {action} {}: {error}", path.display()))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Used as a map_err adapter, which passes the error by value."
)]
fn failed(error: rusqlite::Error) -> BackupError {
    BackupError::Failed(error.to_string())
}

/// A private, uniquely named directory next to a destination, removed on drop.
/// Files inside are unreachable to other users while they are being written.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn beside(destination: &Path) -> Result<Self, BackupError> {
        let parent = match destination.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random)
            .map_err(|error| BackupError::Failed(format!("no system randomness: {error}")))?;
        let path = parent.join(format!(
            ".artifree-scratch-{:016x}",
            u64::from_be_bytes(random)
        ));
        create_private_dir(&path).map_err(io_failure("create", &path))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a leftover scratch directory holds only a partial copy
        // and is safe to delete by hand.
        let _ = fs::remove_dir_all(&self.path);
    }
}
