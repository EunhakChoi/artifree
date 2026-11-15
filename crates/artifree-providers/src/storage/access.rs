//! Artifact access in the artifact database (decision 0060, slice S02a).
//!
//! `artifact_members` holds one row per artifact and user that was ever a
//! member: the role, or no role once revoked, and the access generation, which
//! only grows. `artifacts.access_version` counts the changes of an artifact's
//! access. A change is one `BEGIN IMMEDIATE` transaction that checks the
//! artifact and its owner, looks up the key, compares the base version, checks
//! the user, writes the row and the version, and records the key, so two
//! changes cannot both pass the same base version.
//!
//! A move to or from the trash raises every member's generation, and a purge
//! deletes the rows and blanks the fingerprints of the access keys, both inside
//! the transactions of [`super::SqliteStore`] that do those.

use artifree_domain::access::{AccessGeneration, AccessVersion, Role};
use artifree_domain::identity::{ActorId, ArtifactId};
use artifree_ports::access_store::{
    AccessChange, AccessList, AccessReceipt, AccessRequest, AccessStore, AccessStoreError, Member,
};
use artifree_ports::artifact_store::{Delivery, RequestIdentity, StoreError};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{SqliteStore, corrupt, key_used_elsewhere, unavailable};

/// A stored count, which is never negative.
fn count(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(corrupt)
}

/// A count to store.
fn stored(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(corrupt)
}

/// The access version of an artifact that `owner` owns and that is outside the
/// trash and not purged.
fn owned_in_place(
    connection: &Connection,
    artifact: &ArtifactId,
    owner: &ActorId,
) -> Result<Option<AccessVersion>, StoreError> {
    let version: Option<i64> = connection
        .query_row(
            "SELECT access_version FROM artifacts
             WHERE id = ?1 AND owner = ?2 AND purged_at IS NULL AND deleted_at IS NULL",
            params![artifact.as_str(), owner.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    version
        .map(|value| count(value).map(AccessVersion))
        .transpose()
}

/// What the actor's key says about a change of access.
enum Key {
    /// No request used it.
    Unused,
    /// The same request used it and left the artifact at this version.
    Same(AccessVersion),
    /// Another request used it.
    Reused,
}

fn key_of(
    connection: &Connection,
    request: RequestIdentity<'_>,
    artifact: &ArtifactId,
) -> Result<Key, StoreError> {
    let used: Option<(Vec<u8>, String, i64)> = connection
        .query_row(
            "SELECT fingerprint, artifact_id, access_version FROM access_requests
             WHERE actor = ?1 AND request_key = ?2",
            params![request.actor.as_str(), request.key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(unavailable)?;
    if let Some((fingerprint, used_on, version)) = used {
        // The same request names the same artifact: a key used on another
        // artifact is reused even when the rest of the request is the same.
        return if fingerprint.as_slice() == request.fingerprint.0.as_slice()
            && used_on == artifact.as_str()
        {
            Ok(Key::Same(AccessVersion(count(version)?)))
        } else {
            Ok(Key::Reused)
        };
    }
    // One key space per actor (decisions 0029 and 0036).
    if key_used_elsewhere(connection, request.actor, request.key, "access_requests")? {
        return Ok(Key::Reused);
    }
    Ok(Key::Unused)
}

/// The stored record of `user` on the artifact: the role (none once revoked)
/// and the generation; `None` when the user was never a member.
fn member_record(
    connection: &Connection,
    artifact: &ArtifactId,
    user: &ActorId,
) -> Result<Option<(Option<Role>, AccessGeneration)>, StoreError> {
    let row: Option<(Option<String>, i64)> = connection
        .query_row(
            "SELECT role, access_generation FROM artifact_members
             WHERE artifact_id = ?1 AND user_id = ?2",
            params![artifact.as_str(), user.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(unavailable)?;
    row.map(|(role, generation)| {
        let role = role
            .map(|name| name.parse::<Role>().map_err(corrupt))
            .transpose()?;
        Ok((role, AccessGeneration(count(generation)?)))
    })
    .transpose()
}

/// Whether `user` is registered, and if so whether it is disabled.
fn user_disabled(connection: &Connection, user: &ActorId) -> Result<Option<bool>, StoreError> {
    connection
        .query_row(
            "SELECT disabled_at IS NOT NULL FROM users WHERE id = ?1",
            [user.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)
}

/// The role `request` leaves the user with, or why it is refused. `None` in
/// the answer is a revocation; the same role as now is no change at all.
fn decide(
    connection: &Connection,
    request: &AccessRequest<'_>,
    stored: Option<Role>,
) -> Result<Option<Role>, AccessStoreError> {
    match request.change {
        AccessChange::Grant(role) => {
            // The owner and an unknown user are refused alike, and so is a
            // disabled user who is not a member yet; a disabled member's role
            // can still change (decision 0060 rules 6 and 18).
            let grantable = request.user != request.request.actor
                && match user_disabled(connection, request.user)? {
                    None => false,
                    Some(disabled) => !disabled || stored.is_some(),
                };
            if grantable {
                Ok(Some(role))
            } else {
                Err(AccessStoreError::NotGrantable)
            }
        }
        AccessChange::Revoke => {
            if stored.is_some() {
                Ok(None)
            } else {
                Err(AccessStoreError::NotMember)
            }
        }
    }
}

fn change(
    transaction: &Transaction<'_>,
    request: &AccessRequest<'_>,
) -> Result<AccessReceipt, AccessStoreError> {
    // The target first, a retry included, as a data write does (decision
    // 0031): another user's artifact, or one in the trash or purged, is not
    // found.
    let current = owned_in_place(transaction, request.artifact, request.request.actor)?
        .ok_or(AccessStoreError::NotFound)?;
    match key_of(transaction, request.request, request.artifact)? {
        Key::Same(version) => {
            return Ok(AccessReceipt {
                version,
                delivery: Delivery::Replayed,
            });
        }
        Key::Reused => return Err(AccessStoreError::RequestKeyReused),
        Key::Unused => {}
    }
    if current != request.base {
        return Err(AccessStoreError::VersionConflict { current });
    }
    let record = member_record(transaction, request.artifact, request.user)?;
    let stored_role = record.and_then(|(role, _)| role);
    let role = decide(transaction, request, stored_role)?;
    let version = if role == stored_role {
        current
    } else {
        let generation = record.map_or(AccessGeneration::NEVER, |(_, generation)| generation);
        let next_generation = generation
            .0
            .checked_add(1)
            .ok_or_else(|| corrupt("the access generation is out of range"))?;
        transaction
            .execute(
                "INSERT INTO artifact_members (artifact_id, user_id, role, access_generation)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (artifact_id, user_id) DO UPDATE
                     SET role = excluded.role, access_generation = excluded.access_generation",
                params![
                    request.artifact.as_str(),
                    request.user.as_str(),
                    role.map(Role::as_str),
                    stored(next_generation)?
                ],
            )
            .map_err(unavailable)?;
        let next = current
            .0
            .checked_add(1)
            .ok_or_else(|| corrupt("the access version is out of range"))?;
        transaction
            .execute(
                "UPDATE artifacts SET access_version = ?2 WHERE id = ?1",
                params![request.artifact.as_str(), stored(next)?],
            )
            .map_err(unavailable)?;
        AccessVersion(next)
    };
    transaction
        .execute(
            "INSERT INTO access_requests
                 (actor, request_key, fingerprint, artifact_id, access_version, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                request.request.actor.as_str(),
                request.request.key.as_str(),
                request.request.fingerprint.0.as_slice(),
                request.artifact.as_str(),
                stored(version.0)?,
                request.now.0
            ],
        )
        .map_err(unavailable)?;
    Ok(AccessReceipt {
        version,
        delivery: Delivery::Committed,
    })
}

/// Raises the generation of every member of the artifact, current or revoked,
/// in the caller's transaction (decision 0060 rule 16).
pub(super) fn raise_for_trash(
    transaction: &Transaction<'_>,
    artifact: &ArtifactId,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "UPDATE artifact_members SET access_generation = access_generation + 1
             WHERE artifact_id = ?1",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    Ok(())
}

/// Deletes the artifact's membership records and blanks the fingerprints of
/// its access keys, in the purge's transaction, so no retry of an access
/// change can be answered afterwards (decisions 0029 and 0060 rule 16).
pub(super) fn erase_for_purge(
    transaction: &Transaction<'_>,
    artifact: &ArtifactId,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "DELETE FROM artifact_members WHERE artifact_id = ?1",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "UPDATE access_requests SET fingerprint = zeroblob(32) WHERE artifact_id = ?1",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    Ok(())
}

impl AccessStore for SqliteStore {
    fn access_list(&self, artifact: &ArtifactId) -> Result<Option<AccessList>, StoreError> {
        let mut connection = self.connect()?;
        // One snapshot: the owner, the version and the members agree.
        let snapshot = connection.transaction().map_err(unavailable)?;
        let head: Option<(String, i64)> = snapshot
            .query_row(
                "SELECT owner, access_version FROM artifacts
                 WHERE id = ?1 AND purged_at IS NULL",
                [artifact.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(unavailable)?;
        let Some((owner, version)) = head else {
            return Ok(None);
        };
        let rows: Vec<(String, String, bool)> = snapshot
            .prepare(
                "SELECT m.user_id, m.role, u.disabled_at IS NOT NULL
                 FROM artifact_members m JOIN users u ON u.id = m.user_id
                 WHERE m.artifact_id = ?1 AND m.role IS NOT NULL
                 ORDER BY m.user_id",
            )
            .map_err(unavailable)?
            .query_map([artifact.as_str()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?;
        let members = rows
            .into_iter()
            .map(|(user, role, disabled)| {
                Ok(Member {
                    user: user.parse().map_err(corrupt)?,
                    role: role.parse().map_err(corrupt)?,
                    disabled,
                })
            })
            .collect::<Result<_, StoreError>>()?;
        Ok(Some(AccessList {
            owner: owner.parse().map_err(corrupt)?,
            members,
            version: AccessVersion(count(version)?),
        }))
    }

    fn member_role(
        &self,
        artifact: &ArtifactId,
        user: &ActorId,
    ) -> Result<Option<Role>, StoreError> {
        let mut connection = self.connect()?;
        // One snapshot: the artifact's state and the record agree.
        let snapshot = connection.transaction().map_err(unavailable)?;
        let reachable: bool = snapshot
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM artifacts
                                WHERE id = ?1 AND purged_at IS NULL AND deleted_at IS NULL)",
                [artifact.as_str()],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if !reachable {
            return Ok(None);
        }
        Ok(member_record(&snapshot, artifact, user)?.and_then(|(role, _)| role))
    }

    fn access_generation(
        &self,
        artifact: &ArtifactId,
        user: &ActorId,
    ) -> Result<AccessGeneration, StoreError> {
        let connection = self.connect()?;
        Ok(member_record(&connection, artifact, user)?
            .map_or(AccessGeneration::NEVER, |(_, generation)| generation))
    }

    fn change_access(&self, request: AccessRequest<'_>) -> Result<AccessReceipt, AccessStoreError> {
        self.write(|transaction| change(transaction, &request))
    }
}
