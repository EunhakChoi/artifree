//! The versioned schema of the database file: the one place that creates,
//! upgrades, and describes every persisted table.
//!
//! The artifact store, the access token store, and backup and restore all
//! share the database file `artifree.sqlite3`. Only [`upgrade`] changes its
//! schema; the token store requires the current version and creates nothing.
//! [`check_current`] compares a database with the schema this build creates,
//! without a second description of it: the reference is made by running the
//! same upgrade on an empty in-memory database.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};

use artifree_ports::artifact_store::StoreError;

use crate::storage::{random_bytes, unavailable};

/// The schema version this build creates and reads. It is the database's
/// `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 10;

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

/// Added in schema version 7 (decision 0029): the tombstone of an artifact
/// removed for good, the change kind for it, the record of a cleanup of
/// unreferenced content (the key it ran under and how much it deleted, never
/// what), and an index on the content a
/// revision file refers to, which finding unreferenced content and deleting
/// content both read. A purge needs the artifact in the trash, which the
/// column's check states. The change table is rebuilt for its checked kinds.
const PURGE_SCHEMA: &str = "
ALTER TABLE artifacts ADD COLUMN purged_at INTEGER
    CHECK (purged_at IS NULL OR deleted_at IS NOT NULL);
CREATE INDEX revision_files_by_digest ON revision_files (digest);
CREATE TABLE cleanup_runs (
    actor TEXT NOT NULL,
    request_key TEXT NOT NULL,
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    blobs INTEGER NOT NULL CHECK (blobs >= 0),
    bytes INTEGER NOT NULL CHECK (bytes >= 0),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (actor, request_key)
) STRICT, WITHOUT ROWID;
CREATE TABLE changes_v7 (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN
        ('created', 'revised', 'metadata_updated', 'deleted', 'recovered', 'purged')),
    revision INTEGER NOT NULL,
    metadata_revision INTEGER NOT NULL,
    committed_at INTEGER NOT NULL,
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    FOREIGN KEY (artifact_id, metadata_revision)
        REFERENCES metadata_revisions (artifact_id, metadata_revision)
) STRICT;
INSERT INTO changes_v7 (sequence, artifact_id, kind, revision, metadata_revision, committed_at)
    SELECT sequence, artifact_id, kind, revision, metadata_revision, committed_at FROM changes;
DROP TABLE changes;
ALTER TABLE changes_v7 RENAME TO changes;
";

/// Added in schema version 8 (decision 0030): persistent jobs, the keys of the
/// requests that enqueued and cancelled them, and the mismatches a
/// verification found. A job pins its actor, artifact, and revision; its state
/// and lease columns agree through checks, so no write can leave a running job
/// without a lease or an ended job without an end time. None of these tables
/// holds a content digest: a job reads its revision's files through
/// `revision_files`, which the unreferenced-content rule already counts.
const JOB_SCHEMA: &str = "
CREATE TABLE jobs (
    id TEXT PRIMARY KEY,
    actor TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('verify_revision')),
    state TEXT NOT NULL CHECK (state IN
        ('queued', 'running', 'retry_scheduled', 'succeeded', 'failed', 'cancelled')),
    attempts INTEGER NOT NULL CHECK (attempts >= 0),
    max_attempts INTEGER NOT NULL CHECK (max_attempts >= 1),
    fence INTEGER NOT NULL CHECK (fence >= 0),
    run_after INTEGER NOT NULL,
    worker TEXT,
    lease_expires_at INTEGER,
    cancel_requested INTEGER NOT NULL CHECK (cancel_requested IN (0, 1)),
    failure TEXT CHECK (failure IN
        ('revision_unavailable', 'storage_unavailable', 'lease_expired', 'internal')),
    files_checked INTEGER CHECK (files_checked >= 0),
    bytes_checked INTEGER CHECK (bytes_checked >= 0),
    mismatch_count INTEGER CHECK (mismatch_count >= 0),
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER,
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    CHECK (attempts <= max_attempts),
    CHECK ((state = 'running') = (worker IS NOT NULL AND lease_expires_at IS NOT NULL)),
    CHECK ((state IN ('succeeded', 'failed', 'cancelled')) = (finished_at IS NOT NULL)),
    CHECK ((state = 'failed') = (failure IS NOT NULL)),
    CHECK (state = 'succeeded'
        OR (files_checked IS NULL AND bytes_checked IS NULL AND mismatch_count IS NULL))
) STRICT;
CREATE INDEX jobs_by_actor ON jobs (actor, created_at DESC, id);
CREATE INDEX jobs_by_artifact ON jobs (artifact_id, created_at DESC, id);
CREATE INDEX jobs_by_state ON jobs (state, run_after);
CREATE TABLE job_requests (
    actor TEXT NOT NULL,
    request_key TEXT NOT NULL,
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    job_id TEXT NOT NULL REFERENCES jobs (id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (actor, request_key)
) STRICT, WITHOUT ROWID;
CREATE TABLE job_findings (
    job_id TEXT NOT NULL REFERENCES jobs (id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    path TEXT NOT NULL,
    problem TEXT NOT NULL CHECK (problem IN
        ('digest_mismatch', 'size_mismatch', 'content_missing')),
    PRIMARY KEY (job_id, ordinal)
) STRICT, WITHOUT ROWID;
";

/// Added in schema version 9 (decision 0031): application data. A data space
/// is one JSON object per actor and artifact, kept as canonical text with its
/// own version; the version is not the source revision or the metadata
/// revision. `data_requests` holds the keys of data writes: the version a key
/// left the space at, and whether it changed anything. It is a table of its own
/// because a `request_keys` row names a source revision, which a data write has
/// none of. Neither table holds a content digest or a blob.
const DATA_SCHEMA: &str = "
CREATE TABLE app_data (
    artifact_id TEXT NOT NULL REFERENCES artifacts (id),
    actor TEXT NOT NULL,
    data_version INTEGER NOT NULL CHECK (data_version >= 1),
    document TEXT NOT NULL CHECK (json_valid(document) AND json_type(document) = 'object'),
    PRIMARY KEY (artifact_id, actor)
) STRICT;
CREATE TABLE data_requests (
    actor TEXT NOT NULL,
    request_key TEXT NOT NULL,
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    artifact_id TEXT NOT NULL REFERENCES artifacts (id),
    data_version INTEGER NOT NULL CHECK (data_version >= 0),
    changed INTEGER NOT NULL CHECK (changed IN (0, 1)),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (actor, request_key)
) STRICT, WITHOUT ROWID;
";

/// Added in schema version 10 (decision 0036): the registry of users and the
/// owner of each token and change.
///
/// `users` holds the local owner profile (`local`, which every earlier
/// database already used as the actor of every artifact and token) and the
/// users it registers. The check on `id` is the one place that fixes the
/// spelling of an actor id the registry can hold, as `ActorId::is_registrable`
/// does in code, and the owner can never be disabled. No other table refers to
/// `users`, except `tokens`: artifacts, keys, jobs, and data keep naming an
/// actor as text, so a database or a test that names an actor the registry
/// never saw still works, and nothing about them is rebuilt.
///
/// `tokens` is rebuilt with the user it authenticates as, `local` for every
/// token that exists. `changes` is rebuilt with the actor and a sequence of
/// the actor's own: the global `sequence` keeps its role as the primary key
/// and the order of writes, but the cursor a client sees is `actor_seq`,
/// so that the head of one user's feed counts only that user's changes.
/// Existing rows keep their sequence number as `actor_seq` (so every
/// cursor handed out before stays valid), and `actor_feeds` starts each actor
/// that has changes at the old global head, so a head never moves back. A
/// change row whose artifact row is missing (Artifree enforces foreign keys and
/// never deletes an artifact row, so only a file edited elsewhere can hold one)
/// is kept under the owner profile, and the upgrade fails without changing
/// anything if the rebuilt feed holds fewer rows than the old one.
const USER_SCHEMA: &str = "
CREATE TABLE users (
    id TEXT PRIMARY KEY CHECK (id = 'local' OR (length(id) = 34 AND substr(id, 1, 2) = 'u-'
        AND NOT substr(id, 3) GLOB '*[^0-9a-f]*')),
    label TEXT NOT NULL UNIQUE CHECK (length(label) BETWEEN 1 AND 64),
    created_at TEXT NOT NULL,
    disabled_at TEXT,
    state_generation INTEGER NOT NULL DEFAULT 1 CHECK (state_generation >= 1),
    CHECK (id <> 'local' OR disabled_at IS NULL)
) STRICT, WITHOUT ROWID;
INSERT INTO users (id, label, created_at)
    VALUES ('local', 'Local owner', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));
CREATE TABLE tokens_v10 (
    id TEXT PRIMARY KEY,
    actor TEXT NOT NULL REFERENCES users (id),
    name TEXT NOT NULL,
    secret_hash TEXT NOT NULL UNIQUE,
    prefix TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at TEXT
) STRICT;
INSERT INTO tokens_v10 (id, actor, name, secret_hash, prefix, created_at, last_used_at, revoked_at)
    SELECT id, 'local', name, secret_hash, prefix, created_at, last_used_at, revoked_at FROM tokens;
DROP TABLE tokens;
ALTER TABLE tokens_v10 RENAME TO tokens;
CREATE INDEX idx_tokens_secret_hash ON tokens (secret_hash);
CREATE INDEX tokens_by_actor ON tokens (actor);
CREATE TABLE actor_feeds (
    actor TEXT PRIMARY KEY,
    head INTEGER NOT NULL CHECK (head >= 0)
) STRICT, WITHOUT ROWID;
CREATE TABLE changes_v10 (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    actor TEXT NOT NULL,
    actor_seq INTEGER NOT NULL CHECK (actor_seq >= 1),
    artifact_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN
        ('created', 'revised', 'metadata_updated', 'deleted', 'recovered', 'purged')),
    revision INTEGER NOT NULL,
    metadata_revision INTEGER NOT NULL,
    committed_at INTEGER NOT NULL,
    UNIQUE (actor, actor_seq),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    FOREIGN KEY (artifact_id, metadata_revision)
        REFERENCES metadata_revisions (artifact_id, metadata_revision)
) STRICT;
INSERT INTO changes_v10 (sequence, actor, actor_seq, artifact_id, kind, revision,
        metadata_revision, committed_at)
    SELECT c.sequence, coalesce(a.owner, 'local'), c.sequence, c.artifact_id, c.kind,
        c.revision, c.metadata_revision, c.committed_at
    FROM changes c LEFT JOIN artifacts a ON a.id = c.artifact_id;
DROP TABLE changes;
ALTER TABLE changes_v10 RENAME TO changes;
";

/// The head every actor that has changes starts at when the feed gets a
/// sequence per actor: the global head, which is at least every sequence the
/// actor was ever given. Run after [`USER_SCHEMA`] and after the global head
/// was restored.
const ACTOR_FEED_HEADS: &str = "
INSERT INTO actor_feeds (actor, head)
    SELECT DISTINCT c.actor, coalesce((SELECT seq FROM sqlite_sequence WHERE name = 'changes'), 0)
    FROM changes c;
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
    if version < 7 {
        keeping_feed_head(&transaction, PURGE_SCHEMA)?;
    }
    if version < 8 {
        transaction.execute_batch(JOB_SCHEMA).map_err(unavailable)?;
    }
    if version < 9 {
        transaction
            .execute_batch(DATA_SCHEMA)
            .map_err(unavailable)?;
    }
    // The version 6 table, which version 10 rebuilds: it is made before
    // that step, and in a database that has it already this changes nothing.
    transaction
        .execute_batch(TOKEN_SCHEMA)
        .map_err(unavailable)?;
    if version < 10 {
        let before = change_rows(&transaction)?;
        keeping_feed_head(&transaction, USER_SCHEMA)?;
        transaction
            .execute_batch(ACTOR_FEED_HEADS)
            .map_err(unavailable)?;
        // The rebuilt feed holds every row the old one did, or the upgrade
        // fails and nothing is changed: a cursor a client holds must not
        // point at a position that is gone.
        let after = change_rows(&transaction)?;
        if before != after {
            return Err(StoreError::Unavailable(format!(
                "the upgrade to schema 10 would lose change rows ({before} before, {after} after)"
            )));
        }
    }
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

/// What the schema is, described by `SQLite`'s own introspection and not by
/// the text it stores. `ALTER TABLE ... RENAME` rewrites stored SQL text in a
/// way that may differ between `SQLite` versions, so comparing text would make a
/// library upgrade refuse real databases.
///
/// Every object by kind and table; for each table its columns (type, not-null,
/// default, primary key, hidden), strictness, `WITHOUT ROWID`, indexes with
/// their columns, uniqueness and partiality, foreign keys, and the clauses read
/// by [`clauses`]. That is the one place that compares text, because `SQLite`
/// has no introspection for it; a rename of a table does not rewrite those
/// clauses. The table-valued pragma functions need `SQLite` 3.37; the bundled
/// library is newer.
fn shape(connection: &Connection) -> Result<BTreeMap<String, Vec<String>>, rusqlite::Error> {
    let mut shape = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT type, name, tbl_name, coalesce(sql, '') FROM sqlite_master
         WHERE name NOT LIKE 'sqlite!_%' ESCAPE '!' ORDER BY type, name",
    )?;
    let objects = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (kind, name, table, sql) in objects {
        let mut lines = vec![format!("on {table}")];
        if kind == "table" {
            lines.extend(table_lines(connection, &name, &sql)?);
        } else if kind == "index" {
            lines.extend(clauses(&sql));
        } else {
            // A view or trigger: no build creates one, so its text must match.
            lines.push(sql.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        shape.insert(format!("{kind} {name}"), lines);
    }
    Ok(shape)
}

fn table_lines(
    connection: &Connection,
    table: &str,
    sql: &str,
) -> Result<Vec<String>, rusqlite::Error> {
    let mut lines = Vec::new();
    lines.extend(described(
        connection,
        "SELECT 'column', cid, name, type, \"notnull\", dflt_value, pk, hidden
         FROM pragma_table_xinfo(?1) ORDER BY cid",
        [table],
    )?);
    lines.extend(described(
        connection,
        "SELECT 'table', strict, wr FROM pragma_table_list(?1) WHERE schema = 'main'",
        [table],
    )?);
    lines.extend(described(
        connection,
        "SELECT 'foreign key', id, seq, \"table\", \"from\", \"to\", on_update, on_delete, match
         FROM pragma_foreign_key_list(?1) ORDER BY id, seq",
        [table],
    )?);
    let indexes: Vec<String> = connection
        .prepare("SELECT name FROM pragma_index_list(?1) ORDER BY name")?
        .query_map([table], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for index in &indexes {
        lines.push(format!("index {index}"));
        lines.extend(described(
            connection,
            "SELECT 'key', seqno, cid, name, desc, coll, key
             FROM pragma_index_xinfo(?1) ORDER BY seqno",
            [index],
        )?);
        lines.extend(described(
            connection,
            "SELECT 'flags', \"unique\", origin, partial
             FROM pragma_index_list(?2) WHERE name = ?1",
            [index.as_str(), table],
        )?);
    }
    lines.extend(clauses(sql));
    Ok(lines)
}

/// Runs `query` and renders each row as its fields joined by `|`.
fn described<P: rusqlite::Params>(
    connection: &Connection,
    query: &str,
    parameters: P,
) -> Result<Vec<String>, rusqlite::Error> {
    use rusqlite::types::ValueRef;
    let mut statement = connection.prepare(query)?;
    let count = statement.column_count();
    let rows = statement.query_map(parameters, |row| {
        let mut fields = Vec::with_capacity(count);
        for index in 0..count {
            fields.push(match row.get_ref(index)? {
                ValueRef::Null => "\u{1}null".to_owned(),
                ValueRef::Integer(number) => number.to_string(),
                ValueRef::Real(number) => number.to_string(),
                ValueRef::Text(text) => String::from_utf8_lossy(text).into_owned(),
                ValueRef::Blob(_) => "blob".to_owned(),
            });
        }
        Ok(fields.join("\u{1f}"))
    })?;
    rows.collect()
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// The clauses of a `CREATE` statement that `SQLite`'s introspection does not
/// report, read from its stored text: `CHECK` expressions, `AUTOINCREMENT`,
/// `COLLATE` names, `ON CONFLICT` algorithms, and the `WHERE` predicate of a
/// partial index. Sorted. Read with a tokenizer that skips quoted names, string
/// literals and comments, and works on characters, so no text can make it
/// panic; text it cannot parse (an unbalanced parenthesis) is kept as it is
/// and so differs from the reference.
///
/// Not read: the expression of a generated column, which no table of this
/// schema has.
fn clauses(sql: &str) -> Vec<String> {
    let characters: Vec<char> = sql.chars().collect();
    let mut words: Vec<(String, usize)> = Vec::new();
    let mut position = 0;
    while position < characters.len() {
        let character = characters[position];
        let close = match character {
            '\'' | '"' | '`' => Some(character),
            '[' => Some(']'),
            _ => None,
        };
        if let Some(close) = close {
            position += 1;
            while position < characters.len() && characters[position] != close {
                position += 1;
            }
            position += 1;
        } else if character == '-' && characters.get(position + 1) == Some(&'-') {
            while position < characters.len() && characters[position] != '\n' {
                position += 1;
            }
        } else if character == '/' && characters.get(position + 1) == Some(&'*') {
            position += 2;
            while position < characters.len()
                && !(characters[position] == '*' && characters.get(position + 1) == Some(&'/'))
            {
                position += 1;
            }
            position += 2;
        } else if is_word_character(character) {
            let start = position;
            // At least one character: the branch was entered on one.
            position += 1;
            while position < characters.len() && is_word_character(characters[position]) {
                position += 1;
            }
            words.push((characters[start..position].iter().collect(), start));
        } else {
            position += 1;
        }
    }
    let normalized = |from: usize, to: usize| -> String {
        characters[from..to.min(characters.len())]
            .iter()
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut found = Vec::new();
    for (index, (word, start)) in words.iter().enumerate() {
        let word = word.to_ascii_lowercase();
        let next = words
            .get(index + 1)
            .map(|(next, _)| next.to_ascii_lowercase())
            .unwrap_or_default();
        match word.as_str() {
            "autoincrement" => found.push("autoincrement".to_owned()),
            "collate" => found.push(format!("collate {next}")),
            "conflict" => found.push(format!("conflict {next}")),
            "check" => {
                let open = characters[start + 5..]
                    .iter()
                    .position(|c| !c.is_whitespace())
                    .map(|offset| start + 5 + offset)
                    .filter(|open| characters[*open] == '(');
                found.push(match open {
                    Some(open) => {
                        let mut depth = 0_usize;
                        let mut end = characters.len();
                        for (offset, c) in characters[open..].iter().enumerate() {
                            match c {
                                '(' => depth += 1,
                                ')' => {
                                    depth = depth.saturating_sub(1);
                                    if depth == 0 {
                                        end = open + offset + 1;
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                        format!("check {}", normalized(open, end))
                    }
                    None => "check".to_owned(),
                });
            }
            "where" => found.push(format!(
                "where {}",
                normalized(*start + 5, characters.len())
            )),
            _ => {}
        }
    }
    found.sort();
    found
}

/// The shape of the schema this build creates, computed once: an empty
/// in-memory database upgraded by the ladder.
fn reference_shape() -> Result<&'static BTreeMap<String, Vec<String>>, String> {
    static REFERENCE: OnceLock<Result<BTreeMap<String, Vec<String>>, String>> = OnceLock::new();
    REFERENCE
        .get_or_init(|| {
            let mut reference = Connection::open_in_memory().map_err(|error| error.to_string())?;
            upgrade(&mut reference).map_err(|error| error.to_string())?;
            shape(&reference).map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Checks that the database holds exactly the schema this build creates, and
/// the single feed row the change feed needs. Run it on a database already at
/// [`SCHEMA_VERSION`].
///
/// # Errors
///
/// A description of the first missing, extra, or different object.
pub(crate) fn check_current(connection: &Connection) -> Result<(), String> {
    let expected = reference_shape()?;
    let actual = shape(connection).map_err(|error| error.to_string())?;
    for (object, wanted) in expected {
        match actual.get(object) {
            None => return Err(format!("{object} is missing")),
            Some(found) if found != wanted => {
                let detail = wanted
                    .iter()
                    .zip(found)
                    .find(|(wanted, found)| wanted != found)
                    .map_or_else(
                        || "a different number of parts".to_owned(),
                        |(wanted, found)| format!("expected {wanted:?}, found {found:?}"),
                    );
                return Err(format!(
                    "{object} differs from this build's schema: {detail}"
                ));
            }
            Some(_) => {}
        }
    }
    if let Some(extra) = actual.keys().find(|object| !expected.contains_key(*object)) {
        return Err(format!("unexpected {extra}"));
    }
    let owners: i64 = connection
        .query_row("SELECT count(*) FROM users WHERE id = 'local'", [], |row| {
            row.get(0)
        })
        .map_err(|error| error.to_string())?;
    if owners != 1 {
        return Err("the users table does not hold the local owner".to_owned());
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

/// Expires the lease of every running job, so that the next claim takes it.
/// A restore runs it: a lease in a backup belongs to a worker of the machine
/// the backup was taken on, whose clock and process this database does not
/// know, and which writes to the original database, not to this one. The
/// claim that follows has a higher fencing token.
///
/// # Errors
///
/// [`StoreError::Unavailable`].
pub(crate) fn expire_job_leases(connection: &Connection) -> Result<usize, StoreError> {
    connection
        .execute(
            "UPDATE jobs SET lease_expires_at = 0 WHERE state = 'running'",
            [],
        )
        .map_err(unavailable)
}

/// How many rows the `changes` table holds.
fn change_rows(transaction: &Transaction<'_>) -> Result<i64, StoreError> {
    transaction
        .query_row("SELECT count(*) FROM changes", [], |row| row.get(0))
        .map_err(unavailable)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clauses_are_read_from_tokens_not_from_substrings() {
        let sql = "CREATE TABLE t (
            checksum TEXT, check_at TEXT, _hidden TEXT, __x__ TEXT, -- it's a 'check (x)' comment
            n INTEGER CHECK (n >= 1 AND n < (2 + 3)),
            \"check\" TEXT COLLATE nocase,
            id INTEGER PRIMARY KEY AUTOINCREMENT
        ) STRICT";
        assert_eq!(
            clauses(sql),
            [
                "autoincrement",
                "check (n >= 1 AND n < (2 + 3))",
                "collate nocase"
            ]
        );
    }

    /// Identifiers made of or starting with underscores, as in `request_keys`
    /// and `_x`, end a word instead of looping.
    #[test]
    fn identifiers_with_underscores_are_whole_words() {
        let sql = "CREATE TABLE request_keys (_a TEXT, b__ TEXT, ___ TEXT CHECK (_a <> ''))";
        assert_eq!(clauses(sql), ["check (_a <> '')"]);
    }

    #[test]
    fn text_that_is_not_ascii_or_not_balanced_never_panics() {
        for sql in [
            "CREATE TABLE ü (名前 TEXT CHECK (名前 <> 'é'))",
            "CREATE TABLE t (x CHECK",
            "CREATE TABLE t (x CHECK (a > 1",
            "CREATE TABLE t (x TEXT) /* open comment",
            "é'",
            "",
        ] {
            let found = clauses(sql);
            assert!(found.len() <= 1, "{sql:?}: {found:?}");
        }
        assert_eq!(clauses("x CHECK (a > 1"), ["check (a > 1"]);
        assert_ne!(clauses("x CHECK (a > 1"), clauses("x CHECK (a > 2"));
    }
}
