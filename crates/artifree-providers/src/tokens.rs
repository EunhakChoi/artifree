//! `SQLite` implementation of token storage.
//!
//! Stores access token records with hashed secrets. Plaintext secrets are
//! never stored in the database.

use std::path::{Path, PathBuf};
use std::time::Duration;

use artifree_domain::identity::TokenId;
use artifree_ports::token_store::{NewTokenRecord, TokenRecord, TokenStore, TokenStoreError};
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension};

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

    fn connect(&self) -> Result<Connection, TokenStoreError> {
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

impl TokenStore for SqliteTokenStore {
    fn list(&self) -> Result<Vec<TokenRecord>, TokenStoreError> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(
                "SELECT id, name, secret_hash, prefix, created_at, last_used_at, revoked_at \
                 FROM tokens ORDER BY created_at ASC",
            )
            .map_err(unavailable)?;
        let rows = statement
            .query_map([], |row| {
                let id_raw: String = row.get(0)?;
                let name: String = row.get(1)?;
                let secret_hash: String = row.get(2)?;
                let prefix: String = row.get(3)?;
                let created_at: String = row.get(4)?;
                let last_used_at: Option<String> = row.get(5)?;
                let revoked_at: Option<String> = row.get(6)?;
                Ok((
                    id_raw,
                    name,
                    secret_hash,
                    prefix,
                    created_at,
                    last_used_at,
                    revoked_at,
                ))
            })
            .map_err(unavailable)?;

        let mut records = Vec::new();
        for row in rows {
            let (id_raw, name, secret_hash, prefix, created_at, last_used_at, revoked_at) =
                row.map_err(unavailable)?;
            let id = id_raw.parse().map_err(|error| {
                TokenStoreError::Unavailable(format!("stored token id is invalid: {error}"))
            })?;
            records.push(TokenRecord {
                id,
                name,
                secret_hash,
                prefix,
                created_at,
                last_used_at,
                revoked_at,
            });
        }
        Ok(records)
    }

    fn create(&self, token: &NewTokenRecord) -> Result<TokenRecord, TokenStoreError> {
        let connection = self.connect()?;
        let result = connection.execute(
            "INSERT INTO tokens (id, name, secret_hash, prefix, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                token.id.as_str(),
                &token.name,
                &token.secret_hash,
                &token.prefix,
                &token.created_at,
            ),
        );
        match result {
            Ok(_) => Ok(TokenRecord {
                id: token.id.clone(),
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
                "SELECT id, name, secret_hash, prefix, created_at, last_used_at, revoked_at \
                 FROM tokens WHERE secret_hash = ?1",
                [secret_hash],
                |row| {
                    let id_raw: String = row.get(0)?;
                    let name: String = row.get(1)?;
                    let hash: String = row.get(2)?;
                    let prefix: String = row.get(3)?;
                    let created_at: String = row.get(4)?;
                    let last_used_at: Option<String> = row.get(5)?;
                    let revoked_at: Option<String> = row.get(6)?;
                    Ok((
                        id_raw,
                        name,
                        hash,
                        prefix,
                        created_at,
                        last_used_at,
                        revoked_at,
                    ))
                },
            )
            .optional()
            .map_err(unavailable)?;

        let Some((id_raw, name, hash, prefix, created_at, last_used_at, revoked_at)) = row else {
            return Ok(None);
        };
        let id = id_raw.parse().map_err(|error| {
            TokenStoreError::Unavailable(format!("stored token id is invalid: {error}"))
        })?;
        Ok(Some(TokenRecord {
            id,
            name,
            secret_hash: hash,
            prefix,
            created_at,
            last_used_at,
            revoked_at,
        }))
    }

    fn rotate(
        &self,
        id: &TokenId,
        new_secret_hash: &str,
        new_prefix: &str,
    ) -> Result<TokenRecord, TokenStoreError> {
        let connection = self.connect()?;
        // Check if token exists and is active
        let existing = self
            .list()?
            .into_iter()
            .find(|record| record.id == *id)
            .ok_or_else(|| TokenStoreError::NotFound(id.to_string()))?;
        if existing.revoked_at.is_some() {
            return Err(TokenStoreError::Conflict(
                "cannot rotate a revoked token".into(),
            ));
        }

        let result = connection.execute(
            "UPDATE tokens SET secret_hash = ?1, prefix = ?2, last_used_at = NULL \
             WHERE id = ?3 AND revoked_at IS NULL",
            (new_secret_hash, new_prefix, id.as_str()),
        );
        match result {
            Ok(rows) if rows > 0 => Ok(TokenRecord {
                id: id.clone(),
                name: existing.name,
                secret_hash: new_secret_hash.to_owned(),
                prefix: new_prefix.to_owned(),
                created_at: existing.created_at,
                last_used_at: None,
                revoked_at: None,
            }),
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
