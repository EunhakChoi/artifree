//! The committed change feed, `GET /v1/changes`, as server-sent events
//! (decision 0006).
//!
//! Storage records each commit's change in the commit's own transaction, so
//! the feed is read from the database: it survives restarts, and a commit
//! made while no stream is open is delivered when one connects with an older
//! cursor. A stream wakes when this process commits and also rereads storage
//! every [`HEARTBEAT`], so a change it was not told about still arrives.
//!
//! Each stream reads at most [`PAGE`] changes at a time and hands one event
//! at a time to the connection, so a subscriber that stops reading holds its
//! stream slot and connection, not a growing queue.

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use artifree_contracts::api::{ArtifactChange, ChangeKind as WireChangeKind, FeedPosition};
use artifree_domain::ChangeCursor;
use artifree_domain::identity::ActorId;
use artifree_ports::artifact_store::{ChangeKind, ChangePage, ChangeRecord};
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::header::{self, HeaderValue};
use axum::response::{IntoResponse, Response};
use http_body::Frame;
use serde::Serialize;
use tokio::sync::{mpsc, watch};

use crate::http::{AppState, Authenticated};
use crate::limits::Permit;
use crate::operations::ApiError;

/// Changes read from storage at a time.
pub(crate) const PAGE: usize = 100;
/// How often an idle stream sends a comment and rereads storage.
pub(crate) const HEARTBEAT: Duration = Duration::from_secs(15);

/// Signals shared by every stream of one server.
#[derive(Debug)]
pub(crate) struct Feed {
    /// Counts commits made by this process; streams wait for it to change.
    commits: watch::Sender<u64>,
    /// Set once when the server shuts down; open streams then end.
    closing: watch::Sender<bool>,
}

impl Feed {
    pub(crate) fn new() -> Self {
        Self {
            commits: watch::Sender::new(0),
            closing: watch::Sender::new(false),
        }
    }

    /// Wakes open streams after a commit wrote something.
    pub(crate) fn committed(&self) {
        self.commits
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Ends every open stream, so graceful shutdown does not wait for them.
    pub(crate) fn close(&self) {
        self.closing.send_replace(true);
    }
}

/// `GET /v1/changes`. Without `Last-Event-ID` the stream starts at the
/// current head with a `ready` event; with it, the stream resumes after that
/// cursor, or sends `reset` when the server cannot continue from it.
pub(crate) async fn stream(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let after = match headers.get("last-event-id") {
        None => None,
        Some(value) => Some(cursor(value)?),
    };
    let slot = match state.limiter().open_stream(&actor) {
        Ok(slot) => slot,
        Err(refusal) => return Ok(crate::http::refused(refusal)),
    };
    // One event in flight: the producer waits for the connection to take it.
    let (events, receiver) = mpsc::channel(1);
    let stream = Stream {
        commits: state.feed().commits.subscribe(),
        closing: state.feed().closing.subscribe(),
        state,
        actor,
        events,
        _slot: slot,
    };
    tokio::spawn(stream.run(after));
    let mut response = Body::new(EventBody(receiver)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    Ok(response)
}

/// A cursor from `Last-Event-ID`: a whole number, as the server sends it.
fn cursor(value: &HeaderValue) -> Result<ChangeCursor, ApiError> {
    value
        .to_str()
        .ok()
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse().ok())
        .map(ChangeCursor)
        .ok_or_else(|| ApiError::invalid("Last-Event-ID must be a change cursor from this feed"))
}

/// The producing side of one open stream. It owns the actor's stream slot
/// until it ends.
struct Stream {
    state: AppState,
    actor: ActorId,
    events: mpsc::Sender<Bytes>,
    commits: watch::Receiver<u64>,
    closing: watch::Receiver<bool>,
    _slot: Permit,
}

/// Why a stream stopped early.
struct Ended;

impl Stream {
    async fn run(mut self, after: Option<ChangeCursor>) {
        // `Ended` means the client left, the server is closing, or storage
        // failed; the client resumes from its last cursor either way.
        let _ended: Result<Infallible, Ended> = self.serve(after).await;
    }

    async fn serve(&mut self, after: Option<ChangeCursor>) -> Result<Infallible, Ended> {
        // Commits that land while the first page is read still wake the
        // stream: the receiver was subscribed before the read.
        let mut cursor = if let Some(cursor) = after {
            cursor
        } else {
            // A page of no changes reports the head.
            let head = self.read(ChangeCursor(0), 0).await?.head;
            self.send(position("ready", head)?).await?;
            head
        };
        loop {
            let page = self.read(cursor, PAGE).await?;
            if cursor > page.head {
                // The cursor is from another database or a restored older
                // one: the client must read its state again.
                self.send(position("reset", page.head)?).await?;
                cursor = page.head;
                continue;
            }
            let complete = page.changes.len() < PAGE;
            for change in page.changes {
                cursor = change.cursor;
                self.send(change_event(&change)?).await?;
            }
            if !complete {
                continue;
            }
            // Every change of this actor up to the head was sent.
            cursor = cursor.max(page.head);
            self.wait().await?;
        }
    }

    async fn read(&self, after: ChangeCursor, limit: usize) -> Result<ChangePage, Ended> {
        let actor = self.actor.clone();
        self.state
            .run(move |artifacts| artifacts.changes(&actor, after, limit))
            .await
            .map_err(|_| Ended)
    }

    /// Hands one event to the connection, waiting while it is busy.
    async fn send(&mut self, event: Bytes) -> Result<(), Ended> {
        tokio::select! {
            sent = self.events.send(event) => sent.map_err(|_| Ended),
            () = shutdown(&mut self.closing) => Err(Ended),
        }
    }

    /// Waits for a commit, the heartbeat, the client leaving, or shutdown.
    async fn wait(&mut self) -> Result<(), Ended> {
        tokio::select! {
            changed = self.commits.changed() => changed.map_err(|_| Ended),
            () = tokio::time::sleep(HEARTBEAT) => self.send(Bytes::from_static(b":\n\n")).await,
            () = self.events.closed() => Err(Ended),
            () = shutdown(&mut self.closing) => Err(Ended),
        }
    }
}

/// Resolves once the server starts shutting down.
async fn shutdown(closing: &mut watch::Receiver<bool>) {
    // An error means the signal is gone, which also happens only at shutdown.
    let _closed: bool = closing.wait_for(|closing| *closing).await.is_ok();
}

fn position(event: &str, cursor: ChangeCursor) -> Result<Bytes, Ended> {
    frame(event, cursor, &FeedPosition { cursor: cursor.0 })
}

fn change_event(change: &ChangeRecord) -> Result<Bytes, Ended> {
    let kind = match change.kind {
        ChangeKind::Created => WireChangeKind::Created,
        ChangeKind::Revised => WireChangeKind::Revised,
        _ => return Err(Ended),
    };
    frame(
        "change",
        change.cursor,
        &ArtifactChange {
            cursor: change.cursor.0,
            artifact_id: change.artifact.to_string(),
            change: kind,
            revision: change.revision.0,
            committed_at_ms: change.committed_at.0,
        },
    )
}

/// One server-sent event. JSON escapes line breaks, so the data is one line.
fn frame(event: &str, cursor: ChangeCursor, data: &impl Serialize) -> Result<Bytes, Ended> {
    let data = serde_json::to_string(data).map_err(|_| Ended)?;
    Ok(Bytes::from(format!(
        "event: {event}\nid: {}\ndata: {data}\n\n",
        cursor.0
    )))
}

/// The response body: events as the producer hands them over. It ends when
/// the producer stops.
struct EventBody(mpsc::Receiver<Bytes>);

impl HttpBody for EventBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.0
            .poll_recv(context)
            .map(|event| event.map(|bytes| Ok(Frame::data(bytes))))
    }
}
