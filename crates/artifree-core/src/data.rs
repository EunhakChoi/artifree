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
use artifree_domain::data::{DataChange, DataError, DataPath, DataValue, MemberPage, Members};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, StateGeneration, UnixMillis};
use artifree_ports::artifact_store::{ArtifactStore, Delivery, RequestIdentity};
use artifree_ports::data_store::{
    DataReceipt, DataStore, DataStoreError, DataWrite, OutboxBinding, OutboxFence,
};

use crate::artifacts::{ArtifactError, ArtifactService, Fingerprint, Missing};
use crate::members::{DataMembersPage, MemberQuery, MemberToken};

/// What a read found at a path.
#[derive(Debug, Clone, PartialEq)]
pub struct DataRead {
    /// The version of the space when it was read.
    pub version: AppDataVersion,
    /// The value at the path, or `None` where nothing is.
    pub value: Option<DataValue>,
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

/// A conditional change with optional browser authority.
#[derive(Debug, Clone, Copy)]
pub struct DataMutation<'a> {
    /// Data version observed by the caller.
    pub base: AppDataVersion,
    /// Validated application-data change.
    pub change: &'a DataChange,
    /// Server time for the durable receipt.
    pub now: UnixMillis,
    /// Optional authority checked before replay in the write transaction.
    pub fence: Option<&'a OutboxFence>,
}

impl<S: ArtifactStore + DataStore> ArtifactService<S> {
    /// Establishes a browser outbox binding from one storage snapshot.
    ///
    /// # Errors
    ///
    /// `OutboxBindingChanged` for stale, missing or disabled users, or storage failure.
    pub fn outbox_binding(
        &self,
        actor: &ActorId,
        generation: StateGeneration,
    ) -> Result<OutboxBinding, ArtifactError> {
        self.store
            .outbox_binding(actor, generation)
            .map_err(data_error)
    }

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

    /// One page of the member names of the object at a path of the actor's
    /// data space (decision 0040, rules 9 to 11), from a single read of the
    /// space, so that the names and the version belong together.
    ///
    /// The checks run in this order, each before any later one reads
    /// anything: the page token (it must be the one issued for this artifact
    /// and path, which needs no storage), access, then `at_data_version`, then
    /// the value at the path. A path with nothing at it, or through a value
    /// that is not an object, lists nothing and `exists` is false.
    ///
    /// # Errors
    ///
    /// [`ArtifactError::InvalidPageToken`] for a token issued for another
    /// artifact or path, [`ArtifactError::NotFound`] as for [`Self::data`],
    /// [`ArtifactError::DataConflict`] when the space is not at
    /// `at_data_version`, [`ArtifactError::Data`] with
    /// [`DataError::NotObject`] when a value that is not an object is at
    /// the path, or [`ArtifactError::Unavailable`].
    pub fn data_members(
        &self,
        actor: &ActorId,
        id: &ArtifactId,
        query: &MemberQuery,
    ) -> Result<DataMembersPage, ArtifactError> {
        let foreign = query
            .after
            .as_ref()
            .is_some_and(|token| !token.matches(id, &query.path));
        if foreign {
            return Err(ArtifactError::InvalidPageToken);
        }
        self.authorized(actor, id)?;
        let snapshot = self.store.read_data(id, actor).map_err(data_error)?;
        if query
            .at_data_version
            .is_some_and(|expected| expected != snapshot.version)
        {
            return Err(ArtifactError::DataConflict {
                current: snapshot.version,
            });
        }
        let after = query.after.as_ref().map(MemberToken::name);
        let page = match snapshot
            .space
            .members(&query.path, after, query.limit.get())
        {
            Members::Missing => {
                return Ok(DataMembersPage {
                    version: snapshot.version,
                    exists: false,
                    names: Vec::new(),
                    next: None,
                });
            }
            Members::NotObject => return Err(ArtifactError::Data(DataError::NotObject)),
            Members::Page(page) => page,
        };
        let MemberPage { names, more } = page;
        let next = names
            .last()
            .filter(|_| more)
            .map(|last| MemberToken::new(id, &query.path, last.clone()));
        Ok(DataMembersPage {
            version: snapshot.version,
            exists: true,
            names,
            next,
        })
    }

    /// Applies `change` to the actor's data space of an artifact, only if the
    /// space is still at `base`. A change that alters nothing leaves the
    /// version and writes only the key.
    ///
    /// A change that altered the space is also recorded in the actor's change
    /// feed by the store (decision 0040).
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
        self.write_data_with_fence(
            actor,
            key,
            id,
            DataMutation {
                base,
                change,
                now,
                fence: None,
            },
        )
    }

    /// Applies a conditional keyed change with an optional transactional outbox fence.
    ///
    /// # Errors
    ///
    /// The errors of `write_data`, plus `OutboxBindingChanged` before replay.
    pub fn write_data_with_fence(
        &self,
        actor: &ActorId,
        key: &RequestKey,
        id: &ArtifactId,
        mutation: DataMutation<'_>,
    ) -> Result<DataWriteOutcome, ArtifactError> {
        let DataMutation {
            base,
            change,
            now,
            fence,
        } = mutation;
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
                fence,
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
        DataStoreError::OutboxBindingChanged => ArtifactError::OutboxBindingChanged,
        DataStoreError::NotFound => ArtifactError::NotFound(Missing::Artifact),
        DataStoreError::RequestKeyReused => ArtifactError::RequestKeyReused,
        DataStoreError::VersionConflict { current } => ArtifactError::DataConflict { current },
        DataStoreError::Rejected(error) => ArtifactError::Data(error),
        DataStoreError::Unavailable(reason) => ArtifactError::Unavailable(reason),
        other => ArtifactError::Unavailable(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use artifree_domain::change::ChangeSequence;
    use artifree_domain::data::{DataSpace, MemberName};
    use artifree_domain::package::{FilePackage, PackagePath};
    use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};
    use artifree_ports::artifact_store::{
        ArtifactListing, ArtifactRecord, ChangePage, CleanupReceipt, CleanupRequest, CommitReceipt,
        ConditionalCommit, MetadataReceipt, MetadataUpdate, NewArtifact, PurgeReceipt,
        PurgeRequest, RevisionCommit, RevisionRecord, StoreError, UnreferencedContent,
    };
    use artifree_ports::data_store::DataSnapshot;

    use super::*;
    use crate::members::MemberLimit;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A store that counts what the listing asks of it and answers every
    /// `read_data` with a newer space than the one before: read `n` is at
    /// version `n` and holds the one member `m{n}` and the scalar `s`.
    /// `known` says whether the artifact is in place. Writes are not used.
    struct NewerEachRead {
        record: ArtifactRecord,
        known: bool,
        lookups: Cell<u32>,
        reads: Cell<u64>,
    }

    impl NewerEachRead {
        fn new(owner: &ActorId, id: &ArtifactId, known: bool) -> Self {
            Self {
                record: ArtifactRecord {
                    id: id.clone(),
                    owner: owner.clone(),
                    kind: ArtifactKind::App,
                    title: "Demo".parse().unwrap_or_else(|error| panic!("{error}")),
                    pinned: false,
                    current_revision: SourceRevision(1),
                    metadata_revision: MetadataRevision(1),
                    deleted_at: None,
                    created_at: artifree_domain::identity::UnixMillis(1),
                    updated_at: artifree_domain::identity::UnixMillis(1),
                },
                known,
                lookups: Cell::new(0),
                reads: Cell::new(0),
            }
        }
    }

    impl DataStore for NewerEachRead {
        fn outbox_binding(
            &self,
            _: &ActorId,
            _: StateGeneration,
        ) -> Result<OutboxBinding, DataStoreError> {
            Err(DataStoreError::Unavailable("unused".into()))
        }

        fn read_data(&self, _: &ArtifactId, _: &ActorId) -> Result<DataSnapshot, DataStoreError> {
            let read = self.reads.get() + 1;
            self.reads.set(read);
            let text = format!(r#"{{"m{read}":1,"s":"text"}}"#);
            Ok(DataSnapshot {
                version: AppDataVersion(read),
                space: DataSpace::from_canonical(&text)
                    .unwrap_or_else(|error| panic!("{text}: {error}")),
            })
        }

        fn write_data(&self, _: DataWrite<'_>) -> Result<DataReceipt, DataStoreError> {
            Err(DataStoreError::Unavailable("unused".into()))
        }
    }

    impl ArtifactStore for NewerEachRead {
        fn create_artifact(&self, _: NewArtifact<'_>) -> Result<CommitReceipt, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn commit_revision(&self, _: RevisionCommit<'_>) -> Result<CommitReceipt, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn commit_edits(&self, _: ConditionalCommit<'_>) -> Result<CommitReceipt, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn update_metadata(&self, _: MetadataUpdate<'_>) -> Result<MetadataReceipt, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn replay_create(
            &self,
            _: RequestIdentity<'_>,
        ) -> Result<Option<CommitReceipt>, StoreError> {
            Ok(None)
        }
        fn purge_artifact(&self, _: PurgeRequest<'_>) -> Result<PurgeReceipt, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn unreferenced_content(&self) -> Result<UnreferencedContent, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn remove_unreferenced_content(
            &self,
            _: CleanupRequest<'_>,
        ) -> Result<CleanupReceipt, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
        fn artifact(&self, _: &ArtifactId) -> Result<Option<ArtifactRecord>, StoreError> {
            self.lookups.set(self.lookups.get() + 1);
            Ok(self.known.then(|| self.record.clone()))
        }
        fn list_artifacts(
            &self,
            _: ArtifactListing<'_>,
        ) -> Result<Vec<ArtifactRecord>, StoreError> {
            Ok(Vec::new())
        }
        fn revisions(&self, _: &ArtifactId) -> Result<Vec<RevisionRecord>, StoreError> {
            Ok(Vec::new())
        }
        fn revision(
            &self,
            _: &ArtifactId,
            _: SourceRevision,
        ) -> Result<Option<RevisionRecord>, StoreError> {
            Ok(None)
        }
        fn revision_package(
            &self,
            _: &ArtifactId,
            _: SourceRevision,
        ) -> Result<Option<FilePackage>, StoreError> {
            Ok(None)
        }
        fn revision_file(
            &self,
            _: &ArtifactId,
            _: SourceRevision,
            _: &PackagePath,
        ) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(None)
        }
        fn changes(
            &self,
            _: &ActorId,
            _: ChangeSequence,
            _: usize,
        ) -> Result<ChangePage, StoreError> {
            Err(StoreError::Unavailable("unused".into()))
        }
    }

    struct Fixture {
        owner: ActorId,
        id: ArtifactId,
        service: ArtifactService<NewerEachRead>,
    }

    fn fixture(known: bool) -> Fixture {
        let owner: ActorId = "owner".parse().unwrap_or_else(|error| panic!("{error}"));
        let id = ArtifactId::from_random([3; 16]);
        let service = ArtifactService::new(NewerEachRead::new(&owner, &id, known));
        Fixture { owner, id, service }
    }

    fn query(path: &str) -> MemberQuery {
        MemberQuery {
            path: path
                .parse()
                .unwrap_or_else(|error| panic!("{path}: {error}")),
            after: None,
            limit: MemberLimit::default(),
            at_data_version: None,
        }
    }

    fn token_for(id: &ArtifactId, path: &str) -> MemberToken {
        let last: MemberName = "a".parse().unwrap_or_else(|error| panic!("{error}"));
        MemberToken::new(
            id,
            &path.parse().unwrap_or_else(|error| panic!("{error}")),
            last,
        )
    }

    #[test]
    fn a_page_takes_its_names_and_version_from_one_read() -> TestResult {
        let Fixture { owner, id, service } = fixture(true);
        let page = service.data_members(&owner, &id, &query(""))?;
        assert_eq!(page.version, AppDataVersion(1));
        let names: Vec<&str> = page.names.iter().map(MemberName::as_str).collect();
        assert_eq!(
            names,
            ["m1", "s"],
            "the names of the read that has the version"
        );
        assert_eq!(service.store.reads.get(), 1, "one read of the space");
        Ok(())
    }

    #[test]
    fn a_token_for_another_artifact_or_path_is_refused_before_any_storage_is_read() {
        let Fixture { owner, id, service } = fixture(true);
        let other = ArtifactId::from_random([4; 16]);
        for (case, token, path) in [
            ("other artifact", token_for(&other, "a"), "a"),
            ("other path", token_for(&id, "b"), "a"),
            ("root token on a path", token_for(&id, ""), "a"),
            ("path token on the root", token_for(&id, "a"), ""),
        ] {
            let request = MemberQuery {
                after: Some(token),
                ..query(path)
            };
            assert_eq!(
                service.data_members(&owner, &id, &request),
                Err(ArtifactError::InvalidPageToken),
                "{case}"
            );
        }
        assert_eq!(service.store.lookups.get(), 0, "no artifact lookup");
        assert_eq!(service.store.reads.get(), 0, "no read of the space");
    }

    #[test]
    fn a_token_is_checked_before_access_and_access_before_the_version() {
        let missing = fixture(false);
        let other = ArtifactId::from_random([4; 16]);
        let stale = MemberQuery {
            at_data_version: Some(AppDataVersion(9)),
            ..query("")
        };
        // Access: a missing artifact is not found, not a conflict.
        assert_eq!(
            missing
                .service
                .data_members(&missing.owner, &missing.id, &stale),
            Err(ArtifactError::NotFound(Missing::Artifact))
        );
        assert_eq!(missing.service.store.reads.get(), 0, "no read after 404");
        // Token: a bad token on a missing artifact is the token's error.
        let foreign = MemberQuery {
            after: Some(token_for(&other, "")),
            ..stale
        };
        assert_eq!(
            missing
                .service
                .data_members(&missing.owner, &missing.id, &foreign),
            Err(ArtifactError::InvalidPageToken)
        );
        assert_eq!(missing.service.store.lookups.get(), 1, "only the 404 above");
    }

    #[test]
    fn the_version_is_checked_before_the_value_at_the_path() -> TestResult {
        let Fixture { owner, id, service } = fixture(true);
        // `s` is a scalar: a conflict of the path, but only at the right version.
        let stale = MemberQuery {
            at_data_version: Some(AppDataVersion(7)),
            ..query("s")
        };
        assert_eq!(
            service.data_members(&owner, &id, &stale),
            Err(ArtifactError::DataConflict {
                current: AppDataVersion(1)
            }),
            "a stale version wins over a path conflict"
        );
        // The second read is at version 2, which `at_data_version` now names.
        let current = MemberQuery {
            at_data_version: Some(AppDataVersion(2)),
            ..query("s")
        };
        assert_eq!(
            service.data_members(&owner, &id, &current),
            Err(ArtifactError::Data(DataError::NotObject))
        );
        let missing = service.data_members(&owner, &id, &query("nothing/here"))?;
        assert!(!missing.exists, "{missing:?}");
        assert!(missing.names.is_empty() && missing.next.is_none());
        Ok(())
    }

    #[test]
    fn a_next_token_continues_after_the_last_name_of_the_page() -> TestResult {
        let Fixture { owner, id, service } = fixture(true);
        let one = MemberQuery {
            limit: MemberLimit::try_from(1)?,
            ..query("")
        };
        let first = service.data_members(&owner, &id, &one)?;
        let next = first.next.ok_or("a token: `s` follows `m1`")?;
        assert_eq!(next.name().as_str(), "m1");
        assert!(next.matches(&id, &"".parse()?));
        Ok(())
    }
}
