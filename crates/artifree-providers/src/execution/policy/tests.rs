//! Behavioral tests for the fixture seccomp policy and for the readiness checks that the
//! bootstrap runs on itself and the provider runs on the attempt group (decisions 0042 rules 4
//! and 6, 0056 rules 3 and 5).

#[cfg(target_arch = "x86_64")]
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use super::super::readiness::{
    LimitFacts, LimitPair, ObservationFailure, ReadinessFailure, StatusFacts, confirm_limits,
    confirm_status, observe_group, parse_limits, parse_status,
};
#[cfg(target_arch = "x86_64")]
use super::{
    ALLOWED, CLONE, FilterSet, PolicyError, QUERY_ONLY, SETSOCKOPT, SOCKET, allowlist,
    allowlist_rules, compile, fixture_filters, policy_sha256,
};

/// The measured bootstrap's `/proc/<pid>/status`, as the kernel printed it in the probe (0056
/// rule 5). Every value that the checks read is here.
const MEASURED_STATUS: &[&str] = &[
    "Name:\tartifree-execution-fixture",
    "Umask:\t0022",
    "State:\tS (sleeping)",
    "Tgid:\t4120",
    "Ngid:\t0",
    "Pid:\t4120",
    "PPid:\t4100",
    "TracerPid:\t0",
    "Uid:\t1000\t1000\t1000\t1000",
    "Gid:\t1000\t1000\t1000\t1000",
    "FDSize:\t64",
    "Groups:\t1000",
    "NStgid:\t4120",
    "NSpid:\t4120",
    "NSpgid:\t4120",
    "NSsid:\t4100",
    "Threads:\t1",
    "NoNewPrivs:\t1",
    "Seccomp:\t2",
    "Seccomp_filters:\t2",
    "CapInh:\t0000000000000000",
    "CapPrm:\t0000000000000000",
    "CapEff:\t0000000000000000",
    "CapBnd:\t0000000000000000",
    "CapAmb:\t0000000000000000",
];

/// The measured bootstrap's `/proc/<pid>/limits`, with the kernel's columns (0042 rule 5).
const MEASURED_LIMITS: &[&str] = &[
    "Limit                     Soft Limit           Hard Limit           Units",
    "Max cpu time              unlimited            unlimited            seconds",
    "Max stack size            8388608              unlimited            bytes",
    "Max core file size        0                    0                    bytes",
    "Max open files            256                  256                  files",
    "Max locked memory         67108864             67108864             bytes",
    "Max file locks            unlimited            unlimited            locks",
];

/// The measured bootstrap's `/proc/<pid>/mountinfo` lines (the amendment of 2026-11-12). The
/// mount points, options and filesystems are the kernel's values from the runner; the ids and the
/// root of each line are illustrative. The last line is the launcher's empty read-only tmpfs over
/// `/dev/pts` (0056 rule 2, as reworded on 2026-11-13), which the `/dev/pts` mount check requires.
/// The `/input` line is the staged input after the bootstrap's remount (0056 rule 3, as reworded
/// by the amendment of 2026-11-13, input and output), which the mount check requires too.
const MEASURED_MOUNTINFO: &[&str] = &[
    "1290 1201 0:110 / / ro,nosuid,nodev,relatime - tmpfs tmpfs rw,uid=1001,gid=1001,inode64",
    "1291 1290 8:1 /opt/artifree/tools/0.1.0 /tools ro,nosuid,nodev,relatime - ext4 /dev/root rw,discard",
    "1292 1290 8:1 /opt/artifree/tools/0.1.0/lib /lib ro,nosuid,nodev,relatime - ext4 /dev/root rw,discard",
    "1293 1290 8:1 /opt/artifree/tools/0.1.0/lib64 /lib64 ro,nosuid,nodev,relatime - ext4 /dev/root rw,discard",
    "1294 1290 0:111 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw",
    "1295 1290 0:112 / /dev ro,nosuid,nodev,relatime - tmpfs tmpfs rw,mode=755,uid=1001,gid=1001,inode64",
    "1296 1295 0:113 / /dev/pts rw,nosuid,noexec,relatime - devpts devpts rw,mode=620,ptmxmode=666",
    "1297 1290 8:1 /home/runner/execution/input /input ro,nosuid,nodev,noexec,relatime - ext4 /dev/root rw,discard",
    "1299 1290 0:115 / /tmp rw,nosuid,nodev,noexec,relatime - tmpfs tmpfs rw,size=131072k,nr_inodes=4096,mode=700,uid=1001,gid=1001,inode64",
    "1300 1296 0:116 / /dev/pts ro,nosuid,nodev,relatime - tmpfs tmpfs rw,size=4k,mode=555,uid=1001,gid=1001,inode64",
];

/// The limits line keys, as the kernel prints them at the start of a line.
const OPEN_FILES: &str = "Max open files";
const CORE_FILE_SIZE: &str = "Max core file size";

/// The measured filter table: the unconditional entries and `clone3`, typed out from the probe's
/// facts so that the test does not read the table back and agree with it by construction.
#[cfg(target_arch = "x86_64")]
const MEASURED: [(&str, i64); 48] = [
    ("read", 0),
    ("write", 1),
    ("close", 3),
    ("fstat", 5),
    ("poll", 7),
    ("mmap", 9),
    ("mprotect", 10),
    ("munmap", 11),
    ("brk", 12),
    ("rt_sigaction", 13),
    ("rt_sigprocmask", 14),
    ("rt_sigreturn", 15),
    ("pread64", 17),
    ("access", 21),
    ("madvise", 28),
    // Local networking (0056, amendment of 2026-11-13): typed out from the amendment, not measured.
    ("connect", 42),
    ("sendto", 44),
    ("recvfrom", 45),
    ("shutdown", 48),
    ("bind", 49),
    ("listen", 50),
    ("getsockname", 51),
    ("execve", 59),
    ("exit", 60),
    ("wait4", 61),
    ("kill", 62),
    ("fcntl", 72),
    ("mkdir", 83),
    ("unlink", 87),
    ("setsid", 112),
    ("sigaltstack", 131),
    ("arch_prctl", 158),
    ("gettid", 186),
    ("futex", 202),
    ("sched_getaffinity", 204),
    ("getdents64", 217),
    ("set_tid_address", 218),
    // Not measured: the vDSO answered it on the runner. Kept for hosts where it does not.
    ("clock_gettime", 228),
    ("clock_nanosleep", 230),
    ("exit_group", 231),
    ("openat", 257),
    ("newfstatat", 262),
    ("set_robust_list", 273),
    // Local networking, as above.
    ("accept4", 288),
    ("getrandom", 318),
    ("statx", 332),
    ("rseq", 334),
    ("clone3", 435),
];

/// Calls that the filter must never allow, with their `x86_64` numbers. `clone` and `socket` are
/// not here because the allowlist admits them under conditions (see `allowlist_rules`).
#[cfg(target_arch = "x86_64")]
const DANGEROUS: [(&str, i64); 7] = [
    ("setrlimit", 160),
    ("ptrace", 101),
    ("unshare", 272),
    ("setns", 308),
    ("mount", 165),
    ("bpf", 321),
    ("sync", 162),
];

#[cfg(target_arch = "x86_64")]
#[test]
fn table_is_sorted_unique_and_complete() {
    let numbers: Vec<i64> = ALLOWED.iter().map(|&(_, number)| number).collect();
    assert!(
        numbers
            .windows(2)
            .all(|pair| matches!(pair, [earlier, later] if earlier < later)),
        "the table numbers must strictly increase: {numbers:?}"
    );
    let names: BTreeSet<&str> = ALLOWED.iter().map(|&(name, _)| name).collect();
    assert_eq!(
        names.len(),
        ALLOWED.len(),
        "every table name must be unique"
    );
    // The array type fixes the length at 48; equality with the measured entries pins them.
    assert_eq!(
        ALLOWED, MEASURED,
        "the table must hold exactly the measured entries"
    );
    assert_eq!(QUERY_ONLY, ("prlimit64", 302));
    assert!(
        !ALLOWED.contains(&QUERY_ONLY),
        "prlimit64 must not be allowed unconditionally"
    );
    assert!(
        !ALLOWED.contains(&CLONE),
        "clone must not be allowed unconditionally"
    );
}

#[cfg(target_arch = "x86_64")]
#[test]
fn dangerous_calls_are_absent() {
    for (name, number) in DANGEROUS {
        let listed = ALLOWED
            .iter()
            .any(|&(allowed, allowed_number)| allowed == name || allowed_number == number);
        assert!(!listed, "{name} ({number}) must not be in the allowlist");
        assert!(
            QUERY_ONLY.0 != name && QUERY_ONLY.1 != number,
            "{name} ({number}) must not be the query-only call"
        );
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn filter_compiles_and_has_a_stable_digest() -> Result<(), PolicyError> {
    let programs = fixture_filters()?;
    for program in &programs {
        assert!(
            !program.is_empty(),
            "each fixture filter must compile to instructions"
        );
    }
    let digest = policy_sha256(FilterSet::Fixture)?;
    assert_eq!(
        digest.len(),
        64,
        "a SHA-256 digest has 64 hex characters: {digest}"
    );
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "the digest must be lowercase hex: {digest}"
    );
    assert_eq!(
        policy_sha256(FilterSet::Fixture)?,
        digest,
        "the digest must not change between two builds of one filter"
    );
    Ok(())
}

#[cfg(target_arch = "x86_64")]
#[test]
fn prlimit64_is_allowed_only_as_a_query() -> Result<(), PolicyError> {
    // The allowlist's rules with prlimit64 allowed for every argument: it must differ from the
    // fixture allowlist, which allows only the query.
    let mut unconditional = allowlist_rules()?;
    unconditional.insert(QUERY_ONLY.1, Vec::new());
    let always_allowed = compile(unconditional)?;
    assert_ne!(
        allowlist()?,
        always_allowed,
        "prlimit64 must be allowed only when its new-limit argument is null"
    );
    Ok(())
}

/// Compares the table with the kernel's own numbers. Only the Linux enforcement job runs it: it
/// reads the kernel headers of the runner, which a developer machine may not have.
#[cfg(target_arch = "x86_64")]
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_syscall_table_matches_the_kernel_headers() -> io::Result<()> {
    const HEADER: &str = "/usr/include/x86_64-linux-gnu/asm/unistd_64.h";
    let text = fs::read_to_string(HEADER)?;
    let mut kernel: BTreeMap<String, i64> = BTreeMap::new();
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if let ["#define", key, value] = tokens.as_slice()
            && let Some(name) = key.strip_prefix("__NR_")
            && let Ok(number) = value.parse::<i64>()
        {
            kernel.insert(name.to_owned(), number);
        }
    }
    // Every name is compared, so one missing or wrong number is reported with the others.
    let expected: Vec<(&str, i64)> = ALLOWED
        .iter()
        .copied()
        .chain([QUERY_ONLY, CLONE, SOCKET, SETSOCKOPT])
        .chain(DANGEROUS)
        .collect();
    let mut mismatches: Vec<String> = Vec::new();
    for (name, number) in expected {
        let kernel_number = kernel.get(name);
        if kernel_number != Some(&number) {
            mismatches.push(format!("{name}: table {number}, headers {kernel_number:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "the table disagrees with {HEADER}: {mismatches:?}"
    );
    Ok(())
}

/// The measured status as owned lines, so that a row can change one of them.
fn measured_status_lines() -> Vec<String> {
    MEASURED_STATUS.iter().copied().map(str::to_owned).collect()
}

/// The measured limits as owned lines, so that a row can change one of them.
fn measured_limit_lines() -> Vec<String> {
    MEASURED_LIMITS.iter().copied().map(str::to_owned).collect()
}

/// The measured mount table as owned lines, so that a row can change one of them.
fn measured_mountinfo_lines() -> Vec<String> {
    MEASURED_MOUNTINFO
        .iter()
        .copied()
        .map(str::to_owned)
        .collect()
}

/// Joins lines into a text in which every line ends with a newline; no lines give no text.
fn join_lines(lines: &[String]) -> String {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// The key of a status line: the text before its first colon.
fn status_key(line: &str) -> Option<&str> {
    line.split_once(':').map(|(key, _)| key)
}

/// Sets the value of each status line whose key is exactly `key`.
fn set_status(lines: &mut [String], key: &str, value: &str) {
    for line in lines {
        if status_key(line) == Some(key) {
            *line = format!("{key}:\t{value}");
        }
    }
}

/// Removes each status line whose key is exactly `key`.
fn remove_status(lines: &mut Vec<String>, key: &str) {
    lines.retain(|line| status_key(line) != Some(key));
}

/// Repeats each status line whose key is exactly `key`.
fn repeat_status(lines: &mut Vec<String>, key: &str) {
    let copies: Vec<String> = lines
        .iter()
        .filter(|line| status_key(line) == Some(key))
        .cloned()
        .collect();
    lines.extend(copies);
}

/// A limits line with the kernel's column widths.
fn limit_line(key: &str, soft: &str, hard: &str, unit: &str) -> String {
    format!("{key:<26}{soft:<21}{hard:<21}{unit}")
}

/// Replaces each limits line that starts with `key`.
fn replace_limit(lines: &mut [String], key: &str, replacement: &str) {
    for line in lines {
        if line.starts_with(key) {
            replacement.clone_into(line);
        }
    }
}

/// Removes each limits line that starts with `key`.
fn remove_limit(lines: &mut Vec<String>, key: &str) {
    lines.retain(|line| !line.starts_with(key));
}

/// Repeats each limits line that starts with `key`.
fn repeat_limit(lines: &mut Vec<String>, key: &str) {
    let copies: Vec<String> = lines
        .iter()
        .filter(|line| line.starts_with(key))
        .cloned()
        .collect();
    lines.extend(copies);
}

/// One edit of a measured text, applied before its checks run.
type TextEdit = fn(&mut Vec<String>);

#[test]
fn each_wrong_status_field_is_refused_by_name() {
    let cases: &[(&str, TextEdit, Result<(), ReadinessFailure>)] = &[
        (
            "NoNewPrivs 0",
            |lines: &mut Vec<String>| set_status(lines, "NoNewPrivs", "0"),
            Err(ReadinessFailure::Unexpected("NoNewPrivs")),
        ),
        (
            "Seccomp 0",
            |lines: &mut Vec<String>| set_status(lines, "Seccomp", "0"),
            Err(ReadinessFailure::Unexpected("Seccomp")),
        ),
        (
            "Seccomp_filters 0",
            |lines: &mut Vec<String>| set_status(lines, "Seccomp_filters", "0"),
            Err(ReadinessFailure::Unexpected("Seccomp_filters")),
        ),
        (
            "CapInh set",
            |lines: &mut Vec<String>| set_status(lines, "CapInh", "0000000000000001"),
            Err(ReadinessFailure::Unexpected("CapInh")),
        ),
        (
            "CapPrm set",
            |lines: &mut Vec<String>| set_status(lines, "CapPrm", "0000000000000001"),
            Err(ReadinessFailure::Unexpected("CapPrm")),
        ),
        (
            "CapEff set",
            |lines: &mut Vec<String>| set_status(lines, "CapEff", "0000000000000001"),
            Err(ReadinessFailure::Unexpected("CapEff")),
        ),
        (
            "CapBnd set",
            |lines: &mut Vec<String>| set_status(lines, "CapBnd", "000001ffffffffff"),
            Err(ReadinessFailure::Unexpected("CapBnd")),
        ),
        (
            "CapAmb set",
            |lines: &mut Vec<String>| set_status(lines, "CapAmb", "0000000000000001"),
            Err(ReadinessFailure::Unexpected("CapAmb")),
        ),
        (
            "Seccomp line missing",
            |lines: &mut Vec<String>| remove_status(lines, "Seccomp"),
            Err(ReadinessFailure::Missing("Seccomp")),
        ),
        (
            "NoNewPrivs line repeated",
            |lines: &mut Vec<String>| repeat_status(lines, "NoNewPrivs"),
            Err(ReadinessFailure::Repeated("NoNewPrivs")),
        ),
        (
            "Seccomp value not decimal",
            |lines: &mut Vec<String>| set_status(lines, "Seccomp", "x"),
            Err(ReadinessFailure::Malformed("Seccomp")),
        ),
        (
            "CapEff value not a 16-digit mask",
            |lines: &mut Vec<String>| set_status(lines, "CapEff", "zzzz"),
            Err(ReadinessFailure::Malformed("CapEff")),
        ),
        (
            "Seccomp_filters 1",
            |lines: &mut Vec<String>| set_status(lines, "Seccomp_filters", "1"),
            Err(ReadinessFailure::Unexpected("Seccomp_filters")),
        ),
    ];
    let mut failures: Vec<String> = Vec::new();
    for (name, edit, expected) in cases {
        let mut lines = measured_status_lines();
        edit(&mut lines);
        let outcome = parse_status(&join_lines(&lines)).and_then(|facts| confirm_status(&facts));
        if outcome != *expected {
            failures.push(format!("{name}: expected {expected:?}, got {outcome:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "status rows with a wrong result: {failures:#?}"
    );
}

#[test]
fn each_wrong_limit_is_refused_by_name() {
    let cases: &[(&str, TextEdit, Result<(), ReadinessFailure>)] = &[
        (
            "open files 65536 65536",
            |lines: &mut Vec<String>| {
                replace_limit(
                    lines,
                    OPEN_FILES,
                    &limit_line(OPEN_FILES, "65536", "65536", "files"),
                );
            },
            Err(ReadinessFailure::Unexpected(OPEN_FILES)),
        ),
        (
            "open files soft 256 hard 65536",
            |lines: &mut Vec<String>| {
                replace_limit(
                    lines,
                    OPEN_FILES,
                    &limit_line(OPEN_FILES, "256", "65536", "files"),
                );
            },
            Err(ReadinessFailure::Unexpected(OPEN_FILES)),
        ),
        (
            "core unlimited unlimited",
            |lines: &mut Vec<String>| {
                replace_limit(
                    lines,
                    CORE_FILE_SIZE,
                    &limit_line(CORE_FILE_SIZE, "unlimited", "unlimited", "bytes"),
                );
            },
            Err(ReadinessFailure::Unexpected(CORE_FILE_SIZE)),
        ),
        (
            "core 0 unlimited",
            |lines: &mut Vec<String>| {
                replace_limit(
                    lines,
                    CORE_FILE_SIZE,
                    &limit_line(CORE_FILE_SIZE, "0", "unlimited", "bytes"),
                );
            },
            Err(ReadinessFailure::Unexpected(CORE_FILE_SIZE)),
        ),
        (
            "core line missing",
            |lines: &mut Vec<String>| remove_limit(lines, CORE_FILE_SIZE),
            Err(ReadinessFailure::Missing(CORE_FILE_SIZE)),
        ),
        (
            "open files line repeated",
            |lines: &mut Vec<String>| repeat_limit(lines, OPEN_FILES),
            Err(ReadinessFailure::Repeated(OPEN_FILES)),
        ),
        (
            "open files hard value not a number",
            |lines: &mut Vec<String>| {
                replace_limit(
                    lines,
                    OPEN_FILES,
                    &limit_line(OPEN_FILES, "256", "abc", "files"),
                );
            },
            Err(ReadinessFailure::Malformed(OPEN_FILES)),
        ),
    ];
    let mut failures: Vec<String> = Vec::new();
    for (name, edit, expected) in cases {
        let mut lines = measured_limit_lines();
        edit(&mut lines);
        let outcome = parse_limits(&join_lines(&lines)).and_then(|facts| confirm_limits(&facts));
        if outcome != *expected {
            failures.push(format!("{name}: expected {expected:?}, got {outcome:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "limits rows with a wrong result: {failures:#?}"
    );
}

#[test]
fn status_and_limits_of_the_measured_bootstrap_pass() {
    let status = parse_status(&join_lines(&measured_status_lines()));
    assert_eq!(
        status,
        Ok(StatusFacts {
            no_new_privs: 1,
            seccomp: 2,
            seccomp_filters: 2,
            cap_inh: 0,
            cap_prm: 0,
            cap_eff: 0,
            cap_bnd: 0,
            cap_amb: 0,
        }),
        "the measured status must parse to its values"
    );
    assert_eq!(
        status.and_then(|facts| confirm_status(&facts)),
        Ok(()),
        "the measured status must pass the bootstrap's check"
    );

    let limits = parse_limits(&join_lines(&measured_limit_lines()));
    assert_eq!(
        limits,
        Ok(LimitFacts {
            open_files: LimitPair {
                soft: Some(256),
                hard: Some(256),
            },
            core_bytes: LimitPair {
                soft: Some(0),
                hard: Some(0),
            },
        }),
        "the measured limits must parse to their values"
    );
    assert_eq!(
        limits.and_then(|facts| confirm_limits(&facts)),
        Ok(()),
        "the measured limits must pass the bootstrap's check"
    );
}

/// The namespace of the provider, in the form the kernel gives `ns/pid` links.
const OWN_NAMESPACE: &str = "pid:[4026531836]";
/// The bootstrap's own pid namespace, which differs from the provider's.
const BOOTSTRAP_NAMESPACE: &str = "pid:[4026532001]";
/// The `cgroup` line of a process in the attempt group.
const GROUP_LINE: &str = "0::/unit/attempt-1\n";
/// The `cgroup` line of a process in another group.
const SUPERVISOR_LINE: &str = "0::/unit/supervisor\n";

/// Gives each fake host of this test process its own directory name.
static NEXT_HOST: AtomicU64 = AtomicU64::new(0);

/// A fresh directory that holds a fake `/proc` and a fake cgroup mount, removed when dropped.
struct TempHost {
    root: PathBuf,
}

impl TempHost {
    /// Creates an empty directory whose name no other fake host of this process uses.
    fn new() -> io::Result<Self> {
        let sequence = NEXT_HOST.fetch_add(1, Ordering::Relaxed);
        let pid = process::id();
        let root = env::temp_dir().join(format!("artifree-policy-tests-{pid}-{sequence}"));
        fs::create_dir(&root)?;
        Ok(Self { root })
    }

    fn mount(&self) -> PathBuf {
        self.root.join("mount")
    }

    fn group(&self) -> PathBuf {
        self.mount().join("unit").join("attempt-1")
    }

    fn supervisor(&self) -> PathBuf {
        self.mount().join("unit").join("supervisor")
    }

    fn proc_root(&self) -> PathBuf {
        self.root.join("proc")
    }

    /// Writes the cgroup files and the `/proc` files of `tree` into this host.
    fn populate(&self, tree: &HostTree) -> io::Result<()> {
        for group in [self.group(), self.supervisor()] {
            fs::create_dir_all(&group)?;
            fs::write(group.join("cgroup.controllers"), "cpu memory pids\n")?;
        }
        fs::write(self.group().join("cgroup.procs"), &tree.procs)?;
        for member in &tree.members {
            let dir = self.proc_root().join(member.pid.to_string());
            fs::create_dir_all(dir.join("ns"))?;
            fs::write(dir.join("status"), join_lines(&member.status))?;
            fs::write(dir.join("limits"), join_lines(&member.limits))?;
            fs::write(dir.join("mountinfo"), join_lines(&member.mountinfo))?;
            fs::write(dir.join("cgroup"), &member.cgroup)?;
            symlink(&member.namespace, dir.join("ns").join("pid"))?;
        }
        Ok(())
    }
}

impl Drop for TempHost {
    fn drop(&mut self) {
        // A directory that cannot be removed keeps its unique name, so no other test reads it.
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// One process of a fake host, with the texts that the observation reads.
struct Member {
    pid: u32,
    status: Vec<String>,
    limits: Vec<String>,
    mountinfo: Vec<String>,
    namespace: String,
    cgroup: String,
}

impl Member {
    /// A process that is not in filter mode and shares the provider's namespace.
    fn ordinary(pid: u32) -> Self {
        let mut status = measured_status_lines();
        set_status(&mut status, "NoNewPrivs", "0");
        set_status(&mut status, "Seccomp", "0");
        set_status(&mut status, "Seccomp_filters", "0");
        Self {
            pid,
            status,
            limits: measured_limit_lines(),
            mountinfo: measured_mountinfo_lines(),
            namespace: OWN_NAMESPACE.to_owned(),
            cgroup: GROUP_LINE.to_owned(),
        }
    }

    /// The bootstrap in its measured state, inside its own pid namespace.
    fn bootstrap(pid: u32) -> Self {
        Self {
            pid,
            status: measured_status_lines(),
            limits: measured_limit_lines(),
            mountinfo: measured_mountinfo_lines(),
            namespace: BOOTSTRAP_NAMESPACE.to_owned(),
            cgroup: GROUP_LINE.to_owned(),
        }
    }
}

/// The processes of a fake host and the membership list of the attempt group.
struct HostTree {
    procs: String,
    members: Vec<Member>,
}

/// One change of the baseline host, applied before the observation runs.
type HostEdit = fn(&mut HostTree);

impl HostTree {
    /// The measured host: pids 10 and 11 are ordinary, and pid 12 is the bootstrap.
    fn baseline() -> Self {
        Self {
            procs: "10\n11\n12\n".to_owned(),
            members: vec![
                Member::ordinary(10),
                Member::ordinary(11),
                Member::bootstrap(12),
            ],
        }
    }

    /// Applies `edit` to every member whose pid is `pid`.
    fn edit_member(&mut self, pid: u32, edit: impl Fn(&mut Member)) {
        for member in self.members.iter_mut().filter(|member| member.pid == pid) {
            edit(member);
        }
    }
}

/// A membership list with one process id per line.
fn procs_listing(pids: &[u32]) -> String {
    let lines: Vec<String> = pids.iter().map(u32::to_string).collect();
    join_lines(&lines)
}

/// One observation row: a name, the change that it makes to the measured host, and the result that
/// the observation must return, which is the member count and the bootstrap's pid or a refusal.
type ObservationRow = (
    &'static str,
    HostEdit,
    Result<(usize, u32), ObservationFailure>,
);

/// The observation rows, each one a change of the measured host with its expected result.
fn observation_rows() -> [ObservationRow; 11] {
    [
        ("measured group", |_: &mut HostTree| {}, Ok((3, 12))),
        (
            "bootstrap listed first",
            |tree: &mut HostTree| {
                tree.procs = "12\n10\n11\n".to_owned();
            },
            Ok((3, 12)),
        ),
        (
            "no process in filter mode",
            |tree: &mut HostTree| {
                tree.edit_member(12, |member| set_status(&mut member.status, "Seccomp", "0"));
            },
            Err(ObservationFailure::Membership),
        ),
        (
            "two processes in filter mode",
            |tree: &mut HostTree| {
                tree.edit_member(11, |member| set_status(&mut member.status, "Seccomp", "2"));
            },
            Err(ObservationFailure::Membership),
        ),
        (
            "65 listed processes",
            |tree: &mut HostTree| {
                let pids: Vec<u32> = (1..=65).collect();
                tree.procs = procs_listing(&pids);
            },
            Err(ObservationFailure::Membership),
        ),
        (
            "empty membership list",
            |tree: &mut HostTree| {
                tree.procs = String::new();
            },
            Err(ObservationFailure::Membership),
        ),
        (
            "bootstrap shares the provider's namespace",
            |tree: &mut HostTree| {
                tree.edit_member(12, |member| member.namespace = OWN_NAMESPACE.to_owned());
            },
            Err(ObservationFailure::SameNamespace),
        ),
        (
            "member outside the attempt group",
            |tree: &mut HostTree| {
                tree.edit_member(11, |member| member.cgroup = SUPERVISOR_LINE.to_owned());
            },
            Err(ObservationFailure::OutsideGroup),
        ),
        (
            "bootstrap NoNewPrivs 0",
            |tree: &mut HostTree| {
                tree.edit_member(12, |member| {
                    set_status(&mut member.status, "NoNewPrivs", "0");
                });
            },
            Err(ObservationFailure::Field(ReadinessFailure::Unexpected(
                "NoNewPrivs",
            ))),
        ),
        (
            "bootstrap open files 65536",
            |tree: &mut HostTree| {
                tree.edit_member(12, |member| {
                    replace_limit(
                        &mut member.limits,
                        OPEN_FILES,
                        &limit_line(OPEN_FILES, "65536", "65536", "files"),
                    );
                });
            },
            Err(ObservationFailure::Field(ReadinessFailure::Unexpected(
                OPEN_FILES,
            ))),
        ),
        (
            "bootstrap scratch with 8192 inodes",
            |tree: &mut HostTree| {
                tree.edit_member(12, |member| {
                    for line in &mut member.mountinfo {
                        if line.contains(" /tmp ") {
                            *line = line.replace("nr_inodes=4096", "nr_inodes=8192");
                        }
                    }
                });
            },
            Err(ObservationFailure::Field(ReadinessFailure::Unexpected(
                "scratch",
            ))),
        ),
    ]
}

#[test]
fn observe_group_with_a_fake_proc() -> io::Result<()> {
    let mut failures: Vec<String> = Vec::new();
    for (name, edit, expected) in observation_rows() {
        let host = TempHost::new()?;
        let mut tree = HostTree::baseline();
        edit(&mut tree);
        host.populate(&tree)?;
        let outcome = observe_group(
            &host.group(),
            &host.proc_root(),
            &host.mount(),
            Path::new(OWN_NAMESPACE),
        )
        .map(|observation| (observation.members, observation.bootstrap_pid));
        if outcome != expected {
            failures.push(format!("{name}: expected {expected:?}, got {outcome:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "observation rows with a wrong result: {failures:#?}"
    );
    Ok(())
}
