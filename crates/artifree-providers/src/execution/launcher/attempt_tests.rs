//! Behavioral tests for the attempt's fixed argument list and the namespace
//! check's list (decision 0056 rule 2 with its amendment of 2026-11-12, and
//! decision 0042 rule 1): the scratch is only a mount point that the bootstrap
//! fills, the attempt is granted exactly the two capabilities, the read-only
//! remounts come after every mount option and before `--chdir`, and the
//! namespace check keeps its own scratch and gets neither.

use super::{attempt_arguments, launcher_arguments};

use std::error::Error;
use std::ffi::OsString;
use std::path::Path;

/// The argument list as text, one string per argument.
fn as_text(arguments: &[OsString]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

/// The index of the first `option` argument that is directly followed by `value`, if any.
fn pair_position(arguments: &[String], option: &str, value: &str) -> Option<usize> {
    arguments
        .windows(2)
        .position(|pair| matches!(pair, [flag, argument] if flag == option && argument == value))
}

/// The index of every argument equal to `option`, in order.
fn option_positions(arguments: &[String], option: &str) -> Vec<usize> {
    arguments
        .iter()
        .enumerate()
        .filter(|(_, argument)| argument.as_str() == option)
        .map(|(index, _)| index)
        .collect()
}

#[test]
fn remounts_follow_every_mount_option_and_precede_chdir() -> Result<(), Box<dyn Error>> {
    let arguments = as_text(&attempt_arguments(Path::new("/opt/t")));
    let Some(chdir) = pair_position(&arguments, "--chdir", "/") else {
        return Err(
            format!("the attempt must chdir to / before the fixture: {arguments:?}").into(),
        );
    };
    for target in ["/", "/dev"] {
        let Some(remount) = pair_position(&arguments, "--remount-ro", target) else {
            return Err(format!(
                "the remount of {target} read-only must be present: {arguments:?}"
            )
            .into());
        };
        for option in ["--ro-bind", "--proc", "--dev", "--dir"] {
            let occurrences = option_positions(&arguments, option);
            assert!(
                !occurrences.is_empty(),
                "the attempt must contain {option}, or the order check proves nothing: {arguments:?}"
            );
            for occurrence in occurrences {
                assert!(
                    occurrence < remount,
                    "the remount of {target} at {remount} must follow {option} at {occurrence}: {arguments:?}"
                );
            }
        }
        assert!(
            remount < chdir,
            "the remount of {target} at {remount} must precede --chdir at {chdir}: {arguments:?}"
        );
    }
    Ok(())
}

#[test]
fn attempt_scratch_is_a_mount_point_only() {
    let arguments = as_text(&attempt_arguments(Path::new("/opt/t")));
    assert!(
        pair_position(&arguments, "--dir", "/tmp").is_some(),
        "the attempt must make the scratch's mount point with --dir /tmp: {arguments:?}"
    );
    for option in ["--tmpfs", "--size"] {
        assert!(
            !arguments.iter().any(|argument| argument == option),
            "the launcher must not mount the scratch, so {option} must be absent: {arguments:?}"
        );
    }
}

#[test]
fn attempt_gets_exactly_the_two_capabilities() {
    let arguments = as_text(&attempt_arguments(Path::new("/opt/t")));
    for capability in ["CAP_SYS_ADMIN", "CAP_SETPCAP"] {
        assert!(
            pair_position(&arguments, "--cap-add", capability).is_some(),
            "the attempt must add {capability} with --cap-add: {arguments:?}"
        );
    }
    assert_eq!(
        option_positions(&arguments, "--cap-add").len(),
        2,
        "the attempt must add exactly the two capabilities: {arguments:?}"
    );
}

#[test]
fn attempt_ends_with_the_fixture_bootstrap() {
    let arguments = as_text(&attempt_arguments(Path::new("/opt/t")));
    let tail: Vec<&str> = arguments
        .iter()
        .rev()
        .take(3)
        .rev()
        .map(String::as_str)
        .collect();
    assert_eq!(
        tail,
        [
            "/tools/bin/artifree-execution-fixture",
            "bootstrap",
            "artifree-fixture-inspection-v1",
        ],
        "the attempt must end with the fixture's bootstrap mode and its identity: {arguments:?}"
    );
}

#[test]
fn namespace_check_has_no_capability_and_no_remount() {
    let arguments = as_text(&launcher_arguments(Path::new("/opt/t")));
    assert!(
        pair_position(&arguments, "--size", "1048576").is_some(),
        "the namespace check must keep its 1 MiB scratch size: {arguments:?}"
    );
    assert!(
        pair_position(&arguments, "--tmpfs", "/tmp").is_some(),
        "the namespace check must keep its own scratch tmpfs at /tmp: {arguments:?}"
    );
    for option in ["--cap-add", "--remount-ro", "--dir"] {
        assert!(
            !arguments.iter().any(|argument| argument == option),
            "the namespace check must not carry {option}: {arguments:?}"
        );
    }
}
