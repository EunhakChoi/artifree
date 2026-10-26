import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  chmod,
  copyFile,
  mkdir,
  mkdtemp,
  rm,
  writeFile,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

assert.ok(
  process.platform === 'linux' && process.arch === 'x64',
  'This CI installer supports Linux x64; install actionlint 1.7.12 and cargo-deny 0.20.2 separately elsewhere.',
);
const destination = new URL('../.artifree/tools/', import.meta.url);
await mkdir(destination, { recursive: true });
const temporary = await mkdtemp(join(tmpdir(), 'artifree-ci-tools-'));
const tools = [
  {
    name: 'actionlint',
    url: 'https://github.com/rhysd/actionlint/releases/download/v1.7.12/actionlint_1.7.12_linux_amd64.tar.gz',
    digest: '8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8',
    executable: 'actionlint',
  },
  {
    name: 'cargo-deny',
    url: 'https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-unknown-linux-musl.tar.gz',
    digest: '9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f',
    executable: 'cargo-deny-0.20.2-x86_64-unknown-linux-musl/cargo-deny',
  },
];
try {
  for (const tool of tools) {
    const response = await fetch(tool.url, {
      signal: AbortSignal.timeout(60_000),
    });
    assert.ok(response.ok, `Download failed: ${tool.name}: ${response.status}`);
    const content = Buffer.from(await response.arrayBuffer());
    assert.equal(
      createHash('sha256').update(content).digest('hex'),
      tool.digest,
      `Archive checksum mismatch: ${tool.name}`,
    );
    const archive = join(temporary, `${tool.name}.tar.gz`);
    await writeFile(archive, content);
    execFileSync('tar', ['-xzf', archive, '-C', temporary, tool.executable], {
      stdio: 'inherit',
    });
    const output = new URL(tool.name, destination);
    await copyFile(join(temporary, tool.executable), output);
    await chmod(output, 0o755);
    console.log(`Installed ${tool.name} from its verified archive.`);
  }
} finally {
  await rm(temporary, { recursive: true, force: true });
}
