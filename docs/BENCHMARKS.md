# Benchmarks

Sparkles is measured against Jena and Fuseki, QLever, Fluree and Oxigraph on query
latency, loading, writes, memory and embedded APIs. The results below summarize the
published comparisons. Each result applies to its report's data, build and timing
contract, rather than every workload or the current checkout.

## Representative results

Reference engine comparisons use an Intel Core i5-13500 with 16 GiB of RAM. Engines
run alone with result caches off and answers checked before timing. Individual reports
identify exceptions to the reference hardware, memory limits and request timing.

| Workload | Measured result | Full report |
|---|---|---|
| Synthetic queries at 10.5M triples | Sparkles has the lowest means on 27 of 28 queries and ties Fluree within 10% on `count-all`. Its median speedup over QLever is 3.8×. | [Queries and throughput](benchmarks/synthetic/RESULTS.md) |
| SP²Bench at 10M triples | Complete source-verified answers take 30.626 s for `q4` and 978.4 ms for `q6` under a 12 GiB server cap. Other hard queries retain qualified timeouts or failures. | [SP²Bench](benchmarks/sp2bench/RESULTS.md) |
| Bulk loading at 10.5M triples | Sparkles takes 3.67 s, against QLever's 9.35 s, Oxigraph's 18.22 s and TDB2's 45.70 s. | [Loading](benchmarks/loading/RESULTS.md) |
| WatDiv at 10.97M triples | Geometric mean latency is 4.63 ms, against Fluree's 6.97 ms and QLever's 17.9 ms. | [WatDiv](benchmarks/watdiv/RESULTS.md) |
| English DBpedia at 1.24B triples | Sparkles loads in 480 s, against QLever's 1,674 s and Fluree's 2,919 s. | [DBpedia](benchmarks/dbpedia/RESULTS.md) |
| LUBM over a precomputed OWL 2 RL closure of 24.3M triples | Paired `q2` means are 26.8 ms for Sparkles and 60.6 ms for QLever. These are query times after materialization. | [Reasoning](benchmarks/reasoning/RESULTS.md) |
| Serial literal inserts at 10.5M triples | Sparkles acknowledges synchronized commits in 1.167–1.254 ms, against 0.594–0.625 ms for unsynchronized Oxigraph commits. The durability contracts differ. | [Writes with reader work](benchmarks/writes/RESULTS.md#serial-writes-with-reader-work) |

## Where Sparkles loses

This table lists recorded comparisons where another engine is ahead by more than 10%.
Fluree's warm advantages on `distinct-obj` at 1.05M, `count-all` at 10.5M and two small
DBpedia 50M queries are within 10%. SP²Bench `q12b` is also within 8% of Oxigraph.

| Case | Reference advantage | Qualification |
|---|---|---|
| WatDiv `L1-1`, 10.97M | Fluree 1.11× | Sparkles has the lowest mean on 19 of 20 templates, ties Fluree on S7 and leads on 81 of 88 instances. |
| BSBM Explore `q7` and `q8`, 10M | Oxigraph 1.2× on `q7` and 1.4× on `q8` | Pooled medians cover three instances per template. Unresolved result differences are excluded from the ranking. |
| BSBM BI `q3`, 10M | QLever 1.24× | The three fixed-instance answer checks agree. A complete BI driver throughput result remains unavailable after a bounded retry. |
| SP²Bench `q11`, 10M | QLever 2.37× | Fresh isolated QLever takes 43.5 ms against retained Sparkles `6699f27d` at 103.1 ms. Complete values, ordering and row counts match. Fluree uses a different TSV dialect, so its time is not a format-matched ratio. |
| Changed-store `two-hop-count`, 1.05M | Fluree 1.10× | Sparkles takes 8.6 ms against 7.8 ms with 5,000 changes in its delta, and 5.1 ms on the loaded store. |
| Serial durable commits | Unsynced Oxigraph about 2.0× on literal inserts at 10.5M in the focused assessment | Sparkles takes 1.167–1.254 ms against 0.594–0.625 ms. The separate suite latency table gives Oxigraph a 1.84× advantage at 1.05M under its sampling contract. |
| Writes during mixed load | Oxigraph 3.4× at 1.05M in the suite and 2.1–3.0× at 10.5M in the focused assessment | At 10.5M in the focused assessment, Oxigraph completes no reads in the thirty-second window, while Sparkles completes 232–245 reads/s. Durability and completed reader work differ. |
| Cold point lookups | Oxigraph 1.5–3.0× at 1.05M and 1.3–2.9× at 10.5M | Listed under [Cold starts](benchmarks/synthetic/RESULTS.md#cold-starts). |
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

Sparkles trades memory for speed with a configurable decoded-block cache. Its default
1 GiB cache can be made smaller at a cost to query speed. The
[memory report](benchmarks/memory/RESULTS.md) separates cache, execution and index costs.

## Measurement coverage

Implementation status and measurement coverage are separate. An available capability
can have no peer comparison, and an internal regression guard does not establish a
ranking against another engine. [FEATURES](FEATURES.md#capability-status) lists complete,
partially complete and planned implementation scope.

| Area | Measurement status | Scope |
|---|---|---|
| Query, load and write comparisons | Measured with partial coverage | Published tables cover their named data, engines and timing contracts. Larger suites, sustained workloads and broader hardware remain open. |
| Bindings and model-assisted workflows | Partial coverage | The listed APIs, models and question sets have measurements. Selected API controls and internal A/B runs do not replace a complete peer-suite refresh. |
| Spatial and query-time reasoning comparisons | Unmeasured against peers | Conformance and internal measurements do not establish comparative performance. |
| Shared evidence and maintained-analysis specifications | Planned and unmeasured | C25, C26, C27, F22 and C22b are designs, with no implementation or performance claim. |

## Reading the comparisons

A difference within 10% is treated as a tie, not a significance test. Answer disagreement
is reported separately from timing and remains unranked unless independent evidence
resolves it. Majority agreement screens for differences but does not prove correctness.
Errors and timeouts are missing comparisons, not fast answers.

Coverage is partial. The standard and synthetic suites do not represent every RDF graph
or workload. Sustained writes and compaction, larger standard-suite scales and broad
hardware comparisons remain open. Spatial workloads and query-time reasoning have no
published peer comparison. The [reference methodology](benchmarks/reference/README.md)
describes the limits and answer checks.

Reports retain their own hosts, build identities, units and statistics. Full-response
HTTP timings, internal execution timings, means and medians cannot be combined into
one ranking. Reusing a peer measurement requires its recorded version and environment
to remain applicable.

## Reports and reproduction

The [workload index](benchmarks/README.md) covers all suites, their methodology pages,
results, charts and immutable run catalogs. Manually maintained results and generated
record tables are identified separately. Generated medians supplement the manual reports
and do not replace their timing contracts.

[BENCHMARKING.md](BENCHMARKING.md) documents how to run the harnesses and regression
comparisons. Use a report's recorded configuration when reproducing a result.
