//! What proves an attempt ready: the bootstrap's own final check and the provider's
//! independent observation read the same proc text with the same functions (decisions
//! 0042 rule 6, 0056 rules 3 and 5).
//!
//! The checks cover the bootstrap's status, its limits, its seccomp filter count and its mount
//! table. The mount table holds the scratch tmpfs with its bounds and the read-only root and
//! `/dev` (the amendment of 2026-11-12, rule 3). The bootstrap reads its own table back, and the
//! provider reads the same table of the bootstrap's process independently.

#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::{Path, PathBuf};

/// The largest `/proc` status, limits or mount table text that a check reads, in bytes.
pub(super) const PROC_FILE_BYTES: usize = 16_384;

/// The seccomp mode that means a filter is installed (`SECCOMP_MODE_FILTER`).
const SECCOMP_MODE_FILTER: u64 = 2;
/// The number of seccomp filters that an attempt installs (decision 0042 rule 5): the clone3
/// refusal, then the allowlist.
pub(super) const SECCOMP_FILTER_COUNT: u64 = 2;
/// The open-file limit that the bootstrap sets, soft and hard (0042 rule 5), and that the
/// readiness check requires.
pub(super) const OPEN_FILES_LIMIT: u64 = 256;
/// The core dump limit that the bootstrap sets, soft and hard (0042 rule 5), and that the
/// readiness check requires.
pub(super) const CORE_FILE_LIMIT: u64 = 0;
/// The mount point of the attempt's scratch, a tmpfs of its own (decision 0042 rule 5).
pub(super) const SCRATCH: &str = "/tmp";
/// The size bound of the scratch tmpfs in bytes: 128 MiB (decision 0042 rule 5).
pub(super) const SCRATCH_SIZE_BYTES: u64 = 134_217_728;
/// The inode bound of the scratch tmpfs: at most 4,096 inodes (decision 0042 rule 5).
pub(super) const SCRATCH_INODES: u64 = 4_096;
/// The permission bits of the scratch's root directory: the payload's own account only (the
/// amendment of 2026-11-12, rule 3).
pub(super) const SCRATCH_MODE: u32 = 0o700;
/// The size bound in the kernel's `size=<n>k` form, which `mountinfo` prints in kibibytes.
const SCRATCH_SIZE_KIB: u64 = SCRATCH_SIZE_BYTES / 1024;
/// The mount point of the sandbox's root, which must be read-only (the amendment of 2026-11-12).
const ROOT_MOUNT: &str = "/";
/// The mount point of the sandbox's device tree, which must be read-only (the amendment of
/// 2026-11-12).
const DEV_MOUNT: &str = "/dev";

// Field names of the mount checks. `mountinfo` names a failure of the text itself.
const FIELD_SCRATCH: &str = "scratch";
const FIELD_ROOT: &str = "root";
const FIELD_DEV: &str = "dev";
const FIELD_MOUNTINFO: &str = "mountinfo";

// Keys of `/proc/<pid>/status`, matched exactly.
const NO_NEW_PRIVS: &str = "NoNewPrivs";
const SECCOMP: &str = "Seccomp";
const SECCOMP_FILTERS: &str = "Seccomp_filters";
const CAP_INH: &str = "CapInh";
const CAP_PRM: &str = "CapPrm";
const CAP_EFF: &str = "CapEff";
const CAP_BND: &str = "CapBnd";
const CAP_AMB: &str = "CapAmb";

// Keys of `/proc/<pid>/limits`, matched as the start of a line.
const OPEN_FILES: &str = "Max open files";
const CORE_FILE_SIZE: &str = "Max core file size";

/// Why proc text does not show the state that a check requires.
///
/// The payload is the `/proc` key concerned, so a refusal names the field and never a value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReadinessFailure {
    /// The key appears on no line.
    Missing(&'static str),
    /// The key appears on more than one line.
    Repeated(&'static str),
    /// The key's value is not in the kernel's format.
    Malformed(&'static str),
    /// The key's value is not the state that the attempt requires.
    Unexpected(&'static str),
}

impl ReadinessFailure {
    /// The `/proc` key whose check failed.
    #[must_use]
    pub(super) fn field(self) -> &'static str {
        match self {
            Self::Missing(key)
            | Self::Repeated(key)
            | Self::Malformed(key)
            | Self::Unexpected(key) => key,
        }
    }
}

/// The bootstrap's measured `/proc/<pid>/status` values; each capability field is a hex mask.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StatusFacts {
    pub(super) no_new_privs: u64,
    pub(super) seccomp: u64,
    pub(super) seccomp_filters: u64,
    pub(super) cap_inh: u64,
    pub(super) cap_prm: u64,
    pub(super) cap_eff: u64,
    pub(super) cap_bnd: u64,
    pub(super) cap_amb: u64,
}

/// Reads the fields of `/proc/<pid>/status` that the attempt checks.
///
/// Each line splits at its first `:`, and only a key equal to a wanted name counts, so
/// `Seccomp` never matches `Seccomp_filters`. Other lines are ignored.
///
/// # Errors
/// Returns `Missing` or `Repeated` for a key that is not on exactly one line, and
/// `Malformed` for a value that is neither decimal nor 16 hexadecimal digits.
pub(super) fn parse_status(text: &str) -> Result<StatusFacts, ReadinessFailure> {
    Ok(StatusFacts {
        no_new_privs: status_decimal(text, NO_NEW_PRIVS)?,
        seccomp: status_decimal(text, SECCOMP)?,
        seccomp_filters: status_decimal(text, SECCOMP_FILTERS)?,
        cap_inh: status_hex(text, CAP_INH)?,
        cap_prm: status_hex(text, CAP_PRM)?,
        cap_eff: status_hex(text, CAP_EFF)?,
        cap_bnd: status_hex(text, CAP_BND)?,
        cap_amb: status_hex(text, CAP_AMB)?,
    })
}

/// Checks the bootstrap's measured status (0042 rule 8, 0056 rule 3): `NoNewPrivs` 1, filter
/// mode for `Seccomp`, two filters (the clone3 refusal and the allowlist), and empty effective,
/// permitted, inheritable, bounding and ambient sets.
///
/// # Errors
/// Returns `Unexpected` for the first wrong key, in the field order of `StatusFacts`.
pub(super) fn confirm_status(facts: &StatusFacts) -> Result<(), ReadinessFailure> {
    // Every field is read here, so each one stays in use in the non-test build.
    let checks: [(&str, u64, u64); 8] = [
        (NO_NEW_PRIVS, facts.no_new_privs, 1),
        (SECCOMP, facts.seccomp, SECCOMP_MODE_FILTER),
        (SECCOMP_FILTERS, facts.seccomp_filters, SECCOMP_FILTER_COUNT),
        (CAP_INH, facts.cap_inh, 0),
        (CAP_PRM, facts.cap_prm, 0),
        (CAP_EFF, facts.cap_eff, 0),
        (CAP_BND, facts.cap_bnd, 0),
        (CAP_AMB, facts.cap_amb, 0),
    ];
    for (key, actual, expected) in checks {
        if actual != expected {
            return Err(ReadinessFailure::Unexpected(key));
        }
    }
    Ok(())
}

/// One `/proc/<pid>/limits` line's soft and hard values; `None` stands for `unlimited`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LimitPair {
    pub(super) soft: Option<u64>,
    pub(super) hard: Option<u64>,
}

/// The two limits that the attempt sets: open files and core dump size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LimitFacts {
    pub(super) open_files: LimitPair,
    pub(super) core_bytes: LimitPair,
}

/// Reads the open-file and core-size lines of `/proc/<pid>/limits`.
///
/// A line counts when its text after the key starts with whitespace. That text holds a soft
/// value, a hard value and a unit. Each value is `unlimited` or decimal.
///
/// # Errors
/// Returns `Missing`, `Repeated` or `Malformed` for the first key that does not have exactly
/// one well-formed line.
pub(super) fn parse_limits(text: &str) -> Result<LimitFacts, ReadinessFailure> {
    Ok(LimitFacts {
        open_files: limit_pair(text, OPEN_FILES)?,
        core_bytes: limit_pair(text, CORE_FILE_SIZE)?,
    })
}

/// Checks the limits that the bootstrap set: 256 open files and a zero core dump size, soft
/// and hard (0042 rule 5).
///
/// # Errors
/// Returns `Unexpected` for the first limit that differs.
pub(super) fn confirm_limits(facts: &LimitFacts) -> Result<(), ReadinessFailure> {
    if !is_exactly(facts.open_files, OPEN_FILES_LIMIT) {
        return Err(ReadinessFailure::Unexpected(OPEN_FILES));
    }
    if !is_exactly(facts.core_bytes, CORE_FILE_LIMIT) {
        return Err(ReadinessFailure::Unexpected(CORE_FILE_SIZE));
    }
    Ok(())
}

/// The facts that the mount checks read from one `mountinfo` line. The scratch fields are read
/// for the scratch mount; for the root and `/dev`, only `read_only` is read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ScratchMount {
    /// Whether the filesystem type is `tmpfs`.
    pub(super) is_tmpfs: bool,
    /// Whether the mount options include `ro`.
    pub(super) read_only: bool,
    /// Whether the mount options include `nosuid`, `nodev` and `noexec` (decision 0042 rule 4).
    pub(super) no_suid_dev_exec: bool,
    /// The `size=` bound in KiB from the super options, or `None` when it has another form.
    pub(super) size_kib: Option<u64>,
    /// The `nr_inodes=` bound from the super options, or `None` when it is not a number.
    pub(super) inodes: Option<u64>,
    /// The `mode=` permission bits from the super options, read as octal, or `None` when they
    /// are absent or not octal.
    pub(super) mode: Option<u32>,
}

/// The facts of the last `mountinfo` line for each mount point that the checks read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MountFacts {
    /// The scratch mount's facts, or `None` when no line has the scratch's mount point.
    pub(super) scratch: Option<ScratchMount>,
    /// Whether the root is read-only, or `None` when no line has its mount point.
    pub(super) root_read_only: Option<bool>,
    /// Whether `/dev` is read-only, or `None` when no line has its mount point.
    pub(super) dev_read_only: Option<bool>,
}

/// One `mountinfo` line: its mount point and the facts that the checks read from it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MountLine<'a> {
    mount_point: &'a str,
    facts: ScratchMount,
}

/// Reads the mount points that the checks need from `/proc/<pid>/mountinfo` text.
///
/// Each line has the mount point in its fifth field and the mount options in its sixth. Optional
/// fields follow, up to a lone `-`, and then the filesystem type, the source and the super
/// options. A later line covers an earlier one with the same mount point, so the last line for
/// each mount point decides. Empty lines are skipped.
///
/// # Errors
/// Returns `Malformed` for the `mountinfo` field when the text fills `PROC_FILE_BYTES`, or when
/// any line lacks the fields above. A mount point without a line is `None`, not an error.
pub(super) fn parse_mounts(text: &str) -> Result<MountFacts, ReadinessFailure> {
    if text.len() >= PROC_FILE_BYTES {
        return Err(ReadinessFailure::Malformed(FIELD_MOUNTINFO));
    }
    let mut lines: Vec<MountLine<'_>> = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        lines.push(mount_line(line)?);
    }
    Ok(MountFacts {
        scratch: last_mount(&lines, SCRATCH).map(|line| line.facts),
        root_read_only: last_mount(&lines, ROOT_MOUNT).map(|line| line.facts.read_only),
        dev_read_only: last_mount(&lines, DEV_MOUNT).map(|line| line.facts.read_only),
    })
}

/// Checks the mount table that the bootstrap set up (the amendment of 2026-11-12, rules 2 and
/// 3): a scratch tmpfs with `nosuid`, `nodev` and `noexec` (decision 0042 rule 4) that is not
/// read-only, owner-only, with the size and inode bounds of decision 0042 rule 5, and a read-only
/// root and `/dev`.
///
/// # Errors
/// Returns `Missing` for a mount point without a line, and `Unexpected` for the first mount that
/// differs, checked in the order scratch, root, `/dev`.
pub(super) fn confirm_mounts(facts: &MountFacts) -> Result<(), ReadinessFailure> {
    let scratch = facts
        .scratch
        .ok_or(ReadinessFailure::Missing(FIELD_SCRATCH))?;
    let scratch_as_required = scratch.is_tmpfs
        && !scratch.read_only
        && scratch.no_suid_dev_exec
        && scratch.size_kib == Some(SCRATCH_SIZE_KIB)
        && scratch.inodes == Some(SCRATCH_INODES)
        && scratch.mode == Some(SCRATCH_MODE);
    if !scratch_as_required {
        return Err(ReadinessFailure::Unexpected(FIELD_SCRATCH));
    }
    confirm_read_only(facts.root_read_only, FIELD_ROOT)?;
    confirm_read_only(facts.dev_read_only, FIELD_DEV)
}

/// Requires a mount point's read-only flag to be set; a missing line and a writable mount are
/// refused under the field's name.
fn confirm_read_only(read_only: Option<bool>, field: &'static str) -> Result<(), ReadinessFailure> {
    match read_only {
        None => Err(ReadinessFailure::Missing(field)),
        Some(false) => Err(ReadinessFailure::Unexpected(field)),
        Some(true) => Ok(()),
    }
}

/// Reads one `mountinfo` line into its mount point and facts.
///
/// # Errors
/// Returns `Malformed` for the `mountinfo` field when the line has no mount point or options, no
/// lone `-` after them, or not exactly a filesystem type, a source and super options after it.
fn mount_line(line: &str) -> Result<MountLine<'_>, ReadinessFailure> {
    let malformed = ReadinessFailure::Malformed(FIELD_MOUNTINFO);
    let fields: Vec<&str> = line.split_whitespace().collect();
    let (Some(&mount_point), Some(&options)) = (fields.get(4), fields.get(5)) else {
        return Err(malformed);
    };
    let optional = fields.get(6..).ok_or(malformed)?;
    let separator = optional
        .iter()
        .position(|field| *field == "-")
        .ok_or(malformed)?;
    let Some(&[fs_type, _source, super_options]) = optional.get(separator + 1..) else {
        return Err(malformed);
    };

    let has_option = |token: &str| options.split(',').any(|option| option == token);
    let facts = ScratchMount {
        is_tmpfs: fs_type == "tmpfs",
        read_only: has_option("ro"),
        no_suid_dev_exec: has_option("nosuid") && has_option("nodev") && has_option("noexec"),
        size_kib: last_option(super_options, "size=")
            .and_then(|value| value.strip_suffix('k'))
            .and_then(|digits| decimal_value(digits, FIELD_MOUNTINFO).ok()),
        inodes: last_option(super_options, "nr_inodes=")
            .and_then(|value| decimal_value(value, FIELD_MOUNTINFO).ok()),
        mode: last_option(super_options, "mode=")
            .and_then(|digits| u32::from_str_radix(digits, 8).ok()),
    };
    Ok(MountLine { mount_point, facts })
}

/// The value after the last comma-separated option that starts with `prefix`, such as `size=`.
/// The last one counts, as a later mount option overrides an earlier one.
fn last_option<'a>(options: &'a str, prefix: &str) -> Option<&'a str> {
    options
        .split(',')
        .filter_map(|option| option.strip_prefix(prefix))
        .next_back()
}

/// The last `mountinfo` line whose mount point is `mount_point`.
fn last_mount<'a>(lines: &[MountLine<'a>], mount_point: &str) -> Option<MountLine<'a>> {
    lines
        .iter()
        .rev()
        .find(|line| line.mount_point == mount_point)
        .copied()
}

/// The trimmed value of the one status line whose key is exactly `key`.
fn status_value<'a>(text: &'a str, key: &'static str) -> Result<&'a str, ReadinessFailure> {
    let values: Vec<&str> = text
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|&(name, _)| name == key)
        .map(|(_, value)| value.trim())
        .collect();
    single_value(&values, key)
}

/// The text after the one limits line that starts with `key` and then whitespace.
fn limit_rest<'a>(text: &'a str, key: &'static str) -> Result<&'a str, ReadinessFailure> {
    let values: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix(key))
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .collect();
    single_value(&values, key)
}

/// The one entry of `values`, which holds one entry per matching line of `key`.
fn single_value<'a>(values: &[&'a str], key: &'static str) -> Result<&'a str, ReadinessFailure> {
    match values {
        [] => Err(ReadinessFailure::Missing(key)),
        [value] => Ok(*value),
        _ => Err(ReadinessFailure::Repeated(key)),
    }
}

fn status_decimal(text: &str, key: &'static str) -> Result<u64, ReadinessFailure> {
    decimal_value(status_value(text, key)?, key)
}

fn status_hex(text: &str, key: &'static str) -> Result<u64, ReadinessFailure> {
    hex_value(status_value(text, key)?, key)
}

/// The soft value, the hard value and the unit of the one limits line for `key`.
fn limit_pair(text: &str, key: &'static str) -> Result<LimitPair, ReadinessFailure> {
    let rest = limit_rest(text, key)?;
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    let [soft, hard, _units] = tokens.as_slice() else {
        return Err(ReadinessFailure::Malformed(key));
    };
    Ok(LimitPair {
        soft: limit_value(soft, key)?,
        hard: limit_value(hard, key)?,
    })
}

/// A limit value: `None` for `unlimited`, otherwise a decimal number.
fn limit_value(token: &str, key: &'static str) -> Result<Option<u64>, ReadinessFailure> {
    if token == "unlimited" {
        return Ok(None);
    }
    decimal_value(token, key).map(Some)
}

/// A non-empty run of ASCII digits that fits in `u64`.
fn decimal_value(value: &str, key: &'static str) -> Result<u64, ReadinessFailure> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ReadinessFailure::Malformed(key));
    }
    let number: u64 = value
        .parse()
        .map_err(|_| ReadinessFailure::Malformed(key))?;
    Ok(number)
}

/// Exactly 16 hexadecimal digits, which is the kernel's format for a capability mask.
fn hex_value(value: &str, key: &'static str) -> Result<u64, ReadinessFailure> {
    if value.len() != 16 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ReadinessFailure::Malformed(key));
    }
    let number: u64 =
        u64::from_str_radix(value, 16).map_err(|_| ReadinessFailure::Malformed(key))?;
    Ok(number)
}

/// Whether both the soft and the hard value equal `value`.
fn is_exactly(pair: LimitPair, value: u64) -> bool {
    pair.soft == Some(value) && pair.hard == Some(value)
}

/// The largest number of processes that an attempt group may hold when it is observed.
#[cfg(test)]
const MAX_GROUP_MEMBERS: usize = 64;

/// Why the provider's independent observation refuses an attempt group.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ObservationFailure {
    /// A group or `/proc` file is missing, larger than its bound or not UTF-8, or a
    /// `cgroup.procs` line is not a process id.
    Unreadable,
    /// The group holds no process, more than `MAX_GROUP_MEMBERS` processes, or not exactly one
    /// process in filter mode.
    Membership,
    /// The bootstrap shares a pid namespace with the provider.
    SameNamespace,
    /// A member's `0::` line does not name the attempt group.
    OutsideGroup,
    /// The bootstrap's status, limits or mount table fails the measured check.
    Field(ReadinessFailure),
}

/// What the provider saw in an attempt group before it released the bootstrap.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GroupObservation {
    /// How many processes the group held.
    pub(super) members: usize,
    /// The process that the kernel reports in filter mode, which is the bootstrap.
    pub(super) bootstrap_pid: u32,
    /// The bootstrap's status, which passed `confirm_status`.
    pub(super) status: StatusFacts,
    /// The bootstrap's limits, which passed `confirm_limits`.
    pub(super) limits: LimitFacts,
    /// The bootstrap's scratch mount, which passed `confirm_mounts`.
    pub(super) scratch: ScratchMount,
}

/// Observes an attempt group before release, independently of the bootstrap's own check
/// (decision 0042 rule 6).
///
/// The bootstrap is the one member in filter mode, found from the kernel's status and never
/// from a name or a list position. Every member's `0::` line must name `group`.
///
/// # Errors
/// Returns the first failed check, in this order: the membership list, every member's status,
/// the bootstrap's limits, its mount table, its pid namespace, and every member's cgroup.
#[cfg(test)]
pub(super) fn observe_group(
    group: &Path,
    proc_root: &Path,
    cgroup_mount: &Path,
    own_pid_namespace: &Path,
) -> Result<GroupObservation, ObservationFailure> {
    let listing = read_proc_text(&group.join("cgroup.procs"))?;
    let mut pids: Vec<u32> = Vec::new();
    for line in listing.lines() {
        let pid: u32 = line.parse().map_err(|_| ObservationFailure::Unreadable)?;
        pids.push(pid);
    }
    if pids.is_empty() || pids.len() > MAX_GROUP_MEMBERS {
        return Err(ObservationFailure::Membership);
    }

    let mut members: Vec<(u32, StatusFacts)> = Vec::with_capacity(pids.len());
    for pid in pids {
        let text = read_proc_text(&member_file(proc_root, pid, "status"))?;
        let status = parse_status(&text).map_err(ObservationFailure::Field)?;
        members.push((pid, status));
    }
    let bootstraps: Vec<(u32, StatusFacts)> = members
        .iter()
        .copied()
        .filter(|(_, status)| status.seccomp == SECCOMP_MODE_FILTER)
        .collect();
    let &[(bootstrap_pid, bootstrap_status)] = bootstraps.as_slice() else {
        return Err(ObservationFailure::Membership);
    };
    confirm_status(&bootstrap_status).map_err(ObservationFailure::Field)?;

    let limits_text = read_proc_text(&member_file(proc_root, bootstrap_pid, "limits"))?;
    let limits = parse_limits(&limits_text).map_err(ObservationFailure::Field)?;
    confirm_limits(&limits).map_err(ObservationFailure::Field)?;

    let mountinfo_text = read_proc_text(&member_file(proc_root, bootstrap_pid, "mountinfo"))?;
    let mounts = parse_mounts(&mountinfo_text).map_err(ObservationFailure::Field)?;
    confirm_mounts(&mounts).map_err(ObservationFailure::Field)?;
    let Some(scratch) = mounts.scratch else {
        return Err(ObservationFailure::Field(ReadinessFailure::Missing(
            FIELD_SCRATCH,
        )));
    };

    let namespace = fs::read_link(member_file(proc_root, bootstrap_pid, "ns/pid"))
        .map_err(|_| ObservationFailure::Unreadable)?;
    if namespace.as_path() == own_pid_namespace {
        return Err(ObservationFailure::SameNamespace);
    }

    for (pid, _) in &members {
        let cgroup_file = member_file(proc_root, *pid, "cgroup");
        let path = super::cgroup::discover_own_group(&cgroup_file, cgroup_mount)
            .map_err(|_| ObservationFailure::OutsideGroup)?;
        if path.as_path() != group {
            return Err(ObservationFailure::OutsideGroup);
        }
    }

    Ok(GroupObservation {
        members: members.len(),
        bootstrap_pid,
        status: bootstrap_status,
        limits,
        scratch,
    })
}

/// The `name` file below one process's directory under `proc_root`.
#[cfg(test)]
fn member_file(proc_root: &Path, pid: u32, name: &str) -> PathBuf {
    proc_root.join(pid.to_string()).join(name)
}

/// Reads one bounded `/proc` text file as UTF-8; any failure is `Unreadable`.
#[cfg(test)]
fn read_proc_text(path: &Path) -> Result<String, ObservationFailure> {
    let limit = u64::try_from(PROC_FILE_BYTES).map_err(|_| ObservationFailure::Unreadable)?;
    let bytes = super::read_bounded(path, limit).map_err(|_| ObservationFailure::Unreadable)?;
    String::from_utf8(bytes).map_err(|_| ObservationFailure::Unreadable)
}

#[cfg(test)]
mod tests;
