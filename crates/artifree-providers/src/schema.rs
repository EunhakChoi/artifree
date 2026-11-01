//! The versioned schema of the database file: the one place that creates,
//! upgrades, and describes every persisted table.
//!
//! The artifact store, the access token store, and backup and restore all
//! share the database file `artifree.sqlite3`. Only [`upgrade`] changes its
//! schema; the token store requires the current version and creates nothing.
//! [`check_current`] compares a database with the schema this build creates,
//! without a second description of it: the reference is made by running the
//! same upgrade on an empty in-memory database.

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};

use artifree_ports::artifact_store::StoreError;

use crate::storage::{random_bytes, unavailable};

/// The schema version this build creates and reads. It is the database's
/// `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 6;

const SCHEMA: &str = "
CREATE TABLE artifacts (
    id TEXT PRIMARY KEY,
    owner TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    current_revision INTEGER NOT NULL CHECK (current_revision >= 1),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE INDEX artifacts_by_owner ON artifacts (owner, updated_at DESC, id);
CREATE TABLE revisions (
    artifact_id TEXT NOT NULL REFERENCES artifacts (id),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (artifact_id, revision)
) STRICT, WITHOUT ROWID;
CREATE TABLE blobs (
    digest BLOB PRIMARY KEY CHECK (length(digest) = 32),
    content BLOB NOT NULL
) STRICT;
CREATE TABLE revision_files (
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    path TEXT NOT NULL,
    size INTEGER NOT NULL CHECK (size >= 0),
    digest BLOB NOT NULL REFERENCES blobs (digest),
    PRIMARY KEY (artifact_id, revision, path),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision)
) STRICT, WITHOUT ROWID;
CREATE TABLE request_keys (
    actor TEXT NOT NULL,
    request_key TEXT NOT NULL,
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (actor, request_key),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision)
) STRICT, WITHOUT ROWID;
";

/// Added in schema version 2. `sqlite_sequence` keeps the largest sequence
/// ever issued, so the head survives future pruning of old rows.
const FEED_SCHEMA: &str = "
CREATE TABLE feed (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    id BLOB NOT NULL CHECK (length(id) = 16)
) STRICT;
CREATE TABLE changes (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('created', 'revised')),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision)
) STRICT;
";

/// Added in schema version 3: titles and pins per metadata revision, and the
/// metadata revision of each request key and change. Existing artifacts get
/// metadata revision 1 with their current title, unpinned. Tables whose
/// constraints change are rebuilt; `sqlite_sequence` keeps the feed's head.
const METADATA_SCHEMA: &str = "
ALTER TABLE artifacts ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1));
ALTER TABLE artifacts ADD COLUMN metadata_revision INTEGER NOT NULL DEFAULT 1
    CHECK (metadata_revision >= 1);
CREATE INDEX artifacts_by_owner_created ON artifacts (owner, created_at DESC, id);
CREATE TABLE metadata_revisions (
    artifact_id TEXT NOT NULL REFERENCES artifacts (id),
    metadata_revision INTEGER NOT NULL CHECK (metadata_revision >= 1),
    title TEXT NOT NULL,
    pinned INTEGER NOT NULL CHECK (pinned IN (0, 1)),
    created_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (artifact_id, metadata_revision)
) STRICT, WITHOUT ROWID;
INSERT INTO metadata_revisions (artifact_id, metadata_revision, title, pinned, created_by, created_at)
    SELECT id, 1, title, 0, owner, created_at FROM artifacts;
CREATE TABLE request_keys_v3 (
    actor TEXT NOT NULL,
    request_key TEXT NOT NULL,
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    metadata_revision INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (actor, request_key),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    FOREIGN KEY (artifact_id, metadata_revision)
        REFERENCES metadata_revisions (artifact_id, metadata_revision)
) STRICT, WITHOUT ROWID;
INSERT INTO request_keys_v3
    SELECT actor, request_key, fingerprint, artifact_id, revision, 1, created_at FROM request_keys;
DROP TABLE request_keys;
ALTER TABLE request_keys_v3 RENAME TO request_keys;
CREATE TABLE changes_v3 (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('created', 'revised', 'metadata_updated')),
    revision INTEGER NOT NULL,
    metadata_revision INTEGER NOT NULL,
    committed_at INTEGER NOT NULL,
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    FOREIGN KEY (artifact_id, metadata_revision)
        REFERENCES metadata_revisions (artifact_id, metadata_revision)
) STRICT;
INSERT INTO changes_v3 (sequence, artifact_id, kind, revision, metadata_revision, committed_at)
    SELECT c.sequence, c.artifact_id, c.kind, c.revision, 1, r.created_at
    FROM changes c
    JOIN revisions r ON r.artifact_id = c.artifact_id AND r.revision = c.revision;
DROP TABLE changes;
ALTER TABLE changes_v3 RENAME TO changes;
";

/// Added in schema version 4: the latest commit time, which the next commit
/// must exceed. It starts at the latest time any earlier commit stored, also
/// when stored times are out of order or have no feed entry.
const COMMIT_TIME_SCHEMA: &str = "
ALTER TABLE feed ADD COLUMN committed_at INTEGER NOT NULL DEFAULT 0;
UPDATE feed SET committed_at = coalesce((SELECT max(at) FROM (
    SELECT max(created_at) AS at FROM revisions
    UNION ALL SELECT max(created_at) FROM artifacts
    UNION ALL SELECT max(updated_at) FROM artifacts
    UNION ALL SELECT max(created_at) FROM metadata_revisions
    UNION ALL SELECT max(committed_at) FROM changes
)), 0);
";

/// Added in schema version 5: when an artifact went to the trash, also per
/// metadata revision, and the two change kinds of the trash. The change table
/// is rebuilt for its checked kinds.
const TRASH_SCHEMA: &str = "
ALTER TABLE artifacts ADD COLUMN deleted_at INTEGER;
ALTER TABLE metadata_revisions ADD COLUMN deleted_at INTEGER;
CREATE TABLE changes_v5 (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id TEXT NOT NULL,
    kind TEXT NOT NULL
        CHECK (kind IN ('created', 'revised', 'metadata_updated', 'deleted', 'recovered')),
    revision INTEGER NOT NULL,
    metadata_revision INTEGER NOT NULL,
    committed_at INTEGER NOT NULL,
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    FOREIGN KEY (artifact_id, metadata_revision)
        REFERENCES metadata_revisions (artifact_id, metadata_revision)
) STRICT;
INSERT INTO changes_v5 (sequence, artifact_id, kind, revision, metadata_revision, committed_at)
    SELECT sequence, artifact_id, kind, revision, metadata_revision, committed_at FROM changes;
DROP TABLE changes;
ALTER TABLE changes_v5 RENAME TO changes;
";

/// Added in schema version 6: access tokens. Earlier builds created this table
/// outside the versioned schema, so a version 5 database may already have it.
const TOKEN_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tokens (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    secret_hash TEXT NOT NULL UNIQUE,
    prefix TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS idx_tokens_secret_hash ON tokens (secret_hash);
";

/// Brings the database to [`SCHEMA_VERSION`] in one immediate transaction, or
/// leaves it as it was. An empty database (version 0, no tables) is created
/// fresh; an older version is upgraded step by step.
///
/// It never changes the journal mode: a refused or failed upgrade must leave
/// the file byte for byte as it was, and switching to WAL rewrites the header.
/// The caller does that after success.
///
/// # Errors
///
/// [`StoreError::Unavailable`] when the version is newer than this build
/// knows, when version 0 belongs to a database that already has tables (not
/// an Artifree database), or when a step fails. The transaction is then rolled back.
pub(crate) fn upgrade(connection: &mut Connection) -> Result<(), StoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let version: i64 = transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(unavailable)?;
    if !(0..=SCHEMA_VERSION).contains(&version) {
        return Err(StoreError::Unavailable(format!(
            "database schema version {version} is not supported by this build (expected {SCHEMA_VERSION})"
        )));
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    if version < 1 {
        let objects: i64 = transaction
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
            .map_err(unavailable)?;
        if objects > 0 {
            return Err(StoreError::Unavailable(
                "the file is a database without an Artifree schema version".to_owned(),
            ));
        }
        transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    }
    if version < 2 {
        transaction
            .execute_batch(FEED_SCHEMA)
            .map_err(unavailable)?;
        transaction
            .execute(
                "INSERT INTO feed (singleton, id) VALUES (1, ?1)",
                [random_bytes()?.as_slice()],
            )
            .map_err(unavailable)?;
    }
    if version < 3 {
        keeping_feed_head(&transaction, METADATA_SCHEMA)?;
    }
    if version < 4 {
        transaction
            .execute_batch(COMMIT_TIME_SCHEMA)
            .map_err(unavailable)?;
    }
    if version < 5 {
        keeping_feed_head(&transaction, TRASH_SCHEMA)?;
    }
    transaction
        .execute_batch(TOKEN_SCHEMA)
        .map_err(unavailable)?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

/// One schema object as `SQLite` records it, with whitespace normalized.
#[derive(Debug, PartialEq, Eq)]
struct Object {
    kind: String,
    name: String,
    table: String,
    sql: String,
}

fn objects(connection: &Connection) -> Result<Vec<Object>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT type, name, tbl_name, coalesce(sql, '') FROM sqlite_master
         WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY type, name",
    )?;
    let rows = statement.query_map([], |row| {
        let sql: String = row.get(3)?;
        Ok(Object {
            kind: row.get(0)?,
            name: row.get(1)?,
            table: row.get(2)?,
            sql: sql.split_whitespace().collect::<Vec<_>>().join(" "),
        })
    })?;
    rows.collect()
}

/// Checks that the database holds exactly the schema this build creates, and
/// the single feed row the change feed needs. Run it on a database already at
/// [`SCHEMA_VERSION`].
///
/// # Errors
///
/// A description of the first missing, extra, or different object.
pub(crate) fn check_current(connection: &Connection) -> Result<(), String> {
    let mut reference = Connection::open_in_memory().map_err(|error| error.to_string())?;
    upgrade(&mut reference).map_err(|error| error.to_string())?;
    let expected = objects(&reference).map_err(|error| error.to_string())?;
    let actual = objects(connection).map_err(|error| error.to_string())?;
    for wanted in &expected {
        match actual.iter().find(|found| found.name == wanted.name) {
            None => return Err(format!("{} {} is missing", wanted.kind, wanted.name)),
            Some(found) if found != wanted => {
                return Err(format!(
                    "{} {} differs from this build's schema",
                    wanted.kind, wanted.name
                ));
            }
            Some(_) => {}
        }
    }
    if let Some(extra) = actual
        .iter()
        .find(|found| !expected.iter().any(|wanted| wanted.name == found.name))
    {
        return Err(format!("unexpected {} {}", extra.kind, extra.name));
    }
    let feeds: i64 = connection
        .query_row("SELECT count(*) FROM feed", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if feeds != 1 {
        return Err(format!("the feed table has {feeds} rows, expected 1"));
    }
    Ok(())
}

/// Gives the change feed a new random identity. The head of the sequence is
/// untouched, so sequences never go back; only cursors of the old identity
/// stop matching.
///
/// # Errors
///
/// [`StoreError::Unavailable`] without randomness or when the feed row is missing.
pub(crate) fn renew_feed_identity(connection: &Connection) -> Result<(), StoreError> {
    let changed = connection
        .execute("UPDATE feed SET id = ?1", [random_bytes()?.as_slice()])
        .map_err(unavailable)?;
    if changed == 1 {
        Ok(())
    } else {
        Err(StoreError::Unavailable(format!(
            "the feed table has {changed} rows, expected 1"
        )))
    }
}

/// Applies `schema`, which rebuilds the `changes` table, and keeps the change
/// feed's head, which the rebuilt table would otherwise take from its largest
/// row.
fn keeping_feed_head(transaction: &Transaction<'_>, schema: &str) -> Result<(), StoreError> {
    let head: Option<i64> = transaction
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = 'changes'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    transaction.execute_batch(schema).map_err(unavailable)?;
    if let Some(head) = head {
        let updated = transaction
            .execute(
                "UPDATE sqlite_sequence SET seq = max(seq, ?1) WHERE name = 'changes'",
                [head],
            )
            .map_err(unavailable)?;
        if updated == 0 {
            transaction
                .execute(
                    "INSERT INTO sqlite_sequence (name, seq) VALUES ('changes', ?1)",
                    [head],
                )
                .map_err(unavailable)?;
        }
    }
    Ok(())
}
