# Benchmarks

Sparkles is benchmarked against Apache Jena/Fuseki, QLever, Fluree and Oxigraph across
SPARQL query latency, bulk loading, durable writes, memory use and embedded language
bindings. Reasoning and ontology classification are measured separately against Nemo, ELK,
Konclude, Whelk, HermiT and ROBOT.

Reference engine comparisons run on an Intel Core i5-13500 with 16 GiB of RAM and an NVMe
SSD. Each engine runs alone under a 12 GiB memory limit with result caches disabled, and
every engine's answers are checked before timing. Every figure below belongs to its
report's dataset, build revision and timing contract rather than every workload or the
current checkout.

## Representative results

Individual reports record any exception to the reference hardware, memory cap or timing
contract.

| Workload | Measured result | Full report |
|---|---|---|
| Synthetic queries at 10.5M triples | Sparkles records the lowest native endpoint mean on 27 of 28 queries and ties Fluree within 10% on `count-all`. Median speedup over QLever is 3.8×. | [Queries and throughput](benchmarks/synthetic/RESULTS.md) |
| SP²Bench at 10M triples | Complete source-verified answers take 30.626 s for `q4` and 978.4 ms for `q6` under a 12 GiB server cap. Other hard queries retain qualified timeouts or failures. | [SP²Bench](benchmarks/sp2bench/RESULTS.md) |
| Bulk loading at 10.5M triples | Sparkles loads in 3.67 s, compared with 9.35 s for QLever, 18.22 s for Oxigraph and 45.70 s for TDB2. | [Loading](benchmarks/loading/RESULTS.md) |
| WatDiv at 10.97M triples | Geometric mean latency is 4.63 ms, compared with 6.97 ms for Fluree and 17.9 ms for QLever. | [WatDiv](benchmarks/watdiv/RESULTS.md) |
| English DBpedia at 1.24B triples | Sparkles loads in 480 s, compared with 1,674 s for QLever and 2,919 s for Fluree. | [DBpedia](benchmarks/dbpedia/RESULTS.md) |
| LUBM over a precomputed OWL 2 RL closure of 24.3M triples | Paired `q2` means are 26.8 ms for Sparkles and 60.6 ms for QLever. These are query times after materialization. | [Reasoning](benchmarks/reasoning/RESULTS.md) |
| Serial literal inserts at 10.5M triples | Sparkles acknowledges synchronized commits in 1.167–1.254 ms, compared with 0.594–0.625 ms for unsynchronized Oxigraph commits. The durability contracts differ. | [Writes with reader work](benchmarks/writes/RESULTS.md#serial-writes-with-reader-work) |
| OWL 2 RL materialization against Nemo | On LUBM(100), Sparkles derives the 10.9M triples of the OWL 2 RL closure in 32.8 s on one thread and 21.1 s on 20, compared with 304.3 s for Nemo 0.10.1, with identical closures. Sparkles is faster on every measured input and profile and uses more memory on seven of eight. | [Against Nemo](benchmarks/reasoning/RESULTS.md#against-nemo) |
| OWL 2 EL classification of GO and ChEBI lite | On eight threads, the classifier takes 77 ms on GO and 393 ms on ChEBI lite, compared with 579 ms and 2,591 ms for ELK 0.6.0, and leads on every measured ORE 2015 ontology on one and eight threads with identical taxonomies. | [EL classification](benchmarks/el-classification/RESULTS.md#against-elk) |
| OWL 2 EL classification against Konclude, Whelk and ROBOT | On one thread, the classifier takes 0.28 s on GO and 1.4 s on a 981,151-class ORE 2015 ontology, compared with 1.3 s and 12.4 s for Konclude 0.7.0-1138. `sparkles ontology classify` runs 5.1× as fast as `robot reason` on GO end to end from RDF/XML and classifies ChEBI lite from functional syntax in 1.13 s. `sparkles ontology reason` writes the exact same ontology as `robot reason` axiom for axiom 4.1× as fast on GO. | [Reference reasoners](benchmarks/el-classification/RESULTS.md#reference-reasoners), [file classification](benchmarks/el-classification/RESULTS.md#file-classification) and [ROBOT workflows](benchmarks/el-classification/RESULTS.md#robot) |

## Where Sparkles loses

This table lists every recorded comparison where another engine leads by about 10% or
more, including paired passes where only one pass clears that threshold. Fluree's warm
advantages on `distinct-obj` at 1.05M, `count-all` at 10.5M and two small DBpedia 50M
queries are within 10%, and SP²Bench `q12b` is within 8% of Oxigraph. Fluree's 1.11× lead
on WatDiv `L1-1` in the full run did not recur in three later rounds, where the two
engines' means differed by less than a single instance's variation between rounds.

| Case | Reference advantage | Qualification |
|---|---|---|
| Serial durable commits | Unsynced Oxigraph 1.41× in one of four single-commit latency runs at 1.05M | The other three runs were within 10%, and Sparkles leads or ties on the 5,000-commit churn at both sizes. Sparkles synchronizes each commit, so latency depends on the drive's write latency. |
| Writes during mixed load | Oxigraph 5.0–5.2× at 1.05M and 2.7× at 10.5M | Oxigraph completes 12–16 reads/s at 1.05M and none at 10.5M, compared with Sparkles' 2,101–2,198 and 197. Durability and completed reader work differ. These runs predate load-based request placement, and [roadmap item 600](ROADMAP.md) re-measures them. |
| Cold `count-all` at 1.05M after a 1 s pause | Fluree 3.6× (2.8 against 10.1 ms) | Sparkles leads every cold query sent immediately after readiness. After the pause, its one vocabulary read takes about 8 ms, consistent with the drive waking from a low-power state. See [Cold against Fluree](benchmarks/synthetic/RESULTS.md#cold-against-fluree-at-105m). |
| DBpedia cold `outlink-classes-2` | QLever 1.25× | Sparkles wins 29 of 31 cold queries. |
| Server RSS after the run, 1.05M | QLever 1.32× | Without its block cache, Sparkles holds 345 MiB. Sparkles records the lowest RSS after the run at 10.5M. |
| Peak RSS under 16 readers | QLever 1.67× at 1.05M and 1.43× at 10.5M | Sparkles peaks at 567 and 1,339 MiB, compared with 339 and 939 MiB for QLever. |
| Bulk load peak RSS | QLever 2.90× on the LUBM(100) closure and 1.23× on SP²Bench; Fluree 1.26× on SP²Bench | Sparkles records the lowest peak at 1.05M and on DBpedia, and stays within 10% of QLever at 10.5M. |
| DBpedia server RSS | QLever 3.14× | Sparkles memory-maps its index, so touched file pages count toward RSS. |
| Peak RSS of OWL 2 RL and RDFS materialization | Nemo 0.10.1 1.3–9.8× on seven of eight inputs and profiles | At 20 threads on LUBM(100) under OWL 2 RL, Sparkles holds 3,713 MiB compared with Nemo's 4,230 MiB, and its runtimes are lower on every input. [Roadmap item 845](ROADMAP.md) tracks memory parity. |
| Text index size | QLever about 1.18× at 1.05M and 1.23× at 10.5M | The indexed predicates and features differ. |
| JVM small lookups | TIM 1.3–2.1× and TDB2 1.1–1.4× on `star-lookup`, `values-star` and `employee-docs` | Fixed per-query overhead through Jena dominates short lookups. |
| Python small lookups | pyoxigraph 1.49× on value lookups and `knows-reach`, and 1.23× on subject scans | The gaps are 0.67 µs per value, 20 µs per query and 2.06 µs per subject. `values-star` is within 10%, and Sparkles leads on employee and star lookups. |
| Node.js small lookups and adds | Oxigraph's package 1.1–1.4× on four small queries; N3.js 1.2–1.5× on adds and `has()` | Each add crosses to the transaction's thread. |
| Node.js `export-500k` | N3.js with Comunica 1.44× | Each row's terms cross the native boundary into JavaScript. |

Sparkles trades memory for speed through a configurable decoded-block cache. Its default
1 GiB cache (`--cache-mb`) can be lowered at a cost to query speed. The
[memory report](benchmarks/memory/RESULTS.md#memory-and-the-speed-it-buys) separates
cache, execution and index memory costs.

## Measurement coverage

Implementation status and measurement coverage are tracked separately. An available
feature can lack a peer comparison, and an internal regression guard does not establish a
ranking against another engine. [FEATURES.md](FEATURES.md#capability-status) lists
complete, partial and planned implementation scope.

| Area | Measurement status | Scope |
|---|---|---|
| Query, load and write comparisons | Measured with partial coverage | Published tables cover their named data, engines and timing contracts. Larger suites, sustained workloads and broader hardware remain open. |
| Bindings and model-assisted workflows | Partial coverage | Listed APIs, models and question sets have measurements. Selected API controls and internal A/B runs do not replace a complete peer-suite refresh. |
| Spatial and query-time reasoning comparisons | Unmeasured against peers | Conformance suites and internal measurements do not establish comparative performance. |
| OWL 2 EL classification | Measured against ELK, Konclude, Whelk, HermiT and ROBOT | [Times and memory beside ELK 0.6.0](benchmarks/el-classification/RESULTS.md#against-elk) and [beside the other reference reasoners](benchmarks/el-classification/RESULTS.md#reference-reasoners) on GO, ChEBI lite and the largest ORE 2015 EL ontologies are published. The classifier leads ELK and Konclude on every ontology and thread count measured. |
| OWL 2 RL materialization | Measured against Nemo on a benchmark worker | [Full materialization times and memory beside Nemo 0.10.1](benchmarks/reasoning/RESULTS.md#against-nemo) on LUBM(10), LUBM(100) and OWL2Bench(1) and (5) are published for `owl-rl` and `rdfs`, with closures equal to Nemo's. Sparkles leads on every input and thread count. A reference-host run remains open. |
| Evidence and maintained analysis | Unmeasured | Authenticated evidence proposals, resolution and temporal profile queries exist without peer benchmarks. Review-controlled projections and managed analysis remain planned. |

## Reading the comparisons

Differences within 10% are treated as ties rather than statistical significance tests.
Answer disagreement is reported separately from timing and remains unranked unless
independent evidence resolves it. Majority agreement screens for discrepancies but does
not prove correctness. Errors and timeouts are missing comparisons, not fast answers.

Coverage is partial. Standard and synthetic suites do not represent every RDF graph or
production workload. Sustained writes and compaction, larger standard-suite scales and
broad hardware comparisons remain open, and spatial workloads and query-time reasoning
have no published peer comparison. The [reference methodology](benchmarks/reference/README.md)
documents the limits and answer-checking rules.

Each report retains its own host, build identity, units and statistics. Full-response HTTP
timings, internal execution timings, means and medians cannot be pooled into one ranking.
Reusing a peer measurement requires its recorded version and environment to remain
applicable.

## Reports and reproduction

The [workload index](benchmarks/README.md) links to every suite's methodology page, result
report, charts and immutable run catalog. Manually maintained reports and generated record
tables are identified separately; generated medians supplement the manual reports and do
not replace their timing contracts.

[BENCHMARKING.md](BENCHMARKING.md) documents how to run the harnesses and regression
comparisons. Use a report's recorded configuration when reproducing a result.
