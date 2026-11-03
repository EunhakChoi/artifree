import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { assertGlibcFloor, glibcFromLddOutput } from './lib/elf-glibc.mjs';
import { GLIBC_FLOOR, GLIBC_VERIFIED_RUNTIME } from './lib/release-layout.mjs';

/**
 * `node scripts/check-glibc.mjs --runtime` fails unless the glibc of the
 * machine running it is the one CI verifies the package on (Debian 12). It
 * keeps the "ran on glibc 2.36" claim honest if the verifying image changes.
 *
 * `node scripts/check-glibc.mjs <linux-x64 archive or binary>` prints the
 * highest glibc version the server binary requires and fails when it is above
 * the supported floor (decision 0037). The same check runs while packaging;
 * this entry point lets CI repeat it on the archive that is actually uploaded.
 */

/** Bytes of the server binary inside a `.tar.gz` release package. */
function binaryFromArchive(archive) {
  const listing = spawnSync('tar', ['-tzf', archive], {
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  assert.equal(listing.status, 0, `Cannot list ${archive}: ${listing.stderr}`);
  const entries = listing.stdout
    .split('\n')
    .filter((entry) => /^[^/]+\/bin\/artifree-server$/.test(entry));
  assert.equal(
    entries.length,
    1,
    'The archive has no single bin/artifree-server.',
  );
  const extracted = spawnSync('tar', ['-xzOf', archive, entries[0]], {
    maxBuffer: 512 * 1024 * 1024,
  });
  assert.equal(extracted.status, 0, `Cannot read ${entries[0]}.`);
  return extracted.stdout;
}

function checkRuntime() {
  const ldd = spawnSync('ldd', ['--version'], { encoding: 'utf8' });
  assert.equal(ldd.status, 0, `ldd --version failed: ${ldd.stderr}`);
  const found = glibcFromLddOutput(ldd.stdout);
  assert.equal(
    found,
    GLIBC_VERIFIED_RUNTIME,
    `This machine has glibc ${found}, not ${GLIBC_VERIFIED_RUNTIME}.`,
  );
  console.log(JSON.stringify({ runtime: found }));
}

async function main() {
  if (process.argv[2] === '--runtime') return checkRuntime();
  const [path, ...extra] = process.argv.slice(2);
  assert.ok(
    path && extra.length === 0,
    'Usage: check-glibc.mjs <archive|binary>',
  );
  const bytes = path.endsWith('.tar.gz')
    ? binaryFromArchive(resolve(path))
    : await readFile(resolve(path));
  const required = assertGlibcFloor(bytes, GLIBC_FLOOR);
  console.log(JSON.stringify({ required, floor: GLIBC_FLOOR }));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) await main();
