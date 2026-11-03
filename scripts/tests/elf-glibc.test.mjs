import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  assertGlibcFloor,
  compareVersions,
  glibcFromLddOutput,
  parseVersion,
  requiredGlibcVersions,
} from '../lib/elf-glibc.mjs';
import { elfWithNeeds } from './fixtures/elf.mjs';

test('the required glibc versions are read, sorted numerically, across libraries', () => {
  const elf = elfWithNeeds({
    'libc.so.6': ['GLIBC_2.34', 'GLIBC_2.2.5', 'GLIBC_2.9', 'GLIBC_PRIVATE'],
    'libm.so.6': ['GLIBC_2.27'],
    'libgcc_s.so.1': ['GCC_3.0'],
  });
  assert.deepEqual(requiredGlibcVersions(elf), [
    '2.2.5',
    '2.9',
    '2.27',
    '2.34',
  ]);
});

test('a binary at the floor passes and a binary above it names the version', () => {
  const atFloor = elfWithNeeds({ 'libc.so.6': ['GLIBC_2.17', 'GLIBC_2.34'] });
  assert.equal(assertGlibcFloor(atFloor, '2.34'), '2.34');
  const above = elfWithNeeds({ 'libc.so.6': ['GLIBC_2.34', 'GLIBC_2.38'] });
  assert.throws(
    () => assertGlibcFloor(above, '2.34'),
    /requires glibc 2\.38, above the supported floor 2\.34/,
  );
  // 2.4 is older than 2.34; comparing as text would get this wrong.
  assert.equal(
    assertGlibcFloor(elfWithNeeds({ 'libc.so.6': ['GLIBC_2.4'] }), '2.34'),
    '2.4',
  );
});

test('a file without versioned glibc symbols cannot pass any floor', () => {
  const none = elfWithNeeds({ 'libgcc_s.so.1': ['GCC_3.0'] });
  assert.deepEqual(requiredGlibcVersions(none), []);
  assert.throws(() => assertGlibcFloor(none, '2.34'), /not a glibc executable/);
});

test('a file that is not an ELF64 little-endian executable is refused', () => {
  assert.throws(
    () => requiredGlibcVersions(Buffer.from('#!/bin/sh\n'.repeat(20))),
    /Not a 64-bit little-endian ELF/,
  );
  const elf = elfWithNeeds({ 'libc.so.6': ['GLIBC_2.34'] });
  elf[4] = 1; // 32-bit
  assert.throws(() => requiredGlibcVersions(elf), /Not a 64-bit/);
});

test('a truncated version table is refused instead of read past its end', () => {
  const elf = elfWithNeeds({ 'libc.so.6': ['GLIBC_2.34'] });
  assert.throws(() => requiredGlibcVersions(elf.subarray(0, elf.length - 8)));
});

test('versions compare numerically', () => {
  assert.ok(compareVersions(parseVersion('2.34'), parseVersion('2.4')) > 0);
  assert.ok(compareVersions(parseVersion('2.2.5'), parseVersion('2.17')) < 0);
  assert.equal(
    compareVersions(parseVersion('2.36'), parseVersion('2.36.0')),
    0,
  );
  assert.throws(() => parseVersion('2.x'), /Not a glibc version/);
});

test('the runtime glibc is read from ldd output', () => {
  assert.equal(
    glibcFromLddOutput(
      'ldd (Debian GLIBC 2.36-9+deb12u10) 2.36\nCopyright (C) 2022\n',
    ),
    '2.36',
  );
  assert.equal(
    glibcFromLddOutput('ldd (Ubuntu GLIBC 2.39-0ubuntu8) 2.39\n'),
    '2.39',
  );
  assert.throws(
    () => glibcFromLddOutput('musl libc (x86_64)\nVersion 1.2.5\n'),
    /Cannot read the glibc version/,
  );
});
