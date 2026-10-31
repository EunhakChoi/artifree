import { readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { resolve } from 'node:path';
import { UsageError } from './errors.js';

export interface Profile {
  server?: string;
  token?: string;
  token_file?: string;
}

export interface ProfilesFile {
  default_profile?: string;
  profiles?: Record<string, Profile>;
}

export interface ConnectionOptions {
  server?: string | undefined;
  token?: string | undefined;
  tokenFile?: string | undefined;
  profile?: string | undefined;
  profileFile?: string | undefined;
}

export interface ResolvedConnection {
  server: string;
  token: string;
}

/**
 * Validates the runtime shape of a parsed profiles configuration file.
 * Throws UsageError if invalid types or structures are encountered.
 */
export function validateProfilesFile(
  data: unknown,
  filePath: string,
): ProfilesFile {
  if (typeof data !== 'object' || data === null || Array.isArray(data)) {
    throw new UsageError(
      `invalid profile configuration in ${filePath}: expected root object`,
    );
  }
  const root = data as Record<string, unknown>;
  if ('default_profile' in root && typeof root.default_profile !== 'string') {
    throw new UsageError(
      `invalid profile configuration in ${filePath}: default_profile must be a string`,
    );
  }
  if ('profiles' in root) {
    if (
      typeof root.profiles !== 'object' ||
      root.profiles === null ||
      Array.isArray(root.profiles)
    ) {
      throw new UsageError(
        `invalid profile configuration in ${filePath}: profiles must be an object`,
      );
    }
    const profiles = root.profiles as Record<string, unknown>;
    for (const [name, prof] of Object.entries(profiles)) {
      if (typeof prof !== 'object' || prof === null || Array.isArray(prof)) {
        throw new UsageError(
          `invalid profile "${name}" in ${filePath}: expected object`,
        );
      }
      const p = prof as Record<string, unknown>;
      if ('server' in p && typeof p.server !== 'string') {
        throw new UsageError(
          `invalid server in profile "${name}" in ${filePath}: expected string`,
        );
      }
      if ('token' in p && typeof p.token !== 'string') {
        throw new UsageError(
          `invalid token in profile "${name}" in ${filePath}: expected string`,
        );
      }
      if ('token_file' in p && typeof p.token_file !== 'string') {
        throw new UsageError(
          `invalid token_file in profile "${name}" in ${filePath}: expected string`,
        );
      }
    }
  }
  return data as ProfilesFile;
}

/**
 * Finds and loads the active profile configuration file across standard locations.
 * To prevent untrusted repository checkouts from exfiltrating user files/credentials,
 * a repository checkout's profiles file is NEVER loaded implicitly; it requires an
 * explicit profile-file option (`--profile-file`) or environment variable (`ARTIFREE_PROFILES_FILE`).
 * Passing only a profile name (`--profile`) resolves strictly against the user's home configuration.
 */
export async function findProfilesFile(
  customPath?: string | undefined,
): Promise<{ path: string; content: ProfilesFile } | null> {
  const targetPath = customPath ?? process.env.ARTIFREE_PROFILES_FILE;
  if (targetPath) {
    const resolvedPath = resolve(process.cwd(), targetPath);
    try {
      const raw = await readFile(resolvedPath, 'utf8');
      const parsed: unknown = JSON.parse(raw);
      return {
        path: resolvedPath,
        content: validateProfilesFile(parsed, resolvedPath),
      };
    } catch (err) {
      if (err instanceof UsageError) throw err;
      throw new UsageError(
        `cannot read profile configuration file ${resolvedPath}: ${(err as Error).message}`,
      );
    }
  }

  const candidates: string[] = [
    resolve(homedir(), '.config', 'artifree', 'profiles.json'),
    resolve(homedir(), '.artifree', 'profiles.json'),
  ];

  for (const candidate of candidates) {
    try {
      const raw = await readFile(candidate, 'utf8');
      const parsed: unknown = JSON.parse(raw);
      return {
        path: candidate,
        content: validateProfilesFile(parsed, candidate),
      };
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code === 'ENOENT') continue;
      if (err instanceof UsageError) throw err;
      throw new UsageError(
        `cannot read profile configuration file ${candidate}: ${(err as Error).message}`,
      );
    }
  }

  return null;
}

/**
 * Resolves connection options (server and token) using strict precedence:
 * - Server: CLI option > ARTIFREE_SERVER > ARTIFREE_URL > profile server > default (http://127.0.0.1:7410)
 * - Token: CLI token > CLI token-file > ARTIFREE_TOKEN > ARTIFREE_TOKEN_FILE > profile token > profile token_file
 */
export async function resolveConnection(
  options: ConnectionOptions,
): Promise<ResolvedConnection> {
  let activeProfile: Profile | undefined;

  if (options.profile !== undefined) {
    const file = await findProfilesFile(options.profileFile);
    if (!file) {
      throw new UsageError(
        `profile configuration file not found for profile "${options.profile}"`,
      );
    }
    const profiles = file.content.profiles ?? {};
    if (!Object.hasOwn(profiles, options.profile)) {
      throw new UsageError(
        `profile "${options.profile}" not found in ${file.path}`,
      );
    }
    activeProfile = profiles[options.profile];
  } else if (options.profileFile !== undefined) {
    const file = await findProfilesFile(options.profileFile);
    if (file?.content.default_profile) {
      const defaultName = file.content.default_profile;
      const profiles = file.content.profiles ?? {};
      if (Object.hasOwn(profiles, defaultName)) {
        activeProfile = profiles[defaultName];
      } else {
        throw new UsageError(
          `default profile "${defaultName}" not found in ${file.path}`,
        );
      }
    }
  } else {
    // Default profile auto-activation: resolves only from user home configuration
    const file = await findProfilesFile();
    if (file?.content.default_profile) {
      const defaultName = file.content.default_profile;
      const profiles = file.content.profiles ?? {};
      if (Object.hasOwn(profiles, defaultName)) {
        activeProfile = profiles[defaultName];
      }
    }
  }

  const server =
    options.server ??
    process.env.ARTIFREE_SERVER ??
    process.env.ARTIFREE_URL ??
    activeProfile?.server ??
    'http://127.0.0.1:7410';

  let token: string | undefined = options.token;

  if (token === undefined && options.tokenFile !== undefined) {
    token = (await readFile(options.tokenFile, 'utf8')).trim();
  }

  if (token === undefined) {
    token = process.env.ARTIFREE_TOKEN;
  }

  if (token === undefined && process.env.ARTIFREE_TOKEN_FILE) {
    token = (await readFile(process.env.ARTIFREE_TOKEN_FILE, 'utf8')).trim();
  }

  if (token === undefined && activeProfile?.token !== undefined) {
    token = activeProfile.token;
  }

  if (token === undefined && activeProfile?.token_file !== undefined) {
    token = (await readFile(activeProfile.token_file, 'utf8')).trim();
  }

  if (!token) {
    throw new UsageError(
      'authentication required: specify --token, --token-file, ARTIFREE_TOKEN, ARTIFREE_TOKEN_FILE, or configure a profile',
    );
  }

  return { server, token };
}
