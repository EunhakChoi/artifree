//! The worker's adapter of the execution port for the fixture profile (decision 0057 rules 6, 7,
//! 15, 16, 18 and 19).
//!
//! Implemented, in a Linux build with the `enforcement-fixture` feature only:
//! `FixtureExecution::open` checks the execution directory, its identity file and the delegated
//! start, and writes only the identity file, on first use. `Execution::capacity` takes the
//! admission lock, the host admission and the registry reconciliation, and names the attempts that
//! the caller must settle before its claim. The held capacity's `launch` runs one pinned attempt,
//! renews its guardian only after the store's checkpoint answers `continue`, and returns a typed
//! outcome. Dropping the held capacity releases the lock, so the caller drops it after it has
//! settled the attempt.
//!
//! Test-only, in this feature build only: a file `pause` in the execution directory pauses a launch
//! for the enforcement job (decision 0057, R02bc). Its one line is `<phase> hold` or
//! `<phase> stall <milliseconds>`, where `<phase>` is one of the provider's phase words
//! (`registered`, `created`, `guarded`, `started`, `ready`, `running`, `collecting`) or
//! `settled`, a pause when the held capacity is dropped after the caller settled the attempt and
//! before it publishes. The empty marker file `<phase>` is created in the directory `paused`
//! beside it, which the test makes. A missing, unreadable or malformed file pauses nothing. The
//! record of each launch is a `v2` record naming the database, the job and the fence of the claim.
//! Not here: any wire contract and any command. A release build has no adapter, so its worker
//! claims trusted kinds and ends unknown-profile jobs only (0057 rule 2), and has no pause.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use artifree_domain::job::{AttemptIdentity, AttemptState, CheckpointAnswer, StopReason};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_ports::artifact_store::FileEntry;
use artifree_ports::execution::{
    BoundRun, Capacity, Checkpoint, CheckpointUnavailable, Execution, ExecutionHost, HeldCapacity,
    LaunchFailure, LaunchOutcome, PinnedInput, ProfileOutput, ProfileRun,
};
use artifree_ports::job_store::{AttemptRow, ProfileCounts};

use super::admission;
use super::attempt::{
    self, AttemptEvidence, AttemptFailure, AttemptPlan, Heartbeat, Launched, LeaseLoss,
    OutputRefusal, Pause, PauseMode, Phase,
};
use super::cgroup;
use super::guardian::GuardianReason;
use super::launcher::EntryProgram;
use super::profile;
use super::registry::{self, Identity, QuarantineReason, RecordRun};
use super::staging::{InputListing, VerifiedInput};
use super::{AdmissionRefusal, read_bounded};

/// The file of the execution directory that holds its identity (decision 0057 rule 6).
const DIRECTORY_IDENTITY_FILE: &str = "directory-identity";
/// The largest identity file that is read. A longer file is malformed and refuses the start.
const IDENTITY_FILE_BYTES: u64 = 64;
/// The pause file of the execution directory (see the module documentation).
const PAUSE_FILE: &str = "pause";
/// The directory beside the pause file that receives the marker files; the test makes it.
const PAUSED_DIR: &str = "paused";
/// The pause word of the pause after the caller settled the attempt and before it publishes.
const SETTLED_WORD: &str = "settled";
/// The largest pause file that is read.
const PAUSE_BYTES: u64 = 64;

/// Why the worker refuses to start with an execution directory (decision 0057 rule 18). The text
/// names the missing facility, and the worker prints it on stderr.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ExecutionStartRefusal {
    /// The directory is a link or another non-directory, or it is not an owner-only directory of
    /// this account.
    Directory,
    /// The directory's identity file cannot be read or written, or it is not a private regular
    /// file of this account that holds one identity.
    Identity,
    /// The delegated start refused with the first missing facility.
    Facility(AdmissionRefusal),
}

impl fmt::Display for ExecutionStartRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directory => formatter
                .write_str("the execution directory is not a private directory of this account"),
            Self::Identity => formatter
                .write_str("the execution directory's identity could not be read or written"),
            Self::Facility(refusal) => fmt::Display::fmt(refusal, formatter),
        }
    }
}

impl std::error::Error for ExecutionStartRefusal {}

/// The fixture profile's adapter of the execution port: one execution directory whose identity and
/// delegation were checked at start (decision 0057 rules 6 and 18).
#[derive(Debug)]
pub struct FixtureExecution {
    /// The owner-only execution directory of the lock, the registry and the attempts.
    execution_dir: PathBuf,
    /// The directory's identity, 32 lowercase hexadecimal digits.
    directory: String,
}

impl FixtureExecution {
    /// Checks the execution directory and its identity, then the delegated start of this process,
    /// and returns the adapter. It writes only the identity file, and only on first use. It takes
    /// no lock, claims nothing and launches nothing.
    ///
    /// # Errors
    ///
    /// Returns `Directory` when the path is not an owner-only directory of this account, `Identity`
    /// when the identity file is missing and cannot be made, or present but not a private file
    /// that holds one identity, and `Facility` with the first missing facility of the delegated
    /// start.
    pub fn open(execution_dir: &Path) -> Result<Self, ExecutionStartRefusal> {
        let uid = cgroup::own_uid(Path::new("/proc/self"))
            .map_err(|_| ExecutionStartRefusal::Directory)?;
        let metadata =
            fs::symlink_metadata(execution_dir).map_err(|_| ExecutionStartRefusal::Directory)?;
        if !metadata.file_type().is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(ExecutionStartRefusal::Directory);
        }
        let directory = directory_identity(execution_dir, uid)?;
        admission::check_delegated_start(&admission::production_roots())
            .map_err(ExecutionStartRefusal::Facility)?;
        Ok(Self {
            execution_dir: execution_dir.to_path_buf(),
            directory,
        })
    }
}

impl Execution for FixtureExecution {
    fn capacity(&self, unsettled: &[AttemptRow]) -> Capacity {
        let held = match attempt::hold(&self.execution_dir) {
            Ok(held) => held,
            Err(refusal) if refusal.failure == AttemptFailure::Busy => return Capacity::Busy,
            Err(refusal) => return Capacity::Refused(word(refusal.failure)),
        };
        let host = ExecutionHost {
            machine: held.host.machine.clone(),
            boot: held.host.boot.clone(),
            cgroupns: held.host.cgroupns.clone(),
        };
        let removed: Vec<[u8; 16]> = held
            .reconciled
            .iter()
            .map(|record| record.identity.0)
            .collect();
        let (reconciled, foreign_pending) = settleable(unsettled, &removed, &host, &self.directory);
        if foreign_pending {
            // A row of another host that is still `cleanup_pending` keeps the provider quarantined
            // until the operator acts (decision 0057 rule 19).
            // Whether or not the file was written, the operator's text says what to do (decision
            // 0057 rule 13); an unwritten file is retried at the next capacity.
            let _written = attempt::quarantine(&self.execution_dir, QuarantineReason::Registry);
            return Capacity::Refused(registry::OPERATOR_TEXT);
        }
        let Ok(identity) = Identity::random() else {
            return Capacity::Refused("no-randomness");
        };
        let tool_digest = held.report.tool_manifest_sha256.clone();
        Capacity::Held(Box::new(HeldFixture {
            held,
            host,
            directory: self.directory.clone(),
            tool_digest,
            reconciled,
            attempt: AttemptIdentity::from_random(identity.0),
            execution_dir: self.execution_dir.clone(),
            launched: false,
            settled_pause: None,
        }))
    }
}

/// The capacity of one fixture attempt: the held lock, the host admission and reconciliation, and
/// the attempt identity drawn with them (decision 0057 rules 6 and 19). Its launch runs at most
/// once.
#[derive(Debug)]
struct HeldFixture {
    /// The admission lock, the host admission and the reconciliation. Dropping it releases the
    /// lock.
    held: attempt::Held,
    /// The host identity that the capacity was admitted on.
    host: ExecutionHost,
    /// The execution directory's identity.
    directory: String,
    /// The tool root's manifest digest, in lowercase hexadecimal.
    tool_digest: String,
    /// The unsettled attempts that the caller settles before its claim.
    reconciled: Vec<AttemptIdentity>,
    /// The attempt identity that the claim and the launch both use.
    attempt: AttemptIdentity,
    /// The execution directory that holds the record and the staged input of the launch.
    execution_dir: PathBuf,
    /// Whether the launch already ran, so that a second call launches nothing.
    launched: bool,
    /// The pause at `settled`, read at the launch and kept only when the launch observed its
    /// cleanup, so that the core settles the attempt before the pause.
    settled_pause: Option<PauseMode>,
}

impl HeldCapacity for HeldFixture {
    fn host(&self) -> &ExecutionHost {
        &self.host
    }

    fn directory(&self) -> &str {
        &self.directory
    }

    fn tool_digest(&self) -> &str {
        &self.tool_digest
    }

    fn attempt(&self) -> AttemptIdentity {
        self.attempt
    }

    fn reconciled(&self) -> &[AttemptIdentity] {
        &self.reconciled
    }

    fn launch(
        &mut self,
        run: &ProfileRun<'_>,
        input: PinnedInput,
        checkpoint: &dyn Checkpoint,
    ) -> LaunchOutcome {
        // The job's profile name selects a reviewed profile, and nothing else of the request does
        // (decision 0058 rule 1).
        let Some(profile) = profile::by_name(run.profile.as_str()) else {
            return not_launched("not-launched");
        };
        if self.launched || run.attempt != self.attempt {
            return not_launched("not-launched");
        }
        self.launched = true;
        let pause = read_pause(&self.execution_dir);
        let PinnedInput { package, listing } = input;
        let Ok(verified) = VerifiedInput::verify(package, listing_of(listing)) else {
            return not_launched("input-refused");
        };
        let plan = AttemptPlan {
            lease: Duration::from_millis(run.lease_ms),
            program: EntryProgram::Server,
            profile,
            checkpoint_at_release: true,
            pause: pause.phase,
            ..AttemptPlan::fixture()
        };
        let mut renew = |_heartbeat: u32| heartbeat_of(checkpoint.renew());
        let launched = attempt::launch_held(
            &plan,
            &mut renew,
            &self.held,
            Identity(self.attempt.bytes()),
            verified,
            Some((
                BoundRun::of(run),
                RecordRun {
                    database: run.database,
                    job: run.job.clone(),
                    fence: run.fence,
                },
            )),
            &self.execution_dir,
        );
        // Only a launch whose cleanup was observed is settled before the capacity is dropped; an
        // unsettled one must not mark `settled`.
        if launched.record_removed {
            self.settled_pause = pause.settled;
        }
        outcome(&launched, run, &self.tool_digest)
    }
}

impl Drop for HeldFixture {
    // The core drops the capacity after it settled the attempt and before it publishes (decision
    // 0057 rule 10), so a pause here is the `settled` phase of the recovery matrix (R02bc).
    fn drop(&mut self) {
        if self.launched
            && let Some(mode) = self.settled_pause
        {
            attempt::pause_marked(&self.execution_dir.join(PAUSED_DIR), SETTLED_WORD, mode);
        }
    }
}

/// The outcome of a launch that started no payload. Nothing ran, so its cleanup is observed.
fn not_launched(detail: &'static str) -> LaunchOutcome {
    LaunchOutcome {
        cleanup_observed: true,
        result: Err(LaunchFailure::Unavailable),
        detail,
    }
}

/// The test-only pause that the pause file asks for.
#[derive(Debug, Default)]
struct PauseRequest {
    /// A pause at one of the provider's phases.
    phase: Option<Pause>,
    /// A pause when the held capacity is dropped after its launch.
    settled: Option<PauseMode>,
}

/// Reads the pause file of `execution_dir`. A missing, unreadable or malformed file asks for
/// no pause.
fn read_pause(execution_dir: &Path) -> PauseRequest {
    let Ok(bytes) = read_bounded(&execution_dir.join(PAUSE_FILE), PAUSE_BYTES) else {
        return PauseRequest::default();
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return PauseRequest::default();
    };
    let words: Vec<&str> = text.split_whitespace().collect();
    let (word, mode) = match words.as_slice() {
        [word, "hold"] => (*word, PauseMode::Hold),
        [word, "stall", millis] => match millis.parse::<u64>() {
            Ok(millis) => (*word, PauseMode::Stall(Duration::from_millis(millis))),
            Err(_) => return PauseRequest::default(),
        },
        _ => return PauseRequest::default(),
    };
    if word == SETTLED_WORD {
        return PauseRequest {
            phase: None,
            settled: Some(mode),
        };
    }
    PauseRequest {
        phase: Phase::from_word(word).map(|phase| Pause {
            phase,
            mode,
            markers: execution_dir.join(PAUSED_DIR),
        }),
        settled: None,
    }
}

/// Reads the execution directory's identity, creating it on first use (decision 0057 rule 6).
fn directory_identity(execution_dir: &Path, uid: u32) -> Result<String, ExecutionStartRefusal> {
    let path = execution_dir.join(DIRECTORY_IDENTITY_FILE);
    match fs::symlink_metadata(&path) {
        Ok(_) => read_identity(&path, uid),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_identity(execution_dir, &path)?;
            read_identity(&path, uid)
        }
        Err(_) => Err(ExecutionStartRefusal::Identity),
    }
}

/// Writes a new identity file with mode 0600: the text goes to a temporary file of its own name,
/// which is synced and then linked to `path`, so the identity file appears whole or not at all and
/// a failed write leaves nothing that refuses a later start. A file that another process linked
/// first is left for `read_identity` to check. The temporary name is removed either way, and the
/// directory is synced.
fn create_identity(execution_dir: &Path, path: &Path) -> Result<(), ExecutionStartRefusal> {
    let identity = Identity::random().map_err(|_| ExecutionStartRefusal::Identity)?;
    let temporary = execution_dir.join(format!(".directory-identity.{}.tmp", identity.hex()));
    let written = write_synced(&temporary, &format!("{}\n", identity.hex()));
    let linked = written.and_then(|()| match fs::hard_link(&temporary, path) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => Err(error),
        _ => Ok(()),
    });
    let removed = fs::remove_file(&temporary);
    linked.map_err(|_| ExecutionStartRefusal::Identity)?;
    removed.map_err(|_| ExecutionStartRefusal::Identity)?;
    fs::File::open(execution_dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ExecutionStartRefusal::Identity)
}

/// Creates `path` exclusively with mode 0600, writes `text` and syncs it.
fn write_synced(path: &Path, text: &str) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// Reads an identity file that must be a private regular file of this account, holding exactly
/// 32 lowercase hexadecimal digits and one trailing newline.
fn read_identity(path: &Path, uid: u32) -> Result<String, ExecutionStartRefusal> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ExecutionStartRefusal::Identity)?;
    if !metadata.file_type().is_file() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(ExecutionStartRefusal::Identity);
    }
    let bytes =
        read_bounded(path, IDENTITY_FILE_BYTES).map_err(|_| ExecutionStartRefusal::Identity)?;
    let digits = bytes
        .strip_suffix(b"\n")
        .ok_or(ExecutionStartRefusal::Identity)?;
    let text = std::str::from_utf8(digits).map_err(|_| ExecutionStartRefusal::Identity)?;
    Identity::parse_hex(text)
        .map(|identity| identity.hex())
        .ok_or(ExecutionStartRefusal::Identity)
}

/// Selects the unsettled attempts that the caller settles before its claim (decision 0057 rule
/// 19): those that reconciliation removed, and those without a record that belong to this machine,
/// cgroup namespace and execution directory. The flag is set by another row that is
/// `cleanup_pending`. A live row of another host is left alone, since its worker may still run it.
fn settleable(
    unsettled: &[AttemptRow],
    removed: &[[u8; 16]],
    host: &ExecutionHost,
    directory: &str,
) -> (Vec<AttemptIdentity>, bool) {
    let mut settle = Vec::new();
    let mut foreign_pending = false;
    for row in unsettled {
        let ours = row.machine == host.machine
            && row.cgroupns == host.cgroupns
            && row.directory == directory;
        if ours || removed.contains(&row.attempt.bytes()) {
            settle.push(row.attempt);
        } else if matches!(row.state, AttemptState::CleanupPending) {
            foreign_pending = true;
        }
    }
    (settle, foreign_pending)
}

/// Turns the store's checkpoint answer into the guardian's heartbeat (decision 0057 rule 7). A
/// `continue` confirms, a stop names its part, and an unavailable store is asked again later.
fn heartbeat_of(answer: Result<CheckpointAnswer, CheckpointUnavailable>) -> Heartbeat {
    match answer {
        Ok(CheckpointAnswer::Continue) => Heartbeat::Confirmed,
        Ok(CheckpointAnswer::Stop(reason)) => Heartbeat::Lost(lease_loss_of(reason)),
        Err(_) => Heartbeat::Unavailable,
    }
}

/// The part of a lease that a stop reason names.
fn lease_loss_of(reason: StopReason) -> LeaseLoss {
    match reason {
        StopReason::Cancel => LeaseLoss::Cancelled,
        StopReason::Trash => LeaseLoss::Trash,
        StopReason::Purge => LeaseLoss::Purge,
        StopReason::Generation => LeaseLoss::Generation,
        StopReason::Authority => LeaseLoss::Authority,
        StopReason::Lease => LeaseLoss::Lease,
        StopReason::Fence => LeaseLoss::Fence,
    }
}

/// The stop reason of a lost part of a lease.
fn stop_reason_of(loss: LeaseLoss) -> StopReason {
    match loss {
        LeaseLoss::Fence => StopReason::Fence,
        LeaseLoss::Cancelled => StopReason::Cancel,
        LeaseLoss::Generation => StopReason::Generation,
        LeaseLoss::Authority => StopReason::Authority,
        LeaseLoss::Trash => StopReason::Trash,
        LeaseLoss::Purge => StopReason::Purge,
        LeaseLoss::Lease => StopReason::Lease,
    }
}

/// The listing of the pinned input in the path order that `VerifiedInput` compares against.
fn listing_of(entries: Vec<FileEntry>) -> InputListing {
    let mut listing: Vec<(PackagePath, u64, [u8; 32])> = entries
        .into_iter()
        .map(|entry| (entry.path, entry.size, entry.sha256))
        .collect();
    listing.sort_by(|left, right| left.0.cmp(&right.0));
    InputListing(listing)
}

/// The number of files and bytes of an admitted output, saturating at `u64::MAX`.
fn counts_of(package: &FilePackage) -> ProfileCounts {
    let bytes = package.files().fold(0_u64, |total, (_, content)| {
        total.saturating_add(u64::try_from(content.len()).unwrap_or(u64::MAX))
    });
    ProfileCounts {
        files: u64::try_from(package.len()).unwrap_or(u64::MAX),
        bytes,
    }
}

/// The outcome of a launch that was observed to its end. An admitted output carries its counts and
/// its binding (decision 0057 rule 17). Any other end is a failure with its fixed word.
fn evidence_outcome(
    evidence: &AttemptEvidence,
    run: &ProfileRun<'_>,
    tool_digest: &str,
) -> (Result<ProfileOutput, LaunchFailure>, &'static str) {
    match &evidence.output {
        Ok(collected) => match collected.manifest.binding().run.clone() {
            Some(bound) => (
                Ok(ProfileOutput {
                    counts: counts_of(&collected.package),
                    profile: run.profile.clone(),
                    tool_digest: tool_digest.to_owned(),
                    bound,
                }),
                "succeeded",
            ),
            None => (Err(LaunchFailure::Failed), "unbound"),
        },
        Err(OutputRefusal::NotCollected) => (Err(LaunchFailure::Failed), "exit-nonzero"),
        Err(OutputRefusal::Refused(_)) => (Err(LaunchFailure::Failed), "output-refused"),
    }
}

/// The typed outcome of one launch: the cleanup that `record_removed` reports, the output or the
/// failure, and the fixed word for stderr.
fn outcome(launched: &Launched, run: &ProfileRun<'_>, tool_digest: &str) -> LaunchOutcome {
    let (result, detail) = match &launched.result {
        Ok(evidence) => evidence_outcome(evidence, run, tool_digest),
        Err(refusal) => (Err(refusal_kind(refusal.failure)), word(refusal.failure)),
    };
    LaunchOutcome {
        cleanup_observed: launched.record_removed,
        result,
        detail,
    }
}

/// The kind of a launch that produced no output (decision 0057 rules 7 and 15). A refusal before
/// the payload is `Unavailable`, a payload whose verdict failed is `Failed`, and a stop is
/// `Stopped`.
fn refusal_kind(failure: AttemptFailure) -> LaunchFailure {
    match failure {
        AttemptFailure::LeaseLost(part) => LaunchFailure::Stopped(stop_reason_of(part)),
        AttemptFailure::Busy
        | AttemptFailure::LockUnusable
        | AttemptFailure::Quarantined
        | AttemptFailure::QuarantineNotPersisted
        | AttemptFailure::Refused(_)
        | AttemptFailure::PolicyUnavailable
        | AttemptFailure::NotStarted(_)
        | AttemptFailure::Input(_) => LaunchFailure::Unavailable,
        AttemptFailure::Unobserved
        | AttemptFailure::DeadlineExceeded
        | AttemptFailure::OutOfMemory
        | AttemptFailure::Survivors
        | AttemptFailure::OutputLimitExceeded
        | AttemptFailure::CleanupNotObserved
        | AttemptFailure::Routing(_)
        | AttemptFailure::Guardian(_) => LaunchFailure::Failed,
    }
}

/// The fixed word that names a failed attempt on stderr (decision 0057 rule 15). It never holds a
/// path, a command or content.
fn word(failure: AttemptFailure) -> &'static str {
    match failure {
        AttemptFailure::Busy => "busy",
        AttemptFailure::LockUnusable => "lock-unusable",
        // The operator's text, so that the stderr line says what to do (decision 0057 rule 13).
        AttemptFailure::Quarantined | AttemptFailure::QuarantineNotPersisted => {
            failure.operator_text().unwrap_or("quarantined")
        }
        AttemptFailure::Refused(_) => "admission-refused",
        AttemptFailure::PolicyUnavailable => "policy-unavailable",
        AttemptFailure::NotStarted(_) => "not-started",
        AttemptFailure::Input(_) => "input-refused",
        AttemptFailure::Unobserved => "unobserved",
        AttemptFailure::DeadlineExceeded => "deadline-exceeded",
        AttemptFailure::OutOfMemory => "out-of-memory",
        AttemptFailure::Survivors => "survivors",
        AttemptFailure::OutputLimitExceeded => "output-limit-exceeded",
        AttemptFailure::CleanupNotObserved => "cleanup-not-observed",
        AttemptFailure::Routing(_) => "routing-changed",
        AttemptFailure::LeaseLost(part) => stop_word(stop_reason_of(part)),
        AttemptFailure::Guardian(reason) => guardian_word(reason),
    }
}

/// The fixed word of a stop, named by its reason.
fn stop_word(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Cancel => "stopped-cancel",
        StopReason::Trash => "stopped-trash",
        StopReason::Purge => "stopped-purge",
        StopReason::Generation => "stopped-generation",
        StopReason::Authority => "stopped-authority",
        StopReason::Lease => "stopped-lease",
        StopReason::Fence => "stopped-fence",
    }
}

/// The fixed word of a guardian's reason.
fn guardian_word(reason: GuardianReason) -> &'static str {
    match reason {
        GuardianReason::SupervisorGone => "guardian-supervisor-gone",
        GuardianReason::RenewalExpired => "guardian-renewal-expired",
        GuardianReason::Deadline => "guardian-deadline",
    }
}

#[cfg(test)]
mod tests;
