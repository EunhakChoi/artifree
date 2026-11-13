//! Output collection: the admitted files of a passing attempt's output root, read beneath the
//! root's handle and bound into a manifest (decision 0042, rule 7; decision 0056, amendment of
//! 2026-11-13, input and output).
//!
//! Compiled into the test build and the enforcement-fixture build (0056 rule 16; decision 0057
//! rule 2). It runs only after the attempt's verdict has passed, and it traverses from the output
//! root's handle. Each entry is classified by `statat` without following links, opened only when
//! it is a directory or a regular file, and checked again through the opened descriptor. A regular
//! file is read once, through that descriptor, so no path is read again by name.

use std::collections::BTreeSet;
use std::ffi::CStr;
use std::fs::File;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::str::FromStr;

use artifree_domain::package::{
    FilePackage, MAX_FILE_BYTES, MAX_PACKAGE_BYTES, MAX_PATH_SEGMENTS, PackageError, PackagePath,
};
use artifree_ports::execution::BoundRun;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat};
use sha2::{Digest, Sha256};

#[cfg(test)]
use super::report::hex;
use super::staging::RESOLVE;

/// The most entries one traversal counts, directories included. A valid package has at most 256
/// files, each under at most seven directories of its own, which is exactly this many entries
/// (256 files and 1,792 directories). A larger tree is refused as soon as it passes the bound.
const MAX_ENTRIES: usize = 2048;
/// The deepest directory level entered below the output root, which is level 0. A package path
/// has at most `MAX_PATH_SEGMENTS` segments, so the deepest directory that can hold a packaged
/// file is at level 7.
const MAX_DEPTH: usize = MAX_PATH_SEGMENTS - 1;
/// The version tag that starts the output manifest's canonical text.
#[cfg(test)]
const MANIFEST_TAG: &str = "artifree-output-manifest-v1";
/// The tag of a manifest that also binds its run (decision 0057 rule 17).
#[cfg(test)]
const MANIFEST_TAG_V2: &str = "artifree-output-manifest-v2";

/// Why the collected files do not form a valid package, as far as collection reports it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PackageReason {
    /// The same path appears more than once.
    DuplicatePath,
    /// Two paths differ only by ASCII case.
    CaseCollision,
    /// A file path is also used as a directory prefix.
    FileDirectoryConflict,
    /// More than 256 files.
    TooManyFiles,
    /// A file is larger than 5 MiB.
    FileTooLarge,
    /// The files together are larger than 25 MiB.
    PackageTooLarge,
    /// The package has no `index.html` entry document.
    MissingEntry,
    /// Another package error, such as one of the change-set errors, that collection cannot
    /// produce.
    Other,
}

/// Why the output could not be collected. The traversal reports the first reason it finds, and
/// the attempt then yields nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CollectionRefusal {
    /// More than 2,048 entries, directories included.
    TooManyEntries,
    /// A symbolic link.
    Link,
    /// A socket.
    Socket,
    /// A FIFO.
    Fifo,
    /// A block or character device.
    Device,
    /// Any other non-directory, non-regular file type.
    Special,
    /// An entry that could not be listed, classified or opened.
    Unreadable,
    /// An opened entry has a different type from its first classification, or the bytes read
    /// differ from the size `fstat` reported.
    Changed,
    /// A directory deeper than seven levels below the output root.
    TooDeep,
    /// A regular file with more than one link.
    HardLink,
    /// A file larger than 5 MiB.
    FileTooLarge,
    /// The files together are larger than 25 MiB.
    TooLarge,
    /// A name that is not UTF-8, or a path that is not a portable package path.
    InvalidPath,
    /// The collected files do not form a valid `FilePackage`.
    Package(PackageReason),
}

/// What the output manifest binds the collected files to (0056, amendment of 2026-11-13, input
/// and output). It also carries the run the attempt belongs to (decision 0057 rule 17), under a
/// version tag of its own, so a digest under one tag is never compared with one under another.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ManifestBinding {
    /// The operation profile's identity.
    pub(super) profile: &'static str,
    /// The SHA-256 of the profile's manifest.
    pub(super) profile_manifest_sha256: [u8; 32],
    /// The SHA-256 of the tool root's manifest, in lowercase hexadecimal as `HostReport` has it.
    pub(super) tool_root_sha256: String,
    /// The filter's digest, in lowercase hexadecimal.
    pub(super) filter_sha256: String,
    /// The digest of the input listing.
    pub(super) input_digest: [u8; 32],
    /// The run the attempt belongs to (decision 0057 rule 17). `None` writes R02af's manifest
    /// under `artifree-output-manifest-v1`; `Some` writes `artifree-output-manifest-v2` with seven
    /// more lines.
    pub(super) run: Option<BoundRun>,
}

/// The output manifest: the binding and each collected file's path, length and SHA-256, in path
/// order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OutputManifest {
    binding: ManifestBinding,
    files: Vec<(PackagePath, u64, [u8; 32])>,
}

impl OutputManifest {
    /// The manifest of `package` under `binding`.
    fn of(package: &FilePackage, binding: &ManifestBinding) -> Self {
        Self {
            binding: binding.clone(),
            files: package
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
        }
    }

    /// The canonical text that the digest covers: the version tag, the binding's lines in a fixed
    /// order, the seven run lines when the binding carries a run, then one `file` line per file
    /// (0056, amendment of 2026-11-13, input and output; decision 0057 rule 17). A run selects the
    /// version tag `artifree-output-manifest-v2`; no run keeps `artifree-output-manifest-v1`.
    #[cfg(test)]
    #[must_use]
    pub(super) fn canonical_text(&self) -> String {
        let binding = &self.binding;
        let tag = if binding.run.is_some() {
            MANIFEST_TAG_V2
        } else {
            MANIFEST_TAG
        };
        let mut text = format!(
            "{tag}\nprofile\t{}\nprofile-manifest\t{}\ntool-root\t{}\nfilter\t{}\ninput\t{}\n",
            binding.profile,
            hex(binding.profile_manifest_sha256),
            binding.tool_root_sha256,
            binding.filter_sha256,
            hex(binding.input_digest),
        );
        if let Some(run) = &binding.run {
            text.push_str(&run_lines(run));
        }
        for (path, length, sha256) in &self.files {
            let line = format!("file\t{path}\t{length}\t{}\n", hex(*sha256));
            text.push_str(&line);
        }
        text
    }

    /// The SHA-256 digest of the canonical text.
    #[cfg(test)]
    #[must_use]
    pub(super) fn digest(&self) -> [u8; 32] {
        Sha256::digest(self.canonical_text().as_bytes()).into()
    }

    /// The binding this manifest was made under.
    #[cfg(feature = "enforcement-fixture")]
    #[must_use]
    pub(super) fn binding(&self) -> &ManifestBinding {
        &self.binding
    }
}

/// The seven lines that bind a run, in their canonical order: the actor, its generation (`none`
/// when it has none), the artifact, the revision, the job, the fence and the attempt (decision
/// 0057 rule 17). Each value is printable ASCII without tabs or newlines by construction (0056,
/// amendment of 2026-11-13, input and output).
#[cfg(test)]
fn run_lines(run: &BoundRun) -> String {
    let BoundRun {
        actor,
        generation,
        artifact,
        revision,
        job,
        fence,
        attempt,
    } = run;
    let generation_text = generation.map_or_else(|| "none".to_owned(), |n| n.to_string());
    format!(
        "actor\t{actor}\ngeneration\t{generation_text}\nartifact\t{artifact}\n\
         revision\t{revision}\njob\t{job}\nfence\t{fence}\nattempt\t{attempt}\n"
    )
}

/// A collected package with its manifest.
#[derive(Debug)]
pub(super) struct CollectedOutput {
    /// The admitted files.
    pub(super) package: FilePackage,
    /// The manifest that binds those files.
    pub(super) manifest: OutputManifest,
}

/// Collects the output root held by `out`, which the keeper holds (0042, rule 7).
///
/// # Errors
///
/// Returns the first refusal found; nothing is collected then.
pub(super) fn collect(
    out: &OwnedFd,
    binding: &ManifestBinding,
) -> Result<CollectedOutput, CollectionRefusal> {
    collect_with(out, binding, &mut |_, _| {})
}

/// `collect`, with `hook` run for each entry between its classification and its open. The unit
/// tests use the hook to swap entries.
fn collect_with(
    out: &OwnedFd,
    binding: &ManifestBinding,
    hook: &mut dyn FnMut(&OwnedFd, &str),
) -> Result<CollectedOutput, CollectionRefusal> {
    let mut walk = Walk {
        entries: 0,
        total: 0,
        files: Vec::new(),
        paths: BTreeSet::new(),
    };
    walk_directory(out, "", 0, &mut walk, hook)?;
    let package = FilePackage::new(walk.files)
        .map_err(|error| CollectionRefusal::Package(package_reason(&error)))?;
    let manifest = OutputManifest::of(&package, binding);
    Ok(CollectedOutput { package, manifest })
}

/// What the traversal has read so far.
struct Walk {
    /// The entries counted so far, directories included.
    entries: usize,
    /// The bytes read so far, across every file.
    total: usize,
    /// The files read so far, with their package paths.
    files: Vec<(PackagePath, Vec<u8>)>,
    /// The paths of the directories entered and the files read so far, folded to lower case, so
    /// that two entries whose paths differ only in case are refused even when one is an empty
    /// directory.
    paths: BTreeSet<String>,
}

/// Visits the entries of `directory`, which is `level` levels below the output root and whose
/// package path is `prefix` (empty at the root). Each entry is counted, classified, offered to
/// `hook`, then opened and checked, in that order.
fn walk_directory(
    directory: &OwnedFd,
    prefix: &str,
    level: usize,
    walk: &mut Walk,
    hook: &mut dyn FnMut(&OwnedFd, &str),
) -> Result<(), CollectionRefusal> {
    let listing = directory
        .try_clone()
        .map_err(|_| CollectionRefusal::Unreadable)?;
    let dir = Dir::new(listing).map_err(|_| CollectionRefusal::Unreadable)?;
    for entry in dir {
        let entry = entry.map_err(|_| CollectionRefusal::Unreadable)?;
        let raw = entry.file_name();
        if raw == c"." || raw == c".." {
            continue;
        }
        walk.entries += 1;
        if walk.entries > MAX_ENTRIES {
            return Err(CollectionRefusal::TooManyEntries);
        }
        let kind = classify(directory, raw)?;
        let name = raw.to_str().map_err(|_| CollectionRefusal::InvalidPath)?;
        hook(directory, name);
        let is_directory = kind == FileType::Directory;
        let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
        if is_directory {
            flags |= OFlags::DIRECTORY;
        }
        let opened = rustix::fs::openat2(directory, name, flags, Mode::empty(), RESOLVE)
            .map_err(|_| CollectionRefusal::Unreadable)?;
        let stat = rustix::fs::fstat(&opened).map_err(|_| CollectionRefusal::Unreadable)?;
        if FileType::from_raw_mode(stat.st_mode) != kind {
            return Err(CollectionRefusal::Changed);
        }
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if is_directory {
            let child_level = level + 1;
            if child_level > MAX_DEPTH {
                return Err(CollectionRefusal::TooDeep);
            }
            check_directory_path(&path, walk)?;
            walk_directory(&opened, &path, child_level, walk, hook)?;
        } else {
            read_file(opened, &stat, &path, walk)?;
        }
    }
    Ok(())
}

/// Refuses a directory whose path is not a portable package path, or whose path differs from one
/// already entered only in case (decision 0042 rule 7: invalid or colliding paths and extra
/// entries are refused, so an empty directory with such a name is refused too). The path is
/// checked as the parent of a placeholder file, since a package path names files.
fn check_directory_path(path: &str, walk: &mut Walk) -> Result<(), CollectionRefusal> {
    let placeholder = format!("{path}/{DIRECTORY_PLACEHOLDER}");
    PackagePath::from_str(&placeholder).map_err(|_| CollectionRefusal::InvalidPath)?;
    record_path(path, walk)
}

/// Records an entry's path, folded to lower case, and refuses it when an earlier directory or
/// file has the same folded path.
fn record_path(path: &str, walk: &mut Walk) -> Result<(), CollectionRefusal> {
    if walk.paths.insert(path.to_ascii_lowercase()) {
        Ok(())
    } else {
        Err(CollectionRefusal::Package(PackageReason::CaseCollision))
    }
}

/// The file name that `check_directory_path` appends to a directory's path. It is valid in every
/// position, and no valid file name is shorter, so only the directory's own segments decide and a
/// directory that can hold a valid file is never refused for its length.
const DIRECTORY_PLACEHOLDER: &str = "a.js";

/// Classifies the entry `name` of `directory` by `statat`, without following links. A directory
/// or a regular file is returned; any other type is the refusal that names it.
fn classify(directory: &OwnedFd, name: &CStr) -> Result<FileType, CollectionRefusal> {
    let stat = rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| CollectionRefusal::Unreadable)?;
    match FileType::from_raw_mode(stat.st_mode) {
        kind @ (FileType::Directory | FileType::RegularFile) => Ok(kind),
        FileType::Symlink => Err(CollectionRefusal::Link),
        FileType::Socket => Err(CollectionRefusal::Socket),
        FileType::Fifo => Err(CollectionRefusal::Fifo),
        FileType::BlockDevice | FileType::CharacterDevice => Err(CollectionRefusal::Device),
        FileType::Unknown => Err(CollectionRefusal::Special),
    }
}

/// Reads one regular file once, through `opened`, and records it under `path`.
///
/// The read stops one byte past the file limit, and the size `fstat` reported must match the bytes
/// read. The running total is checked after each file, so the traversal reads no further once it
/// passes the limit.
fn read_file(
    opened: OwnedFd,
    stat: &Stat,
    path: &str,
    walk: &mut Walk,
) -> Result<(), CollectionRefusal> {
    if stat.st_nlink != 1 {
        return Err(CollectionRefusal::HardLink);
    }
    // The name is checked before any byte is read.
    let package_path: PackagePath = path.parse().map_err(|_| CollectionRefusal::InvalidPath)?;
    record_path(path, walk)?;
    let bound = u64::try_from(MAX_FILE_BYTES)
        .map_err(|_| CollectionRefusal::FileTooLarge)?
        .saturating_add(1);
    let mut bytes = Vec::new();
    File::from(opened)
        .take(bound)
        .read_to_end(&mut bytes)
        .map_err(|_| CollectionRefusal::Unreadable)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(CollectionRefusal::FileTooLarge);
    }
    if usize::try_from(stat.st_size).ok() != Some(bytes.len()) {
        return Err(CollectionRefusal::Changed);
    }
    walk.total = walk.total.saturating_add(bytes.len());
    if walk.total > MAX_PACKAGE_BYTES {
        return Err(CollectionRefusal::TooLarge);
    }
    walk.files.push((package_path, bytes));
    Ok(())
}

/// Maps a package error to the reason collection reports. `PackageError` is non-exhaustive, so
/// every variant that collection cannot produce maps to `Other`.
fn package_reason(error: &PackageError) -> PackageReason {
    match error {
        PackageError::DuplicatePath(_) => PackageReason::DuplicatePath,
        PackageError::CaseCollision(..) => PackageReason::CaseCollision,
        PackageError::FileDirectoryConflict(_) => PackageReason::FileDirectoryConflict,
        PackageError::TooManyFiles => PackageReason::TooManyFiles,
        PackageError::FileTooLarge(_) => PackageReason::FileTooLarge,
        PackageError::PackageTooLarge => PackageReason::PackageTooLarge,
        PackageError::MissingEntry => PackageReason::MissingEntry,
        _ => PackageReason::Other,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod run_binding_tests;
