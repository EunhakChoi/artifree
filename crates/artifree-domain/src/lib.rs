//! Framework-independent domain vocabulary. No I/O, transport DTOs, or providers.
//!
//! These counters deliberately have different types. A source revision, document
//! sequence, and running application's data version have independent lifecycles.

pub mod identity;
pub mod package;

/// The family of an artifact's authoritative source model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// An interactive app backed by a file package.
    App,
    /// A structured text document.
    Document,
    /// An ordered slide presentation.
    Slides,
    /// A data dashboard.
    Dashboard,
    /// A visual design document.
    Design,
    /// A timeline or motion composition.
    Motion,
}

impl ArtifactKind {
    /// Stable lowercase name used in storage and wire contracts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Document => "document",
            Self::Slides => "slides",
            Self::Dashboard => "dashboard",
            Self::Design => "design",
            Self::Motion => "motion",
        }
    }
}

impl std::str::FromStr for ArtifactKind {
    type Err = identity::InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [
            Self::App,
            Self::Document,
            Self::Slides,
            Self::Dashboard,
            Self::Design,
            Self::Motion,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == value)
        .ok_or(identity::InvalidValue {
            what: "artifact kind",
            rule: "expected app, document, slides, dashboard, design, or motion",
        })
    }
}

/// A source history position; it does not version running application data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceRevision(pub u64);

/// A structured document's edit sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DocumentSequence(pub u64);

/// The version of a running application's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AppDataVersion(pub u64);

/// A position in the feed of committed changes. It orders commits across
/// artifacts and is unrelated to any one artifact's source revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChangeCursor(pub u64);
