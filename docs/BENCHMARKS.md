# Benchmarks

This page reports how fast Sparkles loads, queries and writes RDF, how much memory and
disk it uses for that speed, and how its Python, JVM and Node.js bindings compare with
the libraries they replace. The comparisons run the same data and queries on every engine,
check every engine's answers before timing them, and list every case where Sparkles loses.

| Measurement context | |
|---|---|
| Hardware | `forge`: Intel Core i5-13500 (6 performance and 8 efficiency cores, 20 threads), 16 GiB of RAM, NVMe SSD. `atlas`: the same CPU, for the bindings. A development laptop (Intel Core Ultra X7 358H, 32 GB) only where a section says so. |
| OS and kernel | NixOS, Linux 6.18 |
| Sparkles | 0.1.0, release builds with default features, rustc 1.98 and 1.99 |
| Other engines | QLever 0.5.48, Fluree 4.2.2 (the release binary), Oxigraph 0.5.11 (server, pyoxigraph and the JavaScript package), Apache Jena TDB2 6.2.0 with Fuseki 5.1.0, Jena 5.6.0 (TDB2 and TIM in the JVM comparison), rdflib 7.6.0, N3.js 2.13.11 with Comunica 5.4.1 |
| Measured | 2026-10-03 to 2026-10-09, and 2026-10-18 to 2026-10-19 for the refreshed sections named below. The QLever figures for the DBpedia `geo-box` and `country-population` queries come from an earlier run on the same host. |

The synthetic suite in all modes was measured again, and SP²Bench at 10M triples and
LUBM(100) for the first time, on `forge` from 2026-10-18 to 2026-10-19, as the first batch
of the benchmark refresh (roadmap item 1). Every engine was timed alone under a 12 GiB
memory limit. Sparkles was the
release binary of commit 81478fbe. The harness was that of commit 81478fbe for the 1.05M
runs and of commit 77032452 for the others. The summary, the synthetic tables, the cold,
churn, mixed-load, updates, load and memory figures of the synthetic suite, and the
[SP²Bench](#sp2bench-at-10m-triples) and [LUBM](#lubm100) sections come from these runs.
Other sections keep their earlier measurements. WatDiv, full-text search and the
50M-triple slice of DBpedia are being measured again, because their first runs in this
batch failed on a QLever build and on a harness that waited for its pressure sampler. BSBM
is waiting for a run with the harness fixes of 2026-10-19, and the later batches of the
refresh have not started.

* [Summary](#summary)
* [Query performance](#query-performance)
* [Loading and index builds](#loading-and-index-builds)
* [Writes and updates](#writes-and-updates)
* [Bindings](#bindings)
* [Memory and disk](#memory-and-disk)
* [Where Sparkles loses](#where-sparkles-loses)
* [Methodology and reproduction](#methodology-and-reproduction)

## Summary

The synthetic suite has 28 queries over generated data at 1.05 million and 10.5 million
triples. Medians are over the queries each engine completes.

| | 1.05M triples | 10.5M triples |
|---|---|---|
| Bulk load | **0.68 s** (QLever 1.36, Fluree 1.38, Oxigraph 4.42, TDB2 4.80) | **3.86 s** (QLever 9.35, Fluree 10.20, Oxigraph 18.22, TDB2 45.70) |
| Fastest of the five engines | 27 of 28 queries | 28 of 28 queries |
| Clear losses | none | none |
| Ties within 10% | Fluree on `count-all`, `distinct-obj`, `star-lookup` and `employee-docs`, Oxigraph on `path-plus` | Fluree on `distinct-obj` and `star-lookup`, Oxigraph on `path-plus` |
| Against QLever | Faster on 28 of 28, median 2.8× | Faster on 28 of 28, median 3.9× |
| Against Fluree | Faster on 27 of 28, median 3.3× | Faster on all 26 completed, median 10.8× |
| Against Fuseki | Faster on all 27 completed, median 14× | Faster on all 27 completed, median 79× |
| Against Oxigraph | Faster on all 28, median 24× | Faster on all 28, median 106× |
| Throughput, 16 clients | **1,204 q/s** (QLever 475, Fluree 241, Fuseki 114, Oxigraph 25) | **239 q/s** (QLever 90, Fluree 22, Fuseki 12, Oxigraph 1.7) |
| Server RSS after the run | 352 MiB (**QLever 310**, Oxigraph 1,231, Fuseki 1,882, Fluree 3,077) | **765 MiB** (QLever 795, Oxigraph 2,074, Fluree 2,357, Fuseki 4,923) |
| Server RSS less the block cache | 289 MiB | 386 MiB |

On larger and standard workloads:

* **DBpedia, 1.24 billion triples.** Sparkles loads English DBpedia in 480 s, against
  QLever's 1,674 s and Fluree's 2,919 s. It is faster than QLever on all 29 warm queries that both answer
  correctly and on 29 of 31 cold ones.
* **WatDiv at 10.97M triples.** Sparkles is fastest on all 20 templates, with a
  geometric mean of 4.84 ms against 7.41 ms for Fluree, 14.2 ms for Fuseki, 17.2 ms for
  Oxigraph and 18.2 ms for QLever.
* **Full-text search.** Sparkles is fastest on all seven queries at 1.05M and on six of
  seven at 10.5M, against Fuseki with jena-text and QLever.
* **LUBM(100).** Over the 24.3M triples of the OWL 2 RL closure, which Sparkles
  materializes in 17.3 s, Sparkles is fastest on 11 of 14 queries. QLever is 3.7× faster
  on `q2`, and Fluree has lower means on `q1` and `q3`, where Sparkles has the lower
  medians.
* **SP²Bench at 10M triples.** Sparkles answers 13 of 17 queries and is fastest on 11. Its
  default query memory budget refuses `q4`, `q5a`, `q6` and `q12a`, and Jena is 181× faster
  on `q8` and Oxigraph about 5,000× on `q12b`.
* **Durable writes.** A commit that adds a new literal takes a median of 1.13 ms at 1.05M
  and 1.28 ms at 10.5M, synchronized to disk before it is acknowledged. Oxigraph commits
  faster without synchronizing in that harness. In the suite's churn of 5,000 commits,
  Sparkles commits 852 and 913 per second at 1.05M and 10.5M, ahead of every other
  engine.
* **Bindings.** Through Jena's own API, Sparkles answers 24 of 28 suite queries faster
  than TDB2 and than Jena's in-memory store. The Python package is faster than pyoxigraph
  on 25 of 28, and the Node.js package is faster than Oxigraph's JavaScript package on 24
  of 28.

Sparkles trades memory for speed by default. Its 1 GiB decoded-block cache and eager
execution explain most of the RSS gap with QLever, and [Memory and disk](#memory-and-disk)
gives the cost and the speed of each such default.

## Query performance

These comparisons run each engine alone on `forge`, with result caches off, over HTTP.
Queries are SPARQL protocol POSTs with TSV results, timed by hyperfine around `curl`, so
each time includes about 3.5 ms of process start and round trip. Bold marks the fastest
engine. [Methodology](#methodology-and-reproduction) gives the settings of each engine.

### Synthetic suite at 1.05M triples

Most queries at this size are close to the cost of starting curl and making the request.

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| count-all | **3.6 ± 0.1** | 170.1 ± 28.1 | 12.0 ± 1.0 ‡ | 3.6 ± 0.3 | 256.4 ± 3.1 |
| types-grouped | **3.6 ± 0.2** | 79.3 ± 6.7 | 5.4 ± 0.1 ‡ | 18.0 ± 1.9 | 43.5 ± 2.2 |
| star-join | **7.8 ± 0.4** | 30.9 ± 1.4 | 32.9 ± 5.5 ‡ | 18.3 ± 0.3 | 400.6 ± 18.5 |
| two-hop-count | **5.3 ± 0.2** | 386.8 ± 0.5 | 21.6 ± 2.9 ‡ | 7.0 ± 0.2 | 836.2 ± 6.5 |
| range-topk | **4.7 ± 0.3** ‡ | 95.2 ± 1.8 | 6.2 ± 0.2 | 20.0 ± 1.0 | 59.9 ± 3.0 |
| optional-count | **4.5 ± 0.2** | 81.7 ± 2.6 | 10.0 ± 0.5 ‡ | 13.8 ± 0.1 | 105.5 ± 8.6 |
| contains | **4.3 ± 0.2** | 42.7 ± 1.8 | 74.1 ± 3.4 ‡ | 8.7 ± 0.3 | 143.9 ± 3.6 |
| group-avg | **10.0 ± 1.7** | 210.1 ± 2.7 ‡ | 16.3 ± 1.6 ‡ | 56.1 ± 1.4 ‡ | 314.9 ± 5.6 |
| path-plus | **3.4 ± 0.1** | 6.3 ± 0.7 | 6.3 ± 0.3 ‡ | 7.3 ± 0.2 | 3.7 ± 0.3 |
| distinct-obj | **3.5 ± 0.1** | 92.1 ± 2.0 | 17.6 ± 2.6 ‡ | 3.6 ± 0.1 | 70.2 ± 1.5 |
| export-500k | **76.6 ± 1.9** | 500.5 ± 16.8 | 792.4 ± 17.1 | 219.9 ± 13.8 | 1738.4 ± 26.6 |
| predicate-counts | **3.6 ± 0.2** | 336.0 ± 2.8 | 15.1 ± 0.6 ‡ | 65.5 ± 0.6 | 288.5 ± 3.4 |
| order-by-full | **48.5 ± 7.4** | 540.2 ± 5.4 | 141.2 ± 3.1 | 105.4 ± 0.5 | 3852.4 ± 59.5 |
| optional-chain | **14.2 ± 0.1** | 228.3 ± 1.7 | 67.0 ± 6.7 | 1773.6 ± 88.9 | 358.6 ± 7.4 |
| minus | **4.3 ± 1.1** | 41.4 ± 1.3 | 7.2 ± 0.2 ‡ | 9.9 ± 0.3 | 32.6 ± 1.9 |
| not-exists | **4.5 ± 0.1** | 57.8 ± 1.0 | 7.2 ± 0.4 ‡ | 16.7 ± 0.9 | 113.9 ± 8.8 |
| exists-join | **5.0 ± 0.2** | 152.0 ± 1.0 | 9.9 ± 0.3 ‡ | 30.5 ± 6.3 | 1451.6 ± 21.6 |
| subquery-agg | **3.9 ± 0.2** | 52.3 ± 1.3 | 4.9 ± 0.1 ‡ | 25.7 ± 16.5 | 27.7 ± 4.4 |
| regex-iri | **4.7 ± 0.2** | 56.6 ± 2.0 | 72.8 ± 3.8 ‡ | 77.5 ± 0.7 | 171.7 ± 7.3 |
| knows-reach | **16.3 ± 0.3** | error | 60.5 ± 6.1 ‡ | 58.2 ± 0.4 | 346.5 ± 4.0 |
| distinct-join | **8.8 ± 0.6** | 423.4 ± 1.4 | 25.9 ± 4.7 | 90.8 ± 3.5 | 679.6 ± 14.0 |
| lang-filter | **4.3 ± 0.2** | 32.3 ± 1.5 | 44.1 ± 3.8 ‡ | 6.3 ± 0.2 | 81.6 ± 6.1 |
| star-lookup | 3.8 ± 0.2 ‡ | 6.0 ± 0.4 ‡ | 10.5 ± 0.5 ‡ | **3.7 ± 0.1** | 4.7 ± 0.3 |
| values-star | **3.6 ± 0.2** | 6.5 ± 0.5 | 11.1 ± 0.7 ‡ | 4.0 ± 0.1 | 4.0 ± 0.1 |
| employee-docs | **3.7 ± 0.2** | 6.6 ± 0.2 | 6.4 ± 0.2 | 3.8 ± 0.1 | 4.3 ± 0.1 |
| expr-bind-group | **4.6 ± 0.4** | 49.8 ± 2.0 ‡ | 7.4 ± 0.2 ‡ | 45.2 ± 0.2 | 42.1 ± 4.9 |
| expr-order-key | **5.2 ± 0.2** | 93.6 ± 3.1 | 13.7 ± 1.1 ‡ | 115.0 ± 1.0 | 2660.9 ± 15.5 |
| expr-agg-arg | **8.4 ± 0.4** | 172.6 ± 1.5 | 15.5 ± 2.2 ‡ | 74.3 ± 3.2 | 300.0 ± 5.6 |
| **throughput** star-join, 16 clients (queries/s) | **1204** | 114 | 475 | 241 | 25 |

### Synthetic suite at 10.5M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| count-all | **3.5 ± 0.1** | 1855.8 ± 6.6 | 51.7 ± 3.3 ‡ | error | 2515.5 ± 18.0 |
| types-grouped | **3.5 ± 0.2** | 3722.1 ± 792.9 | 6.1 ± 0.3 ‡ | 110.6 ± 0.9 | 386.4 ± 7.7 |
| star-join | **39.7 ± 3.9** | 327.2 ± 136.4 | 118.5 ± 10.5 ‡ | 140.9 ± 3.3 | 4789.6 ± 32.7 |
| two-hop-count | **17.0 ± 0.5** | 3954.2 ± 30.2 | 66.9 ± 6.4 ‡ | 35.9 ± 0.2 | 8294.6 ± 71.1 |
| range-topk | **6.9 ± 0.9** | 882.2 ± 3.5 | 21.7 ± 3.5 | 153.1 ± 0.5 | 595.3 ± 6.6 |
| optional-count | **9.8 ± 0.5** | 1102.5 ± 129.9 | 46.2 ± 3.2 ‡ | 113.1 ± 0.7 | 997.2 ± 16.5 |
| contains | **7.1 ± 0.1** | 2669.0 ± 880.1 | 689.3 ± 3.6 ‡ | 18.0 ± 1.0 | 1537.5 ± 19.1 |
| group-avg | **47.7 ± 0.9** | 3759.1 ± 620.2 ‡ | 93.8 ± 8.5 ‡ | 547.9 ± 9.3 ‡ | 4223.8 ± 47.2 |
| path-plus | **3.4 ± 0.2** | 6.7 ± 0.8 | 9.9 ± 1.4 ‡ | 50.5 ± 0.8 | 3.7 ± 0.1 |
| distinct-obj | **3.5 ± 0.1** | 1387.0 ± 58.1 | 205.8 ± 4.9 ‡ | 3.7 ± 0.1 | 748.3 ± 3.4 |
| export-500k | **75.6 ± 0.2** | 535.7 ± 15.9 | 674.4 ± 5.4 | 289.5 ± 1.0 | 2113.7 ± 15.2 |
| predicate-counts | **3.5 ± 0.1** | 3203.1 ± 62.9 | 72.4 ± 2.9 ‡ | 597.2 ± 18.3 | 2821.0 ± 18.4 |
| order-by-full | **362.1 ± 7.5** | 16333.8 ± 370.2 | 1379.5 ± 9.7 | 1848.2 ± 54.3 | 55652.5 ± 120.9 |
| optional-chain | **85.9 ± 8.3** | 2906.7 ± 61.5 | 558.9 ± 9.4 | error | 4917.9 ± 21.7 |
| minus | **7.5 ± 0.2** | 543.8 ± 24.4 | 19.4 ± 1.9 ‡ | 52.1 ± 2.5 § | 354.0 ± 2.4 |
| not-exists | **14.7 ± 0.6** | 569.5 ± 150.3 | 25.7 ± 2.9 ‡ | 147.5 ± 1.4 § | 1042.3 ± 13.1 |
| exists-join | **15.9 ± 1.2** | 1582.9 ± 26.2 | 61.6 ± 3.6 ‡ | 263.2 ± 4.7 § | 122805.6 ± 269.9 |
| subquery-agg | **4.7 ± 0.1** | 593.1 ± 9.4 | 14.8 ± 0.7 ‡ | 140.4 ± 3.0 § | 182.8 ± 1.6 |
| regex-iri | **8.2 ± 0.1** | 4029.7 ± 119.8 | 660.3 ± 4.9 ‡ | 742.4 ± 2.4 § | 2701.9 ± 8.4 |
| knows-reach | **145.5 ± 1.4** | error | 1142.2 ± 44.2 ‡ | 639.3 ± 6.4 § | 4952.2 ± 27.4 |
| distinct-join | **48.8 ± 1.9** | 4415.9 ± 9.7 | 155.2 ± 6.6 | 1196.9 ± 10.2 § | 6666.6 ± 25.4 |
| lang-filter | **6.1 ± 0.1** | 1683.5 ± 1324.2 | 385.8 ± 1.9 ‡ | 29.3 ± 0.5 § | 855.0 ± 10.2 |
| star-lookup | **3.8 ± 0.2** ‡ | 7.1 ± 0.7 | 24.9 ± 3.2 ‡ | 4.1 ± 0.2 § | 5.1 ± 0.2 |
| values-star | **3.6 ± 0.2** | 8.0 ± 0.5 | 10.8 ± 0.6 ‡ | 4.2 ± 0.2 § | 4.0 ± 0.2 |
| employee-docs | **3.6 ± 0.1** | 7.1 ± 0.6 | 13.6 ± 1.4 | 4.4 ± 0.1 § | 4.5 ± 0.3 |
| expr-bind-group | **8.6 ± 0.5** | 421.7 ± 13.6 ‡ | 34.2 ± 4.2 ‡ | 386.3 ± 1.7 § | 352.8 ± 4.9 |
| expr-order-key | **15.9 ± 0.6** | 877.1 ± 13.1 | 108.9 ± 2.6 ‡ | 1090.7 ± 6.6 § | 38261.1 ± 72.7 |
| expr-agg-arg | **26.2 ± 0.6** | 1805.7 ± 27.6 | 92.9 ± 9.0 ‡ | 737.6 ± 6.9 § | 4056.5 ± 46.5 |
| **throughput** star-join, 16 clients (queries/s) | **239** | 12 | 90 | 22 | 1.7 |

‡ Same values as different RDF terms. QLever returns integers such as counts and ages as
`xsd:int`, where the data and the other engines use `xsd:integer`. Sparkles writes
`xsd:decimal` values in XSD 1.1 canonical form, so the data's `"175000.50"` comes back as
`"175000.5"`. In `group-avg` the averages agree to 12 significant digits.

§ Timed, but the answer was not checked, because the server had been killed earlier in the
answer check. Such a time is not ranked.

Fuseki 5.1 fails `knows-reach` (`<person/0> foaf:knows* ?x`) with a `StackOverflowError`
in its property path engine and answers HTTP 500 at both sizes. In the first attempt at
10.5M in this batch the failure also left TDB2's node table cache locked, and every later
Fuseki query ran into the request limit. That attempt was stopped, and the harness now
restarts an engine that stops answering. In the published run Fuseki went on answering
after the failure. Fluree's `optional-chain` at 10.5M grew past the 12 GiB memory limit
and the kernel killed the server, both in the answer check and in the warm-up pass before
the timings. The answer check therefore has no Fluree answers for the 14 queries after
`optional-chain`, and the harness recorded Fluree's `count-all` as an error, because the
server was no longer running when that query was timed. The harness now restarts a server
that is not running before it times a query. The harness restarted Fluree after
`count-all` and after `optional-chain`, and timed the remaining queries on fresh
servers.

### Request latency without client startup

Starting `curl` costs more than most small queries take, so this Sparkles-only run timed
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

### WatDiv

WatDiv v0.6 basic testing at scale 100 has 10,973,381 triples and 20 templates with five
instances each, except C1–C3, which have one, for 88 instances. All answers agree with
independent reference multisets. Times are geometric means over each template's five-run
instance means after one warm-up, and category means weight templates equally. Sparkles is
fastest on all 20 templates and on 86 of 88 instances. Fluree is faster on `S7-5` by 3.8%
and on `F2-1` by 0.04%, both within 10%. C3 returns 419,866 rows, and its time includes
the complete response.

| template | instances | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|---:|
| L1 | 5 | **3.85** | 6.87 | 9.77 | 4.01 | 4.70 |
| L2 | 5 | **4.09** | 12.2 | 9.39 | 5.48 | 15.5 |
| L3 | 5 | **3.83** | 6.98 | 7.11 | 4.07 | 4.59 |
| L4 | 5 | **3.81** | 8.23 | 7.06 | 4.36 | 8.28 |
| L5 | 5 | **4.02** | 11.4 | 11.1 | 4.77 | 13.7 |
| S1 | 5 | **4.32** | 7.63 | 95.7 | 4.84 | 7.06 |
| S2 | 5 | **4.22** | 7.68 | 8.07 | 4.89 | 7.02 |
| S3 | 5 | **3.97** | 10.7 | 11.0 | 5.43 | 15.6 |
| S4 | 5 | **3.86** | 13.2 | 8.00 | 5.29 | 29.9 |
| S5 | 5 | **3.88** | 8.03 | 10.2 | 4.73 | 13.8 |
| S6 | 5 | **3.77** | 7.25 | 7.64 | 4.75 | 6.56 |
| S7 | 5 | **3.78** | 6.45 | 9.67 | 3.90 | 3.93 |
| F1 | 5 | **4.05** | 8.45 | 16.2 | 4.42 | 14.4 |
| F2 | 5 | **4.05** | 7.88 | 32.3 | 4.52 | 8.67 |
| F3 | 5 | **3.98** | 7.51 | 18.5 | 28.0 | 6.17 |
| F4 | 5 | **4.59** | 7.80 | 56.3 | 26.0 | 8.94 |
| F5 | 5 | **4.26** | 7.39 | 20.2 | 4.61 | 7.47 |
| C1 | 1 | **6.89** | 16.8 | 33.9 | 8.16 | 36.5 |
| C2 | 1 | **8.35** | 140 | 54.9 | 39.7 | 369 |
| C3 | 1 | **47.92** | 10247 | 289 | 104 | 23122 |

| category | templates | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|---:|
| **linear (L)** | 5 | **3.92** | 8.86 | 8.74 | 4.51 | 8.24 |
| **star (S)** | 7 | **3.97** | 8.47 | 12.6 | 4.81 | 9.73 |
| **snowflake (F)** | 5 | **4.18** | 7.80 | 25.6 | 9.23 | 8.75 |
| **complex (C)** | 3 | **14.02** | 289 | 81.3 | 32.3 | 678 |
| **all templates** | 20 | **4.84** | 14.2 | 18.2 | 7.41 | 17.2 |

WatDiv was created by G. Aluç, O. Hartig, M. T. Özsu and K. Daudjee, “Diversified Stress
Testing of RDF Data Management Systems,” ISWC 2014.

<a id="sp2bench-at-10m-triples"></a>

### SP²Bench at 10M triples

SP²Bench v1.01 generates bibliographic data in the style of DBLP. At 10M triples its
N-Triples file is 1.49 GB. The 17 queries ran with a request limit of 600 s. The answer
check runs with all five servers up, and the servers of QLever and Fluree were killed
during `q4`, so their answers from `q5a` on were not checked (§).
Times are mean ± σ in ms over 10 runs after 2 warm-ups.

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| q1 | **3.7 ± 0.2** | 8.3 ± 0.8 | error | error | 29.6 ± 5.7 |
| q2 | **778.5 ± 10.6** | 35233.4 ± 1583.9 | 3415.2 ± 38.5 ‡ | 3275.0 ± 40.1 | 40256.7 ± 93.7 |
| q3a | **44.4 ± 5.2** | 1281.1 ± 497.7 | 252.9 ± 9.6 | 3756.5 ± 16.8 | 2868.4 ± 35.4 |
| q3b | **5.1 ± 0.1** | 515.3 ± 40.1 | 10.0 ± 0.8 | 2548.7 ± 641.6 | 2035.7 ± 15.5 |
| q3c | **4.5 ± 0.1** | 472.2 ± 10.5 | 7.4 ± 0.5 | 2739.6 ± 695.3 | 2030.6 ± 15.0 |
| q4 | error | timeout | error | error | timeout |
| q5a | error | timeout | timeout | timeout | timeout |
| q5b | **228.7 ± 6.9** | 7036.6 ± 80.5 | 653.9 ± 7.1 § | 2588.3 ± 16.9 § | 21273.6 ± 22.8 |
| q6 | error | **108648.1 ± 454.0** | 2160.0 ± 25.8 § | timeout | timeout |
| q7 | **142.5 ± 6.7** | timeout | 304.1 ± 15.5 § | 5091.6 ± 50.5 § | timeout |
| q8 | 6370.4 ± 337.8 | **35.2 ± 2.9** | error | 185.1 ± 1.9 § | 1911.6 ± 52.6 |
| q9 | **148.7 ± 13.0** | 4227.3 ± 15.1 | 284.1 ± 8.6 § | 6160.2 ± 46.5 § | 5202.0 ± 22.1 |
| q10 | **3.7 ± 0.1** | 7.3 ± 0.5 | 6.8 ± 0.6 § | 4.2 ± 0.2 § | 7.6 ± 0.9 |
| q11 | **95.7 ± 13.0** | 375.8 ± 21.4 | 42.8 ± 2.8 § | 51.7 ± 0.8 § | 31619.1 ± 97.9 |
| q12a | error | **6.2 ± 0.5** | 245.1 ± 7.3 § | 1670.3 ± 5.7 § | 4883.4 ± 46.4 |
| q12b | 20927.5 ± 746.3 | 7.3 ± 0.6 | error | 180.5 ± 2.9 § | **4.2 ± 0.2** |
| q12c | **3.4 ± 0.1** | 6.7 ± 0.2 | 4.2 ± 0.1 § | 3.4 ± 0.1 § | 3.7 ± 0.1 |

Sparkles answers 13 of the 17 queries and is fastest on 11 of them. Its default memory
profile, with a query budget of 8,192 MiB and a query pool of 5,344 MiB, refuses four
queries within 0.3 s. It answers `q4` with 503 and `q5a`, `q6` and `q12a` with 507, and its
access log gives the outcome `budget` for all four. Disk spill is off by default and does
with it on a hash join still keeps its whole build side ([roadmap](ROADMAP.md) items 7
and 85). No engine finished `q5a`
within 600 s. On `q6`, Jena's checked answer took 108.6 s and QLever's unchecked one 2.2 s.

Two queries are clear losses. Sparkles takes 6.4 s on `q8`, where Jena takes 35 ms, and
20.9 s on `q12b`, the ASK form of `q8`, where Oxigraph takes 4.2 ms and Jena 7.3 ms. Its
access log counts 555,607,405 rows produced inside the `q12b` plan for an answer of one
row. On `q11` the unchecked times of QLever and Fluree, 42.8 and 51.7 ms, are lower than
Sparkles' 95.7 ms.

Sparkles loads the data in 4.12 s, against 8.90 s for Fluree, 9.70 s for QLever, 20.49 s
for Oxigraph and 44.88 s for TDB2. Its index takes 307 MiB on disk, against QLever's
294 MiB, and its load peaks at 2,264 MiB, against Fluree's 1,835 MiB and QLever's
1,879 MiB.

<a id="lubm100"></a>

### LUBM(100)

LUBM's generator, UBA 1.7, made 13,876,262 triples for 100 universities. Sparkles'
reasoner materialized the OWL 2 RL closure, 10,907,029 derived triples, in 17.3 s at a
peak RSS of 3,837 MiB, and every engine then loaded and queried the closure of 24,312,703
triples, so no query needs reasoning at query time. All 14 queries returned the same
answers on all five engines. Times are mean ± σ in ms over 10 runs after 2 warm-ups.

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| q1 | 4.3 ± 2.7 | 7.5 ± 0.8 | 7.0 ± 0.7 | **3.6 ± 0.2** | 959.2 ± 7.0 |
| q2 | 257.9 ± 3.5 | 1274.3 ± 27.3 | **68.8 ± 6.2** | 2856.2 ± 25.9 | 4295.6 ± 19.5 |
| q3 | 4.5 ± 2.8 | 6.9 ± 0.5 | 8.4 ± 1.0 | **3.7 ± 0.2** | 2388.5 ± 28.5 |
| q4 | **3.7 ± 0.1** | 7.4 ± 0.3 | 11.7 ± 1.3 | 4.0 ± 0.1 | 286.4 ± 20.1 |
| q5 | **4.7 ± 2.4** | 8.5 ± 0.5 | 7.5 ± 0.5 | 6.8 ± 0.2 | 3166.5 ± 17.2 |
| q6 | **73.8 ± 1.0** | 944.7 ± 56.6 | 647.1 ± 4.0 | 407.6 ± 9.7 | 1701.9 ± 11.3 |
| q7 | **3.7 ± 0.1** | 6.6 ± 0.4 | 11.9 ± 0.7 | 3.9 ± 0.1 | 4.8 ± 0.2 |
| q8 | **9.8 ± 0.1** | 67.9 ± 18.3 | 153.5 ± 7.2 | 33.3 ± 1.1 | 9527.7 ± 33.4 |
| q9 | **64.0 ± 6.0** | 4446.1 ± 82.9 | 221.6 ± 9.2 | 7273.9 ± 50.2 | 17660.2 ± 29.5 |
| q10 | **3.5 ± 0.1** | 6.0 ± 0.6 | 6.7 ± 0.2 | 4.3 ± 1.4 | 2975.5 ± 14.7 |
| q11 | **3.9 ± 0.6** | 7.4 ± 0.5 | 5.5 ± 0.3 | 4.6 ± 0.2 | 147.4 ± 6.2 |
| q12 | **3.7 ± 0.1** | 7.9 ± 0.4 | 7.8 ± 0.5 | 5.9 ± 0.2 | 30.5 ± 1.9 |
| q13 | **4.3 ± 0.2** | 8.4 ± 0.5 | 11.3 ± 0.6 | 5.9 ± 0.2 | 7.1 ± 1.2 |
| q14 | **58.0 ± 0.4** | 561.3 ± 64.9 | 494.5 ± 4.8 | 328.6 ± 12.2 | 1370.7 ± 29.2 |

Sparkles is fastest on 11 of the 14 queries. QLever is 3.7× faster on `q2`, at 68.8 ms
against 257.9 ms. Fluree's means are lower on `q1` and `q3`, but Sparkles' medians are
lower, 3.4 and 3.5 ms against 3.6 and 3.8 ms, and its means include slow runs with a p95
of 11.9 and 12.3 ms. In the cold runs, the median of 3 restarts with the engine's files
evicted, Sparkles is fastest on all 14 queries and answers 0.09 s after a restart.

Sparkles loads the closure in 8.04 s, against 18.96 s for QLever, 20.03 s for Fluree,
48.23 s for Oxigraph and 60.38 s for TDB2, into the smallest index, 236 MiB. Its load
peaks at 5,096 MiB, the highest after TDB2's 6,299 MiB, against QLever's 1,778 MiB. The
RDFS closure took 11.4 s and derived 5,527,675 triples.

### DBpedia at 1.24 billion triples

This comparison runs English DBpedia, release 2022.12.01, against QLever and Fluree. The 38 files of
`scripts/bench-billion/dbpedia-2022.12.tsv` hold 1,237,418,025 lines of N-Triples, 16 GB
compressed with zstd, which are 1,014,683,275 distinct triples. Sparkles' vocabulary has
205,941,262 terms. The run uses `scripts/bench-billion.sh` with `SCALE=full`, `RUNS=3`,
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

### Cold starts

Cold mode restarts the server and evicts its files from the OS page cache before each
query, three times per query, and reports the median request time without process
startup. Eviction does not guarantee that the device itself is cold. A Sparkles server is
ready 0.085 s after a restart at 1.05M and 0.087 s at 10.5M. Sparkles is fastest on 21 of
28 cold queries at 1.05M and on 24 of 28 at 10.5M. The cold losses are these:

| Query, cold | Faster engine | 1.05M | 10.5M |
|---|---|---|---|
| `count-all` | Fluree at 1.05M | 1.26× (2.7 against 2.2 ms) | — |
| `contains` | Fluree at 1.05M | 1.09× (13.1 against 12.1 ms) | — |
| `path-plus` | Oxigraph | 2.93× (6.5 against 2.2 ms) | 2.66× (4.3 against 1.6 ms) |
| `distinct-obj` | Fluree at 1.05M | 2.64× (4.2 against 1.6 ms) | — |
| `lang-filter` | Fluree at 1.05M | 1.06× (11.6 against 10.9 ms) | — |
| `star-lookup` | Oxigraph at 10.5M | — | 1.31× (20.2 against 15.4 ms) |
| `values-star` | Oxigraph | 1.52× (8.1 against 5.3 ms) | 1.44× (7.8 against 5.4 ms) |
| `employee-docs` | Fluree at 1.05M, Oxigraph at 10.5M | 1.40× (11.8 against 8.5 ms) | 1.03× (14.1 against 13.7 ms) |

Sparse point lookups and several small scans lose because warm execution hides their
first-use reads and block decoding. Sparkles memory-maps its index and vocabulary, and three features limit what a
cold lookup reads:

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

#### Cold reads after X07

The tables above were measured before the cold-read work of [X07](specs/X07-cold-sparse-reads.md),
which adds one read window per scan pass, batched hints for a query's constants and
first-scan prefetch. The figures below were measured on a Namespace instance (AMD EPYC,
8 vCPU, 16 GB, Linux 7.1.4), not on the host of the tables above, which is about 1.5 to
2 times slower on these queries. Each is the median of 11 fresh-server requests with the
page cache dropped, through HTTP with TSV output, for the base commit and the build with
the changes. They show the change from the work and are not comparable with the other
engines' figures.

| Cold, Namespace, median ms | 1.05M base | 1.05M new | 10.5M base | 10.5M new | 105M base | 105M new |
|---|---:|---:|---:|---:|---:|---:|
| `star-lookup` | 7.3 | 7.9 | 33.1 | 25.0 | 21.9 | 14.4 |
| `employee-docs` | 6.4 | 6.2 | 23.9 | 23.3 | 20.2 | 13.0 |
| `values-star` | 4.9 | 4.3 | 17.1 | 11.6 | 5.0 | 4.3 |
| `path-plus` | 3.6 | 3.7 | 2.7 | 2.5 | 1,381 | 1,383 |
| `contains` | 6.9 | 6.4 | 15.6 | 17.0 | 88.9 | 91.7 |
| `export-500k` | 147 | 150 | 173 | 184 | 179 | 207 |

Another run of the same instance, with a quieter host, measured 10.5M `star-lookup` at
24.6 ms for the base and 14.5 ms for the new build. The noise of the table above is larger
than that, so the ratios are better evidence than the milliseconds. A repeat of
`export-500k` on that Namespace instance with 21 runs gave 167.6 and 178.1 ms at 105M.
The repeat with `prefetch_constants`, `prefetch_first_scan`, `prefetch_terms` and
`parallel_decode` disabled showed the same slowdown while `ReadAhead` and the vocabulary
window changes remained active. The ablation was incomplete, and the cause remains
unresolved.

Roadmap item 4 and [X07 §9](specs/X07-cold-sparse-reads.md#9-targets), including A5,
retain the follow-up on quiet forge. After correctness review and integration, compare
the corrected default with the approved baseline. Repeat that comparison with the
candidate's four flags disabled and `SPARKLES_READ_WINDOW_KB=0`. Window zero retains the
original 1 MiB vocabulary window. The cold targets, such as `star-lookup` at 10.5M within
17.3 ms, still require that host's measurements.

These measurements predate the cancellation, decoded-cache budget and advised-coverage
repairs. The corrected read-ahead path counts existing outstanding bytes and coalesced
gaps, allows one next-block extension and restores advice after cache eviction. Its
warm and cold measurements and no-profile overhead gate remain pending on integrated
source. Vocabulary profiling also observes mapped offsets before loading them and
accounts for ordinary and fallible bulk decodes independently of advice. These
correctness tests establish no new speed result or counter-cost measurement.

### Full-text search

The comparison uses Sparkles' Tantivy-backed `text:query`, Fuseki's Lucene-backed
jena-text and QLever's `ql:contains-word`. Sparkles and Jena index `foaf:name`,
`ex:title` and `rdfs:label`, with English stemming of titles. QLever indexes all literals
and joins on predicates, searches the indexed form for the stemmed case, and does not
highlight. Hit sets and counts agree, while scores and the order of tied hits are not
compared. Times are TSV HTTP means ± standard deviation in ms, over ten runs at 1.05M and
five at 10.5M.

| query | sparkles 1.05M | jena-fuseki 1.05M | qlever 1.05M | sparkles 10.5M | jena-fuseki 10.5M | qlever 10.5M |
|---|---:|---:|---:|---:|---:|---:|
| rare-top10 | **4.0 ± 0.2** | 12.7 ± 1.5 | 6.7 ± 0.4 | **5.8 ± 0.4** | 15.0 ± 1.9 | 7.8 ± 0.5 |
| common-top10 | **4.2 ± 0.2** | 11.7 ± 1.5 | 7.8 ± 0.6 | **8.1 ± 0.3** | 14.9 ± 2.8 | 17.5 ± 2.7 |
| common-count | **4.7 ± 0.2** | 53.9 ± 3.4 | 7.2 ± 0.7 | 14.8 ± 0.6 | 406.9 ± 4.3 | **13.5 ± 1.1** |
| text-join | **5.4 ± 0.2** | 66.0 ± 1.5 | 11.3 ± 0.8 | **21.4 ± 0.6** | 585.4 ± 5.1 | 46.6 ± 7.6 |
| conjunction | **4.2 ± 0.1** | 15.0 ± 1.5 | 7.0 ± 0.3 | **8.2 ± 0.2** | 80.7 ± 0.8 | 14.3 ± 1.0 |
| highlight | **11.4 ± 0.2** | 101.8 ± 8.7 | 13.7 ± 1.3 | **70.6 ± 0.5** | 841.8 ± 46.2 | 94.8 ± 6.5 |
| stemmed | **4.0 ± 0.2** | 12.2 ± 0.6 | 5.9 ± 0.3 | **5.4 ± 0.3** | 53.5 ± 0.9 | 18.6 ± 2.6 |

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

### Vector search

This Sparkles-only run searches 100,000 clustered 384-dimensional vectors by cosine
distance. Each configuration answered 1,000 top-10 queries after 20 warm-ups over a
persistent HTTP connection with full TSV responses. Recall is measured against Sparkles'
exact search on the same vectors, and no other vector engine was measured.

| Search | Recall@10 against exact | Mean (ms) | p50 (ms) | p99 (ms) |
|---|---:|---:|---:|---:|
| Exact | 1.0000 | 5.39 | 5.44 | 5.99 |
| HNSW `ef=16` | 0.9903 | 0.45 | 0.52 | 0.60 |
| HNSW `ef=32` | 0.9994 | 0.72 | 0.72 | 0.86 |
| HNSW `ef=64` | 0.9994 | 0.46 | 0.35 | 0.94 |
| HNSW `ef=128` | 0.9994 | 0.54 | 0.54 | 0.65 |
| HNSW `ef=256` | 0.9994 | 0.92 | 0.91 | 1.37 |

The index was built by the 2026-10-13 build, which builds the graph over a half-precision
copy of the vectors. The previous build reached 0.9921 recall at `ef=16` and 0.9989 from
`ef=32` up. Its `ef=128` mean was 0.66 ms and its `ef=256` mean 1.14 ms, so the new graph
answers the higher settings about 18 % faster.

Means below about 0.7 ms are not comparable between runs on this host. The same binary
answered the 1,000 `ef=16` queries in 0.29 ms at the median in one pass and 0.60 ms in the
next, and cheap settings were often slower than `ef=64`. Both the old and the new build
showed this, so it comes from the client and the processor's idle states rather than from
the search.

These figures describe this synthetic distribution, not all vectors.

The vector maintenance work was also screened on a Namespace instance (8 AMD EPYC vCPUs)
with the interleaved A/B harness and the 27-query SPARQL suite over 100,000 people. These
are Namespace numbers, not forge numbers. Both binaries returned the same answers, and the
geometric mean of server execution time was 0.962 of the previous build's, which is within
the harness's noise.

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

### Graph algorithm polling

Roadmap item 8 moved the cancellation and deadline checks of the graph algorithms
inside every long loop and charged every workspace to the call. This A/B measures what
that costs. Both builds ran the ignored `polling_cost` test of
`tests/graph_algorithms_scale.rs` on forge, an Intel i5-13500 with nothing else running,
pinned to four cores. The build before is 824b7238 with the same test file. The graph is
random with 1,000,000 nodes and 5,000,000 edges, and each call ran under a far deadline,
so every check reads the clock. The builds ran in eight ABBA pairs at one and four
threads. Each run is the fastest of three calls, and the table gives the median of the
eight runs of each build, with the slowest and fastest run.

| algorithm | threads | before | range | after | range | change |
|---|---:|---:|---|---:|---|---:|
| PageRank, 20 iterations | 1 | 1956.7 | 1920.0–2022.5 | 505.5 | 488.0–523.7 | -74.2% |
| Weakly connected components | 1 | 145.7 | 144.8–153.0 | 142.8 | 140.8–145.2 | -2.0% |
| Strongly connected components | 1 | 236.2 | 214.9–246.0 | 219.9 | 211.6–265.1 | -6.9% |
| Breadth-first search | 1 | 137.9 | 122.2–141.2 | 117.5 | 111.1–128.0 | -14.8% |
| Label propagation | 1 | 694.8 | 693.3–700.6 | 704.4 | 700.1–711.5 | +1.4% |
| Triangle count | 1 | 489.3 | 467.9–493.1 | 494.0 | 469.4–500.0 | +1.0% |
| K-core | 1 | 252.6 | 250.8–255.6 | 251.8 | 224.5–252.9 | -0.4% |
| FastRP | 1 | 2300.4 | 2298.8–2302.8 | 2310.0 | 2123.9–2313.3 | +0.4% |
| PageRank, 20 iterations | 4 | 543.2 | 533.6–552.6 | 161.6 | 153.0–167.3 | -70.2% |
| Weakly connected components | 4 | 40.8 | 39.5–41.2 | 40.2 | 38.9–40.5 | -1.3% |
| Strongly connected components | 4 | 238.7 | 223.2–260.2 | 221.6 | 189.3–245.9 | -7.2% |
| Breadth-first search | 4 | 39.2 | 37.7–41.9 | 35.3 | 33.9–36.8 | -10.1% |
| Label propagation | 4 | 182.2 | 181.6–182.8 | 183.4 | 182.5–184.8 | +0.7% |
| Triangle count | 4 | 239.9 | 234.9–257.5 | 241.5 | 237.0–250.1 | +0.6% |
| K-core | 4 | 252.9 | 251.4–253.5 | 252.3 | 251.4–254.5 | -0.2% |
| FastRP | 4 | 997.9 | 994.8–1004.5 | 1009.4 | 1003.0–1015.1 | +1.1% |

Times are in milliseconds. PageRank is 3.4 to 3.9 times faster because it read the
clock before every node, and now counts its rows and checks once per 64K units. A first
build that counted every row and every merge cost triangle counting 5 to 7.5% and the
connected components about 5% on one thread. The build measured here counts a block of
rows at once while it holds at most 64K units, keeps the check out of the inner loops,
and lets the search of strongly connected components scan a node's edges in one inner
loop, which also makes that search and breadth-first search faster than before. A
repeat of eight pairs for label propagation and FastRP measured +0.6% and +0.9% for
label propagation and +0.6% and +0.4% for FastRP at one and four threads, so their
changes in the table are within the noise of the measurement.

The rows for strongly connected components, breadth-first search and triangle counting
come from a later run of eight pairs against the same build before. In that build the
search of strongly connected components also checks every 64K edges within one row,
and breadth-first search charges the room of the next frontier one row at a time again
instead of one block of rows at a time. The triangle rows changed by +1.0% and +0.6%
against -0.2% and +0.3% in the first run, with ranges that overlap, after a change that
only made one bound saturate. Breadth-first search charges at most 9.1 MB at its peak on
this graph, against 8.0 MB in the build before, which charged the depths and the output
but not the frontiers. A charge per block of rows had raised the peak to 14.5 MB from
1.09 MB on a graph of 100,000 nodes of degree 50, and a unit test now keeps it under 16
bytes a node there. The graph is uniform random, so it has no node of very high degree.
The checks inside a long row are covered by the cancellation tests and by the 64K split
of strongly connected components, and this measurement does not time them.

### Spatial queries

This Sparkles-only run uses the geo fixture with 100,000 points, its polygons and 180,640
indexed geometry literals. The server returns the same coordinate-normalized results
with the spatial index on and off. Each query had one warm-up and five curl and hyperfine
samples, so the times include process startup. The index builds in 0.54 s and takes
13.8 MiB.

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

The graph algorithm work (F14) first put every query behind a memory ledger, which added a
fixed cost to queries that never call an algorithm. A plain query now charges a lock-free
fast account and creates the ledger only when it calls an algorithm, keeps an output lease
or moves to a cursor. The runs used forge with the 1.05M triple dataset and the small
queries of the suite, where a fixed cost per query is the largest share of the time.

| Comparison | Measure | Result |
|---|---|---|
| Lazy ledger against the build before it | Instructions per HTTP request on the small queries | 6 to 9 thousand fewer |
| Lazy ledger against the build before it | Median execution time of distinct-obj | 3.75 µs down to 3.19 µs |
| Lazy ledger against origin/main at 20f01c82 | Execution time of the three smallest queries | 0.5 to 0.7 µs higher |
| Lazy ledger against origin/main at 20f01c82 | Wall time of the four flagged small queries | 0.7 to 3.3% higher |

The baseline is origin/main at 20f01c82, the release before F14. The remaining 0.5 to 0.7
µs on the smallest queries is spread over the operator result plumbing. The fast account
adds a mutex, four atomic counters and a lazily set grant to each query context and saves
the ledger allocation of a plain query, so a plain query does not use more memory than it
did before.

On values-star, main at fb8d56cf ran 20.4 thousand instructions per request more than
20f01c82. The F14 merge 073e0663 added about 11 thousand, and the lazy ledger brought that
down to about 4.4 thousand. The X07 merge d370602e added 14.6 thousand to warm queries with
constants. About 6 thousand of it was the constant prefetch, which allocated a key and
searched the sparse vocabulary index for every constant of every query, and about 6.5
thousand was the block hints. The planner's counts looked each warm end block up twice,
and the vocabulary checked for an I/O profile at every step of a lookup. The prefetch now
remembers the hashes of keys whose group it has asked for, a warm count takes its end
blocks from the cache once, and a lookup checks for a profile once. These changes recover
about 7 thousand instructions. Each vocabulary with a sparse index keeps 8 KiB more for the
table of key hashes, which saves the search of every warm constant. The table holds 1,024
keys, so it helps queries whose constants repeat and does nothing for a stream of new
ones. The runs used forge with 2,000 requests per server process for instructions and 8
rounds of 50 requests for times.

| Comparison | Measure | Result |
|---|---|---|
| Warm X07 paths against main at 824b7238 | Instructions per request of values-star | 1,005.6 thousand down to 998.7 thousand |
| Warm X07 paths against main at 824b7238 | Instructions per request of star-lookup and employee-docs | 5 to 7 thousand fewer |
| Warm X07 paths against main at 824b7238 | Median wall time of the seven small queries | 0.990 to 1.003 of main |
| Warm X07 paths against origin/main at 20f01c82 | Instructions per request of values-star | 1.4% higher |
| Warm X07 paths against main at 824b7238 | Instructions per request of 10,000 distinct entity lookups, 1 and 8 clients | 1.000 and 0.998 of main |
| Warm X07 paths against main at 824b7238 | Instructions per request of a VALUES list of 4,096 IRIs, 1 and 8 clients | 0.989 and 0.990 of main |
| Warm X07 paths against main at 824b7238 | Median wall time of those four workloads | 1.005 to 1.018 of main |

The rest of the gap to 20f01c82 on values-star is the fast memory account of F14, about
4 thousand instructions on every query, and the warm checks that the X07 hints need. These
are a hash per constant, a bit test per answer term and one cache lookup per scan.

### The cost of the query memory pool

The query memory pool of [X08](specs/X08-disk-spill-and-server-memory.md#gates) is on by
default, and its latency gate asks that it add at most 2% or 0.5 µs to each query under
0.3 ms. The run used forge, an Intel Core i5-13500, with the 1.05M triple dataset. One
build ran with the pool on and with `--total-query-memory-mb 0` from one shared store, in
both orders at 20 rounds of 300 requests with one Rayon thread, and the two orders were
combined to cancel the bias of the second position. `scripts/x08-gates/latency.sh`
reproduces it. The build charges the pool in steps of 64 KiB, so a plain query can hold up
to 64 KiB of the pool beyond its estimate.

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

Every query passes. Repeated runs of the same comparison differed by up to 4% on queries
under 15 µs, and `path-plus` reads faster with the pool on, so ratios of these queries
below 1.05 are within the run-to-run spread. Before the 64 KiB step, a small query grew
its grant three or four times, and a loop in one process put the grant's share of the
execution time at 3.8% on `count-all` and 6.6% on `distinct-obj`, against 1.3% and 1.4%
with the step. The Namespace screen of the step and the allocator change below against
5b2085bc gave a geometric mean of 0.978 over 27 queries, and no query read above 1.062.

### Planning large basic graph patterns

[Roadmap](ROADMAP.md) item 84 made the plan of a basic graph pattern near the 4000-level
plan limit cheap to build. These are measurements on the development laptop in a release
build, and other builds shared the machine, so the times are good to about a fifth. They
are not forge numbers. The ignored test `large_bgp_planning_time` in
`crates/sparkles/tests/nesting.rs` ran `explain` on each shape of `plan_shapes` at the
limit over a dataset of four triples, and the star and hub rows also ran on a build of
the commit before the change. [X05](specs/X05-streaming-query-execution.md#large-basic-graph-patterns)
has the design and the shapes.

| Shape at the limit | `explain` before | `explain` after | Plan JSON before | Plan JSON after |
|---|---|---|---|---|
| Chain of 1998 | 1.08 s | 0.35 s | 53.7 MB | 29.7 MB |
| Reversed chain of 1998 | 1.01 s | 0.27 s | 64.5 MB | 32.5 MB |
| Tree of 1998 | 2.24 s | 0.34 s | 37.4 MB | 22.4 MB |
| Star of 1998 | 3.35 s | 0.73 s | 23.1 MB | 15.1 MB |
| Hub of 1998 | 0.98 s | 0.77 s | 53.8 MB | 29.8 MB |
| Pairs of 999 | 1.36 s | 0.49 s | 34.1 MB | 18.1 MB |
| Chain of 999 with a second predicate | 0.67 s | 0.19 s | 26.2 MB | 11.2 MB |
| Chain of 571 with VALUES | 0.43 s | 0.11 s | 15.9 MB | 7.3 MB |
| Chain of 1019 with filters | 71 ms | 34 ms | 1.3 MB | 1.3 MB |
| Chain of 799 with OPTIONAL | 6.49 s | 0.53 s | 23.3 MB | 13.7 MB |
| Chain of 799 with MINUS | 2.49 s | 0.15 s | 8.6 MB | 4.8 MB |
| Chain of 799 with BIND | 5.88 s | 0.18 s | 21.2 MB | 13.5 MB |

A star of 1998 patterns on `foaf:knows` over the 31,815 triples of
`scripts/gen-data.py 3000` took 2.9 to 3.0 s to plan before and 2.6 to 2.8 s after, so
the data decides how much a star gains. The change keeps the star masks and probed sides
with each plan, which costs 2.6 to 3.2% more planning instructions for a snowflake of
12 patterns and up to 64 MB while the greedy order runs on a plan of 1998 inputs.

### Schema reports and Ask on DBpedia

Roadmap item 89 measured the MCP tools and `sparkles ask` on the DBpedia 2022.12.01
database of [BENCHMARKING](BENCHMARKING.md), 1.01 billion quads, on atlas with a warm page
cache, through `sparkles mcp`. These figures come from atlas, not forge, and from one
session each, so they show the size of the change rather than a published comparison.

| Measurement | Before item 89 | After item 89 |
|---|---|---|
| First `check_query` of a process | 52.7 s, 11.1 GiB | 0.3 s, or up to 1.5 s right after the binary changed |
| First `describe_schema` summary of 200 predicates | a full report, as for `check_query` | 1.4 s, and 11 ms repeated |
| Terms-list query of `check_query` with VALUES outside a UNION | failed after 1.94 s at 5.5 GiB | 0.04 s |
| `sparkles ask` with the harness's fake provider | about 100 s, most failing on the 2 GiB budget | 1.9 s, at most 508 MiB of anonymous memory |

Later `check_query` calls in the same process took 20 to 60 ms. The process held under
500 MiB of anonymous memory after the change. The mapped index pages that the sampling of
predicate objects touches add about 2 GiB of reclaimable file-backed memory to the
resident size. The figures are for a database without changes since its build. A rerun after the
review fixes of item 89, interleaved with the earlier build in two rounds each, gave the
same times: a warm first `check_query` in 0.27 to 0.30 s, later ones in 16 to 20 ms, and
the summary in 1.32 to 1.35 s, with 296 to 392 MiB of anonymous memory for both builds. A
store with changes corrects the statistics with one index probe for each changed value,
and the report scans instead when that would cost more than a scan.

## Loading and index builds

Sparkles builds its index in one parallel pass. Parsing and vocabulary merging run on all
cores, sampled keys divide the partial vocabularies into ranges that merge in parallel,
and each 64-million-quad chunk is remapped once, sorted by SPO, OSP and PSO and written as
compressed runs. SOP, OPS and POS derive from those streams by sorting runs of the first
column, and without named graphs GSPO derives from SPO. Each permutation has its own
writer, so the input is read once.

### Bulk loading

Load times include each engine's own preparation, such as `oxigraph optimize`, and peak
RSS comes from GNU time. Each load ran once. The synthetic, SP²Bench and LUBM loads are
those of the refresh of 2026-10-18 to 2026-10-19. Oxigraph now reads N-Triples from
standard input, which keeps blank node labels whole and takes about three times as long as
its parallel `--file` loader did in the earlier figures.

| Load | sparkles | jena (tdb2.tdbloader) | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M triples, time | **0.68 s** | 4.80 s | 1.36 s | 1.38 s | 4.42 s |
| 1.05M triples, peak RSS | **458 MiB** | 2,166 MiB | 629 MiB | 479 MiB | 826 MiB |
| 10.5M triples, time | **3.86 s** | 45.70 s | 9.35 s | 10.20 s | 18.22 s |
| 10.5M triples, peak RSS | 2,023 MiB | 4,433 MiB | **1,929 MiB** | 3,007 MiB | 2,184 MiB |
| SP²Bench, 10M triples, time | **4.12 s** | 44.88 s | 9.70 s | 8.90 s | 20.49 s |
| LUBM(100) closure, 24.3M triples, time | **8.04 s** | 60.38 s | 18.96 s | 20.03 s | 48.23 s |
| WatDiv, 10.97M triples, time | **4.34 s** | 33.61 s | 9.71 s | 9.46 s | 13.16 s |
| DBpedia, 1.24B lines, time | **480 s** | — | 1,674 s | 2,919 s | — |
| DBpedia, peak RSS | **6,059 MiB** | — | 11,401 MiB | 12,366 MiB | — |

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
| Full-text index, 1.05M triples | **0.41 s** (Fuseki 4.37, QLever 1.86) | 13.1 MB (QLever **11.1**, Fuseki 24.9) |
| Full-text index, 10.5M triples | **6.48 s** (Fuseki 25.98, QLever 19.35) | 122.5 MB (QLever **99.9**, Fuseki 227.8) |
| Full-text document store, every string literal of 10.5M (1,540,012 documents) | 6.17 s with zstd, 6.07 s with LZ4 | 114.1 MB with zstd (the default), 117.3 MB with LZ4 |
| HNSW vector index, 100,000 × 384 dimensions, `M=16`, `efConstruction=128` | 6.43 s (14.02 s before the half-precision build) | 163 MB of mapped files, 1,044.4 MB peak RSS |
| HNSW vector index, 1,000,000 × 384 dimensions, in-process example | 63.5 s, down from 139.5 s. USearch 2.26.2 in half precision takes 50.0 s. | Recall@10 is 0.963 at `ef=128`. Peak RSS is 12.6 GB, up from 10.2 GB. |
| Spatial index, 180,640 geometries | 0.54 s | 13.8 MiB |
| RDFS materialization, 1,001,418 triples to 2,751,996 with inferences | 3.974 s, of which inference takes 2.323 s | — |
| Numeric column added to the DBpedia index by `sparkles vocab-index` | 4.8 s | 20.7 MB |

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
(spec C18 §7.1.1). This is a development measurement on a shared 16-core Linux laptop,
not a quiet-machine benchmark. Each figure is over 200 conversions of the 5 KB DOCX
fixture with the release binary, made by the ignored test `ingest::isolate::tests::overhead`.

| 5 KB DOCX | Median | 90th percentile |
|---|---:|---:|
| In process | 41 µs | 54 µs |
| On a thread, the fallback path | 85 µs | 184 µs |
| In a child process | 8.5 ms | 9.5 ms |

Starting `/bin/true` took 2 to 4 ms on the same machine, so most of the child's cost is
starting a process. The child adds about 10 ms to each document, which is small next to
the model calls of an ingestion. Each running conversion may also use up to
`--convert-memory`, 2 GiB by default, of address space beside the server.

## Writes and updates

Every Sparkles write is durable when it is acknowledged: the WAL record and any new
vocabulary entries are synchronized first. The other engines use their defaults. TDB2
commits durably, QLever keeps updates in memory only, Fluree uses its default commit
path, and Oxigraph commits a RocksDB transaction that writes its WAL without
synchronizing it. Of these comparisons, only
the one with TDB2 is between two commits that are both synchronized.

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
DELETE requests, one at a time, to every engine's store. No engine failed a commit. These
figures come from the refresh of 2026-10-18 to 2026-10-19, in which Sparkles committed
faster than Oxigraph at both sizes. In the earlier run Oxigraph was ahead, at 855 against
770 commits per second at 1.05M and 1,140 against 663 at 10.5M.

| Churn of 5,000 commits | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: commits/s | **852** | 16 | 423 | 259 | 771 |
| 1.05M: latency p50 / p99 (ms) | 1.26 / **2.05** | 70.93 / 88.19 | 2.27 / 5.78 | 3.17 / 25.22 | **1.25** / 2.07 |
| 10.5M: commits/s | **913** | 26 | 387 | 361 | 715 |
| 10.5M: latency p50 / p99 (ms) | **1.03** / 3.35 | 19.75 / 90.27 | 2.48 / 6.30 | 1.69 / 6.46 | 1.35 / **2.14** |

The WAL preallocates space in steps from 64 KiB up to 4 MiB (`--wal-prealloc-kb`, `0`
disables it). Replay, readers, backups and quotas use the logical end of the last commit,
and recovery recognizes a torn tail.

### Concurrent durable commits

These runs measured group commit, now the default, against the writer that
`--no-group-commit` keeps, on `forge` on 2026-10-17, with the same binary at revision
758a6a25. Group commit was then opt-in under the flag `--experimental-group-commit`, which
the switch to the default removed, so the columns named default below are the
`--no-group-commit` writer. `forge` has an Intel Core i5-13500 and an NVMe drive with ext4. The server ran on six CPUs and the client on four, and nothing else ran on the
host. Six rounds alternated the two settings, each run in a fresh process on a fresh copy
of the store. Each cell is the median over the rounds. Every run completed with distinct,
contiguous receipts and the correct final state. No commit of any run fell back from the
group.

| 50,000 changes, transactions/s | 1.05M default | 1.05M group commit | 10.5M default | 10.5M group commit |
|---|---:|---:|---:|---:|
| Four writers | 3,399.8 | **6,173.8** | 3,375.6 | **6,160.0** |
| Sixteen writers | 3,342.8 | **18,119.2** | 3,332.0 | **18,304.4** |

| Request latency (ms) | 1.05M default | 1.05M group commit | 10.5M default | 10.5M group commit |
|---|---:|---:|---:|---:|
| Four writers, p50 / p99 | 1.159 / 1.383 | **0.631 / 0.954** | 1.168 / 1.381 | **0.633 / 0.952** |
| Sixteen writers, p50 / p99 | 4.738 / 5.867 | **0.813 / 1.467** | 4.752 / 6.083 | **0.793 / 1.445** |

With sixteen writers, no writer's share of the commits differed from the mean by more than
1.2% with group commit, and by 0.3% with the default writer.

A short confirmation on 2026-10-17 ran the switched build at revision 8bea2951, where group
commit is the default, against the same build with `--no-group-commit`, at 1.05M with three
rounds. It repeated serial parity, the four- and sixteen-writer throughput and the writers'
shares at 1.05M, and not the request latencies or the 10.5M column. Serial literal inserts
took 0.422 / 0.456 ms at p50 /
p90 with grouping and 0.424 / 0.459 ms without. Four writers reached 6,139.6 transactions/s
against 3,392.8, and sixteen writers 18,211.3 against 3,341.5, with no share more than 1.1%
from the mean. An earlier attempt right after the `log-writes` crash runs, which wrote and
deleted several GiB on the same drive, was about four times slower in both settings, and
its numbers were not kept.

A single writer gains nothing from grouping, so the idle fast path commits it as the
default writer does. These serial runs send 5,000 single-triple commits.

| Serial commits, p50 / p90 (ms) | 1.05M default | 1.05M group commit | 10.5M default | 10.5M group commit |
|---|---:|---:|---:|---:|
| Literal insert | 0.426 / 0.461 | 0.427 / 0.459 | 0.429 / 0.463 | 0.421 / 0.458 |
| IRI insert | 0.426 / 0.458 | 0.425 / 0.459 | 0.422 / 0.460 | 0.425 / 0.464 |
| Delete | 0.425 / 0.459 | 0.430 / 0.459 | 0.423 / 0.456 | 0.425 / 0.456 |

The write modes of `scripts/bench.sh` ran on the same host with four rounds per setting.
The churn applies the 5,000 commits of the `updates` mode one at a time. The mixed load
runs 16 `star-join` readers and one writer for 30 s, with the server on six CPUs.

| bench.sh write modes | 1.05M default | 1.05M group commit | 10.5M default | 10.5M group commit |
|---|---:|---:|---:|---:|
| Churn, commits/s | 1,045.3 | 1,047.7 | 1,000.3 | 1,018.7 |
| Mixed, reads/s | 994.3 | 1,009.6 | 101.4 | 104.8 |
| Mixed, writes/s | 127.9 | 130.8 | 128.0 | 130.3 |

An earlier measurement on the same host, before direct writes into preallocated space,
had 221.5 to 231.8 transactions/s for the default writer and 529.0 to 543.3 for group
commit with four writers at 1.05M.

### Group commit on Namespace instances

These runs are approximate screens of group commit, not the published numbers above. They
ran on ephemeral Namespace instances (8 vCPUs, ext4, AMD EPYC) with `scripts/nsc-writes.sh`,
which alternates the variants in one instance, the server on six CPUs and the client on two.
The instances are shared, so ranges are wide, and a quiet run is the exception. The
dataset is the 10.5M-triple one (1,000,000 people). The default writer is `off`. The
group commit variant was then `--experimental-group-commit` and is now the default. It
includes the idle fast path:
a commit that finds no other writer waiting and no fence running synchronizes as the default
writer does.

| Serial commits, 10.5M, 8 rounds of 5,000 | Default | Group commit | Ratio |
|---|---:|---:|---:|
| Literal insert, p50 / p90 (ms) | 0.183 / 0.218 | 0.184 / 0.223 | 1.005 / 1.021 |
| IRI insert, p50 / p90 (ms) | 0.177 / 0.217 | 0.178 / 0.222 | 1.003 / 1.023 |

The serial literal, IRI and commit-message throughput figures from these runs are
withdrawn. The client excluded warmups from latency samples but included them in the
elapsed denominator used for serial transactions per second. Delete seeding was already
untimed. The retained latency rows excluded warmups and remain provisional Namespace evidence.
Corrected serial throughput requires new measurements.

An earlier serial run on a noisier instance had group commit 6.5% above at p50 and 19% above at
p90 for literal inserts, with a spread of 3.7× between the fastest and slowest default-writer
runs. Without the idle fast path, group commit was 4% to 9% slower than the default writer
in serial runs at 1.05M, which is why the path exists.

The instances sync in about 0.2 ms, which hides the cost of a slow device. The runs below add
4 ms to every synchronization and write-through write (`delay-us=4000` in
`scripts/nsc-writes.sh`), which reproduces the default writer's 230 transactions/s on
`forge`. Four rounds each.

| 10.5M, 4 ms synchronization | Default | Group commit | Ratio |
|---|---:|---:|---:|
| Four writers with two readers, transactions/s | 233 | 466 | 2.0 |
| Four writers with two readers, writer p50 (ms) | 17.1 | 8.5 | 0.50 |
| Sixteen writers, transactions/s | 229 | 1,783 | 7.8 |
| Four writers, commit message on every update, transactions/s | 112.8 | 429.0 | 3.8 |
| Sixteen writers, commit message on every update, transactions/s | 112.7 | 1,636.6 | 14.5 |

A commit message costs the default writer a second synchronization, for the annotation
file. The concurrent message rows show the cost of that additional synchronization.
Group commit synchronizes both files in one fence. In the
sixteen-writer message run, the largest deviation of a writer's share from the mean was 0.002 for
group commit and 0.063 for the default writer.

The reader p99 at 10.5M with four writers and two readers was measured on `forge` in the
next section. The 40% regression above did not reproduce on these instances. With the fast device (80,000
changes) p99 was 5.20 ms for the default writer and 4.16 ms for group commit. With the 4 ms
synchronization it was 0.85, 0.89 and 0.73 ms for the default writer, group commit without
the idle path and group commit with it, and in a later run 0.91 and 1.09 ms for the default
writer and group commit, with ranges of 0.66 to 2.56 and 0.73 to 1.87 ms. These overlap, so
the tail needed `forge` to settle.

### Group commit on indexed datasets

These screens compare origin/main 23075691, where a dataset with a full-text, spatial or
embedding index takes the ordinary path, with the [X09 Phase 4](specs/X09-group-commit-by-default.md#phase-4)
branch at 80f19430 with grouping on and with `--no-group-commit`. They ran on 2026-10-18
with `scripts/nsc-writes.sh --index` on 8-vCPU Namespace instances, as in the previous
section, so they are approximate. The dataset is the 10.5M-triple one with the index built
after the load, and 4 ms are added to every synchronization (`delay-us=4000`). The
full-text index covers every literal, so each write adds a document. The embedding index
embeds the writers' literals, and the server runs with `--no-embedding`, so its work only
queues. The generated data has no geometries and the writers add none, so the spatial rows
measure admission alone. Each row is the median of four rounds of transactions per second.
Before the change every commit of these runs fell back with reason `text`, `geo` or
`embed`, and after it none fell back.

| 10.5M, 4 ms synchronization | Index | origin/main | Grouping off | Grouping on | On / origin/main |
|---|---|---:|---:|---:|---:|
| Serial literal inserts | Full-text | 194.5 | 210.6 | 209.0 | 1.07 |
| Four writers | Full-text | 203.2 | 215.2 | 441.4 | 2.17 |
| Sixteen writers | Full-text | 202.0 | 215.6 | 1,718.0 | 8.51 |
| Four writers with two readers | Full-text | 203.8 | 218.5 | 439.1 | 2.15 |
| Serial literal inserts | Spatial | 213.5 | 214.4 | 207.5 | 0.97 |
| Four writers | Spatial | 230.6 | 230.7 | 466.6 | 2.02 |
| Sixteen writers | Spatial | 229.8 | 230.9 | 1,805.6 | 7.86 |
| Serial literal inserts | Embedding | 217.8 | 218.8 | 220.3 | 1.01 |
| Four writers | Embedding | 232.9 | 232.5 | 469.6 | 2.02 |
| Sixteen writers | Embedding | 233.9 | 232.1 | 1,802.3 | 7.71 |

The serial p50 and p90 with grouping on were within 3.2% of grouping off for every index.
The four-writer p50 fell from about 17 ms to 8.5 ms, and the sixteen-writer p50 from about
64 ms to 8.8 ms. With the full-text index, the reader p99 of the mixed run was 1.9 ms with
grouping on and 1.1 ms with it off, with ranges of 0.9 to 2.9 ms and 1.0 to 1.3 ms. The
Phase 3 reader tail did not settle on these instances either, so this needs `forge`.

On the instances' own synchronization of about 0.1 ms, five rounds with the full-text index
gave 14,488 transactions per second for sixteen writers with grouping on against 9,146 with
it off, and 9,611 against 9,560 for four writers. Serial inserts commit in about 0.12 ms
there, and ten interleaved rounds of 5,000 at 1.05M gave a median per-round ratio of p50 on
to p50 off of 1.035, with single rounds from 0.86 to 1.58. That is within the noise of
these instances, and the serial gate of X09 section 17.9 needs `forge`.

The servers' peak resident memory, read from `VmHWM` after each run, measures what the
speculative index state costs. With four writers grouping on peaked within 5 MiB of
grouping off for every index. With sixteen writers it peaked 15 MiB higher with the
full-text index, 18 MiB higher with the embedding index and 21 MiB higher with the spatial
index, at 210, 188 and 174 MiB.

### Reader tail of group commit on forge

Four writers and two readers ran against the 10.5M dataset on `forge` with its NVMe device,
six rounds per setting, on 2026-10-16. In the paced run both settings were held to 3,452
transactions/s, the most the default writer reaches. The paced run is the X09 acceptance
gate, and the unpaced run shows the read cost of the higher write rate that group commit
allows.

| Measure | Unpaced, default | Unpaced, group commit | Ratio | Paced, default | Paced, group commit | Ratio |
|---|--:|--:|--:|--:|--:|--:|
| Transactions/s | 3,452.5 | 5,995.8 | 1.737 | 3,426.7 | 3,449.7 | 1.007 |
| Writer p50 (ms) | 1.126 | 0.634 | 0.563 | 1.119 | 0.712 | 0.636 |
| Reader p50 (ms) | 0.482 | 0.332 | 0.689 | 0.478 | 0.496 | 1.037 |
| Reader p99 (ms) | 4.639 | 5.039 | 1.086 | 3.980 | 3.986 | 1.002 |

### Concurrent writers with snapshot readers

Four writers insert 5,000 fresh quads while two readers each issue up to 25 queries per
second. Each read counts the inserted namespace and checks the count against the commit
snapshot named in the response. The state, the history and the reopened store are checked
afterwards.

| Four writers and two readers | Default | Group commit |
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
opens a new connection.

| Mixed load | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: reads/s | **2482.1** | 80.6 | 522.9 | 36.5 | 15.0 |
| 1.05M: read p50 / p99 (ms) | **6.1 / 10.9** | 193.8 / 376.2 | 26.9 / 36.3 | 423.0 / 739.7 | 1014.2 / 1855.0 |
| 1.05M: writes/s | 371.7 | 15.2 | 97.1 | 249.2 | **1272.6** |
| 1.05M: write p50 / p99 (ms) | 2.3 / 6.5 | 67.9 / 86.6 | 6.2 / 30.5 | 3.8 / 14.2 | **0.8 / 1.0** |
| 1.05M: failed reads / writes | 0 / 0 | 0 / 0 | 30034 / 1046 | 0 / 0 | 0 / 0 |
| 10.5M: reads/s | **241.5** | 5.7 | 74.7 | 3.4 | 0 |
| 10.5M: read p50 / p99 (ms) | **65.0 / 87.2** | 2704.1 / 3588.4 | 212.1 / 257.6 | 4284.9 / 6931.1 | — |
| 10.5M: writes/s | 198.4 | 18.1 | 47.5 | 237.8 | **565.1** |
| 10.5M: write p50 / p99 (ms) | 4.8 / 9.3 | 67.3 / 87.7 | 10.7 / 133.6 | 3.8 / 16.8 | **1.6 / 3.9** |
| 10.5M: failed reads / writes | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |

Sparkles serves 4.7× QLever's reads at 1.05M and 3.2× at 10.5M. Oxigraph commits more
writes at both sizes, and Fluree does at 10.5M, while both serve far fewer reads. At 10.5M
no Oxigraph reader finished within 30 s. A separate run with the durable-commit harness
above, with 16 readers on the 10.5M store, completed 299 new-literal commits per second at
a median of 3.2 ms.

### Queries after updates

The `updates` mode applies the same 5,000 single-triple changes to each engine's store and
runs the query suite again. The changes stay in Sparkles' delta until a compaction folds
them into the base index. At 10.5M the delta changes `two-hop-count` from 17.0 to 35.6 ms,
`contains` from 7.1 to 15.0 ms and `regex-iri` from 8.2 to 17.9 ms, leaves `star-join` at
39.7 and 38.7 ms, and changes throughput from 239 to 203 q/s, against QLever's 85 q/s on
its changed store. On the changed stores Sparkles is fastest on 27 of 28 queries at both
sizes. QLever's changed-store `types-grouped` answer differs in value and is not ranked.
Fluree is ahead on `two-hop-count` by 1.07× at 1.05M and 1.02× at 10.5M, within 10%, and
there are no clear losses. Fluree's 10.5M run on its changed store failed the same way as
its run on the loaded store, with `optional-chain` killed at the memory limit and
`count-all` recorded as an error.

<a id="automatic-compaction-105m-triples"></a>

### Automatic compaction

This test sends 50,000 single-triple commits, 35,000 inserts and 15,000 deletes, to the
1.05M store, each as its own HTTP request. It ran on the development laptop with other
builds running, at a load average of about 38 on 16 cores, so the tails are noisy.

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

A release build served the 10.5M data on the development laptop, which other builds kept
at a load average of 35 to 85 on 16 cores. Each figure is one request's `time_total`,
except the queries, which are medians of 15 interleaved requests.

| Step | Time |
|---|---:|
| Create a branch of `main` at its head (four runs) | 19.5–32.7 ms |
| `star-join` on `main` | 38.3 ms |
| `star-join` on the new branch | 38.7 ms |
| Preview a merge of 10,000 inserted quads into a `main` with 10,000 of its own | 61.9 ms |
| That merge, committed | 114.9 ms |
| First request to the branch after a restart | 3.2 ms |

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
The figures below are development measurements on 8-vCPU AMD EPYC instances of a cloud CI
provider, not quiet-machine benchmarks, and runs on different instances differed by up to
30%. They use the same 105,000 triples, A/B/B/A order in fresh processes pinned to six
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

## Memory and disk

Sparkles uses more memory than QLever in exchange for speed, and less than Jena and
Oxigraph. The decoded-block cache, eager execution and mimalloc each trade memory for
speed, and each tradeoff is stated with its cost below.

### Server memory and index size

These are server resident sets (VmRSS, and VmHWM for peaks) and index sizes on disk from
the synthetic suite. Lower is better.

| 1.05M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| RSS 2 s after start (MiB) | 67 | 251 | 38 | 48 | **23** |
| RSS after every query ran once (MiB) | 196 | 1092 | 172 | 2291 | **75** |
| RSS after the run (MiB) | 352 | 1882 | **310** | 3077 | 1231 |
| Peak RSS during throughput (MiB) | 487 | 1889 | **339** | 3077 | 1241 |
| Index size on disk | 28 MiB | 132 MiB | **26 MiB** | 37 MiB | 147 MiB |

| 10.5M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| RSS 2 s after start (MiB) | 60 | 256 | 47 | 257 | **27** |
| RSS after every query ran once (MiB) | 651 | 3510 | 528 | — | **408** |
| RSS after the run (MiB) | **765** | 4923 | 795 | 2357 | 2074 |
| Peak RSS during throughput (MiB) | 1261 | 4930 | **939** | 2445 | 2074 |
| Index size on disk | 287 MiB | 1.3 GiB | **276 MiB** | 398 MiB | 1.4 GiB |

These figures come from the refresh of 2026-10-18 to 2026-10-19. Fluree has no reading
after the 10.5M answer check, because its server had been killed at the memory limit. Of
Sparkles' 352 and 765 MiB after the run, 63 and 379 MiB are the block cache. At 10.5M
the largest per-query rises in peak RSS are `order-by-full` at 241 MiB, `two-hop-count`
at 202 MiB, `star-join` at 175 MiB and `knows-reach` at 94 MiB, which include cache
growth as well as intermediate results.

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

**The result cache** answers repeated queries from memory, up to 512 MiB per dataset
(`--result-cache-mb`). It was off in every run on this page.

<a id="streaming-execution"></a>

**Eager execution** materializes each operator's result within the query's row and memory
budgets. Opt-in streaming execution returns bounded batches and charges growing operator
state to the budget, while sorts and some operators still need their full input. These
direct Rust measurements on the development laptop, medians of warm medians from fresh
processes at 10.5M, show the tradeoff:

| 10.5M, ms | Eager | Streaming |
|---|---:|---:|
| Name scan | 1.507 | **0.536** |
| Grouped average | **40.002** | 40.363 |
| OPTIONAL count | 5.162 | **4.501** |
| Range TopK | **10.135** | 15.662 |
| Substring filter | **8.538** | 9.798 |
| Full ordering | 207.640 | **160.071** |
| Expression ordering | **13.013** | 16.426 |
| Native JSON, 500,000 rows | 527.604 | **516.189** |
| TSV, 500,000 rows | **89.868** | 123.520 |

For the complete scan, the query's allocation estimate peaks at 241.0 MiB for eager
result columns and 1.0 MiB when streaming. Automatic execution is the default over HTTP,
in the CLI, in Node and in the JVM library. It streams plain scans and, since the column
kernels described [below](#batch-kernels-and-disk-spill), groupings of an estimated 20,000
input rows or more, where streaming measured faster, and runs every other plan eagerly,
because eager is faster on several shapes at 12 threads. The OPTIONAL count above ran
eagerly for a few hours on 2026-10-17, because the cursor's branching merge count took
3.5 ms against 2.9 ms eagerly on the AMD EPYC hosts of the Namespace screens. Its
branch-free count streams again.

The table below measures the change of default on forge at 10.5M. Each case is one
request to a fresh server process, which reports the query's allocation estimate in the
native metadata and its peak RSS in `VmHWM`. The baseline is 6f0f928c, whose default
was eager, and the candidate runs the same scan by default and with `execution=eager`.

| 10.5M, one request | Eager | Automatic |
|---|---:|---:|
| Full scan, first 10 rows, allocation estimate | 241.0 MiB | **1.0 to 1.1 MiB** |
| Full scan, first 10 rows, server peak RSS | 804 MiB | **63 to 66 MiB** |
| Full scan, first 10 rows, wall time | 240 to 320 ms | **2 to 5 ms** |
| Full scan, 10.5M rows as TSV, server peak RSS | 881 to 889 MiB | **580 to 582 MiB** |
| Full scan, 10.5M rows as TSV, wall time | 5.20 to 5.69 s | 4.87 to 5.46 s |
| Name scan, 1.02M rows, allocation estimate | 16.3 MB | **5.0 MB** |
| Name scan, 1.02M rows, server peak RSS | 140 MiB | **121 MiB** |

A streamed answer stopped after `send` rows reports an unknown total, where the eager
answer counted all 10.5M rows. The automatic decision costs a small query nothing that
`perf stat` can see. Over 1000 requests each of `star-lookup`, `values-star`,
`employee-docs` and `star-join` at 1.05M, with a unique comment so that no request takes
the in-place path, the server's user-space instructions per request moved by -0.2% to
+0.1% against the baseline.

Interleaved A/B screens on forge compared 210f52fc with 6f0f928c at 1.05M and 10.5M
with the bench queries and a full scan. No query's median server or wall time regressed
by 10% or more. At 10.5M, the server time of the name scan fell by 72%, because the
cursor's batches no longer wait for the whole answer, and its wall time for all 1.02M
rows in native JSON stayed at 553 ms. At 1.05M the full scan's server time fell by 81%
with the same wall time. The geometric mean of the server times fell by 6.3% at 1.05M and
5.2% at 10.5M, and that of the wall times moved by -0.7% and +0.1%.

The streaming parity work of 2026-10-17 measured eager against streaming execution again
on forge with a probe binary that interleaves the modes in one process. The table gives
the streaming time divided by the eager time at 10.5M, where under 1 means streaming is
faster. Each row's modes ran in the same process, and
[X05](specs/X05-streaming-query-execution.md#streaming-parity-2026-10-17) describes the
changes.

| 10.5M, ratio of streaming to eager | af4a79d6, 12 threads | 2026-10-17, 12 threads | 2026-10-17, 1 thread |
|---|---:|---:|---:|
| TSV, 500,000 rows | 1.80 | **0.86** | **0.83** |
| TSV, name scan of 1.02M rows | 1.37 | **0.89** | **0.91** |
| Range top-k, rows only | 2.06 | 1.56 | **0.94** |
| Expression ordering, rows only | 1.48 | 1.08 | **0.89** |
| Substring filter, rows only | 1.60 | 1.45 | 1.02 |
| Regular expression filter, rows only | 1.56 | 1.50 | 1.07 |

Small scans stream faster too, which is why automatic execution now streams scans from an
estimated thousand rows.

The threshold was also measured over HTTP, against af4a79d6, whose automatic execution ran
these scans eagerly. Each line is the median of three rounds, each the median wall time of
60 TSV requests from curl to a fresh server at 10.5M with one Rayon thread, the server and
client pinned to their own cores. A first run is the server with `SPARKLES_NO_IN_PLACE`,
so that every request is planned and runs as a first one does. A repeat lets a query that
answered in under 10 ms run eagerly in place, which a streamed answer now also earns. The
wall times on forge include a constant floor of about 1.97 ms in its client path, which
hides differences at 1,000 and 5,000 rows.

| 10.5M, TSV over HTTP, ms | forge af4a79d6 | forge candidate | Namespace af4a79d6 | Namespace candidate |
|---|---:|---:|---:|---:|
| 1,000 rows, first run | 2.012 | 2.014 | 0.447 | 0.467 |
| 5,000 rows, first run | 3.148 | 3.149 | 1.239 | 1.218 |
| 20,000 rows, first run | 6.454 | **5.461** | 3.617 | 3.569 |
| 50,000 rows, first run | 15.086 | **14.007** | 10.230 | 10.050 |
| 1,000 rows, repeated | 2.015 | 2.013 | 0.453 | 0.461 |
| 5,000 rows, repeated | 3.146 | 3.155 | 1.238 | 1.238 |
| 20,000 rows, repeated | 6.433 | 6.443 | 3.522 | 3.637 |
| 50,000 rows, repeated | 15.968 | **14.004** | 9.896 | 10.067 |

The scans read `foaf:name` with `LIMIT` and `ex:city`, which has 20,000 triples. Both
builds returned the same rows.

| 10.5M, TSV, ms | Eager, 12 threads | Streaming, 12 threads | Eager, 1 thread | Streaming, 1 thread |
|---|---:|---:|---:|---:|
| 1,000 triples | 0.332 | **0.259** | 0.335 | **0.260** |
| 5,000 triples | 1.604 | **1.126** | 1.610 | **1.229** |
| 50,000 triples | 15.590 | **8.057** | 15.676 | **12.034** |
| 100,000 names | 19.151 | **16.888** | 30.216 | **23.228** |

<a id="batch-kernels-and-disk-spill"></a>

**Batch kernels and disk spill.** On 2026-10-17 the cursor's merge and hash joins, counts,
groupings and DISTINCT moved to column kernels, and sorts, groupings and unordered
DISTINCT learned to spill (X08 Phase 2). A probe binary interleaved eager and cursor runs
of each query in one process over a store loaded on an 8-vCPU AMD EPYC Namespace instance,
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

Every grouped query streamed faster at every size and thread count, so automatic
execution now streams groupings from an estimated 20,000 input rows, counts over OPTIONAL
and MINUS included. Joins without a grouping stay eager, because they measured slower or
mixed. The cursor also holds much less. At 10.5M its ledger peaked at 2.2 MB for
`optional-count` against 19.8 MB eagerly, and 2.6 MB for `distinct-join` against
102.1 MB. A full sort stays eager by default, because the in-memory cursor sort holds its
whole input with decoded keys, 580 MB in the ledger against 31 MB eagerly.

Spill was measured with the cursor under a 64 MiB budget and the eager run unlimited, at
10.5M. The spill directory was on the instance's local disk.

| 10.5M, ms | Eager, unlimited | Cursor with spill, 64 MiB, 1 thread | Eager, 8 threads | Cursor with spill, 64 MiB, 8 threads |
|---|---:|---:|---:|---:|
| `order-by-full`, 1.02M rows by name | 773 | **658** | **238** | 535 |
| ORDER BY age then person, 1M rows | 1,677 | **1,294** | **298** | 563 |
| ORDER BY an expression, 1M rows | 1,809 | **1,297** | **307** | 600 |
| GROUP BY person, 833,177 groups | **127** | 195 | **124** | 205 |
| DISTINCT over a UNION, 986,333 rows | 56 | **51** | 69 | **56** |

The spilling sorts held 45 to 54 MB in the ledger, where the in-memory cursor sort held 580
to 708 MB and the eager sort reported 31 MB. The merge runs on one thread, so a spilling
sort is faster than eager execution at one thread and about twice as slow at eight. The
grouping held 38 MB against 130 MB eagerly and took 1.5 to 1.7 times as long, and at
16 MiB it held 11 MB and took 2.8 times as long. DISTINCT held 14.5 MB against 152 MB, and
at 16 MiB 4.4 MB at 1.7 times the eager time. A budget near a tenth of the state's size
thus costs 1.7 to 2.8 times the time, and one near half of it costs little.

The review corrections of 2026-10-20 bound retained join output files and repair
merge EOF handling, keyless build limits and numbered DISTINCT accounting and order.
The measurements below precede those corrections and do not establish their cost.

On 2026-10-18 hash joins, merge-join runs and a DISTINCT below its ORDER BY learned to
spill too, and spill turned on by default under item 85. The same probe ran each query's
cursor under a budget beside the unlimited cursor and eager execution, at 10.5M on an
8-vCPU Namespace instance with its default threads, and checked that the spilled rows
matched the eager ones.

| 10.5M, ms | Eager | Cursor, unlimited | Cursor, 16 MiB | Cursor, 4 MiB | Written at 4 MiB |
|---|---:|---:|---:|---:|---:|
| Join on `foaf:knows` under a merge join, 2.5M rows | **66** | 150 | 322 | 1,333 | 157 MB |
| OPTIONAL with a filter, 300,191 rows | **34** | 57 | 77 | 186 | 24.6 MB |
| MINUS, 90,161 rows | **21** | 36 | 59 | 165 | 24.6 MB |
| `SELECT DISTINCT … ORDER BY`, 917,941 rows | 615 | **569** | 1,421 | 3,028 | 60.1 MB |

The unlimited cursors held 61.8 to 90.8 MB for the joins and 1,016 MB for the DISTINCT,
and every spilled run stayed within its budget. A join of 29.2M rows where one key holds
nine tenths of each input took 681 ms under 1 MiB as a block nested loop, against 487 ms
for the unlimited cursor, which held 34.3 MB, and 631 ms eagerly. The `foaf:knows` join sits under a merge
join that relies on its order, so a spilled run writes its output a second time to merge
it back, which is why it slows more than the OPTIONAL and MINUS joins at the root. The
spilled joins wrote 95 to 150 MB a second. `--spill encrypted` added 1 to 6% to the bytes
written and −2 to 21% to the times under 4 MiB, most of them under 10%.

<a id="parallel-cursor-filters"></a>

**Parallel cursor filters.** On 2026-10-18 a cursor filter over an index scan began to
read the scan in morsels of whole blocks and filter each morsel in parallel
([X05](specs/X05-streaming-query-execution.md#parallel-cursor-filters-2026-10-18)). The
probe binary interleaved eager and cursor runs over 10.5M triples on an 8-vCPU AMD EPYC
Namespace instance, reading every row. The table gives the cursor's time divided by the
eager time, the median of seven rounds, against 5c73ced6.

| Ratio of streaming to eager | 5c73ced6, 8 threads | 2026-10-18, 8 threads | 5c73ced6, 1 thread | 2026-10-18, 1 thread |
|---|---:|---:|---:|---:|
| CONTAINS on names, 49,837 rows | 1.95 | **0.91** | **0.95** | 1.00 |
| REGEX on subject IRIs, 1,000 rows | 2.11 | 1.08 | 1.14 | 1.15 |
| STRSTARTS on names, 49,837 rows | 7.99 | **0.94** | 2.98 | **0.90** |
| LANGMATCHES on titles, 500,000 rows | 1.48 | **0.95** | **0.84** | **0.88** |

At one thread CONTAINS moved from 0.95 to 1.00 and REGEX from 1.14 to 1.15, which is
within the noise of the AMD instance and needs an Intel host to confirm. The cursor's
ledger peaked at 13.5 MB for the CONTAINS filter with eight threads, against
11.8 MB before and 15.6 MB eagerly, and at 6.2 MB with one thread, since each worker holds
its own morsel scratch.

The cursor's range top-k reads the parts of its range without value order in parallel
morsels and keeps only its best rows
([X05](specs/X05-streaming-query-execution.md#streaming-range-top-k-2026-10-18)). Over the
salaries above 150,000 at 10.5M it took 0.89 to 1.06 times its eager time at eight threads,
against 1.33 at 5c73ced6, and 0.88 at one thread against 0.93. The eight-thread figure
is a range over runs of 15 to 21 rounds on two 8-vCPU AMD EPYC Namespace instances,
because the runs spread by more than the 5% one instance can resolve. Its ledger peaked
at 6.6 to 7.1 MB at eight threads and 3.5 MB at one, against 5.6 MB before at both. The
figures for the ledger are those after the fixes that followed review.

In native JSON at one thread, the STRSTARTS filter over names took 1.30 times its eager
time at 5c73ced6 and takes 1.01 to 1.03 times now, the median of 15 rounds in two
processes. The parallel filter reads fewer blocks, and the cursor writer's term cache
sorts its ids with a stable sort that merges the already ascending columns
([X05](specs/X05-streaming-query-execution.md#native-json-of-a-streamed-filter-2026-10-18)).
That sort also took the streamed name scan of 1.02M rows from 0.94 to 0.88 to 0.90 times
eager execution.

The JVM library also runs in automatic mode by default since 2026-10-17. In the binding
comparison of `scripts/bench-bindings` at 1.05M on forge, with every case run twice in
each mode in alternating processes, the JNI export of 500,000 rows took 130 ms against
126 ms eagerly on disk and 129 ms against 124 ms in memory. No other query, `find`,
`contains` or `getProperty` moved by more than run-to-run noise, and the small-query
latencies at one thread stayed within 2%.

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
the next size class, so a full decoded block column of 256 KiB took 320 KiB. The X08 A1
gate measured the change on forge with ten datasets of 1.05M triples, after the suite ran
against each, three repeats each and medians below. `scripts/x08-gates/a1.sh` reproduces
it.

| `--cache-mb` | Datasets | Cache bytes | `RssAnon` before | `RssAnon` after | VmRSS before | VmRSS after |
|--:|--:|--:|--:|--:|--:|--:|
| 256 | 1 | 60.6 MiB | 132.5 MiB | 120.7 MiB | 195.7 MiB | 176.5 MiB |
| 256 | 10 | 379 MiB | 572.5 MiB | 492.9 MiB | 702.8 MiB | 612.8 MiB |
| 32 | 1 | 44.2 MiB | 120.7 MiB | 111.0 MiB | 184.2 MiB | 167.5 MiB |
| 32 | 10 | 160 MiB | 274.8 MiB | 264.5 MiB | 405.3 MiB | 387.3 MiB |

Most of the cache bytes at `--cache-mb 32` belong to the result cache, which is sized
separately and whose heap stays about 1.2 times the bytes it counts. Roadmap item 88
covers that overhead.

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

## Where Sparkles loses

This table lists every comparison on this page where another engine is ahead by more
than 10%. Fluree's warm advantage on `star-lookup` at 1.05M is within 10%.

| Case | Reference advantage | Qualification |
|---|---|---|
| SP²Bench `q8` and `q12b`, 10M | Jena 181× on `q8`, Oxigraph about 5,000× and Jena about 2,900× on `q12b` | Sparkles' plan produces 555,607,405 rows for the one-row ASK of `q12b`. |
| SP²Bench `q4`, `q5a`, `q6` and `q12a`, 10M | Jena answers `q6` and `q12a`, and Oxigraph `q12a` | Sparkles' default query memory budget refuses all four. No engine finished `q4` or `q5a`. |
| LUBM(100) `q2` | QLever 3.7× | Sparkles is fastest on 11 of 14 queries. |
| LUBM(100) `q1` and `q3` means | Fluree 1.19× and 1.22× | Sparkles' medians are lower, so the means reflect slow outliers. |
| Single-triple update latency in the suite | Oxigraph 1.51× and QLever 1.39× at 1.05M, Oxigraph 1.59×, QLever 1.35× and Fluree 1.15× at 10.5M | Sparkles takes 5.3 and 5.4 ms per request including curl, and synchronizes each acknowledgment. Oxigraph does not synchronize its WAL, and QLever keeps updates in memory. |
| Serial durable commits | Oxigraph 1.84× on literal inserts at 1.05M and 1.39× at 10.5M | The same durability difference. |
| Writes during mixed load | Oxigraph 3.4× at 1.05M and 2.8× at 10.5M, Fluree 1.2× at 10.5M | Both serve far fewer concurrent reads. |
| Cold point lookups | Oxigraph 1.5–2.9× at 1.05M and 1.3–2.7× at 10.5M | Listed under [Cold starts](#cold-starts). |
| Cold small scans at 1.05M | Fluree 1.26–2.64× on `count-all`, `distinct-obj` and `employee-docs` | Fluree is not ahead on any cold query at 10.5M. |
| DBpedia cold `outlink-classes-2` | QLever 1.25× | Sparkles wins 29 of 31 cold queries. |
| Server RSS after the run, 1.05M | QLever 1.14× | Without its block cache, Sparkles holds 289 MiB. Sparkles has the lowest RSS after the run at 10.5M. |
| Peak RSS under 16 readers | QLever 1.44× at 1.05M and 1.34× at 10.5M | Sparkles peaks at 487 and 1,261 MiB, against 339 and 939 MiB. |
| Bulk load peak RSS | QLever 2.87× on the LUBM(100) closure and 1.20× on SP²Bench, Fluree 1.23× on SP²Bench | Sparkles has the lowest peak at 1.05M and on DBpedia, and is within 10% of QLever at 10.5M. |
| DBpedia server RSS | QLever 3.14× | Sparkles maps its index, so touched file pages count toward its RSS. |
| Text index size | QLever about 1.18× at 1.05M and 1.23× at 10.5M | The indexed predicates and features differ. |
| JVM small lookups | TIM 1.3–2.1× and TDB2 1.1–1.4× on `star-lookup`, `values-star` and `employee-docs` | A small query's fixed cost through Jena dominates. |
| Python small lookups | pyoxigraph 1.2–2.5× on `knows-reach`, `values-star` and `employee-docs` | The same fixed cost per query. |
| Node.js small lookups and adds | Oxigraph's package 1.1–1.4× on four small queries, N3.js 1.2–1.5× on adds and `has()` | Each add crosses to the transaction's thread. |
| Node.js `export-500k` | N3.js with Comunica 1.44× | Each row's terms cross into JavaScript. |

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
  * Fluree 4.2.2 is the release binary. The published Fluree figures come from runs in
    which `scripts/bench.sh` downloaded the release archive and verified it against a
    checksum. Later runs take the binary from this flake's package of the same release
    archive, which `nix/fluree.nix` pins by SHA-256. On Linux that package rewrites the
    executable to load the glibc of the locked nixpkgs instead of the host's, so the
    executable's SHA-256 in result manifests differs from the downloaded binary's. The
    published figures were not measured with the packaged executable. Fluree loads with
    `fluree create bench --from data.nt --chunk-size-mb 16` and serves with
    `fluree server run`, `FLUREE_CACHE_MAX_MB=4096` and
    `FLUREE_PATH_MAX_VISITED=20000000`, because the default traversal cap of 1M nodes
    rejects `knows-reach` at 10.5M. Fluree has no result cache.
  * Oxigraph loads with `oxigraph load` and then `oxigraph optimize`, the compaction its
    loader recommends before read-heavy workloads, and serves with
    `oxigraph serve --timeout-s 600`. The harness reads the N-Triples from standard input.
    With `--file`, Oxigraph 0.5.11 parses N-Triples in parallel chunks and turns a blank
    node label used in two chunks into two nodes. The figures above predate this change.
    The synthetic data and WatDiv contain no blank nodes and the DBpedia load already read
    standard input, so the figures are not affected, but the load times of a later refresh
    will be about 3x longer than with `--file`.
* **Isolation.** Each engine runs alone on the machine, one after another, in a systemd
  scope with a 12 GiB memory limit and no swap, so an engine that runs out of memory is
  killed on its own. The OS page cache is warm except in the cold runs.
* **Timing.** hyperfine 1.20 times SPARQL protocol POSTs with TSV results, with 2 warm-ups
  and 10 runs. Before the refresh of 2026-10-18 the 10.5M runs had 1 warm-up and 5 runs. Times are mean ± σ in ms and
  include starting `curl` and the round trip, about 3.5 ms. The client runs on CPU 11, the
  last performance core, and the servers are not pinned.
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
ranked, which happens only to QLever, on `types-grouped` after the update churn. A ‡ marks
equal values returned as different RDF terms. `export-500k` uses LIMIT without ORDER BY,
so only its row count is checked.

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
  FSST vocabulary compression and IRI encoding matter more as data grows. Sparkles spills
  cursor sorts, groupings, DISTINCT and joins. A query still fails when its eager state
  outgrows its budget.
* **Data larger than RAM.** Every store fits in memory except the DBpedia index of
  28.7 GiB under a 14 GiB limit.
* **Other standard benchmarks.** SP²Bench at 10M triples and LUBM(100) have been compared
  on the reference machine. BSBM's comparison waits for a rerun, SP²Bench at 100M and
  LUBM(1000) have not been run, and the OWL2Bench harness has not been run there either.
  LDBC, WatDiv's stress testing and the Wikidata query log have no harness. The synthetic
  suite has a regular shape, and its 28 queries are hand-picked.
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
  measurement window in the context table.
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
