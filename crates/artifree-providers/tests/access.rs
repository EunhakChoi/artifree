//! Artifact access on a real database (decision 0060, slice S02a): grants,
//! role changes and revocations conditional on the access version, the owner's
//! key replay, the access generation across a revocation, the trash and a
//! purge, and the refusals in their order.

mod common;

use artifree_domain::access::{AccessGeneration, AccessVersion, Role};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, UnixMillis, UserLabel};
use artifree_ports::access_store::{
    AccessChange, AccessReceipt, AccessRequest, AccessStore, AccessStoreError, Member,
};
use artifree_ports::artifact_store::{
    CommitReceipt, Delivery, PurgeRequest, RequestFingerprint, RequestIdentity,
};
use artifree_ports::user_store::{NewUser, UserStore};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use common::{TempDir, TestResult, actor, create_demo, database_in, digest, key, parsed};

struct Fixture {
    _dir: TempDir,
    path: std::path::PathBuf,
    store: SqliteStore,
    identity: SqliteTokenStore,
    owner: ActorId,
    created: CommitReceipt,
    /// A second artifact of the same owner, for keys used across artifacts.
    other: ArtifactId,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let path = database_in(dir.path());
        let store = SqliteStore::open(&path)?;
        let identity = SqliteTokenStore::open(&path)?;
        let owner = actor("local");
        let created = create_demo(&store, &owner, &key("create"), "Shared", "<h1>one</h1>")?;
        let other = create_demo(
            &store,
            &owner,
            &key("create-other"),
            "Other",
            "<h1>two</h1>",
        )?
        .artifact
        .id;
        Ok(Self {
            _dir: dir,
            path,
            store,
            identity,
            owner,
            created,
            other,
        })
    }

    fn register(&self, label: &str, seed: u8) -> Result<ActorId, Box<dyn std::error::Error>> {
        let id = ActorId::user([seed; 16]);
        self.identity.register(&NewUser {
            id: id.clone(),
            label: parsed::<UserLabel>(label),
            created_at: "2026-11-16T00:00:00Z".to_owned(),
        })?;
        Ok(id)
    }

    /// A change by `by` with the key `name`, whose fingerprint covers the
    /// user and the change.
    fn change_as(
        &self,
        by: &ActorId,
        name: &str,
        user: &ActorId,
        change: AccessChange,
        base: u64,
    ) -> Result<AccessReceipt, AccessStoreError> {
        let request_key: RequestKey = key(&name.replace(' ', "-"));
        self.store.change_access(AccessRequest {
            request: RequestIdentity {
                actor: by,
                key: &request_key,
                fingerprint: RequestFingerprint(digest(&format!("{user}{change:?}"))),
            },
            artifact: &self.created.artifact.id,
            user,
            change,
            base: AccessVersion(base),
            now: UnixMillis(1),
        })
    }

    fn change(
        &self,
        name: &str,
        user: &ActorId,
        change: AccessChange,
        base: u64,
    ) -> Result<AccessReceipt, AccessStoreError> {
        self.change_as(&self.owner, name, user, change, base)
    }

    fn members(&self) -> Result<Vec<Member>, Box<dyn std::error::Error>> {
        Ok(self
            .store
            .access_list(&self.created.artifact.id)?
            .ok_or("the artifact has no access list")?
            .members)
    }

    fn generation(&self, user: &ActorId) -> Result<AccessGeneration, Box<dyn std::error::Error>> {
        Ok(self
            .store
            .access_generation(&self.created.artifact.id, user)?)
    }

    fn trash(&self, name: &str, base: u64, deleted: bool) -> TestResult {
        common::update_metadata(
            &self.store,
            &self.created,
            &key(name),
            base,
            None,
            None,
            Some(deleted),
        )?;
        Ok(())
    }
}

fn committed(version: u64) -> AccessReceipt {
    AccessReceipt {
        version: AccessVersion(version),
        delivery: Delivery::Committed,
    }
}

#[test]
fn a_grant_a_role_change_and_a_revocation_each_raise_the_version() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    let list = fixture
        .store
        .access_list(&fixture.created.artifact.id)?
        .ok_or("no access list")?;
    assert_eq!(list.owner, fixture.owner);
    assert_eq!(list.version, AccessVersion::INITIAL);
    assert!(list.members.is_empty());

    assert_eq!(
        fixture.change("grant", &ana, AccessChange::Grant(Role::Viewer), 0)?,
        committed(1)
    );
    assert_eq!(
        fixture.members()?,
        [Member {
            user: ana.clone(),
            label: "Ana".parse()?,
            role: Role::Viewer,
            disabled: false
        }]
    );
    assert_eq!(
        fixture.change("promote", &ana, AccessChange::Grant(Role::Editor), 1)?,
        committed(2)
    );
    assert_eq!(
        fixture
            .store
            .member_role(&fixture.created.artifact.id, &ana)?,
        Some(Role::Editor)
    );
    assert_eq!(
        fixture.change("revoke", &ana, AccessChange::Revoke, 2)?,
        committed(3)
    );
    assert!(fixture.members()?.is_empty());
    assert_eq!(
        fixture
            .store
            .member_role(&fixture.created.artifact.id, &ana)?,
        None
    );
    Ok(())
}

#[test]
fn granting_the_role_a_member_has_changes_nothing() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    fixture.change("grant", &ana, AccessChange::Grant(Role::Viewer), 0)?;
    let before = fixture.generation(&ana)?;
    assert_eq!(
        fixture.change("again", &ana, AccessChange::Grant(Role::Viewer), 1)?,
        committed(1)
    );
    assert_eq!(fixture.generation(&ana)?, before);
    Ok(())
}

#[test]
fn a_stale_base_is_a_conflict_and_writes_nothing() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    let ben = fixture.register("Ben", 2)?;
    fixture.change("grant ana", &ana, AccessChange::Grant(Role::Viewer), 0)?;
    assert_eq!(
        fixture.change("grant ben", &ben, AccessChange::Grant(Role::Editor), 0),
        Err(AccessStoreError::VersionConflict {
            current: AccessVersion(1)
        })
    );
    assert_eq!(fixture.members()?.len(), 1);
    // The refused request recorded no key: the same key at the current base
    // is a new request.
    assert_eq!(
        fixture.change("grant ben", &ben, AccessChange::Grant(Role::Editor), 1)?,
        committed(2)
    );
    Ok(())
}

#[test]
fn the_same_request_replays_and_a_reused_key_is_refused() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    let ben = fixture.register("Ben", 2)?;
    fixture.change("grant", &ana, AccessChange::Grant(Role::Viewer), 0)?;
    fixture.change("other", &ben, AccessChange::Grant(Role::Viewer), 1)?;
    assert_eq!(
        fixture.change("grant", &ana, AccessChange::Grant(Role::Viewer), 0)?,
        AccessReceipt {
            version: AccessVersion(1),
            delivery: Delivery::Replayed,
        }
    );
    assert_eq!(
        fixture.change("grant", &ana, AccessChange::Grant(Role::Editor), 2),
        Err(AccessStoreError::RequestKeyReused)
    );
    // The key of the artifact's creation is in the same space.
    assert_eq!(
        fixture.change("create", &ana, AccessChange::Grant(Role::Editor), 2),
        Err(AccessStoreError::RequestKeyReused)
    );
    Ok(())
}

#[test]
fn the_owner_an_unknown_user_and_a_new_disabled_user_are_refused_alike() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    let gone = ActorId::user([9; 16]);
    let owner = fixture.owner.clone();
    fixture
        .identity
        .set_disabled(&ana, Some("2026-11-16T00:00:01Z"))?;
    for (name, user) in [("owner", &owner), ("unknown", &gone), ("disabled", &ana)] {
        assert_eq!(
            fixture.change(name, user, AccessChange::Grant(Role::Viewer), 0),
            Err(AccessStoreError::NotGrantable),
            "{name}"
        );
    }
    assert_eq!(
        fixture.change("revoke", &ana, AccessChange::Revoke, 0),
        Err(AccessStoreError::NotMember)
    );
    assert!(fixture.members()?.is_empty());
    Ok(())
}

#[test]
fn a_disabled_member_stays_a_member_and_its_role_can_change() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    fixture.change("grant", &ana, AccessChange::Grant(Role::Viewer), 0)?;
    fixture
        .identity
        .set_disabled(&ana, Some("2026-11-16T00:00:01Z"))?;
    assert_eq!(
        fixture.members()?,
        [Member {
            user: ana.clone(),
            label: "Ana".parse()?,
            role: Role::Viewer,
            disabled: true
        }]
    );
    assert_eq!(
        fixture.change("promote", &ana, AccessChange::Grant(Role::Editor), 1)?,
        committed(2)
    );
    Ok(())
}

#[test]
fn only_the_owner_changes_access_and_not_in_the_trash() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    let ben = fixture.register("Ben", 2)?;
    fixture.change("grant", &ana, AccessChange::Grant(Role::Editor), 0)?;
    assert_eq!(
        fixture.change_as(&ana, "by ana", &ben, AccessChange::Grant(Role::Viewer), 1),
        Err(AccessStoreError::NotFound)
    );
    fixture.trash("trash", 1, true)?;
    assert_eq!(
        fixture.change("in trash", &ben, AccessChange::Grant(Role::Viewer), 1),
        Err(AccessStoreError::NotFound)
    );
    // A retry of an earlier change is not answered while it is in the trash.
    assert_eq!(
        fixture.change("grant", &ana, AccessChange::Grant(Role::Editor), 0),
        Err(AccessStoreError::NotFound)
    );
    Ok(())
}

#[test]
fn the_generation_only_grows_across_a_revocation_and_a_new_grant() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    assert_eq!(fixture.generation(&ana)?, AccessGeneration::NEVER);
    fixture.change("grant", &ana, AccessChange::Grant(Role::Viewer), 0)?;
    let granted = fixture.generation(&ana)?;
    fixture.change("revoke", &ana, AccessChange::Revoke, 1)?;
    let revoked = fixture.generation(&ana)?;
    fixture.change("again", &ana, AccessChange::Grant(Role::Viewer), 2)?;
    let regranted = fixture.generation(&ana)?;
    assert!(
        AccessGeneration::NEVER < granted && granted < revoked && revoked < regranted,
        "{granted} {revoked} {regranted}"
    );
    Ok(())
}

#[test]
fn the_trash_raises_every_members_generation_and_keeps_the_list() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    let ben = fixture.register("Ben", 2)?;
    fixture.change("grant ana", &ana, AccessChange::Grant(Role::Viewer), 0)?;
    fixture.change("grant ben", &ben, AccessChange::Grant(Role::Editor), 1)?;
    fixture.change("revoke ben", &ben, AccessChange::Revoke, 2)?;
    let before = [fixture.generation(&ana)?, fixture.generation(&ben)?];
    fixture.trash("trash", 1, true)?;
    let trashed = [fixture.generation(&ana)?, fixture.generation(&ben)?];
    fixture.trash("restore", 2, false)?;
    let restored = [fixture.generation(&ana)?, fixture.generation(&ben)?];
    for index in 0..2 {
        assert!(
            before.get(index) < trashed.get(index) && trashed.get(index) < restored.get(index),
            "{before:?} {trashed:?} {restored:?}"
        );
    }
    assert_eq!(fixture.members()?.len(), 1);
    Ok(())
}

#[test]
fn a_purge_removes_the_members_and_their_retries() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    fixture.change("grant", &ana, AccessChange::Grant(Role::Viewer), 0)?;
    fixture.trash("trash", 1, true)?;
    let purge_key = key("purge");
    artifree_ports::artifact_store::ArtifactStore::purge_artifact(
        &fixture.store,
        PurgeRequest {
            request: RequestIdentity {
                actor: &fixture.owner,
                key: &purge_key,
                fingerprint: RequestFingerprint(digest("purge")),
            },
            artifact: &fixture.created.artifact.id,
            base: artifree_domain::MetadataRevision(2),
        },
    )?;
    assert_eq!(
        fixture.store.access_list(&fixture.created.artifact.id)?,
        None
    );
    // The access keys' fingerprints are blanked, so nothing of the request
    // stays behind (decision 0029).
    let blanked: i64 = rusqlite::Connection::open(&fixture.path)?.query_row(
        "SELECT count(*) FROM access_requests
         WHERE artifact_id = ?1 AND fingerprint = zeroblob(32)",
        [fixture.created.artifact.id.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(blanked, 1);
    assert_eq!(fixture.generation(&ana)?, AccessGeneration::NEVER);
    assert_eq!(
        fixture.change("grant", &ana, AccessChange::Grant(Role::Viewer), 0),
        Err(AccessStoreError::NotFound)
    );
    Ok(())
}

#[test]
fn a_revoked_member_who_is_disabled_is_not_granted_again() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    fixture.change("grant", &ana, AccessChange::Grant(Role::Viewer), 0)?;
    fixture.change("revoke", &ana, AccessChange::Revoke, 1)?;
    fixture
        .identity
        .set_disabled(&ana, Some("2026-11-16T00:00:01Z"))?;
    assert_eq!(
        fixture.change("again", &ana, AccessChange::Grant(Role::Viewer), 2),
        Err(AccessStoreError::NotGrantable)
    );
    Ok(())
}

#[test]
fn a_key_used_on_another_artifact_is_reused_not_replayed() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    let other = fixture.other.clone();
    let request_key = key("shared-key");
    let request = |artifact| AccessRequest {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &request_key,
            fingerprint: RequestFingerprint(digest("the same grant")),
        },
        artifact,
        user: &ana,
        change: AccessChange::Grant(Role::Viewer),
        base: AccessVersion(0),
        now: UnixMillis(1),
    };
    fixture
        .store
        .change_access(request(&fixture.created.artifact.id))?;
    assert_eq!(
        fixture.store.change_access(request(&other)),
        Err(AccessStoreError::RequestKeyReused)
    );
    assert_eq!(fixture.store.member_role(&other, &ana)?, None);
    Ok(())
}

#[test]
fn no_member_has_a_role_while_the_artifact_is_in_the_trash() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    fixture.change("grant", &ana, AccessChange::Grant(Role::Editor), 0)?;
    fixture.trash("trash", 1, true)?;
    assert_eq!(
        fixture
            .store
            .member_role(&fixture.created.artifact.id, &ana)?,
        None
    );
    fixture.trash("restore", 2, false)?;
    assert_eq!(
        fixture
            .store
            .member_role(&fixture.created.artifact.id, &ana)?,
        Some(Role::Editor)
    );
    Ok(())
}
