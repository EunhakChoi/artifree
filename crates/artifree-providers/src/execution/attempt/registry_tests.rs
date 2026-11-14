//! Tests of the attempt registry (decision 0056, amendment of 2026-11-14).
//!
//! `a_quarantine_file_refuses_a_launch` runs in every test build. The `job_` tests need the Linux
//! enforcement job: run.sh part 12 runs each one by its exact name in its own unit, and each reads
//! the `ARTIFREE_*` variables that run.sh sets for it. A job test that quarantines the provider
//! runs alone, because the in-process quarantine is shared by the whole process.

use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

use super::super::cgroup;
use super::super::registry::{
    self, Entry, HostIdentity, Identity, ReconcileOutcome, Reconciled, Record, Registry,
    RegistryRefusal,
};
use super::resource_tests::evidence_of;
use super::tests::{ExecutionDir, assert_no_attempt_groups, refusal_of};
use super::{AttemptFailure, AttemptPlan, launch, reconcile_only};

/// The mount of the cgroup v2 hierarchy, as the admission reads it.
const CGROUP_MOUNT: &str = "/sys/fs/cgroup";
/// The fixed identity of the root-owned decoy, whose group run.sh names: bytes 1 to 16 in order.
const ROOT_DECOY_IDENTITY: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
/// The group name that `ROOT_DECOY_IDENTITY` gives, spelled as run.sh spells it.
const ROOT_DECOY_GROUP: &str = "attempt-72623859790382856-651345242494996240";
/// The parent of the root-owned decoy, relative to the cgroup mount.
const ROOT_DECOY_PARENT: &str = "artifree-decoy";
/// The identity of the leftover that a test plants as a group with a live process.
const LEFTOVER_IDENTITY: [u8; 16] = [0x11; 16];
/// An identity that no test plants as a group of its own.
const OTHER_IDENTITY: [u8; 16] = [0x22; 16];
/// The attempts directory of an execution directory, spelled out (0056, amendment of 2026-11-14).
const ATTEMPTS_NAME: &str = "attempts";
/// The persisted quarantine file of an execution directory.
const QUARANTINE_NAME: &str = "quarantine";
/// The staged input directory of an execution directory.
const INPUT_NAME: &str = "input";
/// A file in the attempts directory that is neither a record nor a temporary file. Its name sorts
/// before any record's, so reconciliation refuses it before it touches a group.
const STRAY_NAME: &str = ".notes";
/// The persisted quarantine of a refused record: the tag line and the reason `registry`.
const REGISTRY_QUARANTINE: &str = "artifree-quarantine-v1\nreason=registry\n";
/// The persisted quarantine of a routing change: the tag line and the reason `routing`.
const ROUTING_QUARANTINE: &str = "artifree-quarantine-v1\nreason=routing\n";
/// The part of the operator text that every quarantine refusal must show.
const OPERATOR_FRAGMENT: &str = "operator action is required";
/// Two machine identities, each valid, that differ.
const MACHINE_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The second machine identity.
const MACHINE_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// Two boot identities, each in UUID form, that differ.
const BOOT_A: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
/// The second boot identity.
const BOOT_B: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
/// The signal that `cgroup.kill` sends to every process of a group.
const SIGKILL: i32 = 9;
/// The program of a decoy: a sleep that only a kill ends within the test.
const SLEEP_PROGRAM: &str = "/bin/sleep";
/// The seconds that a decoy's sleep runs for.
const SLEEP_SECONDS: &str = "600";
/// The wait between two looks at a decoy.
const WAIT_STEP: Duration = Duration::from_millis(10);
/// The number of looks at a decoy: about five seconds in all.
const WAIT_STEPS: u32 = 500;
/// The variable that names the execution directory of the reconciliation unit.
const RECONCILE_DIR_VARIABLE: &str = "ARTIFREE_RECONCILE_DIR";
/// The variable that names the phase at which the first unit's supervisor died.
const HOLD_PHASE_VARIABLE: &str = "ARTIFREE_HOLD_PHASE";
/// The variable that names the malformed-record case of this unit.
const BAD_RECORD_VARIABLE: &str = "ARTIFREE_BAD_RECORD";
/// The variable that names the root-owned decoy group.
const DECOY_GROUP_VARIABLE: &str = "ARTIFREE_DECOY_GROUP";
/// The variable that names the kept execution directory of the persisted quarantine.
const PERSIST_DIR_VARIABLE: &str = "ARTIFREE_PERSIST_DIR";

/// A quarantine file in the execution directory refuses a launch before admission. The failure is
/// `Quarantined` with the operator text, and no attempts directory is created.
#[test]
fn a_quarantine_file_refuses_a_launch() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("registry-quarantine-file")?;
    fs::write(dir.path().join(QUARANTINE_NAME), b"any content")?;
    let failure = refusal_of(launch(&AttemptPlan::fixture(), dir.path()))?;
    assert_eq!(
        failure,
        AttemptFailure::Quarantined,
        "a quarantine file must refuse the launch as quarantined"
    );
    assert_operator_text(failure);
    assert!(
        !exists(&dir.path().join(ATTEMPTS_NAME))?,
        "a refused launch must not create the attempts directory"
    );
    Ok(())
}

/// After the supervisor's death at `registered` or at `created` (run.sh part 12), reconciliation in
/// a second unit settles the one record. The first unit stopped before this one started, so the
/// record's parent is gone and the record settles as gone; the staged input is removed with the
/// record. The next launch then passes and leaves nothing behind.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_reconcile_after_death() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::existing(PathBuf::from(required(RECONCILE_DIR_VARIABLE)?))?;
    let phase = required(HOLD_PHASE_VARIABLE)?;
    // Only `created` stages the input, after the record exists.
    let staged = match phase.as_str() {
        "registered" => false,
        "created" => true,
        other => {
            return Err(format!(
                "{HOLD_PHASE_VARIABLE} must be registered or created, not {other:?}"
            )
            .into());
        }
    };
    let entries = registry_entries(dir.path())?;
    let [Entry::Record { identity, bytes }] = entries.as_slice() else {
        return Err(format!("the registry must hold exactly one record, found {entries:?}").into());
    };
    let identity = *identity;
    let record = Record::parse(bytes)
        .map_err(|refusal| format!("the record does not parse: {refusal:?}"))?;
    assert_eq!(
        exists(&dir.path().join(INPUT_NAME))?,
        staged,
        "the staged input must exist exactly at {phase}"
    );
    assert_eq!(
        reconcile_alone(dir.path()),
        Ok(vec![Reconciled {
            identity,
            outcome: ReconcileOutcome::Gone,
        }]),
        "the record of the death at {phase} must be reconciled as gone"
    );
    assert!(
        !exists(&dir.path().join(INPUT_NAME))?,
        "reconciliation must remove the staged input"
    );
    assert!(
        registry_entries(dir.path())?.is_empty(),
        "reconciliation must remove the record"
    );
    let delegated = delegated()?;
    assert!(
        !exists(&delegated.join(&record.group))?,
        "no attempt group of the record may remain beneath the delegated group"
    );
    let evidence = evidence_of(launch(&AttemptPlan::fixture(), dir.path()))?;
    assert_eq!(
        evidence.exit.code(),
        Some(0),
        "the launch after the reconciliation must pass"
    );
    assert!(
        registry_entries(dir.path())?.is_empty(),
        "the launch after the reconciliation must leave no record"
    );
    assert_no_attempt_groups()
}

/// A record from this boot and namespace whose group still holds a process: reconciliation kills
/// the group, sees it empty and removes it, then removes the record. The sleep ends by SIGKILL.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_same_boot_leftover_is_killed() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("registry-same-boot")?;
    // An empty registry moves the test into its supervisor group, so the delegated group exists.
    assert_eq!(
        reconcile_alone(dir.path()),
        Ok(Vec::new()),
        "an empty registry settles nothing"
    );
    let (delegated, parent) = delegated_relative()?;
    let leftover = Identity(LEFTOVER_IDENTITY);
    let group = leftover.group_name();
    let mut decoy = Decoy::start(&delegated, &group)?;
    write_record(dir.path(), &record_for(leftover, &host()?, &parent, &group))?;
    assert!(
        decoy.is_alive()?,
        "the decoy must run before reconciliation"
    );
    assert_eq!(
        reconcile_alone(dir.path()),
        Ok(vec![Reconciled {
            identity: leftover,
            outcome: ReconcileOutcome::Killed,
        }]),
        "a same-boot record must have its group killed and removed"
    );
    let status = decoy.exit_within()?;
    assert_eq!(
        status.and_then(|status| status.signal()),
        Some(SIGKILL),
        "the leftover's sleep must end by SIGKILL"
    );
    assert!(
        !exists(&delegated.join(&group))?,
        "the leftover's group must be removed"
    );
    assert!(
        registry_entries(dir.path())?.is_empty(),
        "the leftover's record must be removed"
    );
    assert_no_attempt_groups()
}

/// A record from an earlier boot names a group that still holds a process: reconciliation removes
/// the record and touches no group, so the group and its sleep are left running. The decoy is ended
/// by its own drop when the test returns.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_changed_boot_leaves_its_group_alone() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("registry-changed-boot")?;
    assert_eq!(
        reconcile_alone(dir.path()),
        Ok(Vec::new()),
        "an empty registry settles nothing"
    );
    let (delegated, parent) = delegated_relative()?;
    let leftover = Identity(LEFTOVER_IDENTITY);
    let group = leftover.group_name();
    let mut decoy = Decoy::start(&delegated, &group)?;
    let host = host()?;
    let earlier = HostIdentity {
        boot: other_boot(&host.boot),
        ..host
    };
    write_record(dir.path(), &record_for(leftover, &earlier, &parent, &group))?;
    assert!(
        decoy.is_alive()?,
        "the decoy must run before reconciliation"
    );
    assert_eq!(
        reconcile_alone(dir.path()),
        Ok(vec![Reconciled {
            identity: leftover,
            outcome: ReconcileOutcome::ChangedBoot,
        }]),
        "a record from an earlier boot must settle as a changed boot"
    );
    assert!(
        decoy.is_alive()?,
        "a group of an earlier boot must be left running"
    );
    assert!(
        exists(&delegated.join(&group))?,
        "a group of an earlier boot must be left in place"
    );
    assert!(
        registry_entries(dir.path())?.is_empty(),
        "the record of an earlier boot must be removed"
    );
    Ok(())
}

/// A malformed or foreign record, or a stray entry, refuses reconciliation and quarantines the
/// provider. The record and its decoy group are left as they were. Each case is run in its own
/// unit, because a quarantine is shared by the whole process.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_bad_records_quarantine() -> Result<(), Box<dyn Error>> {
    let case = required(BAD_RECORD_VARIABLE)?;
    let dir = ExecutionDir::new(&format!("registry-bad-{case}"))?;
    assert_eq!(
        reconcile_alone(dir.path()),
        Ok(Vec::new()),
        "an empty registry settles nothing"
    );
    let (delegated, parent) = delegated_relative()?;
    let host = host()?;
    let leftover = Identity(LEFTOVER_IDENTITY);
    let group = leftover.group_name();
    let base = record_for(leftover, &host, &parent, &group);
    let (record, stray) = match case.as_str() {
        // The record names the leftover's group, which is not its own identity's group.
        "group-name" => (
            Record {
                identity: Identity(OTHER_IDENTITY),
                ..base
            },
            false,
        ),
        // A parent that climbs out of the supervisor group names the leftover's decoy.
        "dotdot" => (
            Record {
                parent: format!("{parent}/supervisor/.."),
                ..base
            },
            false,
        ),
        "machine" => (
            Record {
                machine: other_machine(&host.machine),
                ..base
            },
            false,
        ),
        "namespace" => (
            Record {
                cgroupns: format!("{}-other", host.cgroupns),
                ..base
            },
            false,
        ),
        "stray" => (base, true),
        other => return Err(format!("unknown {BAD_RECORD_VARIABLE} value {other:?}").into()),
    };
    let mut decoy = Decoy::start(&delegated, &group)?;
    write_record(dir.path(), &record)?;
    if stray {
        fs::write(
            dir.path().join(ATTEMPTS_NAME).join(STRAY_NAME),
            b"not a record\n",
        )?;
    }
    assert!(
        decoy.is_alive()?,
        "{case}: the decoy must run before reconciliation"
    );
    let failure = refusal_of_settle(reconcile_alone(dir.path()))?;
    assert_eq!(
        failure,
        AttemptFailure::Quarantined,
        "{case}: a refused record must quarantine the provider"
    );
    assert_operator_text(failure);
    assert_quarantine(dir.path(), REGISTRY_QUARANTINE)?;
    assert!(
        exists(&record_path(dir.path(), record.identity))?,
        "{case}: the refused record must stay"
    );
    assert!(
        decoy.is_alive()?,
        "{case}: the refused record must not end its group's process"
    );
    assert!(
        exists(&delegated.join(&group))?,
        "{case}: the decoy group must stay"
    );
    if stray {
        assert!(
            exists(&dir.path().join(ATTEMPTS_NAME).join(STRAY_NAME))?,
            "{case}: the stray entry must stay"
        );
    }
    Ok(())
}

/// A record whose parent is the root-owned `artifree-decoy` group is refused by the owner check
/// and not ended. The registry refuses with `Owner`, and the launch quarantines the provider and
/// keeps the record. run.sh checks that the root-owned decoy stays populated and alive.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_root_owned_parent_quarantines() -> Result<(), Box<dyn Error>> {
    let group = required(DECOY_GROUP_VARIABLE)?;
    assert_eq!(
        group, ROOT_DECOY_GROUP,
        "run.sh must name the decoy group of the fixed identity"
    );
    assert_eq!(
        Identity(ROOT_DECOY_IDENTITY).group_name(),
        ROOT_DECOY_GROUP,
        "the fixed identity must name the decoy group"
    );
    let dir = ExecutionDir::new("registry-root-owned-parent")?;
    let host = host()?;
    let identity = Identity(ROOT_DECOY_IDENTITY);
    write_record(
        dir.path(),
        &record_for(identity, &host, ROOT_DECOY_PARENT, &group),
    )?;
    // The registry's own refusal, before the launch reduces it to a quarantine.
    let own_uid = cgroup::own_uid(Path::new("/proc/self"))?;
    let direct: Result<Vec<Reconciled>, RegistryRefusal> =
        registry::reconcile(dir.path(), Path::new(CGROUP_MOUNT), own_uid, &host);
    assert_eq!(
        direct,
        Err(RegistryRefusal::Owner),
        "a parent of another account must be refused by its owner"
    );
    let failure = refusal_of_settle(reconcile_alone(dir.path()))?;
    assert_eq!(
        failure,
        AttemptFailure::Quarantined,
        "a refused parent must quarantine the provider"
    );
    assert_operator_text(failure);
    assert_quarantine(dir.path(), REGISTRY_QUARANTINE)?;
    assert!(
        exists(&record_path(dir.path(), identity))?,
        "the record must stay, since its group was not ended"
    );
    Ok(())
}

/// The persisted quarantine of a routing change (run.sh part 12, in this directory) refuses a
/// second process at admission until the operator removes the file. Once the file is removed, the
/// launch passes after it reconciles the stale record of the routing change.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_quarantine_is_persisted() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::existing(PathBuf::from(required(PERSIST_DIR_VARIABLE)?))?;
    assert_quarantine(dir.path(), ROUTING_QUARANTINE)?;
    let refused = refusal_of(launch(&AttemptPlan::fixture(), dir.path()))?;
    assert_eq!(
        refused,
        AttemptFailure::Quarantined,
        "the persisted quarantine must refuse a second process"
    );
    assert_operator_text(refused);
    fs::remove_file(dir.path().join(QUARANTINE_NAME))?;
    let evidence = evidence_of(launch(&AttemptPlan::fixture(), dir.path()))?;
    assert_eq!(
        evidence.exit.code(),
        Some(0),
        "the launch after the operator removed the quarantine file must pass"
    );
    assert!(
        registry_entries(dir.path())?.is_empty(),
        "the launch must leave no record"
    );
    assert_no_attempt_groups()
}

/// The value of an environment variable that run.sh sets for this test. The test sets none.
fn required(name: &str) -> Result<String, Box<dyn Error>> {
    env::var(name).map_err(|error| format!("{name} must be set by run.sh: {error}").into())
}

/// Whether `path` exists, without following a link. Only an absent path is `false`; any other
/// failure is an error, so that a check cannot pass by accident.
fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// The host's identity, read the way the admission reads it.
fn host() -> Result<HostIdentity, Box<dyn Error>> {
    Ok(HostIdentity::read(
        Path::new("/proc/self"),
        Path::new("/etc/machine-id"),
        Path::new("/proc/sys/kernel/random/boot_id"),
    )?)
}

/// The delegated group: the parent of the test's `supervisor` group. It is known only after an
/// admission has moved the test into `supervisor`.
fn delegated() -> Result<PathBuf, Box<dyn Error>> {
    let own = cgroup::discover_own_group(Path::new("/proc/self/cgroup"), Path::new(CGROUP_MOUNT))?;
    if own.file_name() != Some(OsStr::new(cgroup::SUPERVISOR_GROUP)) {
        return Err(format!(
            "the test process is in {}, not in its supervisor group",
            own.display()
        )
        .into());
    }
    let Some(delegated) = own.parent() else {
        return Err(format!("{} has no delegated parent", own.display()).into());
    };
    Ok(delegated.to_path_buf())
}

/// The delegated group, and its path relative to the cgroup mount, which is the form that a
/// record names.
fn delegated_relative() -> Result<(PathBuf, String), Box<dyn Error>> {
    let delegated = delegated()?;
    let relative = delegated
        .strip_prefix(CGROUP_MOUNT)?
        .to_str()
        .ok_or("the delegated group is not UTF-8")?
        .to_owned();
    Ok((delegated, relative))
}

/// The record of `identity` with the host's identity, the parent and the group.
fn record_for(identity: Identity, host: &HostIdentity, parent: &str, group: &str) -> Record {
    Record {
        identity,
        machine: host.machine.clone(),
        boot: host.boot.clone(),
        cgroupns: host.cgroupns.clone(),
        parent: parent.to_owned(),
        group: group.to_owned(),
        run: None,
    }
}

/// The path of the record file of `identity` in the attempts directory of `dir`.
fn record_path(dir: &Path, identity: Identity) -> PathBuf {
    dir.join(ATTEMPTS_NAME)
        .join(format!("{}.record", identity.hex()))
}

/// Writes `record` as the record file of its identity, byte for byte. The attempts directory is
/// opened first, so that it exists with the mode the registry requires. A malformed record can be
/// written this way, which the registry's own write refuses.
fn write_record(dir: &Path, record: &Record) -> Result<(), Box<dyn Error>> {
    Registry::open(dir)?;
    fs::write(record_path(dir, record.identity), record.render())?;
    Ok(())
}

/// The entries of the attempts directory of `dir`, as the registry lists them.
fn registry_entries(dir: &Path) -> Result<Vec<Entry>, Box<dyn Error>> {
    Ok(Registry::open(dir)?.entries()?)
}

/// The machine identity that differs from `machine`.
fn other_machine(machine: &str) -> String {
    let other = if machine == MACHINE_A {
        MACHINE_B
    } else {
        MACHINE_A
    };
    other.to_owned()
}

/// The boot identity that differs from `boot`.
fn other_boot(boot: &str) -> String {
    let other = if boot == BOOT_A { BOOT_B } else { BOOT_A };
    other.to_owned()
}

/// One reconciliation of `dir` with the failure of a refusal reduced to its failure. It takes the
/// admission lock and admits, so it moves the test into its `supervisor` group.
fn reconcile_alone(dir: &Path) -> Result<Vec<Reconciled>, AttemptFailure> {
    reconcile_only(dir).map_err(|refusal| refusal.failure)
}

/// The failure of a reconciliation that must refuse. A reconciliation that settles is an error.
fn refusal_of_settle(
    outcome: Result<Vec<Reconciled>, AttemptFailure>,
) -> Result<AttemptFailure, Box<dyn Error>> {
    match outcome {
        Ok(settled) => Err(format!("the registry must refuse, but it settled {settled:?}").into()),
        Err(failure) => Ok(failure),
    }
}

/// Asserts that a quarantine refusal shows the operator text.
fn assert_operator_text(failure: AttemptFailure) {
    let text = failure.operator_text();
    assert!(
        text.is_some_and(|text| text.contains(OPERATOR_FRAGMENT)),
        "{failure:?} must show the operator text, got {text:?}"
    );
}

/// Asserts that the quarantine file of `dir` holds `expected`, its tag line and its reason.
fn assert_quarantine(dir: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    let held = fs::read_to_string(dir.join(QUARANTINE_NAME))?;
    assert_eq!(
        held, expected,
        "the quarantine file must hold its tag and reason"
    );
    Ok(())
}

/// Whether the group at `path` holds no process: its `cgroup.events` reports `populated 0`, or
/// the group is gone.
fn group_is_empty(path: &Path) -> bool {
    match fs::read_to_string(path.join("cgroup.events")) {
        Ok(events) => events.lines().any(|line| line == "populated 0"),
        Err(error) => error.kind() == io::ErrorKind::NotFound,
    }
}

/// A `sleep` in a group of its own, beneath the delegated group. Dropping it ends the sleep and
/// removes the group, so that no decoy outlives its test.
#[derive(Debug)]
struct Decoy {
    /// The group's directory.
    path: PathBuf,
    /// The sleeping process, which is moved into the group.
    child: Child,
}

impl Decoy {
    /// Makes the group `name` beneath `parent` and moves a sleep into it.
    ///
    /// # Errors
    /// Returns the I/O error of a step that fails. A sleep that was started is ended and the
    /// group removed before the error is returned.
    fn start(parent: &Path, name: &str) -> Result<Self, Box<dyn Error>> {
        let path = parent.join(name);
        fs::create_dir(&path)?;
        let child = Command::new(SLEEP_PROGRAM)
            .arg(SLEEP_SECONDS)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .inspect_err(|_| {
                fs::remove_dir(&path).ok();
            })?;
        let decoy = Self { path, child };
        fs::write(
            decoy.path.join("cgroup.procs"),
            decoy.child.id().to_string(),
        )?;
        Ok(decoy)
    }

    /// Whether the sleep is still running.
    ///
    /// # Errors
    /// Returns the I/O error of the status query.
    fn is_alive(&mut self) -> io::Result<bool> {
        Ok(self.child.try_wait()?.is_none())
    }

    /// Waits for the sleep to exit, for about five seconds, and returns its status when it did.
    ///
    /// # Errors
    /// Returns the I/O error of a status query.
    fn exit_within(&mut self) -> io::Result<Option<ExitStatus>> {
        for _ in 0..WAIT_STEPS {
            if let Some(status) = self.child.try_wait()? {
                return Ok(Some(status));
            }
            thread::sleep(WAIT_STEP);
        }
        Ok(None)
    }
}

impl Drop for Decoy {
    fn drop(&mut self) {
        // The sleep is ended and reaped whatever the test did. A sleep that is already gone is not
        // an error here.
        self.child.kill().ok();
        self.child.wait().ok();
        for _ in 0..WAIT_STEPS {
            if group_is_empty(&self.path) {
                break;
            }
            thread::sleep(WAIT_STEP);
        }
        let removed = fs::remove_dir(&self.path);
        let gone = match &removed {
            Ok(()) => true,
            Err(error) => error.kind() == io::ErrorKind::NotFound,
        };
        // A group left behind would break a later check, unless this test is already failing.
        assert!(
            gone || thread::panicking(),
            "could not remove the decoy group {}: {removed:?}",
            self.path.display()
        );
    }
}
