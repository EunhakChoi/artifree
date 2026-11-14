//! The measured-calls file of the bundler measurement accepts only canonical, increasing numbers.

use super::parse_measured_calls;

#[test]
fn a_canonical_file_is_read_in_order() {
    assert_eq!(parse_measured_calls(b""), Some(Vec::new()));
    assert_eq!(parse_measured_calls(b"0\n"), Some(vec![0]));
    assert_eq!(
        parse_measured_calls(b"7\n39\n63\n1023\n"),
        Some(vec![7, 39, 63, 1023])
    );
}

#[test]
fn a_malformed_file_is_refused() {
    for bytes in [
        &b"\n"[..],
        b"7",
        b"7\n\n",
        b"07\n",
        b"-1\n",
        b"+7\n",
        b" 7\n",
        b"7 \n",
        b"39\n7\n",
        b"7\n7\n",
        b"1024\n",
        b"99999999999999999999\n",
        b"\xff\n",
    ] {
        assert_eq!(parse_measured_calls(bytes), None, "{bytes:?}");
    }
}
