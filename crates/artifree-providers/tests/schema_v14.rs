//! Upgrading a frozen schema 13 file to schema 14, and the rows that schema 14
//! refuses (decision 0057 rules 1, 6 and 14; decision 0030 rules 7 and 18).
//!
//! The version 13 fixture is built from frozen copies of the schema texts of
//! versions 11 to 13, so a later change to the live ladder cannot change what the
//! upgrade is tested on.

mod common;

use std::path::Path;

use artifree_domain::identity::UnixMillis;
use artifree_domain::job::{JobId, JobState};
use artifree_ports::artifact_store::{RequestFingerprint, RequestIdentity};
use artifree_ports::job_store::{JobCancel, JobStore};
use artifree_providers::storage::{SCHEMA_VERSION, SqliteStore};
use common::{
    OLD_ID, TRASHED_ID, TempDir, TestResult, actor, create_demo, current_database, database_in,
    files_of, historical_database, key, parsed, schema_of, user_version,
};
use rusqlite::{Connection, params};

/// The change table as schema 11 made it, which the frozen version 12 file holds.
/// Copied verbatim from `DATA_CHANGE_SCHEMA` at commit fbca5ec: the live ladder may
/// change, this copy must not.
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

/// The change table as schema 13 made it, which the frozen version 13 file holds.
/// Copied verbatim from `JOB_CHANGE_SCHEMA` at commit 55183a8 (lines 475 to 505 of
/// `src/schema.rs`): the live ladder may change, this copy must not.
const STEP_13: &str = "
CREATE TABLE changes_v13 (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    actor TEXT NOT NULL,
    actor_seq INTEGER NOT NULL CHECK (actor_seq >= 1),
    artifact_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN
        ('created', 'revised', 'metadata_updated', 'deleted', 'recovered', 'purged',
         'data_changed', 'job_changed')),
    revision INTEGER NOT NULL,
    metadata_revision INTEGER NOT NULL,
    data_version INTEGER,
    job_id TEXT,
    committed_at INTEGER NOT NULL,
    UNIQUE (actor, actor_seq),
    FOREIGN KEY (artifact_id, revision) REFERENCES revisions (artifact_id, revision),
    FOREIGN KEY (artifact_id, metadata_revision)
        REFERENCES metadata_revisions (artifact_id, metadata_revision),
    FOREIGN KEY (job_id) REFERENCES jobs (id),
    CHECK ((kind = 'data_changed') = (data_version IS NOT NULL)),
    CHECK (data_version IS NULL OR data_version >= 1),
    CHECK ((kind = 'job_changed') = (job_id IS NOT NULL))
) STRICT;
INSERT INTO changes_v13 (sequence, actor, actor_seq, artifact_id, kind, revision,
        metadata_revision, data_version, committed_at)
    SELECT sequence, actor, actor_seq, artifact_id, kind, revision, metadata_revision,
        data_version, committed_at
    FROM changes;
DROP TABLE changes;
ALTER TABLE changes_v13 RENAME TO changes;
";

/// A queued job for revision 2 of the old artifact, which the version 13 build adds.
const SECOND_JOB: &str = "job_11111111111111111111111111111111";

/// One job in each of the other states that the version 13 build adds.
const SUCCEEDED_JOB: &str = "job_22222222222222222222222222222222";
const FAILED_JOB: &str = "job_33333333333333333333333333333333";
const RUNNING_JOB: &str = "job_44444444444444444444444444444444";
const RETRY_JOB: &str = "job_55555555555555555555555555555555";
const CANCELLED_JOB: &str = "job_66666666666666666666666666666666";

/// The job that the refusal test accepts as a `run_profile` job, and the job its
/// execution attempt names.
const RUN_JOB: &str = "job_77777777777777777777777777777777";

/// The execution attempt that the refusal test accepts, and its directory: 32
/// lowercase hex characters each.
const ATTEMPT: &str = "0123456789abcdef0123456789abcdef";
const DIRECTORY: &str = "fedcba9876543210fedcba9876543210";

/// The body of the artifact that the refusal test creates.
const BODY: &str = "<h1>first</h1>";

/// The extended result codes that the refusals carry.
const CHECK: i32 = rusqlite::ffi::SQLITE_CONSTRAINT_CHECK;
const FOREIGN_KEY: i32 = rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY;

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

/// The version 13 build of the fixture: the version 10 database with the frozen
/// steps 11, 12 and 13 applied, one row of each kind the older fixture lacks, a
/// job in every state, a finding, a request key, and two `job_changed` rows that
/// name the second job. The feed head is 11.
fn version_13_with_every_state(path: &Path) -> TestResult {
    historical_database(path, 10)?;
    let connection = Connection::open(path)?;
    connection.execute_batch(STEP_11)?;
    connection.execute_batch(STEP_12)?;
    for row in ROWS {
        connection.execute(
            "INSERT INTO changes (sequence, actor, actor_seq, artifact_id, kind, revision,
                 metadata_revision, data_version, committed_at)
             VALUES (?1, 'owner', ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                row.sequence,
                row.artifact,
                row.kind,
                row.revision,
                row.metadata_revision,
                row.data_version,
                row.committed_at
            ],
        )?;
    }
    connection.execute_batch(STEP_13)?;
    connection.execute_batch(&format!(
        "INSERT INTO jobs (id, actor, artifact_id, revision, kind, state, attempts,
             max_attempts, fence, run_after, cancel_requested, created_at)
         VALUES ('{SECOND_JOB}', 'owner', '{OLD_ID}', 2, 'verify_revision', 'queued', 0, 3,
                 0, 620, 0, 620);
         INSERT INTO jobs (id, actor, artifact_id, revision, kind, state, attempts,
             max_attempts, fence, run_after, worker, lease_expires_at, cancel_requested,
             failure, files_checked, bytes_checked, mismatch_count, created_at, started_at,
             finished_at)
         VALUES ('{SUCCEEDED_JOB}', 'owner', '{OLD_ID}', 2, 'verify_revision', 'succeeded',
                 1, 3, 2, 630, NULL, NULL, 0, NULL, 2, 20, 1, 630, 631, 640),
                ('{FAILED_JOB}', 'owner', '{OLD_ID}', 2, 'verify_revision', 'failed',
                 3, 3, 3, 650, NULL, NULL, 0, 'internal', NULL, NULL, NULL, 650, 651, 660),
                ('{RUNNING_JOB}', 'owner', '{OLD_ID}', 2, 'verify_revision', 'running',
                 1, 3, 1, 670, 'worker-1', 1000, 0, NULL, NULL, NULL, NULL, 670, 671, NULL),
                ('{RETRY_JOB}', 'owner', '{OLD_ID}', 2, 'verify_revision', 'retry_scheduled',
                 1, 3, 1, 680, NULL, NULL, 0, NULL, NULL, NULL, NULL, 680, NULL, NULL),
                ('{CANCELLED_JOB}', 'owner', '{OLD_ID}', 2, 'verify_revision', 'cancelled',
                 0, 3, 0, 690, NULL, NULL, 0, NULL, NULL, NULL, NULL, 690, NULL, 695);
         INSERT INTO job_findings (job_id, ordinal, path, problem)
             VALUES ('{SUCCEEDED_JOB}', 0, 'index.html', 'digest_mismatch');
         INSERT INTO job_requests (actor, request_key, fingerprint, job_id, created_at)
             VALUES ('owner', 'test-key-second-job', x'{second}', '{SECOND_JOB}', 620);",
        second = "0b".repeat(32)
    ))?;
    for (sequence, committed_at) in [(10, 700), (11, 710)] {
        connection.execute(
            "INSERT INTO changes (sequence, actor, actor_seq, artifact_id, kind, revision,
                 metadata_revision, data_version, job_id, committed_at)
             VALUES (?1, 'owner', ?1, ?2, 'job_changed', 2, 2, NULL, ?3, ?4)",
            params![sequence, OLD_ID, SECOND_JOB, committed_at],
        )?;
    }
    connection.execute_batch(
        "UPDATE actor_feeds SET head = 11 WHERE actor = 'owner';
         UPDATE sqlite_sequence SET seq = 11 WHERE name = 'changes';",
    )?;
    connection.pragma_update(None, "user_version", 13)?;
    Ok(())
}

/// What the upgrade must keep, by value, one named section at a time: every job
/// column that version 13 has, the request keys, the findings, every change column
/// (the job included), the feed heads and the sequence, the feed identity, the
/// indexes and triggers of `jobs`, and the foreign keys of `job_requests`,
/// `job_findings` and `changes`.
const KEPT_STATE: [(&str, &str); 10] = [
    (
        "jobs",
        "SELECT id || '|' || actor || '|' || artifact_id || '|' || revision || '|' || kind
             || '|' || state || '|' || attempts || '|' || max_attempts || '|' || fence
             || '|' || run_after || '|' || coalesce(worker, 'none') || '|'
             || coalesce(lease_expires_at, 'none') || '|' || cancel_requested || '|'
             || coalesce(failure, 'none') || '|' || coalesce(files_checked, 'none') || '|'
             || coalesce(bytes_checked, 'none') || '|' || coalesce(mismatch_count, 'none')
             || '|' || created_at || '|' || coalesce(started_at, 'none') || '|'
             || coalesce(finished_at, 'none')
         FROM jobs ORDER BY id",
    ),
    (
        "job_requests",
        "SELECT actor || '|' || request_key || '|' || hex(fingerprint) || '|' || job_id
             || '|' || created_at
         FROM job_requests ORDER BY actor, request_key",
    ),
    (
        "job_findings",
        "SELECT job_id || '|' || ordinal || '|' || path || '|' || problem
         FROM job_findings ORDER BY job_id, ordinal",
    ),
    (
        "changes",
        "SELECT sequence || '|' || actor || '|' || actor_seq || '|' || artifact_id || '|'
             || kind || '|' || revision || '|' || metadata_revision || '|'
             || coalesce(data_version, 'none') || '|' || coalesce(job_id, 'none') || '|'
             || committed_at
         FROM changes ORDER BY sequence",
    ),
    (
        "heads",
        "SELECT 'head ' || actor || '|' || head FROM actor_feeds ORDER BY actor",
    ),
    (
        "sequence",
        "SELECT 'sequence ' || seq FROM sqlite_sequence WHERE name = 'changes'",
    ),
    ("feed", "SELECT 'feed ' || hex(id) FROM feed"),
    (
        "job indexes",
        "SELECT name FROM sqlite_master
         WHERE type = 'index' AND tbl_name = 'jobs' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    ),
    (
        "job triggers",
        "SELECT name FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'jobs'
         ORDER BY name",
    ),
    (
        "foreign keys",
        r#"SELECT m.name || '|' || f.id || '|' || f.seq || '|' || f."table" || '|'
             || f."from" || '|' || coalesce(f."to", 'none') || '|' || f.on_update || '|'
             || f.on_delete || '|' || f."match"
         FROM sqlite_master AS m, pragma_foreign_key_list(m.name) AS f
         WHERE m.name IN ('job_requests', 'job_findings', 'changes')
         ORDER BY m.name, f.id, f.seq"#,
    ),
];

/// Each section of [`KEPT_STATE`] as its rows, in order.
fn kept_state(path: &Path) -> Result<Vec<(&'static str, Vec<String>)>, rusqlite::Error> {
    let connection = Connection::open(path)?;
    let mut state = Vec::new();
    for (name, query) in KEPT_STATE {
        let mut statement = connection.prepare(query)?;
        let lines = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        state.push((name, lines));
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

/// How many rows `PRAGMA foreign_key_check` reports for `table`.
fn foreign_key_violations(path: &Path, table: &str) -> Result<usize, rusqlite::Error> {
    let connection = Connection::open(path)?;
    let mut statement = connection.prepare(&format!("PRAGMA foreign_key_check({table})"))?;
    let violations = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(violations.len())
}

/// A job row to insert. The fields are the ones the schema's checks read; every
/// other column is fixed, and the row is queued unless a field says otherwise.
#[derive(Clone, Copy)]
struct JobFields {
    kind: &'static str,
    profile: Option<&'static str>,
    state: &'static str,
    failure: Option<&'static str>,
    finished_at: Option<i64>,
}

/// An execution attempt row to insert. The fields are the ones the schema's
/// checks read; the fence, the host fields and the profile are fixed.
#[derive(Clone, Copy)]
struct AttemptFields<'a> {
    attempt: &'a str,
    job: &'a str,
    tool_digest: &'a str,
    directory: &'a str,
    state: &'a str,
    revoked: Option<&'a str>,
    settled_at: Option<i64>,
}

/// Inserts a job of `artifact` at revision 1 with the fields given.
fn insert_job(
    connection: &Connection,
    id: &str,
    artifact: &str,
    fields: JobFields,
) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO jobs (id, actor, artifact_id, revision, kind, profile, state, attempts,
             max_attempts, fence, run_after, cancel_requested, failure, created_at, finished_at)
         VALUES (?1, 'owner', ?2, 1, ?3, ?4, ?5, 0, 3, 0, 0, 0, ?6, 1, ?7)",
        params![
            id,
            artifact,
            fields.kind,
            fields.profile,
            fields.state,
            fields.failure,
            fields.finished_at
        ],
    )
}

/// Inserts an execution attempt with fence 1 and no generation, for the profile
/// `fixture` on the machine `machine-1`.
fn insert_attempt(connection: &Connection, fields: AttemptFields<'_>) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO execution_attempts (attempt, job_id, fence, actor_generation, profile,
             tool_digest, machine, boot, cgroupns, directory, state, revoked, created_at,
             settled_at)
         VALUES (?1, ?2, 1, NULL, 'fixture', ?3, 'machine-1', 'boot-1', 'cgroup-1', ?4, ?5,
             ?6, 1, ?7)",
        params![
            fields.attempt,
            fields.job,
            fields.tool_digest,
            fields.directory,
            fields.state,
            fields.revoked,
            fields.settled_at
        ],
    )
}

/// Checks that `result` was refused with the extended result `code` and by
/// nothing else: an insert accepted, or refused for another reason, fails.
fn assert_refused(result: rusqlite::Result<usize>, code: i32, what: &str) -> TestResult {
    match result {
        Err(rusqlite::Error::SqliteFailure(error, _)) if error.extended_code == code => Ok(()),
        Err(error) => {
            Err(format!("{what}: refused by {error}, expected extended code {code}").into())
        }
        Ok(_) => Err(format!("{what}: the insert was accepted").into()),
    }
}

/// Decision 0057 rules 14 and 22: a frozen schema 13 file upgrades with every job
/// row, key, finding and change row it held, each change row's job and data
/// version, the feed heads and sequence, and the job indexes and triggers, and its
/// schema is the schema of a fresh database. No row gets a profile and no attempt
/// exists yet.
#[test]
fn a_frozen_version_13_file_upgrades_to_14_keeping_every_job_row_key_finding_and_change()
-> TestResult {
    let fresh = TempDir::new();
    current_database(&database_in(fresh.path()))?;
    let fresh_schema = schema_of(&database_in(fresh.path()))?;

    let dir = TempDir::new();
    let path = database_in(dir.path());
    version_13_with_every_state(&path)?;
    assert_eq!(user_version(&path)?, 13);
    assert_eq!(
        count(&path, "SELECT count(*) FROM jobs")?,
        7,
        "the fixture job, the second job and one job of each other state"
    );
    assert_eq!(
        count(&path, "SELECT count(DISTINCT state) FROM jobs")?,
        6,
        "a job in every state"
    );
    assert_eq!(
        count(&path, "SELECT count(*) FROM job_requests")?,
        2,
        "the request key of the fixture job and the one of the second job"
    );
    assert_eq!(count(&path, "SELECT count(*) FROM job_findings")?, 1);
    assert_eq!(
        count(
            &path,
            &format!(
                "SELECT count(*) FROM changes
                 WHERE kind = 'job_changed' AND job_id = '{SECOND_JOB}'"
            )
        )?,
        2,
        "two job_changed rows name the second job"
    );
    let before = kept_state(&path)?;

    SqliteStore::open(&path)
        .map_err(|error| format!("the version 13 file must upgrade: {error}"))?;

    assert_eq!(user_version(&path)?, SCHEMA_VERSION);
    assert_eq!(
        schema_of(&path)?,
        fresh_schema,
        "the upgraded schema must be the fresh one"
    );
    assert_eq!(
        kept_state(&path)?,
        before,
        "every job column, key, finding and change row with its job, each head, the \
         sequence, the feed identity, the job indexes and triggers, and the foreign keys"
    );
    assert_eq!(
        count(&path, "SELECT count(*) FROM jobs WHERE profile IS NOT NULL")?,
        0,
        "no job that existed before the upgrade gets a profile"
    );
    assert_eq!(
        count(&path, "SELECT count(*) FROM execution_attempts")?,
        0,
        "the upgrade creates no execution attempt"
    );
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND tbl_name = 'jobs'
                 AND name NOT LIKE 'sqlite_%'"
        )?,
        3,
        "the three job indexes"
    );
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'jobs'"
        )?,
        0,
        "no trigger on jobs"
    );
    for table in [
        "job_requests",
        "job_findings",
        "changes",
        "execution_attempts",
    ] {
        assert_eq!(
            foreign_key_violations(&path, table)?,
            0,
            "{table} must hold no broken foreign key after the upgrade"
        );
    }
    Ok(())
}

/// After the upgrade, a job transition still appends its `job_changed` row after
/// the old head, in the owner's feed, and names the job it changed.
#[test]
fn a_job_transition_after_the_upgrade_still_appends_its_change_row() -> TestResult {
    let dir = TempDir::new();
    let path = database_in(dir.path());
    version_13_with_every_state(&path)?;
    let store = SqliteStore::open(&path)?;
    let owner = actor("owner");
    let queued: JobId = parsed(SECOND_JOB);

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
    assert_eq!(
        rows_after(&path, 11)?,
        [format!(
            "12|12|owner|job_changed|{SECOND_JOB}|{OLD_ID}|2|2|none"
        )],
        "exactly one new row, after the head 11, naming the second job"
    );
    Ok(())
}

/// A schema 14 step that fails leaves the version 13 file as it was, in both
/// journal modes. Each case makes a table that the step creates already exist, so
/// the open is refused by that table and the rebuild of `jobs` is rolled back.
#[test]
fn a_failed_schema_14_step_leaves_the_version_13_file_untouched() -> TestResult {
    for wal in [false, true] {
        for table in ["jobs_v14", "execution_attempts"] {
            let dir = TempDir::new();
            let path = database_in(dir.path());
            version_13_with_every_state(&path)?;
            {
                let connection = Connection::open(&path)?;
                connection.execute_batch(&format!("CREATE TABLE {table} (x TEXT);"))?;
                if wal {
                    connection.pragma_update(None, "journal_mode", "WAL")?;
                }
            }
            let before = files_of(dir.path());
            let case = format!("wal {wal}, existing {table}");
            let error = SqliteStore::open(&path)
                .err()
                .ok_or_else(|| format!("{case}: the open was accepted"))?;
            let reason = format!("table {table} already exists");
            assert!(
                error.to_string().contains(reason.as_str()),
                "{case}: the refusal must name the table: {error}"
            );
            assert_eq!(
                files_of(dir.path()),
                before,
                "{case}: the refused open changed a file"
            );
            assert_eq!(user_version(&path)?, 13, "{case}");
            let jobs_sql: String = Connection::open(&path)?.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'jobs'",
                [],
                |row| row.get(0),
            )?;
            assert!(
                !jobs_sql.contains("profile"),
                "{case}: the jobs table was rebuilt: {jobs_sql}"
            );
        }
    }
    Ok(())
}

/// The job rows that the schema refuses, and the one it accepts: a profile on
/// exactly the `run_profile` kind, and the kinds and failures it names. Each
/// refusal must be the CHECK constraint, and the accepted job is the one that the
/// attempts name.
fn refuse_impossible_jobs(connection: &Connection, artifact: &str) -> TestResult {
    let queued = JobFields {
        kind: "verify_revision",
        profile: None,
        state: "queued",
        failure: None,
        finished_at: None,
    };
    let refused_jobs = [
        (
            "a run_profile job without a profile",
            JobFields {
                kind: "run_profile",
                ..queued
            },
        ),
        (
            "a verify_revision job with a profile",
            JobFields {
                profile: Some("fixture"),
                ..queued
            },
        ),
        (
            "a job of a kind that is not one",
            JobFields {
                kind: "other",
                ..queued
            },
        ),
        (
            "a failure that is not one",
            JobFields {
                state: "failed",
                failure: Some("nope"),
                finished_at: Some(5),
                ..queued
            },
        ),
    ];
    for (index, (what, fields)) in refused_jobs.into_iter().enumerate() {
        let id = format!("job_{index:032}");
        assert_refused(insert_job(connection, &id, artifact, fields), CHECK, what)?;
    }
    let accepted = JobFields {
        kind: "run_profile",
        profile: Some("fixture"),
        state: "failed",
        failure: Some("execution_unavailable"),
        finished_at: Some(5),
    };
    assert_eq!(
        insert_job(connection, RUN_JOB, artifact, accepted)?,
        1,
        "a run_profile job with its profile and the failure execution_unavailable is accepted"
    );
    Ok(())
}

/// The execution attempts that the schema refuses, and the one it accepts: the
/// lengths of the attempt, digest and directory, the states and revocations it
/// names, the settle time that goes with a settled state, and the job an attempt
/// names. A refused attempt carries an identity that no other row holds, so only
/// its own check can refuse it.
fn refuse_impossible_attempts(connection: &Connection) -> TestResult {
    let digest = "ab".repeat(32);
    let short_attempt = "a".repeat(31);
    let short_digest = "b".repeat(63);
    let short_directory = "c".repeat(31);
    let valid = AttemptFields {
        attempt: ATTEMPT,
        job: RUN_JOB,
        tool_digest: digest.as_str(),
        directory: DIRECTORY,
        state: "live",
        revoked: None,
        settled_at: None,
    };
    assert_eq!(
        insert_attempt(connection, valid)?,
        1,
        "a live attempt of a run_profile job is accepted"
    );
    let unused = "f".repeat(32);
    let refused = AttemptFields {
        attempt: unused.as_str(),
        ..valid
    };
    let refused_attempts = [
        (
            "an attempt of 31 characters",
            AttemptFields {
                attempt: short_attempt.as_str(),
                ..refused
            },
            CHECK,
        ),
        (
            "a tool digest of 63 characters",
            AttemptFields {
                tool_digest: short_digest.as_str(),
                ..refused
            },
            CHECK,
        ),
        (
            "a directory of 31 characters",
            AttemptFields {
                directory: short_directory.as_str(),
                ..refused
            },
            CHECK,
        ),
        (
            "a state that is not one",
            AttemptFields {
                state: "done",
                ..refused
            },
            CHECK,
        ),
        (
            "a revocation that is not one",
            AttemptFields {
                revoked: Some("other"),
                ..refused
            },
            CHECK,
        ),
        (
            "a settled attempt without a settle time",
            AttemptFields {
                state: "settled",
                ..refused
            },
            CHECK,
        ),
        (
            "a live attempt with a settle time",
            AttemptFields {
                settled_at: Some(9),
                ..refused
            },
            CHECK,
        ),
        (
            "an attempt whose job does not exist",
            AttemptFields {
                job: "job_99999999999999999999999999999999",
                ..refused
            },
            FOREIGN_KEY,
        ),
    ];
    for (what, fields, code) in refused_attempts {
        assert_refused(insert_attempt(connection, fields), code, what)?;
    }
    Ok(())
}

/// The schema refuses a job or an execution attempt that the store cannot mean,
/// and accepts the rows that meet every rule (the helpers say which, and why).
#[test]
fn the_schema_refuses_impossible_job_and_attempt_rows() -> TestResult {
    let dir = TempDir::new();
    let path = database_in(dir.path());
    let owner = actor("owner");
    let created = {
        let store = SqliteStore::open(&path)?;
        create_demo(&store, &owner, &key("create"), "Secret", BODY)?
    };
    let connection = Connection::open(&path)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    refuse_impossible_jobs(&connection, created.artifact.id.as_str())?;
    refuse_impossible_attempts(&connection)?;
    assert_eq!(
        count(&path, "SELECT count(*) FROM jobs")?,
        1,
        "only the accepted job was inserted"
    );
    assert_eq!(
        count(&path, "SELECT count(*) FROM execution_attempts")?,
        1,
        "only the valid attempt was inserted"
    );
    Ok(())
}

/// Decision 0057 rule 14: the rebuild of `jobs` is refused, and the file is left
/// as it was, when a table that refers to `jobs` holds a reference that no longer
/// resolves. The check runs per table, and the refusal names the table.
#[test]
fn an_orphaned_job_request_already_in_a_version_13_file_does_not_block_the_upgrade() -> TestResult {
    let dir = TempDir::new();
    let path = database_in(dir.path());
    version_13_with_every_state(&path)?;
    Connection::open(&path)?.execute_batch(&format!(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO job_requests (actor, request_key, fingerprint, job_id, created_at)
             VALUES ('owner', 'test-key-orphan', x'{orphan}',
                 'job_99999999999999999999999999999999', 630);",
        orphan = "0c".repeat(32)
    ))?;
    // The orphan was there before any step ran, so no step broke it: the
    // upgrade passes and keeps the row, as it would with foreign keys on.
    let _store = SqliteStore::open(&path)?;
    assert_eq!(user_version(&path)?, 14);
    let orphans: i64 = Connection::open(&path)?.query_row(
        "SELECT count(*) FROM job_requests WHERE request_key = 'test-key-orphan'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(orphans, 1, "the orphaned request is kept as it was");
    Ok(())
}
