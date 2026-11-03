//! Application data (decision 0031): read and change the JSON data space of
//! an artifact through [`ArtifactService`], like every other mutation.
//!
//! A space belongs to one actor and one artifact and has a version of its own,
//! unrelated to the artifact's source revision and metadata revision. Access
//! follows reads: an artifact that is missing, in the trash, purged, or another
//! actor's makes every data request `not found`, a retry included. Data is app
//! data, not source: it is in no revision, export, or package, and a source
//! restore does not touch it.

use artifree_domain::AppDataVersion;
use artifree_domain::data::{DataChange, DataPath, DataValue};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, UnixMillis};
use artifree_domain::members::{Members, MembersQuery};
use artifree_ports::artifact_store::{ArtifactStore, Delivery, RequestIdentity};
use artifree_ports::data_store::{DataReceipt, DataStore, DataStoreError, DataWrite};

use crate::artifacts::{ArtifactError, ArtifactService, Fingerprint, Missing};

/// What a read found at a path.
#[derive(Debug, Clone, PartialEq)]
pub struct DataRead {
    /// The version of the space when it was read.
    pub version: AppDataVersion,
    /// The value at the path, or `None` where nothing is.
    pub value: Option<DataValue>,
}

/// What a member query found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembersRead {
    /// The version of the space when it was read.
    pub version: AppDataVersion,
    /// The page.
    pub members: Members,
}

/// The result of a write, in terms the adapters report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataWriteOutcome {
    /// The version the request left the space at; for a replay, the version
    /// the original request left it at.
    pub version: AppDataVersion,
    /// Whether the change altered the space.
    pub changed: bool,
    /// Whether this call wrote it or replayed an earlier request.
    pub delivery: Delivery,
}

impl<S: ArtifactStore + DataStore> ArtifactService<S> {
    /// The value at `path` in the actor's data space of an artifact, and the
    /// space's version. A path with nothing at it reads as `None`, as does a
    /// path through a value that is not an object; the root always exists.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] for an artifact that is missing, in the
    /// trash, purged, or another actor's, or [`ArtifactError::Unavailable`].
    pub fn data(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        path: &DataPath,
    ) -> Result<DataRead, ArtifactError> {
        self.authorized(actor, id)?;
        let snapshot = self.store.read_data(id, actor).map_err(data_error)?;
        Ok(DataRead {
            version: snapshot.version,
            value: snapshot.space.get(path).cloned(),
        })
    }

    /// One page of the members of the collection at the query's path in the
    /// actor's data space of an artifact (decision 0040), and the version of
    /// the space the page was read at.
    ///
    /// # Errors
    ///
    /// As [`Self::data`].
    pub fn data_members(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        query: &MembersQuery,
    ) -> Result<MembersRead, ArtifactError> {
        self.authorized(actor, id)?;
        let snapshot = self.store.read_data(id, actor).map_err(data_error)?;
        Ok(MembersRead {
            version: snapshot.version,
            members: snapshot.space.members(query),
        })
    }

    /// Applies `change` to the actor's data space of an artifact, only if the
    /// space is still at `base`. A change that alters nothing leaves the
    /// version and writes only the key.
    ///
    /// Retrying with the same key and request returns the original outcome,
    /// the version it produced included, even after later writes. The same key
    /// for another request, or one of another kind, is
    /// [`ArtifactError::RequestKeyReused`].
    ///
    /// # Errors
    ///
    /// [`ArtifactError::NotFound`] as for [`Self::data`],
    /// [`ArtifactError::DataConflict`] when the space is not at `base`,
    /// [`ArtifactError::Data`] for a change the rules of a space refuse (a
    /// path through a value that is not an object, a root that would not be an
    /// object, a limit), [`ArtifactError::RequestKeyReused`], or
    /// [`ArtifactError::Unavailable`].
    pub fn write_data(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        base: AppDataVersion,
        change: &DataChange,
        now: UnixMillis,
    ) -> Result<DataWriteOutcome, ArtifactError> {
        self.authorized(actor, id)?;
        let mut hasher = match change {
            DataChange::Set { .. } => Fingerprint::new("set_data"),
            DataChange::Merge { .. } => Fingerprint::new("update_data"),
            DataChange::Remove { .. } => Fingerprint::new("delete_data"),
        };
        hasher.field(id.as_str().as_bytes());
        hasher.field(&base.0.to_be_bytes());
        match change {
            DataChange::Set { path, value } => {
                hasher.field(path.to_string().as_bytes());
                hasher.field(value.to_canonical().as_bytes());
            }
            DataChange::Merge { path, patch } => {
                hasher.field(path.to_string().as_bytes());
                hasher.field(patch.to_canonical().as_bytes());
            }
            DataChange::Remove { path } => hasher.field(path.to_string().as_bytes()),
        }
        let DataReceipt {
            version,
            changed,
            delivery,
        } = self
            .store
            .write_data(DataWrite {
                request: RequestIdentity {
                    actor,
                    key,
                    fingerprint: hasher.finish(),
                },
                artifact: id,
                base,
                change,
                now,
            })
            .map_err(data_error)?;
        Ok(DataWriteOutcome {
            version,
            changed,
            delivery,
        })
    }
}

fn data_error(error: DataStoreError) -> ArtifactError {
    match error {
        DataStoreError::NotFound => ArtifactError::NotFound(Missing::Artifact),
        DataStoreError::RequestKeyReused => ArtifactError::RequestKeyReused,
        DataStoreError::VersionConflict { current } => ArtifactError::DataConflict { current },
        DataStoreError::Rejected(error) => ArtifactError::Data(error),
        DataStoreError::Unavailable(reason) => ArtifactError::Unavailable(reason),
        other => ArtifactError::Unavailable(other.to_string()),
    }
}
