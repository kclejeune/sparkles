# C01: Observability, readiness, and byte/work budgets

> **Status:** implemented in part
>
> **Phases:** Phase 1 shipped in full. It covers request ids, the access log, Prometheus
> metrics, readiness, memory and result-size budgets, and cancel on disconnect. Part of
> Phase 2 shipped with it: the JSON metrics snapshot, the Server page panels,
> `meta.memory` and `requestId` in error bodies. Per-request budget overrides, the
> `rows_produced` work budget, `--shutdown-grace` and per-dataset storage quotas came
> later, and so did the Phase 2 metrics of §4.5 and `rows_produced` in the access log.
> From Phase 3, OpenTelemetry export with `traceparent` propagation, per-class
> concurrency caps and the `--metrics-addr` listener shipped, and Fuseki's metric names
> followed as an option. The rest of Phases 2 and 3 is not built. [Outcome](#outcome)
> has the details.
>
> **User docs:** [API: Server](../API.md#server) ·
> [API: Request ids and the access log](../API.md#request-ids-and-the-access-log) ·
> [API: Metrics](../API.md#metrics) · [API: Budgets](../API.md#budgets) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui) ·
> [Benchmarks: Full-text index and observability](../BENCHMARKS.md#full-text-index-and-observability-105m-triples)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

The spec touches three parts of the code. `crates/sparkles-server` gets the HTTP, CLI
and state changes. `crates/sparkles` gets the query context, error and store accessor
changes. `ui/` gets the server status view.

## 1. Summary

Operators have three tools today:

- `tower_http`'s `TraceLayer` with default settings. Its spans are at DEBUG, and the
  default `tower_http=warn` filter hides them.
- `GET /$/ping`, a liveness check.
- `GET /$/stats/{ds}`, an expensive JSON document per dataset.

Execution has two limits: the timeout, and a limit of 200M rows on intermediate tables
(`Ctx::max_rows`). The server does not expose the row limit. When a client disconnects,
its query keeps running in `spawn_blocking`. Each response is serialized into a
`Vec<u8>` with no size limit.

This spec adds five things:

1. Request ids and structured request logs, by extending the existing `TraceLayer`.
2. A bounded Prometheus endpoint, `GET /$/metrics`.
3. `GET /$/ready`, which reports readiness separately from liveness.
4. Two per-query budgets that fail with a structured error: estimated memory of
   intermediate results, and serialized response bytes. Both interact with
   cancellation, and client disconnects now cancel queries.
5. A server status view in the UI (Phase 2).

**Goals.**

- A request can be correlated by `X-Request-Id`.
- Metric cardinality is bounded by construction: no raw query, URI or unbounded
  identifier ever becomes a label.
- The defaults do not change `scripts/bench.sh` answers or timings.
- Phase 1 adds no crates. It only enables the `json` feature of `tracing-subscriber`.

**Non-goals.**

- OTLP or W3C `traceparent` (Phase 3).
- Exact allocator-level accounting.
- A server-wide memory pool or admission control (Phase 3).
- Authentication of `/$/metrics`. No `/$/` endpoint is authenticated today.
- Streaming responses, which are a separate feature. §4.4 says how budgets behave once
  responses stream.

## 2. User-visible behavior

### 2.1 Request ids

**Accepting an id.** An incoming `X-Request-Id` is used only if it is well formed:
1–128 bytes, all from `[A-Za-z0-9._:-]`. Otherwise the server replaces it without
comment. A malformed id is never an error.

**Generated ids** have the form `{boot:08x}-{seq:012x}`, for example
`5f3a9c1e-00000000002a`. `boot` is 32 random bits chosen per process, and `seq` is a
process-wide `AtomicU64`. The ids are cheap to make, unique for the life of the
process, and sort in arrival order.

**Where the id appears.**

- Every response carries `X-Request-Id`, including errors, 404s and UI assets. CORS
  exposes the header.
- The id is the `request_id` field of the request span, so every log event emitted
  while handling the request carries it.

### 2.2 Structured request log

**Which requests are logged.** Each completed request produces one INFO event with
target `sparkles::access`, emitted from the `TraceLayer` `on_response` hook. Requests to
`/ui/*`, `/$/ping`, `/$/ready` and `/$/metrics` are logged at DEBUG instead.

**Span fields.** The span has `request_id`, `method` and `route`. `route` is the matched
template, such as `/{ds}/sparql`. It is never the raw URI, because a GET query string
contains the query. The `uri` field of the default `DefaultMakeSpan` is dropped.

Event fields:

| field | type | notes |
|---|---|---|
| `dataset` | string | The name of an existing dataset, or `$none`. |
| `operation` | string | `query` `update` `gsp` `upload` `shacl` `explain` `admin` `other` |
| `status` | u16 | The HTTP status. |
| `outcome` | string | `ok` `client_error` `error` `timeout` `cancelled` `budget` (§4.2) |
| `rows` | u64 | For a query, the result rows (triples for CONSTRUCT and DESCRIBE). For an update, GSP request or upload, the quads changed. |
| `parse_ms` `plan_ms` `exec_ms` | f64 | Queries only. Taken from `sparql::Timing`. |
| `serialize_ms` | f64 | Measured in the handler. |
| `total_ms` | f64 | Wall time, measured in the middleware. |
| `response_bytes` | u64 | The uncompressed body size, when known. |
| `mem_peak_bytes` | u64 | Queries and updates. The estimated peak memory (§4.3). |

In the default text format an event looks like this, wrapped for width:

```
2026-09-30T12:00:00.123Z  INFO request{request_id=5f3a9c1e-00000000002a method=POST route=/{ds}/sparql}:
  sparkles::access: completed dataset=ds operation=query status=200 outcome=ok rows=10 parse_ms=0.12
  plan_ms=0.40 exec_ms=3.10 serialize_ms=0.22 total_ms=4.05 response_bytes=1532 mem_peak_bytes=4096
```

**Query text.** Query bodies are never logged at INFO or above. At DEBUG, query and
update handlers emit `debug!(target: "sparkles::query", query_len, query = <first 2048 chars>)`.
The text is cut at a char boundary. Enable it with `RUST_LOG=sparkles::query=debug`.

### 2.3 CLI flags

| flag | default | meaning |
|---|---|---|
| `--log-format text\|json` (global) | `text` | `json` uses `fmt().json().with_current_span(true).with_span_list(false)`, one object per line on stderr. `RUST_LOG` still filters. |
| `serve --no-access-log` | off | Drops `sparkles::access` events, like `sparkles::access=off`. |
| `serve --no-metrics` | off | `/$/metrics` returns 404. Nothing is recorded except the active count that readiness uses. |
| `serve --metrics-max-datasets N` | `100` | The maximum number of distinct `dataset` label values (§4.5). |
| `serve --query-memory-mb N` | `8192` | The estimated-memory budget per query. `0` means unlimited. It covers queries and the WHERE evaluation of updates. |
| `serve --max-result-mb N` | `1024` | The budget for the serialized body of query and Graph Store GET responses. `0` means unlimited. |
| `serve --max-rows N` | `200000000` | Sets the existing limit on intermediate rows (`QueryOptions::max_rows`). |

Sizes are in MiB to match the existing `--cache-mb` and `--result-cache-mb`. For the
same reason the flag is `--max-result-mb` and not `--max-result-bytes`.

`sparkles query` gains `--memory-mb N`, which is unlimited by default. On a budget
error it prints the message and exits with status 1.

The `serve` default filter is unchanged, because `sparkles=info` already covers
`sparkles::access`.

### 2.4 HTTP endpoints

| Method | Path | Response |
|---|---|---|
| GET/HEAD | `/$/metrics` | Prometheus text (§4.5) as `200 text/plain; version=0.0.4; charset=utf-8`, with `Cache-Control: no-store`. |
| GET/HEAD | `/$/ready` | A JSON `ReadyInfo`, with `200` when ready and `503` otherwise. `Cache-Control: no-store`. |
| GET/HEAD | `/$/ready/{ds}` | The same for one dataset. An unknown dataset gets `404`. |
| GET | `/$/ping` | The liveness check, unchanged. It returns `200` whenever the process serves HTTP. |
| GET | `/$/server` | Gains `"limits": {"timeoutSeconds","queryMemoryBytes","maxResultBytes","maxRows"}`. `0` means unlimited. |
| GET | `/$/metrics?format=json` | Phase 2. A JSON snapshot of the same registry for the UI, with `"formatVersion": 1`. |

```ts
type ReadyInfo = {
  status: "starting" | "ready" | "draining" | "degraded";   // "degraded": Phase 2
  ready: boolean;
  uptimeSeconds: number;
  datasets: {
    name: string; type: "persistent" | "mem";
    state: "open" | "opening" | "failed";                   // opening/failed: Phase 2
    ready: boolean;
    generation?: string;   // persistent only
    walBytes: number;      // 0 for mem
    deltaQuads: number;    // inserts + deletes not yet compacted
    error?: string;        // state == "failed"
  }[];
};
```

**Budget errors** all use `507 Insufficient Storage`:

```json
{ "error": "query exceeds its memory budget: needs about 1.6 GiB, limit 1.0 GiB",
  "budget": "memory", "limit": 1073741824, "requested": 1717986918 }
```

`budget` is one of:

- `memory`, in bytes.
- `result-bytes`, in bytes.
- `rows`, in rows. This replaces today's free-form `MemoryLimit` message and keeps its
  status of 507.

### 2.5 Rust library API (crate `sparkles`)

```rust
// error.rs: replaces Error::MemoryLimit(String)
#[error("{0}")] BudgetExceeded(Budget),
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)] #[serde(rename_all = "kebab-case")]
pub enum BudgetKind { Rows, Memory, ResultBytes }
#[derive(Clone, Copy, Debug)]
pub struct Budget { pub kind: BudgetKind, pub limit: u64, pub requested: u64 }
// Display: "intermediate result of {requested} rows exceeds the limit of {limit}" |
//          "query exceeds its memory budget: needs about {h(requested)}, limit {h(limit)}" |
//          "response exceeds the result size budget of {h(limit)}"

// sparql/mod.rs
pub struct QueryOptions { /* … */ pub max_memory_bytes: Option<u64> }   // None = unlimited
pub struct QueryResult  { /* … */ pub mem_peak_bytes: u64 }
// UpdateStats gains mem_peak_bytes (serde: memPeakBytes)

// sparql/results.rs: io::Write adapter; fails past `limit`, checks `cancel` every 64 KiB
pub struct LimitedWriter<W> { /* inner, limit, written, cancel, exceeded */ }
impl<W: Write> LimitedWriter<W> {
    pub fn new(inner: W, limit: Option<u64>, cancel: Option<Arc<AtomicBool>>) -> Self;
    pub fn written(&self) -> u64;
    /// Io error caused by the limit -> BudgetExceeded(ResultBytes); by cancel -> Cancelled.
    pub fn classify(&self, e: Error) -> Error;
}

// store.rs
impl Store { pub fn wal_bytes(&self) -> u64 }   // len of <root>/<gen>/wal.log; 0 for mem
```

`Ctx` gains `mem_limit: u64` (default `u64::MAX`), `mem_live: AtomicU64`,
`mem_peak: AtomicU64`, and the methods `charge` and `check_output` (§5.3).

### 2.6 UI (Phase 2)

The **Server** page (`ui/src/routes/server/+page.svelte`) already polls `/$/server`
every 15 s. It gains three panels above *Endpoints*, which poll `/$/ready` and
`/$/metrics?format=json` every 5 s while the tab is visible.

1. **Readiness.** The panel shows a status pill and one row per dataset with its state,
   generation, delta quads and WAL bytes. When the delta exceeds 1M quads or the WAL
   exceeds 256 MiB, a *Compact* button calls `POST /$/compact/{ds}`. The resulting task
   appears in the existing `TaskList`.
2. **Requests.** The panel shows one row for each (dataset, operation) pair with
   traffic, with these columns:
   - request rate: the counter delta between polls divided by the elapsed time;
   - error %;
   - p50 and p95 latency, estimated from the cumulative buckets by linear interpolation
     (the `histogram_quantile` method, about 20 lines of TypeScript);
   - active requests.
3. **Memory and caches.** For each dataset, the panel shows used and capacity bars and
   the hit ratio of the block cache and of the result cache. A *Clear result cache*
   button calls `POST /$/cache/clear/{ds}`. The panel also shows the process RSS and the
   configured limits.

The Query page changes in three ways:

- The result meta shows `meta.memory.peakBytes`, a new field in
  `application/x-sparkles+json`.
- Error toasts show the `X-Request-Id`.
- A 507 carrying `budget` renders as *"Result too large (limit 1 GiB). Add a LIMIT or
  narrow the query."*

Supporting changes:

- `ui/src/lib/api.ts` gains `ReadyInfo`, `MetricsSnapshot`, `ready()` and
  `metricsSnapshot()`.
- `ui/mock/server.mjs` gains both routes with canned data.

## 3. Standards basis

- **SPARQL 1.1 Protocol §2.1.7 and §2.2.5.** A syntax error is `400` and other
  failures are `500`. A service "may also return a 500 response code if they refuse to
  execute a query", and it "may use other 4XX or 5XX HTTP response codes for other
  failure conditions, as per HTTP". A budget refusal can therefore use a more specific
  5xx.
- **RFC 9110.**
  - `408` (§15.5.9) stays the timeout status.
  - `413` (§15.5.14) is about *request* content, so it is not used for response size.
  - `503` (§15.6.4) means temporary overload and comes with `Retry-After`. Sparkles
    keeps it for cancellation and draining, and reserves it for Phase 3 admission
    control.
- **RFC 4918 §11.5, `507`:** "unable to store the representation needed to complete
  the request". Sparkles already uses 507 for the row limit.
- **Prometheus text exposition format 0.0.4.**
  - `# HELP` and `# TYPE` lines come before samples.
  - Label values escape `\`, `"` and newline.
  - Histograms have cumulative `_bucket{le}` series ending in `le="+Inf"`, which equals
    `_count`, plus `_sum`.
  - Output ends with a line feed.
  - Counter names end in `_total`, and names use base units.
- **Apache Jena Fuseki.**
  - `/$/ping` is the liveness check. `/$/metrics` serves Prometheus text with JVM
    metrics and per-endpoint counters such as `Requests`, `RequestsGood`,
    `RequestsBad`, `QueryTimeouts` and `QueryExecErrors`, exported as
    `fuseki_requests_good` and so on. The counters carry the tags `dataset`,
    `endpoint`, `operation` and `description`. Sparkles reuses the label names
    `dataset` and `operation`. Aliases with Fuseki's names are an opt-in Phase 2 item.
  - Fuseki returns 503 for cancellation and timeout (`SC_QueryCancelled`). Sparkles
    keeps its documented 408 for timeouts.
- **Request ids.** `X-Request-Id` is a de facto convention. W3C Trace Context
  (`traceparent`) is Phase 3.

## 4. Semantics

### 4.1 Readiness

`AppState` gains `phase: AtomicU8` with the values `Starting`, `Ready` and `Draining`.

**Phase 1.** `AppState::new` and the `--mem`/`--loc` attaches still run before the
listener is bound. Every `Store::open`, including WAL replay, has therefore finished
before any request arrives.

- `main` sets `Ready` just before `axum::serve`.
- The shutdown future sets `Draining` first. It now fires on SIGTERM as well as SIGINT
  (`tokio::signal::unix`).
- The server is ready when `phase == Ready` and every registered dataset is `open`.
  `POST /$/datasets` registers a dataset only after `Store::open` succeeds, so
  registered datasets are always open.
- Compaction and backup do not affect readiness, because reads continue during both.
- The status is `503` whenever `ready` is false. The body is always `ReadyInfo`.

**Phase 2.** The listener binds first. Datasets open on a background thread, and
meanwhile readiness reports `starting` and `opening`. A dataset that fails to open no
longer aborts startup. It becomes `failed` and carries its error. The server is then
`degraded` and not ready, unless `--ready-ignore-failed` is set.

### 4.2 Outcome, operation, and dataset classification

Handlers and `ApiError::into_response` insert a `RequestReport` into the response
extensions. The middleware takes the first rule that applies:

1. The engine error was `Timeout` → `timeout`, `Cancelled` → `cancelled`,
   `BudgetExceeded` → `budget`.
2. The middleware future was dropped before completion → `cancelled`. This covers a
   client disconnect or a connection closed at shutdown. A drop guard records it.
3. Otherwise by status: below 400 → `ok`, 4xx → `client_error`, 5xx → `error`.

**Operation** comes from the matched route:

| matched route | operation |
|---|---|
| `/{ds}/sparql`, `/{ds}/query` | `query` |
| `/{ds}/data`, `/{ds}/get` | `gsp` |
| `/$/…` | `admin` |
| `/{ds}` | refined by the report of the handler it dispatches to |
| unmatched | `other` |

**Dataset** is the `{ds}` path parameter when `st.get(ds)` finds it, and `$none`
otherwise. `valid_name` forbids `$`, so the sentinel values `$none` and `$other` cannot
collide with real names.

### 4.3 Memory (work) budget

The budget limits the estimated live bytes of one query's intermediate results. It is
not an RSS limit. Each id counts as 8 bytes.

| structure | estimate |
|---|---|
| `Table` | `len × max(width,1) × 8 + 64` |
| operator output before materialization (join pairs, union, path, filter, cache hit) | `rows × out_width × 8` |
| hash-join build `FxHashMap<Id, Vec<u32>>` (Phase 2) | `build_rows × 4 + distinct_keys × 48` |
| composite-key maps/sets for group, distinct and minus (Phase 2) | `entries × (8k + 48)` |
| sort permutation (Phase 2) | `rows × 8` |
| property-path adjacency maps (Phase 2) | `edges × 16 + nodes × 48` |
| CONSTRUCT/DESCRIBE triples (Phase 2) | `Σ term string lengths + 3 × 56` per triple |

**Accounting (Phase 1).**

- `execute_uncached` holds a `Charge` guard for each child table, from the moment the
  child returns until the operator finishes.
- Every `check_rows(n)` site becomes `check_output(n, width)`. This checks the row limit
  and also that `mem_live + n × width × 8 ≤ mem_limit`. The check runs *before* the
  output is allocated, so a query over budget fails fast.
- `fetch_max` records the peak.

Execution in `exec.rs` is fully materialized. The count therefore approximates the
tables alive along the current execution path plus the output being built.

**Failure.**

- The query fails with `Error::BudgetExceeded { kind: Memory, limit, requested }`.
- Guards release on unwind (`?`), so tables are freed as the error propagates.
- The result cache is filled only after an operator succeeds, so failed subtrees are
  never cached.

**Scope.**

- Budgets are per request. Each `Ctx` of a request tracks its own usage against the same
  limit. This covers the WHERE contexts in `update.rs` now and SHACL-SPARQL in Phase 2.
- Concurrent queries have independent budgets. A shared pool is Phase 3.

**Work.** Phase 2 adds `rows_produced`, the sum of the operators' `actual_rows`, as a
measure of CPU work in the result, the log and the metrics. It is reported but not
enforced, because timeouts already bound CPU.

### 4.4 Result-size budget

**Enforcement.** The query handler and Graph Store GET serialize through
`LimitedWriter::new(&mut buf, max_result_bytes, cancel)`. The first write past the limit
fails with `507` and `budget: "result-bytes"`. Nothing has been sent yet, because
responses are buffered.

**What counts.** Uncompressed bytes, measured before `CompressionLayer`. The compressed
size depends on `Accept-Encoding`, while the uncompressed buffer is what uses memory.

**Early rejection.** Before serializing, the handler rejects the response when
`rows × (vars + 1)` exceeds the limit. That product is a lower bound for every solutions
format.

**GSP GET errors** point to `POST /$/backup/{ds}` or `sparkles dump` for full exports.

**Streaming (future feature).** With a streamed response, the status has already been
sent when the limit is hit. The server then aborts the body, and the chunked transfer
ends without its final chunk. The request is logged as `outcome=budget status=200`.
Fuseki handles late cancellation the same way.

### 4.5 Metrics

**Registry.** The registry is hand-rolled and uses only atomics on the request path.
Per-dataset metrics live in an `RwLock<BTreeMap<String, Arc<DsMetrics>>>`. The lock is
taken for writing only the first time a dataset is seen.

**Cardinality.**

- `dataset` takes at most `--metrics-max-datasets` real names, plus `$none` and the
  overflow value `$other`.
- `operation` has 8 values, `outcome` 6 and `budget` 3.
- Deleting a dataset removes its series. A recreated name restarts at zero, which
  Prometheus treats as a counter reset.
- A (dataset, operation) block appears after its first request. From then on all 6
  outcome series are emitted, zeros included.

**Histograms.**

- Buckets are fixed, in seconds: `0.001 0.0025 0.005 0.01 0.025 0.05 0.1 0.25 0.5 1
  2.5 5 10 30 60 300 +Inf`.
- Storage is a non-cumulative `[AtomicU64; 17]` plus `sum_nanos`, cumulated at scrape
  time.
- `_count` is the sum of the buckets, so `+Inf == _count` holds exactly under
  concurrency.

| name (Phase 1) | type | labels | source |
|---|---|---|---|
| `sparkles_build_info` | gauge = 1 | `version` | const |
| `sparkles_start_time_seconds` | gauge | – | `AppState` |
| `sparkles_ready` | gauge 0/1 | – | §4.1 |
| `sparkles_requests_total` | counter | `dataset,operation,outcome` | middleware |
| `sparkles_request_duration_seconds` | histogram | `dataset,operation` | middleware |
| `sparkles_requests_active` | gauge | `operation` (route-level) | middleware |
| `sparkles_result_rows_total` | counter | `dataset` | query reports |
| `sparkles_response_bytes_total` | counter | `dataset,operation` | reports (uncompressed) |
| `sparkles_budget_exceeded_total` | counter | `dataset,budget` | reports |
| `sparkles_dataset_quads` | gauge | `dataset` | scrape: `snapshot().len()` |
| `sparkles_delta_quads` | gauge | `dataset,kind=insert\|delete` | scrape: `delta.inserts()/deletes()` |
| `sparkles_wal_bytes` | gauge | `dataset` | scrape: `Store::wal_bytes()` |
| `sparkles_disk_bytes` | gauge | `dataset` | scrape: `Store::disk_bytes()` (walks a small directory) |
| `sparkles_block_cache_bytes`, `…_capacity_bytes` | gauge | `dataset` | `BlockCache::bytes()`, `StoreOptions::cache_bytes` |
| `sparkles_block_cache_hits_total`, `…_misses_total` | counter | `dataset` | `BlockCache` |
| `sparkles_result_cache_bytes`, `…_capacity_bytes`, `…_entries` | gauge | `dataset` | `ResultCache` |
| `sparkles_result_cache_hits_total`, `…_misses_total` | counter | `dataset` | `ResultCache` |
| `process_resident_memory_bytes` | gauge | – | Linux only: `VmRSS` from `/proc/self/status` |

Each dataset has a block cache and a result cache of its own, each sized to the global
limit. The capacity gauges show this. Scrape-time gauges for datasets beyond the cap are
summed into `$other`.

**Phase 2 metrics.**

- `sparkles_rebuilds_total{dataset,reason="compact|bulk"}`.
- `sparkles_rebuild_duration_seconds{dataset,reason}`, with buckets from 0.1 to
  3600 s.
- `sparkles_query_memory_peak_bytes{dataset}`, with buckets from 1 MiB to 16 GiB in
  steps of ×4.
- `sparkles_rows_produced_total{dataset}`.

The rebuild metrics come from a new `StoreMetrics`, updated in
`Store::rebuild_locked`.

**Size.** A scrape has about 60 lines per active (dataset, operation) block and about
20 per dataset. With 100 busy datasets it stays under about 1 MiB.

### 4.6 Cancellation

**On client disconnect.** The query handler creates `cancel = Arc<AtomicBool>` and
passes it as `QueryOptions::cancel`. Its async part holds a
`CancelOnDrop(cancel.clone())` guard. When the client goes away, hyper drops the
handler future, and then:

1. The guard sets the flag.
2. The blocking task stops at its next `ctx.check()` or `LimitedWriter` check. It frees
   its tables as its `Charge` guards unwind, and returns `Cancelled`, which is
   discarded.
3. The middleware drop guard counts the request as `cancelled` and emits the access
   event with `status=499`. This status appears only in the log and is never sent.

**On normal completion** the guard drops after the task has finished and has no
effect.

**Precedence.** Timeout, cancellation and budget checks run at the same points, and the
first to trip wins. `check()` runs before `check_output`, so a cancelled query is never
reported as `budget`.

**Updates** are not cancelled on disconnect in Phase 1. In Phase 2 they can be
cancelled before `WriteTxn::commit`. Once the commit has started, the update always
completes, so the WAL and generation switching stay untouched.

### 4.7 Defaults, compatibility, persistence

- **The defaults leave the benchmark unaffected.** Its largest response,
  `export-500k`, is under 100 MB of TSV. Its largest intermediate table, in
  `order-by-full`, is about 10M × 2 × 8 B = 160 MB at 10M people.
- **The library default stays unlimited.**
- **Row errors** keep status 507 and gain the JSON fields.
- **The access log** adds about 250 B of stderr per request (§9).
- **Nothing is persisted.** The generation, WAL and snapshot formats do not change.
  Metrics and readiness live in memory and reset on restart. The only new document is the Phase 2
  JSON snapshot, versioned with `"formatVersion": 1`.

## 5. Design sketch

### 5.1 Server

**New module `crates/sparkles-server/src/obs.rs`** (about 450 lines):

- `RequestId`: parsing and generation.
- `RequestReport { dataset: Option<String>, operation: Op, outcome: Option<Outcome>,
  rows, timing: Option<Timing>, response_bytes, mem_peak_bytes }`.
- `observe`, an `axum::middleware::from_fn_with_state`. In order, it does the following:
  1. resolves the id and writes it into the request headers;
  2. classifies the route (`MatchedPath` plus the `ds` param);
  3. increments `active` and arms a `Pending` drop guard;
  4. awaits `next`;
  5. merges the report, records metrics, disarms the guard, and sets the response
     header.
- `MakeSpan`, implementing `tower_http::trace::MakeSpan`:
  `info_span!("request", request_id, method, route)`.
- `AccessLog`, implementing `OnResponse`. It reads the report and emits the
  `sparkles::access` event.
- `Metrics`, including `render_prometheus(&AppState) -> String`.

**`http.rs::router`.** `.layer` wraps each route, so both layers can see `MatchedPath`.
The observe layer is outermost, so the request id exists before the span is created:

```rust
.layer(TraceLayer::new_for_http()
    .make_span_with(obs::MakeSpan)
    .on_request(())
    .on_response(obs::AccessLog)
    .on_failure(DefaultOnFailure::new()))   // keeps today's ERROR log on 5xx
.layer(axum::middleware::from_fn_with_state(state.clone(), obs::observe))
```

Other `http.rs` changes:

- The new routes are `/$/metrics`, `/$/ready` and `/$/ready/{ds}`. CORS adds
  `expose_headers([x-request-id])`.
- Handlers attach reports. `query_endpoint` reports rows, timing, serialize time, bytes
  and peak memory. `update_endpoint` reports the quads from `UpdateStats`. `gsp` reports
  the count or the bytes, and `upload` reports the count.
- `From<Error> for ApiError` builds the budget JSON. `ApiError` carries an
  `Option<Outcome>`, which `into_response` inserts.
- The two `Vec<u8>` writers on the query path become `LimitedWriter`s.
- `query_options` fills in `max_rows` and `max_memory_bytes`. The update path gets the
  same limits and `default_timeout`.

**`state.rs`.** `AppState` gains `phase`, `metrics: obs::Metrics`,
`limits: Limits { query_memory_bytes: Option<u64>, max_result_bytes: Option<u64>, max_rows: usize }`
and `access_log: bool`. The CLI sets them the same way it sets `read_only`.
`AppState::delete` calls `metrics.forget(name)`.

**`main.rs`.**

- `--log-format` switches between `fmt().json()` and text. The workspace
  `tracing-subscriber` features become `["env-filter", "json"]`.
- `main` handles the serve flags and the phase transitions, and adds SIGTERM to the
  shutdown future.

### 5.2 Metrics library decision

Three crates were candidates. The licenses are as crates.io listed them on 2026-09-30.

| crate | license |
|---|---|
| `prometheus` 0.14 | Apache-2.0 |
| `prometheus-client` 0.25 | Apache-2.0 OR MIT |
| `metrics` 0.24 + `metrics-exporter-prometheus` 0.18 | MIT; MIT AND Apache-2.0 |

All three have compatible licenses. But the metric set is small and fixed, the labels
are closed enums plus one bounded string, and rendering the text format takes about 80
lines.

**Decision:** hand-roll the registry in `obs.rs`. This adds no dependency and no global
recorder, and scrape-time gauges read `AppState` directly. If the set grows past about
30 families, or needs OpenMetrics features such as exemplars or `# EOF`, switch to
`prometheus-client`. Record the decision in [PROVENANCE.md](PROVENANCE.md).

### 5.3 Engine (`crates/sparkles`)

```rust
// ctx.rs
pub struct Charge<'a> { ctx: &'a Ctx, bytes: u64 }
impl Drop for Charge<'_> { fn drop(&mut self) { self.ctx.mem_live.fetch_sub(self.bytes, Relaxed); } }
impl Ctx {
    pub fn charge(&self, bytes: u64) -> Result<Charge<'_>>;              // add, fetch_max peak, undo+fail if > limit
    pub fn check_output(&self, rows: usize, width: usize) -> Result<()>; // row limit + live + rows*width*8 ≤ limit
    pub fn mem_peak(&self) -> u64;
}
```

- **`table.rs`:** add `Table::mem_bytes()`.
- **`exec.rs`:**
  - The `child` closure in `execute_uncached` pushes `ctx.charge(t.mem_bytes())?` into a
    local `Vec<Charge>`. The `execute_limited` child in `Slice` and the `Path` inputs do
    the same.
  - Every `check_rows` call becomes `check_output(n, width)`, with the width from
    `n.vars.len()` or `JoinLayout`.
  - New executor fast paths, such as the ones being added for incremental grouping and
    count joins, follow the same rule and check before any large allocation.
- **`cache.rs`:** `get` calls `check_output(e.len, e.cols.len())`.
- **`mod.rs` and `update.rs`:** `make_ctx` and `Request::ctx` copy `max_memory_bytes`,
  and `execute_query` sets `mem_peak_bytes`.
- **`results.rs`:** add `LimitedWriter`.
- **`store.rs`:** add `wal_bytes()`. It is a single `metadata` stat and takes no writer
  lock.

**Overhead.** `check_output` runs where `check_rows` already runs and adds one atomic
load. A `Charge` costs two atomic operations per operator, not per row. Confirm the cost
with `scripts/bench.sh` before merging.

### 5.4 Crash safety

Nothing new is persisted, and WAL replay and generation switching are untouched.
Readiness only reports state that `Store::open` has already established. A budget or
cancellation error in an update's WHERE evaluation happens before `WriteTxn::commit`,
so the delta and the WAL stay unchanged.

## 6. Phasing

### Phase 1: MVP, about one day with tests

1. In `obs.rs`: request ids, the `observe` middleware and its drop guard,
   `RequestReport`, the registry, and `/$/metrics` with the Phase 1 families from §4.5.
2. The `TraceLayer` `MakeSpan` and `AccessLog`, `--log-format json` and
   `--no-access-log`. Query text is logged only at DEBUG, truncated.
3. `/$/ready` and `/$/ready/{ds}`, `phase`, SIGTERM, and `limits` in `/$/server`.
4. Budgets:
   - `Error::BudgetExceeded`;
   - `Ctx` `charge`/`check_output`;
   - `max_memory_bytes` and `mem_peak_bytes`;
   - `LimitedWriter` on query and GSP GET;
   - the flags `--query-memory-mb`, `--max-result-mb` and `--max-rows`;
   - 507 response bodies.
5. Cancel on disconnect for the query endpoint.
6. `Store::wal_bytes`, and `docs/API.md` sections for the endpoints, the flags and the
   507 body.
7. Tests A1–A11 (§7) in `router_tests.rs` and `sparql/tests.rs`.

### Phase 2

- **Memory estimates.** Hash, sort, path and CONSTRUCT estimates, `rows_produced`, the
  peak-memory histogram, and `meta.memory` in the Sparkles JSON result format.
- **Rebuild metrics.** `StoreMetrics`.
- **Wider budget coverage.** Budgets for SHACL-SPARQL and `/$/reason`, and update
  cancellation before commit.
- **Per-request overrides.** `?memory-mb=` and `?max-result-mb=`. They can only lower a
  limit. Larger values are clamped.
- **Status UI.** `/$/metrics?format=json`, the UI in §2.6 and mock routes.
- **Startup and shutdown.** Bind-before-open startup with the `opening`, `failed` and
  `degraded` states. `--shutdown-grace S` keeps serving with `ready=503` for S seconds
  before stopping.
- **Errors.** `requestId` in JSON error bodies.
- **Fuseki compatibility.** An opt-in `--metrics-fuseki-compat` flag emits
  `fuseki_requests`, `fuseki_requests_good`, `fuseki_requests_bad`,
  `fuseki_query_timeouts` and `fuseki_query_execerrors`. They are labelled
  `dataset="/ds"`, `endpoint`, `operation` and `description`, and are derived from the
  same counters.

### Phase 3

- A server-wide query memory pool (`--total-query-memory-mb`) with admission control and
  a concurrency limit. When the pool is full, the server returns `503` with
  `Retry-After`.
- Streaming-response budgets (§4.4).
- `traceparent` propagation and an OTLP exporter.
- A separate metrics listener, `--metrics-addr`.

## 7. Acceptance examples

The examples use `server()` from `router_tests.rs`, which serves the dataset `ds`
loaded with the fixture `DATA` (9 default-graph triples). Set limits on the `AppState`
before building `router()`.

**A1. An incoming id is echoed.** `GET /$/ping` with `X-Request-Id: abc-123` returns
`200` with `X-Request-Id: abc-123`.

**A2. A malformed id is replaced.** For `X-Request-Id: has space`, and for a
129-character id, the response id matches `^[0-9a-f]{8}-[0-9a-f]{12}$`. Two requests
without an id get different ids, and the second has the larger `seq`.

**A3. Metrics.** Send three requests:

- `GET /ds/sparql?query=SELECT%20*%20WHERE%20%7B%3Fs%20%3Fp%20%3Fo%7D`
- `GET /ds/sparql?query=SELEKT` (400)
- `GET /nope/sparql?query=ASK%7B%7D` (404)

Then `GET /$/metrics` returns `200` with a content type that starts with
`text/plain; version=0.0.4`. The body contains:

```
sparkles_requests_total{dataset="ds",operation="query",outcome="ok"} 1
sparkles_requests_total{dataset="ds",operation="query",outcome="client_error"} 1
sparkles_requests_total{dataset="$none",operation="query",outcome="client_error"} 1
sparkles_request_duration_seconds_count{dataset="ds",operation="query"} 2
sparkles_result_rows_total{dataset="ds"} 9
sparkles_ready 1
```

The body also satisfies these checks:

- It contains none of `nope`, `SELEKT` or `?s`.
- For every histogram, the `le="+Inf"` bucket equals `_count`.
- It ends with `\n`.
- Each family has exactly one `# TYPE` line.

**A4. Health traffic is not counted.** After 5 × `GET /$/ping` and 3 ×
`GET /$/metrics`, no `sparkles_requests_total` series has `operation="admin"`.

**A5. Readiness.** `GET /$/ready` returns `200` (with `uptimeSeconds` ≥ 0):

```json
{"status":"ready","ready":true,"uptimeSeconds":0,
 "datasets":[{"name":"ds","type":"mem","state":"open","ready":true,"walBytes":0,"deltaQuads":0}]}
```

After `state.set_phase(Draining)`, it returns `503` with
`"status":"draining","ready":false`. `/$/ready/ds` returns `200`, and `/$/ready/zz`
returns `404`.

**A6. Memory budget.** Set `limits.query_memory_bytes = Some(1024)` and run
`SELECT * WHERE { ?a ?b ?c . ?d ?e ?f }`. That is 81 rows × 6 columns × 8 = 3888 B,
more than 1024, so the response is:

```
507 {"error":"query exceeds its memory budget: needs about …","budget":"memory","limit":1024,"requested":<n > 1024>}
```

The metrics then show:

```
sparkles_requests_total{dataset="ds",operation="query",outcome="budget"} 1
sparkles_budget_exceeded_total{dataset="ds",budget="memory"} 1
```

`SELECT * WHERE { ?s ?p ?o } LIMIT 1` under the same limit returns `200`.

**A7. Result budget.** Set `limits.max_result_bytes = Some(200)`.

- `SELECT * WHERE { ?s ?p ?o }` with `Accept: text/tab-separated-values` returns `507`
  with `"budget":"result-bytes","limit":200`.
- `ASK {}` returns `200`.
- With the limit set to `None`, the first query returns `200`.

**A8. The row limit keeps 507.** With `limits.max_rows = 5`, the A7 query returns `507`
with `"budget":"rows","limit":5,"requested":9`.

**A9. Engine unit test** in `sparql/tests.rs`. After the error, another query on the
same snapshot succeeds. Charges belong to one `Ctx`, so nothing leaks.

```rust
let o = QueryOptions { max_memory_bytes: Some(1024), ..Default::default() };
match sparql::query(snap.clone(), "SELECT * { ?a ?b ?c . ?d ?e ?f }", &o) {
    Err(Error::BudgetExceeded(b)) => assert_eq!((b.kind, b.limit), (BudgetKind::Memory, 1024)),
    r => panic!("{:?}", r.map(|r| r.len())),
}
let r = sparql::query(snap, "SELECT * { ?s ?p ?o }", &QueryOptions::default())?;
assert!(r.mem_peak_bytes >= 9 * 3 * 8);
```

**A10. Access log.** Install a `fmt().json()` subscriber that writes to a shared
buffer, using `with_default` on a current-thread runtime. Then run A3's first query with
`X-Request-Id: t-1`.

The buffer holds exactly one line with `"target":"sparkles::access"`. That line:

- has `"request_id":"t-1"`, `"dataset":"ds"`, `"operation":"query"`, `"outcome":"ok"`,
  `"rows":9`, and numeric `parse_ms` and `exec_ms`;
- contains neither `?o` nor the raw URI.

**A11. Cancel on disconnect.**

1. With `limits.query_memory_bytes = None`, spawn `app.oneshot(heavy)`, where `heavy`
   is a 5-way self cross product with `timeout=30`.
2. Abort the task after 50 ms.
3. Poll `/$/metrics` for up to 2 s.

Expect `outcome="cancelled"` = 1 and `sparkles_requests_active{operation="query"} 0`.

**A12. CLI (smoke test).**

- With `sparkles --log-format json serve --mem ds --port 0`, every stderr line parses
  with `serde_json`.
- `sparkles query --data x.ttl --memory-mb 0 '…'` behaves as it does today.

## 8. Rejected alternatives

- **tower-http's `SetRequestIdLayer` and `PropagateRequestIdLayer`.** They accept any
  incoming value without validation, so arbitrary bytes would reach the logs. The
  feature also pulls `uuid` into the server. Validating, generating and echoing ids
  takes about 30 lines of middleware.
- **Replacing `TraceLayer`.** It already provides the span plumbing and the 5xx failure
  logs, and the requirement was to extend it.
- **Logging query text at INFO**, as Fuseki does. It leaks data into logs, and the log
  grows with query size. The text is available at DEBUG instead, truncated and under its
  own target.
- **The `prometheus` crate, or `metrics` with an exporter.** Both bring a global
  registry or recorder and extra dependencies, and neither helps with a fixed metric set
  (§5.2).
- **Dataset names from the raw URL, or paths, as labels.** A flood of 404s would create
  unbounded series.
- **Allocator-level accounting** with an attributing or memory-limited allocator. It
  would be exact, but it costs something on every allocation. Attributing allocations
  across `rayon` workers (`map_rows`) also needs thread-local plumbing. The estimates
  catch the dominant cost, materialized tables, at negligible overhead. Revisit this if
  they prove too loose.
- **Other status codes for budget failures.**
  - `413` means the *request* is too large (RFC 9110 §15.5.14), so clients and proxies
    would misread it.
  - `503` signals a transient condition and invites retries, but a per-query budget is
    deterministic. Sparkles keeps `503` for real overload (Phase 3).
  - A `500` cannot be told apart from a bug.
  - `400` and `422` do not fit, because the query is valid and is refused for resource
    reasons.

  Sparkles already uses `507`. It is in the 5xx class that the SPARQL Protocol uses for
  refusals, and it is specific.
- **Truncating over-budget responses** and returning `200` with a warning header. That
  returns wrong answers without an error.

## 9. Open questions

1. **Access log default.** The log is on by default, like Fuseki's per-request INFO
   lines, so the benchmark log gets one line per request. The alternative is to turn it
   off by default and add an `--access-log` flag.
2. **Budget defaults.** The defaults are 8 GiB of memory and 1 GiB of result. A default
   derived from RAM, such as 50% of `MemTotal`, adapts better but is less predictable.
3. **Result budget on GSP GET.** A GSP dump over 1 GiB now fails instead of being
   buffered in RAM. That is safer, but it changes behavior. A streaming GSP GET would
   remove the conflict.
4. **Endpoint exposure.** Should `/$/metrics` and `/$/ready` stay unauthenticated on
   the main port, or move to `--metrics-addr`?
5. **Label sentinels and cap.** Are the sentinel values `$none` and `$other` and the
   cap of 100 the right choices?
6. **Fuseki names.** Should the Fuseki-compatible metric names be on by default? It is
   also unconfirmed whether Fuseki's "bad" counters count only client errors or all
   failures.
7. **Disconnect status.** The log-only `status=499` for disconnects is a de facto code.
   The alternative is to leave `status` out.
8. **Shutdown cancellations.** Should server-initiated cancellation at shutdown count as
   `cancelled`, or get a separate `aborted` outcome?

## 10. Sources

- **Sparkles repository** (read-only):
  - `README.md`.
  - `docs/API.md`: endpoint and error conventions.
  - `docs/AUDIT.md`: limits row.
  - `docs/BENCHMARKS.md`: result sizes and memory notes.
  - The project's feature order (internal planning notes).
  - `crates/sparkles-server/src/{main.rs,http.rs,state.rs,http/router_tests.rs}`.
  - `crates/sparkles-core/src/{error.rs,store.rs,index.rs}`.
  - `crates/sparkles-core/src/sparql/{ctx.rs,exec.rs,mod.rs,cache.rs,table.rs,results.rs,update.rs}`.
  - `ui/src/lib/{api.ts,app.svelte.ts}`, `ui/src/routes/server/+page.svelte`, `ui/mock/`.
  - `scripts/bench.sh`: only the Sparkles invocation and the query list were read.
- **Apache Jena** (Apache-2.0):
  - <https://jena.apache.org/documentation/fuseki2/fuseki-server-info.html>:
    `/$/ping`, `/$/stats`, `/$/metrics` and counter names.
  - Apache Jena source, under `jena-fuseki2/`:
    - `jena-fuseki-core/.../metrics/{FusekiRequestsMetrics,PrometheusMetricsProvider,FusekiMetrics}.java`:
      metric names and tags.
    - `server/CounterName.java`.
    - `servlets/{ActionExecLib,Responses,SPARQLQueryProcessor}.java` and `Fuseki.java`:
      `SC_QueryCancelled = 503`.
    - `jena-fuseki-main/src/test/.../TestMetrics.java`.
- **W3C SPARQL 1.1 Protocol**, <https://www.w3.org/TR/sparql11-protocol/> §2.1.7 and
  §2.2.5: failure status codes.
- **IETF** RFC 9110 §15.5.9, §15.5.14 and §15.6.4; RFC 4918 §11.5 (507).
- **Prometheus text exposition format**,
  <https://prometheus.io/docs/instrumenting/exposition_formats/>: content type,
  HELP/TYPE, escaping and histogram rules.
- **crates.io API** (2026-09-30): versions and licenses of `prometheus`,
  `prometheus-client`, `metrics` and `metrics-exporter-prometheus`.
- **Local cargo registry:**
  - `tower-http` 0.7.1 `Cargo.toml` and `src/request_id.rs`: `request-id = ["uuid"]`,
    and no validation of incoming ids.
  - `tracing-subscriber` 0.3.23 `Cargo.toml`: `json` pulls in `tracing-serde`, `serde`
    and `serde_json`.
- The project's earlier feature-review notes were not read.

## Outcome

**Executor extension in progress.** [X05](X05-streaming-query-execution.md) specifies
cursor-owned batches and state, cumulative limits across batches, and reporting at
response completion. Its opt-in SELECT cursor and HTTP integration are undergoing
validation; ordinary query APIs remain eager. X05 extends the budget contract without treating a batch
as a fresh row/work allowance or bounded serialization as bounded execution.

**Delivered.** Phase 1 landed on 2026-09-30 in four commits. The engine budgets and the
result-size writer came first. The server's request ids, access log, metrics, readiness
and budgets came next, and the Server page panels in the UI last. The Phase 2 UI work
came with them. It includes `/$/metrics?format=json` (`MetricsSnapshot`), the
readiness, request and cache panels that poll every 5 s, `meta.memory.peakBytes` in the
Sparkles JSON format, and request ids in UI errors. `requestId` in JSON error bodies
followed the same day.

**As designed.**
- The metrics registry is hand-rolled in `obs.rs`, with no metrics crate and no global
  recorder (see [PROVENANCE.md](PROVENANCE.md)).
- `--metrics-max-datasets` caps the dataset labels. `$other` and `$none` cover the
  rest.
- The access log is on by default, with `--no-access-log` and `--log-format json`.
- The budget defaults are 8 GiB of estimated memory and 1 GiB of result.
- A disconnecting client cancels its query, which is logged as `status=499`,
  `outcome=cancelled`.
- `Error::MemoryLimit` became `Error::BudgetExceeded(Budget)`. The existing full-text and
  vector caps now map to the `rows` and `memory` kinds, and still answer `507`.

**Deviations and later changes.**
- Memory accounting covered the optimized operators and cached results from the start,
  not in Phase 2. The optimized operators are range scans, count joins, incremental
  grouping, path frontiers and top-k.
- Graph Store GET does not share the 1 GiB result budget. Exports are streamed and have
  their own `--max-export-mb`, which is unlimited by default. The maintainer chose this,
  and it settles open question 3. Query bodies over 1 MiB are streamed as well, so the
  §4.4 streaming case is now the live behavior.
- Updates got their own `--update-timeout` instead of inheriting the query timeout. The
  maintainer chose no timeout by default.
- When authentication is on, `/$/metrics` needs the `metrics` permission, which answers
  open question 4. `/$/ready` stays open but lists only the datasets the caller can
  read.
- Later features added more budget kinds: `outbound-bytes`, `validation-work`, and a cap
  on decompressed bytes that answers `413`.
- Some Phase 3 work shipped. OpenTelemetry exports OTLP traces, metrics and logs,
  propagates `traceparent` in and out, and is off by default. Per-class concurrency caps
  in the rate limiter answer `503` with `Retry-After`.
- Fuseki's metric names arrived on 2026-10-02 behind `--metrics-fuseki-names`. Fuseki
  exports `fuseki_requests`, `fuseki_requests_good` and `fuseki_requests_bad` as gauges
  per dataset endpoint, labelled `dataset`, `endpoint`, `operation`, `description` and
  `application="fuseki"`, and Sparkles renders the same series. Good is the `ok` outcome
  and bad is any other outcome. Micrometer's uptime and processor gauges are rendered
  too, and the JVM gauges have no equivalent. The mapping is in
  [API.md](../API.md#fuseki-metric-names). This settles open question 6. The names are
  off by default, and Fuseki's source counts every failed request as bad, from a
  validation error to a cancelled query.
- `--metrics-addr` arrived the same day. The second listener serves only `/$/metrics`,
  behind the same authentication and `Host` check. Without auth it may bind a network
  address only with `--allow-open-network`, because the metrics name every dataset.
- Per-request overrides cover more budgets than planned. A query takes `memory-mb`,
  `max-rows`, `max-rows-produced` and `max-result-mb`, and an update takes the first
  three. Each value is clamped to the server's budget, so a request can only lower it.
  A malformed value is a `400`. The time budget stays the existing `timeout` parameter,
  which may ask for more than the default up to `--max-timeout`.
- `rows_produced` became a budget as well as a measure, with kind `rows-produced`. It is
  the sum of the rows every operator produces, counted where the executor already checks
  its output, so it costs one atomic add per operator. The WHERE clauses of one update
  share a count. `--max-rows-produced` is off by default, because the timeout already
  bounds the work of a query and the benchmark must not change. Results report
  `rowsProduced`. No other budget has a per-dataset setting, so this one has none
  either.
- `--shutdown-grace` (default 20 s) is a deadline rather than a delay. The server stops
  accepting at once and waits up to the grace period for requests in flight. Then it
  shuts the runtime down, which drops the handlers and so sets their cancellation flags,
  and waits up to 5 s more. A write cancelled this way commits nothing, and one already
  committing finishes. The NixOS module sets `TimeoutStopSec` to the grace period plus
  15 s.
- Per-dataset storage quotas were added beyond this spec, with the budget kind
  `dataset-bytes` and status `507`. `--max-dataset-mb` is the default, off unless set,
  and `quota.json` in a dataset's directory overrides it. Changing it needs
  `server-admin`. The size is the dataset directory on disk, write-ahead log included. It
  is walked at most once a second while a quota is set and after each rebuild, and small
  commits add their log bytes in between. Commits that add quads are refused before
  anything is written. Bulk rebuilds are checked after the build and before they are
  published. Deletes, compactions, reads and restores are never refused.

**Performance.** With access logging and metrics on, the 20 harness queries at 10.5M
triples differ by 0.6% from a run with `--no-access-log --no-metrics`. That is noise.
`ASK {}` and star-join throughput under `oha` do not change. The numbers are in
[Benchmarks](../BENCHMARKS.md#full-text-index-and-observability-105m-triples).

**Phase 2 metrics** (2026-10-03). The four metrics of §4.5 landed, with
`rows_produced` in the access log.
- `sparkles_rows_produced_total{dataset}` sums the `rows_produced` of queries and
  updates, and the access log line of each has a `rows_produced` field. Both come from
  the request report, which now carries the count the engine already returned.
- `sparkles_query_memory_peak_bytes{dataset}` is a histogram of the estimated memory peak
  of each query, with buckets from 1 MiB to 16 GiB in steps of ×4. Updates are not
  observed, because the name says query. A dataset has the series once it has a query.
- `sparkles_rebuilds_total{dataset,reason}` and
  `sparkles_rebuild_duration_seconds{dataset,reason}` count the generations a store
  published, with `reason` `compact` or `bulk`. The spec put a `StoreMetrics` in
  `Store::rebuild_locked`, but compactions no longer go through it, since they build in
  the background in `compact_with`. The counters live in the compaction tracker that the
  store already keeps, which records a bulk commit where it marks the generation's new
  base and a compaction where it switches to the new generation. `Store::rebuilds(reason)`
  reads them. Dry runs, abandoned compactions and failed builds publish nothing and count
  nothing. The counts are in memory and start at zero when a dataset is opened. The
  server sums them into `$other` like its other scrape-time series.
  The `compact` reason counts the same compactions as
  `sparkles_compactions_total{outcome="done"}`, which C13 added. It is kept because it
  shares the rebuild histogram with bulk commits.
- `obs_tests::metrics_count_work_memory_peaks_and_rebuilds` and
  `compaction_tests::rebuilds_are_counted_by_reason` cover them, and the access log test
  checks the new field. The JSON snapshot of `/$/metrics?format=json` does not have the
  new series, and neither do the OTLP metrics, which are read from that snapshot.

**Not built.** These parts were not built:

- bind-before-open startup with the `opening`, `failed` and `degraded` states, which
  changes how the server's state attaches datasets;
- a server-wide query memory pool;
- the new Phase 2 metrics in the JSON snapshot and as OTLP metrics.
