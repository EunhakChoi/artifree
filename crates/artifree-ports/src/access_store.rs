//! Durable storage of who may reach an artifact besides its owner (decision
//! 0060, slice S02a).
//!
//! An artifact's members and their roles, the artifact's access version, and
//! each (artifact, user)'s access generation. A change of access is one atomic
//! step: the artifact and its owner, the key, the base version, the user, the
//! change, the version, the generation and the key record are all decided on
//! the state the step reads, so two owners' tabs cannot both pass a base check.
//!
//! Nothing in this slice reads these records to authorize a request: the
//! application core does that from S02b on (0060 rules 3 to 5).

use std::fmt;

use artifree_domain::access::{AccessGeneration, AccessVersion, Role};
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis, UserLabel};

use crate::artifact_store::{Delivery, RequestIdentity, StoreError};

/// A member of an artifact and the role the owner gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The member.
    pub user: ActorId,
    /// The member's label, which the owner and members see (0060 rule 8),
    /// or the member's id for an actor the registry never saw.
    pub label: UserLabel,
    /// What the member may do (0060 rule 3).
    pub role: Role,
    /// Whether the member's user is disabled now (0060 rule 18). Only the
    /// owner is told this (0060 rule 6).
    pub disabled: bool,
}

/// Who may reach an artifact: its owner and its current members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessList {
    /// The artifact's owner.
    pub owner: ActorId,
    /// The owner's label, or the owner's id for an actor the registry never
    /// saw (decision 0036 lets a database name one).
    pub owner_label: UserLabel,
    /// The members, ordered by user id.
    pub members: Vec<Member>,
    /// The artifact's access version.
    pub version: AccessVersion,
    /// Whether the artifact is in the trash, where no member reaches it.
    pub in_trash: bool,
}

/// What a change of access does to one user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessChange {
    /// Makes the user a member with this role, or changes their role to it.
    Grant(Role),
    /// Ends the user's membership.
    Revoke,
}

/// A change of one user's access to an artifact, made by its owner.
#[derive(Debug, Clone, Copy)]
pub struct AccessRequest<'a> {
    /// Retry identity, in the owner's key space; its actor must be the owner.
    pub request: RequestIdentity<'a>,
    /// The artifact. It must be outside the trash and not purged.
    pub artifact: &'a ArtifactId,
    /// The user whose access changes.
    pub user: &'a ActorId,
    /// The change.
    pub change: AccessChange,
    /// The access version the request was prepared against.
    pub base: AccessVersion,
    /// When the request is made, from the caller's clock.
    pub now: UnixMillis,
}

/// The durable result of a change of access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessReceipt {
    /// The access version the request left the artifact at. A grant of the
    /// role the member already has changes nothing and leaves it where it was.
    pub version: AccessVersion,
    /// Whether this call wrote it.
    pub delivery: Delivery,
}

/// Why a change of access was refused. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessStoreError {
    /// The artifact does not exist, is in the trash, was purged, or is not the
    /// caller's own. A retry of a request whose artifact went is answered so too.
    NotFound,
    /// The actor already used the key for another request.
    RequestKeyReused,
    /// The artifact is not at the base version.
    VersionConflict {
        /// The version it is at.
        current: AccessVersion,
    },
    /// A grant to a user who is not registered, is the owner, or is disabled
    /// and not already a member: one answer for all three (0060 rule 6).
    NotGrantable,
    /// A revocation of a user who is not a member.
    NotMember,
    /// Storage could not complete the operation.
    Unavailable(String),
}

impl fmt::Display for AccessStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("artifact not found"),
            Self::RequestKeyReused => formatter.write_str("request key reused"),
            Self::VersionConflict { current } => {
                write!(formatter, "access is at version {current}")
            }
            Self::NotGrantable => formatter.write_str("the user cannot be granted access"),
            Self::NotMember => formatter.write_str("the user is not a member"),
            Self::Unavailable(message) => write!(formatter, "storage unavailable: {message}"),
        }
    }
}

impl std::error::Error for AccessStoreError {}

impl From<StoreError> for AccessStoreError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self::NotFound,
            StoreError::RequestKeyReused => Self::RequestKeyReused,
            StoreError::Unavailable(reason) => Self::Unavailable(reason),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

/// Synchronous storage of artifact access.
pub trait AccessStore {
    /// The artifact's owner, members and access version; `None` when the
    /// artifact does not exist or was purged. An artifact in the trash keeps
    /// its list.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn access_list(&self, artifact: &ArtifactId) -> Result<Option<AccessList>, StoreError>;

    /// The role of `user` on the artifact, `None` when the user is not a
    /// member (the owner included), and for every member while the artifact
    /// is in the trash or after it was purged: no member reaches it then
    /// (0060 rule 4).
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn member_role(
        &self,
        artifact: &ArtifactId,
        user: &ActorId,
    ) -> Result<Option<Role>, StoreError>;

    /// The access generation of `user` on the artifact (0060 rule 16);
    /// [`AccessGeneration::NEVER`] for a user who was never a member. It is
    /// read in the trash too, where a move raised it.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`].
    fn access_generation(
        &self,
        artifact: &ArtifactId,
        user: &ActorId,
    ) -> Result<AccessGeneration, StoreError>;

    /// Grants, changes or revokes one user's access, conditional on the base
    /// version, and records the key; or replays the same request.
    ///
    /// # Errors
    ///
    /// Every [`AccessStoreError`], in this order: a missing target (a retry
    /// included, as for a data write), then a replay, then a reused key, then
    /// the base version, then the user.
    fn change_access(&self, request: AccessRequest<'_>) -> Result<AccessReceipt, AccessStoreError>;
}
