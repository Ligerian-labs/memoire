import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, before, describe, it } from 'node:test';
import { createWebServer, parseConfig } from './serve.mjs';

const root = mkdtempSync(join(tmpdir(), 'memoire-web-'));
writeFileSync(join(root, 'index.html'), '<app-root></app-root>');
writeFileSync(join(root, 'main-ABCDEFGH.js'), 'console.log(1)');
mkdirSync(join(root, 'nested'));
writeFileSync(join(root, 'nested', 'a.txt'), 'a');

const listen = (server) =>
  new Promise((done) => server.listen(0, '127.0.0.1', () => done(server.address().port)));

describe('parseConfig', () => {
  it('defaults to loopback and the production ports', () => {
    const config = parseConfig([], { MEMOIRE_WEB_ROOT: root });
    assert.deepEqual(config.listen, { host: '127.0.0.1', port: 28951 });
    assert.equal(config.gateway.origin, 'http://127.0.0.1:28950');
  });

  it('lets flags override the environment', () => {
    const config = parseConfig(
      ['--listen', '127.0.0.1:28981', '--gateway=http://127.0.0.1:28982'],
      {
        MEMOIRE_WEB_LISTEN: '127.0.0.1:1',
        MEMOIRE_WEB_ROOT: root,
      },
    );
    assert.deepEqual(config.listen, { host: '127.0.0.1', port: 28981 });
    assert.equal(config.gateway.origin, 'http://127.0.0.1:28982');
  });

  it('accepts a bracketed IPv6 listen address', () => {
    const config = parseConfig(['--listen', '[::1]:28981', '--root', root], {});
    assert.deepEqual(config.listen, { host: '::1', port: 28981 });
  });

  it('reports every problem at once', () => {
    assert.throws(
      () =>
        parseConfig(['--listen', 'localhost', '--gateway', 'ftp://x/y', '--bogus'], {
          MEMOIRE_WEB_ROOT: join(root, 'missing'),
        }),
      (error) =>
        ['unknown argument', 'listen must be', 'gateway must be', 'has no index.html'].every(
          (part) => error.message.includes(part),
        ),
    );
  });
});

describe('web server', () => {
  let gateway;
  let web;
  let base;
  const seen = [];

  before(async () => {
    gateway = createServer((req, res) => {
      let body = '';
      req.on('data', (chunk) => (body += chunk));
      req.on('end', () => {
        seen.push({ method: req.method, url: req.url, headers: req.headers, body });
        res.writeHead(201, {
          'Content-Type': 'application/json',
          'Set-Cookie': 'merezha_session=s; HttpOnly',
        });
        res.end('{"ok":true}');
      });
    });
    const gatewayPort = await listen(gateway);
    web = createWebServer({ gateway: new URL(`http://127.0.0.1:${gatewayPort}`), root });
    base = `http://127.0.0.1:${await listen(web)}`;
  });

  after(() => {
    web.close();
    gateway.close();
  });

  it('proxies /v1 with the method, path, query, body, cookies and Origin', async () => {
    const response = await fetch(`${base}/v1/memoire/databases?state=all`, {
      method: 'POST',
      headers: {
        Origin: 'http://localhost:28980',
        Cookie: 'merezha_session=s',
        'X-CSRF-Token': 't',
      },
      body: '{"name":"scratch"}',
    });
    assert.equal(response.status, 201);
    assert.equal(response.headers.get('set-cookie'), 'merezha_session=s; HttpOnly');
    assert.deepEqual(await response.json(), { ok: true });
    const last = seen.at(-1);
    assert.equal(last.method, 'POST');
    assert.equal(last.url, '/v1/memoire/databases?state=all');
    assert.equal(last.body, '{"name":"scratch"}');
    assert.equal(last.headers.origin, 'http://localhost:28980');
    assert.equal(last.headers.cookie, 'merezha_session=s');
    assert.equal(last.headers['x-csrf-token'], 't');
    assert.equal(last.headers['x-forwarded-for'], '127.0.0.1');
    assert.equal(last.headers.host, new URL(base).host);
  });

  it('proxies /health', async () => {
    const response = await fetch(`${base}/health/ready`);
    assert.equal(response.status, 201);
    assert.equal(seen.at(-1).url, '/health/ready');
  });

  it('serves files, with long caching only for hashed bundles', async () => {
    const bundle = await fetch(`${base}/main-ABCDEFGH.js`);
    assert.equal(bundle.status, 200);
    assert.match(bundle.headers.get('content-type'), /javascript/);
    assert.match(bundle.headers.get('cache-control'), /immutable/);
    const index = await fetch(`${base}/`);
    assert.equal(index.headers.get('cache-control'), 'no-cache');
    assert.match(index.headers.get('content-security-policy'), /frame-ancestors 'none'/);
  });

  it('falls back to index.html for app routes and 404s for missing assets', async () => {
    const route = await fetch(`${base}/databases/scratch`);
    assert.equal(route.status, 200);
    assert.equal(await route.text(), '<app-root></app-root>');
    assert.equal((await fetch(`${base}/missing-ABCDEFGH.js`)).status, 404);
  });

  it('never serves outside the root', async () => {
    for (const path of ['/..%2f..%2fetc%2fpasswd', '/%2e%2e/%2e%2e/etc/passwd', '/a%00.txt']) {
      const response = await fetch(base + path);
      assert.notEqual(await response.text(), '');
      assert.ok([200, 404].includes(response.status));
      assert.doesNotMatch(await fetch(base + path).then((r) => r.text()), /root:/);
    }
  });

  it('answers 502 in the error envelope when the gateway is down', async () => {
    const down = createWebServer({ gateway: new URL('http://127.0.0.1:9'), root });
    const port = await listen(down);
    const response = await fetch(`http://127.0.0.1:${port}/v1/memoire/whoami`);
    assert.equal(response.status, 502);
    assert.deepEqual(await response.json(), {
      error: { code: 'gateway_unavailable', classification: 'transient' },
    });
    down.close();
  });
});
