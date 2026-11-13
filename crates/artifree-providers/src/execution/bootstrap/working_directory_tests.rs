//! Tests of the bootstrap's working-directory readback (decision 0056 rule 3, as reworded by the
//! amendment of 2026-11-13, crashes): the readback accepts exactly the scratch root `/tmp`.

use super::is_scratch_root;
use std::ffi::CStr;

/// Only the exact scratch root passes. A trailing slash, a directory below the root, a longer
/// name that starts with the root and the host root are each refused.
#[test]
fn only_the_scratch_root_is_the_working_directory() {
    let cases: [(&CStr, bool); 5] = [
        (c"/tmp", true),
        (c"/", false),
        (c"/tmp/", false),
        (c"/tmp/out", false),
        (c"/tmpx", false),
    ];
    for (cwd, expected) in cases {
        assert_eq!(is_scratch_root(cwd), expected, "{cwd:?}");
    }
}
