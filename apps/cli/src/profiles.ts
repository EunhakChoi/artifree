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
 * a working-tree `artifree-profiles.json` is only allowed when `allowWorkingTree` is true
 * (i.e. when explicitly requested via `--profile` or `ARTIFREE_PROFILES_FILE`).
 */
export async function findProfilesFile(
  allowWorkingTree = false,
): Promise<{ path: string; content: ProfilesFile } | null> {
  const customPath = process.env.ARTIFREE_PROFILES_FILE;
  if (customPath) {
    try {
      const raw = await readFile(customPath, 'utf8');
      const parsed: unknown = JSON.parse(raw);
      return {
        path: customPath,
        content: validateProfilesFile(parsed, customPath),
      };
    } catch (err) {
      if (err instanceof UsageError) throw err;
      throw new UsageError(
        `cannot read profile configuration file ${customPath}: ${(err as Error).message}`,
      );
    }
  }

  const candidates: string[] = [];
  if (allowWorkingTree) {
    candidates.push(resolve(process.cwd(), 'artifree-profiles.json'));
  }
  candidates.push(
    resolve(homedir(), '.config', 'artifree', 'profiles.json'),
    resolve(homedir(), '.artifree', 'profiles.json'),
  );

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
    // Explicit --profile: allow working-tree profiles file as well as user profiles
    const file = await findProfilesFile(true);
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
  } else {
    // Default profile auto-activation: do NOT load untrusted working-tree files
    const file = await findProfilesFile(false);
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
