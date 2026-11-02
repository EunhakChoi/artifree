//! Fixtures shared by the schema upgrade and backup tests.
#![allow(
    dead_code,
    reason = "Each test target uses a different part of the shared helpers."
)]

pub mod history;

use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, CommitReceipt, MetadataReceipt, MetadataUpdate, NewArtifact, RequestFingerprint,
    RequestIdentity, RevisionCommit, StoreError,
};
use artifree_providers::storage::{DATABASE_FILE, SCHEMA_VERSION, SqliteStore};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A unique directory, removed when dropped.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "artifree-o04-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|error| panic!("cannot create {}: {error}", path.display()));
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// A path inside the directory that does not exist yet.
    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn parsed<T: FromStr>(value: &str) -> T
where
    T::Err: Display,
{
    value
        .parse()
        .unwrap_or_else(|error| panic!("invalid fixture {value:?}: {error}"))
}

pub fn actor(name: &str) -> ActorId {
    parsed(name)
}

pub fn key(name: &str) -> RequestKey {
    parsed(&format!("test-key-{name}"))
}

pub fn package(files: &[(&str, &str)]) -> FilePackage {
    FilePackage::new(
        files
            .iter()
            .map(|(path, content)| (parsed::<PackagePath>(path), content.as_bytes().to_vec()))
            .collect(),
    )
    .unwrap_or_else(|error| panic!("invalid fixture package: {error}"))
}

pub fn digest(body: &str) -> [u8; 32] {
    Sha256::digest(body).into()
}

pub fn create_demo(
    store: &SqliteStore,
    owner: &ActorId,
    request_key: &RequestKey,
    title: &str,
    body: &str,
) -> Result<CommitReceipt, StoreError> {
    store.create_artifact(NewArtifact {
        request: RequestIdentity {
            actor: owner,
            key: request_key,
            fingerprint: RequestFingerprint(digest(body)),
        },
        kind: ArtifactKind::App,
        title: &parsed::<Title>(title),
        package: &package(&[("index.html", body), ("style.css", "h1{}")]),
        source: None,
    })
}

pub fn revise(
    store: &SqliteStore,
    created: &CommitReceipt,
    request_key: &RequestKey,
    base: u64,
    body: &str,
) -> Result<CommitReceipt, StoreError> {
    store.commit_revision(RevisionCommit {
        request: RequestIdentity {
            actor: &created.artifact.owner,
            key: request_key,
            fingerprint: RequestFingerprint(digest(body)),
        },
        artifact: &created.artifact.id,
        base: SourceRevision(base),
        package: &package(&[("index.html", body), ("style.css", "h1{}")]),
    })
}

pub fn update_metadata(
    store: &SqliteStore,
    created: &CommitReceipt,
    request_key: &RequestKey,
    base: u64,
    title: Option<&str>,
    pinned: Option<bool>,
    deleted: Option<bool>,
) -> Result<MetadataReceipt, StoreError> {
    store.update_metadata(MetadataUpdate {
        request: RequestIdentity {
            actor: &created.artifact.owner,
            key: request_key,
            fingerprint: RequestFingerprint(digest(&format!("{title:?}{pinned:?}{deleted:?}"))),
        },
        artifact: &created.artifact.id,
        base: MetadataRevision(base),
        title: title.map(parsed::<Title>).as_ref(),
        pinned,
        deleted,
    })
}

/// Every file directly in `directory` with its bytes, to compare before and
/// after an operation that must change nothing. Directories are listed by
/// name with no bytes. A listing, inspection, or read that fails panics with
/// the path: an unreadable file must not compare equal as empty.
pub fn files_of(directory: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot list {}: {error}", directory.display()))
        .map(|entry| {
            let entry = entry
                .unwrap_or_else(|error| panic!("cannot list {}: {error}", directory.display()));
            let path = entry.path();
            let file_type = entry
                .file_type()
                .unwrap_or_else(|error| panic!("cannot inspect {}: {error}", path.display()));
            let bytes = file_type.is_file().then(|| {
                std::fs::read(&path)
                    .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
            });
            (entry.file_name().to_string_lossy().into_owned(), bytes)
        })
        .collect()
}

pub fn user_version(path: &Path) -> Result<i64, rusqlite::Error> {
    Connection::open(path)?.pragma_query_value(None, "user_version", |row| row.get(0))
}

/// The schema objects of a database as (type, name, whitespace-normalized
/// SQL), independent of the production comparison.
pub fn schema_of(path: &Path) -> Result<Vec<(String, String, String)>, rusqlite::Error> {
    let connection = Connection::open(path)?;
    let mut statement = connection.prepare(
        "SELECT type, name, coalesce(sql, '') FROM sqlite_master
         WHERE name NOT LIKE 'sqlite!_%' ESCAPE '!' ORDER BY type, name",
    )?;
    let rows = statement.query_map([], |row| {
        let sql: String = row.get(2)?;
        Ok((
            row.get(0)?,
            row.get(1)?,
            sql.split_whitespace().collect::<Vec<_>>().join(" "),
        ))
    })?;
    rows.collect()
}

/// A database created fresh by this build, at the current version.
pub fn current_database(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    SqliteStore::open(path)?;
    assert_eq!(user_version(path)?, SCHEMA_VERSION);
    Ok(())
}

/// A rebuilt `changes` table starts its sequence at its largest row; the
/// fixture puts the head back where the live feed was before the rebuild.
const HEAD_AT_5: &str = "UPDATE sqlite_sequence SET seq = 5 WHERE name = 'changes';";

pub const OLD_ID: &str = "art_0123456789abcdef0123456789abcdef";
pub const TRASHED_ID: &str = "art_fedcba9876543210fedcba9876543210";
pub const OLD_BODY: &str = "<h1>old</h1>";
/// Fingerprint of the request key "test-key-old" in every fixture.
pub const OLD_FINGERPRINT: [u8; 32] = [7; 32];

pub fn old_artifact() -> ArtifactId {
    parsed(OLD_ID)
}

/// A database as the build of schema `version` (1 to 5) left it, holding real
/// data, built only from the frozen schema texts in [`history`]:
///
/// - the artifact "Old" at revision 2 with the body [`OLD_BODY`], created by
///   the request key "test-key-old";
/// - from version 2, a feed with its head at 5;
/// - from version 3, "Old" renamed to "Renamed" and pinned (metadata revision 2);
/// - at version 5, an artifact "Trashed" in the trash and one access token.
pub fn historical_database(path: &Path, version: i64) -> Result<(), rusqlite::Error> {
    let connection = Connection::open(path)?;
    connection.execute_batch(history::STEP_1)?;
    connection.execute_batch(&format!(
        "INSERT INTO artifacts VALUES ('{OLD_ID}', 'owner', 'app', 'Old', 2, 100, 200);
         INSERT INTO revisions VALUES ('{OLD_ID}', 1, 'owner', 100), ('{OLD_ID}', 2, 'owner', 200);
         INSERT INTO blobs VALUES (zeroblob(32), CAST('{OLD_BODY}' AS BLOB));
         INSERT INTO revision_files VALUES
             ('{OLD_ID}', 1, 'index.html', 12, zeroblob(32)),
             ('{OLD_ID}', 2, 'index.html', 12, zeroblob(32));
         INSERT INTO request_keys VALUES
             ('owner', 'test-key-old', zeroblob(32), '{OLD_ID}', 1, 100);
         UPDATE request_keys SET fingerprint = x'{fingerprint}';",
        fingerprint = "07".repeat(32)
    ))?;
    if version >= 2 {
        connection.execute_batch(history::STEP_2)?;
        connection.execute_batch(&format!(
            "INSERT INTO feed VALUES (1, x'00112233445566778899aabbccddeeff');
             INSERT INTO changes (sequence, artifact_id, revision, kind) VALUES
                 (1, '{OLD_ID}', 1, 'created'), (2, '{OLD_ID}', 2, 'revised');
             UPDATE sqlite_sequence SET seq = 5 WHERE name = 'changes';"
        ))?;
    }
    if version >= 3 {
        connection.execute_batch(history::STEP_3)?;
        connection.execute_batch(HEAD_AT_5)?;
        connection.execute_batch(&format!(
            "INSERT INTO metadata_revisions
                 (artifact_id, metadata_revision, title, pinned, created_by, created_at)
                 VALUES ('{OLD_ID}', 2, 'Renamed', 1, 'owner', 300);
             UPDATE artifacts SET title = 'Renamed', pinned = 1, metadata_revision = 2
                 WHERE id = '{OLD_ID}';"
        ))?;
    }
    if version >= 4 {
        connection.execute_batch(history::STEP_4)?;
    }
    if version >= 5 {
        connection.execute_batch(history::STEP_5)?;
        connection.execute_batch(HEAD_AT_5)?;
        connection.execute_batch(&format!(
            "INSERT INTO artifacts (id, owner, kind, title, current_revision, created_at,
                 updated_at, pinned, metadata_revision, deleted_at)
                 VALUES ('{TRASHED_ID}', 'owner', 'app', 'Trashed', 1, 150, 150, 0, 2, 400);
             INSERT INTO revisions VALUES ('{TRASHED_ID}', 1, 'owner', 150);
             INSERT INTO revision_files VALUES ('{TRASHED_ID}', 1, 'index.html', 12, zeroblob(32));
             INSERT INTO metadata_revisions
                 (artifact_id, metadata_revision, title, pinned, created_by, created_at, deleted_at)
                 VALUES ('{TRASHED_ID}', 1, 'Trashed', 0, 'owner', 150, NULL),
                        ('{TRASHED_ID}', 2, 'Trashed', 0, 'owner', 400, 400);"
        ))?;
        connection.execute_batch(history::OLD_TOKENS)?;
        connection.execute_batch(
            "INSERT INTO tokens (id, name, secret_hash, prefix, created_at)
                 VALUES ('tok_0123456789abcdef0123456789abcdef', 'laptop', 'hash-1', 'art_01', '2026-01-01T00:00:00Z');",
        )?;
    }
    connection.pragma_update(None, "user_version", version)
}

/// Adds `fixture` data checks used after any path to the current schema.
pub fn expected_artifact_name(version: i64) -> &'static str {
    if version >= 3 { "Renamed" } else { "Old" }
}

pub fn database_in(directory: &Path) -> PathBuf {
    directory.join(DATABASE_FILE)
}
