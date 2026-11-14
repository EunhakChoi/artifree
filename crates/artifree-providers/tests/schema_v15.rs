//! Upgrading a frozen schema 14 file to schema 15, and the rows that schema 15
//! refuses (decision 0058 rules 9, 13 and 18).
//!
//! The version 14 fixture is built from frozen copies of the schema texts of
//! versions 11 to 14 (`common::history`), so a later change to the live ladder
//! cannot change what the upgrade is tested on.

mod common;

use std::path::Path;

use artifree_providers::storage::{SCHEMA_VERSION, SqliteStore};
use common::{
    OLD_ID, TempDir, TestResult, current_database, database_in, files_of, schema_of, user_version,
    version_14_database,
};
use rusqlite::types::Value;
use rusqlite::{Connection, params};

/// A `run_profile` job that failed because no worker knew its profile.
const UNAVAILABLE_JOB: &str = "job_11111111111111111111111111111111";
/// A running `run_profile` job, which the attempt names.
const RUNNING_JOB: &str = "job_22222222222222222222222222222222";
/// A succeeded `verify_revision` job with a finding.
const VERIFIED_JOB: &str = "job_33333333333333333333333333333333";

/// The tables whose rows the upgrade must keep, by value.
const KEPT_TABLES: [&str; 5] = [
    "jobs",
    "job_requests",
    "job_findings",
    "execution_attempts",
    "changes",
];

/// The extended result codes that the refusals carry.
const CHECK: i32 = rusqlite::ffi::SQLITE_CONSTRAINT_CHECK;
const FOREIGN_KEY: i32 = rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY;

/// The version 14 fixture with a job of each kind that schema 14 allows beyond
/// the queued `verify_revision` job of version 8: a failed and a running
/// `run_profile` job, the running one's attempt, and a verified job's finding.
fn version_14_with_jobs(path: &Path) -> TestResult {
    version_14_database(path)?;
    let connection = Connection::open(path)?;
    connection.execute_batch(&format!(
        "INSERT INTO jobs (id, actor, artifact_id, revision, kind, profile, state, attempts,
             max_attempts, fence, run_after, worker, lease_expires_at, cancel_requested,
             failure, files_checked, bytes_checked, mismatch_count, created_at, started_at,
             finished_at)
         VALUES ('{UNAVAILABLE_JOB}', 'owner', '{OLD_ID}', 2, 'run_profile', 'react-v1',
                 'failed', 1, 3, 1, 700, NULL, NULL, 0, 'execution_unavailable', NULL, NULL,
                 NULL, 700, 701, 702),
                ('{RUNNING_JOB}', 'owner', '{OLD_ID}', 2, 'run_profile', 'react-v1',
                 'running', 1, 3, 1, 710, 'worker-1', 2000, 0, NULL, NULL, NULL, NULL, 710,
                 711, NULL),
                ('{VERIFIED_JOB}', 'owner', '{OLD_ID}', 2, 'verify_revision', NULL,
                 'succeeded', 1, 3, 1, 720, NULL, NULL, 0, NULL, 1, 12, 1, 720, 721, 722);
         INSERT INTO job_findings (job_id, ordinal, path, problem)
             VALUES ('{VERIFIED_JOB}', 0, 'index.html', 'digest_mismatch');
         INSERT INTO execution_attempts (attempt, job_id, fence, actor_generation, profile,
             tool_digest, machine, boot, cgroupns, directory, state, revoked, created_at,
             settled_at)
         VALUES ('{attempt}', '{RUNNING_JOB}', 1, NULL, 'react-v1', '{digest}', 'machine-1',
                 'boot-1', 'cgroup-1', '{directory}', 'live', NULL, 711, NULL);",
        attempt = "0123456789abcdef".repeat(2),
        digest = "ab".repeat(32),
        directory = "fedcba9876543210".repeat(2),
    ))?;
    Ok(())
}

/// Every row of `table`, each as the debug text of its values, sorted.
fn rows_of(path: &Path, table: &str) -> Result<Vec<String>, rusqlite::Error> {
    let connection = Connection::open(path)?;
    let mut statement = connection.prepare(&format!("SELECT * FROM {table}"))?;
    let columns = statement.column_count();
    let mut rows = statement.query([])?;
    let mut lines = Vec::new();
    while let Some(row) = rows.next()? {
        let mut values = Vec::with_capacity(columns);
        for column in 0..columns {
            values.push(row.get::<_, Value>(column)?);
        }
        lines.push(format!("{values:?}"));
    }
    lines.sort();
    Ok(lines)
}

fn kept_rows(path: &Path) -> Result<Vec<(&'static str, Vec<String>)>, rusqlite::Error> {
    KEPT_TABLES
        .into_iter()
        .map(|table| rows_of(path, table).map(|rows| (table, rows)))
        .collect()
}

fn count(path: &Path, query: &str) -> Result<i64, rusqlite::Error> {
    Connection::open(path)?.query_row(query, [], |row| row.get(0))
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

/// Decision 0058 rule 9: a frozen schema 14 file upgrades with every job,
/// request key, finding, attempt and change row it held, and its schema is the
/// schema of a fresh database. No build or diagnostic exists after it.
#[test]
fn a_frozen_version_14_file_upgrades_to_15_keeping_every_job_and_attempt_row() -> TestResult {
    let fresh = TempDir::new();
    current_database(&database_in(fresh.path()))?;
    let fresh_schema = schema_of(&database_in(fresh.path()))?;

    let dir = TempDir::new();
    let path = database_in(dir.path());
    version_14_with_jobs(&path)?;
    assert_eq!(user_version(&path)?, 14);
    assert_eq!(count(&path, "SELECT count(*) FROM jobs")?, 4);
    assert_eq!(count(&path, "SELECT count(*) FROM execution_attempts")?, 1);
    let before = kept_rows(&path)?;

    SqliteStore::open(&path)
        .map_err(|error| format!("the version 14 file must upgrade: {error}"))?;

    assert_eq!(user_version(&path)?, SCHEMA_VERSION);
    assert_eq!(
        schema_of(&path)?,
        fresh_schema,
        "the upgraded schema must be the fresh one"
    );
    assert_eq!(kept_rows(&path)?, before, "every row of every job table");
    for table in ["builds", "build_files", "job_diagnostics"] {
        assert_eq!(
            count(&path, &format!("SELECT count(*) FROM {table}"))?,
            0,
            "the upgrade adds no row to {table}"
        );
    }
    let connection = Connection::open(&path)?;
    let broken: i64 =
        connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    assert_eq!(broken, 0, "no foreign key is broken after the upgrade");
    Ok(())
}

/// A schema 15 step that fails leaves the version 14 file as it was, in both
/// journal modes. Each case makes a table that the step creates already exist,
/// so the open is refused by that table and the rebuild of `jobs` is rolled back.
#[test]
fn a_failed_schema_15_step_leaves_the_version_14_file_untouched() -> TestResult {
    for wal in [false, true] {
        for table in ["jobs_v15", "builds", "job_diagnostics"] {
            let dir = TempDir::new();
            let path = database_in(dir.path());
            version_14_with_jobs(&path)?;
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
            assert_eq!(user_version(&path)?, 14, "{case}");
            let jobs_sql: String = Connection::open(&path)?.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'jobs'",
                [],
                |row| row.get(0),
            )?;
            assert!(
                !jobs_sql.contains("build_failed"),
                "{case}: the jobs table was rebuilt: {jobs_sql}"
            );
        }
    }
    Ok(())
}

/// Inserts a failed `run_profile` job of the old artifact's revision 1.
fn insert_failed(connection: &Connection, id: &str, failure: &str) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO jobs (id, actor, artifact_id, revision, kind, profile, state, attempts,
             max_attempts, fence, run_after, cancel_requested, failure, created_at, finished_at)
         VALUES (?1, 'owner', ?2, 1, 'run_profile', 'react-v1', 'failed', 1, 3, 1, 0, 0, ?3,
             1, 2)",
        params![id, OLD_ID, failure],
    )
}

fn insert_diagnostic(
    connection: &Connection,
    ordinal: i64,
    location: (Option<&str>, Option<i64>, Option<i64>),
    message: &str,
) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO job_diagnostics (job_id, ordinal, path, line, column_number, message)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            UNAVAILABLE_JOB,
            ordinal,
            location.0,
            location.1,
            location.2,
            message
        ],
    )
}

fn insert_build(connection: &Connection, file_count: i64) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO builds (artifact_id, revision, profile, job_id, tool_digest, file_count,
             total_bytes, published_at)
         VALUES (?1, 1, 'react-v1', ?2, ?3, ?4, 12, 5)",
        params![OLD_ID, RUNNING_JOB, "ab".repeat(32), file_count],
    )
}

/// Decision 0058 rules 9, 13 and 18: the new failures are accepted and an
/// unknown one is not; a diagnostic has all of its location or none, at most 20
/// ordinals and a message of 1 to 512 bytes; a build has 1 to 256 files, and a
/// build file's digest names a stored blob.
#[test]
fn the_schema_refuses_impossible_failure_diagnostic_and_build_rows() -> TestResult {
    let dir = TempDir::new();
    let path = database_in(dir.path());
    version_14_with_jobs(&path)?;
    SqliteStore::open(&path)?;
    let connection = Connection::open(&path)?;
    connection.pragma_update(None, "foreign_keys", true)?;

    for (index, failure) in ["build_failed", "limit_exceeded", "output_rejected"]
        .into_iter()
        .enumerate()
    {
        // Numbered from 100: `job_000…0` is the fixture's version 8 job.
        insert_failed(&connection, &format!("job_{:032}", 100 + index), failure)?;
    }
    assert_refused(
        insert_failed(&connection, &format!("job_{:032}", 109), "build_broken"),
        CHECK,
        "an unknown failure",
    )?;

    insert_diagnostic(&connection, 0, (Some("src/App.tsx"), Some(3), Some(7)), "x")?;
    insert_diagnostic(&connection, 1, (None, None, None), "the filter ended it")?;
    insert_diagnostic(&connection, 19, (None, None, None), &"m".repeat(512))?;
    for (what, ordinal, location, message) in [
        (
            "a path without a line",
            2,
            (Some("a.ts"), None, Some(1)),
            "x",
        ),
        ("a line without a path", 3, (None, Some(1), Some(1)), "x"),
        ("an ordinal of 20", 20, (None, None, None), "x"),
        ("an empty message", 4, (None, None, None), ""),
    ] {
        assert_refused(
            insert_diagnostic(&connection, ordinal, location, message),
            CHECK,
            what,
        )?;
    }
    assert_refused(
        insert_diagnostic(&connection, 5, (None, None, None), &"é".repeat(257)),
        CHECK,
        "a message of 514 bytes in 257 characters",
    )?;

    assert_refused(insert_build(&connection, 0), CHECK, "a build of no file")?;
    assert_refused(
        insert_build(&connection, 257),
        CHECK,
        "a build of 257 files",
    )?;
    insert_build(&connection, 1)?;
    assert_refused(
        connection.execute(
            "INSERT INTO build_files (artifact_id, revision, profile, path, size, digest)
             VALUES (?1, 1, 'react-v1', 'index.html', 12, ?2)",
            params![OLD_ID, [9u8; 32].as_slice()],
        ),
        FOREIGN_KEY,
        "a build file whose blob is not stored",
    )?;
    Ok(())
}
