# Sparkles HTTP API

The server speaks the **Fuseki** protocol surface (so existing Jena tooling —
`rdfconnection`, `s-query`, YASGUI, etc. — works unchanged) plus a small set of
`/$/…` extensions used by the web UI.

All admin endpoints live under `/$/`. Dataset names match `[A-Za-z0-9_.-]+` and are
addressed as `/{ds}`. JSON responses use `application/json`.

Without `sparkles serve --auth-config` the server is open, as described below. With it,
every route needs credentials or a grant to `anonymous`; see
[Authentication and access control](#authentication-and-access-control).

## Server

| Method | Path          | Description |
|--------|---------------|-------------|
| GET    | `/$/ping`     | Plain-text timestamp. Liveness check: `200` whenever the process serves HTTP. |
| GET    | `/$/ready`    | Readiness: `200` when ready, else `503`; the body is always `ReadyInfo`. `Cache-Control: no-store`. |
| GET    | `/$/ready/{ds}` | The same for one dataset (`datasets` has one entry); `404` if the dataset is unknown. |
| GET    | `/$/server`   | `{ "version", "startedAt", "uptimeSeconds", "readOnly", "datasets": [DatasetInfo], "limits": Limits, "auth": { "enabled": boolean } }`; anonymous callers of a server with auth get no `version` or `limits` |
| GET    | `/$/whoami`   | The caller and its permissions (see [whoami](#whoami)). |
| POST   | `/$/format`   | Format a SPARQL query or update; see [Formatting](#formatting). |
| GET    | `/$/metrics`  | Prometheus text format 0.0.4 (`text/plain; version=0.0.4`), see [Metrics](#metrics). `?format=json` returns a JSON snapshot of the same counters (`MetricsSnapshot`) for the UI. `404` when started with `--no-metrics`. |

```ts
type ReadyInfo = {
  status: "starting" | "ready" | "draining";   // draining: shutting down (SIGINT / SIGTERM)
  ready: boolean;
  uptimeSeconds: number;
  datasets: {
    name: string; type: "persistent" | "mem";
    state: "open"; ready: boolean;
    generation?: string;   // persistent datasets: the index generation in use
    walBytes: number;      // size of the write-ahead log (0 for mem)
    deltaQuads: number;    // inserts + deletes not yet compacted
  }[];
};

// 0 means unlimited
type Limits = { timeoutSeconds: number; updateTimeoutSeconds: number; maxTimeoutSeconds: number; queryMemoryBytes: number; maxResultBytes: number; maxExportBytes: number; maxRows: number; maxQueryBodyBytes: number; maxUpdateBodyBytes: number; maxAdminBodyBytes: number; maxUploadBytes: number };
```

### Request ids and the access log

Every response carries `X-Request-Id` (exposed to browsers through CORS). An incoming
`X-Request-Id` of 1–128 characters from `[A-Za-z0-9._:-]` is kept; otherwise the server
generates one (`{boot:08x}-{seq:012x}`, unique per process and increasing). The id is the
`request_id` field of the request's log span, so every log line of the request carries it.

`sparkles serve` logs one INFO line per completed request under the target
`sparkles::access` (`--no-access-log` turns it off; `/ui/*`, `/$/ping`, `/$/ready` and
`/$/metrics` are logged at DEBUG). Fields: `dataset` (or `$none`), `operation` (`query`,
`update`, `gsp`, `upload`, `shacl`, `shex`, `explain`, `admin`, `other`), `status`, `outcome`
(`ok`, `client_error`, `error`, `timeout`, `cancelled`, `budget`, `rate_limited`, `denied`,
`rejected`: a write refused by write-time validation),
with auth the `principal` (`user:bob`, `token:tok_…`, `oidc:…`, `proxy:…`, `anonymous`; never
a credential), `auth` (`none`, `basic`, `bearer`, `session`, `proxy`) and, for a failed
login, `auth_error`, and where known `rows`,
`parse_ms`, `plan_ms`, `exec_ms`, `serialize_ms`, `total_ms`, `response_bytes` and
`mem_peak_bytes`; writes to a validated dataset add `validation` (the status of the
`Sparkles-Validation` header) and `validation_ms`. A request whose client disconnects is logged with `status=499` and
`outcome=cancelled` (never sent). The span holds the matched route (`/{ds}/sparql`), never
the raw URI; query and update text is logged only at DEBUG under `sparkles::query`
(`RUST_LOG=sparkles::query=debug`), cut to 2048 characters. `--log-format json` writes one
JSON object per line.

### Metrics

| Name | Type | Labels |
|------|------|--------|
| `sparkles_build_info` | gauge (1) | `version` |
| `sparkles_start_time_seconds` | gauge | |
| `sparkles_ready` | gauge (0/1) | |
| `sparkles_requests_total` | counter | `dataset`, `operation`, `outcome` |
| `sparkles_request_duration_seconds` | histogram (1 ms … 300 s) | `dataset`, `operation` |
| `sparkles_requests_active` | gauge | `operation` |
| `sparkles_response_bytes_total` | counter (uncompressed) | `dataset`, `operation` |
| `sparkles_result_rows_total` | counter | `dataset` |
| `sparkles_budget_exceeded_total` | counter | `dataset`, `budget` |
| `sparkles_dataset_quads`, `sparkles_wal_bytes`, `sparkles_disk_bytes` | gauge | `dataset` |
| `sparkles_delta_quads` | gauge | `dataset`, `kind` = `insert` \| `delete` |
| `sparkles_block_cache_bytes`, `…_capacity_bytes` | gauge | `dataset` |
| `sparkles_block_cache_hits_total`, `…_misses_total` | counter | `dataset` |
| `sparkles_result_cache_bytes`, `…_capacity_bytes`, `…_entries` | gauge | `dataset` |
| `sparkles_result_cache_hits_total`, `…_misses_total` | counter | `dataset` |
| `sparkles_validation_total` | counter | `dataset`, `language` = `shacl` \| `shex`, `status` = `passed` \| `warned` \| `rejected` \| `skipped` \| `bypassed` \| `timeout` \| `error` |
| `sparkles_validation_duration_seconds` | histogram (1 ms … 300 s) | `dataset`, `language`, `strategy` = `full` \| `incremental` |
| `sparkles_validation_results_total` | counter (results found by validated writes; ShEx: nonconformant associations, as `violation`) | `dataset`, `language`, `severity` = `violation` \| `warning` \| `info` |
| `sparkles_geo_rows` | gauge (rows of the spatial index) | `dataset`, `part` = `base` \| `overlay` \| `tail` |
| `sparkles_geo_build_seconds` | gauge (the last build of the index's base) | `dataset` |
| `sparkles_geo_candidates_total`, `sparkles_geo_refined_total`, `sparkles_geo_matches_total`, `sparkles_geo_rechecked_total` | counter (rows found by the index, exact geometry tests, rows that passed them, and the candidates the index could not place, over the spatial operators of queries) | `dataset` |
| `process_resident_memory_bytes` | gauge (Linux) | |

Label values are bounded: `dataset` is an existing dataset name (at most
`--metrics-max-datasets`, default 100; the others share `$other`) or `$none` for requests
that name no existing dataset. A (dataset, operation) pair appears after its first request,
then with all nine outcomes (`denied`: refused by the auth layer). Health checks (`/$/ping`, `/$/ready`), `/$/metrics` and UI assets
are not counted. The validation series cover every validated write (HTTP, MCP, reasoning
tasks) of datasets with write-time validation. Deleting a dataset removes its series. Each dataset has its own block and
result cache, each sized to the global `--cache-mb` / `--result-cache-mb`.

With rate limits configured (or authentication on), `sparkles_rate_limited_total{dataset,class}`
(counter) counts refused requests per limit class (`preauth` included), and
`outcome="rate_limited"` appears in `sparkles_requests_total`. The size of each limiter's
client state is in `sparkles_rate_limit_keys{limiter}`, `sparkles_rate_limit_max_keys{limiter}`,
`sparkles_rate_limit_evictions_total{limiter}` and `sparkles_rate_limit_penalties{limiter}`
(`limiter` is `requests`, or `auth` for the auth layer's own limits), and
`sparkles_rate_limit_untrusted_forwarded_total{limiter}` counts requests whose
`X-Forwarded-For` or `Forwarded` came from a peer that is not a trusted proxy (ignored).

Backup repositories add the `sparkles_backup_*` families listed under
[Backup repositories](#backup-metrics).

```ts
type MetricsSnapshot = {
  formatVersion: 1;
  version: string; uptimeSeconds: number; ready: boolean;
  processResidentBytes: number | null;
  limits: Limits;
  bucketBounds: number[];                  // histogram upper bounds in seconds, without +Inf
  active: Record<Operation, number>;       // requests in progress
  requests: {
    dataset: string; operation: Operation;
    outcomes: Record<Outcome, number>;
    count: number; sumSeconds: number;
    buckets: number[];                     // cumulative counts; the last (+Inf) equals count
    responseBytes: number;
  }[];
  datasets: {
    name: string; quads: number; deltaInserts: number; deltaDeletes: number;
    walBytes: number; diskBytes: number; resultRows: number;
    budgetExceeded: Record<"rows" | "memory" | "result-bytes", number> | null;
    rateLimited: Record<"auth" | "query" | "update" | "admin" | "preauth", number> | null;
    blockCache: { bytes: number; capacityBytes: number; entries: number; hits: number; misses: number };
    resultCache: { enabled: boolean; bytes: number; capacityBytes: number; entries: number; hits: number; misses: number };
    geo: null | {                          // the spatial index, or queries that ran spatial operators
      enabled: boolean;
      rows: { base: number; overlay: number; tail: number };
      buildSeconds: number | null;
      candidates: number; refined: number; matches: number; rechecked: number;
    };
  }[];
};
```

### OpenTelemetry

`sparkles serve` exports traces, metrics and (optionally) logs over OTLP. It is off by
default: nothing is exported and no connection is opened unless `--otel` is given or the
environment asks for it (`OTEL_EXPORTER_OTLP_ENDPOINT` or a signal-specific endpoint, or
`OTEL_TRACES_EXPORTER` / `OTEL_METRICS_EXPORTER` / `OTEL_LOGS_EXPORTER=otlp`).
`OTEL_SDK_DISABLED=true` turns it off again. Builds without the `otel` cargo feature (on by
default) have none of it.

| Variable | Meaning |
|----------|---------|
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_ENDPOINT` | collector address (default `http://localhost:4318`, or `:4317` for gRPC) |
| `OTEL_EXPORTER_OTLP_PROTOCOL`, `…_{TRACES,METRICS,LOGS}_PROTOCOL` | `http/protobuf` (default) or `grpc` (plain-text gRPC; use `http/protobuf` for an `https://` collector) |
| `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_TIMEOUT` (and per signal) | as specified by OpenTelemetry (no compression support is built in) |
| `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES` | resource; `service.name` defaults to `sparkles` |
| `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG` | default `parentbased_always_on` |
| `OTEL_TRACES_EXPORTER`, `OTEL_METRICS_EXPORTER` | `otlp` (the default once enabled) or `none` |
| `OTEL_LOGS_EXPORTER` | `otlp` or `none` (the default: logs are opt-in, see below) |
| `OTEL_METRIC_EXPORT_INTERVAL` | milliseconds between metric exports (default 60000) |
| `OTEL_BSP_*` | batch span processor settings |

Flags of `serve`: `--otel` (enable), `--otel-logs` (export log events too),
`--otel-query-text` (record query and update text, which may hold data, in
`db.query.text`, cut to 2048 characters, and plan operator descriptions), and
`--otel-plan-spans` (one span per executed plan operator). Spans and log records are sent
in batches; on SIGTERM / SIGINT the server finishes its requests, then flushes the
exporters for at most 5 seconds. The resource carries `service.name`, `service.version`,
`service.instance.id` (a UUID per process), `host.name` and `process.pid`.

**Traces.** Each request is a server span named after its route (`GET /{ds}/sparql`),
continuing the trace of an incoming W3C `traceparent` / `tracestate`. Attributes:
`http.request.method`, `http.route`, `http.response.status_code`, `url.scheme`,
`url.path` (never the query string), `server.address` / `server.port` (from `Host`),
`client.address` (the peer), `user_agent.original`, `sparkles.request_id`,
`db.system.name` = `sparkles`, `db.namespace` (the dataset), `db.operation.name` (the
operation of the access log: `query`, `update`, `gsp`, …), `sparkles.sparql.kind`
(`SELECT`, `ASK`, `CONSTRUCT`, `DESCRIBE`), `sparkles.outcome`,
`db.response.returned_rows`, `http.response.body.size`, `sparkles.memory.peak_bytes`,
and for writes `sparkles.commit.seq`. 5xx responses set the span status to error with
`error.type`. Children:

* `sparql.parse`, `sparql.plan`, `sparql.execute` (with `db.response.returned_rows`) and
  `sparql.serialize` for queries; `sparql.parse` and `sparql.execute` for updates. They
  are synthesized after the request from the recorded timings, so the executor itself is
  not instrumented.
* With `--otel-plan-spans`, the executed operator tree under `sparql.execute`: one span
  per operator (at most 256) with `sparkles.operator`, `sparkles.rows`,
  `sparkles.rows.estimated`, `sparkles.cost.estimated` and `sparkles.cached`. Durations
  are the recorded ones; children are laid out one after another from their parent's
  start, so their offsets are approximate.
* `commit` (`seq`, `kind`, `inserted`, `deleted`) for every commit, `sparql.service` (a
  client span) for each SERVICE call, `shacl.validate`, and `shex.compile` and `shex.validate`.
* Refusals by a rate limit add a `rate_limited` event and `sparkles.rate_limit.class`.

Background tasks (compaction, backups, clones, reasoning, full-text rebuilds) are root
spans `task {kind}` linked to the request that started them. SERVICE and `LOAD <url>`
requests carry `traceparent` (and `tracestate`), so a federated endpoint continues the
trace. A sampled request's response carries `traceresponse: 00-{trace-id}-{span-id}-01`
(W3C Trace Context Level 2, exposed through CORS), and its log lines carry `trace_id`
and `span_id` in the request span.

**Metrics.** `http.server.request.duration` (histogram, seconds, the buckets of
`sparkles_request_duration_seconds`) with `http.request.method`, `http.route`,
`http.response.status_code`, `url.scheme`, `db.namespace` (the capped `dataset` label),
`db.operation.name` and, for 5xx, `error.type`. The Prometheus registry is exported as
observable instruments read at collection time, so nothing is counted twice and
`/$/metrics` is unchanged: `sparkles.requests` (`dataset`, `operation`, `outcome`),
`sparkles.response.size`, `sparkles.requests.active`, `sparkles.result.rows`,
`sparkles.budget.exceeded`, `sparkles.rate_limited`, `sparkles.dataset.quads`,
`sparkles.delta.quads`, `sparkles.wal.size`, `sparkles.disk.size`,
`sparkles.block_cache.{size,capacity,hits,misses}`,
`sparkles.result_cache.{size,capacity,entries,hits,misses}`, `sparkles.geo.rows`
(`dataset`, `part`), `sparkles.geo.build.duration`,
`sparkles.geo.{candidates,refined,matches,rechecked}`, `sparkles.ready`,
`process.uptime` and `process.memory.usage`.

**Logs.** With `--otel-logs` or `OTEL_LOGS_EXPORTER=otlp`, every log event that passes
`RUST_LOG` (the access log included) is also exported as an OTLP log record with the
trace and span id of its request.

### Rate limiting

Off by default, except for failed authentications when authentication is on (see
[Before authentication](#before-authentication-preauth)). `sparkles serve --rate-limit SPEC`
(repeatable) and/or `--rate-limit-config FILE` limit each request class per client:

| Class | Requests |
|-------|----------|
| `auth` | every path under `/$/auth/` (login, token minting, device flow, OIDC callback), matched or not |
| `query` | `/{ds}/sparql`, `/{ds}/query`, `/{ds}/explain`, `/{ds}/shacl`, `/{ds}/shex`, Graph Store `GET`/`HEAD`, `/{ds}` with `query=` or a GET, `/$/schema/*`, `/$/stats/*`, `/$/reason/{ds}/diagnostics`, `/$/format` |
| `update` | `/{ds}/update`, `/{ds}/upload`, Graph Store `PUT`/`POST`/`DELETE`, `/{ds}` with `update=` or any other write (a form POST to `/{ds}` counts as an update) |
| `admin` | `/$/…` requests other than `GET`/`HEAD` and `POST /$/format` (dataset management, compaction, backups, reasoning, caches, full-text) |
| `preauth` | every request, before authentication: failed credential checks per client address and IPv6 /48 (no per-dataset form) |

`/$/ping`, `/$/ready*`, `/$/metrics`, the UI and the other `/$/` reads are never limited.

`SPEC` is `CLASS[@DATASET]=LIMIT`, where `LIMIT` is `off` or a comma-separated list of

* `N/s`, `N/min`, `N/h` or `N/d`: the sustained rate per client;
* `burst=N`: requests a client may make at once after being idle (default: the rate's `N`);
* `concurrency=N`: requests of the class in flight server-wide;
* `client-concurrency=N`: requests in flight per client;
* `failure-cost=N`: what a `401` or `403` response costs, in requests (default 1), so that
  failed logins exhaust the budget faster (for `preauth`: what a failed credential check
  costs).

`CLASS@DATASET=…` replaces the class limit for requests to that dataset (with its own
counters); `CLASS@DATASET=off` exempts the dataset. Examples:

```sh
sparkles serve --rate-limit auth=10/min,burst=5,failure-cost=3 \
               --rate-limit query=100/s,burst=200,client-concurrency=8,concurrency=64 \
               --rate-limit update=10/s --rate-limit query@public=5/s
```

`auth=10/min,burst=5` (with `failure-cost=3`) is a reasonable strict default for
authentication endpoints. The configuration file is JSON; the flags apply on top of it,
and `SIGHUP` re-reads it. A reload keeps the client state of every limit whose name
(`query`, `query@public`, …) stays: debts are not forgiven, and the requests already in
flight count against the new concurrency caps, so a lower cap admits nothing new until
they drop below it. A bad file keeps the running configuration.

```json
{
  "classes": {
    "preauth": { "rate": "30/min", "burst": 60 },
    "auth":  { "rate": "10/min", "burst": 5, "failureCost": 3 },
    "query": { "rate": "100/s", "burst": 200, "concurrency": 64, "clientConcurrency": 8 }
  },
  "datasets": { "public": { "query": { "rate": "5/s" } } },
  "trustedProxies": ["127.0.0.1", "::1"],
  "trustedProxyHeader": "x-forwarded-for",
  "maxKeys": 100000
}
```

**Clients.** A client is its peer address (an IPv6 client by its /64). Behind a reverse
proxy, list the proxy under `trustedProxies` (or `--rate-limit-trusted-proxy CIDR`;
`unix` trusts the `--unix-socket`): for requests from a trusted peer the client is the
rightmost untrusted hop of `X-Forwarded-For`, the header nginx, HAProxy, Caddy, Traefik
and cloud load balancers set. Only that header is read, so a client's own `Forwarded`
changes nothing. For a proxy that sets `Forwarded` (RFC 7239) instead, set
`"trustedProxyHeader": "forwarded"` (or `--rate-limit-trusted-proxy-header forwarded`);
`X-Forwarded-For` is then the ignored one. A hop that is not an address (`unknown`, an
obfuscated `_id`) is a client of its own, named by that text; when every hop is trusted,
the client is the leftmost of them. On the Unix socket without a trusted `unix`, clients
have no address and share one key. Forwarding headers from untrusted peers are ignored,
counted in `sparkles_rate_limit_untrusted_forwarded_total`, and the first is logged as a
warning. With authentication, a signed-in caller is counted as its owner instead (see
[Authentication](#authentication-and-access-control)), except in `preauth` and `auth`.

Limits by address are only as good as the address: they need a peer address that clients
cannot choose. List only proxies that overwrite (nginx: `proxy_set_header X-Forwarded-For
$remote_addr;`) or append to the header they receive, never a network that clients can
send from. Behind a proxy that is not listed, every client has the proxy's address and
shares one budget, so `serve` warns at startup when authentication is on and the listener
(the Unix socket, or a loopback address) trusts no proxy.

At most `maxKeys` clients (default 100,000, about 100 bytes each) are tracked; a flood of
new addresses evicts other rarely seen clients, never one with requests in flight. An
evicted client that still owed time is remembered in a penalty cache an eighth that size,
so churning the cache does not forgive its debt. `maxKeys` is a security setting: a
value far below the number of active clients lets a flood of addresses evict (and a
flood larger than the penalty cache forget) clients; watch
`sparkles_rate_limit_evictions_total`. Clients whose bucket has refilled are dropped every
minute.

**Algorithm.** GCRA (the virtual-scheduling form of a token bucket): a client may send
`burst` requests at once, then one every `period / N`.

**Responses.** Over the rate: `429 Too Many Requests` with `Retry-After` (whole seconds).
Over a concurrency cap: `503 Service Unavailable` with `Retry-After: 1`, immediately;
requests are never queued, so a saturated server sheds load instead of holding waiting
requests. A request holds its concurrency slot until its response body has been sent
(streamed Graph Store GETs included) and the work it started has ended: a client that
disconnects cancels its query or write, and the slot is free once that work has
stopped. The body uses the error format:

```json
{ "error": "too many query requests: retry in 2 s",
  "limitClass": "query", "reason": "rate", "retryAfterSeconds": 2 }
```

`reason` is `rate`, `concurrency` or `client-concurrency` (`failures` for `preauth`; `mint`,
`device` or `device-code` for the auth layer's own limits). Responses of a class with a
rate carry the headers of draft-ietf-httpapi-ratelimit-headers-11:
`RateLimit-Policy: "query";q=100;w=1` (the configured rate: `q` requests per `w`
seconds; the policy name is `CLASS` or `CLASS@DATASET`) and `RateLimit: "query";r=57;t=1`
(`r` requests available now, `t` seconds until the bucket is full). CORS exposes
`Retry-After`, `RateLimit` and `RateLimit-Policy`.

**Observability.** Refused requests are logged with `outcome=rate_limited` and counted
in `sparkles_requests_total{outcome="rate_limited"}` and
`sparkles_rate_limited_total{dataset,class}`.

#### Before authentication (`preauth`)

A first stage runs before any credential is checked, so that password guessing and the
hashing it costs are bounded per client address. Every address has a budget of failed
credential checks: a wrong password (HTTP Basic or a UI login), an unknown, expired or
malformed token, an invalid session cookie, an unknown device user code or loopback
code, a failed OIDC callback, and a cross-origin or CSRF refusal on an `/$/auth/` route.
Nothing else is charged: not a `403` of authorization (a read-only server, `SERVICE` or
`LOAD` refused by policy, a missing permission), not a hidden dataset's `404`, not the
`401` of an anonymous request, and not a busy password check. Each failure costs
`failure-cost` (default 1); an address without failures leaves no state behind. An IPv6
client also spends the budget of its /48, eight times as large (`"preauth/48"` in the
headers), so a network that holds many /64s does not get a budget per /64.

An address (or /48) that has spent its budget is refused with `429` (`"limitClass":
"preauth"`, `"reason": "failures"`) until it refills, but only for what would hash a
password (HTTP Basic and UI password logins whose credentials are not in the
verified-credential cache) or turns out to present an unknown token. Valid tokens,
sessions and proxy identities, anonymous requests, `/$/ping`, `/$/ready` and the UI keep
working, so one client's guesses from a shared address do not take the others, or a load
balancer's health checks, down with it. A password check takes the cost of a failure
before it starts and gives it back when the password is right, so concurrent guesses
from one address cannot all start hashing. Responses to failures carry the stage's
`RateLimit-Policy` and `RateLimit`.
Requests without a client address (over `--unix-socket` with no trusted `unix` proxy)
share one budget, so guessing stays bounded there too; trust the proxy on the socket so
that its clients are told apart.

With `--auth-config` it is on by default at `30/min,burst=60` (60 failures at once, then
one every two seconds). `--rate-limit preauth=RATE[,burst=N][,failure-cost=N]` or
`classes.preauth` changes it (no per-dataset form and no concurrency caps);
`preauth=off` turns it off.

## Datasets (admin)

| Method | Path                         | Description |
|--------|------------------------------|-------------|
| GET    | `/$/datasets`                | `{ "datasets": [DatasetInfo] }` |
| POST   | `/$/datasets`                | Create. Form or JSON body: `dbName`, `dbType` = `persistent` \| `mem`, and optionally `geo` = `true` (a spatial index with the defaults) or, in a JSON body, a `GeoConfig` (see [GeoSPARQL](#geosparql); `400` for an invalid one, `501` in a build without the `geo` feature). `201` on success, `409` if exists. |
| GET    | `/$/datasets/{ds}`           | `DatasetInfo` |
| DELETE | `/$/datasets/{ds}`           | Remove dataset (and its files). |
| POST   | `/$/datasets/{ds}/clone`     | Copy the dataset into a new persistent dataset. See [Clone](#clone). `202` with a `Task`. |
| GET    | `/$/stats/{ds}`              | `DatasetStats` |
| GET    | `/$/schema/{ds}`             | *Extension.* `SchemaSummary`: classes and predicates with exact counts and their declarations; see [Schema discovery](#schema-discovery). |
| GET    | `/$/schema/{ds}/classes`     | *Extension.* `Page<ClassEntry>` |
| GET    | `/$/schema/{ds}/predicates`  | *Extension.* `Page<PredicateEntry>` |
| POST   | `/$/compact/{ds}`            | Merge delta (updates) into a freshly built, sorted base index. Returns `Task`; `409` while a compaction of the dataset is queued or running. |
| POST   | `/$/backup/{ds}`             | Write an N-Quads dump to `<data>/backups/{ds}_{time}.nq.zst` (zstd level 3; gzip, `.nq.gz`, in a build without zstd). `?compression=gzip\|zstd\|brotli\|lz4\|none` and `?level=N` pick another codec (the extension follows it, so `compression=gzip` gives Fuseki's `.nq.gz`; levels: gzip 0–9, zstd 1–19, brotli 0–11, none for lz4 and none, else `400`). Returns a cancellable `Task`; its message gives the size and time. `409` while a backup of the dataset is queued or running; `507` when the data directory's file system keeps less than `--min-free-disk-mb` free, and the task fails once writing would go below it. zstd uses at most 4 threads (a quarter of the cores). Incremental, deduplicated backups to a file system or S3 are under [Backup repositories](#backup-repositories). |
| POST   | `/$/reason/{ds}`             | Materialize inferences. JSON body `{ "profile": "rdfs" \| "owl-rl" \| "rules", "rules"?: string }`, or `{ "rerun": true }` (also `?rerun=true`) to re-run the recorded profile and rules (`409` when nothing is recorded). Returns `Task`. |
| GET    | `/$/reason/{ds}`             | `ReasoningStatus`, or `{ "reasoning": null, "head": number }`. See [Reasoning status and diagnostics](#reasoning-status-and-diagnostics). |
| GET    | `/$/reason/{ds}/diagnostics` | `DiagnosticsReport`: OWL 2 RL inconsistency checks. |
| DELETE | `/$/reason/{ds}`             | Drop materialized inferences. |
| GET    | `/$/tasks`                   | `[Task]` |
| GET    | `/$/tasks/{id}`              | `Task` |
| DELETE | `/$/tasks/{id}`              | *Extension.* Cancel a task that accepts it (a queued task, a clone until it is in place, an N-Quads backup): `202` with the `Task`; it ends `cancelled`. `409 {code: "not-cancellable"}` for other tasks and finished ones. Needs `admin` on the task's dataset (`server-admin` for a server-wide task). |
| POST   | `/$/cache/clear/{ds}`        | *Extension (no Fuseki equivalent).* Drop the dataset's cached query results. `{ "cleared": number /* entries */, "bytes": number }` |
| GET    | `/$/prefixes/{ds}`           | `{ "prefixes": { "rdf": "http://…#", … } }` — the dataset's prefixes plus well-known ones. |
| GET    | `/{ds}/prefixes`             | After Fuseki's prefixes service. `?prefix=p` → `{ prefix, uri }` (`404` if unbound); `?uri=u` → `{ uri, prefixes: [...] }`; neither → `{ prefixes: {...} }` (stored ones only). |
| POST/PUT | `/{ds}/prefixes`           | Bind `prefix` to `uri` (query, form or JSON body `{prefix, uri}`); `400` for an invalid name or IRI (names up to 256 bytes, IRIs up to 4096), or for a new prefix once the dataset has `--max-prefixes` (1000; replacing one is fine). Prefixes of loaded data are added up to the same limit. Prefixes are metadata: no commit is made. |
| DELETE | `/{ds}/prefixes?prefix=p`    | Remove a binding (`204`, or `404` if unbound). |

```ts
type DatasetInfo = {
  name: string;            // "ds"
  type: "persistent" | "mem";
  endpoints: { query: string; update: string; gsp: string; upload: string; shacl?: string; shex?: string /* each absent when built without its feature */ };
  quads: number;           // approximate total (base + delta)
  reasoning: null | {
    profile: string; inferred: number; at: string;
    commit: number | null;       // commit the inferences were materialized at
    stale: boolean | null;       // null: unknown (see ReasoningStatus)
    commitsSince: number | null;
  };
  forkedFrom?: { id: string; seq: number };   // clones: source dataset id and copied commit
  origin?: DatasetOrigin;                     // clones: origin.json (see Clone)
  restoredFrom?: { repository: string; backup: string; datasetId: string; seq: number };
                                              // restored from a backup repository
  access?: "read" | "write" | "admin";        // with auth: the caller's level (absent without)
  text: null | { state: string; docs: number };     // full-text index (see Full-text search)
  geo: null | { state: string; rows: number };      // spatial index: state and rows (base + overlay + tail)
};

type DatasetStats = {
  name: string;
  quads: number;           // base + inserts − deletes
  baseQuads: number;
  deltaInserts: number;
  deltaDeletes: number;
  terms: number;           // vocabulary size
  graphs: { name: string | null; quads: number }[];   // null = default graph
  predicates: { iri: string; count: number; distinctSubjects: number; distinctObjects: number }[]; // top 100
  classes: { iri: string; instances: number }[];      // top 100 by rdf:type
  diskBytes: number;
  cache: { entries: number; bytes: number; hits: number; misses: number };        // decoded-block cache (--cache-mb)
  resultCache: { enabled: boolean; entries: number; bytes: number; hits: number; misses: number }; // query (sub)result cache (--result-cache-mb)
  reasoning: ReasoningStatus | null;
  geo: GeoStatus | null;   // the spatial index (see GeoSPARQL)
};

type Task = {
  id: string;
  kind: "compact" | "backup" | "reason" | "load" | "clone" | "text-rebuild" | "geo-index"
      | "backup-create" | "backup-restore" | "backup-verify" | "backup-gc" | "backup-policy";
  dataset: string;          // "" for a server-wide task (listed for server admins only)
  target?: string;          // the dataset a clone creates; for backup tasks see Backup repositories
  state: "queued" | "running" | "done" | "failed" | "cancelled";
  startedAt: string; finishedAt?: string; progress?: number /*0..1*/;
  message?: string;         // absolute paths cut to "…/" and their last component,
                            // except for callers with server-admin
  cancellable: boolean;     // DELETE /$/tasks/{id} would be accepted now
  detail?: object;          // a typed result, for task kinds that have one
};
```

**Task slots.** At most `sparkles serve --max-tasks` (default 4; `0`: no limit) background
tasks (compaction, clones, reasoning, full-text and spatial index builds, N-Quads backups) run at once; the
others wait `queued`, in start order, and may be cancelled while they wait. Backup
repository tasks (`backup-*` kinds) wait for their own `--backup-max-tasks` slots instead.
Starting a task while 1000 already wait answers `503`. The task list keeps every queued
and running task and the 200 most recent finished ones.

## Schema discovery

`GET /$/schema/{ds}` reports the classes and predicates of a dataset in two separate
layers:

* **observed**: exact counts over the selected graphs at one snapshot. A triple stored in
  several selected graphs counts once. These are measurements of the current data, not
  constraints: `maxPerSubject: 1` only says that no subject has two values *now*.
* **declared**: what the RDFS/OWL vocabulary in the data asserts (`rdf:type` `owl:Class`,
  `rdfs:subClassOf`, `rdfs:domain`, `owl:FunctionalProperty`, labels, …). Only IRI objects
  are listed; blank-node class expressions (`owl:Restriction`, …) are counted in
  `totals.anonymousClassExpressions`.

A class is listed when it is an IRI object of `rdf:type` in the selection, is declared
with `rdf:type rdfs:Class | owl:Class | rdfs:Datatype`, or is an IRI subject or object of
`rdfs:subClassOf`, `owl:equivalentClass` or `owl:disjointWith`. A predicate is listed when
it occurs in the selection (`observed.triples > 0`) or is declared (a property type,
`rdfs:domain`/`range`/`subPropertyOf`, `owl:inverseOf`) with no triples. Nothing is
truncated: every list reports its `total` and is paginated with a cursor.

Parameters (all optional; the read-only server allows them):

| Param | Values | Default | Meaning |
|---|---|---|---|
| `graph` | `default`, `union`, a graph IRI; also `urn:x-arq:DefaultGraph`, `urn:x-arq:UnionGraph` | `default` | Graphs whose triples are counted. `default` is every graph with `--union-default-graph`. A graph IRI with no quads → `404`. |
| `declaredGraph` | same | same as `graph` | Graphs read for declarations (an ontology in its own named graph) |
| `reasoning` | `true`, `false` | `true` if the dataset has materialized inferences | Count `urn:x-sparkles:inferred` as part of `default` / `union` |
| `declared` | `asserted`, `all` | `asserted` | `all` also reads declarations from the inferred graph (which holds the transitive closure of `rdfs:subClassOf`, `rdfs:Resource` supers, …) |
| `limit` | 1–10000 | 1000 | Page size (the summary uses it for both first pages) |
| `cursor` | opaque | — | The `next` of the previous page; send the same selection parameters with it |
| `timeout` | seconds | server query timeout | Budget for computing the report |

```ts
type SchemaSummary = {
  schemaFormat: 1;                 // version of this JSON shape
  dataset: string;
  snapshot: { version: number;     // changes on every commit and compaction; restarts with the server
              generation: string;  // base index generation
              computedAt: string };// RFC 3339
  selection: { graph: string; declaredGraph: string; reasoning: boolean; declared: "asserted" | "all" };
  totals: { triples: number;       // distinct triples in the selection
            classes: number; predicates: number;
            anonymousTypeTargets: number;        // blank-node objects of rdf:type
            anonymousClassExpressions: number }; // blank-node objects of class axioms, rdfs:domain, rdfs:range
  ontology: { iri: string; labels: Lit[]; versionInfo: Lit[]; comments: Lit[] }[];  // every owl:Ontology
  hierarchy: { roots: string[];    // classes with no declared superclass outside their own subClassOf cycle
                                   // (one per cycle), most subclasses first, then by IRI
               cycles: string[][] };// subClassOf cycles with more than one member (A ⊑ A is not a cycle)
  classes: Page<ClassEntry>;
  predicates: Page<PredicateEntry>;
};
type Page<T> = { items: T[]; total: number; next: string | null };  // items in IRI order
type Lit = { value: string; lang?: string };

type ClassEntry = {
  iri: string;
  builtin: boolean;                // rdf:, rdfs:, owl:, xsd: or sh: namespace
  observed: { instances: number }; // distinct subjects with rdf:type C (no subclass roll-up)
  declared: { types: string[];     // subset of rdfs:Class, owl:Class, rdfs:Datatype
              superClasses: string[]; equivalentClasses: string[]; disjointWith: string[];
              labels: Lit[]; comments: Lit[] };
};
type PredicateEntry = {
  iri: string;
  builtin: boolean;
  observed: {
    triples: number; distinctSubjects: number; distinctObjects: number;
    maxPerSubject: number;         // largest number of distinct objects of one subject, in this snapshot
    subjectsWithMultiple: number;  // subjects with two or more distinct objects
    objects: {
      iri?: KindCount; blank?: KindCount; tripleTerm?: KindCount;
      literals: { datatype: string;  // xsd:string for simple literals; rdf:langString / rdf:dirLangString
                  triples: number; distinct: number;  // distinct terms: "01"^^xsd:integer ≠ "1"^^xsd:integer
                  languages?: { lang: string; direction?: "ltr" | "rtl"; triples: number }[] }[];
    };
  };
  declared: { types: string[];     // rdf:Property, owl:ObjectProperty, owl:FunctionalProperty, …
              domains: string[]; ranges: string[]; superProperties: string[]; inverseOf: string[];
              labels: Lit[]; comments: Lit[] };
};
type KindCount = { triples: number; distinct: number };
```

**Pagination.** Every page of a listing comes from the report of one snapshot. The
server keeps the last report per dataset; the summary and a page request without a
cursor reuse it while the snapshot and the selection are unchanged, and compute a new one
otherwise. A cursor from an older snapshot is still served while that report is the one
kept; once a newer report replaces it the request fails with `409` and the client
restarts from the first page. Cursors do not survive a restart.

**Errors** (`{ "error" }` body): `400` for a bad parameter, a malformed cursor or a
cursor issued for other selection parameters; `404` for an unknown dataset or a graph
with no quads; `408` when the report did not finish within `timeout`
(`"schema discovery exceeded 60s while scanning predicates (412/9031); narrow graph= or
raise timeout="`); `409` as above; `413` when there are more than `--schema-max-entries`
(default 1,000,000) classes or predicates (`"dataset has 1204331 classes (limit
1000000)"`). A report is never returned partially.

The counts come from one ordered pass over the PSO and one over the POS index per
predicate, so a report costs about two sequential reads of the selected triples.

The CLI equivalent prints the complete report without pagination:
`sparkles schema --loc DB [--graph default|union|IRI] [--declared-graph G]
[--no-inferences] [--declared asserted|all] [--format text|json] [--timeout S]
[--max-entries N]` (or `--data FILE…`). `json` is the `SchemaSummary` with every item and
`next: null`; `text` prints one line per class and per predicate. It exits with status 2
when the timeout or the entry cap is exceeded. In Rust, `sparkles::schema::discover`.

### Clone

`POST /$/datasets/{ds}/clone` copies one consistent snapshot of `{ds}` into a new,
independent persistent dataset, for trying updates, reasoning or loads without touching
the original. Parameters come from the query string, a form body or a JSON body:

| Param | Required | Meaning |
|---|---|---|
| `name` | yes | name of the new dataset |
| `inferences` | no, default `copy` | `copy`: the inferred graph and the reasoning status; `drop`: neither |

The copy has every quad of every graph (blank-node graph names and triple terms
included), the same blank-node ids (`_:b<hex>` labels), the prefixes, and a freshly
compacted index. It is a new lineage: a new dataset id and a root commit `0`, with the
source's id and the copied commit kept as `forkedFrom`. Commit history, the WAL and
caches are not copied; full-text search stays enabled with the same configuration, and
the clone builds its own index when it is first opened. With `inferences=copy`, inferences that were fresh at the copied
commit are fresh in the clone; stale ones stay stale (`staleReason: "inherited from
source at clone time"`), unknown ones stay unknown. Source updates continue during the
clone and are not included.

Responses: `202` with `Task` (`kind: "clone"`, `target`) and `Location:
/$/datasets/{name}`; `400` for a missing or invalid `name`, a bad `inferences`, or
`type=mem` (in-memory clones are not supported yet); `403` on a read-only server;
`404` for an unknown source; `409` when `name` is registered, being created by another
task (`POST /$/datasets` with that name also gets `409` meanwhile), or
`<data>/databases/{name}` exists without being a registered dataset. The dataset appears
(and is persisted in `config.json`) only when the task is `done`. A failed task leaves
no directory and releases the name; unfinished clones are removed at startup.

```ts
type DatasetOrigin = {            // origin.json in the clone's directory
  originFormat: 1; clonedAt: string;
  source: { name: string; path?: string; version: number; generation: string; quads: number };
  forkedFrom: { id: string; seq: number };
  inferences: "copy" | "drop";
};
```

`sparkles clone --loc SRC --to DST [--inferences copy|drop]` does the same offline
(`DST` must not exist or be empty; `SRC` must be a database, and not open in a server).

## Per-dataset SPARQL protocol (Fuseki compatible)

| Method     | Path                  | Description |
|------------|-----------------------|-------------|
| GET/POST   | `/{ds}` , `/{ds}/sparql`, `/{ds}/query` | SPARQL 1.1 Query protocol (`query=` param, `application/sparql-query` body, or form). `default-graph-uri` / `named-graph-uri` supported. |
| any        | `/{ds}`               | Also the update endpoint (`update=` or `application/sparql-update`) and the Graph Store endpoint for any other body. A form body (`application/x-www-form-urlencoded`) must hold `query` or `update`: with neither it is refused (`400`, or the authorization error of a write for a caller without write access), never read as RDF. |
| POST       | `/{ds}/update`        | SPARQL 1.1 Update protocol (`update=` form or `application/sparql-update` body). An update sent with GET (`/{ds}?update=…`) gets `405`. |
| GET/PUT/POST/DELETE/HEAD | `/{ds}/data` , `/{ds}/get` | Graph Store Protocol. `?default` or `?graph=<iri>`; no param on GET = whole dataset as N-Quads/TriG. GET is streamed from one snapshot (see [Budgets](#budgets)). |
| POST       | `/{ds}/upload`        | Multipart file upload; format chosen from filename extension / content-type. Optional `graph` field. |
| POST       | `/{ds}/shacl`         | SHACL validation (Fuseki `/{ds}/shacl`); see [SHACL validation](#shacl-validation). |
| POST       | `/{ds}/shex`          | ShEx validation (a Sparkles extension; Fuseki has none); see [ShEx validation](#shex-validation). |

Content negotiation via `Accept` or the `format=` parameter (Fuseki style):

* SELECT/ASK: `application/sparql-results+json` (default), `application/sparql-results+xml`,
  `text/csv`, `text/tab-separated-values`, and `application/x-sparkles+json` (see below).
* CONSTRUCT/DESCRIBE/GSP GET: `text/turtle` (default), `application/n-triples`,
  `application/n-quads`, `application/trig`, `application/ld+json`, `application/rdf+xml`.

Query parameters beyond the standard protocol:

* `timeout=<seconds>` — query timeout (default 60 s, `sparkles serve --timeout`), capped
  at `--max-timeout` (default 1800 s; `0`: no cap; never below `--timeout`). Updates,
  Graph Store `PUT`/`POST`/`DELETE` and uploads accept it too, under the same cap (never
  below `--update-timeout`; for a Graph Store write or an upload it starts once the body
  has been received); without it they run under `--update-timeout` (none by default). A
  timed-out write changes nothing. A `408` names the timeout that applied in
  `timeoutSeconds`. A write whose client disconnects is cancelled (also while it waits for
  the dataset's writer lock) and commits nothing; a commit that already started completes.
* `send=<n>` — cap on rows serialized (the UI uses this so a huge result does not hang the browser; `meta.totalRows` still reports the full count).
* `reasoning=true|false` — include materialized inferences (default `true` if present).
* `nocache=true` — bypass the query result cache: nothing is read from or stored in it
  (for benchmarking; `explain` accepts it too). The server-wide budget is set with
  `sparkles serve --result-cache-mb N` (default 512, `0` disables the cache); the cache is
  keyed by snapshot version, so updates invalidate it, and `POST /$/cache/clear/{ds}`
  empties it.

## Commits

Every dataset has a **dataset id** (a UUID created with it) and a gap-free **commit
sequence**. Each write that changes data (update, Graph Store PUT/POST/DELETE, upload,
load, reasoning) gets the next `seq`. A write with no net effect, such as inserting a
quad that is already present, creates no commit. Commit 0 is the root. Compaction keeps
the head. Ids survive restarts and are durable exactly when the data is.

**Headers.** Every successful query, update, Graph Store, explain and SHACL validation
response carries:

```
Sparkles-Commit: 42                 (the commit a read saw, or a write produced)
Sparkles-Dataset-Id: 3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa
```

Both are exposed to cross-origin clients. The `application/x-sparkles+json` result
format also has `meta.commit` and `meta.datasetId`.

**Receipts.** Write responses keep their Fuseki-compatible bodies by default. With
`Accept: application/x-sparkles+json` or `receipt=true`, the body (same status; `200`
instead of `204` for Graph Store DELETE) adds:

```ts
type Receipt = {
  dataset: string; datasetId: string;
  committed: boolean;            // false: no net change, `commit` is the unchanged head
  commit: Commit;
};
type Commit = {
  seq: number; parent: number | null; ref: string;   // "commit:42"
  timestamp: string;             // RFC 3339 UTC with milliseconds, never decreasing
  kind: "create" | "baseline" | "update" | "gsp-put" | "gsp-post" | "gsp-delete"
      | "upload" | "load" | "reason" | "reason-clear" | "transaction" | "unknown";
  inserted: number; deleted: number;   // net change relative to the parent
  quads: number;                        // dataset size after the commit
  generation: string;                   // index generation it was made in
  bulk: boolean;                        // made by rebuilding the index
  exact: boolean;                       // false: a bulk commit that also deleted
};
```

| Method | Path | Description |
|--------|------|-------------|
| GET | `/$/commits/{ds}` | Newest commits first: `?limit=` (default 50, max 1000), `?before=<seq>` pages backwards, `?after=<seq>` lists oldest first after `seq`. Returns `{dataset, datasetId, head, firstRetained, complete, commits: Commit[], next: string \| null}`. |
| GET | `/$/commits/{ds}/{ref}` | One commit; `ref` is `42`, `commit:42` or `head`. `404` beyond the head, `410` if no longer retained. |

`GET /$/datasets[/{ds}]` entries gain `id`, `head` and `modified` (the head's timestamp).
`sparkles log --loc DB [--limit N] [--before SEQ | --after SEQ | --at REF] [--format json]`
lists commits without taking the database lock, so it works next to a running server.

## Point-in-time reads and snapshots

Every commit since the dataset's last compaction or bulk commit can be read, at no extra
cost: its state is the current index generation plus a prefix of its write-ahead log.
Older commits stay readable while a **named snapshot** or the **retention window** keeps
the generation that holds them (compaction and bulk commits then keep that generation
instead of deleting it). Only persistent datasets have history.

**Selector** (`at`, in the query string or a form body) on `/{ds}/sparql`, `/{ds}/query`,
`/{ds}?query=`, `/{ds}/explain` and Graph Store `GET`/`HEAD`:

| `at` | State |
|---|---|
| `head` (or absent) | the live state |
| `42`, `commit:42` | right after commit 42 |
| `time:2026-09-30T14:03:11.482Z` | the last commit at or before that instant (any RFC 3339 offset; a `+` that arrives as a space is accepted) |
| `snapshot:NAME` | the commit a named snapshot pins |

Responses add `Sparkles-At` (the selector, canonical form) and `Sparkles-Head`; for a
past state also `Memento-Datetime` (the commit's time) and `Link: <…>; rel="original"`
(RFC 7089). `Sparkles-Commit` is the commit read. Freshness of inferences
(`Sparkles-Inferences`) is reported for the live state only. `text:query` works at the
head only (`501` at a past commit); vector search works at any commit. Writes with `at`
(even `at=head`) are refused with `400` and `code: "at-on-write"`.

Errors (with a `code`): `400 invalid-at`; `404` for a commit beyond the head, an unknown
snapshot, or a time before history; `410 history-gone` for a commit whose data is no
longer kept, with the readable ranges:

```json
{ "error": "commit 12 is no longer reconstructable; the oldest reconstructable commit is 40",
  "code": "history-gone", "commit": 12, "head": 57, "oldestReconstructable": 40,
  "reconstructable": [ { "from": 40, "to": 57 } ], "metadata": { "seq": 12, … } }
```

`501 history-unsupported` for in-memory datasets. Materializing a past state is bounded by
`--history-cache-mb` (default 1024, `507` beyond it) and the request timeout; results are
cached, one materialization at a time.

| Method | Path | Description |
|---|---|---|
| GET | `/$/snapshots/{ds}` | `{ dataset, datasetId, head, snapshots: NamedSnapshot[] }` |
| POST | `/$/snapshots/{ds}` | Pin `{ name, at?: selector (default head), note? }` (JSON, form or query). `201` + `Location`; `200` if the name already pins that commit; `409` if it pins another one, or with `code: "history-limit"` beyond `--max-snapshots` (256) or `--history-max-generations` (8); `410` if the commit is no longer readable |
| GET | `/$/snapshots/{ds}/{name}` | `NamedSnapshot` |
| DELETE | `/$/snapshots/{ds}/{name}` | `204`; generations only it kept are removed |
| GET | `/$/history/{ds}` | `HistoryStatus` |
| PUT | `/$/history/{ds}` | Set the retention window `{ keepCommits?: number \| null, keepAge?: "7d" \| seconds \| null }` and return `HistoryStatus` |

```ts
type NamedSnapshot = { name: string; ref: string; seq: number; commit: Commit | null;
  created: string; note: string | null; generation: string | null; reconstructable: boolean };
type HistoryStatus = { dataset: string; datasetId: string; head: number;
  oldestReconstructable: number | null; reconstructable: { from: number; to: number }[];
  bytes: number;   // disk of kept non-current generations
  generations: { name: string; baseSeq: number; endSeq: number; bytes: number;
                 current: boolean; heldBy: string[] }[];   // "head", "snapshot:NAME", "retention"
  retention: { keepCommits: number | null; keepAge: string | null };
  snapshots: number;
  cache: { entries: number; bytes: number; hits: number; misses: number; materializations: number } };
```

A pin at the head costs nothing (the next generation starts at that commit); a pin inside
a generation keeps the whole generation, so its other commits stay readable too. Removing
a generation renames it to `gen-NNNN.deleting` first, so an interrupted removal is
finished at the next open. `GET /$/commits/{ds}` adds `oldestReconstructable`,
`reconstructable`, and per commit `reconstructable` and `snapshots`.

CLI: `sparkles snapshot create --loc DB NAME [--at SEL] [--note TEXT]`,
`snapshot list|history --loc DB [--format json]`, `snapshot delete --loc DB NAME`,
`snapshot retain --loc DB [--keep-commits N] [--keep-age 7d] [--off]`, and
`sparkles query --loc DB --at SEL …`, `sparkles dump --loc DB --at SEL`. These open the
database, so stop a server that holds it or use the HTTP API. In Rust:
`Store::snapshot_at`, `create_snapshot`, `set_retention`, `history`.

## Backup repositories

*Extension* (the `backup` cargo feature of `sparkles-server`, on by default). A
**repository** is a directory (`fs`: a local or mounted file system) or a bucket prefix
(`s3`: AWS S3 or an S3-compatible service such as MinIO, Cloudflare R2 or Ceph RGW) that
holds deduplicated, content-addressed copies of dataset files. A **backup** is one
persistent dataset at one commit, described by an immutable manifest. Backups are made,
listed, restored and verified per dataset under `/$/backups/{ds}`; repositories are
registered and maintained under `/$/repositories`; lifecycle policies (schedules and
retention) live under `/$/backup-policies`. The web UI has a Backups page for all three.
The older `POST /$/backup/{ds}` (an N-Quads dump in the data directory) is unchanged.

**What a backup holds.** The files of the dataset's current index generation
(`gen-NNNN/…`: the permutations, the vocabulary, `wal.log`, `delta.vocab`), the commit
catalog `commits.bin`, and `CURRENT`, `dataset.json` and `prefixes.json`, plus, when the
dataset has them, `text.json`, `origin.json`, `validation.json` and
`validation-shapes.ttl`, and `reasoning.json` unless its inferences were made at a later
commit than the captured one. The full-text index is left out and rebuilt when the
restored dataset opens (`derived.text.rebuildOnRestore`). Older index generations, named
snapshots and the retention window (`history.json`) are left out too, so point-in-time
reads of a restored dataset reach back to the start of the backup's generation only.
In-memory datasets cannot be backed up (`501 backup-unsupported`).

**Capture.** A backup pins one commit (the head when the task starts) without blocking
writers: under the writer lock it only records the length of the append-only files and
the prefixes (`sparkles_backup_capture_lock_seconds`), then reads through open file
handles. Writes, compactions and bulk commits continue during the upload. The backup
holds a lease on its generation, so history collection keeps the directory until the
upload ends; `GET /$/history/{ds}` shows it as a hold `backup:<name>`.

**Incremental and deduplicated.** Files are stored as blobs named by the SHA-256 of their
content. Generation files are immutable, so each is cut into 32 MiB pieces and a piece
the repository already holds is not uploaded again: a piece the parent backup (the
newest backup of the same dataset id) references is reused without a request, and any
other piece over 1 MiB is looked for with a `HEAD` first (which deduplicates across
datasets and after an interrupted backup). Of the append-only files (`wal.log`, `delta.vocab`, `commits.bin`)
only the bytes appended since the parent are uploaded, as new segments; a file with more
than 64 segments, or one that no longer extends the parent's copy, is stored from scratch.
So a backup after a few writes adds little more than the new WAL records and catalog
entries, and a backup after a compaction uploads the new generation in full. Blobs are
LZ4-compressed when that saves at least 10 %. `addedBytes` of a backup is what it
uploaded first; `logicalBytes` is the size of its files.

**Consistency.** The manifest `backups/<name>.json` is written last with a conditional
create, so a backup exists if and only if its manifest does: a failed or cancelled backup
leaves only unreferenced blobs, which the next backup reuses and garbage collection
removes. Manifests and blobs are never changed after they are written. A restore
validates the manifest before writing anything (paths, sizes, format), checks every
blob's length and SHA-256 and every file's SHA-256, runs `sparkles check` on the
restored directory (`quick` by default), opens it and compares its head commit and quad
count with the manifest (`500 restore-mismatch` otherwise) before it publishes it.

**Names.** Repository and policy names follow `[a-z0-9][a-z0-9_-]{0,63}` and share one
namespace on a server (a policy cannot take a repository's name, or the other way
round); a policy cannot be named `preview`. Backup names follow
`[A-Za-z0-9][A-Za-z0-9._-]{0,63}`; the default is `{dataset}-{YYYYMMDDtHHMMSSz}`
(`wiki-20260930t140511z`), with the dataset part shortened or its other characters
replaced by `-` when needed.

**Identity.** Every dataset has a dataset id (a UUID, see [Commits](#commits)); a backup
records it. A restore gives the restored dataset:

| `identity` | Dataset id |
|---|---|
| `auto` (default) | the backup's id, unless a dataset on this server has it (the dataset being replaced included, so restoring in place of the live dataset it came from mints a new one); then as `new` |
| `new` | a fresh id; `forkedFrom: {id, seq}` names the backup's dataset and commit. Commit numbers continue from the backup's commit |
| `keep` | the backup's id; `409 duplicate-dataset-id` if another dataset has it, or when replacing in place a dataset with that id whose head is past the backup's commit (the same commit numbers would name different commits) |

A restored dataset's `DatasetInfo` has `restoredFrom: {repository, backup, datasetId,
seq}`, and its directory a `restore.json`.

**Which backups belong to `/{ds}`.** By dataset id, not by name alone, so a new dataset
that reuses a deleted one's name does not reach the old one's backups: the backups of the
live dataset `ds`, and of the dataset it replaced by an in-place restore (its
`forkedFrom` id) under the same name. A `server-admin` also sees every backup taken of a
dataset named `ds`, which is how a deleted dataset is restored (disaster recovery). A
backup outside these answers `404 no-such-backup`, as a missing one does.
`sameLineage` in `GET /$/backups/{ds}` marks backups of the live dataset (or the one it
replaced).

### Backup routes

Every request body is JSON; an empty body is `{}`. Unknown fields are ignored. Tasks
answer `202` with the `Task` (see [Datasets (admin)](#datasets-admin)), once it has
started or queued, and a `Location` where noted.

| Method | Path | Needs | Description |
|--------|------|-------|-------------|
| GET | `/$/repositories` | any caller | `{repositories: (Repository \| RepositoryBrief)[]}`: every repository for `server-admin`; `{name, type, readonly, reachable}` for callers with `admin` on some dataset (to pick a target); `[]` otherwise |
| POST | `/$/repositories[?verify=false]` | `server-admin` | Register a repository (body `RepositoryConfig`). An empty location is initialized (unless `readonly`), an existing repository is attached; then the connection test runs (`?verify=false` skips it). `201` + `Location: /$/repositories/{repo}` + `Repository` with `test`. A location that cannot be reached is registered anyway, shown unreachable, with a failed `test`. `409 repository-exists` for a taken name, a location or repository id registered under another name; `409 not-a-repository` for a location that holds other files; `422 incompatible-repository` |
| GET | `/$/repositories/{repo}` | `server-admin` | `Repository` (its totals and last GC are refreshed in the background) |
| PUT | `/$/repositories/{repo}` | `server-admin` | Change the settings (body `RepositoryConfig`; `name` may be left out, and cannot change). `409 location-immutable` if `type`, `path`, `bucket`, `prefix` or `endpoint` changes; `409 read-only-config` for a repository of the config file. The repository is reopened with the new settings at its next use |
| DELETE | `/$/repositories/{repo}` | `server-admin` | Unregister (`204`); the repository's contents stay. `409 repository-in-use` while a policy backs up into it (`policies`) or a task uses it (`task`); `409 read-only-config` |
| POST | `/$/repositories/{repo}/test` | `server-admin` | The connection test: `TestReport`. Its steps create an object under `probe/` with a conditional create, create it again (expecting "already exists": conditional writes work), read it, list it and delete it |
| POST | `/$/repositories/{repo}/verify` | `server-admin` | Verify every backup and count orphaned blobs (body `{level?: "exists" \| "data"}`, default `exists`; `restore` is `400 invalid-request`): task `backup-verify` (server-wide) with `detail: VerifyReport` |
| GET | `/$/repositories/{repo}/backups` | `server-admin` | `{backups: BackupSummary[], next: string \| null}`, newest `completed` first. `?dataset=NAME`, `?datasetId=UUID`, `?policy=P`, `?limit=N` (default 100, at most 1000), `?before=T` (only backups completed before the RFC 3339 instant `T`; pass `next` to get the following page) |
| POST | `/$/repositories/{repo}/gc` | `server-admin` | Delete unreferenced blobs (body `{dryRun?: boolean, graceHours?: number}`, grace default 24): task `backup-gc` (server-wide) with `detail: GcReport`. `409 repository-read-only` |
| GET | `/$/repositories/{repo}/locks` | `server-admin` | `{locks: Lock[]}` |
| DELETE | `/$/repositories/{repo}/locks/{id}` | `server-admin` | Break a lock (`204`; audited). `404 no-such-lock`; `409 repository-read-only` |
| GET | `/$/backups/{ds}[?repository=R]` | `read` on `ds` | `{dataset, datasetId: string \| null /* the live dataset's */, backups: BackupSummary[]}`: the dataset's backups in every repository (or `R` only), newest first, with `sameLineage`. A repository that cannot be reached is left out (one found unreachable in the last minute is not tried again) |
| POST | `/$/backups/{ds}` | `admin` on `ds` | Back up now (body `{repository, name?, note?}`): task `backup-create` with `detail: BackupSummary`, `Location: /$/backups/{ds}/{repo}/{name}`. `404 no-such-dataset`, `404 no-such-repository`, `409 repository-read-only`, `409 backup-exists`, `409 backup-in-progress` (`task`: one backup of a dataset into a repository at a time), `501 backup-unsupported` (in-memory dataset), `507 insufficient-storage` (an `fs` repository whose file system has less than `--min-free-disk-mb` free; the task also fails with it when a blob would leave less) |
| GET | `/$/backups/{ds}/{repo}/{backup}` | `read` on `ds` | `Backup`: the summary with the manifest's files, blobs and upload statistics |
| DELETE | `/$/backups/{ds}/{repo}/{backup}` | `admin` on `ds` | Delete the backup's manifest (`204`); its blobs go at the next GC. `409 backup-busy` (`task`) while a restore or verification of it runs here; `409 repository-read-only` |
| POST | `/$/backups/{ds}/{repo}/{backup}/restore` | `admin` on `ds` and on the target | Restore (body `RestoreRequest`): task `backup-restore`, `Location: /$/datasets/{target}`. See [Restore](#restore) |
| POST | `/$/backups/{ds}/{repo}/{backup}/verify` | `admin` on `ds` | Verify one backup (body `{level?: "exists" \| "data" \| "restore"}`, default `exists`): task `backup-verify` with `detail: VerifyReport`; the result is remembered as the backup's `verified` |
| GET | `/$/backup-policies` | `server-admin` | `{policies: Policy[]}` |
| POST | `/$/backup-policies` | `server-admin` | Create a policy (body `PolicyConfig`): `201` + `Location: /$/backup-policies/{policy}` + `Policy`. `409 policy-exists` (a policy or repository has the name), `404 no-such-repository`, `409 repository-read-only`. It first runs at its next scheduled instant |
| POST | `/$/backup-policies/preview` | `server-admin` | Body `{schedule, timezone?: string /* UTC */, count?: number /* 5, at most 20 */, nameTemplate?, dataset?}` → `{next: string[] /* RFC 3339 UTC */, description, sample?}`; `sample` renders `nameTemplate` for `dataset` at `next[0]` |
| GET | `/$/backup-policies/{policy}` | `server-admin` | `Policy` |
| PUT | `/$/backup-policies/{policy}` | `server-admin` | Replace the settings (body `PolicyConfig`; `name` may be left out, and cannot change): `Policy`. Enabling or disabling a policy is a PUT. `409 read-only-config` for a policy of the config file. A new schedule or time zone waits for its next instant |
| DELETE | `/$/backup-policies/{policy}` | `server-admin` | `204`; a running run stops before its next dataset. `409 read-only-config` |
| POST | `/$/backup-policies/{policy}/run` | `server-admin` | Run now (also a disabled policy): task `backup-policy` (server-wide) with `detail: PolicyRun`, `Location: /$/tasks/{id}`. The schedule does not move. `409 policy-running` (`task`), `503 too-many-tasks`, `403 server-read-only` |
| POST | `/$/backup-policies/{policy}/retention[?dryRun=true]` | `server-admin` | Apply the policy's retention now: `{dryRun, delete: BackupSummary[], keep: BackupSummary[], errors?: string[]}`. With `dryRun` nothing is deleted. A deletion that fails stays in `delete` and adds to `errors` |
| GET | `/$/backup-policies/{policy}/runs[?limit=N]` | `server-admin` | `{runs: PolicyRun[]}`, newest first (default 50, at most 1000) |

A server built without the `backup` feature answers these paths `404`; the UI then hides
its Backups page.

**`--read-only` servers** create, verify and delete backups, test repositories, apply
a policy's retention, collect repositories and break locks (on writable repositories).
Restores, changes to repositories and policies, and policy runs answer
`403 server-read-only`; the scheduler runs no policies (it logs that once), so their
instants pass.

### Restore

```ts
type RestoreRequest = {
  target?: string;          // the dataset to create (default: {ds}), or to replace
  replace?: boolean;        // replace the registered dataset `target` in place
  identity?: "auto" | "new" | "keep";   // default auto (see Identity)
  check?: "quick" | "full" | "none";    // sparkles check before publishing; default quick
  keepReplaced?: boolean;   // in place: keep the old files as databases/.kept-{target}-{task}
};
```

A restore downloads into `databases/.restore-{target}-{task}` and publishes the directory
only once it is complete and checked, so a failed or cancelled restore leaves nothing
behind. It needs 1.1 × the backup's `logicalBytes` free in the data directory's file
system, plus the `--min-free-disk-mb` reserve: otherwise the request answers
`507 insufficient-storage`, and the task checks again before downloading (the free
space may have shrunk while it waited for a slot).

* **A new dataset** (`replace: false`): `target` must not exist (`409 dataset-exists`);
  its name is reserved while the task runs. The directory is renamed into
  `databases/{target}` and the dataset registered, as a clone is.
* **In place** (`replace: true`): `target` must be a persistent dataset of this server's
  data directory (`404 no-such-dataset`; `409 not-managed` for an in-memory dataset or one
  attached with `--loc`) that no backup task works on (`409 dataset-busy`, `task`).
  Once downloaded and checked, requests to `/{target}` are answered
  `503 {code: "dataset-restoring"}` with `Retry-After: 5`, never `404`. The swap waits up
  to 30 s for requests in progress to finish (`409 dataset-busy` after that, and the
  dataset stays as it was), renames `databases/{target}` to
  `databases/.replaced-{target}-{task}` and the restored directory into its place,
  reopens the dataset, and removes the old files (unless `keepReplaced`). If the new
  database fails to open, the old one is put back. A crash between the two renames is
  undone at the next start.

The task can be cancelled until it publishes the dataset (`cancellable` turns `false`
then). A restore needs `admin` on the target name too (`403 no admin access to the
target name /x`). The task's `detail` is `{backup: BackupSummary, dataset, datasetId,
identity: "kept" | "new", forkedFrom?: {id, seq}, check: object | null, millis}`.

### Backup types

```ts
type RepositoryConfig = {
  name: string;
  type: "fs" | "s3" | "gcs" | "azure";   // gcs, azure: experimental, config file only
  path?: string;            // fs: absolute; not inside the data directory or a config file's directory
  bucket?: string;          // s3, gcs, azure (azure: the container)
  prefix?: string;          // key prefix inside the bucket
  region?: string;          // s3
  endpoint?: string;        // s3: https:// (http:// with allowHttp) URL of MinIO, R2, Ceph RGW, …; no credentials, query or fragment
  pathStyle?: boolean;      // s3: path-style addressing (MinIO)
  allowHttp?: boolean;      // s3: allow an http:// endpoint
  credentials?: Credentials;             // s3; default {source: "default"}
  sse?: "AES256" | "aws:kms";            // s3 server-side encryption
  kmsKeyId?: string;        // with sse "aws:kms"
  conditionalWrites?: boolean;           // default true: creates with If-None-Match: *;
                                         // false: HEAD then PUT, one writer at a time
  readonly?: boolean;       // never write (restore, list and verify only; no locks)
  maxConcurrency?: number;  // parallel object requests (default 8 for s3, 4 otherwise)
  maxUploadBytesPerSec?: number;         // bandwidth limits shared by the repository's
  maxDownloadBytesPerSec?: number;       // tasks (default unlimited)
};

type Credentials =        // references only: Sparkles stores no secrets
  | { source: "default" }   // the AWS environment and provider chain (AWS_*, web identity, instance metadata)
  | { source: "env"; accessKeyIdVar: string; secretAccessKeyVar: string; sessionTokenVar?: string }
  | { source: "file"; path: string }     // JSON {accessKeyId, secretAccessKey, sessionToken?}, re-read at each open
  | { source: "named"; name: string };   // a [credentials.<name>] source of the backup config file

type Repository = RepositoryConfig & {
  source: "api" | "config";  // config: from --backup-config, read-only through the API
  id: string | null;         // from the repository's marker, once reached
  status: { reachable: boolean; checked: string; error?: string;
            conditionalWrites?: boolean; singleWriter: boolean };
  stats: { backups: number; datasets: number; storedBytes: number; logicalBytes: number;
           dedupRatio: number /* logical / stored */; asOf: string } | null;
  lastGc: (GcReport & { finished: string }) | null;
  policies: string[];        // policies that back up into it
  test?: TestReport;         // POST /$/repositories only
};
type RepositoryBrief = { name: string; type: string; readonly: boolean; reachable: boolean };

type TestReport = {
  ok: boolean; conditionalWrites: boolean;
  steps: { step: "create" | "create-again" | "read" | "list" | "delete";
           ok: boolean; millis: number; error?: string }[];
};

type BackupSummary = {
  name: string;
  repository: string;        // the repository's name on this server
  dataset: { name: string; id: string };
  commit: { seq: number; timestamp: string; quads: number; ref: string /* commit:<seq> */ };
  created: string; completed: string; millis: number;
  logicalBytes: number;      // the size of its files
  addedBytes: number;        // stored bytes of the blobs it uploaded first
  policy: string | null; run: string | null; note: string | null;
  sameLineage?: boolean;     // GET /$/backups/{ds} only
  verified: { level: "exists" | "data" | "restore"; status: "ok" | "error"; at: string } | null;
                             // the last verification on this server
};

type Backup = BackupSummary & {
  format: 1; generation: string /* gen-NNNN */; indexFormat: number;
  parent: string | null;     // the backup whose blobs it reused
  server: { version: string };
  files: { path: string; kind: "immutable" | "append" | "meta"; size: number;
           sha256: string; blobs: { id: string; size: number }[] }[];
  stats: { logicalBytes: number; addedBytes: number; files: number; blobs: number;
           newBlobs: number; reusedBlobs: number };
  derived: { text: { rebuildOnRestore: boolean } | null };
};

type VerifyReport = {
  level: "exists" | "data" | "restore";
  status: "ok" | "warning" /* orphans only */ | "error";
  backups: { name: string; status: "ok" | "error";
             missing: string[];   // blobs missing or of the wrong size
             corrupt: string[];   // blobs whose content does not hash to their id
             check?: object }[];  // restore: the sparkles check report
  orphans?: { blobs: number; bytes: number };   // repository verification only
  requests: { list: number; head: number; get: number };
  millis: number;
};

type GcReport = {
  dryRun: boolean; manifests: number; referencedBlobs: number; listedBlobs: number;
  candidates: number;        // unreferenced blobs
  deleted: number; deletedBytes: number;   // dry run: what a real run would delete
  keptYoung: number;         // unreferenced, but younger than the grace period
  storedBytesAfter: number;
  requests: { list: number; get: number; delete: number };
  millis: number; lockWaitMillis: number;
};

type Lock = {
  id: string; kind: "shared" | "exclusive";
  operation: "create" | "restore" | "verify" | "delete" | "gc";
  holder: { host: string; pid: number; server: string /* hash of its data directory; "" for the CLI */; version: string };
  created: string;
  lastModified: string;      // the storage server's time of its last refresh
  stale: boolean;            // not refreshed for 30 min: ignored, removed by GC
};

type PolicyConfig = {
  name: string;
  repository: string;
  datasets?: string[];       // names or * globs; default ["*"] (in-memory datasets are skipped)
  schedule: string;          // cron, or "every <duration>"
  timezone?: string;         // IANA name; default UTC
  nameTemplate?: string;     // default "{policy}-{dataset}-{time}"
  retention?: { expireAfter?: string | null; minCount?: number /* 1 */; maxCount?: number | null };
  skipUnchanged?: boolean;   // skip a dataset whose head is its last policy backup's commit
  gcAfterRetention?: boolean;   // collect the repository after retention deleted something
  catchUp?: "one" | "none";  // default one
  enabled?: boolean;         // default true
};

type Policy = PolicyConfig & {
  source: "api" | "config";
  state: { nextRun: string | null; lastScheduledFor: string | null; lastRun: PolicyRun | null;
           lastSuccess: string | null; consecutiveFailures: number; runningTask: string | null };
};

type PolicyRun = {
  id: string; policy: string;
  trigger: "schedule" | "catch-up" | "manual";
  scheduledFor: string | null; started: string; finished: string | null;
  result: "ok" | "partial" | "failed" | "skipped";
  reason?: string;              // why a scheduled run was skipped
  datasets: { dataset: string; backup: string | null; result: "ok" | "failed" | "skipped";
              reason?: string; addedBytes?: number; millis?: number }[];
  retention: { deleted: string[]; error?: string } | null;
  gc: { task: string } | null;   // the backup-gc task it started
};
```

Timestamps are RFC 3339 in UTC with milliseconds; sizes are bytes.

### Lifecycle policies

A policy backs up the datasets matching `datasets` into `repository` on a schedule, one
dataset after another (each a backup like `POST /$/backups/{ds}`, holding a task slot
while it runs), then applies its retention.

* **Schedules.** Cron with 5 fields (minute hour day-of-month month day-of-week), or 6
  with seconds first (no `@daily`-style macros), evaluated in the policy's `timezone`: a
  local time skipped by a daylight-saving change runs at the first instant after the
  gap, a repeated one runs once, at its first occurrence. `every <duration>` (at least
  one minute) counts from the Unix epoch in UTC, so `every 6h` runs at 00:00, 06:00, …
  UTC whatever the time zone or restarts. A schedule that does not parse, or never runs,
  and an unknown time zone are `400 invalid-schedule`.
* **Durations** (`expireAfter`, `every`): one or more `<n><unit>` terms, such as `30d`,
  `12h`, `1w`, `90m`, `1d 12h`; units `s`, `m`, `h`, `d`, `w` (or `sec`, `min`, `hr`,
  `hour`, `day`, `week`, and plurals).
* **Name templates.** `{policy}`, `{dataset}`, `{seq}` (the head commit), `{run}` (the
  first 8 hex digits of the run id), `{time}` (the scheduled instant,
  `YYYYMMDDtHHMMSSz` in UTC), `{date:FMT}` (the scheduled instant in the policy's zone;
  `%Y %m %d %H %M %S %j %V`). The result must be a valid backup name; characters of the
  dataset name outside the grammar become `-`, and a long dataset name is shortened to
  keep the result within 64 characters. A name that is taken gets `-2`, `-3`, …
* **Retention** considers only the policy's own backups in its repository, per dataset
  id, newest `completed` first: the first `minCount` are kept; of the others, those at
  a position ≥ `maxCount` or completed more than `expireAfter` ago are deleted, except
  backups a restore or verification uses at that moment (kept until the next run).
  Deleting a backup removes its manifest; GC removes the blobs. With `gcAfterRetention`
  a run whose retention deleted something starts a `backup-gc` task, at most once per
  24 h per repository.
* **The scheduler** wakes at least once a minute. A new or rescheduled policy waits for
  its next instant. Instants missed while the server was down are covered by one run
  60 s after startup (`trigger: "catch-up"`), or recorded as `skipped` with
  `catchUp: "none"`. An instant that comes while the previous run still runs, or while
  the backup task queue is full (see [Backup tasks](#backup-tasks)), is recorded as
  `skipped`, with the `reason`. A disabled policy lets its instants pass; disabling or
  deleting one during a run stops it before its next dataset (the run ends `skipped`).
  A run is a backup task like any other: it is admitted to the queue (a manual run
  beyond it answers `503 too-many-tasks`), and so is the GC it starts (not started, and
  tried again after the next run's retention, when the queue is full).
* **Results.** A run is `ok` when every selected dataset was backed up or skipped
  (`in-memory dataset`, `unchanged`), `partial` when some failed, `failed` when none
  succeeded. `lastSuccess` and `consecutiveFailures` follow them. The last 1000 runs of
  all policies are kept.

### Backup tasks

| Kind | `dataset` | `target` | `detail` |
|---|---|---|---|
| `backup-create` | the dataset | the backup name | `BackupSummary` |
| `backup-restore` | `{ds}` of the route | the dataset created or replaced | see [Restore](#restore) |
| `backup-verify` | the dataset, or `""` for a repository | the backup name, or the repository | `VerifyReport` |
| `backup-gc` | `""` | the repository | `GcReport` |
| `backup-policy` | `""` | the policy | `PolicyRun` |

Server-wide tasks (`dataset: ""`) are listed for `server-admin` only. Backup tasks take
their own slots: at most `sparkles serve --backup-max-tasks` (default 2) run at once, the
others wait `queued`. Up to 4 more per slot may wait; past that a request answers
`503 too-many-tasks`. Every backup task can be cancelled (`DELETE /$/tasks/{id}`), while
queued too; cancellation is checked between object requests and every 8 MiB of data,
and the task ends `cancelled`. A failed task's `message` starts with the error code
(`repository-unavailable: …`). A cancelled backup leaves only unreferenced blobs; a
cancelled restore leaves the target as it was.

### Backup errors

Errors are `{error, code, requestId}` plus, for some codes, `task`, `holder`,
`policies` or `field`.

| Status | `code` |
|---|---|
| 400 | `invalid-name`; `invalid-config` (a repository or policy setting, a repository body that is JSON but not a repository configuration, a refused destination; `field` names the setting); `invalid-request` (a body that is not JSON, on every route; a backup, restore, verification, GC or policy body of the wrong shape; a malformed query parameter; a repository verification at level `restore`; a negative `graceHours`); `invalid-schedule` (also an unknown time zone) |
| 403 | `server-read-only` |
| 404 | `no-such-repository`, `no-such-backup` (also a backup of another dataset), `no-such-policy`, `no-such-dataset`, `no-such-lock` |
| 409 | `repository-exists`, `policy-exists`, `not-a-repository` (a location with other files), `location-immutable`, `repository-in-use` (`policies` or `task`), `read-only-config`, `backup-exists`, `backup-in-progress` (`task`), `backup-busy` (`task`), `repository-read-only`, `repository-locked` (a conflicting lock outlived the 10 min wait; `holder`), `dataset-exists`, `dataset-busy` (`task`), `not-managed`, `duplicate-dataset-id`, `policy-running` (`task`) |
| 422 | `incompatible-repository` (a newer repository format, or encryption), `incompatible-format` (an index format this build cannot read), `invalid-backup` (a manifest that fails validation; `field`) |
| 500 | `restore-mismatch`, `internal` |
| 501 | `backup-unsupported` (an in-memory dataset); `not-implemented` |
| 502 | `repository-unavailable` (a storage error after retries; messages never include URL query strings) |
| 503 | `catalog-lagging` (the commit catalog could not be flushed; retry), `too-many-tasks`, `dataset-restoring` (with `Retry-After: 5`); `cancelled` (a task's) |
| 507 | `insufficient-storage` (a restore without room for 1.1 × the backup's size plus the `--min-free-disk-mb` reserve; a backup into an `fs` repository whose file system would keep less than the reserve) |

Callers without `server-admin` see absolute paths in these messages cut to their last
component. Storage requests are retried with exponential backoff (up to 10 retries within
3 minutes each); a downloaded blob that fails its hash is fetched again twice.

### Locks and garbage collection

Operations take a lease object `locks/<uuid>.json` in the repository: a **shared** lock
for create, delete, restore and verify, an **exclusive** one for GC's sweep. A held lock
is rewritten every 5 minutes; one not rewritten for 30 minutes (by the storage server's
clock, never this host's) is **stale**: ignored by others and removed by GC. An
operation that meets a conflicting lock retries with backoff for up to 10 minutes, then
fails with `409 repository-locked`. Read-only repositories take no locks. So several
servers, and the CLI, can share a repository.

GC marks the blobs every manifest references under a shared lock (backups continue),
then takes the exclusive lock, lists the manifests again and deletes the unreferenced
blobs older than the grace period (default 24 h, again by the storage server's clock).
The last GC's report is kept in the repository (`gc/last.json`, `lastGc`); a dry run
reports what a real run would delete and changes nothing.

A repository with `conditionalWrites: false` (or a service without conditional creates,
found by the connection test: `status.singleWriter`) must have one writer at a time.

### Configuration file

`sparkles serve --backup-config FILE` (or `$SPARKLES_BACKUP_CONFIG`) reads repositories,
policies and the limits of API registrations from a TOML file. Keys are snake_case
forms of the JSON fields; unknown keys are errors (with line and column), and a file that
does not load stops the server at startup. SIGHUP re-reads it: its repositories and
policies replace the previous ones, the API's stay; a file that does not load leaves
everything as it was (logged). Its entries have `source: "config"` and answer
`409 read-only-config` to `PUT` and `DELETE`. The file holds no secrets, only references
to them; the server warns when it is readable by group or others. `sparkles repo add`
and `repo remove` edit the same file for the CLI.

```toml
version = 1

[repositories.local]
type = "fs"
path = "/srv/backups/sparkles"

[repositories.s3-main]
type = "s3"
bucket = "kg-backups"
prefix = "prod/sparkles"
region = "eu-central-1"
credentials = { source = "file", path = "/run/secrets/sparkles-s3.json" }
sse = "aws:kms"
kms_key_id = "arn:aws:kms:eu-central-1:111122223333:key/…"
max_upload_bytes_per_sec = 104857600
# also: endpoint, path_style, allow_http, conditional_writes, readonly,
# max_concurrency, max_download_bytes_per_sec

[repositories.minio]
type = "s3"
bucket = "lab"
endpoint = "http://127.0.0.1:9000"
path_style = true
allow_http = true
credentials = { source = "named", name = "lab" }

[policies.nightly]
repository = "s3-main"
datasets = ["*"]
schedule = "30 2 * * *"
timezone = "Europe/Berlin"
name_template = "{policy}-{dataset}-{date:%Y%m%d}"
retention = { expire_after = "30d", min_count = 7, max_count = 60 }
gc_after_retention = true
# also: skip_unchanged, catch_up = "one" | "none", enabled

# credential sources, by name; the only ones repositories registered through the API may use
[credentials.lab]
source = "env"                  # or "file" (path = …), or "default"
access_key_id_var = "LAB_ACCESS_KEY"
secret_access_key_var = "LAB_SECRET_KEY"
# session_token_var = "LAB_SESSION_TOKEN"

# limits of repositories registered through the API
[api]
fs_roots = ["/srv/backups"]
```

**Repositories registered through the API** (and the UI) are held to the operator's
choices, since a caller could otherwise point the server's credentials or its network
access wherever they like:

* `s3` credentials can only be `{"source": "named", "name": …}`, naming a
  `[credentials.<name>]` source of the config file (`400 invalid-config`, `field:
  "credentials"` or `"credentials.name"`, otherwise); never environment variables, files
  or the default provider chain of the caller's choosing. Without a config file an `s3`
  repository cannot be registered through the API. The flow: define the source in the
  config file (by hand, or `sparkles repo add NAME --s3 BUCKET … --credentials-name lab
  --credentials env:LAB_ACCESS_KEY,LAB_SECRET_KEY`, which writes `[credentials.lab]`
  next to its own repository, or names an existing one without `--credentials`), start
  the server with `--backup-config` on that file or send it SIGHUP, then register:

  ```sh
  curl -X POST http://localhost:3030/$/repositories -H 'Content-Type: application/json' -d '{
    "name": "lab", "type": "s3", "bucket": "lab", "endpoint": "http://127.0.0.1:9000",
    "pathStyle": true, "allowHttp": true,
    "credentials": {"source": "named", "name": "lab"}}'
  ```
* An `s3` endpoint, and every address its host name resolves to, must pass the server's
  outbound policy (the `--outbound-*` flags of `SERVICE` and `LOAD`: public addresses
  only by default), and connections go only to the addresses checked. A MinIO on
  localhost needs `--outbound-allow 127.0.0.1` or `--outbound-allow-private`. A refused
  endpoint is `400 invalid-config` (`field: "endpoint"`). These connections never go
  through a proxy of the environment (`HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`), which
  would reach the endpoint past the address checks; repositories of the config file, and
  the CLI's, use the environment's proxies as usual.
* `fs` repositories must lie under one of `[api] fs_roots` when it is set.
* `gcs` and `azure` repositories use the server's own credentials and can only come from
  the config file; `memory` is for tests.

Every `fs` repository, from either source, lies outside the data directory and outside
the directories of the server's config files (`--backup-config`, `--auth-config`).

### Backup metrics

| Name | Type | Labels |
|------|------|--------|
| `sparkles_backup_operations_total` | counter | `repository`, `operation` = `create` \| `restore` \| `verify` \| `delete` \| `gc`, `result` = `ok` \| `failed` \| `cancelled` |
| `sparkles_backup_operation_duration_seconds` | histogram (1 s … 2 h) | `operation` |
| `sparkles_backup_bytes_uploaded_total`, `…_bytes_downloaded_total` | counter | `repository` |
| `sparkles_backup_blobs_uploaded_total`, `…_blobs_reused_total` | counter | `repository` |
| `sparkles_backup_object_requests_total` | counter (every storage request of every operation: backups, restores, verifications, GC, listings, connection tests, locks; "not found" and "already exists" answers are `ok`, a failed request is `error`) | `repository`, `op` = `put` \| `get` \| `head` \| `list` \| `delete`, `result` = `ok` \| `error` |
| `sparkles_backup_last_success_timestamp_seconds` | gauge (the last backup of the dataset into the repository) | `dataset`, `repository` |
| `sparkles_backup_capture_lock_seconds` | histogram (0.5 ms … 1 s; the writer-lock hold of a capture) | |
| `sparkles_backup_repository_stored_bytes`, `…_logical_bytes`, `…_backups` | gauge (from the last listing) | `repository` |
| `sparkles_backup_lock_conflicts_total` | counter (`repository-locked` failures) | `repository` |
| `sparkles_backup_policy_runs_total` | counter | `policy`, `result` |
| `sparkles_backup_policy_last_success_timestamp_seconds`, `…_next_run_timestamp_seconds`, `…_consecutive_failures` | gauge | `policy` |

`repository` is capped like `dataset` (`--metrics-max-datasets`; the rest share
`$other`). Each backup task is a span `task backup-*` with children such as
`backup.capture` (`sparkles.commit`, `sparkles.backup.lock_ms`). Task starts and ends are
logged under `sparkles::backup`; `repository_added`, `repository_changed`,
`repository_removed`, `backup_deleted`, `restore_started`, `restore_finished`,
`gc_finished`, `lock_broken` and `policy_changed` go to the `sparkles::audit` target with
the principal.

### Files on the server

Under `<data>/backup/`: `repositories.json` and `policies.json` (the API's entries),
`policy-state.json` (when each policy last ran and succeeded), `runs.json` (the run
history), `verify.json` (each backup's last verification on this server: `verified`) and
`cache/<repository id>/` (a manifest cache). Restores use
`<data>/databases/.restore-*` and `.replaced-*` (removed or undone at the next start),
`.kept-*` (`keepReplaced`), and restore-level verifications `<data>/tmp/verify-*`.

**The data-directory lock.** `sparkles serve` holds an OS lock on
`<data>/sparkles-server.lock` while it runs: a second server on the same data directory
fails at startup, and an offline `sparkles backup restore --data` refuses to write into
it.

A repository's own layout (format 1), shared by the server and the CLI:

```text
<prefix>/
  sparkles-repo.json            marker: format, repository id, piece size (created once)
  blobs/<hh>/<sha-256>          content blobs, immutable (<hh>: the id's first two hex digits)
  backups/<name>.json           manifests, immutable, created last
  locks/<uuid>.json             lock leases
  probe/<uuid>                  connection-test objects
  gc/last.json                  the last GC's report
```

A blob is a 16-byte header (`SPKB`, format 1, codec: raw or LZ4, encryption: none, the
plaintext length) and its payload; its id is the SHA-256 of the plaintext, so writers
that compress differently still deduplicate.

## Full-text search

Datasets can index their string and language-tagged literals for ranked (BM25) search,
queried with Jena's `text:query` property function (`PREFIX text: <http://jena.apache.org/text#>`):

```sparql
SELECT ?s ?score ?label WHERE {
  (?s ?score ?label) text:query (rdfs:label "brown fox" 10 "lang:en") .
  ?s a ex:Book .
} ORDER BY DESC(?score)
```

* **Subject list** `(?s ?score ?literal ?graph ?predicate)`: every slot after the
  subject is optional. A constant subject restricts the search to that subject.
* **Object**: a query string, or `(predicate* "query" limit "lang:xx")`. A language tag
  on the query string acts as `lang:`.
* **Query syntax**: terms, `"phrases"` (with `~slop`), `AND`/`OR` (OR by default),
  `+required`/`-excluded`, parentheses, and phrase prefixes `"quick bro"*`. A literal `:`
  is written `\:`.
* **Results**: one solution per matching quad (a subject with two matching literals
  appears twice). `?score` is an `xsd:float`. In a merged default graph (union default
  graph, several `FROM`s), identical triples from different graphs count once.
* **Evaluation**: the search runs once within the active graph (`GRAPH`, `FROM`,
  `reasoning=false` apply inside the search), so `limit` is the top n of that scope,
  before any join.
* **Analyzer**: tokens split on non-alphanumeric characters, lowercased and ASCII-folded
  (`café` matches `cafe`).
* **Consistency**: indexes are updated in the same commit as the data, so a query sees
  the text of its own snapshot, including the writes just before it. A write only stages
  its documents; the index commit (a new segment) happens at the next text query that
  needs it, about once a second, or as soon as about 16,000 changes are staged, so a
  burst of writes shares one. If an index is
  behind (a failed update, a rebuild in progress), text queries return `503` until it is
  rebuilt. They never return stale results.
* **Durability**: index commits are not fsynced; the write-ahead log is the durable
  record. The index is checkpointed (synced) about once a second while writes continue,
  before compaction and on close. After a crash, an index with unsynced changes
  (`text.dirty` next to it) is checksum-verified and caught up from the WAL (which also
  restores what was only staged); it is rebuilt only if it is damaged or older than the
  WAL.
* **Errors**: `400` for malformed calls, unparseable query strings, predicates that
  are not indexed, and datasets without an index. `501` if the server was built without
  the `text` feature.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/$/text/{ds}` | `TextStatus` (below), or `{ "enabled": false }` |
| PUT | `/$/text/{ds}` | Enable or reconfigure; the body is a `TextConfig` (empty: defaults). `202` with the build `Task` (`kind: "text-rebuild"`) |
| DELETE | `/$/text/{ds}` | Disable and delete the index (`204`) |
| POST | `/$/text/{ds}/rebuild` | Rebuild from the current data (`202` Task; `409` if one is running; `400` if not enabled) |

```ts
type TextConfig = {
  predicates?: "all" | string[];                         // default "all"
  graphs?: { include?: "all" | string[]; exclude?: string[] };  // graph IRIs; urn:x-arq:DefaultGraph
  maxTextBytes?: number;                                // default 262144 (longer text is indexed truncated)
  maxHits?: number;                                     // default 1000000 hits without a limit (then 507)
  docstoreCompression?: "zstd" | "lz4" | "none";         // default zstd; a change rebuilds
};
type TextStatus = {
  enabled: true; state: "ready" | "stale"; docs: number;
  seq: number; storeSeq: number;       // ready when equal: the commit the index reflects
  epoch: number; diskBytes: number; segments: number;
  config: TextConfig; formatVersion: 1;
  lastRebuild?: { at: string; ms: number; docs: number }; message?: string;
};
```

Dataset info (`/$/datasets`) has `text: null | { state, docs }`. The configuration lives
in the database directory (`text.json`, index in `text/`). CLI:
`sparkles text-index --loc DB [--predicate IRI…] [--exclude-graph IRI…] [--rebuild | --status | --disable]`,
and `sparkles serve --text NAME[=config.json]`.

## Vector similarity

Embeddings are ordinary literals of the datatype `<urn:x-sparkles:vector>`: a JSON array
of 1–16384 finite numbers (`"[0.1, -0.2, 0.3]"^^spk:vector`, with
`PREFIX spk: <urn:x-sparkles:>`), read as `f32`. Literals are stored and returned exactly
as written; one that does not parse is stored but never matched.

* **Functions** (type error on a malformed argument, a dimension mismatch, or a zero
  vector with cosine): `spk:cosine(?a, ?b)`, `spk:dot(?a, ?b)`,
  `spk:euclidean(?a, ?b)` (L2 distance) and `spk:dimension(?a)`.
* **Exact top-k search:**

  ```sparql
  SELECT ?s ?score WHERE {
    (?s ?score ?vector) spk:vectorSearch (ex:emb "[0.1, -0.2, 0.3]"^^spk:vector 10 "metric:cosine") .
  } ORDER BY DESC(?score)
  ```

  * The first argument is the embedding predicate.
  * The query is a vector literal, or an entity whose single vector under that
    predicate is used.
  * `k` defaults to 10 (at most 10000). `metric:` is `cosine` (default), `dot` or
    `euclidean`.
  * Higher scores are better, except for euclidean, where lower is better.
  * The search covers the active graph, and `GRAPH ?g` binds each row's graph. The top
    k are taken before any join, and ties break by term id.
  * Only vectors of the query's dimension are compared. If the predicate has vectors
    but none of that dimension, the result is a `400` naming the dimensions it has.
  * Rows with the same subject and vector in several graphs of a merged default graph
    count once.
* **Implementation:** vectors are packed per predicate and dimension on first use and
  cached per index generation. Every query overlays its snapshot's uncommitted inserts
  and deletes, so results always match its data. A process-wide budget (default 4 GiB)
  caps the packed vectors (`sparkles serve --vector-memory-mb`, default 4096); beyond it
  a search returns `507`. A variable query vector gives `501`.
* **Status:** `GET /$/vector/{ds}` returns
  `{ budgetBytes, usedBytes, generation, predicates: [{ predicate, bytes, malformed, dimensions: [{ dimension, vectors }] }] }`
  for the predicates packed so far in the current generation (packing happens on a
  predicate's first search; `vectors` counts a vector once per graph it is in).

## GeoSPARQL

Built with the `geo` cargo feature (on in the server), Sparkles implements the GeoSPARQL
1.1 functions over geometry literals, Jena's `spatial:` property functions, and a spatial
index per dataset. Prefixes: `geo:` `<http://www.opengis.net/ont/geosparql#>`, `geof:`
`<http://www.opengis.net/def/function/geosparql/>`, `uom:`
`<http://www.opengis.net/def/uom/OGC/1.0/>`, `sf:` `<http://www.opengis.net/ont/sf#>`,
`spatial:` `<http://jena.apache.org/spatial#>`.

```sparql
SELECT ?f ?d WHERE {
  ?f geo:hasDefaultGeometry/geo:asWKT ?w .
  FILTER(geof:sfWithin(?w, "POLYGON((2.2 48.8, 2.5 48.8, 2.5 48.9, 2.2 48.9, 2.2 48.8))"^^geo:wktLiteral))
  BIND(geof:distance(?w, "POINT(2.2945 48.8584)"^^geo:wktLiteral, uom:kilometre) AS ?d)
} ORDER BY ?d
```

**Literals.**

* `geo:wktLiteral`: WKT with an optional leading CRS IRI (`<http://…/EPSG/0/4326> POINT(48.86 2.34)`),
  Z, M and ZM layouts, `EMPTY`, and the empty string (an empty geometry). `LINEARRING`,
  `TRIANGLE`, `TIN` and `POLYHEDRALSURFACE` are read as line strings and polygons, keeping
  their type for `geof:geometryType`.
* `geo:geoJSONLiteral`: an RFC 7946 geometry (always CRS84).
* Literals are stored as written: `"POINT(1 2)"` and `"Point (1.0 2.0)"` are different
  terms with equal geometries (`=` compares terms, `geof:sfEquals` geometries). A literal
  that does not parse is stored all the same; functions give a type error on it and the
  index skips it. A malformed geometry **constant** in a query is a `400`
  (`geo: malformed wktLiteral at offset N: …`).
* **CRSs.** CRS84 (the default, longitude first), CRS84h, EPSG:4326 and EPSG:4979
  (latitude first, as the EPSG definition says; GeoSPARQL Req 16), the legacy
  `http://www.opengis.net/def/crs/EPSG/4326` (longitude first, as in Jena), and Web
  Mercator (EPSG:3857); `https` forms, URNs and other EPSG versions are accepted as
  aliases. A literal in another CRS is a valid geometry: accessors, constructions and
  relations between geometries of that same CRS work, metric functions and mixes with
  other CRSs are type errors, and the index leaves it out.
* **Units.** OGC (`uom:metre`, `uom:kilometre`, `uom:mile`, `uom:degree`, `uom:radian`,
  …), QUDT (`http://qudt.org/vocab/unit/KiloM`, …) and EPSG URNs, as IRIs or
  `xsd:anyURI` literals; an unknown unit is a type error.

**Functions** (a type error on any bad argument: unbound in BIND, false in FILTER). A
geometry result has the datatype and CRS of the first geometry argument; the second
geometry of a binary function is transformed into the first one's CRS.

| Functions | Result |
|---|---|
| the 24 relations: `sfEquals` `sfDisjoint` `sfIntersects` `sfTouches` `sfWithin` `sfContains` `sfOverlaps` `sfCrosses`, `ehEquals` `ehDisjoint` `ehMeet` `ehOverlap` `ehCovers` `ehCoveredBy` `ehInside` `ehContains`, `rcc8eq` `rcc8dc` `rcc8ec` `rcc8po` `rcc8tppi` `rcc8tpp` `rcc8ntpp` `rcc8ntppi`; `relate(g1, g2, "T*F**FFF*")` | `xsd:boolean`, from the DE-9IM matrix (planar, in longitude/latitude for geographic CRSs) |
| `distance(g1, g2, unit)`, `metricDistance(g1, g2)` | `xsd:double`: geodesic on WGS 84 by default (`"distance": "haversine"` in `geo.json`: on a sphere), Euclidean in projected CRSs; an angle unit gives the central angle |
| `buffer(g, r, unit)`, `metricBuffer(g, r)`, `convexHull`, `envelope`, `boundary`, `centroid`, `intersection`, `union`, `difference`, `symDifference` | geometry (2D). A metric buffer on geographic data goes through a local projection (up to 1000 km) |
| `area(g, unit)`, `length`, `perimeter` and their `metric…` forms | `xsd:double`, geodesic on geographic CRSs |
| `getSRID` | `xsd:anyURI` |
| `transform(g, crs)`, `asWKT`, `asGeoJSON` | geometry |
| `dimension`, `coordinateDimension`, `spatialDimension`, `numGeometries` | `xsd:integer` |
| `is3D`, `isMeasured`, `isEmpty` | `xsd:boolean` |
| `geometryType` | `xsd:anyURI` (`sf:Point`, …) |
| `geometryN(g, n)` (1-based) | geometry |
| `minX` `minY` `maxX` `maxY` (in the literal's own axis order), `minZ` `maxZ` | `xsd:double` |

Operations over more input vertices than `serve --geo-op-vertices` (2,000,000) are type
errors; constructed geometries count against the query's memory budget.

**`spatial:` property functions** (Jena's syntax; constant arguments):

```sparql
SELECT ?f WHERE { ?f spatial:nearby (48.8566 2.3522 5 uom:kilometre 10) }   # lat lon radius [unit [limit]]
```

| Function | Arguments | Features whose geometry … |
|---|---|---|
| `nearby`, `withinCircle` | `(lat lon radius [unit [limit]])` | is within the radius (default unit kilometres) of the EPSG:4326 point |
| `nearbyGeom`, `withinCircleGeom` | `(geom radius [unit [limit]])` | is within the radius of `geom` |
| `withinBox` / `intersectBox` | `(latMin lonMin latMax lonMax [limit])` | is within / intersects the box |
| `withinBoxGeom` / `intersectBoxGeom` | `(geom [limit])` | is within / intersects `geom`'s envelope |
| `north` `south` `east` `west` | `(lat lon [limit])` | has an envelope beyond the point in that direction |
| `northGeom` … `westGeom` | `(geom [limit])` | the same from `geom`'s envelope |

The subject is the feature: `?f geo:hasDefaultGeometry ?g` or `?f geo:hasGeometry ?g`
(the `featureLinks`) with `?g` holding a matching serialization. One solution per
feature; under `GRAPH ?g` the graph of the serialization. With a `limit`, the nearest
matches (to the box's centre for the box and cardinal functions), ties by subject. Every
match is tested exactly. Without an index (off, building, failed) the answers are the
same, computed by a scan. Errors: `400` (`spatial:<name>: …`) for a malformed argument
list, a coordinate out of range, an unknown unit or a non-integer limit; `501` for a
variable argument and in a build without the `geo` feature.

**The spatial index.** Optional per dataset. It indexes the geometry literals of the
configured predicates (`geo:asWKT`, `geo:asGeoJSON`, `geo:hasSerialization` by default)
in a packed R-tree over the generation's base, plus an overlay of the rows committed
since. Every snapshot sees exactly its own rows, so a query may use the index at any
commit; an update's own uncommitted changes and past states (`?at=`) run without it.
FILTERs with one of the relations (but the disjoint ones), `relate` with a pattern that
needs an intersection, or `distance`/`metricDistance` compared with a constant, over the
object of an indexed predicate and a constant geometry, search the index
(`SpatialScan` in EXPLAIN); the `spatial:` functions do too (`SpatialPf`). Results are the
same with and without it: every candidate is tested exactly, and literals the index skips
although a function could still match them are candidates of every search, whatever its
window. Those are literals over `maxGeometryBytes` and literals in a built-in CRS whose
envelope has no place in longitude and latitude. Malformed literals, literals over
`maxVertices`, empty geometries and literals in an unknown CRS are never candidates: no
relation or distance with a constant can hold for them (they are type errors or empty).
A `spatial:` call with a constant subject reads that feature's links directly, index or
not. The index lives in memory: it is built when the database is opened (queries run
without it meanwhile), and again for each new generation (bulk loads, compaction).

Each spatial operator in an executed plan reports `counters`: `candidates` (rows the index
or the scan handed out), `rechecked` (those among them the index could not place),
`refined` (exact tests run), `matched` (rows that passed), `treeNodesVisited`, `index`
(`ready`; `building (37%)`, `failed`, `over-budget`, `off`, … when the plan ran without
it; `feature-links` for a `spatial:` call with a constant subject) and `fallback` (the
rows came from a scan instead of the index).

| Method | Path | Description |
|--------|------|-------------|
| GET | `/$/geo/{ds}` | `GeoStatus` (below), or `{ "enabled": false }` |
| PUT | `/$/geo/{ds}` | Enable or reconfigure; the body is a `GeoConfig` (empty: defaults). `202` with the build `Task` (`kind: "geo-index"`); `400 invalid geo configuration: …`; `409 spatial index build already running` |
| DELETE | `/$/geo/{ds}` | Disable (`204`); removes `geo.json` |
| POST | `/$/geo/{ds}/rebuild` | Rebuild the current generation's base (`202` Task; `400 spatial index is not enabled`; `409` if a build runs) |

```ts
type GeoConfig = {
  predicates?: string[];        // serialization predicates; default geo:asWKT, geo:asGeoJSON, geo:hasSerialization
  featureLinks?: string[];      // default geo:hasDefaultGeometry, geo:hasGeometry
  graphs?: { include?: "all" | string[]; exclude?: string[] };   // as for full-text search
  distance?: "geodesic" | "haversine";   // default "geodesic"
  maxGeometryBytes?: number;    // default 16 MiB: longer literals are not indexed
  maxVertices?: number;         // default 1000000 per geometry: not indexed, and a type error in functions
  wgs84?: boolean;              // not supported yet (true: 400)
  queryRewrite?: boolean;       // not supported yet (true: 400)
  formatVersion?: 1;
};
type GeoStatus = {
  enabled: true;
  state: "ready" | "building" | "failed" | "over-budget";
  progress?: number; message?: string;
  generation: string;           // the generation the base was built for
  commit: number;               // the commit the status describes
  rows: { base: number; overlay: number; tail: number };
  literals: number;             // distinct parsed geometries
  skipped: { malformed: number; unknownCrs: number; tooLarge: number; empty: number };
  crs: { [iri: string]: number };   // literals per CRS, unknown ones included
  memory: { treeBytes: number; geometryBytes: number; overlayBytes: number; budgetBytes: number };
  config: GeoConfig; formatVersion: 1;
  lastBuild?: { at: string; ms: number; rows: number };
};
```

`over-budget`: the index would need more than `serve --geo-mb` (4096 MiB); queries run
without it. `failed`: a build or a commit's update of the index failed (the write itself
never fails because of the index); queries run without it until a rebuild or a
compaction. The configuration lives in the database directory (`geo.json`; `sparkles
check` validates it, clones copy it, backups include it). CLI:
`sparkles geo-index --loc DB [--predicate IRI…] [--feature-link IRI…] [--exclude-graph IRI…] [--distance geodesic|haversine] [--rebuild | --status | --disable]`,
and `sparkles serve --geo NAME[=geo.json]`.

### Hulls, aggregates, Jena filter functions, UTM and conversion

**More `geof:` functions** (planar in the CRS of `g`, like `convexHull`):

| Function | Result |
|---|---|
| `boundingCircle(g)` | the smallest circle holding every vertex of `g` (Welzl), as a polygon of 128 sides drawn around the circle, so every input point is inside it; a single point for one distinct point |
| `concaveHull(g)`, `concaveHull(g, targetPercent)` | `geo`'s concave hull (concaveman) of the vertices. `targetPercent` in (0, 100] sets the concavity linearly, `targetPercent / 25` (50 is the default concavity 2.0, smaller values follow the input more closely), and 100 is the convex hull; outside the range: a type error. Degenerate inputs give their convex hull (a point, a segment) |
| `isSimple(g)` | `xsd:boolean`, OGC simplicity: points always; multipoints without repeated points; curves that do not meet themselves except consecutive segments at their shared vertex and a closed curve at its closing vertex; multicurves whose members meet only at ends of both; polygons with simple rings; collections with simple members |

**Aggregates.** `geof:aggBoundingBox`, `geof:aggBoundingCircle`, `geof:aggCentroid` (the
centroid of the union), `geof:aggConvexHull`, `geof:aggConcaveHull` (the default
concavity: an aggregate takes one expression) and `geof:aggUnion` group like `SUM`,
`DISTINCT` included:

```sparql
SELECT ?region (geof:aggUnion(DISTINCT ?w) AS ?shape)
WHERE { ?f ex:region ?region ; geo:hasDefaultGeometry/geo:asWKT ?w }
GROUP BY ?region
```

The result has the datatype and CRS of the group's first value; the others are
transformed into that CRS. A value that is an error or not a geometry, a CRS without a
transform, more input vertices than `--geo-op-vertices`, or an empty group make the
aggregate unbound. The aggregate IRIs are not functions (`BIND(geof:aggUnion(?w) AS ?u)`
is a syntax error). Without the `geo` feature they still group, with an unbound value.

**Jena filter functions** (`spatialF:` `<http://jena.apache.org/function/spatial#>`,
Jena's argument forms: a unit, datatype or CRS may be an IRI, an `xsd:anyURI` literal or
a plain string):

| Function | Result |
|---|---|
| `convertLatLon(lat, lon)` | EPSG:4326 `POINT(lat lon)` (numbers or numeric strings; latitude within ±90, longitude within ±180) |
| `convertLatLonBox(latMin, lonMin, latMax, lonMax)` | EPSG:4326 `POLYGON` |
| `equals(g1, g2)` | `geof:sfEquals` |
| `nearby(g1, g2, radius, unit)`, `withinCircle` | `xsd:boolean`: distance `<` radius |
| `distance(g1, g2, unit)` | `geof:distance` |
| `greatCircle(lat1, lon1, lat2, lon2, unit)` | `xsd:double` in a length unit, under the dataset's distance model (geodesic by default; `haversine` gives Jena's numbers) |
| `greatCircleGeom(g1, g2, unit)` | the same between the closest points (projected geometries are measured on WGS 84) |
| `angle(x1, y1, x2, y2)`, `angleDeg` | direction clockwise from the y axis in [0, 2π) radians / degrees (degrees rounded to 6 decimals, as Jena). Jena's implementation is a quarter turn off south-east and north-west of the first point; Sparkles follows the documented meaning |
| `azimuth(lat1, lon1, lat2, lon2)`, `azimuthDeg` | initial great-circle bearing clockwise from north, in [0, 2π) radians / degrees |
| `transform(g, datatype, crs)`, `transformDatatype(g, datatype)`, `transformSRS(g, crs)` | `g` in another datatype (`geo:wktLiteral`, `geo:geoJSONLiteral`) and/or CRS |

**UTM.** The 120 UTM zones on WGS 84 (`http://www.opengis.net/def/crs/EPSG/0/32601` to
`…/32660` north, `…/32701` to `…/32760` south; easting, northing in metres) are built-in
CRSs: transverse Mercator with Krüger's series to the sixth order (Karney 2011), well
under a millimetre within a zone. Points more than 60° of longitude from the zone's
central meridian have no coordinates (`transform` is a type error). Literals in a UTM CRS
are indexed; distances between them are Euclidean in metres.

**Conversion for maps.**

| Method | Path | Description |
|--------|------|-------------|
| POST | `/$/geo/convert` | Any caller (like `/$/format`). Body `{"literals": [{"value": "POINT(2 3)", "datatype": "http://www.opengis.net/ont/geosparql#wktLiteral"}, …]}`, at most 10,000. `200 {"results": [{"geometry": {…}} \| {"error": "…"}]}` in request order: each literal as an RFC 7946 geometry in CRS84 (longitude, latitude; EPSG:4326 swapped, projected CRSs transformed; an empty geometry is an empty `GeometryCollection`), or why it has none (`malformed literal at offset N: …`, `unknown CRS <…>: no transform to CRS84`, `not a geometry literal datatype: <…>`). `400` for a body of another shape or too many literals; `501` without the `geo` feature |

The route takes the place of `GET /$/geo/{ds}` for a dataset named `convert`: its index
status is not available over HTTP (`405`); its other `/$/geo/convert/…` routes and
`sparkles geo-index --loc DB --status` still work.

**Conformance.** Oxigraph's GeoSPARQL test suite runs with the W3C harness
(`cargo test -p sparkles --features geo --test w3c geosparql`): 37 of its 44 cases pass,
and the 7 others are listed with the reason in
`testsuite/geosparql/oxigraph/expected-failures.txt` (EPSG:4326 is supported, with its
latitude-first axes; unclosed polygon rings are malformed literals).

### Spatial joins and nearest neighbours

**Spatial joins.** A FILTER conjunct that tests two geometry variables bound by different
parts of a group (parts that share no variable) joins those parts on the test instead of
forming their cross product (`SpatialJoin` in EXPLAIN):

```sparql
SELECT ?state (COUNT(?p) AS ?n) WHERE {
  ?state a ex:State ; geo:hasDefaultGeometry/geo:asWKT ?sw .
  ?p a ex:Place ; geo:hasDefaultGeometry/geo:asWKT ?pw .
  FILTER(geof:sfContains(?sw, ?pw))
} GROUP BY ?state
```

The tests are the relations (but the disjoint ones), `relate` with a pattern that needs an
intersection, and `distance`/`metricDistance` below a constant (`<`, `<=`, or the bound
first with `>`, `>=`). A part that is a single pattern `?x <indexed predicate> ?w` is
searched in the spatial index when it is ready: per geometry of the other part when that
part is small next to it (`[index nested loop on <…>]`), otherwise its rows near the
other part are read once. Any other part is planned as usual, and its distinct geometries
are packed into an R-tree for the query (`[tree join]`, also without an index). Each
candidate pair is tested with the function itself, so the answer (duplicates included) is
the cross product's with the filter: geometries in an unknown CRS are tested against those
of the same CRS, and a relation with a literal the index does not hold reads the pattern
instead of searching the index. A disjointness test, a lower bound on a distance, a
pattern that holds without an intersection or a non-constant bound keep the cross product
and add a `geo-not-joined` warning naming the reason. Candidate pairs count against the
query's row limit: past it the query fails with `507`.

**Nearest neighbours.** `ORDER BY ASC(geof:metricDistance(?w, C))` or
`geof:distance(?w, C, unit)` with a length unit (or a variable bound to one of them), with
a `LIMIT`, over a group where `?w` is the object of one pattern of an indexed predicate
and the other patterns connect to that pattern, reads the pattern nearest first
(`SpatialKnn` under the top-k): the group runs over batches of the nearest rows until the
`k`-th distance found is below the bound of every row not read yet. Rows whose distance is
an error (not a geometry, malformed, empty, a CRS without a transform) come first in
SPARQL's order, so without a `FILTER(BOUND(?d))` or a bound on the distance in the group,
the pattern's rows are also read once to find them:

```sparql
SELECT ?g ?d WHERE {
  ?g geo:asWKT ?w
  BIND(geof:metricDistance(?w, "POINT(9 1)"^^geo:wktLiteral) AS ?d)
  FILTER(BOUND(?d))
} ORDER BY ?d LIMIT 10
```

The answer is the generic sort's, up to the choice among rows tied at the `k`-th
distance. A descending order, an angle unit, a constant that is empty or not in longitude
and latitude, an index that is not ready, and a group whose other patterns do not connect
to `?w`'s pattern keep the generic sort and add a `geo-not-knn` warning.

Both operators report the counters of the other spatial operators, and `pairs` (geometry
pairs that passed) and `indexProbes` for a join, `batches` (runs of the group) and
`errorRows` for nearest neighbours. They are the `spatial_join` and `spatial_knn`
optimizations (`QueryOptions::optimizations`, `SPARKLES_DISABLE_OPTIMIZATIONS`), on by
default.

## Reasoning status and diagnostics

Materialized inferences (`urn:x-sparkles:inferred`) are not maintained incrementally.
A materialization records the commit it wrote (or, when it changed nothing, the head it
read) and the dataset id. Any later commit makes the inferences **stale**, including
commits that only touch named graphs the reasoner does not read. Compaction and restarts
do not. A status written by an older version, or recorded for another dataset id, has
unknown freshness (`stale: null`).

```ts
type ReasoningStatus = {
  profile: string;             // "rdfs" | "rdfs-simple" | "owl-rl" | "rules"
  inferred: number;
  at: string;                  // when the run finished
  commit: number | null;       // commit the inferences were materialized at; null = unknown
  head: number;                // current head commit
  stale: boolean | null;       // null = unknown
  commitsSince: number | null; // head − commit; null when unknown or not comparable
  staleReason?: string;        // "3 commits since materialization", "store position moved backwards", …
  auto: { enabled: boolean; debounceSeconds?: number; scheduledAt?: string /* next planned run */ };
  warnings: string[];          // the last run's warnings
};
```

**Header.** A query or SHACL validation that includes the inferred graph while the
inferences are not fresh (at the snapshot it read) carries
`Sparkles-Inferences: stale; commits-since=3`, `stale` (count unknown) or `unknown`.
Fresh inferences send no header. The body is unchanged. The header is exposed to
cross-origin clients.

**Automatic re-runs** are off by default. `sparkles serve --auto-reason SECS
[--auto-reason-max-delay SECS]` re-runs the recorded profile once a dataset with stale
inferences has had no commit for `SECS` seconds, or at the latest after the maximum
delay (default 12 × `SECS`) while writes continue. Such tasks' messages start with
`auto:`. After a failed run, the next attempt waits for the next commit. Runs never
start for unknown freshness, nor on `--read-only` servers. Each run is a full
recomputation that holds the dataset's writer lock, so updates wait while it runs.

**Diagnostics.** `GET /$/reason/{ds}/diagnostics` runs a fixed set of checks from the
OWL 2 RL rules whose conclusion is `false` (OWL 2 Profiles §4.3), each one SPARQL query
over the default graph (plus the inferences when included). Those rules are sound, so
every finding is a genuine inconsistency. Finding nothing does **not** establish OWL
consistency.

| Param | Default | Meaning |
|---|---|---|
| `checks` | all | comma-separated check ids |
| `limit` | 100 (1–10000) | findings per check |
| `reasoning` | `true` if inferences exist | include `urn:x-sparkles:inferred` |
| `closure` | `subclass` | `subclass`: type tests follow `rdfs:subClassOf*`; `none`: stated types only |
| `timeout` | server query timeout | for the whole report |

| Check | Rules | Severity | Query |
|---|---|---|---|
| `nothing-member` | `cls-nothing2` (+`cax-sco`) | inconsistency | [nothing-member.rq](../crates/sparkles-reasoner/diagnostics/nothing-member.rq) |
| `disjoint-classes` | `cax-dw` | inconsistency | [disjoint-classes.rq](../crates/sparkles-reasoner/diagnostics/disjoint-classes.rq) |
| `all-disjoint-classes` | `cax-adc` | inconsistency | [all-disjoint-classes.rq](../crates/sparkles-reasoner/diagnostics/all-disjoint-classes.rq) |
| `same-different` | `eq-diff1` (+`eq-ref`, `eq-sym`, `eq-trans`) | inconsistency | [same-different.rq](../crates/sparkles-reasoner/diagnostics/same-different.rq) |
| `functional-literal-conflict` | `prp-fp`, `dt-diff`, `eq-diff1` | inconsistency | [functional-literal-conflict.rq](../crates/sparkles-reasoner/diagnostics/functional-literal-conflict.rq) |
| `thing-empty` | `thing-nonempty`: the domain is never empty | inconsistency | [thing-empty.rq](../crates/sparkles-reasoner/diagnostics/thing-empty.rq) |
| `unsatisfiable-class` | `lint`: a class below `owl:Nothing` without members | warning | [unsatisfiable-class.rq](../crates/sparkles-reasoner/diagnostics/unsatisfiable-class.rq) |

There is no unique name assumption: two IRIs count as different individuals only through
`owl:differentFrom`. Literal values of a functional property are compared with SPARQL
`!=`, restricted to numbers, strings, language-tagged strings and booleans, so a pair it
cannot compare is never reported. With inferences included, each finding is re-checked
with the same bindings over the asserted data alone: `basis` is `asserted` when that
holds and `uses-inferences` otherwise (with stale inferences, only `asserted` findings
are certain for the current data).

```ts
type DiagnosticsReport = {
  diagnosticsFormat: 1;
  dataset: string; commit: number /* snapshot checked */; computedAt: string;
  scope: { graph: "default";
           inferences: { included: boolean; profile?: string; stale?: boolean | null; commitsSince?: number | null };
           closure: "subclass" | "none" };
  status: "violations-found" | "none-found" | "incomplete";
  note: string;                         // the report never claims consistency
  checks: { id: string; rules: string[]; severity: "inconsistency" | "warning";
            status: "violations" | "none" | "truncated" | "timeout" | "error";
            findings: number; millis: number; error?: string }[];
  findings: { check: string; rule: string; severity: "inconsistency" | "warning";
              focus: Term; evidence: Record<string, Term | Term[]>;
              basis: "asserted" | "uses-inferences"; message: string }[];
};
```

`status` is `violations-found` when an inconsistency check has findings, else
`incomplete` when a check timed out or failed, else `none-found`; warnings never count.
A timeout marks the remaining checks `timeout` (no `408`). Errors: `400` for an unknown
check id or a bad `limit`/`closure`, `404` for an unknown dataset, `501` without the
`reasoning` feature. Diagnostics are read-only and also work on `--read-only` servers.

CLI: `sparkles infer --loc DB --status` prints the status; `sparkles infer --loc DB
--check [--checks a,b] [--limit N] [--no-inferences] [--closure subclass|none]
[--format text|json]` runs the checks (after materializing, when `--profile` or
`--rules` is given) and exits with 0 (`none-found`), 1 (`violations-found`) or 2
(`incomplete` or an error). `sparkles stats` shows a `reasoning` line.

## Write-time validation

A dataset can validate **every write** against SHACL shapes before it commits. The
configuration lives in the database directory (`validation.json`):

```json
{ "mode": "reject", "shapes": { "graphs": ["urn:x-shapes:main"] },
  "dataGraph": "default", "includeInferences": false,
  "threshold": "violation", "timeoutSeconds": 10, "reportLimit": 100 }
```

| Field | Values | Default | Meaning |
|---|---|---|---|
| `mode` | `reject`, `warn`, `off` | — | `reject`: a write that leaves results at or above the threshold is not committed (`422`); `warn`: it commits, and the receipt and header report the findings |
| `shapes` | `{ "graphs": [iri, …] }` or `{ "inline": "<turtle>", "format"?: media type }` | — | Named graphs of the dataset, read from the state being validated (so changes to them are validated, and must parse), or shapes given inline and copied to `validation-shapes.ttl` |
| `dataGraph` | `"default"`, `"union"`, `[iri, …]` | `"default"` | The data graph; the shapes graphs are never part of it, and the inferred graph only with `includeInferences` |
| `threshold` | `violation`, `warning`, `info` | `violation` | Results at or above it block |
| `timeoutSeconds`, `reportLimit` | number, 1–10000 | 10, 100 | Budget per write (exceeding it fails the write with `408`); results carried in a report |

| Method | Path | Description |
|---|---|---|
| GET | `/$/validation/{ds}` | `{ language, config, status }` (`language`: `shacl`; `status`: mode, shape count, the baseline of the last commit, counters, warnings) or `{ config: null }` |
| PUT | `/$/validation/{ds}` | Set the configuration. The current data is validated under the writer lock; `reject` on data that does not pass is refused with `409` and the report. `400` for a bad configuration or shapes that do not parse |
| DELETE | `/$/validation/{ds}` | Turn validation off (`204`) |

**Writes** (update, Graph Store PUT/POST/DELETE, upload, and through the CLI `load`,
`update`, `infer`, and bulk loads) are validated once per request, on the final state,
before any byte is written; a write that touches neither the data graph nor the shapes
graphs is skipped. Responses carry
`Sparkles-Validation: status=passed|warned|rejected|skipped|bypassed, mode=…, strategy=full, blocking=N, total=N, violations=N, warnings=N, infos=N, ms=N`,
and receipts (`receipt=true`) include a `validation` object (with `"language": "shacl"`). A rejection is
`422 Unprocessable Content`:

```json
{ "error": "SHACL validation failed: 2 blocking results (threshold violation); nothing was committed",
  "validation": { "status": "rejected", "blocking": 2, "total": 3, "limit": 100, "truncated": false,
                  "results": [ … ], "head": 41, "kind": "update" } }
```

or a Turtle `sh:ValidationReport` when the request's `Accept` names `text/turtle`. No
commit number is used. `?validationLimit=N` bounds the results of one request.
`?validate=false` (or `Sparkles-Validate: off`) skips validation only on a server started
with `--allow-unvalidated-writes` (`403` otherwise); the CLI has `--no-validate`. A dataset
whose `validation.json` cannot be loaded refuses writes (`501`) rather than accepting
them unvalidated.

CLI: `sparkles validation --loc DB --mode reject|warn (--shapes-graph IRI … | --shapes FILE) [--data-graph …] [--threshold …]`,
`--status`, `--off`. A write rejected in the CLI exits with status 3; `load`, `update` and
`infer` end their summary line with the validation status, and `sparkles stats` shows the
configuration (`validation      reject · 1 shape graph · 20 shapes`). A reasoning task
whose inferences are rejected fails with
`inferences rejected by SHACL validation: N blocking results (first: <shape> at <node>)`.
Rejections are logged at INFO under `sparkles::validation` (dataset, kind, counts, first
shape and focus node); see [Metrics](#metrics) for the counters. Cost: each validated
write runs a full validation of the data graph (about 160 ms at 1M triples); writes that do
not touch the data graph are free.

## SHACL validation

`POST /{ds}/shacl?graph=default|union|<iri>` validates a data graph of the dataset against
the shapes graph in the request body (Fuseki semantics):

* **Body**: the shapes graph. `Content-Type` selects the syntax: `application/n-triples`,
  `application/rdf+xml`, `application/ld+json`, `application/trig`, `application/n-quads`
  (all graphs of a quad format are merged); Turtle for `text/turtle` and for any other or
  absent content type (e.g. curl's default `application/x-www-form-urlencoded`).
* **`graph`**: `default` (the default; the dataset's default graph, i.e. the union of all
  graphs with `--union-default-graph`), `union` (all graphs, `urn:x-arq:UnionGraph`), or a
  graph IRI (`404` if the graph does not exist). The Jena special IRIs
  `urn:x-arq:DefaultGraph` / `urn:x-arq:UnionGraph` are accepted as well.
* **`reasoning=true|false`**: when the dataset has materialized inferences, validation runs
  over data ∪ `urn:x-sparkles:inferred` unless `reasoning=false`; with `false` the inferred
  graph is also left out of `graph=union`.
* **`timeout=<seconds>`**: as for queries (server default otherwise); `408` on timeout.
* Supports SHACL Core and SHACL-SPARQL. Parse errors in the shapes graph → `400`.
* **Budgets.** The report is bounded like a query result: `507` with `budget:
  "result-bytes"` once it holds more results than fit in `--max-result-mb` at 48 bytes each
  (or in `--query-memory-mb` at an estimated 512 bytes each), or once its serialized form is
  larger than `--max-result-mb`. Validations run in a pool of half the cores shared by all
  `/{ds}/shacl` requests, and stop when their client disconnects.

The response is the validation report (`200` whether or not the data conforms),
negotiated via `Accept` or `format=`:

| Accept / `format=` | Body |
|---|---|
| `text/turtle` / `ttl` (default) | `sh:ValidationReport` in Turtle |
| `application/n-triples` / `nt`, `application/ld+json` / `jsonld`, `application/rdf+xml` / `rdfxml` | the same report triples |
| `application/json` / `json` | compact JSON (below) |
| `format=text` | human-readable summary (one line per result) |

```ts
type ShaclReport = {
  conforms: boolean;
  results: {
    focusNode: Term;
    resultPath: Term | { type: "path"; value: string /* SPARQL property path */ } | null;
    value: Term | null;
    sourceShape: Term;
    sourceConstraintComponent: Term;   // e.g. { type: "uri", value: "http://www.w3.org/ns/shacl#MinCountConstraintComponent" }
    sourceConstraint?: Term;           // SHACL-SPARQL constraints
    severity: Term;                    // sh:Violation | sh:Warning | sh:Info
    messages: string[];                // sh:resultMessage texts
  }[];
};
```

The CLI equivalent is `sparkles shacl --loc DB --shapes shapes.ttl [--graph default|union|IRI]
[--format ttl|json|text|nt|jsonld|rdfxml] [--no-inferences]` (or `--data FILE…` to validate
files in memory); like Jena's `shacl validate` it exits with status 1 when the data does not
conform.

## ShEx validation

`POST /{ds}/shex` validates nodes of a data graph against a ShEx 2.1 schema (Shape
Expressions). Fuseki has no ShEx operation; the parameters follow `/{ds}/shacl` where they
overlap. Built with the `shex` cargo feature (on by default; `501` without it).

**Request.** Either the schema is the body and the shape map is in the query string, or a
JSON envelope carries both:

* **Schema as the body.** `Content-Type: text/shex` (ShExC), `application/shex+json`, or
  `application/json` / `application/ld+json` when the body is a ShExJ `Schema` object. Any
  other content type is sniffed: ShExJ when the body starts with `{`, ShExC otherwise. The
  shape map is `map=<compact shape map>`, or `node=<term>` with `shape=<label>`
  (`START` when `shape` is absent). `base=<iri>` resolves relative IRIs of the schema.
* **JSON envelope** (`Content-Type: application/json`, a body that is not a ShExJ schema):

  ```json
  { "schema": "PREFIX ex: <http://ex.org/> ex:S { ex:name . }",
    "schemaFormat": "shexc",
    "map": "{FOCUS a ex:Person}@ex:S",
    "externs": "ex:Ext { … }",
    "imports": { "http://ex.org/common": "<ShExC or ShExJ text>" },
    "base": "http://ex.org/schema" }
  ```

  `schemaFormat` is `shexc` or `shexj` (default: sniffed); `map` is a compact shape map
  (a string) or a JSON shape map (an array); `externs` defines the schema's `EXTERNAL`
  shapes; `imports` gives the bodies of `IMPORT`ed IRIs. Only `schema` is required, and
  the shape map comes from the envelope or the query string, not both. Unknown keys are
  an error.

**Shape maps.** The compact syntax of the ShapeMap draft, plus Jena's `BASE`/`PREFIX`
directives, commas between associations, a trailing `.` and `a` for `rdf:type`. Without
directives, prefixed names use the schema's prefixes. A node is an IRI, a prefixed name, a
literal or a blank node as Sparkles prints it in query results (`_:b1f`); `{FOCUS p o}`,
`{FOCUS p _}`, `{s p FOCUS}` and `{_ p FOCUS}` select the nodes of the data graph with
those arcs. The JSON syntax is an array of `{"node": …, "shape": …}` (the draft's
`nodeSelector` and `shapeLabel` are accepted too). A node that is not in the data graph
is validated with no arcs. `SPARQL """…"""` selectors are not supported yet (`400`).

**Imports** (`IMPORT <iri>`) resolve from the envelope's `imports`, then `file:` IRIs under
`--load-dir` (none without it), then http(s) IRIs through the `--outbound-*` policy of
SPARQL `LOAD`; an IRI that does not resolve as given is tried with `.shex`, then `.json`
appended. One validation reads at most 64 schemas and 16 MiB of imports, and its http(s)
imports share the `outbound-bytes` budget of one request.

**Semantic actions.** The Test extension (`http://shex.io/extensions/Test/`, `fail` and
`print`) runs; actions of other extensions are skipped, with a warning in the report.
`semact-trace=true` adds the Test extension's `print` output to each result's `appinfo`.

| param | values | default |
|---|---|---|
| `graph` | `default`, `union` or a graph IRI (`urn:x-arq:DefaultGraph`, `urn:x-arq:UnionGraph` too); `404` if the graph does not exist | `default` |
| `reasoning` | `true` / `false`: merge `urn:x-sparkles:inferred` into the data graph | `true` when the dataset has inferences |
| `results` | `all` / `nonconformant` (the counts still cover all) | `all` |
| `format` | `json`, `shapemap`, `smap` or `text` (otherwise `Accept`: `application/json`, `text/plain`) | `json` |
| `timeout` | seconds, as for queries; `408` past it | the server's |
| `semact-trace` | `true` / `false` | `false` |
| `stats` | `true`: add the typing's counters to the JSON report | `false` |
| `base` | the base IRI of the schema | none |

**Response.** `200` whether or not the nodes conform, with `Sparkles-Commit` (and the
inference headers of `/{ds}/shacl` when inferences were included):

```ts
type ShexReport = {
  conforms: boolean;                        // every association conformant
  counts: { conformant: number; nonconformant: number };
  results: {                                // in shape-map order
    node: Term;
    shape: Term | { type: "start" };
    status: "conformant" | "nonconformant";
    reason?: string;                        // the first failure, in one line
    appinfo?: { failures: ShexFailure[]; prints?: string[] };   // up to 8 failures
  }[];
  warnings: string[];
  millis: number;
  stats?: { pairs: number; evaluations: number; waves: number[] };   // stats=true
};
type ShexFailure =
  | { kind: "nodeKind" | "datatype" | "facet" | "valueSet"; value: Term; constraint: string }
  | { kind: "cardinality"; predicate: string; inverse: boolean; min: number; max: number | null; count: number }
  | { kind: "closed" | "extra"; predicate: string; value: Term }
  | { kind: "noMatch"; detail: string }
  | { kind: "reference"; shape: string; value: Term }
  | { kind: "not" | "external"; shape: string }
  | { kind: "semAct"; extension: string; message: string };
```

`format=shapemap` is the ShapeMap draft's JSON result map (`[{node, shape, status,
reason?, appinfo?}]`, compact-syntax strings); `format=smap` the compact result map, one
`<node>@<shape>` (conformant) or `<node>@!<shape>` (nonconformant) per line; `format=text`
Jena's report, `OK` or one `<n> @ <S> :: Focus = <n>, Status = nonconformant, Reason = …`
line per association.

**Errors.** `400` with `line` and `column` for a syntax error in the schema, the shape map,
the externs or an inline import (also ShEx 2.2 syntax); `400` for a schema that cannot be
used (an undefined reference, a negated reference cycle, an invalid `&include`, an import
that does not resolve or is not allowed, an `EXTERNAL` shape without a definition), a shape
label the schema does not define, `START` without a start shape, and invalid parameters;
`404` for a missing graph; `408` on timeout; `413` for a body over
`--max-query-body-mb`; `507` with `budget: "result-bytes"` for a report over
`--max-result-mb` (as for `/{ds}/shacl`), `"validation-work"` past the partition or pair
budget, or `"outbound-bytes"` when the imports exceed the request's outbound budget (see
[Budgets](#budgets)).

The CLI equivalent is `sparkles shex validate (--loc DB | --data FILE…) --schema FILE
(--map FILE | --shape-map 'MAP' | --node TERM [--shape LABEL]) [--graph default|union|IRI]
[--no-inferences] [--externs FILE] [--format text|json|shapemap|smap] [--only-nonconformant]
[--timeout S] [--semact-trace] [--stats]`, with Jena's flag names as aliases (`val`, `v`;
`--shapes`/`-s`, `--datafile`/`-d`, `--shapesMap`/`-m`, `--target`/`-n`). Imports resolve
against the schema file's directory. It prints Jena's text report by default and exits
with 0 when every association conforms, 1 when one does not (or on a timeout or budget
error) and 2 for usage, parse and schema errors. `sparkles shex parse FILE… [--out
shexc|shexj|text] [--base IRI]` prints schemas as ShExC, ShExJ or a structural dump.

## Formatting

`POST /$/format` formats a SPARQL query or update (Turtle, TriG, N-Triples, N-Quads and
JSON-LD later) and answers the formatted text, in the style of `sparkles fmt` (see the
README). It reads no dataset and no config file: the style options come with the request,
and omitted ones take their defaults. The formatter checks its own output before answering:
it must parse to the same SPARQL algebra as the input, keep every comment and format to
itself; when a check fails, the request fails and nothing is returned.

**JSON body** (`Content-Type: application/json`, what the UI sends):

```ts
type FormatRequest = {
  text: string;
  language?: "sparql" | "turtle" | "trig" | "ntriples" | "nquads" | "jsonld"; // default: detected
  cursorOffset?: number;   // in UTF-16 code units, like the editor's
  options?: FormatOptions;
};
type FormatOptions = {
  lineWidth?: number;            // 40..=400, default 100
  indentWidth?: number;          // 1..=8, default 2 (spaces)
  prefixGroups?: string[][];     // default []; e.g. [["rdf", "rdfs", "xsd", "owl"]], "" is the empty prefix
  typeShorthand?: boolean;       // default true: rdf:type → a
  compactIris?: boolean;         // default true: full IRI → prefixed name
  quoteStyle?: "double" | "preserve";           // default "double"
  operatorPosition?: "leading" | "trailing";    // default "leading": where a broken || or && chain puts its operator
  // accepted for the formats to come; no effect on SPARQL
  sort?: boolean; directiveStyle?: "sparql" | "turtle"; turtleLayout?: "diff" | "conventional";
  // accepted, not implemented yet (a warning says so)
  prunePrefixes?: boolean; alignValues?: boolean;
};
type FormatResult = {
  text: string;
  changed: boolean;                // text differs from the input
  language: string;
  cursorOffset: number | null;     // the cursor mapped into text (UTF-16 code units)
  warnings: { code: "undeclared-prefix" | "comment-moved" | "option-not-implemented";
              message: string; line: number; column: number }[];  // 0 when it has no position
};
```

```sh
curl -s localhost:3030/'$/format' -H 'Content-Type: application/json' \
  -d '{"text": "select * { ?s ?p ?o }", "cursorOffset": 9, "options": {"lineWidth": 80}}'
```

Without `language`, the text's first keyword after the prologue decides (`SELECT`,
`INSERT`, … is SPARQL). The cursor is kept next to the same token; a byte order mark is
dropped.

**Raw body** (curl): the body is the document, and its media type names the language unless
`language` is in the query string. The answer is `200` with the same media type, the
formatted text as the body, and `Sparkles-Format-Changed: true|false` (exposed to browsers
through CORS).

| `Content-Type` | Language |
|---|---|
| `application/sparql-query`, `application/sparql-update` | `sparql` |
| `text/turtle`, `application/trig`, `application/n-triples`, `application/n-quads`, `application/ld+json` | `turtle`, `trig`, `ntriples`, `nquads`, `jsonld` (`415` until they are implemented) |
| `text/plain` | needs `?language=` |

```sh
curl -s --data-binary @q.rq -H 'Content-Type: application/sparql-query' \
  'localhost:3030/$/format?lineWidth=80&operatorPosition=trailing'
```

**Query-string options.** Every option can also be a query parameter of either body form,
with the same camelCase name (`lineWidth=80`, `typeShorthand=false`); each `prefixGroup=rdf,rdfs,xsd,owl`
is one group, repeatable, in order (`""` is the empty prefix). Options in a JSON body win
over the query string's; both are checked.

**Errors** (JSON, with `requestId` as everywhere):

| Status | Body | When |
|---|---|---|
| `400` | `{error, detail?, line, column, code: "syntax", language}` | the input does not parse. `error` reads `SPARQL syntax error at line L, column C: …` with the head of the parser's message (1-based line, column in characters); `detail` holds the whole message when it was cut |
| `400` | `{error, code: "bad-request", option?}` | a bad option (`option` names it: an unknown name, a wrong type, a value out of range or not one of the choices, a prefix label in two groups), an unknown `language`, a body that is not a JSON object with `text`, a non-UTF-8 raw body, `text/plain` without `language`, a language that cannot be detected, or a `cursorOffset` past the end of the text |
| `401` | `{error}` | an anonymous caller under `--format-endpoint authenticated` |
| `404` | `{error}` | `--format-endpoint off` |
| `408` | `{error}` | the request took longer than `--format-timeout`, waiting for a slot included |
| `413` | `{error}` | a body larger than `--format-max-mb` |
| `415` | `{error}` | RDF/XML (`application/rdf+xml` or `language=rdfxml`: "RDF/XML formatting is not supported; convert to Turtle to format"), a language this build does not format yet ("turtle formatting is not available yet"), or another media type |
| `422` | `{error, code}` | the formatter refused its own output: `unsafe-format` (`algebra differs`, `comment lost`), `unstable-format` (`not idempotent`), or `unsupported-syntax` (the reference parser accepts a construct the formatter cannot handle yet). Logged at `warn` with the SHA-256 of the input (never the text); please report |

**Server settings and access.**

| `sparkles serve` flag | Default | |
|---|---|---|
| `--format-endpoint on\|authenticated\|off` | `on` | who may format: every caller the server admits (anonymous ones included when the server admits them), every caller but the anonymous principal (`401`), or nobody (`404`) |
| `--format-max-mb N` | `16` | the largest request body (`0`: unlimited) |
| `--format-timeout S` | `10` | seconds a request may take, waiting for a slot included; formatting runs on one slot per core |

With authentication, the route needs any caller (no dataset permission), like `/$/server`;
cookie sessions send the CSRF header as for every other `POST`. Rate limits count it in the
`query` class.

## `application/x-sparkles+json` (UI result format)

Rich result format inspired by QLever's `qlever-json`, used by the UI for results
rendering and query-plan visualization:

```ts
type SparklesResult = {
  queryType: "SELECT" | "ASK" | "CONSTRUCT" | "DESCRIBE";
  vars?: string[];                        // SELECT
  rows?: (Term | null)[][];               // SELECT; null = unbound
  boolean?: boolean;                      // ASK
  triples?: [Term, Term, Term][];         // CONSTRUCT / DESCRIBE
  meta: {
    totalRows: number; sentRows: number;
    timing: { parseMs: number; planMs: number; execMs: number; serializeMs: number; totalMs: number };
    plan: PlanNode;                        // executed operator tree
    memory: { peakBytes: number };         // peak estimated memory of intermediate results
  };
};
type Term =
  | { type: "uri"; value: string }
  | { type: "bnode"; value: string }
  | { type: "literal"; value: string; datatype?: string; "xml:lang"?: string }
  | { type: "triple"; value: { subject: Term; predicate: Term; object: Term } };
type PlanNode = {
  operator: string;          // "IndexScan", "Join", "Sort", "Filter", ...
  description: string;       // human readable, e.g. "PSO ?s <p> ?o"
  columns: string[];         // variables produced
  sortedOn: string[];
  estimatedRows: number; estimatedCost: number;
  actualRows: number; timeMs: number;       // wall time incl. children
  cached: boolean;
  children: PlanNode[];
  counters?: Record<string, number | string | boolean>;  // spatial operators: candidates, rechecked,
                             // refined, matched, treeNodesVisited, index ("ready", "building (37%)",
                             // "feature-links", …), fallback (see GeoSPARQL); expressions evaluated
                             // once per distinct value: exprCacheHits (rows that reused a result),
                             // exprCacheMisses (evaluations), exprCacheSkipped (ran row by row)
  warnings?: { code: string; message: string }[];       // root only: notes about the plan
};
```

## Explain

`GET|POST /{ds}/explain?query=…` → `{ "algebra": string /* SSE */, "plan": PlanNode }` (plan not executed; `actualRows`=-1).
The root `PlanNode` lists `warnings` when something in the query did not run the way it
reads, with the same answer: `geo-not-pushed` (a spatial FILTER evaluated row by row, and
why), `geo-index-building` (the spatial index is being built; plans without it run),
`geo-not-built` (`geof:` functions in a build without the `geo` feature).

## Compression

**Responses** are compressed when the client sends `Accept-Encoding` with `zstd`, `br`,
`gzip` or `deflate`, streamed bodies included. Bodies under 256 bytes and images are sent
as they are. The UI's larger assets are built with brotli and gzip copies, which are
served as they are (with `Vary: Accept-Encoding`) rather than compressed per request.

| `sparkles serve` flag | Default | |
|---|---|---|
| `--http-compression auto\|off` | `auto` | |
| `--http-compression-level fastest\|default\|best\|N` | `default` | zstd 3, brotli 4, gzip 6; a number applies to whichever algorithm is chosen |
| `--http-compression-algorithms` | `zstd,br,gzip,deflate` | the encodings offered |
| `--max-decompressed-mb` | `65536` | cap on a compressed request body or upload after decompression (0: none) |
| `--max-query-body-mb` | `16` | largest body of a SPARQL query, `/{ds}/explain`, `/{ds}/shacl` or `/{ds}/shex` request (0: none) |
| `--max-update-body-mb` | `256` | largest body of a SPARQL update (0: none) |
| `--max-admin-body-mb` | `16` | largest body of an admin request (`/$/…`) or `/{ds}/prefixes` change (0: none) |
| `--max-upload-mb` | `4096` | largest Graph Store write or upload body, after HTTP decompression (0: none) |
| `--min-free-disk-mb` | `1024` | free space a spooled request body must leave in the temporary directory, and a commit, rebuild, clone or N-Quads backup on the data directory's file system (0: no check) |
| `--max-mem-dataset-mb` | `4096` | largest in-memory (`dbType=mem`) dataset; a commit that would grow one past it fails with `507` (0: none) |

**Request bodies** (updates, queries, Graph Store PUT/POST, uploads) may be sent with
`Content-Encoding: gzip`, `br`, `zstd` or `deflate`. Another encoding gets `415` with an
`Accept-Encoding` header naming the supported ones. RDF bodies and uploaded files are also
recognised as compressed by their first bytes (gzip, zstd, LZ4 frames) and, for uploads,
by file name (`.gz`, `.zst`, `.br`, `.lz4`). A body that decompresses past
`--max-decompressed-mb` fails with `413` and commits nothing.

**Body ceilings.** A body that is read whole has the ceiling of its request class:
`--max-query-body-mb` for queries (also `/{ds}/explain` and the shapes graph of
`/{ds}/shacl` and the schema of `/{ds}/shex`), `--max-update-body-mb` for updates, `--max-admin-body-mb` for `/$/…`
requests and prefix changes, and a fixed 64 KiB for `/$/auth/*`. It counts decompressed
bytes and is checked while the body is read (a declared `Content-Length` over it is
refused before anything is read), so no more than the ceiling is held; past it the request
fails with `413`. A form POST to `/{ds}` may hold either operation, so it is read up to
the larger of the query and update ceilings. Graph Store PUT/POST (also through `/{ds}`)
and `/{ds}/upload` are the bulk endpoints: their bodies stream to a temporary file instead,
up to `--max-upload-mb` (default 4096, i.e. 4 GiB, the body limit of the bundled NixOS
nginx virtual host; counted after HTTP decompression; `0`: unlimited), else `413`. Files compressed inside the body are capped separately by
`--max-decompressed-mb` as they are parsed. Before a spooled body is written to the
temporary directory (every 64 MiB), the server checks that the file system keeps
`--min-free-disk-mb` free (default 1024; `0`: no check), else `507`.

**Storage.** A commit to a persistent dataset is refused with `507 {code: "storage-full"}`
when it would leave less than `--min-free-disk-mb` free on the data directory's file
system (measured with `statvfs`, cached for a second between small commits); a rebuild
(large load, compaction) or clone checks it while it builds and stops, removing what it
wrote, once the file system goes below it. Nothing is committed either way. An in-memory
dataset (`dbType=mem`) holds at most `--max-mem-dataset-mb` (default 4096, estimated from
its index files, delta and vocabulary): a commit that would grow it past that fails the
same way; deletes always pass. Storage quotas per dataset do not exist yet.

**Files.** `sparkles load` reads the same codecs (`--compression auto|none|gzip|zstd|brotli|lz4`;
`auto` goes by magic bytes, then the extension; brotli has no magic bytes, so it needs
`.br` or `--compression brotli`). When a file's name and its data disagree, the data
wins and a warning is logged; an explicit `--compression` that disagrees is an error.
`sparkles dump --out FILE` and `sparkles backup` take `--compress CODEC`, `--level N`
and `--threads N` (zstd). `sparkles backup` and `/$/backup` write zstd (level 3) by default,
about five times faster than gzip for a slightly larger file; `--compress gzip` (`?compression=gzip`)
gives `.nq.gz`, as Fuseki writes.
`sparkles dump --out FILE` goes by the file's extension (uncompressed without one).

**Full-text documents** are stored with zstd (level 3). `"docstoreCompression": "lz4"`
or `"none"` in the text configuration picks another; changing it rebuilds the index.
Indexes built before zstd was available keep LZ4 until they are rebuilt.

## Errors

Non-2xx responses carry `{ "error": string, "detail"?: string, "line"?: number, "column"?: number, "requestId": string }`
(`requestId` is the response's `X-Request-Id`, for finding the request in the logs)
with `400` for parse errors, `401`/`403` for authentication and permissions, `404` unknown
dataset, `405` an update sent with GET, `408` timeout, `409` conflict, `413` a body
over its ceiling or a compressed body over `--max-decompressed-mb`, `415` an unsupported content type or
`Content-Encoding`, `429` over a rate limit, `503` for a cancelled query, over a concurrency limit (see
[Rate limiting](#rate-limiting)), when a write-ahead log write failed (writes are refused
until restart; reads continue) or while an in-place restore replaces the dataset
(`{code: "dataset-restoring"}`, `Retry-After: 5`), `500` otherwise. The backup routes add
a machine-readable `code` (see [Backup repositories](#backup-errors)).

### Budgets

Queries run under per-request budgets (see `limits` in `/$/server`); exceeding one fails
the request with `507 Insufficient Storage` and

```json
{ "error": "query exceeds its memory budget: needs about 1.6 GiB, limit 1.0 GiB",
  "budget": "memory", "limit": 1073741824, "requested": 1717986918 }
```

* `memory` (`sparkles serve --query-memory-mb`, default 8192): the estimated bytes of the
  intermediate results a query (or the WHERE clause of an update) holds at once, 8 bytes
  per value. It is checked before large intermediate results are built, so an oversized
  query fails fast. It is an estimate, not a limit on the process's memory.
* `result-bytes` (`--max-result-mb`, default 1024): the serialized, uncompressed body of a
  SPARQL query response. Graph Store GET (a graph or whole-dataset export) has its own
  budget, `--max-export-mb`, unlimited (`0`) by default; past it the export fails the
  same way and reports the same `result-bytes` budget.
* `outbound-bytes` (`--outbound-request-max-mb`, default 4 × `--outbound-max-mb`, 1024):
  the bytes all the SERVICE calls and `LOAD <http…>` of one query or update receive (a
  compressed `LOAD` counts once decompressed). An update that exceeds it commits nothing;
  `SILENT` does not hide it. Their summed time has a total as well
  (`--outbound-request-timeout`, default 4 × `--outbound-timeout`, 240 s). The http(s)
  imports of one `/{ds}/shex` validation share the same budget.
* `validation-work`: the work of one ShEx validation, the partitions tried to match one
  node's neighbourhood to a shape (100,000) and the (node, shape) pairs of its typing
  (10,000,000, or `--query-memory-mb` at 64 bytes per pair if that is fewer). A
  validation past either fails; it never becomes a nonconformant result.

**Streaming.** Query and Graph Store GET bodies are serialized on a worker thread. A body
of up to 1 MiB is sent whole, with `Content-Length`, and an error (including this budget)
gets its status code. A larger body is streamed in 64 KiB chunks as it is serialized, so
server memory stays flat; an error after that point (e.g. the budget exceeded at 1.2 GiB)
aborts the transfer, and the client sees a truncated response instead of a status code.
A query result whose smallest encoding already exceeds the budget is refused with `507`
before anything is sent. A client that disconnects stops the serialization.

**Large request bodies.** Graph Store PUT/POST bodies over 16 MiB, and upload files, are
written to a temporary file as they arrive rather than held in memory. A large PUT
(estimated above the bulk threshold) replaces its graphs in one index rebuild that parses
the body as a stream; like every write it is atomic, so a parse error leaves the data as
it was.
* `rows` (`--max-rows`, default 200,000,000): the rows of any intermediate result.

`limit` and `requested` are in bytes (rows for `rows`). The response of `/{ds}/update`
includes `memPeakBytes`, and `meta.memory.peakBytes` in `application/x-sparkles+json` reports
the peak estimate of a query.

## Authentication and access control

`sparkles serve --auth-config FILE` turns authentication on. Without it there are no
credentials and every request may do everything, as the local principal. With it the
server **denies by default**: a caller may do only what a grant allows.

Without it the server listens on loopback only: `--host` defaults to `127.0.0.1`, and a
non-loopback address is refused at startup unless `--allow-open-network` (or
`SPARKLES_ALLOW_OPEN_NETWORK=1`) is given, which logs a warning. A Unix socket
(`--unix-socket`) counts as local. An authenticating reverse proxy is no substitute: the
backend it protects must not be reachable around it.

Since any web page the operator opens can send requests to a local server, a server
without auth also refuses:

- a `Host` (or HTTP/2 `:authority`) that is not an IP address, `localhost`,
  `*.localhost`, `--host` or a `--public-host` name: **421** (a page that rebinds its
  own DNS name to the server's address sends its own name);
- unsafe requests, and requests needing `write`, `admin` or `server-admin`, that are
  cross-origin by the rules of [CSRF and CORS](#csrf-and-cors) (`Origin` other than the
  request's own or a `--cors-origin`, or `Sec-Fetch-Site: cross-site`): **403**
  `cross-origin request refused`.

CORS headers are sent only for `--cors-origin` origins, without credentials. Requests
without `Origin` or `Sec-Fetch-Site` (the CLI, curl, other servers) and the server's own
UI pass.

### Principals and credentials

Each request resolves to one principal. The first applicable source wins:

1. **`Authorization`**: `Bearer spk_…` (an API token), or `Basic` with a configured user
   and password, or with a token as the password (any user name; for Basic-only clients
   such as Jena). Invalid credentials are **401**, never treated as anonymous.
2. **The session cookie** of the web UI (`__Host-sparkles_session` over https,
   `sparkles_session` on http://localhost). A bad, expired or revoked cookie is ignored and
   cleared.
3. **Trusted proxy headers** (`Remote-User`, `X-Forwarded-User`, …), only from a peer in
   `proxy.trusted` (a CIDR, or `unix` for `--unix-socket`). From any other peer they are
   ignored and counted (`sparkles_auth_untrusted_proxy_headers_total`).
4. **Anonymous**, with the grants of `[anonymous]` (none by default).

| Principal | Log name | From |
|---|---|---|
| user | `user:bob` | `[[users]]` (argon2id password) |
| token | `token:tok_…`, `token:cfg-NAME` | minted tokens, and static `[[tokens]]` |
| oidc | `oidc:alice@example.org` | a web UI login through the OIDC provider |
| proxy | `proxy:dave` | trusted headers of a forward-auth proxy |
| anonymous | `anonymous` | nothing else applied |

### Permissions

Per dataset, by name or `*` pattern (`"team-*"`): `read` < `write` < `admin`.

| Level | Allows |
|---|---|
| `read` | queries (including full-text and vector search), explain, Graph Store GET/HEAD, SHACL, `DatasetInfo`, stats, schema, prefixes, commits, reasoning status and diagnostics, text index status, `/$/ready/{ds}`, the dataset's tasks |
| `write` | `read` plus SPARQL Update, Graph Store PUT/POST/DELETE, upload |
| `admin` | `write` plus compact, backup (N-Quads dumps; backups to repositories: create, delete, verify, restore), reason/unreason, text index configuration, result-cache clear, clone (source), delete |

Server permissions: `metrics` (`/$/metrics`, the full `/$/ready` list), `federate`
(`SERVICE` and `LOAD <http…>`), and `server-admin` (everything: `admin` on every dataset,
create datasets, every token, `LOAD <file:…>`, which also needs `serve --load-dir` and
reads only files under it). Grants are a union of a principal's own
grants and its roles'; there are no deny rules. `--read-only` still applies to everyone,
after authorization. `federate` does not open every URL: `SERVICE` and `LOAD <http…>`
also follow the server's outbound policy (public addresses only unless
`--outbound-allow-private` or `--outbound-allow`; see the README, Outbound requests), and a
refused destination answers `403` as well. The local `sparkles query` and `sparkles update`
(no server, no permissions) allow loopback and private destinations by default and take
`--outbound-block-private` for the strict policy.

A **token** never exceeds its owner: at each use its permissions are its scope
intersected with its owner's current grants (or its parent token's, for a token minted
by a token). Removing a grant or a role mapping shrinks every token at its next request.

### Status codes

| Caller's level on `{ds}` | Caller | Dataset exists | Answer |
|---|---|---|---|
| none | anonymous | either | `401` with `WWW-Authenticate` |
| none | signed in | either | `404 {"error":"no such dataset: /ds"}` (hidden, like a missing one) |
| too low | anonymous | either | `401` |
| too low | signed in | no | `404` |
| too low | signed in | yes | `403 {"error":"write access to /ds required"}` |

Invalid credentials get `401 {"error":"invalid credentials"}` (or `token expired`) with
`WWW-Authenticate: Bearer realm="sparkles", error="invalid_token"` and, for browser
navigations and non-browser clients when users are configured, a `Basic` challenge.
Missing server permissions give `401` (anonymous) or `403`
(`{"error":"metrics permission required"}`). A clone needs `admin` on the source and on
the new name (`403 no admin access to the target name /x`). `SERVICE` or `LOAD` without
the permission is `403` before any connection or file is opened, even under `SILENT`.

### Route permissions

| Route | Method | Needs |
|---|---|---|
| `/ui/*`, `/$/ping`, `/$/ready` | GET | nothing (`/$/ready` lists only readable datasets without `metrics`) |
| `/$/whoami`, `/$/auth/config`, `/$/auth/login`, `/$/auth/oidc/*`, `/$/auth/device`, `/$/auth/token` | | nothing (invalid credentials are still `401`) |
| `/$/server`, `/$/datasets` (GET), `/$/tasks`, `/$/tasks/{id}`, `/$/auth/logout`, `/$/format` (POST) | | any caller (`/$/format`: none under `--format-endpoint off`, signed-in callers under `authenticated`); listings show readable datasets only (server-wide tasks: `server-admin`); cancelling a task (DELETE) needs `admin` on its dataset |
| `/$/metrics` | GET | `metrics` |
| `/$/datasets` | POST | `server-admin` |
| `/$/datasets/{ds}`, `/$/stats/{ds}`, `/$/schema/{ds}…`, `/$/prefixes/{ds}`, `/$/commits/{ds}…`, `/$/ready/{ds}`, `/$/reason/{ds}` (GET), `/$/reason/{ds}/diagnostics`, `/$/text/{ds}` (GET), `/$/geo/{ds}` (GET), `/$/vector/{ds}`, `/$/snapshots/{ds}…` (GET), `/$/history/{ds}` (GET), `/{ds}/prefixes` (GET) | GET | `read` |
| `/$/datasets/{ds}` (DELETE), `/$/datasets/{ds}/clone`, `/$/compact/{ds}`, `/$/backup/{ds}`, `/$/cache/clear/{ds}`, `/$/reason/{ds}` (POST, DELETE), `/$/text/{ds}` (PUT, DELETE), `/$/text/{ds}/rebuild`, `/$/geo/{ds}` (PUT, DELETE), `/$/geo/{ds}/rebuild`, `/$/snapshots/{ds}` (POST), `/$/snapshots/{ds}/{name}` (DELETE), `/$/history/{ds}` (PUT) | | `admin` |
| `/$/backups/{ds}`, `/$/backups/{ds}/{repo}/{backup}` | GET | `read` (a backup of another dataset is `404`) |
| `/$/backups/{ds}` (POST), `/$/backups/{ds}/{repo}/{backup}` (DELETE), `…/restore`, `…/verify` | | `admin` (a restore also on its target name) |
| `/$/repositories` | GET | any caller; the full list for `server-admin`, names and types for callers with `admin` on some dataset, else empty |
| `/$/repositories…` (other routes), `/$/backup-policies…` | | `server-admin` |
| `/{ds}/sparql`, `/{ds}/query`, `/{ds}/explain`, `/{ds}/get`, `/{ds}/shacl`, `/{ds}/shex`, `/{ds}/data` (GET, HEAD) | | `read` |
| `/{ds}/update`, `/{ds}/upload`, `/{ds}/data` (other methods), `/{ds}/prefixes` (other methods) | | `write` |
| `/{ds}` | any | by operation: `update=` or `application/sparql-update` → `write`; queries and GET → `read`; other writes → `write` |
| `/$/auth/tokens` (GET, POST), `/$/auth/tokens/{id}` (DELETE) | | a signed-in caller |
| `/$/auth/tokens?owner=…` | DELETE | `server-admin` |
| `/$/auth/device/{code}`, `…/approve`, `…/deny`, `/$/auth/cli/authorize` | | a web UI session or proxy identity |
| any other route | | `server-admin` (fail closed) |

`/$/metrics` needs the `metrics` permission: counters by dataset name would otherwise
reveal which datasets exist (so `metrics` shows the names of all datasets, up to
`--metrics-max-datasets`, whatever the holder's dataset grants). Prometheus scrapes it
with a static token
(`Authorization: Bearer spk_…`, e.g. `bearer_token_file` in the scrape config).

### CSRF and CORS

With auth, unsafe requests (and any request needing `write`, `admin` or `server-admin`)
are refused with `403 cross-origin request refused` when `Sec-Fetch-Site: cross-site` is
sent, or when `Origin` is neither the server's (`server.public_url`, else the request's
own) nor in `cors.origins` or `--cors-origin` (an allowed origin passes even though its
pages are cross-site). Session and proxy principals must also send
`X-Sparkles-CSRF: <whoami csrfToken>` on unsafe requests (`403 CSRF token missing or
invalid`). CORS then allows only `cors.origins` and `--cors-origin`, without
credentials; tools such as YASGUI send `Authorization: Bearer` themselves. `Host` is not
checked with auth: a page on another name gets no credentials of this server.

### whoami

`GET /$/whoami` (`Cache-Control: no-store`; `401` only for invalid credentials):

```ts
type Whoami = {
  authEnabled: boolean;
  principal: { kind: "local" | "anonymous" | "user" | "token" | "oidc" | "proxy";
               name?: string; displayName?: string; groups?: string[];
               owner?: string /* tokens: e.g. "oidc:alice@example.org" */ };
  method: "none" | "basic" | "bearer" | "session" | "proxy";
  expires?: string;       // the credential's expiry (tokens, sessions)
  csrfToken?: string;     // session and proxy principals
  tokenId?: string;       // token principals
  server: ("metrics" | "federate" | "server-admin")[];
  datasets: Record<string, "read" | "write" | "admin">;   // existing datasets only
  canMintTokens: boolean;
  logout: boolean;
  tokensPolicy?: { defaultTtlSeconds: number; maxTtlSeconds: number };
};
```

Without auth: `{"authEnabled": false, "principal": {"kind": "local"}, "server":
["server-admin"], "datasets": {…all "admin"}}`.

`GET /$/auth/config` (public) tells the UI and the CLI how to sign in:
`{"enabled": true, "methods": ["oidc", "token", "password", "proxy"], "oidc": {"loginUrl",
"displayName"}, "cli": {"authorizeUrl", "deviceAuthorizationEndpoint", "tokenEndpoint",
"deviceVerificationUri"}}`, or `{"enabled": false}`. Without auth the other `/$/auth/*`
routes answer `404`.

### Web UI sign-in and sessions

* `POST /$/auth/login` with `{"user", "password"}` or `{"token": "spk_…"}` → `204` and a
  session cookie (`HttpOnly`, `SameSite=Lax`, `Secure` over https, `Max-Age` = `session.ttl`,
  default 12 h; a token session ends with its token). Wrong credentials → `401`.
* `GET /$/auth/oidc/login?return_to=/ui/…` → `302` to the provider (authorization code
  with PKCE `S256`, `state` bound to the browser by a login cookie, `nonce`). The callback
  `GET /$/auth/oidc/callback` checks the state, redeems the code, verifies the ID token
  (JWKS signature with the configured algorithms, `iss`, `aud`, `azp`, `exp`, `iat`,
  `nonce`), reads the name and groups (UserInfo when the ID token lacks them), checks
  admission, and answers `303` to `return_to` with a session cookie. Failures go to
  `/ui/login?error=state|idp|idp_unavailable|not_allowed`.
* `POST /$/auth/logout` → `{"redirect": url | null}`: the provider's end-session URL for
  OIDC sessions, `proxy.logout_url` for proxy users.

Sessions are kept in `<data>/auth/sessions.json` (hashed ids, 0600) and survive restarts;
replacing `<data>/auth/session.key` signs everyone out. An owner (a user, an OIDC or
proxy identity; a token login counts for the token's owner) keeps at most 50 sessions: a
new one ends the owner's oldest. The server keeps at most 10,000; when full, the owner
that holds the most loses its oldest, so that no one can sign the others out by opening
sessions.

### API tokens

Tokens are `spk_` plus 43 base64url characters (256 random bits); the server stores only
their SHA-256 (`<data>/auth/tokens.json`, 0600).

* `POST /$/auth/tokens` `{"name", "datasets": {"wiki": "read"}, "server": [], "expiresIn":
  "30d"}` → `201` with `token` (shown only here), `id` (`tok_…`), `scope`, `created`,
  `expires`. Defaults: all the minter's access, `tokens_policy.default_ttl`; at most
  `tokens_policy.max_ttl`, and no later than a minting token. Static tokens cannot mint.
* `GET /$/auth/tokens` → `{"tokens": [{id, name, scope, created, expires, lastUsed, via,
  client, owner}]}`: the caller's own; `?all=true` (server-admin) adds everyone's and the
  static ones.
* `DELETE /$/auth/tokens/{id}` (`self`: the token in use) → `204`; the owner or
  server-admin, else `404`. Tokens minted by a revoked token die with it.
* `DELETE /$/auth/tokens?owner=oidc:alice@example.org` (server-admin) → `{"revoked": n}`.

### CLI logins

`sparkles auth login --server URL` gets a token without copying secrets around:

* **Browser** (default on desktops, `--web`): the CLI listens on `127.0.0.1:PORT`, opens
  `/ui/cli/authorize?port&state&code_challenge…`; after approval the browser is sent to
  the CLI with a one-time code (valid 120 s) that the CLI redeems at `POST /$/auth/token`
  (`grant_type=authorization_code`, `code`, `code_verifier`). The token never appears in
  a URL.
* **Device code** (over SSH, without a display, or `--device`; RFC 8628):
  `POST /$/auth/device` → `{device_code, user_code: "WDJB-MJHT", verification_uri,
  verification_uri_complete, expires_in: 600, interval: 5}`; the user approves at
  `/ui/cli/device`; the CLI polls `POST /$/auth/token`
  (`grant_type=urn:ietf:params:oauth:grant-type:device_code`) and gets
  `authorization_pending`, `slow_down`, `access_denied`, `expired_token`, or once
  `{access_token, token_type: "Bearer", expires_in, token_id, principal}`. At most 1000
  logins are pending; a session may fail 20 code lookups per 10 minutes (then `429`).

Approval needs a web UI session or proxy identity (`403 this action requires signing in
to the web UI` for Bearer or Basic callers).

### Configuration

TOML, unknown keys are errors (with line and column). Keep it `0600`; `sparkles auth
check --config FILE` validates it; `SIGHUP` reloads it (sessions and tokens follow the new
policy at their next request; a bad file keeps the old policy).

```toml
version = 1
realm = "sparkles"

[server]
public_url = "https://sparql.example.org"   # required with [oidc]

[anonymous]
datasets = { public = "read" }

[roles.wiki-editors]
datasets = { wiki = "write", "wiki-*" = "write" }
[roles.admins]
server = ["server-admin"]

[[users]]                                    # HTTP Basic and UI password sign-in
name = "bob"
password = "$argon2id$v=19$m=19456,t=2,p=1$…"  # sparkles auth hash
roles = ["wiki-editors"]
datasets = { "team-*" = "read" }

[[tokens]]                                   # static machine token
name = "prometheus"
hash = "sha256:…"                            # sparkles auth gen-token --name prometheus
server = ["metrics"]
# expires = "2027-06-30T00:00:00Z"

[tokens_policy]
default_ttl = "30d"
max_ttl = "90d"
max_active_per_owner = 100                   # unexpired minted tokens per owner
mint_rate = "60/h"                           # per owner: N/s|min|h|d[,burst=N] or "off"

[oidc]
issuer = "https://auth.example.org"
client_id = "sparkles"
client_secret_file = "/run/secrets/sparkles-oidc"   # omit for a public client (PKCE only)
scopes = ["openid", "profile", "email", "groups"]
name_claim = "email"                         # or preferred_username, sub
groups_claim = "groups"
display_name = "Example SSO"
algorithms = ["RS256", "ES256"]

[external]                                   # OIDC and proxy identities
allowed_groups = ["sparkles"]                # empty lists admit everyone
allowed_users = []
default_roles = []
[external.group_roles]
"kg-editors" = ["wiki-editors"]
"kg-admins" = ["admins"]
[external.user_roles]
"alice@example.org" = ["admins"]

[session]
ttl = "12h"
# key_file = "/var/lib/sparkles/auth/session.key"

[proxy]                                      # off unless present
preset = "authelia"                          # oauth2-proxy, authelia, tailscale, cloudflare-access
trusted = ["127.0.0.1/32", "unix"]
# user_header = "Remote-User"; email_header = "Remote-Email"; groups_header = "Remote-Groups"
groups_separator = ","
name_from = "user"                           # or "email"
logout_url = "https://auth.example.org/logout"

[cors]
origins = ["https://yasgui.example.org"]     # default: none
```

The OIDC redirect URI to register at the provider is
`{public_url}/$/auth/oidc/callback`.

**Forward-auth proxies.** The proxy must overwrite or strip client-supplied identity
headers on every route, including routes it lets through without authentication. Trust
the narrowest range: the Unix socket (`--unix-socket`, mode 0660, `trusted = ["unix"]`) is
the safest; `tailscale serve` connects from 127.0.0.1. Let `/$/auth/config`,
`/$/auth/device`, `/$/auth/token` and requests with `Authorization: Bearer spk_…` through
the proxy unauthenticated so the CLI can sign in, or give the CLI a separate route.
An `Authorization` header wins over proxy headers. Do not combine a proxy's own Basic
authentication with Sparkles auth: the proxy would forward its `Authorization` header.
When a local peer is trusted (a loopback address or `unix`), a request that carries
identity headers must name a known `Host`: an IP address, `localhost`, `--host`, a
`--public-host` name or the host of `server.public_url`; any other is refused with `421`,
so that a web page that rebinds its own DNS name to the server (or to the proxy in
front of it) cannot send its own `Remote-User`. Pass the name the proxy is reached by
with `--public-host` or `server.public_url` (the server warns at startup when it knows
none; the NixOS module passes its virtual host).

Credentials travel as bearer secrets: terminate TLS in front of the server (the server
warns when auth is on and it listens beyond loopback).

**Command line.** `sparkles auth hash` (password → argon2id), `sparkles auth gen-token
--name N` (static token; the `[[tokens]]` entry on stderr), `sparkles auth check --config
FILE`, `sparkles auth login|logout|status`, `sparkles auth token create|list|revoke`.
`query`, `update` and `load` accept `--server URL --dataset NAME` (or `SPARKLES_SERVER`)
and use the stored token (`$XDG_CONFIG_HOME/sparkles/credentials.toml`, 0600) or
`SPARKLES_TOKEN`.

**Rate limits.** Authentication failures are limited per client address before any
credential is checked (`preauth`, on by default; see [Rate limiting](#rate-limiting)).
The `--rate-limit` classes count a signed-in caller as its owner, across addresses and
credentials: bob's Basic requests, sessions and minted tokens share one budget (a token
minted by a token belongs to the same owner), while each static `[[tokens]]` entry is a
client of its own. Anonymous callers and the `auth` class (logins, CLI grants) are counted
per client address.

The auth layer has limits of its own, which answer `429` with `"limitClass": "auth"`:
tokens minted per owner (`tokens_policy.mint_rate`, default `60/h`; `reason` `mint`),
device logins started per client network (an IPv4 address or an IPv6 /48; 20, then two a
minute, whether `preauth` is on or not; `device`) and
unknown user codes per client network and per owner (20, then two a minute, whichever
runs out first; `device-code`; each is also a failure for `preauth`). An owner has at
most `tokens_policy.max_active_per_owner` unexpired tokens (default 100); minting another
answers `409` until one is revoked or expires. At most max(1, cores / 2) argon2 password
verifications run at once and four per permit wait (up to five seconds); a check beyond
that is refused at once with `503` and `Retry-After: 1`. One client network (an IPv4
address, an IPv6 /48, or the clients of an untrusted Unix socket together) has at most
max(2, cores / 2) of them running and waiting, and its further checks are refused the
same way, so that a single network cannot fill the queue for everyone.

**Metrics.** `sparkles_auth_failures_total{scheme,reason}`,
`sparkles_auth_denied_total{kind}` (`unauthenticated`, `forbidden`, `hidden`,
`cross_origin`, `csrf`, `not_interactive`), `sparkles_auth_logins_total{method,result}`,
`sparkles_auth_tokens_minted_total{via}`, `sparkles_auth_tokens_revoked_total`,
`sparkles_auth_tokens_active`, `sparkles_auth_sessions_active`,
`sparkles_auth_device_grants_pending`, `sparkles_auth_password_verifications_total`,
`sparkles_auth_password_verifications_running`, `sparkles_auth_password_verifications_waiting`,
`sparkles_auth_untrusted_proxy_headers_total`, `sparkles_auth_reloads_total{result}`, and
the policy sizes `sparkles_auth_policy_{users,tokens,roles}`. Audit events (logins,
logouts, minted and revoked tokens, device approvals, reloads) are logged at INFO under
`sparkles::audit`.

## MCP server

`sparkles mcp` speaks the [Model Context Protocol](https://modelcontextprotocol.io)
(JSON-RPC 2.0, one message per line) on stdin/stdout. It is not an HTTP endpoint; this
section documents it here because it exposes the same engine.

```
sparkles mcp (--loc [NAME=]PATH)... | (--data FILE... [--name NAME])
             [--allow-service] [--timeout SECS] [--query-memory-mb N] [--max-rows N]
             [--mcp-max-rows N] [--mcp-max-bytes N] [--max-concurrent N]
             [--disable-tool NAME]... [--schema-max-entries N] [--text]
```

| Flag | Default | Meaning |
|---|---|---|
| `--loc [NAME=]PATH` | | a database directory (repeatable); the name defaults to the directory's name |
| `--data FILE…`, `--name` | `data` | RDF files loaded into one in-memory dataset |
| `--text` | off | index the `--data` dataset for `search_text` (a `--loc` database keeps the index it has, see `sparkles text-index`) |
| `--timeout SECS` | `60` | largest `timeoutSeconds` a call may ask for (calls default to 30) |
| `--query-memory-mb N` | `2048` | memory budget of every call's queries (`0`: unlimited) |
| `--max-rows N` | `200000000` | rows of any intermediate result |
| `--mcp-max-rows N` / `--mcp-max-bytes N` | `1000` / `1048576` | largest `maxRows` / `maxBytes` of `sparql_query` |
| `--max-concurrent N` | `4` | tool calls running at once; further calls wait (and their timeout runs) |
| `--allow-service` | off | allow `SERVICE` in queries |
| `--outbound-allow-private`, `--outbound-block-private`, `--outbound-allow HOST_OR_CIDR`, `--outbound-timeout S`, `--outbound-max-mb N` | private blocked, none, `60`, `256` | where an allowed `SERVICE` may connect, as for `sparkles serve` |
| `--disable-tool NAME` | | do not offer a tool |

The process exits 0 when stdin closes and 1 on a startup error. Logs go to stderr.

**Protocol.** Revisions `2026-07-28` (stateless: `server/discover`, the protocol
version and client capabilities in each request's `_meta`) and the legacy `initialize`
handshake of `2025-11-25` and `2025-06-18`; an unknown revision gets `-32022` with
`data.supported`. Capabilities: `{"tools": {}}`. `server/discover` and `tools/list`
are cacheable for an hour (`ttlMs: 3600000`, `cacheScope: "public"`): the tool set is
fixed for the life of the process. `notifications/cancelled` stops the referenced call
(no response is sent for it). The server's `instructions` describe the workflow
(`list_datasets` → `describe_schema` → `sparql_query`) and that tool results are
untrusted data.

### Tools

Tools appear in this order. All are read-only
(`annotations: {"readOnlyHint": true, "openWorldHint": false}`; `sparql_query` is
open-world when SERVICE is allowed). Common arguments:

* `dataset`: a name from `list_datasets`; optional when the server has one dataset.
* `atCommit` (integer): read the snapshot of that commit (see below).
* `reasoning` (boolean): include materialized inferences (default: when the dataset
  has them).
* IRIs may be given as `<http://…>`, `http://…`, a prefixed name (`ex:alice`, with the
  dataset's prefixes) or, in `describe_resource`, a blank node label `_:b…`.

| Tool | Arguments (besides the common ones) | Result |
|---|---|---|
| `list_datasets` | none | `{datasets: [{name, quads, commit, modified, reasoning: null\|{profile, stale}, textSearch, writable}], limits: {defaultMaxRows, maxRows, defaultMaxBytes, maxBytes, defaultTimeoutSeconds, maxTimeoutSeconds, service, updates}}` |
| `describe_schema` | `section` (`summary`\|`classes`\|`predicates`), `graph` (`default`\|`union`\|IRI), `includeBuiltin`, `limit` (1–500; 25 for the summary, 100 for lists), `cursor` | `{dataset, commit, graph, reasoning, section, totals: {triples, classes, predicates}, builtinClassesHidden, ontology?, roots?, classes?: [{iri, label?, instances, declared, superClasses?}], predicates?: [{iri, label?, triples, distinctSubjects, distinctObjects, maxPerSubject, objects: ["iri 120", "xsd:string 98", "rdf:langString@en,de 12", …], domains?, ranges?, vector?}], next, prefixes}`. The summary lists the largest classes and predicates; `classes`/`predicates` page through all entries in IRI order |
| `sparql_query` | `query` (required), `format` (`table`\|`json`), `maxRows` (100), `maxBytes` (65536), `maxTermChars` (500), `offset`, `exactTotal` (true), `timeoutSeconds` (30) | one text block: a table or a JSON document (below); no `structuredContent` |
| `explain_query` | `query` (required), `includeAlgebra` | `{dataset, commit, queryType, estimatedRows, plan, algebra?, warnings: [{code, message}]}`; `plan` has one line per operator, `<operator> <description> est=<rows> [<columns>]`, indented by depth. Warnings: `unknown-term` (a constant IRI or literal of a triple pattern that the dataset does not contain), `no-limit` (no top-level LIMIT and over 10,000 rows estimated), `large-estimate` (an intermediate result over 50M rows), `service-disabled` |
| `describe_resource` | `iri` (required), `direction` (`both`\|`outgoing`\|`incoming`), `maxTriples` (50 per direction, ≤ 500), `lang` (`en`) | `{dataset, commit, iri, exists, label?, types, outgoing?, incoming?, prefixes}`; each side is `{total, predicates: [{p, count}], predicatesTotal, triples: [{p, o, oLabel?}` or `{s, sLabel?, p}], truncated}`. Triples are sampled round-robin by predicate, so a hub's largest predicate does not hide the others |
| `list_commits` | `limit` (10, ≤ 100), `before` | `{dataset, head, firstRetained, complete, commits: [{seq, timestamp, kind, inserted, deleted, quads}], next: {before} \| null}` |
| `search_text` | `query` (required, ≤ 1000 characters: terms, `"phrases"`, AND/OR, `+required`, `-excluded`), `predicates` (≤ 20 IRIs), `lang`, `limit` (20, ≤ 200), `withTypes` (true) | `{dataset, commit, hits: [{s, score, text, p, label?, types?}], limited, prefixes}`: BM25-ranked matches of `text:query`, `text` being the matched literal (escaped, ≤ 300 characters) and `types` at most 3. Only in builds with the `text` feature; a dataset without an index (`textSearch: false`) gives `text-disabled` |
| `similar_entities` | `predicate` (required), exactly one of `entity` (an IRI with one stored vector under `predicate`) and `vector` (1–16384 numbers), `k` (10, ≤ 100), `metric` (`cosine`\|`dot`\|`euclidean`), `excludeSelf` (true), `withLabels` (true) | `{dataset, commit, metric, higherIsBetter, hits: [{iri, score, label?}], prefixes}`: exact `spk:vectorSearch` over the stored `spk:vector` literals (it never computes embeddings). `no-vectors` when the predicate has none, the dimensions differ, or the entity has no vector |

Every tool except `sparql_query` declares an `outputSchema` and returns
`structuredContent` plus the same object as one compact JSON text block. `tools/list`
has the complete JSON Schemas.

**Terms** in results use Turtle/SPARQL syntax, so they can be pasted into queries:
`ex:alice` (a dataset prefix whose namespace fits), `<http://…>`, `_:b1f`, `"text"`,
`"text"@en`, `"x"^^xsd:date`, bare `42` / `1.5` / `true` for canonical integers, decimals
and booleans, and `<<( s p o )>>`. Inside quotes, `\`, `"`, line breaks, TAB, other
control characters and U+2028/U+2029 are escaped, so a term is always one line. A lexical
form or IRI longer than `maxTermChars` characters is cut, with the cut marked outside the
quotes: `"Lorem ipsum"…(+4519 chars)`. `prefixes` lists the prefixes a result used.
Labels come from `rdfs:label`, `skos:prefLabel`, `schema:name`, `foaf:name` and
`dcterms:title`, in that priority, preferring the requested language, then no language.

**`sparql_query` tables.**

```
# SELECT · rows 1–100 of 12345 (TRUNCATED: maxRows=100) · commit 42
PREFIX ex: <http://ex.org/>
?s	?name
ex:alice	"Alice"@en
…
# more: call sparql_query with the same query, offset=100, atCommit=42
```

The first line has the query type, the rows shown, the total (`of ≥N` with
`exactTotal: false`, which stops after `offset+maxRows+1` solutions), the truncation
reason, the commit and `· N terms shortened`. Cells are separated by TAB and an unbound
variable is an empty cell; CONSTRUCT and DESCRIBE rows are `s p o .`; ASK is `true` or
`false`. Rows stop at `maxRows`, or before the row that would take the whole text past
`maxBytes` bytes. Status lines start with `#`, which no rendered term can. `format: "json"`
gives `{dataset, commit, queryType, vars, rows: [[term | null]], boolean?, total | null,
offset, returned, truncated: null | {reason: "maxRows"|"maxBytes", next: {offset,
atCommit}}, termsShortened, prefixes, elapsedMs}`, also bounded by `maxBytes`.

**Snapshots.** A call without `atCommit` reads the head and names its commit. With
`atCommit`, the call reads that commit if it is the head or still held: the server
keeps the last 4 commits read per dataset (32 overall) for 10 minutes after their last
use. Otherwise the call fails with `unknown-commit` ("commit 38 is no longer held (head
is 42); rerun without atCommit …", or "commit 57 does not exist …"). All internal queries
of one call read one snapshot, and `describe_schema` cursors are bound to theirs.

### Errors

A failed call is a result with `isError: true`, one text block `"<message>\nHint:
<remedy>"` and `_meta["io.github.kclejeune.sparkles/error"] = {code, status, budget?}`
(`status` is the equivalent HTTP status):

| code | status | when |
|---|---|---|
| `bad-argument` | 400 | an argument outside its schema (unknown field, out of range, bad IRI) |
| `unknown-dataset` | 404 | no such dataset, or `dataset` omitted on a server with several (the hint lists them) |
| `syntax` | 400 | SPARQL syntax error (line and column; the hint lists the predeclared prefixes) |
| `not-a-query` | 400 | SPARQL Update sent to `sparql_query` |
| `timeout` | 408 | the call's timeout passed |
| `budget-memory`, `budget-rows` | 507 | a query budget was exceeded |
| `service-disabled` | 403 | a query uses SERVICE and it is not allowed |
| `unknown-commit` | 404 / 410 | `atCommit` in the future / no longer held |
| `stale-cursor` | 409 / 400 | a schema cursor whose snapshot is gone / a malformed cursor |
| `unknown-graph`, `too-many-entries` | 404, 413 | schema discovery errors |
| `text-disabled` | 400 | `search_text` on a dataset without a full-text index |
| `no-vectors` | 400 | `similar_entities`: no vectors under the predicate, a dimension mismatch, or an entity without a vector |
| `text-unavailable`, `write-failed`, `unsupported` | 503, 503, 501 | as over HTTP |
| `internal` | 500 | anything else ("internal error (request id …)", logged at ERROR) |

An unknown tool is a protocol error (`-32602`, "Unknown tool: NAME").
