import https from 'node:https';
import net from 'node:net';
import tls from 'node:tls';
import { createHash, timingSafeEqual } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const BODY_MAX = 2048;
const PROBE_BODY_MAX = 4096;
const PROBE_TOTAL_MS = 4000;
const REQUEST_TIMEOUT_MS = 8000;
const MAX_ACTIVE = 4;
const COOLDOWN_MS = 30_000;
const PEER_CACHE_MAX = 1024;
const peerCooldowns = new Map();
let activeProbes = 0;

const deniedV4 = [
  ['0.0.0.0', 8], ['10.0.0.0', 8], ['100.64.0.0', 10], ['127.0.0.0', 8],
  ['169.254.0.0', 16], ['172.16.0.0', 12], ['192.0.0.0', 24], ['192.0.2.0', 24],
  ['192.88.99.0', 24], ['192.168.0.0', 16], ['198.18.0.0', 15], ['198.51.100.0', 24],
  ['203.0.113.0', 24], ['224.0.0.0', 4], ['240.0.0.0', 4],
].map(([address, bits]) => [ipv4ToBigInt(address), bits]);
const globalV6 = [ipv6ToBigInt('2000::'), 3];
const deniedV6 = [
  ['2001::', 23], // IETF protocol assignments and special-purpose space
  ['2001:db8::', 32], // documentation
  ['2002::', 16], // 6to4
  ['3fff::', 20], // documentation
].map(([address, bits]) => [ipv6ToBigInt(address), bits]);

export function createProbeServer({ key, cert, check = productionCheck }) {
  if (!key || !cert) throw new TypeError('TLS key and certificate are required');
  const server = https.createServer({ key, cert, minVersion: 'TLSv1.2', handshakeTimeout: REQUEST_TIMEOUT_MS }, (request, response) => {
    void handleRequest(request, response, check);
  });
  server.requestTimeout = REQUEST_TIMEOUT_MS;
  server.headersTimeout = REQUEST_TIMEOUT_MS;
  server.keepAliveTimeout = 1000;
  server.maxHeadersCount = 32;
  server.maxConnections = 64;
  server.setTimeout(REQUEST_TIMEOUT_MS, socket => socket.destroy());
  return server;
}

export async function productionCheck({ peerAddress, port, installation_id, tls_sha256 }, connect = tls.connect) {
  if (!Number.isInteger(port) || port < 1 || port > 65535 ||
      typeof installation_id !== 'string' || !/^[0-9a-f]{32}$/.test(installation_id) ||
      typeof tls_sha256 !== 'string' || !/^[0-9a-f]{64}$/.test(tls_sha256)) return false;
  let target = normalizeAddress(peerAddress);
  if (!target || !isGlobalAddress(target)) return false;
  const pin = Buffer.from(tls_sha256, 'hex');
  if (pin.length !== 32) return false;

  return new Promise(resolve => {
    let settled = false;
    let responseBytes = Buffer.alloc(0);
    let parsed = false;
    // Public target validation remains above the transport seam used by local TLS tests.
    const socket = connect({ host: target, port, rejectUnauthorized: false, minVersion: 'TLSv1.2' });
    const deadline = setTimeout(() => finish(false), PROBE_TOTAL_MS);
    socket.setNoDelay(true);

    function finish(reachable) {
      if (settled) return;
      settled = true;
      clearTimeout(deadline);
      socket.destroy();
      resolve(reachable);
    }

    socket.once('error', () => finish(false));
    socket.once('timeout', () => finish(false));
    socket.setTimeout(PROBE_TOTAL_MS);
    socket.once('secureConnect', () => {
      try {
        const raw = socket.getPeerCertificate(true)?.raw;
        if (!raw || !timingSafeEqual(createHash('sha256').update(raw).digest(), pin)) {
          finish(false);
          return;
        }
        const host = target.includes(':') ? `[${target}]` : target;
        socket.write(`GET /api/v3/setup HTTP/1.1\r\nHost: ${host}:${port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n`);
      } catch {
        finish(false);
      }
    });
    socket.on('data', chunk => {
      if (settled) return;
      responseBytes = Buffer.concat([responseBytes, chunk]);
      if (responseBytes.length > 12 * 1024) {
        finish(false);
        return;
      }
      const result = parseHttpResponse(responseBytes);
      if (result === null) return;
      parsed = true;
      if (!result) {
        finish(false);
        return;
      }
      try {
        const body = JSON.parse(result.body.toString('utf8'));
        finish(result.status === 200 && body && typeof body === 'object' &&
          body.installation_id === installation_id && body.status === 'awaiting_app_onboarding');
      } catch {
        finish(false);
      }
    });
    socket.once('end', () => { if (!parsed) finish(false); });
  });
}

export async function handleRequest(request, response, check = productionCheck) {
  const route = request.url;
  if (route !== '/v1/check') {
    drain(request);
    sendError(response, 404, 'not_found');
    return;
  }
  if (request.method !== 'POST') {
    drain(request);
    sendError(response, 405, 'method_not_allowed');
    return;
  }
  const peerAddress = normalizeAddress(request.socket.remoteAddress ?? '');
  if (!peerAddress || !isGlobalAddress(peerAddress)) {
    drain(request);
    sendError(response, 403, 'unavailable');
    return;
  }

  let body;
  try {
    body = await readRequestBody(request);
  } catch (error) {
    sendError(response, error?.code === 'BODY_TOO_LARGE' ? 413 : 400, 'invalid_request');
    return;
  }
  let input;
  try {
    input = JSON.parse(body.toString('utf8'));
  } catch {
    sendError(response, 400, 'invalid_request');
    return;
  }
  if (!validInput(input)) {
    sendError(response, 400, 'invalid_request');
    return;
  }
  if (isCoolingDown(peerAddress)) {
    drain(request);
    sendError(response, 429, 'unavailable');
    return;
  }
  if (activeProbes >= MAX_ACTIVE) {
    sendError(response, 503, 'unavailable');
    return;
  }

  markPeer(peerAddress);
  activeProbes++;
  let reachable = false;
  try {
    reachable = Boolean(await withDeadline(Promise.resolve(check({
      peerAddress,
      port: input.port,
      installation_id: input.installation_id,
      tls_sha256: input.tls_sha256,
    })), PROBE_TOTAL_MS));
  } catch {
    reachable = false;
  } finally {
    activeProbes--;
  }
  sendJson(response, 200, {
    schema: 1,
    port: input.port,
    installation_id: input.installation_id,
    tls_sha256: input.tls_sha256,
    nonce: input.nonce,
    reachable,
  });
}

function validInput(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const keys = Object.keys(value).sort();
  if (keys.join(',') !== 'installation_id,nonce,port,schema,tls_sha256') return false;
  return value.schema === 1 && Number.isInteger(value.port) && value.port >= 1 && value.port <= 65535 &&
    typeof value.installation_id === 'string' && /^[0-9a-f]{32}$/.test(value.installation_id) &&
    typeof value.tls_sha256 === 'string' && /^[0-9a-f]{64}$/.test(value.tls_sha256) &&
    typeof value.nonce === 'string' && /^[0-9a-f]{32}$/.test(value.nonce);
}

function readRequestBody(request) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    let rejected = false;
    const deadline = setTimeout(() => {
      rejected = true;
      reject(new Error('request deadline'));
      request.destroy();
    }, REQUEST_TIMEOUT_MS);
    const finish = () => clearTimeout(deadline);
    request.once('close', finish);
    request.once('end', finish);
    request.on('data', chunk => {
      if (rejected) return;
      size += chunk.length;
      if (size > BODY_MAX) {
        rejected = true;
        const error = new Error('body too large');
        error.code = 'BODY_TOO_LARGE';
        reject(error);
        request.resume();
        return;
      }
      chunks.push(chunk);
    });
    request.once('end', () => { if (!rejected) resolve(Buffer.concat(chunks, size)); });
    request.once('error', reject);
    request.once('aborted', () => reject(new Error('request aborted')));
  });
}

function sendJson(response, status, value) {
  if (response.destroyed || response.writableEnded) return;
  const bytes = Buffer.from(JSON.stringify(value));
  response.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': bytes.length,
    'cache-control': 'no-store',
    connection: 'close',
  });
  response.end(bytes);
}

function sendError(response, status, code) {
  sendJson(response, status, { error: code });
}

function drain(request) {
  request.resume();
}

function isCoolingDown(peer) {
  const now = Date.now();
  for (const [address, until] of peerCooldowns) if (until <= now) peerCooldowns.delete(address);
  const until = peerCooldowns.get(peer);
  if (until && until > now) return true;
  return false;
}

function markPeer(peer) {
  if (peerCooldowns.has(peer)) peerCooldowns.delete(peer);
  while (peerCooldowns.size >= PEER_CACHE_MAX) peerCooldowns.delete(peerCooldowns.keys().next().value);
  peerCooldowns.set(peer, Date.now() + COOLDOWN_MS);
}

function withDeadline(promise, ms) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('deadline')), ms);
    promise.then(resolve, reject).finally(() => clearTimeout(timer));
  });
}

function parseHttpResponse(bytes) {
  const separator = bytes.indexOf('\r\n\r\n');
  if (separator < 0) return bytes.length > 8192 ? false : null;
  if (separator > 8192) return false;
  const headerText = bytes.subarray(0, separator).toString('latin1');
  const lines = headerText.split('\r\n');
  const statusMatch = /^HTTP\/1\.[01] ([0-9]{3})(?: .*)?$/.exec(lines.shift() ?? '');
  if (!statusMatch) return false;
  const headers = new Map();
  for (const line of lines) {
    const colon = line.indexOf(':');
    if (colon <= 0) return false;
    const name = line.slice(0, colon).trim().toLowerCase();
    const value = line.slice(colon + 1).trim();
    if (headers.has(name)) return false;
    headers.set(name, value);
  }
  const bodyStart = separator + 4;
  const transferEncoding = headers.get('transfer-encoding')?.toLowerCase();
  if (transferEncoding) {
    if (transferEncoding !== 'chunked' || headers.has('content-length')) return false;
    const decoded = decodeChunked(bytes.subarray(bodyStart));
    if (decoded === undefined) return null;
    if (decoded === false || decoded.length > PROBE_BODY_MAX) return false;
    return { status: Number(statusMatch[1]), body: decoded };
  }
  const lengthText = headers.get('content-length');
  if (!lengthText || !/^(0|[1-9][0-9]*)$/.test(lengthText)) return false;
  const length = Number(lengthText);
  if (!Number.isSafeInteger(length) || length > PROBE_BODY_MAX) return false;
  if (bytes.length - bodyStart < length) return null;
  return { status: Number(statusMatch[1]), body: bytes.subarray(bodyStart, bodyStart + length) };
}

function decodeChunked(bytes) {
  let offset = 0;
  const chunks = [];
  let total = 0;
  while (true) {
    const lineEnd = bytes.indexOf('\r\n', offset);
    if (lineEnd < 0) return undefined;
    const line = bytes.subarray(offset, lineEnd).toString('ascii');
    const sizeText = line.split(';', 1)[0];
    if (!/^[0-9a-fA-F]{1,8}$/.test(sizeText)) return false;
    const size = Number.parseInt(sizeText, 16);
    offset = lineEnd + 2;
    if (size === 0) {
      const end = bytes.indexOf('\r\n\r\n', offset);
      if (bytes.subarray(offset, offset + 2).toString() === '\r\n') return Buffer.concat(chunks, total);
      if (end < 0) return undefined;
      return Buffer.concat(chunks, total);
    }
    total += size;
    if (total > PROBE_BODY_MAX) return false;
    if (bytes.length < offset + size + 2) return undefined;
    chunks.push(bytes.subarray(offset, offset + size));
    offset += size;
    if (bytes.subarray(offset, offset + 2).toString() !== '\r\n') return false;
    offset += 2;
  }
}

function normalizeAddress(address) {
  if (typeof address !== 'string' || address.includes('%')) return null;
  if (net.isIP(address) === 4) return address;
  if (net.isIP(address) !== 6) return null;
  const value = ipv6ToBigInt(address);
  if (value === null) return null;
  if ((value >> 32n) === 0xffffn) return bigIntToIpv4(value & 0xffffffffn);
  return bigIntToIpv6(value);
}

function isGlobalAddress(address) {
  if (net.isIP(address) === 4) {
    const value = ipv4ToBigInt(address);
    return !deniedV4.some(([network, bits]) => inCidr(value, network, bits, 32));
  }
  if (net.isIP(address) !== 6) return false;
  const value = ipv6ToBigInt(address);
  return value !== null && inCidr(value, globalV6[0], globalV6[1], 128) &&
    !deniedV6.some(([network, bits]) => inCidr(value, network, bits, 128));
}

function inCidr(value, network, bits, width) {
  const shift = BigInt(width - bits);
  return (value >> shift) === (network >> shift);
}

function ipv4ToBigInt(value) {
  return value.split('.').reduce((result, part) => (result << 8n) | BigInt(Number(part)), 0n);
}

function ipv6ToBigInt(value) {
  let source = value.toLowerCase();
  if (source.includes('.')) {
    const lastColon = source.lastIndexOf(':');
    const v4 = source.slice(lastColon + 1);
    if (net.isIP(v4) !== 4) return null;
    const n = ipv4ToBigInt(v4);
    source = `${source.slice(0, lastColon + 1)}${(n >> 16n).toString(16)}:${(n & 0xffffn).toString(16)}`;
  }
  const halves = source.split('::');
  if (halves.length > 2) return null;
  const left = halves[0] ? halves[0].split(':') : [];
  const right = halves.length === 2 && halves[1] ? halves[1].split(':') : [];
  const groups = halves.length === 2 ? [...left, ...Array(8 - left.length - right.length).fill('0'), ...right] : left;
  if (groups.length !== 8 || groups.some(group => !/^[0-9a-f]{1,4}$/.test(group))) return null;
  return groups.reduce((result, group) => (result << 16n) | BigInt(`0x${group}`), 0n);
}

function bigIntToIpv4(value) {
  return [24n, 16n, 8n, 0n].map(shift => Number((value >> shift) & 255n)).join('.');
}

function bigIntToIpv6(value) {
  const groups = [];
  for (let shift = 112n; shift >= 0n; shift -= 16n) groups.push(Number((value >> shift) & 0xffffn).toString(16));
  return groups.join(':');
}

async function main() {
  const certFile = process.env.TLS_CERT_FILE;
  const keyFile = process.env.TLS_KEY_FILE;
  if (!certFile || !keyFile) throw new Error('TLS_CERT_FILE and TLS_KEY_FILE must be set');
  const [cert, key] = await Promise.all([readFile(certFile), readFile(keyFile)]);
  const port = Number(process.env.PORT ?? '9443');
  const bind = process.env.BIND ?? '0.0.0.0';
  if (!Number.isInteger(port) || port < 1 || port > 65535 || net.isIP(bind) === 0) {
    throw new Error('PORT or BIND is invalid');
  }
  const server = createProbeServer({ key, cert });
  server.listen(port, bind, () => process.stdout.write(`Reachability probe listening on ${bind}:${port}\n`));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch(() => {
    process.stderr.write('Reachability probe could not start.\n');
    process.exitCode = 1;
  });
}
