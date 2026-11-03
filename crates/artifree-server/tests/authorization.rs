//! Owner-only access enforced by the application core over real storage.
use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;

use artifree_core::artifacts::{ArtifactError, ArtifactService, MetadataChange, Missing};
use artifree_core::listing::ListQuery;
use artifree_domain::data::{DataChange, DataPath, DataValue};
use artifree_domain::identity::{ActorId, RequestKey, Title, UnixMillis};
use artifree_domain::package::{FileChange, FilePackage, PackagePath};
use artifree_domain::{AppDataVersion, MetadataRevision, SourceRevision};
use artifree_ports::artifact_store::{Delivery, FileEdit, FileExpectation};
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

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn another_actor_can_neither_see_nor_change_an_artifact() -> TestResult {
    let directory =
        TempDir(std::env::temp_dir().join(format!("artifree-authz-{}", std::process::id())));
    std::fs::create_dir_all(&directory.0)?;
    let service = ArtifactService::new(SqliteStore::open(&directory.0.join("db.sqlite3"))?);
    let owner: ActorId = parsed("owner");
    let intruder: ActorId = parsed("intruder");
    let package = FilePackage::new(vec![(PackagePath::entry(), b"<p>secret</p>".to_vec())])?;
    let created = service.create(
        &owner,
        &parsed::<RequestKey>("owner-key-1"),
        &parsed::<Title>("Private"),
        &package,
    )?;
    let id = created.artifact.id;
    let hidden = Err(ArtifactError::NotFound(Missing::Artifact));

    assert_eq!(service.artifact(&intruder, &id).map(|_| ()), hidden);
    assert_eq!(service.revisions(&intruder, &id).map(|_| ()), hidden);
    assert_eq!(
        service
            .read_file(&intruder, &id, SourceRevision(1), &PackagePath::entry())
            .map(|_| ()),
        hidden
    );
    assert_eq!(
        service
            .export(&intruder, &id, SourceRevision(1))
            .map(|_| ()),
        hidden
    );
    let change = vec![FileChange::Put {
        path: PackagePath::entry(),
        content: b"<p>defaced</p>".to_vec(),
    }];
    assert_eq!(
        service
            .save(
                &intruder,
                &parsed::<RequestKey>("intruder-key-1"),
                &id,
                SourceRevision(1),
                change
            )
            .map(|_| ()),
        hidden
    );
    let rename = MetadataChange::new(Some(parsed("Defaced")), Some(true), None)
        .ok_or("a change with fields")?;
    let delete = MetadataChange::new(None, None, Some(true)).ok_or("a change with fields")?;
    for (key, change) in [("intruder-key-2", &rename), ("intruder-key-3", &delete)] {
        assert_eq!(
            service
                .update(
                    &intruder,
                    &parsed::<RequestKey>(key),
                    &id,
                    MetadataRevision(1),
                    change
                )
                .map(|_| ()),
            hidden,
            "{key}"
        );
    }
    assert!(
        service
            .list(&intruder, &ListQuery::default(), None)?
            .artifacts
            .is_empty(),
        "listing must only include owned artifacts"
    );

    let current = service.artifact(&owner, &id)?;
    assert_eq!(
        current.current_revision,
        SourceRevision(1),
        "a rejected actor must not write"
    );
    assert_eq!(
        (
            current.title.as_str(),
            current.pinned,
            current.metadata_revision
        ),
        ("Private", false, MetadataRevision(1)),
        "a rejected actor must not change metadata"
    );
    assert_eq!(
        service.read_file(&owner, &id, SourceRevision(1), &PackagePath::entry())?,
        b"<p>secret</p>"
    );
    Ok(())
}

/// An edit, none, and a path edited twice are all answered as for an unknown
/// artifact: the artifact is judged before the request is.
#[test]
fn another_actor_cannot_edit_files_or_learn_what_the_edit_would_hold() -> TestResult {
    let directory =
        TempDir(std::env::temp_dir().join(format!("artifree-authz-edit-{}", std::process::id())));
    std::fs::create_dir_all(&directory.0)?;
    let service = ArtifactService::new(SqliteStore::open(&directory.0.join("db.sqlite3"))?);
    let owner: ActorId = parsed("owner");
    let intruder: ActorId = parsed("intruder");
    let package = FilePackage::new(vec![(PackagePath::entry(), b"<p>secret</p>".to_vec())])?;
    let created = service.create(
        &owner,
        &parsed::<RequestKey>("owner-key-1"),
        &parsed::<Title>("Private"),
        &package,
    )?;
    let id = created.artifact.id;
    let hidden = Err(ArtifactError::NotFound(Missing::Artifact));
    let would_hold = FileEdit {
        change: FileChange::Put {
            path: PackagePath::entry(),
            content: b"<p>defaced</p>".to_vec(),
        },
        expect: FileExpectation::Absent,
    };
    for (key, edits) in [
        ("intruder-key-4", vec![would_hold.clone()]),
        ("intruder-key-5", Vec::new()),
        ("intruder-key-6", vec![would_hold.clone(), would_hold]),
    ] {
        assert_eq!(
            service
                .edit_files(
                    &intruder,
                    &parsed::<RequestKey>(key),
                    &id,
                    SourceRevision(1),
                    &edits
                )
                .map(|_| ()),
            hidden,
            "{key}"
        );
    }
    assert_eq!(
        service.artifact(&owner, &id)?.current_revision,
        SourceRevision(1),
        "a rejected actor must not write"
    );
    Ok(())
}

/// Removing for good is the owner's alone, and tells another actor nothing:
/// not whether the artifact is in the trash, nor whether it was purged.
#[test]
fn another_actor_cannot_purge_or_learn_of_a_purge() -> TestResult {
    let directory =
        TempDir(std::env::temp_dir().join(format!("artifree-authz-purge-{}", std::process::id())));
    std::fs::create_dir_all(&directory.0)?;
    let service = ArtifactService::new(SqliteStore::open(&directory.0.join("db.sqlite3"))?);
    let owner: ActorId = parsed("owner");
    let intruder: ActorId = parsed("intruder");
    let package = FilePackage::new(vec![(PackagePath::entry(), b"<p>secret</p>".to_vec())])?;
    let created = service.create(
        &owner,
        &parsed::<RequestKey>("owner-key-1"),
        &parsed::<Title>("Private"),
        &package,
    )?;
    let id = created.artifact.id;
    let hidden = Err(ArtifactError::NotFound(Missing::Artifact));
    let intruder_purge = |key: &str, base| {
        service
            .purge(
                &intruder,
                &parsed::<RequestKey>(key),
                &id,
                MetadataRevision(base),
            )
            .map(|_| ())
    };

    // Outside the trash the owner is told so; another actor is told nothing.
    assert_eq!(intruder_purge("intruder-key-1", 1), hidden);
    assert_eq!(
        service
            .purge(&owner, &parsed("owner-key-2"), &id, MetadataRevision(1))
            .map(|_| ()),
        Err(ArtifactError::NotInTrash)
    );
    let delete = MetadataChange::new(None, None, Some(true)).ok_or("a change with fields")?;
    service.update(
        &owner,
        &parsed("owner-key-3"),
        &id,
        MetadataRevision(1),
        &delete,
    )?;
    assert_eq!(intruder_purge("intruder-key-2", 2), hidden);
    assert!(
        service.artifact(&owner, &id).is_err()
            && service.list(&owner, &deleted(), None)?.artifacts.len() == 1,
        "the intruder's attempts must leave the artifact in the trash"
    );

    let owner_key = parsed::<RequestKey>("owner-key-4");
    service.purge(&owner, &owner_key, &id, MetadataRevision(2))?;
    // After the purge the answer is the same for both, but only the owner's
    // own retry, under the owner's key, replays.
    assert_eq!(intruder_purge("intruder-key-3", 3), hidden);
    assert_eq!(
        intruder_purge("owner-key-4", 2),
        hidden,
        "keys are per actor"
    );
    let replay = service.purge(&owner, &owner_key, &id, MetadataRevision(2))?;
    assert_eq!(replay.delivery, Delivery::Replayed);
    // Another actor cannot recover or read it either.
    assert_eq!(service.revisions(&intruder, &id).map(|_| ()), hidden);
    Ok(())
}

/// Application data (decision 0031) is the owner's: another actor can neither
/// read nor write it, and is told the same as for an artifact that is not there.
/// Each actor's data space is its own, and so are its keys.
#[test]
fn another_actor_can_neither_read_nor_write_application_data() -> TestResult {
    let directory =
        TempDir(std::env::temp_dir().join(format!("artifree-authz-data-{}", std::process::id())));
    std::fs::create_dir_all(&directory.0)?;
    let service = ArtifactService::new(SqliteStore::open(&directory.0.join("db.sqlite3"))?);
    let owner: ActorId = parsed("owner");
    let intruder: ActorId = parsed("intruder");
    let package = FilePackage::new(vec![(PackagePath::entry(), b"<p>secret</p>".to_vec())])?;
    let id = service
        .create(
            &owner,
            &parsed::<RequestKey>("owner-key-1"),
            &parsed::<Title>("Private"),
            &package,
        )?
        .artifact
        .id;
    let hidden = Err(ArtifactError::NotFound(Missing::Artifact));
    let path: DataPath = parsed("secret");
    let set = DataChange::Set {
        path: path.clone(),
        value: DataValue::parse("\"kept\"")?,
    };
    let now = UnixMillis(1);
    service.write_data(
        &owner,
        &parsed::<RequestKey>("owner-key-2"),
        &id,
        AppDataVersion(0),
        &set,
        now,
    )?;

    assert_eq!(service.data(&intruder, &id, &path).map(|_| ()), hidden);
    let overwrite = DataChange::Set {
        path: path.clone(),
        value: DataValue::parse("\"defaced\"")?,
    };
    for (key, base) in [("intruder-key-1", 0), ("intruder-key-2", 1)] {
        assert_eq!(
            service
                .write_data(
                    &intruder,
                    &parsed::<RequestKey>(key),
                    &id,
                    AppDataVersion(base),
                    &overwrite,
                    now,
                )
                .map(|_| ()),
            hidden,
            "{key}"
        );
    }
    // The owner's retry of their own request is theirs alone to replay.
    assert_eq!(
        service
            .write_data(
                &intruder,
                &parsed::<RequestKey>("owner-key-2"),
                &id,
                AppDataVersion(0),
                &set,
                now,
            )
            .map(|_| ()),
        hidden,
        "keys are per actor"
    );
    let read = service.data(&owner, &id, &path)?;
    assert_eq!(
        (read.version, read.value.map(|value| value.to_canonical())),
        (AppDataVersion(1), Some("\"kept\"".to_owned())),
        "a rejected actor must not write"
    );
    Ok(())
}

fn deleted() -> ListQuery {
    ListQuery {
        deleted: true,
        ..ListQuery::default()
    }
}
