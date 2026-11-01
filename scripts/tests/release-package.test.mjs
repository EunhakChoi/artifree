import assert from 'node:assert/strict';
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  symlink,
  writeFile,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  assertBinaryFormat,
  buildReleasePackage,
  hostTarget,
} from '../package-release.mjs';
import { buildServer } from '../lib/server-process.mjs';
import {
  extract,
  refuses,
  verifyChecksum,
  verifyManifest,
  verifyRelease,
} from '../verify-release.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const target = hostTarget();
const options = { skip: target === undefined && 'no release target here' };
let scratch;
let binary;
let webDir;

before(async () => {
  scratch = await mkdtemp(join(tmpdir(), 'artifree-release-test-'));
  webDir = join(scratch, 'web');
  await mkdir(join(webDir, 'assets'), { recursive: true });
  await writeFile(
    join(webDir, 'index.html'),
    '<!doctype html><title>t</title>',
  );
  await writeFile(join(webDir, 'assets', 'app.js'), 'console.log(1);');
  binary = buildServer(root);
});

after(async () => {
  await rm(scratch, { recursive: true, force: true });
});

function pack(outDir, dir = webDir) {
  return buildReleasePackage({ root, target, binary, webDir: dir, outDir });
}

test(
  'an unpacked release package starts, saves, restarts, and refuses bad startup',
  options,
  async () => {
    const built = await pack(join(scratch, 'out-run'));
    const summary = await verifyRelease(built.archive);
    assert.deepEqual(summary, { revision: 2 });
  },
);

test('the same inputs produce the same archive bytes', options, async () => {
  const first = await pack(join(scratch, 'out-a'));
  const second = await pack(join(scratch, 'out-b'));
  assert.deepEqual(
    await readFile(first.archive),
    await readFile(second.archive),
  );
  assert.equal(
    await readFile(first.checksum, 'utf8'),
    await readFile(second.checksum, 'utf8'),
  );
});

test(
  'the manifest check rejects a changed, added, or removed file',
  options,
  async () => {
    const built = await pack(join(scratch, 'out-tamper'));
    const installRoot = join(scratch, 'tamper');
    await mkdir(installRoot);
    extract(built.archive, installRoot);
    const dir = join(installRoot, built.name);
    await verifyManifest(dir);

    const index = join(dir, 'web', 'index.html');
    const original = await readFile(index);
    await writeFile(index, `${original}<!-- changed -->`);
    await assert.rejects(verifyManifest(dir), /Size mismatch: web\/index.html/);
    await writeFile(index, Buffer.from(original).fill(0x41));
    await assert.rejects(
      verifyManifest(dir),
      /SHA-256 mismatch: web\/index.html/,
    );
    await writeFile(index, original);
    await verifyManifest(dir);

    await writeFile(join(dir, 'extra.txt'), 'unlisted');
    await assert.rejects(verifyManifest(dir), /differ from the manifest/);
    await rm(join(dir, 'extra.txt'));
    await rm(index);
    await assert.rejects(verifyManifest(dir), /Files differ/);
  },
);

test('packaging refuses web assets without index.html', options, async () => {
  const empty = join(scratch, 'empty-web');
  await mkdir(empty);
  await assert.rejects(
    pack(join(scratch, 'out-empty'), empty),
    /No index.html/,
  );
});

test('packaging refuses a binary of the wrong platform or format', () => {
  const elf = Buffer.alloc(64);
  elf.writeUInt32BE(0x7f454c46, 0);
  elf.writeUInt16LE(0x3e, 18);
  assertBinaryFormat('linux-x64', elf);
  assert.throws(() => assertBinaryFormat('windows-x64', elf), /PE/);
  assert.throws(
    () => assertBinaryFormat('linux-x64', Buffer.from('#!/bin/sh\n')),
    /ELF/,
  );
  elf.writeUInt16LE(0xb7, 18); // AArch64
  assert.throws(() => assertBinaryFormat('linux-x64', elf), /ELF/);

  const pe = Buffer.alloc(0x100);
  pe.writeUInt16BE(0x4d5a, 0);
  pe.writeUInt32LE(0x80, 0x3c);
  pe.writeUInt32BE(0x50450000, 0x80);
  pe.writeUInt16LE(0x8664, 0x84);
  assertBinaryFormat('windows-x64', pe);
  assert.throws(() => assertBinaryFormat('linux-x64', pe), /ELF/);
  pe.writeUInt16LE(0xaa64, 0x84); // ARM64
  assert.throws(() => assertBinaryFormat('windows-x64', pe), /PE/);
});

test('the archive checksum rejects a changed archive', options, async () => {
  const built = await pack(join(scratch, 'out-checksum'));
  await verifyChecksum(built.archive);
  const bytes = await readFile(built.archive);
  bytes[bytes.length - 1] ^= 0xff;
  await writeFile(built.archive, bytes);
  await assert.rejects(verifyChecksum(built.archive), /checksum mismatch/);
});

/** Builds and unpacks a package; returns its directory and archive info. */
async function unpacked(label) {
  const built = await pack(join(scratch, `out-${label}`));
  const installRoot = join(scratch, label);
  await mkdir(installRoot);
  extract(built.archive, installRoot);
  return { built, dir: join(installRoot, built.name) };
}

test(
  'the manifest check rejects a link, even to identical bytes',
  options,
  async (t) => {
    const { dir } = await unpacked('link');
    const index = join(dir, 'web', 'index.html');
    const same = join(scratch, 'outside-copy.html');
    await copyFile(index, same);
    await rm(index);
    try {
      await symlink(same, index);
    } catch (error) {
      if (error.code === 'EPERM' || error.code === 'EACCES') {
        t.skip(
          'this account may not create symlinks (Windows without privilege)',
        );
        return;
      }
      throw error;
    }
    await assert.rejects(
      verifyManifest(dir),
      /Not a regular file or directory: web\/index.html/,
    );
  },
);

test(
  'the manifest check rejects an unlisted empty directory',
  options,
  async () => {
    const { dir } = await unpacked('emptydir');
    await verifyManifest(dir);
    await mkdir(join(dir, 'web', 'hidden'));
    await assert.rejects(verifyManifest(dir), /Directories differ/);
  },
);

test(
  'the manifest check rejects entrypoints other than the target layout',
  options,
  async () => {
    const { dir } = await unpacked('entry');
    const path = join(dir, 'MANIFEST.json');
    const manifest = JSON.parse(await readFile(path, 'utf8'));
    await writeFile(
      path,
      JSON.stringify({
        ...manifest,
        entrypoints: { ...manifest.entrypoints, server: 'web/index.html' },
      }),
    );
    await assert.rejects(verifyManifest(dir), /Unexpected entrypoints/);

    await writeFile(
      path,
      JSON.stringify({
        ...manifest,
        files: manifest.files.filter(
          (f) => f.path !== manifest.entrypoints.cli,
        ),
      }),
    );
    await assert.rejects(
      verifyManifest(dir),
      /does not list cli\/artifree.mjs/,
    );
  },
);

test(
  'verification rejects an archive renamed away from its manifest',
  options,
  async () => {
    const built = await pack(join(scratch, 'out-rename'));
    const renamed = join(
      scratch,
      built.archive.endsWith('.zip')
        ? 'artifree-9.9.9-x.zip'
        : 'artifree-9.9.9-x.tar.gz',
    );
    await copyFile(built.archive, renamed);
    await assert.rejects(
      verifyRelease(renamed),
      /does not match the archive name/,
    );
  },
);

test(
  'packaging rejects a binary that reports another version',
  options,
  async () => {
    // Node is a native executable of this platform whose --version is "v22...".
    await assert.rejects(
      buildReleasePackage({
        root,
        target,
        binary: process.execPath,
        webDir,
        outDir: join(scratch, 'out-version'),
      }),
      /does not report the repository version/,
    );
  },
);

test('the refusal check fails when the server starts anyway', async () => {
  let killed = false;
  const accepting = Promise.resolve({
    kill: () => {
      killed = true;
    },
  });
  await assert.rejects(refuses(accepting, /must differ/), /started although/);
  assert.equal(killed, true, 'a wrongly started server must be stopped');
  await refuses(
    Promise.reject(new Error('origins must differ')),
    /must differ/,
  );
  await assert.rejects(
    refuses(Promise.reject(new Error('some other failure')), /must differ/),
  );
});
