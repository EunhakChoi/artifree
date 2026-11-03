import type {
  CreateUserRequest,
  UserInfo,
  UserList,
} from '@artifree/contracts';
import type { ArtifreeClient, ReadOptions } from './client.js';
import { isRecord, list } from './responses.js';

/**
 * User management (decision 0036): functions, not methods of the client
 * class, because that class is in the web workspace's main bundle, which has
 * no room, and the workspace does not manage users. Every call needs the
 * administrative credential of the local owner profile; a token issued to a
 * user is refused with `403`.
 */

/** Parses a UserInfo object. */
export function parseUserInfo(value: unknown): UserInfo | undefined {
  if (!isRecord(value)) return undefined;
  const { id, label, created_at, disabled_at, is_owner } = value;
  if (
    typeof id !== 'string' ||
    typeof label !== 'string' ||
    typeof created_at !== 'string' ||
    typeof is_owner !== 'boolean' ||
    (disabled_at !== undefined &&
      disabled_at !== null &&
      typeof disabled_at !== 'string')
  )
    return undefined;
  const user: UserInfo = { id, label, created_at, is_owner };
  if (disabled_at !== undefined) user.disabled_at = disabled_at;
  return user;
}

/** Parses a UserList object. */
export function parseUserList(value: unknown): UserList | undefined {
  if (!isRecord(value)) return undefined;
  const users = list(value.users, parseUserInfo);
  return users === undefined ? undefined : { users };
}

/** Lists the registered users, the owner first. */
export async function listUsers(
  client: ArtifreeClient,
  options: ReadOptions = {},
): Promise<UserList> {
  return client.json('GET', '/v1/users', parseUserList, {
    signal: options.signal,
  });
}

/** Registers a user. The user has no token until one is issued for it. */
export async function createUser(
  client: ArtifreeClient,
  request: CreateUserRequest,
  options: ReadOptions = {},
): Promise<UserInfo> {
  return client.json('POST', '/v1/users', parseUserInfo, {
    body: request,
    signal: options.signal,
  });
}

/**
 * Disables a user, or enables it again. A disabled user's tokens are refused
 * and what the server holds open for it is closed; enabling does not bring
 * that back.
 */
export async function setUserDisabled(
  client: ArtifreeClient,
  id: string,
  disabled: boolean,
  options: ReadOptions = {},
): Promise<UserInfo> {
  return client.json(
    'POST',
    `/v1/users/${encodeURIComponent(id)}/${disabled ? 'disable' : 'enable'}`,
    parseUserInfo,
    { signal: options.signal },
  );
}
