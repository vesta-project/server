import assert from 'node:assert/strict';
import { PassThrough } from 'node:stream';
import test from 'node:test';
import tls from 'node:tls';
import { readFile } from 'node:fs/promises';
import { createHash, X509Certificate } from 'node:crypto';
import { once } from 'node:events';
import { handleRequest, productionCheck } from './server.mjs';

const input = { schema: 1, port: 8443, installation_id: 'a'.repeat(32), tls_sha256: 'b'.repeat(64), nonce: 'c'.repeat(32) };

test('a real TLS connection proves the pinned certificate and installation identity', async t => {
  const cert = await readFile(new URL('../../crates/vesta-transport/tests/fixtures/test-only-server.pem', import.meta.url));
  const key = await readFile(new URL('../../crates/vesta-transport/tests/fixtures/test-only-server-key.pem', import.meta.url));
  const pin = createHash('sha256').update(new X509Certificate(cert).raw).digest('hex');
  let requests = 0;
  const server = tls.createServer({ cert, key }, socket => {
    socket.on('error', () => {});
    socket.once('data', bytes => {
      requests++;
      assert.ok(bytes.toString().startsWith('GET /api/v3/setup HTTP/1.1\r\n'));
      const body = JSON.stringify({ installation_id: input.installation_id, status: 'awaiting_app_onboarding' });
      socket.end(`HTTP/1.1 200 OK\r\nContent-Length: ${Buffer.byteLength(body)}\r\nConnection: close\r\n\r\n${body}`);
    });
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise(resolve => server.close(resolve)));
  const connect = options => {
    assert.equal(options.host, '8.8.8.8');
    assert.equal(options.port, input.port);
    return tls.connect({ ...options, host: '127.0.0.1', port: server.address().port });
  };
  const expected = { ...input, peerAddress: '8.8.8.8', tls_sha256: pin };
  assert.equal(await productionCheck(expected, connect), true);
  assert.equal(requests, 1);
  assert.equal(await productionCheck({ ...expected, tls_sha256: '0'.repeat(64) }, connect), false);
  assert.equal(requests, 1, 'wrong pins must be rejected before the HTTP request');
  assert.equal(await productionCheck({ ...expected, installation_id: 'd'.repeat(32) }, connect), false);
});
async function request({ peer = '8.8.8.10', body = input, check = async () => true, method = 'POST', url = '/v1/check', headers = {} } = {}) {
  const stream = new PassThrough();
  Object.assign(stream, { method, url, headers, socket: { remoteAddress: peer } });
  const result = {};
  const response = { destroyed: false, writableEnded: false,
    writeHead(status, values) { result.status = status; result.headers = values; },
    end(bytes) { result.body = JSON.parse(bytes); this.writableEnded = true; } };
  const done = handleRequest(stream, response, check);
  stream.end(typeof body === 'string' ? body : JSON.stringify(body));
  await done;
  stream.destroy();
  return result;
}

test('probe targets only the public TCP peer and binds the result to the requested identity', async () => {
  let target;
  const result = await request({ peer: '::ffff:8.8.8.11', headers: { 'x-forwarded-for': '127.0.0.1' }, check: async value => { target = value; return true; } });
  assert.equal(result.status, 200);
  assert.deepEqual(result.body, { ...input, reachable: true });
  assert.deepEqual(target, { peerAddress: '8.8.8.11', port: input.port, installation_id: input.installation_id, tls_sha256: input.tls_sha256 });
  assert.equal(result.headers['cache-control'], 'no-store');
  assert.equal((await request({ peer: '8.8.8.12', check: async () => false })).body.reachable, false);
  assert.equal((await request({ peer: '8.8.8.13', check: async () => { throw new Error('private detail'); } })).body.reachable, false);
});

test('private, reserved and transition peers cannot trigger outbound probes', async () => {
  const peers = ['127.0.0.1', '10.0.0.1', '172.16.1.1', '192.168.1.1', '100.64.0.1', '169.254.1.1', '192.0.2.1', '203.0.113.1', '224.0.0.1', '::1', 'fc00::1', 'fe80::1', '2001:db8::1', '2002:0808:0808::1', '::ffff:127.0.0.1', '::ffff:7f00:1'];
  for (const peer of peers) {
    let called = false;
    const result = await request({ peer, headers: { 'x-forwarded-for': '8.8.8.8' }, check: async () => { called = true; return true; } });
    assert.equal(result.status, 403, peer);
    assert.equal(called, false, peer);
    assert.equal(await productionCheck({ ...input, peerAddress: peer }), false, peer);
  }
});

test('strict bounded requests reject arbitrary targets, invalid ports and unsupported routes', async () => {
  for (const body of [{ ...input, target: 'https://127.0.0.1' }, { ...input, port: 0 }, { ...input, port: 65536 }, { ...input, schema: 2 }, { ...input, nonce: 'invalid' }, 'broken json', 'x'.repeat(2049)]) {
    let called = false;
    const result = await request({ body, check: async () => { called = true; return true; } });
    assert.ok([400, 413].includes(result.status));
    assert.equal(called, false);
  }
  assert.equal((await request({ method: 'GET' })).status, 405);
  assert.equal((await request({ url: '/other' })).status, 404);
  assert.equal((await request({ url: '/v1/check?target=127.0.0.1' })).status, 404);
});

test('the stateless checker bounds concurrency and applies per-peer cooldown', async () => {
  const release = [];
  const pending = Array.from({ length: 4 }, (_, index) => request({ peer: `8.8.4.${index + 10}`, check: () => new Promise(resolve => release.push(resolve)) }));
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(release.length, 4);
  assert.equal((await request({ peer: '8.8.4.20' })).status, 503);
  assert.equal((await request({ peer: '8.8.4.10' })).status, 429);
  for (const resolve of release) resolve(false);
  const results = await Promise.all(pending);
  assert.ok(results.every(result => result.status === 200 && result.body.reachable === false));
});
