#!/usr/bin/env node
import { createPrivateKey, createPublicKey, sign } from 'node:crypto';
import { chmod, copyFile, lstat, mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';
import { releaseCommands } from './release-commands.mjs';
import { targetFiles, verifyBinaries } from './verify-binaries.mjs';

function usage() { throw new Error('Usage: node package-release.mjs --binaries DIR --output NEW_DIR --target windows-x86_64|linux-x86_64|linux-aarch64 --base-url HTTPS_URL --signing-key PKCS8_PEM --version VERSION [--asset-prefix TARGET_PREFIX] [--channel stable|nightly --sequence INTEGER]'); }
const options = {};
const args = process.argv.slice(2);
for (let i = 0; i < args.length; i += 2) {
  const key = args[i];
  if (!key?.startsWith('--') || i + 1 >= args.length) usage();
  const name = key.slice(2);
  if (!['binaries', 'output', 'target', 'base-url', 'signing-key', 'version', 'asset-prefix', 'sequence', 'channel'].includes(name) || options[name]) usage();
  options[name] = args[i + 1];
}
if (['binaries', 'output', 'target', 'base-url', 'signing-key', 'version'].some(name => !options[name])) usage();
if (!targetFiles[options.target]) throw new Error('Unsupported target.');
const hasUpdates=options.sequence!==undefined||options.channel!==undefined;
const sequence=Number(options.sequence);
if(hasUpdates&&(!Number.isSafeInteger(sequence)||sequence<1||!/^\d+$/.test(options.sequence??'')||!['stable','nightly'].includes(options.channel))) throw new Error('Update metadata needs a positive safe-integer --sequence and --channel stable|nightly.');
if (options['asset-prefix'] && options['asset-prefix'] !== `${options.target}-`) throw new Error('--asset-prefix must exactly match the supported target prefix.');
const base = new URL(options['base-url']);
if (base.protocol !== 'https:' || base.username || base.password || base.search || base.hash) throw new Error('base-url must be a clean HTTPS release directory URL.');
base.pathname = base.pathname.replace(/\/?$/, '/');
if (!/^[A-Za-z0-9._+-]{1,64}$/.test(options.version)) throw new Error('version must contain 1 to 64 ASCII letters, digits, dot, underscore, plus, or hyphen.');

const binaries = await realpath(options.binaries);
const output = path.resolve(options.output);
const signingPath = await realpath(options['signing-key']);
const outParent = path.dirname(output);
await mkdir(outParent, { recursive: true });
const realParent = await realpath(outParent);
const resolvedOutput = path.join(realParent, path.basename(output));
const inside = (child, parent) => child === parent || child.startsWith(parent + path.sep);
if (inside(resolvedOutput, binaries) || inside(binaries, resolvedOutput)) throw new Error('output must be separate from the binaries input directory.');
try { await lstat(resolvedOutput); throw new Error('output directory already exists; choose a new directory.'); } catch (e) { if (e.code !== 'ENOENT') throw e; }
if (await stat(signingPath).then(s => !s.isFile())) throw new Error('signing key must be a file.');

const names = await verifyBinaries(binaries, options.target);
if (options.target.startsWith('windows-') && process.platform !== 'win32') throw new Error('Windows target packaging must run on Windows.');
if (options.target.startsWith('linux-') && process.platform !== 'linux') throw new Error('Linux target packaging must run on Linux.');

await mkdir(resolvedOutput, { recursive: false });
try {
  const files = [];
  for (const name of names) {
    const source = path.join(binaries, name);
    const destination = path.join(resolvedOutput, name);
    await copyFile(source, destination);
    if (options.target.startsWith('linux-')) await chmod(destination, 0o755);
    const bytes = await readFile(destination);
    const { createHash } = await import('node:crypto');
    files.push({ name, sha256: createHash('sha256').update(bytes).digest('hex'), size: bytes.length });
  }
  const manifest = { schema: 1, version: options.version, target: options.target, files };
  const manifestBytes = Buffer.from(JSON.stringify(manifest, null, 2) + '\n', 'utf8');
  const privateKey = createPrivateKey(await readFile(signingPath));
  if (privateKey.asymmetricKeyType !== 'ed25519') throw new Error('signing key must be Ed25519 PKCS8 PEM.');
  const publicDer = createPublicKey(privateKey).export({ type: 'spki', format: 'der' });
  const publicRaw = publicDer.subarray(-32);
  const signature = sign(null, manifestBytes, privateKey);
  await writeFile(path.join(resolvedOutput, 'manifest.json'), manifestBytes, { flag: 'wx' });
  await writeFile(path.join(resolvedOutput, 'manifest.sig'), signature.toString('base64') + '\n', { flag: 'wx' });
  await writeFile(path.join(resolvedOutput, 'manifest.pub'), publicRaw.toString('base64') + '\n', { flag: 'wx' });
  const setupName = names[0];
  const setupHash = files[0].sha256;
  const manifestHash = (await import('node:crypto')).createHash('sha256').update(manifestBytes).digest('hex');
  if(hasUpdates) {
    const issued=Date.now();
    const metadata={schema:1,purpose:'vesta-update-release-v1',channel:options.channel,sequence,version:options.version,target:options.target,manifest_sha256:manifestHash,issued_at_ms:issued,expires_at_ms:issued+7*24*60*60*1000};
    const bytes=Buffer.from(JSON.stringify(metadata,null,2)+'\n');
    await writeFile(path.join(resolvedOutput,'update.json'),bytes,{flag:'wx'});
    await writeFile(path.join(resolvedOutput,'update.sig'),sign(null,bytes,privateKey).toString('base64')+'\n',{flag:'wx'});
  }
  const assetPrefix = options['asset-prefix'] ?? '';
  const setupUrl = new URL(`${assetPrefix}${setupName}`, base).href;
  const bundleUrl = base.href.replace(/\/$/, '');
  const keyHex = publicRaw.toString('hex');
  const release = { schema: 1, version: options.version, target: options.target, setupUrl, setupSha256: setupHash, bundleUrl, ...(assetPrefix ? { assetPrefix } : {}), manifestSha256: manifestHash, releaseKey: keyHex };
  await writeFile(path.join(resolvedOutput, 'commands.txt'), releaseCommands(release), { flag: 'wx' });
  await writeFile(path.join(resolvedOutput, 'release.json'), JSON.stringify(release, null, 2) + '\n', { flag: 'wx' });
  console.log(`Packaged ${options.target} ${options.version} into ${resolvedOutput}`);
} catch (error) {
  const { rm } = await import('node:fs/promises');
  const safe = path.resolve(resolvedOutput);
  if (safe === resolvedOutput && path.dirname(safe) === realParent && path.basename(safe) === path.basename(output)) await rm(safe, { recursive: true, force: true });
  throw error;
}
