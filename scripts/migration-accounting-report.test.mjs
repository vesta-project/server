import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildReport } from './migration-accounting-report.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));

const row = (overrides = {}) => ({
  kind: 'production',
  boundary: 'host',
  status: 'not_migrated',
  rustCounterpart: null,
  applicableRequirements: [],
  ...overrides,
});

test('counts requirement references, unknown IDs, prefixes, and inventory groups', () => {
  const manifest = {
    baselineRequirements: [
      { id: 'alpha-001', text: 'Alpha one' },
      { id: 'alpha-002', text: 'Alpha two' },
      { id: 'beta-014', text: 'Beta fourteen' },
    ],
    inventory: [
      row({ applicableRequirements: ['alpha-001', { id: 'invented-999' }] }),
      row({
        kind: 'test', boundary: 'plugin_boundary', status: 'verified',
        rustCounterpart: 'rust/src/test.rs',
        applicableRequirements: [{ id: 'alpha-001' }, 'beta-014'],
      }),
      row({ kind: 'test_helper', boundary: 'shared_host_plugin_boundary', status: 'migrated' }),
    ],
  };

  assert.deepEqual(buildReport(manifest), {
    requirements: {
      total: 3,
      referenced: 2,
      unreferenced: 1,
      unreferencedByPrefix: { alpha: 1 },
    },
    inventory: {
      total: 3,
      byStatus: { not_migrated: 1, verified: 1, migrated: 1 },
      byKindBoundaryStatus: {
        'production|host|not_migrated': 1,
        'test|plugin_boundary|verified': 1,
        'test_helper|shared_host_plugin_boundary|migrated': 1,
      },
    },
    rowsWithRustCounterpart: 1,
    rowsWithoutRustCounterpart: 2,
    unknownReferences: 1,
  });
});

test('reports the real manifest totals without pinning mutable counts', () => {
  const manifest = JSON.parse(readFileSync(resolve(root, 'docs/RUST_WRAPPER_PARITY.json'), 'utf8'));
  const report = buildReport(manifest);
  assert.equal(report.requirements.total, manifest.baselineRequirements.length);
  assert.equal(report.inventory.total, 222);
});
