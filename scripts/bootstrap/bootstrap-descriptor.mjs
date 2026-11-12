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
