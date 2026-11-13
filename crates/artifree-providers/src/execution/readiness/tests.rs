//! Tests of the mount checks in `readiness` (the amendment of 2026-11-12, rules 2 and 3, the
//! amendment of 2026-11-13, rule 3 as reworded, and the amendment of 2026-11-13, input and
//! output): the measured mount table passes, each wrong mount is refused under its field name, the
//! empty read-only tmpfs over `/dev/pts` is required and the later of two `/dev/pts` lines decides,
//! `/input` must be read-only with `nosuid`, `nodev` and `noexec` and its last line decides, any
//! propagation tag is refused, the scratch mount id is read, the keeper's membership re-check
//! requires the bootstrap in filter mode, the last line of a mount point decides, and malformed or
//! oversized text is refused.

use std::{env, fs, io, process};

use super::{
    InputMount, MountFacts, ObservationFailure, PROC_FILE_BYTES, PtsMount, ReadinessFailure,
    SCRATCH_INODES, SCRATCH_SIZE_BYTES, ScratchMount, confirm_member, confirm_mounts, parse_mounts,
};

/// A readiness verdict on one mount table text.
type Verdict = Result<(), ReadinessFailure>;

// The measured bootstrap's `/proc/<pid>/mountinfo` lines (the amendment of 2026-11-12, and the
// staged input's line of the amendment of 2026-11-13, input and output). The ids and the roots of
// the lines are illustrative; the mount points, options and filesystems are the kernel's values
// from the runner, except `noexec` on `/tmp`: the probe mounted the scratch without it, and the
// amended rule 3 adds it.
const ROOT_LINE: &str =
    "1290 1201 0:110 / / ro,nosuid,nodev,relatime - tmpfs tmpfs rw,uid=1001,gid=1001,inode64";
const DEV_LINE: &str = "1295 1290 0:112 / /dev ro,nosuid,nodev,relatime - tmpfs tmpfs rw,mode=755,uid=1001,gid=1001,inode64";
const TMP_LINE: &str = "1299 1290 0:115 / /tmp rw,nosuid,nodev,noexec,relatime - tmpfs tmpfs rw,size=131072k,nr_inodes=4096,mode=700,uid=1001,gid=1001,inode64";
/// The launcher's empty read-only tmpfs over `/dev/pts`, stacked on its devpts (0056, amendment of
/// 2026-11-13, the launcher's `/dev/pts`). Illustrative ids; the options of this line and of the
/// devpts line under it are the kernel's, as the enforcement job recorded them inside an attempt
/// (`canary.dev-pts.mount_in_attempt`, run 43373521166). The read-only flag is a mount option:
/// the super options still say `rw`.
const PTS_LINE: &str = "1300 1296 0:116 / /dev/pts ro,nosuid,nodev,relatime - tmpfs tmpfs rw,size=4k,mode=555,uid=1001,gid=1001,inode64";
/// The staged input's line after the bootstrap's remount (the amendment of 2026-11-13, input and
/// output): a bind of the probe's staging directory, read-only with `nosuid`, `nodev` and `noexec`.
/// Its options, bind source and filesystem are the probe's measured values; its ids are
/// illustrative.
const INPUT_LINE: &str = "1297 1290 252:0 /tmp/artifree-probe-input /input ro,nosuid,nodev,noexec,relatime - ext4 /dev/mapper/cow-root rw,discard";

const MEASURED_MOUNTINFO: [&str; 10] = [
    ROOT_LINE,
    "1291 1290 8:1 /opt/artifree/tools/v1 /tools ro,nosuid,nodev,relatime - ext4 /dev/root rw,discard",
    "1292 1290 8:1 /opt/artifree/tools/v1/lib /lib ro,nosuid,nodev,relatime - ext4 /dev/root rw,discard",
    "1293 1290 8:1 /opt/artifree/tools/v1/lib64 /lib64 ro,nosuid,nodev,relatime - ext4 /dev/root rw,discard",
    "1294 1290 0:111 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw",
    DEV_LINE,
    "1296 1295 0:113 / /dev/pts rw,nosuid,noexec,relatime - devpts devpts rw,mode=620,ptmxmode=666",
    INPUT_LINE,
    TMP_LINE,
    PTS_LINE,
];

/// The scratch facts of the measured table: a writable, owner-only tmpfs with `nosuid`, `nodev`
/// and `noexec`, at 128 MiB
/// (`131072k` in the kernel's form) and 4,096 inodes.
const MEASURED_SCRATCH: ScratchMount = ScratchMount {
    mount_id: 1299,
    is_tmpfs: true,
    read_only: false,
    no_suid_dev_exec: true,
    size_kib: Some(131_072),
    inodes: Some(4_096),
    mode: Some(0o700),
};

/// Joins lines into mountinfo text, with a newline after each line.
fn join_lines<I, S>(lines: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut text = String::new();
    for line in lines {
        text.push_str(line.as_ref());
        text.push('\n');
    }
    text
}

/// The mount point of a mountinfo line, which is its fifth field.
fn mount_point_of(line: &str) -> Option<&str> {
    line.split_whitespace().nth(4)
}

/// The measured table, with each line whose mount point is `mount_point` passed through `edit`.
/// A line that `edit` maps to `None` is left out.
fn edited_table(mount_point: &str, edit: impl Fn(&str) -> Option<String>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in MEASURED_MOUNTINFO {
        if mount_point_of(line) == Some(mount_point) {
            if let Some(kept) = edit(line) {
                lines.push(kept);
            }
        } else {
            lines.push(line.to_owned());
        }
    }
    join_lines(lines)
}

/// The line with `from` replaced by `to`. The anchor must occur in the line, so that a row cannot
/// quietly keep the measured text.
fn replaced(line: &str, from: &str, to: &str) -> String {
    assert!(
        line.contains(from),
        "the anchor {from:?} is not in {line:?}"
    );
    line.replace(from, to)
}

/// The line with `from` replaced by `to` when it is the over-mount's tmpfs line, and kept as
/// measured otherwise, so that an edit of `/dev/pts` leaves the launcher's devpts line alone.
fn replaced_in_over_mount(line: &str, from: &str, to: &str) -> String {
    if line.contains(" - tmpfs ") {
        replaced(line, from, to)
    } else {
        line.to_owned()
    }
}

/// Pads `text` with blank lines to exactly `length` bytes.
fn padded_to(text: &str, length: usize) -> String {
    assert!(
        text.len() <= length,
        "the text is {} bytes, longer than the {length} asked for",
        text.len()
    );
    let mut padded = text.to_owned();
    while padded.len() < length {
        padded.push('\n');
    }
    padded
}

/// The verdict that the bootstrap's own check reaches for a mount table text.
fn outcome(text: &str) -> Verdict {
    confirm_mounts(&parse_mounts(text)?)
}

/// Checks every row and reports each one whose verdict differs, so one run shows all of them.
fn assert_rows(rows: impl IntoIterator<Item = (&'static str, String, Verdict)>) {
    let mismatches: Vec<String> = rows
        .into_iter()
        .filter_map(|(name, text, expected)| {
            let actual = outcome(&text);
            (actual != expected).then(|| format!("{name}: expected {expected:?}, got {actual:?}"))
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "{} row(s) gave another verdict:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn measured_mount_table_passes() -> Verdict {
    let facts = parse_mounts(&join_lines(MEASURED_MOUNTINFO))?;
    assert_eq!(
        facts,
        MountFacts {
            scratch: Some(MEASURED_SCRATCH),
            root_read_only: Some(true),
            dev_read_only: Some(true),
            dev_pts: Some(PtsMount {
                is_tmpfs: true,
                read_only: true,
                size_kib: Some(4),
                mode: Some(0o555),
            }),
            input: Some(InputMount {
                read_only: true,
                nosuid: true,
                nodev: true,
                noexec: true,
            }),
            propagation_tagged: false,
        },
        "the measured table must read as the measured scratch, root, /dev and /dev/pts"
    );
    confirm_mounts(&facts)?;
    assert_eq!(
        SCRATCH_SIZE_BYTES / 1024,
        131_072,
        "the scratch bound must be 128 MiB in KiB"
    );
    assert_eq!(
        SCRATCH_INODES, 4_096,
        "the scratch bound must be 4,096 inodes"
    );
    Ok(())
}

#[test]
fn each_wrong_mount_is_refused_by_name() {
    let scratch_refused = Err(ReadinessFailure::Unexpected("scratch"));
    let root_refused = Err(ReadinessFailure::Unexpected("root"));
    let dev_refused = Err(ReadinessFailure::Unexpected("dev"));
    assert_rows([
        ("the measured table", join_lines(MEASURED_MOUNTINFO), Ok(())),
        (
            "no /tmp line",
            edited_table("/tmp", |_| None),
            Err(ReadinessFailure::Missing("scratch")),
        ),
        (
            "a 64 MiB scratch",
            edited_table("/tmp", |line| {
                Some(replaced(line, "size=131072k", "size=65536k"))
            }),
            scratch_refused,
        ),
        (
            "8192 inodes",
            edited_table("/tmp", |line| {
                Some(replaced(line, "nr_inodes=4096", "nr_inodes=8192"))
            }),
            scratch_refused,
        ),
        (
            "no inode bound",
            edited_table("/tmp", |line| Some(replaced(line, "nr_inodes=4096,", ""))),
            scratch_refused,
        ),
        (
            "a read-only scratch",
            edited_table("/tmp", |line| {
                Some(replaced(
                    line,
                    "rw,nosuid,nodev,noexec,relatime",
                    "ro,nosuid,nodev,noexec,relatime",
                ))
            }),
            scratch_refused,
        ),
        (
            "a scratch that is not tmpfs",
            edited_table("/tmp", |line| {
                Some(replaced(line, " tmpfs tmpfs rw,", " ext4 tmpfs rw,"))
            }),
            scratch_refused,
        ),
        (
            "no / line",
            edited_table("/", |_| None),
            Err(ReadinessFailure::Missing("root")),
        ),
        (
            "a writable /",
            edited_table("/", |line| Some(replaced(line, "/ ro,", "/ rw,"))),
            root_refused,
        ),
        (
            "no /dev line",
            edited_table("/dev", |_| None),
            Err(ReadinessFailure::Missing("dev")),
        ),
        (
            "a writable /dev",
            edited_table("/dev", |line| Some(replaced(line, "/dev ro,", "/dev rw,"))),
            dev_refused,
        ),
    ]);
}

/// The flags of decision 0042 rule 4 and the owner-only mode of the amended rule 3: each one
/// that is missing is refused.
#[test]
fn scratch_flags_and_mode_are_required() {
    let scratch_refused = Err(ReadinessFailure::Unexpected("scratch"));
    assert_rows([
        (
            "a scratch without nosuid",
            edited_table("/tmp", |line| {
                Some(replaced(
                    line,
                    "rw,nosuid,nodev,noexec,",
                    "rw,nodev,noexec,",
                ))
            }),
            scratch_refused,
        ),
        (
            "a scratch without nodev",
            edited_table("/tmp", |line| {
                Some(replaced(
                    line,
                    "rw,nosuid,nodev,noexec,",
                    "rw,nosuid,noexec,",
                ))
            }),
            scratch_refused,
        ),
        (
            "a scratch without noexec",
            edited_table("/tmp", |line| {
                Some(replaced(
                    line,
                    "rw,nosuid,nodev,noexec,",
                    "rw,nosuid,nodev,",
                ))
            }),
            scratch_refused,
        ),
        (
            "a scratch that others may enter",
            edited_table("/tmp", |line| {
                Some(replaced(line, "mode=700,", "mode=1777,"))
            }),
            scratch_refused,
        ),
        (
            "a scratch without a mode",
            edited_table("/tmp", |line| Some(replaced(line, "mode=700,", ""))),
            scratch_refused,
        ),
    ]);
}

#[test]
fn last_line_of_a_mount_point_wins() {
    let measured = join_lines(MEASURED_MOUNTINFO);
    let bad_scratch = replaced(TMP_LINE, "nr_inodes=4096", "nr_inodes=8192");
    let bad_root = replaced(ROOT_LINE, "/ ro,", "/ rw,");
    assert_rows([
        (
            "a later bad /tmp line covers the measured one",
            format!("{measured}{bad_scratch}\n"),
            Err(ReadinessFailure::Unexpected("scratch")),
        ),
        (
            "an earlier bad /tmp line is covered by the measured one",
            format!("{bad_scratch}\n{measured}"),
            Ok(()),
        ),
        (
            "a later writable / covers the measured one",
            format!("{measured}{bad_root}\n"),
            Err(ReadinessFailure::Unexpected("root")),
        ),
        (
            "an earlier writable / is covered by the measured one",
            format!("{bad_root}\n{measured}"),
            Ok(()),
        ),
    ]);
}

/// The launcher's cover must be the last `/dev/pts` line and a read-only tmpfs. Each other
/// table is refused as `DevPts`, whichever line is wrong (the amendment of 2026-11-13, rule 3 as
/// reworded).
#[test]
fn dev_pts_must_be_the_read_only_tmpfs_over_mount() {
    assert_rows([
        ("the measured table", join_lines(MEASURED_MOUNTINFO), Ok(())),
        (
            "only the launcher's devpts",
            edited_table("/dev/pts", |line| {
                (!line.contains(" - tmpfs ")).then(|| line.to_owned())
            }),
            Err(ReadinessFailure::DevPts),
        ),
        (
            "no /dev/pts line",
            edited_table("/dev/pts", |_| None),
            Err(ReadinessFailure::DevPts),
        ),
        (
            "a writable tmpfs over /dev/pts",
            edited_table("/dev/pts", |line| {
                Some(replaced_in_over_mount(line, "/dev/pts ro,", "/dev/pts rw,"))
            }),
            Err(ReadinessFailure::DevPts),
        ),
        (
            "a devpts over-mount",
            edited_table("/dev/pts", |line| {
                Some(replaced_in_over_mount(
                    line,
                    " - tmpfs tmpfs rw,size=4k",
                    " - devpts devpts rw,size=4k",
                ))
            }),
            Err(ReadinessFailure::DevPts),
        ),
    ]);
}

/// The cover must be the launcher's empty tmpfs: 4 KiB and mode 555 (0056 rules 2 and 3, as
/// reworded by the amendments of 2026-11-13). A read-only tmpfs with other bounds is refused as
/// `DevPts`, including one with the scratch's bounds, which the same mount table would otherwise
/// accept (the read-only tmpfs `/dev/shm` is not empty).
#[test]
fn dev_pts_over_mount_must_be_empty() {
    assert_rows([
        (
            "a read-only tmpfs of 128 MiB and 4,096 inodes",
            edited_table("/dev/pts", |line| {
                Some(replaced_in_over_mount(
                    line,
                    "size=4k,",
                    "size=131072k,nr_inodes=4096,",
                ))
            }),
            Err(ReadinessFailure::DevPts),
        ),
        (
            "an 8 KiB size",
            edited_table("/dev/pts", |line| {
                Some(replaced_in_over_mount(line, "size=4k,", "size=8k,"))
            }),
            Err(ReadinessFailure::DevPts),
        ),
        (
            "no size bound",
            edited_table("/dev/pts", |line| {
                Some(replaced_in_over_mount(line, "size=4k,", ""))
            }),
            Err(ReadinessFailure::DevPts),
        ),
        (
            "mode 755",
            edited_table("/dev/pts", |line| {
                Some(replaced_in_over_mount(line, "mode=555", "mode=755"))
            }),
            Err(ReadinessFailure::DevPts),
        ),
        (
            "no mode",
            edited_table("/dev/pts", |line| {
                Some(replaced_in_over_mount(line, "mode=555,", ""))
            }),
            Err(ReadinessFailure::DevPts),
        ),
    ]);
}

/// With stacked mounts the later `/dev/pts` line decides. A launcher's devpts that comes after the
/// over-mount is refused, and a writable tmpfs that comes after a read-only over-mount is refused.
#[test]
fn the_later_dev_pts_line_decides() {
    // The over-mount goes in front of the launcher's devpts line, which is then the later line.
    let mut over_mount_first: Vec<&str> = Vec::new();
    for line in MEASURED_MOUNTINFO {
        if line == PTS_LINE {
            continue;
        }
        if mount_point_of(line) == Some("/dev/pts") {
            over_mount_first.push(PTS_LINE);
        }
        over_mount_first.push(line);
    }
    assert_eq!(
        over_mount_first.len(),
        MEASURED_MOUNTINFO.len(),
        "the reordering must keep every line"
    );
    let writable_over_mount = replaced(PTS_LINE, "/dev/pts ro,", "/dev/pts rw,");
    assert_rows([
        (
            "the devpts line after the over-mount",
            join_lines(over_mount_first),
            Err(ReadinessFailure::DevPts),
        ),
        (
            "the over-mount once more after the measured table",
            join_lines(MEASURED_MOUNTINFO.into_iter().chain([PTS_LINE])),
            Ok(()),
        ),
        (
            "a writable tmpfs after the measured table",
            join_lines(
                MEASURED_MOUNTINFO
                    .into_iter()
                    .chain([writable_over_mount.as_str()]),
            ),
            Err(ReadinessFailure::DevPts),
        ),
    ]);
}

#[test]
fn malformed_or_oversized_text_is_refused() {
    let measured = join_lines(MEASURED_MOUNTINFO);
    let malformed = Err(ReadinessFailure::Malformed("mountinfo"));
    let mut filled = String::new();
    while filled.len() < PROC_FILE_BYTES {
        filled.push_str(&measured);
    }
    assert_rows([
        (
            "a /tmp line without the lone dash",
            edited_table("/tmp", |line| Some(replaced(line, " - tmpfs", " tmpfs"))),
            malformed,
        ),
        (
            "a line with too few fields",
            format!("1290 1201 0:110 / /tmp\n{measured}"),
            malformed,
        ),
        ("measured lines repeated past the bound", filled, malformed),
        (
            "a text one byte under the bound",
            padded_to(&measured, PROC_FILE_BYTES - 1),
            Ok(()),
        ),
        (
            "a text that fills the bound",
            padded_to(&measured, PROC_FILE_BYTES),
            malformed,
        ),
    ]);
}

/// `/input` must be the staged input read-only with `nosuid`, `nodev` and `noexec`, and the last
/// `/input` line decides (0056 rule 3, as reworded by the amendment of 2026-11-13, input and
/// output; 0042 rule 4). Each edit below breaks one of the four flags or makes the line writable.
#[test]
fn input_must_be_read_only_without_suid_devices_or_exec() {
    let input_refused = Err(ReadinessFailure::Input);
    let writable_input = replaced(INPUT_LINE, "/input ro,", "/input rw,");
    let mut rows: Vec<(&'static str, String, Verdict)> = vec![
        ("the measured table", join_lines(MEASURED_MOUNTINFO), Ok(())),
        (
            "no /input line",
            edited_table("/input", |_| None),
            input_refused,
        ),
        (
            "a writable /input after the measured table",
            join_lines(
                MEASURED_MOUNTINFO
                    .into_iter()
                    .chain([writable_input.as_str()]),
            ),
            input_refused,
        ),
        (
            "a writable /input before the measured table",
            join_lines(std::iter::once(writable_input.as_str()).chain(MEASURED_MOUNTINFO)),
            Ok(()),
        ),
    ];
    rows.extend(INPUT_EDITS.map(|(name, from, to)| {
        let text = edited_table("/input", |line| Some(replaced(line, from, to)));
        (name, text, input_refused)
    }));
    assert_rows(rows);
}

/// The edits of the staged input's line that each break one required flag, or make the line
/// writable: the row's name, then the text to replace and its replacement.
const INPUT_EDITS: [(&str, &str, &str); 4] = [
    ("a writable /input", "/input ro,", "/input rw,"),
    ("an /input without nosuid", "ro,nosuid,", "ro,"),
    ("an /input without nodev", "nosuid,nodev,", "nosuid,"),
    ("an /input without noexec", "nodev,noexec,", "nodev,"),
];

/// Any propagation tag on any line refuses the table as `Propagation` (the amendment of 2026-11-13,
/// input and output): on a checked mount, on a mount that no check reads, and on a line that a
/// later line of the same mount point covers.
#[test]
fn propagation_tags_are_refused() {
    let propagation_refused = Err(ReadinessFailure::Propagation);
    let tagged_scratch = replaced(TMP_LINE, " - tmpfs", " unbindable - tmpfs");
    let measured = join_lines(MEASURED_MOUNTINFO);
    assert_rows([
        (
            "master:1 on /tools",
            edited_table("/tools", |line| {
                Some(replaced(line, " - ext4", " master:1 - ext4"))
            }),
            propagation_refused,
        ),
        (
            "shared:1 on /",
            edited_table("/", |line| {
                Some(replaced(line, " - tmpfs", " shared:1 - tmpfs"))
            }),
            propagation_refused,
        ),
        (
            "unbindable on /tmp",
            edited_table("/tmp", |line| {
                Some(replaced(line, " - tmpfs", " unbindable - tmpfs"))
            }),
            propagation_refused,
        ),
        (
            "shared:2 and master:1 on /input",
            edited_table("/input", |line| {
                Some(replaced(line, " - ext4", " shared:2 master:1 - ext4"))
            }),
            propagation_refused,
        ),
        (
            "shared:1 on /proc, which no check reads",
            edited_table("/proc", |line| {
                Some(replaced(line, " - proc", " shared:1 - proc"))
            }),
            propagation_refused,
        ),
        (
            "a tag on an earlier line that a later line covers",
            format!("{tagged_scratch}\n{measured}"),
            propagation_refused,
        ),
    ]);
}

/// The scratch mount id is the first field of its line, read as decimal, and a line whose id is
/// not decimal is malformed (the amendment of 2026-11-13, input and output).
#[test]
fn the_scratch_mount_id_is_read() -> Verdict {
    let facts = parse_mounts(&join_lines(MEASURED_MOUNTINFO))?;
    assert_eq!(
        facts.scratch.map(|scratch| scratch.mount_id),
        Some(1299),
        "the scratch line's first field is its mount id"
    );
    assert_eq!(
        parse_mounts(&edited_table("/tmp", |line| {
            Some(replaced(line, "1299 ", "x299 "))
        })),
        Err(ReadinessFailure::Malformed("mountinfo")),
        "a scratch mount id that is not decimal is malformed"
    );
    Ok(())
}

/// The pid of the fake bootstrap that the member trees below hold.
const BOOTSTRAP_PID: u32 = 42;

/// A verdict of `confirm_member`.
type MemberVerdict = Result<(), ObservationFailure>;

/// A `/proc/<pid>/status` text with every key that `parse_status` reads. `seccomp` is the kernel's
/// `Seccomp` mode (2 for filter mode); the other keys have the values of the measured bootstrap.
fn status_text(seccomp: u32) -> String {
    format!(
        "Name:\tbootstrap\nNoNewPrivs:\t1\nSeccomp:\t{seccomp}\nSeccomp_filters:\t2\n\
         CapInh:\t0000000000000000\nCapPrm:\t0000000000000000\nCapEff:\t0000000000000000\n\
         CapBnd:\t0000000000000000\nCapAmb:\t0000000000000000\n"
    )
}

/// Runs `confirm_member` for `BOOTSTRAP_PID` on a fake group and `/proc` tree. The group's
/// `cgroup.procs` holds `listing`, and the pid's `status` holds `status` when it is given. `index`
/// keeps the directory of each case apart, and the tree is removed again.
fn member_verdict(index: usize, listing: &str, status: Option<&str>) -> io::Result<MemberVerdict> {
    let root = env::temp_dir().join(format!("artifree-confirm-member-{}-{index}", process::id()));
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    let group = root.join("group");
    let proc_root = root.join("proc");
    let member = proc_root.join(BOOTSTRAP_PID.to_string());
    fs::create_dir_all(&group)?;
    fs::create_dir_all(&member)?;
    fs::write(group.join("cgroup.procs"), listing)?;
    if let Some(text) = status {
        fs::write(member.join("status"), text)?;
    }
    let verdict = confirm_member(&group, &proc_root, BOOTSTRAP_PID);
    fs::remove_dir_all(&root)?;
    Ok(verdict)
}

/// `confirm_member` requires the pid in the group's listing and in seccomp filter mode, read from
/// the pid's own status (decision 0042 rule 6; the amendment of 2026-11-13, input and output).
#[test]
fn confirm_member_requires_the_bootstrap_in_filter_mode() -> io::Result<()> {
    let cases: [(&str, &str, Option<String>, MemberVerdict); 6] = [
        (
            "listed in filter mode",
            "42\n",
            Some(status_text(2)),
            Ok(()),
        ),
        (
            "listed among other members",
            "7\n42\n",
            Some(status_text(2)),
            Ok(()),
        ),
        (
            "not listed",
            "7\n",
            Some(status_text(2)),
            Err(ObservationFailure::Membership),
        ),
        (
            "listed, not in filter mode",
            "42\n",
            Some(status_text(0)),
            Err(ObservationFailure::Membership),
        ),
        (
            "a listing line that is not a pid",
            "x\n",
            Some(status_text(2)),
            Err(ObservationFailure::Unreadable),
        ),
        (
            "no status file",
            "42\n",
            None,
            Err(ObservationFailure::Unreadable),
        ),
    ];
    let mut mismatches: Vec<String> = Vec::new();
    for (index, (name, listing, status, expected)) in cases.into_iter().enumerate() {
        let actual = member_verdict(index, listing, status.as_deref())?;
        if actual != expected {
            mismatches.push(format!("{name}: expected {expected:?}, got {actual:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} row(s) gave another verdict:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    Ok(())
}
