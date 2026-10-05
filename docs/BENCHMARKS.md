# Benchmarks

This page is a dated snapshot of Sparkles performance, including comparisons with
Apache Jena (TDB2 and Fuseki), QLever, Fluree and Oxigraph. The main comparisons were
measured on 2026-10-03 on `forge`, with Sparkles at commit `98a75c1a`.
[Sparkles HTTP latency](#sparkles-http-latency) gives Sparkles-only measurements from
2026-10-05 with a different timing method. Feature and configuration comparisons
explain the effects of caching, loading, query execution and storage settings.
[Other measurements](#other-measurements) includes separately dated measurements,
mostly on a different machine. Each section states its scope and method.

To reproduce the runs, use `mise run bench [people] [workdir]` or `scripts/bench.sh`. With
`mise run bench`, `--engines fluree` re-measures one engine and merges its results into an
existing run, and `--answers-only` re-checks every engine's answers without timing anything.
`scripts/bench.sh` takes the same settings as the variables `ENGINES` and `ANSWERS_ONLY=1`.
`mise run bench:watdiv [scale]` runs WatDiv, `scripts/bench-text.sh` runs the full-text
comparison, and `mise run bench:billion full` runs DBpedia.

## Summary

| | 1.05M triples | 10.5M triples |
|---|---|---|
| Bulk load | **0.49 s** (Fluree 1.27, Oxigraph 1.28, QLever 1.51, TDB2 4.78) | **3.68 s** (QLever 9.69, Fluree 11.07, Oxigraph 14.13, TDB2 45.57) |
| Fastest of the five | 26 of 28 queries | 28 of 28 queries |
| Losses | `range-topk` to QLever, 1.24×. `star-lookup` to Fluree by 1%, a tie. | none |
| Ties within 10% | Fluree on `count-all`, `distinct-obj` and `employee-docs` | Fluree on `count-all`, `distinct-obj` and `star-lookup`. Oxigraph on `values-star`. |
| vs. QLever | Faster on 27 of 28, median 2.7× | Faster on 28 of 28, median 3.8× |
| vs. Fluree | Faster on 27 of 28, median 3.3× | Faster on 27 of 27, median 7.7× |
| vs. Fuseki | 1.5–92× faster, median 13×. Fuseki fails `knows-reach`. | 1.7–1,379× faster, median 60× |
| vs. Oxigraph | 1.14–450× faster, median 24× | 1.08–8,295× faster, median 87× |
| Update latency, 1 triple | 3.83 ms (**Oxigraph 3.71**, QLever 3.79, Fluree 5.60, Fuseki 32.6) | **4.17 ms** (QLever 4.19, Oxigraph 4.27, Fluree 4.55, Fuseki 32.5) |
| Throughput, 16 clients | **1,239 q/s** (QLever 462, Fluree 246, Fuseki 113, Oxigraph 25) | **243 q/s** (QLever 92, Fluree 22, Fuseki 11, Oxigraph 1.7) |
| Server RSS after the run | 403 MiB (**QLever 307**, Oxigraph 1,244, Fuseki 1,769, Fluree 2,735) | 916 MiB (**QLever 653**, Oxigraph 2,381, Fluree 4,664, Fuseki 4,899) |
| Server RSS less the block cache | 339 MiB | 537 MiB |
| Bulk load peak RSS | **363 MiB** (Fluree 475, QLever 660, Oxigraph 738, TDB2 2,188) | 2,112 MiB (**QLever 1,931**, Fluree 3,142, TDB2 4,115, Oxigraph 6,204) |

On WatDiv at scale 100 (10.97M triples), Sparkles is the fastest of the five on 19 of 20
query templates and on 80 of 88 query instances. Fluree is faster on the template `S7` by
0.5%, and on eight instances Fluree or Oxigraph is faster by 4% or less, which are ties.
Sparkles' geometric mean is 5.04 ms, against 7.41 ms for Fluree, 14.2 ms for Fuseki,
17.2 ms for Oxigraph and 18.2 ms for QLever.

In the full-text comparison, Sparkles is the fastest on 6 of 7 queries at 1.05M and on 5
of 7 at 10.5M. QLever wins `highlight` at both sizes, for which it returns the literals
without highlighting them, and it counts the hits of a common word 1.07× faster at 10.5M.

On English DBpedia at 1.24 billion triples, Sparkles loads the data in 596 s, against
1,674 s for QLever and 2,919 s for Fluree. Warm, it is faster than QLever on 28 of the 29
queries whose answers agree, and QLever is 1.3% faster on `geo-box`, which is a tie. It
wins both throughput tests, and cold it is faster on 26 of 31 queries. Fluree loaded the
data, but its server reached its 14 GiB limit during the warm runs and was killed, so
only its cold runs and four warm queries were measured. Jena and Oxigraph were not run
at this scale, and [DBpedia at 1.24 billion triples](#dbpedia-at-124-billion-triples)
explains why.

Sparkles uses more memory than QLever, and that is a choice. Its defaults spend memory on
a 1 GiB decoded-block cache per dataset and on materialized intermediate results.
[Memory and the speed it buys](#memory-and-the-speed-it-buys) shows what each part costs
and what a smaller cache gives up. [Where Sparkles loses](#where-sparkles-loses) lists
every loss and tie with its likely cause.

## Setup

* **Data:** `scripts/gen-data.py N` generates synthetic data with about 10.5 triples per
  person. The data has people, organisations, documents, typed literals (integers,
  decimals and dates), `foaf:knows` and citation graphs, language-tagged titles, and a
  small OWL class hierarchy. Two sizes were measured:
  * **1.05M triples** (N = 100k)
  * **10.5M triples** (N = 1M, 1.12 GB of N-Triples)
* **Queries:** `scripts/bench.sh` has 28 queries covering scans, counts, joins,
  OPTIONAL, property paths, subqueries, grouping, sorting, point lookups and expressions
  in BIND, ORDER BY and aggregate arguments.

* **Engines:** no engine was allowed to answer from a result cache.
  * Sparkles at commit `98a75c1a`, a release build of `sparkles-server` with its default
    features made with rustc 1.99.0 on `forge`: `sparkles serve --result-cache-mb 0`, with
    every other setting at its default.
  * Jena: loaded with `tdb2.tdbloader` 6.2.0 and served by Fuseki 5.1.0 with `-Xmx8G`.
    5.1.0 is the Fuseki version that nixpkgs packages. Fuseki has no result cache.
  * QLever 0.5.48: `-m 8G -j 16 --cache-max-size-single-entry 0B`. The last setting
    stops QLever from caching results, which `--cache-max-num-entries 0` does not. Its
    cache is also cleared before every timed run.
  * Fluree 4.2.2 (BUSL-1.1): the release binary, which the script downloads and
    verifies against a checksum.
    * Load: `fluree create bench --from data.nt --chunk-size-mb 16`, a parallel bulk
      import.
    * Server: `fluree server run` with `FLUREE_CACHE_MAX_MB=4096` and
      `FLUREE_PATH_MAX_VISITED=20000000`. The second setting is needed because the
      default traversal cap of 1M nodes rejects `knows-reach` at 10M.
    * No result cache.
  * Oxigraph 0.5.11: loaded with `oxigraph load` and then `oxigraph optimize`, and served
    by `oxigraph serve --timeout-s 600`. `optimize` is the compaction that Oxigraph's
    loader recommends before read-heavy workloads, and the load time includes both steps.
    No result cache.
* **Machine:** `forge`, an Intel Core i5-13500 with 6 performance cores (12 threads, CPUs
  0–11) and 8 efficiency cores (CPUs 12–19). It has 16 GiB of RAM, of which 15 GiB is
  usable, and an NVMe drive. It runs NixOS with Linux 6.18. Every engine comes from the
  same nixpkgs revision, except Fluree, which is the release binary. The OS page cache is
  warm except in the cold runs.
* **Isolation:** each engine ran alone on the machine, one after another, with nothing
  else heavy running. Every server ran in a systemd scope with a 12 GiB memory limit and
  no swap (`SERVER_MEM_MAX=12G`), so an engine that runs out of memory is killed on its
  own.
* **Method:** timings use hyperfine 1.20.
  * Queries are SPARQL protocol POSTs with TSV results. Each query gets 2 warm-ups and
    10 runs at 1.05M, and 1 warm-up and 5 runs at 10.5M.
  * Times are mean ± σ in ms. They include starting a `curl` process and the HTTP round
    trip, which together take about 3.5 ms. A query that reports 3.5–4 ms did almost no
    work.
  * The client is pinned to one CPU. hyperfine and its `curl` processes run on CPU 11,
    the last performance core. The servers are not pinned. Starting a `curl` process
    takes 3.5 to 9 ms of CPU depending on the core it lands on, which is more than most
    small queries take. Pinning reduces that source of variation. Measurements with
    unpinned clients, identified in their sections, are not directly comparable.

  * Loads: 1 run each, with peak RSS from GNU time.
  * Answers are checked before timing. `scripts/bench-answers.py` fetches every
    engine's answer to every query as SPARQL JSON and fingerprints it by:
    * the row count;
    * the solution multiset by RDF term identity;
    * the same multiset with numeric literals compared by value, to 12 significant
      digits. Engines print the non-terminating averages of `group-avg` to different
      precisions.

    An engine whose answer differs *in value* from the majority is marked † and not
    ranked. That happened only to QLever, on `types-grouped` after the update churn at
    both sizes. A ‡
    marks equal values returned as different RDF terms. `export-500k` uses LIMIT without
    ORDER BY, so only its row count is checked.
  * Update latency is measured with a single-triple `INSERT DATA` into a named graph.
    An untimed `DELETE DATA` runs before every timed request, so each request performs
    a real insertion. An ASK before and after checks that the change is visible. Each
    engine uses its default durability:
    * Sparkles fsyncs its WAL before acknowledging;
    * TDB2 commits durably;
    * QLever keeps updates in memory only;
    * Fluree uses its default commit path;
    * Oxigraph commits a RocksDB transaction with RocksDB's default write options,
      which write the WAL but do not fsync it.
  * Throughput: 160 `star-join` requests from 16 parallel `curl` clients, which are not
    pinned.
  * Memory: the server's RSS 2 s after the throughput run, and its peak during that run.
    Each server's peak is also reset before every query of the answer check, which gives
    the rise in peak RSS per query.

### Coverage and failures

The comparison covers the default, `updates`, `mixed` and `cold` modes of
`scripts/bench.sh` at both sizes, plus WatDiv and full-text search. Cache-size and
execution-feature comparisons cover Sparkles alone. Each comparative run loads its
own data. The `mixed` mode uses `oha`; the `cold` mode uses curl's request timer,
which excludes process startup. Those two modes do not use the pinned client.

Two queries are excluded at 10.5M:

* Fluree's `optional-chain` exceeds the 12 GiB memory limit.
* Fuseki's `knows-reach` overflows its stack and leaves TDB2's node cache wedged,
  causing later queries to hang.

At 1.05M, Fuseki's `knows-reach` fails with the same error but leaves the server usable,
so it is reported as an error.

## Results: 1.05M triples

At this size most queries take 3.5–10 ms, and the request itself takes about 3.5 ms of
that.

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| **load** (s) | **0.49** | 4.78 | 1.51 | 1.27 | 1.28 |
| count-all | **3.6 ± 0.1** | 173.7 ± 19.8 | 10.7 ± 0.9 ‡ | 3.7 ± 0.2 | 256.3 ± 3.8 |
| types-grouped | **3.6 ± 0.1** | 77.4 ± 8.0 | 5.9 ± 0.3 ‡ | 19.0 ± 1.0 | 42.9 ± 2.2 |
| star-join | **7.5 ± 0.2** | 30.6 ± 1.7 | 33.5 ± 3.6 ‡ | 22.5 ± 1.8 | 388.9 ± 4.8 |
| two-hop-count | **5.2 ± 0.2** | 373.2 ± 2.6 | 19.1 ± 2.9 ‡ | 7.1 ± 0.3 | 827.3 ± 5.7 |
| range-topk | 7.9 ± 0.9 ‡ | 95.5 ± 2.5 | **6.4 ± 0.3** | 24.6 ± 1.2 | 57.3 ± 2.2 |
| optional-count | **4.6 ± 0.2** | 80.5 ± 4.9 | 10.1 ± 0.6 ‡ | 15.8 ± 0.7 | 109.6 ± 5.8 |
| contains | **4.2 ± 0.2** | 43.8 ± 2.7 | 75.4 ± 3.8 ‡ | 8.6 ± 0.2 | 147.9 ± 7.0 |
| group-avg | **10.3 ± 0.2** | 207.7 ± 3.3 ‡ | 14.2 ± 0.9 ‡ | 58.6 ± 3.8 ‡ | 308.6 ± 5.6 |
| path-plus | **3.5 ± 0.1** | 6.6 ± 0.6 | 6.2 ± 0.4 ‡ | 7.5 ± 0.2 | 4.0 ± 0.2 |
| distinct-obj | **3.6 ± 0.1** | 90.7 ± 3.2 | 18.0 ± 2.9 ‡ | 3.7 ± 0.1 | 70.6 ± 2.6 |
| export-500k | **122.7 ± 16.0** | 500.8 ± 17.5 | 778.5 ± 16.6 | 245.2 ± 1.9 | 1708.6 ± 3.8 |
| predicate-counts | **3.5 ± 0.1** | 326.6 ± 2.1 | 14.6 ± 0.4 ‡ | 67.4 ± 1.4 | 288.1 ± 3.5 |
| order-by-full | **54.2 ± 6.7** | 543.5 ± 6.6 | 138.6 ± 4.0 | 106.0 ± 0.8 | 3835.0 ± 27.9 |
| optional-chain | **14.4 ± 0.3** | 226.1 ± 4.6 | 63.8 ± 5.2 | 1829.5 ± 85.9 | 362.1 ± 3.9 |
| minus | **4.0 ± 0.2** | 41.7 ± 2.6 | 7.1 ± 0.2 ‡ | 11.9 ± 3.8 | 33.6 ± 4.3 |
| not-exists | **4.5 ± 0.1** | 56.5 ± 1.2 | 7.1 ± 0.3 ‡ | 20.5 ± 1.4 | 111.6 ± 7.0 |
| exists-join | **5.0 ± 0.2** | 161.3 ± 37.7 | 10.1 ± 0.6 ‡ | 31.9 ± 0.4 | 1458.5 ± 18.9 |
| subquery-agg | **3.9 ± 0.1** | 51.1 ± 1.3 | 5.4 ± 0.2 ‡ | 20.4 ± 1.1 | 25.3 ± 3.4 |
| regex-iri | **4.5 ± 0.1** | 58.5 ± 6.6 | 72.2 ± 2.6 ‡ | 88.2 ± 0.6 | 173.9 ± 7.8 |
| knows-reach | **15.8 ± 0.3** | error ¹ | 62.0 ± 6.1 ‡ | 61.4 ± 10.7 | 342.8 ± 6.5 |
| distinct-join | **8.4 ± 0.1** | 412.3 ± 3.4 | 23.6 ± 4.3 | 91.1 ± 1.1 | 672.9 ± 7.3 |
| lang-filter | **4.3 ± 0.2** | 31.8 ± 3.2 | 42.8 ± 1.9 ‡ | 6.4 ± 0.1 | 86.2 ± 6.6 |
| star-lookup | 3.8 ± 0.2 ‡ | 5.8 ± 0.4 ‡ | 10.4 ± 0.3 ‡ | **3.7 ± 0.2** | 4.9 ± 0.3 |
| values-star | **3.6 ± 0.2** | 6.7 ± 0.4 | 10.8 ± 0.7 ‡ | 4.1 ± 0.1 | 4.2 ± 0.2 |
| employee-docs | **3.6 ± 0.1** | 6.9 ± 0.5 | 7.1 ± 0.6 | 3.8 ± 0.2 | 4.4 ± 0.1 |
| expr-bind-group | **4.6 ± 0.3** | 48.1 ± 1.1 ‡ | 7.9 ± 0.5 ‡ | 44.4 ± 0.2 | 46.2 ± 4.1 |
| expr-order-key | **5.9 ± 0.5** | 89.1 ± 1.1 | 14.2 ± 0.6 ‡ | 114.8 ± 0.3 | 2672.4 ± 18.4 |
| expr-agg-arg | **8.3 ± 0.3** | 166.1 ± 0.9 | 14.4 ± 1.0 ‡ | 75.9 ± 2.8 | 298.7 ± 9.2 |
| **update** (1-triple INSERT DATA) | 3.8 ± 0.1 | 32.6 ± 3.6 | 3.8 ± 0.1 | 5.6 ± 0.5 | **3.7 ± 0.1** |
| **throughput** star-join, 16 clients (queries/s) | **1239** | 113 | 462 | 246 | 25 |

## Results: 10.5M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| **load** (s) | **3.68** | 45.57 | 9.69 | 11.07 | 14.13 |
| count-all | **3.8 ± 0.3** | 1621.1 ± 16.0 | 53.3 ± 2.7 ‡ | 3.9 ± 0.3 | 2497.6 ± 18.9 |
| types-grouped | **3.8 ± 0.2** | 5208.4 ± 285.4 | 6.6 ± 1.1 ‡ | 113.4 ± 9.3 | 381.2 ± 3.6 |
| star-join | **31.4 ± 0.6** | 251.7 ± 17.1 | 115.1 ± 7.1 ‡ | 142.2 ± 4.6 | 4710.7 ± 22.6 |
| two-hop-count | **17.2 ± 0.5** | 3845.8 ± 30.9 | 61.3 ± 5.5 ‡ | 36.3 ± 0.5 | 8292.3 ± 59.6 |
| range-topk | **16.4 ± 1.3** | 897.4 ± 30.3 | 22.4 ± 2.6 | 153.1 ± 0.9 | 596.1 ± 3.0 |
| optional-count | **10.7 ± 0.2** | 1326.0 ± 161.4 | 45.8 ± 7.7 ‡ | 114.7 ± 5.4 | 992.3 ± 9.8 |
| contains | **7.0 ± 0.2** | 3811.0 ± 60.2 | 690.9 ± 3.5 ‡ | 17.7 ± 0.3 | 1534.9 ± 13.5 |
| group-avg | **71.5 ± 2.2** | 4252.3 ± 382.6 ‡ | 93.4 ± 5.2 ‡ | 549.8 ± 9.5 ‡ | 4057.6 ± 59.3 |
| path-plus | **3.7 ± 0.2** | 6.3 ± 0.2 | 10.3 ± 0.4 ‡ | 54.6 ± 3.5 | 4.1 ± 0.2 |
| distinct-obj | **3.8 ± 0.3** | 2103.1 ± 675.4 | 205.4 ± 5.5 ‡ | 3.9 ± 0.2 | 746.9 ± 2.0 |
| export-500k | **107.2 ± 6.8** | 734.4 ± 84.6 | 661.2 ± 3.1 | 260.4 ± 1.3 | 2087.7 ± 6.6 |
| predicate-counts | **3.8 ± 0.3** | 3266.4 ± 28.4 | 74.1 ± 3.5 ‡ | 608.3 ± 6.0 | 2798.3 ± 5.3 |
| order-by-full | **365.4 ± 12.8** | 13852.8 ± 730.3 | 1421.4 ± 10.9 | 1913.6 ± 7.7 | 55456.3 ± 163.5 |
| optional-chain | **78.9 ± 4.3** | 3825.0 ± 346.9 | 571.2 ± 23.4 | excluded ² | 4764.9 ± 72.5 |
| minus | **8.7 ± 0.3** | 2297.5 ± 246.6 | 20.0 ± 2.5 ‡ | 56.9 ± 2.1 | 349.6 ± 12.5 |
| not-exists | **14.5 ± 0.7** | 578.6 ± 141.0 | 22.5 ± 2.2 ‡ | 164.1 ± 2.0 | 1032.3 ± 4.5 |
| exists-join | **14.8 ± 0.7** | 1999.2 ± 290.0 | 66.7 ± 6.6 ‡ | 276.2 ± 2.3 | 122436.9 ± 245.5 |
| subquery-agg | **5.0 ± 0.3** | 1771.5 ± 307.8 | 14.6 ± 0.8 ‡ | 146.7 ± 1.1 | 183.6 ± 2.3 |
| regex-iri | **8.4 ± 0.3** | 1611.5 ± 21.5 | 659.4 ± 1.6 ‡ | 873.5 ± 26.4 | 2642.1 ± 34.5 |
| knows-reach | **145.2 ± 2.0** | excluded ¹ | 1157.6 ± 10.0 ‡ | 658.3 ± 13.8 | 4651.3 ± 88.4 |
| distinct-join | **49.0 ± 1.6** | 4468.2 ± 84.5 | 155.8 ± 8.1 | 1207.0 ± 59.2 | 6704.7 ± 31.7 |
| lang-filter | **6.3 ± 0.2** | 2734.7 ± 189.2 | 389.2 ± 3.2 ‡ | 28.6 ± 1.4 | 839.1 ± 11.2 |
| star-lookup | **4.0 ± 0.2** ‡ | 7.2 ± 0.3 | 24.3 ± 2.8 ‡ | 4.3 ± 0.3 | 5.4 ± 0.4 |
| values-star | **4.0 ± 0.2** | 8.2 ± 2.0 | 11.8 ± 1.1 ‡ | 4.8 ± 0.7 | 4.3 ± 0.2 |
| employee-docs | **4.0 ± 0.2** | 7.7 ± 0.5 | 12.5 ± 0.8 | 4.7 ± 0.3 | 4.7 ± 0.3 |
| expr-bind-group | **9.3 ± 0.6** | 426.7 ± 11.6 ‡ | 34.1 ± 3.6 ‡ | 397.2 ± 1.9 | 354.4 ± 1.5 |
| expr-order-key | **22.3 ± 2.0** | 937.2 ± 18.3 | 109.4 ± 4.0 ‡ | 1120.8 ± 27.1 | 38266.9 ± 54.8 |
| expr-agg-arg | **47.6 ± 2.0** | 1789.4 ± 2.6 | 98.7 ± 6.9 ‡ | 767.0 ± 4.3 | 3899.0 ± 22.3 |
| **update** (1-triple INSERT DATA) | **4.2 ± 0.1** | 32.4 ± 5.5 | 4.2 ± 0.2 | 4.5 ± 0.1 | 4.3 ± 1.4 |
| **throughput** star-join, 16 clients (queries/s) | **243** | 11 | 92 | 22 | 1.7 |

‡ Same values, different RDF terms. QLever returns integers such as counts and ages as
`xsd:int`, where the data and the other engines use `xsd:integer`. Sparkles writes
`xsd:decimal` values in XSD 1.1 canonical form, so the data's `"175000.50"` comes back as
`"175000.5"`. In `group-avg`, the averages agree to 12 significant digits. Sparkles and
Oxigraph share the `oxsdatatypes` decimal type and print the same digits. The other
engines print fewer or more.

1. Fuseki 5.1 fails `knows-reach` (`<person/0> foaf:knows* ?x`) with a
   `StackOverflowError` in TDB2's node cache. At 10.5M the failure also left the cache
   wedged, so `knows-reach` is left out of Jena's 10.5M runs.
2. Fluree's `optional-chain` at 10.5M grows past the 12 GiB memory limit, so it is left
   out of Fluree's 10.5M runs.

### Memory

These are the server's resident set (VmRSS, and VmHWM for peaks) and each bulk load's
peak RSS. Lower is better.

| 1.05M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 51 | 257 | 28 | 52 | **22** |
| server RSS after every query ran once (MiB) | 189 | 981 | 146 | 2083 | **91** |
| server RSS after the run (MiB) | 403 | 1769 | **307** | 2735 | 1244 |
| peak RSS during the throughput run (MiB) | 619 | 1769 | **335** | 2735 | 1250 |
| Sparkles RSS after the run less its 64 MiB block cache (MiB) | 339 | — | — | — | — |
| bulk load peak RSS (MiB) | **363** | 2188 | 660 | 475 | 738 |
| index size on disk | 27 MiB | 132 MiB | **26 MiB** | 37 MiB | 147 MiB |

| 10.5M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 47 | 270 | 43 | 260 | **28** |
| server RSS after every query ran once (MiB) | 731 | 3409 | 414 | 3377 | **408** |
| server RSS after the run (MiB) | 916 | 4899 | **653** | 4664 | 2381 |
| peak RSS during the throughput run (MiB) | 1394 | 4899 | **778** | 4756 | 2381 |
| Sparkles RSS after the run less its 379 MiB block cache (MiB) | 537 | — | — | — | — |
| bulk load peak RSS (MiB) | 2112 | 4115 | **1931** | 3142 | 6204 |
| index size on disk | 286 MiB | 1.3 GiB | **276 MiB** | 398 MiB | 1.4 GiB |

`scripts/rss-probe.sh` measures memory retention on a fresh Sparkles server. It runs
every query once, then 3 rounds of 160 concurrent `star-join` requests, and reads RSS 2 s
after each step. At 10.5M, RSS was 729 MiB after the queries and 890 MiB after the
concurrent rounds, with a peak of 1,148 MiB. The block cache held 378 MiB of that. At
1.05M, the figures were 191 MiB, 383 MiB and a peak of 407 MiB, with 64 MiB in the block
cache.

The peak rise per query shows what materialization costs. At 10.5M, Sparkles' largest
rises were `order-by-full` at 259 MiB, `two-hop-count` at 246 MiB, `optional-chain` at
183 MiB and `star-join` at 171 MiB. QLever's largest were `distinct-join` at 135 MiB and
`knows-reach` at 124 MiB, and it rose 43 MiB on `order-by-full` and 34 MiB on
`star-join`. Part of Sparkles' rise is the block cache filling. With the cache off,
`star-join` rose 46 MiB and `two-hop-count` 132 MiB, while `order-by-full` still rose 299
MiB.

## Memory and the speed it buys

Sparkles trades memory for speed by default. These are the defaults that do it:

* **The decoded-block cache** keeps decompressed index blocks, up to 1 GiB per dataset
  (`--cache-mb`). It is the largest part of the RSS gap with QLever.
* **The query result cache** keeps answers to repeated queries, up to 512 MiB per dataset
  (`--result-cache-mb`). It was off in every run on this page, so none of these numbers
  include it. Under a workload that repeats queries, it adds up to that much RSS.
* **Materialized intermediate results.** Every operator materializes its result, within
  the query's row and memory budgets, where QLever streams. This shows as the per-query
  rises above and as a higher peak under concurrent load, 1,394 MiB against QLever's 778
  MiB at 10.5M.
* **mimalloc** keeps freed memory in per-thread heaps. The server hands free memory back
  to the OS after it has been idle for a second (`--idle-release-ms`, default 1000).

The cache-size runs measured the 10.5M suite with the block cache at 0, 256 MiB and
4 GiB, on the same build and data. The 1 GiB row is the main 10.5M run above.

| Block cache (`--cache-mb`) | RSS at start | RSS after every query once | Peak RSS under throughput | RSS after the run | Held by the cache | Throughput (q/s) | Sum of the 28 query means |
|---|---:|---:|---:|---:|---:|---:|---:|
| 0 | 48 MiB | 141 MiB | 897 MiB | **331 MiB** | 0 | 123 | 3,638 ms |
| 256 MiB | 48 MiB | 536 MiB | 1,256 MiB | 791 MiB | 255 MiB | 238 | 1,088 ms |
| 1 GiB (default) | 47 MiB | 731 MiB | 1,394 MiB | 916 MiB | 379 MiB | **243** | **1,067 ms** |
| 4 GiB | 47 MiB | 706 MiB | 1,354 MiB | 903 MiB | 379 MiB | 240 | 1,088 ms |

* **No cache** saves 585 MiB of RSS against the default and brings it below QLever's
  653 MiB. It costs half the throughput, 123 against 243 q/s, and makes the suite 3.4×
  slower in total. `knows-reach` becomes 17× slower, 2,527 ms against 145 ms, which is
  slower than QLever (1,158 ms) and Fluree (658 ms). `star-lookup` becomes 5.7× slower,
  `employee-docs` 3.0×, `optional-count`, `star-join` and `exists-join` about 1.8× and
  `range-topk` 1.5×.
* **256 MiB** is about as fast as the default on this data and saves 125 MiB of RSS.
  `star-lookup` was slower, 6.3 against 4.0 ms, and `expr-bind-group` 11.5 against
  9.3 ms.
* **4 GiB** changes nothing at this size, because the suite touches only 379 MiB of
  blocks. Its `expr-order-key` took 29.3 against 22.3 ms, but that query varied from 21
  to 32 ms across repeated measurements, so this difference falls within that spread.
  A larger dataset fills more of the cache, so the default buys more there and costs
  more RSS.

At 10.5M, about 41% of Sparkles' RSS after the run is block cache, and the rest is
working memory and allocator retention. Less its block cache, the default server's RSS
after the run is 537 MiB, which is lower than QLever's 653 MiB, and a server without a
cache ends at 331 MiB. Setting `--cache-mb 256` keeps most of the speed on this data for
125 MiB less. Setting `--cache-mb 0` gives the smallest footprint at about half the
throughput.

### Allocator choice (2026-09-30, laptop)

These measurements ran on the laptop described under
[Other measurements](#other-measurements), with an unpinned client. Allocator retention,
preallocation of scan outputs from index counts, and idle heap release affect RSS as
well as throughput. Each configuration used a fresh 10.5M server; RSS includes the
block cache.

| Allocator and memory settings (laptop, 2026-09-30) | after the queries | after the concurrent rounds | peak |
|---|---:|---:|---:|
| glibc malloc | 701–704 MiB | 1592–1727 MiB | 1845 MiB |
| glibc, exact scan reservation | 675 MiB | 1426 MiB | 1430 MiB |
| glibc, reservation + idle release (`malloc_trim`) | 644 MiB | 838 MiB | 1439 MiB |
| **mimalloc, reservation + idle release (default build)** | 765 MiB | 920 MiB | 1682 MiB |

mimalloc kept more memory than glibc with idle release, but it was faster. In 10.5M runs
of Sparkles alone, the mimalloc build reached 190 q/s against 144 for glibc. `star-join`
took 38 against 72 ms, `optional-count` 33 against 55 ms, `minus` 27 against 42 ms and
`two-hop-count` 65 against 96 ms, and the load took 4.95 against 5.5 s. jemalloc, and
glibc with fixed mmap thresholds, were measured too. Both were slower.

## Where Sparkles loses

Sparkles is the fastest of the five engines on 26 of 28 queries at 1.05M and on all 28
at 10.5M. Against each engine:

* **vs. QLever:** Sparkles wins 27 of 28 at 1.05M (median 2.7×) and 28 of 28 at 10.5M
  (median 3.8×). QLever wins `range-topk` at 1.05M by 1.24× (6.4 against 7.9 ms).
* **vs. Fluree:** Sparkles wins 27 of 28 at 1.05M (median 3.3×) and all 27 that Fluree
  runs at 10.5M (median 7.7×). Fluree wins `star-lookup` at 1.05M by 1% (3.74 against
  3.78 ms), which is a tie. Three more queries at 1.05M and three at 10.5M are within
  10%, which this page counts as ties.
* **vs. Jena/Fuseki:** Sparkles wins every query. It is 1.5–92× faster at 1.05M and
  1.7–1,379× faster at 10.5M. Fuseki fails `knows-reach`.
* **vs. Oxigraph:** Sparkles wins all 28 at both sizes, by a median of 24× at 1.05M and
  87× at 10.5M. On the point lookups `path-plus`, `values-star`, `employee-docs` and
  `star-lookup`, the margin is only 1.08–1.36×.

### Specific benchmarks

| Case | Loses to | By | Likely cause |
|---|---|---|---|
| `range-topk` at 1.05M | QLever | 1.24× (7.9 against 6.4 ms) | The 10% of salaries written in non-canonical form, such as `"175000.50"`, are stored as vocabulary literals, which are read and tested. Sparkles is 1.37× faster at 10.5M (16.4 against 22.4 ms). |
| `star-lookup` at 1.05M | Fluree | 1.01× (3.78 against 3.74 ms), a tie | Both engines answer with a few index lookups. The time is mostly the request itself. |
| `count-all` and `distinct-obj` at both sizes, `employee-docs` at 1.05M, `star-lookup` at 10.5M | Fluree | ties within 10%, 1.04–1.07× | Both engines answer from index statistics or a few index lookups. The times are 3.6–4.3 ms, which is mostly the request itself. |
| `values-star` at 10.5M | Oxigraph | a tie within 10%, 1.08× | The same. Oxigraph follows a few lookups quickly. |
| Update latency, 1 triple, at 1.05M | Oxigraph, QLever | 1.03× and 1.01× (3.83 against 3.71 and 3.79 ms), ties | Sparkles fsyncs its WAL and publishes a new snapshot before it acknowledges. Oxigraph does not fsync, and QLever keeps updates in memory only. At 10.5M Sparkles is the fastest, 4.17 ms against QLever's 4.19 and Oxigraph's 4.27. |
| Commit rate | Oxigraph, and QLever at 10.5M | 5,000 single-triple commits ran at 664 commits/s at 1.05M and 416 at 10.5M, against 855 and 1,140 for Oxigraph and 475 for QLever at 10.5M. | Each commit waits for an fsync. Sparkles' median commit took 1.4 ms at 1.05M and 1.9 ms at 10.5M ([Commit latency](#commit-latency)). |
| Writes under concurrent reads | Oxigraph, and Fluree at 10.5M | In the mixed run at 10.5M, Sparkles committed 202 writes/s against Oxigraph's 677 and Fluree's 299. At 1.05M it committed 289 against Oxigraph's 1,247. | The writer's fsyncs compete with 16 readers that Sparkles serves 2.8–4× faster than QLever does. Oxigraph does not fsync. See [Updates and mixed load](#updates-and-mixed-load). |
| Queries after 5,000 commits | Fluree, QLever | At 10.5M, `two-hop-count` 1.03× to Fluree, and `count-all` and `distinct-obj` within 2%. At 1.05M, `range-topk` 1.14× to QLever, `two-hop-count` 1.10× and `star-lookup` 1.06× to Fluree, and `distinct-obj` within 1%. | Reads merge the in-memory delta until a compaction folds it into the index. See [Updates and mixed load](#updates-and-mixed-load). |
| Queries after a restart with cold caches | Oxigraph, Fluree, QLever | Up to 3.6× on point lookups, and `star-join`, `contains`, `lang-filter` and `employee-docs` at 1.05M. See [Cold starts](#cold-starts). | Sparkles opens and decodes index blocks on first use. |
| Server RSS after the run | QLever | 1.31× at 1.05M (403 against 307 MiB), 1.40× at 10.5M (916 against 653 MiB) | The decoded-block cache and materialized intermediates. Less the block cache, the figures are 339 and 537 MiB, and a 10.5M server with the cache off ends at 331 MiB. See [Memory and the speed it buys](#memory-and-the-speed-it-buys). |
| Server RSS after the DBpedia run | QLever | 3.1× (4,448 against 1,426 MiB) | Memory-mapped index pages that a query touched count toward Sparkles' RSS, 2,678 MiB here, and the full 1 GiB block cache is in it too. Less both, Sparkles held about 750 MiB. See [DBpedia at 1.24 billion triples](#dbpedia-at-124-billion-triples). |
| DBpedia cold queries | QLever | 5 of 31 queries. QLever was 2.5× faster on `geo-box`, 1.9× on `label-regex`, 1.4× on `country-population` and 1.1× on `people-no-birthdate` and `outlink-classes-2`. | The cold-read change reads only the pages that a query asks for, which slows queries that read many pages. With it off, `label-regex` took 67 ms cold, faster than QLever's 71 ms. |
| DBpedia `geo-box` warm | QLever | a tie, 1.01× (145.7 against 143.8 ms) | In the first of the two passes, Sparkles was faster on `geo-box` (139.0 ms), and QLever was 3% faster on `entity-facts-2`. |
| Bulk load peak RSS at 10.5M | QLever | 1.09× (2,112 against 1,931 MiB) | Parallel parsing and in-memory sorting during the load. Sparkles loads 2.6× faster. |
| Full-text `common-count` at 10.5M | QLever | 1.07× (14.5 against 13.5 ms) | Counting all 49,837 hits of a common word. |
| Full-text `highlight` at both sizes | QLever | 1.09× at 1.05M (15.0 against 13.7 ms), 1.08× at 10.5M (102.7 against 94.8 ms) | QLever has no highlighting and returns the plain literals, while Sparkles marks the matched words in all 49,837 of them. |
| Full-text index size | QLever | 1.24× at 10.5M (123.4 against 99.9 MB) | QLever's index covers every literal and is still smaller. Sparkles builds its index 3.2× faster. |
| WatDiv `S7` | Fluree | a tie, 1.01× (3.92 against 3.90 ms) | All five instances return no rows, and the time is mostly the request itself. See [WatDiv](#watdiv). |
| Large results | QLever / Jena (in principle) | — | Sparkles streams responses over 1 MiB as they are serialized, but it materializes the result before serializing. `export-500k` is still fastest in Sparkles at both sizes. |

Where Sparkles beats QLever at 10.5M, the margin is often wide. It is 98× on `contains`,
79× on `regex-iri`, 62× on `lang-filter`, 55× on `distinct-obj`, 19× on
`predicate-counts`, 14× on `count-all`, 8.0× on `knows-reach`, 7.2× on `optional-chain`,
6.2× on `export-500k` and 6.1× on `star-lookup`. The closest are `group-avg` at 1.31×,
`range-topk` at 1.37× and `not-exists` at 1.55×.

Fluree is fast on single-pattern scans, counts and point lookups. It is much slower on
general joins, OPTIONAL, subqueries, grouping, sorting and expressions:

| Query | Fluree slower by (1.05M / 10.5M) |
|---|---|
| `optional-chain` | 127× (1.83 s) / runs out of memory |
| `predicate-counts` | 19× / 158× |
| `regex-iri` | 20× / 104× |
| `expr-order-key` | 19× / 50× |
| `expr-bind-group` | 9.8× / 43× |
| `types-grouped` | 5.3× / 30× |
| `subquery-agg` | 5.3× / 29× |
| `distinct-join` | 11× / 25× |
| `exists-join` | 6.4× / 19× |
| `path-plus` | 2.1× / 15× |
| `group-avg` | 5.7× / 7.7× |

Fluree's throughput is lower as well, 246 against 1,239 q/s at 1.05M and 22 against 243
at 10.5M. At 1.05M its server held 6.8× the memory after the run (2,735 against 403 MiB).

Jena/Fuseki wins no query. Its one advantage is that it streams results.

Oxigraph follows a single path or a few lookups as fast as Sparkles, within 1.08–1.36×.
Every query that joins, groups, sorts or counts over many rows is one to three orders of
magnitude slower than in Sparkles, and the gap grows with the data:

| Query | Oxigraph slower by (1.05M / 10.5M) |
|---|---|
| `exists-join` | 293× / 8,295× (122 s) |
| `expr-order-key` | 450× / 1,720× |
| `predicate-counts` | 82× / 729× |
| `count-all` | 72× / 665× |
| `two-hop-count` | 159× / 483× |
| `order-by-full` | 71× / 152× |
| `star-join` | 52× / 150× |
| `distinct-join` | 80× / 137× |
| `export-500k` | 14× / 19× |

Oxigraph's throughput is 25 q/s at 1.05M and 1.7 q/s at 10.5M, against 1,239 and 243 for
Sparkles. Its server used 1.2 GiB and 2.3 GiB after the runs. Its update latency is the
lowest at 1.05M, 3.71 ms, because it does not fsync each commit. Its load, including
`optimize`, takes 1.28 s at 1.05M and 14.1 s at 10.5M.

## Sparkles HTTP latency

These Sparkles-only measurements ran on `forge` on 2026-10-05, with Rust 1.99.0,
a release build and default features. The binary's SHA-256 is
`84196c31e41ec257a4a1a405c34071f7e21243c07e01671b26db0e5dd47e1863`.
The result cache was off (`--result-cache-mb 0`), and the decoded-block cache used
its default size. The client ran on CPU 11 and the server on CPUs 0–9.

Each query had 30 warm-ups and 20 timed requests in each of two fresh server
processes. The table gives the pooled median of 40 samples in milliseconds, including
HTTP and the full TSV response, with a warm page cache. It excludes starting a
`curl` process, so these numbers are not directly comparable with the hyperfine
means in the five-engine tables. The datasets contain 1,052,801 and 10,527,319 triples.
All query answers matched the checked reference results.

| Query | 1.05M (ms) | 10.5M (ms) |
|---|---:|---:|
| `contains` | 0.943 | 4.950 |
| `count-all` | 0.203 | 0.206 |
| `distinct-join` | 4.220 | 43.360 |
| `distinct-obj` | 0.346 | 0.224 |
| `employee-docs` | 0.590 | 0.501 |
| `exists-join` | 2.558 | 8.837 |
| `export-500k` | 131.063 | 132.036 |
| `expr-agg-arg` | 5.711 | 41.986 |
| `expr-bind-group` | 1.806 | 4.548 |
| `expr-order-key` | 3.944 | 15.964 |
| `group-avg` | 4.521 | 48.654 |
| `knows-reach` | 11.339 | 139.745 |
| `lang-filter` | 0.819 | 3.448 |
| `minus` | 0.690 | 4.154 |
| `not-exists` | 1.610 | 9.732 |
| `optional-chain` | 10.845 | 84.143 |
| `optional-count` | 0.880 | 6.315 |
| `order-by-full` | 39.052 | 430.709 |
| `path-plus` | 0.219 | 0.231 |
| `predicate-counts` | 0.221 | 0.220 |
| `range-topk` | 3.742 | 9.552 |
| `regex-iri` | 1.193 | 7.321 |
| `star-join` | 3.153 | 27.779 |
| `star-lookup` | 0.795 | 0.373 |
| `subquery-agg` | 0.868 | 0.782 |
| `two-hop-count` | 1.275 | 11.146 |
| `types-grouped` | 0.287 | 0.269 |
| `values-star` | 0.378 | 0.438 |

## Updates and mixed load

The `updates` mode copies each engine's store, commits the same 5,000 single-triple
`INSERT DATA` and `DELETE DATA` requests to it, one request each, and then runs the full
query suite on the changed store. In Sparkles the commits stay in the in-memory delta,
since 5,000 quads is below the threshold for an automatic compaction.

| Churn of 5,000 commits | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: commits/s | 664 | 14 | 426 | 499 | **855** |
| 1.05M: latency p50 / p99 (ms) | 1.42 / 2.52 | 74.06 / 102.20 | 2.27 / 4.57 | 1.68 / 7.16 | **1.16 / 2.04** |
| 10.5M: commits/s | 416 | 14 | 475 | 459 | **1140** |
| 10.5M: latency p50 / p99 (ms) | 1.94 / 6.70 | 74.69 / 103.16 | 2.07 / 4.73 | 1.55 / 12.76 | **0.90 / 1.58** |

No engine failed a commit. After the churn, Sparkles was still the fastest on 24 of 28
queries at 1.05M and 25 of 28 at 10.5M. At 1.05M it lost `range-topk` to QLever (1.14×),
`two-hop-count` (1.10×) and `star-lookup` (1.06×) to Fluree, and `distinct-obj` to Fluree
by 0.2%, a tie. At 10.5M it lost `two-hop-count` to Fluree (1.03×), and Fluree was faster
by 2% or less on `count-all` and `distinct-obj`, which are ties. Fluree also came within
10% on `count-all`, `employee-docs` and `values-star` at 1.05M and on `star-lookup` at
10.5M, and Oxigraph on `path-plus` at 10.5M. QLever's answer to `types-grouped` differed
from the majority after the churn at both sizes, so it was not ranked there.

The delta slows Sparkles' joins until the next compaction. At 10.5M, `star-join` went from
31.4 to 58.7 ms, `two-hop-count` from 17.2 to 36.6 ms, `contains` from 7.0 to 16.6 ms and
`regex-iri` from 8.4 to 17.8 ms. Throughput fell from 243 to 166 q/s, still ahead of
QLever's 87. RSS after the run rose from 916 to 1,409 MiB, of which 717 MiB was block
cache. Fluree's rose to 9,190 MiB.

### Commit latency

Sparkles' median commit is 1.4 ms at 1.05M and 1.9 ms at 10.5M. At 10.5M it
commits 416 single-triple transactions per second. Every acknowledged commit is
synced to disk. Oxigraph commits faster without fsync, and QLever is slightly faster
at 10.5M while keeping updates in memory.

The write-ahead log preallocates space by writing and syncing zeros in advance,
starting with 64 KiB and growing up to 4 MiB. `--wal-prealloc-kb` sets that limit;
`0` disables preallocation. Commits overwrite already allocated blocks, reducing
file-system journal work while preserving durability. Replay, readers, backups and
quotas stop at the end of the last commit. A close trims unused space, and recovery
recognizes a torn transaction tail.

The harness flushes copied stores before timing, so pending copy writeback does not
inflate an engine's commit latency.

### Mixed load

The `mixed` mode runs 16 concurrent `star-join` readers with `oha` for 30 s while one
writer commits single-triple `INSERT DATA` requests as fast as it can. Every request opens
a new connection.

| Mixed load | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: reads/s | **2054.2** | 94.5 | 510.6 | 34.6 | 10.9 |
| 1.05M: read p50 / p99 (ms) | **7.6 / 12.0** | 158.4 / 351.3 | 27.4 / 39.7 | 462.2 / 536.7 | 1192.3 / 3351.1 |
| 1.05M: writes/s | 288.8 | 28.3 | 109.4 | 246.5 | **1247.3** |
| 1.05M: write p50 / p99 (ms) | 2.8 / 8.5 | 17.9 / 97.7 | 6.0 / 25.8 | 3.6 / 16.2 | **0.8 / 1.0** |
| 1.05M: failed reads / writes | 0 / 0 | 0 / 0 | 23929 / 827 | 0 / 0 | 0 / 0 |
| 10.5M: reads/s | **195.6** | 7.7 | 69.5 | 3.6 | 0 |
| 10.5M: read p50 / p99 (ms) | **81.8 / 103.7** | 1977.0 / 2859.2 | 229.5 / 270.6 | 4402.7 / 8621.1 | — |
| 10.5M: writes/s | 201.6 | 20.7 | 51.1 | 299.4 | **676.7** |
| 10.5M: write p50 / p99 (ms) | 4.2 / 10.5 | 62.8 / 86.7 | 11.9 / 108.8 | 1.9 / 14.8 | **1.4 / 3.2** |

Sparkles serves 4.0× QLever's reads under a concurrent writer at 1.05M and 2.8× at 10.5M.
Oxigraph commits more writes per second at both sizes, and Fluree does at 10.5M, but
their reads are far slower. At 10.5M no Oxigraph reader finished within the 30 s. QLever
failed 23,929 reads and 827 writes at 1.05M, and its log does not say why.

## Cold starts

The `cold` mode stops the server, evicts the engine's files from the page cache, starts
the server and times one query, three times per query. The tables give the median.

| Cold, median of 3 | 1.05M | 10.5M |
|---|---|---|
| Sparkles fastest | 19 of 28 | 23 of 28 |
| Time to ready after the restart | 0.085 s (Fluree 0.082, QLever 0.084, Oxigraph 0.088, Fuseki 1.23) | 0.084 s (Fluree 0.081, QLever 0.084, Oxigraph 0.093, Fuseki 1.23) |

| Query, cold | Loses to | 1.05M | 10.5M |
|---|---|---|---|
| `star-lookup` | Oxigraph | 1.81× (26.9 against 14.9 ms) | 3.2× (50.4 against 15.7 ms) |
| `values-star` | Oxigraph | 2.5× (13.8 against 5.5 ms) | 2.1× (10.3 against 4.8 ms) |
| `employee-docs` | Fluree at 1.05M, Oxigraph at 10.5M | 2.1× (21.3 against 10.2 ms) | 2.5× (31.6 against 12.8 ms) |
| `path-plus` | Oxigraph | 3.6× (7.1 against 1.9 ms) | 2.5× (4.9 against 1.9 ms) |
| `range-topk` | QLever | 1.25× (28.7 against 23.0 ms) | 1.03× (45.1 against 43.9 ms) |
| `count-all` | Fluree | 2.4× (3.2 against 1.3 ms) | — |
| `star-join` | Fluree | 1.45× (50.4 against 34.9 ms) | — |
| `contains` | Fluree | 1.87× (23.2 against 12.4 ms) | — |
| `lang-filter` | Fluree | 2.0× (17.8 against 8.9 ms) | — |

These are single requests timed by curl without process start, so they are lower than
the warm hyperfine times of small queries. The losses are point lookups, statistics and,
at 1.05M, a few scans, where a cold Sparkles server reads and decodes index blocks for a
few rows. On queries that read many rows at 10.5M, Sparkles stays ahead when cold. Cold
`star-join` took 315 ms against QLever's 374 ms, and cold `order-by-full` 423 ms against
QLever's 1,432 ms.

### Cold reads

Sparkles memory-maps its index and vocabulary files. Linux's read-ahead window is
128 KiB on `forge` and 512 KiB on the laptop. Reading a large window around every
sparse lookup can fetch much more data than the query needs. Three features address
this:

* Random-access mappings and explicit read-ahead request the columns or term ranges
  that a query will read. `SPARKLES_IO_HINTS=off` disables those hints.
* A sparse vocabulary index, `vocab.idx`, holds the first key of every 128th
  front-coded block. It is loaded at open and narrows lookups to one range of offsets
  and data. Read-ahead is requested once per group of 128 blocks. The index costs
  66 KB at 10.5M and 8.4 MB of memory at 1.01B quads. Loads and compactions write it;
  `sparkles vocab-index` adds it to a database without one.
  `SPARKLES_SPARSE_VOCAB=off` disables its use.
* An index join with up to 16,384 keys requests its blocks before decoding the first,
  allowing their reads to overlap (`prefetch_blocks` in
  `SPARKLES_DISABLE_OPTIMIZATIONS`).

The tradeoff is better sparse point lookups versus less automatic read-ahead for
dense scans. These configuration measurements used `forge`, commit `98a75c1a`,
and three cold requests per query. "Both off" sets
`SPARKLES_IO_HINTS=off SPARKLES_SPARSE_VOCAB=off`.

| 10.5M, cold, median of 3, ms | Both off | Both on (default) |
|---|---:|---:|
| `star-lookup` | 63.6 | **49.4** |
| `star-join` | **104.7** | 312.4 |
| `contains` | **47.3** | 54.4 |
| `regex-iri` | **38.1** | 53.2 |
| `lang-filter` | **38.6** | 48.6 |

With all three features enabled, a cold server on the laptop read the following
amounts from disk on 2026-10-02. The load average was 11–65, so byte counts are more
reliable than latency in these measurements.

| 10.5M, first query after a restart, laptop | Read from disk (KiB) |
|---|---:|
| Start, before the first query | 348 |
| `star-lookup` | 6,756 |
| `values-star` | 608 |
| `employee-docs` | 2,588 |
| `range-topk` | 5,716 |
| `distinct-obj` | 228 |
| `path-plus` | 120 |

## DBpedia at 1.24 billion triples

This run compared Sparkles with QLever and Fluree on real data at full scale, English
DBpedia in its release of 2022.12.01. The 38 files of
`scripts/bench-billion/dbpedia-2022.12.tsv` hold 1,237,418,025 lines of N-Triples, 16 GB
compressed with zstd. Some lines repeat a triple, and 1,014,683,275 distinct triples
remain. Sparkles' vocabulary has 205,941,262 terms.

The run used `scripts/bench-billion.sh` with `SCALE=full`, `RUNS=3`, `TIMEOUT=300` and a
query memory budget of 8 GB for Sparkles and QLever (`QUERY_MEM_GB=8`). It ran on `forge`
on 2026-10-03, with each engine alone. Sparkles was at commit `98a75c1a`. QLever was the nixpkgs build of 0.5.48; its load figures are from 2026-10-02 and
its query figures from 2026-10-03. Loads were pinned to the P-cores, CPUs 0–11, and every step ran in a systemd scope capped at 14 GiB with no
swap, with an open-file limit of 65,536. QLever indexed with 8 GB of sort memory and 5
million triples per batch. Each query ran once to warm up and then three times. The cold
runs restarted the server with the engine's files evicted from the page cache and ran
each query once.

Fluree 4.2.2 ran with the settings of the 10.5M runs. Its bulk import had an 8 GiB memory
budget, and its server had a 4 GiB cache and the run's 300 s query limit. Its load had a
limit of 4 hours. Fluree's import fails on a file that is larger than its chunk size,
768 MB with that budget, because it finds no statement boundary where it cuts the file. The
harness therefore splits the 38 files into 370 pieces of at most 512 MB of N-Triples,
which took 106 s and is not part of the load time. The import then loaded all
1,014,683,275 distinct triples in 2,919 s, with a peak RSS of 12.1 GiB, into an index of
69.7 GiB. Each cold run starts a fresh server, and all 31 cold queries completed.
Fluree's warm measurements are incomplete because its server reached the 14 GiB
memory limit. Only four warm queries have timings. Its answers agreed with the other
engines on 27 of the 29 queries it answered warm. On `births-by-decade` it returned
165 rows where Sparkles and QLever returned 171; on `entity-facts-2` it returned the
same values as different RDF terms.

Jena and Oxigraph were not run at this scale. At 10.5M, TDB2's
`tdbloader` took 45.6 s and Oxigraph's load with `optimize` 14.1 s, and their indexes
took 1.3 and 1.4 GiB. The full data has 97 times as many distinct triples. A linear
estimate gives about 75 minutes for TDB2 and 25 minutes for Oxigraph, and indexes of
about 125 and 135 GB. Neither index would fit in the 15 GiB of RAM, so the loads would
take longer than that, and the 250 GB left on the disk would not hold Oxigraph's index
while `optimize` rewrites it. The queries are what rule both engines out. Each query gets
a warm-up, three timed runs and a cold run, each limited to 300 s, so a query that times
out costs 25 minutes. At 10.5M, Fuseki took 1.6 s to count all triples and 3.3 s to count
them per predicate, and Oxigraph 2.5 and 2.8 s. At 97 times the data, both would reach
the limit on `count-all`, `predicate-counts`, `top-linked`, `category-tree` and the other
queries that read a large part of the data, and six such queries alone would take 2.5
hours per engine.

| query | sparkles (ms) | qlever (ms) | fluree (ms) |
|---|---:|---:|---:|
| **load** (s) | **596.14** | 1673.78 | 2919.33 |
| abstract-contains | **43.0 ± 8.8** | 234.2 ± 1.1 ‡ | 6719.1 ± 317.4 |
| births-by-decade | **50.0 ± 4.0** | 137.9 ± 8.3 | 4300.2 ± 88.3 † |
| category-people-1 | **16.1 ± 7.8** | 53.6 ± 3.5 | 63.2 ± 34.1 |
| category-people-2 | **13.0 ± 2.9** | 36.3 ± 8.2 | 49.1 ± 34.3 |
| category-tree | **269.8 ± 4.9** | 2050.1 ± 10.1 ‡ | — ⁴ |
| class-counts | **5.8 ± 0.3** | 99.1 ± 5.8 ‡ | — ⁴ |
| costar-birthplace | **47.5 ± 4.4** | 154.4 ± 5.6 ‡ | — ⁴ |
| count-all | **9.5 ± 1.7** | 2616.8 ± 11.9 † | — ⁴ |
| country-population | **11.3 ± 0.9** | 19.6 ± 6.0 ‡ | — ⁴ |
| entity-facts-1 | **5.3 ± 1.4** | 16.1 ± 0.3 ‡ | — ⁴ |
| entity-facts-2 | **4.1 ± 0.4** | 12.1 ± 4.0 | — ⁴ |
| entity-summary-1 | **6.2 ± 3.4** | 21.6 ± 7.0 | — ⁴ |
| entity-summary-2 | **5.9 ± 2.7** | 12.1 ± 3.1 | — ⁴ |
| export-1m | **260.0 ± 4.2** | 1440.1 ± 3.3 | — ⁴ |
| film-director-optional | **19.2 ± 1.8** | 33.3 ± 8.1 ‡ | — ⁴ |
| geo-box | 145.7 ± 9.1 | **143.8 ± 1.4** ‡ | — ⁴ |
| inlinks-count-1 | **12.5 ± 0.6** | 14.7 ± 1.4 ‡ | — ⁴ |
| inlinks-count-2 | **8.7 ± 3.9** | 16.5 ± 1.1 ‡ | — ⁴ |
| label-regex | **12.4 ± 5.1** | 19.0 ± 0.0 | — ⁴ |
| outlink-classes-1 | **16.7 ± 1.1** | 22.6 ± 5.1 ‡ | — ⁴ |
| outlink-classes-2 | **13.6 ± 0.8** | 23.7 ± 4.1 ‡ | — ⁴ |
| people-no-birthdate | **49.6 ± 0.9** | 65.6 ± 4.9 ‡ | — ⁴ |
| place-births-1 | **11.1 ± 1.9** | 43.9 ± 2.4 | — ⁴ |
| place-births-2 | **17.8 ± 0.7** | 41.9 ± 1.0 | — ⁴ |
| place-union-1 | **11.4 ± 4.4** | 13.7 ± 4.3 | — ⁴ |
| place-union-2 | **9.6 ± 3.6** | 15.0 ± 0.6 | — ⁴ |
| predicate-counts | 25.7 ± 5.0 † | 5732.6 ± 45.0 † | — ⁴ |
| redirect-target-1 | **11.2 ± 1.4** | 22.8 ± 1.3 | — ⁴ |
| redirect-target-2 | **7.6 ± 4.0** | 22.7 ± 1.6 | — ⁴ |
| sameas-subjects | **5.5 ± 2.5** | 21.3 ± 2.9 ‡ | — ⁴ |
| top-linked | **573.7 ± 19.5** | 5580.6 ± 7.6 | — ⁴ |
| **throughput** entity-facts-1, 16 clients (queries/s) | **1410** | 1064 | — ⁴ |
| **throughput** place-births-1, 16 clients (queries/s) | **802** | 231 | — ⁴ |

‡ Same values, different RDF terms. QLever returns counts as `xsd:int`.
† The engine's answer differs from the majority, so the query is not ranked for it.
QLever's `count-all` is explained [below](#why-count-all-and-predicate-counts-differ). On
`predicate-counts` Sparkles and QLever disagree for the same reason, and with Fluree's
answer missing there is no majority, so the query is not ranked.

4. Fluree's server was killed at the 14 GiB limit before it ran these queries warm.

The table is the second of two passes of the query step on the same day, both with the
same build and index. The second ran after the other engines, on an index whose pages had
long been written back. Sparkles was faster than QLever on 28 of the 29 queries that both
rank. QLever was 1.3% faster on `geo-box` (143.8 against 145.7 ms), which is a tie. In
the first pass, Sparkles took 139.0 ms on `geo-box` and QLever won `entity-facts-2` by 3%
(12.1 against 12.5 ms) instead. Sparkles served 1,410 `entity-facts-1` requests a second
under 16 clients against QLever's 1,064, and 802 `place-births-1` requests against 231.
It loaded the data 2.8× faster than QLever and 4.9× faster than Fluree. Fluree took
6,719 ms on `abstract-contains` against Sparkles' 43 ms, and 63 and 49 ms on the two
`category-people` lookups against 16 and 13 ms.

| memory | sparkles | qlever | fluree |
|---|---:|---:|---:|
| server RSS after the run (MiB) | 4,448 | **1,426** | killed at 14 GiB |
| of which file-backed pages of the index (MiB) | 2,678 | — | — |
| of which anonymous memory (MiB) | 1,769 | — | — |
| load time (s) | **596** | 1,674 | 2,919 |
| load peak RSS (MiB) | **6,432** | 11,401 | 12,366 |
| index size on disk | **28.7 GiB** | 33.4 GiB | 69.7 GiB |

The server RSS figures do not measure the same thing for Sparkles and QLever. Sparkles
maps its index files into memory, so the pages of the index that a query touched count
toward its VmRSS, although they are clean file pages that the kernel can drop at any
time. QLever reads its files with `pread`, so its page cache never shows up in its RSS.
After the run, 2,678 MiB of Sparkles' 4,448 MiB were such pages and 1,769 MiB were
anonymous memory. A separate server that ran every query twice held 1,024 MiB in its
decoded-block cache, so the cache is full at this size and is most of the anonymous
memory. Less the mapped pages and the block cache, Sparkles held about 750 MiB, about
half of QLever's RSS. Setting `--cache-mb 256` would give back about 770 MiB of the
anonymous memory, at a cost that this run did not measure at this size
([Memory and the speed it buys](#memory-and-the-speed-it-buys) has the cost at 10.5M).

| query (cold) | sparkles (ms) | qlever (ms) | fluree (ms) |
|---|---:|---:|---:|
| abstract-contains | **738.8** | 4296.2 | 4195.1 |
| births-by-decade | **114.9** | 165.2 | 6108.5 |
| category-people-1 | **197.3** | 224.0 | 2886.2 |
| category-people-2 | **136.7** | 166.2 | 2172.0 |
| category-tree | **730.9** | 2098.1 | 4559.3 |
| class-counts | **9.9** | 177.8 | 9581.4 |
| costar-birthplace | **156.6** | 183.0 | 4358.2 |
| count-all | **2.2** | 4630.4 | 1273.3 |
| country-population | 63.4 | **45.6** | 4324.1 |
| entity-facts-1 | **19.6** | 36.4 | 1312.2 |
| entity-facts-2 | **27.4** | 40.4 | 837.6 |
| entity-summary-1 | **9.3** | 29.0 | 1303.7 |
| entity-summary-2 | **9.6** | 29.5 | 799.9 |
| export-1m | **1949.6** | 4261.4 | 6647.1 |
| film-director-optional | **41.3** | 68.6 | 13434.8 |
| geo-box | 395.5 | **157.9** | 12218.4 |
| inlinks-count-1 | **11.5** | 30.2 | 792.6 |
| inlinks-count-2 | **13.1** | 27.3 | 1275.8 |
| label-regex | 138.5 | **71.3** | 4328.6 |
| outlink-classes-1 | **59.5** | 73.7 | 1711.0 |
| outlink-classes-2 | 39.7 | **34.9** | 1665.2 |
| people-no-birthdate | 105.3 | **92.0** | 2113.6 |
| place-births-1 | **209.2** | 283.9 | 3293.1 |
| place-births-2 | **261.1** | 361.5 | 3956.5 |
| place-union-1 | **168.1** | 220.3 | 1235.7 |
| place-union-2 | **164.8** | 217.4 | 1244.6 |
| predicate-counts | **32.8** | 5880.8 | 81502.3 |
| redirect-target-1 | **18.7** | 45.9 | 1290.5 |
| redirect-target-2 | **17.1** | 48.0 | 1276.7 |
| sameas-subjects | **8.2** | 38.2 | 772.8 |
| top-linked | **1117.4** | 6538.4 | 57624.6 |

Cold, Sparkles was faster than QLever on 26 of the 31 queries. QLever was faster on
`geo-box` (2.5×), `label-regex` (1.9×), `country-population` (1.4×),
`people-no-birthdate` and `outlink-classes-2` (1.1×). Fluree was the slowest of the
three on every query but `count-all` and `abstract-contains`, where QLever was slower.

The [cold-read features](#cold-reads) favor sparse point lookups but can slow queries
that read many pages. The following configuration comparison uses the same index,
two rounds per setting, with I/O hints and the sparse vocabulary index enabled or
disabled together.

| 1.01B, cold, ms | Both off | Both on (default) |
|---|---:|---:|
| `entity-facts-1` | 36.0 to 38.3 | **14.6 to 15.2** |
| `label-regex` | **66.4 to 68.5** | 119.5 to 130.8 |
| `export-1m` | **1,231 to 1,239** | 1,883 to 1,956 |
| `category-tree` | 369.7 to 371.1 | 364.5 to 365.2 |
| `top-linked` | 949 to 1,000 | 946 to 964 |

### Why `count-all` and `predicate-counts` differ

QLever counts 1,014,683,273 triples, and Sparkles and Fluree count 1,014,683,275. QLever
folds `xsd:float` and `xsd:double` literals whose lexical forms parse to the same number
into one term. DBpedia has such pairs, for example distinct lexical forms of the same
value in `geo:lat` and in `dbo:orbitalPeriod`, so QLever merges two pairs of triples that
differ only in that literal. RDF 1.1 identifies a literal by its lexical form and
datatype, so Sparkles keeps these as distinct terms and distinct triples, as Jena does.
The per-predicate counts differ for the same reason. [COMPARISON.md](COMPARISON.md#general)
lists this among the divergences.

### Bulk loading

The DBpedia load takes 596 s with a peak RSS of 6,432 MiB. Load logs give these
phase times for Sparkles at `98a75c1a` and QLever:

| Phase | Sparkles (s) | QLever (s) |
|---|---:|---:|
| Parse and partial vocabularies | 297 | 914 |
| Vocabulary merge | 67 | 183 |
| Remap to global ids | included in the sort | 117 |
| Sorts and permutations | 233 | 459 |
| Total | 596 | 1,674 |

The loading pipeline combines parallel parsing and vocabulary merging with external
sorting. Sampled keys divide partial vocabularies into ranges that merge in parallel
and are appended in order. Each 64-million-quad chunk is remapped once, sorted by
SPO, OSP and PSO, and written as compressed runs. Each order merges in its own thread.
SOP, OPS and POS are derived from those streams by sorting runs of the first column;
without named graphs, GSPO derives from SPO. Each permutation has its own writer.
This limits repeated input reads while using the available parser and sorting cores.

### Prefix filtering and large results

The `label-regex` filter, `REGEX(?l, "^The B")`, fixes the start of the label.
Sparkles uses the sorted vocabulary to narrow matching ids to ranges before joining
with the bands. The filtered range contains 23,812 rows here. Ids outside the range
fail without reading their keys.

Results above 65,536 rows are serialized in chunks of that size. Within a chunk,
vocabulary ids are sorted, their pages are requested ahead, and front-coded blocks
are decoded once in id order and in parallel. Requests for the next chunk overlap
writing the current one. Smaller results are decoded row by row to limit per-request
memory under concurrency.

In the comparative DBpedia results, `export-1m` takes 260.0 ms warm and 1,949.6 ms
cold. Batched serialization holds up to about 70 MiB of additional memory while
writing an export. The cold-read configuration comparison above shows the remaining
tradeoff between sparse lookup behavior and dense export reads.

## WatDiv

WatDiv v0.6 basic testing at scale 100 has 10,973,381 triples, 20 query templates and 5
instances of each template, except C1–C3, which have one. Every engine returned the same
answer to all 88 instances. Times are in ms, the geometric mean over each template's
instances of the mean of 5 runs, after 1 warm-up.

| load (s) | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| load | **3.78** | 33.61 | 9.71 | 9.46 | 13.16 |

| template | instances | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|---:|
| L1 | 5 | **3.91** | 6.87 | 9.77 | 4.01 | 4.70 |
| L2 | 5 | **3.98** | 12.2 | 9.39 | 5.48 | 15.5 |
| L3 | 5 | **3.91** | 6.98 | 7.11 | 4.07 | 4.59 |
| L4 | 5 | **4.04** | 8.23 | 7.06 | 4.36 | 8.28 |
| L5 | 5 | **4.26** | 11.4 | 11.1 | 4.77 | 13.7 |
| S1 | 5 | **4.50** | 7.63 | 95.7 | 4.84 | 7.06 |
| S2 | 5 | **4.35** | 7.68 | 8.07 | 4.89 | 7.02 |
| S3 | 5 | **4.17** | 10.7 | 11.0 | 5.43 | 15.6 |
| S4 | 5 | **4.09** | 13.2 | 8.00 | 5.29 | 29.9 |
| S5 | 5 | **4.07** | 8.03 | 10.2 | 4.73 | 13.8 |
| S6 | 5 | **4.07** | 7.25 | 7.64 | 4.75 | 6.56 |
| S7 | 5 | 3.92 | 6.45 | 9.67 | **3.90** | 3.93 |
| F1 | 5 | **4.24** | 8.45 | 16.2 | 4.42 | 14.4 |
| F2 | 5 | **4.26** | 7.88 | 32.3 | 4.52 | 8.67 |
| F3 | 5 | **4.17** | 7.51 | 18.5 | 28.0 | 6.17 |
| F4 | 5 | **4.77** | 7.80 | 56.3 | 26.0 | 8.94 |
| F5 | 5 | **4.45** | 7.39 | 20.2 | 4.61 | 7.47 |
| C1 | 1 | **7.22** | 16.8 | 33.9 | 8.16 | 36.5 |
| C2 | 1 | **8.61** | 140 | 54.9 | 39.7 | 369 |
| C3 | 1 | **49.5** | 10247 | 289 | 104 | 23122 |

| category | templates | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|---:|
| **linear (L)** | 5 of 5 | **4.02** | 8.86 | 8.74 | 4.51 | 8.24 |
| **star (S)** | 7 of 7 | **4.16** | 8.47 | 12.6 | 4.81 | 9.73 |
| **snowflake (F)** | 5 of 5 | **4.37** | 7.80 | 25.6 | 9.23 | 8.75 |
| **complex (C)** | 3 of 3 | **14.5** | 289 | 81.3 | 32.3 | 678 |
| **all templates** | 20 of 20 | **5.04** | 14.2 | 18.2 | 7.41 | 17.2 |

Most WatDiv instances are selective, and Sparkles and Fluree answer most of them within
1 ms of the 3.5 ms cost of the request. Sparkles is the fastest on 19 of the 20 templates
and on 80 of the 88 instances. Fluree is faster on `S7` by 0.5% and on six instances, and
Oxigraph on two, all by 4% or less, so those are ties. Fluree comes within 10% on 40 of
the 88 instances, mostly in the L, F1, F2, F5 and S7 templates, and Oxigraph on 9, five
of them in S7. The clear
wins are the complex templates. C3 is a star of six patterns with 419,866 result rows, and
Sparkles answers it in 49.5 ms against Fluree's 104 ms and QLever's 289 ms.
`results/instances.md` in the WatDiv workdir lists every instance. WatDiv was created by
G. Aluç, O. Hartig, M. T. Özsu and K. Daudjee, "Diversified Stress Testing of RDF Data
Management Systems", ISWC 2014.

## Full-text search

`scripts/bench-text.sh` compares Sparkles' `text:query` (Tantivy) with Fuseki's
jena-text (Lucene) and QLever's `ql:contains-word`. Sparkles and Jena index `foaf:name`,
`ex:title` and `rdfs:label`, with English titles stemmed in both. QLever's text index
from literals cannot be limited to predicates, so it indexes every literal, and its
queries join with the predicate instead. QLever does not stem, so its form of the
stemmed query searches for the indexed word. The seven queries include this stemmed
search. All three engines returned the same hits for every query.

| Text index build | sparkles | jena-fuseki | qlever |
|---|---:|---:|---:|
| 1.05M: build (s) | **0.88** | 4.37 | 1.86 |
| 1.05M: size on disk (MB) | 11.9 | 24.9 | **11.1** |
| 10.5M: build (s) | **6.03** | 25.98 | 19.35 |
| 10.5M: size on disk (MB) | 123.4 | 227.8 | **99.9** |

Query times are mean ± σ in ms, with TSV results, 10 runs at 1.05M and 5 at 10.5M:

| query | hits at 10.5M | sparkles 1.05M | jena-fuseki 1.05M | qlever 1.05M | sparkles 10.5M | jena-fuseki 10.5M | qlever 10.5M |
|---|---:|---:|---:|---:|---:|---:|---:|
| rare-top10 | 2 | **4.1 ± 0.1** | 12.7 ± 1.5 | 6.7 ± 0.4 | **5.8 ± 0.4** | 15.0 ± 1.9 | 7.8 ± 0.5 |
| common-top10 | 10 | **4.4 ± 0.2** | 11.7 ± 1.5 | 7.8 ± 0.6 | **7.9 ± 0.2** | 14.9 ± 2.8 | 17.5 ± 2.7 |
| common-count | 49,837 | **4.9 ± 0.2** | 53.9 ± 3.4 | 7.2 ± 0.7 | 14.5 ± 0.7 | 406.9 ± 4.3 | **13.5 ± 1.1** |
| text-join | 3,469 | **5.6 ± 0.2** | 66.0 ± 1.5 | 11.3 ± 0.8 | **22.1 ± 0.7** | 585.4 ± 5.1 | 46.6 ± 7.6 |
| conjunction | 2,893 | **4.1 ± 0.1** | 15.0 ± 1.5 | 7.0 ± 0.3 | **8.2 ± 0.3** | 80.7 ± 0.8 | 14.3 ± 1.0 |
| highlight | 49,837 | 15.0 ± 1.1 | 101.8 ± 8.7 | **13.7 ± 1.3** | 102.7 ± 0.8 | 841.8 ± 46.2 | **94.8 ± 6.5** |
| stemmed | 502 | **3.9 ± 0.2** | 12.2 ± 0.6 | 5.9 ± 0.3 | **5.5 ± 0.3** | 53.5 ± 0.9 | 18.6 ± 2.6 |

Sparkles is the fastest on 6 of the 7 queries at 1.05M and on 5 of 7 at 10.5M. QLever
counts the hits of a common word 1.07× faster at 10.5M, and it wins `highlight` at both
sizes. QLever has no highlighting, so its `highlight` form returns the plain literals,
while Sparkles marks the matched words in each of the 49,837 literals.

## Path search

These runs time `SERVICE path:search` against the
property path queries that test the same reachability, on the `foaf:knows` graph of the
benchmark data. The `+` path from person 0 reaches 89,090 nodes at 1.05M and 892,556 at 10.5M. The pair
queries go from person 0 to the last person, 99,999 or 999,999, whose shortest chains
have 12 and 17 edges. The queries are Sparkles syntax, so they are not part of
`scripts/bench.sh`, which runs the same queries on every engine. QLever has a path
search service of its own with other parameters, and it was not measured.

The runs were on 2026-10-02 with a release build, against a warm server with
the result cache off. Each query ran 31 times, interleaved with the others, with the
client pinned to one core. Background compilation was running on the machine, and the load
average stayed between 33 and 80 on 16 cores, so the medians are noisy. The minimum is
the better guide to the cost of each query.

| query | what it does | 1.05M min | 1.05M median | 10.5M min | 10.5M median |
|---|---|---:|---:|---:|---:|
| `reach-plus` | `COUNT` of `ex:0 foaf:knows+ ?x` | 11.6 | 27.9 | 132.8 | 248.2 |
| `ps-any` | `COUNT` of one shortest path to every reachable person | 19.0 | 42.1 | 226.8 | 384.8 |
| `ask-pair` | `ASK { ex:0 foaf:knows+ ex:last }` | 11.2 | 24.4 | 123.1 | 192.1 |
| `ps-pair` | the shortest path between the same pair, one row per edge | 1.0 | 1.5 | 3.0 | 4.1 |
| `ps-pair-all` | every shortest path between them (1 at 1.05M, 3 at 10.5M) | 0.9 | 1.7 | 2.7 | 4.0 |
| `ps-pair-both` | the shortest path with `path:direction path:both` | 1.1 | 1.7 | 1.6 | 2.3 |
| `ps-pair-k5` | the 5 shortest paths, by Yen's algorithm | 11.4 | 23.5 | 68.6 | 106.6 |
| `ps-values10` | the distance from 10 people bound by `VALUES` to the last person | 2.6 | 6.2 | 12.2 | 18.3 |
| `ps-all4` | `COUNT` of every path of up to 4 edges from person 0 (21 and 43) | 0.7 | 1.1 | 0.7 | 1.1 |
| `pp-len4` | the same count as a `UNION` of four fixed-length property paths | 0.8 | 1.3 | 0.7 | 1.3 |
| `ps-any-weighted` | `ps-any` with `path:weight`, run by Dijkstra's algorithm | 87.5 | 220.2 | 1,644 | 5,094 |

Times are in ms. Between one pair, the bidirectional search is 11 to 41 times faster than
`ASK` with `foaf:knows+`, because it stops when the two frontiers meet near the middle,
while the property path explores everything person 0 reaches. A search from one person
to everyone costs 1.6 to 1.7 times the `+` query. It reads the same index ranges with the
same batched sweeps, but it also keeps a parent and a level for each person and returns
a path for each. The weighted search is 5 to 7 times slower than the unweighted one.
Dijkstra's algorithm settles one node at a time, so it seeks the index once per person
instead of sweeping whole levels, and it keeps a heap. None of the edges in the data has
a weight, so every edge counts 1 and the answers equal the unweighted ones.

The server's peak resident memory after these runs was 205 MB at 1.05M and 936 MB at
10.5M, most of it the block cache. A search charges 48 bytes per visited node against the
query's memory budget, about 43 MB for the 892,556 nodes at 10.5M.

## Query execution features

Each executor optimization can be switched off with `SPARKLES_DISABLE_OPTIMIZATIONS`. The
configuration comparison ran the 10.5M suite with each of the 31 switches of
`Optimizations::NAMES` disabled individually, against the default configuration. It
used the main comparison's build, machine and timing method. A query counts as
changed when its mean moved by more than 25%.
The default run served 243 `star-join` queries a second under 16 clients.

| Feature disabled | Effect at 10.5M |
|---|---|
| Correlated subquery decorrelation (`decorrelate_exists`) | `exists-join` 227× slower (3,345 against 14.8 ms), `not-exists` 82× slower (1,182 against 14.5 ms) |
| Leading-key top-k selection (`topk_first_key`) | `expr-order-key` 8.1× slower (179 against 22.3 ms) |
| Expression reuse (`expr_cache`) | `expr-bind-group` 5.0×, `expr-order-key` 3.6× and `expr-agg-arg` 1.9× slower |
| Batched joins (`batched_join`) | `values-star` 4.8×, `star-lookup` 3.1× and `employee-docs` 1.6× slower. Throughput 112 against 243 q/s. |
| Batched path traversal (`batched_paths`) | `knows-reach` 2.7× slower (390 against 145 ms) |
| Anti-joins (`anti_join`) | `minus` 2.7× slower (23.4 against 8.7 ms) |
| Counting joined runs (`count_join_runs`) | `two-hop-count` 2.0× slower (35.0 against 17.2 ms) |
| Counts from index metadata (`metadata_counts`) | `predicate-counts` 2.0× and `distinct-obj` 1.8× slower. Throughput 214 against 243 q/s. |
| Counting matching vocabulary runs (`count_filter_runs`) | `contains` and `lang-filter` 1.7× and `regex-iri` 1.6× slower |
| Incremental aggregation (`incremental_group`) | `group-avg` 1.4× slower (101 against 71.5 ms) |
| Ordered top-k selection (`ordered_topk`) | `range-topk` 1.4× slower (22.3 against 16.4 ms) |
| Galloping index joins (`gallop_index_join`) | Throughput 134 against 243 q/s. No single query changed by more than 25%. |

Other planner, range-filter and spatial features did not change query means by more
than the 25% reporting threshold, apart from variability in `expr-order-key`.
That query ranged from 20.9 to 31.9 ms, with a median of 23.4 ms, against 22.3 ms
in the default configuration; differences within that spread are inconclusive.

Coverage matters when interpreting these feature comparisons. Spatial features need
GeoSPARQL filters. Vocabulary prefix ranges need `STRSTARTS` or anchored `REGEX`,
as in DBpedia's `label-regex`. Prefetching matters on cold servers, and delta
statistics matter after writes. Planner estimates affect latency only when they
change the chosen plan. The fresh, warm synthetic suite does not exercise all of
those cases.

## What the benchmark does not cover

* **Larger scale.** The largest dataset measured is English DBpedia at 1.24 billion
  triples ([DBpedia at 1.24 billion triples](#dbpedia-at-124-billion-triples)). QLever is
  built for 10⁹–10¹¹ triples and routinely runs at that size (Wikidata, UniProt). Its
  design advantages grow with size: lazy evaluation, FSST vocabulary compression and IRI
  encoding. Sparkles materializes intermediate results, so it would hit memory limits
  earlier on very large intermediate results.
* **Data larger than RAM.** The cold runs restart the server with an empty page cache.
  Every store here fits in memory, except the DBpedia index of 28.7 GiB on a machine
  with 15 GiB of RAM.
* **Other standard benchmarks.** WatDiv's basic testing is covered. LUBM, BSBM, SP²Bench,
  WatDiv's stress testing and the Wikidata query log are not. The synthetic suite has a
  fairly regular shape, and its 28 queries are hand-picked.
* **Large writes and long runs.** The update runs use single-triple commits for at most
  30 s or 5,000 commits. Large batches, hours of sustained writes and compaction under
  load were not compared with other engines.
* **Spatial queries and inference-time reasoning.** GeoSPARQL workloads were not compared
  with other engines. The sections below give the spatial index's commit cost and the
  GeoSPARQL Compliance Benchmark results. Sparkles' only query-time reasoning is RDFS on
  read, and it has no backward chaining, so query-time reasoning was not compared with
  other engines.
* **Result-cache benefit.** All runs had result caches off. With the cache on, repeated
  queries are mostly served from memory, which is not a fair comparison.
* **Other hardware.** Every comparison ran on one 20-thread machine with 15 GiB of RAM.

## Other measurements

These measurements time Sparkles on its own. Unless a section names another machine, they
ran on 2026-09-30 to 2026-10-02 on a laptop with an Intel Core Ultra X7 358H (16
threads), 30 GB of RAM and NVMe, with an unpinned client. Their small-request times
include the start-up noise described under [Setup](#setup).

| | Sparkles |
|---|---:|
| RDFS materialization, 1.0M → 2.75M inferred triples | 3.9 s end to end |
| SHACL validation, 20 shapes, 1.05M triples, 48,428 results (`mise run bench:shacl 100000`) | 164 ms parallel, 741 ms sequential |
| `ASK { ?s ?p ?o }` / `SELECT * … LIMIT 100` at 10.5M (early termination) | 1.9 ms / 1.8 ms |

### Compression (10.5M triples)

These runs measured Sparkles alone, with a release build, on 2026-09-30. HTTP times are
`hyperfine` means over 5 runs and include `curl`. Server CPU is the server process's
user + system time per request.

| Response | Encoding | Bytes | Ratio | Wall (s) | Server CPU (s) |
|---|---|---:|---:|---:|---:|
| Full N-Quads export (`GET /bench/data`) | identity | 1,121,335,687 | 1.0 | 3.78 | 4.28 |
| | gzip-6 | 74,937,481 | 15.0 | 7.49 | 12.13 |
| | br-4 | 96,352,029 | 11.6 | 4.20 | 8.15 |
| | zstd-3 | 83,274,404 | 13.5 | 3.74 | 5.58 |
| | zstd-1 | 79,052,839 | 14.2 | 3.75 | 5.21 |
| `SELECT * … LIMIT 500000` as TSV | identity | 45,411,223 | 1.0 | 0.139 | 0.158 |
| | gzip-6 | 2,919,841 | 15.6 | 0.337 | 0.508 |
| | br-4 | 3,503,443 | 13.0 | 0.196 | 0.318 |
| | zstd-3 | 3,140,275 | 14.5 | 0.137 | 0.188 |
| | zstd-1 | 2,909,611 | 15.6 | 0.141 | 0.190 |

zstd streams the export as fast as identity and makes it 13–14× smaller. gzip doubles
the wall time and triples the server CPU. zstd-1 came out slightly smaller than zstd-3
on both responses.

`/$/backup` writes an N-Quads dump to a file. The times below are medians of 3 runs. The
CLI `sparkles backup --threads 16` with zstd-3 takes the same 8.2 s, so the server's 4
threads are not the bottleneck.

| Codec | Seconds | Size |
|---|---:|---:|
| none | 4.33 | 1,121 MB |
| lz4 | 4.73 | 162 MB |
| zstd-3 | 8.16 | 81.5 MB |
| brotli-5 | 12.82 | 119.0 MB |
| gzip-6 | 40.96 | 74.9 MB |

`sparkles load` was timed on the 1.1 GB N-Triples file, raw and compressed. Times are
medians of 3 runs. CPU is the load's user + system time, and parallelism is CPU divided
by wall time.

| Input | File size | Seconds | CPU (s) | Parallelism |
|---|---:|---:|---:|---:|
| raw | 1,121 MB | 4.82 | 25.6 | 5.2 |
| lz4 | 160 MB | 5.32 | 25.5 | 4.7 |
| zstd-3 | 78.6 MB | 5.41 | 25.3 | 4.7 |
| gzip-6 | 81.9 MB | 5.66 | 25.9 | 4.5 |
| brotli-5 | 69.0 MB | 5.71 | 26.1 | 4.6 |

Decompression runs as a single stream in front of the parallel parser and costs
0.5–0.9 s. The parser keeps about 4.5 cores busy either way.

The full-text document store holds every string literal, 1,540,012 documents. With LZ4
it takes 117.6 MB and builds in 8.67 s. With zstd, the default, it takes 106.1 MB and
builds in 8.71 s.

<a id="point-in-time-reads-105m-triples"></a>

### Point-in-time reads (1M triples)

These HTTP measurements use a 1,000,000-triple base and 50,000 single-quad commits
in one generation, written over four HTTP connections. History retention covers
all commits. Times use individual curl requests and exclude process startup, on
the laptop described above.

| Request | Time |
|---|---:|
| `COUNT(*)` at base + 25,000, first | 67 ms |
| The same, second and later | 1.2–1.6 ms |
| `COUNT(*)` at base + 25,001 | 2.1 ms |
| `COUNT(*)` at head − 100 | 2.7 ms |
| `COUNT(*)` at base + 12,500, cold | 31 ms |
| Diff of one commit at the head | 0.6–2.0 ms |
| Diff of the last 100 commits | 1.6 ms |
| After a restart, at base + 25,000 | 69 ms |

A sparse WAL index and cached snapshots let a read start from the nearest known
state and replay the intervening commits. The first read of an uncached state
costs more than a repeated read or a read close to a cached state or the head.

### Branches and merges (10.5M triples)

A release build loaded the 10.5M-triple benchmark data into a new database and served it
over HTTP, on a machine that other builds kept at a load average of 35 to 85 on 16
cores. Each figure is one `curl` request's `time_total`, except the query rows, which
are medians of 15 interleaved requests.

| Step | Time |
|---|---:|
| Create a branch of `main` at its head (four runs) | 19.5–32.7 ms |
| `star-join` on `main` | 38.3 ms |
| `star-join` on the new, linked branch | 38.7 ms |
| Preview a merge of 10,000 inserted quads into a `main` with 10,000 of its own | 61.9 ms |
| That merge, committed | 114.9 ms |
| First request to a linked branch after a restart | 3.2 ms |

Creating a branch writes about 1.4 KB of files: the branch's identity, its link, its
commit catalog and its copies of the configuration. When the branch's store first opens,
its change log preallocates a 68 KiB segment, so a new branch takes about 70 KB on disk.
A query on a linked branch reads `main`'s index files through the same cached blocks, and
here took as long as on `main`. The first request after a restart replayed no inherited
log, because `main` had no updates at the branch's starting commit; the replay costs
what a past-state read of the same commits costs.

### Backup repositories (10.5M triples)

`mise run bench:backup` (`scripts/backup-bench.sh`) ran against the 10.5M-quad benchmark
database (301 MB), with Sparkles alone, a release build and a warm page cache. The `fs`
repository was on the same ext4 file system. Each figure is the median (min–max) of 3
rounds, in wall-clock time including process start.

| Case | Seconds | Size |
|---|---:|---|
| Full backup into an empty repository | 0.56 (0.55–0.60) | 301.3 MB logical, 281.5 MB stored, 534 MB/s |
| Incremental backup after 1,000 single-quad commits | 0.34 (0.31–0.35) | 55.3 KB added (3 new blobs, 28 reused) |
| Restore into a new directory (quick check) | 0.79 (0.78–0.93) | 301.5 MB |
| Verify at level `data` (every blob read and hashed) | 0.08 (0.07–0.13) | 32 blobs, ok |

An incremental backup stores only the appended tail of the update log and the files that
changed, so its size follows the writes since the last backup, not the database size.
S3, cold caches and databases larger than memory have not been measured.

### Full-text index and observability (10.5M triples)

These runs measured Sparkles alone, one configuration at a time, on the laptop. They
used `scripts/bench.sh` with `ENGINES=sparkles SKIP_LOAD=1`, plus `hyperfine` and `oha`.
Update times are end to end over HTTP with `curl`. The comparison with jena-text and
QLever is under [Full-text search](#full-text-search).

The full-text index commits staged documents when a text query needs them, on a
roughly one-second tick, or when about 16,000 changes are staged. It syncs to disk
at most once a second and replays the WAL after a crash.

| | Text search off | Text search on (lazy commit) |
|---|---:|---:|
| Build the index (every string literal, `sparkles text-index`) | — | 8.0 s, 1.3 GiB peak RSS, 102 MB on disk |
| 1,000-triple `INSERT DATA` | 24.4–26.0 ms (median) | 28.4 ± 1.7 ms |
| 1-triple `INSERT DATA` (harness) | 5.7–9.4 ms | 6.7 ± 3.0 ms |
| `text:query ("Perlman" 10)`, top 10 with scores | — | 15.0 ± 0.7 ms |
| `COUNT` of `text:query "Zurich"` (1,954 hits) | — | 28.6 ± 0.8 ms |

Text-off medians span four fresh-server runs of 30 requests each. With text search
on, a 1,000-triple batch costs about 3 ms more. Single-triple inserts vary by a few
milliseconds between runs.

Access logging and Prometheus metrics are on by default. Compared with `--no-access-log
--no-metrics`, the sum of a 20-query workload differs by 0.6% (1,248 vs 1,240 ms).
That is within run-to-run noise, since single queries vary by up to ±50% in either
direction at these sizes. The load test used `oha` with 16 connections and two
alternating rounds of each configuration:

| | Logs and metrics on | Off |
|---|---:|---:|
| `ASK {}`, 50,000 requests | 71,034 / 70,175 req/s, p99 0.63 / 0.67 ms | 69,776 / 68,782 req/s, p99 0.63 / 0.66 ms |
| star join, 1,000 requests | 103 / 103 req/s, p50 155 / 156 ms | 104 / 100 req/s, p50 154 / 159 ms |

Logging and metrics add no measurable overhead.

### Spatial index: commit cost

This test compares commit latency on the same store with the spatial index on and off,
on the laptop with nothing else running. It ran on 2026-10-01 with a release build at `02e3a78`, using
`commit_latency` in `crates/sparkles-core/src/store/geo.rs`. The figures are medians of 200
one-triple commits and of 20 commits of 1,000 `geo:asWKT` points each, over three runs.

| | Index off | Index on |
|---|---:|---:|
| 1-triple non-geometry commit | 0.012–0.016 ms | 0.015–0.017 ms |
| 1,000-point commit | 3.19–3.50 ms | 3.11–3.16 ms |

The index adds no measurable time to a commit. The differences are within run-to-run
noise.

### Automatic compaction (1.05M triples)

This test loaded the 1.05M-triple benchmark data (`gen-data.py 100000`) and sent the
50,000 single-triple commits of `bench-writes.py gen` (35,000 inserts, 15,000 deletes),
each as its own HTTP request on a new connection. It ran on 2026-10-02 with a release
build at `9de1492` and `--result-cache-mb 0`. Background builds and tests were running
on the machine (a load average of about 38 on 16 cores), so the tails are noisy.

The statistics-answered queries are medians of 20 runs after 3 warm-up runs. The uncompacted
store has a 50,000-quad delta with automatic compaction disabled. The compacted store
uses automatic compaction with `idleSeconds` 5. That compaction took 2.0 s and held the writer lock for 5.7 ms.

| Query | With the delta | After the compaction |
|---|---:|---:|
| `types-grouped` | 5.8 ms | 1.9 ms |
| `predicate-counts` | 2.0 ms | 1.9 ms |
| `distinct-obj` | 7.8 ms | 2.2 ms |

The write latency was measured over the same 50,000 commits twice. The first run had
automatic compaction off. The second set `deltaRatio` 0.01, so two compactions ran during
the commits, at about 20,500 delta quads each. They took 1.26 s and 1.44 s, carried 348
and 497 commits made during their builds into the new generation, and held the writer
lock for 3.4 ms and 3.3 ms.

| Commit latency | Compaction off | During the compactions (860 commits) | Rest of the run with compactions |
|---|---:|---:|---:|
| p50 | 2.7 ms | 2.0 ms | 2.0 ms |
| p99 | 21.0 ms | 15.1 ms | 14.2 ms |
| p99.9 | 622 ms | 28.7 ms | 250 ms |
| max | 9.8 s | 58.7 ms | 12.1 s |

Commits during a compaction were no slower than the others. The multi-second outliers
appeared in both runs outside any compaction, and most likely come from `fsync`
stalls on the shared machine.

### Partial compaction and the spatial index

Both measurements are ignored tests of the `sparkles` crate, run in release mode on
2026-10-03 at `6c46372`. Other builds were running on the 16-core machine, with a load
average between 12 and 50, so single timings vary by a factor of two or more.

`partial_compaction_at_scale` loads the 10.5M-triple benchmark data
(`SPARKLES_BENCH_NT`), then, for each delta, copies the store, applies the delta and
compacts the copy once with `partial` forced to `always` and once to `off`. Each delta is
90% new `foaf:knows` links and 10% deleted `foaf:age` quads, all between terms the data
already has. A concentrated delta links 10 people to a narrow range of others and deletes
the ages of consecutive people. A spread delta picks people at random. The generation has
2,254 blocks over its seven permutations. Copies are flushed before timing. The table
gives the range of build times across two measurements under varying background load.

| Delta | Blocks rewritten | Partial | Full |
|---|---:|---:|---:|
| 1,000 concentrated | 102 | 0.14–0.21 s | 8.4–16.4 s |
| 10,000 concentrated | 107 | 0.20–2.7 s | 8.1–11.3 s |
| 100,000 concentrated | 117 | 0.40–0.82 s | 7.5–10.3 s |
| 1,000 spread | 1,201 | 1.6–2.4 s | 9.4–16.4 s |
| 10,000 spread | 1,223 | 0.60–0.93 s | 8.3–8.4 s |
| 100,000 spread | 1,223 | 1.1–1.4 s | 8.8–13.6 s |

The wide 0.20–2.7 s range in the second row reflects the busy host; these figures
should not be treated as a monotonic scaling curve. The concentrated deltas touch few
blocks of the subject-ordered permutations, but the deleted ages, which are integers spread over the
object-ordered permutations, still touch a hundred blocks. A spread delta touches every
block of five permutations and some of the other two, 54% in all, and a partial
compaction of it still took a tenth of a full build. A full build merges the vocabulary
and sorts every quad, which a partial compaction never does. Both kinds of compaction held
the writer lock for 2 to 22 ms. The `auto` setting chose a partial compaction for every
delta here.

`compaction_lock_with_a_spatial_index` loads 100,000 features with a point geometry and a
label (300,000 quads), enables the spatial index, and three times makes 500 commits that
add features and then compacts while 100 more commits are made during the build. It
measures the writer-lock duration with the spatial base built alongside the
generation, outside the lock.

| Case | Writer lock at the switch |
|---|---:|
| No spatial index | 2.5–20 ms |
| Spatial base built with the generation | 2.7–4.3 ms |

The base takes about 0.1 s more of the build, which runs without the lock.

### GeoSPARQL Compliance Benchmark

The GeoSPARQL Compliance Benchmark (Jovanovik, Homburg and Spasić, 2021) has 206 queries
over the 30 requirements of GeoSPARQL 1.0 and a 338-triple dataset. It was run with
`scripts/geosparql-benchmark.sh` on `forge` on 2026-10-03, using the benchmark at commit
`879e0746` and the release build of `sparkles-server` at commit `98a75c1a` with its
default features.

Each requirement ran against the configuration of its conformance class. The queries of
R25 to R30 test the RDFS entailment and query rewrite extensions, so they ran against a
database with `infer --profile rdfs --vocab geosparql` and `"queryRewrite": true`. The
other requirements test the asserted data, so they ran against the same data with the
spatial index and neither extension. Entailment and rewrite add answers to those queries,
which the benchmark does not expect, and with every query against the extended database
Sparkles scored 147.

An answer counts as correct when it matches one of the expected result files. Solutions
are compared as multisets, and numbers within a relative 1e-6. Geometry literals are
compared by their coordinates, rounded to 6 decimals. GML and KML literals are first
converted to GeoJSON by the server's `POST /$/geo/convert`. The comparison ignores ring
starts and directions, line directions, vertices on straight runs and member order.
Requirement R17 has no query.

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
GeoSPARQL Fuseki 3.17 published 177 of 206. GML literal support is exercised by
72 of the 96 R22 to R24 queries; the extension requirements use the configured
RDFS entailment and query rewrite features described above.

The 19 queries that fail disagree with choices Sparkles made on purpose.

* R13 and R16 (4 queries) expect two empty geometries to be `sfEquals`. In Sparkles an
  empty geometry is only disjoint, as in Jena.
* Four R19 queries expect distances that are neither geodesic nor haversine, and two
  expect a 10-metre buffer to be 10 degrees wide.
* Nine R28 to R30 queries expect relations that DE-9IM and the standard's tables do not
  give. They count a region as an RCC8 tangential proper part of itself, count a region
  strictly inside another as `ehCoveredBy` it, leave a feature's own geometries and the points inside
  it out of `sfIntersects`, and leave the EPSG:4326 point at latitude 31.95, longitude -88.38 out
  of the geometries disjoint from feature B.
