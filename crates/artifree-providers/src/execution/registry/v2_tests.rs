//! The `v2` record of decision 0057 (R02bc): its lines, its refusals, and what a `v2` reader does
//! with a `v1` record. Each test is a pure check of the record's text and host decision, so no test
//! touches a file, a cgroup or a clock.

use std::io;

use artifree_domain::change::FeedId;
use artifree_domain::job::JobId;

use super::{Decision, HostIdentity, Identity, Record, RecordRefusal, RecordRun, decide};

/// The machine identity of the test host, as 32 lowercase hexadecimal digits.
const MACHINE: &str = "0123456789abcdef0123456789abcdef";
/// The machine identity of another machine.
const OTHER_MACHINE: &str = "fedcba9876543210fedcba9876543210";
/// The boot identity of the test host.
const BOOT: &str = "01234567-89ab-cdef-0123-456789abcdef";
/// The boot identity of a later boot of the test host.
const OTHER_BOOT: &str = "11111111-2222-3333-4444-555555555555";
/// The cgroup namespace of the test supervisor.
const NAMESPACE: &str = "cgroup:[4026531835]";
/// Another cgroup namespace of the same boot.
const OTHER_NAMESPACE: &str = "cgroup:[4026531999]";
/// The delegated parent of the attempt group in the test records.
const PARENT: &str = "system.slice/x.service";
/// The group name of the identity 1 to 16, written out rather than computed by the code.
const GROUP: &str = "attempt-72623859790382856-651345242494996240";

/// The tag line of a `v1` record.
const TAG_V1: &str = "artifree-attempt-v1";
/// The tag line of a `v2` record.
const TAG_V2: &str = "artifree-attempt-v2";
/// The identity line of the test records.
const IDENTITY_LINE: &str = "identity=0102030405060708090a0b0c0d0e0f10";
/// The machine line of the test records.
const MACHINE_LINE: &str = "machine=0123456789abcdef0123456789abcdef";
/// The boot line of the test records.
const BOOT_LINE: &str = "boot=01234567-89ab-cdef-0123-456789abcdef";
/// The cgroup namespace line of the test records.
const NAMESPACE_LINE: &str = "cgroupns=cgroup:[4026531835]";
/// The parent line of the test records.
const PARENT_LINE: &str = "parent=system.slice/x.service";
/// The group line of the test records.
const GROUP_LINE: &str = "group=attempt-72623859790382856-651345242494996240";
/// The database line of the test records: the feed identity of 16 bytes of `0xab`.
const DATABASE_LINE: &str = "database=abababababababababababababababab";
/// The job line of the test records, whose job is written in its `job_` form.
const JOB_LINE: &str = "job=job_00112233445566778899aabbccddeeff";
/// The fence line of the test records.
const FENCE_LINE: &str = "fence=7";

/// The seven lines of a `v1` record, in order.
const V1_LINES: [&str; 7] = [
    TAG_V1,
    IDENTITY_LINE,
    MACHINE_LINE,
    BOOT_LINE,
    NAMESPACE_LINE,
    PARENT_LINE,
    GROUP_LINE,
];

/// The ten lines of a `v2` record, in order.
const V2_LINES: [&str; 10] = [
    TAG_V2,
    IDENTITY_LINE,
    MACHINE_LINE,
    BOOT_LINE,
    NAMESPACE_LINE,
    PARENT_LINE,
    GROUP_LINE,
    DATABASE_LINE,
    JOB_LINE,
    FENCE_LINE,
];

/// The six `v1` field lines that a `v2` record starts with, in order.
const HEAD_LINES: [&str; 6] = [
    IDENTITY_LINE,
    MACHINE_LINE,
    BOOT_LINE,
    NAMESPACE_LINE,
    PARENT_LINE,
    GROUP_LINE,
];

/// The three claim lines that a `v2` record ends with, in order.
const CLAIM_LINES: [&str; 3] = [DATABASE_LINE, JOB_LINE, FENCE_LINE];

/// The identity 1 to 16 of the test records.
fn sample_identity() -> Identity {
    Identity([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16])
}

/// The job of the test records, parsed from its text through the domain's `FromStr`.
fn sample_job() -> io::Result<JobId> {
    JOB_LINE
        .strip_prefix("job=")
        .ok_or_else(|| io::Error::other("the job line must start with its key"))?
        .parse()
        .map_err(|_| io::Error::other("the job literal must parse as a job id"))
}

/// The claim of the test records: the database of 16 bytes of `0xab`, the job and the fence 7.
fn sample_run() -> io::Result<RecordRun> {
    Ok(RecordRun {
        database: FeedId::from_random([0xab; 16]),
        job: sample_job()?,
        fence: 7,
    })
}

/// The `v1` record of the test records, as R02bb's worker writes it.
fn v1_record() -> Record {
    Record {
        identity: sample_identity(),
        machine: MACHINE.to_owned(),
        boot: BOOT.to_owned(),
        cgroupns: NAMESPACE.to_owned(),
        parent: PARENT.to_owned(),
        group: GROUP.to_owned(),
        run: None,
    }
}

/// The `v2` record of the test records: the `v1` record with its claim added.
fn v2_record() -> io::Result<Record> {
    Ok(Record {
        run: Some(sample_run()?),
        ..v1_record()
    })
}

/// The host that the test records were written on.
fn test_host() -> HostIdentity {
    HostIdentity {
        machine: MACHINE.to_owned(),
        boot: BOOT.to_owned(),
        cgroupns: NAMESPACE.to_owned(),
    }
}

/// The text of `lines`, each followed by a newline.
fn record_text(lines: &[&str]) -> String {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// The bytes of `lines`, each followed by a newline.
fn record_bytes(lines: &[&str]) -> Vec<u8> {
    record_text(lines).into_bytes()
}

/// The bytes of a record made of `tag`, then `head`, then `claim`, each line followed by a newline.
fn assembled(tag: &str, head: &[&str], claim: &[&str]) -> Vec<u8> {
    let mut lines = vec![tag];
    lines.extend_from_slice(head);
    lines.extend_from_slice(claim);
    record_bytes(&lines)
}

/// The bytes of `lines` with the line at `position` replaced by `line`.
fn replaced<'a>(lines: &[&'a str], position: usize, line: &'a str) -> Vec<u8> {
    let edited: Vec<&str> = lines
        .iter()
        .enumerate()
        .map(|(index, &existing)| if index == position { line } else { existing })
        .collect();
    record_bytes(&edited)
}

/// The `v2` record with `x` appended to its parent until it is `total` bytes long.
fn padded_v2_to(total: usize) -> io::Result<Vec<u8>> {
    let base = record_text(&V2_LINES).len();
    let padding = total.checked_sub(base).ok_or_else(|| {
        io::Error::other("the valid v2 record is longer than the size that a test asks for")
    })?;
    let parent = format!("{PARENT_LINE}{}", "x".repeat(padding));
    Ok(assembled(
        TAG_V2,
        &[
            IDENTITY_LINE,
            MACHINE_LINE,
            BOOT_LINE,
            NAMESPACE_LINE,
            parent.as_str(),
            GROUP_LINE,
        ],
        &CLAIM_LINES,
    ))
}

/// The valid `v2` record without its final newline.
fn v2_without_final_newline() -> Vec<u8> {
    let mut bytes = record_bytes(&V2_LINES);
    let newline = bytes.pop();
    assert_eq!(newline, Some(b'\n'), "the fixture ends with a newline");
    bytes
}

/// The `v2` records that the layout check refuses, each with the refusal that it gives.
fn v2_layout_cases() -> Vec<(&'static str, Vec<u8>, RecordRefusal)> {
    vec![
        (
            "the six v1 lines under the v2 tag",
            assembled(TAG_V2, &HEAD_LINES, &[]),
            RecordRefusal::Layout,
        ),
        (
            "a missing database line",
            assembled(TAG_V2, &HEAD_LINES, &[JOB_LINE, FENCE_LINE]),
            RecordRefusal::Layout,
        ),
        (
            "a missing fence line",
            assembled(TAG_V2, &HEAD_LINES, &[DATABASE_LINE, JOB_LINE]),
            RecordRefusal::Layout,
        ),
        (
            "the job line before the database line",
            assembled(TAG_V2, &HEAD_LINES, &[JOB_LINE, DATABASE_LINE, FENCE_LINE]),
            RecordRefusal::Layout,
        ),
        (
            "a repeated fence line",
            assembled(
                TAG_V2,
                &HEAD_LINES,
                &[DATABASE_LINE, JOB_LINE, FENCE_LINE, FENCE_LINE],
            ),
            RecordRefusal::Layout,
        ),
        (
            "an extra line after the fence line",
            assembled(
                TAG_V2,
                &HEAD_LINES,
                &[DATABASE_LINE, JOB_LINE, FENCE_LINE, "extra=1"],
            ),
            RecordRefusal::Layout,
        ),
        (
            "a tag of a later version",
            replaced(&V2_LINES, 0, "artifree-attempt-v3"),
            RecordRefusal::Layout,
        ),
        (
            "a v1 tag followed by the three v2 lines",
            assembled(TAG_V1, &HEAD_LINES, &CLAIM_LINES),
            RecordRefusal::Layout,
        ),
        (
            "no trailing newline",
            v2_without_final_newline(),
            RecordRefusal::Layout,
        ),
    ]
}

/// The `v2` records whose claim fields are outside their formats, each with the refusal that it
/// gives.
fn v2_field_cases() -> Vec<(&'static str, Vec<u8>, RecordRefusal)> {
    vec![
        (
            "a database identity of 31 digits",
            replaced(&V2_LINES, 7, &format!("database={}", "a".repeat(31))),
            RecordRefusal::DatabaseField,
        ),
        (
            "a database identity with an uppercase digit",
            replaced(&V2_LINES, 7, &format!("database={}A", "a".repeat(31))),
            RecordRefusal::DatabaseField,
        ),
        (
            "a job without the job_ prefix",
            replaced(&V2_LINES, 8, "job=00112233445566778899aabbccddeeff"),
            RecordRefusal::JobField,
        ),
        (
            "a job of 31 hexadecimal digits",
            replaced(&V2_LINES, 8, &format!("job=job_{}", "0".repeat(31))),
            RecordRefusal::JobField,
        ),
        (
            "a fence with a leading zero",
            replaced(&V2_LINES, 9, "fence=01"),
            RecordRefusal::FenceField,
        ),
        (
            "a fence with a plus sign",
            replaced(&V2_LINES, 9, "fence=+1"),
            RecordRefusal::FenceField,
        ),
        (
            "a fence with a minus sign",
            replaced(&V2_LINES, 9, "fence=-1"),
            RecordRefusal::FenceField,
        ),
        (
            "an empty fence",
            replaced(&V2_LINES, 9, "fence="),
            RecordRefusal::FenceField,
        ),
        (
            "a fence one above i64::MAX",
            replaced(&V2_LINES, 9, "fence=9223372036854775808"),
            RecordRefusal::FenceField,
        ),
    ]
}

#[test]
fn v2_record_renders_ten_lines_and_parses_back() -> io::Result<()> {
    let record = v2_record()?;
    assert_eq!(
        record.render(),
        record_text(&V2_LINES),
        "a v2 record is the tag, the six v1 lines, then database=, job= and fence=, one per line"
    );
    assert_eq!(
        Record::parse(&record_bytes(&V2_LINES)),
        Ok(record),
        "the ten lines parse back to the same record"
    );
    Ok(())
}

#[test]
fn a_v1_record_parses_with_no_run() {
    assert_eq!(
        Record::parse(&record_bytes(&V1_LINES)),
        Ok(v1_record()),
        "a v1 record parses with no claim"
    );
    assert_eq!(
        v1_record().render(),
        record_text(&V1_LINES),
        "a record without a claim renders the v1 text"
    );
}

#[test]
fn v2_layout_refusals() {
    for (label, bytes, refusal) in v2_layout_cases() {
        assert_eq!(
            Record::parse(&bytes),
            Err(refusal),
            "{label} must be refused"
        );
    }
}

#[test]
fn v2_field_refusals() {
    for (label, bytes, refusal) in v2_field_cases() {
        assert_eq!(
            Record::parse(&bytes),
            Err(refusal),
            "{label} must be refused"
        );
    }
}

#[test]
fn a_fence_of_i64_max_is_accepted() -> io::Result<()> {
    let bytes = replaced(&V2_LINES, 9, &format!("fence={}", i64::MAX));
    let expected = Record {
        run: Some(RecordRun {
            fence: 9_223_372_036_854_775_807,
            ..sample_run()?
        }),
        ..v1_record()
    };
    assert_eq!(
        Record::parse(&bytes),
        Ok(expected),
        "a fence of i64::MAX is the largest fence that is accepted"
    );
    Ok(())
}

#[test]
fn a_v2_record_of_exactly_1024_bytes_is_accepted() -> io::Result<()> {
    let bytes = padded_v2_to(1024)?;
    assert_eq!(bytes.len(), 1024, "the fixture is exactly at the limit");
    let record = Record::parse(&bytes)
        .map_err(|refusal| io::Error::other(format!("{refusal:?} is not expected")))?;
    assert_eq!(
        record.run,
        Some(sample_run()?),
        "a v2 record of 1024 bytes keeps its claim"
    );
    Ok(())
}

#[test]
fn a_v2_record_over_1024_bytes_is_refused() -> io::Result<()> {
    let bytes = padded_v2_to(1025)?;
    assert_eq!(bytes.len(), 1025, "the fixture is one byte over the limit");
    assert_eq!(
        Record::parse(&bytes),
        Err(RecordRefusal::TooLong),
        "a v2 record of 1025 bytes is refused"
    );
    Ok(())
}

#[test]
fn decide_ignores_the_v2_lines() -> io::Result<()> {
    let cases = [
        (
            "the same boot and namespace",
            test_host(),
            Decision::SameBoot,
        ),
        (
            "a later boot",
            HostIdentity {
                boot: OTHER_BOOT.to_owned(),
                ..test_host()
            },
            Decision::ChangedBoot,
        ),
        (
            "another namespace",
            HostIdentity {
                cgroupns: OTHER_NAMESPACE.to_owned(),
                ..test_host()
            },
            Decision::OtherNamespace,
        ),
        (
            "another machine",
            HostIdentity {
                machine: OTHER_MACHINE.to_owned(),
                ..test_host()
            },
            Decision::OtherMachine,
        ),
    ];
    let without_claim = v1_record();
    let with_claim = v2_record()?;
    for (label, facts, expected) in cases {
        assert_eq!(
            decide(&without_claim, &facts),
            expected,
            "a v1 record, {label}"
        );
        assert_eq!(
            decide(&with_claim, &facts),
            expected,
            "a v2 record, {label}"
        );
    }
    Ok(())
}
