//! `SQLite` implementation of token storage.
//!
//! Stores access token records with hashed secrets. Plaintext secrets are
//! never stored in the database.

use std::path::{Path, PathBuf};
use std::time::Duration;

use artifree_domain::identity::TokenId;
use artifree_ports::token_store::{NewTokenRecord, TokenRecord, TokenStore, TokenStoreError};
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior};

use crate::schema::SCHEMA_VERSION;
use sha2::{Digest, Sha256};

const DIGITS: &[u8; 16] = b"0123456789abcdef";
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Computes the SHA-256 hex digest of a token secret string.
#[must_use]
pub fn hash_token_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    let result = hasher.finalize();
    let mut text = String::with_capacity(result.len() * 2);
    for byte in result {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// Token storage backed by a local `SQLite` database file.
#[derive(Debug, Clone)]
pub struct SqliteTokenStore {
    path: PathBuf,
}

impl SqliteTokenStore {
    /// Opens token storage in the database at `path`. The file must already be
    /// at the current schema version, which [`SqliteStore::open`] creates and
    /// upgrades together with the `tokens` table; this opener changes nothing,
    /// so it leaves a newer, older, or foreign file as it found it.
    ///
    /// [`SqliteStore::open`]: crate::storage::SqliteStore::open
    ///
    /// # Errors
    ///
    /// When the file does not exist, cannot be opened, or is not at
    /// [`SCHEMA_VERSION`].
    pub fn open(path: &Path) -> Result<Self, TokenStoreError> {
        let store = Self {
            path: path.to_owned(),
        };
        let connection = store.connect()?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(unavailable)?;
        if version != SCHEMA_VERSION {
            return Err(TokenStoreError::Unavailable(format!(
                "database schema version {version} is not the version {SCHEMA_VERSION} this build uses; open the artifact store first to upgrade it"
            )));
        }
        Ok(store)
    }

    pub(crate) fn connect(&self) -> Result<Connection, TokenStoreError> {
        // Never creates the file: the artifact store owns it.
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(unavailable)?;
        connection.busy_timeout(BUSY_TIMEOUT).map_err(unavailable)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(unavailable)?;
        Ok(connection)
    }
}

/// The columns of a [`TokenRecord`], joined with the user the token belongs
/// to. The join is inner on purpose: a token whose user is missing from the
/// registry is not listed and does not authenticate.
const TOKEN_COLUMNS: &str = "SELECT t.id, t.actor, u.disabled_at IS NOT NULL, t.name, \
     t.secret_hash, t.prefix, t.created_at, t.last_used_at, t.revoked_at \
     FROM tokens t JOIN users u ON u.id = t.actor";

/// The values of one row of [`TOKEN_COLUMNS`], before they are parsed.
type TokenRow = (
    String,
    String,
    bool,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

fn token_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TokenRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn token_record(row: TokenRow) -> Result<TokenRecord, TokenStoreError> {
    let (
        id,
        actor,
        actor_disabled,
        name,
        secret_hash,
        prefix,
        created_at,
        last_used_at,
        revoked_at,
    ) = row;
    Ok(TokenRecord {
        id: id.parse().map_err(|error| {
            TokenStoreError::Unavailable(format!("stored token id is invalid: {error}"))
        })?,
        actor: actor.parse().map_err(|error| {
            TokenStoreError::Unavailable(format!("stored token user is invalid: {error}"))
        })?,
        actor_disabled,
        name,
        secret_hash,
        prefix,
        created_at,
        last_used_at,
        revoked_at,
    })
}

/// The token `id` with its user, read through `connection`.
fn token_by_id(
    connection: &Connection,
    id: &TokenId,
) -> Result<Option<TokenRecord>, TokenStoreError> {
    connection
        .query_row(
            &format!("{TOKEN_COLUMNS} WHERE t.id = ?1"),
            [id.as_str()],
            token_row,
        )
        .optional()
        .map_err(unavailable)?
        .map(token_record)
        .transpose()
}

impl TokenStore for SqliteTokenStore {
    fn list(&self) -> Result<Vec<TokenRecord>, TokenStoreError> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(&format!("{TOKEN_COLUMNS} ORDER BY t.created_at ASC"))
            .map_err(unavailable)?;
        let rows = statement.query_map([], token_row).map_err(unavailable)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(token_record(row.map_err(unavailable)?)?);
        }
        Ok(records)
    }

    fn create(&self, token: &NewTokenRecord) -> Result<TokenRecord, TokenStoreError> {
        let connection = self.connect()?;
        // One statement: the token exists only if its user is registered and
        // enabled at the moment of the insert.
        let result = connection.execute(
            "INSERT INTO tokens (id, actor, name, secret_hash, prefix, created_at) \
             SELECT ?1, u.id, ?3, ?4, ?5, ?6 FROM users u \
             WHERE u.id = ?2 AND u.disabled_at IS NULL",
            (
                token.id.as_str(),
                token.actor.as_str(),
                &token.name,
                &token.secret_hash,
                &token.prefix,
                &token.created_at,
            ),
        );
        match result {
            Ok(0) => {
                let disabled: Option<bool> = connection
                    .query_row(
                        "SELECT disabled_at IS NOT NULL FROM users WHERE id = ?1",
                        [token.actor.as_str()],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(unavailable)?;
                Err(match disabled {
                    None => TokenStoreError::NotFound(format!("user {}", token.actor)),
                    Some(_) => TokenStoreError::Conflict("the user is disabled".into()),
                })
            }
            Ok(_) => Ok(TokenRecord {
                id: token.id.clone(),
                actor: token.actor.clone(),
                actor_disabled: false,
                name: token.name.clone(),
                secret_hash: token.secret_hash.clone(),
                prefix: token.prefix.clone(),
                created_at: token.created_at.clone(),
                last_used_at: None,
                revoked_at: None,
            }),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == ErrorCode::ConstraintViolation =>
            {
                Err(TokenStoreError::Conflict(
                    "token with same ID or secret already exists".into(),
                ))
            }
            Err(err) => Err(unavailable(err)),
        }
    }

    fn find_by_hash(&self, secret_hash: &str) -> Result<Option<TokenRecord>, TokenStoreError> {
        let connection = self.connect()?;
        let row = connection
            .query_row(
                &format!("{TOKEN_COLUMNS} WHERE t.secret_hash = ?1"),
                [secret_hash],
                token_row,
            )
            .optional()
            .map_err(unavailable)?;
        row.map(token_record).transpose()
    }

    fn rotate(
        &self,
        id: &TokenId,
        new_secret_hash: &str,
        new_prefix: &str,
    ) -> Result<TokenRecord, TokenStoreError> {
        let mut connection = self.connect()?;
        // One write transaction: the token that is checked, changed and
        // returned is one snapshot, so the answer says whether the user is
        // disabled now and not when an earlier read happened.
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let existing = token_by_id(&transaction, id)?
            .ok_or_else(|| TokenStoreError::NotFound(id.to_string()))?;
        if existing.revoked_at.is_some() {
            return Err(TokenStoreError::Conflict(
                "cannot rotate a revoked token".into(),
            ));
        }
        // A disabled user's token can be rotated: the old secret stops now,
        // and the new one authenticates only once the user is enabled.
        let result = transaction.execute(
            "UPDATE tokens SET secret_hash = ?1, prefix = ?2, last_used_at = NULL \
             WHERE id = ?3 AND revoked_at IS NULL",
            (new_secret_hash, new_prefix, id.as_str()),
        );
        match result {
            Ok(rows) if rows > 0 => {
                let record = token_by_id(&transaction, id)?
                    .ok_or_else(|| TokenStoreError::NotFound(id.to_string()))?;
                transaction.commit().map_err(unavailable)?;
                Ok(record)
            }
            Ok(_) => Err(TokenStoreError::NotFound(id.to_string())),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == ErrorCode::ConstraintViolation =>
            {
                Err(TokenStoreError::Conflict(
                    "new secret hash already exists".into(),
                ))
            }
            Err(err) => Err(unavailable(err)),
        }
    }

    fn revoke(&self, id: &TokenId, revoked_at: &str) -> Result<(), TokenStoreError> {
        let connection = self.connect()?;
        let rows = connection
            .execute(
                "UPDATE tokens SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
                (revoked_at, id.as_str()),
            )
            .map_err(unavailable)?;
        if rows == 0 {
            // Check if it exists at all
            let exists: bool = connection
                .query_row("SELECT 1 FROM tokens WHERE id = ?1", [id.as_str()], |_| {
                    Ok(true)
                })
                .optional()
                .map_err(unavailable)?
                .unwrap_or(false);
            if !exists {
                return Err(TokenStoreError::NotFound(id.to_string()));
            }
            // Already revoked: idempotent success
        }
        Ok(())
    }

    fn touch(&self, id: &TokenId, last_used_at: &str) -> Result<(), TokenStoreError> {
        let connection = self.connect()?;
        connection
            .execute(
                "UPDATE tokens SET last_used_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
                (last_used_at, id.as_str()),
            )
            .map_err(unavailable)?;
        Ok(())
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Used as a map_err adapter, which passes the error by value."
)]
fn unavailable(error: rusqlite::Error) -> TokenStoreError {
    TokenStoreError::Unavailable(error.to_string())
}

#[cfg(test)]
mod tests {
    use artifree_domain::identity::ActorId;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn token_hash_computation() {
        let hash = hash_token_secret("secret123");
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, hash_token_secret("secret123"));
        assert_ne!(hash, hash_token_secret("secret124"));
    }

    #[test]
    fn lifecycle_and_persistence() -> TestResult {
        let dir = std::env::temp_dir().join(format!("artifree-test-tokens-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join("test.db");

        crate::storage::SqliteStore::open(&db_path)?;
        let store = SqliteTokenStore::open(&db_path)?;
        let id1 = TokenId::from_random([1; 16]);
        let token1 = NewTokenRecord {
            id: id1.clone(),
            actor: ActorId::owner(),
            name: "agent-1".into(),
            secret_hash: hash_token_secret("art_secret_1"),
            prefix: "art_11111111...".into(),
            created_at: "2026-11-01T00:00:00Z".into(),
        };
        let created = store.create(&token1)?;
        assert_eq!(created.id, id1);
        assert!(created.is_active());

        // List
        let list = store.list()?;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "agent-1");

        // Find by hash
        let found = store
            .find_by_hash(&token1.secret_hash)?
            .ok_or("missing token")?;
        assert_eq!(found.id, id1);
        assert!(store.find_by_hash("nonexistent")?.is_none());

        // Touch
        store.touch(&id1, "2026-11-01T00:05:00Z")?;
        let touched = store
            .find_by_hash(&token1.secret_hash)?
            .ok_or("missing touched token")?;
        assert_eq!(
            touched.last_used_at.as_deref(),
            Some("2026-11-01T00:05:00Z")
        );

        // Rotate
        let new_hash = hash_token_secret("art_secret_1_rotated");
        let rotated = store.rotate(&id1, &new_hash, "art_22222222...")?;
        assert_eq!(rotated.secret_hash, new_hash);
        assert_eq!(rotated.prefix, "art_22222222...");
        assert!(rotated.last_used_at.is_none());
        assert!(store.find_by_hash(&token1.secret_hash)?.is_none());
        assert!(store.find_by_hash(&new_hash)?.is_some());

        // Revoke
        store.revoke(&id1, "2026-11-01T00:10:00Z")?;
        let revoked = store
            .find_by_hash(&new_hash)?
            .ok_or("missing revoked token")?;
        assert_eq!(revoked.revoked_at.as_deref(), Some("2026-11-01T00:10:00Z"));
        assert!(!revoked.is_active());

        // Rotate on revoked token is rejected
        let result = store.rotate(&id1, "another_hash", "art_33333333...");
        assert!(matches!(result, Err(TokenStoreError::Conflict(_))));

        // Reopening database verifies persistence across restart
        let store2 = SqliteTokenStore::open(&db_path)?;
        let list2 = store2.list()?;
        assert_eq!(list2.len(), 1);
        assert_eq!(list2[0].id, id1);
        assert_eq!(list2[0].revoked_at.as_deref(), Some("2026-11-01T00:10:00Z"));

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
