//! Artifact access on a real database (decision 0060, slice S02a): grants,
//! role changes and revocations conditional on the access version, the owner's
//! key replay, the access generation across a revocation, the trash and a
//! purge, and the refusals in their order; listings with the caller's role
//! (slice S02e3).

mod common;

use artifree_domain::access::{Access, AccessGeneration, AccessVersion, Role};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, UnixMillis, UserLabel};
use artifree_ports::access_store::{
    AccessChange, AccessReceipt, AccessRequest, AccessStore, AccessStoreError, Member,
};
use artifree_ports::artifact_store::{
    ArtifactListing, ArtifactStore, CommitReceipt, Delivery, ListAccess, ListOrder, ListPosition,
    PurgeRequest, RequestFingerprint, RequestIdentity,
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

/// The list names the owner's label, and an owner that no registry holds,
/// which decision 0036 lets a database name, under its id rather than as
/// damage. It says whether the artifact is in the trash, in its snapshot.
#[test]
fn the_list_names_the_owner_and_whether_the_artifact_is_in_the_trash() -> TestResult {
    let fixture = Fixture::new()?;
    let list = |id: &ArtifactId| -> Result<_, Box<dyn std::error::Error>> {
        let list = fixture.store.access_list(id)?.ok_or("no access list")?;
        Ok((list.owner_label.to_string(), list.in_trash))
    };
    assert_eq!(
        list(&fixture.created.artifact.id)?,
        ("Local owner".to_owned(), false)
    );
    fixture.trash("trash", 1, true)?;
    assert_eq!(
        list(&fixture.created.artifact.id)?,
        ("Local owner".to_owned(), true)
    );
    let ghost = actor("ghost");
    let unregistered = create_demo(&fixture.store, &ghost, &key("ghost"), "Ghost", "<p>g</p>")?;
    assert_eq!(
        list(&unregistered.artifact.id)?,
        ("ghost".to_owned(), false)
    );
    Ok(())
}

/// A listed artifact's title, the caller's access and the owner's label.
type Listed = (String, Access, String);

/// The titles, accesses and owner labels `who` lists, by update time.
fn listed(
    store: &SqliteStore,
    who: &ActorId,
    access: Option<ListAccess>,
    deleted: bool,
) -> Result<Vec<Listed>, Box<dyn std::error::Error>> {
    Ok(store
        .list_artifacts(ArtifactListing {
            owner: who,
            access,
            search: None,
            pinned: None,
            deleted,
            order: ListOrder::Updated,
            after: None,
            limit: 50,
        })?
        .into_iter()
        .map(|listed| {
            (
                listed.record.title.as_str().to_owned(),
                listed.access,
                listed.owner_label.as_str().to_owned(),
            )
        })
        .collect())
}

fn titles(listed: &[Listed]) -> Vec<&str> {
    let mut titles: Vec<&str> = listed.iter().map(|(title, ..)| title.as_str()).collect();
    titles.sort_unstable();
    titles
}

/// Decision 0060 rule 7: a member lists what they own and what they are a
/// member of, each with their access and the owner's label; the filter
/// selects one or the other; the trash and a revoked membership list nothing
/// shared.
#[test]
fn a_member_lists_what_they_own_and_what_is_shared_with_them() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    let ben = fixture.register("Ben", 2)?;
    create_demo(&fixture.store, &ana, &key("ana-own"), "Mine", "<h1>a</h1>")?;
    fixture.change("grant ana", &ana, AccessChange::Grant(Role::Viewer), 0)?;
    fixture.change("grant ben", &ben, AccessChange::Grant(Role::Editor), 1)?;

    let all = listed(&fixture.store, &ana, None, false)?;
    assert_eq!(titles(&all), ["Mine", "Shared"]);
    for (title, access, label) in &all {
        match title.as_str() {
            "Shared" => assert_eq!(
                (access, label.as_str()),
                (&Access::Member(Role::Viewer), "Local owner")
            ),
            _ => assert_eq!((access, label.as_str()), (&Access::Owner, "Ana")),
        }
    }
    let shared = listed(&fixture.store, &ben, Some(ListAccess::Shared), false)?;
    assert_eq!(
        shared,
        [(
            "Shared".to_owned(),
            Access::Member(Role::Editor),
            "Local owner".to_owned()
        )]
    );
    assert!(listed(&fixture.store, &ben, Some(ListAccess::Owned), false)?.is_empty());
    assert_eq!(
        titles(&listed(
            &fixture.store,
            &ana,
            Some(ListAccess::Owned),
            false
        )?),
        ["Mine"]
    );
    // The owner's list holds the owner's artifacts only, as before.
    assert_eq!(
        titles(&listed(&fixture.store, &fixture.owner, None, false)?),
        ["Other", "Shared"]
    );
    assert!(
        listed(
            &fixture.store,
            &fixture.owner,
            Some(ListAccess::Shared),
            false
        )?
        .is_empty()
    );

    // In the trash the artifact is the owner's only.
    fixture.trash("trash", 1, true)?;
    assert_eq!(
        titles(&listed(&fixture.store, &ana, None, false)?),
        ["Mine"]
    );
    assert!(listed(&fixture.store, &ana, None, true)?.is_empty());
    assert!(listed(&fixture.store, &ana, Some(ListAccess::Shared), true)?.is_empty());
    assert_eq!(
        titles(&listed(&fixture.store, &fixture.owner, None, true)?),
        ["Shared"]
    );
    fixture.trash("restore", 2, false)?;
    assert_eq!(
        titles(&listed(&fixture.store, &ana, None, false)?),
        ["Mine", "Shared"]
    );

    fixture.change("revoke ana", &ana, AccessChange::Revoke, 2)?;
    assert_eq!(
        titles(&listed(&fixture.store, &ana, None, false)?),
        ["Mine"]
    );
    assert_eq!(
        titles(&listed(&fixture.store, &ben, None, false)?),
        ["Shared"]
    );
    Ok(())
}

/// A grant on `owner`'s artifact `artifact` to `user`, by the owner.
fn grant_on(
    store: &SqliteStore,
    owner: &ActorId,
    artifact: &ArtifactId,
    user: &ActorId,
    name: &str,
) -> Result<AccessReceipt, AccessStoreError> {
    let request_key = key(name);
    store.change_access(AccessRequest {
        request: RequestIdentity {
            actor: owner,
            key: &request_key,
            fingerprint: RequestFingerprint(digest(name)),
        },
        artifact,
        user,
        change: AccessChange::Grant(Role::Editor),
        base: AccessVersion(0),
        now: UnixMillis(1),
    })
}

/// A registered owner is listed under their label, and an owner no registry
/// holds (0036 lets a database name one) under their id.
#[test]
fn the_owner_is_listed_by_label_or_by_id() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    let ben = fixture.register("Ben", 2)?;
    let outsider = actor("outsider");
    let bens = create_demo(&fixture.store, &ben, &key("ben-own"), "Ben's", "<h1>b</h1>")?;
    let theirs = create_demo(
        &fixture.store,
        &outsider,
        &key("out-own"),
        "Theirs",
        "<h1>c</h1>",
    )?;
    grant_on(
        &fixture.store,
        &ben,
        &bens.artifact.id,
        &ana,
        "ben-grants-ana",
    )?;
    grant_on(
        &fixture.store,
        &outsider,
        &theirs.artifact.id,
        &ana,
        "out-grants-ana",
    )?;
    let mut all = listed(&fixture.store, &ana, None, false)?;
    all.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        all,
        [
            (
                "Ben's".to_owned(),
                Access::Member(Role::Editor),
                "Ben".to_owned()
            ),
            (
                "Theirs".to_owned(),
                Access::Member(Role::Editor),
                "outsider".to_owned()
            ),
        ]
    );
    Ok(())
}

/// Pages continue across owned and shared artifacts alike: each appears once.
#[test]
fn pages_continue_across_owned_and_shared_artifacts() -> TestResult {
    let fixture = Fixture::new()?;
    let ana = fixture.register("Ana", 1)?;
    for name in ["one", "two", "three"] {
        create_demo(&fixture.store, &ana, &key(name), name, "<h1>a</h1>")?;
    }
    fixture.change("grant ana", &ana, AccessChange::Grant(Role::Viewer), 0)?;
    let mut paged = Vec::new();
    let mut after: Option<ListPosition> = None;
    loop {
        let page = fixture.store.list_artifacts(ArtifactListing {
            owner: &ana,
            access: None,
            search: None,
            pinned: None,
            deleted: false,
            order: ListOrder::Created,
            after: after.as_ref(),
            limit: 1,
        })?;
        let Some(last) = page.last() else { break };
        paged.push(last.record.title.as_str().to_owned());
        after = Some(ListPosition {
            at: last.record.created_at,
            id: last.record.id.clone(),
        });
    }
    paged.sort_unstable();
    assert_eq!(paged, ["Shared", "one", "three", "two"]);
    Ok(())
}
