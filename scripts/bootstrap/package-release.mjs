#!/usr/bin/env node
import { createPrivateKey, createPublicKey, sign } from 'node:crypto';
import { copyFile, lstat, mkdir, readFile, readdir, realpath, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';

const expected = {
  'windows-x86_64': ['vesta-setup.exe', 'vesta-agent.exe', 'vesta-supervisor.exe', 'vesta.exe'],
  'linux-x86_64': ['vesta-setup', 'vesta-agent', 'vesta-supervisor', 'vesta'],
  'linux-aarch64': ['vesta-setup', 'vesta-agent', 'vesta-supervisor', 'vesta'],
};
function usage() { throw new Error('Usage: node package-release.mjs --binaries DIR --output NEW_DIR --target windows-x86_64|linux-x86_64|linux-aarch64 --base-url HTTPS_URL --signing-key PKCS8_PEM --version VERSION'); }
const options = {};
const args = process.argv.slice(2);
for (let i = 0; i < args.length; i += 2) {
  const key = args[i];
  if (!key?.startsWith('--') || i + 1 >= args.length) usage();
  const name = key.slice(2);
  if (!['binaries', 'output', 'target', 'base-url', 'signing-key', 'version'].includes(name) || options[name]) usage();
  options[name] = args[i + 1];
}
if (Object.keys(options).length !== 6) usage();
if (!expected[options.target]) throw new Error('Unsupported target.');
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

const names = expected[options.target];
const available = await readdir(binaries);
for (const name of names) {
  if (!available.includes(name)) throw new Error(`Missing required binary: ${name}`);
  const info = await lstat(path.join(binaries, name));
  if (!info.isFile() || info.isSymbolicLink()) throw new Error(`Required binary must be a regular file: ${name}`);
}
if (options.target.startsWith('windows-') && process.platform !== 'win32') throw new Error('Windows target packaging must run on Windows.');
if (options.target.startsWith('linux-') && process.platform !== 'linux') throw new Error('Linux target packaging must run on Linux.');

await mkdir(resolvedOutput, { recursive: false });
try {
  const files = [];
  for (const name of names) {
    const source = path.join(binaries, name);
    const destination = path.join(resolvedOutput, name);
    await copyFile(source, destination);
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
  const setupUrl = new URL(setupName, base).href;
  const bundleUrl = base.href.replace(/\/$/, '');
  const keyHex = publicRaw.toString('hex');
  const shQuote = value => `'${value.replaceAll("'", "'\\''")}'`;
  const psQuote = value => `'${value.replaceAll("'", "''")}'`;
  const localSetupName = options.target.startsWith('windows-') ? 'vesta-setup.exe' : 'vesta-setup';
  const bash = [
    '(',
    'set -euo pipefail',
    'tmp_root=$(cd -P -- "${TMPDIR:-/tmp}" && pwd)',
    'work=$(mktemp -d "$tmp_root/vesta-setup-bootstrap.XXXXXXXX")',
    'cleanup() { case "$work" in "$tmp_root"/vesta-setup-bootstrap.*) rm -rf -- "$work";; *) echo "Refusing unsafe cleanup." >&2;; esac; }',
    'trap cleanup EXIT HUP INT TERM',
    `cd -- "$work"`,
    `curl --fail --silent --show-error --location --max-time 180 --proto '=https' --proto-redir '=https' ${shQuote(setupUrl)} | head -c 67108865 > ${shQuote(localSetupName)}`,
    `size=$(wc -c < ${shQuote(localSetupName)} | tr -d '[:space:]'); (( size <= 67108864 )) || { echo 'Setup binary exceeds 64 MiB.' >&2; exit 1; }`,
    `printf '%s  %s\\n' '${setupHash}' ${shQuote(localSetupName)} | sha256sum --check -`,
    `chmod 700 ${shQuote(localSetupName)}`,
    `./${localSetupName} --bundle-url ${shQuote(bundleUrl)} --manifest-sha256 '${manifestHash}' --release-key '${keyHex}'`,
    ')',
  ].join('\n');
  const powershell = [
    "$ErrorActionPreference = 'Stop'",
    "$vestaTempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\\')",
    "$vestaWork = Join-Path $vestaTempRoot ('vesta-setup-bootstrap-' + [Guid]::NewGuid().ToString('N'))",
    "$vestaFullWork = [IO.Path]::GetFullPath($vestaWork)",
    "if (-not $vestaFullWork.StartsWith($vestaTempRoot + '\\', [StringComparison]::OrdinalIgnoreCase) -or [IO.Path]::GetFileName($vestaFullWork) -notmatch '^vesta-setup-bootstrap-[0-9a-f]{32}$') { throw 'Refusing unsafe temporary directory.' }",
    'New-Item -ItemType Directory -Path $vestaFullWork | Out-Null',
    'try {',
    `    $vestaUri = [Uri]${psQuote(setupUrl)}`,
    "    if ($vestaUri.Scheme -ne 'https' -or $vestaUri.UserInfo -or $vestaUri.Query -or $vestaUri.Fragment) { throw 'Invalid setup URL.' }",
    `    $vestaFile = Join-Path $vestaFullWork ${psQuote(localSetupName)}`,
    '    Add-Type -AssemblyName System.Net.Http',
    '    $vestaHandler = [Net.Http.HttpClientHandler]::new(); $vestaHandler.AllowAutoRedirect = $false',
    '    $vestaClient = [Net.Http.HttpClient]::new($vestaHandler)',
    '    $vestaCts = [Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds(180))',
    '    try {',
    '        $vestaRequest = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Get, $vestaUri)',
    '        $vestaResponse = $vestaClient.SendAsync($vestaRequest, [Net.Http.HttpCompletionOption]::ResponseHeadersRead, $vestaCts.Token).GetAwaiter().GetResult()',
    '        if ([int]$vestaResponse.StatusCode -ne 200) { throw \'Setup download failed.\' }',
    '        if ($vestaResponse.Content.Headers.ContentLength -gt 67108864) { throw \'Setup binary exceeds 64 MiB.\' }',
    '        $vestaStream = $vestaResponse.Content.ReadAsStreamAsync().GetAwaiter().GetResult()',
    '        $vestaOut = [IO.File]::Open($vestaFile, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)',
    '        try {',
    '            $vestaBuffer = New-Object byte[] 65536; [long]$vestaTotal = 0',
    '            while (($vestaRead = $vestaStream.ReadAsync($vestaBuffer, 0, $vestaBuffer.Length, $vestaCts.Token).GetAwaiter().GetResult()) -gt 0) { $vestaTotal += $vestaRead; if ($vestaTotal -gt 67108864) { throw \'Setup binary exceeds 64 MiB.\' }; $vestaOut.Write($vestaBuffer, 0, $vestaRead) }',
    '        } finally { $vestaOut.Dispose(); $vestaStream.Dispose(); $vestaResponse.Dispose(); $vestaRequest.Dispose() }',
    '    } finally { $vestaCts.Dispose(); $vestaClient.Dispose(); $vestaHandler.Dispose() }',
    `    if ((Get-FileHash -LiteralPath $vestaFile -Algorithm SHA256).Hash.ToLowerInvariant() -ne '${setupHash}') { throw 'Setup SHA-256 mismatch.' }`,
    `    & $vestaFile --bundle-url ${psQuote(bundleUrl)} --manifest-sha256 '${manifestHash}' --release-key '${keyHex}'`,
    "    if ($LASTEXITCODE -ne 0) { throw 'Setup failed.' }",
    '} finally {',
    '    $vestaResolved = [IO.Path]::GetFullPath($vestaFullWork)',
    "    if ($vestaResolved.StartsWith($vestaTempRoot + '\\', [StringComparison]::OrdinalIgnoreCase) -and [IO.Path]::GetFileName($vestaResolved) -match '^vesta-setup-bootstrap-[0-9a-f]{32}$' -and (Test-Path -LiteralPath $vestaResolved -PathType Container)) { $vestaItem = Get-Item -LiteralPath $vestaResolved; if (($vestaItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) { Remove-Item -LiteralPath $vestaResolved -Recurse -Force } }",
    '}',
  ].join('\n');
  await writeFile(path.join(resolvedOutput, 'commands.txt'), `Bash (interactive):\n${bash}\n\nPowerShell (interactive):\n${powershell}\n`, { flag: 'wx' });
  const release = { schema: 1, version: options.version, target: options.target, setupUrl, setupSha256: setupHash, bundleUrl, manifestSha256: manifestHash, releaseKey: keyHex };
  await writeFile(path.join(resolvedOutput, 'release.json'), JSON.stringify(release, null, 2) + '\n', { flag: 'wx' });
  console.log(`Packaged ${options.target} ${options.version} into ${resolvedOutput}`);
} catch (error) {
  const { rm } = await import('node:fs/promises');
  const safe = path.resolve(resolvedOutput);
  if (safe === resolvedOutput && path.dirname(safe) === realParent && path.basename(safe) === path.basename(output)) await rm(safe, { recursive: true, force: true });
  throw error;
}
