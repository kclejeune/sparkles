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
| GET    | `/$/server`   | `{ "version", "startedAt", "uptimeSeconds", "datasets": [DatasetInfo], "limits": Limits }` |
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
type Limits = { timeoutSeconds: number; queryMemoryBytes: number; maxResultBytes: number; maxRows: number };
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
(`ok`, `client_error`, `error`, `timeout`, `cancelled`, `budget`), and where known `rows`,
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
    blockCache: { bytes: number; capacityBytes: number; entries: number; hits: number; misses: number };
    resultCache: { enabled: boolean; bytes: number; capacityBytes: number; entries: number; hits: number; misses: number };
  }[];
};
```

## Datasets (admin)

| Method | Path                         | Description |
|--------|------------------------------|-------------|
| GET    | `/$/datasets`                | `{ "datasets": [DatasetInfo] }` |
| POST   | `/$/datasets`                | Create. Form or JSON body: `dbName`, `dbType` = `persistent` \| `mem`. `201` on success, `409` if exists. |
| GET    | `/$/datasets/{ds}`           | `DatasetInfo` |
| DELETE | `/$/datasets/{ds}`           | Remove dataset (and its files). |
| GET    | `/$/stats/{ds}`              | `DatasetStats` |
| POST   | `/$/compact/{ds}`            | Merge delta (updates) into a freshly built, sorted base index. Returns `Task`. |
| POST   | `/$/backup/{ds}`             | Write gzipped N-Quads dump to `<data>/backups/`. Returns `Task`. |
| POST   | `/$/reason/{ds}`             | Materialize inferences. JSON body `{ "profile": "rdfs" \| "owl-rl" \| "rules", "rules"?: string }`. Returns `Task`. |
| DELETE | `/$/reason/{ds}`             | Drop materialized inferences. |
| GET    | `/$/tasks`                   | `[Task]` |
| GET    | `/$/tasks/{id}`              | `Task` |
| POST   | `/$/cache/clear/{ds}`        | *Extension (no Fuseki equivalent).* Drop the dataset's cached query results. `{ "cleared": number /* entries */, "bytes": number }` |
| GET    | `/$/prefixes/{ds}`           | `{ "prefixes": { "rdf": "http://…#", … } }` — prefixes seen during loading plus well-known ones. |

```ts
type DatasetInfo = {
  name: string;            // "ds"
  type: "persistent" | "mem";
  endpoints: { query: string; update: string; gsp: string; upload: string; shacl?: string /* absent when built without the `shacl` feature */ };
  quads: number;           // approximate total (base + delta)
  reasoning: null | { profile: string; inferred: number; at: string };
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
};

type Task = {
  id: string; kind: "compact" | "backup" | "reason" | "load";
  dataset: string; state: "running" | "done" | "failed";
  startedAt: string; finishedAt?: string; message?: string; progress?: number /*0..1*/;
};
```

## Per-dataset SPARQL protocol (Fuseki compatible)

| Method     | Path                  | Description |
|------------|-----------------------|-------------|
| GET/POST   | `/{ds}` , `/{ds}/sparql`, `/{ds}/query` | SPARQL 1.1 Query protocol (`query=` param, `application/sparql-query` body, or form). `default-graph-uri` / `named-graph-uri` supported. |
| POST       | `/{ds}/update`        | SPARQL 1.1 Update protocol (`update=` form or `application/sparql-update` body). |
| GET/PUT/POST/DELETE/HEAD | `/{ds}/data` , `/{ds}/get` | Graph Store Protocol. `?default` or `?graph=<iri>`; no param on GET = whole dataset as N-Quads/TriG. |
| POST       | `/{ds}/upload`        | Multipart file upload; format chosen from filename extension / content-type. Optional `graph` field. |
| POST       | `/{ds}/shacl`         | SHACL validation (Fuseki `/{ds}/shacl`); see [SHACL validation](#shacl-validation). |

Content negotiation via `Accept` or the `format=` parameter (Fuseki style):

* SELECT/ASK: `application/sparql-results+json` (default), `application/sparql-results+xml`,
  `text/csv`, `text/tab-separated-values`, and `application/x-sparkles+json` (see below).
* CONSTRUCT/DESCRIBE/GSP GET: `text/turtle` (default), `application/n-triples`,
  `application/n-quads`, `application/trig`, `application/ld+json`, `application/rdf+xml`.

Query parameters beyond the standard protocol:

* `timeout=<seconds>` — query timeout (default 60 s, `sparkles serve --timeout`); updates
  accept it too.
* `send=<n>` — cap on rows serialized (the UI uses this so a huge result does not hang the browser; `meta.totalRows` still reports the full count).
* `reasoning=true|false` — include materialized inferences (default `true` if present).
* `nocache=true` — bypass the query result cache: nothing is read from or stored in it
  (for benchmarking; `explain` accepts it too). The server-wide budget is set with
  `sparkles serve --result-cache-mb N` (default 512, `0` disables the cache); the cache is
  keyed by snapshot version, so updates invalidate it, and `POST /$/cache/clear/{ds}`
  empties it.

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

Non-2xx responses carry `{ "error": string, "detail"?: string, "line"?: number, "column"?: number }`
with `400` for parse errors, `404` unknown dataset, `408` timeout, `409` conflict, `503` for a
cancelled query, `500` otherwise.

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
  query or Graph Store GET response. For whole-dataset exports use `POST /$/backup/{ds}` or
  `sparkles dump`.
* `rows` (`--max-rows`, default 200,000,000): the rows of any intermediate result.

`limit` and `requested` are in bytes (rows for `rows`). The response of `/{ds}/update`
includes `memPeakBytes`, and `meta.memory.peakBytes` in `application/x-sparkles+json` reports
the peak estimate of a query.
