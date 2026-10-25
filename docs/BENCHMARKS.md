# Benchmarks

This page reports how fast Sparkles loads, queries and writes RDF, how much memory and
disk it uses for that speed, and how its Python, JVM and Node.js bindings compare with
the libraries they replace. The comparisons run the same data and queries on every engine,
check every engine's answers before timing them, and list every case where Sparkles loses.
[BENCHMARKING.md](BENCHMARKING.md) explains how to run the harnesses.

| Measurement context | |
|---|---|
| Hardware | `forge` has an Intel Core i5-13500 with 6 performance and 8 efficiency cores and 20 threads, 16 GiB of RAM and an NVMe SSD. `atlas` has the same CPU and runs the bindings. A development laptop or a Namespace instance is used only where a section says so. |
| OS and kernel | NixOS, Linux 6.18 |
| Sparkles | 0.1.0, release builds with default features, rustc 1.98 and 1.99 |
| Other engines | QLever 0.5.48, Fluree 4.2.2 (the release binary), Oxigraph 0.5.11 (server, pyoxigraph and the JavaScript package), Apache Jena TDB2 6.2.0 with Fuseki 5.1.0, Jena 5.6.0 (TDB2 and TIM in the JVM comparison), rdflib 7.6.0, N3.js 2.13.11 with Comunica 5.4.1 |
| Dates | Each table names the host, the date and the build it comes from. A section that names no date comes from the runs of 2026-10-03 to 2026-10-09 on forge. |

Every engine is timed alone under a 12 GiB memory limit, with the timing client on CPU 11
and result caches off. [Methodology and reproduction](#methodology-and-reproduction) gives
the settings of each engine and the commands that reproduce each table.

* [Summary](#summary)
* [Where Sparkles loses](#where-sparkles-loses)
* [Synthetic suite](#synthetic-suite)
* [WatDiv](#watdiv)
* [SP²Bench](#sp2bench)
* [LUBM and reasoning](#lubm-and-reasoning)
* [DBpedia](#dbpedia)
* [BSBM](#bsbm)
* [Full-text search](#full-text-search)
* [Vector search](#vector-search)
* [Spatial queries](#spatial-queries)
* [Paths and graph algorithms](#paths-and-graph-algorithms)
* [Engine features and overheads](#engine-features-and-overheads)
* [Loading and index builds](#loading-and-index-builds)
* [Writes and updates](#writes-and-updates)
* [Bindings](#bindings)
* [Natural-language questions](#natural-language-questions)
* [Memory and disk](#memory-and-disk)
* [Methodology and reproduction](#methodology-and-reproduction)

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
  QLever wins `births-by-decade` by 4.9× warm and 1.7× cold, and Fluree is within 5% ahead
  on two small warm queries.
* **DBpedia, 1.24 billion triples.** Sparkles loads English DBpedia in 480 s, against
  QLever's 1,674 s and Fluree's 2,919 s. It is faster than QLever on all 29 warm queries
  that both answer correctly and on 29 of 31 cold ones.
* **WatDiv at 10.97M triples.** Sparkles is fastest on 19 of the 20 templates and ties
  Fluree on S7, with a geometric mean of 4.63 ms against 6.97 ms for Fluree, 13.6 ms for
  Fuseki, 16.2 ms for Oxigraph and 17.9 ms for QLever.
* **Full-text search.** Sparkles is fastest on all seven queries at 1.05M and on six of
  seven at 10.5M, against Fuseki with jena-text and QLever.
* **LUBM(100).** Over the 24.3M triples of the OWL 2 RL closure, which Sparkles
  materializes in 17.8 s, Sparkles is fastest on 13 of 14 queries. QLever is 3.7× faster
  on `q2`.
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
* **Durable writes.** A commit that adds a new literal takes a median of 1.13 ms at 1.05M
  and 1.28 ms at 10.5M, synchronized to disk before it is acknowledged. Oxigraph commits
  faster without synchronizing in that harness. In the suite's churn of 5,000 commits,
  Sparkles commits 878 and 909 per second at 1.05M and 10.5M, ahead of every other
  engine.
* **Bindings.** Through Jena's own API, Sparkles answers 24 of 28 suite queries faster
  than TDB2 and than Jena's in-memory store. The Python package is faster than pyoxigraph
  on 25 of 28, and the Node.js package is faster than Oxigraph's JavaScript package on 24
  of 28.

Sparkles trades memory for speed by default. Its 1 GiB decoded-block cache and eager
execution explain most of the RSS gap with QLever, and [Memory and disk](#memory-and-disk)
gives the cost and the speed of each such default.

## Where Sparkles loses

This table lists every comparison on this page where another engine is ahead by more
than 10%. Fluree's warm advantages on `distinct-obj` at 1.05M and on two small DBpedia
50M queries are within 10%. SP²Bench `q12b` is also within 8% of Oxigraph.

| Case | Reference advantage | Qualification |
|---|---|---|
| WatDiv `L1-1`, 10.97M | Fluree 1.11× | Sparkles has the lowest mean on 19 of 20 templates, ties Fluree on S7 and leads on 81 of 88 instances. |
| BSBM Explore `q7` and `q8`, 10M | Oxigraph 1.2× on `q7` and 1.4× on `q8` | Pooled medians cover three instances per template. Wrong answers are excluded from the ranking. |
| BSBM BI `q3`, 10M | QLever 1.24× | The three fixed-instance answer checks agree. The driver cells have not been rerun since [roadmap](ROADMAP.md) item 96 removed the budget refusal that failed them. |
| LUBM(100) `q2` | QLever 3.7× | Sparkles is fastest on 13 of 14 queries. |
| Changed-store `two-hop-count`, 1.05M | Fluree 1.10× | Sparkles takes 8.6 ms against 7.8 ms with 5,000 changes in its delta, and 5.1 ms on the loaded store. |
| Serial durable commits | Oxigraph 1.84× on literal inserts at 1.05M and 1.39× at 10.5M | The same durability difference. |
| Writes during mixed load | Oxigraph 3.4× at 1.05M and 2.1× at 10.5M | Oxigraph serves far fewer concurrent reads. |
| Cold point lookups | Oxigraph 1.5–3.0× at 1.05M and 1.3–2.9× at 10.5M | Listed under [Cold starts](#cold-starts). |
| Cold small scans at 1.05M | Fluree 1.17–3.0× on `count-all`, `contains`, `distinct-obj` and `employee-docs` | Fluree is not ahead on any cold query at 10.5M. |
| DBpedia 50M slice `births-by-decade` | QLever 4.9× warm and 1.7× cold | Sparkles takes 54.4 ms warm and 71.4 ms cold. At 1.24 billion triples Sparkles is faster on this query. |
| DBpedia cold `outlink-classes-2` | QLever 1.25× | Sparkles wins 29 of 31 cold queries. |
| Server RSS after the run, 1.05M | QLever 1.32× | Without its block cache, Sparkles holds 345 MiB. Sparkles has the lowest RSS after the run at 10.5M. |
| Peak RSS under 16 readers | QLever 1.67× at 1.05M and 1.43× at 10.5M | Sparkles peaks at 567 and 1,339 MiB, against 339 and 939 MiB. |
| Bulk load peak RSS | QLever 2.90× on the LUBM(100) closure and 1.23× on SP²Bench, Fluree 1.26× on SP²Bench | Sparkles has the lowest peak at 1.05M and on DBpedia, and is within 10% of QLever at 10.5M. |
| DBpedia server RSS | QLever 3.14× | Sparkles maps its index, so touched file pages count toward its RSS. |
| Text index size | QLever about 1.18× at 1.05M and 1.23× at 10.5M | The indexed predicates and features differ. |
| JVM small lookups | TIM 1.3–2.1× and TDB2 1.1–1.4× on `star-lookup`, `values-star` and `employee-docs` | A small query's fixed cost through Jena dominates. |
| Python small lookups | pyoxigraph 1.2–2.5× on `knows-reach`, `values-star` and `employee-docs` | The same fixed cost per query. |
| Node.js small lookups and adds | Oxigraph's package 1.1–1.4× on four small queries, N3.js 1.2–1.5× on adds and `has()` | Each add crosses to the transaction's thread. |
| Node.js `export-500k` | N3.js with Comunica 1.44× | Each row's terms cross into JavaScript. |

## Synthetic suite

`scripts/gen-data.py` generates people, organisations, documents, typed literals,
`foaf:knows` and citation graphs, and the 28 queries of `scripts/bench.sh` cover scans,
counts, joins, OPTIONAL, property paths, subqueries, grouping, sorting, point lookups and
expressions. Queries are SPARQL protocol POSTs with TSV results, timed by hyperfine
around `curl`, so each time includes about 3.5 ms of process start and round trip. Times
are mean ± σ in ms over 10 runs after 2 warm-ups, and bold marks the fastest engine. Most
queries at 1.05M are close to the cost of starting curl and making the request.
`mise run bench` reproduces these tables.

### Synthetic suite at 1.05M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| count-all | **3.5 ± 0.1** | 170.1 ± 28.1 | 12.0 ± 1.0 ‡ | 3.6 ± 0.3 | 256.4 ± 3.1 |
| types-grouped | **3.6 ± 0.1** | 79.3 ± 6.7 | 5.4 ± 0.1 ‡ | 18.0 ± 1.9 | 43.5 ± 2.2 |
| star-join | **7.3 ± 0.3** | 30.9 ± 1.4 | 32.9 ± 5.5 ‡ | 18.3 ± 0.3 | 400.6 ± 18.5 |
| two-hop-count | **5.1 ± 0.2** | 386.8 ± 0.5 | 21.6 ± 2.9 ‡ | 7.0 ± 0.2 | 836.2 ± 6.5 |
| range-topk | **4.3 ± 0.1** ‡ | 95.2 ± 1.8 | 6.2 ± 0.2 | 20.0 ± 1.0 | 59.9 ± 3.0 |
| optional-count | **4.1 ± 0.2** | 81.7 ± 2.6 | 10.0 ± 0.5 ‡ | 13.8 ± 0.1 | 105.5 ± 8.6 |
| contains | **4.2 ± 0.2** | 42.7 ± 1.8 | 74.1 ± 3.4 ‡ | 8.7 ± 0.3 | 143.9 ± 3.6 |
| group-avg | **8.9 ± 0.1** | 210.1 ± 2.7 ‡ | 16.3 ± 1.6 ‡ | 56.1 ± 1.4 ‡ | 314.9 ± 5.6 |
| path-plus | **3.5 ± 0.1** | 6.3 ± 0.7 | 6.3 ± 0.3 ‡ | 7.3 ± 0.2 | 3.7 ± 0.3 |
| distinct-obj | 3.7 ± 0.1 | 92.1 ± 2.0 | 17.6 ± 2.6 ‡ | **3.6 ± 0.1** | 70.2 ± 1.5 |
| export-500k | **78.0 ± 1.6** | 500.5 ± 16.8 | 792.4 ± 17.1 | 219.9 ± 13.8 | 1738.4 ± 26.6 |
| predicate-counts | **3.5 ± 0.1** | 336.0 ± 2.8 | 15.1 ± 0.6 ‡ | 65.5 ± 0.6 | 288.5 ± 3.4 |
| order-by-full | **46.4 ± 6.1** | 540.2 ± 5.4 | 141.2 ± 3.1 | 105.4 ± 0.5 | 3852.4 ± 59.5 |
| optional-chain | **14.9 ± 0.5** | 228.3 ± 1.7 | 67.0 ± 6.7 | 1773.6 ± 88.9 | 358.6 ± 7.4 |
| minus | **3.8 ± 0.2** | 41.4 ± 1.3 | 7.2 ± 0.2 ‡ | 9.9 ± 0.3 | 32.6 ± 1.9 |
| not-exists | **4.6 ± 0.2** | 57.8 ± 1.0 | 7.2 ± 0.4 ‡ | 16.7 ± 0.9 | 113.9 ± 8.8 |
| exists-join | **5.0 ± 0.2** | 152.0 ± 1.0 | 9.9 ± 0.3 ‡ | 30.5 ± 6.3 | 1451.6 ± 21.6 |
| subquery-agg | **3.7 ± 0.3** | 52.3 ± 1.3 | 4.9 ± 0.1 ‡ | 25.7 ± 16.5 | 27.7 ± 4.4 |
| regex-iri | **4.3 ± 0.2** | 56.6 ± 2.0 | 72.8 ± 3.8 ‡ | 77.5 ± 0.7 | 171.7 ± 7.3 |
| knows-reach | **16.1 ± 0.2** | error | 60.5 ± 6.1 ‡ | 58.2 ± 0.4 | 346.5 ± 4.0 |
| distinct-join | **8.2 ± 0.2** | 423.4 ± 1.4 | 25.9 ± 4.7 | 90.8 ± 3.5 | 679.6 ± 14.0 |
| lang-filter | **4.2 ± 0.2** | 32.3 ± 1.5 | 44.1 ± 3.8 ‡ | 6.3 ± 0.2 | 81.6 ± 6.1 |
| star-lookup | **3.7 ± 0.2** ‡ | 6.0 ± 0.4 ‡ | 10.5 ± 0.5 ‡ | 3.7 ± 0.1 | 4.7 ± 0.3 |
| values-star | **3.6 ± 0.2** | 6.5 ± 0.5 | 11.1 ± 0.7 ‡ | 4.0 ± 0.1 | 4.0 ± 0.1 |
| employee-docs | **3.6 ± 0.1** | 6.6 ± 0.2 | 6.4 ± 0.2 | 3.8 ± 0.1 | 4.3 ± 0.1 |
| expr-bind-group | **4.5 ± 0.3** | 49.8 ± 2.0 ‡ | 7.4 ± 0.2 ‡ | 45.2 ± 0.2 | 42.1 ± 4.9 |
| expr-order-key | **5.1 ± 0.2** | 93.6 ± 3.1 | 13.7 ± 1.1 ‡ | 115.0 ± 1.0 | 2660.9 ± 15.5 |
| expr-agg-arg | **8.3 ± 0.5** | 172.6 ± 1.5 | 15.5 ± 2.2 ‡ | 74.3 ± 3.2 | 300.0 ± 5.6 |
| **throughput** star-join, 16 clients (queries/s) | **1205** | 114 | 475 | 241 | 25 |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-18 and 2026-10-19. The [record](benchmarks/2026-10-23-synthetic-1.05m-forge.json)
has every sample and answer fingerprint.*

### Synthetic suite at 10.5M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| count-all | **3.6 ± 0.2** | 1855.8 ± 6.6 | 51.7 ± 3.3 ‡ | error | 2515.5 ± 18.0 |
| types-grouped | **3.6 ± 0.2** | 3722.1 ± 792.9 | 6.1 ± 0.3 ‡ | 110.6 ± 0.9 | 386.4 ± 7.7 |
| star-join | **36.7 ± 4.2** | 327.2 ± 136.4 | 118.5 ± 10.5 ‡ | 140.9 ± 3.3 | 4789.6 ± 32.7 |
| two-hop-count | **17.5 ± 1.0** | 3954.2 ± 30.2 | 66.9 ± 6.4 ‡ | 35.9 ± 0.2 | 8294.6 ± 71.1 |
| range-topk | **6.6 ± 0.4** | 882.2 ± 3.5 | 21.7 ± 3.5 | 153.1 ± 0.5 | 595.3 ± 6.6 |
| optional-count | **10.6 ± 1.4** | 1102.5 ± 129.9 | 46.2 ± 3.2 ‡ | 113.1 ± 0.7 | 997.2 ± 16.5 |
| contains | **7.0 ± 0.2** | 2669.0 ± 880.1 | 689.3 ± 3.6 ‡ | 18.0 ± 1.0 | 1537.5 ± 19.1 |
| group-avg | **47.7 ± 0.4** | 3759.1 ± 620.2 ‡ | 93.8 ± 8.5 ‡ | 547.9 ± 9.3 ‡ | 4223.8 ± 47.2 |
| path-plus | **3.5 ± 0.1** | 6.7 ± 0.8 | 9.9 ± 1.4 ‡ | 50.5 ± 0.8 | 3.7 ± 0.1 |
| distinct-obj | **3.5 ± 0.1** | 1387.0 ± 58.1 | 205.8 ± 4.9 ‡ | 3.7 ± 0.1 | 748.3 ± 3.4 |
| export-500k | **78.4 ± 0.6** | 535.7 ± 15.9 | 674.4 ± 5.4 | 289.5 ± 1.0 | 2113.7 ± 15.2 |
| predicate-counts | **3.5 ± 0.1** | 3203.1 ± 62.9 | 72.4 ± 2.9 ‡ | 597.2 ± 18.3 | 2821.0 ± 18.4 |
| order-by-full | **367.4 ± 12.8** | 16333.8 ± 370.2 | 1379.5 ± 9.7 | 1848.2 ± 54.3 | 55652.5 ± 120.9 |
| optional-chain | **82.0 ± 5.2** | 2906.7 ± 61.5 | 558.9 ± 9.4 | error | 4917.9 ± 21.7 |
| minus | **7.6 ± 0.4** | 543.8 ± 24.4 | 19.4 ± 1.9 ‡ | 52.1 ± 2.5 § | 354.0 ± 2.4 |
| not-exists | **15.0 ± 1.1** | 569.5 ± 150.3 | 25.7 ± 2.9 ‡ | 147.5 ± 1.4 § | 1042.3 ± 13.1 |
| exists-join | **15.3 ± 1.3** | 1582.9 ± 26.2 | 61.6 ± 3.6 ‡ | 263.2 ± 4.7 § | 122805.6 ± 269.9 |
| subquery-agg | **4.6 ± 0.1** | 593.1 ± 9.4 | 14.8 ± 0.7 ‡ | 140.4 ± 3.0 § | 182.8 ± 1.6 |
| regex-iri | **8.3 ± 0.1** | 4029.7 ± 119.8 | 660.3 ± 4.9 ‡ | 742.4 ± 2.4 § | 2701.9 ± 8.4 |
| knows-reach | **145.5 ± 0.7** | error | 1142.2 ± 44.2 ‡ | 639.3 ± 6.4 § | 4952.2 ± 27.4 |
| distinct-join | **49.0 ± 0.8** | 4415.9 ± 9.7 | 155.2 ± 6.6 | 1196.9 ± 10.2 § | 6666.6 ± 25.4 |
| lang-filter | **6.2 ± 0.2** | 1683.5 ± 1324.2 | 385.8 ± 1.9 ‡ | 29.3 ± 0.5 § | 855.0 ± 10.2 |
| star-lookup | **3.7 ± 0.1** ‡ | 7.1 ± 0.7 | 24.9 ± 3.2 ‡ | 4.1 ± 0.2 § | 5.1 ± 0.2 |
| values-star | **3.9 ± 0.5** | 8.0 ± 0.5 | 10.8 ± 0.6 ‡ | 4.2 ± 0.2 § | 4.0 ± 0.2 |
| employee-docs | **3.6 ± 0.1** | 7.1 ± 0.6 | 13.6 ± 1.4 | 4.4 ± 0.1 § | 4.5 ± 0.3 |
| expr-bind-group | **9.6 ± 0.8** | 421.7 ± 13.6 ‡ | 34.2 ± 4.2 ‡ | 386.3 ± 1.7 § | 352.8 ± 4.9 |
| expr-order-key | **16.5 ± 2.0** | 877.1 ± 13.1 | 108.9 ± 2.6 ‡ | 1090.7 ± 6.6 § | 38261.1 ± 72.7 |
| expr-agg-arg | **27.1 ± 0.9** | 1805.7 ± 27.6 | 92.9 ± 9.0 ‡ | 737.6 ± 6.9 § | 4056.5 ± 46.5 |
| **throughput** star-join, 16 clients (queries/s) | **239** | 12 | 90 | 22 | 1.7 |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-18 and 2026-10-19. The [record](benchmarks/2026-10-23-synthetic-10.5m-forge.json)
has every sample and answer fingerprint.*

‡ Same values as different RDF terms. QLever returns integers such as counts and ages as
`xsd:int`, where the data and the other engines use `xsd:integer`. Sparkles writes
`xsd:decimal` values in XSD 1.1 canonical form, so the data's `"175000.50"` comes back as
`"175000.5"`. In `group-avg` the averages agree to 12 significant digits.

§ Timed, but the answer was not checked, because the server had been killed earlier in the
answer check. Such a time is not ranked.

Fuseki 5.1 fails `knows-reach` (`<person/0> foaf:knows* ?x`) with a `StackOverflowError`
in its property path engine and answers HTTP 500 at both sizes. Fluree's `optional-chain`
at 10.5M grows past the 12 GiB memory limit and the kernel kills the server, both in the
answer check and in the warm-up pass before the timings. The answer check therefore has no
Fluree answers for the 14 queries after `optional-chain`, and Fluree's `count-all` is an
error because the server was not running when that query was timed. The harness restarts
a server that has stopped before it times the next query.

### Request latency without client startup

Starting `curl` costs more than most small queries take, so this Sparkles-only run times
requests from a persistent client instead. Each query had 30 warm-ups and 20 timed
requests in each of two fresh server processes, and the table gives the pooled median of
the 40 samples in milliseconds, including HTTP and the full TSV response. The client ran
on CPU 11 and the server on CPUs 0–9, with the result cache off.

| Query | 1.05M (ms) | 10.5M (ms) |
|---|---:|---:|
| `contains` | 0.879 | 4.853 |
| `count-all` | 0.200 | 0.200 |
| `distinct-join` | 4.251 | 43.712 |
| `distinct-obj` | 0.236 | 0.208 |
| `employee-docs` | 0.412 | 0.476 |
| `exists-join` | 2.386 | 8.996 |
| `export-500k` | 114.668 | 114.259 |
| `expr-agg-arg` | 4.194 | 43.005 |
| `expr-bind-group` | 1.484 | 5.073 |
| `expr-order-key` | 1.995 | 15.748 |
| `group-avg` | 4.484 | 49.517 |
| `knows-reach` | 11.358 | 140.175 |
| `lang-filter` | 0.822 | 3.390 |
| `minus` | 0.749 | 4.543 |
| `not-exists` | 1.894 | 9.663 |
| `optional-chain` | 10.040 | 75.501 |
| `optional-count` | 0.827 | 6.862 |
| `order-by-full` | 36.660 | 405.727 |
| `path-plus` | 0.219 | 0.204 |
| `predicate-counts` | 0.232 | 0.230 |
| `range-topk` | 3.346 | 9.093 |
| `regex-iri` | 1.200 | 7.338 |
| `star-join` | 3.567 | 26.918 |
| `star-lookup` | 0.639 | 0.362 |
| `subquery-agg` | 0.880 | 0.793 |
| `two-hop-count` | 1.274 | 11.563 |
| `types-grouped` | 0.222 | 0.225 |
| `values-star` | 0.587 | 0.296 |

### Cold starts

Cold mode restarts the server and evicts its files from the OS page cache before each
query, three times per query, and reports the median request time without process
startup. Eviction does not guarantee that the device itself is cold. A Sparkles server is
ready 0.09 s after a restart at both scales. Sparkles is fastest on 21 of 28 cold queries
at 1.05M and on 24 of 28 at 10.5M. The cold losses are these:

| Query, cold | Faster engine | 1.05M | 10.5M |
|---|---|---|---|
| `count-all` | Fluree at 1.05M | 1.55× (3.4 against 2.2 ms) | — |
| `two-hop-count` | Fluree at 1.05M | 1.05× (18.6 against 17.7 ms) | — |
| `contains` | Fluree at 1.05M | 1.17× (14.2 against 12.1 ms) | — |
| `path-plus` | Oxigraph | 2.95× (6.5 against 2.2 ms) | 2.94× (4.7 against 1.6 ms) |
| `distinct-obj` | Fluree at 1.05M | 3.0× (4.8 against 1.6 ms) | — |
| `star-lookup` | Oxigraph at 10.5M | — | 1.31× (20.1 against 15.4 ms) |
| `values-star` | Oxigraph | 1.51× (8.0 against 5.3 ms) | 1.50× (8.1 against 5.4 ms) |
| `employee-docs` | Fluree at 1.05M, Oxigraph at 10.5M | 1.38× (11.7 against 8.5 ms) | 1.07× (14.7 against 13.7 ms) |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-19. Medians of three restarts are noisy at these sizes.*

Sparse point lookups and several small scans lose because warm execution hides their
first-use reads and block decoding. Sparkles memory-maps its index and vocabulary, and
these features limit what a cold lookup reads:

* Random-access mappings and explicit read-ahead request only the columns or term ranges
  a query reads. `SPARKLES_IO_HINTS=off` disables them.
* A sparse vocabulary index, `vocab.idx`, holds the first key of every 128th front-coded
  block. It narrows a lookup to one range of offsets and data, and costs 66 KB at 10.5M and
  8.4 MB of memory at 1.01B quads. `SPARKLES_SPARSE_VOCAB=off` disables its use.
* An index join with up to 16,384 keys requests its blocks before it decodes the first, so
  their reads overlap.
* A filter, sort or aggregate over non-canonical numeric literals reads 8-byte values from
  the [numeric column](#numeric-literals-in-the-vocabulary) instead of decoding each key
  from the vocabulary.

The tradeoff is faster sparse lookups against less automatic read-ahead for dense scans.
These cold medians switch the first two features together.

| Cold, median of 3, ms | Both off | Both on (default) |
|---|---:|---:|
| 10.5M `star-lookup` | 69.3 | **46.7** |
| 10.5M `star-join` | 99.9 | **93.3** |
| 10.5M `contains` | 82.4 | **33.5** |
| 10.5M `regex-iri` | 55.2 | **31.3** |
| 10.5M `lang-filter` | 36.1 | **27.9** |
| 1.01B `entity-facts-1` | 36.0 to 38.3 | **14.6 to 15.2** |
| 1.01B `label-regex` | **66.4 to 68.5** | 119.5 to 130.8 |
| 1.01B `export-1m` | **1,231 to 1,239** | 1,883 to 1,956 |
| 1.01B `category-tree` | 369.7 to 371.1 | 364.5 to 365.2 |
| 1.01B `top-linked` | 949 to 1,000 | 946 to 964 |

[X07](specs/X07-cold-sparse-reads.md#remaining-cold-gaps) adds one read window per scan
pass, batched hints for a query's constants and first-scan prefetch, and profiles the
remaining cold time.

Index format 3 splits each column of a block into frames of 2,048 rows, so a point lookup
decodes a sixteenth of a column. This table compares stores in format 3 with stores whose
blocks are not segmented, each loaded by its own build. Each 10.5M cell is the median of
four interleaved rounds of 11 samples timed by `bench-regressions.py --cold`, and each
1.05M cell the median of two. These times come from a Python client and are not
comparable with the `bench.sh` times above.

| Cold, forge, median ms | 1.05M unsegmented | 1.05M format 3 | 10.5M unsegmented | 10.5M format 3 |
|---|---:|---:|---:|---:|
| `star-lookup` | 15.4 | **14.9** | 20.4 | **12.7** |
| `employee-docs` | 13.2 | **11.7** | 16.8 | **12.2** |
| `values-star` | 9.8 | **7.7** | 10.1 | **7.6** |
| `path-plus` | 8.0 | **5.0** | 6.7 | **4.7** |
| `knows-reach` | 33.5 | **32.1** | 198.9 | **197.4** |
| `star-join` | 27.8 | **25.8** | **82.7** | 89.2 |
| `export-500k` | 121.8 | **120.4** | 129.8 | **127.7** |

*Measured on forge on 2026-10-23, with the format 3 build at commit e86a371a.*

`star-join` at 10.5M is the one cold query that is slower, by 6% to 8% in two runs of 12
rounds. Its index join reads up to a quarter of a block's segments one by one before the
block is read whole, and [X07](specs/X07-cold-sparse-reads.md#phase-3-results) records the
fixes that were tried. Warm instruction counts per request stay within 0.969 to 1.024 of
unsegmented blocks, and a warm pass makes no read hint. The store at 10.5M grows by 3.0%,
from 300.6 MB to 309.7 MB, and the load takes 3.7 seconds with a peak of about 2.05 GB
either way.

## WatDiv

WatDiv v0.6 basic testing at scale 100 has 10,973,381 triples and 20 templates with
five instances each, except C1 to C3, which have one, for 88 instances. Times are
geometric means over each template's ten-run instance means after two warm-up requests,
and category means weight templates equally. Every engine's answers agreed by RDF value
multiset for every instance. Agreement is against the other engines, rather than an
independent reference oracle. `mise run bench:watdiv 100` reproduces the run.

| template | instances | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|---:|
| L1 | 5 | **3.59** | 7.59 | 9.39 | 3.62 | 4.44 |
| L2 | 5 | **3.70** | 12.4 | 8.94 | 5.18 | 14.5 |
| L3 | 5 | **3.55** | 6.68 | 6.88 | 3.84 | 4.16 |
| L4 | 5 | **3.71** | 8.04 | 6.84 | 4.09 | 7.33 |
| L5 | 5 | **3.76** | 11.0 | 11.1 | 4.64 | 13.3 |
| S1 | 5 | **4.08** | 6.74 | 92.6 | 4.29 | 6.72 |
| S2 | 5 | **4.01** | 6.55 | 7.84 | 4.36 | 6.53 |
| S3 | 5 | **3.78** | 10.4 | 11.0 | 5.14 | 14.8 |
| S4 | 5 | **3.61** | 13.1 | 7.62 | 5.01 | 28.4 |
| S5 | 5 | **3.67** | 6.90 | 10.2 | 4.35 | 13.4 |
| S6 | 5 | **3.63** | 6.61 | 6.99 | 3.90 | 6.19 |
| S7 | 5 | 3.56 | 5.39 | 9.48 | **3.56** | 3.65 |
| F1 | 5 | **3.85** | 8.00 | 16.0 | 4.12 | 14.1 |
| F2 | 5 | **3.80** | 7.09 | 33.0 | 4.20 | 8.42 |
| F3 | 5 | **3.78** | 6.31 | 17.3 | 27.7 | 6.03 |
| F4 | 5 | **4.42** | 7.35 | 59.0 | 28.2 | 8.86 |
| F5 | 5 | **4.17** | 6.53 | 19.3 | 4.32 | 7.21 |
| C1 | 1 | **6.89** | 17.6 | 36.2 | 7.95 | 28.6 |
| C2 | 1 | **8.07** | 149 | 54.5 | 41.7 | 362 |
| C3 | 1 | **50.9** | 11866 | 303 | 94.8 | 23094 |

| category | templates | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|---:|
| **linear (L)** | 5 of 5 | **3.66** | 8.89 | 8.47 | 4.24 | 7.64 |
| **star (S)** | 7 of 7 | **3.76** | 7.60 | 12.2 | 4.34 | 9.21 |
| **snowflake (F)** | 5 of 5 | **4.00** | 7.03 | 25.3 | 8.99 | 8.56 |
| **complex (C)** | 3 of 3 | **14.1** | 314 | 84.3 | 31.6 | 621 |
| **all templates** | 20 of 20 | **4.63** | 13.6 | 17.9 | 6.97 | 16.2 |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-20. The [WatDiv record](benchmarks/2026-10-23-watdiv-100-forge.json) has the raw
instance timings, answer fingerprints and load measurements.*

Sparkles has the lowest mean on 19 of the 20 templates and on 81 of 88 instances.
Fluree's S7 mean is 0.04% lower, a tie. Fluree has the lower mean on L1-1, L1-5, F2-1,
F5-1, S6-5, S7-1 and S7-2, and Oxigraph on S7-1 and S7-2. The L1-1 difference puts
Sparkles 11% slower, while the others are within 5%. C3 returns 419,866 rows and includes
the complete response.

WatDiv was created by G. Aluç, O. Hartig, M. T. Özsu and K. Daudjee, “Diversified Stress
Testing of RDF Data Management Systems,” ISWC 2014.

<a id="sp2bench"></a>
<a id="sp2bench-at-10m-triples"></a>

## SP²Bench

SP²Bench v1.01 generates bibliographic data in the style of DBLP. At 10M triples its
N-Triples file is 1.49 GB. The 17 queries ran with a request limit of 600 s. Times are
mean ± σ in ms. The paired `q12a` cells and Jena's `q12b` cell use 30 runs after
10 warm-ups. The Sparkles and Oxigraph `q12b` cells pool two reversed-order runs of
60 requests after 15 warm-ups per engine. Other cells use 10 runs after 2 warm-ups. In the original answer check, which kept all five
servers up, the servers of QLever and Fluree were killed during `q4`, so their answers
from `q5a` on were not checked (§). `mise run bench:sp2b 10m` reproduces the run.

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| q1 | **3.5 ± 0.1** | 8.3 ± 0.8 | error | error | 29.6 ± 5.7 |
| q2 | **771.6 ± 11.2** | 35233.4 ± 1583.9 | 3415.2 ± 38.5 ‡ | 3275.0 ± 40.1 | 40256.7 ± 93.7 |
| q3a | **45.1 ± 4.9** | 1281.1 ± 497.7 | 252.9 ± 9.6 | 3756.5 ± 16.8 | 2868.4 ± 35.4 |
| q3b | **5.2 ± 0.2** | 515.3 ± 40.1 | 10.0 ± 0.8 | 2548.7 ± 641.6 | 2035.7 ± 15.5 |
| q3c | **4.6 ± 0.4** | 472.2 ± 10.5 | 7.4 ± 0.5 | 2739.6 ± 695.3 | 2030.6 ± 15.0 |
| q4 | error | timeout | error | error | timeout |
| q5a | **3946.0 ± 665.9** | timeout | timeout | timeout | timeout |
| q5b | **234.9 ± 23.4** | 7036.6 ± 80.5 | 653.9 ± 7.1 § | 2588.3 ± 16.9 § | 21273.6 ± 22.8 |
| q6 | error | **108648.1 ± 454.0** | 2160.0 ± 25.8 § | timeout | timeout |
| q7 | **145.0 ± 7.5** | timeout | 304.1 ± 15.5 § | 5091.6 ± 50.5 § | timeout |
| q8 | **8.3 ± 1.0** | 35.2 ± 2.9 | error | 185.1 ± 1.9 § | 1911.6 ± 52.6 |
| q9 | **158.5 ± 33.5** | 4227.3 ± 15.1 | 284.1 ± 8.6 § | 6160.2 ± 46.5 § | 5202.0 ± 22.1 |
| q10 | **3.8 ± 0.2** | 7.3 ± 0.5 | 6.8 ± 0.6 § | 4.2 ± 0.2 § | 7.6 ± 0.9 |
| q11 | **103.1 ± 9.0** | 375.8 ± 21.4 | 42.8 ± 2.8 § | 51.7 ± 0.8 § | 31619.1 ± 97.9 |
| q12a | **3.8 ± 0.1** | 9.0 ± 1.1 | 245.1 ± 7.3 § | 1670.3 ± 5.7 § | 6794.7 ± 61.5 |
| q12b | 4.0 ± 0.1 | 9.7 ± 1.3 | error | 180.5 ± 2.9 § | **3.7 ± 0.1** |
| q12c | **3.6 ± 0.1** | 6.7 ± 0.2 | 4.2 ± 0.1 § | 3.4 ± 0.1 § | 3.7 ± 0.1 |

*The Sparkles and Oxigraph `q12b` cells come from the
[pure ASK prefix record](benchmarks/2026-10-25-pure-ask-prefix-q12b-forge.json), with
Sparkles at `b26ce6a9`. Oxigraph has the lower mean, and the 7.8% difference is inside
the ten-percent tie band. The `q12a` cells and Jena's `q12b` cell come from the
[paired forge record](benchmarks/2026-10-25-competitive-query-pairs-forge.json), with
Sparkles at `2ec8a266`. Other Sparkles cells were measured at `6699f27d`, and the
[SP²Bench record](benchmarks/2026-10-23-sp2b-10m-forge.json) keeps those samples and the
other engine cells.*

Sparkles is the only engine that answers `q5a`, whose 385,208 rows match an independent
oracle built from the data, in 3.9 s. Its answers to `q8`, `q12a` and `q12b` agree with
Jena's and Oxigraph's.

The `q4` and `q6` errors in the table are refusals by the default memory budgets of the
build measured there. The current engine answers both. Its `q6` OPTIONAL shares no variable
with the outer pattern, and a keyed left join pairs the rows through `?author = ?author2`,
testing 7.4 million candidate pairs instead of a product of 1.6 × 10¹² rows. Over two
interleaved rounds of 10 runs under the same 12 GiB cap, `q6` took a mean of 948 ms and
returned 906,313 rows with the exact and value digests of Jena's answer, 115× faster than
Jena's 108.6 s. `q4` runs as a cursor without spill. Its answer has 41,073,618 rows and
1,955,839,207 bytes of TSV, more than the default response budget of 1 GiB, so `q4` stays an
error with the default settings. With `--max-result-mb 4096` on one thread and without the
server cap, a streamed answer takes 30.4 s and an eager one 16.3 s. These figures come from
forge on 2026-10-23.

Sparkles answers `q12a` faster than Jena in both paired passes. `q12b` remains 1.33× slower than
Oxigraph ([roadmap](ROADMAP.md) item 91). On `q11` the unchecked times of QLever and Fluree, 42.8
and 51.7 ms, are lower than Sparkles' 103.1 ms.

Sparkles loads the data in 3.93 s, against 8.90 s for Fluree, 9.70 s for QLever, 20.49 s
for Oxigraph and 44.88 s for TDB2. Its index takes 307 MiB on disk, against QLever's
294 MiB, and its load peaks at 2,318 MiB, against Fluree's 1,835 MiB and QLever's
1,879 MiB.

## LUBM and reasoning

<a id="lubm100"></a>

### LUBM(100)

LUBM's generator, UBA 1.7, made 13,876,262 triples for 100 universities. Sparkles'
reasoner materialized the OWL 2 RL closure, 10,907,029 derived triples, in 17.8 s at a
peak RSS of 3,967 MiB, and every engine then loaded and queried the closure of 24,312,703
triples, so no query needs reasoning at query time. All 14 queries returned the same
answers on all five engines. Times are mean ± σ in ms over 10 runs after 2 warm-ups.
`mise run bench:lubm 100` reproduces the run.

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| q1 | **3.5 ± 0.1** | 7.5 ± 0.8 | 7.0 ± 0.7 | 3.6 ± 0.2 | 959.2 ± 7.0 |
| q2 | 256.5 ± 1.2 | 1274.3 ± 27.3 | **68.8 ± 6.2** | 2856.2 ± 25.9 | 4295.6 ± 19.5 |
| q3 | **3.4 ± 0.1** | 6.9 ± 0.5 | 8.4 ± 1.0 | 3.7 ± 0.2 | 2388.5 ± 28.5 |
| q4 | **3.6 ± 0.1** | 7.4 ± 0.3 | 11.7 ± 1.3 | 4.0 ± 0.1 | 286.4 ± 20.1 |
| q5 | **3.7 ± 0.2** | 8.5 ± 0.5 | 7.5 ± 0.5 | 6.8 ± 0.2 | 3166.5 ± 17.2 |
| q6 | **72.7 ± 0.7** | 944.7 ± 56.6 | 647.1 ± 4.0 | 407.6 ± 9.7 | 1701.9 ± 11.3 |
| q7 | **3.7 ± 0.1** | 6.6 ± 0.4 | 11.9 ± 0.7 | 3.9 ± 0.1 | 4.8 ± 0.2 |
| q8 | **10.3 ± 0.2** | 67.9 ± 18.3 | 153.5 ± 7.2 | 33.3 ± 1.1 | 9527.7 ± 33.4 |
| q9 | **67.0 ± 5.0** | 4446.1 ± 82.9 | 221.6 ± 9.2 | 7273.9 ± 50.2 | 17660.2 ± 29.5 |
| q10 | **3.5 ± 0.1** | 6.0 ± 0.6 | 6.7 ± 0.2 | 4.3 ± 1.4 | 2975.5 ± 14.7 |
| q11 | **3.8 ± 0.1** | 7.4 ± 0.5 | 5.5 ± 0.3 | 4.6 ± 0.2 | 147.4 ± 6.2 |
| q12 | **3.7 ± 0.1** | 7.9 ± 0.4 | 7.8 ± 0.5 | 5.9 ± 0.2 | 30.5 ± 1.9 |
| q13 | **4.1 ± 0.2** | 8.4 ± 0.5 | 11.3 ± 0.6 | 5.9 ± 0.2 | 7.1 ± 1.2 |
| q14 | **58.1 ± 0.8** | 561.3 ± 64.9 | 494.5 ± 4.8 | 328.6 ± 12.2 | 1370.7 ± 29.2 |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-19 over a closure with the same SHA-256. The
[LUBM record](benchmarks/2026-10-23-lubm-100-forge.json) has the samples, answer
fingerprints and materialization costs.*

Sparkles is fastest on 13 of the 14 queries. QLever is 3.7× faster on `q2`, at 68.8 ms
against 256.5 ms. In the cold runs, the median of 3 restarts with the engine's files
evicted, Sparkles is fastest on all 14 queries and answers 0.09 s after a restart.

Sparkles loads the closure in 7.57 s, against 18.96 s for QLever, 20.03 s for Fluree,
48.23 s for Oxigraph and 60.38 s for TDB2, into the smallest index, 236 MiB. Its load
peaks at 5,158 MiB, the highest after TDB2's 6,299 MiB, against QLever's 1,778 MiB. The
RDFS closure derives 5,527,675 triples in 11.3 to 12.7 s.

### Materialization at scale

These runs materialize closures that forge's 16 GiB cannot hold, so they ran on a
Namespace instance with 16 vCPUs and 32 GiB, each materialization alone in a cgroup of
28 GiB without swap, with `--max-inferred 0` and 16 Rayon threads. They are development
numbers, not forge numbers.

| Run | Time | Peak RSS | Derived triples |
|---|---:|---:|---:|
| LUBM(100), `rdfs` | 8.6 s | 3,125 MiB | |
| LUBM(1000), `rdfs`, 138,278,680 input triples | 90.9 s | 15,187 MiB | 55,069,413 |
| OWL2Bench, 2 universities, `owl-rl` | 25.0 s | 1,892 MiB | 4,914,915 |
| OWL2Bench, 5 universities, `owl-rl` | killed at 28 GiB after 12 min 50 s | | |

*Measured on a Namespace instance at commit 0e0fa5b4.*

[F16's Outcome](specs/F16-materialization-at-scale.md#outcome) has the method and the
comparison with the reasoner that preceded it.

### Small incremental runs

Each run materializes the closure, then changes 1,000 triples, undoes the change and makes
it again, each time from the closure kept in memory. The data is LUBM-style data with 100
universities from the generator of `examples/lubm.rs`, 13.1 million triples in index format
3, not the UBA data above. The table gives the medians of six interleaved rounds at 16
threads, each run alone on a fresh copy of the dataset.

| Run, 16 threads | Change | Undo | Change again | Full run | Kept closure |
|---|---:|---:|---:|---:|---:|
| RDFS | 18 ms | 13.5 ms | 12 ms | 14.7 s | 927 MiB |
| OWL 2 RL | 40.5 ms | 44.5 ms | 25 ms | 20.8 s | 1,244 MiB |

*Measured on forge on 2026-10-23 at commit 77789822. The kept closure sizes come from the
format 2 dataset at commit 534a7ad3.*

The kept closure leaves out the explicit facts, so it needs 41% and 47% of the 2,267 and
2,654 MiB that a closure keeping them needs. Changes of 1,000 triples stay within 1.2 times
their time with the larger closure, while changes of 100 triples at 16 threads and undos at
1 thread take 1.3 to 2.3 times as long. [Roadmap](ROADMAP.md) item 113 follows up those
slowdowns.

[F16's follow-ups](specs/F16-materialization-at-scale.md#roadmap-follow-ups-2026-10-18) give
the ranges, the other sizes and the 1-thread runs.

## DBpedia

### DBpedia at the 50M slice

The subject-hash slice of DBpedia 2022.12.01 contains 50,143,058 input statements.
Sparkles and QLever retain 41,127,700 distinct triples. Jena used the TDB2 6.2.0 bulk
loader and Fuseki 5.1.0 for queries. Warm times are medians of five requests after one
warm-up, including curl. Cold times are one request after a server restart and file-cache
eviction. Both use a 600 second request limit. Answer checks ran separately before warm
timing with a 300 second limit, and their result also qualifies the cold comparison.
`mise run bench:billion` reproduces the run.

Of the 27 queries with complete, agreeing content checks across all five engines,
Sparkles has the lowest median on 24 warm and 26 cold. QLever wins `births-by-decade`,
taking 11.1 ms against Sparkles' 54.4 ms warm and 41.0 ms against 71.4 ms cold. Fluree's
warm medians are lower on `inlinks-count-2`, 3.7 against 3.9 ms, and on `place-union-1`,
3.9 against 4.0 ms, both within 10%. This is agreement between engines, rather than an
independent oracle. Several small-query medians and throughput rates are within 10%.
`label-regex` is bimodal in Sparkles, near 0.65 ms or near 1.4 ms of server time in a
given server process.

| query | Sparkles warm (ms) | Fuseki warm (ms) | QLever warm (ms) | Fluree warm (ms) | Oxigraph warm (ms) |
|---|---:|---:|---:|---:|---:|
| abstract-contains | **6.8** | 58.7 | 20.8 | 9.1 | 49.0 |
| births-by-decade | 54.4 | 225.7 | **11.1** | 166.7 | 433.5 |
| category-people-1 | **3.8** | 6.9 | 12.1 | 4.5 | 4.7 |
| category-people-2 | **3.7** | 7.9 | 13.9 | 4.2 | 4.4 |
| category-tree | **3.6** | 7.5 | 7.2 | 3.7 | 4.0 |
| class-counts | **3.8** | 9639.2 | 24.7 | 297.0 | 1081.3 |
| costar-birthplace | **5.8** | 92.9 | 21.7 | 21.5 | 158.8 |
| count-all | 3.6 | 6585.3 † | 137.6 | 3.8 | 9778.3 † |
| country-population | **3.6** | 7.0 | 7.9 | 5.0 | 5.9 |
| entity-facts-1 | **3.5** | 7.6 | 6.0 | 3.8 | 4.6 |
| entity-facts-2 | **3.7** | 8.6 | 6.0 | 4.0 | 5.6 |
| entity-summary-1 | **3.8** | 8.0 | 6.5 | 3.9 | 4.3 |
| entity-summary-2 | **3.5** | 7.7 | 6.6 | 4.0 | 4.3 |
| export-1m § | 282.7 | 2579.0 | 1597.1 | 579.6 | 5304.6 |
| film-director-optional | **4.1** | 41.5 | 14.9 | 15.2 | 54.1 |
| geo-box | **6.5** | 48.9 | 13.5 | 40.8 | 36.6 |
| inlinks-count-1 | **3.9** | 11.2 | 4.4 | **3.9** | 5.0 |
| inlinks-count-2 | 3.9 | 9.6 | 4.8 | **3.7** | 4.8 |
| label-regex | **4.3** | 12.8 | 6.2 | 5.6 | 15.6 |
| outlink-classes-1 | **3.9** | 7.9 | 12.5 | 5.7 | 5.5 |
| outlink-classes-2 | **3.7** | 7.4 | 10.5 | 5.2 | 4.4 |
| people-no-birthdate | **5.4** | 186.9 | 8.4 | 20.3 | 397.8 |
| place-births-1 | **4.1** | 6.2 | 12.2 | 4.7 | 5.9 |
| place-births-2 | **3.8** | 7.6 | 11.6 | 4.7 | 6.4 |
| place-union-1 | 4.0 | 7.3 | 6.0 | **3.9** | 4.6 |
| place-union-2 | **3.7** | 7.3 | 5.6 | 3.8 | 4.3 |
| predicate-counts | 8.5 | 12505.5 † | 250.7 | 2480.6 | 11289.1 † |
| redirect-target-1 | **3.8** | 6.0 | 17.2 | 4.1 | 4.2 |
| redirect-target-2 | **3.9** | 6.7 | 12.5 | 4.2 | 4.3 |
| sameas-subjects | **3.6** | 9.4 | 5.2 | 4.0 | 5.0 |
| top-linked | **61.0** | 33712.5 | 471.6 | 5369.4 | 282458.5 ‡ |

| query | Sparkles cold (ms) | Fuseki cold (ms) | QLever cold (ms) | Fluree cold (ms) | Oxigraph cold (ms) |
|---|---:|---:|---:|---:|---:|
| abstract-contains | **57.5** | 1544.0 | 259.7 | 146.0 | 1240.8 |
| births-by-decade | 71.4 | 2883.1 | **41.0** | 246.8 | 4380.9 |
| category-people-1 | **12.8** | 71.7 | 45.8 | 130.2 | 50.6 |
| category-people-2 | **17.8** | 70.9 | 48.5 | 121.1 | 33.0 |
| category-tree | **6.9** | 58.4 | 29.5 | 48.3 | 11.0 |
| class-counts | **7.9** | 17438.0 | 58.7 | 389.7 | 1298.7 |
| costar-birthplace | **22.5** | 1550.2 | 49.3 | 91.9 | 1216.0 |
| count-all | 4.1 | 4857.7 † | 265.8 | 36.0 | 10994.9 † |
| country-population | **11.7** | 115.9 | 36.5 | 222.9 | 73.4 |
| entity-facts-1 | **8.2** | 66.3 | 28.1 | 78.4 | 41.5 |
| entity-facts-2 | **10.8** | 85.5 | 32.8 | 91.1 | 48.5 |
| entity-summary-1 | **9.5** | 49.0 | 27.5 | 43.6 | 17.1 |
| entity-summary-2 | **10.7** | 68.9 | 30.6 | 35.4 | 18.1 |
| export-1m § | 543.0 | 20355.4 | 2159.1 | 1259.5 | 24409.3 |
| film-director-optional | **12.2** | 1504.7 | 42.0 | 750.8 | 935.8 |
| geo-box | **31.4** | 502.3 | 42.8 | 708.1 | 223.2 |
| inlinks-count-1 | **6.5** | 426.1 | 23.3 | 61.2 | 15.5 |
| inlinks-count-2 | **6.8** | 371.7 | 24.2 | 44.2 | 17.8 |
| label-regex | **19.7** | 604.3 | 32.4 | 173.1 | 262.8 |
| outlink-classes-1 | **21.6** | 128.8 | 40.9 | 101.3 | 73.2 |
| outlink-classes-2 | **18.1** | 92.0 | 35.3 | 85.0 | 50.5 |
| people-no-birthdate | **17.3** | 1118.4 | 28.3 | 86.4 | 4383.6 |
| place-births-1 | **15.2** | 109.1 | 60.4 | 180.4 | 73.4 |
| place-births-2 | **19.0** | 150.3 | 65.5 | 186.8 | 88.3 |
| place-union-1 | **12.9** | 80.2 | 43.5 | 97.9 | 47.5 |
| place-union-2 | **12.5** | 71.2 | 39.9 | 103.8 | 38.5 |
| predicate-counts | 19.0 | 11164.3 † | 294.8 | 3475.5 | 14950.4 † |
| redirect-target-1 | **9.3** | 60.8 | 48.1 | 56.3 | 18.1 |
| redirect-target-2 | **12.6** | 63.9 | 41.6 | 58.6 | 21.9 |
| sameas-subjects | **6.3** | 325.7 | 25.0 | 67.7 | 17.5 |
| top-linked | **135.0** | 48716.9 | 576.4 | 6656.7 | 301481.4 ‡ |

*Sparkles was loaded and measured on forge on 2026-10-23 at commit 6699f27d, and the other
engines on 2026-10-20 over the stores of 2026-10-19. The
[result record](benchmarks/2026-10-23-dbpedia-50m-forge.json) has every timing result,
answer fingerprint and the SHA-256 and size of each of the 38 input files.*

† On `count-all` and `predicate-counts`, Sparkles, QLever and Fluree have one
value fingerprint, while Jena and Oxigraph have another, and the collector marks the
smaller group as `wrong-answer`. Sparkles and QLever count 41,127,700 triples and Jena
and Oxigraph 41,127,697, two fewer for `geo:lat` and one fewer for `geo:long`. The slice
has exactly three such pairs, a subject with two `xsd:float` values of one predicate
whose lexical forms differ but parse to the same number, such as `"37.89527777777778"`
and `"37.895278"`. Jena and Oxigraph store a float by its value and so merge each pair,
while RDF 1.1 identifies a literal by its lexical form and datatype, as Sparkles, QLever
and Fluree do here. These rows stay out of the ranking because the engines answer
different questions, not because one answer is wrong.

‡ Oxigraph's `top-linked` answer check did not complete within 300 seconds. Its five
warm timing requests and its cold request all completed within the separate 600
second limit, but the returned values remain unverified. Its timing is shown
without ranking it against the engines whose answers agreed.

§ `export-1m` checks the number of returned rows only. All five returned one million
rows. It is excluded from the content-verified ranking above.

The throughput runs send 64 separate curl requests through 16 workers, with one
warm-up batch and three measured batches. The rates below use the median batch
time and include client process overhead.

| query | Sparkles (q/s) | Fuseki (q/s) | QLever (q/s) | Fluree (q/s) | Oxigraph (q/s) |
|---|---:|---:|---:|---:|---:|
| entity-facts-1 | 272.5 | 240.9 | 261.3 | 271.6 | 269.7 |
| place-births-1 | 269.6 | 244.3 | 244.5 | 267.3 | 268.7 |

| load measurement | Sparkles | TDB2 | QLever | Fluree | Oxigraph |
|---|---:|---:|---:|---:|---:|
| Time in seconds | 24.55 | 579.35 | 49.46 | 106.14 | 83.27 |
| Peak RSS in MiB | 3229 | 6411 | 7134 | 5057 | 3550 |
| Index size in GiB | 1.41 | 4.25 | 1.57 | 3.48 | 5.96 |

### DBpedia at 1.24 billion triples

This comparison runs English DBpedia, release 2022.12.01, against QLever and Fluree. The 38 files of
`scripts/bench-billion/dbpedia-2022.12.tsv` hold 1,237,418,025 lines of N-Triples, 16 GB
compressed with zstd, which are 1,014,683,275 distinct triples. Sparkles' vocabulary has
205,941,262 terms. The run uses `mise run bench:billion full` with `RUNS=3`,
`TIMEOUT=300` and a query memory budget of 8 GB for both engines. Each step runs alone in
a systemd scope capped at 14 GiB with no swap, so the index of 28.7 GiB does not fit in
memory. Each query runs once to warm up and then three times. Cold runs restart the server
and evict its files from the page cache before each query, then measure it once.

Fluree runs with the settings of the 10.5M runs, an 8 GiB import budget and a 4 GiB cache.
Its import fails on a file larger than its chunk size, because it finds no statement
boundary where it cuts the file, so the harness splits the input into 370 pieces of at
most 512 MB first, which is not part of the load time. Fluree's server reached the 14 GiB
limit during the warm runs, so only four warm queries have Fluree timings, while all 31
cold queries completed. Of the 29 queries Fluree answered warm, its answers agreed with
the other engines on 27. On `births-by-decade` it returned 165 rows where Sparkles and
QLever returned 171, and on `entity-facts-2` it returned the same values as different RDF
terms.

| query | sparkles (ms) | qlever (ms) | fluree (ms) |
|---|---:|---:|---:|
| abstract-contains | **37.2 ± 7.0** | 234.2 ± 1.1 ‡ | 6719.1 ± 317.4 |
| births-by-decade | **51.3 ± 3.5** | 137.9 ± 8.3 | 4300.2 ± 88.3 † |
| category-people-1 | **13.0 ± 0.9** | 53.6 ± 3.5 | 63.2 ± 34.1 |
| category-people-2 | **6.8 ± 1.8** | 36.3 ± 8.2 | 49.1 ± 34.3 |
| category-tree | **269.3 ± 2.3** | 2050.1 ± 10.1 ‡ | — § |
| class-counts | **10.8 ± 2.2** | 99.1 ± 5.8 ‡ | — § |
| costar-birthplace | **50.9 ± 5.8** | 154.4 ± 5.6 ‡ | — § |
| count-all | **8.4 ± 3.4** | 2616.8 ± 11.9 † | — § |
| country-population | **12.2 ± 1.7** | 19.6 ± 6.0 ‡ | — § |
| entity-facts-1 | **10.9 ± 0.5** | 16.1 ± 0.3 ‡ | — § |
| entity-facts-2 | **5.7 ± 2.6** | 12.1 ± 4.0 | — § |
| entity-summary-1 | **10.5 ± 0.9** | 21.6 ± 7.0 | — § |
| entity-summary-2 | **6.4 ± 1.5** | 12.1 ± 3.1 | — § |
| export-1m | **241.7 ± 13.2** | 1440.1 ± 3.3 | — § |
| film-director-optional | **21.2 ± 1.2** | 33.3 ± 8.1 ‡ | — § |
| geo-box | **27.1** | 143.8 ± 1.4 ‡ | — § |
| inlinks-count-1 | **10.4 ± 1.0** | 14.7 ± 1.4 ‡ | — § |
| inlinks-count-2 | **11.5 ± 0.6** | 16.5 ± 1.1 ‡ | — § |
| label-regex | **13.6 ± 3.1** | 19.0 ± 0.0 | — § |
| outlink-classes-1 | **16.4 ± 5.3** | 22.6 ± 5.1 ‡ | — § |
| outlink-classes-2 | **16.3 ± 6.2** | 23.7 ± 4.1 ‡ | — § |
| people-no-birthdate | **50.7 ± 0.8** | 65.6 ± 4.9 ‡ | — § |
| place-births-1 | **16.9 ± 0.7** | 43.9 ± 2.4 | — § |
| place-births-2 | **15.6 ± 2.8** | 41.9 ± 1.0 | — § |
| place-union-1 | **12.8 ± 1.1** | 13.7 ± 4.3 | — § |
| place-union-2 | **10.0 ± 0.7** | 15.0 ± 0.6 | — § |
| predicate-counts | 18.4 ± 2.6 † | 5732.6 ± 45.0 † | — § |
| redirect-target-1 | **13.6 ± 4.2** | 22.8 ± 1.3 | — § |
| redirect-target-2 | **9.1 ± 5.5** | 22.7 ± 1.6 | — § |
| sameas-subjects | **8.8 ± 1.1** | 21.3 ± 2.9 ‡ | — § |
| top-linked | **572.6 ± 14.9** | 5580.6 ± 7.6 | — § |
| **throughput**, entity-facts-1, 16 clients (q/s) | **1517** | 1064 | — § |
| **throughput**, place-births-1, 16 clients (q/s) | **830** | 231 | — § |

‡ Same values as different RDF terms, because QLever returns counts as `xsd:int`.
§ Fluree's server was killed at the 14 GiB limit before it ran these queries warm.
† The engine's answer differs from the majority, so the query is not ranked for it. On `count-all` and `predicate-counts`, Sparkles and QLever disagree for the reason given [below](#why-count-all-and-predicate-counts-differ).
QLever's `count-all` is one triple short of the distinct input, and `predicate-counts` is
not ranked. The `geo-box` row is the median of 11 warm requests with the
[numeric column](#numeric-literals-in-the-vocabulary).

*Measured on forge between 2026-10-03 and 2026-10-09.*

Sparkles is faster than QLever on all 29 queries that both rank. `place-union-1` is within
10%. Under 16 clients it serves 1,517 `entity-facts-1` requests per second against
QLever's 1,064, and 830 `place-births-1` requests per second against 231. Fluree is slower
on the four warm queries it completed.

| query (cold) | sparkles (ms) | qlever (ms) | fluree (ms) |
|---|---:|---:|---:|
| abstract-contains | **721.0** | 4296.2 | 4195.1 |
| births-by-decade | **101.8** | 165.2 | 6108.5 |
| category-people-1 | **118.6** | 224.0 | 2886.2 |
| category-people-2 | **154.1** | 166.2 | 2172.0 |
| category-tree | **368.9** | 2098.1 | 4559.3 |
| class-counts | **6.4** | 177.8 | 9581.4 |
| costar-birthplace | **146.6** | 183.0 | 4358.2 |
| count-all | **5.5** | 4630.4 | 1273.3 |
| country-population | **31.6** | 45.6 | 4324.1 |
| entity-facts-1 | **18.7** | 36.4 | 1312.2 |
| entity-facts-2 | **25.0** | 40.4 | 837.6 |
| entity-summary-1 | **8.4** | 29.0 | 1303.7 |
| entity-summary-2 | **9.3** | 29.5 | 799.9 |
| export-1m | **1000.4** | 4261.4 | 6647.1 |
| film-director-optional | **35.3** | 68.6 | 13434.8 |
| geo-box | **108** | 157.9 | 12218.4 |
| inlinks-count-1 | **13.1** | 30.2 | 792.6 |
| inlinks-count-2 | **10.4** | 27.3 | 1275.8 |
| label-regex | **40.1** | 71.3 | 4328.6 |
| outlink-classes-1 | **61.7** | 73.7 | 1711.0 |
| outlink-classes-2 | 43.8 | **34.9** | 1665.2 |
| people-no-birthdate | 97.3 | **92.0** | 2113.6 |
| place-births-1 | **74.3** | 283.9 | 3293.1 |
| place-births-2 | **85.8** | 361.5 | 3956.5 |
| place-union-1 | **34.3** | 220.3 | 1235.7 |
| place-union-2 | **30.9** | 217.4 | 1244.6 |
| predicate-counts | **32.0** | 5880.8 | 81502.3 |
| redirect-target-1 | **21.9** | 45.9 | 1290.5 |
| redirect-target-2 | **20.4** | 48.0 | 1276.7 |
| sameas-subjects | **9.4** | 38.2 | 772.8 |
| top-linked | **938.8** | 6538.4 | 57624.6 |

Cold, Sparkles is faster than QLever on 29 of 31 queries. QLever wins
`outlink-classes-2`, at 34.9 against 43.8 ms, and `people-no-birthdate` by less than 10%.
Fluree is slower than Sparkles on every cold query.

#### Why `count-all` and `predicate-counts` differ

QLever counts 1,014,683,273 triples, and Sparkles and Fluree count 1,014,683,275. QLever folds
`xsd:float` and `xsd:double` literals whose lexical forms parse to the same number into
one term. DBpedia has such pairs, for example distinct lexical forms of the same value in
`geo:lat` and in `dbo:orbitalPeriod`, so QLever merges two pairs of triples that differ
only in that literal. RDF 1.1 identifies a literal by its lexical form and datatype, so
Sparkles keeps them as distinct terms and distinct triples, as Jena does. The
per-predicate counts differ for the same reason, and
[COMPARISON.md](COMPARISON.md#general) lists this among the divergences.

#### Numeric literals in the vocabulary

Sparkles stores a number inline in its id only when its lexical form is canonical.
DBpedia writes coordinates as `"48.8566"^^xsd:float` and many counts as `xsd:int` or
`xsd:nonNegativeInteger`, so 2,057,247 of its numeric literals live in the vocabulary. The
numeric column, `vocab.num`, holds their exact values next to the vocabulary, so a range
filter, sort or aggregate over them reads 8 bytes per value instead of decoding each key.
It costs 20.7 MB on disk beside an 8.0 GB `vocab.dat`, and 4.3 MB of it stays in memory
while the database is open. The `geo-box` and `country-population` queries of the suite and five extra queries over
`geo:lat`, `geo:long`, `dbo:populationTotal` and `dbo:elevation` compare the same binary
with the column and with `SPARKLES_NUMERIC_COLUMN=off`. Cold times are medians of 10
alternating rounds with a restart and file-cache eviction before each query, and warm
times are medians of 11 requests. The answers are identical in both modes.

| 1.01B | cold, with (ms) | cold, without (ms) | warm, with (ms) | warm, without (ms) |
|---|---:|---:|---:|---:|
| `geo-box` | **108** | 255 | **27.1** | 139.5 |
| `country-population` | 29.0 | 28.2 | **2.8** | 3.2 |
| `geo-avg` | **553** | 3,001 | **191** | 580 |
| `lat-order` | **72** | 137 | **21.8** | 50.2 |
| `pop-topk` | **41** | 90 | **9.3** | 20.7 |
| `pop-range` | **43** | 65 | **7.8** | 17.2 |
| `elev` | **41** | 54 | **6.7** | 14.0 |

Cold, the column's values are read ahead page by page, so a filter over a few thousand
values reads only the pages that hold them. `country-population` reads few such literals
and is within 10% either way.

#### Prefix filtering and large results

The `label-regex` filter, `REGEX(?l, "^The B")`, fixes the start of the label. Sparkles
uses the sorted vocabulary to narrow the matching ids to ranges before the join, so ids
outside the range fail without reading their keys. Results above 65,536 rows are
serialized in chunks of that size. Within a chunk, vocabulary ids are sorted, their pages
are requested ahead, and front-coded blocks are decoded once, in id order and in parallel.
Requests for the next chunk overlap the writing of the current one. This holds up to about
70 MiB of additional memory while an export is written.

## BSBM

### BSBM Explore at 10M triples

This run used 10,095,035 generated triples and 28,480 products. The configured driver mix
produced three instances of each of eleven query templates. Each latency below is the
pooled median of thirty HTTP samples, after two warm-up requests per instance, except
`q5`'s paired Sparkles, QLever and Fluree cells, which pool ninety samples after ten
warm-ups per instance. DESCRIBE
answers are not compared because SPARQL leaves their contents to the engine. A dagger
marks an unresolved answer disagreement, whose latency is not ranked.
`SPARKLES_ALLOW_GPL_BENCHMARK=1 mise run bench:bsbm 10m` reproduces the run.

| Template, median ms | Sparkles | Jena | QLever | Fluree | Oxigraph |
|---|---:|---:|---:|---:|---:|
| q1 | **3.95** | 8.00 | 9.63 | 4.71 | 4.60 |
| q2 | **4.09** | 9.31 | 14.91 | 4.41 | 4.67 |
| q3 | **4.02** | 8.22 | 12.64 | 4.53 | 4.79 |
| q4 | **4.11** | 8.23 | 25.43 | 5.25 | 5.65 |
| q5 | **4.65** | 629.44 | 20.26 | 5.59 | 42.65 |
| q7 | 5.27 | 7.23 | 90.64 | 5.07 | **4.51** |
| q8 | 5.97 | 7.26 | 50.76 | 4.29 † | **4.13** |
| q9, unranked | 3.46 | 6.61 | 6.24 | 3.61 | 3.67 |
| q10 | **4.12** | 6.98 | 139.03 | 5.55 | 4.35 |
| q11 | **3.41** | 6.63 | 6.13 | 3.56 † | 3.54 |
| q12 | **3.65** | 7.18 | 11.96 † | 3.96 † | 3.90 |

*The paired `q5` cells for Sparkles, QLever and Fluree come from the
[query record](benchmarks/2026-10-25-competitive-query-pairs-forge.json), with Sparkles
at `2ec8a266`. The [Explore record](benchmarks/2026-10-23-bsbm-10m-explore-forge.json)
keeps the other cells, measured with Sparkles at `6699f27d`, and the driver status.*

Direct checks against the generated triples independently confirm Sparkles' answers to
all three instances of q8, q11 and q12. The checks reconstruct the selected reviews,
product offers and CONSTRUCT triples without a SPARQL engine. Fluree's retained value
fingerprints differ on those nine instances. QLever differs on q12. The
q9 row is a DESCRIBE whose answers are not compared, so it is not ranked.
Sparkles has the lower pooled q5 median in both paired passes. The closest instance
takes a mean of 4.58 ms against Fluree's 4.76 ms, within 4%. Oxigraph was fastest on
q7, 1.17 times faster, and on q8, 1.45 times.

The driver ran fifty warm-up mixes and five hundred measured mixes with each
client count. The standard mix includes DESCRIBE and CONSTRUCT, which the driver
requests as RDF/XML. QLever refuses that format. The SELECT-only mix omits those
queries and is reported separately. An error means the driver failed or its result
counts failed the comparison. Fluree's throughput is two to three times Sparkles' at
every client count. It is marked with a dagger and not ranked, because its fixed-instance
answers to q8, q11 and q12 disagree with the other engines.

| Mix | Clients | Sparkles | Jena | QLever | Fluree | Oxigraph |
|---|---:|---:|---:|---:|---:|---:|
| Standard QMpH | 1 | 45,077 | 2,047 | error | 95,969 † | 15,622 |
| Standard QMpH | 4 | 165,605 | 3,173 | error | 304,636 † | error |
| Standard QMpH | 16 | 200,083 | 2,012 | error | 579,271 † | error |
| SELECT-only QMpH | 1 | 36,687 | 2,105 | error | 103,489 † | error |
| SELECT-only QMpH | 4 | 169,528 | 3,237 | error | 329,816 † | 74,102 |
| SELECT-only QMpH | 16 | 203,454 | error | error | 662,847 † | error |

The 100M scale has not been measured.

### BSBM BI at 10M triples

This run used the same 10,095,035 triples and 28,480 products. Each template has three
fixed instances. The paired `q2` cells for Sparkles, QLever and Fluree pool ninety HTTP
samples after ten warm-ups per instance. The other cells pool thirty samples after
two warm-ups per instance. A dagger marks an answer that differs from the majority and is
excluded from ranking. Oxigraph timed out on two q7 instances, so its pooled cell is a
timeout. `SPARKLES_ALLOW_GPL_BENCHMARK=1 mise run bench:bsbm 10m --usecase bi` reproduces
the run.

| Template, median ms | Sparkles | Jena | QLever | Fluree | Oxigraph |
|---|---:|---:|---:|---:|---:|
| q1 | **13.59** | 3,241.71 | 52.51 | 155.26 | 2,077.28 |
| q2 | **20.54** | 3,562.21 | 162.30 | 22.50 † | 32,569.39 |
| q3 | 35.18 | 1,733.90 | **28.45** | 287.78 † | 3,287.87 |
| q4 | **152.80** | 8,452.57 | 1,848.57 | 162.22 | 6,868.73 |
| q5 | **27.32** | 198.17 | 98.63 | 693.70 | 138.93 |
| q6 | **16.50** | 23.72 | 67.88 | 338.27 | 28.77 |
| q7 | **52.62** | 74.60 | 59.38 | 210.01 | timeout |
| q8 | **35.04** | 121.75 | 68.09 † | 202.66 † | 184.04 |

*The paired `q2` cells for Sparkles, QLever and Fluree come from the
[query record](benchmarks/2026-10-25-competitive-query-pairs-forge.json), with Sparkles
at `2ec8a266`. The [BI record](benchmarks/2026-10-23-bsbm-10m-bi-forge.json) preserves
the other cells, measured with Sparkles at `6699f27d`, including failures.*

The answer checks compare raw answers captured on atlas, where each engine served the same
data alone with the same binaries, because QLever's check on forge left q4 to q8 without
fingerprints. q4 and q5 average `xsd:float` prices, and SPARQL leaves the order of the
additions open, so engines that add in different orders legitimately return different
floats. `scripts/bench-bsbm/bi-float-oracle.py` computes the exact mean of the
float-rounded prices of every q4 and q5 instance from the data and accepts a value
within the error bound of a float summation in any order. All five engines' q4 and q5
answers pass it, with the same rows, counts and IRIs. Sparkles, Jena and Oxigraph return
`xsd:float`, QLever `xsd:decimal` and Fluree `xsd:double`.

`scripts/bench-bsbm/bi-answer-oracle.py` independently computes q6 reviewer scores and
its HAVING threshold from the same data. All five engines' three q6 captures pass the
binary32 error bounds. This establishes numeric equivalence for the comparison, rather
than identical RDF datatypes. The same tool checks every successful retained q7 capture
against the offer counts and country exclusions. The inner LIMIT orders only by offer
count. On the third instance 41 products tie for its last 21 places, and the outer
country filter permits 75 to 85 rows. The 81-row Sparkles, Jena and QLever answers and
the 79-row Fluree and Oxigraph answers are all valid. The checked Fluree captures total
80 rows across the three instances. They replace an unavailable 77-row fingerprint in
the record without changing any timing sample.

The paired q2 answers also pass an independent streamed data oracle for Product
membership, common-feature counts, anchor exclusion and the complete ordered top ten.
Its pooled fingerprint matches the published Sparkles answer pool. Fluree's q2 pool
differs from that independently verified result, and remains unranked.

Fluree's q3 and q8 answers and two QLever q8 answers remain unresolved fingerprint
disagreements. The q3 and q8 exclusions have no independent data oracle. Sparkles has
the lowest median on q1, q2 and q4 to q8. Its q4 lead over Fluree is within 10%.
QLever remains 1.24 times faster on q3, where its answers agree with Sparkles'. Its two
q8 instances whose answers differ from the majority carry a dagger.

The driver uses generated parameters beyond the three fixed instances. It requested
fifty warm-up mixes and five hundred measured mixes at each client count, with a
1,800 second wall budget per cell.

| BI QMpH | Clients | Sparkles | Jena | QLever | Fluree | Oxigraph |
|---|---:|---:|---:|---:|---:|---:|
| Standard mix | 1 | error | timeout | error | timeout | timeout |
| Standard mix | 4 | error | timeout | error | 2,174 † | timeout |
| Standard mix | 16 | error | timeout | error | 3,143 † | timeout |

In this run Sparkles failed all three driver cells. At one and four clients, q4 with
ProductType6 asked the query pool for about 5.8 GiB, more than the default pool's 5.2 GiB.
At sixteen clients, q2 instances of about 450 MB each queued for the pool and some exceeded
the admission wait. The current engine streams that q4 instance and answers it in 15.1 s
with a peak resident size of 282 MB under the default pool, where eager execution takes
17.7 s and 8.3 GB, and a request larger than the whole pool answers `507` without
`Retry-After` ([roadmap](ROADMAP.md) item 96). The driver cells have not been rerun with
it. QLever's driver failed with a refused connection at one client and HTTP 500 at four and
sixteen clients. Jena and Oxigraph timed out at every client count. Fluree alone completed
two driver cells, but their result-count check has no successful competing engine to
compare, and its fixed q2, q3 and q8 answers have unresolved disagreements, so its QMpH figures are not
ranked.

## Full-text search

The comparison uses Sparkles' Tantivy-backed `text:query`, Fuseki's Lucene-backed
jena-text and QLever's `ql:contains-word`. Sparkles and Jena index `foaf:name`,
`ex:title` and `rdfs:label`, with English stemming of titles. QLever indexes all literals
and joins on predicates, searches the indexed form for the stemmed case, and does not
highlight. Hit sets and counts agree, except on `common-top10` and `highlight`, where
the record compares only result counts. Scores and the order of tied hits are not
compared. Times are TSV HTTP means ± standard deviation in ms, over ten runs at 1.05M and
five runs after one warm-up at 10.5M. `scripts/bench-text.sh` reproduces the run.

| query | sparkles 1.05M | jena-fuseki 1.05M | qlever 1.05M | sparkles 10.5M | jena-fuseki 10.5M | qlever 10.5M |
|---|---:|---:|---:|---:|---:|---:|
| rare-top10 | **4.0 ± 0.2** | 13.1 ± 1.0 | 6.8 ± 0.4 | **5.8 ± 0.4** | 15.0 ± 1.9 | 7.8 ± 0.5 |
| common-top10 | **4.1 ± 0.2** | 11.1 ± 1.2 | 7.4 ± 0.6 | **8.1 ± 0.3** | 14.9 ± 2.8 | 17.5 ± 2.7 |
| common-count | **4.8 ± 0.2** | 54.7 ± 3.2 | 7.2 ± 0.5 | 14.8 ± 0.6 | 406.9 ± 4.3 | **13.5 ± 1.1** |
| text-join | **5.6 ± 0.2** | 70.7 ± 5.2 | 11.9 ± 1.2 | **21.4 ± 0.6** | 585.4 ± 5.1 | 46.6 ± 7.6 |
| conjunction | **4.1 ± 0.2** | 15.0 ± 1.8 | 7.2 ± 0.3 | **8.2 ± 0.2** | 80.7 ± 0.8 | 14.3 ± 1.0 |
| highlight | **11.4 ± 0.3** | 103.7 ± 10.1 | 15.0 ± 1.9 | **70.6 ± 0.5** | 841.8 ± 46.2 | 94.8 ± 6.5 |
| stemmed | **3.9 ± 0.2** | 11.1 ± 1.3 | 6.2 ± 0.3 | **5.4 ± 0.3** | 53.5 ± 0.9 | 18.6 ± 2.6 |

*At 1.05M, Sparkles was measured on forge on 2026-10-23 at commit 6699f27d and the other
engines on 2026-10-20, each under a 12 GiB limit with clients pinned to CPU 11. The
[full-text record](benchmarks/2026-10-23-text-1.05m-forge.json) has the samples and answer
fingerprints. The 10.5M figures come from the runs of 2026-10-03 to 2026-10-09 without that
isolation.*

QLever's lead on `common-count` at 10.5M, which counts 49,837 hits, is within 10%. The
highlight form returns plain literals in QLever and marked words in Sparkles and Jena.
Index build times and sizes are under [Index builds](#index-builds).

<a id="full-text-index-and-observability-105m-triples"></a>

### Full-text index and observability

The full-text index commits staged documents when a text query needs them, on a tick of
about one second, or when about 16,000 changes are staged. It syncs to disk at most once a
second and replays the WAL after a crash, so a write does not wait for the index.

Access logging and Prometheus metrics are on by default. With both on, the canonical 28
queries at 10.5M summed to 1,027 ms of means, against 1,031 ms with
`--no-access-log --no-metrics`. Under 16 clients the server answered 225 star joins per
second with them on and 239 with them off, in a single pair of runs.

## Vector search

This Sparkles-only run searches 100,000 clustered 384-dimensional vectors by cosine
distance. Each configuration answered 1,000 top-10 queries after 20 warm-ups over a
persistent HTTP connection with full TSV responses. Recall is measured against Sparkles'
exact search on the same vectors, and no other vector engine was measured. The HNSW graph
is built over a half-precision copy of the vectors.

| Search | Recall@10 against exact | Mean (ms) | p50 (ms) | p99 (ms) |
|---|---:|---:|---:|---:|
| Exact | 1.0000 | 5.39 | 5.44 | 5.99 |
| HNSW `ef=16` | 0.9903 | 0.45 | 0.52 | 0.60 |
| HNSW `ef=32` | 0.9994 | 0.72 | 0.72 | 0.86 |
| HNSW `ef=64` | 0.9994 | 0.46 | 0.35 | 0.94 |
| HNSW `ef=128` | 0.9994 | 0.54 | 0.54 | 0.65 |
| HNSW `ef=256` | 0.9994 | 0.92 | 0.91 | 1.37 |

*Measured on forge with an index built on 2026-10-13.*

Means below about 0.7 ms are not comparable between runs on this host. The same binary
answered the 1,000 `ef=16` queries in 0.29 ms at the median in one pass and 0.60 ms in the
next, and cheap settings were often slower than `ef=64`. This comes from the client and the
processor's idle states rather than from the search. These figures describe this synthetic
distribution, not all vectors.

## Spatial queries

This Sparkles-only run uses the geo fixture with 100,000 points, its polygons and 180,640
indexed geometry literals. The server returns the same coordinate-normalized results
with the spatial index on and off. Each query had one warm-up and five curl and hyperfine
samples, so the times include process startup. The index builds in 0.54 s and takes
13.8 MiB. `scripts/bench-geo.sh` reproduces the run.

| query | rows | with the index (ms) | without (ms) | speedup | rewrites off (ms) |
|---|---:|---:|---:|---:|---:|
| geo-q1-within | 2861 | 21.9 ± 4.0 | 793.7 ± 48.6 | 36.3× | — |
| geo-q2-distance | 85 | 16.0 ± 2.9 | 419.6 ± 41.6 | 26.2× | — |
| geo-q3-nearby | 10 | 15.9 ± 1.5 | 400.1 ± 15.7 | 25.1× | — |
| geo-q4-withinbox | 1 | 31.2 ± 3.1 | 344.4 ± 6.5 | 11.0× | — |
| geo-q5-points-per-state | 562 | 47.2 ± 2.2 | 100.9 ± 5.1 | 2.1× | — |
| geo-q6-touching-counties | 1 | 241.2 ± 4.0 | 309.5 ± 4.3 | 1.3× | — |
| geo-q7-knn | 10 | 7.2 ± 2.3 | 492.5 ± 10.8 | 68.0× | 304.2 ± 43.1 |
| geo-q8-area | 1 | 756.2 ± 0.9 | 971.6 ± 9.9 | 1.3× | — |
| geo-q9-star | 1374 | 23.8 ± 5.3 | 821.7 ± 22.2 | 34.6× | — |

### GeoSPARQL Compliance Benchmark

The GeoSPARQL Compliance Benchmark (Jovanovik, Homburg and Spasić, 2021) has 206 queries
over the 30 requirements of GeoSPARQL 1.0 and a 338-triple dataset, and
`scripts/geosparql-benchmark.sh` runs it. Each requirement runs against the configuration
of its conformance class. R25 to R30 test RDFS entailment and query rewrite, so they run
against a database with `infer --profile rdfs --vocab geosparql` and
`"queryRewrite": true`. The other requirements run against the asserted data with the
spatial index and neither extension, because entailment and rewrite add answers that
those queries do not expect. With every query against the extended database, Sparkles
scores 147.

An answer counts as correct when it matches one of the expected result files. Solutions
are compared as multisets, numbers within a relative 1e-6, and geometry literals by their
coordinates rounded to 6 decimals. GML and KML literals are converted to GeoJSON by the
server's `POST /$/geo/convert` first, and the comparison ignores ring starts and
directions, line directions, vertices on straight runs and member order. R17 has no query.

| Requirement | Correct | Queries |
|---|---:|---:|
| R1 core: SPARQL protocol | 1 | 1 |
| R2 core: spatial object class | 1 | 1 |
| R3 core: feature class | 1 | 1 |
| R4 topology vocabulary: Simple Features relations | 8 | 8 |
| R5 topology vocabulary: Egenhofer relations | 8 | 8 |
| R6 topology vocabulary: RCC8 relations | 8 | 8 |
| R7 geometry class | 1 | 1 |
| R8 feature properties | 2 | 2 |
| R9 geometry properties | 6 | 6 |
| R10 WKT literal | 1 | 1 |
| R11 WKT literal default CRS | 1 | 1 |
| R12 WKT axis order | 1 | 1 |
| R13 empty WKT literal | 0 | 2 |
| R14 `asWKT` | 1 | 1 |
| R15 GML literal | 1 | 1 |
| R16 empty GML literal | 0 | 2 |
| R18 `asGML` | 1 | 1 |
| R19 query functions | 22 | 28 |
| R20 `getSRID` | 2 | 2 |
| R21 `relate` | 4 | 4 |
| R22 Simple Features functions | 32 | 32 |
| R23 Egenhofer functions | 32 | 32 |
| R24 RCC8 functions | 32 | 32 |
| R25 RDFS entailment: basic graph patterns | 3 | 3 |
| R26 RDFS entailment: WKT geometry types | 2 | 2 |
| R27 RDFS entailment: GML geometry types | 1 | 1 |
| R28 query rewrite: Simple Features | 6 | 8 |
| R29 query rewrite: Egenhofer | 5 | 8 |
| R30 query rewrite: RCC8 | 4 | 8 |
| **Total** | **187** | **206** |

The mean of the per-requirement scores is 88.5% over the 29 requirements with queries.
GeoSPARQL Fuseki 3.17 published 177 of 206. The 19 queries that fail disagree with
choices Sparkles makes on purpose:

* R13 and R16 (4 queries) expect two empty geometries to be `sfEquals`. In Sparkles an
  empty geometry is only disjoint, as in Jena.
* Four R19 queries expect distances that are neither geodesic nor haversine, and two
  expect a 10-metre buffer to be 10 degrees wide.
* Nine R28 to R30 queries expect relations that DE-9IM and the standard's tables do not
  give. They count a region as an RCC8 tangential proper part of itself, count a region
  strictly inside another as `ehCoveredBy` it, leave a feature's own geometries and the
  points inside it out of `sfIntersects`, and leave the EPSG:4326 point at latitude
  31.95, longitude -88.38 out of the geometries disjoint from feature B.

## Paths and graph algorithms

### Path search

These Sparkles-only runs compare `SERVICE path:search` with property paths on the
benchmark's `foaf:knows` graph, with a warm server and the result cache off. Each query
had one warm-up and 31 measured requests, interleaved with the other queries, and times
include a fresh HTTP connection and the complete TSV response. Answers were checked
against an independent traversal of the input: reachability, distances, shortest-path
counts, five simple-path lengths and every returned edge. The `+` path from person 0
reaches 89,090 nodes at 1.05M and 892,556 at 10.5M. The last person is 99,999 or 999,999,
with shortest chains of 12 and 17 edges.

| query | what it does | 1.05M min | 1.05M median | 10.5M min | 10.5M median |
|---|---|---:|---:|---:|---:|
| `reach-plus` | `COUNT` of `ex:0 foaf:knows+ ?x` | 12.25 | 12.77 | 133.75 | 139.06 |
| `ps-any` | `COUNT` of one shortest path to every reachable person | 17.03 | 17.38 | 243.84 | 257.02 |
| `ask-pair` | `ASK { ex:0 foaf:knows+ ex:last }` | 10.98 | 11.56 | 129.86 | 131.24 |
| `ps-pair` | the shortest path between the same pair, one row per edge | 0.92 | 1.03 | 3.03 | 3.24 |
| `ps-pair-all` | every shortest path between them (1 at 1.05M, 3 at 10.5M) | 0.77 | 0.86 | 2.95 | 3.08 |
| `ps-pair-both` | shortest path with `path:direction path:both` (7 or 8 edges) | 0.80 | 0.87 | 1.54 | 1.69 |
| `ps-pair-k5` | five shortest simple paths, by Yen's algorithm | 11.50 | 12.10 | 73.22 | 79.08 |
| `ps-values10` | distances from 10 people bound by `VALUES` to the last person | 2.41 | 2.81 | 14.84 | 15.25 |
| `ps-all4` | `COUNT` of every path of up to 4 edges from person 0 | 0.36 | 0.43 | 0.65 | 0.78 |
| `pp-len4` | the same count as a `UNION` of four fixed-length property paths | 0.48 | 0.59 | 0.70 | 0.83 |
| `ps-any-weighted` | `ps-any` with `path:weight`, by Dijkstra's algorithm | 94.54 | 97.52 | 1,601.75 | 1,615.50 |

Times are in milliseconds. A pair search is 11–41× faster than the property-path ASK,
because the bidirectional search stops where its frontiers meet. Searching from one
person to everyone costs 1.4–1.8× the `+` count, since it also keeps path parents and
levels, and weighted search costs 5.6–6.3× the unweighted search.

`path:excludePredicate` and `path:maxDegree` add no measurable cost to searches that use
neither parameter. In a screen of the path searches above at 10.5M, on an 8-vCPU Namespace
instance on 2026-10-23 with four A, B, B, A rounds, the geometric mean of server time with
the parameters' code against without it was 0.97, and medians of one build varied by up to
77% between server processes, so no change was above the noise. Searches backward and both
ways do not pass through literals.

### Weighted pair searches

A weighted search for the cheapest path of one pair runs Dijkstra's algorithm from both
ends, and so do the spur searches of Yen's algorithm with weights
([F07](specs/F07-path-search.md#weighted-searches-from-both-ends-2026-10-24)). The table
compares the build before this change with the current one on the 10.5M-triple data, once
with every edge weighing 1 and once with an `ex:km` weight from 1 to 100 on the reifier of
every `foaf:knows` triple. The pair is person 0 and the last person, and every query binds
`path:weight` and `path:cost`.

| query | weights | one end (ms) | both ends (ms) |
|---|---|---:|---:|
| `ps-pair` | all 1 | 820.2 | 3.5 |
| `ps-pair` with `path:direction path:both` | all 1 | 730.6 | 1.4 |
| `ps-pair-k5`, five cheapest paths | all 1 | fails at 10M visits | 68.9 |
| `ps-pair` | 1 to 100 | 4,937.9 | 29.0 |
| `ps-pair` with `path:direction path:both` | 1 to 100 | 11,904.4 | 51.6 |
| `ps-pair-k5`, five cheapest paths | 1 to 100 | fails at 10M visits | 656.0 |
| `ps-pair-all`, every cheapest path | all 1 | 876.2 | 896.8 |

Times are medians of the server's execution time. With every edge weighing 1 the weighted
pair search costs about 1.5 times the unweighted `ps-pair`, which took 2.3 ms in the same
runs. Weighted `path:allShortest` without `path:maxLength` still runs from the source
alone and measured 2.3% slower, although its search did not change, which item 276 covers.
[Roadmap](ROADMAP.md) item 277 would run it from both ends. Between equally cheap paths the two builds returned different ones in one case, which
the spec allows, and every other answer matched.

A weighted shortest search with `path:maxLength 6`, as the UI's path finder sends, failed
in both builds at its limit of a million visits, because no path of this pair has fewer
than 17 edges and the search lists paths by cost until one is short enough. Item 275
addresses it. The unweighted `ps-pair`, `ps-pair-all`, `ps-pair-k5` and `ps-values10`
measured 3 to 4% slower in every round although the change does not touch their search,
and item 276 looks for the cause.

*Measured on forge, an Intel Core i5-13500, on 2026-10-24, with the server on one core and
the client on another, in interleaved A, B, B, A rounds. With all weights 1 there were four
rounds of 20 requests per query, or one round of three for the five cheapest paths. With
weights from 1 to 100 there were two rounds of five, or one round of three.*

<a id="graph-algorithm-polling"></a>

### Graph algorithms

The ignored `polling_cost` test of `tests/graph_algorithms_scale.rs` runs each algorithm
on a random graph with 1,000,000 nodes and 5,000,000 edges, pinned to four cores, under a
far deadline so that every cancellation check reads the clock. Each run is the fastest of
three calls, and the table gives the median of eight runs with the slowest and fastest.

| Algorithm | Threads | Time (ms) | Range |
|---|---:|---:|---|
| PageRank, 20 iterations | 1 | 505.5 | 488.0–523.7 |
| Weakly connected components | 1 | 142.8 | 140.8–145.2 |
| Strongly connected components | 1 | 219.9 | 211.6–265.1 |
| Breadth-first search | 1 | 117.5 | 111.1–128.0 |
| Label propagation | 1 | 704.4 | 700.1–711.5 |
| Triangle count | 1 | 494.0 | 469.4–500.0 |
| K-core | 1 | 251.8 | 224.5–252.9 |
| FastRP | 1 | 2310.0 | 2123.9–2313.3 |
| PageRank, 20 iterations | 4 | 161.6 | 153.0–167.3 |
| Weakly connected components | 4 | 40.2 | 38.9–40.5 |
| Strongly connected components | 4 | 221.6 | 189.3–245.9 |
| Breadth-first search | 4 | 35.3 | 33.9–36.8 |
| Label propagation | 4 | 183.4 | 182.5–184.8 |
| Triangle count | 4 | 241.5 | 237.0–250.1 |
| K-core | 4 | 252.3 | 251.4–254.5 |
| FastRP | 4 | 1009.4 | 1003.0–1015.1 |

*Measured on forge.*

Every long loop checks for cancellation and its deadline within bounded work, at most once
per 64K units, and charges its workspace to the call. Against a build without these checks,
label propagation, triangle counting, K-core and FastRP moved by −0.4% to +1.4%, within the
noise of the measurement. Breadth-first search charges at most 9.1 MB at its peak on this
graph, and a unit test keeps it under 16 bytes a node on a graph of 100,000 nodes of degree
50. The graph is uniform random, so it has no node of very high degree.

## Engine features and overheads

These sections measure single features of the engine, mostly with Sparkles alone.

### Query execution features

Executor optimizations can be switched off one at a time with
`SPARKLES_DISABLE_OPTIMIZATIONS`. This comparison ran the warm 10.5M suite with each of 33
switches off in turn and once with the defaults, and every configuration passed the answer
checks. The table lists the changes larger than 25%.

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
([U04](specs/U04-live-query-profiling.md#phase-4-2026-10-23) Phase 4). It does so only when
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
not move with changes to the server, as [Benchmarking](BENCHMARKING.md#release-build-settings)
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

The query memory pool of [X08](specs/X08-disk-spill-and-server-memory.md#gates) is on by
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
four triples. [X05](specs/X05-streaming-query-execution.md#large-basic-graph-patterns) has
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
plans ([roadmap](ROADMAP.md) item 84). The planner keeps the star masks and probed sides
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
[U06](specs/U06-large-graph-rendering.md) on the 10.5M-triple synthetic dataset. Each
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
([X11](specs/X11-index-compression-and-predicate-patterns.md#outcome)). The table compares
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
([X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads)).
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
`entity-summary-1` on DBpedia takes 1.17 times as long. [Roadmap](ROADMAP.md) item 160
follows up `star-join` and `entity-summary-1`, and item 241 follows up three warm DBpedia
queries that run 1.4% to 4.2% slower with front coding since item 108.

## Loading and index builds

Sparkles builds its index in one parallel pass. Parsing and vocabulary merging run on all
cores, sampled keys divide the partial vocabularies into ranges that merge in parallel,
and each 64-million-quad chunk is remapped once, sorted by SPO, OSP and PSO and written as
compressed runs. SOP, OPS and POS derive from those streams by sorting runs of the first
column, and without named graphs GSPO derives from SPO. Each permutation has its own
writer, so the input is read once.

### Bulk loading

Load times include each engine's own preparation, such as `oxigraph optimize`, and peak
RSS comes from GNU time. Each load ran once. Oxigraph reads N-Triples from standard input,
which keeps blank node labels whole and takes about three times as long as its parallel
`--file` loader.

| Load | sparkles | jena (tdb2.tdbloader) | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M triples, time | **0.47 s** | 4.80 s | 1.36 s | 1.38 s | 4.42 s |
| 1.05M triples, peak RSS | **444 MiB** | 2,166 MiB | 629 MiB | 479 MiB | 826 MiB |
| 10.5M triples, time | **3.67 s** | 45.70 s | 9.35 s | 10.20 s | 18.22 s |
| 10.5M triples, peak RSS | 2,024 MiB | 4,433 MiB | **1,929 MiB** | 3,007 MiB | 2,184 MiB |
| SP²Bench, 10M triples, time | **3.93 s** | 44.88 s | 9.70 s | 8.90 s | 20.49 s |
| LUBM(100) closure, 24.3M triples, time | **7.57 s** | 60.38 s | 18.96 s | 20.03 s | 48.23 s |
| WatDiv, 10.97M triples, time | **3.51 s** | 33.77 s | 9.56 s | 8.92 s | 19.41 s |
| DBpedia, 1.24B lines, time | **480 s** | — | 1,674 s | 2,919 s | — |
| DBpedia, peak RSS | **6,059 MiB** | — | 11,401 MiB | 12,366 MiB | — |

*The synthetic, SP²Bench, LUBM and WatDiv loads of Sparkles were measured on forge on
2026-10-23 at commit 6699f27d, and those of the other engines from 2026-10-18 to
2026-10-20. The DBpedia loads come from the runs of 2026-10-03 to 2026-10-09.*

The DBpedia load is one load of the complete input with the CPUs pinned to the
performance cores. Its peak includes file-cache pages counted in the 14 GiB scope, and
writing the numeric column during the load had no measurable cost. QLever indexed with
8 GB of sort memory and 5 million triples per batch. Jena and Oxigraph were not measured
at this scale.

### Compressed inputs

`sparkles load` read the 1.1 GB N-Triples file of the 10.5M dataset raw and compressed.
Times are medians of 3 runs, CPU is the load's user and system time, and parallelism is
CPU divided by wall time.

| Input | File size | Seconds | CPU (s) | Parallelism |
|---|---:|---:|---:|---:|
| raw | 1,121.3 MB | 3.85 | 28.5 | 7.4 |
| gzip-6 | 81.9 MB | 4.27 | 36.8 | 8.7 |
| zstd-3 | 78.6 MB | 4.08 | 36.2 | 8.7 |
| brotli-5 | 69.0 MB | 4.37 | 36.9 | 8.2 |
| lz4 | 160.0 MB | 3.99 | 36.2 | 9.0 |

Decompression runs as one stream in front of the parallel parser and adds 0.1–0.5 s.

### Index builds

| Build | Time | Result |
|---|---:|---|
| Full-text index, 1.05M triples | **0.39 s** (Fuseki 4.53, QLever 1.88) | 13.2 MB (QLever **11.1**, Fuseki 24.9) |
| Full-text index, 10.5M triples | **6.48 s** (Fuseki 25.98, QLever 19.35) | 122.5 MB (QLever **99.9**, Fuseki 227.8) |
| Full-text document store, every string literal of 10.5M (1,540,012 documents) | 6.17 s with zstd, 6.07 s with LZ4 | 114.1 MB with zstd (the default), 117.3 MB with LZ4 |
| HNSW vector index, 100,000 × 384 dimensions, `M=16`, `efConstruction=128` | 6.43 s | 163 MB of mapped files, 1,044.4 MB peak RSS |
| HNSW vector index, 1,000,000 × 384 dimensions, in-process example | 63.5 s. USearch 2.26.2 in half precision takes 50.0 s. | Recall@10 is 0.963 at `ef=128`. Peak RSS is 12.6 GB. |
| Spatial index, 180,640 geometries | 0.54 s | 13.8 MiB |
| RDFS materialization, 1,001,418 triples to 2,751,996 with inferences | 3.974 s, of which inference takes 2.323 s | — |
| Numeric column added to the DBpedia index by `sparkles vocab-index` | 4.8 s | 20.7 MB |
| Predicate patterns, DBpedia 50M slice, 41.1M quads | 0.36 s of a 26.0 s load | 24.1 MB, 1.6% of the index |

The half-precision graph build takes the vector index build from 14.02 s to 6.43 s at
100,000 vectors and from 139.5 s to 63.5 s at a million, and raises the peak RSS of the
million-vector build from 10.2 GB to 12.6 GB.

### Validation

| Validation of 1.05M triples, medians of 5 | Parallel | Sequential |
|---|---:|---:|
| SHACL, 20 shapes, 48,428 results | 449 ms | 933 ms |
| ShEx, 152,000 shape associations | 1.532 s | 2.192 s |

<a id="partial-compaction-and-the-spatial-index"></a>

### Compaction

Compaction writes a new generation while writes go on. A partial compaction rewrites only
the index blocks a delta touches. This test loads the 10.5M data, copies the store for each
delta and compacts once with partial compaction forced and once without. Each delta is 90%
new `foaf:knows` links and 10% deleted `foaf:age` quads between existing terms.
Concentrated deltas touch a narrow range of people, and spread deltas choose people at
random. The generation has 2,254 blocks over seven permutations.

| Delta | Blocks rewritten | Partial build | Full build |
|---|---:|---:|---:|
| 1,000 concentrated | 102 | 0.112 s | 7.343 s |
| 10,000 concentrated | 107 | 0.584 s | 7.168 s |
| 100,000 concentrated | 117 | 0.234 s | 7.317 s |
| 1,000 spread | 1,201 | 0.572 s | 7.313 s |
| 10,000 spread | 1,223 | 0.597 s | 7.245 s |
| 100,000 spread | 1,223 | 0.669 s | 7.752 s |

Partial compaction is 12–66× faster in these samples, and the writer lock is held for 3.0
to 12.1 ms. Automatic selection chooses partial compaction for every delta here.

With a spatial index, the index's base is built alongside the new generation rather than
at the switch. On 100,000 point features with 500 commits before each compaction and 100
more during its build, the median writer lock at the switch is 4.9 ms without a spatial
index, 64.5 ms when the spatial base is built at the switch, and 25.3 ms when it is built
with the generation, which is the default.

### Document conversion in a child process

The server converts each PDF and Office document in a child process with resource limits
([C18](specs/C18-natural-language-questions-and-ingest.md) §7.1.1). Each figure is over 200 conversions of the
5 KB DOCX fixture with the release binary, made by the ignored test
`ingest::isolate::tests::overhead`.

| 5 KB DOCX | Median | 90th percentile |
|---|---:|---:|
| In process | 41 µs | 54 µs |
| On a thread, the fallback path | 85 µs | 184 µs |
| In a child process | 8.5 ms | 9.5 ms |

*Measured on a shared 16-core development laptop, not a quiet machine.*

Starting `/bin/true` took 2 to 4 ms on the same machine, so most of the child's cost is
starting a process. The child adds about 10 ms to each document, which is small next to
the model calls of an ingestion. Each running conversion may also use up to
`--convert-memory`, 2 GiB by default, of address space beside the server.

### Tables through mappings

A YARRRML mapping of a CSV table runs faster than C05's default mapping of the same
table ([spec F19](specs/F19-mappings-and-virtual-graphs.md#outcome)). The table has
1,000,000 rows of four columns, and both mappings write the same 4,000,000 triples, one
subject per row and one predicate per column. The figures are medians of three rounds of
the release binary at commit 9bd9feb6, run on atlas on 2026-10-24. Atlas is an Intel
i5-13500 with 20 threads, and other work shared it, so these are not quiet-machine
figures.

| 1,000,000 rows | Wall time | Peak memory |
|---|---:|---:|
| `sparkles csv convert`, the C05 default mapping | 2.81 s | 39 MB |
| `sparkles map run`, the YARRRML mapping | 1.61 s | 41 MB |
| `sparkles load`, the C05 default mapping | 4.12 s | 830 MB |
| `sparkles load --mapping`, the YARRRML mapping | 2.88 s | 834 MB |

C05's default mapping passes each cell through CSVW's datatype and URL template steps,
which the mapping engine does not have. A Parquet file of 10,000,000 rows
and two columns, 227 MB on disk, maps to 20,000,000 triples in 11.6 s with a peak of
46 MB, since the file is read one row group at a time.

## Writes and updates

Every Sparkles write is durable when it is acknowledged, because the WAL record and any
new vocabulary entries are synchronized first. The other engines use their defaults. TDB2
commits durably, QLever keeps updates in memory only, Fluree uses its default commit
path, and Oxigraph commits a RocksDB transaction that writes its WAL without
synchronizing it. Of these comparisons, only the one with TDB2 is between two commits that
are both synchronized.

### Durable commit latency

These serial single-triple commits alternate Sparkles and Oxigraph over 6 rounds, each
with a fresh copy of the store and a fresh server. Literal inserts add a new term to the
vocabulary, IRI inserts reuse known terms, and deletes remove triples.

| Serial commits, ms | Sparkles median | Sparkles p90 | Oxigraph median | Oxigraph p90 |
|---|---:|---:|---:|---:|
| 1.05M, literal insert | 1.126 | 1.345 | **0.613** | 1.255 |
| 1.05M, IRI insert | 1.080 | 1.273 | **0.627** | 1.298 |
| 1.05M, delete | 1.075 | 1.272 | **0.549** | 1.060 |
| 10.5M, literal insert | 1.284 | 2.781 | **0.926** | 1.318 |
| 10.5M, IRI insert | 1.200 | 2.702 | **0.929** | 1.360 |
| 10.5M, delete | 1.205 | 2.711 | **0.817** | 1.117 |

Oxigraph issued no device writes during these runs, while a run of 5,000 Sparkles commits
issued 14 device cache flushes.

The comparative suite's `updates` mode applies the same 5,000 single-triple INSERT and
DELETE requests, one at a time, to every engine's store. No engine failed a commit.
`mise run bench 100000 target/bench --mode updates` reproduces it.

| Churn of 5,000 commits | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: commits/s | **878** | 16 | 423 | 259 | 771 |
| 1.05M: latency p50 / p99 (ms) | **1.20 / 2.06** | 70.93 / 88.19 | 2.27 / 5.78 | 3.17 / 25.22 | 1.25 / 2.07 |
| 10.5M: commits/s | **909** | 26 | 387 | 361 | 715 |
| 10.5M: latency p50 / p99 (ms) | **1.10** / 2.74 | 19.75 / 90.27 | 2.48 / 6.30 | 1.69 / 6.46 | 1.35 / **2.14** |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-18 and 2026-10-19.*

The WAL preallocates space in steps from 64 KiB up to 4 MiB (`--wal-prealloc-kb`, `0`
disables it). Replay, readers, backups and quotas use the logical end of the last commit,
and recovery recognizes a torn tail.

### Concurrent durable commits

Group commit is the default, and `--no-group-commit` keeps the ordinary writer, which
synchronizes each commit on its own. The server ran on six CPUs and the client on four, and
six rounds alternated the two writers, each run in a fresh process on a fresh copy of the
store. Each cell is the median over the rounds. Every run completed with distinct,
contiguous receipts and the correct final state.

| 50,000 changes, transactions/s | 1.05M ordinary | 1.05M group commit | 10.5M ordinary | 10.5M group commit |
|---|---:|---:|---:|---:|
| Four writers | 3,399.8 | **6,173.8** | 3,375.6 | **6,160.0** |
| Sixteen writers | 3,342.8 | **18,119.2** | 3,332.0 | **18,304.4** |

| Request latency (ms) | 1.05M ordinary | 1.05M group commit | 10.5M ordinary | 10.5M group commit |
|---|---:|---:|---:|---:|
| Four writers, p50 / p99 | 1.159 / 1.383 | **0.631 / 0.954** | 1.168 / 1.381 | **0.633 / 0.952** |
| Sixteen writers, p50 / p99 | 4.738 / 5.867 | **0.813 / 1.467** | 4.752 / 6.083 | **0.793 / 1.445** |

*Measured on forge on 2026-10-17 at commit 758a6a25, with the same binary for both writers.*

With sixteen writers, no writer's share of the commits differed from the mean by more than
1.2% with group commit, and by 0.3% with the ordinary writer. A single writer gains
nothing from grouping, so a commit that finds no other writer waiting synchronizes as the
ordinary writer does. These serial runs send 5,000 single-triple commits.

| Serial commits, p50 / p90 (ms) | 1.05M ordinary | 1.05M group commit | 10.5M ordinary | 10.5M group commit |
|---|---:|---:|---:|---:|
| Literal insert | 0.426 / 0.461 | 0.427 / 0.459 | 0.429 / 0.463 | 0.421 / 0.458 |
| IRI insert | 0.426 / 0.458 | 0.425 / 0.459 | 0.422 / 0.460 | 0.425 / 0.464 |
| Delete | 0.425 / 0.459 | 0.430 / 0.459 | 0.423 / 0.456 | 0.425 / 0.456 |

The churn and mixed modes of `scripts/bench.sh` measured within 3% between the two writers
in the same session.

<a id="group-commit-on-indexed-datasets-on-forge"></a>

### Group commit on indexed datasets

Datasets with a full-text, spatial or embedding index also group their commits. These runs
apply the gates of [X09 section 17.9](specs/X09-group-commit-by-default.md#179-performance-gates)
with `scripts/bench-indexed-commit.py`, on native storage with no added delay. The server
ran on the six performance cores and their sibling threads, and the client on two
efficiency cores. Every cell is one index on one dataset of 1.05M or 10.5M triples, with
five blocks of four processes in the order ordinary, grouped, grouped, ordinary, so each
scenario has ten processes per setting. Every process started from a fresh copy of the
seed store, checked every acknowledgement and the index answers, and reopened cold to
check them again.

Under the gates, 35 of 36 serial checks, all six throughput gates and all ten memory gates
are met. The p90 of spatial literal inserts at 10.5M is the one serial miss. The reader
gates are met for the full-text cells at both sizes and the spatial cell at 10.5M. The
spatial reader gate at 1.05M is not established, because its ordinary writer did not keep
the offered schedule ([roadmap](ROADMAP.md) item 171).

*The serial, writer and memory cells were measured on forge on 2026-10-23 at commit
5b40d0ec, and the mixed cells on 2026-10-24 with the bounded full-text checkpoint.*

#### Serial commits

The serial scenarios commit 2,000 single changes after 200 warmup
changes. A serial commit finds the group idle and takes the ordinary path, so both columns
measure the same code. The paired ratio is the median over the ten pairs of a grouped
process with its neighbouring ordinary process in the same block, and the gate asks for at
most 1.05.

| Index | Dataset | Change | p50 ordinary (ms) | p50 grouped (ms) | p50 paired ratio | p90 ordinary (ms) | p90 grouped (ms) | p90 paired ratio |
|---|---|---|---:|---:|---:|---:|---:|---:|
| Full-text | 1.05M | literal | 1.774 | 1.768 | 0.984 met | 1.874 | 1.966 | 1.008 met |
| Full-text | 1.05M | iri | 0.464 | 1.053 | 1.001 met | 1.106 | 1.723 | 0.973 met |
| Full-text | 1.05M | delete | 0.412 | 0.407 | 0.985 met | 0.431 | 0.427 | 0.990 met |
| Full-text | 10.5M | literal | 0.437 | 0.443 | 1.000 met | 1.188 | 0.483 | 1.007 met |
| Full-text | 10.5M | iri | 0.412 | 0.414 | 1.004 met | 0.458 | 0.446 | 0.986 met |
| Full-text | 10.5M | delete | 0.411 | 0.410 | 1.002 met | 0.432 | 0.428 | 0.990 met |
| Spatial | 1.05M | literal | 1.733 | 0.433 | 0.989 met | 1.822 | 1.163 | 0.973 met |
| Spatial | 1.05M | iri | 0.411 | 0.413 | 1.010 met | 0.456 | 0.456 | 1.001 met |
| Spatial | 1.05M | delete | 0.406 | 0.409 | 1.011 met | 0.431 | 0.428 | 1.005 met |
| Spatial | 10.5M | literal | 1.736 | 1.786 | 1.025 met | 1.846 | 1.950 | 1.077 **missed** |
| Spatial | 10.5M | iri | 0.409 | 0.414 | 1.012 met | 0.449 | 0.463 | 1.032 met |
| Spatial | 10.5M | delete | 0.412 | 0.411 | 0.996 met | 0.435 | 0.429 | 0.985 met |
| Embedding | 1.05M | literal | 0.473 | 0.441 | 0.978 met | 1.536 | 0.797 | 0.960 met |
| Embedding | 1.05M | iri | 0.410 | 0.415 | 1.002 met | 0.431 | 0.448 | 1.008 met |
| Embedding | 1.05M | delete | 0.411 | 0.410 | 0.996 met | 0.435 | 0.434 | 1.003 met |
| Embedding | 10.5M | literal | 0.381 | 0.376 | 0.987 met | 1.852 | 1.843 | 0.976 met |
| Embedding | 10.5M | iri | 0.411 | 0.412 | 1.010 met | 0.437 | 0.441 | 0.999 met |
| Embedding | 10.5M | delete | 0.415 | 0.417 | 1.011 met | 0.441 | 0.452 | 1.019 met |

The medians over ten processes follow the drive more than the writer. The mean time of a
commit synchronization was about 0.23 ms in some processes and between 1.2 and 1.7 ms in
others of the same cell and setting, and it often changed inside a block. Of the 360 serial
processes, 32 of 180 ordinary and 27 of 180 grouped ones had a mean synchronization above
1 ms, with a Fisher test p value of 0.57, which is why the full-text IRI inserts at 1.05M
have p50 medians of 0.464 and 1.053 ms and a paired ratio of 1.001. The engine overhead,
the mean request latency less the mean synchronization, differs by at most 3.6% between the
settings in 8 of 10 cells where it can be computed. It is 12.7% higher with grouping for
spatial literals at 10.5M, 0.240 against 0.213 ms, the cell that misses. Of the 36 paired
medians, 19 are above 1.0, with a median of 1.001 and a sign test p value of 0.43, so the
single-writer path shows no general cost.

#### Four writers

The writer scenario sends 50,000 changes from four clients. The paired
ratio takes the median over the ten pairs of a grouped process with its neighbour in the
same block, and the gate asks it to reach the required gain, which is 1.5 when the
ordinary path's mean commit synchronization takes 1.2 ms or longer and falls in proportion
to it below that. The peak RSS difference subtracts the ordinary process's sampled peak
inside the write interval from the grouped process's peak over the whole window, and the
limit is 64 MiB.

| Index | Dataset | Ordinary tx/s | Grouped tx/s | Paired ratio | Ordinary sync (ms) | Required gain | Peak RSS difference |
|---|---|---:|---:|---:|---:|---:|---:|
| Full-text | 1.05M | 926 (637–1,008) | 1,241 (1,207–1,503) | 1.496 met | 0.98 | 1.408 | 8.5 MiB met |
| Full-text | 10.5M | 795 (634–1,028) | 1,311 (1,154–1,543) | 1.675 met | 1.16 | 1.483 | -3.4 MiB met |
| Spatial | 1.05M | 860 (638–1,071) | 1,284 (1,199–4,106) | 1.522 met | 1.09 | 1.452 | 7.8 MiB met |
| Spatial | 10.5M | 929 (726–1,035) | 1,243 (1,118–3,634) | 1.593 met | 0.99 | 1.413 | 9.3 MiB met |
| Embedding | 1.05M | 650 (463–740) | 1,549 (766–1,816) | 2.382 met | 1.03 | 1.428 | 19.6 MiB met |
| Embedding | 10.5M | 608 (569–662) | 1,245 (1,151–1,256) | 2.059 met | 1.10 | 1.458 | 0.1 MiB met |

With four clients a fence carries a median of 1.9 to 2.0 commits in the text and spatial
cells, so a commit waits for about two synchronizations and the gain is bounded near 2. The
ordinary writer ran at 950 to 1,010 transactions/s in the first position of a block and at
700 to 740 in the last, after two grouped runs, so the gain depends on the neighbour. It is
1.26 to 1.60 against the first position and 1.56 to 2.26 against the last. The grouped
spatial processes at 3,634 and 4,106 transactions/s ran in the fast state of the drive,
where the mean synchronization was 0.44 ms, and barely move the medians.

#### Readers during writes

The mixed scenario paces four writers at 60% of the slowest
unpaced process, the same offered rate for both settings, and two readers run the index
query throughout, counting the matches of a text query or a spatial window. The medians
pool the ten processes of each setting, the ratios are the paired medians of grouped to
ordinary, and the gate asks for at most 1.10 with a schedule lag of at most 50 ms.

| Cell | Rate (tx/s) | Reader p50, ordinary and grouped (ms) | Reader p99, ordinary and grouped (ms) | Paired p50 and p99 ratios | Max schedule lag, ordinary and grouped (ms) | Gate |
|---|---:|---|---|---|---|---|
| Text at 1.05M | 414 | 4.85, 4.84 | 7.94, 7.72 | 1.001, 0.993 | 6.3–30.1, 4.2–12.1 | Met. |
| Text at 10.5M | 354 | 4.87, 4.83 | 8.00, 8.10 | 0.987, 1.008 | 3.7–6.4, 2.2–10.1 | Met. |
| Spatial at 1.05M | 583 | 1.04, 0.98 | 1.66, 1.35 | 0.947, 0.813 | 136.1–271.7, 8.0–11.7 | Not established. |
| Spatial at 10.5M | 444 | 1.03, 1.02 | 1.48, 1.45 | 0.999, 0.988 | 3.5–12.3, 4.0–8.8 | Met. |

No text process stalled. The ordinary spatial processes at 1.05M had no slow request, but
in 36 to 56 of their 86 seconds four queued commit synchronizations took about 7 ms, so
they sustained about 570 transactions/s against the offered 583. The unpaced calibration
averaged the drive's fast and slow states, and [roadmap](ROADMAP.md) item 171 corrects it.
The text readers' paired ratios of 1.001 and 1.008 are within the gate and the spread of
the runs, and [roadmap](ROADMAP.md) item 170 measures them with more rounds.

#### Memory

Every cell stayed under the 64 MiB limit. The certified peak resident memory
difference was between −3.4 and 19.6 MiB for the writer scenarios and between 0.3 and
5.6 MiB for the mixed scenarios, where both settings run for the same time. The
certificate subtracts the lower sampled bound of the ordinary process, so it errs upward.

#### Limits of these runs

A block carries over its state. The drive's flush time lasts for minutes and
crosses blocks, so ten processes are not ten independent draws, and the order of
ordinary, grouped, grouped and ordinary cancels a linear drift but not what the previous
process wrote. The embedding cell has no reader gate, and its worker called a
deterministic local provider.

<a id="bounded-full-text-checkpoint-on-forge"></a>

### Full-text checkpoint

The full-text index checkpoints in the background. A checkpoint synchronizes only the
files written since its last pass, from eight threads so that ext4 commits them together,
and repeats its unlocked pass up to four times while new files remain before it takes the
live writer lock for a final locked pass. Commits written faster than they can be
synchronized therefore still end in a locked pass. In this trace no tick needed the lock for
its I/O. The trace ran the mixed scenario of the full-text cell at 1.05M, one block of
an ordinary, two grouped and an ordinary process at a fixed 500 transactions/s and two
readers, with the debug target `sparkles::text::checkpoint`. A stall is a run of writes
slower than 25 ms that starts within 1 s of the last.

| Setting | Locked passes | Unlocked pass p50 (ms) | Stalls | Longest write (ms) | Max schedule lag (ms) | Write p99 (ms) | Reader p99 (ms) |
|---|---:|---:|---:|---:|---:|---:|---:|
| Ordinary | 0 | 27.7 | 0 | 19.5 | 31.0, 37.9 | 10.04, 9.41 | 7.69, 7.48 |
| Grouped | 0 | 31.8 | 0 | 18.6 | 10.1, 10.7 | 6.83, 6.70 | 7.51, 7.19 |

*Measured on forge on 2026-10-24. Each cell with two values gives the two processes of
that setting.*

A checkpoint that took the writer lock through a synchronization of every index file
stalled writes for up to about 170 ms every 5.3 s. The reader p99 of the current
checkpoint is 7.47 ms on average against 7.20 ms with the locked design, 3.8% higher over
four processes each, within the spread of these runs. The eight synchronizing threads may
compete with readers for the drive, and [roadmap](ROADMAP.md) item 170 follows it up.

### Reader tail of group commit

Four writers and two readers ran against the 10.5M dataset, six rounds per setting. In the
paced run both settings were held to 3,452 transactions/s, the most the ordinary writer
reaches. The paced run is the X09 acceptance gate, and the unpaced run shows the read cost
of the higher write rate that group commit allows.

| Measure | Unpaced, ordinary | Unpaced, group commit | Ratio | Paced, ordinary | Paced, group commit | Ratio |
|---|--:|--:|--:|--:|--:|--:|
| Transactions/s | 3,452.5 | 5,995.8 | 1.737 | 3,426.7 | 3,449.7 | 1.007 |
| Writer p50 (ms) | 1.126 | 0.634 | 0.563 | 1.119 | 0.712 | 0.636 |
| Reader p50 (ms) | 0.482 | 0.332 | 0.689 | 0.478 | 0.496 | 1.037 |
| Reader p99 (ms) | 4.639 | 5.039 | 1.086 | 3.980 | 3.986 | 1.002 |

*Measured on forge on 2026-10-16.*

### Concurrent writers with snapshot readers

Four writers insert 5,000 fresh quads while two readers each issue up to 25 queries per
second. Each read counts the inserted namespace and checks the count against the commit
snapshot named in the response. The state, the history and the reopened store are checked
afterwards.

| Four writers and two readers | Ordinary writer | Group commit |
|---|---:|---:|
| 1.05M, single-quad requests, transactions/s | 1,375–1,386 | **3,344–3,365** |
| 1.05M, writer p50 (ms) | 2.796–2.810 | **1.142–1.143** |
| 1.05M, reader p50 (ms) | 0.845–0.862 | **0.545–0.585** |
| 1.05M, 100-quad requests, effective changes/s | 74,231–75,187 | **118,627–123,253** |
| 10.5M, single-quad requests, transactions/s | 202–231 | **370–386** |
| 10.5M, writer p50 (ms) | 19.093–19.274 | **9.932–10.315** |
| 10.5M, reader p50 (ms) | 0.917–0.936 | 0.895–1.000 |
| 10.5M, reader p99 (ms) | **1.410–1.549** | 1.960–2.143 |

Readers keep their scheduled rate and see advancing snapshots throughout, so these runs
show reads proceeding under write load rather than saturated query throughput. The
100-quad runs at 1.05M last 41–67 ms and are short-burst observations.

<a id="http-write-snapshot"></a>

### Write throughput with one client

One client sends requests to a server on one logical CPU, starting from a compacted
1.05M store. Throughput counts effective changes, so it is not interchangeable with
transactions per second. Ranges span the fresh processes.

| One client | Changes per request | Effective changes/s | Request p50 (ms) | Request p99 (ms) |
|---|---:|---:|---:|---:|
| 1.05M, single-triple insert and delete churn | 1 | 1,046–1,062 | 0.856–0.870 | 1.536–1.609 |
| 1.05M, inserts of new literals | 100 | 53,979–59,414 | 1.493–1.515 | 3.964–7.118 |
| 1.05M, insert and delete batches | 100 | 54,972–58,586 | 1.516–1.548 | 2.857–3.981 |
| 10.5M, single-triple insert and delete churn | 1 | 320–494 | 2.154–2.351 | 5.989–7.733 |
| 10.5M, insert and delete batches | 100 | 45,788–46,123 | 1.613–1.622 | 16.070–16.364 |

On the same new-literal workload, two Oxigraph processes produced 34,641 and 58,853
effective changes per second, without synchronizing its WAL. The ranges overlap.

### Mixed load

Sixteen `star-join` readers run for 30 s with `oha` while one writer commits
single-triple `INSERT DATA` requests of new literals as fast as it can. Every request
opens a new connection. `mise run bench 100000 target/bench --mode mixed` reproduces it.

| Mixed load | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: reads/s | **2491.3** | 80.6 | 522.9 | 36.5 | 15.0 |
| 1.05M: read p50 / p99 (ms) | **6.1 / 10.9** | 193.8 / 376.2 | 26.9 / 36.3 | 423.0 / 739.7 | 1014.2 / 1855.0 |
| 1.05M: writes/s | 374.2 | 15.2 | 97.1 | 249.2 | **1272.6** |
| 1.05M: write p50 / p99 (ms) | 2.3 / 6.4 | 67.9 / 86.6 | 6.2 / 30.5 | 3.8 / 14.2 | **0.8 / 1.0** |
| 1.05M: failed reads / writes | 0 / 0 | 0 / 0 | 30034 / 1046 | 0 / 0 | 0 / 0 |
| 10.5M: reads/s | **244.1** | 5.7 | 74.7 | 3.4 | 0 |
| 10.5M: read p50 / p99 (ms) | **64.6 / 84.3** | 2704.1 / 3588.4 | 212.1 / 257.6 | 4284.9 / 6931.1 | — |
| 10.5M: writes/s | 265.8 | 18.1 | 47.5 | 237.8 | **565.1** |
| 10.5M: write p50 / p99 (ms) | 3.5 / 8.0 | 67.3 / 87.7 | 10.7 / 133.6 | 3.8 / 16.8 | **1.6 / 3.9** |
| 10.5M: failed reads / writes | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-18 and 2026-10-19.*

Sparkles serves 4.8× QLever's reads at 1.05M and 3.3× at 10.5M. Oxigraph commits more
writes at both sizes while it serves far fewer reads. At 10.5M no Oxigraph reader finished
within 30 s. A separate run with the durable-commit harness above, with 16 readers on the
10.5M store, completed 299 new-literal commits per second at a median of 3.2 ms.

### Queries after updates

The `updates` mode applies the same 5,000 single-triple changes to each engine's store and
runs the query suite again. The changes stay in Sparkles' delta until a compaction folds
them into the base index. At 10.5M the delta changes `two-hop-count` from 17.5 to
36.9 ms, `contains` from 7.0 to 15.6 ms and `regex-iri` from 8.3 to 18.0 ms, changes
`star-join` from 36.7 to 39.0 ms, and changes throughput from 239 to 217 q/s, against
QLever's 85 q/s on its changed store. On the changed stores Sparkles is fastest on 27 of
28 queries at both sizes. QLever's changed-store `types-grouped` answer differs in value
and is not ranked. Fluree is ahead on `two-hop-count` by 1.10× at 1.05M, 7.8 against
8.6 ms, and by 1.06× at 10.5M. Fluree's 10.5M run on its changed store failed the same way
as its run on the loaded store, with `optional-chain` killed at the memory limit and
`count-all` recorded as an error.

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d. The records for
[1.05M](benchmarks/2026-10-23-synthetic-updates-1.05m-forge.json) and
[10.5M](benchmarks/2026-10-23-synthetic-updates-10.5m-forge.json) have every sample.*

<a id="automatic-compaction-105m-triples"></a>

### Automatic compaction

This test sends 50,000 single-triple commits, 35,000 inserts and 15,000 deletes, to the
1.05M store, each as its own HTTP request.

| Query, median of 20 | With a 50,000-quad delta | After an automatic compaction |
|---|---:|---:|
| `types-grouped` | 5.8 ms | 1.9 ms |
| `predicate-counts` | 2.0 ms | 1.9 ms |
| `distinct-obj` | 7.8 ms | 2.2 ms |

That compaction took 2.0 s and held the writer lock for 5.7 ms. With `deltaRatio` 0.01,
two compactions ran during the 50,000 commits. They took 1.26 s and 1.44 s, carried 348
and 497 commits made during their builds into the new generation, and held the writer
lock for 3.4 ms and 3.3 ms. The commits made during a compaction had a p50 of 2.0 ms and
a p99 of 15.1 ms, against 2.7 ms and 21.0 ms in the run without compaction, so a
compaction does not slow the writes around it.

*Measured on the development laptop with other builds running, at a load average of about
38 on 16 cores, so the tails are noisy.*

<a id="other-measurements"></a>

### Validation on writes

Write-time validation checks only the focus nodes a write can affect. On 1.05M quads,
single-triple HTTP inserts take about the same time with validation off, in `warn` mode
and in `reject` mode, with or without 10,000 pending delta changes. Each mode had 20
samples, and its mean varies by several milliseconds between runs, so this test shows
incremental validation rather than a latency difference between modes.

| Single-triple insert, mean ms | off | warn | reject | full validation of the store |
|---|---:|---:|---:|---:|
| SHACL, plain write, no delta | 9.58 ± 1.13 | 6.76 ± 2.46 | 10.04 ± 2.05 | 478 to 485 |
| SHACL, write of a person, 10,000-change delta | 9.28 ± 2.06 | 8.90 ± 2.55 | 11.02 ± 2.49 | 488 to 489 |
| ShEx, plain write, no delta | 10.02 ± 2.25 | 10.63 ± 2.28 | 10.13 ± 2.26 | 1,091 to 1,432 |
| ShEx, write of a person, 10,000-change delta | 8.33 ± 2.84 | 11.37 ± 1.77 | 10.23 ± 2.69 | 1,046 to 1,398 |

<a id="spatial-index-commit-cost"></a>

### Spatial index commit cost

On the native store, a one-triple commit without geometry takes 0.025 ms with the spatial
index on and off, medians of 200. A commit of 1,000 `geo:asWKT` points takes 4.119 ms with
the index and 3.546 ms without it, medians of 20.

<a id="branches-and-merges-105m-triples"></a>

### Branches and merges

A release build served the 10.5M data. Each figure is one request's `time_total`, except
the queries, which are medians of 15 interleaved requests.

| Step | Time |
|---|---:|
| Create a branch of `main` at its head (four runs) | 19.5–32.7 ms |
| `star-join` on `main` | 38.3 ms |
| `star-join` on the new branch | 38.7 ms |
| Preview a merge of 10,000 inserted quads into a `main` with 10,000 of its own | 61.9 ms |
| That merge, committed | 114.9 ms |
| First request to the branch after a restart | 3.2 ms |

*Measured on the development laptop, which other builds kept at a load average of 35 to 85
on 16 cores.*

A new branch writes about 1.4 KB of files and takes about 70 KB on disk once its change
log opens, because it shares its upstream's index files until it compacts. A query on the
branch reads the same cached blocks as `main` and takes as long.

<a id="point-in-time-reads-105m-triples"></a>

### Point-in-time reads

A 1,000,000-triple base takes 50,000 single-quad commits in one generation, with history
retention covering every commit. Request times exclude process startup, and repeated rows
are medians of 20.

| Request | Time |
|---|---:|
| `COUNT(*)` at base + 25,000, first | 19.89 ms |
| The same, second / warm median | 1.04 / 0.95 ms |
| Selective read at base + 25,001, first / warm | 1.20 / 0.69 ms |
| `COUNT(*)` at the sealed base, first / warm | 18.12 / 0.67 ms |
| Sealed base, first after a restart (median of 3) | 14.40 ms |
| After a restart, at base + 25,000 | 21.39 ms |

A read starts from the nearest known state, found through a sparse WAL index, and replays
the commits in between, so a first read of an uncached state costs more than a repeat. In
the native store, a read of the middle of a 50,000-commit history takes 42.33 ms on first
use and 1.82 µs from the snapshot cache, a diff of one commit at the head 0.126 ms and of
100 commits 0.181 ms, and reopening with a replay of all 50,000 commits 221.51 ms.

### Diffs grouped by class

A base of 200,000 typed resources with one link each takes one commit of 1,000, 10,000 or
100,000 changes, half of them type triples of new resources and half links from those
resources to the base. `Diff::group_by_class` reads the classes of both ends of each
change, which here gives 30 groups. Times are the best of five, and
`cargo test --release -p sparkles-core --test diff_groups -- --ignored` runs it.

| Changes | Diff | Grouping by class |
|---:|---:|---:|
| 1,000 | 0.68 ms | 1.30 ms |
| 10,000 | 5.35 ms | 11.21 ms |
| 100,000 | 78.63 ms | 119.54 ms |

*Measured on a development laptop that was running other work, so the times are
approximate.*

Grouping costs about 1.2 µs per change on top of the diff, and it grows with the changes
rather than with the dataset.

<a id="backup-repositories-105m-triples"></a>

### Backup repositories

`mise run bench:backup` ran against the 10.5M database (299.8 MB) with a warm page cache
and an `fs` repository on the same ext4 file system. Each figure is the median and range
of 3 rounds, including process start.

| Case | Seconds | Size |
|---|---:|---|
| Full backup into an empty repository | 0.59 (0.56–0.61) | 299.8 MB logical, 281.1 MB stored, 509 MB/s |
| Incremental backup after 1,000 single-quad commits | 0.34 (0.33–0.34) | 55.2 KB added (3 new blobs, 29 reused) |
| Restore into a new directory, with the quick check | 0.81 (0.71–1.63) | 300.0 MB |
| Verify at level `data` | 0.21 (0.20–0.22) | 33 GETs |

An incremental backup stores only the appended tail of the update log and the files that
changed, so its size follows the writes since the last backup. S3, cold caches and
databases larger than memory were not measured.

### Incremental views

`crates/sparkles-core/tests/incr_bench.rs` measures Phase 1 of
[F13](specs/F13-incremental-views-and-provenance.md) on synthetic data in which each
person belongs to a team, has an age and knows two other people, and half the teams have a
label. The runs used a release build on atlas, an Intel Core i5-13500 with 20 threads,
with no other work on the host. The SNB, BSBM and fan-out benchmarks of the spec's §11 are
not built, and item 225 tracks them.

Each row below defines one view over an in-memory store, then makes 200 commits of one or
two quads each and times the round that brings the view up to date. A round includes
reading the changes, building the snapshot before them and the circuit's own work. The
query column is the median of 5 evaluations of the same query with the cache off.

| View | People | Query ms | Pass median ms | Pass p95 ms | Query / pass | State MiB |
|---|---:|---:|---:|---:|---:|---:|
| `team-sizes`, COUNT by team | 200,000 | 0.4 | 0.012 | 0.020 | 35 | 0.2 |
| `team-ages`, SUM and AVG by team over a join | 200,000 | 5.1 | 0.020 | 0.032 | 259 | 0.4 |
| `colleagues-known`, a three-pattern join | 200,000 | 9.8 | 0.193 | 0.313 | 51 | 0.0 |
| `labelled`, OPTIONAL label | 200,000 | 1.7 | 0.010 | 0.018 | 167 | 22.9 |
| `teams`, DISTINCT | 200,000 | 0.6 | 0.007 | 0.016 | 89 | 0.1 |
| `team-sizes` | 1,000,000 | 0.5 | 0.035 | 0.042 | 15 | 0.8 |
| `team-ages` | 1,000,000 | 27.1 | 0.096 | 0.115 | 282 | 1.8 |
| `colleagues-known` | 1,000,000 | 78.2 | 0.230 | 0.588 | 340 | 0.0 |
| `labelled` | 1,000,000 | 9.5 | 0.011 | 0.019 | 838 | 114.7 |
| `teams` | 1,000,000 | 4.0 | 0.008 | 0.017 | 492 | 0.5 |

The spec's target is a pass at least 100 times cheaper than evaluating the query again.
At a million people, four of the five views meet it. `team-sizes` misses it at both sizes
because the query engine answers a grouped COUNT over one pattern from the index in under
a millisecond. At 200,000 people, the join view and the two cheap queries miss it as well,
because the queries take only a few milliseconds there. `colleagues-known` stores no join
state, so each change probes the indexes, and its pass is the slowest. The `labelled`
view keeps its output rows, which is 115 MiB for 1M rows.

The commit path was measured on a persistent store of 50,000 people with two-quad
commits from one writer, using the default store options. Each figure is the median of
3,000 commits, in 3 rounds.

| Case | Commit median ms | Commit p99 ms |
|---|---:|---:|
| Nothing subscribed | 0.321–0.335 | 0.472–0.504 |
| One live query subscribed | 0.345–0.346 | 0.511–0.548 |

With nothing subscribed, a commit only checks an atomic counter, and its time matches a
build without views, which measured 0.329–0.336 ms. While a view or live query is
subscribed, each commit also copies its changes into the ring that passes read, which
adds about 4%. Item 226 tracks measuring it on forge and reducing it.

With 0, 10 and 100 views defined and a worker thread keeping them current, 2,000 commits
gave the following times. Each view had its own circuit.

| Views | Commit median ms | Commit p99 ms | Commits per second | Catch-up after the last commit ms |
|---:|---:|---:|---:|---:|
| 0 | 0.324 | 1.908 | 1,609 | 0 |
| 10 | 0.229 | 0.355 | 4,175 | 64 |
| 100 | 0.230 | 0.370 | 4,128 | 5,352 |

The writer does not wait for passes, so the commit time does not grow with the number of
views. The run with no views came first and paid for a cold page cache. With 100 views, the
single worker thread falls behind the writer and needs 5.4 seconds after the last commit to
catch up. Item 223 tracks passes on several threads.

On a dataset with RDFS on read or materialized inferences, a view evaluates its whole query
again with them, and item 221 tracks maintaining it incrementally. The test
`inferred_pass_against_query` in the same file adds a small schema to the data and, for the
materialized case, an inferred `rdf:type` per person. It then makes 100 commits that touch
each view and 100 that do not, and compares the round after a touching commit with a
one-off evaluation of the query. The query column below is the median of 5 one-off
evaluations with the same inference options and the cache off. Each figure is the median
of three runs. The ratio column takes the median of each run's ratio separately, so it
need not equal the quotient of the rounded time columns.

| View | People | Query ms | Round median ms | Round p95 ms | Median round / query | Round after an unrelated commit ms |
|---|---:|---:|---:|---:|---:|---:|
| RDFS, COUNT by team over a superproperty | 200,000 | 18.4 | 17.2 | 17.5 | 0.93 | 0.031 |
| RDFS, COUNT of a class reached by a domain | 200,000 | 6.0 | 6.1 | 6.2 | 1.01 | 0.041 |
| Materialized, COUNT by team | 200,000 | 2.0 | 2.1 | 2.2 | 1.07 | 0.011 |
| RDFS, COUNT by team over a superproperty | 1,000,000 | 215.4 | 195.6 | 209.3 | 0.93 | 0.044 |
| RDFS, COUNT of a class reached by a domain | 1,000,000 | 40.5 | 40.8 | 41.0 | 1.00 | 0.053 |
| Materialized, COUNT by team | 1,000,000 | 9.8 | 10.3 | 10.4 | 1.05 | 0.011 |

These runs used a release build in the Nix development shell on forge, an Intel Core
i5-13500, pinned to CPUs 0–11 with no other work on the host.

A round after a touching commit costs about one evaluation of the query. A round after a
commit outside the rewritten query's footprint skips the view and takes at most 0.053 ms
in these medians. A view of the materialized class count ran on the 34 commits that changed
the inferred graph and skipped the other 166. Its 95th percentile round took 3.8 ms at
200,000 people and 12.6 ms at 1,000,000 people, against one-off queries of 3.8 ms and
12.5 ms.

The persistent commit-path comparison remains inconclusive. It used the same forge host,
after SSD trimming, with matched release compilers and three A,B,B,A groups. Each process
made three rounds of 3,000 two-quad commits without a subscription, then three with a live
query subscribed. The medians of the unsubscribed rounds ranged from 0.272 to 1.611 ms
on the reference build and from 0.274 to 1.604 ms on the build with inferred views. The
ratio of group medians changed from 3.40 to 0.30 and 0.18 across the three groups, so the
run does not isolate a cost of the inference change. Item 265 tracks profiling this
latency variation and obtaining a stable comparison.

Syscall tracing of four more processes in A,B,B,A order, after SSD trimming on forge,
located the large variation inside writes to the WAL and delta vocabulary. Blocks of
3,000 aligned writes had median syscall times between about 0.24 and 1.55 ms. Both
builds switched between faster and slower writes within a process. Tracing changes the
timings, so these figures diagnose where the variation occurs rather than measure the
inference change's cost. Item 265 requires a comparison with matched durable-write
latency before attributing a difference to the engine.

## Bindings

The bindings embed the engine in a JVM, Python or Node.js process. The comparison of
`mise run bench:bindings` loads 105,302 synthetic triples (10,000 people) into each library
and runs the suite's 28 queries through each library's own API, consuming every row and
term. It ran on `atlas` with the processes pinned to CPUs 0–11, two processes per engine
and mode, one warm-up and five samples per case, and reports medians of the process
medians. Every engine's answers agree by value on all 33 cases.

### JVM

Jena's `QueryExec` runs on a `DatasetGraphSparkles`, on disk and in memory, against TDB2
on disk and Jena's in-memory TIM dataset.

| Case, ms | Sparkles on disk | Sparkles in memory | TDB2 | TIM |
|---|---:|---:|---:|---:|
| Load the N-Triples file (s) | **0.51** | 0.53 | 1.30 | 0.87 |
| `count-all` | 0.988 | **0.979** | 23.7 | 46.3 |
| `star-join` | 1.728 | **1.694** | 5.740 | 4.014 |
| `two-hop-count` | 1.239 | **1.122** | 56.3 | 32.6 |
| `group-avg` | 1.478 | **1.456** | 25.5 | 15.3 |
| `order-by-full` | **6.991** | 8.091 | 41.4 | 22.8 |
| `export-500k` | 28.0 | **25.2** | 54.0 | 44.5 |
| `knows-reach` | 0.570 | 0.591 | 0.565 | **0.449** |
| `star-lookup` | 0.739 | 0.800 | 0.739 | **0.426** |
| `values-star` | 0.775 | 0.952 | 0.662 | **0.463** |
| `employee-docs` | 0.626 | 0.707 | 0.509 | **0.391** |
| Peak RSS, load process (MiB) | **149** | 151 | 457 | 416 |
| Peak RSS, read process (MiB) | **345** | 346 | 472 | 590 |

Over all 28 queries, Sparkles on disk is faster than TDB2 on 24, with a geometric mean
8.1× faster, and Sparkles in memory is faster than TIM on 24, 5.8× faster. The four
losses are the small lookups in the last rows, where a query's fixed cost through Jena
dominates.

Calls that read a few triples go through hand-written JNI entry points. This table gives
their latency on one thread in microseconds, from the same dataset in memory, against
TDB2 and TIM.

| Operation, µs | Sparkles | TDB2 | TIM |
|---|---:|---:|---:|
| `Graph.contains` in its own read transaction | 1.68 | 2.47 | **0.53** |
| `Model.getProperty` | 2.64 | 3.46 | **0.68** |
| `find` on a subject | 5.20 | 5.59 | **1.71** |
| `ASK` | 42.69 | 28.75 | **26.18** |
| `SELECT ?o` on a subject | 42.61 | 27.48 | **23.92** |
| `star-lookup` | 233.7 | 209.8 | **115.5** |
| `values-star` | 195.8 | 101.7 | **88.9** |

On four threads, `contains` reaches 1,680,000 operations per second against TDB2's
678,000, `getProperty` 1,130,000 against 657,000, and `find` on a subject 704,000 against
446,000. A small `ASK` reaches 43,200 queries per second against TDB2's 45,500.

### Python

The native API runs against pyoxigraph 0.5.11, and the rdflib store plugin against
rdflib's in-memory store.

| Case, ms | sparkles | pyoxigraph | rdflib on Sparkles | rdflib Memory |
|---|---:|---:|---:|---:|
| Load the N-Triples file (s) | 0.15 | **0.14** | 1.46 | 1.52 |
| `count-all` | **0.236** | 14.8 | 2.413 | 641.5 |
| `star-join` | **1.224** | 8.567 | 7.682 | 400.3 |
| `two-hop-count` | **0.368** | 27.9 | 4.295 | 600.5 |
| `group-avg` | **0.711** | 6.921 | 3.580 | 237.7 |
| `order-by-full` | **20.5** | 41.6 | 76.5 | 511.1 |
| `export-500k` | **233.1** | 308.9 | 881.9 | 2,234 |
| `knows-reach` | 0.115 | **0.046** | 1.819 | 2.060 |
| `star-lookup` | **0.291** | 0.316 | 3.928 | 4.625 |
| `values-star` | 0.177 | **0.126** | 3.887 | 3.671 |
| `employee-docs` | 0.164 | **0.140** | 2.337 | 3.234 |
| Peak RSS, load process (MiB) | 65 | 65 | 127 | 160 |
| Peak RSS, read process (MiB) | 135 | **97** | 197 | 291 |

Over all 28 queries the native API is faster than pyoxigraph on 25, with a geometric
mean 7.5× faster. Through rdflib's API the plugin answers every query faster than
rdflib's own store, by a geometric mean of about 42×.

### Node.js

`@sparkles-rdf/engine` runs against Oxigraph's JavaScript package and N3.js's `Store`
queried through Comunica.

| Case, ms | @sparkles-rdf/engine | oxigraph (JS) | N3.js and Comunica |
|---|---:|---:|---:|
| Load the N-Triples file (s) | **0.17** | 0.22 | 0.39 |
| `count-all` | **0.462** | 25.6 | 397.8 |
| `star-join` | **3.301** | 102.0 | 212.1 |
| `two-hop-count` | **0.606** | 37.5 | 278.1 |
| `group-avg` | **1.161** | 10.8 | 210.8 |
| `order-by-full` | **91.6** | 599.5 | 192.6 |
| `export-500k` | 586.8 | 3,900 | **406.8** |
| `knows-reach` | 0.340 | **0.308** | 2.932 |
| `star-lookup` | 1.033 | **0.757** | 28.0 |
| `values-star` | 0.564 | **0.488** | 8.004 |
| `employee-docs` | 0.613 | **0.516** | 27.1 |
| Peak RSS, load process (MiB) | **94** | 143 | 341 |
| Peak RSS, read process (MiB) | **238** | 739 | 459 |

Over all 28 queries the engine is faster than Oxigraph's package on 24, with a geometric
mean 9.9× faster, and faster than N3.js with Comunica on 27.

### Per-call costs

Calls that touch a few quads, such as a `has()` probe or a single add in a transaction,
cost more through a native binding than through a store written in the host language.
These runs use the same 105,000 triples, A/B/B/A order in fresh processes pinned to six
cores, and four to six process medians per arm.

| Python, per call | sparkles | pyoxigraph |
|---|---:|---:|
| Single `tx.add` | **6.0 µs** | 6.8 µs |
| `add_all`, per quad | **5.2 µs** | 7.7 µs |
| `in` | **1.1 µs** | 1.4 µs |
| First object of `(s p ?)` | 1.7 µs | **1.6 µs** |
| `(s ? ?)`, per subject | **9.4 µs** | 9.7 µs |
| `(? p o)` over 2,983 matches | **2.5 ms** | 2.9 ms |
| Iterate every quad | **102 ms** | 119 ms |
| Small `SELECT` with a five-subject `VALUES` | 246 µs | **147 µs** |

Through the rdflib plugin, against rdflib's Memory store, `(? p o)` takes 4.1 ms against
4.1 ms, iterating every triple 122 ms against 155 ms, a `contains` probe 4.0 µs against
3.1 µs and `Graph.value` 6.7 µs against 5.1 µs.

| Node.js, per call | @sparkles-rdf/engine | N3.js `Store` |
|---|---:|---:|
| Single add in a transaction | 7.6 µs | **5.1 µs** |
| `addAll`, per quad | 7.0 µs | **5.8 µs** |
| `has()` | 1.9 µs | **1.3 µs** |
| First object of `(s p ?)` | **6.5 µs** | 8.0 µs |
| `(s ? ?)`, per subject | **12 µs** | 21 µs |
| `(? p o)` over 2,983 matches | 2.5 ms | **2.4 ms** |
| Iterate every quad | **59 ms** | 123 ms |

On the same kind of instance, ordinary Jena use on Sparkles took 25 µs for an `ASK` on a
bound triple against TDB2's 27 µs, 22 µs for a `SELECT ?o` on a subject against 23 µs,
10 µs for an initial binding on a parsed query against 12 µs, and 52 µs for a DESCRIBE
of one resource against 36 µs. A DELETE/INSERT WHERE on one resource in the in-memory
store took 13 to 15 ms against TDB2's 2.3 to 3.2 ms.

*Measured on 8-vCPU AMD EPYC instances of a cloud CI provider, not a quiet machine. Runs on
different instances differed by up to 30%.*

## Natural-language questions

These numbers measure `sparkles ask` and global answering from community reports with
Anthropic models. They depend on the models and prompts more than on the engine.
`testsuite/ask/README.md` and `testsuite/graphrag/README.md` explain the harnesses, which
call the provider through a local proxy that holds every request against a cost cap.

| Measurement context | |
|---|---|
| Date and host | 2026-10-24, on a development laptop with the DBpedia database on local disk |
| Models | Claude Haiku 5.5, Sonnet 5.5 and Opus 5.5 at effort `medium`, with a 200,000-token context |
| Builds | Ask from commit 03aa64bb, global answering from commit 847d8d0b |
| Cost | $28.80 for the Ask runs and $11.25 for the evaluation of global answering |

### Ask on DBpedia

The questions run on the DBpedia 2022.12.01 database of 1.01 billion quads, against
answers recomputed on that release. When the pipeline asks which resource a name means,
the harness answers from the gold query by choosing one of the offered choices. An answer
is correct when its rows match the gold's on the projected columns, and the equal-width
row gives no credit for extra columns such as a label beside an IRI.

| Set | Haiku 5.5 | Sonnet 5.5 | Opus 5.5 |
|---|---|---|---|
| QALD-9-plus, 107 questions | 16 (15%) | 35 (33%) | 40 (37%) |
| QALD-9-plus, equal width | 13 | 15 | 15 |
| DBpedia-only, 74 questions | 11 (15%) | 28 (38%) | 32 (43%) |
| DBpedia-only, equal width | 9 | 14 | 13 |
| Cost per correct answer, QALD-9-plus | $0.020 | $0.160 | $0.279 |
| Median time per question, QALD-9-plus | 11.8 s | 12.7 s | 18.6 s |

Haiku found most questions unanswerable, because the grounding seldom lists the
predicate a question needs. Sonnet and Opus guess the predicate and are often wrong.
[C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12)
gives the failures and the earlier runs on the demo set and CK25, and
[ROADMAP](ROADMAP.md) item 263 holds the follow-up.

### Global answering

Sonnet 5.5 answered 30 generated questions about the whole demo organisation graph four
ways, and Opus 5.5 judged each pair of answers in both orders on the criteria of Edge et
al. The table gives the win rate of global answering from the finest level, which holds
27 reports, against each other mode, with a tie counted as half.

| Against | Comprehensiveness | Diversity | Empowerment | Directness |
|---|---|---|---|---|
| Local Ask | 0.88 | 0.97 | 0.92 | 0.68 |
| BM25 over 20 documents | 1.00 | 1.00 | 0.98 | 1.00 |
| Global from the coarsest level | 0.62 | 0.57 | 0.55 | 0.79 |

A global answer took a median of 10.4 s against 4.7 s for local Ask. Every cited fact
exists, and the judge found 44% of the claims of fine answers fully supported and 48%
partly supported.
[C22](specs/C22-graph-rag-community-summaries.md#paid-acceptance-and-evaluation-2026-10-24)
gives the intervals and the other measures.

## Memory and disk

Sparkles uses more memory than QLever in exchange for speed, and less than Jena and
Oxigraph. The decoded-block cache, eager execution and mimalloc each trade memory for
speed, and each tradeoff is stated with its cost below.

### Server memory and index size

These are server resident sets (VmRSS, and VmHWM for peaks) and index sizes on disk from
the synthetic suite. Lower is better.

| 1.05M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| RSS 2 s after start (MiB) | 66 | 251 | 38 | 48 | **23** |
| RSS after every query ran once (MiB) | 189 | 1092 | 172 | 2291 | **75** |
| RSS after the run (MiB) | 408 | 1882 | **310** | 3077 | 1231 |
| Peak RSS during throughput (MiB) | 567 | 1889 | **339** | 3077 | 1241 |
| Index size on disk | 28 MiB | 132 MiB | **26 MiB** | 37 MiB | 147 MiB |

| 10.5M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| RSS 2 s after start (MiB) | 67 | 256 | 47 | 257 | **27** |
| RSS after every query ran once (MiB) | 668 | 3510 | 528 | — | **408** |
| RSS after the run (MiB) | **780** | 4923 | 795 | 2357 | 2074 |
| Peak RSS during throughput (MiB) | 1339 | 4930 | **939** | 2445 | 2074 |
| Index size on disk | 287 MiB | 1.3 GiB | **276 MiB** | 398 MiB | 1.4 GiB |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-18 and 2026-10-19.*

Fluree has no reading after the 10.5M answer check, because its server had been killed at
the memory limit. Of Sparkles' 408 and 780 MiB after the run, 63 and 379 MiB are the block
cache. Readings after the run vary between sessions, and four interleaved runs of two
builds in one session gave 402 to 432 MiB after the run and 545 to 582 MiB at the peak at
1.05M. At 10.5M the largest per-query rises in peak RSS are `order-by-full` at 241 MiB,
`two-hop-count` at 202 MiB, `star-join` at 175 MiB and `knows-reach` at 94 MiB, which
include cache growth as well as intermediate results.

| DBpedia, 1.24B lines | sparkles | qlever | fluree |
|---|---:|---:|---:|
| Server RSS after the run (MiB) | 4,480 | **1,426** | killed at 14 GiB |
| Index size on disk | **28.7 GiB** | 33.4 GiB | 69.7 GiB |

The DBpedia RSS figures do not measure the same memory. Sparkles maps its index, so the
clean file pages it touches count toward its RSS and the kernel can reclaim them. QLever
reads with `pread`, so the file cache does not count toward its RSS. The numeric column
adds 20.7 MB on disk and 4.3 MB of resident memory to the Sparkles index.

<a id="memory-and-the-speed-it-buys"></a>

### Memory and the speed it buys

**The decoded-block cache** keeps decompressed index blocks, up to 1 GiB per dataset by
default (`--cache-mb`). These runs measured the 10.5M suite with four cache sizes. The
first three memory columns come from a fresh-server probe that runs all 28 queries once
and then three rounds of 160 concurrent star joins.

| Block cache | Probe RSS after the queries | Probe peak RSS | Suite RSS after the run | Held by the cache | Throughput (q/s) | Sum of 28 query means |
|---|---:|---:|---:|---:|---:|---:|
| 0 MiB | 150 MiB | 614 MiB | 304 MiB | 0 MiB | 127 | 3,548 ms |
| 256 MiB | 577 MiB | 985 MiB | 713 MiB | 255 MiB | 237 | 1,056 ms |
| 1024 MiB (default) | 724 MiB | 1101 MiB | 851 MiB | 378 MiB | 234 | 1,058 ms |
| 4096 MiB | 724 MiB | 1121 MiB | 849 MiB | 378 MiB | 231 | 1,056 ms |

Turning the cache off saves 547 MiB of RSS after the run, below QLever's 653 MiB, and
costs about 46% of throughput and a 3.4× slower suite. A 256 MiB cache saves 138 MiB at
the same speed on this workload, which touches about 378 MiB of blocks. A larger cache
adds nothing here.

**The `small` memory profile** sets a 256 MiB block cache and smaller result, SERVICE and
history caches and query budgets. A prefetch request may decode up to a quarter of its
block cache in parallel, with a floor of 64 MiB. On forge, with the result cache off, the
profile's geometric means are within 1% of the default profile's warm and cold at 1.05M and
10.5M and on the DBpedia 50M slice, and its cold `star-lookup` takes 18.5 ms against
18.1 ms. After every 10.5M query ran three times the server held 709 to 720 MiB of RSS
against 775 to 794 MiB with the default profile.
[X08's Outcome](specs/X08-disk-spill-and-server-memory.md#small-profile-retune-and-the-phase-1a-control-item-97-2026-10-23)
has the runs.

**The result cache** answers repeated queries from memory, up to 512 MiB per dataset
(`--result-cache-mb`). It was off in every run on this page.

<a id="streaming-execution"></a>

**Automatic execution** is the default over HTTP, in the CLI, in Node and in the JVM
library. Eager execution materializes each operator's result within the query's row and
memory budgets, and streaming execution returns bounded batches and charges growing
operator state to the budget, while sorts and some operators still need their full input.
Automatic execution streams plain scans from an estimated thousand rows, groupings and
the FILTER EXISTS and NOT EXISTS that a key set answers from an estimated 20,000 input rows,
and a plan whose eager output would outgrow the memory the query may use while the
cursor's own state fits. It runs every other plan eagerly, because eager is faster on
several shapes at 12 threads.

Each case below is one request to a fresh server process at 10.5M, which reports the
query's allocation estimate in the native metadata and its peak RSS in `VmHWM`.

| 10.5M, one request | Eager | Automatic |
|---|---:|---:|
| Full scan, first 10 rows, allocation estimate | 241.0 MiB | **1.0 to 1.1 MiB** |
| Full scan, first 10 rows, server peak RSS | 804 MiB | **63 to 66 MiB** |
| Full scan, first 10 rows, wall time | 240 to 320 ms | **2 to 5 ms** |
| Full scan, 10.5M rows as TSV, server peak RSS | 881 to 889 MiB | **580 to 582 MiB** |
| Full scan, 10.5M rows as TSV, wall time | 5.20 to 5.69 s | 4.87 to 5.46 s |
| Name scan, 1.02M rows, allocation estimate | 16.3 MB | **5.0 MB** |
| Name scan, 1.02M rows, server peak RSS | 140 MiB | **121 MiB** |

*Measured on forge.*

A streamed answer stopped after `send` rows reports an unknown total, where the eager
answer counted all 10.5M rows. The automatic decision costs a small query nothing that
`perf stat` can see, with user-space instructions per request within −0.2% to +0.1% of an
eager-only build over 1,000 requests each of `star-lookup`, `values-star`, `employee-docs`
and `star-join` at 1.05M.

A probe binary that interleaves the modes in one process measured streaming time divided
by eager time at 10.5M, where under 1 means streaming is faster
([X05](specs/X05-streaming-query-execution.md#streaming-parity-2026-10-17)).

| 10.5M, ratio of streaming to eager | 12 threads | 1 thread |
|---|---:|---:|
| TSV, 500,000 rows | **0.86** | **0.83** |
| TSV, name scan of 1.02M rows | **0.89** | **0.91** |
| Range top-k, rows only | 1.56 | **0.94** |
| Expression ordering, rows only | 1.08 | **0.89** |
| Substring filter, rows only | 1.45 | 1.02 |
| Regular expression filter, rows only | 1.50 | 1.07 |

*Measured on forge on 2026-10-17.*

Small scans stream faster too, which is why automatic execution streams scans from an
estimated thousand rows. The scans read `foaf:name` with `LIMIT` and `ex:city`, which has
20,000 triples. The first table measures the probe binary in one process, and the second
measures automatic execution over HTTP, each line the median of three rounds of the median
wall time of 60 TSV requests from curl to a fresh server at 10.5M with one Rayon thread, the
server and client pinned to their own cores. A first run sets `SPARKLES_NO_IN_PLACE`, so
that every request is planned and runs as a first one does. A repeat lets a query that
answered in under 10 ms run eagerly in place, which a streamed answer also earns. The wall
times include a constant floor of about 1.97 ms in the client path, which hides
differences at 1,000 and 5,000 rows.

| 10.5M, TSV, ms | Eager, 12 threads | Streaming, 12 threads | Eager, 1 thread | Streaming, 1 thread |
|---|---:|---:|---:|---:|
| 1,000 triples | 0.332 | **0.259** | 0.335 | **0.260** |
| 5,000 triples | 1.604 | **1.126** | 1.610 | **1.229** |
| 50,000 triples | 15.590 | **8.057** | 15.676 | **12.034** |
| 100,000 names | 19.151 | **16.888** | 30.216 | **23.228** |

| 10.5M, TSV over HTTP, ms | First run | Repeated |
|---|---:|---:|
| 1,000 rows | 2.014 | 2.013 |
| 5,000 rows | 3.149 | 3.155 |
| 20,000 rows | 5.461 | 6.443 |
| 50,000 rows | 14.007 | 14.004 |

*Measured on forge.*

<a id="batch-kernels-and-disk-spill"></a>

#### Batch kernels and disk spill

The cursor's merge and hash joins, counts, groupings and
DISTINCT run on column kernels, and sorts, groupings, unordered DISTINCT, hash joins,
merge-join runs and a DISTINCT below its ORDER BY can spill (X08 Phase 2). Spill is
opt-in. A probe binary interleaved eager and cursor runs of each query in one process,
reading every row. The table gives the cursor's time divided by the eager time, the median
of three to five rounds, where under 1 means streaming is faster.

| Ratio of streaming to eager | 1.05M, 1 thread | 10.5M, 1 thread | 10.5M, 8 threads |
|---|---:|---:|---:|
| `expr-agg-arg`, a grouped SUM of an expression | **0.50** | **0.51** | **0.51** |
| `group-avg`, grouped AVG and COUNT then top 10 | **0.90** | **0.61** | **0.71** |
| `expr-bind-group`, a COUNT by a computed key | **0.70** | **0.66** | **0.78** |
| `x-group-out`, 2,000 groups of a join | **0.91** | **0.83** | **0.75** |
| `x-group-knows`, a COUNT per subject | **0.65** | **0.70** | **0.79** |
| `optional-count`, COUNT over OPTIONAL | **0.88** | **0.80** | **0.79** |
| `minus`, COUNT over MINUS | **0.88** | **0.82** | **0.83** |
| `x-distinct-out`, DISTINCT of a scan | **0.90** | **0.93** | **0.96** |
| `distinct-join`, DISTINCT of a join | 1.09 | **0.95** | **0.99** |
| `optional-chain`, two OPTIONAL joins | 1.00 | 1.33 | **0.92** |
| `x-hash-join`, a hash join's 1.75M rows | 1.10 | 1.02 | **0.99** |
| `x-join-out`, an index join's 1M rows | 1.28 | 1.03 | **0.98** |
| `star-join` | 1.03 | 1.09 | 1.16 |
| `regex-iri` | 1.26 | 1.26 | 1.30 |
| `order-by-full`, a sort of 1.02M rows | 1.01 | **0.69** | **0.82** |

*Measured on an 8-vCPU AMD EPYC Namespace instance on 2026-10-17.*

Every grouped query streamed faster at every size and thread count, so automatic
execution streams groupings from an estimated 20,000 input rows, counts over OPTIONAL
and MINUS included. Joins without a grouping stay eager, because they measured slower or
mixed. The cursor also holds much less. At 10.5M its ledger peaked at 2.2 MB for
`optional-count` against 19.8 MB eagerly, and 2.6 MB for `distinct-join` against
102.1 MB. A full sort stays eager by default, because the in-memory cursor sort holds its
whole input with decoded keys, 580 MB in the ledger against 31 MB eagerly.

<a id="automatic-exists"></a>

#### Groupings with EXISTS

The cursor answers a FILTER EXISTS or NOT EXISTS from a key set
of its pattern. This comparison of eager and automatic execution ran with one Rayon thread,
four A, B, B, A rounds and fresh servers per round, and both modes returned the same
answers. The table gives the median server execution time of automatic execution divided
by that of eager execution.

| Ratio of automatic to eager | 1.05M | 10.5M |
|---|---:|---:|
| `not-exists-knows`, COUNT of `knows` edges without a reverse edge | **0.91** | **0.68** |
| `not-exists`, the bench query | 0.96, eager at this size | **0.96** |
| `exists-join`, the bench query | 1.01, eager at this size | **0.98** |
| `exists-group`, a grouped COUNT over FILTER EXISTS, top 10 | 1.01 | 0.99 |
| `exists-bind`, a COUNT grouped by BIND(EXISTS …) | 1.07 | 1.15 |

*Measured on an 8-vCPU AMD EPYC Namespace instance on 2026-10-23.*

At 1.05M `not-exists` and `exists-join` filter fewer than 20,000 estimated rows and stay
eager, so their ratios compare a mode with itself. An EXISTS in BIND, in an OPTIONAL
condition or inside a larger expression is evaluated once per distinct outer key, which
was slower in the cursor, so plans with one stay eager.

Spill was measured with the cursor under a 64 MiB budget and the eager run unlimited, at
10.5M, with the spill directory on the instance's local disk.

| 10.5M, ms | Eager, unlimited | Cursor with spill, 64 MiB, 1 thread | Eager, 8 threads | Cursor with spill, 64 MiB, 8 threads |
|---|---:|---:|---:|---:|
| `order-by-full`, 1.02M rows by name | 773 | **658** | **238** | 535 |
| ORDER BY age then person, 1M rows | 1,677 | **1,294** | **298** | 563 |
| ORDER BY an expression, 1M rows | 1,809 | **1,297** | **307** | 600 |
| GROUP BY person, 833,177 groups | **127** | 195 | **124** | 205 |
| DISTINCT over a UNION, 986,333 rows | 56 | **51** | 69 | **56** |

*Measured on an 8-vCPU AMD EPYC Namespace instance on 2026-10-17.*

The spilling sorts held 45 to 54 MB in the ledger, where the in-memory cursor sort held 580
to 708 MB and the eager sort reported 31 MB. The merge runs on one thread, so a spilling
sort is faster than eager execution at one thread and about twice as slow at eight. The
grouping held 38 MB against 130 MB eagerly and took 1.5 to 1.7 times as long, and at
16 MiB it held 11 MB and took 2.8 times as long. DISTINCT held 14.5 MB against 152 MB, and
at 16 MiB 4.4 MB at 1.7 times the eager time. A budget near a tenth of the state's size
thus costs 1.7 to 2.8 times the time, and one near half of it costs little.

The same probe ran joins and an ordered DISTINCT under a budget beside the unlimited cursor
and eager execution, and checked that the spilled rows matched the eager ones.

| 10.5M, ms | Eager | Cursor, unlimited | Cursor, 16 MiB | Cursor, 4 MiB | Written at 4 MiB |
|---|---:|---:|---:|---:|---:|
| Join on `foaf:knows` under a merge join, 2.5M rows | **66** | 150 | 322 | 1,333 | 157 MB |
| OPTIONAL with a filter, 300,191 rows | **34** | 57 | 77 | 186 | 24.6 MB |
| MINUS, 90,161 rows | **21** | 36 | 59 | 165 | 24.6 MB |
| `SELECT DISTINCT … ORDER BY`, 917,941 rows | 615 | **569** | 1,421 | 3,028 | 60.1 MB |

*Measured on an 8-vCPU AMD EPYC Namespace instance on 2026-10-18, before later corrections
to join spill that bound retained output files and repaired merge and DISTINCT accounting.
These figures do not establish the cost of those corrections.*

The unlimited cursors held 61.8 to 90.8 MB for the joins and 1,016 MB for the DISTINCT,
and every spilled run stayed within its budget. A join of 29.2M rows where one key holds
nine tenths of each input took 681 ms under 1 MiB as a block nested loop, against 487 ms
for the unlimited cursor, which held 34.3 MB, and 631 ms eagerly. The `foaf:knows` join
sits under a merge join that relies on its order, so a spilled run writes its output a
second time to merge it back, which is why it slows more than the OPTIONAL and MINUS joins
at the root. The spilled joins wrote 95 to 150 MB a second. `--spill encrypted` added 1 to
6% to the bytes written and −2 to 21% to the times under 4 MiB, most of them under 10%.

<a id="parallel-cursor-filters"></a>

#### Parallel cursor filters

A cursor filter over an index scan reads the scan in morsels
of whole blocks and filters each morsel in parallel
([X05](specs/X05-streaming-query-execution.md#parallel-cursor-filters-2026-10-18)). The
table gives the cursor's time divided by the eager time over 10.5M triples, the median of
seven rounds, reading every row.

| Ratio of streaming to eager | 8 threads | 1 thread |
|---|---:|---:|
| CONTAINS on names, 49,837 rows | **0.91** | 1.00 |
| REGEX on subject IRIs, 1,000 rows | 1.08 | 1.15 |
| STRSTARTS on names, 49,837 rows | **0.94** | **0.90** |
| LANGMATCHES on titles, 500,000 rows | **0.95** | **0.88** |

*Measured on an 8-vCPU AMD EPYC Namespace instance on 2026-10-18.*

The cursor's ledger peaked at 13.5 MB for the CONTAINS filter with eight threads, against
15.6 MB eagerly, and at 6.2 MB with one thread, since each worker holds its own morsel
scratch. The cursor's range top-k reads the parts of its range without value order in
parallel morsels and keeps only its best rows
([X05](specs/X05-streaming-query-execution.md#streaming-range-top-k-2026-10-18)). Over the
salaries above 150,000 at 10.5M it takes 0.89 to 1.06 times its eager time at eight threads
and 0.88 at one, with a ledger peak of 6.6 to 7.1 MB at eight threads and 3.5 MB at one. In
native JSON at one thread, the STRSTARTS filter over names takes 1.01 to 1.03 times its
eager time and the streamed name scan of 1.02M rows 0.88 to 0.90 times
([X05](specs/X05-streaming-query-execution.md#native-json-of-a-streamed-filter-2026-10-18)).

In the JVM binding comparison at 1.05M on forge, the JNI export of 500,000 rows takes
130 ms in automatic mode against 126 ms eagerly on disk and 129 ms against 124 ms in
memory. No other query, `find`, `contains` or `getProperty` moved by more than run-to-run
noise between the modes, and the small-query latencies at one thread stayed within 2%.

**mimalloc** keeps freed memory in per-thread heaps, and the server hands idle memory back
to the OS after a second (`--idle-release-ms`). On the development laptop, a fresh 10.5M
server with mimalloc held 920 MiB after concurrent rounds against 838 MiB for glibc with
idle release, and peaked at 1,682 MiB against 1,439 MiB. In exchange it served 190 q/s
against 144, and `star-join` took 38 ms against 72 ms. The bindings make the same choice
case by case. The Python package and the JVM library use mimalloc, which raises the
Python benchmark process's peak RSS by about 20% and a mixed JVM run's from 653 to
962 MiB. The Node.js addon uses mimalloc as well, because single adds, bulk adds and
`has()` took 15 to 20% less time with it, and the process keeps more memory in exchange
([P05](specs/P05-node-bindings.md#outcome)).

The server and the bindings call mimalloc through its plain entry points for buffers
aligned to 8 bytes or less (`crates/sparkles-alloc`). The aligned entry point, which the
`mimalloc` crate uses for every request, puts a power-of-two buffer of 64 KiB or more into
the next size class, so a full decoded block column of 256 KiB would take 320 KiB. The X08
A1 gate measured the difference with ten datasets of 1.05M triples, after the suite ran
against each, three repeats each and medians below. `scripts/x08-gates/a1.sh` reproduces
it.

| `--cache-mb` | Datasets | Cache bytes | `RssAnon`, aligned entry | `RssAnon`, plain entry | VmRSS, aligned entry | VmRSS, plain entry |
|--:|--:|--:|--:|--:|--:|--:|
| 256 | 1 | 60.6 MiB | 132.5 MiB | 120.7 MiB | 195.7 MiB | 176.5 MiB |
| 256 | 10 | 379 MiB | 572.5 MiB | 492.9 MiB | 702.8 MiB | 612.8 MiB |
| 32 | 1 | 44.2 MiB | 120.7 MiB | 111.0 MiB | 184.2 MiB | 167.5 MiB |
| 32 | 10 | 160 MiB | 274.8 MiB | 264.5 MiB | 405.3 MiB | 387.3 MiB |

*Measured on forge.*

Most of the cache bytes at `--cache-mb 32` belong to the result cache, which is sized
separately and whose heap stays about 1.2 times the bytes it counts. [Roadmap](ROADMAP.md)
item 88 covers that overhead.

<a id="compression-105m-triples"></a>

### Compression of exports and backups

HTTP responses and backups can be compressed. Times are hyperfine means over 5 runs at
10.5M, including `curl`, and server CPU is the server's user and system time per request.

| Response | Encoding | Bytes | Ratio | Wall (s) | Server CPU (s) |
|---|---|---:|---:|---:|---:|
| Full N-Quads export | identity | 1,121,335,687 | 1.00 | 5.141 | 5.427 |
| | gzip-6 | 74,937,009 | 14.96 | 9.478 | 14.745 |
| | br-4 | 96,641,420 | 11.60 | 5.295 | 10.453 |
| | zstd-3 | 83,261,471 | 13.47 | 5.139 | 6.650 |
| | zstd-1 | 79,063,209 | 14.18 | 5.145 | 6.325 |
| `SELECT * … LIMIT 500000` as TSV | identity | 45,411,223 | 1.00 | 0.089 | 0.253 |
| | gzip-6 | 2,919,848 | 15.55 | 0.432 | 0.672 |
| | br-4 | 3,490,551 | 13.01 | 0.266 | 0.517 |
| | zstd-3 | 3,134,195 | 14.49 | 0.093 | 0.307 |
| | zstd-1 | 2,905,885 | 15.63 | 0.093 | 0.290 |

zstd streams the export as fast as identity at a 13–14× smaller size, while gzip takes
about 1.8× the wall time and 2.7× the server CPU. `/$/backup` writes an N-Quads dump to a
file, medians of 3 runs:

| Codec | Seconds | Size |
|---|---:|---:|
| none | 5.66 | 1,121.3 MB |
| gzip-6 | 45.11 | 74.9 MB |
| zstd-3 | 9.70 | 81.5 MB |
| brotli-5 | 17.69 | 119.0 MB |
| lz4 | 6.12 | 161.9 MB |

## Methodology and reproduction

<a id="setup"></a>

### Setup

* **Data.** `scripts/gen-data.py N` generates about 10.5 triples per person: people,
  organisations, documents, typed literals, `foaf:knows` and citation graphs,
  language-tagged titles and a small OWL class hierarchy. N = 100,000 gives 1.05M triples,
  and N = 1,000,000 gives 10.5M triples, 1.12 GB of N-Triples.
* **Queries.** `scripts/bench.sh` has 28 queries covering scans, counts, joins, OPTIONAL,
  property paths, subqueries, grouping, sorting, point lookups and expressions in BIND,
  ORDER BY and aggregate arguments.
* **Engines.** No engine may answer from a result cache.
  * Sparkles runs as `sparkles serve --result-cache-mb 0` with every other setting at its
    default.
  * Jena loads with `tdb2.tdbloader` and serves with Fuseki and `-Xmx8G`. Fuseki has no
    result cache.
  * QLever runs with `-m 8G -j 16 --cache-max-size-single-entry 0B`, which stops it from
    caching results, and its cache is cleared before every timed run.
  * Fluree 4.2.2 is the release binary, downloaded from the release archive and verified
    against a checksum. The flake's package of the same archive, which `nix/fluree.nix`
    pins by SHA-256, rewrites the Linux executable to load the glibc of the locked
    nixpkgs, so its SHA-256 in result manifests differs from the downloaded binary's. The
    published figures were not measured with the packaged executable. Fluree loads with
    `fluree create bench --from data.nt --chunk-size-mb 16` and serves with
    `fluree server run`, `FLUREE_CACHE_MAX_MB=4096` and
    `FLUREE_PATH_MAX_VISITED=20000000`, because the default traversal cap of 1M nodes
    rejects `knows-reach` at 10.5M. Fluree has no result cache.
  * Oxigraph loads with `oxigraph load` and then `oxigraph optimize`, the compaction its
    loader recommends before read-heavy workloads, and serves with
    `oxigraph serve --timeout-s 600`. The harness reads the N-Triples from standard input,
    because with `--file` Oxigraph 0.5.11 parses N-Triples in parallel chunks and turns a
    blank node label used in two chunks into two nodes.
* **Isolation.** Each engine runs alone on the machine, one after another, in a systemd
  scope with a 12 GiB memory limit and no swap, so an engine that runs out of memory is
  killed on its own. The OS page cache is warm except in the cold runs.
* **SSD state.** The SSD is trimmed with `fstrim` before each batch of timed runs, so free
  blocks do not slow the writes of loads and commits. The trim needs root, so the operator
  trims before launching a batch. The cold runs evict each engine's own files with
  `posix_fadvise`, which needs no privilege, instead of dropping the whole page cache. Runs
  dated before 2026-10-23 had only the host's weekly trim.
* **Carried-over engines.** A refresh may time Sparkles alone and keep another engine's
  results when that engine's version and binary and the host are unchanged.
  `bench-record.py build --reuse` refuses a change of either, writes the carried-over
  run's dates, host and harness under `manifest.reused` and marks each carried-over
  result. The answer checks still compare Sparkles' new answers with the other engines'.
* **Timing.** hyperfine 1.20 times SPARQL protocol POSTs with TSV results, with 2 warm-ups
  and 10 runs. Times are mean ± σ in ms and include starting `curl` and the round trip,
  about 3.5 ms. The client runs on CPU 11, the last performance core, and the servers are
  not pinned.
* **Writes.** Update latency is a single-triple `INSERT DATA` into a named graph, preceded
  by an untimed `DELETE DATA` so that each request inserts, with an ASK before and after.
  Throughput is 160 `star-join` requests from 16 unpinned `curl` clients. Memory is the
  server's RSS 2 s after the throughput run, and its peak during it.
* **Comparisons.** A difference within 10% is a tie. No figure is a significance test or
  a confidence interval.

### Answer checks

`scripts/bench-answers.py` fetches every engine's answer to every query as SPARQL JSON
before timing and fingerprints it by the row count, by the solution multiset under RDF
term identity, and by the same multiset with numbers compared by value to 12 significant
digits. An engine whose answer differs in value from the majority is marked † and not
ranked unless an independent data oracle accepts the difference. A majority is a screening
rule, not proof that its answer is correct. Unresolved disagreements remain excluded,
including the full DBpedia births-by-decade counts of 165 and 171, which have no retained
independent data check. A ‡ marks equal values returned as different RDF terms. `export-500k` uses LIMIT
without ORDER BY, so only its row count is checked.

### Engine failures

These failures of other engines were seen in the measurements and are results, not
harness errors.

* Fuseki 5.1 answers `knows-reach` with HTTP 500 after a `StackOverflowError` in its
  property path engine, at 1.05M and 10.5M. In one run at 10.5M the failure also left
  TDB2's node table cache locked, and every later Fuseki query ran into the request limit.
* Fluree 4.2.2 grows past the 12 GiB memory limit on `optional-chain` at 10.5M, on the
  loaded store and on the changed store of the `updates` mode, and the kernel kills it.
* QLever 0.5.48 keeps updates in memory. In a one-minute smoke run of the long write
  harness its RSS reached 27 GiB after about 2,300 write requests and the kernel killed
  it, so the long write runs set `SERVER_MEM_MAX`.
* QLever and Fluree were killed during SP²Bench `q4` at 10M triples, in the answer check
  that runs all five servers at once.
* QLever 0.5.48 answers a request for RDF/XML with 406, so it cannot run BSBM's Explore
  mix, whose driver asks for DESCRIBE and CONSTRUCT results in that format only. In BSBM
  smoke runs with 500 products on a development machine, QLever's server crashed with
  SIGSEGV twice while the test driver ran, once with one client in Explore and once with
  four clients in Business Intelligence. The core dump shows the fault in Boost.Asio's
  epoll reactor.

### What the benchmarks do not cover

* **Larger scale.** The largest dataset is English DBpedia at 1.24 billion triples. QLever
  is built for 10⁹–10¹¹ triples and runs Wikidata and UniProt, and its lazy evaluation,
  FSST vocabulary compression and IRI encoding matter more as data grows. Sparkles'
  FSST option was measured only up to the DBpedia 50M slice. Sparkles spills cursor sorts,
  groupings, DISTINCT and joins when spill is enabled. A query still fails when its eager
  state outgrows its budget.
* **Data larger than RAM.** Every store fits in memory except the DBpedia index of
  28.7 GiB under a 14 GiB limit.
* **Other standard benchmarks.** SP²Bench at 10M triples, LUBM(100) and BSBM Explore and
  BI at 10M have been compared on the reference machine. BI's driver cells have not been
  rerun with the current engine. The 100M BSBM and SP²Bench scales and the query
  comparisons at LUBM(1000) and OWL2Bench remain open. LDBC, WatDiv's stress testing and
  the Wikidata query log have no harness. The synthetic suite has a regular shape, and its
  28 queries are hand-picked.
* **Long runs.** Hours of sustained writes and compaction under load were not compared
  with other engines. `scripts/bench-longwrite.sh` runs that comparison and has run only
  at smoke scale.
* **Spatial and query-time reasoning.** GeoSPARQL workloads and query-time reasoning were
  not compared with other engines.
* **Result caches.** All runs had result caches off.
* **Other hardware.** Every comparison ran on one 20-thread machine with 15 GiB of usable
  RAM.

### Reproducing

[BENCHMARKING.md](BENCHMARKING.md) documents the harnesses, modes and outputs, including
local and cloud regression comparisons. The commands below reproduce the published
comparisons.

* `mise run bench [people] [workdir]` or `scripts/bench.sh` runs the synthetic suite.
  `--engines NAME` re-measures one engine and merges it into an existing run, and
  `--answers-only` checks every engine's answers without timing. `scripts/bench.sh` takes
  the same settings as `ENGINES` and `ANSWERS_ONLY=1`.
* `mise run bench:watdiv [scale]` runs WatDiv, and `scripts/bench-text.sh` the full-text
  comparison.
* `mise run bench:sp2b 10m`, `mise run bench:lubm 100` and
  `SPARKLES_ALLOW_GPL_BENCHMARK=1 mise run bench:bsbm 10m` run the standard suites. Each
  writes a result record and a run manifest that name the Sparkles commit, the engine
  versions and the data checksums, and `scripts/bench-regressions.py compare` compares two
  records. A refresh puts the figures of its records into this document, with the
  measurement dates in each table's environment line.
* `mise run bench:billion full` runs DBpedia. Run it on a quiet machine right after
  `fstrim`, because an untrimmed SSD stalls the load's writes and slows cold lookups.
  `scripts/bench-billion.sh` records the last trim, the free space and the I/O pressure,
  and warns when the trim is more than two days old or less than 30% of the file system
  is free.
* `mise run bench:bindings` runs the JVM, Python and Node.js comparison, and
  `mise run bench:jena` the ordinary Jena use cases.
* `mise run bench:backup` runs the backup measurements, and
  `scripts/geosparql-benchmark.sh` the GeoSPARQL Compliance Benchmark.
* Engines must run alone. Stop each server by its PID or port before starting the next,
  and keep builds and other benchmarks off the machine.
