//! The readiness rows of decision 0058 rule 20 (slice R03cda), against a real
//! `SQLite` store: when an execution worker's row is written, rewritten,
//! pruned and forgotten, when it counts as ready, that a lease renewal of a
//! running `run_profile` job keeps it fresh, and that a restore empties the
//! table. Times are given, so nothing sleeps.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_core::artifacts::ArtifactService;
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, RequestKey, Title, UnixMillis};
use artifree_domain::job::{AttemptIdentity, CheckpointAnswer, ProfileName, WorkerId};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_ports::job_store::{
    ExecutionClaim, ExecutionPresence, JobClaim, JobStore, READY_WINDOW_MS, SEEN_PRUNE_MS,
    SEEN_REWRITE_MS,
};
use artifree_providers::backup;
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};
use rusqlite::Connection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SEEN: i64 = 1_800_000_000_000;
const LEASE_MS: u64 = 30_000;

/// A directory for one test, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        let path = fresh_path();
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn database(&self) -> PathBuf {
        self.0.join(DATABASE_FILE)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Drop cannot report a failure; a directory left behind is cleared by the
        // next run that uses the same name.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A path that does not exist yet, unique to this run.
fn fresh_path() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "artifree-presence-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    // A path left by an earlier run with the same name is cleared first.
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_file(&path);
    path
}

fn parsed<T>(text: &str) -> Result<T, Box<dyn std::error::Error>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    text.parse::<T>()
        .map_err(|error| format!("invalid fixture {text:?}: {error}").into())
}

/// Every row of the table, as worker and last-seen time, by worker.
fn rows(dir: &TempDir) -> Result<Vec<(String, i64)>, Box<dyn std::error::Error>> {
    let connection = Connection::open(dir.database())?;
    let mut statement =
        connection.prepare("SELECT worker, last_seen FROM execution_workers ORDER BY worker")?;
    let found = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(found)
}

fn row(worker: &str, at: i64) -> (String, i64) {
    (worker.to_owned(), at)
}

/// Rule 20: a worker is ready from the time it was seen until five minutes
/// after it. A row up to five minutes later than now (a clock that stepped
/// back) counts too, and one further ahead does not.
#[test]
fn a_worker_is_ready_for_five_minutes_after_it_was_seen() -> TestResult {
    let dir = TempDir::new()?;
    let store = SqliteStore::open(&dir.database())?;
    let worker: WorkerId = parsed("execution-a")?;
    assert!(!store.execution_ready(UnixMillis(SEEN))?, "no row yet");

    store.mark_seen(&worker, UnixMillis(SEEN))?;
    for (at, ready) in [
        (SEEN - READY_WINDOW_MS - 1, false),
        (SEEN - READY_WINDOW_MS, true),
        (SEEN - 1, true),
        (SEEN, true),
        (SEEN + READY_WINDOW_MS, true),
        (SEEN + READY_WINDOW_MS + 1, false),
    ] {
        assert_eq!(store.execution_ready(UnixMillis(at))?, ready, "at {at}");
    }
    Ok(())
}

/// A row is rewritten only once it is a minute old, or when it is later than
/// now, so an idle poll writes at most once a minute.
#[test]
fn a_row_is_rewritten_only_when_a_minute_old_or_in_the_future() -> TestResult {
    let dir = TempDir::new()?;
    let store = SqliteStore::open(&dir.database())?;
    let worker: WorkerId = parsed("execution-a")?;

    store.mark_seen(&worker, UnixMillis(SEEN))?;
    store.mark_seen(&worker, UnixMillis(SEEN + SEEN_REWRITE_MS - 1))?;
    assert_eq!(
        rows(&dir)?,
        [row("execution-a", SEEN)],
        "a fresh row is kept"
    );

    store.mark_seen(&worker, UnixMillis(SEEN + SEEN_REWRITE_MS))?;
    let later = SEEN + SEEN_REWRITE_MS;
    assert_eq!(rows(&dir)?, [row("execution-a", later)], "a minute old");

    store.mark_seen(&worker, UnixMillis(later - 1))?;
    assert_eq!(
        rows(&dir)?,
        [row("execution-a", later - 1)],
        "a row later than now is rewritten"
    );
    Ok(())
}

/// Each write deletes the rows last seen more than an hour before it; a row
/// exactly an hour old is kept.
#[test]
fn a_write_prunes_rows_older_than_an_hour() -> TestResult {
    let dir = TempDir::new()?;
    let store = SqliteStore::open(&dir.database())?;
    store.mark_seen(&parsed("execution-a")?, UnixMillis(SEEN))?;
    store.mark_seen(&parsed("execution-b")?, UnixMillis(SEEN + 1))?;

    store.mark_seen(&parsed("execution-c")?, UnixMillis(SEEN + SEEN_PRUNE_MS))?;
    assert_eq!(
        rows(&dir)?,
        [
            row("execution-a", SEEN),
            row("execution-b", SEEN + 1),
            row("execution-c", SEEN + SEEN_PRUNE_MS),
        ],
        "an hour old is kept"
    );

    // A row more than an hour ahead of now, left by a clock that stepped back,
    // is pruned as well.
    Connection::open(dir.database())?.execute(
        "INSERT INTO execution_workers (worker, last_seen) VALUES ('execution-d', ?1)",
        [SEEN + 3 * SEEN_PRUNE_MS],
    )?;
    // `execution-b`'s row is stale, so this writes, and the write prunes.
    let pruning = SEEN + SEEN_PRUNE_MS + 1;
    store.mark_seen(&parsed("execution-b")?, UnixMillis(pruning))?;
    assert_eq!(
        rows(&dir)?,
        [
            row("execution-b", pruning),
            row("execution-c", SEEN + SEEN_PRUNE_MS),
        ],
        "more than an hour old is deleted"
    );
    Ok(())
}

/// A refused worker forgets its own row and no other.
#[test]
fn forgetting_a_worker_deletes_only_its_row() -> TestResult {
    let dir = TempDir::new()?;
    let store = SqliteStore::open(&dir.database())?;
    let refused: WorkerId = parsed("execution-a")?;
    store.mark_seen(&refused, UnixMillis(SEEN))?;
    store.mark_seen(&parsed("execution-b")?, UnixMillis(SEEN))?;

    store.forget_worker(&refused)?;
    assert_eq!(rows(&dir)?, [row("execution-b", SEEN)]);
    store.forget_worker(&refused)?;
    assert_eq!(rows(&dir)?, [row("execution-b", SEEN)], "forgetting twice");
    Ok(())
}

/// A store with one artifact and a `run_profile` job claimed by an execution
/// worker at `SEEN`.
struct Claimed {
    dir: TempDir,
    store: SqliteStore,
    lease: artifree_ports::job_store::Lease,
    attempt: AttemptIdentity,
}

impl Claimed {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let store = SqliteStore::open(&dir.database())?;
        let service = ArtifactService::new(SqliteStore::open(&dir.database())?);
        let owner: ActorId = parsed("owner")?;
        let package = FilePackage::new(vec![(PackagePath::entry(), b"<h1>1</h1>".to_vec())])?;
        let created = service.create(
            &owner,
            &parsed::<RequestKey>("create-key")?,
            &parsed::<Title>("Presence")?,
            &package,
        )?;
        let profile: ProfileName = parsed("react-v1")?;
        service.enqueue_profile_job(
            &owner,
            &parsed("enqueue-key")?,
            &created.artifact.id,
            SourceRevision(1),
            &profile,
            UnixMillis(SEEN),
        )?;
        let attempt = AttemptIdentity::from_random([7; 16]);
        let claimed = store
            .claim_job(JobClaim {
                worker: &parsed::<WorkerId>("execution-a")?,
                now: UnixMillis(SEEN),
                lease_ms: LEASE_MS,
                known_profiles: std::slice::from_ref(&profile),
                execution: Some(ExecutionClaim {
                    attempt,
                    tool_digest: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    machine: "machine-a",
                    boot: "boot-a",
                    cgroupns: "cgroupns-a",
                    directory: "0123456789abcdef0123456789abcdef",
                }),
            })?
            .claimed
            .ok_or("no job was due")?;
        Ok(Self {
            dir,
            store,
            lease: claimed.lease,
            attempt,
        })
    }

    fn renew(&self, at: i64) -> TestResult {
        let answer =
            self.store
                .checkpoint_attempt(&self.lease, &self.attempt, UnixMillis(at), LEASE_MS)?;
        assert_eq!(answer, CheckpointAnswer::Continue, "the renewal at {at}");
        Ok(())
    }
}

/// A renewal of a running job's lease marks the job's worker seen, under the
/// same one-minute rule, so a long attempt keeps its worker ready.
#[test]
fn a_lease_renewal_keeps_the_workers_row_fresh() -> TestResult {
    let claimed = Claimed::new()?;
    claimed.renew(SEEN)?;
    assert_eq!(
        rows(&claimed.dir)?,
        [row("execution-a", SEEN)],
        "the first renewal"
    );

    claimed.renew(SEEN + SEEN_REWRITE_MS - 1)?;
    assert_eq!(
        rows(&claimed.dir)?,
        [row("execution-a", SEEN)],
        "a fresh row"
    );

    let later = SEEN + 2 * READY_WINDOW_MS;
    claimed.renew(later)?;
    assert_eq!(rows(&claimed.dir)?, [row("execution-a", later)]);
    assert!(claimed.store.execution_ready(UnixMillis(later))?);

    // A checkpoint that tells the guardian to stop still marks the worker
    // seen: it is alive and cleaning up.
    Connection::open(claimed.dir.database())?.execute(
        "UPDATE jobs SET cancel_requested = 1 WHERE id = ?1",
        [claimed.lease.job.as_str()],
    )?;
    let stopping = later + 2 * READY_WINDOW_MS;
    let answer = claimed.store.checkpoint_attempt(
        &claimed.lease,
        &claimed.attempt,
        UnixMillis(stopping),
        LEASE_MS,
    )?;
    assert!(matches!(answer, CheckpointAnswer::Stop(_)), "{answer:?}");
    assert_eq!(rows(&claimed.dir)?, [row("execution-a", stopping)]);
    Ok(())
}

/// A restore empties the table: no worker of the backed-up database serves the
/// restored one, even a moment after the backup.
#[test]
fn a_restore_empties_the_table() -> TestResult {
    let dir = TempDir::new()?;
    let store = SqliteStore::open(&dir.database())?;
    store.mark_seen(&parsed("execution-a")?, UnixMillis(SEEN))?;
    drop(store);

    let output = fresh_path();
    backup::backup(&dir.0, &output)?;
    let restored = TempDir(fresh_path());
    backup::restore(&output, &restored.0)?;
    let _ = std::fs::remove_file(&output);

    assert_eq!(
        rows(&dir)?,
        [row("execution-a", SEEN)],
        "the original keeps it"
    );
    assert_eq!(rows(&restored)?, [], "the restored database has none");
    let store = SqliteStore::open(&restored.database())?;
    assert!(!store.execution_ready(UnixMillis(SEEN))?);
    Ok(())
}
