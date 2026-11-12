import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { checkBootstrapKey } from './check-bootstrap-key.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const nightly = await readFile(path.join(root, '.github/workflows/nightly.yml'), 'utf8');
const bash = process.platform === 'win32' ? path.join(process.env.ProgramFiles ?? 'C:/Program Files', 'Git/bin/bash.exe') : 'bash';
const pin = (await readFile(path.join(root, 'scripts/bootstrap/install.sh'), 'utf8')).match(/^  release_key=([a-f0-9]{64})$/m)[1];
const quote = value => `'${value.replaceAll("'", "'\\''")}'`;

test('bootstrap pin guard rejects missing, ambiguous, malformed or mismatched trust pins', () => {
  const source = `  release_key=${pin}\n`;
  assert.doesNotThrow(() => checkBootstrapKey(source, pin));
  for (const [script, expected] of [['', pin], [source + source, pin], [source, 'invalid'], [source, undefined], [source, '0'.repeat(64)]]) {
    assert.throws(() => checkBootstrapKey(script, expected), /Installer public key does not match/);
  }
  const publish = nightly.split('\n  publish:')[1];
  assert.notEqual(publish.indexOf('check-bootstrap-key.mjs'), -1);
  assert.ok(publish.indexOf('check-bootstrap-key.mjs') < publish.indexOf('for target in'));
});

test('actual signing preflight rejects mismatched bootstrap key before writing signing material', async t => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'vesta-bootstrap-key-test-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const step = nightly.split('      - name: Require pinned production signing credentials\n')[1].split('      - name: Verify unsigned executables')[0];
  const run = step.split('        run: |\n')[1].trimEnd().split('\n').map(line => line.replace(/^          /, '')).join('\n');
  const env = { ...process.env, RUNNER_TEMP: temp.replaceAll('\\', '/'), VESTA_RELEASE_SIGNING_KEY: 'test-only fake private material', VESTA_RELEASE_PUBLIC_KEY: '0'.repeat(64) };
  const rejected = spawnSync(bash, ['-e', '-c', run], { cwd: root, env, encoding: 'utf8', timeout: 10000 });
  assert.notEqual(rejected.status, 0, 'mismatched bootstrap trust must stop signing preflight');
  assert.equal(await readFile(path.join(temp, 'vesta-release-signing-key.pem')).then(() => true, () => false), false);
  const accepted = spawnSync(bash, ['-e', '-c', run], { cwd: root, env: { ...env, VESTA_RELEASE_PUBLIC_KEY: pin.toUpperCase() }, encoding: 'utf8', timeout: 10000 });
  assert.equal(accepted.status, 0, accepted.stderr);
  assert.equal(await readFile(path.join(temp, 'vesta-release-signing-key.pem'), 'utf8'), env.VESTA_RELEASE_SIGNING_KEY);
});

test('documented command refuses failed or partial source downloads without executing received bytes', async t => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'vesta-bootstrap-fetch-test-'));
  t.after(() => rm(temp, { recursive: true, force: true }));
  const bin = path.join(temp, 'bin');
  await mkdir(bin);
  const capture = path.join(temp, 'executed');
  const script = `#!/usr/bin/env bash\nprintf '%s\\n' "$@" > "$VESTA_TEST_CAPTURE"\n`;
  await writeFile(path.join(temp, 'fetch.mjs'), `import fs from 'node:fs';
const args=process.argv.slice(2), index=args.findIndex(a=>a==='-o'||a==='--output');
const bytes=process.env.VESTA_TEST_EMPTY==='1'?'':${JSON.stringify(script)};
if(index>=0)fs.writeFileSync(args[index+1],bytes);else process.stdout.write(bytes);
process.exit(Number(process.env.VESTA_TEST_FETCH_STATUS));
`);
  await writeFile(path.join(bin, 'curl'), `#!/usr/bin/env bash\nexec ${quote(process.execPath.replaceAll('\\', '/'))} ${quote(path.join(temp, 'fetch.mjs').replaceAll('\\', '/'))} "$@"\n`, { mode: 0o700 });
  const readme = (await readFile(path.join(root, 'README.md'), 'utf8')).replaceAll('\r\n', '\n');
  const command = readme.split('```bash\n')[1].split('\n```')[0];
  const shellBin = bin.replaceAll('\\', '/').replace(/^([A-Za-z]):/, (_, drive) => `/${drive.toLowerCase()}`);
  const env = { ...process.env, VESTA_TEST_BIN: shellBin, VESTA_TEST_CAPTURE: capture.replaceAll('\\', '/'), TMPDIR: temp.replaceAll('\\', '/') };
  for (const empty of ['0', '1']) {
    const rejected = spawnSync(bash, ['-c', `export PATH="$VESTA_TEST_BIN:$PATH"; ${command}`], { encoding: 'utf8', env: { ...env, VESTA_TEST_FETCH_STATUS: '22', VESTA_TEST_EMPTY: empty }, timeout: 10000 });
    assert.notEqual(rejected.status, 0, 'curl failure must propagate out of documented command');
    assert.equal(await readFile(capture).then(() => true, () => false), false, 'partial script must never execute');
  }
  const accepted = spawnSync(bash, ['-c', `export PATH="$VESTA_TEST_BIN:$PATH"; ${command}`], { encoding: 'utf8', env: { ...env, VESTA_TEST_FETCH_STATUS: '0' }, timeout: 10000 });
  assert.equal(accepted.status, 0, accepted.stderr);
  assert.equal((await readFile(capture, 'utf8')).trim(), '', 'the default command leaves package choice inside native setup');
  for (const file of ['docs/operations/bootstrap-release.md', 'docs/operations/host-setup.md']) {
    assert.ok((await readFile(path.join(root, file), 'utf8')).replaceAll('\r\n', '\n').includes(command), `${file} uses the same safe command`);
  }
  const notes = nightly.split('cat > "$RUNNER_TEMP/release-notes.md" <<EOF\n')[1].split('\n          EOF')[0].split('\n').map(line => line.replace(/^          /, '')).join('\n');
  const rendered = spawnSync(bash, ['-c', `cat <<EOF\n${notes}\nEOF`], { encoding: 'utf8', env: { ...env, RELEASE_VERSION: 'nightly-test' }, timeout: 10000 });
  assert.equal(rendered.status, 0, rendered.stderr);
  assert.ok(rendered.stdout.replace(/^    /gm, '').includes(command.replace('bash "$installer"', 'bash "$installer" --tag nightly-test')), 'actual release-note heredoc retains the safe command without early shell expansion');
});
