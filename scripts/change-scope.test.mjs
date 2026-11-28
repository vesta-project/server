import assert from 'node:assert/strict';
import test from 'node:test';
import { classifyChanges, changedFiles, planPublications, mergeRustScopes } from './change-scope.mjs';
import * as scopeModule from './change-scope.mjs';

test('PR routing defaults portable tests to Linux and selects native adapters by OS', () => {
  const cases = [
    [['crates/vesta-protocol/tests/discovery.rs', 'README.md'], 'linux', 'vesta-protocol'],
    [['crates/vesta-protocol/src/framing.rs'], 'linux', 'vesta-protocol+'],
    [['apps/vesta-setup/src/linux-service.sh'], 'linux', 'vesta-setup+'],
    [['apps/vesta-setup/src/platform/windows_detach.rs'], 'windows', 'vesta-setup+'],
    [['apps/vesta-agent/src/host_local_windows.rs'], 'windows', 'vesta-agent+'],
    [['crates/vesta-control/src/shutdown_signal_windows.rs'], 'windows', 'vesta-control+'],
    [['crates/vesta-state/src/windows_security.rs'], 'windows', 'vesta-state+'],
    [['apps/vesta-setup/src/linux_acl.rs', 'apps/vesta-agent/src/host_local_windows.rs'], 'both', 'vesta-agent+,vesta-setup+'],
    [['crates/vesta-control/src/files/native.rs', 'crates/vesta-protocol/tests/discovery.rs'], 'both', 'vesta-control+,vesta-protocol'],
    [['apps/vesta-setup/tests/setup.rs'], 'both', 'vesta-setup'],
  ];
  for (const [files, platform, packages] of cases) {
    assert.deepEqual(scopeModule.prRustSelection(files), {pr_rust_platform: platform, pr_rust_packages: packages});
  }
});

test('shared, unknown, or build-contract edits fall back even in a portable PR', () => {
  for (const file of ['Cargo.lock', 'crates/vesta-protocol/Cargo.toml', 'protocol/v1/fixtures/request.json',
    'crates/vesta-control/build.rs', 'apps/vesta-setup/src/install-profile.toml',
    'scripts/change-scope.mjs', '.github/workflows/ci.yml', 'crates/unknown/src/lib.rs', 'unknown.file']) {
    assert.deepEqual(scopeModule.prRustSelection(['crates/vesta-protocol/src/framing.rs', file]),
      {pr_rust_platform: 'both', pr_rust_packages: ''}, file);
  }
});

test('editing test sharding logic or timings exercises the full two-platform PR gate', () => {
  for (const file of ['scripts/rust-test-shards.mjs', 'scripts/fixtures/ci-rust-test-timings.json']) {
    assert.deepEqual(classifyChanges([file]), {rust_scope: 'full', server_release: false, installer_release: false});
    assert.deepEqual(scopeModule.prRustSelection([file]), {pr_rust_platform: 'both', pr_rust_packages: ''});
  }
});

test('shards cover whole-workspace and slow affected suites on the selected platforms', () => {
  const plan = files => scopeModule.prRustShards(classifyChanges(files).rust_scope, scopeModule.prRustSelection(files));
  const indexes = count => Array.from({length: count}, (_, index) => index);
  const whole = plan(['.github/workflows/ci.yml']);
  assert.deepEqual(whole.filter(shard => shard.os === 'ubuntu-24.04').map(shard => shard.shard), indexes(8));
  assert.deepEqual(whole.filter(shard => shard.os === 'windows-2025').map(shard => shard.shard), indexes(12));
  assert.ok(plan(['crates/vesta-protocol/src/framing.rs']).every(shard => shard.os === 'ubuntu-24.04'));
  assert.equal(plan(['crates/vesta-protocol/src/framing.rs']).length, 8);
  assert.ok(plan(['crates/vesta-control/src/shutdown_signal_windows.rs']).every(shard => shard.os === 'windows-2025'));
  assert.equal(plan(['crates/vesta-control/src/shutdown_signal_windows.rs']).length, 12);
  assert.equal(plan(['crates/vesta-transport/tests/https.rs']).length, 20);
  assert.deepEqual(plan(['apps/vesta-setup/src/linux_acl.rs']), []);
  assert.deepEqual(plan(['crates/vesta-protocol/tests/discovery.rs']), []);
  assert.deepEqual(plan(['README.md']), []);
});

test('legacy cache restores require the known main baseline and matching Cargo inputs', () => {
  const base = '5739d92b50d5050758c404503d117726eabfebd0';
  const keys = scopeModule.legacyRustCacheKeys(base, ['.github/workflows/ci.yml']);
  assert.match(keys.legacy_linux_cache, /^trusted-ci-Linux-rust-1\.96\.0-/);
  assert.match(keys.legacy_windows_cache, /^trusted-ci-Windows-rust-1\.96\.0-/);
  assert.ok(Object.values(keys).every(key => key.endsWith(base)));
  assert.deepEqual(scopeModule.legacyRustCacheKeys('a'.repeat(40), []), {});
  for (const file of ['Cargo.lock', 'Cargo.toml', 'rust-toolchain.toml', '.cargo/config.toml', 'crates/vesta-protocol/Cargo.toml']) {
    assert.deepEqual(scopeModule.legacyRustCacheKeys(base, [file]), {}, file);
  }
});

test('source changes include downstream target-specific, development and build dependencies', () => {
  const pkg = (name, dir, dependencies = []) => ({name, manifest_path: `/workspace/${dir}/Cargo.toml`, dependencies});
  const dependency = (dir, kind, target = null) => ({path: `/workspace/${dir}`, kind, target});
  const workspace = { packages: [
    pkg('vesta-protocol', 'crates/protocol'),
    pkg('vesta-control', 'crates/control', [dependency('crates/protocol', 'dev', 'cfg(windows)')]),
    pkg('vesta-agent', 'apps/agent', [dependency('crates/control', 'build')]),
    pkg('vesta-state', 'crates/state'),
  ] };
  assert.equal(scopeModule.affectedCargoArgs('vesta-protocol', workspace), '-p vesta-protocol');
  assert.equal(scopeModule.affectedCargoArgs('vesta-protocol+,vesta-state', workspace),
    '-p vesta-agent -p vesta-control -p vesta-protocol -p vesta-state');
  assert.throws(() => scopeModule.affectedCargoArgs('unknown+', workspace), /Unknown Rust package/);
});

test('one CI matrix includes both current push and unpublished release scope',()=>{
  assert.equal(mergeRustScopes('none','full'),'full');
  assert.equal(mergeRustScopes('setup','update'),'setup-update');
  assert.equal(mergeRustScopes('none','setup'),'setup');
  assert.equal(mergeRustScopes('none','none'),'none');
  assert.equal(mergeRustScopes('setup','bogus'),'full');
});

test('publication catches skipped pushes from separate product baselines and complete history', () => {
  const a='a'.repeat(40), b='b'.repeat(40), head='c'.repeat(40);
  const server={draft:false,tag_name:'nightly-old',published_at:'2026-01-01T00:00:00Z'};
  const installer={draft:false,tag_name:'installer-old',published_at:'2026-01-02T00:00:00Z'};
  const git=args=> {
    if(args[0]==='rev-parse') return Buffer.from(args.at(-1).includes('nightly-')?a:b);
    if(args[0]==='merge-base') return Buffer.from('');
    if(args[0]==='diff') return Buffer.from(args.at(-3)===a?'crates/vesta-control/src/lib.rs\0README.md\0':'apps/vesta-setup/src/wizard.rs\0README.md\0');
    throw Error('unexpected command');
  };
  const options={commit:head,serverVersion:'nightly-new',installerVersion:'installer-new'};
  assert.equal(classifyChanges(['README.md']).server_release,false);
  assert.deepEqual(scopeModule.planFromHistory([[server],[installer]],options,git),{rust_scope:'full',server_release:true,installer_release:true,should_build:true});
  const page1=Array.from({length:100},(_,i)=>({draft:false,tag_name:`other-${i}`}));
  const sameCommitGit=args=>args[0]==='rev-parse'?Buffer.from(head):git(args);
  assert.equal(scopeModule.planFromHistory([page1,[installer]],options,sameCommitGit).installer_release,false);
  assert.equal(scopeModule.planFromHistory([[server]],options,git).installer_release,true,'first installer still publishes');
  const installerOnlyGit=args=> {
    if(args[0]==='rev-parse') return Buffer.from(args.at(-1).includes('nightly-')?head:b);
    if(args[0]==='diff') return Buffer.from(args.at(-3)===head?'':'apps/vesta-setup/src/wizard.rs\0');
    return git(args);
  };
  assert.deepEqual(scopeModule.planFromHistory([[server,installer]],options,installerOnlyGit),{rust_scope:'setup',server_release:false,installer_release:true,should_build:true});
  const unavailable=()=>{throw Error('missing or non-ancestor baseline');};
  assert.equal(scopeModule.planFromHistory([[server,installer]],options,unavailable).rust_scope,'full');
  assert.throws(()=>scopeModule.planFromHistory([[server,installer]],options,args=>{
    if(args[0]==='diff') throw Error('diff failed');
    return git(args);
  }),/diff failed/);
  assert.throws(()=>scopeModule.planFromHistory({},options,git),/history/);
});

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
