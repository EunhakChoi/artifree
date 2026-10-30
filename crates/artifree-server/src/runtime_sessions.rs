//! Bounded, process-local runtime connections and revocable grants.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::auth::hex;
use artifree_domain::SourceRevision;
use artifree_domain::identity::{ActorId, ArtifactId};

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
    allowed: bool,
}

#[derive(Debug)]
struct SessionState {
    closed: bool,
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

    /// Runs an issuance while close and grant changes are excluded.
    pub(crate) fn with_grant<T>(
        &self,
        scope: &GrantScope,
        issue: impl FnOnce(u64, bool) -> T,
    ) -> Result<T, SessionError> {
        let state = self.state.lock().map_err(|_| SessionError::Unavailable)?;
        if !Self::active(&state) {
            return Err(SessionError::Closed);
        }
        let grant = state.grants.get(scope);
        Ok(issue(
            grant.map_or(0, |grant| grant.version),
            grant.is_some_and(|grant| grant.allowed),
        ))
    }

    /// Replaces the supported grant; a revoked version cannot be revived.
    pub(crate) fn set_grant(
        &self,
        scope: GrantScope,
        metadata_read: bool,
        expected_version: u64,
    ) -> Result<u64, SessionError> {
        let mut state = self.state.lock().map_err(|_| SessionError::Unavailable)?;
        if !Self::active(&state) {
            return Err(SessionError::Closed);
        }
        let current = state.grants.get(&scope).copied().unwrap_or(Grant {
            version: 0,
            allowed: false,
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
                allowed: metadata_read,
            },
        );
        Ok(next)
    }

    /// Every RPC checks the exact grant bound to its view.
    pub(crate) fn permits(&self, scope: &GrantScope, version: u64) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        Self::active(&state)
            && version != 0
            && state
                .grants
                .get(scope)
                .is_some_and(|grant| grant.allowed && grant.version == version)
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
    pub(crate) fn create(&self, actor: &ActorId) -> Result<String, SessionError> {
        let now = Instant::now();
        let expires = now
            .checked_add(self.lifetime)
            .ok_or(SessionError::Unavailable)?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| SessionError::Unavailable)?;
        Self::prune(&mut sessions, now);
        if sessions.len() >= MAX_SESSIONS {
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
                expires,
                next_version: 0,
                grants: HashMap::new(),
            }),
        });
        sessions.insert(key, Arc::clone(&session));
        Ok(id)
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
    use artifree_domain::identity::{ActorId, ArtifactId};

    use super::{GrantScope, RuntimeSessions, SessionError};

    #[test]
    fn grant_revoke_regrant_and_logout_reject_old_bindings()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let other: ActorId = "other".parse()?;
        let sessions = RuntimeSessions::new(Duration::from_secs(60));
        let id = sessions.create(&actor).map_err(|e| format!("{e:?}"))?;
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
        assert!(!session.permits(&scope, 0));
        let first = session
            .set_grant(scope.clone(), true, 0)
            .map_err(|e| format!("{e:?}"))?;
        assert!(session.permits(&scope, first));
        let other_revision =
            GrantScope::new(scope.artifact.clone(), SourceRevision(2), SourceRevision(1));
        let other_head =
            GrantScope::new(scope.artifact.clone(), SourceRevision(1), SourceRevision(2));
        assert!(!session.permits(&other_revision, first));
        assert!(!session.permits(&other_head, first));
        let revoked = session
            .set_grant(scope.clone(), false, first)
            .map_err(|e| format!("{e:?}"))?;
        assert!(!session.permits(&scope, first));
        assert!(matches!(
            session.set_grant(scope.clone(), true, first),
            Err(SessionError::Conflict)
        ));
        let second = session
            .set_grant(scope.clone(), true, revoked)
            .map_err(|e| format!("{e:?}"))?;
        assert_ne!(first, second);
        assert!(!session.permits(&scope, first));
        assert!(session.permits(&scope, second));
        sessions.close(&actor, &id).map_err(|e| format!("{e:?}"))?;
        assert!(!session.permits(&scope, second));
        assert!(matches!(
            session.set_grant(scope, true, second),
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
        let id = sessions.create(&actor).map_err(|e| format!("{e:?}"))?;
        let session = sessions.get(&actor, &id).map_err(|e| format!("{e:?}"))?;
        let pending = Arc::clone(&session);
        sessions.close(&actor, &id).map_err(|e| format!("{e:?}"))?;
        let next = sessions.create(&actor).map_err(|e| format!("{e:?}"))?;
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
        let id = sessions.create(&actor).map_err(|e| format!("{e:?}"))?;
        assert!(matches!(
            sessions.get(&actor, &id),
            Err(SessionError::Closed)
        ));
        Ok(())
    }

    #[test]
    fn clearing_an_empty_grant_fences_a_late_permission_change()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let sessions = RuntimeSessions::new(Duration::from_secs(60));
        let id = sessions
            .create(&actor)
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
            .set_grant(scope.clone(), false, 0)
            .map_err(|error| format!("{error:?}"))?;
        assert_ne!(barrier, 0);
        assert!(matches!(
            session.set_grant(scope.clone(), true, 0),
            Err(SessionError::Conflict)
        ));
        assert!(!session.permits(&scope, barrier));
        let allowed = session
            .set_grant(scope.clone(), true, barrier)
            .map_err(|error| format!("{error:?}"))?;
        assert!(session.permits(&scope, allowed));
        Ok(())
    }
}
