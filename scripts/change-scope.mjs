import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const gui = /^apps\/vesta-setup\/src\/(wizard|ui|directory_picker|visual_states|installer)\.rs$/;
const documentation = p => /\.md$/.test(p) || p.startsWith('docs/');
const bootstrap = p => /^scripts\/bootstrap\/install\.(sh|ps1)$/.test(p);
const workflow = p => p.startsWith('.github/workflows/');
const packageTool = p => p.startsWith('scripts/bootstrap/') && !bootstrap(p) && !p.endsWith('.test.mjs');
const workspaceRustFile = /^(?:crates\/(modpack-engine|vesta-(?:protocol|state|control|transport|host-client|game-runtime|server-install|supervisor|update))|apps\/(vesta-(?:agent|setup))|tools\/(vesta-dev))\/(src|tests)\/.+\.rs$/;

export function classifyChanges(files) {
  let setup = false, update = false, full = false, server = false, installer = false;
  for (const p of files) {
    if (['scripts/change-scope.mjs', 'scripts/rust-test-shards.mjs', 'scripts/fixtures/ci-rust-test-timings.json'].includes(p)) {
      full = true;
      continue;
    }
    if (documentation(p) || bootstrap(p)) continue;
    if (workflow(p)) {
      full = true;
      if (!['.github/workflows/ci.yml', '.github/workflows/installer.yml'].includes(p)) server = installer = true;
      continue;
    }
    if (p.startsWith('apps/vesta-setup/')) {
      setup = true;
      if (!p.includes('/tests/')) {
        installer = true;
        if (!gui.test(p)) server = true;
      }
      continue;
    }
    if (p.startsWith('crates/vesta-update/tests/')) { update = true; continue; }
    if (p.startsWith('crates/vesta-update/')) { full = true; server = installer = true; continue; }
    if (packageTool(p)) { setup = update = true; server = installer = true; continue; }
    if (p.startsWith('scripts/')) continue;
    // Shared Rust/config/protocol and unknown paths fail conservatively to full.
    full = true; server = installer = true;
  }
  return { rust_scope: full ? 'full' : setup && update ? 'setup-update' : setup ? 'setup' : update ? 'update' : 'none', server_release: server, installer_release: installer };
}

export function changedFiles({ event, base, before, head }, git = args => execFileSync('git', args)) {
  if (!['push', 'pull_request'].includes(event)) return null;
  const from = event === 'pull_request' ? base : before;
  if (!/^[0-9a-f]{40}$/i.test(from ?? '') || /^0+$/.test(from) || !/^[0-9a-f]{40}$/i.test(head ?? '')) return null;
  return git(['diff', '--name-only', '--no-renames', '-z', from, head, '--']).toString('utf8').split('\0').filter(Boolean);
}

export function cargoArgs(scope) {
  return new Map([['setup','-p vesta-setup'],['update','-p vesta-update'],['setup-update','-p vesta-setup -p vesta-update']]).get(scope) ?? '--workspace';
}
export function mergeRustScopes(...scopes) {
  if (scopes.some(scope => !['none','setup','update','setup-update','full'].includes(scope)) || scopes.includes('full')) return 'full';
  if (scopes.includes('setup-update') || scopes.includes('setup') && scopes.includes('update')) return 'setup-update';
  return scopes.find(scope=>scope!=='none') ?? 'none';
}

// Only route code whose platform boundary has been checked. Unknown paths,
// manifests, shared fixtures and CI changes use the existing two-OS gate.
export function prRustSelection(files) {
  const platforms = new Set();
  const packages = new Set();
  for (const file of files) {
    if (documentation(file) || bootstrap(file)) continue;
    if (/^crates\/vesta-protocol\/(?:src|tests)\/.+\.rs$/.test(file)) {
      platforms.add('linux');
      packages.add(file.includes('/src/') ? 'vesta-protocol+' : 'vesta-protocol');
    } else if (/^apps\/vesta-setup\/src\/(?:linux_(?:acl|import|install)\.rs|linux-service\.sh)$/.test(file)) {
      platforms.add('linux');
      packages.add('vesta-setup+');
    } else if (file === 'apps/vesta-setup/src/platform/windows_detach.rs') {
      platforms.add('windows');
      packages.add('vesta-setup+');
    } else if (file === 'apps/vesta-agent/src/host_local_windows.rs') {
      platforms.add('windows');
      packages.add('vesta-agent+');
    } else if (file === 'crates/vesta-control/src/shutdown_signal_windows.rs') {
      platforms.add('windows');
      packages.add('vesta-control+');
    } else if (file === 'crates/vesta-state/src/windows_security.rs') {
      platforms.add('windows');
      packages.add('vesta-state+');
    } else if (workspaceRustFile.test(file)) {
      const [, crate, app, tool, area] = workspaceRustFile.exec(file);
      platforms.add('linux');
      platforms.add('windows');
      packages.add(`${crate ?? app ?? tool}${area === 'src' ? '+' : ''}`);
    } else {
      return { pr_rust_platform: 'both', pr_rust_packages: '' };
    }
  }
  return {
    pr_rust_platform: platforms.size === 1 ? [...platforms][0] : 'both',
    pr_rust_packages: [...packages].sort().join(','),
  };
}

export function prRustShards(scope, selection) {
  const roots = selection.pr_rust_packages.split(',');
  const slow = roots.some(root => root === 'vesta-control' || root === 'vesta-transport'
    || (root.endsWith('+') && !['vesta-agent+', 'vesta-setup+', 'vesta-dev+'].includes(root)));
  if (!(scope === 'full' && !selection.pr_rust_packages) && !slow) return [];
  return [['linux', 'ubuntu-24.04', 8], ['windows', 'windows-2025', 12]]
    .filter(([platform]) => selection.pr_rust_platform === 'both' || selection.pr_rust_platform === platform)
    .flatMap(([, os, count]) => Array.from({length: count}, (_, shard) => ({os, shard})));
}

// Seed the new key from the last successful protected-main build while its
// Cargo inputs still match. Future main builds save under the new key.
export function legacyRustCacheKeys(base, files) {
  const main = '5739d92b50d5050758c404503d117726eabfebd0';
  if (base !== main || files.some(file => /(?:^|\/)(?:Cargo\.(?:toml|lock)|rust-toolchain\.toml)$|(?:^|\/)\.cargo\//.test(file))) {
    return {};
  }
  return {
    legacy_linux_cache: `trusted-ci-Linux-rust-1.96.0-31d9655438a61b3dec0b0c9042df84de0fd2463a292e427c1c57ca3d417d3feb-${main}`,
    legacy_windows_cache: `trusted-ci-Windows-rust-1.96.0-0e546f20423a1388e0fadf6cd74c68a9bf0cb723e9fad0a2c9ffc24b40357254-${main}`,
  };
}

// A '+' root changes production code. Include its transitive consumers, even
// through build, dev or target-specific dependencies. Manifests and lockfiles
// select the full gate above, so the current graph is valid for either side.
export function affectedCargoArgs(selection, workspace) {
  const packages = new Map(workspace.packages.map(pkg => [pkg.name, pkg]));
  const byDirectory = new Map(workspace.packages.map(pkg => [path.dirname(pkg.manifest_path), pkg.name]));
  const affected = new Set();
  const pending = [];
  for (const root of selection.split(',')) {
    const name = root.replace(/\+$/, '');
    if (!packages.has(name)) throw new Error(`Unknown Rust package in PR scope: ${name}`);
    affected.add(name);
    if (root.endsWith('+')) pending.push(name);
  }
  const expanded = new Set();
  for (const name of pending) {
    if (expanded.has(name)) continue;
    expanded.add(name);
    for (const pkg of packages.values()) {
      if (pkg.dependencies.some(dep => dep.path && byDirectory.get(dep.path) === name)) {
        affected.add(pkg.name);
        pending.push(pkg.name);
      }
    }
  }
  return [...affected].sort().map(name => {
    if (!/^[a-z][a-z0-9-]*$/.test(name)) throw new Error('Invalid Cargo package name in PR scope');
    return `-p ${name}`;
  }).join(' ');
}
export function planPublications(scope, releases, {commit, serverVersion, installerVersion}, releaseCommit = r => r.target_commitish) {
  if (!Array.isArray(releases) || typeof scope.server_release !== 'boolean' || typeof scope.installer_release !== 'boolean') throw new Error('Invalid publication scope.');
  const exists = (prefix, tag) => releases.some(r => !r.draft && typeof r.tag_name === 'string' && r.tag_name.startsWith(prefix) && (r.tag_name === tag || releaseCommit(r) === commit));
  const server = scope.server_release && !exists('nightly-',serverVersion);
  const installer = scope.installer_release && !exists('installer-',installerVersion);
  return {rust_scope:scope.rust_scope,server_release:server,installer_release:installer,should_build:server||installer};
}

// gh --paginate --slurp supplies every page. Product baselines are independent:
// a dropped/failed intermediate push must not disappear behind a docs-only push.
export function planFromHistory(pages, options, git = args => execFileSync('git', args)) {
  if (!Array.isArray(pages) || pages.some(page => !Array.isArray(page))) throw new Error('Invalid complete release history.');
  if (!/^[0-9a-f]{40}$/i.test(options.commit ?? '')) throw new Error('Invalid release commit.');
  const releases = pages.flat().filter(r => r && !r.draft && typeof r.tag_name === 'string')
    .sort((a,b) => String(b.published_at ?? '').localeCompare(String(a.published_at ?? '')));
  const resolved = new Map();
  const commitOf = release => {
    if (!resolved.has(release.tag_name)) {
      let value = null;
      if (/^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$/.test(release.tag_name)) {
        try {
          const sha = git(['rev-parse','--verify',`refs/tags/${release.tag_name}^{commit}`]).toString().trim();
          if (/^[0-9a-f]{40}$/i.test(sha)) value = sha;
        } catch { /* Missing tag/history fails conservatively to a full build. */ }
      }
      resolved.set(release.tag_name,value);
    }
    return resolved.get(release.tag_name);
  };
  const full = {rust_scope:'full',server_release:true,installer_release:true};
  const scopes = ['nightly-','installer-'].map(prefix => {
    const previous = releases.find(r => r.tag_name.startsWith(prefix));
    const base = previous && commitOf(previous);
    if (!base) return full;
    try { git(['merge-base','--is-ancestor',base,options.commit]); }
    catch { return full; }
    // Once ancestry is established a diff failure is an error, never a skip.
    return classifyChanges(changedFiles({event:'push',before:base,head:options.commit},git));
  });
  const plan = planPublications({rust_scope:'none',server_release:scopes[0].server_release,installer_release:scopes[1].installer_release},releases,options,commitOf);
  const active = [plan.server_release && scopes[0].rust_scope, plan.installer_release && scopes[1].rust_scope].filter(Boolean);
  const rustScope = mergeRustScopes(...active);
  return {...plan,rust_scope:rustScope};
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv[2] === '--cargo-args') {
    const selection = process.argv[4];
    const args = selection
      ? affectedCargoArgs(selection, JSON.parse(execFileSync('cargo', ['metadata', '--no-deps', '--format-version', '1', '--locked'])))
      : cargoArgs(process.argv[3]);
    process.stdout.write(args + '\n');
  } else if (process.argv[2] === '--release-plan') {
    const result = planFromHistory(JSON.parse(fs.readFileSync(process.argv[3],'utf8')), {commit:process.env.COMMIT,serverVersion:process.env.RELEASE_VERSION,installerVersion:process.env.INSTALLER_VERSION});
    const output = Object.entries(result).map(([key,value])=>`${key}=${value}\n`).join('');
    fs.appendFileSync(process.env.GITHUB_OUTPUT, output);
    process.stdout.write(output);
  } else {
  const files = changedFiles({ event: process.env.CI_EVENT, base: process.env.CI_BASE, before: process.env.CI_BEFORE, head: process.env.CI_HEAD });
  const result = files === null ? { rust_scope: 'full', server_release: true, installer_release: true } : classifyChanges(files);
  result.rust_scope=mergeRustScopes(result.rust_scope,process.env.CI_RELEASE_SCOPE || 'none');
  if (process.env.CI_EVENT === 'pull_request' && files !== null && (process.env.CI_RELEASE_SCOPE || 'none') === 'none') {
    const selection = prRustSelection(files);
    const shards = prRustShards(result.rust_scope, selection);
    Object.assign(result, selection, legacyRustCacheKeys(process.env.CI_BASE, files), {
      pr_rust_shard: shards.length > 0,
      pr_rust_shard_matrix: JSON.stringify({include: shards}),
      pr_rust_linux_shard_count: shards.filter(shard => shard.os === 'ubuntu-24.04').length,
      pr_rust_windows_shard_count: shards.filter(shard => shard.os === 'windows-2025').length,
    });
  }
  const output = Object.entries(result).map(([key, value]) => `${key}=${value}\n`).join('');
  if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT, output);
  process.stdout.write(output);
  }
}
