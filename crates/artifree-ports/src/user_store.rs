//! Storage port for the registry of users (decision 0036).
//!
//! A user is an actor that may own artifacts, with a label, and can be
//! disabled. The registry holds the local owner profile and the users it
//! registers; it holds no credential (tokens belong to one user each, see
//! [`crate::token_store`]) and no role.

use std::fmt;

use artifree_domain::identity::{ActorId, StateGeneration, UserLabel};

/// The most users the registry holds, the owner and disabled users included.
/// Users are never deleted, and per-user in-memory limits (decision 0036,
/// decision 11) multiply by this number, so it is the bound of that memory.
pub const MAX_USERS: usize = 32;

/// A registered user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRecord {
    /// The actor this user authenticates as.
    pub id: ActorId,
    /// The display label the owner gave; no two users have the same.
    pub label: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 timestamp of the disabling, while the user is disabled.
    pub disabled_at: Option<String>,
    /// Raised by one every time the user is disabled or enabled, never
    /// lowered, and kept across restarts. See [`StateGeneration`].
    pub state_generation: StateGeneration,
}

impl UserRecord {
    /// Whether the user's tokens may authenticate.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.disabled_at.is_none()
    }
}

/// A user to register.
#[derive(Debug, Clone)]
pub struct NewUser {
    /// The actor, built by [`ActorId::user`]; the owner is never created.
    pub id: ActorId,
    /// The display label.
    pub label: UserLabel,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// Errors from user storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserStoreError {
    /// No such registered user.
    NotFound(String),
    /// The change is refused by a rule of the registry: the actor is not a
    /// spelling the registry can hold, the actor or the label is held
    /// already, or the owner would be disabled.
    Conflict(String),
    /// [`MAX_USERS`] users are registered.
    LimitReached,
    /// Storage failed.
    Unavailable(String),
}

impl fmt::Display for UserStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(msg) => write!(formatter, "user not found: {msg}"),
            Self::Conflict(msg) => write!(formatter, "user conflict: {msg}"),
            Self::LimitReached => write!(formatter, "at most {MAX_USERS} users can be registered"),
            Self::Unavailable(msg) => write!(formatter, "user storage unavailable: {msg}"),
        }
    }
}

impl std::error::Error for UserStoreError {}

/// Port for the registry of users.
pub trait UserStore: Send + Sync {
    /// Every registered user, the owner first, then by creation order.
    ///
    /// # Errors
    ///
    /// [`UserStoreError::Unavailable`].
    fn users(&self) -> Result<Vec<UserRecord>, UserStoreError>;

    /// One registered user.
    ///
    /// # Errors
    ///
    /// [`UserStoreError::Unavailable`].
    fn user(&self, id: &ActorId) -> Result<Option<UserRecord>, UserStoreError>;

    /// Registers `user`, enabled.
    ///
    /// # Errors
    ///
    /// [`UserStoreError::Conflict`] for an actor the registry cannot hold
    /// (see [`ActorId::is_registrable`]), the owner's, or an actor or label
    /// registered already; [`UserStoreError::LimitReached`]; [`UserStoreError::Unavailable`].
    fn register(&self, user: &NewUser) -> Result<UserRecord, UserStoreError>;

    /// Disables the user at `at`, or enables it with `None`. Each change
    /// raises the user's [`StateGeneration`] by one in the same write.
    /// Disabling a disabled user keeps its first timestamp, and enabling an
    /// enabled user changes nothing: neither raises the generation.
    ///
    /// # Errors
    ///
    /// [`UserStoreError::NotFound`]; [`UserStoreError::Conflict`] for the
    /// owner, which cannot be disabled; [`UserStoreError::Unavailable`].
    fn set_disabled(&self, id: &ActorId, at: Option<&str>) -> Result<UserRecord, UserStoreError>;
}
