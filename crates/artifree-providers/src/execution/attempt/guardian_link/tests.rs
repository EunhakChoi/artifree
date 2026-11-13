//! The provider's refusal of a guardian that does not report ready (decision 0056, amendment of
//! 2026-11-13, the guardian). These tests start no guardian and no payload: they use a socket pair
//! in the test process, and a tool root that does not exist.

use std::error::Error;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use super::super::super::guardian::READY_BYTE;
use super::super::{AttemptFailure, NotStartedReason};
use super::{GuardianLink, read_ready_byte};

/// A readiness bound that is short for a test and long for a write that has already happened.
const WITHIN: Duration = Duration::from_millis(200);

#[test]
fn the_ready_byte_is_accepted() -> Result<(), Box<dyn Error>> {
    let (mut provider, mut guardian) = UnixStream::pair()?;
    guardian.write_all(&[READY_BYTE])?;
    assert!(read_ready_byte(&mut provider, WITHIN));
    Ok(())
}

#[test]
fn another_byte_is_not_ready() -> Result<(), Box<dyn Error>> {
    let (mut provider, mut guardian) = UnixStream::pair()?;
    guardian.write_all(&[READY_BYTE.wrapping_add(1)])?;
    assert!(!read_ready_byte(&mut provider, WITHIN));
    Ok(())
}

#[test]
fn an_end_of_stream_is_not_ready() -> Result<(), Box<dyn Error>> {
    let (mut provider, guardian) = UnixStream::pair()?;
    drop(guardian);
    assert!(!read_ready_byte(&mut provider, WITHIN));
    Ok(())
}

#[test]
fn silence_is_not_ready_once_the_bound_passes() -> Result<(), Box<dyn Error>> {
    let (mut provider, _guardian) = UnixStream::pair()?;
    let started = Instant::now();
    assert!(!read_ready_byte(&mut provider, WITHIN));
    let elapsed = started.elapsed();
    assert!(
        (WITHIN..WITHIN.saturating_mul(10)).contains(&elapsed),
        "the wait must end at its bound, took {elapsed:?}"
    );
    Ok(())
}

#[test]
fn a_guardian_that_cannot_start_refuses_the_attempt() {
    // No such directory exists, so the spawn of its fixture program fails.
    let outcome = GuardianLink::start(
        Path::new("/nonexistent/artifree-tool-root"),
        "attempt-1-1",
        Duration::from_secs(1),
        Duration::from_secs(1),
        WITHIN,
    );
    assert_eq!(
        outcome.err(),
        Some(AttemptFailure::NotStarted(
            NotStartedReason::GuardianNotStarted
        ))
    );
}
