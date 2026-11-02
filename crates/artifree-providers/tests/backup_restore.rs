//! Backup and restore of the data directory's database.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use artifree_domain::SourceRevision;
use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::TokenId;
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactStore, CommitReceipt, Delivery, ListOrder, StoreError,
};
use artifree_ports::token_store::{NewTokenRecord, TokenStore};
use artifree_providers::backup::{BackupError, SCRATCH_PREFIX, backup, restore};
use artifree_providers::storage::{DATABASE_FILE, SCHEMA_VERSION, SqliteStore};
use artifree_providers::tokens::{SqliteTokenStore, hash_token_secret};
use common::{
    OLD_BODY, TempDir, TestResult, actor, create_demo, current_database, database_in, digest,
    expected_artifact_name, files_of, historical_database, key, old_artifact, parsed, revise,
    update_metadata, user_version,
};
use rusqlite::Connection;

/// A data directory `root/data` holding the database file, and an empty
/// `root/out` for outputs.
struct Layout {
    root: TempDir,
}

impl Layout {
    fn new() -> Self {
        let root = TempDir::new();
        fs::create_dir(root.join("data")).unwrap_or_else(|error| panic!("{error}"));
        fs::create_dir(root.join("out")).unwrap_or_else(|error| panic!("{error}"));
        Self { root }
    }
    fn data(&self) -> PathBuf {
        self.root.join("data")
    }
    fn database(&self) -> PathBuf {
        database_in(&self.data())
    }
    fn output(&self) -> PathBuf {
        self.root.join("out").join("backup.sqlite3")
    }
    fn out_dir(&self) -> PathBuf {
        self.root.join("out")
    }
    fn target(&self) -> PathBuf {
        self.root.join("restored")
    }
}

fn token(index: u8) -> NewTokenRecord {
    NewTokenRecord {
        id: parsed::<TokenId>(&format!("tok_{index:032x}")),
        name: format!("token {index}"),
        secret_hash: hash_token_secret(&format!("secret {index}")),
        prefix: format!("art_{index:02}"),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

fn error_of<T: std::fmt::Debug>(result: Result<T, BackupError>, case: &str) -> BackupError {
    match result {
        Err(error) => error,
        Ok(value) => panic!("{case}: unexpectedly succeeded with {value:?}"),
    }
}

fn refused<T: std::fmt::Debug>(result: &Result<T, BackupError>, case: &str) {
    assert!(
        matches!(result, Err(BackupError::Refused(_))),
        "{case}: expected a refusal, got {result:?}"
    );
}

/// SQL that edits the stored `CREATE` text of `table` in place, as `SQLite`'s own
/// rewrites do, without changing the table's data.
fn rewritten_schema(table: &str, from: &str, to: &str) -> String {
    format!(
        "PRAGMA writable_schema = ON;
         UPDATE sqlite_master SET sql = replace(sql, '{from}', '{to}') WHERE name = '{table}';
         PRAGMA writable_schema = OFF;"
    )
}

/// Databases a backup and a restore must both refuse: each is named with the
/// function that builds it in `directory`.
fn refusable(case: &str, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mutate = |sql: &str| -> TestResult {
        current_database(path)?;
        Connection::open(path)?.execute_batch(sql)?;
        Ok(())
    };
    match case {
        "newer schema" => mutate("PRAGMA user_version = 99"),
        "empty file" => Ok(fs::write(path, b"")?),
        "unrelated database" => Ok(Connection::open(path)?
            .execute_batch("CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('x')")?),
        "unrelated database with our version" => Ok(Connection::open(path)?.execute_batch(
            &format!("CREATE TABLE notes (body TEXT); PRAGMA user_version = {SCHEMA_VERSION}"),
        )?),
        "utf-16 database with our version" => {
            Ok(Connection::open(path)?.execute_batch(&format!(
                "PRAGMA encoding = 'UTF-16le'; CREATE TABLE notes (body TEXT);
             PRAGMA user_version = {SCHEMA_VERSION}"
            ))?)
        }
        "dropped check constraint" => mutate(&rewritten_schema(
            "artifacts",
            "CHECK (current_revision >= 1)",
            "",
        )),
        "dropped foreign key" => mutate(&rewritten_schema(
            "revisions",
            " REFERENCES artifacts (id)",
            "",
        )),
        "dropped autoincrement" => mutate(&rewritten_schema("changes", " AUTOINCREMENT", "")),
        "changed column default" => mutate(&rewritten_schema(
            "artifacts",
            "DEFAULT 0 CHECK (pinned",
            "DEFAULT 1 CHECK (pinned",
        )),
        "missing table" => mutate("DROP TABLE request_keys"),
        "missing index" => mutate("DROP INDEX artifacts_by_owner"),
        "missing token index" => mutate("DROP INDEX idx_tokens_secret_hash"),
        "extra table" => mutate("CREATE TABLE extra (x TEXT)"),
        "missing feed row" => mutate("DELETE FROM feed"),
        "malformed tokens table" => mutate(
            "DROP TABLE tokens;
             CREATE TABLE tokens (id TEXT PRIMARY KEY, name TEXT NOT NULL,
                 secret_hash TEXT NOT NULL UNIQUE, prefix TEXT NOT NULL,
                 created_at TEXT NOT NULL) STRICT;
             CREATE INDEX idx_tokens_secret_hash ON tokens (secret_hash)",
        ),
        "integrity check failure" => mutate(
            "PRAGMA ignore_check_constraints = ON;
             INSERT INTO blobs VALUES (zeroblob(5), x'00')",
        ),
        "foreign key violation" => mutate(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO request_keys VALUES ('owner', 'k', zeroblob(32), 'art_missing', 1, 1, 1)",
        ),
        "truncated file" => {
            let store = SqliteStore::open(path)?;
            let owner = actor("owner");
            for index in 0..40 {
                create_demo(
                    &store,
                    &owner,
                    &key(&format!("t{index}")),
                    "T",
                    &"x".repeat(2000),
                )?;
            }
            Connection::open(path)?.pragma_update(None, "journal_mode", "DELETE")?;
            let length = fs::metadata(path)?.len();
            fs::OpenOptions::new()
                .write(true)
                .open(path)?
                .set_len(length / 2)?;
            Ok(())
        }
        "failed migration step" => {
            historical_database(path, 2)?;
            Connection::open(path)?.execute_batch("CREATE TABLE request_keys_v3 (x TEXT)")?;
            Ok(())
        }
        "failed last migration step" => {
            historical_database(path, 5)?;
            Connection::open(path)?
                .execute_batch("DROP TABLE tokens; CREATE TABLE idx_tokens_secret_hash (x TEXT)")?;
            Ok(())
        }
        other => panic!("unknown case {other}"),
    }
}

const REFUSED: [&str; 20] = [
    "newer schema",
    "empty file",
    "unrelated database",
    "unrelated database with our version",
    "utf-16 database with our version",
    "dropped check constraint",
    "dropped foreign key",
    "dropped autoincrement",
    "changed column default",
    "missing table",
    "missing index",
    "missing token index",
    "extra table",
    "missing feed row",
    "malformed tokens table",
    "integrity check failure",
    "foreign key violation",
    "truncated file",
    "failed migration step",
    "failed last migration step",
];

#[test]
fn backup_refuses_every_bad_database_and_leaves_nothing_behind() -> TestResult {
    // A refused database must not leave a stray file in the working directory
    // either (a path misread by SQLite once did: decision 0027, review 2).
    let working_dir = std::env::current_dir()?;
    let before_cwd = files_of(&working_dir);
    for case in REFUSED {
        let layout = Layout::new();
        refusable(case, &layout.database())?;
        let source = files_of(&layout.data());

        refused(&backup(&layout.data(), &layout.output()), case);

        assert_eq!(
            files_of(&layout.data()),
            source,
            "{case}: the source changed"
        );
        assert!(
            files_of(&layout.out_dir()).is_empty(),
            "{case}: left files behind"
        );
    }
    assert_eq!(
        files_of(&working_dir),
        before_cwd,
        "a file appeared in the working directory"
    );
    Ok(())
}

#[test]
fn restore_refuses_every_bad_database_and_leaves_no_directory() -> TestResult {
    for case in REFUSED {
        let layout = Layout::new();
        refusable(case, &layout.database())?;
        let input = layout.out_dir().join("input.sqlite3");
        fs::rename(layout.database(), &input)?;
        let before = files_of(&layout.out_dir());

        refused(&restore(&input, &layout.target()), case);

        assert_eq!(
            files_of(&layout.out_dir()),
            before,
            "{case}: the input changed"
        );
        assert!(!layout.target().exists(), "{case}: left the target behind");
        let leftovers: Vec<_> = files_of(layout.root.path())
            .into_keys()
            .filter(|name| name.starts_with(SCRATCH_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "{case}: scratch left: {leftovers:?}");
    }
    Ok(())
}

#[test]
fn a_backup_of_a_missing_database_fails_without_creating_anything() {
    let layout = Layout::new();
    let error = error_of(backup(&layout.data(), &layout.output()), "no database");
    assert!(matches!(error, BackupError::Failed(_)), "{error}");
    assert!(!layout.database().exists());
    assert!(files_of(&layout.out_dir()).is_empty());
}

/// A stopped server's cleanly closed database is left as it was, whichever
/// journal mode it was last in.
#[test]
fn backing_up_a_cleanly_closed_database_leaves_it_untouched() -> TestResult {
    for mode in ["WAL", "DELETE"] {
        let layout = Layout::new();
        let store = SqliteStore::open(&layout.database())?;
        create_demo(&store, &actor("owner"), &key("a"), "A", "<h1>a</h1>")?;
        Connection::open(layout.database())?.pragma_update(None, "journal_mode", mode)?;
        let before = files_of(&layout.data());

        backup(&layout.data(), &layout.output())?;

        assert_eq!(files_of(&layout.data()), before, "{mode}");
    }
    Ok(())
}

#[test]
fn a_backup_is_one_private_file_without_scratch_leftovers() -> TestResult {
    let layout = Layout::new();
    let store = SqliteStore::open(&layout.database())?;
    create_demo(&store, &actor("owner"), &key("a"), "A", "<h1>a</h1>")?;

    let report = backup(&layout.data(), &layout.output())?;

    let listing = files_of(&layout.out_dir());
    assert_eq!(listing.keys().collect::<Vec<_>>(), ["backup.sqlite3"]);
    let bytes = fs::read(layout.output())?;
    assert_eq!(report.bytes, bytes.len() as u64);
    assert_eq!(report.schema_version, SCHEMA_VERSION);
    // Bytes 18 and 19 are 1 for a rollback-journal file: it needs no -wal.
    assert_eq!(bytes[18..20], [1, 1]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(layout.output())?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    Ok(())
}

#[test]
fn an_older_backup_keeps_its_version_and_a_restore_upgrades_it() -> TestResult {
    for version in 1..=5 {
        let layout = Layout::new();
        historical_database(&layout.database(), version)?;
        let source = files_of(&layout.data());

        let saved = backup(&layout.data(), &layout.output())?;

        assert_eq!(saved.schema_version, version);
        assert_eq!(user_version(&layout.output())?, version);
        assert_eq!(
            files_of(&layout.data()),
            source,
            "v{version}: the source changed"
        );
        let restored = restore(&layout.output(), &layout.target())?;
        assert_eq!(restored.schema_version, SCHEMA_VERSION);
        assert_eq!(
            user_version(&database_in(&layout.target()))?,
            SCHEMA_VERSION
        );
        let store = SqliteStore::open(&database_in(&layout.target()))?;
        let old = store
            .artifact(&old_artifact())?
            .ok_or("artifact restored")?;
        assert_eq!(old.title.as_str(), expected_artifact_name(version));
        let body =
            store.revision_file(&old_artifact(), SourceRevision(2), &parsed("index.html"))?;
        assert_eq!(body.as_deref(), Some(OLD_BODY.as_bytes()));
    }
    Ok(())
}

#[test]
fn nothing_existing_at_the_destination_is_replaced() -> TestResult {
    let layout = Layout::new();
    SqliteStore::open(&layout.database())?;
    fs::write(layout.output(), b"precious")?;
    let error = error_of(backup(&layout.data(), &layout.output()), "replaced");
    assert!(matches!(error, BackupError::Exists(_)), "{error}");
    assert_eq!(fs::read(layout.output())?, b"precious");
    assert_eq!(files_of(&layout.out_dir()).len(), 1, "no scratch left");

    #[cfg(unix)]
    {
        let link = layout.out_dir().join("link.sqlite3");
        let elsewhere = layout.root.join("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, &link)?;
        let error = error_of(backup(&layout.data(), &link), "followed a link");
        assert!(matches!(error, BackupError::Exists(_)), "{error}");
        assert!(!elsewhere.exists(), "the link was followed");
    }

    // Restore: an existing directory or file is never used.
    fs::write(layout.target().with_extension("file"), b"x")?;
    let input = layout.out_dir().join("good.sqlite3");
    fs::remove_file(layout.output())?;
    backup(&layout.data(), &input)?;
    fs::create_dir(layout.target())?;
    fs::write(layout.target().join("keep"), b"mine")?;
    let before = files_of(&layout.target());
    for target in [layout.target(), layout.target().with_extension("file")] {
        let error = error_of(restore(&input, &target), "existing target");
        assert!(matches!(error, BackupError::Exists(_)), "{error}");
    }
    assert_eq!(files_of(&layout.target()), before);
    Ok(())
}

#[test]
fn an_input_with_a_stale_wal_or_journal_is_refused() -> TestResult {
    for suffix in ["-wal", "-shm", "-journal"] {
        let layout = Layout::new();
        SqliteStore::open(&layout.database())?;
        let input = layout.out_dir().join("in.sqlite3");
        backup(&layout.data(), &input)?;
        fs::write(
            layout.out_dir().join(format!("in.sqlite3{suffix}")),
            b"stale",
        )?;

        refused(&restore(&input, &layout.target()), suffix);

        assert!(!layout.target().exists());
    }
    Ok(())
}

#[test]
fn a_restore_creates_a_private_data_directory_without_a_token_file() -> TestResult {
    let layout = Layout::new();
    SqliteStore::open(&layout.database())?;
    backup(&layout.data(), &layout.output())?;

    restore(&layout.output(), &layout.target())?;

    assert_eq!(
        files_of(&layout.target()).keys().collect::<Vec<_>>(),
        [DATABASE_FILE],
        "only the database; the access token is created by serve"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let directory = fs::metadata(layout.target())?.permissions().mode() & 0o777;
        let file = fs::metadata(database_in(&layout.target()))?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!((directory, file), (0o700, 0o600));
    }
    // The input is only read.
    assert!(layout.output().exists());
    Ok(())
}

#[test]
fn a_restored_database_has_a_new_feed_identity_and_keeps_its_sequence_head() -> TestResult {
    let layout = Layout::new();
    let store = SqliteStore::open(&layout.database())?;
    let owner = actor("owner");
    let created = create_demo(&store, &owner, &key("a"), "A", "<h1>a</h1>")?;
    revise(&store, &created, &key("b"), 1, "<h1>b</h1>")?;
    let original = store.changes(&owner, ChangeSequence(0), 10)?;
    backup(&layout.data(), &layout.output())?;

    restore(&layout.output(), &layout.target())?;

    let restored = SqliteStore::open(&database_in(&layout.target()))?;
    let page = restored.changes(&owner, ChangeSequence(0), 10)?;
    assert_ne!(
        page.feed, original.feed,
        "a cursor of the original feed must not match the restored one"
    );
    assert_eq!(page.head, original.head, "the head must not go back");
    assert_eq!(page.changes.len(), original.changes.len());
    // A second restore of the same backup is another database again.
    let second = layout.root.join("second");
    restore(&layout.output(), &second)?;
    let other = SqliteStore::open(&database_in(&second))?.changes(&owner, ChangeSequence(0), 0)?;
    assert_ne!(other.feed, page.feed);
    // New commits continue the sequence.
    let next = revise(&restored, &created, &key("c"), 2, "<h1>c</h1>")?;
    assert_eq!(next.revision.revision, SourceRevision(3));
    let after = restored.changes(&owner, ChangeSequence(0), 10)?;
    assert_eq!(after.head, ChangeSequence(original.head.0 + 1));
    Ok(())
}

#[test]
fn request_keys_replay_after_a_restore() -> TestResult {
    let layout = Layout::new();
    let store = SqliteStore::open(&layout.database())?;
    let owner = actor("owner");
    let created = create_demo(&store, &owner, &key("a"), "A", "<h1>a</h1>")?;
    let second = revise(&store, &created, &key("b"), 1, "<h1>b</h1>")?;
    backup(&layout.data(), &layout.output())?;
    restore(&layout.output(), &layout.target())?;
    let restored = SqliteStore::open(&database_in(&layout.target()))?;

    let replay = revise(&restored, &created, &key("b"), 1, "<h1>b</h1>")?;

    assert_eq!(replay.delivery, Delivery::Replayed);
    assert_eq!(replay.revision, second.revision);
    assert_eq!(replay.artifact, second.artifact);
    assert!(matches!(
        revise(&restored, &created, &key("b"), 1, "<h1>other</h1>"),
        Err(StoreError::RequestKeyReused)
    ));
    let first_again = create_demo(&restored, &owner, &key("a"), "A", "<h1>a</h1>")?;
    assert_eq!(first_again.delivery, Delivery::Replayed);
    assert_eq!(first_again.artifact.id, created.artifact.id);
    Ok(())
}

struct Writer {
    handle: thread::JoinHandle<Result<u64, String>>,
    progress: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl Writer {
    /// Commits revisions 2, 3, ... of `live` until stopped or `limit`.
    fn start(store: &SqliteStore, live: &CommitReceipt, limit: u64) -> Self {
        let progress = Arc::new(AtomicU64::new(1));
        let stop = Arc::new(AtomicBool::new(false));
        let (store, live) = (store.clone(), live.clone());
        let (written_so_far, stopped) = (progress.clone(), stop.clone());
        let handle = thread::spawn(move || {
            let mut written = 1;
            while written < limit && !stopped.load(Ordering::Relaxed) {
                let next = written + 1;
                let body = format!("rev {next}");
                revise(&store, &live, &key(&format!("rev{next}")), written, &body)
                    .map_err(|error| format!("commit {next}: {error}"))?;
                written = next;
                written_so_far.store(written, Ordering::Relaxed);
            }
            Ok(written)
        });
        Self {
            handle,
            progress,
            stop,
        }
    }

    /// Waits for `commits` revisions, but never for a writer that stopped.
    fn wait_for(&self, commits: u64) -> Result<u64, String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.progress.load(Ordering::Relaxed) < commits {
            if self.handle.is_finished() || Instant::now() > deadline {
                self.stop.store(true, Ordering::Relaxed);
                return Err(format!("the writer did not reach {commits} commits"));
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(self.progress.load(Ordering::Relaxed))
    }

    /// Stops the writer and returns the last revision it committed.
    fn finish(self) -> Result<u64, String> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle
            .join()
            .map_err(|_| "the writer panicked".to_owned())?
    }
}

/// Every restored revision of `live` is the writer's, in order, with its
/// bytes and digest. Returns how many there are.
fn assert_history_prefix(restored: &SqliteStore, live: &CommitReceipt) -> Result<u64, StoreError> {
    let mut revisions = restored.revisions(&live.artifact.id)?;
    // Listed newest first.
    revisions.sort_by_key(|revision| revision.revision);
    for (index, revision) in revisions.iter().enumerate() {
        let number = index as u64 + 1;
        assert_eq!(revision.revision, SourceRevision(number), "contiguous");
        let body = format!("rev {number}");
        let entry = revision
            .files
            .iter()
            .find(|file| file.path.as_str() == "index.html")
            .unwrap_or_else(|| panic!("revision {number} lists index.html"));
        assert_eq!(
            (entry.size, entry.sha256),
            (body.len() as u64, digest(&body))
        );
        let bytes =
            restored.revision_file(&live.artifact.id, revision.revision, &parsed("index.html"))?;
        assert_eq!(bytes.as_deref(), Some(body.as_bytes()), "revision {number}");
        let style =
            restored.revision_file(&live.artifact.id, revision.revision, &parsed("style.css"))?;
        assert_eq!(style.as_deref(), Some(b"h1{}".as_slice()));
    }
    Ok(revisions.len() as u64)
}

/// A backup taken while a writer commits is a state the database had: a
/// contiguous, non-empty prefix of the writer's history, with each file's
/// bytes and digest, and everything else.
#[test]
fn a_backup_during_writes_restores_a_contiguous_prefix_of_history() -> TestResult {
    let layout = Layout::new();
    let store = SqliteStore::open(&layout.database())?;
    let tokens = SqliteTokenStore::open(&layout.database())?;
    let owner = actor("owner");
    tokens.create(&token(1))?;
    tokens.create(&token(2))?;
    tokens.revoke(&token(2).id, "2026-02-01T00:00:00Z")?;
    tokens.touch(&token(1).id, "2026-03-01T00:00:00Z")?;
    let side = create_demo(&store, &owner, &key("side"), "Side", "<h1>side</h1>")?;
    update_metadata(
        &store,
        &side,
        &key("rename"),
        1,
        Some("Side renamed"),
        Some(true),
        None,
    )?;
    let trashed = update_metadata(&store, &side, &key("trash"), 2, None, None, Some(true))?;
    let live = create_demo(&store, &owner, &key("live"), "Live", "rev 1")?;
    let writer = Writer::start(&store, &live, 5000);
    let before_backup = writer.wait_for(20)?;

    let saved = backup(&layout.data(), &layout.output());

    let written = writer.finish()?;
    saved?;
    restore(&layout.output(), &layout.target())?;
    let restored_path = database_in(&layout.target());
    let restored = SqliteStore::open(&restored_path)?;

    let count = assert_history_prefix(&restored, &live)?;
    assert!(
        (before_backup..=written).contains(&count),
        "{count} revisions restored, expected {before_backup}..={written}"
    );
    let record = restored
        .artifact(&live.artifact.id)?
        .ok_or("live artifact")?;
    assert_eq!(record.current_revision, SourceRevision(count));
    // The writer's later commits are absent and their keys are free.
    let next = count + 1;
    let body = format!("rev {next}");
    let continued = revise(&restored, &live, &key(&format!("rev{next}")), count, &body)?;
    assert_eq!(continued.delivery, Delivery::Committed);
    // Key replay of an early commit, and a conflict for other content.
    let replay = revise(&restored, &live, &key("rev5"), 4, "rev 5")?;
    assert_eq!(replay.delivery, Delivery::Replayed);
    assert_eq!(replay.revision.revision, SourceRevision(5));
    assert!(matches!(
        revise(&restored, &live, &key("rev5"), 4, "other"),
        Err(StoreError::RequestKeyReused)
    ));

    // Metadata and trash state.
    let side_now = restored
        .artifact(&side.artifact.id)?
        .ok_or("side artifact")?;
    assert_eq!(side_now, trashed.artifact);
    assert_eq!(side_now.title.as_str(), "Side renamed");
    assert!(side_now.pinned && side_now.deleted_at.is_some());
    let in_trash = restored.list_artifacts(ArtifactListing {
        owner: &owner,
        search: None,
        pinned: None,
        deleted: true,
        order: ListOrder::Updated,
        after: None,
        limit: 10,
    })?;
    assert_eq!(in_trash.len(), 1);

    // Token records, including revocation and last use.
    let records = SqliteTokenStore::open(&restored_path)?.list()?;
    assert_eq!(records, tokens.list()?);
    assert_eq!(records.len(), 2);
    assert!(records.iter().any(|record| record.revoked_at.is_some()));
    assert!(records.iter().any(|record| record.last_used_at.is_some()));
    Ok(())
}

/// The rollback-journal header bytes 18 and 19 are 1; WAL files have 2.
fn journal_header(path: &Path) -> Result<[u8; 2], Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    Ok([bytes[18], bytes[19]])
}

/// Both commands publish a self-contained rollback-journal file whatever
/// the input's version or journal mode was (the server switches to WAL when
/// it opens it).
#[test]
fn published_files_are_in_the_rollback_journal_mode() -> TestResult {
    for version in [None, Some(3)] {
        let layout = Layout::new();
        match version {
            None => {
                let store = SqliteStore::open(&layout.database())?;
                create_demo(&store, &actor("owner"), &key("a"), "A", "<h1>a</h1>")?;
            }
            Some(version) => historical_database(&layout.database(), version)?,
        }
        // The source is WAL (current) or rollback (old); a raw copy keeps that.
        let raw = layout.out_dir().join("raw.sqlite3");
        fs::copy(layout.database(), &raw)?;
        let raw_header = journal_header(&raw)?;
        assert_eq!(raw_header, if version.is_none() { [2, 2] } else { [1, 1] });

        backup(&layout.data(), &layout.output())?;
        assert_eq!(journal_header(&layout.output())?, [1, 1], "{version:?}");

        for (name, input) in [("backup", layout.output()), ("raw", raw)] {
            let target = layout.root.path().join(format!("restored-{name}"));
            restore(&input, &target)?;
            let restored = database_in(&target);
            assert_eq!(journal_header(&restored)?, [1, 1], "{name} {version:?}");
            let mode: String =
                Connection::open(&restored)?
                    .pragma_query_value(None, "journal_mode", |row| row.get(0))?;
            assert_eq!(mode, "delete", "{name} {version:?}");
            assert_eq!(files_of(&target).len(), 1, "no sidecar files");
        }
    }
    Ok(())
}

/// A backup kept read-only (mode 0400) is a valid input, at the current
/// version and at an older one that needs an upgrade.
#[cfg(unix)]
#[test]
fn a_read_only_backup_restores() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    for version in [None, Some(3)] {
        let layout = Layout::new();
        match version {
            None => {
                let store = SqliteStore::open(&layout.database())?;
                create_demo(&store, &actor("owner"), &key("a"), "A", "<h1>a</h1>")?;
            }
            Some(version) => historical_database(&layout.database(), version)?,
        }
        backup(&layout.data(), &layout.output())?;
        fs::set_permissions(layout.output(), fs::Permissions::from_mode(0o400))?;

        let report = restore(&layout.output(), &layout.target());

        let report = report.map_err(|error| format!("{version:?}: {error}"))?;
        assert_eq!(report.schema_version, SCHEMA_VERSION);
        assert_eq!(
            user_version(&database_in(&layout.target()))?,
            SCHEMA_VERSION
        );
        let mode = fs::metadata(layout.output())?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o400, "the input is only read");
    }
    Ok(())
}

/// Directory names are bytes on Unix: a backup into one, and a restore below
/// one, work although the name is not UTF-8.
#[cfg(unix)]
#[test]
fn directories_that_are_not_utf8_work() -> TestResult {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let layout = Layout::new();
    let store = SqliteStore::open(&layout.database())?;
    create_demo(&store, &actor("owner"), &key("a"), "A", "<h1>a</h1>")?;
    let odd = layout
        .root
        .path()
        .join(OsString::from_vec(vec![b'o', 0xff, 0xfe]));
    fs::create_dir(&odd)?;
    let output = odd.join("backup.sqlite3");

    backup(&layout.data(), &output)?;

    assert_eq!(files_of(&odd).len(), 1, "only the backup, no scratch");
    let target = odd.join("restored");
    restore(&output, &target)?;
    let restored = SqliteStore::open(&database_in(&target))?;
    assert!(restored.artifact(&store_artifact_id(&store)?)?.is_some());
    Ok(())
}

#[cfg(unix)]
fn store_artifact_id(
    store: &SqliteStore,
) -> Result<artifree_domain::identity::ArtifactId, StoreError> {
    let page = store.list_artifacts(ArtifactListing {
        owner: &actor("owner"),
        search: None,
        pinned: None,
        deleted: false,
        order: ListOrder::Updated,
        after: None,
        limit: 10,
    })?;
    page.first()
        .map(|record| record.id.clone())
        .ok_or(StoreError::NotFound)
}

/// A restore target below a file is reported as what it is, not as an
/// existing target.
#[test]
fn a_restore_target_below_a_file_names_the_real_cause() -> TestResult {
    let layout = Layout::new();
    SqliteStore::open(&layout.database())?;
    backup(&layout.data(), &layout.output())?;
    let file = layout.root.join("notes.txt");
    fs::write(&file, b"mine")?;

    let error = error_of(restore(&layout.output(), &file.join("new")), "below a file");

    assert!(matches!(error, BackupError::Failed(_)), "{error:?}");
    // Not `Exists`; the operating system's own text may mention existing files.
    assert!(error.to_string().contains("notes.txt"), "{error}");
    assert_eq!(fs::read(&file)?, b"mine");
    Ok(())
}

/// The schema is compared by what `SQLite` reports about it, not by the text it
/// stores: a different library version may rewrite that text (for example the
/// quoting after `ALTER TABLE ... RENAME`) without changing the schema.
#[test]
fn a_schema_whose_stored_text_differs_but_whose_structure_is_the_same_is_accepted() -> TestResult {
    let layout = Layout::new();
    current_database(&layout.database())?;
    Connection::open(layout.database())?.execute_batch(&format!(
        "{}{}",
        rewritten_schema("tokens", "CREATE TABLE tokens", "CREATE TABLE   \"tokens\""),
        rewritten_schema("changes", "sequence INTEGER", "\"sequence\"   INTEGER"),
    ))?;

    backup(&layout.data(), &layout.output())?;
    restore(&layout.output(), &layout.target())?;
    Ok(())
}

/// The "left byte for byte" comparisons rest on `files_of`: a file it cannot
/// read must stop the test, not compare equal as empty.
#[cfg(unix)]
#[test]
fn files_of_does_not_turn_an_unreadable_file_into_empty_bytes() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new();
    let file = dir.join("secret");
    fs::write(&file, b"data")?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000))?;
    if fs::read(&file).is_ok() {
        // Root reads everything, so the failure cannot be provoked.
        return Ok(());
    }

    let outcome = std::panic::catch_unwind(|| files_of(dir.path()));

    let Err(payload) = outcome else {
        panic!("files_of returned for an unreadable file");
    };
    let Some(message) = payload.downcast_ref::<String>() else {
        panic!("files_of panicked without a formatted message");
    };
    assert!(
        message.contains("cannot read") && message.contains("secret"),
        "{message}"
    );
    Ok(())
}
