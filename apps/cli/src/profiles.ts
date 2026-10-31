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
 */
export async function findProfilesFile(): Promise<{
  path: string;
  content: ProfilesFile;
} | null> {
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

  const candidates = [
    resolve(process.cwd(), 'artifree-profiles.json'),
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
      if (
        err &&
        typeof err === 'object' &&
        'code' in err &&
        err.code === 'ENOENT'
      ) {
        continue;
      }
      if (err instanceof UsageError) throw err;
      throw new UsageError(
        `cannot read profile configuration file ${candidate}: ${(err as Error).message}`,
      );
    }
  }

  return null;
}

/**
 * Resolves connection parameters (server URL and access token) using strict precedence:
 * 1. CLI flags (--server, --token, --token-file, --profile)
 * 2. Environment variables (ARTIFREE_SERVER, ARTIFREE_TOKEN, ARTIFREE_TOKEN_FILE, ARTIFREE_PROFILE)
 * 3. Configuration file profiles (explicit profile or default_profile)
 * 4. Built-in defaults (server: http://127.0.0.1:7410)
 */
export async function resolveConnection(
  options: ConnectionOptions,
): Promise<ResolvedConnection> {
  const fileResult = await findProfilesFile();
  const explicitProfile = options.profile ?? process.env.ARTIFREE_PROFILE;

  let activeProfile: Profile | undefined;

  if (explicitProfile !== undefined && explicitProfile !== '') {
    if (!fileResult) {
      throw new UsageError(
        `profile configuration file not found, but profile "${explicitProfile}" was requested`,
      );
    }
    if (
      !fileResult.content.profiles ||
      !Object.hasOwn(fileResult.content.profiles, explicitProfile)
    ) {
      throw new UsageError(
        `profile "${explicitProfile}" not found in ${fileResult.path}`,
      );
    }
    activeProfile = fileResult.content.profiles[explicitProfile];
  } else if (fileResult?.content.default_profile) {
    const defaultName = fileResult.content.default_profile;
    if (
      !fileResult.content.profiles ||
      !Object.hasOwn(fileResult.content.profiles, defaultName)
    ) {
      throw new UsageError(
        `default_profile "${defaultName}" not found in ${fileResult.path}`,
      );
    }
    activeProfile = fileResult.content.profiles[defaultName];
  }

  // 1. Resolve server URL
  const server =
    options.server ??
    process.env.ARTIFREE_SERVER ??
    process.env.ARTIFREE_URL ??
    activeProfile?.server ??
    'http://127.0.0.1:7410';

  // 2. Resolve access token with strict precedence
  let token: string | undefined;

  if (options.token !== undefined && options.token !== '') {
    token = options.token;
  } else if (options.tokenFile !== undefined) {
    try {
      token = (await readFile(options.tokenFile, 'utf8')).trim();
    } catch (err) {
      throw new UsageError(
        `cannot read token file ${options.tokenFile}: ${(err as Error).message}`,
      );
    }
  } else if (
    process.env.ARTIFREE_TOKEN !== undefined &&
    process.env.ARTIFREE_TOKEN !== ''
  ) {
    token = process.env.ARTIFREE_TOKEN;
  } else if (
    process.env.ARTIFREE_TOKEN_FILE !== undefined &&
    process.env.ARTIFREE_TOKEN_FILE !== ''
  ) {
    try {
      token = (await readFile(process.env.ARTIFREE_TOKEN_FILE, 'utf8')).trim();
    } catch (err) {
      throw new UsageError(
        `cannot read token file ${process.env.ARTIFREE_TOKEN_FILE}: ${(err as Error).message}`,
      );
    }
  } else if (activeProfile?.token !== undefined && activeProfile.token !== '') {
    token = activeProfile.token;
  } else if (activeProfile?.token_file !== undefined) {
    try {
      token = (await readFile(activeProfile.token_file, 'utf8')).trim();
    } catch (err) {
      throw new UsageError(
        `cannot read profile token file ${activeProfile.token_file}: ${(err as Error).message}`,
      );
    }
  }

  if (token === undefined || token === '') {
    throw new UsageError(
      'An access token is required: pass --token or --token-file, or set ARTIFREE_TOKEN or ARTIFREE_TOKEN_FILE, or configure a profile.',
    );
  }

  return { server, token };
}
