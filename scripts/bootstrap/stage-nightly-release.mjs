#!/usr/bin/env node
import { createHash, createPublicKey, verify } from 'node:crypto';
import { constants as fsConstants, chmod, copyFile, lstat, mkdir, readFile, readdir, realpath } from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import { targetFiles, verifyBinaries } from './verify-binaries.mjs';
import { releaseCommands } from './release-commands.mjs';
import { bootstrapDescriptor } from './bootstrap-descriptor.mjs';

const artifactNames = ['manifest.json', 'manifest.sig', 'manifest.pub', 'commands.txt', 'release.json'];
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

function parseArgs(args) {
  const allowed = new Set(['package', 'target', 'version', 'public-key', 'site-root']);
  const options = {};
  for (let i = 0; i < args.length; i += 2) {
    const key = args[i];
    if (!key?.startsWith('--') || i + 1 >= args.length || !allowed.has(key.slice(2)) || options[key.slice(2)]) {
      throw new Error('Usage: node stage-nightly-release.mjs --package DIR --target TARGET --version TAG --public-key HEX --site-root DIR');
    }
    options[key.slice(2)] = args[i + 1];
  }
  if (Object.keys(options).length !== allowed.size) throw new Error('All staging options are required.');
  return options;
}

export async function stageNightlyRelease({ packageDir, target, version, publicKeyHex, siteRoot }) {
  if (!targetFiles[target]) throw new Error('Unsupported release target.');
  if (!/^[A-Za-z0-9._+-]{1,64}$/.test(version)) throw new Error('Invalid nightly version.');
  if (!/^[0-9a-f]{64}$/i.test(publicKeyHex)) throw new Error('Expected public key pin must be 64 hex characters.');

  const input = await realpath(packageDir);
  const presentNames = (await readdir(input)).sort();
  const hasUpdates=presentNames.includes('update.json')||presentNames.includes('update.sig');
  const hasBootstrap=presentNames.includes('bootstrap.txt')||presentNames.includes('bootstrap.sig');
  const expectedNames = [...targetFiles[target], ...artifactNames,...(hasUpdates?['update.json','update.sig']:[]),...(hasBootstrap?['bootstrap.txt','bootstrap.sig']:[])].sort();
  if (JSON.stringify(presentNames) !== JSON.stringify(expectedNames)) throw new Error('Signed package file set is incomplete or contains unexpected files.');
  for (const name of expectedNames) {
    const info = await lstat(path.join(input, name));
    if (!info.isFile() || info.isSymbolicLink() || info.size === 0) throw new Error(`Package entry is not a non-empty regular file: ${name}`);
  }
  await verifyBinaries(input, target);

  const manifestBytes = await readFile(path.join(input, 'manifest.json'));
  const manifest = JSON.parse(manifestBytes);
  if (manifest.schema !== 1 || manifest.version !== version || manifest.target !== target ||
      !Array.isArray(manifest.files) || manifest.files.length !== targetFiles[target].length ||
      manifest.files.some((file, index) => file.name !== targetFiles[target][index] || !Number.isSafeInteger(file.size) || file.size < 1 || !/^[0-9a-f]{64}$/.test(file.sha256))) {
    throw new Error('Manifest does not match the expected target, version, and executable list.');
  }
  for (const file of manifest.files) {
    const bytes = await readFile(path.join(input, file.name));
    if (bytes.length !== file.size || sha256(bytes) !== file.sha256) throw new Error(`Manifest hash or size mismatch: ${file.name}`);
  }

  const publicRaw = Buffer.from(publicKeyHex, 'hex');
  const packagePub = Buffer.from((await readFile(path.join(input, 'manifest.pub'), 'utf8')).trim(), 'base64');
  if (packagePub.length !== 32 || !packagePub.equals(publicRaw)) throw new Error('Package public key does not match the configured release pin.');
  const publicKey = createPublicKey({
    key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), publicRaw]),
    format: 'der',
    type: 'spki',
  });
  const signature = Buffer.from((await readFile(path.join(input, 'manifest.sig'), 'utf8')).trim(), 'base64');
  if (signature.length !== 64 || !verify(null, manifestBytes, publicKey, signature)) throw new Error('Manifest signature verification failed.');

  const release = JSON.parse(await readFile(path.join(input, 'release.json'), 'utf8'));
  const manifestHash = sha256(manifestBytes);
  if (hasBootstrap) {
    const descriptor = await readFile(path.join(input, 'bootstrap.txt'));
    const signature = Buffer.from((await readFile(path.join(input, 'bootstrap.sig'), 'utf8')).trim(), 'base64');
    const expected = bootstrapDescriptor({ version, target, setupSha256: manifest.files[0].sha256, setupSize: manifest.files[0].size, manifestSha256: manifestHash });
    if (!descriptor.equals(expected) || signature.length !== 64 || !verify(null, descriptor, publicKey, signature)) {
      throw new Error('Bootstrap descriptor signature or manifest binding is invalid.');
    }
  }
  if(hasUpdates) {
    const bytes=await readFile(path.join(input,'update.json'));
    const signature=Buffer.from((await readFile(path.join(input,'update.sig'),'utf8')).trim(),'base64');
    if(bytes.length>16384||signature.length!==64||!verify(null,bytes,publicKey,signature)) throw new Error('Update metadata signature verification failed.');
    const update=JSON.parse(bytes);const now=Date.now();
    if(update.schema!==1||update.purpose!=='vesta-update-release-v1'||update.channel!=='nightly'||!Number.isSafeInteger(update.sequence)||update.sequence<1||update.target!==target||update.version!==version||update.manifest_sha256!==manifestHash||!Number.isSafeInteger(update.issued_at_ms)||!Number.isSafeInteger(update.expires_at_ms)||update.issued_at_ms>now+300000||update.expires_at_ms<=now||update.expires_at_ms<=update.issued_at_ms||update.expires_at_ms-update.issued_at_ms>30*86400000) throw new Error('Update metadata does not match this release or is expired.');
  }
  if (release.schema !== 1 || release.version !== version || release.target !== target ||
      release.setupSha256 !== manifest.files[0].sha256 || release.manifestSha256 !== manifestHash ||
      release.releaseKey !== publicRaw.toString('hex')) throw new Error('Release metadata does not match the signed manifest and public key pin.');
  const setupUrl = new URL(release.setupUrl);
  const bundleUrl = new URL(release.bundleUrl);
  const expectedBundlePath = `/vesta-project/server/releases/download/${version}`;
  const assetPrefix = `${target}-`;
  if (setupUrl.protocol !== 'https:' || bundleUrl.protocol !== 'https:' || setupUrl.username || setupUrl.password ||
      bundleUrl.username || bundleUrl.password || setupUrl.search || setupUrl.hash || bundleUrl.search || bundleUrl.hash ||
      setupUrl.origin !== 'https://github.com' || bundleUrl.origin !== 'https://github.com' ||
      bundleUrl.pathname !== expectedBundlePath ||
      setupUrl.pathname !== `${bundleUrl.pathname}/${assetPrefix}${targetFiles[target][0]}` ||
      release.assetPrefix !== assetPrefix) {
    throw new Error('Release setup and bundle URLs are invalid or inconsistent.');
  }
  const commands = await readFile(path.join(input, 'commands.txt'), 'utf8');
  if (commands !== releaseCommands(release)) {
    throw new Error('Bootstrap commands differ from the canonical commands for the verified release pins.');
  }

  const destinationRoot = path.resolve(siteRoot, 'nightly', version, target);
  try {
    await lstat(destinationRoot);
    throw new Error('Nightly destination already exists; refusing to overwrite published files.');
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  await mkdir(destinationRoot, { recursive: true });
  try {
    for (const name of expectedNames) {
      const destination = path.join(destinationRoot, name);
      await copyFile(path.join(input, name), destination, fsConstants.COPYFILE_EXCL);
      if (target.startsWith('linux-') && targetFiles[target].includes(name)) await chmod(destination, 0o755);
    }
  } catch (error) {
    const { rm } = await import('node:fs/promises');
    await rm(destinationRoot, { recursive: true, force: true });
    throw error;
  }
  return destinationRoot;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const options = parseArgs(process.argv.slice(2));
  const destination = await stageNightlyRelease({
    packageDir: options.package,
    target: options.target,
    version: options.version,
    publicKeyHex: options['public-key'],
    siteRoot: options['site-root'],
  });
  console.log(`Verified and staged ${options.target} ${options.version} in ${destination}`);
}
