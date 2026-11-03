//! `SQLite` implementation of the registry of users (decision 0036).
//!
//! It shares the database file, and the opener, of the token store: a token
//! and the user it belongs to are read and written together.

use artifree_domain::identity::ActorId;
use artifree_ports::token_store::TokenStoreError;
use artifree_ports::user_store::{MAX_USERS, NewUser, UserRecord, UserStore, UserStoreError};
use rusqlite::{ErrorCode, OptionalExtension, Row, TransactionBehavior};

use crate::tokens::SqliteTokenStore;

const USER_COLUMNS: &str = "SELECT id, label, created_at, disabled_at FROM users";

fn user_row(row: &Row<'_>) -> rusqlite::Result<(String, String, String, Option<String>)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn user_record(
    (id, label, created_at, disabled_at): (String, String, String, Option<String>),
) -> Result<UserRecord, UserStoreError> {
    Ok(UserRecord {
        id: id.parse().map_err(|error| {
            UserStoreError::Unavailable(format!("stored user is invalid: {error}"))
        })?,
        label,
        created_at,
        disabled_at,
    })
}

impl UserStore for SqliteTokenStore {
    fn users(&self) -> Result<Vec<UserRecord>, UserStoreError> {
        let connection = self.connect().map_err(unavailable_token)?;
        // The owner first: its row is made by the schema, before any other.
        let mut statement = connection
            .prepare(&format!(
                "{USER_COLUMNS} ORDER BY id <> 'local', created_at, id"
            ))
            .map_err(unavailable)?;
        let rows = statement.query_map([], user_row).map_err(unavailable)?;
        let mut users = Vec::new();
        for row in rows {
            users.push(user_record(row.map_err(unavailable)?)?);
        }
        Ok(users)
    }

    fn user(&self, id: &ActorId) -> Result<Option<UserRecord>, UserStoreError> {
        let connection = self.connect().map_err(unavailable_token)?;
        connection
            .query_row(
                &format!("{USER_COLUMNS} WHERE id = ?1"),
                [id.as_str()],
                user_row,
            )
            .optional()
            .map_err(unavailable)?
            .map(user_record)
            .transpose()
    }

    fn register(&self, user: &NewUser) -> Result<UserRecord, UserStoreError> {
        if !user.id.is_registrable() || user.id.is_owner() {
            return Err(UserStoreError::Conflict(
                "only a generated user id can be registered".into(),
            ));
        }
        let mut connection = self.connect().map_err(unavailable_token)?;
        // The count and the insert are one write transaction, so two
        // registrations cannot both take the last place.
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let count: i64 = transaction
            .query_row("SELECT count(*) FROM users", [], |row| row.get(0))
            .map_err(unavailable)?;
        if usize::try_from(count).map_or(true, |count| count >= MAX_USERS) {
            return Err(UserStoreError::LimitReached);
        }
        let inserted = transaction.execute(
            "INSERT INTO users (id, label, created_at) VALUES (?1, ?2, ?3)",
            (user.id.as_str(), user.label.as_str(), &user.created_at),
        );
        match inserted {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == ErrorCode::ConstraintViolation =>
            {
                return Err(UserStoreError::Conflict(
                    "a user with that id or label is registered already".into(),
                ));
            }
            Err(error) => return Err(unavailable(error)),
        }
        transaction.commit().map_err(unavailable)?;
        Ok(UserRecord {
            id: user.id.clone(),
            label: user.label.as_str().to_owned(),
            created_at: user.created_at.clone(),
            disabled_at: None,
        })
    }

    fn set_disabled(&self, id: &ActorId, at: Option<&str>) -> Result<UserRecord, UserStoreError> {
        if id.is_owner() {
            return Err(UserStoreError::Conflict(
                "the owner profile cannot be disabled".into(),
            ));
        }
        let mut connection = self.connect().map_err(unavailable_token)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        // Disabling keeps the first timestamp; enabling clears it.
        let changed = match at {
            Some(at) => transaction.execute(
                "UPDATE users SET disabled_at = coalesce(disabled_at, ?1) WHERE id = ?2",
                (at, id.as_str()),
            ),
            None => transaction.execute(
                "UPDATE users SET disabled_at = NULL WHERE id = ?1",
                [id.as_str()],
            ),
        }
        .map_err(unavailable)?;
        if changed == 0 {
            return Err(UserStoreError::NotFound(id.to_string()));
        }
        let record = transaction
            .query_row(
                &format!("{USER_COLUMNS} WHERE id = ?1"),
                [id.as_str()],
                user_row,
            )
            .map_err(unavailable)?;
        transaction.commit().map_err(unavailable)?;
        user_record(record)
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Used as a map_err adapter, which passes the error by value."
)]
fn unavailable(error: rusqlite::Error) -> UserStoreError {
    UserStoreError::Unavailable(error.to_string())
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Used as a map_err adapter, which passes the error by value."
)]
fn unavailable_token(error: TokenStoreError) -> UserStoreError {
    UserStoreError::Unavailable(error.to_string())
}
