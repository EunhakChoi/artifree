//! `GET /v1/artifacts/{id}/data/events`: a subscription to one path of the
//! caller's application data, as server-sent events (decision 0040).
//!
//! A stream starts with a `snapshot` of the value, then sends the new full
//! value in a `change` event whenever the data version moved, and ends with a
//! `too_large` event when the value outgrows what is sent whole. There is no
//! replay and no resume: a client that lost its stream opens another and gets
//! a fresh snapshot.
//!
//! Each stream reads storage for itself, at most once per interval, and keeps
//! only the last version it sent. What the streams share is a counter that a
//! write bumps after it committed, the shutdown signal, and a bounded registry
//! of the streams that are open or opening, which revocation walks.

use std::collections::HashMap;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use artifree_contracts::api::{AppData, DataTooLarge, ErrorCode, JsonValue, ReadDataQuery};
use artifree_core::artifacts::ArtifactError;
use artifree_core::data::{DataWatch, WatchedValue};
use artifree_domain::data::DataPath;
use artifree_domain::identity::{ActorId, ArtifactId, TokenId};
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::auth::AuthContext;
use crate::http::AppState;
use crate::operations::{self, ApiError};

/// Data streams one actor may keep open (decision 0040).
pub const DEFAULT_STREAMS_PER_ACTOR: u32 = 8;
/// Data streams the process may keep open (decision 0040).
pub const DEFAULT_STREAMS_PER_PROCESS: u32 = 128;
/// `Retry-After` seconds when an actor has too many data streams open.
const ACTOR_RETRY_SECONDS: u32 = 5;
/// `Retry-After` seconds when the process cannot take another data stream.
const PROCESS_RETRY_SECONDS: u32 = 10;

/// Limits and timings of data streams. The defaults are the numbers of
/// decision 0040; tests use smaller ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataStreamSettings {
    /// Streams one actor may keep open, at least 1.
    pub per_actor: u32,
    /// Streams the process may keep open, at least 1. Streams that are still
    /// opening count against twice this number.
    pub per_process: u32,
    /// The least time between two reads of one stream.
    pub min_interval: Duration,
    /// The longest time between two reads of one stream, which also checks
    /// that the artifact is still reachable.
    pub heartbeat: Duration,
    /// How long a stream lives before it ends cleanly.
    pub lifetime: Duration,
}

impl Default for DataStreamSettings {
    fn default() -> Self {
        Self {
            per_actor: DEFAULT_STREAMS_PER_ACTOR,
            per_process: DEFAULT_STREAMS_PER_PROCESS,
            min_interval: Duration::from_millis(250),
            heartbeat: Duration::from_secs(15),
            lifetime: Duration::from_secs(10 * 60),
        }
    }
}

impl DataStreamSettings {
    /// Refuses a limit or a time that is zero.
    pub(crate) fn validate(self) -> Result<Self, String> {
        let zero = self.per_actor == 0
            || self.per_process == 0
            || self.min_interval.is_zero()
            || self.heartbeat.is_zero()
            || self.lifetime.is_zero();
        if zero {
            return Err("data stream limits and times must be at least 1".into());
        }
        Ok(self)
    }

    /// Most streams that may be registered, open or still opening.
    fn registrations(self) -> usize {
        usize::try_from(self.per_process).map_or(usize::MAX, |count| count.saturating_mul(2))
    }
}

/// Why a stream cannot be opened now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The actor has all its streams open.
    Actor,
    /// The process cannot take another stream.
    Process,
    /// The registry is unusable: a task panicked while holding its lock.
    Unavailable,
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let (status, code, message, seconds) = match self {
            Self::Actor => (
                StatusCode::TOO_MANY_REQUESTS,
                ErrorCode::RateLimited,
                "too many data streams are open; close one or retry after the given delay",
                ACTOR_RETRY_SECONDS,
            ),
            Self::Process => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                "the server has all its data streams open; retry after the given delay",
                PROCESS_RETRY_SECONDS,
            ),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                "data streams are unavailable; retry after the given delay",
                PROCESS_RETRY_SECONDS,
            ),
        };
        let mut response = ApiError::new(status, code, message).into_response();
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        response
    }
}

/// One stream that is open or opening, as revocation sees it.
#[derive(Debug)]
struct Registered {
    id: u64,
    actor: ActorId,
    token: Option<TokenId>,
    revoked: watch::Sender<bool>,
}

#[derive(Debug, Default)]
struct Registry {
    next_id: u64,
    registered: Vec<Registered>,
    /// Open streams per actor: the places taken.
    places: HashMap<ActorId, u32>,
    /// Open streams in all: the sum of `places`.
    open: u32,
}

/// What the streams of the process share (decision 0040).
#[derive(Debug)]
pub(crate) struct DataStreams {
    settings: DataStreamSettings,
    /// Bumped after a commit that can change what a stream reads.
    wake: watch::Sender<u64>,
    closing: watch::Sender<bool>,
    registry: Arc<Mutex<Registry>>,
    #[cfg(test)]
    seams: seams::Seams,
}

impl DataStreams {
    pub(crate) fn new(settings: DataStreamSettings) -> Self {
        Self {
            settings,
            wake: watch::Sender::new(0),
            closing: watch::Sender::new(false),
            registry: Arc::default(),
            #[cfg(test)]
            seams: seams::Seams::default(),
        }
    }

    /// Tells open streams that data may have changed or become unreachable.
    /// Called after the commit; it takes no lock, waits for nothing, and
    /// cannot fail.
    pub(crate) fn wake(&self) {
        self.wake
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Ends every open stream and every stream opened later.
    pub(crate) fn close(&self) {
        self.closing.send_replace(true);
    }

    /// Ends every open or opening stream of `token_id`.
    pub(crate) fn revoke_token(&self, token_id: &TokenId) {
        self.revoke(|entry| entry.token.as_ref() == Some(token_id));
    }

    /// Ends every open or opening stream of `actor`, as when the user is
    /// disabled (decision 0036).
    pub(crate) fn revoke_actor(&self, actor: &ActorId) {
        self.revoke(|entry| &entry.actor == actor);
    }

    fn revoke(&self, matches: impl Fn(&Registered) -> bool) {
        if let Ok(registry) = self.registry.lock() {
            for entry in registry.registered.iter().filter(|entry| matches(entry)) {
                entry.revoked.send_replace(true);
            }
        }
    }

    /// Compares the counts with the limits, without taking or recording
    /// anything: a refusal costs no storage read.
    fn precheck(&self, actor: &ActorId) -> Result<(), Refusal> {
        let registry = self.registry.lock().map_err(|_| Refusal::Unavailable)?;
        self.check(&registry, actor)
    }

    fn check(&self, registry: &Registry, actor: &ActorId) -> Result<(), Refusal> {
        if registry.places.get(actor).copied().unwrap_or(0) >= self.settings.per_actor {
            return Err(Refusal::Actor);
        }
        if registry.open >= self.settings.per_process {
            return Err(Refusal::Process);
        }
        Ok(())
    }

    /// Registers a stream before its first read, so that a revocation from
    /// now on reaches it. Refuses at the bound.
    fn register(&self, auth: &AuthContext) -> Result<(Registration, Signals), Refusal> {
        let mut registry = self.registry.lock().map_err(|_| Refusal::Unavailable)?;
        if registry.registered.len() >= self.settings.registrations() {
            return Err(Refusal::Process);
        }
        let id = registry.next_id;
        registry.next_id = registry.next_id.wrapping_add(1);
        let (revoked, revoked_receiver) = watch::channel(false);
        registry.registered.push(Registered {
            id,
            actor: auth.actor.clone(),
            token: auth.token_id.clone(),
            revoked,
        });
        Ok((
            Registration {
                id,
                registry: Arc::clone(&self.registry),
            },
            Signals {
                closing: self.closing.subscribe(),
                revoked: revoked_receiver,
            },
        ))
    }

    /// Takes one place for `actor` in one step that checks both limits again.
    fn take_place(&self, actor: &ActorId) -> Result<Place, Refusal> {
        let mut registry = self.registry.lock().map_err(|_| Refusal::Unavailable)?;
        self.check(&registry, actor)?;
        *registry.places.entry(actor.clone()).or_default() += 1;
        registry.open += 1;
        Ok(Place {
            registry: Arc::clone(&self.registry),
            actor: actor.clone(),
        })
    }

    /// Reads the watched value. Every read of a stream passes through here.
    async fn observe(
        &self,
        state: &AppState,
        actor: &ActorId,
        artifact: &ArtifactId,
        path: &DataPath,
    ) -> Result<DataWatch, ArtifactError> {
        #[cfg(test)]
        self.seams.count_read();
        let (actor, artifact, path) = (actor.clone(), artifact.clone(), path.clone());
        let watched = state
            .run(move |artifacts| artifacts.watch_data(&actor, &artifact, &path))
            .await;
        // Held after storage was looked at: a write that comes now is newer
        // than what this read found.
        #[cfg(test)]
        self.seams.after_read().await;
        watched
    }

    /// Waits for a throttle interval to pass.
    async fn pause(&self, until: Instant) {
        #[cfg(test)]
        if self.seams.pause().await {
            return;
        }
        tokio::time::sleep_until(until).await;
    }
}

/// A stream's entry in the registry; dropping it removes the entry.
#[derive(Debug)]
struct Registration {
    id: u64,
    registry: Arc<Mutex<Registry>>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        // A poisoned registry refuses every later stream, so what it still
        // lists does not matter.
        if let Ok(mut registry) = self.registry.lock() {
            registry.registered.retain(|entry| entry.id != self.id);
        }
    }
}

/// One open stream of an actor; dropping it frees the place.
#[derive(Debug)]
struct Place {
    registry: Arc<Mutex<Registry>>,
    actor: ActorId,
}

impl Drop for Place {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.registry.lock() {
            if let Some(count) = registry.places.get_mut(&self.actor) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    registry.places.remove(&self.actor);
                }
            }
            registry.open = registry.open.saturating_sub(1);
        }
    }
}

/// Shutdown and revocation, as one stream sees them.
#[derive(Debug, Clone)]
struct Signals {
    closing: watch::Receiver<bool>,
    revoked: watch::Receiver<bool>,
}

impl Signals {
    /// Whether a signal is visible now.
    fn ended(&self) -> bool {
        *self.closing.borrow() || *self.revoked.borrow()
    }

    /// The refusal a request gets when a signal was visible before its stream
    /// started.
    fn refusal(&self) -> Option<ApiError> {
        if *self.revoked.borrow() {
            Some(ApiError::unauthenticated())
        } else if *self.closing.borrow() {
            Some(ApiError::unavailable("the server is shutting down"))
        } else {
            None
        }
    }

    /// Resolves when a signal is visible. A sender that is gone counts as a
    /// signal.
    async fn wait_ended(&mut self) {
        tokio::select! {
            biased;
            _ = self.closing.wait_for(|closing| *closing) => {}
            _ = self.revoked.wait_for(|revoked| *revoked) => {}
        }
    }
}

/// Opens a data stream (decision 0040): the order of the steps is the
/// contract, and each exit drops what the earlier steps took.
pub(crate) async fn events(
    State(state): State<AppState>,
    auth: AuthContext,
    headers: HeaderMap,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<ReadDataQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = path?;
    let artifact = operations::artifact_id(&id)?;
    if headers.contains_key("last-event-id") {
        return Err(ApiError::invalid(
            "a data stream cannot be resumed: open it again for a fresh snapshot",
        ));
    }
    let Query(ReadDataQuery { path: at }) = query?;
    let watched = operations::data_path("path", at.as_deref().unwrap_or_default())?;
    let streams = state.data_streams();
    if let Err(refusal) = streams.precheck(&auth.actor) {
        return Ok(refusal.into_response());
    }
    let (registration, signals) = match streams.register(&auth) {
        Ok(registered) => registered,
        Err(refusal) => return Ok(refusal.into_response()),
    };
    // The registration is what a disable ends; confirming the user after it
    // closes the window in which a request that authenticated earlier opens a
    // stream the disable has already passed.
    state.ensure_current(&auth.actor, auth.generation).await?;
    let wake = streams.wake.subscribe();
    // The first read is the access check: an artifact that is missing, in the
    // trash, purged, or another actor's is the same `404` and holds nothing
    // but this request's registration.
    let first = streams
        .observe(&state, &auth.actor, &artifact, &watched)
        .await?;
    if let Some(refusal) = signals.refusal() {
        return Err(refusal);
    }
    let place = match streams.take_place(&auth.actor) {
        Ok(place) => place,
        Err(refusal) => return Ok(refusal.into_response()),
    };
    let (sender, receiver) = mpsc::channel(1);
    let heartbeat = streams.settings.heartbeat;
    let now = Instant::now();
    let producer = Producer {
        lifetime_end: now + streams.settings.lifetime,
        last_read: now,
        version: 0,
        artifact: artifact.to_string(),
        path: watched.to_string(),
        state,
        actor: auth.actor,
        artifact_id: artifact,
        watched,
        sender,
        wake,
        signals: signals.clone(),
        _place: place,
        _registration: registration,
    };
    tokio::spawn(producer.run(first));
    Ok(Sse::new(Events { receiver, signals })
        .keep_alive(KeepAlive::new().interval(heartbeat))
        .into_response())
}

/// Why a producer stops: the reader left, a signal or a deadline came, or a
/// read failed. The client learns the reason from its next request.
#[derive(Debug)]
struct Stop;

/// What to do after an observation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Nothing moved.
    Nothing,
    /// Send this and go on.
    Send,
    /// Send this and end.
    SendAndEnd,
    /// End without sending: the version went backwards.
    End,
}

/// Decides what an observation at `version` means for a stream that last sent
/// `sent`. Versions a stream sends strictly increase.
pub(crate) fn next_step(sent: Option<u64>, version: u64, too_large: bool) -> Step {
    match sent {
        Some(sent) if version < sent => Step::End,
        Some(sent) if version == sent => Step::Nothing,
        _ if too_large => Step::SendAndEnd,
        _ => Step::Send,
    }
}

/// Reads one watched path for one stream and sends what changed.
struct Producer {
    state: AppState,
    actor: ActorId,
    artifact_id: ArtifactId,
    watched: DataPath,
    /// The artifact and path as the events name them.
    artifact: String,
    path: String,
    /// The version of the last event sent. The value read is dropped once its
    /// event is handed over.
    version: u64,
    lifetime_end: Instant,
    /// When the last read started.
    last_read: Instant,
    sender: mpsc::Sender<Result<Event, Infallible>>,
    wake: watch::Receiver<u64>,
    signals: Signals,
    _place: Place,
    _registration: Registration,
}

impl Producer {
    async fn run(mut self, first: DataWatch) {
        let _stopped: Result<(), Stop> = self.serve(first).await;
    }

    async fn serve(&mut self, first: DataWatch) -> Result<(), Stop> {
        if !self.emit(first, None).await? {
            return Ok(());
        }
        loop {
            // Deadlines and signals first: nothing below can postpone them.
            self.check()?;
            self.wait().await?;
            self.throttle().await?;
            let watch = self.read().await?;
            if !self.emit(watch, Some(self.version)).await? {
                return Ok(());
            }
        }
    }

    /// Ends the stream when its lifetime is over or a signal is visible.
    fn check(&self) -> Result<(), Stop> {
        if self.signals.ended() || Instant::now() >= self.lifetime_end {
            return Err(Stop);
        }
        Ok(())
    }

    /// Waits for a write in this process or the next heartbeat, whichever
    /// comes first. Signals and the lifetime come before both.
    async fn wait(&mut self) -> Result<(), Stop> {
        let heartbeat = self.last_read + self.state.data_streams().settings.heartbeat;
        tokio::select! {
            biased;
            () = self.signals.wait_ended() => Err(Stop),
            () = tokio::time::sleep_until(self.lifetime_end) => Err(Stop),
            () = self.sender.closed() => Err(Stop),
            () = tokio::time::sleep_until(heartbeat) => Ok(()),
            changed = self.wake.changed() => changed.map_err(|_| Stop),
        }
    }

    /// Keeps the least time between two reads. The wait ends at the lifetime
    /// at the latest.
    async fn throttle(&mut self) -> Result<(), Stop> {
        let due = self.last_read + self.state.data_streams().settings.min_interval;
        if Instant::now() < due {
            let until = due.min(self.lifetime_end);
            let streams = self.state.data_streams();
            tokio::select! {
                biased;
                () = self.signals.wait_ended() => return Err(Stop),
                () = self.sender.closed() => return Err(Stop),
                () = streams.pause(until) => {}
            }
        }
        self.check()
    }

    /// Reads the value. The wake-ups seen so far are marked as seen first, so
    /// that a write before this point is covered by the read and a write after
    /// it stays pending for the next one.
    async fn read(&mut self) -> Result<DataWatch, Stop> {
        self.wake.borrow_and_update();
        self.last_read = Instant::now();
        self.state
            .data_streams()
            .observe(&self.state, &self.actor, &self.artifact_id, &self.watched)
            .await
            .map_err(|_| Stop)
    }

    /// Sends what an observation calls for. `sent` is the version of the last
    /// event sent, `None` for the first. Returns whether the stream goes on.
    async fn emit(&mut self, watch: DataWatch, sent: Option<u64>) -> Result<bool, Stop> {
        let version = watch.version.0;
        let too_large = watch.value == WatchedValue::TooLarge;
        let step = next_step(sent, version, too_large);
        let name = match (step == Step::SendAndEnd, sent) {
            (true, _) => "too_large",
            (false, None) => "snapshot",
            (false, Some(_)) => "change",
        };
        match step {
            Step::End => return Err(Stop),
            Step::Nothing => return Ok(true),
            Step::Send | Step::SendAndEnd => {}
        }
        let event = self.event(name, watch)?;
        self.send(event).await?;
        self.version = version;
        Ok(step == Step::Send)
    }

    fn event(&self, name: &str, watch: DataWatch) -> Result<Event, Stop> {
        let event = Event::default().event(name);
        let version = watch.version.0;
        let built = match watch.value {
            WatchedValue::TooLarge => event.json_data(&DataTooLarge {
                artifact_id: self.artifact.clone(),
                path: self.path.clone(),
                data_version: version,
            }),
            WatchedValue::Absent => event.json_data(&AppData {
                artifact_id: self.artifact.clone(),
                path: self.path.clone(),
                data_version: version,
                exists: false,
                value: None,
            }),
            WatchedValue::Value(text) => {
                let value = JsonValue::from_text(text).ok_or(Stop)?;
                event.json_data(&AppData {
                    artifact_id: self.artifact.clone(),
                    path: self.path.clone(),
                    data_version: version,
                    exists: true,
                    value: Some(value),
                })
            }
        };
        built.map_err(|_| Stop)
    }

    /// Hands one event to the connection, waiting while the reader is behind.
    /// A signal or the lifetime comes before the hand-over and ends the wait.
    /// What is already queued is dropped by [`Events`] when a signal is
    /// visible, so the reader never sees an event after one.
    async fn send(&mut self, event: Event) -> Result<(), Stop> {
        tokio::select! {
            biased;
            () = self.signals.wait_ended() => Err(Stop),
            () = tokio::time::sleep_until(self.lifetime_end) => Err(Stop),
            sent = self.sender.send(Ok(event)) => sent.map_err(|_| Stop),
        }
    }
}

/// The events a producer sends, as the response body's stream. An event that
/// is waiting for the connection when a signal becomes visible is dropped, not
/// delivered.
struct Events {
    receiver: mpsc::Receiver<Result<Event, Infallible>>,
    signals: Signals,
}

impl Stream for Events {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.signals.ended() {
            return Poll::Ready(None);
        }
        self.receiver.poll_recv(context)
    }
}

/// Seams that let tests hold a read (after it looked at storage) or a throttle
/// wait open and count reads.
#[cfg(test)]
pub(crate) mod seams {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use tokio::sync::Semaphore;

    /// A point a stream waits at until the test lets it go on.
    #[derive(Debug)]
    pub(crate) struct Gate {
        arrived: Semaphore,
        released: Semaphore,
    }

    impl Gate {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {
                arrived: Semaphore::new(0),
                released: Semaphore::new(0),
            })
        }

        /// A stream calls this where the gate is armed.
        async fn pass(&self) {
            self.arrived.add_permits(1);
            if let Ok(permit) = self.released.acquire().await {
                permit.forget();
            }
        }

        /// Resolves when a stream has arrived at the gate.
        pub(crate) async fn arrival(&self) {
            if let Ok(permit) = self.arrived.acquire().await {
                permit.forget();
            }
        }

        /// Lets one waiting stream go on.
        pub(crate) fn release(&self) {
            self.released.add_permits(1);
        }
    }

    #[derive(Debug, Default)]
    pub(crate) struct Seams {
        reads: AtomicU64,
        read_gate: Mutex<Option<Arc<Gate>>>,
        throttle_gate: Mutex<Option<Arc<Gate>>>,
    }

    impl Seams {
        pub(super) fn count_read(&self) {
            self.reads.fetch_add(1, Ordering::SeqCst);
        }

        pub(super) async fn after_read(&self) {
            let gate = self.read_gate.lock().ok().and_then(|gate| gate.clone());
            if let Some(gate) = gate {
                gate.pass().await;
            }
        }

        /// Whether the wait was a held one.
        pub(super) async fn pause(&self) -> bool {
            let gate = self.throttle_gate.lock().ok().and_then(|gate| gate.clone());
            match gate {
                Some(gate) => {
                    gate.pass().await;
                    true
                }
                None => false,
            }
        }
    }

    impl super::DataStreams {
        /// How many reads streams have started.
        pub(crate) fn reads(&self) -> u64 {
            self.seams.reads.load(Ordering::SeqCst)
        }

        /// Holds every read at a gate, or releases the hold.
        pub(crate) fn hold_reads(&self, gate: Option<Arc<Gate>>) {
            if let Ok(mut held) = self.seams.read_gate.lock() {
                *held = gate;
            }
        }

        /// Holds every throttle wait at a gate, or releases the hold.
        pub(crate) fn hold_throttle(&self, gate: Option<Arc<Gate>>) {
            if let Ok(mut held) = self.seams.throttle_gate.lock() {
                *held = gate;
            }
        }

        /// Panics while holding the registry lock, as a faulty task would.
        pub(crate) fn poison_registry(&self) {
            let registry = Arc::clone(&self.registry);
            let poisoned = std::thread::spawn(move || {
                let _guard = registry.lock();
                panic!("poisoning the registry for a test");
            })
            .join();
            assert!(poisoned.is_err(), "the thread was meant to panic");
        }

        /// Open streams (places taken) and registered streams.
        pub(crate) fn counts(&self) -> Option<(u32, usize)> {
            let registry = self.registry.lock().ok()?;
            Some((registry.open, registry.registered.len()))
        }
    }
}
