# Query optimization reference

[ARCHITECTURE.md](../ARCHITECTURE.md#query-execution-and-where-optimizations-live)
explains the query pipeline and where each optimization sits. This reference documents
operator applicability, cost-model parameters and the local measurements that motivated
each choice. Local timings describe implementation experiments and are not performance
guarantees; [BENCHMARKS.md](../BENCHMARKS.md) owns published engine comparisons and
methodology.

Toggleable switches are defined in
[`Optimizations`](../../crates/sparkles-core/src/sparql/ctx.rs) and can be disabled per
query (`QueryOptions::optimizations`) or per process
(`SPARKLES_DISABLE_OPTIMIZATIONS=range_pushdown,…`). Every fast path has an applicability
check and a generic fallback, and `EXPLAIN` reports which paths ran. The source is
authoritative whenever an operator or its cost model changes.

| Category | Toggleable switches (`SPARKLES_DISABLE_OPTIMIZATIONS`) |
|---|---|
| Cardinality & planning | `sampled_filters`, `characteristic_sets`, `set_remainders`, `node_sets`, `key_backoff`, `domain_estimates`, `probed_keys` |
| Filters & scans | `count_filter_runs`, `filter_scan_runs`, `expr_cache`, `range_pushdown`, `point_patterns` |
| Joins & stars | `anti_join`, `merge_left_join`, `merge_pair_conditions`, `probed_optional`, `keyed_join`, `keyed_left_join`, `batched_join`, `gallop_index_join`, `star_fusion`, `fused_star_costs` |
| Aggregation & top-k | `metadata_counts`, `topk_heap`, `topk_first_key`, `bound_order_keys` |
| Streaming | `predicate_patterns`, `grow_input` |

* [Optimizations adopted from QLever](#optimizations-adopted-from-qlever)
* [Further scan and filter optimizations](#further-scan-and-filter-optimizations)
  * [Filter and range pushdown](#filter-and-range-pushdown)
  * [Cardinality estimation and join ordering](#cardinality-estimation-and-join-ordering)
  * [Aggregation and top-k](#aggregation-and-top-k)
  * [Join operators, paths and star fusion](#join-operators-paths-and-star-fusion)
* [Cold reads](#cold-reads)

## Optimizations adopted from QLever

* **Ids with inline values.** The top 4 bits hold a tag and the low 60 bits a payload.
  UNDEF is 0, so it sorts first. Canonical, losslessly representable numbers and
  booleans can be inline. Other literals stay in the vocabulary.
* **Sorted vocabulary.** Base-vocabulary id order follows sorted term keys, so prefix
  and range restrictions become id ranges (`Vocab::prefix_range`). Delta-vocabulary ids
  instead follow insertion order and are handled separately.
* **Permutation files.** Blocks store each column separately (delta, zig-zag varint,
  LZ4). Each block's first and last key stay in RAM, which allows block skipping on
  bound prefixes and exact counts with at most two block decodes.
* **Segmented blocks.** A column is a run of LZ4 frames of 2,048
  rows each, whose deltas restart at every frame, and `<perm>.seg` holds each frame's
  first key and length (X07 §6.5). A reader of a few rows of a block, such as an index
  join's probe, a short range scan or the count of an edge block, decodes only the frames
  that can hold them, 16 KiB per column instead of 256 KiB. A reader of most of a block
  decodes all its frames into one column as before. A lookup uses a block's columns
  whole when the cache already holds them. The cost is index size. At 10.5M triples the
  permutation files grow 3.3% and the whole generation 3.0%, from 300.6 MB to 309.7 MB.
* **Bulk build pipeline.** Parallel chunked parsing produces per-batch vocabularies. They
  are merged into one in parallel, over ranges of keys that sampled keys delimit. The
  batches are then read once, in chunks that are remapped to global ids in parallel and
  sorted in place by SPO, OSP and PSO, each order written as a compressed sorted run.
  The runs of each order are merged in a thread of their own. As in QLever, the
  permutations are built in pairs: SOP, OPS and POS come from the merged SPO, OSP and PSO
  streams by sorting each run of the shared first column. Data that fits into the sort
  budget is sorted in memory the same way. The temporary files are compressed too. The
  batches' quads use the column blocks of the runs, the partial vocabularies are
  front-coded LZ4 blocks, and the maps from batch ids to global ids are delta-coded.
* **Immutable base plus delta.** Updates are layered on the immutable index, in the style
  of QLever's `DeltaTriples`. Snapshots are versioned, and caches are keyed by snapshot
  version. A scan merges the delta into the blocks it changes. It finds the base rows
  between two delta keys by binary search, and only those blocks have every column
  decoded. The reads that visit index blocks in parallel, such as the runs of the count
  and filter operators below, copy each changed block with its delta keys merged in, so
  uncompacted commits keep those reads parallel. The copy holds only the columns that the
  read asks for, and lives until the read's task for that block ends. These copies are
  not charged to the query's memory budget, so a read whose ranges hold more than 65,536
  delta keys merges them in order instead, as a scan does.
* **Rebuilds in the background.** Like QLever's index rebuild, a compaction builds the
  new index from a snapshot while updates continue, then carries the updates made since
  the snapshot into the new index with their ids remapped. Sparkles carries each commit
  into the new generation's log, so the commits made during the build stay readable at
  `?at=`. The automatic trigger takes QLever's `min`, `max` and `fraction` form
  (`--rebuild-index-strategy automatic:min:max:fraction`), adds the size of the log, age
  and idle time, and waits for bulk loads, backups and the retention window.
* **Columnar execution and planning.** Execution is column-major. The planner orders joins
  with QLever's dynamic program, which keeps the cheapest plan per subset of patterns and
  sort order, and merge joins run on sorted scans. A hash join's output keeps the order of
  the input it probes, so a merge join above it reads it as sorted. Groups too large for
  the program are planned in rounds or greedily, as described under join ordering on cost
  summaries below.
* **Decoded-block cache.** A shared cache of decoded blocks, weighted by bytes.
* **Result cache.** Executed subtrees are cached under a canonical plan key and the
  snapshot version, so updates invalidate entries without extra work. Results with
  query-local terms or non-deterministic functions are not cached. Each operator reports
  `cached` in the plan.
* **GROUP BY + COUNT from index runs.** When the group key is a scan's sort column, counts
  come from runs in the blocks. The scan is never materialized.
* **Planner details.** Filters are placed as soon as their variables are bound. Scan sizes
  are exact from block metadata (at most two block decodes). Joins are estimated from
  characteristic sets or probed values where those apply (see below), and otherwise from
  per-predicate distinct subject and object counts with QLever's 0.7 correction factor. A
  pattern with a single free subject, predicate or object has a distinct value of it per
  row. Costs are counted in rows read by a scan. A hash join costs 8 of them for each row
  of its smaller input, which goes into the hash table at 3 to 15 ns a row, and one for
  each row of the larger input that probes it. A scan reads a row in 1.3 to 2.2 ns. Merge joins gallop through skewed
  inputs. `COUNT(*)` over one pattern comes from index
  metadata. Transitive paths traverse from the bound side, with index lookups per
  frontier node, instead of materializing the closure.
* **Executed-plan feedback.** Every query returns a runtime-information tree (estimated
  and actual rows, time per operator), like `qlever-json`. The UI renders it.

## Further scan and filter optimizations

Each entry that names a switch can be disabled per query (`QueryOptions::optimizations`)
or per process (`SPARKLES_DISABLE_OPTIMIZATIONS=…`). Entries without a switch replaced an
earlier implementation and always apply. `EXPLAIN` shows which fast paths ran.

### Filter and range pushdown

* **`COUNT(DISTINCT ?v)` from index runs.** Over one triple pattern, the scan switches to a
  permutation sorted on `?v` and counts runs of equal IDs (`CountDistinctFromIndex`) without
  materializing or hashing rows. When the pattern is `?s ?p ?o` or binds only its predicate,
  the count comes from index statistics instead (`CountDistinctFromMetadata`, part of
  `metadata_counts`).
* **Filters on vocabulary keys.** `CONTAINS`, `STRSTARTS`, `STRENDS` and `REGEX` over `?v`
  or `STR(?v)`, `STRLEN` comparisons with integer constants, and
  `LANGMATCHES(LANG(?v), …)` test stored key bytes (`"lexical 0xFF @lang`, `<iri`) directly.
  Each front-coded block is scanned once in parallel without allocating a string per term;
  a `CONTAINS` needle builds its substring finder once, and each parallel task holds its
  own regex copy to avoid match-cache contention. Passing keys must be valid UTF-8, and
  `STRLEN` counts Unicode scalar values while preserving literal argument rules. Delta terms
  are tested on delta keys, while inline values (numbers, dates) use the general evaluator.
* **Filtered counts from index runs** (`count_filter_runs`). `COUNT(*)`, `COUNT(?x)` and
  `COUNT(DISTINCT ?v)` over a FILTER reading only `?v` of one triple pattern read the
  permutation sorted on `?v`, test each run of equal IDs once, and sum passing run lengths
  or count passing runs (`CountFilterFromRuns`) without materializing rows. Blocks are read
  in parallel in 8,192-row pieces with changed blocks merging their delta; union-graph dedup
  and oversized deltas fall back to sequential runs. In local measurements at 10.5M triples,
  counting `foaf:name` values containing `"Ada"` (1.02M names) takes about 4 ms versus 14 ms
  when materializing the column first.
* **Filters on the runs of a scan** (`filter_scan_runs`). A FILTER directly over a scan
  sorted on a tested variable evaluates single-variable conjuncts once per distinct value,
  then copies passing rows out of index blocks in parallel and applies remaining conjuncts.
  `EXPLAIN` notes `[runs of ?v: N values tested on vocabulary keys, M passed]`. Union-graph
  dedup and oversized deltas keep the generic scan; streaming execution reads whole-block
  morsels only when the snapshot has no delta.
* **Key ranges for a fixed start.** Under `STRSTARTS` or a `REGEX` anchored on a literal
  prefix (`^abc` with at most the `s` flag and no alternation), the run-based operators
  above scan only base-vocabulary IDs in that prefix range (literals for `?v`; literals and
  IRIs for `STR(?v)`). Inline values and delta terms are still read and tested.
* **ID ranges for a fixed start.** The same prefix ranges restrict post-join filters:
  base-vocabulary IDs outside the ranges fail without reading their keys, and only keys
  inside the ranges are read. The planner costs a filtered sorted scan by the exact row
  count of its ranges rather than the full scan. In local measurements on English DBpedia,
  `?s a dbo:Band ; rdfs:label ?l FILTER(REGEX(?l, "^The B"))` reads 23,812 labels and merges
  them with bands in 3.8 ms warm (90 ms cold), versus 18.8 ms warm (1,040 ms cold) for
  per-band label lookups.
* **Pure expressions per distinct value** (`expr_cache`). A FILTER conjunct, BIND, ORDER BY
  key or aggregate argument reading one variable with deterministic output runs once per
  distinct ID, caching results and errors. Volatile or effectful calls (`RAND`, `UUID`,
  `STRUUID`, `BNODE`, `EXISTS`, ARQ UUID aliases, `fn:apply`, `afn:eval` and effectful
  extension functions) always run per row; `NOW` and the base IRI are fixed per query. On a
  sorted column distinct IDs are runs; otherwise a sample skips caching when fewer than half
  the rows repeat (`EXPLAIN` reports `[expr cache: …]`, `exprCacheHits`, `exprCacheMisses`
  and `exprCacheSkipped`). Constant regexes compile once per query or reuse the server's
  compiled-program table with per-thread budgeted match caches.

  The planner accounts for sorting an unsorted column so a single filtered pattern picks the
  permutation sorted on the filtered variable. A cursor BIND can retain cached results
  across batches in a map keyed by exact input IDs. A batch of dense inline integers
  admits the map at once, and so does a first batch in which a key repeats while
  unchanged-snapshot statistics estimate the input domain below 80% of rows, walking at
  most 64 plan nodes. Otherwise the first batch admits the map when one row in eight
  repeats on one worker or for an expression that cannot run in parallel, and when half
  its rows repeat for a parallel-capable expression on more than one worker. Later
  batches feed a sketch of one key in sixteen and the exact distinct count of each
  batch. On more than one worker, weaker repetition of a parallel-capable expression
  admits only when a measured evaluation rate shows that the map pays, or when
  sustained split losses show that parallel evaluation contends. The map's misses and
  the ordinary evaluation share one split controller that keeps a contended expression
  serial for a cooldown and then retries the pool. The per-query cache caps at 65,536
  entries, charged at about 6 MiB, and dense inline integers cap at 16,384 entries. A
  full cache still answers the keys it holds and evaluates each distinct missed key once
  for the batch without keeping it. A cache whose growth the budget refuses keeps the
  entries it holds. A refused batch charge, or any refusal of memory that the
  evaluation of the cache's misses meets while the cache holds its charges, releases the
  cache and evaluates the batch the ordinary way, and an operator that ends its input releases the cache too. Identity copies, volatile expressions, application callbacks and
  expressions that can construct a geometry, whose charge lasts until the query ends,
  never enter this cache.
* **Numeric range scans.** A FILTER comparing a scan's sort column with numeric constants
  reads only matching ID ranges (`IndexRangeScan`). Inline integers and single-scale inline
  decimals sort by value within their ID segments, so binary search finds each segment's
  range. Doubles and vocabulary literals are read and tested; booleans, dates and blank
  nodes are skipped. The planner costs the scan from exact range row counts.
* **Comparisons decided on IDs** (part of `range_pushdown`). When a FILTER compares one
  variable with numeric, `xsd:date` or `xsd:dateTime` constants over a non-range-scan table,
  IDs are classified first. Numbers reuse the numeric range-scan segments (searched lazily
  when a segment first appears). Inline dates and dateTimes sort by local calendar fields,
  and values three or more days away from a constant decide in every timezone (covering
  ±14h offsets). Non-comparable inline kinds fail immediately, while vocabulary literals,
  computed values and near-boundary dates fall back to expression evaluation (with
  `expr_cache` where eligible). In local measurements at 10M triples, together with
  `bound_order_keys`, BSBM BI `q3` drops from 41.4 ms to 31.5 ms. `EXPLAIN` notes
  `[decided on ids: … for N of M rows]`.
* **Single-row point patterns** (`point_patterns`). For a pattern with one free variable
  and at most four estimated rows, the planner reads up to two rows and substitutes a
  constant when the snapshot holds exactly one matching row, restoring the variable in the
  result and preserving term identity, graph scope and multiplicity. This also turns
  dependent arithmetic FILTER bounds into constants. Volatile filters, application callbacks
  and variables referenced in other algebra nodes keep their bindings.
* **Whole-block scans under graph filters.** When a pass over a block's graph column shows
  every row matches the graph filter, the slice is copied column-wise instead of row by row.

### Cardinality estimation and join ordering

* **Filter selectivity from samples** (`sampled_filters`). By default the planner assumes a
  FILTER conjunct retains 30% of rows. For a conjunct whose variables come from one triple
  pattern, the planner instead tests it on a sample from the sorted index and caches the
  selectivity on the snapshot for later queries at the same commit.

  Samples use in-memory block first/last keys first (zero I/O); if too few exist, one or
  two blocks (preferring cached blocks, sorted on another variable when available) yield 128
  rows each, and patterns of at most a few thousand rows are read whole with the delta.
  Multi-pattern conjuncts keep the fixed 30% estimate. `EXPLAIN` notes `[selectivity 0.0650
  of 187 sampled rows]`. In local measurements at 10.5M triples, `?p foaf:name ?n ; foaf:age
  ?a FILTER(CONTAINS(?n, "Ada"))` (5% true selectivity, 6.5% sampled) switches to a
  name-sorted scan and merge, dropping runtime from 20 ms to 12 ms. Sampling adds 0.02 to
  0.08 ms when blocks are cached, or 0.3 to 0.6 ms when decoding a block.
* **Star estimates from characteristic sets** (`characteristic_sets`). Estimating a join
  from distinct counts assumes the smaller side's keys all appear on the larger side, which
  overestimated six-pattern WatDiv stars tenfold. During bulk load and compaction, Sparkles
  counts subject characteristic sets (after Neumann and Moerkotte): each exact set of
  predicates and `rdf:type` classes a subject has, keeping the top 32,768 sets by subject
  count along with predicate triple counts.

  A subject join of constant-predicate patterns (`?s p ?o` or `?s a <class>`) sums over
  matching sets, treating other restrictions as independent; patterns with other constant
  objects or older stores fall back to distinct-value estimates. Matching sets are indexed
  once per generation and predicate-set sums are cached. In local measurements on WatDiv at
  1.1M triples, C3 drops from 1.48 ms to 0.65 ms and C2 from 0.21 ms to 0.03 ms.
* **Characteristic-set remainders** (`set_remainders`). Subjects outside the top 32,768
  sets are summarized in one remainder record (total subjects plus per-predicate subject and
  triple counts, assuming predicate independence inside the remainder). Every triple is
  accounted for, removing the older 95%-coverage cutoff. At WatDiv scale-100, keeping
  32,768 sets captures all 24,037 sets, bringing S3's worst join estimate within 2× and S5
  from 1,041× to 106×; on DBpedia's 8.2M-triple slice (64,814 sets), the top 32,768 cover
  95% of subjects.
* **Node characteristic sets** (`node_sets`). Subject sets cannot estimate object-object
  joins (`?a p ?v . ?b q ?v`) or chain links (`?a p ?v . ?v q ?b`). Bulk load and
  compaction also collect bidirectional node sets (outgoing predicates, incoming predicates
  and classes, capped at 32,768 sets plus a remainder) in one parallel pass over SPO and
  OPS; partial compaction updates affected nodes. Past 2^20 distinct signatures, a count-min
  sketch with stable hash tie-breaking selects candidates for a deterministic second pass so
  `stats.json` is independent of thread count (absent blank nodes).

  Any variable appearing as the object of a constant-predicate pattern `?x p ?v` (except
  `?x a ?c`) uses node sets across all its joins; inputs restricted by a constant object,
  subquery or VALUES fall back to subject sets for outgoing joins or distinct-value
  estimates otherwise. In local measurements, WatDiv L1 error falls from 10–87× to 2–11×,
  C2's worst join from 1,222× to 30×, and DBpedia's `birthPlace` object join from 1,478× to
  8×. The tradeoff is statistics memory: on that DBpedia slice, `stats.json` grows from
  5.0 MB to 44.3 MB, open time from 0.07 s to 0.30 s, and ready server RSS from 133 MB to
  214 MB ([cardinality results](../benchmarks/engine/RESULTS.md#cardinality-estimates)).
* **Joins on several keys** (`key_backoff`). Multi-key joins previously divided only by the
  largest single-key distinct count, overestimating closing edges such as triangle joins or
  LUBM `q9`'s `?Y teacherOf ?Z`. The estimate now divides by the larger side's estimated
  distinct key tuples using exponential back-off (exponents 1, 1/2, 1/4, 1/8 on sorted key
  counts), capped at the side's row count when one input binds all keys. LUBM `q9`'s closing
  join estimate improves from 32,256 to 792 (actual 2,540), and `q2`'s to 115 (actual 28).
* **Join estimates against a constant restriction** (`domain_estimates`). When a pattern
  fixes both predicate (other than `rdf:type`) and the opposite column to constants (such as
  `?v0 age AgeGroup5`), its bound variable is a subset of that predicate's domain. When
  probed keys and characteristic sets decline on an unchanged snapshot, distinct-value
  estimates divide by at least the predicate's distinct subject or object count across that
  pattern and joins, filters, sorts, projections and extends above it. On WatDiv `S4-2`,
  this merges `artist` and `nationality` (estimated 122 for 21 rows) and probes `age` and
  `familyName` with 21 keys instead of 704. Because domains do not record the constant's
  identity, two sides sharing the same constant restriction are still treated as independent
  ([ROADMAP.md](../ROADMAP.md), item 875).
* **Join estimates from probed values** (`probed_keys`). When one input in a group is small
  (VALUES or a pattern of at most 1,024 rows) and characteristic sets do not apply, the
  planner samples up to 32 evenly spaced keys and counts their matches in every other
  pattern on that variable (index-based join sampling after Leis et al.). Probes touch at
  most two uncached blocks per group and cache counts on the snapshot; uncached small
  patterns defer to later queries. When a partial sample finds zero hits across `k` probed
  keys, it uses a non-zero floor of `1 / (k + 2)` times average rows per key so sparse
  joins (such as LUBM `q2`, where 10 of 1,000 universities have departments) are not
  underestimated at 1 row.

  Joins outside characteristic sets and probed keys keep QLever's 0.7 factor. Across the
  synthetic benchmark queries and WatDiv 1.1M, the combined estimators bring the median
  join error to exact on both datasets and reduce mean error from 1.37× to 1.04× on
  synthetic data and from 4.2× to 1.6× on WatDiv.
* **Join ordering on cost summaries.** The dynamic-programming join orderer (always active)
  operates on compact plan summaries (cost, estimated rows, sort variable and distinct
  counts) and materializes only the winning plan tree. A greedy baseline prunes any partial
  plan that exceeds its cost; plans sorted on variables no remaining join reads are pruned
  when matched by an equal-or-cheaper plan; only connected subsets are enumerated.

  Up to 10 patterns this matches exhaustive DP (bringing a nine-pattern star like WatDiv S1
  from 400 ms to ~1 ms). Larger groups whose subsets exceed the ~1 ms split budget are
  planned in bounded rounds (never worse than greedy), and groups with more than 16 inputs
  or more than 128 shared variables use the greedy plan directly.

### Aggregation and top-k

* **Top-k heap** (`topk_heap`). `ORDER BY … LIMIT k` (with or without `OFFSET`) keeps the
  best `k` rows in a bounded heap. Keys are classified once so most comparisons stay on
  exact decimals, doubles or strings, and later keys evaluate only for rows whose first key
  can still enter. Streaming execution feeds batches into the same heap. `EXPLAIN` notes
  `[top-k heap kept N of M rows]`. With the heap off, numeric top-k and first-key top-k
  apply instead.
* **Numeric top-k.** `ORDER BY ?v LIMIT k` over numbers ranks cheap rounded keys first and
  computes exact values only for candidates that can reach the top `k`.
* **First-key top-k** (`topk_first_key`). `ORDER BY k1 k2 … LIMIT k` locates the `k`-th row
  by `k1` alone and drops worse rows before decoding or evaluating later keys. `EXPLAIN`
  notes `[first-key prefilter kept N rows]`.
* **ORDER BY keys from BIND** (`bound_order_keys`). When an ORDER BY key matches a BIND or
  SELECT expression directly below it (for example `SELECT (?a / ?b AS ?r) … ORDER BY
  DESC(?a / ?b)`), the sort reads the bound variable instead of re-evaluating the expression
  (unbound and error sort identically). Volatile expressions and functions outside SPARQL
  and XSD casts are still evaluated again.
* **Ordered-scan top-k.** `ORDER BY ?v LIMIT k` (with or without `OFFSET`) over one triple
  pattern and its FILTERs reads a `?v`-sorted permutation and stops once the first `k` rows
  are proven (`IndexTopK`). Inline integers, single-scale decimals, same-sign doubles and
  base-vocabulary IRIs sort by value within their ID segments and are read incrementally
  from the best end.

  In ascending order, base-vocabulary literals are read from their low end as well; because
  a literal's key appends `0xFF` after the lexical form, shorter prefix strings sort after
  longer ones and are looked up in the vocabulary (up to 256 bytes; non-plain-string `k`-th
  candidates fall back to plain sort). Descending literals, blank nodes and delta terms are
  read whole. Ties preserve plain-scan order, while NaN, dates and durations (no total
  order) use plain sort. The planner chooses `IndexTopK` from exact segment counts when it
  reads at most half the pattern's rows.
* **Incremental GROUP BY.** With one group key and `COUNT`, `SUM`, `AVG`, `MIN`, `MAX` or
  `SAMPLE` over variables, each group updates running state in an ID-keyed hash map. Sums
  stay exact 64-bit integers until a non-inline-integer value arrives.
* **Count joins from key runs.** `COUNT(*)` over two scans joined on one variable reads both
  sorted indexes as `(key, run length)` pairs and sums their products (`CountJoinFromRuns`).
* **Counts from statistics.** `GROUP BY ?class` over `?s a ?class` and `GROUP BY ?p` over
  `?s ?p ?o` read index statistics (`GroupCountFromMetadata`, part of `metadata_counts`) and
  adjust them at query time for delta quads and excluded graphs (distinct counts probe the
  index to verify whether a value appeared before or disappeared after the change; predicate
  quad counts apply only to single-graph reads). The planner picks this when probe cost is
  below scan cost, and caches corrected counts on the snapshot. `EXPLAIN` notes `[from
  statistics, corrected for N delta quads]` or `[from statistics, without N quads of graphs
  not read]`.

### Join operators, paths and star fusion

* **Batched path frontiers.** `p*` and `p+` traversals expand large BFS levels with one
  merged pass over the predicate's index rows instead of one seek per node.
* **Selective column decoding.** Scans decode only the columns they read (variables, graph,
  repeated variables) into the block cache, reducing decode work and cache footprint.
* **Decorrelated EXISTS.** `FILTER EXISTS { P }` and `FILTER NOT EXISTS { P }` (where `P`
  uses triple patterns, `*`/`?`-free paths, `GRAPH` and deterministic FILTERs) evaluate `P`
  once into a set of distinct outer-variable tuples that outer rows probe. Rows leaving some
  keys unbound probe the bound-key subset; rows binding variables used only inside `P`'s
  FILTER still run by substitution. Decorrelation runs only when cheaper than per-key
  evaluation and within the memory budget (`EXPLAIN` reports `[EXISTS decorrelated on ?y: …]`
  and `exists*` counters).
* **Anti-join MINUS** (`anti_join`). When both sides of `MINUS` share one always-bound
  variable, matching left rows are removed by merge (if both inputs are sorted) or via an
  ID hash set (`EXPLAIN` notes `[anti-join on ?p by merge]`). Other shapes keep generic
  compatibility testing.
* **Merge left joins** (`merge_left_join`). An `OPTIONAL` sharing one always-bound variable
  with both sides sorted on it runs as a branch-free zipper merge, preserving left-side sort
  order for upstream operators and evaluating inner FILTERs on matched pairs before falling
  back to unmatched left rows. In local measurements at 10.5M triples, `optional-count`
  drops from 15 ms to 6 ms warm (`EXPLAIN` notes `[merge on ?p]`). Other OPTIONALs use hash
  join.
* **Pair conditions in streamed merge OPTIONAL** (`merge_pair_conditions`). A conditioned
  streamed merge OPTIONAL whose merge key is the only shared variable evaluates the
  condition on staged pairs before copying output rows, matching eager merge behavior. With
  the switch off, all pairs and left extensions are copied into the output batch and then
  compacted.

  The fallback copy-and-compact path is retained for cross-batch right runs, oversized left
  runs, spooled runs, tight memory budgets that cannot fit deciding scratch, and any query
  referencing registered functions or observable conditions (side effects, `RAND`, `UUID`,
  `STRUUID`, `BNODE`, `afn:system-timezone`, `afn:nowtz`, or `EXISTS` calling `SERVICE`,
  volatile functions or non-XSD-cast IRI functions). Deciding merges can hold up to one
  extra output batch plus condition scratch within tight budgets and may end batches at
  different left rows than the fallback path; observable calls remain identical in arguments
  and order, though batch boundaries are not part of that contract
  ([ROADMAP.md](../ROADMAP.md), item 1199).
* **Probed OPTIONAL** (`probed_optional`). In eager execution, an `OPTIONAL` whose right
  side is a triple pattern (with optional FILTERs) sorted on a variable always bound by the
  left side probes only the left side's distinct keys when cheaper than a full scan
  (`EXPLAIN` notes `[probed for the left side's keys: N keys, … rows read]`). In local
  measurements at 10M triples, BSBM Explore `q7` and `q8` drop from 5.2 and 5.8 ms to
  3.8 ms. Streamed OPTIONALs still read the full right side.
* **Keyed inner joins and alternating ASK** (`keyed_join`). Groups linked only by a FILTER
  equality run a keyed inner join to propose candidate pairs before applying the full
  FILTER. An eager `LIMIT` reads the larger input in growing prefixes; a large `ASK` reads
  both inputs alternately starting at 64 rows and stops at the first match. Hash joins below
  that `ASK` can probe a triple-pattern index when conservative scan and multiplicity bounds
  fit output and memory budgets; unproved expansions and finite row caps keep the streaming
  hash join. Alternating joins charge both prefix buffers before allocation and drop
  obsolete history once an input ends.
* **Eager ASK over bounded UNION.** An eager `ASK` over a bounded pure `UNION` orders
  branches by work before the first answer when conservative output bounds fit row and
  memory budgets (covering block counts for scans, input products for joins, sums for
  unions; index probes keep original order because metadata does not bound duplicate-key
  fanout). On an unchanged snapshot without a finite produced-row cap, whitelisted pure
  projections, filters, constant bindings, index joins and limited join sides propagate
  exact requested demand instead of a 64-row minimum, while retrying operators keep the
  ordinary prefix when widening would rebuild a full join input and proven bounds allow it.
  `SELECT`, effectful consumers and explicit streaming keep their existing order and demand
  policies.
* **Keyed left joins** (`keyed_left_join`). An `OPTIONAL` whose sides share no variable
  normally evaluates a Cartesian product before filtering. When the FILTER contains `?a = ?b`
  across both sides, candidate pairs are matched via equality keys (term IDs for IRIs, blank
  nodes, simple strings and uninterpreted datatypes; case-normalized keys for language
  strings; category keys for interpreted datatypes and triple terms) and tested in 64K-pair
  chunks, preserving bag multiplicity. On SP²Bench `q6` at 10M triples, this narrows
  1.58 × 10¹² product rows to 7.4M candidate pairs (`EXPLAIN` notes `[keyed on ?a = ?b: N
  candidate pairs]`).
* **Flat hash joins.** Build-side rows are stored contiguously per key in flat arrays in row
  order. Inputs of 64K rows or more are hash-partitioned into ~8K-row cache-resident chunks
  and built in parallel; the probe side probes in parallel 32K-row pieces and stitches
  results in probe order. In local measurements, joining 1M × 1M rows drops from 62–69 ms
  to 16–21 ms (`EXPLAIN` notes `[flat hash table: N keys in M parts]`).
* **Batched index joins** (`batched_join`, `gallop_index_join`). When an input always binds
  a variable a triple pattern can read sorted on and has few distinct keys relative to
  pattern size, `IndexJoin` reads only those key ranges while preserving input order and
  duplicates. With `gallop_index_join`, one permutation cursor gallops across block metadata
  and within blocks (merging delta scans where changed).

  In local measurements on 1.05M and 10.5M-triple stores (`sparql/costcal_tests.rs`),
  galloping costs 35–90 ns per clustered key and 50–630 ns per scattered key (versus
  120–490 ns and 250–1,250 ns for separate binary searches), against 2.2 ns per sequentially
  scanned row. The planner models 15 scanned rows per key plus 3.5 per doubling of rows per
  key (256 per touched block, 8 per row read), winning up to ~5% key density versus merge
  join (and higher versus large hash builds); probing is not offered above twice the scan
  cost. When a two-column probe's secondary variable is already bound, each primary span
  binary-searches the secondary column after verifying secondary ordering, falling back to
  compatibility scan for unbound or unordered values.
* **Fused stars** (`star_fusion`, `fused_star_costs`). Consecutive constant-predicate index
  joins on one subject (`?p ex:worksFor ex:org7 ; foaf:name ?n ; foaf:age ?a`) execute as
  one `StarJoin`, choosing between one SPO sweep per subject and per-predicate probes by
  estimated block cost and writing the output table once. Each member join is costed against
  the star's input keys rather than the rows remaining after earlier members
  (`fused_star_costs`).

## Cold reads

Read hints and read-ahead overlap reads on a server whose index and vocabulary files
are outside the page cache, such as on the first queries after a restart.
[X07](../specs/X07-cold-sparse-reads.md) has the design and the measurements.

* **Constants requested together.** Before the planner looks up
  the constants of a query one by one, it collects the IRIs and literals of the patterns,
  paths, VALUES rows, graph names and the common FILTER comparisons. It asks for the
  sparse-index group of every one of them in one pass (`Vocab::prefetch_keys`), so the
  group reads overlap instead of following each other. It visits at most 4,096 keys,
  and it does nothing without a sparse index or with the read hints off. A table of
  1,024 key hashes per vocabulary remembers the keys whose group was already asked for,
  so a warm constant costs a hash and a load instead of a search of the sparse index.
* **Scans requested before they run.** When the plan is built,
  every scan of 64 blocks or fewer asks the kernel for the columns the reader will decode.
  A block inside the key range needs the scan's columns, and a block at an end of the range
  also needs the columns that bound it. A range scan asks for the blocks of its id ranges.
  A scan whose first block is already decoded in the cache is skipped, so a warm plan
  costs one lookup per scan. A scan under a `LIMIT`, and a scan of more blocks, reads ahead
  as it goes. A `COUNT` scan asks for the two blocks at the ends of its range when it
  decodes them, and only if the first of them is not cached.
* **A read-ahead window per scan.** Index and vocabulary files are mapped for random
  access, so the kernel reads nothing ahead of a page fault. Every pass over consecutive
  blocks of a permutation (`ReadAhead` in `index.rs`) keeps its own window of requests
  ahead of it. The first request covers about 1 MiB and each refill, made when half of
  the window has been used, doubles the window up to 4 MiB. A scan of a few blocks is
  covered by the first request and a long scan keeps a window of reads in flight. A scan
  that stops early has requested at most the target plus one next-block extension,
  including its bridged gap of at most 64 KiB. Each refill counts existing unconsumed
  advice and actual coalesced bytes against the target. Coverage is tracked separately
  from examined cached blocks and pruned as the pass advances. An evicted column that
  received no advice gets current-block fallback advice in eager and cursor readers.
  Cached and empty spans also have a finite traversal budget, so repeated calls at one
  position do not add another window. Concurrent scans share no coverage state and do
  not change the mapping's random-access advice. The target bounds requested bytes.
  A pass consults its window only at a block the cache does not hold, so a warm pass does
  no read-ahead work. Passes over the vocabulary in id order use the same 4 MiB target.
* **Terms of small inputs.** A FILTER, BIND or ORDER BY over a table of
  2 to 4,095 rows evaluates the rows one at a time. Before the first row, it asks for the
  vocabulary blocks of the terms it will read. With a numeric column it also asks for the
  pages of the values, and for the pages of the kinds and the rank when the column keeps
  them mapped. It reads no value and builds no result, and each page is asked for once per
  mapping, so a warm call makes no system call. It does locate each id, which costs a
  memory read. Larger tables decode their terms in id order as before. The native JSON
  result writer asks for the blocks of the terms it prints, as the other result writers do.
* **Parallel decode.** An index join that has asked the kernel for
  its blocks decodes their columns on a small thread pool of the block cache's own, before
  the reader reaches them. Scans do not use it. It decodes only columns the reader decodes
  anyway and keeps them in the cache. A request adds at most an eighth of the cache, so a
  single request cannot push out much of what other queries hold. Concurrent requests can
  still evict each other's decoded columns, which wastes decoding but never changes an
  answer. The pool starts at the first request that decodes ahead, which pays for starting
  up to eight threads inside its own deadline. A failure to start the pool leaves the
  decoding to the reader's thread. A lookup that touches dozens of blocks for a few rows
  each decodes the segments that can hold them, and this spreads that work over several
  cores when it comes to 32 segments or more. Reads through a view that keeps no blocks,
  such as an export, skip it.
* **Whole blocks for large joins.** In index format 3 a lookup decodes the segments of
  2,048 rows that can hold its keys, and a block turns dense, so that its readers decode
  it whole, once a quarter of its segments were read one by one. An index join whose
  first range in a block reaches a segment the cache does not hold looks ahead over the
  join's later ranges in that block. When they span more than a quarter of its segments,
  the join marks the block dense and reads it whole at once. The join prefetch skips a
  join of more than 16,384 ranges, so without this a large join read a quarter of each
  block by segments and then the whole block again. The lookahead runs once per block and
  checks for cancellation as the join's reads do.
* **Prewarm after a restart** (`serve --prewarm`). This is not an `Optimizations` flag.
  The server records the decoded columns of each persistent dataset's block cache and
  the resident pages of its other index files, and the next start reads them on a
  background thread after it is ready. [Memory and caches](../USAGE.md#memory-and-caches)
  describes it.
* **Decoding the next cursor batch ahead.** A result writer that reads
  a cursor decodes the terms of the next batch on another Rayon thread while it writes the
  current one. It writes the first batch before it pulls the second, so the first rows
  wait for one batch only, and it reads ahead only when the memory budget has room for
  twice what the first batch took. The decoded terms of a batch are charged to the
  query's memory ledger, and a batch whose terms the ledger refuses is decoded cell by
  cell as before. With one Rayon thread the writer decodes in turn. Together with
  charging each written row's terms once, in local measurements this took a streamed TSV
  export of 500,000 rows from 134.7 ms to 72 ms at 12 threads, where eager execution takes
  84 ms.
* **Predicate patterns** (`predicate_patterns`). A generation keeps `patterns.bin`, which
  gives every subject the number of its set of predicates and lists those sets, once for
  the default graph and once for the union of the named graphs. A `GROUP BY ?p` that
  counts distinct subjects over an input binding `?s` joined with `?s ?p ?o`, where `?p`
  and `?o` appear nowhere else, becomes `PatternCount`. It looks up each distinct input
  subject's pattern, counts subjects per pattern and adds each count to the pattern's
  predicates. With no input it reads the per-predicate totals that the file stores.
  `SELECT DISTINCT ?p` over the same shape and `spk:hasPredicate` (`PatternScan`) use the
  same tables. The tables describe the base index, and every subject that the snapshot's
  delta touches is read again from SPO, so the cost grows with the touched subjects. Past
  one subject per 32 quads (at least 1,024 and at most a million), the query reads the
  permutations. The plan does not use the tables under a triple-level protection, with
  RDFS on read, for a set of named graphs or for the named table under a graph view.
* **Growing filter and top-k inputs** (`grow_input`). A cursor filter starts by asking
  for 32,768 rows and doubles its demand with each read up to 262,144 rows while the row
  budget and the memory ledger have room. The scan below it still stops at the caller's
  `batch_bytes`, so with the default 1 MiB a two-column read holds at most 65,536 rows. A
  cursor top-k starts at the cursor's batch size and doubles up to 65,536 rows on the same
  condition, raising the byte cap with it. Larger reads let the expression caches and the
  top-k screen work on more rows at a time. The tradeoff is memory. Over 300,000 rows a
  substring filter peaked at 12.3 MB against 6.7 MB, and a top-k at 24.1 MB against
  2.5 MB, bounded by the query's budgets. Independently of the switch, when the first key
  of a cursor top-k holds one result per distinct value and every value is of a totally
  ordered kind, its screen remembers which values it rejected for the rest of the batch,
  so a repeated value is screened once. In a total order a rejected value is worse than
  the heap's worst row, which only improves as rows enter, so it stays rejected. Dates,
  times and durations can form cycles, where a value rejected once ranks ahead of a
  later worst row, so their rows are screened one by one.
