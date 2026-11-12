//! What proves an attempt ready: the bootstrap's own final check and the provider's
//! independent observation read the same proc text with the same functions (decisions
//! 0042 rule 6, 0056 rules 3 and 5).

#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::{Path, PathBuf};

/// The largest `/proc` status or limits text that a check reads, in bytes.
pub(super) const PROC_FILE_BYTES: usize = 16_384;

/// The seccomp mode that means a filter is installed (`SECCOMP_MODE_FILTER`).
const SECCOMP_MODE_FILTER: u64 = 2;
/// The open-file limit that the bootstrap sets, soft and hard (0042 rule 5), and that the
/// readiness check requires.
pub(super) const OPEN_FILES_LIMIT: u64 = 256;
/// The core dump limit that the bootstrap sets, soft and hard (0042 rule 5), and that the
/// readiness check requires.
pub(super) const CORE_FILE_LIMIT: u64 = 0;

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
/// mode for `Seccomp`, one filter, and empty effective, permitted, inheritable, bounding and
/// ambient sets.
///
/// # Errors
/// Returns `Unexpected` for the first wrong key, in the field order of `StatusFacts`.
pub(super) fn confirm_status(facts: &StatusFacts) -> Result<(), ReadinessFailure> {
    // Every field is read here, so each one stays in use in the non-test build.
    let checks: [(&str, u64, u64); 8] = [
        (NO_NEW_PRIVS, facts.no_new_privs, 1),
        (SECCOMP, facts.seccomp, SECCOMP_MODE_FILTER),
        (SECCOMP_FILTERS, facts.seccomp_filters, 1),
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
    /// The bootstrap's status or limits fail the measured check.
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
}

/// Observes an attempt group before release, independently of the bootstrap's own check
/// (decision 0042 rule 6).
///
/// The bootstrap is the one member in filter mode, found from the kernel's status and never
/// from a name or a list position. Every member's `0::` line must name `group`.
///
/// # Errors
/// Returns the first failed check, in this order: the membership list, every member's status,
/// the bootstrap's limits, its pid namespace, and every member's cgroup.
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
