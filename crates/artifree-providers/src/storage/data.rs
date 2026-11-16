//! Application data in the artifact database (decision 0031).
//!
//! A space is one row of `app_data` per artifact and actor: its canonical JSON
//! text and its version. A write is one `BEGIN IMMEDIATE` transaction that
//! checks the artifact, then, for a runtime frame, the source revision its view
//! was pinned to, then looks up the key, compares the base version, applies the
//! change to the space it just read, writes the row, and records the key, so
//! two connections cannot both pass the same base version. A space nobody wrote
//! has no row and is at version 0.
//!
//! A write that changed the space also appends a `data_changed` row to the feed
//! of the actor whose space it is, last in the same transaction (decision 0040).
//!
//! An artifact's declared shared space (decision 0060 rule 10) is one row of
//! `shared_data`. It is reached only while the head revision's runtime
//! manifest declares it, read in the transaction that reads or writes it; a
//! write needs the role the declaration names, and a change appends a
//! `shared_data_changed` row to the feed of the owner and every member. Its
//! keys are in `data_requests`, in the key space of the writer, and its
//! fingerprint names the space, so a key cannot replay across spaces.
//!
//! Neither table holds a content digest or a blob. A purge deletes the rows of
//! the artifact's spaces and blanks the fingerprints of its keys, both inside
//! the purge transaction and with `secure_delete` on.

use artifree_domain::AppDataVersion;
use artifree_domain::SourceRevision;
use artifree_domain::access::{Need, SharedWriters, Space};
use artifree_domain::data::DataSpace;
use artifree_domain::identity::{ActorId, ArtifactId, StateGeneration};
use artifree_ports::artifact_store::{Delivery, RequestIdentity, StoreError};
use artifree_ports::data_store::{
    DataReceipt, DataSnapshot, DataStore, DataStoreError, DataWrite, OutboxBinding,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::jobs::artifact_in_place;
use super::{
    SqliteStore, corrupt, key_used_elsewhere, record_data_change, record_shared_data_change,
    unavailable,
};

/// Path of the runtime manifest that declares the shared space (decision 0020
/// rule 1, decision 0060 rule 10).
const MANIFEST_PATH: &str = "artifree-runtime.json";

/// The shared space the artifact's head revision declares, read in the
/// caller's transaction (decision 0060 rule 10). A head with no manifest, or
/// with one whose declaration cannot be read, declares none.
fn head_declaration(
    connection: &Connection,
    artifact: &ArtifactId,
) -> Result<Option<SharedWriters>, StoreError> {
    let manifest: Option<Vec<u8>> = connection
        .query_row(
            "SELECT b.content FROM artifacts a
             JOIN revision_files f ON f.artifact_id = a.id AND f.revision = a.current_revision
             JOIN blobs b ON b.digest = f.digest
             WHERE a.id = ?1 AND f.path = ?2",
            params![artifact.as_str(), MANIFEST_PATH],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    Ok(manifest.and_then(|bytes| SharedWriters::declared_in(&bytes).ok().flatten()))
}

/// Checks, in the caller's transaction, that `space` is reachable: the actor's
/// own always is, and the shared one while the head declares it. A write also
/// needs the role the declaration names ([`StoreError::Forbidden`]).
fn reach(
    connection: &Connection,
    artifact: &ArtifactId,
    actor: &ActorId,
    space: Space,
    writing: bool,
) -> Result<(), StoreError> {
    if space == Space::Own {
        return Ok(());
    }
    let Some(writers) = head_declaration(connection, artifact)? else {
        return Err(StoreError::NotFound);
    };
    if writing {
        super::access::admit(connection, artifact, actor, writers.need())?;
    }
    Ok(())
}

/// The space of `actor`, or the artifact's shared space, as stored, or the
/// empty space at version 0.
fn read_space(
    connection: &Connection,
    artifact: &ArtifactId,
    actor: &ActorId,
    space: Space,
) -> Result<DataSnapshot, StoreError> {
    let stored: Option<(i64, String)> = match space {
        Space::Own => connection.query_row(
            "SELECT data_version, document FROM app_data WHERE artifact_id = ?1 AND actor = ?2",
            params![artifact.as_str(), actor.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ),
        Space::Shared => connection.query_row(
            "SELECT data_version, document FROM shared_data WHERE artifact_id = ?1",
            [artifact.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ),
    }
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

/// The feed and enabled user state belong to the caller's existing transaction.
fn binding(connection: &Connection, actor: &ActorId) -> Result<OutboxBinding, DataStoreError> {
    let generation: Option<i64> = connection
        .query_row(
            "SELECT state_generation FROM users WHERE id = ?1 AND disabled_at IS NULL",
            [actor.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    let generation = generation.ok_or(DataStoreError::OutboxBindingChanged)?;
    let generation =
        crate::users::stored_generation(generation).map_err(DataStoreError::Unavailable)?;
    let epoch = super::read_feed_id(connection)?;
    Ok(OutboxBinding {
        actor: actor.clone(),
        generation,
        epoch,
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

/// The artifact's current source revision, read in the caller's transaction.
fn source_head(
    connection: &Connection,
    artifact: &ArtifactId,
) -> Result<SourceRevision, StoreError> {
    let head: i64 = connection
        .query_row(
            "SELECT current_revision FROM artifacts WHERE id = ?1",
            [artifact.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    Ok(SourceRevision(u64::try_from(head).map_err(corrupt)?))
}

/// Stores the space a write left at `version`: the actor's own row, or the
/// artifact's shared row.
fn store_space(
    transaction: &Transaction<'_>,
    request: &DataWrite<'_>,
    version: AppDataVersion,
    space: &DataSpace,
) -> Result<(), StoreError> {
    let stored = i64::try_from(version.0).map_err(corrupt)?;
    match request.space {
        Space::Own => transaction.execute(
            "INSERT INTO app_data (artifact_id, actor, data_version, document)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (artifact_id, actor) DO UPDATE
                 SET data_version = excluded.data_version, document = excluded.document",
            params![
                request.artifact.as_str(),
                request.request.actor.as_str(),
                stored,
                space.to_canonical()
            ],
        ),
        Space::Shared => transaction.execute(
            "INSERT INTO shared_data (artifact_id, data_version, document)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (artifact_id) DO UPDATE
                 SET data_version = excluded.data_version, document = excluded.document",
            params![request.artifact.as_str(), stored, space.to_canonical()],
        ),
    }
    .map_err(unavailable)?;
    Ok(())
}

fn write(
    transaction: &Transaction<'_>,
    request: DataWrite<'_>,
    expected_source: Option<SourceRevision>,
) -> Result<DataReceipt, DataStoreError> {
    // The target first, as every write does (decision 0013): data of an
    // artifact in the trash or purged is not found, a retry included.
    if !artifact_in_place(transaction, request.artifact)? {
        return Err(DataStoreError::NotFound);
    }
    // A member writes its own space while it is a member, read again here
    // (decision 0060 rules 5 and 9); a read is all a role needs for that.
    super::access::admit(
        transaction,
        request.artifact,
        request.request.actor,
        Need::Read,
    )?;
    // The shared space is written only while the head declares it, and only
    // by a role the declaration names, read here before the key (decision
    // 0060 rules 10 and 17).
    reach(
        transaction,
        request.artifact,
        request.request.actor,
        request.space,
        true,
    )?;
    // A runtime frame's write is fenced to its view's head before the key is
    // looked at, so a retry after the head moved is refused, not replayed
    // (decision 0053, rule 10).
    if let Some(expected) = expected_source {
        let current = source_head(transaction, request.artifact)?;
        if current != expected {
            return Err(DataStoreError::SourceConflict { current });
        }
    }
    if let Some(fence) = request.fence {
        let current = binding(transaction, request.request.actor)?;
        if current != fence.expected || current.generation != fence.authenticated_generation {
            return Err(DataStoreError::OutboxBindingChanged);
        }
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
    let current = read_space(
        transaction,
        request.artifact,
        request.request.actor,
        request.space,
    )?;
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
        let version = AppDataVersion(next);
        store_space(transaction, &request, version, &applied.space)?;
        version
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
    // Last: after the data row and the key, in the same transaction, so a
    // failure here undoes both (decision 0040). The actor is the one whose
    // space changed, named here and not looked up from the artifact; the
    // shared space reaches every reader of the artifact (decision 0060 rule
    // 13).
    if applied.changed {
        match request.space {
            Space::Own => record_data_change(
                transaction,
                request.request.actor,
                request.artifact,
                version,
            )?,
            Space::Shared => record_shared_data_change(transaction, request.artifact, version)?,
        }
    }
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
    for table in [
        "DELETE FROM app_data WHERE artifact_id = ?1",
        "DELETE FROM shared_data WHERE artifact_id = ?1",
    ] {
        transaction
            .execute(table, [artifact.as_str()])
            .map_err(unavailable)?;
    }
    transaction
        .execute(
            "UPDATE data_requests SET fingerprint = zeroblob(32) WHERE artifact_id = ?1",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    Ok(())
}

impl DataStore for SqliteStore {
    fn outbox_binding(
        &self,
        actor: &ActorId,
        generation: StateGeneration,
    ) -> Result<OutboxBinding, DataStoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        let current = binding(&snapshot, actor)?;
        if current.generation != generation {
            return Err(DataStoreError::OutboxBindingChanged);
        }
        Ok(current)
    }

    fn read_data(
        &self,
        artifact: &ArtifactId,
        actor: &ActorId,
        space: Space,
    ) -> Result<DataSnapshot, DataStoreError> {
        let mut connection = self.connect()?;
        // One snapshot: the artifact check, the head's declaration and the
        // space agree.
        let snapshot = connection.transaction().map_err(unavailable)?;
        if !artifact_in_place(&snapshot, artifact)? {
            return Err(DataStoreError::NotFound);
        }
        reach(&snapshot, artifact, actor, space, false)?;
        Ok(read_space(&snapshot, artifact, actor, space)?)
    }

    fn write_data(&self, request: DataWrite<'_>) -> Result<DataReceipt, DataStoreError> {
        // Erasing: a value that a write replaces or removes is overwritten in
        // the file's free pages as it goes, not left there until they are
        // reused (decision 0031).
        self.write_erasing(|transaction| write(transaction, request, None))
    }

    fn write_runtime_data(
        &self,
        request: DataWrite<'_>,
        expected_source: SourceRevision,
    ) -> Result<DataReceipt, DataStoreError> {
        self.write_erasing(|transaction| write(transaction, request, Some(expected_source)))
    }
}
