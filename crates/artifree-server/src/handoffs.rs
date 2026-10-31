//! Bounded, process-local handoffs to explicitly registered agent inboxes.
//!
//! A workspace connection and the registry serialize send, cancel and offer:
//! a connection close cannot be followed by an offer of a queued delivery.
//! While the connection remains active, an offered delivery is replayed until
//! its agent acknowledges receipt. Closing it stops further delivery, but
//! cannot recall an item the agent has already received.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use artifree_contracts::api::{AgentSession, Handoff, HandoffSelection, HandoffState};
use artifree_contracts::mcp::{HandoffReceipt, RegisteredHandoffSession};
use artifree_core::parts::FilePart;
use artifree_domain::identity::ActorId;

use crate::auth::{AccessToken, hex};
use crate::runtime_sessions::{RuntimeSession, SessionError};

const MAX_AGENTS: usize = 128;
const MAX_HANDOFFS: usize = 512;
const MAX_SELECTION_BYTES: usize = 16 * 1024;
const MAX_INTENT_BYTES: usize = 4 * 1024;

/// A refused handoff transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandoffError {
    /// Unknown, expired, or inaccessible identity.
    Missing,
    /// Invalid input.
    Invalid(&'static str),
    /// A transition cannot be made from the current state.
    Conflict(&'static str),
    /// State or randomness is unavailable.
    Unavailable,
    /// The owning workspace connection is no longer active.
    ConnectionClosed,
}

struct Agent {
    owner: ActorId,
    name: String,
    secret: AccessToken,
    expires: Instant,
}

struct Item {
    owner: ActorId,
    workspace_id: String,
    workspace: Arc<RuntimeSession>,
    handoff: Handoff,
    expires: Instant,
    sequence: u64,
    suspended: bool,
}

#[derive(Default)]
struct Registry {
    agents: HashMap<String, Agent>,
    items: HashMap<String, Item>,
    next_sequence: u64,
}

impl Registry {
    fn prune(&mut self, now: Instant) {
        self.agents.retain(|_, agent| agent.expires > now);
        self.items.retain(|_, item| item.expires > now);
    }

    fn agent(&self, actor: &ActorId, id: &str, secret: &str) -> Result<&Agent, HandoffError> {
        self.agents
            .get(id)
            .filter(|agent| agent.owner == *actor && agent.secret.matches(secret))
            .ok_or(HandoffError::Missing)
    }

    fn item(&self, actor: &ActorId, workspace: &str, id: &str) -> Result<&Item, HandoffError> {
        self.items
            .get(id)
            .filter(|item| item.owner == *actor && item.workspace_id == workspace)
            .ok_or(HandoffError::Missing)
    }
}

/// Owns agent sessions and proposed deliveries for one server process.
pub(crate) struct Handoffs {
    lifetime: Duration,
    state: Mutex<Registry>,
}

impl fmt::Debug for Handoffs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Handoffs(..)")
    }
}

impl Handoffs {
    pub(crate) fn new(lifetime: Duration) -> Self {
        Self {
            lifetime,
            state: Mutex::new(Registry::default()),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, Registry>, HandoffError> {
        self.state.lock().map_err(|_| HandoffError::Unavailable)
    }

    fn expiry(&self, now: Instant) -> Result<Instant, HandoffError> {
        now.checked_add(self.lifetime)
            .ok_or(HandoffError::Unavailable)
    }

    fn random_id() -> Result<String, HandoffError> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| HandoffError::Unavailable)?;
        Ok(hex(&bytes))
    }

    /// Registers a new inbox. A lost reply cannot send it a delivery.
    pub(crate) fn register(
        &self,
        actor: &ActorId,
        name: &str,
    ) -> Result<RegisteredHandoffSession, HandoffError> {
        if name.trim() != name
            || name.is_empty()
            || name.chars().count() > 80
            || name.chars().any(char::is_control)
        {
            return Err(HandoffError::Invalid(
                "agent name must be 1-80 visible characters",
            ));
        }
        let now = Instant::now();
        let expires = self.expiry(now)?;
        let id = Self::random_id()?;
        let secret = AccessToken::generate().map_err(|_| HandoffError::Unavailable)?;
        let mut registry = self.lock()?;
        registry.prune(now);
        if registry.agents.len() >= MAX_AGENTS || registry.agents.contains_key(&id) {
            return Err(HandoffError::Unavailable);
        }
        let result = RegisteredHandoffSession {
            session_id: id.clone(),
            session_secret: secret.secret().to_owned(),
        };
        registry.agents.insert(
            id,
            Agent {
                owner: actor.clone(),
                name: name.to_owned(),
                secret,
                expires,
            },
        );
        Ok(result)
    }

    /// Lists registered names, without presenting registration as liveness.
    pub(crate) fn list(&self, actor: &ActorId) -> Result<Vec<AgentSession>, HandoffError> {
        let mut registry = self.lock()?;
        registry.prune(Instant::now());
        let mut agents: Vec<_> = registry
            .agents
            .iter()
            .filter(|(_, agent)| agent.owner == *actor)
            .map(|(id, agent)| AgentSession {
                id: id.clone(),
                name: agent.name.clone(),
            })
            .collect();
        agents.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        Ok(agents)
    }

    /// Stages already validated source while the workspace remains active.
    pub(crate) fn stage(
        &self,
        actor: &ActorId,
        workspace_id: &str,
        workspace: &Arc<RuntimeSession>,
        agent_id: &str,
        selection: HandoffSelection,
        intent: String,
    ) -> Result<Handoff, HandoffError> {
        let now = Instant::now();
        let expires = self.expiry(now)?;
        let id = Self::random_id()?;
        workspace
            .while_active(|| {
                let mut registry = self.lock()?;
                registry.prune(now);
                if registry.items.len() >= MAX_HANDOFFS || registry.items.contains_key(&id) {
                    return Err(HandoffError::Unavailable);
                }
                let agent = registry
                    .agents
                    .get(agent_id)
                    .filter(|agent| agent.owner == *actor)
                    .ok_or(HandoffError::Missing)?;
                let handoff = Handoff {
                    id: id.clone(),
                    agent_session_id: agent_id.to_owned(),
                    agent_name: agent.name.clone(),
                    state: HandoffState::Staged,
                    selection,
                    intent,
                };
                let sequence = registry
                    .next_sequence
                    .checked_add(1)
                    .ok_or(HandoffError::Unavailable)?;
                registry.next_sequence = sequence;
                registry.items.insert(
                    id,
                    Item {
                        owner: actor.clone(),
                        workspace_id: workspace_id.to_owned(),
                        workspace: Arc::clone(workspace),
                        handoff: handoff.clone(),
                        expires,
                        sequence,
                        suspended: false,
                    },
                );
                Ok(handoff)
            })
            .map_err(session_error)?
    }

    /// Reads the state only for the actor and workspace that staged it.
    pub(crate) fn get(
        &self,
        actor: &ActorId,
        workspace: &str,
        id: &str,
    ) -> Result<Handoff, HandoffError> {
        let connection = {
            let mut registry = self.lock()?;
            registry.prune(Instant::now());
            Arc::clone(&registry.item(actor, workspace, id)?.workspace)
        };
        match connection.while_active(|| self.status(actor, workspace, id, true)) {
            Ok(result) => result,
            Err(SessionError::Closed | SessionError::Missing) => {
                self.status(actor, workspace, id, false)
            }
            Err(SessionError::Conflict | SessionError::Unavailable) => {
                Err(HandoffError::Unavailable)
            }
        }
    }

    fn status(
        &self,
        actor: &ActorId,
        workspace: &str,
        id: &str,
        connection_active: bool,
    ) -> Result<Handoff, HandoffError> {
        let mut registry = self.lock()?;
        registry.prune(Instant::now());
        let agent_id = &registry
            .item(actor, workspace, id)?
            .handoff
            .agent_session_id;
        let agent_active = registry
            .agents
            .get(agent_id)
            .is_some_and(|agent| agent.owner == *actor);
        let item = registry.items.get_mut(id).ok_or(HandoffError::Missing)?;
        if !connection_active || !agent_active {
            match item.handoff.state {
                HandoffState::Staged | HandoffState::Queued => {
                    item.handoff.state = HandoffState::Expired;
                }
                HandoffState::Offered => item.suspended = true,
                _ => {}
            }
        }
        Ok(item.handoff.clone())
    }

    /// Confirms a stage exactly once, or returns its later state on retry.
    pub(crate) fn send(
        &self,
        actor: &ActorId,
        workspace_id: &str,
        id: &str,
    ) -> Result<Handoff, HandoffError> {
        let workspace = {
            let mut registry = self.lock()?;
            registry.prune(Instant::now());
            Arc::clone(&registry.item(actor, workspace_id, id)?.workspace)
        };
        workspace
            .while_active(|| {
                let mut registry = self.lock()?;
                registry.prune(Instant::now());
                let agent_id = registry
                    .item(actor, workspace_id, id)?
                    .handoff
                    .agent_session_id
                    .clone();
                if registry
                    .agents
                    .get(&agent_id)
                    .is_none_or(|agent| agent.owner != *actor)
                {
                    return Err(HandoffError::Missing);
                }
                let item = registry
                    .items
                    .get_mut(id)
                    .filter(|item| item.owner == *actor && item.workspace_id == workspace_id)
                    .ok_or(HandoffError::Missing)?;
                match item.handoff.state {
                    HandoffState::Staged => item.handoff.state = HandoffState::Queued,
                    HandoffState::Queued | HandoffState::Offered | HandoffState::Acknowledged => {}
                    HandoffState::Cancelled | HandoffState::Expired => {
                        return Err(HandoffError::Conflict("this handoff cannot be sent"));
                    }
                }
                Ok(item.handoff.clone())
            })
            .map_err(session_error)?
    }

    /// Cancels only before an agent may have received the delivery.
    pub(crate) fn cancel(
        &self,
        actor: &ActorId,
        workspace: &str,
        id: &str,
    ) -> Result<Handoff, HandoffError> {
        let mut registry = self.lock()?;
        registry.prune(Instant::now());
        registry.item(actor, workspace, id)?;
        let item = registry.items.get_mut(id).ok_or(HandoffError::Missing)?;
        match item.handoff.state {
            HandoffState::Staged | HandoffState::Queued => {
                item.handoff.state = HandoffState::Cancelled;
            }
            HandoffState::Cancelled => {}
            HandoffState::Offered | HandoffState::Acknowledged => {
                return Err(HandoffError::Conflict(
                    "this handoff can no longer be recalled",
                ));
            }
            HandoffState::Expired => {
                return Err(HandoffError::Conflict("workspace or agent session expired"));
            }
        }
        Ok(item.handoff.clone())
    }

    /// Offers the earliest queued item or repeats the earliest offered item.
    pub(crate) fn poll(
        &self,
        actor: &ActorId,
        agent_id: &str,
        secret: &str,
    ) -> Result<Vec<Handoff>, HandoffError> {
        loop {
            let candidate = {
                let now = Instant::now();
                let expires = self.expiry(now)?;
                let mut registry = self.lock()?;
                registry.prune(now);
                registry.agent(actor, agent_id, secret)?;
                if let Some(agent) = registry.agents.get_mut(agent_id) {
                    agent.expires = expires;
                }
                registry
                    .items
                    .values()
                    .filter(|item| {
                        item.owner == *actor
                            && !item.suspended
                            && item.handoff.agent_session_id == agent_id
                            && matches!(
                                item.handoff.state,
                                HandoffState::Queued | HandoffState::Offered
                            )
                    })
                    .min_by_key(|item| item.sequence)
                    .map(|item| (item.handoff.id.clone(), Arc::clone(&item.workspace)))
            };
            let Some((id, workspace)) = candidate else {
                return Ok(Vec::new());
            };
            let offer = || {
                let mut registry = self.lock()?;
                registry.prune(Instant::now());
                registry.agent(actor, agent_id, secret)?;
                let Some(item) = registry.items.get_mut(&id) else {
                    return Ok(None);
                };
                if item.owner != *actor || item.handoff.agent_session_id != agent_id {
                    return Err(HandoffError::Missing);
                }
                match item.handoff.state {
                    HandoffState::Queued => item.handoff.state = HandoffState::Offered,
                    HandoffState::Offered => {}
                    _ => return Ok(None),
                }
                Ok(Some(item.handoff.clone()))
            };
            let offered = match workspace.while_active(offer) {
                Ok(result) => result?,
                Err(SessionError::Closed | SessionError::Missing) => {
                    let mut registry = self.lock()?;
                    if let Some(item) = registry.items.get_mut(&id) {
                        match item.handoff.state {
                            HandoffState::Queued => item.handoff.state = HandoffState::Expired,
                            HandoffState::Offered => item.suspended = true,
                            _ => {}
                        }
                    }
                    None
                }
                Err(SessionError::Conflict | SessionError::Unavailable) => {
                    return Err(HandoffError::Unavailable);
                }
            };
            if let Some(handoff) = offered {
                return Ok(vec![handoff]);
            }
        }
    }

    /// Rechecks the offer after its source lookup, serialized with logout.
    pub(crate) fn confirm_offer(
        &self,
        actor: &ActorId,
        agent_id: &str,
        secret: &str,
        id: &str,
    ) -> Result<bool, HandoffError> {
        let workspace =
            {
                let mut registry = self.lock()?;
                registry.prune(Instant::now());
                registry.agent(actor, agent_id, secret)?;
                let Some(item) = registry.items.get(id).filter(|item| {
                    item.owner == *actor && item.handoff.agent_session_id == agent_id
                }) else {
                    return Ok(false);
                };
                Arc::clone(&item.workspace)
            };
        workspace
            .while_active(|| {
                let mut registry = self.lock()?;
                registry.prune(Instant::now());
                registry.agent(actor, agent_id, secret)?;
                Ok(registry.items.get(id).is_some_and(|item| {
                    item.owner == *actor
                        && item.handoff.agent_session_id == agent_id
                        && !item.suspended
                        && item.handoff.state == HandoffState::Offered
                }))
            })
            .map_err(session_error)?
    }

    /// Acknowledges only an item actually offered to this exact inbox.
    pub(crate) fn acknowledge(
        &self,
        actor: &ActorId,
        agent_id: &str,
        secret: &str,
        id: &str,
    ) -> Result<HandoffReceipt, HandoffError> {
        let mut registry = self.lock()?;
        registry.prune(Instant::now());
        registry.agent(actor, agent_id, secret)?;
        let item = registry
            .items
            .get_mut(id)
            .filter(|item| item.owner == *actor && item.handoff.agent_session_id == agent_id)
            .ok_or(HandoffError::Missing)?;
        match item.handoff.state {
            HandoffState::Offered => item.handoff.state = HandoffState::Acknowledged,
            HandoffState::Acknowledged => {}
            _ => {
                return Err(HandoffError::Conflict(
                    "delivery was not offered to this session",
                ));
            }
        }
        Ok(HandoffReceipt {
            delivery_id: id.to_owned(),
            state: HandoffState::Acknowledged,
        })
    }
}

fn session_error(error: SessionError) -> HandoffError {
    match error {
        SessionError::Missing | SessionError::Closed => HandoffError::ConnectionClosed,
        SessionError::Conflict | SessionError::Unavailable => HandoffError::Unavailable,
    }
}

/// Verifies a saved file and reconstructs the editor's selected text.
pub(crate) fn selection(
    artifact_id: String,
    revision: u64,
    path: String,
    digest: &str,
    start: u64,
    end: u64,
    file: FilePart,
) -> Result<HandoffSelection, HandoffError> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(HandoffError::Invalid("invalid source file digest"));
    }
    if hex(&file.sha256) != digest {
        return Err(HandoffError::Conflict(
            "the saved file no longer matches the selection",
        ));
    }
    let text = String::from_utf8(file.content)
        .map_err(|_| HandoffError::Invalid("the selected file is not UTF-8 text"))?;
    let bytes = text.as_bytes();
    let crlf = text.contains("\r\n");
    for (index, byte) in bytes.iter().enumerate() {
        if (*byte == b'\r' && bytes.get(index + 1) != Some(&b'\n'))
            || (*byte == b'\n' && crlf && (index == 0 || bytes[index - 1] != b'\r'))
        {
            return Err(HandoffError::Invalid(
                "the file's line endings are not editable",
            ));
        }
    }
    let normalized = text.replace("\r\n", "\n");
    let first = utf16_boundary(&normalized, start).ok_or(HandoffError::Invalid(
        "selection start is not a character boundary",
    ))?;
    let last = utf16_boundary(&normalized, end).ok_or(HandoffError::Invalid(
        "selection end is not a character boundary",
    ))?;
    if first >= last || last - first > MAX_SELECTION_BYTES {
        return Err(HandoffError::Invalid(
            "selection must contain at most 16 KiB of text",
        ));
    }
    Ok(HandoffSelection {
        artifact_id,
        revision,
        path,
        file_sha256: digest.to_owned(),
        start,
        end,
        text: normalized[first..last].to_owned(),
    })
}

fn utf16_boundary(text: &str, target: u64) -> Option<usize> {
    let mut units = 0_u64;
    for (offset, character) in text.char_indices() {
        if units == target {
            return Some(offset);
        }
        units = units.checked_add(u64::try_from(character.len_utf16()).ok()?)?;
        if units > target {
            return None;
        }
    }
    (units == target).then_some(text.len())
}

/// Validates the user's separate edit request.
pub(crate) fn intent(value: &str) -> Result<String, HandoffError> {
    if value
        .trim_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
        .is_empty()
        || value.len() > MAX_INTENT_BYTES
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t' | '\r'))
    {
        return Err(HandoffError::Invalid(
            "intent must contain at most 4 KiB of text",
        ));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    use artifree_contracts::api::{HandoffSelection, HandoffState};
    use artifree_core::parts::{Boundary, ByteRange, part};
    use artifree_domain::identity::ActorId;

    use super::{HandoffError, Handoffs, hex, intent, selection};
    use crate::runtime_sessions::RuntimeSessions;

    fn fixture_selection() -> HandoffSelection {
        HandoffSelection {
            artifact_id: format!("art_{}", "1".repeat(32)),
            revision: 1,
            path: "index.html".into(),
            file_sha256: "a".repeat(64),
            start: 0,
            end: 4,
            text: "test".into(),
        }
    }

    #[test]
    fn staging_sending_and_cancelling_are_distinct() -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let other: ActorId = "other".parse()?;
        let connections = RuntimeSessions::new(Duration::from_secs(60));
        let connection_id = connections.create(&actor).map_err(|e| format!("{e:?}"))?;
        let connection = connections
            .get(&actor, &connection_id)
            .map_err(|e| format!("{e:?}"))?;
        let handoffs = Handoffs::new(Duration::from_secs(60));
        let agent = handoffs
            .register(&actor, "Editor")
            .map_err(|e| format!("{e:?}"))?;
        let other_agent = handoffs
            .register(&actor, "Other")
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(
            handoffs.list(&other).map_err(|e| format!("{e:?}"))?.len(),
            0
        );
        let staged = handoffs
            .stage(
                &actor,
                &connection_id,
                &connection,
                &agent.session_id,
                fixture_selection(),
                "Make it clearer".into(),
            )
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(staged.state, HandoffState::Staged);
        assert!(
            handoffs
                .poll(&actor, &agent.session_id, &agent.session_secret)
                .map_err(|e| format!("{e:?}"))?
                .is_empty()
        );
        assert!(matches!(
            handoffs.acknowledge(&actor, &agent.session_id, &agent.session_secret, &staged.id),
            Err(HandoffError::Conflict(_))
        ));
        assert!(matches!(
            handoffs.poll(&actor, &agent.session_id, &other_agent.session_secret),
            Err(HandoffError::Missing)
        ));
        assert_eq!(
            handoffs
                .send(&actor, &connection_id, &staged.id)
                .map_err(|e| format!("{e:?}"))?
                .state,
            HandoffState::Queued
        );
        assert_eq!(
            handoffs
                .send(&actor, &connection_id, &staged.id)
                .map_err(|e| format!("{e:?}"))?
                .state,
            HandoffState::Queued
        );
        assert!(
            handoffs
                .poll(&actor, &other_agent.session_id, &other_agent.session_secret)
                .map_err(|e| format!("{e:?}"))?
                .is_empty()
        );
        assert_eq!(
            handoffs
                .cancel(&actor, &connection_id, &staged.id)
                .map_err(|e| format!("{e:?}"))?
                .state,
            HandoffState::Cancelled
        );
        assert!(matches!(
            handoffs.send(&actor, &connection_id, &staged.id),
            Err(HandoffError::Conflict(_))
        ));
        Ok(())
    }

    #[test]
    fn offered_delivery_repeats_and_requires_its_session() -> Result<(), Box<dyn std::error::Error>>
    {
        let actor: ActorId = "local".parse()?;
        let connections = RuntimeSessions::new(Duration::from_secs(60));
        let connection_id = connections.create(&actor).map_err(|e| format!("{e:?}"))?;
        let connection = connections
            .get(&actor, &connection_id)
            .map_err(|e| format!("{e:?}"))?;
        let handoffs = Handoffs::new(Duration::from_secs(60));
        let agent = handoffs
            .register(&actor, "Editor")
            .map_err(|e| format!("{e:?}"))?;
        let other_agent = handoffs
            .register(&actor, "Other")
            .map_err(|e| format!("{e:?}"))?;
        let sent = handoffs
            .stage(
                &actor,
                &connection_id,
                &connection,
                &agent.session_id,
                fixture_selection(),
                "Edit".into(),
            )
            .map_err(|e| format!("{e:?}"))?;
        handoffs
            .send(&actor, &connection_id, &sent.id)
            .map_err(|e| format!("{e:?}"))?;
        let first = handoffs
            .poll(&actor, &agent.session_id, &agent.session_secret)
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].state, HandoffState::Offered);
        assert!(
            handoffs
                .confirm_offer(&actor, &agent.session_id, &agent.session_secret, &sent.id)
                .map_err(|e| format!("{e:?}"))?
        );
        assert_eq!(
            first,
            handoffs
                .poll(&actor, &agent.session_id, &agent.session_secret)
                .map_err(|e| format!("{e:?}"))?
        );
        assert!(matches!(
            handoffs.cancel(&actor, &connection_id, &sent.id),
            Err(HandoffError::Conflict(_))
        ));
        assert!(matches!(
            handoffs.acknowledge(
                &actor,
                &other_agent.session_id,
                &other_agent.session_secret,
                &sent.id
            ),
            Err(HandoffError::Missing)
        ));
        let receipt = handoffs
            .acknowledge(&actor, &agent.session_id, &agent.session_secret, &sent.id)
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(receipt.state, HandoffState::Acknowledged);
        assert!(
            !handoffs
                .confirm_offer(&actor, &agent.session_id, &agent.session_secret, &sent.id)
                .map_err(|e| format!("{e:?}"))?
        );
        assert_eq!(
            receipt,
            handoffs
                .acknowledge(&actor, &agent.session_id, &agent.session_secret, &sent.id)
                .map_err(|e| format!("{e:?}"))?
        );
        assert!(
            handoffs
                .poll(&actor, &agent.session_id, &agent.session_secret)
                .map_err(|e| format!("{e:?}"))?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn logout_fences_queued_but_cannot_recall_offered_delivery()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let connections = RuntimeSessions::new(Duration::from_secs(60));
        let id = connections.create(&actor).map_err(|e| format!("{e:?}"))?;
        let connection = connections.get(&actor, &id).map_err(|e| format!("{e:?}"))?;
        let handoffs = Handoffs::new(Duration::from_secs(60));
        let agent = handoffs
            .register(&actor, "Editor")
            .map_err(|e| format!("{e:?}"))?;
        let offered = handoffs
            .stage(
                &actor,
                &id,
                &connection,
                &agent.session_id,
                fixture_selection(),
                "Edit".into(),
            )
            .map_err(|e| format!("{e:?}"))?;
        handoffs
            .send(&actor, &id, &offered.id)
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(
            handoffs
                .poll(&actor, &agent.session_id, &agent.session_secret)
                .map_err(|e| format!("{e:?}"))?[0]
                .state,
            HandoffState::Offered
        );
        let queued = handoffs
            .stage(
                &actor,
                &id,
                &connection,
                &agent.session_id,
                fixture_selection(),
                "Another".into(),
            )
            .map_err(|e| format!("{e:?}"))?;
        handoffs
            .send(&actor, &id, &queued.id)
            .map_err(|e| format!("{e:?}"))?;
        connections
            .close(&actor, &id)
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(
            handoffs
                .get(&actor, &id, &queued.id)
                .map_err(|e| format!("{e:?}"))?
                .state,
            HandoffState::Expired,
            "status must not depend on an agent polling after logout"
        );
        assert!(
            handoffs
                .poll(&actor, &agent.session_id, &agent.session_secret)
                .map_err(|e| format!("{e:?}"))?
                .is_empty()
        );
        handoffs
            .acknowledge(
                &actor,
                &agent.session_id,
                &agent.session_secret,
                &offered.id,
            )
            .map_err(|e| format!("{e:?}"))?;
        assert!(
            handoffs
                .poll(&actor, &agent.session_id, &agent.session_secret)
                .map_err(|e| format!("{e:?}"))?
                .is_empty()
        );
        assert_eq!(
            handoffs
                .get(&actor, &id, &queued.id)
                .map_err(|e| format!("{e:?}"))?
                .state,
            HandoffState::Expired
        );
        assert!(matches!(
            handoffs.send(&actor, &id, &queued.id),
            Err(HandoffError::ConnectionClosed)
        ));
        assert!(matches!(
            handoffs.stage(
                &actor,
                &id,
                &connection,
                &agent.session_id,
                fixture_selection(),
                "late".into()
            ),
            Err(HandoffError::ConnectionClosed)
        ));
        Ok(())
    }

    #[test]
    fn selection_checks_digest_utf16_boundaries_and_editor_line_endings()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = "\u{feff}a😀\r\nb".as_bytes().to_vec();
        let file = part(bytes, ByteRange::WHOLE, Boundary::Byte)?;
        let digest = hex(&file.sha256);
        let selected = selection(
            "art".into(),
            1,
            "index.html".into(),
            &digest,
            2,
            5,
            file.clone(),
        )
        .map_err(|e| format!("{e:?}"))?;
        assert_eq!(selected.text, "😀\n");
        assert!(matches!(
            selection(
                "art".into(),
                1,
                "index.html".into(),
                &digest,
                3,
                5,
                file.clone()
            ),
            Err(HandoffError::Invalid(_))
        ));
        assert!(matches!(
            selection(
                "art".into(),
                1,
                "index.html".into(),
                &"0".repeat(64),
                2,
                5,
                file
            ),
            Err(HandoffError::Conflict(_))
        ));
        let mixed = part(b"a\r\nb\nc".to_vec(), ByteRange::WHOLE, Boundary::Byte)?;
        let mixed_digest = hex(&mixed.sha256);
        assert!(matches!(
            selection(
                "art".into(),
                1,
                "index.html".into(),
                &mixed_digest,
                0,
                1,
                mixed
            ),
            Err(HandoffError::Invalid(_))
        ));
        assert!(intent("   ").is_err());
        assert!(intent("\u{feff} \u{feff}").is_err());
        assert!(intent(&"x".repeat(4097)).is_err());
        let oversized = part(vec![b'x'; 16_385], ByteRange::WHOLE, Boundary::Byte)?;
        let oversized_digest = hex(&oversized.sha256);
        assert!(matches!(
            selection(
                "art".into(),
                1,
                "index.html".into(),
                &oversized_digest,
                0,
                16_385,
                oversized
            ),
            Err(HandoffError::Invalid(_))
        ));
        assert_eq!(
            intent("  Clarify  ").map_err(|e| format!("{e:?}"))?,
            "  Clarify  "
        );
        assert_eq!(
            intent("\u{feff}Clarify\u{feff}").map_err(|e| format!("{e:?}"))?,
            "\u{feff}Clarify\u{feff}"
        );
        Ok(())
    }

    #[test]
    fn agent_and_workspace_identities_do_not_cross_actors_or_expiry()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let other: ActorId = "other".parse()?;
        let connections = RuntimeSessions::new(Duration::from_secs(60));
        let id = connections.create(&actor).map_err(|e| format!("{e:?}"))?;
        let connection = connections.get(&actor, &id).map_err(|e| format!("{e:?}"))?;
        let registry = Handoffs::new(Duration::from_secs(60));
        let agent = registry
            .register(&actor, "Editor")
            .map_err(|e| format!("{e:?}"))?;
        assert!(matches!(
            registry.poll(&other, &agent.session_id, &agent.session_secret),
            Err(HandoffError::Missing)
        ));
        let staged = registry
            .stage(
                &actor,
                &id,
                &connection,
                &agent.session_id,
                fixture_selection(),
                "Edit".into(),
            )
            .map_err(|e| format!("{e:?}"))?;
        assert!(matches!(
            registry.get(&other, &id, &staged.id),
            Err(HandoffError::Missing)
        ));
        assert!(matches!(
            registry.get(&actor, "other-workspace", &staged.id),
            Err(HandoffError::Missing)
        ));
        assert!(matches!(
            registry.send(&other, &id, &staged.id),
            Err(HandoffError::Missing)
        ));
        let expired = Handoffs::new(Duration::ZERO);
        let dead = expired
            .register(&actor, "Editor")
            .map_err(|e| format!("{e:?}"))?;
        assert!(matches!(
            expired.poll(&actor, &dead.session_id, &dead.session_secret),
            Err(HandoffError::Missing)
        ));
        Ok(())
    }

    #[test]
    fn expired_agent_is_visible_without_waiting_for_a_poll()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let connections = RuntimeSessions::new(Duration::from_secs(60));
        let id = connections.create(&actor).map_err(|e| format!("{e:?}"))?;
        let connection = connections.get(&actor, &id).map_err(|e| format!("{e:?}"))?;
        let handoffs = Handoffs::new(Duration::from_secs(60));
        let agent = handoffs
            .register(&actor, "Editor")
            .map_err(|e| format!("{e:?}"))?;
        let staged = handoffs
            .stage(
                &actor,
                &id,
                &connection,
                &agent.session_id,
                fixture_selection(),
                "Edit".into(),
            )
            .map_err(|e| format!("{e:?}"))?;
        let queued = handoffs
            .stage(
                &actor,
                &id,
                &connection,
                &agent.session_id,
                fixture_selection(),
                "Edit".into(),
            )
            .map_err(|e| format!("{e:?}"))?;
        handoffs
            .send(&actor, &id, &queued.id)
            .map_err(|e| format!("{e:?}"))?;
        let mut registry = handoffs.lock().map_err(|e| format!("{e:?}"))?;
        registry
            .agents
            .get_mut(&agent.session_id)
            .ok_or("missing agent")?
            .expires = Instant::now();
        drop(registry);
        for handoff in [staged, queued] {
            assert_eq!(
                handoffs
                    .get(&actor, &id, &handoff.id)
                    .map_err(|e| format!("{e:?}"))?
                    .state,
                HandoffState::Expired
            );
        }
        assert!(matches!(
            handoffs.poll(&actor, &agent.session_id, &agent.session_secret),
            Err(HandoffError::Missing)
        ));
        Ok(())
    }

    #[test]
    fn logout_and_poll_serialize_before_any_further_offer() -> Result<(), Box<dyn std::error::Error>>
    {
        let actor: ActorId = "local".parse()?;
        let connections = Arc::new(RuntimeSessions::new(Duration::from_secs(60)));
        let id = connections.create(&actor).map_err(|e| format!("{e:?}"))?;
        let connection = connections.get(&actor, &id).map_err(|e| format!("{e:?}"))?;
        let registry = Arc::new(Handoffs::new(Duration::from_secs(60)));
        let agent = registry
            .register(&actor, "Editor")
            .map_err(|e| format!("{e:?}"))?;
        let staged = registry
            .stage(
                &actor,
                &id,
                &connection,
                &agent.session_id,
                fixture_selection(),
                "Edit".into(),
            )
            .map_err(|e| format!("{e:?}"))?;
        registry
            .send(&actor, &id, &staged.id)
            .map_err(|e| format!("{e:?}"))?;
        let barrier = Arc::new(Barrier::new(3));
        let poll = {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            let actor = actor.clone();
            let agent = agent.clone();
            std::thread::spawn(move || {
                barrier.wait();
                registry.poll(&actor, &agent.session_id, &agent.session_secret)
            })
        };
        let close = {
            let connections = Arc::clone(&connections);
            let barrier = Arc::clone(&barrier);
            let actor = actor.clone();
            let id = id.clone();
            std::thread::spawn(move || {
                barrier.wait();
                connections.close(&actor, &id)
            })
        };
        barrier.wait();
        let result = poll
            .join()
            .map_err(|_| "poll thread panicked")?
            .map_err(|e| format!("{e:?}"))?;
        close
            .join()
            .map_err(|_| "close thread panicked")?
            .map_err(|e| format!("{e:?}"))?;
        assert!(
            registry
                .poll(&actor, &agent.session_id, &agent.session_secret)
                .map_err(|e| format!("{e:?}"))?
                .is_empty()
        );
        if result.is_empty() {
            assert_eq!(
                registry
                    .get(&actor, &id, &staged.id)
                    .map_err(|e| format!("{e:?}"))?
                    .state,
                HandoffState::Expired
            );
        } else {
            assert_eq!(result[0].state, HandoffState::Offered);
            assert_eq!(
                registry
                    .acknowledge(&actor, &agent.session_id, &agent.session_secret, &staged.id)
                    .map_err(|e| format!("{e:?}"))?
                    .state,
                HandoffState::Acknowledged
            );
        }
        Ok(())
    }

    #[test]
    fn cancel_and_poll_choose_one_atomic_outcome() -> Result<(), Box<dyn std::error::Error>> {
        let actor: ActorId = "local".parse()?;
        let connections = RuntimeSessions::new(Duration::from_secs(60));
        let id = connections.create(&actor).map_err(|e| format!("{e:?}"))?;
        let connection = connections.get(&actor, &id).map_err(|e| format!("{e:?}"))?;
        let registry = Arc::new(Handoffs::new(Duration::from_secs(60)));
        let agent = registry
            .register(&actor, "Editor")
            .map_err(|e| format!("{e:?}"))?;
        let staged = registry
            .stage(
                &actor,
                &id,
                &connection,
                &agent.session_id,
                fixture_selection(),
                "Edit".into(),
            )
            .map_err(|e| format!("{e:?}"))?;
        registry
            .send(&actor, &id, &staged.id)
            .map_err(|e| format!("{e:?}"))?;
        let barrier = Arc::new(Barrier::new(3));
        let poll = {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            let actor = actor.clone();
            let agent = agent.clone();
            std::thread::spawn(move || {
                barrier.wait();
                registry.poll(&actor, &agent.session_id, &agent.session_secret)
            })
        };
        let cancel = {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            let actor = actor.clone();
            let id = id.clone();
            let handoff_id = staged.id.clone();
            std::thread::spawn(move || {
                barrier.wait();
                registry.cancel(&actor, &id, &handoff_id)
            })
        };
        barrier.wait();
        let delivered = poll
            .join()
            .map_err(|_| "poll thread panicked")?
            .map_err(|e| format!("{e:?}"))?;
        let cancelled = cancel.join().map_err(|_| "cancel thread panicked")?;
        if delivered.is_empty() {
            assert_eq!(
                cancelled.map_err(|e| format!("{e:?}"))?.state,
                HandoffState::Cancelled
            );
        } else {
            assert_eq!(delivered[0].state, HandoffState::Offered);
            assert!(matches!(cancelled, Err(HandoffError::Conflict(_))));
        }
        Ok(())
    }
}
