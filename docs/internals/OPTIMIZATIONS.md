# Query optimization reference

[ARCHITECTURE.md](../ARCHITECTURE.md#query-execution-and-where-optimizations-live)
explains the query pipeline and the role of these mechanisms. This reference records
operator applicability, cost-model choices and local implementation measurements.
The timings below describe those measurements, not a current performance guarantee;
[BENCHMARKS.md](../BENCHMARKS.md) owns the published engine comparisons and methodology.

The switches are defined in
[`Optimizations`](../../crates/sparkles-core/src/sparql/ctx.rs). Each fast path has an
applicability check and a generic fallback. The source is authoritative when an
operator or its cost model changes.

* [Optimizations adopted from QLever](#optimizations-adopted-from-qlever)
* [Further scan and filter optimizations](#further-scan-and-filter-optimizations)

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
  decoded.
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
  each row of the larger input that probes it. A scan reads a row in 1.3 to 2.2 ns. With
  the flat table switched off (`flat_hash_join`), a row of the smaller input costs 48,
  since the lists per key take 18 to 100 ns a row. Merge joins gallop through skewed
  inputs. `COUNT(*)` over one pattern comes from index
  metadata. Transitive paths traverse from the bound side, with index lookups per
  frontier node, instead of materializing the closure.
* **Executed-plan feedback.** Every query returns a runtime-information tree (estimated
  and actual rows, time per operator), like `qlever-json`. The UI renders it.

## Further scan and filter optimizations

Each of these can be switched off per query (`QueryOptions::optimizations`) or per process
(`SPARKLES_DISABLE_OPTIMIZATIONS=range_pushdown,…`). EXPLAIN shows which ones ran.

* **`COUNT(DISTINCT ?v)` from index runs.** Over one triple pattern, the scan switches to a
  permutation sorted on `?v` and counts runs of equal ids (`CountDistinctFromIndex`). No
  rows are materialized or hashed. When the pattern is `?s ?p ?o`, or binds only its
  predicate, the count comes from the index statistics instead, corrected like the counts
  from statistics below (`CountDistinctFromMetadata`, part of `metadata_counts`).
* **Filters on vocabulary keys.** `CONTAINS`, `STRSTARTS`, `STRENDS` and `REGEX` over `?v`
  or `STR(?v)`, and `LANGMATCHES(LANG(?v), …)`, are tested on the stored key bytes
  (`"lexical 0xFF @lang`, `<iri`). Each front-coded block is read once, in parallel,
  without allocating a string per term. A `CONTAINS` needle is searched with a substring
  finder built once, and a key is checked to be UTF-8 only when it matches, since a byte
  match of UTF-8 text is a match of the strings. Each parallel task tests with its own
  copy of a regular expression, because threads that share one wait for its match caches.
  Terms added by updates are tested on their delta keys. Inline values (numbers, dates)
  go through the general evaluator.
* **Filtered counts from index runs** (`count_filter_runs`). `COUNT(*)`, `COUNT(?x)` and
  `COUNT(DISTINCT ?v)` over a FILTER that reads only `?v` of a single triple pattern read
  the pattern from the permutation sorted on `?v`. The filter is tested once per run of
  equal ids, and the count is the sum of the lengths of the runs that pass, or the number
  of such runs for `COUNT(DISTINCT ?v)` (`CountFilterFromRuns`). No rows are
  materialized. When the snapshot's delta has no key in the pattern's range, the index
  blocks are read in parallel and each block's vocabulary ids are tested on their keys as
  the block is read, in pieces of 8192 rows so that a short scan still uses every thread.
  Otherwise the runs are read in order with the delta merged in. At 10.5M triples,
  counting the `foaf:name` values that contain "Ada" tests 1.02M names and takes about
  4 ms in the server, against 14 ms when the name column was materialized first.
* **Filters on the runs of a scan** (`filter_scan_runs`). A FILTER directly over a scan
  sorted on a variable it tests evaluates the conjuncts that read only that variable once
  per value, then copies out of the index blocks only the rows of the values that pass,
  in parallel. The other conjuncts are applied to those rows. EXPLAIN notes
  `[runs of ?v: N values tested on vocabulary keys, M passed]`. Union-graph dedup, which
  compares neighbouring rows, and a delta with keys in the scan's range keep the generic
  scan and filter.
* **Filter selectivity from samples** (`sampled_filters`). Without other knowledge the
  planner assumes that a FILTER conjunct keeps 30% of its input. A conjunct whose
  variables are all bound by one triple pattern is instead tested on a sample of that
  pattern's rows, and the share of the sample it keeps becomes its estimate. The sample
  comes from the sorted index. The first and last keys of the pattern's blocks are held in
  memory by the block metadata and cost nothing to read. When they are too few, one or
  two blocks are decoded as well, preferably blocks the cache already holds, and give 128
  rows each. A pattern of at most a few thousand rows is read whole, with the delta. The
  pattern is read sorted on another variable when it has one, so that a decoded block
  holds values from all over the filtered variable's range. Selectivities are kept with
  the snapshot, and later queries at the same commit reuse them. A conjunct over several
  patterns keeps the fixed estimate. EXPLAIN notes `[selectivity 0.0650 of 187 sampled
  rows]` on the filter. At 10.5M triples, `?p foaf:name ?n ; foaf:age ?a
  FILTER(CONTAINS(?n, "Ada"))` keeps 5% of the names. With an estimate of 6.5% instead
  of 30%, the planner tests each distinct name once in a scan sorted on the name, sorts
  the 50,000 rows that pass and merges them with the ages. The query runs in 12 ms
  instead of 20 ms. Sampling adds 0.02 to 0.08 ms to planning a filter when the blocks are
  cached, and 0.3 to 0.6 ms when one has to be decoded.
* **Star estimates from characteristic sets** (`characteristic_sets`). A join on a
  variable estimated from distinct values assumes that the values of the side with fewer
  are all among the other side's. For patterns on one subject this holds only when every
  subject with the rarer predicate has the others. In WatDiv, where users have most
  predicates independently of each other, it overestimated a star of six patterns
  tenfold. A bulk load or compaction now counts the characteristic sets of the subjects,
  after Neumann and Moerkotte: each set of predicates some subject has exactly, with its
  subjects and the triples of each predicate. The classes a subject has by `rdf:type`
  count as predicates of their own, since a class usually decides which predicates its
  instances have. The statistics keep the 10,000 sets with the most subjects. A join on a
  subject variable of patterns `?s p ?o` with a constant predicate, or `?s a <class>`, is
  then estimated from the sets that hold the predicates of both sides, including the
  number of triples per subject of each predicate within those sets. Whatever else
  restricts an input is taken to be independent of the predicates. Patterns with another
  constant object, predicates that the kept sets cover poorly, and stores loaded before
  this change keep the estimate from distinct values. The sets holding a predicate are
  found once per index generation, and the sums for a query's predicates are kept for
  later queries. On WatDiv at 1.1M triples, the joins of the C3 star are estimated
  exactly, and C3 runs in 0.65 ms instead of 1.48 ms. C2 runs in 0.03 ms instead of
  0.21 ms.
* **Join estimates from probed values** (`probed_keys`). When one input of a group is
  small, a VALUES table or a pattern of at most 1,024 rows, the planner reads up to 32 of
  its values, spread over them in order, and counts the rows each has in every other
  pattern of the group on the same variable, as an index join would look them up. The mean
  count and the share of values that have rows then estimate a join of the small input
  with that pattern. They replace the assumption that every value is on the other side
  with the pattern's average number of rows, less 30%. This is index-based join sampling
  as Leis et al. describe it, from one small input. When the small input is itself a
  pattern the characteristic sets count, their estimate comes first, since the sets know
  how a star's predicates occur together. The counts read each block of a pattern once,
  and at most two blocks per group that the block cache does not hold. A small pattern
  whose blocks are not cached is left to a later query, after this one has read them. The
  counts are kept with the snapshot. At 10.5M triples, a star of three patterns over
  30,000 VALUES keys is estimated at its 30,000 rows instead of 10,290.

  Joins that neither the characteristic sets nor probed values estimate keep QLever's
  0.7 correction. Measured over the two-input joins of every plan of `scripts/bench.sh`,
  the queries for these estimates and WatDiv at 1.1M triples, with the new estimates
  switched off, the factor makes the median join of the generated data 30% low, since its
  joins follow references that exist. The median WatDiv join is still 3.4 times too high
  with it. No single factor fits both. With the new estimates, the median join of both
  datasets is estimated exactly, and the mean error falls from a factor of 1.37 to 1.04
  on the generated data and from 4.2 to 1.6 on WatDiv.
* **Key ranges for a fixed start** (`filter_key_ranges`). Under a `STRSTARTS`, or a
  `REGEX` anchored on a literal start (`^abc` with no flag other than `s` and no
  alternation), the two operators above read only the base-vocabulary ids of the keys
  with that start: literals for `?v`, literals and IRIs for `STR(?v)`. Ids outside the
  base vocabulary (inline values and terms added by updates) are still read and tested.
* **Id ranges for a fixed start** (`filter_id_ranges`). The same starts restrict the other
  places that test such a filter on vocabulary keys. A FILTER over the rows of a join
  fails the base-vocabulary ids outside the ranges without reading their keys, and reads
  only the keys of the ids inside them, which lie together in the vocabulary. The planner
  costs a filtered scan sorted on the tested variable by the rows of its ranges, counted
  exactly, instead of by every row of the scan. On English DBpedia, `?s a dbo:Band ;
  rdfs:label ?l FILTER(REGEX(?l, "^The B"))` then reads the 23,812 labels in the range
  and merges them with the bands, instead of looking up the label of each of the 36,745
  bands. It went from 18.8 to 3.8 ms warm and from 1,040 to 90 ms cold.
* **Pure expressions per distinct value** (`expr_cache`). A FILTER conjunct, BIND, ORDER BY
  key or aggregate argument that reads one variable and gives the same result for the same
  term is evaluated once per distinct id of that variable. Rows look up the result, errors
  included. RAND, UUID, STRUUID, BNODE and EXISTS run per row. NOW and the base IRI are
  fixed for the query. On a column sorted by the variable, the distinct ids are its runs.
  Otherwise a sample estimates how often values repeat, and when fewer than half the rows
  repeat a value the operator runs row by row. EXPLAIN notes `[expr cache: …]` with the
  distinct count or the reason it ran row by row, and reports `exprCacheHits`,
  `exprCacheMisses` and `exprCacheSkipped`. A constant regular expression is compiled once
  per thread and reuses its match cache. The planner counts the cost of sorting an
  unsorted input column, so a single pattern under such a FILTER is read from the
  permutation sorted on the filtered variable.
* **Numeric range scans.** A FILTER that compares a scan's sort column with numeric
  constants reads only the id ranges that can match. Inline integers, and inline decimals
  of one scale, sort by value within their id segment, so each segment's matches form one
  range, found by binary search with the ordinary comparison. Doubles and vocabulary
  literals are read and tested. Booleans, dates and blank nodes are skipped. The planner
  costs the scan from an exact count of the rows in those ranges (`IndexRangeScan`).
* **Top-k heap** (`topk_heap`). `ORDER BY … LIMIT k` (with or without OFFSET) keeps the
  best k rows in a bounded heap. Each key is classified once, so most comparisons are
  of exact decimals, doubles or strings, and the remaining pairs go to the ordinary
  comparator. The later keys are evaluated only for rows whose first key can still
  enter. Streaming execution reads its input batch by batch into the same heap.
  EXPLAIN notes `[top-k heap kept N of M rows]`. With the heap off, the two shortcuts
  below apply.
* **Numeric top-k.** `ORDER BY ?v LIMIT k` over numbers ranks cheap rounded keys first and
  computes exact values only for rows that can still reach the first k.
* **First-key top-k** (`topk_first_key`). `ORDER BY k1 k2 … LIMIT k` finds the k-th row by
  the first key alone and drops every row whose first key is worse, since at least k rows
  come before it. The later keys, often IRIs or strings that must be decoded, are
  evaluated only for the rows that remain. EXPLAIN notes `[first-key prefilter kept N
  rows]`.
* **Ordered-scan top-k.** `ORDER BY ?v LIMIT k` (with or without OFFSET) over one triple
  pattern and its FILTERs reads the pattern in `?v` order and stops once the first k rows
  are proven (`IndexTopK`). The scan switches to a permutation sorted on `?v`. Inline
  integers, decimals of one scale and doubles of one sign sort by value within their id
  segment, and each segment is read from its best end, a few rows at a time, until the
  k-th candidate beats the best unread value. Vocabulary literals (non-canonical numerals,
  other numeric types, strings), IRIs and blank nodes are not in value order by id, so
  they are read whole. Candidates are ranked by the ordinary ORDER BY in the plain scan's
  row order, so ties come out the same. NaN, dates and durations have no total order and
  use the plain sort. The planner picks this operator, from exact row counts per segment,
  when it reads at most half of the pattern's rows.
* **Incremental GROUP BY.** With one group key and COUNT, SUM, AVG, MIN, MAX or SAMPLE over
  variables, each group keeps a running state in a hash map keyed by id. Sums stay exact
  64-bit integers until a value is not an inline integer.
* **Count joins from key runs.** `COUNT(*)` over two scans joined on one variable reads both
  sides as (key, run length) pairs from indexes sorted on that variable and sums the
  products (`CountJoinFromRuns`).
* **Counts from statistics.** `GROUP BY ?class` with a count over `?s a ?class`, and
  `GROUP BY ?p` with a count over `?s ?p ?o`, read their counts from the index statistics
  (`GroupCountFromMetadata`, part of `metadata_counts`). The statistics describe the base
  index as the last compaction or bulk load wrote it, so the updates since then are
  applied at query time. Each inserted or deleted quad changes a quad count by one. It
  changes a distinct count only when an index probe finds no other quad that holds the
  value before or after the change. Quads of graphs the query does not read are taken out
  in the same way. The quad counts per predicate are used only when the query reads a
  single graph, because a union of graphs counts a triple once however many graphs hold
  it. The planner uses the statistics when one probe per changed value costs less than
  reading the scan. The corrected counts are kept with the snapshot, so later queries at
  the same commit reuse them. EXPLAIN notes `[from statistics, corrected for N delta
  quads]` or `[from statistics, without N quads of graphs not read]`. The
  `delta_statistics` switch limits the statistics to a store without a delta.
* **Batched path frontiers.** `p*` and `p+` traversals expand a large BFS level with one
  merged pass over the predicate's index rows instead of a seek per node.
* **Selective column decoding.** The block cache holds decoded columns, and scans decode
  only the key columns they read (variables, graph, repeated variables), which also leaves
  more room in the cache.
* **Decorrelated EXISTS.** `FILTER EXISTS { P }` and `FILTER NOT EXISTS { P }`, where `P`
  holds triple patterns, paths without `*` or `?`, `GRAPH` and deterministic FILTERs,
  evaluate `P` once and keep the distinct values of the variables the outer rows bind.
  Each outer row probes that set instead of evaluating the substituted pattern. A row that
  leaves some of them unbound (after OPTIONAL) probes the set of the bound ones. A row
  that binds a variable used only by a FILTER inside `P` is still evaluated by
  substitution. The key set is built once per query, only when `P` costs less than
  evaluating it per distinct outer key, and EXISTS stays per row when the set does not fit
  in the memory budget. EXPLAIN shows `[EXISTS decorrelated on ?y: …]` or the reason it
  was not, with `exists*` counters.
* **Anti-join MINUS** (`anti_join`). When the two sides of a MINUS share one variable that
  every row of both binds, the left rows whose value appears on the right are removed. Both
  sides sorted on the variable give a merge. Otherwise the right side's values go into a
  set of ids that the left rows probe. EXPLAIN notes `[anti-join on ?p by merge]`. Other
  MINUS shapes keep the generic compatibility test.
* **Merge left joins** (`merge_left_join`). An OPTIONAL whose two sides share one
  variable, are both sorted on it and both always bind it runs as a merge. A branch-free
  zipper like the anti-join's finds where each left row's key starts on the right, and
  each left row is then written with the right rows of its key, or alone. The output keeps
  the left side's order, and the planner treats it as sorted that way. A FILTER inside
  the OPTIONAL is tested on the matched rows, and a left row none of whose matches pass
  is kept alone. At 10.5M triples, `optional-count` runs in 6 ms instead of 15 ms on a
  warm store. EXPLAIN notes `[merge on ?p]`. Other OPTIONALs keep the hash join, which
  puts the unmatched left rows last.
* **Flat hash joins** (`flat_hash_join`). A hash join groups the rows of its smaller input
  by key in flat arrays instead of a list per key. It counts each key's rows and then
  places every row, so the rows of a key are one span of an array, in row order. An input
  of 64K rows or more is first split into partitions of about 8K rows by bits of a hash
  of the key, so that each partition's table stays in the CPU caches while it is built,
  and the partitions are built in parallel. The larger input probes the table in pieces
  of 32K rows in parallel, and the pieces' pairs are joined in its order, so the output
  keeps the probing side's order. Joining a million rows with a million takes 16 to 21
  ms instead of 62 to 69 ms, and joins of inputs that fit in the caches take as long as
  before. EXPLAIN notes `[flat hash table: N keys in M parts]`.
* **Batched index joins.** When a join's input always binds a variable that a triple
  pattern can be read sorted on, and has few distinct values of it for the pattern's size,
  the pattern is read only for those values (`IndexJoin`). The sorted distinct keys become
  key ranges, and ranges in adjacent blocks are read in one scan: scattered keys cost a
  seek per region, dense keys one sweep. Each input row then joins its key's rows, so the
  input's order and duplicates are kept. The planner offers this next to merge and hash
  joins. The keys are read with one cursor over the permutation (`gallop_index_join`).
  Each key finds its first block by galloping over the blocks' first and last keys from
  the block of the key before. It finds its rows by galloping in the block the cursor
  holds, from where the key before ended, and the spans of the keys in the rows read are
  found the same way. A range with changes from updates in it is read by a scan that
  merges them. A key then takes 35 to 90 ns when the keys lie close together and 50 to
  630 ns when they are spread over the pattern, where binary searches and a scan per
  cluster of keys took 120 to 490 ns and 250 to 1250 ns. A scan reads a row in 2.2 ns on
  the same machine. The planner counts 15 scanned rows per key, plus 3.5 for each
  doubling of the pattern's rows per key, as fitted on keys spread at random. Each block
  the keys touch costs 256 and each row read 8. These figures were measured on stores of
  1.05M and 10.5M triples by the ignored tests in `sparql/costcal_tests.rs`. Probing then
  wins when the input has up to about 5% as many keys as the pattern has rows, against a
  merge join, and more against a hash join with a large input to build. With the cursor
  switched off, a key costs 140 rows plus 6 per doubling, and probing wins up to about 1
  or 2%. Probing is not offered when it would cost more than twice the scan of the
  pattern. EXPLAIN counts the keys, seeks, blocks and rows read (`batched_join`).
* **Fused stars.** Index joins on one subject over constant predicates (`?p ex:worksFor
  ex:org7 ; foaf:name ?n ; foaf:age ?a`) run as one operator (`StarJoin`). It walks each
  subject's SPO run once and picks out the star's predicates, or probes each pattern's own
  permutation, whichever touches fewer blocks, and builds the output once instead of
  through intermediate tables (`star_fusion`). The planner costs a star as its separate
  index joins, since a fused star still spends about as long per key and pattern. A fused
  star looks up every pattern for every key of its input, so each of those index joins is
  costed for the keys of the star's input, not for those left after the patterns below
  it (`fused_star_costs`).
* **Whole-block scans under graph filters.** A block slice is copied column-wise whenever
  every row passes the graph filter (one pass over the graph column), so default-graph
  queries avoid row-by-row filtering.
* **Join ordering on cost summaries** (`pruned_join_order`). The dynamic program runs on
  small summaries of plans, which hold the cost, the estimated rows, the sort variable and
  the distinct-value estimates that later joins read. The plan tree is built once, for the
  chosen joins only. A greedy plan is made first, and a partial plan that costs more than
  it is dropped, because it cannot be part of a cheaper plan. A plan sorted on a variable
  that no later join reads is dropped when another plan of the same patterns has the same
  estimates at no more cost. Only subsets whose patterns share variables are planned. Up to
  ten patterns, the result is the exhaustive program's plan or one of equal cost, except in
  rare cases where a dropped plan is the one that a later filter would have favored. Every
  WatDiv and `scripts/bench.sh` query plans at the same cost as before, and a star of nine
  patterns (WatDiv S1) plans in about a millisecond instead of 400 ms. A group whose
  subsets have too many splits to enumerate in about a millisecond is planned in rounds.
  Each round plans the subsets up to the size that fits, and the cheapest plan of that
  size becomes one input of the next round. That plan never costs more than the greedy
  one. Groups of more than 16 patterns keep the greedy plan, which costs each pair of
  plans once. Switched off, the program builds every candidate plan tree for every split
  of up to 12 patterns, and larger groups are planned greedily.
