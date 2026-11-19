// The web debugger's production server: serves the built app and proxies
// /v1 and /health to the gateway, so the browser sees one origin.
//
//   node server/serve.mjs [--listen HOST:PORT] [--gateway URL] [--root DIR]
//
// Environment (flags win): MEMOIRE_WEB_LISTEN (default 127.0.0.1:28951),
// MEMOIRE_WEB_GATEWAY (default http://127.0.0.1:28950), MEMOIRE_WEB_ROOT
// (default dist/memoire-web/browser next to this directory). The public
// origin is not configured here: the build uses relative URLs only, and the
// gateway's modules.identity.app_origin names the origin browsers use.
import { createReadStream, existsSync, statSync } from 'node:fs';
import { createServer as createHttpServer, request as httpRequest } from 'node:http';
import { request as httpsRequest } from 'node:https';
import { isIP } from 'node:net';
import { dirname, extname, join, normalize, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const PROXIED = ['/v1/', '/health/'];
const HOP_BY_HOP = new Set([
  'connection',
  'keep-alive',
  'proxy-authenticate',
  'proxy-authorization',
  'proxy-connection',
  'te',
  'trailer',
  'transfer-encoding',
  'upgrade',
]);
const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
  '.woff2': 'font/woff2',
  '.txt': 'text/plain; charset=utf-8',
  '.map': 'application/json',
};
const SECURITY_HEADERS = {
  'Content-Security-Policy':
    "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; " +
    "connect-src 'self'; font-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self'; " +
    "frame-ancestors 'none'",
  'X-Content-Type-Options': 'nosniff',
  'Referrer-Policy': 'no-referrer',
  'X-Frame-Options': 'DENY',
};
// Hashed bundles never change under a name; everything else is revalidated.
const HASHED = /-[A-Z0-9]{8,}\.(js|css)$/;

/** Parses flags and environment into one validated config, or throws every problem at once. */
export function parseConfig(argv = process.argv.slice(2), env = process.env) {
  const flags = {};
  const problems = [];
  for (let i = 0; i < argv.length; i++) {
    const match = /^--(listen|gateway|root)(?:=(.*))?$/.exec(argv[i]);
    if (!match) {
      problems.push(`unknown argument ${JSON.stringify(argv[i])}`);
      continue;
    }
    const value = match[2] ?? argv[++i];
    if (value === undefined) problems.push(`--${match[1]} needs a value`);
    else flags[match[1]] = value;
  }
  const listenText = flags.listen ?? env.MEMOIRE_WEB_LISTEN ?? '127.0.0.1:28951';
  const gatewayText = flags.gateway ?? env.MEMOIRE_WEB_GATEWAY ?? 'http://127.0.0.1:28950';
  const root = resolve(
    flags.root ?? env.MEMOIRE_WEB_ROOT ?? join(here, '../dist/memoire-web/browser'),
  );

  let listen;
  const at = listenText.lastIndexOf(':');
  const host = listenText.slice(0, at).replace(/^\[(.*)\]$/, '$1');
  const port = Number(listenText.slice(at + 1));
  if (at <= 0 || !isIP(host) || !Number.isInteger(port) || port < 1 || port > 65535) {
    problems.push(`listen must be IP:PORT, got ${JSON.stringify(listenText)}`);
  } else listen = { host, port };

  let gateway;
  try {
    gateway = new URL(gatewayText);
    if (
      !['http:', 'https:'].includes(gateway.protocol) ||
      gateway.pathname !== '/' ||
      gateway.search
    )
      throw new Error();
  } catch {
    problems.push(
      `gateway must be an http(s) origin like http://127.0.0.1:28950, got ${JSON.stringify(gatewayText)}`,
    );
  }

  if (!existsSync(join(root, 'index.html')))
    problems.push(`root ${root} has no index.html; run pnpm build`);
  if (problems.length) throw new Error(`invalid configuration:\n  ${problems.join('\n  ')}`);
  return { listen, gateway, root };
}

function log(level, event, fields = {}) {
  process.stdout.write(
    JSON.stringify({
      ts: new Date().toISOString(),
      level,
      event,
      service: 'memoire-web',
      ...fields,
    }) + '\n',
  );
}

/** Builds the server; `start` binds it. */
export function createWebServer({ gateway, root }) {
  const send = gateway.protocol === 'https:' ? httpsRequest : httpRequest;

  const server = createHttpServer((req, res) => {
    const started = process.hrtime.bigint();
    const path = (req.url ?? '/').split('?')[0];
    res.on('finish', () => {
      const ms = Number(process.hrtime.bigint() - started) / 1e6;
      log(res.statusCode >= 500 ? 'error' : 'info', 'http.request', {
        method: req.method,
        path: path.length > 200 ? path.slice(0, 200) : path,
        status: res.statusCode,
        duration_ms: Math.round(ms),
      });
    });
    if (PROXIED.some((prefix) => path.startsWith(prefix))) proxy(req, res);
    else serveStatic(req, res, path);
  });

  function proxy(req, res) {
    const headers = {};
    for (const [name, value] of Object.entries(req.headers)) {
      if (!HOP_BY_HOP.has(name)) headers[name] = value;
    }
    // The gateway uses these only when it trusts this server as a proxy.
    const peer = req.socket.remoteAddress ?? '';
    headers['x-forwarded-for'] = headers['x-forwarded-for']
      ? `${headers['x-forwarded-for']}, ${peer}`
      : peer;
    headers['x-forwarded-proto'] ??= 'http';
    const upstream = send(
      {
        protocol: gateway.protocol,
        hostname: gateway.hostname.replace(/^\[(.*)\]$/, '$1'),
        port: gateway.port || (gateway.protocol === 'https:' ? 443 : 80),
        method: req.method,
        path: req.url,
        headers,
      },
      (answer) => {
        const out = {};
        for (const [name, value] of Object.entries(answer.headers)) {
          if (!HOP_BY_HOP.has(name)) out[name] = value;
        }
        res.writeHead(answer.statusCode ?? 502, out);
        answer.pipe(res);
        answer.on('error', () => res.destroy());
      },
    );
    upstream.on('error', (error) => {
      log('error', 'proxy.failed', { code: error.code ?? 'error' });
      if (res.headersSent) return res.destroy();
      res.writeHead(502, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
      res.end(
        JSON.stringify({ error: { code: 'gateway_unavailable', classification: 'transient' } }),
      );
    });
    req.on('aborted', () => upstream.destroy());
    req.pipe(upstream);
  }

  function serveStatic(req, res, path) {
    if (req.method !== 'GET' && req.method !== 'HEAD') {
      res.writeHead(405, { Allow: 'GET, HEAD', ...SECURITY_HEADERS });
      return res.end();
    }
    let decoded;
    try {
      decoded = decodeURIComponent(path);
    } catch {
      return notFound(res);
    }
    if (decoded.includes('\0')) return notFound(res);
    const file = normalize(join(root, decoded));
    if (file !== root && !file.startsWith(root + sep)) return notFound(res);
    if (isFile(file)) return sendFile(req, res, file);
    // A path with an extension names an asset; a missing one is a 404, not the app.
    if (extname(decoded) !== '') return notFound(res);
    sendFile(req, res, join(root, 'index.html'));
  }

  function sendFile(req, res, file) {
    const name = file.slice(root.length + 1);
    res.writeHead(200, {
      'Content-Type': TYPES[extname(file)] ?? 'application/octet-stream',
      'Content-Length': statSync(file).size,
      'Cache-Control': HASHED.test(name) ? 'public, max-age=31536000, immutable' : 'no-cache',
      ...SECURITY_HEADERS,
    });
    if (req.method === 'HEAD') return res.end();
    createReadStream(file)
      .on('error', () => res.destroy())
      .pipe(res);
  }

  function notFound(res) {
    res.writeHead(404, { 'Content-Type': 'text/plain; charset=utf-8', ...SECURITY_HEADERS });
    res.end('Not found\n');
  }

  return server;
}

function isFile(path) {
  try {
    return statSync(path).isFile();
  } catch {
    return false;
  }
}

/** Starts the server and drains it on SIGTERM or SIGINT within ten seconds. */
export async function start(config) {
  const server = createWebServer(config);
  await new Promise((resolveListen, reject) => {
    server.once('error', reject);
    server.listen(config.listen.port, config.listen.host, resolveListen);
  });
  log('info', 'server.started', {
    listen: `${config.listen.host}:${config.listen.port}`,
    gateway: config.gateway.origin,
  });
  const stop = (signal) => {
    log('info', 'server.stopping', { signal });
    server.close(() => {
      log('info', 'server.stopped');
      process.exit(0);
    });
    server.closeIdleConnections();
    setTimeout(() => {
      log('warn', 'server.drain_timeout');
      process.exit(1);
    }, 10_000).unref();
  };
  process.once('SIGTERM', stop);
  process.once('SIGINT', stop);
  return server;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  try {
    await start(parseConfig());
  } catch (error) {
    log('error', 'server.start_failed', { message: error.message });
    process.exit(1);
  }
}
