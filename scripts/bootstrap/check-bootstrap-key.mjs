import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export function checkBootstrapKey(source, expected) {
  const pins = [...source.matchAll(/^  release_key=([0-9a-f]{64})$/gm)];
  if (!/^[0-9a-f]{64}$/i.test(expected ?? '') || pins.length !== 1 || pins[0][1] !== expected.toLowerCase()) {
    throw new Error('Installer public key does not match the configured release pin; update the trusted bootstrap before signing or publishing.');
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    checkBootstrapKey(await readFile(new URL('./install.sh', import.meta.url), 'utf8'), process.argv[2]);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
