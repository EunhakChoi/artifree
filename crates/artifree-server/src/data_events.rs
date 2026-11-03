//! `GET /v1/artifacts/{id}/data/events`: one path of the caller's application
//! data as server-sent events (decision 0040).
//!
//! A stream sends a `snapshot`, then a `change` with the full value each time
//! it reads a greater `data_version`, and ends on its own after its lifetime.
//! Nothing is replayed or kept: a stream remembers the version it last sent
//! and nothing else about the data, so a reconnect is a fresh snapshot.
//!
//! The only state shared between streams is the table of places (how many
//! streams exist, per actor and in all) and a counter that this process bumps
//! after a data write committed. A stream reads from storage when the counter
//! moves, at most once per `min_interval`, and when its heartbeat is due. Every
//! read is also the access check.

use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use artifree_contracts::api::{AppData, DataTooLarge, ErrorCode, WatchDataQuery};
use artifree_core::data::DataRead;
use artifree_domain::data::DataPath;
use artifree_domain::identity::{ActorId, ArtifactId};
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use tokio::sync::{mpsc, watch};

use crate::auth::AuthContext;
use crate::changes::{Events, TokenStreamRegistration, wait_for_revocation};
use crate::http::AppState;
use crate::operations::{self, ApiError};

/// Largest canonical value an event carries; a larger one is a `too_large`
/// event and ends the stream.
pub const EVENT_VALUE_BYTES: usize = 128 * 1024;
/// Streams one actor may hold.
pub const DEFAULT_STREAMS_PER_ACTOR: u32 = 8;
/// Streams the process may hold.
pub const DEFAULT_STREAMS_PER_PROCESS: u32 = 128;
/// Shortest time between two reads of one stream.
pub const MIN_READ_INTERVAL: Duration = Duration::from_millis(250);
/// Time after the last read at which a stream reads again, whatever happened.
pub const HEARTBEAT: Duration = Duration::from_secs(15);
/// How long a stream lives before it ends cleanly.
pub const LIFETIME: Duration = Duration::from_secs(10 * 60);
/// The longest any of the durations above may be configured.
const LONGEST_DURATION: Duration = Duration::from_secs(24 * 60 * 60);
/// `Retry-After` seconds for a refusal at a limit.
const RETRY_SECONDS: u32 = 5;

/// Limits and timings of data streams. The command line uses the defaults;
/// tests change them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataStreamSettings {
    /// Streams one actor may hold, at least 1.
    pub per_actor: u32,
    /// Streams the process may hold, at least 1.
    pub per_process: u32,
    /// Shortest time between two reads of one stream.
    pub min_interval: Duration,
    /// Time after the last read at which a stream reads again.
    pub heartbeat: Duration,
    /// How long a stream lives.
    pub lifetime: Duration,
}

impl Default for DataStreamSettings {
    fn default() -> Self {
        Self {
            per_actor: DEFAULT_STREAMS_PER_ACTOR,
            per_process: DEFAULT_STREAMS_PER_PROCESS,
            min_interval: MIN_READ_INTERVAL,
            heartbeat: HEARTBEAT,
            lifetime: LIFETIME,
        }
    }
}

impl DataStreamSettings {
    /// Checks the settings.
    ///
    /// # Errors
    ///
    /// A limit of 0, or a duration of 0 or over a day.
    pub fn validate(self) -> Result<Self, String> {
        if self.per_actor == 0 || self.per_process == 0 {
            return Err("data streams per actor and per process must be at least 1".into());
        }
        for duration in [self.min_interval, self.heartbeat, self.lifetime] {
            if duration.is_zero() || duration > LONGEST_DURATION {
                return Err("data stream timings must be above 0 and at most a day".into());
            }
        }
        Ok(self)
    }
}

/// Why a stream could not take a place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The actor holds its share.
    Actor,
    /// The process holds its share.
    Process,
    /// The table is unusable (a holder panicked).
    Unusable,
}

impl Refusal {
    fn into_response(self) -> Response {
        let error = match self {
            Self::Actor => ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                ErrorCode::RateLimited,
                "too many data streams are open; close one or retry after the given delay",
            ),
            Self::Process | Self::Unusable => ApiError::unavailable(
                "the server holds as many data streams as it allows; retry after the given delay",
            ),
        };
        let mut response = error.into_response();
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(RETRY_SECONDS));
        response
    }
}

#[derive(Debug, Default)]
struct Table {
    total: u32,
    by_actor: HashMap<ActorId, u32>,
}

/// What all data streams of the process share: the table of places and the
/// counter of writes.
#[derive(Debug)]
pub(crate) struct DataStreams {
    settings: DataStreamSettings,
    table: Arc<Mutex<Table>>,
    writes: watch::Sender<u64>,
    /// Storage reads that streams made, for tests.
    #[cfg(test)]
    reads: AtomicU64,
}

impl DataStreams {
    pub(crate) fn new(settings: DataStreamSettings) -> Self {
        Self {
            settings,
            table: Arc::default(),
            writes: watch::Sender::new(0),
            #[cfg(test)]
            reads: AtomicU64::new(0),
        }
    }

    /// Tells open streams that a data write (or a trash or purge) committed
    /// in this process. It carries nothing, cannot fail, and never waits.
    pub(crate) fn notify(&self) {
        self.writes
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Whether `actor` could take a place now. It records nothing.
    fn precheck(&self, actor: &ActorId) -> Result<(), Refusal> {
        let table = self.table.lock().map_err(|_| Refusal::Unusable)?;
        Self::fits(&table, actor, self.settings)
    }

    fn fits(table: &Table, actor: &ActorId, settings: DataStreamSettings) -> Result<(), Refusal> {
        if table.by_actor.get(actor).copied().unwrap_or(0) >= settings.per_actor {
            Err(Refusal::Actor)
        } else if table.total >= settings.per_process {
            Err(Refusal::Process)
        } else {
            Ok(())
        }
    }

    /// Takes a place, checking both limits again under the lock.
    fn admit(&self, actor: &ActorId) -> Result<Place, Refusal> {
        let mut table = self.table.lock().map_err(|_| Refusal::Unusable)?;
        Self::fits(&table, actor, self.settings)?;
        table.total += 1;
        *table.by_actor.entry(actor.clone()).or_default() += 1;
        Ok(Place {
            table: Arc::clone(&self.table),
            actor: actor.clone(),
        })
    }

    #[cfg(test)]
    pub(crate) fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn open_streams(&self) -> (u32, usize) {
        self.table.lock().map_or((u32::MAX, usize::MAX), |table| {
            (table.total, table.by_actor.len())
        })
    }
}

/// One open stream; dropping it frees the place and, when it was the actor's
/// last, the actor's entry.
#[derive(Debug)]
struct Place {
    table: Arc<Mutex<Table>>,
    actor: ActorId,
}

impl Drop for Place {
    fn drop(&mut self) {
        // A poisoned table refuses every later stream, so the count left
        // behind does not matter.
        if let Ok(mut table) = self.table.lock() {
            table.total = table.total.saturating_sub(1);
            if let Some(count) = table.by_actor.get_mut(&self.actor) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    table.by_actor.remove(&self.actor);
                }
            }
        }
    }
}

/// What the stream does next, as a function of the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The lifetime is over: end the stream.
    End,
    /// Read now.
    Read,
    /// Nothing is due; wait for a write, or for this instant.
    Idle(Instant),
    /// A read is due but too soon after the last; wait for this instant.
    Throttle(Instant),
}

/// The timing of one stream. Pure: every method takes the time.
#[derive(Debug, Clone, Copy)]
struct Pace {
    settings: DataStreamSettings,
    lifetime_end: Instant,
    last_read: Instant,
    /// A write was announced since the last read.
    pending: bool,
}

fn after(instant: Instant, duration: Duration) -> Instant {
    instant.checked_add(duration).unwrap_or(instant)
}

impl Pace {
    fn new(settings: DataStreamSettings, started: Instant) -> Self {
        Self {
            settings,
            lifetime_end: after(started, settings.lifetime),
            last_read: started,
            pending: false,
        }
    }

    /// The lifetime is judged first, then whether a read is due (a wake or
    /// the heartbeat), so a writer that never pauses cannot delay either, and
    /// no wait reaches past the lifetime.
    fn step(&self, now: Instant) -> Step {
        if now >= self.lifetime_end {
            return Step::End;
        }
        let heartbeat_at = after(self.last_read, self.settings.heartbeat);
        if self.pending || now >= heartbeat_at {
            let earliest = after(self.last_read, self.settings.min_interval);
            if now >= earliest {
                Step::Read
            } else {
                Step::Throttle(earliest.min(self.lifetime_end))
            }
        } else {
            Step::Idle(heartbeat_at.min(self.lifetime_end))
        }
    }

    fn woke(&mut self) {
        self.pending = true;
    }

    fn read(&mut self, now: Instant) {
        self.last_read = now;
        self.pending = false;
    }
}

/// What a read of the path found, as an event.
enum Frame {
    Value(AppData),
    TooLarge(DataTooLarge),
}

impl Frame {
    fn version(&self) -> u64 {
        match self {
            Self::Value(data) => data.data_version,
            Self::TooLarge(large) => large.data_version,
        }
    }

    /// The event: `name` for a value, `too_large` for the other.
    fn event(&self, name: &str) -> Result<Event, ApiError> {
        let built = match self {
            Self::Value(data) => Event::default().event(name).json_data(data),
            Self::TooLarge(large) => Event::default().event("too_large").json_data(large),
        };
        built.map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "an event could not be written",
            )
        })
    }
}

/// The event for what `read` found at `path` of `artifact`.
fn frame(artifact: &str, path: &str, read: &DataRead) -> Result<Frame, ApiError> {
    let text = read
        .value
        .as_ref()
        .map(artifree_domain::data::DataValue::to_canonical);
    if text
        .as_ref()
        .is_some_and(|text| text.len() > EVENT_VALUE_BYTES)
    {
        return Ok(Frame::TooLarge(DataTooLarge {
            artifact_id: artifact.to_owned(),
            path: path.to_owned(),
            data_version: read.version.0,
            max_bytes: u64::try_from(EVENT_VALUE_BYTES).unwrap_or(u64::MAX),
        }));
    }
    Ok(Frame::Value(AppData {
        artifact_id: artifact.to_owned(),
        path: path.to_owned(),
        data_version: read.version.0,
        exists: text.is_some(),
        value: text.map(operations::wire_value).transpose()?,
    }))
}

/// The identity of what a stream follows.
#[derive(Clone)]
struct Target {
    actor: ActorId,
    artifact: ArtifactId,
    path: DataPath,
    artifact_text: String,
    path_text: String,
}

impl Target {
    async fn read(&self, state: &AppState) -> Result<Frame, ApiError> {
        #[cfg(test)]
        state.data_streams().reads.fetch_add(1, Ordering::Relaxed);
        let target = self.clone();
        let read = state
            .run(move |artifacts| artifacts.data(&target.actor, &target.artifact, &target.path))
            .await
            .map_err(ApiError::from)?;
        frame(&self.artifact_text, &self.path_text, &read)
    }
}

/// Opens a data stream.
pub(crate) async fn stream(
    State(state): State<AppState>,
    auth: AuthContext,
    headers: HeaderMap,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<WatchDataQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    // Everything that needs no storage comes first.
    let Path(id) = path?;
    let artifact = operations::artifact_id(&id)?;
    let Query(WatchDataQuery { path: at }) = query?;
    let path = operations::data_path("path", at.as_deref().unwrap_or_default())?;
    if headers.contains_key("last-event-id") {
        return Err(ApiError::invalid(
            "data streams have no resume cursor: reconnect without Last-Event-ID for a new snapshot",
        ));
    }
    let streams = state.data_streams();
    if let Err(refusal) = streams.precheck(&auth.actor) {
        return Ok(refusal.into_response());
    }
    let target = Target {
        actor: auth.actor.clone(),
        artifact_text: artifact.to_string(),
        path_text: path.to_string(),
        artifact,
        path,
    };
    // Subscribe before the first read, so a write after it wakes the stream.
    let writes = streams.writes.subscribe();
    // The first read is the access check: an artifact that is missing,
    // trashed, purged, or someone else's holds no place here.
    let first = target.read(&state).await?;
    let place = match streams.admit(&auth.actor) {
        Ok(place) => place,
        Err(refusal) => return Ok(refusal.into_response()),
    };
    let feed = state.feed();
    let closing = feed.closing();
    let (revoked, registration) = if let Some(token_id) = &auth.token_id {
        let (sender, receiver) = watch::channel(false);
        let registration = feed
            .register_token_stream(&auth.actor, token_id, sender)
            .ok_or_else(|| ApiError::unavailable("the stream registry is unusable"))?;
        (Some(receiver), Some(registration))
    } else {
        (None, None)
    };
    // After registering, so that a disable that already ran is seen here or
    // runs its cleanup later (decision 0036).
    state.ensure_current(&auth.actor, auth.generation).await?;
    let started = Instant::now();
    let settings = streams.settings;
    let (sender, receiver) = mpsc::channel(1);
    let producer = Producer {
        state,
        target,
        pace: Pace::new(settings, started),
        sent: None,
        sender,
        writes,
        closing,
        revoked,
        _place: place,
        _registration: registration,
    };
    tokio::spawn(producer.run(first));
    Ok(Sse::new(Events(receiver))
        .keep_alive(KeepAlive::new().interval(settings.heartbeat))
        .into_response())
}

/// Why a producer stops. The reader reconnects after any of them.
#[derive(Debug)]
struct Stopped;

struct Producer {
    state: AppState,
    target: Target,
    pace: Pace,
    /// The version of the last event sent.
    sent: Option<u64>,
    sender: mpsc::Sender<Result<Event, std::convert::Infallible>>,
    writes: watch::Receiver<u64>,
    closing: watch::Receiver<bool>,
    revoked: Option<watch::Receiver<bool>>,
    /// Held until the producer ends.
    _place: Place,
    _registration: Option<TokenStreamRegistration>,
}

impl Producer {
    async fn run(mut self, first: Frame) {
        let _stopped: Result<(), Stopped> = self.drive(first).await;
    }

    async fn drive(&mut self, first: Frame) -> Result<(), Stopped> {
        if self.deliver("snapshot", &first).await? {
            return Ok(());
        }
        self.pace.read(Instant::now());
        loop {
            match self.pace.step(Instant::now()) {
                Step::End => return Ok(()),
                Step::Read => {
                    let frame = self.target.read(&self.state).await.map_err(|_| Stopped)?;
                    self.pace.read(Instant::now());
                    if self.deliver("change", &frame).await? {
                        return Ok(());
                    }
                }
                Step::Idle(until) => {
                    tokio::select! {
                        _ = self.closing.wait_for(|closing| *closing) => return Err(Stopped),
                        () = wait_for_revocation(&mut self.revoked) => return Err(Stopped),
                        () = self.sender.closed() => return Err(Stopped),
                        changed = self.writes.changed() => {
                            changed.map_err(|_| Stopped)?;
                            self.pace.woke();
                        }
                        () = tokio::time::sleep_until(until.into()) => {}
                    }
                }
                Step::Throttle(until) => {
                    tokio::select! {
                        _ = self.closing.wait_for(|closing| *closing) => return Err(Stopped),
                        () = wait_for_revocation(&mut self.revoked) => return Err(Stopped),
                        () = self.sender.closed() => return Err(Stopped),
                        () = tokio::time::sleep_until(until.into()) => {}
                    }
                }
            }
        }
    }

    /// Sends the frame if its version is new. Returns whether the stream is
    /// over (a `too_large` event was sent). A version below the last sent
    /// ends the stream: versions never go back unless the database was
    /// replaced.
    async fn deliver(&mut self, name: &str, frame: &Frame) -> Result<bool, Stopped> {
        let version = frame.version();
        match self.sent {
            Some(sent) if version == sent => return Ok(false),
            Some(sent) if version < sent => return Err(Stopped),
            _ => {}
        }
        let event = frame.event(name).map_err(|_| Stopped)?;
        self.send(event).await?;
        self.sent = Some(version);
        Ok(matches!(frame, Frame::TooLarge(_)))
    }

    /// Sends one event, waiting while the reader is behind but not past the
    /// lifetime.
    async fn send(&mut self, event: Event) -> Result<(), Stopped> {
        let until = self.pace.lifetime_end;
        tokio::select! {
            _ = self.closing.wait_for(|closing| *closing) => Err(Stopped),
            () = wait_for_revocation(&mut self.revoked) => Err(Stopped),
            () = tokio::time::sleep_until(until.into()) => Err(Stopped),
            sent = self.sender.send(Ok(event)) => sent.map_err(|_| Stopped),
        }
    }
}

#[cfg(test)]
mod tests;
