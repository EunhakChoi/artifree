//! Private asset associations, atomic quota admission and metadata-only replay.
//!
//! An actor's own space is held by the actor. The shared space an artifact's
//! head revision declares (decision 0060 rule 10, slice S02f2) is held by the
//! reserved holder [`SHARED_HOLDER`], which no actor id can spell; each of its
//! associations is charged to the user who uploaded it (`charged_to`), whose
//! own limits it counts against as well as the shared space's.

use super::{SqliteStore, corrupt, key_used_elsewhere, record_shared_assets_change, unavailable};
use artifree_domain::SourceRevision;
use artifree_domain::access::{Need, Space};
use artifree_domain::assets::{
    AssetId, AssetMediaType, AssetMetadata, AssetRejection, AssetVersion, MAX_ACTOR_ASSETS,
    MAX_ACTOR_BYTES, MAX_ASSET_BYTES, MAX_SPACE_ASSETS, MAX_SPACE_BYTES,
};
use artifree_domain::identity::{ActorId, ArtifactId, UnixMillis};
use artifree_ports::artifact_store::{Delivery, StoreError};
use artifree_ports::asset_store::{
    AssetContent, AssetReceipt, AssetSnapshot, AssetStore, AssetStoreError, AssetWrite,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use sha2::{Digest, Sha256};

/// The holder of the shared asset space's rows. Actor ids are lowercase
/// letters, digits and hyphens, so no actor holds rows under this name.
const SHARED_HOLDER: &str = "*shared";

/// The holder of `space`'s rows for `actor`.
fn holder(actor: &ActorId, space: Space) -> &str {
    match space {
        Space::Own => actor.as_str(),
        Space::Shared => SHARED_HOLDER,
    }
}

/// Checks that `actor` reaches `space` of the artifact, and the optional
/// runtime source fence. The owner or a current member reaches its own space
/// (decision 0060 rule 9); the shared space needs a head that declares it, and
/// a write the role the declaration names (rule 10). No member reaches the
/// trash.
fn authority(
    connection: &Connection,
    actor: &ActorId,
    artifact: &ArtifactId,
    (space, writing): (Space, bool),
    source: Option<SourceRevision>,
) -> Result<(), AssetStoreError> {
    match space {
        Space::Own => {
            super::access::admit(connection, artifact, actor, Need::Read).map_err(|error| {
                match error {
                    StoreError::NotFound | StoreError::Forbidden => AssetStoreError::NotFound,
                    other => AssetStoreError::from(other),
                }
            })?;
        }
        Space::Shared => {
            super::data::reach(connection, artifact, actor, space, writing)?;
        }
    }
    let head: Option<i64> = connection
        .query_row(
            "SELECT current_revision FROM artifacts WHERE id = ?1 AND deleted_at IS NULL
             AND purged_at IS NULL",
            [artifact.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    let head = head.ok_or(AssetStoreError::NotFound)?;
    let current = SourceRevision(u64::try_from(head).map_err(corrupt)?);
    if source.is_some_and(|expected| expected != current) {
        return Err(AssetStoreError::SourceConflict { current });
    }
    Ok(())
}
fn version(
    connection: &Connection,
    holder: &str,
    artifact: &ArtifactId,
) -> Result<AssetVersion, AssetStoreError> {
    let value: Option<i64> = connection
        .query_row(
            "SELECT asset_version FROM asset_spaces WHERE artifact_id = ?1 AND actor = ?2",
            params![artifact.as_str(), holder],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    Ok(AssetVersion(
        u64::try_from(value.unwrap_or(0)).map_err(corrupt)?,
    ))
}
fn metadata(row: &Row<'_>) -> rusqlite::Result<(String, String, i64, Vec<u8>, i64)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}
fn parsed_metadata(
    stored: (String, String, i64, Vec<u8>, i64),
) -> Result<AssetMetadata, AssetStoreError> {
    let (id, content_type, size, digest, created_at) = stored;
    Ok(AssetMetadata {
        id: id.parse().map_err(corrupt)?,
        content_type: content_type.parse().map_err(corrupt)?,
        size: u64::try_from(size).map_err(corrupt)?,
        digest: digest
            .try_into()
            .map_err(|_| corrupt("asset digest length"))?,
        created_at: UnixMillis(created_at),
    })
}
fn replay(
    connection: &Connection,
    write: AssetWrite<'_>,
) -> Result<Option<AssetReceipt>, AssetStoreError> {
    let stored = connection
        .query_row(
            "SELECT fingerprint, asset_version, asset_id, content_type, size, digest, uploaded_at,
             deleted FROM asset_requests WHERE actor = ?1 AND request_key = ?2",
            params![write.request.actor.as_str(), write.request.key.as_str()],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, bool>(7)?,
                ))
            },
        )
        .optional()
        .map_err(unavailable)?;
    if let Some((fingerprint, version, id, content_type, size, digest, created_at, deleted)) =
        stored
    {
        // Purge clears the ID as well as the fingerprint. The structural marker
        // makes a scrubbed receipt unmatchable even for a zero fingerprint.
        let Some(id) =
            id.filter(|_| fingerprint.as_slice() == write.request.fingerprint.0.as_slice())
        else {
            return Err(AssetStoreError::RequestKeyReused);
        };
        let uploaded = match (content_type, size, digest, created_at) {
            (Some(media), Some(size), Some(digest), Some(time)) => {
                Some(parsed_metadata((id.clone(), media, size, digest, time))?)
            }
            (None, None, None, None) => None,
            _ => return Err(corrupt("incomplete upload receipt").into()),
        };
        return Ok(Some(AssetReceipt {
            version: AssetVersion(u64::try_from(version).map_err(corrupt)?),
            uploaded,
            id: id.parse().map_err(corrupt)?,
            deleted,
            delivery: Delivery::Replayed,
        }));
    }
    if key_used_elsewhere(
        connection,
        write.request.actor,
        write.request.key,
        "asset_requests",
    )? {
        return Err(AssetStoreError::RequestKeyReused);
    }
    Ok(None)
}
fn preflight(
    connection: &Connection,
    write: AssetWrite<'_>,
) -> Result<Option<AssetReceipt>, AssetStoreError> {
    authority(
        connection,
        write.request.actor,
        write.artifact,
        (write.space, true),
        write.expected_source,
    )?;
    if let Some(receipt) = replay(connection, write)? {
        return Ok(Some(receipt));
    }
    let current = version(
        connection,
        holder(write.request.actor, write.space),
        write.artifact,
    )?;
    if current != write.base {
        return Err(AssetStoreError::Conflict { current });
    }
    Ok(None)
}
/// The count and bytes of the associations a space holds.
fn space_usage(
    connection: &Connection,
    holder: &str,
    artifact: &ArtifactId,
) -> Result<(u64, u64), StoreError> {
    totals(connection.query_row(
        "SELECT count(*), coalesce(sum(size),0) FROM assets WHERE actor = ?1 AND artifact_id = ?2",
        params![holder, artifact.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ))
}
/// The count and bytes charged to `actor` across every artifact, trash
/// included: its own associations and the shared uploads charged to it. A
/// shared row is held by [`SHARED_HOLDER`], so no row is counted twice.
fn actor_usage(connection: &Connection, actor: &ActorId) -> Result<(u64, u64), StoreError> {
    totals(connection.query_row(
        "SELECT count(*), coalesce(sum(size),0) FROM assets
         WHERE actor = ?1 OR charged_to = ?1",
        [actor.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ))
}
fn totals(read: rusqlite::Result<(i64, i64)>) -> Result<(u64, u64), StoreError> {
    let (count, bytes) = read.map_err(unavailable)?;
    Ok((
        u64::try_from(count).map_err(corrupt)?,
        u64::try_from(bytes).map_err(corrupt)?,
    ))
}
/// Whether `add_count` more associations totalling `add_bytes` fit the
/// limits of the space `holder` holds in `artifact` and the actor's limits
/// across everything charged to it, trash included. Uploads and
/// asset-copying duplicates both admit through here, so they enforce
/// identical limits.
fn admits(
    connection: &Connection,
    actor: &ActorId,
    (holder, artifact): (&str, &ArtifactId),
    add_count: u64,
    add_bytes: u64,
) -> Result<bool, StoreError> {
    for (scope, max_count, max_bytes) in [
        (Some(artifact), MAX_SPACE_ASSETS, MAX_SPACE_BYTES),
        (None, MAX_ACTOR_ASSETS, MAX_ACTOR_BYTES),
    ] {
        let (count, bytes) = match scope {
            Some(artifact) => space_usage(connection, holder, artifact)?,
            None => actor_usage(connection, actor)?,
        };
        let fits = count
            .checked_add(add_count)
            .is_some_and(|next| next <= max_count)
            && bytes
                .checked_add(add_bytes)
                .is_some_and(|next| next <= max_bytes);
        if !fits {
            return Ok(false);
        }
    }
    Ok(true)
}
fn check_quota(
    connection: &Connection,
    write: AssetWrite<'_>,
    size: u64,
) -> Result<(), AssetStoreError> {
    let space = (holder(write.request.actor, write.space), write.artifact);
    if admits(connection, write.request.actor, space, 1, size)? {
        Ok(())
    } else {
        Err(AssetStoreError::Rejected(AssetRejection::LimitExceeded))
    }
}
fn advance(
    transaction: &Transaction<'_>,
    write: AssetWrite<'_>,
) -> Result<AssetVersion, AssetStoreError> {
    let next = write
        .base
        .0
        .checked_add(1)
        .filter(|next| *next <= 9_007_199_254_740_991)
        .ok_or_else(|| corrupt("asset version exhausted"))?;
    transaction.execute("INSERT INTO asset_spaces (artifact_id, actor, asset_version) VALUES (?1,?2,?3) ON CONFLICT
             (artifact_id,actor) DO UPDATE SET asset_version = excluded.asset_version", params![write.artifact.as_str(),holder(write.request.actor, write.space),i64::try_from(next).map_err(corrupt)?]).map_err(unavailable)?;
    Ok(AssetVersion(next))
}
fn record(
    transaction: &Transaction<'_>,
    write: AssetWrite<'_>,
    receipt: &AssetReceipt,
) -> Result<(), AssetStoreError> {
    let uploaded = receipt.uploaded.as_ref();
    transaction.execute("INSERT INTO asset_requests
             (actor,request_key,fingerprint,artifact_id,asset_version,asset_id,content_type,size,digest,uploaded_at,deleted,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![write.request.actor.as_str(),write.request.key.as_str(),write.request.fingerprint.0.as_slice(),write.artifact.as_str(),i64::try_from(receipt.version.0).map_err(corrupt)?,receipt.id.as_str(),uploaded.map(|m|m.content_type.as_str()),uploaded.map(|m|i64::try_from(m.size)).transpose().map_err(corrupt)?,uploaded.map(|m|m.digest.as_slice()),uploaded.map(|m|m.created_at.0),receipt.deleted,write.now.0]).map_err(unavailable)?;
    Ok(())
}
fn new_id(transaction: &Transaction<'_>) -> Result<AssetId, AssetStoreError> {
    for _attempt in 0..16 {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes)
            .map_err(|error| AssetStoreError::Unavailable(error.to_string()))?;
        let id = AssetId::from_random(bytes);
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO asset_ids(id) VALUES (?1)",
                [id.as_str()],
            )
            .map_err(unavailable)?;
        if inserted == 1 {
            return Ok(id);
        }
    }
    Err(AssetStoreError::Unavailable(
        "asset identifier collision budget exhausted".into(),
    ))
}
/// Copies the actor's entire asset space of `source` into the new artifact
/// `copy` (decision 0045 rule 10): the same IDs, types, sizes, digests and
/// creation times, as new associations of the copy alone, at asset version 1.
/// Nothing is written for an empty space, so the copy stays at version 0.
/// Blobs are shared, but every association is charged in full to the copy's
/// space and to the actor, trash included; a copy that does not fit is
/// refused before anything is written. Receipts are not copied.
pub(super) fn copy_space(
    transaction: &Transaction<'_>,
    source: &ArtifactId,
    copy: &ArtifactId,
    actor: &ActorId,
) -> Result<(), StoreError> {
    let (count, bytes) = space_usage(transaction, actor.as_str(), source)?;
    if count == 0 {
        return Ok(());
    }
    // The copy is new, so its space starts empty. The source space was
    // admitted under the same limits, so the space check here only guards
    // against legacy or corrupt data; the actor-wide check is the real limit.
    if !admits(transaction, actor, (actor.as_str(), copy), count, bytes)? {
        return Err(StoreError::AssetLimitExceeded);
    }
    transaction
        .execute(
            "INSERT INTO assets (artifact_id, actor, id, content_type, size, digest, created_at)
             SELECT ?2, actor, id, content_type, size, digest, created_at FROM assets
             WHERE artifact_id = ?1 AND actor = ?3",
            params![source.as_str(), copy.as_str(), actor.as_str()],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "INSERT INTO asset_spaces (artifact_id, actor, asset_version) VALUES (?1, ?2, 1)",
            params![copy.as_str(), actor.as_str()],
        )
        .map_err(unavailable)?;
    Ok(())
}
/// Deletes a blob only if no source history, asset association or build needs it.
fn remove_blob(transaction: &Transaction<'_>, digest: &[u8]) -> Result<(), StoreError> {
    transaction
        .execute(
            "DELETE FROM blobs WHERE digest = ?1 AND NOT EXISTS (SELECT 1 FROM revision_files WHERE
             digest = ?1) AND NOT EXISTS (SELECT 1 FROM assets WHERE digest = ?1)
             AND NOT EXISTS (SELECT 1 FROM build_files WHERE digest = ?1)",
            [digest],
        )
        .map_err(unavailable)?;
    Ok(())
}
/// Purge erases all asset content and receipt metadata while retaining keys and opaque ID reservations.
pub(super) fn erase_for_purge(
    transaction: &Transaction<'_>,
    artifact: &ArtifactId,
) -> Result<(), StoreError> {
    let digests: Vec<Vec<u8>> = transaction
        .prepare("SELECT DISTINCT digest FROM assets WHERE artifact_id = ?1")
        .map_err(unavailable)?
        .query_map([artifact.as_str()], |row| row.get(0))
        .map_err(unavailable)?
        .collect::<Result<_, _>>()
        .map_err(unavailable)?;
    transaction
        .execute(
            "DELETE FROM assets WHERE artifact_id = ?1",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "DELETE FROM asset_spaces WHERE artifact_id = ?1",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    transaction
        .execute(
            "UPDATE asset_requests SET fingerprint = zeroblob(32), asset_id = NULL, content_type =
             NULL, size = NULL, digest = NULL, uploaded_at = NULL WHERE artifact_id = ?1",
            [artifact.as_str()],
        )
        .map_err(unavailable)?;
    for digest in digests {
        remove_blob(transaction, &digest)?;
    }
    Ok(())
}
impl AssetStore for SqliteStore {
    fn assets(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
        space: Space,
    ) -> Result<AssetSnapshot, AssetStoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        authority(&snapshot, actor, artifact, (space, false), None)?;
        let holder = holder(actor, space);
        let stored = snapshot
            .prepare(
                "SELECT id, content_type, size, digest, created_at FROM assets WHERE actor = ?1 AND
             artifact_id = ?2 ORDER BY id",
            )
            .map_err(unavailable)?
            .query_map(params![holder, artifact.as_str()], metadata)
            .map_err(unavailable)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?;
        let assets = stored
            .into_iter()
            .map(parsed_metadata)
            .collect::<Result<Vec<_>, _>>()?;
        let (_, bytes) = space_usage(&snapshot, holder, artifact)?;
        Ok(AssetSnapshot {
            version: version(&snapshot, holder, artifact)?,
            assets,
            bytes,
        })
    }
    fn asset_content(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
        space: Space,
        id: &AssetId,
    ) -> Result<AssetContent, AssetStoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        authority(&snapshot, actor, artifact, (space, false), None)?;
        let holder = holder(actor, space);
        let stored=snapshot.query_row("SELECT a.id,a.content_type,a.size,a.digest,a.created_at,b.content FROM assets a JOIN blobs
             b ON b.digest=a.digest WHERE a.actor=?1 AND a.artifact_id=?2 AND a.id=?3",params![holder,artifact.as_str(),id.as_str()],|row|Ok((metadata(row)?,row.get::<_,Vec<u8>>(5)?))).optional().map_err(unavailable)?.ok_or(AssetStoreError::NotFound)?;
        Ok(AssetContent {
            version: version(&snapshot, holder, artifact)?,
            metadata: parsed_metadata(stored.0)?,
            bytes: stored.1,
        })
    }
    fn admit_asset_write(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
        space: Space,
    ) -> Result<(), AssetStoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        authority(&snapshot, actor, artifact, (space, true), None)
    }
    fn preflight_asset(
        &self,
        write: AssetWrite<'_>,
    ) -> Result<Option<AssetReceipt>, AssetStoreError> {
        let mut connection = self.connect()?;
        let snapshot = connection.transaction().map_err(unavailable)?;
        preflight(&snapshot, write)
    }
    fn upload_asset(
        &self,
        write: AssetWrite<'_>,
        media_type: &AssetMediaType,
        bytes: &[u8],
        validation: Result<(), AssetRejection>,
    ) -> Result<AssetReceipt, AssetStoreError> {
        self.write(|transaction| {
            if let Some(receipt) = preflight(transaction, write)? {
                return Ok(receipt);
            }
            validation.map_err(AssetStoreError::Rejected)?;
            // Keep physical bounds even when policy is injected by a trusted caller.
            if bytes.is_empty() {
                return Err(AssetStoreError::Rejected(AssetRejection::Invalid));
            }
            if bytes.len() > MAX_ASSET_BYTES {
                return Err(AssetStoreError::Rejected(AssetRejection::LimitExceeded));
            }
            let size = u64::try_from(bytes.len()).map_err(corrupt)?;
            check_quota(transaction, write, size)?;
            let id = new_id(transaction)?;
            let digest: [u8; 32] = Sha256::digest(bytes).into();
            transaction
                .execute(
                    "INSERT OR IGNORE INTO blobs(digest, content) VALUES (?1, ?2)",
                    params![digest.as_slice(), bytes],
                )
                .map_err(unavailable)?;
            // A shared upload is charged to the uploader (decision 0060 rule
            // 10); an own asset is the actor's and needs no charge.
            let charged_to = (write.space == Space::Shared).then(|| write.request.actor.as_str());
            transaction
                .execute(
                    "INSERT INTO assets
                    (artifact_id, actor, id, content_type, size, digest, created_at, charged_to)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        write.artifact.as_str(),
                        holder(write.request.actor, write.space),
                        id.as_str(),
                        media_type.as_str(),
                        i64::try_from(size).map_err(corrupt)?,
                        digest.as_slice(),
                        write.now.0,
                        charged_to
                    ],
                )
                .map_err(unavailable)?;
            let version = advance(transaction, write)?;
            if write.space == Space::Shared {
                record_shared_assets_change(transaction, write.artifact)?;
            }
            let receipt = AssetReceipt {
                version,
                id: id.clone(),
                uploaded: Some(AssetMetadata {
                    id,
                    content_type: media_type.clone(),
                    size,
                    digest,
                    created_at: write.now,
                }),
                deleted: false,
                delivery: Delivery::Committed,
            };
            record(transaction, write, &receipt)?;
            Ok(receipt)
        })
    }
    fn delete_asset(
        &self,
        write: AssetWrite<'_>,
        id: &AssetId,
    ) -> Result<AssetReceipt, AssetStoreError> {
        self.write_erasing(|transaction| {
            if let Some(receipt) = preflight(transaction, write)? {
                return Ok(receipt);
            }
            let digest: Option<Vec<u8>> = transaction
                .query_row(
                    "SELECT digest FROM assets WHERE actor=?1 AND artifact_id=?2 AND id=?3",
                    params![
                        holder(write.request.actor, write.space),
                        write.artifact.as_str(),
                        id.as_str()
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(unavailable)?;
            let deleted = digest.is_some();
            let version = if let Some(digest) = digest {
                transaction
                    .execute(
                        "DELETE FROM assets WHERE actor=?1 AND artifact_id=?2 AND id=?3",
                        params![
                            holder(write.request.actor, write.space),
                            write.artifact.as_str(),
                            id.as_str()
                        ],
                    )
                    .map_err(unavailable)?;
                remove_blob(transaction, &digest)?;
                if write.space == Space::Shared {
                    record_shared_assets_change(transaction, write.artifact)?;
                }
                advance(transaction, write)?
            } else {
                write.base
            };
            let receipt = AssetReceipt {
                version,
                id: id.clone(),
                uploaded: None,
                deleted,
                delivery: Delivery::Committed,
            };
            record(transaction, write, &receipt)?;
            Ok(receipt)
        })
    }
}
