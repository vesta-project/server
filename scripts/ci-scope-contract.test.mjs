import assert from 'node:assert/strict';
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

const ci = await readFile(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8');
function workflowStep(name) {
  const start = ci.indexOf(`name: ${name}\n`);
  assert.notEqual(start, -1, name);
  return ci.slice(start).split(/\n      - |\n  [\w-]+:/)[0];
}
test('main uses the same CI once and cannot omit pending release validation',async()=>{
  const nightly=await readFile(new URL('../.github/workflows/nightly.yml',import.meta.url),'utf8');
  assert.match(ci,/workflow_call:[\s\S]*release_rust_scope:/);
  assert.match(ci,/CI_RELEASE_SCOPE: \$\{\{ inputs.release_rust_scope \}\}/);
  assert.equal((nightly.match(/uses: .\/\.github\/workflows\/ci.yml/g)||[]).length,1);
  assert.doesNotMatch(nightly,/cargo test|cargo clippy|cargo check/);
  assert.match(nightly,/needs: \[validate, prepare\]/);
  assert.doesNotMatch(ci.slice(0,ci.indexOf('permissions:')),/\bpush:/);
});
test('required job names remain; no Rust change is explicit, classifier failure cannot skip checks', () => {
  assert.match(ci, /name: Rust \(\$\{\{ matrix.os \}\}\)/);
  assert.match(ci, /needs: scope/);
  assert.doesNotMatch(ci, /needs: test/);
  assert.match(ci, /needs: scope\n    if: \$\{\{ !cancelled\(\) \}\}/);
  assert.match(ci, /name: Require successful scope calculation\n\s+if: needs\.scope\.result != 'success'\n\s+working-directory: \.\n\s+run: exit 1/);
  assert.match(ci, /rust_scope: \$\{\{ steps.scope.outputs.rust_scope \}\}/);
  assert.match(ci, /fetch-depth: 0/);
  for (const name of ['Checkout pinned shared sandbox source', 'Select verified development toolchain', 'Select Rust scope', 'Check formatting', 'Check all targets', 'Run strict all-target Clippy']) {
    const step = workflowStep(name);
    assert.match(step, /needs\.scope\.outputs\.rust_scope != 'none'/, name);
  }
  assert.match(ci, /name: Rust checks not applicable to this revision/);
  assert.doesNotMatch(ci, /secrets\.CURSEFORGE_API_KEY/);
});
test('platform routing affects PR test execution only; both runners keep strict all-target Clippy', () => {
  const tests = workflowStep('Run Rust tests');
  assert.match(tests, /if: steps.packages.outputs.test_mode == 'serial'/);
  assert.match(tests, /--test-threads=1/);
  const lint = workflowStep('Run strict all-target Clippy');
  assert.doesNotMatch(lint, /pr_rust_platform/);
  assert.match(lint, /--all-targets --locked -- -D warnings/);
  assert.match(lint, /needs\.scope\.outputs\.pr_rust_shard != 'true'/);
  const check = workflowStep('Check all targets');
  assert.match(check, /github\.event_name != 'pull_request'/);
});
test('full-scope PRs build test executables once per OS, run doctests, and require every shard', () => {
  assert.match(ci, /pr_rust_shard: \$\{\{ steps.scope.outputs.pr_rust_shard \}\}/);
  assert.match(ci, /cargo test \$\{\{ steps.packages.outputs.args \}\} --locked --no-run --message-format=json/);
  assert.match(ci, /cargo test \$\{\{ steps.packages.outputs.args \}\} --locked --doc -- --test-threads=1/);
  assert.match(ci, /rust-shards:\n\s+needs: \[scope, rust\]\n\s+if: needs.scope.outputs.pr_rust_shard == 'true'/);
  assert.match(ci, /matrix: \$\{\{ fromJSON\(needs.scope.outputs.pr_rust_shard_matrix\) \}\}/);
  assert.match(ci, /rust-pr-checks:\n\s+needs: scope\n\s+if: needs.scope.outputs.pr_rust_shard == 'true'/);
  assert.match(workflowStep('Run parallel strict all-target Clippy'), /--all-targets --locked -- -D warnings/);
  assert.match(ci, /rust-required:\n\s+needs: \[scope, rust, rust-pr-checks, rust-shards\]\n\s+if: \$\{\{ !cancelled\(\) \}\}/);
  assert.match(ci, /if \[\[ \$SHARDED == true \]\]; then\s+test "\$CHECK_RESULT" = success\s+test "\$SHARD_RESULT" = success\s+else\s+test "\$CHECK_RESULT" = skipped\s+test "\$SHARD_RESULT" = skipped/);
});
test('cache writes require successful protected main, never PR writes, cache errors cannot skip tests', () => {
  const save = ci.split('      - name: Save cache only from successful protected main')[1];
  assert.match(save, /if: success\(\).*github.event_name == 'push'.*github.ref == 'refs\/heads\/main'/);
  assert.match(save, /&& github\.ref_protected/);
  assert.match(save, /continue-on-error: true/);
  assert.match(ci, /uses: actions\/cache\/restore@[0-9a-f]{40}/);
  assert.match(ci, /uses: actions\/cache\/save@[0-9a-f]{40}/);
  assert.match(ci, /PLATFORM: \$\{\{ matrix.os \}\}-\$\{\{ runner.arch \}\}/);
  assert.match(ci, /prefix=trusted-ci-v2-\$PLATFORM-rust-1\.96\.0-debug-.*\$CI_LAUNCHER_REVISION-\$CARGO_INPUTS-/);
  assert.equal((ci.match(/key: \$\{\{ steps.cache-profile.outputs.prefix \}\}\$\{\{ github.sha \}\}/g) || []).length, 2);
  assert.match(ci, /ref: \$\{\{ env.CI_LAUNCHER_REVISION \}\}/);
  const profile = workflowStep('Identify Rust cache profile');
  assert.match(profile, /CI_LAUNCHER_REVISION == [0-9a-f]{40}/);
  assert.match(profile, /ubuntu-24\.04-X64\) echo "legacy_key=\$LEGACY_LINUX"/);
  assert.match(profile, /windows-2025-X64\) echo "legacy_key=\$LEGACY_WINDOWS"/);
  assert.match(workflowStep('Restore trusted main Rust cache'), /restore-keys: \|\s+\$\{\{ steps.cache-profile.outputs.prefix \}\}\s+\$\{\{ steps.cache-profile.outputs.legacy_key \}\}/);
});
test('actual scope shell selects bounded package arguments, unknown values fall back to workspace', async t => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'vesta-ci-scope-'));
  t.after(() => rm(temp, {recursive:true,force:true}));
  const step = workflowStep('Select Rust scope');
  const run = step.split('        run: |\n')[1].trimEnd().split('\n').map(l => l.replace(/^          /, '')).join('\n');
  const bash = process.platform === 'win32' ? path.join(process.env.ProgramFiles ?? 'C:/Program Files', 'Git/bin/bash.exe') : 'bash';
  const cases = [
    ['setup', '-p vesta-setup', 'Linux', '', '', 'serial'],
    ['update', '-p vesta-update', 'Windows', 'linux', '0', 'skip'],
    ['setup-update', '-p vesta-setup -p vesta-update', 'Linux', 'both', '8', 'sharded'],
    ['full', '--workspace', 'Windows', 'windows', '12', 'sharded'],
    ['', '--workspace', 'Linux', 'windows', '0', 'skip'],
    ['$(exit 99)', '--workspace', 'Windows', '', '', 'serial'],
  ];
  for (const [scope, expected, runner, platform, shards, mode] of cases) {
    const file = path.join(temp, `output-${expected.length}-${scope.length}.txt`).replaceAll('\\','/');
    await rm(file, {force:true});
    const result = spawnSync(bash, ['-e','-c',run], {encoding:'utf8',env:{
      ...process.env, SCOPE:scope, PR_PACKAGES:'', GITHUB_OUTPUT:file, RUNNER_OS:runner, TEST_PLATFORM:platform, SHARD_COUNT:shards,
    },timeout:5000});
    assert.equal(result.status, 0, result.stderr);
    assert.equal((await readFile(file,'utf8')).trim(), `args=${expected}\ntest_mode=${mode}`);
  }
});
