import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse } from 'yaml';

/**
 * Base image digest pins (decision 0037). `--check` (offline, part of
 * `npm run check`) requires every container image reference in the Dockerfile
 * and the workflows to be `name:tag@sha256:<64 hex>` and one reference to name
 * one digest everywhere. Without `--check`, a human on a networked machine runs
 * it to resolve each tag's current digest from its registry and print the lines
 * to change; it never edits a file and refuses to run in CI. Dependabot is not
 * used for these pins (decision 0037 says why).
 */

const MANIFEST_TYPES = [
  'application/vnd.oci.image.index.v1+json',
  'application/vnd.docker.distribution.manifest.list.v2+json',
  'application/vnd.oci.image.manifest.v1+json',
  'application/vnd.docker.distribution.manifest.v2+json',
];

/** Files that may pin an image, relative to the repository root. */
async function pinnedFiles(root) {
  const files = [];
  const deploy = join(root, 'deploy');
  const walk = async (dir) => {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) await walk(path);
      else if (entry.name.startsWith('Dockerfile')) files.push(path);
    }
  };
  await walk(deploy);
  const workflows = join(root, '.github', 'workflows');
  for (const name of await readdir(workflows))
    if (/\.ya?ml$/.test(name)) files.push(join(workflows, name));
  return files.map((path) => relative(root, path).split('\\').join('/')).sort();
}

/**
 * Every `@sha256:` reference in `text`: `{ line, reference, name, tag, digest }`.
 * A reference is the run of reference characters before `@sha256:`; `tag` is
 * undefined when it has none, and `digest` is whatever follows, valid or not.
 */
export function findPins(text) {
  const pins = [];
  text.split('\n').forEach((content, index) => {
    for (const match of content.matchAll(
      /([A-Za-z0-9][A-Za-z0-9._/:-]*)@sha256:([^\s"'`)]*)/g,
    )) {
      const reference = match[1];
      const colon = reference.lastIndexOf(':');
      const tagged = colon > reference.lastIndexOf('/');
      pins.push({
        line: index + 1,
        reference,
        name: tagged ? reference.slice(0, colon) : reference,
        tag: tagged ? reference.slice(colon + 1) : undefined,
        digest: match[2],
      });
    }
  });
  return pins;
}

/** Throws on the first pin that is not `name:tag@sha256:<64 hex>`. */
export function checkPin(pin, where) {
  assert.ok(
    pin.tag !== undefined && pin.tag !== 'latest' && pin.tag !== '',
    `${where}: ${pin.reference} needs a version tag before @sha256 (a bare digest cannot be refreshed).`,
  );
  assert.match(
    pin.digest,
    /^[0-9a-f]{64}$/,
    `${where}: ${pin.reference} is not pinned to a 64-digit sha256 digest.`,
  );
}

function imageValues(workflow) {
  return Object.values(workflow.jobs ?? {}).flatMap((job) => [
    job.container?.image ?? job.container,
    ...Object.values(job.services ?? {}).map((service) => service.image),
  ]);
}

/**
 * Checks `files` (`{ path, text }`): every pin is well formed, one image
 * reference names one digest across all files, a Dockerfile `FROM` and a
 * workflow job `container`/`services` image is pinned, and at least one pin
 * exists. Returns the distinct pins as `{ reference, digest, places }`.
 */
export function checkPins(files) {
  const byReference = new Map();
  for (const { path, text } of files) {
    for (const pin of findPins(text)) {
      checkPin(pin, `${path}:${pin.line}`);
      const known = byReference.get(pin.reference);
      if (known) {
        assert.equal(
          known.digest,
          pin.digest,
          `${path}:${pin.line}: ${pin.reference} has two different digests (also ${known.places[0]}).`,
        );
        known.places.push(`${path}:${pin.line}`);
      } else {
        byReference.set(pin.reference, {
          reference: pin.reference,
          digest: pin.digest,
          places: [`${path}:${pin.line}`],
        });
      }
    }
    if (/\.ya?ml$/.test(path))
      for (const image of imageValues(parse(text)))
        if (typeof image === 'string' && !image.includes('${{'))
          assert.match(
            image,
            /@sha256:/,
            `${path}: image ${image} is not pinned by digest.`,
          );
    if (/(^|\/)Dockerfile/.test(path))
      for (const line of text.split('\n'))
        if (/^FROM\s/i.test(line))
          assert.match(
            line,
            /@sha256:/,
            `${path}: ${line} is not pinned by digest.`,
          );
  }
  assert.ok(byReference.size > 0, 'No image pins found.');
  return [...byReference.values()];
}

/** Reads the repository's pin files. */
export async function readPinFiles(root) {
  return Promise.all(
    (await pinnedFiles(root)).map(async (path) => ({
      path,
      text: await readFile(join(root, path), 'utf8'),
    })),
  );
}

/** `{ registry, repository, tag }` for `name:tag` (Docker Hub when no host). */
export function parseReference(reference) {
  const colon = reference.lastIndexOf(':');
  const name = reference.slice(0, colon);
  const tag = reference.slice(colon + 1);
  const parts = name.split('/');
  const hasHost = parts.length > 1 && /[.:]|^localhost$/.test(parts[0]);
  const registry = hasHost ? parts[0] : 'docker.io';
  const path = hasHost ? parts.slice(1) : parts;
  if (registry === 'docker.io' && path.length === 1) path.unshift('library');
  return {
    registry: registry === 'docker.io' ? 'registry-1.docker.io' : registry,
    repository: path.join('/'),
    tag,
  };
}

/**
 * The digest the registry currently serves for `reference` (the image index,
 * for a multi-arch tag), using anonymous pull access. `fetchImpl` is injected
 * for tests.
 */
export async function resolveDigest(reference, fetchImpl = fetch) {
  const { registry, repository, tag } = parseReference(reference);
  const url = `https://${registry}/v2/${repository}/manifests/${tag}`;
  const headers = { accept: MANIFEST_TYPES.join(', ') };
  let response = await fetchImpl(url, { method: 'HEAD', headers });
  if (response.status === 401) {
    const challenge = /Bearer realm="([^"]+)"(?:,service="([^"]*)")?/.exec(
      response.headers.get('www-authenticate') ?? '',
    );
    assert.ok(
      challenge,
      `${registry} asked for credentials; anonymous pull is not available.`,
    );
    const tokenUrl = new URL(challenge[1]);
    if (challenge[2]) tokenUrl.searchParams.set('service', challenge[2]);
    tokenUrl.searchParams.set('scope', `repository:${repository}:pull`);
    const granted = await fetchImpl(tokenUrl);
    assert.equal(
      granted.status,
      200,
      `Token request for ${reference} failed: ${granted.status}`,
    );
    const { token } = await granted.json();
    assert.equal(typeof token, 'string', `No token for ${reference}.`);
    response = await fetchImpl(url, {
      method: 'HEAD',
      headers: { ...headers, authorization: `Bearer ${token}` },
    });
  }
  assert.equal(
    response.status,
    200,
    `${reference}: registry answered ${response.status}.`,
  );
  const digest = response.headers.get('docker-content-digest') ?? '';
  assert.match(
    digest,
    /^sha256:[0-9a-f]{64}$/,
    `${reference}: no content digest in the response.`,
  );
  return digest.slice('sha256:'.length);
}

/**
 * Runs the tool. `check` is offline; otherwise each distinct pin is resolved
 * and the lines to change are printed to `out`. Returns the number of changed
 * pins in resolve mode. Refuses to resolve when `env.CI` is set.
 */
export async function run({ root, check, env, fetchImpl, out }) {
  const pins = checkPins(await readPinFiles(root));
  if (check) {
    out(`${pins.length} image pins are well formed and consistent.`);
    return 0;
  }
  assert.ok(
    !env.CI,
    'Resolving digests needs the network and a human review; it does not run in CI. Use --check there.',
  );
  let changed = 0;
  for (const pin of pins) {
    const latest = await resolveDigest(pin.reference, fetchImpl);
    if (latest === pin.digest) {
      out(`current   ${pin.reference}@sha256:${pin.digest}`);
      continue;
    }
    changed += 1;
    out(`CHANGED   ${pin.reference}`);
    out(`            pinned  sha256:${pin.digest}`);
    out(`            latest  sha256:${latest}`);
    for (const place of pin.places) out(`            edit    ${place}`);
  }
  out(
    changed === 0
      ? 'All pins match the registries. Nothing to change.'
      : `${changed} pin(s) differ. Edit the lines above, then follow the checklist in docs/release-packages.md.`,
  );
  return changed;
}

async function main() {
  const root = fileURLToPath(new URL('../', import.meta.url));
  const args = process.argv.slice(2);
  assert.ok(
    args.every((arg) => arg === '--check'),
    'Usage: refresh-base-digests.mjs [--check]',
  );
  await run({
    root,
    check: args.includes('--check'),
    env: process.env,
    fetchImpl: fetch,
    out: (line) => console.log(line),
  });
}

if (process.argv[1] === fileURLToPath(import.meta.url)) await main();
