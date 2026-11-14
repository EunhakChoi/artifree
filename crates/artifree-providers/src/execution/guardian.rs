//! The guardian of one attempt (decision 0056, amendment of 2026-11-13, the guardian).
//!
//! The guardian stays in `supervisor`, outside the attempt group, and is the provider's child.
//! Its entry point is the test-support program, like the stub's (0056 rule 4). The provider
//! writes `L` at the release, `R` for each renewal, `S` once it stops polling the stub and `D`
//! once its group is gone. The guardian answers with the ready byte `G` once it holds the attempt
//! group.
//!
//! The amendment's rules, which the state machine here applies: "End of file without `D`, from
//! the supervisor's death or a closed socket, ends the attempt at any time, and so does the
//! absolute deadline. Between `L` and `S`, the renewal window passing without an `R` ends it too."
//! "No renewal, record or live supervisor extends the deadline." "It removes nothing": a group
//! left by a dead supervisor stays for the reconciliation of R02ai.
//!
//! Times are milliseconds. The deadline is measured from the guardian's own start, on
//! `std::time::Instant` (`CLOCK_MONOTONIC` on Linux), rather than as an absolute monotonic value,
//! so that the time feature of `rustix` is not needed.

use std::fmt;
use std::io::Write as _;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::{Mode, OFlags, ResolveFlags};
use rustix::io::Errno;
use rustix::stdio;

use super::{cgroup, probe_group};

/// The byte the guardian writes on its socket once it holds the attempt group.
pub(super) const READY_BYTE: u8 = b'G';
/// The provider's record at the release: the renewal window starts.
pub(super) const LEASE: u8 = b'L';
/// The provider's record after a confirming heartbeat.
pub(super) const RENEW: u8 = b'R';
/// The provider's record once it stops polling the stub.
pub(super) const STOP: u8 = b'S';
/// The provider's record once its group is gone.
pub(super) const DONE: u8 = b'D';

/// Why the guardian could not start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GuardianFailure {
    /// The attempt group's name is not of the form `attempt-<digits>-<digits>`.
    Name,
    /// Its own group, its parent or the attempt group beneath it could not be opened.
    Group,
    /// Descriptor 0 could not be made non-blocking or the ready byte could not be written.
    Socket,
}

impl fmt::Display for GuardianFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name => {
                formatter.write_str("the attempt group name is not of the provider's form")
            }
            Self::Group => formatter
                .write_str("the guardian's cgroup or the attempt group could not be opened"),
            Self::Socket => formatter.write_str("the guardian's socket could not be readied"),
        }
    }
}

impl std::error::Error for GuardianFailure {}

/// Why the guardian ends an attempt. The word of each reason is the one its report line names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GuardianReason {
    /// End of file without `D`: the supervisor died or the socket closed.
    SupervisorGone,
    /// The renewal window passed between `L` and `S` without an `R`.
    RenewalExpired,
    /// The absolute deadline passed.
    Deadline,
}

impl GuardianReason {
    /// The word that names this reason in the report line.
    #[must_use]
    pub(super) fn word(self) -> &'static str {
        match self {
            Self::SupervisorGone => "supervisor-gone",
            Self::RenewalExpired => "renewal-expired",
            Self::Deadline => "deadline",
        }
    }
}

/// Whether `name` is `attempt-<digits>-<digits>`: both digit runs are non-empty and hold ASCII
/// digits only, and nothing else follows the prefix.
#[must_use]
fn is_attempt_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("attempt-") else {
        return false;
    };
    let Some((first, second)) = rest.split_once('-') else {
        return false;
    };
    is_digit_run(first) && is_digit_run(second)
}

/// Whether `text` is one or more ASCII digits.
#[must_use]
fn is_digit_run(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// The guardian's view of one attempt. Every time is in milliseconds from the guardian's start.
#[derive(Clone, Debug, Eq, PartialEq)]
struct GuardianState {
    /// The absolute deadline; reaching it ends the attempt at any time.
    deadline_ms: u64,
    /// The renewal window: while `renewing`, an `R` must arrive within this many milliseconds.
    window_ms: u64,
    /// Whether the window runs, from `L` until `S`.
    renewing: bool,
    /// When the last `L` or the last `R` during the window arrived.
    last_renewal_ms: u64,
    /// Whether the provider has written `D`.
    done: bool,
}

impl GuardianState {
    /// Starts with no window running and no `D` received.
    #[must_use]
    fn new(deadline_ms: u64, window_ms: u64) -> Self {
        Self {
            deadline_ms,
            window_ms,
            renewing: false,
            last_renewal_ms: 0,
            done: false,
        }
    }

    /// Applies one record that arrived at `now_ms`. Returns `false` for any byte that is not one of
    /// the four records of the protocol; the caller treats that as end of file.
    #[must_use]
    fn on_record(&mut self, byte: u8, now_ms: u64) -> bool {
        match byte {
            LEASE => {
                self.renewing = true;
                self.last_renewal_ms = now_ms;
            }
            // An `R` before `L` starts no window, so it is not recorded.
            RENEW => {
                if self.renewing {
                    self.last_renewal_ms = now_ms;
                }
            }
            STOP => self.renewing = false,
            DONE => self.done = true,
            _ => return false,
        }
        true
    }

    /// The reason that ends the attempt at `now_ms`, if any. The deadline is checked first, then
    /// end of file without `D`, then an expired window.
    #[must_use]
    fn decide(&self, now_ms: u64, eof: bool) -> Option<GuardianReason> {
        if now_ms >= self.deadline_ms {
            return Some(GuardianReason::Deadline);
        }
        if eof && !self.done {
            return Some(GuardianReason::SupervisorGone);
        }
        if self.renewing && now_ms.saturating_sub(self.last_renewal_ms) >= self.window_ms {
            return Some(GuardianReason::RenewalExpired);
        }
        None
    }
}

/// The line the guardian writes when it ends the attempt: `guardian <reason> emptied=<0|1>`, with a
/// trailing newline.
#[must_use]
fn report_line(reason: GuardianReason, emptied: bool) -> String {
    format!("guardian {} emptied={}\n", reason.word(), u8::from(emptied))
}

// The parser serves only the provider's launch, which runs in the test build and in the
// enforcement-fixture build (decision 0057 rule 2).
/// Parses a line that [`report_line`] wrote, with or without its trailing newline. Returns `None`
/// for any other text.
#[cfg(any(test, feature = "enforcement-fixture"))]
#[must_use]
pub(super) fn parse_report(line: &str) -> Option<(GuardianReason, bool)> {
    let body = line.strip_suffix('\n').unwrap_or(line);
    let rest = body.strip_prefix("guardian ")?;
    let (word, emptied) = rest.split_once(" emptied=")?;
    let reason = [
        GuardianReason::SupervisorGone,
        GuardianReason::RenewalExpired,
        GuardianReason::Deadline,
    ]
    .into_iter()
    .find(|reason| reason.word() == word)?;
    let emptied = match emptied {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    Some((reason, emptied))
}

/// How often the guardian reads its socket and checks its clocks.
const POLL: Duration = Duration::from_millis(10);
/// The guardian's own cgroup, as the kernel reports it.
const PROC_SELF_CGROUP: &str = "/proc/self/cgroup";
/// Where the unified cgroup hierarchy is mounted.
const CGROUP_MOUNT: &str = "/sys/fs/cgroup";
/// The most of `cgroup.events` that is read; the file holds a few short fields.
const EVENTS_BYTES: usize = 4096;
/// How many socket bytes are read at a time.
const RECORD_CHUNK: usize = 64;

/// Runs the guardian of the attempt group `name` (decision 0056, amendment of 2026-11-13, the
/// guardian).
///
/// The guardian writes its ready byte on descriptor 0 once it holds the attempt group, then reads
/// the provider's records every 10 ms until it ends the attempt or the provider writes `D`.
/// `deadline_ms` is the absolute deadline, in milliseconds from this call; nothing extends it.
/// `window_ms` is the renewal window. After a decision it ends the attempt, writes its report line
/// and returns `Ok`. After `D` it returns `Ok` without a line.
///
/// # Errors
///
/// Returns `GuardianFailure::Name` for a name that is not `attempt-<digits>-<digits>`,
/// `GuardianFailure::Group` when its own group, the parent of that group or the attempt group
/// beneath that parent cannot be opened, and `GuardianFailure::Socket` when descriptor 0 cannot be
/// made non-blocking or the ready byte cannot be written.
pub fn run_guardian(name: &str, deadline_ms: u64, window_ms: u64) -> Result<(), GuardianFailure> {
    let started = Instant::now();
    if !is_attempt_name(name) {
        return Err(GuardianFailure::Name);
    }
    let group = open_group(name)?;
    let socket = stdio::stdin();
    let flags = rustix::fs::fcntl_getfl(socket).map_err(|_| GuardianFailure::Socket)?;
    rustix::fs::fcntl_setfl(socket, flags | OFlags::NONBLOCK)
        .map_err(|_| GuardianFailure::Socket)?;
    if rustix::io::write(socket, &[READY_BYTE]) != Ok(1) {
        return Err(GuardianFailure::Socket);
    }
    let mut state = GuardianState::new(deadline_ms, window_ms);
    loop {
        let eof = read_records(socket, &mut state, started);
        if state.done {
            return Ok(());
        }
        if let Some(reason) = state.decide(elapsed_ms(started), eof) {
            end_attempt(&group, reason);
            return Ok(());
        }
        thread::sleep(POLL);
    }
}

/// Opens the attempt group `name` beneath the parent of the guardian's own group, where the
/// provider places it beside `supervisor`. The lookup stays beneath that parent and follows no
/// symbolic or magic link.
fn open_group(name: &str) -> Result<OwnedFd, GuardianFailure> {
    let own_group =
        cgroup::discover_own_group(Path::new(PROC_SELF_CGROUP), Path::new(CGROUP_MOUNT))
            .map_err(|_| GuardianFailure::Group)?;
    let parent = own_group.parent().ok_or(GuardianFailure::Group)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let parent_dir =
        rustix::fs::open(parent, flags, Mode::empty()).map_err(|_| GuardianFailure::Group)?;
    rustix::fs::openat2(
        &parent_dir,
        name,
        flags,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    )
    .map_err(|_| GuardianFailure::Group)
}

/// Milliseconds elapsed since `started`, saturating at `u64::MAX`.
fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Applies at most one chunk of the records that are available on `socket` to `state`, so that a
/// peer that keeps writing cannot keep the caller from deciding by the time. Returns `false` when
/// the chunk was applied or the socket has no data for now, and `true` at end of file, at any other
/// read error and at a byte that is not a record of the protocol.
fn read_records(socket: BorrowedFd<'_>, state: &mut GuardianState, started: Instant) -> bool {
    let mut chunk = [0u8; RECORD_CHUNK];
    let count = loop {
        match rustix::io::read(socket, &mut chunk) {
            Err(Errno::INTR) => {}
            Err(Errno::AGAIN) => return false,
            Err(_) | Ok(0) => return true,
            Ok(count) => break count,
        }
    };
    for byte in chunk.get(..count).unwrap_or_default() {
        if !state.on_record(*byte, elapsed_ms(started)) {
            return true;
        }
        if state.done {
            return false;
        }
    }
    false
}

/// Ends the attempt for `reason`: writes `1` to the group's `cgroup.kill`, waits for the group to
/// be seen empty, then writes the report line. The line goes first on the standard error the
/// provider passes on, so that it survives a dead supervisor, and then on the socket. A failure of
/// either write is ignored. The group is never removed.
fn end_attempt(group: &OwnedFd, reason: GuardianReason) {
    let kill_flags = OFlags::WRONLY | OFlags::CLOEXEC;
    if let Ok(kill) = rustix::fs::openat(group, "cgroup.kill", kill_flags, Mode::empty()) {
        rustix::io::write(&kill, b"1").ok();
    }
    let emptied = await_emptied(group);
    let line = report_line(reason, emptied);
    std::io::stderr().write_all(line.as_bytes()).ok();
    rustix::io::write(stdio::stdin(), line.as_bytes()).ok();
}

/// Whether the group is seen with `populated 0` within the cleanup observation deadline. The
/// deadline is measured from this call, which follows the kill.
fn await_emptied(group: &OwnedFd) -> bool {
    let since = Instant::now();
    loop {
        if read_populated(group) == Some(false) {
            return true;
        }
        if since.elapsed() >= probe_group::CLEANUP_DEADLINE {
            return false;
        }
        thread::sleep(POLL);
    }
}

/// Reads the `populated` field of the group's `cgroup.events`. A group whose file is gone was
/// removed, which the kernel allows only for an empty group, so it reads as `Some(false)`. Returns
/// `None` when the file cannot be read otherwise or does not parse.
fn read_populated(group: &OwnedFd) -> Option<bool> {
    let events_flags = OFlags::RDONLY | OFlags::CLOEXEC;
    let events = match rustix::fs::openat(group, "cgroup.events", events_flags, Mode::empty()) {
        Ok(events) => events,
        Err(Errno::NOENT) => return Some(false),
        Err(_) => return None,
    };
    let mut buffer = [0u8; EVENTS_BYTES];
    let count = rustix::io::read(&events, &mut buffer).ok()?;
    probe_group::parse_populated(buffer.get(..count)?)
}

#[cfg(test)]
mod tests;
