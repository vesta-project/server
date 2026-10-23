import assert from 'node:assert/strict';
import { createHash, generateKeyPairSync, verify } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const script = fileURLToPath(new URL('./package-release.mjs', import.meta.url));
const target = process.platform === 'win32' ? 'windows-x86_64' : process.platform === 'linux' && process.arch === 'x64' ? 'linux-x86_64' : process.platform === 'linux' && process.arch === 'arm64' ? 'linux-aarch64' : null;
const names = target?.startsWith('windows-')
  ? ['vesta-setup.exe', 'vesta-agent.exe', 'vesta-supervisor.exe', 'vesta.exe']
  : ['vesta-setup', 'vesta-agent', 'vesta-supervisor', 'vesta'];

async function fixture(t) {
  const root = await mkdtemp(path.join(os.tmpdir(), 'vesta-package-test-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const binaries = path.join(root, 'binaries');
  const output = path.join(root, 'release');
  await mkdir(binaries);
  for (const [index, name] of names.entries()) {
    await writeFile(path.join(binaries, name), Buffer.from(`fixture binary ${index}\n`));
  }
  const { privateKey } = generateKeyPairSync('ed25519');
  const signingKey = path.join(root, 'signing-key.pem');
  await writeFile(signingKey, privateKey.export({ type: 'pkcs8', format: 'pem' }), { mode: 0o600 });
  return { root, binaries, output, signingKey };
}

function packageArgs(f, output = f.output) {
  return [
    script,
    '--binaries', f.binaries,
    '--output', output,
    '--target', target,
    '--base-url', 'https://downloads.example.invalid/vesta/1.2.3/test-target/',
    '--signing-key', f.signingKey,
    '--version', '1.2.3',
  ];
}

function run(args) {
  return spawnSync(process.execPath, args, { encoding: 'utf8' });
}

test('packager emits a pinned manifest, a valid detached signature, and isolated one-paste commands', { skip: !target }, async t => {
  const f = await fixture(t);
  const result = run(packageArgs(f));
  assert.equal(result.status, 0, result.stderr);

  const manifestBytes = await readFile(path.join(f.output, 'manifest.json'));
  const manifest = JSON.parse(manifestBytes);
  assert.deepEqual(Object.keys(manifest), ['schema', 'version', 'target', 'files']);
  assert.equal(manifest.schema, 1);
  assert.equal(manifest.target, target);
  assert.equal(manifest.version, '1.2.3');
  assert.deepEqual(manifest.files.map(file => file.name), names);
  for (const artifact of manifest.files) {
    const bytes = await readFile(path.join(f.output, artifact.name));
    assert.equal(artifact.size, bytes.length);
    assert.match(artifact.sha256, /^[0-9a-f]{64}$/);
    assert.equal(createHash('sha256').update(bytes).digest('hex'), artifact.sha256);
  }

  const signature = Buffer.from((await readFile(path.join(f.output, 'manifest.sig'), 'utf8')).trim(), 'base64');
  const publicRaw = Buffer.from((await readFile(path.join(f.output, 'manifest.pub'), 'utf8')).trim(), 'base64');
  assert.equal(publicRaw.length, 32);
  const publicKey = await import('node:crypto').then(({ createPublicKey }) => createPublicKey({
    key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), publicRaw]),
    format: 'der',
    type: 'spki',
  }));
  assert.equal(verify(null, manifestBytes, publicKey, signature), true);

  const release = JSON.parse(await readFile(path.join(f.output, 'release.json'), 'utf8'));
  assert.equal(release.setupSha256, manifest.files[0].sha256);
  assert.equal(release.manifestSha256, createHash('sha256').update(manifestBytes).digest('hex'));
  assert.equal(release.releaseKey, publicRaw.toString('hex'));
  const commands = await readFile(path.join(f.output, 'commands.txt'), 'utf8');
  assert.match(commands, /vesta-setup-bootstrap/);
  assert.match(commands, /67108865/);
  assert.match(commands, new RegExp(release.setupSha256));
  assert.match(commands, new RegExp(release.manifestSha256));
  assert.match(commands, new RegExp(release.releaseKey));
  assert.doesNotMatch(result.stdout + result.stderr, /BEGIN PRIVATE KEY/);

  if (process.platform === 'win32') {
    const powershell = commands.split('PowerShell (interactive):\n')[1];
    assert.ok(powershell, 'generated PowerShell section exists');
    const psPath = path.join(f.root, 'generated-command.ps1');
    await writeFile(psPath, powershell);
    const psLiteral = psPath.replaceAll("'", "''");
    const parse = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', `$tokens=$null; $errors=$null; [System.Management.Automation.Language.Parser]::ParseFile('${psLiteral}', [ref]$tokens, [ref]$errors) > $null; if ($errors.Count) { $errors | ForEach-Object { [Console]::Error.WriteLine($_.Message) }; exit 1 }`], { encoding: 'utf8' });
    if (parse.error?.code === 'ENOENT') t.diagnostic('PowerShell AST parser unavailable; skipped generated PowerShell parse.');
    else assert.equal(parse.status, 0, parse.stderr);
  }
});

test('packager refuses existing output directories without altering them', { skip: !target }, async t => {
  const f = await fixture(t);
  await mkdir(f.output);
  const sentinel = path.join(f.output, 'keep.txt');
  await writeFile(sentinel, 'preserve');
  const result = run(packageArgs(f));
  assert.notEqual(result.status, 0);
  assert.equal(await readFile(sentinel, 'utf8'), 'preserve');
});

test('packager refuses output directories inside the binaries input', { skip: !target }, async t => {
  const f = await fixture(t);
  const nested = path.join(f.binaries, 'release-output');
  const result = run(packageArgs(f, nested));
  assert.notEqual(result.status, 0);
  await assert.rejects(readFile(path.join(nested, 'manifest.json')));
});
