//! Upgrading a frozen schema 12 file to schema 13, which records job changes in
//! the change feed (decision 0055, rule 7 and acceptance 22).

mod common;

use std::path::Path;

use artifree_domain::SourceRevision;
use artifree_domain::identity::UnixMillis;
use artifree_domain::job::{JobId, JobKind, JobState};
use artifree_ports::artifact_store::{Delivery, RequestFingerprint, RequestIdentity};
use artifree_ports::job_store::{JobCancel, JobStore, NewJob};
use artifree_providers::storage::{SCHEMA_VERSION, SqliteStore};
use common::{
    OLD_ID, TRASHED_ID, TempDir, TestResult, actor, current_database, database_in, files_of,
    historical_database, key, old_artifact, parsed, schema_of, user_version,
};
use rusqlite::{Connection, params};

/// The change table as schema 11 made it, which the frozen version 12 file
/// holds. Copied verbatim from `DATA_CHANGE_SCHEMA` at commit fbca5ec: the live
/// ladder may change, this copy must not.
const STEP_11: &str = "
CREATE TABLE changes_v11 (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    actor TEXT NOT NULL,
    actor_seq INTEGER NOT NULL CHECK (actor_seq >= 1),
    artifact_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN
        ('created', 'revised', 'metadata_updated', 'deleted', 'recovered', 'purged',
         'data_changed')),
    revision INTEGER NOT NULL,
    metadata_revision INTEGER NOT NULL,
    data_version INTEGER,
    committed_at INTEGER NOT NULL,
    UNIQUE (actor, actor_seq),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    FOREIGN KEY (artifact_id, metadata_revision)
        REFERENCES metadata_revisions (artifact_id, metadata_revision),
    CHECK ((kind = 'data_changed') = (data_version IS NOT NULL)),
    CHECK (data_version IS NULL OR data_version >= 1)
) STRICT;
INSERT INTO changes_v11 (sequence, actor, actor_seq, artifact_id, kind, revision,
        metadata_revision, committed_at)
    SELECT sequence, actor, actor_seq, artifact_id, kind, revision, metadata_revision,
        committed_at
    FROM changes;
DROP TABLE changes;
ALTER TABLE changes_v11 RENAME TO changes;
";

/// The asset tables as schema 12 made them. Copied verbatim from `ASSET_SCHEMA`
/// at commit fbca5ec.
const STEP_12: &str = "
CREATE TABLE asset_ids (id TEXT PRIMARY KEY CHECK (length(id) = 64)) STRICT;
CREATE TABLE asset_spaces (
 artifact_id TEXT NOT NULL REFERENCES artifacts(id), actor TEXT NOT NULL,
 asset_version INTEGER NOT NULL CHECK (asset_version >= 1),
 PRIMARY KEY (artifact_id, actor)
) STRICT, WITHOUT ROWID;
CREATE TABLE assets (
 artifact_id TEXT NOT NULL REFERENCES artifacts(id), actor TEXT NOT NULL,
 id TEXT NOT NULL REFERENCES asset_ids(id), content_type TEXT NOT NULL,
 size INTEGER NOT NULL CHECK (size >= 1 AND size <= 5242880),
 digest BLOB NOT NULL REFERENCES blobs(digest), created_at INTEGER NOT NULL,
 PRIMARY KEY (artifact_id, actor, id)
) STRICT, WITHOUT ROWID;
CREATE INDEX assets_by_actor ON assets(actor, artifact_id);
CREATE INDEX assets_by_digest ON assets(digest);
CREATE TABLE asset_requests (
 actor TEXT NOT NULL, request_key TEXT NOT NULL,
 fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
 artifact_id TEXT NOT NULL REFERENCES artifacts(id),
 asset_version INTEGER NOT NULL CHECK (asset_version >= 0),
 asset_id TEXT, content_type TEXT, size INTEGER, digest BLOB, uploaded_at INTEGER,
 deleted INTEGER NOT NULL CHECK (deleted IN (0,1)), created_at INTEGER NOT NULL,
 PRIMARY KEY (actor, request_key)
) STRICT, WITHOUT ROWID;
CREATE INDEX asset_requests_by_artifact ON asset_requests(artifact_id);
";

/// The queued job of the version 8 fixture, for revision 1 of the old artifact.
const FIXTURE_JOB: &str = "job_00000000000000000000000000000000";

/// A queued job for revision 2 of the old artifact, which the version 12 build adds.
const SECOND_JOB: &str = "job_11111111111111111111111111111111";

/// A change row of the version 12 build, beyond the two that the version 10
/// fixture holds (`created` at 1 and `revised` at 2). Its actor sequence is
/// its sequence.
struct Row {
    sequence: i64,
    kind: &'static str,
    artifact: &'static str,
    revision: i64,
    metadata_revision: i64,
    data_version: Option<i64>,
    committed_at: i64,
}

/// One row of each kind that the version 11 and 12 builds allow and the
/// fixture lacks. Only `data_changed` carries a data version.
const ROWS: [Row; 5] = [
    Row {
        sequence: 3,
        kind: "metadata_updated",
        artifact: OLD_ID,
        revision: 2,
        metadata_revision: 2,
        data_version: None,
        committed_at: 310,
    },
    Row {
        sequence: 4,
        kind: "data_changed",
        artifact: OLD_ID,
        revision: 2,
        metadata_revision: 2,
        data_version: Some(3),
        committed_at: 320,
    },
    Row {
        sequence: 5,
        kind: "deleted",
        artifact: TRASHED_ID,
        revision: 1,
        metadata_revision: 2,
        data_version: None,
        committed_at: 400,
    },
    Row {
        sequence: 6,
        kind: "recovered",
        artifact: TRASHED_ID,
        revision: 1,
        metadata_revision: 1,
        data_version: None,
        committed_at: 410,
    },
    Row {
        sequence: 7,
        kind: "purged",
        artifact: TRASHED_ID,
        revision: 1,
        metadata_revision: 2,
        data_version: None,
        committed_at: 500,
    },
];

/// The version 12 build of the fixture: the version 10 database with the frozen
/// steps 11 and 12 applied, one row of each kind the fixture lacks, a second
/// queued job, and a feed head (9) above its largest row (7), so that the
/// upgrade must keep the head instead of deriving it from the rows.
fn version_12_with_every_kind(path: &Path) -> TestResult {
    historical_database(path, 10)?;
    let connection = Connection::open(path)?;
    connection.execute_batch(STEP_11)?;
    connection.execute_batch(STEP_12)?;
    let mut insert = connection.prepare(
        "INSERT INTO changes (sequence, actor, actor_seq, artifact_id, kind, revision,
             metadata_revision, data_version, committed_at)
         VALUES (?1, 'owner', ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for row in ROWS {
        insert.execute(params![
            row.sequence,
            row.artifact,
            row.kind,
            row.revision,
            row.metadata_revision,
            row.data_version,
            row.committed_at
        ])?;
    }
    connection.execute_batch(&format!(
        "INSERT INTO jobs (id, actor, artifact_id, revision, kind, state, attempts,
             max_attempts, fence, run_after, cancel_requested, created_at)
         VALUES ('{SECOND_JOB}', 'owner', '{OLD_ID}', 2, 'verify_revision', 'queued', 0, 3,
                 0, 620, 0, 620);
         UPDATE actor_feeds SET head = 9 WHERE actor = 'owner';
         UPDATE sqlite_sequence SET seq = 9 WHERE name = 'changes';"
    ))?;
    connection.pragma_update(None, "user_version", 12)?;
    Ok(())
}

/// Everything the upgrade must keep, by value: each change row's columns as
/// schema 12 has them (the data version included), each actor's head, the global
/// sequence, and the feed's identity.
fn feed_state(path: &Path) -> Result<Vec<String>, rusqlite::Error> {
    let connection = Connection::open(path)?;
    let mut state = Vec::new();
    for query in [
        "SELECT 'row ' || sequence || '|' || actor || '|' || actor_seq || '|' || artifact_id
             || '|' || kind || '|' || revision || '|' || metadata_revision || '|'
             || coalesce(data_version, 'none') || '|' || committed_at
         FROM changes ORDER BY sequence",
        "SELECT 'head ' || actor || '|' || head FROM actor_feeds ORDER BY actor",
        "SELECT 'sequence ' || seq FROM sqlite_sequence WHERE name = 'changes'",
        "SELECT 'feed ' || hex(id) FROM feed",
    ] {
        let mut statement = connection.prepare(query)?;
        for line in statement.query_map([], |row| row.get::<_, String>(0))? {
            state.push(line?);
        }
    }
    Ok(state)
}

/// The change rows after `after`, one line each: sequence, actor sequence,
/// actor, kind, job, artifact, revision, metadata revision and data version.
fn rows_after(path: &Path, after: i64) -> Result<Vec<String>, rusqlite::Error> {
    let connection = Connection::open(path)?;
    let mut statement = connection.prepare(
        "SELECT sequence || '|' || actor_seq || '|' || actor || '|' || kind || '|'
             || coalesce(job_id, 'none') || '|' || artifact_id || '|' || revision || '|'
             || metadata_revision || '|' || coalesce(data_version, 'none')
         FROM changes WHERE sequence > ?1 ORDER BY sequence",
    )?;
    let lines = statement
        .query_map([after], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(lines)
}

/// The single number that a count query returns.
fn count(path: &Path, query: &str) -> Result<i64, rusqlite::Error> {
    Connection::open(path)?.query_row(query, [], |row| row.get(0))
}

/// Acceptance 22: a frozen schema 12 file upgrades to 13 with every change row
/// it held, each row's data version, the feed heads, and no job named by any
/// row, and its schema is the schema of a fresh database.
#[test]
fn a_frozen_version_12_file_upgrades_to_13_keeping_its_rows_heads_and_schema() -> TestResult {
    let fresh = TempDir::new();
    current_database(&database_in(fresh.path()))?;
    let fresh_schema = schema_of(&database_in(fresh.path()))?;

    let dir = TempDir::new();
    let path = database_in(dir.path());
    version_12_with_every_kind(&path)?;
    assert_eq!(user_version(&path)?, 12);
    let before = feed_state(&path)?;
    assert_eq!(
        before
            .iter()
            .filter(|line| line.starts_with("row "))
            .count(),
        7,
        "one row of each of the seven kinds"
    );
    assert!(
        before.iter().any(|line| line == "sequence 9"),
        "the head must sit above the largest row: {before:?}"
    );
    let jobs = count(&path, "SELECT count(*) FROM jobs")?;
    assert_eq!(jobs, 2, "the fixture job and the second job");

    SqliteStore::open(&path)
        .map_err(|error| format!("the version 12 file must upgrade: {error}"))?;

    assert_eq!(user_version(&path)?, SCHEMA_VERSION);
    assert_eq!(
        schema_of(&path)?,
        fresh_schema,
        "the upgraded schema must be the fresh one"
    );
    assert_eq!(
        feed_state(&path)?,
        before,
        "each row with its data version, each head, the sequence, and the feed identity"
    );
    assert_eq!(
        count(&path, "SELECT count(job_id) FROM changes")?,
        0,
        "no row from before names a job"
    );
    assert_eq!(
        count(&path, "SELECT count(*) FROM jobs")?,
        jobs,
        "the jobs are kept"
    );
    Ok(())
}

/// After the upgrade, each job transition appends one `job_changed` row after
/// the old head, in the owner's feed: a job that is enqueued, then a job that is
/// cancelled.
#[test]
fn a_job_transition_after_the_upgrade_appends_a_job_changed_row_after_the_old_head() -> TestResult {
    let dir = TempDir::new();
    let path = database_in(dir.path());
    version_12_with_every_kind(&path)?;
    let store = SqliteStore::open(&path)?;
    let owner = actor("owner");
    let queued: JobId = parsed(FIXTURE_JOB);

    let enqueued = store.enqueue_job(NewJob {
        request: RequestIdentity {
            actor: &owner,
            key: &key("enqueued-after-upgrade"),
            fingerprint: RequestFingerprint([4; 32]),
        },
        artifact: &old_artifact(),
        revision: SourceRevision(2),
        kind: JobKind::VerifyRevision,
        now: UnixMillis(1_000),
    })?;
    assert_eq!(enqueued.delivery, Delivery::Committed);
    let cancelled = store.cancel_job(JobCancel {
        request: RequestIdentity {
            actor: &owner,
            key: &key("cancelled-after-upgrade"),
            fingerprint: RequestFingerprint([5; 32]),
        },
        job: &queued,
        now: UnixMillis(1_000),
    })?;
    assert_eq!(cancelled.job.state, JobState::Cancelled);

    let created_job = enqueued.job.id.as_str();
    assert_eq!(
        rows_after(&path, 9)?,
        [
            format!("10|10|owner|job_changed|{created_job}|{OLD_ID}|2|2|none"),
            format!("11|11|owner|job_changed|{FIXTURE_JOB}|{OLD_ID}|2|2|none"),
        ]
    );
    Ok(())
}

/// A version 13 step that fails leaves the version 12 file as it was, in both
/// journal modes: a table already holds the new name, so the open is refused.
#[test]
fn a_failing_version_13_step_leaves_the_version_12_file_untouched() -> TestResult {
    for wal in [false, true] {
        let dir = TempDir::new();
        let path = database_in(dir.path());
        version_12_with_every_kind(&path)?;
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch("CREATE TABLE changes_v13 (x TEXT);")?;
            if wal {
                connection.pragma_update(None, "journal_mode", "WAL")?;
            }
        }
        let before = files_of(dir.path());
        let error = SqliteStore::open(&path)
            .err()
            .ok_or_else(|| format!("wal {wal}: the open was accepted"))?;
        assert!(
            error
                .to_string()
                .contains("table changes_v13 already exists"),
            "wal {wal}: {error}"
        );
        assert_eq!(
            files_of(dir.path()),
            before,
            "wal {wal}: the refused open changed a file"
        );
        assert_eq!(user_version(&path)?, 12, "wal {wal}");
    }
    Ok(())
}
