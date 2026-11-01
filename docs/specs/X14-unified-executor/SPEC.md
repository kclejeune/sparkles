# X14: Unified executor

> **Status:** in progress.
>
> **Roadmap:** [item 453](../../ROADMAP.md) holds this work. Each candidate phase below gets
> its own numbered roadmap item when it is ready to start, and the items already numbered
> are named in their phase.

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

**Delete the off paths of replacement-style optimization toggles.** Remove the superseded implementations behind about 15 replacement toggles and the toggles themselves, keep the algorithm toggles for diagnosis, and move generic reference evaluation into a test-only spec-literal evaluator. Optimizations has 44 boolean fields, and every kernel change must keep each off path correct. Roadmap item 476 holds this phase.

**Shared format registry and small helpers.** Add one syntax registry for format names, extensions and media types, one INFERRED_GRAPH constant, one `parse_duration` and one durable atomic-write helper, and fix the Turtle string escaping that differs between `mapping/rdf.rs` and `mapping/draft.rs`. About ten format tables, nine INFERRED_GRAPH constants and ten atomic-write helpers exist. The unescaped draft writer could produce invalid Turtle. Roadmap item 477 holds this phase.

**Implementation-selecting switches made test-only.** Make the EnvSwitch statics, implementation-only StoreOptions booleans and measurement switches test-only or remove them after X07 closes. No measured difference has been recorded for I/O hints on bulk loads. It depends on item 4. Roadmap item 478 holds this phase.

### Stage 3: policy and plumbing

The short-term half of the shared commit publish tail belongs to this stage as item 484, and [X16](../X16-storage-consolidation/SPEC.md#candidate-phases) holds its design.

**One execution-policy function for every entry point.** Move the execution choice into one core function keyed by consumer properties (exact total, plan, inside transaction, format), so HTTP, CLI, MCP, GraphQL and the bindings state what they consume. HTTP, CLI, FFI, Node, Python, MCP and GraphQL each encode their own policy. It depends on items 7 and 240. Roadmap item 480 holds this phase.

**Typed reasoning mode and one inference view.** Add ReasoningMode { Off, Materialized, Demand(profile), Auto } and one InferenceView on Dataset that produces QueryOptions, the incremental inference input and the explanation source. Inferred-data selection is decided in about eight places with repeated string matching. It is the natural home of item 19's auto. Roadmap item 481 holds this phase.

**Library QueryRequest builder and server CallerContext.** Replace about 16 copies of the per-caller QueryOptions recipe and the binding copies with a library QueryRequest builder with Budgets and `with_reasoning`, plus a server `CallerContext::for_caller`. The GraphRAG builder builds options from the dataset defaults plus restrict and carries only the cancel flag, so server timeouts and limits plausibly do not apply to it. That would be a correctness gap. Roadmap item 482 holds this phase.

**Shared RRF fusion and text hits in the library.** Add a library `fuse_rrf(lists, k, weights)` and `TextIndexes::hits()`, used by `spk:hybridSearch`, MCP search and the memory layer. `text:query` SPARQL is generated three times and reciprocal rank fusion is implemented five times in the MCP memory layer. Roadmap item 483 holds this phase.

### Stage 4: one accounting model

**One memory-accounting model for all query state.** Charge every retained structure, keep the eager fast account's speed with chunked reservations, make the value cache query-scoped and charged, and pick one regex engine per measured case. `Ctx::is_cursor()` is tested at 47 non-test sites and about 1,300 lines of `cursor_*` twins exist only because the regimes differ. Uncharged eager state is a budget hole. It is the keystone of the executor merge. It depends on items 7, 40 and 88.

**Hierarchical budget across reasoner, views and queries.** Link the reasoner Budget, the incr per-view memory and the query pool into one hierarchical budget. A reasoning run, a view and a query can each stay within their limits and overshoot together. It depends on item 192.

### Stage 5: shared kernels

**One join-kernel library over column batches.** Keep merge, hash, keyed, index and ripple joins as distinct algorithms with one implementation each, where an eager join is one probe of the whole input and spill partitioning wraps the same kernel. About 1,000 eager and 4,000 cursor lines implement the same join families, and the copies have diverged on OPTIONAL counting. It depends on items 7 and 93.

**One grouping operator with an accumulator trait.** Use one hash and sorted-streaming grouping operator with spill over an accumulator trait (init, `add_batch`, merge, finish) shared by FOLD, registered aggregates and incremental views with a retraction method. Grouping exists four times (`exec.rs`, `cursor/group.rs`, `incr/ops/group.rs` and the count plans). It also delivers item 418's retraction semantics. It depends on items 7, 93 and 418.

**One frontier-expansion kernel for paths and path search.** Share one expand(frontier, direction, lanes, graph filter, exclusions, dedup) kernel with the seek-or-sweep and dense-block policies across property paths, ARQ path ranges and path search. The sweep ratio is defined in both the SPARQL executor and path search, and only the executor applies the X07 cold-read rule. The fixed-point operator of item 417 would add a third copy. It depends on items 8, 51 and 417.

**One query-scoped expression memo.** Replace exprcache, cursor BIND reuse and the EXISTS memo with one charged memo keyed by expression and input value with one admission policy fed by observed repetition. Three memos each have their own admission and charging. It depends on item 293.

**One columnar writer per format over batch sources.** Write each result format once over a batch source, with an eager QueryResult as a one-batch source, vectorized term decoding, per-column writes into prebuilt buffers, and the Thrift writer moved into core. Two serializer stacks exist, Thrift forces eager execution, cursor writers write a few bytes at a time (item 240), and large answers such as SP2Bench q4 at 1.96 GB need a raised limit. It depends on items 91, 240 and 243.

### Stage 6: the executor merge

**Single morsel-driven executor with eager as a consumption policy.** Replace the eager columnar executor and the pull-cursor executor with one morsel-driven pull engine in which every operator serves demand over column batches, `collect()` requests unbounded demand with whole-input morsels and Rayon across them, and exchange operators, partitioned hash builds and parallel merge joins give pipeline parallelism. The remaining fallback kinds get native leaf cursors, so the 840-line admission layer, `State::Fallback` and the eager memory retry go. Two complete executors exist (about 8,800 and 11,650 non-test lines) with 17 entry-point rules. Eager's remaining wins at 12 threads come from whole-table parallelism inside operators. It depends on items 7, 93, 240, 243 and 293.

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
  the remaining eager fallbacks that the merge must absorb.
* Items 240, 243 and 293 settle the cursor writers, row accounting and BIND reuse that
  stage 6 relies on.
* The join-algorithm work of [X15](../X15-join-algorithms-and-estimation/README.md),
  [item 454](../../ROADMAP.md), follows the merge so that new algorithms land in one executor.

## Acceptance gates

* No query is 5% or more slower on forge at 12 threads, and no query that runs under
  0.3 ms is slower on an Intel host. This is the decision gate of the executor merge, and
  every stage that changes an executor path meets it.
* The eager-against-cursor differential tests pass before either copy of a kernel is
  deleted, and the forge-pinned matrix of item 469 shows no regression for the 33
  synthetic queries at 1.05M and 10.5M triples with 12 threads and with one.
* The W3C suites pass with `execution=streaming` as well as with the default, which item
  470 makes part of the gate.
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

Stage 1 has shipped and stage 3 is under way, and [item 453](../../ROADMAP.md) tracks the
rest of the work.

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
mode into an `InferenceView`. Its query options read the selected data, its `apply`
changes a caller's options as the mode changes the dataset's defaults, its `inference` is
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

### Measurements of stage 1

A Namespace screen of the base against the five deletions ran the 30 synthetic queries
at 1.05M triples with one thread in four A,B,B,A rounds. The geometric mean of server
time was 0.982 of the base, and no query was 5% or more slower. `regex-iri` read 1.024
and `class-predicates` 1.020 with every round above 1, which item 514 measures on
an Intel host. EXPLAIN printed the same plans before and after for four generated groups
of 17 to 22 triple patterns over the same data, a chain, a chain with a filter, a star
and a snowflake, since a group of more than 16 inputs already kept the greedy plan of
`joinorder.rs`.
