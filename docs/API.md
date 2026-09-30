# Sparkles HTTP API

The server speaks the **Fuseki** protocol surface (so existing Jena tooling —
`rdfconnection`, `s-query`, YASGUI, etc. — works unchanged) plus a small set of
`/$/…` extensions used by the web UI.

All admin endpoints live under `/$/`. Dataset names match `[A-Za-z0-9_.-]+` and are
addressed as `/{ds}`. JSON responses use `application/json`.

## Server

| Method | Path          | Description |
|--------|---------------|-------------|
| GET    | `/$/ping`     | Plain-text timestamp. Liveness check: `200` whenever the process serves HTTP. |
| GET    | `/$/ready`    | Readiness: `200` when ready, else `503`; the body is always `ReadyInfo`. `Cache-Control: no-store`. |
| GET    | `/$/ready/{ds}` | The same for one dataset (`datasets` has one entry); `404` if the dataset is unknown. |
| GET    | `/$/server`   | `{ "version", "startedAt", "uptimeSeconds", "readOnly", "datasets": [DatasetInfo], "limits": Limits }` |
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
type Limits = { timeoutSeconds: number; updateTimeoutSeconds: number; queryMemoryBytes: number; maxResultBytes: number; maxRows: number };
```

### Request ids and the access log

Every response carries `X-Request-Id` (exposed to browsers through CORS). An incoming
`X-Request-Id` of 1–128 characters from `[A-Za-z0-9._:-]` is kept; otherwise the server
generates one (`{boot:08x}-{seq:012x}`, unique per process and increasing). The id is the
`request_id` field of the request's log span, so every log line of the request carries it.

`sparkles serve` logs one INFO line per completed request under the target
`sparkles::access` (`--no-access-log` turns it off; `/ui/*`, `/$/ping`, `/$/ready` and
`/$/metrics` are logged at DEBUG). Fields: `dataset` (or `$none`), `operation` (`query`,
`update`, `gsp`, `upload`, `shacl`, `explain`, `admin`, `other`), `status`, `outcome`
(`ok`, `client_error`, `error`, `timeout`, `cancelled`, `budget`, `rate_limited`), and where known `rows`,
`parse_ms`, `plan_ms`, `exec_ms`, `serialize_ms`, `total_ms`, `response_bytes` and
`mem_peak_bytes`. A request whose client disconnects is logged with `status=499` and
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
| `process_resident_memory_bytes` | gauge (Linux) | |

Label values are bounded: `dataset` is an existing dataset name (at most
`--metrics-max-datasets`, default 100; the others share `$other`) or `$none` for requests
that name no existing dataset. A (dataset, operation) pair appears after its first request,
then with all six outcomes. Health checks (`/$/ping`, `/$/ready`), `/$/metrics` and UI assets
are not counted. Deleting a dataset removes its series. Each dataset has its own block and
result cache, each sized to the global `--cache-mb` / `--result-cache-mb`.

With rate limits configured, `sparkles_rate_limited_total{dataset,class}` (counter)
counts refused requests per limit class, and `outcome="rate_limited"` appears in
`sparkles_requests_total`.

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
    rateLimited: Record<"auth" | "query" | "update" | "admin", number> | null;
    blockCache: { bytes: number; capacityBytes: number; entries: number; hits: number; misses: number };
    resultCache: { enabled: boolean; bytes: number; capacityBytes: number; entries: number; hits: number; misses: number };
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
  client span) for each SERVICE call, and `shacl.validate`.
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
`sparkles.result_cache.{size,capacity,entries,hits,misses}`, `sparkles.ready`,
`process.uptime` and `process.memory.usage`.

**Logs.** With `--otel-logs` or `OTEL_LOGS_EXPORTER=otlp`, every log event that passes
`RUST_LOG` (the access log included) is also exported as an OTLP log record with the
trace and span id of its request.

### Rate limiting

Off by default. `sparkles serve --rate-limit SPEC` (repeatable) and/or
`--rate-limit-config FILE` limit each request class per client:

| Class | Requests |
|-------|----------|
| `auth` | every path under `/$/auth/` (login, token minting, device flow, OIDC callback), matched or not |
| `query` | `/{ds}/sparql`, `/{ds}/query`, `/{ds}/explain`, `/{ds}/shacl`, Graph Store `GET`/`HEAD`, `/{ds}` with `query=` or a GET, `/$/schema/*`, `/$/stats/*`, `/$/reason/{ds}/diagnostics` |
| `update` | `/{ds}/update`, `/{ds}/upload`, Graph Store `PUT`/`POST`/`DELETE`, `/{ds}` with `update=` or any other write (a form POST to `/{ds}` counts as an update) |
| `admin` | `/$/…` requests other than `GET`/`HEAD` (dataset management, compaction, backups, reasoning, caches, full-text) |

`/$/ping`, `/$/ready*`, `/$/metrics`, the UI and the other `/$/` reads are never limited.

`SPEC` is `CLASS[@DATASET]=LIMIT`, where `LIMIT` is `off` or a comma-separated list of

* `N/s`, `N/min`, `N/h` or `N/d`: the sustained rate per client;
* `burst=N`: requests a client may make at once after being idle (default: the rate's `N`);
* `concurrency=N`: requests of the class in flight server-wide;
* `client-concurrency=N`: requests in flight per client;
* `failure-cost=N`: what a `401` or `403` response costs, in requests (default 1), so that
  failed logins exhaust the budget faster.

`CLASS@DATASET=…` replaces the class limit for requests to that dataset (with its own
counters); `CLASS@DATASET=off` exempts the dataset. Examples:

```sh
sparkles serve --rate-limit auth=10/min,burst=5,failure-cost=3 \
               --rate-limit query=100/s,burst=200,client-concurrency=8,concurrency=64 \
               --rate-limit update=10/s --rate-limit query@public=5/s
```

`auth=10/min,burst=5` (with `failure-cost=3`) is a reasonable strict default for
authentication endpoints. The configuration file is JSON; the flags apply on top of it,
and `SIGHUP` re-reads it (client counters start afresh; a bad file keeps the running
configuration):

```json
{
  "classes": {
    "auth":  { "rate": "10/min", "burst": 5, "failureCost": 3 },
    "query": { "rate": "100/s", "burst": 200, "concurrency": 64, "clientConcurrency": 8 }
  },
  "datasets": { "public": { "query": { "rate": "5/s" } } },
  "trustedProxies": ["127.0.0.1", "::1"],
  "maxKeys": 100000
}
```

**Clients.** A client is its peer address (an IPv6 client by its /64). Behind a reverse
proxy, list the proxy under `trustedProxies` (or `--rate-limit-trusted-proxy CIDR`): for
requests from a trusted address the client is the rightmost untrusted hop of `Forwarded`
(RFC 7239), else of `X-Forwarded-For`. Headers from untrusted peers are ignored. At most
`maxKeys` clients (default 100,000, about 100 bytes each) are tracked; a flood of new
addresses evicts other rarely seen clients, never one with requests in flight. Clients
whose bucket has refilled are dropped every minute.

**Algorithm.** GCRA (the virtual-scheduling form of a token bucket): a client may send
`burst` requests at once, then one every `period / N`.

**Responses.** Over the rate: `429 Too Many Requests` with `Retry-After` (whole seconds).
Over a concurrency cap: `503 Service Unavailable` with `Retry-After: 1`, immediately;
requests are never queued, so a saturated server sheds load instead of holding waiting
requests. A request holds its concurrency slot until its response body has been sent
(streamed Graph Store GETs included). The body uses the error format:

```json
{ "error": "too many query requests: retry in 2 s",
  "limitClass": "query", "reason": "rate", "retryAfterSeconds": 2 }
```

`reason` is `rate`, `concurrency` or `client-concurrency`. Responses of a class with a
rate carry the headers of draft-ietf-httpapi-ratelimit-headers-11:
`RateLimit-Policy: "query";q=100;w=1` (the configured rate: `q` requests per `w`
seconds; the policy name is `CLASS` or `CLASS@DATASET`) and `RateLimit: "query";r=57;t=1`
(`r` requests available now, `t` seconds until the bucket is full). CORS exposes
`Retry-After`, `RateLimit` and `RateLimit-Policy`.

**Observability.** Refused requests are logged with `outcome=rate_limited` and counted
in `sparkles_requests_total{outcome="rate_limited"}` and
`sparkles_rate_limited_total{dataset,class}`.

## Datasets (admin)

| Method | Path                         | Description |
|--------|------------------------------|-------------|
| GET    | `/$/datasets`                | `{ "datasets": [DatasetInfo] }` |
| POST   | `/$/datasets`                | Create. Form or JSON body: `dbName`, `dbType` = `persistent` \| `mem`. `201` on success, `409` if exists. |
| GET    | `/$/datasets/{ds}`           | `DatasetInfo` |
| DELETE | `/$/datasets/{ds}`           | Remove dataset (and its files). |
| POST   | `/$/datasets/{ds}/clone`     | Copy the dataset into a new persistent dataset. See [Clone](#clone). `202` with a `Task`. |
| GET    | `/$/stats/{ds}`              | `DatasetStats` |
| GET    | `/$/schema/{ds}`             | *Extension.* `SchemaSummary`: classes and predicates with exact counts and their declarations; see [Schema discovery](#schema-discovery). |
| GET    | `/$/schema/{ds}/classes`     | *Extension.* `Page<ClassEntry>` |
| GET    | `/$/schema/{ds}/predicates`  | *Extension.* `Page<PredicateEntry>` |
| POST   | `/$/compact/{ds}`            | Merge delta (updates) into a freshly built, sorted base index. Returns `Task`. |
| POST   | `/$/backup/{ds}`             | Write gzipped N-Quads dump to `<data>/backups/`. Returns `Task`. |
| POST   | `/$/reason/{ds}`             | Materialize inferences. JSON body `{ "profile": "rdfs" \| "owl-rl" \| "rules", "rules"?: string }`, or `{ "rerun": true }` (also `?rerun=true`) to re-run the recorded profile and rules (`409` when nothing is recorded). Returns `Task`. |
| GET    | `/$/reason/{ds}`             | `ReasoningStatus`, or `{ "reasoning": null, "head": number }`. See [Reasoning status and diagnostics](#reasoning-status-and-diagnostics). |
| GET    | `/$/reason/{ds}/diagnostics` | `DiagnosticsReport`: OWL 2 RL inconsistency checks. |
| DELETE | `/$/reason/{ds}`             | Drop materialized inferences. |
| GET    | `/$/tasks`                   | `[Task]` |
| GET    | `/$/tasks/{id}`              | `Task` |
| POST   | `/$/cache/clear/{ds}`        | *Extension (no Fuseki equivalent).* Drop the dataset's cached query results. `{ "cleared": number /* entries */, "bytes": number }` |
| GET    | `/$/prefixes/{ds}`           | `{ "prefixes": { "rdf": "http://…#", … } }` — the dataset's prefixes plus well-known ones. |
| GET    | `/{ds}/prefixes`             | After Fuseki's prefixes service. `?prefix=p` → `{ prefix, uri }` (`404` if unbound); `?uri=u` → `{ uri, prefixes: [...] }`; neither → `{ prefixes: {...} }` (stored ones only). |
| POST/PUT | `/{ds}/prefixes`           | Bind `prefix` to `uri` (query, form or JSON body `{prefix, uri}`); `400` for an invalid name or IRI. Prefixes are metadata: no commit is made. |
| DELETE | `/{ds}/prefixes?prefix=p`    | Remove a binding (`204`, or `404` if unbound). |

```ts
type DatasetInfo = {
  name: string;            // "ds"
  type: "persistent" | "mem";
  endpoints: { query: string; update: string; gsp: string; upload: string; shacl?: string /* absent when built without the `shacl` feature */ };
  quads: number;           // approximate total (base + delta)
  reasoning: null | {
    profile: string; inferred: number; at: string;
    commit: number | null;       // commit the inferences were materialized at
    stale: boolean | null;       // null: unknown (see ReasoningStatus)
    commitsSince: number | null;
  };
  forkedFrom?: { id: string; seq: number };   // clones: source dataset id and copied commit
  origin?: DatasetOrigin;                     // clones: origin.json (see Clone)
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
};

type Task = {
  id: string; kind: "compact" | "backup" | "reason" | "load" | "clone";
  dataset: string; target?: string /* the dataset a clone creates */;
  state: "running" | "done" | "failed";
  startedAt: string; finishedAt?: string; message?: string; progress?: number /*0..1*/;
};
```

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
| POST       | `/{ds}/update`        | SPARQL 1.1 Update protocol (`update=` form or `application/sparql-update` body). |
| GET/PUT/POST/DELETE/HEAD | `/{ds}/data` , `/{ds}/get` | Graph Store Protocol. `?default` or `?graph=<iri>`; no param on GET = whole dataset as N-Quads/TriG. GET is streamed from one snapshot (see [Budgets](#budgets)). |
| POST       | `/{ds}/upload`        | Multipart file upload; format chosen from filename extension / content-type. Optional `graph` field. |
| POST       | `/{ds}/shacl`         | SHACL validation (Fuseki `/{ds}/shacl`); see [SHACL validation](#shacl-validation). |

Content negotiation via `Accept` or the `format=` parameter (Fuseki style):

* SELECT/ASK: `application/sparql-results+json` (default), `application/sparql-results+xml`,
  `text/csv`, `text/tab-separated-values`, and `application/x-sparkles+json` (see below).
* CONSTRUCT/DESCRIBE/GSP GET: `text/turtle` (default), `application/n-triples`,
  `application/n-quads`, `application/trig`, `application/ld+json`, `application/rdf+xml`.

Query parameters beyond the standard protocol:

* `timeout=<seconds>` — query timeout (default 60 s, `sparkles serve --timeout`). Updates
  accept it too; without it they run under `--update-timeout` (none by default). A timed-out
  update changes nothing.
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
  the text of its own snapshot. If an index is behind (a failed update, a rebuild in
  progress), text queries return `503` until it is rebuilt. They never return stale
  results.
* **Durability**: index commits are not fsynced; the write-ahead log is the durable
  record. The index is checkpointed (synced) about once a second while writes continue,
  before compaction and on close. After a crash, an index with unsynced changes
  (`text.dirty` next to it) is checksum-verified and caught up from the WAL; it is
  rebuilt only if it is damaged or older than the WAL.
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
};
```

## Explain

`GET|POST /{ds}/explain?query=…` → `{ "algebra": string /* SSE */, "plan": PlanNode }` (plan not executed; `actualRows`=-1).

## Errors

Non-2xx responses carry `{ "error": string, "detail"?: string, "line"?: number, "column"?: number, "requestId": string }`
(`requestId` is the response's `X-Request-Id`, for finding the request in the logs)
with `400` for parse errors, `404` unknown dataset, `408` timeout, `409` conflict, `429`
over a rate limit, `503` for a cancelled query, over a concurrency limit (see
[Rate limiting](#rate-limiting)) or when a write-ahead log write failed (writes are refused
until restart; reads continue), `500` otherwise.

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
  query response. Graph Store GET has no such budget and suits whole-dataset exports.

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
