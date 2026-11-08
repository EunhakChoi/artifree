//! Duplicates that copy the actor's asset space (decision 0045 rule 10):
//! explicit opt-in, retained IDs, full quota charging with whole-copy refusal,
//! and independence of the two spaces afterwards.

mod common;

use artifree_domain::ArtifactKind;
use artifree_domain::assets::{AssetMediaType, AssetVersion};
use artifree_domain::identity::{ActorId, ArtifactId, Title, UnixMillis};
use artifree_domain::{MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, CommitReceipt, CopySource, Delivery, NewArtifact, PurgeRequest,
    RequestFingerprint, RequestIdentity, StoreError,
};
use artifree_ports::asset_store::{AssetReceipt, AssetStore, AssetStoreError, AssetWrite};
use artifree_providers::storage::SqliteStore;
use common::{
    TempDir, TestResult, actor, create_demo, database_in, digest, key, package, parsed,
    update_metadata,
};
use rusqlite::{Connection, params};

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    source: CommitReceipt,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let store = SqliteStore::open(&database_in(dir.path()))?;
        let owner = actor("owner");
        let source = create_demo(&store, &owner, &key("create"), "Source", "<h1>source</h1>")?;
        Ok(Self {
            dir,
            store,
            owner,
            source,
        })
    }

    fn id(&self) -> &ArtifactId {
        &self.source.artifact.id
    }

    fn sql(&self) -> rusqlite::Result<Connection> {
        Connection::open(database_in(self.dir.path()))
    }

    fn upload(
        &self,
        artifact: &ArtifactId,
        name: &str,
        base: u64,
        bytes: &[u8],
    ) -> Result<AssetReceipt, AssetStoreError> {
        let request_key = key(name);
        let media: AssetMediaType = parsed("image/png");
        self.store.upload_asset(
            AssetWrite {
                request: RequestIdentity {
                    actor: &self.owner,
                    key: &request_key,
                    fingerprint: RequestFingerprint(digest(&format!("upload {name}"))),
                },
                artifact,
                base: AssetVersion(base),
                expected_source: None,
                now: UnixMillis(10),
            },
            &media,
            bytes,
            Ok(()),
        )
    }

    fn delete(
        &self,
        artifact: &ArtifactId,
        name: &str,
        base: u64,
        asset: &AssetReceipt,
    ) -> Result<AssetReceipt, AssetStoreError> {
        let request_key = key(name);
        self.store.delete_asset(
            AssetWrite {
                request: RequestIdentity {
                    actor: &self.owner,
                    key: &request_key,
                    fingerprint: RequestFingerprint(digest(&format!("delete {name}"))),
                },
                artifact,
                base: AssetVersion(base),
                expected_source: None,
                now: UnixMillis(20),
            },
            &asset.id,
        )
    }

    /// A duplicate of the source as the core makes one, with a fingerprint
    /// that depends on what it copies.
    fn copy(&self, name: &str, copy_assets: bool) -> Result<CommitReceipt, StoreError> {
        let request_key = key(name);
        self.store.create_artifact(NewArtifact {
            request: RequestIdentity {
                actor: &self.owner,
                key: &request_key,
                fingerprint: RequestFingerprint(digest(&format!("copy {copy_assets}"))),
            },
            kind: ArtifactKind::App,
            title: &parsed::<Title>("Source (Copy)"),
            package: &package(&[("index.html", "<h1>source</h1>"), ("style.css", "h1{}")]),
            source: Some(CopySource {
                artifact: self.id(),
                copy_data: false,
                copy_assets,
            }),
        })
    }

    /// Trashes then purges an artifact whose metadata is at revision 1.
    fn purge(&self, created: &CommitReceipt, name: &str) -> TestResult {
        update_metadata(
            &self.store,
            created,
            &key(&format!("trash-{name}")),
            1,
            None,
            None,
            Some(true),
        )?;
        let purge_key = key(&format!("purge-{name}"));
        self.store.purge_artifact(PurgeRequest {
            request: RequestIdentity {
                actor: &self.owner,
                key: &purge_key,
                fingerprint: RequestFingerprint(digest(&format!("purge {name}"))),
            },
            artifact: &created.artifact.id,
            base: MetadataRevision(2),
        })?;
        Ok(())
    }

    /// Rows that a refused copy must leave as they were: artifacts, create
    /// keys, asset associations, asset spaces, upload receipts, and blobs.
    fn rows(&self) -> rusqlite::Result<[i64; 6]> {
        self.sql()?.query_row(
            "SELECT (SELECT count(*) FROM artifacts), (SELECT count(*) FROM request_keys),
                    (SELECT count(*) FROM assets), (SELECT count(*) FROM asset_spaces),
                    (SELECT count(*) FROM asset_requests), (SELECT count(*) FROM blobs)",
            [],
            |row| {
                Ok([
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ])
            },
        )
    }

    /// Fills one new artifact with 64 one-byte assets and moves it to the
    /// trash, where its assets still count toward the actor's limit.
    fn trashed_full_space(&self, name: &str) -> TestResult {
        let created = create_demo(
            &self.store,
            &self.owner,
            &key(&format!("create-{name}")),
            "Full",
            "<h1>full</h1>",
        )?;
        for n in 0..64 {
            self.upload(&created.artifact.id, &format!("{name}-{n}"), n, b"a")?;
        }
        update_metadata(
            &self.store,
            &created,
            &key(&format!("trash-{name}")),
            1,
            None,
            None,
            Some(true),
        )?;
        Ok(())
    }
}

#[test]
fn a_default_duplicate_copies_no_assets() -> TestResult {
    let f = Fixture::new()?;
    f.upload(f.id(), "one", 0, b"png bytes")?;
    let copy = f.copy("copy", false)?;
    let listed = f.store.assets(&f.owner, &copy.artifact.id)?;
    assert_eq!(
        (listed.assets.len(), listed.bytes, listed.version),
        (0, 0, AssetVersion(0))
    );
    Ok(())
}

#[test]
fn an_asset_copy_takes_every_association_with_its_id_at_version_one() -> TestResult {
    let f = Fixture::new()?;
    // Neither upload is referenced by the source files; both are copied.
    f.upload(f.id(), "one", 0, b"first bytes")?;
    let second = f.upload(f.id(), "two", 1, b"second")?;
    let receipts_before = f.rows()?[4];

    let copy = f.copy("copy", true)?;

    let source = f.store.assets(&f.owner, f.id())?;
    let copied = f.store.assets(&f.owner, &copy.artifact.id)?;
    assert_eq!(
        copied.assets, source.assets,
        "same IDs, types, sizes, digests, times"
    );
    assert_eq!(copied.assets.len(), 2);
    assert_eq!((copied.version, copied.bytes), (AssetVersion(1), 17));
    assert_eq!(source.version, AssetVersion(2), "the source is not touched");
    assert_eq!(
        f.store
            .asset_content(&f.owner, &copy.artifact.id, &second.id)?
            .bytes,
        b"second"
    );
    assert_eq!(
        f.rows()?[4],
        receipts_before,
        "upload receipts are not copied"
    );
    Ok(())
}

#[test]
fn an_asset_copy_of_an_empty_space_writes_no_space() -> TestResult {
    let f = Fixture::new()?;
    let before = f.rows()?;
    let copy = f.copy("copy", true)?;
    assert_eq!(
        f.store.assets(&f.owner, &copy.artifact.id)?.version,
        AssetVersion(0)
    );
    let after = f.rows()?;
    assert_eq!((after[2], after[3]), (before[2], before[3]));
    Ok(())
}

#[test]
fn a_copy_over_the_actor_limit_is_refused_whole_and_leaves_its_key_free() -> TestResult {
    let f = Fixture::new()?;
    for n in 0..64 {
        f.upload(f.id(), &format!("source-{n}"), n, b"a")?;
    }
    f.trashed_full_space("first")?;
    f.trashed_full_space("second")?;
    // 192 associations plus a copy of 64 would fit exactly; one more does not.
    let extra = create_demo(
        &f.store,
        &f.owner,
        &key("create-extra"),
        "Extra",
        "<h1>x</h1>",
    )?;
    let one = f.upload(&extra.artifact.id, "extra", 0, b"a")?;
    let before = f.rows()?;

    assert_eq!(f.copy("copy", true), Err(StoreError::AssetLimitExceeded));
    assert_eq!(f.rows()?, before, "no artifact, key, association or blob");

    f.delete(&extra.artifact.id, "free-extra", 1, &one)?;
    let copy = f.copy("copy", true)?;
    assert_eq!(
        copy.delivery,
        Delivery::Committed,
        "the refusal kept no key"
    );
    assert_eq!(
        f.store.assets(&f.owner, &copy.artifact.id)?.assets.len(),
        64
    );
    assert_eq!(f.copy("copy", true)?.delivery, Delivery::Replayed);
    Ok(())
}

#[test]
fn a_copy_over_the_space_limit_is_refused() -> TestResult {
    let f = Fixture::new()?;
    let uploaded = f.upload(f.id(), "one", 0, b"a")?;
    // Uploads cannot exceed the space limit, so the excess is written
    // directly: the copy's space must be checked on its own.
    let connection = f.sql()?;
    for n in 0..64 {
        let id = format!("{n:064x}");
        connection.execute("INSERT INTO asset_ids (id) VALUES (?1)", [&id])?;
        connection.execute(
            "INSERT INTO assets (artifact_id, actor, id, content_type, size, digest, created_at)
             SELECT artifact_id, actor, ?1, content_type, size, digest, created_at FROM assets
             WHERE id = ?2",
            params![id, uploaded.id.as_str()],
        )?;
    }
    let before = f.rows()?;
    assert_eq!(f.copy("copy", true), Err(StoreError::AssetLimitExceeded));
    assert_eq!(f.rows()?, before);
    Ok(())
}

#[test]
fn only_the_copying_actors_space_is_copied() -> TestResult {
    let f = Fixture::new()?;
    let own = f.upload(f.id(), "own", 0, b"own bytes")?;
    // Another actor's space of the same artifact, written directly: no
    // command creates one for an artifact that actor does not own.
    let connection = f.sql()?;
    let foreign = format!("{:064x}", 7);
    connection.execute("INSERT INTO asset_ids (id) VALUES (?1)", [&foreign])?;
    connection.execute(
        "INSERT INTO assets (artifact_id, actor, id, content_type, size, digest, created_at)
         SELECT artifact_id, 'other', ?1, content_type, size, digest, created_at FROM assets
         WHERE id = ?2",
        params![foreign, own.id.as_str()],
    )?;
    connection.execute(
        "INSERT INTO asset_spaces (artifact_id, actor, asset_version) VALUES (?1, 'other', 1)",
        [f.id().as_str()],
    )?;

    let copy = f.copy("copy", true)?;

    let actors: Vec<String> = connection
        .prepare(
            "SELECT actor FROM assets WHERE artifact_id = ?1
             UNION ALL SELECT actor FROM asset_spaces WHERE artifact_id = ?1",
        )?
        .query_map([copy.artifact.id.as_str()], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    assert_eq!(actors, ["owner", "owner"]);
    Ok(())
}

#[test]
fn later_changes_to_either_space_leave_the_other_as_it_was() -> TestResult {
    let f = Fixture::new()?;
    let shared = f.upload(f.id(), "shared", 0, b"shared bytes")?;
    let copy = f.copy("copy", true)?;

    f.delete(f.id(), "delete-in-source", 1, &shared)?;
    let added = f.upload(&copy.artifact.id, "copy-only", 1, b"copy only")?;

    let copied = f.store.assets(&f.owner, &copy.artifact.id)?;
    assert_eq!(copied.version, AssetVersion(2));
    assert_eq!(
        f.store
            .asset_content(&f.owner, &copy.artifact.id, &shared.id)?
            .bytes,
        b"shared bytes"
    );
    assert_eq!(
        f.store.asset_content(&f.owner, f.id(), &added.id),
        Err(AssetStoreError::NotFound)
    );
    assert_eq!(f.store.assets(&f.owner, f.id())?.assets, []);
    Ok(())
}

#[test]
fn purging_the_source_keeps_the_copys_assets_and_their_bytes() -> TestResult {
    let f = Fixture::new()?;
    let shared = f.upload(f.id(), "shared", 0, b"shared bytes")?;
    let copy = f.copy("copy", true)?;
    f.purge(&f.source, "source")?;
    assert_eq!(
        f.store
            .asset_content(&f.owner, &copy.artifact.id, &shared.id)?
            .bytes,
        b"shared bytes"
    );
    assert_eq!(f.copy("copy", true)?.delivery, Delivery::Replayed);
    Ok(())
}

#[test]
fn purging_the_copy_keeps_the_sources_assets() -> TestResult {
    let f = Fixture::new()?;
    let shared = f.upload(f.id(), "shared", 0, b"shared bytes")?;
    let copy = f.copy("copy", true)?;
    f.purge(&copy, "copy")?;
    let read = f.store.asset_content(&f.owner, f.id(), &shared.id)?;
    assert_eq!(
        (read.bytes.as_slice(), read.version),
        (b"shared bytes".as_slice(), AssetVersion(1))
    );
    assert_eq!(
        f.store
            .revision_file(f.id(), SourceRevision(1), &parsed("index.html"))?,
        Some(b"<h1>source</h1>".to_vec())
    );
    Ok(())
}
