import type {
  AccessChangeResponse,
  AccessMember,
  AccessUser,
  ArtifactAccess,
  Me,
  MemberRole,
} from '@artifree/contracts';
import type { ArtifreeClient, MutationOptions, ReadOptions } from './client.js';
import { isCount, isRecord, list } from './responses.js';

/**
 * The access reads of decision 0060 rule 6: functions, not methods of the
 * client class, for the same reason as the user management functions. They
 * read who the caller is and who may reach an artifact, and let its owner
 * grant, change and revoke a role.
 */

/** Parses the answer of `GET /v1/me`. */
export function parseMe(value: unknown): Me | undefined {
  if (!isRecord(value)) return undefined;
  const { id, label } = value;
  if (typeof id !== 'string' || typeof label !== 'string') return undefined;
  return { id, label };
}

function parseAccessUser(value: unknown): AccessUser | undefined {
  return parseMe(value);
}

function parseAccessMember(value: unknown): AccessMember | undefined {
  if (!isRecord(value)) return undefined;
  const { id, label, role, disabled } = value;
  if (
    typeof id !== 'string' ||
    typeof label !== 'string' ||
    (role !== 'editor' && role !== 'viewer') ||
    // Always present: a boolean for the owner, null for a member.
    (disabled !== null && typeof disabled !== 'boolean')
  )
    return undefined;
  return { id, label, role, disabled };
}

/** Parses the answer of `GET /v1/artifacts/{id}/access`. */
export function parseArtifactAccess(
  value: unknown,
): ArtifactAccess | undefined {
  if (!isRecord(value)) return undefined;
  const { artifact_id, owner, members, access_version } = value;
  const parsedOwner = parseAccessUser(owner);
  const parsedMembers = list(members, parseAccessMember);
  if (
    typeof artifact_id !== 'string' ||
    parsedOwner === undefined ||
    parsedMembers === undefined ||
    !isCount(access_version)
  )
    return undefined;
  return {
    artifact_id,
    owner: parsedOwner,
    members: parsedMembers,
    access_version,
  };
}

/** Reads the caller's id and label, to hand the id to an owner. */
export async function readMe(
  client: ArtifreeClient,
  options: ReadOptions = {},
): Promise<Me> {
  return client.json('GET', '/v1/me', parseMe, { signal: options.signal });
}

/**
 * Reads who may reach an artifact: its owner, its members with their roles,
 * and its access version. Only the owner's answer says which members are
 * disabled; a member's answer has `disabled: null` for each.
 */
export async function readAccess(
  client: ArtifreeClient,
  artifactId: string,
  options: ReadOptions = {},
): Promise<ArtifactAccess> {
  return client.json(
    'GET',
    `/v1/artifacts/${encodeURIComponent(artifactId)}/access`,
    (value) => {
      const access = parseArtifactAccess(value);
      return access?.artifact_id === artifactId ? access : undefined;
    },
    { signal: options.signal },
  );
}

/** Parses the answer of a change of access. */
export function parseAccessChange(
  value: unknown,
): AccessChangeResponse | undefined {
  if (!isRecord(value)) return undefined;
  const { artifact_id, user_id, access_version, replayed } = value;
  if (
    typeof artifact_id !== 'string' ||
    typeof user_id !== 'string' ||
    !isCount(access_version) ||
    typeof replayed !== 'boolean'
  )
    return undefined;
  return { artifact_id, user_id, access_version, replayed };
}

function accessPath(artifactId: string, userId: string): string {
  return `/v1/artifacts/${encodeURIComponent(artifactId)}/access/${encodeURIComponent(userId)}`;
}

function sameChange(
  artifactId: string,
  userId: string,
): (value: unknown) => AccessChangeResponse | undefined {
  return (value) => {
    const change = parseAccessChange(value);
    return change?.artifact_id === artifactId && change.user_id === userId
      ? change
      : undefined;
  };
}

/**
 * Grants `userId` `role` on the caller's artifact, or changes the role they
 * have, if the artifact's access is at `baseAccessVersion`. Rejects with
 * `access_conflict` and `currentAccessVersion` when it is not, with
 * `invalid_request` for a user who is not registered, is disabled, or is
 * the owner (one answer for all three), with `forbidden` for a member or a
 * token issued to the owner profile, and with `not_found` for an artifact
 * the caller cannot reach or that is in the trash. A retry with the same key
 * and request resolves with `replayed: true`.
 */
export async function setAccess(
  client: ArtifreeClient,
  artifactId: string,
  userId: string,
  request: { role: MemberRole; baseAccessVersion: number },
  options: MutationOptions,
): Promise<AccessChangeResponse> {
  return client.json(
    'PUT',
    accessPath(artifactId, userId),
    sameChange(artifactId, userId),
    {
      mutation: {
        body: {
          role: request.role,
          base_access_version: request.baseAccessVersion,
        },
        idempotencyKey: options.idempotencyKey,
      },
    },
  );
}

/**
 * Ends `userId`'s membership of the caller's artifact, if its access is at
 * `baseAccessVersion`. Rejects as {@link setAccess} does, and with
 * `not_found` for a user who is not a member.
 */
export async function revokeAccess(
  client: ArtifreeClient,
  artifactId: string,
  userId: string,
  { baseAccessVersion }: { baseAccessVersion: number },
  options: MutationOptions,
): Promise<AccessChangeResponse> {
  return client.json(
    'DELETE',
    accessPath(artifactId, userId),
    sameChange(artifactId, userId),
    {
      mutation: {
        body: { base_access_version: baseAccessVersion },
        idempotencyKey: options.idempotencyKey,
      },
    },
  );
}
