//! A member's write reads the member's role again in the transaction that
//! commits it (decision 0060 rule 5, slice S02b), before its key (rule 17).
//! These tests call the store directly, as a write that passed the core's
//! check before a revocation or a downgrade reaches it.

mod common;

use artifree_domain::AppDataVersion;
use artifree_domain::access::{AccessVersion, Role};
use artifree_domain::data::{DataChange, DataPath, DataValue};
use artifree_domain::identity::{ActorId, RequestKey, UnixMillis, UserLabel};
use artifree_domain::job::JobKind;
use artifree_domain::package::{FileChange, PackagePath};
use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
use artifree_ports::access_store::{AccessChange, AccessRequest, AccessStore};
use artifree_ports::artifact_store::{
    ArtifactStore, CommitReceipt, ConditionalCommit, CopySource, FileEdit, FileExpectation,
    MetadataUpdate, NewArtifact, PurgeRequest, RequestFingerprint, RequestIdentity, RevisionCommit,
    StoreError,
};
use artifree_ports::asset_store::{AssetStore, AssetStoreError};
use artifree_ports::data_store::{DataStore, DataStoreError, DataWrite};
use artifree_ports::job_store::{JobStore, JobStoreError, NewJob};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use common::{TempDir, TestResult, actor, create_demo, database_in, digest, key, package, parsed};

struct Fixture {
    _dir: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
    editor: ActorId,
    viewer: ActorId,
    stranger: ActorId,
    /// The access version after the two grants.
    access: u64,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let path = database_in(dir.path());
        let store = SqliteStore::open(&path)?;
        let identity = SqliteTokenStore::open(&path)?;
        let owner = actor("local");
        let created = create_demo(&store, &owner, &key("create"), "Shared", "v1")?;
        let mut users = Vec::new();
        for (label, seed) in [("Ed", 1), ("Vi", 2), ("St", 3)] {
            let id = ActorId::user([seed; 16]);
            identity.register(&NewUser {
                id: id.clone(),
                label: parsed::<UserLabel>(label),
                created_at: "2026-11-16T00:00:00Z".to_owned(),
            })?;
            users.push(id);
        }
        let [editor, viewer, stranger] =
            <[ActorId; 3]>::try_from(users).map_err(|_| "three users were registered")?;
        let fixture = Self {
            _dir: dir,
            store,
            owner,
            created,
            editor,
            viewer,
            stranger,
            access: 2,
        };
        fixture.set_role("grant-editor", &fixture.editor, Some(Role::Editor), 0)?;
        fixture.set_role("grant-viewer", &fixture.viewer, Some(Role::Viewer), 1)?;
        Ok(fixture)
    }

    fn set_role(
        &self,
        name: &str,
        user: &ActorId,
        role: Option<Role>,
        base: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let change = role.map_or(AccessChange::Revoke, AccessChange::Grant);
        self.store.change_access(AccessRequest {
            request: RequestIdentity {
                actor: &self.owner,
                key: &key(name),
                fingerprint: RequestFingerprint(digest(&format!("{user}{change:?}"))),
            },
            artifact: &self.created.artifact.id,
            user,
            change,
            base: AccessVersion(base),
            now: UnixMillis(1),
        })?;
        Ok(())
    }

    fn identity<'a>(
        by: &'a ActorId,
        request_key: &'a RequestKey,
        body: &str,
    ) -> RequestIdentity<'a> {
        RequestIdentity {
            actor: by,
            key: request_key,
            fingerprint: RequestFingerprint(digest(body)),
        }
    }

    fn commit(
        &self,
        by: &ActorId,
        name: &str,
        base: u64,
        body: &str,
    ) -> Result<CommitReceipt, StoreError> {
        let request_key = key(name);
        self.store.commit_revision(RevisionCommit {
            request: Self::identity(by, &request_key, body),
            artifact: &self.created.artifact.id,
            base: SourceRevision(base),
            package: &package(&[("index.html", body), ("style.css", "h1{}")]),
        })
    }

    fn edit(&self, by: &ActorId, name: &str) -> Result<CommitReceipt, StoreError> {
        let request_key = key(name);
        self.store.commit_edits(ConditionalCommit {
            request: Self::identity(by, &request_key, "edit"),
            artifact: &self.created.artifact.id,
            edits: &[FileEdit {
                change: FileChange::Put {
                    path: parsed::<PackagePath>("notes.txt"),
                    content: b"n".to_vec(),
                },
                expect: FileExpectation::Absent,
            }],
        })
    }

    fn metadata(
        &self,
        by: &ActorId,
        name: &str,
        base: u64,
        title: Option<&str>,
        deleted: Option<bool>,
    ) -> Result<(), StoreError> {
        let request_key = key(name);
        self.store.update_metadata(MetadataUpdate {
            request: Self::identity(by, &request_key, &format!("{title:?}{deleted:?}")),
            artifact: &self.created.artifact.id,
            base: MetadataRevision(base),
            title: title.map(parsed).as_ref(),
            pinned: None,
            deleted,
        })?;
        Ok(())
    }

    fn purge(&self, by: &ActorId, base: u64) -> Result<(), StoreError> {
        let request_key = key("purge");
        self.store.purge_artifact(PurgeRequest {
            request: Self::identity(by, &request_key, "purge"),
            artifact: &self.created.artifact.id,
            base: MetadataRevision(base),
        })?;
        Ok(())
    }

    fn copy(&self, by: &ActorId, name: &str) -> Result<CommitReceipt, StoreError> {
        let request_key = key(name);
        self.store.create_artifact(NewArtifact {
            request: Self::identity(by, &request_key, "copy"),
            kind: ArtifactKind::App,
            artifact_type: None,
            title: &parsed("Copy"),
            package: &package(&[("index.html", "v1"), ("style.css", "h1{}")]),
            source: Some(CopySource {
                artifact: &self.created.artifact.id,
                copy_data: true,
                copy_assets: false,
            }),
        })
    }

    fn enqueue(&self, by: &ActorId, name: &str) -> Result<(), JobStoreError> {
        let request_key = key(name);
        self.store.enqueue_job(NewJob {
            request: Self::identity(by, &request_key, "job"),
            artifact: &self.created.artifact.id,
            revision: SourceRevision(1),
            kind: JobKind::VerifyRevision,
            now: UnixMillis(1),
        })?;
        Ok(())
    }

    fn write_data(&self, by: &ActorId, name: &str) -> Result<(), DataStoreError> {
        let request_key = key(name);
        let change = DataChange::Set {
            path: parsed::<DataPath>("note"),
            value: DataValue::parse("\"mine\"").map_err(|_| DataStoreError::NotFound)?,
        };
        self.store.write_data(DataWrite {
            fence: None,
            request: Self::identity(by, &request_key, name),
            artifact: &self.created.artifact.id,
            base: AppDataVersion(0),
            change: &change,
            now: UnixMillis(1),
        })?;
        Ok(())
    }

    fn head(&self) -> Result<SourceRevision, Box<dyn std::error::Error>> {
        Ok(self
            .store
            .artifact(&self.created.artifact.id)?
            .ok_or("the artifact is gone")?
            .current_revision)
    }
}

#[test]
fn a_viewer_commits_nothing_and_a_stranger_finds_nothing() -> TestResult {
    let f = Fixture::new()?;
    assert_eq!(
        f.commit(&f.viewer, "viewer-save", 1, "v2"),
        Err(StoreError::Forbidden)
    );
    assert_eq!(f.edit(&f.viewer, "viewer-edit"), Err(StoreError::Forbidden));
    assert_eq!(
        f.metadata(&f.viewer, "viewer-title", 1, Some("Mine"), None),
        Err(StoreError::Forbidden)
    );
    assert_eq!(
        f.commit(&f.stranger, "stranger-save", 1, "v2"),
        Err(StoreError::NotFound)
    );
    assert_eq!(
        f.edit(&f.stranger, "stranger-edit"),
        Err(StoreError::NotFound)
    );
    assert_eq!(
        f.metadata(&f.stranger, "stranger-title", 1, Some("Mine"), None),
        Err(StoreError::NotFound)
    );
    assert_eq!(f.head()?, SourceRevision(1));
    Ok(())
}

#[test]
fn an_editor_commits_and_changes_the_title_but_not_the_trash() -> TestResult {
    let f = Fixture::new()?;
    f.commit(&f.editor, "editor-save", 1, "v2")?;
    f.edit(&f.editor, "editor-edit")?;
    f.metadata(&f.editor, "editor-title", 1, Some("Ours"), None)?;
    assert_eq!(f.head()?, SourceRevision(3));
    assert_eq!(
        f.metadata(&f.editor, "editor-trash", 2, None, Some(true)),
        Err(StoreError::Forbidden)
    );
    // A purge outside the trash is refused for the role before the trash.
    assert_eq!(f.purge(&f.editor, 2), Err(StoreError::Forbidden));
    f.metadata(&f.owner, "owner-trash", 2, None, Some(true))?;
    // In the trash no member reaches the artifact (decision 0060 rule 4).
    assert_eq!(
        f.commit(&f.editor, "editor-late", 3, "v4"),
        Err(StoreError::NotFound)
    );
    assert_eq!(f.purge(&f.editor, 3), Err(StoreError::NotFound));
    Ok(())
}

#[test]
fn a_downgrade_refuses_a_replay_of_an_editor_era_key() -> TestResult {
    let f = Fixture::new()?;
    f.commit(&f.editor, "editor-save", 1, "v2")?;
    f.set_role("downgrade", &f.editor, Some(Role::Viewer), f.access)?;
    assert_eq!(
        f.commit(&f.editor, "editor-save", 1, "v2"),
        Err(StoreError::Forbidden)
    );
    f.set_role("revoke", &f.editor, None, f.access + 1)?;
    assert_eq!(
        f.commit(&f.editor, "editor-save", 1, "v2"),
        Err(StoreError::NotFound)
    );
    Ok(())
}

#[test]
fn a_viewer_enqueues_no_job_and_an_editor_does() -> TestResult {
    let f = Fixture::new()?;
    assert_eq!(
        f.enqueue(&f.viewer, "viewer-job"),
        Err(JobStoreError::Forbidden)
    );
    assert_eq!(
        f.enqueue(&f.stranger, "stranger-job"),
        Err(JobStoreError::NotFound)
    );
    f.enqueue(&f.editor, "editor-job")?;
    f.set_role("downgrade", &f.editor, Some(Role::Viewer), f.access)?;
    assert_eq!(
        f.enqueue(&f.editor, "editor-job"),
        Err(JobStoreError::Forbidden)
    );
    Ok(())
}

#[test]
fn every_member_writes_its_own_data_space_and_copies_while_a_member() -> TestResult {
    let f = Fixture::new()?;
    f.write_data(&f.viewer, "viewer-data")?;
    assert_eq!(
        f.write_data(&f.stranger, "stranger-data"),
        Err(DataStoreError::NotFound)
    );
    f.copy(&f.viewer, "viewer-copy")?;
    assert!(matches!(
        f.copy(&f.stranger, "stranger-copy"),
        Err(StoreError::NotFound)
    ));
    f.set_role("revoke", &f.viewer, None, f.access)?;
    assert_eq!(
        f.write_data(&f.viewer, "viewer-late"),
        Err(DataStoreError::NotFound)
    );
    assert!(matches!(
        f.copy(&f.viewer, "viewer-late-copy"),
        Err(StoreError::NotFound)
    ));
    Ok(())
}

#[test]
fn a_member_reaches_its_own_asset_space_while_a_member() -> TestResult {
    let f = Fixture::new()?;
    let id = &f.created.artifact.id;
    assert!(f.store.assets(&f.viewer, id)?.assets.is_empty());
    assert!(matches!(
        f.store.assets(&f.stranger, id),
        Err(AssetStoreError::NotFound)
    ));
    f.set_role("revoke", &f.viewer, None, f.access)?;
    assert!(matches!(
        f.store.assets(&f.viewer, id),
        Err(AssetStoreError::NotFound)
    ));
    Ok(())
}
