//! Framework-independent domain vocabulary. No I/O, transport DTOs, or providers.
//!
//! These counters deliberately have different types. A source revision, document
//! sequence, and running application's data version have independent lifecycles.

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

/// A source history position; it does not version running application data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceRevision(pub u64);

/// A structured document's edit sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DocumentSequence(pub u64);

/// The version of a running application's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AppDataVersion(pub u64);
