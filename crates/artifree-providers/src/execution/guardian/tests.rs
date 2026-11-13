//! Behavioral tests of the guardian's pure parts (decision 0056, amendment of 2026-11-13, the
//! guardian): the attempt-name check, the decision that ends an attempt and the report line that
//! the provider parses.
//!
//! The tests touch no process, file or clock; they run in the normal `cargo test` of the Linux
//! build.

use super::{
    DONE, GuardianReason, GuardianState, LEASE, RENEW, STOP, is_attempt_name, parse_report,
    report_line,
};

/// The absolute deadline of the decision table, in milliseconds from the guardian's start.
const DEADLINE_MS: u64 = 10_000;
/// The renewal window of the decision table, in milliseconds.
const WINDOW_MS: u64 = 1_000;

/// One row of the decision table: the records that arrive in order, then one decision.
struct Case {
    /// Names the row in assertion messages.
    name: &'static str,
    /// Each record as its byte and the time it arrives, in milliseconds.
    records: &'static [(u8, u64)],
    /// The time of the decision, in milliseconds.
    now_ms: u64,
    /// Whether the socket reports end of file before the decision.
    eof: bool,
    /// The reason that the decision must give, or `None` while the attempt goes on.
    expected: Option<GuardianReason>,
}

/// The decision table, run with `DEADLINE_MS` and `WINDOW_MS`. The rows at the end pin which check
/// comes first when several would end the attempt.
const DECISIONS: &[Case] = &[
    Case {
        name: "deadline before eof",
        records: &[],
        now_ms: 10_000,
        eof: true,
        expected: Some(GuardianReason::Deadline),
    },
    Case {
        name: "deadline while running",
        records: &[(LEASE, 0), (RENEW, 9_990)],
        now_ms: 10_000,
        eof: false,
        expected: Some(GuardianReason::Deadline),
    },
    Case {
        name: "before the deadline",
        records: &[],
        now_ms: 9_999,
        eof: false,
        expected: None,
    },
    Case {
        name: "eof without D",
        records: &[],
        now_ms: 5,
        eof: true,
        expected: Some(GuardianReason::SupervisorGone),
    },
    Case {
        name: "D then eof",
        records: &[(DONE, 5)],
        now_ms: 6,
        eof: true,
        expected: None,
    },
    Case {
        name: "S then D then eof",
        records: &[(LEASE, 100), (STOP, 200), (DONE, 500)],
        now_ms: 5_000,
        eof: true,
        expected: None,
    },
    Case {
        name: "R before L starts no window",
        records: &[(RENEW, 0)],
        now_ms: 5_000,
        eof: false,
        expected: None,
    },
    Case {
        name: "L then no R just before the window",
        records: &[(LEASE, 100)],
        now_ms: 1_099,
        eof: false,
        expected: None,
    },
    Case {
        name: "L then no R for the window",
        records: &[(LEASE, 100)],
        now_ms: 1_100,
        eof: false,
        expected: Some(GuardianReason::RenewalExpired),
    },
    Case {
        name: "R resets the window",
        records: &[(LEASE, 100), (RENEW, 900)],
        now_ms: 1_100,
        eof: false,
        expected: None,
    },
    Case {
        name: "R resets the window, which then expires",
        records: &[(LEASE, 100), (RENEW, 900)],
        now_ms: 1_900,
        eof: false,
        expected: Some(GuardianReason::RenewalExpired),
    },
    Case {
        name: "S stops the window",
        records: &[(LEASE, 100), (STOP, 500)],
        now_ms: 5_000,
        eof: false,
        expected: None,
    },
    Case {
        name: "deadline before an expired window",
        records: &[(LEASE, 0)],
        now_ms: 10_000,
        eof: false,
        expected: Some(GuardianReason::Deadline),
    },
    Case {
        name: "eof before an expired window",
        records: &[(LEASE, 0)],
        now_ms: 1_000,
        eof: true,
        expected: Some(GuardianReason::SupervisorGone),
    },
];

/// Each report line of the table: a reason, its `emptied` value and the exact line that the
/// guardian writes for them.
const REPORT_LINES: &[(GuardianReason, bool, &str)] = &[
    (
        GuardianReason::SupervisorGone,
        false,
        "guardian supervisor-gone emptied=0\n",
    ),
    (
        GuardianReason::SupervisorGone,
        true,
        "guardian supervisor-gone emptied=1\n",
    ),
    (
        GuardianReason::RenewalExpired,
        false,
        "guardian renewal-expired emptied=0\n",
    ),
    (
        GuardianReason::RenewalExpired,
        true,
        "guardian renewal-expired emptied=1\n",
    ),
    (
        GuardianReason::Deadline,
        false,
        "guardian deadline emptied=0\n",
    ),
    (
        GuardianReason::Deadline,
        true,
        "guardian deadline emptied=1\n",
    ),
];

/// The names the provider gives attempt groups, and whether the guardian accepts each of them.
#[test]
fn name_check_accepts_only_attempt_digit_pairs() {
    let cases = [
        ("attempt-1-0", true),
        ("attempt-1", false),
        ("attempt--0", false),
        ("attempt-1-", false),
        ("attempt-a-0", false),
        ("attempt-1-0-2", false),
        ("../attempt-1-0", false),
        ("attempt-1-0/x", false),
        ("", false),
    ];
    for (name, expected) in cases {
        assert_eq!(is_attempt_name(name), expected, "name {name:?}");
    }
}

/// Each row of the table gives its decision after its records, at its time and with its end-of-file
/// flag. Every record of a row must be a record of the protocol.
#[test]
fn decision_follows_the_protocol_order() {
    for case in DECISIONS {
        let mut state = GuardianState::new(DEADLINE_MS, WINDOW_MS);
        for &(byte, at_ms) in case.records {
            assert!(
                state.on_record(byte, at_ms),
                "{}: byte {byte} must be a protocol record",
                case.name
            );
        }
        assert_eq!(
            state.decide(case.now_ms, case.eof),
            case.expected,
            "{}",
            case.name
        );
    }
}

/// Each of the four records of the protocol is accepted and none of them is treated as end of file.
#[test]
fn protocol_records_are_accepted() {
    let mut state = GuardianState::new(DEADLINE_MS, WINDOW_MS);
    for byte in [LEASE, RENEW, STOP, DONE] {
        assert!(state.on_record(byte, 0), "record {byte} must be accepted");
    }
}

/// A byte that is not a record of the protocol is refused, and the caller then treats it as end of
/// file: with no `D` the attempt ends as `supervisor-gone`.
#[test]
fn unknown_byte_acts_as_end_of_file() {
    let mut state = GuardianState::new(DEADLINE_MS, WINDOW_MS);
    assert!(
        !state.on_record(b'X', 0),
        "X is not a record of the protocol"
    );
    assert_eq!(state.decide(1, true), Some(GuardianReason::SupervisorGone));
}

/// Each reason with each `emptied` value writes its exact line, and the provider's parser reads
/// that line back, with or without its trailing newline.
#[test]
fn report_lines_round_trip_through_parse_report() {
    for &(reason, emptied, line) in REPORT_LINES {
        assert_eq!(
            report_line(reason, emptied),
            line,
            "line of {reason:?} with emptied={emptied}"
        );
        assert_eq!(
            parse_report(line),
            Some((reason, emptied)),
            "parsing {line:?}"
        );
        assert_eq!(
            parse_report(line.trim_end_matches('\n')),
            Some((reason, emptied)),
            "parsing {line:?} without its newline"
        );
    }
}

/// The provider's parser refuses every text that is not a report line, including a line with two
/// trailing newlines, a line with a leading space and a line with an unknown word.
#[test]
fn parse_report_refuses_malformed_lines() {
    let malformed = [
        "",
        "guardian",
        "guardian deadline",
        "guardian deadline emptied=2",
        "guardian later emptied=1",
        "Guardian deadline emptied=1",
        "guardian deadline emptied=1 extra",
        "guardian deadline emptied=1\n\n",
        " guardian deadline emptied=1",
    ];
    for line in malformed {
        assert_eq!(parse_report(line), None, "{line:?} must be refused");
    }
}
