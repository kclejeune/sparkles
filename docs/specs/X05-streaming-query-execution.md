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
| Timeout | One query deadline that counts the query's own time. Time blocked on the reader is added to a per-query pause and does not count, and one wait longer than the request's timeout ends the stream. |
| Cancellation | Checked during production, decoding, serialization and interruptible transport waits |

Counters use checked or saturating arithmetic and fail on overflow. Counts are updated
as work happens, so an early stop or a failure keeps the partial work. For a fixed
physical plan, splitting batches differently must not change the counts, must not
double-count ownership transfers, and must not let an unlimited stream through as a
series of batches that each stay under the limit. Eager and cursor plans can
legitimately do different physical work, so semantic parity does not require identical
counters. In access-filtered plans and reports, checks of hidden candidates do not
reveal hidden cardinalities.

The Timeout row changed on 2026-10-17, when automatic execution became the default. It
used to read that consumer waits and transport backpressure do not suspend the deadline.
That rule made a large default export to a slow but steady client fail where eager
execution, whose deadline ends before the transfer starts, had succeeded. The deadline
now leaves out the time a cursor waits for its reader over HTTP and in Node, and so does
every copy of the deadline that a SERVICE call, an algorithm run or a history service
takes when it starts. The stall bound limits one wait, not the whole transfer, so a
client that takes a chunk just often enough keeps the stream and its snapshot, memory
grant and producer thread for as long as it reads, as an eager answer always did. A
limit on the whole transfer belongs to the `--stream-idle-timeout` follow-up under
[Roadmap follow-ups](#roadmap-follow-ups-2026-10-12).

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
observe cancellation, the deadline and the stall bound. A request with a timeout never
waits longer than that timeout for another write, and one without a timeout waits until
the client reads or disconnects. When the receiver drops, blocked sends wake up and the producer is released. The
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
  deadline, and since 2026-10-17 it does not count against it either. Check that work checks happen at bounded intervals rather than relying on a
  test that hangs indefinitely.
- **A9 — Backpressure:** stall an HTTP reader until the bounded channel fills, and
  verify that examined and emitted rows stop growing beyond the bounded producer
  allowance. Disconnect, and let the stall bound expire, during a blocked send. The
  producer is reaped and its resources are released.
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
duplicates and unbound compatibility. The hash join keys its build rows on the shared
variable with the fewest unbound values in the built data, as eager execution does. A
bound probe visits the build rows with its key and then the build rows that leave the
key unbound, and an unbound probe visits every build row. The chains of row numbers are
checked against their 32-bit width and charged with the build.

Index joins, LET, UNFOLD, triple-term decomposition and transitive paths that start
from their input rows run their eager kernel on one input batch at a time. The output
of a batch is charged state that later pulls resume, so a batch that expands into many
solutions still returns batches no larger than the caller asks for. The input demand
follows the expansion seen so far. A transitive path without an input walks one start
node at a time. It charges the start nodes it keeps and the set of nodes it has
visited. DISTINCT owns a charged key set. Groups keep aggregate state rather than input
rows for composite keys, expression arguments, DISTINCT aggregates, GROUP_CONCAT,
SAMPLE and the statistical aggregates. Group counts read from the index or its
statistics, and counts of two-hop joins over key runs, run as barriers that return
their counts without an eager fallback. Blocking sorts without LIMIT consume normal
input batches even when the requested output prefix is small. They reserve their input,
keys and reordering state, and they report their full-input barrier separately from
eager fallback, so the strict policy admits them. Growing state fails explicitly when its
reservation exceeds the budget, because disk spill is not implemented.

An operator without a cursor implementation materializes only itself. It reads its
inputs from child cursors, or from exactly sized scans when an input is a plain scan,
so the rest of the plan keeps streaming. A fallback drains its inputs in full before it
runs, so a LIMIT above it does not stop those inputs early. Registered and fold
aggregates still run eagerly when first demanded, and the strict policy rejects them
before that demand. A registered aggregate runs one accumulator per group whose
callbacks see the rows in input order, and a fold needs the rows of its group in order,
so neither has running state that a cursor group can hold.

EXISTS runs inside the cursor wherever it appears, in FILTER, BIND, ORDER BY, OPTIONAL
conditions and aggregate arguments. The first batches that reach an EXISTS collect the
distinct outer keys as retained state. Once they hold as many keys as the pattern's
estimated cost divided by the cost of one evaluation per row, the pattern runs as a
child cursor under the query's charges and fills a key set. Each batch of the pattern
is charged before its keys enter the set, and later outer batches probe the set. When
the outer rows have fewer distinct keys than that, or the key set does not fit the
budget, each distinct outer key is evaluated once and its answer is kept in a memo. The
memo is charged per entry. It stops growing at an eighth of the memory budget, or when
less than a quarter of the budget remains, so that it cannot exhaust the query's
memory. A pattern that calls a SERVICE ignores the second limit while the budget can
charge an entry, since evaluating a key again sends the request again. The cost is that
such a query can leave less than a quarter of its budget free, up to the eighth that the
memo may hold, so a later operator of the same query can fail on memory where it would
not have. The memo stays charged, so the query never exceeds its budget. A row that binds
none of the pattern's variables has the same answer as every other such row, and that one
answer is kept outside the memo without a charge. Before this, a cursor whose budget had
no room for the memo called a SERVICE inside such an EXISTS once per row, 60,000 times for
60,000 rows in one test. A variable that only a FILTER inside the EXISTS reads still
forces evaluation per row, as it does in eager execution.

A transitive path over a sequence or alternative reads its edge relation from a child
cursor into charged adjacency maps on first demand. Without a bound input it then walks
one start node at a time from the sorted sources of those edges. With a bound input it
expands one input batch at a time. COUNT(DISTINCT *) keeps a charged set of the
distinct rows of each group. Local LATERAL subqueries and the ARQ property functions
run their eager kernel on one input batch at a time, like the other expanding
operators, so a LIMIT above them stops their input early. Remote LATERAL, SERVICE,
registered property functions and the search and spatial operators still drain their
inputs. Their answers for a prefix of the input depend on a remote endpoint or on
callbacks, so the cursor cannot prove that the prefix is enough.

MINUS and semi and anti joins run as a forward merge when their only shared variable is
bound on every row of both inputs and both inputs keep their order on it. A left row
passes on the presence of its key on the right, so the merge holds no runs, and the
right input reads only the key column. A COUNT over such a join counts the passing rows
without building them.

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

Auto selects plain immutable SELECT scans under the measured batch, graph-predicate and
memory conditions that the API documents. Until 2026-10-17 it also selected uncached
OPTIONAL COUNT queries on a single key, which
[the parity section](#streaming-parity-2026-10-17) explains. Over HTTP, auto falls back
to eager execution when the negotiated encoding is SPARQL Results Thrift, which has no
cursor writer. Auto became the default mode over
HTTP, in the CLI and in Node on 2026-10-16, as
[the section below](#automatic-execution-by-default-2026-10-16) describes.

The W3C SPARQL 1.0 and 1.1 query evaluation tests run through cursors at batch sizes
of 1, 2, 3 and 4,096 rows and match the expected results. The queries that need eager
fallback are counted, and they run with fallback allowed. The count fell from 53 to 25
when joins, expanding operators, groups, group counts and paths gained cursor
implementations. Fifteen of the 25 filtered with EXISTS, nine used a transitive path
over a sequence or alternative, and one counted distinct rows with COUNT(DISTINCT *).
With those three in the cursor, no W3C query needs an eager fallback at any of the four
batch sizes, and all 508 evaluated tests still pass in every cursor variant. A seeded
differential test compares cursor and eager answers at the same batch sizes over
generated VALUES, scans, property paths, joins, OPTIONAL, MINUS, UNION, filters,
ordering, slicing, grouping and aggregates. It also generates EXISTS and NOT EXISTS in
filters, BIND, OPTIONAL conditions, ORDER BY and aggregate arguments, transitive paths
over sequences and alternatives, COUNT(DISTINCT *) and LATERAL subqueries with ORDER
BY and LIMIT. It runs over a compacted base, pending changes, many small blocks and an
access-restricted view. Across 1,500 generated seeds every query with EXISTS, a
transitive path over a sequence or alternative, COUNT(DISTINCT *) or LATERAL streams
under the strict policy.
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

The heap first ran ahead of the prefilter on the first key of several keys, so a top-k
over a large input built a sort key for every row before any row was screened.
DBpedia's top-linked query orders tens of millions of grouped counts, and it became
about 46% slower. The first-key prefilter now runs before the heap. It ranks the
distinct values of the first key, keeps the rows that can still reach the first k, and
the heap orders only those. Ranking needs a total order, so the prefilter steps aside
when the first key holds dates, times, durations, NaN or other values that the
comparator orders only partially, and the heap ranks those rows. On the billion-triple DBpedia index, 64 interleaved runs per
build gave 543.5 ms for top-linked, against 795.2 ms with the heap first and 537.5 ms
before the heap existed. Class counts, co-star birthplaces, country population and
entity facts stayed within 1% of the build before the heap. At 1.05M the expression sort
key, grouped average and expression aggregate queries stayed within 2.2%.

A range TopK whose input is a monotone piece of an index reads that piece from its best
end. The planner charged placing the read a whole block of rows, so at 1.05M it kept the
full scan, which ran 16% slower than the ordered read. Placing the read decodes the
leading key columns of one block, which the block cache keeps, and the planner now
charges an eighth of a block for it. At 10.5M the plan and its time did not change. The
1.05M range TopK also filters about ten thousand salaries stored as non-canonical
decimals in the dictionary. Those values now decode straight into numbers without a
detour through RDF terms, and they convert to doubles without 128-bit division. The
conversion gives the same bits as the decimal type's own conversion on random values and
on the extreme ones.

The fallback work was measured on the 1.05M suite against the previous commit with
both servers running at once, pinned to the same three cores with the result cache
off. Requests alternated in A, B, B, A order and the change is the median of 41 paired
ratios. Streaming expression aggregation (expr-agg-arg) fell from 10.5 to 4.6 ms,
because its group no longer falls back. Streaming MINUS fell from 3.5 to 3.0 ms with
the new hash join keys. Every other query in both modes stayed within 10% of the
previous commit, including the paths, EXISTS filters, group counts and two-hop counts
that changed mode. Among the 30 bench queries, the number with an eager fallback under
streaming fell from 13 to 2. The two that remain are the EXISTS filters of exists-join
and not-exists, and their inputs now stream into the filter. The machine was shared
with other work during these runs, so the request-level interleaving is what keeps
them comparable. Memory use is unchanged for the counts read from the index. The
expanding operators and path walks hold the output of one input batch or one start
node rather than the whole result. In exchange the hash index adds 4 bytes per build
row and one map entry per distinct key to the charged build state.

Those two EXISTS filters now stream as well, so no bench query needs an eager fallback
under streaming. The work was measured on Namespace instances with `mise run
bench:nsc`, whose `--execution` option now sets the mode of each side. Each run starts
fresh servers pinned to one core in four A, B, B, A rounds of 20 timed requests per
query and server, with one Rayon worker and no result cache. The times below are client
wall times, which include serialization. Against streaming on the starting commit,
streaming on the final code had a geometric mean 7.8% lower at 1.05M and 14.1% lower at
10.5M. At 10.5M optional-chain fell from 448 to 198 ms, order-by-full from 1,615 to 917
ms, distinct-join from 59 to 32 ms and star-join from 87 to 46 ms. The exists-join query
changed by +1.6% and not-exists by -21%, so evaluating EXISTS inside the cursor is not
slower than the eager fallback it replaces.

Profiles of the queries that were slower in streaming found five costs in the cursor
engine and its writer. The HTTP writer of an explicit streaming response read the clock
on every write to check the deadline. Serializers write a few bytes at a time, so those
clock reads took 36% of the full ordering query. The writer now checks cancellation on
every write and the deadline when its buffer crosses a 16 KiB mark and before it sends a
chunk, and the cursor still checks its deadline every 1,024 rows. The merge join left
its loop over loaded batches for every run of duplicate keys and built a resumable pair
for it. Runs that end inside their batches now join directly. The hash join walked a
state machine and filled a one-row predicate table for every candidate even without a
predicate. It now joins whole probe rows in one pass when every build row binds the
key. The streamed EXISTS key set grew from empty and rehashed, which took a fifth of the
NOT EXISTS query. It is now sized for the pattern's estimate when a quarter of the
remaining budget covers it. MINUS over two inputs ordered on their one shared key used
the hash join. It now runs as the merge described above and decides each batch with the
branch-free lower-bound zipper of eager MINUS. The comparisons with the starting commit
ran before these last two changes, which affect only MINUS, semi and anti joins and
hash joins.

Before this work, streaming against eager on the same commit had a geometric mean of
+14.9% at 1.05M, with minus, optional-chain, distinct-join, star-join and order-by-full
between 41% and 145% slower. On the final code the geometric mean is +0.9% at 1.05M and
+6.9% at 10.5M. Streaming is faster for range-topk, order-by-full, expr-agg-arg and
expr-order-key. At 1.05M it stays more than 10% slower for group-avg, distinct-join,
minus and regex-iri, and for several queries under a millisecond. Those small queries
moved by as much as 36% between two servers running the same binary in an A/A control,
so their differences are within the noise of the instance. At 10.5M optional-chain is
35% slower, group-avg 28%, minus and expr-bind-group 22%, and the EXISTS filters 11% to
13%. The execution time of optional-chain at 10.5M is more than twice the eager time,
so its hash join is the largest remaining gap. An earlier run at 10.5M on an instance
with ten times the usual steal time was discarded. Profiles of group-avg and
distinct-join show the same leading costs in both modes, with the cursor's group and
merge join taking about the share that eager execution's kernels take. Eager execution
did not change. Against the starting commit its geometric mean was +1.1% at 1.05M and
-0.6% at 10.5M, and an interleaved local run of the queries with the largest shifts
stayed within 2%.

Disk spill, broader automatic selection and removing every remaining cost of complete
responses are follow-up work. A review of the implementation found further follow-ups:

- A fallback drains its inputs in full. Local LATERAL and the ARQ property functions
  now stream per input batch, so LIMIT reaches their inputs. Passing LIMIT demand into
  the inputs of the remaining fallbacks, which are remote LATERAL, SERVICE, registered
  property functions and the search and spatial operators, needs a proof for each of
  them that a prefix of its input yields the required solutions. That proof must cover
  OFFSET and the effects of callbacks and remote requests. The absence of ORDER alone
  is not enough.
- The group-avg, distinct-join and minus queries remain 10% to 15% slower in streaming
  at 1.05M, and optional-chain, group-avg, minus and expr-bind-group are 22% to 35%
  slower at 10.5M. Their profiles do not single out one cost, so closing the gap needs
  grouping and join kernels that work on whole batches the way the eager kernels work
  on whole tables. The hash join's per-row chain walk is the first candidate, because
  the eager join builds its pairs and gathers each output column in one pass.
- Registered and fold aggregates need running state of their own before their groups
  stop falling back. A registered aggregate's callbacks must keep seeing one group's
  rows in input order, and a fold needs its group's rows in order.
- Sort order travels as a claim on each batch. It should be a property of the operator,
  checked at batch boundaries in debug builds.
- Measure parallel batch decoding and one-batch prefetch in the result writer. With
  the deadline read once per 16 KiB, profiles of the streaming and eager writers for
  the full ordering and the 500,000-row TSV export share their leading costs, which are
  sorting the cell IDs, decoding the vocabulary and the serializer itself. Parallel
  decoding can help only with more than one Rayon worker, and the Namespace benchmark
  runs with one. All overlapping input, decoded and transport state must remain
  charged, cancellable and bounded under slow consumers. Extra producer work must
  respect callback ownership.
- Extend charged scan sharing and selective decoding to untouched block ranges of
  snapshots with pending changes. The merged scanner already emits untouched base
  blocks, but inserts, deletes and access-control masks must still be applied.
- Explicit streaming over HTTP should send its first bytes sooner than after 1 MiB.
  Earlier response commitment needs explicit truncated-body behavior for later errors,
  bounded admission of slow streaming bodies and documented transfer deadlines.

These are follow-up proposals, not measured speedups or a change to the delivered
defaults. Strengthened tests and ordering invariants precede the operator changes,
and disk spill follows the nearer-term fallback and output work. Matched controls on
full DBpedia attributed the slower cold reads and loads of 2026-10-08 to the state of
the benchmark machine's SSD rather than to query execution. After a trim, the current
and 2026-10-03 binaries loaded equally fast, and the current binary's cold reads were
within 10% or faster on every query.

Range filters over numeric literals stored in the vocabulary read their values from the
numeric column (`vocab.num`) in both execution modes. A scan with an inexact range, such
as `FILTER(?lat > 48 && ?lat < 49)` over `xsd:float` literals in non-canonical form,
tests each batch's distinct ids against the column and decodes only ids that the column
does not hold, such as strings, dates and decimals with too many digits. The cursor
scan does this per batch with no extra state, and the eager scan per parallel chunk.
Sorts, top-k and the cursor's decoded value columns read numbers from the column too.
On full DBpedia the cold `geo-box` query fell from 255 to 108 ms, and the answers are
identical with the column switched off (`SPARKLES_NUMERIC_COLUMN=off`).
[BENCHMARKS.md](../BENCHMARKS.md#numeric-literals-in-the-vocabulary) has the
measurements.

### Roadmap follow-ups (2026-10-12)

[ROADMAP](../ROADMAP.md) item 7 takes up the follow-ups listed above. Disk spill and the
batch kernels now have their own spec, [X08](X08-disk-spill-and-server-memory.md), and
the rest are sketched here.

* **Batch-oriented hash joins and grouping.** X08 §5 replaces the hash join's per-row
  chain walk with build rows grouped into contiguous slices per key and a two-pass probe
  (pairs, then a gather per column), and the cursor group's per-row updates with a group
  slot vector and column-wise aggregate updates. Its gate is streaming within 10% of eager
  on `optional-chain`, `group-avg`, `minus`, `expr-bind-group` and `distinct-join`.
* **Disk spill.** X08 §6 is the implementation design that §4.3 above asks for: reserved
  disk per query and per server, files opened unnamed in a directory owned by the process
  and swept only when positively identified, a frame format with checksums, AES-256-GCM
  with an ephemeral key for encrypted spill, hybrid hash join, external sort, partitioned
  aggregation and DISTINCT.
* **LIMIT into the remaining fallbacks.** Each fallback gains a property
  `prefix_sufficient`, false by default. It is set only with a proof for the operator: a
  non-correlated SERVICE without OFFSET whose remote query the enhancer can wrap in a
  `LIMIT` (G10's bulk mode excluded), a full-text search whose hits arrive in score order
  with no later filter on the score, and a spatial nearest-neighbour search, which is
  ordered by distance. Registered property functions keep draining unless their
  registration declares that a prefix of the input yields a prefix of the output.
* **Running state for registered and fold aggregates.** When the group's input is sorted
  on the group keys (index order or a prior sort), groups arrive whole and in input order,
  so the cursor can run one accumulator per group and emit each group when its key
  changes. Otherwise the operator sorts its input on the keys with a stable sort, which
  keeps input order within each group, using X08's external sort when the input exceeds
  memory. The eager path stays for registries whose callbacks need the whole input.
* **Parallel decoding and prefetch.** The batch writer decodes batch n+1 on a Rayon task
  while it serializes batch n, at most one batch ahead, with both batches charged and the
  task cancelled on close. It is off with one Rayon worker and when callbacks own the
  terms. It is kept only if the full ordering and the 500,000-row TSV export gain more than
  the tie threshold with four workers.
* **Earlier first bytes.** Explicit streaming commits the response after the first
  batch or 64 KiB, whichever comes first, instead of after 1 MiB. A later failure aborts
  the transport as §6 specifies. Slow streaming bodies count against the X08 pool through
  their retained batches. The stall bound is now the request's timeout, and a
  `--stream-idle-timeout` (default 60 s) would replace it with its own limit on how long a
  body's reader may take nothing.
* **Broader automatic selection.** X08 §6.8 lets `auto` stream SELECT plans whose
  estimate exceeds half the memory budget, and retries an eager query that fails its
  budget before output as a cursor with spill. Each widening needs the matched
  measurements of §10.

### Automatic execution by default (2026-10-16)

`execution=auto` is now the default where a caller gets its answer as a stream of rows
or bytes. Plans that measured faster as cursors then stream without being asked for, and
every other plan runs eagerly with the response it had before. The table lists every
entry point that picks a mode and what it does now.

| Entry point | Default | Reason |
|---|---|---|
| HTTP `/{ds}/sparql`, GET and POST, every result format | `auto` | The answer is a byte stream, and the cursor writer covers every format except SPARQL Results Thrift. `serve --default-execution eager` restores eager for a whole server. |
| HTTP with SPARQL Results Thrift | eager | That encoding has no cursor writer. |
| A query that ran in under 10 ms last time | eager, in place | The decision would cost more than any stream can save, and the in-place path stays as it was. |
| Stored queries, `/{ds}/queries/{name}` | as `/{ds}/sparql` | A run with parameter values has initial bindings, which rule out every automatic plan, so it runs eagerly. |
| Graph algorithms over HTTP | as `/{ds}/sparql` | Their plans never qualify, so they run eagerly. |
| UI result table | eager, asked for explicitly | The table shows the full row count and the time slider compares totals, which a streamed answer stopped after `send` rows does not know. |
| UI downloads | the server's default | A download reads the whole answer. |
| CLI `sparkles query`, local | `auto`, but eager for the `text` table and `--explain` | The table measures every row before printing one. A plan that stays eager prints exactly what an eager query prints. |
| CLI `sparkles query --server` | the server's default | It sends `execution` only when `--execution` is given. |
| Rust `Dataset::query` and `query_with` | eager | They return a collected `QueryResult`. `ExecutionMode::default()` is now `Auto`, and `Dataset::query_execution` uses it. |
| Rust client | the server's default | `QueryOptions::execution` chooses a mode. |
| Python `query`, `select` and the rdflib store | eager | They return collected values whose iteration cannot fail, and they also run inside transactions. `select_cursor` and `graph_cursor` stream on request. |
| Node `query`, `select`, `construct`, `ask` and `queryToStream` | `auto` | Node results are already async iterators that pull batches, so a streamed result only loses `size` and can reject while it is read. A plan that stays eager keeps its size and its first batch from the call. |
| Node transactions | eager | A cursor cannot read a transaction's state. |
| JVM and Jena | `auto` since 2026-10-17 | ARQ result sets are lazy iterators that already fail from `hasNext`, so a streamed result keeps their contract. Write transactions run eagerly, and `Sparkles.STREAMING_EXECUTION` set to false opts out. |
| TypeScript client | the server's default | It sends `execution` only when the caller sets it. |
| MCP tools, GraphQL, SPARQL diffs, `/{ds}/explain`, Ask | eager | Each returns one document built from the whole result, such as an MCP answer with an exact total. |
| SERVICE to Sparkles, including the same server | the remote server's default | The SERVICE client reads SPARQL JSON or XML as it arrives, so a cut-off answer fails the parse and the transfer and the SERVICE call fails. |

When automatic execution streams an answer, the caller sees the explicit-streaming
contract that the API documents. An error after the first 1 MiB of the body, such as a
timeout or an exhausted budget, aborts the transfer after a `200` status, and in TSV,
CSV or N-Triples the rows before the cut are well formed. The Rust client, the CLI, the
TypeScript client and the UI report the cut-off transfer as an error, and new tests serve
such a response to each of them. They all already did, through the transfer errors of
reqwest and of fetch, so none of those tests failed before the change. The deadline
counts the query's own time and not the time the client takes to read, a client that reads
nothing for longer than the timeout ends the stream, and the snapshot and the query's
memory grant stay held until the body ends. With `send`, the native metadata reports `stopped`
and an unknown total. Rows come in the same order as eagerly, which a test checks over a
million-row scan. Streamed answers skip the full-result cache. A plain scan is never
cached, so this matters only for its projection, which a cursor reads from decoded
blocks at about a third of the eager cost. The access log and the metrics record a
streamed answer when its body ends.

One rule keeps late errors rarer than in explicit streaming. A scan whose estimate passes
the request's `max-rows` or `max-rows-produced` stays eager, so the budget fails it with
`507` before any output. A request that lowers `max-result-mb` does not stay eager. A
streamed answer that passes the lowered budget within its first 1 MiB is refused with
`507` before any output, and one that passes it later aborts the transfer, as an eager
answer past 1 MiB already did. A streamed scan can still run out its `timeout`
partway through when the query itself is slow, and a reader that takes nothing for
longer than the timeout is cut off.

Automatic execution plans the query once on the blocking pool. When it keeps the plan
eager, the HTTP handler continues on the eager path with that result, so the response,
its headers, the quick-query memory and the error body with the failed plan are those of
an eager request. The comparisons in the server tests that relied on eager being the
default now ask for it, and the test of the default streams a million-row scan, checks
the eager opt-outs per request and per server, the row-budget rule and a late timeout
that cuts off a default stream.

The first build of the change cost a never-repeated small query between 0.5% and 0.9%
more instructions on forge, because the server measured the query's nesting a second
time to size the parser's stack. A profile put about 1.5% of the instructions in that
scan. Spargebra already checks the nesting on every parse, so the server now skips the
measurement when the thread has at least 2 MiB of stack left, which every server thread
has. After that fix, 1000 requests each of `star-lookup`, `values-star`,
`employee-docs` and `star-join` at 1.05M, each with a unique comment so that none takes
the in-place path, cost -0.2% to +0.1% instructions against 6f0f928c. Repeated requests,
which take the in-place path, moved by less than 0.1% except `star-join` at +0.4%. The
in-place path runs the same code in both builds, so that difference is noise between
server processes.

The memory effect is the one the explicit-streaming measurements predicted. At 10.5M,
the full scan with `send=10` peaks at a 1.1 MiB allocation estimate and 66 MiB of server
RSS by default, against 241.0 MiB and 804 MiB eagerly, and answers in 2 to 5 ms instead
of 240 to 320 ms. Read whole as TSV, the scan peaks at 581 MiB of RSS instead of 885 MiB
in about the same time. [BENCHMARKS](../BENCHMARKS.md#streaming-execution) has the table.

The first A/B screen on forge flagged the whole name scan at 10.5M in native JSON, which
the default streams. Its wall time rose by 10.5%, from 553 ms to 611 ms. A profile put
about 15% of the cycles in the memory ledger, because the writer cache of each batch
reserved every decoded term on its own and a cursor's charge cannot take the fast path of
an eager query. The cache now reserves ahead in steps of at least 64 KiB, still before
each term is allocated, and overstates a batch by less than one step. After that fix the
default name scan takes 547 ms in native JSON against 531 to 563 ms eagerly, 1.37 s against
1.41 s in SPARQL JSON and 273 ms against 357 ms in TSV. The final screens at 1.05M and
10.5M found no query whose median server or wall time regressed by 10% or more, and the
name scan's wall time matched the baseline's at 553 ms.

### Streaming parity (2026-10-17)

The first part of roadmap item 7 measured eager against streaming execution again on
forge, at 10.5M with 12 Rayon threads and with one, and profiled each gap before changing
anything. Most of the remaining eager wins at 12 threads come from parallelism inside
eager operators, which run Rayon over the whole table, while a cursor works on one batch
at a time. With one Rayon thread, the setting of the Namespace screens, streaming ties
or wins on most shapes.

Four changes closed the gaps that profiles traced to the cursor itself.

* A result writer that reads a cursor decodes the terms of the next batch on another
  Rayon thread while it writes the current one (`decode_ahead`), and charges each written
  row's terms to the memory ledger once instead of once per cell. It writes the first
  batch before it pulls the second, so the first rows wait for one batch only, and it
  reads ahead only when the memory budget has room for twice what the first batch took.
  That room is an estimate from the first batch, so when later batches hold larger terms
  a budget within a few KiB of the writer's own need can still fail with decoding ahead,
  always with a memory budget error.
  An error from a batch read ahead reaches the writer after it has written the batch
  before it. A memory budget failure inside the plan is the exception, since it fails
  every later check of the query, and the writer then stops before writing that batch.
* A cursor filter starts at 32,768 rows and doubles its row demand with each read up to
  262,144 rows, and a cursor top-k doubles from the cursor's batch size up to 65,536 rows,
  while the row budget and the memory ledger have room (`grow_input`). The scan under a
  filter still stops at the caller's `batch_bytes`, so with the default 1 MiB a filter
  over two columns reads at most 65,536 rows at once and over one column 131,072. A top-k
  raises the byte cap with its demand. The tradeoff is memory. Over 300,000 rows a
  substring filter peaked at 12.3 MB against 6.7 MB with fixed reads, and a top-k at
  24.1 MB against 2.5 MB, both within the query's budgets.
* A cursor top-k reuses the values it decoded for its prefilter instead of decoding them
  again. When its first key holds one result per distinct value of totally ordered
  kinds, its screen remembers which values it rejected for the rest of the batch. In a
  total order a rejected value is worse than the heap's worst row, which only improves,
  so it stays rejected. DateTimes with and without a timezone can form a cycle, and the
  first build remembered their rejections too, so a batch of 1,024 rows or more could
  rank them differently from eager execution. Such keys are now screened row by row.
* The numeric columns of a cursor's expression cache are located in parallel when a
  batch has at least 16,384 ids.

The table gives the streaming time divided by the eager time, each measured in the same
interleaved run of a probe binary. A ratio under 1 means streaming is faster.

| 10.5M, ratio of streaming to eager | af4a79d6, 12 threads | Now, 12 threads | Now, 1 thread |
|---|---:|---:|---:|
| TSV, 500,000 rows | 1.80 | **0.86** | **0.83** |
| TSV, name scan of 1.02M rows | 1.37 | **0.89** | **0.91** |
| Native JSON, 500,000 rows | not measured | **0.47** | not measured |
| Range top-k, rows only | 2.06 | 1.56 | **0.94** |
| Expression ordering, rows only | 1.48 | 1.08 | **0.89** |
| Substring filter, rows only | 1.60 | 1.45 | 1.02 |
| Regular expression filter, rows only | 1.56 | 1.50 | 1.07 |

The streamed TSV export of 500,000 rows went from 134.7 ms to 72 ms, where eager takes
75 to 84 ms. The filters over a scan stay slower at 12 threads because the cursor scans,
copies and filters one batch at a time, where eager filters the scan's runs in parallel.
A morsel-parallel scan and filter in the cursor would close that gap and is left to the
next part of item 7. Range top-k runs as an eager `IndexTopK` inside the cursor, so
streaming it saves no memory. A filter over a scan in native JSON at one thread is also
left open. It takes 1.35 to 1.39 times as long streamed, because the cursor's sorted batch
decode of unique IRIs costs more than eager's per-row lookup.

The first build of the writer pulled the second batch before it wrote the first, so the
first rows of every streamed answer waited for two batches. With the first batch written
in turn, on the development laptop at 10.5M with four threads on its performance cores,
the first row of a substring filter came after 0.37 ms instead of 1.25 to 1.43 ms, and
that of a regular expression matching two names after 9.0 ms instead of 15.6 ms. Scans of
1,000 to 500,000 rows took the same time either way. Decoding ahead also made a tight
memory budget fail a query that the writer in turn completed, which the room it now
requires prevents. Decoding ahead runs on the global Rayon pool, so under many concurrent
queries a writer can wait for its decoding task where decoding in turn would not wait.
That case has not been measured and is left to item 7.

Automatic execution now streams a plain scan, its projection or a `LIMIT` over it from an
estimated thousand rows, where it needed a million before. On forge a cursor wrote such a
scan faster than eager execution from about a hundred rows on, at one thread and at 12,
in TSV and native JSON. [BENCHMARKS](../BENCHMARKS.md#streaming-execution) gives the
library times of one probe run, 0.259 ms streamed against 0.332 ms at 1,000 triples and
1.126 ms against 1.604 ms at 5,000 with 12 threads. An earlier probe run measured 0.22
against 0.24 ms and 0.85 against 1.10 ms. Filters, top-k and orderings stay eager by
default, since streaming them is still slower at 12 threads.

The server remembers a query that answered in under 10 ms as quick and runs its next
request eagerly in place, without a thread hop or the automatic decision. Only eager
answers were remembered at first, so a scan of a few thousand rows that streamed on its
first run went through the blocking pool and the streamed writer on every run. A streamed
answer that completes in one body, which the server buffers up to 1 MiB, is now
remembered with its time as well. A stream that `send` stops early has not run the whole
query and is not remembered. [BENCHMARKS](../BENCHMARKS.md#streaming-execution) measures the threshold
over HTTP against af4a79d6, which ran these scans eagerly. With one Rayon thread on forge,
scans of 20,000 and 50,000 rows answered 7% to 15% faster on their first run, and scans of
1,000 and 5,000 rows matched. On a Namespace AMD EPYC instance every size was within 5%,
and a repeated scan that answered in under 10 ms ran in place as fast as before.

An uncached OPTIONAL COUNT on a single key, which automatic execution streamed from an
estimated million rows, now runs eagerly. The pre-push screen of af4a79d6 measured the
benchmark's `optional-count` at 10.5M 1.19 to 1.23 times slower under automatic execution
on Namespace's AMD EPYC instances, while forge's i5-13500 measured 0.99. On a Namespace
instance with one Rayon thread the cursor took 3.55 ms against 2.92 ms eagerly, both in
place and on the blocking pool. `perf` put 83% of the cursor's samples in the merge count,
whose branches on the comparison of the two key columns mispredict when the left and
right keys interleave, as students and employees do in id order. Eager execution counts
with a branch-free merge over several lanes. The tradeoff is memory, since the eager
count held 20.8 MB at 10.5M where the cursor held 2.2 MB. A branch-free count in the
cursor is left to the next part of item 7.

The JVM library now runs queries in automatic mode by default as well. ARQ's result sets
are lazy iterators that are valid only while their query execution and transaction are
open, and errors already surface from `hasNext` and `next`, so a streamed result keeps the
contract Jena callers rely on. Unset `Sparkles.STREAMING_EXECUTION` is automatic
execution, true streams every SELECT and false runs every query eagerly. Queries in a write
transaction still run eagerly, because a cursor cannot read the transaction's changes.
`FfiQuery::execute_with` makes the decision, and when it streams, `FfiQuery::next_batch`
reads the cursor and can fail. Its batches share one term dictionary up to the dataset's
term cache size, as an eager result's batches do, and restart it when it is full. The
encoder decodes a term it has not seen without charging it to the query's memory budget,
as eager execution decodes its results, so a row of very large literals is held
uncharged while it is encoded. That is the one exception to the cursor's budget
accounting, and `QueryBatch::transient_term` is public only for this encoder. A query
iterator now refuses to read once its transaction has ended and throws `JenaTransactionException`, eager or streamed,
as Jena's own iterators do, instead of handing out the rest of an eager result.

The first build of the JVM change exported 500,000 rows through JNI in 169 ms against
126 ms eagerly on forge at 1.05M. Profiles of the native side put the gap in the select
cursor's encoder. It decoded each cell and charged the term and its vocabulary block's
scratch to the memory ledger, and it restarted its dictionary every batch. The encoder now
keys its dictionary by id, decodes a term only when the dictionary lacks it, decodes it as
eager execution does without the transient charges, and keeps the dictionary across
batches. The native export then took 52.3 ms against 51.3 ms eagerly, and the JNI export
130 ms against 126 ms on disk and 129 ms against 124 ms in memory. The binding comparison
of `scripts/bench-bindings` ran every case twice in each mode in alternating processes.
No other query, `find`, `contains` or `getProperty` moved by more than run-to-run noise,
and the small-query latencies at one thread stayed within 2%. The Jena adapter tests now
cover automatic execution: result iteration, early close, rewindable and copied result
sets, `ResultSetFormatter` in TSV, JSON and XML, CONSTRUCT and DESCRIBE through graph
iterators, timeouts and `abort` between reads, and reads after a read transaction ends.
Jena's contract suites passed on the new default with and without the hand-written JNI
calls.

The regression of `regex-iri` by about 5% on forge since 6f0f928c has no source change
behind it. `perf stat` over 2000 requests at 1.05M with one thread counted 3.7% more
instructions and 5% to 10% more cycles for 6f0f928c than for bb379b91, while `contains`
moved by 0.6%. In the slower build the regular expression search runs out of line, and
`KeyFilter::test` shrank from 0xa48 to 0x82a bytes. Unrelated commits changed the
inlining decisions of thin LTO with four codegen units. Giving the eager key filter
explicit per-clone `regex_automata` caches, as the cursor's regular expression filter
has, would make the call independent of that inlining, and is left to item 7.

### Batch kernels and disk spill (2026-10-17)

X08 Phase 2 moved the cursor's joins, counts, groupings and DISTINCT to column kernels
and added disk spill for sorts, groupings and unordered DISTINCT
([X08 Outcome](X08-disk-spill-and-server-memory.md#phase-2a-batch-kernels-2026-10-17)).
Three of the follow-ups above changed with it.

* The cursor's merge count is branch-free. An OPTIONAL count measured 0.88 times eager
  execution at 1.05M with one thread, and 0.80 and 0.79 at 10.5M with one and eight
  threads, on an 8-vCPU AMD EPYC Namespace instance, where it had measured 1.19 to 1.23.
  It streams again under automatic execution, now as one case of a wider rule.
* Automatic execution streams a grouping whose input is estimated at 20,000 rows or more
  when every operator of the plan has a cursor implementation, under Project, Slice, BIND,
  FILTER and ORDER BY. Every grouped query of the screen, seven shapes including counts
  over OPTIONAL and MINUS, measured 0.50 to 0.91 times eager execution at 1.05M and
  10.5M, at one thread and at eight. Below 20,000 rows the cursor's fixed cost of a few
  microseconds is not repaid, and the smallest measured input was 30,000 rows. Joins
  without a grouping stay eager. `star-join` measured 1.03 to 1.16, `regex-iri` 1.26 to
  1.30, and `x-join-out` and `x-hash-join` 0.98 to 1.28.
* With spill configured, automatic execution also streams a SELECT of cursor operators
  whose largest estimated operator output exceeds half of the memory left, and an eager
  SELECT that fails its memory budget runs once more as a cursor
  ([X08 §6.8](X08-disk-spill-and-server-memory.md#68-eager-queries)).

`ExecutionMode::Auto` and `auto_streaming` in `cursor.rs` document the rules, and the
decision tables are `automatic_admission_requires_the_measured_shared_scan_conditions`,
`automatic_execution_streams_groupings_over_large_inputs` and
`automatic_admission_streams_plans_expected_to_outgrow_memory_when_they_can_spill`.

The in-memory cursor ORDER BY no longer reserves three times its input before the sort,
which charges its own index, copy and key values. It had charged both.

These follow-ups stay with roadmap item 7. A morsel-parallel cursor scan and filter would
close the gap of filters and top-k at 12 threads. A range top-k still collects its range
inside the cursor. EXISTS and DESCRIBE still read their whole input first. The eager
`regex-iri` key filter still needs per-clone `regex_automata` caches. A one-key index
join that read larger input batches, so that it seeks its index less often, was tried
and set aside unmeasured, since automatic execution runs those joins eagerly.

### Parallel cursor filters (2026-10-18)

A cursor FILTER over an index scan now reads the scan itself in morsels of whole index
blocks and filters each morsel in parallel, as eager execution filters a whole scan. The
filter chooses morsels on its first read when eager execution would filter the same scan
by key runs, the scan reads only immutable blocks over exact key ranges, the first demand
is at least 4,096 rows, and some conjunct of the filter reads only the scan's first free
key column. The Rayon tasks of a morsel each take a piece of a block. A task gathers the
piece's distinct values of that column, tests each once, either against the vocabulary
through a key filter or through the expression cache for inline values, and gathers the
rows that pass without branches. The other conjuncts run on each output batch. Morsels
follow the filter's input size, from 32,768 rows up to 262,144, and only those parts of
a morsel that a STRSTARTS prefix range can match are read. Output keeps the scan's
order, so a LIMIT stops after the morsel that fills it.

Each worker keeps its own copy of the key filter in a pool that lives as long as the
cursor. A copy of a regular expression filter starts with a cold matching cache, and
copying the filter for each morsel kept the regular expression query slower streamed
than eager at eight threads. When the row budget or the memory ledger no longer has room
for a block, the filter hands the rest of the scan back to the scan's own batches from the
next unread block.

A morsel charges its input columns, the passing rows it holds until they are written and
a scratch area of 32 bytes per block row for each worker. On an 8-vCPU AMD EPYC Namespace
instance at 10.5M the cursor's ledger peaked at 13.5 MB for the CONTAINS filter with
eight threads, against 11.8 MB before and 15.6 MB eagerly, and at 5.2 MB with one
thread. More threads hold more morsel scratch at once, which is the memory this speed
costs.

Two designs were measured before this one. Reading each morsel in three parallel passes
with a barrier after each was slower than eager execution at eight threads. Copying the
rows of each run of equal values was slow for a column whose runs are mostly one row
long, such as language-tagged names. The single pass over distinct values replaced both.

The table gives the streaming time divided by the eager time at 10.5M on the same
instance, the median of seven rounds, reading every row. The base is 5c73ced6.

| Ratio of streaming to eager | Base, 8 threads | Now, 8 threads | Base, 1 thread | Now, 1 thread |
|---|---:|---:|---:|---:|
| CONTAINS on names, 49,837 rows | 1.95 | **0.91** | **0.95** | 1.00 |
| REGEX on subject IRIs, 1,000 rows | 2.11 | 1.08 | 1.14 | 1.15 |
| STRSTARTS on names, 49,837 rows | 7.99 | **0.94** | 2.98 | **0.90** |
| LANGMATCHES on titles, 500,000 rows | 1.48 | **0.95** | **0.84** | **0.88** |

### Streaming range top-k (2026-10-18)

A cursor no longer runs a range top-k (`IndexTopK`) as an eager operator. The operator
still reads the parts of its range whose ids follow value order from the best end and
stops once it has its k rows. A part whose ids say nothing of value order, such as
values kept in the dictionary, used to be read whole into memory before its rows were
ranked. The cursor now reads such a part in morsels of one index block, a wave of as
many morsels as Rayon has threads at a time. Each morsel keeps its own best k rows, and
the morsels then offer their rows in index order to one bounded heap, so ties fall as
they do eagerly. A value that is not totally ordered still sends the query to the
generic plan, which the cursor builds as the operator's child and which streams as well.
Eager execution keeps its own path.

The cursor's memory for the operator is about one morsel's working set per worker
thread plus the k rows it keeps, whatever the size of the range. The test
`ordered_top_k_memory_does_not_grow_with_its_unordered_pieces` reads 300,000 and 900,000
dictionary values on two threads and requires the second peak to stay within a quarter
of the first. The peak grows with the number of threads instead, since each holds a
morsel at once, so the test fixes its pool. Running the morsels in waves of one per
thread keeps a waiting worker from starting further morsels by work stealing.

Reading the morsels one after another was 1.72 times slower than eager execution at eight
threads on the Namespace instance, so the morsels run in parallel. At 10.5M the range
top-k over salaries measured 1.06 times eager execution at eight threads, the median of
21 rounds, where it measured 1.33, and 0.88 at one thread, where it measured 0.93. The
cursor's ledger peaked at 6.1 MB with eight threads against 5.6 MB before, and at 3.3 MB
with one thread.

### Large basic graph patterns

[ROADMAP](../ROADMAP.md) item 84. The planner joins the triple patterns of a basic graph
pattern into a binary join tree with a join and a sort for each pattern, and the planner,
the executor and the plan description all recurse through it. A query is refused when its
plan would nest deeper than 4000 levels, as the
[nesting limits](../API.md#nesting-limits) describe, so a basic graph pattern holds at
most 1998 triple patterns.

Item 84 made the plans at that limit cheap to plan, run and write, and made planning stop
at the request's timeout or cancellation. The ignored test `large_bgp_planning_time` in
`crates/sparkles/tests/nesting.rs` measures each shape of `plan_shapes` at the limit in a
release build. The table gives its numbers on the development laptop before and after the
change. A shape's name gives its number of triple patterns, and the plan JSON is what
`/{ds}/explain` writes. A chain of 500 now plans in 43 ms and writes 1.9 MB, against
66 ms and 3.5 MB before, and a chain of 1000 in 92 ms and 7.4 MB, against 193 ms and
13.4 MB. Other builds shared the laptop during both runs, so the times are good to
about a fifth. The star joins every pattern to the others through one variable, with the
predicates `<urn:p>` and `<urn:q>` alternating, and the hub binds one variable in the
predicate position of every pattern of a chain. The two rows come from the same shapes
run through `explain` on a build of the commit before item 84 and on this one, the least
of three runs each, over the four triples of the test.

| Shape at the limit | `explain` before | `explain` after | Plan JSON before | Plan JSON after |
|---|---|---|---|---|
| Chain of 1998 | 1.08 s | 0.35 s | 53.7 MB | 29.7 MB |
| Reversed chain of 1998 | 1.01 s | 0.27 s | 64.5 MB | 32.5 MB |
| Tree of 1998 | 2.24 s | 0.34 s | 37.4 MB | 22.4 MB |
| Pairs of 999 | 1.36 s | 0.49 s | 34.1 MB | 18.1 MB |
| Chain of 999 with a second predicate | 0.67 s | 0.19 s | 26.2 MB | 11.2 MB |
| Chain of 571 with VALUES | 0.43 s | 0.11 s | 15.9 MB | 7.3 MB |
| Chain of 1019 with filters | 71 ms | 34 ms | 1.3 MB | 1.3 MB |
| Chain of 799 with OPTIONAL | 6.49 s | 0.53 s | 23.3 MB | 13.7 MB |
| Chain of 799 with MINUS | 2.49 s | 0.15 s | 8.6 MB | 4.8 MB |
| Chain of 799 with BIND | 5.88 s | 0.18 s | 21.2 MB | 13.5 MB |
| Star of 1998 | 3.35 s | 0.73 s | 23.1 MB | 15.1 MB |
| Hub of 1998 | 0.98 s | 0.77 s | 53.8 MB | 29.8 MB |

The gain depends on the shape, from 1.3 times for the hub to 33 times for the BIND, and
a plain chain or its reverse is 3 to 4 times faster. A star over data does not always
gain. A star of 1998 patterns on `foaf:knows`, which has the most triples of any
predicate in `scripts/gen-data.py 3000`, a dataset of 31,815 triples, plans in 2.6 to
2.8 s against 2.9 to 3.0 s before in three runs on each build, a gain of 1 to 13%. The
quadratic cost of a variable that many patterns share, described below, dominates it.

Before the change, a plain chain, its reverse and the chain with a second predicate ran
past a timeout of half their planning time and returned a plan. Every shape now stops
with `timed out` within half of its planning time past the timeout, or within a second,
which the test `planning_at_the_limit_stops_at_the_timeout` checks at a quarter and at
half of the planning time, and the same holds for a cancellation. The eager run of a
chain at the limit did not finish in ten minutes, and a cursor over it took about four
minutes to open. Each now takes 0.12 to 1.04 s for every shape, and a cursor opens in 64
to 890 ms. The test measures the planning time as the least of three runs, and a nextest
group runs it alone on the machine, since other work stretches the time that a stop takes.
Whether the description of a plan checks the deadline for each node is tested in
`sparkles-core` with a deadline that has passed and a count of the checks, which needs no
clock.

The planner did several kinds of work for each join that grew with the plan below it.
The join ordering copied an input that is a whole subtree, such as the plan of the
patterns before an OPTIONAL, to cost it and again to build the plan, so an OPTIONAL
after each pattern copied the plan once per OPTIONAL. It now moves each input into the
plan it builds and copies one only to place a filter on it. The greedy ordering tested
every pair of plans at every step. It now pairs only the plans that share a variable,
which it finds through a list of the plans that bind each variable, and it keeps with each
plan what its inputs hold on the join variables, the star masks and the probed sides, rather
than gathering them from every input at each pairing. The estimate of a join read the
star masks of every pattern below both sides and kept only those of its join variable.
It now reads only those. The union of two plans' variables and their distinct-value
estimates tested membership with a linear search, which costs the product of their
variables near the root of a deep plan, and they use a hash set once both sides are large.
The connected parts of a group of more than 32 inputs are found through a map of
variables instead of a test of every pair of inputs. None of these changes a plan, since
the costs, the estimates and the tie breaks are the same, and the planner tests pass
unchanged.

Keeping the star masks and probed sides with each plan trades memory and a little time
for speed. Each unit holds a sorted vector of the join variables its inputs hold and
merges it at every join, so planning a snowflake of 12 patterns costs 2.6 to 3.2% more
instructions than before, and a plan of n inputs whose variables all have star masks can
hold a vector of n entries for each unit. At the limit the worst case is about 1998 times
1998 times 32 bytes over two, or 64 MB, which the greedy ordering frees when it ends. The
lists of the plans that bind each variable add about 100 KB. The trade is made because the
alternative gathers the same facts from every input at each pairing, which is the work the
table above removes. Ordinary queries have too few inputs for the vectors to matter.

The greedy ordering checks the timeout and the cancellation for each input, for each
plan whose pairs it costs and for each plan it builds, and the dynamic program for each
plan it builds. `explain` checks them for each node it describes, describing a node
before its children so that each check follows the work of one node. Each operator of a
cursor describes its own node instead of the subtree below it, which it discarded. A key
of the result cache writes the variables of every node of its subtree, so the keys of a
deep plan grew with the cube of its depth. A subtree whose nodes hold more than 16,384
variables in all, a key of about 100 KB, is no longer cached. The subtrees of ordinary
queries hold far fewer. The order of the columns of a `SELECT *` result is found once per
variable, and the parser tests whether a selected variable is new with a set once there
are more than 32. A merge join of a cursor finds its columns in its inputs through a map
when both are large.

A plan deeper than 256 levels is written in plan format 2. Its root names `"planFormat":
2`, and its nodes have no `id`. The ids were paths from the root and took 24 MB of the
53.7 MB of a chain at the limit, so the JSON grew with the square of the depth. A client
derives the same ids from the shape, as the plan view of the UI already did and the reader
of `/{ds}/sparql/explain` now does. A plan of 256 levels or fewer keeps its ids, and every
query of the test suites plans far less deep. The columns of each node still name every
variable it binds, so the JSON of a chain stays quadratic in the number of its triple
patterns, as the plan's own size is. `/{ds}/sparql/explain` refuses a plan whose
`planFormat` it does not know, and so does the plan view of the UI, which shows a message
that it cannot show the plan. The OpenAPI description and the generated types of the
remote JavaScript client list `planFormat`.

A parsed query or update that the nesting check refuses is taken apart without
recursion, so a shape whose text the nesting scan undercounts cannot overflow the stack
when it is dropped. The explanation reader and its walk of the patterns use a list of the
nodes still to visit, and so do the command-line plan printers. The explanation keeps its
limit of 256 levels below the root and 10,000 nodes, because it names each node by its
path in its `nodes` and notes and in the `explain_query` text, which would grow with the
square of the depth. The engine writes format 2 only above 256 levels, so the one
format 2 plan that the explanation can read has exactly 257 levels, the root and 256
below it.

The change costs ordinary queries nothing that a count of instructions shows. Eleven
small queries, from a single triple pattern to a five-pattern join, an OPTIONAL with
FILTER, ORDER BY and LIMIT, two property paths, a grouped COUNT and a UNION, ran between
0.95 and 1.01 times the user-mode instructions per query of the base, counted in process
as the best of five runs on one performance core. The one at 1.01 varied by 5% between
runs of the base. A chain of 30 triple patterns ran 0.69 times the instructions of the
base.

Planning time still grows with the square of the triple patterns. A variable shared by
many patterns costs the most. The index of pairs lists every plan that binds a variable,
so a variable bound by k plans costs k squared over two candidate pairs at the start and k
pairs at each of k steps, which is why a star of 1998 patterns plans in 0.7 s on a tiny
dataset and in 2.6 to 2.8 s on a larger one, which gained the least. Pairing only the
cheapest holders of such a variable would change plans, so item 84 leaves it to the n-ary
join. Each join keeps a map of distinct-value estimates for all its variables, each
estimate of a join walks the patterns below it for its star masks, and variable names are
interned by a linear search. An n-ary join would let the limit count only the levels that
nest. The planner would emit one join node for a basic graph pattern whose join tree is
left-deep, holding its inputs in join order with the algorithm, the keys and the sort of
each step, so the plan is one level with an input for each pattern. The eager executor
would fold the inputs in a loop, a cursor would keep the binary operators of the steps in
a list and pull through them without recursion, and the plan description would write the
steps as the children of one node. The bushy trees that the greedy order makes for some
shapes would stay binary. The depth walk would then count a basic graph pattern as two
levels, and the limit could rise to what the text allows. That changes the planner, both
executors and the plan format, so it stays on the roadmap. Growing the stack at the
recursion points of the planner and the executor, or an iterative clone and drop of plan
nodes, would also make the stack independent of the count of the walk.
