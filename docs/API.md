# Sparkles HTTP API

The server speaks the Fuseki protocol, so existing Jena tooling (`rdfconnection`,
`s-query`, YASGUI and others) works unchanged. A small set of `/$/…` extensions serves the
web UI.

All admin endpoints live under `/$/`. Dataset names match `[A-Za-z0-9_.-]+` and are
addressed as `/{ds}`. JSON responses use `application/json`.

Without `sparkles serve --auth-config` the server is open, as described below. With it,
every route needs credentials or a grant to `anonymous`. See
[Authentication and access control](#authentication-and-access-control).

## Server

The design and its rationale are in [C01 Observability, readiness and budgets](specs/C01-observability-and-budgets.md).

| Method | Path          | Description |
|--------|---------------|-------------|
| GET    | `/$/ping`     | Liveness check. Returns a plain-text timestamp with `200` whenever the process serves HTTP. |
| GET    | `/$/ready`    | Readiness check. Returns `200` when the server is ready and `503` otherwise. The body is always `ReadyInfo`, sent with `Cache-Control: no-store`. |
| GET    | `/$/ready/{ds}` | Readiness of one dataset. `datasets` has one entry. `404` if the dataset is unknown. |
| GET    | `/$/server`   | `{ "version", "startedAt", "uptimeSeconds", "readOnly", "datasets": [DatasetInfo], "limits": Limits, "auth": { "enabled": boolean } }`. When auth is on, anonymous callers get no `version` or `limits`. |
| GET    | `/$/whoami`   | The caller and its permissions. See [whoami](#whoami). |
| POST   | `/$/format`   | Formats a SPARQL query or update. See [Formatting](#formatting). |
| GET    | `/$/metrics`  | Prometheus text format 0.0.4 (`text/plain; version=0.0.4`). See [Metrics](#metrics). `?format=json` returns the same counters as a JSON `MetricsSnapshot`, which the UI uses. `404` when the server runs with `--no-metrics`. `--metrics-addr` serves it on a second address too. |

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

Every response carries `X-Request-Id`, and CORS exposes it to browsers. The server keeps
an incoming `X-Request-Id` of 1–128 characters from `[A-Za-z0-9._:-]`. Otherwise it
generates one of the form `{boot:08x}-{seq:012x}`, which is unique per process and
increasing. The id is the `request_id` field of the request's log span, so every log line
of the request carries it.

`sparkles serve` logs one INFO line per completed request under the target
`sparkles::access`. `--no-access-log` turns it off. Requests to `/ui/*`, `/$/ping`,
`/$/ready` and `/$/metrics` are logged at DEBUG. Each line has these fields:

* `dataset`, or `$none`.
* `operation`: `query`, `update`, `gsp`, `upload`, `shacl`, `shex`, `explain`, `admin` or
  `other`.
* `status`.
* `outcome`: `ok`, `client_error`, `error`, `timeout`, `cancelled`, `budget`,
  `rate_limited`, `denied` or `rejected`. `rejected` is a write refused by write-time
  validation.
* With auth, `principal` and `auth`. `principal` is `user:bob`, `token:tok_…`, `oidc:…`,
  `proxy:…` or `anonymous`, never a credential. `auth` is `none`, `basic`, `bearer`,
  `session` or `proxy`. A failed login adds `auth_error`.
* Where known, `rows`, `parse_ms`, `plan_ms`, `exec_ms`, `serialize_ms`, `total_ms`,
  `response_bytes` and `mem_peak_bytes`.
* For writes to a validated dataset, `validation` (the status from the
  `Sparkles-Validation` header) and `validation_ms`.

A request whose client disconnects is logged with `status=499` and `outcome=cancelled`. The
499 is never sent. The span holds the matched route (`/{ds}/sparql`), never the raw URI.
Query and update text is logged only at DEBUG under `sparkles::query`
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
| `sparkles_validation_results_total` | counter. Results found by validated writes. ShEx counts nonconformant associations as `violation`. | `dataset`, `language`, `severity` = `violation` \| `warning` \| `info` |
| `sparkles_geo_rows` | gauge. Rows of the spatial index. | `dataset`, `part` = `base` \| `overlay` \| `tail` |
| `sparkles_geo_build_seconds` | gauge. Duration of the last build of the index's base. | `dataset` |
| `sparkles_geo_candidates_total`, `sparkles_geo_refined_total`, `sparkles_geo_matches_total`, `sparkles_geo_rechecked_total` | counter. Summed over the spatial operators of queries, in order: rows the index found, exact geometry tests, rows that passed them, and candidates the index could not place. | `dataset` |
| `process_resident_memory_bytes` | gauge (Linux) | |

Label values are bounded. `dataset` is an existing dataset name, or `$none` for requests
that name no existing dataset. At most `--metrics-max-datasets` datasets (default 100) get
their own label, and the others share `$other`. A (dataset, operation) pair appears after
its first request, and from then on with all nine outcomes. The `denied` outcome is a
refusal by the auth layer. Health checks (`/$/ping`, `/$/ready`), `/$/metrics` and UI
assets are not counted. The validation series cover every validated write to a dataset
with write-time validation, whether it comes over HTTP, over MCP or from a reasoning task.
Deleting a dataset removes its series. Each dataset has its own block cache and result
cache, each sized to the global `--cache-mb` / `--result-cache-mb`.

When rate limits are configured or authentication is on, the counter
`sparkles_rate_limited_total{dataset,class}` counts refused requests per limit class,
`preauth` included, and `outcome="rate_limited"` appears in `sparkles_requests_total`. Four
series report the size of each limiter's client state:
`sparkles_rate_limit_keys{limiter}`, `sparkles_rate_limit_max_keys{limiter}`,
`sparkles_rate_limit_evictions_total{limiter}` and `sparkles_rate_limit_penalties{limiter}`.
`limiter` is `requests`, or `auth` for the auth layer's own limits.
`sparkles_rate_limit_untrusted_forwarded_total{limiter}` counts requests whose
`X-Forwarded-For` or `Forwarded` header came from a peer that is not a trusted proxy. The
server ignores those headers.

Backup repositories add the `sparkles_backup_*` families listed under
[Backup repositories](#backup-metrics).

#### Fuseki metric names

`serve --metrics-fuseki-names` adds the gauges that Fuseki exports, so that dashboards
built for Fuseki keep working. The Sparkles families stay as they are.

| Name | Type | Labels | Value |
|------|------|--------|-------|
| `fuseki_requests` | gauge | `application="fuseki"`, `dataset`, `description`, `endpoint`, `operation` | Finished requests to the endpoint. |
| `fuseki_requests_good` | gauge | the same | Requests with the `ok` outcome. |
| `fuseki_requests_bad` | gauge | the same | Requests with any other outcome. |
| `process_uptime_seconds`, `process_start_time_seconds` | gauge | `application="fuseki"` | The time since the server started, and the start time. |
| `system_cpu_count` | gauge | `application="fuseki"` | Processors available to the server. |

`dataset` is the dataset path as Fuseki writes it, such as `/ds`. The datasets past
`--metrics-max-datasets` share `/$other`. Requests that name no existing dataset are not
counted.
`endpoint`, `operation` and `description` are Fuseki's names for the dataset's services.

| Request | `endpoint` | `operation` | `description` |
|---------|------------|-------------|---------------|
| `/{ds}/sparql`, `/{ds}/query` | `sparql`, `query` | `query` | `SPARQL Query` |
| `/{ds}/update` | `update` | `update` | `SPARQL Update` |
| `/{ds}/data` | `data` | `gsp-rw`, or `gsp-r` on a read-only server | `Graph Store Protocol`, or `Graph Store Protocol (Read)` |
| `/{ds}/get` | `get` | `gsp-r` | `Graph Store Protocol (Read)` |
| `/{ds}/upload` | `upload` | `upload` | `File Upload` |
| `/{ds}/shacl` | `shacl` | `SHACL` | `SHACL Validation` |
| `/{ds}` | empty | `query`, `update`, `gsp-rw` or `gsp-r`, by the request | As above. |

The good and bad counts split `sparkles_requests_total` for the same dataset and route.
`good` is `outcome="ok"`, and `bad` is the sum of the other outcomes. Fuseki counts a
request when it starts and its outcome when it ends, while Sparkles counts both when the
request ends, so `fuseki_requests` always equals good plus bad. An endpoint's series
appear after its first request, like the Sparkles series. Fuseki lists every configured
endpoint from the start with zeros.

Fuseki's other meters come from Micrometer's JVM and system binders, and Sparkles has no
equivalent for them. They are the `jvm_*` memory, garbage collector, thread and class
loader gauges, `process_files_*`, `process_cpu_usage`, `system_cpu_usage`,
`system_load_average_1m`, `disk_free_bytes` and `disk_total_bytes`. `/{ds}/shex`,
`/{ds}/explain`, `/{ds}/prefixes` and the `/$/` routes have no Fuseki endpoint, so only
the Sparkles names count them.

#### Metrics listener

`serve --metrics-addr HOST:PORT` also serves `/$/metrics` on a second address, such as a
port that only the Prometheus server can reach. That listener serves nothing else. It
applies the same authentication, `metrics` permission and `Host` check as the main
listener. The main listener keeps `/$/metrics`, because the UI's Server page reads it.
Without `--auth-config`, an address that is not loopback needs `--allow-open-network`.

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

`sparkles serve` can export traces, metrics and logs over OTLP. Export is off by default.
Nothing is exported and no connection is opened unless `--otel` is given or the
environment asks for it. The environment asks for it by setting
`OTEL_EXPORTER_OTLP_ENDPOINT`, a signal-specific endpoint, or `OTEL_TRACES_EXPORTER` /
`OTEL_METRICS_EXPORTER` / `OTEL_LOGS_EXPORTER=otlp`. `OTEL_SDK_DISABLED=true` turns export
off again. The `otel` cargo feature is on by default, and builds without it have none of
this.

| Variable | Meaning |
|----------|---------|
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_ENDPOINT` | Collector address. Defaults to `http://localhost:4318`, or `:4317` for gRPC. |
| `OTEL_EXPORTER_OTLP_PROTOCOL`, `…_{TRACES,METRICS,LOGS}_PROTOCOL` | `http/protobuf` (default) or `grpc`. gRPC is plain text only, so use `http/protobuf` for an `https://` collector. |
| `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_TIMEOUT` (and per signal) | As specified by OpenTelemetry. No compression is built in. |
| `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES` | Resource attributes. `service.name` defaults to `sparkles`. |
| `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG` | Default `parentbased_always_on`. |
| `OTEL_TRACES_EXPORTER`, `OTEL_METRICS_EXPORTER` | `otlp` or `none`. `otlp` is the default once export is enabled. |
| `OTEL_LOGS_EXPORTER` | `otlp` or `none`. The default is `none`, because logs are opt-in. |
| `OTEL_METRIC_EXPORT_INTERVAL` | Milliseconds between metric exports (default 60000). |
| `OTEL_BSP_*` | Batch span processor settings. |

`serve` has four OpenTelemetry flags:

* `--otel` enables export.
* `--otel-logs` exports log events as well.
* `--otel-query-text` records query and update text in `db.query.text`, cut to 2048
  characters, along with plan operator descriptions. The text may contain data.
* `--otel-plan-spans` adds one span per executed plan operator.

Spans and log records are sent in batches. On SIGTERM or SIGINT the server finishes its
requests, then flushes the exporters for at most 5 seconds. The resource carries
`service.name`, `service.version`, `service.instance.id` (a UUID per process), `host.name`
and `process.pid`.

**Traces.** Each request is a server span named after its route (`GET /{ds}/sparql`). It
continues the trace of an incoming W3C `traceparent` / `tracestate`. The span has these
attributes: `http.request.method`, `http.route`, `http.response.status_code`,
`url.scheme`, `url.path` (never the query string), `server.address` / `server.port` (from
`Host`), `client.address` (the peer), `user_agent.original`, `sparkles.request_id`,
`db.system.name` = `sparkles`, `db.namespace` (the dataset), `db.operation.name` (the
access log's operation: `query`, `update`, `gsp`, …), `sparkles.sparql.kind` (`SELECT`,
`ASK`, `CONSTRUCT`, `DESCRIBE`), `sparkles.outcome`, `db.response.returned_rows`,
`http.response.body.size`, `sparkles.memory.peak_bytes`, and `sparkles.commit.seq` for
writes. A 5xx response sets the span status to error and adds `error.type`. The request
span has these children:

* `sparql.parse`, `sparql.plan`, `sparql.execute` (with `db.response.returned_rows`) and
  `sparql.serialize` for queries, and `sparql.parse` and `sparql.execute` for updates.
  These spans are built after the request from the recorded timings. The executor itself
  is not instrumented.
* With `--otel-plan-spans`, the executed operator tree under `sparql.execute`. Each
  operator gets one span, up to 256, with `sparkles.operator`, `sparkles.rows`,
  `sparkles.rows.estimated`, `sparkles.cost.estimated` and `sparkles.cached`. Durations
  are the recorded ones. Children are laid out one after another from their parent's
  start, so their offsets are approximate.
* `commit` for every commit, with `seq`, `kind`, `inserted` and `deleted`.
* `sparql.service`, a client span, for each SERVICE call.
* `shacl.validate`, `shex.compile` and `shex.validate`.

A refusal by a rate limit adds a `rate_limited` event and `sparkles.rate_limit.class` to
the span.

Background tasks (compaction, backups, clones, reasoning, full-text rebuilds) are root
spans named `task {kind}`, linked to the request that started them. SERVICE and
`LOAD <url>` requests send `traceparent` and `tracestate`, so a federated endpoint
continues the trace. The response to a sampled request carries
`traceresponse: 00-{trace-id}-{span-id}-01` (W3C Trace Context Level 2), which CORS
exposes. The request's log lines carry `trace_id` and `span_id` in the request span.

**Metrics.** `http.server.request.duration` is a histogram in seconds, with the buckets of
`sparkles_request_duration_seconds`. Its attributes are `http.request.method`,
`http.route`, `http.response.status_code`, `url.scheme`, `db.namespace` (the capped
`dataset` label), `db.operation.name` and, for 5xx, `error.type`. The Prometheus registry
is exported as observable instruments read at collection time, so nothing is counted
twice and `/$/metrics` does not change. These instruments are
`sparkles.requests` (`dataset`, `operation`, `outcome`),
`sparkles.response.size`, `sparkles.requests.active`, `sparkles.result.rows`,
`sparkles.budget.exceeded`, `sparkles.rate_limited`, `sparkles.dataset.quads`,
`sparkles.delta.quads`, `sparkles.wal.size`, `sparkles.disk.size`,
`sparkles.block_cache.{size,capacity,hits,misses}`,
`sparkles.result_cache.{size,capacity,entries,hits,misses}`, `sparkles.geo.rows`
(`dataset`, `part`), `sparkles.geo.build.duration`,
`sparkles.geo.{candidates,refined,matches,rechecked}`, `sparkles.ready`,
`process.uptime` and `process.memory.usage`.

**Logs.** With `--otel-logs` or `OTEL_LOGS_EXPORTER=otlp`, every log event that passes
`RUST_LOG` is also exported as an OTLP log record, with the trace and span id of its
request. This includes the access log.

### Rate limiting

Rate limiting is off by default. The one exception is failed authentications when
authentication is on (see [Before authentication](#before-authentication-preauth)).
`sparkles serve --rate-limit SPEC` (repeatable), `--rate-limit-config FILE`, or both, limit
each request class per client:

| Class | Requests |
|-------|----------|
| `auth` | Every path under `/$/auth/`, matched or not: login, token minting, device flow and the OIDC callback. |
| `query` | `/{ds}/sparql`, `/{ds}/query`, `/{ds}/explain`, `/{ds}/shacl`, `/{ds}/shex`, Graph Store `GET`/`HEAD`, `/{ds}` with `query=` or a GET, `/$/schema/*`, `/$/stats/*`, `/$/reason/{ds}/diagnostics`, `/$/format` |
| `update` | `/{ds}/update`, `/{ds}/upload`, Graph Store `PUT`/`POST`/`DELETE`, and `/{ds}` with `update=` or any other write. A form POST to `/{ds}` counts as an update. |
| `admin` | `/$/…` requests other than `GET`/`HEAD` and `POST /$/format`. These cover dataset management, compaction, backups, reasoning, caches and full-text. |
| `preauth` | Every request, before authentication. Counts failed credential checks per client address and per IPv6 /48. Has no per-dataset form. |

`/$/ping`, `/$/ready*`, `/$/metrics`, the UI and the other `/$/` reads are never limited.

`SPEC` is `CLASS[@DATASET]=LIMIT`. `LIMIT` is `off` or a comma-separated list of these
settings:

* `N/s`, `N/min`, `N/h` or `N/d`: the sustained rate per client.
* `burst=N`: how many requests a client may make at once after being idle. Defaults to the
  rate's `N`.
* `concurrency=N`: requests of the class in flight server-wide.
* `client-concurrency=N`: requests in flight per client.
* `failure-cost=N`: what a `401` or `403` response costs, in requests (default 1). A higher
  cost makes failed logins exhaust the budget faster. For `preauth` it is the cost of a
  failed credential check.

`CLASS@DATASET=…` replaces the class limit for requests to that dataset, with its own
counters. `CLASS@DATASET=off` exempts the dataset. Examples:

```sh
sparkles serve --rate-limit auth=10/min,burst=5,failure-cost=3 \
               --rate-limit query=100/s,burst=200,client-concurrency=8,concurrency=64 \
               --rate-limit update=10/s --rate-limit query@public=5/s
```

`auth=10/min,burst=5` with `failure-cost=3` is a reasonable strict default for
authentication endpoints. The configuration file is JSON. The flags apply on top of it,
and `SIGHUP` re-reads it. A reload keeps the client state of every limit whose name
(`query`, `query@public`, …) is still configured. Debts are not forgiven, and requests
already in flight count against the new concurrency caps. A lower cap therefore admits
nothing new until they drop below it. A bad file leaves the running configuration in
place.

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

**Clients.** A client is its peer address. An IPv6 client is identified by its /64.
Behind a reverse proxy, list the proxy under `trustedProxies` or pass
`--rate-limit-trusted-proxy CIDR`. The value `unix` trusts the `--unix-socket`. For a
request from a trusted peer, the client is the rightmost untrusted hop of
`X-Forwarded-For`, the header that nginx, HAProxy, Caddy, Traefik and cloud load balancers
set. Only that header is read, so a client's own `Forwarded` header changes nothing. For a
proxy that sets `Forwarded` (RFC 7239) instead, set `"trustedProxyHeader": "forwarded"` or
pass `--rate-limit-trusted-proxy-header forwarded`. `X-Forwarded-For` is then the ignored
header.

A hop that is not an address, such as `unknown` or an obfuscated `_id`, is a client of its
own, named by that text. When every hop is trusted, the client is the leftmost hop. On the
Unix socket without a trusted `unix`, clients have no address and share one key.
Forwarding headers from untrusted peers are ignored and counted in
`sparkles_rate_limit_untrusted_forwarded_total`, and the first one is logged as a warning.
With authentication, a signed-in caller is counted by its owner instead of its address
(see [Authentication](#authentication-and-access-control)). The `preauth` and `auth`
classes still count by address.

Limits by address are only as good as the address. They need a peer address that clients
cannot choose. List only proxies that overwrite the header they receive (in nginx,
`proxy_set_header X-Forwarded-For $remote_addr;`) or append to it. Never list a network
that clients can send from. Behind a proxy that is not listed, every client has the
proxy's address and shares one budget. `serve` therefore warns at startup when
authentication is on, the listener is the Unix socket or a loopback address, and no proxy
is trusted.

The limiter tracks at most `maxKeys` clients (default 100,000, about 100 bytes each). A
flood of new addresses evicts other rarely seen clients, but never one with requests in
flight. An evicted client that still owed time is remembered in a penalty cache one
eighth that size, so churning the cache does not forgive its debt. `maxKeys` is a security
setting. If it is far below the number of active clients, a flood of addresses can evict
clients, and a flood larger than the penalty cache can make the server forget them. Watch
`sparkles_rate_limit_evictions_total`. Clients whose bucket has refilled are dropped every
minute.

**Algorithm.** The limiter uses GCRA, the virtual-scheduling form of a token bucket. A
client may send `burst` requests at once, then one every `period / N`.

**Responses.** A request over the rate gets `429 Too Many Requests` with `Retry-After` in
whole seconds. A request over a concurrency cap gets `503 Service Unavailable` with
`Retry-After: 1` at once. Requests are never queued, so a saturated server sheds load
instead of holding waiting requests. A request holds its concurrency slot until its
response body has been sent and the work it started has ended. Streamed Graph Store GETs
count too. A client that disconnects cancels its query or write, and the slot is free once
that work has stopped. The body uses the error format:

```json
{ "error": "too many query requests: retry in 2 s",
  "limitClass": "query", "reason": "rate", "retryAfterSeconds": 2 }
```

`reason` is `rate`, `concurrency` or `client-concurrency`. It is `failures` for `preauth`,
and `mint`, `device` or `device-code` for the auth layer's own limits. Responses of a class
with a rate carry the headers of draft-ietf-httpapi-ratelimit-headers-11:

* `RateLimit-Policy: "query";q=100;w=1` gives the configured rate, `q` requests per `w`
  seconds. The policy name is `CLASS` or `CLASS@DATASET`.
* `RateLimit: "query";r=57;t=1` gives `r`, the requests available now, and `t`, the
  seconds until the bucket is full.

CORS exposes `Retry-After`, `RateLimit` and `RateLimit-Policy`.

**Observability.** Refused requests are logged with `outcome=rate_limited` and counted
in `sparkles_requests_total{outcome="rate_limited"}` and
`sparkles_rate_limited_total{dataset,class}`.

#### Before authentication (`preauth`)

A first stage runs before any credential is checked. It bounds password guessing, and the
hashing that guessing costs, per client address. Every address has a budget of failed
credential checks. These count as failures:

* a wrong password, over HTTP Basic or a UI login
* an unknown, expired or malformed token
* an invalid session cookie
* an unknown device user code or loopback code
* a failed OIDC callback
* a cross-origin or CSRF refusal on an `/$/auth/` route

Nothing else is charged. An authorization `403` does not count, whether it comes from a
read-only server, a `SERVICE` or `LOAD` refused by policy, or a missing permission.
Neither does a hidden dataset's `404`, the `401` of an anonymous request, or a busy
password check. Each failure costs `failure-cost` (default 1). An address without failures
leaves no state behind. An IPv6 client also spends the budget of its /48, which is eight
times as large and appears as `"preauth/48"` in the headers. A network that holds many
/64s therefore does not get one budget per /64.

An address or /48 that has spent its budget is refused with `429` (`"limitClass":
"preauth"`, `"reason": "failures"`) until the budget refills. The refusal applies only to
requests that would hash a password or that turn out to present an unknown token. The
requests that hash a password are HTTP Basic and UI password logins whose credentials are
not in the verified-credential cache. Valid tokens, sessions and proxy identities,
anonymous requests, `/$/ping`, `/$/ready` and the UI keep working. One client guessing
from a shared address therefore does not take down the other clients or a load
balancer's health checks. A password check takes the cost of a failure before it starts
and gives it back when the password is right, so concurrent guesses from one address
cannot all start hashing. Responses to failures carry the stage's `RateLimit-Policy` and
`RateLimit` headers.

Requests without a client address share one budget, so guessing stays bounded for them
as well. These are requests over `--unix-socket` with no trusted `unix` proxy. Trust the
proxy on the socket so that its clients are told apart.

With `--auth-config`, this stage is on by default at `30/min,burst=60`: 60 failures at
once, then one every two seconds. Change it with
`--rate-limit preauth=RATE[,burst=N][,failure-cost=N]` or `classes.preauth`. It has no
per-dataset form and no concurrency caps. `preauth=off` turns it off.

## Datasets (admin)

| Method | Path                         | Description |
|--------|------------------------------|-------------|
| GET    | `/$/datasets`                | `{ "datasets": [DatasetInfo] }` |
| POST   | `/$/datasets`                | Creates a dataset. The form or JSON body has `dbName`, `dbType` = `persistent` \| `mem`, and optionally `geo`. `geo` = `true` adds a spatial index with the defaults. In a JSON body `geo` can also be a `GeoConfig` (see [GeoSPARQL](#geosparql)). An invalid one is a `400`, and a build without the `geo` feature returns `501`. `201` on success, `409` if the dataset exists. |
| GET    | `/$/datasets/{ds}`           | `DatasetInfo` |
| DELETE | `/$/datasets/{ds}`           | Removes the dataset and its files. |
| POST   | `/$/datasets/{ds}/clone`     | Copies the dataset into a new persistent dataset. Returns `202` with a `Task`. See [Clone](#clone). |
| GET    | `/$/stats/{ds}`              | `DatasetStats` |
| GET    | `/$/schema/{ds}`             | *Extension.* `SchemaSummary`: classes and predicates with exact counts and their declarations. An RDF `Accept` gets the same report as a VoID description. See [Schema discovery](#schema-discovery). |
| GET    | `/$/schema/{ds}/classes`     | *Extension.* `Page<ClassEntry>` |
| GET    | `/$/schema/{ds}/predicates`  | *Extension.* `Page<PredicateEntry>` |
| POST   | `/$/compact/{ds}`            | Merges the delta (updates) into a freshly built, sorted base index. Returns a `Task`. `409` while a compaction of the dataset is queued or running. |
| POST   | `/$/backup/{ds}`             | Writes an N-Quads dump to `<data>/backups/{ds}_{time}.nq.zst` with zstd level 3. A build without zstd writes gzip (`.nq.gz`). `?compression=gzip\|zstd\|brotli\|lz4\|none` and `?level=N` pick another codec. The extension follows the codec, so `compression=gzip` gives Fuseki's `.nq.gz`. Levels are 0–9 for gzip, 1–19 for zstd and 0–11 for brotli. lz4 and none take no level. Any other level is a `400`. Returns a cancellable `Task` whose message gives the size and time. `409` while a backup of the dataset is queued or running. `507` when the data directory's file system has less than `--min-free-disk-mb` free, and the task fails once writing would go below it. zstd uses at most 4 threads (a quarter of the cores). Incremental, deduplicated backups to a file system or S3 are described under [Backup repositories](#backup-repositories). |
| POST   | `/$/reason/{ds}`             | Materializes inferences. The JSON body is `{ "profile": "rdfs" \| "owl-rl" \| "rules", "rules"?: string, "vocabularies"?: ["geosparql"], "geoDefaultGeometry"?: boolean }`. A form takes `vocabulary` (repeated) and `geoDefaultGeometry`. See [Query rewrite and RDFS entailment](#query-rewrite-spatialequals-and-rdfs-entailment). `{ "rerun": true }` or `?rerun=true` re-runs the recorded profile, rules and extras, and returns `409` when nothing is recorded. `400` for an unknown profile or vocabulary. Returns a `Task`. |
| GET    | `/$/reason/{ds}`             | `ReasoningStatus`, or `{ "reasoning": null, "head": number }`. See [Reasoning status and diagnostics](#reasoning-status-and-diagnostics). |
| GET    | `/$/reason/{ds}/diagnostics` | `DiagnosticsReport`: OWL 2 RL inconsistency checks. |
| DELETE | `/$/reason/{ds}`             | Drops materialized inferences. |
| GET    | `/$/tasks`                   | `[Task]` |
| GET    | `/$/tasks/{id}`              | `Task` |
| DELETE | `/$/tasks/{id}`              | *Extension.* Cancels a task that accepts cancellation: a queued task, a clone until it is in place, or an N-Quads backup. Returns `202` with the `Task`, which ends `cancelled`. Other tasks and finished ones get `409 {code: "not-cancellable"}`. Needs `admin` on the task's dataset, or `server-admin` for a server-wide task. |
| POST   | `/$/cache/clear/{ds}`        | *Extension (no Fuseki equivalent).* Drops the dataset's cached query results. Returns `{ "cleared": number /* entries */, "bytes": number }`. |
| GET    | `/$/prefixes/{ds}`           | `{ "prefixes": { "rdf": "http://…#", … } }`: the dataset's prefixes plus well-known ones. |
| GET    | `/{ds}/prefixes`             | Modelled on Fuseki's prefixes service. `?prefix=p` returns `{ prefix, uri }`, or `404` if `p` is unbound. `?uri=u` returns `{ uri, prefixes: [...] }`. With neither, the response is `{ prefixes: {...} }` with the stored prefixes only. |
| POST/PUT | `/{ds}/prefixes`           | Binds `prefix` to `uri`, given in the query, a form or a JSON body `{prefix, uri}`. Names may be up to 256 bytes and IRIs up to 4096. An invalid name or IRI is a `400`. So is a new prefix once the dataset has `--max-prefixes` (1000) of them. Replacing an existing binding is fine. Prefixes of loaded data are added up to the same limit. Prefixes are metadata, so no commit is made. |
| DELETE | `/{ds}/prefixes?prefix=p`    | Removes a binding. Returns `204`, or `404` if the prefix is unbound. |

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

**Task slots.** At most `sparkles serve --max-tasks` background tasks run at once (default
4, `0` for no limit). Background tasks are compaction, clones, reasoning, full-text and
spatial index builds, and N-Quads backups. The others wait as `queued`, in start order,
and can be cancelled while they wait. Backup repository tasks (`backup-*` kinds) wait for
their own `--backup-max-tasks` slots instead. Starting a task while 1000 already wait
returns `503`. The task list keeps every queued and running task and the 200 most recent
finished ones.

## Schema discovery

The design and its rationale are in [C02 Schema discovery](specs/C02-schema-discovery.md).

`GET /$/schema/{ds}` reports the classes and predicates of a dataset in two separate
layers:

* **observed** holds exact counts over the selected graphs at one snapshot. A triple
  stored in several selected graphs counts once. These counts measure the current data and
  are not constraints. `maxPerSubject: 1` only says that no subject has two values *now*.
* **declared** holds what the RDFS/OWL vocabulary in the data asserts, such as `rdf:type`
  `owl:Class`, `rdfs:subClassOf`, `rdfs:domain`, `owl:FunctionalProperty` and labels. Only
  IRI objects are listed. Blank-node class expressions such as `owl:Restriction` are
  counted in `totals.anonymousClassExpressions`.

A class is listed if any of these hold:

* it is an IRI object of `rdf:type` in the selection
* it is declared with `rdf:type rdfs:Class | owl:Class | rdfs:Datatype`
* it is an IRI subject or object of `rdfs:subClassOf`, `owl:equivalentClass` or
  `owl:disjointWith`.

A predicate is listed if it occurs in the selection (`observed.triples > 0`). A predicate
with no triples is listed if it is declared, by a property type,
`rdfs:domain`/`range`/`subPropertyOf` or `owl:inverseOf`. Nothing is truncated. Every list
reports its `total` and is paginated with a cursor.

All parameters are optional, and the read-only server accepts them:

| Param | Values | Default | Meaning |
|---|---|---|---|
| `graph` | `default`, `union`, a graph IRI; also `urn:x-arq:DefaultGraph`, `urn:x-arq:UnionGraph` | `default` | Graphs whose triples are counted. With `--union-default-graph`, `default` is every graph. A graph IRI with no quads returns `404`. |
| `declaredGraph` | same | same as `graph` | Graphs read for declarations, for example an ontology in its own named graph. |
| `reasoning` | `true`, `false` | `true` if the dataset has materialized inferences | Counts `urn:x-sparkles:inferred` as part of `default` / `union`. |
| `declared` | `asserted`, `all` | `asserted` | `all` also reads declarations from the inferred graph. That graph holds the transitive closure of `rdfs:subClassOf`, the `rdfs:Resource` superclasses, and similar inferences. |
| `limit` | 1–10000 | 1000 | Page size. The summary uses it for both first pages. |
| `cursor` | opaque | — | The `next` of the previous page. Send the same selection parameters with it. |
| `timeout` | seconds | server query timeout | Time budget for computing the report. |

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
server keeps the last report per dataset. The summary and a page request without a cursor
reuse that report while the snapshot and the selection are unchanged, and compute a new
one otherwise. A cursor from an older snapshot is still served while its report is the
one kept. Once a newer report replaces it, the request fails with `409` and the client
restarts from the first page. Cursors do not survive a restart.

**Errors.** Errors have a `{ "error" }` body.

* `400`: a bad parameter, a malformed cursor, or a cursor issued for other selection
  parameters.
* `404`: an unknown dataset or a graph with no quads.
* `408`: the report did not finish within `timeout`, for example
  `"schema discovery exceeded 60s while scanning predicates (412/9031); narrow graph= or
  raise timeout="`.
* `409`: a stale cursor, as described above.
* `413`: the dataset has more than `--schema-max-entries` (default 1,000,000) classes or
  predicates, for example `"dataset has 1204331 classes (limit 1000000)"`.

A report is never returned partially.

The counts come from one ordered pass over the PSO index and one over the POS index per
predicate, so a report costs about two sequential reads of the selected triples.

**VoID export.** The summary is also served as RDF, as a description in the
[VoID](https://www.w3.org/TR/void/) vocabulary. Ask for it with an RDF media type in
`Accept`, or with the `format` parameter. The media types are `text/turtle`,
`application/n-triples`, `application/ld+json`, `application/rdf+xml`,
`application/trig` and `application/n-quads`. The `format` values are `turtle`,
`ntriples`, `jsonld`, `rdfxml`, `trig` and `nquads`, and `format=json` asks for the JSON
document, which stays the default. The selection parameters apply as above. `limit` and
`cursor` do not, because the description is always complete.

```turtle
<urn:x-sparkles:schema:wiki:42> a void:Dataset ;
    dcterms:title "wiki" ;
    dcterms:created "2026-10-02T12:04:00Z"^^xsd:dateTime ;
    void:triples 5 ;                # distinct triples in the selection
    void:entities 3 ;               # distinct IRI subjects
    void:classes 2 ;                # distinct rdf:type objects
    void:properties 3 ;             # predicates with triples
    void:distinctSubjects 3 ;
    void:distinctObjects 4 ;
    void:classPartition _:c1 , _:c2 ;
    void:propertyPartition _:p1 , _:p2 , _:p3 .
_:c1 void:class ex:Person ; void:entities 2 .   # instances
_:c2 void:class owl:Class ; void:entities 1 .
_:p1 void:property ex:knows ; void:triples 1 ; void:distinctSubjects 1 ; void:distinctObjects 1 .
_:p2 void:property rdf:type ; void:triples 3 ; void:distinctSubjects 3 ; void:distinctObjects 2 .
_:p3 void:property rdfs:label ; void:triples 1 ; void:distinctSubjects 1 ; void:distinctObjects 1 .
```

The node's IRI is `urn:x-sparkles:schema:<dataset>:<snapshot version>`. Every class with
instances gets a class partition, and every predicate with triples a property
partition. The partitions reuse the report's exact counts. `void:entities`,
`void:distinctSubjects` and `void:distinctObjects` of the whole selection are not part of
the JSON report. They cost one more pass over the SPO and OSP indexes, which only RDF
requests make. The declarations follow the description as the triples that assert them:
`rdf:type`, `rdfs:subClassOf`, `owl:equivalentClass`, `owl:disjointWith`, `rdfs:domain`,
`rdfs:range`, `rdfs:subPropertyOf`, `owl:inverseOf`, labels, comments and the
`owl:Ontology` headers. `declarations=false` leaves them out. Labels keep their language
tags.

The CLI equivalent prints the complete report without pagination:
`sparkles schema --loc DB [--graph default|union|IRI] [--declared-graph G]
[--no-inferences] [--declared asserted|all] [--format text|json|void|turtle]
[--timeout S] [--max-entries N]`, or `--data FILE…` in place of `--loc`. `json` is the
`SchemaSummary` with every item and `next: null`. `text` prints one line per class and per
predicate. `void` prints the VoID description in Turtle, and `turtle` prints it with the
declarations. The command exits with status 2 when the timeout or the entry cap is
exceeded. The Rust API is `sparkles::schema::discover`, and
`sparkles::schema::void_text` renders a report as VoID.

### Clone

The design and its rationale are in [C06 Clone-to-sandbox](specs/C06-clone-to-sandbox.md).

`POST /$/datasets/{ds}/clone` copies one consistent snapshot of `{ds}` into a new,
independent persistent dataset. Use it to try updates, reasoning or loads without touching
the original. Parameters come from the query string, a form body or a JSON body:

| Param | Required | Meaning |
|---|---|---|
| `name` | yes | Name of the new dataset. |
| `inferences` | no, default `copy` | `copy` copies the inferred graph and the reasoning status. `drop` copies neither. |

The copy has every quad of every graph, including blank-node graph names and triple
terms. It keeps the same blank-node ids (`_:b<hex>` labels) and the prefixes, and gets a
freshly compacted index. The clone starts a new lineage, with a new dataset id and a root
commit `0`. The source's id and the copied commit are kept as `forkedFrom`. Commit
history, the WAL and caches are not copied. Full-text search stays enabled with the same
configuration, and the clone builds its own index when it is first opened.

With `inferences=copy`, inferences that were fresh at the copied commit are fresh in the
clone. Stale ones stay stale, with `staleReason: "inherited from source at clone time"`,
and unknown ones stay unknown. Updates to the source continue during the clone and are not
included.

The endpoint returns `202` with a `Task` (`kind: "clone"`, `target`) and
`Location: /$/datasets/{name}`. The errors are:

* `400`: a missing or invalid `name`, a bad `inferences`, or `type=mem`. In-memory clones
  are not supported yet.
* `403`: the server is read-only.
* `404`: the source is unknown.
* `409`: `name` is registered, or another task is creating it, or
  `<data>/databases/{name}` exists without being a registered dataset. While the clone
  runs, `POST /$/datasets` with that name also gets `409`.

The dataset appears, and is saved in `config.json`, only when the task is `done`. A failed
task leaves no directory and releases the name. Unfinished clones are removed at startup.

```ts
type DatasetOrigin = {            // origin.json in the clone's directory
  originFormat: 1; clonedAt: string;
  source: { name: string; path?: string; version: number; generation: string; quads: number };
  forkedFrom: { id: string; seq: number };
  inferences: "copy" | "drop";
};
```

`sparkles clone --loc SRC --to DST [--inferences copy|drop]` does the same offline. `DST`
must not exist or must be empty. `SRC` must be a database that no server has open.

## Per-dataset SPARQL protocol (Fuseki compatible)

| Method     | Path                  | Description |
|------------|-----------------------|-------------|
| GET/POST   | `/{ds}` , `/{ds}/sparql`, `/{ds}/query` | SPARQL 1.1 Query protocol, with a `query=` parameter, an `application/sparql-query` body, or a form. Supports `default-graph-uri` / `named-graph-uri`. |
| any        | `/{ds}`               | Also the update endpoint (`update=` or `application/sparql-update`), and the Graph Store endpoint for any other body. A form body (`application/x-www-form-urlencoded`) must hold `query` or `update`. A form with neither is refused and never read as RDF. The refusal is a `400`, or a write's authorization error for a caller without write access. |
| POST       | `/{ds}/update`        | SPARQL 1.1 Update protocol, with an `update=` form or an `application/sparql-update` body. An update sent with GET (`/{ds}?update=…`) gets `405`. |
| GET/PUT/POST/DELETE/HEAD | `/{ds}/data` , `/{ds}/get` | Graph Store Protocol, with `?default` or `?graph=<iri>`. A GET with neither returns the whole dataset as N-Quads or TriG. GET is streamed from one snapshot (see [Budgets](#budgets)). |
| POST       | `/{ds}/upload`        | Multipart file upload. The format comes from the file name extension or the content type. Optional `graph` field. |
| POST       | `/{ds}/shacl`         | SHACL validation, as in Fuseki's `/{ds}/shacl`. See [SHACL validation](#shacl-validation). |
| POST       | `/{ds}/shex`          | ShEx validation. This is a Sparkles extension; Fuseki has none. See [ShEx validation](#shex-validation). |

Results are negotiated with `Accept` or, Fuseki style, the `format=` parameter:

* SELECT/ASK: `application/sparql-results+json` (default), `application/sparql-results+xml`,
  `text/csv`, `text/tab-separated-values`, and `application/x-sparkles+json` (see below).
* CONSTRUCT/DESCRIBE/GSP GET: `text/turtle` (default), `application/n-triples`,
  `application/n-quads`, `application/trig`, `application/ld+json`, `application/rdf+xml`.

Query parameters beyond the standard protocol:

* `timeout=<seconds>` sets the query timeout. The default is 60 s
  (`sparkles serve --timeout`). The value is capped at `--max-timeout`, which defaults to
  1800 s, is never below `--timeout`, and means no cap when set to `0`.

  Updates, Graph Store `PUT`/`POST`/`DELETE` and uploads accept `timeout` too, under the
  same cap. For writes the cap is never below `--update-timeout`. For a Graph Store write
  or an upload, the timeout starts once the body has been received. Without `timeout`,
  writes run under `--update-timeout`, which is unset by default. A timed-out write
  changes nothing. A `408` names the timeout that applied in `timeoutSeconds`.

  A write whose client disconnects is cancelled and commits nothing, even while it waits
  for the dataset's writer lock. A commit that has already started completes.
* `send=<n>` caps the number of rows serialized. The UI uses it so that a huge result does
  not hang the browser. `meta.totalRows` still reports the full count.
* `reasoning=true|false` includes or excludes materialized inferences. The default is
  `true` if the dataset has any.
* `nocache=true` bypasses the query result cache, so nothing is read from it or stored in
  it. It is meant for benchmarking, and `explain` accepts it too. `sparkles serve
  --result-cache-mb N` sets the server-wide cache budget (default 512, `0` disables the
  cache). The cache is keyed by snapshot version, so updates invalidate it.
  `POST /$/cache/clear/{ds}` empties it.

## Commits

The design and its rationale are in [CI Durable commit identity](specs/CI-commit-identity.md).

Every dataset has a **dataset id**, a UUID created with it, and a gap-free **commit
sequence**. Each write that changes data gets the next `seq`. Such writes are updates,
Graph Store PUT/POST/DELETE, uploads, loads and reasoning. A write with no net effect, such as inserting a
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
`Accept: application/x-sparkles+json` or `receipt=true`, the body adds a receipt. The
status stays the same, except that a Graph Store DELETE returns `200` instead of `204`.

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
| GET | `/$/commits/{ds}` | Lists commits, newest first. `?limit=` sets the page size (default 50, max 1000). `?before=<seq>` pages backwards, and `?after=<seq>` lists the commits after `seq`, oldest first. Returns `{dataset, datasetId, head, firstRetained, complete, commits: Commit[], next: string \| null}`. |
| GET | `/$/commits/{ds}/{ref}` | One commit. `ref` is `42`, `commit:42` or `head`. `404` beyond the head, `410` if the commit is no longer retained. |

Entries of `GET /$/datasets[/{ds}]` gain `id`, `head` and `modified`, the head's timestamp.
`sparkles log --loc DB [--limit N] [--before SEQ | --after SEQ | --at REF] [--format json]`
lists commits without taking the database lock, so it works next to a running server.

## Point-in-time reads and snapshots

The design and its rationale are in [F06 Named snapshots and point-in-time queries](specs/F06-snapshots-and-point-in-time.md).

Every commit since the dataset's last compaction or bulk commit can be read at no extra
cost. Its state is the current index generation plus a prefix of its write-ahead log.
Older commits stay readable while a **named snapshot** or the **retention window** keeps
the generation that holds them. Compaction and bulk commits then keep that generation
instead of deleting it. Only persistent datasets have history.

**Selector.** The `at` parameter, in the query string or a form body, selects the state
to read. `/{ds}/sparql`, `/{ds}/query`, `/{ds}?query=`, `/{ds}/explain` and Graph Store
`GET`/`HEAD` accept it.

| `at` | State |
|---|---|
| `head` (or absent) | The live state. |
| `42`, `commit:42` | The state right after commit 42. |
| `time:2026-09-30T14:03:11.482Z` | The last commit at or before that instant. Any RFC 3339 offset works, and a `+` that arrives as a space is accepted. |
| `snapshot:NAME` | The commit a named snapshot pins. |

Responses add `Sparkles-At`, the selector in canonical form, and `Sparkles-Head`. For a
past state they also add `Memento-Datetime`, the commit's time, and
`Link: <…>; rel="original"` (RFC 7089). `Sparkles-Commit` is the commit read. The
freshness of inferences (`Sparkles-Inferences`) is reported for the live state only.
`text:query` works only at the head and returns `501` at a past commit. Vector search
works at any commit. Writes with `at`, even `at=head`, are refused with `400` and
`code: "at-on-write"`.

Errors carry a `code`. A malformed selector is `400 invalid-at`. A commit beyond the
head, an unknown snapshot or a time before history is a `404`. A commit whose data is no
longer kept is `410 history-gone`, and the body lists the readable ranges:

```json
{ "error": "commit 12 is no longer reconstructable; the oldest reconstructable commit is 40",
  "code": "history-gone", "commit": 12, "head": 57, "oldestReconstructable": 40,
  "reconstructable": [ { "from": 40, "to": 57 } ], "metadata": { "seq": 12, … } }
```

In-memory datasets return `501 history-unsupported`. Materializing a past state is bounded
by `--history-cache-mb` (default 1024, `507` beyond it) and by the request timeout. Results
are cached, and one materialization runs at a time.

| Method | Path | Description |
|---|---|---|
| GET | `/$/snapshots/{ds}` | `{ dataset, datasetId, head, snapshots: NamedSnapshot[] }` |
| POST | `/$/snapshots/{ds}` | Pins a commit. The body is `{ name, at?: selector (default head), note? }`, as JSON, a form or the query string. Returns `201` and `Location`, or `200` if the name already pins that commit. `409` if the name pins another commit. `409` with `code: "history-limit"` beyond `--max-snapshots` (256) or `--history-max-generations` (8). `410` if the commit is no longer readable. |
| GET | `/$/snapshots/{ds}/{name}` | `NamedSnapshot` |
| DELETE | `/$/snapshots/{ds}/{name}` | `204`. Generations that only this snapshot kept are removed. |
| GET | `/$/history/{ds}` | `HistoryStatus` |
| PUT | `/$/history/{ds}` | Sets the retention window `{ keepCommits?: number \| null, keepAge?: "7d" \| seconds \| null }` and returns `HistoryStatus`. |

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

A pin at the head costs nothing, because the next generation starts at that commit. A pin
inside a generation keeps the whole generation, so its other commits stay readable too.
Removing a generation first renames it to `gen-NNNN.deleting`, so an interrupted removal
is finished at the next open. `GET /$/commits/{ds}` adds `oldestReconstructable` and
`reconstructable`, plus `reconstructable` and `snapshots` on each commit.

The CLI has `sparkles snapshot create --loc DB NAME [--at SEL] [--note TEXT]`,
`snapshot list|history --loc DB [--format json]`, `snapshot delete --loc DB NAME`,
`snapshot retain --loc DB [--keep-commits N] [--keep-age 7d] [--off]`,
`sparkles query --loc DB --at SEL …` and `sparkles dump --loc DB --at SEL`. These commands
open the database, so stop a server that holds it or use the HTTP API. The Rust API has
`Store::snapshot_at`, `create_snapshot`, `set_retention` and `history`.

## Backup repositories

The design and its rationale are in [F05 Backup repositories](specs/F05-snapshot-repositories.md).

*Extension.* Backup repositories come from the `backup` cargo feature of
`sparkles-server`, which is on by default. A **repository** holds deduplicated,
content-addressed copies of dataset files. It is either a directory (`fs`, a local or
mounted file system) or a bucket prefix (`s3`, AWS S3 or an S3-compatible service such as
MinIO, Cloudflare R2 or Ceph RGW). A **backup** is one persistent dataset at one commit,
described by an immutable manifest.

Backups are made, listed, restored and verified per dataset under `/$/backups/{ds}`.
Repositories are registered and maintained under `/$/repositories`. Lifecycle policies,
which hold schedules and retention, live under `/$/backup-policies`. The web UI has a
Backups page for all three. The older `POST /$/backup/{ds}`, which writes an N-Quads dump
in the data directory, is unchanged.

**What a backup holds.** A backup contains:

* the files of the dataset's current index generation (`gen-NNNN/…`): the permutations,
  the vocabulary, `wal.log` and `delta.vocab`
* the commit catalog `commits.bin`
* `CURRENT`, `dataset.json` and `prefixes.json`
* `text.json`, `origin.json`, `validation.json` and `validation-shapes.ttl`, when the
  dataset has them
* `reasoning.json`, unless its inferences were made at a later commit than the captured
  one

The full-text index is left out and rebuilt when the restored dataset opens
(`derived.text.rebuildOnRestore`). Older index generations, named snapshots and the
retention window (`history.json`) are left out too. Point-in-time reads of a restored
dataset therefore reach back only to the start of the backup's generation. In-memory
datasets cannot be backed up (`501 backup-unsupported`).

**Capture.** A backup pins one commit, the head when the task starts, without blocking
writers. Under the writer lock it only records the length of the append-only files and
the prefixes, and `sparkles_backup_capture_lock_seconds` measures that step. It then reads
through open file handles. Writes, compactions and bulk commits continue during the
upload. The backup holds a lease on its generation, so history collection keeps the
directory until the upload ends. `GET /$/history/{ds}` shows the lease as a hold
`backup:<name>`.

**Incremental and deduplicated.** Files are stored as blobs named by the SHA-256 of their
content. Generation files are immutable, so each is cut into 32 MiB pieces, and a piece
the repository already holds is not uploaded again. A piece that the parent backup
references is reused without a request. The parent is the newest backup of the same
dataset id. Any other piece over 1 MiB is first looked up with a `HEAD`, which
deduplicates across datasets and after an interrupted backup.

For the append-only files (`wal.log`, `delta.vocab`, `commits.bin`), only the bytes
appended since the parent are uploaded, as new segments. A file with more than 64
segments, or one that no longer extends the parent's copy, is stored from scratch. A
backup after a few writes therefore adds little more than the new WAL records and catalog
entries. A backup after a compaction uploads the new generation in full. Blobs are
LZ4-compressed when that saves at least 10 %. A backup's `addedBytes` counts the bytes it
was first to upload, and `logicalBytes` is the size of its files.

**Consistency.** The manifest `backups/<name>.json` is written last, with a conditional
create, so a backup exists if and only if its manifest does. A failed or cancelled backup
leaves only unreferenced blobs, which the next backup reuses and garbage collection
removes. Manifests and blobs never change after they are written. A restore makes these
checks before it publishes a dataset:

1. It validates the manifest's paths, sizes and format before writing anything.
2. It checks every blob's length and SHA-256, and every file's SHA-256.
3. It runs `sparkles check` on the restored directory, `quick` by default.
4. It opens the directory and compares its head commit and quad count with the manifest.
   A mismatch fails with `500 restore-mismatch`.

**Names.** Repository and policy names follow `[a-z0-9][a-z0-9_-]{0,63}`. They share one
namespace on a server, so a policy cannot take a repository's name and a repository
cannot take a policy's. A policy cannot be named `preview`. Backup names follow
`[A-Za-z0-9][A-Za-z0-9._-]{0,63}`. The default name is `{dataset}-{YYYYMMDDtHHMMSSz}`,
for example `wiki-20260930t140511z`. When needed, the dataset part is shortened or its
other characters are replaced by `-`.

**Identity.** Every dataset has a dataset id, a UUID (see [Commits](#commits)), and a
backup records it. The `identity` option of a restore decides the restored dataset's id:

| `identity` | Dataset id |
|---|---|
| `auto` (default) | The backup's id, unless a dataset on this server already has it, in which case `auto` acts as `new`. The dataset being replaced counts, so restoring in place of the live dataset the backup came from mints a new id. |
| `new` | A fresh id. `forkedFrom: {id, seq}` names the backup's dataset and commit. Commit numbers continue from the backup's commit. |
| `keep` | The backup's id. Fails with `409 duplicate-dataset-id` if another dataset has that id. It also fails when replacing in place a dataset with that id whose head is past the backup's commit, because the same commit numbers would then name different commits. |

A restored dataset's `DatasetInfo` has `restoredFrom: {repository, backup, datasetId,
seq}`, and its directory has a `restore.json`.

**Which backups belong to `/{ds}`.** Backups are matched by dataset id, not by name alone.
A new dataset that reuses a deleted dataset's name therefore cannot reach the old
dataset's backups. The backups of `/{ds}` are those of the live dataset `ds`, plus those
of the dataset it replaced under the same name by an in-place restore (its `forkedFrom`
id). A `server-admin` also sees every backup taken of a dataset named `ds`, which is how a
deleted dataset is restored for disaster recovery. A backup outside this set returns
`404 no-such-backup`, as a missing one does. In `GET /$/backups/{ds}`, `sameLineage`
marks the backups of the live dataset or of the one it replaced.

### Backup routes

Every request body is JSON, and an empty body means `{}`. Unknown fields are ignored. Task
endpoints return `202` with the `Task` (see [Datasets (admin)](#datasets-admin)) once it
has started or been queued. Some also return a `Location`, as noted.

| Method | Path | Needs | Description |
|--------|------|-------|-------------|
| GET | `/$/repositories` | any caller | `{repositories: (Repository \| RepositoryBrief)[]}`. A `server-admin` sees every repository. A caller with `admin` on some dataset sees `{name, type, readonly, reachable}`, enough to pick a target. Other callers get `[]`. |
| POST | `/$/repositories[?verify=false]` | `server-admin` | Registers a repository. The body is a `RepositoryConfig`. An empty location is initialized, unless `readonly` is set, and an existing repository is attached. The connection test then runs, unless `?verify=false` skips it. Returns `201`, `Location: /$/repositories/{repo}` and the `Repository` with `test`. A location that cannot be reached is registered anyway and shown as unreachable, with a failed `test`. `409 repository-exists` for a taken name, or for a location or repository id registered under another name. `409 not-a-repository` for a location that holds other files. `422 incompatible-repository`. |
| GET | `/$/repositories/{repo}` | `server-admin` | `Repository`. Its totals and last GC are refreshed in the background. |
| PUT | `/$/repositories/{repo}` | `server-admin` | Changes the settings. The body is a `RepositoryConfig`, in which `name` may be left out and cannot change. `409 location-immutable` if `type`, `path`, `bucket`, `prefix` or `endpoint` changes. `409 read-only-config` for a repository from the config file. The repository is reopened with the new settings at its next use. |
| DELETE | `/$/repositories/{repo}` | `server-admin` | Unregisters the repository (`204`). Its contents stay. `409 repository-in-use` while a policy backs up into it (`policies`) or a task uses it (`task`). `409 read-only-config`. |
| POST | `/$/repositories/{repo}/test` | `server-admin` | Runs the connection test and returns a `TestReport`. The test creates an object under `probe/` with a conditional create. It creates the object again and expects "already exists", which shows that conditional writes work. It then reads, lists and deletes the object. |
| POST | `/$/repositories/{repo}/verify` | `server-admin` | Verifies every backup and counts orphaned blobs. The body is `{level?: "exists" \| "data"}`, default `exists`. `restore` is a `400 invalid-request`. Starts a server-wide `backup-verify` task with `detail: VerifyReport`. |
| GET | `/$/repositories/{repo}/backups` | `server-admin` | `{backups: BackupSummary[], next: string \| null}`, newest `completed` first. Filters are `?dataset=NAME`, `?datasetId=UUID` and `?policy=P`. `?limit=N` defaults to 100, at most 1000. `?before=T` lists only backups completed before the RFC 3339 instant `T`. Pass `next` to get the following page. |
| POST | `/$/repositories/{repo}/gc` | `server-admin` | Deletes unreferenced blobs. The body is `{dryRun?: boolean, graceHours?: number}`, and the grace period defaults to 24 hours. Starts a server-wide `backup-gc` task with `detail: GcReport`. `409 repository-read-only`. |
| GET | `/$/repositories/{repo}/locks` | `server-admin` | `{locks: Lock[]}` |
| DELETE | `/$/repositories/{repo}/locks/{id}` | `server-admin` | Breaks a lock (`204`). The action is audited. `404 no-such-lock`, `409 repository-read-only`. |
| GET | `/$/backups/{ds}[?repository=R]` | `read` on `ds` | `{dataset, datasetId: string \| null /* the live dataset's */, backups: BackupSummary[]}`. Lists the dataset's backups in every repository, or in `R` only, newest first, with `sameLineage`. A repository that cannot be reached is left out. One found unreachable in the last minute is not tried again. |
| POST | `/$/backups/{ds}` | `admin` on `ds` | Backs up now. The body is `{repository, name?, note?}`. Starts a `backup-create` task with `detail: BackupSummary` and `Location: /$/backups/{ds}/{repo}/{name}`. Errors are `404 no-such-dataset`, `404 no-such-repository`, `409 repository-read-only` and `409 backup-exists`. `409 backup-in-progress` (with `task`) means a backup of the dataset into that repository is already running; only one runs at a time. `501 backup-unsupported` means an in-memory dataset. `507 insufficient-storage` means an `fs` repository whose file system has less than `--min-free-disk-mb` free. The task also fails with it when a blob would leave less. |
| GET | `/$/backups/{ds}/{repo}/{backup}` | `read` on `ds` | `Backup`: the summary plus the manifest's files, blobs and upload statistics. |
| DELETE | `/$/backups/{ds}/{repo}/{backup}` | `admin` on `ds` | Deletes the backup's manifest (`204`). Its blobs go at the next GC. `409 backup-busy` (with `task`) while a restore or verification of the backup runs on this server. `409 repository-read-only`. |
| POST | `/$/backups/{ds}/{repo}/{backup}/restore` | `admin` on `ds` and on the target | Restores the backup. The body is a `RestoreRequest`. Starts a `backup-restore` task with `Location: /$/datasets/{target}`. See [Restore](#restore). |
| POST | `/$/backups/{ds}/{repo}/{backup}/verify` | `admin` on `ds` | Verifies one backup. The body is `{level?: "exists" \| "data" \| "restore"}`, default `exists`. Starts a `backup-verify` task with `detail: VerifyReport`. The result is remembered as the backup's `verified`. |
| GET | `/$/backup-policies` | `server-admin` | `{policies: Policy[]}` |
| POST | `/$/backup-policies` | `server-admin` | Creates a policy. The body is a `PolicyConfig`. Returns `201`, `Location: /$/backup-policies/{policy}` and the `Policy`. `409 policy-exists` if a policy or repository has the name. `404 no-such-repository`, `409 repository-read-only`. A new policy first runs at its next scheduled instant. |
| POST | `/$/backup-policies/preview` | `server-admin` | Previews a schedule. The body is `{schedule, timezone?: string /* UTC */, count?: number /* 5, at most 20 */, nameTemplate?, dataset?}`. Returns `{next: string[] /* RFC 3339 UTC */, description, sample?}`. `sample` renders `nameTemplate` for `dataset` at `next[0]`. |
| GET | `/$/backup-policies/{policy}` | `server-admin` | `Policy` |
| PUT | `/$/backup-policies/{policy}` | `server-admin` | Replaces the settings and returns the `Policy`. The body is a `PolicyConfig`, in which `name` may be left out and cannot change. Enabling or disabling a policy is a PUT. `409 read-only-config` for a policy from the config file. A new schedule or time zone takes effect at its next instant. |
| DELETE | `/$/backup-policies/{policy}` | `server-admin` | `204`. A run in progress stops before its next dataset. `409 read-only-config`. |
| POST | `/$/backup-policies/{policy}/run` | `server-admin` | Runs the policy now, even a disabled one. Starts a server-wide `backup-policy` task with `detail: PolicyRun` and `Location: /$/tasks/{id}`. The schedule does not move. `409 policy-running` (with `task`), `503 too-many-tasks`, `403 server-read-only`. |
| POST | `/$/backup-policies/{policy}/retention[?dryRun=true]` | `server-admin` | Applies the policy's retention now. Returns `{dryRun, delete: BackupSummary[], keep: BackupSummary[], errors?: string[]}`. With `dryRun` nothing is deleted. A deletion that fails stays in `delete` and adds an entry to `errors`. |
| GET | `/$/backup-policies/{policy}/runs[?limit=N]` | `server-admin` | `{runs: PolicyRun[]}`, newest first. `limit` defaults to 50, at most 1000. |

A server built without the `backup` feature returns `404` for these paths, and the UI then
hides its Backups page.

**`--read-only` servers** still create, verify and delete backups, test repositories,
apply a policy's retention, garbage-collect repositories and break locks, on writable
repositories. Restores, changes to repositories and policies, and policy runs return
`403 server-read-only`. The scheduler runs no policies and logs that once, so their
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
only once it is complete and checked. A failed or cancelled restore therefore leaves
nothing behind. A restore needs 1.1 × the backup's `logicalBytes` free in the data
directory's file system, plus the `--min-free-disk-mb` reserve. Otherwise the request
returns `507 insufficient-storage`. The task checks again before downloading, because the
free space may have shrunk while it waited for a slot.

* **A new dataset** (`replace: false`). `target` must not exist, or the request fails with
  `409 dataset-exists`. The name is reserved while the task runs. The directory is then
  renamed into `databases/{target}` and the dataset is registered, as with a clone.
* **In place** (`replace: true`). `target` must be a persistent dataset in this server's
  data directory. An unknown target is `404 no-such-dataset`, and an in-memory dataset or
  one attached with `--loc` is `409 not-managed`. No backup task may be working on the
  target (`409 dataset-busy`, with `task`).

  Once the backup is downloaded and checked, requests to `/{target}` get
  `503 {code: "dataset-restoring"}` with `Retry-After: 5`, never `404`. The swap waits up
  to 30 s for requests in progress to finish. After that it fails with `409 dataset-busy`
  and the dataset stays as it was. The swap renames `databases/{target}` to
  `databases/.replaced-{target}-{task}`, renames the restored directory into its place,
  reopens the dataset, and removes the old files unless `keepReplaced` is set. If the new
  database fails to open, the old one is put back. A crash between the two renames is
  undone at the next start.

The task can be cancelled until it publishes the dataset. At that point `cancellable`
turns `false`. A restore also needs `admin` on the target name, or it fails with
`403 no admin access to the target name /x`. The task's `detail` is
`{backup: BackupSummary, dataset, datasetId, identity: "kept" | "new", forkedFrom?: {id,
seq}, check: object | null, millis}`.

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

Timestamps are RFC 3339 in UTC with milliseconds. Sizes are in bytes.

### Lifecycle policies

A policy backs up the datasets that match `datasets` into `repository` on a schedule, then
applies its retention. It backs up one dataset after another. Each is a backup like
`POST /$/backups/{ds}` and holds a task slot while it runs.

* **Schedules.** A schedule is a cron expression with 5 fields (minute hour day-of-month
  month day-of-week), or 6 fields with seconds first. `@daily`-style macros are not
  supported. Cron schedules are evaluated in the policy's `timezone`. A local time that a
  daylight-saving change skips runs at the first instant after the gap. A repeated one
  runs once, at its first occurrence. `every <duration>` (at least one minute) counts from
  the Unix epoch in UTC. `every 6h` therefore runs at 00:00, 06:00, … UTC, whatever the
  time zone and however often the server restarts. A schedule that does not parse or
  never runs, and an unknown time zone, return `400 invalid-schedule`.
* **Durations.** `expireAfter` and `every` take one or more `<n><unit>` terms, such as
  `30d`, `12h`, `1w`, `90m` or `1d 12h`. The units are `s`, `m`, `h`, `d` and `w`, or
  `sec`, `min`, `hr`, `hour`, `day`, `week` and their plurals.
* **Name templates.** A template can use these placeholders:
  * `{policy}` and `{dataset}`
  * `{seq}`, the head commit
  * `{run}`, the first 8 hex digits of the run id
  * `{time}`, the scheduled instant as `YYYYMMDDtHHMMSSz` in UTC
  * `{date:FMT}`, the scheduled instant in the policy's zone, formatted with
    `%Y %m %d %H %M %S %j %V`

  The result must be a valid backup name. Characters of the dataset name outside the
  grammar become `-`, and a long dataset name is shortened to keep the result within 64
  characters. A name that is taken gets `-2`, `-3`, …
* **Retention** considers only the policy's own backups in its repository, per dataset
  id, newest `completed` first. The first `minCount` are always kept. Each of the others
  is deleted if its position is ≥ `maxCount` or it completed more than `expireAfter` ago.
  A backup that a restore or verification is using at that moment is kept until the next
  run. Deleting a backup removes its manifest, and GC removes the blobs. With
  `gcAfterRetention`, a run whose retention deleted something starts a `backup-gc` task,
  at most once per 24 h per repository.
* **The scheduler** wakes at least once a minute. A new or rescheduled policy waits for
  its next instant. Instants missed while the server was down are covered by one run
  60 s after startup (`trigger: "catch-up"`). With `catchUp: "none"` they are recorded as
  `skipped` instead. An instant that arrives while the previous run is still going, or
  while the backup task queue is full (see [Backup tasks](#backup-tasks)), is recorded as
  `skipped` with a `reason`. A disabled policy lets its instants pass. Disabling or
  deleting a policy during a run stops the run before its next dataset, and the run ends
  `skipped`.

  A run is a backup task like any other and must be admitted to the queue. A manual run
  beyond the queue's capacity returns `503 too-many-tasks`. The GC that a run starts is
  admitted the same way. When the queue is full, that GC is not started and is tried again
  after the next run's retention.
* **Results.** A run is `ok` when every selected dataset was backed up or skipped
  (`in-memory dataset`, `unchanged`). It is `partial` when some failed, and `failed` when
  none succeeded. `lastSuccess` and `consecutiveFailures` follow these results. The last
  1000 runs of all policies are kept.

### Backup tasks

| Kind | `dataset` | `target` | `detail` |
|---|---|---|---|
| `backup-create` | the dataset | the backup name | `BackupSummary` |
| `backup-restore` | `{ds}` of the route | the dataset created or replaced | see [Restore](#restore) |
| `backup-verify` | the dataset, or `""` for a repository | the backup name, or the repository | `VerifyReport` |
| `backup-gc` | `""` | the repository | `GcReport` |
| `backup-policy` | `""` | the policy | `PolicyRun` |

Server-wide tasks (`dataset: ""`) are listed for `server-admin` only. Backup tasks take
their own slots. At most `sparkles serve --backup-max-tasks` (default 2) run at once, and
the others wait as `queued`. Up to 4 more per slot may wait. Past that, a request returns
`503 too-many-tasks`. Every backup task can be cancelled with `DELETE /$/tasks/{id}`,
including while it is queued. Cancellation is checked between object requests and every
8 MiB of data, and the task ends `cancelled`. A failed task's `message` starts with the
error code, for example `repository-unavailable: …`. A cancelled backup leaves only
unreferenced blobs. A cancelled restore leaves the target as it was.

### Backup errors

Errors are `{error, code, requestId}`. Some codes add `task`, `holder`, `policies` or
`field`.

| Status | `code` |
|---|---|
| 400 | `invalid-name`. `invalid-config`: a bad repository or policy setting, a repository body that is JSON but not a repository configuration, or a refused destination. `field` names the setting. `invalid-request`: a body that is not JSON (on every route), a backup, restore, verification, GC or policy body of the wrong shape, a malformed query parameter, a repository verification at level `restore`, or a negative `graceHours`. `invalid-schedule`: a bad schedule or an unknown time zone. |
| 403 | `server-read-only` |
| 404 | `no-such-repository`, `no-such-backup` (also for a backup of another dataset), `no-such-policy`, `no-such-dataset`, `no-such-lock` |
| 409 | `repository-exists`, `policy-exists`, `not-a-repository` (a location with other files), `location-immutable`, `repository-in-use` (with `policies` or `task`), `read-only-config`, `backup-exists`, `backup-in-progress` (with `task`), `backup-busy` (with `task`), `repository-read-only`, `repository-locked` (a conflicting lock outlived the 10 min wait, with `holder`), `dataset-exists`, `dataset-busy` (with `task`), `not-managed`, `duplicate-dataset-id`, `policy-running` (with `task`) |
| 422 | `incompatible-repository` (a newer repository format, or encryption), `incompatible-format` (an index format this build cannot read), `invalid-backup` (a manifest that fails validation, with `field`) |
| 500 | `restore-mismatch`, `internal` |
| 501 | `backup-unsupported` (an in-memory dataset), `not-implemented` |
| 502 | `repository-unavailable`: a storage error after retries. The message never includes URL query strings. |
| 503 | `catalog-lagging` (the commit catalog could not be flushed, so retry), `too-many-tasks`, `dataset-restoring` (with `Retry-After: 5`), `cancelled` (a cancelled task) |
| 507 | `insufficient-storage`. A restore has no room for 1.1 × the backup's size plus the `--min-free-disk-mb` reserve, or a backup into an `fs` repository would leave its file system with less than the reserve. |

For callers without `server-admin`, absolute paths in these messages are cut to their last
component. Storage requests are retried with exponential backoff, up to 10 retries within
3 minutes per request. A downloaded blob that fails its hash is fetched again twice.

### Locks and garbage collection

Operations take a lease object `locks/<uuid>.json` in the repository. Create, delete,
restore and verify take a **shared** lock, and GC's sweep takes an **exclusive** one. A
held lock is rewritten every 5 minutes. A lock not rewritten for 30 minutes is **stale**:
other operations ignore it and GC removes it. Staleness is judged by the storage server's
clock, never this host's. An operation that meets a conflicting lock retries with backoff
for up to 10 minutes, then fails with `409 repository-locked`. These leases let several
servers, and the CLI, share a repository. Read-only repositories take no locks.

GC first marks the blobs that every manifest references, under a shared lock, so backups
continue. It then takes the exclusive lock, lists the manifests again and deletes the
unreferenced blobs older than the grace period. The grace period defaults to 24 h, again
by the storage server's clock. The last GC's report is kept in the repository as
`gc/last.json` and shown as `lastGc`. A dry run reports what a real run would delete and
changes nothing.

A repository must have one writer at a time when it has `conditionalWrites: false`. The
same holds for a service without conditional creates, which the connection test reports
as `status.singleWriter`.

### Configuration file

`sparkles serve --backup-config FILE` (or `$SPARKLES_BACKUP_CONFIG`) reads repositories,
policies and the limits on API registrations from a TOML file. Keys are the snake_case
forms of the JSON fields. Unknown keys are errors, reported with line and column, and a
file that does not load stops the server at startup. SIGHUP re-reads the file. Its
repositories and policies replace the ones it defined before, and those created through
the API stay. If the file does not load on SIGHUP, everything stays as it was and the
error is logged. Entries from the file have `source: "config"` and return
`409 read-only-config` to `PUT` and `DELETE`. The file holds no secrets, only references
to them, and the server warns when it is readable by group or others. `sparkles repo add`
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

**Repositories registered through the API** or the UI are held to the operator's choices.
Otherwise a caller could point the server's credentials or its network access wherever
they like.

* `s3` credentials can only be `{"source": "named", "name": …}`, which names a
  `[credentials.<name>]` source of the config file. Anything else is a
  `400 invalid-config` with `field: "credentials"` or `"credentials.name"`. A caller can
  never choose environment variables, files or the default provider chain. Without a
  config file, an `s3` repository cannot be registered through the API.

  To set one up, first define the source in the config file. Write it by hand, or run
  `sparkles repo add NAME --s3 BUCKET … --credentials-name lab
  --credentials env:LAB_ACCESS_KEY,LAB_SECRET_KEY`. That command writes
  `[credentials.lab]` next to its own repository. Without `--credentials` it names an
  existing source instead. Then start the server with `--backup-config` on that file, or
  send it SIGHUP, and register the repository:

  ```sh
  curl -X POST http://localhost:3030/$/repositories -H 'Content-Type: application/json' -d '{
    "name": "lab", "type": "s3", "bucket": "lab", "endpoint": "http://127.0.0.1:9000",
    "pathStyle": true, "allowHttp": true,
    "credentials": {"source": "named", "name": "lab"}}'
  ```
* An `s3` endpoint, and every address its host name resolves to, must pass the server's
  outbound policy. The `--outbound-*` flags of `SERVICE` and `LOAD` set that policy, which
  allows only public addresses by default. Connections go only to the addresses checked.
  A MinIO on localhost needs `--outbound-allow 127.0.0.1` or `--outbound-allow-private`. A
  refused endpoint is a `400 invalid-config` with `field: "endpoint"`. These connections
  never go through a proxy from the environment (`HTTPS_PROXY`, `HTTP_PROXY`,
  `ALL_PROXY`), because the proxy would reach the endpoint past the address checks.
  Repositories from the config file, and the CLI's, use the environment's proxies as
  usual.
* `fs` repositories must lie under one of `[api] fs_roots` when it is set.
* `gcs` and `azure` repositories use the server's own credentials and can only come from
  the config file. `memory` is for tests.

Every `fs` repository, from either source, lies outside the data directory and outside
the directories of the server's config files (`--backup-config`, `--auth-config`).

### Backup metrics

| Name | Type | Labels |
|------|------|--------|
| `sparkles_backup_operations_total` | counter | `repository`, `operation` = `create` \| `restore` \| `verify` \| `delete` \| `gc`, `result` = `ok` \| `failed` \| `cancelled` |
| `sparkles_backup_operation_duration_seconds` | histogram (1 s … 2 h) | `operation` |
| `sparkles_backup_bytes_uploaded_total`, `…_bytes_downloaded_total` | counter | `repository` |
| `sparkles_backup_blobs_uploaded_total`, `…_blobs_reused_total` | counter | `repository` |
| `sparkles_backup_object_requests_total` | counter. Every storage request of every operation: backups, restores, verifications, GC, listings, connection tests and locks. "Not found" and "already exists" answers count as `ok`, and a failed request as `error`. | `repository`, `op` = `put` \| `get` \| `head` \| `list` \| `delete`, `result` = `ok` \| `error` |
| `sparkles_backup_last_success_timestamp_seconds` | gauge. The last backup of the dataset into the repository. | `dataset`, `repository` |
| `sparkles_backup_capture_lock_seconds` | histogram (0.5 ms … 1 s). How long a capture holds the writer lock. | |
| `sparkles_backup_repository_stored_bytes`, `…_logical_bytes`, `…_backups` | gauge. From the last listing. | `repository` |
| `sparkles_backup_lock_conflicts_total` | counter. `repository-locked` failures. | `repository` |
| `sparkles_backup_policy_runs_total` | counter | `policy`, `result` |
| `sparkles_backup_policy_last_success_timestamp_seconds`, `…_next_run_timestamp_seconds`, `…_consecutive_failures` | gauge | `policy` |

`repository` is capped like `dataset`. Beyond `--metrics-max-datasets`, the rest share
`$other`. Each backup task is a span `task backup-*`, with children such as
`backup.capture` (`sparkles.commit`, `sparkles.backup.lock_ms`). Task starts and ends are
logged under `sparkles::backup`. These events go to the `sparkles::audit` target with the
principal: `repository_added`, `repository_changed`, `repository_removed`,
`backup_deleted`, `restore_started`, `restore_finished`, `gc_finished`, `lock_broken` and
`policy_changed`.

### Files on the server

`<data>/backup/` holds these files:

* `repositories.json` and `policies.json`, the entries created through the API
* `policy-state.json`, when each policy last ran and succeeded
* `runs.json`, the run history
* `verify.json`, each backup's last verification on this server (`verified`)
* `cache/<repository id>/`, a manifest cache

Restores use `<data>/databases/.restore-*` and `.replaced-*`, which are removed or undone
at the next start, and `.kept-*` for `keepReplaced`. Restore-level verifications use
`<data>/tmp/verify-*`.

**The data-directory lock.** `sparkles serve` holds an OS lock on
`<data>/sparkles-server.lock` while it runs. A second server on the same data directory
fails at startup, and an offline `sparkles backup restore --data` refuses to write into
it.

A repository's own layout (format 1) is shared by the server and the CLI:

```text
<prefix>/
  sparkles-repo.json            marker: format, repository id, piece size (created once)
  blobs/<hh>/<sha-256>          content blobs, immutable (<hh>: the id's first two hex digits)
  backups/<name>.json           manifests, immutable, created last
  locks/<uuid>.json             lock leases
  probe/<uuid>                  connection-test objects
  gc/last.json                  the last GC's report
```

A blob is a 16-byte header followed by its payload. The header holds `SPKB`, format 1, the
codec (raw or LZ4), the encryption (none) and the plaintext length. A blob's id is the
SHA-256 of the plaintext, so writers that compress differently still deduplicate.

## Full-text search

The design and its rationale are in [F03 Full-text search](specs/F03-full-text-search.md).

Datasets can index their string and language-tagged literals for ranked (BM25) search.
Queries use Jena's `text:query` property function (`PREFIX text: <http://jena.apache.org/text#>`):

```sparql
SELECT ?s ?score ?label WHERE {
  (?s ?score ?label) text:query (rdfs:label "brown fox" 10 "lang:en") .
  ?s a ex:Book .
} ORDER BY DESC(?score)
```

* **Subject list.** `(?s ?score ?literal ?graph ?predicate)`. Every slot after the
  subject is optional. A constant subject restricts the search to that subject.
* **Object.** A query string, or `(predicate* "query" limit "lang:xx")`. A language tag
  on the query string acts as `lang:`.
* **Query syntax.** The syntax has terms, `"phrases"` (with `~slop`), `AND`/`OR` (OR by
  default), `+required`/`-excluded`, parentheses, and phrase prefixes `"quick bro"*`. A
  literal `:` is written `\:`.
* **Results.** There is one solution per matching quad, so a subject with two matching
  literals appears twice. `?score` is an `xsd:float`. In a merged default graph (the union
  default graph, or several `FROM`s), identical triples from different graphs count once.
* **Evaluation.** The search runs once within the active graph, and `GRAPH`, `FROM` and
  `reasoning=false` apply inside the search. `limit` is therefore the top n of that scope,
  before any join.
* **Analyzer.** Tokens are split on non-alphanumeric characters, lowercased and
  ASCII-folded, so `café` matches `cafe`.
* **Consistency.** Indexes are updated in the same commit as the data, so a query sees
  the text of its own snapshot, including the writes just before it. A write only stages
  its documents. The index commit, which writes a new segment, happens at the next text
  query that needs it, about once a second, or as soon as about 16,000 changes are
  staged. A burst of writes therefore shares one index commit. If an index is behind,
  after a failed update or during a rebuild, text queries return `503` until it is
  rebuilt. They never return stale results.
* **Durability.** Index commits are not fsynced. The write-ahead log is the durable
  record. The index is checkpointed (synced) about once a second while writes continue,
  before compaction and on close. After a crash, an index with unsynced changes, marked
  by a `text.dirty` file next to it, is checksum-verified and caught up from the WAL. The
  WAL also restores what was only staged. The index is rebuilt only if it is damaged or
  older than the WAL.
* **Errors.** `400` for malformed calls, unparseable query strings, predicates that
  are not indexed, and datasets without an index. `501` if the server was built without
  the `text` feature.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/$/text/{ds}` | `TextStatus` (below), or `{ "enabled": false }` |
| PUT | `/$/text/{ds}` | Enables or reconfigures the index. The body is a `TextConfig`, and an empty body means the defaults. Returns `202` with the build `Task` (`kind: "text-rebuild"`). |
| DELETE | `/$/text/{ds}` | Disables and deletes the index (`204`). |
| POST | `/$/text/{ds}/rebuild` | Rebuilds the index from the current data. Returns `202` with a `Task`, `409` if a rebuild is running, or `400` if the index is not enabled. |

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
in the database directory, in `text.json`, with the index in `text/`. The CLI commands
are
`sparkles text-index --loc DB [--predicate IRI…] [--exclude-graph IRI…] [--rebuild | --status | --disable]`
and `sparkles serve --text NAME[=config.json]`.

## Vector similarity

The design and its rationale are in [F04 Vector similarity search](specs/F04-vector-search.md).

Embeddings are ordinary literals of the datatype `<urn:x-sparkles:vector>`. The lexical
form is a JSON array of 1–16384 finite numbers, read as `f32`, for example
`"[0.1, -0.2, 0.3]"^^spk:vector` with `PREFIX spk: <urn:x-sparkles:>`. Literals are
stored and returned exactly as written. One that does not parse is stored but never
matched.

* **Functions.** `spk:cosine(?a, ?b)`, `spk:dot(?a, ?b)`, `spk:euclidean(?a, ?b)` (L2
  distance) and `spk:dimension(?a)`. They raise a type error on a malformed argument, a
  dimension mismatch, or a zero vector with cosine.
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
* **Implementation.** Vectors are packed per predicate and dimension on first use and
  cached per index generation. Every query overlays its snapshot's uncommitted inserts
  and deletes, so results always match its data. A process-wide budget caps the packed
  vectors. `sparkles serve --vector-memory-mb` sets it, and the default is 4096 (4 GiB).
  Beyond it a search returns `507`. A variable query vector gives `501`.
* **Status.** `GET /$/vector/{ds}` returns
  `{ budgetBytes, usedBytes, generation, predicates: [{ predicate, bytes, malformed, dimensions: [{ dimension, vectors }] }] }`
  for the predicates packed so far in the current generation. Packing happens on a
  predicate's first search. `vectors` counts a vector once per graph it is in.

## GeoSPARQL

The design and its rationale are in [G01 GeoSPARQL](specs/G01-geosparql.md).

With the `geo` cargo feature, which the server enables, Sparkles implements the GeoSPARQL
1.1 functions over geometry literals, Jena's `spatial:` property functions, and a spatial
index per dataset. The prefixes are `geo:` `<http://www.opengis.net/ont/geosparql#>`,
`geof:` `<http://www.opengis.net/def/function/geosparql/>`, `uom:`
`<http://www.opengis.net/def/uom/OGC/1.0/>`, `sf:` `<http://www.opengis.net/ont/sf#>` and
`spatial:` `<http://jena.apache.org/spatial#>`.

```sparql
SELECT ?f ?d WHERE {
  ?f geo:hasDefaultGeometry/geo:asWKT ?w .
  FILTER(geof:sfWithin(?w, "POLYGON((2.2 48.8, 2.5 48.8, 2.5 48.9, 2.2 48.9, 2.2 48.8))"^^geo:wktLiteral))
  BIND(geof:distance(?w, "POINT(2.2945 48.8584)"^^geo:wktLiteral, uom:kilometre) AS ?d)
} ORDER BY ?d
```

**Literals.**

* `geo:wktLiteral` is WKT with an optional leading CRS IRI
  (`<http://…/EPSG/0/4326> POINT(48.86 2.34)`). It supports Z, M and ZM layouts, `EMPTY`,
  and the empty string, which is an empty geometry. `LINEARRING`, `TRIANGLE`, `TIN` and
  `POLYHEDRALSURFACE` are read as line strings and polygons and keep their type for
  `geof:geometryType`.
* `geo:geoJSONLiteral` is an RFC 7946 geometry, always in CRS84.
* Literals are stored as written. `"POINT(1 2)"` and `"Point (1.0 2.0)"` are different
  terms with equal geometries: `=` compares terms, and `geof:sfEquals` compares
  geometries. A literal that does not parse is stored all the same. Functions give a type
  error on it, and the index skips it. A malformed geometry constant in a query is a `400`
  (`geo: malformed wktLiteral at offset N: …`).
* **CRSs.** The supported CRSs are CRS84 (the default, longitude first), CRS84h,
  EPSG:4326, EPSG:4979, the legacy `http://www.opengis.net/def/crs/EPSG/4326`, and Web
  Mercator (EPSG:3857). EPSG:4326 and EPSG:4979 are latitude first, as the EPSG
  definition says (GeoSPARQL Req 16). The legacy IRI is longitude first, as in Jena.
  `https` forms, URNs and other EPSG versions are accepted as aliases. A literal in
  another CRS is still a valid geometry. Accessors, constructions and relations between
  geometries of that same CRS work. Metric functions and mixes with other CRSs are type
  errors, and the index leaves the literal out.
* **Units.** Units can be OGC units (`uom:metre`, `uom:kilometre`, `uom:mile`,
  `uom:degree`, `uom:radian`, …), QUDT units (`http://qudt.org/vocab/unit/KiloM`, …) or
  EPSG URNs, given as IRIs or `xsd:anyURI` literals. An unknown unit is a type error.

**Functions.** Any bad argument is a type error, which leaves the variable unbound in BIND
and makes a FILTER false. A geometry result has the datatype and CRS of the first
geometry argument. The second geometry of a binary function is transformed into the
first one's CRS.

| Functions | Result |
|---|---|
| the 24 relations: `sfEquals` `sfDisjoint` `sfIntersects` `sfTouches` `sfWithin` `sfContains` `sfOverlaps` `sfCrosses`, `ehEquals` `ehDisjoint` `ehMeet` `ehOverlap` `ehCovers` `ehCoveredBy` `ehInside` `ehContains`, `rcc8eq` `rcc8dc` `rcc8ec` `rcc8po` `rcc8tppi` `rcc8tpp` `rcc8ntpp` `rcc8ntppi`; `relate(g1, g2, "T*F**FFF*")` | `xsd:boolean`, from the DE-9IM matrix. Planar, in longitude/latitude for geographic CRSs. |
| `distance(g1, g2, unit)`, `metricDistance(g1, g2)` | `xsd:double`. Geodesic on WGS 84 by default, or on a sphere with `"distance": "haversine"` in `geo.json`. Euclidean in projected CRSs. An angle unit gives the central angle. |
| `buffer(g, r, unit)`, `metricBuffer(g, r)`, `convexHull`, `envelope`, `boundary`, `centroid`, `intersection`, `union`, `difference`, `symDifference` | geometry (2D). A metric buffer on geographic data goes through a local projection, up to 1000 km. |
| `area(g, unit)`, `length`, `perimeter` and their `metric…` forms | `xsd:double`, geodesic on geographic CRSs |
| `getSRID` | `xsd:anyURI` |
| `transform(g, crs)`, `asWKT`, `asGeoJSON` | geometry |
| `dimension`, `coordinateDimension`, `spatialDimension`, `numGeometries` | `xsd:integer` |
| `is3D`, `isMeasured`, `isEmpty` | `xsd:boolean` |
| `geometryType` | `xsd:anyURI` (`sf:Point`, …) |
| `geometryN(g, n)` (1-based) | geometry |
| `minX` `minY` `maxX` `maxY` (in the literal's own axis order), `minZ` `maxZ` | `xsd:double` |

Operations over more input vertices than `serve --geo-op-vertices` (2,000,000) are type
errors. Constructed geometries count against the query's memory budget.

**`spatial:` property functions.** These use Jena's syntax and take constant arguments:

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

The subject is the feature. It is linked to a geometry by `?f geo:hasDefaultGeometry ?g`
or `?f geo:hasGeometry ?g` (the `featureLinks`), and `?g` holds a matching serialization.
There is one solution per feature. Under `GRAPH ?g`, the graph is that of the
serialization. With a `limit`, the function returns the nearest matches, with ties broken
by subject. For the box and cardinal functions, nearest means nearest to the box's
centre. Every match is tested exactly. Without an index (off, building or failed), the
answers are the same, computed by a scan. A malformed argument list, a coordinate out of
range, an unknown unit or a non-integer limit is a `400` (`spatial:<name>: …`). A
variable argument, or a build without the `geo` feature, gives `501`.

**The spatial index.** The spatial index is optional per dataset. It indexes the geometry
literals of the configured predicates in a packed R-tree over the generation's base, plus
an overlay of the rows committed since. The default predicates are `geo:asWKT`,
`geo:asGeoJSON` and `geo:hasSerialization`. Every snapshot sees exactly its own rows, so a
query can use the index at any commit. An update's own uncommitted changes and past
states (`?at=`) run without it.

A FILTER searches the index (`SpatialScan` in EXPLAIN) when it applies one of these tests
to the object of an indexed predicate and a constant geometry:

* one of the relations, except the disjoint ones
* `relate` with a pattern that needs an intersection
* `distance`/`metricDistance` compared with a constant

The `spatial:` functions search it too (`SpatialPf`). Results are the same with and
without the index, because every candidate is tested exactly. Some literals are skipped by
the index although a function could still match them, and these are candidates of every
search, whatever its window. They are literals over `maxGeometryBytes`, and literals in a
built-in CRS whose envelope has no place in longitude and latitude. Malformed literals,
literals over `maxVertices`, empty geometries and literals in an unknown CRS are never
candidates. They are type errors or empty, so no relation or distance with a constant can
hold for them. A `spatial:` call with a constant subject reads that feature's links
directly, with or without the index.

The index's base belongs to a generation. It is built when the index is enabled, and
again for each new generation (bulk loads, compaction). A new build takes over the
literals that the previous generation's index parsed, and parses only new ones. A
persistent database writes the base to `gen-NNNN/geo/` (`rtree.spkg`, `column.spkg`). The
files are checksummed, written to a temporary file and renamed. The database reads them
in place and decodes a geometry the first time a query needs it. Opening the database, or
enabling the same predicates, graphs and limits again, therefore parses no literal.

Files that are missing, damaged, written by another version, or made for another
configuration or generation are removed, and the base is built again. Queries run
without the index in the meantime. `serve --read-only` writes no index files, and builds
the index in memory when the files do not fit. The files are derived data. Backups and
clones leave them out, and they are deleted with their generation or by
`DELETE /$/geo/{ds}`.

**W3C Basic Geo.** With `"wgs84": true`, a subject with `wgs84_pos:lat` and
`wgs84_pos:long` (`http://www.w3.org/2003/01/geo/wgs84_pos#`) in the same graph is also a
point of the index. A subject with several of either gives every combination, as in
Jena. Values are numbers of any XSD numeric type, or strings that hold one, within ±90°
and ±180°. Other pairs are not points. A point lives while both of its quads do. The
`spatial:` functions find such a subject as a feature without a feature link, and the map
view reports it. `geof:` FILTERs do not see the pairs, because there is no geometry
literal, so they are not pushed down for them either.

Each spatial operator in an executed plan reports these `counters`:

* `candidates`: rows the index or the scan handed out
* `rechecked`: the candidates the index could not place
* `refined`: exact tests run
* `matched`: rows that passed
* `treeNodesVisited`
* `index`: `ready`, or the reason the plan ran without the index (`building (37%)`,
  `failed`, `over-budget`, `off`, …), or `feature-links` for a `spatial:` call with a
  constant subject
* `fallback`: the rows came from a scan instead of the index

| Method | Path | Description |
|--------|------|-------------|
| GET | `/$/geo/{ds}` | `GeoStatus` (below), or `{ "enabled": false }` |
| PUT | `/$/geo/{ds}` | Enables or reconfigures the index. The body is a `GeoConfig`, and an empty body means the defaults. Returns `202` with the build `Task` (`kind: "geo-index"`). Errors are `400 invalid geo configuration: …` and `409 spatial index build already running`. |
| DELETE | `/$/geo/{ds}` | Disables the index (`204`) and removes `geo.json` and the index files. |
| POST | `/$/geo/{ds}/rebuild` | Rebuilds the current generation's base, and its files, from RDF. Returns `202` with a `Task`, `400 spatial index is not enabled`, or `409` if a build is running. |
| GET | `/{ds}/geo?bbox=minLon,minLat,maxLon,maxLat[&graph=IRI][&predicate=IRI][&limit=N][&tolerance=DEG]` | Returns the indexed geometries that meet a CRS84 box, as `application/geo+json` (below). Needs read access to the dataset. Without a ready index (off, building or failed), the same answer comes from a scan of the configured predicates, or of the default ones. `400` for a bad `bbox` (not four numbers, min after max, or a latitude outside ±90), a `limit` outside 1–50,000, or a negative `tolerance`. `404` for an unknown dataset. |

```ts
type GeoConfig = {
  predicates?: string[];        // serialization predicates; default geo:asWKT, geo:asGeoJSON, geo:hasSerialization
  featureLinks?: string[];      // default geo:hasDefaultGeometry, geo:hasGeometry
  graphs?: { include?: "all" | string[]; exclude?: string[] };   // as for full-text search
  distance?: "geodesic" | "haversine";   // default "geodesic"
  maxGeometryBytes?: number;    // default 16 MiB: longer literals are not indexed
  maxVertices?: number;         // default 1000000 per geometry: not indexed, and a type error in functions
  wgs84?: boolean;              // W3C Basic Geo lat/long pairs as points (default false)
  queryRewrite?: boolean;       // default false: match the topological geo: properties against geometries too
  formatVersion?: 1;
};
type GeoStatus = {
  enabled: true;
  state: "ready" | "building" | "failed" | "over-budget";
  progress?: number; message?: string;
  generation: string;           // the generation the base was built for
  commit: number;               // the commit the status describes
  rows: { base: number; overlay: number; tail: number; wgs84?: number };  // wgs84: W3C Basic Geo points among them
  literals: number;             // distinct parsed geometries
  skipped: { malformed: number; unknownCrs: number; tooLarge: number; empty: number };
  crs: { [iri: string]: number };   // literals per CRS, unknown ones included
  memory: { treeBytes: number; geometryBytes: number; overlayBytes: number; budgetBytes: number;
            mappedBytes?: number };   // index files read in place (not counted against the budget)
  config: GeoConfig; formatVersion: 1;
  lastBuild?: { at: string; ms: number; rows: number };
  files?: { bytes: number; opened: boolean };   // the base's index files; opened: read, not built
};
type GeoFeatureCollection = {   // GET /{ds}/geo
  type: "FeatureCollection";
  features: {
    type: "Feature";
    id: string;                 // the row's subject (an IRI, or _:label)
    geometry: object;           // GeoJSON, CRS84, simplified with Douglas-Peucker
    properties: {
      subject: string;
      feature?: string;         // a feature linked to the subject (one Feature per link; the subject itself for a W3C Basic Geo point)
      graph: string | null;     // null: the default graph
      predicate: string;        // the serialization predicate, or wgs84_pos:lat_long
    };
  }[];
  truncated: boolean;           // more than `limit` features met the box
};
```

`GET /{ds}/geo` tests each geometry exactly against the box in CRS84. It transforms each
geometry to CRS84 and simplifies it to `tolerance` degrees. The default tolerance is the
box's width / 1024, and a ring keeps at least four positions. `graph` narrows the result
to one graph, with `urn:x-arq:DefaultGraph` for the default graph. `predicate` narrows it
to one serialization predicate, or to `http://www.w3.org/2003/01/geo/wgs84_pos#lat_long`
for the W3C Basic Geo points.

The `over-budget` state means that the index would need more than `serve --geo-mb`
(4096 MiB), and queries run without it. The `failed` state means that a build or a
commit's update of the index failed. The write itself never fails because of the index.
Queries run without the index until a rebuild or a compaction. The configuration lives in
the database directory as `geo.json`. `sparkles check` validates it, clones copy it, and
backups include it. The CLI commands are
`sparkles geo-index --loc DB [--predicate IRI…] [--feature-link IRI…] [--exclude-graph IRI…] [--wgs84] [--distance geodesic|haversine] [--rebuild | --status | --disable]`
and `sparkles serve --geo NAME[=geo.json]`.

### Hulls, aggregates, Jena filter functions, UTM and conversion

**More `geof:` functions.** These are planar in the CRS of `g`, like `convexHull`.

| Function | Result |
|---|---|
| `boundingCircle(g)` | The smallest circle that holds every vertex of `g` (Welzl). It is returned as a polygon of 128 sides drawn around the circle, so every input point is inside it. One distinct point gives a single point. |
| `concaveHull(g)`, `concaveHull(g, targetPercent)` | The concave hull of the vertices, from `geo` (concaveman). `targetPercent` in (0, 100] sets the concavity linearly as `targetPercent / 25`. 50 gives the default concavity 2.0, smaller values follow the input more closely, and 100 gives the convex hull. A value outside the range is a type error. Degenerate inputs, such as a point or a segment, give their convex hull. |
| `isSimple(g)` | `xsd:boolean`, by OGC simplicity. Points are always simple. Multipoints are simple without repeated points. Curves are simple if they do not meet themselves, except consecutive segments at their shared vertex and a closed curve at its closing vertex. Multicurves are simple if their members meet only at ends of both. Polygons need simple rings, and collections need simple members. |

**Aggregates.** `geof:aggBoundingBox`, `geof:aggBoundingCircle`, `geof:aggCentroid`,
`geof:aggConvexHull`, `geof:aggConcaveHull` and `geof:aggUnion` group like `SUM`,
`DISTINCT` included. `geof:aggCentroid` is the centroid of the union.
`geof:aggConcaveHull` uses the default concavity, because an aggregate takes one
expression.

```sparql
SELECT ?region (geof:aggUnion(DISTINCT ?w) AS ?shape)
WHERE { ?f ex:region ?region ; geo:hasDefaultGeometry/geo:asWKT ?w }
GROUP BY ?region
```

The result has the datatype and CRS of the group's first value, and the other values are
transformed into that CRS. The aggregate is unbound if a value is an error or not a
geometry, if a CRS has no transform, if the input has more vertices than
`--geo-op-vertices`, or if the group is empty. The aggregate IRIs are not functions, so
`BIND(geof:aggUnion(?w) AS ?u)` is a syntax error. Without the `geo` feature they still
group, with an unbound value.

**Jena filter functions.** These use the `spatialF:` prefix
(`<http://jena.apache.org/function/spatial#>`) and Jena's argument forms. A unit,
datatype or CRS may be an IRI, an `xsd:anyURI` literal or a plain string.

| Function | Result |
|---|---|
| `convertLatLon(lat, lon)` | EPSG:4326 `POINT(lat lon)`. The arguments are numbers or numeric strings, with the latitude within ±90 and the longitude within ±180. |
| `convertLatLonBox(latMin, lonMin, latMax, lonMax)` | EPSG:4326 `POLYGON` |
| `equals(g1, g2)` | `geof:sfEquals` |
| `nearby(g1, g2, radius, unit)`, `withinCircle` | `xsd:boolean`: distance `<` radius |
| `distance(g1, g2, unit)` | `geof:distance` |
| `greatCircle(lat1, lon1, lat2, lon2, unit)` | `xsd:double` in a length unit, under the dataset's distance model. The model is geodesic by default, and `haversine` gives Jena's numbers. |
| `greatCircleGeom(g1, g2, unit)` | The same, between the closest points. Projected geometries are measured on WGS 84. |
| `angle(x1, y1, x2, y2)`, `angleDeg` | Direction clockwise from the y axis, in radians in [0, 2π) or in degrees. Degrees are rounded to 6 decimals, as in Jena. Jena's implementation is a quarter turn off south-east and north-west of the first point. Sparkles follows the documented meaning. |
| `azimuth(lat1, lon1, lat2, lon2)`, `azimuthDeg` | Initial great-circle bearing clockwise from north, in radians in [0, 2π) or in degrees. |
| `transform(g, datatype, crs)`, `transformDatatype(g, datatype)`, `transformSRS(g, crs)` | `g` in another datatype (`geo:wktLiteral`, `geo:geoJSONLiteral`), another CRS, or both. |

**UTM.** The 120 UTM zones on WGS 84 are built-in CRSs. They run from
`http://www.opengis.net/def/crs/EPSG/0/32601` to `…/32660` in the north and from
`…/32701` to `…/32760` in the south, with easting and northing in metres. The projection
is transverse Mercator with Krüger's series to the sixth order (Karney 2011), accurate to
well under a millimetre within a zone. Points more than 60° of longitude from the zone's
central meridian have no coordinates, and `transform` gives a type error for them.
Literals in a UTM CRS are indexed. Distances between them are Euclidean in metres.

**Conversion for maps.**

| Method | Path | Description |
|--------|------|-------------|
| POST | `/$/geo/convert` | Open to any caller, like `/$/format`. The body is `{"literals": [{"value": "POINT(2 3)", "datatype": "http://www.opengis.net/ont/geosparql#wktLiteral"}, …]}`, with at most 10,000 literals. Returns `200 {"results": [{"geometry": {…}} \| {"error": "…"}]}` in request order. Each result is the literal as an RFC 7946 geometry in CRS84 (longitude, latitude), or the reason it has none. EPSG:4326 coordinates are swapped and projected CRSs are transformed. An empty geometry becomes an empty `GeometryCollection`. Reasons are messages such as `malformed literal at offset N: …`, `unknown CRS <…>: no transform to CRS84` and `not a geometry literal datatype: <…>`. `400` for a body of another shape or too many literals. `501` without the `geo` feature. |

For a dataset named `convert`, this route takes the place of `GET /$/geo/{ds}`. That
dataset's index status is not available over HTTP (`405`). Its other `/$/geo/convert/…`
routes and `sparkles geo-index --loc DB --status` still work.

**Conformance.** Oxigraph's GeoSPARQL test suite runs with the W3C harness
(`cargo test -p sparkles --features geo --test w3c geosparql`). 37 of its 44 cases pass.
The 7 others are listed with the reason in
`testsuite/geosparql/oxigraph/expected-failures.txt`. They fail because EPSG:4326 is
supported with its latitude-first axes, and because unclosed polygon rings are malformed
literals.

### Spatial joins and nearest neighbours

**Spatial joins.** A FILTER conjunct can test two geometry variables bound by different
parts of a group, where the parts share no variable. Such a conjunct joins those parts on
the test instead of forming their cross product (`SpatialJoin` in EXPLAIN):

```sparql
SELECT ?state (COUNT(?p) AS ?n) WHERE {
  ?state a ex:State ; geo:hasDefaultGeometry/geo:asWKT ?sw .
  ?p a ex:Place ; geo:hasDefaultGeometry/geo:asWKT ?pw .
  FILTER(geof:sfContains(?sw, ?pw))
} GROUP BY ?state
```

The tests that join are the relations other than the disjoint ones, `relate` with a
pattern that needs an intersection, and `distance`/`metricDistance` below a constant. The
bound can be written with `<` or `<=`, or with the bound first and `>` or `>=`.

A part that is a single pattern `?x <indexed predicate> ?w` is searched in the spatial
index when the index is ready. When the other part is small next to it, the index is
probed once per geometry of the other part (`[index nested loop on <…>]`). Otherwise the
pattern's rows near the other part are read once. Any other part is planned as usual,
and its distinct geometries are packed into an R-tree for the query (`[tree join]`). This
works without an index too.

Each candidate pair is tested with the function itself. The answer, duplicates included,
is therefore the same as the cross product with the filter. Geometries in an unknown CRS
are tested against those of the same CRS. A relation with a literal the index does not
hold reads the pattern instead of searching the index. A disjointness test, a lower bound
on a distance, a pattern that holds without an intersection, or a non-constant bound
keeps the cross product and adds a `geo-not-joined` warning naming the reason. Candidate
pairs count against the query's row limit, and past it the query fails with `507`.

**Nearest neighbours.** Some top-k queries read the pattern nearest first (`SpatialKnn`
under the top-k). The query needs a `LIMIT` and an `ORDER BY` on
`ASC(geof:metricDistance(?w, C))`, on `geof:distance(?w, C, unit)` with a length unit, or
on a variable bound to one of them. `?w` must be the object of one pattern of an indexed
predicate, and the group's other patterns must connect to that pattern. The group then
runs over batches of the nearest rows until the `k`-th distance found is below the bound
of every row not read yet.

Rows whose distance is an error come first in SPARQL's order. Their value is not a
geometry, is malformed or empty, or is in a CRS without a transform. Without a
`FILTER(BOUND(?d))` or a bound on the distance in the group, the pattern's rows are
therefore also read once to find them:

```sparql
SELECT ?g ?d WHERE {
  ?g geo:asWKT ?w
  BIND(geof:metricDistance(?w, "POINT(9 1)"^^geo:wktLiteral) AS ?d)
  FILTER(BOUND(?d))
} ORDER BY ?d LIMIT 10
```

The answer is the same as the generic sort's, up to the choice among rows tied at the
`k`-th distance. These cases keep the generic sort and add a `geo-not-knn` warning: a
descending order, an angle unit, a constant that is empty or not in longitude and
latitude, an index that is not ready, and a group whose other patterns do not connect to
`?w`'s pattern.

Both operators report the counters of the other spatial operators. A join adds `pairs`,
the geometry pairs that passed, and `indexProbes`. Nearest neighbours adds `batches`, the
runs of the group, and `errorRows`. The two operators are the `spatial_join` and
`spatial_knn` optimizations, which are on by default (see `QueryOptions::optimizations`
and `SPARKLES_DISABLE_OPTIMIZATIONS`).

### Query rewrite, `spatial:equals` and RDFS entailment

**Query rewrite**, GeoSPARQL's Query Rewrite Extension, is off by default. Switch it on
per dataset with `"queryRewrite": true` in the spatial index configuration
(`PUT /$/geo/{ds}`). `sparkles serve --no-geo-rewrite` switches it off for the whole
server, whatever the datasets say. `GET /$/geo/{ds}` shows the effective value. With
rewrite on, a triple pattern whose predicate is one of the 24 topological properties
(`geo:sfWithin`, `geo:ehMeet`, `geo:rcc8po`, …) matches the asserted triples and the
derived ones as one set:

```sparql
SELECT ?x WHERE { ?x geo:sfContains ex:g1 }   # features and geometries containing ex:g1
```

* `so1 geo:R so2` is derived when some geometry literal of `so1` and some geometry
  literal of `so2` satisfy `geof:R`, computed exactly as the function computes it. A
  feature's literals are those of its `geo:hasDefaultGeometry`, not its
  `geo:hasGeometry`. A geometry's literals are its serializations, through the index's
  `predicates`. A geometry literal written in the query stands for itself. A feature
  therefore relates to its own geometry, a point contains itself, and
  `?x geo:sfWithin ex:region` returns features and geometries alike. Add
  `?x a geo:Feature` to keep only the features.
* Variables bind features and geometries, never literals. The literals considered are
  those the spatial index covers: the configured predicates, in graphs of its scope.
  Literals the index leaves out still count where the function says so. An empty geometry
  is `sfDisjoint` from everything, and two literals in the same unknown CRS can be equal.
* Under `GRAPH ?g`, both ends' serializations and the feature links must be in the graph
  that `?g` binds. In a merged default graph (`reasoning=true`, `default-graph-uri`) they
  may be in any of its graphs. A predicate variable (`ex:a ?p ex:b`) matches asserted
  triples only.
* With one constant end, the index is searched around each of the constant's literals.
  While the index is not ready, every literal is read instead, with the same answers.
  With two variable ends, every literal is paired with those whose envelope meets it. The
  disjoint relations (`sfDisjoint`, `ehDisjoint`, `rcc8dc`) test every pair, within the
  query's row limit, and return `507` beyond it.
* EXPLAIN shows `SpatialRelate ?x geo:sfContains <…g1> [asserted ∪ derived]` with the
  spatial counters, plus `asserted` (asserted triples) and `pairs` (literal pairs that
  hold).

**`spatial:equals`** comes from Jena and is always available, with or without query
rewrite or an index. `?f spatial:equals ex:A` derives `sfEquals` between features,
geometries and geometry literals in the same way, and never matches asserted triples.

**RDFS entailment** of the GeoSPARQL vocabulary comes from
`sparkles infer --loc DB --profile rdfs --vocab geosparql`, or from `POST /$/reason/{ds}`
with `"vocabularies": ["geosparql"]`. It adds the GeoSPARQL 1.1 and Simple Features class
and property axioms to the profile's rules. These include
`sf:Polygon ⊑ sf:Surface ⊑ sf:Geometry ⊑ geo:Geometry`,
`geo:asWKT ⊑ geo:hasSerialization`, `geo:hasDefaultGeometry ⊑ geo:hasGeometry`, and the
domains and ranges of the feature, geometry and topological properties. The axioms were
written for Sparkles from the standard, and no OGC file is shipped. The axioms, and what
the rules derive from them, go to `urn:x-sparkles:inferred`, never to the data's graphs.
Queries see them with `reasoning=true`, like other inferences:

```sparql
SELECT ?g WHERE { ?g a geo:Geometry }          # ex:gA, given ex:gA a sf:Polygon
```

**Default geometries.** Query rewrite follows `geo:hasDefaultGeometry` only.
`sparkles infer --geo-default-geometry` (`"geoDefaultGeometry": true`) materializes
`F geo:hasDefaultGeometry G` for every feature `F` that has exactly one `geo:hasGeometry`
(`G`) and no `geo:hasDefaultGeometry`, like Jena's `applyDefaultGeometry`. It runs with
the profile, which is `rdfs` unless another is given, and the profile's rules see these
triples. The triples go to the inferred graph, so a re-run recomputes them and clearing
the inferences removes them. The reasoning status records both options (`vocabularies`,
`geoDefaultGeometry`), and manual and automatic re-runs repeat them.

### Maps in the web UI

The UI draws geometries with MapLibre GL JS, which it loads when a map first opens. Maps
appear in three places:

* The Map tab of query results. Literals in CRS84, EPSG:4326 and Web Mercator are read in
  the browser, and others go through `POST /$/geo/convert`.
* The explorer's map card, with Nearby (`spatial:nearbyGeom`).
* The map of the Spatial index panel, which calls `GET /{ds}/geo` for the box in view.

The basemap is the UI's own Natural Earth 1:110m land, coastlines and boundaries (public
domain), so the maps work offline and contact nothing else.

`serve --map-style-url URL` replaces the basemap with a MapLibre style JSON at an absolute
`http(s)` URL. `GET /$/server` reports it as `mapStyleUrl`, which is `null` without the
flag. The pages' Content Security Policy adds the URL's origin to `connect-src` and
`img-src`, and to the map worker's `connect-src`. The style's tiles, glyphs and sprites
must therefore come from that same origin. Scripts stay the UI's own. Attribution, when
the style needs it, comes from the style.

## Reasoning status and diagnostics

The design and its rationale are in [C08 Inference freshness and diagnostics](specs/C08-inference-freshness.md).

Materialized inferences (`urn:x-sparkles:inferred`) are not maintained incrementally. A
materialization records the dataset id and the commit it wrote. When it changed nothing,
it records the head it read instead. Any later commit makes the inferences **stale**,
including commits that only touch named graphs the reasoner does not read. Compaction and
restarts do not. A status written by an older version, or recorded for another dataset
id, has unknown freshness (`stale: null`).

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
  vocabularies?: string[];     // built-in vocabularies added to the profile ("geosparql")
  geoDefaultGeometry?: true;   // default geometries were materialized
};
```

**Header.** A query or SHACL validation that includes the inferred graph while the
inferences are not fresh at the snapshot it read carries a `Sparkles-Inferences` header.
Its value is `stale; commits-since=3`, `stale` when the count is unknown, or `unknown`.
Fresh inferences send no header. The body is unchanged. The header is exposed to
cross-origin clients.

**Automatic re-runs** are off by default.
`sparkles serve --auto-reason SECS [--auto-reason-max-delay SECS]` re-runs the recorded
profile once a dataset with stale inferences has had no commit for `SECS` seconds. While
writes continue, it runs at the latest after the maximum delay, which defaults to
12 × `SECS`. The messages of these tasks start with `auto:`. After a failed run, the next
attempt waits for the next commit. Runs never start for unknown freshness, nor on
`--read-only` servers. Each run is a full recomputation that holds the dataset's writer
lock, so updates wait while it runs.

**Diagnostics.** `GET /$/reason/{ds}/diagnostics` runs a fixed set of checks taken from
the OWL 2 RL rules whose conclusion is `false` (OWL 2 Profiles §4.3). Each check is one
SPARQL query over the default graph, plus the inferences when they are included. Those
rules are sound, so every finding is a genuine inconsistency. Finding nothing does not
establish OWL consistency.

| Param | Default | Meaning |
|---|---|---|
| `checks` | all | Comma-separated check ids. |
| `limit` | 100 (1–10000) | Findings per check. |
| `reasoning` | `true` if inferences exist | Includes `urn:x-sparkles:inferred`. |
| `closure` | `subclass` | `subclass` makes type tests follow `rdfs:subClassOf*`. `none` uses stated types only. |
| `timeout` | server query timeout | Time budget for the whole report. |

| Check | Rules | Severity | Query |
|---|---|---|---|
| `nothing-member` | `cls-nothing2` (+`cax-sco`) | inconsistency | [nothing-member.rq](../crates/sparkles-reasoner/diagnostics/nothing-member.rq) |
| `disjoint-classes` | `cax-dw` | inconsistency | [disjoint-classes.rq](../crates/sparkles-reasoner/diagnostics/disjoint-classes.rq) |
| `all-disjoint-classes` | `cax-adc` | inconsistency | [all-disjoint-classes.rq](../crates/sparkles-reasoner/diagnostics/all-disjoint-classes.rq) |
| `same-different` | `eq-diff1` (+`eq-ref`, `eq-sym`, `eq-trans`) | inconsistency | [same-different.rq](../crates/sparkles-reasoner/diagnostics/same-different.rq) |
| `functional-literal-conflict` | `prp-fp`, `dt-diff`, `eq-diff1` | inconsistency | [functional-literal-conflict.rq](../crates/sparkles-reasoner/diagnostics/functional-literal-conflict.rq) |
| `thing-empty` | `thing-nonempty`: the domain is never empty | inconsistency | [thing-empty.rq](../crates/sparkles-reasoner/diagnostics/thing-empty.rq) |
| `unsatisfiable-class` | `lint`: a class below `owl:Nothing` without members | warning | [unsatisfiable-class.rq](../crates/sparkles-reasoner/diagnostics/unsatisfiable-class.rq) |

There is no unique name assumption. Two IRIs count as different individuals only through
`owl:differentFrom`. Literal values of a functional property are compared with SPARQL
`!=`, restricted to numbers, strings, language-tagged strings and booleans, so a pair it
cannot compare is never reported. With inferences included, each finding is re-checked
with the same bindings over the asserted data alone. `basis` is `asserted` when the
finding holds there and `uses-inferences` otherwise. With stale inferences, only
`asserted` findings are certain for the current data.

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

`status` is `violations-found` when an inconsistency check has findings. Otherwise it is
`incomplete` when a check timed out or failed, and `none-found` when none did. Warnings
never count. A timeout marks the remaining checks `timeout`, and the request does not
fail with `408`. Errors are `400` for an unknown check id or a bad `limit` or `closure`,
`404` for an unknown dataset, and `501` without the `reasoning` feature. Diagnostics are
read-only and also work on `--read-only` servers.

In the CLI, `sparkles infer --loc DB --status` prints the status.
`sparkles infer --loc DB --check [--checks a,b] [--limit N] [--no-inferences] [--closure subclass|none] [--format text|json]`
runs the checks, after materializing when `--profile` or `--rules` is given. It exits
with 0 (`none-found`), 1 (`violations-found`) or 2 (`incomplete` or an error).
`sparkles stats` shows a `reasoning` line.

## Write-time validation

The design and its rationale are in [C10 Write-time SHACL validation](specs/C10-write-time-validation.md). The ShEx guard is described in [G02 ShEx](specs/G02-shex.md).

A dataset can validate every write against SHACL shapes or a ShEx schema before it
commits. It uses one language at a time. The configuration lives in the database
directory as `validation.json`, format 2. SHACL configurations from older Sparkles
versions, format 1 without `language`, are still read:

```json
{ "format": 2, "language": "shacl", "mode": "reject", "shapes": { "graphs": ["urn:x-shapes:main"] },
  "dataGraph": "default", "includeInferences": false,
  "threshold": "violation", "timeoutSeconds": 10, "reportLimit": 100 }
```

| Field | Values | Default | Meaning |
|---|---|---|---|
| `language` | `shacl`, `shex` | `shacl` | The shape language. It is always written, and a `PUT` without it means SHACL. |
| `mode` | `reject`, `warn`, `off` | — | With `reject`, a write that leaves results at or above the threshold is not committed (`422`). With `warn`, the write commits, and the receipt and header report the findings. |
| `shapes` (SHACL) | `{ "graphs": [iri, …] }` or `{ "inline": "<turtle>", "format"?: media type }` | — | Named graphs of the dataset, or shapes given inline. Named graphs are read from the state being validated, so changes to them are validated too and must parse. Inline shapes are copied to `validation-shapes.ttl`. |
| `dataGraph` | `"default"`, `"union"`, `[iri, …]` | `"default"` | The data graph. It never includes the shapes graphs, and includes the inferred graph only with `includeInferences`. |
| `threshold` (SHACL) | `violation`, `warning`, `info` | `violation` | Results at or above it block. |
| `timeoutSeconds`, `reportLimit` | number, 1–10000 | 10, 100 | `timeoutSeconds` is the time budget per write. Exceeding it fails the write with `408`. `reportLimit` is the number of results a report carries. |

**ShEx.** A ShEx configuration names a schema and a query shape map instead of shapes:

```json
{ "format": 2, "language": "shex", "mode": "reject",
  "schema": { "file": "validation-schema.shex", "format": "shexc", "sha256": "…" },
  "shapeMap": "{FOCUS a ex:Person}@ex:Person, {FOCUS a ex:Org}@ex:Org",
  "dataGraph": "default", "includeInferences": false, "timeoutSeconds": 10, "reportLimit": 100 }
```

| Field | Values | Meaning |
|---|---|---|
| `schema` | `PUT`: `{ "inline": "<schema>", "format"?: "shexc" \| "shexj" \| "shexr", "base"?: iri, "source"?: text }` | The schema text, and the base its relative IRIs resolve against. The text is ShExC, ShExJ, or ShExR in Turtle. Without `format` the language is sniffed, and text that starts with `{` is ShExJ. The schema is copied into the database. Without imports, it is stored verbatim as `validation-schema.shex` (ShExC) or `validation-schema.json` (ShExJ). With imports, the imports are resolved during the `PUT` and the merged schema is written as ShExJ to `validation-schema.json`. The schema's prefixes are then kept in `schema.prefixes` for the shape map. The stored configuration names the copy (`file`, `format`), keeps `base` and `source`, and records the SHA-256 of the text given. A later write never fetches anything. To change the other fields, a `PUT` may send the stored `schema` back, with its `file` and without `inline`. |
| `shapeMap` | compact string, or the JSON form `[{ "node", "shape" }]` | A query map. It is expanded again on every validated state, so new focus nodes are picked up. Prefixed names use the schema's prefixes unless the map has its own `PREFIX`es. |
| `threshold` | — | Not accepted. Every nonconformant association blocks. |

Imports resolve as for `POST /{ds}/shex`. `file:` IRIs and relative IRIs must be inside
`--load-dir`, and http(s) imports go through the outbound policy. A `PUT` takes no inline
import bodies or externs, so an EXTERNAL shape is a `400`. Labels the schema does not
define, START without a start shape, and SPARQL node selectors are `400` too. Node
selectors are refused because every validated write would run them.

| Method | Path | Description |
|---|---|---|
| GET | `/$/validation/{ds}` | `{ language, config, status }`, or `{ config: null }`. `status` holds the mode, the shape count, the baseline of the last commit, counters and warnings. ShEx adds `associations`, the size of the last expanded map. |
| PUT | `/$/validation/{ds}` | Sets the configuration, in either language, and replaces the other language's files. The current data is validated under the writer lock. A `reject` configuration on data that does not pass is refused with `409` and the summary. `400` for a bad configuration, or for shapes or a schema that cannot be used. `501` for a language this binary was built without. |
| DELETE | `/$/validation/{ds}` | Turns validation off (`204`) and removes every validation file. |

**Writes** are validated once per request, on the final state, before any byte is
written. This covers updates, Graph Store PUT/POST/DELETE and uploads, and in the CLI
`load`, `update`, `infer` and bulk loads. A write that touches neither the data graph nor
the shapes graphs is skipped. ShEx also skips a write when every quad it changes has a
predicate that no triple constraint and no `{FOCUS p …}` selector mentions, because a
neighbourhood holds only the arcs of the predicates its shape mentions. It does not skip
the write when a shape is `CLOSED`. Responses carry
`Sparkles-Validation: status=passed|warned|rejected|skipped|bypassed, mode=…, strategy=full|none, blocking=N, total=N, violations=N, warnings=N, infos=N, ms=N`.
ShEx adds `lang=shex` after `strategy`. Receipts (`receipt=true`) include a `validation`
object with its `language`. A rejection is `422 Unprocessable Content`:

```json
{ "error": "SHACL validation failed: 2 blocking results (threshold violation); nothing was committed",
  "validation": { "language": "shacl", "status": "rejected", "blocking": 2, "total": 3, "limit": 100,
                  "truncated": false, "results": [ … ], "head": 41, "kind": "update" } }
```

When the request's `Accept` names `text/turtle`, the rejection is a Turtle
`sh:ValidationReport` instead. A ShEx rejection counts associations. `total` is the size
of the expanded map, and `blocking` (and `bySeverity.violation`) counts the nonconformant
ones. `threshold` is `violation`, and `results` holds the first `limit` nonconformant
[ShEx result objects](#shex-validation) in map order. ShEx has no Turtle report, so its
rejection is JSON whatever the `Accept`:

```json
{ "error": "ShEx validation failed: 1 nonconformant association; nothing was committed",
  "validation": { "language": "shex", "status": "rejected", "strategy": "full", "blocking": 1, "total": 4,
                  "results": [ { "node": { "type": "uri", "value": "http://ex.org/dave" },
                                 "shape": { "type": "uri", "value": "http://ex.org/Person" },
                                 "status": "nonconformant", "reason": "…", "appinfo": { "failures": [ … ] } } ],
                  … } }
```

A rejected write uses no commit number. `?validationLimit=N` bounds the results of one
request. `?validate=false`, or the header `Sparkles-Validate: off`, skips validation, but
only on a server started with `--allow-unvalidated-writes`. On other servers it gets
`403`. The CLI has `--no-validate`. A dataset whose `validation.json` cannot be loaded
refuses writes with `501` rather than accepting them unvalidated. This happens, for
example, when a binary built without `shex` opens a ShEx configuration.

In the CLI, `sparkles validation` sets the configuration. For SHACL it is
`sparkles validation --loc DB --mode reject|warn (--shapes-graph IRI … | --shapes FILE) [--data-graph …] [--threshold …]`.
For ShEx it is
`sparkles validation --loc DB [--lang shex] --schema FILE [--schema-format shexc|shexj|shexr] --shape-map MAP --mode reject|warn [--data-graph …]`.
`--schema` and `--shape-map` imply `--lang shex`, and imports resolve against the schema's
directory. `--status [--format json]` shows the configuration, and `--off` turns
validation off.

A write rejected in the CLI exits with status 3, and ShEx lists
`  <node> @ <shape>: <reason>`. `load`, `update` and `infer` end their summary line with
the validation status. `sparkles stats` shows the configuration, as
`validation      reject · 1 shape graph · 20 shapes` or
`reject · ShEx · 3 shapes · last full 12 ms`. A reasoning task whose inferences are
rejected fails with
`inferences rejected by SHACL validation: N blocking results (first: <shape> at <node>)`.
Rejections are logged at INFO under `sparkles::validation`, with the dataset, language,
kind, counts, and first shape and focus node. The counters, with a `language` label, are
listed under [Metrics](#metrics). Each validated write runs a full validation of the data
graph, which takes about 160 ms at 1M triples for SHACL. Skipped writes cost nothing.

## SHACL validation

`POST /{ds}/shacl?graph=default|union|<iri>` validates a data graph of the dataset against
the shapes graph in the request body, with Fuseki's semantics:

* **Body.** The body is the shapes graph. `Content-Type` selects the syntax:
  `application/n-triples`, `application/rdf+xml`, `application/ld+json`,
  `application/trig` or `application/n-quads`. All graphs of a quad format are merged.
  Turtle is used for `text/turtle` and for any other or absent content type, such as
  curl's default `application/x-www-form-urlencoded`.
* **`graph`.** `default` is the default and means the dataset's default graph. With
  `--union-default-graph`, that is the union of all graphs. `union` means all graphs
  (`urn:x-arq:UnionGraph`). A graph IRI selects that graph, or returns `404` if the graph
  does not exist. Jena's special IRIs `urn:x-arq:DefaultGraph` and `urn:x-arq:UnionGraph`
  are accepted as well.
* **`reasoning=true|false`.** When the dataset has materialized inferences, validation
  runs over data ∪ `urn:x-sparkles:inferred` unless `reasoning=false`. With `false`, the
  inferred graph is also left out of `graph=union`.
* **`timeout=<seconds>`.** Works as for queries, with the server default otherwise. A
  timeout returns `408`.
* SHACL Core and SHACL-SPARQL are supported. A parse error in the shapes graph is a `400`.
* **Budgets.** The report is bounded like a query result. It fails with `507` and
  `budget: "result-bytes"` once it holds more results than fit in `--max-result-mb` at 48
  bytes each, or in `--query-memory-mb` at an estimated 512 bytes each. It also fails once
  its serialized form is larger than `--max-result-mb`. Validations run in a pool of half
  the cores, shared by all `/{ds}/shacl` requests, and stop when their client disconnects.

The response is the validation report, with `200` whether or not the data conforms. It is
negotiated with `Accept` or `format=`:

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
[--format ttl|json|text|nt|jsonld|rdfxml] [--no-inferences]`, or `--data FILE…` in place
of `--loc` to validate files in memory. Like Jena's `shacl validate`, it exits with status
1 when the data does not conform.

## ShEx validation

The design and its rationale are in [G02 ShEx 2.1 validation](specs/G02-shex.md).

`POST /{ds}/shex` validates nodes of a data graph against a ShEx 2.1 schema (Shape
Expressions). Fuseki has no ShEx operation, so the parameters follow `/{ds}/shacl` where
they overlap. The endpoint needs the `shex` cargo feature, which is on by default, and
returns `501` without it.

**Request.** The schema can be the body, with the shape map in the query string, or a
JSON envelope can carry both.

* **Schema as the body.** The `Content-Type` gives the schema's syntax.
  * `text/shex` is ShExC.
  * `application/shex+json` is ShExJ, and so are `application/json` and
    `application/ld+json` when the body is a ShExJ `Schema` object.
  * `text/turtle`, `application/n-triples`, `application/trig`, `application/n-quads` and
    `application/rdf+xml` are ShExR, the schema as RDF in the ShEx vocabulary
    `http://www.w3.org/ns/shex#`. The triples of every graph are read as one graph, and a
    Turtle or TriG body's prefixes become the schema's.
  * Any other content type, such as `text/plain` or a form type, is sniffed. The body is
    ShExJ when it starts with `{` and ShExC otherwise.

  `schema-format=shexc|shexj|shexr` names the syntax where the media type does not. ShExR
  is then read in the RDF syntax of the media type, or as Turtle. The shape map is
  `map=<compact shape map>`, or `node=<term>` with `shape=<label>`. Without `shape`, the
  label is `START`. `base=<iri>` resolves relative IRIs of the schema.
* **JSON envelope.** Send `Content-Type: application/json` with a body that is not a ShExJ
  schema:

  ```json
  { "schema": "PREFIX ex: <http://ex.org/> ex:S { ex:name . }",
    "schemaFormat": "shexc",
    "map": "{FOCUS a ex:Person}@ex:S",
    "externs": "ex:Ext { … }",
    "imports": { "http://ex.org/common": "<ShExC or ShExJ text>" },
    "base": "http://ex.org/schema" }
  ```

  `schemaFormat` is `shexc`, `shexj` or `shexr` (Turtle). Without it, the schema is
  sniffed as ShExC or ShExJ. `map` is a compact shape map (a string) or a JSON shape map
  (an array). `externs` defines the schema's `EXTERNAL` shapes. `imports` gives the bodies
  of `IMPORT`ed IRIs. Only `schema` is required. The shape map comes from the envelope or
  the query string, not both. Unknown keys are an error.

**Shape maps.** The compact syntax is that of the ShapeMap draft. Sparkles adds Jena's
`BASE`/`PREFIX` directives, commas between associations, a trailing `.`, and `a` for
`rdf:type`. Without directives, prefixed names use the schema's prefixes. A node is an
IRI, a prefixed name, a literal, or a blank node as Sparkles prints it in query results
(`_:b1f`). `{FOCUS p o}`, `{FOCUS p _}`, `{s p FOCUS}` and `{_ p FOCUS}` select the nodes
of the data graph that have those arcs. The JSON syntax is an array of
`{"node": …, "shape": …}`, and the draft's `nodeSelector` and `shapeLabel` are accepted
too. A node that is not in the data graph is validated with no arcs.

`SPARQL """SELECT …"""`, with any of the four string quotes, selects the bindings of
`?focus`, or of the first projected variable, in the order of the solutions. Unbound
values are skipped, and values the store does not hold are validated with no arcs. This
selector is an extension taken from other ShEx tools and is not part of the ShapeMap
draft.

The query is checked when the map is parsed. It must be a SELECT query that projects a
variable and has no `SERVICE`. Otherwise the request fails with `400` and the `line` and
`column` of the selector. The query runs on the data graph. Its default graph is the data
graph (`graph`, with the inferences when `reasoning` includes them), and `FROM`,
`FROM NAMED` and `GRAPH` see nothing else. It has only its own prefixes and base IRI, not
the map's or the schema's. It runs under the row and memory budgets of a query
(`--max-rows`, `--query-memory-mb`, `507` past them) and under the validation's
`timeout`, which covers the selectors and the validation together. The selected nodes
count against the report's size along with the other associations, and are deduplicated
with them per (node, shape).

**Imports** (`IMPORT <iri>`) are resolved in this order: from the envelope's `imports`,
then as `file:` IRIs under `--load-dir` (none without it), then as http(s) IRIs through
the `--outbound-*` policy of SPARQL `LOAD`. An IRI that does not resolve as given is tried
with `.shex` appended, then with `.json`. One validation reads at most 64 schemas and
16 MiB of imports, and its http(s) imports share the `outbound-bytes` budget of one
request.

**Semantic actions.** Actions of the Test extension (`http://shex.io/extensions/Test/`,
`fail` and `print`) run. Actions of other extensions are skipped, with a warning in the
report. `semact-trace=true` adds the Test extension's `print` output to each result's
`appinfo`.

| param | values | default |
|---|---|---|
| `graph` | `default`, `union` or a graph IRI. `urn:x-arq:DefaultGraph` and `urn:x-arq:UnionGraph` work too. `404` if the graph does not exist. | `default` |
| `reasoning` | `true` / `false`. Whether to merge `urn:x-sparkles:inferred` into the data graph. | `true` when the dataset has inferences |
| `results` | `all` / `nonconformant`. The counts always cover all associations. | `all` |
| `format` | `json`, `shapemap`, `smap` or `text`. Without it, `Accept` decides (`application/json`, `text/plain`). | `json` |
| `timeout` | Seconds, as for queries. `408` past it. | the server's |
| `semact-trace` | `true` / `false` | `false` |
| `stats` | `true` adds the typing's counters to the JSON report. | `false` |
| `base` | The base IRI of the schema. | none |

**Response.** The status is `200` whether or not the nodes conform, and the response
carries `Sparkles-Commit`. When inferences were included, it also carries the inference
headers of `/{ds}/shacl`:

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

The other formats:

* `format=shapemap` is the ShapeMap draft's JSON result map
  (`[{node, shape, status, reason?, appinfo?}]`), with compact-syntax strings.
* `format=smap` is the compact result map, with one `<node>@<shape>` (conformant) or
  `<node>@!<shape>` (nonconformant) per line.
* `format=text` is Jena's report: `OK`, or one
  `<n> @ <S> :: Focus = <n>, Status = nonconformant, Reason = …` line per association.

**Errors.**

* `400` with `line` and `column`: a syntax error in the schema, the shape map, the
  externs or an inline import. ShEx 2.2 syntax is reported the same way.
* `400` without them: a ShExR schema whose graph is not a schema, for example one with no
  `sx:Schema` node, a missing `sx:predicate`, or a value of the wrong kind. The message
  names the node.
* `400`: a schema that cannot be used, a shape label the schema does not define, `START`
  without a start shape, or invalid parameters. A schema cannot be used when it has an
  undefined reference, a negated reference cycle, an invalid `&include`, an import that
  does not resolve or is not allowed, or an `EXTERNAL` shape without a definition.
* `404`: a missing graph.
* `408`: a timeout.
* `413`: a body over `--max-query-body-mb`.
* `507` with a `budget`. `"result-bytes"` is a report over `--max-result-mb`, as for
  `/{ds}/shacl`. `"validation-work"` means the partition or pair budget ran out.
  `"outbound-bytes"` means the imports exceeded the request's outbound budget (see
  [Budgets](#budgets)).

The CLI equivalent is `sparkles shex validate (--loc DB | --data FILE…) --schema FILE
[--schema-format shexc|shexj|shexr] (--map FILE | --shape-map 'MAP' | --node TERM [--shape
LABEL]) [--graph default|union|IRI]
[--no-inferences] [--externs FILE] [--format text|json|shapemap|smap] [--only-nonconformant]
[--timeout S] [--semact-trace] [--stats]`. Jena's flag names work as aliases: `val` and
`v`, `--shapes`/`-s`, `--datafile`/`-d`, `--shapesMap`/`-m` and `--target`/`-n`. Imports
resolve against the schema file's directory. The command prints Jena's text report by
default. It exits with 0 when every association conforms, with 1 when one does not or on
a timeout or budget error, and with 2 for usage, parse and schema errors.

`sparkles shex parse FILE… [--in shexc|shexj|shexr] [--out shexc|shexj|shexr|text] [--base IRI]`
prints schemas as ShExC, ShExJ, ShExR (Turtle) or a structural dump. Schema files are read
by extension. `.json` and `.shexj` files are ShExJ. `.ttl`, `.nt`, `.nq`, `.trig`, `.rdf`,
`.owl` and `.n3` files are ShExR in that RDF syntax. Other files are sniffed as ShExC or
ShExJ. `--in`/`--schema-format` name the syntax of stdin (`-`) or of a file whose name
does not say. SPARQL selectors on the command line have no row or memory budgets.

## Formatting

The design and its rationale are in [X02 Formatter](specs/X02-formatter.md).

`POST /$/format` formats a SPARQL query or update, or a Turtle, TriG, N-Triples, N-Quads
or JSON-LD document. It returns the formatted text in the style of `sparkles fmt` (see
[USAGE.md](USAGE.md#formatting)). It reads no dataset and no config file. The style
options come with the request, and omitted ones take their defaults. The formatter checks
its own output before answering. The output must parse to the same SPARQL algebra, RDF
dataset or JSON as the input, keep every comment, and format to itself. When a check
fails, the request fails and nothing is returned.

When the UI is built with the formatter's WebAssembly module, it formats in the page
instead. `mise run ui:wasm` builds the module, and the Nix packages always include it. The
module takes this endpoint's JSON and answers in the same shape. The UI calls
`POST /$/format` only as a fallback, when the build has no module or the module fails to
load or run. `--format-endpoint authenticated` or `off` therefore does not stop such a UI
from formatting. It limits only the endpoint.

The MCP server offers the same formatter as its `format` tool, with the same options
(see [MCP server](#mcp-server)).

**JSON body.** The UI sends `Content-Type: application/json` with this body:

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
  alignValues?: boolean;         // default false; SPARQL: pad multi-variable VALUES rows into columns
  prunePrefixes?: boolean;       // default false; SPARQL, Turtle, TriG: drop prefix declarations nothing uses
  directiveStyle?: "sparql" | "turtle";         // default "sparql"; Turtle, TriG: PREFIX and GRAPH, or @prefix
  turtleLayout?: "diff" | "conventional";       // default "diff"; Turtle, TriG
  sort?: boolean;                // default false; Turtle, TriG, JSON-LD terms, N-Triples, N-Quads
  // a key that does not act on the language is accepted and has no effect
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

Without `language`, the text decides. Its first significant token after comments and a
`PREFIX`/`BASE`/`VERSION` prologue sets the language. For example, `SELECT` or `INSERT`
means SPARQL. N-Triples is also valid Turtle, so it needs `language` or its media type.
The cursor stays next to the same token. A byte order mark is dropped.

**Raw body.** This is the form to use with curl. The body is the document, and its media
type names the language unless `language` is in the query string. The answer is `200`
with the same media type and the formatted text as the body. It carries
`Sparkles-Format-Changed: true|false`, which CORS exposes to browsers.

| `Content-Type` | Language |
|---|---|
| `application/sparql-query`, `application/sparql-update` | `sparql` |
| `text/turtle`, `application/trig`, `application/n-triples`, `application/n-quads`, `application/ld+json` | `turtle`, `trig`, `ntriples`, `nquads`, `jsonld` |
| `text/plain` | needs `?language=` |

```sh
curl -s --data-binary @q.rq -H 'Content-Type: application/sparql-query' \
  'localhost:3030/$/format?lineWidth=80&operatorPosition=trailing'
```

**Query-string options.** Every option can also be a query parameter of either body form,
with the same camelCase name (`lineWidth=80`, `typeShorthand=false`). Each
`prefixGroup=rdf,rdfs,xsd,owl` is one group. The parameter can repeat, and the groups keep
their order. `""` is the empty prefix. Options in a JSON body win over those in the query
string, and both are checked.

**Errors.** Errors are JSON, with `requestId` as everywhere.

| Status | Body | When |
|---|---|---|
| `400` | `{error, detail?, line, column, code: "syntax", language}` | The input does not parse. `error` reads `SPARQL syntax error at line L, column C: …` with the head of the parser's message. The line is 1-based, and the column counts characters. `detail` holds the whole message when it was cut. |
| `400` | `{error, code: "bad-request", option?}` | A bad option, an unknown `language`, a body that is not a JSON object with `text`, a non-UTF-8 raw body, `text/plain` without `language`, a language that cannot be detected, or a `cursorOffset` past the end of the text. For a bad option, `option` names it. An option is bad when its name is unknown, its type is wrong, its value is out of range or not one of the choices, or a prefix label is in two groups. |
| `401` | `{error}` | An anonymous caller under `--format-endpoint authenticated`. |
| `404` | `{error}` | `--format-endpoint off`. |
| `408` | `{error}` | The request took longer than `--format-timeout`, including the wait for a slot. |
| `413` | `{error}` | A body larger than `--format-max-mb`. |
| `415` | `{error}` | RDF/XML, or another unsupported media type. RDF/XML (`application/rdf+xml` or `language=rdfxml`) gets "RDF/XML formatting is not supported; convert to Turtle to format". |
| `422` | `{error, code}` | The formatter refused its own output. The `code` is `unsafe-format` (`algebra differs`, `comment lost`), `unstable-format` (`not idempotent`), or `unsupported-syntax`. `unsupported-syntax` means the reference parser accepts a construct the formatter cannot handle yet. The refusal is logged at `warn` with the SHA-256 of the input, never the text. Please report it. |

**Server settings and access.**

| `sparkles serve` flag | Default | |
|---|---|---|
| `--format-endpoint on\|authenticated\|off` | `on` | Who may format. `on` admits every caller the server admits, including anonymous ones when the server admits them. `authenticated` admits every caller but the anonymous principal, which gets `401`. `off` admits nobody (`404`). |
| `--format-max-mb N` | `16` | The largest request body. `0` means unlimited. |
| `--format-timeout S` | `10` | Seconds a request may take, including the wait for a slot. Formatting runs on one slot per core. |

With authentication, the route accepts any caller and needs no dataset permission, like
`/$/server`. Cookie sessions send the CSRF header, as for every other `POST`. Rate limits
count the route in the `query` class.

## `application/x-sparkles+json` (UI result format)

This result format is modelled on QLever's `qlever-json`. The UI uses it to render results
and draw query plans:

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

`GET|POST /{ds}/explain?query=…` returns `{ "algebra": string /* SSE */, "plan": PlanNode }`.
The plan is not executed, so `actualRows` is -1. The root `PlanNode` lists `warnings` when
something in the query did not run the way it reads, although the answer is the same:

* `geo-not-pushed`: a spatial FILTER is evaluated row by row. The warning says why.
* `geo-index-building`: the spatial index is being built, and plans run without it.
* `geo-not-built`: the query uses `geof:` functions in a build without the `geo` feature.

## Compression

The design and its rationale are in [X01 Compression codecs](specs/X01-compression-codecs.md).

**Responses** are compressed when the client sends `Accept-Encoding` with `zstd`, `br`,
`gzip` or `deflate`. This includes streamed bodies. Bodies under 256 bytes and images are
sent uncompressed. The UI's larger assets are built with brotli and gzip copies. Those
copies are served as they are, with `Vary: Accept-Encoding`, rather than compressed per
request.

| `sparkles serve` flag | Default | |
|---|---|---|
| `--http-compression auto\|off` | `auto` | |
| `--http-compression-level fastest\|default\|best\|N` | `default` | zstd 3, brotli 4, gzip 6. A number applies to whichever algorithm is chosen. |
| `--http-compression-algorithms` | `zstd,br,gzip,deflate` | The encodings offered. |
| `--max-decompressed-mb` | `65536` | Cap on a compressed request body or upload after decompression. `0` means none. |
| `--max-query-body-mb` | `16` | Largest body of a SPARQL query, `/{ds}/explain`, `/{ds}/shacl` or `/{ds}/shex` request. `0` means none. |
| `--max-update-body-mb` | `256` | Largest body of a SPARQL update. `0` means none. |
| `--max-admin-body-mb` | `16` | Largest body of an admin request (`/$/…`) or a `/{ds}/prefixes` change. `0` means none. |
| `--max-upload-mb` | `4096` | Largest Graph Store write or upload body, after HTTP decompression. `0` means none. |
| `--min-free-disk-mb` | `1024` | Free space that must remain in the temporary directory after a spooled request body, and on the data directory's file system after a commit, rebuild, clone or N-Quads backup. `0` turns the check off. |
| `--max-mem-dataset-mb` | `4096` | Largest in-memory (`dbType=mem`) dataset. A commit that would grow one past it fails with `507`. `0` means none. |

**Request bodies** of updates, queries, Graph Store PUT/POST and uploads may be sent with
`Content-Encoding: gzip`, `br`, `zstd` or `deflate`. Another encoding gets `415` with an
`Accept-Encoding` header naming the supported ones. RDF bodies and uploaded files are also
recognised as compressed by their first bytes (gzip, zstd, LZ4 frames). Uploads are also
recognised by file name (`.gz`, `.zst`, `.br`, `.lz4`). A body that decompresses past
`--max-decompressed-mb` fails with `413` and commits nothing.

**Body ceilings.** A body that is read whole has the ceiling of its request class:

* `--max-query-body-mb` for queries, `/{ds}/explain`, the shapes graph of `/{ds}/shacl`
  and the schema of `/{ds}/shex`
* `--max-update-body-mb` for updates
* `--max-admin-body-mb` for `/$/…` requests and prefix changes
* a fixed 64 KiB for `/$/auth/*`

The ceiling counts decompressed bytes and is checked while the body is read, so the server
never holds more than the ceiling. A declared `Content-Length` over it is refused before
anything is read. Past the ceiling, the request fails with `413`. A form POST to `/{ds}`
may hold either operation, so it is read up to the larger of the query and update
ceilings.

Graph Store PUT/POST, including through `/{ds}`, and `/{ds}/upload` are the bulk
endpoints. Their bodies stream to a temporary file instead, up to `--max-upload-mb`, or
the request fails with `413`. That limit counts bytes after HTTP decompression. Its
default of 4096 (4 GiB) is the body limit of the bundled NixOS nginx virtual host, and `0`
means unlimited. Files compressed inside the body are capped separately by
`--max-decompressed-mb` as they are parsed. Before each 64 MiB of a spooled body is
written to the temporary directory, the server checks that the file system keeps
`--min-free-disk-mb` free (default 1024, `0` for no check). Otherwise the request fails
with `507`.

**Storage.** A commit to a persistent dataset is refused with `507 {code: "storage-full"}`
when it would leave less than `--min-free-disk-mb` free on the data directory's file
system. Free space is measured with `statvfs` and cached for a second between small
commits. A rebuild (a large load or compaction) or a clone checks the free space while it
builds. Once the file system goes below the limit, it stops and removes what it wrote.
Nothing is committed in either case. An in-memory dataset (`dbType=mem`) holds at most
`--max-mem-dataset-mb` (default 4096), estimated from its index files, delta and
vocabulary. A commit that would grow it past that fails the same way, but deletes always
pass. Storage quotas per dataset do not exist yet.

**Files.** `sparkles load` reads the same codecs, chosen with
`--compression auto|none|gzip|zstd|brotli|lz4`. `auto` goes by magic bytes, then by the
extension. Brotli has no magic bytes, so it needs `.br` or `--compression brotli`. When a
file's name and its data disagree, the data wins and a warning is logged. An explicit
`--compression` that disagrees is an error. Compressed N-Triples and N-Quads are
decompressed and parsed as a stream, so they need neither an uncompressed copy nor memory
for their decompressed size. Other compressed formats are decompressed into memory first.

`sparkles load --lenient` skips the validation of IRIs and language tags. It is meant for
data whose IRIs are not all valid RFC 3987 IRIs; DBpedia, for example, has some that
contain U+FFFD. Syntax errors still fail the load.

`sparkles dump --out FILE` and `sparkles backup` take `--compress CODEC`, `--level N` and
`--threads N` (zstd). `sparkles backup` and `/$/backup` write zstd (level 3) by default,
which is about five times faster than gzip for a slightly larger file. `--compress gzip`
(`?compression=gzip`) gives `.nq.gz`, as Fuseki writes. `sparkles dump --out FILE` goes by
the file's extension, and writes uncompressed without one.

**Full-text documents** are stored with zstd (level 3). `"docstoreCompression": "lz4"`
or `"none"` in the text configuration picks another codec, and changing it rebuilds the
index. Indexes built before zstd was available keep LZ4 until they are rebuilt.

## Errors

Non-2xx responses carry `{ "error": string, "detail"?: string, "line"?: number, "column"?: number, "requestId": string }`.
`requestId` is the response's `X-Request-Id`, for finding the request in the logs. The
statuses are:

* `400` for parse errors, and for queries, updates and RDF bodies nested too deeply (see
  [Nesting limits](#nesting-limits))
* `401`/`403` for authentication and permissions
* `404` for an unknown dataset
* `405` for an update sent with GET
* `408` for a timeout
* `409` for a conflict
* `413` for a body over its ceiling, or a compressed body over `--max-decompressed-mb`
* `415` for an unsupported content type or `Content-Encoding`
* `429` for a request over a rate limit
* `503` for a cancelled query, a request over a concurrency limit (see
  [Rate limiting](#rate-limiting)), a failed write-ahead log write, or a dataset that an
  in-place restore is replacing. After a failed WAL write, writes are refused until
  restart while reads continue. During a restore the body has
  `{code: "dataset-restoring"}`, with `Retry-After: 5`.
* `500` otherwise

The backup routes add a machine-readable `code` (see
[Backup repositories](#backup-errors)).

### Nesting limits

The SPARQL parser recurses once for each level of nesting, and so do the planner, the
evaluator and the code that frees a parsed query. A query nested a few thousand levels
deep would overflow a thread's stack, and a stack overflow ends the whole server process.
The server refuses such requests with `400` before it parses them. The limits are fixed:

* A query or update nests at most 256 brackets deep. Every `(`, `[`, `{`, `<<`, `<<(` and
  `{|` counts, including the parentheses of a function call or a property path, and so
  does each unary `!`. The error reads `nested deeper than 256 levels`, with the line and
  column where the limit was passed.
* A query or update nests at most 1024 levels in its algebra. A chain is flat in the text
  but nested in the algebra, so each element of a chain counts as a level, added to the
  brackets around it. Chains are the operators of an expression such as
  `?a || ?b || …` or `1 + 2 + …`, the steps and operators of a property path, and the
  elements of a group: OPTIONAL, MINUS, UNION, FILTER, BIND, VALUES, a nested group, or a
  triple with a property path. The count is made on the text and errs on the high side,
  so a chain of a little under 1024 elements can already be refused. The error reads
  `nested deeper than 1024 levels, counting each operator of an expression, step of a
  property path and element of a group as a level`.
* RDF read from a request body, an upload, a `LOAD` or the shapes of `/{ds}/shacl` nests
  at most 256 triple terms (`<<( … )>>` or `<< … >>`) in Turtle, TriG, N-Triples, N-Quads
  and N3. A JSON-LD document nests at most 256 arrays and objects, and an RDF/XML
  document at most 1024 elements. Blank nodes (`[ … ]`) and collections (`( … )`) have no
  limit. The error names the limit that was passed.
* An update cannot store a triple term nested deeper than 256 levels. Without this check,
  an update such as `INSERT { ?s ?p <<( ?s ?p ?o )>> } WHERE { ?s ?p ?o }`, run again and
  again, would add a level each time.
* The results of a `SERVICE` call nest at most 1024 JSON arrays and objects, or XML
  elements. A triple term takes two levels. Deeper results fail the query as any other
  SERVICE error does.

Real queries stay far below these limits. In the W3C SPARQL test suites, no query nests
more than 5 brackets deep, and the deepest algebra count is 29. Parsing a query at the
bracket limit takes up to 1.5 MiB of stack, which fits the 2 MiB that Rust, tokio and
rayon give the threads they start. The server runs requests on threads with 8 MiB
stacks. The planner and the evaluator size their stack from the parsed query and grow it
when the thread's stack is too small, so a long chain runs on any thread.

The same limits apply to the MCP tools, SHACL-SPARQL constraints, ShEx SPARQL selectors,
the formatter, and the `sparkles` library (`Dataset::query`, `Dataset::update`,
`Dataset::load_str`).

### Budgets

The design and its rationale are in [C01 Observability, readiness and budgets](specs/C01-observability-and-budgets.md).

Queries run under per-request budgets, listed as `limits` in `/$/server`. Exceeding one
fails the request with `507 Insufficient Storage` and a body like this:

```json
{ "error": "query exceeds its memory budget: needs about 1.6 GiB, limit 1.0 GiB",
  "budget": "memory", "limit": 1073741824, "requested": 1717986918 }
```

* `memory` (`sparkles serve --query-memory-mb`, default 8192) is the estimated size in
  bytes of the intermediate results that a query, or the WHERE clause of an update, holds
  at once, at 8 bytes per value. It is checked before large intermediate results are
  built, so an oversized query fails fast. It is an estimate, not a limit on the
  process's memory.
* `result-bytes` (`--max-result-mb`, default 1024) is the serialized, uncompressed body of
  a SPARQL query response. Graph Store GET, the export path for a graph or the whole
  dataset, has its own budget, `--max-export-mb`, which is unlimited (`0`) by default.
  Past that budget the export fails the same way and reports the same `result-bytes`
  budget.
* `outbound-bytes` (`--outbound-request-max-mb`, default 4 × `--outbound-max-mb`, 1024)
  is the number of bytes that all the SERVICE calls and `LOAD <http…>` of one query or
  update receive. A compressed `LOAD` counts once decompressed. An update that exceeds
  the budget commits nothing, and `SILENT` does not hide the error. The summed time of
  these calls has a total too: `--outbound-request-timeout`, default
  4 × `--outbound-timeout`, 240 s. The http(s) imports of one `/{ds}/shex` validation
  share the same budget.
* `validation-work` is the work of one ShEx validation. It limits the partitions tried to
  match one node's neighbourhood to a shape (100,000), and the (node, shape) pairs of the
  typing (10,000,000, or `--query-memory-mb` at 64 bytes per pair if that is fewer). A
  validation past either limit fails. It never becomes a nonconformant result.
* `rows` (`--max-rows`, default 200,000,000) is the number of rows of any intermediate
  result.

`limit` and `requested` are in bytes, or in rows for `rows`. The response of
`/{ds}/update` includes `memPeakBytes`. `meta.memory.peakBytes` in
`application/x-sparkles+json` reports the peak estimate of a query.

**Streaming.** Query and Graph Store GET bodies are serialized on a worker thread. A body
of up to 1 MiB is sent whole, with `Content-Length`, and an error, including the
`result-bytes` budget, gets its status code. A larger body is streamed in 64 KiB chunks
as it is serialized, so server memory stays flat. An error after that point, such as the
budget running out at 1.2 GiB, aborts the transfer, and the client sees a truncated
response instead of a status code. A query result whose smallest encoding already
exceeds the budget is refused with `507` before anything is sent. A client that
disconnects stops the serialization.

**Large request bodies.** Graph Store PUT/POST bodies over 16 MiB, and upload files, are
written to a temporary file as they arrive rather than held in memory. A large PUT,
estimated above the bulk threshold, replaces its graphs in one index rebuild that parses
the body as a stream. Like every write it is atomic, so a parse error leaves the data as
it was.

## Authentication and access control

The design and its rationale are in [C09 Authentication and dataset-level access control](specs/C09-dataset-access-control.md).

`sparkles serve --auth-config FILE` turns authentication on. Without it there are no
credentials, and every request may do everything as the local principal. With it the
server denies by default, and a caller may do only what a grant allows.

Without authentication the server listens on loopback only. `--host` defaults to
`127.0.0.1`, and a non-loopback address is refused at startup unless
`--allow-open-network` (or `SPARKLES_ALLOW_OPEN_NETWORK=1`) is given. The override logs a
warning. A Unix socket (`--unix-socket`) counts as local. An authenticating reverse proxy
does not replace authentication, because the backend it protects must not be reachable
around it.

Any web page the operator opens can send requests to a local server, so a server without
auth also refuses these requests:

- A `Host` (or HTTP/2 `:authority`) that is not an IP address, `localhost`,
  `*.localhost`, `--host` or a `--public-host` name gets `421`. This stops a page that
  rebinds its own DNS name to the server's address, because that page sends its own name.
- Unsafe requests, and requests that need `write`, `admin` or `server-admin`, get
  `403 cross-origin request refused` when they are cross-origin by the rules of
  [CSRF and CORS](#csrf-and-cors). A request is cross-origin when its `Origin` is neither
  the request's own nor a `--cors-origin`, or when it sends
  `Sec-Fetch-Site: cross-site`.

CORS headers are sent only for `--cors-origin` origins, without credentials. Requests
without `Origin` or `Sec-Fetch-Site`, such as those from the CLI, curl or other servers,
pass. So does the server's own UI.

### Principals and credentials

Each request resolves to one principal. The first applicable source wins:

1. **`Authorization`.** `Bearer spk_…` with an API token, or `Basic` with a configured
   user and password. `Basic` also accepts a token as the password, with any user name,
   for Basic-only clients such as Jena. Invalid credentials are `401`, never treated as
   anonymous.
2. **The session cookie** of the web UI, `__Host-sparkles_session` over https or
   `sparkles_session` on http://localhost. A bad, expired or revoked cookie is ignored and
   cleared.
3. **Trusted proxy headers** (`Remote-User`, `X-Forwarded-User`, …), only from a peer in
   `proxy.trusted`. An entry there is a CIDR, or `unix` for `--unix-socket`. From any
   other peer the headers are ignored and counted in
   `sparkles_auth_untrusted_proxy_headers_total`.
4. **Anonymous**, with the grants of `[anonymous]` (none by default).

| Principal | Log name | From |
|---|---|---|
| user | `user:bob` | `[[users]]` (argon2id password) |
| token | `token:tok_…`, `token:cfg-NAME` | minted tokens, and static `[[tokens]]` |
| oidc | `oidc:alice@example.org` | a web UI login through the OIDC provider |
| proxy | `proxy:dave` | trusted headers of a forward-auth proxy |
| anonymous | `anonymous` | nothing else applied |

### Permissions

Dataset permissions are granted per dataset, by name or by `*` pattern (`"team-*"`). The
levels are `read` < `write` < `admin`.

| Level | Allows |
|---|---|
| `read` | Queries (including full-text and vector search), explain, Graph Store GET/HEAD, SHACL, `DatasetInfo`, stats, schema, prefixes, commits, reasoning status and diagnostics, text index status, `/$/ready/{ds}` and the dataset's tasks. |
| `write` | `read`, plus SPARQL Update, Graph Store PUT/POST/DELETE and upload. |
| `admin` | `write`, plus compaction, N-Quads backups, backups to repositories (create, delete, verify, restore), reasoning and clearing inferences, text index configuration, clearing the result cache, cloning (as the source) and deletion. |

There are three server permissions:

* `metrics` allows `/$/metrics` and the full `/$/ready` list.
* `federate` allows `SERVICE` and `LOAD <http…>`.
* `server-admin` allows everything: `admin` on every dataset, creating datasets, every
  token, and `LOAD <file:…>`. `LOAD <file:…>` also needs `serve --load-dir` and reads only
  files under it.

A principal's grants are the union of its own grants and its roles' grants. There are no
deny rules. `--read-only` still applies to everyone, after authorization. `federate` does
not open every URL. `SERVICE` and `LOAD <http…>` also follow the server's outbound
policy, which allows only public addresses unless `--outbound-allow-private` or
`--outbound-allow` is set (see
[Outbound requests](USAGE.md#outbound-requests-service-and-load)). A refused destination
also gets `403`. The local `sparkles query` and `sparkles update` run without a server or
permissions. They allow loopback and private destinations by default, and take
`--outbound-block-private` for the strict policy.

A **token** never exceeds its owner. At each use, its permissions are its scope
intersected with its owner's current grants, or with its parent token's grants for a
token minted by a token. Removing a grant or a role mapping shrinks every token at its
next request.

### Status codes

| Caller's level on `{ds}` | Caller | Dataset exists | Answer |
|---|---|---|---|
| none | anonymous | either | `401` with `WWW-Authenticate` |
| none | signed in | either | `404 {"error":"no such dataset: /ds"}`. The dataset is hidden, like a missing one. |
| too low | anonymous | either | `401` |
| too low | signed in | no | `404` |
| too low | signed in | yes | `403 {"error":"write access to /ds required"}` |

Invalid credentials get `401 {"error":"invalid credentials"}`, or `token expired`, with
`WWW-Authenticate: Bearer realm="sparkles", error="invalid_token"`. Browser navigations,
and non-browser clients when users are configured, also get a `Basic` challenge. Missing
server permissions give `401` to anonymous callers and `403`
(`{"error":"metrics permission required"}`) to others. A clone needs `admin` on the source
and on the new name (`403 no admin access to the target name /x`). `SERVICE` or `LOAD`
without the permission is a `403` before any connection or file is opened, even under
`SILENT`.

### Route permissions

| Route | Method | Needs |
|---|---|---|
| `/ui/*`, `/$/ping`, `/$/ready` | GET | Nothing. Without `metrics`, `/$/ready` lists only readable datasets. |
| `/$/whoami`, `/$/auth/config`, `/$/auth/login`, `/$/auth/oidc/*`, `/$/auth/device`, `/$/auth/token` | | Nothing. Invalid credentials are still `401`. |
| `/$/server`, `/$/datasets` (GET), `/$/tasks`, `/$/tasks/{id}`, `/$/auth/logout`, `/$/format` (POST) | | Any caller. `/$/format` admits nobody under `--format-endpoint off`, and only signed-in callers under `authenticated`. Listings show readable datasets only, and server-wide tasks only to `server-admin`. Cancelling a task (DELETE) needs `admin` on its dataset. |
| `/$/metrics` | GET | `metrics` |
| `/$/datasets` | POST | `server-admin` |
| `/$/datasets/{ds}`, `/$/stats/{ds}`, `/$/schema/{ds}…`, `/$/prefixes/{ds}`, `/$/commits/{ds}…`, `/$/ready/{ds}`, `/$/reason/{ds}` (GET), `/$/reason/{ds}/diagnostics`, `/$/text/{ds}` (GET), `/$/geo/{ds}` (GET), `/$/vector/{ds}`, `/$/snapshots/{ds}…` (GET), `/$/history/{ds}` (GET), `/{ds}/prefixes` (GET) | GET | `read` |
| `/$/datasets/{ds}` (DELETE), `/$/datasets/{ds}/clone`, `/$/compact/{ds}`, `/$/backup/{ds}`, `/$/cache/clear/{ds}`, `/$/reason/{ds}` (POST, DELETE), `/$/text/{ds}` (PUT, DELETE), `/$/text/{ds}/rebuild`, `/$/geo/{ds}` (PUT, DELETE), `/$/geo/{ds}/rebuild`, `/$/snapshots/{ds}` (POST), `/$/snapshots/{ds}/{name}` (DELETE), `/$/history/{ds}` (PUT) | | `admin` |
| `/$/backups/{ds}`, `/$/backups/{ds}/{repo}/{backup}` | GET | `read`. A backup of another dataset is `404`. |
| `/$/backups/{ds}` (POST), `/$/backups/{ds}/{repo}/{backup}` (DELETE), `…/restore`, `…/verify` | | `admin`. A restore also needs it on its target name. |
| `/$/repositories` | GET | Any caller. `server-admin` gets the full list, callers with `admin` on some dataset get names and types, and other callers get an empty list. |
| `/$/repositories…` (other routes), `/$/backup-policies…` | | `server-admin` |
| `/{ds}/sparql`, `/{ds}/query`, `/{ds}/explain`, `/{ds}/get`, `/{ds}/shacl`, `/{ds}/shex`, `/{ds}/data` (GET, HEAD) | | `read` |
| `/{ds}/update`, `/{ds}/upload`, `/{ds}/data` (other methods), `/{ds}/prefixes` (other methods) | | `write` |
| `/{ds}` | any | Depends on the operation. `update=` or `application/sparql-update` needs `write`, queries and GET need `read`, and other writes need `write`. |
| `/$/auth/tokens` (GET, POST), `/$/auth/tokens/{id}` (DELETE) | | a signed-in caller |
| `/$/auth/tokens?owner=…` | DELETE | `server-admin` |
| `/$/auth/device/{code}`, `…/approve`, `…/deny`, `/$/auth/cli/authorize` | | a web UI session or proxy identity |
| any other route | | `server-admin`. Unknown routes fail closed. |

`/$/metrics` needs the `metrics` permission, because counters by dataset name would
otherwise reveal which datasets exist. `metrics` therefore shows the names of all
datasets, up to `--metrics-max-datasets`, whatever the holder's dataset grants.
Prometheus scrapes it with a static token (`Authorization: Bearer spk_…`), for example
through `bearer_token_file` in the scrape config.

### CSRF and CORS

With auth, the server refuses unsafe requests, and any request that needs `write`,
`admin` or `server-admin`, with `403 cross-origin request refused` in two cases. The
first is a request that sends `Sec-Fetch-Site: cross-site`. The second is a request whose
`Origin` is neither the server's own nor in `cors.origins` or `--cors-origin`. The
server's own origin is `server.public_url`, or else the request's own origin. An allowed
origin passes, even though its pages are cross-site.

Session and proxy principals must also send `X-Sparkles-CSRF: <whoami csrfToken>` on
unsafe requests, or get `403 CSRF token missing or invalid`. CORS then allows only
`cors.origins` and `--cors-origin`, without credentials. Tools such as YASGUI send
`Authorization: Bearer` themselves. `Host` is not checked with auth, because a page on
another name gets no credentials of this server.

### whoami

`GET /$/whoami` describes the caller, with `Cache-Control: no-store`. It returns `401`
only for invalid credentials.

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

Without auth, the response is `{"authEnabled": false, "principal": {"kind": "local"},
"server": ["server-admin"], "datasets": {…all "admin"}}`.

`GET /$/auth/config` is public and tells the UI and the CLI how to sign in. It returns
`{"enabled": true, "methods": ["oidc", "token", "password", "proxy"], "oidc": {"loginUrl",
"displayName"}, "cli": {"authorizeUrl", "deviceAuthorizationEndpoint", "tokenEndpoint",
"deviceVerificationUri"}}`, or `{"enabled": false}`. Without auth, the other `/$/auth/*`
routes return `404`.

### Web UI sign-in and sessions

* `POST /$/auth/login` with `{"user", "password"}` or `{"token": "spk_…"}` returns `204`
  and a session cookie. The cookie is `HttpOnly`, `SameSite=Lax` and `Secure` over https,
  with `Max-Age` = `session.ttl` (default 12 h). A token session ends with its token.
  Wrong credentials get `401`.
* `GET /$/auth/oidc/login?return_to=/ui/…` redirects (`302`) to the provider. It uses the
  authorization code flow with PKCE `S256`, a `state` bound to the browser by a login
  cookie, and a `nonce`. The callback `GET /$/auth/oidc/callback` checks the state and
  redeems the code. It verifies the ID token: the JWKS signature with the configured
  algorithms, `iss`, `aud`, `azp`, `exp`, `iat` and `nonce`. It reads the name and groups,
  from UserInfo when the ID token lacks them, and checks admission. It then answers `303`
  to `return_to` with a session cookie. Failures go to
  `/ui/login?error=state|idp|idp_unavailable|not_allowed`.
* `POST /$/auth/logout` returns `{"redirect": url | null}`. The URL is the provider's
  end-session URL for OIDC sessions, and `proxy.logout_url` for proxy users.

Sessions are kept in `<data>/auth/sessions.json`, with hashed ids and mode 0600, and
survive restarts. Replacing `<data>/auth/session.key` signs everyone out. An owner keeps
at most 50 sessions, and a new session ends the owner's oldest. An owner is a user, or an
OIDC or proxy identity. A token login counts for the token's owner. The server keeps at
most 10,000 sessions. When it is full, the owner that holds the most loses its oldest
session, so that no one can sign the others out by opening sessions.

### API tokens

Tokens are `spk_` plus 43 base64url characters (256 random bits). The server stores only
their SHA-256, in `<data>/auth/tokens.json` with mode 0600.

* `POST /$/auth/tokens` with `{"name", "datasets": {"wiki": "read"}, "server": [],
  "expiresIn": "30d"}` returns `201` with `token`, `id` (`tok_…`), `scope`, `created` and
  `expires`. The `token` value appears only in this response. By default a token gets all
  of the minter's access and expires after `tokens_policy.default_ttl`. It lives at most
  `tokens_policy.max_ttl`, and expires no later than a token that minted it. Static tokens
  cannot mint.
* `GET /$/auth/tokens` returns `{"tokens": [{id, name, scope, created, expires, lastUsed,
  via, client, owner}]}` with the caller's own tokens. `?all=true` (server-admin) adds
  everyone's tokens and the static ones.
* `DELETE /$/auth/tokens/{id}` returns `204`. The id `self` means the token in use. Only
  the owner or a server-admin can delete a token, and other callers get `404`. Tokens
  minted by a revoked token die with it.
* `DELETE /$/auth/tokens?owner=oidc:alice@example.org` (server-admin) returns
  `{"revoked": n}`.

### CLI logins

`sparkles auth login --server URL` gets a token without copying secrets around:

* **Browser.** This is the default on desktops, or with `--web`. The CLI listens on
  `127.0.0.1:PORT` and opens `/ui/cli/authorize?port&state&code_challenge…`. After
  approval, the browser is sent back to the CLI with a one-time code, valid for 120 s. The
  CLI redeems the code at `POST /$/auth/token` (`grant_type=authorization_code`, `code`,
  `code_verifier`). The token never appears in a URL.
* **Device code** (RFC 8628). This is used over SSH, without a display, or with
  `--device`. `POST /$/auth/device` returns `{device_code, user_code: "WDJB-MJHT",
  verification_uri, verification_uri_complete, expires_in: 600, interval: 5}`. The user
  approves at `/ui/cli/device`. The CLI polls `POST /$/auth/token`
  (`grant_type=urn:ietf:params:oauth:grant-type:device_code`). It gets
  `authorization_pending`, `slow_down`, `access_denied` or `expired_token`, and finally
  `{access_token, token_type: "Bearer", expires_in, token_id, principal}` once. At most
  1000 logins can be pending. A session may fail 20 code lookups per 10 minutes, and then
  gets `429`.

Approval needs a web UI session or proxy identity. Bearer and Basic callers get
`403 this action requires signing in to the web UI`.

### Configuration

The configuration is TOML. Unknown keys are errors, reported with line and column. Keep
the file at mode `0600`. `sparkles auth check --config FILE` validates it, and `SIGHUP`
reloads it. Sessions and tokens follow the new policy at their next request, and a bad
file keeps the old policy.

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
the narrowest range. The Unix socket (`--unix-socket`, mode 0660, `trusted = ["unix"]`) is
the safest, and `tailscale serve` connects from 127.0.0.1. So that the CLI can sign in,
let `/$/auth/config`, `/$/auth/device`, `/$/auth/token` and requests with
`Authorization: Bearer spk_…` through the proxy unauthenticated, or give the CLI a
separate route. An `Authorization` header wins over proxy headers. Do not combine a
proxy's own Basic authentication with Sparkles auth, because the proxy would forward its
`Authorization` header.

When a local peer is trusted (a loopback address or `unix`), a request that carries
identity headers must name a known `Host`. A known host is an IP address, `localhost`,
`--host`, a `--public-host` name or the host of `server.public_url`. Any other host is
refused with `421`. This stops a web page that rebinds its own DNS name to the server, or
to the proxy in front of it, from sending its own `Remote-User`. Pass the name that
clients use to reach the proxy with `--public-host` or `server.public_url`. The server
warns at startup when it knows no such name, and the NixOS module passes its virtual
host.

Credentials travel as bearer secrets, so terminate TLS in front of the server. The server
warns when auth is on and it listens beyond loopback.

**Command line.** These commands handle authentication:

* `sparkles auth hash` hashes a password with argon2id.
* `sparkles auth gen-token --name N` creates a static token and prints its `[[tokens]]`
  entry on stderr.
* `sparkles auth check --config FILE` validates a configuration.
* `sparkles auth login|logout|status` and `sparkles auth token create|list|revoke` sign
  in to a server and manage tokens.

`query`, `update` and `load` accept `--server URL --dataset NAME` (or `SPARKLES_SERVER`).
They use the stored token (`$XDG_CONFIG_HOME/sparkles/credentials.toml`, mode 0600) or
`SPARKLES_TOKEN`.

**Rate limits.** Authentication failures are limited per client address before any
credential is checked. This is the `preauth` stage, on by default (see
[Rate limiting](#rate-limiting)). The `--rate-limit` classes count a signed-in caller by
its owner, across addresses and credentials. Bob's Basic requests, sessions and minted
tokens share one budget, and a token minted by a token belongs to the same owner. Each
static `[[tokens]]` entry is a client of its own. Anonymous callers and the `auth` class
(logins, CLI grants) are counted per client address.

The auth layer has limits of its own, which return `429` with `"limitClass": "auth"`:

* Tokens minted per owner: `tokens_policy.mint_rate`, default `60/h`. The `reason` is
  `mint`.
* Device logins started per client network, where a network is an IPv4 address or an
  IPv6 /48: 20, then two a minute, whether `preauth` is on or not. The `reason` is
  `device`.
* Unknown user codes per client network and per owner: 20, then two a minute, whichever
  runs out first. The `reason` is `device-code`, and each one also counts as a failure for
  `preauth`.

An owner has at most `tokens_policy.max_active_per_owner` unexpired tokens (default 100).
Minting another returns `409` until one is revoked or expires.

At most max(1, cores / 2) argon2 password verifications run at once, and four per permit
may wait, for up to five seconds. A check beyond that is refused at once with `503` and
`Retry-After: 1`. One client network has at most max(2, cores / 2) checks running and
waiting, and its further checks are refused the same way. A client network here is an
IPv4 address, an IPv6 /48, or all clients of an untrusted Unix socket together. This
keeps a single network from filling the queue for everyone.

**Metrics.** The auth layer exports `sparkles_auth_failures_total{scheme,reason}`,
`sparkles_auth_denied_total{kind}` (`unauthenticated`, `forbidden`, `hidden`,
`cross_origin`, `csrf`, `not_interactive`), `sparkles_auth_logins_total{method,result}`,
`sparkles_auth_tokens_minted_total{via}`, `sparkles_auth_tokens_revoked_total`,
`sparkles_auth_tokens_active`, `sparkles_auth_sessions_active`,
`sparkles_auth_device_grants_pending`, `sparkles_auth_password_verifications_total`,
`sparkles_auth_password_verifications_running`, `sparkles_auth_password_verifications_waiting`,
`sparkles_auth_untrusted_proxy_headers_total`, `sparkles_auth_reloads_total{result}`, and
the policy sizes `sparkles_auth_policy_{users,tokens,roles}`. Audit events are logged at
INFO under `sparkles::audit`. They cover logins, logouts, minted and revoked tokens,
device approvals and reloads.

## MCP server

The design and its rationale are in [C11 MCP server](specs/C11-mcp-server.md).

`sparkles mcp` speaks the [Model Context Protocol](https://modelcontextprotocol.io) on
stdin/stdout, as JSON-RPC 2.0 with one message per line. It is not an HTTP endpoint, but
it exposes the same engine as the server.

```
sparkles mcp (--loc [NAME=]PATH)... | (--data FILE... [--name NAME])
             [--allow-service] [--timeout SECS] [--query-memory-mb N] [--max-rows N]
             [--mcp-max-rows N] [--mcp-max-bytes N] [--max-concurrent N]
             [--disable-tool NAME]... [--schema-max-entries N] [--text]
```

| Flag | Default | Meaning |
|---|---|---|
| `--loc [NAME=]PATH` | | A database directory (repeatable). The name defaults to the directory's name. |
| `--data FILE…`, `--name` | `data` | RDF files loaded into one in-memory dataset. |
| `--text` | off | Indexes the `--data` dataset for `search_text`. A `--loc` database keeps the index it has (see `sparkles text-index`). |
| `--timeout SECS` | `60` | Largest `timeoutSeconds` a call may ask for. Calls default to 30. |
| `--query-memory-mb N` | `2048` | Memory budget of every call's queries. `0` means unlimited. |
| `--max-rows N` | `200000000` | Rows of any intermediate result. |
| `--mcp-max-rows N` / `--mcp-max-bytes N` | `1000` / `1048576` | Largest `maxRows` / `maxBytes` of `sparql_query`. |
| `--max-concurrent N` | `4` | Tool calls running at once. Further calls wait, and their timeout runs while they wait. |
| `--allow-service` | off | Allows `SERVICE` in queries. |
| `--outbound-allow-private`, `--outbound-block-private`, `--outbound-allow HOST_OR_CIDR`, `--outbound-timeout S`, `--outbound-max-mb N` | private blocked, none, `60`, `256` | Where an allowed `SERVICE` may connect, as for `sparkles serve`. |
| `--disable-tool NAME` | | Does not offer the tool. |

The process exits 0 when stdin closes and 1 on a startup error. Logs go to stderr.

**Protocol.** The server supports revision `2026-07-28` and the legacy `initialize`
handshake of `2025-11-25` and `2025-06-18`. Revision `2026-07-28` is stateless. It uses
`server/discover`, and each request carries the protocol version and client capabilities
in its `_meta`. An unknown revision gets `-32022` with `data.supported`. The capabilities
are `{"tools": {}}`. `server/discover` and `tools/list` are cacheable for an hour
(`ttlMs: 3600000`, `cacheScope: "public"`), because the tool set is fixed for the life of
the process. `notifications/cancelled` stops the referenced call, and no response is sent
for that call. The server's `instructions` describe the workflow
(`list_datasets` → `describe_schema` → `sparql_query`) and say that tool results are
untrusted data.

### Tools

Tools appear in this order. All are read-only
(`annotations: {"readOnlyHint": true, "openWorldHint": false}`). `sparql_query` is
open-world when SERVICE is allowed. The common arguments are:

* `dataset`: a name from `list_datasets`. It is optional when the server has one
  dataset.
* `atCommit` (integer): read the snapshot of that commit (see below).
* `reasoning` (boolean): include materialized inferences. By default they are included
  when the dataset has them.
* IRIs may be given as `<http://…>`, `http://…` or a prefixed name (`ex:alice`, with the
  dataset's prefixes). In `describe_resource` they may also be a blank node label
  `_:b…`.

| Tool | Arguments (besides the common ones) | Result |
|---|---|---|
| `list_datasets` | none | `{datasets: [{name, quads, commit, modified, reasoning: null\|{profile, stale}, textSearch, writable}], limits: {defaultMaxRows, maxRows, defaultMaxBytes, maxBytes, defaultTimeoutSeconds, maxTimeoutSeconds, service, updates}}` |
| `describe_schema` | `section` (`summary`\|`classes`\|`predicates`), `graph` (`default`\|`union`\|IRI), `includeBuiltin`, `limit` (1–500; 25 for the summary, 100 for lists), `cursor` | `{dataset, commit, graph, reasoning, section, totals: {triples, classes, predicates}, builtinClassesHidden, ontology?, roots?, classes?: [{iri, label?, instances, declared, superClasses?}], predicates?: [{iri, label?, triples, distinctSubjects, distinctObjects, maxPerSubject, objects: ["iri 120", "xsd:string 98", "rdf:langString@en,de 12", …], domains?, ranges?, vector?}], next, prefixes}`. The summary lists the largest classes and predicates. `classes` and `predicates` page through all entries in IRI order. |
| `sparql_query` | `query` (required), `format` (`table`\|`json`), `maxRows` (100), `maxBytes` (65536), `maxTermChars` (500), `offset`, `exactTotal` (true), `timeoutSeconds` (30) | One text block: a table or a JSON document (below). No `structuredContent`. |
| `explain_query` | `query` (required), `includeAlgebra` | `{dataset, commit, queryType, estimatedRows, plan, algebra?, warnings: [{code, message}]}`. `plan` has one line per operator, `<operator> <description> est=<rows> [<columns>]`, indented by depth. The warnings are `unknown-term` (a constant IRI or literal of a triple pattern that the dataset does not contain), `no-limit` (no top-level LIMIT, and over 10,000 rows estimated), `large-estimate` (an intermediate result over 50M rows) and `service-disabled`. |
| `describe_resource` | `iri` (required), `direction` (`both`\|`outgoing`\|`incoming`), `maxTriples` (50 per direction, ≤ 500), `lang` (`en`) | `{dataset, commit, iri, exists, label?, types, outgoing?, incoming?, prefixes}`. Each side is `{total, predicates: [{p, count}], predicatesTotal, triples: [{p, o, oLabel?}` or `{s, sLabel?, p}], truncated}`. Triples are sampled round-robin by predicate, so a hub's largest predicate does not hide the others. |
| `list_commits` | `limit` (10, ≤ 100), `before` | `{dataset, head, firstRetained, complete, commits: [{seq, timestamp, kind, inserted, deleted, quads}], next: {before} \| null}` |
| `search_text` | `query` (required, ≤ 1000 characters: terms, `"phrases"`, AND/OR, `+required`, `-excluded`), `predicates` (≤ 20 IRIs), `lang`, `limit` (20, ≤ 200), `withTypes` (true) | `{dataset, commit, hits: [{s, score, text, p, label?, types?}], limited, prefixes}`: BM25-ranked matches of `text:query`. `text` is the matched literal, escaped and at most 300 characters long, and `types` has at most 3 entries. Only in builds with the `text` feature. A dataset without an index (`textSearch: false`) gives `text-disabled`. |
| `similar_entities` | `predicate` (required), exactly one of `entity` (an IRI with one stored vector under `predicate`) and `vector` (1–16384 numbers), `k` (10, ≤ 100), `metric` (`cosine`\|`dot`\|`euclidean`), `excludeSelf` (true), `withLabels` (true) | `{dataset, commit, metric, higherIsBetter, hits: [{iri, score, label?}], prefixes}`: an exact `spk:vectorSearch` over the stored `spk:vector` literals. The tool never computes embeddings. `no-vectors` when the predicate has none, the dimensions differ, or the entity has no vector. |
| `validate_shacl` | `shapes` (required: a shapes graph in Turtle, ≤ 1 MiB), `graph` (`default`\|`union`\|IRI), `maxResults` (20, ≤ `--mcp-max-rows`), `timeoutSeconds` (30) | `{dataset, commit, reasoning, conforms, total, bySeverity: {violation, warning, info}, results: [{focus, path?, value?, shape, constraint, severity, message?}], truncated, prefixes}`: the validation of [`/{ds}/shacl`](#shacl-validation). The most severe results come first, then results are ordered by shape and focus node. `severity` is `Violation`, `Warning` or `Info`. SHACL 1.2 `Debug` and `Trace` count as info. A complex `path` is a SPARQL property path. Only in builds with the `shacl` feature. |
| `validate_shex` | `schema` (required: ShExC, or ShExJ when it starts with `{`; ≤ 1 MiB), `shapeMap` (required: a compact shape map, ≤ 65536 characters), `graph`, `onlyNonconformant` (true), `maxResults` (20, ≤ `--mcp-max-rows`), `timeoutSeconds` (30) | `{dataset, commit, reasoning, conforms, counts: {conformant, nonconformant}, results: [{node, shape, status, reason?, failures?}], truncated, warnings, prefixes}`: the validation of [`/{ds}/shex`](#shex-validation), with results in shape-map order. `shape` is `START` for a START association. `failures` are the report's `appinfo.failures`, with `value` as a term and `predicate` as an IRI. Prefixed names in the map use the schema's prefixes, then the dataset's. `IMPORT` is refused with `bad-argument`, so put the imported shapes into the schema. EXTERNAL shapes have no definition (`invalid-schema`). `SPARQL """…"""` node selectors run on the data graph under the call's row and memory budgets, without SERVICE, and with only their own prefixes. A failing selector query is `invalid-schema`. Only in builds with the `shex` feature. |
| `format` | `text` (required, ≤ 1 MiB), `language` (`sparql`\|`turtle`\|`trig`\|`ntriples`\|`nquads`\|`jsonld`; detected when left out), `options` (the camelCase style options of [`POST /$/format`](#formatting)), `timeoutSeconds` (30). It takes no `dataset`. | `{language, changed, text, warnings: [{code, message, line, column}]}`: the text formatted by the engine of `sparkles fmt`. A syntax error is `syntax`, with the line and column in the message. RDF/XML is `unsupported-language`. A result larger than `--mcp-max-bytes` is `too-large`. Only in builds with the `fmt` feature. |

Every tool except `sparql_query` declares an `outputSchema` and returns
`structuredContent` plus the same object as one compact JSON text block. `tools/list`
has the complete JSON Schemas.

**Validation tools.** `validate_shacl` and `validate_shex` read one snapshot, and take
`atCommit` and `reasoning` like the other tools. They write nothing and fetch nothing.
There are no imports, and no SERVICE, which SHACL-SPARQL refuses as it does over HTTP.
`total` and `counts` cover every result. `results` holds the first `maxResults`, cut
earlier when the results would pass `--mcp-max-bytes`, and `truncated` says whether any
were left out. A call runs under its timeout, in the validation thread pool of
`/{ds}/shacl`. The memory budget bounds the report at 512 bytes a result, so a SHACL
report or ShEx result map larger than that is `budget-memory`. It also bounds the ShEx
typing at 64 bytes a pair, and running past that is `budget-validation-work`.

**Terms** in results use Turtle/SPARQL syntax, so they can be pasted into queries. They
look like `ex:alice` (when a dataset prefix's namespace fits), `<http://…>`, `_:b1f`,
`"text"`, `"text"@en`, `"x"^^xsd:date`, bare `42` / `1.5` / `true` for canonical integers,
decimals and booleans, and `<<( s p o )>>`. Inside quotes, `\`, `"`, line breaks, TAB,
other control characters and U+2028/U+2029 are escaped, so a term is always one line. A
lexical form or IRI longer than `maxTermChars` characters is cut, and the cut is marked
outside the quotes: `"Lorem ipsum"…(+4519 chars)`. `prefixes` lists the prefixes a result
used. Labels come from `rdfs:label`, `skos:prefLabel`, `schema:name`, `foaf:name` and
`dcterms:title`, in that priority. The requested language is preferred, then no language.

**`sparql_query` tables.**

```
# SELECT · rows 1–100 of 12345 (TRUNCATED: maxRows=100) · commit 42
PREFIX ex: <http://ex.org/>
?s	?name
ex:alice	"Alice"@en
…
# more: call sparql_query with the same query, offset=100, atCommit=42
```

The first line has the query type, the rows shown, the total, the truncation reason, the
commit and `· N terms shortened`. With `exactTotal: false` the total reads `of ≥N`,
because counting stops after `offset+maxRows+1` solutions. Cells are separated by TAB,
and an unbound variable is an empty cell. CONSTRUCT and DESCRIBE rows are `s p o .`, and
ASK is `true` or `false`. Rows stop at `maxRows`, or before the row that would take the
whole text past `maxBytes` bytes. Status lines start with `#`, which no rendered term
can. `format: "json"` gives `{dataset, commit, queryType, vars, rows: [[term | null]],
boolean?, total | null, offset, returned, truncated: null | {reason: "maxRows"|"maxBytes",
next: {offset, atCommit}}, termsShortened, prefixes, elapsedMs}`, also bounded by
`maxBytes`.

**Snapshots.** A call without `atCommit` reads the head and names its commit. With
`atCommit`, the call reads that commit if it is the head or is still held. The server
holds the last 4 commits read per dataset, and 32 overall, for 10 minutes after their
last use. Otherwise the call fails with `unknown-commit`, with a message such as
"commit 38 is no longer held (head is 42); rerun without atCommit …" or
"commit 57 does not exist …". All internal queries of one call read one snapshot, and
`describe_schema` cursors are bound to their snapshot.

### Errors

A failed call is a result with `isError: true` and one text block
`"<message>\nHint: <remedy>"`. It also carries
`_meta["io.github.kclejeune.sparkles/error"] = {code, status, budget?}`, where `status`
is the equivalent HTTP status:

| code | status | when |
|---|---|---|
| `bad-argument` | 400 | An argument outside its schema: an unknown field, a value out of range or a bad IRI. |
| `unknown-dataset` | 404 | No such dataset, or `dataset` omitted on a server with several. The hint lists them. |
| `syntax` | 400 | A SPARQL syntax error, with line and column. The hint lists the predeclared prefixes. Also a shapes graph, ShEx schema or shape map that does not parse. |
| `invalid-shapes`, `invalid-schema` | 400 | Shapes the SHACL validator cannot use. A ShEx schema that parses but cannot be used (an undefined reference, a negated cycle, an EXTERNAL shape), or a shape-map label it does not define. |
| `not-a-query` | 400 | SPARQL Update sent to `sparql_query`. |
| `timeout` | 408 | The call's timeout passed. |
| `budget-memory`, `budget-rows`, `budget-validation-work` | 507 | A query or validation budget was exceeded. |
| `service-disabled` | 403 | A query uses SERVICE and it is not allowed. |
| `unknown-commit` | 404 / 410 | `atCommit` is in the future (404) or no longer held (410). |
| `stale-cursor` | 409 / 400 | A schema cursor whose snapshot is gone (409), or a malformed cursor (400). |
| `unknown-graph`, `too-many-entries` | 404, 413 | Schema discovery errors. `unknown-graph` also covers the `graph` of a validation tool. |
| `text-disabled` | 400 | `search_text` on a dataset without a full-text index. |
| `no-vectors` | 400 | From `similar_entities`: no vectors under the predicate, a dimension mismatch, or an entity without a vector. |
| `text-unavailable`, `write-failed`, `unsupported` | 503, 503, 501 | As over HTTP. |
| `internal` | 500 | Anything else. The message is "internal error (request id …)", and the error is logged at ERROR. |

An unknown tool is a protocol error (`-32602`, "Unknown tool: NAME").
