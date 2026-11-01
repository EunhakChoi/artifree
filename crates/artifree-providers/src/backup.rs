//! Consistent backup and restore of the local `SQLite` database.
//!
//! One database file holds every persisted domain: artifacts, revisions, file
//! bytes, request keys, the change feed, and access token records. A backup is
//! therefore one file, and a restore creates one file.
//!
//! A backup is taken with `VACUUM INTO` while the server runs. It reads one
//! snapshot in a single read transaction (in WAL mode `SQLite` lets writers
//! proceed meanwhile), and the snapshot holds only commits that were complete
//! when it began. The copy is written inside a private scratch directory next
//! to the destination and checked: `integrity_check`, `foreign_key_check`, a
//! schema version of 1 or more, and, for the current version, a schema equal
//! to the one this build creates. It is then flushed and linked to its final
//! name, and the destination directory is flushed (Unix). The final step refuses an existing destination, so
//! no backup silently replaces another file.
//!
//! A restore copies the backup into a scratch directory next to the new
//! database, checks it, upgrades it to the current schema with the normal
//! store open, checks it again (including the schema comparison), and links it
//! into place. It never writes to the
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

use crate::storage::{DATABASE_FILE, SCHEMA_VERSION, SqliteStore};
use crate::tokens::SqliteTokenStore;

/// Name prefix of the private scratch directories a backup or restore creates
/// next to its destination. An interrupted run can leave one behind; it holds
/// at most a partial copy of the database and is safe to delete.
pub const SCRATCH_PREFIX: &str = ".artifree-scratch-";

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
/// it. A snapshot of the current schema must have exactly the tables, indexes,
/// and constraints this build creates (the optional token table aside).
///
/// The snapshot and, on Unix, the destination directory are flushed to disk
/// before success is reported. Windows has no directory flush in the standard
/// library, so there only the file is flushed.
///
/// # Errors
///
/// [`BackupError::Missing`] when `database` does not exist,
/// [`BackupError::Exists`] when `output` does,
/// [`BackupError::NewerSchema`] and [`BackupError::Damaged`] for a source that
/// cannot be backed up faithfully (including one that is not an Artifree
/// database), and [`BackupError::Failed`] otherwise.
pub fn create_backup(database: &Path, output: &Path) -> Result<BackupSummary, BackupError> {
    if !database.is_file() {
        return Err(BackupError::Missing(database.to_owned()));
    }
    if output.symlink_metadata().is_ok() {
        return Err(BackupError::Exists(output.to_owned()));
    }
    let scratch = Scratch::beside(output)?;
    let snapshot = scratch.path().join(DATABASE_FILE);
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
        accept_version(schema_version(&source)?)?;
        let target = snapshot.to_str().ok_or_else(|| {
            BackupError::Failed(format!("{} is not valid UTF-8", snapshot.display()))
        })?;
        source.execute("VACUUM INTO ?1", [target]).map_err(failed)?;
    }
    let summary = check(&snapshot, Integrity::Full)?;
    if summary.schema_version == SCHEMA_VERSION {
        // An older snapshot is checked after the migration of its restore.
        verify_schema(&snapshot, Tokens::Optional, &scratch)?;
    }
    publish(&snapshot, output)?;
    Ok(summary)
}

/// Creates the database `database` from the backup file `backup`, upgraded to
/// the current schema.
///
/// The backup is only read. `database` and its `-wal`, `-shm`, and `-journal`
/// files must not exist, and its directory must. The result must have exactly
/// the schema this build creates, or nothing is published. The file and, on
/// Unix, its directory are flushed to disk before success is reported.
///
/// # Errors
///
/// [`BackupError::Missing`] when `backup` does not exist,
/// [`BackupError::Exists`] when `database` or one of its side files does,
/// [`BackupError::NewerSchema`] and [`BackupError::Damaged`] for a backup that
/// is not restored (including an empty file or another application's
/// database), and [`BackupError::Failed`] otherwise.
pub fn restore_backup(backup: &Path, database: &Path) -> Result<BackupSummary, BackupError> {
    if !backup.is_file() {
        return Err(BackupError::Missing(backup.to_owned()));
    }
    // A stale `-journal` could be rolled back into the new file.
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut name = database.as_os_str().to_owned();
        name.push(suffix);
        let existing = PathBuf::from(name);
        if existing.symlink_metadata().is_ok() {
            return Err(BackupError::Exists(existing));
        }
    }
    let scratch = Scratch::beside(database)?;
    let copy = scratch.path().join(DATABASE_FILE);
    copy_new(backup, &copy)?;
    check(&copy, Integrity::Full)?;
    // The store open upgrades older schemas in one transaction and refuses
    // newer ones; both stores must see the file before it serves.
    SqliteStore::open(&copy).map_err(|error| BackupError::Failed(error.to_string()))?;
    SqliteTokenStore::open(&copy).map_err(|error| BackupError::Failed(error.to_string()))?;
    let summary = check(&copy, Integrity::Quick)?;
    verify_schema(&copy, Tokens::Required, &scratch)?;
    publish(&copy, database)?;
    Ok(summary)
}

/// Accepts the schema versions that were ever released and this build reads.
/// Version 0 is an empty or foreign database: Artifree never shipped it.
fn accept_version(version: i64) -> Result<(), BackupError> {
    if version > SCHEMA_VERSION {
        return Err(BackupError::NewerSchema {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if version < 1 {
        return Err(BackupError::Damaged(format!(
            "schema version {version}: this is not an Artifree database"
        )));
    }
    Ok(())
}

fn schema_version(connection: &Connection) -> Result<i64, BackupError> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| BackupError::Damaged(error.to_string()))
}

#[derive(Clone, Copy)]
enum Integrity {
    Full,
    Quick,
}

/// Opens the file read-only and verifies it, returning its summary.
fn check(path: &Path, depth: Integrity) -> Result<BackupSummary, BackupError> {
    let damaged = |error: rusqlite::Error| BackupError::Damaged(error.to_string());
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(damaged)?;
    let version = schema_version(&connection)?;
    accept_version(version)?;
    let pragma = match depth {
        Integrity::Full => "PRAGMA integrity_check",
        Integrity::Quick => "PRAGMA quick_check",
    };
    let verdicts: Vec<String> = {
        let mut statement = connection.prepare(pragma).map_err(damaged)?;
        let rows = statement.query_map([], |row| row.get(0)).map_err(damaged)?;
        rows.collect::<Result<_, _>>().map_err(damaged)?
    };
    if verdicts != ["ok"] {
        return Err(BackupError::Damaged(verdicts.join("; ")));
    }
    let violations: i64 = connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .map_err(damaged)?;
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

/// Whether the token table belongs to the expected schema.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tokens {
    /// A current-schema database may predate the token table.
    Optional,
    /// The token store has opened the file.
    Required,
}

type Schema = Vec<(String, String, String, String)>;

/// Requires the file's tables, indexes, and constraints to be those this build
/// creates, by comparing `sqlite_master` with a database the build creates
/// now. Whitespace and quoting are ignored because `ALTER` and renames change
/// them without changing the schema.
fn verify_schema(path: &Path, tokens: Tokens, scratch: &Scratch) -> Result<(), BackupError> {
    let reference = scratch.path().join("reference.sqlite3");
    SqliteStore::open(&reference).map_err(|error| BackupError::Failed(error.to_string()))?;
    if tokens == Tokens::Required {
        SqliteTokenStore::open(&reference)
            .map_err(|error| BackupError::Failed(error.to_string()))?;
    }
    let expected = schema_of(&reference, tokens)?;
    let actual = schema_of(path, tokens)?;
    if expected == actual {
        return Ok(());
    }
    let mut differing: Vec<&str> = expected
        .iter()
        .filter(|entry| !actual.contains(entry))
        .chain(actual.iter().filter(|entry| !expected.contains(entry)))
        .map(|(_, name, _, _)| name.as_str())
        .collect();
    differing.sort_unstable();
    differing.dedup();
    Err(BackupError::Damaged(format!(
        "the schema differs from the one this build creates: {}",
        differing.join(", ")
    )))
}

fn schema_of(path: &Path, tokens: Tokens) -> Result<Schema, BackupError> {
    let damaged = |error: rusqlite::Error| BackupError::Damaged(error.to_string());
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(damaged)?;
    let mut statement = connection
        .prepare(
            "SELECT type, name, tbl_name, coalesce(sql, '') FROM sqlite_master \
             ORDER BY type, name",
        )
        .map_err(damaged)?;
    let rows = statement
        .query_map([], |row| {
            let sql: String = row.get(3)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                sql.chars()
                    .filter(|c| !c.is_whitespace() && *c != '"')
                    .collect(),
            ))
        })
        .map_err(damaged)?;
    let mut schema: Schema = rows.collect::<Result<_, _>>().map_err(damaged)?;
    if tokens == Tokens::Optional {
        schema.retain(|(_, _, table, _)| table != "tokens");
    }
    Ok(schema)
}

/// Moves a finished file to `destination` without replacing anything: the
/// hard link fails when the destination exists, also when it appeared after
/// the early check.
fn publish(finished: &Path, destination: &Path) -> Result<(), BackupError> {
    // Mode first, then flush: `VACUUM INTO` does not flush its output and the
    // migration of a restore wrote after the copy was flushed.
    restrict(finished)?;
    sync_file(finished)?;
    let parent = match destination.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::hard_link(finished, destination).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            BackupError::Exists(destination.to_owned())
        } else {
            BackupError::Failed(format!(
                "cannot write {}: {error}; the destination file system must support hard links",
                destination.display()
            ))
        }
    })?;
    // The new directory entry is durable only once its directory is flushed.
    sync_directory(parent)
}

fn sync_file(path: &Path) -> Result<(), BackupError> {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(io_failure("flush", path))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), BackupError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(io_failure("flush", path))
}

/// Windows offers no directory flush through the standard library; the file
/// itself was flushed.
#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Same signature as the Unix variant, which can fail."
)]
fn sync_directory(_path: &Path) -> Result<(), BackupError> {
    Ok(())
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
            "{SCRATCH_PREFIX}{:016x}",
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
