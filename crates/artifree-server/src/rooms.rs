//! Rooms and presence over HTTP (decision 0034).
//!
//! The pure room lives in `artifree-core`. This module gives it a monotonic
//! clock, one lock, and the checks that need the server: the caller's tab is
//! a runtime connection the server issued to this actor and has not closed,
//! the artifact is the actor's and outside the trash, and the answer is the
//! wire contract. A room is memory of this process. Nothing is written to a
//! database, and a restart empties every room.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use artifree_contracts::api::{
    ErrorCode, RoomEvent, RoomEventReceipt, RoomEventRequest, RoomHeartbeatRequest,
    RoomJoinRequest, RoomMember, RoomSnapshot,
};
use artifree_core::room::{Caller, Limits, RoomError, Rooms, Snapshot, TabKey};
use artifree_domain::identity::{ActorId, ArtifactId, InvalidValue};
use artifree_domain::room::{
    Epoch, EventData, EventKind, EventSeq, ParticipantId, PresenceState, Slot,
};
use axum::http::StatusCode;

use crate::http::AppState;
use crate::operations::{ApiError, artifact_id, session_error, validate_view};
use crate::runtime_sessions::RuntimeSession;

/// The largest number a client may name, so that it is a safe integer.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Where a room's time comes from: a monotonic reading that starts anywhere
/// and never goes back. Leases are measured on it, never on the wall clock,
/// and tests move it without waiting.
pub trait RoomClock: std::fmt::Debug + Send + Sync {
    /// Time since an arbitrary start, which never decreases.
    fn elapsed(&self) -> Duration;
}

/// The operating system's monotonic clock, started when it was made.
#[derive(Debug)]
pub struct ProcessClock(Instant);

impl ProcessClock {
    /// A clock that reads zero now.
    #[must_use]
    pub fn new() -> Self {
        Self(Instant::now())
    }
}

impl Default for ProcessClock {
    fn default() -> Self {
        Self::new()
    }
}

impl RoomClock for ProcessClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Every room of this process behind one lock. A call reads the clock while
/// it holds the lock, so two callers never present times out of order, and
/// it does no I/O while it holds it.
#[derive(Debug)]
pub(crate) struct RoomHub {
    rooms: Mutex<Rooms>,
    clock: Arc<dyn RoomClock>,
}

impl RoomHub {
    pub(crate) fn new(clock: Arc<dyn RoomClock>) -> Self {
        Self {
            rooms: Mutex::new(Rooms::new(Limits::STANDARD)),
            clock,
        }
    }

    fn with<T>(
        &self,
        action: impl FnOnce(&mut Rooms, Duration) -> Result<T, RoomError>,
    ) -> Result<T, ApiError> {
        let mut rooms = self.rooms.lock().map_err(|_| unavailable())?;
        let now = self.clock.elapsed();
        action(&mut rooms, now).map_err(room_error)
    }

    /// Removes the tab's participants after its connection was closed.
    pub(crate) fn leave_tab(&self, actor: &ActorId, tab: &TabKey) {
        // A poisoned lock fails every later call closed; there is nothing
        // to clean up for them.
        if let Ok(mut rooms) = self.rooms.lock() {
            rooms.leave_tab(actor, tab);
        }
    }
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        ErrorCode::Unavailable,
        "rooms are temporarily unavailable",
    )
}

fn room_error(error: RoomError) -> ApiError {
    match error {
        RoomError::NotFound => ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            error.to_string(),
        ),
        RoomError::RoomFull
        | RoomError::TabFull
        | RoomError::TooManyRooms
        | RoomError::RateLimited { .. } => ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::RateLimited,
            error.to_string(),
        ),
        RoomError::CursorAhead => ApiError::invalid(error.to_string()),
        RoomError::Exhausted => unavailable(),
    }
}

/// A number from a route: ASCII digits only, no sign, no leading zero, from
/// 1 to the largest safe integer.
fn counter(text: &str, what: &str) -> Result<u64, ApiError> {
    let digits = !text.is_empty()
        && !text.starts_with('0')
        && text.bytes().all(|byte| byte.is_ascii_digit());
    digits
        .then(|| text.parse::<u64>().ok())
        .flatten()
        .filter(|value| *value <= MAX_SAFE_INTEGER)
        .ok_or_else(|| ApiError::invalid(format!("{what} must be a positive safe integer")))
}

fn text<T: std::str::FromStr<Err = InvalidValue>>(field: &str, value: &str) -> Result<T, ApiError> {
    value
        .parse()
        .map_err(|error| ApiError::invalid(format!("{field}: {error}")))
}

fn state_of(value: Option<&str>) -> Result<Option<PresenceState>, ApiError> {
    value.map(|value| text("state", value)).transpose()
}

/// The checks every call that reads or writes a room shares, in this order:
/// the shape of the route names (`400`), the caller's tab as a live runtime
/// connection of this actor (`404` `runtime_session_expired`), and, when the
/// call needs it, the artifact outside the trash (`404`). Arguments in the
/// body are judged after, so a caller without a live tab learns nothing from
/// how they were judged.
struct Located {
    session: Arc<RuntimeSession>,
    tab: TabKey,
    artifact: ArtifactId,
}

impl Located {
    /// Changes a room while the tab's connection cannot be closed: a close
    /// that comes first is seen here, and one that comes after removes what
    /// this left, so no participant outlives a closed connection.
    fn act<T>(
        &self,
        state: &AppState,
        actor: &ActorId,
        action: impl FnOnce(&mut Rooms, Duration, &Caller<'_>) -> Result<T, RoomError>,
    ) -> Result<T, ApiError> {
        self.session
            .while_active(|| {
                state.rooms().with(|rooms, now| {
                    let caller = Caller {
                        actor,
                        artifact: &self.artifact,
                        tab: &self.tab,
                    };
                    action(rooms, now, &caller)
                })
            })
            .map_err(session_error)?
    }
}

async fn locate(
    state: &AppState,
    actor: &ActorId,
    session_id: &str,
    artifact: &str,
    live: bool,
) -> Result<Located, ApiError> {
    validate_view(session_id, 1)?;
    let artifact = artifact_id(artifact)?;
    let session = state
        .runtime_sessions()
        .get(actor, session_id)
        .map_err(session_error)?;
    if live {
        let (owner, id) = (actor.clone(), artifact.clone());
        state
            .run(move |artifacts| artifacts.artifact(&owner, &id))
            .await?;
    }
    Ok(Located {
        session,
        tab: TabKey::new(session_id),
        artifact,
    })
}

fn snapshot(view: Snapshot) -> RoomSnapshot {
    RoomSnapshot {
        participant: view.participant.0,
        epoch: view.epoch.0,
        tab: view.tab.0,
        lease_ms: u64::try_from(view.lease.as_millis()).unwrap_or(u64::MAX),
        members: view
            .members
            .into_iter()
            .map(|member| RoomMember {
                participant: member.participant.0,
                tab: member.tab.0,
                actor: member.actor.to_string(),
                is_me: member.is_me,
                same_tab: member.same_tab,
                state: member.state.to_string(),
            })
            .collect(),
        events: view
            .events
            .into_iter()
            .map(|event| RoomEvent {
                seq: event.seq.0,
                participant: event.participant.0,
                tab: event.tab.0,
                is_me: event.is_me,
                same_tab: event.same_tab,
                kind: event.kind.as_str().to_owned(),
                data: event.data.to_string(),
            })
            .collect(),
        head: view.head.0,
        missed: view.missed,
    }
}

/// Joins the room of `artifact` as a participant of the tab `session_id`, or
/// resumes the one the tab has in the request's slot.
pub(crate) async fn join(
    state: &AppState,
    actor: &ActorId,
    session_id: &str,
    artifact: &str,
    body: Result<RoomJoinRequest, ApiError>,
) -> Result<RoomSnapshot, ApiError> {
    let located = locate(state, actor, session_id, artifact, true).await?;
    let request = body?;
    let slot: Slot = text("slot", &request.slot)?;
    let presence = state_of(request.state.as_deref())?;
    located
        .act(state, actor, |rooms, now, caller| {
            rooms.join(now, caller, slot, presence)
        })
        .map(snapshot)
}

/// Renews a participant's lease and reads the room.
pub(crate) async fn heartbeat(
    state: &AppState,
    actor: &ActorId,
    names: RoomNames<'_>,
    body: Result<RoomHeartbeatRequest, ApiError>,
) -> Result<RoomSnapshot, ApiError> {
    let (participant, epoch) = names.numbers()?;
    let located = locate(state, actor, names.session, names.artifact, true).await?;
    let request = body?;
    let presence = state_of(request.state.as_deref())?;
    located
        .act(state, actor, |rooms, now, caller| {
            rooms.heartbeat(
                now,
                caller,
                participant,
                epoch,
                EventSeq(request.after),
                presence,
            )
        })
        .map(snapshot)
}

/// Sends one ephemeral event.
pub(crate) async fn send(
    state: &AppState,
    actor: &ActorId,
    names: RoomNames<'_>,
    body: Result<RoomEventRequest, ApiError>,
) -> Result<RoomEventReceipt, ApiError> {
    let (participant, epoch) = names.numbers()?;
    let located = locate(state, actor, names.session, names.artifact, true).await?;
    let request = body?;
    let kind: EventKind = text("kind", &request.kind)?;
    let data: EventData = text("data", &request.data)?;
    located
        .act(state, actor, |rooms, now, caller| {
            rooms.send(now, caller, participant, epoch, kind, data)
        })
        .map(|seq| RoomEventReceipt { seq: seq.0 })
}

/// Removes a participant. It needs the tab's connection but not the
/// artifact, so a tab can leave a room whose artifact was trashed.
pub(crate) async fn leave(
    state: &AppState,
    actor: &ActorId,
    names: RoomNames<'_>,
) -> Result<(), ApiError> {
    let (participant, epoch) = names.numbers()?;
    let located = locate(state, actor, names.session, names.artifact, false).await?;
    located.act(state, actor, |rooms, now, caller| {
        rooms.leave(now, caller, participant, epoch)
    })
}

/// The names a participant's route carries, before they are judged.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RoomNames<'route> {
    pub(crate) session: &'route str,
    pub(crate) artifact: &'route str,
    pub(crate) participant: &'route str,
    pub(crate) epoch: &'route str,
}

impl RoomNames<'_> {
    fn numbers(&self) -> Result<(ParticipantId, Epoch), ApiError> {
        Ok((
            ParticipantId(counter(self.participant, "participant")?),
            Epoch(counter(self.epoch, "epoch")?),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_numbers_are_plain_positive_safe_integers() {
        for good in ["1", "42", "9007199254740991"] {
            assert!(counter(good, "n").is_ok(), "{good}");
        }
        for bad in [
            "",
            "0",
            "01",
            "+1",
            "-1",
            "1.0",
            "1e3",
            " 1",
            "1 ",
            "٣",
            "9007199254740992",
            "99999999999999999",
            "18446744073709551616",
        ] {
            assert!(counter(bad, "n").is_err(), "{bad:?}");
        }
    }
}
