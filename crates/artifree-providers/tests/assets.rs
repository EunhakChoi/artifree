//! Atomic private upload admission, durable replay and blob lifecycle.

mod common;

use artifree_domain::assets::{AssetId, AssetMediaType, AssetRejection, AssetVersion};
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis};
use artifree_domain::{MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, CommitReceipt, Delivery, PurgeRequest, RequestFingerprint, RequestIdentity,
};
use artifree_ports::asset_store::{AssetReceipt, AssetStore, AssetStoreError, AssetWrite};
use artifree_providers::storage::SqliteStore;
use common::{TempDir, TestResult, actor, create_demo, database_in, digest, key, update_metadata};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Barrier};
use std::thread;

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
}
impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let store = SqliteStore::open(&database_in(dir.path()))?;
        let owner = actor("owner");
        let created = create_demo(&store, &owner, &key("create"), "Assets", "<h1>source</h1>")?;
        Ok(Self {
            dir,
            store,
            owner,
            created,
        })
    }
    fn id(&self) -> &ArtifactId {
        &self.created.artifact.id
    }
    fn upload(&self, name: &str, base: u64, bytes: &[u8]) -> Result<AssetReceipt, AssetStoreError> {
        upload(
            &self.store,
            &self.owner,
            self.id(),
            name,
            base,
            bytes,
            Ok(()),
        )
    }
    fn sql(&self) -> rusqlite::Result<Connection> {
        Connection::open(database_in(self.dir.path()))
    }
    fn counts(&self) -> rusqlite::Result<(i64, i64, i64, i64)> {
        self.sql()?.query_row("SELECT (SELECT count(*) FROM assets),(SELECT count(*) FROM asset_spaces),(SELECT count(*) FROM asset_requests),(SELECT count(*) FROM blobs)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))
    }
    fn delete(&self, name: &str, base: u64, id: &AssetId) -> Result<AssetReceipt, AssetStoreError> {
        let request_key = key(name);
        self.store.delete_asset(
            AssetWrite {
                request: RequestIdentity {
                    actor: &self.owner,
                    key: &request_key,
                    fingerprint: RequestFingerprint(digest(&format!(
                        "delete {base} {}",
                        id.as_str()
                    ))),
                },
                artifact: self.id(),
                base: AssetVersion(base),
                expected_source: None,
                now: UnixMillis(20),
            },
            id,
        )
    }
}
fn upload(
    store: &SqliteStore,
    owner: &ActorId,
    artifact: &ArtifactId,
    name: &str,
    base: u64,
    bytes: &[u8],
    validation: Result<(), AssetRejection>,
) -> Result<AssetReceipt, AssetStoreError> {
    let request_key = key(name);
    let media: AssetMediaType = common::parsed("text/plain");
    let mut input = format!("upload {artifact:?} {base} ").into_bytes();
    input.extend_from_slice(bytes);
    store.upload_asset(
        AssetWrite {
            request: RequestIdentity {
                actor: owner,
                key: &request_key,
                fingerprint: RequestFingerprint(Sha256::digest(&input).into()),
            },
            artifact,
            base: AssetVersion(base),
            expected_source: None,
            now: UnixMillis(10),
        },
        &media,
        bytes,
        validation,
    )
}
#[test]
fn exact_bytes_survive_reopen_and_equal_uploads_are_distinct() -> TestResult {
    let f = Fixture::new()?;
    let first = f.upload("one", 0, b"\xef\xbb\xbfhello\r\n")?;
    let second = f.upload("two", 1, b"\xef\xbb\xbfhello\r\n")?;
    assert_ne!(first.id, second.id);
    assert_ne!(
        first.id.as_str(),
        AssetId::from_random(digest("\u{feff}hello\r\n")).as_str()
    );
    let store = SqliteStore::open(&database_in(f.dir.path()))?;
    let read = store.asset_content(&f.owner, f.id(), &first.id)?;
    assert_eq!(read.bytes, b"\xef\xbb\xbfhello\r\n");
    assert_eq!(read.version, AssetVersion(2));
    let list = store.assets(&f.owner, f.id())?;
    assert_eq!((list.assets.len(), list.bytes), (2, 20));
    assert!(list.assets.windows(2).all(|pair| pair[0].id < pair[1].id));
    assert_eq!(
        f.counts()?,
        (2, 1, 2, 3),
        "equal uploads share one asset blob plus two source blobs"
    );
    Ok(())
}
#[test]
fn replay_precedes_base_policy_and_deleted_content_is_never_recreated() -> TestResult {
    let f = Fixture::new()?;
    let first = f.upload("one", 0, b"a")?;
    let replay = upload(
        &f.store,
        &f.owner,
        f.id(),
        "one",
        0,
        b"a",
        Err(AssetRejection::UnsupportedType),
    )?;
    assert_eq!(replay.delivery, Delivery::Replayed);
    assert_eq!(replay.id, first.id);
    let deleted = f.delete("delete", 1, &first.id)?;
    assert!(deleted.deleted);
    assert_eq!(deleted.version, AssetVersion(2));
    let replay = f.upload("one", 0, b"a")?;
    assert_eq!(
        (replay.id, replay.version, replay.delivery),
        (first.id.clone(), AssetVersion(1), Delivery::Replayed)
    );
    assert_eq!(
        f.store.asset_content(&f.owner, f.id(), &first.id),
        Err(AssetStoreError::NotFound)
    );
    assert_eq!(
        f.delete("delete", 1, &first.id)?.delivery,
        Delivery::Replayed
    );
    let noop = f.delete("noop", 2, &first.id)?;
    assert!(!noop.deleted);
    assert_eq!(noop.version, AssetVersion(2));
    assert_eq!(f.counts()?, (0, 1, 3, 2));
    Ok(())
}
#[test]
fn authority_key_and_base_win_over_content_rejection() -> TestResult {
    let f = Fixture::new()?;
    assert_eq!(
        upload(
            &f.store,
            &actor("bob"),
            f.id(),
            "foreign",
            0,
            b"a",
            Err(AssetRejection::Invalid)
        ),
        Err(AssetStoreError::NotFound)
    );
    assert_eq!(
        f.upload("create", 0, b"a"),
        Err(AssetStoreError::RequestKeyReused)
    );
    assert_eq!(
        f.upload("stale", 1, b"a"),
        Err(AssetStoreError::Conflict {
            current: AssetVersion(0)
        })
    );
    assert_eq!(
        upload(
            &f.store,
            &f.owner,
            f.id(),
            "reject",
            0,
            b"a",
            Err(AssetRejection::Invalid)
        ),
        Err(AssetStoreError::Rejected(AssetRejection::Invalid))
    );
    assert_eq!(f.counts()?, (0, 0, 0, 2));
    let first = f.upload("reject", 0, b"a")?;
    assert_eq!(
        f.delete("reject", 1, &first.id),
        Err(AssetStoreError::RequestKeyReused)
    );
    let request_key = key("reject");
    let stored: Vec<u8> = f.sql()?.query_row(
        "SELECT fingerprint FROM asset_requests WHERE request_key=?1",
        [request_key.as_str()],
        |row| row.get(0),
    )?;
    let matching = RequestFingerprint(stored.try_into().map_err(|_| "fingerprint length")?);
    let fenced = AssetWrite {
        request: RequestIdentity {
            actor: &f.owner,
            key: &request_key,
            fingerprint: matching,
        },
        artifact: f.id(),
        base: AssetVersion(0),
        expected_source: Some(SourceRevision(2)),
        now: UnixMillis(30),
    };
    assert_eq!(
        f.store.preflight_asset(fenced),
        Err(AssetStoreError::SourceConflict {
            current: SourceRevision(1)
        })
    );
    let media: AssetMediaType = "text/plain".parse()?;
    assert_eq!(
        f.store
            .upload_asset(fenced, &media, b"a", Err(AssetRejection::UnsupportedType)),
        Err(AssetStoreError::SourceConflict {
            current: SourceRevision(1)
        })
    );
    Ok(())
}
#[test]
fn empty_oversized_and_quota_rejection_leave_the_key_free() -> TestResult {
    let f = Fixture::new()?;
    assert_eq!(
        f.upload("retry", 0, b""),
        Err(AssetStoreError::Rejected(AssetRejection::Invalid))
    );
    let too_large = vec![b'a'; 5 * 1024 * 1024 + 1];
    assert_eq!(
        f.upload("retry", 0, &too_large),
        Err(AssetStoreError::Rejected(AssetRejection::LimitExceeded))
    );
    assert_eq!(f.counts()?, (0, 0, 0, 2));
    f.upload("retry", 0, b"a")?;
    for n in 1..64 {
        f.upload(&format!("fill-{n}"), n, b"a")?;
    }
    assert_eq!(
        f.upload("quota", 64, b"a"),
        Err(AssetStoreError::Rejected(AssetRejection::LimitExceeded))
    );
    assert_eq!(f.counts()?, (64, 1, 64, 3));
    let id = f.store.assets(&f.owner, f.id())?.assets[0].id.clone();
    f.delete("room", 64, &id)?;
    f.upload("quota", 65, b"a")?;
    assert_eq!(f.store.assets(&f.owner, f.id())?.assets.len(), 64);
    Ok(())
}
#[test]
fn final_slot_race_has_one_commit_and_no_partial_receipt() -> TestResult {
    let f = Fixture::new()?;
    for n in 0..63 {
        f.upload(&format!("fill-{n}"), n, b"a")?;
    }
    let gate = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for name in ["race-a", "race-b"] {
        let store = f.store.clone();
        let owner = f.owner.clone();
        let id = f.id().clone();
        let gate = Arc::clone(&gate);
        handles.push(thread::spawn(move || {
            gate.wait();
            upload(&store, &owner, &id, name, 63, b"last", Ok(()))
        }));
    }
    gate.wait();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .unwrap_or_else(|error| panic!("worker failed: {error:?}"))
        })
        .collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(
                r,
                Err(AssetStoreError::Conflict {
                    current: AssetVersion(64)
                })
            ))
            .count(),
        1
    );
    assert_eq!(f.counts()?, (64, 1, 64, 4));
    Ok(())
}
#[test]
fn concurrent_matching_settlement_replays_even_when_local_validation_rejected() -> TestResult {
    let f = Fixture::new()?;
    // Both first attempts have completed preflight before either settles.
    let request_key = key("same");
    let media: AssetMediaType = "text/plain".parse()?;
    let write = AssetWrite {
        request: RequestIdentity {
            actor: &f.owner,
            key: &request_key,
            fingerprint: RequestFingerprint([17; 32]),
        },
        artifact: f.id(),
        base: AssetVersion(0),
        expected_source: None,
        now: UnixMillis(10),
    };
    assert_eq!(f.store.preflight_asset(write)?, None);
    assert_eq!(f.store.preflight_asset(write)?, None);
    let winner = f.store.upload_asset(write, &media, b"a", Ok(()))?;
    let loser = f
        .store
        .upload_asset(write, &media, b"a", Err(AssetRejection::Invalid))?;
    assert_eq!(loser.delivery, Delivery::Replayed);
    assert_eq!(loser.id, winner.id);
    let different = AssetWrite {
        request: RequestIdentity {
            fingerprint: RequestFingerprint([18; 32]),
            ..write.request
        },
        ..write
    };
    assert_eq!(
        f.store
            .upload_asset(different, &media, b"b", Err(AssetRejection::Invalid)),
        Err(AssetStoreError::RequestKeyReused)
    );
    assert_eq!(f.counts()?, (1, 1, 1, 3));
    Ok(())
}
#[test]
fn late_receipt_failure_rolls_back_blob_space_and_id_reservation() -> TestResult {
    let f = Fixture::new()?;
    f.sql()?.execute_batch("CREATE TRIGGER refuse_receipt BEFORE INSERT ON asset_requests BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;")?;
    let result = f.upload("retry", 0, b"new unique bytes");
    assert!(
        matches!(result, Err(AssetStoreError::Unavailable(_))),
        "{result:?}"
    );
    assert_eq!(f.counts()?, (0, 0, 0, 2));
    let ids: i64 = f
        .sql()?
        .query_row("SELECT count(*) FROM asset_ids", [], |row| row.get(0))?;
    assert_eq!(ids, 0);
    f.sql()?.execute_batch("DROP TRIGGER refuse_receipt;")?;
    assert_eq!(
        f.upload("retry", 0, b"new unique bytes")?.delivery,
        Delivery::Committed
    );
    Ok(())
}
#[test]
fn trash_recover_purge_keep_versions_and_scrub_reserved_receipts() -> TestResult {
    let f = Fixture::new()?;
    let first = f.upload("upload", 0, b"private bytes")?;
    update_metadata(
        &f.store,
        &f.created,
        &key("trash"),
        1,
        None,
        None,
        Some(true),
    )?;
    assert_eq!(
        f.store.assets(&f.owner, f.id()),
        Err(AssetStoreError::NotFound)
    );
    assert_eq!(
        f.upload("upload", 0, b"private bytes"),
        Err(AssetStoreError::NotFound)
    );
    update_metadata(
        &f.store,
        &f.created,
        &key("recover"),
        2,
        None,
        None,
        Some(false),
    )?;
    assert_eq!(
        f.store.asset_content(&f.owner, f.id(), &first.id)?.version,
        AssetVersion(1)
    );
    update_metadata(
        &f.store,
        &f.created,
        &key("trash-again"),
        3,
        None,
        None,
        Some(true),
    )?;
    let purge_key = key("purge");
    let purge = PurgeRequest {
        request: RequestIdentity {
            actor: &f.owner,
            key: &purge_key,
            fingerprint: RequestFingerprint([12; 32]),
        },
        artifact: f.id(),
        base: MetadataRevision(4),
    };
    f.store.purge_artifact(purge)?;
    assert_eq!(f.store.purge_artifact(purge)?.delivery, Delivery::Replayed);
    assert_eq!(f.counts()?, (0, 0, 1, 0));
    let scrubbed:(i64,bool)=f.sql()?.query_row("SELECT count(*), min(asset_id IS NULL AND content_type IS NULL AND size IS NULL AND digest IS NULL AND uploaded_at IS NULL AND fingerprint=zeroblob(32)) FROM asset_requests",[],|row|Ok((row.get(0)?,row.get(1)?)))?;
    assert_eq!(scrubbed, (1, true));
    let other = create_demo(&f.store, &f.owner, &key("other"), "Other", "<h1>other</h1>")?;
    assert_eq!(
        upload(
            &f.store,
            &f.owner,
            &other.artifact.id,
            "upload",
            0,
            b"private bytes",
            Ok(())
        ),
        Err(AssetStoreError::RequestKeyReused)
    );
    assert_eq!(
        create_demo(
            &f.store,
            &f.owner,
            &key("upload"),
            "Again",
            "<h1>again</h1>"
        ),
        Err(artifree_ports::artifact_store::StoreError::RequestKeyReused)
    );
    assert_eq!(
        f.store.asset_content(&f.owner, f.id(), &first.id),
        Err(AssetStoreError::NotFound)
    );
    let reserved = key("upload");
    assert_eq!(
        f.store.preflight_asset(AssetWrite {
            request: RequestIdentity {
                actor: &f.owner,
                key: &reserved,
                fingerprint: RequestFingerprint([0; 32])
            },
            artifact: &other.artifact.id,
            base: AssetVersion(0),
            expected_source: None,
            now: UnixMillis(40),
        }),
        Err(AssetStoreError::RequestKeyReused),
        "a scrubbed receipt cannot match a zero fingerprint"
    );
    Ok(())
}
#[test]
fn source_and_asset_references_both_protect_shared_blob_bytes() -> TestResult {
    let f = Fixture::new()?;
    let shared = f.upload("shared", 0, b"<h1>source</h1>")?;
    assert_eq!(f.store.unreferenced_content()?.blobs, 0);
    f.delete("remove-shared", 1, &shared.id)?;
    assert_eq!(
        f.store
            .revision_file(f.id(), SourceRevision(1), &"index.html".parse()?)?
            .as_deref(),
        Some(b"<h1>source</h1>".as_slice())
    );
    let other = create_demo(&f.store, &f.owner, &key("other"), "Other", "<h1>other</h1>")?;
    let preserved = upload(
        &f.store,
        &f.owner,
        &other.artifact.id,
        "other-shared",
        0,
        b"<h1>source</h1>",
        Ok(()),
    )?;
    update_metadata(
        &f.store,
        &f.created,
        &key("trash"),
        1,
        None,
        None,
        Some(true),
    )?;
    let purge_key = key("purge");
    f.store.purge_artifact(PurgeRequest {
        request: RequestIdentity {
            actor: &f.owner,
            key: &purge_key,
            fingerprint: RequestFingerprint([8; 32]),
        },
        artifact: f.id(),
        base: MetadataRevision(2),
    })?;
    assert_eq!(
        f.store
            .asset_content(&f.owner, &other.artifact.id, &preserved.id)?
            .bytes,
        b"<h1>source</h1>"
    );
    assert_eq!(f.store.unreferenced_content()?.blobs, 0);
    Ok(())
}

#[test]
fn actor_quota_includes_trashed_spaces_and_equal_byte_associations() -> TestResult {
    let f = Fixture::new()?;
    let mut artifacts = vec![f.created.clone()];
    for n in 1..5 {
        artifacts.push(create_demo(
            &f.store,
            &f.owner,
            &key(&format!("create-{n}")),
            "Quota",
            "<h1>quota</h1>",
        )?);
    }
    for (space, created) in artifacts.iter().take(4).enumerate() {
        for n in 0..64 {
            upload(
                &f.store,
                &f.owner,
                &created.artifact.id,
                &format!("fill-{space}-{n}"),
                n,
                b"a",
                Ok(()),
            )?;
        }
        update_metadata(
            &f.store,
            created,
            &key(&format!("trash-{space}")),
            1,
            None,
            None,
            Some(true),
        )?;
    }
    assert_eq!(
        upload(
            &f.store,
            &f.owner,
            &artifacts[4].artifact.id,
            "actor-overflow",
            0,
            b"a",
            Ok(())
        ),
        Err(AssetStoreError::Rejected(AssetRejection::LimitExceeded))
    );
    assert_eq!(
        f.store.assets(&f.owner, &artifacts[4].artifact.id)?.version,
        AssetVersion(0)
    );
    assert_eq!(f.counts()?.0, 256);
    Ok(())
}
#[test]
fn byte_quota_uses_exact_logical_bytes_and_rolls_back_overflow() -> TestResult {
    let f = Fixture::new()?;
    let bytes = vec![b'a'; 5 * 1024 * 1024];
    for n in 0..5 {
        f.upload(&format!("fill-{n}"), n, &bytes)?;
    }
    assert_eq!(f.store.assets(&f.owner, f.id())?.bytes, 25 * 1024 * 1024);
    assert_eq!(
        f.upload("overflow", 5, b"a"),
        Err(AssetStoreError::Rejected(AssetRejection::LimitExceeded))
    );
    assert_eq!(f.counts()?, (5, 1, 5, 3));
    Ok(())
}
#[test]
fn backup_restore_keeps_asset_bytes_versions_receipts_and_independent_future_changes() -> TestResult
{
    use artifree_providers::backup::{backup, restore};
    let f = Fixture::new()?;
    let first = f.upload("before-backup", 0, b"backup bytes")?;
    let output = TempDir::new();
    let snapshot = output.join("snapshot.sqlite3");
    backup(f.dir.path(), &snapshot)?;
    f.delete("delete-after-backup", 1, &first.id)?;
    let restored_path = output.join("restored");
    restore(&snapshot, &restored_path)?;
    let restored = SqliteStore::open(&database_in(&restored_path))?;
    assert_eq!(
        restored.asset_content(&f.owner, f.id(), &first.id)?.bytes,
        b"backup bytes"
    );
    assert_eq!(
        upload(
            &restored,
            &f.owner,
            f.id(),
            "before-backup",
            0,
            b"backup bytes",
            Err(AssetRejection::Invalid)
        )?
        .delivery,
        Delivery::Replayed
    );
    assert_eq!(
        f.store.asset_content(&f.owner, f.id(), &first.id),
        Err(AssetStoreError::NotFound)
    );
    Ok(())
}
#[test]
fn schema_eleven_upgrade_adds_assets_and_failed_upgrade_is_atomic() -> TestResult {
    let f = Fixture::new()?;
    let sql = "DROP TABLE asset_requests; DROP TABLE assets; DROP TABLE asset_spaces; DROP TABLE asset_ids; PRAGMA user_version=11;";
    f.sql()?.execute_batch(sql)?;
    let reopened = SqliteStore::open(&database_in(f.dir.path()))?;
    assert_eq!(reopened.assets(&f.owner, f.id())?.version, AssetVersion(0));
    upload(
        &reopened,
        &f.owner,
        f.id(),
        "upgraded",
        0,
        b"persist",
        Ok(()),
    )?;
    // A conflicting first migration table must roll the complete migration back.
    let other = Fixture::new()?;
    other.sql()?.execute_batch(sql)?;
    other
        .sql()?
        .execute_batch("CREATE TABLE asset_ids (wrong TEXT);")?;
    let result = SqliteStore::open(&database_in(other.dir.path()));
    assert!(
        matches!(
            result,
            Err(artifree_ports::artifact_store::StoreError::Unavailable(_))
        ),
        "{result:?}"
    );
    let version: i64 = other
        .sql()?
        .query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(version, 11);
    let spaces: i64 = other.sql()?.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name='asset_spaces'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(spaces, 0);
    Ok(())
}

#[test]
fn an_upload_between_cleanup_dry_run_and_apply_protects_the_new_association() -> TestResult {
    use artifree_ports::artifact_store::{CleanupRequest, StoreError};
    let fixture = Fixture::new()?;
    let bytes = b"asset rescue across dry run";
    let hash: [u8; 32] = Sha256::digest(bytes).into();
    fixture.sql()?.execute(
        "INSERT INTO blobs(digest,content) VALUES (?1,?2)",
        rusqlite::params![hash.as_slice(), bytes.as_slice()],
    )?;
    let reviewed = fixture.store.unreferenced_content()?;
    assert_eq!((reviewed.blobs, reviewed.bytes), (1, 27));
    let uploaded = fixture.upload("rescue", 0, bytes)?;
    let cleanup_key = key("cleanup");
    let refused = fixture.store.remove_unreferenced_content(CleanupRequest {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &cleanup_key,
            fingerprint: RequestFingerprint(digest(&format!("cleanup {}", reviewed.digest))),
        },
        base: reviewed.digest,
    });
    assert_eq!(refused, Err(StoreError::ContentChanged));
    assert_eq!(
        fixture
            .store
            .asset_content(&fixture.owner, fixture.id(), &uploaded.id)?
            .bytes,
        bytes
    );
    let current = fixture.store.unreferenced_content()?;
    assert_eq!((current.blobs, current.bytes), (0, 0));
    let applied = fixture.store.remove_unreferenced_content(CleanupRequest {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &cleanup_key,
            fingerprint: RequestFingerprint(digest(&format!("cleanup {}", current.digest))),
        },
        base: current.digest,
    })?;
    assert_eq!(
        (applied.blobs, applied.bytes),
        (0, 0),
        "stale refusal did not consume the key"
    );
    assert_eq!(
        fixture
            .store
            .asset_content(&fixture.owner, fixture.id(), &uploaded.id)?
            .bytes,
        bytes
    );
    Ok(())
}

#[test]
fn an_opaque_id_or_digest_does_not_authorize_another_accessible_space() -> TestResult {
    let fixture = Fixture::new()?;
    let first = fixture.upload("first-space", 0, b"equal private bytes")?;
    let other = create_demo(
        &fixture.store,
        &fixture.owner,
        &key("create-other"),
        "Other",
        "<h1>other</h1>",
    )?;
    let second = upload(
        &fixture.store,
        &fixture.owner,
        &other.artifact.id,
        "second-space",
        0,
        b"equal private bytes",
        Ok(()),
    )?;
    assert_ne!(first.id, second.id);
    assert_eq!(
        fixture
            .store
            .asset_content(&fixture.owner, &other.artifact.id, &first.id),
        Err(AssetStoreError::NotFound)
    );
    assert_eq!(
        fixture
            .store
            .asset_content(&fixture.owner, fixture.id(), &second.id),
        Err(AssetStoreError::NotFound)
    );
    let digest_id = AssetId::from_random(Sha256::digest(b"equal private bytes").into());
    assert_eq!(
        fixture
            .store
            .asset_content(&fixture.owner, fixture.id(), &digest_id),
        Err(AssetStoreError::NotFound)
    );
    let noop_key = key("foreign-id-delete");
    let noop = fixture.store.delete_asset(
        AssetWrite {
            request: RequestIdentity {
                actor: &fixture.owner,
                key: &noop_key,
                fingerprint: RequestFingerprint([27; 32]),
            },
            artifact: &other.artifact.id,
            base: AssetVersion(1),
            expected_source: None,
            now: UnixMillis(11),
        },
        &first.id,
    )?;
    assert!(!noop.deleted);
    assert_eq!(noop.version, AssetVersion(1));
    assert_eq!(
        fixture
            .store
            .asset_content(&fixture.owner, fixture.id(), &first.id)?
            .bytes,
        b"equal private bytes"
    );
    assert_eq!(
        fixture
            .store
            .asset_content(&fixture.owner, &other.artifact.id, &second.id)?
            .bytes,
        b"equal private bytes"
    );
    Ok(())
}
