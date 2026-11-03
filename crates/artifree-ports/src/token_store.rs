//! Storage port for access tokens.
//!
//! Preserves credential lifecycle across restarts: issuance, listing,
//! rotation, and revocation. Secrets are never stored or logged in plaintext.

use std::fmt;

use artifree_domain::identity::{ActorId, StateGeneration, TokenId};

/// A stored token record. Secret plaintext is never stored or returned here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRecord {
    /// Opaque token identifier.
    pub id: TokenId,
    /// The one user this token authenticates as, for the token's whole life
    /// (decision 0036). A rotation keeps it; nothing moves a token to
    /// another user.
    pub actor: ActorId,
    /// Whether that user is disabled now. Read together with the token, in
    /// one statement, so that the answer to "may this token authenticate" is
    /// never made of two reads.
    pub actor_disabled: bool,
    /// The user's state generation in the same read. A request that
    /// authenticates with this token carries it, and confirms it again after
    /// it registers something that outlives the request.
    pub actor_generation: StateGeneration,
    /// Human-readable label for this token.
    pub name: String,
    /// Hex-encoded SHA-256 hash of the secret bearer token.
    pub secret_hash: String,
    /// Masked prefix string for identification without revealing secret.
    pub prefix: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 last-used timestamp, if used.
    pub last_used_at: Option<String>,
    /// ISO 8601 revocation timestamp, if revoked.
    pub revoked_at: Option<String>,
}

impl TokenRecord {
    /// Whether this token may authenticate a request: it is not revoked and
    /// its user is not disabled.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none() && !self.actor_disabled
    }
}

/// Parameters for creating a new token record.
#[derive(Debug, Clone)]
pub struct NewTokenRecord {
    /// Unique identifier for the token.
    pub id: TokenId,
    /// The registered, enabled user the token authenticates as.
    pub actor: ActorId,
    /// Human-readable label for this token.
    pub name: String,
    /// Hex-encoded SHA-256 hash of the secret bearer token.
    pub secret_hash: String,
    /// Masked prefix string.
    pub prefix: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// Errors from token storage operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenStoreError {
    /// The requested token does not exist, or the user a new token names is
    /// not registered.
    NotFound(String),
    /// A conflict occurred (e.g. secret hash already exists, or the user a
    /// new token names is disabled).
    Conflict(String),
    /// Storage is unavailable or returned an internal error.
    Unavailable(String),
}

impl fmt::Display for TokenStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(msg) => write!(formatter, "token not found: {msg}"),
            Self::Conflict(msg) => write!(formatter, "token conflict: {msg}"),
            Self::Unavailable(msg) => write!(formatter, "token storage unavailable: {msg}"),
        }
    }
}

impl std::error::Error for TokenStoreError {}

/// Port for persisting and querying access tokens.
pub trait TokenStore: Send + Sync {
    /// Lists all tokens (both active and revoked).
    ///
    /// # Errors
    ///
    /// When storage is unavailable.
    fn list(&self) -> Result<Vec<TokenRecord>, TokenStoreError>;

    /// Stores a new token record.
    ///
    /// # Errors
    ///
    /// When a token with the same ID or secret hash already exists, the user
    /// is not registered ([`TokenStoreError::NotFound`]) or disabled
    /// ([`TokenStoreError::Conflict`]), or storage fails.
    fn create(&self, token: &NewTokenRecord) -> Result<TokenRecord, TokenStoreError>;

    /// Finds a token by its secret hash.
    ///
    /// # Errors
    ///
    /// When storage is unavailable.
    fn find_by_hash(&self, secret_hash: &str) -> Result<Option<TokenRecord>, TokenStoreError>;

    /// Rotates a token's secret, updating `secret_hash` and prefix.
    ///
    /// # Errors
    ///
    /// When the token does not exist or storage is unavailable.
    fn rotate(
        &self,
        id: &TokenId,
        new_secret_hash: &str,
        new_prefix: &str,
    ) -> Result<TokenRecord, TokenStoreError>;

    /// Revokes a token by setting its `revoked_at` timestamp.
    ///
    /// # Errors
    ///
    /// When the token does not exist or storage is unavailable.
    fn revoke(&self, id: &TokenId, revoked_at: &str) -> Result<(), TokenStoreError>;

    /// Updates `last_used_at` timestamp.
    ///
    /// # Errors
    ///
    /// When storage is unavailable.
    fn touch(&self, id: &TokenId, last_used_at: &str) -> Result<(), TokenStoreError>;
}
