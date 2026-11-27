# X14: Unified executor

> **Status:** in progress.
>
> **Roadmap:** [item 453](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase. The [plan](PLAN.md) orders the remaining phases.

The design covers one morsel-driven pull executor in which eager execution is a consumption policy, with one memory-accounting model, one kernel per join and grouping algorithm, and one execution-policy function for every entry point.

## Problem and goal

Sparkles runs queries through two complete executors. The eager columnar executor
evaluates whole inputs with parallelism inside operators, and the pull-cursor executor of
[X05](../X05-streaming-query-execution.md) serves demand batch by batch with operator
state charged to the query budget. An admission layer of about 840 lines decides which
one runs, with a fallback state and an eager memory retry. Most of the `cursor_*` twins
exist because the two executors account memory differently, so every kernel change is
made and tested twice and the copies have already diverged on OPTIONAL counting. The
HTTP server, CLI, MCP server, GraphQL adapter and each binding also encode their own
execution policy.

The goal is one engine. Every operator serves demand over column batches, `collect()`
asks for unbounded demand with whole-input morsels and Rayon across them, and exchange
operators, partitioned hash builds and parallel merge joins supply pipeline parallelism.
Eager execution survives only as the way a consumer asks for its answer.

## Scope

The work runs in stages, and each stage removes a reason for later duplication
before the duplication itself goes. A stage lands only when the gates below pass.

1. Cheap deletions with no change in behaviour, items 474 to 478.
2. The first-release format cutoff, item 479, which [X16](../X16-storage-consolidation/README.md)
   designs. It shrinks the reader code that the storage stages must unify.
3. Policy and plumbing consolidation, items 480 to 483 and the short-term half of item
   484, so that every later engine change lands in one place per surface.
4. One accounting model, followed by the link to the reasoner and view budgets. This
   stage is the keystone of the merge, and it is measured on an Intel host.
5. Shared kernels under both executors, each landing with the eager-against-cursor
   differential tests as its oracle.
6. The morsel-parallel cursor and the executor merge. ASK and unordered LIMIT go through
   pull demand, the remaining fallback kinds get native leaf cursors, `collect()` becomes
   the only eager API, and the admission layer, the eager memory retry and the server's
   mode routing go. The in-place quick runner stays as a scheduling choice.

The [plan](PLAN.md) orders the work of stages 5 and 6 into phases, records what the audit
of the code against this design found, and lists the decisions that are open. Items 825
to 829 are its first phases.

Phases that touch a single area, such as the CLI's remote mode and the Python
transaction worker, are scheduled when other work changes that area.

The merge keeps the fast paths that are genuinely different algorithms. These are
index-order top-k with its runtime fallback, the bounded top-k heap, the numeric top-k
prefilter and the first-key screen, range pushdown and filter-on-runs, the count plans,
keyed joins and keyed OPTIONAL, merge OPTIONAL, ripple joins for ASK, index joins with
galloping and star fusion, batched joins, decorrelated EXISTS key sets, point and
predicate patterns, the seek-or-sweep frontier with its dense-block switch, spatial
pushdown and joins, vector top-k, the text subject pushdown, Grace hash-join spill, the
external sort and spilled DISTINCT and grouping, the in-place quick runner, self-service
SERVICE and per-distinct-value expression evaluation. `crate::spill` remains the one
spill format.

## Candidate phases

### Stage 1: cheap deletions

**Dead compaction arm and one generation switch.** Make `rebuild_locked` take a plain BulkCommit and delete the unreachable branches, split `build_generation` from a shared `switch_generation`, and replace the thread-per-call `sync_commit` with the LazySync worker. All three callers pass Some(bulk), and one CURRENT-switch bug needed two edits. Roadmap item 474 holds this phase.

**One join-order planner.** Keep `joinorder.rs` only, lift its 128-join-variable fallback with the existing Bits set or route it to its own greedy, and delete `Planner::dp`, `apply_dp_filters`, `Planner::greedy` and the `pruned_join_order` toggle. `Planner::dp` and `Planner::greedy` survive in `plan.rs` as a narrow fallback and toggle-off path. Item 84 should land on the unified planner. Roadmap item 475 holds this phase.

**Delete the off paths of replacement-style optimization toggles.** Remove the superseded implementations behind about 15 replacement toggles and the toggles themselves, and keep the algorithm toggles for diagnosis. Optimizations has 44 boolean fields, and every kernel change must keep each off path correct. Roadmap item 476 holds this phase. Item 512's test-only evaluator, which follows the SPARQL algebra literally, is the oracle of the differential tests, so the off paths that only tests reach can go.

**Shared format registry and small helpers.** Add one syntax registry for format names, extensions and media types, one INFERRED_GRAPH constant, one `parse_duration` and one durable atomic-write helper, and fix the Turtle string escaping that differs between `mapping/rdf.rs` and `mapping/draft.rs`. About ten format tables, nine INFERRED_GRAPH constants and ten atomic-write helpers exist. The unescaped draft writer could produce invalid Turtle. Roadmap item 477 holds this phase.

**Implementation-selecting switches made test-only.** Make the EnvSwitch statics, implementation-only StoreOptions booleans and measurement switches test-only or remove them after X07 closes. No measured difference has been recorded for I/O hints on bulk loads. Roadmap item 478 holds this phase. Item 513 covers the cold-read switches after item 4 measures them, with the I/O hint switch also depending on item 573.

### Stage 3: policy and plumbing

The short-term half of the shared commit publish tail belongs to this stage as item 484, and [X16](../X16-storage-consolidation/SPEC.md#candidate-phases) holds its design.

**One execution-policy function for every entry point.** Move the execution choice into one core function keyed by consumer properties (exact total, plan, inside transaction, format), so HTTP, CLI, MCP, GraphQL and the bindings state what they consume. HTTP, CLI, FFI, Node, Python, MCP and GraphQL each encode their own policy. Roadmap item 480 holds this phase.

**Typed reasoning mode and one inference view.** Add ReasoningMode { Off, Materialized, Demand(profile), Auto } and one InferenceView on Dataset that produces QueryOptions, the incremental inference input and the explanation source. Inferred-data selection is decided in about eight places with repeated string matching. It is the natural home of item 19's auto. Roadmap item 481 holds this phase.

**Library QueryRequest builder and server CallerContext.** Replace about 16 copies of the per-caller QueryOptions recipe and the binding copies with a library QueryRequest builder with Budgets and `with_reasoning`, plus a server `CallerContext::for_caller`. The GraphRAG builder builds options from the dataset defaults plus restrict and carries only the cancel flag, so server timeouts and limits plausibly do not apply to it. That would be a correctness gap. Roadmap item 482 holds this phase.

**Shared RRF fusion and text hits in the library.** Add a library `fuse_rrf(lists, k, weights)` and `TextIndexes::hits()`, used by `spk:hybridSearch`, MCP search and the memory layer. `text:query` SPARQL is generated three times and reciprocal rank fusion is implemented five times in the MCP memory layer. Roadmap item 483 holds this phase.

### Stage 4: one accounting model

**One memory-accounting model for all query state.** Charge every retained structure in both modes, make the value cache query-scoped and charged, and pick one regex engine per measured case. The eager fast account already reserves in chunks of 64 KiB, so the chunked reservations that remain are those of the ledger and its cursor accounts. `Ctx::is_cursor()` is tested at 43 non-test sites, and about 1,450 lines of `cursor_*` twins exist only because the regimes differ. Eager execution leaves its decoded value cache, its query-local terms and memos, the geometry memo of up to 64 MiB, the EXISTS memo of up to 100,000 entries and a process-wide thread-local cache of regular expressions uncharged, which is a budget hole. It is the keystone of the executor merge. Items 40 and 88 measure related memory and instruction costs, and neither blocks this stage.

The accounting steps that leave the operators item 7 changes alone come first. They rename the cursor accounting state to query state, count produced rows through one checked counter, charge one query-scoped value cache, reserve one decode envelope, charge terms, memos and geometries, move the scalar regular expressions onto query-owned engines and give the ledger's live accounts chunked reservations. Roadmap items 671, 672 and 673 hold them. The mode-neutral kernels of 674 follow item 7, since item 7 changes the eager EXISTS in BIND and OPTIONAL conditions and DESCRIBE that those kernels merge, and they end with the transition and `CursorMemory` gone.

The maintainer settled these questions for stage 4.

* Stage 4 does not wait for items 479 and 484, which are storage work under X16 and share no code with query accounting.
* Eager contexts keep their pool contribution at the admitted high point until the result is dropped, as a retention policy inside the ledger, until stage 6 leases every table.
* Eager budgets become stricter, so a query close to its memory limit can fail where it passed, because its value cache, terms, memos, geometries, regular expressions and scratch now count. Tests that pin an exact limit change only where newly charged state explains the difference.
* A query under 0.3 ms passes the Intel rule of the acceptance gates when the upper end of the 95% interval of its ratio is at most 1.02, or when its median added time is at most 0.5 µs.
* The charged value cache holds at most 64 MiB or an eighth of the query's memory budget, whichever is smaller.
* The regular-expression step closes item 7's remaining eager `regex-iri` filter and item 514.
* Item 534 keeps the response writer's share of the streamed small-query cost, and records the share that chunked live reservations remove.
* Item 293's admission rule for cursor BIND reuse stays out of the merged per-value path.
* A CI check bounds the mode checks that remain after the kernels merge.

**Hierarchical budget across reasoner, views and queries.** Link the reasoner Budget, the incr per-view memory and the query pool into one hierarchical budget. A reasoning run, a view and a query can each stay within their limits and overshoot together. A child reserves from its parent in chunks through the ledger's live reservations. It depends on item 192, and roadmap item 675 holds it.

### Stage 5: shared kernels

**One join-kernel library over column batches.** Keep merge, hash, keyed, index and ripple joins as distinct algorithms with one implementation each, where an eager join is one probe of the whole input and spill partitioning wraps the same kernel. About 1,000 eager and 4,000 cursor lines implement the same join families, and the copies have diverged on OPTIONAL counting. It depends on items 7 and 93.

**One grouping operator with an accumulator trait.** Use one hash and sorted-streaming grouping operator with spill over an accumulator trait (init, `add_batch`, merge, finish) shared by FOLD, registered aggregates and incremental views with a retraction method. Grouping exists four times (`exec.rs`, `cursor/group.rs`, `incr/ops/group.rs` and the count plans), and five times once item 7 runs eager grouping over finished key runs in cursors, with `registeredagg.rs` for registered aggregates. It also delivers item 418's retraction semantics. It depends on items 7, 93 and 418.

**One frontier-expansion kernel for paths and path search.** Share one expand(frontier, direction, lanes, graph filter, exclusions, dedup) kernel with the seek-or-sweep and dense-block policies across property paths, ARQ path ranges and path search. The sweep ratio is defined in both the SPARQL executor and path search, and only the executor applies the X07 cold-read rule. The fixed-point operator of item 417 would add a third copy. It depends on items 8, 51 and 417.

**One query-scoped expression memo.** Replace exprcache, cursor BIND reuse and the EXISTS memo with one charged memo keyed by expression and input value with one admission policy fed by observed repetition. Three memos each have their own admission and charging. It depends on item 293.

**One columnar writer per format over batch sources.** Write each result format once over a batch source, with an eager QueryResult as a one-batch source, vectorized term decoding, per-column writes into prebuilt buffers, and the Thrift writer moved into core. Two serializer stacks exist, Thrift forces eager execution, and large answers such as SP2Bench q4 at 1.96 GB need a raised limit. It depends on items 91, 243 and 534, and item 240 has shipped.

### Stage 6: the executor merge

**Single morsel-driven executor with eager as a consumption policy.** Replace the eager columnar executor and the pull-cursor executor with one morsel-driven pull engine in which every operator serves demand over column batches, `collect()` requests unbounded demand with whole-input morsels and Rayon across them, and exchange operators, partitioned hash builds and parallel merge joins give pipeline parallelism. The remaining fallback kinds get native leaf cursors, so the 840-line admission layer, `State::Fallback` and the eager memory retry go. Two complete executors exist (about 8,800 and 11,650 non-test lines) with 17 entry-point rules. Eager's remaining wins at 12 threads come from whole-table parallelism inside operators. It depends on items 7, 243, 293 and 534. Items 93, 240 and 512 have shipped. Once the eager executor is gone the differential tests have no second executor, so item 512's reference evaluator remains their oracle, with the answer fingerprints of the forge matrix and the W3C suites.

**Eager LIMIT and ASK through pull demand.** Route every ASK and unordered LIMIT through the cursor's pull demand, move UNION branch ordering for ASK into the planner's cost model, and delete `run_limited`'s geometric prefix replay and the unionpush first-work estimator. Eager LIMIT replays growing prefixes and unionpush keeps a duplicate cost model. `ask_streaming` exists, so this can land before the executor merge. It depends on item 7.

**Plans independent of execution mode.** Fix the plan before consumption is known and turn demand-dependent choices into runtime operator decisions, keeping the OrderedTopK and vector order fallbacks. The planner reads ctx.is_cursor() and `cursor.rs` rewrites plans, which makes EXPLAIN mode-dependent.

**Demand seeding inside the planner and one algebra visitor.** Make demand a planner-integrated operator that takes its seeds from the real plan, share one GraphPattern visitor and fold, and have explain call `prepare_query`. Demand has about 350 lines of its own join ordering and runs earlier patterns twice when seeding, and GraphPattern visitors are copied in six places. It depends on item 190.

### Phases scheduled with their area

**CLI remote mode on sparkles-client.** Move the CLI's remote commands onto `sparkles_client::blocking` and serve local mode through the in-process router. A 1,000-line reqwest client predates sparkles-client and was never replaced. It depends on item 47.

**Python transactions on TxnWorker.** Port the Python bindings' private transaction worker to `sparkles::embed::TxnWorker`. The module header states that Python keeps its own worker until it next changes. It depends on item 33.

**Serializable library wire structs.** Give NamedSnapshot, HistoryStatus and branch information Serialize wire structs shared with the generated client types. Hand-built JSON in four places already disagrees on keys (created against createdMs). It depends on item 60.

**Settings in one system.** Move the library's per-kind settings files, prefixes and compaction settings into the C19 layered registry, and give the remaining dataset configuration declarative layers there: the guard, text index, compaction, reasoning, quota, grants, protections and stored queries. Two settings systems plus two more paths exist, and C19 was built for more kinds.

### Execution additions

**Partitioned aggregation with a per-group overflow path.** Grace-partition grouping so that a single group can exceed the budget, as part of making spill the default. Spill is opt-in, a single group can outgrow the budget, and a spilled ordered join was 8.9 times slower. It depends on items 7 and 93.

**SIMD kernels and a vectorized expression evaluator.** Add explicit SIMD kernels for filters, hashing, joins and decoding, and evaluate expressions over column batches. Execution relies on autovectorization of u64 blocks only. Item 7 covers batching and spill, not kernels. It depends on item 7.

**Savepoints and statement-level atomicity in write transactions.** Roll back only a failed SPARQL update inside a longer write transaction instead of aborting the transaction. Embedded agents and Jena-style code retry one failed statement, and all three bindings document the abort.

**Parallel partial aggregation for registered aggregates.** Add an optional associative merge to registered accumulators so grouped queries can aggregate in parallel. P03 leaves a future merge capability that item 25 does not list. It depends on item 25.

## Dependencies

* The gates of items 469 to 473 exist before any stage that rewrites an executor path.
* Items 7 and 93 land before the executor merge, because they settle the spill default and
  the remaining eager fallbacks that the merge must absorb. Item 93 has shipped.
* Items 240, 243, 293 and 534 settle the cursor writers, row accounting, BIND reuse and
  the streamed response cost that stage 6 relies on. Item 240 has shipped.
* Item 512's test-only reference evaluator is the oracle of the differential tests, as it
  must be before the eager executor is deleted.
* The join-algorithm work of [X15](../X15-join-algorithms-and-estimation/README.md),
  [item 454](../../ROADMAP.md), follows the merge so that new algorithms land in one executor.

## Acceptance gates

* No query is 5% or more slower on forge at 12 threads, and no query that runs under
  0.3 ms is slower on an Intel host. This is the decision gate of the executor merge, and
  every stage that changes an executor path meets it. On the Intel host the base and the
  candidate run interleaved in pinned A,B,B,A rounds with the position bias removed, and
  a query under 0.3 ms passes when the upper end of the 95% interval of its ratio is at
  most 1.02 or its median added time is at most 0.5 µs, as the X08 latency gate decided.
  A strict reading of not slower cannot be resolved below about 4% for queries under
  15 µs.
* Stages 1 and 3 were screened on Namespace only. A forge matrix of the build after them
  settles them on forge and is the baseline of stage 4.
* The eager-against-cursor differential tests pass before either copy of a kernel is
  deleted, and the forge-pinned matrix of item 469 shows no regression for the 31
  synthetic queries at 1.05M and 10.5M triples with 12 threads and with one.
* The W3C suites pass with `execution=streaming` as well as with the default, which item
  470 makes part of the gate.
* `mise run bench:incr` guards changes to the interning and contexts that incremental
  views share, `mise run bench:reasoning-latency` guards changes to value decoding and
  the query path that query-time reasoning runs through, and `mise run bench:insn` guards
  changes to vocabulary readers and WAL replay.
* The guards are `scripts/bench.sh` and the Boat query screen (`mise run bench:remote --executor boat:bx_ID`) for queries, with forge runs
  of BSBM Explore and BI, SP²Bench `q4`, `q6` and `q12b`, WatDiv, LUBM and DBpedia at 50M,
  the memory benchmarks in `docs/benchmarks/memory`, `query_cursor.rs`,
  `plan_fidelity_tests.rs` and the W3C suites. Bindings use `bench:bindings`.
* Stage 4 is measured on an Intel host, since its effects on short queries are small.
* Query budgets, cancellation on disconnect and the results of every query stay as they
  are. A measured slowdown that a gate accepts gets its own roadmap item.
* From stage 5 to the merge, a phase may land with a real cost on a query under 0.3 ms,
  recorded as a line of the stage's small-costs item with the host, the commits compared
  and its class. The costs recorded against one query add up to at most 2% or 0.5 µs
  across the whole stage, and a query at that limit takes no further cost until one of
  its lines is recovered. A larger cost keeps its own roadmap item.
* Each recorded cost is classed by a profile that names the functions it lives in. A
  coexistence cost lives only in code the merge deletes, such as the eager executor's
  path or a check of the mode. A kept-path cost lives in code that survives the merge,
  and a cost without a profile counts as one. A kept-path cost names the item that
  recovers it and is recovered within two batches of the one that accepted it. A cost
  still unrecovered after those two batches stops any further phase that changes that
  query's path from landing until it is recovered.
* The forge matrix after each push that changes the query path also compares every
  query with cc94ac7f, the main branch before item 825 landed. A query whose drift from
  cc94ac7f is larger than its recorded costs is bisected and fixed forward at once. The
  merge meets the first gate above against cc94ac7f.

## Non-goals

* Removing a fast path listed under Scope because the merged executor could express it.
* Changes to SPARQL semantics or result formats.
* New query features. The executor merge changes how queries run, not what they answer.

## Design constraints

Streaming, non-materialized execution is the default, and eager whole-input work
remains only where a measurement shows it is faster. Higher memory use is acceptable when
it buys speed, and each stage states such a tradeoff in its Outcome.

## Provenance

This design derives from Sparkles' existing specs and code. Its phases also draw on the published papers, standards and public documentation of these sources: DuckDB external aggregation (Kuiper et al. 2024), DuckDB, Velox, DuckDB buffer manager, Leis et al. SIGMOD 2014 (HyPer morsels), Umbra, DuckDB aggregation, QLever result export and Sideways information passing. No code from another project is copied.

## Outcome

Stages 1 and 3 have shipped, apart from item 484 of stage 3, which X16 holds with item
479 of stage 2. Stage 4's query-memory accounting, mode-neutral kernels and decoding twins
have shipped. Item 675 remains open for reasoning and incremental-view budgets. Stage 5's
join-kernel library has shipped, and its other phases follow the
[plan](PLAN.md#order-of-work). [Item 453](../../ROADMAP.md) tracks the rest of the work.

Item 513 removed the sparse vocabulary and read-window environment switches and moved
the query prefetch and decoding switches to test controls. The read-ahead target is
4 MiB. A budgeted historical reader still looks up terms through the offsets when
its budget cannot hold the sparse vocabulary index. `SPARKLES_IO_HINTS` remains in use
for the dense export comparison in item 573.

Items 678 and 1069 have shipped. The string floors of `string_order_mask` read their keys
and search for a string's first extension inside the decoding envelope, and a refused
read fails the comparison with its refusal. The row-aligned pre-decode of `decode_for`
decodes its columns through the same sorted pre-decode as the dictionary values of top k
and the numeric FILTER, which admits each value's storage before it builds it, and moves
or clones the distinct values to their rows. Indexing the rows through the distinct
values' slots was considered and not built, because the row evaluator reads the decoded
column on its hot path and that change would need its own Intel measurement. Forge
measured item 678 against the build before it on 36 queries at 1.05M triples with one
thread, in three A,B,B,A rounds of ten requests, with the same answers and geometric means
of 1.002 in streaming and 0.993 in automatic execution.

The query's context keeps the floors in a cache that lasts the query, so cursor batches
compare two string columns through the floors as eager tables do. A floor cache that
each batch built again had measured 24.8% slower on streaming `str-pair-lt` and 33.4% on
`str-pair-names` on atlas, since names that repeat across batches had their floors
computed in every batch. Every worker of a mask reads and writes one direct-mapped table
of atomic words without a lock, since maps of their own made each worker compute a
recurring string's floor again. A word holds the id's slot tag, the distance down to its
floor and a bit set when the floor is read again. The first table has up to 2^16 slots,
fewer when the vocabulary's strings or the cap need fewer. Before a mask or a part of a
large eager table it grows up to eightfold, within the same limits, once its estimate of
the floors it evicted that had been read again reaches a thirty-second of its slots, so
inputs whose strings rarely repeat keep it. Each table is charged to the query before it is allocated, inside
the spill threshold, and the cache holds at most 64 MiB or a sixteenth of the query's
memory budget. This memory buys the speed below, and a table that the budget refuses
leaves the floors the cache lacks to be computed each time. A cursor batch spreads its
floor comparison over the pool as the generic filter does. The bounded-reads test
counts every read of the mode, seven in all.

Forge measured 3a8b7adb, the build with the shared table, against c0948014, the build
before item 1069, with 1.05M triples in five A,B,B,A rounds of ten requests per cell, and
every answer was the same. With one thread in streaming execution, `str-pair-names` took
0.517 of the base's time, 29.8 against 57.7 ms, `str-pair-lt` 0.815 and
`str-pair-colleague` 1.042. Against the direct comparison of the two keys in eager
execution, a probe build on the same base that the run record names, they read 0.512,
0.778 and 0.991. With 12 threads, streaming `str-pair-lt` read 0.935, `str-pair-names`
0.777 and `str-pair-colleague` 1.065, and eager execution read 0.843, 0.636 and 0.802.
Every round of these cells agreed in direction with its median.

An earlier build of the item, 174de1f8, gave each worker a map of its own, so a repeated
name's floor was computed once for each worker that met it, and with 12 threads its
streaming `str-pair-names` read 1.000 of the base and `str-pair-colleague` 1.118. Against
that build with one thread, the shared table reads 0.952 on `str-pair-colleague` and
0.907 on `str-pair-lt` in streaming execution and 0.931 and 0.908 in eager execution,
while `str-pair-names` takes 5.3% longer in streaming and 9.7% longer in eager execution.
The names query's table grew from its first 2^16 slots during the query. On wally, a
first table as large as the vocabulary's clamp closed that gap but made `str-pair-lt`,
whose table never grows, 1.5% slower in streaming and 2.2% in eager execution. Item 1140
now validates the initial sample of `string_order_mask` in left-to-right order before
sizing or installing the table, so ineligible base-vocabulary literals fall back without
reserving memory or locking cache initialization while cursor decode refusals keep their
order and already-decoded floors seed the cache. For the first eligible mask, it sizes
the initial table from a stack-allocated stratified sample of the two columns using
widened arithmetic, clamped to the vocabulary's strings, the cache cap and the remaining
memory budget and stepping down by halves to 2^10 slots before refusing, so
small-distinct and one-unique-column inputs stay compact while large repeating inputs
pre-size without growth. When a table does grow, live non-zero entries are copied
directly into the replacement table before publication and `FloorMap` drops its secondary
previous-table lookup and caches `first_triple`, so lookups probe only the current table.

On `atlas` at 1.05M triples in twelve alternating ABBA and BAAB rounds of 16 measured
requests per query and server process, with four processes per round across 12 rounds,
`329f82f9` against its own base `a643de75` returned byte-identical sorted TSV answers
across every cell and same-binary control, and `f0240009` only extracted an inline
stratum-bounds helper and tightened a unit test. Both one-thread A/B cells stayed within
the 3% spread limit, with A/B spreads of 1.79% in streaming execution and 1.77% in eager
execution. The separate same-binary A/A controls ran six alternating ABBA and BAAB rounds
of 12 measured requests per query and server process with four processes per round. The
`eager-1t` A/A control passed at 1.45% spread, whereas the `streaming-1t` A/A control
failed the 3% limit at 4.55%. Against `a643de75`, `str-pair-names` improved by 3.0% in
streaming execution, at 0.970 times the base's execution time, 27.40 against 28.23 ms,
with a 95% interval of 0.966 to 0.974 and 0.975 times the instructions per request. In
eager execution it improved by 10.0%, at 0.900 times, 24.95 against 27.68 ms, with a pooled
ratio of 0.901, a 95% interval of 0.896 to 0.903 and 0.918 times the instructions per
request, and all twelve rounds were faster in both modes. This Atlas comparison measures
the gain of `329f82f9` against `a643de75` alone. The earlier Forge gap of 5.3% in
streaming and 9.7% in eager execution (`174de1f8..3a8b7adb`) stays historical context on a
different host and revision pair, and claiming recovery against the old per-worker maps
would need a direct comparison on one host. Eager `str-pair-lt` stayed flat at 1.000
times, 9.33 against 9.34 ms with a 95% interval of 0.997 to 1.002 and 0.994 times the
instructions, and streaming `str-filter-two` crossed 1.0 at 1.002 times with an interval
of 0.993 to 1.008. Both 12-thread cells failed the 3% spread diagnostic without a repeat
run, with A/B spreads of 7.19% in streaming execution and 40.50% in eager execution
against 9.37% and 25.30% in their A/A controls. Their ratios are therefore observations
from cells that failed the spread diagnostic, including eager `str-pair-names` at 0.954
times with a 95% interval of 0.946 to 0.962 across twelve faster rounds. Six one-thread
observations kept their 95% interval above 1.0 with more than 0.5 µs of added time despite
unchanged or lower per-request instruction counts. Added times below use the formula
`pooled base median × (order-balanced ratio − 1)`, and all six also exceed 0.5 µs on the
median of the 12 paired round differences, where eager `str-filter-two` reads +15.6 µs
rather than +20.2 µs. Item 1089 tracks all six for profiling and recovery on Intel
hardware by the second batch after the one that lands item 1140, with the `streaming-1t`
A/A failure disclosed above and eager `str-pair-colleague` awaiting confirmation:

* On `atlas`, `a643de75` to `329f82f9`, streaming `str-pair-colleague` reads 1.0049 times
  with a 95% interval of 1.0006 to 1.0078, +49.1 µs across 9 of 12 slower rounds and
  0.9979 times the instructions.
* On `atlas`, `a643de75` to `329f82f9`, streaming `str-pair-lt` reads 1.0021 times with a
  95% interval of 1.0004 to 1.0029, +24.0 µs across 10 of 12 slower rounds and 0.9939
  times the instructions.
* On `atlas`, `a643de75` to `329f82f9`, streaming `lang-filter` reads 1.0047 times with a
  95% interval of 1.0038 to 1.0060, +6.1 µs across all 12 rounds and 1.0000 times the
  instructions.
* On `atlas`, `a643de75` to `329f82f9`, eager `str-pair-colleague` reads 1.0046 times with
  a 95% interval of 1.0014 to 1.0067, +36.6 µs across 9 of 12 slower rounds and 0.9978
  times the instructions. In the valid `eager-1t` A/A control it read 1.0028 times with an
  overlapping 95% interval of 1.0008 to 1.0039 and +22.6 µs, so this cost awaits
  confirmation.
* On `atlas`, `a643de75` to `329f82f9`, eager `lang-filter` reads 1.0061 times with a 95%
  interval of 1.0043 to 1.0073, +7.9 µs across all 12 rounds and 1.0000 times the
  instructions.
* On `atlas`, `a643de75` to `329f82f9`, eager `str-filter-two` reads 1.0008 times with a
  95% interval of 1.00003 to 1.0015, +20.2 µs by the formula (+15.6 µs paired round
  median) across 9 of 12 slower rounds and 1.0000 times the instructions.

`str-pair-colleague` holds 30,077 and 25,967 distinct names in its 30,077 rows, so most
floors are computed only once. A profile of the per-worker maps put most of their cost
in `Ctx::with_vocab_key_prev` and `Vocab::get_with_prev_checked`, which read the key and
its predecessor. Item 1088 uses the common prefix length stored by both vocabulary
codecs to detect a proper extension inside a block. A block boundary still needs the
preceding key. The checked floor walk admits its initial scratch before the walk and
reuses it across lookups. The cache can defer allocation when the first eligible mask's
estimate predicts few hits, or bypass a table after a sufficiently large sample of
lookups falls below one hit in eight. Every fourth mask probes so that late reuse can
re-enable floors, including when shuffled `str-pair-names` repeats few values in its
first batch.

The direct comparison uses the same single-key block walk for each operand, with a
reusable buffer for each key. A full FSST suffix needs no separate length scan when
the admitted buffer fits the maximum decoded length and the optional decoder headroom.
For other suffixes the growth check still uses the measured length. Scratch remains
inside the decoding envelope, and the cursor's checkpoints still report cancellation
and required decoding refusals. [Forge confirmation](#forge-confirmation-for-item-1088)
of the streaming cost is pending.

Queries that the floors do not reach also became slower in every round, and item 1089
holds these costs. The 37-query screens compared 174de1f8, before the shared table, with
c0948014, and these lines have not been measured on 3a8b7adb. Over three rounds the screens
had geometric means of 0.980 in streaming and 0.970 in automatic execution, and no query
was 10% slower. With one thread, 174de1f8 made streaming `str-filter-two` 2.8% slower over
five rounds, which 3a8b7adb reads at 1.000 with rounds on both sides, and with 12 threads
3a8b7adb makes streaming `str-filter-two` 1.4% slower in all five rounds. The screens
found streaming `distinct-join` 2.5% slower, `star-join` 1.7% in streaming and 2.0% in
automatic execution, automatic `employee-docs` 2.8% at 0.036 ms and automatic
`star-lookup` 2.5% at 0.062 ms. `range-topk`, `order-lcase`, automatic `two-hop-count`
and automatic `minus` read 0.8% to 1.3% slower, and streaming `lang-filter`, streaming
`optional-count` and automatic `order-by-full` 0.2% to 0.4%. With both builds made on
forge, item 678, 75a70587 against 7e272f4d, made `lang-filter` 2.9% slower in automatic
and 2.7% in streaming execution, `str-filter-two` 2.1% and 1.8%, and streaming
`distinct-join` 1.9%, in all five rounds, while `employee-docs` showed no cost. Profiles
of item 678's costs on the laptop found no function the item changed above 0.14% of the
samples and the same instruction counts for `lang-filter`. Automatic `str-filter-two`
changes mode between server processes, which item 1087 holds.

The test-only reference evaluator of item 512 has shipped in
`crates/sparkles-core/tests/support/reference`. It reads a list of quads and a parsed
query and evaluates every operator as a nested loop over solution mappings, as the
SPARQL 1.1 algebra of section 18 and the SPARQL 1.2 drafts define them, with no part of
the engine but the parser. Indexes on the predicate, subject and object narrow the
triples a pattern tries, a join tries only the solutions that agree on the variables
every solution of both sides binds, and a closure path with a fixed end is walked from
that end, none of which changes an answer. It answers all 508 query evaluation tests of SPARQL 1.0 and 1.1
and all 63 of SPARQL 1.2 under a comparison that holds literals to their exact form,
numbers to their value and ORDER BY results to their order. It reports the queries it
does not evaluate, such as SERVICE, DESCRIBE and extension functions, rather than
answer them approximately. It follows SPARQL 1.2 where the two versions differ, so the
effective boolean value of an ill-formed literal is an error. It reads XPath regular
expressions in their own syntax, checks where each quantifier stands and reports as
unsupported the constructs whose XPath meaning it does not establish, which are a
back-reference, a class with a `-` other than a simple range, the `i` flag with a category,
a negation or text that is not ASCII, and the `m` flag with an anchor on text that ends in
a newline, where XPath's anchors differ from the regex crate's. Where Jena's reading
differs from the specification's text and the engine follows Jena, the evaluator follows
Jena too, and a switch selects the text, which the W3C suites take. Under Jena's reading
the default graph of several FROM clauses is the union of their graphs, as in TDB, a
negated property set gives a pair once for each triple that joins it, and `\b` in REGEX,
which Java reads as a word boundary and XPath rejects, is unsupported. ORDER BY orders two
different terms with equal values, such as `1` and `1.0`, by the terms themselves, where
the text ties them and orders them by the next condition, so the comparison leaves their
order open. Where the
specification allows several answers, it says which. A value that it leaves open, such as
a SAMPLE, a MIN or MAX among unordered values, a GROUP_CONCAT order, a RAND, a UUID, NOW
or a new blank node, makes its column open, and an open value that decides which rows
there are, in a FILTER, a join, a group key or DISTINCT, makes the rows open. For an
ordered or sliced answer it reports each row's sort key, including keys the query does
not project. The seeded queries of `tests/query_cursor.rs` compare the eager answer and
the cursor answers at batch sizes 1, 2, 3 and 4,096 with it over a compacted base, a
pending delta, several index blocks and a masked view, and the optimizer, index-join and
string-filter tests compare `Optimizations::ALL` with it as well as with
`Optimizations::NONE`. Both use the same comparison of terms, numeric values and ordering.
It reports when it can check only part of an answer.
An open column is compared only by its place in the rows, which makes the comparison
partial. The evaluator records how each binding was produced. Copied terms and constants
compare exactly. Arithmetic, numeric functions, numeric casts and COUNT, SUM and AVG
produce numbers that compare by datatype and value. Variables, IF, COALESCE, MIN, MAX
and SAMPLE retain the provenance of the binding they return.

STR, STRDT, STRLANG and STRLANGDIR can carry the spelling of a computed number. An exact
match compares fully. A different spelling of the same value in the source number's
datatype compares partially if the result's datatype, language tag and base direction
match. An xsd:string cast from a number has a prescribed spelling. A cast that copies a
string can carry its provenance. String transformations compare exactly. The comparison
uses the reference's spelling for expressions that select rows or aggregate values.

An ordered or sliced answer must follow an order that ORDER BY allows. The comparison
uses the engine's spelling for a key it can recover from the returned bindings.
A key stops before the first open value or spelling it cannot recover. This includes
a hidden aggregate key whose inputs, selection or distinct solution count depend on
spelling, or a variable whose BIND may be unbound under a different spelling. A
distinct solution count stays fixed when fixed bindings, fixed presence or distinct
fixed metadata already separate every spelling-dependent pair within the evaluator's
proof budget. That budget bounds both recursive bucket refinement and pairwise
distinctness checks across the evaluation, and the count keeps its spelling dependency
if the budget runs out. In MIN and MAX, a
guaranteed candidate in a strictly better RDF term class rules out lexical-only
candidates in worse classes. Metadata reads such as LANG and DATATYPE keep their
place when the remaining input can change only lexically, as with UCASE on a computed
string or a selection aggregate whose winning-class candidates share their RDF
metadata. Earlier conditions can still rule out an order or slice.

When several reference rows match a returned row, the comparison uses the observed
keys of those candidates to check for inversions. A slice must have room on either side
for omitted rows whose order is forced across the candidate keys, counting each
returned row at most once across the groups it can match. The comparison reports the
order unchecked when it cannot establish the order from these constraints and leaves a
slice of it uncompared. It also leaves an answer with open rows uncompared.
A slice that passes the checks against each other row is compared when the reference's
other rows, placed around it in their own order, complete it to an order that ORDER BY
allows. Otherwise the slice is left uncompared. DISTINCT retains the common sort key
prefix of a row's copies. The order check tests the strict order between every two
blocks of equal keys it meets and gives up past 50 million key comparisons.
Whether an open value is bound is open too,
so BOUND, EXISTS, a join, DISTINCT, a LATERAL join and a GRAPH pattern whose name is a
variable that its inner pattern binds make the rows open when that variable holds open
values. Numbers sort by their exact values, which keeps the sort
transitive where promotion makes values of different datatypes equal. These rules decline to
judge some answers that the evaluator once compared. Of the 5,000 answers of the seeded
queries, 20 with open columns are compared in part, 15 others with open columns are
compared without their order because an ORDER BY condition reads an open value, no slice fails its completion, and 105
have open rows, 20 more than before because whether an open value is
bound is open. In the optimizer, index-join and string-filter tests, 3 answers with open columns
are compared in part, and 12 string filters whose regular expression takes the `i` flag
over text that is not ASCII are unsupported. No answer of either set has a column bound
by both copied terms and computed values that differs only in a number's lexical form. The parser writes the middle of a path sequence as a blank node, which
the algebra treats as a variable, so a `COUNT(DISTINCT *)` over such a middle is an open
value. A query whose nested loops pass the evaluator's work limit of 20 million
intermediate solutions is left uncompared, and so is one that reads a number of more than
16 digits. The comparison found disagreements, which items 1064, 1065, 1071 and 1077 held. The
engine passes over an error in MIN and MAX, where the specification's order makes MIN an
error and Jena makes both an error, and it follows Jena in making AVG over errors only an
error and in letting GROUP_CONCAT read IRIs and numbers. It did not read an
`xsd:integer` or `xsd:decimal` beyond the range of its decimal type as a number, which
item 1065 resolved by adding bounded arbitrary-precision `BigInt` and `BigDec` fallback
variants capped at 4,096 significant digits and 4,096 fractional digits while keeping
ordinary `i64` and `Decimal` values on their fixed-width fast paths, and bumping
`vocab.num` to magic `SPKVNUM4` so out-of-range numeric literals classify as `UNKNOWN`
and decode from the vocabulary. Its string functions once took a directional string and a
language-tagged string of the same language as compatible arguments, which SPARQL 1.2
and Jena do not. With item 1071, `STRSTARTS`, `STRENDS`, `CONTAINS`, `STRBEFORE`,
`STRAFTER` and vocabulary-key filters compare language tag and base direction.
Matching string-returning functions preserve the base direction. The evaluator dropped
its compatibility workaround for these functions. For the aggregates of item 1064,
the evaluator answers the W3C suites under the specification's reading. Engine
comparisons follow item 1064's setting, whose default is Jena's. Item 1077 has shipped and resolves the three XSD and XPath disagreements flagged
by the reference comparison. `xsd:float` and `xsd:double` parsing now accepts only XSD 1.1
lexical forms (`INF`, `+INF`, `-INF`, `NaN` and decimal or exponent numerals) across
expression evaluation, XML-whitespace-trimmed string casts, `numeric_key`, inline literal
encoding, `vocab.num` classification and `xsd::is_valid`. The numeric column format marker
is bumped to `SPKVNUM3` so pre-existing `SPKVNUM2` files fall back to key decoding until
recompaction or `add_numeric_column`. Casting `xsd:float` or `xsd:double` to `xsd:string` spells `NaN`, `INF`, `-INF`, `0`, and
`-0` directly. Finite nonzero values whose absolute magnitude lies in `[1e-6, 1e6)` format
as each type's shortest round-trip decimal, matching the reference evaluator while Jena
widens `xsd:float` to `double` through `BigDecimal.valueOf`. Finite nonzero values with
absolute magnitude below `1e-6` or at least `1e6` format in scientific notation with an
uppercase `E` and at least one fractional digit, such as `"1.0E6"` for `1e6`, `"-1.0E6"`
for `-1e6`, and `"1.0E-7"` for `1e-7`. XPath regular expression translation removes XML
whitespace outside character classes before interpreting escapes under the `x` flag while
preserving literal spaces inside classes, tracks inline Rust `(?s)`, `(?-s)`, and `(?x)`
flag scopes including whitespace and comments after `(`, preserves named capture headers
verbatim, rewrites unescaped `.` outside character classes to `[^\n\r]` when effective `s`
is disabled, and treats `REPLACE` replacement strings literally under `q` without capture
expansion across `REGEX`, `REPLACE`, key-filter pushdowns, mapping `REPLACE`, SHACL, and
ShEx. The
evaluator takes two well-formed values of different supported types, such as a string
and an integer, as unequal, which the note to section 17.4.1.7 allows and Jena does by
default, where Jena's strict mode and the section's main text make the comparison an
error.

The mode-independent plans of item 827 have shipped. `PatternScan` has a resumable cursor
over the predicate patterns, which the eager scan shares, so `spk:hasPredicate` plans its
scan in every mode. The planner deduplicates a DISTINCT below its ORDER BY whenever the
query may spill, and the eager executor numbers the rows of that DISTINCT as the cursor
does. The planner also sets the ripple join and the first-row index joins of an ASK or a
`LIMIT 1` over a keyed join, and streams a filtered product's larger input. A cursor
keyed join that held a small input and probed it measured slower than the ripple join,
which keys each row once and uses that key both to index and to probe the row. A cursor
FILTER without effects reads four times as many rows after each batch in which no row
passes, up to eight cursor batches, so a filtered product under an ASK reads its pairs in
batches as the eager executor's growing prefixes did. The result cache and sampled
filters stay on under cursor accounting. Every ASK and every LIMIT without ORDER BY pulls
its input through cursor operators with a demand of `offset + limit` rows, in eager
execution through the eager context and in automatic execution, which streams every ASK
without initial bindings. A LIMIT over one scan or a table of values, under projections,
reads its rows directly, and so does the per-row evaluation of an EXISTS pattern of that
shape, which item 829 leaves eager. A larger EXISTS pattern evaluated per row pulls its
first solution. A LIMIT over an ORDER BY runs its input as before, since the sort reads
it whole. A UNION under a LIMIT or an ASK orders its branches by their estimated cost to
the first rows, from each branch's cost and estimated rows, when no branch calls an
application function, a SERVICE or another operator with effects. `run_limited`,
`execute_limited`, the first-work estimator, `limited_input`, `filtered_ask_admitted`,
`order_ask` and `Dataset::ask_streaming_with` are gone, and the accounting mode is read
in four places. Pulling needs a shared context, so every caller that executes a plan shares
its context once planning is done (item 980). The WHERE clause of an update, the
queries that SHACL-SPARQL constraints, SPARQL functions and node expressions run, and
the context in which an incremental view evaluates a correlated EXISTS share theirs
through `Ctx::into_shared`, the public `exec::execute` shares a context it is
given, and a debug build fails a pull in a context that was never shared outside the
core crate's unit tests. The planner marks a LIMIT or an ASK to run its input whole, as a SELECT
does, when exact index counts bound every operator of the input, and the rows an index
join's probes read, to a cursor batch of 4,096 rows and its estimated cost past the first
rows is below the 4,096 units that building cursor operators costs
(`demand::runs_whole`). An input that evaluates a FILTER, a BIND, a keyed join's
condition, a range scan's test or another expression is pulled, since an expression may
form terms whose memory no row count bounds, and so is every input of a query with a
budget of produced rows, which the consumer's rows count against too. A whole run that a
row or memory budget refuses gives way to pulling the input (`exec::whole_or_refused`),
because the bounds do not cover the server pool's headroom or what the query already
holds. A LIMIT over a `spk:hasPredicate` scan reads the pattern reader directly, as one
over a scan does. A join is bounded by the product of its inputs or, where an input that
always binds a key meets a pattern with a constant predicate, by that input's rows times
the pattern's most rows for one key, which the predicate's statistics bound. The eager
LIMIT runs such an input whole, automatic execution keeps such an ASK eager, and the per-
row test of an EXISTS pattern applies the same choice. The choice depends on the plan and
the snapshot, never on the mode. `scan_limited` remains for the planner's two-row probe
and the direct reads. A pulled limit charges its operators' batches and its copy of the
plan to the query's ledger, which an eager query of a few rows did not take before, and
the cursor's scan charges one decoded block. A path walk grows its charge a reservation
step at a time and checks its output each time it doubles, and an eager query's empty
charges do not take the ledger to trim its grant. A pulled `LIMIT 500` over the paths of
300 people who know one other person each therefore takes 585 ledger operations rather
than 3,233 in a release measurement, and the unit test's single cycle of 300 people takes
fewer than 300 where it took 3,054. The decoded rows of a pulled result charge their
terms a reservation step at a time, so `QueryResult::rows` over 500 rows of two columns
takes fewer than 100 operations rather than about 4,000. A small pulled LIMIT over a join
still takes 50 to 100, one group per batch and operator, as a streaming query does. A
`PatternScan` of a constant subject hands out its predicates a batch at a time, and the
cursor charges the pattern reader's decoded block. A cursor cross product now holds its
smaller estimated input and streams the larger one, as an inner hash join does, so a
LIMIT over a cross product with a short VALUES list holds the list rather than the other
input's whole answer.

The phase departs from its plan in three places. The UNION branch order is a choice of
the planner's LIMIT and ASK arm in `demand.rs` on the branches' cost summaries, not a
term of the join-order program in `joinorder.rs`. That program orders the operands of a
join, while a UNION's branches are not joined and their order matters only to a consumer
that stops early, so the choice belongs where the planner knows the demand. The ASK
orientation, the ripple join and the first-row index joins apply to every slice of one
row at offset zero, which covers a `LIMIT 1` as well as an ASK, rather than to the shapes
the removed admission accepted. Both need one row, and a choice that depended on the
admission would have kept the plan dependent on the mode. `scan_limited` stays, for the
planner's two-row probe and for the direct reads of a LIMIT over one scan, because
building cursor operators and a ledger for a read of a few rows costs more than the read.

A LIMIT over an inner hash join now builds the hash index of one whole input, as the
cursor does, where the removed eager limited join indexed a prefix of the limited input
and ran again with eight times the demand until it had its rows. A `LIMIT 1` over the join
of two 65,537-row inputs peaks at about 7 MB where it fit in a 6 MiB pool before. A
first-rows join that reads both inputs a few rows at a time for small demand is
[item 947](../../ROADMAP.md).

A lane A/B on `atlas`, the benchmark host's hardware, timed a release loop of 15 ASK and
LIMIT queries and three filtered products over generated people datasets of 300 and
3,000, in eager and automatic execution, with builds before and after the item
alternating over three passes. It measured a small pulled ASK and a `LIMIT 1` or `LIMIT
10` over a join of 300-row patterns at 1.00 to 1.05 times the earlier build, a filtered
or path LIMIT at 0.14 to 0.87 times, and a filtered product under an ASK or a `LIMIT 1`
at 0.77 to 1.37 times. A pulled `LIMIT 1` or `LIMIT 10` over a join of 3,000-row patterns
takes 1.16 to 1.18 times as long and a `LIMIT 500` 1.28 times, a false keyed ASK takes
1.31 to 1.33 times over 300-row patterns and 1.12 to 1.13 times over 3,000-row patterns
in eager execution, against 0.67 in automatic execution, and a three-pattern ASK and a
`LIMIT` over `spk:hasPredicate` over 3,000 people take 1.05 to 1.07 times. These costs of
building and charging cursor operators are [item 952](../../ROADMAP.md). A FILTER that
reads ahead evaluates its condition on rows past its consumer's first solutions, so a
memory budget that a row past them exceeds, such as one whose value decodes a long
literal, refuses the query where reading at the consumer's demand would have answered.
The scan FILTER's morsels read ahead in the same way. The `grow_input` switch turns off
the generic FILTER's read- ahead and the growth of the scan FILTER's morsels, while a
demand of 4,096 rows or more still reads a first morsel.

Native SELECT output and standard solution responses of at most 65,536 rows admit and
decode a complete row before serializing its cells. Row storage and decoded terms keep
query credit until their payloads are freed. A required decode refusal returns its
original local or shared-pool memory error, preserves earlier complete-row counts and
stops before successful completion metadata. Native output holds a whole decoded row
rather than one cell at a time, and its additional live storage counts against the
budget. These remain estimated heap charges rather than process RSS limits.

The remaining stage 4 ownership seams are tracked by their own items. Retaining every
result table under its own lease belongs to the merge phase of item 453, linking the
reasoner's budgets and incremental views into the query pool belongs to item 675, binding
result buffers belong to item 1306, strict decoding across all callers belongs to item
1308 and the post-merge retained-source flip for MCP rendering belongs to item 1305.

MCP tool results, resource reads and completed task results keep their rendered payloads
and encoded wire frames under query memory credit until the transport writes or drops
them. `sparql_query` renders compact table and JSON pages iteratively under `NativeBudget`
with a single-row transient workspace rather than building uncredited full-result text
copies. On crates.io `rmcp`, qualified stdio and Streamable HTTP transports,
`Last-Event-ID` session replay buffers and background tasks carry staged responses and
encode each wire frame directly into `Bytes` backed by `OwnedBytes` credit, releasing that
credit on delivery, client disconnect, cancellation, TTL expiry or session close. Budget
refusals release retained credit before returning an MCP error frame. Rendering directly
from the post-merge leased result table follows the flip in the merge phase of item 453.

### Snapshot, mask and graph-access escapes

Item 1307 audited 46 snapshot, mask and graph-access surfaces across the core, facade,
server and binding crates, closed the actionable escapes behind owned handles or
non-owning caches and documented the intentional caller-owned views.

`PreparedView::snapshot`, `PreparedView::access`, `BoundEvidenceView::snapshot` and
`BoundEvidenceView::shared_snapshot` are now crate-private so external callers cannot
clone an uncredited masked snapshot or bound access policy past its preparation
reservation. `GuardDecision::exempt_controlled` now takes a scoped non-escaping
`GuardSnapshot` instead of `&Arc<Snapshot>`, so controlled providers cannot retain a
paid snapshot past preparation. External callers consume prepared and evidence views
through `BoundReadSource`, `BoundEvidenceView::prepare_view`, `Reads::from_prepared`,
`Reads::from_evidence` and non-escaping policy and metadata accessors on `PreparedView`.
`BoundEvidenceView::prepare_view` retains the evidence view, its reservation, its
Store-instance validation and a weak identity of the original policy `Arc` inside the
returned `PreparedView`. In the server, GraphRAG's `Evidence` replaces `snapshot`
with `read_source` and `commit` using the server query pool and configured execution
memory budget with a distinct `Preparation::with_selection_limit` ceiling for evidence
selection, and MCP memory recall runs evidence-backed reads and conflict checks
through `BoundReadSource` and `Schema::constraints_for_evidence` under the call's grant
or dataset pool with the same distinct selection ceiling instead of cloning the raw
snapshot or opening live `HEAD`.

`Snapshot::mask`, `Snapshot::counts` and `access::triples::apply` are now crate-private,
with read-only non-escaping accessors `has_mask`, `mask_id`, `mask_key` and
`hidden_quad_count` on `Snapshot`. `CountCache` stores non-owning weak references in
separate legacy `masks` and controlled `controlled_masks` tables alongside weak `views`,
with best-effort drop-time slot eviction on `Mask` and dead-entry pruning on lookup and
insertion. Legacy callers cannot upgrade a mask paid for by a controlled preparation, and
masked delta nodes and visible-graph lists do not survive past their last live owner.
`GraphAccess::bound` stamps each decided guard with the view's dataset, generation,
commit, mask and uncommitted view identity; checked read paths and facade evidence
readers validate both top-level and nested rules, accepting the matching view and
refusing another commit, store, uncommitted view or masked derivative. `WriteTxn::graphs`
is removed and `WriteTxn::prepare_read_view` reserves the transaction snapshot storage up
front and returns a `PreparedTxnView` borrowed from the writer so neither the view nor
its `ScopedQueryResult` can outlive `WriteTxn::discard_terms` or a dry-run rollback;
`PreparedTxnView::query` also refuses owning snapshot callbacks (`QueryOptions::reasoning`
and `QueryOptions::views`) so a provider cannot retain the transaction snapshot past drop.
`QueryBatch::transient_term`, used only by the FFI cursor encoder, remains deferred to
coordinate with items 1306 and 1308.

The remaining audited getters are intentional caller-owned unbudgeted views or unbound
policy builders documented as limitations in the server and storage guides. These cover
`Store::snapshot`, `Dataset::snapshot`, `QuadIter::snapshot`, `Embedder::snapshot`,
`WriteTxn::view`, `WriteTxn::base`, `WriteTxn::read_view`, `Snapshot::without_cache_fill`,
`Snapshot::with_overlay`, `Snapshot::generation`, `Snapshot::delta`,
`sparql::with_union_default`, `Opened::snapshot`, `ReportOutcome::snapshot`,
`Store::managed_snapshot`, `Access::view`, `Grants::graphs`, `guard::apply`, incremental
view snapshot accessors covered by item 675 and internal context structs that carry a
caller's snapshot.

### Dead compaction arm and one generation switch

Item 474 shipped. `rebuild_locked` takes a plain `BulkCommit` and its write options,
since every caller passes a bulk commit, and the branches that served a compaction
through it are gone. The build of a bulk commit's generation is its own function,
`build_generation`. A bulk commit and a compaction switch `CURRENT` through one
function, `switch_generation`, which poisons the writer when the switch may have
happened, and move the writer onto the new generation's log through one function,
`adopt_generation_log`. The compaction's last catch-up sync runs the delta-vocabulary
sync on the store writer's lazily started worker instead of a thread spawned per call,
and the thread-per-call `sync_commit` is deleted. A bulk commit now reads the size of
its new log before the switch rather than after it, so a failed read can no longer
leave the writer on the old log after `CURRENT` moved. Behaviour is otherwise unchanged,
and the existing compaction, bulk-load, relink and crash-boundary tests cover the paths.

### One join-order planner

Item 475 shipped. The planner orders every connected group with the program on cost
summaries in `joinorder.rs`, and the `pruned_join_order` toggle is gone with the
exhaustive program over plan trees and the planner's own greedy ordering that it chose
between. A group that the program cannot track, with more than 16 inputs or more than
128 variables shared between inputs, keeps the greedy plan of `joinorder.rs`, which
tracks variables in sets of any size. Before, a group of up to 12 inputs that shared
more than 128 variables fell back to the exhaustive program. The filter placement that
both programs shared moved into `joinorder.rs`. The deleted ordering survives only as
the oracle of `joinorder_tests.rs`, behind a test-only switch, so the tests still
compare the program's costs and answers with the exhaustive program's. With every
optimization off, a group is now planned on cost summaries as well, where it used to
take the exhaustive program. Default plans change only for groups that share more than
128 variables, since every other group already took the program on cost summaries or
its greedy plan.

### Superseded replacement toggles

Item 476 shipped for the switches whose off path served no purpose but the old
implementation. Five switches are gone with their off paths. Hash joins always build the
flat table, so the per-key lists and their cost constant are deleted (`flat_hash_join`).
Scans always decode only the key columns they read (`selective_columns`). Index
statistics are always corrected for the delta and for unread graphs (`delta_statistics`).
A filter with a fixed string start always reads only its key ranges and fails the base
ids outside its id ranges (`filter_key_ranges` and `filter_id_ranges`). Every result was
already the same with each switch on, which the tests checked, and the tests now compare
these paths with a nested-loop join, with the statistics switched off and with the
generic operators.

The other switches stay. The algorithm switches that the merge keeps choose between
different algorithms for diagnosis. `expr_cache`, `sampled_filters`,
`decorrelate_exists`, `count_filter_runs`, `filter_scan_runs` and `topk_first_key` are
switched off at run time for queries that call extensions, so their off paths are live.
The cost-model switches `sampled_filters`, `characteristic_sets`, `probed_keys` and
`fused_star_costs` are what the cost calibration harness compares. `grow_input` remains a diagnostic switch. Cursor tests still compare decoding ahead
with sequential decoding, which also serves a single Rayon worker. The switches
that shipped with their measurements in the current release, `probed_optional` and
`bound_order_keys`, keep their off paths as the baseline of the forge checks.
`gallop_index_join` chooses between the galloping reader and the clustered reader, which
are different read paths.

Item 512's test-only reference evaluator is the oracle of the differential tests. The
optimizer tests still compare `Optimizations::ALL` with `Optimizations::NONE` and both
with the reference where it evaluates the query, so the remaining off paths that only
tests reach can go once their tests rest on the reference alone.

### Shared format registry and small helpers

Item 477 shipped. `sparkles_core::syntaxes` is the registry of the names, file
extensions and media types of the W3C RDF syntaxes. File names, media types, result
format names, Jena-style syntax names, SHACL shapes syntax names, the `convert` tool,
the ShEx schema hint and Fuseki configuration files look a syntax up there, so an alias
that one of them accepted is accepted by all of them. The callers keep their
exclusions. Results are not written in N3, and the generic JSON and XML media types
still name result formats. A shapes file is not read as N3, and a `.json` ShEx schema
is ShExJ. Jena reads the N3 media types as Turtle, and so do media-type lookups.
`sparkles_core::INFERRED_GRAPH` is the one inferred-graph constant, which the other
crates re-export, and `sparkles_core::duration::parse_duration` is the one parser of
written durations, behind the backup policy, the server's retention and age flags,
the auth configuration and the transcript import. `sparkles_core::fsutil::write_atomic`
and `write_atomic_via` are the durable atomic write, which the store, the change-log
index, the catalog and the server's state files use. The change-log index now also
syncs its directory after the rename. The R2RML draft writer escapes its strings with
the main mapping writer's Turtle escaping, so a table or column name with a line break
or a quote no longer produces invalid Turtle.

The helpers whose differences are deliberate keep their own code. The reasoner's
unsynced write, the backup cache and the server's writers that set file permissions or
keep a file's mode are not durable replacements in this sense. The day-based `assist`
ages accept fractions and years, the rate limiter parses rates, and Fuseki's timeouts
are milliseconds. The client and the formatter do not depend on `sparkles-core` and keep
their extension tables.

### Test-only implementation switches

Item 478 shipped for the switches that X07 does not measure. `SPARKLES_NUMERIC_COLUMN`
and `SPARKLES_WRITEBACK` are deleted with their switches. Vocabularies always read their
numeric column when they have one, and a vocabulary without the file, which the numeric
column tests open, takes the path the switch used to select. Index builds always send
their files to the device as they write them. The hidden server flags `--no-wal-direct-writes` and `--no-group-commit-idle-direct`
are gone, and the store options they set, `wal_direct_writes` and
`group_commit_idle_direct`, are hidden fields that the tests of the buffered log path
and of the group set. The buffered path still serves a file system that refuses direct
I/O. The undocumented `SPARKLES_DEBUG_CACHE` printout of result-cache keys is deleted.

Item 513 removed `SPARKLES_SPARSE_VOCAB` and `SPARKLES_READ_WINDOW_KB`. Vocabulary
opens use the sparse index when the reader can hold it, and permutation scans and
vocabulary passes use a fixed 4 MiB read-ahead target. The `prefetch_blocks`,
`prefetch_constants`, `prefetch_terms` and `prefetch_first_scan` fields of
`Optimizations` compile only for unit tests. The `parallel_decode` and `decode_ahead`
fields are hidden controls used by integration tests. The six switches are absent
from `SPARKLES_DISABLE_OPTIMIZATIONS` and the named diagnostics. Item 513 still tracks
`SPARKLES_IO_HINTS` after item 573.
`SPARKLES_NO_IN_PLACE` remains as the method of the published memory benchmark, which
compares the in-place runner with the blocking pool.

### One execution policy

Item 480 shipped. `sparkles::sparql::choose_execution` decides how a query runs from a
`Consumer`, the description of what the consumer of the answer needs. A consumer names
the mode it asked for and the mode it runs in without one, and states whether it needs the
exact total before its first row, the eager plan explanation or the whole answer at once,
whether it runs inside a write transaction, whether its format has a cursor writer and
whether the query has initial bindings. A requested mode wins over the default. Inside a
write transaction a requested cursor or automatic execution is refused, and a query that
asked for nothing runs eagerly. Automatic execution runs eagerly for a format without a
cursor writer and for a query with initial bindings. `ExecutionMode::parse` reads the
names `eager`, `streaming` and `auto`.

The HTTP query endpoint, `sparkles query`, the Node addon and the FFI's `execute_with`
state their needs and take the mode the policy returns. SPARQL Results Thrift is a format
without a cursor writer, the CLI's text table needs the exact total, `--explain` needs the
plan, a Node or FFI transaction is a write transaction, and the FFI's `auto` argument is a
default rather than a request. The MCP tools, GraphQL and the Python `query` methods read
whole answers through the eager query APIs, which is the policy's answer for a whole-result
consumer. The HTTP server's in-place quick runner remains a scheduling choice after the
policy, as stage 6 keeps it. Behaviour is unchanged, and the unit tests of the policy
compare it with each surface's former rules for every combination of their inputs. The
server's tests of Thrift, stored queries with values and an eager server default, the
CLI's execution tests and the FFI's automatic execution tests cover the surfaces.

### Typed reasoning mode and one inference view

Item 481 shipped. `sparkles::reasoning::ReasoningMode` is `Off`, `Materialized`,
`Demand` with an optional profile and limit of derived facts, or `Auto`.
`ReasoningMode::parse` reads `false`, `true`, `materialized`, `demand` and `auto`, and
`from_param` reads a request parameter, where an absent or unknown value keeps the
materialized inferences as the endpoints always have. `Dataset::inference_view` turns a
mode into an `InferenceView`. Its query options read the selected data, and its `apply`
changes a caller's options as the mode changes the dataset's defaults. Options that do not
hold the dataset's defaults get them first, and a view replaces a query-time reasoner that
the options had unless its mode is demand. Its `inference` is
the input of incremental views and live queries, and `explain_source` names the source of
an explanation. Demand builds the query-time reasoner and fails as `query_reasoner`
does, and `Auto` is refused. `stored_inference_view` gives the infallible views of the
materialized inferences or of the asserted data alone.

The HTTP query options, `reasoning=demand` and `reasoning=auto` on queries and
explanations, the reasoning diagnostics, SHACL and ShEx validation over HTTP, the CLI's
`--reasoning` with its profile, rules and limit, and the MCP tools' `reasoning` argument
now read a mode and apply its view. `Dataset::query_options` and the views runtime read the
materialized inferences through one function, where each repeated the rule before. The
schema report and the GraphQL draft keep their own true-or-false parameters, which reject
other values or default to off, and the explanation request keeps its `source`, which
names only `materialized` or `demand`. Behaviour is unchanged. The new CLI test covers
`--reasoning false`, `true`, `materialized` and `demand` with and without a profile and
with an unlimited limit, and the library tests cover each view.

### Query request builder and caller context

Item 482 shipped. `sparkles::QueryRequest` builds the options of one query from a
dataset's defaults. `budgets` sets a `sparkles::Budgets` of timeout, rows, memory, rows
produced and spill bytes, `lower_budgets` only lowers them as a request lowers a server's,
`with_reasoning` reads the data of an `InferenceView`, and `cancel` sets the cancellation
flag. The server's `CallerContext::for_caller` holds the state, the dataset and the caller.
Its `options` gives the request options of the dataset's defaults, the server's budgets and
settings and the request's parameters, restricted to the caller's grants on an endpoint.
`task_options` gives a background task the same budgets with the update timeout and no
pool grant, and `read_options` gives a route without parameters of its own the query
timeout and the budgets.

The query, text search and explanation endpoints, the graph and facet routes, the SPARQL
diff, the forms model and check, the view reads, GraphQL, ShEx selectors, in-process
SERVICE calls and algorithm write-back now take their options from the caller context
instead of repeating the recipe. The request budgets lower the options through `Budgets`,
the MCP tools build theirs with `QueryRequest` and their own memory budget, and the Node,
Python and FFI bindings apply their callers' limits as `Budgets`. The update endpoint keeps
its own options, which have no spill budget and no dataset defaults besides the pool.

The GraphRAG build and its read routes used to run with the dataset's defaults and the
caller's grants alone, so no server timeout or budget applied to them. A build now runs
under the server's row, memory, rows-produced and spill budgets and its update timeout, as
an algorithm write-back does, and the read routes run under the query timeout and the same
budgets. This is the one intended change in behaviour. A unit test of the caller context
checks the budgets, settings, parameters and inferred data of a request, a task and a
read, and the library tests check the builder's budgets, reasoning and cancellation. The
server's budget, limit and quota tests cover the endpoints.

### Shared rank fusion and text hits

Item 483 shipped. `sparkles::rank` holds the reciprocal rank fusion. `fuse_rrf` sums
`weight / (k + rank)` over weighted ranked lists in list order and returns each key once,
best first, with ties broken by key. `rrf_score` gives one key's score from its ranks, and
`competition_ranks` ranks keys by their best score, so that tied keys share a rank.
`spk:hybridSearch` ranks and fuses its two lists with them, and the MCP memory layer's
`recall`, `community_reports`, `link_entities` and `similar_queries` fuse with them where
each had its own loop. `link_entities` keeps its tiers before the score, and
`similar_queries` keeps its ties broken by query name. The server's two rankers of memory
hits keep their own handling of a NaN score, which ranks first there.

`sparkles::handles::text_call` writes the one `text:query` call, and `TextIndex::hits`
reads its hits from a given snapshot, as `search` reads them from the current state. The
MCP `search_text` tool reads its hits through `hits` with its caller's options and commit,
and the memory layer's text hits build their call with `text_call` inside their graph
pattern. A search without highlighting now returns a literal as stored, where the search
of the library and `GET /{ds}/text` used to remove the private-use characters that mark a
match from it even when no match was marked. Results and their order are otherwise
unchanged, and the hybrid search, MCP search and memory tests cover them, with unit tests
of the fusion's sums, weights and ties.

### Charged query state and chunked live reservations

The accounting steps of stage 4 that leave item 7's operators alone shipped, with the
regular-expression step of item 672, and so did the first step of item
674, which charges retained scratch and the EXISTS memo in both modes. Every context now
carries its accounting regime as `Accounting::Collect` or `Accounting::Cursor`, one
record of a memory failure and one checked counter of produced rows, so the state that
only cursors had is query state in both modes.

The decoded value cache is query-scoped and charged in both modes. It holds at most
64 MiB or an eighth of the query's memory budget, whichever is smaller, split evenly over
its 64 shards. Each entry counts twice its key and value slots and the heap bytes of its
value, a shard that reaches its share is cleared and gives its slots back, and a value
whose charge the budget or the pool refuses is returned without being cached. The cache
survives the transition from eager to cursor accounting, which used to clear it.

Dictionary decoding reserves one envelope in both modes, a key of the largest size seen
so far on every worker, charged once when it grows and released when the work ends.
`Ctx::term`, `Ctx::value` and the comparison of simple literals have one path each, and
templates decode through `batch_term`. A cursor's envelope is decode bytes on the
ledger, as before, and its refusal fails the query. An eager envelope is a charge of its
own that is taken when the budget has room and is never refused, so an eager key is
rebuilt on the vocabulary's unchecked path and the envelope grows to it afterwards.
Checking the envelope before each step of the rebuild made an eager `term` about a
quarter slower, and writing an answer of 500,000 rows term by term 1.25 times as slow. A finished eager result
is decoded through infallible readers after its last check, and an update applies its
template after its WHERE clause, so a refusal there would read as an unbound value. Plan
text, which no answer depends on, decodes through `Ctx::plan_term` without the envelope in
both modes, so describing a substituted constant never fails a query. An update checks its
context before it applies deletions and insertions.

The terms that expressions compute, the blank nodes and callback terms they memoize and
the parsed geometries of the geometry memo are charged in both modes through one step
reservation each. A reservation grows a step ahead of its bytes, a step of 64 KiB or a
64th of the budget when that is smaller and at least 256 bytes, and falls back to the
exact bytes for the terms, which a query cannot drop. A refusal of the terms records a
failure, so an expression never turns a budget refusal into an unbound BIND. The value
cache and the geometry memo are optional, so their refusal only skips the entry. A context
that copies another's local vocabulary charges its copy. The transition no
longer charges the local vocabulary again and keeps the geometry memo, which it used to
drop. `retained_state_bytes` is gone, since what it estimated is charged. When an eager
SELECT or ASK finishes, its primary account keeps its live bytes plus its columns.

A live account of the ledger now reserves from its grant in steps of 64 KiB ahead of its
live bytes, with an exact fallback when the step does not fit, and gives back everything
above the next step once its reservation is more than 1 MiB above its live bytes. Most
cursor charges therefore take the ledger's lock without a pool operation. Owners that
retire release their reservations, and the drop of the ledger releases what remains.

These steps make eager budgets stricter, as the maintainer decided. A query close to its
limit can fail where it passed, since its value cache, terms, memos, geometries and
decoding now count. Two tests changed their limits for that reason.
`a_batch_too_large_to_buffer_spills_as_its_own_run` gives its spill sort a quarter more
memory, since the value cache now takes part of the budget the run sized itself to, and
the CONSTRUCT budget test of `sparql/tests.rs` uses 8 KiB where it used 1 KiB, since its
template terms are charged. The tradeoffs are memory for speed. A cursor account can
hold up to about 1.06 MiB of the pool above its live bytes, the step plus the slack, and a
charged state reservation keeps up to one step above its bytes until the query ends.

`charged_state.rs` runs 50,000 rows of `STRUUID()` under a 3 MiB budget, which fails with
the memory budget in both modes where eager execution used to answer, and checks that a
finished result of a 4 KiB literal reads back under an 8 KiB budget and that an update
whose template is refused leaves the store as it was. Unit tests in `ctx.rs` cover the
stepped cache charges, a refused cache reservation, a refused decode envelope, a cleared
shard and the shrink of a finished result, `geo/memo.rs` covers a refused geometry charge,
and `query_memory.rs` covers the stepped live reservations and 17 concurrent cursors in a
256 MiB pool. `plain_queries_take_no_ledger_operations` checks that plain eager queries
still take no ledger operation.

Retained charges, which belong to state that outlives the call that builds it, such as
a cache of the plan, now work in both modes. Every context that query preparation shares
holds a weak reference to itself, attached once the automatic choice between eager and
cursor execution is made, and a retained charge releases through it. A retained charge
grows on its own owner, which an eager context's lock-free account takes without the
ledger. A context that is never shared, such as one a unit test builds alone, has no owner,
so an eager one keeps such state uncharged as before. With that owner the EXISTS memo is
one path, which holds at most 100,000 entries and an eighth of the budget and charges each
entry in both modes. The key sets of a decorrelated EXISTS take one charge shape, in which
the probe vectors are transient and a key set is a retained charge, or part of the
probe's charge in a context without an owner. The admission of the planned pattern is
charged in both modes. `map_rows` takes its collector charge and runs one partitioned job
in both modes, in pieces of at least the 1,024 rows between two polls of cancellation and
the deadline, and the key count over front-coded blocks sizes its pieces by the budget
and charges its scratch and decoding in both modes. A finished query drops its plan before
its pool contribution shrinks, so the plan's retained
charges leave the pool with them. `an_eager_exists_memo_is_charged` checks that 20,000
distinct EXISTS keys raise an eager query's peak by their entries, and
`a_finished_result_gives_back_its_exists_state` checks that a one-row COUNT over 100,000
EXISTS keys keeps at most 2 MiB of the pool.

Regular expressions have one engine in both modes. `REGEX`, `REPLACE` and the key filters
on vocabulary keys take each pattern's program once per query, a program of
`regex_automata` with a bounded hybrid DFA and PikeVM. The query is charged for the
program and the search caches it holds. Eager execution used a process-wide thread-local
cache of up to 1,000 uncharged programs of up to 4 MiB with their search caches, and it
is gone. Compiling `regex-iri`'s pattern took about 25 µs on the critical path of a 0.25
ms filter on atlas with 12 threads, and a query that compiled each pattern again was 8 to
10% slower on forge than the thread-local cache at 1.05M triples. Compiled programs are
therefore kept in one process-wide table of at most 64 programs and 16 MiB, and a query
whose budget allows the bound a kept program was built under shares it. On atlas, a lock
on one shared search cache cost about 9 ns a row, nearly as much as the 11 ns of matching
`regex-iri`'s pattern against an IRI, so each Rayon worker that searches a program gets a
clone of it with its own cache. The clone's cache is charged when the worker takes it and
is optional, so a worker whose cache would leave less than three quarters of the budget
shares the locked cache instead. A kept program with cold caches on every worker still
left `regex-iri` 4 to 6% slower at 1.05M triples with 12 threads on atlas, so when a
query's work ends each worker's clone of a kept program waits in the table, keyed by the
thread whose pool serves its cache, and the next query that searches the program on that
thread takes it back warm and charges it again. Parked clones count their charged cache
bounds against a second limit of 16 MiB. Finding a worker's clone reads the thread's
Rayon index, its number and the clone's slot, which took 13% of a filter over 314,000
IRIs on one thread when it ran for every key, so the key kernels bind a filter once per
piece of keys and find each clone at its first search there. The clone is found where an
unbound search finds it, after the storage of the first key, so binding admits no cache
before the work the query needs. The table thus holds up to 32 MiB outside every
query's budget, which is the tradeoff that keeps a repeated pattern at the speed of the
old cache. A key filter copy no longer forks a cache of its own. The per-row lookup of a
pattern reads the query's map of programs without a lock, and the planner reads a
filter's key prefixes without compiling its patterns. SHACL and ShEx compile their
patterns once per shape through `compile_regex`, which no longer caches.
`a_regular_expression_beyond_the_budget_fails_the_query_in_both_modes` checks that a
program a 32 KiB budget cannot hold fails an eager query, which compiled it uncharged
before, `regular_expressions_on_rayon_workers_count_what_the_data_holds` checks key
filters, `REGEX` on computed strings and `REPLACE` against counts that follow from the
data on four workers in both modes, and
`a_charged_query_charges_the_parked_clones_it_takes` checks that a worker charges the
clone it takes back and that a worker without room leaves it parked.

Per-value reuse in BIND and FILTER has one implementation, which chooses its algorithm
by the size of its input rather than by the accounting mode. An input of at most 16,384
rows, which holds a cursor's batches, keeps only ids and slots under one reservation until
the output is expanded, finds its distinct inputs from a dense integer range or one sort
and expands on the calling thread. A larger input estimates its distinct inputs before it
collects them and expands in parallel, and its slots, distinct values and results are a
retained charge until they are read, in both modes. Eager tables of 1,024 to 16,384 rows
therefore take the batch algorithms, and a FILTER whose conjuncts read several variables
groups them by variable in a cursor batch too. When the key tests that a FILTER's shape
admits do not compile, the distinct values go to the general evaluator, where a cursor
batch used to panic. FILTER and BIND inputs of at most 16,384 rows time their first 256
rows before spreading the rest over the pool in both modes.
`the_table_and_batch_algorithms_agree_with_rows_on_both_sides_of_the_threshold` compares
reuse with row-by-row evaluation over 32,768 rows in eager execution and in cursors with
batches on both sides of the threshold.

ORDER BY and its top k choose by input size in the same way. From 1,024 rows, a numeric
key over one variable can be evaluated once per distinct input whatever the input's size.
Up to 16,384 rows its distinct inputs come from a dense integer range or one sort, and a
larger input only maps integers whose span is at most half its rows and at most 65,536
values to their slots directly, in parallel above 262,144 rows. From 1,024 rows, a
variable's dictionary and query-local values can be decoded once per distinct term in
key order, and the decoded column then keeps a charge for its slots, its distinct values
and their payload rather than the reservation that built it. From 4,096 distinct terms the
decoding runs in one parallel pass over pieces of the sorted ids, in which each piece reads
its numbers from the numeric column, admits each other value's payload before it builds the
value and writes the value in place. A piece's charge outlives what it wrote, and the
pass's charges join the column's charge when it ends. A column of more than 16,384 decoded
values is freed on the pool after the sort, before its charge is released. These paths are
optional, and a refused reservation leaves the key to the paths below. Otherwise, up to
16,384 rows, keys are evaluated on the calling thread with each value admitted before the
column keeps it. Both kinds of payload charge grow a step ahead, by the query's
reservation step of at most 64 KiB, and take the exact amount when the step is refused, so
a query at its budget fails where it failed before. A larger input evaluates each distinct key once in parallel through the value cache,
and so does a large column of inline values, which the decoding would hold once per row,
or one whose query-local payload the budget refuses. Its key column is charged for its
slots before evaluation and for its values' payload after it, so a query over its budget
fails after one column.
The numeric prefilter of `ORDER BY ?v LIMIT k`, the total-order check of a top-k scan
and the heap's first key follow the same rule, and their charges, the sort's positions
and its sorted copy are taken in both modes. An eager top-k scan of pieces without value
order now reads them in morsels of index blocks into one heap, as cursors did, rather
than reading every row of the pieces first. `sorts_agree_on_both_sides_of_the_threshold`
compares full sorts and top k over 8,192 and 32,768 rows eagerly and in cursors with
batches on both sides of the threshold.

The sorted pre-decode of the columns that row-by-row FILTER, BIND and ORDER BY
evaluation reads runs for inputs of more than 16,384 rows in both modes and whatever the
`expr_cache` switch says, and smaller inputs read their values through the value cache.
It is the one sorted pre-decode that also serves the dictionary values of top k and the
numeric FILTER and the ORDER keys of a variable, which decodes each distinct term once in
key order and admits its storage before it builds it. Each decoded column's slots are
charged before it is decoded, the distinct values with their own charge while they are
decoded, and the rows' values after. A column whose charge or decoding does not fit gives
that charge back and is left to the rows, and only cancellation and the deadline fail the
pre-decode. The shared pre-decode admits 128 bytes for each decoded row and 8 for each
row of the table before it starts, which is more than the 12 bytes and one value for each
decoded row that the separate large-input pre-decode admitted. Under a budget with little room left,
a large column can therefore fall back to its rows where it was decoded before, which
costs time but no correctness. The comparison of two string columns through cached
lexicographic floors serves eager tables and cursor batches of at least 4,096 rows. A floor
uses the string's key and its shared prefix length within the block. When that length is
unavailable, it reads the preceding key. If the preceding key extends the string, the
lookup searches for the first extension. These reads use the decoding envelope, and a
refused required read fails the comparison with the refusal it recorded. The floors last
the query in a charged cache, which the batches of a cursor share.

A unit test, `the_accounting_mode_is_read_in_a_bounded_number_of_places`, counts the
reads of the accounting mode outside tests and fails above its bound. A read is a call of
`streams()` or `accounting()`, a use of either `Accounting` variant or a direct read of
the `streams` field outside its accessors. The bound is 7. The first read is the routing
of `query_execution_with_plan` in `cursor.rs`. The second accumulates an EXISTS key set
across a cursor's batches, which an eager table builds in one batch. The third lets a
cursor's decoding envelope refuse on the ledger, because eager callers of `term` and
`value` cannot report a refusal and decode without the envelope instead. The other four read the field in `ctx.rs` to decide whether a cursor owner exists, whether a context
without an owner may keep retained state uncharged, whether an empty release trims the
ledger and whether a finished query's pool contribution shrinks. `CursorMemory` is gone, and a context in cursor accounting differs
from an eager one only by its `streams` flag, its ledger retention and its deadline. The
result cache and sampled filters stay off for cursors, since they are planning choices
that stage 6 makes independent of the mode.

Roadmap item 1039 removed the cursor scan's block charge. A scan over an immutable
snapshot kept its decoded index block between batches and charged a whole block's
columns, `BLOCK_ROWS * 32 + 512` bytes, when the memory left at its first read covered
that and one batch, and under a smaller budget it seeked each batch without a charge. A
query could therefore be refused under a budget above one at which it answered. On 2,000
subjects of one class, a count over `?s (:name|:none)* ?v` was refused under automatic
and streaming execution at 1,216 and 1,280 KiB, and the closure's unfilled LIMIT in every
mode at 1,152 and 1,216 KiB, while each answered within 512 KiB. With 69,536 subjects,
so that the scan reads whole blocks, streaming execution refused the count from 1,216 to
1,472 KiB and the LIMIT from 1,152 to 1,408 KiB. The scan now holds its block only during
a read and keeps a `WeakBlock` of its columns between reads, which the block cache keeps
alive, and it decodes the block again at its row when the cache has dropped it. The
batches that the cursor root hands out without copying share the block and one charge of
its decoded columns through one handle. The root scan keeps that handle while it reads
the block, so a writer that drops each batch before it pulls the next, as the result
writers do with one Rayon worker, does not charge the block again for every batch. While the root shares a block, the scan keeps no weak handle on it and reads it through the shared handle in place, and it takes the block back from that handle when it switches to owned rows inside the block or a batch declines the shared path, so a scan stopped inside a block always has a way back to it. The
charge ends when the scan leaves the block, reads it as owned rows or ends, and when the
last batch handed out from it drops, so a cursor stopped inside a block keeps it charged
until the cursor closes. The root shares a
block only when the memory left covers a whole block's columns and one batch, as it did,
and copies rows otherwise, so it never holds more than it did under the same budget.
`cursor_headroom` and `cursor_fits` count a block for automatic admission, which keeps
eager the plans it kept eager, and not for the eager memory retry, whose alternative is
the eager failure. The tests in `tests/scan_block_budget.rs` check that each mode's
answers are monotone in the budget on both fixtures, that the scan's peak does not step
by a block, and that root scans answer at every larger budget, one of them decoding a
literal of 300 KiB. A root scan that shares its block can still be refused the decoding
of a term larger than the room the block leaves, under a budget at which a copy would
answer, and roadmap item 1051 holds that. A cursor over a read-through snapshot, which keeps no blocks,
decodes its current block again for each batch of copied rows, and no cursor query reads such a
snapshot.

Roadmap item 1063 removed most of the cost that item 1039 left on root scans. The shared
path no longer makes and drops a weak handle on the block for each batch, and it reads the
block in place instead of taking it back at each read. On 100,000 people on atlas with one
thread, `root-scan`, `export-500k` and `root-limit` became 1.6% to 3.5% faster in server
time under streaming and automatic execution, and in a cycle profile on stanley the range
of the scan's cost over four passes overlaps its range before item 1039. Against that
build, `root-scan` still takes 2.4% longer under streaming, 1.98 µs, and 1.1% under
automatic execution, 0.89 µs, and streaming `export-500k` 0.5% longer, 1.95 µs. The
remainder is about the same on both queries although the export reads more blocks, and the
profile does not show it. Item 1063 stays open for that remainder. `distinct-obj` and `predicate-counts` read 0.3 µs
slower under streaming and faster under automatic execution, and their plans do not reach
the changed code. The
[run record](../../benchmarks/engine/runs/2026-11-19-shared-scan-atlas/README.md) has the
method and the numbers.

### One join-kernel library

The module `sparql/join` of `sparkles-core` holds one kernel per join algorithm, and
neither kernel knows the mode that runs it. The hash kernel owns the flat table of build
rows grouped by key, which the cursor's `HashIndex` was, with the eager `KeyGroups`'
partitioned parallel build above 65,536 rows, and one probe loop in the five modes,
inner, OPTIONAL, semi, anti and MINUS, that takes a range of probe rows and an output
room and returns pairs or verdicts. The keyed kernel owns the `EqualityIndex` of a join
keyed on the equality of its condition. The merge kernel takes the lower bounds of the
left keys in the right column from the zipper, which now gallops over the larger side
whichever side that is, and expands, counts or filters the runs without a branch per
row. The galloping search of the merge joins, the zipper and the index join is one
function with a predicate.

The eager executor hands each kernel its whole input tables as one batch and probes in
parallel pieces of 32,768 rows, checking the pairs of all pieces against the output
limits as they grow, and the cursor hands the kernels the batches it pulls and resumes
where the room ran out. The eager hash join, OPTIONAL, keyed join, semi join, anti join
and MINUS, the eager merge join, merge OPTIONAL, merge count and merge anti join, the
cursor's binary operator and the rows a loaded right batch decides in the cursor's merge
operator all run through the kernels. The cursor's merge operator keeps its runs across
batch boundaries and the spooling of long runs, and the ripple join and the index join
stay as they were.

A hash join's OPTIONAL condition is tested once over the gathered candidate rows of a
chunk in both modes, so the generic conditional OPTIONAL over two inputs that share no
variable no longer materializes their product. The condition of a join, in the hash,
keyed and merge forms of both modes, is evaluated from the ids of its candidate rows and
never has their terms decoded first as a FILTER over a large table has. The first kernel
tested conditions through the FILTER path, and decoding the 300,000 candidate rows of
item 570's eager OPTIONAL at 1M people, whose `?o != ex:x` the ids decide, took the
query from 28 ms to 120 ms on the laptop and to five times its server time on the Intel
worker. A unit test holds the conditional OPTIONAL over 20,000 rows, in both forms, to the
memory of the OPTIONAL without the condition plus its mask and merged rows and, in the
hash form, the kernel's scratch for one chunk of candidates, which together are below
what decoding the 20,000 terms charges. The `merge_left_join` switch selects the
merge OPTIONAL without a condition in both modes, and EXPLAIN names the algorithm that
ran, `[merge on ?k]` or `[hash join]`, in both. An OPTIONAL with a condition over two
sorted inputs merged only in eager execution, where its condition is tested over all of
the matched pairs at once, and hashed in the cursor until item 570 gave the cursor its
own conditional merge. Item 854 follows one conditional merge for both modes. An
eager hash join, OPTIONAL, semi join, anti join or MINUS whose hash table does not fit
the memory budget of a query that may spill runs again as the cursor's partitioned hash
join over its two input tables, whose Grace partitioning wraps the same kernel, and its
plan notes `[build did not fit: partitioned to disk]`. A keyed join partitions in neither
mode, since its sides share no variable to partition on, which item 853 follows. The
eager memory retry still covers the operators that do not spill.

A unit test checks the galloping search against `partition_point` on random sorted
slices, a cursor test checks that the OPTIONAL over sorted inputs honours the switch and
names its algorithm in both plans, and a spill test runs an eager MINUS under a budget
its hash table does not fit and expects the eager answer. The hash table of the smaller
side is charged in eager execution, which it was not before, and the eager OPTIONAL
builds on its right side as the cursor does rather than on the smaller side. An eager
MINUS, semi join or anti join on one bound key now builds the kernel's table with its
grouped rows, about twice the memory of the id set it used, which item 852 follows. The
measurement of item 570's conditional OPTIONAL in both modes on the benchmark worker,
which the plan asks of this phase, was taken after the cursor gained a conditional
merge, and the
[X05 outcome](../X05-streaming-query-execution.md#the-cursors-conditional-merge-optional-2026-11-19)
records it. A follow-up in the
[pair-condition X05 outcome](../X05-streaming-query-execution.md#pair-conditions-in-the-streamed-merge-2026-11-23)
enables `merge_pair_conditions` by default to test eligible single-key pairs before
copying output rows; measuring the streamed-to-eager ratio on `knows-*` and admitting
eligible EXISTS conditions once item 1105 unifies merge-order derivation remain under
item 570.

### One grouping operator

The module `sparql/group` of `sparkles-core` holds the accumulator trait and the grouping
operator, and neither knows the mode that runs it. An accumulator holds the state of one
aggregate for every group of a pass and folds a batch of rows into it per call, given
each row's slot, with `init`, `add_batch`, `merge`, `finish` and `retract`. `Running`
wraps `exec::AggState` for COUNT, SUM, AVG, MIN, MAX, SAMPLE and ARQ's variance and
standard deviations,
`Concat` holds GROUP_CONCAT's text, and `Distinct` passes each value or row of a group to
the aggregate underneath once. `Rows` runs the per-group kernel `exec::aggregate` or a
registered aggregate over its group's rows, which FOLD, registered aggregates and the
other custom aggregates, `MEDIAN` and `MODE` among them, need. `AggState` gained `merge` and
`retract`. `merge` keeps row order for SAMPLE, GROUP_CONCAT and the ties of MIN and MAX,
and a statistics aggregate does not merge. `retract` takes back rows with an optional
signed weight column on `Batch`, defaulting to unit weight for one-off queries. The
incremental view operator in `incr/ops/group.rs` runs every aggregate through the shared
`Accumulator` trait via `acc::retractable`, folding positive weights with `add_batch` and
negative weights with `retract`, and reading live groups without destroying their state.
`COUNT` uses `Running` directly, `SUM` and `AVG` use `RetractableSum`, `MIN`, `MAX`,
`SAMPLE` and `GROUP_CONCAT` use `Ordered` over an ordered multiset, and `DISTINCT`
aggregates wrap their inner accumulator in `RetractableDistinct`. All one-off and view
accumulators share `finish_sum_avg`, `finish_best` and `finish_concat` so every reading of
`aggregate-errors`, empty groups and numeric promotions agrees across both paths.
`RetractableSum` sums integers and decimals in 256-bit exact arithmetic and promotes an
overflowing exact subtotal to float or double when the group holds float or double terms,
keeping its result independent of row order where a one-off left-fold can overflow earlier
if overflowing decimals precede a float or double. Deleted non-implicit groups release
their slot and state with `swap_remove`, reclaim oversized or emptied backing buffers and
account for allocated buffer capacity in view memory budgeting, and `Group::save` writes a
versioned state header alongside the bumped incremental compiler version `f13-phase2-6` so
unknown versions and earlier checkpoints are refused cleanly.

The operator finds each row's group through a hash table on the keys, in order of first
appearance. When every aggregate keeps running state, rows arrive in any batches, and a
grouping that may spill passes the rows of the groups that do not fit to 16 partition
files and reads each in a pass of its own, as the cursor's grouping did. The files hold
input rows and never accumulator state, so no format changed. Otherwise the operator
takes one batch that holds every row of each of its groups and indexes the rows of each
slot once for the aggregates that read them. The eager grouping is one call of the
operator over its whole input table, folded in chunks of 65,536 rows so that GROUP_CONCAT
text and DISTINCT sets are charged as they grow. The cursor's `Group` drives the operator
over the batches it pulls, and its `RunGroup` holds the rows of unfinished groups of an
input ordered on the keys and runs the operator over the finished ones. `exec::group`,
`exec::group_incremental` and `cursor/runs.rs` are gone.

Every running aggregate is admitted in both modes, so DISTINCT aggregates, expression
arguments, GROUP_CONCAT and groupings on several keys keep one state per group in eager
execution as well, and EXPLAIN marks them `[incremental]` in both. Expression arguments
are evaluated once per batch through `compute_column`, which reuses one value per
distinct input in both modes. An eager grouping of a query that may spill charges its
groups, and when they do not fit, it runs again as the cursor's grouping over its input
table and its plan notes `[groups did not fit: partitioned to disk]`. An eager grouping
of a query that cannot spill charges nothing, as before. The `incremental_group` switch stays as a diagnostic that sends every aggregate through `Rows`, so the optimization tests keep an independent check of the running states until item 512's reference evaluator replaces it, and the merge phase retires it. Eager hashable groupings with at least `PAR_GROUP_THRESHOLD` and at most `u32::MAX` rows on multi-threaded pools now dispatch to `par::whole_parallel`, except ungrouped pure-count plans that take the constant-time row-count shortcut. `whole_parallel` runs contiguous range-split folding with `Accumulator::merge` for ungrouped and low-cardinality mergeable groupings and key-hash partitioning for higher-cardinality groupings and for grouped non-mergeable running aggregates that sample more than one distinct key. Ungrouped non-mergeable plans go straight to sequential grouping, and grouped non-mergeable plans with at most one sampled distinct key also bypass hash partitioning and run sequentially. `AggState::can_merge` and `Running::merge` check exactness against single-pass semantics for `SUM`, `AVG`, `MIN` and `MAX`, re-folding only refused aggregates sequentially over the built global group index, and falling back to key-hash partitioning on high cardinality or to sequential execution under tight memory pressure without latching sticky memory failures. The 12-thread benchmark cells on `forge` remain open under roadmap item 1066 to decide where parallel dispatch stays enabled. A DISTINCT accumulator charges the slots and values it passes to the aggregate underneath while that aggregate reads them.

Tests compare mixed aggregates with HAVING between eager execution and the cursor at
batches of one row and 4,096 rows, with and without the memory to hold the groups, and
run an eager grouping under a budget its groups do not fit, which spills in partitions
and returns the eager answer. A grouping holds its value cache outside the pass
threshold, so GROUP_CONCAT or MIN and MAX over many distinct values can still exceed a
small budget while the pass has room, as before. A group whose own state, such as a
DISTINCT set or a GROUP_CONCAT text, exceeds the budget cannot be split and fails, while a
group whose rows alone exceed the budget lands in one partition and is folded whole.

On forge alone, at 1.05M triples with one thread in twelve rounds of A,B,B,A against the
base 7e272f4d, eager `expr-bind-group` read 0.839, `expr-agg-arg` 0.888 and `group-avg`
0.906, and streamed they read 0.975, 1.004 and 0.980. `types-grouped`, `predicate-counts`
and `subquery-agg` keep their count plans and read 0.955 to 1.021 in both modes, within
the base's spread. Eager `types-grouped` read 1.003, a median of 0.02 µs more in one order
of the A/B, under the 0.5 µs bound of the Intel rule. Both builds
returned the same answers. The
[synthetic results](../../benchmarks/synthetic/RESULTS.md#grouping-operator) keep the run
record.

### Native search and leaf operators

The first part of item 829 classifies each search, call and leaf by its form, from the
`needs_input` of its specification, in `cursor/leaf.rs`. An expanding operator in
`cursor/expand.rs` runs the kernel over each input batch for a text, hybrid, history or
spatial search whose arguments come from its input and for a vector search with a
variable query. It keeps the distinct query values of earlier batches in a charged set,
so the caps of the text, hybrid, spatial and vector kernels hold for the whole input as
they do for one eager call. A remote LATERAL loop runs through `enhancer::RemoteRun`,
which the eager loop calls once and the cursor calls per batch. The cursor holds the rows
of a partial bulk request until the request is full or its input ends, so it sends the
requests and the bulk groups of an eager run, and it keeps the answers it holds charged
and counts each key's solutions once. Both modes still parse a whole remote response before
the budget sees it, which [item 1104](../../ROADMAP.md) changes.

The whole-input operator of `cursor/whole.rs` serves a vector or hybrid search whose
candidates come from its input, a path search under a limit and an algorithm call that
reads its group or a nested edge pattern. It reads each input into a charged table, so
an input over the budget refuses the query as a sort without spill does, and then calls
the kernel once and serves its answer batch by batch. It calls `exec::vector_search`,
`hybrid::search_bound`, `pathsearch::run` and `algosvc::run` itself rather than
`run_node`. It adopts an algorithm's lease, charges every other answer, counts its rows
against the work limit once and redacts its plan under a graph view, which `run_node`
did for these kinds. It does not read or fill the result cache, so an algorithm call over
an edge pattern that the cache answered under eager execution runs again when streamed.
A search bounded by `k`, `maxHits` or its own limit argument, an algorithm call over
constants and `CountJoin` run their kernel once into a charged table, as decision 2
admits. The expanding kinds and those leaves still
run the eager kernel through `execute_with_inputs` or `execute_owned`, so the merge must
move them. EXPLAIN gives each operator a reason that names what it reads whole or what
bounds its answer.

The strict policy originally refused the unbounded forms, which included SERVICE and view reads, a
spatial scan or relation over constants, a search without a bound, a path search between
constants without a limit, a spatial join, a property function over constants and a
registered property function without prefix output. They are the second part of the item, whose [readers](#streamed-service-text-and-history-readers) serve a SERVICE without SILENT, a text search without a bound and a history query without a limit. Automatic execution keeps its base
set of expanding kinds, and each new kind joins it only after a same-host A/B of eager
against streamed runs. The text, geo and vector benchmark scripts send an `EXECUTION`
mode with every request, and the vector script fingerprints its exact and approximate
answers, for the A/B of the item's plan. The W3C streaming record of the tip matches its
base, with 24 eager fallbacks in ARQ's property-function suite and none elsewhere.

Tests compare each form with eager answers at batches of one row to 4,096 rows, count
the same rows as eager runs against the work limit, refuse an input over the memory
budget in both modes, read a pending transaction's view in an algorithm call, redact a
masked plan, and send the requests of an eager remote loop at every batch size, with a
failing endpoint and with SILENT.

`CountJoin` under streaming groups the rows of its join without keys and asks a merge
join for its count, so it never holds the pairs that eager execution forms. The two modes
admit the pairs against the row limit and the work limit by the same rule. The eager count
counts a merge join on one shared variable from its key runs and forms the pairs of any
other join, admitting them before it counts them, and the streamed count admits each
batch of pairs of the same joins and checks the total once more when its join has read
all of its input. Three differences remain. Eager execution chooses between the key runs
and the pairs at run time and forms the pairs when a key column holds UNDEF or is not
sorted, while the cursor chooses from the plan, so a limit can refuse such a join in one
mode and not in the other. The eager count also admits the memory of its pair indices,
which the streamed count never holds, so it can refuse under a memory budget that the
streamed count meets, and the merge removes that difference. A count can refuse at a work
limit equal to the work that its own run reported, in both modes, and
[item 1103](../../ROADMAP.md) makes the two counts agree.

A count over a merge join on two or more shared variables runs the eager kernel under a
cursor instead, as a whole-input operator under either policy, because the streamed join
writes every pair into its batches and took 1.81 times the eager count on forge. The
operator reads both inputs under the memory budget and counts once, so it holds both
inputs and the pair indices where the streamed join held one batch of pairs, and an eager
run of such a count that exceeds the memory budget with spill on retries into the same
kernel. [Item 1129](../../ROADMAP.md) counts that join in the cursor without writing its
pairs and then streams it again.

The ignored test `measure_whole_input_and_leaf_forms` in `query_cursor.rs` measured the
peak memory and the best of five times of each form on 200,000 subjects in a release
build on the development laptop, which is not a benchmark host. A path search under a
limit of 10 over its group or over an edge pattern peaked at 7.1 MiB streamed against 3.1
MiB eager and took the same time within 1 ms. An algorithm call over its group,
over an edge pattern or over constants peaked within 1 MiB of eager and took within 1 ms of
it. A streamed count over a join that eager counts from its pairs peaked at 0.3 MiB
against 6.1 to 7.6 MiB eager. Over a filtered scan it took 6.0 ms against 6.3 ms, and on
two shared variables it took 9.8 ms against 8.6 ms, through the streamed join that a
cursor no longer runs for that shape. Streaming trades that time for the
memory, and automatic execution keeps such counts eager until the A/B of the item's
plan admits them. A count of two scans on one variable from their key runs took 12.0 ms
streamed against 6.1 ms eager.

On forge at 1.05M triples with one thread, 9df856f3, the part-1 tip before a cursor
returned the count on two variables to the eager kernel, was compared with
its base, bccc5f7b, in twelve rounds of A,B,B,A in both orders, and the tip's eager and
streamed counts with each other in the same way. The streamed count on two shared
variables took 1.81 times the eager count, 9.58 ms against 5.28 ms, and runs 2.26 times
the instructions per request on stanley, since the cursor writes the merge join's pairs
into batches that the eager count holds as indices, and item 1129 removes that cost.
Explicit streaming has since run the eager kernel for that shape, as the paragraph above
says. The
streamed count over a filtered scan took 0.15 of the eager time, 8.4 ms against 55.3 ms,
and automatic execution keeps both counts eager. Against the base, streamed queries that
run 0.6% to 1.9% more instructions are 1.3% to 5.2% slower, which item 1160 holds, and
queries that run the same instructions in either mode are 0.6% to 2.8% slower, which item
1161 holds as likely code layout. Item 1074 holds the costs under 0.3 ms within its cap.

The text, geo and vector scripts ran on forge in three rounds of the two binaries, with six
pairs per query, as the phase's comparison of wall time. Of 88 configurations, 80 read as
noise and 5 as faster. Eager `q6-touching-counties` with one thread reads 1.2% slower and
eager `q-ef-64` with one thread 8.0% slower, which is over the plan's 5% threshold on wall
time. Its wall times are bimodal, its servers differ from each other by up to 78% and its
server time is 1.2% longer, and item 1162 holds both costs. Streamed `q9-star` with 12 threads read 1.144, and a rerun of that query
alone, with 12 server processes per build and mode, put the tip at cde29b37 at 0.966 of
its merge base with main, 5f3f69c2, streamed and 1.006 eager, so no query is 5% or more
slower at 12 threads, which is the stage's acceptance gate. The
[synthetic results](../../benchmarks/synthetic/RESULTS.md#leaf-cursors) list each query.

### Streamed SERVICE, text and history readers

The second part of item 829 gives three of the refused kinds resumable readers that hold
one page of their answer between pulls. They serve explicit streaming, and automatic
execution runs these forms eagerly until a same-host A/B admits them. Each opens nothing
until its first pull, because operators are also built to learn whether an EXISTS
pattern streams, so its checks, its request and its errors come when an eager run would
reach the node.

A SERVICE without SILENT, in `cursor/service.rs`, parses a remote response as it arrives
and holds one output batch and the parser's buffer. A query endpoint of the same process
opens as a cursor of its own under the caller's policy, charged to the caller's memory
budget, and the reader pulls one of its batches per batch. One blank-node map spans the
whole answer, as in one eager call. A cached answer is read from its entry, which the
reader keeps charged while it reads, since the cache may evict the entry meanwhile. Three
things differ from an eager run. A failure after rows were emitted fails the cursor. An
answer enters the cache only when it was read to its end, and the copy for the cache is
dropped once it grows past the largest entry the cache keeps or past what the budget
holds beside the answer. The outbound timeout counts from the start of the request, while
the query's deadline leaves out the time a consumer keeps the cursor waiting, so a slow
consumer can expire a transfer that an eager run would finish. In part 2, the strict policy
refused a SILENT SERVICE, which needed its whole answer before its first row, and a JSON
response whose results come before its head, which [item 1125](../../ROADMAP.md) decodes in
either order; part 3 below moves SILENT SERVICE onto a native budget-buffered cursor. Under
the permissive policy, a late-head JSON response is buffered under a memory
charge, and the runtime plan reports and retains `fullInputBeforeOutput` on that SERVICE
node after the response is released. Its refusal inside an EXISTS fails the query, as it
does outside one.

A text search with a constant query and no limit, in `cursor/text.rs`, collects its
scored hits in one pass over each index segment, charging the hit list before each growth,
and decodes rows from it a page at a time. The row state that eager execution keeps for
one call, which holds the duplicate set, the default graph's filter, the stale hits and
the rank, lasts across pages, so ties and duplicates that span a batch keep their eager
answers. A search over more hits than the index's `maxHits` fails with eager's row error.
A search with a limit keeps the bounded leaf of the first part, and
[item 1155](../../ROADMAP.md) routes a limit above `maxHits` through the reader. An
earlier form counted the hits without scores before it collected them, which built the
query's automaton and scorers twice and cost about 2.2 million instructions per request,
and the [experiments page](../../PERFORMANCE-EXPERIMENTS.md#query-execution) records it.

A history query without a limit or input, in `cursor/history.rs`, reads the change log a
page of whole commits at a time through `store::HistoryPages`. Each record's frame is
admitted before it is read and its decoded size before it is decoded, and the page stays
charged until its rows are handed out. A commit larger than a page is held whole, as eager
execution holds it. The reader pins the log at the first commit of its range until the
range ends or the reader is released, so retention keeps the segments it has still to
read, past `maxBytes` too, and turning the log off is refused meanwhile. It counts the
changes it accepts and fails past the row budget at the change where an eager run fails.
Deleting the branch or dataset still removes the segments under a read, which
[item 1173](../../ROADMAP.md) holds, eager execution does not admit records before it
decodes them, which [item 1124](../../ROADMAP.md) holds, and a read-only log's newest-first
walk is [item 1175](../../ROADMAP.md).

Every `SERVICE path:search` without `path:limit` now runs natively through
`cursor/pathsearch.rs` and `pathsearch::Reader`, covering `path:shortest`,
`path:allShortest`, `path:kShortest` and `path:all` across constant or variable
endpoints, group inputs and nested edge patterns (items 1122 and 1123). Search trees,
distance maps, priority queues, candidate tables, path-reconstruction stacks and
triple-term caches pre-admit every allocation and growth through the query's memory
budget before allocating and release their charges on completion, error or early stop.
Single-ended breadth-first and Dijkstra searches, bidirectional unweighted and weighted
`path:allShortest`, Yen's algorithm and depth-first `path:all` stream matching paths one
at a time through resumable iterators, so a `LIMIT` over a graph with many tied shortest
paths stops without materializing every path. Searches over a group input or a nested
edge pattern without `path:limit` read their group rows into a charged buffer and stream
edge-pattern batches into pre-admitted adjacency maps before streaming matching paths
batch by batch, reporting `fullInputBeforeOutput` on the `PathSearch` node. Grouped
streaming emits each discovered path across its matching group rows without
materializing every path of a search, matching eager execution in full solution
multiset, `pathIndex` numbering, per-node search counters and work counts at every batch
size while allowing row order across group rows that share an endpoint to vary when no
`ORDER BY` is present.

A maintained view read streams from the immutable version published by its circuit.
Current and stale readers release the circuit lock after taking a reference. A published
row table and frozen term table count toward the view memory pool while readers still
hold them. A local term key has one charge across the circuit and all frozen tables that
share its backing. When the circuit has advanced
within the retained history, the reader admits and folds history slices and applies the
net change while streaming the shared table.

When history has expired or the reader asks for another aggregate-errors reading, a
child query runs under the caller's memory, row-work, spill, cancellation and deadline
controls. Rebuilds admit the principal's permission mask before allocating it. The child
retains the mask's charge and releases temporary planning credit before it streams.
SELECT rebuilds preserve the maintained treatment of each REDUCED occurrence. CONSTRUCT
rebuilds use the maintained VALUES terms for template blank-node identity and charge
distinct graph state as the graph streams. Strict policy also
applies inside the child. A custom source that only implements eager view reads requires
the permissive policy. The cursor inventory and heap audit cover the maintained readers.
The inventory test `cursor_inventory.rs` lists every built-in plan form as native.

Leaf ARQ property functions (`Kind::PropertyFn` without plan inputs) and `SILENT SERVICE`
now run natively under `FallbackPolicy::RejectMaterialization`, completing item 829.
`cursor/propertyfn.rs` and `arqpf::LeafReader` stream store-backed collections, RDF
containers, `rdfs:member` triples and `apf:strSplit` tokens a batch at a time under
`OwnedCharge` with cooperative control polling, and emit constant scalar functions on
first pull. `cursor/service.rs` runs `SILENT SERVICE` lazily by draining remote and
in-process responses into a memory-charged buffer before the first row so open-time and
mid-stream endpoint failures become one empty solution while cancellation, query
deadlines, policy refusals, budget limits and strict callee refusals still propagate;
cache hits stream directly and late-head JSON is accepted under byte metering because the
operator already reports `fullInputBeforeOutput`. Every built-in form in
`cursor_inventory.rs` runs natively, and the ARQ property-function and CDT streaming
suites record zero eager fallbacks. Strict cursors still refuse only unprefixed
user-registered property functions, legacy eager-only `ViewSource` implementations,
late-head JSON responses on non-`SILENT` `SERVICE` (tracked in
[item 1125](../../ROADMAP.md)) and operators whose `EXISTS` embeds one of those shapes.

Tests compare each reader with eager answers at batch sizes down to one row, refuse the
budget and row limits where eager execution refuses, measure each reader's heap against
its ledger in `cursor_text_heap.rs`, `cursor_history_heap.rs` and
`cursor_memory_audit.rs`, and cover retention and disabling between pulls, a branch
deleted under a read, endpoint failures, timeouts, cancellation and the cache.

Checked history cursors honor source and query cancellation and absolute deadlines during
read-only timestamp lookup as well as during page reads. To avoid allocating term tables
while resolving time bounds on a read-only log, `ReadOnlyLog::commit_at_time` validates
each frame's CRC, metadata, term framing and quad term references in place without
materializing `Arc<[u8]>` term payloads, while preserving consecutive-prefix semantics,
full-walk head-before-clamp selection and cooperative stop precedence. On `forge`, an
Intel i5-13500 pinned to CPU 2 with one Rayon thread, matched release builds of
`2233637a` and `01b46ba1` ran all 26 cells of the history timing matrix with 12 ABBA,
12 BAAB and 12 same-binary rounds of 200 samples per process and 100% row-count
agreement. Checked first pulls on archived ancestor logs became 9.9% faster on
`ancestor-one`, at 0.901 of the base and 16.95 ms against 18.80 ms, and 15.1% faster on
`ancestor-two`, at 0.849 and 20.90 ms against 24.61 ms. No cell failed the 2% bound.
The 16 primary cells on writable history logs that showed a real slowdown between 0.46%
and 1.13% are recorded below for [item 1074](../../ROADMAP.md) on host `forge` comparing
`2233637a..01b46ba1`, to be recovered by profiling the checked cursor's per-entry control
polls and timestamp-index lookup path within two batches or by 2026-12-08.

| Query and route | Phase | Host | Commits | Baseline | Ratio and 95% interval | Added | Recovery and deadline |
|---|---|---|---|---:|---|---:|---|
| `commit-warm` checked | `first` | `forge` | `2233637a..01b46ba1` | 8.26 ms | 1.0053, 1.0036 to 1.0063 | +43.9 µs | Item 1074, within two batches or by 2026-12-08 |
| `commit-warm` ordinary | `first` | `forge` | `2233637a..01b46ba1` | 7.50 ms | 1.0046, 1.0029 to 1.0059 | +34.5 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-warm-first-one` checked | `first` | `forge` | `2233637a..01b46ba1` | 123.1 µs | 1.0103, 1.0080 to 1.0113 | +1.26 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-warm-first-two` checked | `first` | `forge` | `2233637a..01b46ba1` | 123.6 µs | 1.0112, 1.0099 to 1.0121 | +1.38 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-warm-cross-one` checked | `first` | `forge` | `2233637a..01b46ba1` | 8.12 ms | 1.0075, 1.0060 to 1.0089 | +60.8 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-warm-cross-one` ordinary | `first` | `forge` | `2233637a..01b46ba1` | 7.37 ms | 1.0056, 1.0027 to 1.0090 | +41.3 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-warm-cross-one` eager | `eager` | `forge` | `2233637a..01b46ba1` | 31.48 ms | 1.0113, 1.0094 to 1.0126 | +354.3 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-warm-cross-two` checked | `first` | `forge` | `2233637a..01b46ba1` | 8.23 ms | 1.0057, 1.0048 to 1.0069 | +46.8 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-warm-cross-two` ordinary | `first` | `forge` | `2233637a..01b46ba1` | 7.49 ms | 1.0046, 1.0030 to 1.0061 | +34.4 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-warm-cross-two` eager | `eager` | `forge` | `2233637a..01b46ba1` | 30.99 ms | 1.0103, 1.0083 to 1.0141 | +320.7 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-uncached-one` checked | `first` | `forge` | `2233637a..01b46ba1` | 8.21 ms | 1.0083, 1.0050 to 1.0117 | +68.0 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-uncached-one` ordinary | `first` | `forge` | `2233637a..01b46ba1` | 7.43 ms | 1.0089, 1.0064 to 1.0108 | +66.3 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-uncached-one` eager | `eager` | `forge` | `2233637a..01b46ba1` | 31.56 ms | 1.0107, 1.0090 to 1.0120 | +337.0 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-uncached-two` checked | `first` | `forge` | `2233637a..01b46ba1` | 8.45 ms | 1.0060, 1.0042 to 1.0089 | +50.4 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-uncached-two` ordinary | `first` | `forge` | `2233637a..01b46ba1` | 7.71 ms | 1.0104, 1.0061 to 1.0126 | +80.3 µs | Item 1074, within two batches or by 2026-12-08 |
| `time-uncached-two` eager | `eager` | `forge` | `2233637a..01b46ba1` | 31.17 ms | 1.0104, 1.0084 to 1.0119 | +325.0 µs | Item 1074, within two batches or by 2026-12-08 |

All three streamed readers report `growingState` from construction onward because the
batch cap does not bound their retained state. An in-process SERVICE call clears the
target's pool alongside inheriting the caller's grant so an unpooled caller keeps its
unpooled accounting domain and a pooled caller charges only its own grant, and
`Service::fill` polls stop conditions between remote solutions, before cache
publication and across cached and local rows. The history reader uses the same execution
check when it opens on first demand, resolves timestamp bounds, reads pages and converts
changes to rows. Writable timestamp lookup polls during cold index reads, CRC and
decoding. Read-only timestamp lookup polls while walking the captured log. Both page
backends poll during record reads, CRC, footprint calculation, decoding and filtering.
Consumer wait credit still extends ordinary query deadlines. A checked source retains
its original cancellation controls and absolute source and query deadlines. A zero
internal page budget asks for at least one matching change. Read-only paging skips
segments above the clamped upper commit bound and stops once a validated record covers
that bound, while [item 1175](../../ROADMAP.md) retains the valid-prefix limitation for
earlier records in an overlapping segment.

### Readings of errors in aggregates

Item 1064 makes the treatment of errors in MIN, MAX, AVG and GROUP_CONCAT a setting,
`aggregate-errors`, with the readings `jena`, `sparkles` and `spec`. `jena` is the
default on every surface. The readings differ only when the aggregates finish, so the
running accumulators, the per-group kernel and the incremental group operator keep a
count of the arguments that were errors and decide at `finish`. A count rather than a
flag keeps retraction exact, since a group that loses its last error answers again as
if it never had one, and it keeps item 1066's merges of partial states exact. A query's
reading comes from `QueryOptions::aggregate_errors`, else the dataset's setting in
`aggregate-errors.json`, else the store's option, which is the server's default.

The `jena` reading was fixed by running ARQ 6.2.0 on every aggregate, with and without
DISTINCT and over a variable and an expression, for groups with unbound values, type
errors, only errors and no rows. The fixture `tests/fixtures/aggregate-errors/` keeps
that output, and the `jena` reading gives every answer in it apart from SAMPLE's choice.
An error makes SUM, AVG, MIN, MAX and GROUP_CONCAT unbound, and COUNT and SAMPLE pass
over it. The one further difference was `GROUP_CONCAT(DISTINCT …)` over no rows, which ARQ makes
unbound and the `jena` reading follows. The planner's shortcuts were audited, and only
the COUNT plans assume anything about errors, which every reading shares.

Item 1084 gives AVG of integers and decimals the same division as ARQ under all three
readings. Eager queries, cursors and maintained views divide the exact subtotal by the
count. Partial groups merge their subtotals before dividing. A terminating quotient
stays exact, and a repeating quotient rounds to 24 fractional digits using half-even
rounding. A quotient outside the numeric bounds is unbound. The view compiler
fingerprint changed so checkpoints that store results at the previous precision are
rebuilt from the data.

Materialized and incrementally maintained state follows the dataset's reading, else the
server's. Views and live queries run under it, switch when it changes as they switch on
a change of inference, and fingerprint their checkpoints with it. A query whose own
reading differs never reads or writes the maintained state. It evaluates the view's
query from the data at its snapshot, and its plan notes the fallback. A Group node names
a reading other than `jena`, which also keys the result cache. The group state's new
error counts changed the incremental compiler's version, so a checkpoint of an earlier
release is refused and the view is rebuilt from the data, and no file format changed.

The W3C SPARQL suites pass under all three readings, which
`mise run test:w3c:aggregate-errors` checks for the two readings that are not the default.
None of their tests tells the readings apart. `agg-err-01` averages a group that holds a
string, which leaves AVG unbound under every reading, and their MIN and MAX tests read
groups in which no value is an error, so the fixture of ARQ's answers in
`sparkles-core/tests/aggregate_errors.rs` is what separates the readings.

### Measurements of stage 1

A Namespace screen of the base against the five deletions ran the 30 synthetic queries
at 1.05M triples with one thread in four A,B,B,A rounds. The geometric mean of server
time was 0.982 of the base, and no query was 5% or more slower. `regex-iri` read 1.024
and `class-predicates` 1.020 with every round above 1, and the
[measurements of stage 4](#measurements-of-stage-4) close both. EXPLAIN printed the same plans before and after for four generated groups
of 17 to 22 triple patterns over the same data, a chain, a chain with a filter, a star
and a snowflake, since a group of more than 16 inputs already kept the greedy plan of
`joinorder.rs`.

### Measurements of stage 3

A Namespace screen of the base against items 480 to 483 ran the 30 synthetic queries at
1.05M triples with one thread in four A,B,B,A rounds, with both binaries built on one host
by the same compilers. Both returned the same answers to every query. The geometric mean
of server time was 0.984 of the base, and no query of 0.3 ms or more was 5% or more slower.
`distinct-obj`, which runs in about 0.008 ms, read 1.130, and a rerun of it with
`count-all` and `path-plus` in 12 rounds read 1.155, 1.059 and 1.037. Server time covers
the execution of the plan, which none of the items changes for these queries, so item
524 measures `distinct-obj` on an Intel host. Building the options of a request
through the caller context and the query request builder adds a few moves of the options
and one reference count, outside the measured time.

### Measurements of stage 4

Stage 4 was measured on forge, alone under its benchmark lock, against the record of
e52937ca that the [synthetic results](../../benchmarks/synthetic/RESULTS.md#forge-record-after-stages-1-and-3)
keep, and in interleaved A/Bs of release builds. Each A/B ran ten rounds of A,B,B,A in
each order with 12 Rayon threads at 1.05M triples, or four rounds at 10.5M, and the
latency harness of decision D4 ran the queries under 0.3 ms with 200 requests per round.

Several of the first comparisons were wrong because Boxington's learned incremental
reuse had compiled one build of a pair and not the other. Such a build of `sparkles-core`
had a `.text` section about 2% smaller than a fresh compile of the same commit. On atlas
at 10.5M triples with 12 threads, streamed `expr-agg-arg` took 26.2 ms in a fresh build
before per-value reuse merged, 26.6 ms in a fresh build after it and 29.4 ms in an
incremental build of the same code, which forge had read as a regression of 1.19. The
incremental builds of the charged query state and its base likewise failed the D4 rule on
`employee-docs`, `star-lookup` and `values-star`, which a fresh pair passes. Every number
below compares fresh builds, timed binaries are now built without the reuse, and item
677 records it in the build configuration.

A query that compiled each regular expression again made `regex-iri` 8 to 10% slower at
1.05M triples with 12 threads than the thread-local cache it replaced, and a kept program
with cold search caches on every worker still read 1.069 in eager execution and 1.034
when streamed. With the workers' clones parked between queries it read 1.010 and 1.015,
and 1.011 and 1.004 at 10.5M triples. With one query thread it read 1.029 and 1.030,
about 115 µs of 3.9 ms, until the key kernels found each clone once per piece.

The first build of the size-chosen sort keys made streamed `expr-order-key` 1.20 times
slower and `order-by-full` 1.06 times slower at 1.05M triples with 12 threads, because a
cursor batch above 16,384 rows went to the hashed parallel distinct of per-value reuse
and a variable key cloned a value per row. With dense integer slots and decoding per
distinct term above 16,384 rows too they read 0.953, 0.917 and 0.819 for `expr-order-key`,
`order-by-full` and `range-topk` in eager execution and 0.999, 0.997 and 0.996 when
streamed.

Against be727388, the build before the mode-neutral kernels, the final build ran every
query above 0.3 ms that the kernels change within 1.019 of its time at 1.05M triples with
12 threads in both modes, and eager `order-by-full`, `range-topk` and `regex-iri` read
0.921, 0.802 and 0.957. `types-grouped`, a GROUP BY of about 0.011 ms with an ORDER BY over
its few groups, read 1.130 in eager execution and 1.053 when streamed, 1.4 and 1.0 µs more,
which fails the D4 rule. The [per-query costs after stage 4](#per-query-costs-after-stage-4)
removed it. At 10.5M triples in four rounds,
every such query read within 1.024 except streamed `expr-order-key` at 1.045, 0.49 ms of
10.8 ms, which item 680 investigates, and eager `order-by-full`, `range-topk` and
`expr-order-key` read 0.797, 0.928 and 0.971.

The whole stage was then compared with main at the commit it merged, 58bbfca5. Writing
`export-500k` as `application/x-sparkles+json` in eager execution took 1.248 times as long,
52 ms of 210 ms, with equal execution times, because that writer decodes each cell through
`Ctx::term`, which checked the envelope before each step of a key's rebuild. With the
unchecked rebuild described above it read 1.0085. The same comparison put eager
`regex-iri` at 1.058 at 1.05M triples with 12 threads, about 60 µs of 0.95 ms, and eager
`subquery-agg` at 1.12 at 10.5M triples, about 80 µs of 0.68 ms. Builds on either side of
each step placed the first after the regular expressions became query-owned, where finding
each thread's clone for every key cost the time described above, and the second at the
step that charged retained scratch, whose parallel map took pieces of at least 4,096 rows
in both modes. The 20,003 groups that `subquery-agg` filters at 10.5M triples then ran on
at most five of twelve threads, and pieces of one poll's rows spread them over every
thread again. With both changes, eager execution at 10.5M triples with 12 threads read
1.029 of main for `subquery-agg`, 1.023 for `expr-bind-group`, 1.020 for `regex-iri`,
1.011 for `lang-filter`, and 0.995 and 0.998 for `expr-agg-arg` and `group-avg`. The same
queries read at most 1.011 when streamed at 10.5M triples, and with `star-join` at most
1.02 in both modes at 1.05M triples, where eager `regex-iri` read 0.998. Item 682
holds the costs of 1% to 3% that remain in eager execution at 10.5M triples. With one query
thread at 1.05M triples, `regex-iri` read 0.990 in eager execution and
0.978 when streamed. The latency harness ran on the merge before the unchecked rebuild and
these two changes, which leave queries this short alone. It passed every query under 0.3 ms in both modes except
`star-lookup`, which read 1.019 in eager execution and 1.017 when streamed, about 1.1 µs of
59 µs, and eager `types-grouped` at 1.186, which the
[per-query costs after stage 4](#per-query-costs-after-stage-4) follow up. In twelve rounds of 200 requests, eager `subquery-agg` at 1.05M triples read 0.999
of main, and main read 0.981 of itself.

The 12-thread matrix of 9d086646, the build with the mode-neutral kernels, returned the
same answers as the record of e52937ca in every cell. The execution times above 0.3 ms
that it flagged against that record, eager `lang-filter`, `regex-iri`, `star-join` and
`subquery-agg`, are among the queries that the comparison with main measured, and the
flags of `regex-iri` and `subquery-agg` were the two costs it traced. Streamed
responses still take about 0.27 ms more wall time than eager ones on the queries under
1 ms, as they did before the stage, so chunked live reservations removed no measurable
share of item 534's cost.

### Per-query costs after stage 4

The forge check after stage 4 merged found queries of a few microseconds slower with one
thread than the main before it, 005df56d. `count-all`, `distinct-obj` and `types-grouped`
read 1.135, 1.125 and 1.138, 0.6, 0.5 and 1.0 µs more, and eager `order-by-full` at 1.05M
triples with 12 threads read 1.033. Counting user-space instructions per request of the
server with `perf stat` on atlas, with one Rayon thread, put the whole request of these
queries 2,800 to 6,400 instructions above 005df56d, and profiles of the plan's execution
with LBR call stacks traced the share in the timed execution to five costs.

- The query's regular-expression cache replaced its empty map when the query's context was
  shared and again when its work ended. Each replacement of an `ArcSwap` pays the debts of
  every thread that ever read the map, about 900 instructions. Freeing the context paid them
  again, and the map and the 64 shards of the decoded value cache were allocated, set up and
  freed for queries that never used them.
- The expression cache of a BIND asked whether the expression was eligible, which walks it
  and collects its variables, before it checked the input's size, so the one row of an
  aggregate's BIND paid for the walk. FILTER and BIND inputs of up to 16,384 rows read the
  clock and asked whether their expressions could run on the pool even when the batch was
  too small to split, and ORDER BY keys called the per-value paths that decline fewer than
  1,024 rows.
- The key column of a small eager sort resized its retained charge on every row, which
  upgrades the context's weak owner and releases nothing for a numeric key, about 1,000
  instructions over the few groups of `types-grouped`.
- The sort of `order-by-full`, 102,000 distinct names, decoded each key twice in parallel,
  once to charge its payload and once to build it, and flattened the numbers, the values and
  the pairs of ids and rows on the query's thread. Its serial part grew from 22.7 to 25.0
  million cycles per request.
- Freeing the 102,000 decoded keys after the sort ran on the query's thread, about 9.7
  million cycles in both builds.

The regular-expression map and the value cache's shards are now made on first use, so a
query without patterns or decoded values neither builds nor frees them, and clearing an
empty map keeps it. The BIND cache checks the input's size first, and small inputs skip the
clock, the walk of their expressions and the per-value paths. A key column covers each
value's payload a reservation step ahead, at most 64 KiB or a 64th of the budget, and
takes the exact amount when the step is refused, so a query at its budget fails where it
failed before and each column holds at most one step of charge above its values. From 4,096
distinct terms the decoding runs in one parallel pass over pieces of the sorted ids, in
which each piece reads its numbers from the numeric column, admits each value's payload
before it builds the value and writes it in place. A piece's charge lives until what it
wrote is cleared, and the pass's charges join the column's charge, less their step slack,
when it ends. A sorted key column of more than 16,384 values is freed on the pool before
its charge is released.

On atlas, the whole request of `count-all`, `distinct-obj` and `path-plus` took about 800
fewer instructions than on 005df56d after these changes, `predicate-counts` the same and
`types-grouped` about 1,200 more, against 2,800 to 6,400 more before them. In the
benchmark host's latency harness with one thread at 1.05M triples, in twelve rounds of 200
requests in each order, the final build read `count-all` at 0.924 of 005df56d,
`distinct-obj` at 0.948, `path-plus` at 0.941, `predicate-counts` at 0.978 and
`predicate-subjects` at 0.967, and `types-grouped` at 1.054, 0.38 µs more, which passes the
D4 rule. With 12 threads at 1.05M triples, eager `order-by-full` read 0.677 of 005df56d,
13.1 ms against 8.9 ms, and both builds returned the same answers to all 30 queries.

Against 62d9cb53, the build that merged stage 4, the final build ran the 30 queries at
1.05M triples with 12 threads at a geometric mean of 0.959, with `order-by-full` at 0.643
and the queries under 0.1 ms 2% to 21% faster. Every other query read within 1.02 of it except
`distinct-join` at 1.036. Atlas put that 3.7% on the last change, against the build before
it, and all of it in the hash join's pair loop, `join_pairs`, whose code is the same in
both builds but starts 32 bytes later within its 64-byte line. Item 696 follows the
loop's sensitivity to its alignment. `star-lookup`, which item 681 followed, read 0.964 of
62d9cb53 and 0.996 of 005df56d with 12 threads in eager execution. With one thread the
build after review read 0.988 of 005df56d, so the charged query state no longer adds to
it, and the final build read 1.019 of 005df56d in two runs and 1.031 of the build after
review, 1.1 to 1.8 µs more. Atlas put all of that on the binary search of the index join's
galloping reads, `indexjoin::search`, which took 5,200 more cycles per request in the final
build. Its code is the same in both builds but starts 16 bytes into its 64-byte line
instead of at the line's start, so item 696 follows it with `join_pairs`.
The eager costs of 1% to 4% that stage 4 added to filters and aggregates, such as
`subquery-agg` at 1.040 of 005df56d at 10.5M triples and `contains` at 1.036 at 1.05M
triples, are unchanged, and item 682 holds them.

### Fixed cost of the hierarchical budget

An eager SELECT stores its charged projection metadata inline in the result backing.
The query admits the vectors and names before building them and admits the inline
storage as part of the result header before publishing it. Cursors still use one
shared allocation for their variables and the batches that can outlive them.
The common result header is larger for collected cursor, ASK and graph results.
The fast account also owns a single grant reference and skips returning credit when
it held none. Admission and pool granules are unchanged.

Item 884 remains open pending a valid wall-time comparison on `atlas`. Set
`BASE_BIN` and `TIP_BIN` to the server binaries built on the same Boat worker with
matched `nix develop` compilers, identical release settings and
`MBX_LEARNED_INCREMENTAL=false` from `0c425e86` and the item 884 tip. Set
`PEOPLE_100K_ZST` to the 1.05M-triple synthetic dataset. Run the comparison under the
timing kit's atlas locks. Arm C runs the base binary again as a same-binary control
for all four small queries. Use syscall spinners in every arm so that a wake-up can
run before the next scheduler tick.

```sh
python3 scripts/timing-kit.py stage --out .artifacts/884-atlas \
  --base "A=$BASE_BIN" --tip "B=$TIP_BIN" --tip "C=$BASE_BIN" --data "$PEOPLE_100K_ZST" \
  --only count-all,distinct-obj,predicate-counts,employee-docs,class-predicates,contains,two-hop-count \
  --controls class-predicates,contains,two-hop-count --execution auto --spin-loop syscall \
  --serve-args="--total-query-memory-mb 256" --rounds 18 --reps 200 --rotate \
  --rerun-rounds 24 --rerun-reps 300
printf 'STORE_OF_B=A\nSTORE_OF_C=A\n' >> .artifacts/884-atlas/run.env
python3 scripts/timing-kit.py preflight .artifacts/884-atlas
python3 scripts/timing-kit.py launch --host atlas --unit item-884-atlas .artifacts/884-atlas
```

After `DONE` reports exit 0, copy back `pass1` and retain its summaries and raw samples.
Check the `wall_ms` distributions in `pass1/results/samples.tsv` for all three arms on
the four small queries and reject a run with clusters at scheduler tick boundaries.
The kit's execution verdict can leave a wall-only cost unflagged. Check wall and
execution separately with paired per-round medians from `pass1/results/samples.tsv`,
bootstrapping whole three-round rotations of per-round microsecond differences
`1000 * (median(other) - median(A))` for absolute bounds and per-round ratios for the
relative criterion. The wall control's 95% interval must fit inside ±2 µs for every
query. The tip must meet the [Intel rule](PLAN.md#the-intel-rule) for both metrics,
and the wall interval's paired upper bound must also be below 2 µs added.

```sh
python3 - .artifacts/884-atlas/pass1 <<'PY'
import csv
import json
import random
import statistics as stats
import subprocess
import sys
from collections import defaultdict
from itertools import chain
from pathlib import Path

run = Path(sys.argv[1])
rounds = int(next(line.split("=", 1)[1] for line in (run / "run.env").read_text().splitlines()
                  if line.startswith("ROUNDS=")))
if rounds < 18 or rounds % 3:
    sys.exit("Use at least 18 rounds in complete three-round rotations")
queries = ("count-all", "distinct-obj", "predicate-counts", "employee-docs")
samples = defaultdict(lambda: defaultdict(lambda: defaultdict(list)))
with (run / "results" / "samples.tsv").open() as fh:
    for row in csv.DictReader(fh, delimiter="\t"):
        if row["query"] not in queries or row["exec_ms"] == "error":
            continue
        r = int(row["round"])
        for metric in ("wall_ms", "exec_ms"):
            samples[(row["query"], metric)][row["variant"]][r].append(float(row[metric]))

def boot_interval(values, seed):
    rng = random.Random(seed)
    rotations = [values[i:i+3] for i in range(0, len(values), 3)]
    boot = [stats.median(chain.from_iterable(rng.choices(rotations, k=len(rotations))))
            for _ in range(10_000)]
    low, *_, high = stats.quantiles(boot, n=40, method="inclusive")
    return low, stats.median(values), high

accepted = True
for pair, other, metrics in (("A-C", "C", ("wall_ms",)), ("A-B", "B", ("wall_ms", "exec_ms"))):
    summary = run / f"summary-{pair}.json"
    subprocess.run([sys.executable, "scripts/nsc-bench-summary.py", "--check",
                    str(summary), "first"], check=True)
    data = json.loads(summary.read_text())
    if data["errors"]:
        sys.exit(f"Failed timed requests in {pair}")
    for query in queries:
        for metric in metrics:
            cell = data["queries"][query][metric]
            ratios = cell["rounds"]
            if len(ratios) != rounds or min(cell["epochs_a"], cell["epochs_b"]) != 2 * rounds:
                sys.exit(f"Incomplete rotations for {pair} {query} {metric}")
            by_var = samples[(query, metric)]
            common = sorted(set(by_var["A"]) & set(by_var[other]))
            if len(common) != rounds:
                sys.exit(f"Incomplete sample rounds for {pair} {query} {metric}")
            diffs_us = [1000 * (stats.median(by_var[other][r]) - stats.median(by_var["A"][r]))
                        for r in common]
            low, estimate, high = boot_interval(ratios, 884)
            low_us, added_us, high_us = boot_interval(diffs_us, 884)
            ok = (high <= 1.02 or added_us <= 0.5) if pair == "A-B" else low_us > -2
            if metric == "wall_ms":
                ok = ok and high_us < 2
            accepted &= ok
            print(f"{pair} {query} {metric}: medians {cell['a']:.6f}, {cell['b']:.6f} ms, "
                  f"ratio {estimate:.4f} [{low:.4f}, {high:.4f}], "
                  f"added {added_us:+.2f} µs [{low_us:+.2f}, {high_us:+.2f}], "
                  f"{'pass' if ok else 'check with a repeat'}")
sys.exit(0 if accepted else 1)
PY
```

If the controls fail or the wall intervals leave a 2 µs effect unresolved, stage a
fresh run of all arms with 36 rounds and 300 requests per round, even when the kit
selects no rerun. Report a wall improvement only if its interval lies below zero.
The adjacent comparison measures the effect of the item 884 change on one shared store.
To estimate the remaining cost against the earlier query path, repeat the same procedure
using `001f2328`, the first parent of the query hierarchy merge `65686274`, for both A
and C, keeping `STORE_OF_C=A`, verifying store-format compatibility before setting
`STORE_OF_B=A`, and recording separate imports if the older format requires them.
Report that comparison separately because it includes changes between the two bases.

### Checked result output

The eager SELECT writers admit every decoded cell against the query's memory account
before they allocate it, and each writer rebuilds the front-coded vocabulary keys of its
cells in one buffer that it reuses for the whole answer. Each step of a rebuild compares
the rebuilt key's length with the buffer's capacity and admits the key before the buffer
grows. A refused growth leaves the buffer's allocation unchanged and returns the typed
refusal.

Roadmap item 763 measured what that check costs. On atlas at 100,000 people, with the
server pinned to one CPU and `perf stat` counting its user-space work per request over
three interleaved passes of matched release builds, the checked writer of c2b5d7cf ran
4.2% more instructions than the unchecked writer of 6fba19d6 on `optional-chain`, 2.7%
on `order-by-full` and 2.4% on `star-join`, and 7.90%, 4.05% and 3.86% more cycles. A
diagnostic build without the per-step check ran 1.4%, 0.0% and 0.8% fewer instructions
than 6fba19d6, so the check holds the instruction difference. The same build still ran
6.1%, 4.3% and 3.0% more cycles, which makes it unlikely that one reservation per
front-coded block would close the cycle difference, so that design was not built. Two
repairs were built and reverted. The first shared the check with the copy's own growth
check, and the second, 0ae09b0a, then copied the suffix into the checked room, which
brought the instructions within 0.8% of 6fba19d6. Neither lowered the cycles. The
second's final form, 0acba641, ran 1.3% more instructions than 6fba19d6 on
`order-by-full`, and its server execution of that query took 2.6% and 3.2% longer than
c2b5d7cf over the two runs, longer in every round. The checked writer also had 44% more
branch misses than 6fba19d6 on `optional-chain`, and none of the builds changed that
count, so item 763 looks there next.

### The streamed response cost

Roadmap item 534 measured where a streamed response spends the time that an eager one
does not, at 1.05M triples in native JSON. On the laptop, an Intel Core Ultra X7 358H
with the server on four performance cores, `perf stat` counted 472 thousand user-space
instructions per request of a streamed `count-all` against 311 thousand eager, 1.78
million against 1.19 million on `star-lookup`, and 263 thousand more on
`predicate-counts`. The user-space cycles of `count-all` were nearly equal, 410 thousand
streamed against 396 thousand eager per request, so the gap of the short queries is not
work in the server's code. On the large answers the streamed requests ran 6.13 billion
instructions against 3.54 billion on `export-500k`, 315 million against 183 million on
`optional-chain`, 68 million against 39 million on `star-join` and 1.41 billion against
976 million on `order-by-full`. LBR call stacks showed that the cursor writers' chain of
calls through the server's size-limited switch writer, which serde's escaping wrote
through a few bytes at a time and which was not inlined, cost more than the whole
difference on `export-500k` and about 90 thousand of the 160 thousand instructions on
`count-all`, while the streamed decoding was cheaper than the eager one.

On forge, with the server on CPUs 0 to 11 with 12 Rayon threads and curl on CPU 12, a
streamed answer took 0.13 to 0.23 ms more wall time than an eager one on the nine
queries under 1 ms, 1.29 to 1.47 times as long. The large answers took 7.53 ms against
5.00 ms on `star-join`, 22.4 against 20.1 ms on `optional-chain`, 49.4 against 40.2 ms
on `order-by-full` and 284 against 213 ms on `export-500k`. A phase trace of
the nine short queries in a diagnostic build placed their gap in the hand-off between
threads, with each phase measured per request. An eager request ran in place on the
worker that received it, and `count-all` had serialized its answer a median of 65 µs
after the handler started. A streamed request woke a blocking-pool thread to open its
cursor, woke the handler to receive the cursor and woke a second pool thread to write
the body. Each pool wake took either about 10 µs or about 80 µs, and their means across
the nine queries were 35 to 80 µs for the open's wake and 32 to 73 µs for the
producer's, with medians of 8 to 10 µs for the return to the handler. The end of the body, from the
last write to the producer's finish, took 49 to 85 µs on average. On `count-all` the
cursor's open took a median of 59 µs against the eager run's 45 µs, and the first batch
with its write 42 µs against the eager serialization's 8 µs. The same build answered a streamed request whole, without the
early commit at the first batch, which removes the channel, the end-of-body handshake and
the early commit together, and it was no faster. Its streamed `count-all` took 0.729 ms
against 0.546 ms with the early commit, and the other short queries took 0.58 to 0.89 ms
against 0.53 to 0.82 ms. The Python client bounded the large answers' wall times in the
laptop's runs, so their wall times come from curl.

Roadmap item 240 then gave the cursor writers of SELECT, native JSON, RDF and Jena graph
answers the 8 KiB `Buffered` writer of the eager writers through `write_buffered`, which
is compiled in `sparkles-core`, so a streamed answer reaches the server's writer in
pieces of that size. Each writer counts the rows whose bytes reached the server's writer,
and the access log and the metrics report that count for a streamed body that stopped
partway, as they do for an eager one. A streamed body that the result-size budget stops
after its first bytes now sends what it had buffered before it aborts, as an eager body
already did, so the rows reported are the rows a client that keeps reading receives.
That last send waits under the body's idle limit like any other, and when it fails the
bytes reported leave out the unsent chunk while the row count still included its rows,
so the count was exact when the client kept reading and otherwise within one chunk.
Roadmap item 243 made it exact in that case too. The writer records where each row
ends, and the transport counts a row once a send that enqueued has handed over the bytes
up to its end.

For Rust callers, `write_with_completion` runs a callback after the output
buffer has drained and credited the accepted rows. The callback can check
pending cancellation on a `LimitedWriter`, including one it reaches through
a known wrapper, without writing or flushing the sink. A failed serialization
keeps its original error and skips completion. A completion failure is
recorded on a cursor before the writer can return successful statistics.

Matched release builds of 8baa5686 and a016b7bd, both from wally, were counted on the
laptop with the server on four performance cores, over two passes of 200 requests for
the short queries and 10 for the large ones, at 1.05M triples. The eager requests ran
the same instructions within 0.5% in native JSON and in SPARQL JSON. In native JSON the
streamed requests ran 0.46 times the base's instructions on `export-500k`, 2.82 billion
against 6.13 billion, and below the eager requests' 3.54 billion. They ran 0.50 times on
`optional-chain`, 158 million against 315 million with 182 million eager, 0.60 times on
`star-join`, 40.4 million against 67.6 million with 39.6 million eager, and 0.69 times
on `order-by-full`, 970 million against 1.41 billion with 976 million eager. The short
queries kept part of their gap, as the hand-off is outside the writer. A streamed
`count-all` ran 391 thousand instructions against 453 thousand with 307 thousand eager,
`predicate-counts` 510 thousand against 670 thousand with 422 thousand eager, and
`star-lookup` 1.33 million against 1.76 million with 1.19 million eager. In SPARQL JSON
the streamed requests ran 0.44 to 0.50 times the base's instructions on `export-500k`,
`optional-chain` and `star-join`, 7.64 billion against 7.69 billion eager on
`export-500k` and 78.8 million against 73.3 million eager on `star-join`. The laptop ran
other builds at the time, so only the instruction counts are reported here.

The wall times of the same two builds come from atlas, an Intel host with forge's
hardware. The server ran on CPUs 0 to 11 and the client on CPU 12, at 1.05M triples in
native JSON, in rounds of A,B,B,A with 10 requests per variant in each position, and each
cell ran once with the base first and once with the base second. The ratios below are
the geometric means of the two orders, over the 31 queries of the matrix with eight
rounds in each order, from runs that had the host to themselves. With 12 Rayon threads a
streamed request took 0.42 times the base's wall time on `export-500k`, 0.56 times on
`optional-chain`, 0.63 times on `order-by-full` and 0.77 times on `star-join`. In the run
with the base second, these streamed requests took 121 ms against 289 ms, 10.4 against
18.4 ms, 31.7 against 50.6 ms and 4.55 against 5.88 ms, and they finished before the
eager requests of the same build, which took 220, 14.1, 42.7 and 4.89 ms. With one thread
the four queries took 0.53, 0.65, 0.78 and 0.73 times the base's wall time, and the
streamed `star-join` and `order-by-full` took about as long as the eager ones. The
geometric mean of the streamed ratios over the 31 queries was 0.935 with 12 threads. The
eager requests of the two builds took 0.984 to 1.023 times each other's wall time with 12
threads, in the order with the base second, since the other order's eager run may have
overlapped a build, and 0.983 to 1.018 with one thread over both orders.

The seven queries under 1 ms ran again on atlas with 12 rounds in each order and 12
threads, as a pinned check of the streamed `count-all` and `distinct-obj`, which one
order of the matrix had shown about 7% slower. The streamed wall-time ratios of the two
builds were 0.96 to 1.02 on all seven queries, and the 95% interval of every ratio
included 1.0, as did the eager control's. A streamed answer still took 0.21 to 0.29 ms
more wall time than an eager one on those queries, against 0.23 to 0.31 ms in the base,
because the hand-off between threads that the phase trace measured lies outside the
writer.

Both thread-handoff changes of item 534 have now landed on top of item 828's batch-source
writers. Pool-routed cursors, both explicit `execution=streaming` and pool-routed
`execution=auto` or eager-trial requests, open and serialize in one blocking-pool task
through `stream::open_and_serialize_controlled`, removing the return wake to the handler
and the second pool wake while keeping open-phase error responses, concurrency permit
lifetimes, `StreamControl`, and the execution-to-serialization timing split unchanged.
When a request targets the current snapshot and an in-place runtime is available, eligible
`execution=auto` requests open and drain cursors in place through resumable `Send`
`CursorWriter` sessions in `sparkles-core`. Eligibility covers texts whose quick history
retains automatic execution and unseen first runs that carry a timeout, arrive after a
server-idle interval (`FIRST_RUN_QUIET`) and pass a conservative text scan rejecting any
case-insensitive `service` substring or `\u` or `\U` Unicode escape prefix.
A cursor whose encoded output finishes within `STREAM_AFTER` returns a whole response with
`Content-Length` and zero thread hops, and a cursor that would cross `STREAM_AFTER` pauses
before buffering an oversized row, quad or footer and hands off the cursor, encoder state
and buffered `SwitchWriter` prefix to a blocking streaming task whose initial chunk stays
within `STREAM_AFTER + 1` bytes and whose subsequent chunks drain in `CHUNK`-sized
backpressured frames. Explicit `execution=streaming` keeps the blocking-pool route with
early first-batch or 64 KiB commit and immediate cancellation on client disconnect, and
queries that call graph algorithms are never recorded as quick on either route. Core
session parity tests and HTTP router integration tests cover route selection, format parity
across all SELECT, ASK and graph formats (including JSON-LD, with decoded statement
equality for Jena binary formats and native JSON), open errors, memory, result-byte and
row budgets on both sides of commit, mid-stream timeouts, and client-disconnect
cancellation. The quiet `atlas` timing comparison against pre-handoff `5369883f` is
pending.

### One writer per format and exact row accounting (items 828 and 243)

Roadmap items 828 and 243 shipped one columnar writer per format over a shared `Source`
trait for SELECT, ASK and graph answers, replacing the separate eager and cursor writers
and removing the public `_counted` writer functions. The engine now writes SPARQL JSON,
XML, CSV and TSV SELECT results with its own encoders in place of the sparesults writers,
keeping the same bytes and delivering each row whole under the query's memory and
result-size budgets. SPARQL Results Thrift moved from `sparkles-server` into
`sparkles-core` so a streamed Thrift SELECT writes from a cursor in the same bytes as an
eager answer and no longer returns `501`. The result buffer records row ends in a
`RowLedger` rather than checking the row count on every write, and a streamed HTTP
response credits a row once the transport has handed over the bytes up to its end, so a
failed final send excludes the unsent chunk's rows. The in-place cursor hand-off remains
under item 534, graph response shape unification remains under item 1111, and the
per-row cost of whole-row output remains under item 1195.

Initial quiet `atlas` timing of `fd47b7ae` against `726563ad` confirmed a 1.1919×
regression (+43.73 ms, 16/16 slower) on 12-thread native streaming `optional-chain`
entirely inside serialization. Commit `a6814599` resolved that backpressure cliff by
changing `Shared::timer` in `http/stream/lifetime.rs` so it no longer wakes on every
64 KiB chunk or `Err(Full)` and instead notifies only when `idle_at` starts, moves
earlier or enters a terminal or aborted state, and by growing `SwitchWriter`'s bounded
stream channel from 4 to 16 entries (`CHANNEL_CAP = 16`). A row-storage charge hoist
also included in `a6814599` was reverted afterward so `CursorSolutions::drive` releases
row-storage credit between batches instead of holding it across subsequent pulls and
decode-ahead checks. Each post-switch channel entry holds at most 64 KiB, while a
threshold-driven switch queues its initial buffer as one entry (up to `STREAM_AFTER` + 1
byte before the remaining 15 chunks). Raising the queue from 4 to 16 entries trades up to
768 KiB more queued payload per active streamed HTTP response, released when the stream
ends or aborts, so decoded cursor batches can enqueue without stalling the blocking
producer inside `rayon::in_place_scope` when their encoded bytes fit in the available
queue space.

Quiet `atlas` order-corrected ABBA measurements with matched `wally` release builds
(8 rounds of 10 requests in both orders at 10.5M triples, recorded with full provenance
in the [engine benchmark report](../../benchmarks/engine/RESULTS.md#unified-result-writers-and-exact-row-accounting))
confirmed the gains across both streaming and format encoders with identical answer
fingerprints. In the 12-thread native streaming confirmation cell comparing `fd47b7ae`
with `a6814599`, all four workload queries improved with 0/16 rounds slower: `export-500k`
ran at 0.3710× (250.94 against 676.68 ms), `optional-chain` at 0.9420× (211.72 against
224.86 ms), `order-by-full` at 0.7782× (794.14 against 1,018.84 ms) and `star-join` at
0.8508× (53.87 against 63.35 ms). Across the full 34-pair `atlas` matrix comparing
`fd47b7ae` with `726563ad`, 1-thread `optional-chain` ran at 0.4381× streamed and 0.5658×
eager in SPARQL JSON, 0.5691× streamed and 0.7154× eager in XML, 0.9078× streamed in TSV
and 0.8979× streamed in CSV; on the `literals` dataset, `long-literal` ran at 0.4234×
streamed and 0.4447× eager in SPARQL JSON, 0.7580× eager and 0.8632× streamed in CSV and
0.8381× streamed in TSV. The streamed Thrift probe passed with matching HTTP chunked
framing and identical body fingerprints across base eager, tip eager and tip streaming.
Three confirmed small costs on quiet `atlas` (`fd47b7ae..726563ad`) that passed the gate
are tracked in roadmap item 1195: `workload-native-eager-1t` `order-by-full` at 1.0029×
(+2.43 ms, 13/16), `literal-tsv-eager-1t` `escape-straddle` at 1.0017× (+3.76 µs, 14/16)
and `short-native-streaming-1t` `predicate-counts` at 1.0010× (+2.00 µs, 23/24). Non-Intel
gates passed on `726563ad`: `mise run test:w3c:streaming` passed 1,274 of 1,274 tests
with zero regressions and 24 unchanged fallbacks, and `py:test`, `jvm:test`, `node:test`
and `bench:bindings` across all five Sparkles engines passed with zero answer
discrepancies.

### Strict decoding across non-SELECT callers

Item 1308 Wave 2 closed the eager decode bypass in `Ctx` by removing `envelope_refuses`
and unifying `DecodeEnvelope` across both accounting modes, which lowered the
bounded-reads check from seven to six sites. Eager and streaming execution now enforce
the same memory budget and live request controls across expression combinators, batch
loops, executor operators, ORDER BY and top-k heaps, external sort frame reloads,
streaming and eager group accumulators, DESCRIBE, SERVICE, text highlighting, dataset
restriction, incremental passes, SHACL embedded query results and GeoSPARQL execution.
Optional FILTER selectivity sampling skips impure expressions and uses a disposable
trial on a snapshot cache miss. The trial admits copies of query setup before cloning
them, including the local vocabulary's backing buckets, and releases its decode,
cache and local-term state before execution starts.
Triple keys validate their framing and nesting depth before recursive decoding, and
vocabulary read locks are released before memory admission or geometry parsing.
GeoJSON fallback and recheck paths admit the JSON value tree before parsing. The parser
reads object keys as ordinary strings and bounds the unknown-type diagnostic. The
nearest-neighbour fallback keeps parsed entry credit through distance measurement.

### Forge confirmation for item 1088

Use release binaries built on the Boat worker with the same compiler and profile and
`MBX_LEARNED_INCREMENTAL=false`. Set `BASE_BIN` to the binary for `0c425e86`,
`TIP_BIN` to the candidate, and `PRE1069_BIN` to the binary for `c0948014`. Run the
following from the candidate checkout on forge in a named terminal. It reuses the
historical queries and CPU placement, compares the candidate against its base and the
pre-1069 baseline, and records a same-binary control for each thread count.

At both one and 12 threads, require valid summaries, matching answers and zero timed
request errors in all three comparisons. Judge server execution time against the
corresponding query's same-binary control. The control's largest deviation from parity
across the pooled and per-round ratios must be at most 3%. The candidate must stay
within that noise bound against its base on all three queries and against the pre-1069
baseline on `str-pair-colleague`. Reject any of those comparisons when every round is
slower and the pooled median adds more than 0.5 µs, even if it falls within the control's
noise bound. Retain the summaries for both thread counts when closing item 1088.

```bash
export BASE_BIN TIP_BIN PRE1069_BIN
flock "$HOME/lanes/.heavy.lock" bash -s <<'SH'
set -euo pipefail
: "${BASE_BIN:?}" "${TIP_BIN:?}" "${PRE1069_BIN:?}"
repo=$PWD
runs="$repo/.artifacts/bench-floor-1088/$(date -u +%Y%m%dT%H%M%SZ)"
queries="$repo/docs/benchmarks/synthetic/runs/2026-11-19-floor-table-ab-forge-3a8b7adb/g1-streaming/queries"
mkdir -p "$runs"
python3 scripts/gen-data.py 100000 | zstd -q -o "$runs/data.nt.zst"
for threads in 1 12; do
    server_cpu=2 client_cpu=4 spin=1
    if [ "$threads" -eq 12 ]; then
        server_cpu=0-11 client_cpu=12 spin=0
    fi
    for comparison in control candidate recovery; do
        a=$BASE_BIN b=$TIP_BIN
        case "$comparison" in
            control) b=$BASE_BIN ;;
            recovery) a=$PRE1069_BIN ;;
        esac
        out="$runs/$threads-$comparison"
        mkdir -p "$out/queries"
        for query in str-pair-colleague str-pair-names str-pair-lt; do
            cp "$queries/$query.rq" "$out/queries/"
        done
        install -m 755 "$a" "$out/bin-A"
        install -m 755 "$b" "$out/bin-B"
        {
            printf 'host: %s\nA: %s\nB: %s\n' "$(hostname)" "$a" "$b"
            sha256sum "$a" "$b" "$runs/data.nt.zst"
            readelf -p .comment "$a" "$b"
        } >"$out/manifest.txt"
        cp "$runs/data.nt.zst" "$out/"
        cp scripts/nsc-bench-remote.sh "$out/"
        cat >"$out/run.env" <<EOF
ROUNDS=12
REPS=16
WARMUP=3
ORDERS="A B B A;B A A B"
BINARY_OF_A=A
BINARY_OF_B=B
EXPECTED_ANSWER_CHANGE=0
EXPECTED_ANSWER_CHANGE_REASON=""
RAYON_THREADS=$threads
SERVER_CPU=$server_cpu
CLIENT_CPU=$client_cpu
SPIN=$spin
EXECUTION=streaming
PORT=31941
EOF
        env -u BINARY_OF_A -u BINARY_OF_B -u EXPECTED_ANSWER_CHANGE -u EXPECTED_ANSWER_CHANGE_REASON \
            bash "$out/nsc-bench-remote.sh"
        python3 scripts/nsc-bench-summary.py "$out" >"$out/summary.md"
    done
done
python3 - "$runs" <<'PY'
import json
import subprocess
import sys
from pathlib import Path

runs = Path(sys.argv[1])
queries = ("str-pair-colleague", "str-pair-names", "str-pair-lt")
failures = []
for threads in (1, 12):
    comparisons = {}
    for comparison in ("control", "candidate", "recovery"):
        path = runs / f"{threads}-{comparison}" / "summary.json"
        result = subprocess.run(
            [sys.executable, "scripts/nsc-bench-summary.py", "--check", str(path), "first"]
        )
        summary = json.loads(path.read_text())
        if summary["errors"]:
            failures.append(f"{threads}t {comparison}: timed request errors {summary['errors']}")
        for query in queries:
            answers = summary.get("answers", {}).get(query)
            if answers != "same":
                failures.append(f"{threads}t {comparison} {query}: answers {answers!r} != 'same'")
        if result.returncode:
            failures.append(f"{threads}t {comparison}: invalid summary")
            continue
        comparisons[comparison] = summary["queries"]
    if len(comparisons) != 3:
        continue

    for query in queries:
        control = comparisons["control"][query]["exec_ms"]
        ratios = [control["ratio"], *control["rounds"]]
        bound = max(max(r, 1 / r) for r in ratios)
        if len(control["rounds"]) != 12 or bound > 1.03:
            failures.append(f"{threads}t {query}: incomplete or noisy control ({bound:.4f}x)")
            continue

        for comparison in ("candidate", "recovery") if query == queries[0] else ("candidate",):
            stats = comparisons[comparison][query]["exec_ms"]
            ratio = stats["ratio"]
            added_us = (stats["b"] - stats["a"]) * 1000
            print(f"{threads}t {comparison} {query}: {ratio:.4f}x, control limit {bound:.4f}x")
            if len(stats["rounds"]) != 12:
                failures.append(f"{threads}t {comparison} {query}: incomplete timing rounds")
            elif ratio > bound or (added_us > 0.5 and all(r > 1 for r in stats["rounds"])):
                failures.append(f"{threads}t {comparison} {query}: regression ({ratio:.4f}x)")
if failures:
    sys.exit("\n".join(failures))
PY
SH
```

### Cursor BIND reuse admission (item 293)

Item 293 settles the admission policy of the cursor's exact-id BIND cache in
`cursor/bind.rs`, which the query-scoped expression memo of stage 5 takes over. A first
batch that does not admit the map seeds a sketch of one input key in sixteen, chosen by a
hash of the id, and later batches keep feeding it until it holds as many keys as a full
map. The exact distinct count of the expression cache's pass over each batch is a second
signal, because hot keys can all fall outside the sample. Dense inline integers and the
snapshot hint admit at once as before, and so does a batch whose exact count repeats half
its rows. An admitted map evaluates its misses once per distinct key, holds a pending
entry for a missed key until then, and is released when it answers fewer than one row in
sixteen. The sketch, the map and each batch's scratch are charged to the query's memory
budget, a refused charge falls back to ordinary evaluation, and every loop checks
cancellation.

Weak repetition, from one repeated row in eight up to one in two, admits a
parallel-capable expression on more than one worker only when it pays for the map. The
guard keeps the last two timed fallback evaluations of at least 1,024 rows in one size
bucket and estimates the saving from the lower of their rates. A saving of at least
100 ns a row admits on one window, at least 50 ns needs two consecutive windows, and less
declines. Sustained split losses in the bucket count as a pass, because contended
parallel evaluation is then the cost that the map saves. A single worker and expressions
that cannot run in parallel keep the plain one-in-eight rule.

Map misses, per-batch distinct columns and the ordinary fallback evaluation share one
split controller with two size buckets, 768 to 2,047 and 2,048 to 16,384 evaluated rows.
Two consecutive splits that run at least 20% slower than their timed sample projects put
the bucket in a serial cooldown of eight eligible evaluations, after which one retry
decides whether the pool is used again. The controller survives a declined map, so a
cheap expression that contends on the pool stays serial without disabling parallelism
for the rest of the query. Map misses that are all stored IRIs under a direct
`STR(input)` intern their strings and contend sooner, so they keep a separate controller
in which one split that does not pay starts a cooldown of 16 evaluations. Tables larger
than 16,384 rows keep the executor's own parallel threshold.

The policy was chosen on forge, an Intel i5-13500, at 13,320,192 triples from
`scripts/gen-bind-reuse.py`, with 17 query threads, streaming execution and no result
cache. Each of the A,B,B,A and B,A,A,B orders ran six rounds of four fresh processes with
eight timed requests a query, and every answer matched. The prototype `b71512f1`, which
has the policy above, ran against its own build with the 100 ns rule and the IRI cooldown
turned off. It also ran against the earlier lane state `224caadc`, which kept map misses
serial after their first unpaid split and admitted at one in eight without a cost guard,
and against the base `fd47b7ae`. Against the switched-off build it took 0.779 of the time
on joined SHA256 with random repeats at 65,536 rows, 0.838 and 0.905 on moderate IRI
repetition and 0.927 to 0.930 on SHA256 with a stride of 8. Against `224caadc` it took
0.54 to 0.59 on unique and low-repeat IRIs and 0.74 to 0.79 on unique and low-repeat
SHA256. It removed that state's 1.21 to 1.24 times regressions on dates and strings with a
stride of 8 and its 1.105 on joined strings with random repeats. It gave up 7.9% to 12.1%
against `224caadc` on SHA256 with a stride of 8, whose map misses split on every batch,
8.0% and 5.3% on joined SHA256 and strings with global repeats and 5.5% on strings with a
stride of 8 at 262,144 rows.
Against the base, the three 65,536-row repairs measured 1.018 on dates with an interval
that reaches 1.0, 1.014 on strings and 1.037 on joined strings with random repeats, which
add 0.15 ms and 0.25 ms. Item 1319 holds those two costs. The matrix has no paired
comparison against the base on the cheap inputs, the 2,048-row inputs or the small eager
and dense-string queries, so item 293 stays open until the landed build is confirmed
against its base.

The rejected arms are recorded in PERFORMANCE-EXPERIMENTS. A diagnostic arm in `cd5561af`
gave the map misses of `SHA256(STR(input))` a separate first-unpaid cooldown, to test
whether split scheduling alone causes the stride-8 SHA256 cost. It was never measured and
is not part of the policy. The shortcut for the 50M slice and reuse beyond one query
remain deferred.

The confirmation builds the landed tip and its base `17674a34` as release binaries on one
build host with the same compiler and profile. The commands below run on forge from the
candidate checkout in a named terminal with `BASE_BIN` and `TIP_BIN` set. Each pair
runs beside a same-binary control. The streamed pairs cover the 166 queries at all
threads and the cheap and 2,048-row queries at one thread, and the eager pair covers the
2,048-row and string queries at all threads, since the eager executor shares the batch
evaluation that the split timing changes. The build passes when every answer matches and no query is
10% or more slower after the flagged rerun. A ratio whose interval stays above 1.0 and
adds more than 0.5 µs is a real cost that gets its own line in item 1319.

```bash
export BASE_BIN TIP_BIN
flock "$HOME/lanes/.heavy.lock" bash -s <<'SH'
set -euo pipefail
: "${BASE_BIN:?}" "${TIP_BIN:?}"
repo=$PWD
runs="$repo/.artifacts/bench-bind-reuse-293/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$runs"
python3 scripts/gen-bind-reuse.py data | zstd -q -o "$runs/data.nt.zst"
python3 scripts/gen-bind-reuse.py queries "$runs/queries"
status=0
for arm in all-control all-candidate one-control one-candidate eager-control eager-candidate; do
    a=$BASE_BIN b=$TIP_BIN threads=default server_cpu=0-10,12-19 client_cpu=11 spin=0
    execution=streaming
    case "$arm" in
        *-control) b=$BASE_BIN ;;
    esac
    out="$runs/$arm"
    mkdir -p "$out/queries"
    case "$arm" in
        all-*) cp "$runs"/queries/*.rq "$out/queries/" ;;
        one-*)
            threads=1 server_cpu=2 client_cpu=4 spin=1
            cp "$runs"/queries/*-2048.rq "$runs"/queries/*-low-repeat-*.rq \
                "$runs"/queries/*-stride-16-*.rq "$runs"/queries/*-unique-*.rq "$out/queries/"
            ;;
        eager-*)
            execution=
            cp "$runs"/queries/*-2048.rq "$runs"/queries/str-*.rq "$runs"/queries/join-str-*.rq \
                "$out/queries/"
            ;;
    esac
    install -m 755 "$a" "$out/bin-A"
    install -m 755 "$b" "$out/bin-B"
    {
        printf 'host: %s\nA: %s\nB: %s\n' "$(hostname)" "$a" "$b"
        sha256sum "$a" "$b" "$runs/data.nt.zst"
        readelf -p .comment "$a" "$b"
    } >"$out/manifest.txt"
    cp "$runs/data.nt.zst" "$out/"
    cp scripts/nsc-bench-remote.sh "$out/"
    cat >"$out/run.env" <<ENV
ROUNDS=6
REPS=8
WARMUP=3
ORDERS="A B B A;B A A B"
BINARY_OF_A=A
BINARY_OF_B=B
EXPECTED_ANSWER_CHANGE=0
EXPECTED_ANSWER_CHANGE_REASON=""
RAYON_THREADS=$threads
SERVER_CPU=$server_cpu
CLIENT_CPU=$client_cpu
SPIN=$spin
EXECUTION=$execution
PORT=31943
ENV
    env -u BINARY_OF_A -u BINARY_OF_B -u EXPECTED_ANSWER_CHANGE -u EXPECTED_ANSWER_CHANGE_REASON \
        bash "$out/nsc-bench-remote.sh"
    python3 scripts/nsc-bench-summary.py "$out" >"$out/summary.md"
    python3 scripts/nsc-bench-summary.py --check "$out/summary.json" first || status=1
done
exit "$status"
SH
```
