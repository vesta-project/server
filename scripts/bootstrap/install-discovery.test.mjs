import assert from 'node:assert/strict';
import { createHash, generateKeyPairSync, sign } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

const source = await readFile(new URL('./install.sh', import.meta.url), 'utf8');
const productionKey = '84e1fa3eae990d870d671c0fb585a62906f50d91e3382f3cc9f025d904c8dd8a';
const bash = process.platform === 'win32' ? path.join(process.env.ProgramFiles ?? 'C:/Program Files', 'Git/bin/bash.exe') : 'bash';
const pythonProbe = spawnSync(process.env.VESTA_BOOTSTRAP_TEST_PYTHON ?? 'python3', ['-c', 'import sys; print(sys.executable)'], { encoding: 'utf8' });
const python = pythonProbe.stdout?.trim() ?? '';
const canRun = spawnSync(bash, ['-c', 'openssl version'], { encoding: 'utf8' }).status === 0
  && pythonProbe.status === 0;
const options = { skip: !canRun && process.platform === 'win32' };
const first = 'nightly-20261110-111111111111';
const newest = 'nightly-20261112-222222222222';
const api = 'https://api.github.com/repos/vesta-project/server/releases';
const base = tag => `https://github.com/vesta-project/server/releases/download/${tag}`;
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const quote = value => `'${value.replaceAll("'", "'\\''")}'`;

async function fixture(t, { tag = first, target = 'linux-x86_64', changeManifest, changeBytes, changeSignature, changePayload, releases, releaseObject, arch = 'x86_64', oversizeManifest = false } = {}) {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'vesta-discovery-test-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const bin = path.join(temp, 'bin');
  await mkdir(bin);
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const key = publicKey.export({ type: 'spki', format: 'der' }).subarray(-32).toString('hex');
  // Test copy only: production exposes no environment/CLI override of its key pin.
  assert.equal(source.split(productionKey).length, 2);
  const script = path.join(temp, 'install.sh');
  await writeFile(script, source.replace(productionKey, key));
  const payload = Buffer.from('#!/usr/bin/env bash\nprintf "%s\\n" "$@" > "$VESTA_TEST_CAPTURE"\n');
  const manifest = {
    schema: 1, version: tag, target,
    files: ['vesta-setup', 'vesta-agent', 'vesta-supervisor', 'vesta'].map((name, index) => ({ name, size: index ? 1 : payload.length, sha256: index ? 'a'.repeat(64) : digest(payload) })),
  };
  changeManifest?.(manifest);
  const bytes = changeBytes?.(manifest) ?? Buffer.from(JSON.stringify(manifest, null, 2) + '\n');
  const signature = sign(null, bytes, privateKey).toString('base64') + '\n';
  const release = { tag_name: tag, draft: false, prerelease: true, published_at: '2026-11-10T00:00:00Z', ...releaseObject };
  const responses = {
    [`${api}/tags/${tag}`]: Buffer.from(JSON.stringify(release)).toString('base64'),
    [`${api}?per_page=100`]: Buffer.from(JSON.stringify(releases ?? [release])).toString('base64'),
    [`${base(tag)}/${target}-manifest.json`]: Buffer.from(oversizeManifest ? 'x'.repeat(65537) : bytes).toString('base64'),
    [`${base(tag)}/${target}-manifest.sig`]: Buffer.from(changeSignature?.(signature) ?? signature).toString('base64'),
    [`${base(tag)}/${target}-vesta-setup`]: Buffer.from(changePayload?.(payload) ?? payload).toString('base64'),
  };
  await writeFile(path.join(temp, 'responses.json'), JSON.stringify(responses));
  await writeFile(path.join(temp, 'curl.mjs'), `import fs from 'node:fs';
const args = process.argv.slice(2), url = args.at(-1);
fs.appendFileSync(process.env.VESTA_TEST_REQUESTS, JSON.stringify(args) + '\\n');
const responses = JSON.parse(fs.readFileSync(process.env.VESTA_TEST_RESPONSES));
if (!(url in responses)) process.exit(22);
process.stdout.write(Buffer.from(responses[url], 'base64'));
`);
  await writeFile(path.join(bin, 'curl'), `#!/usr/bin/env bash\nexec ${quote(process.execPath.replaceAll('\\', '/'))} ${quote(path.join(temp, 'curl.mjs').replaceAll('\\', '/'))} "$@"\n`, { mode: 0o700 });
  await writeFile(path.join(bin, 'python3'), `#!/usr/bin/env bash\nexec ${quote(python.replaceAll('\\', '/'))} "$@"\n`, { mode: 0o700 });
  await writeFile(path.join(bin, 'uname'), `#!/usr/bin/env bash\ncase "$1" in -s) echo Linux;; -m) echo ${quote(arch)};; *) exit 1;; esac\n`, { mode: 0o700 });
  const capture = path.join(temp, 'capture');
  const requests = path.join(temp, 'requests');
  const shellBin = bin.replaceAll('\\', '/').replace(/^([A-Za-z]):/, (_, drive) => `/${drive.toLowerCase()}`);
  const env = { ...process.env, VESTA_TEST_BIN: shellBin, TMPDIR: temp.replaceAll('\\', '/'), VESTA_TEST_CAPTURE: capture.replaceAll('\\', '/'), VESTA_TEST_REQUESTS: requests, VESTA_TEST_RESPONSES: path.join(temp, 'responses.json') };
  return {
    temp, capture,
    run: args => spawnSync(bash, ['-c', 'export PATH="$VESTA_TEST_BIN:$PATH"; script=$1; shift; source "$script"', 'bootstrap-test', script.replaceAll('\\', '/'), ...args], { encoding: 'utf8', env, timeout: 15000 }),
    requests: async () => (await readFile(requests, 'utf8').catch(() => '')).trim().split('\n').filter(Boolean).map(line => JSON.parse(line)),
  };
}

test('exact release verifies signature and setup bytes, forwards options, cleans private workspace', options, async t => {
  const f = await fixture(t);
  const result = f.run(['--tag', first, '--answers', '/fixture/answers with spaces.json', '--yes']);
  assert.equal(result.status, 0, result.stderr);
  const args = (await readFile(f.capture, 'utf8')).trim().split('\n');
  assert.equal(args[0], '--bundle-url');
  assert.equal(args[1], base(first));
  assert.match(args[3], /^[a-f0-9]{64}$/);
  assert.deepEqual(args.slice(6), ['--asset-prefix', 'linux-x86_64-', '--answers', '/fixture/answers with spaces.json', '--yes']);
  const requests = await f.requests();
  assert.equal(requests.length, 4);
  assert.deepEqual(requests.map(r => r.at(-1)), [`${api}/tags/${first}`, `${base(first)}/linux-x86_64-manifest.json`, `${base(first)}/linux-x86_64-manifest.sig`, `${base(first)}/linux-x86_64-vesta-setup`]);
  for (const r of requests) {
    assert.ok(r.includes('--proto-redir') && r.includes('=https'));
    assert.ok(r.includes('--max-redirs') && r.includes('--max-filesize') && r.includes('--max-time'));
  }
  const { readdir } = await import('node:fs/promises');
  assert.equal((await readdir(f.temp)).some(name => name.startsWith('vesta-setup-bootstrap.')), false);
});

test('latest nightly chooses latest published nightly rather than stable, draft or list order', options, async t => {
  const r = (tag, published_at, extra = {}) => ({ tag_name: tag, published_at, draft: false, prerelease: true, ...extra });
  const f = await fixture(t, { tag: newest, releases: [r(first, '2026-11-10T00:00:00Z'), r('v1.0', '2026-11-13T00:00:00Z', { prerelease: false }), r('nightly-20261114-333333333333', '2026-11-14T00:00:00Z', { draft: true }), r(newest, '2026-11-12T00:00:00Z')] });
  const result = f.run(['--latest-nightly']);
  assert.equal(result.status, 0, result.stderr);
  assert.equal((await f.requests())[0].at(-1), `${api}?per_page=100`);
  assert.equal((await readFile(f.capture, 'utf8')).trim().split('\n')[1], base(newest));
});

test('ARM64 uses signed ARM64 manifest and artifacts; uninstall still uses native confirmation', options, async t => {
  const f = await fixture(t, { arch: 'aarch64', target: 'linux-aarch64' });
  const result = f.run(['--tag', first, '--uninstall', '--root', '/fixture/host', '--plan']);
  assert.equal(result.status, 0, result.stderr);
  const args = (await readFile(f.capture, 'utf8')).trim().split('\n');
  assert.equal(args[0], 'uninstall');
  assert.deepEqual(args.slice(7), ['--asset-prefix', 'linux-aarch64-', '--root', '/fixture/host', '--plan']);
});

for (const [name, change, error] of [
  ['tampered manifest signature', { changeSignature: s => `${s[0] === 'A' ? 'B' : 'A'}${s.slice(1)}` }, /signature verification failed/],
  ['malformed signature', { changeSignature: () => 'not base64' }, /Invalid manifest signature/],
  ['wrong signed target', { changeManifest: m => { m.target = 'linux-aarch64'; } }, /Signed manifest/],
  ['wrong signed release', { changeManifest: m => { m.version = newest; } }, /Signed manifest/],
  ['unexpected executable path', { changeManifest: m => { m.files[0].name = '../vesta-setup'; } }, /Signed manifest/],
  ['duplicate JSON identity', { changeBytes: m => Buffer.from(JSON.stringify(m).replace('"schema":1', '"schema":1,"schema":1')) }, /Signed manifest/],
  ['oversize metadata with no advertised length', { oversizeManifest: true }, /Download/],
  ['tampered setup with unchanged size', { changePayload: p => Buffer.concat([Buffer.from('X'), p.subarray(1)]) }, /SHA-256 mismatch/],
  ['truncated setup', { changePayload: p => p.subarray(0, p.length - 1) }, /Setup size mismatch/],
  ['missing latest nightly', { releases: [] }, /No matching published release/],
  ['exact API tag mismatch', { releaseObject: { tag_name: newest } }, /No matching published release/],
]) {
  test(`rejects ${name} before setup execution`, options, async t => {
    const f = await fixture(t, change);
    const result = f.run(change.releases ? ['--latest-nightly'] : ['--tag', first]);
    assert.notEqual(result.status, 0, result.stderr);
    assert.match(result.stderr, error);
    assert.equal(await readFile(f.capture).then(() => true, () => false), false);
  });
}

test('rejects selector ambiguity, URL injection, unsupported architecture and uninstall bypass before network', options, async t => {
  const f = await fixture(t);
  for (const args of [['--latest-nightly', '--tag', first], ['--tag', '../main'], ['--tag', 'x?token=bad'], ['--latest-nightly', '--release-key', 'a'.repeat(64)], ['--tag', first, '--uninstall', '--root', '/fixture', '--yes']]) {
    assert.equal(f.run(args).status, 2, args.join(' '));
  }
  assert.deepEqual(await f.requests(), []);
  const unsupported = await fixture(t, { arch: 'riscv64' });
  assert.equal(unsupported.run(['--latest-nightly']).status, 2);
  assert.deepEqual(await unsupported.requests(), []);
});
