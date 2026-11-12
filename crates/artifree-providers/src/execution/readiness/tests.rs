//! Tests of the mount checks in `readiness` (the amendment of 2026-11-12, rules 2 and 3): the
//! measured mount table passes, each wrong mount is refused under its field name, the last line of
//! a mount point decides, and malformed or oversized text is refused.

use super::{
    MountFacts, PROC_FILE_BYTES, ReadinessFailure, SCRATCH_INODES, SCRATCH_SIZE_BYTES,
    ScratchMount, confirm_mounts, parse_mounts,
};

/// A readiness verdict on one mount table text.
type Verdict = Result<(), ReadinessFailure>;

// The measured bootstrap's `/proc/<pid>/mountinfo` lines (the amendment of 2026-11-12). The ids
// and the roots of the lines are illustrative; the mount points, options and filesystems are the
// kernel's values from the runner, except `noexec` on `/tmp`: the probe mounted the scratch
// without it, and the amended rule 3 adds it.
const ROOT_LINE: &str =
    "1290 1201 0:110 / / ro,nosuid,nodev,relatime - tmpfs tmpfs rw,uid=1001,gid=1001,inode64";
const DEV_LINE: &str = "1295 1290 0:112 / /dev ro,nosuid,nodev,relatime - tmpfs tmpfs rw,mode=755,uid=1001,gid=1001,inode64";
const TMP_LINE: &str = "1299 1290 0:115 / /tmp rw,nosuid,nodev,noexec,relatime - tmpfs tmpfs rw,size=131072k,nr_inodes=4096,mode=700,uid=1001,gid=1001,inode64";

const MEASURED_MOUNTINFO: [&str; 8] = [
    ROOT_LINE,
    "1291 1290 8:1 /opt/artifree/tools/v1 /tools ro,nosuid,nodev,relatime - ext4 /dev/root rw,discard",
    "1292 1290 8:1 /opt/artifree/tools/v1/lib /lib ro,nosuid,nodev,relatime - ext4 /dev/root rw,discard",
    "1293 1290 8:1 /opt/artifree/tools/v1/lib64 /lib64 ro,nosuid,nodev,relatime - ext4 /dev/root rw,discard",
    "1294 1290 0:111 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw",
    DEV_LINE,
    "1296 1295 0:113 / /dev/pts rw,nosuid,noexec,relatime - devpts devpts rw,mode=600,ptmxmode=666",
    TMP_LINE,
];

/// The scratch facts of the measured table: a writable, owner-only tmpfs with `nosuid`, `nodev`
/// and `noexec`, at 128 MiB
/// (`131072k` in the kernel's form) and 4,096 inodes.
const MEASURED_SCRATCH: ScratchMount = ScratchMount {
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
fn assert_rows<const N: usize>(rows: [(&str, String, Verdict); N]) {
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
        },
        "the measured table must read as the measured scratch, root and /dev"
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
