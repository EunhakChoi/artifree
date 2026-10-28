//! File packages: the authoritative source of an HTML artifact revision.
//!
//! Paths are portable relative names, not filesystem paths. Validation rejects
//! traversal, absolute or drive-qualified names, hidden segments, reserved
//! Windows names, case-insensitive collisions, and file/directory conflicts so
//! that a package can be served and downloaded without reinterpretation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

/// Maximum number of files in one package.
pub const MAX_FILES: usize = 256;
/// Upper bound on the size of a change set. Paths in a change set are
/// distinct, so a valid one holds at most [`MAX_FILES`] puts and
/// [`MAX_FILES`] deletes; a larger one always deletes a missing file or
/// exceeds [`MAX_FILES`]. Adapters use it to reject such a set before
/// decoding it.
pub const MAX_CHANGES: usize = 2 * MAX_FILES;
/// Maximum size of one file in bytes.
pub const MAX_FILE_BYTES: usize = 5 * 1024 * 1024;
/// Maximum combined size of every file in a package in bytes.
pub const MAX_PACKAGE_BYTES: usize = 25 * 1024 * 1024;
/// Maximum path length in bytes.
pub const MAX_PATH_BYTES: usize = 240;
/// Maximum number of path segments.
pub const MAX_PATH_SEGMENTS: usize = 8;
/// Maximum length of one path segment in bytes.
pub const MAX_SEGMENT_BYTES: usize = 100;
/// The document opened when a package is previewed.
pub const ENTRY_PATH: &str = "index.html";

/// Why a package path was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathError {
    /// The path is empty or longer than [`MAX_PATH_BYTES`].
    Length,
    /// The path has more than [`MAX_PATH_SEGMENTS`] segments.
    Depth,
    /// A segment is empty or longer than [`MAX_SEGMENT_BYTES`].
    Segment,
    /// A character outside ASCII letters, digits, `-`, `_`, and `.`.
    Character,
    /// A segment starts or ends with `.`, including `.` and `..`.
    Dot,
    /// A segment is a reserved Windows device name such as `con` or `nul.txt`.
    Reserved,
    /// The file extension has no supported media type.
    Extension,
}

impl fmt::Display for PathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Length => "path must contain 1-240 bytes",
            Self::Depth => "path must have at most 8 segments",
            Self::Segment => "each path segment must contain 1-100 bytes",
            Self::Character => {
                "path may contain only ASCII letters, digits, '-', '_', '.', and '/'"
            }
            Self::Dot => "path segments must not start or end with '.'",
            Self::Reserved => "path segments must not use reserved device names",
            Self::Extension => "file extension is not a supported media type",
        })
    }
}

impl std::error::Error for PathError {}

/// Media types accepted in a package. The extension decides the type; content
/// is never sniffed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MediaType {
    /// HTML documents.
    Html,
    /// Stylesheets.
    Css,
    /// JavaScript classic scripts and modules.
    JavaScript,
    /// JSON data and source maps.
    Json,
    /// SVG images.
    Svg,
    /// PNG images.
    Png,
    /// JPEG images.
    Jpeg,
    /// GIF images.
    Gif,
    /// WebP images.
    Webp,
    /// AVIF images.
    Avif,
    /// Icon files.
    Icon,
    /// Plain text.
    Text,
    /// Markdown text.
    Markdown,
    /// Comma-separated values.
    Csv,
    /// WOFF fonts.
    Woff,
    /// WOFF2 fonts.
    Woff2,
    /// TrueType fonts.
    Ttf,
    /// OpenType fonts.
    Otf,
    /// WebAssembly modules.
    Wasm,
    /// XML documents.
    Xml,
}

impl MediaType {
    /// Every supported file extension, in lowercase, with its media type.
    /// Extensions match without regard to ASCII case.
    pub const EXTENSIONS: &'static [(&'static str, Self)] = &[
        ("html", Self::Html),
        ("htm", Self::Html),
        ("css", Self::Css),
        ("js", Self::JavaScript),
        ("mjs", Self::JavaScript),
        ("json", Self::Json),
        ("map", Self::Json),
        ("svg", Self::Svg),
        ("png", Self::Png),
        ("jpg", Self::Jpeg),
        ("jpeg", Self::Jpeg),
        ("gif", Self::Gif),
        ("webp", Self::Webp),
        ("avif", Self::Avif),
        ("ico", Self::Icon),
        ("txt", Self::Text),
        ("md", Self::Markdown),
        ("csv", Self::Csv),
        ("woff", Self::Woff),
        ("woff2", Self::Woff2),
        ("ttf", Self::Ttf),
        ("otf", Self::Otf),
        ("wasm", Self::Wasm),
        ("xml", Self::Xml),
    ];

    fn from_extension(extension: &str) -> Option<Self> {
        Self::EXTENSIONS
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(extension))
            .map(|&(_, media_type)| media_type)
    }

    /// The `Content-Type` value, with a UTF-8 charset for text formats.
    #[must_use]
    pub fn essence(self) -> &'static str {
        match self {
            Self::Html => "text/html; charset=utf-8",
            Self::Css => "text/css; charset=utf-8",
            Self::JavaScript => "text/javascript; charset=utf-8",
            Self::Json => "application/json",
            Self::Svg => "image/svg+xml",
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
            Self::Icon => "image/x-icon",
            Self::Text => "text/plain; charset=utf-8",
            Self::Markdown => "text/markdown; charset=utf-8",
            Self::Csv => "text/csv; charset=utf-8",
            Self::Woff => "font/woff",
            Self::Woff2 => "font/woff2",
            Self::Ttf => "font/ttf",
            Self::Otf => "font/otf",
            Self::Wasm => "application/wasm",
            Self::Xml => "application/xml",
        }
    }
}

/// A validated, portable, slash-separated path inside a package.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackagePath(String);

impl PackagePath {
    /// The canonical string form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The media type implied by the validated extension.
    #[must_use]
    pub fn media_type(&self) -> MediaType {
        // Parsing guarantees a supported extension; Text is an unreachable fallback.
        self.0
            .rsplit_once('.')
            .and_then(|(_, extension)| MediaType::from_extension(extension))
            .unwrap_or(MediaType::Text)
    }

    /// The preview entry document, `index.html`.
    #[must_use]
    pub fn entry() -> Self {
        Self(ENTRY_PATH.to_owned())
    }

    fn folded(&self) -> String {
        self.0.to_ascii_lowercase()
    }
}

fn is_reserved_device(segment: &str) -> bool {
    let stem = segment
        .split_once('.')
        .map_or(segment, |(stem, _)| stem)
        .to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || ((stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit())
}

impl FromStr for PackagePath {
    type Err = PathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() || value.len() > MAX_PATH_BYTES {
            return Err(PathError::Length);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./".contains(&byte))
        {
            return Err(PathError::Character);
        }
        let segments: Vec<&str> = value.split('/').collect();
        if segments.len() > MAX_PATH_SEGMENTS {
            return Err(PathError::Depth);
        }
        for segment in &segments {
            if segment.is_empty() || segment.len() > MAX_SEGMENT_BYTES {
                return Err(PathError::Segment);
            }
            if segment.starts_with('.') || segment.ends_with('.') {
                return Err(PathError::Dot);
            }
            if is_reserved_device(segment) {
                return Err(PathError::Reserved);
            }
        }
        let has_media_type = segments
            .last()
            .and_then(|name| name.rsplit_once('.'))
            .and_then(|(_, extension)| MediaType::from_extension(extension))
            .is_some();
        if !has_media_type {
            return Err(PathError::Extension);
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for PackagePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Why a package or change set was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PackageError {
    /// The same path appears more than once.
    DuplicatePath(PackagePath),
    /// Two paths differ only by ASCII case.
    CaseCollision(PackagePath, PackagePath),
    /// A file path is also used as a directory prefix.
    FileDirectoryConflict(PackagePath),
    /// More than [`MAX_FILES`] files.
    TooManyFiles,
    /// A file exceeds [`MAX_FILE_BYTES`].
    FileTooLarge(PackagePath),
    /// The files together exceed [`MAX_PACKAGE_BYTES`].
    PackageTooLarge,
    /// The package has no `index.html` entry document.
    MissingEntry,
    /// A change set has no changes.
    EmptyChange,
    /// A change deletes a path that is not in the base package.
    DeleteMissing(PackagePath),
    /// The changes leave the package identical to its base.
    Unchanged,
}

impl fmt::Display for PackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicatePath(path) => write!(formatter, "path appears more than once: {path}"),
            Self::CaseCollision(first, second) => {
                write!(formatter, "paths differ only by case: {first} and {second}")
            }
            Self::FileDirectoryConflict(path) => {
                write!(formatter, "path is both a file and a directory: {path}")
            }
            Self::TooManyFiles => {
                write!(formatter, "a package may contain at most {MAX_FILES} files")
            }
            Self::FileTooLarge(path) => {
                write!(formatter, "file exceeds {MAX_FILE_BYTES} bytes: {path}")
            }
            Self::PackageTooLarge => {
                write!(formatter, "package exceeds {MAX_PACKAGE_BYTES} bytes")
            }
            Self::MissingEntry => write!(formatter, "package must contain {ENTRY_PATH}"),
            Self::EmptyChange => write!(formatter, "change set must contain at least one change"),
            Self::DeleteMissing(path) => {
                write!(
                    formatter,
                    "cannot delete a path that does not exist: {path}"
                )
            }
            Self::Unchanged => write!(formatter, "changes do not modify the package"),
        }
    }
}

impl std::error::Error for PackageError {}

/// One requested modification to a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    /// Create or replace a file.
    Put {
        /// Target path.
        path: PackagePath,
        /// Complete new content.
        content: Vec<u8>,
    },
    /// Remove an existing file.
    Delete {
        /// Path to remove.
        path: PackagePath,
    },
}

impl FileChange {
    /// The path this change targets.
    #[must_use]
    pub fn path(&self) -> &PackagePath {
        match self {
            Self::Put { path, .. } | Self::Delete { path } => path,
        }
    }
}

/// A validated set of files with an `index.html` entry document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePackage {
    files: BTreeMap<PackagePath, Vec<u8>>,
}

impl FilePackage {
    /// Validates files supplied by a caller.
    ///
    /// # Errors
    ///
    /// Returns a [`PackageError`] when a path repeats or collides, a size or
    /// count limit is exceeded, or `index.html` is missing.
    pub fn new(files: Vec<(PackagePath, Vec<u8>)>) -> Result<Self, PackageError> {
        let mut map = BTreeMap::new();
        for (path, content) in files {
            if map.contains_key(&path) {
                return Err(PackageError::DuplicatePath(path));
            }
            map.insert(path, content);
        }
        Self::validated(map)
    }

    fn validated(files: BTreeMap<PackagePath, Vec<u8>>) -> Result<Self, PackageError> {
        if files.len() > MAX_FILES {
            return Err(PackageError::TooManyFiles);
        }
        let mut total = 0usize;
        let mut folded: BTreeMap<String, &PackagePath> = BTreeMap::new();
        for (path, content) in &files {
            if content.len() > MAX_FILE_BYTES {
                return Err(PackageError::FileTooLarge(path.clone()));
            }
            total = total.saturating_add(content.len());
            if let Some(existing) = folded.insert(path.folded(), path) {
                return Err(PackageError::CaseCollision(existing.clone(), path.clone()));
            }
        }
        if total > MAX_PACKAGE_BYTES {
            return Err(PackageError::PackageTooLarge);
        }
        let directories: BTreeSet<String> = folded
            .keys()
            .flat_map(|path| {
                path.match_indices('/')
                    .map(|(index, _)| path[..index].to_owned())
            })
            .collect();
        if let Some((_, path)) = folded.iter().find(|(name, _)| directories.contains(*name)) {
            return Err(PackageError::FileDirectoryConflict((*path).clone()));
        }
        if !files.contains_key(&PackagePath::entry()) {
            return Err(PackageError::MissingEntry);
        }
        Ok(Self { files })
    }

    /// Applies a change set and validates the resulting package.
    ///
    /// # Errors
    ///
    /// Returns a [`PackageError`] for an empty or repeated change, a deletion
    /// of a missing file, a result identical to `self`, or an invalid result.
    pub fn apply(&self, changes: Vec<FileChange>) -> Result<Self, PackageError> {
        if changes.is_empty() {
            return Err(PackageError::EmptyChange);
        }
        let mut seen = BTreeSet::new();
        let mut files = self.files.clone();
        for change in changes {
            if !seen.insert(change.path().clone()) {
                return Err(PackageError::DuplicatePath(change.path().clone()));
            }
            match change {
                FileChange::Put { path, content } => {
                    files.insert(path, content);
                }
                FileChange::Delete { path } => {
                    if files.remove(&path).is_none() {
                        return Err(PackageError::DeleteMissing(path));
                    }
                }
            }
        }
        if files == self.files {
            return Err(PackageError::Unchanged);
        }
        Self::validated(files)
    }

    /// The content of one file.
    #[must_use]
    pub fn file(&self, path: &PackagePath) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    /// Files in path order.
    #[must_use]
    pub fn files(&self) -> impl ExactSizeIterator<Item = (&PackagePath, &[u8])> {
        self.files
            .iter()
            .map(|(path, content)| (path, content.as_slice()))
    }

    /// Files in path order, taking their bytes.
    #[must_use]
    pub fn into_files(self) -> impl ExactSizeIterator<Item = (PackagePath, Vec<u8>)> {
        self.files.into_iter()
    }

    /// Number of files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Always false: a valid package contains at least `index.html`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(value: &str) -> PackagePath {
        value
            .parse()
            .unwrap_or_else(|error| panic!("{value}: {error}"))
    }

    fn package(paths: &[&str]) -> Result<FilePackage, PackageError> {
        FilePackage::new(
            paths
                .iter()
                .map(|value| (path(value), value.as_bytes().to_vec()))
                .collect(),
        )
    }

    #[test]
    fn accepts_nested_portable_paths() {
        for value in [
            "index.html",
            "assets/app.v2.js",
            "a/b/c/d/e/f/g/h.css",
            "Fonts/Inter_Var.woff2",
        ] {
            assert!(value.parse::<PackagePath>().is_ok(), "rejected {value}");
        }
    }

    #[test]
    fn rejects_traversal_absolute_and_ambiguous_paths() {
        for (value, expected) in [
            ("", PathError::Length),
            ("../index.html", PathError::Dot),
            ("a/../index.html", PathError::Dot),
            ("./index.html", PathError::Dot),
            ("/index.html", PathError::Segment),
            ("a//b.html", PathError::Segment),
            ("dir/", PathError::Segment),
            ("C:/index.html", PathError::Character),
            ("a\\b.html", PathError::Character),
            ("a%2fb.html", PathError::Character),
            ("café.html", PathError::Character),
            ("nul\0.html", PathError::Character),
            (".env.json", PathError::Dot),
            ("index.html.", PathError::Dot),
            ("con.html", PathError::Reserved),
            ("lpt1/a.css", PathError::Reserved),
            ("README", PathError::Extension),
            ("tool.exe", PathError::Extension),
            ("a/b/c/d/e/f/g/h/i.css", PathError::Depth),
        ] {
            assert_eq!(value.parse::<PackagePath>(), Err(expected), "{value:?}");
        }
        assert_eq!(
            format!("{}.html", "a".repeat(MAX_PATH_BYTES)).parse::<PackagePath>(),
            Err(PathError::Length)
        );
    }

    #[test]
    fn media_type_follows_extension_case_insensitively() {
        assert_eq!(path("index.HTML").media_type(), MediaType::Html);
        assert_eq!(
            path("app.mjs").media_type().essence(),
            "text/javascript; charset=utf-8"
        );
    }

    #[test]
    fn package_requires_entry_and_unambiguous_paths() {
        assert!(package(&["index.html", "app.js"]).is_ok());
        assert_eq!(package(&["app.js"]), Err(PackageError::MissingEntry));
        assert!(matches!(
            package(&["index.html", "App.js", "app.js"]),
            Err(PackageError::CaseCollision(..))
        ));
        assert!(matches!(
            package(&["index.html", "index.html"]),
            Err(PackageError::DuplicatePath(_))
        ));
        assert!(matches!(
            package(&["index.html", "a.html", "A.html/b.css"]),
            Err(PackageError::FileDirectoryConflict(_))
        ));
    }

    #[test]
    fn package_enforces_size_and_count_limits() {
        let big = vec![0; MAX_FILE_BYTES + 1];
        assert!(matches!(
            FilePackage::new(vec![(PackagePath::entry(), big)]),
            Err(PackageError::FileTooLarge(_))
        ));
        let mut files: Vec<_> = (0..6)
            .map(|index| (path(&format!("f{index}.txt")), vec![0; MAX_FILE_BYTES]))
            .collect();
        files.push((PackagePath::entry(), Vec::new()));
        assert_eq!(FilePackage::new(files), Err(PackageError::PackageTooLarge));
        let many: Vec<_> = (0..MAX_FILES)
            .map(|index| (path(&format!("f{index}.txt")), Vec::new()))
            .chain([(PackagePath::entry(), Vec::new())])
            .collect();
        assert_eq!(FilePackage::new(many), Err(PackageError::TooManyFiles));
    }

    #[test]
    fn apply_puts_and_deletes_without_mutating_base() -> Result<(), PackageError> {
        let base = package(&["index.html", "old.css"])?;
        let next = base.apply(vec![
            FileChange::Put {
                path: path("new.css"),
                content: b"n".to_vec(),
            },
            FileChange::Delete {
                path: path("old.css"),
            },
        ])?;
        assert_eq!(
            next.files()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["index.html", "new.css"]
        );
        assert!(base.file(&path("old.css")).is_some());
        Ok(())
    }

    #[test]
    fn apply_rejects_ambiguous_or_ineffective_changes() -> Result<(), PackageError> {
        let base = package(&["index.html"])?;
        assert_eq!(base.apply(Vec::new()), Err(PackageError::EmptyChange));
        assert!(matches!(
            base.apply(vec![FileChange::Delete {
                path: path("gone.css")
            }]),
            Err(PackageError::DeleteMissing(_))
        ));
        assert_eq!(
            base.apply(vec![FileChange::Delete {
                path: PackagePath::entry()
            }]),
            Err(PackageError::MissingEntry)
        );
        assert_eq!(
            base.apply(vec![FileChange::Put {
                path: PackagePath::entry(),
                content: b"index.html".to_vec()
            }]),
            Err(PackageError::Unchanged)
        );
        assert!(matches!(
            base.apply(vec![
                FileChange::Put {
                    path: path("a.css"),
                    content: Vec::new()
                },
                FileChange::Delete {
                    path: path("a.css")
                },
            ]),
            Err(PackageError::DuplicatePath(_))
        ));
        Ok(())
    }
}
