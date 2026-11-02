//! Upgrading released schemas, and opens that must change nothing.

mod common;

use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::UnixMillis;
use artifree_domain::job::JobKind;
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactStore, Delivery, ListOrder, NewArtifact, PurgeRequest,
    RequestFingerprint, RequestIdentity, StoreError,
};
use artifree_ports::job_store::{JobStore, JobStoreError, NewJob};
use artifree_ports::token_store::TokenStore;
use artifree_providers::storage::{SCHEMA_VERSION, SqliteStore};
use artifree_providers::tokens::SqliteTokenStore;
use common::{
    OLD_BODY, OLD_FINGERPRINT, TRASHED_ID, TempDir, TestResult, actor, current_database,
    database_in, expected_artifact_name, files_of, historical_database, history, key, old_artifact,
    parsed, schema_of, user_version,
};
use rusqlite::Connection;

/// Every released version, with data in every table it has, upgrades to the
/// schema a fresh database has, and keeps what it held.
#[test]
fn every_released_version_upgrades_to_the_fresh_schema_and_keeps_its_data() -> TestResult {
    let fresh = TempDir::new();
    current_database(&database_in(fresh.path()))?;
    let fresh_schema = schema_of(&database_in(fresh.path()))?;
    for version in 1..=7 {
        let dir = TempDir::new();
        let path = database_in(dir.path());
        historical_database(&path, version)?;
        assert_eq!(user_version(&path)?, version);

        let store = SqliteStore::open(&path)?;

        assert_eq!(user_version(&path)?, SCHEMA_VERSION, "version {version}");
        assert_eq!(schema_of(&path)?, fresh_schema, "version {version}");
        let owner = actor("owner");
        let old = store.artifact(&old_artifact())?.ok_or("artifact kept")?;
        assert_eq!(old.title.as_str(), expected_artifact_name(version));
        assert_eq!(old.pinned, version >= 3, "version {version}");
        assert_eq!(
            old.metadata_revision,
            MetadataRevision(if version >= 3 { 2 } else { 1 })
        );
        assert_eq!(old.current_revision, SourceRevision(2));
        for revision in [1, 2] {
            let file = store.revision_file(
                &old_artifact(),
                SourceRevision(revision),
                &parsed("index.html"),
            )?;
            assert_eq!(file.as_deref(), Some(OLD_BODY.as_bytes()), "v{version}");
        }
        // The original request key still replays its original result.
        assert!(
            matches!(
                common::create_demo(&store, &owner, &key("old"), "Old", "<h1>other</h1>"),
                Err(StoreError::RequestKeyReused)
            ),
            "version {version}: the old key must still reject other content"
        );
        let reused = store.create_artifact(NewArtifact {
            request: RequestIdentity {
                actor: &owner,
                key: &parsed("test-key-old"),
                fingerprint: RequestFingerprint(OLD_FINGERPRINT),
            },
            kind: ArtifactKind::App,
            title: &parsed("Old"),
            package: &common::package(&[("index.html", OLD_BODY)]),
            source: None,
        })?;
        assert_eq!(reused.delivery, Delivery::Replayed, "version {version}");
        assert_eq!(reused.artifact.id, old_artifact());

        assert_jobs_work_beside_old_keys(&store, version)?;

        // The feed head never moves back through an upgrade.
        let page = store.changes(&owner, ChangeSequence(0), 100)?;
        assert_eq!(page.head, ChangeSequence(if version >= 2 { 5 } else { 0 }));

        let trash = store.list_artifacts(ArtifactListing {
            owner: &owner,
            search: None,
            pinned: None,
            deleted: true,
            order: ListOrder::Updated,
            after: None,
            limit: 10,
        })?;
        let trashed: Vec<_> = trash.iter().map(|item| item.title.as_str()).collect();
        assert_eq!(
            trashed,
            if version >= 5 {
                vec!["Trashed"]
            } else {
                vec![]
            }
        );
        let found = store.unreferenced_content()?;
        assert_eq!(
            (found.blobs, found.bytes),
            (0, 0),
            "version {version}: an upgrade must not leave content unreferenced"
        );
        if version >= 5 {
            purge_the_trashed_artifact(&store, version)?;
        }
        if version >= 5 {
            let tokens = SqliteTokenStore::open(&path)?.list()?;
            assert_eq!(tokens.len(), 1, "the token of the version 5 file is kept");
            assert_eq!(tokens[0].name, "laptop");
        } else {
            assert!(SqliteTokenStore::open(&path)?.list()?.is_empty());
        }
    }
    Ok(())
}

/// The tables of persistent jobs exist and work on the old artifact, and their
/// keys share one space with the keys the file already held.
fn assert_jobs_work_beside_old_keys(store: &SqliteStore, version: i64) -> TestResult {
    let owner = actor("owner");
    let job = |name: &str, revision: u64| {
        let request_key = parsed(&format!("test-key-{name}"));
        store.enqueue_job(NewJob {
            request: RequestIdentity {
                actor: &owner,
                key: &request_key,
                fingerprint: RequestFingerprint([3; 32]),
            },
            artifact: &old_artifact(),
            revision: SourceRevision(revision),
            kind: JobKind::VerifyRevision,
            now: UnixMillis(1_000),
        })
    };
    assert_eq!(job("first-job", 1)?.delivery, Delivery::Committed);
    assert_eq!(
        job("old", 1).err(),
        Some(JobStoreError::RequestKeyReused),
        "version {version}: a key an artifact request used"
    );
    if version >= 7 {
        assert_eq!(
            job("swept", 1).err(),
            Some(JobStoreError::RequestKeyReused),
            "version {version}: a key a cleanup used"
        );
    }
    Ok(())
}

/// The trashed artifact of a version 5 or 6 file shares its content with
/// "Old"; removing it for good keeps what "Old" refers to.
fn purge_the_trashed_artifact(store: &SqliteStore, version: i64) -> TestResult {
    let owner = actor("owner");
    let trashed_id = parsed(TRASHED_ID);
    let receipt = store.purge_artifact(PurgeRequest {
        request: RequestIdentity {
            actor: &owner,
            key: &key("purge"),
            fingerprint: RequestFingerprint([9; 32]),
        },
        artifact: &trashed_id,
        base: MetadataRevision(2),
    })?;
    assert_eq!(receipt.delivery, Delivery::Committed, "version {version}");
    assert_eq!(store.artifact(&trashed_id)?, None);
    let shared = store.revision_file(&old_artifact(), SourceRevision(2), &parsed("index.html"))?;
    assert_eq!(
        shared.as_deref(),
        Some(OLD_BODY.as_bytes()),
        "version {version}"
    );
    let page = store.changes(&owner, ChangeSequence(0), 100)?;
    assert_eq!(page.head, ChangeSequence(6), "the head continues from 5");
    Ok(())
}

/// Opening a file that must be refused, in rollback-journal and in WAL mode,
/// changes no byte and leaves no sidecar file.
fn assert_refused_untouched(
    prepare: impl Fn(&Connection) -> rusqlite::Result<()>,
    mention: &str,
) -> TestResult {
    for wal in [false, true] {
        let dir = TempDir::new();
        let path = database_in(dir.path());
        {
            let connection = Connection::open(&path)?;
            prepare(&connection)?;
            if wal {
                connection.pragma_update(None, "journal_mode", "WAL")?;
            }
        }
        let before = files_of(dir.path());
        let error = SqliteStore::open(&path)
            .err()
            .ok_or_else(|| format!("{mention} (wal {wal}) was accepted"))?;
        assert!(
            error.to_string().contains(mention),
            "wal {wal}: {error}, wanted {mention}"
        );
        assert_eq!(
            files_of(dir.path()),
            before,
            "wal {wal}: the open changed files"
        );
    }
    Ok(())
}

#[test]
fn a_newer_schema_is_refused_without_touching_the_file() -> TestResult {
    assert_refused_untouched(
        |connection| {
            connection.execute_batch(history::STEP_1)?;
            connection.pragma_update(None, "user_version", 99)
        },
        "version 99",
    )?;
    Ok(())
}

#[test]
fn a_foreign_database_without_a_version_is_refused_without_touching_the_file() -> TestResult {
    assert_refused_untouched(
        |connection| {
            connection
                .execute_batch("CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('x');")
        },
        "without an Artifree schema version",
    )?;
    Ok(())
}

#[test]
fn a_failed_migration_step_is_rolled_back_without_touching_the_file() -> TestResult {
    // Version 2 -> 3 fails in the middle of its rebuild, after earlier
    // statements of the same step ran.
    assert_refused_untouched(
        |connection| {
            connection.execute_batch(history::STEP_1)?;
            connection.execute_batch(history::STEP_2)?;
            connection.execute_batch(
                "INSERT INTO feed VALUES (1, zeroblob(16));
                 CREATE TABLE request_keys_v3 (unrelated TEXT);",
            )?;
            connection.pragma_update(None, "user_version", 2)
        },
        "request_keys_v3",
    )?;
    // The last step fails: a table already uses the name of the token index.
    assert_refused_untouched(
        |connection| {
            connection.execute_batch(history::STEP_1)?;
            connection.execute_batch(history::STEP_2)?;
            connection.execute_batch("INSERT INTO feed VALUES (1, zeroblob(16));")?;
            connection.execute_batch(history::STEP_3)?;
            connection.execute_batch(history::STEP_4)?;
            connection.execute_batch(history::STEP_5)?;
            connection.execute_batch("CREATE TABLE idx_tokens_secret_hash (x TEXT);")?;
            connection.pragma_update(None, "user_version", 5)
        },
        "idx_tokens_secret_hash",
    )?;
    // The purge step (version 6 to 7) fails after it added its column.
    assert_refused_untouched(
        |connection| {
            connection.execute_batch(history::STEP_1)?;
            connection.execute_batch(history::STEP_2)?;
            connection.execute_batch("INSERT INTO feed VALUES (1, zeroblob(16));")?;
            connection.execute_batch(history::STEP_3)?;
            connection.execute_batch(history::STEP_4)?;
            connection.execute_batch(history::STEP_5)?;
            connection.execute_batch(history::OLD_TOKENS)?;
            connection.execute_batch("CREATE TABLE changes_v7 (x TEXT);")?;
            connection.pragma_update(None, "user_version", 6)
        },
        "changes_v7",
    )?;
    // The jobs step (version 7 to 8) fails: a table already uses the name.
    assert_refused_untouched(
        |connection| {
            connection.execute_batch(history::STEP_1)?;
            connection.execute_batch(history::STEP_2)?;
            connection.execute_batch("INSERT INTO feed VALUES (1, zeroblob(16));")?;
            connection.execute_batch(history::STEP_3)?;
            connection.execute_batch(history::STEP_4)?;
            connection.execute_batch(history::STEP_5)?;
            connection.execute_batch(history::OLD_TOKENS)?;
            connection.execute_batch(history::STEP_7)?;
            connection.execute_batch("CREATE TABLE jobs (x TEXT);")?;
            connection.pragma_update(None, "user_version", 7)
        },
        "table jobs already exists",
    )?;
    Ok(())
}

#[test]
fn an_empty_or_missing_file_is_a_fresh_database() -> TestResult {
    let dir = TempDir::new();
    let empty = dir.join("empty.sqlite3");
    std::fs::File::create(&empty)?;
    SqliteStore::open(&empty)?;
    assert_eq!(user_version(&empty)?, SCHEMA_VERSION);
    let missing = dir.join("missing.sqlite3");
    SqliteStore::open(&missing)?;
    assert_eq!(user_version(&missing)?, SCHEMA_VERSION);
    assert_eq!(schema_of(&empty)?, schema_of(&missing)?);
    Ok(())
}

/// The token store creates and alters nothing: on a file that is not at the
/// current version it fails and leaves it byte for byte.
#[test]
fn the_token_store_alone_refuses_other_files_without_touching_them() -> TestResult {
    for wal in [false, true] {
        for (name, setup) in [
            ("newer", "PRAGMA user_version = 99"),
            ("older", "CREATE TABLE a (x TEXT); PRAGMA user_version = 4"),
            ("foreign", "CREATE TABLE notes (body TEXT)"),
            ("empty", "SELECT 1"),
        ] {
            let dir = TempDir::new();
            let path = database_in(dir.path());
            {
                let connection = Connection::open(&path)?;
                connection.execute_batch(setup)?;
                if wal {
                    connection.pragma_update(None, "journal_mode", "WAL")?;
                }
            }
            let before = files_of(dir.path());
            let error = SqliteTokenStore::open(&path)
                .err()
                .ok_or_else(|| format!("{name} (wal {wal}) was accepted"))?;
            assert!(
                error.to_string().contains("schema version"),
                "{name}: {error}"
            );
            assert_eq!(files_of(dir.path()), before, "{name} (wal {wal})");
        }
    }
    let missing = TempDir::new().join("none.sqlite3");
    assert!(SqliteTokenStore::open(&missing).is_err());
    assert!(
        !missing.exists(),
        "the token store must not create the file"
    );
    Ok(())
}
