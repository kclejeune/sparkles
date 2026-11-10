# X14 plan for the remaining stages

This file is the implementation plan for the stages of the [unified executor](SPEC.md)
that have not shipped. It records what the audit of the code against the design found,
orders the remaining work into phases with their files, decisions, gates and
measurements, and names the pieces that are routine enough for a worker with an exact
specification. The [design](SPEC.md) stays the authority on what each stage delivers,
and the [Outcome](SPEC.md#outcome) records what has shipped.
[Roadmap item 453](../../ROADMAP.md) holds the work, and the items numbered here are the
phases that are ready to start. The [decisions](#decisions) at the end record the maintainer's
answers, and the schedule below follows them.

## Where the code stands

Stages 1 and 3 have shipped, apart from the storage items 479 and 484 that
[X16](../X16-storage-consolidation/README.md) holds. Stage 4's accounting model and
mode-neutral kernels have shipped as items 671 to 674, item 675's hierarchical budget is
landing, and item 678 holds the two decoding twins that remain. The rest of the design is
stages 5 and 6, together with the phases that are scheduled with their area and the
execution additions, which this plan does not schedule.

The audit compared the design's description of the code with the code at the base of
this plan. The findings below are what the phases build on.

Both executors already share one batch type. The eager executor's `Table` in
`sparql/table.rs` is the column batch of the cursor as well, and a cursor batch is a
`Table` with an owned charge, the `Buffer` of `sparql/cursor.rs`. The cursor is not an
operator trait. Its operators are the variants of the `State` enum in `cursor.rs`, which
`Operator::build_for` selects from a plan node and `Operator::next_inner` drives, and
each variant's module holds its own `next`.

The cursor already runs many eager kernels batch by batch. `cursor/expand.rs` runs index
joins, local LATERAL, property functions, registered property functions that declare
`prefix_output` and bound paths through `exec::execute_with_inputs` and
`exec::path_batch`, `cursor/runs.rs` groups finished key runs through `exec::group`,
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
`exec::filter_mask` and `exec::run_end`. Three bounded searches exist, `exec::gallop`
over an id slice, which `zipper.rs` uses, `indexjoin::gallop_to` over a predicate, and
the `before` search of `cursor/merge.rs`. The eager merge OPTIONAL honours the
`merge_left_join` switch and the cursor's merge does not, so an algorithm toggle that the
design keeps for diagnosis selects an algorithm in one executor only. The cursor's
`count_input` serves the count-only grouping of `Group::load` in production, through the
key-only mode of the merge join, and the eager side counts merge joins in `join_count`.

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
functions the cursor calls.

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
have a cursor writer of their own. An ASK that a streaming request runs is pulled through
the cursor by `finish_ask`, and its boolean is then written by the eager writers, since
one value needs no streamed writer. The SPARQL Results Thrift encoder `ThriftResults` is
already in `sparkles-core`'s `jena_formats.rs`, and only its writer
`write_thrift_results` is in the server, where Thrift still forces eager execution
through the policy's `cursor_format` and the streaming route refuses a streamed SELECT in
Thrift.

The admission layer is 845 lines of `cursor.rs`, from `ExecutionMode` to
`fits_without_spill`. `auto_streaming` admits scans of at least 1,000 estimated rows,
groupings of at least 20,000, spilling or memory-admitted plans, and the narrow filtered
ASK shape of `filtered_ask_admitted`, for SELECT and ASK only and never over a delta
snapshot or with initial bindings. Every other kind and every other ASK is eager under
automatic execution, and `query_execution_with_plan` retries an eager memory failure as a
cursor through `retry_as_cursor`. The retry is counted as the `eagerMemoryRetry` counter
of the query's plan and as the Prometheus metric `sparkles_spill_eager_retries_total` of
`server_memory.rs`.

`Operator::build_for` builds `State::Fallback` for every node that no eligibility
predicate admits, which runs its subtree eagerly or collects its children's output first.
Read from the predicates `supported`, `binary::eligible`, `merge::eligible`,
`ripple::eligible`, `group::eligible`, `runs::eligible`, `expand::eligible`,
`walk::eligible`, `filter::eligible`, `topk::eligible` and `extsort::eligible`, the kinds
that fall back are SERVICE, text search, vector and hybrid search, the five spatial kinds,
path search, history changes, view reads, algorithm calls, the predicate-table
`PatternScan`, `CountJoin`, an index-order top-k without the heap, a path of an
unsupported shape, remote LATERAL, a registered property function whose registration does
not declare `prefix_output`, and any node one of whose EXISTS patterns cannot itself be
built without materialization, which `exists_streams` decides by building each pattern
under the strict policy. The depth cap of four and the refusal of a LeftJoin condition
with EXISTS belong to `streams_natively`, the admission predicate of automatic execution,
and not to the fallback. Several of these kinds have two forms. The `needs_input` methods
of `TextSpec`, `VectorSpec`, `HybridSpec`, `HistorySpec`, `AlgoSpec`, `SpatialPfSpec` and
`PathSearchSpec` say whether a node reads variables its input binds, and the planner
places a node that does above its input.

The cursor rewrites plans in three places, `orient_filtered_ask` for ASK, `first_rows`
for first-row demand and `distinct_below_order`, which moves a DISTINCT over a projection
below an ORDER BY as a numbered DISTINCT when spill is available. The planner reads the
mode in one place, where `has_predicate` declines `PatternScan` for a cursor. Because
automatic execution plans before it enables cursor accounting, an automatically admitted
plan may use `PatternScan` and sample its filters where an explicit streaming request may
not, so EXPLAIN differs between the two ways of reaching the cursor. The result cache and
sampled filters are off for explicit cursors. The unit test of the mode checks counts six
textual `streams()` calls and has no headroom. Three direct reads of the `streams` field
in `ctx.rs`, in `cursor_owner`, `retained_charge` and `shrink_retained`, are not
counted.

Eager LIMIT replays prefixes in `run_limited`, 256 lines, which grows its budget eightfold
per round, and `unionpush::limited_input` decides from the mode whether to seed that
budget. `unionpush::order_ask` orders UNION branches for eager ASK alone. `ask_streaming`
and `QueryCursor::collect` exist and have no production caller. `collect` refuses a
cursor that has emitted rows, since earlier batches cannot be recovered. Beside
`execute_query` and `query_with_plan`, the eager executor is called directly by
`reasoning::seed_values`, `exists::build`, `plan::eval_exists`, the update path of
`update.rs` and the algorithm service.

The entry points state their needs through `choose_execution`, as stage 3 delivered. The
server keeps three further choices of its own. The `execution` parameter and the Thrift
exclusion feed the policy, the streaming route refuses a streamed SELECT in Thrift, and
the in-place quick runner of `http/inline.rs` keeps a history per query text that chooses
between an eager run and automatic execution and retries a failed eager trial
automatically. The design keeps the runner as a scheduling choice, and its mode choice
has to go with the merge. The Jena binding selects streaming through its own context
symbol, and MCP, GraphQL and the Python `query` methods read whole answers eagerly.

The parallelism of the two executors differs most. `exec.rs` uses Rayon in about 45
places, inside scans, filters, joins, sorts and decoding, and the cursor uses it in four,
for its parallel filters and decoding. The morsel-parallel cursor of stage 6 is the piece
of the design with no code behind it.

## Dependencies

The design's [dependencies](SPEC.md#dependencies) and the dependencies its candidate
phases name are carried here as they stand at the base of this plan.

| Phase | Completed prerequisites | Outstanding prerequisites |
|---|---|---|
| Join-kernel library, item 825 | Item 93. | Item 7's remaining work does not change joins, so item 825 does not wait for it. |
| Grouping operator, item 826 | Item 93. | Item 7's grouping over key runs has shipped. Item 418 is outstanding, and the [schedule](#order-of-work) says how item 826 proceeds with and without it. |
| Frontier kernel | Item 417. | Items 8 and 51. |
| Expression memo | None. | Item 293. |
| Columnar writers, item 828 | None. | Items 91, 240, 243 and 534. Item 828 follows items 534 and 240 and carries items 243 and 91 as its own gates. |
| Mode-independent plans and pull-demand LIMIT and ASK, item 827 | None. | Item 7, for the EXISTS shapes that stay eager, which item 827 leaves to item 829. |
| Native leaf cursors, item 829 | None. | Item 827 for the admission code, and item 825 for the operators that build over their input. |
| The merge | Items 93, 469 to 473. | Items 7, 240, 243, 293, 534 and 512, and items 825 to 829. |
| Demand seeding and one visitor | None. | Item 190. |

Items 7, 240, 243, 293 and 534 are on the merge's critical path because the design puts
them there, and this plan does not shorten that path. Item 243 lands inside item 828's
work on the writers, and item 293 settles the memo's admission before the merge deletes
the eager memo.

## Order of work

The phases run in the order below. A phase lands when its review is clean and its gates
pass, with its performance evidence taken on atlas, and the forge matrix runs after the
push of each stage's batch rather than for each phase, since one run takes about two and
a half hours of the benchmark host, which runs published measurements and post-push
checks only. Phases in one row of the table can run at the same time within the
ownership boundaries given below, and the rows are serial.

| Step | Phases | Why they wait |
|---|---|---|
| Now | Item 675 lands. Items 678, 512 and 534 start. Item 825 starts. Item 827 starts. | Each owns its own functions, as the boundaries below say. Item 534 is measurement work that items 240 and 828 build on. |
| After 825 | Item 826 starts. Item 240 starts once item 534 says where the streamed time goes. | Item 826 shares `Operator::build_for` and the eager dispatch with item 825. |
| After 827 and 825 | Item 829 starts. | Its leaf cursors land in the admission code that item 827 changes, and its operators that build over their input take item 825's hash kernel. |
| After 240 | Item 828 starts. | The writers take the buffer that item 240 gives the cursor writers. |
| After 293 | The expression memo phase is numbered and starts. | Its admission policy is item 293's open question. |
| After 825 to 829, 512, 243, 293, 534 and the remaining work of item 7 | The merge phase is numbered and starts. | The merge deletes the eager executor, so every kernel it keeps must already be one, and the reference evaluator must already be the oracle. |
| After the merge | The frontier kernel and the demand seeding phases are numbered and start, and so does [X15](../X15-join-algorithms-and-estimation/README.md). | Neither blocks the merge, and new algorithms land in one executor. |

Item 826 defines the trait's `retract` method and leaves the view operator of
`incr/ops/group.rs` to item 418, and exchange operators with partitioned hash builds
follow the merge as item 857, as the [decisions](#decisions) record.

The phases that run at the same time own these parts of the code. Item 678 owns
`exec::string_order_mask`, `exec::decode_for`, `exprcache::cursor_dictionary_values` and
the bounded-reads test. Item 825 owns the join functions of `exec.rs` named above, the
join arm of `exec::run_node`, `cursor/binary.rs`, `cursor/merge.rs`,
`cursor/joinspill.rs` and the join selection of `Operator::build_for`. Item 827 owns the
admission block of `cursor.rs`, the three plan rewrites, `has_predicate` in `plan.rs`,
`unionpush.rs`, `run_limited` and its callers, and the ASK paths of `mod.rs`. Item 512
owns new test code only. Item 534 changes no engine code until it has measured. Where two
phases touch one function, the later one merges the earlier one's landing commit. A phase
that needs a predecessor that local `main` holds but `origin/main` does not merges the
predecessor's landing commit rather than local `main`, so that it does not carry another
batch's unscreened work.

Each phase keeps the stage 4 rules that the maintainer settled, which the
[design](SPEC.md#stage-4-one-accounting-model) lists. Each phase that changes an executor
path meets the [acceptance gates](SPEC.md#acceptance-gates), measured as the
[measurement section](#how-the-gates-are-measured) describes.

## Stage 4 remainder

### Hierarchical budget, item 675

The phase is landing. The maintainer decided that a graph result keeps the query's
admitted high point charged to the pool until the result is dropped, as the design says
for eager contexts until stage 6 leases every table. The seams it leaves open, native MCP
frames, snapshot and mask escapes, deallocation proofs and the later milestones, stay
recorded in its report and do not block stage 5.

### Decoding twins, item 678

Give the string floors of `string_order_mask` a checked decoding so that cursor batches
compare two string columns through them as eager tables do, and merge
`cursor_dictionary_values` into `decode_for`. The bounded-reads test then counts every
read of the mode, which it does not do today. Its unit becomes a read of the `streams`
field outside its accessors, which covers the method calls it counts now and the three
direct reads in `cursor_owner`, `retained_charge` and `shrink_retained`. After item 678
the expected sites are the mode branch of `query_execution_with_plan`, `has_predicate`,
`limited_input`, the EXISTS key set accumulated across batches in `exists::prepare`,
`envelope_refuses` and the three direct reads, eight in all, and the test's bound is
eight with the string floors gone. The test must fail at the old bound before the change.
This is routine work with the design's text as its specification.

## Stage 5: shared kernels

### Item 825: one join-kernel library over column batches

This is the keystone of stage 5 and the recommended first implementation phase. It needs
substantial judgment, since it changes executor cost decisions, and its planner should
implement it.

The phase builds one `sparql/join` module in `sparkles-core` with one kernel per
algorithm and no knowledge of the mode. A hash kernel owns one flat table, the
`HashIndex` shape of `cursor/binary.rs`, which replaces `KeyGroups` in `exec.rs`, and one
probe loop that takes a probe batch and an output room and returns pairs, which replaces
the hash branch of `join_pairs` and `Binary::pairs`. The kernel has the five modes of
`binary::Mode`, inner, OPTIONAL, semi, anti and MINUS, so `left_join`,
`append_unmatched`, `half_join`, `minus` and `anti_join` become calls of the kernel in a
mode. A merge kernel takes two sorted batches with their runs, the `Merge` of
`cursor/merge.rs`, and replaces the zipper branch of `join_pairs`, `merge_left_join` and
the merge count of `join_count`, with the spooling of long runs kept and the key-only
count that `count_input` serves kept for the count-only grouping. A keyed kernel owns
`EqualityIndex` and replaces `keyed_join` and `keyed_left_join`. The ripple join stays in
`cursor/ripple.rs` as the ASK algorithm, and the index join stays in `indexjoin.rs`. The
three bounded searches, `exec::gallop`, `indexjoin::gallop_to` and the merge's `before`,
become one galloping search over a slice with a predicate. The Grace partitioning of
`cursor/joinspill.rs` wraps the hash kernel, so an eager join under a budget that its
build does not fit spills instead of failing. An eager join is then one probe of a
whole-input batch with Rayon over probe pieces of `PROBE_PIECE` rows, and the cursor's
join is the same kernel over its batches.

The phase settles these questions inside the design's rules. The eager merge OPTIONAL
honours `merge_left_join` and the cursor's does not, so the kernel honours it in both
modes and EXPLAIN names the algorithm the switch selected in both. An OPTIONAL condition
is evaluated once over the gathered candidates of a chunk in both modes, as
`Binary::condition` already does for the cursor and `left_join` for the eager table, and
the phase measures the conditional OPTIONAL of item 570's record,
`OPTIONAL { ?p ex:advisor ?o FILTER(?o != ex:x) }` at 10.5M quads, in both modes. Item
570 closes when that measurement puts the streamed query within the gate of the eager
one and the admission of an OPTIONAL whose condition is an EXISTS with a key set, whose
earlier admission and revert
[PERFORMANCE-EXPERIMENTS.md](../../PERFORMANCE-EXPERIMENTS.md#query-execution) records,
passes the same measurement. Until then item 570 stays open with the measurement
recorded. The UNDEF-compatible nested loop of `join_pairs` stays as the kernel's fallback
for keys the hash cannot take.

The regression tests that fail before the change are a test in `tests/query_cursor.rs`
that the cursor plan of an OPTIONAL over sorted inputs names the merge OPTIONAL with
`merge_left_join` on and the hash OPTIONAL with it off, as the eager plan does, a test in
`tests/spill_join.rs` that runs an eager hash join under a budget its build does not fit
and expects the spilled answer rather than a memory error, and a unit test that the one
galloping search agrees with `partition_point` on random sorted slices. The conditional
OPTIONAL across candidate chunks is already covered at batch sizes 1, 2, 3 and 4,096 in
`tests/query_cursor.rs`, and that coverage stays. The edge cases are UNDEF on a join key,
a key column with every row equal, a build side larger than the probe side, an OPTIONAL
whose condition reads a variable only the right side binds, MINUS with no shared
variable, and a spill that partitions a heavy key three levels deep.

Its gates are the differential tests of `tests/query_cursor.rs` and `charged_state.rs`,
`mise run test:w3c:streaming` with no new eager fallback, the plain and encrypted spill
matrices of the batch gate, `tests/spill_join.rs` and `tests/keyed_join.rs`,
`tests/eager_memory_retry.rs` with its count of retries lowered where joins now spill,
and the performance gates on `star-join`, `optional-chain`, `optional-count`,
`distinct-join`, `minus`, `exists-join`, `star-lookup`, `values-star` and the BSBM
Explore mix. Item 696 records that `join_pairs` and `indexjoin::search` move with their
alignment, so the phase reads the Intel A/B with that in mind and records the alignment
of the kernel's loops when a flag survives the rerun. The docs to update are the design's
Outcome, the querying guide `docs/querying/README.md` where it describes automatic
execution and OPTIONAL, `docs/API.md` where it describes `eagerMemoryRetry`, and items
570 and 696.

### Item 826: one grouping operator with an accumulator trait

The phase follows item 825 because both change `Operator::build_for` and the eager
dispatch. It needs judgment for the trait and is otherwise a port of known code.

The phase defines an accumulator trait in `sparql/group` with `init`, `add_batch`,
`merge`, `finish` and `retract`, implemented by `exec::AggState`, the `Concat` state of
the cursor, FOLD through `exec::fold` and registered aggregates through
`registeredagg::aggregate`, and one grouping operator with two strategies, hash grouping
with spill in partitions from `cursor/group.rs` and sorted-streaming grouping over key
runs from `cursor/runs.rs`. The operator replaces `exec::group`,
`exec::group_incremental` and `exec::aggregate`, and the eager grouping becomes one call
of the operator over a whole-input batch, with Rayon over partitions and `merge` to
combine them. `GROUP_CONCAT` is admitted on every path, so the eager incremental grouping
stops declining it. The count plans keep their own kinds, since they read counts from the
index rather than aggregate rows, and the count-only grouping keeps its shortcut through
the merge join's key-only count.

The view operator of `incr/ops/group.rs` stays with item 418, as
[decision 3](#decisions) records, so the phase defines `retract` for item 418 to
implement and leaves the view operator where it is.

The regression tests that fail before the change are a test that an eager `GROUP_CONCAT`
over one key takes the incremental path in EXPLAIN, a test in `tests/group_runs.rs` that
an eager grouping under a budget its groups do not fit spills in partitions and returns
the cursor's answer, and a differential test over mixed aggregates with `HAVING` at batch
sizes one and 4,096 in both modes. The edge cases are an ungrouped aggregate over an empty
input, a group key that is UNDEF, `SAMPLE` over a group whose rows arrive in several
partitions, a registered aggregate that records argument order, and a spill in which one
group exceeds a partition.

Its gates are the differential tests, `mise run test:w3c:streaming`, the spill matrices,
`tests/spill_group.rs`, `mise run bench:incr` if the view operator moves, and the
performance gates on `group-avg`, `types-grouped`, `subquery-agg`, `expr-bind-group`,
`expr-agg-arg` and `predicate-counts`, where `types-grouped` is a query of about 11 µs
under the Intel rule. Item 682 holds the small eager costs on these queries, and the
phase records their change.

### Item 828: one columnar writer per format over batch sources

The phase follows item 240, which follows the measurement of item 534. Once the design is
written down it is routine work, apart from item 534's profiling.

Item 534 first finds where a streamed response spends about 0.27 ms more than an eager
one on queries under 1 ms, between `QueryCursor::next_batch` and the response writer. It
counts user-space instructions per request with `perf stat` on atlas and profiles with
LBR call stacks, as stage 4's per-query costs were traced, since the instruction harness
of `bench:insn` covers vocabulary scans and WAL replay and not response writing. Item 240
then gives the cursor writers the `Buffered` writer of the eager path, so that every
writer counts the rows the client received the same way, and item 243 finds a row-end
record that costs nothing per write, measured on `optional-chain`.

Item 828 then defines a batch source trait in `sparql/results` with `next_batch` and the
variables, implemented by `QueryCursor`, by `GraphCursor` and by an eager `QueryResult`
as a one-batch source, and writes each format once over it. The JSON, XML, CSV and TSV
writers of `write_cursor_solutions`, the native JSON writer, the RDF writers of
`cursor/graph.rs`, the Jena writers and the Thrift writer each become one function. The
Thrift writer moves from `sparkles-server`'s `http.rs` into `sparkles-core` beside
`ThriftResults`, so `cursor_format` and the streaming route's Thrift refusal go from the
policy and the server. The ASK boolean is written through the same functions from a
one-row source. Term decoding runs once per batch column through `decode_for` into
prebuilt buffers, and the eager writers' row scratch of `results/output.rs` becomes the
source's decode buffer. The `_counted` eager writers and the `Shared` adapter are
deleted.

The regression tests that fail before the change are a server test that a streaming
request for a SELECT in Thrift streams rather than being refused, a test in
`results/output_boundary_tests.rs` that a streamed answer cut by the byte budget reports
the rows the client received as the eager one does, and a differential test that every
format's bytes for a result are equal whether written from the eager result or from a
cursor at a batch size of one. The edge cases are an answer of zero rows in each format,
a literal larger than the buffer, a graph answer whose last batch holds only labels, a
decode refusal in the middle of a batch, which keeps its original error and the earlier
complete rows as stage 4 decided, and a client that disconnects between batches.

Its gates are the server's budget, limit and quota tests, `mise run test:w3c:streaming`,
`scripts/ci.sh --fast`, the end-to-end wall times of `export-500k`, `optional-chain`,
`star-join` and `order-by-full` in both modes on atlas, where streaming is 8% to 51%
slower today and the merge needs it within the gate, and SP²Bench `q4` through
`scripts/bench-sp2b.sh` on atlas for item 91, with its forge record after the push. The
bindings run `bench:bindings`, since the Node, Python and FFI bindings serialize through
these writers.

### The frontier-expansion kernel

This phase shares one `expand(frontier, direction, lanes, graph filter, exclusions,
dedup)` kernel with the seek-or-sweep and dense-block policies across the eager `Graph`
of `exec.rs`, the `Engine` of `pathsearch.rs` and the `Closure` program of
`incr/ops/path.rs`. The audit found that the two executors already share the path
kernel, so the phase removes a copy between the executor and path search rather than
between the executors, and it does not block the merge. It is scheduled after the merge
and gets its number then. Item 417 has shipped, it depends on items 8 and 51 for the ARQ
path ranges and the graph-algorithm surface, and the cold-read rule of X07 applies to
every copy once they are one.

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

This phase combines two phases of the design that change the same files, `unionpush.rs`,
`plan.rs` and the admission code of `cursor.rs`. It needs judgment, since it moves the
ASK branch ordering into the planner's cost model.

The phase first gives `PatternScan` a resumable cursor over the predicate table, so that
`has_predicate` can stop reading the mode and both ways of reaching the cursor plan the
same scan. It then deletes the mode reads of `has_predicate` and `limited_input`, moves
the three cursor rewrites into the planner as decisions every mode takes, where
`orient_filtered_ask` and `first_rows` become planning choices and `distinct_below_order`
becomes a planning choice taken whenever spill is configured, and makes the result cache
and sampled filters planning choices that do not depend on the mode. The result cache
serves a cursor's subtree when the subtree is materialized anyway, and sampled filters
prepare their samples once per plan. EXPLAIN then prints one plan for a query whatever
the mode, which `plan_fidelity_tests.rs` checks, with a spill-configured DISTINCT over
ORDER BY among its cases. After item 827 the mode is read in six places, the mode branch
of `query_execution_with_plan`, the EXISTS key set of `exists::prepare`,
`envelope_refuses` and the three direct reads in `ctx.rs`, and the bounded-reads test's
bound is six. The merge removes the rest.

The phase then routes every ASK and unordered LIMIT through pull demand. A streaming
request already pulls its ASK through the cursor, so the change is that automatic and
eager execution do the same, the narrow `filtered_ask_admitted` shape and the eager ASK
path of `mod.rs` go, and `order_ask`'s UNION branch ordering becomes a cost of the
planner's join-order program on cost summaries, so that a UNION under ASK or a LIMIT puts
its cheapest branch first in both modes. An unordered LIMIT runs the cursor with a demand
of `offset + limit` rows, so `run_limited`, `execute_limited`, `scan_limited`, the
first-work estimator of `unionpush.rs` and `limited_input` are deleted. The LIMIT
pushdowns of item 7 into SERVICE, text search and prefix-output property functions stay
in the planner. `Dataset::ask_streaming_with` folds into `ask`, since it has no caller.

The regression tests that fail before the change are a test in `plan_fidelity_tests.rs`
that EXPLAIN of a variable-predicate query is the same under `execution=eager`,
`streaming` and `auto`, a test in `tests/union_planning.rs` that a UNION under LIMIT with
an expensive first branch answers in the time of its cheap branch in eager execution, as
the cursor does, and a test that an ASK over a plain basic graph pattern under automatic
execution reports a cursor plan rather than an eager one. The edge cases are a LIMIT whose
offset exceeds the answer, a LIMIT over a UNION whose branches share no variable, an ASK
whose pattern calls SERVICE, which keeps draining its input, an ASK with initial
bindings, and a LIMIT over a `PatternScan` that stops at the first block.

Its gates are `plan_fidelity_tests.rs`, `tests/union_planning.rs`,
`tests/exists_auto.rs`, `mise run test:w3c:streaming` with no new eager fallback, the
differential tests, the performance gates on `export-500k`, `values-star`,
`class-predicates`, `class-predicate-list`, `predicate-subjects`, `not-exists` and
`exists-join`, and the ASK shapes whose eager and streamed times the
[bounded filtered ASK admission](../X05-streaming-query-execution.md#bounded-filtered-ask-admission-2026-10-22)
of X05 recorded. The matrix has no ASK query, so the phase adds those ASK shapes to its
atlas A/B and applies the Intel rule to them and to the `PatternScan` queries under
0.3 ms.

### Item 829: native leaf cursors for the remaining fallback kinds

The phase follows item 827 and lands in the admission code it changes, and its operators
that build over their input take item 825's hash kernel. Its routine half suits a worker
with this plan as its specification, and its input-driven half needs judgment.

The phase classifies each node by its form rather than its kind, with the `needs_input`
method of its specification. A form that reads no input, a SERVICE without input
variables, a text search with a constant query string, a vector search with a constant
vector and no candidate join, a hybrid search, history changes, a view read, an algorithm
call, a spatial scan with constant arguments or a path search between a constant source
and a constant target, and `CountJoin`, answers a whole table from the index or a remote
endpoint. Under [decision 2](#decisions), a text, vector, hybrid or spatial search
bounded by `k` or `maxHits`, an algorithm call and a form under a LIMIT the planner
pushed down become charged materialized leaves, which run their kernel once into a
`Buffer` charged to the budget and serve it batch by batch as `State::Fallback` without
inputs does today. Each such leaf shows its peak memory against its budget on a
measurement before it counts as native. SERVICE and history changes get resumable
streaming readers instead, since a remote response already arrives as a stream and a
change history is ordered. Each form gets a `State` variant with its reason string,
`RejectMaterialization` then refuses a materialized leaf whose answer is not bounded by
`k`, `maxHits` or a pushed-down LIMIT, and the subtree fallback goes for these kinds.

A form that reads its input, a SERVICE or text, vector, hybrid, history, algorithm or
spatial node whose `needs_input` is true, remote LATERAL, path search with input, a path
of an unsupported shape and a registered property function without `prefix_output`,
becomes an expanding operator in the manner of `cursor/expand.rs`, which runs the eager
kernel over each input batch and serves the result where a batch's output is the output
of its rows. A text search groups the distinct query strings of a batch as the eager
kernel groups them, a SERVICE keeps its correlation with its input rows, and a registered
property function without the declaration still collects its whole input, since its calls
may have effects. A vector or hybrid search whose candidates come from its input is not
such a form, because `exec::vector_search` collects the candidate subjects of the whole
group before it asks for its global `k`, so two candidates split across batches would
both be returned where one search returns the winner. That form accumulates its
candidates across the input's batches under a charge, ranks them once when the input is
exhausted, and then serves its `k` results, as a sort does. A form whose eager kernel
needs the whole input for another reason, such as a spatial join whose build side is the
input, builds over the input's batches as the hash kernel of item 825 does. An
index-order top-k without the heap takes the heap of `cursor/topk.rs` with its runtime
fallback.

After the phase, every kind has a cursor, and an EXISTS pattern can still fail to plan
or fail its strict construction. `exists_streams` plans each pattern without a LIMIT of
its own and builds it under the strict policy, and it turns every planning or
construction error other than cancellation, a timeout and a budget failure into a
fallback. After the phase it therefore still answers a fallback in two cases. One is a
pattern that does not plan, such as one over an embedding provider that is not
configured, which fails in eager execution too. The other is a pattern whose leaf the
strict policy refuses. [Decision 2](#decisions) admits only bounded materialized leaves. If the strict policy admits every leaf, or
admits a leaf inside an EXISTS pattern whatever its bound, `State::Fallback` goes in this
phase. If the strict policy refuses an unbounded materialized leaf inside an EXISTS
pattern, the fallback remains for that one shape, an EXISTS over a SERVICE or search
without a bound, as a maintained limitation that the streaming record names, and the
merge removes it with the eager executor. In both cases
`mise run test:w3c:streaming` records no eager fallback for the W3C query suites, which
hold no such shape. The EXISTS shapes that automatic execution keeps eager through
`streams_natively`, an EXISTS deeper than four levels, a LeftJoin condition with EXISTS
and an EXISTS inside a larger expression, are item 7's remaining work and an admission
choice rather than a fallback, and the merge removes the admission with the mode.

The regression tests that fail before the change are a test in `tests/query_cursor.rs`
that a streaming request over each former fallback form returns a cursor plan without a
materializing subtree, a differential test per form at a batch size of one, and a test
that a vector search with `k` of one over candidates split across two input batches
returns the one global winner. The edge cases are a remote SERVICE that fails after its
first batch, which aborts the transfer as X05 specifies, a text search whose hits exceed
`maxHits`, a spatial join with an empty input, a view read during a view rebuild, and an
algorithm call inside a transaction.

Its gates are the W3C streaming count, the differential tests, `tests/exists_service.rs`,
`tests/service_enhancer.rs`, `tests/geo_join.rs` and the text and vector test suites,
and a performance comparison of its base and its tip on atlas over the queries of
`scripts/bench-text.sh`, `scripts/bench-geo.sh` and `scripts/bench-vector.sh`. Those
scripts compare engines and index variants and send no `execution` parameter, so the
phase first gives them an `EXECUTION` setting that their server requests carry, as
`scripts/nsc-bench-remote.sh` has, carried by their timing requests and their
correctness requests alike, and then runs each at its standard scale with 12 threads and
with one, in both modes, with the forge rule's 5% as its threshold on wall time, since
the leaves write while they read. None of the three scripts checks answers against a
fixed expectation. The text script takes its reference from the agreement of the engines
it compares, the geo script compares its index and rewrite variants and reports a
disagreement without failing, and the vector script measures each run's recall against
exact answers it computes itself. The phase therefore gives each script a fingerprint of
its answers, the exact answers for the vector script, that the base, the tip and both
modes must share, and a disagreement invalidates the timings of that run. The matrix has
no query of these kinds, so this comparison is the phase's evidence.

### The reference evaluator, item 512

The test-only evaluator that follows the SPARQL algebra literally becomes the oracle of
the differential tests before the eager executor is deleted, since the tests then have no
second executor to compare with. It is routine work with an exact specification, the
SPARQL 1.1 algebra, and its semantics are validated on the W3C suites by the planner. It
runs in parallel with stage 5 and must land before the merge phase starts. Its gate is
that it agrees with the engine on every W3C query test and on the seeded queries of
`tests/query_cursor.rs` at both modes.

### The merge

The merge phase gets its number when items 825 to 829, 512, 243, 293 and 534 have landed
and the remaining work of item 7 is done or decided. It is the design's single
morsel-driven executor, and it is judgment-sensitive throughout.

The phase makes `collect()` the eager API. `QueryCursor::collect` keeps its contract, a
cursor that has emitted rows cannot be collected, since earlier batches cannot be
recovered. Collection is defined for the three forms of `QueryExecution`, a SELECT
cursor into a `QueryResult`, an ASK cursor into its boolean through `finish_ask`, and a
graph cursor into a graph result, and `execute_query` and `query_with_plan` become the
collection of `query_cursor`, with `Dataset::query` and the bindings' eager methods on
top. Every direct caller of `exec::execute` and `exec::execute_owned` moves to a
collected cursor in this phase, `reasoning::seed_values`, `exists::build`,
`plan::eval_exists`, the update path of `update.rs` and the algorithm service among them.
Collection asks for unbounded demand, so a scan serves its whole range as one morsel, a
join probes it in parallel pieces, and the operators keep the Rayon they have inside
kernels. This is the parallelism eager execution has today, and exchange
operators and partitioned hash builds across pipelines follow the merge as item 857. The phase then deletes `run_node`
and the eager executor, the admission layer from `auto_streaming` to
`fits_without_spill`, `retry_as_cursor` with the `eagerMemoryRetry`
plan counter and the `sparkles_spill_eager_retries_total` metric, the mode branch of
`query_execution_with_plan`, `Accounting::Collect`, the `streams` flag and its
bounded-reads test, the `execution` parameter's effect on how a query runs, and the mode
history of the in-place quick runner, which keeps its choice between the receiving worker
and the blocking pool. The eager context's retention at its admitted high point becomes a
lease on every table, as stage 4 anticipated.

The gates are the whole set. The W3C suites pass with the default and with
`execution=streaming`, which then differ only in the response's commit point. The
differential tests run against item 512's evaluator and the answer fingerprints of the
forge matrix. The forge matrix of the merged build is compared with the baseline records
under both mode keys, so that the merged engine must be within 5% of the baseline's
eager time and of its streaming time for every query at 12 threads, which is the design's
oracle of the faster mode, and no query under 0.3 ms may be slower under the Intel rule.
The memory benchmarks of `docs/benchmarks/memory`, the spill matrices, `bench:incr`,
`bench:reasoning-latency`, `bench:insn` and `bench:bindings` run as the design lists. Before it
lands, the merge runs the same matrix on atlas, which has forge's hardware, as
[decision 8](#decisions) records, and forge runs it again after the push.

### Demand seeding inside the planner and one algebra visitor

This phase makes demand a planner-integrated operator that takes its seeds from the real
plan, so that `reasoning::apply` stops ordering joins of its own and `seed_values` stops
running earlier patterns twice, and it shares one `GraphPattern` visitor and fold across
the about 45 traversal functions in 28 files that the audit counted, from the planner's
`collect_pattern_vars` to the MCP tools' `has_service`. It depends on item 190 and gets
its number when item 190 settles the per-view cost of the demand stages. It does not
block the merge, and `seed_values` reaches the merge as a collected cursor without it.
Its visitor half is routine work once the visitor's shape is designed.

## How the gates are measured

### The forge rule

No query is 5% or more slower on forge at 12 threads. Before a phase lands, it runs the
31 queries of the matrix in both modes at 12 threads and with one on atlas, which has
forge's hardware, as an interleaved A/B of its base and its tip through
`scripts/nsc-bench-remote.sh` with `EXECUTION` set per cell, in at least eight rounds of
A,B,B,A in each order, and treats a confirmed 5% as the gate. After the push of the batch
that ends a stage, the coordinator runs the matrix on forge with a binary built elsewhere
inside `nix develop` without learned incremental reuse, as item 677 records. The build
host writes the build configuration with
`scripts/bench-gate.py build-config --cargo 'build --release -p sparkles-server --bin sparkles'`
in the build's own environment, since the command reads the checkout, the environment and
Boxington's policy where it runs and cannot recover them from a binary, and the JSON
travels with the binary to forge, where
`mise run bench:executors -- --commit REV --build-config "$CONFIG" BIN` runs under the
host's benchmark and heavy locks. The record is compared with the baseline through
`scripts/bench-gate.py compare BASELINE RECORD --keys '/exec$'`, then
`--keys '/wall$' --floor 5` for the large answers and `--keys '/wall$'` without the floor
for the rest, since the response cost that item 534 follows lies outside the execution
time and a wall time of a few milliseconds still counts. The baselines are the forge
record after stage 4, `docs/benchmarks/synthetic/runs/2026-11-02-executor-matrix-forge-9d086646`,
for the 12-thread cells, and the record of
`docs/benchmarks/synthetic/runs/2026-11-02-executor-matrix-forge-e52937ca` for the
one-thread cells, until a later stage's record replaces them. The record of e52937ca
holds the 12-thread cells as well, so the comparison with it qualifies its keys by cell,
`--keys '^1\.05m-1t/.*/exec$'` and the like for each one-thread cell, and the comparison
with the record of 9d086646 uses the suffix patterns above. The two records differ in
their run settings and in the digest of Boxington's policy, so a comparison with either
passes `--allow-different`, lists the differences it accepted beside the result, and
keeps every answer check, and a new baseline recorded after a stage removes the need.
`compare` pairs metrics by key, so a comparison under both mode keys is how the merge is
held to the faster mode. A flagged query is rerun as an interleaved A/B of the two builds
on the same host and counts as a regression only when the A/B confirms it. A regression a phase accepts gets
its own roadmap item in the same change, and a regression the forge record confirms after
the push is bisected across the batch and fixed forward. Raw records stay immutable, and
every answer check counts in every comparison.

### The Intel rule

No query that runs under 0.3 ms is slower on an Intel host. Each phase measures on atlas,
with the host to itself, through the harness of X08's latency gate. That harness,
`scripts/x08-gates/latency.sh`, takes one binary and compares its query pool on and off,
so the first phase that needs it gives it a second form that takes a base and a tip
binary as A and B with the same `scripts/nsc-bench-remote.sh` settings, the two builds
sharing one store, the server pinned to CPU 2 and the client to CPU 4 with one Rayon
thread and idle spinners, rounds of A,B,B,A in both orders with 200 to 300 requests per
round and at least twelve rounds, and `scripts/x08-gates/combine.py` combining the
orders to cancel the position bias. A query passes when the upper end of the 95% interval
of its ratio is at most 1.02 or its median added time is at most 0.5 µs. Repeated runs
differ by up to 4% on queries under 15 µs, which the absolute bound covers. The queries
are every query of the matrix that runs under 0.3 ms in either mode at 1.05M triples,
which the stage 4 record lists, and the phase names the ones its change can reach. A
difference that survives is profiled with `perf stat` and LBR call stacks on atlas before
the phase reports, as stage 4's per-query costs were.

### Correctness and conformance

The differential tests of `tests/query_cursor.rs` run their seeded queries at batch sizes
1, 2, 3 and 4,096 against the eager answer, and after the merge against item 512's
evaluator. `charged_state.rs` compares both modes under budgets, `opt_tests.rs` compares
every optimization with `Optimizations::NONE`, and `plan_fidelity_tests.rs` checks that
EXPLAIN matches what ran. `mise run test:w3c` and `mise run test:w3c:streaming` pass, and
the streaming record's count of eager fallbacks does not rise in any phase and reaches
zero for the query suites after item 829. `scripts/ci.sh --fast --changed main` runs for
every phase, with the binding tasks of the bindings a phase touches.

### Resources and durability

The plain and encrypted spill matrices of the batch gate run for every phase that changes
the files `scripts/gate/select-checks.py` lists, which include `exec.rs`, `cursor.rs` and
`spill.rs`. The memory tests of `ctx.rs`, `query_memory.rs` and
`tests/eager_memory_retry.rs` pass, with their expectations changed only where newly
shared behaviour explains the difference, and the memory benchmarks of
`docs/benchmarks/memory` run at the end of each stage. No phase changes an on-disk
format, and `crate::spill` keeps its version.

### The other gates

`mise run bench:incr` runs when item 826 moves the view operator,
`mise run bench:reasoning-latency` runs for any phase that changes value decoding or the
query path of query-time reasoning, which item 828 and the merge do, and
`mise run bench:insn` runs for any phase that changes vocabulary readers, which none of
them plans to. `bench:bindings` runs for items 828 and 829 and for the merge.

## Judgment and routine work

Items 825, 826 and 827 and the merge phase need substantial engineering judgment, since
they change planner and executor cost decisions, and their validating planner implements
them. Item 678, item 512, the expanding operators of item 829 for forms whose eager
kernel already runs per batch, and the visitor half of the demand phase are routine work
with this plan and the design as their specification, with the planner validating item
512's semantics. Item 828 is routine once items 534 and 240 have written down where the
time goes, and item 534 itself is performance work for a capable implementer on atlas.

Items 678, 512, 534 and 825 run at the same time with item 827 within the ownership
boundaries above. Items 826 and 829 follow their predecessors in the order of work. No two
phases run the Namespace screen of one commit at once.

## Decisions

The maintainer took these answers, and the phases above follow them.

1. The merge keeps whole-input morsels with Rayon inside kernels, the parallelism eager
   execution has today. Exchange operators and partitioned hash builds across pipelines
   follow as item 857, measured on forge. The merge's gate at 12 threads guards against
   the merged engine losing eager execution's parallel speed.
2. A charged materialized leaf counts as native only for a form whose answer is bounded,
   a text, vector, hybrid or spatial search by `k` or `maxHits`, an algorithm call, or a
   form under a pushed-down LIMIT, and only after a measurement shows its peak memory
   against its budget. SERVICE and history changes get resumable streaming readers.
   `RejectMaterialization` refuses an unbounded materialized leaf.
3. Item 826 defines `retract` on the accumulator trait and leaves the view operator of
   `incr/ops/group.rs` to item 418, so `bench:incr` is not a gate of item 826.
4. The frontier kernel and the algebra visitor stay in X14 as phases after the merge,
   numbered when items 8, 51 and 190 allow.
5. The merge removes `retry_as_cursor`, the `eagerMemoryRetry` plan counter and the
   `sparkles_spill_eager_retries_total` metric, and records the change in `docs/API.md`
   and the server's metrics documentation.
6. The merge reduces the in-place quick runner to its scheduling choice, the receiving
   worker against the blocking pool, keyed by the same history of run times, and items
   827 and 829 leave it alone.
7. ASK keeps one unchunked response body with its status code in every mode, and the
   Jena binding's context symbol keeps its meaning.
8. Forge runs the matrix after the push of the batch that ends each stage, after items
   826 and 829, and after the merge. Every phase runs atlas A/Bs of the matrix queries
   before it lands. The merge also runs the full matrix on atlas before it lands, since
   it deletes the eager executor and atlas has forge's hardware.
9. The bounded-reads test counts direct reads of the mode flag as well as `streams()`
   calls, so its bound is eight after item 678 and six after item 827.
