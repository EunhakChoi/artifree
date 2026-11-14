//! Behavioral tests of the worker's execution adapter that need no launch (decision 0057 rules 7
//! and 19): which unsettled attempts a capacity settles, which foreign row quarantines the
//! provider, and that every checkpoint stop reason reaches the guardian as its own lease-loss part
//! and comes back unchanged. The functions tested here are pure: nothing is locked or launched.

use std::error::Error;

use artifree_domain::identity::UnixMillis;
use artifree_domain::job::{JobId, ProfileName};

use super::*;

/// The result of a test body: a setup failure ends the test with its error.
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The execution directory of the host under test, as 32 lowercase hexadecimal digits.
const DIRECTORY: &str = "0123456789abcdef0123456789abcdef";
/// Another execution directory, on the same host.
const OTHER_DIRECTORY: &str = "fedcba9876543210fedcba9876543210";

/// Every stop reason of a checkpoint (decision 0057 rule 7).
const STOP_REASONS: [StopReason; 7] = [
    StopReason::Cancel,
    StopReason::Trash,
    StopReason::Purge,
    StopReason::Generation,
    StopReason::Authority,
    StopReason::Lease,
    StopReason::Fence,
];

/// The host of the worker under test: its machine, boot and cgroup namespace.
fn host() -> ExecutionHost {
    ExecutionHost {
        machine: "machine-1".to_owned(),
        boot: "boot-1".to_owned(),
        cgroupns: "cgroup-1".to_owned(),
    }
}

/// A stored attempt of the given identity, claimed on `on` in `directory`, in `state`. Its job,
/// profile and fence are fixed, since the selection reads only the host, the directory and the
/// state.
fn row_on(
    seed: u8,
    on: &ExecutionHost,
    directory: &str,
    state: AttemptState,
) -> TestResult<AttemptRow> {
    let profile: ProfileName = "fixture".parse()?;
    Ok(AttemptRow {
        attempt: AttemptIdentity::from_random([seed; 16]),
        job: JobId::from_random([seed; 16]),
        fence: 1,
        actor_generation: None,
        profile,
        tool_digest: "0".repeat(64),
        machine: on.machine.clone(),
        boot: on.boot.clone(),
        cgroupns: on.cgroupns.clone(),
        directory: directory.to_owned(),
        state,
        revoked: None,
        created_at: UnixMillis(100),
        settled_at: None,
    })
}

#[test]
fn settleable_selects_removed_and_own_record_less_rows() -> TestResult {
    let this = host();
    let other_machine = ExecutionHost {
        machine: "machine-2".to_owned(),
        ..host()
    };
    let other_boot = ExecutionHost {
        boot: "boot-0".to_owned(),
        ..host()
    };
    let other_cgroupns = ExecutionHost {
        cgroupns: "cgroup-2".to_owned(),
        ..host()
    };
    // An attempt of another machine that reconciliation removed: it is settled by identity.
    let removed = row_on(1, &other_machine, OTHER_DIRECTORY, AttemptState::Live)?;
    // A record-less attempt of this machine, cgroup namespace and directory, after another boot:
    // the boot is not compared, so it is settled.
    let own = row_on(2, &other_boot, DIRECTORY, AttemptState::Live)?;
    // Each of the three below differs from this worker in one of the identities that select it.
    let other_directory = row_on(3, &this, OTHER_DIRECTORY, AttemptState::Live)?;
    let other_machine_row = row_on(4, &other_machine, DIRECTORY, AttemptState::Live)?;
    let other_cgroupns_row = row_on(5, &other_cgroupns, DIRECTORY, AttemptState::Live)?;
    let (removed_id, own_id) = (removed.attempt, own.attempt);
    let unsettled = [
        removed,
        own,
        other_directory,
        other_machine_row,
        other_cgroupns_row,
    ];

    let (settle, foreign_pending) = settleable(&unsettled, &[removed_id.bytes()], &this, DIRECTORY);

    assert_eq!(
        settle,
        [removed_id, own_id],
        "the removed attempt and the record-less attempt of this machine, namespace and directory \
         are settled, and an attempt of another directory, machine or namespace is left alone"
    );
    assert!(
        !foreign_pending,
        "no row is cleanup_pending, so no foreign row quarantines the provider"
    );
    Ok(())
}

#[test]
fn a_foreign_cleanup_pending_row_quarantines_and_a_foreign_live_row_does_not() -> TestResult {
    let this = host();
    let cases = [
        (AttemptState::CleanupPending, true),
        (AttemptState::Live, false),
    ];
    for (state, quarantines) in cases {
        let foreign = row_on(1, &this, OTHER_DIRECTORY, state)?;

        let (settle, foreign_pending) = settleable(&[foreign], &[], &this, DIRECTORY);

        assert!(
            settle.is_empty(),
            "a foreign {state:?} row of another directory is never settled, got {settle:?}"
        );
        assert_eq!(
            foreign_pending, quarantines,
            "a foreign {state:?} row must set the quarantine flag: {quarantines}"
        );
    }
    Ok(())
}

#[test]
fn every_stop_reason_round_trips() -> TestResult {
    for reason in STOP_REASONS {
        let heartbeat = heartbeat_of(Ok(CheckpointAnswer::Stop(reason)));
        let Heartbeat::Lost(part) = heartbeat else {
            return Err(
                format!("a stop of {reason:?} must lose the lease, got {heartbeat:?}").into(),
            );
        };
        assert_eq!(
            stop_reason_of(part),
            reason,
            "the lost part {part:?} of a stop of {reason:?} must give the same stop reason back"
        );
    }
    assert_eq!(
        heartbeat_of(Ok(CheckpointAnswer::Continue)),
        Heartbeat::Confirmed,
        "a continue confirms the lease, so the guardian renews"
    );
    assert_eq!(
        heartbeat_of(Err(CheckpointUnavailable)),
        Heartbeat::Unavailable,
        "an unavailable store is asked again later and is not a stop"
    );
    Ok(())
}

/// A private temporary directory, removed when dropped.
struct ScratchDir(std::path::PathBuf);

impl Drop for ScratchDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn the_directory_identity_is_made_whole_once_and_leaves_no_temporary_file() -> TestResult {
    use std::os::unix::fs::DirBuilderExt;
    let path = std::env::temp_dir().join(format!("artifree-identity-{}", std::process::id()));
    if path.exists() {
        fs::remove_dir_all(&path)?;
    }
    fs::DirBuilder::new().mode(0o700).create(&path)?;
    let scratch = ScratchDir(path);
    let identity_path = scratch.0.join(DIRECTORY_IDENTITY_FILE);
    create_identity(&scratch.0, &identity_path)?;
    let first = fs::read_to_string(&identity_path)?;
    create_identity(&scratch.0, &identity_path)?;
    assert_eq!(
        fs::read_to_string(&identity_path)?,
        first,
        "a second creation keeps the first identity"
    );
    assert_eq!(first.len(), 33, "32 hexadecimal digits and a newline");
    let names: Vec<String> = fs::read_dir(&scratch.0)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()?;
    assert_eq!(
        names,
        vec![DIRECTORY_IDENTITY_FILE.to_owned()],
        "no temporary file is left"
    );
    Ok(())
}
