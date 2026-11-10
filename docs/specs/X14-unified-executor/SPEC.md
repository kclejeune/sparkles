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

**Delete the off paths of replacement-style optimization toggles.** Remove the superseded implementations behind about 15 replacement toggles and the toggles themselves, and keep the algorithm toggles for diagnosis. Optimizations has 44 boolean fields, and every kernel change must keep each off path correct. Roadmap item 476 holds this phase, and item 512 holds the test-only evaluator that follows the SPARQL algebra literally, after which the off paths that only tests reach can go.

**Shared format registry and small helpers.** Add one syntax registry for format names, extensions and media types, one INFERRED_GRAPH constant, one `parse_duration` and one durable atomic-write helper, and fix the Turtle string escaping that differs between `mapping/rdf.rs` and `mapping/draft.rs`. About ten format tables, nine INFERRED_GRAPH constants and ten atomic-write helpers exist. The unescaped draft writer could produce invalid Turtle. Roadmap item 477 holds this phase.

**Implementation-selecting switches made test-only.** Make the EnvSwitch statics, implementation-only StoreOptions booleans and measurement switches test-only or remove them after X07 closes. No measured difference has been recorded for I/O hints on bulk loads. Roadmap item 478 holds this phase, and item 513 makes the cold-read switches test-only once item 4 has measured them.

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

**One columnar writer per format over batch sources.** Write each result format once over a batch source, with an eager QueryResult as a one-batch source, vectorized term decoding, per-column writes into prebuilt buffers, and the Thrift writer moved into core. Two serializer stacks exist, Thrift forces eager execution, cursor writers write a few bytes at a time (item 240), and large answers such as SP2Bench q4 at 1.96 GB need a raised limit. It depends on items 91, 240, 243 and 534.

### Stage 6: the executor merge

**Single morsel-driven executor with eager as a consumption policy.** Replace the eager columnar executor and the pull-cursor executor with one morsel-driven pull engine in which every operator serves demand over column batches, `collect()` requests unbounded demand with whole-input morsels and Rayon across them, and exchange operators, partitioned hash builds and parallel merge joins give pipeline parallelism. The remaining fallback kinds get native leaf cursors, so the 840-line admission layer, `State::Fallback` and the eager memory retry go. Two complete executors exist (about 8,800 and 11,650 non-test lines) with 17 entry-point rules. Eager's remaining wins at 12 threads come from whole-table parallelism inside operators. It depends on items 7, 240, 243, 293, 534 and 512. Item 93 has shipped. Once the eager executor is gone the differential tests have no second executor, so item 512's reference evaluator becomes their oracle, with the answer fingerprints of the forge matrix and the W3C suites.

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
  the streamed response cost that stage 6 relies on.
* Item 512's test-only reference evaluator is the oracle of the differential tests before
  the eager executor is deleted.
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
* The guards are `scripts/bench.sh` and `mise run bench:nsc` for queries, with forge runs
  of BSBM Explore and BI, SP²Bench `q4`, `q6` and `q12b`, WatDiv, LUBM and DBpedia at 50M,
  the memory benchmarks in `docs/benchmarks/memory`, `query_cursor.rs`,
  `plan_fidelity_tests.rs` and the W3C suites. Bindings use `bench:bindings`.
* Stage 4 is measured on an Intel host, since its effects on short queries are small.
* Query budgets, cancellation on disconnect and the results of every query stay as they
  are. A measured slowdown that a gate accepts gets its own roadmap item.

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
479 of stage 2. Stage 4's accounting model and mode-neutral kernels have shipped, and the
hierarchical budget of item 675 and the decoding twins of item 678 remain.
[Item 453](../../ROADMAP.md) tracks the rest of the work.

Native SELECT output and standard solution responses of at most 65,536 rows admit and
decode a complete row before serializing its cells. Row storage and decoded terms keep
query credit until their payloads are freed. A required decode refusal returns its
original local or shared-pool memory error, preserves earlier complete-row counts and
stops before successful completion metadata. Native output holds a whole decoded row
rather than one cell at a time, and its additional live storage counts against the
budget. These remain estimated heap charges rather than process RSS limits.

Hierarchical ownership of public results, native buffers, reasoner work and views, and
strict decoding throughout their callers remain part of item 675.

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
`fused_star_costs` are what the cost calibration harness compares. `grow_input` and
`decode_ahead` are the memory and latency tradeoffs that the cursor tests measure against
their off paths, and the sequential writer stays as the path of a single Rayon worker.
The prefetch and decoding switches of X07 wait for item 4 under item 478. The switches
that shipped with their measurements in the current release, `probed_optional` and
`bound_order_keys`, keep their off paths as the baseline of the forge checks.
`gallop_index_join` chooses between the galloping reader and the clustered reader, which
are different read paths.

The generic operators of `Optimizations::NONE` remain the oracle of the differential
tests. The test-only evaluator that follows the SPARQL algebra literally, after which
the remaining off paths that only tests reach can go, is item 512.

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

The cold-read switches `SPARKLES_IO_HINTS`, `SPARKLES_SPARSE_VOCAB` and
`SPARKLES_READ_WINDOW_KB`, and the prefetch and decoding switches of `Optimizations`,
remain until item 4 has measured them, and item 513 makes them test-only then.
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
ledger. A context that is never shared, such as one of an incremental view, has no owner,
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
evaluation reads runs for inputs of more than 16,384 rows in both modes, and smaller
inputs read their values through the value cache. Each decoded column's slots are
charged before it is decoded, with the distinct values it decodes on the way, and its
values after. A column whose charge does not fit gives that charge back and is left to the
rows. The comparison of two string columns through cached lexicographic floors stays off
for cursor batches, because the floors reconstruct strings without reserving the
decoding, which only an eager query may do.

A unit test, `the_accounting_mode_is_read_in_a_bounded_number_of_places`, counts the
calls of `streams()`, formerly `is_cursor()`, outside tests and fails above its bound,
which each step lowered. It was 26 after the regular expressions, 17 after per-value
reuse, 7 after the sort keys and is 6 after the decoded columns. Three are the planning and
routing choices that stage 6 removes, in `plan.rs`, `unionpush.rs` and `cursor.rs`. The
fourth accumulates an EXISTS key set across a cursor's batches, which an eager table
builds in one batch. The fifth lets a cursor's decoding envelope refuse on the ledger,
because eager callers of `term` and `value` cannot report a refusal and decode without
the envelope instead. The sixth keeps cursors from the string floors above, until those
reconstruct with the decoding admitted. `CursorMemory` is gone, and a context in cursor accounting differs
from an eager one only by its `streams` flag, its ledger retention and its deadline. The
result cache and sampled filters stay off for cursors, since they are planning choices
that stage 6 makes independent of the mode. Item 678 holds the two decoding twins
that remain, the string floors and the dictionary pre-decode of a cursor batch, which
`decode_for` does not yet serve.

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
