//! The keeper: the provider's handles on an attempt's scratch, held outside the payload group
//! (decision 0042, rule 7; decision 0056, amendment of 2026-11-13, input and output).
//!
//! Compiled only into the test build (0056 rule 16). Between its observation and the release, the
//! provider opens the keeper, checks it and makes the output root. It collects only after a passing
//! verdict, and drops the keeper on every path (0056 rule 5). The keeper opens the scratch through
//! `/proc/<bootstrap>/root/tmp`, close-on-exec, and keeps that handle beside the output root's:
//! both hold the tmpfs mounted until collection or rejection ends, and both are dropped then.

use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;

use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags, StatxFlags};

use super::super::collection::{self, CollectedOutput, CollectionRefusal, ManifestBinding};
use super::super::readiness::{self, GroupObservation};
use super::super::staging::RESOLVE;
use super::NotStartedReason;

/// The filesystem type that `fstatfs` reports for a tmpfs (`TMPFS_MAGIC`); the scratch must be one.
const TMPFS_MAGIC: i64 = 0x0102_1994;

/// The output root's name in the scratch, which the payload sees as `/tmp/out`.
const OUTPUT_ROOT: &str = "out";

/// The most entries that `list_scratch` reads: the scratch's inode bound (decision 0042 rule 5).
const LIST_ENTRIES: usize = 4_096;

/// The deepest directory level below the scratch root that `list_scratch` enters.
const LIST_DEPTH: usize = 8;

/// The provider's handles on the scratch and on the output root of one attempt. The provider drops
/// both handles when the attempt ends, which frees the tmpfs (0056, amendment of 2026-11-13, input
/// and output; crashes).
#[derive(Debug)]
pub(super) struct Keeper {
    /// The scratch root, opened through `/proc/<bootstrap>/root/tmp` and checked; kept so that the
    /// test-only listing can read it after the group is empty (0056, amendment of 2026-11-13,
    /// crashes).
    scratch: OwnedFd,
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
    /// output root `out` is made with mode `0700` through the scratch handle, and the keeper keeps
    /// the scratch handle beside the output root's. The provider drops both handles when the
    /// attempt ends, which frees the tmpfs.
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
        let output_root = fs::openat2(
            &scratch,
            OUTPUT_ROOT,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            RESOLVE,
        )
        .map_err(|_| not_held)?;
        Ok(Self {
            scratch,
            output_root,
        })
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

    /// Lists the whole scratch through the kept handle, after the group is empty and before the
    /// keeper is dropped (0056, amendment of 2026-11-13, crashes). Each entry is one line
    /// `<path relative to the scratch>\t<kind>\t<size>`, and the lines are sorted by path. The kind
    /// is `directory`, `file`, `link`, `socket`, `fifo`, `device` or `other`, and the size is the
    /// `st_size` of the entry itself. Only names, types and sizes are read, never file contents,
    /// and the walk uses the staging's `RESOLVE` flags.
    ///
    /// # Errors
    ///
    /// Returns the first I/O error, or an error when the scratch holds more than `LIST_ENTRIES`
    /// entries or a directory deeper than `LIST_DEPTH`.
    pub(super) fn list_scratch(&self) -> io::Result<Vec<String>> {
        let mut entries = Vec::new();
        list_directory(&self.scratch, "", 0, &mut entries)?;
        // A pair sorts by its path first; the line only breaks a tie between names that show alike.
        entries.sort();
        Ok(entries.into_iter().map(|(_, line)| line).collect())
    }
}

/// Whether a descriptor is on a tmpfs with the mount id that the observation read from the scratch
/// line. A reused PID or a reused mount id fails here or at the member check.
fn is_checked_scratch(filesystem_type: i64, mount_id: u64, scratch_mount_id: u64) -> bool {
    filesystem_type == TMPFS_MAGIC && mount_id == scratch_mount_id
}

/// Adds one entry for each name in `directory`, a directory `level` levels below the scratch root
/// whose path is `prefix` (empty at the root), and walks each subdirectory depth first.
///
/// # Errors
///
/// Returns the first I/O error, or an error when the listing exceeds `LIST_ENTRIES` or
/// `LIST_DEPTH`.
fn list_directory(
    directory: &OwnedFd,
    prefix: &str,
    level: usize,
    entries: &mut Vec<(String, String)>,
) -> io::Result<()> {
    // The duplicate shares its file offset with `directory`, which the keeper keeps for the next
    // walk, so the stream starts again from the first name.
    let mut names = Dir::new(directory.try_clone()?)?;
    names.rewind();
    for entry in names {
        let entry = entry?;
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        if entries.len() >= LIST_ENTRIES {
            return Err(io::Error::other(
                "the scratch holds more entries than the listing bound",
            ));
        }
        let stat = fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)?;
        let text = name.to_string_lossy();
        let path = if prefix.is_empty() {
            text.into_owned()
        } else {
            format!("{prefix}/{text}")
        };
        let kind = FileType::from_raw_mode(stat.st_mode);
        let word = kind_word(kind);
        let size = stat.st_size;
        let line = format!("{path}\t{word}\t{size}");
        if kind == FileType::Directory {
            let child_level = level + 1;
            if child_level > LIST_DEPTH {
                return Err(io::Error::other(
                    "the scratch is deeper than the listing bound",
                ));
            }
            let child = fs::openat2(
                directory,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
                RESOLVE,
            )?;
            list_directory(&child, &path, child_level, entries)?;
        }
        entries.push((path, line));
    }
    Ok(())
}

/// The word that a scratch listing gives a file type.
fn kind_word(kind: FileType) -> &'static str {
    match kind {
        FileType::Directory => "directory",
        FileType::RegularFile => "file",
        FileType::Symlink => "link",
        FileType::Socket => "socket",
        FileType::Fifo => "fifo",
        FileType::BlockDevice | FileType::CharacterDevice => "device",
        FileType::Unknown => "other",
    }
}

#[cfg(test)]
mod tests;
