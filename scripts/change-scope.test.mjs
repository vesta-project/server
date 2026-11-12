import assert from 'node:assert/strict';
import test from 'node:test';
import { classifyChanges, changedFiles, planPublications } from './change-scope.mjs';

test('bootstrap and docs need no Rust build or binary publication', () => {
  assert.deepEqual(classifyChanges(['scripts/bootstrap/install.sh', 'scripts/bootstrap/install.ps1', 'README.md']), { rust_scope: 'none', server_release: false, installer_release: false });
});
test('installer and server publishing deduplicate independently and resume partial publication', () => {
  const scope = {rust_scope:'full',server_release:true,installer_release:true};
  const options = {commit:'a'.repeat(40),serverVersion:'nightly-new',installerVersion:'installer-new'};
  const server = {draft:false,tag_name:'nightly-old-date',target_commitish:options.commit};
  const installer = {draft:false,tag_name:'installer-old-date',target_commitish:options.commit};
  assert.equal(planPublications(scope,[],options).server_release,true);
  assert.equal(planPublications(scope,[server],options).server_release,false);
  assert.equal(planPublications(scope,[server],options).installer_release,true);
  assert.equal(planPublications(scope,[installer],options).server_release,true);
  assert.equal(planPublications(scope,[server,installer],options).should_build,false);
  assert.equal(planPublications(classifyChanges(['scripts/bootstrap/install.sh']),[],options).should_build,false);
  assert.equal(planPublications(classifyChanges(['apps/vesta-setup/src/wizard.rs']),[],options).server_release,false);
  assert.throws(()=>planPublications({...scope,server_release:'false'},[],options),/Invalid/);
});
test('GUI-only changes release installer independently and test setup on both platforms', () => {
  assert.deepEqual(classifyChanges(['apps/vesta-setup/src/wizard.rs', 'apps/vesta-setup/src/ui.rs']), { rust_scope: 'setup', server_release: false, installer_release: true });
  assert.equal(classifyChanges(['apps/vesta-setup/src/host.rs']).server_release, true);
});
test('shared, configuration, workflow and unknown changes retain full checks', () => {
  for (const p of ['Cargo.lock', 'Cargo.toml', 'rust-toolchain.toml', 'crates/vesta-control/src/lib.rs', 'protocol/schema.json', 'unknown.file', '.github/workflows/ci.yml']) assert.equal(classifyChanges([p]).rust_scope, 'full', p);
  assert.equal(classifyChanges(['.github/workflows/ci.yml']).server_release, false);
});
test('release metadata changes exercise both setup and shared verifier', () => {
  assert.equal(classifyChanges(['scripts/bootstrap/package-release.mjs']).rust_scope, 'setup-update');
  assert.equal(classifyChanges(['crates/vesta-update/src/release.rs']).rust_scope, 'full');
  assert.equal(classifyChanges(['apps/vesta-setup/src/wizard.rs', 'Cargo.lock']).rust_scope, 'full');
});
test('diff includes rename/delete paths; absent/manual/invalid baselines cannot skip tests', () => {
  const from = 'a'.repeat(40), head = 'b'.repeat(40);
  let args;
  const files = changedFiles({ event: 'pull_request', base: from, head }, a => { args = a; return Buffer.from('apps/vesta-setup/src/wizard.rs\0crates/vesta-control/src/old.rs\0'); });
  assert.ok(args.includes('--no-renames'));
  assert.deepEqual(args.slice(-3), [from, head, '--']);
  assert.equal(classifyChanges(files).rust_scope, 'full');
  for (const data of [{event:'workflow_dispatch'}, {event:'push',before:'0'.repeat(40),head}, {event:'pull_request',base:'--bad',head}]) assert.equal(changedFiles(data), null);
  assert.throws(() => changedFiles({event:'push',before:from,head}, () => { throw Error('git failure'); }), /git failure/);
});
