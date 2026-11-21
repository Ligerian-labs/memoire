# Benchmarks

One client timed the hosted service at `https://app.memoire.network` on
21 November 2026 with [`scripts/bench.py`](../scripts/bench.py). This page
gives the method, every number the script produced, the limits that were in
force and what the numbers do not show. The raw results are in
[`docs/benchmarks/`](benchmarks/), and [Run it yourself](#run-it-yourself)
has the command.

Every latency here is measured at the client, so it includes the network.
The client was about 3 ms from the server. A client farther away adds its
own round trip to each request.

Latency per request in milliseconds, at scale 1 (1.4 million rows, one
burstable database), 3 000 requests per row unless noted:

| Request | p50 | p95 | p99 |
| --- | ---: | ---: | ---: |
| Round trip alone: `GET /health/live`, no key, no database | 2.8 | 8.4 | 18.0 |
| Credential check alone: `GET /v1/memoire/whoami` | 7.7 | 18.9 | 42.1 |
| `SELECT 1` | 12.1 | 28.7 | 55.5 |
| Read one row by primary key | 13.0 | 30.5 | 55.6 |
| Read 20 rows through an index | 13.2 | 31.4 | 68.0 |
| Join three tables for one order | 13.2 | 31.5 | 64.8 |
| Read a page of 1 000 rows | 14.1 | 32.9 | 72.7 |
| Insert one row | 13.3 | 31.2 | 79.5 |
| Update and insert in one transaction | 13.3 | 33.1 | 68.1 |
| Insert 100 rows in one statement (600 requests) | 13.7 | 33.0 | 55.0 |

In short, a small request takes about 13 ms from 3 ms away, whatever it
reads or writes. `SELECT 1` costs almost as much as a point
read, so most of that time is the request itself (the round trip, the
credential check, finding the database) and 1 to 2 ms is SQLite. Queries
that scan a lot of data cost what SQLite costs: a join and group over
100 000 orders took 1.8 s.

## Method

### The server

A single VPS behind the platform's proxy. The proxy terminates TLS and
forwards to the gateway. The SQLite files are on the server's local disk.
The hosted service deploys from `main`, which was at commit `6d88eba`
during the runs. The gateway opens every database in WAL mode with
`synchronous = normal`, and gives each database one writer connection and a
pool of four read-only connections
([architecture](architecture.md#data-plane-rules)).

The service kept serving its other users during the runs. It is invite-only
and small, so that load was light, but nobody measured it.

### The client

One Linux machine in a datacenter. Five `ping` checks of 20 packets each
over the day lost no packet and averaged between 0.9 and 3.9 ms. The round
trip of a whole HTTP request was 2.8 ms at the median.

The script is one Python 3.12 process that uses only the standard library
(`http.client`): HTTP/1.1 with keep-alive, a team API key as the
credential. The machine has 4 cores and was doing other work during the
runs. Its 1-minute load average was between 1.5 and 2.8, and the result
files record it. A busy client inflates what it measures, so read the tails
as upper bounds on what the server did.

### What a latency includes

The clock starts before the client writes the request and stops when it
has read the whole response, on a connection that is already open. A
latency therefore includes:

- the network round trip and the proxy,
- the gateway checking the API key and finding the database,
- SQLite running the statements,
- JSON encoding on the server and the transfer of the response.

It excludes opening the connection (TCP and TLS, measured
[separately](#opening-a-connection)), building the request body and parsing
the response in Python.

`GET /health/live` needs no key and touches no database. It is the round
trip through the proxy to the gateway and back, and nothing else, so the
script uses it as the baseline:

- The request-by-request tests run interleaved. Each of the 3 000 rounds
  sends one `GET /health/live` and one request of every test. A slow minute
  on the network, the server or the client then lands on every test and on
  the baseline alike. Two trial runs with the tests back to back gave
  similar medians but tails that jumped from test to test, which is why the
  script interleaves.
- Before each of the other tests (aggregates, loading, the other routes)
  the script times 100 round trips on the same connection. The tables show
  that median as "round trip".

Percentiles are nearest-rank over all samples. No warm-up request is
discarded and no outlier is removed. Tables show p99 only for tests with at
least 600 samples.

### The data

One burstable database, created by the script and deleted at the end. The
data is synthetic, generated from a fixed seed. At scale 1:

| Table | Rows | How it is loaded |
| --- | ---: | --- |
| `bench_customers` | 20 000 | multi-row `INSERT` over HTTP |
| `bench_products` | 2 000 | multi-row `INSERT` over HTTP |
| `bench_orders` | 100 000 | multi-row `INSERT` over HTTP |
| `bench_order_items` | 299 459 | multi-row `INSERT` over HTTP |
| `bench_events` | 1 000 000 | generated inside SQLite by one recursive statement |

That is 1 421 459 rows in five tables with five indexes. After the
load the service reported 158.4 MB for the database, which counts the file
and its write-ahead log. The exported file was 100.5 MB at the end of the
run, including the rows the write tests added.

## Results at scale 1

Two runs, one after the other, from 14:09 to 14:33 UTC. The first is the
headline and fills the tables. The second is there to show how much a run
moves. Raw results:
[`2026-11-21-scale-1.json`](benchmarks/2026-11-21-scale-1.json) and
[`2026-11-21-scale-1-repeat.json`](benchmarks/2026-11-21-scale-1-repeat.json).

### Request latency

Milliseconds. 3 000 interleaved rounds on one open connection took 395 s.

| Request | n | p50 | p95 | p99 | max |
| --- | ---: | ---: | ---: | ---: | ---: |
| Round trip alone: `GET /health/live` | 3 000 | 2.8 | 8.4 | 18.0 | 87.3 |
| Credential check alone: `GET /v1/memoire/whoami` | 3 000 | 7.7 | 18.9 | 42.1 | 327.2 |
| `SELECT 1` | 3 000 | 12.1 | 28.7 | 55.5 | 527.8 |
| Read one row by primary key | 3 000 | 13.0 | 30.5 | 55.6 | 766.8 |
| Read 20 rows through an index | 3 000 | 13.2 | 31.4 | 68.0 | 247.8 |
| Join three tables for one order | 3 000 | 13.2 | 31.5 | 64.8 | 351.8 |
| Read a page of 1 000 rows | 3 000 | 14.1 | 32.9 | 72.7 | 266.0 |
| Insert one row | 3 000 | 13.3 | 31.2 | 79.5 | 313.0 |
| Update and insert in one transaction | 3 000 | 13.3 | 33.1 | 68.1 | 331.7 |
| Insert 100 rows in one statement | 600 | 13.7 | 33.0 | 55.0 | 667.0 |

Reads use `"mode": "read"` and writes `"mode": "write"`, one request per
statement batch, each in its own implicit or explicit transaction.

Where the 13 ms goes, from the medians above:

| Step | p50 | Added by the step |
| --- | ---: | ---: |
| Round trip through the proxy | 2.8 | 2.8 |
| Plus the credential check (`whoami`) | 7.7 | 4.9 |
| Plus finding the database and running `SELECT 1` | 12.1 | 4.4 |
| Plus reading one row by primary key | 13.0 | 0.9 |

Subtracting medians is a rough guide, not a measurement of each step. It
still says where to look: about 12 ms of a small request is spent before
SQLite sees a table.

The same tests in the repeat run:

| Request | p50 | p50, repeat | p99 | p99, repeat |
| --- | ---: | ---: | ---: | ---: |
| Round trip alone: `GET /health/live` | 2.8 | 3.0 | 18.0 | 18.1 |
| Credential check alone: `GET /v1/memoire/whoami` | 7.7 | 8.7 | 42.1 | 43.2 |
| `SELECT 1` | 12.1 | 13.8 | 55.5 | 73.3 |
| Read one row by primary key | 13.0 | 14.6 | 55.6 | 66.8 |
| Read 20 rows through an index | 13.2 | 14.9 | 68.0 | 74.0 |
| Join three tables for one order | 13.2 | 14.8 | 64.8 | 73.1 |
| Read a page of 1 000 rows | 14.1 | 15.8 | 72.7 | 75.5 |
| Insert one row | 13.3 | 15.1 | 79.5 | 71.5 |
| Update and insert in one transaction | 13.3 | 14.9 | 68.1 | 67.5 |
| Insert 100 rows in one statement | 13.7 | 15.5 | 55.0 | 69.0 |

Between the two runs the medians moved by up to 14% and the p99 by up to
32%. Take the medians as good to about 2 ms and the tails as rough.

### Aggregates over the whole data set

Milliseconds. Each query ran 20 times in a row with a 30 s deadline.

| Query | min | p50 | max | p50, repeat | Round trip |
| --- | ---: | ---: | ---: | ---: | ---: |
| Revenue by country: orders joined to customers and items, grouped | 1 650 | 1 821 | 2 376 | 1 668 | 2.1 |
| Top 10 products: items joined to products, grouped and sorted | 432 | 515 | 1 228 | 407 | 2.8 |
| Count events by kind, from an index on 1 000 000 rows | 79 | 101 | 165 | 90 | 2.3 |
| Count events by a JSON field, full scan of 1 000 000 rows | 363 | 405 | 572 | 398 | 2.5 |

The default deadline is 5 s per statement. All four fit in it at this scale.
The [scale 4 run](#results-at-scale-4) shows what happens when they do not.

### Several connections at once

Each thread has its own connection, opened and used once before the clock
starts, and sends 750 requests. All threads are in one Python process, so
the client limits what it can send. The first table shows that ceiling:
the same threads calling `GET /health/live`.

`GET /health/live`:

| Connections | Requests | Requests/s | p50 | p95 | p99 | Requests/s, repeat |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 750 | 499 | 1.7 | 3.9 | 5.3 | 566 |
| 2 | 1 500 | 757 | 2.2 | 5.0 | 7.7 | 1 298 |
| 4 | 3 000 | 1 423 | 2.3 | 5.7 | 8.6 | 1 955 |
| 8 | 6 000 | 1 790 | 3.7 | 9.3 | 14.9 | 2 109 |
| 16 | 12 000 | 2 915 | 4.5 | 11.9 | 16.8 | 2 510 |

Read one row by primary key:

| Connections | Requests | Requests/s | p50 | p95 | p99 | Requests/s, repeat |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 750 | 84 | 10.9 | 19.3 | 27.0 | 83 |
| 2 | 1 500 | 134 | 13.5 | 25.7 | 34.8 | 76 |
| 4 | 3 000 | 167 | 17.2 | 56.8 | 133.2 | 148 |
| 8 | 6 000 | 322 | 21.8 | 47.2 | 69.0 | 279 |
| 16 | 12 000 | 354 | 34.1 | 103.1 | 215.1 | 382 |

Insert one row (a database has one writer, so inserts queue):

| Connections | Requests | Requests/s | p50 | p95 | p99 | Requests/s, repeat |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 750 | 74 | 12.6 | 21.6 | 30.2 | 78 |
| 2 | 1 500 | 127 | 14.3 | 26.7 | 35.4 | 142 |
| 4 | 3 000 | 213 | 15.6 | 35.8 | 69.0 | 150 |
| 8 | 6 000 | 199 | 26.5 | 102.0 | 252.4 | 280 |

No request failed in any of these: no `sqlite_busy`, no `429`, no `503`.
Reads level off near 350 to 380 requests a second on 16 connections, with
the median latency close to three times the single-connection one. Inserts
level off between 200 and 280 a second. Both are far below the
`/health/live` ceiling, so the client was probably not the limit. Throughput
moved a lot between the two runs (see the 2-connection read row), so treat
these as the right order of magnitude and no more.

### Loading data

Rows sent over HTTP as multi-row `INSERT` statements, as many as fit under
999 parameters per statement, 100 statements and 1 MiB per request, one
transaction per request. Seconds are the sum of the request times: they
include the upload and the insert, and exclude building the JSON in Python.

| Table | Rows | Requests | Body, MB | Seconds | Rows/s | Rows/s, repeat |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `bench_customers` | 20 000 | 2 | 1.6 | 0.13 | 157 360 | 127 787 |
| `bench_products` | 2 000 | 1 | 0.1 | 0.02 | 114 843 | 63 930 |
| `bench_orders` | 100 000 | 6 | 4.7 | 1.22 | 82 280 | 74 612 |
| `bench_order_items` | 299 459 | 13 | 9.7 | 4.10 | 73 117 | 41 091 |
| All four | 421 459 | 22 | 16.2 | 5.46 | 77 261 | 47 806 |

Generated inside SQLite, where no row crosses the network:

| Work | First run | Repeat |
| --- | ---: | ---: |
| `INSERT` of 1 000 000 events from a recursive query, one statement | 1.64 s, 609 346 rows/s | 2.15 s, 465 057 rows/s |
| `CREATE INDEX` on two columns of those 1 000 000 rows | 0.95 s | 0.78 s |
| `CREATE` of 5 tables and 4 indexes in one transaction | 11 ms | 37 ms |
| Create the database (`POST /v1/memoire/databases`) | 37 ms | 80 ms |

### Other routes

| Route | n | p50 | p95 | max | p50, repeat | Round trip |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `GET {name}/schema` (5 tables) | 150 | 18.9 | 45.4 | 94.5 | 15.9 | 3.1 |
| `GET {name}/tables/bench_orders/rows?limit=1000` | 150 | 20.2 | 49.8 | 97.5 | 12.6 | 3.0 |

`GET {name}/export` returned the 100.5 MB file three times per run, in 6.7,
7.6 and 8.9 s in the first run and 5.4, 6.7 and 7.2 s in the repeat. That is
11 to 19 MB/s. The client cannot tell how much of that is the backup on the
server and how much is the transfer through the proxy.

### Opening a connection

Every latency above is on an open connection. A new one costs this, in
milliseconds, 20 tries per run:

| Step | p50 | max | p50, repeat |
| --- | ---: | ---: | ---: |
| TCP connect | 3.0 | 4.0 | 2.9 |
| TCP and TLS | 15.7 | 21.1 | 19.7 |
| TCP, TLS and a first `GET /health/live` | 18.1 | 23.7 | 23.6 |

A client that opens a connection per request, as a command-line call does,
pays the TCP and TLS setup on top of every figure. That makes a point read
about 29 ms instead of 13.

## Results at scale 4

One run with four times the data, from 14:33 to 14:54 UTC: 5 686 465 rows.
The service reported 471.6 MB after the load and 475.7 MB at the end, file
and write-ahead log together. The exported file was 395.9 MB, 37% of the
1 GiB limit. Raw result:
[`2026-11-21-scale-4.json`](benchmarks/2026-11-21-scale-4.json).

Request latency, 3 000 interleaved rounds:

| Request | n | p50 | p95 | p99 | max |
| --- | ---: | ---: | ---: | ---: | ---: |
| Round trip alone: `GET /health/live` | 3 000 | 3.1 | 10.5 | 22.8 | 84.3 |
| Credential check alone: `GET /v1/memoire/whoami` | 3 000 | 9.1 | 26.2 | 60.0 | 338.6 |
| `SELECT 1` | 3 000 | 14.1 | 42.5 | 94.2 | 428.3 |
| Read one row by primary key | 3 000 | 15.1 | 42.0 | 91.8 | 422.3 |
| Read 20 rows through an index | 3 000 | 15.1 | 43.5 | 91.2 | 408.4 |
| Join three tables for one order | 3 000 | 15.3 | 44.1 | 99.4 | 408.6 |
| Read a page of 1 000 rows | 3 000 | 16.4 | 44.0 | 93.0 | 363.2 |
| Insert one row | 3 000 | 15.2 | 44.6 | 89.8 | 328.6 |
| Update and insert in one transaction | 3 000 | 15.5 | 45.7 | 104.1 | 337.9 |
| Insert 100 rows in one statement | 600 | 16.0 | 41.7 | 70.3 | 484.0 |

Every row is slower than at scale 1, and so are the round trip and the
credential check, which touch no table. The gap between `SELECT 1` and a
point read is still 1 ms. So this run was a slower stretch for the whole
path, and nothing here shows indexed reads or small writes slowing down
with four times the data.

The aggregates do grow, and faster than the data:

| Query | min | p50 | max | Times the scale 1 p50 |
| --- | ---: | ---: | ---: | ---: |
| Revenue by country: orders joined to customers and items, grouped | 11 366 | 13 636 | 20 638 | 7.5 |
| Top 10 products: items joined to products, grouped and sorted | 3 454 | 3 978 | 7 474 | 7.7 |
| Count events by kind, from an index on 4 000 000 rows | 285 | 322 | 457 | 3.2 |
| Count events by a JSON field, full scan of 4 000 000 rows | 1 479 | 1 665 | 2 418 | 4.1 |

The first query passes the 5 s default deadline at this size. The script
asks for 30 s, the most a request may ask for, and the slowest run used
20.6 s of it. Past 30 s a statement fails with `statement_timeout`.

Several connections at once, 750 requests per connection:

| Test | Connections | Requests/s | p50 | p95 | p99 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `GET /health/live` | 16 | 2 245 | 5.9 | 15.5 | 22.0 |
| Read one row by primary key | 1 | 79 | 11.7 | 19.4 | 27.6 |
| Read one row by primary key | 4 | 119 | 19.3 | 99.8 | 236.8 |
| Read one row by primary key | 8 | 286 | 23.4 | 56.2 | 89.6 |
| Read one row by primary key | 16 | 332 | 37.4 | 105.0 | 192.7 |
| Insert one row | 1 | 66 | 13.2 | 25.8 | 37.5 |
| Insert one row | 4 | 194 | 17.6 | 39.4 | 64.4 |
| Insert one row | 8 | 228 | 25.3 | 81.2 | 200.9 |

Loading and export:

| Work | Result |
| --- | --- |
| 1 686 465 rows over HTTP, 80 requests, 66.8 MB of JSON | 46.9 s, 35 973 rows/s |
| 4 000 000 events generated inside SQLite, 4 statements | 11.8 s, 340 259 rows/s |
| `CREATE INDEX` on two columns of 4 000 000 rows | 4.4 s |
| Export of the 395.9 MB file, three times | 41.7, 24.6 and 24.1 s (9 to 16 MB/s) |

One keep-alive connection dropped during this run. The script reconnected
and retried that request once, and its time is in the figures.

## Limits

These limits were in force during the runs. They are the contract's values
([`docs/api.md`](api.md), `crates/memoire-api/src/lib.rs`). The script
sends one request past each limit it can reach cheaply and records the
answer.

| Limit | Value | Past it, the script saw |
| --- | --- | --- |
| Statements per request | 100 | `422 validation_failed` for 101 |
| Parameters per statement | 999 | `422 validation_failed` for 1 000 |
| Request body | 1 MiB | `413 body_limit` 14 times in 15, `502` once (see below) |
| Rows one statement returns | 10 000 | `422 result_too_large` for 10 001 |
| Size of one query response | 16 MiB | `422 result_too_large` for about 18 MB |
| Deadline per statement | 5 s by default, 30 s at most | `422 validation_failed` for 30 001 ms |
| A statement that never ends | deadline of 1 s asked for | `422 statement_timeout` after 1.01 s |
| Database size | 1 GiB | not tested; the largest run reached 476 MB |
| Text of one statement | 100 KiB | not tested |
| Active databases per team | 50 | not tested |
| Rows per page of the row browser | 1 000 | used at the limit, not past it |

The script also checks three refusals: a write in `read` mode
(`422 read_only`), `ATTACH` and `BEGIN` (`403 statement_denied`). All three
answered as the contract says in every run.

About the `502`. The gateway refuses an oversized body as soon as it sees
it is too large, while the client is still uploading. Through the proxy
that race sometimes ends in `502 Bad Gateway` instead of `413 body_limit`.
It happened once in the 15 oversized requests of the published runs, and
more often in trial runs. Either way the request is refused and nothing
runs. A client should read a `502` on an upload over 1 MiB as the same
refusal.

## What the numbers do not show

- **Capacity.** One client, one database, one server. Nothing here says how
  many teams, databases or requests the server carries at once, or what
  happens when several databases are busy together.
- **The server's size.** The server is described only as a single VPS, so
  these are not per-core numbers and they will not transfer to other
  hardware.
- **Your network.** The client was 3 ms away in a datacenter. The next
  section shows the same service from a home connection.
- **Where the tails come from.** The p95 and p99 mix the server, the proxy,
  the network and a client that was busy with other work. The script cannot
  separate them. The round trip alone had a p99 of 18 ms, six times its
  median, without touching the database.
- **A cold database.** A burstable database's connections close after 5
  minutes idle and reopen on the next request. Every test here ran on a
  database in use. The first request after an idle period is not timed.
- **Persistent databases.** The run used a burstable database. Snapshots,
  restore and the cost of a snapshot running during queries are not
  measured.
- **Import, the memory routes and the CLI.** Not measured. The CLI opens a
  connection per command, so add the [connection cost](#opening-a-connection).
- **Long runs.** The longest run lasted 21 minutes. Nothing here covers
  hours of load, a large write-ahead log or a database near the 1 GiB limit.
- **Another workload.** Rows here are small, about 100 bytes or less. Large values,
  wide rows, full-text search and write-heavy mixes will behave differently.
- **Other clients.** One Python process with threads. A faster client might
  push more requests a second at the higher connection counts, though the
  `/health/live` ceiling suggests the client was not the limit for
  database requests.

## The same service from a home connection

A first version of the script ran the same day from a laptop on a
residential connection: 3.3% packet loss and 13 to 99 ms of ping to the
server. That version ran the tests back to back with 200 requests each, so
its numbers are not comparable line by line. They show what a poor network
does to the figures above.

| Request | p50 | p95 | p99 |
| --- | ---: | ---: | ---: |
| Round trip alone (20 requests) | 66.7 | 291.6 | 411.9 |
| Read one row by primary key | 37.9 | 480.8 | 1 006.8 |
| Read a page of 1 000 rows | 82.4 | 1 240.7 | 3 677.3 |
| Insert one row | 85.7 | 604.4 | 3 895.7 |

The export of the 96.5 MB file took 260.6 s there. The gateway logs every
request that takes a second or more, with its duration on the server. For
that run it logged the export at 2.3 s, and otherwise only the aggregate
queries, the large loading requests and the deliberate 1 s timeout. It
logged no point read and no single insert. The multi-second tails in that
table were the network. The raw output of that run is not in the
repository.

## Run it yourself

The script needs Python 3 and a team API key with the read, write, create
and delete database permissions. It reads the key from the environment,
never from an argument.

```sh
export MEMOIRE_SERVER=https://app.memoire.network
export MEMOIRE_API_KEY="$(cat bench.key)"
scripts/bench.py --scale 1 --samples 3000 --out bench-result.json
```

That is the command behind the scale 1 results; `--scale 4` gives the
larger run. A run at scale 1 takes about 12 minutes from nearby.

The script creates a burstable database named `bench`, fills it, measures,
writes the result file and deletes the database. If a database of that
name already exists, the script replaces it only when it is burstable and
holds nothing but `bench_` tables. Otherwise it stops without touching it.
`--db NAME` picks another name, `--samples N` sets the requests per latency
test (default 1 000) and `--keep` leaves the database in place. The output
and the result file hold no id and no key.

It also runs against a development gateway, which is a quick way to try
it. `scripts/dev-run.sh` starts one and `scripts/dev-owner.py` makes a key:

```sh
export MEMOIRE_SERVER=http://127.0.0.1:28960
export MEMOIRE_API_KEY=$(jq -r .api_key .memoire-dev/owner.json)
scripts/bench.py --scale 0.1 --samples 100
```

Your numbers will differ from these. The round trip in your own result
file tells you by how much the network alone moves them.
