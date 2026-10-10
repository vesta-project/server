import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildData, renderPage } from './build-roadmap-html.mjs';

const page = '<html><body><script id="roadmap-data" type="application/json">{}</script></body></html>';

test('regeneration is idempotent for the same sources', () => {
  const data = buildData({ status: { features: [] }, requirements: 'r', plan: 'p' });
  const once = renderPage(page, data);
  assert.equal(renderPage(once, data), once);
});

test('embedded JSON cannot close the script element', () => {
  const data = buildData({ status: {}, requirements: '</script><b>x</b>', plan: '' });
  assert.equal(data.includes('</script'), false);
  const parsed = JSON.parse(renderPage(page, data).match(/type="application\/json">([\s\S]*?)<\/script>/)[1]);
  assert.equal(parsed.requirements, '</script><b>x</b>');
});

test('a page without the data block is refused', () => {
  assert.throws(() => renderPage('<html></html>', '{}'), /not found/);
});
