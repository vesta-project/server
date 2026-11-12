import { pathToFileURL } from 'node:url';

// Independent draft-v1 implementation. No Java, network, permissions, or downloaded policy.
export const PROTOCOL = 'vesta.server-capabilities';
export const MAX_BYTES = 16_384;
export const KNOWN_FEATURES = Object.freeze([
  'game.start', 'game.stop', 'game.force_kill', 'tasks.replay', 'game.readiness', 'supervisor.continuity',
]);
const errors = new Set(['invalid_message', 'message_too_large', 'unsupported_protocol', 'unexpected_response']);
const idPattern = /^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*(?![\s\S])/;
const reasonPattern = /^[a-z][a-z0-9_]*(?![\s\S])/;
const uuidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}(?![\s\S])/;

export class ProtocolError extends Error {
  constructor(code) {
    super(`Server feature protocol failed: ${errors.has(code) ? code : 'invalid_message'}`);
    this.code = errors.has(code) ? code : 'invalid_message';
  }
}
const invalid = () => { throw new ProtocolError('invalid_message'); };
class NumberToken { constructor(text) { this.text = text; } }

// JSON.parse alone loses duplicate keys and integer lexemes. Only individual JSON strings use it.
function parse(bytes) {
  if (!(bytes instanceof Uint8Array)) invalid();
  if (bytes.byteLength > MAX_BYTES) throw new ProtocolError('message_too_large');
  let text;
  try { text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes); }
  catch { invalid(); }
  if (text.charCodeAt(0) === 0xfeff) invalid();
  let at = 0;
  const space = () => { while (at < text.length && /[ \r\n\t]/.test(text[at])) at++; };
  const string = () => {
    if (text[at] !== '"') invalid();
    const start = at++;
    while (at < text.length) {
      if (text[at] === '\\') { at += 2; continue; }
      if (text[at++] === '"') {
        try { return JSON.parse(text.slice(start, at)); } catch { invalid(); }
      }
    }
    invalid();
  };
  const value = (depth) => {
    if (depth > 4) invalid();
    space();
    if (text[at] === '"') return string();
    if (text[at] === '{') {
      at++; space();
      const out = Object.create(null), seen = new Set();
      if (text[at] === '}') { at++; return out; }
      for (;;) {
        const key = string();
        if (seen.has(key)) invalid();
        seen.add(key); space();
        if (text[at++] !== ':') invalid();
        out[key] = value(depth + 1); space();
        const delimiter = text[at++];
        if (delimiter === '}') return out;
        if (delimiter !== ',') invalid();
        space();
      }
    }
    if (text[at] === '[') {
      at++; space();
      const out = [];
      if (text[at] === ']') { at++; return out; }
      for (;;) {
        out.push(value(depth + 1)); space();
        const delimiter = text[at++];
        if (delimiter === ']') return out;
        if (delimiter !== ',') invalid();
      }
    }
    for (const [literal, result] of [['null', null], ['true', true], ['false', false]]) {
      if (text.startsWith(literal, at)) { at += literal.length; return result; }
    }
    const match = /^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/.exec(text.slice(at));
    if (!match) invalid();
    at += match[0].length;
    return new NumberToken(match[0]);
  };
  const result = value(1); space();
  if (at !== text.length) invalid();
  return result;
}

function keys(value, expected) {
  if (value === null || typeof value !== 'object' || Array.isArray(value) || value instanceof NumberToken) invalid();
  const actual = Object.keys(value);
  if (actual.length !== expected.length || expected.some((key) => !Object.hasOwn(value, key))) invalid();
}
function header(value, fields) {
  keys(value, fields);
  if (value.protocol !== PROTOCOL) invalid();
}
function integer(value) {
  if (!(value instanceof NumberToken) || !/^[1-9][0-9]{0,4}(?![\s\S])/.test(value.text)) invalid();
  const result = Number(value.text);
  if (result > 65535) invalid();
  return result;
}
function token(value, pattern) {
  return typeof value === 'string' && value.length <= 64 && pattern.test(value);
}
const freeze = (value) => Object.freeze(value);

export function decodeRequest(bytes) {
  const root = parse(bytes); header(root, ['protocol', 'versions']);
  if (!Array.isArray(root.versions) || root.versions.length < 1 || root.versions.length > 8) invalid();
  const versions = root.versions.map(integer);
  if (new Set(versions).size !== versions.length) invalid();
  return freeze({ versions: freeze(versions.sort((a, b) => b - a)) });
}
export function negotiate(request) {
  if (!requestValue(request).versions.includes(1)) throw new ProtocolError('unsupported_protocol');
  return 1;
}
export function decodeManifest(bytes) {
  const root = parse(bytes); header(root, ['protocol', 'version', 'server_id', 'features']);
  if (!Array.isArray(root.features) || root.features.length > 64) invalid();
  const features = root.features.map((entry) => {
    keys(entry, ['id', 'support', 'reason']);
    if (!token(entry.id, idPattern) || !['supported', 'unsupported'].includes(entry.support)) invalid();
    if (entry.support === 'supported' ? entry.reason !== null : !token(entry.reason, reasonPattern)) invalid();
    return freeze({ id: entry.id, support: entry.support, reason: entry.reason });
  });
  const version = integer(root.version);
  if (version !== 1) throw new ProtocolError('unsupported_protocol');
  if (typeof root.server_id !== 'string' || !uuidPattern.test(root.server_id)) invalid();
  if (new Set(features.map((f) => f.id)).size !== features.length) invalid();
  features.sort((a, b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
  return freeze({ version, serverId: root.server_id, features: freeze(features) });
}
export function decodeFailure(bytes) {
  const root = parse(bytes); header(root, ['protocol', 'error']);
  if (!errors.has(root.error)) invalid();
  return freeze({ error: root.error });
}
export function supports(manifest, knownId) {
  if (!KNOWN_FEATURES.includes(knownId)) invalid();
  return manifestValue(manifest).features.some((f) => f.id === knownId && f.support === 'supported');
}
export function checkResponse(request, expectedServerId, manifest) {
  const offered = decodeRequest(encodeRequest(request));
  const received = decodeManifest(encodeManifest(manifest));
  if (typeof expectedServerId !== 'string' || !uuidPattern.test(expectedServerId)) invalid();
  if (!offered.versions.includes(received.version) || expectedServerId !== received.serverId) {
    throw new ProtocolError('unexpected_response');
  }
}

function encodeRaw(value) {
  try {
    const bytes = Buffer.from(JSON.stringify(value), 'utf8');
    if (bytes.length > MAX_BYTES) throw new ProtocolError('message_too_large');
    return bytes;
  } catch (error) {
    if (error instanceof ProtocolError) throw error;
    invalid();
  }
}
function requestValue(request) {
  keys(request, ['versions']);
  if (!Array.isArray(request.versions) || request.versions.length < 1 || request.versions.length > 8) invalid();
  const versions = Array.from(request.versions, (version) => {
    if (!Number.isInteger(version) || version < 1 || version > 65535) invalid();
    return version;
  });
  if (new Set(versions).size !== versions.length) invalid();
  return { versions: versions.sort((a, b) => b - a) };
}
function manifestValue(manifest) {
  keys(manifest, ['version', 'serverId', 'features']);
  if (!Array.isArray(manifest.features) || manifest.features.length > 64) invalid();
  const features = Array.from(manifest.features, (entry) => {
    keys(entry, ['id', 'support', 'reason']);
    if (!token(entry.id, idPattern) || !['supported', 'unsupported'].includes(entry.support)) invalid();
    if (entry.support === 'supported' ? entry.reason !== null : !token(entry.reason, reasonPattern)) invalid();
    return { id: entry.id, support: entry.support, reason: entry.reason };
  });
  if (!Number.isInteger(manifest.version) || manifest.version < 1 || manifest.version > 65535) invalid();
  if (manifest.version !== 1) throw new ProtocolError('unsupported_protocol');
  if (typeof manifest.serverId !== 'string' || !uuidPattern.test(manifest.serverId)) invalid();
  if (new Set(features.map((f) => f.id)).size !== features.length) invalid();
  features.sort((a, b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
  return { version: 1, serverId: manifest.serverId, features };
}
export function encodeRequest(request) {
  const valid = requestValue(request);
  return encodeRaw({ protocol: PROTOCOL, versions: valid.versions });
}
export function encodeManifest(manifest) {
  const valid = manifestValue(manifest);
  return encodeRaw({ protocol: PROTOCOL, version: valid.version, server_id: valid.serverId, features: valid.features });
}
export function encodeFailure(failure) {
  keys(failure, ['error']);
  if (!errors.has(failure.error)) invalid();
  return encodeRaw({ protocol: PROTOCOL, error: failure.error });
}

// CLI peer: canonicalize one bounded stdin message. Failure stdout never includes the rejected input.
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const mode = process.argv[2];
  const codecs = { request: [decodeRequest, encodeRequest], manifest: [decodeManifest, encodeManifest], failure: [decodeFailure, encodeFailure] };
  let chunks = [], size = 0, done = false;
  const fail = (code) => {
    if (done) return;
    done = true; chunks = [];
    process.stdout.write(encodeFailure({ error: code })); process.exitCode = 1;
  };
  process.stdin.on('data', (chunk) => {
    size += chunk.length;
    if (size > MAX_BYTES) { fail('message_too_large'); process.stdin.destroy(); }
    else if (!done) chunks.push(chunk);
  });
  process.stdin.on('error', () => fail('invalid_message'));
  process.stdin.on('end', () => {
    if (done) return;
    try {
      if (!Object.hasOwn(codecs, mode)) invalid();
      const [decode, encode] = codecs[mode];
      process.stdout.write(encode(decode(Buffer.concat(chunks, size)))); done = true;
    } catch (error) { fail(error instanceof ProtocolError ? error.code : 'invalid_message'); }
  });
}
