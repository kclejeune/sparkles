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
table. Bounded response serialization alone cannot do this: the original eager query executes
to completion before the writer sees its first row. Intermediate tables can exceed
the final answer, so paging the final table is insufficient as well.

The design adds synchronous, fallible pull execution over immutable snapshots.
Operators produce bounded columnar ID batches and retain only the state needed to
resume. The serializer consumes batches directly; bounded transport buffering makes
slow readers stop upstream execution. The existing columnar executor remains available
for small queries and operators that initially lack an incremental implementation.

**Goals**

1. Return the first batch before scanning or constructing the complete answer where
   the algebra permits it, and stop promptly at LIMIT, cancellation or consumer close.
2. Preserve SPARQL multiplicity, unbound values, expression errors, ordering, graph
   visibility, blank-node identity, RDF-star and existing query-extension semantics.
3. Account live batches, retained operator state and query-created terms. Fail with
   structured budgets rather than silently collecting unbounded state.
4. Make materialization barriers and memory growth visible in plans. Incremental
   production and constant working memory are separate properties.
5. Preserve current small-query performance and collected-result APIs. Enable broader
   selection only after matched correctness, latency and memory measurements.

**Non-goals of the initial phases**

- Streaming UPDATE publication or changing commit durability.
- A universal constant-memory algorithm for every query; some operators require
  growing state, full-input processing, external storage or an explicit budget failure.
- A server-wide query admission pool, distributed execution or persistent cursors
  resumed by a later HTTP request.
- Changing RDF loading thresholds, result syntax or query semantics to accommodate
  batching. A caller collecting every batch still retains the complete answer.

## 2. Existing engine and compatibility

The baseline is the code at `a6b6b449`, examined before this design:

| Component | Current behavior | Required relationship to X05 |
|---|---|---|
| `sparql::query` / `execute_query` | Parse/plan and return a complete `QueryResult` | Remain compatible; share semantic preparation with the cursor |
| `exec.rs` / `Table` | Materialized columnar operators; some LIMIT paths repeat increasingly large prefixes | Retain as the eager executor and initial fallback; cursor scans resume instead of replaying prefixes |
| `Snapshot::scan_between_cols` | Callback traversal merging immutable blocks and delta inserts/deletes | Provide resumable traversal with the same visibility and ordering |
| `Ctx` | Query-local vocabulary, decoding caches, limits, cancellation, extensions and access view | Retain correct lifetime and add accounting needed by cursors |
| `results.rs` | Writer-based serialization of completed results | Add a consumer of cursor batches |
| HTTP `SwitchWriter` | Bounded serialized bytes after a small-body buffer | Connect producer backpressure to execution and retain controls through body completion |
| `Dataset::select` | Fully decoded `Solutions` | Keep its contract; add an explicit cursor API |

Cursor and eager execution use the same parsing, scope/depth validation, initial
bindings, SELECT variable order, dataset defaults, protocol dataset, RDFS and
materialized-inference overlays, access restrictions and extension resolution.
Factor those preparation steps where needed; do not maintain a second interpretation
of query options. Historic queries capture the resolved historical snapshot and its
vocabulary/index eligibility, as in [F06](F06-snapshots-and-point-in-time.md).

Existing APIs keep their eager default through Phases 1–3. A cursor's explicit
collection method may implement the same result representation, but replacing the
existing eager implementation is a separate performance-gated decision. No format,
generation migration or new dependency is required by Phases 1–2.

## 3. Cursor API and lifetime

### 3.1 Public contract

The initial API accepts SELECT only. The following names describe the intended
surface; implementation may refine type names while preserving these contracts:

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

Opening captures a snapshot, options and immutable extension registry, validates and
plans the query, and determines its output columns. It does not execute a whole
fallback subtree. Planning probes/samples must themselves obey existing budgets and
cancellation; strict incremental mode must not hide full execution in planning.
ASK, CONSTRUCT, DESCRIBE and UPDATE are rejected by the SELECT API before producing
rows. They keep their existing APIs until Phase 4.

`CursorOptions` supplies a maximum batch row count, an ID-buffer byte target and a
fallback policy (`AllowMaterialization` or `RejectMaterialization`). Invalid zero
limits fail at open. Initial defaults are 4,096 rows and a 1 MiB ID-buffer target,
subject to the remaining query memory budget. These are engineering starting points,
not measured optimal values or the loader's byte thresholds. A batch never exceeds
the row cap. At least one row may exceed the byte target if the query memory budget
permits it; a single over-budget row fails rather than being truncated. Variable-width
decoded values are accounted separately (§5).

While Open, `None` means successful exhaustion; terminal Failed/Stopped cursors also
return `None` on subsequent fused pulls (§3.2), so the terminal status determines
whether the answer is complete. An empty batch is never returned to signal temporary
lack of output. Filters may need several input batches to produce one output batch.
Zero-column solutions carry an explicit row count, including duplicates.

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

The cursor supports movement between threads (`Send`) and sequential pulls through
`&mut self`; concurrent pulls are not supported. Each pull and guarded teardown must
preserve algebra stack-depth protection and the callback family/failure boundaries
of [P03](P03-query-extensions.md), including when invoked on a different thread.

A `QueryBatch` owns immutable columns and a charge on the shared query budget. It
retains the term resolver needed for its IDs, so it remains readable after the cursor
closes or drops. Public access exposes variables, rows and decoded RDF terms, without
exposing a context/storage handle that bypasses access restrictions. Any borrowed
term view is tied to the batch's lifetime; owned terms copied by application code are
application memory. Unbound cells are explicit and distinct from valid RDF terms.

The state transitions are:

```text
Open -> Complete              root exhausted, including its query LIMIT
Open -> Stopped               explicit close or a consumer output cap
Open -> Failed(error)         execution, timeout, cancellation or budget failure
```

Successful exhaustion and terminal errors release operator buffers and temporary
resources. Previously returned batches retain only their own required ownership.
After a terminal error, `next_batch` is fused: it returns `None` on later calls, while
the failure remains recorded. `collect` refuses a Failed/Stopped cursor or one that
has already delivered a batch to another consumer. Otherwise it drains and collects
the complete answer; it cannot recover previously yielded rows or return only the
remainder as a successful complete `QueryResult`.
The Python cursor wrappers differ at this point because Python's iteration protocol
cannot carry a status. After a failure, each later `next()` raises an error that names
the original failure, so a loop cannot end as if the answer were complete.
Close is idempotent and preserves a prior Complete or Failed state. Stats never drain
the cursor. Completion may be known with the last batch; it need not await an extra
pull when the root has already proved exhaustion.

Drop/close releases this cursor's production state without setting a caller-supplied
shared cancellation flag, which may control other queries. It does not drain input.
No background producer exists in the core API. Retained batches remain charged; they
can keep snapshot/resolver resources alive after close.

Phase 1 does not expose cursors that escape an open write transaction. Such an API
requires an explicit lifetime/family design and foreign-language owner guards; it
must not acquire a writer lock already held by the caller or continue callbacks after
transaction teardown. Existing transaction query behavior is preserved.

## 4. Physical execution

### 4.1 Resumable scans and unary operators

Phase 1 pipelines Empty/VALUES, ordinary index scans, supported range scans, FILTER,
BIND/Extend, projection, Slice/OFFSET/LIMIT and UNION where each child is supported.
Existing index-order guarantees may be carried through; specialized ordered top-k
paths qualify only if they resume without collecting/replaying their full input.
An operator name alone does not establish eligibility.

Scan state retains its range position and any pending block/delta position, graph
selection, repeated-variable equality checks and merged-default-graph duplicate
state. State survives a batch boundary, including one inside an immutable block or
an equal-triple run across graphs. Resume neither skips nor repeats a key. Seek-based
resume is acceptable initially if it preserves the full ordering key, handles the
inclusive bound and maximum key correctly, and passes the performance gates.
Undecoded columns cannot supply an ordering key; a selective decode optimization
must carry enough real key data to resume correctly.

Operators never restart the query with a larger LIMIT to obtain the next page.
OFFSET consumes and discards input without retaining it. LIMIT stops child production
as soon as its required rows are established. UNION preserves multiplicity and only
opens the next arm when needed. Projection retains row counts for zero columns.
Filters and extensions reuse existing expression semantics, including unbound/error
handling and short-circuit evaluation. NOW is fixed for the query; volatile functions,
BNODE and callback invocation behavior cannot change because of batch size.

Long loops check cancellation/deadline at entry, exit and at least every 1,024 examined
candidates, including candidates producing no visible row. This is a work checkpoint,
not a promise that blocking I/O or user callbacks complete within a fixed wall time.

### 4.2 Capability and fallback visibility

Each physical node records its production mode (incremental or materialized), whether
first output requires its full input, retained state (fixed buffers, growing budgeted
state or spill), and the reason for a barrier. A supported external sort may consume
all input before emitting incremental output without materializing a complete RAM
table. Its blocking startup is reported separately from eager fallback.

The native ORDER BY reads cursor batches into charged state, sorts them and then
emits batches. Its plan node reports that it needs its full input before output and
gives the reason, while `materializes` stays false because no eager subtree runs. The
policy that rejects materialization therefore admits it. An ORDER key that evaluates
EXISTS runs subqueries outside the cursor, so that sort counts as materialization.
The root plan summarizes whether any subtree materializes and whether state can grow
with input/result cardinality. Partial runtime counts are explicitly partial.

`AllowMaterialization` lazily runs an unsupported subtree through the current budgeted
executor on its first demand, then serves batches from its retained result. It must
charge that result and any copied batch simultaneously. The barrier is visible before
the first pull. Exceptions inside FILTER/EXISTS, LATERAL, SERVICE or registered
callbacks are also barriers if their implementations retain whole subresults; a
streaming parent cannot conceal them.

`RejectMaterialization` rejects a known unsupported subtree before output. A dynamically
planned subtree is rejected when reached, before executing it, and the possible
dynamic barrier is visible in the initial plan. This policy forbids fallback, not all
growing state: generated terms or a supported DISTINCT may still grow under a budget.
Do not label such a plan constant-memory. Caller-supplied VALUES and query syntax are
input-sized state and are reported separately.

### 4.3 Joins, state and blocking operators

Phase 3 extends coverage in independently reviewable steps:

| Operator family | Incremental design | State or barrier to expose |
|---|---|---|
| Ordered/merge join | Resume both inputs and emit bounded products | Equal-key runs can be arbitrarily large; budget or spool them |
| Index/nested-loop join | Keep driving-row/batch and probe position | Bound probe batches and preserve repeated driving keys |
| OPTIONAL/LeftJoin | Resume matches; emit unbound extension only after proving no qualifying match | ON-expression rules, unbound keys and unmatched state |
| MINUS / EXISTS / semijoin | Probe or build eligible key state | Compatibility/domain rules; correlated fallback where needed |
| Hash join | Budget the build side and stream probes; partition/spill where supported | Build bytes, skewed partitions and output products |
| ORDER BY / top-k | Budgeted run generation and merge; bounded heap for eligible LIMIT | Full-input barrier unless index order proves early termination |
| DISTINCT / REDUCED | Budgeted exact keys; partition/spill for DISTINCT | DISTINCT across batches; REDUCED retains its allowed semantics |
| GROUP / aggregates | Budgeted groups or ordered aggregation; spill eligible state | Empty groups, DISTINCT aggregates and accumulator ownership |
| Paths / text / vector / geo / SERVICE | Retain specialized algorithms where compatible | Frontiers, candidate sets, remote pages and existing extension state |

Rewrites require the existing algebra/scoping proofs. A large duplicate join run,
unbound wildcard keys or skewed hash partition must not allocate outside the budget.
An implementation may fail with a budget error instead of spilling until its spill
algorithm is accepted. Unsupported custom accumulators or property functions stay
behind an explicit barrier; P03 invocation-local expression errors must not leave
already-published provisional rows from a discarded invocation.

Spill requires reserved per-query and aggregate server disk bytes, safe temporary
file ownership, cancellation/error cleanup and bounded merge fan-in. Disk exhaustion
is a structured resource failure. Crash cleanup removes only positively identified
query temporaries. Spill cannot write plaintext values from an encrypted dataset:
use suitable ephemeral encryption under [F11](F11-encryption-at-rest.md), or disable
spill for that dataset and fail within its memory budget. File formats, reservation
configuration and recovery details need an implementation design before Phase 3
spill lands; this spec does not authorize unrestricted temporary disk growth.

## 5. Budgets and accounting

This extends [C01](C01-observability-and-budgets.md); it does not weaken existing limits.

| Resource | Cursor interpretation |
|---|---|
| `max_rows` | Cumulative output rows of each physical operator across its batches, rather than a fresh allowance for each batch |
| `max_rows_produced` / shared `work` | Sum of rows produced by physical operators, retaining the existing shared request counter |
| `max_memory_bytes` | Estimated simultaneously live query-owned capacity and retained data, including outstanding batches |
| Result-byte budget | Serialized uncompressed bytes actually written; independent of row/batch count |
| Timeout | One absolute query deadline; consumer wait and transport backpressure do not reset or suspend it |
| Cancellation | Checked during production, decoding, serialization and interruptible transport waits |

Counters use checked/saturating arithmetic and fail on overflow. Counts are updated
as work is performed, so early stop and failure retain partial work. Batch splitting
must not change counts for a fixed physical plan, double-count ownership transfers,
or permit an unlimited stream through repeated below-limit batches. Different eager
and cursor plans may legitimately have different physical work; semantic parity does
not require their counters to be identical. Checks of hidden candidates do not expose
hidden cardinalities in access-filtered plans/reports.

Charge allocated column capacity, including zero-column row overhead, before growth.
Track parent/child batches, materialized fallback tables, hash/group/dedup/path state,
sort buffers, decoded terms, expression caches, blank-node ledgers and query-local
vocabulary. A BIND producing a different string per row can grow the vocabulary even
with tiny batches. Phase 1 must charge and fail that growth or implement safe reclamation;
it cannot defer the issue while claiming bounded query memory. Live batches keep their
local terms valid. Releasing cached decoded values between pulls is allowed only when
it preserves identities and observable function behavior.

Outstanding caller-retained batches remain on the same shared budget. Moving a buffer
transfers its charge; copying retains both charges until the original is released.
Collecting explicitly charges the growing final table and transient live batch, and
fails if it cannot fit. Partial tables are released on failure; collect never disables
limits to emulate the old API.

The accounting remains an estimate rather than an attributing allocator. Shared mapped
indexes, vocabulary and decoded-block cache are reported separately from query-owned
memory/RSS; their ownership does not become per-query just because a cursor pins them.
Transport/compression buffers have separately bounded capacities. A very large RDF
term may itself exceed a query budget; row batching does not split or truncate a term.
Unknown allocator/transient overhead is disclosed in measurements and must not mask
an input-sized unaccounted collection.

## 6. Serialization, HTTP and observability

Phase 2 adds writer consumers for standard SELECT JSON, XML, CSV and TSV. They write
the header, decode/serialize a bounded batch, release it and only then demand more.
The rich Sparkles JSON format follows with results first and final metadata after
successful exhaustion. Final timing/count/plan fields cannot be fabricated at open.
Jena binary formats require their own cursor adapters before being supported.

HTTP uses the explicit query parameter `execution=eager|streaming|auto`. Omitting it
means eager, and invalid values are rejected. The streaming mode permits visible
budgeted fallback, as in §4.2, and does not guarantee constant memory. When streaming
is requested explicitly, unsupported query forms or negotiated encodings are refused
before execution with an actionable error, rather than silently selecting eager
execution.

The `auto` mode streams only the plans that the measured admission rules accept, which
the API documentation lists, and runs every other query eagerly. It also runs eagerly
when the negotiated encoding has no cursor writer. SPARQL Results Thrift is such an
encoding, so `auto` with Thrift returns an eager answer while `streaming` with Thrift
is refused. The initial phase supports SELECT and the
four standard encodings; native JSON is enabled only with its completed adapter.
Update OpenAPI and API/usage docs when the option lands. Authorization, historical
snapshot resolution, substitutions and dataset defaults follow the existing handler.

The snapshot's dataset/commit identity and history headers are available before output.
Exact total rows, final execution timings and peak memory are not. A small answer may
still return as a whole body through the existing bounded switch writer. Once its byte
threshold is crossed, a bounded channel connects the blocking producer to the body.
Current 1 MiB switching and 64 KiB/four-chunk buffering are reusable, not a query batch
size. No unbounded row queue or full-result cache sits between execution and transport.

The producer does not retry an already-advanced cursor after a serialization trial.
Use a single-use writer path; the repeatable quick-result serializer remains for eager
results. Finite queued bytes and one in-progress batch can run ahead; once those fill,
the producer stops pulling. Channel waits observe cancellation/deadline without waiting
indefinitely for another write. A receiver drop wakes blocked sends and releases the
producer. The disconnect guard belongs to the body/producer lifetime, not just the
handler future; returning response headers cannot cancel the ongoing query.

Before response headers are committed, parsing, permission, budget and execution errors
use the existing HTTP status/error representation. After commitment, failures abort the
body transport and omit the success terminator/final metadata. Do not append an error
object inside a standard result document or return a normal EOF for a failed CSV/TSV
stream. The response status is no longer changeable. Direct writer callers receive
`Err` and must treat any bytes already written as partial output.

The existing `send` output cap may close the cursor after the requested prefix. This
is an intentional Stopped result, distinct from a root SPARQL LIMIT completing the
query. Its standard result document can close successfully, but the remaining total
is unknown; native metadata and completion logs identify the cap. Do not drain the
rest just to populate a count. Serialization/output failure is recorded separately
from execution completion if the cursor already exhausted successfully.

Stats distinguish parse/plan time, active execution time, emitted rows, physical work,
peak accounted bytes, and Complete/Stopped/Failed. Active execution time accumulates
planning-independent operator/pull evaluation and excludes consumer idle time and
blocked response writes; total elapsed time runs from open through termination and
includes waiting. Serialization and transport wait are separate measurements. Final
access-log/metric/span records are emitted exactly once when execution and response
terminate; an early response header cannot carry a fabricated final report. Existing
plan redaction applies to all capability, estimate and runtime fields.

Cursor mode initially bypasses the store result cache. It must never insert a partial
answer or replay callback effects through retries. Supporting a complete-result cache
later requires the same registry/access/snapshot isolation as eager execution and a
visible materialization cost.

## 7. Later query forms and bindings

ASK consumes the minimum necessary input and stops after a qualifying solution, with
the same truth/error rules. CONSTRUCT consumes WHERE batches but still needs exact
triple/quad deduplication and per-solution template blank-node identity. DESCRIBE needs
budgeted traversal and its existing truncation policy. These use distinct cursor
result types; SELECT-only types are not stretched to imply graph support.

N-Triples/N-Quads/Turtle-style output may consume graph batches once their adapters
are correct. RDF/JSON subject grouping and ordinary JSON-LD may require buffering or
spool; identify those barriers rather than calling every writer constant-memory.

Python iteration, Node async batches and JVM/Jena iteration should consume the native
cursor rather than split an already-collected answer. Define early close/finalizers,
abort/disconnect ownership, callback threading and transaction-owner guards in each
binding. The remote streaming client in [P02](P02-rust-client.md) is a transport
consumer; it does not make native execution incremental. Update P01/P04/P05/P06 parity
tables when their cursor surfaces actually ship. CLI and stored-query runners can use
the same writer adapters without creating another engine.

## 8. Delivery phases

| Phase | Deliverable | Landing gate |
|---|---|---|
| 1 | SELECT cursor API, shared preparation, resumable scans/unary operators, visible fallback, full cursor-owned memory/lifetime accounting | A1–A8, A11; eager defaults unchanged; cursor/eager performance and memory measurements |
| 2 | Standard SELECT writers, opt-in HTTP mode, disconnect/backpressure, final reporting; native JSON adapter separately | A9–A10, A12; API/OpenAPI docs; no replay; paired small/large-output measurements |
| 3 | Incremental join families, stateful operators and accepted spill strategies in separate slices | A13 plus affected earlier cases for each operator; skew/error/disk/encryption gates before enabling spill |
| 4 | ASK/graph cursors, binding/CLI integration, then optional automatic selection | A14; binding lifecycle tests and complete cross-scale performance evidence |

Automatic selection is deferred until measured. It can use plan barriers, estimates,
observed execution/serialization costs and memory risk. A guessed row-count threshold
cannot predict wide rows or huge literals. Keep an explicit eager mode for comparison
and rollback. Any change of the default has its own documented acceptance decision.

## 9. Acceptance examples

- **A1 — Early production:** over a million-row scan, obtain one batch without
  visiting the entire input. LIMIT 1 stops after sufficient qualifying rows. OFFSET
  and low-selectivity FILTER remain incremental and cancellable, including no matches.
- **A2 — Resume correctness:** compare eager and cursor bags with delta inserts/deletes,
  graph-union duplicates, repeated variables and boundaries inside blocks/runs. Include
  the maximum ordering key and a final batch smaller than the cap.
- **A3 — Result semantics:** compare SELECT variable order, initial bindings, duplicate
  VALUES/UNION, unbound BIND/FILTER, empty results and duplicate zero-column solutions.
  Compare ordered sequences only when query semantics require an order.
- **A4 — Terms/functions:** verify RDF-star and composite literals, query-created blank
  identities, NOW/volatile functions, short-circuit errors and P03 callback invocation,
  fatal-error and teardown rules at several batch sizes.
- **A5 — Snapshot ownership:** mutate/compact after open, drop the Dataset handle, and
  finish the original answer. Retain a batch after close and decode its local terms.
  Generation resources release after all owners drop.
- **A6 — Terminal behavior:** cover exhaustion, repeated close, consumer stop, errors
  after earlier batches, and fused pulls. Collect refuses partial/failed cursors;
  it also refuses collection after a batch has already been yielded to the caller.
  dropping one cursor does not set a shared caller cancellation token.
- **A7 — Budgets:** splitting batches cannot reset cumulative operator/work limits.
  Retain batches until the shared budget is exceeded, then release them. Test wide
  rows, a huge literal, unique-string BIND growth, caches, fallback and collect copies,
  overflow and failure cleanup; no charge leaks or hidden budget bypass.
- **A8 — Deadline/cancellation:** cancel during rejected scan candidates, filtering,
  callback work and between pulls. Consumer sleep does not reset the deadline. Check
  bounded work-check intervals rather than relying on an indefinitely hanging test.
- **A9 — Backpressure:** stall an HTTP reader until the bounded channel fills; verify
  examined/emitted rows stop growing beyond the bounded producer allowance. Disconnect
  and expire a deadline during a blocked send; reap the producer and release resources.
- **A10 — HTTP failures:** inject errors before/after header commitment, result-byte
  overflow, partial CSV/TSV and serializer failure after execution completion. Assert
  proper early status or late body error, no success footer on failure, and one final
  completion record. Verify resolved identity/history/auth and `send` stop semantics.
- **A11 — Capability honesty:** strict fallback policy rejects an unsupported static
  plan at open and a dynamic barrier before its execution. Allowed fallback appears in
  the plan and remains budgeted. A budgeted native sort is reported as a full-input
  barrier, not as fallback, and the strict policy admits it. Access redaction conceals
  protected details.
- **A12 — Writer parity:** parse completed JSON/XML/CSV/TSV and native JSON outputs and
  compare answers to eager output. Encoded byte counts match result-byte enforcement;
  metadata is final only for the appropriate completion state.
- **A13 — Stateful operators:** exercise OPTIONAL with unbound/multiple matches and
  expression errors, MINUS compatibility, correlated EXISTS, duplicate/skewed join
  runs, global DISTINCT, ordered ties, empty groups and aggregate failures. Spill cases
  cover reservation denial, disk errors, cancellation, crash cleanup and encryption.
- **A14 — Graph/binding lifecycle:** compare graph sets and template blank nodes;
  respect DESCRIBE truncation. Binding iteration breaks, aborts and GC release the
  producer; transaction-owner guards prevent lock reentry and escaped execution.

## 10. Performance acceptance

Measure the same admitted source/toolchain, dataset, cache/CPU conditions and query
semantics. Retain raw samples and negative outcomes. Use counterbalanced repeated
eager/cursor runs and enough independent processes to identify the previously observed
runtime regimes; a cold/fast/slow process must not be silently excluded as noise.

For each implemented slice, report first-batch/first-byte latency, complete execution
and response latency, throughput, p95, physical work, peak query estimate and process
RSS with the shared cache distinguished. Include small answers, full scans, wide/large
literal answers, joins, sort/top-k, OPTIONAL, DISTINCT/groups and slow consumers.
Run matched 1M and 10.5M controls first; validate large-answer memory and representative
queries on the retained 1B+ dataset once resource headroom permits. Competitor references
may remain separately dated; these gates do not require unrelated competitor reruns.

For supported scan/unary pipelines without growing term/operator state, memory after
warmup must follow batch/live-state bounds as answer cardinality increases, rather
than the complete answer size. Growing-state plans must report that growth and either
stay within their budget, spill, or fail. Report first-batch gains separately from
full-response cost; early output does not excuse a material throughput regression.

Initial performance target: no reproducible loss above 5% in median or p95 for existing
small-query eager paths, or for a workload selected into streaming by a future default.
Investigate repeatable smaller losses as well. This is an admission target, not a claim
of statistical significance: evidence must distinguish variance from a stable cost.
An unresolved regression keeps the affected path opt-in or on the eager default.
Sweep batch targets before promoting the provisional defaults or adding auto thresholds.
Public BENCHMARKS and README tables change only after completed representative runs.

## 11. Alternatives and decisions

- **Page a finished result:** useful API pagination, but leaves full execution memory
  unchanged. It does not satisfy this feature.
- **Re-run increasing LIMIT prefixes:** repeats scans, expressions and callback effects,
  and can accumulate excess work. A cursor retains resumable positions instead.
- **Replace all eager execution immediately:** loses a fast comparison path before
  equivalent coverage and costs are established. Begin with explicit cursor use.
- **Producer threads in the core API:** add queues and cancellation lifetime problems
  without helping a synchronous consumer. Core execution is pull-based; HTTP/bindings
  supply their existing blocking/async bridges with bounded transport.
- **Hide fallback or local vocabulary growth:** produces misleading memory guarantees.
  Report capabilities/state and charge retained query-created terms from Phase 1.
- **Automatically buffer by estimated result rows:** misses literal bytes, skew and
  blocking operators. Selection waits for measured plan/cost evidence.

## 12. Design sources

This is internal engineering derived from Sparkles' current executor, snapshot scans,
context/vocabulary, result writers, HTTP stream lifecycle and embedding APIs. The
semantic and resource contracts come from existing specs:
[C01](C01-observability-and-budgets.md), [C12](C12-graph-access-control.md),
[C12b](C12b-triple-access-control.md), [F06](F06-snapshots-and-point-in-time.md),
[F11](F11-encryption-at-rest.md), [G06](G06-arq-query-extensions.md),
[P03](P03-query-extensions.md) and [P06](P06-library-admin-api.md).
Phases 1 and 2 add no dependency. [PROVENANCE](PROVENANCE.md) records this source set.

## Outcome

The implementation exposes explicit snapshot-owning SELECT, graph and ASK execution.
SELECT batches retain charged IDs and vocabulary ownership. Graph batches retain charged
RDF terms. Both remain usable after the dataset closes. Caller-created term copies are
application memory. Parse, plan, pull, metadata and internal destruction protect deep
query structures. Cumulative row/work controls, cancellation, deadlines and fused errors
cover production and consumer waits; memory accounting uses conservative reservations.

Plain immutable SELECT scans return read-only views of decoded index block columns.
Returned batches share the block's retained memory reservation; loading another block
requires another reservation while consumers retain previous batches. Other scans copy
passing columns and preserve full resume keys.

Merge joins resume both inputs across batches. Hash, cross, OPTIONAL, semi, anti and
MINUS joins first collect their build input into charged state. They then resume the
probe input and the expansion of matching rows across batches, so neither the join's
output nor its matching row pairs are materialized. All of these joins preserve
duplicates and unbound compatibility. DISTINCT owns a charged key set. Eligible
plain-variable groups retain aggregate state rather than input rows. Blocking sorts
consume normal input batches even when the requested output prefix is small. They
reserve their input, keys and reordering state, and they report their full-input
barrier separately from eager fallback, so the strict policy admits them.
Growing state fails explicitly when its reservation exceeds the budget; disk spill is
not implemented. Unsupported operators and EXISTS expressions remain visible lazy eager
barriers, rejected before demand under strict policy.

Batch writers decode distinct base-vocabulary terms under retained and reconstruction
charges, declining the optional cache when memory is tight. Pure BIND ID reuse and fixed
numeric ORDER keys avoid repeated expression work without keeping uncharged value caches.
Variable ORDER columns use charged sorted vocabulary decoding. EXISTS plans, key sets and
partial projections retain their charges, with row evaluation as an optional-cache fallback.
Query-owned REGEX/REPLACE programs and explicit bounded search caches replace eager
thread-local state, including capture-product estimates and checked replacement growth.
String/key filters borrow keys under charged scratch. Cursor evaluation avoids uncharged
result, decoded-value and geometry caches; geometry operation limits remain in force.

CONSTRUCT instantiates templates per solution, preserves fresh/shared blank nodes and
uses charged global quad deduplication. DESCRIBE exposes a budgeted full-input traversal
barrier. ASK stops after a qualifying solution. RDF, native graph JSON and Jena graph
writers consume graph batches; RDF/JSON reserves its subject/predicate object map before
insertion. Native graph JSON emits four-term quads with null default graph names.

HTTP explicit execution uses a single producer, bounded response queues, interruptible
capacity waits and controls through body completion. Native metadata distinguishes
Complete and Stopped and reports unknown totals for consumer prefixes. Rust dataset APIs,
Python SELECT/graph cursors, Node result/byte streams, JVM opt-in Jena WHERE execution and
CLI explicit execution are implemented. Foreign row/transport copies have their own
bounded batching contract. Python rejects snapshot capture inside an owned transaction.

The ordinary query path remains eager. Auto selects large plain immutable SELECT scans
and uncached eligible single-key OPTIONAL COUNT queries under the measured batch,
graph-predicate and memory conditions documented in the API. Aggregate admission preserves
enabled result caches. Over HTTP, auto falls back to eager execution when the negotiated
encoding is SPARQL Results Thrift, which has no cursor writer.

The W3C SPARQL 1.0 and 1.1 query evaluation tests run through cursors at batch sizes
of 1, 2, 3 and 4,096 rows and match the expected results. The 53 queries that need
eager fallback are counted, and they run with fallback allowed. A seeded differential
compares cursor and eager answers at the same batch sizes over generated VALUES,
scans, property path sequences, joins, OPTIONAL, MINUS and UNION with unbound
columns. Correctness and resource gates passed across core, HTTP and
bindings. The Sparkles-only benchmark refresh covers 1.05M, 10.5M and full DBpedia,
plus specialized suites and matched JVM controls. [BENCHMARKS](../BENCHMARKS.md#streaming-execution)
reports representative current mode costs without tying them to an implementation session.

Matched production-allocator eager controls are near the previous implementation:
at 10.5M, grouped average is +0.9%, OPTIONAL count +1.3% and expression grouping
within 0.5%. The 1.05M range and star controls retain approximately 5.3% differences,
and a point lookup differs by about 1.9 microseconds. These do not establish universal
performance parity. Explicit streaming improves scans, OPTIONAL count and full sorting
in the recorded controls, while range TopK, expression sorting, substring filters and
TSV output remain slower. Their growing-state accounting and mode differences remain
opt-in. Narrow automatic admission does not select those losing shapes.

Disk spill, broader automatic selection and elimination of every complete-response
cost remain follow-ups. Full-scale cold reads and loading need separately matched
historical controls before attributing their dated snapshot differences to query execution.
