import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { expectedLedger, extractRequirements } from './requirements-ledger.mjs';

test('every baseline requirement bullet has a unique, phased traceability entry', () => {
  const ledger = expectedLedger();
  assert.ok(ledger.requirements.length > 300);
  assert.equal(new Set(ledger.requirements.map((r) => r.id)).size, ledger.requirements.length);
  for (const entry of ledger.requirements) {
    assert.match(entry.phase, /^P[1-8]$/);
    assert.match(entry.textSha256, /^[a-f0-9]{64}$/);
    assert.match(entry.source, /SERVER_SOFTWARE_REQUIREMENTS\.md:\d+$/);
    assert.ok(Array.isArray(entry.tests));
    assert.ok(Array.isArray(entry.evidence));
    assert.notEqual(entry.status, 'verified', 'Focused initial-batch tests cannot certify a compound requirement');
  }
});

test('committed ledger matches source, including requirement amendments', () => {
  const actual = JSON.parse(readFileSync(new URL('../docs/REQUIREMENT_TRACEABILITY.json', import.meta.url), 'utf8'));
  assert.deepEqual(actual, expectedLedger());
});

test('partial evidence names real test sources and retains a limitation statement', () => {
  const partial = expectedLedger().requirements.filter((r) => r.status === 'partially_verified');
  assert.ok(partial.length > 0);
  for (const entry of partial) {
    assert.ok(entry.tests.length > 0, entry.id);
    assert.ok(entry.evidence.some((e) => /remain|absent|unverified|not implemented|incomplete/i.test(e)), entry.id);
    assert.ok(entry.evidence.includes('docs/archive/IMPLEMENTATION_PROGRESS.md'), entry.id);
    for (const source of entry.tests) {
      if (source.startsWith('launcher-server-mvp:')) {
        assert.match(source, /^launcher-server-mvp:crates\/piston-lib\/(tests|src\/game\/server_install\/tests)\/.+\.rs$/);
        const launcher = new URL('../../launcher-server-mvp/', import.meta.url);
        if (existsSync(launcher)) {
          assert.ok(existsSync(new URL(source.slice('launcher-server-mvp:'.length), launcher)), source);
        }
      } else if (source.startsWith('crates/vesta-server-install/')) {
        assert.match(source, /^crates\/vesta-server-install\/src\/tests\/[a-z_]+\.rs$/);
        assert.ok(existsSync(new URL('../' + source, import.meta.url)), source);
      } else if (/^crates\/vesta-(control|state|transport|game-runtime|supervisor)\//.test(source)) {
        assert.match(source, /^crates\/vesta-(control|state|transport|game-runtime|supervisor)\/(src|tests)\/.+\.rs$/);
        assert.ok(existsSync(new URL('../' + source, import.meta.url)), source);
      } else {
        assert.match(source, /^server-java:(core|wrapper)\/src\/test\/java\/.+Test\.java$/);
        const javaRepo = process.env.VESTA_JAVA_REPO
          ? pathToFileURL(resolve(process.env.VESTA_JAVA_REPO) + sep)
          : new URL('../../server-java/', import.meta.url);
        if (existsSync(javaRepo)) {
          assert.ok(existsSync(new URL(source.slice('server-java:'.length), javaRepo)), source);
        }
      }
    }
  }
});

test('extractor excludes research and joins wrapped requirement text', () => {
  const entries = extractRequirements('# Requirements\n## Locked decisions\n### Authorization\n- First requirement\n  continued here.\n- Second requirement.\n## Research influences\n- Not a requirement.\n');
  assert.equal(entries.length, 2);
  assert.equal(entries[0].text, 'First requirement continued here.');
  assert.equal(entries[0].source, 'docs/SERVER_SOFTWARE_REQUIREMENTS.md:4');
  assert.equal(entries[0].phase, 'P2');
});

test('a newly introduced section must receive an explicit phase mapping', () => {
  assert.throws(() => extractRequirements('## Locked decisions\n### Unknown feature\n- New requirement.\n'), /Unmapped requirement section/);
});
