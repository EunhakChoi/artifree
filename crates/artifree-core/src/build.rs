//! The trusted worker's half of a build (decision 0058 rules 7, 8, 13 and 18,
//! slice R03cb): the composition of a bundler's output with the pinned
//! revision, and the split of the bundler's messages into diagnostics.
//!
//! Implemented: both, as pure functions the worker calls after an attempt has
//! ended. Nothing here runs the bundler, reads a file or writes the store.

use std::fmt;

use artifree_domain::job::{
    BuildDiagnostic, DiagnosticMessage, MAX_DIAGNOSTIC_MESSAGE_BYTES, MAX_DIAGNOSTICS,
    SourceLocation,
};
use artifree_domain::package::{FilePackage, FileSet, PackagePath};

/// The bundle's script, which the entry document loads (decision 0058 rule 7).
const BUNDLE_SCRIPT: &str = "app.js";
/// The bundle's style sheet, which the bundler writes when the source imports
/// CSS.
const BUNDLE_STYLE: &str = "app.css";
/// The one element by which `index.html` loads the bundle (decision 0058 rule
/// 7).
const SCRIPT_ELEMENT: &str = r#"<script type="module" src="./app.js"></script>"#;
/// The directory of the bundler's input. The worker adds none of its files.
const SOURCE_DIRECTORY: &str = "src/";
/// The mount point of the revision inside the sandbox.
const INPUT_MOUNT: &str = "/input/";
/// The mount point as a path relative to the sandbox's root.
const INPUT_RELATIVE: &str = "input/";

/// Why a composition was refused (decision 0058 rule 8). Each one ends the job
/// as `output_rejected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// The bundler wrote a file other than `app.js` and `app.css`.
    UnexpectedOutput,
    /// The bundler wrote no `app.js`.
    MissingScript,
    /// A path is both in the bundle and among the revision's files outside
    /// `src/`.
    Collision,
    /// The revision's `index.html` is not UTF-8, or does not hold the script
    /// element of rule 7 exactly once.
    EntryLoadsNoBundle,
    /// The revision's `index.html` refers to a path under `src/`.
    EntryRefersToSource,
    /// The composed files break a package rule.
    Package,
}

impl fmt::Display for Rejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnexpectedOutput => "the bundler wrote a file other than app.js and app.css",
            Self::MissingScript => "the bundler wrote no app.js",
            Self::Collision => "a path is both in the bundle and in the revision outside src/",
            Self::EntryLoadsNoBundle => {
                "index.html does not load the bundle with exactly the element of rule 7, once"
            }
            Self::EntryRefersToSource => "index.html refers to a path under src/",
            Self::Package => "the composed files break a package rule",
        })
    }
}

impl std::error::Error for Rejection {}

/// Composes a build (decision 0058 rules 7 and 8): the bundle, which must be
/// `app.js` and at most `app.css`, with the revision's files outside `src/`.
/// The revision's `index.html` must load the bundle with exactly the element of
/// rule 7, once, and refer to no path under `src/`.
///
/// # Errors
///
/// The [`Rejection`] of the first broken rule, checked in the order the
/// variants are listed.
pub fn compose(bundle: FileSet, source: &FilePackage) -> Result<FilePackage, Rejection> {
    if bundle
        .files()
        .any(|(path, _)| !matches!(path.as_str(), BUNDLE_SCRIPT | BUNDLE_STYLE))
    {
        return Err(Rejection::UnexpectedOutput);
    }
    if !bundle
        .files()
        .any(|(path, _)| path.as_str() == BUNDLE_SCRIPT)
    {
        return Err(Rejection::MissingScript);
    }
    let added: Vec<(&PackagePath, &[u8])> = source
        .files()
        .filter(|(path, _)| !path.as_str().starts_with(SOURCE_DIRECTORY))
        .collect();
    if added.iter().any(|(path, _)| bundle.file(path).is_some()) {
        return Err(Rejection::Collision);
    }
    check_entry(source.file(&PackagePath::entry()))?;
    let mut files: Vec<(PackagePath, Vec<u8>)> = bundle.into_files().collect();
    files.extend(
        added
            .into_iter()
            .map(|(path, content)| (path.clone(), content.to_vec())),
    );
    FilePackage::new(files).map_err(|_| Rejection::Package)
}

/// Checks the entry document against rule 7. A reference to `src/` is any
/// occurrence of `src/` in any ASCII case: the template's document has none,
/// and refusing prose that names the directory is the safe side.
fn check_entry(entry: Option<&[u8]>) -> Result<(), Rejection> {
    let text = entry
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .ok_or(Rejection::EntryLoadsNoBundle)?;
    if text.matches(SCRIPT_ELEMENT).count() != 1 {
        return Err(Rejection::EntryLoadsNoBundle);
    }
    if text.to_ascii_lowercase().contains(SOURCE_DIRECTORY) {
        return Err(Rejection::EntryRefersToSource);
    }
    Ok(())
}

/// The fixed diagnostic of a build that the system call filter ended (decision
/// 0058 rule 18).
#[must_use]
pub fn filter_kill_diagnostics() -> Vec<BuildDiagnostic> {
    fixed("the bundler was stopped because it made a system call outside its filter")
}

/// The fixed diagnostic of a revision that holds resolver configuration
/// (decision 0058, note "R03c split"), naming the first such path when there
/// is one.
#[must_use]
pub fn resolver_diagnostics(path: Option<&PackagePath>) -> Vec<BuildDiagnostic> {
    const RULE: &str = "would change how imports resolve; react-v1 builds only with its \
                        preinstalled packages, so remove it";
    match path {
        Some(path) => fixed(&format!("{path} {RULE}")),
        None => fixed(&format!("a file of the revision {RULE}")),
    }
}

/// One diagnostic that points at no file. The text is a constant or holds a
/// package path of at most 240 bytes, so it always fits the message rules.
fn fixed(text: &str) -> Vec<BuildDiagnostic> {
    message_of(text)
        .map(|message| BuildDiagnostic {
            location: None,
            message,
        })
        .into_iter()
        .collect()
}

/// Splits the bundler's standard error into rule 13's diagnostics: each error
/// message, with the location line that follows it when there is one. Notes,
/// warnings, the quoted source excerpt and the closing count are dropped. At
/// most [`MAX_DIAGNOSTICS`] are kept. A failed bundler with no message that can
/// be read gets one fixed diagnostic, so a failed build always says something.
#[must_use]
pub fn diagnostics(stderr: &[u8]) -> Vec<BuildDiagnostic> {
    let text = String::from_utf8_lossy(stderr);
    let mut lines = text.lines().peekable();
    let mut found = Vec::new();
    while let Some(line) = lines.next() {
        let Some(header) = error_header(line) else {
            continue;
        };
        // The location line, when the message has one, is the first non-empty
        // line after the header.
        while lines.peek().is_some_and(|next| next.trim().is_empty()) {
            lines.next();
        }
        let location = match lines.peek().and_then(|next| location_of(next)) {
            Some(location) => {
                lines.next();
                Some(location)
            }
            None => None,
        };
        if let Some(message) = message_of(&rewrite_mount(header)) {
            found.push(BuildDiagnostic { location, message });
        }
        if found.len() == MAX_DIAGNOSTICS {
            break;
        }
    }
    if found.is_empty() {
        found = fixed("the bundler failed without a message that could be read");
    }
    found
}

/// The text of an error header, `✘ [ERROR] <text>`, or `X [ERROR] <text>` where
/// the bundler's output cannot show `✘`, or `None` for any other line.
fn error_header(line: &str) -> Option<&str> {
    const TAG: &str = "[ERROR] ";
    let line = line.trim_start();
    let index = line.find(TAG)?;
    let mark = line.get(..index)?;
    matches!(mark.trim_end(), "" | "\u{2718}" | "X")
        .then(|| line.get(index + TAG.len()..))
        .flatten()
}

/// The location of a line `<path>:<line>:<column>:`, with the path under the
/// input mount rewritten to its package path. A path outside the revision, or
/// a line of another form, gives `None`.
fn location_of(line: &str) -> Option<SourceLocation> {
    let text = line.trim().strip_suffix(':')?;
    let mut parts = text.rsplitn(3, ':');
    let column = parts.next()?.parse().ok()?;
    let line_number = parts.next()?.parse().ok()?;
    let path = package_path(parts.next()?)?;
    Some(SourceLocation {
        path,
        line: line_number,
        column,
    })
}

/// The package path of a path the bundler printed: absolute under the input
/// mount, or relative to its working directory, which may lie beside or above
/// the mount.
fn package_path(printed: &str) -> Option<PackagePath> {
    let mut rest = printed;
    while let Some(stripped) = rest.strip_prefix("../") {
        rest = stripped;
    }
    let rest = rest
        .strip_prefix(INPUT_MOUNT)
        .or_else(|| rest.strip_prefix(INPUT_RELATIVE))?;
    rest.parse().ok()
}

/// Rewrites every path under the input mount in a message to its package path
/// (decision 0058 rule 13).
fn rewrite_mount(text: &str) -> String {
    text.replace("../input/", "").replace(INPUT_MOUNT, "")
}

/// A message under rule 13: control characters become spaces, the text is
/// trimmed and cut to [`MAX_DIAGNOSTIC_MESSAGE_BYTES`] at a character boundary.
/// `None` when nothing is left.
fn message_of(text: &str) -> Option<DiagnosticMessage> {
    let cleaned: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let mut cut = cleaned.trim();
    if cut.len() > MAX_DIAGNOSTIC_MESSAGE_BYTES {
        let mut end = MAX_DIAGNOSTIC_MESSAGE_BYTES;
        while !cut.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        cut = cut.get(..end).unwrap_or_default().trim_end();
    }
    cut.parse().ok()
}

#[cfg(test)]
mod tests;
