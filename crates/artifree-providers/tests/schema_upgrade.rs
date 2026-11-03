//! Upgrading released schemas, and opens that must change nothing.

mod common;

use artifree_domain::AppDataVersion;
use artifree_domain::change::ChangeSequence;
use artifree_domain::data::{DataChange, DataValue};
use artifree_domain::identity::UnixMillis;
use artifree_domain::job::JobKind;
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactStore, ChangeKind, Delivery, ListOrder, NewArtifact, PurgeRequest,
    RequestFingerprint, RequestIdentity, RevisionCommit, StoreError,
};
use artifree_ports::data_store::{DataStore, DataStoreError, DataWrite};
use artifree_ports::job_store::{JobStore, JobStoreError, NewJob};
use artifree_ports::token_store::TokenStore;
use artifree_ports::user_store::UserStore;
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
    for version in 1..=9 {
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
        assert_data_works_beside_old_keys(&store, version)?;

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
            let identity = SqliteTokenStore::open(&path)?;
            let tokens = identity.list()?;
            assert_eq!(tokens.len(), 1, "the token of the version 5 file is kept");
            assert_eq!(tokens[0].name, "laptop");
            // The token keeps authenticating, as the owner profile
            // (decision 0036): its user is the one every earlier file used.
            assert!(tokens[0].actor.is_owner(), "version {version}");
            assert!(tokens[0].is_active(), "version {version}");
            let found = identity.find_by_hash("hash-1")?.ok_or("token found")?;
            assert!(found.actor.is_owner() && found.is_active());
        } else {
            assert!(SqliteTokenStore::open(&path)?.list()?.is_empty());
        }
        // The registry of every upgraded file holds the owner profile alone,
        // enabled, and no other user (the artifacts of the fixtures name an
        // actor, "owner", that was never registered, and still work).
        let users = SqliteTokenStore::open(&path)?.users()?;
        assert_eq!(users.len(), 1, "version {version}");
        assert!(users[0].id.is_owner() && users[0].is_enabled());
    }
    Ok(())
}

/// A database of any released version gets a feed of the owner's own: every
/// cursor handed out before stays valid, the head does not move back, and the
/// next commit continues after it. Another actor starts at nothing.
#[test]
fn an_upgraded_feed_keeps_its_cursors_and_continues_after_its_head() -> TestResult {
    for version in 2..=9 {
        let dir = TempDir::new();
        let path = database_in(dir.path());
        historical_database(&path, version)?;
        let store = SqliteStore::open(&path)?;
        let owner = actor("owner");

        let behind = store.changes(&owner, ChangeSequence(1), 100)?;
        let seen: Vec<_> = behind
            .changes
            .iter()
            .map(|change| (change.sequence, change.kind))
            .collect();
        assert_eq!(
            seen,
            [(ChangeSequence(2), ChangeKind::Revised)],
            "version {version}"
        );
        assert_eq!(behind.head, ChangeSequence(5), "version {version}");

        let saved = store.commit_revision(RevisionCommit {
            request: RequestIdentity {
                actor: &owner,
                key: &key("after-upgrade"),
                fingerprint: RequestFingerprint([5; 32]),
            },
            artifact: &old_artifact(),
            base: SourceRevision(2),
            package: &common::package(&[("index.html", "<h1>new</h1>")]),
        })?;
        assert_eq!(saved.delivery, Delivery::Committed, "version {version}");
        let next = store.changes(&owner, ChangeSequence(5), 100)?;
        let numbers: Vec<_> = next.changes.iter().map(|c| c.sequence).collect();
        assert_eq!(numbers, [ChangeSequence(6)], "version {version}");
        assert_eq!(next.head, ChangeSequence(6), "version {version}");

        let stranger = store.changes(&actor("other"), ChangeSequence(0), 100)?;
        assert!(stranger.changes.is_empty(), "version {version}");
        assert_eq!(stranger.head, ChangeSequence(0), "version {version}");
    }
    Ok(())
}

const PURGED_ID: &str = "art_22222222222222222222222222222222";
const ORPHAN_ID: &str = "art_33333333333333333333333333333333";

/// Every change row as text, in write order, whatever the columns around it.
fn change_rows(path: &std::path::Path) -> Result<Vec<String>, rusqlite::Error> {
    let connection = Connection::open(path)?;
    let mut statement = connection.prepare(
        "SELECT sequence || '|' || artifact_id || '|' || kind || '|' || revision || '|'
             || metadata_revision || '|' || committed_at FROM changes ORDER BY sequence",
    )?;
    statement.query_map([], |row| row.get(0))?.collect()
}

/// Change rows of an artifact that was purged (its row stays as a tombstone)
/// and of one whose artifact row is gone (no Artifree build removes one, and
/// foreign keys forbid it, so only a file edited elsewhere holds such a row):
/// the upgrade keeps every row and every number, and the second kind goes to
/// the owner profile.
#[test]
fn an_upgrade_keeps_every_change_row_including_a_purged_and_an_orphaned_artifacts() -> TestResult {
    for version in 7..=9 {
        let dir = TempDir::new();
        let path = database_in(dir.path());
        historical_database(&path, version)?;
        // Foreign keys are off while the file is edited "elsewhere": the
        // orphan's revision rows name an artifact that is not there.
        Connection::open(&path)?.execute_batch(&format!(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO artifacts (id, owner, kind, title, current_revision, created_at,
                 updated_at, pinned, metadata_revision, deleted_at, purged_at)
                 VALUES ('{PURGED_ID}', 'owner', 'app', 'Gone', 1, 160, 160, 0, 3, 450, 500);
             INSERT INTO revisions VALUES ('{PURGED_ID}', 1, 'owner', 160);
             INSERT INTO metadata_revisions
                 (artifact_id, metadata_revision, title, pinned, created_by, created_at, deleted_at)
                 VALUES ('{PURGED_ID}', 1, 'Gone', 0, 'owner', 160, NULL),
                        ('{PURGED_ID}', 2, 'Gone', 0, 'owner', 450, 450),
                        ('{PURGED_ID}', 3, 'Gone', 0, 'owner', 500, 500);
             INSERT INTO changes (sequence, artifact_id, kind, revision, metadata_revision, committed_at)
                 VALUES (6, '{PURGED_ID}', 'created', 1, 1, 160),
                        (7, '{PURGED_ID}', 'deleted', 1, 2, 450),
                        (8, '{PURGED_ID}', 'purged', 1, 3, 500);
             INSERT INTO revisions VALUES ('{ORPHAN_ID}', 1, 'owner', 170);
             INSERT INTO metadata_revisions
                 (artifact_id, metadata_revision, title, pinned, created_by, created_at)
                 VALUES ('{ORPHAN_ID}', 1, 'Lost', 0, 'owner', 170);
             INSERT INTO changes (sequence, artifact_id, kind, revision, metadata_revision, committed_at)
                 VALUES (9, '{ORPHAN_ID}', 'created', 1, 1, 170);
             UPDATE sqlite_sequence SET seq = 9 WHERE name = 'changes';"
        ))?;
        let before = change_rows(&path)?;
        assert_eq!(before.len(), 6, "the fixture of version {version}");

        let store = SqliteStore::open(&path)?;

        assert_eq!(change_rows(&path)?, before, "version {version}");
        let numbering: Vec<String> = {
            let connection = Connection::open(&path)?;
            let mut statement = connection
                .prepare("SELECT actor || '|' || sequence || '|' || actor_seq FROM changes ORDER BY sequence")?;
            statement
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?
        };
        assert_eq!(
            numbering,
            [
                "owner|1|1",
                "owner|2|2",
                "owner|6|6",
                "owner|7|7",
                "owner|8|8",
                "local|9|9"
            ],
            "version {version}: the purged artifact's rows stay the owner's, the orphan's go to the owner profile"
        );
        // The owner's cursors of before are still valid and see the purge.
        let owner = actor("owner");
        let seen: Vec<_> = store
            .changes(&owner, ChangeSequence(5), 100)?
            .changes
            .iter()
            .map(|change| (change.sequence, change.kind))
            .collect();
        assert_eq!(
            seen,
            [
                (ChangeSequence(6), ChangeKind::Created),
                (ChangeSequence(7), ChangeKind::Deleted),
                (ChangeSequence(8), ChangeKind::Purged)
            ],
            "version {version}"
        );
        let local = store.changes(&actor("local"), ChangeSequence(0), 100)?;
        assert_eq!(
            local
                .changes
                .iter()
                .map(|change| change.sequence)
                .collect::<Vec<_>>(),
            [ChangeSequence(9)],
            "version {version}"
        );
        assert_eq!(local.head, ChangeSequence(9), "version {version}");
    }
    Ok(())
}

/// A file that already holds changes of two actors (no build wrote one, but
/// the schema allows it): each actor keeps the numbers its changes had and
/// starts at the old global head.
#[test]
fn an_upgrade_gives_every_actor_with_changes_the_old_head() -> TestResult {
    let dir = TempDir::new();
    let path = database_in(dir.path());
    historical_database(&path, 9)?;
    let other_id = "art_11111111111111111111111111111111";
    Connection::open(&path)?.execute_batch(&format!(
        "INSERT INTO artifacts (id, owner, kind, title, current_revision, created_at, updated_at,
             pinned, metadata_revision)
             VALUES ('{other_id}', 'other', 'app', 'Theirs', 1, 700, 700, 0, 1);
         INSERT INTO revisions VALUES ('{other_id}', 1, 'other', 700);
         INSERT INTO revision_files VALUES ('{other_id}', 1, 'index.html', 12, zeroblob(32));
         INSERT INTO metadata_revisions
             (artifact_id, metadata_revision, title, pinned, created_by, created_at)
             VALUES ('{other_id}', 1, 'Theirs', 0, 'other', 700);
         INSERT INTO changes (sequence, artifact_id, kind, revision, metadata_revision, committed_at)
             VALUES (6, '{other_id}', 'created', 1, 1, 700);
         UPDATE sqlite_sequence SET seq = 6 WHERE name = 'changes';"
    ))?;
    let store = SqliteStore::open(&path)?;

    let theirs = store.changes(&actor("other"), ChangeSequence(0), 100)?;
    let numbers: Vec<_> = theirs.changes.iter().map(|c| c.sequence).collect();
    assert_eq!(numbers, [ChangeSequence(6)]);
    assert_eq!(theirs.head, ChangeSequence(6));
    let ours = store.changes(&actor("owner"), ChangeSequence(0), 100)?;
    let numbers: Vec<_> = ours.changes.iter().map(|c| c.sequence).collect();
    assert_eq!(numbers, [ChangeSequence(1), ChangeSequence(2)]);
    assert_eq!(ours.head, ChangeSequence(6), "no actor's head moves back");
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

/// Application data works on the old artifact of every released version, and
/// its keys share one space with the keys the file already held (decision
/// 0031).
fn assert_data_works_beside_old_keys(store: &SqliteStore, version: i64) -> TestResult {
    let owner = actor("owner");
    let change = DataChange::Set {
        path: parsed("greeting"),
        value: DataValue::parse("\"hello\"")?,
    };
    let write = |name: &str, base: u64| {
        let request_key = parsed(&format!("test-key-{name}"));
        store.write_data(DataWrite {
            request: RequestIdentity {
                actor: &owner,
                key: &request_key,
                fingerprint: RequestFingerprint([4; 32]),
            },
            artifact: &old_artifact(),
            base: AppDataVersion(base),
            change: &change,
            now: UnixMillis(2_000),
        })
    };
    let before = store.read_data(&old_artifact(), &owner)?;
    assert_eq!(
        (before.version, before.space.is_empty()),
        (AppDataVersion(0), true)
    );
    let mut used = vec!["old"];
    if version >= 7 {
        used.push("swept");
    }
    if version >= 8 {
        used.push("legacy-job");
    }
    for name in used {
        assert_eq!(
            write(name, 0).err(),
            Some(DataStoreError::RequestKeyReused),
            "version {version}: a data write under the key {name}"
        );
    }
    let first = write("first-data", 0)?;
    assert_eq!(first.version, AppDataVersion(1), "version {version}");
    let after = store.read_data(&old_artifact(), &owner)?;
    assert_eq!(after.space.to_canonical(), "{\"greeting\":\"hello\"}");
    // The write moved no revision of the old artifact.
    let old = store.artifact(&old_artifact())?.ok_or("artifact kept")?;
    assert_eq!(old.current_revision, SourceRevision(2));
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
    // The data step (version 8 to 9) fails: a table already uses the name.
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
            connection.execute_batch(history::STEP_8)?;
            connection.execute_batch("CREATE TABLE data_requests (x TEXT);")?;
            connection.pragma_update(None, "user_version", 8)
        },
        "table data_requests already exists",
    )?;
    // The users step (version 9 to 10) fails after it made the registry and
    // rebuilt the tokens: a table already uses a later name. All of it is
    // undone.
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
            connection.execute_batch(history::STEP_8)?;
            connection.execute_batch(history::STEP_9)?;
            connection.execute_batch("CREATE TABLE actor_feeds (x TEXT);")?;
            connection.pragma_update(None, "user_version", 9)
        },
        "table actor_feeds already exists",
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
