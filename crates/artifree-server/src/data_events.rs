//! `GET /v1/artifacts/{id}/data/events`: the value at a path of the caller's
//! data space, then every committed change to it, as server-sent events
//! (decision 0038).
//!
//! A stream opens with a snapshot (or replays from a data cursor), and
//! follows the retained window of the space ([`crate::data_hub`]). Each
//! stream is produced by its own task into a channel of one event, so a slow
//! reader holds back only its own producer, and a reader that falls behind the
//! window is reset instead of queued.

use std::convert::Infallible;
use std::sync::Arc;

use artifree_contracts::api::{
    DataChangeEvent, DataPosition, DataSnapshotEvent, ErrorCode, JsonValue, ReadDataEventsQuery,
};
use artifree_domain::AppDataVersion;
use artifree_domain::change::{ChangeSequence, FeedId};
use artifree_domain::data::{DataPath, DataValue};
use artifree_domain::data_feed::{DataCursor, DataEdit, Seen};
use artifree_domain::identity::{ActorId, ArtifactId};
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use tokio::sync::{mpsc, watch};

use crate::auth::AuthContext;
use crate::changes::{
    Events, HEARTBEAT, Stopped, TokenStreamRegistration, event, too_many_streams,
    wait_for_revocation,
};
use crate::data_hub::{Replay, Space, SpaceError};
use crate::http::AppState;
use crate::limits::{Refusal, Slot};
use crate::operations::{self, ApiError};

/// Data streams one actor may keep open. One more is `429`.
pub(crate) const MAX_STREAMS_PER_ACTOR: u32 = 8;

/// What a stream sends, before it is written as an event.
#[derive(Debug, Clone, PartialEq)]
enum Outgoing {
    /// `snapshot` or `reset`: the value at the watched path.
    Snapshot {
        name: &'static str,
        version: AppDataVersion,
        value: Option<DataValue>,
    },
    /// `change`: the effect of one write on the watched path.
    Change { version: AppDataVersion, seen: Seen },
    /// `position`: everything up to this version was sent.
    Position { version: AppDataVersion },
}

impl Outgoing {
    fn version(&self) -> AppDataVersion {
        match self {
            Self::Snapshot { version, .. }
            | Self::Change { version, .. }
            | Self::Position { version } => *version,
        }
    }
}

/// What to send for the writes `edits`, which follow `since`, the last
/// position the reader has, up to `head`: each write that touches `watched`,
/// then a `position` when the last of them does not already reach `head`.
/// With `acknowledge`, a resumption that sent nothing is still told where it
/// stands.
fn outgoing(
    edits: &[Arc<DataEdit>],
    watched: &DataPath,
    since: AppDataVersion,
    head: AppDataVersion,
    acknowledge: bool,
) -> Vec<Outgoing> {
    let mut items = Vec::new();
    let mut reached = since;
    for edit in edits {
        if let Some(seen) = edit.seen_from(watched) {
            items.push(Outgoing::Change {
                version: edit.version,
                seen,
            });
            reached = edit.version;
        }
    }
    if head > reached || (acknowledge && items.is_empty()) {
        items.push(Outgoing::Position { version: head });
    }
    items
}

/// Opens a data stream. Without `Last-Event-ID` it starts with `snapshot`;
/// with a data cursor it replays the writes after it, or sends `reset`.
pub(crate) async fn stream(
    State(state): State<AppState>,
    auth: AuthContext,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<ReadDataEventsQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let Path(id) = id?;
    let artifact = operations::artifact_id(&id)?;
    let Query(ReadDataEventsQuery { path }) = query?;
    let watched = operations::data_path("path", path.as_deref().unwrap_or_default())?;
    let resume = last_event_id(&headers)?;
    // Access first: a stream for an artifact that is missing or not this
    // actor's takes no place and makes no window (decision 0038).
    let (reader, id) = (auth.actor.clone(), artifact.clone());
    state
        .run(move |artifacts| artifacts.check_data_access(&reader, &id))
        .await?;
    let slot = match state.data_streams().take(&auth.actor) {
        Ok(slot) => slot,
        Err(Refusal::Unavailable) => {
            return Err(ApiError::unavailable("stream limits are unavailable"));
        }
        Err(_) => {
            return Ok(too_many_streams(
                "too many data streams are open; close one or retry after the given delay",
            ));
        }
    };
    let space = match state.data_hub().space(&auth.actor, &artifact) {
        Ok(space) => space,
        Err(SpaceError::Full) => {
            return Ok(too_many_streams(
                "too many data subscriptions are open on this server; retry after the given delay",
            ));
        }
        Err(error @ SpaceError::Poisoned) => {
            return Err(ApiError::unavailable(error.to_string()));
        }
    };
    // Subscribe before the first read, so a write after it wakes the stream.
    let writes = space.watch();
    let commits = state.feed().commits();
    let closing = state.feed().closing();
    let (revoked, registration) = if let Some(token_id) = &auth.token_id {
        let (tx, rx) = watch::channel(false);
        let registration = state
            .feed()
            .register_token_stream(&auth.actor, token_id, tx)
            // An unusable registry could not end this stream on a revocation.
            .ok_or_else(|| ApiError::unavailable("stream registry is unavailable"))?;
        (Some(rx), Some(registration))
    } else {
        (None, None)
    };
    // The registration above is what a disable ends; confirming the user
    // after it closes the window in which a request that authenticated
    // earlier opens a stream the disable has already passed.
    state.ensure_current(&auth.actor, auth.generation).await?;
    let opening = begin(&state, &space, &auth.actor, &artifact, &watched, resume).await?;
    let (sender, receiver) = mpsc::channel(1);
    let producer = Producer {
        state,
        actor: auth.actor,
        artifact,
        watched,
        feed: opening.feed,
        space,
        position: opening.position,
        sender,
        writes,
        commits,
        closing,
        revoked,
        _slot: slot,
        _registration: registration,
    };
    tokio::spawn(producer.run(opening.first));
    Ok(Sse::new(Events::new(receiver))
        .keep_alive(KeepAlive::new().interval(HEARTBEAT))
        .into_response())
}

/// The data cursor in `Last-Event-ID`, if any.
fn last_event_id(headers: &HeaderMap) -> Result<Option<DataCursor>, ApiError> {
    let Some(value) = headers.get("last-event-id") else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .and_then(|text| text.parse().ok())
        .map(Some)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidRequest,
                "Last-Event-ID must be a data cursor received from a data stream",
            )
        })
}

/// The first events of a stream, and where the reader stands after them.
struct Opening {
    feed: FeedId,
    first: Vec<Outgoing>,
    position: AppDataVersion,
}

/// Reads the value and anchors the window under the space's gate, and decides
/// how the stream starts.
async fn begin(
    state: &AppState,
    space: &Space,
    actor: &ActorId,
    artifact: &ArtifactId,
    watched: &DataPath,
    resume: Option<DataCursor>,
) -> Result<Opening, ApiError> {
    let (feed, read, replay) = {
        let _gate = space.gate().lock().await;
        let (reader, id, path) = (actor.clone(), artifact.clone(), watched.clone());
        let (feed, read) = state
            .run(move |artifacts| {
                let feed = artifacts.changes(&reader, ChangeSequence(0), 0)?.feed;
                Ok((feed, artifacts.data(&reader, &id, &path)?))
            })
            .await?;
        let replay = space
            .with_window(|window| {
                window.anchor(read.version);
                resume
                    .filter(|cursor| cursor.feed == feed && cursor.version <= read.version)
                    .map(|cursor| window.after(cursor.version))
            })
            .map_err(|error| ApiError::unavailable(error.to_string()))?;
        (feed, read, replay)
    };
    let snapshot = |name| Outgoing::Snapshot {
        name,
        version: read.version,
        value: read.value.clone(),
    };
    let (first, position) = match (resume, replay) {
        (None, _) => (vec![snapshot("snapshot")], read.version),
        (Some(cursor), Some(Replay::Edits { edits, head })) => (
            outgoing(&edits, watched, cursor.version, head, true),
            cursor.version,
        ),
        // A cursor of another database, ahead of the data, or older than the
        // window.
        (Some(_), _) => (vec![snapshot("reset")], read.version),
    };
    Ok(Opening {
        feed,
        first,
        position,
    })
}

/// Why the next step happens.
enum Wake {
    /// A write was recorded in the window.
    Write,
    /// A commit in this process, or the heartbeat: read storage to see that
    /// the artifact is still there and the window agrees with it.
    Check,
}

/// Reads one actor's window and sends it to one stream.
struct Producer {
    state: AppState,
    actor: ActorId,
    artifact: ArtifactId,
    watched: DataPath,
    feed: FeedId,
    space: Arc<Space>,
    /// The last version the reader has been sent.
    position: AppDataVersion,
    sender: mpsc::Sender<Result<Event, Infallible>>,
    writes: watch::Receiver<()>,
    commits: watch::Receiver<()>,
    closing: watch::Receiver<bool>,
    revoked: Option<watch::Receiver<bool>>,
    /// Held until the producer ends.
    _slot: Slot,
    _registration: Option<TokenStreamRegistration>,
}

impl Producer {
    async fn run(mut self, first: Vec<Outgoing>) {
        let _stopped: Result<(), Stopped> = async {
            self.send_all(first).await?;
            loop {
                match self.wait().await? {
                    Wake::Write => self.advance(false).await?,
                    Wake::Check => self.advance(true).await?,
                }
            }
        }
        .await;
    }

    /// Sends what the window holds after the position. With `verify`, or when
    /// the window cannot continue the position, storage is read under the
    /// gate first: the window is anchored to it, and a position it cannot
    /// continue is reset.
    async fn advance(&mut self, verify: bool) -> Result<(), Stopped> {
        if !verify {
            let position = self.position;
            let replay = self
                .space
                .with_window(|window| window.after(position))
                .map_err(|_| Stopped)?;
            if let Replay::Edits { edits, head } = replay {
                let items = outgoing(&edits, &self.watched, position, head, false);
                return self.send_all(items).await;
            }
        }
        // The gate is released before anything is sent: a slow reader must
        // not hold up the writers of the space.
        let items = {
            let _gate = self.space.gate().lock().await;
            let (reader, id, path) = (
                self.actor.clone(),
                self.artifact.clone(),
                self.watched.clone(),
            );
            // The artifact gone (trash, purge) or storage failing ends the
            // stream; the reader opens a new one and is told what is true.
            let read = self
                .state
                .run(move |artifacts| artifacts.data(&reader, &id, &path))
                .await
                .map_err(|_| Stopped)?;
            let position = self.position;
            let replay = self
                .space
                .with_window(|window| {
                    window.anchor(read.version);
                    window.after(position)
                })
                .map_err(|_| Stopped)?;
            match replay {
                Replay::Edits { edits, head } => {
                    outgoing(&edits, &self.watched, position, head, false)
                }
                Replay::Gap => vec![Outgoing::Snapshot {
                    name: "reset",
                    version: read.version,
                    value: read.value,
                }],
            }
        };
        self.send_all(items).await
    }

    /// Waits for a write in the window, a commit in this process, or the
    /// heartbeat.
    async fn wait(&mut self) -> Result<Wake, Stopped> {
        tokio::select! {
            biased;
            _ = self.closing.wait_for(|closing| *closing) => Err(Stopped),
            () = wait_for_revocation(&mut self.revoked) => Err(Stopped),
            () = self.sender.closed() => Err(Stopped),
            changed = self.writes.changed() => changed.map(|()| Wake::Write).map_err(|_| Stopped),
            changed = self.commits.changed() => changed.map(|()| Wake::Check).map_err(|_| Stopped),
            () = tokio::time::sleep(HEARTBEAT) => Ok(Wake::Check),
        }
    }

    async fn send_all(&mut self, items: Vec<Outgoing>) -> Result<(), Stopped> {
        for item in items {
            let event = self.event(&item)?;
            self.send(event).await?;
            self.position = item.version();
        }
        Ok(())
    }

    /// Sends one event, waiting while the reader is behind.
    async fn send(&mut self, event: Event) -> Result<(), Stopped> {
        tokio::select! {
            biased;
            _ = self.closing.wait_for(|closing| *closing) => Err(Stopped),
            () = wait_for_revocation(&mut self.revoked) => Err(Stopped),
            sent = self.sender.send(Ok(event)) => sent.map_err(|_| Stopped),
        }
    }

    fn event(&self, item: &Outgoing) -> Result<Event, Stopped> {
        let cursor = DataCursor {
            feed: self.feed,
            version: item.version(),
        }
        .to_string();
        let path = self.watched.to_string();
        match item {
            Outgoing::Snapshot {
                name,
                version,
                value,
            } => event(
                name,
                &cursor,
                &DataSnapshotEvent {
                    cursor: cursor.clone(),
                    data_version: version.0,
                    path,
                    exists: value.is_some(),
                    value: json(value.as_ref())?,
                },
            ),
            Outgoing::Change { version, seen } => event(
                "change",
                &cursor,
                &DataChangeEvent {
                    cursor: cursor.clone(),
                    data_version: version.0,
                    path: seen.path.to_string(),
                    exists: seen.value.is_some(),
                    value: json(seen.value.as_ref())?,
                },
            ),
            Outgoing::Position { version } => event(
                "position",
                &cursor,
                &DataPosition {
                    cursor: cursor.clone(),
                    data_version: version.0,
                },
            ),
        }
    }
}

/// The wire form of a value, in the canonical text the space stores. A value
/// that cannot be stated stops the stream: an event that says a path exists
/// and carries no value would be wrong, and the client's reconnect is reset.
fn json(value: Option<&DataValue>) -> Result<Option<JsonValue>, Stopped> {
    value.map(|value| wire(value.to_canonical())).transpose()
}

fn wire(text: String) -> Result<JsonValue, Stopped> {
    JsonValue::from_text(text).ok_or(Stopped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> DataPath {
        text.parse()
            .unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn edit(version: u64, at: &str, value: Option<i64>) -> Arc<DataEdit> {
        Arc::new(DataEdit {
            version: AppDataVersion(version),
            path: path(at),
            value: value.and_then(|number| {
                artifree_domain::data::DataNumber::from_i64(number).map(DataValue::Number)
            }),
        })
    }

    fn kinds(items: &[Outgoing]) -> Vec<(&'static str, u64)> {
        items
            .iter()
            .map(|item| match item {
                Outgoing::Snapshot { name, version, .. } => (*name, version.0),
                Outgoing::Change { version, .. } => ("change", version.0),
                Outgoing::Position { version } => ("position", version.0),
            })
            .collect()
    }

    #[test]
    fn a_value_that_is_not_json_stops_the_stream_instead_of_being_left_out() {
        assert!(matches!(json(Some(&DataValue::Null)), Ok(Some(_))));
        assert!(matches!(json(None), Ok(None)));
        assert!(wire("{".into()).is_err());
    }

    #[test]
    fn only_writes_that_touch_the_path_are_sent_and_a_position_closes_the_run() {
        let edits = [
            edit(5, "todos/a", Some(1)),
            edit(6, "settings", Some(2)),
            edit(7, "todos/b", None),
            edit(8, "settings", Some(3)),
        ];
        let items = outgoing(
            &edits,
            &path("todos"),
            AppDataVersion(4),
            AppDataVersion(8),
            false,
        );
        assert_eq!(
            kinds(&items),
            [("change", 5), ("change", 7), ("position", 8)],
            "the writes at 6 and 8 are beside the path"
        );
    }

    #[test]
    fn no_position_when_the_last_change_is_the_head_or_nothing_is_new() {
        let edits = [edit(5, "todos/a", Some(1))];
        let at_head = outgoing(
            &edits,
            &path("todos"),
            AppDataVersion(4),
            AppDataVersion(5),
            false,
        );
        assert_eq!(kinds(&at_head), [("change", 5)]);
        let quiet = outgoing(
            &[],
            &path("todos"),
            AppDataVersion(5),
            AppDataVersion(5),
            false,
        );
        assert!(quiet.is_empty(), "{quiet:?}");
    }

    #[test]
    fn a_resumption_is_acknowledged_even_when_nothing_happened() {
        let items = outgoing(
            &[],
            &path("todos"),
            AppDataVersion(5),
            AppDataVersion(5),
            true,
        );
        assert_eq!(kinds(&items), [("position", 5)]);
        // And not twice when a change already reaches the head.
        let edits = [edit(6, "todos/a", Some(1))];
        let items = outgoing(
            &edits,
            &path("todos"),
            AppDataVersion(5),
            AppDataVersion(6),
            true,
        );
        assert_eq!(kinds(&items), [("change", 6)]);
    }
}
