//! Application data in the artifact database (decision 0031).
//!
//! A space is one row of `app_data` per artifact and actor: its canonical JSON
//! text and its version. A write is one `BEGIN IMMEDIATE` transaction that
//! checks the artifact, looks up the key, compares the base version, applies the
//! change to the space it just read, writes the row, and records the key, so
//! two connections cannot both pass the same base version. A space nobody wrote
//! has no row and is at version 0.
//!
//! Neither table holds a content digest or a blob. A purge deletes the rows of
//! the artifact's spaces and blanks the fingerprints of its keys, both inside
//! the purge transaction and with `secure_delete` on.

use artifree_domain::AppDataVersion;
use artifree_domain::data::DataSpace;
use artifree_domain::identity::{ActorId, ArtifactId};
use artifree_ports::artifact_store::{Delivery, RequestIdentity, StoreError};
use artifree_ports::data_store::{DataReceipt, DataSnapshot, DataStore, DataStoreError, DataWrite};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::jobs::artifact_in_place;
use super::{SqliteStore, corrupt, key_used_elsewhere, unavailable};

/// The space of `actor` as stored, or the empty space at version 0.
fn read_space(
    connection: &Connection,
    artifact: &ArtifactId,
    actor: &ActorId,
) -> Result<DataSnapshot, StoreError> {
    let stored: Option<(i64, String)> = connection
        .query_row(
            "SELECT data_version, document FROM app_data WHERE artifact_id = ?1 AND actor = ?2",
            params![artifact.as_str(), actor.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((version, document)) = stored else {
        return Ok(DataSnapshot {
            version: AppDataVersion::EMPTY,
            space: DataSpace::empty(),
        });
    };
    Ok(DataSnapshot {
        version: AppDataVersion(u64::try_from(version).map_err(corrupt)?),
        space: DataSpace::from_canonical(&document).map_err(corrupt)?,
    })
}

/// What the actor's key says.
enum Key {
    Unused,
    /// The same request ran and left the space at this version.
    Same {
        version: AppDataVersion,
        changed: bool,
    },
    /// Another request, of any kind, used the key.
    Reused,
}

fn key_of(connection: &Connection, request: RequestIdentity<'_>) -> Result<Key, StoreError> {
    let stored: Option<(Vec<u8>, i64, bool)> = connection
        .query_row(
            "SELECT fingerprint, data_version, changed FROM data_requests
             WHERE actor = ?1 AND request_key = ?2",
            params![request.actor.as_str(), request.key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(unavailable)?;
    if let Some((fingerprint, version, changed)) = stored {
        return if fingerprint.as_slice() == request.fingerprint.0.as_slice() {
            Ok(Key::Same {
                version: AppDataVersion(u64::try_from(version).map_err(corrupt)?),
                changed,
            })
        } else {
            Ok(Key::Reused)
        };
    }
    // One key space per actor (decisions 0029, 0030, and 0031).
    if key_used_elsewhere(connection, request.actor, request.key, "data_requests")? {
        return Ok(Key::Reused);
    }
    Ok(Key::Unused)
}

fn write(
    transaction: &Transaction<'_>,
    request: DataWrite<'_>,
) -> Result<DataReceipt, DataStoreError> {
    // The target first, as every write does (decision 0013): data of an
    // artifact in the trash or purged is not found, a retry included.
    if !artifact_in_place(transaction, request.artifact)? {
        return Err(DataStoreError::NotFound);
    }
    match key_of(transaction, request.request)? {
        Key::Same { version, changed } => {
            return Ok(DataReceipt {
                version,
                changed,
                delivery: Delivery::Replayed,
            });
        }
        Key::Reused => return Err(DataStoreError::RequestKeyReused),
        Key::Unused => {}
    }
    let current = read_space(transaction, request.artifact, request.request.actor)?;
    if current.version != request.base {
        return Err(DataStoreError::VersionConflict {
            current: current.version,
        });
    }
    let applied = current
        .space
        .apply(request.change)
        .map_err(DataStoreError::Rejected)?;
    let version = if applied.changed {
        let next = current
            .version
            .0
            .checked_add(1)
            .ok_or_else(|| corrupt("the data version is out of range"))?;
        transaction
            .execute(
                "INSERT INTO app_data (artifact_id, actor, data_version, document)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (artifact_id, actor) DO UPDATE
                     SET data_version = excluded.data_version, document = excluded.document",
                params![
                    request.artifact.as_str(),
                    request.request.actor.as_str(),
                    i64::try_from(next).map_err(corrupt)?,
                    applied.space.to_canonical()
                ],
            )
            .map_err(unavailable)?;
        AppDataVersion(next)
    } else {
        current.version
    };
    transaction
        .execute(
            "INSERT INTO data_requests
                 (actor, request_key, fingerprint, artifact_id, data_version, changed, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                request.request.actor.as_str(),
                request.request.key.as_str(),
                request.request.fingerprint.0.as_slice(),
                request.artifact.as_str(),
                i64::try_from(version.0).map_err(corrupt)?,
                applied.changed,
                request.now.0
            ],
        )
        .map_err(unavailable)?;
    Ok(DataReceipt {
        version,
        changed: applied.changed,
        delivery: Delivery::Committed,
    })
}

/// Copies the data space the actor has for `source` into `copy`, as version 1.
/// A space with no members is not copied: the copy's space stays unwritten.
pub(super) fn copy_space(
    transaction: &Transaction<'_>,
    source: &ArtifactId,
    copy: &ArtifactId,
    actor: &ActorId,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO app_data (artifact_id, actor, data_version, document)
             SELECT ?2, actor, 1, document FROM app_data
             WHERE artifact_id = ?1 AND actor = ?3 AND document <> '{}'",
            params![source.as_str(), copy.as_str(), actor.as_str()],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Erases what a purge must erase of an artifact's application data: every
/// space, and the fingerprints of the keys that wrote them. A fingerprint
/// hashes the written value, so it would let anyone with the file confirm a
/// guess about data that was removed for good. The key rows stay, with a zero
/// fingerprint: the key stays reserved, and no request can match it, which
/// changes nothing observable because an artifact that is gone answers `not
/// found` before any key is looked at.
pub(super) fn erase_for_purge(
    transaction: &Transaction<'_>,
    artifact: &ArtifactId,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "DELETE FROM app_data WHERE artifact_id = ?1",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "UPDATE data_requests SET fingerprint = zeroblob(32) WHERE artifact_id = ?1",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    Ok(())
}

impl DataStore for SqliteStore {
    fn read_data(
        &self,
        artifact: &ArtifactId,
        actor: &ActorId,
    ) -> Result<DataSnapshot, DataStoreError> {
        let mut connection = self.connect()?;
        // One snapshot: the artifact check and the space agree.
        let snapshot = connection.transaction().map_err(unavailable)?;
        if !artifact_in_place(&snapshot, artifact)? {
            return Err(DataStoreError::NotFound);
        }
        Ok(read_space(&snapshot, artifact, actor)?)
    }

    fn write_data(&self, request: DataWrite<'_>) -> Result<DataReceipt, DataStoreError> {
        // Erasing: a value that a write replaces or removes is overwritten in
        // the file's free pages as it goes, not left there until they are
        // reused (decision 0031).
        self.write_erasing(|transaction| write(transaction, request))
    }
}
