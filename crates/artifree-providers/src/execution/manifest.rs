//! The tool root's manifest of every file's length and SHA-256 (decisions 0042 rule 3 and
//! 0056 rule 6).
//!
//! Each line reads `<64 lowercase hex digits> <decimal length> <path>`, and the lines are
//! strictly sorted by path bytes. Verification compares the root's regular files with the
//! listed paths in both directions, then checks each file's length and digest. A link, a
//! special file or an unlisted file is a mismatch; the manifest itself is never listed.
//!
//! Before that, the root, each of its entries and each of its ancestors must be protected from
//! change by anyone but root (decision 0058 rule 19, R03bca): owned by the tool owner, with no
//! group or other write bit and no set-ID bit. An ancestor may also be owned by root, and may be
//! world-writable when its sticky bit is set, as `/tmp` is.

use std::fs::{self, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::AdmissionRefusal;

/// The manifest's file name at the root of a tool root. The manifest does not list itself.
pub(super) const MANIFEST_FILE: &str = "MANIFEST";

/// The largest manifest that is read.
const MAX_MANIFEST_BYTES: u64 = 65_536;
/// The most lines a manifest may hold.
const MAX_LINES: usize = 512;
/// The most directory entries a walk of the tool root examines.
const MAX_ENTRIES: usize = 512;
/// The deepest entry, in path segments; the root's children are at depth 1.
const MAX_DEPTH: usize = 8;
/// The length of a SHA-256 digest rendered in lowercase hexadecimal.
const DIGEST_HEX_BYTES: usize = 64;

/// The identity of a verified tool root: the SHA-256 of its manifest's exact bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ManifestIdentity {
    /// The manifest's SHA-256 in lowercase hexadecimal.
    sha256: String,
}

impl ManifestIdentity {
    /// The manifest's SHA-256 in lowercase hexadecimal.
    #[must_use]
    pub(super) fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// The group and other write bits.
const SHARED_WRITE: u32 = 0o022;
/// The set-user-ID and set-group-ID bits.
const SET_ID: u32 = 0o6000;
/// The sticky bit, which keeps others from renaming or removing what they do not own.
const STICKY: u32 = 0o1000;

/// Whether an entry of the root with this owner and mode is protected: owned by `owner`, with
/// no group or other write bit and no set-ID bit.
fn entry_protected(uid: u32, mode: u32, owner: u32) -> bool {
    uid == owner && mode & (SHARED_WRITE | SET_ID) == 0
}

/// Whether an ancestor directory with this owner and mode is protected: owned by root or by
/// `owner`, with no set-ID bit, and with no group or other write bit unless its sticky bit is set.
fn ancestor_protected(uid: u32, mode: u32, owner: u32) -> bool {
    (uid == 0 || uid == owner)
        && mode & SET_ID == 0
        && (mode & SHARED_WRITE == 0 || mode & STICKY != 0)
}

/// Checks that an entry's metadata is protected.
fn check_entry(metadata: &Metadata, owner: u32) -> Result<(), AdmissionRefusal> {
    if entry_protected(metadata.uid(), metadata.mode(), owner) {
        Ok(())
    } else {
        Err(AdmissionRefusal::ToolRootUnprotected)
    }
}

/// Checks every ancestor of `root` up to and including `top`: each must be a directory, not a
/// link, and protected. `top` is `/` in production. A `top` that is no ancestor of `root` is
/// refused, so a mistaken one never skips the check.
fn check_ancestors(root: &Path, owner: u32, top: &Path) -> Result<(), AdmissionRefusal> {
    for ancestor in root.ancestors().skip(1) {
        let metadata =
            fs::symlink_metadata(ancestor).map_err(|_| AdmissionRefusal::ToolRootUnprotected)?;
        if !metadata.file_type().is_dir()
            || !ancestor_protected(metadata.uid(), metadata.mode(), owner)
        {
            return Err(AdmissionRefusal::ToolRootUnprotected);
        }
        if ancestor == top {
            return Ok(());
        }
    }
    Err(AdmissionRefusal::ToolRootUnprotected)
}

/// Verifies the tool root at `root` against its manifest and returns the manifest's identity.
/// `owner` is the account that must own the root and its entries: root (0) in production. The
/// ancestors are checked up to and including `top`, which is `/` in production.
///
/// # Errors
///
/// Returns `ToolRootUnprotected` when an ancestor, the root or an entry could be changed by
/// someone other than `owner` or root. Returns `MissingToolManifest` when the manifest is not a
/// readable regular file, is too large, is not UTF-8 or is malformed. Returns
/// `ToolRootMismatch` when the root's files differ from the manifest in any way.
pub(super) fn verify(
    root: &Path,
    owner: u32,
    top: &Path,
) -> Result<ManifestIdentity, AdmissionRefusal> {
    check_ancestors(root, owner, top)?;
    let bytes = read_manifest(&root.join(MANIFEST_FILE))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| AdmissionRefusal::MissingToolManifest)?;
    let entries = parse(text)?;
    let files = walk(root, owner)?;
    if !files
        .iter()
        .map(String::as_str)
        .eq(entries.iter().map(|entry| entry.path))
    {
        return Err(AdmissionRefusal::ToolRootMismatch);
    }
    for entry in &entries {
        check_file(root, entry)?;
    }
    Ok(ManifestIdentity {
        sha256: super::report::sha256_hex(&bytes),
    })
}

/// Reads the manifest's bytes. Only a regular file is opened: a named pipe cannot block
/// admission, and a symbolic link is refused rather than followed.
fn read_manifest(path: &Path) -> Result<Vec<u8>, AdmissionRefusal> {
    let regular = fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file());
    if !regular {
        return Err(AdmissionRefusal::MissingToolManifest);
    }
    super::read_bounded(path, MAX_MANIFEST_BYTES).map_err(|_| AdmissionRefusal::MissingToolManifest)
}

/// One manifest line.
#[derive(Debug)]
struct Entry<'text> {
    /// The recorded SHA-256 of the file in lowercase hexadecimal.
    digest: &'text str,
    /// The recorded length of the file in bytes.
    length: u64,
    /// The file's path relative to the root, with `/` separators.
    path: &'text str,
}

/// Parses the manifest text into entries that are strictly sorted by path bytes. An empty
/// manifest, one without a final newline, and a duplicate or unsorted path are all malformed.
fn parse(text: &str) -> Result<Vec<Entry<'_>>, AdmissionRefusal> {
    let body = text
        .strip_suffix('\n')
        .ok_or(AdmissionRefusal::MissingToolManifest)?;
    let lines: Vec<&str> = body.split('\n').collect();
    if lines.len() > MAX_LINES {
        return Err(AdmissionRefusal::MissingToolManifest);
    }
    let mut entries: Vec<Entry<'_>> = Vec::with_capacity(lines.len());
    for line in lines {
        let entry = parse_line(line)?;
        if entries
            .last()
            .is_some_and(|previous| previous.path.as_bytes() >= entry.path.as_bytes())
        {
            return Err(AdmissionRefusal::MissingToolManifest);
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// Parses one `<digest> <length> <path>` line. The path is the rest of the line, so it may
/// contain spaces.
fn parse_line(line: &str) -> Result<Entry<'_>, AdmissionRefusal> {
    let fields: Vec<&str> = line.splitn(3, ' ').collect();
    let &[digest, length, path] = fields.as_slice() else {
        return Err(AdmissionRefusal::MissingToolManifest);
    };
    let length = parse_length(length).ok_or(AdmissionRefusal::MissingToolManifest)?;
    if !is_digest(digest) || !is_path(path) {
        return Err(AdmissionRefusal::MissingToolManifest);
    }
    Ok(Entry {
        digest,
        length,
        path,
    })
}

/// Parses a length of 1 to 20 ASCII digits with no leading zero unless it is exactly `0`.
fn parse_length(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    let canonical = (1..=20).contains(&bytes.len())
        && bytes.iter().all(u8::is_ascii_digit)
        && (bytes.len() == 1 || bytes.first() != Some(&b'0'));
    if !canonical {
        return None;
    }
    text.parse::<u64>().ok()
}

/// Whether `text` is exactly one SHA-256 digest in lowercase hexadecimal.
fn is_digest(text: &str) -> bool {
    text.len() == DIGEST_HEX_BYTES
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether `path` is a relative path without empty, `.` or `..` segments, without control
/// characters, that is not the top-level manifest itself.
fn is_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path != MANIFEST_FILE
        && !path.bytes().any(|byte| byte.is_ascii_control())
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// Checks one listed file: its length first, then its SHA-256.
fn check_file(root: &Path, entry: &Entry<'_>) -> Result<(), AdmissionRefusal> {
    let path = root.join(entry.path);
    let metadata = fs::symlink_metadata(&path).map_err(|_| AdmissionRefusal::ToolRootMismatch)?;
    if !metadata.file_type().is_file() || metadata.len() != entry.length {
        return Err(AdmissionRefusal::ToolRootMismatch);
    }
    let digest =
        super::report::sha256_file_hex(&path).map_err(|_| AdmissionRefusal::ToolRootMismatch)?;
    if digest != entry.digest {
        return Err(AdmissionRefusal::ToolRootMismatch);
    }
    Ok(())
}

/// A directory that `walk` still has to list.
struct Directory {
    /// The directory's path on disk.
    path: PathBuf,
    /// The directory's path relative to the root with `/` separators; empty for the root.
    relative: String,
    /// The directory's depth in path segments; the root is depth 0.
    depth: usize,
}

/// Lists the regular files under `root` by relative path, sorted by bytes, leaving out the
/// top-level manifest. Entries are never followed.
///
/// # Errors
///
/// Returns `ToolRootMismatch` when the root is not a directory, an entry is neither a directory
/// nor a regular file, a name is not UTF-8, or the entry or depth bound is exceeded. Returns
/// `ToolRootUnprotected` when the root or an entry, the manifest included, is not protected.
fn walk(root: &Path, owner: u32) -> Result<Vec<String>, AdmissionRefusal> {
    let root_metadata =
        fs::symlink_metadata(root).map_err(|_| AdmissionRefusal::ToolRootMismatch)?;
    if !root_metadata.file_type().is_dir() {
        return Err(AdmissionRefusal::ToolRootMismatch);
    }
    check_entry(&root_metadata, owner)?;
    let mut pending = vec![Directory {
        path: root.to_path_buf(),
        relative: String::new(),
        depth: 0,
    }];
    let mut files = Vec::new();
    let mut seen = 0_usize;
    while let Some(directory) = pending.pop() {
        let listing =
            fs::read_dir(&directory.path).map_err(|_| AdmissionRefusal::ToolRootMismatch)?;
        for entry in listing {
            let entry = entry.map_err(|_| AdmissionRefusal::ToolRootMismatch)?;
            seen += 1;
            let depth = directory.depth + 1;
            if seen > MAX_ENTRIES || depth > MAX_DEPTH {
                return Err(AdmissionRefusal::ToolRootMismatch);
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| AdmissionRefusal::ToolRootMismatch)?;
            let relative = if directory.relative.is_empty() {
                name
            } else {
                format!("{}/{name}", directory.relative)
            };
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|_| AdmissionRefusal::ToolRootMismatch)?;
            let file_type = metadata.file_type();
            // A link or a special file is refused below; its own mode means nothing.
            if file_type.is_dir() || file_type.is_file() {
                check_entry(&metadata, owner)?;
            }
            if file_type.is_dir() {
                pending.push(Directory {
                    path,
                    relative,
                    depth,
                });
            } else if file_type.is_file() {
                if depth != 1 || relative != MANIFEST_FILE {
                    files.push(relative);
                }
            } else {
                return Err(AdmissionRefusal::ToolRootMismatch);
            }
        }
    }
    files.sort_unstable();
    Ok(files)
}

#[cfg(test)]
mod tests;
