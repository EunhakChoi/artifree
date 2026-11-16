//! Artifacts made from catalog types through `ArtifactService` over real
//! storage (decision 0059 rules 5 to 7): creation with and without files,
//! the required paths of every commit, a replay across a release change, the
//! type a duplicate and a restore keep, and a release the server does not
//! know.

use std::path::{Path, PathBuf};

use artifree_core::artifacts::{ArtifactError, ArtifactService, CopyParts};
use artifree_core::catalog::{Catalog, Release};
use artifree_domain::artifact_type::{ArtifactType, TypeId, TypeRelease};
use artifree_domain::identity::{ActorId, RequestKey, Title};
use artifree_domain::package::{FileChange, FilePackage, FileSet, PackagePath};
use artifree_domain::{ArtifactKind, SourceRevision};
use artifree_ports::artifact_store::{CommitReceipt, Delivery, FileEdit, FileExpectation};
use artifree_providers::storage::SqliteStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Result<Self, std::io::Error> {
        let path =
            std::env::temp_dir().join(format!("artifree-types-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn database(&self) -> PathBuf {
        self.0.join("db.sqlite3")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn package(files: &[(&str, &str)]) -> Result<FilePackage, Box<dyn std::error::Error>> {
    let mut parsed = Vec::new();
    for (path, content) in files {
        parsed.push((path.parse::<PackagePath>()?, content.as_bytes().to_vec()));
    }
    Ok(FilePackage::new(parsed)?)
}

/// The files a typed create uploads, which may lack `index.html`.
fn files(files: &[(&str, &str)]) -> Result<FileSet, Box<dyn std::error::Error>> {
    let mut parsed = Vec::new();
    for (path, content) in files {
        parsed.push((path.parse::<PackagePath>()?, content.as_bytes().to_vec()));
    }
    Ok(FileSet::new(parsed)?)
}

/// Release `number` of the type `app`, requiring `required`, whose starter
/// holds the required paths and `starter.css`.
fn release(number: u32, required: &[&str]) -> Result<Release, Box<dyn std::error::Error>> {
    let mut starter = vec![("starter.css", "p {}")];
    starter.extend(required.iter().map(|path| (*path, "starter")));
    if !required.contains(&"index.html") {
        starter.push(("index.html", "<p>starter</p>"));
    }
    let mut paths = Vec::new();
    for path in required {
        paths.push(path.parse::<PackagePath>()?);
    }
    paths.sort();
    Ok(Release {
        id: "app".parse()?,
        release: TypeRelease::new(number)?,
        title: "App".to_owned(),
        description: "A test type.".to_owned(),
        kind: ArtifactKind::App,
        required: paths,
        starter: package(&starter)?,
        build_profile: None,
        guide: format!("# Release {number}\n"),
        references: Vec::new(),
    })
}

/// A catalog whose type `app` has `releases`, each as its required paths.
fn catalog(releases: &[&[&str]]) -> Result<Catalog, Box<dyn std::error::Error>> {
    let mut built = Vec::new();
    for (number, required) in (1_u32..).zip(releases) {
        built.push(release(number, required)?);
    }
    Ok(Catalog::new(vec![built])?)
}

fn service(
    database: &Path,
    catalog: Catalog,
) -> Result<ArtifactService<SqliteStore>, Box<dyn std::error::Error>> {
    Ok(ArtifactService::new(SqliteStore::open(database)?).with_catalog(catalog))
}

fn app_type() -> Result<TypeId, Box<dyn std::error::Error>> {
    Ok("app".parse()?)
}

fn pinned(receipt: &CommitReceipt, release: u32) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(receipt.artifact.artifact_type
        == Some(ArtifactType {
            id: app_type()?,
            release: TypeRelease::new(release)?,
        }))
}

fn paths(receipt: &CommitReceipt) -> Vec<&str> {
    receipt
        .revision
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

fn missing(paths: &[&str]) -> Result<ArtifactError, Box<dyn std::error::Error>> {
    let mut missing = Vec::new();
    for path in paths {
        missing.push(path.parse::<PackagePath>()?);
    }
    Ok(ArtifactError::TypeRequirements { missing })
}

#[test]
fn a_create_without_files_gets_the_starter_and_records_the_release() -> TestResult {
    let directory = TempDir::new("starter")?;
    let artifacts = service(&directory.database(), catalog(&[&["src/main.tsx"]])?)?;
    let owner: ActorId = "owner".parse()?;
    let title: Title = "Typed".parse()?;
    let receipt =
        artifacts.create_from_type(&owner, &"type-key-1".parse()?, &title, &app_type()?, None)?;
    assert!(pinned(&receipt, 1)?);
    assert_eq!(receipt.artifact.kind, ArtifactKind::App);
    assert_eq!(
        paths(&receipt),
        ["index.html", "src/main.tsx", "starter.css"]
    );
    // The type is what every read shows, the listing included.
    let read = artifacts.artifact(&owner, &receipt.artifact.id)?;
    assert_eq!(read.artifact_type, receipt.artifact.artifact_type);
    let listed = artifacts.list(&owner, &listing_query(), None)?;
    assert_eq!(listed.artifacts.len(), 1);
    assert_eq!(
        listed
            .artifacts
            .first()
            .map(|listed| &listed.record.artifact_type),
        Some(&receipt.artifact.artifact_type)
    );
    // An untyped create is unchanged: no type.
    let plain = artifacts.create(
        &owner,
        &"type-key-2".parse()?,
        &title,
        &package(&[("index.html", "<p>plain</p>")])?,
    )?;
    assert_eq!(plain.artifact.artifact_type, None);
    Ok(())
}

fn listing_query() -> artifree_core::listing::ListQuery {
    artifree_core::listing::ListQuery::default()
}

#[test]
fn files_are_the_whole_first_revision_and_must_hold_the_required_paths() -> TestResult {
    let directory = TempDir::new("files")?;
    let artifacts = service(&directory.database(), catalog(&[&["src/main.tsx"]])?)?;
    let owner: ActorId = "owner".parse()?;
    let title: Title = "Typed".parse()?;
    let own = files(&[("index.html", "<p>own</p>"), ("src/main.tsx", "own")])?;
    let receipt = artifacts.create_from_type(
        &owner,
        &"type-key-1".parse()?,
        &title,
        &app_type()?,
        Some(&own),
    )?;
    // The starter is not merged in.
    assert_eq!(paths(&receipt), ["index.html", "src/main.tsx"]);
    assert!(pinned(&receipt, 1)?);

    let lacking = files(&[("index.html", "<p>own</p>")])?;
    let refused = artifacts.create_from_type(
        &owner,
        &"type-key-2".parse()?,
        &title,
        &app_type()?,
        Some(&lacking),
    );
    assert_eq!(refused, Err(missing(&["src/main.tsx"])?));
    let unknown = artifacts.create_from_type(
        &owner,
        &"type-key-3".parse()?,
        &title,
        &"slides".parse()?,
        None,
    );
    assert_eq!(unknown, Err(ArtifactError::TypeUnknown));
    // Neither refusal wrote anything.
    assert_eq!(
        artifacts
            .list(&owner, &listing_query(), None)?
            .artifacts
            .len(),
        1
    );
    Ok(())
}

#[test]
fn a_retried_create_replays_its_release_after_the_catalog_gains_one() -> TestResult {
    let directory = TempDir::new("replay")?;
    let owner: ActorId = "owner".parse()?;
    let title: Title = "Typed".parse()?;
    let key: RequestKey = "type-key-1".parse()?;
    let first = service(&directory.database(), catalog(&[&["src/main.tsx"]])?)?.create_from_type(
        &owner,
        &key,
        &title,
        &app_type()?,
        None,
    )?;
    // A server upgrade adds release 2, which requires another path.
    let upgraded = service(
        &directory.database(),
        catalog(&[&["src/main.tsx"], &["src/main.tsx", "src/extra.ts"]])?,
    )?;
    let replayed = upgraded.create_from_type(&owner, &key, &title, &app_type()?, None)?;
    assert_eq!(replayed.delivery, Delivery::Replayed);
    assert_eq!(replayed.artifact.id, first.artifact.id);
    assert!(pinned(&replayed, 1)?);
    // The same files sent again also replay, though release 2 would refuse
    // them.
    let files = files(&[("index.html", "<p>own</p>"), ("src/main.tsx", "own")])?;
    let with_files_key: RequestKey = "type-key-2".parse()?;
    let before = service(&directory.database(), catalog(&[&["src/main.tsx"]])?)?.create_from_type(
        &owner,
        &with_files_key,
        &title,
        &app_type()?,
        Some(&files),
    )?;
    let again =
        upgraded.create_from_type(&owner, &with_files_key, &title, &app_type()?, Some(&files))?;
    assert_eq!(again.delivery, Delivery::Replayed);
    assert_eq!(again.artifact.id, before.artifact.id);
    // A new key gets the current release.
    let fresh =
        upgraded.create_from_type(&owner, &"type-key-3".parse()?, &title, &app_type()?, None)?;
    assert!(pinned(&fresh, 2)?);
    assert_eq!(
        paths(&fresh),
        ["index.html", "src/extra.ts", "src/main.tsx", "starter.css"]
    );
    Ok(())
}

#[test]
fn every_commit_keeps_the_required_paths() -> TestResult {
    let directory = TempDir::new("commits")?;
    let artifacts = service(&directory.database(), catalog(&[&["src/main.tsx"]])?)?;
    let owner: ActorId = "owner".parse()?;
    let created = artifacts.create_from_type(
        &owner,
        &"type-key-1".parse()?,
        &"Typed".parse()?,
        &app_type()?,
        None,
    )?;
    let id = created.artifact.id;
    let main: PackagePath = "src/main.tsx".parse()?;
    let refused = artifacts.save(
        &owner,
        &"type-key-2".parse()?,
        &id,
        SourceRevision(1),
        vec![FileChange::Delete { path: main.clone() }],
    );
    assert_eq!(refused, Err(missing(&["src/main.tsx"])?));
    let digest = created
        .revision
        .files
        .iter()
        .find(|file| file.path == main)
        .map(|file| file.sha256)
        .ok_or("no src/main.tsx")?;
    let edit = FileEdit {
        change: FileChange::Delete { path: main.clone() },
        expect: FileExpectation::Sha256(digest),
    };
    let refused = artifacts.edit_files(
        &owner,
        &"type-key-3".parse()?,
        &id,
        SourceRevision(1),
        &[edit],
    );
    assert_eq!(refused, Err(missing(&["src/main.tsx"])?));
    // A commit that keeps them passes, and the head is still revision 2 after
    // both refusals.
    let saved = artifacts.save(
        &owner,
        &"type-key-4".parse()?,
        &id,
        SourceRevision(1),
        vec![FileChange::Delete {
            path: "starter.css".parse()?,
        }],
    )?;
    assert_eq!(saved.revision.revision, SourceRevision(2));
    assert!(pinned(&saved, 1)?);
    Ok(())
}

/// A release that requires `index.html` names it as a missing path, as any
/// other, in a create's files and in a save or edit that deletes it, rather
/// than refusing the package for lacking its entry document.
#[test]
fn a_required_index_html_is_named_as_a_missing_path() -> TestResult {
    let directory = TempDir::new("entry")?;
    let artifacts = service(
        &directory.database(),
        catalog(&[&["index.html", "src/main.tsx"]])?,
    )?;
    let owner: ActorId = "owner".parse()?;
    let title: Title = "Typed".parse()?;
    let refused = artifacts.create_from_type(
        &owner,
        &"type-key-1".parse()?,
        &title,
        &app_type()?,
        Some(&files(&[("notes.txt", "n")])?),
    );
    assert_eq!(refused, Err(missing(&["index.html", "src/main.tsx"])?));
    let created =
        artifacts.create_from_type(&owner, &"type-key-2".parse()?, &title, &app_type()?, None)?;
    let id = created.artifact.id;
    let entry = PackagePath::entry();
    let refused = artifacts.save(
        &owner,
        &"type-key-3".parse()?,
        &id,
        SourceRevision(1),
        vec![FileChange::Delete {
            path: entry.clone(),
        }],
    );
    assert_eq!(refused, Err(missing(&["index.html"])?));
    // The refusal does not depend on the base: rebasing would not help.
    artifacts.save(
        &owner,
        &"type-key-4".parse()?,
        &id,
        SourceRevision(1),
        vec![FileChange::Delete {
            path: "starter.css".parse()?,
        }],
    )?;
    let stale = artifacts.save(
        &owner,
        &"type-key-5".parse()?,
        &id,
        SourceRevision(1),
        vec![FileChange::Delete { path: entry }],
    );
    assert_eq!(stale, Err(missing(&["index.html"])?));
    // A release that does not require it refuses files without it as a
    // package, as an untyped create does.
    let lenient = service(&directory.database(), catalog(&[&["src/main.tsx"]])?)?;
    let refused = lenient.create_from_type(
        &owner,
        &"type-key-6".parse()?,
        &title,
        &app_type()?,
        Some(&files(&[("src/main.tsx", "m")])?),
    );
    assert!(
        matches!(refused, Err(ArtifactError::InvalidPackage(_))),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_duplicate_and_a_restore_keep_the_type_and_a_restore_checks_it() -> TestResult {
    let directory = TempDir::new("copies")?;
    let owner: ActorId = "owner".parse()?;
    let artifacts = service(&directory.database(), catalog(&[&["index.html"]])?)?;
    let created = artifacts.create_from_type(
        &owner,
        &"type-key-1".parse()?,
        &"Typed".parse()?,
        &app_type()?,
        None,
    )?;
    let id = created.artifact.id;
    // Revision 2 holds src/main.tsx, which revision 1 lacks.
    artifacts.save(
        &owner,
        &"type-key-2".parse()?,
        &id,
        SourceRevision(1),
        vec![FileChange::Put {
            path: "src/main.tsx".parse()?,
            content: b"main".to_vec(),
        }],
    )?;
    let copy = artifacts.duplicate(
        &owner,
        &"type-key-3".parse()?,
        &id,
        SourceRevision(1),
        None,
        CopyParts::default(),
    )?;
    assert_ne!(copy.artifact.id, id);
    assert!(pinned(&copy, 1)?);
    let restored = artifacts.restore(
        &owner,
        &"type-key-4".parse()?,
        &id,
        SourceRevision(2),
        SourceRevision(1),
    )?;
    assert!(pinned(&restored, 1)?);
    // A store whose revision no longer meets its release, here because the
    // release is read as requiring more than it did, refuses the restore.
    let stricter = service(
        &directory.database(),
        catalog(&[&["index.html", "src/main.tsx"]])?,
    )?;
    let refused = stricter.restore(
        &owner,
        &"type-key-5".parse()?,
        &id,
        SourceRevision(3),
        SourceRevision(1),
    );
    assert_eq!(refused, Err(missing(&["src/main.tsx"])?));
    Ok(())
}

#[test]
fn a_release_the_server_does_not_know_serves_reads_and_refuses_new_writes() -> TestResult {
    let directory = TempDir::new("unknown")?;
    let owner: ActorId = "owner".parse()?;
    let newer = service(
        &directory.database(),
        catalog(&[&["index.html"], &["index.html"]])?,
    )?;
    let created = newer.create_from_type(
        &owner,
        &"type-key-1".parse()?,
        &"Typed".parse()?,
        &app_type()?,
        None,
    )?;
    assert!(pinned(&created, 2)?);
    let id = created.artifact.id;
    let notes: PackagePath = "notes.txt".parse()?;
    let save_change = vec![FileChange::Put {
        path: notes.clone(),
        content: b"n".to_vec(),
    }];
    let committed = newer.save(
        &owner,
        &"type-key-2".parse()?,
        &id,
        SourceRevision(1),
        save_change.clone(),
    )?;
    assert_eq!(committed.delivery, Delivery::Committed);

    // A downgraded server knows release 1 only.
    let older = service(&directory.database(), catalog(&[&["index.html"]])?)?;
    assert_eq!(
        older.artifact(&owner, &id)?.artifact_type,
        created.artifact.artifact_type
    );
    let refused = older.save(
        &owner,
        &"type-key-3".parse()?,
        &id,
        SourceRevision(2),
        save_change.clone(),
    );
    assert_eq!(refused, Err(ArtifactError::TypeReleaseUnknown));
    let refused = older.restore(
        &owner,
        &"type-key-4".parse()?,
        &id,
        SourceRevision(2),
        SourceRevision(1),
    );
    assert_eq!(refused, Err(ArtifactError::TypeReleaseUnknown));
    let refused = older.duplicate(
        &owner,
        &"type-key-5".parse()?,
        &id,
        SourceRevision(2),
        None,
        CopyParts::default(),
    );
    assert_eq!(refused, Err(ArtifactError::TypeReleaseUnknown));
    // A retry of the save that committed replays before the refusal.
    let replayed = older.save(
        &owner,
        &"type-key-2".parse()?,
        &id,
        SourceRevision(1),
        save_change,
    )?;
    assert_eq!(replayed.delivery, Delivery::Replayed);
    assert_eq!(replayed.revision.revision, SourceRevision(2));
    // Nothing was written by the refusals.
    assert_eq!(
        older.artifact(&owner, &id)?.current_revision,
        SourceRevision(2)
    );
    Ok(())
}
