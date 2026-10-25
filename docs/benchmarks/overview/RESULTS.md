# Benchmark overview results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Measurement coverage

Implementation status and measurement coverage are separate. An available capability
can have no peer comparison, and an internal regression guard does not establish a
ranking against another engine. [FEATURES](../../FEATURES.md#capability-status) lists complete,
partially complete and planned implementation scope. No unshipped capability is marked
in progress solely because its specification exists.

| Area | Measurement status | Scope |
|---|---|---|
| Query, load and write comparisons | Measured with partial coverage | Published tables cover their named data, engines and timing contracts. Larger suites, sustained workloads and broader hardware remain open. |
| Bindings and model-assisted workflows | Partial coverage | The listed APIs, models and question sets have measurements. Selected API controls and internal A/B runs do not replace a complete peer-suite refresh. |
| Spatial and query-time reasoning comparisons | Unmeasured against peers | Conformance and internal measurements do not establish comparative performance. |
| Shared evidence and maintained-analysis specifications | Planned and unmeasured | C25, C26, C27, F22 and C22b are designs, with no implementation or performance claim. |

## Summary

The synthetic suite has 28 queries over generated data at 1.05 million and 10.5 million
triples. Medians are over the queries each engine completes.

| | 1.05M triples | 10.5M triples |
|---|---|---|
| Bulk load | **0.47 s** (QLever 1.36, Fluree 1.38, Oxigraph 4.42, TDB2 4.80) | **3.67 s** (QLever 9.35, Fluree 10.20, Oxigraph 18.22, TDB2 45.70) |
| Fastest of the five engines | 27 of 28 queries | 28 of 28 queries |
| Clear losses | none | none |
| Ties within 10% | Fluree on `count-all`, `distinct-obj`, `star-lookup` and `employee-docs`, Oxigraph on `path-plus` | Fluree on `distinct-obj`, Oxigraph on `path-plus` and `values-star` |
| Against QLever | Faster on 28 of 28, median 2.9× | Faster on 28 of 28, median 3.8× |
| Against Fluree | Faster on 27 of 28, median 3.5× | Faster on all 26 completed, median 10.3× |
| Against Fuseki | Faster on all 27 completed, median 14× | Faster on all 27 completed, median 79× |
| Against Oxigraph | Faster on all 28, median 24× | Faster on all 28, median 100× |
| Throughput, 16 clients | **1,205 q/s** (QLever 475, Fluree 241, Fuseki 114, Oxigraph 25) | **239 q/s** (QLever 90, Fluree 22, Fuseki 12, Oxigraph 1.7) |
| Server RSS after the run | 408 MiB (**QLever 310**, Oxigraph 1,231, Fuseki 1,882, Fluree 3,077) | **780 MiB** (QLever 795, Oxigraph 2,074, Fluree 2,357, Fuseki 4,923) |
| Server RSS less the block cache | 345 MiB | 401 MiB |

On larger and standard workloads:

* **DBpedia at the 50M slice.** Among the 27 queries with complete, agreeing content
  checks across all five engines, Sparkles has the lowest median on 24 warm and 26 cold.
  Fluree is within 5% ahead on two small warm queries. The focused complete-HTTP
  [cursor assessment](../dbpedia/RESULTS.md#cursor-expression-reuse-controls) puts `births-by-decade` at
  50.2 versus QLever's 16.6 ms warm and 68.1 versus 27.6 ms cold, about 3.0× and 2.5×
  slower. Its timing contract differs from the comparative TSV tables.
* **DBpedia, 1.24 billion triples.** Sparkles loads English DBpedia in 480 s, against
  QLever's 1,674 s and Fluree's 2,919 s. The comparative tables report lower times than
  QLever on 29 warm queries with agreeing qualified answer checks and on 29 of 31 cold
  queries. The full-data cursor assessment is an internal A/B and does not establish
  a current peer ranking for `births-by-decade`.
* **WatDiv at 10.97M triples.** Sparkles is fastest on 19 of the 20 templates and ties
  Fluree on S7, with a geometric mean of 4.63 ms against 6.97 ms for Fluree, 13.6 ms for
  Fuseki, 16.2 ms for Oxigraph and 17.9 ms for QLever.
* **Full-text search.** Sparkles is fastest on all seven queries at 1.05M and on six of
  seven at 10.5M, against Fuseki with jena-text and QLever.
* **LUBM(100).** Over the 24.3M triples of the OWL 2 RL closure, which Sparkles
  materializes in 17.8 s, paired `q2` means are 26.8 ms for Sparkles and 60.6 ms for
  QLever. The query guard preserves all fourteen answers.
* **SP²Bench at 10M triples.** Paired forge runs put `q12a` at 3.8 ms against Jena's
  9.0 ms. `q12b` takes 4.0 ms against Oxigraph's 3.7 ms, inside the ten-percent tie band.
  Sparkles is the only
  engine that answers `q5a`, whose rows match an independent oracle. It answers `q6` in
  0.95 s, 115× faster than Jena. Its `q4` answer of 1.96 GB is larger than the default
  response budget.
* **BSBM Explore at 10M triples.** The paired `q5` pooled median is 4.65 ms against
  Fluree's 5.59 ms, with the closest instance within 4%. Oxigraph is faster on q7 and
  q8. Fluree's driver throughput is two to three times Sparkles' in both
  mixes, but it is not ranked because its q8, q11 and q12 answers differ.
* **BSBM BI at 10M triples.** Every engine's q4 and q5 averages pass an exact oracle
  within float rounding. The paired `q2` median is 20.54 ms against QLever's 162.30 ms.
  QLever remains 1.24× faster on q3. No engine completes a driver throughput run with
  answers qualified against the others.
* **Durable writes.** The focused [write assessment](../writes/RESULTS.md#serial-writes-with-reader-work)
  puts new-literal commits near 1.2 ms for Sparkles and 0.6 ms for unsynced Oxigraph at
  10.5M. Sparkles synchronizes before acknowledgement. With sixteen readers, Oxigraph
  completes 2.1–3.0× as many writes but no reads in the thirty-second 10.5M window,
  against Sparkles' 232–245 reads/s. The separate suite churn table records 878 and
  909 Sparkles commits/s at 1.05M and 10.5M under its own contract.
* **Bindings.** Through Jena's own API, Sparkles answers 24 of 28 suite queries faster
  than TDB2 and than Jena's in-memory store. The Python package is faster than pyoxigraph
  on 25 of 28, and the Node.js package is faster than Oxigraph's JavaScript package on 24
  of 28.

Sparkles trades memory for speed by default. Its 1 GiB decoded-block cache and eager
execution explain most of the RSS gap with QLever, and [Memory and disk](../memory/RESULTS.md#memory-and-disk)
gives the cost and the speed of each such default.

## Where Sparkles loses

This table lists every comparison on this page where another engine is ahead by more
than 10%. Fluree's warm advantages on `distinct-obj` at 1.05M and on two small DBpedia
50M queries are within 10%. SP²Bench `q12b` is also within 8% of Oxigraph.

| Case | Reference advantage | Qualification |
|---|---|---|
| WatDiv `L1-1`, 10.97M | Fluree 1.11× | Sparkles has the lowest mean on 19 of 20 templates, ties Fluree on S7 and leads on 81 of 88 instances. |
| BSBM Explore `q7` and `q8`, 10M | Oxigraph 1.2× on `q7` and 1.4× on `q8` | Pooled medians cover three instances per template. Unresolved result differences are excluded from the ranking. |
| BSBM BI `q3`, 10M | QLever 1.24× | The three fixed-instance answer checks agree. The driver cells have not been rerun since [roadmap](../../ROADMAP.md) item 96 removed the budget refusal that failed them. |
| Changed-store `two-hop-count`, 1.05M | Fluree 1.10× | Sparkles takes 8.6 ms against 7.8 ms with 5,000 changes in its delta, and 5.1 ms on the loaded store. |
| Serial durable commits | Unsynced Oxigraph about 2.0× on literal inserts at 10.5M in the focused assessment | Sparkles takes 1.167–1.254 ms against 0.594–0.625 ms. The separate suite latency table gives Oxigraph a 1.84× advantage at 1.05M under its sampling contract. |
| Writes during mixed load | Oxigraph 3.4× at 1.05M in the suite and 2.1–3.0× at 10.5M in the focused assessment | At 10.5M in the focused assessment, Oxigraph completes no reads in the thirty-second window, while Sparkles completes 232–245 reads/s. Durability and completed reader work differ. |
| Cold point lookups | Oxigraph 1.5–3.0× at 1.05M and 1.3–2.9× at 10.5M | Listed under [Cold starts](../synthetic/RESULTS.md#cold-starts). |
| Cold small scans at 1.05M | Fluree 1.17–3.0× on `count-all`, `contains`, `distinct-obj` and `employee-docs` | Fluree is not ahead on any cold query at 10.5M. |
| DBpedia 50M slice `births-by-decade` | QLever 3.0× warm and 2.5× cold in the complete-HTTP assessment | Sparkles takes 50.2 ms warm and 68.1 ms cold, against 16.6 and 27.6 ms. The comparative TSV tables use another timer contract, and the full-data internal guard establishes no current peer ranking. |
| DBpedia cold `outlink-classes-2` | QLever 1.25× | Sparkles wins 29 of 31 cold queries. |
| Server RSS after the run, 1.05M | QLever 1.32× | Without its block cache, Sparkles holds 345 MiB. Sparkles has the lowest RSS after the run at 10.5M. |
| Peak RSS under 16 readers | QLever 1.67× at 1.05M and 1.43× at 10.5M | Sparkles peaks at 567 and 1,339 MiB, against 339 and 939 MiB. |
| Bulk load peak RSS | QLever 2.90× on the LUBM(100) closure and 1.23× on SP²Bench, Fluree 1.26× on SP²Bench | Sparkles has the lowest peak at 1.05M and on DBpedia, and is within 10% of QLever at 10.5M. |
| DBpedia server RSS | QLever 3.14× | Sparkles maps its index, so touched file pages count toward its RSS. |
| Text index size | QLever about 1.18× at 1.05M and 1.23× at 10.5M | The indexed predicates and features differ. |
| JVM small lookups | TIM 1.3–2.1× and TDB2 1.1–1.4× on `star-lookup`, `values-star` and `employee-docs` | A small query's fixed cost through Jena dominates. |
| Python small lookups | pyoxigraph 1.49× on value lookups and `knows-reach`, and 1.23× on subject scans | The gaps are 0.67 µs per value, 20 µs per query and 2.06 µs per subject. `values-star` is within 10%, and Sparkles leads on employee and star lookups. |
| Node.js small lookups and adds | Oxigraph's package 1.1–1.4× on four small queries, N3.js 1.2–1.5× on adds and `has()` | Each add crosses to the transaction's thread. |
| Node.js `export-500k` | N3.js with Comunica 1.44× | Each row's terms cross into JavaScript. |
