import { createHash } from 'node:crypto';

/**
 * What the packager and the verifier must agree on: the platforms, the
 * paths of the files an operator runs, and the checksum. Kept free of
 * third-party imports because the verifier runs without `npm install`.
 */

/** Release targets and the native platform each is built and run on. */
export const TARGET_LAYOUT = {
  'linux-x64': {
    platform: 'linux',
    arch: 'x64',
    executable: 'artifree-server',
  },
  'windows-x64': {
    platform: 'win32',
    arch: 'x64',
    executable: 'artifree-server.exe',
  },
};

/** Package-relative path of the bundled command-line client. */
export const CLI_PATH = 'cli/artifree.mjs';

/** The entrypoints a manifest for `target` must name, exactly. */
export function entrypoints(target) {
  return {
    server: `bin/${TARGET_LAYOUT[target].executable}`,
    cli: CLI_PATH,
    web: 'web',
  };
}

/** Lowercase hex SHA-256 of `bytes`. */
export function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

/**
 * The oldest glibc the Linux package supports (decision 0037): the highest
 * `GLIBC_*` symbol version its server binary may require. It was measured as
 * 2.34 with objdump; packaging refuses a binary that needs more.
 */
export const GLIBC_FLOOR = '2.34';

/**
 * The glibc of the Debian 12 environments in which CI runs the package and the
 * container. It is the newest version at which the floor is exercised; versions
 * between the floor and this one are not run.
 */
export const GLIBC_VERIFIED_RUNTIME = '2.36';
