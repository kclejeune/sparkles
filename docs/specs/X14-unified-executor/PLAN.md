# X14 plan for the remaining stages

This file is the implementation plan for the stages of the [unified executor](SPEC.md)
that have not shipped. It records what the audit of the code against the design found,
orders the remaining work into lanes with their files, decisions, gates and
measurements, and names the pieces a cheaper model can take. The [design](SPEC.md) stays
the authority on what each stage delivers, and the [Outcome](SPEC.md#outcome) records
what has shipped. [Roadmap item 453](../../ROADMAP.md) holds the work, and the items
numbered here are the phases that are ready to start.

## Where the code stands

Stages 1 and 3 have shipped, apart from the storage items 479 and 484 that
[X16](../X16-storage-consolidation/README.md) holds. Stage 4's accounting model and
mode-neutral kernels have shipped as items 671 to 674, item 675's hierarchical budget is
landing, and item 678 holds the two decoding twins that remain. The rest of the design is
stages 5 and 6, together with the phases that are scheduled with their area and the
execution additions, which this plan does not schedule.

The audit compared the design's description of the code with the code at the base of
this plan. The findings below are what the lanes build on.

Both executors already share one batch type. The eager executor's `Table` in
`sparql/table.rs` is the column batch of the cursor as well, and a cursor batch is a
`Table` with an owned charge, the `Buffer` of `sparql/cursor.rs`. The cursor is not an
operator trait. Its operators are the variants of the `State` enum in `cursor.rs`, which
`Operator::build_for` selects from a plan node and `Operator::next_inner` drives, and
each variant's module holds its own `next`.

The cursor already runs many eager kernels batch by batch. `cursor/expand.rs` runs index
joins, LATERAL, property functions and bound paths through `exec::execute_with_inputs`
and `exec::path_batch`, `cursor/runs.rs` groups finished key runs through `exec::group`,
`cursor/topk.rs` offers rows through `exec::offer_rows` and `exec::ordered_topk`,
`cursor/walk.rs` walks `exec::PathWalk`, `cursor/filter.rs` filters through
`exec::apply_unary` and `exec::filter_scan_morsel`, the external sort pieces sort through
`exec::apply_blocking`, and `cursor/graph.rs` describes through `describe::describe_batch`
and builds triples through the eager `construct_row`. The remaining twins are therefore
concentrated in the join, grouping, DISTINCT and sort families and in the result writers.

The join families exist twice. The eager side is `join_pairs` with its flat table
`KeyGroups`, `left_join`, `merge_left_join`, `keyed_join`, `minus`, `anti_join` and
`half_join` in `exec.rs`, about 700 lines, and it never spills. The cursor side is
`cursor/binary.rs` with its flat table `HashIndex` and `EqualityIndex`, `cursor/merge.rs`,
the Grace partitioning of `cursor/joinspill.rs` and the ripple join of `cursor/ripple.rs`,
about 4,000 lines. The two sides share `zipper::lower_bounds`, `eqkey::eq_key`,
`exec::filter_mask`, `exec::gallop` and `exec::run_end`. Two galloping searches exist,
`exec::gallop` over an id slice and `indexjoin::gallop_to` over a predicate. The eager
merge OPTIONAL honours the `merge_left_join` switch and the cursor's merge does not, so an
algorithm toggle that the design keeps for diagnosis selects an algorithm in one executor
only.

Grouping exists five times. `exec::group` and `exec::group_incremental` serve eager
plans, `cursor/group.rs` holds the hash grouping with spill in partitions,
`cursor/runs.rs` groups key runs, and `incr/ops/group.rs` maintains the aggregates of
incremental views with its own `AggKind` and i128 sums. The eager incremental grouping
admits no `GROUP_CONCAT` and the cursor grouping does. All but the view operator share
`exec::AggState`, and registered aggregates reach every path through
`registeredagg::aggregate`. No accumulator trait exists apart from the user-extension
traits of `extensions.rs`.

DISTINCT and sorting exist twice. `exec::distinct` is a hash set over rows without spill,
and `cursor/distinct.rs` spills in partitions. `exec::order_by_rows` sorts whole tables,
and `cursor/extsort.rs` sorts runs and merges them with a fan-in of 64. The sort keys,
the top-k heap and the index-order top-k are shared through `sortkey.rs` and the eager
functions the cursor calls. `cursor.rs` keeps a `count_input` helper that only the
merge tests reach.

Paths are one implementation in two executors, and path search is another. The eager
`Graph` of `exec.rs` with its `SWEEP_RATIO` and the `Engine` of `pathsearch.rs` with the
same ratio each decide between seeking and sweeping, and the fixed-point operator of
`incr/fixpoint.rs` with `incr/ops/path.rs` maintains closures with its own expansion. Path
ranges live in `plan.rs`, not in the ARQ property functions.

Three expression memos keep three admission policies. `exprcache.rs` admits per-value
reuse from a Chao1 estimate of distinct inputs and a sample of 4,096 rows,
`cursor/bind.rs` keeps the cursor's own exact id cache behind a pilot over its first
4,096 rows, and the EXISTS memo of `expr.rs` admits up to 100,000 entries within an
eighth of the budget. The decorrelated EXISTS key sets of `exists.rs` are shared.

The result writers exist twice, and the design's two gaps have narrowed. The eager
writers of `sparql/results.rs` write a whole `QueryResult` through `Buffered`, which marks
row ends and counts the rows the client received. The cursor writers write through the
`Shared` adapter without that count, and SELECT, graph, Jena graph and native JSON each
have a cursor writer of their own. ASK has no cursor writer, so a streaming request runs
ASK eagerly. The SPARQL Results Thrift encoder `ThriftResults` is already in
`sparkles-core`'s `jena_formats.rs`, and only its writer `write_thrift_results` is in the
server, where Thrift still forces eager execution through the policy's `cursor_format`.

The admission layer is 845 lines of `cursor.rs`, from `ExecutionMode` to
`fits_without_spill`. `auto_streaming` admits scans of at least 1,000 estimated rows,
groupings of at least 20,000, spilling or memory-admitted plans, and a narrow filtered
ASK shape, for SELECT and ASK only and never over a delta snapshot or with initial
bindings. Every other kind is eager under automatic execution, and `query_execution_with_plan`
retries an eager memory failure as a cursor through `retry_as_cursor`, which the
`eagerMemoryRetry` counter of `/$/server/memory` reports. `Operator::build_for` builds
`State::Fallback` for every node without a cursor, which runs its subtree eagerly or
collects its children's output first. The kinds that fall back are SERVICE, text search,
vector and hybrid search, the five spatial kinds, path search, history changes, view
reads, algorithm calls, the predicate-table `PatternScan`, `CountJoin`, an index-order
top-k without the heap, a path of an unsupported shape, remote LATERAL and any node whose
EXISTS does not stream.

The cursor rewrites plans in two places, `orient_filtered_ask` for ASK and `first_rows`
for first-row demand, and the planner reads the mode in one, where `has_predicate`
declines `PatternScan` for a cursor. Because automatic execution plans before it enables
cursor accounting, an automatically admitted plan may use `PatternScan` and sample its
filters where an explicit streaming request may not, so EXPLAIN differs between the two
ways of reaching the cursor. The result cache and sampled filters are off for explicit
cursors. The unit test of the mode checks counts six textual `streams()` calls and has no
headroom, and four direct reads of the `streams` field in `ctx.rs` are not counted.

Eager LIMIT replays prefixes in `run_limited`, 256 lines, which grows its budget eightfold
per round, and `unionpush::limited_input` decides from the mode whether to seed that
budget. `unionpush::order_ask` orders UNION branches for eager ASK alone. `ask_streaming`
and `QueryCursor::collect` exist and have no production caller.

The entry points state their needs through `choose_execution`, as stage 3 delivered. The
server keeps three further choices of its own. The `execution` parameter and the Thrift
exclusion feed the policy, the streaming route refuses ASK and Thrift, and the in-place
quick runner of `http/inline.rs` keeps a history per query text that chooses between an
eager run and automatic execution and retries a failed eager trial automatically. The
design keeps the runner as a scheduling choice, and its mode choice has to go with the
merge. The Jena binding selects streaming through its own context symbol, and MCP,
GraphQL and the Python `query` methods read whole answers eagerly.

The parallelism of the two executors differs most. `exec.rs` uses Rayon in about 45
places, inside scans, filters, joins, sorts and decoding, and the cursor uses it in four,
for its parallel filters and decoding. The morsel-parallel cursor of stage 6 is the piece
of the design with no code behind it.

## Order of work

The lanes run in the order below. A lane lands when its review is clean and its gates
pass, with its Intel measurements taken on atlas, and the forge matrix runs after the
push of each stage's batch rather than for each lane, since one run takes about two and
a half hours of the benchmark host, which runs published measurements and post-push
checks only. Lanes in one row of the table can run at
the same time, and the rows are serial.

| Step | Lanes | Why they wait |
|---|---|---|
| Now | Item 675 lands. Items 678, 512 and 534 start. Item 825 starts. Item 827 starts. | Nothing in this row changes another row's files beyond small merges. Item 534 is measurement work that items 240 and 828 build on. |
| After 825 | Item 826 starts. Item 240 starts once item 534 says where the streamed time goes. | Item 826 shares `Operator::build_for` and the eager dispatch with item 825, so it follows rather than conflicts. |
| After 827 | Item 829 starts. | The leaf cursors land in the admission code that item 827 changes. |
| After 240 | Item 828 starts. | The writers take the buffer that item 240 gives the cursor writers. |
| After 293 | The expression memo phase is numbered and starts. | Its admission policy is item 293's open question. |
| After 825 to 829, 512 and the remaining work of item 7 | The merge phase is numbered and starts, measured on forge against the faster of the two modes. | The merge deletes the eager executor, so every kernel it keeps must already be one, and the reference evaluator must already be the oracle. |
| After the merge | The frontier kernel and the demand seeding phases are numbered and start, and so does [X15](../X15-join-algorithms-and-estimation/README.md). | Neither blocks the merge, and new algorithms land in one executor. |

Each lane keeps the stage 4 rules that the maintainer settled, which the
[design](SPEC.md#stage-4-one-accounting-model) lists. Each lane that changes an executor
path meets the [acceptance gates](SPEC.md#acceptance-gates), measured as the
[measurement section](#how-the-gates-are-measured) describes.

## Stage 4 remainder

**Hierarchical budget, item 675.** The lane is landing. The maintainer decided that a
graph result keeps the query's admitted high point charged to the pool until the result is
dropped, as the design says for eager contexts until stage 6 leases every table. The
seams it leaves open, native MCP frames, snapshot and mask escapes, deallocation proofs
and the later milestones, stay recorded in its report and do not block stage 5.

**Decoding twins, item 678.** Give the string floors of `string_order_mask` a checked
decoding so that cursor batches compare two string columns through them as eager tables
do, merge `cursor_dictionary_values` into `decode_for`, and lower `MODE_CHECKS` in the
bounded-reads test to five. The test must fail at six before the change. The lane also
counts the direct reads of the `streams` field in `ctx.rs` in that test, so that the
bound covers every read of the mode and not only the method calls. This is routine work
for a cheaper model with the design's text as its specification.

## Stage 5: shared kernels

### Item 825: one join-kernel library over column batches

This is the keystone of stage 5 and the recommended first implementation lane. It needs
substantial judgment, since it changes executor cost decisions, and its planner should
implement it.

The lane builds one `sparql/join` module in `sparkles-core` with one kernel per algorithm
and no knowledge of the mode. A hash kernel owns one flat table, the `HashIndex` shape of
`cursor/binary.rs`, which replaces `KeyGroups` in `exec.rs`, and one probe loop that
takes a probe batch and an output room and returns pairs, which replaces the hash branch
of `join_pairs` and `Binary::pairs`. The kernel has the five modes of `binary::Mode`,
inner, OPTIONAL, semi, anti and MINUS, so `left_join`, `append_unmatched`, `half_join`,
`minus` and `anti_join` become calls of the kernel in a mode. A merge kernel takes two
sorted batches with their runs, the `Merge` of `cursor/merge.rs`, and replaces the zipper
branch of `join_pairs`, `merge_left_join` and the merge count of `join_count`, with the
spooling of long runs kept. A keyed kernel owns `EqualityIndex` and replaces `keyed_join`
and `keyed_left_join`. The ripple join stays in `cursor/ripple.rs` as the ASK algorithm,
and the index join stays in `indexjoin.rs`, with `exec::gallop` and `indexjoin::gallop_to`
merged into one galloping search over a slice with a predicate. The Grace partitioning of
`cursor/joinspill.rs` wraps the hash kernel, so an eager join under a budget that its
build does not fit spills instead of failing. An eager join is then one probe of a
whole-input batch with Rayon over probe pieces of `PROBE_PIECE` rows, and the cursor's
join is the same kernel over its batches.

The lane settles these questions inside the design's rules. The eager merge OPTIONAL
honours `merge_left_join` and the cursor's does not, so the kernel honours it in both
modes and the differential tests compare the switch's two paths. Eager OPTIONAL conditions
are evaluated once over the gathered candidates of a chunk, as `Binary::condition` already
does, which is the behaviour item 570 asked the cursor to reach. The lane therefore
closes item 570 by construction and measures the conditional OPTIONAL of its record,
`OPTIONAL { ?p ex:advisor ?o FILTER(?o != ex:x) }` at 10.5M quads, in both modes. An
EXISTS as an OPTIONAL condition stays eager under automatic execution until the
measurement shows the shared kernel within the gate, and the lane then admits it, since
its earlier admission and revert are recorded in
[PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md#query-execution). The
UNDEF-compatible nested loop of `join_pairs` stays as the kernel's fallback for keys the
hash cannot take.

The regression tests that fail before the change are a differential test in
`tests/query_cursor.rs` that joins with `merge_left_join` off in both modes and compares
the answers, a test in `tests/spill_join.rs` that runs an eager hash join under a budget
its build does not fit and expects the spilled answer rather than a memory error, and a test that a conditional OPTIONAL streamed at a batch size of
one returns the eager answer. The edge cases are UNDEF on a join key, a key column with
every row equal, a build side larger than the probe side, an OPTIONAL whose condition
reads a variable only the right side binds, MINUS with no shared variable, and a spill
that partitions a heavy key three levels deep.

Its gates are the differential tests of `tests/query_cursor.rs` and `charged_state.rs`,
`mise run test:w3c:streaming` with no new eager fallback, the plain and encrypted spill
matrices of the batch gate, `tests/spill_join.rs` and `tests/keyed_join.rs`,
`tests/eager_memory_retry.rs` with its count of retries lowered where joins now spill,
and the performance gates on `star-join`, `optional-chain`, `optional-count`,
`distinct-join`, `minus`, `exists-join`, `star-lookup`, `values-star` and the BSBM
Explore mix. Item 696 records that `join_pairs` and `indexjoin::search` move with their
alignment, so the lane reads the Intel A/B with that in mind and records the alignment of
the kernel's loops when a flag survives the rerun. The docs to update are the design's
Outcome, the querying guide `docs/querying/README.md` where it describes automatic
execution and OPTIONAL, `docs/API.md` where it describes `eagerMemoryRetry`, and items
570 and 696.

### Item 826: one grouping operator with an accumulator trait

The lane follows item 825 because both change `Operator::build_for` and the eager
dispatch. It needs judgment for the trait and is otherwise a port of known code.

The lane defines an accumulator trait in `sparql/group` with `init`, `add_batch`,
`merge` and `finish`, implemented by `exec::AggState`, the `Concat` state of the cursor,
FOLD through `exec::fold` and registered aggregates through `registeredagg::aggregate`,
and one grouping operator with two strategies, hash grouping with spill in partitions
from `cursor/group.rs` and sorted-streaming grouping over key runs from `cursor/runs.rs`.
The operator replaces `exec::group`, `exec::group_incremental` and `exec::aggregate`, and
the eager grouping becomes one call of the operator over a whole-input batch, with Rayon
over partitions and `merge` to combine them. `GROUP_CONCAT` is admitted on every path, so
the eager incremental grouping stops declining it. The count plans keep their own kinds,
since they read counts from the index rather than aggregate rows.

The trait's `retract` method is defined for the view operator of `incr/ops/group.rs`, and
whether that operator moves onto the trait in this lane is an [open decision](#open-decisions).
The lane ports it only if the maintainer chooses so, with `incr_bench` as its gate.

The regression tests that fail before the change are a test that an eager `GROUP_CONCAT`
over one key takes the incremental path in EXPLAIN, a test in `tests/group_runs.rs` that
an eager grouping under a budget its groups do not fit spills in partitions and returns
the cursor's answer, and a differential test over mixed aggregates with `HAVING` at batch
sizes one and 4,096 in both modes. The edge cases are an ungrouped aggregate over an empty
input, a group key that is UNDEF, `SAMPLE` over a group whose rows arrive in several
partitions, a registered aggregate that records argument order, and a spill in which one
group exceeds a partition.

Its gates are the differential tests, `mise run test:w3c:streaming`, the spill matrices,
`mise run bench:incr` if the view operator moves, and the performance gates on
`group-avg`, `types-grouped`, `subquery-agg`, `expr-bind-group`, `expr-agg-arg` and
`predicate-counts`, where `types-grouped` is a query of about 11 µs under the Intel rule.
Item 682 holds the small eager costs on these queries, and the lane records their change.

### Item 828: one columnar writer per format over batch sources

The lane follows item 240, which follows the measurement of item 534. Once the design is
written down it is routine work, apart from item 534's profiling.

Item 534 first finds where a streamed response spends about 0.27 ms more than an eager
one on queries under 1 ms, between `QueryCursor::next_batch` and the response writer, with
`perf` on atlas and the instruction counts of item 473's harness. Item 240 then gives the
cursor writers the `Buffered` writer of the eager path, so that every writer counts the
rows the client received the same way, and item 243 finds a row-end record that costs
nothing per write, measured on `optional-chain`.

Item 828 then defines a batch source trait in `sparql/results` with `next_batch` and the
variables, implemented by `QueryCursor`, by `GraphCursor` and by an eager `QueryResult`
as a one-batch source, and writes each format once over it. The JSON, XML, CSV and TSV
writers of `write_cursor_solutions`, the native JSON writer, the RDF writers of
`cursor/graph.rs`, the Jena writers and the Thrift writer each become one function. The
Thrift writer moves from `sparkles-server`'s `http.rs` into `sparkles-core` beside
`ThriftResults`, and ASK gets a writer over a one-row source, so `cursor_format` and the
streaming route's ASK refusal go from the policy and the server. Term decoding runs once
per batch column through `decode_for` into prebuilt buffers, and the eager writers' row
scratch of `results/output.rs` becomes the source's decode buffer. The `_counted` eager
writers and the `Shared` adapter are deleted.

The regression tests that fail before the change are a server test that a streaming
request for Thrift streams rather than running eagerly, a server test that a streaming
ASK answers through the same route as SELECT, a test in `results/output_boundary_tests.rs`
that a streamed answer cut by the byte budget reports the rows the client received as the
eager one does, and a differential test that every format's bytes for a result are equal
whether written from the eager result or from a cursor at a batch size of one. The edge
cases are an answer of zero rows in each format, a literal larger than the buffer, a
graph answer whose last batch holds only labels, a decode refusal in the middle of a
batch, which keeps its original error and the earlier complete rows as stage 4 decided,
and a client that disconnects between batches.

Its gates are the server's budget, limit and quota tests, `mise run test:w3c:streaming`,
`scripts/ci.sh --fast`, the end-to-end wall times of `export-500k`, `optional-chain`,
`star-join` and `order-by-full` in the matrix, where streaming is 8% to 51% slower today
and the merge needs it within the gate, and SP²Bench `q4` on forge for item 91. The
bindings run `bench:bindings`, since the Node, Python and FFI bindings serialize through
these writers.

### The frontier-expansion kernel

This phase shares one `expand(frontier, direction, lanes, graph filter, exclusions,
dedup)` kernel with the seek-or-sweep and dense-block policies across the eager `Graph`
of `exec.rs`, the `Engine` of `pathsearch.rs` and the `Closure` program of
`incr/ops/path.rs`. The audit found that the two executors already share the path
kernel, so the phase removes a copy between the executor and path search rather than
between the executors, and it does not block the merge. It is scheduled after the merge
and gets its number then. It depends on items 8 and 51 for the ARQ path ranges and the
graph-algorithm surface, and the cold-read rule of X07 applies to every copy once they
are one.

### The query-scoped expression memo

This phase replaces `exprcache`'s per-value reuse, the `Reuse` of `cursor/bind.rs` and the
EXISTS memo of `expr.rs` with one charged memo keyed by expression and input value, with
one admission policy fed by observed repetition. It waits for item 293, whose question is
the admission policy for sparse reuse, and gets its number when item 293 settles it. The
memo's charge is a retained charge of stage 4, its bound is the EXISTS memo's eighth of
the budget, and its oracle is the differential test of stage 4 that compares reuse with
row-by-row evaluation on both sides of the 16,384-row threshold.

## Stage 6: the executor merge

### Item 827: plans independent of the mode, with LIMIT and ASK through pull demand

This lane combines two phases of the design that change the same files, `unionpush.rs`,
`plan.rs` and the admission code of `cursor.rs`. It needs judgment, since it moves the
ASK branch ordering into the planner's cost model.

The lane first gives `PatternScan` a resumable cursor over the predicate table, so that
`has_predicate` can stop reading the mode and both ways of reaching the cursor plan the
same scan. It then deletes the mode reads of `has_predicate` and `limited_input`, moves
`orient_filtered_ask` and `first_rows` from the cursor into the planner as decisions that
every mode takes, and makes the result cache and sampled filters planning choices that do
not depend on the mode. The result cache serves a cursor's subtree when the subtree is
materialized anyway, and sampled filters prepare their samples once per plan. EXPLAIN
then prints one plan for a query whatever the mode, which `plan_fidelity_tests.rs`
checks. `MODE_CHECKS` falls to the two reads that stage 6's merge removes, the mode branch
of `query_execution_with_plan` and the EXISTS key set accumulated across batches.

The lane then routes every ASK and unordered LIMIT through pull demand. ASK runs
`ask_streaming` in every mode, so the narrow `filtered_ask_admitted` shape and the eager
ASK path go, and `order_ask`'s UNION branch ordering becomes a cost of the planner's
join-order program on cost summaries, so that a UNION under ASK or a LIMIT puts its
cheapest branch first in both modes. An unordered LIMIT runs the cursor with a demand of
`offset + limit` rows, so `run_limited`, `execute_limited`, `scan_limited`, the
first-work estimator of `unionpush.rs` and `limited_input` are deleted. The LIMIT
pushdowns of item 7 into SERVICE, text search and prefix-output property functions stay
in the planner. `Dataset::ask_streaming_with` folds into `ask`, since it has no caller.

The regression tests that fail before the change are a test in `plan_fidelity_tests.rs`
that EXPLAIN of a variable-predicate query is the same under `execution=eager`,
`streaming` and `auto`, a test in `tests/union_planning.rs` that a UNION under LIMIT with
an expensive first branch answers in the time of its cheap branch in eager execution, as
the cursor does, and a test that an ASK over a cross product with an equality filter
streams through the ripple join from any mode. The edge cases are a LIMIT whose offset
exceeds the answer, a LIMIT over a UNION whose branches share no variable, an ASK whose
pattern calls SERVICE, which keeps draining its input, an ASK with initial bindings, and
a LIMIT over a `PatternScan` that stops at the first block.

Its gates are `plan_fidelity_tests.rs`, `tests/union_planning.rs`,
`tests/exists_auto.rs`, `mise run test:w3c:streaming` with no new eager fallback, the
differential tests, the performance gates on `export-500k`, `values-star`,
`class-predicates`, `class-predicate-list`, `predicate-subjects`, `not-exists` and
`exists-join`, and the ASK shapes whose eager and streamed times the
[bounded filtered ASK admission](../X05-streaming-query-execution.md#bounded-filtered-ask-admission-2026-10-22)
of X05 recorded. The matrix has no ASK query, so the lane adds those ASK shapes to its
atlas A/B and applies the Intel rule to them and to the `PatternScan` queries under 0.3 ms.

### Item 829: native leaf cursors for the remaining fallback kinds

The lane follows item 827 and lands in the admission code it changes. Its routine half
suits a cheaper model with this plan as its specification, and its input-driven half
needs judgment.

A fallback kind whose eager kernel answers a whole table from the index or a remote
answer, SERVICE without input, text search, vector and hybrid search, spatial scans and
nearest neighbours, history changes, view reads, algorithm calls and `CountJoin`, becomes
a materialized leaf. The leaf runs the kernel once into a `Buffer` charged to the budget
and serves it batch by batch, which is what `State::Fallback` without inputs does today,
so the change is the leaf's own `State` variant with its reason string and the deletion
of the subtree fallback for these kinds. Whether a charged materialized leaf counts as
native for these kinds is an [open decision](#open-decisions), and the lane starts with
the kinds whose answers are bounded by `k` or `maxHits`.

A fallback kind that reads its input, SERVICE with input, remote LATERAL, spatial joins
and relations, path search with input and a path of an unsupported shape, becomes an
expanding operator in the manner of `cursor/expand.rs`, which runs the eager kernel over
each input batch and serves the result, and a kind whose eager kernel needs the whole
input, such as a spatial join whose build side is the input, builds over the input's
batches as the hash kernel of item 825 does. An index-order top-k without the heap takes
the heap of `cursor/topk.rs` with its runtime fallback. After the lane, `State::Fallback`
builds only for a node whose EXISTS does not stream, and `mise run test:w3c:streaming`
records no eager fallback for the W3C query suites.

The regression tests that fail before the change are a test in `tests/query_cursor.rs`
that a streaming request over each former fallback kind returns a cursor plan without a
materializing subtree, and a differential test per kind at a batch size of one. The edge
cases are a remote SERVICE that fails after its first batch, which aborts the transfer
as X05 specifies, a text search whose hits exceed `maxHits`, a spatial join with an empty
input, a view read during a view rebuild, and an algorithm call inside a transaction.

Its gates are the W3C streaming count, the differential tests, `tests/exists_service.rs`,
`tests/service_enhancer.rs`, `tests/geo_join.rs` and the text and vector test suites, and
the performance gates of `scripts/bench-text.sh`, `scripts/bench-geo.sh` and
`scripts/bench-vector.sh` in both modes with `bench:bindings`, measured as end-to-end
wall times, since the leaves write while they read. The matrix has no query of these
kinds, so these scripts are the lane's forge evidence.

### The reference evaluator, item 512

The test-only evaluator that follows the SPARQL algebra literally becomes the oracle of
the differential tests before the eager executor is deleted, since the tests then have no
second executor to compare with. It is routine work with an exact specification, the
SPARQL 1.1 algebra, and a cheaper model can take it with an Opus or Sol validation of its
semantics on the W3C suites. It runs in parallel with stage 5 and must land before the
merge phase starts. Its gate is that it agrees with the engine on every W3C query test and
on the seeded queries of `tests/query_cursor.rs` at both modes.

### The merge

The merge phase gets its number when items 825 to 829 and 512 have landed and the
remaining work of item 7 is done or decided. It is the design's single morsel-driven
executor, and it is judgment-sensitive throughout.

The phase makes `collect()` the eager API. `QueryCursor::collect` drains a cursor into a
`QueryResult` under the budget and admits a cursor that has already emitted rows, and
`execute_query` and `query_with_plan` become `query_cursor(...).collect()`, with
`Dataset::query` and the bindings' eager methods on top. Collection asks for unbounded
demand, so a scan serves its whole range as one morsel, a join probes it in parallel
pieces, and the operators keep the Rayon they have inside kernels. This is the
parallelism eager execution has today, and whether exchange operators and partitioned
hash builds across pipelines are part of this phase or a later one is an
[open decision](#open-decisions). The phase then deletes `run_node` and the eager
executor, the admission layer from `auto_streaming` to `fits_without_spill`,
`State::Fallback`, `retry_as_cursor` with the `eagerMemoryRetry` counter, the mode
branch of `query_execution_with_plan`, `Accounting::Collect`, the `streams` flag and its
bounded-reads test, the `execution` parameter's effect on how a query runs, and the mode
history of the in-place quick runner, which keeps its choice between the receiving worker
and the blocking pool. The eager context's retention at its admitted high point becomes a
lease on every table, as stage 4 anticipated.

The gates are the whole set. The W3C suites pass with the default and with
`execution=streaming`, which then differ only in the response's commit point. The
differential tests run against item 512's evaluator and the answer fingerprints of the
forge matrix. The forge matrix runs the merged build against the stage 4 record, and the
oracle of each query is the faster of the record's two modes, with no query 5% or more
slower at 12 threads and no query under 0.3 ms slower under the Intel rule. The memory
benchmarks of `docs/benchmarks/memory`, the spill matrices, `bench:incr`,
`bench:reasoning-latency`, `bench:insn` and `bench:bindings` run as the design lists. The
plan recommends that the merge, as the design's decision gate, run the forge matrix
before it lands rather than after the push, which is an [open decision](#open-decisions)
because the benchmark host otherwise takes no lane measurements.

### Demand seeding inside the planner and one algebra visitor

This phase makes demand a planner-integrated operator that takes its seeds from the real
plan, so that `reasoning::apply` stops ordering joins of its own and running earlier
patterns twice, and it shares one `GraphPattern` visitor and fold across the about 45
traversal functions in 28 files that the audit counted, from the planner's
`collect_pattern_vars` to the MCP tools' `has_service`. It depends on item 190 and gets
its number when item 190 settles the per-view cost of the demand stages. It does not
block the merge, and its visitor half is routine work for a cheaper model once the
visitor's shape is designed.

## How the gates are measured

**The forge rule.** No query is 5% or more slower on forge at 12 threads. Before a lane
lands, it runs the 31 queries of the matrix in both modes at 12 threads on atlas, which
has forge's hardware, as an interleaved A/B of its base and its tip through
`scripts/nsc-bench-remote.sh`, and treats a confirmed 5% as the gate. After the push of
the batch that ends a stage, the coordinator runs `mise run bench:executors` on forge
under the host's benchmark and heavy locks, with the build made inside `nix develop`
without learned incremental reuse, as item 677 records, and compares its record with the
baseline through
`scripts/bench-gate.py compare BASELINE RECORD --keys '/exec$'` and
`--keys '/wall$' --floor 5`. The baseline is the forge record after stage 4,
`docs/benchmarks/synthetic/runs/2026-11-02-executor-matrix-forge-9d086646`, until a later
stage's record replaces it, and the merge compares with the faster of the record's two
modes. A flagged query is rerun as an interleaved A/B of the two builds on the same host
through `scripts/nsc-bench-remote.sh` with `EXECUTION` set to the flagged mode, in at least
eight rounds of A,B,B,A in each order, and counts as a regression only when the A/B
confirms it. A regression the lane accepts gets its own roadmap item in the same change,
and a regression the forge record confirms after the push is bisected across the batch
and fixed forward. Forge runs no builds, so the binaries are built elsewhere and copied.

**The Intel rule.** No query that runs under 0.3 ms is slower on an Intel host. Each
lane measures on atlas, with the host to itself, through the harness of X08's latency
gate. The two builds share one store, the server is pinned to one performance core and
the client to another with one Rayon thread, and the rounds run A,B,B,A in both orders
with 200 to 300 requests per round and at least twelve rounds, which
`scripts/x08-gates/combine.py` combines to cancel the position bias. A query passes when
the upper end of the 95% interval of its ratio is at most 1.02 or its median added time
is at most 0.5 µs. Repeated runs differ by up to 4% on queries under 15 µs, which the
absolute bound covers. The queries are every query of the matrix that runs under 0.3 ms
in either mode at 1.05M triples, which the stage 4 record lists, and the lane names the
ones its change can reach. A difference that survives is profiled with `perf stat` and
LBR call stacks on atlas before the lane reports, as stage 4's per-query costs were.

**Correctness and conformance.** The differential tests of `tests/query_cursor.rs` run
their seeded queries at batch sizes 1, 2, 3 and 4,096 against the eager answer, and after
the merge against item 512's evaluator. `charged_state.rs` compares both modes under
budgets, `opt_tests.rs` compares every optimization with `Optimizations::NONE`, and
`plan_fidelity_tests.rs` checks that EXPLAIN matches what ran. `mise run test:w3c` and
`mise run test:w3c:streaming` pass, and the streaming record's count of eager fallbacks
does not rise in any lane and reaches zero for the query suites after item 829.
`scripts/ci.sh --fast --changed main` runs for every lane, with the binding tasks of the
bindings a lane touches.

**Resources and durability.** The plain and encrypted spill matrices of the batch gate
run for every lane that changes the files `scripts/gate/select-checks.py` lists, which
include `exec.rs`, `cursor.rs` and `spill.rs`. The memory tests of `ctx.rs`,
`query_memory.rs` and `tests/eager_memory_retry.rs` pass, with their expectations changed
only where newly shared behaviour explains the difference, and the memory benchmarks of
`docs/benchmarks/memory` run at the end of each stage. No lane changes an on-disk format,
and `crate::spill` keeps its version.

**The other gates.** `mise run bench:incr` runs when item 826 moves the view operator,
`mise run bench:reasoning-latency` runs for any lane that changes value decoding or the
query path of query-time reasoning, which items 828 and the merge do, and
`mise run bench:insn` runs for item 534's instruction counts. `bench:bindings` runs for
items 828 and 829 and for the merge.

## Models and parallelism

Items 825, 826 and 827 and the merge phase need substantial engineering judgment and run
on the planning model at high effort, with Sol at high effort reviewing. Item 678, item
512 and the routine half of item 829 run on a cheaper model from this plan, with the
validating planner checking item 512's semantics. Item 828 runs on a cheaper model once
items 534 and 240 have written down where the time goes, and item 534 itself is
performance work for a capable model on atlas. The visitor half of the demand phase is
routine once its shape is designed.

Items 678, 512, 534 and 825 run at the same time with item 827, since they change
different files. Items 826 and 829 follow their predecessors in the table above. Each
lane merges `origin/main` rather than local `main` when it needs main, and no two lanes
run the Namespace screen of one commit at once.

## Open decisions

These questions are for the maintainer. The plan recommends an answer for each and does
not decide it.

1. **Parallelism of the merged engine.** The design names both whole-input morsels with
   Rayon inside kernels and exchange operators with partitioned hash builds and parallel
   merge joins across pipelines. The plan recommends that the merge phase keep the first,
   which is the parallelism eager execution has today, and that exchange operators follow
   as a later phase measured on forge, so that the deletion of the eager executor is not
   held up by a new scheduler.
2. **What counts as a native leaf.** For SERVICE, text, vector, spatial nearest
   neighbours, algorithm calls, history and view reads, whose answers come whole from a
   remote endpoint or an index, the plan recommends that a leaf which runs its kernel once
   into a charged buffer and serves it in batches counts as native, so that only the eager
   subtree fallback goes. The alternative is a resumable reader per source, which the
   remote and `k`-bounded sources do not need.
3. **Incremental views in the grouping operator.** Item 418 maintains `MIN`, `MAX`,
   `GROUP_CONCAT`, `SAMPLE`, `HAVING` and correlated EXISTS in views and has no priority.
   The plan recommends that item 826 define `retract` on the trait and leave the view
   operator of `incr/ops/group.rs` where it is until item 418 adopts the trait, so that
   item 826 stays bounded and `bench:incr` is not at stake in it.
4. **Scope of the frontier kernel and the algebra visitor.** Both phases remove copies
   outside the two executors and neither blocks the merge. The plan recommends keeping
   them in X14 as phases after the merge, numbered when items 8, 51 and 190 allow, rather
   than moving them to their own specs.
5. **The eager memory retry.** After the merge, an eager answer is a collected cursor
   whose joins, groupings, sorts and DISTINCT spill, so the retry of an eager memory
   failure as a cursor and the `eagerMemoryRetry` counter of `/$/server/memory` go. The
   plan recommends removing the counter in the merge phase and recording the change in
   the server's memory documentation, since a metric that can only read zero misleads.
6. **The in-place quick runner.** The runner keeps a history per query text that chooses
   between an eager run and automatic execution. The plan recommends that the merge phase
   reduce it to its scheduling choice, the receiving worker against the blocking pool,
   keyed by the same history of run times, and that items 827 and 829 leave it alone.
7. **ASK under streaming.** After item 827 every ASK runs through pull demand in every
   mode, and the response stays an unchunked body with its status code, since it is one
   value. The plan recommends that `execution=streaming` for ASK no longer be refused by
   the streaming route and that the Jena binding's context symbol keep its meaning.
8. **Cadence of the forge matrix.** The plan recommends one forge matrix after the push
   of the batch that ends each stage, after items 826 and 829, with atlas A/Bs of the
   matrix queries for every lane before it lands, since a matrix takes about two and a
   half hours and the benchmark host takes no lane measurements. The merge is the
   design's decision gate, and the plan asks whether it may run the forge matrix before
   it lands as the one exception.
9. **Mode-check accounting.** The bounded-reads test counts textual `streams()` calls and
   not the four direct reads of the field in `ctx.rs`. The plan recommends that item 678
   count both, which raises the bound before it lowers it, so the CI check means what the
   design says.
