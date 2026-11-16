//! The declared shared asset space of an artifact (decision 0060 rule 10,
//! slice S02f2): reached only while the head revision's runtime manifest
//! declares it, written by the role the declaration names, read by every
//! reader, charged to the uploader, and changed in every reader's feed (rule
//! 13). These tests call the store directly, as a write that passed the
//! core's check reaches it.

mod common;

use artifree_domain::access::{AccessVersion, Role, Space};
use artifree_domain::assets::{
    AssetId, AssetMediaType, AssetRejection, AssetVersion, MAX_ACTOR_ASSETS, MAX_SPACE_ASSETS,
};
use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, UnixMillis, UserLabel};
use artifree_domain::{MetadataRevision, SourceRevision};
use artifree_ports::access_store::{AccessChange, AccessRequest, AccessStore};
use artifree_ports::artifact_store::{
    ArtifactStore, ChangeKind, CommitReceipt, Delivery, MetadataUpdate, PurgeRequest,
    RequestFingerprint, RequestIdentity, RevisionCommit,
};
use artifree_ports::asset_store::{AssetReceipt, AssetStore, AssetStoreError, AssetWrite};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::storage::{SCHEMA_VERSION, SqliteStore};
use artifree_providers::tokens::SqliteTokenStore;
use common::{TempDir, TestResult, actor, create_demo, database_in, digest, key, package, parsed};
use rusqlite::{Connection, params};

struct Fixture {
    dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
    editor: ActorId,
    viewer: ActorId,
    stranger: ActorId,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let database = database_in(dir.path());
        let store = SqliteStore::open(&database)?;
        let owner = actor("local");
        let created = create_demo(&store, &owner, &key("create"), "Shared", "v1")?;
        let editor = ActorId::user([1; 16]);
        let viewer = ActorId::user([2; 16]);
        let identity = SqliteTokenStore::open(&database)?;
        for (base, user, role, label) in [
            (0, &editor, Role::Editor, "Ed"),
            (1, &viewer, Role::Viewer, "Vi"),
        ] {
            identity.register(&NewUser {
                id: user.clone(),
                label: parsed::<UserLabel>(label),
                created_at: "2026-11-17T00:00:00Z".to_owned(),
            })?;
            store.change_access(AccessRequest {
                request: RequestIdentity {
                    actor: &owner,
                    key: &key(label),
                    fingerprint: RequestFingerprint(digest(label)),
                },
                artifact: &created.artifact.id,
                user,
                change: AccessChange::Grant(role),
                base: AccessVersion(base),
                now: UnixMillis(1),
            })?;
        }
        Ok(Self {
            dir,
            store,
            owner,
            created,
            editor,
            viewer,
            stranger: ActorId::user([3; 16]),
        })
    }

    fn id(&self) -> &ArtifactId {
        &self.created.artifact.id
    }

    /// Commits revision `base + 1` with `manifest` as its runtime manifest,
    /// or with none.
    fn head(&self, base: u64, manifest: Option<&str>) -> TestResult {
        let mut files = vec![("index.html", "<p>shared</p>")];
        if let Some(manifest) = manifest {
            files.push(("artifree-runtime.json", manifest));
        }
        let name = format!("head-{base}");
        self.store.commit_revision(RevisionCommit {
            request: RequestIdentity {
                actor: &self.owner,
                key: &key(&name),
                fingerprint: RequestFingerprint(digest(&name)),
            },
            artifact: self.id(),
            base: SourceRevision(base),
            package: &package(&files),
        })?;
        Ok(())
    }

    fn write<'a>(
        &'a self,
        by: &'a ActorId,
        (name, request_key): (&str, &'a RequestKey),
        space: Space,
        base: u64,
    ) -> AssetWrite<'a> {
        AssetWrite {
            request: RequestIdentity {
                actor: by,
                key: request_key,
                fingerprint: RequestFingerprint(digest(name)),
            },
            artifact: self.id(),
            space,
            base: AssetVersion(base),
            expected_source: None,
            now: UnixMillis(10),
        }
    }

    fn upload(
        &self,
        by: &ActorId,
        name: &str,
        space: Space,
        base: u64,
    ) -> Result<AssetReceipt, AssetStoreError> {
        let media: AssetMediaType = parsed("text/plain");
        let request_key = key(name);
        self.store.upload_asset(
            self.write(by, (name, &request_key), space, base),
            &media,
            name.as_bytes(),
            Ok(()),
        )
    }

    fn delete(
        &self,
        by: &ActorId,
        name: &str,
        (space, base): (Space, u64),
        id: &AssetId,
    ) -> Result<AssetReceipt, AssetStoreError> {
        let request_key = key(name);
        self.store
            .delete_asset(self.write(by, (name, &request_key), space, base), id)
    }

    /// The version and asset ids of a space as `by` lists it.
    fn list(&self, by: &ActorId, space: Space) -> Result<(u64, Vec<AssetId>), AssetStoreError> {
        let snapshot = self.store.assets(by, self.id(), space)?;
        Ok((
            snapshot.version.0,
            snapshot.assets.into_iter().map(|asset| asset.id).collect(),
        ))
    }

    /// The kinds of the rows of `user`'s feed.
    fn feed(&self, user: &ActorId) -> Result<Vec<ChangeKind>, Box<dyn std::error::Error>> {
        Ok(self
            .store
            .changes(user, ChangeSequence(0), 100)?
            .changes
            .into_iter()
            .map(|change| change.kind)
            .collect())
    }

    /// Grants, or with `None` revokes, `user`'s role on `artifact`.
    fn access(
        &self,
        artifact: &ArtifactId,
        user: &ActorId,
        change: Option<Role>,
        base: u64,
    ) -> TestResult {
        let name = format!("access-{artifact}-{user}-{base}");
        self.store.change_access(AccessRequest {
            request: RequestIdentity {
                actor: &self.owner,
                key: &key(&name),
                fingerprint: RequestFingerprint(digest(&name)),
            },
            artifact,
            user,
            change: change.map_or(AccessChange::Revoke, AccessChange::Grant),
            base: AccessVersion(base),
            now: UnixMillis(2),
        })?;
        Ok(())
    }

    fn sql(&self) -> rusqlite::Result<Connection> {
        Connection::open(database_in(self.dir.path()))
    }

    /// Stores `count` one-byte associations in `artifact`'s space held by
    /// `holder`, charged to `charged_to`, as earlier uploads would have.
    fn fill(
        &self,
        (artifact, holder): (&ArtifactId, &str),
        charged_to: Option<&ActorId>,
        count: u64,
    ) -> TestResult {
        let sql = self.sql()?;
        let blob = digest("filler");
        sql.execute(
            "INSERT OR IGNORE INTO blobs (digest, content) VALUES (?1, x'00')",
            [blob.as_slice()],
        )?;
        // Ids follow the ones already reserved, so a later fill adds new ones.
        let reserved: i64 =
            sql.query_row("SELECT count(*) FROM asset_ids", [], |row| row.get(0))?;
        for n in reserved..reserved + i64::try_from(count)? {
            let id = format!("{n:064x}");
            sql.execute("INSERT OR IGNORE INTO asset_ids (id) VALUES (?1)", [&id])?;
            sql.execute(
                "INSERT INTO assets (artifact_id, actor, id, content_type, size, digest,
                     created_at, charged_to)
                 VALUES (?1, ?2, ?3, 'text/plain', 1, ?4, 1, ?5)",
                params![
                    artifact.as_str(),
                    holder,
                    id,
                    blob.as_slice(),
                    charged_to.map(ActorId::as_str)
                ],
            )?;
        }
        Ok(())
    }
}

const EDITORS: &str = r#"{"sdk":"artifree.app.v1","shared":"editors"}"#;
const MEMBERS: &str = r#"{"sdk":"artifree.app.v1","shared":"members"}"#;
const SHARED: &str = "*shared";

#[test]
fn the_shared_asset_space_is_reached_only_while_the_head_declares_it() -> TestResult {
    let f = Fixture::new()?;
    let missing = Err(AssetStoreError::NotFound);
    // Revision 1 has no manifest.
    assert_eq!(f.list(&f.owner, Space::Shared), missing);
    assert_eq!(
        f.upload(&f.owner, "before", Space::Shared, 0).map(|_| ()),
        Err(AssetStoreError::NotFound)
    );
    f.head(1, Some(EDITORS))?;
    let shared = f.upload(&f.owner, "declared", Space::Shared, 0)?;
    assert_eq!(shared.version, AssetVersion(1));
    // Every reader reads it, its metadata and its bytes.
    assert_eq!(
        f.list(&f.viewer, Space::Shared),
        Ok((1, vec![shared.id.clone()]))
    );
    let content = f
        .store
        .asset_content(&f.viewer, f.id(), Space::Shared, &shared.id)?;
    assert_eq!(content.bytes, b"declared");
    // Each user's own space is apart from it, the uploader's too.
    assert_eq!(f.list(&f.owner, Space::Own), Ok((0, vec![])));
    assert_eq!(
        f.store
            .asset_content(&f.owner, f.id(), Space::Own, &shared.id)
            .map(|_| ()),
        Err(AssetStoreError::NotFound)
    );
    // Removing the declaration makes it unreachable and keeps it.
    f.head(2, None)?;
    assert_eq!(f.list(&f.viewer, Space::Shared), missing);
    f.head(3, Some(MEMBERS))?;
    assert_eq!(f.list(&f.viewer, Space::Shared)?.1, [shared.id]);
    Ok(())
}

#[test]
fn the_declaration_names_who_writes_and_access_comes_before_replay() -> TestResult {
    let f = Fixture::new()?;
    f.head(1, Some(EDITORS))?;
    assert_eq!(
        f.store.admit_asset_write(&f.editor, f.id(), Space::Shared),
        Ok(())
    );
    for (who, refused) in [
        (&f.viewer, AssetStoreError::Forbidden),
        (&f.stranger, AssetStoreError::NotFound),
    ] {
        assert_eq!(
            f.store.admit_asset_write(who, f.id(), Space::Shared),
            Err(refused.clone())
        );
        assert_eq!(
            f.upload(who, "refused", Space::Shared, 0).map(|_| ()),
            Err(refused)
        );
    }
    // Under "members" the viewer writes, with the key it was refused under,
    // and deletes what another member uploaded.
    f.head(2, Some(MEMBERS))?;
    let by_editor = f.upload(&f.editor, "by-editor", Space::Shared, 0)?;
    let by_viewer = f.upload(&f.viewer, "refused", Space::Shared, 1)?;
    assert!(
        f.delete(&f.viewer, "rm-editor", (Space::Shared, 2), &by_editor.id)?
            .deleted
    );
    // The head is read before the key: declaring "editors" again refuses the
    // viewer's retry of a write that committed, rather than replaying it.
    assert_eq!(
        f.upload(&f.viewer, "refused", Space::Shared, 1)
            .map(|receipt| receipt.delivery),
        Ok(Delivery::Replayed),
        "the retry replays while the viewer may still write"
    );
    f.head(3, Some(EDITORS))?;
    assert_eq!(
        f.upload(&f.viewer, "refused", Space::Shared, 1).map(|_| ()),
        Err(AssetStoreError::Forbidden)
    );
    assert_eq!(
        f.delete(&f.viewer, "rm-viewer", (Space::Shared, 3), &by_viewer.id)
            .map(|_| ()),
        Err(AssetStoreError::Forbidden)
    );
    Ok(())
}

#[test]
fn a_shared_upload_is_charged_to_its_uploader() -> TestResult {
    let f = Fixture::new()?;
    f.head(1, Some(EDITORS))?;
    let other = create_demo(&f.store, &f.owner, &key("other"), "Other", "v1")?;
    f.access(&other.artifact.id, &f.editor, Some(Role::Editor), 0)?;
    // Shared uploads to another artifact the editor is a member of already
    // charge it one short of its limit; they are not in this artifact's
    // shared space.
    f.fill(
        (&other.artifact.id, SHARED),
        Some(&f.editor),
        MAX_ACTOR_ASSETS - 1,
    )?;
    f.upload(&f.editor, "last-fits", Space::Shared, 0)?;
    // The editor's own space is charged the same limit, so it is full.
    let full = Err(AssetStoreError::Rejected(AssetRejection::LimitExceeded));
    assert_eq!(
        f.upload(&f.editor, "own-full", Space::Own, 0).map(|_| ()),
        full
    );
    assert_eq!(
        f.upload(&f.editor, "shared-full", Space::Shared, 1)
            .map(|_| ()),
        full
    );
    // Nothing of it is charged to the owner, who uploads to the same space.
    f.upload(&f.owner, "owner-fits", Space::Shared, 1)?;
    // Revoked from the other artifact, the editor can no longer delete what
    // it uploaded there, and those uploads stop counting against it.
    f.access(&other.artifact.id, &f.editor, None, 1)?;
    f.upload(&f.editor, "own-after-revoke", Space::Own, 0)?;
    Ok(())
}

#[test]
fn the_owner_is_charged_for_its_shared_uploads_and_the_schema_ties_charge_to_holder() -> TestResult
{
    let f = Fixture::new()?;
    let other = create_demo(&f.store, &f.owner, &key("other"), "Other", "v1")?;
    f.fill(
        (&other.artifact.id, SHARED),
        Some(&f.owner),
        MAX_ACTOR_ASSETS,
    )?;
    assert_eq!(
        f.upload(&f.owner, "owner-full", Space::Own, 0).map(|_| ()),
        Err(AssetStoreError::Rejected(AssetRejection::LimitExceeded))
    );
    // A shared row names who it is charged to, and an own row names no one.
    for (holder, charged_to) in [(SHARED, None), (f.editor.as_str(), Some(&f.editor))] {
        assert!(
            f.fill((&other.artifact.id, holder), charged_to, 1).is_err(),
            "{holder} {charged_to:?}"
        );
    }
    Ok(())
}

#[test]
fn the_shared_space_has_its_own_limit() -> TestResult {
    let f = Fixture::new()?;
    f.head(1, Some(MEMBERS))?;
    f.fill((f.id(), SHARED), Some(&f.owner), MAX_SPACE_ASSETS)?;
    assert_eq!(
        f.upload(&f.viewer, "space-full", Space::Shared, 0)
            .map(|_| ()),
        Err(AssetStoreError::Rejected(AssetRejection::LimitExceeded))
    );
    // The viewer's own space in the artifact is a different space.
    f.upload(&f.viewer, "own-fits", Space::Own, 0)?;
    Ok(())
}

#[test]
fn a_shared_change_reaches_every_reader_and_an_own_change_no_one() -> TestResult {
    let f = Fixture::new()?;
    f.head(1, Some(MEMBERS))?;
    let users = [&f.owner, &f.editor, &f.viewer];
    let mut counts = Vec::new();
    for user in users {
        counts.push(f.feed(user)?.len());
    }
    let shared = f.upload(&f.viewer, "shared", Space::Shared, 0)?;
    let own = f.upload(&f.viewer, "own", Space::Own, 0)?;
    f.delete(&f.viewer, "rm-own", (Space::Own, 1), &own.id)?;
    f.delete(&f.editor, "rm-shared", (Space::Shared, 1), &shared.id)?;
    // Deleting what is gone changes nothing and adds no row.
    f.delete(&f.editor, "rm-again", (Space::Shared, 2), &shared.id)?;
    for (user, count) in users.into_iter().zip(counts) {
        let feed = f.feed(user)?;
        assert_eq!(
            feed.get(count..),
            Some([ChangeKind::SharedAssetsChanged; 2].as_slice()),
            "{user}"
        );
    }
    assert_eq!(f.feed(&f.stranger)?, []);
    Ok(())
}

#[test]
fn the_upgrade_to_schema_21_keeps_every_change_and_takes_shared_assets() -> TestResult {
    let f = Fixture::new()?;
    f.head(1, Some(EDITORS))?;
    f.upload(&f.owner, "own-before", Space::Own, 0)?;
    let before = f.feed(&f.owner)?;
    let path = database_in(f.dir.path());
    let old_kinds = "'created', 'revised', 'metadata_updated', 'deleted', 'recovered',
        'purged', 'data_changed', 'job_changed', 'access_granted', 'access_changed',
        'access_revoked', 'shared_data_changed'";
    f.sql()?.execute_batch(&format!(
        "PRAGMA foreign_keys = OFF;
        CREATE TABLE changes_v20 (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            actor TEXT NOT NULL,
            actor_seq INTEGER NOT NULL CHECK (actor_seq >= 1),
            artifact_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ({old_kinds})),
            revision INTEGER NOT NULL,
            metadata_revision INTEGER NOT NULL,
            data_version INTEGER,
            job_id TEXT,
            committed_at INTEGER NOT NULL,
            UNIQUE (actor, actor_seq),
            CHECK ((kind IN ('data_changed', 'shared_data_changed'))
                = (data_version IS NOT NULL)),
            CHECK ((kind = 'job_changed') = (job_id IS NOT NULL))
        ) STRICT;
        INSERT INTO changes_v20 SELECT sequence, actor, actor_seq, artifact_id, kind, revision,
            metadata_revision, data_version, job_id, committed_at FROM changes;
        DROP TABLE changes;
        ALTER TABLE changes_v20 RENAME TO changes;
        DROP INDEX assets_by_charge;
        ALTER TABLE assets DROP COLUMN charged_to;
        PRAGMA user_version = 20;"
    ))?;
    let upgraded = Fixture {
        store: SqliteStore::open(&path)?,
        ..f
    };
    assert_eq!(common::user_version(&path)?, SCHEMA_VERSION);
    assert_eq!(upgraded.feed(&upgraded.owner)?, before);
    assert_eq!(upgraded.list(&upgraded.owner, Space::Own)?.1.len(), 1);
    upgraded.upload(&upgraded.editor, "shared-after", Space::Shared, 0)?;
    assert_eq!(
        upgraded.feed(&upgraded.owner)?.last(),
        Some(&ChangeKind::SharedAssetsChanged)
    );
    Ok(())
}

#[test]
fn a_purge_erases_the_shared_asset_space() -> TestResult {
    let f = Fixture::new()?;
    f.head(1, Some(EDITORS))?;
    f.upload(&f.editor, "shared", Space::Shared, 0)?;
    let rows = || -> rusqlite::Result<(i64, i64)> {
        f.sql()?.query_row(
            "SELECT (SELECT count(*) FROM assets WHERE actor = ?1),
                (SELECT count(*) FROM asset_spaces WHERE actor = ?1)",
            [SHARED],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    };
    assert_eq!(rows()?, (1, 1));
    f.store.update_metadata(MetadataUpdate {
        request: RequestIdentity {
            actor: &f.owner,
            key: &key("trash"),
            fingerprint: RequestFingerprint(digest("trash")),
        },
        artifact: f.id(),
        base: MetadataRevision(1),
        title: None,
        pinned: None,
        deleted: Some(true),
    })?;
    f.store.purge_artifact(PurgeRequest {
        request: RequestIdentity {
            actor: &f.owner,
            key: &key("purge"),
            fingerprint: RequestFingerprint(digest("purge")),
        },
        artifact: f.id(),
        base: MetadataRevision(2),
    })?;
    assert_eq!(rows()?, (0, 0));
    Ok(())
}
