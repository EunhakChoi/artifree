//! Bounded, process-local runtime connections and revocable grants.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::auth::hex;
use crate::runtime_policy::{Capability, CapabilitySet};
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId, TokenId};

/// Open connections one actor may hold (decision 0036): the bound is per
/// actor so that one user's connections cannot use up the others' places.
const MAX_SESSIONS: usize = 4096;
const MAX_GRANTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct GrantScope {
    pub(crate) artifact: ArtifactId,
    pub(crate) revision: u64,
    pub(crate) head: u64,
}

impl GrantScope {
    pub(crate) fn new(
        artifact: ArtifactId,
        revision: SourceRevision,
        head: SourceRevision,
    ) -> Self {
        Self {
            artifact,
            revision: revision.0,
            head: head.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionError {
    Missing,
    Closed,
    Conflict,
    Unavailable,
}

#[derive(Debug, Clone, Copy)]
struct Grant {
    version: u64,
    capabilities: CapabilitySet,
}

/// Why a view's grant does not permit a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantDenied {
    /// The connection is closed or expired: the view no longer exists.
    Closed,
    /// The connection lives, but this grant version does not include the
    /// capability: it was revoked, replaced, or never given.
    NotGranted,
}

#[derive(Debug)]
struct SessionState {
    closed: bool,
    token_id: Option<TokenId>,
    expires: Instant,
    next_version: u64,
    grants: HashMap<GrantScope, Grant>,
}

/// One connection identity. Held handles never become a later connection.
#[derive(Debug)]
pub(crate) struct RuntimeSession {
    state: Mutex<SessionState>,
}

impl RuntimeSession {
    fn active(state: &SessionState) -> bool {
        !state.closed && state.expires > Instant::now()
    }

    /// Runs a handoff transition while connection close is excluded.
    pub(crate) fn while_active<T>(&self, action: impl FnOnce() -> T) -> Result<T, SessionError> {
        let state = self.state.lock().map_err(|_| SessionError::Unavailable)?;
        if !Self::active(&state) {
            return Err(SessionError::Closed);
        }
        Ok(action())
    }

    /// Runs an issuance while close and grant changes are excluded.
    pub(crate) fn with_grant<T>(
        &self,
        scope: &GrantScope,
        issue: impl FnOnce(u64, CapabilitySet) -> T,
    ) -> Result<T, SessionError> {
        let state = self.state.lock().map_err(|_| SessionError::Unavailable)?;
        if !Self::active(&state) {
            return Err(SessionError::Closed);
        }
        let grant = state.grants.get(scope);
        Ok(issue(
            grant.map_or(0, |grant| grant.version),
            grant.map_or_else(CapabilitySet::default, |grant| grant.capabilities),
        ))
    }

    /// Replaces the supported grant; a revoked version cannot be revived.
    pub(crate) fn set_grant(
        &self,
        scope: GrantScope,
        capabilities: CapabilitySet,
        expected_version: u64,
    ) -> Result<u64, SessionError> {
        let mut state = self.state.lock().map_err(|_| SessionError::Unavailable)?;
        if !Self::active(&state) {
            return Err(SessionError::Closed);
        }
        let current = state.grants.get(&scope).copied().unwrap_or(Grant {
            version: 0,
            capabilities: CapabilitySet::default(),
        });
        if expected_version != current.version {
            return Err(SessionError::Conflict);
        }
        if !state.grants.contains_key(&scope) && state.grants.len() >= MAX_GRANTS {
            return Err(SessionError::Unavailable);
        }
        let next = state
            .next_version
            .checked_add(1)
            .filter(|version| *version <= 9_007_199_254_740_991)
            .ok_or(SessionError::Unavailable)?;
        state.next_version = next;
        state.grants.insert(
            scope,
            Grant {
                version: next,
                capabilities,
            },
        );
        Ok(next)
    }

    /// Every RPC checks the exact grant bound to its view, for the one
    /// capability it needs. A grant is a set; another capability's consent
    /// never stands in for this one.
    pub(crate) fn permits(
        &self,
        scope: &GrantScope,
        version: u64,
        capability: Capability,
    ) -> Result<(), GrantDenied> {
        let Ok(state) = self.state.lock() else {
            return Err(GrantDenied::Closed);
        };
        if !Self::active(&state) {
            return Err(GrantDenied::Closed);
        }
        let granted = version != 0
            && state.grants.get(scope).is_some_and(|grant| {
                grant.version == version && grant.capabilities.contains(capability)
            });
        if granted {
            Ok(())
        } else {
            Err(GrantDenied::NotGranted)
        }
    }
}

/// Owns issued session IDs; outstanding handles keep their own closed state.
#[derive(Debug)]
pub(crate) struct RuntimeSessions {
    lifetime: Duration,
    sessions: Mutex<HashMap<(ActorId, String), Arc<RuntimeSession>>>,
}

impl RuntimeSessions {
    pub(crate) fn new(lifetime: Duration) -> Self {
        Self {
            lifetime,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn prune(sessions: &mut HashMap<(ActorId, String), Arc<RuntimeSession>>, now: Instant) {
        sessions.retain(|_, session| {
            let Ok(state) = session.state.lock() else {
                return true;
            };
            state.expires > now
        });
    }

    /// Allocates a new random identity. No API can recreate a chosen identity.
    pub(crate) fn create(
        &self,
        actor: &ActorId,
        token_id: Option<TokenId>,
    ) -> Result<String, SessionError> {
        let now = Instant::now();
        let expires = now
            .checked_add(self.lifetime)
            .ok_or(SessionError::Unavailable)?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| SessionError::Unavailable)?;
        Self::prune(&mut sessions, now);
        let held = sessions.keys().filter(|(owner, _)| owner == actor).count();
        if held >= MAX_SESSIONS {
            return Err(SessionError::Unavailable);
        }
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|_| SessionError::Unavailable)?;
        let id = hex(&random);
        let key = (actor.clone(), id.clone());
        if sessions.contains_key(&key) {
            return Err(SessionError::Unavailable);
        }
        let session = Arc::new(RuntimeSession {
            state: Mutex::new(SessionState {
                closed: false,
                token_id,
                expires,
                next_version: 0,
                grants: HashMap::new(),
            }),
        });
        sessions.insert(key, Arc::clone(&session));
        Ok(id)
    }

    /// Closes all runtime sessions created under `token_id`.
    pub(crate) fn revoke_token(&self, token_id: &TokenId) {
        if let Ok(sessions) = self.sessions.lock() {
            for session in sessions.values() {
                if let Ok(mut state) = session.state.lock() {
                    state.closed = state.closed || state.token_id.as_ref() == Some(token_id);
                }
            }
        }
    }

    /// Closes every runtime session of `actor` and forgets it, as when the
    /// user is disabled (decision 0036). Its grants go with them. The entries
    /// are removed, not just marked closed: a closed entry would keep
    /// counting against the actor's [`MAX_SESSIONS`] until its lifetime ended,
    /// and an actor at the bound would find every new connection refused after
    /// being enabled again. A handle held elsewhere stays closed.
    pub(crate) fn revoke_actor(&self, actor: &ActorId) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.retain(|(owner, _), session| {
                if owner != actor {
                    return true;
                }
                if let Ok(mut state) = session.state.lock() {
                    state.closed = true;
                    state.grants.clear();
                }
                false
            });
        }
    }

    /// How many connections `actor` holds, for tests.
    #[cfg(test)]
    pub(crate) fn held_by(&self, actor: &ActorId) -> usize {
        self.sessions.lock().map_or(usize::MAX, |sessions| {
            sessions.keys().filter(|(owner, _)| owner == actor).count()
        })
    }

    pub(crate) fn get(
        &self,
        actor: &ActorId,
        id: &str,
    ) -> Result<Arc<RuntimeSession>, SessionError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| SessionError::Unavailable)?;
        let session = sessions
            .get(&(actor.clone(), id.to_owned()))
            .ok_or(SessionError::Missing)?;
        let state = session
            .state
            .lock()
            .map_err(|_| SessionError::Unavailable)?;
        if !RuntimeSession::active(&state) {
            return Err(SessionError::Closed);
        }
        Ok(Arc::clone(session))
    }

    /// Closes an issued identity; unknown and already closed IDs are harmless.
    pub(crate) fn close(&self, actor: &ActorId, id: &str) -> Result<(), SessionError> {
        let now = Instant::now();
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| SessionError::Unavailable)?;
        Self::prune(&mut sessions, now);
        let key = (actor.clone(), id.to_owned());
        if let Some(session) = sessions.get(&key) {
            let mut state = session
                .state
                .lock()
                .map_err(|_| SessionError::Unavailable)?;
            state.expires = now;
            state.closed = true;
            state.grants.clear();
            return Ok(());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use artifree_domain::SourceRevision;
    use artifree_domain::identity::{ActorId, ArtifactId, TokenId};

    use super::{GrantDenied, GrantScope, RuntimeSessions, SessionError};
    use crate::runtime_policy::{Capability, CapabilitySet, RuntimePolicy};

    fn all() -> Result<CapabilitySet, Box<dyn std::error::Error>> {
        let policy = RuntimePolicy::from_source(
            true,
            Some(
                br#"{"sdk":"artifree.app.v1","capabilities":["assets.download","assets.read","assets.write","data.read","data.write","metadata.read","source.download","source.read","source.write"]}"#,
            ),
        )?;
        policy
            .grant_of(&RuntimePolicy::supported())
            .ok_or_else(|| "the fixture grant must be valid".into())
    }

    fn only(name: &str) -> Result<CapabilitySet, Box<dyn std::error::Error>> {
        let policy = RuntimePolicy::from_source(
            true,
            Some(
                br#"{"sdk":"artifree.app.v1","capabilities":["assets.download","assets.read","assets.write","data.read","data.write","metadata.read","source.download","source.read","source.write"]}"#,
            ),
        )?;
        policy
            .grant_of(&[name.to_owned()])
            .ok_or_else(|| "the fixture grant must be valid".into())
    }

    fn permits(session: &super::RuntimeSession, scope: &GrantScope, version: u64) -> bool {
        session
            .permits(scope, version, Capability::MetadataRead)
            .is_ok()
    }

    #[test]
    fn grant_revoke_regrant_and_logout_reject_old_bindings()
    -> Result<(), Box<dyn std::error::Error>> {
        let on = only("metadata.read")?;
        let actor: ActorId = "local".parse()?;
        let other: ActorId = "other".parse()?;
        let sessions = RuntimeSessions::new(Duration::from_secs(60));
        let id = sessions
            .create(&actor, None)
            .map_err(|e| format!("{e:?}"))?;
        let session = sessions.get(&actor, &id).map_err(|e| format!("{e:?}"))?;
        assert!(matches!(
            sessions.get(&other, &id),
            Err(SessionError::Missing)
        ));
        let scope = GrantScope::new(
            ArtifactId::from_random([1; 16]),
            SourceRevision(1),
            SourceRevision(1),
        );
        assert!(!permits(&session, &scope, 0));
        let first = session
            .set_grant(scope.clone(), on, 0)
            .map_err(|e| format!("{e:?}"))?;
        assert!(permits(&session, &scope, first));
        let other_revision =
            GrantScope::new(scope.artifact.clone(), SourceRevision(2), SourceRevision(1));
        let other_head =
            GrantScope::new(scope.artifact.clone(), SourceRevision(1), SourceRevision(2));
        assert!(!permits(&session, &other_revision, first));
        assert!(!permits(&session, &other_head, first));
        let revoked = session
            .set_grant(scope.clone(), CapabilitySet::default(), first)
            .map_err(|e| format!("{e:?}"))?;
        assert!(!permits(&session, &scope, first));
        assert!(matches!(
            session.set_grant(scope.clone(), on, first),
            Err(SessionError::Conflict)
        ));
        let second = session
            .set_grant(scope.clone(), on, revoked)
            .map_err(|e| format!("{e:?}"))?;
        assert_ne!(first, second);
        assert!(!permits(&session, &scope, first));
        assert!(permits(&session, &scope, second));
        sessions.close(&actor, &id).map_err(|e| format!("{e:?}"))?;
        assert!(!permits(&session, &scope, second));
        assert!(matches!(
            session.set_grant(scope, on, second),
            Err(SessionError::Closed)
        ));
        assert!(matches!(
            sessions.get(&actor, &id),
            Err(SessionError::Closed | SessionError::Missing)
        ));
        Ok(())
    }

    #[test]
    fn closed_or_restarted_sessions_cannot_be_recreated_by_id()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let sessions = RuntimeSessions::new(Duration::from_secs(60));
        let id = sessions
            .create(&actor, None)
            .map_err(|e| format!("{e:?}"))?;
        let session = sessions.get(&actor, &id).map_err(|e| format!("{e:?}"))?;
        let pending = Arc::clone(&session);
        sessions.close(&actor, &id).map_err(|e| format!("{e:?}"))?;
        let next = sessions
            .create(&actor, None)
            .map_err(|e| format!("{e:?}"))?;
        assert_ne!(id, next);
        assert!(matches!(
            sessions.get(&actor, &id),
            Err(SessionError::Missing)
        ));
        assert!(matches!(
            pending.with_grant(
                &GrantScope::new(
                    ArtifactId::from_random([1; 16]),
                    SourceRevision(1),
                    SourceRevision(1)
                ),
                |_, _| ()
            ),
            Err(SessionError::Closed)
        ));
        drop(pending);
        drop(session);
        let restarted = RuntimeSessions::new(Duration::from_secs(60));
        restarted.close(&actor, &id).map_err(|e| format!("{e:?}"))?;
        assert!(matches!(
            restarted.get(&actor, &id),
            Err(SessionError::Missing)
        ));
        Ok(())
    }

    #[test]
    fn an_expired_session_cannot_be_used() -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let sessions = RuntimeSessions::new(Duration::ZERO);
        let id = sessions
            .create(&actor, None)
            .map_err(|e| format!("{e:?}"))?;
        assert!(matches!(
            sessions.get(&actor, &id),
            Err(SessionError::Closed)
        ));
        Ok(())
    }

    #[test]
    fn clearing_an_empty_grant_fences_a_late_permission_change()
    -> Result<(), Box<dyn std::error::Error>> {
        let on = only("metadata.read")?;
        let actor: ActorId = "local".parse()?;
        let sessions = RuntimeSessions::new(Duration::from_secs(60));
        let id = sessions
            .create(&actor, None)
            .map_err(|error| format!("{error:?}"))?;
        let session = sessions
            .get(&actor, &id)
            .map_err(|error| format!("{error:?}"))?;
        let scope = GrantScope::new(
            ArtifactId::from_random([1; 16]),
            SourceRevision(1),
            SourceRevision(1),
        );
        let barrier = session
            .set_grant(scope.clone(), CapabilitySet::default(), 0)
            .map_err(|error| format!("{error:?}"))?;
        assert_ne!(barrier, 0);
        assert!(matches!(
            session.set_grant(scope.clone(), on, 0),
            Err(SessionError::Conflict)
        ));
        assert!(!permits(&session, &scope, barrier));
        let allowed = session
            .set_grant(scope.clone(), on, barrier)
            .map_err(|error| format!("{error:?}"))?;
        assert!(permits(&session, &scope, allowed));
        Ok(())
    }

    #[test]
    fn a_grant_is_a_set_and_each_capability_needs_its_own_consent()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let sessions = RuntimeSessions::new(Duration::from_secs(60));
        let id = sessions
            .create(&actor, None)
            .map_err(|error| format!("{error:?}"))?;
        let session = sessions
            .get(&actor, &id)
            .map_err(|error| format!("{error:?}"))?;
        let scope = GrantScope::new(
            ArtifactId::from_random([1; 16]),
            SourceRevision(1),
            SourceRevision(1),
        );
        let read = session
            .set_grant(scope.clone(), only("source.read")?, 0)
            .map_err(|error| format!("{error:?}"))?;
        assert_eq!(
            session.permits(&scope, read, Capability::SourceRead),
            Ok(())
        );
        assert_eq!(
            session.permits(&scope, read, Capability::SourceDownload),
            Err(GrantDenied::NotGranted),
            "reading does not consent to downloading"
        );
        assert_eq!(
            session.permits(&scope, read, Capability::SourceWrite),
            Err(GrantDenied::NotGranted),
            "reading does not consent to writing"
        );
        assert_eq!(
            session.permits(&scope, read, Capability::MetadataRead),
            Err(GrantDenied::NotGranted)
        );
        let everything = session
            .set_grant(scope.clone(), all()?, read)
            .map_err(|error| format!("{error:?}"))?;
        for capability in Capability::ALL {
            assert_eq!(session.permits(&scope, everything, capability), Ok(()));
            assert_eq!(
                session.permits(&scope, read, capability),
                Err(GrantDenied::NotGranted),
                "a replaced grant version is not honoured"
            );
        }
        sessions
            .close(&actor, &id)
            .map_err(|error| format!("{error:?}"))?;
        assert_eq!(
            session.permits(&scope, everything, Capability::SourceRead),
            Err(GrantDenied::Closed),
            "a closed connection is not a revoked grant"
        );
        Ok(())
    }

    #[test]
    fn assets_write_is_granted_and_withdrawn_on_its_own() -> Result<(), Box<dyn std::error::Error>>
    {
        let actor: ActorId = "local".parse()?;
        let sessions = RuntimeSessions::new(Duration::from_secs(60));
        let id = sessions
            .create(&actor, None)
            .map_err(|error| format!("{error:?}"))?;
        let session = sessions
            .get(&actor, &id)
            .map_err(|error| format!("{error:?}"))?;
        let scope = GrantScope::new(
            ArtifactId::from_random([1; 16]),
            SourceRevision(1),
            SourceRevision(1),
        );
        let write = session
            .set_grant(scope.clone(), only("assets.write")?, 0)
            .map_err(|error| format!("{error:?}"))?;
        for capability in Capability::ALL {
            let expected = if capability == Capability::AssetsWrite {
                Ok(())
            } else {
                Err(GrantDenied::NotGranted)
            };
            assert_eq!(
                session.permits(&scope, write, capability),
                expected,
                "assets.write alone must permit only itself, checked {capability:?}"
            );
        }
        // Withdrawing assets.write while source.read stays granted withdraws
        // only writing.
        let read = session
            .set_grant(scope.clone(), only("source.read")?, write)
            .map_err(|error| format!("{error:?}"))?;
        assert_eq!(
            session.permits(&scope, read, Capability::AssetsWrite),
            Err(GrantDenied::NotGranted),
            "withdrawing assets.write must withdraw writing"
        );
        assert_eq!(
            session.permits(&scope, read, Capability::SourceRead),
            Ok(()),
            "withdrawing assets.write must leave source.read"
        );
        Ok(())
    }

    #[test]
    fn assets_download_is_granted_and_withdrawn_on_its_own()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let sessions = RuntimeSessions::new(Duration::from_secs(60));
        let id = sessions
            .create(&actor, None)
            .map_err(|error| format!("{error:?}"))?;
        let session = sessions
            .get(&actor, &id)
            .map_err(|error| format!("{error:?}"))?;
        let scope = GrantScope::new(
            ArtifactId::from_random([1; 16]),
            SourceRevision(1),
            SourceRevision(1),
        );
        let download = session
            .set_grant(scope.clone(), only("assets.download")?, 0)
            .map_err(|error| format!("{error:?}"))?;
        for capability in Capability::ALL {
            let expected = if capability == Capability::AssetsDownload {
                Ok(())
            } else {
                Err(GrantDenied::NotGranted)
            };
            assert_eq!(
                session.permits(&scope, download, capability),
                expected,
                "assets.download alone must permit only itself, checked {capability:?}"
            );
        }
        // Withdrawing assets.download while source.read stays granted withdraws
        // only the download.
        let read = session
            .set_grant(scope.clone(), only("source.read")?, download)
            .map_err(|error| format!("{error:?}"))?;
        assert_eq!(
            session.permits(&scope, read, Capability::AssetsDownload),
            Err(GrantDenied::NotGranted),
            "withdrawing assets.download must withdraw downloading"
        );
        assert_eq!(
            session.permits(&scope, read, Capability::SourceRead),
            Ok(()),
            "withdrawing assets.download must leave source.read"
        );
        Ok(())
    }

    #[test]
    fn no_other_capability_alone_permits_downloading() -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let sessions = RuntimeSessions::new(Duration::from_secs(60));
        let id = sessions
            .create(&actor, None)
            .map_err(|error| format!("{error:?}"))?;
        let session = sessions
            .get(&actor, &id)
            .map_err(|error| format!("{error:?}"))?;
        let scope = GrantScope::new(
            ArtifactId::from_random([1; 16]),
            SourceRevision(1),
            SourceRevision(1),
        );
        let mut version = 0;
        for other in [
            "assets.read",
            "assets.write",
            "data.read",
            "data.write",
            "metadata.read",
            "source.download",
            "source.read",
            "source.write",
        ] {
            version = session
                .set_grant(scope.clone(), only(other)?, version)
                .map_err(|error| format!("{error:?}"))?;
            assert_eq!(
                session.permits(&scope, version, Capability::AssetsDownload),
                Err(GrantDenied::NotGranted),
                "{other} alone must not permit downloading"
            );
        }
        Ok(())
    }

    #[test]
    fn token_revocation_closes_associated_sessions() -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let token_1: TokenId = "tok_0123456789abcdef0123456789abcdef".parse()?;
        let token_2: TokenId = "tok_fedcba9876543210fedcba9876543210".parse()?;
        let sessions = RuntimeSessions::new(Duration::from_secs(60));

        let s1 = sessions
            .create(&actor, Some(token_1.clone()))
            .map_err(|e| format!("{e:?}"))?;
        let s2 = sessions
            .create(&actor, Some(token_2))
            .map_err(|e| format!("{e:?}"))?;
        let s_bootstrap = sessions
            .create(&actor, None)
            .map_err(|e| format!("{e:?}"))?;

        assert!(sessions.get(&actor, &s1).is_ok());
        assert!(sessions.get(&actor, &s2).is_ok());
        assert!(sessions.get(&actor, &s_bootstrap).is_ok());

        sessions.revoke_token(&token_1);

        assert!(matches!(
            sessions.get(&actor, &s1),
            Err(SessionError::Closed)
        ));
        assert!(sessions.get(&actor, &s2).is_ok());
        assert!(sessions.get(&actor, &s_bootstrap).is_ok());

        Ok(())
    }

    #[test]
    fn the_bound_is_per_actor_and_closing_one_actor_leaves_the_others()
    -> Result<(), Box<dyn std::error::Error>> {
        let alice: ActorId = "u-00000000000000000000000000000001".parse()?;
        let bob: ActorId = "u-00000000000000000000000000000002".parse()?;
        let sessions = RuntimeSessions::new(Duration::from_secs(60));
        let first = sessions
            .create(&alice, None)
            .map_err(|error| format!("{error:?}"))?;
        for _ in 1..super::MAX_SESSIONS {
            sessions
                .create(&alice, None)
                .map_err(|error| format!("{error:?}"))?;
        }
        assert!(matches!(
            sessions.create(&alice, None),
            Err(SessionError::Unavailable)
        ));
        let bobs = sessions
            .create(&bob, None)
            .map_err(|error| format!("{error:?}"))?;

        let held = sessions
            .get(&alice, &first)
            .map_err(|error| format!("{error:?}"))?;

        sessions.revoke_actor(&alice);

        // The handle taken before stays closed; the entries are forgotten, so
        // an actor that was at the bound can open connections again once
        // enabled, and only its own entries went.
        assert!(matches!(
            held.while_active(|| ()),
            Err(SessionError::Closed)
        ));
        assert!(matches!(
            sessions.get(&alice, &first),
            Err(SessionError::Missing)
        ));
        assert_eq!(sessions.held_by(&alice), 0, "closed entries were kept");
        assert_eq!(sessions.held_by(&bob), 1);
        assert!(
            sessions.create(&alice, None).is_ok(),
            "an actor at the bound cannot connect after a disable and enable"
        );
        assert!(sessions.get(&bob, &bobs).is_ok());
        // A connection id is only valid for the actor that holds it.
        assert!(matches!(
            sessions.get(&bob, &first),
            Err(SessionError::Missing)
        ));
        Ok(())
    }
}
