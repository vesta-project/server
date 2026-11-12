#!/usr/bin/env node
import { open, lstat, readdir } from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

export const targetFiles = Object.freeze({
  'windows-x86_64': ['vesta-setup.exe', 'vesta-agent.exe', 'vesta-supervisor.exe', 'vesta.exe'],
  'linux-x86_64': ['vesta-setup', 'vesta-agent', 'vesta-supervisor', 'vesta'],
  'linux-aarch64': ['vesta-setup', 'vesta-agent', 'vesta-supervisor', 'vesta'],
});

const expected = {
  'windows-x86_64': { format: 'PE32+', machine: 0x8664 },
  'linux-x86_64': { format: 'ELF64 little-endian', machine: 62 },
  'linux-aarch64': { format: 'ELF64 little-endian', machine: 183 },
};

async function readHeader(file) {
  const handle = await open(file, 'r');
  try {
    const header = Buffer.alloc(4096);
    const { bytesRead } = await handle.read(header, 0, header.length, 0);
    return header.subarray(0, bytesRead);
  } finally {
    await handle.close();
  }
}

function matchesTarget(header, target) {
  if (target === 'windows-x86_64') {
    if (header.length < 64 || header.toString('ascii', 0, 2) !== 'MZ') return false;
    const peOffset = header.readUInt32LE(0x3c);
    return peOffset <= header.length - 26 && header.toString('ascii', peOffset, peOffset + 4) === 'PE\0\0'
      && header.readUInt16LE(peOffset + 4) === expected[target].machine
      && header.readUInt16LE(peOffset + 24) === 0x20b;
  }
  return header.length >= 20
    && header.toString('hex', 0, 4) === '7f454c46'
    && header[4] === 2
    && header[5] === 1
    && header.readUInt16LE(18) === expected[target].machine;
}

export async function verifyBinaries(directory, target, installerOnly = false) {
  if (!expected[target]) throw new Error(`Unsupported target: ${target}`);
  const files = installerOnly ? targetFiles[target].slice(0, 1) : targetFiles[target];
  const available = await readdir(directory);
  for (const name of files) {
    if (!available.includes(name)) throw new Error(`Missing required binary: ${name}`);
    const file = path.join(directory, name);
    const info = await lstat(file);
    if (!info.isFile() || info.isSymbolicLink() || info.size === 0) {
      throw new Error(`Required binary must be a non-empty regular file: ${name}`);
    }
    const header = await readHeader(file);
    if (!matchesTarget(header, target)) throw new Error(`Binary format or architecture mismatch: ${name} is not ${expected[target].format}.`);
  }
  return files;
}

function usage() {
  throw new Error('Usage: node verify-binaries.mjs --binaries DIR --target windows-x86_64|linux-x86_64|linux-aarch64');
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const options = {};
  if (![4,5].includes(args.length) || args[0] !== '--binaries' || args[2] !== '--target' || (args.length === 5 && args[4] !== '--installer-only')) usage();
  options.binaries = args[1];
  options.target = args[3];
  await verifyBinaries(options.binaries, options.target, args.length === 5);
  console.log(`Verified ${options.target} executable headers in ${path.resolve(options.binaries)}`);
}
