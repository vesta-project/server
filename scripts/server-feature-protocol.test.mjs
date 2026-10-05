import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import {
  MAX_BYTES, PROTOCOL, ProtocolError, decodeRequest, decodeManifest, decodeFailure,
  encodeRequest, encodeManifest, encodeFailure, negotiate, checkResponse, supports,
} from '../protocol/peer/server-feature-peer.mjs';

const fixtures = new URL('../protocol/v1/fixtures/', import.meta.url);
const corpus = JSON.parse(readFileSync(new URL('corpus.json', fixtures), 'utf8'));
const codec = { request: [decodeRequest, encodeRequest], manifest: [decodeManifest, encodeManifest], failure: [decodeFailure, encodeFailure] };
function bytes(row) {
  const input = row.hex ? Buffer.from(row.hex, 'hex') : Buffer.from(row.json, 'utf8');
  return row.padTo ? Buffer.concat([Buffer.alloc(row.padTo - input.length, 0x20), input]) : input;
}
function rejects(code, fn) {
  assert.throws(fn, (error) => {
    assert.ok(error instanceof ProtocolError);
    assert.equal(error.code, code);
    assert.equal(error.message, `Server feature protocol failed: ${code}`);
    assert.equal(error.cause, undefined);
    return true;
  });
}
for (const row of corpus) {
  test(`shared wire corpus: ${row.name}`, () => {
    const [decode, encode] = codec[row.kind];
    if (row.error) rejects(row.error, () => decode(bytes(row)));
    else {
      const value = decode(bytes(row));
      assert.equal(encode(value).toString('utf8'), row.canonical);
      assert.deepEqual(decode(encode(value)), value);
      if (row.negotiation) assert.equal(negotiate(value), row.negotiation);
      if (row.negotiationError) rejects(row.negotiationError, () => negotiate(value));
    }
  });
}

for (const kind of Object.keys(codec)) {
  test(`independent CLI consumes and reproduces canonical ${kind} bytes`, () => {
    const input = readFileSync(new URL(`${kind}.json`, fixtures));
    const result = spawnSync(process.execPath, [fileURLToPath(new URL('../protocol/peer/server-feature-peer.mjs', import.meta.url)), kind],
      { input, timeout: 10_000, maxBuffer: MAX_BYTES * 2, shell: false });
    assert.ifError(result.error); assert.equal(result.status, 0); assert.deepEqual(result.stdout, input); assert.equal(result.stderr.length, 0);
  });
}

test('CLI bounds streamed input and prints only a fixed failure envelope', () => {
  const result = spawnSync(process.execPath, [fileURLToPath(new URL('../protocol/peer/server-feature-peer.mjs', import.meta.url)), 'manifest'],
    { input: Buffer.alloc(MAX_BYTES + 1, 0x78), timeout: 10_000, maxBuffer: MAX_BYTES * 2, shell: false });
  assert.equal(result.status, 1); assert.equal(result.stderr.length, 0);
  assert.deepEqual(decodeFailure(result.stdout), { error: 'message_too_large' });
});

test('unknown exact feature IDs and familiar permission strings enable nothing', () => {
  const input = { version: 1, serverId: '12345678-1234-5678-9abc-123456789abc',
    features: ['game.start.extra', 'lifecycle.start', 'security.manage', 'constructor'].map((id) => ({ id, support: 'supported', reason: null })) };
  const manifest = decodeManifest(encodeManifest(input));
  assert.equal(supports(manifest, 'game.start'), false);
  rejects('invalid_message', () => supports(manifest, 'security.manage'));
  assert.equal(Object.isFrozen(manifest), true); assert.equal(Object.isFrozen(manifest.features), true);
  assert.equal(Object.isFrozen(manifest.features[0]), true);
});

test('response context and offered versions are checked without creating identity', () => {
  const manifest = decodeManifest(readFileSync(new URL('manifest.json', fixtures)));
  checkResponse({ versions: [2, 1] }, manifest.serverId, manifest);
  rejects('unexpected_response', () => checkResponse({ versions: [2] }, manifest.serverId, manifest));
  rejects('unexpected_response', () => checkResponse({ versions: [1] }, 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', manifest));
  rejects('invalid_message', () => checkResponse({ versions: [1] }, 'secret', manifest));
});

test('caller-created encoder values are validated before unrestricted serialization', () => {
  rejects('invalid_message', () => encodeRequest({ versions: Array(9).fill(1) }));
  rejects('invalid_message', () => encodeRequest({ versions: [1], permission: 'owner' }));
  rejects('invalid_message', () => encodeRequest({ versions: [NaN] }));
  rejects('invalid_message', () => encodeRequest(null));
  rejects('invalid_message', () => encodeManifest({ version: 1, serverId: 'secret', features: [] }));
  rejects('invalid_message', () => encodeManifest({ version: 1, serverId: '12345678-1234-5678-9abc-123456789abc', features: Array(65).fill(null) }));
  rejects('invalid_message', () => encodeFailure({ error: 'secret' }));
  rejects('invalid_message', () => encodeFailure({ error: 'invalid_message', credentials: 'secret' }));
  rejects('invalid_message', () => decodeRequest(null));
});

test('sparse request arrays cannot produce invalid wire messages', () => {
  rejects('invalid_message', () => encodeRequest({ versions: Array(1) }));
});

test('sparse manifest arrays cannot produce invalid wire messages', () => {
  rejects('invalid_message', () => encodeManifest({ version: 1, serverId: '12345678-1234-5678-9abc-123456789abc', features: Array(1) }));
});

test('negotiation validates holes even when the offer includes v1', () => {
  rejects('invalid_message', () => negotiate({ versions: [1,,] }));
});

test('support helper cannot accept conflicting duplicate declarations or invalid caller values', () => {
  const manifest = { version: 1, serverId: '12345678-1234-5678-9abc-123456789abc', features: [
    { id: 'game.start', support: 'unsupported', reason: 'not_configured' },
    { id: 'game.start', support: 'supported', reason: null },
  ] };
  rejects('invalid_message', () => supports(manifest, 'game.start'));
  rejects('invalid_message', () => supports({ ...manifest, features: Array(1) }, 'game.start'));
  rejects('invalid_message', () => supports(null, 'game.start'));
});

test('schema structure agrees with draft-v1 fields and critical semantic assertions (not a schema engine)', () => {
  const load = (name) => JSON.parse(readFileSync(new URL(`../protocol/v1/${name}.schema.json`, import.meta.url), 'utf8'));
  const request = load('discovery-request'), manifest = load('feature-manifest'), failure = load('error');
  for (const schema of [request, manifest, failure]) {
    assert.equal(schema.$schema, 'https://json-schema.org/draft/2020-12/schema');
    assert.equal(schema.additionalProperties, false);
    assert.equal(schema.properties.protocol.const, PROTOCOL);
    assert.deepEqual([...schema.required].sort(), Object.keys(schema.properties).sort());
  }
  assert.equal(request.properties.versions.maxItems, 8); assert.equal(request.properties.versions.minItems, 1);
  assert.equal(request.properties.versions.uniqueItems, true); assert.equal(request.properties.versions.items.maximum, 65535);
  assert.equal(manifest.properties.version.const, 1); assert.equal(manifest.properties.features.maxItems, 64);
  const f = manifest.$defs.feature;
  assert.equal(f.additionalProperties, false); assert.deepEqual(f.required, ['id', 'support', 'reason']);
  assert.equal(f.properties.id.maxLength, 64); assert.equal(f.else.properties.reason.maxLength, 64);
  assert.equal(f.then.properties.reason.type, 'null'); assert.equal(f.else.properties.reason.type, 'string');
  assert.equal(new RegExp(f.properties.id.pattern).test('game.start\n'), false);
  assert.equal(new RegExp(manifest.properties.server_id.pattern).test('12345678-1234-5678-9abc-123456789abc\n'), false);
  for (const row of corpus.filter((r) => !r.error && r.kind === 'manifest')) {
    const value = decodeManifest(bytes(row));
    assert.equal(new RegExp(manifest.properties.server_id.pattern).test(value.serverId), true);
    for (const entry of value.features) {
      assert.equal(new RegExp(f.properties.id.pattern).test(entry.id), true);
      if (entry.support === 'unsupported') assert.equal(new RegExp(f.else.properties.reason.pattern).test(entry.reason), true);
    }
  }
  assert.deepEqual(failure.properties.error.enum, ['invalid_message', 'message_too_large', 'unsupported_protocol', 'unexpected_response']);
});
