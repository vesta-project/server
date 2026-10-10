#!/usr/bin/env node
// Regenerates the data block in docs/SERVER_SOFTWARE_ROADMAP.html from its sources:
// the status registry (snapshot) and the requirements and implementation plan Markdown.
// The page's markup, styling and rendering script are kept; only the embedded
// roadmap-data JSON block is replaced. Use --check to fail when the page is stale.
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const htmlPath = path.join(root, 'docs', 'SERVER_SOFTWARE_ROADMAP.html');
const statusPath = path.join(root, 'docs', 'SERVER_SOFTWARE_STATUS.json');
const requirementsPath = path.join(root, 'docs', 'SERVER_SOFTWARE_REQUIREMENTS.md');
const planPath = path.join(root, 'docs', 'SERVER_SOFTWARE_IMPLEMENTATION_PLAN.md');

const BLOCK = /<script id="roadmap-data" type="application\/json">([\s\S]*?)<\/script>/;

export function buildData({ status, requirements, plan }) {
  // Escape "<" so no string in the JSON can close the surrounding script element.
  return JSON.stringify({ snapshot: status, requirements, plan }).replace(/</g, '\\u003c');
}

export function renderPage(html, data) {
  if (!BLOCK.test(html)) throw new Error('roadmap-data block not found');
  return html.replace(BLOCK, () => `<script id="roadmap-data" type="application/json">${data}</script>`);
}

function sources() {
  return {
    status: JSON.parse(readFileSync(statusPath, 'utf8')),
    requirements: readFileSync(requirementsPath, 'utf8'),
    plan: readFileSync(planPath, 'utf8'),
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const html = readFileSync(htmlPath, 'utf8');
  const next = renderPage(html, buildData(sources()));
  if (process.argv.includes('--check')) {
    if (next !== html) {
      console.error('docs/SERVER_SOFTWARE_ROADMAP.html is out of date; run node scripts/build-roadmap-html.mjs');
      process.exit(1);
    }
    console.log('roadmap HTML is up to date');
  } else {
    writeFileSync(htmlPath, next);
    console.log('roadmap HTML regenerated');
  }
}
