//! Owner-only access enforced by the application core over real storage.
use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;

use artifree_core::artifacts::{ArtifactError, ArtifactService, MetadataChange, Missing};
use artifree_core::listing::ListQuery;
use artifree_domain::identity::{ActorId, RequestKey, Title};
use artifree_domain::package::{FileChange, FilePackage, PackagePath};
use artifree_domain::{MetadataRevision, SourceRevision};
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
    assert_eq!(
        service
            .update(
                &intruder,
                &parsed::<RequestKey>("intruder-key-2"),
                &id,
                MetadataRevision(1),
                &rename
            )
            .map(|_| ()),
        hidden
    );
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
