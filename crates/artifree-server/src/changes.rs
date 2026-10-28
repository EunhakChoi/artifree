//! `GET /v1/changes`: the committed change feed as server-sent events
//! (decision 0006).
//!
//! A stream reads the actor's changes from storage, so it sends exactly what
//! was committed, in order, also across restarts. Commits in this process
//! wake open streams at once; a timer catches commits by other processes on
//! the same database. Each stream is produced by its own task into a channel
//! of one event, so a slow reader holds back only its own producer.

use std::collections::HashMap;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use artifree_contracts::api::{ArtifactChange, ChangeKind as WireKind, ErrorCode, FeedPosition};
use artifree_domain::change::{ChangeCursor, ChangeSequence, FeedId};
use artifree_domain::identity::ActorId;
use artifree_ports::artifact_store::{ChangeKind, ChangePage, ChangeRecord};
use axum::extract::State;
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use tokio::sync::{mpsc, watch};

use crate::http::{AppState, Authenticated};
use crate::operations::ApiError;

/// Idle time after which a stream sends a comment line, and after which it
/// rereads storage for commits made by other processes.
pub const HEARTBEAT: Duration = Duration::from_secs(15);
/// Default number of streams one actor may keep open.
pub const DEFAULT_STREAMS_PER_ACTOR: u32 = 16;
/// Changes read from storage at a time.
const PAGE: usize = 100;
/// `Retry-After` seconds when an actor has too many streams open.
const STREAM_RETRY_SECONDS: u32 = 5;

/// What all change streams of the process share: commit wake-ups, the
/// shutdown signal, and the per-actor stream count.
#[derive(Debug)]
pub(crate) struct Feed {
    commits: watch::Sender<()>,
    closing: watch::Sender<bool>,
    per_actor: u32,
    open: Arc<Mutex<HashMap<ActorId, u32>>>,
}

impl Feed {
    pub(crate) fn new(per_actor: u32) -> Self {
        Self {
            commits: watch::Sender::new(()),
            closing: watch::Sender::new(false),
            per_actor,
            open: Arc::default(),
        }
    }

    /// Wakes every open stream to read new commits.
    pub(crate) fn committed(&self) {
        self.commits.send_replace(());
    }

    /// Ends every open stream and every stream opened later.
    pub(crate) fn close(&self) {
        self.closing.send_replace(true);
    }

    /// Takes one of the actor's stream slots, or `None` when all are taken
    /// or the count is unusable.
    fn open(&self, actor: &ActorId) -> Option<Slot> {
        let mut open = self.open.lock().ok()?;
        let count = open.entry(actor.clone()).or_default();
        if *count >= self.per_actor {
            return None;
        }
        *count += 1;
        Some(Slot {
            open: Arc::clone(&self.open),
            actor: actor.clone(),
        })
    }
}

/// One open stream of an actor; dropping it frees the slot.
#[derive(Debug)]
struct Slot {
    open: Arc<Mutex<HashMap<ActorId, u32>>>,
    actor: ActorId,
}

impl Drop for Slot {
    fn drop(&mut self) {
        // A poisoned lock refuses every later stream, so the count left
        // behind does not matter.
        if let Ok(mut open) = self.open.lock()
            && let Some(count) = open.get_mut(&self.actor)
        {
            *count = count.saturating_sub(1);
        }
    }
}

/// Opens a change stream. Without `Last-Event-ID` it starts at the feed's
/// head with `ready`; with one it continues after that cursor, or sends
/// `reset` when the cursor belongs to another feed or lies beyond the head.
pub(crate) async fn stream(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let resume = last_event_id(&headers)?;
    let feed = state.feed();
    let Some(slot) = feed.open(&actor) else {
        return Ok(too_many_streams());
    };
    // Subscribe before the first read, so a commit after it wakes the stream.
    let commits = feed.commits.subscribe();
    let closing = feed.closing.subscribe();
    let reader = actor.clone();
    let start = state
        .run(move |artifacts| artifacts.changes(&reader, ChangeSequence(0), 0))
        .await?;
    let (first, position) = match resume {
        Some(cursor) if cursor.feed == start.feed && cursor.sequence <= start.head => {
            (None, cursor.sequence)
        }
        Some(_) => (Some(("reset", start.head)), start.head),
        None => (Some(("ready", start.head)), start.head),
    };
    let (sender, receiver) = mpsc::channel(1);
    let producer = Producer {
        state,
        actor,
        feed: start.feed,
        position,
        sender,
        commits,
        closing,
        _slot: slot,
    };
    tokio::spawn(producer.run(first));
    Ok(Sse::new(Events(receiver))
        .keep_alive(KeepAlive::new().interval(HEARTBEAT))
        .into_response())
}

/// The cursor in `Last-Event-ID`, if any.
fn last_event_id(headers: &HeaderMap) -> Result<Option<ChangeCursor>, ApiError> {
    let Some(value) = headers.get("last-event-id") else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .and_then(|text| text.parse().ok())
        .map(Some)
        .ok_or_else(|| ApiError::invalid("Last-Event-ID must be a cursor received from this feed"))
}

fn too_many_streams() -> Response {
    let mut response = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::RateLimited,
        "too many change streams are open; close one or retry after the given delay",
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(STREAM_RETRY_SECONDS));
    response
}

/// Why a producer stops: the reader left, the server is shutting down, or
/// storage failed. The reader reconnects in the last case.
#[derive(Debug)]
struct Stopped;

/// Reads one actor's changes and sends them to one stream.
struct Producer {
    state: AppState,
    actor: ActorId,
    feed: FeedId,
    /// The last sequence sent or known to hold no change for the actor.
    position: ChangeSequence,
    sender: mpsc::Sender<Result<Event, Infallible>>,
    commits: watch::Receiver<()>,
    closing: watch::Receiver<bool>,
    /// Held until the producer ends.
    _slot: Slot,
}

impl Producer {
    async fn run(mut self, first: Option<(&'static str, ChangeSequence)>) {
        let _stopped: Result<(), Stopped> = async {
            if let Some((name, sequence)) = first {
                let cursor = self.cursor(sequence).to_string();
                self.send(event(
                    name,
                    &cursor,
                    &FeedPosition {
                        cursor: cursor.clone(),
                    },
                )?)
                .await?;
            }
            loop {
                self.catch_up().await?;
                self.wait().await?;
            }
        }
        .await;
    }

    /// Sends every change after `position`, a page at a time.
    async fn catch_up(&mut self) -> Result<(), Stopped> {
        loop {
            let page = self.read().await?;
            for change in &page.changes {
                let data = self.change(change);
                self.send(event("change", &data.cursor, &data)?).await?;
                self.position = change.sequence;
            }
            if page.changes.len() < PAGE {
                // The actor has no change up to the head of this snapshot.
                self.position = self.position.max(page.head);
                return Ok(());
            }
        }
    }

    async fn read(&self) -> Result<ChangePage, Stopped> {
        let actor = self.actor.clone();
        let after = self.position;
        self.state
            .run(move |artifacts| artifacts.changes(&actor, after, PAGE))
            .await
            .map_err(|_| Stopped)
    }

    /// Waits for a commit in this process or the next storage poll.
    async fn wait(&mut self) -> Result<(), Stopped> {
        tokio::select! {
            biased;
            _ = self.closing.wait_for(|closing| *closing) => Err(Stopped),
            () = self.sender.closed() => Err(Stopped),
            changed = self.commits.changed() => changed.map_err(|_| Stopped),
            () = tokio::time::sleep(HEARTBEAT) => Ok(()),
        }
    }

    /// Sends one event, waiting while the reader is behind.
    async fn send(&mut self, event: Event) -> Result<(), Stopped> {
        tokio::select! {
            biased;
            _ = self.closing.wait_for(|closing| *closing) => Err(Stopped),
            sent = self.sender.send(Ok(event)) => sent.map_err(|_| Stopped),
        }
    }

    fn cursor(&self, sequence: ChangeSequence) -> ChangeCursor {
        ChangeCursor {
            feed: self.feed,
            sequence,
        }
    }

    fn change(&self, record: &ChangeRecord) -> ArtifactChange {
        ArtifactChange {
            cursor: self.cursor(record.sequence).to_string(),
            artifact_id: record.artifact.to_string(),
            change: match record.kind {
                ChangeKind::Created => WireKind::Created,
                ChangeKind::MetadataUpdated => WireKind::MetadataUpdated,
                ChangeKind::Deleted => WireKind::Deleted,
                ChangeKind::Recovered => WireKind::Recovered,
                // A kind added later is a revision to clients of this
                // contract: they read the artifact again.
                _ => WireKind::Revised,
            },
            revision: record.revision.0,
            metadata_revision: record.metadata_revision.0,
            committed_at_ms: record.committed_at.0,
        }
    }
}

/// An event whose `id` is `cursor` and whose data is `data` as JSON.
fn event(name: &str, cursor: &str, data: &impl serde::Serialize) -> Result<Event, Stopped> {
    Event::default()
        .event(name)
        .id(cursor)
        .json_data(data)
        .map_err(|_| Stopped)
}

/// The events a producer sends, as the response body's stream.
struct Events(mpsc::Receiver<Result<Event, Infallible>>);

impl Stream for Events {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(context)
    }
}
