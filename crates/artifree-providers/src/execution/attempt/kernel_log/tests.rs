//! Tests of the kernel-log record parser (decision 0056, amendment of 2026-11-13). They parse
//! literal lines in the runner's `/dev/kmsg` format and never open the kernel log, so they run in
//! the normal `cargo test`.

use std::io;

use super::{SeccompRecord, collect_lines, parse_record};

/// A `/dev/kmsg` line of a kernel message that is not a seccomp record.
const OTHER_RECORD: &str =
    "6,1235,567891,-;usb 1-1: new high-speed USB device number 4 using xhci_hcd";

/// One `/dev/kmsg` line of a refused i386 probe, with its prefix, as the runner's kernel printed
/// it. The probe enters the kernel through `int 0x80` with call 1, so `arch` is the i386 audit
/// value.
const RUNNER_RECORD: &str = "6,1234,567890,-;audit: type=1326 audit(1794499092.272:128): auid=4294967295 uid=1001 gid=1001 ses=4294967295 subj=artifree-probe pid=2726 comm=\"abi_i386_exit\" exe=\"/tools/elf/abi_i386_exit\" sig=31 arch=40000003 syscall=1 compat=1 ip=0x400084 code=0x80000000";

/// The x32 number of call 186 (`gettid`): the x32 bit `0x4000_0000` set on the native number.
/// Its decimal form is `1073742010`.
const X32_GETTID: u64 = 0x4000_00BA;

/// The record that [`RUNNER_RECORD`] parses to.
fn runner_record() -> SeccompRecord {
    SeccompRecord {
        exe: "/tools/elf/abi_i386_exit".to_owned(),
        sig: 31,
        arch: 0x4000_0003,
        syscall: 1,
    }
}

/// Returns `line` with the first field named `key` replaced by `token`, a whole `key=value`
/// token, or removed when `token` is `None`. The other fields keep their text.
///
/// The helper asserts that the field exists, so a test cannot pass on an edit that changed
/// nothing.
fn set_field(line: &str, key: &str, token: Option<&str>) -> String {
    let prefix = format!("{key}=");
    let mut found = false;
    let mut parts: Vec<&str> = Vec::new();
    for part in line.split(' ') {
        if !found && part.starts_with(prefix.as_str()) {
            found = true;
            if let Some(replacement) = token {
                parts.push(replacement);
            }
        } else {
            parts.push(part);
        }
    }
    assert!(found, "the record has no `{key}=` field to edit");
    parts.join(" ")
}

#[test]
fn parses_the_runners_seccomp_record() {
    assert_eq!(
        parse_record(RUNNER_RECORD),
        Some(runner_record()),
        "the runner's i386 refusal must parse to its program, SIGSYS, the i386 audit value and call 1",
    );
}

#[test]
fn parses_a_native_record_with_the_x32_bit() {
    let with_arch = set_field(RUNNER_RECORD, "arch", Some("arch=c000003e"));
    let line = set_field(&with_arch, "syscall", Some("syscall=1073742010"));
    assert_eq!(
        parse_record(&line),
        Some(SeccompRecord {
            arch: 0xC000_003E,
            syscall: X32_GETTID,
            ..runner_record()
        }),
        "an x86-64 record must keep its audit value, and its x32 number must keep the x32 bit",
    );
}

#[test]
fn ignores_a_record_of_another_type() {
    assert_eq!(
        parse_record(RUNNER_RECORD),
        Some(runner_record()),
        "the runner line must parse, so that the other-type cases fail on the type alone",
    );
    for other in ["type=1400", "type=13260"] {
        let line = set_field(RUNNER_RECORD, "type", Some(other));
        assert_eq!(
            parse_record(&line),
            None,
            "a record of `{other}` is not a seccomp refusal",
        );
    }
}

#[test]
fn a_line_without_the_prefix_separator_gives_none() {
    let line = RUNNER_RECORD.replace(';', " ");
    assert_eq!(
        parse_record(&line),
        None,
        "a line without `;` has no message to read",
    );
}

#[test]
fn a_missing_field_gives_none() {
    for key in ["exe", "sig", "arch", "syscall"] {
        let line = set_field(RUNNER_RECORD, key, None);
        assert_eq!(
            parse_record(&line),
            None,
            "a seccomp record without `{key}=` must not parse",
        );
    }
}

#[test]
fn a_malformed_number_gives_none() {
    for (key, token) in [
        ("sig", "sig=abc"),
        ("arch", "arch=zz"),
        ("syscall", "syscall=-1"),
    ] {
        let line = set_field(RUNNER_RECORD, key, Some(token));
        assert_eq!(
            parse_record(&line),
            None,
            "`{token}` is not a valid `{key}` value",
        );
    }
}

#[test]
fn an_unquoted_exe_gives_none() {
    let line = set_field(RUNNER_RECORD, "exe", Some("exe=2F746D702F78"));
    assert_eq!(
        parse_record(&line),
        None,
        "a hex-encoded exe value (here `/tmp/x`) is not a quoted program path",
    );
}

#[test]
fn continuation_lines_are_ignored() {
    let followed = format!("{RUNNER_RECORD}\n SUBSYSTEM=x");
    assert_eq!(
        parse_record(&followed),
        Some(runner_record()),
        "a continuation line after the first line must not change the record",
    );
    let without_exe = set_field(RUNNER_RECORD, "exe", None);
    let completed = format!("{without_exe}\n exe=\"/tools/elf/abi_i386_exit\"");
    assert_eq!(
        parse_record(&completed),
        None,
        "a continuation line must not supply a field that the first line lacks",
    );
}

#[test]
fn collects_seccomp_records_and_skips_other_lines() -> io::Result<()> {
    let other_type = set_field(RUNNER_RECORD, "type", Some("type=1400"));
    let lines = [
        OTHER_RECORD.to_owned(),
        other_type,
        RUNNER_RECORD.to_owned(),
    ];
    let mut records = Vec::new();
    collect_lines(&lines, &mut records)?;
    assert_eq!(
        records,
        [runner_record()],
        "only the seccomp record may be collected; the other lines are not refusals",
    );
    Ok(())
}

#[test]
fn an_unparseable_seccomp_record_fails_the_window() {
    let cases = [
        (
            "an unquoted exe",
            set_field(RUNNER_RECORD, "exe", Some("exe=2F746D702F78")),
        ),
        ("a missing sig", set_field(RUNNER_RECORD, "sig", None)),
        (
            "a malformed arch",
            set_field(RUNNER_RECORD, "arch", Some("arch=zz")),
        ),
        ("no prefix separator", RUNNER_RECORD.replace(';', " ")),
    ];
    for (case, line) in cases {
        let mut records = Vec::new();
        let result = collect_lines(std::slice::from_ref(&line), &mut records);
        let Some(error) = result.err() else {
            panic!(
                "{case}: a seccomp record that cannot be parsed must fail the window, got {records:?}"
            );
        };
        assert_eq!(
            error.kind(),
            io::ErrorKind::InvalidData,
            "{case}: the error must be InvalidData, got {error}",
        );
        assert!(
            error.to_string().contains(line.as_str()),
            "{case}: the error must name the line, got {error}",
        );
        assert!(
            records.is_empty(),
            "{case}: no record may be collected from the line, got {records:?}",
        );
    }
}
