//! Rooms and presence (decision 0034): who is looking at an artifact now,
//! and short-lived events between them.
//!
//! A room is memory of one server process and nothing else. Nothing here
//! reads a clock, a socket or a database: callers pass the time as a
//! [`Duration`] on a monotonic scale of their choosing, so a lease can be
//! tested without waiting. State is lost when the process stops, and an
//! event is never kept for someone who was not there: the room keeps a few
//! recent events only so that a connected participant that polls late can
//! still read them, and says when it dropped some.
//!
//! Who is who comes from facts the server established, never from a claim
//! of the peer: the caller names its `actor`, `artifact` and `tab` through
//! [`Caller`], and the room derives `is_me` and `same_tab` for every member
//! and event it returns.

use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::time::Duration;

use artifree_domain::identity::{ActorId, ArtifactId};
use artifree_domain::room::{
    Epoch, EventData, EventKind, EventSeq, ParticipantId, PresenceState, Slot, TabLabel,
};

/// The largest counter a room hands out, so that every number it sends is a
/// safe integer in JavaScript.
const MAX_COUNTER: u64 = 9_007_199_254_740_991;

/// The bounds of a [`Rooms`]. Every bound is enforced when the room is used;
/// none is a hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Rooms with at least one participant that one actor may have
    /// (decision 0036): the cap is per actor, so that one user's rooms
    /// cannot keep another from opening a first.
    pub max_rooms: usize,
    /// Participants in one room.
    pub max_participants: usize,
    /// Participants one tab may have in one room.
    pub max_per_tab: usize,
    /// How long a join or heartbeat keeps a participant present.
    pub lease: Duration,
    /// Events a room keeps for participants that read late.
    pub kept_events: usize,
    /// Sustained time between two events of one participant.
    pub event_interval: Duration,
    /// Events one participant may send at once after being quiet.
    pub event_burst: u32,
}

impl Limits {
    /// The numbers decision 0034 documents: 64 rooms, 16 participants per
    /// room and 4 per tab, a 15 second lease, 128 kept events, and 4 events
    /// a second per participant with a burst of 8.
    pub const STANDARD: Self = Self {
        max_rooms: 64,
        max_participants: 16,
        max_per_tab: 4,
        lease: Duration::from_secs(15),
        kept_events: 128,
        event_interval: Duration::from_millis(250),
        event_burst: 8,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::STANDARD
    }
}

/// An opaque name of a tab that the server issued and authenticated, such
/// as the id of a runtime connection. It is a credential of that tab, so it
/// never leaves a room and its `Debug` form hides it.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct TabKey(String);

impl TabKey {
    /// Wraps the id the server issued to one tab.
    #[must_use]
    pub fn new(connection: &str) -> Self {
        Self(connection.to_owned())
    }
}

impl fmt::Debug for TabKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TabKey(..)")
    }
}

/// Who is calling, as the server knows it. None of the three comes from a
/// request body.
#[derive(Debug, Clone, Copy)]
pub struct Caller<'caller> {
    /// The authenticated account.
    pub actor: &'caller ActorId,
    /// The artifact whose room the call is about.
    pub artifact: &'caller ArtifactId,
    /// The tab the authenticated call came from.
    pub tab: &'caller TabKey,
}

/// Why a call was refused. Nothing was changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomError {
    /// The participant does not exist for this caller: it never joined,
    /// its lease ran out, it left, it belongs to another tab, or a newer
    /// connection has replaced the one that called. All are one answer.
    NotFound,
    /// The room already has the most participants it allows.
    RoomFull,
    /// The tab already has the most participants it may have in this room.
    TabFull,
    /// The server already holds the most rooms it allows.
    TooManyRooms,
    /// The participant sent events faster than it may.
    RateLimited {
        /// How long until one more event is allowed.
        retry_after: Duration,
    },
    /// The position the caller says it has read is beyond the newest event.
    CursorAhead,
    /// A counter reached the largest safe number.
    Exhausted,
}

impl fmt::Display for RoomError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotFound => "no such participant; join the room again",
            Self::RoomFull => "the room has the most participants it allows",
            Self::TabFull => "this tab has the most participants it may have in the room",
            Self::TooManyRooms => "the server holds the most rooms it allows",
            Self::RateLimited { .. } => "events are sent faster than allowed; retry shortly",
            Self::CursorAhead => "the position is beyond the newest event of the room",
            Self::Exhausted => "room counters are exhausted",
        })
    }
}

impl Error for RoomError {}

/// One member of a room as one viewer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberView {
    /// The member's participant.
    pub participant: ParticipantId,
    /// The member's tab.
    pub tab: TabLabel,
    /// The account of the member.
    pub actor: ActorId,
    /// Whether the member is the viewer's own participant.
    pub is_me: bool,
    /// Whether the member is in the viewer's tab, the viewer included.
    pub same_tab: bool,
    /// What the member says about itself.
    pub state: PresenceState,
}

/// One ephemeral event as one viewer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventView {
    /// The event's position in the room.
    pub seq: EventSeq,
    /// The sender's participant.
    pub participant: ParticipantId,
    /// The sender's tab.
    pub tab: TabLabel,
    /// Whether the viewer's own participant sent it.
    pub is_me: bool,
    /// Whether it came from the viewer's tab, the viewer included.
    pub same_tab: bool,
    /// What the sender says the event is about.
    pub kind: EventKind,
    /// The sender's text.
    pub data: EventData,
}

/// What a join or a heartbeat returns: the viewer's own identity, the
/// members present, and events it has not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The viewer's participant.
    pub participant: ParticipantId,
    /// The viewer's connection generation; send it with every later call.
    pub epoch: Epoch,
    /// The viewer's tab as this room labels it.
    pub tab: TabLabel,
    /// How long the lease lasts from this call.
    pub lease: Duration,
    /// Everyone present, the viewer included, by participant.
    pub members: Vec<MemberView>,
    /// Events after the viewer's position, oldest first.
    pub events: Vec<EventView>,
    /// The newest event's position, or 0 if the room never had one.
    pub head: EventSeq,
    /// Whether the room dropped events the viewer had not read.
    pub missed: bool,
}

/// A minimum time between events of one participant: a generic cell rate
/// schedule, as for requests in decision 0005.
#[derive(Debug, Clone, Copy)]
struct Pacer {
    next: Duration,
}

impl Pacer {
    fn admit(&mut self, now: Duration, limits: &Limits) -> Result<(), Duration> {
        let next = self.next.max(now);
        let tolerance = limits
            .event_interval
            .saturating_mul(limits.event_burst.saturating_sub(1));
        let allowed_from = next.saturating_sub(tolerance);
        if now < allowed_from {
            return Err(allowed_from.saturating_sub(now));
        }
        self.next = next.saturating_add(limits.event_interval);
        Ok(())
    }
}

#[derive(Debug)]
struct Member {
    id: ParticipantId,
    epoch: Epoch,
    tab: TabKey,
    label: TabLabel,
    slot: Slot,
    state: PresenceState,
    lease_until: Duration,
    /// The room's newest event when the participant first joined. Events at
    /// or before it are never delivered to it, whatever position it asks
    /// for.
    floor: EventSeq,
    /// The newest position the participant has said it read (the `after` of
    /// its heartbeats), at least its floor. A resumed join returns the
    /// events after it, so none that arrived while the participant was away
    /// is skipped without a word.
    read: EventSeq,
    pacer: Pacer,
}

#[derive(Debug)]
struct Stored {
    seq: EventSeq,
    from: ParticipantId,
    tab: TabLabel,
    kind: EventKind,
    data: EventData,
}

#[derive(Debug, Default)]
struct Room {
    members: Vec<Member>,
    tabs: Vec<(TabKey, TabLabel)>,
    events: VecDeque<Stored>,
    head: u64,
}

impl Room {
    fn find(&self, tab: &TabKey, participant: ParticipantId, epoch: Epoch) -> Option<usize> {
        self.members.iter().position(|member| {
            member.id == participant && member.epoch == epoch && member.tab == *tab
        })
    }

    fn forget_empty_tabs(&mut self) {
        let members = &self.members;
        self.tabs
            .retain(|(key, _)| members.iter().any(|member| member.tab == *key));
    }

    /// The members and unread events of `index`'s participant, reading from
    /// `after` or from the participant's floor, whichever is later.
    fn snapshot(&self, index: usize, actor: &ActorId, after: u64, limits: &Limits) -> Snapshot {
        let me = &self.members[index];
        let from = after.max(me.floor.0);
        let first_kept = self
            .head
            .saturating_add(1)
            .saturating_sub(self.events.len() as u64);
        Snapshot {
            participant: me.id,
            epoch: me.epoch,
            tab: me.label,
            lease: limits.lease,
            members: self
                .members
                .iter()
                .map(|member| MemberView {
                    participant: member.id,
                    tab: member.label,
                    actor: actor.clone(),
                    is_me: member.id == me.id,
                    same_tab: member.label == me.label,
                    state: member.state.clone(),
                })
                .collect(),
            events: self
                .events
                .iter()
                .filter(|event| event.seq.0 > from)
                .map(|event| EventView {
                    seq: event.seq,
                    participant: event.from,
                    tab: event.tab,
                    is_me: event.from == me.id,
                    same_tab: event.tab == me.label,
                    kind: event.kind.clone(),
                    data: event.data.clone(),
                })
                .collect(),
            head: EventSeq(self.head),
            missed: from.saturating_add(1) < first_kept,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct RoomKey {
    actor: ActorId,
    artifact: ArtifactId,
}

impl RoomKey {
    fn of(caller: &Caller<'_>) -> Self {
        Self {
            actor: caller.actor.clone(),
            artifact: caller.artifact.clone(),
        }
    }
}

/// How many rooms and participants exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Rooms with a participant.
    pub rooms: usize,
    /// Participants over all rooms.
    pub participants: usize,
}

/// Every room of one server process.
#[derive(Debug)]
pub struct Rooms {
    limits: Limits,
    table: HashMap<RoomKey, Room>,
    /// The numbers handed out, kept per actor (decision 0036): a number is
    /// only compared inside a room, which belongs to one actor, and a counter
    /// shared by all actors would tell one user how many joins the others made.
    counters: HashMap<ActorId, Counters>,
}

/// The next number of each kind an actor's rooms hand out.
#[derive(Debug)]
struct Counters {
    participant: u64,
    tab: u64,
    epoch: u64,
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            participant: 1,
            tab: 1,
            epoch: 1,
        }
    }
}

fn take(counter: &mut u64) -> Result<u64, RoomError> {
    let value = *counter;
    if value > MAX_COUNTER {
        return Err(RoomError::Exhausted);
    }
    *counter = value + 1;
    Ok(value)
}

impl Rooms {
    /// No rooms, under `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            table: HashMap::new(),
            counters: HashMap::new(),
        }
    }

    /// Rooms and participants that exist now, leases not yet judged.
    #[must_use]
    pub fn counts(&self) -> Counts {
        Counts {
            rooms: self.table.len(),
            participants: self.table.values().map(|room| room.members.len()).sum(),
        }
    }

    /// Removes participants whose lease ended at or before `now`, and the
    /// rooms they leave empty. Every call below does this first.
    fn sweep(&mut self, now: Duration) {
        for room in self.table.values_mut() {
            room.members.retain(|member| member.lease_until > now);
            room.forget_empty_tabs();
        }
        self.table.retain(|_, room| !room.members.is_empty());
    }

    /// Joins the room, or resumes the participant this tab already has in
    /// `slot`. Resuming keeps the participant (no second entry appears) and
    /// gives it a newer epoch, so the connection that held the older epoch
    /// can no longer act. A participant whose lease ran out is gone: the
    /// same call then makes a new one. The state is replaced by `state`, or
    /// emptied if there is none.
    ///
    /// A new participant's snapshot has no events: an event sent before the
    /// join is never delivered to it. A resumed participant's snapshot holds
    /// the events after the newest position it reported by
    /// [`Rooms::heartbeat`] (its join floor if it reported none), and
    /// `missed` is set when the room already dropped some of them. That can
    /// repeat events an earlier connection of the participant handled;
    /// readers tell them apart by `seq`.
    ///
    /// # Errors
    ///
    /// [`RoomError::RoomFull`], [`RoomError::TabFull`],
    /// [`RoomError::TooManyRooms`] or [`RoomError::Exhausted`].
    pub fn join(
        &mut self,
        now: Duration,
        caller: &Caller<'_>,
        slot: Slot,
        state: Option<PresenceState>,
    ) -> Result<Snapshot, RoomError> {
        self.sweep(now);
        let key = RoomKey::of(caller);
        let state = state.unwrap_or_else(PresenceState::empty);
        let lease_until = now.saturating_add(self.limits.lease);
        if let Some(room) = self.table.get_mut(&key)
            && let Some(index) = room
                .members
                .iter()
                .position(|member| member.tab == *caller.tab && member.slot == slot)
        {
            let counters = self.counters.entry(caller.actor.clone()).or_default();
            let epoch = Epoch(take(&mut counters.epoch)?);
            let member = &mut room.members[index];
            member.epoch = epoch;
            member.state = state;
            member.lease_until = lease_until;
            let read = member.read.0;
            return Ok(room.snapshot(index, caller.actor, read, &self.limits));
        }
        if let Some(room) = self.table.get(&key) {
            if room.members.len() >= self.limits.max_participants {
                return Err(RoomError::RoomFull);
            }
            let own = room
                .members
                .iter()
                .filter(|member| member.tab == *caller.tab)
                .count();
            if own >= self.limits.max_per_tab {
                return Err(RoomError::TabFull);
            }
        } else if self
            .table
            .keys()
            .filter(|room| room.actor == *caller.actor)
            .count()
            >= self.limits.max_rooms
        {
            return Err(RoomError::TooManyRooms);
        }
        let counters = self.counters.entry(caller.actor.clone()).or_default();
        let id = ParticipantId(take(&mut counters.participant)?);
        let epoch = Epoch(take(&mut counters.epoch)?);
        let known = self.table.get(&key).and_then(|room| {
            room.tabs
                .iter()
                .find(|(tab, _)| tab == caller.tab)
                .map(|(_, label)| *label)
        });
        let label = match known {
            Some(label) => label,
            None => TabLabel(take(&mut counters.tab)?),
        };
        let room = self.table.entry(key).or_default();
        if known.is_none() {
            room.tabs.push((caller.tab.clone(), label));
        }
        room.members.push(Member {
            id,
            epoch,
            tab: caller.tab.clone(),
            label,
            slot,
            state,
            lease_until,
            floor: EventSeq(room.head),
            read: EventSeq(room.head),
            pacer: Pacer {
                next: Duration::ZERO,
            },
        });
        let index = room.members.len() - 1;
        let head = room.head;
        Ok(room.snapshot(index, caller.actor, head, &self.limits))
    }

    /// Renews the lease of a participant and reads the room: the members
    /// now and the events after `after`. The caller says only what it has
    /// read; events from before the participant joined are never returned.
    /// A `state` replaces the participant's state; `None` keeps it.
    ///
    /// # Errors
    ///
    /// [`RoomError::NotFound`] when the participant is not the caller's
    /// current one, [`RoomError::CursorAhead`] when `after` is past the
    /// newest event.
    pub fn heartbeat(
        &mut self,
        now: Duration,
        caller: &Caller<'_>,
        participant: ParticipantId,
        epoch: Epoch,
        after: EventSeq,
        state: Option<PresenceState>,
    ) -> Result<Snapshot, RoomError> {
        self.sweep(now);
        let room = self
            .table
            .get_mut(&RoomKey::of(caller))
            .ok_or(RoomError::NotFound)?;
        let index = room
            .find(caller.tab, participant, epoch)
            .ok_or(RoomError::NotFound)?;
        if after.0 > room.head {
            return Err(RoomError::CursorAhead);
        }
        let member = &mut room.members[index];
        member.lease_until = now.saturating_add(self.limits.lease);
        member.read = EventSeq(member.read.0.max(after.0));
        if let Some(state) = state {
            member.state = state;
        }
        Ok(room.snapshot(index, caller.actor, after.0, &self.limits))
    }

    /// Sends one ephemeral event to the room. It is delivered to whoever is
    /// present and polls before it is dropped, and to nobody else, ever.
    /// Sending does not renew the lease.
    ///
    /// # Errors
    ///
    /// [`RoomError::NotFound`], [`RoomError::RateLimited`] or
    /// [`RoomError::Exhausted`].
    pub fn send(
        &mut self,
        now: Duration,
        caller: &Caller<'_>,
        participant: ParticipantId,
        epoch: Epoch,
        kind: EventKind,
        data: EventData,
    ) -> Result<EventSeq, RoomError> {
        self.sweep(now);
        let room = self
            .table
            .get_mut(&RoomKey::of(caller))
            .ok_or(RoomError::NotFound)?;
        let index = room
            .find(caller.tab, participant, epoch)
            .ok_or(RoomError::NotFound)?;
        let seq = room
            .head
            .checked_add(1)
            .filter(|seq| *seq <= MAX_COUNTER)
            .ok_or(RoomError::Exhausted)?;
        let member = &mut room.members[index];
        member
            .pacer
            .admit(now, &self.limits)
            .map_err(|retry_after| RoomError::RateLimited { retry_after })?;
        let (from, tab) = (member.id, member.label);
        room.head = seq;
        room.events.push_back(Stored {
            seq: EventSeq(seq),
            from,
            tab,
            kind,
            data,
        });
        while room.events.len() > self.limits.kept_events {
            room.events.pop_front();
        }
        Ok(EventSeq(seq))
    }

    /// Removes a participant at once.
    ///
    /// # Errors
    ///
    /// [`RoomError::NotFound`] when it is not the caller's current one; a
    /// connection that was replaced cannot remove its replacement.
    pub fn leave(
        &mut self,
        now: Duration,
        caller: &Caller<'_>,
        participant: ParticipantId,
        epoch: Epoch,
    ) -> Result<(), RoomError> {
        self.sweep(now);
        let key = RoomKey::of(caller);
        let room = self.table.get_mut(&key).ok_or(RoomError::NotFound)?;
        let index = room
            .find(caller.tab, participant, epoch)
            .ok_or(RoomError::NotFound)?;
        room.members.remove(index);
        room.forget_empty_tabs();
        if room.members.is_empty() {
            self.table.remove(&key);
        }
        Ok(())
    }

    /// Removes every participant in every room of `actor`, as when the user
    /// is disabled (decision 0036).
    pub fn leave_actor(&mut self, actor: &ActorId) {
        self.table.retain(|key, _| key.actor != *actor);
    }

    /// Removes every participant of a tab in every room of `actor`, as when
    /// the tab's connection is closed.
    pub fn leave_tab(&mut self, actor: &ActorId, tab: &TabKey) {
        for (key, room) in &mut self.table {
            if key.actor == *actor {
                room.members.retain(|member| member.tab != *tab);
                room.forget_empty_tabs();
            }
        }
        self.table.retain(|_, room| !room.members.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unwraps a fixture call whose failure would be a bug in the test.
    trait Ready<T> {
        fn ready(self) -> T;
    }

    impl<T, E: fmt::Debug> Ready<T> for Result<T, E> {
        fn ready(self) -> T {
            self.unwrap_or_else(|error| panic!("fixture call failed: {error:?}"))
        }
    }

    fn actor() -> ActorId {
        "local".parse().ready()
    }

    fn artifact(digit: char) -> ArtifactId {
        format!("art_{}", digit.to_string().repeat(32))
            .parse()
            .ready()
    }

    fn at(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    fn slot(name: &str) -> Slot {
        name.parse().ready()
    }

    fn kind() -> EventKind {
        "cursor".parse().ready()
    }

    fn data(text: &str) -> EventData {
        text.parse().ready()
    }

    /// One tab of the fixture actor in the fixture artifact.
    struct Tab {
        actor: ActorId,
        artifact: ArtifactId,
        key: TabKey,
    }

    impl Tab {
        fn new(connection: &str) -> Self {
            Self {
                actor: actor(),
                artifact: artifact('a'),
                key: TabKey::new(connection),
            }
        }

        fn in_artifact(mut self, digit: char) -> Self {
            self.artifact = artifact(digit);
            self
        }

        fn caller(&self) -> Caller<'_> {
            Caller {
                actor: &self.actor,
                artifact: &self.artifact,
                tab: &self.key,
            }
        }
    }

    fn rooms() -> Rooms {
        Rooms::new(Limits::STANDARD)
    }

    fn join(rooms: &mut Rooms, now: u64, tab: &Tab, name: &str) -> Snapshot {
        rooms.join(at(now), &tab.caller(), slot(name), None).ready()
    }

    #[test]
    fn is_me_and_same_tab_follow_the_server_known_tab_of_each_participant() {
        let mut rooms = rooms();
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let page = join(&mut rooms, 0, &one, "page");
        let frame = join(&mut rooms, 0, &one, "frame");
        let other = join(&mut rooms, 0, &two, "page");
        // The same slot name in another tab is another participant.
        assert_ne!(other.participant, page.participant);
        assert_ne!(page.tab, other.tab);
        assert_eq!(page.tab, frame.tab);

        // (viewer, member) -> (is_me, same_tab), the whole truth table.
        let cases = [
            (&one, &page, [(true, true), (false, true), (false, false)]),
            (&one, &frame, [(false, true), (true, true), (false, false)]),
            (&two, &other, [(false, false), (false, false), (true, true)]),
        ];
        for (tab, who, expected) in cases {
            let view = rooms
                .heartbeat(
                    at(1),
                    &tab.caller(),
                    who.participant,
                    who.epoch,
                    EventSeq(0),
                    None,
                )
                .ready();
            let flags: Vec<_> = view
                .members
                .iter()
                .map(|member| (member.is_me, member.same_tab))
                .collect();
            assert_eq!(flags, expected, "viewer {:?}", who.participant);
            let ids: Vec<_> = view
                .members
                .iter()
                .map(|member| member.participant)
                .collect();
            assert_eq!(
                ids,
                [page.participant, frame.participant, other.participant],
                "members are listed by participant"
            );
        }
    }

    #[test]
    fn events_carry_the_same_flags_and_a_sender_reads_its_own() {
        let mut rooms = rooms();
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let page = join(&mut rooms, 0, &one, "page");
        let frame = join(&mut rooms, 0, &one, "frame");
        let other = join(&mut rooms, 0, &two, "page");
        let sent = rooms
            .send(
                at(1),
                &one.caller(),
                frame.participant,
                frame.epoch,
                kind(),
                data("x"),
            )
            .ready();
        let read = |rooms: &mut Rooms, tab: &Tab, who: &Snapshot| {
            rooms
                .heartbeat(
                    at(2),
                    &tab.caller(),
                    who.participant,
                    who.epoch,
                    EventSeq(0),
                    None,
                )
                .ready()
                .events
        };
        let flags = |events: &[EventView]| -> Vec<(bool, bool)> {
            events
                .iter()
                .map(|event| (event.is_me, event.same_tab))
                .collect()
        };
        assert_eq!(flags(&read(&mut rooms, &one, &frame)), [(true, true)]);
        assert_eq!(flags(&read(&mut rooms, &one, &page)), [(false, true)]);
        assert_eq!(flags(&read(&mut rooms, &two, &other)), [(false, false)]);
        let events = read(&mut rooms, &two, &other);
        assert_eq!(events[0].seq, sent);
        assert_eq!(events[0].participant, frame.participant);
        assert_eq!(events[0].data.as_str(), "x");
    }

    #[test]
    fn a_lease_ends_exactly_when_it_runs_out_and_a_heartbeat_extends_it() {
        let mut rooms = rooms();
        let tab = Tab::new("tab-one");
        let me = join(&mut rooms, 1_000, &tab, "page");
        // 14.999 s after the join the lease (15 s) still holds.
        let renewed = rooms
            .heartbeat(
                at(15_999),
                &tab.caller(),
                me.participant,
                me.epoch,
                EventSeq(0),
                None,
            )
            .ready();
        assert_eq!(renewed.lease, Duration::from_secs(15));
        // The renewal started at 15.999 s, so 30.998 s is still inside it...
        assert!(
            rooms
                .heartbeat(
                    at(30_998),
                    &tab.caller(),
                    me.participant,
                    me.epoch,
                    EventSeq(0),
                    None
                )
                .is_ok()
        );
        // ...and the lease ends exactly 15 s after the last renewal.
        assert_eq!(
            rooms
                .heartbeat(
                    at(45_998),
                    &tab.caller(),
                    me.participant,
                    me.epoch,
                    EventSeq(0),
                    None
                )
                .err(),
            Some(RoomError::NotFound)
        );
        assert_eq!(
            rooms.counts(),
            Counts {
                rooms: 0,
                participants: 0
            }
        );
    }

    #[test]
    fn an_expired_participant_is_not_listed_to_the_others() {
        let mut rooms = rooms();
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let gone = join(&mut rooms, 0, &one, "page");
        let kept = join(&mut rooms, 0, &two, "page");
        rooms
            .heartbeat(
                at(10_000),
                &two.caller(),
                kept.participant,
                kept.epoch,
                EventSeq(0),
                None,
            )
            .ready();
        let view = rooms
            .heartbeat(
                at(15_000),
                &two.caller(),
                kept.participant,
                kept.epoch,
                EventSeq(0),
                None,
            )
            .ready();
        let ids: Vec<_> = view
            .members
            .iter()
            .map(|member| member.participant)
            .collect();
        assert_eq!(ids, [kept.participant], "{gone:?} expired at 15 s");
    }

    #[test]
    fn rejoining_with_a_live_lease_resumes_without_a_second_entry() {
        let mut rooms = rooms();
        let tab = Tab::new("tab-one");
        let first = join(&mut rooms, 0, &tab, "page");
        let again = join(&mut rooms, 5_000, &tab, "page");
        assert_eq!(again.participant, first.participant, "the same participant");
        assert!(again.epoch > first.epoch, "a newer epoch");
        assert_eq!(again.members.len(), 1, "no duplicate in the list");
        assert_eq!(
            rooms.counts(),
            Counts {
                rooms: 1,
                participants: 1
            }
        );
        // The lease restarted at the rejoin.
        assert!(
            rooms
                .heartbeat(
                    at(19_999),
                    &tab.caller(),
                    again.participant,
                    again.epoch,
                    EventSeq(0),
                    None
                )
                .is_ok()
        );
    }

    #[test]
    fn the_replaced_connection_can_no_longer_act() {
        let mut rooms = rooms();
        let tab = Tab::new("tab-one");
        let old = join(&mut rooms, 0, &tab, "page");
        let new = join(&mut rooms, 1, &tab, "page");
        let stale = |rooms: &mut Rooms| {
            (
                rooms
                    .heartbeat(
                        at(2),
                        &tab.caller(),
                        old.participant,
                        old.epoch,
                        EventSeq(0),
                        None,
                    )
                    .err(),
                rooms
                    .send(
                        at(2),
                        &tab.caller(),
                        old.participant,
                        old.epoch,
                        kind(),
                        data("x"),
                    )
                    .err(),
                rooms
                    .leave(at(2), &tab.caller(), old.participant, old.epoch)
                    .err(),
            )
        };
        assert_eq!(
            stale(&mut rooms),
            (
                Some(RoomError::NotFound),
                Some(RoomError::NotFound),
                Some(RoomError::NotFound)
            )
        );
        // The refused calls changed nothing for the new connection.
        assert!(
            rooms
                .heartbeat(
                    at(3),
                    &tab.caller(),
                    new.participant,
                    new.epoch,
                    EventSeq(0),
                    None
                )
                .is_ok()
        );
        assert_eq!(rooms.counts().participants, 1);
    }

    #[test]
    fn rejoining_after_the_lease_ran_out_is_a_new_participant() {
        let mut rooms = rooms();
        let tab = Tab::new("tab-one");
        let first = join(&mut rooms, 0, &tab, "page");
        let later = join(&mut rooms, 15_000, &tab, "page");
        assert_ne!(later.participant, first.participant);
        assert_eq!(later.members.len(), 1);
        assert_eq!(
            rooms
                .heartbeat(
                    at(15_001),
                    &tab.caller(),
                    first.participant,
                    first.epoch,
                    EventSeq(0),
                    None
                )
                .err(),
            Some(RoomError::NotFound),
            "the old participant stays gone"
        );
    }

    #[test]
    fn another_tab_cannot_act_as_a_participant_it_does_not_hold() {
        let mut rooms = rooms();
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let mine = join(&mut rooms, 0, &one, "page");
        join(&mut rooms, 0, &two, "page");
        let attempts = (
            rooms
                .heartbeat(
                    at(1),
                    &two.caller(),
                    mine.participant,
                    mine.epoch,
                    EventSeq(0),
                    None,
                )
                .err(),
            rooms
                .send(
                    at(1),
                    &two.caller(),
                    mine.participant,
                    mine.epoch,
                    kind(),
                    data("x"),
                )
                .err(),
            rooms
                .leave(at(1), &two.caller(), mine.participant, mine.epoch)
                .err(),
        );
        assert_eq!(
            attempts,
            (
                Some(RoomError::NotFound),
                Some(RoomError::NotFound),
                Some(RoomError::NotFound)
            )
        );
        assert_eq!(rooms.counts().participants, 2, "nothing was removed");
        // A different artifact's room is another room even for the same tab.
        let elsewhere = Tab::new("tab-one").in_artifact('b');
        assert_eq!(
            rooms
                .heartbeat(
                    at(1),
                    &elsewhere.caller(),
                    mine.participant,
                    mine.epoch,
                    EventSeq(0),
                    None
                )
                .err(),
            Some(RoomError::NotFound)
        );
    }

    #[test]
    fn an_actor_has_its_own_rooms() {
        let mut rooms = rooms();
        let tab = Tab::new("tab-one");
        let mine = join(&mut rooms, 0, &tab, "page");
        let other_actor: ActorId = "someone".parse().ready();
        let impostor = Caller {
            actor: &other_actor,
            artifact: &tab.artifact,
            tab: &tab.key,
        };
        assert_eq!(
            rooms
                .heartbeat(
                    at(1),
                    &impostor,
                    mine.participant,
                    mine.epoch,
                    EventSeq(0),
                    None
                )
                .err(),
            Some(RoomError::NotFound)
        );
        let own = rooms.join(at(1), &impostor, slot("page"), None).ready();
        assert_eq!(
            own.members.len(),
            1,
            "it does not see the first actor's member"
        );
        assert_eq!(
            rooms.counts(),
            Counts {
                rooms: 2,
                participants: 2
            }
        );
    }

    #[test]
    fn an_event_is_not_replayed_to_a_late_joiner_whatever_position_it_asks_for() {
        let mut rooms = rooms();
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let early = join(&mut rooms, 0, &one, "page");
        rooms
            .send(
                at(1),
                &one.caller(),
                early.participant,
                early.epoch,
                kind(),
                data("before"),
            )
            .ready();
        let late = join(&mut rooms, 2, &two, "page");
        assert!(late.events.is_empty(), "the join returns no events");
        assert_eq!(late.head, EventSeq(1));
        // It asks to read from the start; the room still holds the event.
        let asked = rooms
            .heartbeat(
                at(3),
                &two.caller(),
                late.participant,
                late.epoch,
                EventSeq(0),
                None,
            )
            .ready();
        assert!(asked.events.is_empty(), "{:?}", asked.events);
        assert!(!asked.missed, "nothing it was owed was dropped");
        rooms
            .send(
                at(4),
                &one.caller(),
                early.participant,
                early.epoch,
                kind(),
                data("after"),
            )
            .ready();
        let next = rooms
            .heartbeat(
                at(5),
                &two.caller(),
                late.participant,
                late.epoch,
                EventSeq(0),
                None,
            )
            .ready();
        let texts: Vec<_> = next
            .events
            .iter()
            .map(|event| event.data.as_str())
            .collect();
        assert_eq!(texts, ["after"], "only what came after the join");
    }

    #[test]
    fn a_resumed_participant_still_reads_what_came_while_it_was_away() {
        let mut rooms = rooms();
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let sender = join(&mut rooms, 0, &one, "page");
        let reader = join(&mut rooms, 0, &two, "page");
        rooms
            .send(
                at(1),
                &one.caller(),
                sender.participant,
                sender.epoch,
                kind(),
                data("gap"),
            )
            .ready();
        // The reader reconnects before its lease ends and reads from its
        // last position (0): ephemeral, but still in the room.
        let resumed = join(&mut rooms, 2_000, &two, "page");
        assert_eq!(resumed.participant, reader.participant);
        let view = rooms
            .heartbeat(
                at(2_001),
                &two.caller(),
                resumed.participant,
                resumed.epoch,
                EventSeq(0),
                None,
            )
            .ready();
        let texts: Vec<_> = view
            .events
            .iter()
            .map(|event| event.data.as_str())
            .collect();
        assert_eq!(texts, ["gap"]);
    }

    fn send_text(rooms: &mut Rooms, now: u64, tab: &Tab, from: &Snapshot, text: &str) {
        rooms
            .send(
                at(now),
                &tab.caller(),
                from.participant,
                from.epoch,
                kind(),
                data(text),
            )
            .ready();
    }

    fn texts(view: &Snapshot) -> Vec<&str> {
        view.events
            .iter()
            .map(|event| event.data.as_str())
            .collect()
    }

    #[test]
    fn a_resumed_join_delivers_what_came_after_the_position_its_participant_reported() {
        let mut rooms = rooms();
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let sender = join(&mut rooms, 0, &one, "page");
        let reader = join(&mut rooms, 0, &two, "page");
        send_text(&mut rooms, 1, &one, &sender, "read");
        let seen = rooms
            .heartbeat(
                at(2),
                &two.caller(),
                reader.participant,
                reader.epoch,
                EventSeq(0),
                None,
            )
            .ready();
        assert_eq!(texts(&seen), ["read"]);
        // The reader reports having read event 1, then goes away; two more
        // events arrive.
        rooms
            .heartbeat(
                at(3),
                &two.caller(),
                reader.participant,
                reader.epoch,
                EventSeq(1),
                None,
            )
            .ready();
        send_text(&mut rooms, 4, &one, &sender, "away-1");
        send_text(&mut rooms, 1_000, &one, &sender, "away-2");
        let resumed = join(&mut rooms, 2_000, &two, "page");
        assert_eq!(resumed.participant, reader.participant);
        assert_eq!(texts(&resumed), ["away-1", "away-2"]);
        assert_eq!(resumed.head, EventSeq(3));
        assert!(!resumed.missed, "nothing was dropped");
    }

    #[test]
    fn a_position_reported_once_never_goes_back() {
        let mut rooms = rooms();
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let sender = join(&mut rooms, 0, &one, "page");
        let reader = join(&mut rooms, 0, &two, "page");
        send_text(&mut rooms, 1, &one, &sender, "a");
        send_text(&mut rooms, 1_000, &one, &sender, "b");
        for after in [2, 1] {
            rooms
                .heartbeat(
                    at(2_000),
                    &two.caller(),
                    reader.participant,
                    reader.epoch,
                    EventSeq(after),
                    None,
                )
                .ready();
        }
        send_text(&mut rooms, 3_000, &one, &sender, "c");
        let resumed = join(&mut rooms, 4_000, &two, "page");
        assert_eq!(texts(&resumed), ["c"], "from the newest reported position");
    }

    #[test]
    fn a_resumed_join_says_when_the_room_dropped_what_its_participant_had_not_read() {
        let limits = Limits {
            kept_events: 3,
            event_burst: 100,
            ..Limits::STANDARD
        };
        let mut rooms = Rooms::new(limits);
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let sender = join(&mut rooms, 0, &one, "page");
        let reader = join(&mut rooms, 0, &two, "page");
        for round in 0..5 {
            send_text(&mut rooms, round, &one, &sender, "e");
        }
        let resumed = join(&mut rooms, 1_000, &two, "page");
        let seqs: Vec<_> = resumed.events.iter().map(|event| event.seq.0).collect();
        assert_eq!(seqs, [3, 4, 5], "the room kept the newest three");
        assert!(resumed.missed, "events 1 and 2 were dropped unread");
        assert_eq!(resumed.participant, reader.participant);
    }

    #[test]
    fn a_new_participant_after_its_lease_ran_out_gets_no_old_events_in_its_join() {
        let mut rooms = rooms();
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let sender = join(&mut rooms, 0, &one, "page");
        join(&mut rooms, 0, &two, "page");
        send_text(&mut rooms, 1, &one, &sender, "old");
        // The sender keeps its lease alive; the reader's ran out.
        rooms
            .heartbeat(
                at(14_000),
                &one.caller(),
                sender.participant,
                sender.epoch,
                EventSeq(1),
                None,
            )
            .ready();
        let fresh = join(&mut rooms, 15_000, &two, "page");
        assert!(fresh.events.is_empty() && !fresh.missed);
        assert_eq!(fresh.head, EventSeq(1));
    }

    #[test]
    fn a_position_ahead_of_the_room_is_refused() {
        let mut rooms = rooms();
        let tab = Tab::new("tab-one");
        let me = join(&mut rooms, 0, &tab, "page");
        assert_eq!(
            rooms
                .heartbeat(
                    at(1),
                    &tab.caller(),
                    me.participant,
                    me.epoch,
                    EventSeq(1),
                    None
                )
                .err(),
            Some(RoomError::CursorAhead)
        );
    }

    #[test]
    fn dropped_events_are_reported_not_hidden() {
        let limits = Limits {
            kept_events: 3,
            event_burst: 100,
            ..Limits::STANDARD
        };
        let mut rooms = Rooms::new(limits);
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let sender = join(&mut rooms, 0, &one, "page");
        let reader = join(&mut rooms, 0, &two, "page");
        for round in 0..5 {
            rooms
                .send(
                    at(round),
                    &one.caller(),
                    sender.participant,
                    sender.epoch,
                    kind(),
                    data("e"),
                )
                .ready();
        }
        let view = rooms
            .heartbeat(
                at(10),
                &two.caller(),
                reader.participant,
                reader.epoch,
                EventSeq(0),
                None,
            )
            .ready();
        let seqs: Vec<_> = view.events.iter().map(|event| event.seq.0).collect();
        assert_eq!(seqs, [3, 4, 5], "the room kept the newest three");
        assert!(view.missed, "events 1 and 2 were dropped unread");
        let caught_up = rooms
            .heartbeat(
                at(11),
                &two.caller(),
                reader.participant,
                reader.epoch,
                EventSeq(2),
                None,
            )
            .ready();
        assert!(!caught_up.missed, "event 3 follows the position 2");
        let none = rooms
            .heartbeat(
                at(12),
                &two.caller(),
                reader.participant,
                reader.epoch,
                EventSeq(5),
                None,
            )
            .ready();
        assert!(none.events.is_empty() && !none.missed);
    }

    #[test]
    fn one_participant_at_its_full_rate_cannot_make_another_miss_an_event() {
        let mut rooms = rooms();
        let (loud_tab, quiet_tab) = (Tab::new("loud"), Tab::new("quiet"));
        let loud = join(&mut rooms, 0, &loud_tab, "page");
        let quiet = join(&mut rooms, 0, &quiet_tab, "page");
        let mut position = EventSeq(0);
        let mut delivered = 0;
        // Twenty seconds in 50 ms steps: the loud tab sends whenever it may,
        // the quiet tab reads every 5 seconds and keeps its lease.
        for step in 1..=400_u64 {
            let now = step * 50;
            let _ = rooms.send(
                at(now),
                &loud_tab.caller(),
                loud.participant,
                loud.epoch,
                kind(),
                data("x"),
            );
            rooms
                .heartbeat(
                    at(now),
                    &loud_tab.caller(),
                    loud.participant,
                    loud.epoch,
                    EventSeq(0),
                    None,
                )
                .ready();
            if now % 5_000 == 0 {
                let view = rooms
                    .heartbeat(
                        at(now),
                        &quiet_tab.caller(),
                        quiet.participant,
                        quiet.epoch,
                        position,
                        None,
                    )
                    .ready();
                assert!(!view.missed, "at {now} ms");
                delivered += view.events.len();
                position = view.head;
            }
        }
        // The n-th event (from 0) is allowed at 50 + 250 n - 1750 ms, so by
        // 20 s the loud tab sent 87: a burst of 8, then 4 a second.
        assert_eq!(delivered, 87);
    }

    #[test]
    fn a_participant_may_send_a_burst_then_the_sustained_rate() {
        let mut rooms = rooms();
        let tab = Tab::new("tab-one");
        let me = join(&mut rooms, 0, &tab, "page");
        let send = |rooms: &mut Rooms, now| {
            rooms.send(
                at(now),
                &tab.caller(),
                me.participant,
                me.epoch,
                kind(),
                data("x"),
            )
        };
        for _ in 0..8 {
            assert!(send(&mut rooms, 1_000).is_ok());
        }
        assert_eq!(
            send(&mut rooms, 1_000).err(),
            Some(RoomError::RateLimited {
                retry_after: at(250)
            })
        );
        assert!(send(&mut rooms, 1_249).is_err(), "just before the interval");
        assert!(send(&mut rooms, 1_250).is_ok(), "one interval later");
        let kept = rooms
            .heartbeat(
                at(1_251),
                &tab.caller(),
                me.participant,
                me.epoch,
                EventSeq(0),
                None,
            )
            .ready();
        assert_eq!(kept.head, EventSeq(9), "a refused event takes no position");
    }

    #[test]
    fn the_pace_of_one_participant_does_not_limit_another() {
        let mut rooms = rooms();
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let first = join(&mut rooms, 0, &one, "page");
        let second = join(&mut rooms, 0, &two, "page");
        for _ in 0..8 {
            rooms
                .send(
                    at(1),
                    &one.caller(),
                    first.participant,
                    first.epoch,
                    kind(),
                    data("x"),
                )
                .ready();
        }
        assert!(
            rooms
                .send(
                    at(1),
                    &one.caller(),
                    first.participant,
                    first.epoch,
                    kind(),
                    data("x")
                )
                .is_err()
        );
        assert!(
            rooms
                .send(
                    at(1),
                    &two.caller(),
                    second.participant,
                    second.epoch,
                    kind(),
                    data("x")
                )
                .is_ok()
        );
    }

    #[test]
    fn participants_per_room_per_tab_and_rooms_are_bounded() {
        let limits = Limits {
            max_rooms: 2,
            max_participants: 3,
            max_per_tab: 2,
            ..Limits::STANDARD
        };
        let mut rooms = Rooms::new(limits);
        let (one, two, three) = (Tab::new("one"), Tab::new("two"), Tab::new("three"));
        join(&mut rooms, 0, &one, "a");
        join(&mut rooms, 0, &one, "b");
        assert_eq!(
            rooms.join(at(0), &one.caller(), slot("c"), None).err(),
            Some(RoomError::TabFull),
            "a third participant of one tab"
        );
        join(&mut rooms, 0, &two, "a");
        assert_eq!(
            rooms.join(at(0), &three.caller(), slot("a"), None).err(),
            Some(RoomError::RoomFull),
            "a fourth participant of the room"
        );
        // A resume is not a new participant, so a full room still resumes.
        assert!(rooms.join(at(1), &one.caller(), slot("a"), None).is_ok());
        let second_room = Tab::new("one").in_artifact('b');
        join(&mut rooms, 0, &second_room, "a");
        let third_room = Tab::new("one").in_artifact('c');
        assert_eq!(
            rooms
                .join(at(0), &third_room.caller(), slot("a"), None)
                .err(),
            Some(RoomError::TooManyRooms)
        );
        assert_eq!(
            rooms.counts(),
            Counts {
                rooms: 2,
                participants: 4
            }
        );
        // Expired rooms free their places: the last renewal was at 1 ms.
        assert!(
            rooms
                .join(at(15_001), &third_room.caller(), slot("a"), None)
                .is_ok()
        );
        assert_eq!(
            rooms.counts(),
            Counts {
                rooms: 1,
                participants: 1
            }
        );
    }

    fn other_actor() -> ActorId {
        "u-0123456789abcdef0123456789abcdef".parse().ready()
    }

    /// A tab of another actor, in the artifact `digit`.
    fn tab_of(actor: ActorId, connection: &str, digit: char) -> Tab {
        Tab {
            actor,
            ..Tab::new(connection).in_artifact(digit)
        }
    }

    #[test]
    fn the_room_cap_is_per_actor_and_one_actors_rooms_cannot_fill_anothers() {
        let limits = Limits {
            max_rooms: 1,
            ..Limits::STANDARD
        };
        let mut rooms = Rooms::new(limits);
        let mine = tab_of(actor(), "mine", 'a');
        let theirs = tab_of(other_actor(), "theirs", 'b');
        join(&mut rooms, 0, &mine, "page");
        // Another room of the same actor is refused, and another actor's
        // first room is not.
        let second = tab_of(actor(), "mine", 'c');
        assert_eq!(
            rooms
                .join(at(0), &second.caller(), slot("page"), None)
                .err(),
            Some(RoomError::TooManyRooms)
        );
        assert!(
            rooms
                .join(at(0), &theirs.caller(), slot("page"), None)
                .is_ok()
        );
        assert_eq!(rooms.counts().rooms, 2);
    }

    #[test]
    fn an_actor_at_the_room_cap_can_join_again_after_its_rooms_are_removed() {
        let limits = Limits {
            max_rooms: 1,
            ..Limits::STANDARD
        };
        let mut rooms = Rooms::new(limits);
        let mine = tab_of(actor(), "mine", 'a');
        join(&mut rooms, 0, &mine, "page");
        let second = tab_of(actor(), "mine", 'c');
        assert_eq!(
            rooms
                .join(at(0), &second.caller(), slot("page"), None)
                .err(),
            Some(RoomError::TooManyRooms)
        );

        rooms.leave_actor(&actor());

        assert!(
            rooms
                .join(at(0), &second.caller(), slot("page"), None)
                .is_ok(),
            "a user enabled again must find its places free"
        );
    }

    #[test]
    fn each_actor_counts_its_own_participants_and_epochs() {
        let mut rooms = rooms();
        let (mine, theirs) = (
            tab_of(actor(), "mine", 'a'),
            tab_of(other_actor(), "theirs", 'b'),
        );
        for _ in 0..3 {
            join(&mut rooms, 0, &mine, "page");
        }
        let first = join(&mut rooms, 0, &theirs, "page");
        // Joining again resumes and takes a new epoch for the same actor
        // only: the other actor's numbers started at 1 and stay its own.
        assert_eq!(
            (first.participant, first.epoch, first.tab),
            (ParticipantId(1), Epoch(1), TabLabel(1))
        );
        let again = join(&mut rooms, 0, &mine, "page");
        assert!(again.epoch > Epoch(1));
        let still = join(&mut rooms, 0, &theirs, "page");
        assert_eq!(
            still.epoch,
            Epoch(2),
            "another actor's joins moved this actor's epoch"
        );
    }

    #[test]
    fn leaving_an_actor_removes_its_rooms_and_no_one_elses() {
        let mut rooms = rooms();
        let (mine, theirs) = (
            tab_of(actor(), "mine", 'a'),
            tab_of(other_actor(), "theirs", 'a'),
        );
        join(&mut rooms, 0, &mine, "page");
        join(&mut rooms, 0, &theirs, "page");
        assert_eq!(rooms.counts().rooms, 2);
        rooms.leave_actor(&actor());
        assert_eq!(
            rooms.counts(),
            Counts {
                rooms: 1,
                participants: 1
            }
        );
        let view = rooms
            .heartbeat(
                at(1),
                &theirs.caller(),
                ParticipantId(1),
                Epoch(1),
                EventSeq(0),
                None,
            )
            .ready();
        assert_eq!(view.members.len(), 1);
    }

    #[test]
    fn memory_is_bounded_by_the_limits_whatever_the_traffic() {
        let limits = Limits {
            kept_events: 4,
            event_burst: 1_000,
            ..Limits::STANDARD
        };
        let mut rooms = Rooms::new(limits);
        let tab = Tab::new("tab-one");
        let me = join(&mut rooms, 0, &tab, "page");
        for round in 0..1_000 {
            rooms
                .send(
                    at(round),
                    &tab.caller(),
                    me.participant,
                    me.epoch,
                    kind(),
                    data("x"),
                )
                .ready();
        }
        let view = rooms
            .heartbeat(
                at(1_000),
                &tab.caller(),
                me.participant,
                me.epoch,
                EventSeq(0),
                None,
            )
            .ready();
        assert_eq!(view.events.len(), 4, "at most the kept events are held");
        assert_eq!(view.head, EventSeq(1_000));
    }

    #[test]
    fn a_join_state_and_a_heartbeat_state_replace_and_none_keeps() {
        let mut rooms = rooms();
        let tab = Tab::new("tab-one");
        let me = rooms
            .join(
                at(0),
                &tab.caller(),
                slot("page"),
                Some("editing".parse().ready()),
            )
            .ready();
        assert_eq!(me.members[0].state.as_str(), "editing");
        let kept = rooms
            .heartbeat(
                at(1),
                &tab.caller(),
                me.participant,
                me.epoch,
                EventSeq(0),
                None,
            )
            .ready();
        assert_eq!(kept.members[0].state.as_str(), "editing");
        let changed = rooms
            .heartbeat(
                at(2),
                &tab.caller(),
                me.participant,
                me.epoch,
                EventSeq(0),
                Some("reading".parse().ready()),
            )
            .ready();
        assert_eq!(changed.members[0].state.as_str(), "reading");
        let rejoined = join(&mut rooms, 3, &tab, "page");
        assert_eq!(
            rejoined.members[0].state.as_str(),
            "",
            "a join without a state empties it"
        );
    }

    #[test]
    fn leaving_removes_the_participant_and_an_empty_room_disappears() {
        let mut rooms = rooms();
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let first = join(&mut rooms, 0, &one, "page");
        let second = join(&mut rooms, 0, &two, "page");
        rooms
            .leave(at(1), &one.caller(), first.participant, first.epoch)
            .ready();
        assert_eq!(
            rooms
                .leave(at(1), &one.caller(), first.participant, first.epoch)
                .err(),
            Some(RoomError::NotFound)
        );
        assert_eq!(
            rooms.counts(),
            Counts {
                rooms: 1,
                participants: 1
            }
        );
        rooms
            .leave(at(2), &two.caller(), second.participant, second.epoch)
            .ready();
        assert_eq!(
            rooms.counts(),
            Counts {
                rooms: 0,
                participants: 0
            }
        );
    }

    #[test]
    fn closing_a_tab_removes_all_its_participants_in_every_room_of_the_actor() {
        let mut rooms = rooms();
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let elsewhere = Tab::new("tab-one").in_artifact('b');
        join(&mut rooms, 0, &one, "page");
        join(&mut rooms, 0, &one, "frame");
        join(&mut rooms, 0, &elsewhere, "page");
        let kept = join(&mut rooms, 0, &two, "page");
        rooms.leave_tab(&actor(), &one.key);
        assert_eq!(
            rooms.counts(),
            Counts {
                rooms: 1,
                participants: 1
            }
        );
        assert!(
            rooms
                .heartbeat(
                    at(1),
                    &two.caller(),
                    kept.participant,
                    kept.epoch,
                    EventSeq(0),
                    None
                )
                .is_ok()
        );
        // Another actor's tab with the same key is not touched.
        let other: ActorId = "someone".parse().ready();
        let theirs = Caller {
            actor: &other,
            artifact: &two.artifact,
            tab: &two.key,
        };
        rooms.join(at(1), &theirs, slot("page"), None).ready();
        rooms.leave_tab(&actor(), &two.key);
        assert_eq!(
            rooms.counts(),
            Counts {
                rooms: 1,
                participants: 1
            }
        );
    }

    #[test]
    fn counters_stop_at_the_largest_safe_number() {
        let mut rooms = rooms();
        rooms.counters.entry(actor()).or_default().participant = MAX_COUNTER;
        let (one, two) = (Tab::new("tab-one"), Tab::new("tab-two"));
        let last = join(&mut rooms, 0, &one, "page");
        assert_eq!(last.participant, ParticipantId(MAX_COUNTER));
        assert_eq!(
            rooms.join(at(0), &two.caller(), slot("page"), None).err(),
            Some(RoomError::Exhausted)
        );
        assert_eq!(
            rooms.counts().participants,
            1,
            "the refused join added nothing"
        );
    }

    #[test]
    fn the_debug_form_of_a_tab_does_not_show_its_credential() {
        let shown = format!("{:?}", TabKey::new("secret-connection-id"));
        assert!(!shown.contains("secret"), "{shown}");
    }
}
