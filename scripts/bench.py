#!/usr/bin/env python3
"""Benchmark one memoire database over the HTTP API.

    scripts/bench.py [--db NAME] [--scale N] [--samples N] [--heavy-samples N]
                     [--ttl SECONDS] [--out FILE] [--keep]

Reads MEMOIRE_SERVER (default https://app.memoire.network) and
MEMOIRE_API_KEY from the environment, never from arguments. The key needs
the read, write, create and delete database permissions.

Creates a burstable database (default name `bench`), fills it with
synthetic data in tables prefixed `bench_`, measures, writes the results
to FILE as JSON (default bench-result.json) and deletes the database.
A database of that name that already exists is replaced only when it is
burstable and holds nothing but `bench_` tables; otherwise the script
refuses to run. --keep leaves the database in place at the end. The API
deletes by name, so two runs on the same name at the same time can delete
each other's database: give them different names with --db.

Every latency is measured at the client around one request and its whole
response on a connection that is already open, so it includes the network
round trip. `GET /health/live` is that round trip alone, and the script
stores it beside every test as its `baseline`: interleaved with the read
and write tests, one request of each per round, and measured just before
each of the other tests.

--scale multiplies the data volume: at 1 it is 20 000 customers, 2 000
products, 100 000 orders, about 300 000 order items and 1 000 000 events.
The data is the same for a given scale (fixed seed), apart from the random
payload of the events. Neither the output nor FILE holds an id or a key.
Python 3 standard library only.
"""
import argparse
import datetime
import http.client
import json
import math
import os
import platform
import random
import socket
import string
import sys
import tempfile
import threading
import time
import urllib.parse

PREFIX = "/v1/memoire"
TABLE_PREFIX = "bench_"
SEED = 42
BASELINE_SAMPLES = 100
EVENTS_PER_STATEMENT = 1_000_000
# A fixed "now" keeps the generated timestamps the same from run to run.
NOW = 1_795_200_000
COUNTRIES = ["FR", "DE", "ES", "IT", "GB", "US", "CA", "BR", "JP", "IN"]
CATEGORIES = ["books", "tools", "garden", "kitchen", "toys", "sports", "music", "office"]
STATUSES = ["pending", "paid", "shipped", "delivered", "refunded"]

SCHEMA = [
    "CREATE TABLE bench_customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE, name TEXT NOT NULL,"
    " country TEXT NOT NULL, created_at INTEGER NOT NULL)",
    "CREATE TABLE bench_products (id INTEGER PRIMARY KEY, sku TEXT NOT NULL UNIQUE, title TEXT NOT NULL,"
    " price_cents INTEGER NOT NULL, category TEXT NOT NULL)",
    "CREATE TABLE bench_orders (id INTEGER PRIMARY KEY, customer_id INTEGER NOT NULL REFERENCES bench_customers(id),"
    " status TEXT NOT NULL, created_at INTEGER NOT NULL)",
    "CREATE TABLE bench_order_items (order_id INTEGER NOT NULL REFERENCES bench_orders(id),"
    " product_id INTEGER NOT NULL REFERENCES bench_products(id), qty INTEGER NOT NULL, price_cents INTEGER NOT NULL,"
    " PRIMARY KEY (order_id, product_id)) WITHOUT ROWID",
    "CREATE TABLE bench_events (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, payload TEXT NOT NULL, at INTEGER NOT NULL)",
    "CREATE INDEX bench_orders_customer ON bench_orders(customer_id, created_at)",
    "CREATE INDEX bench_orders_status ON bench_orders(status)",
    "CREATE INDEX bench_items_product ON bench_order_items(product_id)",
    "CREATE INDEX bench_customers_country ON bench_customers(country)",
]

AGGREGATES = {
    "revenue_by_country": "SELECT c.country, count(DISTINCT o.id) orders, sum(i.qty * i.price_cents) / 100.0 revenue"
    " FROM bench_orders o JOIN bench_customers c ON c.id = o.customer_id"
    " JOIN bench_order_items i ON i.order_id = o.id GROUP BY c.country ORDER BY revenue DESC",
    "top_10_products": "SELECT p.sku, sum(i.qty) sold FROM bench_order_items i"
    " JOIN bench_products p ON p.id = i.product_id GROUP BY p.id ORDER BY sold DESC LIMIT 10",
    "events_by_kind": "SELECT kind, count(*) FROM bench_events GROUP BY kind",
    "events_json_scan": "SELECT count(*) FROM bench_events WHERE json_extract(payload, '$.user') < 100",
}


class BenchError(Exception):
    pass


class Client:
    """One keep-alive connection. `request` times a request and its response."""

    def __init__(self, server, key):
        self.server, self.key, self.reconnects = server, key, 0
        self.connection = self._open()

    def _open(self):
        kind = http.client.HTTPSConnection if self.server.scheme == "https" else http.client.HTTPConnection
        return kind(self.server.hostname, self.server.port, timeout=120)

    def close(self):
        self.connection.close()

    def request(self, method, path, body=None, raw=None, authenticated=True, replay=None):
        """`replay` says whether the request may be sent again when the
        connection drops: by default only a GET."""
        replay = method == "GET" if replay is None else replay
        data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
        headers = {"Accept": "application/json"}
        if authenticated:
            headers["Authorization"] = "Bearer " + self.key
        if data is not None:
            headers["Content-Type"] = "application/json"
        started = time.perf_counter()
        for attempt in range(2):
            try:
                self.connection.request(method, path, body=data, headers=headers)
                response = self.connection.getresponse()
                payload = response.read()
                break
            except (http.client.HTTPException, OSError):
                # A dropped connection is retried once, and the time of the
                # failed attempt stays in the measurement. The result file
                # counts these retries as `reconnects`. A write is never
                # sent twice: its first answer may have been lost after it
                # applied, so the error stops the run instead.
                self.connection.close()
                self.connection = self._open()
                if attempt or not replay:
                    raise
                self.reconnects += 1
        elapsed_ms = (time.perf_counter() - started) * 1000
        return Response(response.status, parse(response, payload), elapsed_ms, len(payload))

    def upload(self, path, data):
        """Sends a body the server may refuse before it has read all of it,
        and reads the answer even when the upload was cut short."""
        self.connection.putrequest("POST", path)
        self.connection.putheader("Authorization", "Bearer " + self.key)
        self.connection.putheader("Content-Type", "application/json")
        self.connection.putheader("Content-Length", str(len(data)))
        self.connection.endheaders()
        try:
            self.connection.send(data)
        except OSError:
            pass
        response = self.connection.getresponse()
        payload = response.read()
        return Response(response.status, parse(response, payload), 0.0, len(payload))

    def query(self, db, statements, mode="read", transaction=False, timeout_ms=None):
        body = {"mode": mode, "transaction": transaction, "statements": statements}
        if timeout_ms:
            body["timeout_ms"] = timeout_ms
        return self.request("POST", f"{PREFIX}/databases/{db}/query", body, replay=mode == "read")


def parse(response, payload):
    if payload and response.getheader("Content-Type", "").startswith("application/json"):
        try:
            return json.loads(payload)
        except ValueError:
            return None
    return None


class Response:
    def __init__(self, status, body, ms, size):
        self.status, self.body, self.ms, self.size = status, body, ms, size

    @property
    def code(self):
        """The error code, never the rest of an error body."""
        if isinstance(self.body, dict) and isinstance(self.body.get("error"), dict):
            return self.body["error"].get("code")
        return None

    def ok(self, what, status=200):
        if self.status != status:
            raise BenchError(f"{what}: HTTP {self.status} {self.code or ''}".rstrip())
        return self


def summary(samples):
    """Nearest-rank percentiles of latencies in milliseconds."""
    ordered = sorted(samples)
    rank = lambda p: ordered[max(0, math.ceil(p * len(ordered)) - 1)]
    return {
        "n": len(ordered),
        "min_ms": round(ordered[0], 2),
        "p50_ms": round(rank(0.50), 2),
        "p95_ms": round(rank(0.95), 2),
        "p99_ms": round(rank(0.99), 2),
        "max_ms": round(ordered[-1], 2),
        "mean_ms": round(sum(ordered) / len(ordered), 2),
    }


def line(stats):
    return (
        f"n={stats['n']} p50={stats['p50_ms']:.1f} p95={stats['p95_ms']:.1f}"
        f" p99={stats['p99_ms']:.1f} max={stats['max_ms']:.1f} ms"
    )


class Bench:
    def __init__(self, args, server, key):
        self.args, self.server, self.key = args, server, key
        self.db = args.db
        self.client = Client(server, key)
        self.rng = random.Random(SEED)
        self.created, self.created_id = False, None
        self.reconnects = 0
        self.customers = int(20_000 * args.scale)
        self.products = int(2_000 * args.scale)
        self.orders = int(100_000 * args.scale)
        self.events = int(1_000_000 * args.scale)
        self.result = {
            "tool": "scripts/bench.py",
            "format": 1,
            "started_at": utc_now(),
            "server": f"{server.scheme}://{server.netloc}",
            "database": {"name": self.db, "kind": "burstable", "idle_ttl_seconds": args.ttl},
            "scale": args.scale,
            "samples": args.samples,
            "heavy_samples": args.heavy_samples,
            "seed": SEED,
            "client": {"python": platform.python_version(), "os": sys.platform, "cpus": os.cpu_count(),
                       "load_average_1m_at_start": load_average()},
        }

    # ------------------------------------------------------------ helpers

    def path(self, suffix=""):
        return f"{PREFIX}/databases/{self.db}{suffix}"

    def health(self, client=None):
        return (client or self.client).request("GET", "/health/live", authenticated=False).ok("health/live").ms

    def baseline(self, n=BASELINE_SAMPLES):
        return summary([self.health() for _ in range(n)])

    def measure(self, section, name, call, n):
        """Runs `call` n times on the main connection, after a baseline."""
        base = self.baseline()
        stats = summary([call().ok(name).ms for _ in range(n)])
        stats["baseline"] = base
        self.result.setdefault(section, {})[name] = stats
        print(f"{name}: {line(stats)} | baseline p50={base['p50_ms']:.1f} ms")
        return stats

    def read(self, sql, params=None, timeout_ms=None):
        statement = {"sql": sql}
        if params is not None:
            statement["params"] = params
        return self.client.query(self.db, [statement], timeout_ms=timeout_ms)

    def fan_out(self, threads, per_thread, call):
        """`per_thread` calls on each of `threads` connections of their own.

        Each connection is opened and used once before the clock starts.
        Returns (latencies of 200s, {status or error code: count}, seconds).
        """
        latencies, errors, lock, started = [], {}, threading.Lock(), []
        barrier = threading.Barrier(threads + 1, action=lambda: started.append(time.perf_counter()))

        def work(index):
            client, rng = Client(self.server, self.key), random.Random(SEED + 1 + index)
            mine, failed = [], {}
            try:
                self.health(client)
                barrier.wait()
                for _ in range(per_thread):
                    try:
                        response = call(client, rng)
                        label = "ok" if response.status == 200 else response.code or str(response.status)
                    except (http.client.HTTPException, OSError):
                        label = "connection_error"
                    if label == "ok":
                        mine.append(response.ms)
                    else:
                        failed[label] = failed.get(label, 0) + 1
            except threading.BrokenBarrierError:
                failed["not_started"] = per_thread
            except (BenchError, http.client.HTTPException, OSError):
                failed["not_started"] = per_thread
                barrier.abort()
            finally:
                with lock:
                    latencies.extend(mine)
                    for label, count in failed.items():
                        errors[label] = errors.get(label, 0) + count
                    self.reconnects += client.reconnects
                client.close()

        workers = [threading.Thread(target=work, args=(i,)) for i in range(threads)]
        for worker in workers:
            worker.start()
        try:
            barrier.wait(timeout=60)
        except threading.BrokenBarrierError:
            pass
        for worker in workers:
            worker.join()
        return latencies, errors, time.perf_counter() - (started[0] if started else time.perf_counter())

    def concurrency(self, section, name, levels, per_thread, call):
        rows = []
        for threads in levels:
            latencies, errors, seconds = self.fan_out(threads, per_thread, call)
            row = {"connections": threads, "requests": threads * per_thread, "errors": errors}
            if latencies:
                row["requests_per_s"] = round(len(latencies) / seconds, 1)
                row.update(summary(latencies))
                print(f"{name} x{threads}: {row['requests_per_s']:,.0f} req/s, {line(row)}, errors={errors}")
            else:
                print(f"{name} x{threads}: no request succeeded, errors={errors}")
            rows.append(row)
        self.result.setdefault(section, {})[name] = rows

    # ------------------------------------------------------------ database

    def prepare(self):
        """Creates the database, replacing a leftover of an earlier run."""
        existing = self.client.request("GET", self.path())
        if existing.status == 200:
            kind, state = existing.body.get("kind"), existing.body.get("state")
            if kind != "burstable":
                raise BenchError(f"database {self.db!r} exists and is not burstable; refusing to touch it")
            if state == "active":
                schema = self.client.request("GET", self.path("/schema")).ok("schema").body
                try:
                    names = [o["name"] for o in schema["tables"] + schema["views"]]
                except (KeyError, TypeError):
                    raise BenchError(f"database {self.db!r}: cannot read its schema; refusing to touch it") from None
                foreign = [n for n in names if not n.startswith(TABLE_PREFIX)]
                if foreign:
                    raise BenchError(
                        f"database {self.db!r} holds {len(foreign)} table(s) or view(s) not named"
                        f" {TABLE_PREFIX}*; refusing to touch it"
                    )
                print(f"replacing the leftover database {self.db!r}")
                self.client.request("DELETE", self.path()).ok("delete leftover", 204)
            elif state != "expired":
                raise BenchError(f"database {self.db!r} is in an unknown state; refusing to touch it")
            # An expired entry has no file left, and creating takes its name.
        elif existing.status != 404:
            existing.ok("database lookup")
        created = self.client.request(
            "POST", f"{PREFIX}/databases", {"name": self.db, "kind": "burstable", "idle_ttl_seconds": self.args.ttl}
        ).ok("create", 201)
        self.created = True
        self.created_id = created.body.get("id")
        print(f"created burstable database {self.db!r}, idle TTL {self.args.ttl} s, in {created.ms:.0f} ms")
        self.result["database"]["create_ms"] = round(created.ms, 1)

    def cleanup(self):
        if not self.created or self.args.keep:
            return
        try:
            client = Client(self.server, self.key)
            current = client.request("GET", self.path())
            if current.status == 404:
                print(f"database {self.db!r} is already gone")
                return
            if current.status != 200 or not self.created_id or current.body.get("id") != self.created_id:
                # Another run took the name, or the lookup failed: not ours to delete.
                print(f"database {self.db!r} is not the one this run created; leaving it", file=sys.stderr)
                return
            deleted = client.request("DELETE", self.path())
        except (http.client.HTTPException, OSError):
            print(f"could not delete database {self.db!r}: connection failed", file=sys.stderr)
            return
        if deleted.status == 204:
            print(f"deleted database {self.db!r}")
        else:
            print(f"could not delete database {self.db!r}: HTTP {deleted.status}", file=sys.stderr)

    def size(self):
        return self.client.request("GET", self.path()).ok("database detail").body.get("size_bytes")

    # ------------------------------------------------------------ sections

    def connection_costs(self):
        """What a client pays before its first request on a new connection."""
        print("== connection")
        host, port = self.server.hostname, self.server.port or (443 if self.server.scheme == "https" else 80)
        tcp, ready, first = [], [], []
        for _ in range(20):
            started = time.perf_counter()
            socket.create_connection((host, port), timeout=30).close()
            tcp.append((time.perf_counter() - started) * 1000)
            client = Client(self.server, self.key)
            started = time.perf_counter()
            client.connection.connect()
            ready.append((time.perf_counter() - started) * 1000)
            self.health(client)
            first.append((time.perf_counter() - started) * 1000)
            client.close()
        self.result["connection"] = {
            "tcp_connect": summary(tcp),
            "connect_with_tls": summary(ready),
            "connect_and_first_request": summary(first),
        }
        for name, stats in self.result["connection"].items():
            print(f"{name}: n={stats['n']} p50={stats['p50_ms']:.1f} max={stats['max_ms']:.1f} ms")

    def identity(self):
        self.client.request("GET", "/health/ready", authenticated=False).ok("health/ready")
        who = self.client.request("GET", f"{PREFIX}/whoami").ok("whoami")
        self.result["credential"] = who.body.get("kind")

    def refusals(self):
        """Statements the gateway must refuse, with the code it answers."""
        checks = {}

        def expect(name, response, status, code):
            checks[name] = {
                "status": response.status,
                "code": response.code,
                "expected": f"{status} {code}",
                "ok": response.status == status and response.code == code,
            }
            print(f"{name} -> {response.status} {response.code} ({'ok' if checks[name]['ok'] else 'UNEXPECTED'})")

        query = lambda *a, **k: self.client.query(self.db, *a, **k)
        expect("write_in_read_mode", query([{"sql": "CREATE TABLE bench_ro (x)"}]), 422, "read_only")
        expect("attach", query([{"sql": "ATTACH DATABASE 'other.sqlite' AS o"}], mode="write"), 403, "statement_denied")
        expect("begin", query([{"sql": "BEGIN"}], mode="write"), 403, "statement_denied")
        self.result["refusals"] = checks

    def schema(self):
        print("== schema")
        created = self.client.query(self.db, [{"sql": s} for s in SCHEMA], mode="write", transaction=True).ok("schema")
        version = self.read("SELECT sqlite_version()").ok("sqlite_version").body["results"][0]["rows"][0][0]
        self.result["sqlite_version"] = version
        self.result["schema_ms"] = round(created.ms, 1)
        print(f"5 tables and 4 indexes in one transaction: {created.ms:.0f} ms (SQLite {version})")

    def ingest_table(self, table, columns, rows):
        """Multi-row INSERTs, as many statements per request as the limits allow."""
        per_statement = 999 // len(columns)
        values = "(" + ",".join("?" * len(columns)) + ")"
        bodies, statements, size = [], [], 0
        for start in range(0, len(rows), per_statement):
            chunk = rows[start : start + per_statement]
            statement = {
                "sql": f"INSERT INTO {table} ({','.join(columns)}) VALUES " + ",".join([values] * len(chunk)),
                "params": [value for row in chunk for value in row],
            }
            encoded = len(json.dumps(statement))
            if statements and (size + encoded > 900_000 or len(statements) == 100):
                bodies.append(statements)
                statements, size = [], 0
            statements.append(statement)
            size += encoded
        if statements:
            bodies.append(statements)
        encoded = [json.dumps({"mode": "write", "transaction": True, "statements": b}).encode() for b in bodies]
        base = self.baseline()
        latencies = [
            self.client.request("POST", self.path("/query"), raw=body).ok(f"insert into {table}").ms for body in encoded
        ]
        seconds = sum(latencies) / 1000
        stats = {
            "rows": len(rows),
            "requests": len(encoded),
            "request_bytes": sum(len(b) for b in encoded),
            "seconds": round(seconds, 3),
            "rows_per_s": round(len(rows) / seconds),
            "request": summary(latencies),
            "baseline": base,
        }
        self.result.setdefault("ingest", {})[table] = stats
        print(
            f"{table}: {len(rows):,} rows in {len(encoded)} requests, {seconds:.2f} s,"
            f" {stats['rows_per_s']:,} rows/s; request p50={stats['request']['p50_ms']:.0f} ms"
        )

    def ingest(self):
        print("== ingest over HTTP (multi-row INSERTs, one transaction per request)")
        rng, word = self.rng, lambda n: "".join(self.rng.choice(string.ascii_lowercase) for _ in range(n))
        self.ingest_table(
            "bench_customers",
            ["id", "email", "name", "country", "created_at"],
            [
                (i, f"user{i}@example.com", f"{word(6).title()} {word(9).title()}", rng.choice(COUNTRIES),
                 NOW - rng.randint(0, 86400 * 900))
                for i in range(1, self.customers + 1)
            ],
        )
        self.ingest_table(
            "bench_products",
            ["id", "sku", "title", "price_cents", "category"],
            [
                (i, f"SKU-{i:06d}", " ".join(word(rng.randint(4, 9)) for _ in range(3)), rng.randint(199, 49999),
                 rng.choice(CATEGORIES))
                for i in range(1, self.products + 1)
            ],
        )
        self.ingest_table(
            "bench_orders",
            ["id", "customer_id", "status", "created_at"],
            [
                (i, rng.randint(1, self.customers), rng.choice(STATUSES), NOW - rng.randint(0, 86400 * 365))
                for i in range(1, self.orders + 1)
            ],
        )
        items = [
            (order, product, rng.randint(1, 4), rng.randint(199, 49999))
            for order in range(1, self.orders + 1)
            for product in rng.sample(range(1, self.products + 1), rng.randint(1, 5))
        ]
        self.ingest_table("bench_order_items", ["order_id", "product_id", "qty", "price_cents"], items)

    def bulk(self):
        print("== bulk generation inside SQLite (recursive CTE, one statement per request)")
        base, latencies = self.baseline(), []
        for first in range(1, self.events + 1, EVENTS_PER_STATEMENT):
            last = min(first + EVENTS_PER_STATEMENT - 1, self.events)
            sql = (
                f"WITH RECURSIVE s(i) AS (SELECT {first} UNION ALL SELECT i + 1 FROM s WHERE i < {last})"
                " INSERT INTO bench_events (id, kind, payload, at) SELECT i,"
                " CASE i % 4 WHEN 0 THEN 'click' WHEN 1 THEN 'view' WHEN 2 THEN 'signup' ELSE 'purchase' END,"
                f" json_object('user', abs(random()) % {self.customers}, 'ref', hex(randomblob(8))),"
                f" {NOW} - (i * 7) % 31536000 FROM s"
            )
            done = self.client.query(self.db, [{"sql": sql}], mode="write", transaction=True, timeout_ms=30000)
            latencies.append(done.ok("bulk events").ms)
        seconds = sum(latencies) / 1000
        index = self.client.query(
            self.db, [{"sql": "CREATE INDEX bench_events_kind_at ON bench_events(kind, at)"}], mode="write",
            timeout_ms=30000,
        ).ok("index events")
        self.result["bulk"] = {
            "rows": self.events,
            "requests": len(latencies),
            "seconds": round(seconds, 3),
            "rows_per_s": round(self.events / seconds),
            "create_index_ms": round(index.ms, 1),
            "baseline": base,
        }
        print(f"bench_events: {self.events:,} rows in {seconds:.2f} s ({self.events / seconds:,.0f} rows/s)")
        print(f"index on (kind, at) over {self.events:,} rows: {index.ms / 1000:.2f} s")

    def volumes(self):
        tables = ["bench_customers", "bench_products", "bench_orders", "bench_order_items", "bench_events"]
        counts = self.client.query(self.db, [{"sql": f"SELECT count(*) FROM {t}"} for t in tables]).ok("counts")
        rows = {t: r["rows"][0][0] for t, r in zip(tables, counts.body["results"])}
        size = self.size()
        self.result["data"] = {"rows": rows, "size_bytes_after_load": size}
        print(f"rows: {rows}")
        print(f"database size after load (file and write-ahead log): {size / 1e6:.1f} MB")

    def latencies(self):
        """The request-by-request tests, interleaved.

        Each round sends one request of every test and one `GET
        /health/live`, on the main connection. A slow minute on the network
        or the server then touches every test and the round trip alike,
        instead of landing on whichever test happened to run in it.
        """
        rng, query, n = self.rng, self.client.query, self.args.samples
        hundred = "INSERT INTO bench_events (kind, payload, at) VALUES " + ",".join(["('view', ?, ?)"] * 100)
        # (section, name, one round in how many, request)
        tests = [
            ("baseline", "health_live", 1, lambda: self.client.request("GET", "/health/live", authenticated=False)),
            ("baseline", "whoami", 1, lambda: self.client.request("GET", f"{PREFIX}/whoami")),
            ("reads", "select_1", 1, lambda: self.read("SELECT 1")),
            ("reads", "point_lookup", 1, lambda: self.read(
                "SELECT * FROM bench_customers WHERE id = ?", [rng.randint(1, self.customers)])),
            # One customer's orders, newest first: 5 on average, 20 at most.
            ("reads", "index_range_20_rows", 1, lambda: self.read(
                "SELECT id, status, created_at FROM bench_orders WHERE customer_id = ?"
                " ORDER BY created_at DESC LIMIT 20",
                [rng.randint(1, self.customers)])),
            ("reads", "join_order_items", 1, lambda: self.read(
                "SELECT o.id, p.title, i.qty, i.price_cents FROM bench_orders o"
                " JOIN bench_order_items i ON i.order_id = o.id JOIN bench_products p ON p.id = i.product_id"
                " WHERE o.id = ?",
                [rng.randint(1, self.orders)])),
            ("reads", "page_1000_rows", 1, lambda: self.read(
                "SELECT * FROM bench_orders WHERE id > ? ORDER BY id LIMIT 1000",
                [rng.randint(1, max(1, self.orders - 1000))])),
            ("writes", "insert_one_row", 1, lambda: query(
                self.db,
                [{"sql": "INSERT INTO bench_orders (customer_id, status, created_at) VALUES (?, 'pending', ?)",
                  "params": [rng.randint(1, self.customers), NOW]}],
                mode="write")),
            ("writes", "transaction_update_and_insert", 1, lambda: query(
                self.db,
                [{"sql": "UPDATE bench_orders SET status = 'paid' WHERE id = ?", "params": [rng.randint(1, self.orders)]},
                 {"sql": "INSERT INTO bench_events (kind, payload, at) VALUES ('purchase', ?, ?)",
                  "params": [json.dumps({"order": rng.randint(1, self.orders)}), NOW]}],
                mode="write",
                transaction=True)),
            ("writes", "insert_100_rows_one_statement", 5, lambda: query(
                self.db, [{"sql": hundred, "params": ['{"batch":true}', NOW] * 100}], mode="write")),
        ]
        print(f"== latency ({n} rounds, one request of every test per round, one connection)")
        samples = {name: [] for _, name, _, _ in tests}
        started = time.perf_counter()
        for round_number in range(n):
            for _, name, every, call in tests:
                if round_number % every == 0:
                    samples[name].append(call().ok(name).ms)
        self.result["latency_rounds"] = {"rounds": n, "seconds": round(time.perf_counter() - started, 1)}
        round_trip = summary(samples["health_live"])
        for section, name, _, _ in tests:
            stats = summary(samples[name])
            if name != "health_live":
                stats["baseline"] = round_trip
            self.result.setdefault(section, {})[name] = stats
            print(f"{name}: {line(stats)}")

    def aggregates(self):
        print("== aggregates over the whole data set (sequential)")
        for name, sql in AGGREGATES.items():
            self.measure("aggregates", name, lambda sql=sql: self.read(sql, timeout_ms=30000), self.args.heavy_samples)

    def concurrent(self):
        print("== concurrent connections (each thread has its own connection)")
        per_thread = max(10, self.args.samples // 4)
        self.concurrency(
            "concurrency", "health_live", [1, 2, 4, 8, 16], per_thread, lambda client, rng: self.health_response(client)
        )
        self.concurrency(
            "concurrency",
            "point_lookup",
            [1, 2, 4, 8, 16],
            per_thread,
            lambda client, rng: client.query(
                self.db, [{"sql": "SELECT * FROM bench_customers WHERE id = ?", "params": [rng.randint(1, self.customers)]}]
            ),
        )
        self.concurrency(
            "concurrency",
            "insert_one_row",
            [1, 2, 4, 8],
            per_thread,
            lambda client, rng: client.query(
                self.db,
                [{"sql": "INSERT INTO bench_events (kind, payload, at) VALUES ('view', '{}', ?)", "params": [NOW]}],
                mode="write",
            ),
        )

    @staticmethod
    def health_response(client):
        return client.request("GET", "/health/live", authenticated=False)

    def routes(self):
        print("== other routes")
        n = max(10, self.args.samples // 20)
        self.measure("routes", "schema", lambda: self.client.request("GET", self.path("/schema")), n)
        self.measure(
            "routes",
            "rows_page_1000",
            lambda: self.client.request("GET", self.path("/tables/bench_orders/rows?limit=1000")),
            n,
        )
        base, runs = self.baseline(), []
        for _ in range(3):
            exported = self.client.request("GET", self.path("/export")).ok("export")
            runs.append({"bytes": exported.size, "seconds": round(exported.ms / 1000, 3)})
        best = min(runs, key=lambda r: r["seconds"])
        self.result["export"] = {"runs": runs, "baseline": base}
        print(
            f"export: {best['bytes'] / 1e6:.1f} MB, fastest of 3 in {best['seconds']:.2f} s"
            f" ({best['bytes'] / 1e6 / best['seconds']:.0f} MB/s), slowest"
            f" {max(r['seconds'] for r in runs):.2f} s"
        )

    def limits(self):
        """Sends one request past each limit and records the answer."""
        print("== limits")
        observed = {}

        def expect(name, limit, call, status, code):
            try:
                response = call()
                got = {"status": response.status, "code": response.code}
            except (http.client.HTTPException, OSError):
                got = {"status": None, "code": "connection_closed"}
            got.update(limit=limit, expected=f"{status} {code}", ok=got["status"] == status and got["code"] == code)
            observed[name] = got
            print(f"{name} ({limit}) -> {got['status']} {got['code']} ({'ok' if got['ok'] else 'UNEXPECTED'})")

        query = lambda *a, **k: self.client.query(self.db, *a, **k)
        expect("statements", "100 per request", lambda: query([{"sql": "SELECT 1"}] * 101), 422, "validation_failed")
        expect(
            "parameters",
            "999 per statement",
            lambda: query([{"sql": "SELECT " + ",".join(["?"] * 1000), "params": [1] * 1000}]),
            422,
            "validation_failed",
        )
        # The gateway refuses this one while the body is still on its way,
        # so the answer can depend on timing: ask a few times.
        oversized = json.dumps({"statements": [{"sql": "SELECT length(?)", "params": ["x" * (1 << 20)]}]}).encode()
        answers = {}
        for _ in range(5):
            client = Client(self.server, self.key)
            try:
                response = client.upload(self.path("/query"), oversized)
                answer = f"{response.status} {response.code or ''}".rstrip()
            except (http.client.HTTPException, OSError):
                answer = "connection_closed"
            finally:
                client.close()
            answers[answer] = answers.get(answer, 0) + 1
        observed["request_body"] = {
            "limit": "1 MiB",
            "answers": answers,
            "expected": "413 body_limit",
            "ok": list(answers) == ["413 body_limit"],
        }
        print(f"request_body (1 MiB), 5 tries -> {answers} ({'ok' if observed['request_body']['ok'] else 'UNEXPECTED'})")
        expect(
            "result_rows",
            "10 000 per statement",
            lambda: query([{"sql": "SELECT id FROM bench_events LIMIT 10001"}]),
            422,
            "result_too_large",
        )
        expect(
            "result_bytes",
            "16 MiB per response",
            lambda: query(
                [{"sql": "WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM s WHERE i < 9000)"
                         " SELECT hex(zeroblob(1000)) FROM s"}]
            ),
            422,
            "result_too_large",
        )
        expect(
            "timeout_ceiling",
            "30 s per statement",
            lambda: query([{"sql": "SELECT 1"}], timeout_ms=30001),
            422,
            "validation_failed",
        )
        runaway = "WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM s) SELECT count(*) FROM s"
        started = time.perf_counter()
        expect(
            "statement_deadline",
            "1 s asked for, on a query that never ends",
            lambda: query([{"sql": runaway}], timeout_ms=1000),
            422,
            "statement_timeout",
        )
        observed["statement_deadline"]["answered_after_ms"] = round((time.perf_counter() - started) * 1000)
        self.result["limits"] = observed

    # ------------------------------------------------------------ run

    def run(self):
        self.prepare()
        self.connection_costs()
        self.identity()
        self.refusals()
        self.schema()
        self.ingest()
        self.bulk()
        self.volumes()
        self.latencies()
        self.aggregates()
        self.concurrent()
        self.routes()
        self.limits()
        self.result["data"]["size_bytes_at_end"] = self.size()
        print(f"database size at the end: {self.result['data']['size_bytes_at_end'] / 1e6:.1f} MB")


def load_average():
    """The client machine's 1-minute load, to tell a busy client from a slow server."""
    try:
        return round(os.getloadavg()[0], 2)
    except (AttributeError, OSError):
        return None


def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def write_json(path, value):
    directory = os.path.dirname(os.path.abspath(path))
    os.makedirs(directory, exist_ok=True)
    handle, temporary = tempfile.mkstemp(dir=directory, prefix=".bench-")
    with os.fdopen(handle, "w") as out:
        json.dump(value, out, indent=1)
        out.write("\n")
    os.chmod(temporary, 0o644)
    os.replace(temporary, path)


def main():
    parser = argparse.ArgumentParser(description="Benchmark one memoire database over the HTTP API.")
    parser.add_argument("--db", default="bench", help="database to create and delete (default: bench)")
    parser.add_argument("--scale", type=float, default=1.0, help="data volume multiplier (default: 1)")
    parser.add_argument("--samples", type=int, default=1000, help="requests per latency test (default: 1000)")
    parser.add_argument("--heavy-samples", type=int, default=20, help="runs per aggregate query (default: 20)")
    parser.add_argument("--ttl", type=int, default=3600, help="idle TTL of the database in seconds (default: 3600)")
    parser.add_argument("--out", default="bench-result.json", help="JSON result file (default: bench-result.json)")
    parser.add_argument("--keep", action="store_true", help="leave the database in place at the end")
    args = parser.parse_args()
    if not 0.01 <= args.scale <= 1000 or args.samples < 20 or args.heavy_samples < 1:
        parser.error("--scale must be between 0.01 and 1000, --samples at least 20 and --heavy-samples at least 1")

    key = os.environ.get("MEMOIRE_API_KEY")
    if not key:
        sys.exit("MEMOIRE_API_KEY is not set")
    server = urllib.parse.urlsplit(os.environ.get("MEMOIRE_SERVER", "https://app.memoire.network"))
    credentials = server.username is not None or server.password is not None
    if server.scheme not in ("http", "https") or not server.hostname or credentials:
        sys.exit("MEMOIRE_SERVER must be an http or https URL without credentials")

    bench, status = Bench(args, server, key), 0
    try:
        bench.run()
    except BenchError as error:
        bench.result["error"] = str(error)
        print(f"FAIL {error}", file=sys.stderr)
        status = 1
    except (http.client.HTTPException, OSError) as error:
        bench.result["error"] = f"connection failed: {type(error).__name__}"
        print(f"FAIL connection failed: {type(error).__name__}", file=sys.stderr)
        status = 1
    except KeyboardInterrupt:
        bench.result["error"] = "interrupted"
        status = 130
    finally:
        bench.cleanup()
    bench.result["finished_at"] = utc_now()
    bench.result["client"]["load_average_1m_at_end"] = load_average()
    bench.result["reconnects"] = bench.reconnects + bench.client.reconnects
    write_json(args.out, bench.result)
    print(f"wrote {args.out}")
    return status


if __name__ == "__main__":
    sys.exit(main())
