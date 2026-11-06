//! Durable storage for application data (decision 0031).
//!
//! One data space exists per actor and artifact. Its version is separate from
//! the artifact's source revision and metadata revision: a write that changes
//! the space raises it by one, and nothing else does. An implementation must
//! make each write atomic: the artifact check, the key lookup, the base
//! version check, the change, the version, and the key record all happen in one
//! step that reads the state it decides on, so two writers cannot both pass a
//! base check.

use std::fmt;

use artifree_domain::AppDataVersion;
use artifree_domain::change::FeedId;
use artifree_domain::data::{DataChange, DataError, DataSpace};
use artifree_domain::identity::{ActorId, ArtifactId, StateGeneration, UnixMillis};

use crate::artifact_store::{Delivery, RequestIdentity, StoreError};

/// Server identity and user state read from one database snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxBinding {
    /// Authenticated registered actor.
    pub actor: ActorId,
    /// Enabled user's durable state generation.
    pub generation: StateGeneration,
    /// Existing database feed identity, renewed by supported restore.
    pub epoch: FeedId,
}

/// Optional authority checked in the write transaction before key replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxFence {
    /// Authority explicitly admitted by the browser.
    pub expected: OutboxBinding,
    /// Generation under which this request authenticated.
    pub authenticated_generation: StateGeneration,
}

/// A space and the version it is at.
#[derive(Debug, Clone, PartialEq)]
pub struct DataSnapshot {
    /// The version: 0 for a space nobody wrote.
    pub version: AppDataVersion,
    /// The space.
    pub space: DataSpace,
}

/// A change to a space, conditional on the version the caller saw.
#[derive(Debug, Clone, Copy)]
pub struct DataWrite<'a> {
    /// Optional outbox precondition; absent for existing direct writes.
    pub fence: Option<&'a OutboxFence>,
    /// Retry identity; its actor owns the space.
    pub request: RequestIdentity<'a>,
    /// The artifact. It must be outside the trash and not purged.
    pub artifact: &'a ArtifactId,
    /// The version the change was prepared against.
    pub base: AppDataVersion,
    /// The change.
    pub change: &'a DataChange,
    /// When the request is made, from the caller's clock.
    pub now: UnixMillis,
}

/// The durable result of a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataReceipt {
    /// The version the request left the space at. A change that altered
    /// nothing leaves the version where it was.
    pub version: AppDataVersion,
    /// Whether the change altered the space.
    pub changed: bool,
    /// Whether this call wrote it or replayed an earlier request. A replay
    /// reports the version of the original request, not the current one.
    pub delivery: Delivery,
}

/// A data store failure or a rejected conditional write.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DataStoreError {
    /// The artifact does not exist, is in the trash, was purged, or belongs
    /// to another actor.
    NotFound,
    /// The current database or enabled user no longer matches the fence.
    OutboxBindingChanged,
    /// The actor already used the key for another request.
    RequestKeyReused,
    /// The space is not at the base version; nothing was written.
    VersionConflict {
        /// The version it is at.
        current: AppDataVersion,
    },
    /// The change is refused by the rules of the space (decision 0031);
    /// nothing was written and no key was recorded.
    Rejected(DataError),
    /// Storage could not complete the operation.
    Unavailable(String),
}

impl fmt::Display for DataStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutboxBindingChanged => formatter.write_str("outbox binding changed"),
            Self::NotFound => formatter.write_str("artifact not found"),
            Self::RequestKeyReused => {
                formatter.write_str("request key was already used for a different request")
            }
            Self::VersionConflict { current } => {
                write!(formatter, "the data is at version {}", current.0)
            }
            Self::Rejected(error) => write!(formatter, "{error}"),
            Self::Unavailable(reason) => write!(formatter, "storage unavailable: {reason}"),
        }
    }
}

impl std::error::Error for DataStoreError {}

impl From<StoreError> for DataStoreError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self::NotFound,
            StoreError::RequestKeyReused => Self::RequestKeyReused,
            StoreError::Unavailable(reason) => Self::Unavailable(reason),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

/// Durable application data storage.
///
/// Artifact ownership authorization belongs to the application core. Binding
/// and fenced writes additionally check enabled user state. An
/// artifact in the trash or purged is [`DataStoreError::NotFound`] for every
/// read or write, a retry included.
pub trait DataStore {
    /// Reads feed identity and enabled actor state in one snapshot.
    ///
    /// # Errors
    ///
    /// `OutboxBindingChanged` for missing, disabled or stale users, or storage failure.
    fn outbox_binding(
        &self,
        actor: &ActorId,
        generation: StateGeneration,
    ) -> Result<OutboxBinding, DataStoreError>;

    /// The space of `actor` for an artifact, and its version, read in one
    /// snapshot.
    ///
    /// # Errors
    ///
    /// [`DataStoreError::NotFound`] or [`DataStoreError::Unavailable`].
    fn read_data(
        &self,
        artifact: &ArtifactId,
        actor: &ActorId,
    ) -> Result<DataSnapshot, DataStoreError>;

    /// Applies a change if the space is at the base version, or returns the
    /// stored receipt of the same earlier request. In one step: the artifact
    /// is checked first, then the optional outbox fence (enabled user,
    /// authentication generation, actor and feed identity), then the key
    /// (a replay, or a key another request used), then the base version,
    /// then the change. Only a request that
    /// succeeds records its key; a refusal leaves the key free. A change that
    /// alters nothing writes no new version, but its key is recorded. The key
    /// shares its space with the keys of artifact requests, cleanups, and jobs.
    ///
    /// A change that altered the space also appends one
    /// [`ChangeKind::DataChanged`](crate::artifact_store::ChangeKind::DataChanged)
    /// to the change feed of the actor of the request, whose space changed,
    /// as the last part of the same step (decision 0040). A replay, a change
    /// that altered nothing, and a refusal append nothing, and a failure to
    /// append undoes the whole write.
    ///
    /// # Errors
    ///
    /// [`DataStoreError::NotFound`], [`DataStoreError::RequestKeyReused`],
    /// [`DataStoreError::VersionConflict`], [`DataStoreError::Rejected`], or
    /// [`DataStoreError::Unavailable`].
    fn write_data(&self, request: DataWrite<'_>) -> Result<DataReceipt, DataStoreError>;
}
