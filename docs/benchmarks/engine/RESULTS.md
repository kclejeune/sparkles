# Engine features and overheads results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Engine features and overheads

These sections measure single features of the engine, mostly with Sparkles alone.

### Query execution features

Executor optimizations can be switched off one at a time with
`SPARKLES_DISABLE_OPTIMIZATIONS`. This comparison ran the warm 10.5M suite with each of 33
switches off in turn and once with the defaults, and every configuration passed the answer
checks. The table lists the changes larger than 25%. Id ranges for a fixed start can no
longer be switched off, and the `filter_id_ranges` row records the run before that.

| Feature disabled | Effect at 10.5M |
|---|---|
| `anti_join` | `minus` 2.6× slower (22.8 against 8.9 ms) |
| `batched_join` | `values-star` 4.7× slower (18.9 against 4.0 ms), `employee-docs` 1.6× slower (6.6 against 4.2 ms), `star-lookup` 2.9× slower (12.2 against 4.2 ms), throughput 112 against 242 q/s |
| `batched_paths` | `knows-reach` 2.6× slower (382.5 against 145.6 ms) |
| `count_filter_runs` | `lang-filter` 1.7× slower (11.1 against 6.6 ms), `contains` 1.8× slower (12.7 against 7.1 ms), `regex-iri` 1.5× slower (12.9 against 8.4 ms) |
| `count_join_runs` | `two-hop-count` 2.1× slower (34.5 against 16.7 ms) |
| `decorrelate_exists` | `exists-join` 196× slower (2835.7 against 14.5 ms), `not-exists` 68× slower (974.3 against 14.4 ms), `expr-order-key` 1.3× slower (26.3 against 20.9 ms) |
| `expr_cache` | `expr-order-key` 3.7× slower (78.2 against 20.9 ms), `expr-agg-arg` 2.0× slower (93.3 against 45.9 ms), `expr-bind-group` 5.0× slower (46.0 against 9.2 ms) |
| `filter_id_ranges` | `expr-order-key` 1.3× slower (26.7 against 20.9 ms) |
| `gallop_index_join` | throughput 124 against 242 q/s |
| `incremental_group` | `group-avg` 1.6× slower (83.5 against 53.6 ms) |
| `metadata_counts` | `predicate-counts` 2.0× slower (7.7 against 3.9 ms), `distinct-obj` 1.8× slower (6.9 against 3.8 ms), `expr-order-key` 1.4× slower (28.2 against 20.9 ms) |
| `ordered_topk` | `range-topk` 1.4× slower (21.2 against 14.8 ms), `expr-order-key` 1.3× slower (26.7 against 20.9 ms) |
| `range_pushdown` | `optional-chain` 1.3× slower (93.0 against 73.2 ms) |
| `topk_first_key` | `expr-order-key` 8.3× slower (174.1 against 20.9 ms) |

### The fixed cost of a plain query

A plain query charges a lock-free fast memory account and creates the full memory ledger
only when it calls a graph algorithm, keeps an output lease or moves to a cursor. The fast
account adds a mutex, four atomic counters and a lazily set grant to each query context.
Against the release before graph algorithms, the three smallest queries of the suite at
1.05M take 0.5 to 0.7 µs more execution time, spread over the operator result plumbing,
and `values-star` runs 1.4% more instructions per request. The rest of that gap is the
warm checks that the cold-read hints need, a hash per constant, a bit test per answer term
and one cache lookup per scan.

The constant prefetch remembers the hashes of up to 1,024 keys whose group it has
requested, in 8 KiB per vocabulary with a sparse index. It saves the search of every warm
constant for queries whose constants repeat and does nothing for a stream of new ones.
On such streams, 10,000 distinct entity lookups and a VALUES list of 4,096 IRIs at 1 and 8
clients, the warm paths keep instructions per request within 0.989 to 1.000 and wall time
within 1.005 to 1.018 of a build without the table and the other warm-path changes.

*Measured on forge at 1.05M, with 2,000 requests per server process for instructions and 8
rounds of 50 requests for times.*

### The cost of plan pattern references

The planner records the triple patterns each plan node reads, for live query profiling
([U04](../../specs/U04-live-query-profiling.md#phase-4-2026-10-23) Phase 4). It does so only when
the answer shows the plan, which on the query route means the Sparkles format. The table
gives `instructions:u` per request of a plain SPARQL JSON request at 10.5M, over 2,000
warm requests in two passes that agreed within 0.1%, for a build that still named the
patterns for every request and for the current one.

| Query | Naming patterns for every request | Naming them only for a shown plan |
|---|--:|--:|
| `count-all` | 329.6 thousand | 329.0 thousand |
| `path-plus` | 321.4 thousand | 320.0 thousand |
| `types-grouped` | 659.8 thousand | 659.1 thousand |
| `values-star` | 1,154.2 thousand | 1,150.9 thousand |
| `star-lookup` | 2,439.0 thousand | 2,433.9 thousand |

*Instructions were counted on atlas, an Intel Core i5-13500, with both binaries built in
the Nix development shell, before the result buffer of the next section.*

In 16 pinned rounds of 200 SPARQL JSON requests on forge, `star-lookup` took 2.0% less
client time without the references, 0.559 ms against 0.570 ms, and was faster in all 16
rounds. `values-star` took 0.6% less, and `count-all`, `path-plus` and `types-grouped`
moved by 0.7% or less, which is within their noise at about 0.27 ms of client time. A
request in the Sparkles format names its patterns and pays these instructions, since its
answer shows them.

### The cost of result serialization

Eager answers are serialized into an 8 KiB buffer of the engine and written to the response
in pieces of that size. The serializers are compiled with the engine, so their speed does
not move with changes to the server, as [Benchmarking](../../BENCHMARKING.md#release-build-settings)
explains. The table gives `instructions:u` per request of a plain SPARQL JSON request at
10.5M, over 2,000 warm requests, for a build whose serializers wrote to the response writer
a few bytes at a time and for the current one.

| Query | Writing to the response writer | Writing to the buffer |
|---|--:|--:|
| `count-all` | 329.6 thousand | 316.6 thousand |
| `path-plus` | 320.6 thousand | 308.5 thousand |
| `types-grouped` | 660.1 thousand | 520.2 thousand |
| `values-star` | 1,152.4 thousand | 895.3 thousand |
| `star-lookup` | 2,437.8 thousand | 1,616.8 thousand |
| `subquery-agg` | 15,383.0 thousand | 15,271.0 thousand |

*Instructions were counted on an Intel Core Ultra X7 358H laptop, with both binaries built
in the Nix development shell.*

On forge, in 8 pinned rounds of 100 requests, the 300,191 rows of `optional-chain` reach
the client in 150.8 ms against 220.4 ms, and every round was faster. `star-lookup` takes
0.496 ms of client time against 0.526 ms, and `count-all`, `types-grouped` and
`values-star` stay within 1.2%. A screen of all queries on forge, at 8
rounds of 20 requests, found client time 19% lower for `order-by-full`, 30% lower for
`star-join` and 5% lower for `distinct-join`. `two-hop-count` keeps its speed. In 24
pinned A,B,B,A rounds with 40 requests per server process on forge, its server time was
27.63 ms against 27.68 ms for the build before the buffer, and its client time was
28.32 ms against 28.37 ms,
with the ranges of round medians overlapping.

The buffer remembers where each row ends, so an answer that stops partway reports the rows
whose bytes the response writer took. That accounting costs 7% of the instructions of
`optional-chain`, 3.7% of those of `star-lookup` and 2% of those of `values-star` against
a buffer that counts rows only when it is written out, and on forge `optional-chain` took
150.8 ms of client time against 132.0 ms without it. Roadmap item 243 follows this up.

A pinned forge comparison of the combined feature changes, with 16 A,B,B,A rounds and
100 requests per server process, found `count-all` server time 12.5% higher at 6.0 µs
against 5.3 µs and `distinct-obj` 14.5% higher at 4.2 µs against 3.6 µs. Every round was slower for both
queries. Client time stayed within 1.2%, at about 0.30 ms. Item 306 tracks isolating and
removing the server-time cost.

A diagnostic A,B,B,A comparison of the same builds counted about 11% fewer user-space
instructions for each complete HTTP request in the later build. It used 20,000 requests
per query and server process over persistent connections, with the server on CPU 2, the
client on CPU 4 and one Rayon thread on forge. The answers and operators agreed. These
counters include parsing, planning and serialization, so their reduction does not
establish that execution itself does less work. The index-count routine has the same
instruction sequence in both binaries after addresses are normalized. Checks of the
intermediate builds did not isolate a single responsible change. Item 306 remains open
for a measurement confined to execution and its plan bookkeeping.

### The cost of the query memory pool

The query memory pool of [X08](../../specs/X08-disk-spill-and-server-memory.md#gates) is on by
default, and its latency gate asks that it add at most 2% or 0.5 µs to each query under
0.3 ms. One build ran with the pool on and with `--total-query-memory-mb 0` from one shared
store at 1.05M, in both orders at 20 rounds of 300 requests with one Rayon thread, and the
two orders were combined to cancel the bias of the second position.
`scripts/x08-gates/latency.sh` reproduces it. The pool is charged in steps of 64 KiB, so a
plain query can hold up to 64 KiB of the pool beyond its estimate.

| Query | Pool off | Pool on against off | 95% interval | Added time |
|---|--:|--:|---|--:|
| `distinct-obj` | 3.2 µs | 1.019 | 1.006 to 1.029 | 0.06 µs |
| `count-all` | 3.5 µs | 1.015 | 1.002 to 1.026 | 0.05 µs |
| `predicate-counts` | 3.7 µs | 1.032 | 1.020 to 1.039 | 0.12 µs |
| `types-grouped` | 6.4 µs | 1.053 | 1.041 to 1.068 | 0.34 µs |
| `path-plus` | 8.6 µs | 0.969 | 0.956 to 0.978 | −0.27 µs |
| `values-star` | 13.5 µs | 1.034 | 1.027 to 1.045 | 0.45 µs |
| `employee-docs` | 31.0 µs | 1.013 | 1.007 to 1.019 | 0.42 µs |
| `star-lookup` | 57.4 µs | 1.008 | 1.001 to 1.013 | 0.46 µs |
| `subquery-agg` | 190.3 µs | 1.002 | 0.999 to 1.004 | 0.32 µs |
| `minus` | 280.8 µs | 1.008 | 1.005 to 1.010 | 2.20 µs |

*Measured on forge.*

Every query passes. Repeated runs of the same comparison differed by up to 4% on queries
under 15 µs, and `path-plus` reads faster with the pool on, so ratios of these queries
below 1.05 are within the run-to-run spread.

### Planning large basic graph patterns

The ignored test `large_bgp_planning_time` in `crates/sparkles/tests/nesting.rs` runs
`explain` on each shape of `plan_shapes` at the 4000-level plan limit over a dataset of
four triples. [X05](../../specs/X05-streaming-query-execution.md#large-basic-graph-patterns) has
the design and the shapes.

| Shape at the limit | `explain` | Plan JSON |
|---|---:|---:|
| Chain of 1998 | 0.35 s | 29.7 MB |
| Reversed chain of 1998 | 0.27 s | 32.5 MB |
| Tree of 1998 | 0.34 s | 22.4 MB |
| Star of 1998 | 0.73 s | 15.1 MB |
| Hub of 1998 | 0.77 s | 29.8 MB |
| Pairs of 999 | 0.49 s | 18.1 MB |
| Chain of 999 with a second predicate | 0.19 s | 11.2 MB |
| Chain of 571 with VALUES | 0.11 s | 7.3 MB |
| Chain of 1019 with filters | 34 ms | 1.3 MB |
| Chain of 799 with OPTIONAL | 0.53 s | 13.7 MB |
| Chain of 799 with MINUS | 0.15 s | 4.8 MB |
| Chain of 799 with BIND | 0.18 s | 13.5 MB |

*Measured on the development laptop in a release build with other builds running, so the
times are good to about a fifth.*

A star of 1998 patterns on `foaf:knows` over the 31,815 triples of
`scripts/gen-data.py 3000` takes 2.6 to 2.8 s to plan, so the data decides how fast a star
plans ([roadmap](../../ROADMAP.md) item 84). The planner keeps the star masks and probed sides
with each plan, which costs up to 64 MB while the greedy order runs on a plan of 1998
inputs.

### Schema reports and Ask on DBpedia

These figures measure the MCP tools and `sparkles ask` on the 1.01 billion quads of the
DBpedia 2022.12.01 database, through `sparkles mcp` with a warm page cache.

| Measurement | Time |
|---|---|
| First `check_query` of a process | 0.27 to 0.30 s, or up to 1.5 s right after the binary changed |
| Later `check_query` calls in the same process | 16 to 60 ms |
| First `describe_schema` summary of 200 predicates | 1.32 to 1.4 s, and 11 ms repeated |
| Terms-list query of `check_query` with VALUES outside a UNION | 0.04 s |
| `sparkles ask` with the harness's fake provider | 1.9 s, at most 508 MiB of anonymous memory |

*Measured on atlas in single sessions.*

The process holds under 500 MiB of anonymous memory. The mapped index pages that the
sampling of predicate objects touches add about 2 GiB of reclaimable file-backed memory to
the resident size. The figures are for a database without changes since its build. A store
with changes corrects the statistics with one index probe for each changed value, and the
report scans instead when that would cost more than a scan.

### The Explore overview

These figures measure the class overview and the groups route of
[U06](../../specs/U06-large-graph-rendering.md) on the 10.5M-triple synthetic dataset. Each
class-profile figure is the first request after a server start, over five rounds, and the
groups figures are three rounds.

| Measurement | Time |
|---|---|
| Class profiles, first request | 0.95 to 1.62 s, median 1.12 s |
| Class profiles, repeated at the same commit | 0.5 to 0.8 ms from the cache |
| Open `Researcher`, 300,002 instances, grouped by salary ranges | 1.35 to 2.26 s, and 1.0 to 1.2 ms repeated |
| Open one salary range, 24,780 instances, grouped by age ranges | 0.39 to 0.47 s |
| `Researcher` grouped by `worksFor` | 0.39 to 0.68 s |
| Open `Company`, 13,333 instances, grouped by city | 0.11 to 0.12 s |
| Open `Article`, 375,000 instances, grouped by year ranges | 0.51 to 0.82 s |
| F14 Leiden over `foaf:knows`, 986,333 nodes and 5.0 million edges | 22.2 s |

*Measured on an 8-vCPU Namespace instance on 2026-10-23.*

The profile of each predicate is computed on its own rayon task, which halves the first
picture of the overview against a serial build, at 2.08 to 2.60 s. It uses more threads for
the length of one profile request, and the profile cache keeps up to 64 answers and 16 MiB
of JSON per server, shared with the groups route. The Leiden figure is why the groups route
does not run a community detection inside a request.

### Predicate patterns

Each generation keeps predicate patterns, from which Sparkles answers predicate listings
and per-predicate subject counts
([X11](../../specs/X11-index-compression-and-predicate-patterns.md#outcome)). The table compares
the same queries with and without the patterns, as medians of the server's execution time.

| Query | Without patterns | With patterns |
|---|---:|---:|
| DBpedia 50M, `COUNT(DISTINCT ?s)` per predicate over every subject | 1,915 ms | 1.3 ms |
| DBpedia 50M, the same for `dbo:Person` instances | 264 ms | 21 ms |
| DBpedia 50M, the same for `dbo:Film` instances | 24 ms | 11.7 ms |
| DBpedia 50M, `SELECT DISTINCT ?p` of `dbo:Film` instances | 17.4 ms | 10.8 ms |
| 10.5M, `predicate-subjects` | 469 ms | 0.020 ms |
| 10.5M, `class-predicates` | 508 ms | 10.2 ms |
| 10.5M, `class-predicate-list` | 73 ms | 10.2 ms |

*Measured on forge on 2026-10-23.*

The other queries of both suites gave the same answers and none above 0.3 ms was more than
5% slower. `predicate-counts` counts triples with `COUNT(*)`, which the patterns do not
answer. The class profiles endpoint takes 24.2 s on DBpedia either way, because almost
every DBpedia predicate is used by an instance of some class. The patterns take 24.1 MB,
1.6% of the index, on the DBpedia slice, and 2.9 MiB, 1.0% of the index, at 10.5M, where 24
patterns cover 1.52 million subjects. Writing them takes 0.36 s of the slice's 26.0 s load.
The collector holds at most 1 GiB of distinct patterns by default and skips the patterns
of a generation that needs more.

### FSST vocabulary suffixes

A load with `--vocab-codec fsst` compresses the front-coded vocabulary's suffixes with FSST
([X11](../../specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads)).
Front coding stays the default. Each variant loaded both datasets twice in A, B, B, A order.
Warm 10.5M cells are medians of four runs of 15 requests, warm DBpedia cells of two runs of
10, and cold cells of two runs of five cold samples. The geometric means cover the queries
at 0.3 ms or more.

| Measurement | 10.5M front | 10.5M FSST | DBpedia 50M front | DBpedia 50M FSST |
|---|---:|---:|---:|---:|
| `vocab.dat` | 25.87 MB | 17.69 MB | 485.06 MB | 344.48 MB |
| Store on disk | 312.8 MB | 304.7 MB | 1,577.5 MB | 1,436.9 MB |
| Mean load time | 3.8 s | 3.8 s | 24.0 s | 24.9 s |
| Warm `export-500k` | 105.6 ms | 115.7 ms | | |
| Warm `contains` | 12.77 ms | 13.31 ms | | |
| Warm `lang-filter` | 9.30 ms | 9.69 ms | | |
| Warm `star-join` | 28.86 ms | 30.40 ms | | |
| Warm `export-1m` | | | 347.7 ms | 366.4 ms |
| Cold `export-500k` | 131.4 ms | 134.1 ms | | |
| Warm geometric mean, FSST against front | | 1.012 | | 1.022 |
| Cold geometric mean, FSST against front | | 1.000 | | 0.990 |

*Sizes and load times were measured on forge on 2026-10-23, and query times on 2026-10-24
at commit c2518d7b.*

Load peak RSS stays within the spread between loads, at 2.0 to 2.1 GB at 10.5M and 3.1 to
3.3 GB on DBpedia. Warm `contains`, `lang-filter` and the export at 10.5M run 4% to 10%
slower with FSST, DBpedia's `abstract-contains` and `label-regex` 12% slower, and cold
`entity-summary-1` on DBpedia takes 1.17 times as long. [Roadmap](../../ROADMAP.md) item 160
follows up `star-join` and `entity-summary-1`, and item 241 follows up three warm DBpedia
queries that run 1.4% to 4.2% slower with front coding since item 108.

### Cardinality estimates

`scripts/bench-estimates/qerror.py` records every plan node's estimated and actual rows.
The q-error of a node is `max(est, act) / min(est, act)`, both at least one row, over the
nodes that ran to completion. The table compares the join nodes of the base build before
the correlation-aware estimates of [X15](../../specs/X15-join-algorithms-and-estimation/SPEC.md#outcome)
with the build that adds them. Both answered every query identically.

| Workload | Joins | Geometric mean | 90th percentile | Largest |
|---|---|---|---|---|
| Synthetic, 1.05M triples | 34 | 1.39 to 1.37 | 2.7 to 2.7 | 42 to 42 |
| WatDiv at scale 100 | 260 to 246 | 8.80 to 3.98 | 661 to 59 | 1,222 to 666 |
| BSBM explore, 10M | 136 to 135 | 1.89 to 1.86 | 5.6 to 5.6 | 18 to 18 |
| BSBM BI, 10M | 130 | 2.17 to 1.78 | 12.5 to 6.5 | 41 to 31 |
| LUBM(10) with OWL RL | 26 | 3.52 to 1.81 | 32 to 4.1 | 3,016 to 9 |
| DBpedia, 8.2M-triple slice | 22 | 4.84 to 1.96 | 78 to 7.7 | 1,478 to 78 |

*Recorded on the development laptop. Estimates do not depend on the host.*

Both set families keep 32,768 sets. The table compares smaller caps on WatDiv and the
DBpedia slice, with the q-errors from the laptop and the open time and memory from atlas,
an Intel host like forge. Open is the median of five server starts to the ready endpoint,
polled every 20 ms, and memory is the resident set once ready. At 16,384 sets the
geometric mean rises by 10% on WatDiv and 5% on DBpedia, and at 8,192 by 19% and 71%, so
the smaller caps give up much of the gain.

| Workload | Sets | Geometric mean | 90th percentile | Largest | `stats.json` | Open | Memory once ready |
|---|---|---|---|---|---|---|---|
| WatDiv at scale 100 | Base build | 8.80 | 661 | 1,222 | 2.5 MB | 0.10 s | 127 MB |
| WatDiv at scale 100 | 8,192 | 4.72 | 49 | 666 | 4.4 MB | 0.07 s | 133 MB |
| WatDiv at scale 100 | 16,384 | 4.36 | 31 | 666 | 9.3 MB | 0.10 s | 144 MB |
| WatDiv at scale 100 | 32,768 | 3.98 | 59 | 666 | 16.3 MB | 0.13 s | 156 MB |
| DBpedia, 8.2M-triple slice | Base build | 4.84 | 78 | 1,478 | 5.0 MB | 0.07 s | 133 MB |
| DBpedia, 8.2M-triple slice | 8,192 | 3.36 | 15 | 78 | 7.1 MB | 0.07 s | 141 MB |
| DBpedia, 8.2M-triple slice | 16,384 | 2.06 | 6.0 | 78 | 16.7 MB | 0.14 s | 157 MB |
| DBpedia, 8.2M-triple slice | 32,768 | 1.96 | 7.7 | 78 | 44.3 MB | 0.30 s | 214 MB |

The 90th percentile is lower at 16,384 sets than at 32,768 because the plans, and so the
joins measured, differ. Against 32,768 sets, the largest join error of WatDiv `F1-2` rises
from 17 to 49 at 8,192 sets and that of `F4-2` from 9 to 58, and at 16,384 sets that of
`F4-1` rises from 2.1 to 7.0. The node pass keeps the same sets at any thread count.

### Estimation timing on forge

`abtime.py` timed main without the correlation-aware estimates against the build with them
on forge, with every other benchmark held off by the bench lock. Both builds came from
the same host, wally, without learned incremental reuse. Each workload ran four rounds
of base, change, change and base. Each run was a fresh server pinned to 12 CPUs with 12
threads and the result cache off, and each query got 3 warm-ups and 20 timed requests. A
request's time is the server's planning and execution time, and the table gives the
geometric mean of the per-query ratios of the medians. The change ran on stores that it
loaded itself, and every query returned the same answers on both. The flagged synthetic
and DBpedia queries ran again in eight rounds. The
[run record](runs/2026-11-03-estimation-ab-forge-e8e0e4cd/record.json) holds every query's
medians, per-round ratios, changed plans and reruns, and its
[commands](runs/2026-11-03-estimation-ab-forge-e8e0e4cd/run.sh.txt).

| Workload | Queries | Change to base | Queries 5% or more slower |
|---|---|---|---|
| Synthetic, 1.05M triples | 45 | 1.028 | Five queries under 0.15 ms, `expr-agg-arg`, `f-expr-range-join` and `f-regex-iri-join`. |
| WatDiv at scale 100 | 88 | 0.968 | `F4-1` to `F4-5`, `S4-2` and `S4-4`. |
| BSBM explore, 10M | 33 | 1.000 | None. |
| BSBM BI, 10M | 24 | 0.933 | None. |
| LUBM(10) with OWL RL | 14 | 0.946 | None. |
| DBpedia, 8.2M-triple slice | 30 | 0.993 | Three queries under 0.1 ms, each within 2.5% in eight rounds. |

The plans of 41 queries change. WatDiv's `C` queries take 0.20 to 0.45 times as long,
BSBM BI `q1-3` 0.23 times, LUBM `q2` 0.51 times and DBpedia's `costar-birthplace` 0.63
times. WatDiv `S4-2` and `S4-4` take 1.6 times as long. The node sets estimate their join
of `artist` and `familyName` within 3%, and the planner then probes `familyName` with 704
keys, which costs about 0.1 ms more than the base plan's merge join with a scan of the
pattern. A cost term for keys spread at random brought the base plan back but slowed
other queries more, as the
[performance experiments](../../PERFORMANCE-EXPERIMENTS.md#query-execution) record, and
roadmap item NEW-ITEM-5 prices probes by how their keys spread.

| Workload | Query | Base ms | Change ms | Ratio |
|---|---|---|---|---|
| WatDiv | `C1-1` | 3.099 | 0.923 | 0.30 |
| WatDiv | `C2-1` | 5.140 | 1.050 | 0.20 |
| WatDiv | `C3-1` | 31.586 | 14.280 | 0.45 |
| WatDiv | `F2-1` | 0.266 | 0.267 | 1.01 |
| WatDiv | `F2-2` | 0.398 | 0.417 | 1.05 |
| WatDiv | `F2-3` | 0.314 | 0.316 | 1.01 |
| WatDiv | `F2-4` | 0.303 | 0.303 | 1.00 |
| WatDiv | `F2-5` | 0.258 | 0.259 | 1.00 |
| WatDiv | `F4-1` | 1.028 | 1.108 | 1.08 |
| WatDiv | `F4-2` | 0.996 | 1.068 | 1.07 |
| WatDiv | `F4-3` | 0.996 | 1.070 | 1.07 |
| WatDiv | `F4-4` | 0.899 | 0.965 | 1.07 |
| WatDiv | `F4-5` | 0.987 | 1.057 | 1.07 |
| WatDiv | `F5-1` | 0.229 | 0.230 | 1.01 |
| WatDiv | `F5-2` | 0.314 | 0.319 | 1.02 |
| WatDiv | `F5-3` | 0.279 | 0.284 | 1.02 |
| WatDiv | `F5-4` | 0.291 | 0.283 | 0.97 |
| WatDiv | `F5-5` | 0.242 | 0.238 | 0.98 |
| WatDiv | `S3-1` | 0.132 | 0.133 | 1.00 |
| WatDiv | `S3-2` | 0.125 | 0.123 | 0.99 |
| WatDiv | `S3-3` | 0.124 | 0.122 | 0.99 |
| WatDiv | `S3-4` | 0.122 | 0.122 | 1.00 |
| WatDiv | `S3-5` | 0.218 | 0.202 | 0.93 |
| WatDiv | `S4-2` | 0.167 | 0.272 | 1.63 |
| WatDiv | `S4-4` | 0.164 | 0.265 | 1.61 |
| WatDiv | `S5-1` | 0.169 | 0.166 | 0.98 |
| WatDiv | `S5-2` | 0.163 | 0.168 | 1.03 |
| WatDiv | `S5-3` | 0.163 | 0.169 | 1.03 |
| WatDiv | `S5-4` | 0.165 | 0.167 | 1.02 |
| WatDiv | `S5-5` | 0.163 | 0.169 | 1.04 |
| BSBM explore | `explore-q7-2` | 0.417 | 0.428 | 1.02 |
| BSBM explore | `explore-q8-3` | 0.291 | 0.295 | 1.01 |
| BSBM BI | `bi-q1-2` | 6.952 | 6.140 | 0.88 |
| BSBM BI | `bi-q1-3` | 86.681 | 19.725 | 0.23 |
| BSBM BI | `bi-q4-3` | 230.362 | 232.738 | 1.01 |
| LUBM | `q12` | 0.136 | 0.142 | 1.04 |
| LUBM | `q2` | 4.136 | 2.089 | 0.51 |
| LUBM | `q9` | 6.372 | 4.984 | 0.78 |
| DBpedia | `category-people-1` | 0.094 | 0.096 | 1.02 |
| DBpedia | `category-people-2` | 0.088 | 0.093 | 1.06 |
| DBpedia | `costar-birthplace` | 0.333 | 0.210 | 0.63 |

The other slower queries keep their plans. WatDiv `F4` and the synthetic and DBpedia
queries under 0.15 ms spend 1 to 90 microseconds longer in planning, 5% to 10% of their
time, on registering the sets of their variables and walking their plans for set masks.
Roadmap item NEW-ITEM-6 keeps that work across queries. In eight rounds `expr-agg-arg`
ran 0.99 times as long, the synthetic `f-regex-iri-join` 1.06 times and
`f-expr-range-join` 1.11 times, the last two in execution. The rounds of either build
ran `f-expr-range-join` in 1.7 to 2.3 ms or in 2.7 to 3.3 ms of execution, as the
record's per-round medians show, so its median depends on which rounds fall slow.
`two-hop-count` and `births-by-decade`, which an earlier pair of builds ran 1.15 and 1.13
times as long, ran 0.98 and 1.00 times as long here. Roadmap item NEW-ITEM-7 separates code layout from
code in these execution differences.

## Rewrite gate baselines

These baselines belong to the rewrite gates of [BENCHMARKING.md](../../BENCHMARKING.md#rewrite-gates).
A change that a gate covers compares its own gate record with them using
`scripts/bench-gate.py compare`.

### W3C suites under streaming execution

`mise run test:w3c:streaming` ran the query evaluation suites with `execution=streaming`
at commit fcff390a, whose engine code is that of 04d49b57. The suites came from the
Apache Jena checkout that the tests read by default. Every suite passed with no new
failures, and the debug test binary took 23 seconds on a busy development laptop.

| Suite | Passed | Failed | Skipped | Queries with eager fallback |
|---|--:|--:|--:|--:|
| SPARQL 1.0 and 1.1 query | 508 | 0 | 103 | 0 |
| SPARQL 1.2 | 63 | 0 | 206 | 0 |
| Jena's CDT, FOLD and UNFOLD tests | 655 | 0 | 0 | 30 |
| ARQ property functions | 48 | 0 | 0 | 24 |

The skipped tests are syntax and update tests, which do not depend on the execution mode
and run in the default pass. The fallback counts are the queries whose plans hold an
operator that the cursors do not stream, which then runs eagerly on first demand. These
counts are the measure that the executor merge drives to zero. The
[run record](runs/2026-11-01-w3c-streaming-stanley/record.json) and its
[log](runs/2026-11-01-w3c-streaming-stanley/output.log) hold the counts. `mise run test`
runs the same tests in CI with Oxigraph's GeoSPARQL tests added. A separate run with the
`geo` feature gave 37 passes and the 7 listed failures of those tests in both modes, with
no eager fallback. The streaming tests
replace the cursor pass at 4,096 rows that `sparql_query_cursors` used to run, so CI
runs about 10 seconds more of these tests than before.

### Instruction counts of vocabulary scans and WAL replay

`mise run bench:insn` counted the user-space instructions of the vocabulary readers and
of WAL replay on forge alone at commit 5d33aacb, whose engine code is that of 04d49b57.
The store held 20,000 items, and the WAL case replayed 2,000 commits. Each figure is the
median of five processes, each of which reports the median of five runs. Every result
check passed, and no case saw other threads use measurable CPU time.

| Case | Median | Lowest process | Highest process |
|---|--:|--:|--:|
| `vocab/front-coded/open` | 21,786 | 21,078 | 21,810 |
| `vocab/front-coded/for-each` | 8,535,784 | 8,535,784 | 9,436,303 |
| `vocab/front-coded/get-sorted` | 7,191,526 | 7,191,525 | 7,192,077 |
| `vocab/front-coded/get-random` | 60,908,283 | 60,908,283 | 62,325,401 |
| `vocab/front-coded/find-present` | 11,147,739 | 11,146,629 | 11,162,209 |
| `vocab/front-coded/find-missing` | 12,178,242 | 12,175,211 | 12,197,300 |
| `vocab/front-coded/prefix-range` | 1,877,678 | 1,877,214 | 1,878,995 |
| `vocab/fsst/open` | 29,285 | 28,551 | 29,648 |
| `vocab/fsst/for-each` | 13,235,608 | 13,235,608 | 13,235,609 |
| `vocab/fsst/get-sorted` | 8,642,304 | 8,642,303 | 8,642,304 |
| `vocab/fsst/get-random` | 57,289,681 | 57,289,681 | 57,289,683 |
| `vocab/fsst/find-present` | 14,541,328 | 14,536,664 | 14,566,444 |
| `vocab/fsst/find-missing` | 15,507,745 | 15,499,312 | 15,511,024 |
| `vocab/fsst/prefix-range` | 1,977,206 | 1,975,336 | 1,978,471 |
| `wal/open-without-wal` | 330,060,548 | 330,057,645 | 330,060,944 |
| `wal/open-with-2000-commits` | 394,099,730 | 394,093,307 | 394,105,567 |
| `wal/replay-2000-commits` | 64,039,182 | 64,032,609 | 64,045,438 |

Within one process the scans, lookups and store opens repeat to within a tenth of a
percent. The vocabulary opens count only 20,000 to 30,000 instructions and vary by up to
3.5% for front coding and 5.2% for FSST within a process. Between processes the full
front-coded scan moved by 10.5%, between 8,535,784 and 9,436,303 instructions, and its
random lookups and both vocabulary opens by 2% to 4%, as the addresses their data lands at
change. The other cases moved by 0.2% or less. The gate
flags a case only when a candidate's whole range lies above the baseline's, so these
cases catch a change larger than their spread. Replaying 2,000 commits cost 64.0 million
instructions, about 32,000 per commit.

The [run record](runs/2026-11-01-insn-screen-forge-5d33aacb/record.json) holds every
count, the result checks, the workload fingerprint, the compilers and the build
configuration, with the raw lines and logs of the five processes and the
[commands](runs/2026-11-01-insn-screen-forge-5d33aacb/run.sh.txt). The screen took
33 seconds on forge after its build.
