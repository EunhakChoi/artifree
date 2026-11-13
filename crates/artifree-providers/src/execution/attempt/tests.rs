//! Behavioral tests for one attempt: its argument lists, its plan, the wait decision after
//! the start, and the rendering of its evidence (decisions 0042 rules 5 and 6, 0056 rules 2,
//! 5 and 14).
//!
//! The local tests run in the normal `cargo test`. The `job_*` tests need the prepared Linux
//! enforcement runner, so they are ignored here, and `scripts/enforcement/run.sh` runs each
//! one by its exact name. Each job test looks at every `attempt-*` group under the delegated
//! group, so the job script runs them one at a time.

use super::super::cgroup::SUPERVISOR_GROUP;
use super::super::readiness::{LimitFacts, LimitPair, ScratchMount, StatusFacts};
use super::super::usage::GroupUsage;
use super::*;

use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;

/// The attempt's argument list for the root `/opt/t` and the staged input `/opt/i`, exactly as the launch passes it. The launcher binds the input read-only at `/input` right after the sandbox arguments, makes the scratch's mount point and remounts `/` and `/dev` read-only; the bootstrap mounts the scratch.
const ATTEMPT_ARGUMENTS: &[&str] = &[
    "--unshare-user",
    "--unshare-pid",
    "--unshare-net",
    "--unshare-ipc",
    "--unshare-uts",
    "--unshare-cgroup",
    "--disable-userns",
    "--die-with-parent",
    "--new-session",
    "--clearenv",
    "--ro-bind",
    "/opt/t",
    "/tools",
    "--ro-bind",
    "/opt/t/lib",
    "/lib",
    "--ro-bind",
    "/opt/t/lib64",
    "/lib64",
    "--proc",
    "/proc",
    "--dev",
    "/dev",
    "--ro-bind",
    "/opt/i",
    "/input",
    "--cap-add",
    "CAP_SYS_ADMIN",
    "--cap-add",
    "CAP_SETPCAP",
    "--dir",
    "/tmp",
    "--perms",
    "0555",
    "--size",
    "4096",
    "--tmpfs",
    "/dev/pts",
    "--remount-ro",
    "/",
    "--remount-ro",
    "/dev",
    "--remount-ro",
    "/dev/pts",
    "--chdir",
    "/",
    "/tools/bin/artifree-execution-fixture",
    "bootstrap",
    "artifree-fixture-inspection-v1",
];

/// The admission's namespace check's argument list for the root `/opt/t`, unchanged since R02aa:
/// the shared namespace and bind options, its own 1 MiB scratch, no capabilities and no
/// read-only remounts (0056, amendment of 2026-11-12, rule 2), and the `namespaces` mode in place
/// of the bootstrap.
const CHECK_ARGUMENTS: &[&str] = &[
    "--unshare-user",
    "--unshare-pid",
    "--unshare-net",
    "--unshare-ipc",
    "--unshare-uts",
    "--unshare-cgroup",
    "--disable-userns",
    "--die-with-parent",
    "--new-session",
    "--clearenv",
    "--ro-bind",
    "/opt/t",
    "/tools",
    "--ro-bind",
    "/opt/t/lib",
    "/lib",
    "--ro-bind",
    "/opt/t/lib64",
    "/lib64",
    "--proc",
    "/proc",
    "--dev",
    "/dev",
    "--size",
    "1048576",
    "--tmpfs",
    "/tmp",
    "--chdir",
    "/",
    "/tools/bin/artifree-execution-fixture",
    "namespaces",
];

/// A SHA-256 digest used as a fixed value in the rendering test; it is the digest of no input.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The launcher arguments as text, one string per argument.
fn text_arguments(arguments: &[OsString]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

/// A host report with every value set, as one admission run records it.
fn sample_report() -> HostReport {
    HostReport {
        kernel_release: "7.0.0-test".to_owned(),
        architecture: "x86_64",
        launcher_version: Some("bubblewrap 0.9.0".to_owned()),
        launcher_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            .to_owned(),
        tool_manifest_sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
            .to_owned(),
        policy_sha256: EMPTY_SHA256.to_owned(),
        core_routing: "|/usr/lib/systemd/systemd-coredump".to_owned(),
        delegated_controllers: "cpu io memory pids".to_owned(),
        effective_limits: vec![("memory.max", "1073741824".to_owned())],
        peak_memory_bytes: Some(1_048_576),
    }
}

#[test]
fn attempt_arguments_add_the_capabilities_and_leave_the_scratch_to_the_bootstrap() {
    let arguments = text_arguments(&launcher::attempt_arguments(
        Path::new("/opt/t"),
        Path::new("/opt/i"),
    ));
    assert_eq!(
        arguments, ATTEMPT_ARGUMENTS,
        "the attempt must bind the input read-only at /input after the sandbox arguments, make the scratch's mount point, add the two capabilities, remount / and /dev read-only, and then run the fixture's bootstrap mode"
    );
}

#[test]
fn namespace_check_arguments_keep_their_own_scratch_and_mode() {
    let arguments = text_arguments(&launcher::launcher_arguments(Path::new("/opt/t")));
    assert_eq!(
        arguments, CHECK_ARGUMENTS,
        "the namespace check must use the 1 MiB scratch and the namespaces mode"
    );
}

#[test]
fn running_verdict_boundaries_and_output_precedence() {
    let limit = Duration::from_secs(120);
    let cases: [(&str, Duration, bool, Result<(), AttemptFailure>); 4] = [
        (
            "just before the deadline",
            Duration::from_millis(119_999),
            false,
            Ok(()),
        ),
        (
            "exactly at the deadline",
            limit,
            false,
            Err(AttemptFailure::DeadlineExceeded),
        ),
        (
            "output past the budget before the deadline",
            Duration::from_secs(1),
            true,
            Err(AttemptFailure::OutputLimitExceeded),
        ),
        (
            "output past the budget after the deadline",
            Duration::from_secs(121),
            true,
            Err(AttemptFailure::OutputLimitExceeded),
        ),
    ];
    for (case, elapsed, output_exceeded, expected) in cases {
        assert_eq!(
            running_verdict(elapsed, limit, output_exceeded),
            expected,
            "the verdict for {case} is wrong"
        );
    }
}

fn sample_evidence() -> AttemptEvidence {
    AttemptEvidence {
        group: PathBuf::from("/sys/fs/cgroup/test/attempt-1-0"),
        limits: vec![
            ("memory.max", "1073741824".to_owned()),
            ("pids.max", "64".to_owned()),
        ],
        exit: ExitStatus::from_raw(0),
        stdout: b"secret-text".to_vec(),
        stderr: b"stderr-secret".to_vec(),
        wall_time: Duration::from_millis(1234),
        peak_memory_bytes: Some(4_194_304),
        usage: GroupUsage {
            memory_peak_bytes: Some(4_194_304),
            oom_kills: 0,
            oom_group_kills: 0,
            task_refusals: 0,
            tasks_peak: None,
            cpu_usage_usec: 1500,
            cpu_periods: 3,
            cpu_throttled_periods: 0,
        },
        readiness: GroupObservation {
            members: 2,
            bootstrap_pid: 4242,
            status: StatusFacts {
                no_new_privs: 1,
                seccomp: 2,
                seccomp_filters: 2,
                cap_inh: 0,
                cap_prm: 0,
                cap_eff: 0,
                cap_bnd: 0,
                cap_amb: 0,
            },
            limits: LimitFacts {
                open_files: LimitPair {
                    soft: Some(256),
                    hard: Some(256),
                },
                core_bytes: LimitPair {
                    soft: Some(0),
                    hard: Some(0),
                },
            },
            scratch: ScratchMount {
                mount_id: 1299,
                is_tmpfs: true,
                read_only: false,
                no_suid_dev_exec: true,
                size_kib: Some(131_072),
                inodes: Some(4_096),
                mode: Some(0o700),
            },
        },
        filter_sha256: EMPTY_SHA256.to_owned(),
        report: sample_report(),
        output: Err(OutputRefusal::NotCollected),
    }
}

#[test]
fn evidence_renders_its_keys_and_never_the_captured_bytes() {
    let evidence = sample_evidence();
    let rendered = evidence.to_string();
    let lines: Vec<&str> = rendered.lines().collect();
    let expected_lines = [
        "attempt_group=/sys/fs/cgroup/test/attempt-1-0",
        "attempt_limit.memory.max=1073741824",
        "exit=code 0",
        "wall_time_ms=1234",
        "stdout_bytes=11",
        "stderr_bytes=13",
        "attempt_peak_memory_bytes=4194304",
        "attempt_usage.memory_peak_bytes=4194304",
        "attempt_usage.tasks_peak=unavailable",
        "attempt_usage.cpu_usage_usec=1500",
        "readiness.members=2",
        "readiness.bootstrap_pid=4242",
        "readiness.no_new_privs=1",
        "readiness.seccomp=2",
        "readiness.seccomp_filters=2",
        "readiness.cap_eff=0000000000000000",
        "readiness.open_files=256 256",
        "readiness.core_bytes=0 0",
        "readiness.scratch_size=131072k",
        "readiness.scratch_inodes=4096",
        "filter_sha256=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    ];
    for expected in expected_lines {
        assert!(
            lines.contains(&expected),
            "the rendering must contain the line {expected:?}, got:\n{rendered}"
        );
    }
    // A reader that takes the first line of a key, or that refuses a repeated key, must both
    // get one answer: the attempt's lines and the embedded host report share no key.
    let mut keys: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.split_once('=').map(|(key, _)| key))
        .collect();
    let key_count = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(
        keys.len(),
        key_count,
        "every key of the rendering must appear once, got:\n{rendered}"
    );
    for secret in ["secret-text", "stderr-secret"] {
        assert!(
            !rendered.contains(secret),
            "the rendering must not contain the captured bytes {secret:?}, got:\n{rendered}"
        );
    }
}

/// A directory that a job test passes to `launch` as its execution directory. It is created
/// with mode 0o700 and removed when the test ends.
pub(super) struct ExecutionDir {
    path: PathBuf,
}

impl ExecutionDir {
    /// Creates `artifree-attempt-<pid>-<label>` under the system temporary directory. A leftover
    /// with the same name is removed first.
    pub(super) fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let path = env::temp_dir().join(format!("artifree-attempt-{}-{label}", process::id()));
        // A leftover with the same name is cleared first; its absence is expected.
        let cleared = fs::remove_dir_all(&path).or_else(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error)
            }
        });
        cleared?;
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self { path })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ExecutionDir {
    fn drop(&mut self) {
        let removed = fs::remove_dir_all(&self.path);
        // A failed removal fails the test, unless the test is already unwinding.
        assert!(
            removed.is_ok() || thread::panicking(),
            "could not remove the execution directory {}: {removed:?}",
            self.path.display()
        );
    }
}

/// The attempt groups under the delegated group. Once an admission has run, the test process is
/// inside its `supervisor` child, so the delegated group is that child's parent.
pub(super) fn attempt_groups() -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let own = super::super::cgroup::discover_own_group(
        Path::new("/proc/self/cgroup"),
        Path::new("/sys/fs/cgroup"),
    )?;
    if own.file_name() != Some(OsStr::new(SUPERVISOR_GROUP)) {
        return Err(format!(
            "the test process is in {}, not in its supervisor group",
            own.display()
        )
        .into());
    }
    let Some(delegated) = own.parent() else {
        return Err(format!("{} has no delegated parent", own.display()).into());
    };
    let mut groups = Vec::new();
    for entry in fs::read_dir(delegated)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("attempt-") {
            groups.push(entry.path());
        }
    }
    Ok(groups)
}

/// Asserts that no attempt group remains in the delegated group.
pub(super) fn assert_no_attempt_groups() -> Result<(), Box<dyn Error>> {
    let groups = attempt_groups()?;
    assert!(
        groups.is_empty(),
        "no attempt group may remain, found {groups:?}"
    );
    Ok(())
}

/// The refusal of an attempt that must not run. An attempt that ran is an error here, reported
/// by its exit status only, so that no captured output is printed.
pub(super) fn refusal_of(
    outcome: Result<AttemptEvidence, AttemptRefusal>,
) -> Result<AttemptFailure, Box<dyn Error>> {
    match outcome {
        Ok(evidence) => Err(format!(
            "the attempt must not run, but it exited with {:?}",
            evidence.exit
        )
        .into()),
        Err(refusal) => Ok(refusal.failure),
    }
}

/// Runs one launch that must end before the release as `expected`, then checks that the
/// attempt left no group behind.
fn assert_not_started(
    label: &str,
    plan: &AttemptPlan,
    expected: NotStartedReason,
) -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new(label)?;
    let failure = refusal_of(launch(plan, dir.path()))?;
    assert_eq!(
        failure,
        AttemptFailure::NotStarted(expected),
        "the attempt {label} must count as not started"
    );
    assert_no_attempt_groups()
}

// The job tests below need the prepared runner. Each one is run by its exact name, one at a time.

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_fixture_attempt_runs_in_its_group_and_exits() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("fixture")?;
    let outcome = launch(&AttemptPlan::fixture(), dir.path());
    let evidence = outcome
        .map_err(|refusal| format!("the fixture attempt was not run: {:?}", refusal.failure))?;
    // Written before any assertion, so that a failed run still leaves its report behind.
    if let Some(report) = env::var_os("ARTIFREE_ATTEMPT_REPORT") {
        fs::write(report, evidence.to_string())?;
    }

    // A launch that returns Ok does not prove the payload ran: the exit and the output decide.
    assert_eq!(
        evidence.exit.code(),
        Some(0),
        "the payload must exit with status 0, got {:?}",
        evidence.exit
    );
    assert_eq!(
        evidence.stdout,
        b"artifree-execution-payload: ran\n".to_vec(),
        "the payload must print exactly its line"
    );

    let observed = evidence.readiness;
    let status = observed.status;
    assert_eq!(
        status.no_new_privs, 1,
        "NoNewPrivs must be set in the bootstrap"
    );
    assert_eq!(
        status.seccomp, 2,
        "the bootstrap must run in seccomp filter mode"
    );
    assert_eq!(
        status.seccomp_filters, 2,
        "exactly two seccomp programs must be installed"
    );
    assert_eq!(
        observed.scratch.size_kib,
        Some(131_072),
        "the scratch must be a tmpfs of 131072 KiB"
    );
    assert_eq!(
        observed.scratch.inodes,
        Some(4_096),
        "the scratch must hold 4096 inodes"
    );
    let capability_sets = [
        ("CapInh", status.cap_inh),
        ("CapPrm", status.cap_prm),
        ("CapEff", status.cap_eff),
        ("CapBnd", status.cap_bnd),
        ("CapAmb", status.cap_amb),
    ];
    for (set, mask) in capability_sets {
        assert_eq!(mask, 0, "{set} must be empty in the bootstrap");
    }

    let limits = observed.limits;
    assert_eq!(
        (limits.open_files.soft, limits.open_files.hard),
        (Some(256), Some(256)),
        "the bootstrap must read back 256 open files, soft and hard"
    );
    assert_eq!(
        (limits.core_bytes.soft, limits.core_bytes.hard),
        (Some(0), Some(0)),
        "the bootstrap must read back a core size of 0, soft and hard"
    );
    assert!(
        observed.members >= 2,
        "the attempt group must hold the bootstrap and its launcher, got {} members",
        observed.members
    );
    assert_eq!(
        evidence.limits,
        vec![
            ("memory.max", "1073741824".to_owned()),
            ("memory.swap.max", "0".to_owned()),
            ("memory.oom.group", "1".to_owned()),
            ("cpu.max", "100000 100000".to_owned()),
            ("pids.max", "64".to_owned()),
        ],
        "the attempt group must read back the five fixture limits"
    );

    assert!(
        !evidence.group.exists(),
        "the attempt group {} must be removed",
        evidence.group.display()
    );
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_false_ready_is_never_released() -> Result<(), Box<dyn Error>> {
    let plan = AttemptPlan {
        stub_arguments: &["false-ready"],
        ..AttemptPlan::fixture()
    };
    assert_not_started(
        "false-ready",
        &plan,
        NotStartedReason::ReadinessNotConfirmed,
    )
}

/// The release gate of the real bootstrap (0056 rule 3): after a readiness that the provider
/// confirmed, a byte other than RELEASE starts no payload. The bootstrap ends with its failure
/// status and the payload's line never appears.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_wrong_release_byte_starts_no_payload() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("wrong-release")?;
    let plan = AttemptPlan {
        release_record: b"Z",
        ..AttemptPlan::fixture()
    };
    let evidence = launch(&plan, dir.path())
        .map_err(|refusal| format!("the attempt did not reach its exit: {:?}", refusal.failure))?;
    assert_eq!(
        evidence.exit.code(),
        Some(70),
        "a bootstrap that was not released must end with its failure status, got {:?}",
        evidence.exit
    );
    assert!(
        evidence.stdout.is_empty(),
        "no payload may run without the release, but standard output holds {} bytes",
        evidence.stdout.len()
    );
    assert_no_attempt_groups()
}

/// The stub takes the fixture profile's identity and nothing else (0056 rule 3): another
/// identity starts no launcher. The stub then exits at once, so the provider either cannot
/// place it in the group any more or places it and never reads a readiness record. Both count
/// as not started; which one it is depends on timing and is not asserted.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_unknown_identity_starts_no_launcher() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("unknown-identity")?;
    let plan = AttemptPlan {
        stub_arguments: &["stub", "another-profile"],
        ..AttemptPlan::fixture()
    };
    let failure = refusal_of(launch(&plan, dir.path()))?;
    assert!(
        matches!(
            failure,
            AttemptFailure::NotStarted(
                NotStartedReason::StubNotPlaced | NotStartedReason::NoReadiness
            )
        ),
        "a stub with another identity must count as not started, got {failure:?}"
    );
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_stalled_start_is_killed_at_the_readiness_deadline() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("stalled-start")?;
    let plan = AttemptPlan {
        stub_arguments: &["wait"],
        readiness_deadline: Duration::from_secs(2),
        ..AttemptPlan::fixture()
    };
    let started = Instant::now();
    let outcome = launch(&plan, dir.path());
    let elapsed = started.elapsed();
    let failure = refusal_of(outcome)?;
    assert_eq!(
        failure,
        AttemptFailure::NotStarted(NotStartedReason::NoReadiness),
        "a stalled start must count as no readiness"
    );
    assert!(
        elapsed >= Duration::from_secs(2),
        "the readiness deadline is 2 s, but the attempt ended after {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "the stalled start must be killed at its deadline, but the attempt took {elapsed:?}"
    );
    assert_no_attempt_groups()
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_payload_exec_failure_counts_as_not_started() -> Result<(), Box<dyn Error>> {
    // run.sh replaces the payload by a text file around this test, so only the exec can fail.
    assert_not_started(
        "exec-failure",
        &AttemptPlan::fixture(),
        NotStartedReason::ExecFailed,
    )
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_second_attempt_is_refused_while_the_lock_is_held() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("lock-held")?;
    // Declared after `dir`, so that the lock is released before the directory is removed.
    let _held = AdmissionLock::acquire(dir.path())
        .map_err(|refusal| format!("the test could not take the lock: {refusal:?}"))?;
    let failure = refusal_of(launch(&AttemptPlan::fixture(), dir.path()))?;
    // Admission has not run yet, so the delegated group is not checked here, only the answer.
    assert_eq!(
        failure,
        AttemptFailure::Busy,
        "a second attempt must be refused while the lock is held"
    );
    Ok(())
}
