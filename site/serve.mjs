#!/usr/bin/env node
// Serves the built site: a small static server with no dependencies.
// Files are read and compressed once at startup, so a new build needs a restart.
//
//   PORT       port to listen on            (default 28952)
//   HOST       address to bind              (default 127.0.0.1)
//   SITE_ROOT  directory holding the build  (default ./dist next to this file)
import { createServer } from "node:http";
import { readdir, readFile } from "node:fs/promises";
import { extname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".woff2": "font/woff2",
  ".txt": "text/plain; charset=utf-8",
};
const COMPRESSIBLE = new Set([".html", ".css", ".js", ".json", ".svg", ".txt"]);
const SECURITY_HEADERS = {
  "Content-Security-Policy":
    "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
  "X-Content-Type-Options": "nosniff",
  "Referrer-Policy": "strict-origin-when-cross-origin",
};

function log(event, fields = {}) {
  process.stdout.write(`${JSON.stringify({ time: new Date().toISOString(), event, ...fields })}\n`);
}

function config() {
  const port = Number(process.env.PORT ?? 28952);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("PORT must be 1 to 65535");
  const here = fileURLToPath(new URL(".", import.meta.url));
  return {
    port,
    host: process.env.HOST ?? "127.0.0.1",
    root: resolve(process.env.SITE_ROOT ?? join(here, "dist")),
  };
}

function cacheControl(path) {
  if (path.startsWith("/assets/")) return "public, max-age=31536000, immutable";
  return "no-cache";
}

async function load(root) {
  const files = new Map();
  const walk = async (dir) => {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const full = join(dir, entry.name);
      if (entry.isDirectory()) {
        await walk(full);
        continue;
      }
      const path = `/${relative(root, full).split(sep).join("/")}`;
      const ext = extname(entry.name);
      const body = await readFile(full);
      const file = { type: TYPES[ext] ?? "application/octet-stream", cache: cacheControl(path), body };
      if (COMPRESSIBLE.has(ext) && body.length > 512) {
        file.br = brotliCompressSync(body, { params: { [constants.BROTLI_PARAM_QUALITY]: 11 } });
        file.gzip = gzipSync(body, { level: 9 });
      }
      files.set(path, file);
    }
  };
  await walk(root);
  if (!files.has("/index.html")) throw new Error(`no index.html in ${root}; run the build first`);
  return files;
}

function respond(req, res, files) {
  if (req.method !== "GET" && req.method !== "HEAD") {
    res.writeHead(405, { Allow: "GET, HEAD" }).end();
    return;
  }
  let path;
  try {
    path = decodeURIComponent(new URL(req.url ?? "/", "http://localhost").pathname);
  } catch {
    res.writeHead(400).end();
    return;
  }
  if (path === "/healthz") {
    res.writeHead(200, { "Content-Type": "text/plain; charset=utf-8", "Cache-Control": "no-store" }).end("ok\n");
    return;
  }
  const file = files.get(path === "/" ? "/index.html" : path);
  if (!file) {
    res
      .writeHead(404, { "Content-Type": "text/plain; charset=utf-8", "Cache-Control": "no-store", ...SECURITY_HEADERS })
      .end("Not found\n");
    return;
  }
  const accepts = String(req.headers["accept-encoding"] ?? "");
  const headers = { "Content-Type": file.type, "Cache-Control": file.cache, ...SECURITY_HEADERS };
  let body = file.body;
  if (file.br) {
    headers.Vary = "Accept-Encoding";
    if (/\bbr\b/.test(accepts)) {
      body = file.br;
      headers["Content-Encoding"] = "br";
    } else if (/\bgzip\b/.test(accepts)) {
      body = file.gzip;
      headers["Content-Encoding"] = "gzip";
    }
  }
  headers["Content-Length"] = body.length;
  res.writeHead(200, headers).end(req.method === "HEAD" ? undefined : body);
}

try {
  const { port, host, root } = config();
  const files = await load(root);
  const server = createServer((req, res) => respond(req, res, files));
  server.requestTimeout = 10_000;
  server.headersTimeout = 5_000;
  server.on("error", (error) => {
    log("server_error", { message: error.message });
    process.exit(1);
  });
  server.listen(port, host, () => log("listening", { host, port, root, files: files.size }));
  for (const signal of ["SIGTERM", "SIGINT"]) {
    process.on(signal, () => {
      log("stopping", { signal });
      server.close(() => process.exit(0));
      server.closeIdleConnections();
      setTimeout(() => process.exit(0), 3000).unref();
    });
  }
} catch (error) {
  log("startup_failed", { message: error instanceof Error ? error.message : String(error) });
  process.exit(1);
}
