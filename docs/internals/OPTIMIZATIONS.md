# Query optimization reference

[ARCHITECTURE.md](../ARCHITECTURE.md#query-execution-and-where-optimizations-live)
explains the query pipeline and the role of these mechanisms. This reference records
operator applicability, cost-model choices and local implementation measurements.
The timings below describe those measurements and are not a performance guarantee.
[BENCHMARKS.md](../BENCHMARKS.md) owns the published engine comparisons and methodology.

The switches are defined in
[`Optimizations`](../../crates/sparkles-core/src/sparql/ctx.rs). Each fast path has an
applicability check and a generic fallback. The source is authoritative when an
operator or its cost model changes.

* [Optimizations adopted from QLever](#optimizations-adopted-from-qlever)
* [Further scan and filter optimizations](#further-scan-and-filter-optimizations)
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
  or `STR(?v)`, `STRLEN` comparisons with integer constants, and
  `LANGMATCHES(LANG(?v), …)`, are tested on the stored key bytes
  (`"lexical 0xFF @lang`, `<iri`). Each front-coded block is read once, in parallel,
  without allocating a string per term. A `CONTAINS` needle is searched with a substring
  finder built once. A passing key must be valid UTF-8, and string lengths count Unicode
  scalar values while preserving the literal argument rules. Each parallel task tests
  with its own
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
  4 ms in the server, against 14 ms when the name column is materialized first.
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
  tenfold. A bulk load or compaction therefore counts the characteristic sets of the subjects,
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
  bands. It takes 3.8 ms warm and 90 ms cold, against 18.8 ms and 1,040 ms with
  per-band label lookups.
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
* Groups linked only by a FILTER equality use a keyed inner join when `keyed_join` is
  enabled. The keys propose candidate pairs and the full FILTER still decides which
  pairs pass. An eager LIMIT bounds the larger input in growing prefixes. A large ASK
  instead reads both inputs alternately, starting with 64 rows, and stops at its first
  matching pair. Hash joins below that ASK can probe a triple-pattern index when
  conservative scan and key-multiplicity bounds fit the output and memory budgets.
  This avoids collecting a whole input before their first output. Unproved expansion
  and finite produced-row caps retain the original streaming hash join. The alternating join can hold
  prefixes of both inputs, which uses more memory than one hash-build input. It charges
  that storage before allocation and drops obsolete history once an input ends.
* An eager ASK over a bounded pure UNION compares work before the first answer. An
  unfiltered scan uses its requested prefix. VALUES, filters, sorts and probes retain
  their full costs, and joins include the input eager execution builds in full.
  Nested UNION estimates include later operands until they meet the remaining demand.
  On an unchanged snapshot without a total produced-row cap, the whole ASK must pass
  the whitelist before pure projections, filters, constant bindings, index joins and
  the limited side of joins pass the requested demand instead of at least 64 rows.
  SELECT and effectful consumers retain their order and demand policy. Explicit
  streaming uses its existing operators. A finite produced-row cap keeps the original
  prefixes, since geometric retries can exceed a sum of full-plan rows.
  Branch order changes only when conservative output bounds fit the row and memory
  budgets. Covering block counts bound scan rows before graph filtering. Join bounds
  use the product of their inputs, including compatible UNDEF keys, and UNION bounds
  sum their operands. Index probes retain their original order because cheap metadata
  does not bound duplicate-key fanout. Pure demand admission is independent, so reducing
  excess demand can still avoid unnecessary work. Retrying filters, probes and nested
  joins keep the ordinary initial prefix when a new input prefix would rebuild a full
  join input and conservative bounds prove that widening fits the row and memory
  limits. Otherwise they retain exact demand. Row-preserving projections and constant
  bindings still pass exact demand.
* A pattern with one free variable and at most four estimated rows is eligible for
  `point_patterns`. The planner reads at most two rows and substitutes a constant only
  when exactly one row exists in the query snapshot. It restores the variable in the
  result and preserves RDF term identity, graph scope and multiplicity. The substitution
  also makes arithmetic FILTER bounds constant. Filters with volatile functions,
  application callbacks and occurrences in other algebra nodes keep their bindings.
* **Keyed left joins** (`keyed_left_join`). Without this path, an OPTIONAL whose two
  sides share no variable joins every left row with every right row before its FILTER
  runs. When
  the FILTER has a conjunct `?a = ?b` with one variable from each side, the join pairs
  the rows through that equality instead. IRIs, blank nodes, simple strings and literals
  of uninterpreted datatypes use their term ids. Language strings use keys that normalize
  the case of their tags. Interpreted datatypes and triple terms meet other values in
  that category, and unbound values meet none. The whole FILTER is then tested
  on these candidate pairs, 64K pairs at a time, so the result and its bag multiplicity
  are the product's. SP²Bench `q6` links its OPTIONAL to the outer pattern only by
  `?author = ?author2`. At 10M triples the product has 1.58 × 10¹² rows, while the
  candidate pairs number 7.4 million. EXPLAIN notes `[keyed on ?a = ?b: N
  candidate pairs]`.
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
  A single two-column probe whose other variable is already present in the input can
  narrow each primary-key span by a bound secondary value. It verifies secondary
  ordering within equal primary keys before binary search, with cancellation and
  deadline checks during that pass. This uses the existing probe table without another
  index allocation. Equal secondary rows keep their multiplicity, and an unbound value
  or unordered secondary column retains the complete compatibility scan.
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

## Cold reads

These switches matter on a server whose index and vocabulary files are not in the page
cache, such as the first queries after a restart. With the files in memory they cost
little and change nothing. Each one has the same answers with it on and off.
[X07](../specs/X07-cold-sparse-reads.md) has the design and the measurements.

* **Constants requested together** (`prefetch_constants`). Before the planner looks up
  the constants of a query one by one, it collects the IRIs and literals of the patterns,
  paths, VALUES rows, graph names and the common FILTER comparisons. It asks for the
  sparse-index group of every one of them in one pass (`Vocab::prefetch_keys`), so the
  group reads overlap instead of following each other. It visits at most 4,096 keys,
  and it does nothing without a sparse index or with the read hints off. A table of
  1,024 key hashes per vocabulary remembers the keys whose group was already asked for,
  so a warm constant costs a hash and a load instead of a search of the sparse index.
* **Scans requested before they run** (`prefetch_first_scan`). When the plan is built,
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
  not change the mapping's random-access advice. `SPARKLES_READ_WINDOW_KB` sets the
  target, 4096 KiB by default. This bounds requested bytes, not physical I/O completion.
  A pass consults its window only at a block the cache does not hold, so a warm pass does
  no read-ahead work. Passes over the vocabulary in id order use the same configured size.
* **Terms of small inputs** (`prefetch_terms`). A FILTER, BIND or ORDER BY over a table of
  2 to 4,095 rows evaluates the rows one at a time. Before the first row, it asks for the
  vocabulary blocks of the terms it will read. With a numeric column it also asks for the
  pages of the values, and for the pages of the kinds and the rank when the column keeps
  them mapped. It reads no value and builds no result, and each page is asked for once per
  mapping, so a warm call makes no system call. It does locate each id, which costs a
  memory read. Larger tables decode their terms in id order as before. The native JSON
  result writer asks for the blocks of the terms it prints, as the other result writers do.
* **Parallel decode** (`parallel_decode`). An index join that has asked the kernel for
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
* **Decoding the next cursor batch ahead** (`decode_ahead`). A result writer that reads
  a cursor decodes the terms of the next batch on another Rayon thread while it writes the
  current one. It writes the first batch before it pulls the second, so the first rows
  wait for one batch only, and it reads ahead only when the memory budget has room for
  twice what the first batch took. The decoded terms of a batch are charged to the
  query's memory ledger, and a batch whose terms the ledger refuses is decoded cell by
  cell as before. With one Rayon thread the writer decodes in turn. Together with
  charging each written row's terms once, this took a streamed TSV export of 500,000 rows
  on forge from 134.7 ms to 72 ms at 12 threads, where eager execution takes 84 ms.
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
