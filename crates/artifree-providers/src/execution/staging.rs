//! The attempt's input: a package checked against its listing and staged read-only for the
//! launcher's `--ro-bind <path> /input` (decision 0056, amendment of 2026-11-13, input and output;
//! decision 0042 rules 3 and 5).
//!
//! Compiled only into the test build (0056 rule 16). Where the files come from (the store, access
//! checks and the pinned revision) is R02b's (0056 rule 15); this module takes the package as given.

use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, File};
use std::io::{self, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use artifree_domain::package::{FilePackage, PackagePath};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, ResolveFlags};
use sha2::{Digest, Sha256};

use super::report::hex;

/// The staging directory's name inside the execution directory.
pub(super) const INPUT_DIR: &str = "input";
/// The version tag that starts the listing's canonical text.
const LISTING_TAG: &str = "artifree-input-listing-v1";
/// The permission bits of a staged file: read only.
const FILE_MODE: Mode = Mode::from_raw_mode(0o444);
/// The permission bits of a filled staging directory: read and search only.
const DIRECTORY_MODE: Mode = Mode::from_raw_mode(0o555);
/// The owner-only permission bits of a directory that is being filled or removed.
const OWNER_MODE: Mode = Mode::from_raw_mode(0o700);
/// The resolution of every open beneath a handle: no escape by `..` or an absolute path, no
/// symbolic or magic link, and no other mount (0056, amendment of 2026-11-13).
pub(super) const RESOLVE: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_MAGICLINKS)
    .union(ResolveFlags::NO_XDEV);

/// Why an input was not staged or not removed (0056, amendment of 2026-11-13, input and output).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InputRefusal {
    /// The listing and the files differ in a path, a length or a digest.
    ListingMismatch,
    /// The staging directory could not be made, written or made read-only.
    NotStaged,
    /// A leftover staging directory of an earlier attempt could not be removed, so nothing is
    /// launched.
    LeftoverNotRemoved,
    /// The staging directory could not be removed when the attempt ended.
    NotRemoved,
}

/// Each file's path, length and SHA-256, in path order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct InputListing(pub(super) Vec<(PackagePath, u64, [u8; 32])>);

impl InputListing {
    /// The listing of the files of `package`, in path order.
    #[must_use]
    pub(super) fn of(package: &FilePackage) -> Self {
        Self(
            package
                .files()
                .map(|(path, bytes)| {
                    let sha256: [u8; 32] = Sha256::digest(bytes).into();
                    (
                        path.clone(),
                        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                        sha256,
                    )
                })
                .collect(),
        )
    }

    /// The SHA-256 digest of the listing's canonical text.
    ///
    /// The text is `artifree-input-listing-v1`, then one line `<path>\t<length>\t<sha256>` per
    /// file, in path order (0056, amendment of 2026-11-13).
    #[must_use]
    pub(super) fn digest(&self) -> [u8; 32] {
        let mut text = format!("{LISTING_TAG}\n");
        for (path, length, sha256) in &self.0 {
            let line = format!("{path}\t{length}\t{}\n", hex(*sha256));
            text.push_str(&line);
        }
        Sha256::digest(text.as_bytes()).into()
    }
}

/// A package whose files match its listing, not yet staged.
#[derive(Debug)]
pub(super) struct VerifiedInput {
    package: FilePackage,
    listing: InputListing,
}

impl VerifiedInput {
    /// Checks `package` against `listing`, which the caller took from the pinned input.
    ///
    /// # Errors
    ///
    /// Returns `ListingMismatch` when the paths, a length or a digest differ.
    pub(super) fn verify(
        package: FilePackage,
        listing: InputListing,
    ) -> Result<Self, InputRefusal> {
        if InputListing::of(&package) != listing {
            return Err(InputRefusal::ListingMismatch);
        }
        Ok(Self { package, listing })
    }

    /// Stages the files as `execution_dir/input`, read only, and drops their bytes.
    ///
    /// A leftover staging directory is removed first. A partly written tree is removed at once,
    /// and the attempt is refused either way.
    ///
    /// # Errors
    ///
    /// Returns `LeftoverNotRemoved` when a leftover cannot be removed, and `NotStaged` when the
    /// tree cannot be written or made read only.
    pub(super) fn stage(self, execution_dir: &Path) -> Result<StagedInput, InputRefusal> {
        let path = execution_dir.join(INPUT_DIR);
        remove_tree(&path).map_err(|_| InputRefusal::LeftoverNotRemoved)?;
        let Self { package, listing } = self;
        let digest = listing.digest();
        if write_tree(&path, package).is_err() {
            remove_tree(&path).ok();
            return Err(InputRefusal::NotStaged);
        }
        Ok(StagedInput {
            path,
            listing,
            digest,
            removed: false,
        })
    }
}

/// Writes `package` beneath a new directory at `path`, then makes every directory read only.
///
/// Files are created beneath a handle on the directory, without following links. Each directory
/// is then made read only through its own handle.
///
/// # Errors
///
/// Returns the first I/O error.
fn write_tree(path: &Path, package: FilePackage) -> io::Result<()> {
    DirBuilder::new().mode(0o700).create(path)?;
    let root = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    let mut directories: BTreeSet<String> = BTreeSet::new();
    for (file_path, bytes) in package.into_files() {
        let text = file_path.as_str();
        if let Some((parent, _)) = text.rsplit_once('/') {
            let mut prefix = String::new();
            for segment in parent.split('/') {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(segment);
                if directories.insert(prefix.clone()) {
                    rustix::fs::mkdirat(&root, prefix.as_str(), OWNER_MODE)?;
                }
            }
        }
        let opened = rustix::fs::openat2(
            &root,
            text,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            FILE_MODE,
            RESOLVE,
        )?;
        let mut file = File::from(opened);
        file.write_all(&bytes)?;
        // The creation mode passes through the umask; the descriptor gets the mode itself.
        rustix::fs::fchmod(&file, FILE_MODE)?;
    }
    // Each directory is opened and changed through its handle; `0555` keeps it searchable, so the
    // order does not matter. The root is changed last.
    for directory in &directories {
        let handle = rustix::fs::openat2(
            &root,
            directory.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            RESOLVE,
        )?;
        rustix::fs::fchmod(handle, DIRECTORY_MODE)?;
    }
    rustix::fs::fchmod(root, DIRECTORY_MODE)?;
    Ok(())
}

/// Removes the tree at `path`, if there is one.
///
/// Every directory is made owner-writable first, through handles and without following links,
/// since an owner cannot unlink entries in a `0555` directory. `fs::remove_dir_all` then removes
/// the tree, and it does not follow links either.
///
/// # Errors
///
/// Returns an error when a directory cannot be opened or made owner-writable, or when the tree
/// cannot be removed.
pub(super) fn remove_tree(path: &Path) -> io::Result<()> {
    let root = match rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    ) {
        Ok(root) => root,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        // A leftover file or link is unlinked; `remove_file` does not follow a link.
        Err(rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP) => return fs::remove_file(path),
        Err(error) => return Err(error.into()),
    };
    make_owner_writable(&root)?;
    // The handle is closed before the tree is removed.
    drop(root);
    fs::remove_dir_all(path)
}

/// Makes `directory` and every directory beneath it owner-writable, through handles.
///
/// # Errors
///
/// Returns the first I/O error.
fn make_owner_writable(directory: &OwnedFd) -> io::Result<()> {
    rustix::fs::fchmod(directory, OWNER_MODE)?;
    let mut children = Vec::new();
    for entry in Dir::new(directory.try_clone()?)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        let stat = rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
            children.push(name.to_owned());
        }
    }
    for name in children {
        let child = rustix::fs::openat2(
            directory,
            name.as_c_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            RESOLVE,
        )?;
        make_owner_writable(&child)?;
    }
    Ok(())
}

/// A staged input: the staging directory and the listing it was checked against. Its bytes are
/// no longer held here.
#[derive(Debug)]
pub(super) struct StagedInput {
    path: PathBuf,
    listing: InputListing,
    digest: [u8; 32],
    removed: bool,
}

impl StagedInput {
    /// The staging directory, which the launcher binds at `/input`.
    #[must_use]
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// The listing the staged files were checked against.
    #[must_use]
    pub(super) fn listing(&self) -> &InputListing {
        &self.listing
    }

    /// The digest of the listing.
    #[must_use]
    pub(super) fn digest(&self) -> [u8; 32] {
        self.digest
    }

    /// Removes the staging directory when the attempt ends (0056, amendment of 2026-11-13).
    ///
    /// # Errors
    ///
    /// Returns `NotRemoved` when the tree could not be made owner-writable or removed.
    pub(super) fn remove(mut self) -> Result<(), InputRefusal> {
        self.removed = true;
        remove_tree(&self.path).map_err(|_| InputRefusal::NotRemoved)
    }
}

impl Drop for StagedInput {
    fn drop(&mut self) {
        if !self.removed {
            // Best effort: a failed removal has no one to report it to in a drop.
            remove_tree(&self.path).ok();
        }
    }
}

#[cfg(test)]
mod tests;
