import { readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { dirname, isAbsolute, resolve } from 'node:path';
import { UsageError } from './errors.js';

export interface Profile {
  server?: string | undefined;
  token?: string | undefined;
  token_file?: string | undefined;
}

export interface ProfilesConfig {
  default_profile?: string | undefined;
  profiles?: Record<string, Profile> | undefined;
}

export interface ConnectionConfig {
  baseUrl: string;
  accessToken: string;
}

export interface ConnectionArgs {
  server?: string | undefined;
  token?: string | undefined;
  'token-file'?: string | undefined;
  profile?: string | undefined;
  config?: string | undefined;
}

async function tryReadFile(path: string): Promise<string | undefined> {
  try {
    return await readFile(path, 'utf8');
  } catch (error: unknown) {
    if (
      typeof error === 'object' &&
      error !== null &&
      'code' in error &&
      error.code === 'ENOENT'
    ) {
      return undefined;
    }
    throw error;
  }
}

function parseConfig(raw: string, path: string): ProfilesConfig {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error: unknown) {
    const message = error instanceof Error ? error.message : String(error);
    throw new UsageError(
      `Failed to parse configuration file at "${path}": ${message}`,
    );
  }

  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new UsageError(
      `Configuration file at "${path}" must contain a JSON object.`,
    );
  }

  const candidate = parsed as Record<string, unknown>;
  const defaultProfile =
    typeof candidate.default_profile === 'string'
      ? candidate.default_profile
      : undefined;

  let profiles: Record<string, Profile> | undefined;
  if (
    typeof candidate.profiles === 'object' &&
    candidate.profiles !== null &&
    !Array.isArray(candidate.profiles)
  ) {
    profiles = {};
    for (const [key, val] of Object.entries(candidate.profiles)) {
      if (typeof val === 'object' && val !== null && !Array.isArray(val)) {
        const item = val as Record<string, unknown>;
        profiles[key] = {
          server: typeof item.server === 'string' ? item.server : undefined,
          token: typeof item.token === 'string' ? item.token : undefined,
          token_file:
            typeof item.token_file === 'string' ? item.token_file : undefined,
        };
      }
    }
  }

  return {
    default_profile: defaultProfile,
    profiles,
  };
}

interface LoadedConfig {
  config: ProfilesConfig;
  configPath: string;
}

async function loadConfigFile(
  explicitPath?: string | undefined,
): Promise<LoadedConfig | undefined> {
  const envConfig = process.env.ARTIFREE_CONFIG_FILE;
  const path =
    explicitPath ??
    (envConfig && envConfig.trim() !== '' ? envConfig.trim() : undefined);

  if (path !== undefined) {
    const content = await tryReadFile(path);
    if (content === undefined) {
      throw new UsageError(`Configuration file not found: "${path}".`);
    }
    return {
      config: parseConfig(content, path),
      configPath: resolve(path),
    };
  }

  const candidates = [
    resolve('.artifree', 'profiles.json'),
    resolve('.artifree', 'config.json'),
    resolve(homedir(), '.config', 'artifree', 'profiles.json'),
  ];

  for (const candidate of candidates) {
    const content = await tryReadFile(candidate);
    if (content !== undefined) {
      return {
        config: parseConfig(content, candidate),
        configPath: candidate,
      };
    }
  }

  return undefined;
}

function resolveProfile(
  loaded: LoadedConfig | undefined,
  requestedProfile?: string | undefined,
): Profile | undefined {
  if (loaded === undefined) {
    if (requestedProfile !== undefined) {
      throw new UsageError(
        `Profile "${requestedProfile}" requested, but no configuration file was found.`,
      );
    }
    return undefined;
  }

  const { config, configPath } = loaded;
  const profileName =
    requestedProfile ??
    config.default_profile ??
    (config.profiles && 'default' in config.profiles ? 'default' : undefined);

  if (profileName === undefined) {
    return undefined;
  }

  const profile = config.profiles?.[profileName];
  if (profile === undefined) {
    throw new UsageError(
      `Profile "${profileName}" not found in configuration file "${configPath}".`,
    );
  }

  return profile;
}

export async function resolveConnection(
  args: ConnectionArgs,
): Promise<ConnectionConfig> {
  const loaded = await loadConfigFile(args.config);
  const profile = resolveProfile(loaded, args.profile);

  // 1. Resolve baseUrl
  const rawBaseUrl =
    args.server ??
    process.env.ARTIFREE_URL ??
    profile?.server ??
    'http://127.0.0.1:7410';

  let baseUrl: string;
  try {
    const parsed = new URL(rawBaseUrl);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
      throw new Error('Protocol must be http or https');
    }
    baseUrl = parsed.origin;
  } catch {
    throw new UsageError(
      `Invalid server URL: "${rawBaseUrl}". Must be a valid HTTP or HTTPS URL.`,
    );
  }

  // 2. Resolve accessToken
  let accessToken = args.token;

  if (accessToken === undefined && args['token-file'] !== undefined) {
    accessToken = (await readFile(args['token-file'], 'utf8')).trim();
  }

  if (accessToken === undefined) {
    const envToken = process.env.ARTIFREE_TOKEN;
    if (envToken !== undefined && envToken.trim() !== '') {
      accessToken = envToken.trim();
    }
  }

  if (accessToken === undefined) {
    const envTokenFile = process.env.ARTIFREE_TOKEN_FILE;
    if (envTokenFile !== undefined && envTokenFile.trim() !== '') {
      accessToken = (await readFile(envTokenFile.trim(), 'utf8')).trim();
    }
  }

  if (accessToken === undefined && profile?.token !== undefined) {
    accessToken = profile.token.trim();
  }

  if (accessToken === undefined && profile?.token_file !== undefined) {
    const tokenFilePath =
      isAbsolute(profile.token_file) || loaded === undefined
        ? profile.token_file
        : resolve(dirname(loaded.configPath), profile.token_file);
    accessToken = (await readFile(tokenFilePath, 'utf8')).trim();
  }

  if (accessToken === undefined || accessToken === '') {
    throw new UsageError(
      'An access token is required: pass --token or --token-file, set ARTIFREE_TOKEN or ARTIFREE_TOKEN_FILE, or configure a profile.',
    );
  }

  return { baseUrl, accessToken };
}
