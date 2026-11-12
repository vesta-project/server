import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import {spawnSync} from 'node:child_process';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const ci = await readFile(path.join(root, '.github/workflows/ci.yml'), 'utf8');
const nightly = await readFile(path.join(root, '.github/workflows/nightly.yml'), 'utf8');

test('nightly binaries require the provider integration key without exposing it to PR validation', () => {
  const build = nightly.split('\n  build:')[1].split('\n  sign:')[0];
  assert.match(build, /CURSEFORGE_API_KEY: \$\{\{ secrets\.CURSEFORGE_API_KEY \}\}/);
  assert.match(build, /VESTA_REQUIRE_PROVIDER_KEYS: '1'/);
  assert.doesNotMatch(ci, /secrets\.CURSEFORGE_API_KEY/);
});

test('CI checks main pushes, pull requests, and manual validation without duplicate feature pushes', () => {
  assert.match(ci, /on:\s*\n\s+push:\s*\n\s+branches:\s*\[main\]/);
  assert.match(ci, /pull_request:\s*\n\s+branches:\s*\['\*\*'\]/);
  assert.match(ci, /workflow_dispatch:/);
  assert.match(ci, /permissions:\s*\n\s+contents:\s*read/);
});

test('nightly publishes only new main pushes, never schedules, manual dispatch or feature branches', () => {
  const triggers=nightly.slice(nightly.indexOf('on:'),nightly.indexOf('permissions:'));
  assert.match(triggers,/push:\s*\n\s+branches:\s*\[main\]/);
  assert.doesNotMatch(triggers,/schedule:|workflow_dispatch:|pull_request:|paths:|feat\//);
  assert.match(nightly,/test "\$REPOSITORY" = 'vesta-project\/server'/);
  assert.match(nightly,/test "\$EVENT" = 'push'/);
  assert.match(nightly,/test "\$REF" = 'refs\/heads\/main'/);
});

test('Rust jobs check out the shared dependency beside the server at an immutable revision', () => {
  for(const workflow of [ci,nightly]){
    assert.match(workflow,/repository: vesta-project\/launcher\n\s+ref: [0-9a-f]{40}\n\s+path: launcher/);
    assert.match(workflow,/working-directory: server/);
    assert.match(workflow,/path: server\n\s+persist-credentials: false/);
  }
});

test('signing waits for validation and native builds, requires the pinned key, and publishes only after re-verification', () => {
  assert.match(nightly, /sign:\s*\n(?:.*\n){0,8}\s+needs:\s*\[build, prepare\]/);
  assert.match(nightly, /Required environment secret VESTA_RELEASE_SIGNING_KEY is missing/);
  assert.match(nightly, /Required environment variable VESTA_RELEASE_PUBLIC_KEY is missing/);
  assert.match(nightly, /Generated public key does not match VESTA_RELEASE_PUBLIC_KEY/);
  assert.match(nightly, /stage-nightly-release\.mjs/);
  assert.match(nightly, /persist-credentials:\s*false/);
  assert.match(nightly, /Refuse an existing release tag or release/);
  assert.match(nightly, /Create the immutable GitHub Release with flat target-prefixed assets/);
  assert.match(nightly, /group: nightly-release-publish/);
  assert.match(nightly, /should_build: \$\{\{ steps.version.outputs.should_build \}\}/);
  assert.match(nightly, /--draft --prerelease --latest=false/);
  assert.match(nightly, /gh release edit "\$RELEASE_VERSION" --draft=false/);
  assert.match(nightly, /--asset-prefix "\$\{RELEASE_TARGET\}-"/);
  assert.match(nightly, /"\$RUNNER_TEMP\/release-assets\/"\*/);
  assert.doesNotMatch(nightly, /nightly-releases|git push origin/);
  assert.match(nightly, /contents:\s*write/);
  assert.match(nightly, /needs:\s*\[validate, prepare\]/);
  assert.match(nightly, /needs:\s*\[sign, prepare\]/);
});

test('all third-party workflow actions are pinned to full commit SHAs', () => {
  for (const [filename, workflow] of [['ci.yml', ci], ['nightly.yml', nightly]]) {
    const refs = [...workflow.matchAll(/^\s+uses:\s+([^\s#]+)(?:\s+#.*)?$/gm)].map(match => match[1]);
    assert.ok(refs.length > 0, `${filename} contains actions`);
    for (const ref of refs) assert.match(ref, /^[^@]+@[0-9a-f]{40}$/, `${filename} has immutable action reference: ${ref}`);
  }
});

test('independent products share verification and gate every publication on its own scope', () => {
  assert.match(nightly, /gh api --paginate --slurp "repos\/\$GH_REPO\/releases\?per_page=100"/);
  assert.doesNotMatch(nightly, /scope.env/);
  assert.match(nightly, /node scripts\/change-scope.mjs --release-plan/);
  const validate = nightly.split('\n  validate:')[1].split('\n  build:')[0];
  assert.match(validate, /if: needs.prepare.outputs.should_build == 'true'/);
  assert.match(validate, /--cargo-args "\$SCOPE"/);
  const publish = nightly.split('\n  publish:')[1];
  assert.ok(publish.indexOf('stage-nightly-release.mjs --product installer') < publish.indexOf('gh release create'));
  for (const [name, product] of [
    ['Refuse an existing release tag or release', 'server'],
    ['Create the immutable GitHub Release with flat target-prefixed assets', 'server'],
    ['Publish independently verified installer release', 'installer'],
  ]) {
    const step = publish.split(`      - name: ${name}\n`)[1].split('\n      - name:')[0];
    assert.match(step, new RegExp(`if: needs.prepare.outputs.${product}_release == 'true'`));
  }
  assert.match(nightly, /Installer signing key does not match configured public pin/);
  assert.match(nightly, /check-bootstrap-key.mjs "\$RELEASE_PUBLIC_KEY"/);
  assert.match(nightly, /if: always\(\)[\s\S]*rm -f -- "\$KEY_PATH"/);
});

// Execute the actual gate, with no signing/provider secrets, network or filesystem effects.
test('release gate accepts only a changed, non-deleted main push in the canonical repository',()=>{
  const gate=nightly.split('\n  gate:')[1].split('\n  prepare:')[0];
  const run=gate.split('        run: |\n')[1].trimEnd().split('\n').map(line=>line.replace(/^          /,'')).join('\n');
  const bash=process.platform==='win32'?path.join(process.env.ProgramFiles??'C:/Program Files','Git/bin/bash.exe'):'bash';
  const before='1'.repeat(40),after='2'.repeat(40);const baseline={REPOSITORY:'vesta-project/server',EVENT:'push',REF:'refs/heads/main',BEFORE:before,AFTER:after,COMMIT:after,DELETED:'false'};
  for(const[change,allowed]of [[{},true],[{REPOSITORY:'someone/fork'},false],[{EVENT:'schedule'},false],[{EVENT:'workflow_dispatch'},false],[{EVENT:'pull_request'},false],[{REF:'refs/heads/feat/launcher-compat'},false],[{REF:'refs/tags/v1.0'},false],[{DELETED:'true'},false],[{AFTER:before,COMMIT:before},false],[{AFTER:'0'.repeat(40),COMMIT:'0'.repeat(40)},false],[{COMMIT:before},false]]){
    const result=spawnSync(bash,['-c',run],{env:{PATH:process.env.PATH,SYSTEMROOT:process.env.SYSTEMROOT,...baseline,...change},encoding:'utf8',timeout:5000,windowsHide:true});
    assert.ifError(result.error);assert.equal(result.status===0,allowed,JSON.stringify(change));
  }
});
