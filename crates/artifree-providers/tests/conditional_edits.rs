//! Per-file conditional edits at the store (decision 0035): the expectations
//! are judged against the current revision in the commit's own step.

mod common;

use std::sync::Arc;
use std::thread;

use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::RequestKey;
use artifree_domain::package::{FileChange, PackageError, PackagePath};
use artifree_domain::{ArtifactKind, SourceRevision};
use artifree_ports::artifact_store::{
    ArtifactStore, CommitReceipt, ConditionalCommit, Delivery, FileEdit, FileExpectation,
    FileMismatch, MAX_LISTED_MISMATCHES, NewArtifact, RequestFingerprint, RequestIdentity,
    StoreError,
};
use artifree_providers::storage::SqliteStore;
use common::{
    TempDir, TestResult, actor, create_demo, digest, key, package, parsed, revise, update_metadata,
};
use sha2::{Digest, Sha256};

fn path(name: &str) -> PackagePath {
    parsed(name)
}

fn put(name: &str, body: &str, expect: FileExpectation) -> FileEdit {
    FileEdit {
        change: FileChange::Put {
            path: path(name),
            content: body.as_bytes().to_vec(),
        },
        expect,
    }
}

fn remove(name: &str, body: &str) -> FileEdit {
    FileEdit {
        change: FileChange::Delete { path: path(name) },
        expect: has(body),
    }
}

fn has(body: &str) -> FileExpectation {
    FileExpectation::Sha256(digest(body))
}

/// An edit request whose fingerprint is that of its content, as the service
/// derives it.
fn edit(
    store: &SqliteStore,
    created: &CommitReceipt,
    request_key: &RequestKey,
    edits: &[FileEdit],
) -> Result<CommitReceipt, StoreError> {
    store.commit_edits(ConditionalCommit {
        request: RequestIdentity {
            actor: &created.artifact.owner,
            key: request_key,
            fingerprint: RequestFingerprint(Sha256::digest(format!("{edits:?}")).into()),
        },
        artifact: &created.artifact.id,
        edits,
    })
}

/// Revision 1 holds `index.html` ("v1") and `style.css` ("h1{}").
fn fixture() -> Result<(TempDir, SqliteStore, CommitReceipt), Box<dyn std::error::Error>> {
    let directory = TempDir::new();
    let store = SqliteStore::open(&directory.join("artifree.sqlite3"))?;
    let created = create_demo(&store, &actor("owner"), &key("create"), "Demo", "v1")?;
    Ok((directory, store, created))
}

fn file(
    store: &SqliteStore,
    created: &CommitReceipt,
    revision: u64,
    name: &str,
) -> Result<Option<Vec<u8>>, StoreError> {
    store.revision_file(&created.artifact.id, SourceRevision(revision), &path(name))
}

fn revision_count(store: &SqliteStore, created: &CommitReceipt) -> Result<usize, StoreError> {
    Ok(store.revisions(&created.artifact.id)?.len())
}

fn feed_length(store: &SqliteStore, created: &CommitReceipt) -> Result<usize, StoreError> {
    Ok(store
        .changes(&created.artifact.owner, ChangeSequence(0), 1000)?
        .changes
        .len())
}

#[test]
fn a_stale_disjoint_edit_applies_to_the_current_files() -> TestResult {
    let (_directory, store, created) = fixture()?;
    revise(&store, &created, &key("other"), 1, "v2")?;

    // The edit's reader saw revision 1, but touches only style.css, which
    // revision 2 left as it was.
    let receipt = edit(
        &store,
        &created,
        &key("edit"),
        &[put("style.css", "h1{color:red}", has("h1{}"))],
    )?;

    assert_eq!(receipt.revision.revision, SourceRevision(3));
    assert_eq!(receipt.delivery, Delivery::Committed);
    assert_eq!(
        file(&store, &created, 3, "index.html")?.as_deref(),
        Some(b"v2".as_slice()),
        "an unlisted file is carried over from the current revision, not the one read"
    );
    assert_eq!(
        file(&store, &created, 3, "style.css")?.as_deref(),
        Some(b"h1{color:red}".as_slice())
    );
    assert_eq!(
        file(&store, &created, 2, "style.css")?.as_deref(),
        Some(b"h1{}".as_slice()),
        "earlier revisions are untouched"
    );
    Ok(())
}

#[test]
fn an_edit_of_a_file_that_changed_conflicts_and_writes_nothing() -> TestResult {
    let (_directory, store, created) = fixture()?;
    revise(&store, &created, &key("other"), 1, "v2")?;
    let revisions = revision_count(&store, &created)?;
    let feed = feed_length(&store, &created)?;

    let result = edit(
        &store,
        &created,
        &key("edit"),
        &[put("index.html", "mine", has("v1"))],
    );

    assert_eq!(
        result,
        Err(StoreError::FilesChanged {
            current: SourceRevision(2),
            mismatches: vec![FileMismatch {
                path: path("index.html"),
                current: Some(digest("v2")),
            }],
            total: 1,
        })
    );
    assert_eq!(revision_count(&store, &created)?, revisions);
    assert_eq!(feed_length(&store, &created)?, feed, "no feed entry either");
    assert_eq!(
        file(&store, &created, 2, "index.html")?.as_deref(),
        Some(b"v2".as_slice())
    );
    Ok(())
}

#[test]
fn one_unmet_condition_rejects_every_edit_and_lists_each_failure() -> TestResult {
    let (_directory, store, created) = fixture()?;
    revise(&store, &created, &key("other"), 1, "v2")?;
    let revisions = revision_count(&store, &created)?;

    let result = edit(
        &store,
        &created,
        &key("edit"),
        &[
            // Holds on its own: style.css is as it was read.
            put("style.css", "h1{color:red}", has("h1{}")),
            // Stale: index.html changed.
            put("index.html", "mine", has("v1")),
            // Expects a file that is not there.
            remove("gone.js", "x"),
            // Expects a create but the path is taken.
            put("app/z.js", "ok", FileExpectation::Absent),
        ],
    );

    assert_eq!(
        result,
        Err(StoreError::FilesChanged {
            current: SourceRevision(2),
            mismatches: vec![
                FileMismatch {
                    path: path("gone.js"),
                    current: None,
                },
                FileMismatch {
                    path: path("index.html"),
                    current: Some(digest("v2")),
                },
            ],
            total: 2,
        }),
        "every failed path in path order; the new file app/z.js and the holding style.css pass"
    );
    assert_eq!(revision_count(&store, &created)?, revisions);
    assert_eq!(
        file(&store, &created, 2, "style.css")?.as_deref(),
        Some(b"h1{}".as_slice()),
        "the edit whose own condition held was not applied"
    );
    Ok(())
}

#[test]
fn a_create_expects_the_path_to_be_absent() -> TestResult {
    let (_directory, store, created) = fixture()?;

    let taken = edit(
        &store,
        &created,
        &key("taken"),
        &[put("style.css", "new", FileExpectation::Absent)],
    );
    assert_eq!(
        taken,
        Err(StoreError::FilesChanged {
            current: SourceRevision(1),
            mismatches: vec![FileMismatch {
                path: path("style.css"),
                current: Some(digest("h1{}")),
            }],
            total: 1,
        })
    );

    let made = edit(
        &store,
        &created,
        &key("made"),
        &[put("extra.js", "1", FileExpectation::Absent)],
    )?;
    assert_eq!(made.revision.revision, SourceRevision(2));
    assert_eq!(
        file(&store, &created, 2, "extra.js")?.as_deref(),
        Some(b"1".as_slice())
    );
    Ok(())
}

#[test]
fn a_delete_needs_the_digest_the_file_has() -> TestResult {
    let (_directory, store, created) = fixture()?;

    let wrong = edit(
        &store,
        &created,
        &key("wrong"),
        &[remove("style.css", "other")],
    );
    assert!(
        matches!(wrong, Err(StoreError::FilesChanged { total: 1, .. })),
        "{wrong:?}"
    );

    let right = edit(
        &store,
        &created,
        &key("right"),
        &[remove("style.css", "h1{}")],
    )?;
    assert_eq!(right.revision.revision, SourceRevision(2));
    assert_eq!(file(&store, &created, 2, "style.css")?, None);
    assert_eq!(
        file(&store, &created, 1, "style.css")?.as_deref(),
        Some(b"h1{}".as_slice())
    );
    Ok(())
}

#[test]
fn a_rejected_edit_does_not_use_its_key() -> TestResult {
    let (_directory, store, created) = fixture()?;
    revise(&store, &created, &key("other"), 1, "v2")?;

    let refused = edit(
        &store,
        &created,
        &key("edit"),
        &[put("index.html", "mine", has("v1"))],
    );
    assert!(matches!(refused, Err(StoreError::FilesChanged { .. })));

    // The caller read the file again and sends a different request, under
    // the same key.
    let corrected = edit(
        &store,
        &created,
        &key("edit"),
        &[put("index.html", "mine", has("v2"))],
    )?;
    assert_eq!(corrected.delivery, Delivery::Committed);
    assert_eq!(corrected.revision.revision, SourceRevision(3));
    Ok(())
}

#[test]
fn a_retry_replays_after_the_head_moved_and_another_request_is_refused() -> TestResult {
    let (_directory, store, created) = fixture()?;
    let edits = [put("style.css", "h1{color:red}", has("h1{}"))];
    let first = edit(&store, &created, &key("edit"), &edits)?;
    // A later writer changes the very file the edit expected to be "h1{}".
    revise(&store, &created, &key("later"), 2, "v3")?;
    let revisions = revision_count(&store, &created)?;

    let again = edit(&store, &created, &key("edit"), &edits)?;
    assert_eq!(again.delivery, Delivery::Replayed);
    assert_eq!(again.revision, first.revision);
    assert_eq!(
        revision_count(&store, &created)?,
        revisions,
        "a replay writes nothing"
    );

    let different = edit(
        &store,
        &created,
        &key("edit"),
        &[put("style.css", "other", has("h1{}"))],
    );
    assert_eq!(different, Err(StoreError::RequestKeyReused));
    Ok(())
}

#[test]
fn an_edit_that_changes_nothing_is_unchanged_but_a_conflict_is_reported_first() -> TestResult {
    let (_directory, store, created) = fixture()?;

    let same = edit(
        &store,
        &created,
        &key("same"),
        &[put("style.css", "h1{}", has("h1{}"))],
    );
    assert_eq!(same, Err(StoreError::Unchanged));
    assert_eq!(revision_count(&store, &created)?, 1);

    revise(&store, &created, &key("other"), 1, "v2")?;
    let stale = edit(
        &store,
        &created,
        &key("stale"),
        &[
            put("style.css", "h1{}", has("h1{}")),
            put("index.html", "v1", has("v1")),
        ],
    );
    assert!(
        matches!(stale, Err(StoreError::FilesChanged { total: 1, .. })),
        "bytes equal to the caller's own base are no no-op once the file moved on: {stale:?}"
    );
    Ok(())
}

#[test]
fn an_edit_that_forms_no_package_is_refused_without_using_its_key() -> TestResult {
    let (_directory, store, created) = fixture()?;

    let result = edit(
        &store,
        &created,
        &key("edit"),
        &[remove("index.html", "v1")],
    );
    assert_eq!(result, Err(StoreError::Package(PackageError::MissingEntry)));
    assert_eq!(revision_count(&store, &created)?, 1);

    let retried = edit(
        &store,
        &created,
        &key("edit"),
        &[put("index.html", "v2", has("v1"))],
    )?;
    assert_eq!(retried.revision.revision, SourceRevision(2));
    Ok(())
}

#[test]
fn only_the_first_mismatches_are_listed_but_all_are_counted() -> TestResult {
    let directory = TempDir::new();
    let store = SqliteStore::open(&directory.join("artifree.sqlite3"))?;
    let owner = actor("owner");
    let names: Vec<String> = (0..MAX_LISTED_MISMATCHES + 8)
        .map(|index| format!("f{index:03}.txt"))
        .collect();
    let mut files: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "old")).collect();
    files.push(("index.html", "v1"));
    let create_key = key("create");
    let created = store.create_artifact(NewArtifact {
        request: RequestIdentity {
            actor: &owner,
            key: &create_key,
            fingerprint: RequestFingerprint([1; 32]),
        },
        kind: ArtifactKind::App,
        title: &parsed("Many"),
        package: &package(&files),
        source: None,
    })?;
    let edits: Vec<FileEdit> = names
        .iter()
        .map(|name| put(name, "new", has("not what it is")))
        .collect();

    let result = edit(&store, &created, &key("edit"), &edits);

    let Err(StoreError::FilesChanged {
        mismatches, total, ..
    }) = result
    else {
        return Err(format!("expected a conflict: {result:?}").into());
    };
    assert_eq!(total, names.len());
    assert_eq!(mismatches.len(), MAX_LISTED_MISMATCHES);
    let listed: Vec<&str> = mismatches.iter().map(|item| item.path.as_str()).collect();
    let expected: Vec<&str> = names
        .iter()
        .take(MAX_LISTED_MISMATCHES)
        .map(String::as_str)
        .collect();
    assert_eq!(listed, expected, "the first ones in path order");
    Ok(())
}

#[test]
fn racing_edits_of_one_file_have_exactly_one_winner() -> TestResult {
    let (_directory, store, created) = fixture()?;
    let store = Arc::new(store);
    let created = Arc::new(created);
    let writers: Vec<_> = (0..8)
        .map(|index| {
            let store = Arc::clone(&store);
            let created = Arc::clone(&created);
            thread::spawn(move || {
                edit(
                    &store,
                    &created,
                    &key(&format!("writer-{index}")),
                    &[put("style.css", &format!("h1{{n:{index}}}"), has("h1{}"))],
                )
            })
        })
        .collect();
    let mut winners = Vec::new();
    for (index, writer) in writers.into_iter().enumerate() {
        match writer.join().map_err(|_| "writer panicked")? {
            Ok(receipt) => {
                assert_eq!(receipt.revision.revision, SourceRevision(2));
                winners.push(index);
            }
            Err(StoreError::FilesChanged {
                current,
                mismatches,
                total,
            }) => {
                assert_eq!((current, total), (SourceRevision(2), 1));
                assert_eq!(mismatches[0].path, path("style.css"));
            }
            Err(other) => return Err(format!("unexpected failure: {other}").into()),
        }
    }
    assert_eq!(winners.len(), 1, "exactly one writer may win: {winners:?}");
    assert_eq!(revision_count(&store, &created)?, 2);
    assert_eq!(
        file(&store, &created, 2, "style.css")?,
        Some(format!("h1{{n:{}}}", winners[0]).into_bytes()),
        "the stored file is the winner's"
    );
    Ok(())
}

#[test]
fn racing_edits_of_different_files_all_apply_in_turn() -> TestResult {
    let directory = TempDir::new();
    let store = Arc::new(SqliteStore::open(&directory.join("artifree.sqlite3"))?);
    let owner = actor("owner");
    let names: Vec<String> = (0..8).map(|index| format!("part{index}.txt")).collect();
    let mut files: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "old")).collect();
    files.push(("index.html", "v1"));
    let create_key = key("create");
    let created = Arc::new(store.create_artifact(NewArtifact {
        request: RequestIdentity {
            actor: &owner,
            key: &create_key,
            fingerprint: RequestFingerprint([1; 32]),
        },
        kind: ArtifactKind::App,
        title: &parsed("Parts"),
        package: &package(&files),
        source: None,
    })?);
    // All eight read revision 1 and edit their own part, so each one is
    // stale by the time it commits unless it ran first.
    let writers: Vec<_> = names
        .iter()
        .cloned()
        .map(|name| {
            let store = Arc::clone(&store);
            let created = Arc::clone(&created);
            thread::spawn(move || {
                edit(
                    &store,
                    &created,
                    &key(&format!("writer-{name}")),
                    &[put(&name, &format!("new {name}"), has("old"))],
                )
            })
        })
        .collect();
    for writer in writers {
        writer.join().map_err(|_| "writer panicked")??;
    }

    assert_eq!(revision_count(&store, &created)?, 9, "one revision each");
    for name in &names {
        assert_eq!(
            file(&store, &created, 9, name)?,
            Some(format!("new {name}").into_bytes()),
            "no edit overwrote another's part"
        );
    }
    Ok(())
}

#[test]
fn an_edit_in_the_trash_is_its_replay_or_not_found() -> TestResult {
    let (_directory, store, created) = fixture()?;
    let edits = [put("style.css", "h1{color:red}", has("h1{}"))];
    edit(&store, &created, &key("edit"), &edits)?;
    update_metadata(&store, &created, &key("trash"), 1, None, None, Some(true))?;

    let replay = edit(&store, &created, &key("edit"), &edits)?;
    assert_eq!(replay.delivery, Delivery::Replayed);
    let fresh = edit(
        &store,
        &created,
        &key("fresh"),
        &[put("extra.js", "1", FileExpectation::Absent)],
    );
    assert_eq!(fresh, Err(StoreError::NotFound));
    let reused = edit(
        &store,
        &created,
        &key("edit"),
        &[put("extra.js", "1", FileExpectation::Absent)],
    );
    assert_eq!(reused, Err(StoreError::NotFound));
    Ok(())
}
