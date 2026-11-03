//! `GET /v1/artifacts/{id}/data/events`: the value at one path of the caller's
//! data space as server-sent events (decision 0040).
//!
//! A stream sends a `snapshot` when it opens and a `change` with the new full
//! value whenever the space's version moves. It reads storage after a wake-up
//! and sends the current state if its version is newer than the last one sent;
//! there is no replay, no retained window, and no resume cursor. The only
//! state shared between streams is a wake-up signal and the counts of open
//! streams. Each stream is produced by its own task into a channel of one
//! event; while the reader is behind, the task keeps only the newest state it
//! has read and still notices that the artifact is gone. Only a write to data
//! in this process wakes a stream; a trash, a purge, or a write by another
//! process is seen at the next heartbeat read, not at each commit of anyone.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use artifree_contracts::api::{AppData, DataEventTooLarge, ErrorCode, JsonValue, ReadDataQuery};
use artifree_core::data::DataRead;
use artifree_domain::AppDataVersion;
use artifree_domain::data::{DataPath, DataValue};
use artifree_domain::identity::{ActorId, ArtifactId};
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::auth::AuthContext;
use crate::changes::{Events, TokenStreamRegistration, wait_for_revocation};
use crate::http::AppState;
use crate::operations::{self, ApiError};

/// Largest canonical value, in bytes, that an event carries. A larger value
/// ends the stream with a `too_large` event.
pub const MAX_EVENT_VALUE_BYTES: usize = 128 * 1024;
/// `Retry-After` seconds when an actor has too many data streams open.
const ACTOR_RETRY_SECONDS: u32 = 5;
/// `Retry-After` seconds when the process has too many data streams open.
const PROCESS_RETRY_SECONDS: u32 = 10;

/// Limits and intervals of data streams. The defaults are the documented
/// ones; tests use shorter intervals.
#[derive(Debug, Clone, Copy)]
pub struct DataStreamSettings {
    /// Data streams one actor may keep open, at least 1.
    pub per_actor: u32,
    /// Data streams the process may keep open, at least 1.
    pub process: u32,
    /// Idle time after which a stream sends a comment line and reads storage
    /// again, so that a trash, a purge, or a write by another process is seen.
    pub heartbeat: Duration,
    /// Least time between two reads of storage by one stream: writes that
    /// come faster are sent as the newest state.
    pub min_read_interval: Duration,
    /// How long a stream lives. It then ends, and its client connects again.
    pub lifetime: Duration,
}

impl Default for DataStreamSettings {
    fn default() -> Self {
        Self {
            per_actor: 8,
            process: 128,
            heartbeat: Duration::from_secs(15),
            min_read_interval: Duration::from_millis(250),
            lifetime: Duration::from_secs(10 * 60),
        }
    }
}

impl DataStreamSettings {
    /// Refuses a setting that would make streams unusable.
    pub(crate) fn validate(self) -> Result<Self, String> {
        if self.per_actor == 0 || self.process == 0 {
            return Err("data streams per actor and per process must be at least 1".into());
        }
        if self.heartbeat.is_zero() || self.lifetime.is_zero() {
            return Err("the data stream heartbeat and lifetime must be positive".into());
        }
        Ok(self)
    }
}

/// How many data streams are open, in total and per actor.
#[derive(Debug, Default)]
struct Counts {
    total: u32,
    per_actor: HashMap<ActorId, u32>,
}

/// Why a stream was not opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The actor has its share open.
    Actor,
    /// The process has its share open.
    Process,
    /// The counts cannot be used.
    Unusable,
}

/// One open data stream; dropping it frees its place in both counts.
#[derive(Debug)]
struct Slot {
    counts: Arc<Mutex<Counts>>,
    actor: ActorId,
}

impl Drop for Slot {
    fn drop(&mut self) {
        // A poisoned lock refuses every later stream, so the counts left
        // behind do not matter.
        if let Ok(mut counts) = self.counts.lock() {
            counts.total = counts.total.saturating_sub(1);
            if let Some(count) = counts.per_actor.get_mut(&self.actor) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    counts.per_actor.remove(&self.actor);
                }
            }
        }
    }
}

/// What all data streams of the process share: the wake-up signal for a
/// write to data and the counts of open streams.
#[derive(Debug)]
pub(crate) struct DataStreams {
    wakes: watch::Sender<()>,
    counts: Arc<Mutex<Counts>>,
    settings: DataStreamSettings,
}

impl DataStreams {
    pub(crate) fn new(settings: DataStreamSettings) -> Self {
        Self {
            wakes: watch::Sender::new(()),
            counts: Arc::default(),
            settings,
        }
    }

    /// Wakes every open data stream. Called after a write to data committed;
    /// it cannot fail, so no write depends on a subscription.
    pub(crate) fn changed(&self) {
        self.wakes.send_replace(());
    }

    /// Takes a place for one stream of `actor`, or says which limit is full.
    /// A refusal records nothing.
    fn open(&self, actor: &ActorId) -> Result<Slot, Refusal> {
        let mut counts = self.counts.lock().map_err(|_| Refusal::Unusable)?;
        if counts.total >= self.settings.process {
            return Err(Refusal::Process);
        }
        let open = counts.per_actor.get(actor).copied().unwrap_or(0);
        if open >= self.settings.per_actor {
            return Err(Refusal::Actor);
        }
        counts.total += 1;
        counts.per_actor.insert(actor.clone(), open + 1);
        Ok(Slot {
            counts: Arc::clone(&self.counts),
            actor: actor.clone(),
        })
    }

    #[cfg(test)]
    fn open_counts(&self) -> (u32, usize) {
        self.counts.lock().map_or((u32::MAX, usize::MAX), |counts| {
            (counts.total, counts.per_actor.len())
        })
    }
}

/// Opens a data stream.
pub(crate) async fn stream(
    State(state): State<AppState>,
    auth: AuthContext,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<ReadDataQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let Path(id) = path?;
    let artifact = operations::artifact_id(&id)?;
    let Query(ReadDataQuery { path: at }) = query?;
    // The events carry no `id`, so a client has no cursor to send. One that
    // sends it expects a resume this stream does not have.
    if headers.contains_key("last-event-id") {
        return Err(ApiError::invalid(
            "this stream has no resume cursor; connect without Last-Event-ID and use the snapshot",
        ));
    }
    let watched = operations::data_path("path", at.as_deref().unwrap_or_default())?;
    let streams = state.data_streams();
    let feed = state.feed();
    // Subscribe before the first read, so a data write after it wakes the stream.
    let wakes = streams.wakes.subscribe();
    let closing = feed.subscribe_closing();
    // The place comes first, so that a request the limits refuse costs no
    // storage work. It is held only while the request lasts if the first read,
    // which is also the access check, fails: an artifact the caller cannot see
    // leaves no place taken and nothing registered.
    let slot = match streams.open(&auth.actor) {
        Ok(slot) => slot,
        Err(refusal) => return Ok(too_many_streams(refusal)),
    };
    let first = read_state(&state, &auth.actor, &artifact, &watched).await?;
    let (revoked, registration) = if let Some(token_id) = &auth.token_id {
        let (tx, rx) = watch::channel(false);
        let Some(registration) = feed.register_token_stream(&auth.actor, token_id, tx) else {
            return Err(ApiError::unavailable("the stream registry is unusable"));
        };
        (Some(rx), Some(registration))
    } else {
        (None, None)
    };
    // The registration above is what a disable or a revocation ends; the
    // confirmation after it closes the window for a request that
    // authenticated earlier (decision 0036).
    state.ensure_current(&auth.actor, auth.generation).await?;
    let (sender, receiver) = mpsc::channel(1);
    let settings = streams.settings;
    let producer = Producer {
        state,
        actor: auth.actor,
        wire_artifact: artifact.to_string(),
        artifact,
        wire_path: watched.to_string(),
        path: watched,
        settings,
        last_sent: None,
        last_read: Instant::now(),
        deadline: Instant::now() + settings.lifetime,
        sender,
        wakes,
        closing,
        revoked,
        _slot: slot,
        _registration: registration,
    };
    tokio::spawn(producer.run(first));
    Ok(Sse::new(Events(receiver))
        .keep_alive(KeepAlive::new().interval(settings.heartbeat))
        .into_response())
}

fn too_many_streams(refusal: Refusal) -> Response {
    let (status, code, message, seconds) = match refusal {
        Refusal::Actor => (
            StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::RateLimited,
            "too many data streams are open; close one or retry after the given delay",
            Some(ACTOR_RETRY_SECONDS),
        ),
        Refusal::Process => (
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            "the server has too many data streams open; retry after the given delay",
            Some(PROCESS_RETRY_SECONDS),
        ),
        Refusal::Unusable => (
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            "data streams are unavailable",
            None,
        ),
    };
    let mut response = ApiError::new(status, code, message).into_response();
    if let Some(seconds) = seconds {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    }
    response
}

async fn read_state(
    state: &AppState,
    actor: &ActorId,
    artifact: &ArtifactId,
    path: &DataPath,
) -> Result<DataRead, artifree_core::artifacts::ArtifactError> {
    let (actor, artifact, path) = (actor.clone(), artifact.clone(), path.clone());
    state
        .run(move |artifacts| artifacts.data(&actor, &artifact, &path))
        .await
}

/// Why a producer stops: the reader left, the server is shutting down, access
/// ended, storage failed, or the stream reached the end of its life. The
/// client connects again in every case.
#[derive(Debug)]
struct Stopped;

/// What a producer does with a read, given the last version it sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// The version was sent already.
    Skip,
    /// Send it, as an event of this name.
    Send(&'static str),
    /// The version is older than one already sent: the database was replaced.
    /// Versions never go back within a stream, so it ends.
    Regressed,
}

fn decide(last_sent: Option<AppDataVersion>, read: AppDataVersion) -> Decision {
    match last_sent {
        None => Decision::Send("snapshot"),
        Some(last) if read > last => Decision::Send("change"),
        Some(last) if read == last => Decision::Skip,
        Some(_) => Decision::Regressed,
    }
}

/// The state a stream that could not send yet holds after reading again:
/// the newer one. Versions never go back within a stream, so a lower one
/// ends it, as it does when it is the next state to send.
fn newest(held: AppDataVersion, read: DataRead) -> Result<DataRead, Stopped> {
    if read.version < held {
        Err(Stopped)
    } else {
        Ok(read)
    }
}

/// What interrupted a wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    /// Read storage again.
    Wake,
    /// End the stream.
    Stop,
}

/// Reads one actor's data space and sends one stream.
struct Producer {
    state: AppState,
    actor: ActorId,
    artifact: ArtifactId,
    wire_artifact: String,
    path: DataPath,
    wire_path: String,
    settings: DataStreamSettings,
    /// The version of the last state sent; `None` before the snapshot.
    last_sent: Option<AppDataVersion>,
    last_read: Instant,
    deadline: Instant,
    sender: mpsc::Sender<Result<Event, Infallible>>,
    wakes: watch::Receiver<()>,
    closing: watch::Receiver<bool>,
    revoked: Option<watch::Receiver<bool>>,
    /// Held until the producer ends.
    _slot: Slot,
    _registration: Option<TokenStreamRegistration>,
}

impl Producer {
    async fn run(mut self, first: DataRead) {
        let _stopped: Result<(), Stopped> = async {
            self.push(first).await?;
            loop {
                if self.signal().await == Signal::Stop {
                    return Err(Stopped);
                }
                let read = self.read().await?;
                self.push(read).await?;
            }
        }
        .await;
    }

    /// Whether the server is shutting down or the user or token was revoked.
    fn ended(&self) -> bool {
        *self.closing.borrow()
            || self
                .revoked
                .as_ref()
                .is_some_and(|revoked| *revoked.borrow())
    }

    /// Waits for something to do: a data write in this process, or the
    /// heartbeat; or for a reason to stop.
    async fn signal(&mut self) -> Signal {
        let until = (Instant::now() + self.settings.heartbeat).min(self.deadline);
        wait_for_signal(
            stopped(&mut self.closing, &mut self.revoked, &self.sender),
            &mut self.wakes,
            self.deadline,
            until,
        )
        .await
    }

    /// Reads the current state, at most once per `min_read_interval`.
    async fn read(&mut self) -> Result<DataRead, Stopped> {
        let ready = self.last_read + self.settings.min_read_interval;
        if Instant::now() < ready {
            tokio::select! {
                biased;
                () = stopped(&mut self.closing, &mut self.revoked, &self.sender) => {
                    return Err(Stopped);
                }
                // The wait for the interval does not outlast the lifetime.
                () = tokio::time::sleep_until(self.deadline) => return Err(Stopped),
                () = tokio::time::sleep_until(ready) => {}
            }
        }
        self.last_read = Instant::now();
        // A missing, trashed, purged or unreadable artifact or space ends the
        // stream: an unreadable stored value is never skipped.
        read_state(&self.state, &self.actor, &self.artifact, &self.path)
            .await
            .map_err(|_| Stopped)
    }

    /// Sends `read` if it is newer than what was sent. While the reader is
    /// behind, it reads again at each wake-up and keeps only the newest state,
    /// and it stops if access ended.
    async fn push(&mut self, mut read: DataRead) -> Result<(), Stopped> {
        loop {
            let name = match decide(self.last_sent, read.version) {
                Decision::Skip => return Ok(()),
                Decision::Regressed => return Err(Stopped),
                Decision::Send(name) => name,
            };
            let sender = self.sender.clone();
            tokio::select! {
                biased;
                reserved = sender.reserve() => {
                    let permit = reserved.map_err(|_| Stopped)?;
                    // Room can be ready at the same moment as a revocation;
                    // nothing is sent to a stream that was ended.
                    if self.ended() {
                        return Err(Stopped);
                    }
                    let (event, fits) = self.event(name, &read)?;
                    permit.send(Ok(event));
                    self.last_sent = Some(read.version);
                    return if fits { Ok(()) } else { Err(Stopped) };
                }
                signal = self.signal() => {
                    if signal == Signal::Stop {
                        return Err(Stopped);
                    }
                    read = newest(read.version, self.read().await?)?;
                }
            }
        }
    }

    /// The event for `read`, and whether the value fits in one. A value over
    /// the bound is never sent: the event says so instead and the stream ends.
    fn event(&self, name: &'static str, read: &DataRead) -> Result<(Event, bool), Stopped> {
        let text = read.value.as_ref().map(DataValue::to_canonical);
        if text
            .as_ref()
            .is_some_and(|text| text.len() > MAX_EVENT_VALUE_BYTES)
        {
            let event = Event::default()
                .event("too_large")
                .json_data(DataEventTooLarge {
                    artifact_id: self.wire_artifact.clone(),
                    path: self.wire_path.clone(),
                    data_version: read.version.0,
                    max_bytes: MAX_EVENT_VALUE_BYTES as u64,
                })
                .map_err(|_| Stopped)?;
            return Ok((event, false));
        }
        let exists = text.is_some();
        // Text the domain wrote as canonical JSON that is not JSON is a
        // fault, not an absent value.
        let value = match text {
            Some(text) => Some(JsonValue::from_text(text).ok_or(Stopped)?),
            None => None,
        };
        let event = Event::default()
            .event(name)
            .json_data(AppData {
                artifact_id: self.wire_artifact.clone(),
                path: self.wire_path.clone(),
                data_version: read.version.0,
                exists,
                value,
            })
            .map_err(|_| Stopped)?;
        Ok((event, true))
    }
}

/// Waits for the first of: a reason to stop, a write to data, and the
/// heartbeat at `until` (never later than `deadline`). Whether the lifetime has
/// ended is decided first and without waiting: a timer that is already past
/// still takes a turn of the clock to fire, and a write that is always waiting
/// would be chosen over it every time, keeping the stream alive for ever.
async fn wait_for_signal(
    stop: impl Future<Output = ()>,
    wakes: &mut watch::Receiver<()>,
    deadline: Instant,
    until: Instant,
) -> Signal {
    if Instant::now() >= deadline {
        return Signal::Stop;
    }
    tokio::select! {
        biased;
        () = stop => Signal::Stop,
        changed = wakes.changed() => {
            if changed.is_ok() { Signal::Wake } else { Signal::Stop }
        }
        () = tokio::time::sleep_until(until.min(deadline)) => {
            if Instant::now() >= deadline { Signal::Stop } else { Signal::Wake }
        }
    }
}

/// Completes when the stream must end: the server is shutting down, the
/// user or token was revoked, or the reader left.
async fn stopped(
    closing: &mut watch::Receiver<bool>,
    revoked: &mut Option<watch::Receiver<bool>>,
    sender: &mpsc::Sender<Result<Event, Infallible>>,
) {
    tokio::select! {
        biased;
        _ = closing.wait_for(|closing| *closing) => {}
        () = wait_for_revocation(revoked) => {}
        () = sender.closed() => {}
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use artifree_core::data::DataRead;
    use artifree_domain::AppDataVersion;
    use artifree_domain::data::DataValue;
    use artifree_domain::identity::ActorId;

    use super::{
        DataStreamSettings, DataStreams, Decision, Refusal, Signal, decide, newest, wait_for_signal,
    };

    fn actor(number: u8) -> Result<ActorId, Box<dyn std::error::Error>> {
        Ok(format!("u-{number:032x}").parse()?)
    }

    fn streams(per_actor: u32, process: u32) -> DataStreams {
        DataStreams::new(DataStreamSettings {
            per_actor,
            process,
            ..DataStreamSettings::default()
        })
    }

    #[test]
    fn settings_that_would_make_streams_unusable_are_refused() {
        let usual = DataStreamSettings::default();
        assert!(usual.validate().is_ok());
        let refused = [
            (
                "no place per actor",
                DataStreamSettings {
                    per_actor: 0,
                    ..usual
                },
            ),
            (
                "no place in the process",
                DataStreamSettings {
                    process: 0,
                    ..usual
                },
            ),
            (
                "no heartbeat",
                DataStreamSettings {
                    heartbeat: Duration::ZERO,
                    ..usual
                },
            ),
            (
                "no lifetime",
                DataStreamSettings {
                    lifetime: Duration::ZERO,
                    ..usual
                },
            ),
        ];
        for (label, settings) in refused {
            assert!(settings.validate().is_err(), "{label} was accepted");
        }
    }

    #[test]
    fn a_version_is_sent_only_when_it_is_newer_than_the_last_sent() {
        let v = AppDataVersion;
        assert_eq!(decide(None, v(0)), Decision::Send("snapshot"));
        assert_eq!(decide(None, v(7)), Decision::Send("snapshot"));
        assert_eq!(decide(Some(v(3)), v(4)), Decision::Send("change"));
        assert_eq!(decide(Some(v(3)), v(9)), Decision::Send("change"));
        assert_eq!(decide(Some(v(3)), v(3)), Decision::Skip);
        assert_eq!(decide(Some(v(3)), v(2)), Decision::Regressed);
    }

    #[tokio::test]
    async fn the_end_of_the_lifetime_wins_over_wake_ups_that_never_pause() {
        let now = tokio::time::Instant::now();
        let (writer, mut wakes) = tokio::sync::watch::channel(());
        // A write is waiting, as it always is when writes never pause.
        writer.send_replace(());
        let signal = wait_for_signal(
            std::future::pending(),
            &mut wakes,
            now,
            now + Duration::from_secs(9),
        )
        .await;
        assert_eq!(signal, Signal::Stop, "the lifetime had ended");
        // Before the lifetime ends, the same write is a wake-up.
        writer.send_replace(());
        let signal = wait_for_signal(
            std::future::pending(),
            &mut wakes,
            now + Duration::from_secs(60),
            now + Duration::from_secs(9),
        )
        .await;
        assert_eq!(signal, Signal::Wake);
    }

    #[test]
    fn a_stream_that_is_behind_keeps_the_newest_state_and_ends_if_the_version_goes_back() {
        let at = |version: u64, value: Option<DataValue>| DataRead {
            version: AppDataVersion(version),
            value,
        };
        let held = AppDataVersion(5);
        let newer = at(7, Some(DataValue::Bool(true)));
        assert_eq!(newest(held, newer.clone()).ok(), Some(newer));
        let same = at(5, None);
        assert_eq!(newest(held, same.clone()).ok(), Some(same));
        assert!(newest(held, at(4, None)).is_err());
    }

    #[test]
    fn a_refused_stream_leaves_the_counts_as_they_were() -> Result<(), Box<dyn std::error::Error>> {
        let streams = streams(2, 3);
        let (alice, bob) = (actor(1)?, actor(2)?);
        let first = streams.open(&alice).map_err(|_| "first")?;
        let _second = streams.open(&alice).map_err(|_| "second")?;
        assert_eq!(streams.open(&alice).err(), Some(Refusal::Actor));
        assert_eq!(streams.open_counts(), (2, 1), "a refusal inserted");
        let _third = streams.open(&bob).map_err(|_| "third")?;
        // The process is full: even an actor with room is refused, and still
        // nothing is inserted.
        assert_eq!(streams.open(&actor(3)?).err(), Some(Refusal::Process));
        assert_eq!(streams.open_counts(), (3, 2));
        drop(first);
        assert_eq!(streams.open_counts(), (2, 2));
        let _again = streams.open(&alice).map_err(|_| "after a close")?;
        Ok(())
    }

    #[test]
    fn closed_streams_leave_no_entry_behind() -> Result<(), Box<dyn std::error::Error>> {
        let streams = streams(8, 128);
        for number in 0..40 {
            let slot = streams.open(&actor(number)?).map_err(|_| "open")?;
            drop(slot);
        }
        assert_eq!(streams.open_counts(), (0, 0), "the map keeps closed actors");
        Ok(())
    }
}
