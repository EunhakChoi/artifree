//! One trusted launch of the fixture stub in its own attempt group (decision 0056 rule 5 and
//! decision 0042 rules 5 and 6).
//!
//! Compiled into the test build and into the `enforcement-fixture` build (decision 0057 rule 2;
//! 0056 rule 16). Nothing here is visible outside `execution`.
//!
//! R02ac (0056 rule 14) added the resource part of the group phase. The scratch bounds are read
//! back from the bootstrap's mount table (`readiness`), and the stub runs behind the two seccomp
//! programs of `policy`. The group's counters are read once the group is seen empty (`usage`).
//! After the stub's exit the group's listing decides survivors once the PID namespace's
//! teardown has had `TEARDOWN_GRACE` to finish, and the verdict is typed:
//! `OutOfMemory` and `Survivors` are decided in the order that `final_verdict` documents. The
//! payload scenarios that exercise these limits run only in the enforcement job (`run.sh`).
//!
//! R02af (0056 rule 14, amendment of 2026-11-13) adds the staged input, the keeper and the
//! collection of the output root after a passing verdict. The input is staged under the admission
//! lock, before the group exists, and removed when the attempt ends. The keeper holds the output
//! root from the observation to the release, and is dropped unread on every path but a collected
//! one.
//!
//! R02ag (0056 rule 14, amendment of 2026-11-13, crashes) reads `kernel.core_pattern` again at
//! each poll of the stub and once after the group is empty. A change or a failed read ends the
//! group, quarantines the provider in its process and refuses the attempt as `Routing`. A
//! test-only step of the plan lists the scratch through the keeper.
//!
//! R02ah (0056 rule 14, amendment of 2026-11-13, the guardian) starts the guardian after the
//! limits and before the stub, and waits for its ready byte. The provider sends `L` at the
//! release, `R` after each confirming heartbeat, `S` once it no longer polls the stub and `D` once
//! the group is gone. A guardian that is not reaped without a kill leaves the cleanup unobserved.
//! A reason that the guardian reports refuses the attempt as `Guardian`, after `Routing`. The
//! plan's test-only switches and pauses serve the job tests of the enforcement job only.
//!
//! R02ai (0056 rule 14, amendment of 2026-11-14, the attempt registry) checks the persisted
//! quarantine before admission, reconciles the registry after it, writes the attempt's record
//! before the input is staged and names the group by the record's identity. The record is removed
//! only when the attempt did not quarantine, its group was observed gone and its input removed.
//! Every quarantine also writes the quarantine file; a file that was not written refuses the
//! attempt as `QuarantineNotPersisted`, first.
//!
//! R02bb (decision 0057 rules 6, 7 and 19) splits the launch into `hold`, which takes the lock,
//! checks the quarantine, admits the host and reconciles the registry, and `launch_held`, which
//! runs one attempt on the capacity that `hold` took. The worker claims between the two, and the
//! record carries the attempt identity that its claim recorded. The guardian's renewals are asked
//! from the store's checkpoint (`renew`), and the guardian is renewed only after a `continue`.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(test)]
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_ports::execution::BoundRun;
use sha2::{Digest, Sha256};

use super::admission::{self, AdmissionRoots, wait_within};
use super::bootstrap::{EXEC_FAILED, LAUNCH, READY, RELEASE};
use super::cgroup;
use super::collection::{CollectedOutput, CollectionRefusal, ManifestBinding};
use super::guardian::{self, GuardianReason};
use super::lock::{AdmissionLock, LockRefusal};
use super::policy::FilterSet;
use super::probe_group::{self, ProbeGroup};
use super::profile::{self, ExecutionProfile, FIXTURE_EXECUTION};
use super::readiness::{self, GroupObservation};
use super::registry::{self, HostIdentity, Identity, QuarantineReason, Reconciled, Registry};
use super::report::HostReport;
#[cfg(test)]
use super::staging::InputListing;
use super::staging::{self, InputRefusal, VerifiedInput};
use super::usage::{self, GroupUsage};
use super::{AdmissionRefusal, InspectionProfile, launcher, policy, read_bounded};
use guardian_link::GuardianLink;
use keeper::Keeper;

/// The combined stdout and stderr budget of one attempt, in bytes (decision 0042 rule 5).
const OUTPUT_BUDGET: u64 = 1_048_576;
/// The wall deadline of one attempt, counted from the LAUNCH record (decision 0042 rule 5).
const WALL_DEADLINE: Duration = Duration::from_secs(120);
/// The readiness deadline, counted from the LAUNCH record.
const READINESS_DEADLINE: Duration = Duration::from_secs(10);
/// The pause between two polls of the stub and of the readers.
const POLL: Duration = Duration::from_millis(10);

/// How much longer than the cleanup observation deadline the provider waits for the guardian's line
/// when it closed the socket without `D`: the guardian then kills the group and waits up to that
/// deadline itself before it writes the line.
const GUARDIAN_REPORT_MARGIN: Duration = Duration::from_secs(1);
/// How long the provider waits on the control socket once the stub has exited.
const CONTROL_GRACE: Duration = Duration::from_secs(1);
/// The largest read that one pipe reader makes at a time.
const CHUNK_BYTES: usize = 8192;
/// The largest `cgroup.procs` listing that the launch reads when it places the stub.
const LISTING_BYTES: u64 = 4096;
/// How long the group may take to empty after the stub's exit. The launcher can exit before the
/// kernel has ended the rest of its PID namespace: on the job's runner a process bomb's children
/// were still listed when the stub's exit was observed (main, 2026-11-12). A process that is
/// still listed after this wait is a survivor. The wait never runs past the wall deadline.
const TEARDOWN_GRACE: Duration = Duration::from_secs(2);
/// The bytes of the fixture input's one page, `index.html`.
#[cfg(test)]
const FIXTURE_PAGE: &[u8] = b"<!doctype html><title>fixture</title>";
/// The fixture plan's job lease. Its renewal window is a third of it (0042 rule 6).
const FIXTURE_LEASE: Duration = Duration::from_secs(30);
/// The margin of the guardian's deadline after the readiness and wall deadlines (0056, amendment of
/// 2026-11-13, the guardian).
const GUARDIAN_MARGIN: Duration = Duration::from_secs(2);
/// The sleep step of a held pause.
const HOLD_STEP: Duration = Duration::from_secs(1);

/// Set once this process quarantined the provider: a cleanup not observed, a routing change, a
/// guardian not reaped, or a refusal of reconciliation (0042 rule 6; 0056 rule 5, as reworded by
/// the amendments of 2026-11-13 and 2026-11-14). Launches in this process are refused. The
/// persisted quarantine file of the execution directory (`registry::persist_quarantine`) carries it
/// across restarts. The group of a failed cleanup stays in place.
static QUARANTINED: AtomicBool = AtomicBool::new(false);

/// The inputs of one launch: the stub's arguments and its two deadlines. After `list_scratch` come
/// the job's lease, the stand-in store and the guardian's test-only switches (decision 0056,
/// amendment of 2026-11-13, the guardian).
#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the plan's switches are independent and named by decision 0056, amendment of 2026-11-13 (the guardian), and decision 0057 rule 7"
)]
pub(super) struct AttemptPlan {
    /// The tool root's program that runs the stub, the bootstrap and the guardian (decision 0057
    /// rule 4). `EntryProgram::Fixture` in `fixture()`.
    pub(super) program: launcher::EntryProgram,
    /// The execution profile: its identity, payload, filter and manifest (decision 0058 rule 1).
    /// `FIXTURE_EXECUTION` in `fixture()`.
    pub(super) profile: &'static ExecutionProfile,
    /// A test-only replacement of the stub's arguments after the program's prefix, before the
    /// staged input's path. `None` in `fixture()`, so the stub is given `stub` and the profile's
    /// own identity, and the profile it runs is the one the plan names.
    #[cfg(test)]
    pub(super) stub_override: Option<&'static [&'static str]>,
    /// The wall deadline, counted from the LAUNCH record on a monotonic clock.
    pub(super) wall_deadline: Duration,
    /// The readiness deadline, counted from the LAUNCH record on a monotonic clock.
    pub(super) readiness_deadline: Duration,
    /// The bytes sent as the release. Only a test sends anything but RELEASE, to show that the
    /// bootstrap starts no payload on another byte.
    pub(super) release_record: &'static [u8],
    /// A test-only step that adds entries to the output root through the keeper, after its
    /// checks and before the release (0056, amendment of 2026-11-13: planted trees). `None` in
    /// `fixture()`; a failure is `KeeperNotHeld`.
    pub(super) plant: Option<fn(&OwnedFd) -> io::Result<()>>,
    /// A test-only step: after the group is ended, whatever the verdict, list the whole
    /// scratch through the keeper into `AttemptEvidence::scratch` (0056,
    /// amendment of 2026-11-13, crashes). `false` in `fixture()`.
    #[cfg(test)]
    pub(super) list_scratch: bool,
    /// The job's lease, from which the renewal window is a third (0042 rule 6; 0056, amendment of
    /// 2026-11-13, the guardian). `FIXTURE_LEASE` in `fixture()`.
    pub(super) lease: Duration,
    /// The stand-in store: the answer to the heartbeat with this number, counted from 0, given at
    /// once. `always_confirms` in `fixture()`. Test-only: the renewals of a launch are asked from
    /// the `renew` function that its caller passes (decision 0057 rule 7).
    #[cfg(test)]
    pub(super) store: fn(u32) -> Heartbeat,
    /// Whether the guardian is started. `false` leaves it out, so that the launcher's parent-death
    /// signal is shown alone. `true` in `fixture()`.
    pub(super) guardian: bool,
    /// Whether `running_verdict` checks the wall deadline. `false` switches off that part only,
    /// never the output budget. `true` in `fixture()`.
    pub(super) provider_deadline: bool,
    /// The faulty supervisor of 0042's acceptance: it sends `R` after every heartbeat whatever its
    /// answer, and never ends the group on it. `false` in `fixture()`.
    pub(super) renew_regardless: bool,
    /// A test-only pause at one phase: set by the provider's tests and, in the
    /// `enforcement-fixture` build, by the worker's adapter from its pause file (decision 0057,
    /// R02bc). `None` in `fixture()`.
    pub(super) pause: Option<Pause>,
    /// Whether the store's checkpoint is asked once more after readiness and before the release,
    /// and the payload released only on its confirmation (decision 0042 rule 3: the generation
    /// is compared at readiness; decision 0057 rule 7). The worker sets it; `false` in
    /// `fixture()`, so that the provider's own tests keep their heartbeat numbering.
    pub(super) checkpoint_at_release: bool,
}

impl AttemptPlan {
    /// The renewal window: a third of the lease (0042 rule 6). The provider's heartbeat schedule and
    /// the guardian's window both use this value.
    #[must_use]
    pub(super) fn renewal_window(&self) -> Duration {
        self.lease / 3
    }

    /// The default plan: the fixture program, the fixture stub and the decision 0042 deadlines.
    #[must_use]
    pub(super) fn fixture() -> Self {
        Self {
            program: launcher::EntryProgram::Fixture,
            profile: &FIXTURE_EXECUTION,
            #[cfg(test)]
            stub_override: None,
            wall_deadline: WALL_DEADLINE,
            readiness_deadline: READINESS_DEADLINE,
            release_record: &RELEASE,
            plant: None,
            #[cfg(test)]
            list_scratch: false,
            lease: FIXTURE_LEASE,
            #[cfg(test)]
            store: always_confirms,
            guardian: true,
            provider_deadline: true,
            renew_regardless: false,
            pause: None,
            checkpoint_at_release: false,
        }
    }
}

/// The fixture plan's stand-in store: it confirms every heartbeat.
#[cfg(test)]
fn always_confirms(_heartbeat: u32) -> Heartbeat {
    Heartbeat::Confirmed
}

/// A test-only pause of the provider at one phase. At the phase the provider creates the empty
/// file `<markers>/<word>` and then holds or stalls, as `mode` says (0056, amendment of
/// 2026-11-13, the guardian).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Pause {
    /// The phase at which the pause takes place.
    pub(super) phase: Phase,
    /// What the provider does once the marker file exists.
    pub(super) mode: PauseMode,
    /// The directory that receives the marker file, named by the phase's word.
    pub(super) markers: PathBuf,
}

/// The phases of one launch at which a test may pause the provider (0056, amendment of
/// 2026-11-13, the guardian, and amendment of 2026-11-14, the attempt registry).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Phase {
    /// `registered`: the attempt's record is durable; nothing else exists (0056, amendment of
    /// 2026-11-14).
    Registered,
    /// `created`: the input is staged and the group exists; the guardian and the stub do not
    /// (0056, amendment of 2026-11-14).
    Created,
    /// `guarded`: the group and the guardian exist, the stub does not.
    Guarded,
    /// `started`: the stub runs, before the readiness report is read.
    Started,
    /// `ready`: the report was read, before the release.
    Ready,
    /// `running`: after the release.
    Running,
    /// `collecting`: after a passing verdict and `D`, before collection.
    Collecting,
}

impl Phase {
    /// The word that names this phase in its marker file.
    #[must_use]
    pub(super) fn word(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Created => "created",
            Self::Guarded => "guarded",
            Self::Started => "started",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Collecting => "collecting",
        }
    }

    /// The phase named by `word`, as `word` writes it.
    #[cfg(feature = "enforcement-fixture")]
    #[must_use]
    pub(super) fn from_word(word: &str) -> Option<Self> {
        [
            Self::Registered,
            Self::Created,
            Self::Guarded,
            Self::Started,
            Self::Ready,
            Self::Running,
            Self::Collecting,
        ]
        .into_iter()
        .find(|phase| phase.word() == word)
    }
}

/// What a paused provider does once its marker file exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PauseMode {
    /// Waits, in a loop, until the process is killed.
    Hold,
    /// Waits for the duration and then goes on.
    Stall(Duration),
}

/// The stand-in store's answer to one heartbeat (0056, amendment of 2026-11-13, the guardian).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Heartbeat {
    /// The current fence is confirmed, with no cancel, the pinned actor generation and continuing
    /// authority.
    Confirmed,
    /// The store did not answer.
    Unavailable,
    /// The lease was lost, with the part that failed.
    Lost(LeaseLoss),
}

/// The part of a lease that a heartbeat found lost (0056, amendment of 2026-11-13, the guardian).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LeaseLoss {
    /// The fence was revoked.
    Fence,
    /// A cancel was requested.
    Cancelled,
    /// The actor generation changed.
    Generation,
    /// The authority was lost.
    Authority,
    /// The artifact was trashed (decision 0057 rule 7). Only the worker's checkpoint answers it.
    #[cfg(feature = "enforcement-fixture")]
    Trash,
    /// The artifact was purged (decision 0057 rule 7). Only the worker's checkpoint answers it.
    #[cfg(feature = "enforcement-fixture")]
    Purge,
    /// A revocation of the lease was stored while the run went on (decisions 0057 rules 7 and 13).
    /// Only the worker's checkpoint answers it.
    #[cfg(feature = "enforcement-fixture")]
    Lease,
}

/// Why the stub did not reach a run, with the step that failed (decision 0056 rule 5).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NotStartedReason {
    /// The stub could not be spawned or its pipes read, or the LAUNCH record was not sent.
    StubNotStarted,
    /// The stub's pid did not appear in the attempt group's `cgroup.procs`.
    StubNotPlaced,
    /// The READY record did not arrive whole before the readiness deadline.
    NoReadiness,
    /// The provider's independent observation refused the group before release.
    ReadinessNotConfirmed,
    /// The keeper could not open the scratch through `/proc/<bootstrap>/root/tmp`, the descriptor
    /// was not the checked scratch tmpfs, the bootstrap was no longer a member in filter mode, the
    /// output root could not be made, or the plan's plant step failed (0056 rule 5, as reworded by
    /// the amendment of 2026-11-13).
    KeeperNotHeld,
    /// The RELEASE record could not be sent.
    ReleaseNotSent,
    /// The bootstrap reported that the payload's `exec` returned.
    ExecFailed,
    /// The control socket gave neither end of stream nor `EXEC_FAILED`.
    ExecUnconfirmed,
    /// The guardian could not be started, or it did not write its ready byte within the readiness
    /// deadline (0056, amendment of 2026-11-13, the guardian).
    GuardianNotStarted,
    /// The attempt's record was not written, or the host identity for it could not be read, before
    /// any resource existed (0056, amendment of 2026-11-14, the attempt registry).
    RegistryNotWritten,
    /// The store did not answer the checkpoint asked just before the release, so the payload was
    /// not released (decision 0042 rule 3; decision 0057 rule 7).
    CheckpointUnavailable,
}

/// Why a launch produced no evidence of a run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AttemptFailure {
    /// Another process holds the admission lock.
    Busy,
    /// The admission lock could not be used.
    LockUnusable,
    /// The provider is quarantined: in this process (a cleanup not observed, a routing change, a
    /// guardian not reaped, a refusal of reconciliation), or by the quarantine file of the
    /// execution directory, which exists or cannot be looked up. Operator action is required
    /// (`operator_text`).
    Quarantined,
    /// The attempt quarantined the provider, but the quarantine file was not written: the
    /// quarantine holds in this process only, and a restart would not remember it (0056, amendment
    /// of 2026-11-14). Operator action is required (`operator_text`).
    QuarantineNotPersisted,
    /// The host admission refused, with its refusal.
    Refused(AdmissionRefusal),
    /// The fixture filter could not be built for the policy digest.
    PolicyUnavailable,
    /// The stub did not reach a run.
    NotStarted(NotStartedReason),
    /// The input was not verified, staged or removed (0056, amendment of 2026-11-13, input and
    /// output).
    Input(InputRefusal),
    /// The stub's state could not be read, or its output could not be read back.
    Unobserved,
    /// The stub was still running at the wall deadline.
    DeadlineExceeded,
    /// The group's memory limit killed it (`memory.events` `oom_kill` above zero).
    OutOfMemory,
    /// A process was still in the attempt group when the stub's normal exit was observed
    /// (decision 0042 rule 6).
    Survivors,
    /// The stub's output went past the combined budget.
    OutputLimitExceeded,
    /// The group, the stub or a reader was not observed gone, so the attempt is quarantined.
    CleanupNotObserved,
    /// `kernel.core_pattern` changed or could not be read during the attempt, at a poll of the
    /// stub or after the group was empty (0056 rule 5, as reworded by the amendment of
    /// 2026-11-13, crashes). The group was ended as at the wall deadline, the provider is
    /// quarantined in its process, and the quarantine file of the execution directory is written
    /// (amendment of 2026-11-14).
    Routing(AdmissionRefusal),
    /// A heartbeat did not confirm the lease, with the part that failed. The renewals stopped, and
    /// the group was ended as at the wall deadline (0056, amendment of 2026-11-13, the guardian).
    LeaseLost(LeaseLoss),
    /// The guardian ended the attempt, with its reason, even when the stub's exit was also seen
    /// (0056, amendment of 2026-11-13, the guardian).
    Guardian(GuardianReason),
}

impl AttemptFailure {
    /// The operator's text of a quarantine refusal (0056, amendment of 2026-11-14, the attempt
    /// registry): `Some` for `Quarantined` and `QuarantineNotPersisted`, `None` otherwise.
    #[must_use]
    pub(super) fn operator_text(self) -> Option<&'static str> {
        match self {
            Self::Quarantined | Self::QuarantineNotPersisted => Some(registry::OPERATOR_TEXT),
            _ => None,
        }
    }
}

/// Why a launch produced no evidence of a run, with the group's counters when they were read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AttemptRefusal {
    /// The reason that the launch produced no evidence.
    pub(super) failure: AttemptFailure,
    /// The group's counters. Present whenever the group was created, emptied and read, so that a
    /// refused attempt still shows what it used.
    pub(super) usage: Option<GroupUsage>,
}

/// Why an attempt that ran yields no output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OutputRefusal {
    /// The stub's exit status was not 0, so the output root was dropped unread.
    NotCollected,
    /// Collection refused the output root with its first reason.
    Refused(CollectionRefusal),
}

/// A refusal of a launch that has no group counters: the failure came before the group was
/// created, or the group was not read.
fn without_usage(failure: AttemptFailure) -> AttemptRefusal {
    AttemptRefusal {
        failure,
        usage: None,
    }
}

/// Evidence of one launch that ran to an observed exit.
#[derive(Debug)]
pub(super) struct AttemptEvidence {
    /// The attempt group's directory, which the launch removed.
    pub(super) group: PathBuf,
    /// The limits read back from the attempt group, as control file and value.
    pub(super) limits: Vec<(&'static str, String)>,
    /// How the stub exited.
    pub(super) exit: ExitStatus,
    /// The captured standard output, which is untrusted.
    pub(super) stdout: Vec<u8>,
    /// The captured standard error, which is untrusted.
    pub(super) stderr: Vec<u8>,
    /// The time from the LAUNCH record to the observed exit.
    pub(super) wall_time: Duration,
    /// The attempt group's peak memory in bytes, when the kernel offers it.
    pub(super) peak_memory_bytes: Option<u64>,
    /// The attempt group's counters, read after it was seen empty.
    pub(super) usage: GroupUsage,
    /// What the provider saw in the group before it released the bootstrap.
    pub(super) readiness: GroupObservation,
    /// SHA-256 of the seccomp program, in lowercase hexadecimal.
    pub(super) filter_sha256: String,
    /// The host admission's evidence.
    pub(super) report: HostReport,
    /// The collected output with its manifest, or why there is none. Collected only after a
    /// passing verdict with exit status 0 (0042 rule 7).
    pub(super) output: Result<CollectedOutput, OutputRefusal>,
    /// The scratch listing of the plan's test-only step, or the kind of the error that ended it;
    /// `None` when the plan did not ask for it or no keeper was held. Not printed. Test-only.
    #[cfg(test)]
    pub(super) scratch: Option<Result<Vec<String>, io::ErrorKind>>,
}

impl fmt::Display for AttemptEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "attempt_group={}", self.group.display())?;
        for (file, value) in &self.limits {
            writeln!(formatter, "attempt_limit.{file}={value}")?;
        }
        match (self.exit.code(), self.exit.signal()) {
            (Some(code), _) => writeln!(formatter, "exit=code {code}")?,
            (None, Some(signal)) => writeln!(formatter, "exit=signal {signal}")?,
            (None, None) => writeln!(formatter, "exit=unknown")?,
        }
        writeln!(formatter, "wall_time_ms={}", self.wall_time.as_millis())?;
        writeln!(formatter, "stdout_bytes={}", self.stdout.len())?;
        writeln!(formatter, "stderr_bytes={}", self.stderr.len())?;
        match self.peak_memory_bytes {
            Some(bytes) => writeln!(formatter, "attempt_peak_memory_bytes={bytes}")?,
            None => writeln!(formatter, "attempt_peak_memory_bytes=unavailable")?,
        }
        write!(formatter, "{}", self.usage)?;
        let observed = &self.readiness;
        let status = observed.status;
        let limits = observed.limits;
        writeln!(formatter, "readiness.members={}", observed.members)?;
        writeln!(
            formatter,
            "readiness.bootstrap_pid={}",
            observed.bootstrap_pid
        )?;
        writeln!(formatter, "readiness.no_new_privs={}", status.no_new_privs)?;
        writeln!(formatter, "readiness.seccomp={}", status.seccomp)?;
        writeln!(
            formatter,
            "readiness.seccomp_filters={}",
            status.seccomp_filters
        )?;
        writeln!(formatter, "readiness.cap_inh={:016x}", status.cap_inh)?;
        writeln!(formatter, "readiness.cap_prm={:016x}", status.cap_prm)?;
        writeln!(formatter, "readiness.cap_eff={:016x}", status.cap_eff)?;
        writeln!(formatter, "readiness.cap_bnd={:016x}", status.cap_bnd)?;
        writeln!(formatter, "readiness.cap_amb={:016x}", status.cap_amb)?;
        writeln!(
            formatter,
            "readiness.open_files={} {}",
            limit_word(limits.open_files.soft),
            limit_word(limits.open_files.hard)
        )?;
        writeln!(
            formatter,
            "readiness.core_bytes={} {}",
            limit_word(limits.core_bytes.soft),
            limit_word(limits.core_bytes.hard)
        )?;
        match observed.scratch.size_kib {
            Some(kib) => writeln!(formatter, "readiness.scratch_size={kib}k")?,
            None => writeln!(formatter, "readiness.scratch_size=unavailable")?,
        }
        match observed.scratch.inodes {
            Some(inodes) => writeln!(formatter, "readiness.scratch_inodes={inodes}")?,
            None => writeln!(formatter, "readiness.scratch_inodes=unavailable")?,
        }
        writeln!(formatter, "filter_sha256={}", self.filter_sha256)?;
        write!(formatter, "{}", self.report)
    }
}

/// A limit value as the evidence prints it: its number, or `unlimited`.
fn limit_word(value: Option<u64>) -> String {
    value.map_or_else(|| "unlimited".to_owned(), |number| number.to_string())
}

/// A reader thread's result: the bytes it kept, or the read error that ended it.
type Capture = JoinHandle<io::Result<Vec<u8>>>;

/// What the group's `cgroup.procs` listing held at its last read after the stub's normal exit,
/// within `TEARDOWN_GRACE` and the wall deadline.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SurvivorListing {
    /// No exit was observed before a deadline or a failure, so no listing was read.
    #[default]
    NotChecked,
    /// The listing holds no pid: the group is empty.
    Empty,
    /// The listing holds at least one pid.
    Occupied,
    /// The listing could not be read, was over its bound, was not UTF-8, or had a line that is
    /// not a pid.
    Unreadable,
}

/// Classifies a `cgroup.procs` listing read after the stub's exit. Every non-empty line must be a
/// pid, so a listing that is not understood is never taken for an empty group.
fn survivor_listing(listing: io::Result<Vec<u8>>) -> SurvivorListing {
    let Ok(bytes) = listing else {
        return SurvivorListing::Unreadable;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return SurvivorListing::Unreadable;
    };
    let mut occupied = false;
    for line in text.lines().filter(|line| !line.is_empty()) {
        if line.parse::<u32>().is_err() {
            return SurvivorListing::Unreadable;
        }
        occupied = true;
    }
    if occupied {
        SurvivorListing::Occupied
    } else {
        SurvivorListing::Empty
    }
}

/// Reads the group's listing until it is empty, unreadable, or still occupied once `grace` has
/// passed since the first read. The listing is read at least once, and once more after the last
/// pause, so a group that empties within `grace` is never reported as occupied. A listing that
/// was occupied and then cannot be read stays occupied: the processes were seen.
fn await_empty_listing(
    mut read: impl FnMut() -> io::Result<Vec<u8>>,
    grace: Duration,
) -> SurvivorListing {
    let started = Instant::now();
    let mut seen_occupied = false;
    loop {
        match survivor_listing(read()) {
            SurvivorListing::Occupied if started.elapsed() < grace => seen_occupied = true,
            SurvivorListing::Unreadable if seen_occupied => return SurvivorListing::Occupied,
            listing => return listing,
        }
        thread::sleep(POLL);
    }
}

/// The routing check of one attempt: the exact bytes that its admission read and the routing file
/// to read again (0056, amendment of 2026-11-13, crashes). The default holds no bytes and no path,
/// so its check fails closed.
#[derive(Debug, Default)]
struct RoutingWatch {
    /// The bytes that admission read from `kernel.core_pattern`.
    admitted: Vec<u8>,
    /// The routing file, `kernel.core_pattern` of the admission's roots.
    core_pattern: PathBuf,
}

impl RoutingWatch {
    /// The watch of one attempt: the bytes in `report` and the routing file of `roots`.
    fn of(report: &HostReport, roots: &AdmissionRoots<'_>) -> Self {
        Self {
            admitted: report.core_routing_bytes.clone(),
            core_pattern: roots.core_pattern.to_path_buf(),
        }
    }

    /// Reads the routing again, fresh, and compares it with the admitted bytes.
    fn check(&self) -> Result<(), AdmissionRefusal> {
        super::routing_unchanged(&self.admitted, fs::File::open(&self.core_pattern))
    }
}

/// What one launch holds while its group exists. `settle` ends every part of it.
#[derive(Debug, Default)]
struct Session {
    /// The stub, once it has been spawned.
    child: Option<Child>,
    /// The stub's exit status, once the poll observed it.
    exit: Option<ExitStatus>,
    /// The group's last listing read after the stub's exit, within `TEARDOWN_GRACE`, before the
    /// group is killed.
    survivors: SurvivorListing,
    /// The stdout reader, once it has been started.
    stdout: Option<Capture>,
    /// The stderr reader, once it has been started.
    stderr: Option<Capture>,
    /// The bytes that both readers have taken in so far, kept or discarded.
    counted: Arc<AtomicU64>,
    /// Set once the readers have taken in more than `OUTPUT_BUDGET` bytes.
    exceeded: Arc<AtomicBool>,
    /// The keeper, once `release` opened it. Every path but a collected one drops it unread.
    keeper: Option<Keeper>,
    /// The routing that every poll and the last check compare against.
    routing: RoutingWatch,
    /// The guardian, once it reported ready. Every path ends it in `conclude`.
    guardian: Option<GuardianLink>,
    /// The execution directory whose quarantine file every quarantine of this attempt writes.
    execution_dir: PathBuf,
    /// Whether this attempt quarantined the provider, so that its record stays.
    quarantined: bool,
    /// Whether a quarantine of this attempt did not write its quarantine file, so the attempt is
    /// refused as `QuarantineNotPersisted`.
    persist_failed: bool,
    /// The supervisor's own pid namespace, read at admission. The readiness check refuses a
    /// bootstrap that shares it (decision 0042 rule 6).
    own_pid_namespace: PathBuf,
}

impl Session {
    /// Quarantines the provider for `reason` (see `quarantine`) and notes it in this attempt.
    fn enter_quarantine(&mut self, reason: QuarantineReason) {
        let persisted = quarantine(&self.execution_dir, reason);
        self.note_quarantine(persisted);
    }

    /// Notes a quarantine of this attempt, and whether its quarantine file was written.
    fn note_quarantine(&mut self, persisted: bool) {
        self.quarantined = true;
        if !persisted {
            self.persist_failed = true;
        }
    }
}

/// What the group phase measured before the group was ended.
#[derive(Debug)]
struct Passed {
    /// The limits read back from the attempt group.
    limits: Vec<(&'static str, String)>,
    /// What the provider saw in the group before the release.
    readiness: GroupObservation,
    /// How the stub exited.
    exit: ExitStatus,
    /// The time from the LAUNCH record to the observed exit.
    wall_time: Duration,
}

/// What the cleanup of the group yielded.
#[derive(Debug)]
struct Settled {
    /// Whether the group was seen empty, the stub reaped, both readers finished and the group
    /// removed. Only then is the cleanup observed; otherwise the attempt is quarantined.
    cleanup_observed: bool,
    /// The group's counters, read after it was seen empty and before it was removed.
    usage: Option<GroupUsage>,
    /// The captured standard output, or the read error that ended it. `None` when the reader
    /// did not finish in time.
    stdout: Option<io::Result<Vec<u8>>>,
    /// The captured standard error, with the same meaning as `stdout`.
    stderr: Option<io::Result<Vec<u8>>>,
}

/// The capacity that `hold` took for one attempt (decision 0057 rule 19): the admission lock and
/// what admission and reconciliation found. The lock stays held while the value lives, and a
/// launch on it runs one attempt without releasing the lock itself.
#[derive(Debug)]
#[must_use]
pub(super) struct Held {
    /// The admission lock. It is never read: it holds the lock until the value is dropped (0042
    /// rule 6).
    _lock: AdmissionLock,
    /// The host admission's evidence.
    pub(super) report: HostReport,
    /// The delegated group in which each attempt's group is created.
    delegated: PathBuf,
    /// The supervisor's own pid namespace, read at admission.
    own_pid_namespace: PathBuf,
    /// The digest of every profile's filter that could be built when the capacity was taken. A
    /// launch reads its own profile's digest here and is refused when it is missing, so a filter
    /// that cannot be built refuses only its own profiles.
    filters: Vec<(FilterSet, String)>,
    /// The host identity that each attempt's record carries.
    pub(super) host: HostIdentity,
    /// The records that reconciliation settled and removed in this execution directory. Only the
    /// worker reads them.
    #[cfg(feature = "enforcement-fixture")]
    pub(super) reconciled: Vec<Reconciled>,
}

/// Takes the capacity of one attempt: the admission lock, the quarantine check and the whole host
/// admission, the filter digest and the pid namespace, then the reconciliation of the registry
/// (decision 0057 rule 19; 0056 rule 5, as reworded by the amendments of 2026-11-13 and
/// 2026-11-14). It is the first half of the launch that `launch_with` ran as one: no record, input
/// or group exists yet. The caller keeps the value through its launch and its settlement, so the
/// lock is released only after the attempt is settled (decision 0057 rule 10).
///
/// # Errors
///
/// Returns the refusal of the first step that failed. A refusal carries no group counters.
pub(super) fn hold(execution_dir: &Path) -> Result<Held, AttemptRefusal> {
    let roots = &admission::production_roots();
    let lock = AdmissionLock::acquire(execution_dir)
        .map_err(|refusal| without_usage(lock_failure(refusal)))?;
    let (report, delegated) = admit_checked(roots, execution_dir).map_err(without_usage)?;
    let (filters, own_pid_namespace) = policy_and_namespace(roots)?;
    let (host, reconciled) = reconcile_registry(roots, execution_dir).map_err(without_usage)?;
    // Only the worker reads what reconciliation removed.
    #[cfg(not(feature = "enforcement-fixture"))]
    drop(reconciled);
    Ok(Held {
        _lock: lock,
        report,
        delegated,
        own_pid_namespace,
        filters,
        host,
        #[cfg(feature = "enforcement-fixture")]
        reconciled,
    })
}

/// The end of one launch on held capacity. `record_removed` is `true` when the attempt's group was
/// seen gone and its record removed, and also when no record was written, so that the caller may
/// settle the attempt (decision 0057 rules 10 and 19).
#[derive(Debug)]
#[must_use]
pub(super) struct Launched {
    /// The attempt's evidence, or why the launch produced none.
    pub(super) result: Result<AttemptEvidence, AttemptRefusal>,
    /// Whether the attempt's record is gone, or no record was written. Only the worker reads it.
    #[cfg(feature = "enforcement-fixture")]
    pub(super) record_removed: bool,
}

/// Runs one attempt with the fixture input (see `launch_with`). Test-only.
#[cfg(test)]
pub(super) fn launch(
    plan: &AttemptPlan,
    execution_dir: &Path,
) -> Result<AttemptEvidence, AttemptRefusal> {
    let input = fixture_input().map_err(|failure| without_usage(AttemptFailure::Input(failure)))?;
    launch_with(plan, input, execution_dir)
}

/// The input of `launch`: one `index.html` with `FIXTURE_PAGE`, verified against its own listing.
#[cfg(test)]
fn fixture_input() -> Result<VerifiedInput, InputRefusal> {
    let package = FilePackage::new(vec![(PackagePath::entry(), FIXTURE_PAGE.to_vec())])
        .map_err(|_| InputRefusal::NotStaged)?;
    let listing = InputListing::of(&package);
    VerifiedInput::verify(package, listing)
}

/// Runs one attempt on the capacity that `hold` took, under the identity that the claim recorded
/// (decision 0057 rule 6). The record is written first and the `registered` pause follows; then
/// `run_registered` stages the input, makes the group and ends it. `renew` answers the guardian's
/// renewals, which are asked only at the store's checkpoint (decision 0057 rule 7). `run` binds the
/// output manifest to the claimed run (rule 17) and names the claim in the attempt's `v2` record
/// (0042 rule 6); without it, as in the provider's own tests, the record is `v1`. `held` is only
/// borrowed, so the caller keeps the lock until it has settled the attempt (rule 10).
pub(super) fn launch_held(
    plan: &AttemptPlan,
    renew: &mut dyn FnMut(u32) -> Heartbeat,
    held: &Held,
    identity: Identity,
    input: VerifiedInput,
    run: Option<(BoundRun, registry::RecordRun)>,
    execution_dir: &Path,
) -> Launched {
    let (bound, record_run) = match run {
        Some((bound, record)) => (Some(bound), Some(record)),
        None => (None, None),
    };
    let roots = &admission::production_roots();
    let registry = match register(
        roots,
        &held.delegated,
        execution_dir,
        held.host.clone(),
        identity,
        record_run,
    ) {
        Ok(registry) => registry,
        Err(failure) => {
            // A write can fail after its rename made the record durable, so the record counts as
            // gone only when the registry shows none; otherwise the attempt stays unsettled for
            // the next reconciliation (decision 0057 rules 10 and 19).
            #[cfg(feature = "enforcement-fixture")]
            let record_removed = Registry::open(execution_dir)
                .is_ok_and(|registry| registry.record_absent(identity));
            return Launched {
                result: Err(without_usage(failure)),
                #[cfg(feature = "enforcement-fixture")]
                record_removed,
            };
        }
    };
    pause_at(plan, Phase::Registered);
    let mut registered = Registered {
        registry,
        identity,
        removed: false,
    };
    let result = run_registered(
        plan,
        renew,
        held,
        input,
        bound,
        execution_dir,
        &mut registered,
    );
    Launched {
        result,
        #[cfg(feature = "enforcement-fixture")]
        record_removed: registered.removed,
    }
}

/// Removes the record of an attempt that ended before its group existed, so that its job can be
/// settled at once instead of waiting for its lease (decision 0057 rules 10 and 19). Only when no
/// group of the record's name is under `delegated` and the staged input is gone; otherwise the
/// record stays, and the next reconciliation removes it as it would after a crash (0056, amendment
/// of 2026-11-14).
fn release_ungrouped(registered: &mut Registered, delegated: &Path, execution_dir: &Path) {
    let group = delegated.join(registered.identity.group_name());
    let no_group = matches!(
        fs::symlink_metadata(&group),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    );
    if no_group
        && staging::remove_tree(&execution_dir.join(staging::INPUT_DIR)).is_ok()
        && registered.registry.remove(registered.identity).is_ok()
    {
        registered.removed = true;
    }
}

/// The attempt's record once `launch_held` wrote it: what `run_registered` needs to remove it.
#[derive(Debug)]
struct Registered {
    /// The registry of the execution directory.
    registry: Registry,
    /// The identity of the record, which names the attempt's group.
    identity: Identity,
    /// Whether the record was removed, so that the caller may settle the attempt.
    removed: bool,
}

/// The stub's arguments before the staged input's path: `stub` and the plan's profile identity,
/// unless a test replaces them.
fn stub_arguments(plan: &AttemptPlan) -> Vec<&'static str> {
    #[cfg(test)]
    if let Some(arguments) = plan.stub_override {
        return arguments.to_vec();
    }
    vec!["stub", plan.profile.inspection.identity()]
}

/// The digest of the plan's filter, read from what `hold` built (decision 0058 rule 3). A filter
/// that `hold` could not build releases the registered attempt, which has no group yet, and
/// refuses it.
fn filter_digest(
    plan: &AttemptPlan,
    held: &Held,
    execution_dir: &Path,
    registered: &mut Registered,
) -> Result<String, AttemptRefusal> {
    let built = held
        .filters
        .iter()
        .find(|(set, _)| *set == plan.profile.filter)
        .map(|(_, digest)| digest.clone());
    built.ok_or_else(|| {
        release_ungrouped(registered, &held.delegated, execution_dir);
        without_usage(AttemptFailure::PolicyUnavailable)
    })
}

/// Runs one attempt after its record is written (0056 rule 5, as reworded by the amendment of
/// 2026-11-14): the staging of the input, the attempt group named by the record's identity, its
/// phase, the cleanup and the final verdict. The group is always ended and removed, or the attempt
/// is quarantined (decision 0042 rule 6); `conclude` ends the guardian with it. A refusal carries
/// the group's counters whenever the group was created, emptied and read. `registered.removed` is
/// set before the verdict, so the record's state is known whatever the outcome.
///
/// `execution_dir` holds the registry and the staging directory of `input`. The group is created
/// beside the admission's `supervisor` child, not in `execution_dir`.
fn run_registered(
    plan: &AttemptPlan,
    renew: &mut dyn FnMut(u32) -> Heartbeat,
    held: &Held,
    input: VerifiedInput,
    run: Option<BoundRun>,
    execution_dir: &Path,
    registered: &mut Registered,
) -> Result<AttemptEvidence, AttemptRefusal> {
    // The stub and the bootstrap use the fixed tool root, so the roots that are verified here
    // must be the same ones: no caller chooses them.
    let roots = &admission::production_roots();
    let filter = filter_digest(plan, held, execution_dir, registered)?;
    // Staged under the admission lock, after the record and before any group exists (0056,
    // amendment of 2026-11-13).
    let staged = match input.stage(execution_dir) {
        Ok(staged) => staged,
        Err(failure) => {
            release_ungrouped(registered, &held.delegated, execution_dir);
            return Err(without_usage(AttemptFailure::Input(failure)));
        }
    };
    let input_digest = staged.digest();
    let name = registered.identity.group_name();
    let group = match probe_group::create_probe(&held.delegated, &name) {
        Ok(group) => group,
        Err(failure) => {
            drop(staged);
            release_ungrouped(registered, &held.delegated, execution_dir);
            return Err(without_usage(AttemptFailure::Refused(failure)));
        }
    };
    let group_path = held.delegated.join(&name);
    pause_at(plan, Phase::Created);

    // From here on every path ends the group in `settle`, whatever the group phase returned.
    let mut session = Session {
        execution_dir: execution_dir.to_path_buf(),
        routing: RoutingWatch::of(&held.report, roots),
        own_pid_namespace: held.own_pid_namespace.clone(),
        ..Session::default()
    };
    let phase = run_group(
        plan,
        renew,
        roots,
        staged.path(),
        &group,
        &group_path,
        &mut session,
    );
    let (settled, guardian_reason) = conclude(group, &group_path, &mut session);
    // Removed after the group is gone, on every path that staged it.
    let removed = staged.remove();
    // The last routing check runs once the group is empty and before the record is removed, so an
    // attempt that quarantines keeps its record (0056 rule 5, as reworded by the amendment of
    // 2026-11-13, crashes; amendment of 2026-11-14).
    let routing = if settled.cleanup_observed {
        routing_after(&phase, &mut session)
    } else {
        Ok(())
    };
    // Removed only when the group was observed gone and the input removed, and no quarantine took
    // place (0056, amendment of 2026-11-14).
    let released = settled.cleanup_observed && removed.is_ok();
    let removable = released && !session.quarantined;
    let record_not_removed = release_record(
        &registered.registry,
        registered.identity,
        &mut session,
        released,
    );
    registered.removed = removable && !record_not_removed;
    let keeper = session.keeper.take();
    let usage = settled.usage;
    let refused = |failure: AttemptFailure| AttemptRefusal { failure, usage };
    #[cfg(test)]
    let scratch = scratch_listing(plan, keeper.as_ref());
    // A quarantine whose file was not written ranks first, because a restart would not remember it
    // (0056, amendment of 2026-11-14).
    if session.persist_failed {
        return Err(refused(AttemptFailure::QuarantineNotPersisted));
    }
    // A refusal of the routing ends the attempt here, and so does a reason that the guardian
    // reported, even when the stub's exit was seen (0056, amendment of 2026-11-13, crashes and the
    // guardian). A group that was not observed gone is not empty: `conclude` has quarantined the
    // provider already, and the verdict names `CleanupNotObserved` first, after
    // `QuarantineNotPersisted`, whatever a poll saw or the guardian reported.
    if settled.cleanup_observed {
        routing.map_err(refused)?;
        if let Some(reason) = guardian_reason {
            return Err(refused(AttemptFailure::Guardian(reason)));
        }
    }
    let outcome = outcome_of(&settled, &session, &phase);
    final_verdict(&outcome).map_err(refused)?;
    let (Ok(passed), Some(Ok(stdout)), Some(Ok(stderr)), Some(counters)) =
        (phase, settled.stdout, settled.stderr, usage)
    else {
        return Err(refused(AttemptFailure::Unobserved));
    };
    pause_at(plan, Phase::Collecting);
    removed.map_err(|failure| refused(AttemptFailure::Input(failure)))?;
    if record_not_removed {
        return Err(refused(AttemptFailure::Quarantined));
    }
    let binding = manifest_binding(plan.profile, &held.report, &filter, input_digest, run);
    let output = collect_output(keeper, passed.exit, &binding);
    Ok(AttemptEvidence {
        group: group_path,
        limits: passed.limits,
        exit: passed.exit,
        stdout,
        stderr,
        wall_time: passed.wall_time,
        peak_memory_bytes: counters.memory_peak_bytes,
        usage: counters,
        readiness: passed.readiness,
        filter_sha256: filter,
        report: held.report.clone(),
        output,
        #[cfg(test)]
        scratch,
    })
}

/// Runs one attempt with the given input under a held capacity: `hold`, then `launch_held` with a
/// fresh identity and the plan's stand-in store as the renewals (decision 0057 rule 7). Test-only.
#[cfg(test)]
pub(super) fn launch_with(
    plan: &AttemptPlan,
    input: VerifiedInput,
    execution_dir: &Path,
) -> Result<AttemptEvidence, AttemptRefusal> {
    let held = hold(execution_dir)?;
    let identity = Identity::random().map_err(|_| {
        without_usage(AttemptFailure::NotStarted(
            NotStartedReason::RegistryNotWritten,
        ))
    })?;
    let mut renew = |heartbeat: u32| (plan.store)(heartbeat);
    launch_held(
        plan,
        &mut renew,
        &held,
        identity,
        input,
        None,
        execution_dir,
    )
    .result
}

/// The quarantine check and the admission under the held lock (0056 rule 5, as reworded by the
/// amendment of 2026-11-14). A process quarantined in memory, or an execution directory whose
/// quarantine file exists or cannot be looked up, is refused as `Quarantined` before admission.
/// The check itself quarantines nothing.
fn admit_checked(
    roots: &AdmissionRoots<'_>,
    execution_dir: &Path,
) -> Result<(HostReport, PathBuf), AttemptFailure> {
    if QUARANTINED.load(Ordering::SeqCst) || registry::quarantine_present(execution_dir) {
        return Err(AttemptFailure::Quarantined);
    }
    admission::admit_for_attempt(InspectionProfile::fixture(), roots)
        .map_err(AttemptFailure::Refused)
}

/// Builds every profile's filter and reads the supervisor's pid namespace after admission. The
/// capacity is taken before the claim (0042 rule 5), so no profile is known yet: the digests of
/// the filters that build are kept, and the launch reads its own profile's. When no filter builds,
/// or the namespace cannot be read, the attempt is refused before the registry is touched.
fn policy_and_namespace(
    roots: &AdmissionRoots<'_>,
) -> Result<(Vec<(FilterSet, String)>, PathBuf), AttemptRefusal> {
    let filters: Vec<(FilterSet, String)> = profile::filter_sets()
        .filter_map(|set| policy::policy_sha256(set).ok().map(|digest| (set, digest)))
        .collect();
    if filters.is_empty() {
        return Err(without_usage(AttemptFailure::PolicyUnavailable));
    }
    let own_pid_namespace =
        fs::read_link(roots.proc_self.join("ns").join("pid")).map_err(|_| {
            without_usage(AttemptFailure::Refused(
                AdmissionRefusal::NamespacesUnavailable,
            ))
        })?;
    Ok((filters, own_pid_namespace))
}

/// Reconciles the registry of `execution_dir` after admission (0056, amendment of 2026-11-14) and
/// returns the host's identity with what reconciliation did. A host identity that cannot be read
/// refuses as `NotStarted(RegistryNotWritten)` and quarantines nothing. A refusal of
/// reconciliation quarantines the provider with the reason `registry` and refuses as
/// `Quarantined`, or as `QuarantineNotPersisted` when the quarantine file was not written.
fn reconcile_registry(
    roots: &AdmissionRoots<'_>,
    execution_dir: &Path,
) -> Result<(HostIdentity, Vec<Reconciled>), AttemptFailure> {
    let own_uid = cgroup::own_uid(roots.proc_self).map_err(AttemptFailure::Refused)?;
    let host = HostIdentity::read(
        roots.proc_self,
        Path::new(registry::MACHINE_ID),
        Path::new(registry::BOOT_ID),
    )
    .map_err(|_| AttemptFailure::NotStarted(NotStartedReason::RegistryNotWritten))?;
    match registry::reconcile(execution_dir, roots.cgroup_mount, own_uid, &host) {
        Ok(reconciled) => Ok((host, reconciled)),
        Err(_) => Err(if quarantine(execution_dir, QuarantineReason::Registry) {
            AttemptFailure::Quarantined
        } else {
            AttemptFailure::QuarantineNotPersisted
        }),
    }
}

/// Writes the record of a new attempt before the input is staged and the group made (0056,
/// amendment of 2026-11-14): the attempt's identity as its claim recorded it, the host's identity,
/// the delegated group relative to the cgroup mount and the group name of the identity and, for a
/// worker's claim, the database, job and fence (a `v2` record). Any failure refuses as
/// `NotStarted(RegistryNotWritten)` before any resource exists.
fn register(
    roots: &AdmissionRoots<'_>,
    delegated: &Path,
    execution_dir: &Path,
    host: HostIdentity,
    identity: Identity,
    run: Option<registry::RecordRun>,
) -> Result<Registry, AttemptFailure> {
    let not_written = AttemptFailure::NotStarted(NotStartedReason::RegistryNotWritten);
    let parent = delegated
        .strip_prefix(roots.cgroup_mount)
        .ok()
        .and_then(Path::to_str)
        .ok_or(not_written)?
        .to_owned();
    let record = registry::Record {
        identity,
        machine: host.machine,
        boot: host.boot,
        cgroupns: host.cgroupns,
        parent,
        group: identity.group_name(),
        run,
    };
    let registry = Registry::open(execution_dir).map_err(|_| not_written)?;
    registry.write(&record).map_err(|_| not_written)?;
    Ok(registry)
}

/// Removes the attempt's record once the attempt is over (0056, amendment of 2026-11-14): only
/// when the attempt did not quarantine and `released` says that its group was observed gone and
/// its staged input removed. A removal that fails quarantines the provider with the reason
/// `registry` and returns `true`; the caller then refuses as `Quarantined` unless an earlier
/// refusal applies.
#[must_use]
fn release_record(
    registry: &Registry,
    identity: Identity,
    session: &mut Session,
    released: bool,
) -> bool {
    if session.quarantined || !released || registry.remove(identity).is_ok() {
        return false;
    }
    session.enter_quarantine(QuarantineReason::Registry);
    true
}

/// The parts of one attempt as plain values for `final_verdict`: the cleanup, the output budget,
/// the group phase, the counters, the survivor listing and the captures.
fn outcome_of(
    settled: &Settled,
    session: &Session,
    phase: &Result<Passed, AttemptFailure>,
) -> Outcome {
    Outcome {
        cleanup_observed: settled.cleanup_observed,
        output_exceeded: session.exceeded.load(Ordering::SeqCst),
        phase: phase.as_ref().map(|_| ()).map_err(|failure| *failure),
        usage: settled.usage,
        survivors: session.survivors,
        captures_read: matches!(
            (&settled.stdout, &settled.stderr),
            (Some(Ok(_)), Some(Ok(_)))
        ),
    }
}

/// Test-only (0056, amendment of 2026-11-14, reconciliation alone): takes the lock, checks the
/// quarantine, admits and reconciles, and returns what reconciliation did for each record. It
/// stages no input, makes no group and writes no record.
#[cfg(test)]
pub(super) fn reconcile_only(execution_dir: &Path) -> Result<Vec<Reconciled>, AttemptRefusal> {
    let roots = &admission::production_roots();
    let _lock = AdmissionLock::acquire(execution_dir)
        .map_err(|refusal| without_usage(lock_failure(refusal)))?;
    admit_checked(roots, execution_dir).map_err(without_usage)?;
    reconcile_registry(roots, execution_dir)
        .map(|(_, reconciled)| reconciled)
        .map_err(without_usage)
}

/// The binding of the output manifest: the profile identity, the SHA-256 of the profile's
/// manifest, the tool root's manifest digest, the filter's digest and the input listing's digest
/// (0056, amendment of 2026-11-13), and the values of the claimed run when there is one (decision
/// 0057 rule 17).
fn manifest_binding(
    profile: &ExecutionProfile,
    report: &HostReport,
    filter_sha256: &str,
    input_digest: [u8; 32],
    run: Option<BoundRun>,
) -> ManifestBinding {
    ManifestBinding {
        profile: profile.inspection.identity(),
        profile_manifest_sha256: Sha256::digest(profile.inspection.manifest()).into(),
        tool_root_sha256: report.tool_manifest_sha256.clone(),
        filter_sha256: filter_sha256.to_owned(),
        input_digest,
        run,
    }
}

/// Collects the output root after a passing verdict, when the stub exited with status 0 (0042
/// rule 7; 0056 rule 5, as reworded). Any other exit drops the keeper unread.
fn collect_output(
    keeper: Option<Keeper>,
    exit: ExitStatus,
    binding: &ManifestBinding,
) -> Result<CollectedOutput, OutputRefusal> {
    let Some(keeper) = keeper.filter(|_| exit.code() == Some(0)) else {
        return Err(OutputRefusal::NotCollected);
    };
    keeper.collect(binding).map_err(OutputRefusal::Refused)
}

/// The last routing check, once the group is empty (0056 rule 5, as reworded by the amendment of
/// 2026-11-13, crashes): a change or a failed read ends the group, quarantines the provider in its
/// process and refuses the attempt. A routing refusal that a poll saw comes first; the check still
/// runs once more. The quarantine is noted in `session`, and its file is written (amendment of
/// 2026-11-14).
fn routing_after(
    phase: &Result<Passed, AttemptFailure>,
    session: &mut Session,
) -> Result<(), AttemptFailure> {
    let last = session.routing.check();
    let refusal = match phase {
        Err(AttemptFailure::Routing(refusal)) => Some(*refusal),
        _ => last.err(),
    };
    if let Some(refusal) = refusal {
        session.enter_quarantine(QuarantineReason::Routing);
        return Err(AttemptFailure::Routing(refusal));
    }
    Ok(())
}

/// The plan's test-only scratch listing through the keeper, taken after `settle`, whatever the
/// verdict (0056, amendment of 2026-11-13, crashes). `None` when the plan does not ask for it or
/// no keeper is held. It reaches the test in the evidence; a refused attempt carries none, and the
/// crash tests require evidence.
#[cfg(test)]
fn scratch_listing(
    plan: &AttemptPlan,
    keeper: Option<&Keeper>,
) -> Option<Result<Vec<String>, io::ErrorKind>> {
    if !plan.list_scratch {
        return None;
    }
    keeper.map(|keeper| keeper.list_scratch().map_err(|error| error.kind()))
}

/// Maps the lock's refusal: contention is `Busy`, and nothing else is then done.
fn lock_failure(refusal: LockRefusal) -> AttemptFailure {
    match refusal {
        LockRefusal::Busy => AttemptFailure::Busy,
        LockRefusal::Unusable => AttemptFailure::LockUnusable,
    }
}

/// Quarantines the provider: sets the in-process flag, so that later launches in this process are
/// refused (decision 0042 rule 6), and writes the quarantine file of `execution_dir` with `reason`
/// (0056, amendment of 2026-11-14, the attempt registry). Returns whether the file was written or
/// already existed; `false` leaves the quarantine in this process only, and a restart would not
/// remember it.
pub(super) fn quarantine(execution_dir: &Path, reason: QuarantineReason) -> bool {
    QUARANTINED.store(true, Ordering::SeqCst);
    registry::persist_quarantine(execution_dir, reason).is_ok()
}

/// The group phase: limits, the stub, its placement, the readiness check and release, the wait
/// for exit, and the exec confirmation. `input` is the staged directory that the stub receives as
/// its last argument. The caller ends the group whatever this returns.
fn run_group(
    plan: &AttemptPlan,
    renew: &mut dyn FnMut(u32) -> Heartbeat,
    roots: &AdmissionRoots<'_>,
    input: &Path,
    group: &ProbeGroup,
    group_path: &Path,
    session: &mut Session,
) -> Result<Passed, AttemptFailure> {
    let limits = probe_group::apply_limits(group).map_err(AttemptFailure::Refused)?;
    // The guardian holds the group before the stub exists (0042 rule 6, amendment of 2026-11-13).
    start_guardian(plan, roots, group_path, session)?;
    pause_at(plan, Phase::Guarded);
    let mut control = start_stub(plan, roots, input, session)?;
    place_stub(group, group_path, session)?;
    pause_at(plan, Phase::Started);
    let (readiness, start) = release(plan, renew, roots, &mut control, group_path, session)?;
    // `await_exit` records the exit in the session itself, on every path that reaped the stub.
    let awaited = await_exit(plan, renew, session, start);
    // The stub is no longer polled and no renewal follows, so the guardian's window stops here,
    // not after the teardown grace below (0056, amendment of 2026-11-13, the guardian: `S`).
    send_to_guardian(session.guardian.as_mut(), guardian::STOP);
    let (exit, wall_time) = awaited?;
    // Decision 0042 rule 6: after the launcher and init teardown a normal exit also requires an
    // empty group. `populated` is not used: measured on 2026-11-12, it stays 1 for a few
    // milliseconds after ordinary exits while the listing is already empty. `settle` kills
    // survivors as it kills every group.
    // The wait ends at the wall deadline too, so a group never runs past it (0042 rule 5).
    let listing = group_path.join("cgroup.procs");
    let grace = TEARDOWN_GRACE.min(plan.wall_deadline.saturating_sub(start.elapsed()));
    session.survivors = await_empty_listing(|| read_bounded(&listing, LISTING_BYTES), grace);
    confirm_started(&mut control)?;
    Ok(Passed {
        limits,
        readiness,
        exit,
        wall_time,
    })
}

/// Starts the stub with its standard input on a control socket and its two pipes captured. The
/// staged input's path is the stub's last argument (0056, amendment of 2026-11-13). Returns the
/// provider's end of the socket.
fn start_stub(
    plan: &AttemptPlan,
    roots: &AdmissionRoots<'_>,
    input: &Path,
    session: &mut Session,
) -> Result<UnixStream, AttemptFailure> {
    let (control, child_end) = UnixStream::pair()
        .map_err(|_| AttemptFailure::NotStarted(NotStartedReason::StubNotStarted))?;
    // The command holds the child's end of the socket, so it is dropped right after the spawn.
    let spawned = {
        let mut command = Command::new(roots.tool_root.join(plan.program.file()));
        command
            .args(plan.program.prefix())
            .args(stub_arguments(plan))
            .arg(input)
            .env_clear()
            .stdin(Stdio::from(OwnedFd::from(child_end)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.spawn()
    };
    let mut child =
        spawned.map_err(|_| AttemptFailure::NotStarted(NotStartedReason::StubNotStarted))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // Stored before the readers start, so that a failed reader still lets `settle` end the stub.
    session.child = Some(child);
    let stdout_reader = start_reader(stdout, session)?;
    session.stdout = Some(stdout_reader);
    let stderr_reader = start_reader(stderr, session)?;
    session.stderr = Some(stderr_reader);
    Ok(control)
}

/// Starts one reader thread for a pipe. Both readers share the output budget.
fn start_reader<R: Read + Send + 'static>(
    pipe: Option<R>,
    session: &Session,
) -> Result<Capture, AttemptFailure> {
    let pipe = pipe.ok_or(AttemptFailure::NotStarted(NotStartedReason::StubNotStarted))?;
    let counted = Arc::clone(&session.counted);
    let exceeded = Arc::clone(&session.exceeded);
    thread::Builder::new()
        .spawn(move || capture(pipe, &counted, &exceeded))
        .map_err(|_| AttemptFailure::NotStarted(NotStartedReason::StubNotStarted))
}

/// Reads `pipe` to its end. Bytes that fit the shared budget are kept. The rest are read and
/// discarded, and `exceeded` is set (decision 0042 rule 5).
fn capture(mut pipe: impl Read, counted: &AtomicU64, exceeded: &AtomicBool) -> io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut chunk = [0u8; CHUNK_BYTES];
    loop {
        let read = match pipe.read(&mut chunk) {
            Ok(0) => return Ok(kept),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let received = u64::try_from(read).unwrap_or(u64::MAX);
        let before = counted.fetch_add(received, Ordering::SeqCst);
        let room = OUTPUT_BUDGET.saturating_sub(before);
        if received > room {
            exceeded.store(true, Ordering::SeqCst);
        }
        let keep = usize::try_from(room.min(received)).unwrap_or(0);
        if let Some(bytes) = chunk.get(..keep) {
            kept.extend_from_slice(bytes);
        }
    }
}

/// Writes the stub's pid to the group and confirms from `cgroup.procs` that the group holds it.
fn place_stub(
    group: &ProbeGroup,
    group_path: &Path,
    session: &Session,
) -> Result<(), AttemptFailure> {
    let not_placed = AttemptFailure::NotStarted(NotStartedReason::StubNotPlaced);
    let pid = session.child.as_ref().map(Child::id).ok_or(not_placed)?;
    probe_group::add_process(group, pid).map_err(|_| not_placed)?;
    let listing =
        read_bounded(&group_path.join("cgroup.procs"), LISTING_BYTES).map_err(|_| not_placed)?;
    if lists_pid(&listing, pid) {
        Ok(())
    } else {
        Err(not_placed)
    }
}

/// Whether a `cgroup.procs` listing holds `pid` on one of its lines.
fn lists_pid(listing: &[u8], pid: u32) -> bool {
    std::str::from_utf8(listing).is_ok_and(|text| {
        text.lines().any(|line| {
            let parsed: Option<u32> = line.parse().ok();
            parsed == Some(pid)
        })
    })
}

/// Sends LAUNCH, reads READY before the readiness deadline, observes the group independently and
/// only then sends RELEASE; when the plan says so, the store's checkpoint must confirm first. Between the observation and RELEASE the keeper is opened, checked and
/// held in `session`, and the plan's plant step runs (0056 rule 5, as reworded). After RELEASE the
/// guardian receives `L` (0056, amendment of 2026-11-13, the guardian). Returns the observation and
/// the instant of LAUNCH.
fn release(
    plan: &AttemptPlan,
    renew: &mut dyn FnMut(u32) -> Heartbeat,
    roots: &AdmissionRoots<'_>,
    control: &mut UnixStream,
    group_path: &Path,
    session: &mut Session,
) -> Result<(GroupObservation, Instant), AttemptFailure> {
    let start = Instant::now();
    control
        .write_all(&LAUNCH)
        .map_err(|_| AttemptFailure::NotStarted(NotStartedReason::StubNotStarted))?;
    read_ready(control, start, plan.readiness_deadline)?;
    pause_at(plan, Phase::Ready);
    // Only a group confirmed by the provider's own reads may be released (0042 rule 6).
    let observation = readiness::observe_group(
        group_path,
        Path::new("/proc"),
        roots.cgroup_mount,
        &session.own_pid_namespace,
    )
    .map_err(|_| AttemptFailure::NotStarted(NotStartedReason::ReadinessNotConfirmed))?;
    // Opened and checked from outside the group after the observation and before the release
    // (0056 rule 5, as reworded).
    let keeper = Keeper::open(Path::new("/proc"), group_path, &observation)
        .map_err(AttemptFailure::NotStarted)?;
    if let Some(plant) = plan.plant {
        plant(keeper.output_root())
            .map_err(|_| AttemptFailure::NotStarted(NotStartedReason::KeeperNotHeld))?;
    }
    // Held by the session, so that every later path drops it.
    session.keeper = Some(keeper);
    if plan.checkpoint_at_release {
        // A cancel, trash, purge or change of generation or authority since the claim ends the
        // attempt here, before any payload code runs (decision 0042 rule 3; 0057 rule 7).
        let confirmed =
            on_answer(renew(0), plan.renew_regardless).map_err(AttemptFailure::LeaseLost)?;
        if !confirmed {
            return Err(AttemptFailure::NotStarted(
                NotStartedReason::CheckpointUnavailable,
            ));
        }
    }
    control
        .write_all(plan.release_record)
        .map_err(|_| AttemptFailure::NotStarted(NotStartedReason::ReleaseNotSent))?;
    send_to_guardian(session.guardian.as_mut(), guardian::LEASE);
    Ok((observation, start))
}

/// Reads exactly the READY record before the deadline, which counts from `start`.
fn read_ready(
    control: &mut UnixStream,
    start: Instant,
    deadline: Duration,
) -> Result<(), AttemptFailure> {
    let refused = AttemptFailure::NotStarted(NotStartedReason::NoReadiness);
    let mut received = [0u8; READY.len()];
    let mut filled = 0;
    while filled < received.len() {
        let remaining = deadline
            .checked_sub(start.elapsed())
            .filter(|left| !left.is_zero())
            .ok_or(refused)?;
        control
            .set_read_timeout(Some(remaining))
            .map_err(|_| refused)?;
        let Some(slot) = received.get_mut(filled..) else {
            return Err(refused);
        };
        match control.read(slot) {
            Ok(0) => return Err(refused),
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(refused),
        }
    }
    if received == READY {
        Ok(())
    } else {
        Err(refused)
    }
}

/// Polls the stub until it exits, the wall deadline passes, the output budget is exceeded or the
/// routing is no longer the admitted one. Returns the exit and the time from `start` to the
/// observed exit.
///
/// At each poll that sees the stub running, a heartbeat is asked when one is due, after the
/// deadline check (`heartbeat`). The plan's `provider_deadline` switch removes the wall-deadline
/// part of the check and nothing else, and the plan's pause at `Running` comes first.
fn await_exit(
    plan: &AttemptPlan,
    renew: &mut dyn FnMut(u32) -> Heartbeat,
    session: &mut Session,
    start: Instant,
) -> Result<(ExitStatus, Duration), AttemptFailure> {
    pause_at(plan, Phase::Running);
    let mut renewals = Renewals::default();
    let deadline = if plan.provider_deadline {
        plan.wall_deadline
    } else {
        Duration::MAX
    };
    let child = session.child.as_mut().ok_or(AttemptFailure::Unobserved)?;
    loop {
        let polled = child.try_wait();
        let elapsed = start.elapsed();
        if let Ok(Some(status)) = polled {
            // The stub is reaped now, so `settle` must know it whatever the verdict below: a kill
            // of a reaped child is not a failed cleanup.
            session.exit = Some(status);
        }
        // The routing is read again at each poll, also at the poll that sees the exit (0056 rule
        // 5, as reworded by the amendment of 2026-11-13, crashes). A change ends the group as at
        // the wall deadline.
        session.routing.check().map_err(AttemptFailure::Routing)?;
        match polled {
            Ok(Some(status)) => {
                // An exit that is first seen at or after the deadline is past it too: the
                // deadline is absolute, whatever the polling interval.
                running_verdict(elapsed, deadline, session.exceeded.load(Ordering::SeqCst))?;
                return Ok((status, elapsed));
            }
            Ok(None) => {}
            Err(_) => return Err(AttemptFailure::Unobserved),
        }
        running_verdict(elapsed, deadline, session.exceeded.load(Ordering::SeqCst))?;
        heartbeat(
            plan,
            renew,
            &mut renewals,
            session.guardian.as_mut(),
            Instant::now(),
        )?;
        thread::sleep(POLL);
    }
}

/// The decision for one poll of the stub, running or just seen exited: output past the budget first, then the wall
/// deadline.
fn running_verdict(
    elapsed: Duration,
    wall_deadline: Duration,
    output_exceeded: bool,
) -> Result<(), AttemptFailure> {
    if output_exceeded {
        return Err(AttemptFailure::OutputLimitExceeded);
    }
    if elapsed >= wall_deadline {
        return Err(AttemptFailure::DeadlineExceeded);
    }
    Ok(())
}

/// What the parts of one attempt measured, as plain values, for the one final decision.
#[derive(Clone, Copy, Debug)]
struct Outcome {
    /// Whether the group, the stub, both readers and the group's removal were all observed.
    cleanup_observed: bool,
    /// Whether the output budget was exceeded.
    output_exceeded: bool,
    /// The group phase's result: `Ok` for a run that exited, or the failure that ended the phase.
    phase: Result<(), AttemptFailure>,
    /// The group's counters, when they were read.
    usage: Option<GroupUsage>,
    /// The group's last listing read after the stub's exit, within `TEARDOWN_GRACE`.
    survivors: SurvivorListing,
    /// Whether both captured streams were read to their end.
    captures_read: bool,
}

/// The final verdict of one attempt. The first matching rule decides: cleanup not observed
/// (the group is quarantined), output budget exceeded, wall deadline exceeded, out of memory,
/// survivors, the not-started reasons of the control channel (`ExecFailed`, `ExecUnconfirmed`),
/// success.
///
/// An attempt that the group's OOM killed is `OutOfMemory`, although its stub exited by signal 9
/// and its control channel read end of stream: the counters decide, not the exit status. Counters
/// that cannot be read are `Unobserved` before the OOM rule, because that rule cannot be decided
/// without them. A survivor listing that cannot be read is `Unobserved` at the survivors rule, for
/// the same reason: an unread listing must never count as an empty group.
fn final_verdict(outcome: &Outcome) -> Result<(), AttemptFailure> {
    if !outcome.cleanup_observed {
        return Err(AttemptFailure::CleanupNotObserved);
    }
    if outcome.output_exceeded {
        return Err(AttemptFailure::OutputLimitExceeded);
    }
    if outcome.phase == Err(AttemptFailure::DeadlineExceeded) {
        return Err(AttemptFailure::DeadlineExceeded);
    }
    let Some(usage) = outcome.usage else {
        // Without the counters a run cannot be told from an out-of-memory kill. A launch that
        // failed before or at its start keeps its own reason, which needs no counter.
        outcome.phase?;
        return Err(AttemptFailure::Unobserved);
    };
    if usage.oom_kills > 0 {
        return Err(AttemptFailure::OutOfMemory);
    }
    match outcome.survivors {
        SurvivorListing::Occupied => return Err(AttemptFailure::Survivors),
        SurvivorListing::Unreadable => return Err(AttemptFailure::Unobserved),
        // A listing is read whenever the stub's exit was observed, so a run that ended well
        // without one was not observed.
        SurvivorListing::NotChecked if outcome.phase.is_ok() => {
            return Err(AttemptFailure::Unobserved);
        }
        SurvivorListing::NotChecked | SurvivorListing::Empty => {}
    }
    outcome.phase?;
    if !outcome.captures_read {
        return Err(AttemptFailure::Unobserved);
    }
    Ok(())
}

/// After the stub exited, `EXEC_FAILED` on the control socket means the exec of the payload
/// returned, so the attempt did not start (0056 rule 5). End of stream means only that no exec
/// failure was reported: a bootstrap that was killed between the release and the exec leaves
/// the same end of stream. Such an attempt ends by a signal or a non-zero status and is a
/// failed attempt for every caller; no descriptor of this chain closes at the exec alone,
/// because the launcher and its init keep their copies of the stub's standard input. Any other
/// answer is unconfirmed.
fn confirm_started(control: &mut UnixStream) -> Result<(), AttemptFailure> {
    let unconfirmed = AttemptFailure::NotStarted(NotStartedReason::ExecUnconfirmed);
    control
        .set_read_timeout(Some(CONTROL_GRACE))
        .map_err(|_| unconfirmed)?;
    let mut answer = [0u8; EXEC_FAILED.len()];
    match control.read(&mut answer) {
        Ok(0) => Ok(()),
        Ok(_) if answer == EXEC_FAILED => {
            Err(AttemptFailure::NotStarted(NotStartedReason::ExecFailed))
        }
        _ => Err(unconfirmed),
    }
}

/// Ends every part of the attempt after any outcome of the group phase (decision 0042 rule 6):
/// kills the group and waits until it is empty, reaps the stub, joins both readers and reads
/// the group's counters. The group is removed only when every part was observed, the removal
/// included. Otherwise the attempt is quarantined and the group stays in place. A counter read
/// that fails leaves `usage` empty; the verdict then refuses the attempt, and the group is
/// still removed.
fn settle(group: ProbeGroup, group_path: &Path, session: &mut Session) -> Settled {
    // One deadline for the whole cleanup (0042 rule 6: five seconds), not one per part.
    let deadline = Instant::now().checked_add(probe_group::CLEANUP_DEADLINE);
    let remaining = || {
        deadline.map_or(Duration::ZERO, |deadline| {
            deadline.saturating_duration_since(Instant::now())
        })
    };
    let emptied =
        probe_group::kill_and_await_empty(&group, probe_group::CLEANUP_DEADLINE, Instant::now)
            .is_ok();
    let reaped = reap_child(session.child.as_mut(), session.exit.is_some(), remaining());
    let stdout = join_reader(session.stdout.take(), remaining());
    let stderr = join_reader(session.stderr.take(), remaining());
    // Read after the group was seen empty and before it is removed.
    let usage = if emptied {
        usage::read_usage(group_path).ok()
    } else {
        None
    };
    let readers_finished = stdout.is_some() && stderr.is_some();
    // The removal comes last, so a removed group was observed in every earlier part too.
    let cleanup_observed =
        emptied && reaped && readers_finished && probe_group::remove_probe(group).is_ok();
    if !cleanup_observed {
        session.enter_quarantine(QuarantineReason::Cleanup);
    }
    Settled {
        cleanup_observed,
        usage,
        stdout,
        stderr,
    }
}

/// Kills the stub if it may still run, then waits for it for at most `within`, with at least
/// one look. Returns whether it was reaped. A kill error is ignored only when the exit is
/// already known.
fn reap_child(child: Option<&mut Child>, exited: bool, within: Duration) -> bool {
    let Some(child) = child else {
        return true;
    };
    let killed = child.kill();
    if killed.is_err() && !exited {
        return false;
    }
    wait_within(child, within).is_ok()
}

/// Waits, for at most `within`, for one reader to finish and returns what it captured. It
/// looks at least once. `None` means that the reader did not finish or that it panicked.
fn join_reader(reader: Option<Capture>, within: Duration) -> Option<io::Result<Vec<u8>>> {
    let Some(reader) = reader else {
        return Some(Ok(Vec::new()));
    };
    let started = Instant::now();
    while !reader.is_finished() {
        if started.elapsed() >= within {
            return None;
        }
        thread::sleep(POLL);
    }
    reader.join().ok()
}

/// Whether a heartbeat is due at `now`: at the first poll after the release, when no heartbeat has
/// been asked, and then at the first poll after half a window has passed since the last heartbeat
/// that was asked. An unavailable store is asked again on the same schedule (0056, amendment of
/// 2026-11-13, the guardian).
#[must_use]
fn heartbeat_due(last_asked: Option<Instant>, now: Instant, window: Duration) -> bool {
    last_asked.is_none_or(|last| now.saturating_duration_since(last) >= window / 2)
}

/// The provider's decision on one heartbeat's answer. `Ok(true)` sends `R`, `Ok(false)` sends
/// nothing (an unavailable store is asked again half a window later), and `Err` stops the renewals
/// with the part that failed. With `renew_regardless` every answer sends `R`, as the faulty
/// supervisor of 0042's acceptance does.
fn on_answer(answer: Heartbeat, renew_regardless: bool) -> Result<bool, LeaseLoss> {
    match answer {
        Heartbeat::Confirmed => Ok(true),
        _ if renew_regardless => Ok(true),
        Heartbeat::Unavailable => Ok(false),
        Heartbeat::Lost(part) => Err(part),
    }
}

/// The heartbeats that one launch has asked for (0056, amendment of 2026-11-13, the guardian).
/// Starts with none asked.
#[derive(Debug, Default)]
struct Renewals {
    /// The number of heartbeats asked so far. The store receives it, counted from 0.
    asked: u32,
    /// The instant at which the last heartbeat was asked, whatever its answer.
    last_asked: Option<Instant>,
}

/// Starts the guardian of the attempt group, when the plan asks for one, and waits for its ready
/// byte. Its name is the group's last path component. Its deadline is the readiness deadline plus
/// the wall deadline plus `GUARDIAN_MARGIN`, and its window is a third of the lease (0056,
/// amendment of 2026-11-13, the guardian). The guardian is stored in `session`, so that every
/// later path ends it.
fn start_guardian(
    plan: &AttemptPlan,
    roots: &AdmissionRoots<'_>,
    group_path: &Path,
    session: &mut Session,
) -> Result<(), AttemptFailure> {
    if !plan.guardian {
        return Ok(());
    }
    let not_started = AttemptFailure::NotStarted(NotStartedReason::GuardianNotStarted);
    let name = group_path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or(not_started)?;
    let deadline = plan
        .readiness_deadline
        .saturating_add(plan.wall_deadline)
        .saturating_add(GUARDIAN_MARGIN);
    let started = GuardianLink::start(
        roots.tool_root,
        plan.program,
        name,
        deadline,
        plan.renewal_window(),
        plan.readiness_deadline,
        session,
    )?;
    session.guardian = Some(started);
    Ok(())
}

/// Pauses the provider at `phase`, when the plan pauses there. The empty marker file named after
/// the phase is created first, so that a test can wait for it. Then the pause holds in a loop
/// until the process is killed, or stalls for its duration and goes on. A marker that cannot be
/// created is ignored: the waiting test then fails for want of it.
fn pause_at(plan: &AttemptPlan, phase: Phase) {
    let Some(pause) = plan.pause.as_ref().filter(|pause| pause.phase == phase) else {
        return;
    };
    pause_marked(&pause.markers, phase.word(), pause.mode);
}

/// Creates the empty marker file `<markers>/<word>`, then holds in a loop until the process is
/// killed, or stalls for the mode's duration and returns. A marker that cannot be created is
/// ignored: the waiting test then fails for want of it.
pub(super) fn pause_marked(markers: &Path, word: &str, mode: PauseMode) {
    fs::File::create(markers.join(word)).ok();
    match mode {
        PauseMode::Hold => loop {
            thread::sleep(HOLD_STEP);
        },
        PauseMode::Stall(duration) => thread::sleep(duration),
    }
}

/// Asks `renew`, the store's checkpoint, for a heartbeat when one is due (see `heartbeat_due`) and
/// applies its answer. A confirmation sends `R` to the guardian. An unavailable store sends
/// nothing. A lost lease returns its part as `LeaseLost`, unless the plan renews regardless (0042
/// rule 6; 0056, amendment of 2026-11-13, the guardian; 0057 rule 7).
fn heartbeat(
    plan: &AttemptPlan,
    renew: &mut dyn FnMut(u32) -> Heartbeat,
    renewals: &mut Renewals,
    guardian: Option<&mut GuardianLink>,
    now: Instant,
) -> Result<(), AttemptFailure> {
    if !heartbeat_due(renewals.last_asked, now, plan.renewal_window()) {
        return Ok(());
    }
    let answer = renew(renewals.asked);
    renewals.asked = renewals.asked.saturating_add(1);
    renewals.last_asked = Some(now);
    if on_answer(answer, plan.renew_regardless).map_err(AttemptFailure::LeaseLost)? {
        send_to_guardian(guardian, guardian::RENEW);
    }
    Ok(())
}

/// Writes one record to the guardian, when there is one. A failed write is ignored: a guardian
/// that can no longer read is judged by the report it wrote and by `finish` (0056, amendment of
/// 2026-11-13, the guardian).
fn send_to_guardian(guardian: Option<&mut GuardianLink>, record: u8) {
    if let Some(link) = guardian {
        link.send(record).ok();
    }
}

/// Ends the group phase's part of the attempt and the guardian's part, in this order: sends `S`,
/// settles the group (`settle`), sends `D` when the group was removed and otherwise closes the
/// provider's side of the socket, then reads the guardian's report and reaps it. A
/// guardian that is not reaped without a kill leaves the cleanup unobserved, and the provider is
/// quarantined. Returns the settled group and the reason that the guardian reported, if any.
fn conclude(
    group: ProbeGroup,
    group_path: &Path,
    session: &mut Session,
) -> (Settled, Option<GuardianReason>) {
    send_to_guardian(session.guardian.as_mut(), guardian::STOP);
    let mut settled = settle(group, group_path, session);
    let Some(mut link) = session.guardian.take() else {
        return (settled, None);
    };
    let within = if settled.cleanup_observed {
        link.send(guardian::DONE).ok();
        probe_group::CLEANUP_DEADLINE
    } else {
        // The group may still exist, so the guardian gets no `D`. It reads end of file instead and
        // ends the attempt itself, which takes up to its own cleanup deadline after its kill, so
        // its line is awaited for that and `GUARDIAN_REPORT_MARGIN` more.
        link.close_records();
        probe_group::CLEANUP_DEADLINE.saturating_add(GUARDIAN_REPORT_MARGIN)
    };
    let (report, reaped) = link.finish(within);
    if !reaped {
        settled.cleanup_observed = false;
        session.enter_quarantine(QuarantineReason::Guardian);
    }
    (settled, report.map(|(reason, _emptied)| reason))
}

#[cfg(test)]
mod crash_tests;
mod guardian_link;
#[cfg(test)]
mod guardian_tests;
#[cfg(test)]
mod io_tests;
#[cfg(test)]
mod isolation_tests;
mod keeper;
#[cfg(test)]
mod kernel_log;
#[cfg(test)]
mod mapping_tests;
#[cfg(test)]
mod registry_tests;
#[cfg(test)]
mod release_tests;
#[cfg(test)]
mod renewal_tests;
#[cfg(test)]
mod resource_tests;
#[cfg(test)]
mod tests;
