//! What each member role may do with an artifact, enforced by the application
//! core over real storage (decision 0060 rules 3 and 4, slice S02b). Members
//! are granted through the store; the access routes are a later slice.
use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;

use artifree_core::artifacts::{
    ArtifactError, ArtifactService, CopyParts, MetadataChange, Missing,
};
use artifree_domain::access::{AccessVersion, Role};
use artifree_domain::data::{DataChange, DataPath, DataValue};
use artifree_domain::identity::{ActorId, ArtifactId, Title, UnixMillis, UserLabel};
use artifree_domain::job::JobKind;
use artifree_domain::package::{FileChange, FilePackage, PackagePath};
use artifree_domain::{AppDataVersion, MetadataRevision, SourceRevision};
use artifree_ports::access_store::{AccessChange, AccessRequest, AccessStore};
use artifree_ports::artifact_store::{
    ArtifactStore, FileEdit, FileExpectation, RequestFingerprint, RequestIdentity,
};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;

type TestError = Box<dyn std::error::Error>;
type TestResult = Result<(), TestError>;

const HIDDEN: ArtifactError = ArtifactError::NotFound(Missing::Artifact);

fn parsed<T: FromStr>(value: &str) -> T
where
    T::Err: Display,
{
    value
        .parse()
        .unwrap_or_else(|error| panic!("invalid fixture {value:?}: {error}"))
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    _directory: TempDir,
    service: ArtifactService<SqliteStore>,
    /// The same database, for granting.
    access: SqliteStore,
    owner: ActorId,
    editor: ActorId,
    viewer: ActorId,
    stranger: ActorId,
    id: ArtifactId,
    /// The access version after the grants.
    version: u64,
}

impl Fixture {
    fn new(name: &str) -> Result<Self, TestError> {
        let directory = TempDir(
            std::env::temp_dir().join(format!("artifree-roles-{name}-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&directory.0);
        std::fs::create_dir_all(&directory.0)?;
        let path = directory.0.join("db.sqlite3");
        let service = ArtifactService::new(SqliteStore::open(&path)?);
        let access = SqliteStore::open(&path)?;
        let identity = SqliteTokenStore::open(&path)?;
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
        let owner: ActorId = parsed("owner");
        let package = FilePackage::new(vec![(PackagePath::entry(), b"v1".to_vec())])?;
        let id = service
            .create(
                &owner,
                &parsed("create-key"),
                &parsed::<Title>("Shared"),
                &package,
            )?
            .artifact
            .id;
        let fixture = Self {
            _directory: directory,
            service,
            access,
            owner,
            editor,
            viewer,
            stranger,
            id,
            version: 2,
        };
        fixture.set_role("grant-editor", &fixture.editor, Some(Role::Editor), 0)?;
        fixture.set_role("grant-viewer", &fixture.viewer, Some(Role::Viewer), 1)?;
        Ok(fixture)
    }

    fn set_role(&self, name: &str, user: &ActorId, role: Option<Role>, base: u64) -> TestResult {
        let change = role.map_or(AccessChange::Revoke, AccessChange::Grant);
        self.access.change_access(AccessRequest {
            request: RequestIdentity {
                actor: &self.owner,
                key: &parsed(name),
                fingerprint: RequestFingerprint([base.to_le_bytes()[0]; 32]),
            },
            artifact: &self.id,
            user,
            change,
            base: AccessVersion(base),
            now: UnixMillis(1),
        })?;
        Ok(())
    }

    fn save(&self, by: &ActorId, key: &str, base: u64) -> Result<(), ArtifactError> {
        self.service
            .save(
                by,
                &parsed(key),
                &self.id,
                SourceRevision(base),
                vec![FileChange::Put {
                    path: PackagePath::entry(),
                    content: key.as_bytes().to_vec(),
                }],
            )
            .map(|_| ())
    }

    fn edit(&self, by: &ActorId, key: &str) -> Result<(), ArtifactError> {
        let edits = [FileEdit {
            change: FileChange::Put {
                path: parsed("notes.txt"),
                content: key.as_bytes().to_vec(),
            },
            expect: FileExpectation::Absent,
        }];
        self.service
            .edit_files(by, &parsed(key), &self.id, self.head()?, &edits)
            .map(|_| ())
    }

    fn restore(&self, by: &ActorId, key: &str) -> Result<(), ArtifactError> {
        self.service
            .restore(by, &parsed(key), &self.id, self.head()?, SourceRevision(1))
            .map(|_| ())
    }

    fn metadata(
        &self,
        by: &ActorId,
        key: &str,
        title: Option<&str>,
        deleted: Option<bool>,
    ) -> Result<(), ArtifactError> {
        let change = MetadataChange::new(title.map(parsed), None, deleted)
            .ok_or_else(|| ArtifactError::Unavailable("a change with fields".into()))?;
        let base = self.metadata_revision()?;
        self.service
            .update(by, &parsed(key), &self.id, base, &change)
            .map(|_| ())
    }

    fn purge(&self, by: &ActorId, key: &str) -> Result<(), ArtifactError> {
        let base = self.metadata_revision()?;
        self.service
            .purge(by, &parsed(key), &self.id, base)
            .map(|_| ())
    }

    fn enqueue(&self, by: &ActorId, key: &str) -> Result<(), ArtifactError> {
        self.service
            .enqueue_job(
                by,
                &parsed(key),
                &self.id,
                SourceRevision(1),
                JobKind::VerifyRevision,
                UnixMillis(1),
            )
            .map(|_| ())
    }

    fn duplicate(&self, by: &ActorId, key: &str) -> Result<ActorId, ArtifactError> {
        let parts = CopyParts {
            data: false,
            assets: false,
        };
        self.service
            .duplicate(by, &parsed(key), &self.id, SourceRevision(1), None, parts)
            .map(|receipt| receipt.artifact.owner)
    }

    fn write_data(&self, by: &ActorId, key: &str) -> Result<(), ArtifactError> {
        let change = DataChange::Set {
            path: parsed::<DataPath>("note"),
            value: DataValue::parse("\"mine\"")
                .map_err(|_| ArtifactError::Unavailable("a JSON value".into()))?,
        };
        self.service
            .write_data(
                by,
                &parsed(key),
                &self.id,
                AppDataVersion(0),
                &change,
                UnixMillis(1),
            )
            .map(|_| ())
    }

    fn head(&self) -> Result<SourceRevision, ArtifactError> {
        Ok(self
            .service
            .artifact(&self.owner, &self.id)?
            .current_revision)
    }

    fn metadata_revision(&self) -> Result<MetadataRevision, ArtifactError> {
        // Read from the store, which finds it in the trash too.
        Ok(self
            .access
            .artifact(&self.id)?
            .ok_or(HIDDEN)?
            .metadata_revision)
    }
}

#[test]
fn a_viewer_reads_and_copies_but_changes_nothing() -> TestResult {
    let f = Fixture::new("viewer")?;
    let viewer = &f.viewer;
    assert_eq!(f.service.artifact(viewer, &f.id)?.owner, f.owner);
    assert_eq!(f.service.revisions(viewer, &f.id)?.len(), 1);
    assert_eq!(
        f.duplicate(viewer, "viewer-copy")?,
        *viewer,
        "the copy is the viewer's"
    );
    f.write_data(viewer, "viewer-data")?;
    let refused = Err(ArtifactError::Forbidden);
    assert_eq!(f.save(viewer, "viewer-save", 1), refused);
    assert_eq!(f.edit(viewer, "viewer-edit"), refused);
    assert_eq!(f.restore(viewer, "viewer-restore"), refused);
    assert_eq!(
        f.metadata(viewer, "viewer-title", Some("Mine"), None),
        refused
    );
    assert_eq!(
        f.metadata(viewer, "viewer-pin-trash", None, Some(true)),
        refused
    );
    assert_eq!(f.enqueue(viewer, "viewer-job"), refused);
    assert_eq!(f.purge(viewer, "viewer-purge"), refused);
    let record = f.service.artifact(&f.owner, &f.id)?;
    assert_eq!(
        (record.current_revision, record.title.as_str()),
        (SourceRevision(1), "Shared")
    );
    Ok(())
}

#[test]
fn an_editor_commits_and_enqueues_but_owns_nothing() -> TestResult {
    let f = Fixture::new("editor")?;
    let editor = &f.editor;
    f.save(editor, "editor-save", 1)?;
    f.edit(editor, "editor-edit")?;
    f.restore(editor, "editor-restore")?;
    f.metadata(editor, "editor-title", Some("Ours"), None)?;
    f.enqueue(editor, "editor-job")?;
    let record = f.service.artifact(&f.owner, &f.id)?;
    assert_eq!(
        (record.current_revision, record.title.as_str()),
        (SourceRevision(4), "Ours")
    );
    // The history names the member who committed (decision 0060 rule 5).
    let revisions = f.service.revisions(&f.owner, &f.id)?;
    let by: Vec<_> = revisions
        .iter()
        .map(|revision| (revision.revision.0, revision.created_by == *editor))
        .collect();
    assert!(
        by.iter().all(|&(number, editors)| editors == (number > 1)),
        "{by:?}"
    );
    let refused = Err(ArtifactError::Forbidden);
    assert_eq!(
        f.metadata(editor, "editor-trash", None, Some(true)),
        refused
    );
    assert_eq!(f.purge(editor, "editor-purge"), refused);
    Ok(())
}

#[test]
fn the_role_is_refused_before_the_body_is_looked_at() -> TestResult {
    let f = Fixture::new("order")?;
    // A stale base would be a conflict for the editor; the viewer is told
    // only that its role does not allow the write (decision 0060 rule 4).
    f.save(&f.editor, "editor-save", 1)?;
    assert_eq!(
        f.save(&f.editor, "editor-stale", 1),
        Err(ArtifactError::RevisionConflict {
            current: SourceRevision(2)
        })
    );
    assert_eq!(
        f.save(&f.viewer, "viewer-stale", 1),
        Err(ArtifactError::Forbidden)
    );
    // Nor whether the change would make a package, or the revision exists.
    let emptied = f.service.save(
        &f.viewer,
        &parsed("viewer-empty"),
        &f.id,
        SourceRevision(2),
        vec![FileChange::Delete {
            path: PackagePath::entry(),
        }],
    );
    assert_eq!(emptied.map(|_| ()), Err(ArtifactError::Forbidden));
    let unknown = f.service.enqueue_job(
        &f.viewer,
        &parsed("viewer-job-99"),
        &f.id,
        SourceRevision(99),
        JobKind::VerifyRevision,
        UnixMillis(1),
    );
    assert_eq!(unknown.map(|_| ()), Err(ArtifactError::Forbidden));
    Ok(())
}

#[test]
fn a_stranger_a_revoked_member_and_every_member_in_the_trash_find_nothing() -> TestResult {
    let f = Fixture::new("hidden")?;
    let stranger = &f.stranger;
    assert_eq!(f.service.artifact(stranger, &f.id).map(|_| ()), Err(HIDDEN));
    assert_eq!(f.save(stranger, "stranger-save", 1), Err(HIDDEN));
    assert_eq!(
        f.duplicate(stranger, "stranger-copy").map(|_| ()),
        Err(HIDDEN)
    );
    assert_eq!(f.purge(stranger, "stranger-purge"), Err(HIDDEN));

    f.set_role("revoke-viewer", &f.viewer, None, f.version)?;
    assert_eq!(
        f.service.artifact(&f.viewer, &f.id).map(|_| ()),
        Err(HIDDEN)
    );
    assert_eq!(f.write_data(&f.viewer, "revoked-data"), Err(HIDDEN));

    f.metadata(&f.owner, "owner-trash", None, Some(true))?;
    let editor = &f.editor;
    assert_eq!(f.service.artifact(editor, &f.id).map(|_| ()), Err(HIDDEN));
    assert_eq!(f.save(editor, "editor-late", 1), Err(HIDDEN));
    assert_eq!(f.service.revisions(editor, &f.id).map(|_| ()), Err(HIDDEN));
    Ok(())
}
