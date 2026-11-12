import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

const root = fileURLToPath(new URL('../', import.meta.url));
const sourcePath = 'docs/SERVER_SOFTWARE_REQUIREMENTS.md';
const ledgerPath = resolve(root, 'docs/REQUIREMENT_TRACEABILITY.json');

const phases = {
  'Deployment and instance scope': 'P1',
  'Process modes': 'P3',
  'Installation components and process continuity': 'P3',
  'Identity, pairing, and authority': 'P2',
  'Unified principal model': 'P2',
  'Authorization': 'P2',
  'Built-in role defaults': 'P2',
  'Trust boundaries and powerful operational permissions': 'P2',
  'Pack access versus management access': 'P5',
  'Pack revisions and update policy': 'P5',
  'Live server files and hosted modpack publication': 'P5',
  'Pack authorization': 'P5',
  'Server operations': 'P7',
  'Lifecycle, maintenance, and sleep mode': 'P7',
  'Minecraft status and MOTD': 'P7',
  'Files and content': 'P4',
  'Backups': 'P6',
  'Backup crash-safety contract': 'P6',
  'Scheduling and notifications': 'P7',
  'Durable operations and concurrency': 'P1',
  'Console and player actions': 'P7',
  'Isolation': 'P3',
  'Java runtime': 'P3',
  'Metrics retention': 'P7',
  'Resource controls and diagnostics': 'P7',
  'Connector role': 'P3',
  'Connectivity and NAT': 'P8',
  'Management authentication': 'P2',
  'SFTP': 'P4',
  'Secrets, credentials, and abuse controls': 'P2',
  'Structured configuration': 'P4',
  'Data and recovery export': 'P6',
  'Agent state, repair, and pending changes': 'P1',
  'Software update policy': 'P8',
  'Installation experience': 'P8',
  'Audit retention': 'P1',
  'Discord integration': 'P7',
  'Configuration philosophy': 'P8',
  'Proxies and hosting extensibility': 'P8',
  'Extension and telemetry boundary': 'P8',
  'Security direction': 'P2',
};

export function extractRequirements(text) {
  const lines = text.replace(/\r\n/g, '\n').split('\n');
  const requirements = [];
  let section = '';
  let active = false;
  for (let i = 0; i < lines.length; i++) {
    const heading = /^(#{2,3}) (.+)$/.exec(lines[i]);
    if (heading) {
      section = heading[2];
      if (section === 'Locked decisions') active = true;
      if (section === 'Research influences') active = false;
      continue;
    }
    if (!active || !lines[i].startsWith('- ')) continue;
    const sourceLine = i + 1;
    let requirement = lines[i].slice(2);
    while (i + 1 < lines.length && /^  \S/.test(lines[i + 1])) {
      requirement += ' ' + lines[++i].trim();
    }
    if (!phases[section]) throw new Error(`Unmapped requirement section: ${section}`);
    const ordinal = requirements.filter((r) => r.section === section).length + 1;
    const slug = section.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/-$/, '');
    requirements.push({
      id: `${slug}-${String(ordinal).padStart(3, '0')}`,
      section,
      source: `${sourcePath}:${sourceLine}`,
      text: requirement,
      textSha256: createHash('sha256').update(requirement).digest('hex'),
      phase: phases[section],
      status: 'not_verified',
      tests: [],
      evidence: [],
    });
  }
  return requirements;
}

export function expectedLedger() {
  const text = readFileSync(resolve(root, sourcePath), 'utf8');
  let prior = [];
  try { prior = JSON.parse(readFileSync(ledgerPath, 'utf8')).requirements; }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  const requirements = extractRequirements(text).map((entry) => {
    const previous = prior.find((r) => r.id === entry.id && r.textSha256 === entry.textSha256);
    return previous ? { ...entry, status: previous.status, tests: previous.tests, evidence: previous.evidence } : entry;
  });
  return {
    formatVersion: 1,
    baselineDate: '2026-10-04',
    source: sourcePath,
    note: 'One entry per normative baseline bullet. Compound bullets require multiple assertions. Passing a focused test does not verify the complete bullet or its platform release gate. Deferred clauses remain explicitly governed by the accepted plan.',
    requirements,
  };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const expected = JSON.stringify(expectedLedger(), null, 2) + '\n';
  if (process.argv.includes('--check')) {
    if (readFileSync(ledgerPath, 'utf8').replace(/\r\n/g, '\n') !== expected) {
      throw new Error('Requirement ledger is stale; run node scripts/requirements-ledger.mjs');
    }
  } else {
    writeFileSync(ledgerPath, expected);
  }
  console.log(`${expectedLedger().requirements.length} baseline requirement bullets indexed`);
}
