# X05: Streaming query execution

> **Status:** implemented in part (Phases 1, 2 and 4, and Phase 3 without disk spill).
> Growing state fails explicitly when it exceeds the query budget, because disk spill
> was not built. Eager execution remains the default, and automatic selection is
> restricted to measured eligible cases. On some workloads, explicit streaming takes
> longer than eager execution to deliver a complete response.
>
> **Phases:** Phase 1 adds opt-in SELECT cursors with resumable scans and unary
> operators. Phase 2 integrates result writers and HTTP with backpressure. Phase 3 adds
> incremental joins, budgeted state for blocking operators and disk spill. Phase 4 adds
> graph queries, binding integration and measured automatic selection.
>
> **User docs:** [API](../API.md#applicationx-sparklesjson-ui-result-format),
> [Usage](../USAGE.md#incremental-query-execution). Existing query APIs remain eager.

## 1. Purpose and scope

Sparkles must be able to return a large answer without retaining its complete result
table. Bounded response serialization alone cannot do this, because the original eager
query runs to completion before the writer sees its first row. Paging the final table
is not enough either, because intermediate tables can be larger than the final answer.

The design adds synchronous pull execution over immutable snapshots, in which any pull
can fail. Operators produce bounded batches of columnar IDs and keep only the state
they need to resume. The serializer consumes batches directly, and bounded transport
buffers make a slow reader stop execution upstream. The existing columnar executor
remains available for small queries and for operators that lack an incremental
implementation at first.

**Goals**

1. Return the first batch before scanning or building the complete answer where the
   algebra permits it, and stop promptly at LIMIT, on cancellation or when the consumer
   closes the cursor.
2. Preserve SPARQL multiplicity, unbound values, expression errors, ordering, graph
   visibility, blank-node identity, RDF-star and the existing semantics of query
   extensions.
3. Account for live batches, retained operator state and query-created terms. Fail with
   a structured budget error rather than silently collecting unbounded state.
4. Make materialization barriers and memory growth visible in plans. Producing output
   incrementally and working in constant memory are separate properties.
5. Preserve current small-query performance and the APIs that return collected
   results. Widen automatic selection only after matched measurements of correctness,
   latency and memory.

**Non-goals of the initial phases**

- Streaming UPDATE publication or changing commit durability.
- A universal constant-memory algorithm for every query. Some operators need growing
  state, full-input processing, external storage or an explicit budget failure.
- A server-wide query admission pool, distributed execution, or persistent cursors that
  a later HTTP request resumes.
- Changing RDF loading thresholds, result syntax or query semantics to suit batching. A
  caller that collects every batch still retains the complete answer.

## 2. Existing engine and compatibility

The design starts from the code at commit `a6b6b449`. The table lists the components
it changes and how each relates to X05.

| Component | Current behavior | Required relationship to X05 |
|---|---|---|
| `sparql::query` / `execute_query` | Parses and plans the query, then returns a complete `QueryResult` | Stays compatible and shares semantic preparation with the cursor |
| `exec.rs` / `Table` | Materialized columnar operators. Some LIMIT paths repeat increasingly large prefixes. | Stays as the eager executor and the initial fallback. Cursor scans resume instead of replaying prefixes. |
| `Snapshot::scan_between_cols` | Traverses with a callback, merging immutable blocks with delta inserts and deletes | Provides resumable traversal with the same visibility and ordering |
| `Ctx` | Holds the query-local vocabulary, decoding caches, limits, cancellation, extensions and access view | Keeps the correct lifetime and adds the accounting that cursors need |
| `results.rs` | Serializes completed results through writers | Gains a consumer of cursor batches |
| HTTP `SwitchWriter` | Bounds serialized bytes after a small body buffer | Connects producer backpressure to execution and keeps controls in force until the body completes |
| `Dataset::select` | Returns fully decoded `Solutions` | Keeps its contract and gains an explicit cursor API |

Cursor and eager execution share parsing, scope and depth validation, initial
bindings, SELECT variable order, dataset defaults, the protocol dataset, the RDFS and
materialized-inference overlays, access restrictions and extension resolution. These
preparation steps are factored out where needed, so that query options have a single
interpretation. A historical query captures the resolved historical snapshot along with
its vocabulary and index eligibility, as in [F06](F06-snapshots-and-point-in-time.md).

Existing APIs keep their eager default through Phases 1–3. A cursor's explicit
collection method may produce the same result representation, but replacing the
existing eager implementation is a separate decision gated on performance. Phases 1
and 2 require no format change, generation migration or new dependency.

## 3. Cursor API and lifetime

### 3.1 Public contract

The initial API accepts SELECT only. The following names describe the intended surface.
The implementation may refine type names as long as it keeps these contracts.

```rust,ignore
sparql::select_cursor(snapshot, query, query_options, cursor_options)
    -> Result<QueryCursor>
Dataset::select_cursor(query) -> Result<QueryCursor>
Dataset::select_cursor_with(query, query_options, cursor_options)
    -> Result<QueryCursor>

QueryCursor::variables(&self) -> &[Variable]
QueryCursor::plan(&self) -> &CursorPlan
QueryCursor::next_batch(&mut self) -> Result<Option<QueryBatch>>
QueryCursor::status(&self) -> CursorStatus
QueryCursor::stats(&self) -> CursorStats
QueryCursor::close(&mut self)
QueryCursor::collect(self) -> Result<QueryResult>
```

Opening a cursor captures a snapshot, the options and the immutable extension
registry, validates and plans the query, and determines its output columns. It does not
execute a whole fallback subtree. Probes and samples taken during planning obey the
existing budgets and cancellation, so strict incremental mode cannot hide full
execution inside planning. The SELECT API rejects ASK, CONSTRUCT, DESCRIBE and UPDATE
before producing rows. These forms keep their existing APIs until Phase 4.

`CursorOptions` supplies a maximum row count per batch, a byte target for the ID buffer
and a fallback policy (`AllowMaterialization` or `RejectMaterialization`). A zero limit
fails at open. The initial defaults are 4,096 rows and a 1 MiB ID-buffer target,
subject to the remaining query memory budget. These defaults are engineering starting
points. They are not measured optimal values, and they are not the loader's byte
thresholds. A batch never exceeds the row cap. A batch holds at least one row even when
that row exceeds the byte target, as long as the query memory budget permits it. A
single row that exceeds the budget fails rather than being truncated. Decoded values of
variable width are accounted separately (§5).

While the cursor is Open, `None` means successful exhaustion. Failed and Stopped
cursors are terminal and also return `None` on later pulls, because pulls are fused
(§3.2). The terminal status therefore tells the caller whether the answer is complete.
The cursor never returns an empty batch to signal a temporary lack of output, so a
filter may consume several input batches to produce one output batch. Solutions with
zero columns carry an explicit row count, including duplicates.

### 3.2 Ownership and state

The cursor owns its captured snapshot and execution state, without borrowing a live
Dataset handle or holding a writer lock. Commits and compaction after open cannot
change its answer. Dropping the Dataset handle does not invalidate a live cursor.
Snapshots and returned batches keep their generation's memory and open files until
their last owner drops. Generation collection does not wait for them. When history no
longer needs a generation, collection removes its directory even while a cursor still
reads it. The cursor keeps working because Unix keeps a removed file's data for the
processes that have it open or mapped, and the disk space returns when the last owner
drops. This relies on Unix file semantics. A platform that cannot remove open files
would need collection to defer the generations that live snapshots read.

The cursor can move between threads (`Send`) and takes sequential pulls through
`&mut self`. Concurrent pulls are not supported. Each pull and each guarded teardown
keeps the algebra's stack-depth protection and the callback-family and failure
boundaries of [P03](P03-query-extensions.md), including when it runs on a different
thread.

A `QueryBatch` owns immutable columns and a charge on the shared query budget. It keeps
the term resolver that its IDs need, so it stays readable after the cursor closes or
drops. The public accessors expose variables, rows and decoded RDF terms, but no
context or storage handle that could bypass access restrictions. A borrowed term view
lives only as long as the batch, and owned terms that application code copies count as
application memory. Unbound cells are explicit and distinct from valid RDF terms.

The state transitions are:

```text
Open -> Complete              root exhausted, including its query LIMIT
Open -> Stopped               explicit close or a consumer output cap
Open -> Failed(error)         execution, timeout, cancellation or budget failure
```

Successful exhaustion and terminal errors release operator buffers and temporary
resources. Batches returned earlier keep only the ownership they need. After a terminal
error, `next_batch` is fused. It returns `None` on later calls, and the failure stays
recorded. `collect` refuses a Failed or Stopped cursor, and it refuses a cursor that
has already delivered a batch to another consumer. Otherwise it drains the cursor and
collects the complete answer. It cannot recover rows yielded earlier, and it cannot
return only the remainder as a successful complete `QueryResult`. The Python cursor
wrappers differ at this point because Python's iteration protocol cannot carry a
status. After a failure, each later `next()` raises an error that names the original
failure, so a loop cannot end as if the answer were complete. Close is idempotent and
preserves a prior Complete or Failed state. Reading stats never drains the cursor. The
cursor may know it is complete when it returns the last batch, so it need not wait for
an extra pull once the root has proved exhaustion.

Dropping or closing a cursor releases its production state without setting a shared
cancellation flag that the caller supplied, since that flag may control other queries.
It does not drain input. The core API has no background producer. Retained batches stay
charged, and they can keep snapshot and resolver resources alive after close.

Phase 1 does not expose cursors that escape an open write transaction. Such an API needs
an explicit design for lifetimes and execution families, plus owner guards in the
foreign-language bindings. It must not acquire a writer lock that the caller already
holds, and it must not continue callbacks after the transaction is torn down. Existing
query behavior inside transactions is preserved.

## 4. Physical execution

### 4.1 Resumable scans and unary operators

Phase 1 pipelines Empty and VALUES, ordinary index scans, supported range scans,
FILTER, BIND (Extend), projection, Slice with OFFSET and LIMIT, and UNION when each
child is supported. Existing index-order guarantees may carry through. A specialized
ordered top-k path qualifies only if it resumes without collecting or replaying its
full input. An operator's name alone does not make it eligible.

Scan state keeps its range position, any pending position within a block or the delta,
the graph selection, the equality checks for repeated variables and the duplicate state
of a merged default graph. This state survives a batch boundary, including a boundary
inside an immutable block or inside a run of equal triples across graphs. Resuming
neither skips nor repeats a key. Resuming by seeking is acceptable at first if it keeps
the full ordering key, handles the inclusive bound and the maximum key correctly, and
passes the performance gates. Undecoded columns cannot supply an ordering key, so a
selective-decode optimization must carry enough real key data to resume correctly.

Operators never restart the query with a larger LIMIT to get the next page. OFFSET
consumes and discards input without retaining it. LIMIT stops its child as soon as it
has the rows it needs. UNION preserves multiplicity and opens the next arm only when
needed. Projection keeps row counts when there are zero columns. Filters and
extensions reuse the existing expression semantics, including the handling of unbound
values and errors and short-circuit evaluation. NOW is fixed for the query, and batch
size cannot change the behavior of volatile functions, BNODE or callback invocation.

Long loops check cancellation and the deadline on entry, on exit and at least every
1,024 examined candidates, counting candidates that produce no visible row. This is a
work checkpoint. It does not promise that blocking I/O or user callbacks finish within a
fixed wall time.

### 4.2 Capability and fallback visibility

Each physical node records its production mode (incremental or materialized), whether
its first output needs its full input, the state it retains and the reason for any
barrier. The retained state is fixed buffers, growing budgeted state or spill. A
supported external sort may consume all of its input before emitting incremental output
without materializing a complete table in RAM. Its blocking startup is reported
separately from eager fallback.

The native ORDER BY reads cursor batches into charged state, sorts them and then
emits batches. Its plan node reports that it needs its full input before output and
gives the reason, while `materializes` stays false because no eager subtree runs. The
policy that rejects materialization therefore admits it. An ORDER key that evaluates
EXISTS runs subqueries outside the cursor, so that sort counts as materialization.
The root plan summarizes whether any subtree materializes and whether state can grow
with input or result cardinality. Runtime counts reported before completion are marked
as partial.

`AllowMaterialization` runs an unsupported subtree lazily through the current budgeted
executor when the subtree is first demanded, then serves batches from the retained
result. It charges that result and any copied batch at the same time. The barrier is
visible in the plan before the first pull. FILTER, EXISTS, LATERAL, SERVICE and
registered callbacks are also barriers when their implementations retain whole
subresults, and a streaming parent cannot conceal them.

`RejectMaterialization` rejects a known unsupported subtree before any output. A
subtree that is planned dynamically is rejected when execution reaches it, before it
runs, and the initial plan shows that a dynamic barrier is possible. This policy
forbids fallback, but it does not forbid all growing state. Generated terms or a
supported DISTINCT may still grow under a budget, so such a plan must not be labelled
constant-memory. VALUES supplied by the caller and the query syntax itself are state
sized by the input, and they are reported separately.

### 4.3 Joins, state and blocking operators

Phase 3 extends coverage in steps that can each be reviewed on their own.

| Operator family | Incremental design | State or barrier to expose |
|---|---|---|
| Ordered or merge join | Resume both inputs and emit bounded products | Runs of equal keys can be arbitrarily large, so they are budgeted or spooled |
| Index or nested-loop join | Keep the driving row or batch and the probe position | Bound probe batches and preserve repeated driving keys |
| OPTIONAL (LeftJoin) | Resume matches, and emit the unbound extension only after proving that no qualifying match exists | ON-expression rules, unbound keys and unmatched state |
| MINUS, EXISTS and semijoin | Probe or build eligible key state | Compatibility and domain rules, with correlated fallback where needed |
| Hash join | Budget the build side and stream probes. Partition or spill where supported. | Build bytes, skewed partitions and output products |
| ORDER BY and top-k | Budgeted run generation and merge, with a bounded heap for an eligible LIMIT | A full-input barrier unless index order proves early termination |
| DISTINCT and REDUCED | Budgeted exact keys, with partitioning or spill for DISTINCT | DISTINCT holds across batches, and REDUCED keeps its permitted semantics |
| GROUP and aggregates | Budgeted groups or ordered aggregation, spilling eligible state | Empty groups, DISTINCT aggregates and accumulator ownership |
| Paths, text, vector, geo and SERVICE | Keep the specialized algorithms where compatible | Frontiers, candidate sets, remote pages and existing extension state |

Rewrites need the existing algebra and scoping proofs. A large run of duplicate join
keys, unbound wildcard keys or a skewed hash partition must not allocate outside the
budget. Until a spill algorithm is accepted, an implementation may fail with a budget
error instead of spilling. Unsupported custom accumulators and property functions stay
behind an explicit barrier. When a P03 expression error is local to one invocation, the
provisional rows of that discarded invocation must not have been published already.

Spill needs reserved disk bytes per query and across the server, safe ownership of
temporary files, cleanup on cancellation and error, and a bounded merge fan-in. Running
out of disk is a structured resource failure. Crash cleanup removes only files that are
positively identified as query temporaries. Spill cannot write plaintext values from an
encrypted dataset. It must use suitable ephemeral encryption under
[F11](F11-encryption-at-rest.md), or be disabled for that dataset so that queries fail
within their memory budget. File formats, reservation configuration and recovery
details need an implementation design before Phase 3 spill lands, and this spec does
not authorize unrestricted growth of temporary disk use.

## 5. Budgets and accounting

This section extends [C01](C01-observability-and-budgets.md) and does not weaken
existing limits.

| Resource | Cursor interpretation |
|---|---|
| `max_rows` | Cumulative output rows of each physical operator across all its batches. It is not a fresh allowance for each batch. |
| `max_rows_produced` and shared `work` | Sum of rows produced by physical operators, kept on the existing shared request counter |
| `max_memory_bytes` | Estimated query-owned capacity and retained data that are live at the same time, including outstanding batches |
| Result-byte budget | Serialized uncompressed bytes actually written, independent of row and batch counts |
| Timeout | One absolute query deadline. Consumer waits and transport backpressure do not reset or suspend it. |
| Cancellation | Checked during production, decoding, serialization and interruptible transport waits |

Counters use checked or saturating arithmetic and fail on overflow. Counts are updated
as work happens, so an early stop or a failure keeps the partial work. For a fixed
physical plan, splitting batches differently must not change the counts, must not
double-count ownership transfers, and must not let an unlimited stream through as a
series of batches that each stay under the limit. Eager and cursor plans can
legitimately do different physical work, so semantic parity does not require identical
counters. In access-filtered plans and reports, checks of hidden candidates do not
reveal hidden cardinalities.

Allocated column capacity is charged before it grows, including the overhead of
zero-column rows. Accounting covers parent and child batches, materialized fallback
tables, hash, group, deduplication and path state, sort buffers, decoded terms,
expression caches, blank-node ledgers and the query-local vocabulary. A BIND that
produces a different string per row can grow the vocabulary even with tiny batches.
Phase 1 must charge that growth and fail on it, or implement safe reclamation. It cannot
defer the issue while claiming bounded query memory. Live batches keep their local
terms valid. Cached decoded values may be released between pulls only when that
preserves identities and observable function behavior.

Batches that the caller retains stay on the same shared budget. Moving a buffer
transfers its charge, and copying keeps both charges until the original is released.
Collecting explicitly charges the growing final table and the batch in transit, and it
fails if they do not fit. Partial tables are released on failure. `collect` never
disables limits to emulate the old API.

The accounting remains an estimate, not an allocator that attributes every byte.
Shared mapped indexes, the vocabulary and the decoded-block cache are reported
separately from the memory and RSS that the query owns. A cursor that pins them does
not turn them into per-query memory. Transport and compression buffers have their own
bounded capacities. A very large RDF term may exceed a query budget by itself, and row
batching does not split or truncate a term. Measurements disclose unknown allocator and
transient overhead, and that overhead must not hide an unaccounted collection that
grows with the input.

## 6. Serialization, HTTP and observability

Phase 2 adds writer consumers for the standard SELECT JSON, XML, CSV and TSV formats.
Each writes the header, then decodes and serializes a bounded batch and releases it
before demanding more. The rich Sparkles JSON format follows. It writes results first
and final metadata after successful exhaustion, because the final timing, count and
plan fields are unknown at open. Jena's binary formats need their own cursor adapters
before they are supported.

HTTP uses the explicit query parameter `execution=eager|streaming|auto`. Omitting it
means eager, and invalid values are rejected. The streaming mode permits visible
budgeted fallback, as in §4.2, and does not guarantee constant memory. When streaming
is requested explicitly, an unsupported query form or negotiated encoding is refused
with an actionable error before execution, rather than silently running eagerly.

The `auto` mode streams only the plans that the measured admission rules accept, which
the API documentation lists, and runs every other query eagerly. It also runs eagerly
when the negotiated encoding has no cursor writer. SPARQL Results Thrift is such an
encoding, so `auto` with Thrift returns an eager answer while `streaming` with Thrift
is refused. The initial phase supports SELECT and the four standard encodings, and
native JSON is enabled only once its adapter is complete. The OpenAPI description and
the API and usage docs are updated when the option lands. Authorization, historical
snapshot resolution, substitutions and dataset defaults follow the existing handler.

The snapshot's dataset and commit identity and its history headers are available
before output. The exact total row count, final execution timings and peak memory are
not. A small answer may still be returned as a whole body through the existing bounded
switch writer. Once the body crosses its byte threshold, a bounded channel connects the
blocking producer to the body. The current 1 MiB switch threshold and the 64 KiB,
four-chunk buffering can be reused, but they are not a query batch size. No unbounded
row queue or full-result cache sits between execution and transport.

The producer does not retry a cursor that has already advanced after a serialization
trial. Streaming uses a single-use writer path, and the repeatable quick-result
serializer remains for eager results. The producer can run ahead by a finite amount of
queued bytes and one batch in progress. Once those fill, it stops pulling. Channel waits
observe cancellation and the deadline and never wait indefinitely for another write.
When the receiver drops, blocked sends wake up and the producer is released. The
disconnect guard belongs to the lifetime of the body and the producer, not only to the
handler future, so returning response headers cannot cancel the running query.

Before response headers are committed, parsing, permission, budget and execution errors
use the existing HTTP status and error representation. After commitment, a failure
aborts the body transport and omits the success terminator and final metadata. The
server does not append an error object inside a standard result document, and it does
not end a failed CSV or TSV stream with a normal EOF. The response status can no longer
change at that point. Callers that use a writer directly receive `Err` and must treat
any bytes already written as partial output.

The existing `send` output cap may close the cursor after the requested prefix. This
is an intentional Stopped result, distinct from a root SPARQL LIMIT that completes the
query. The standard result document can close successfully, but the remaining total is
unknown, and native metadata and completion logs identify the cap. The cursor is not
drained just to fill in a count. If the cursor already finished successfully, a
serialization or output failure is recorded separately from execution completion.

Stats report parse and plan time, active execution time, emitted rows, physical work,
peak accounted bytes and the terminal state (Complete, Stopped or Failed). Active
execution time accumulates operator evaluation during pulls, separately from planning,
and excludes consumer idle time and blocked response writes. Total elapsed time runs
from open to termination and includes waiting. Serialization time and transport waits
are measured separately. The final access-log entry, metrics and span records are
emitted exactly once, when both execution and the response end, so an early response
header cannot carry an invented final report. Existing plan redaction applies to every
capability, estimate and runtime field.

Cursor mode bypasses the store result cache at first. It must never insert a partial
answer or replay callback effects through retries. A later cache of complete results
needs the same isolation by registry, access and snapshot as eager execution, and its
materialization cost must be visible.

## 7. Later query forms and bindings

ASK consumes only the input it needs and stops after a qualifying solution, with the
same rules for truth values and errors. CONSTRUCT consumes WHERE batches but still
needs exact deduplication of triples and quads, and blank-node identity per solution in
the template. DESCRIBE needs a budgeted traversal and keeps its existing truncation
policy. These forms use their own cursor result types, and the SELECT-only types are
not stretched to imply graph support.

N-Triples, N-Quads and Turtle-style writers may consume graph batches once their
adapters are correct. Subject grouping in RDF/JSON and ordinary JSON-LD may need
buffering or a spool. The plan identifies those barriers rather than calling every
writer constant-memory.

Python iteration, Node async batches and JVM iteration through Jena should consume the
native cursor rather than split an answer that was already collected. Each binding
defines early close and finalizers, ownership on abort and disconnect, callback
threading and transaction-owner guards. The remote streaming client in
[P02](P02-rust-client.md) is a transport consumer and does not make native execution
incremental. The parity tables in P01, P04, P05 and P06 are updated when their cursor
surfaces ship. The CLI and the stored-query runners can use the same writer adapters
without creating another engine.

## 8. Delivery phases

| Phase | Deliverable | Landing gate |
|---|---|---|
| 1 | SELECT cursor API, shared preparation, resumable scans and unary operators, visible fallback, and full accounting of cursor-owned memory and lifetimes | A1–A8 and A11. Eager defaults unchanged. Cursor and eager performance and memory measured. |
| 2 | Standard SELECT writers, an opt-in HTTP mode, disconnect handling, backpressure and final reporting. The native JSON adapter lands separately. | A9–A10 and A12. API and OpenAPI docs. No replay. Paired measurements of small and large outputs. |
| 3 | Incremental join families, stateful operators and accepted spill strategies, each in its own slice | A13, plus the earlier cases each operator affects. Skew, error, disk and encryption gates before spill is enabled. |
| 4 | ASK and graph cursors, binding and CLI integration, then optional automatic selection | A14. Binding lifecycle tests and complete performance evidence across scales. |

Automatic selection waits for measurements. It can use plan barriers, estimates,
observed execution and serialization costs, and memory risk. A guessed row-count
threshold cannot predict wide rows or huge literals. An explicit eager mode stays
available for comparison and rollback. Any change of the default needs its own
documented acceptance decision.

## 9. Acceptance examples

- **A1 — Early production:** over a million-row scan, obtain one batch without
  visiting the entire input. LIMIT 1 stops once it has enough qualifying rows. OFFSET
  and a low-selectivity FILTER stay incremental and cancellable, including when nothing
  matches.
- **A2 — Resume correctness:** compare eager and cursor bags with delta inserts and
  deletes, duplicates from a graph union, repeated variables and batch boundaries
  inside blocks and runs. Include the maximum ordering key and a final batch smaller
  than the cap.
- **A3 — Result semantics:** compare SELECT variable order, initial bindings,
  duplicates from VALUES and UNION, unbound values from BIND and FILTER, empty results
  and duplicate zero-column solutions. Compare ordered sequences only when the query
  semantics require an order.
- **A4 — Terms and functions:** at several batch sizes, verify RDF-star and composite
  literals, blank-node identities that the query creates, NOW and volatile functions,
  short-circuit errors, and the P03 rules for callback invocation, fatal errors and
  teardown.
- **A5 — Snapshot ownership:** mutate and compact the store after open, drop the
  Dataset handle, and finish the original answer. Keep a batch after close and decode
  its local terms. Generation resources are released after every owner drops.
- **A6 — Terminal behavior:** cover exhaustion, repeated close, a consumer stop, errors
  after earlier batches, and fused pulls. Collect refuses partial and failed cursors,
  and it refuses collection after a batch has already been yielded to the caller.
  Dropping one cursor does not set a shared caller cancellation token.
- **A7 — Budgets:** splitting batches cannot reset cumulative operator or work limits.
  Retain batches until the shared budget is exceeded, then release them. Test wide
  rows, a huge literal, growth from a BIND of unique strings, caches, copies made by
  fallback and collect, overflow and cleanup after failure. No charge may leak, and no
  path may bypass the budget unseen.
- **A8 — Deadline and cancellation:** cancel during rejected scan candidates, during
  filtering and callback work, and between pulls. Consumer sleep does not reset the
  deadline. Check that work checks happen at bounded intervals rather than relying on a
  test that hangs indefinitely.
- **A9 — Backpressure:** stall an HTTP reader until the bounded channel fills, and
  verify that examined and emitted rows stop growing beyond the bounded producer
  allowance. Disconnect, and let a deadline expire, during a blocked send. The producer
  is reaped and its resources are released.
- **A10 — HTTP failures:** inject errors before and after header commitment,
  result-byte overflow, partial CSV and TSV output, and a serializer failure after
  execution completes. Assert the proper early status or late body error, no success
  footer on failure, and one final completion record. Verify the resolved identity,
  history and authorization, and the stop semantics of `send`.
- **A11 — Capability honesty:** the strict fallback policy rejects an unsupported
  static plan at open and a dynamic barrier before it executes. Allowed fallback
  appears in the plan and stays budgeted. A budgeted native sort is reported as a
  full-input barrier, not as fallback, and the strict policy admits it. Access
  redaction conceals protected details.
- **A12 — Writer parity:** parse completed JSON, XML, CSV, TSV and native JSON outputs
  and compare the answers to eager output. Encoded byte counts match result-byte
  enforcement, and metadata is final only for the appropriate completion state.
- **A13 — Stateful operators:** exercise OPTIONAL with unbound matches, multiple
  matches and expression errors, MINUS compatibility, correlated EXISTS, duplicate and
  skewed join runs, global DISTINCT, ordered ties, empty groups and aggregate failures.
  Spill cases cover reservation denial, disk errors, cancellation, crash cleanup and
  encryption.
- **A14 — Graph and binding lifecycle:** compare graph sets and template blank nodes, and
  respect DESCRIBE truncation. Breaking out of binding iteration, aborts and garbage
  collection release the producer. Transaction-owner guards prevent lock reentry and
  execution that escapes the transaction.

## 10. Performance acceptance

Measurements use the same admitted source and toolchain, the same dataset, the same
cache and CPU conditions and the same query semantics. Raw samples and negative
outcomes are kept. Eager and cursor runs are repeated in counterbalanced order, with
enough independent processes to identify the runtime regimes observed earlier. A cold,
fast or slow process must not be silently excluded as noise.

For each implemented slice, report the latency to the first batch and the first byte,
complete execution and response latency, throughput, p95, physical work, the peak query
estimate and process RSS, with the shared cache shown separately. Include small
answers, full scans, wide answers and answers with large literals, joins, sort and
top-k, OPTIONAL, DISTINCT and grouping, and slow consumers. Run matched 1M and 10.5M
controls first. Validate large-answer memory and representative queries on the retained
1B+ dataset once resources allow. Competitor references may keep their own dates, and
these gates do not require rerunning unrelated competitors.

For supported scan and unary pipelines without growing term or operator state, memory
after warmup must follow the bounds on batches and live state as answer cardinality
increases, rather than the size of the complete answer. Plans with growing state must
report that growth and then stay within their budget, spill, or fail. Report
first-batch gains separately from full-response cost, because early output does not
excuse a material throughput regression.

The initial performance target is no reproducible loss above 5% in median or p95 for
existing small-query eager paths, or for a workload that a future default selects into
streaming. Repeatable smaller losses are investigated as well. This is an admission
target, not a claim of statistical significance, and the evidence must distinguish
variance from a stable cost. An unresolved regression keeps the affected path opt-in or
on the eager default. Batch targets are swept before the provisional defaults are
promoted or auto thresholds are added. The public BENCHMARKS and README tables change
only after completed representative runs.

## 11. Alternatives and decisions

- **Page a finished result:** this is useful API pagination, but it leaves the memory
  of full execution unchanged, so it does not satisfy this feature.
- **Re-run increasing LIMIT prefixes:** this repeats scans, expressions and callback
  effects, and it can accumulate excess work. A cursor keeps resumable positions
  instead.
- **Replace all eager execution immediately:** this loses a fast comparison path before
  equivalent coverage and costs are established. The work begins with explicit cursor
  use.
- **Producer threads in the core API:** these add queues and cancellation lifetime
  problems without helping a synchronous consumer. Core execution is pull-based, and
  HTTP and the bindings supply their existing blocking and async bridges with bounded
  transport.
- **Hide fallback or local vocabulary growth:** this produces misleading memory
  guarantees. Plans report capabilities and state, and retained query-created terms are
  charged from Phase 1.
- **Automatically buffer by estimated result rows:** this misses literal bytes, skew
  and blocking operators. Selection waits for measured evidence about plans and costs.

## 12. Design sources

The design derives from the current Sparkles executor, snapshot scans, context and
vocabulary, result writers, HTTP stream lifecycle and embedding APIs. The semantic and
resource contracts come from the existing specs [C01](C01-observability-and-budgets.md),
[C12](C12-graph-access-control.md), [C12b](C12b-triple-access-control.md),
[F06](F06-snapshots-and-point-in-time.md), [F11](F11-encryption-at-rest.md),
[G06](G06-arq-query-extensions.md), [P03](P03-query-extensions.md) and
[P06](P06-library-admin-api.md). Phases 1 and 2 add no dependency.
[PROVENANCE](PROVENANCE.md) records these sources.

## Outcome

The implementation exposes explicit SELECT, graph and ASK execution that owns its
snapshot. SELECT batches keep charged IDs and ownership of their vocabulary. Graph
batches keep charged RDF terms. Both stay usable after the dataset closes. Term copies
that the caller creates are application memory. Parsing, planning, pulls, metadata and
internal destruction are protected against deeply nested query structures. Cumulative
row and work controls, cancellation, deadlines and fused errors cover both production
and consumer waits, and memory accounting uses conservative reservations.

Plain immutable SELECT scans return read-only views of the decoded columns of index
blocks. Returned batches share the block's retained memory reservation, and loading
another block needs another reservation while consumers still hold earlier batches.
Other scans copy the columns that pass and keep full resume keys.

Merge joins resume both inputs across batches. Hash, cross, OPTIONAL, semi, anti and
MINUS joins first collect their build input into charged state. They then resume the
probe input and the expansion of matching rows across batches, so neither the join's
output nor its matching row pairs are materialized. All of these joins preserve
duplicates and unbound compatibility. DISTINCT owns a charged key set. Eligible groups
over plain variables keep aggregate state rather than input rows. Blocking sorts
without LIMIT consume normal input batches even when the requested output prefix is
small. They reserve their input, keys and reordering state, and they report their full-input
barrier separately from eager fallback, so the strict policy admits them. Growing state
fails explicitly when its reservation exceeds the budget, because disk spill is not
implemented. Unsupported operators and EXISTS expressions remain visible barriers that
run eagerly when first demanded, and the strict policy rejects them before that demand.

Batch writers decode distinct base-vocabulary terms under charges for retained and
reconstruction memory, and they skip the optional cache when memory is tight. BIND
expressions that only reuse IDs and fixed numeric ORDER keys avoid repeated expression
work without keeping uncharged value caches. Variable ORDER columns decode the
vocabulary in sorted order under a charge. EXISTS plans, key sets and partial
projections keep their charges, and when the optional cache is unavailable they fall
back to evaluating each row. REGEX and REPLACE programs owned by the query, with
explicit bounded search caches, replace the thread-local state used by eager
execution. They include estimates of capture products and checked growth of
replacements. String and key filters borrow keys under charged scratch memory. Cursor
evaluation avoids uncharged result, decoded-value and geometry caches, and geometry
operation limits stay in force.

CONSTRUCT instantiates its template for each solution, preserves fresh and shared blank
nodes, and deduplicates quads globally under a charge. DESCRIBE exposes a budgeted
traversal barrier that needs its full input. ASK stops after a qualifying solution.
The RDF writers, the native graph JSON writer and the Jena graph writers consume graph
batches. RDF/JSON reserves memory for its map from subjects and predicates to objects
before inserting into it. Native graph JSON emits quads of four terms, with null
default graph names.

Explicit execution over HTTP uses a single producer, bounded response queues and
interruptible waits for capacity, and it keeps its controls in force until the body
completes. Native metadata distinguishes Complete from Stopped and reports unknown
totals when a consumer takes only a prefix. Explicit execution is implemented in the
Rust dataset APIs, the Python SELECT and graph cursors, Node result and byte streams,
opt-in Jena WHERE execution on the JVM, and the CLI. Copies of rows and transport data
in the foreign bindings follow their own bounded batching contract. Python rejects
snapshot capture inside an owned transaction.

The ordinary query path remains eager. Auto selects large plain immutable SELECT scans,
and uncached eligible OPTIONAL COUNT queries on a single key, under the measured batch,
graph-predicate and memory conditions that the API documents. Admitting aggregates in
this way preserves enabled result caches. Over HTTP, auto falls back to eager execution
when the negotiated encoding is SPARQL Results Thrift, which has no cursor writer.

The W3C SPARQL 1.0 and 1.1 query evaluation tests run through cursors at batch sizes
of 1, 2, 3 and 4,096 rows and match the expected results. The 53 queries that need
eager fallback are counted, and they run with fallback allowed. A seeded differential
test compares cursor and eager answers at the same batch sizes over generated VALUES,
scans, property path sequences, joins, OPTIONAL, MINUS and UNION with unbound columns.
Correctness and resource gates passed across core, HTTP and the bindings. The benchmark
refresh, which measures Sparkles only, covers 1.05M, 10.5M and full DBpedia, plus
specialized suites and matched JVM controls.
[BENCHMARKS](../BENCHMARKS.md#streaming-execution) reports the current cost of each
mode on representative queries.

Eager controls matched with the production allocator are close to the previous
implementation. At 10.5M, grouped average is +0.9%, OPTIONAL count +1.3% and
expression grouping within 0.5%. The 1.05M range and star controls keep differences of
about 5.3%, and a point lookup differs by about 1.9 microseconds. These results do not
establish performance parity in general. In the recorded controls, explicit streaming
improves scans, OPTIONAL count and full sorting, while range TopK, expression sorting,
substring filters and TSV output remain slower. Because of their growing-state
accounting and mode differences, those shapes stay opt-in, and the narrow automatic
admission does not select them.

ORDER BY with LIMIT keeps a bounded heap of offset plus limit rows in both modes. Each
ORDER BY key of a row is classified once into a sort key. Two keys compare by the class
of their values, and within a class by an exact decimal, a double, a string or a
boolean when that decides the order. Every other pair goes to the existing comparator.
Such pairs are equal numbers of different datatypes, NaN, signed zeros, dates, times,
durations, composite literals and triple terms. The key therefore gives the
comparator's result on every pair of values, and ties keep falling back to input order.
The heap reads the first key of every row and evaluates the later keys only for rows
that can still enter it. Eager execution offers the rows of its input table. The cursor
operator offers each input batch as it arrives and keeps at most k + 1 rows of IDs and
their keys under the query budget, so a top-k over an input larger than the budget
completes. Both modes feed the same heap in the same order. They return the same rows
at every batch size, also when dates with and without a timezone make the order
partial, which is the case that broke the earlier chunked top-k. When every value of a
single numeric key is a number, the existing numeric prefilter first drops the rows that
already have k rows strictly ahead of them, and the heap ranks the rest. The full sort
without LIMIT still calls the comparator directly, because a key for every row of a 10.5M
sort cost about 2% more than it saved. Ordered tests compare the key with the
comparator on every pair of random values of every class and check the full sort's
permutation. They check the heap against the sorted prefix whenever the random values
are totally ordered, and streaming against eager over partial dates at batch sizes from
1 to 4,096 rows. The `topk_heap` optimization turns the heap off.

A/B/B/A runs on an otherwise idle 20-CPU host, pinned to 12 CPUs with mimalloc, compared
the heap with the previous commit. At 1.05M, the expression sort key query took 1.20 ms
instead of 1.71 ms eagerly and 1.31 ms instead of 1.69 ms when streaming. At 10.5M it took
11.2 ms instead of 15.6 ms eagerly and 12.9 ms instead of 18.9 ms when streaming. Range
TopK streaming at 1.05M took 3.09 ms instead of 3.69 ms. Eager range TopK and both range
TopK runs at 10.5M stayed within 3% of the previous commit, as did the full sort, the
other ordered and grouped queries, and 19 other queries in both modes. NOT EXISTS was the
exception at 3.6% slower in both modes. Its code path did not change, and it executed the
same number of instructions in both builds, so the difference is code layout.

Disk spill, broader automatic selection and removing every remaining cost of complete
responses are follow-up work. A review of the implementation found further follow-ups:

- An operator without a cursor implementation runs its whole subtree eagerly, and that
  fallback does not use the early stop that LIMIT gets in eager execution. The fallback
  should eventually materialize only the unsupported operator and read its inputs from
  child cursors. Prefix demand may pass only through operators that preserve the
  required solutions, including OFFSET and callback effects. The absence of ORDER alone
  is not enough. Sharing kernels and accounting is incremental work. Replacing eager
  execution with cursor collection is subject to the existing performance gates.
- Index joins already run per input table, so they are candidates for streaming instead
  of fallback. Because a batch can expand into many solutions, the operator needs charged
  fan-out state and resumable output before it counts as bounded.
- The streaming hash join picks its key from the plan's certainty alone and scans the
  build side for every probe row otherwise. It should check the built data for unbound
  values, as eager execution does. Bound probes must also match unbound build rows,
  and an unbound probe must consider all compatible build rows. Compact row chains need
  checked index-width limits, charges and duplicate/OPTIONAL/MINUS coverage.
- Sort order travels as a claim on each batch. It should be a property of the operator,
  checked at batch boundaries in debug builds.
- Profile bounded coalescing, parallel batch decoding and one-batch prefetch in the
  result writer. The serial decoding mechanism is established, but its contribution
  to the remaining throughput difference needs measurement. All overlapping input,
  decoded and transport state must remain charged, cancellable and bounded under
  slow consumers. Extra producer work must respect callback ownership.
- Extend charged scan sharing and selective decoding to untouched block ranges of
  snapshots with pending changes. The merged scanner already emits untouched base
  blocks, but inserts, deletes and access-control masks must still be applied.
- Explicit streaming over HTTP should send its first bytes sooner than after 1 MiB.
  Earlier response commitment needs explicit truncated-body behavior for later errors,
  bounded admission of slow streaming bodies and documented transfer deadlines.
- The differential test should cover filters, ordering, slicing, grouping, pending
  changes, multiple blocks and access-restricted views. This extends the combined
  generated matrix alongside existing focused tests and W3C cursor coverage.

These are follow-up proposals, not measured speedups or a change to the delivered
defaults. Strengthened tests and ordering invariants precede the operator changes,
and disk spill follows the nearer-term fallback and output work. Full-scale cold reads
and loading need separately matched historical controls before their dated snapshot
differences can be attributed to query execution.
