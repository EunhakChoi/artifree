//! Behavioral tests of the routing check during an attempt (decision 0056 rule 5, as reworded by
//! the amendment of 2026-11-13, crashes): the bytes read at each poll must equal the bytes that
//! admission read, and a routing that cannot be read is refused as unreadable.
//!
//! The tests use byte slices as readers and run in the normal `cargo test`.

use super::{AdmissionRefusal, routing_unchanged};

use std::io;

/// A named check: the bytes that admission read, the reader result of the attempt's poll and the
/// outcome that the attempt must see.
type RoutingCase<'a> = (
    &'static str,
    &'a [u8],
    io::Result<&'a [u8]>,
    Result<(), AdmissionRefusal>,
);

/// Each reader result of a routing check, with the bytes that admission read and the outcome
/// that the attempt must see. A change is named as a change whatever the new value is, and a
/// value over the bound keeps its own refusal.
#[test]
fn routing_during_the_attempt_is_compared_byte_for_byte() {
    let oversized = [b'a'; 4097];
    let cases: [RoutingCase<'_>; 7] = [
        ("same bytes", &b"core\n"[..], Ok(&b"core\n"[..]), Ok(())),
        (
            "a lost newline",
            &b"core"[..],
            Ok(&b"core\n"[..]),
            Err(AdmissionRefusal::CoreRoutingChanged),
        ),
        (
            "a different file",
            &b"core\n"[..],
            Ok(&b"other\n"[..]),
            Err(AdmissionRefusal::CoreRoutingChanged),
        ),
        (
            "a pipe after a file",
            &b"core\n"[..],
            Ok(&b"|/x"[..]),
            Err(AdmissionRefusal::CoreRoutingChanged),
        ),
        (
            "a socket after a file",
            &b"core\n"[..],
            Ok(&b"@/x"[..]),
            Err(AdmissionRefusal::CoreRoutingChanged),
        ),
        (
            "an emptied routing",
            &b"core\n"[..],
            Ok(&b""[..]),
            Err(AdmissionRefusal::CoreRoutingChanged),
        ),
        (
            "an oversized routing",
            &b"core\n"[..],
            Ok(oversized.as_slice()),
            Err(AdmissionRefusal::UnknownCoreRouting),
        ),
    ];
    for (case, admitted, read, expected) in cases {
        assert_eq!(
            routing_unchanged(admitted, read),
            expected,
            "case `{case}` with admitted bytes {admitted:?}"
        );
    }
}

/// An unreadable routing is refused as unreadable, never as a change, through the same reader.
#[test]
fn a_failed_read_is_unreadable() {
    let unreadable: io::Result<&[u8]> = Err(io::Error::from(io::ErrorKind::PermissionDenied));
    assert_eq!(
        routing_unchanged(b"core\n", unreadable),
        Err(AdmissionRefusal::UnreadableCoreRouting)
    );
}
