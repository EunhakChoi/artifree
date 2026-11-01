//! Databases of every supported older schema, upgraded by the current build,
//! and databases the build must leave untouched.
//!
//! The schemas below are written out as released, not derived from the
//! current code, so a change to a migration cannot change its own fixture.

mod common;

use std::path::Path;

use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey};
use artifree_domain::package::PackagePath;
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, ChangeKind, Delivery, NewArtifact, RequestFingerprint, RequestIdentity,
    StoreError,
};
use artifree_ports::token_store::{NewTokenRecord, TokenStore};
use artifree_providers::backup;
use artifree_providers::storage::{SCHEMA_VERSION, SqliteStore};
use artifree_providers::tokens::{SqliteTokenStore, hash_token_secret};
use rusqlite::{Connection, params};

use common::{TempDir, TestResult, package, parsed, sha256};

const OLD_ID: &str = "art_0123456789abcdef0123456789abcdef";
const INDEX_V1: &str = "<h1>old</h1>";
const INDEX_V2: &str = "<h1>older, then newer</h1>";

const VERSION_1: &str = "
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

const VERSION_2: &str = "
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

/// Version 3 as released: the version 2 tables carry the metadata revision.
const VERSION_3: &str = "
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
DROP TABLE request_keys;
CREATE TABLE request_keys (
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
DROP TABLE changes;
CREATE TABLE changes (
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
";

const VERSION_4: &str = "ALTER TABLE feed ADD COLUMN committed_at INTEGER NOT NULL DEFAULT 0;";

/// A database of `version` holding one artifact, "Old", at revision 2: the
/// files of revision 1 and 2, the request key "test-key-old" for revision 1,
/// and (from version 2) a feed whose head is 5.
fn fixture(path: &Path, version: i64) -> Result<(), rusqlite::Error> {
    let connection = Connection::open(path)?;
    connection.execute_batch(VERSION_1)?;
    if version >= 2 {
        connection.execute_batch(VERSION_2)?;
    }
    if version >= 3 {
        connection.execute_batch(VERSION_3)?;
    }
    if version >= 4 {
        connection.execute_batch(VERSION_4)?;
    }
    connection.execute(
        "INSERT INTO artifacts (id, owner, kind, title, current_revision, created_at, updated_at)
         VALUES (?1, 'owner', 'app', 'Old', 2, 100, 200)",
        [OLD_ID],
    )?;
    connection.execute(
        "INSERT INTO revisions VALUES (?1, 1, 'owner', 100), (?1, 2, 'owner', 200)",
        [OLD_ID],
    )?;
    for (revision, content) in [(1, INDEX_V1), (2, INDEX_V2)] {
        connection.execute(
            "INSERT OR IGNORE INTO blobs VALUES (?1, ?2)",
            params![sha256(content).as_slice(), content.as_bytes()],
        )?;
        connection.execute(
            "INSERT INTO revision_files VALUES (?1, ?2, 'index.html', ?3, ?4)",
            params![
                OLD_ID,
                revision,
                i64::try_from(content.len()).unwrap_or(0),
                sha256(content).as_slice()
            ],
        )?;
    }
    if version >= 3 {
        connection.execute(
            "UPDATE artifacts SET pinned = 1, metadata_revision = 2 WHERE id = ?1",
            [OLD_ID],
        )?;
        connection.execute(
            "INSERT INTO metadata_revisions VALUES (?1, 1, 'Old', 0, 'owner', 100),
                                                   (?1, 2, 'Old', 1, 'owner', 250)",
            [OLD_ID],
        )?;
        connection.execute(
            "INSERT INTO request_keys VALUES ('owner', 'test-key-old', ?1, ?2, 1, 1, 100)",
            params![[7_u8; 32].as_slice(), OLD_ID],
        )?;
    } else {
        connection.execute(
            "INSERT INTO request_keys VALUES ('owner', 'test-key-old', ?1, ?2, 1, 100)",
            params![[7_u8; 32].as_slice(), OLD_ID],
        )?;
    }
    if version == 2 {
        connection.execute(
            "INSERT INTO feed (singleton, id) VALUES (1, zeroblob(16))",
            [],
        )?;
        connection.execute(
            "INSERT INTO changes (sequence, artifact_id, revision, kind) VALUES
                 (1, ?1, 1, 'created'), (2, ?1, 2, 'revised')",
            [OLD_ID],
        )?;
        connection.execute(
            "UPDATE sqlite_sequence SET seq = 5 WHERE name = 'changes'",
            [],
        )?;
    }
    if version >= 3 {
        connection.execute(
            "INSERT INTO feed (singleton, id) VALUES (1, zeroblob(16))",
            [],
        )?;
        connection.execute(
            "INSERT INTO changes VALUES (1, ?1, 'created', 1, 1, 100),
                                        (2, ?1, 'revised', 2, 1, 200),
                                        (3, ?1, 'metadata_updated', 2, 2, 250)",
            [OLD_ID],
        )?;
        connection.execute(
            "UPDATE sqlite_sequence SET seq = 5 WHERE name = 'changes'",
            [],
        )?;
    }
    if version >= 4 {
        connection.execute("UPDATE feed SET committed_at = 300", [])?;
    }
    connection.pragma_update(None, "user_version", version)
}

fn user_version(path: &Path) -> Result<i64, rusqlite::Error> {
    Connection::open(path)?.pragma_query_value(None, "user_version", |row| row.get(0))
}

/// The schema as `SQLite` stores it, without whitespace and quoting, which an
/// `ALTER` or rename changes without changing the table.
type Schema = Vec<(String, String, String, String)>;

fn schema_of(path: &Path) -> Result<Schema, rusqlite::Error> {
    let connection = Connection::open(path)?;
    let mut statement = connection.prepare(
        "SELECT type, name, tbl_name, coalesce(sql, '') FROM sqlite_master ORDER BY type, name",
    )?;
    let rows = statement.query_map([], |row| {
        let sql: String = row.get(3)?;
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            sql.chars()
                .filter(|c| !c.is_whitespace() && *c != '"')
                .collect(),
        ))
    })?;
    rows.collect()
}

fn fresh_schema() -> Result<Schema, Box<dyn std::error::Error>> {
    let dir = TempDir::new("fresh");
    let path = dir.join("artifree.sqlite3");
    SqliteStore::open(&path)?;
    Ok(schema_of(&path)?)
}

fn owner() -> ActorId {
    parsed("owner")
}

fn old_id() -> ArtifactId {
    parsed(OLD_ID)
}

fn healthy(path: &Path) -> TestResult {
    let connection = Connection::open(path)?;
    let verdict: String = connection.pragma_query_value(None, "integrity_check", |r| r.get(0))?;
    assert_eq!(verdict, "ok");
    let violations: i64 =
        connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })?;
    assert_eq!(violations, 0, "foreign key violations");
    Ok(())
}

#[test]
fn every_older_schema_upgrades_to_the_fresh_schema_and_keeps_its_data() -> TestResult {
    let fresh = fresh_schema()?;
    for version in 1..SCHEMA_VERSION {
        let dir = TempDir::new("upgrade");
        let path = dir.join("artifree.sqlite3");
        fixture(&path, version)?;
        let store = SqliteStore::open(&path)?;

        assert_eq!(user_version(&path)?, SCHEMA_VERSION, "from {version}");
        assert_eq!(schema_of(&path)?, fresh, "from {version}: schema");
        healthy(&path)?;

        let record = store.artifact(&old_id())?.ok_or("artifact kept")?;
        assert_eq!(record.title.as_str(), "Old", "from {version}");
        assert_eq!(record.current_revision, SourceRevision(2), "from {version}");
        assert_eq!(record.pinned, version >= 3, "from {version}: pin");
        assert_eq!(
            record.metadata_revision,
            MetadataRevision(if version >= 3 { 2 } else { 1 }),
            "from {version}"
        );
        assert_eq!(record.deleted_at, None, "from {version}: trash");

        // File bytes and digests of every revision.
        for (revision, content) in [(1, INDEX_V1), (2, INDEX_V2)] {
            let bytes = store
                .revision_file(&old_id(), SourceRevision(revision), &PackagePath::entry())?
                .ok_or("file kept")?;
            assert_eq!(bytes, content.as_bytes(), "from {version}, rev {revision}");
            let listed = store
                .revision(&old_id(), SourceRevision(revision))?
                .ok_or("revision kept")?;
            assert_eq!(listed.files[0].sha256, sha256(content), "from {version}");
        }

        // The old request key still replays, and conflicts on other content.
        let owner = owner();
        let request = |fingerprint: u8| -> Result<_, StoreError> {
            let key: RequestKey = parsed("test-key-old");
            store.create_artifact(NewArtifact {
                request: RequestIdentity {
                    actor: &owner,
                    key: &key,
                    fingerprint: RequestFingerprint([fingerprint; 32]),
                },
                kind: ArtifactKind::App,
                title: &parsed("Old"),
                package: &package(&[("index.html", INDEX_V1)]),
                source: None,
            })
        };
        let replay = request(7)?;
        assert_eq!(replay.delivery, Delivery::Replayed, "from {version}");
        assert_eq!(
            replay.revision.revision,
            SourceRevision(1),
            "from {version}"
        );
        assert_eq!(
            request(8).err(),
            Some(StoreError::RequestKeyReused),
            "from {version}"
        );

        // The feed keeps its head, and new commits continue after it.
        let page = store.changes(&owner, ChangeSequence(0), 100)?;
        let kinds: Vec<_> = page
            .changes
            .iter()
            .map(|c| (c.sequence.0, c.kind))
            .collect();
        let expected = match version {
            1 => vec![],
            2 => vec![(1, ChangeKind::Created), (2, ChangeKind::Revised)],
            _ => vec![
                (1, ChangeKind::Created),
                (2, ChangeKind::Revised),
                (3, ChangeKind::MetadataUpdated),
            ],
        };
        assert_eq!(kinds, expected, "from {version}: feed");
        assert_eq!(
            page.head.0,
            if version >= 2 { 5 } else { 0 },
            "from {version}"
        );
    }
    Ok(())
}

#[test]
fn an_empty_or_new_file_becomes_the_fresh_schema() -> TestResult {
    let fresh = fresh_schema()?;
    let dir = TempDir::new("empty");
    let missing = dir.join("missing.sqlite3");
    let zero_bytes = dir.join("zero.sqlite3");
    std::fs::write(&zero_bytes, b"")?;
    let version_zero = dir.join("zero-version.sqlite3");
    Connection::open(&version_zero)?
        .execute_batch("CREATE TABLE unrelated (x); DROP TABLE unrelated;")?;
    for path in [&missing, &zero_bytes, &version_zero] {
        SqliteStore::open(path)?;
        assert_eq!(user_version(path)?, SCHEMA_VERSION, "{path:?}");
        assert_eq!(schema_of(path)?, fresh, "{path:?}");
    }
    Ok(())
}

#[test]
fn a_failed_migration_leaves_the_file_as_it_was() -> TestResult {
    for version in 1..SCHEMA_VERSION {
        let dir = TempDir::new("failed");
        let path = dir.join("artifree.sqlite3");
        fixture(&path, version)?;
        // The last step creates `changes_v5`; an object of that name makes it
        // fail after the earlier steps have run.
        Connection::open(&path)?.execute_batch("CREATE TABLE changes_v5 (in_the_way TEXT);")?;
        let before = std::fs::read(&path)?;
        let files_before = names(&dir)?;

        let error = SqliteStore::open(&path).err();
        assert!(
            matches!(&error, Some(StoreError::Unavailable(reason)) if reason.contains("changes_v5")),
            "from {version}: expected the conflicting object in the error, got {error:?}"
        );
        assert_eq!(std::fs::read(&path)?, before, "from {version}: file bytes");
        assert_eq!(names(&dir)?, files_before, "from {version}: no stray files");
        assert_eq!(user_version(&path)?, version, "from {version}");

        // Without the obstacle the same file upgrades with all its data.
        Connection::open(&path)?.execute_batch("DROP TABLE changes_v5;")?;
        let store = SqliteStore::open(&path)?;
        let record = store.artifact(&old_id())?.ok_or("artifact kept")?;
        assert_eq!(record.current_revision, SourceRevision(2), "from {version}");
    }
    Ok(())
}

fn names(dir: &TempDir) -> Result<Vec<String>, std::io::Error> {
    let mut names: Vec<String> = std::fs::read_dir(dir.path())?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, std::io::Error>>()?;
    names.sort();
    Ok(names)
}

#[test]
fn a_newer_schema_is_refused_without_touching_the_file() -> TestResult {
    // One file still in rollback-journal mode and one in WAL mode, as the
    // server leaves its database.
    let dir = TempDir::new("newer");
    let rollback = dir.join("rollback.sqlite3");
    fixture(&rollback, 4)?;
    Connection::open(&rollback)?.pragma_update(None, "user_version", SCHEMA_VERSION + 1)?;
    let wal = dir.join("wal.sqlite3");
    SqliteStore::open(&wal)?;
    Connection::open(&wal)?.pragma_update(None, "user_version", SCHEMA_VERSION + 1)?;
    for path in [&rollback, &wal] {
        let before = std::fs::read(path)?;
        let files_before = names(&dir)?;
        let error = SqliteStore::open(path).err();
        assert!(
            matches!(&error, Some(StoreError::Unavailable(reason)) if reason.contains("not supported")),
            "{path:?}: expected a refusal, got {error:?}"
        );
        assert_eq!(std::fs::read(path)?, before, "{path:?}: file bytes");
        assert_eq!(names(&dir)?, files_before, "{path:?}: no stray files");
    }
    Ok(())
}

#[test]
fn token_records_survive_before_and_after_the_store_upgrade() -> TestResult {
    let dir = TempDir::new("tokens");
    // A database that only ever held tokens (opened by the token store
    // first), and a version 5 database from before token records existed.
    let tokens_first = dir.join("tokens-first.sqlite3");
    let record = NewTokenRecord {
        id: artifree_domain::identity::TokenId::from_random([3; 16]),
        name: "laptop".to_owned(),
        secret_hash: hash_token_secret("secret"),
        prefix: "afr_ab…".to_owned(),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    SqliteTokenStore::open(&tokens_first)?.create(&record)?;
    SqliteStore::open(&tokens_first)?;
    assert_eq!(user_version(&tokens_first)?, SCHEMA_VERSION);
    let found = SqliteTokenStore::open(&tokens_first)?
        .find_by_hash(&record.secret_hash)?
        .ok_or("token kept")?;
    assert_eq!(found.name, "laptop");

    let no_tokens = dir.join("no-tokens.sqlite3");
    SqliteStore::open(&no_tokens)?;
    let tokens = SqliteTokenStore::open(&no_tokens)?;
    assert!(tokens.list()?.is_empty());
    tokens.create(&record)?;
    assert_eq!(tokens.list()?.len(), 1);
    Ok(())
}

#[test]
fn a_backup_of_an_older_schema_is_restored_upgraded() -> TestResult {
    let fresh = fresh_schema()?;
    let dir = TempDir::new("restore-old");
    let source = dir.join("old.sqlite3");
    fixture(&source, 3)?;
    let snapshot = dir.join("snapshot.sqlite3");
    let backed_up = backup::create_backup(&source, &snapshot)?;
    assert_eq!(backed_up.schema_version, 3, "a backup is not upgraded");
    let restored = dir.join("restored.sqlite3");
    let summary = backup::restore_backup(&snapshot, &restored)?;
    assert_eq!(summary.schema_version, SCHEMA_VERSION);
    // The restore also readies the token table, which a fresh store does not.
    let without_tokens: Vec<_> = schema_of(&restored)?
        .into_iter()
        .filter(|(_, name, table, _)| !name.contains("tokens") && !table.contains("tokens"))
        .collect();
    assert_eq!(without_tokens, fresh);
    let store = SqliteStore::open(&restored)?;
    let bytes = store
        .revision_file(&old_id(), SourceRevision(2), &PackagePath::entry())?
        .ok_or("file kept")?;
    assert_eq!(bytes, INDEX_V2.as_bytes());
    Ok(())
}
