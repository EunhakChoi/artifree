import { readFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { UsageError } from './errors.js';

export interface ProfileConfig {
  server?: string | undefined;
  token?: string | undefined;
  token_file?: string | undefined;
}

export interface ProfilesFile {
  default_profile?: string | undefined;
  profiles?: Record<string, ProfileConfig> | undefined;
  // Single-profile config shorthand
  server?: string | undefined;
  token?: string | undefined;
  token_file?: string | undefined;
}

export interface ConnectionCliOptions {
  server?: string | undefined;
  token?: string | undefined;
  'token-file'?: string | undefined;
  config?: string | undefined;
  profile?: string | undefined;
}

export interface ResolvedConnection {
  baseUrl: string;
  accessToken: string;
}

async function tryReadFile(path: string): Promise<string | undefined> {
  try {
    return await readFile(path, 'utf8');
  } catch {
    return undefined;
  }
}

async function loadProfileConfig(options: ConnectionCliOptions): Promise<{
  config?: ProfileConfig | undefined;
  configDir?: string | undefined;
}> {
  let configPath = options.config ?? process.env.ARTIFREE_CONFIG_FILE;
  let rawContent: string | undefined;

  if (configPath !== undefined) {
    rawContent = await tryReadFile(configPath);
    if (rawContent === undefined) {
      throw new UsageError(`Configuration file not found: ${configPath}`);
    }
  } else {
    const candidates = [
      resolve('.artifree', 'profiles.json'),
      resolve('.artifree', 'config.json'),
      resolve(homedir(), '.config', 'artifree', 'profiles.json'),
    ];
    for (const candidate of candidates) {
      rawContent = await tryReadFile(candidate);
      if (rawContent !== undefined) {
        configPath = candidate;
        break;
      }
    }
  }

  if (configPath === undefined || rawContent === undefined) {
    return {};
  }

  let parsed: ProfilesFile;
  try {
    parsed = JSON.parse(rawContent) as ProfilesFile;
  } catch (error) {
    throw new UsageError(
      `Invalid JSON in configuration file ${configPath}: ${error instanceof Error ? error.message : String(error)}`,
    );
  }

  const configDir = dirname(configPath);
  const explicitProfile = options.profile ?? process.env.ARTIFREE_PROFILE;

  if (
    parsed.profiles !== undefined &&
    parsed.profiles !== null &&
    typeof parsed.profiles === 'object' &&
    !Array.isArray(parsed.profiles)
  ) {
    const profileName = explicitProfile ?? parsed.default_profile ?? 'default';
    const profile = parsed.profiles[profileName];
    if (profile === undefined) {
      throw new UsageError(
        `Profile "${profileName}" not found in configuration file ${configPath}`,
      );
    }
    return { config: profile, configDir };
  }

  if (explicitProfile !== undefined) {
    throw new UsageError(
      `Profile "${explicitProfile}" not found in configuration file ${configPath}`,
    );
  }

  if (
    parsed.server !== undefined ||
    parsed.token !== undefined ||
    parsed.token_file !== undefined
  ) {
    return {
      config: {
        server: parsed.server,
        token: parsed.token,
        token_file: parsed.token_file,
      },
      configDir,
    };
  }

  return { configDir };
}

/**
 * Resolves API server URL and access token with strict precedence:
 * - Server: CLI --server > ARTIFREE_URL > profile server > default http://127.0.0.1:7410
 * - Token: CLI --token > CLI --token-file > ARTIFREE_TOKEN > ARTIFREE_TOKEN_FILE > profile token > profile token_file
 */
export async function resolveConnection(
  options: ConnectionCliOptions,
): Promise<ResolvedConnection> {
  const { config, configDir } = await loadProfileConfig(options);

  const baseUrl =
    options.server ??
    process.env.ARTIFREE_URL ??
    config?.server ??
    'http://127.0.0.1:7410';

  let accessToken: string | undefined;

  if (options.token !== undefined && options.token.trim() !== '') {
    accessToken = options.token.trim();
  } else if (options['token-file'] !== undefined) {
    const content = await tryReadFile(options['token-file']);
    if (content === undefined) {
      throw new UsageError(`Token file not found: ${options['token-file']}`);
    }
    accessToken = content.trim();
  } else if (
    process.env.ARTIFREE_TOKEN !== undefined &&
    process.env.ARTIFREE_TOKEN.trim() !== ''
  ) {
    accessToken = process.env.ARTIFREE_TOKEN.trim();
  } else if (
    process.env.ARTIFREE_TOKEN_FILE !== undefined &&
    process.env.ARTIFREE_TOKEN_FILE.trim() !== ''
  ) {
    const content = await tryReadFile(process.env.ARTIFREE_TOKEN_FILE);
    if (content === undefined) {
      throw new UsageError(
        `Token file not found: ${process.env.ARTIFREE_TOKEN_FILE}`,
      );
    }
    accessToken = content.trim();
  } else if (config?.token !== undefined && config.token.trim() !== '') {
    accessToken = config.token.trim();
  } else if (config?.token_file !== undefined) {
    const tokenFilePath = configDir
      ? resolve(configDir, config.token_file)
      : config.token_file;
    const content = await tryReadFile(tokenFilePath);
    if (content === undefined) {
      throw new UsageError(`Profile token file not found: ${tokenFilePath}`);
    }
    accessToken = content.trim();
  }

  if (accessToken === undefined || accessToken === '') {
    throw new UsageError(
      'An access token is required: pass --token or --token-file, set ARTIFREE_TOKEN or ARTIFREE_TOKEN_FILE, or configure a profile token.',
    );
  }

  return { baseUrl, accessToken };
}
