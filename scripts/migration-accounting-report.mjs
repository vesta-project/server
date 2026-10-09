import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const manifestPath = 'docs/RUST_WRAPPER_PARITY.json';

function referenceId(reference) {
  if (typeof reference === 'string') return reference;
  if (reference && typeof reference === 'object' && typeof reference.id === 'string') return reference.id;
  return null;
}

export function buildReport(manifest) {
  const requirements = Array.isArray(manifest?.baselineRequirements) ? manifest.baselineRequirements : [];
  const inventory = Array.isArray(manifest?.inventory) ? manifest.inventory : [];
  const requirementIds = new Set(requirements
    .filter((requirement) => requirement && typeof requirement.id === 'string')
    .map((requirement) => requirement.id));
  const referencedIds = new Set();

  for (const row of inventory) {
    for (const reference of Array.isArray(row?.applicableRequirements) ? row.applicableRequirements : []) {
      const id = referenceId(reference);
      if (id !== null) referencedIds.add(id);
    }
  }

  const unreferencedByPrefix = {};
  for (const requirement of requirements) {
    if (!requirement || typeof requirement.id !== 'string' || referencedIds.has(requirement.id)) continue;
    const prefix = requirement.id.replace(/-\d+$/, '');
    unreferencedByPrefix[prefix] = (unreferencedByPrefix[prefix] ?? 0) + 1;
  }

  const byStatus = {};
  const byKindBoundaryStatus = {};
  let rowsWithRustCounterpart = 0;
  for (const row of inventory) {
    const status = row?.status;
    const kind = row?.kind;
    const boundary = row?.boundary;
    byStatus[status] = (byStatus[status] ?? 0) + 1;
    const key = `${kind}|${boundary}|${status}`;
    byKindBoundaryStatus[key] = (byKindBoundaryStatus[key] ?? 0) + 1;
    if (row?.rustCounterpart !== null && row?.rustCounterpart !== undefined) rowsWithRustCounterpart += 1;
  }

  const referenced = [...referencedIds].filter((id) => requirementIds.has(id)).length;
  return {
    requirements: {
      total: requirements.length,
      referenced,
      unreferenced: requirements.length - referenced,
      unreferencedByPrefix,
    },
    inventory: {
      total: inventory.length,
      byStatus,
      byKindBoundaryStatus,
    },
    rowsWithRustCounterpart,
    rowsWithoutRustCounterpart: inventory.length - rowsWithRustCounterpart,
    unknownReferences: [...referencedIds].filter((id) => !requirementIds.has(id)).length,
  };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const manifest = JSON.parse(readFileSync(resolve(root, manifestPath), 'utf8'));
  console.log(JSON.stringify(buildReport(manifest), null, 2));
}
