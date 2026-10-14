import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { extractRequirements } from './requirements-ledger.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
const manifestPath = 'docs/RUST_WRAPPER_PARITY.json';
const ledgerPath = 'docs/REQUIREMENT_TRACEABILITY.json';
const javaRoots = [
  'wrapper/src/main/java',
  'core/src/main/java',
  'wrapper/src/test/java',
  'core/src/test/java',
  'src/main/java',
  'src/test/java',
];
const statuses = new Set([
  'not_migrated',
  'retained_java_plugin',
  'planned_safer_replacement',
  'migrated',
  'verified',
]);
const dispositions = new Set([
  'migrate_to_rust',
  'retain_java_plugin',
  'safer_replacement',
  'pending_mapping',
]);
const boundaries = new Set(['host', 'plugin_boundary', 'shared_host_plugin_boundary']);
const kinds = new Set(['production', 'test', 'test_helper']);
const sha256 = (value) => createHash('sha256').update(value).digest('hex');
const canonicalInventory = (rows) => JSON.stringify(rows
  .filter((row) => row && typeof row.javaPath === 'string' && typeof row.javaSha256 === 'string')
  .map(({ javaPath, javaSha256 }) => [javaPath, javaSha256])
  .sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0));
const isRelativeRepoPath = (value) => typeof value === 'string'
  && value.length > 0
  && !value.includes('\\')
  && !isAbsolute(value)
  && !/^[a-zA-Z]:/.test(value)
  && value.split('/').every((part) => part && part !== '.' && part !== '..');
const repoFile = (repoRoot, path) => resolve(repoRoot, ...path.split('/'));
const sha256File = (path) => sha256(readFileSync(path));

function walkJava(rootDir, repoRoot, out = []) {
  if (!existsSync(rootDir)) return out;
  for (const entry of readdirSync(rootDir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    const fullPath = join(rootDir, entry.name);
    if (entry.isDirectory()) walkJava(fullPath, repoRoot, out);
    else if (entry.isFile() && entry.name.endsWith('.java')) {
      out.push(relative(repoRoot, fullPath).split(sep).join('/'));
    }
  }
  return out;
}

export function discoverJavaInventory(repoRoot) {
  return javaRoots.flatMap((sourceRoot) => walkJava(repoFile(repoRoot, sourceRoot), repoRoot)).sort();
}

function add(errors, condition, message) {
  if (!condition) errors.push(message);
}

function isDate(value) {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
  const date = new Date(`${value}T00:00:00.000Z`);
  return Number.isFinite(date.getTime()) && date.toISOString().slice(0, 10) === value;
}

function validateDatedEvidence(items, label, repoRoot, errors, required) {
  add(errors, Array.isArray(items), `${label} evidence must be an array`);
  if (!Array.isArray(items)) return;
  if (required) add(errors, items.length > 0, `${label} requires path-backed dated evidence`);
  for (const item of items) {
    add(errors, item && typeof item === 'object' && !Array.isArray(item), `${label} evidence entries must be objects`);
    if (!item || typeof item !== 'object' || Array.isArray(item)) continue;
    add(errors, isRelativeRepoPath(item.path), `${label} evidence path is invalid`);
    add(errors, typeof item.claim === 'string' && item.claim.trim().length > 0, `${label} evidence claim is required`);
    add(errors, isDate(item.date), `${label} evidence date must be YYYY-MM-DD`);
    if (isRelativeRepoPath(item.path)) {
      const path = repoFile(repoRoot, item.path);
      add(errors, existsSync(path), `${label} evidence file is missing: ${item.path}`);
      if (existsSync(path)) add(errors, statSync(path).isFile() && statSync(path).size > 0, `${label} evidence must reference a non-empty file: ${item.path}`);
    }
  }
}

function validateIndependentGates(manifest, repoRoot, errors) {
  const review = manifest.finalIndependentSolReview;
  add(errors, review && typeof review === 'object' && !Array.isArray(review), 'finalIndependentSolReview must be an object');
  if (review && typeof review === 'object' && !Array.isArray(review)) {
    add(errors, ['pending', 'not_run', 'passed', 'failed'].includes(review.status), 'finalIndependentSolReview.status is invalid');
    add(errors, review.model === null || typeof review.model === 'string', 'finalIndependentSolReview.model must be null or a string');
    add(errors, review.date === null || isDate(review.date), 'finalIndependentSolReview.date must be null or YYYY-MM-DD');
    validateDatedEvidence(review.evidence, 'finalIndependentSolReview', repoRoot, errors, false);
    if (manifest.overallStatus === 'complete') {
      add(errors, review.status === 'passed', 'overall complete requires finalIndependentSolReview.status passed');
      add(errors, review.model === 'gpt-6.1-sol', 'overall complete requires finalIndependentSolReview.model gpt-6.1-sol');
      add(errors, isDate(review.date), 'overall complete requires a dated final independent Sol review');
      validateDatedEvidence(review.evidence, 'finalIndependentSolReview', repoRoot, errors, true);
    }
  }

  const gates = manifest.platformGates;
  add(errors, gates && typeof gates === 'object' && !Array.isArray(gates), 'platformGates must be an object');
  if (gates && typeof gates === 'object' && !Array.isArray(gates)) {
    for (const platform of ['windows', 'linux']) {
      const gate = gates[platform];
      add(errors, gate && typeof gate === 'object' && !Array.isArray(gate), `platformGates.${platform} must be an object`);
      if (!gate || typeof gate !== 'object' || Array.isArray(gate)) continue;
      add(errors, ['not_run', 'passed', 'failed', 'skipped'].includes(gate.status), `platformGates.${platform}.status is invalid`);
      validateDatedEvidence(gate.evidence, `platformGates.${platform}`, repoRoot, errors, false);
      if (manifest.overallStatus === 'complete') {
        add(errors, gate.status === 'passed', `overall complete requires ${platform} platform gate passed`);
        validateDatedEvidence(gate.evidence, `platformGates.${platform}`, repoRoot, errors, true);
      }
    }
  }
}

function validateManifestShape(manifest, errors) {
  add(errors, manifest && typeof manifest === 'object' && !Array.isArray(manifest), 'manifest must be an object');
  if (!manifest || typeof manifest !== 'object' || Array.isArray(manifest)) return;
  add(errors, manifest.formatVersion === 1, 'formatVersion must be 1');
  add(errors, typeof manifest.checkerScopeNote === 'string' && /structure only/i.test(manifest.checkerScopeNote) && /not a test runner/i.test(manifest.checkerScopeNote) && /not security certification/i.test(manifest.checkerScopeNote), 'checkerScopeNote must disclaim test-runner and security-certification authority');
  add(errors, ['baseline_inventory', 'in_progress', 'complete'].includes(manifest.overallStatus), 'overallStatus is invalid');
  add(errors, Array.isArray(manifest.baselineRequirements), 'baselineRequirements must be an array');
  add(errors, typeof manifest.baselineRequirementsSha256 === 'string' && /^[a-f0-9]{64}$/.test(manifest.baselineRequirementsSha256), 'baselineRequirementsSha256 must be a SHA-256 hex digest');
  add(errors, Array.isArray(manifest.inventory), 'inventory must be an array');
  add(errors, typeof manifest.baselineInventorySha256 === 'string' && /^[a-f0-9]{64}$/.test(manifest.baselineInventorySha256), 'baselineInventorySha256 must be a SHA-256 hex digest');
}

function validateBaseline(manifest, ledger, errors) {
  if (!Array.isArray(manifest.baselineRequirements)) return new Set();
  const ids = new Set();
  const pairs = [];
  for (const [index, item] of manifest.baselineRequirements.entries()) {
    add(errors, item && typeof item === 'object' && !Array.isArray(item), `baselineRequirements[${index}] must be an object`);
    if (!item || typeof item !== 'object' || Array.isArray(item)) continue;
    add(errors, typeof item.id === 'string' && item.id.length > 0, `baselineRequirements[${index}].id is invalid`);
    add(errors, typeof item.text === 'string' && item.text.length > 0, `baselineRequirements[${index}].text is invalid`);
    if (typeof item.id === 'string') {
      add(errors, !ids.has(item.id), `duplicate baseline requirement ID: ${item.id}`);
      ids.add(item.id);
    }
    if (typeof item.id === 'string' && typeof item.text === 'string') pairs.push([item.id, item.text]);
  }
  add(errors, sha256(JSON.stringify(pairs)) === manifest.baselineRequirementsSha256, 'baseline requirement ID/text digest does not match its snapshot');

  if (!ledger || !Array.isArray(ledger.requirements)) {
    errors.push('current requirement traceability ledger is missing or malformed');
    return ids;
  }
  const current = new Map();
  for (const item of ledger.requirements) {
    if (item && typeof item.id === 'string') {
      add(errors, !current.has(item.id), `duplicate current requirement ID: ${item.id}`);
      current.set(item.id, item.text);
    }
  }
  for (const item of manifest.baselineRequirements) {
    if (!item || typeof item.id !== 'string') continue;
    add(errors, current.has(item.id), `baseline requirement ID is missing from current ledger: ${item.id}`);
    add(errors, current.get(item.id) === item.text, `baseline normative text changed for requirement ID: ${item.id}`);
  }
  return new Set([...current.keys()]);
}

function validateRequirementsSource(repoRoot, ledger, errors) {
  if (!ledger || !Array.isArray(ledger.requirements)) return;
  let markdown;
  try { markdown = readFileSync(repoFile(repoRoot, 'docs/SERVER_SOFTWARE_REQUIREMENTS.md'), 'utf8'); }
  catch { errors.push('accepted requirement Markdown source is missing or unreadable'); return; }
  try {
    const fromMarkdown = extractRequirements(markdown).map(({ id, text }) => [id, text]);
    const fromLedger = ledger.requirements.map(({ id, text }) => [id, text]);
    add(errors, JSON.stringify(fromMarkdown) === JSON.stringify(fromLedger), 'requirement traceability ledger is stale relative to accepted requirement Markdown ID/text');
  } catch (error) {
    errors.push(`could not extract accepted requirement Markdown: ${error.message}`);
  }
}

function validateInventory(manifest, repoRoot, knownRequirements, errors, javaRepoRoot) {
  if (!Array.isArray(manifest.inventory)) return;
  const rows = manifest.inventory;
  add(errors, sha256(canonicalInventory(rows)) === manifest.baselineInventorySha256, 'baseline Java path/hash digest does not match inventory');
  const byPath = new Map();
  for (const [index, row] of rows.entries()) {
    const where = `inventory[${index}]`;
    add(errors, row && typeof row === 'object' && !Array.isArray(row), `${where} must be an object`);
    if (!row || typeof row !== 'object' || Array.isArray(row)) continue;
    add(errors, isRelativeRepoPath(row.javaPath), `${where}.javaPath must be a normalized repository-relative path`);
    if (!isRelativeRepoPath(row.javaPath)) continue;
    add(errors, !byPath.has(row.javaPath), `duplicate Java inventory path: ${row.javaPath}`);
    byPath.set(row.javaPath, row);
    const sourceRoot = javaRoots.find((candidate) => row.javaPath.startsWith(`${candidate}/`));
    add(errors, Boolean(sourceRoot), `${where}.javaPath is outside the configured Java source roots: ${row.javaPath}`);
    add(errors, kinds.has(row.kind), `${where}.kind is invalid: ${row.javaPath}`);
    add(errors, typeof row.symbol === 'string' && row.symbol.trim().length > 0, `${where}.symbol is required: ${row.javaPath}`);
    if (row.kind === 'test') add(errors, typeof row.testFamily === 'string' && row.testFamily.trim().length > 0, `${where}.testFamily is required: ${row.javaPath}`);
    if (row.kind === 'production') add(errors, sourceRoot?.endsWith('src/main/java'), `production kind must use a main Java root: ${row.javaPath}`);
    if (row.kind === 'test' || row.kind === 'test_helper') add(errors, sourceRoot?.endsWith('src/test/java'), `test/helper kind must use a test Java root: ${row.javaPath}`);
    if (sourceRoot?.endsWith('src/main/java')) add(errors, row.kind === 'production', `main Java source cannot be classified as a test/helper: ${row.javaPath}`);
    if (sourceRoot?.endsWith('src/test/java')) add(errors, row.kind !== 'production', `test Java source cannot be classified as production: ${row.javaPath}`);
    add(errors, boundaries.has(row.boundary), `${where}.boundary is invalid: ${row.javaPath}`);
    if (row.javaPath.startsWith('wrapper/')) add(errors, row.boundary === 'host', `wrapper source must be classified as host: ${row.javaPath}`);
    if (row.javaPath.startsWith('core/')) add(errors, row.boundary === 'plugin_boundary' || row.boundary === 'shared_host_plugin_boundary', `core source must record its plugin/shared boundary: ${row.javaPath}`);
    if (row.javaPath.startsWith('src/')) {
      add(errors, row.boundary === 'plugin_boundary', `src Java source must be classified at the plugin boundary: ${row.javaPath}`);
      add(errors, row.status === 'retained_java_plugin' && row.disposition === 'retain_java_plugin', `src Java source must be explicitly retained as Java plugin code: ${row.javaPath}`);
    }
    add(errors, typeof row.javaSha256 === 'string' && /^[a-f0-9]{64}$/.test(row.javaSha256), `${where}.javaSha256 must be a SHA-256 hex digest: ${row.javaPath}`);
    add(errors, dispositions.has(row.disposition), `${where}.disposition is invalid: ${row.javaPath}`);
    add(errors, statuses.has(row.status), `${where}.status is invalid: ${row.javaPath}`);
    add(errors, typeof row.rationale === 'string' && row.rationale.trim().length > 0, `${where}.rationale is required: ${row.javaPath}`);
    add(errors, Array.isArray(row.applicableRequirements), `${where}.applicableRequirements must be an array: ${row.javaPath}`);
    add(errors, Array.isArray(row.testPaths), `${where}.testPaths must be an array: ${row.javaPath}`);
    add(errors, Array.isArray(row.fixturePaths), `${where}.fixturePaths must be an array: ${row.javaPath}`);
    add(errors, Array.isArray(row.gaps), `${where}.gaps must be an array: ${row.javaPath}`);
    add(errors, Array.isArray(row.rustTests), `${where}.rustTests must be an array: ${row.javaPath}`);
    add(errors, Array.isArray(row.evidence), `${where}.evidence must be an array: ${row.javaPath}`);
    add(errors, row.rustCounterpart === null || typeof row.rustCounterpart === 'string', `${where}.rustCounterpart must be null or a path: ${row.javaPath}`);
    add(errors, row.results && typeof row.results === 'object' && !Array.isArray(row.results) && typeof row.results.status === 'string' && typeof row.results.summary === 'string', `${where}.results must include status and summary: ${row.javaPath}`);
    add(errors, Array.isArray(row.gaps) && row.gaps.every((gap) => typeof gap === 'string'), `${where}.gaps must contain only strings: ${row.javaPath}`);
    add(errors, row.results && ['not_run', 'passed', 'failed', 'skipped', 'unverified'].includes(row.results.status), `${where}.results.status is invalid: ${row.javaPath}`);

    const abs = repoFile(javaRepoRoot, row.javaPath);
    add(errors, existsSync(abs), `inventory Java source is missing: ${row.javaPath}`);
    if (existsSync(abs) && typeof row.javaSha256 === 'string' && /^[a-f0-9]{64}$/.test(row.javaSha256)) {
      add(errors, sha256File(abs) === row.javaSha256, `baseline Java byte hash changed; explicitly review/update inventory: ${row.javaPath}`);
    }
    for (const requirementId of Array.isArray(row.applicableRequirements) ? row.applicableRequirements : []) {
      add(errors, typeof requirementId === 'string' && knownRequirements.has(requirementId), `unknown requirement reference ${requirementId} in ${row.javaPath}`);
    }
    for (const path of Array.isArray(row.testPaths) ? row.testPaths : []) {
      add(errors, isRelativeRepoPath(path) && javaRoots.some((testRoot) => testRoot.endsWith('src/test/java') && path.startsWith(`${testRoot}/`)) && path.endsWith('.java'), `invalid Java test path ${path} in ${row.javaPath}`);
      if (isRelativeRepoPath(path)) add(errors, existsSync(repoFile(javaRepoRoot, path)), `test path is missing: ${path} (referenced by ${row.javaPath})`);
    }
    for (const path of Array.isArray(row.fixturePaths) ? row.fixturePaths : []) {
      add(errors, isRelativeRepoPath(path), `invalid fixture path ${path} in ${row.javaPath}`);
      if (isRelativeRepoPath(path)) add(errors, existsSync(evidenceFile(repoRoot, javaRepoRoot, path)), `fixture path is missing: ${path} (referenced by ${row.javaPath})`);
    }
    if (row.rustCounterpart !== null) {
      add(errors, isRelativeRepoPath(row.rustCounterpart) && row.rustCounterpart.endsWith('.rs'), `invalid Rust counterpart path: ${row.rustCounterpart}`);
      if (isRelativeRepoPath(row.rustCounterpart)) add(errors, existsSync(repoFile(repoRoot, row.rustCounterpart)), `Rust counterpart path is missing: ${row.rustCounterpart}`);
    }
    for (const path of Array.isArray(row.rustTests) ? row.rustTests : []) {
      add(errors, isRelativeRepoPath(path) && path.endsWith('.rs'), `invalid Rust test path: ${path}`);
      if (isRelativeRepoPath(path)) add(errors, existsSync(repoFile(repoRoot, path)), `Rust test path is missing: ${path}`);
    }
    for (const item of Array.isArray(row.evidence) ? row.evidence : []) {
      add(errors, item && typeof item === 'object' && isRelativeRepoPath(item.path) && typeof item.claim === 'string' && item.claim.trim().length > 0, `invalid path-backed evidence entry: ${row.javaPath}`);
      if (item && isRelativeRepoPath(item.path)) add(errors, existsSync(evidenceFile(repoRoot, javaRepoRoot, item.path)), `evidence path is missing: ${item.path}`);
    }
    if (row.status === 'retained_java_plugin') add(errors, row.disposition === 'retain_java_plugin', `retained_java_plugin status requires retain_java_plugin disposition: ${row.javaPath}`);
    if (row.status === 'planned_safer_replacement') add(errors, row.disposition === 'safer_replacement', `planned_safer_replacement status requires safer_replacement disposition: ${row.javaPath}`);
    if (row.status === 'not_migrated') add(errors, ['migrate_to_rust', 'pending_mapping'].includes(row.disposition), `not_migrated status requires a migration or pending-mapping disposition: ${row.javaPath}`);

    if (row.status === 'verified') add(errors, Array.isArray(row.gaps) && row.gaps.length === 0, `verified row has unresolved gaps: ${row.javaPath}`);
    if (row.status === 'migrated' || row.status === 'verified') {
      add(errors, isRelativeRepoPath(row.rustCounterpart) && row.rustCounterpart.endsWith('.rs'), `completed claim has no valid Rust counterpart path: ${row.javaPath}`);
      add(errors, Array.isArray(row.rustTests) && row.rustTests.length > 0, `completed claim has no Rust test paths: ${row.javaPath}`);
      add(errors, Array.isArray(row.evidence) && row.evidence.some((item) => item && isRelativeRepoPath(item.path) && typeof item.claim === 'string' && item.claim.trim()), `completed claim has no path-backed evidence: ${row.javaPath}`);
      if (row.status === 'verified') add(errors, row.results?.status === 'passed', `verified claim must record passed results: ${row.javaPath}`);
    }
  }

  const discovered = discoverJavaInventory(javaRepoRoot);
  const actual = new Set(byPath.keys());
  for (const path of discovered) add(errors, actual.has(path), `Java source is absent from explicit inventory: ${path}`);
  for (const path of actual) add(errors, discovered.includes(path), `inventory contains an extra/non-source Java path: ${path}`);

  if (manifest.overallStatus === 'complete') {
    const unresolved = rows.filter((row) => !row || !['verified', 'retained_java_plugin'].includes(row.status));
    add(errors, unresolved.length === 0, `overallStatus cannot be complete with ${unresolved.length} unresolved inventory row(s)`);
  }
}

function evidenceFile(repoRoot, javaRepoRoot, path) {
  const isJava = ['core/', 'wrapper/', 'src/'].some((prefix) => path.startsWith(prefix));
  return repoFile(isJava ? javaRepoRoot : repoRoot, path);
}

export function validateParity(manifest, repoRoot, ledgerOverride, javaRepoRoot = repoRoot) {
  const errors = [];
  validateManifestShape(manifest, errors);
  if (!manifest || typeof manifest !== 'object' || Array.isArray(manifest)) return errors;
  let ledger = ledgerOverride;
  if (ledger === undefined) {
    try { ledger = JSON.parse(readFileSync(repoFile(repoRoot, ledgerPath), 'utf8')); }
    catch { ledger = null; }
  }
  const knownRequirements = validateBaseline(manifest, ledger, errors);
  validateRequirementsSource(repoRoot, ledger, errors);
  validateIndependentGates(manifest, repoRoot, errors);
  validateInventory(manifest, repoRoot, knownRequirements, errors, javaRepoRoot);
  return errors;
}

export function checkParity(repoRoot = root, javaRepoRoot = process.env.VESTA_JAVA_REPO || resolve(repoRoot, '../server-java')) {
  const manifest = JSON.parse(readFileSync(repoFile(repoRoot, manifestPath), 'utf8'));
  const source = manifest.javaSource;
  if (source?.repository !== 'https://github.com/vesta-project/server-java'
      || !/^[a-f0-9]{40}$/.test(source?.revision ?? '')) {
    throw new Error('Java migration source must pin the server-java repository and an exact commit');
  }
  if (!existsSync(javaRepoRoot)) throw new Error('Pinned Java checkout missing; clone server-java or set VESTA_JAVA_REPO');
  const head = execFileSync('git', ['-C', javaRepoRoot, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  if (head !== source.revision) throw new Error(`Java migration checkout must be at pinned revision ${source.revision}`);
  const errors = validateParity(manifest, repoRoot, undefined, javaRepoRoot);
  if (errors.length) throw new Error(`Rust wrapper parity check failed:\n- ${errors.join('\n- ')}`);
  return { files: manifest.inventory.length, baselineRequirements: manifest.baselineRequirements.length };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const result = checkParity(root);
  console.log(`Rust wrapper parity inventory structure valid: ${result.files} Java files; ${result.baselineRequirements} frozen baseline requirements (not a test run or security certification)`);
}
