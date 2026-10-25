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
| Synthetic queries at 10.5M triples | Sparkles has the lowest times on all 28 queries, with a median 3.8× speedup over QLever. | [Queries and throughput](benchmarks/synthetic/RESULTS.md) |
| Bulk loading at 10.5M triples | Sparkles takes 3.67 s, against QLever's 9.35 s, Oxigraph's 18.22 s and TDB2's 45.70 s. | [Loading](benchmarks/loading/RESULTS.md) |
| WatDiv at 10.97M triples | Geometric mean latency is 4.63 ms, against Fluree's 6.97 ms and QLever's 17.9 ms. | [WatDiv](benchmarks/watdiv/RESULTS.md) |
| English DBpedia at 1.24B triples | Sparkles loads in 480 s, against QLever's 1,674 s and Fluree's 2,919 s. | [DBpedia](benchmarks/dbpedia/RESULTS.md) |
| LUBM over a precomputed OWL 2 RL closure of 24.3M triples | Paired `q2` means are 26.8 ms for Sparkles and 60.6 ms for QLever. These are query times after materialization. | [Reasoning](benchmarks/reasoning/RESULTS.md) |
| Serial literal inserts at 10.5M triples | Sparkles acknowledges synchronized commits in 1.167–1.254 ms, against 0.594–0.625 ms for unsynchronized Oxigraph commits. The durability contracts differ. | [Writes with reader work](benchmarks/writes/RESULTS.md#serial-writes-with-reader-work) |

## Remaining losses and tradeoffs

Sparkles does not lead every comparison. These examples matter when choosing settings
or deciding whether a workload resembles the published suites.

* On the DBpedia 50M slice, QLever answers `births-by-decade` about 3.0× faster warm
  and 2.5× faster cold in the complete-HTTP assessment. Sparkles takes 50.2 and
  68.1 ms, against 16.6 and 27.6 ms. The full-data internal comparison establishes
  no current peer ranking for that query.
* Small lookups and crossing a language boundary can dominate embedded workloads.
  Python value lookups take about 0.67 µs more than pyoxigraph, while Node.js
  `export-500k` takes 1.44× as long as N3.js with Comunica under the recorded contract.
* With 16 readers at 10.5M triples, peak server RSS is 1,339 MiB for Sparkles and
  939 MiB for QLever. Sparkles' default 1 GiB decoded-block cache can be made smaller.
  Lower cache memory can cost query speed.

The [complete loss table](benchmarks/overview/RESULTS.md#where-sparkles-loses) includes
cold reads, standard-suite queries, writes, bindings and memory. The
[memory report](benchmarks/memory/RESULTS.md) separates cache, execution and index costs.

## Reading the comparisons

A difference within 10% is treated as a tie, not a significance test. Answer disagreement
is reported separately from timing and remains unranked unless independent evidence
resolves it. Majority agreement screens for differences but does not prove correctness.
Errors and timeouts are missing comparisons, not fast answers.

Coverage is partial. The standard and synthetic suites do not represent every RDF graph
or workload. Sustained writes and compaction, larger standard-suite scales and broad
hardware comparisons remain open. Spatial workloads and query-time reasoning have no
published peer comparison. The [coverage summary](benchmarks/overview/RESULTS.md#measurement-coverage)
and [reference methodology](benchmarks/reference/README.md) describe the limits and
answer checks.

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
