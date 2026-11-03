import type { RoomEvent, RoomMember, RoomSnapshot } from '@artifree/contracts';
import type { ArtifreeClient } from './client.js';
import { ArtifreeApiError } from './errors.js';
import { isCount, isRecord } from './responses.js';
import type { FrameTransport } from './runtime-source.js';

/**
 * Rooms and presence (decision 0034). A room belongs to one artifact and
 * lives in the server's memory: it lists who is looking now and carries
 * short-lived events. Nothing in it is stored, and an event is delivered to
 * those present who read before the room drops it, never to anyone else.
 *
 * Calls go through the client's runtime connection, which is the tab: the
 * server decides `isMe` and `sameTab` from it, and a response that disagrees
 * with the numbers it states is refused here. This module is loaded on
 * demand and is not part of the client class.
 */

/** Largest members of a room, from the server's documented bounds. */
const MAX_MEMBERS = 16;
/** Largest events one response carries. */
const MAX_EVENTS = 128;
/** The longest lease this client believes, in milliseconds. */
const MAX_LEASE_MS = 3_600_000;
const MAX_STATE_BYTES = 256;
const MAX_DATA_BYTES = 512;
const LABEL = /^[A-Za-z0-9._-]{1,32}$/;

/** Names a participant in calls after its join. */
export interface RoomIdentity {
  readonly artifactId: string;
  readonly participant: number;
  readonly epoch: number;
}

/** The participant a snapshot names, to pass to later calls. */
export function identityOf(
  artifactId: string,
  snapshot: Pick<RoomSnapshot, 'participant' | 'epoch'>,
): RoomIdentity {
  return {
    artifactId,
    participant: snapshot.participant,
    epoch: snapshot.epoch,
  };
}

function invalid(message: string): ArtifreeApiError {
  return new ArtifreeApiError(message, { status: 0, code: 'invalid_request' });
}

function positive(value: unknown): value is number {
  return isCount(value) && value >= 1;
}

function fits(value: unknown, bytes: number): value is string {
  return (
    typeof value === 'string' && new TextEncoder().encode(value).length <= bytes
  );
}

function parseMember(value: unknown): RoomMember | undefined {
  if (!isRecord(value)) return undefined;
  const { participant, tab, actor, is_me, same_tab, state } = value;
  if (
    !positive(participant) ||
    !positive(tab) ||
    typeof actor !== 'string' ||
    actor === '' ||
    typeof is_me !== 'boolean' ||
    typeof same_tab !== 'boolean' ||
    !fits(state, MAX_STATE_BYTES)
  )
    return undefined;
  return { participant, tab, actor, is_me, same_tab, state };
}

function parseEvent(value: unknown): RoomEvent | undefined {
  if (!isRecord(value)) return undefined;
  const { seq, participant, tab, is_me, same_tab, kind, data } = value;
  if (
    !positive(seq) ||
    !positive(participant) ||
    !positive(tab) ||
    typeof is_me !== 'boolean' ||
    typeof same_tab !== 'boolean' ||
    typeof kind !== 'string' ||
    !LABEL.test(kind) ||
    !fits(data, MAX_DATA_BYTES)
  )
    return undefined;
  return { seq, participant, tab, is_me, same_tab, kind, data };
}

/** What a response must agree with: the call it answers. */
export interface RoomExpectation {
  /** The participant the call named; absent for a join. */
  readonly participant?: number;
  /** The epoch the call named; absent for a join. */
  readonly epoch?: number;
  /** The position the call said it had read; 0 for a join. */
  readonly after: number;
  /** Whether the call was a join, which returns no events. */
  readonly join: boolean;
}

/**
 * Validates a snapshot against the call it answers. Every number and flag
 * the server states is checked against the others: the viewer is the one
 * member with `is_me`, `same_tab` is exactly equality of tab labels, members
 * are strictly ordered, and events are newer than the position given, in
 * order, and not newer than `head`.
 */
export function parseRoomSnapshot(
  value: unknown,
  expect: RoomExpectation,
): RoomSnapshot | undefined {
  if (!isRecord(value)) return undefined;
  const { participant, epoch, tab, lease_ms, head, missed } = value;
  if (
    !positive(participant) ||
    !positive(epoch) ||
    !positive(tab) ||
    !positive(lease_ms) ||
    lease_ms > MAX_LEASE_MS ||
    !isCount(head) ||
    typeof missed !== 'boolean' ||
    !Array.isArray(value.members) ||
    !Array.isArray(value.events) ||
    value.members.length > MAX_MEMBERS ||
    value.events.length > MAX_EVENTS
  )
    return undefined;
  if (
    (expect.participant !== undefined && participant !== expect.participant) ||
    (expect.epoch !== undefined && epoch !== expect.epoch) ||
    head < expect.after ||
    (expect.join && (value.events.length > 0 || missed))
  )
    return undefined;
  const members: RoomMember[] = [];
  let previous = 0;
  const rawMembers: unknown[] = value.members;
  for (const raw of rawMembers) {
    const member = parseMember(raw);
    if (
      member === undefined ||
      member.participant <= previous ||
      member.is_me !== (member.participant === participant) ||
      member.same_tab !== (member.tab === tab) ||
      (member.is_me && member.tab !== tab)
    )
      return undefined;
    previous = member.participant;
    members.push(member);
  }
  if (members.filter((member) => member.is_me).length !== 1) return undefined;
  const events: RoomEvent[] = [];
  let last = expect.after;
  const rawEvents: unknown[] = value.events;
  for (const raw of rawEvents) {
    const event = parseEvent(raw);
    if (
      event === undefined ||
      event.seq <= last ||
      event.seq > head ||
      event.is_me !== (event.participant === participant) ||
      event.same_tab !== (event.tab === tab)
    )
      return undefined;
    last = event.seq;
    events.push(event);
  }
  return { participant, epoch, tab, lease_ms, members, events, head, missed };
}

function mismatch(call: string): ArtifreeApiError {
  return new ArtifreeApiError(
    `${call} returned a body that does not match the contract.`,
    { status: 200, code: 'invalid_response' },
  );
}

/** Runs `run` on the client's runtime connection, which is the tab. */
async function onTab<T>(
  client: ArtifreeClient,
  run: (frame: FrameTransport, session: string) => Promise<T>,
): Promise<T> {
  const session = client.runtimeSessionId;
  return client.runtimeCall({ session_id: session }, (_checks, frame) =>
    run(frame, session),
  );
}

function roomPath(session: string, artifactId: string): string {
  return `/v1/runtime-sessions/${session}/artifacts/${encodeURIComponent(artifactId)}/room/participants`;
}

function participantPath(session: string, who: RoomIdentity): string {
  return `${roomPath(session, who.artifactId)}/${who.participant}/${who.epoch}`;
}

function checkIdentity(who: RoomIdentity): void {
  if (!positive(who.participant) || !positive(who.epoch))
    throw invalid('A room participant and epoch must be positive integers.');
}

function checkState(state: string | undefined): void {
  if (state !== undefined && !fits(state, MAX_STATE_BYTES))
    throw invalid(`A room state is at most ${MAX_STATE_BYTES} bytes.`);
}

/** Options of a call that can be cancelled. */
export interface RoomCallOptions {
  readonly signal?: AbortSignal;
}

/**
 * Joins the room of `artifactId` as the participant this tab keeps in
 * `slot`, or resumes it: the same slot returns the same participant with a
 * newer epoch, and the connection that held the older epoch can no longer
 * act. After its lease ran out the same call makes a new participant. A
 * join returns no events; read from the returned `head`.
 */
export async function joinRoom(
  client: ArtifreeClient,
  artifactId: string,
  options: RoomCallOptions & { slot: string; state?: string },
): Promise<RoomSnapshot> {
  if (!LABEL.test(options.slot))
    throw invalid('A room slot is 1 to 32 letters, digits, ".", "_" or "-".');
  checkState(options.state);
  return onTab(client, (frame, session) =>
    frame.json(
      roomPath(session, artifactId),
      {
        slot: options.slot,
        ...(options.state === undefined ? {} : { state: options.state }),
      },
      (value) => parseRoomSnapshot(value, { after: 0, join: true }),
      options.signal,
    ),
  );
}

/**
 * Renews the lease and reads the room: the members now and the events after
 * `after`, the `head` of the last response already handled. A `404
 * not_found` means the participant is gone (its lease ran out or a newer
 * connection replaced it): join again.
 */
export async function pollRoom(
  client: ArtifreeClient,
  who: RoomIdentity,
  options: RoomCallOptions & { after: number; state?: string },
): Promise<RoomSnapshot> {
  checkIdentity(who);
  if (!isCount(options.after))
    throw invalid('A room position must be a non-negative safe integer.');
  checkState(options.state);
  return onTab(client, (frame, session) =>
    frame.json(
      `${participantPath(session, who)}/heartbeat`,
      {
        after: options.after,
        ...(options.state === undefined ? {} : { state: options.state }),
      },
      (value) =>
        parseRoomSnapshot(value, {
          participant: who.participant,
          epoch: who.epoch,
          after: options.after,
          join: false,
        }),
      options.signal,
    ),
  );
}

/**
 * Sends one ephemeral event and resolves with its position in the room. That
 * means the room accepted it, not that anyone read it, and a lost answer is
 * not worth retrying: the event may already have been delivered.
 */
export async function sendRoomEvent(
  client: ArtifreeClient,
  who: RoomIdentity,
  event: RoomCallOptions & { kind: string; data: string },
): Promise<number> {
  checkIdentity(who);
  if (!LABEL.test(event.kind))
    throw invalid(
      'A room event kind is 1 to 32 letters, digits, ".", "_" or "-".',
    );
  if (!fits(event.data, MAX_DATA_BYTES))
    throw invalid(`A room event is at most ${MAX_DATA_BYTES} bytes.`);
  return onTab(client, (frame, session) =>
    frame.json(
      `${participantPath(session, who)}/events`,
      { kind: event.kind, data: event.data },
      (value) =>
        isRecord(value) && positive(value.seq) ? value.seq : undefined,
      event.signal,
    ),
  );
}

/** Leaves the room. A participant that is already gone is not an error. */
export async function leaveRoom(
  client: ArtifreeClient,
  who: RoomIdentity,
  options: RoomCallOptions = {},
): Promise<void> {
  checkIdentity(who);
  await onTab(client, async (frame, session) => {
    try {
      const response = await frame.post(
        `${participantPath(session, who)}/leave`,
        {},
        options.signal,
      );
      if (response.status !== 204) throw mismatch('Leaving the room');
    } catch (error: unknown) {
      if (error instanceof ArtifreeApiError && error.code === 'not_found')
        return;
      throw error;
    }
  });
}

/** A snapshot the follower received, and how it came about. */
export interface FollowedRoom {
  readonly snapshot: RoomSnapshot;
  /** True for the first join and for every join after the participant was lost. */
  readonly joined: boolean;
}

/** Options of {@link followRoom}. */
export interface FollowRoomOptions {
  readonly artifactId: string;
  /** The tab's name for this participant; see {@link joinRoom}. */
  readonly slot: string;
  /** What to tell the others, read at each join and heartbeat. */
  readonly state?: () => string;
  /** Stops following and leaves the room; the promise then resolves. */
  readonly signal: AbortSignal;
  /** Receives every snapshot in order, across rejoins. */
  readonly onSnapshot: (followed: FollowedRoom) => void;
  /** Called when a call failed and following continues. */
  readonly onDisconnect?: (error: ArtifreeApiError, retryInMs: number) => void;
  /** Delay between heartbeats; default a fifth of the lease. */
  readonly intervalMs?: number;
  /** Retry delays after a failed call, doubling from `min` to `max`. */
  readonly retryDelayMs?: { readonly min: number; readonly max: number };
  /** Waits; tests replace it. Must resolve early when `signal` aborts. */
  readonly sleep?: (milliseconds: number, signal: AbortSignal) => Promise<void>;
}

function retryable(error: ArtifreeApiError): boolean {
  return (
    error.code === 'network_error' ||
    error.status === 429 ||
    error.status >= 500
  );
}

function wait(milliseconds: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) {
      resolve();
      return;
    }
    const done = (): void => {
      clearTimeout(timer);
      signal.removeEventListener('abort', done);
      resolve();
    };
    const timer = setTimeout(done, milliseconds);
    signal.addEventListener('abort', done);
  });
}

/**
 * Keeps this tab present in a room until `signal` aborts. It joins, then
 * renews the lease every interval and hands over each snapshot. After a
 * failed call it waits and tries the heartbeat again, so a short outage
 * costs nothing: the lease is still alive and the same participant goes on.
 * When the server says the participant is gone (`not_found`, after a longer
 * outage), it joins again with the same slot and says so with `joined`; a
 * join whose answer was lost resumes the same participant instead of adding
 * a second. It rejects on a failure retrying cannot fix, such as a closed
 * runtime connection, a rejected token, or a response that breaks the
 * contract, and on what `onSnapshot` throws. On abort it leaves the room
 * once, ignoring a failure: the lease ends it anyway.
 */
export async function followRoom(
  client: ArtifreeClient,
  options: FollowRoomOptions,
): Promise<void> {
  const { artifactId, slot, signal, onSnapshot } = options;
  const sleep = options.sleep ?? wait;
  const { min, max } = options.retryDelayMs ?? { min: 500, max: 15_000 };
  let current: RoomSnapshot | undefined;
  let attempt = 0;
  while (!signal.aborted) {
    let followed: FollowedRoom | undefined;
    let delay = 0;
    try {
      const state = options.state?.();
      const stateOption = state === undefined ? {} : { state };
      const snapshot =
        current === undefined
          ? await joinRoom(client, artifactId, { slot, signal, ...stateOption })
          : await pollRoom(client, identityOf(artifactId, current), {
              after: current.head,
              signal,
              ...stateOption,
            });
      followed = { snapshot, joined: current === undefined };
      current = snapshot;
      attempt = 0;
      delay = options.intervalMs ?? Math.max(250, snapshot.lease_ms / 5);
    } catch (error: unknown) {
      if (signal.aborted) break;
      if (!(error instanceof ArtifreeApiError)) throw error;
      if (current !== undefined && error.code === 'not_found') {
        // The participant is gone. Join again at once: the next heartbeat
        // is an interval away, so this cannot spin.
        current = undefined;
        continue;
      }
      if (!retryable(error)) throw error;
      delay =
        error.retryAfterSeconds === undefined
          ? Math.min(max, min * 2 ** attempt)
          : error.retryAfterSeconds * 1_000;
      attempt += 1;
      options.onDisconnect?.(error, delay);
    }
    // Outside the try: what the handler throws is never retried.
    if (followed !== undefined) onSnapshot(followed);
    await sleep(delay, signal);
  }
  if (current !== undefined) {
    await leaveRoom(client, identityOf(artifactId, current)).catch(
      () => undefined,
    );
  }
}
