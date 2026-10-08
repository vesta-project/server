import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { discoverJavaInventory, validateParity } from './rust-wrapper-parity.mjs';

const hash = (value) => createHash('sha256').update(value).digest('hex');
const sourceMarkdown = `# Fixture Requirements
## Locked decisions
### Deployment and instance scope
- A bounded fixture requirement.
## Research influences
- Not normative.
`;
const makeBaselineRequirements = () => [{ id: 'deployment-and-instance-scope-001', text: 'A bounded fixture requirement.' }];
const baselineDigest = (requirements) => hash(JSON.stringify(requirements.map(({ id, text }) => [id, text])));
const inventoryDigest = (inventory) => hash(JSON.stringify(inventory.map(({ javaPath, javaSha256 }) => [javaPath, javaSha256]).sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)));
const sourceFiles = {
  'wrapper/src/main/java/sample/Host.java': 'package sample; public final class Host {}\n',
  'core/src/test/java/sample/HostTest.java': 'package sample; class HostTest { @org.junit.jupiter.api.Test void works() {} }\n',
  'src/main/java/sample/Connector.java': 'package sample; public final class Connector {}\n',
  'src/test/java/sample/ConnectorTest.java': 'package sample; class ConnectorTest { @org.junit.jupiter.api.Test void works() {} }\n',
};

function makeFixture(t) {
  const baselineRequirements = makeBaselineRequirements();
  const baselineRequirementsSha256 = baselineDigest(baselineRequirements);
  const repoRoot = mkdtempSync(join(tmpdir(), 'rust-wrapper-parity-'));
  t.after(() => rmSync(repoRoot, { recursive: true, force: true }));
  for (const [path, body] of Object.entries(sourceFiles)) {
    const target = join(repoRoot, path);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, body);
  }
  const sourcePaths = discoverJavaInventory(repoRoot);
  const inventory = sourcePaths.map((javaPath) => {
    const isPlugin = javaPath.startsWith('src/');
    const isTest = javaPath.endsWith('Test.java');
    const simpleName = javaPath.split('/').at(-1).replace(/\.java$/, '');
    const retained = isPlugin;
    return {
      javaPath,
      symbol: javaPath.endsWith('/Host.java') ? 'Host' : (isTest ? `${simpleName}, ${simpleName}#works` : 'Connector'),
      ...(isTest ? { testFamily: `sample:${simpleName.replace(/Test$/, '')}` } : {}),
      kind: isTest ? 'test' : 'production',
      boundary: isPlugin ? 'plugin_boundary' : (javaPath.startsWith('core/') ? 'shared_host_plugin_boundary' : 'host'),
      javaSha256: hash(readFileSync(join(repoRoot, javaPath))),
      rustCounterpart: null,
      testPaths: javaPath.endsWith('/Host.java') ? ['core/src/test/java/sample/HostTest.java'] : (isTest ? [javaPath] : []),
      fixturePaths: [],
      applicableRequirements: [],
      disposition: retained ? 'retain_java_plugin' : 'migrate_to_rust',
      status: retained ? 'retained_java_plugin' : 'not_migrated',
      rationale: retained ? 'Fixture connector remains at Java plugin boundary.' : 'Fixture baseline; no migration claim.',
      gaps: retained ? [] : ['Parity not yet assessed.'],
      rustTests: [],
      evidence: [],
      results: { status: 'not_run', summary: 'Fixture baseline only.' },
    };
  });
  const manifest = {
    formatVersion: 1,
    baselineDate: '2026-10-07',
    sourceRequirements: 'docs/SERVER_SOFTWARE_REQUIREMENTS.md',
    sourceTraceability: 'docs/REQUIREMENT_TRACEABILITY.json',
    note: 'Fixture manifest.',
    checkerScopeNote: 'This validates structure only; it is not a test runner and not security certification.',
    mappingNote: 'Fixture mapping.',
    overallStatus: 'baseline_inventory',
    finalIndependentSolReview: { status: 'pending', model: null, date: null, evidence: [] },
    platformGates: {
      windows: { status: 'not_run', evidence: [] },
      linux: { status: 'not_run', evidence: [] },
    },
    baselineRequirements,
    baselineRequirementsSha256,
    baselineInventorySha256: inventoryDigest(inventory),
    inventory,
  };
  const ledger = {
    formatVersion: 1,
    requirements: [{
      id: baselineRequirements[0].id,
      text: baselineRequirements[0].text,
      status: 'not_verified',
      tests: [],
      evidence: [],
    }],
  };
  const ledgerPath = join(repoRoot, 'docs/REQUIREMENT_TRACEABILITY.json');
  mkdirSync(dirname(ledgerPath), { recursive: true });
  writeFileSync(ledgerPath, JSON.stringify(ledger));
  writeFileSync(join(repoRoot, 'docs/SERVER_SOFTWARE_REQUIREMENTS.md'), sourceMarkdown);
  return { repoRoot, manifest, ledger };
}

function messages(fixture) {
  return validateParity(fixture.manifest, fixture.repoRoot, fixture.ledger);
}

function makeComplete(fixture) {
  const evidencePaths = [
    'rust/src/host.rs',
    'rust/tests/host.rs',
    'evidence/host-test-result.txt',
    'evidence/sol-review.txt',
    'evidence/windows-gate.txt',
    'evidence/linux-gate.txt',
  ];
  for (const path of evidencePaths) {
    const target = join(fixture.repoRoot, path);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, `fixture evidence for ${path}\n`);
  }
  fixture.manifest.overallStatus = 'complete';
  fixture.manifest.finalIndependentSolReview = {
    status: 'passed', model: 'gpt-6.1-sol', date: '2026-10-07',
    evidence: [{ path: 'evidence/sol-review.txt', claim: 'Fixture review record.', date: '2026-10-07' }],
  };
  fixture.manifest.platformGates = {
    windows: { status: 'passed', evidence: [{ path: 'evidence/windows-gate.txt', claim: 'Fixture Windows gate record.', date: '2026-10-07' }] },
    linux: { status: 'passed', evidence: [{ path: 'evidence/linux-gate.txt', claim: 'Fixture Linux gate record.', date: '2026-10-07' }] },
  };
  for (const row of fixture.manifest.inventory) {
    if (row.status === 'retained_java_plugin') continue;
    row.status = 'verified';
    row.rustCounterpart = 'rust/src/host.rs';
    row.rustTests = ['rust/tests/host.rs'];
    row.evidence = [{ path: 'evidence/host-test-result.txt', claim: 'Fixture test output was recorded.' }];
    row.gaps = [];
    row.results = { status: 'passed', summary: 'Fixture assertions passed.' };
  }
}

test('accepts explicit fixture inventory and ignores mutable evidence changes', (t) => {
  const fixture = makeFixture(t);
  fixture.ledger.requirements[0].status = 'partially_verified';
  fixture.ledger.requirements[0].tests = ['wrapper/src/test/java/sample/ChangedTest.java'];
  fixture.ledger.requirements[0].evidence = ['new evidence does not rewrite baseline text'];
  assert.deepEqual(messages(fixture), []);
});

test('includes src plugin production and test Java as retained Java-boundary inventory', (t) => {
  const fixture = makeFixture(t);
  const pluginRows = fixture.manifest.inventory.filter((row) => row.javaPath.startsWith('src/'));
  assert.equal(pluginRows.length, 2);
  for (const row of pluginRows) {
    assert.equal(row.boundary, 'plugin_boundary');
    assert.equal(row.status, 'retained_java_plugin');
    assert.equal(row.disposition, 'retain_java_plugin');
    assert.match(row.rationale, /Java plugin boundary/);
  }
  fixture.manifest.inventory = fixture.manifest.inventory.filter((row) => !row.javaPath.endsWith('/ConnectorTest.java'));
  assert.ok(messages(fixture).some((message) => message.includes('src/test/java/sample/ConnectorTest.java')));
});

test('detects missing, extra, and duplicate Java inventory paths', (t) => {
  const fixture = makeFixture(t);
  fixture.manifest.inventory.pop();
  assert.ok(messages(fixture).some((message) => message.includes('absent from explicit inventory')));

  const extra = makeFixture(t);
  extra.manifest.inventory.push({ ...extra.manifest.inventory[0], javaPath: 'wrapper/src/main/java/sample/Extra.java' });
  assert.ok(messages(extra).some((message) => message.includes('extra/non-source Java path')));

  const duplicate = makeFixture(t);
  duplicate.manifest.inventory.push({ ...duplicate.manifest.inventory[0] });
  assert.ok(messages(duplicate).some((message) => message.includes('duplicate Java inventory path')));
});

test('detects missing source files and changed baseline source bytes', (t) => {
  const missing = makeFixture(t);
  rmSync(join(missing.repoRoot, missing.manifest.inventory[0].javaPath));
  assert.ok(messages(missing).some((message) => message.includes('Java source is missing')));

  const changed = makeFixture(t);
  writeFileSync(join(changed.repoRoot, changed.manifest.inventory[0].javaPath), 'changed source\n');
  assert.ok(messages(changed).some((message) => message.includes('baseline Java byte hash changed')));

  const tamperedBaseline = makeFixture(t);
  tamperedBaseline.manifest.inventory[0].javaSha256 = '0'.repeat(64);
  assert.ok(messages(tamperedBaseline).some((message) => message.includes('baseline Java path/hash digest')));
});

test('rejects invalid status, unsafe source path, and unknown requirement references', (t) => {
  const badStatus = makeFixture(t);
  badStatus.manifest.inventory[0].status = 'done';
  assert.ok(messages(badStatus).some((message) => message.includes('.status is invalid')));

  const badPath = makeFixture(t);
  badPath.manifest.inventory[0].javaPath = '../outside.java';
  assert.ok(messages(badPath).some((message) => message.includes('normalized repository-relative path')));

  const badRequirement = makeFixture(t);
  badRequirement.manifest.inventory[0].applicableRequirements = ['invented-requirement-999'];
  assert.ok(messages(badRequirement).some((message) => message.includes('unknown requirement reference')));
});

test('freezes baseline IDs and normative text while allowing current evidence updates', (t) => {
  const changedId = makeFixture(t);
  changedId.ledger.requirements[0].id = 'replacement-id-001';
  assert.ok(messages(changedId).some((message) => message.includes('baseline requirement ID is missing')));

  const changedText = makeFixture(t);
  changedText.ledger.requirements[0].text = 'Rewritten normative requirement.';
  assert.ok(messages(changedText).some((message) => message.includes('baseline normative text changed')));

  const changedSnapshot = makeFixture(t);
  changedSnapshot.manifest.baselineRequirements[0].text = 'Tampered snapshot text.';
  assert.ok(messages(changedSnapshot).some((message) => message.includes('baseline requirement ID/text digest')));
});

test('rejects unresolved overall-complete claims and incomplete migration evidence', (t) => {
  const unresolved = makeFixture(t);
  unresolved.manifest.overallStatus = 'complete';
  const unresolvedErrors = messages(unresolved);
  assert.ok(unresolvedErrors.some((message) => message.includes('cannot be complete with')));
  assert.ok(unresolvedErrors.some((message) => message.includes('finalIndependentSolReview.status passed')));
  assert.ok(unresolvedErrors.some((message) => message.includes('windows platform gate passed')));
  assert.ok(unresolvedErrors.some((message) => message.includes('linux platform gate passed')));

  const incompleteClaim = makeFixture(t);
  incompleteClaim.manifest.inventory.find((row) => row.disposition === 'migrate_to_rust').status = 'migrated';
  const errors = messages(incompleteClaim);
  assert.ok(errors.some((message) => message.includes('no valid Rust counterpart')));
  assert.ok(errors.some((message) => message.includes('no Rust test paths')));
  assert.ok(errors.some((message) => message.includes('no path-backed evidence')));
});

test('complete status requires a dated path-backed final Sol review and both platform gates', (t) => {
  const missingReviewEvidence = makeFixture(t);
  makeComplete(missingReviewEvidence);
  missingReviewEvidence.manifest.finalIndependentSolReview.evidence = [];
  assert.ok(messages(missingReviewEvidence).some((message) => message.includes('finalIndependentSolReview requires path-backed dated evidence')));

  const missingPlatformEvidence = makeFixture(t);
  makeComplete(missingPlatformEvidence);
  missingPlatformEvidence.manifest.platformGates.windows.evidence = [];
  assert.ok(messages(missingPlatformEvidence).some((message) => message.includes('platformGates.windows requires path-backed dated evidence')));

  const badPlatform = makeFixture(t);
  makeComplete(badPlatform);
  badPlatform.manifest.platformGates.linux.status = 'not_run';
  assert.ok(messages(badPlatform).some((message) => message.includes('linux platform gate passed')));
});

test('verified rows cannot retain unresolved gaps', (t) => {
  const fixture = makeFixture(t);
  makeComplete(fixture);
  const verified = fixture.manifest.inventory.find((row) => row.status === 'verified');
  verified.gaps = ['Unresolved migration gap.'];
  assert.ok(messages(fixture).some((message) => message.includes('verified row has unresolved gaps')));
});

test('raw accepted requirement Markdown drift is caught even when the ledger is unchanged', (t) => {
  const fixture = makeFixture(t);
  const markdownPath = join(fixture.repoRoot, 'docs/SERVER_SOFTWARE_REQUIREMENTS.md');
  writeFileSync(markdownPath, sourceMarkdown.replace('A bounded fixture requirement.', 'A changed normative fixture requirement.'));
  assert.ok(messages(fixture).some((message) => message.includes('stale relative to accepted requirement Markdown')));
});

test('accepts a complete claim only when every Java row has real Rust paths and evidence', (t) => {
  const fixture = makeFixture(t);
  makeComplete(fixture);
  assert.deepEqual(messages(fixture), []);
});

test('accepts multiple Java behavior references to one cohesive Rust module', (t) => {
  const fixture = makeFixture(t);
  const original = fixture.manifest.inventory.find((row) => row.javaPath.endsWith('/Host.java'));
  const javaPath = 'wrapper/src/main/java/sample/Related.java';
  const body = 'package sample; public final class Related {}\n';
  writeFileSync(join(fixture.repoRoot, javaPath), body);
  fixture.manifest.inventory.push({
    ...structuredClone(original),
    javaPath,
    symbol: 'Related',
    javaSha256: hash(body),
    rationale: 'A separate Java implementation detail belongs to the same Rust domain boundary.',
  });
  // This is an independently constructed test baseline, not a regenerated product inventory.
  fixture.manifest.baselineInventorySha256 = inventoryDigest(fixture.manifest.inventory);
  makeComplete(fixture);
  const hostRows = fixture.manifest.inventory.filter((row) => row.kind === 'production' && row.boundary === 'host');
  assert.equal(hostRows.length, 2);
  assert.equal(new Set(hostRows.map((row) => row.rustCounterpart)).size, 1);
  assert.deepEqual(messages(fixture), []);
});

test('validates test and fixture pointers against inert repository files', (t) => {
  const fixture = makeFixture(t);
  fixture.manifest.inventory[0].fixturePaths = ['fixtures/not-present.json'];
  assert.ok(messages(fixture).some((message) => message.includes('fixture path is missing')));

  const existing = makeFixture(t);
  mkdirSync(join(existing.repoRoot, 'fixtures'), { recursive: true });
  writeFileSync(join(existing.repoRoot, 'fixtures/data.json'), '{}');
  existing.manifest.inventory[0].fixturePaths = ['fixtures/data.json'];
  assert.deepEqual(messages(existing), []);
});
