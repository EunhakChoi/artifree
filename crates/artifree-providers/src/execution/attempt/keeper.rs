//! The keeper: the provider's handles on an attempt's scratch, held outside the payload group
//! (decision 0042, rule 7; decision 0056, amendment of 2026-11-13, input and output).
//!
//! Compiled only into the test build (0056 rule 16). Between its observation and the release, the
//! provider opens the keeper, checks it and makes the output root. It collects only after a passing
//! verdict, and drops the keeper on every path (0056 rule 5). The keeper opens the scratch through
//! `/proc/<bootstrap>/root/tmp`, close-on-exec, and keeps only the output root's handle: the
//! descriptor that holds the tmpfs mounted until collection or rejection ends.

use std::os::fd::OwnedFd;
use std::path::Path;

use rustix::fs::{self, AtFlags, Mode, OFlags, StatxFlags};

use super::super::collection::{self, CollectedOutput, CollectionRefusal, ManifestBinding};
use super::super::readiness::{self, GroupObservation};
use super::super::staging::RESOLVE;
use super::NotStartedReason;

/// The filesystem type that `fstatfs` reports for a tmpfs (`TMPFS_MAGIC`); the scratch must be one.
const TMPFS_MAGIC: i64 = 0x0102_1994;

/// The output root's name in the scratch, which the payload sees as `/tmp/out`.
const OUTPUT_ROOT: &str = "out";

/// The provider's handle on the output root of one attempt. Dropping it, after collection or
/// instead of it, frees the scratch's tmpfs (0056, amendment of 2026-11-13, input and output).
#[derive(Debug)]
pub(super) struct Keeper {
    /// The output root, opened beneath the scratch with the four resolve flags.
    output_root: OwnedFd,
}

impl Keeper {
    /// Opens the scratch of the bootstrap that `observation` describes, checks it, and makes the
    /// output root through it.
    ///
    /// The scratch is opened as `<proc_root>/<bootstrap_pid>/root/tmp`, close-on-exec. It must be a
    /// tmpfs whose mount id is the one that `observation` read from the scratch line. The group's
    /// members are then read again, and the bootstrap must still be among them in filter mode. The
    /// output root `out` is made with mode `0700` through the scratch handle, which is dropped
    /// after that. The keeper keeps only a handle on the output root.
    ///
    /// # Errors
    ///
    /// Returns `NotStartedReason::KeeperNotHeld` when the scratch cannot be opened, is not a tmpfs
    /// with the observed mount id, fails the member check, or the output root cannot be made or
    /// opened.
    pub(super) fn open(
        proc_root: &Path,
        group: &Path,
        observation: &GroupObservation,
    ) -> Result<Self, NotStartedReason> {
        let not_held = NotStartedReason::KeeperNotHeld;
        let scratch_path = proc_root
            .join(observation.bootstrap_pid.to_string())
            .join("root")
            .join("tmp");
        let scratch = fs::open(
            &scratch_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| not_held)?;
        let filesystem = fs::fstatfs(&scratch).map_err(|_| not_held)?;
        let mount = fs::statx(&scratch, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)
            .map_err(|_| not_held)?;
        if !is_checked_scratch(
            filesystem.f_type,
            mount.stx_mnt_id,
            observation.scratch.mount_id,
        ) {
            return Err(not_held);
        }
        readiness::confirm_member(group, proc_root, observation.bootstrap_pid)
            .map_err(|_| not_held)?;
        fs::mkdirat(&scratch, OUTPUT_ROOT, Mode::RWXU).map_err(|_| not_held)?;
        // The scratch handle is the last use of `scratch`, so it is moved here and closed when
        // this call returns; the output root's handle is the only one that the keeper keeps.
        let output_root = fs::openat2(
            scratch,
            OUTPUT_ROOT,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            RESOLVE,
        )
        .map_err(|_| not_held)?;
        Ok(Self { output_root })
    }

    /// The output root's handle, which the plan's test-only plant step writes through before the
    /// release.
    #[must_use]
    pub(super) fn output_root(&self) -> &OwnedFd {
        &self.output_root
    }

    /// Collects the output root and then drops the keeper, which frees the tmpfs. The caller
    /// collects only after the attempt's verdict has passed (0056, amendment of 2026-11-13, input
    /// and output).
    ///
    /// # Errors
    ///
    /// Returns the first refusal that the traversal finds; nothing is collected then.
    pub(super) fn collect(
        self,
        binding: &ManifestBinding,
    ) -> Result<CollectedOutput, CollectionRefusal> {
        let output = collection::collect(&self.output_root, binding);
        drop(self);
        output
    }
}

/// Whether a descriptor is on a tmpfs with the mount id that the observation read from the scratch
/// line. A reused PID or a reused mount id fails here or at the member check.
fn is_checked_scratch(filesystem_type: i64, mount_id: u64, scratch_mount_id: u64) -> bool {
    filesystem_type == TMPFS_MAGIC && mount_id == scratch_mount_id
}

#[cfg(test)]
mod tests;
