import { createHash } from 'node:crypto';

/**
 * What the packager and the verifier must agree on: the platforms, the
 * paths of the files an operator runs, and the checksum. Kept free of
 * third-party imports because the verifier runs without `npm install`.
 */

/**
 * Release targets and the native platform each is built and run on.
 * `hostFiles` are the package-relative paths of what an operator needs to run
 * builds on the supported host (decision 0058, R03fb): the packaging script
 * for an execution tool root and its lock list (rule 19), and the systemd
 * units of the server and of the execution worker (0056 rule 8), copied from
 * `scripts/tools/`. Only the x86_64 Linux server knows a build profile, so
 * only its package ships them; see docs/execution-host.md. `buildProfiles`
 * are the build profiles the target's release server knows (decision 0058,
 * R03fa), which the clean-install check requires of the unpacked server.
 */
export const TARGET_LAYOUT = {
  'linux-x64': {
    platform: 'linux',
    arch: 'x64',
    executable: 'artifree-server',
    buildProfiles: ['react-v1'],
    hostFiles: [
      'tools/artifree-execution.service',
      'tools/artifree.service',
      'tools/package-root.sh',
      'tools/react-v1.lock',
    ],
  },
  'windows-x64': {
    platform: 'win32',
    arch: 'x64',
    executable: 'artifree-server.exe',
    buildProfiles: [],
    hostFiles: [],
  },
};

/**
 * A package version that may name an execution tool root, as the packaging
 * script accepts it (`is_version` in `scripts/tools/package-root.sh`; a
 * tooling test runs that function against this pattern).
 */
export const ROOT_VERSION = /^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$/;

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
