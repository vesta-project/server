import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { targetFiles, verifyBinaries } from './verify-binaries.mjs';

function headerFor(target, fill = 1) {
  const header = Buffer.alloc(512, fill);
  if (target === 'windows-x86_64') {
    header.write('MZ', 0, 'ascii');
    header.writeUInt32LE(64, 0x3c);
    header.write('PE\0\0', 64, 'binary');
    header.writeUInt16LE(0x8664, 68);
    header.writeUInt16LE(0x20b, 88);
  } else {
    header.set([0x7f, 0x45, 0x4c, 0x46, 2, 1], 0);
    header.writeUInt16LE(target === 'linux-aarch64' ? 183 : 62, 18);
  }
  return header;
}

async function fixture(t, target) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'vesta-binary-header-test-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  for (const [index, name] of targetFiles[target].entries()) {
    await writeFile(path.join(directory, name), headerFor(target, index + 1));
  }
  return directory;
}

test('verifies native PE and ELF headers for every published target', async t => {
  for (const target of Object.keys(targetFiles)) {
    const directory = await fixture(t, target);
    assert.deepEqual(await verifyBinaries(directory, target), targetFiles[target]);
  }
});

test('rejects a binary whose executable architecture does not match its target', async t => {
  const directory = await fixture(t, 'linux-aarch64');
  await writeFile(path.join(directory, targetFiles['linux-aarch64'][0]), headerFor('linux-x86_64'));
  await assert.rejects(verifyBinaries(directory, 'linux-aarch64'), /format or architecture mismatch/);
});

test('rejects a truncated PE optional header without reading outside the buffer', async t => {
  const directory = await fixture(t, 'windows-x86_64');
  const truncated = headerFor('windows-x86_64').subarray(0, 90);
  truncated.writeUInt32LE(84, 0x3c);
  truncated.write('PE\0\0', 84, 'binary');
  await writeFile(path.join(directory, 'vesta-setup.exe'), truncated);
  await assert.rejects(verifyBinaries(directory, 'windows-x86_64'), /format or architecture mismatch/);
});

test('rejects missing, empty, and unsupported target inputs', async t => {
  const directory = await fixture(t, 'linux-x86_64');
  await rm(path.join(directory, targetFiles['linux-x86_64'][0]));
  await assert.rejects(verifyBinaries(directory, 'linux-x86_64'), /Missing required binary/);
  await assert.rejects(verifyBinaries(directory, 'freebsd-x86_64'), /Unsupported target/);
  const emptyDirectory = await mkdtemp(path.join(os.tmpdir(), 'vesta-empty-binary-test-'));
  t.after(() => rm(emptyDirectory, { recursive: true, force: true }));
  for (const name of targetFiles['linux-x86_64']) await writeFile(path.join(emptyDirectory, name), '');
  await assert.rejects(verifyBinaries(emptyDirectory, 'linux-x86_64'), /non-empty regular file/);
});
