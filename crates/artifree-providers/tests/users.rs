//! The registry of users, the owner of each token, the feed and the request
//! keys of each actor, on a real database (decision 0036).

mod common;

use artifree_domain::change::ChangeSequence;
use artifree_domain::identity::{ActorId, TokenId, UserLabel};
use artifree_ports::artifact_store::{
    ArtifactStore, CommitReceipt, Delivery, RequestFingerprint, RequestIdentity, RevisionCommit,
    StoreError,
};
use artifree_ports::token_store::{NewTokenRecord, TokenStore, TokenStoreError};
use artifree_ports::user_store::{MAX_USERS, NewUser, UserStore, UserStoreError};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::{SqliteTokenStore, hash_token_secret};
use common::{TempDir, TestResult, actor, create_demo, database_in, key, package, parsed};
use rusqlite::Connection;

struct Fixture {
    _dir: TempDir,
    path: std::path::PathBuf,
    store: SqliteStore,
    identity: SqliteTokenStore,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new();
        let path = database_in(dir.path());
        let store = SqliteStore::open(&path)?;
        let identity = SqliteTokenStore::open(&path)?;
        Ok(Self {
            _dir: dir,
            path,
            store,
            identity,
        })
    }

    fn register(&self, label: &str, seed: u8) -> Result<ActorId, UserStoreError> {
        let id = ActorId::user([seed; 16]);
        self.identity.register(&NewUser {
            id: id.clone(),
            label: parsed::<UserLabel>(label),
            created_at: "2026-11-03T00:00:00Z".to_owned(),
        })?;
        Ok(id)
    }

    fn token(&self, user: &ActorId, seed: u8) -> Result<TokenId, TokenStoreError> {
        let id = TokenId::from_random([seed; 16]);
        self.identity.create(&NewTokenRecord {
            id: id.clone(),
            actor: user.clone(),
            name: format!("token {seed}"),
            secret_hash: hash_token_secret(&format!("secret {seed}")),
            prefix: "art_".to_owned(),
            created_at: "2026-11-03T00:00:00Z".to_owned(),
        })?;
        Ok(id)
    }
}

#[test]
fn a_fresh_database_holds_the_enabled_owner_profile() -> TestResult {
    let fixture = Fixture::new()?;
    let users = fixture.identity.users()?;
    assert_eq!(users.len(), 1);
    assert!(users[0].id.is_owner());
    assert!(users[0].is_enabled());
    assert_eq!(
        fixture.identity.user(&ActorId::owner())?,
        Some(users[0].clone())
    );
    assert_eq!(fixture.identity.user(&actor("unknown"))?, None);
    Ok(())
}

#[test]
fn registering_lists_the_owner_first_and_refuses_what_the_registry_cannot_hold() -> TestResult {
    let fixture = Fixture::new()?;
    let alice = fixture.register("Alice", 1)?;
    let bob = fixture.register("Bob", 2)?;
    let listed: Vec<_> = fixture
        .identity
        .users()?
        .into_iter()
        .map(|u| u.id)
        .collect();
    assert_eq!(listed, [ActorId::owner(), alice.clone(), bob]);

    let user = |id: ActorId, label: &str| NewUser {
        id,
        label: parsed(label),
        created_at: "2026-11-03T00:00:00Z".to_owned(),
    };
    // The same actor, the same label (case sensitive), the owner's actor, and
    // spellings no generator makes are all conflicts.
    for (rejected, case) in [
        (user(alice, "Another"), "same actor"),
        (user(ActorId::user([3; 16]), "Alice"), "same label"),
        (user(ActorId::owner(), "Second owner"), "the owner's actor"),
        (user(actor("local-2"), "Custom"), "a chosen spelling"),
        (user(actor("owner"), "Custom 2"), "a test spelling"),
    ] {
        assert!(
            matches!(
                fixture.identity.register(&rejected),
                Err(UserStoreError::Conflict(_))
            ),
            "{case} was registered"
        );
    }
    assert_eq!(fixture.identity.users()?.len(), 3);
    // A spelling no generator makes is refused by the store's own check, before
    // the database is asked (the schema's check is a second layer).
    for spelling in ["local-2", "owner", "u-gggggggggggggggggggggggggggggggg"] {
        let refused = fixture
            .identity
            .register(&user(actor(spelling), "Custom 3"));
        let Err(UserStoreError::Conflict(message)) = refused else {
            return Err(format!("{spelling:?} was not refused by the store: {refused:?}").into());
        };
        assert!(
            message.contains("generated user id"),
            "{spelling:?}: {message}"
        );
    }
    // The same label in another case is another label.
    fixture.register("alice", 4)?;
    Ok(())
}

#[test]
fn the_registry_is_bounded() -> TestResult {
    let fixture = Fixture::new()?;
    for seed in 1..u8::try_from(MAX_USERS)? {
        fixture.register(&format!("User {seed}"), seed)?;
    }
    assert_eq!(fixture.identity.users()?.len(), MAX_USERS);
    assert_eq!(
        fixture.register("One too many", 200).err(),
        Some(UserStoreError::LimitReached)
    );
    assert_eq!(fixture.identity.users()?.len(), MAX_USERS);
    Ok(())
}

#[test]
fn disabling_keeps_the_first_time_and_enabling_clears_it() -> TestResult {
    let fixture = Fixture::new()?;
    let alice = fixture.register("Alice", 1)?;
    let first = fixture
        .identity
        .set_disabled(&alice, Some("2026-11-03T01:00:00Z"))?;
    assert_eq!(first.disabled_at.as_deref(), Some("2026-11-03T01:00:00Z"));
    let again = fixture
        .identity
        .set_disabled(&alice, Some("2026-11-03T02:00:00Z"))?;
    assert_eq!(again.disabled_at.as_deref(), Some("2026-11-03T01:00:00Z"));
    assert!(!again.is_enabled());
    let enabled = fixture.identity.set_disabled(&alice, None)?;
    assert!(enabled.is_enabled());
    assert!(fixture.identity.set_disabled(&alice, None)?.is_enabled());
    assert_eq!(
        fixture
            .identity
            .set_disabled(&ActorId::user([9; 16]), None)
            .err(),
        Some(UserStoreError::NotFound(ActorId::user([9; 16]).to_string()))
    );
    Ok(())
}

#[test]
fn the_owner_profile_cannot_be_disabled_by_the_store_or_by_the_schema() -> TestResult {
    let fixture = Fixture::new()?;
    assert!(matches!(
        fixture
            .identity
            .set_disabled(&ActorId::owner(), Some("2026-11-03T01:00:00Z")),
        Err(UserStoreError::Conflict(_))
    ));
    assert!(
        fixture
            .identity
            .user(&ActorId::owner())?
            .is_some_and(|u| u.is_enabled())
    );
    // Not even a statement that bypasses the store: the schema refuses it.
    let direct = Connection::open(&fixture.path)?
        .execute("UPDATE users SET disabled_at = 'x' WHERE id = 'local'", []);
    assert!(direct.is_err(), "the owner row was disabled");
    // And it refuses an actor spelling the registry cannot hold.
    for id in ["owner", "local-2", "u-abc", "root"] {
        let direct = Connection::open(&fixture.path)?.execute(
            "INSERT INTO users (id, label, created_at) VALUES (?1, ?1, 'now')",
            [id],
        );
        assert!(direct.is_err(), "the schema registered {id}");
    }
    Ok(())
}

#[test]
fn a_token_belongs_to_one_user_and_stops_with_it() -> TestResult {
    let fixture = Fixture::new()?;
    let alice = fixture.register("Alice", 1)?;
    let bob = fixture.register("Bob", 2)?;
    let a = fixture.token(&alice, 1)?;
    let b = fixture.token(&bob, 2)?;
    let owned = fixture.token(&ActorId::owner(), 3)?;

    let found = |seed: u8| {
        fixture
            .identity
            .find_by_hash(&hash_token_secret(&format!("secret {seed}")))
    };
    assert_eq!(
        found(1)?.map(|t| (t.id, t.actor)),
        Some((a.clone(), alice.clone()))
    );
    assert_eq!(found(2)?.map(|t| (t.id, t.actor)), Some((b, bob.clone())));
    assert_eq!(
        found(3)?.map(|t| (t.id, t.actor)),
        Some((owned, ActorId::owner()))
    );

    // A rotation keeps the user.
    let rotated = fixture
        .identity
        .rotate(&a, &hash_token_secret("rotated"), "art_r")?;
    assert_eq!(rotated.actor, alice);
    assert_eq!(
        fixture
            .identity
            .find_by_hash(&hash_token_secret("rotated"))?
            .map(|t| t.actor),
        Some(alice.clone())
    );

    // Disabling Bob makes his tokens inactive and nobody else's.
    fixture
        .identity
        .set_disabled(&bob, Some("2026-11-03T01:00:00Z"))?;
    assert!(found(2)?.is_some_and(|t| t.actor_disabled && !t.is_active()));
    assert!(found(3)?.is_some_and(|t| t.is_active()));
    assert!(
        fixture
            .identity
            .find_by_hash(&hash_token_secret("rotated"))?
            .is_some_and(|t| t.is_active())
    );
    // Enabling brings them back.
    fixture.identity.set_disabled(&bob, None)?;
    assert!(found(2)?.is_some_and(|t| t.is_active()));
    // The listing names each token's user.
    let listed: Vec<_> = fixture
        .identity
        .list()?
        .into_iter()
        .map(|t| t.actor)
        .collect();
    assert_eq!(listed.iter().filter(|a| **a == alice).count(), 1);
    assert_eq!(listed.iter().filter(|a| **a == bob).count(), 1);
    assert_eq!(listed.iter().filter(|a| a.is_owner()).count(), 1);
    Ok(())
}

#[test]
fn a_token_is_issued_only_to_a_registered_enabled_user() -> TestResult {
    let fixture = Fixture::new()?;
    let ghost = ActorId::user([7; 16]);
    assert!(matches!(
        fixture.token(&ghost, 1),
        Err(TokenStoreError::NotFound(_))
    ));
    // A chosen spelling is no user either, even one other tables use.
    assert!(matches!(
        fixture.token(&actor("owner"), 2),
        Err(TokenStoreError::NotFound(_))
    ));
    let alice = fixture.register("Alice", 1)?;
    fixture
        .identity
        .set_disabled(&alice, Some("2026-11-03T01:00:00Z"))?;
    assert!(matches!(
        fixture.token(&alice, 3),
        Err(TokenStoreError::Conflict(_))
    ));
    assert!(fixture.identity.list()?.is_empty(), "nothing was issued");
    fixture.identity.set_disabled(&alice, None)?;
    fixture.token(&alice, 3)?;
    Ok(())
}

#[test]
fn each_actor_has_a_feed_of_its_own() -> TestResult {
    let fixture = Fixture::new()?;
    let (alice, bob) = (actor("alice"), actor("bob"));
    let a1 = create_demo(&fixture.store, &alice, &key("a1"), "A1", "<h1>a1</h1>")?;
    create_demo(&fixture.store, &bob, &key("b1"), "B1", "<h1>b1</h1>")?;
    create_demo(&fixture.store, &bob, &key("b2"), "B2", "<h1>b2</h1>")?;
    common::revise(&fixture.store, &a1, &key("a2"), 1, "<h1>a2</h1>")?;

    let numbers = |who: &ActorId, after: u64| -> Result<(Vec<u64>, u64), StoreError> {
        let page = fixture.store.changes(who, ChangeSequence(after), 100)?;
        Ok((
            page.changes.iter().map(|c| c.sequence.0).collect(),
            page.head.0,
        ))
    };
    assert_eq!(numbers(&alice, 0)?, (vec![1, 2], 2));
    assert_eq!(numbers(&bob, 0)?, (vec![1, 2], 2));
    assert_eq!(numbers(&alice, 1)?, (vec![2], 2));
    // An actor with no artifact has an empty feed at the start: it is not
    // told that others committed.
    assert_eq!(numbers(&actor("carol"), 0)?, (vec![], 0));
    // Every change is the owner's own artifact.
    for change in fixture.store.changes(&bob, ChangeSequence(0), 100)?.changes {
        assert_ne!(change.artifact, a1.artifact.id);
    }
    Ok(())
}

fn save(
    store: &SqliteStore,
    owner: &ActorId,
    request_key: &str,
    artifact: &CommitReceipt,
    body: &str,
) -> Result<CommitReceipt, StoreError> {
    store.commit_revision(RevisionCommit {
        request: RequestIdentity {
            actor: owner,
            key: &key(request_key),
            fingerprint: RequestFingerprint(common::digest(body)),
        },
        artifact: &artifact.artifact.id,
        base: artifact.artifact.current_revision,
        package: &package(&[("index.html", body)]),
    })
}

#[test]
fn the_same_request_key_text_is_a_separate_request_for_each_actor() -> TestResult {
    let fixture = Fixture::new()?;
    let (alice, bob) = (actor("alice"), actor("bob"));
    // Create: the same key and the same content are two artifacts, and the
    // same key with other content is no conflict for the other actor.
    let a = create_demo(
        &fixture.store,
        &alice,
        &key("shared"),
        "Same",
        "<h1>same</h1>",
    )?;
    let b = create_demo(
        &fixture.store,
        &bob,
        &key("shared"),
        "Same",
        "<h1>same</h1>",
    )?;
    assert_eq!(a.delivery, Delivery::Committed);
    assert_eq!(
        b.delivery,
        Delivery::Committed,
        "Bob's request is not a replay of Alice's"
    );
    assert_ne!(a.artifact.id, b.artifact.id);
    let c = create_demo(
        &fixture.store,
        &actor("carol"),
        &key("shared"),
        "Other",
        "<h1>c</h1>",
    )?;
    assert_eq!(
        c.delivery,
        Delivery::Committed,
        "other content is no key reuse across actors"
    );
    // Each actor still replays its own, and is refused a changed request.
    let replay = create_demo(
        &fixture.store,
        &alice,
        &key("shared"),
        "Same",
        "<h1>same</h1>",
    )?;
    assert_eq!(
        (replay.delivery, replay.artifact.id),
        (Delivery::Replayed, a.artifact.id.clone())
    );
    assert_eq!(
        create_demo(&fixture.store, &bob, &key("shared"), "Other", "<h1>x</h1>").err(),
        Some(StoreError::RequestKeyReused)
    );
    // Save: one key text, two artifacts, two commits.
    let a2 = save(&fixture.store, &alice, "save-one", &a, "<h1>a2</h1>")?;
    let b2 = save(&fixture.store, &bob, "save-one", &b, "<h1>a2</h1>")?;
    assert_eq!(a2.delivery, Delivery::Committed);
    assert_eq!(b2.delivery, Delivery::Committed);
    assert_eq!(a2.revision.revision.0, 2);
    assert_eq!(b2.revision.revision.0, 2);
    assert_ne!(a2.artifact.id, b2.artifact.id);
    Ok(())
}
