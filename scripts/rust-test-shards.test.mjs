import assert from 'node:assert/strict';
import path from 'node:path';
import test from 'node:test';
import { cargoExecutables, parseLibtestList, partitionSuites, testArguments } from './rust-test-shards.mjs';

test('Cargo artifacts retain each test harness with its package directory and the binaries it can invoke', () => {
  const workspace = path.resolve('server');
  const target = path.join(workspace, 'target');
  const file = (name, isTest) => ({
    reason: 'compiler-artifact',
    manifest_path: path.join(workspace, 'crates', 'fixture', 'Cargo.toml'),
    executable: path.join(target, 'debug', name),
    profile: {test: isTest},
  });
  const messages = [file('fixture', false), file('fixture-tests', true), file('fixture-tests', true)];
  assert.deepEqual(cargoExecutables(messages.map(message => JSON.stringify(message)).join('\n'), workspace, target), [
    {executable: 'target/debug/fixture'},
    {executable: 'target/debug/fixture-tests', cwd: 'crates/fixture'},
  ]);
  assert.throws(() => cargoExecutables(JSON.stringify({...file('fixture', false), executable: path.join(workspace, 'elsewhere')}), workspace, target),
    /outside/);
  assert.throws(() => cargoExecutables(JSON.stringify({...file('custom', false), target: {kind: ['test'], name: 'custom'}}), workspace, target),
    /does not use libtest/);
});

test('partitions cover every listed case exactly once and preserve empty harnesses', () => {
  const suites = [
    {executable: 'unit', cwd: 'crate', names: parseLibtestList('unit::other: test\r\nunit::second: test\r\nbench: benchmark\r\n')},
    {executable: 'integration', cwd: 'crate', names: ['a', 'b', 'c', 'd', 'e', 'f']},
    {executable: 'empty', cwd: 'crate', names: []},
  ];
  for (const count of [1, 2, 8]) {
    const batches = partitionSuites(suites, count).flat();
    for (const source of suites) {
      const selected = batches.filter(batch => batch.executable === source.executable);
      assert.ok(selected.length >= 1);
      assert.ok(selected.every(batch => batch.cwd === source.cwd));
      assert.deepEqual(selected.flatMap(batch => batch.names).sort(), [...source.names].sort());
    }
  }
  assert.throws(() => parseLibtestList('unrecognized harness output'), /Unexpected/);
  assert.throws(() => parseLibtestList('x: test\nx: test'), /Duplicate/);
});

test('batches are serial and use exact names within the Windows command-line budget', () => {
  const names = Array.from({length: 750}, (_, i) => `group::test_case_${i}`);
  const batches = testArguments(names);
  assert.ok(batches.length > 1);
  assert.deepEqual(batches.flatMap(batch => batch.slice(2)), names);
  for (const batch of batches) {
    assert.deepEqual(batch.slice(0, 2), ['--test-threads=1', '--exact']);
    assert.ok(batch.join(' ').length < 16000);
  }
  assert.deepEqual(testArguments([]), [['--test-threads=1', '--exact']]);
});
