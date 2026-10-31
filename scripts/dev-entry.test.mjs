import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, mkdir, copyFile, writeFile, readFile, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

test('Windows dev entry forwards literal arguments and script failures', { skip: process.platform !== 'win32' }, async () => {
  const fixture = await mkdtemp(path.join(os.tmpdir(), 'vesta-dev-entry-'));
  try {
    await mkdir(path.join(fixture, 'scripts'));
    await copyFile(fileURLToPath(new URL('../dev.ps1', import.meta.url)), path.join(fixture, 'dev.ps1'));
    const target = path.join(fixture, 'scripts/dev-shell.ps1');
    await writeFile(target, '$args | ConvertTo-Json -Compress');
    const args = ['--server-root', 'D:\\Vesta test\\servers', '--release', 'literal$(nothing)'];
    const run = (arguments_ = args) => spawnSync('powershell.exe', ['-NoLogo', '-NoProfile', '-File', path.join(fixture, 'dev.ps1'), ...arguments_], { encoding: 'utf8' });
    const result = run();
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(JSON.parse(result.stdout.trim()), ['update', ...args]);
    for (const arguments_ of [[], ['restart', 'another server'], ['build'], ['shell'], ['pair']]) {
      const result = run(arguments_);
      assert.equal(result.status, 0, result.stderr);
      const value = JSON.parse(result.stdout.trim());
      assert.deepEqual(Array.isArray(value) ? value : [value], arguments_.length ? arguments_ : ['update']);
    }
    assert.match(run(['--help']).stdout, /Build and update dev-server/);
    await writeFile(target, 'exit 7');
    assert.equal(run().status, 7);
    await writeFile(target, "throw 'owned fixture failure'");
    assert.notEqual(run().status, 0);
  } finally {
    assert.equal(path.dirname(path.resolve(fixture)), path.resolve(os.tmpdir()));
    assert.match(path.basename(fixture), /^vesta-dev-entry-/);
    await rm(fixture, { recursive: true, force: true });
  }
});

test('Windows bootstrap stages the helper, preserves arguments and exit status, and removes its temporary copy', { skip: process.platform !== 'win32' }, async () => {
  const fixture = await mkdtemp(path.join(os.tmpdir(), 'vesta-dev-bootstrap-'));
  const literal = value => `'${value.replaceAll("'", "''")}'`;
  try {
    await mkdir(path.join(fixture, 'scripts'));
    await copyFile(fileURLToPath(new URL('./dev-shell.ps1', import.meta.url)), path.join(fixture, 'scripts/dev-shell.ps1'));
    const source = path.join(fixture, 'helper.rs');
    const executable = path.join(fixture, 'fixture helper.exe');
    await writeFile(source, `fn main() {
      std::fs::write(std::env::var("VESTA_DEV_FIXTURE_PATH").unwrap(), std::env::current_exe().unwrap().to_string_lossy().as_bytes()).unwrap();
      println!("{:?}", std::env::args().skip(1).collect::<Vec<_>>());
      let code = std::env::var("VESTA_DEV_FIXTURE_EXIT").unwrap().parse().unwrap();
      if code != 0 { eprintln!("vesta-dev: owned fixture failure"); }
      std::process::exit(code);
    }`);
    const compiled = spawnSync('rustc', ['--edition=2024', source, '-o', executable], { encoding: 'utf8' });
    assert.equal(compiled.status, 0, compiled.stderr);
    const record = path.join(fixture, 'path.txt');
    const command = path.join(fixture, 'run.ps1');
    const registryPath = String.raw`C:\literal path\servers`;
    await writeFile(command, `
      function cargo {
        if ($env:VESTA_DEV_FIXTURE_BUILD_FAIL) { $global:LASTEXITCODE=17; return }
        if ($args -notcontains 'vesta-agent' -or $args -notcontains 'vesta-setup' -or $args -notcontains 'vesta-dev') { throw 'inconsistent Cargo package set' }
        @{reason='compiler-artifact'; target=@{name='vesta-dev'}; executable=${literal(executable)}} | ConvertTo-Json -Compress
        $global:LASTEXITCODE=0
      }
      & ${literal(path.join(fixture, 'scripts/dev-shell.ps1'))} update --server-root ${literal(registryPath)} 'literal$(nothing)'
      exit $LASTEXITCODE
    `);
    for (const code of [0, 7]) {
      const result = spawnSync('powershell.exe', ['-NoLogo', '-NoProfile', '-File', command], {
        encoding: 'utf8', env: { ...process.env, VESTA_DEV_FIXTURE_PATH: record, VESTA_DEV_FIXTURE_EXIT: String(code) },
      });
      assert.equal(result.status, code, result.stderr);
      assert.equal(result.stderr.trim(), code ? 'vesta-dev: owned fixture failure' : '');
      assert.deepEqual(JSON.parse(result.stdout.trim()), ['update', '--server-root', registryPath, 'literal$(nothing)']);
      const copiedPath = await readFile(record, 'utf8');
      assert.match(path.basename(path.dirname(copiedPath)), /^vesta-dev-shell-/);
      assert.notEqual(copiedPath, executable);
      await assert.rejects(readFile(copiedPath), { code: 'ENOENT' });
    }
    const failedBuild = spawnSync('powershell.exe', ['-NoLogo', '-NoProfile', '-File', command], {
      encoding: 'utf8', env: { ...process.env, VESTA_DEV_FIXTURE_BUILD_FAIL: '1' },
    });
    assert.equal(failedBuild.status, 17);
    assert.equal(failedBuild.stderr.trim(), 'vesta-dev: Could not build the development tool.');
  } finally {
    assert.equal(path.dirname(path.resolve(fixture)), path.resolve(os.tmpdir()));
    assert.match(path.basename(fixture), /^vesta-dev-bootstrap-/);
    await rm(fixture, { recursive: true, force: true });
  }
});
