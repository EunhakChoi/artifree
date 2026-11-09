//! Runtime asset writes are fenced to the source head their binding pinned
//! (decision 0045 rule 12), exercised through `ArtifactService` on the real
//! store (`SqliteStore`).
//!
//! This target lives in artifree-server because the layer rules in
//! `scripts/lib/boundaries.mjs` forbid artifree-providers from depending on
//! artifree-core, even as a dev-dependency. The server is the only crate that
//! already reaches both the service and the store.

use artifree_core::artifacts::ArtifactService;
use artifree_core::assets::{AssetDeletion, AssetUpload};
use artifree_domain::SourceRevision;
use artifree_domain::assets::{AssetId, AssetMediaType, AssetVersion};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title, UnixMillis};
use artifree_domain::package::{FileChange, FilePackage, PackagePath};
use artifree_ports::artifact_store::Delivery;
use artifree_ports::asset_store::{AssetReceipt, AssetSnapshot, AssetStoreError};
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use rusqlite::Connection;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A unique directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "artifree-r09d2-fence-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|error| panic!("cannot create {}: {error}", path.display()));
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn parsed<T: FromStr>(value: &str) -> T
where
    T::Err: Display,
{
    value
        .parse()
        .unwrap_or_else(|error| panic!("invalid fixture {value:?}: {error}"))
}

fn actor(name: &str) -> ActorId {
    parsed(name)
}

fn key(name: &str) -> RequestKey {
    parsed(&format!("test-key-{name}"))
}

fn media() -> AssetMediaType {
    parsed("text/plain")
}

fn package(body: &str) -> FilePackage {
    FilePackage::new(vec![(
        parsed::<PackagePath>("index.html"),
        body.as_bytes().to_vec(),
    )])
    .unwrap_or_else(|error| panic!("invalid fixture package: {error}"))
}

fn database_in(directory: &Path) -> PathBuf {
    directory.join(DATABASE_FILE)
}

fn upload_input(content_type: &AssetMediaType, base: u64) -> AssetUpload<'_> {
    AssetUpload {
        base: AssetVersion(base),
        content_type,
        bytes: b"private bytes",
        now: UnixMillis(10),
    }
}

/// One artifact at source revision 1, served by a service over the real store.
struct Fixture {
    dir: TempDir,
    service: ArtifactService<SqliteStore>,
    owner: ActorId,
    artifact: ArtifactId,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let service = ArtifactService::new(SqliteStore::open(&database_in(dir.path()))?);
        let owner = actor("owner");
        let created = service.create(
            &owner,
            &key("create"),
            &parsed::<Title>("Assets"),
            &package("<h1>source</h1>"),
        )?;
        assert_eq!(
            created.revision.revision,
            SourceRevision(1),
            "a new artifact starts at source revision 1"
        );
        Ok(Self {
            dir,
            service,
            owner,
            artifact: created.artifact.id,
        })
    }

    /// Moves the source head from revision 1 to revision 2 through the service.
    fn move_source_head(&self) -> Result<(), Box<dyn std::error::Error>> {
        let change = FileChange::Put {
            path: parsed("index.html"),
            content: b"<h1>moved</h1>".to_vec(),
        };
        let receipt = self.service.save(
            &self.owner,
            &key("move-head"),
            &self.artifact,
            SourceRevision(1),
            vec![change],
        )?;
        assert_eq!(
            receipt.revision.revision,
            SourceRevision(2),
            "the save must move the source head to revision 2"
        );
        Ok(())
    }

    fn upload(
        &self,
        name: &str,
        base: u64,
        expected: SourceRevision,
    ) -> Result<AssetReceipt, AssetStoreError> {
        let content_type = media();
        self.service.runtime_upload_asset(
            &self.owner,
            &key(name),
            &self.artifact,
            upload_input(&content_type, base),
            expected,
        )
    }

    fn plain_upload(&self, name: &str, base: u64) -> Result<AssetReceipt, AssetStoreError> {
        let content_type = media();
        self.service.upload_asset(
            &self.owner,
            &key(name),
            &self.artifact,
            upload_input(&content_type, base),
        )
    }

    fn delete(
        &self,
        name: &str,
        base: u64,
        id: &AssetId,
        expected: SourceRevision,
    ) -> Result<AssetReceipt, AssetStoreError> {
        self.service.runtime_delete_asset(
            &self.owner,
            &key(name),
            &self.artifact,
            AssetDeletion {
                base: AssetVersion(base),
                id,
                now: UnixMillis(20),
            },
            expected,
        )
    }

    fn snapshot(&self) -> Result<AssetSnapshot, AssetStoreError> {
        self.service.assets(&self.owner, &self.artifact)
    }

    /// Rows of assets, asset spaces, asset receipts and blobs, read from disk.
    fn counts(&self) -> rusqlite::Result<(i64, i64, i64, i64)> {
        let connection = Connection::open(database_in(self.dir.path()))?;
        connection.query_row(
            "SELECT (SELECT count(*) FROM assets), (SELECT count(*) FROM asset_spaces),
                    (SELECT count(*) FROM asset_requests), (SELECT count(*) FROM blobs)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
    }
}

#[test]
fn stale_upload_head_is_refused_without_writing_or_reserving_the_key() -> TestResult {
    let f = Fixture::new()?;
    f.move_source_head()?;
    let before = f.counts()?;
    assert_eq!(
        before,
        (0, 0, 0, 2),
        "creation and the new revision each stored one blob"
    );
    assert_eq!(
        f.upload("stale", 0, SourceRevision(1)),
        Err(AssetStoreError::SourceConflict {
            current: SourceRevision(2)
        }),
        "a head fenced at revision 1 must be refused once the source is at revision 2"
    );
    assert_eq!(
        f.counts()?,
        before,
        "a refused upload writes no association, space, receipt or blob"
    );
    assert_eq!(f.snapshot()?.version, AssetVersion(0));

    // The refusal reserved nothing: the same request under the current head commits.
    let committed = f.upload("stale", 0, SourceRevision(2))?;
    assert_eq!(committed.delivery, Delivery::Committed);
    assert_eq!(committed.version, AssetVersion(1));
    assert_eq!(f.counts()?, (1, 1, 1, 3));
    Ok(())
}

#[test]
fn stale_delete_head_is_refused_without_changing_the_association() -> TestResult {
    let f = Fixture::new()?;
    let uploaded = f.upload("upload", 0, SourceRevision(1))?;
    f.move_source_head()?;
    let before = f.counts()?;
    assert_eq!(before, (1, 1, 1, 3));
    assert_eq!(
        f.delete("remove", 1, &uploaded.id, SourceRevision(1)),
        Err(AssetStoreError::SourceConflict {
            current: SourceRevision(2)
        }),
        "a delete fenced at revision 1 must be refused once the source is at revision 2"
    );
    assert_eq!(
        f.counts()?,
        before,
        "a refused delete writes no receipt and removes no blob"
    );
    let snapshot = f.snapshot()?;
    assert_eq!(
        (snapshot.version, snapshot.assets.len()),
        (AssetVersion(1), 1),
        "the association and its version survive a refused delete"
    );
    let deleted = f.delete("remove", 1, &uploaded.id, SourceRevision(2))?;
    assert_eq!(
        (deleted.deleted, deleted.version, deleted.delivery),
        (true, AssetVersion(2), Delivery::Committed),
        "the same request under the current head deletes the association"
    );
    Ok(())
}

#[test]
fn stale_upload_head_beats_a_matching_replay() -> TestResult {
    let f = Fixture::new()?;
    let first = f.upload("retry", 0, SourceRevision(1))?;
    f.move_source_head()?;
    // Control: under the current head the same request replays the first receipt.
    let replay = f.upload("retry", 0, SourceRevision(2))?;
    assert_eq!(
        (replay.delivery, replay.id, replay.version),
        (Delivery::Replayed, first.id, AssetVersion(1)),
        "a matching key must replay the committed receipt under the current head"
    );
    // The fence runs before replay, so a stale head refuses the same key.
    assert_eq!(
        f.upload("retry", 0, SourceRevision(1)),
        Err(AssetStoreError::SourceConflict {
            current: SourceRevision(2)
        }),
        "a stale head must win over a matching key"
    );
    Ok(())
}

#[test]
fn stale_delete_head_beats_a_matching_replay() -> TestResult {
    let f = Fixture::new()?;
    let uploaded = f.upload("upload", 0, SourceRevision(1))?;
    let removed = f.delete("remove", 1, &uploaded.id, SourceRevision(1))?;
    assert_eq!(
        (removed.delivery, removed.deleted, removed.version),
        (Delivery::Committed, true, AssetVersion(2)),
    );
    f.move_source_head()?;
    // Control: under the current head the same delete replays its receipt.
    let replay = f.delete("remove", 1, &uploaded.id, SourceRevision(2))?;
    assert_eq!(
        (replay.delivery, replay.deleted, replay.version),
        (Delivery::Replayed, true, AssetVersion(2)),
        "a matching delete key must replay its receipt under the current head"
    );
    assert_eq!(
        f.delete("remove", 1, &uploaded.id, SourceRevision(1)),
        Err(AssetStoreError::SourceConflict {
            current: SourceRevision(2)
        }),
        "a stale head must win over a matching delete key"
    );
    Ok(())
}

#[test]
fn plain_upload_ignores_the_source_head() -> TestResult {
    let f = Fixture::new()?;
    f.move_source_head()?;
    let receipt = f.plain_upload("plain", 0)?;
    assert_eq!(
        (receipt.delivery, receipt.version),
        (Delivery::Committed, AssetVersion(1)),
        "an upload that holds no frame must not be fenced by the source head"
    );
    Ok(())
}
