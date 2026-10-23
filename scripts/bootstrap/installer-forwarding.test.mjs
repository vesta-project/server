import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const shellScript = path.join(root, 'scripts/bootstrap/install.sh');
const requiredPins = ['--setup-url', 'https://example.invalid/setup', '--setup-sha256', 'a'.repeat(64), '--bundle-url', 'https://example.invalid/release', '--manifest-sha256', 'b'.repeat(64), '--release-key', 'c'.repeat(64)];

test('shell bootstrap forwards verified bundle pins and uninstall selection to native setup', { skip: process.platform === 'win32' }, async t => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'vesta-installer-forward-test-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const payload = path.join(temp, 'payload');
  const capture = path.join(temp, 'argv');
  await writeFile(payload, `#!/bin/sh\nprintf '%s\\n' "$@" > '${capture}'\n`, { mode: 0o700 });
  const checksum = createHash('sha256').update(await readFile(payload)).digest('hex');
  const bin = path.join(temp, 'bin');
  await import('node:fs/promises').then(({ mkdir }) => mkdir(bin));
  const curl = path.join(bin, 'curl');
  await writeFile(curl, `#!/bin/sh\ncat '${payload}'\n`, { mode: 0o700 });
  const args = [...requiredPins];
  args[3] = checksum;
  args.push('--asset-prefix', 'linux-x86_64-', '--uninstall', '--servers-root', '/srv/servers', '--server', 'alpha', '--plan');
  const result = spawnSync('bash', [shellScript, ...args], { encoding: 'utf8', env: { ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH}`, TMPDIR: temp } });
  assert.equal(result.status, 0, result.stderr);
  const forwarded = (await readFile(capture, 'utf8')).trim().split('\n');
  assert.deepEqual(forwarded, ['uninstall', '--bundle-url', 'https://example.invalid/release', '--manifest-sha256', 'b'.repeat(64), '--release-key', 'c'.repeat(64), '--asset-prefix', 'linux-x86_64-', '--servers-root', '/srv/servers', '--server', 'alpha', '--plan']);
});

test('both helpers reject uninstall confirmation bypasses and enforce target selection roots', async () => {
  const bash = await readFile(shellScript, 'utf8');
  const ps = await readFile(path.join(root, 'scripts/bootstrap/install.ps1'), 'utf8');
  assert.match(bash, /--yes is not supported for uninstall/);
  assert.match(bash, /--server requires --servers-root/);
  assert.match(bash, /Full Vesta uninstall requires --root/);
  assert.match(ps, /--yes is not supported for uninstall/);
  assert.match(ps, /--server requires --servers-root/);
  assert.match(ps, /Full Vesta uninstall requires --root/);
  assert.match(ps, /\$argv = @\('uninstall'\) \+ \$argv/);
  assert.match(ps, /'--asset-prefix', \$AssetPrefix/);
});

test('shell helper refuses uninstall bypass and missing registry root before downloading setup', { skip: process.platform === 'win32' }, () => {
  for (const extra of [['--uninstall', '--root', '/srv/vesta', '--yes'], ['--uninstall', '--server', 'alpha']]) {
    const result = spawnSync('bash', [shellScript, ...requiredPins, ...extra], { encoding: 'utf8' });
    assert.equal(result.status, 2);
    assert.match(result.stderr, extra.includes('--yes') ? /--yes is not supported for uninstall/ : /--server requires --servers-root/);
  }
});
