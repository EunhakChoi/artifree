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

function validateProfilesFile(
  parsed: unknown,
  configPath: string,
): ProfilesFile {
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new UsageError(
      `Invalid configuration in ${configPath}: expected JSON object`,
    );
  }

  const record = parsed as Record<string, unknown>;

  if (
    record.default_profile !== undefined &&
    typeof record.default_profile !== 'string'
  ) {
    throw new UsageError(
      `Invalid configuration in ${configPath}: "default_profile" must be a string`,
    );
  }

  if (record.server !== undefined && typeof record.server !== 'string') {
    throw new UsageError(
      `Invalid configuration in ${configPath}: "server" must be a string`,
    );
  }

  if (record.token !== undefined && typeof record.token !== 'string') {
    throw new UsageError(
      `Invalid configuration in ${configPath}: "token" must be a string`,
    );
  }

  if (
    record.token_file !== undefined &&
    typeof record.token_file !== 'string'
  ) {
    throw new UsageError(
      `Invalid configuration in ${configPath}: "token_file" must be a string`,
    );
  }

  if (record.profiles !== undefined) {
    if (
      typeof record.profiles !== 'object' ||
      record.profiles === null ||
      Array.isArray(record.profiles)
    ) {
      throw new UsageError(
        `Invalid configuration in ${configPath}: "profiles" must be an object`,
      );
    }

    const profilesMap = record.profiles as Record<string, unknown>;
    for (const [name, prof] of Object.entries(profilesMap)) {
      if (typeof prof !== 'object' || prof === null || Array.isArray(prof)) {
        throw new UsageError(
          `Invalid profile "${name}" in ${configPath}: expected object`,
        );
      }
      const profRecord = prof as Record<string, unknown>;
      if (
        profRecord.server !== undefined &&
        typeof profRecord.server !== 'string'
      ) {
        throw new UsageError(
          `Invalid profile "${name}" in ${configPath}: "server" must be a string`,
        );
      }
      if (
        profRecord.token !== undefined &&
        typeof profRecord.token !== 'string'
      ) {
        throw new UsageError(
          `Invalid profile "${name}" in ${configPath}: "token" must be a string`,
        );
      }
      if (
        profRecord.token_file !== undefined &&
        typeof profRecord.token_file !== 'string'
      ) {
        throw new UsageError(
          `Invalid profile "${name}" in ${configPath}: "token_file" must be a string`,
        );
      }
    }
  }

  return parsed as ProfilesFile;
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
      resolve(homedir(), '.artifree', 'profiles.json'),
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

  const explicitProfile = options.profile ?? process.env.ARTIFREE_PROFILE;

  if (configPath === undefined || rawContent === undefined) {
    if (explicitProfile !== undefined) {
      throw new UsageError(
        `Profile "${explicitProfile}" requested, but no configuration file was found`,
      );
    }
    return {};
  }

  let parsedUnknown: unknown;
  try {
    parsedUnknown = JSON.parse(rawContent);
  } catch (error) {
    throw new UsageError(
      `Failed to parse configuration file ${configPath}: ${error instanceof Error ? error.message : String(error)}`,
    );
  }

  const parsed = validateProfilesFile(parsedUnknown, configPath);
  const configDir = dirname(configPath);

  let config: ProfileConfig | undefined;

  if (explicitProfile !== undefined) {
    if (
      parsed.profiles === undefined ||
      !Object.hasOwn(parsed.profiles, explicitProfile)
    ) {
      throw new UsageError(
        `Profile "${explicitProfile}" not found in ${configPath}`,
      );
    }
    config = parsed.profiles[explicitProfile];
  } else if (parsed.default_profile !== undefined) {
    if (
      parsed.profiles === undefined ||
      !Object.hasOwn(parsed.profiles, parsed.default_profile)
    ) {
      throw new UsageError(
        `Profile "${parsed.default_profile}" not found in ${configPath}`,
      );
    }
    config = parsed.profiles[parsed.default_profile];
  } else if (
    parsed.server !== undefined ||
    parsed.token !== undefined ||
    parsed.token_file !== undefined
  ) {
    config = {
      server: parsed.server,
      token: parsed.token,
      token_file: parsed.token_file,
    };
  }

  return { config, configDir };
}

export async function resolveConnection(
  options: ConnectionCliOptions,
): Promise<ResolvedConnection> {
  const { config, configDir } = await loadProfileConfig(options);

  // 1. Resolve baseUrl
  const server =
    options.server ??
    process.env.ARTIFREE_SERVER ??
    process.env.ARTIFREE_URL ??
    config?.server ??
    'http://127.0.0.1:7410';

  const baseUrl = server.trim().replace(/\/+$/, '');

  // 2. Resolve accessToken
  let token: string | undefined;

  // Precedence: CLI flags (--token, --token-file) > Environment (ARTIFREE_TOKEN, ARTIFREE_TOKEN_FILE) > Profile config
  if (options.token !== undefined) {
    token = options.token.trim();
  } else if (options['token-file'] !== undefined) {
    const content = await tryReadFile(options['token-file']);
    if (content === undefined) {
      throw new UsageError(`Token file not found: ${options['token-file']}`);
    }
    token = content.trim();
  } else if (process.env.ARTIFREE_TOKEN !== undefined) {
    token = process.env.ARTIFREE_TOKEN.trim();
  } else if (process.env.ARTIFREE_TOKEN_FILE !== undefined) {
    const content = await tryReadFile(process.env.ARTIFREE_TOKEN_FILE);
    if (content === undefined) {
      throw new UsageError(
        `Token file not found: ${process.env.ARTIFREE_TOKEN_FILE}`,
      );
    }
    token = content.trim();
  } else if (config?.token !== undefined) {
    token = config.token.trim();
  } else if (config?.token_file !== undefined) {
    const resolvedPath = configDir
      ? resolve(configDir, config.token_file)
      : resolve(config.token_file);
    const content = await tryReadFile(resolvedPath);
    if (content === undefined) {
      throw new UsageError(`Token file not found: ${resolvedPath}`);
    }
    token = content.trim();
  }

  if (token === undefined || token === '') {
    throw new UsageError(
      'An access token is required: pass --token, --token-file, set ARTIFREE_TOKEN or ARTIFREE_TOKEN_FILE, or configure a profile.',
    );
  }

  return { baseUrl, accessToken: token };
}
