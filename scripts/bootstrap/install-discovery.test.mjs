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
const canRun = spawnSync(bash, ['-c', 'openssl version'], { encoding: 'utf8' }).status === 0;
const options = { skip: !canRun && process.platform === 'win32' };
const first = 'nightly-20261110-111111111111';
const newest = 'nightly-20261112-222222222222';
const installerTag = 'installer-20261113-333333333333';
const feedUrl = 'https://github.com/vesta-project/server/releases.atom';
const base = tag => `https://github.com/vesta-project/server/releases/download/${tag}`;
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const quote = value => `'${value.replaceAll("'", "'\\''")}'`;

async function fixture(t, { tag = first, target = 'linux-x86_64', changeManifest, changeBytes, changeSignature, changePayload, releases, feed, arch = 'x86_64', oversizeManifest = false, independent = false } = {}) {
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
  const manifestHash = digest(Buffer.from(JSON.stringify(manifest)));
  const fields = [independent ? 'vesta-installer-bootstrap-v1' : 'vesta-setup-bootstrap-v1', manifest.version, manifest.target, manifest.files[0].name, manifest.files[0].sha256, String(manifest.files[0].size), independent ? '1' : manifestHash];
  const bytes = changeBytes?.(fields) ?? Buffer.from(fields.join('\n') + '\n');
  const signature = sign(null, bytes, privateKey).toString('base64') + '\n';
  const release = { tag_name: tag, draft: false };
  const atom = feed ?? `<feed>\n${(releases ?? [release]).filter(r => !r.draft).map(r => `<entry>\n<link rel="alternate" type="text/html" href="https://github.com/vesta-project/server/releases/tag/${r.tag_name}"/>\n<content type="html">&lt;link href="https://example.invalid/"/&gt;</content>\n</entry>`).join('\n')}\n</feed>\n`;
  const responses = {
    [feedUrl]: Buffer.from(atom).toString('base64'),
    [`${base(tag)}/${target}-bootstrap.txt`]: Buffer.from(oversizeManifest ? 'x'.repeat(65537) : bytes).toString('base64'),
    [`${base(tag)}/${target}-bootstrap.sig`]: Buffer.from(changeSignature?.(signature) ?? signature).toString('base64'),
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
  for (const dependency of ['python3', 'python', 'jq', 'node']) {
    await writeFile(path.join(bin, dependency), '#!/usr/bin/env bash\necho unexpected > "$VESTA_TEST_DEPENDENCY"\nexit 97\n', { mode: 0o700 });
  }
  await writeFile(path.join(bin, 'uname'), `#!/usr/bin/env bash\ncase "$1" in -s) echo Linux;; -m) echo ${quote(arch)};; *) exit 1;; esac\n`, { mode: 0o700 });
  const capture = path.join(temp, 'capture');
  const requests = path.join(temp, 'requests');
  const shellBin = bin.replaceAll('\\', '/').replace(/^([A-Za-z]):/, (_, drive) => `/${drive.toLowerCase()}`);
  const env = { ...process.env, VESTA_TEST_BIN: shellBin, VESTA_TEST_DEPENDENCY: path.join(temp, 'unexpected-dependency').replaceAll('\\', '/'), TMPDIR: temp.replaceAll('\\', '/'), VESTA_TEST_CAPTURE: capture.replaceAll('\\', '/'), VESTA_TEST_REQUESTS: requests, VESTA_TEST_RESPONSES: path.join(temp, 'responses.json') };
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
  assert.deepEqual(requests.map(r => r.at(-1)), [feedUrl,`${base(first)}/linux-x86_64-bootstrap.txt`, `${base(first)}/linux-x86_64-bootstrap.sig`, `${base(first)}/linux-x86_64-vesta-setup`]);
  assert.equal(await readFile(path.join(f.temp, 'unexpected-dependency')).then(() => true, () => false), false, 'Python, jq and Node are not invoked by the installer');
  for (const r of requests) {
    assert.ok(r.includes('--proto-redir') && r.includes('=https'));
    assert.ok(r.includes('--max-redirs') && r.includes('--max-filesize') && r.includes('--max-time'));
  }
  const { readdir } = await import('node:fs/promises');
  assert.equal((await readdir(f.temp)).some(name => name.startsWith('vesta-setup-bootstrap.')), false);
});

test('latest nightly chooses the first canonical nightly entry in the published release feed', options, async t => {
  const r = (tag, published_at, extra = {}) => ({ tag_name: tag, published_at, draft: false, prerelease: true, ...extra });
  const f = await fixture(t, { tag: newest, releases: [r('v1.0', '2026-11-13T00:00:00Z', { prerelease: false }), r('nightly-20261114-333333333333', '2026-11-14T00:00:00Z', { draft: true }), r(newest, '2026-11-12T00:00:00Z'), r(first, '2026-11-10T00:00:00Z')] });
  const result = f.run(['--latest-nightly']);
  assert.equal(result.status, 0, result.stderr);
  assert.equal((await f.requests())[0].at(-1), feedUrl);
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

test('release-note content cannot inject a candidate and release history never fetches asset inventories', options, async t => {
  const feed = `<feed>\n<entry>\n<link rel="alternate" href="https://github.com/vesta-project/server/releases/tag/v1.0"/>\n<content type="html"><![CDATA[\n<entry>\n<link rel="alternate" href="https://github.com/vesta-project/server/releases/tag/${first}"/>\n</entry>\n]]></content>\n</entry>\n<entry>\n<link rel="alternate" href="https://github.com/vesta-project/server/releases/tag/${newest}"/>\n</entry>\n</feed>\n`;
  const f = await fixture(t, { tag: newest, feed });
  const result = f.run(['--latest-nightly']);
  assert.equal(result.status, 0, result.stderr);
  const requests = await f.requests();
  assert.equal(requests[0].at(-1), feedUrl);
  assert.equal(requests.length, 4);
  assert.equal(requests.some(r => r.at(-1).includes('api.github.com')), false);
  assert.equal((await readFile(f.capture, 'utf8')).trim().split('\n')[1], base(newest));
});

for (const [name, change, error] of [
  ['tampered descriptor signature', { changeSignature: s => `${s[0] === 'A' ? 'B' : 'A'}${s.slice(1)}` }, /signature verification failed/],
  ['malformed signature', { changeSignature: () => 'not base64' }, /Invalid bootstrap signature/],
  ['wrong signed target', { changeManifest: m => { m.target = 'linux-aarch64'; } }, /Signed bootstrap descriptor/],
  ['wrong signed release', { changeManifest: m => { m.version = newest; } }, /Signed bootstrap descriptor/],
  ['wrong signed purpose', { changeBytes: fields => Buffer.from(['other-purpose', ...fields.slice(1), ''].join('\n')) }, /Signed bootstrap descriptor/],
  ['oversize signed setup', { changeManifest: m => { m.files[0].size = 67108865; } }, /Setup exceeds size limit/],
  ['unexpected executable path', { changeManifest: m => { m.files[0].name = '../vesta-setup'; } }, /Signed bootstrap descriptor/],
  ['duplicate descriptor identity', { changeBytes: fields => Buffer.from([...fields, fields[1], ''].join('\n')) }, /Signed bootstrap descriptor/],
  ['embedded NUL in signed identity', { changeBytes: fields => Buffer.from(fields.join('\n').replace('vesta-setup\n', 'vesta-setup\0\n') + '\n') }, /Noncanonical bootstrap descriptor/],
  ['oversize release feed', { feed: 'x'.repeat(1048577), releases: [] }, /Download/],
  ['oversize metadata with no advertised length', { oversizeManifest: true }, /Download/],
  ['tampered setup with unchanged size', { changePayload: p => Buffer.concat([Buffer.from('X'), p.subarray(1)]) }, /SHA-256 mismatch/],
  ['truncated setup', { changePayload: p => p.subarray(0, p.length - 1) }, /Setup size mismatch/],
  ['missing latest nightly', { releases: [] }, /No published installer or nightly/],
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

test('newest installer is independent of requested package and permits interactive version selection', options, async t => {
  for (const [args, expected] of [[[],['install']], [['--latest-nightly'],['install','--channel','nightly']], [['--channel','stable'],['install','--channel','stable']], [['--tag',first],['install','--tag',first]]]) {
    const f = await fixture(t, {tag:installerTag, independent:true});
    const result = f.run(args);
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual((await readFile(f.capture,'utf8')).trim().split('\n'), expected);
    const requests = await f.requests();
    assert.equal(requests.length, 4);
    assert.equal(requests[1].at(-1), `${base(installerTag)}/linux-x86_64-bootstrap.txt`);
  }
});

test('installer compatibility purpose/schema cannot be replaced with server bootstrap pins', options, async t => {
  const f = await fixture(t, {tag:installerTag, independent:true, changeBytes:fields => Buffer.from([...fields.slice(0,6),'2',''].join('\n'))});
  const result = f.run([]);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Signed installer compatibility is invalid/);
  assert.equal(await readFile(f.capture).then(()=>true,()=>false), false);
});
