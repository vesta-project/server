import assert from 'node:assert/strict';
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

const ci = await readFile(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8');
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
  assert.match(ci, /needs: test/);
  assert.match(ci, /rust_scope: \$\{\{ steps.scope.outputs.rust_scope \}\}/);
  assert.match(ci, /fetch-depth: 0/);
  for (const name of ['Checkout pinned shared sandbox source', 'Select verified development toolchain', 'Check formatting', 'Check all targets', 'Run Rust tests', 'Run strict all-target Clippy']) {
    const step = ci.split(`      - name: ${name}\n`)[1].split('\n      - name:')[0];
    assert.match(step, /if: needs.test.outputs.rust_scope != 'none'/, name);
  }
  assert.match(ci, /name: Rust checks not applicable to this revision/);
  assert.doesNotMatch(ci, /secrets\.CURSEFORGE_API_KEY/);
});
test('cache writes require successful protected main, never PR writes, cache errors cannot skip tests', () => {
  const save = ci.split('      - name: Save cache only from successful protected main')[1];
  assert.match(save, /if: success\(\).*github.event_name == 'push'.*github.ref == 'refs\/heads\/main'/);
  assert.match(save, /continue-on-error: true/);
  assert.match(ci, /uses: actions\/cache\/restore@[0-9a-f]{40}/);
  assert.match(ci, /uses: actions\/cache\/save@[0-9a-f]{40}/);
  assert.match(ci, /key: trusted-ci-\$\{\{ runner.os \}\}-rust-1.96.0-/);
});
test('actual scope shell selects bounded package arguments, unknown values fall back to workspace', async t => {
  const temp = await mkdtemp(path.join(os.tmpdir(), 'vesta-ci-scope-'));
  t.after(() => rm(temp, {recursive:true,force:true}));
  const step = ci.split('      - name: Select Rust scope\n')[1].split('\n      - name:')[0];
  const run = step.split('        run: |\n')[1].trimEnd().split('\n').map(l => l.replace(/^          /, '')).join('\n');
  const bash = process.platform === 'win32' ? path.join(process.env.ProgramFiles ?? 'C:/Program Files', 'Git/bin/bash.exe') : 'bash';
  for (const [scope, expected] of [['setup','-p vesta-setup'],['update','-p vesta-update'],['setup-update','-p vesta-setup -p vesta-update'],['full','--workspace'],['','--workspace'],['$(exit 99)','--workspace']]) {
    const file = path.join(temp, `output-${expected.length}-${scope.length}.txt`).replaceAll('\\','/');
    await rm(file, {force:true});
    const result = spawnSync(bash, ['-e','-c',run], {encoding:'utf8',env:{...process.env,SCOPE:scope,GITHUB_OUTPUT:file},timeout:5000});
    assert.equal(result.status, 0, result.stderr);
    assert.equal((await readFile(file,'utf8')).trim(), `args=${expected}`);
  }
});
