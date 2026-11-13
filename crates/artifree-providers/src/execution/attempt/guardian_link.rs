//! The provider's side of the guardian (decision 0056, amendment of 2026-11-13, the guardian).
//!
//! Compiled only into the test build, with the attempt module (0056 rule 16). The guardian is a
//! child of the provider and stays outside the attempt group. Its standard input is one end of a
//! Unix stream pair whose other end only the provider holds; both ends close on `exec`, and the
//! spawn gives the guardian its own copy of the child's end as standard input. Its standard output
//! is null. The provider never sends the guardian a credential, the input or the output: only the
//! records of the guardian's protocol cross the socket.

use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::super::admission::wait_within;
use super::super::guardian::{self, GuardianReason};
use super::super::{launcher, probe_group};
use super::{AttemptFailure, NotStartedReason, quarantine};

/// The most bytes of the guardian's output that are collected. The report line is far shorter, so
/// anything after the first 256 bytes is ignored.
const REPORT_BYTES: usize = 256;

/// The provider's handle on the guardian of one attempt: its child process and the provider's end
/// of the socket that is the child's standard input.
#[derive(Debug)]
pub(super) struct GuardianLink {
    /// The guardian process. `finish` reaps it, and kills it first when it is still running.
    child: Child,
    /// The provider's end of the socket. The records are written here, and the report is read back
    /// from here.
    socket: UnixStream,
}

impl GuardianLink {
    /// Starts the guardian of the attempt group `name` and waits for its ready byte.
    ///
    /// `deadline` and `window` are passed to the guardian as whole milliseconds from its start. The
    /// ready byte is awaited for at most `readiness`, the plan's readiness deadline.
    /// The command holds the child's end of the socket, so it is dropped right after the spawn,
    /// and only the guardian keeps that end.
    ///
    /// # Errors
    ///
    /// Returns `NotStarted(GuardianNotStarted)` when the socket or the child cannot be made, when a
    /// duration does not fit in milliseconds, or when the guardian does not write its ready byte
    /// within `readiness`. A guardian that does not report ready is killed and waited
    /// for. When it is not reaped within the cleanup deadline, the provider is also quarantined in
    /// its process.
    pub(super) fn start(
        tool_root: &Path,
        name: &str,
        deadline: Duration,
        window: Duration,
        readiness: Duration,
    ) -> Result<Self, AttemptFailure> {
        let not_started = AttemptFailure::NotStarted(NotStartedReason::GuardianNotStarted);
        let deadline_ms = u64::try_from(deadline.as_millis()).map_err(|_| not_started)?;
        let window_ms = u64::try_from(window.as_millis()).map_err(|_| not_started)?;
        let (mut socket, child_end) = UnixStream::pair().map_err(|_| not_started)?;
        let spawned = {
            let mut command = Command::new(tool_root.join(launcher::FIXTURE_FILE));
            command
                .arg("guardian")
                .arg(name)
                .arg(deadline_ms.to_string())
                .arg(window_ms.to_string())
                .env_clear()
                .stdin(Stdio::from(OwnedFd::from(child_end)))
                .stdout(Stdio::null())
                .stderr(Stdio::inherit());
            command.spawn()
        };
        let mut child = spawned.map_err(|_| not_started)?;
        if read_ready_byte(&mut socket, readiness) {
            return Ok(Self { child, socket });
        }
        // A guardian that is still running after the kill stays in `supervisor`, so the provider
        // is quarantined in its process.
        child.kill().ok();
        if wait_within(&mut child, probe_group::CLEANUP_DEADLINE).is_err() {
            quarantine();
        }
        Err(not_started)
    }

    /// Writes one record to the guardian. A failed write means that the guardian can no longer read
    /// the socket. Its report and its exit then decide the attempt, not this error.
    pub(super) fn send(&mut self, record: u8) -> io::Result<()> {
        self.socket.write_all(&[record])
    }

    /// Shuts down the provider's writing side of the socket, so that the guardian reads end of file
    /// without `D` and ends the attempt. The reading side stays open for the report. A failed
    /// shutdown leaves the guardian to its window and deadline, and `finish` still bounds the wait.
    pub(super) fn close_records(&mut self) {
        self.socket.shutdown(std::net::Shutdown::Write).ok();
    }

    /// Reads the guardian's report and reaps the guardian. The caller calls it after `D` was sent
    /// or after `close_records`.
    ///
    /// The socket is read until its end, until `REPORT_BYTES` have arrived, or until `within` has
    /// passed. The first line of what was read is parsed as the report. The guardian is then waited
    /// for within the rest of `within`, and a guardian that is still running is killed and waited
    /// for.
    ///
    /// Returns the report, when the first line is a valid one, and whether the guardian was reaped
    /// without a kill. A `false` second value means the guardian was not observed gone, which the
    /// caller refuses as `CleanupNotObserved`.
    #[must_use]
    pub(super) fn finish(mut self, within: Duration) -> (Option<(GuardianReason, bool)>, bool) {
        let started = Instant::now();
        let mut received: Vec<u8> = Vec::new();
        let mut chunk = [0u8; REPORT_BYTES];
        while received.len() < REPORT_BYTES {
            let remaining = within.saturating_sub(started.elapsed());
            if remaining.is_zero() || self.socket.set_read_timeout(Some(remaining)).is_err() {
                break;
            }
            let count = match self.socket.read(&mut chunk) {
                Ok(count) if count > 0 => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Ok(_) | Err(_) => break,
            };
            let room = REPORT_BYTES.saturating_sub(received.len());
            if let Some(taken) = chunk.get(..count.min(room)) {
                received.extend_from_slice(taken);
            }
        }
        // The report is the first line of what was read. Bytes that are not UTF-8 are no report.
        let report = received
            .split(|&byte| byte == b'\n')
            .next()
            .and_then(|line| std::str::from_utf8(line).ok())
            .and_then(guardian::parse_report);
        let reaped = wait_within(&mut self.child, within.saturating_sub(started.elapsed())).is_ok();
        if !reaped {
            // The kill and its wait may fail. Either way `reaped` stays false, which is what the
            // caller reads, so the outcome of the kill needs no other handling here.
            self.child.kill().ok();
            wait_within(&mut self.child, probe_group::CLEANUP_DEADLINE).ok();
        }
        (report, reaped)
    }
}

/// Reads the guardian's ready byte within `within`, counted from the call. Any other byte, an end
/// of stream, a read error or a passed deadline means that the guardian is not ready.
fn read_ready_byte(socket: &mut UnixStream, within: Duration) -> bool {
    let started = Instant::now();
    let mut byte = [0u8; 1];
    loop {
        let Some(remaining) = within
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
        else {
            return false;
        };
        if socket.set_read_timeout(Some(remaining)).is_err() {
            return false;
        }
        match socket.read(&mut byte) {
            Ok(1) => return byte == [guardian::READY_BYTE],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Ok(_) | Err(_) => return false,
        }
    }
}

#[cfg(test)]
mod tests;
