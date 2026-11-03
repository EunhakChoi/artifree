//! Retained windows of data writes for subscriptions (decision 0038).
//!
//! For each actor and artifact the server keeps, in process memory, the last
//! writes to the data space: what each left at the path it addressed. A
//! subscription replays from this window after a reconnect and follows it
//! live. The window is bounded in writes and bytes, and a reader whose position
//! it no longer covers is told to start again (`reset`); the window is never
//! persisted.
//!
//! A write and the recording of its event happen under one [`Space::gate`], so
//! the window lists writes in the order they committed in this process. A
//! write made by anyone else, or a window that was dropped, shows up as a
//! version the window cannot place: [`Window::anchor`] and [`Window::record`]
//! then forget what they hold and start again from the version they can
//! vouch for.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use artifree_core::artifacts::{ArtifactError, ArtifactService};
use artifree_domain::AppDataVersion;
use artifree_domain::data::{DataChange, DataValue};
use artifree_domain::data_feed::DataEdit;
use artifree_domain::identity::{ActorId, ArtifactId};
use artifree_providers::storage::SqliteStore;
use tokio::sync::watch;

/// Most writes a window retains.
pub(crate) const RETAIN_WRITES: usize = 64;
/// Most bytes of values a window retains. A write heavier than this is not
/// retained and ends the window's coverage at that write.
pub(crate) const RETAIN_BYTES: usize = 256 * 1024;
/// Most windows kept in the process.
pub(crate) const MAX_WINDOWS: usize = 256;
/// Most windows kept for one actor.
pub(crate) const MAX_WINDOWS_PER_ACTOR: usize = 32;

/// A lock that a panic poisoned. The hub fails closed: nothing it holds is
/// used after that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Poisoned;

impl fmt::Display for Poisoned {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the data subscription state is unavailable")
    }
}

impl From<Poisoned> for ArtifactError {
    fn from(error: Poisoned) -> Self {
        Self::Unavailable(error.to_string())
    }
}

/// Why [`DataHub::space`] gave no window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpaceError {
    /// The registry is unusable; see [`Poisoned`].
    Poisoned,
    /// Every window that could make room is held by an open stream or a
    /// write in flight, so a new one would exceed the bounds. Nothing was
    /// inserted.
    Full,
}

impl fmt::Display for SpaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Poisoned => "the data subscription state is unavailable",
            Self::Full => "too many data subscriptions are open on this server",
        })
    }
}

/// What a window can say about the writes after a position.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Replay {
    /// Every write after the position, in version order, up to `head`.
    Edits {
        edits: Vec<Arc<DataEdit>>,
        head: AppDataVersion,
    },
    /// The window does not cover the position.
    Gap,
}

/// The recent writes to one space.
///
/// When `known`, `edits` holds the write of every version in `(floor, head]`,
/// in order, so a position in `[floor, head]` can be continued.
#[derive(Debug, Default)]
pub(crate) struct Window {
    known: bool,
    floor: u64,
    head: u64,
    edits: VecDeque<Kept>,
    bytes: usize,
}

/// A retained write and what it weighs, computed once.
#[derive(Debug)]
struct Kept {
    edit: Arc<DataEdit>,
    weight: usize,
}

impl Window {
    /// Records the write that left the space at `edit.version`. A write that
    /// follows the head extends the window; any other (the first, or after a
    /// write the window never saw) starts it again just before this write.
    pub(crate) fn record(&mut self, edit: DataEdit) {
        let version = edit.version.0;
        if !(self.known && version == self.head.saturating_add(1)) {
            self.clear();
            self.known = true;
            self.floor = version.saturating_sub(1);
        }
        self.head = version;
        let weight = edit.weight();
        self.bytes += weight;
        self.edits.push_back(Kept {
            edit: Arc::new(edit),
            weight,
        });
        while (self.edits.len() > RETAIN_WRITES || self.bytes > RETAIN_BYTES)
            && let Some(dropped) = self.edits.pop_front()
        {
            self.bytes = self.bytes.saturating_sub(dropped.weight);
            self.floor = dropped.edit.version.0;
        }
    }

    /// Takes `version`, which was just read from storage under the space's
    /// gate, as the head. A window that agrees keeps what it has; one that
    /// does not (a write by another process, a restore, a window that was
    /// never filled) forgets it.
    pub(crate) fn anchor(&mut self, version: AppDataVersion) {
        if !(self.known && self.head == version.0) {
            self.clear();
            self.known = true;
            self.floor = version.0;
            self.head = version.0;
        }
    }

    /// Forgets everything; the next [`Self::anchor`] or [`Self::record`]
    /// starts it again.
    pub(crate) fn invalidate(&mut self) {
        self.clear();
        self.known = false;
    }

    /// The writes after `position`, or [`Replay::Gap`] when the window does
    /// not hold all of them: it is not known, the position is ahead of the
    /// head, or before the oldest write it can vouch for.
    pub(crate) fn after(&self, position: AppDataVersion) -> Replay {
        if !self.known || position.0 > self.head || position.0 < self.floor {
            return Replay::Gap;
        }
        Replay::Edits {
            edits: self
                .edits
                .iter()
                .filter(|kept| kept.edit.version > position)
                .map(|kept| Arc::clone(&kept.edit))
                .collect(),
            head: AppDataVersion(self.head),
        }
    }

    fn clear(&mut self) {
        self.edits.clear();
        self.bytes = 0;
        self.floor = 0;
        self.head = 0;
    }
}

/// The window of one actor's space of one artifact, and what orders access to
/// it.
#[derive(Debug)]
pub(crate) struct Space {
    /// Held by a writer from before the storage write until its event is
    /// recorded, and by a reader while it reads storage and anchors the
    /// window, so the two agree on the version.
    gate: Arc<tokio::sync::Mutex<()>>,
    window: Mutex<Window>,
    wake: watch::Sender<()>,
    /// Set when the artifact is purged: the window holds nothing and any use
    /// of it fails, so a stream that still holds the space ends.
    ended: AtomicBool,
}

impl Space {
    fn new() -> Self {
        Self {
            gate: Arc::new(tokio::sync::Mutex::new(())),
            window: Mutex::new(Window::default()),
            wake: watch::Sender::new(()),
            ended: AtomicBool::new(false),
        }
    }

    /// The gate; see the type.
    pub(crate) fn gate(&self) -> &tokio::sync::Mutex<()> {
        &self.gate
    }

    /// Waits for the gate without holding a thread, and returns a guard that
    /// can move into a blocking task. A writer takes the gate here, before it
    /// asks for a blocking thread: a reader holds the gate while it waits for
    /// one, so a writer parked on a blocking thread could starve it.
    pub(crate) async fn gate_owned(&self) -> tokio::sync::OwnedMutexGuard<()> {
        Arc::clone(&self.gate).lock_owned().await
    }

    /// Runs `use_window` on the window. Fails once the space has ended.
    pub(crate) fn with_window<T>(
        &self,
        use_window: impl FnOnce(&mut Window) -> T,
    ) -> Result<T, Poisoned> {
        if self.ended.load(Ordering::Acquire) {
            return Err(Poisoned);
        }
        let mut window = self.window.lock().map_err(|_| Poisoned)?;
        Ok(use_window(&mut window))
    }

    /// Ends the space: drops every value its window retains, makes any later
    /// use of the window fail, and wakes its streams so that they end. A
    /// stream blocked on a slow reader still holds the space, but nothing in
    /// it. A window whose lock was poisoned is left alone: it is unusable
    /// already.
    fn end(&self) {
        self.ended.store(true, Ordering::Release);
        if let Ok(mut window) = self.window.lock() {
            window.invalidate();
        }
        self.wake();
    }

    /// The bytes of values the window retains, whatever its state.
    #[cfg(test)]
    pub(crate) fn retained_bytes(&self) -> usize {
        self.window.lock().map_or(0, |window| window.bytes)
    }

    /// A receiver that sees every wake-up from now on.
    pub(crate) fn watch(&self) -> watch::Receiver<()> {
        self.wake.subscribe()
    }

    /// Wakes the readers of this space to look at the window.
    pub(crate) fn wake(&self) {
        self.wake.send_replace(());
    }
}

#[derive(Debug)]
struct Entry {
    space: Arc<Space>,
    touched: u64,
}

#[derive(Debug, Default)]
struct Registry {
    clock: u64,
    spaces: HashMap<(ActorId, ArtifactId), Entry>,
}

impl Registry {
    /// Drops the least recently used window that no reader holds among those
    /// `candidate` accepts. A window with an open stream is never dropped.
    fn evict(&mut self, candidate: impl Fn(&ActorId) -> bool) {
        let victim = self
            .spaces
            .iter()
            .filter(|((actor, _), entry)| candidate(actor) && Arc::strong_count(&entry.space) == 1)
            .min_by_key(|(_, entry)| entry.touched)
            .map(|(key, _)| key.clone());
        if let Some(key) = victim {
            self.spaces.remove(&key);
        }
    }
}

/// Every retained window of the process.
#[derive(Debug, Default)]
pub(crate) struct DataHub {
    registry: Mutex<Registry>,
}

impl DataHub {
    /// The space of `actor` for `artifact`, made when it is missing. Making
    /// one over the bounds drops the least recently used windows that no
    /// stream or write holds, first the actor's own; when none can be
    /// dropped, nothing is inserted and the answer is [`SpaceError::Full`], so
    /// the registry never exceeds its bounds.
    ///
    /// Callers ask for a space only for an artifact the actor may use: a
    /// request that ends in `not found` must not make or evict a window.
    pub(crate) fn space(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
    ) -> Result<Arc<Space>, SpaceError> {
        let mut registry = self.registry.lock().map_err(|_| SpaceError::Poisoned)?;
        registry.clock += 1;
        let touched = registry.clock;
        let key = (actor.clone(), artifact.clone());
        if let Some(entry) = registry.spaces.get_mut(&key) {
            entry.touched = touched;
            return Ok(Arc::clone(&entry.space));
        }
        let own = |registry: &Registry| {
            registry
                .spaces
                .keys()
                .filter(|(owner, _)| owner == actor)
                .count()
        };
        if own(&registry) >= MAX_WINDOWS_PER_ACTOR {
            registry.evict(|owner| owner == actor);
            if own(&registry) >= MAX_WINDOWS_PER_ACTOR {
                return Err(SpaceError::Full);
            }
        }
        if registry.spaces.len() >= MAX_WINDOWS {
            registry.evict(|_| true);
            if registry.spaces.len() >= MAX_WINDOWS {
                return Err(SpaceError::Full);
            }
        }
        let space = Arc::new(Space::new());
        registry.spaces.insert(
            key,
            Entry {
                space: Arc::clone(&space),
                touched,
            },
        );
        Ok(space)
    }

    /// Drops one window from the registry (for example, one whose lock was
    /// poisoned), so that the next stream starts a new one. Streams that hold
    /// the old one end at their next look at it.
    pub(crate) fn discard(&self, actor: &ActorId, artifact: &ArtifactId) {
        self.unpoisoned()
            .spaces
            .remove(&(actor.clone(), artifact.clone()));
    }

    /// The registry for removing windows only. Removal is safe on a registry
    /// whose lock a panic poisoned (a map entry is whole or absent), and it
    /// must still work there: a purge cannot leave values in memory because
    /// the registry is unusable for everything else.
    fn unpoisoned(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Ends and drops the windows of a purged artifact, so that its values do
    /// not stay in memory, not even in a window that a stream still holds:
    /// each is emptied and marked ended before it is unlinked, and its
    /// streams are woken to end.
    pub(crate) fn forget(&self, artifact: &ArtifactId) {
        self.unpoisoned().spaces.retain(|(_, id), entry| {
            let keep = id != artifact;
            if !keep {
                entry.space.end();
            }
            keep
        });
    }

    /// The window kept for the pair, if there is one; none is made.
    ///
    /// # Errors
    ///
    /// When the registry is unusable.
    pub(crate) fn existing(
        &self,
        actor: &ActorId,
        artifact: &ArtifactId,
    ) -> Result<Option<Arc<Space>>, Poisoned> {
        let registry = self.registry.lock().map_err(|_| Poisoned)?;
        Ok(registry
            .spaces
            .get(&(actor.clone(), artifact.clone()))
            .map(|entry| Arc::clone(&entry.space)))
    }

    /// Poisons the registry's lock, as a panic in a holder would.
    #[cfg(test)]
    pub(crate) fn poison_registry(&self) {
        std::thread::scope(|scope| {
            let holder = scope.spawn(|| {
                let _guard = self.registry.lock();
                panic!("poisons the registry on purpose");
            });
            assert!(holder.join().is_err(), "the fixture did not poison");
        });
    }

    /// Whether a window is kept for the pair.
    #[cfg(test)]
    pub(crate) fn contains(&self, actor: &ActorId, artifact: &ArtifactId) -> bool {
        self.registry.lock().is_ok_and(|registry| {
            registry
                .spaces
                .contains_key(&(actor.clone(), artifact.clone()))
        })
    }

    /// How many windows are kept.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.registry
            .lock()
            .map_or(0, |registry| registry.spaces.len())
    }
}

/// Records the write that just committed and left the space at `version`, and
/// wakes the readers. Called under [`Space::gate`], right after the write.
///
/// The write is durable by now, so nothing here may fail the request. When the
/// event cannot be recorded (the merged value could not be read back, or the
/// window's lock was poisoned) the window is forgotten, so every stream
/// behind it is reset rather than left believing it saw everything; a window
/// that cannot even be forgotten is dropped from the hub and its streams end.
pub(crate) fn record_after_commit(
    hub: &DataHub,
    space: &Space,
    artifacts: &ArtifactService<SqliteStore>,
    (actor, artifact): (&ActorId, &ArtifactId),
    change: &DataChange,
    version: AppDataVersion,
) {
    if try_record(space, artifacts, actor, artifact, change, version).is_err()
        && space.with_window(Window::invalidate).is_err()
    {
        hub.discard(actor, artifact);
    }
    space.wake();
}

/// What a committed write does when it had no window to record in (the hub was
/// full): any window that exists for the pair at this moment is forgotten and
/// woken, so its streams reset at once instead of waiting for the next check.
/// A stream that opens later anchors from storage and sees the write. Returns
/// whether the registry could not be searched (it is poisoned), in which case
/// the caller wakes every data stream to compare itself with storage.
pub(crate) fn forget_unrecorded(hub: &DataHub, actor: &ActorId, artifact: &ArtifactId) -> bool {
    match hub.existing(actor, artifact) {
        Ok(Some(space)) => {
            if space.with_window(Window::invalidate).is_err() {
                hub.discard(actor, artifact);
            }
            space.wake();
            false
        }
        Ok(None) => false,
        Err(Poisoned) => true,
    }
}

fn try_record(
    space: &Space,
    artifacts: &ArtifactService<SqliteStore>,
    actor: &ActorId,
    artifact: &ArtifactId,
    change: &DataChange,
    version: AppDataVersion,
) -> Result<(), ArtifactError> {
    let edit = match change {
        DataChange::Set { path, value } => Some((path, Some(value.clone()))),
        // The root stays an object: emptying it leaves `{}`.
        DataChange::Remove { path } if path.is_root() => {
            Some((path, Some(DataValue::empty_object())))
        }
        DataChange::Remove { path }
        | DataChange::Merge {
            path,
            patch: DataValue::Null,
        } => Some((path, None)),
        DataChange::Merge { path, .. } => {
            // The result of a merge is what is at the path afterwards, so a
            // reader needs no merge algorithm. Read it back; if the space
            // moved on, another writer made the version this write expected.
            let read = artifacts.data(actor, artifact, path)?;
            (read.version == version).then_some((path, read.value))
        }
    };
    space.with_window(|window| match edit {
        Some((path, value)) => window.record(DataEdit {
            version,
            path: path.clone(),
            value,
        }),
        None => window.invalidate(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use artifree_domain::data::DataPath;

    use super::*;

    fn edit(version: u64, value: &str) -> DataEdit {
        DataEdit {
            version: AppDataVersion(version),
            path: DataPath::root(),
            value: Some(DataValue::String(value.to_owned())),
        }
    }

    fn versions(replay: &Replay) -> Option<Vec<u64>> {
        match replay {
            Replay::Edits { edits, .. } => Some(edits.iter().map(|edit| edit.version.0).collect()),
            Replay::Gap => None,
        }
    }

    fn actor(number: u8) -> ActorId {
        format!("u-{number:032x}")
            .parse()
            .unwrap_or_else(|error| panic!("fixture actor: {error}"))
    }

    fn small(count: usize) -> u8 {
        u8::try_from(count).unwrap_or_else(|_| panic!("{count} does not fit a fixture number"))
    }

    fn artifact(number: u8) -> ArtifactId {
        ArtifactId::from_random([number; 16])
    }

    #[test]
    fn a_window_replays_exactly_the_writes_after_a_position() {
        let mut window = Window::default();
        for version in 5..=8 {
            window.record(edit(version, "x"));
        }
        // The first write it saw is 5, so it vouches for positions from 4.
        assert_eq!(
            versions(&window.after(AppDataVersion(4))),
            Some(vec![5, 6, 7, 8])
        );
        assert_eq!(versions(&window.after(AppDataVersion(6))), Some(vec![7, 8]));
        assert_eq!(versions(&window.after(AppDataVersion(8))), Some(vec![]));
        assert_eq!(
            window.after(AppDataVersion(3)),
            Replay::Gap,
            "before the floor"
        );
        assert_eq!(
            window.after(AppDataVersion(9)),
            Replay::Gap,
            "ahead of the head"
        );
        assert_eq!(
            Window::default().after(AppDataVersion(0)),
            Replay::Gap,
            "unknown"
        );
    }

    #[test]
    fn a_write_the_window_never_saw_starts_it_again() {
        let mut window = Window::default();
        window.record(edit(1, "a"));
        window.record(edit(2, "b"));
        // Version 3 was written by someone else.
        window.record(edit(4, "d"));
        assert_eq!(
            window.after(AppDataVersion(2)),
            Replay::Gap,
            "version 3 is missing"
        );
        assert_eq!(versions(&window.after(AppDataVersion(3))), Some(vec![4]));
    }

    #[test]
    fn the_window_keeps_the_last_writes_and_says_where_its_coverage_ends() {
        let mut window = Window::default();
        for version in 1..=RETAIN_WRITES as u64 + 10 {
            window.record(edit(version, "x"));
        }
        let newest = RETAIN_WRITES as u64 + 10;
        let oldest_kept = newest - RETAIN_WRITES as u64 + 1;
        assert_eq!(
            versions(&window.after(AppDataVersion(oldest_kept - 1))).map(|v| v.len()),
            Some(RETAIN_WRITES)
        );
        assert_eq!(
            window.after(AppDataVersion(oldest_kept - 2)),
            Replay::Gap,
            "a position before the oldest retained write cannot be continued"
        );
    }

    #[test]
    fn a_window_is_bounded_in_bytes_and_a_heavy_write_ends_coverage() {
        let big = "x".repeat(RETAIN_BYTES / 2);
        let mut window = Window::default();
        window.record(edit(1, &big));
        window.record(edit(2, &big));
        window.record(edit(3, &big));
        // Two writes of half the bound plus overhead do not fit together.
        assert_eq!(
            window.after(AppDataVersion(2)),
            Replay::Edits {
                edits: vec![Arc::new(edit(3, &big))],
                head: AppDataVersion(3),
            }
        );
        assert_eq!(window.after(AppDataVersion(1)), Replay::Gap);
        // A write over the bound is not retained at all.
        window.record(edit(4, &"y".repeat(RETAIN_BYTES + 1)));
        assert_eq!(window.after(AppDataVersion(3)), Replay::Gap);
        assert_eq!(versions(&window.after(AppDataVersion(4))), Some(vec![]));
    }

    #[test]
    fn anchoring_trusts_storage_and_forgets_a_window_that_disagrees() {
        let mut window = Window::default();
        window.anchor(AppDataVersion(7));
        assert_eq!(versions(&window.after(AppDataVersion(7))), Some(vec![]));
        assert_eq!(window.after(AppDataVersion(6)), Replay::Gap);
        window.record(edit(8, "a"));
        // Storage agrees with the head: nothing is forgotten.
        window.anchor(AppDataVersion(8));
        assert_eq!(versions(&window.after(AppDataVersion(7))), Some(vec![8]));
        // Storage is ahead (another process wrote) or behind (a restore).
        window.anchor(AppDataVersion(10));
        assert_eq!(window.after(AppDataVersion(8)), Replay::Gap);
        window.anchor(AppDataVersion(2));
        assert_eq!(window.after(AppDataVersion(8)), Replay::Gap);
        assert_eq!(versions(&window.after(AppDataVersion(2))), Some(vec![]));
        window.invalidate();
        assert_eq!(window.after(AppDataVersion(2)), Replay::Gap);
    }

    #[test]
    fn windows_are_bounded_per_actor_and_in_all_and_spare_those_in_use() -> Result<(), SpaceError> {
        let hub = DataHub::default();
        let alice = actor(1);
        let bob = actor(2);
        let held = hub.space(&alice, &artifact(0))?;
        for number in 1..=40 {
            hub.space(&alice, &artifact(number))?;
        }
        let own = |hub: &DataHub, who: &ActorId| {
            hub.registry.lock().map_or(0, |registry| {
                registry.spaces.keys().filter(|(a, _)| a == who).count()
            })
        };
        assert_eq!(own(&hub, &alice), MAX_WINDOWS_PER_ACTOR);
        // The window a stream holds is still the registered one.
        let again = hub.space(&alice, &artifact(0))?;
        assert!(
            Arc::ptr_eq(&held, &again),
            "an open stream's window was dropped"
        );
        // Another actor's windows are not taken by the first actor's.
        let kept = hub.space(&bob, &artifact(100))?;
        for number in 41..=80 {
            hub.space(&alice, &artifact(number))?;
        }
        assert!(Arc::ptr_eq(&kept, &hub.space(&bob, &artifact(100))?));
        // The process-wide bound holds with many actors.
        for who in 3..=60 {
            for number in 0..8 {
                hub.space(&actor(who), &artifact(number))?;
            }
        }
        assert!(hub.len() <= MAX_WINDOWS, "{} windows", hub.len());
        Ok(())
    }

    #[test]
    fn a_registry_full_of_held_windows_refuses_instead_of_growing() {
        let hub = DataHub::default();
        let mut held = Vec::new();
        // Windows that streams hold fill the process, spread over actors.
        for who in 0..small(MAX_WINDOWS / MAX_WINDOWS_PER_ACTOR) {
            for number in 0..small(MAX_WINDOWS_PER_ACTOR) {
                held.push(
                    hub.space(&actor(who), &artifact(number))
                        .map(|space| (who, space)),
                );
            }
        }
        assert!(
            held.iter().all(Result::is_ok),
            "the bounds themselves were refused"
        );
        assert_eq!(hub.len(), MAX_WINDOWS);
        let newcomer = actor(200);
        for number in 0..20 {
            assert_eq!(
                hub.space(&newcomer, &artifact(number)).err(),
                Some(SpaceError::Full)
            );
            assert_eq!(hub.len(), MAX_WINDOWS, "the registry grew past its bound");
        }
        // A window that is already kept is still found, and one that is
        // released makes room.
        assert!(hub.space(&actor(0), &artifact(0)).is_ok());
        drop(held.pop());
        assert!(hub.space(&newcomer, &artifact(1)).is_ok());
        assert_eq!(hub.len(), MAX_WINDOWS);
    }

    #[test]
    fn one_actor_cannot_hold_more_windows_than_its_bound() {
        let hub = DataHub::default();
        let held: Vec<_> = (0..small(MAX_WINDOWS_PER_ACTOR))
            .map(|number| hub.space(&actor(1), &artifact(number)))
            .collect();
        assert!(held.iter().all(Result::is_ok));
        assert_eq!(
            hub.space(&actor(1), &artifact(100)).err(),
            Some(SpaceError::Full)
        );
        assert_eq!(hub.len(), MAX_WINDOWS_PER_ACTOR);
        // Another actor is unaffected.
        assert!(hub.space(&actor(2), &artifact(100)).is_ok());
    }

    #[test]
    fn a_discarded_window_leaves_the_registry() -> Result<(), SpaceError> {
        let hub = DataHub::default();
        let first = hub.space(&actor(1), &artifact(1))?;
        hub.discard(&actor(1), &artifact(1));
        assert!(!hub.contains(&actor(1), &artifact(1)));
        let second = hub.space(&actor(1), &artifact(1))?;
        assert!(!Arc::ptr_eq(&first, &second), "the old window came back");
        Ok(())
    }

    #[test]
    fn forgetting_an_artifact_drops_its_windows_for_every_actor() -> Result<(), SpaceError> {
        let hub = DataHub::default();
        let shared = artifact(1);
        hub.space(&actor(1), &shared)?;
        hub.space(&actor(2), &shared)?;
        hub.space(&actor(1), &artifact(2))?;
        hub.forget(&shared);
        assert_eq!(hub.len(), 1);
        Ok(())
    }
}
