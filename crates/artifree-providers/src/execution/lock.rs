//! One untrusted attempt at a time per deployment, by a lock the kernel releases when the holder dies (decision 0042 rule 5).

use std::fs;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use super::cgroup;

/// The lock file inside the execution directory.
pub(super) const LOCK_FILE: &str = "admission.lock";

/// Why the admission lock was not acquired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LockRefusal {
    /// The directory or the lock file cannot be used: a missing directory, a symlink or other
    /// non-directory, an account other than the provider's, a group or other permission, or a
    /// lock error that is not contention.
    Unusable,
    /// Another process holds the lock, so an attempt is already running.
    Busy,
}

/// A held admission lock. The kernel releases it when the holding process exits, even after a crash.
#[derive(Debug)]
pub(super) struct AdmissionLock {
    /// Held open only to keep the lock; the file is never read or written after acquisition.
    _held: fs::File,
}

impl AdmissionLock {
    /// Takes the lock in `execution_dir` without waiting. The lock lasts while the returned value lives.
    ///
    /// # Errors
    ///
    /// Returns [`LockRefusal::Busy`] when another process holds the lock, and
    /// [`LockRefusal::Unusable`] when the directory is not a private directory of this account
    /// or the lock file cannot be opened or locked for another reason. `Busy` can also be the
    /// answer for a moment after this process released the lock: a child that another thread
    /// forked meanwhile holds a copy of the locked descriptor until its `exec` closes it.
    pub(super) fn acquire(execution_dir: &Path) -> Result<Self, LockRefusal> {
        let uid = cgroup::own_uid(Path::new("/proc/self")).map_err(|_| LockRefusal::Unusable)?;
        let directory = fs::symlink_metadata(execution_dir).map_err(|_| LockRefusal::Unusable)?;
        if !directory.file_type().is_dir()
            || directory.uid() != uid
            || directory.mode() & 0o077 != 0
        {
            return Err(LockRefusal::Unusable);
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(execution_dir.join(LOCK_FILE))
            .map_err(|_| LockRefusal::Unusable)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _held: file }),
            Err(fs::TryLockError::WouldBlock) => Err(LockRefusal::Busy),
            Err(fs::TryLockError::Error(_)) => Err(LockRefusal::Unusable),
        }
    }
}

#[cfg(test)]
mod tests;
