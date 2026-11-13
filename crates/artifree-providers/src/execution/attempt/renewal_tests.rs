//! Unit tests of the provider's renewal decision (decision 0056, amendment of 2026-11-13, the
//! guardian): when a heartbeat is due and what each answer does.

use std::error::Error;
use std::time::{Duration, Instant};

use super::{Heartbeat, LeaseLoss, heartbeat_due, on_answer};

/// The window of the schedule table. A heartbeat is due once half of it has passed.
const WINDOW: Duration = Duration::from_secs(1);

/// A heartbeat is due at the first poll, is not due again until half a window has passed since the
/// last one asked, and a last heartbeat recorded in the future never makes one due.
#[test]
fn heartbeat_schedule_table() -> Result<(), Box<dyn Error>> {
    let now = Instant::now();
    let half_window = WINDOW / 2;
    // The expected values are written out by hand, not derived from heartbeat_due.
    let cases: [(&str, Option<Instant>, bool); 6] = [
        ("no heartbeat asked yet", None, true),
        ("a heartbeat asked just now", Some(now), false),
        (
            "a heartbeat asked 499 ms ago",
            Some(
                now.checked_sub(Duration::from_millis(499))
                    .ok_or("clock too early")?,
            ),
            false,
        ),
        (
            "a heartbeat asked half a window ago",
            Some(now.checked_sub(half_window).ok_or("clock too early")?),
            true,
        ),
        (
            "a heartbeat asked two windows ago",
            Some(now.checked_sub(WINDOW * 2).ok_or("clock too early")?),
            true,
        ),
        (
            "a heartbeat recorded one window later",
            Some(now.checked_add(WINDOW).ok_or("clock overflow")?),
            false,
        ),
    ];
    for (label, last_asked, expected) in cases {
        assert_eq!(
            heartbeat_due(last_asked, now, WINDOW),
            expected,
            "heartbeat_due for {label}"
        );
    }
    Ok(())
}

/// A confirmation renews, an unavailable store sends nothing, every lost part stops the renewals
/// with that part, and `renew_regardless` renews on every answer.
#[test]
fn answer_table() {
    // The expected values are written out by hand, not derived from on_answer.
    let cases: [(&str, Heartbeat, bool, Result<bool, LeaseLoss>); 12] = [
        ("confirmed", Heartbeat::Confirmed, false, Ok(true)),
        ("unavailable", Heartbeat::Unavailable, false, Ok(false)),
        (
            "lost fence",
            Heartbeat::Lost(LeaseLoss::Fence),
            false,
            Err(LeaseLoss::Fence),
        ),
        (
            "lost cancel",
            Heartbeat::Lost(LeaseLoss::Cancelled),
            false,
            Err(LeaseLoss::Cancelled),
        ),
        (
            "lost generation",
            Heartbeat::Lost(LeaseLoss::Generation),
            false,
            Err(LeaseLoss::Generation),
        ),
        (
            "lost authority",
            Heartbeat::Lost(LeaseLoss::Authority),
            false,
            Err(LeaseLoss::Authority),
        ),
        (
            "confirmed, renew regardless",
            Heartbeat::Confirmed,
            true,
            Ok(true),
        ),
        (
            "unavailable, renew regardless",
            Heartbeat::Unavailable,
            true,
            Ok(true),
        ),
        (
            "lost fence, renew regardless",
            Heartbeat::Lost(LeaseLoss::Fence),
            true,
            Ok(true),
        ),
        (
            "lost cancel, renew regardless",
            Heartbeat::Lost(LeaseLoss::Cancelled),
            true,
            Ok(true),
        ),
        (
            "lost generation, renew regardless",
            Heartbeat::Lost(LeaseLoss::Generation),
            true,
            Ok(true),
        ),
        (
            "lost authority, renew regardless",
            Heartbeat::Lost(LeaseLoss::Authority),
            true,
            Ok(true),
        ),
    ];
    for (label, answer, renew_regardless, expected) in cases {
        assert_eq!(
            on_answer(answer, renew_regardless),
            expected,
            "on_answer for {label}"
        );
    }
}
