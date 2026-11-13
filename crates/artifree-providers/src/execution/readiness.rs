//! What proves an attempt ready: the bootstrap's own final check and the provider's
//! independent observation read the same proc text with the same functions (decisions
//! 0042 rule 6, 0056 rules 3 and 5).
//!
//! The checks cover the bootstrap's status, its limits, its seccomp filter count and its mount
//! table. The mount table holds the scratch tmpfs with its bounds, the read-only root and `/dev`
//! (the amendment of 2026-11-12, rule 3), the empty read-only tmpfs over `/dev/pts` (the
//! amendment of 2026-11-13, rule 3 as reworded), and the staged input at `/input`, read-only with
//! `nosuid`, `nodev` and `noexec`, on a tree with no propagation tag (the amendment of
//! 2026-11-13, input and output). The bootstrap reads its own table back, and the provider reads
//! the same table of the bootstrap's process independently. `confirm_member`, which exists only in
//! test builds, is the keeper's re-check that the bootstrap is still a member of the group in
//! filter mode.

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
/// The mount point of the launcher's terminals, which the bootstrap covers with an empty
/// read-only tmpfs (the amendment of 2026-11-13, rule 3 as reworded).
const DEV_PTS_MOUNT: &str = "/dev/pts";
/// The mount point of the staged input, bound read-only by the launcher and remounted with
/// `nosuid`, `nodev` and `noexec` by the bootstrap (0056 rules 2 and 3, as reworded by the
/// amendment of 2026-11-13, input and output).
pub(super) const INPUT_MOUNT: &str = "/input";
/// The size bound of the launcher's empty tmpfs over `/dev/pts` in the kernel's `size=<n>k` form:
/// one page, as the attempt's arguments set it with `--size 4096` (0056 rule 2, as reworded by the
/// amendment of 2026-11-13, the launcher's `/dev/pts`). Bubblewrap cannot bound its inodes, and a
/// read-only mount needs no inode bound.
const TERMINALS_SIZE_KIB: u64 = 4;
/// The permission bits of that tmpfs: read and search only, as the arguments set them with
/// `--perms 0555`.
const TERMINALS_MODE: u32 = 0o555;

// Field names of the mount checks. `mountinfo` names a failure of the text itself.
const FIELD_SCRATCH: &str = "scratch";
const FIELD_ROOT: &str = "root";
const FIELD_DEV: &str = "dev";
const FIELD_DEV_PTS: &str = "dev_pts";
const FIELD_INPUT: &str = "input";
const FIELD_PROPAGATION: &str = "propagation";
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
/// The payload is the `/proc` key or mount name concerned, so a refusal names the field and never
/// a value.
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
    /// `/dev/pts` has no line, or its last line is not the launcher's empty read-only tmpfs with
    /// its bounds: the cover is missing (0056 rules 2 and 3, as reworded by the amendments of
    /// 2026-11-13).
    DevPts,
    /// `/input` has no line, or its last line lacks `ro`, `nosuid`, `nodev` or `noexec` (0056 rule
    /// 3 as reworded by the amendment of 2026-11-13, input and output; 0042 rule 4).
    Input,
    /// A mount line carries a propagation tag (an optional field such as `shared:`, `master:`,
    /// `propagate_from:` or `unbindable`), so the bootstrap's tree is not private (the same
    /// amendment).
    Propagation,
}

impl ReadinessFailure {
    /// The `/proc` key or mount name whose check failed.
    #[must_use]
    pub(super) fn field(self) -> &'static str {
        match self {
            Self::Missing(key)
            | Self::Repeated(key)
            | Self::Malformed(key)
            | Self::Unexpected(key) => key,
            Self::DevPts => FIELD_DEV_PTS,
            Self::Input => FIELD_INPUT,
            Self::Propagation => FIELD_PROPAGATION,
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

/// The facts that the mount checks read from one `mountinfo` line. The filesystem and bound
/// fields are read for the scratch mount; for the root and `/dev`, only `read_only` is read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ScratchMount {
    /// The mount id, the line's first field. The keeper compares it with the mount id that
    /// `statx` gives for its descriptor (0056, amendment of 2026-11-13, input and output).
    pub(super) mount_id: u64,
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

/// The facts that the mount checks read from the last `/dev/pts` line: the filesystem, the
/// read-only flag, and the size and mode bounds of the launcher's cover.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PtsMount {
    /// Whether the filesystem type is `tmpfs`.
    pub(super) is_tmpfs: bool,
    /// Whether the mount options include `ro`.
    pub(super) read_only: bool,
    /// The `size=` bound in KiB from the super options, or `None` when it has another form.
    pub(super) size_kib: Option<u64>,
    /// The `mode=` permission bits from the super options, read as octal, or `None` when they
    /// are absent or not octal.
    pub(super) mode: Option<u32>,
}

/// The mount flags of the last `/input` line, which the checks require all set (0042 rule 4; 0056
/// rule 3, as reworded by the amendment of 2026-11-13, input and output).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the four mount flags that 0042 rule 4 and the amendment of 2026-11-13 require on /input, each checked by name"
)]
pub(super) struct InputMount {
    /// Whether the mount options include `ro`.
    pub(super) read_only: bool,
    /// Whether the mount options include `nosuid`.
    pub(super) nosuid: bool,
    /// Whether the mount options include `nodev`.
    pub(super) nodev: bool,
    /// Whether the mount options include `noexec`.
    pub(super) noexec: bool,
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
    /// The last `/dev/pts` line's facts, or `None` when no line has its mount point. With stacked
    /// mounts the cover is the later line, over the launcher's devpts, so the later one decides.
    pub(super) dev_pts: Option<PtsMount>,
    /// The last `/input` line's flags, or `None` when no line has its mount point.
    pub(super) input: Option<InputMount>,
    /// Whether any line carries a propagation tag.
    pub(super) propagation_tagged: bool,
}

/// One `mountinfo` line: its mount point, its mount options, whether it carries a propagation
/// tag, and the facts that the checks read from it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MountLine<'a> {
    mount_point: &'a str,
    /// The mount options, which are the line's sixth field.
    options: &'a str,
    /// Whether an optional field before the lone `-` names a propagation tag.
    tagged: bool,
    facts: ScratchMount,
}

/// Reads the mount points that the checks need from `/proc/<pid>/mountinfo` text.
///
/// Each line has the mount id in its first field, the mount point in its fifth and the mount
/// options in its sixth. Optional fields follow, up to a lone `-`; any of them is a propagation
/// tag (`shared:`, `master:`, `propagate_from:` or `unbindable`), and one sets
/// `propagation_tagged`. Then come the filesystem type, the source and the super options. A later
/// line covers an earlier one with the same mount point, so the last line for each mount point
/// decides. Empty lines are skipped.
///
/// # Errors
/// Returns `Malformed` for the `mountinfo` field when the text fills `PROC_FILE_BYTES`, or when
/// any line lacks the fields above or has a mount id that is not decimal. A mount point without a
/// line is `None`, not an error.
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
        dev_pts: last_mount(&lines, DEV_PTS_MOUNT).map(|line| PtsMount {
            is_tmpfs: line.facts.is_tmpfs,
            read_only: line.facts.read_only,
            size_kib: line.facts.size_kib,
            mode: line.facts.mode,
        }),
        input: last_mount(&lines, INPUT_MOUNT).map(|line| input_mount(line.options)),
        propagation_tagged: lines.iter().any(|line| line.tagged),
    })
}

/// Checks the mount table that the bootstrap set up (the amendment of 2026-11-12, rules 2 and
/// 3): a scratch tmpfs with `nosuid`, `nodev` and `noexec` (decision 0042 rule 4) that is not
/// read-only, owner-only, with the size and inode bounds of decision 0042 rule 5, a read-only
/// root and `/dev`, and `/dev/pts` covered by the launcher's empty read-only tmpfs of 4 KiB with
/// mode 555 (rules 2 and 3, as reworded by the amendments of 2026-11-13). It also requires `/input`
/// to be read-only with `nosuid`, `nodev` and `noexec`, and no line to carry a propagation tag
/// (0056 rule 3, as reworded by the amendment of 2026-11-13, input and output: "its readback, and
/// the provider's read of the mount table, require these flags and no propagation tag").
///
/// # Errors
/// Returns `Missing` for the scratch, root or `/dev` without a line, and `Unexpected` for the first
/// of them that differs. `DevPts` refuses `/dev/pts` when its last line is not the launcher's empty
/// read-only tmpfs with those bounds. `Input` refuses `/input` without a line, or whose last line
/// lacks one of the four flags, and `Propagation` refuses any line with a propagation tag. The
/// checks run in the order scratch, root, `/dev`, `/dev/pts`, `/input`, propagation.
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
    confirm_read_only(facts.dev_read_only, FIELD_DEV)?;
    let terminals_as_required = facts.dev_pts.is_some_and(|pts| {
        pts.is_tmpfs
            && pts.read_only
            && pts.size_kib == Some(TERMINALS_SIZE_KIB)
            && pts.mode == Some(TERMINALS_MODE)
    });
    if !terminals_as_required {
        return Err(ReadinessFailure::DevPts);
    }
    let input_as_required = facts
        .input
        .is_some_and(|input| input.read_only && input.nosuid && input.nodev && input.noexec);
    if !input_as_required {
        return Err(ReadinessFailure::Input);
    }
    if facts.propagation_tagged {
        return Err(ReadinessFailure::Propagation);
    }
    Ok(())
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

/// Reads one `mountinfo` line into its mount point, its options, its propagation tag and its facts.
///
/// # Errors
/// Returns `Malformed` for the `mountinfo` field when the line has no mount id, mount point or
/// options, a mount id that is not decimal, no lone `-` after the options, or not exactly a
/// filesystem type, a source and super options after it.
fn mount_line(line: &str) -> Result<MountLine<'_>, ReadinessFailure> {
    let malformed = ReadinessFailure::Malformed(FIELD_MOUNTINFO);
    let fields: Vec<&str> = line.split_whitespace().collect();
    let Some(&id) = fields.first() else {
        return Err(malformed);
    };
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

    let tagged = optional
        .get(..separator)
        .is_some_and(|tags| !tags.is_empty());
    let facts = ScratchMount {
        mount_id: decimal_value(id, FIELD_MOUNTINFO)?,
        is_tmpfs: fs_type == "tmpfs",
        read_only: has_option(options, "ro"),
        no_suid_dev_exec: has_option(options, "nosuid")
            && has_option(options, "nodev")
            && has_option(options, "noexec"),
        size_kib: last_option(super_options, "size=")
            .and_then(|value| value.strip_suffix('k'))
            .and_then(|digits| decimal_value(digits, FIELD_MOUNTINFO).ok()),
        inodes: last_option(super_options, "nr_inodes=")
            .and_then(|value| decimal_value(value, FIELD_MOUNTINFO).ok()),
        mode: last_option(super_options, "mode=")
            .and_then(|digits| u32::from_str_radix(digits, 8).ok()),
    };
    Ok(MountLine {
        mount_point,
        options,
        tagged,
        facts,
    })
}

/// Whether the comma-separated mount `options` include `token` as a whole option.
fn has_option(options: &str, token: &str) -> bool {
    options.split(',').any(|option| option == token)
}

/// The flags that the checks read from an `/input` line's mount options, each as a whole option.
fn input_mount(options: &str) -> InputMount {
    InputMount {
        read_only: has_option(options, "ro"),
        nosuid: has_option(options, "nosuid"),
        nodev: has_option(options, "nodev"),
        noexec: has_option(options, "noexec"),
    }
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

/// Reads the attempt group's members again and requires `pid` among them, still in seccomp filter
/// mode: the keeper's re-check after it opened the scratch (0056, amendment of 2026-11-13, input
/// and output). A bootstrap that died, or whose pid was reused, fails it, since nothing outside the
/// attempt can join its group.
///
/// # Errors
/// Returns `Unreadable` for a listing or status that cannot be read or a listing line that is not
/// a pid, `Membership` when `pid` is not listed or not in filter mode, and `Field` for a malformed
/// status.
#[cfg(test)]
pub(super) fn confirm_member(
    group: &Path,
    proc_root: &Path,
    pid: u32,
) -> Result<(), ObservationFailure> {
    let listing = read_proc_text(&group.join("cgroup.procs"))?;
    let mut listed = false;
    for line in listing.lines() {
        let member: u32 = line.parse().map_err(|_| ObservationFailure::Unreadable)?;
        listed |= member == pid;
    }
    if !listed {
        return Err(ObservationFailure::Membership);
    }
    let text = read_proc_text(&member_file(proc_root, pid, "status"))?;
    let status = parse_status(&text).map_err(ObservationFailure::Field)?;
    if status.seccomp != SECCOMP_MODE_FILTER {
        return Err(ObservationFailure::Membership);
    }
    Ok(())
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
