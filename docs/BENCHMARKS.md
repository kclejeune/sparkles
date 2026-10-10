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
| Measured | 2026-10-03 to 2026-10-09. The QLever figures for the DBpedia `geo-box` and `country-population` queries come from an earlier run on the same host. |

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
| Bulk load | **0.48 s** (Fluree 1.27, Oxigraph 1.28, QLever 1.51, TDB2 4.78) | **3.87 s** (QLever 9.69, Fluree 11.07, Oxigraph 14.13, TDB2 45.57) |
| Fastest of the five engines | 26 of 28 queries | 27 of 28 queries |
| Clear losses | `range-topk` to QLever, 1.12× | none |
| Ties within 10% | Fluree on `count-all`, `distinct-obj`, `star-lookup`, `values-star` and `employee-docs` | Fluree on `count-all`, `distinct-obj` and `star-lookup` |
| Against QLever | Faster on 27 of 28, median 2.7× | Faster on 28 of 28, median 3.9× |
| Against Fluree | Faster on 27 of 28, median 3.5× | Faster on 26 of 27 completed, median 9.9× |
| Against Fuseki | Faster on all 27 completed, median 13× | Faster on all 27 completed, median 79× |
| Against Oxigraph | Faster on all 28, median 25× | Faster on all 28, median 85× |
| Throughput, 16 clients | **1,220 q/s** (QLever 462, Fluree 246, Fuseki 113, Oxigraph 25) | **241 q/s** (QLever 92, Fluree 22, Fuseki 11, Oxigraph 1.7) |
| Server RSS after the run | 405 MiB (**QLever 307**, Oxigraph 1,244, Fuseki 1,769, Fluree 2,735) | 840 MiB (**QLever 653**, Oxigraph 2,381, Fluree 4,664, Fuseki 4,899) |
| Server RSS less the block cache | 341 MiB | 461 MiB |

On larger and standard workloads:

* **DBpedia, 1.24 billion triples.** Sparkles loads English DBpedia in 480 s, against
  QLever's 1,674 s and Fluree's 2,919 s. It is faster than QLever on all 29 warm queries that both answer
  correctly and on 29 of 31 cold ones.
* **WatDiv at 10.97M triples.** Sparkles is fastest on all 20 templates, with a
  geometric mean of 4.84 ms against 7.41 ms for Fluree, 14.2 ms for Fuseki, 17.2 ms for
  Oxigraph and 18.2 ms for QLever.
* **Full-text search.** Sparkles is fastest on all seven queries at 1.05M and on six of
  seven at 10.5M, against Fuseki with jena-text and QLever.
* **Durable writes.** A commit that adds a new literal takes a median of 1.13 ms at 1.05M
  and 1.28 ms at 10.5M, synchronized to disk before it is acknowledged. Oxigraph commits
  faster without synchronizing, and Fluree takes 1.55 to 1.68 ms in the suite's churn.
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
| count-all | **3.5 ± 0.2** | 173.7 ± 19.8 | 10.7 ± 0.9 ‡ | 3.7 ± 0.2 | 256.3 ± 3.8 |
| types-grouped | **3.6 ± 0.1** | 77.4 ± 8.0 | 5.9 ± 0.3 ‡ | 19.0 ± 1.0 | 42.9 ± 2.2 |
| star-join | **7.5 ± 0.2** | 30.6 ± 1.7 | 33.5 ± 3.6 ‡ | 22.5 ± 1.8 | 388.9 ± 4.8 |
| two-hop-count | **5.2 ± 0.1** | 373.2 ± 2.6 | 19.1 ± 2.9 ‡ | 7.1 ± 0.3 | 827.3 ± 5.7 |
| range-topk | 7.1 ± 0.4 ‡ | 95.5 ± 2.5 | **6.4 ± 0.3** | 24.6 ± 1.2 | 57.3 ± 2.2 |
| optional-count | **4.4 ± 0.2** | 80.5 ± 4.9 | 10.1 ± 0.6 ‡ | 15.8 ± 0.7 | 109.6 ± 5.8 |
| contains | **4.3 ± 0.3** | 43.8 ± 2.7 | 75.4 ± 3.8 ‡ | 8.6 ± 0.2 | 147.9 ± 7.0 |
| group-avg | **8.7 ± 0.2** | 207.7 ± 3.3 ‡ | 14.2 ± 0.9 ‡ | 58.6 ± 3.8 ‡ | 308.6 ± 5.6 |
| path-plus | **3.5 ± 0.1** | 6.6 ± 0.6 | 6.2 ± 0.4 ‡ | 7.5 ± 0.2 | 4.0 ± 0.2 |
| distinct-obj | **3.6 ± 0.1** | 90.7 ± 3.2 | 18.0 ± 2.9 ‡ | 3.7 ± 0.1 | 70.6 ± 2.6 |
| export-500k | **92.0 ± 2.7** | 500.8 ± 17.5 | 778.5 ± 16.6 | 245.2 ± 1.9 | 1708.6 ± 3.8 |
| predicate-counts | **3.4 ± 0.1** | 326.6 ± 2.1 | 14.6 ± 0.4 ‡ | 67.4 ± 1.4 | 288.1 ± 3.5 |
| order-by-full | **48.7 ± 6.7** | 543.5 ± 6.6 | 138.6 ± 4.0 | 106.0 ± 0.8 | 3835.0 ± 27.9 |
| optional-chain | **14.2 ± 1.1** | 226.1 ± 4.6 | 63.8 ± 5.2 | 1829.5 ± 85.9 | 362.1 ± 3.9 |
| minus | **4.0 ± 0.1** | 41.7 ± 2.6 | 7.1 ± 0.2 ‡ | 11.9 ± 3.8 | 33.6 ± 4.3 |
| not-exists | **4.6 ± 0.2** | 56.5 ± 1.2 | 7.1 ± 0.3 ‡ | 20.5 ± 1.4 | 111.6 ± 7.0 |
| exists-join | **5.0 ± 0.1** | 161.3 ± 37.7 | 10.1 ± 0.6 ‡ | 31.9 ± 0.4 | 1458.5 ± 18.9 |
| subquery-agg | **4.0 ± 0.2** | 51.1 ± 1.3 | 5.4 ± 0.2 ‡ | 20.4 ± 1.1 | 25.3 ± 3.4 |
| regex-iri | **4.7 ± 0.4** | 58.5 ± 6.6 | 72.2 ± 2.6 ‡ | 88.2 ± 0.6 | 173.9 ± 7.8 |
| knows-reach | **16.8 ± 0.7** | error | 62.0 ± 6.1 ‡ | 61.4 ± 10.7 | 342.8 ± 6.5 |
| distinct-join | **8.5 ± 0.1** | 412.3 ± 3.4 | 23.6 ± 4.3 | 91.1 ± 1.1 | 672.9 ± 7.3 |
| lang-filter | **4.4 ± 0.2** | 31.8 ± 3.2 | 42.8 ± 1.9 ‡ | 6.4 ± 0.1 | 86.2 ± 6.6 |
| star-lookup | 3.9 ± 0.1 ‡ | 5.8 ± 0.4 ‡ | 10.4 ± 0.3 ‡ | **3.7 ± 0.2** | 4.9 ± 0.3 |
| values-star | **3.7 ± 0.1** | 6.7 ± 0.4 | 10.8 ± 0.7 ‡ | 4.1 ± 0.1 | 4.2 ± 0.2 |
| employee-docs | **3.78 ± 0.13** | 6.9 ± 0.5 | 7.1 ± 0.6 | 3.81 ± 0.17 | 4.4 ± 0.1 |
| expr-bind-group | **4.7 ± 0.4** | 48.1 ± 1.1 ‡ | 7.9 ± 0.5 ‡ | 44.4 ± 0.2 | 46.2 ± 4.1 |
| expr-order-key | **6.1 ± 0.3** | 89.1 ± 1.1 | 14.2 ± 0.6 ‡ | 114.8 ± 0.3 | 2672.4 ± 18.4 |
| expr-agg-arg | **8.4 ± 0.2** | 166.1 ± 0.9 | 14.4 ± 1.0 ‡ | 75.9 ± 2.8 | 298.7 ± 9.2 |
| **throughput** star-join, 16 clients (queries/s) | **1220** | 113 | 462 | 246 | 25 |

### Synthetic suite at 10.5M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| count-all | **3.86 ± 0.25** | 1621.1 ± 16.0 | 53.3 ± 2.7 ‡ | 3.92 ± 0.28 | 2497.6 ± 18.9 |
| types-grouped | **3.8 ± 0.3** | 5208.4 ± 285.4 | 6.6 ± 1.1 ‡ | 113.4 ± 9.3 | 381.2 ± 3.6 |
| star-join | **31.0 ± 0.4** | 251.7 ± 17.1 | 115.1 ± 7.1 ‡ | 142.2 ± 4.6 | 4710.7 ± 22.6 |
| two-hop-count | **17.1 ± 1.0** | 3845.8 ± 30.9 | 61.3 ± 5.5 ‡ | 36.3 ± 0.5 | 8292.3 ± 59.6 |
| range-topk | **14.9 ± 1.4** | 897.4 ± 30.3 | 22.4 ± 2.6 | 153.1 ± 0.9 | 596.1 ± 3.0 |
| optional-count | **11.5 ± 0.2** | 1326.0 ± 161.4 | 45.8 ± 7.7 ‡ | 114.7 ± 5.4 | 992.3 ± 9.8 |
| contains | **7.2 ± 0.3** | 3811.0 ± 60.2 | 690.9 ± 3.5 ‡ | 17.7 ± 0.3 | 1534.9 ± 13.5 |
| group-avg | **53.8 ± 2.3** | 4252.3 ± 382.6 ‡ | 93.4 ± 5.2 ‡ | 549.8 ± 9.5 ‡ | 4057.6 ± 59.3 |
| path-plus | **3.7 ± 0.2** | 6.3 ± 0.2 | 10.3 ± 0.4 ‡ | 54.6 ± 3.5 | 4.1 ± 0.2 |
| distinct-obj | **3.7 ± 0.3** | 2103.1 ± 675.4 | 205.4 ± 5.5 ‡ | 3.9 ± 0.2 | 746.9 ± 2.0 |
| export-500k | **95.6 ± 7.7** | 734.4 ± 84.6 | 661.2 ± 3.1 | 260.4 ± 1.3 | 2087.7 ± 6.6 |
| predicate-counts | **3.8 ± 0.3** | 3266.4 ± 28.4 | 74.1 ± 3.5 ‡ | 608.3 ± 6.0 | 2798.3 ± 5.3 |
| order-by-full | **354.1 ± 21.1** | 13852.8 ± 730.3 | 1421.4 ± 10.9 | 1913.6 ± 7.7 | 55456.3 ± 163.5 |
| optional-chain | **79.4 ± 7.5** | 3825.0 ± 346.9 | 571.2 ± 23.4 | — | 4764.9 ± 72.5 |
| minus | **8.6 ± 0.1** | 2297.5 ± 246.6 | 20.0 ± 2.5 ‡ | 56.9 ± 2.1 | 349.6 ± 12.5 |
| not-exists | **15.1 ± 1.1** | 578.6 ± 141.0 | 22.5 ± 2.2 ‡ | 164.1 ± 2.0 | 1032.3 ± 4.5 |
| exists-join | **14.6 ± 0.7** | 1999.2 ± 290.0 | 66.7 ± 6.6 ‡ | 276.2 ± 2.3 | 122436.9 ± 245.5 |
| subquery-agg | **5.0 ± 0.4** | 1771.5 ± 307.8 | 14.6 ± 0.8 ‡ | 146.7 ± 1.1 | 183.6 ± 2.3 |
| regex-iri | **8.5 ± 0.3** | 1611.5 ± 21.5 | 659.4 ± 1.6 ‡ | 873.5 ± 26.4 | 2642.1 ± 34.5 |
| knows-reach | **145.0 ± 2.6** | — | 1157.6 ± 10.0 ‡ | 658.3 ± 13.8 | 4651.3 ± 88.4 |
| distinct-join | **48.0 ± 1.0** | 4468.2 ± 84.5 | 155.8 ± 8.1 | 1207.0 ± 59.2 | 6704.7 ± 31.7 |
| lang-filter | **6.2 ± 0.2** | 2734.7 ± 189.2 | 389.2 ± 3.2 ‡ | 28.6 ± 1.4 | 839.1 ± 11.2 |
| star-lookup | 4.7 ± 1.4 ‡ | 7.2 ± 0.3 | 24.3 ± 2.8 ‡ | **4.3 ± 0.3** | 5.4 ± 0.4 |
| values-star | **3.8 ± 0.3** | 8.2 ± 2.0 | 11.8 ± 1.1 ‡ | 4.8 ± 0.7 | 4.3 ± 0.2 |
| employee-docs | **3.9 ± 0.3** | 7.7 ± 0.5 | 12.5 ± 0.8 | 4.7 ± 0.3 | 4.7 ± 0.3 |
| expr-bind-group | **8.9 ± 0.4** | 426.7 ± 11.6 ‡ | 34.1 ± 3.6 ‡ | 397.2 ± 1.9 | 354.4 ± 1.5 |
| expr-order-key | **20.1 ± 0.4** | 937.2 ± 18.3 | 109.4 ± 4.0 ‡ | 1120.8 ± 27.1 | 38266.9 ± 54.8 |
| expr-agg-arg | **46.0 ± 1.2** | 1789.4 ± 2.6 | 98.7 ± 6.9 ‡ | 767.0 ± 4.3 | 3899.0 ± 22.3 |
| **throughput** star-join, 16 clients (queries/s) | **241** | 11 | 92 | 22 | 1.7 |

‡ Same values as different RDF terms. QLever returns integers such as counts and ages as
`xsd:int`, where the data and the other engines use `xsd:integer`. Sparkles writes
`xsd:decimal` values in XSD 1.1 canonical form, so the data's `"175000.50"` comes back as
`"175000.5"`. In `group-avg` the averages agree to 12 significant digits.

Fuseki 5.1 fails `knows-reach` (`<person/0> foaf:knows* ?x`) with a `StackOverflowError`
in TDB2's node cache. At 10.5M the failure leaves the cache wedged, so `knows-reach` is
left out of Fuseki's 10.5M run. Fluree's `optional-chain` at 10.5M grows past the 12 GiB
memory limit, so it is left out of Fluree's 10.5M run.

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
ready 0.089 s after a restart at both sizes. Sparkles is fastest on 19 of 28 cold queries
at 1.05M and on 23 of 28 at 10.5M. The cold losses are these:

| Query, cold | Faster engine | 1.05M | 10.5M |
|---|---|---|---|
| `count-all` | Fluree at 1.05M | 2.64× (3.5 against 1.3 ms) | — |
| `star-join` | Fluree at 1.05M | 1.52× (53.1 against 34.9 ms) | — |
| `range-topk` | QLever | 1.15× (26.5 against 23.0 ms) | 1.01× (44.5 against 43.9 ms) |
| `contains` | Fluree at 1.05M | 1.15× (14.4 against 12.4 ms) | — |
| `path-plus` | Oxigraph | 3.83× (7.5 against 1.9 ms) | 2.77× (5.3 against 1.9 ms) |
| `lang-filter` | Fluree at 1.05M | 1.28× (11.4 against 8.9 ms) | — |
| `star-lookup` | Oxigraph | 1.84× (27.4 against 14.9 ms) | 3.05× (47.8 against 15.7 ms) |
| `values-star` | Oxigraph | 2.16× (12.0 against 5.5 ms) | 2.71× (12.9 against 4.8 ms) |
| `employee-docs` | Fluree at 1.05M, Oxigraph at 10.5M | 1.64× (16.8 against 10.2 ms) | 2.29× (29.3 against 12.8 ms) |

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
| Exact | 1.0000 | 5.458 | 5.476 | 6.109 |
| HNSW `ef=16` | 0.9916 | 0.257 | 0.251 | 0.336 |
| HNSW `ef=32` | 0.9982 | 0.309 | 0.299 | 0.413 |
| HNSW `ef=64` | 0.9993 | 0.429 | 0.418 | 0.783 |
| HNSW `ef=128` | 0.9993 | 0.655 | 0.648 | 0.927 |
| HNSW `ef=256` | 0.9993 | 1.181 | 1.169 | 1.554 |

These figures describe this synthetic distribution, not all vectors.

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

## Loading and index builds

Sparkles builds its index in one parallel pass. Parsing and vocabulary merging run on all
cores, sampled keys divide the partial vocabularies into ranges that merge in parallel,
and each 64-million-quad chunk is remapped once, sorted by SPO, OSP and PSO and written as
compressed runs. SOP, OPS and POS derive from those streams by sorting runs of the first
column, and without named graphs GSPO derives from SPO. Each permutation has its own
writer, so the input is read once.

### Bulk loading

Load times include each engine's own preparation, such as `oxigraph optimize`, and peak
RSS comes from GNU time. Each load ran once.

| Load | sparkles | jena (tdb2.tdbloader) | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M triples, time | **0.48 s** | 4.78 s | 1.51 s | 1.27 s | 1.28 s |
| 1.05M triples, peak RSS | **349 MiB** | 2,188 MiB | 660 MiB | 475 MiB | 738 MiB |
| 10.5M triples, time | **3.87 s** | 45.57 s | 9.69 s | 11.07 s | 14.13 s |
| 10.5M triples, peak RSS | 2,165 MiB | 4,115 MiB | **1,931 MiB** | 3,142 MiB | 6,204 MiB |
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
| HNSW vector index, 100,000 × 384 dimensions, `M=16`, `efConstruction=128` | 14.0 s | 163 MB of mapped files, 1,019.5 MB peak RSS |
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
DELETE requests, one at a time, to every engine's store. No engine failed a commit.

| Churn of 5,000 commits | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: commits/s | 770 | 14 | 426 | 499 | **855** |
| 1.05M: latency p50 / p99 (ms) | 1.33 / 2.31 | 74.06 / 102.20 | 2.27 / 4.57 | 1.68 / 7.16 | **1.16 / 2.04** |
| 10.5M: commits/s | 663 | 14 | 475 | 459 | **1,140** |
| 10.5M: latency p50 / p99 (ms) | 1.38 / 3.62 | 74.69 / 103.16 | 2.07 / 4.73 | 1.55 / 12.76 | **0.90 / 1.58** |

The WAL preallocates space in steps from 64 KiB up to 4 MiB (`--wal-prealloc-kb`, `0`
disables it). Replay, readers, backups and quotas use the logical end of the last commit,
and recovery recognizes a torn tail.

### Concurrent durable commits

Four clients apply 50,000 fresh-literal changes, one quad per request, with the server on
ten logical CPUs. The runs alternate the default writer and opt-in group commit
(`--experimental-group-commit`), two fresh processes per setting. Group commit shares one
synchronization among eligible commits and keeps acknowledgments durable. Every run
completes all 50,000 inserts with contiguous receipts and the correct final state.

| Four writers, 50,000 changes | 1.05M default | 1.05M group commit | 10.5M default | 10.5M group commit |
|---|---:|---:|---:|---:|
| Acknowledged transactions/s | 221.5–231.8 | **529.0–543.3** | 201.1–204.0 | **402.3–406.9** |
| Request p50 (ms) | 19.016–19.211 | **9.051–9.104** | 19.338–19.415 | **9.709–9.735** |
| Request p99 (ms) | 39.972–41.342 | **15.693–15.894** | 41.066–41.200 | **16.089–16.171** |

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
| 1.05M: reads/s | **2054.0** | 94.5 | 510.6 | 34.6 | 10.9 |
| 1.05M: read p50 / p99 (ms) | **7.6 / 11.9** | 158.4 / 351.3 | 27.4 / 39.7 | 462.2 / 536.7 | 1192.3 / 3351.1 |
| 1.05M: writes/s | 264.0 | 28.3 | 109.4 | 246.5 | **1247.3** |
| 1.05M: write p50 / p99 (ms) | 2.9 / 10.1 | 17.9 / 97.7 | 6.0 / 25.8 | 3.6 / 16.2 | **0.8 / 1.0** |
| 1.05M: failed reads / writes | 0 / 0 | 0 / 0 | 23929 / 827 | 0 / 0 | 0 / 0 |
| 10.5M: reads/s | **198.8** | 7.7 | 69.5 | 3.6 | 0 |
| 10.5M: read p50 / p99 (ms) | **80.8 / 100.6** | 1977.0 / 2859.2 | 229.5 / 270.6 | 4402.7 / 8621.1 | — |
| 10.5M: writes/s | 187.3 | 20.7 | 51.1 | 299.4 | **676.7** |
| 10.5M: write p50 / p99 (ms) | 5.2 / 10.6 | 62.8 / 86.7 | 11.9 / 108.8 | 1.9 / 14.8 | **1.4 / 3.2** |

Sparkles serves 4.0× QLever's reads at 1.05M and 2.9× at 10.5M. Oxigraph commits more
writes at both sizes, and Fluree does at 10.5M, while both serve far fewer reads. At 10.5M no Oxigraph reader finished within
30 s. A separate run with the durable-commit harness above, with 16 readers on the 10.5M
store, completed 299 new-literal commits per second at a median of 3.2 ms.

### Queries after updates

The `updates` mode applies the same 5,000 single-triple changes to each engine's store and
runs the query suite again. The changes stay in Sparkles' delta until a compaction folds
them into the base index. At 10.5M the delta changes `star-join` from 31.0 to 59.8 ms,
`two-hop-count` from 17.1 to 38.7 ms, `contains` from 7.2 to 14.9 ms and `regex-iri` from
8.5 to 16.4 ms, and throughput from 241 to 169 q/s, against QLever's 87 q/s on its changed
store. On the changed stores Sparkles is fastest on 26 of 28 queries at both sizes.
QLever's changed-store `types-grouped` answer differs in value and is not ranked. The one
clear loss is `range-topk` at 1.05M, where QLever is 1.13× faster, and the other
differences are within 10%.

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
| RSS 2 s after start (MiB) | 52 | 257 | 28 | 52 | **22** |
| RSS after every query ran once (MiB) | 188 | 981 | 146 | 2083 | **91** |
| RSS after the run (MiB) | 405 | 1769 | **307** | 2735 | 1244 |
| Peak RSS during throughput (MiB) | 614 | 1769 | **335** | 2735 | 1250 |
| Index size on disk | 27 MiB | 132 MiB | **26 MiB** | 37 MiB | 147 MiB |

| 10.5M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| RSS 2 s after start (MiB) | 53 | 270 | 43 | 260 | **28** |
| RSS after every query ran once (MiB) | 743 | 3409 | 414 | 3377 | **408** |
| RSS after the run (MiB) | 840 | 4899 | **653** | 4664 | 2381 |
| Peak RSS during throughput (MiB) | 1383 | 4899 | **778** | 4756 | 2381 |
| Index size on disk | 286 MiB | 1.3 GiB | **276 MiB** | 398 MiB | 1.4 GiB |

Of Sparkles' 405 and 840 MiB after the run, 64 and 379 MiB are the block cache. At 10.5M
the largest per-query rises in peak RSS are `order-by-full` at 278 MiB, `two-hop-count`
at 239 MiB, `star-join` at 171 MiB and `optional-chain` at 97 MiB, which include cache
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
result columns and 1.0 MiB when streaming. Eager execution stays the default because it
is faster on several shapes, and automatic selection streams only the large scans and
OPTIONAL counts where streaming measured faster.

**mimalloc** keeps freed memory in per-thread heaps, and the server hands idle memory back
to the OS after a second (`--idle-release-ms`). On the development laptop, a fresh 10.5M
server with mimalloc held 920 MiB after concurrent rounds against 838 MiB for glibc with
idle release, and peaked at 1,682 MiB against 1,439 MiB. In exchange it served 190 q/s
against 144, and `star-join` took 38 ms against 72 ms. The bindings make the same choice
case by case. The Python package and the JVM library use mimalloc, which raises the
Python benchmark process's peak RSS by about 20% and a mixed JVM run's from 653 to
962 MiB. The Node.js addon keeps the system allocator, because mimalloc made no
measurable difference there.

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
than 10%. Fluree's warm advantages on small lookups at 1.05M are within 10%.

| Case | Reference advantage | Qualification |
|---|---|---|
| Warm `range-topk`, 1.05M | QLever 1.12× | The query filters salaries and keeps the top k. Sparkles wins at 10.5M. |
| Single-triple update latency in the suite | Oxigraph 1.13× and QLever 1.11× at 1.05M, QLever 1.17× and Oxigraph 1.14× at 10.5M | Sparkles takes 4.2 and 4.9 ms per request including curl, and synchronizes each acknowledgment. Oxigraph does not synchronize its WAL, and QLever keeps updates in memory. |
| Serial durable commits | Oxigraph 1.84× on literal inserts at 1.05M and 1.39× at 10.5M | The same durability difference. |
| Serial commits in the suite's churn | Oxigraph 1.11× at 1.05M and 1.72× at 10.5M | The same durability difference. |
| Writes during mixed load | Oxigraph 4.7× at 1.05M and 3.6× at 10.5M, Fluree 1.6× at 10.5M | Both serve far fewer concurrent reads. |
| Cold point lookups | Oxigraph 1.8–3.8× at 1.05M and 2.3–3.1× at 10.5M | Listed under [Cold starts](#cold-starts). |
| Cold small scans at 1.05M | Fluree 1.15–2.64× on `count-all`, `star-join`, `contains`, `lang-filter` and `employee-docs` | Sparkles wins these at 10.5M. |
| DBpedia cold `outlink-classes-2` | QLever 1.25× | Sparkles wins 29 of 31 cold queries. |
| Server RSS after the run | QLever 1.32× at 1.05M and 1.29× at 10.5M | Without its block cache, Sparkles holds 341 and 461 MiB. |
| Peak RSS under 16 readers | QLever 1.83× at 1.05M and 1.78× at 10.5M | Sparkles peaks at 614 and 1,383 MiB, against 335 and 778 MiB. |
| Bulk load peak RSS, 10.5M | QLever 1.12× | Sparkles has the lowest peak at 1.05M and on DBpedia. |
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
  * Fluree 4.2.2 is the release binary, which `scripts/bench.sh` downloads and verifies
    against a checksum. It loads with `fluree create bench --from data.nt
    --chunk-size-mb 16` and serves with `fluree server run`, `FLUREE_CACHE_MAX_MB=4096` and
    `FLUREE_PATH_MAX_VISITED=20000000`, because the default traversal cap of 1M nodes
    rejects `knows-reach` at 10.5M. Fluree has no result cache.
  * Oxigraph loads with `oxigraph load` and then `oxigraph optimize`, the compaction its
    loader recommends before read-heavy workloads, and serves with
    `oxigraph serve --timeout-s 600`.
* **Isolation.** Each engine runs alone on the machine, one after another, in a systemd
  scope with a 12 GiB memory limit and no swap, so an engine that runs out of memory is
  killed on its own. The OS page cache is warm except in the cold runs.
* **Timing.** hyperfine 1.20 times SPARQL protocol POSTs with TSV results, with 2 warm-ups
  and 10 runs at 1.05M and 1 warm-up and 5 runs at 10.5M. Times are mean ± σ in ms and
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

### What the benchmarks do not cover

* **Larger scale.** The largest dataset is English DBpedia at 1.24 billion triples. QLever
  is built for 10⁹–10¹¹ triples and runs Wikidata and UniProt, and its lazy evaluation,
  FSST vocabulary compression and IRI encoding matter more as data grows. Sparkles has no
  disk spill, so a query whose operator state outgrows its budget fails.
* **Data larger than RAM.** Every store fits in memory except the DBpedia index of
  28.7 GiB under a 14 GiB limit.
* **Other standard benchmarks.** LUBM, BSBM, SP²Bench, WatDiv's stress testing and the
  Wikidata query log were not run. The synthetic suite has a regular shape, and its 28
  queries are hand-picked.
* **Long runs.** Hours of sustained writes and compaction under load were not compared
  with other engines.
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
