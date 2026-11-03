//! Framework-independent domain vocabulary. No I/O, transport DTOs, or providers.
//!
//! These counters deliberately have different types. A source revision,
//! metadata revision, document sequence, running application's data version,
//! and change feed sequence have independent lifecycles.

pub mod change;
pub mod data;
pub mod data_feed;
pub mod data_query;
pub mod identity;
pub mod job;
pub mod package;
pub mod room;

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

/// A position in an artifact's metadata history: its title and pin. It
/// advances when they change, independently of the source revision, so a
/// rename never conflicts with a source edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MetadataRevision(pub u64);

/// A structured document's edit sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DocumentSequence(pub u64);

/// The version of the application data of one actor and artifact (decision
/// 0031). It is 0 for a space nobody wrote, and every write that changes the
/// space raises it by one. It is unrelated to the source revision and to the
/// metadata revision, and a source restore does not move it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AppDataVersion(pub u64);

impl AppDataVersion {
    /// The version of a space that was never written.
    pub const EMPTY: Self = Self(0);
}
