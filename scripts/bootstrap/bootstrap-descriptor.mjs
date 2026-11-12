// A strict line protocol lets the shell authenticate setup without a JSON runtime.
export function bootstrapDescriptor({ version, target, setupSha256, setupSize, manifestSha256 }) {
  if (!/^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$/.test(version ?? '') ||
      !['linux-x86_64', 'linux-aarch64', 'windows-x86_64'].includes(target) ||
      !/^[0-9a-f]{64}$/.test(setupSha256 ?? '') || !/^[0-9a-f]{64}$/.test(manifestSha256 ?? '') ||
      !Number.isSafeInteger(setupSize) || setupSize < 1 || setupSize > 67108864) {
    throw new Error('Invalid bootstrap descriptor identity.');
  }
  const setupName = target.startsWith('windows-') ? 'vesta-setup.exe' : 'vesta-setup';
  return Buffer.from(['vesta-setup-bootstrap-v1', version, target, setupName, setupSha256, String(setupSize), manifestSha256, ''].join('\n'));
}

export function installerDescriptor({ version, target, setupSha256, setupSize }) {
  if (!version?.startsWith('installer-')) throw new Error('Installer release identity is required.');
  // Reuse all identity/size validation and canonical encoding; the purpose and
  // last field distinguish independent installer trust from package bootstrap.
  const lines = bootstrapDescriptor({ version, target, setupSha256, setupSize, manifestSha256:'0'.repeat(64) }).toString().split('\n');
  lines[0] = 'vesta-installer-bootstrap-v1';
  lines[6] = '1'; // supported server manifest schema, not an unsigned package pin
  return Buffer.from(lines.join('\n'));
}
