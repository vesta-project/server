import http from 'node:http';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
function option(name) {
  const index = args.indexOf(name);
  if (index < 0) return undefined;
  if (!args[index + 1] || args[index + 1].startsWith('--')) throw new Error(`${name} requires a value`);
  return args[index + 1];
}

let statesDir;
let port;
try {
  const rawDir = option('--states');
  if (!rawDir) throw new Error('Usage: node server.mjs --states ABS_EXPORT_DIR [--port 4318]');
  if (!path.isAbsolute(rawDir)) throw new Error('--states must be an absolute export directory path');
  statesDir = path.resolve(rawDir);
  const rawPort = option('--port') ?? '4318';
  if (!/^\d{1,5}$/.test(rawPort)) throw new Error('--port must be a valid port number');
  port = Number(rawPort);
  if (port < 1 || port > 65535) throw new Error('--port must be between 1 and 65535');
} catch (error) {
  console.error(error.message);
  process.exit(2);
}

const mime = new Map([
  ['/index.html', 'text/html; charset=utf-8'],
  ['/xterm.js', 'text/javascript; charset=utf-8'],
  ['/xterm.css', 'text/css; charset=utf-8'],
]);
const localFiles = new Map([
  ['/index.html', path.join(here, 'index.html')],
  ['/xterm.js', path.join(here, 'node_modules', '@xterm', 'xterm', 'lib', 'xterm.js')],
  ['/xterm.css', path.join(here, 'node_modules', '@xterm', 'xterm', 'css', 'xterm.css')],
]);

function send(res, status, contentType, body) {
  res.writeHead(status, {
    'Content-Type': contentType,
    'Cache-Control': 'no-store',
    'X-Content-Type-Options': 'nosniff',
    'Content-Security-Policy': "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; font-src 'self' data:; connect-src 'self'; img-src 'self' data:",
  });
  res.end(body);
}

const server = http.createServer(async (req, res) => {
  if (req.method !== 'GET' && req.method !== 'HEAD') {
    send(res, 405, 'text/plain; charset=utf-8', 'Method not allowed\n');
    return;
  }
  let url;
  try { url = new URL(req.url, 'http://127.0.0.1'); }
  catch { send(res, 400, 'text/plain; charset=utf-8', 'Bad request\n'); return; }

  try {
    if (url.pathname === '/favicon.ico') { send(res, 204, 'image/x-icon', ''); return; }
    if (url.pathname === '/states.json') {
      const data = await readFile(path.join(statesDir, 'states.json'));
      send(res, 200, 'application/json; charset=utf-8', req.method === 'HEAD' ? '' : data);
      return;
    }
    const route = url.pathname === '/' ? '/index.html' : url.pathname;
    const localPath = localFiles.get(route);
    if (!localPath) { send(res, 404, 'text/plain; charset=utf-8', 'Not found\n'); return; }
    const data = await readFile(localPath);
    send(res, 200, mime.get(route), req.method === 'HEAD' ? '' : data);
  } catch (error) {
    if (error.code === 'ENOENT') {
      const message = url.pathname === '/states.json'
        ? 'Preview export not found. Export Rust frames with: vesta-setup preview --export DIR\n'
        : 'Preview dependency or page is missing. Run npm install in scripts/setup-preview.\n';
      send(res, 404, 'text/plain; charset=utf-8', message);
    } else {
      send(res, 500, 'text/plain; charset=utf-8', 'Unable to read the preview resource\n');
    }
  }
});

server.on('error', error => {
  console.error(`Preview server failed: ${error.message}`);
  process.exitCode = 1;
});
server.listen(port, '127.0.0.1', () => {
  console.log(`Vesta setup preview: http://127.0.0.1:${port}/`);
});
