//! Per-file conditional edits through `ArtifactService` over real storage
//! (decision 0035): the contrast with the strict save, and the read-back of
//! a new revision against a store that does not tell the truth.

use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use artifree_core::artifacts::{ArtifactError, ArtifactService};
use artifree_domain::SourceRevision;
use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title};
use artifree_domain::package::{FileChange, FilePackage, PackagePath};
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactRecord, ArtifactStore, ChangePage, CleanupReceipt, CleanupRequest,
    CommitReceipt, ConditionalCommit, Delivery, FileEdit, FileExpectation, MetadataReceipt,
    MetadataUpdate, NewArtifact, PurgeReceipt, PurgeRequest, RequestIdentity, RevisionCommit,
    RevisionRecord, StoreError, UnreferencedContent,
};
use artifree_providers::storage::SqliteStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn parsed<T: FromStr>(value: &str) -> T
where
    T::Err: Display,
{
    value
        .parse()
        .unwrap_or_else(|error| panic!("invalid fixture {value:?}: {error}"))
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("artifree-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|error| panic!("cannot create {}: {error}", path.display()));
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How the store misreports what it holds once the edit has committed.
const HONEST: u8 = 0;
const OTHER_BYTES: u8 = 1;
const OTHER_DIGEST: u8 = 2;
const EXTRA_FILE: u8 = 3;
/// The store cannot be read for the new revision.
const UNREADABLE: u8 = 4;

/// A real store that lies about the revisions it reads back.
struct Misreporting {
    inner: SqliteStore,
    mode: Arc<AtomicU8>,
}

impl ArtifactStore for Misreporting {
    fn create_artifact(&self, request: NewArtifact<'_>) -> Result<CommitReceipt, StoreError> {
        self.inner.create_artifact(request)
    }
    fn commit_revision(&self, request: RevisionCommit<'_>) -> Result<CommitReceipt, StoreError> {
        self.inner.commit_revision(request)
    }
    fn commit_edits(&self, request: ConditionalCommit<'_>) -> Result<CommitReceipt, StoreError> {
        self.inner.commit_edits(request)
    }
    fn update_metadata(&self, request: MetadataUpdate<'_>) -> Result<MetadataReceipt, StoreError> {
        self.inner.update_metadata(request)
    }
    fn replay_create(
        &self,
        request: RequestIdentity<'_>,
    ) -> Result<Option<CommitReceipt>, StoreError> {
        self.inner.replay_create(request)
    }
    fn purge_artifact(&self, request: PurgeRequest<'_>) -> Result<PurgeReceipt, StoreError> {
        self.inner.purge_artifact(request)
    }
    fn unreferenced_content(&self) -> Result<UnreferencedContent, StoreError> {
        self.inner.unreferenced_content()
    }
    fn remove_unreferenced_content(
        &self,
        request: CleanupRequest<'_>,
    ) -> Result<CleanupReceipt, StoreError> {
        self.inner.remove_unreferenced_content(request)
    }
    fn artifact(&self, id: &ArtifactId) -> Result<Option<ArtifactRecord>, StoreError> {
        self.inner.artifact(id)
    }
    fn list_artifacts(
        &self,
        listing: ArtifactListing<'_>,
    ) -> Result<Vec<ArtifactRecord>, StoreError> {
        self.inner.list_artifacts(listing)
    }
    fn revisions(&self, id: &ArtifactId) -> Result<Vec<RevisionRecord>, StoreError> {
        self.inner.revisions(id)
    }
    fn revision(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
    ) -> Result<Option<RevisionRecord>, StoreError> {
        if self.mode.load(Ordering::SeqCst) == UNREADABLE && revision.0 >= 3 {
            return Err(StoreError::Unavailable("injected read failure".into()));
        }
        let mut record = self.inner.revision(id, revision)?;
        // Only the revision the edit created is misreported: the earlier one
        // is what the edit is compared with.
        if let Some(record) = record.as_mut().filter(|record| record.revision.0 >= 3) {
            match self.mode.load(Ordering::SeqCst) {
                OTHER_DIGEST => record.files[0].sha256[0] ^= 1,
                EXTRA_FILE => {
                    let mut extra = record.files[0].clone();
                    extra.path = parsed("zz-extra.txt");
                    record.files.push(extra);
                }
                _ => {}
            }
        }
        Ok(record)
    }
    fn revision_package(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
    ) -> Result<Option<FilePackage>, StoreError> {
        self.inner.revision_package(id, revision)
    }
    fn revision_file(
        &self,
        id: &ArtifactId,
        revision: SourceRevision,
        path: &PackagePath,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let mut bytes = self.inner.revision_file(id, revision, path)?;
        if self.mode.load(Ordering::SeqCst) == OTHER_BYTES
            && let Some(bytes) = bytes.as_mut()
        {
            bytes.push(b'!');
        }
        Ok(bytes)
    }
    fn changes(
        &self,
        owner: &ActorId,
        after: ChangeSequence,
        limit: usize,
    ) -> Result<ChangePage, StoreError> {
        self.inner.changes(owner, after, limit)
    }
}

struct Fixture {
    _directory: TempDir,
    service: ArtifactService<Misreporting>,
    mode: Arc<AtomicU8>,
    owner: ActorId,
    id: ArtifactId,
}

/// Revision 1: `index.html` ("v1") and `notes.txt` ("n1"). Revision 2 changes
/// `index.html` ("v2").
fn fixture(name: &str) -> Result<Fixture, Box<dyn std::error::Error>> {
    let directory = TempDir::new(name);
    let mode = Arc::new(AtomicU8::new(HONEST));
    let service = ArtifactService::new(Misreporting {
        inner: SqliteStore::open(&directory.0.join("db.sqlite3"))?,
        mode: Arc::clone(&mode),
    });
    let owner: ActorId = parsed("owner");
    let package = FilePackage::new(vec![
        (PackagePath::entry(), b"v1".to_vec()),
        (parsed("notes.txt"), b"n1".to_vec()),
    ])?;
    let created = service.create(
        &owner,
        &parsed("create-key-1"),
        &parsed::<Title>("Demo"),
        &package,
    )?;
    let id = created.artifact.id;
    service.save(
        &owner,
        &parsed("save-key-1"),
        &id,
        SourceRevision(1),
        vec![FileChange::Put {
            path: PackagePath::entry(),
            content: b"v2".to_vec(),
        }],
    )?;
    Ok(Fixture {
        _directory: directory,
        service,
        mode,
        owner,
        id,
    })
}

fn digest_of(fixture: &Fixture, revision: u64, name: &str) -> Result<[u8; 32], ArtifactError> {
    let record = fixture
        .service
        .revision(&fixture.owner, &fixture.id, SourceRevision(revision))?;
    let path: PackagePath = parsed(name);
    let Some(entry) = record.files.iter().find(|entry| entry.path == path) else {
        panic!("no {name} in revision {revision}");
    };
    Ok(entry.sha256)
}

fn put_notes(expect: FileExpectation) -> FileEdit {
    FileEdit {
        change: FileChange::Put {
            path: parsed("notes.txt"),
            content: b"n2".to_vec(),
        },
        expect,
    }
}

#[test]
fn the_strict_save_still_conflicts_where_the_edit_of_the_same_stale_base_applies() -> TestResult {
    let fixture = fixture("edit-strict")?;
    let change = || FileChange::Put {
        path: parsed("notes.txt"),
        content: b"n2".to_vec(),
    };

    // Revision 2 moved the head; the caller prepared this against revision 1.
    let strict = fixture.service.save(
        &fixture.owner,
        &parsed("save-key-2"),
        &fixture.id,
        SourceRevision(1),
        vec![change()],
    );
    assert_eq!(
        strict.map(|_| ()),
        Err(ArtifactError::RevisionConflict {
            current: SourceRevision(2)
        }),
        "the whole-revision command keeps its strict meaning"
    );

    let notes = digest_of(&fixture, 1, "notes.txt")?;
    let edited = fixture.service.edit_files(
        &fixture.owner,
        &parsed("edit-key-1"),
        &fixture.id,
        SourceRevision(1),
        &[put_notes(FileExpectation::Sha256(notes))],
    )?;
    assert_eq!(edited.revision.revision, SourceRevision(3));
    assert_eq!(
        fixture.service.read_file(
            &fixture.owner,
            &fixture.id,
            SourceRevision(3),
            &PackagePath::entry()
        )?,
        b"v2",
        "revision 2's index.html is kept"
    );
    Ok(())
}

#[test]
fn the_edit_names_the_conflicting_file_with_the_current_revision() -> TestResult {
    let fixture = fixture("edit-conflict")?;
    let stale = digest_of(&fixture, 1, "index.html")?;
    let current = digest_of(&fixture, 2, "index.html")?;
    let edit = FileEdit {
        change: FileChange::Put {
            path: PackagePath::entry(),
            content: b"mine".to_vec(),
        },
        expect: FileExpectation::Sha256(stale),
    };

    let result = fixture.service.edit_files(
        &fixture.owner,
        &parsed("edit-key-1"),
        &fixture.id,
        SourceRevision(1),
        &[edit],
    );

    let Err(ArtifactError::FilesChanged {
        current: head,
        files,
        total,
    }) = result
    else {
        return Err(format!("expected a file conflict: {result:?}").into());
    };
    assert_eq!((head, total), (SourceRevision(2), 1));
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, PackagePath::entry());
    assert_eq!(files[0].current, Some(current));
    Ok(())
}

#[test]
fn a_new_revision_that_is_not_what_was_asked_for_is_reported_though_it_exists() -> TestResult {
    for (what, mode) in [
        ("other bytes", OTHER_BYTES),
        ("another digest in the listing", OTHER_DIGEST),
        ("a file nobody asked for", EXTRA_FILE),
    ] {
        let fixture = fixture(&format!("edit-readback-{mode}"))?;
        let notes = digest_of(&fixture, 1, "notes.txt")?;
        fixture.mode.store(mode, Ordering::SeqCst);

        let result = fixture.service.edit_files(
            &fixture.owner,
            &parsed("edit-key-1"),
            &fixture.id,
            SourceRevision(2),
            &[put_notes(FileExpectation::Sha256(notes))],
        );

        assert_eq!(
            result.map(|_| ()),
            Err(ArtifactError::EditNotAsRequested),
            "{what}"
        );
        assert_eq!(
            fixture
                .service
                .artifact(&fixture.owner, &fixture.id)?
                .current_revision,
            SourceRevision(3),
            "{what}: the revision was committed and stays"
        );
    }
    Ok(())
}

#[test]
fn a_replay_is_not_read_back_again() -> TestResult {
    let fixture = fixture("edit-replay")?;
    let notes = digest_of(&fixture, 1, "notes.txt")?;
    let edits = [put_notes(FileExpectation::Sha256(notes))];
    let first = fixture.service.edit_files(
        &fixture.owner,
        &parsed("edit-key-1"),
        &fixture.id,
        SourceRevision(1),
        &edits,
    )?;
    // The store would now misreport, but the retry only returns the receipt.
    fixture.mode.store(OTHER_BYTES, Ordering::SeqCst);
    let again = fixture.service.edit_files(
        &fixture.owner,
        &parsed("edit-key-1"),
        &fixture.id,
        SourceRevision(1),
        &edits,
    )?;
    assert_eq!(again.revision, first.revision);
    Ok(())
}

#[test]
fn a_key_used_by_an_edit_is_not_free_for_a_strict_save() -> TestResult {
    let fixture = fixture("edit-key-scope")?;
    let notes = digest_of(&fixture, 1, "notes.txt")?;
    fixture.service.edit_files(
        &fixture.owner,
        &parsed("edit-key-1"),
        &fixture.id,
        SourceRevision(1),
        &[put_notes(FileExpectation::Sha256(notes))],
    )?;
    let reused = fixture.service.save(
        &fixture.owner,
        &parsed::<RequestKey>("edit-key-1"),
        &fixture.id,
        SourceRevision(3),
        vec![FileChange::Put {
            path: parsed("notes.txt"),
            content: b"n3".to_vec(),
        }],
    );
    assert_eq!(reused.map(|_| ()), Err(ArtifactError::RequestKeyReused));
    Ok(())
}

#[test]
fn a_retry_is_the_same_request_in_any_order_but_not_under_other_conditions() -> TestResult {
    let fixture = fixture("edit-identity")?;
    let notes = FileExpectation::Sha256(digest_of(&fixture, 1, "notes.txt")?);
    let index = FileExpectation::Sha256(digest_of(&fixture, 2, "index.html")?);
    let put_index = |expect| FileEdit {
        change: FileChange::Put {
            path: PackagePath::entry(),
            content: b"v3".to_vec(),
        },
        expect,
    };
    let run = |base, edits: &[FileEdit]| {
        fixture.service.edit_files(
            &fixture.owner,
            &parsed("edit-key-1"),
            &fixture.id,
            SourceRevision(base),
            edits,
        )
    };
    let first = run(2, &[put_notes(notes), put_index(index)])?;

    let reordered = run(2, &[put_index(index), put_notes(notes)])?;
    assert_eq!(
        (reordered.delivery, reordered.revision),
        (Delivery::Replayed, first.revision),
        "the same edits in another order"
    );
    // The same changes; the condition on one file is the only difference.
    let other_condition = run(2, &[put_notes(FileExpectation::Absent), put_index(index)]);
    assert_eq!(
        other_condition.map(|_| ()),
        Err(ArtifactError::RequestKeyReused)
    );
    let other_base = run(1, &[put_notes(notes), put_index(index)]);
    assert_eq!(other_base.map(|_| ()), Err(ArtifactError::RequestKeyReused));
    Ok(())
}

#[test]
fn an_edit_whose_commit_cannot_be_checked_is_an_error_that_says_a_revision_exists() -> TestResult {
    let fixture = fixture("edit-unverified")?;
    let notes = digest_of(&fixture, 1, "notes.txt")?;
    fixture.mode.store(UNREADABLE, Ordering::SeqCst);
    let edits = [put_notes(FileExpectation::Sha256(notes))];

    let result = fixture.service.edit_files(
        &fixture.owner,
        &parsed("edit-key-1"),
        &fixture.id,
        SourceRevision(2),
        &edits,
    );

    let Err(error) = result else {
        return Err("an unchecked edit must not be reported as verified".into());
    };
    assert_eq!(error, ArtifactError::EditUnverified);
    assert!(error.committed_a_revision());
    assert_eq!(
        fixture
            .service
            .artifact(&fixture.owner, &fixture.id)?
            .current_revision,
        SourceRevision(3),
        "the revision was committed"
    );
    // The retry returns the receipt of that revision.
    fixture.mode.store(HONEST, Ordering::SeqCst);
    let again = fixture.service.edit_files(
        &fixture.owner,
        &parsed("edit-key-1"),
        &fixture.id,
        SourceRevision(2),
        &edits,
    )?;
    assert_eq!(
        (again.delivery, again.revision.revision),
        (Delivery::Replayed, SourceRevision(3))
    );
    Ok(())
}
