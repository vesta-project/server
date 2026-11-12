import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const gui = /^apps\/vesta-setup\/src\/(wizard|ui|directory_picker|visual_states|installer)\.rs$/;
const documentation = p => /\.md$/.test(p) || p.startsWith('docs/');
const bootstrap = p => /^scripts\/bootstrap\/install\.(sh|ps1)$/.test(p);
const workflow = p => p.startsWith('.github/workflows/');
const packageTool = p => p.startsWith('scripts/bootstrap/') && !bootstrap(p) && !p.endsWith('.test.mjs');

export function classifyChanges(files) {
  let setup = false, update = false, full = false, server = false, installer = false;
  for (const p of files) {
    if (p === 'scripts/change-scope.mjs') { full = true; continue; }
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
  const rustScope = active.includes('full') ? 'full' : active.includes('setup-update') || (active.includes('setup') && active.includes('update'))
    ? 'setup-update' : active[0] ?? 'none';
  return {...plan,rust_scope:rustScope};
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv[2] === '--cargo-args') {
    process.stdout.write(cargoArgs(process.argv[3]) + '\n');
  } else if (process.argv[2] === '--release-plan') {
    const result = planFromHistory(JSON.parse(fs.readFileSync(process.argv[3],'utf8')), {commit:process.env.COMMIT,serverVersion:process.env.RELEASE_VERSION,installerVersion:process.env.INSTALLER_VERSION});
    const output = Object.entries(result).map(([key,value])=>`${key}=${value}\n`).join('');
    fs.appendFileSync(process.env.GITHUB_OUTPUT, output);
    process.stdout.write(output);
  } else {
  const files = changedFiles({ event: process.env.CI_EVENT, base: process.env.CI_BASE, before: process.env.CI_BEFORE, head: process.env.CI_HEAD });
  const result = files === null ? { rust_scope: 'full', server_release: true, installer_release: true } : classifyChanges(files);
  const output = Object.entries(result).map(([key, value]) => `${key}=${value}\n`).join('');
  if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT, output);
  process.stdout.write(output);
  }
}
