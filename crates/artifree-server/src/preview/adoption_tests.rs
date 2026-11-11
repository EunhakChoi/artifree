//! Adoption of a live runtime view (decision 0054, rules 4 and 5).
//!
//! A refused adoption changes nothing. An accepted one advances the same
//! record: the same secret, and a new generation, initialization, lease and
//! head. A committed write stays recorded until an adoption uses it.

use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId, TokenId};

use super::{
    Adoption, IssuedRuntimeTicket, PreviewTickets, RuntimeAuthority, RuntimeIssueError,
    RuntimeTicketRequest, ViewKey,
};
use crate::runtime_policy::Lifecycle;
use crate::runtime_sessions::{GrantScope, RuntimeSession, RuntimeSessions};

type TestResult = Result<(), Box<dyn Error>>;

/// The view id of every test.
const VIEW: &str = "0123456789abcdef0123456789abcdef";
/// The lifetime of every ticket and connection in these tests.
const TTL: Duration = Duration::from_secs(60);
/// The lease, in milliseconds, that a newly issued ticket reports.
const LEASE_MS: u64 = 60_000;

/// One preview registry with one artifact. Connections are made per actor.
struct Registry {
    tickets: PreviewTickets,
    sessions: RuntimeSessions,
    artifact: ArtifactId,
}

impl Registry {
    fn new() -> Self {
        Self {
            tickets: PreviewTickets::new(TTL),
            sessions: RuntimeSessions::new(TTL),
            artifact: ArtifactId::from_random([1; 16]),
        }
    }

    /// Opens a new connection of `actor`, as the HTTP layer does.
    fn connect(&self, actor: &ActorId) -> Result<Arc<RuntimeSession>, Box<dyn Error>> {
        let id = self
            .sessions
            .create(actor, None)
            .map_err(|error| format!("{error:?}"))?;
        self.sessions
            .get(actor, &id)
            .map_err(|error| format!("{error:?}").into())
    }

    /// The request of `actor` for `generation` of the view, which pins
    /// `revision` and reports `head`. It makes no adoption.
    fn view(
        &self,
        actor: &ActorId,
        generation: u64,
        revision: u64,
        head: u64,
        lifecycle: Lifecycle,
    ) -> RuntimeTicketRequest {
        RuntimeTicketRequest {
            actor: actor.clone(),
            token_id: None,
            artifact: self.artifact.clone(),
            revision: SourceRevision(revision),
            head_revision: SourceRevision(head),
            view_id: VIEW.to_owned(),
            generation,
            lifecycle,
            adopt: None,
        }
    }

    /// Issues a view that the registry must accept.
    fn open(&self, request: RuntimeTicketRequest) -> Result<IssuedRuntimeTicket, Box<dyn Error>> {
        self.tickets
            .issue_runtime(request)
            .map_err(|error| format!("{error:?}").into())
    }

    /// The refusal of `request`, or `None` when the registry accepts it.
    fn refusal(&self, request: RuntimeTicketRequest) -> Option<RuntimeIssueError> {
        self.tickets.issue_runtime(request).err()
    }

    /// Attaches `session` as the authority of the view that `secret` names.
    fn bind(
        &self,
        secret: &str,
        session: &Arc<RuntimeSession>,
        revision: u64,
        head: u64,
    ) -> TestResult {
        let scope = GrantScope::new(
            self.artifact.clone(),
            SourceRevision(revision),
            SourceRevision(head),
        );
        let authority = RuntimeAuthority {
            session: Arc::clone(session),
            scope,
            version: 0,
        };
        if self.tickets.bind_authority(secret, authority) {
            Ok(())
        } else {
            Err("the view must accept its authority".into())
        }
    }

    /// Serves the view's one document and bootstrap, as its first load does.
    fn serve(&self, secret: &str) -> TestResult {
        self.tickets
            .take_document(secret, true)
            .ok_or("the view's document must be served once")?;
        Ok(())
    }

    /// The head that `runtime_head` reports for a frame that presents this
    /// binding.
    fn head(
        &self,
        actor: &ActorId,
        revision: u64,
        generation: u64,
        initialization: &str,
    ) -> Option<SourceRevision> {
        self.tickets.runtime_head(
            actor,
            &self.artifact,
            SourceRevision(revision),
            VIEW,
            generation,
            initialization,
        )
    }

    /// The connection that holds the authority of this binding, if any.
    fn holder(
        &self,
        actor: &ActorId,
        revision: u64,
        generation: u64,
        initialization: &str,
    ) -> Option<Arc<RuntimeSession>> {
        let (_, authority) = self.tickets.runtime_binding(
            actor,
            &self.artifact,
            SourceRevision(revision),
            VIEW,
            generation,
            initialization,
        )?;
        authority.map(|held| held.session)
    }
}

/// `request`, adopting `previous`, the generation that `session` holds.
fn adopting(
    request: RuntimeTicketRequest,
    previous: u64,
    session: &Arc<RuntimeSession>,
) -> RuntimeTicketRequest {
    RuntimeTicketRequest {
        adopt: Some(Adoption {
            generation: previous,
            session: Arc::clone(session),
        }),
        ..request
    }
}

/// The key that a frame presents to record or check `generation` of the view.
fn key<'a>(
    actor: &'a ActorId,
    artifact: &'a ArtifactId,
    generation: u64,
    revision: u64,
    initialization: &'a str,
) -> ViewKey<'a> {
    ViewKey {
        actor,
        artifact,
        revision: SourceRevision(revision),
        view_id: VIEW,
        generation,
        initialization,
    }
}

/// Decision 0054, rule 5: an accepted adoption advances the view that it names.
/// The secret and the document revision stay, while the generation,
/// initialization, lease and head advance. The old generation stops binding.
#[test]
fn adoption_advances_the_same_view_in_place() -> TestResult {
    let registry = Registry::new();
    let local: ActorId = "local".parse()?;
    let session = registry.connect(&local)?;
    let first = registry.open(registry.view(&local, 1, 1, 1, Lifecycle::Adopt))?;
    registry.bind(&first.preview.secret, &session, 1, 1)?;
    registry.serve(&first.preview.secret)?;

    let before = Instant::now();
    let second = registry.open(adopting(
        registry.view(&local, 2, 1, 3, Lifecycle::Adopt),
        1,
        &session,
    ))?;
    let after = Instant::now();

    assert_eq!(
        second.preview.secret, first.preview.secret,
        "an adoption keeps the ticket secret"
    );
    assert_ne!(
        second.initialization, first.initialization,
        "an adoption issues a new initialization"
    );
    assert_eq!(
        second.lease_ms, LEASE_MS,
        "an adoption reports a full lease"
    );
    let ticket = registry
        .tickets
        .resolve(&second.preview.secret)
        .ok_or("the adopted ticket must still resolve")?;
    let expires = ticket.expires;
    let earliest = before.checked_add(TTL).ok_or("the lease bound overflows")?;
    let latest = after.checked_add(TTL).ok_or("the lease bound overflows")?;
    assert!(
        (earliest..=latest).contains(&expires),
        "the adopted lease starts at the adoption: expected {earliest:?}..={latest:?}, got {expires:?}"
    );
    assert_eq!(
        registry.head(&local, 1, 1, &first.initialization),
        None,
        "the old generation's binding is gone"
    );
    registry.bind(&second.preview.secret, &session, 1, 3)?;
    assert_eq!(
        registry.head(&local, 1, 2, &second.initialization),
        Some(SourceRevision(3)),
        "the new generation binds at the adopted head"
    );
    assert!(
        registry
            .tickets
            .take_document(&second.preview.secret, true)
            .is_none(),
        "the document is never served twice"
    );
    assert_eq!(
        ticket.revision,
        SourceRevision(1),
        "the document revision stays the one that resolve serves"
    );
    Ok(())
}

/// Decision 0054, rule 4: only the view's own actor can adopt it, even when
/// another actor presents the view's own connection.
#[test]
fn a_foreign_actor_cannot_adopt() -> TestResult {
    let local: ActorId = "local".parse()?;
    let other: ActorId = "other".parse()?;
    for (case, presents_victim_connection) in [
        ("with its own connection", false),
        ("with the view's connection", true),
    ] {
        let registry = Registry::new();
        let victim = registry.connect(&local)?;
        let first = registry.open(registry.view(&local, 1, 1, 1, Lifecycle::Adopt))?;
        registry.bind(&first.preview.secret, &victim, 1, 1)?;
        registry.serve(&first.preview.secret)?;
        let presented = if presents_victim_connection {
            Arc::clone(&victim)
        } else {
            registry.connect(&other)?
        };
        let refusal = registry.refusal(adopting(
            registry.view(&other, 2, 1, 3, Lifecycle::Adopt),
            1,
            &presented,
        ));
        assert_eq!(
            refusal,
            Some(RuntimeIssueError::NotAdoptable),
            "another actor must not adopt the view {case}"
        );
        assert_eq!(
            registry.head(&local, 1, 1, &first.initialization),
            Some(SourceRevision(1)),
            "the view of its own actor must be unchanged {case}"
        );
    }
    Ok(())
}

/// Decision 0054, rule 4: a refused adoption leaves the old binding in place
/// and raises no fence, so the same generation can still adopt afterwards.
#[test]
fn a_refused_adoption_changes_nothing() -> TestResult {
    let registry = Registry::new();
    let local: ActorId = "local".parse()?;
    let bound = registry.connect(&local)?;
    let unbound = registry.connect(&local)?;
    let first = registry.open(registry.view(&local, 1, 1, 1, Lifecycle::Adopt))?;
    registry.bind(&first.preview.secret, &bound, 1, 1)?;
    registry.serve(&first.preview.secret)?;

    let refusal = registry.refusal(adopting(
        registry.view(&local, 2, 1, 3, Lifecycle::Adopt),
        1,
        &unbound,
    ));
    assert_eq!(
        refusal,
        Some(RuntimeIssueError::NotAdoptable),
        "a connection that does not hold the view must not adopt it"
    );
    assert!(
        registry
            .holder(&local, 1, 1, &first.initialization)
            .is_some_and(|held| Arc::ptr_eq(&held, &bound)),
        "the refused adoption keeps the old binding and its connection"
    );

    let second = registry.open(adopting(
        registry.view(&local, 2, 1, 3, Lifecycle::Adopt),
        1,
        &bound,
    ))?;
    assert_eq!(
        second.preview.secret, first.preview.secret,
        "the same generation adopts after a refusal, so the refusal raised no fence"
    );
    registry.bind(&second.preview.secret, &bound, 1, 3)?;
    assert_eq!(
        registry.head(&local, 1, 2, &second.initialization),
        Some(SourceRevision(3)),
        "the accepted adoption advances the view to its head"
    );
    Ok(())
}

/// Decision 0054, rule 4: a committed write is recorded only for the live view
/// that pins its base and holds its initialization. The record survives an
/// adoption onto another revision until an adoption uses it.
#[test]
fn a_written_revision_survives_an_adoption_that_does_not_use_it() -> TestResult {
    let registry = Registry::new();
    let local: ActorId = "local".parse()?;
    let session = registry.connect(&local)?;
    let first = registry.open(registry.view(&local, 1, 1, 1, Lifecycle::Adopt))?;
    registry.bind(&first.preview.secret, &session, 1, 1)?;
    registry.serve(&first.preview.secret)?;

    // A wrong initialization and a base other than the pinned revision record nothing.
    registry.tickets.record_written(
        &key(&local, &registry.artifact, 1, 1, "incorrect"),
        SourceRevision(2),
    );
    registry.tickets.record_written(
        &key(&local, &registry.artifact, 1, 2, &first.initialization),
        SourceRevision(2),
    );
    let refusal = registry.refusal(adopting(
        registry.view(&local, 2, 2, 3, Lifecycle::Adopt),
        1,
        &session,
    ));
    assert_eq!(
        refusal,
        Some(RuntimeIssueError::NotAdoptable),
        "a revision that no matching write created cannot be adopted"
    );

    registry.tickets.record_written(
        &key(&local, &registry.artifact, 1, 1, &first.initialization),
        SourceRevision(2),
    );
    // This adoption moves the view to revision 1, so the recorded revision 2 is kept.
    let kept = registry.open(adopting(
        registry.view(&local, 2, 1, 3, Lifecycle::Adopt),
        1,
        &session,
    ))?;
    registry.bind(&kept.preview.secret, &session, 1, 3)?;
    // A later adoption onto revision 2 can still use the record.
    let moved = registry.open(adopting(
        registry.view(&local, 3, 2, 4, Lifecycle::Adopt),
        2,
        &session,
    ))?;
    registry.bind(&moved.preview.secret, &session, 2, 4)?;

    assert_eq!(
        moved.preview.secret, first.preview.secret,
        "every adoption keeps the ticket secret"
    );
    assert_eq!(
        registry.head(&local, 2, 3, &moved.initialization),
        Some(SourceRevision(4)),
        "the frame is judged against the revision it adopted"
    );
    assert_eq!(
        registry.head(&local, 1, 3, &moved.initialization),
        None,
        "the revision the frame left no longer binds"
    );
    let ticket = registry
        .tickets
        .resolve(&moved.preview.secret)
        .ok_or("the adopted ticket must still resolve")?;
    assert_eq!(
        ticket.revision,
        SourceRevision(1),
        "the document revision stays the one served at creation"
    );
    Ok(())
}

/// Decision 0054, rule 4: an adoption needs the `adopt` lifecycle on both the
/// old view and the revision it adopts.
#[test]
fn adoption_needs_adopt_on_both_sides() -> TestResult {
    let local: ActorId = "local".parse()?;
    for (old, new) in [
        (Lifecycle::Restart, Lifecycle::Adopt),
        (Lifecycle::Adopt, Lifecycle::Restart),
        (Lifecycle::Restart, Lifecycle::Restart),
    ] {
        let registry = Registry::new();
        let session = registry.connect(&local)?;
        let first = registry.open(registry.view(&local, 1, 1, 1, old))?;
        registry.bind(&first.preview.secret, &session, 1, 1)?;
        registry.serve(&first.preview.secret)?;
        let refusal = registry.refusal(adopting(registry.view(&local, 2, 1, 3, new), 1, &session));
        assert_eq!(
            refusal,
            Some(RuntimeIssueError::NotAdoptable),
            "an adoption from {old:?} to {new:?} must be refused"
        );
        assert_eq!(
            registry.head(&local, 1, 1, &first.initialization),
            Some(SourceRevision(1)),
            "a refused adoption from {old:?} to {new:?} leaves the view bound"
        );
    }
    Ok(())
}

/// Decision 0054, rule 5: an adopted view takes the token of the adopting
/// request, so its lease ends with that token and not with the token that
/// opened it.
#[test]
fn an_adopted_view_ends_with_the_token_that_adopted_it() -> TestResult {
    let registry = Registry::new();
    let local: ActorId = "local".parse()?;
    let opening = TokenId::from_random([1; 16]);
    let adopting_token = TokenId::from_random([2; 16]);
    let session = registry.connect(&local)?;
    let first = registry.open(RuntimeTicketRequest {
        token_id: Some(opening.clone()),
        ..registry.view(&local, 1, 1, 1, Lifecycle::Adopt)
    })?;
    registry.bind(&first.preview.secret, &session, 1, 1)?;
    registry.serve(&first.preview.secret)?;

    let adopted = registry.open(adopting(
        RuntimeTicketRequest {
            token_id: Some(adopting_token.clone()),
            ..registry.view(&local, 2, 1, 3, Lifecycle::Adopt)
        },
        1,
        &session,
    ))?;
    let secret = adopted.preview.secret;

    registry.tickets.revoke_token(&opening);
    assert!(
        registry.tickets.resolve(&secret).is_some(),
        "revoking the token that opened the view must not end the view that an adoption under another token renewed"
    );
    registry.tickets.revoke_token(&adopting_token);
    assert!(
        registry.tickets.resolve(&secret).is_none(),
        "revoking the token that adopted the view must end it"
    );
    Ok(())
}
