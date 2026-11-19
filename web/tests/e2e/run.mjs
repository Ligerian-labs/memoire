// Runs the Playwright journey against a real gateway: a disposable
// PostgreSQL (or MEMOIRE_E2E_DATABASE_URL), the gateway with the development
// configuration and test overrides, and the production build behind
// server/serve.mjs. Synthetic data only; everything lives under
// test-results/e2e-runtime/ and is removed afterwards unless the run fails.
//
//   pnpm e2e [playwright args…]
//
// MEMOIRE_GATEWAY_BIN   prebuilt memoire-gateway (default: cargo build)
// MEMOIRE_E2E_DATABASE_URL  an empty disposable database (default: own cluster)
// MEMOIRE_E2E_WEB_PORT / _PG_PORT / _GATEWAY_PORT  (default 28981 / 28982 / 28983)
import { spawn, spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import {
  createWriteStream,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

const web = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const repo = resolve(web, '..');
const port = (name, fallback) => {
  const value = Number(process.env[`MEMOIRE_E2E_${name}_PORT`] ?? fallback);
  if (!Number.isInteger(value) || value < 1024 || value > 65535)
    throw new Error(`bad ${name} port`);
  return value;
};
const webPort = port('WEB', 28981);
const pgPort = port('PG', 28982);
const gatewayPort = port('GATEWAY', 28983);
const origin = `http://localhost:${webPort}`;
const runtime = join(web, 'test-results/e2e-runtime');
const children = [];
let pgDir = null;
let failed = true;

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { stdio: 'inherit', ...options });
  if (result.status !== 0) throw new Error(`${command} ${args[0] ?? ''} exited ${result.status}`);
}

async function free(p) {
  await new Promise((ok, no) => {
    const probe = createServer();
    probe.once('error', () => no(new Error(`port ${p} is in use`)));
    probe.listen(p, '127.0.0.1', () => probe.close(ok));
  });
}

function start(name, command, args, env = {}) {
  const log = createWriteStream(join(runtime, `${name}.log`), { mode: 0o600 });
  const child = spawn(command, args, {
    env: { ...process.env, ...env },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout.pipe(log, { end: false });
  child.stderr.pipe(log, { end: false });
  child.once('exit', () => log.end());
  children.push({ name, child });
  return child;
}

async function ready(url, child, name) {
  for (let i = 0; i < 1200; i++) {
    if (child.exitCode !== null)
      throw new Error(`${name} exited ${child.exitCode}; see ${name}.log`);
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      /* starting */
    }
    await delay(100);
  }
  throw new Error(`${name} not ready at ${url}`);
}

async function cleanup() {
  for (const { child } of children.reverse()) {
    if (child.exitCode === null) {
      child.kill('SIGTERM');
      await Promise.race([new Promise((ok) => child.once('exit', ok)), delay(10_000)]);
      if (child.exitCode === null) child.kill('SIGKILL');
    }
  }
  if (pgDir) spawnSync(join(repo, 'scripts/dev-postgres.sh'), ['stop', pgDir], { stdio: 'ignore' });
  if (!failed) rmSync(runtime, { recursive: true, force: true });
  else console.error(`Logs kept in ${runtime}`);
}

process.once('SIGINT', () => cleanup().then(() => process.exit(130)));

try {
  for (const p of [webPort, pgPort, gatewayPort]) await free(p);
  rmSync(runtime, { recursive: true, force: true });
  mkdirSync(join(runtime, 'secrets'), { recursive: true, mode: 0o700 });

  let gateway = process.env.MEMOIRE_GATEWAY_BIN;
  if (!gateway) {
    run('cargo', ['build', '--locked', '-p', 'memoire-gateway'], { cwd: repo });
    gateway = join(process.env.CARGO_TARGET_DIR ?? join(repo, 'target'), 'debug/memoire-gateway');
  }
  if (!existsSync(join(web, 'dist/memoire-web/browser/index.html')))
    run('pnpm', ['build'], { cwd: web });

  let databaseUrl = process.env.MEMOIRE_E2E_DATABASE_URL;
  if (!databaseUrl) {
    pgDir = join(runtime, 'pg');
    const started = spawnSync(
      join(repo, 'scripts/dev-postgres.sh'),
      ['start', pgDir, String(pgPort)],
      {
        encoding: 'utf8',
      },
    );
    if (started.status !== 0) throw new Error(`PostgreSQL did not start: ${started.stderr}`);
    databaseUrl = started.stdout.trim();
  }

  for (const name of ['identity', 'notifications']) {
    writeFileSync(join(runtime, 'secrets', `${name}.key`), randomBytes(48).toString('base64'), {
      mode: 0o600,
    });
  }
  // The development configuration, moved into this run: its .memoire-dev paths
  // point here (secrets may not be command-line overrides) and its other
  // relative paths resolve against config/dev as before.
  const devDir = join(repo, 'config/dev');
  const config = readFileSync(join(devDir, 'memoire.toml'), 'utf8').replace(
    /^([^#\n]*\b(?:manifest|path|file|data_dir)\s*=\s*)"([^"]+)"/gm,
    (_, key, value) => {
      const dev = value.indexOf('.memoire-dev/');
      const absolute =
        dev !== -1
          ? join(runtime, value.slice(dev + '.memoire-dev/'.length))
          : resolve(devDir, value);
      return `${key}${JSON.stringify(absolute)}`;
    },
  );
  // Build the guests when the runtime manifest names a component that is missing.
  const manifestPath = /^manifest\s*=\s*"([^"]+)"/m.exec(config)?.[1];
  if (!manifestPath) throw new Error('config/dev/memoire.toml has no [runtime] manifest');
  const manifest = readFileSync(manifestPath, 'utf8');
  const components = [...manifest.matchAll(/^\s*path\s*=\s*"([^"]+\.wasm)"/gm)].map((m) =>
    resolve(dirname(manifestPath), m[1]),
  );
  if (components.some((c) => !existsSync(c)))
    run(join(repo, 'scripts/build-guests.sh'), [], { cwd: repo });
  writeFileSync(join(runtime, 'memoire.toml'), config, { mode: 0o600 });
  const gatewayProcess = start(
    'gateway',
    gateway,
    [
      'run',
      '--config',
      join(runtime, 'memoire.toml'),
      '--listen',
      `127.0.0.1:${gatewayPort}`,
      '--set',
      `modules.identity.app_origin=${JSON.stringify(origin)}`,
    ],
    { MEMOIRE_DATABASE_URL: databaseUrl },
  );
  await ready(`http://127.0.0.1:${gatewayPort}/health/ready`, gatewayProcess, 'gateway');

  const webProcess = start('web', process.execPath, [
    join(web, 'server/serve.mjs'),
    '--listen',
    `127.0.0.1:${webPort}`,
    '--gateway',
    `http://127.0.0.1:${gatewayPort}`,
  ]);
  await ready(`http://127.0.0.1:${webPort}/health/ready`, webProcess, 'web');

  const result = spawnSync('pnpm', ['exec', 'playwright', 'test', ...process.argv.slice(2)], {
    cwd: web,
    stdio: 'inherit',
    env: { ...process.env, MEMOIRE_E2E_BASE_URL: origin },
  });
  failed = result.status !== 0;
  process.exitCode = result.status ?? 1;
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
} finally {
  await cleanup();
}
