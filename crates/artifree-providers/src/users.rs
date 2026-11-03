//! `SQLite` implementation of the registry of users (decision 0036).
//!
//! It shares the database file, and the opener, of the token store: a token
//! and the user it belongs to are read and written together.

use artifree_domain::identity::{ActorId, StateGeneration};
use artifree_ports::token_store::TokenStoreError;
use artifree_ports::user_store::{MAX_USERS, NewUser, UserRecord, UserStore, UserStoreError};
use rusqlite::{ErrorCode, OptionalExtension, Row, TransactionBehavior};

use crate::tokens::SqliteTokenStore;

const USER_COLUMNS: &str = "SELECT id, label, created_at, disabled_at, state_generation FROM users";

/// The values of one row of [`USER_COLUMNS`], before they are parsed.
type UserRow = (String, String, String, Option<String>, i64);

fn user_row(row: &Row<'_>) -> rusqlite::Result<UserRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn user_record(
    (id, label, created_at, disabled_at, generation): UserRow,
) -> Result<UserRecord, UserStoreError> {
    Ok(UserRecord {
        id: id.parse().map_err(|error| {
            UserStoreError::Unavailable(format!("stored user is invalid: {error}"))
        })?,
        label,
        created_at,
        disabled_at,
        state_generation: stored_generation(generation).map_err(UserStoreError::Unavailable)?,
    })
}

/// The generation in a stored `INTEGER`; zero and negative values are not one.
pub(crate) fn stored_generation(value: i64) -> Result<StateGeneration, String> {
    u64::try_from(value)
        .ok()
        .and_then(StateGeneration::new)
        .ok_or_else(|| format!("stored state generation {value} is invalid"))
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
            state_generation: StateGeneration::FIRST,
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
        // Disabling keeps the first timestamp; enabling clears it. A change
        // of state raises the generation in the same statement; repeating the
        // state the user is in changes neither.
        let update = match at {
            Some(at) => transaction.execute(
                "UPDATE users SET disabled_at = ?1, state_generation = state_generation + 1 \
                 WHERE id = ?2 AND disabled_at IS NULL",
                (at, id.as_str()),
            ),
            None => transaction.execute(
                "UPDATE users SET disabled_at = NULL, state_generation = state_generation + 1 \
                 WHERE id = ?1 AND disabled_at IS NOT NULL",
                [id.as_str()],
            ),
        };
        update.map_err(unavailable)?;
        let record = transaction
            .query_row(
                &format!("{USER_COLUMNS} WHERE id = ?1"),
                [id.as_str()],
                user_row,
            )
            .optional()
            .map_err(unavailable)?
            .ok_or_else(|| UserStoreError::NotFound(id.to_string()))?;
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
