//! Helpers that the recovery cases of R02bc share (decision 0057 rule 20): the worker's pause
//! file, a worker on other directories, a forced kill, a restored copy of the database, trash
//! and recovery, and the attempt rows of a job.

use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use artifree_core::artifacts::MetadataChange;
use artifree_domain::MetadataRevision;
use artifree_domain::identity::{ArtifactId, RequestKey};
use artifree_domain::job::{AttemptIdentity, JobId};
use artifree_ports::job_store::{AttemptRow, JobStore};
use artifree_ports::user_store::UserStore;
use artifree_providers::backup::{backup, restore};
use artifree_providers::tokens::SqliteTokenStore;

use super::{
    ATTACH, BIN, DEADLINE, STEP, Scratch, Store, TestResult, Worker, enter_harness, owner, parsed,
    payload_processes, poll, private_dir, read_lines, within, worker_group,
};

/// The persisted quarantine file of an execution directory (0056, amendment of 2026-11-14). Its
/// lines are the tag, then `reason=<word>`.
pub(super) const QUARANTINE_FILE: &str = "quarantine";
/// The time that a disable records. Only whether the owner is disabled matters to the store.
const DISABLED_AT: &str = "2026-11-14T00:00:00Z";

/// Disables the case's owner when `disabled` is true and enables it again when it is false,
/// through the token store's user port. Each change raises the owner's state generation
/// (decision 0042 rule 6).
pub(super) fn set_disabled(store: &Store, disabled: bool) -> TestResult {
    SqliteTokenStore::open(&store.path)?
        .set_disabled(&owner()?, disabled.then_some(DISABLED_AT))?;
    Ok(())
}

/// The file in the execution directory that carries the worker's pause line: `<phase> hold`, or
/// `<phase> stall <ms>`.
pub(super) const PAUSE_FILE: &str = "pause";
/// The directory in the execution directory where the worker writes one marker file for each
/// phase that it pauses at. A case waits for a marker before it acts on that phase.
pub(super) const PAUSED_DIR: &str = "paused";

impl Scratch {
    /// The path `name` in the case's root directory. Nothing is created.
    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Creates the owner-only directory `name` in the case's root and returns it, for an
    /// execution directory that is not the case's own.
    pub(super) fn other_execution(&self, name: &str) -> Result<PathBuf, Box<dyn Error>> {
        let path = self.path(name);
        private_dir(&path)?;
        Ok(path)
    }
}

impl Worker {
    /// Starts `worker run` on the data directory `data` and the execution directory `execution`,
    /// with `extra` added, as the only process of a new child group of the unit. `Worker::spawn`
    /// calls it with the case's own directories.
    pub(super) fn spawn_in(
        data: &Path,
        execution: &Path,
        extra: &[&str],
    ) -> Result<Self, Box<dyn Error>> {
        let group = worker_group(&enter_harness()?)?;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(ATTACH)
            .arg("sh")
            .arg(group.join("cgroup.procs"))
            .arg(BIN)
            .args(["worker", "run", "--poll-ms", "50", "--data-dir"])
            .arg(data)
            .arg("--execution-dir")
            .arg(execution)
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("the worker has no stdout")?;
        let stderr = child.stderr.take().ok_or("the worker has no stderr")?;
        Ok(Self {
            child,
            group,
            stdout_lines: read_lines(stdout),
            stderr_lines: read_lines(stderr),
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    /// Kills the worker with SIGKILL and reaps it, so that no process of it remains. The guard's
    /// later kill and wait are best effort.
    pub(super) fn kill(&mut self) -> TestResult {
        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }
}

/// Accepts an error of `kind` as the outcome the caller asked for: a path that is already
/// there, or already gone. Any other failure is returned.
fn tolerate(result: io::Result<()>, kind: io::ErrorKind) -> io::Result<()> {
    result.or_else(|error| {
        if error.kind() == kind {
            Ok(())
        } else {
            Err(error)
        }
    })
}

/// Makes the marker directory of `execution` and writes its pause file with the one line
/// `line`, which is `<phase> hold` or `<phase> stall <ms>`. The pause file is owner-only.
pub(super) fn pause_launch(execution: &Path, line: &str) -> TestResult {
    tolerate(
        private_dir(&execution.join(PAUSED_DIR)),
        io::ErrorKind::AlreadyExists,
    )?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(execution.join(PAUSE_FILE))?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Removes the pause file and the marker directory with its markers from `execution`. Either
/// may be absent, so a case can clear a pause that never began, and a later pause never sees a
/// marker of an earlier one.
pub(super) fn clear_pause(execution: &Path) -> TestResult {
    tolerate(
        fs::remove_file(execution.join(PAUSE_FILE)),
        io::ErrorKind::NotFound,
    )?;
    tolerate(
        fs::remove_dir_all(execution.join(PAUSED_DIR)),
        io::ErrorKind::NotFound,
    )?;
    Ok(())
}

/// Waits until the worker has written the marker `word` in `execution`, which means that it has
/// reached the phase that `word` names and is paused there.
pub(super) fn wait_marker(execution: &Path, word: &str) -> TestResult {
    poll(within(DEADLINE), &format!("the {word} marker"), || {
        Ok(execution.join(PAUSED_DIR).join(word).exists().then_some(()))
    })
}

/// The path of the record that the worker writes for `attempt` in `execution`.
pub(super) fn record_path(execution: &Path, attempt: &AttemptIdentity) -> PathBuf {
    execution
        .join("attempts")
        .join(format!("{}.record", attempt.hex()))
}

/// The attempt rows of `job`, ordered by their fencing token. A name that the store lists
/// without a row is an error, because the store lists only the rows it holds.
pub(super) fn attempt_rows(store: &Store, job: &JobId) -> Result<Vec<AttemptRow>, Box<dyn Error>> {
    let mut rows = Vec::new();
    for name in store.attempt_names(job)? {
        let attempt: AttemptIdentity = parsed(&name)?;
        let row = store
            .port
            .attempt_row(&attempt)?
            .ok_or_else(|| format!("no attempt row is named {name}"))?;
        rows.push(row);
    }
    rows.sort_by_key(|row| row.fence);
    Ok(rows)
}

/// Waits until no process of the payload runs on the host.
pub(super) fn wait_no_payload() -> TestResult {
    poll(within(DEADLINE), "no payload process", || {
        Ok(payload_processes()?.is_empty().then_some(()))
    })
}

/// Backs up the case's data directory and restores the copy into the case's own directory
/// `name`, which restore creates as a new data directory (decision 0027 rule 7). Returns the
/// restored directory.
pub(super) fn restored_copy(scratch: &Scratch, name: &str) -> Result<PathBuf, Box<dyn Error>> {
    let copy = scratch.path(&format!("{name}.backup"));
    let _backup = backup(&scratch.data(), &copy)?;
    let restored = scratch.path(name);
    let _restored = restore(&copy, &restored)?;
    Ok(restored)
}

/// Moves `artifact` to the trash when `trashed` is true, and recovers it when it is false. The
/// change goes through the core's `update` under the request key `key`, at the base revision
/// that the store holds at the time of the call.
pub(super) fn set_trashed(
    store: &Store,
    artifact: &ArtifactId,
    trashed: bool,
    key: &str,
) -> TestResult {
    let base: i64 = store.sql()?.query_row(
        "SELECT metadata_revision FROM artifacts WHERE id = ?1",
        [artifact.as_str()],
        |row| row.get(0),
    )?;
    let change = MetadataChange::new(None, None, Some(trashed))
        .ok_or("a trash or a recovery is a metadata change")?;
    store.service()?.update(
        &owner()?,
        &parsed::<RequestKey>(key)?,
        artifact,
        MetadataRevision(u64::try_from(base)?),
        &change,
    )?;
    Ok(())
}

/// Checks that `job` keeps exactly `count` attempt rows for the whole of `span`. The check looks
/// every step until the window ends, so an absence of change is observed over that window; it is
/// not a delay that stands in for a condition.
pub(super) fn keeps_attempts(
    store: &Store,
    job: &JobId,
    count: usize,
    span: Duration,
) -> TestResult {
    let limit = within(span);
    loop {
        let found = store.attempt_names(job)?.len();
        if found != count {
            return Err(format!("{} has {found} attempts, expected {count}", job.as_str()).into());
        }
        if Instant::now() >= limit {
            return Ok(());
        }
        thread::sleep(STEP);
    }
}
