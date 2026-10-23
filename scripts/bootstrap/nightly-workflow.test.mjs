import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const ci = await readFile(path.join(root, '.github/workflows/ci.yml'), 'utf8');
const nightly = await readFile(path.join(root, '.github/workflows/nightly.yml'), 'utf8');

test('CI checks pushes, pull requests, and manual validation with read-only contents access', () => {
  assert.match(ci, /on:\s*\n\s+push:\s*\n\s+branches:\s*\['\*\*'\]/);
  assert.match(ci, /pull_request:\s*\n\s+branches:\s*\['\*\*'\]/);
  assert.match(ci, /workflow_dispatch:/);
  assert.match(ci, /permissions:\s*\n\s+contents:\s*read/);
});

test('nightly publishing is limited to canonical internal sources and scheduled or approved pushes', () => {
  assert.match(nightly, /cron:\s*'17 3 \* \* \*'/);
  assert.match(nightly, /branches:\s*\[main, feat\/launcher-compat\]/);
  assert.match(nightly, /test "\$REPOSITORY" = 'vesta-project\/server'/);
  assert.match(nightly, /Publishing is allowed only from main or feat\/launcher-compat/);
  assert.doesNotMatch(nightly, /^\s*pull_request:/m);
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
