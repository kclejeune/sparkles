# Benchmarks

This page is a dated snapshot of Sparkles performance, including comparisons with
Apache Jena (TDB2 and Fuseki), QLever, Fluree and Oxigraph. Sparkles was measured on
2026-10-06 on `forge`, at commit `8dd7a662`; the
other engines retain their 2026-10-03 measurements. Data, queries and timing methods
match, but these are dated reference comparisons rather than interleaved new runs.
[Sparkles HTTP latency](#sparkles-http-latency) gives Sparkles-only measurements from
2026-10-05 with a different timing method. Feature and configuration comparisons
explain the effects of caching, loading, query execution and storage settings.
[HTTP writes](#http-write-snapshot) gives a separately dated snapshot of durable
batches and concurrent commit grouping.
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
| Bulk load | **0.52 s** (Fluree 1.27, Oxigraph 1.28, QLever 1.51, TDB2 4.78) | **3.61 s** (QLever 9.69, Fluree 11.07, Oxigraph 14.13, TDB2 45.57) |
| Fastest of the five | 24 of 28 queries | 28 of 28 queries |
| Clear warm-query losses | `range-topk` to QLever, 1.21× | none |
| Ties within 10% | Fluree on `count-all`, `distinct-obj`, `star-lookup`, `values-star` and `employee-docs` | Fluree on `count-all`, `distinct-obj` and `star-lookup`; Oxigraph on `path-plus` and `values-star` |
| vs. QLever | Faster on 27 of 28, median 2.7× | Faster on 28 of 28, median 3.7× |
| vs. Fluree | Faster on 25 of 28, median 3.4× | Faster on all 27 completed queries, median 9.6× |
| vs. Fuseki | Faster on all 27 completed queries, median 13× | Faster on all 27 completed queries, median 78× |
| vs. Oxigraph | Faster on all 28, median 24× | Faster on all 28, median 82× |
| Update latency, 1 triple | 3.89 ms (**Oxigraph 3.71**, QLever 3.79, Fluree 5.60, Fuseki 32.6) | **4.12 ms** (QLever 4.19, Oxigraph 4.27, Fluree 4.55, Fuseki 32.4) |
| Throughput, 16 clients | **1,193 q/s** (QLever 462, Fluree 246, Fuseki 113, Oxigraph 25) | **232 q/s** (QLever 92, Fluree 22, Fuseki 11, Oxigraph 1.7) |
| Server RSS after the run | 436 MiB (**QLever 307**, Oxigraph 1,244, Fuseki 1,769, Fluree 2,735) | 952 MiB (**QLever 653**, Oxigraph 2,381, Fluree 4,664, Fuseki 4,899) |
| Server RSS less the block cache | 372 MiB | 573 MiB |
| Bulk load peak RSS | **325 MiB** (Fluree 475, QLever 660, Oxigraph 738, TDB2 2,188) | 2,099 MiB (**QLever 1,931**, Fluree 3,142, TDB2 4,115, Oxigraph 6,204) |

“Fastest” counts exact mean comparisons. Differences within 10% are descriptive ties,
not a statistical significance test; small HTTP measurements include the curl process.
Sparkles uses durable acknowledgments, whereas Oxigraph's default does not synchronize
its WAL and QLever retains writes in memory.

On WatDiv at scale 100 (10.97M triples), Sparkles is fastest on all 20 templates and
87 of 88 instances. Fluree is 1.1% faster on one instance, which is a tie. Sparkles'
geometric mean is 4.82 ms, against 7.41 for Fluree, 14.2 for Fuseki, 17.2 for Oxigraph
and 18.2 for QLever.

In full-text search, Sparkles is fastest on all seven queries at 1.05M and six at
10.5M. QLever's advantage on the remaining common-word count is within 10%. Its text
index is smaller; predicate, stemming and highlighting support differ.

The separately dated [DBpedia results](#dbpedia-at-124-billion-triples) cover 1.24 billion
triples: Sparkles loaded the data in 596 s, against QLever's 1,674 s and Fluree's
2,919 s. It was faster than QLever on 28 of 29 agreeing warm queries and 26 of 31 cold
queries. That dataset was not rerun for this snapshot.

The clearest remaining costs are cold point lookups and memory. Sparkles defaults to a
1 GiB decoded-block cache per dataset and materializes intermediate results.
[Memory and the speed it buys](#memory-and-the-speed-it-buys) describes configuration
tradeoffs; [Where Sparkles loses](#where-sparkles-loses) separates clear differences,
small ties and unequal feature or durability contracts.

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
  * Sparkles at commit `8dd7a662`, measured on 2026-10-06, a release build of
    `sparkles-server` with its default features made with rustc 1.99.0 on `forge`: `sparkles serve --result-cache-mb 0`, with
    every other setting at its default. Competitor results below were retained from
    2026-10-03; their versions and settings are unchanged.
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

Most queries at this size are close to the cost of starting curl and making
the request; differences of a few percent near that floor are ties.

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| **load** (s) | **0.52** | 4.78 | 1.51 | 1.27 | 1.28 |
| count-all | 3.7 ± 0.2 | 173.7 ± 19.8 | 10.7 ± 0.9 ‡ | **3.7 ± 0.2** | 256.3 ± 3.8 |
| types-grouped | **3.6 ± 0.2** | 77.4 ± 8.0 | 5.9 ± 0.3 ‡ | 19.0 ± 1.0 | 42.9 ± 2.2 |
| star-join | **7.5 ± 0.3** | 30.6 ± 1.7 | 33.5 ± 3.6 ‡ | 22.5 ± 1.8 | 388.9 ± 4.8 |
| two-hop-count | **5.2 ± 0.2** | 373.2 ± 2.6 | 19.1 ± 2.9 ‡ | 7.1 ± 0.3 | 827.3 ± 5.7 |
| range-topk | 7.7 ± 0.7 ‡ | 95.5 ± 2.5 | **6.4 ± 0.3** | 24.6 ± 1.2 | 57.3 ± 2.2 |
| optional-count | **4.4 ± 0.2** | 80.5 ± 4.9 | 10.1 ± 0.6 ‡ | 15.8 ± 0.7 | 109.6 ± 5.8 |
| contains | **4.3 ± 0.2** | 43.8 ± 2.7 | 75.4 ± 3.8 ‡ | 8.6 ± 0.2 | 147.9 ± 7.0 |
| group-avg | **9.1 ± 1.0** | 207.7 ± 3.3 ‡ | 14.2 ± 0.9 ‡ | 58.6 ± 3.8 ‡ | 308.6 ± 5.6 |
| path-plus | **3.6 ± 0.1** | 6.6 ± 0.6 | 6.2 ± 0.4 ‡ | 7.5 ± 0.2 | 4.0 ± 0.2 |
| distinct-obj | 3.7 ± 0.1 | 90.7 ± 3.2 | 18.0 ± 2.9 ‡ | **3.7 ± 0.1** | 70.6 ± 2.6 |
| export-500k | **114.7 ± 11.6** | 500.8 ± 17.5 | 778.5 ± 16.6 | 245.2 ± 1.9 | 1708.6 ± 3.8 |
| predicate-counts | **3.5 ± 0.1** | 326.6 ± 2.1 | 14.6 ± 0.4 ‡ | 67.4 ± 1.4 | 288.1 ± 3.5 |
| order-by-full | **48.8 ± 7.9** | 543.5 ± 6.6 | 138.6 ± 4.0 | 106.0 ± 0.8 | 3835.0 ± 27.9 |
| optional-chain | **13.5 ± 0.2** | 226.1 ± 4.6 | 63.8 ± 5.2 | 1829.5 ± 85.9 | 362.1 ± 3.9 |
| minus | **4.3 ± 0.1** | 41.7 ± 2.6 | 7.1 ± 0.2 ‡ | 11.9 ± 3.8 | 33.6 ± 4.3 |
| not-exists | **4.7 ± 0.1** | 56.5 ± 1.2 | 7.1 ± 0.3 ‡ | 20.5 ± 1.4 | 111.6 ± 7.0 |
| exists-join | **5.0 ± 0.1** | 161.3 ± 37.7 | 10.1 ± 0.6 ‡ | 31.9 ± 0.4 | 1458.5 ± 18.9 |
| subquery-agg | **3.9 ± 0.1** | 51.1 ± 1.3 | 5.4 ± 0.2 ‡ | 20.4 ± 1.1 | 25.3 ± 3.4 |
| regex-iri | **4.4 ± 0.3** | 58.5 ± 6.6 | 72.2 ± 2.6 ‡ | 88.2 ± 0.6 | 173.9 ± 7.8 |
| knows-reach | **16.5 ± 0.1** | error | 62.0 ± 6.1 ‡ | 61.4 ± 10.7 | 342.8 ± 6.5 |
| distinct-join | **8.5 ± 0.2** | 412.3 ± 3.4 | 23.6 ± 4.3 | 91.1 ± 1.1 | 672.9 ± 7.3 |
| lang-filter | **4.3 ± 0.1** | 31.8 ± 3.2 | 42.8 ± 1.9 ‡ | 6.4 ± 0.1 | 86.2 ± 6.6 |
| star-lookup | 3.9 ± 0.2 ‡ | 5.8 ± 0.4 ‡ | 10.4 ± 0.3 ‡ | **3.7 ± 0.2** | 4.9 ± 0.3 |
| values-star | **3.7 ± 0.1** | 6.7 ± 0.4 | 10.8 ± 0.7 ‡ | 4.1 ± 0.1 | 4.2 ± 0.2 |
| employee-docs | **3.7 ± 0.1** | 6.9 ± 0.5 | 7.1 ± 0.6 | 3.8 ± 0.2 | 4.4 ± 0.1 |
| expr-bind-group | **4.6 ± 0.3** | 48.1 ± 1.1 ‡ | 7.9 ± 0.5 ‡ | 44.4 ± 0.2 | 46.2 ± 4.1 |
| expr-order-key | **6.0 ± 0.3** | 89.1 ± 1.1 | 14.2 ± 0.6 ‡ | 114.8 ± 0.3 | 2672.4 ± 18.4 |
| expr-agg-arg | **8.5 ± 0.2** | 166.1 ± 0.9 | 14.4 ± 1.0 ‡ | 75.9 ± 2.8 | 298.7 ± 9.2 |
| **update** (1-triple INSERT DATA) | 3.9 ± 0.1 | 32.6 ± 3.6 | 3.8 ± 0.1 | 5.6 ± 0.5 | **3.7 ± 0.1** |
| **throughput** star-join, 16 clients (queries/s) | **1193** | 113 | 462 | 246 | 25 |

## Results: 10.5M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| **load** (s) | **3.61** | 45.57 | 9.69 | 11.07 | 14.13 |
| count-all | **3.9 ± 0.3** | 1621.1 ± 16.0 | 53.3 ± 2.7 ‡ | 3.9 ± 0.3 | 2497.6 ± 18.9 |
| types-grouped | **3.7 ± 0.3** | 5208.4 ± 285.4 | 6.6 ± 1.1 ‡ | 113.4 ± 9.3 | 381.2 ± 3.6 |
| star-join | **30.6 ± 0.3** | 251.7 ± 17.1 | 115.1 ± 7.1 ‡ | 142.2 ± 4.6 | 4710.7 ± 22.6 |
| two-hop-count | **17.0 ± 0.5** | 3845.8 ± 30.9 | 61.3 ± 5.5 ‡ | 36.3 ± 0.5 | 8292.3 ± 59.6 |
| range-topk | **15.6 ± 1.3** | 897.4 ± 30.3 | 22.4 ± 2.6 | 153.1 ± 0.9 | 596.1 ± 3.0 |
| optional-count | **11.9 ± 0.5** | 1326.0 ± 161.4 | 45.8 ± 7.7 ‡ | 114.7 ± 5.4 | 992.3 ± 9.8 |
| contains | **7.0 ± 0.2** | 3811.0 ± 60.2 | 690.9 ± 3.5 ‡ | 17.7 ± 0.3 | 1534.9 ± 13.5 |
| group-avg | **54.3 ± 0.7** | 4252.3 ± 382.6 ‡ | 93.4 ± 5.2 ‡ | 549.8 ± 9.5 ‡ | 4057.6 ± 59.3 |
| path-plus | **3.8 ± 0.2** | 6.3 ± 0.2 | 10.3 ± 0.4 ‡ | 54.6 ± 3.5 | 4.1 ± 0.2 |
| distinct-obj | **3.8 ± 0.2** | 2103.1 ± 675.4 | 205.4 ± 5.5 ‡ | 3.9 ± 0.2 | 746.9 ± 2.0 |
| export-500k | **105.8 ± 6.4** | 734.4 ± 84.6 | 661.2 ± 3.1 | 260.4 ± 1.3 | 2087.7 ± 6.6 |
| predicate-counts | **3.8 ± 0.3** | 3266.4 ± 28.4 | 74.1 ± 3.5 ‡ | 608.3 ± 6.0 | 2798.3 ± 5.3 |
| order-by-full | **383.0 ± 12.1** | 13852.8 ± 730.3 | 1421.4 ± 10.9 | 1913.6 ± 7.7 | 55456.3 ± 163.5 |
| optional-chain | **84.4 ± 7.4** | 3825.0 ± 346.9 | 571.2 ± 23.4 | — | 4764.9 ± 72.5 |
| minus | **8.7 ± 0.4** | 2297.5 ± 246.6 | 20.0 ± 2.5 ‡ | 56.9 ± 2.1 | 349.6 ± 12.5 |
| not-exists | **15.0 ± 0.8** | 578.6 ± 141.0 | 22.5 ± 2.2 ‡ | 164.1 ± 2.0 | 1032.3 ± 4.5 |
| exists-join | **14.6 ± 0.5** | 1999.2 ± 290.0 | 66.7 ± 6.6 ‡ | 276.2 ± 2.3 | 122436.9 ± 245.5 |
| subquery-agg | **5.0 ± 0.4** | 1771.5 ± 307.8 | 14.6 ± 0.8 ‡ | 146.7 ± 1.1 | 183.6 ± 2.3 |
| regex-iri | **8.3 ± 0.3** | 1611.5 ± 21.5 | 659.4 ± 1.6 ‡ | 873.5 ± 26.4 | 2642.1 ± 34.5 |
| knows-reach | **144.8 ± 0.8** | — | 1157.6 ± 10.0 ‡ | 658.3 ± 13.8 | 4651.3 ± 88.4 |
| distinct-join | **49.3 ± 1.9** | 4468.2 ± 84.5 | 155.8 ± 8.1 | 1207.0 ± 59.2 | 6704.7 ± 31.7 |
| lang-filter | **6.3 ± 0.2** | 2734.7 ± 189.2 | 389.2 ± 3.2 ‡ | 28.6 ± 1.4 | 839.1 ± 11.2 |
| star-lookup | **4.2 ± 0.3** ‡ | 7.2 ± 0.3 | 24.3 ± 2.8 ‡ | 4.3 ± 0.3 | 5.4 ± 0.4 |
| values-star | **3.9 ± 0.3** | 8.2 ± 2.0 | 11.8 ± 1.1 ‡ | 4.8 ± 0.7 | 4.3 ± 0.2 |
| employee-docs | **4.0 ± 0.2** | 7.7 ± 0.5 | 12.5 ± 0.8 | 4.7 ± 0.3 | 4.7 ± 0.3 |
| expr-bind-group | **10.1 ± 2.0** | 426.7 ± 11.6 ‡ | 34.1 ± 3.6 ‡ | 397.2 ± 1.9 | 354.4 ± 1.5 |
| expr-order-key | **24.7 ± 4.2** | 937.2 ± 18.3 | 109.4 ± 4.0 ‡ | 1120.8 ± 27.1 | 38266.9 ± 54.8 |
| expr-agg-arg | **48.5 ± 3.9** | 1789.4 ± 2.6 | 98.7 ± 6.9 ‡ | 767.0 ± 4.3 | 3899.0 ± 22.3 |
| **update** (1-triple INSERT DATA) | **4.1 ± 0.1** | 32.4 ± 5.5 | 4.2 ± 0.2 | 4.5 ± 0.1 | 4.3 ± 1.4 |
| **throughput** star-join, 16 clients (queries/s) | **232** | 11 | 92 | 22 | 1.7 |

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

These are server resident sets (VmRSS, and VmHWM for peaks), load peak RSS and allocated
index bytes. Sparkles values are from 2026-10-06; competitors retain the same phases
from 2026-10-03. Lower is better.

| 1.05M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 53 | 257 | 28 | 52 | **22** |
| server RSS after every query ran once (MiB) | 194 | 981 | 146 | 2083 | **91** |
| server RSS after the run (MiB) | 436 | 1769 | **307** | 2735 | 1244 |
| peak RSS during throughput (MiB) | 631 | 1769 | **335** | 2735 | 1250 |
| Sparkles RSS after the run less its 64 MiB block cache (MiB) | 372 | — | — | — | — |
| bulk load peak RSS (MiB) | **325** | 2188 | 660 | 475 | 738 |
| index size on disk | 27 MiB | 132 MiB | **26 MiB** | 37 MiB | 147 MiB |

| 10.5M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 56 | 270 | 43 | 260 | **28** |
| server RSS after every query ran once (MiB) | 749 | 3409 | 414 | 3377 | **408** |
| server RSS after the run (MiB) | 952 | 4899 | **653** | 4664 | 2381 |
| peak RSS during throughput (MiB) | 1408 | 4899 | **778** | 4756 | 2381 |
| Sparkles RSS after the run less its 379 MiB block cache (MiB) | 573 | — | — | — | — |
| bulk load peak RSS (MiB) | 2099 | 4115 | **1931** | 3142 | 6204 |
| index size on disk | 286 MiB | 1.3 GiB | **276 MiB** | 398 MiB | 1.4 GiB |

A fresh-server probe runs every query once and then three rounds of 160 concurrent
star joins. At 10.5M, Sparkles held 739 MiB after the query pass and 890 MiB after the
third round, with a peak of 1,121 MiB and a 378 MiB block cache. At 1.05M the same
figures were 192 MiB, 394 MiB and a peak of 419 MiB, with a 64 MiB cache. This probe
uses a fresh process; its figures are distinct from the full suite's post-run RSS.

At 10.5M the largest per-query RSS rises were `order-by-full` at 261 MiB,
`two-hop-count` at 228 MiB, `star-join` at 173 MiB and `optional-chain` at 163 MiB.
These include cache growth as well as intermediate results. QLever's retained rises
include 135 MiB for `distinct-join` and 124 MiB for `knows-reach`.

## Memory and the speed it buys

Sparkles trades memory for speed by default. These are the defaults that do it:

* **The decoded-block cache** keeps decompressed index blocks, up to 1 GiB per dataset
  (`--cache-mb`). It accounts for much of the RSS gap with QLever.
* **The query result cache** keeps answers to repeated queries, up to 512 MiB per dataset
  (`--result-cache-mb`). It was off in every run on this page.
* **Materialized intermediate results** consume memory within the query's row and
  memory budgets. The main 10.5M suite peaked at 1,408 MiB under concurrent load,
  against QLever's retained 778 MiB reference.
* **mimalloc** keeps freed memory in per-thread heaps. The server hands free memory back
  to the OS after it has been idle for a second (`--idle-release-ms`, default 1000).

The cache-size runs measured the same 10.5M suite on `forge` on 2026-10-06.
The first three memory columns come from a separate fresh-server probe: all 28 queries
once, followed by three rounds of 160 concurrent star joins. Full-suite RSS, throughput
and query means come from the canonical suite. These phases are kept separate; the
1 GiB row here is this cache cohort's control, rather than the main run above.

| Block cache | Probe RSS at start | Probe RSS after queries once | Probe peak RSS | Full-suite post-run RSS | Held by cache after probe | Suite throughput (q/s) | Sum of 28 query means |
|---|---:|---:|---:|---:|---:|---:|---:|
| 0 MiB | 47 MiB | 148 MiB | 599 MiB | 393 MiB | 0 MiB | 124 | 3,577 ms |
| 256 MiB | 49 MiB | 582 MiB | 989 MiB | 754 MiB | 255 MiB | 221 | 1,077 ms |
| 1024 MiB (default) | 49 MiB | 729 MiB | 1140 MiB | 879 MiB | 378 MiB | 229 | 1,057 ms |
| 4096 MiB | 49 MiB | 730 MiB | 1138 MiB | 861 MiB | 378 MiB | 236 | 1,062 ms |

Disabling the cache saves 486 MiB of full-suite post-run RSS against this cohort's
default, and brings it below QLever's 653 MiB reference. It costs about 46% of the
throughput and makes the suite 3.4× slower in total. The 256 MiB configuration saves
125 MiB with only about 4% lower throughput here. Increasing the cache to 4 GiB adds
no useful capacity for this workload, which touches about 378 MiB of blocks.

The default's full-suite post-run RSS less its block cache is about 501 MiB in this
cohort. That subtraction describes resident memory composition; a server without a
cache has a different allocation and query history. The main suite's corresponding
figures are 952 MiB total and 573 MiB less cache.

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

The warm synthetic comparison has one clear latency loss: `range-topk` at 1.05M,
7.73 ms against QLever's 6.37 ms, or 1.21×. Sparkles is fastest on 24 of 28 queries
at this size and all 28 at 10.5M. The other first-place differences at 1.05M are
Fluree's small advantages on counts and a lookup; all are within 10%.

| Case | Reference advantage | Qualification |
|---|---|---|
| Warm `range-topk`, 1.05M | QLever 1.21× | Salary filtering and top-k; at 10.5M Sparkles is faster. |
| Queries after 5,000 commits, 1.05M | QLever 1.32× on `range-topk`; Fluree 1.11× on `two-hop-count` | The latter differs by 0.82 ms. At 10.5M no changed-state query loses by more than 10%. |
| Cold point lookups | Oxigraph 1.8–3.5× at 1.05M and 2.4–3.0× at 10.5M | See [Cold starts](#cold-starts), including the separate request-only timing method. |
| Other cold queries, 1.05M | QLever 1.17× on `range-topk`; Fluree 1.2–2.3× on several scans/counts | First-use index and vocabulary reads differ from warmed execution. |
| Server RSS after the run | QLever 1.42× at 1.05M and 1.46× at 10.5M | Sparkles holds a decoded-block cache and intermediate results; subtracting the resident cache leaves 372/573 MiB. |
| Peak RSS under 16 readers | QLever 1.88× at 1.05M and 1.81× at 10.5M | 631/1,408 MiB versus 335/778 MiB. |
| Text index size | QLever about 1.19× at 1.05M and 1.24× at 10.5M | Index predicates and features differ; QLever indexes all literals. |
| 5,000 serial single-triple commits | Oxigraph 1.14× at 1.05M and 1.78× at 10.5M | Sparkles synchronizes each acknowledgment; Oxigraph does not. Batch and concurrent results are separately reported. |

Small differences remain on selective lookups, counts, single-triple HTTP latency and
one text COUNT. The page treats differences within 10% as descriptive ties. WatDiv
has no template loss and only one instance loss, 1.1%, also a tie. QLever's highlight
form does not highlight words, so that comparison cannot isolate highlighting cost.

The canonical 30-second mixed workload is reported under [Updates and mixed
load](#updates-and-mixed-load); its 16 star readers are different from the specialized
snapshot-reader write workload. Neither write result should be interpreted without
the completed read rates, client/batch counts and durability contract.

Sparkles materializes results before serializing them, even when the HTTP response
streams. That is an architectural memory limit; the measured `export-500k` is faster
than the retained comparator results at both sizes. The separate historical DBpedia
section records billion-scale memory and cold-read costs that were not rerun here.

## Sparkles HTTP latency

These Sparkles-only measurements ran on `forge` on 2026-10-06, with Rust 1.99.0,
a release build and default features. The binary's SHA-256 is
`ce28cbfb2f4981b91755fa25c61c0e4e17eebb195c002f579c5786290d282f8e`.
The result cache was off (`--result-cache-mb 0`), and the decoded-block cache used
its default size. The client ran on CPU 11 and the server on CPUs 0–9.

Each query had 30 warm-ups and 20 timed requests in each of two fresh server
processes. The table gives the pooled median of 40 samples in milliseconds, including
HTTP and the full TSV response, with a warm page cache. It excludes starting a
`curl` process, so these numbers are not directly comparable with the hyperfine
means in the five-engine tables. The datasets contain 1,052,801 and 10,527,319 triples.
Recorded response bags were stable across both processes at each size.
For `export-500k`, the unordered limit is checked by row count. The main suite
separately verifies standard SPARQL results against reference answers; this HTTP
cohort records TSV fingerprints and row counts.

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

## Updates and mixed load

Sparkles' changed-state and commit measurements below are from 2026-10-06; competitor
references retain 2026-10-03. [HTTP write snapshot](#http-write-snapshot) uses separately
dated measurements with explicit client counts, affinity and batch sizes.

The `updates` mode copies each engine's store, applies the same 5,000 single-triple
INSERT/DELETE requests one at a time, and reruns the query suite. These changes remain
in Sparkles' delta until a compaction folds them into the base index.

| Churn of 5,000 commits | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: commits/s | 748 | 14 | 426 | 499 | **855** |
| 1.05M: latency p50 / p99 (ms) | 1.32 / 2.37 | 74.06 / 102.20 | 2.27 / 4.57 | 1.68 / 7.16 | **1.16 / 2.04** |
| 10.5M: commits/s | 640 | 14 | 475 | 459 | **1,140** |
| 10.5M: latency p50 / p99 (ms) | 1.41 / 3.51 | 74.69 / 103.16 | 2.07 / 4.73 | 1.55 / 12.76 | **0.90 / 1.58** |

No engine failed a commit. After the changes Sparkles is fastest on 24 of 28 queries
at 1.05M and 26 of 28 at 10.5M. At 1.05M its clear losses are `range-topk` to QLever
(1.32×) and `two-hop-count` to Fluree (1.11×); the remaining losses are small ties.
At 10.5M the differences on `count-all` and `two-hop-count` are within 10%.
QLever's changed-state `types-grouped` answer differs in value and is not ranked.

At 10.5M the delta changes `star-join` from 30.6 to 57.0 ms, `two-hop-count` from
17.0 to 37.4 ms, `contains` from 7.0 to 14.3 ms and `regex-iri` from 8.3 to 16.6 ms.
Throughput falls from 232 to 165 q/s, compared with QLever's retained 87 q/s.
Post-run RSS rises from 952 to 1,398 MiB, of which 719 MiB is resident block cache.
The changed-state timing records and semantic checks cover all 28 queries.

### Commit latency

The 5,000-request run gives Sparkles median single-triple commit times of 1.32 ms at
1.05M and 1.41 ms at 10.5M, with rates of 748 and 640 commits/s. Both the WAL and dirty
vocabulary data are synchronized before acknowledgment. Oxigraph's retained default
rates are higher without per-commit synchronization; QLever keeps writes in memory.

The WAL preallocates space by writing and synchronizing zeros, starting with 64 KiB and
growing up to 4 MiB. `--wal-prealloc-kb` sets that limit; `0` disables preallocation.
Replay, readers, backups and quotas use the logical end of the last commit. Close trims
unused space, and recovery recognizes a torn transaction tail. Copied stores are
flushed before timing.

### Mixed load

Sparkles measurements are from 2026-10-06; competitors retain the 2026-10-03 results.
The `mixed` mode runs 16 concurrent `star-join` readers with `oha` for 30 s while one
writer commits single-triple `INSERT DATA` requests as fast as it can. Each insert adds
a fresh literal, so these writes include vocabulary durability work. Every request opens
a new connection.

| Mixed load | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: reads/s | **2076.9** | 94.5 | 510.6 | 34.6 | 10.9 |
| 1.05M: read p50 / p99 (ms) | **7.5 / 11.9** | 158.4 / 351.3 | 27.4 / 39.7 | 462.2 / 536.7 | 1192.3 / 3351.1 |
| 1.05M: writes/s | 252.3 | 28.3 | 109.4 | 246.5 | **1247.3** |
| 1.05M: write p50 / p99 (ms) | 3.0 / 10.9 | 17.9 / 97.7 | 6.0 / 25.8 | 3.6 / 16.2 | **0.8 / 1.0** |
| 1.05M: failed reads / writes | 0 / 0 | 0 / 0 | 23929 / 827 | 0 / 0 | 0 / 0 |
| 10.5M: reads/s | **215.8** | 7.7 | 69.5 | 3.6 | 0 |
| 10.5M: read p50 / p99 (ms) | **72.8 / 96.9** | 1977.0 / 2859.2 | 229.5 / 270.6 | 4402.7 / 8621.1 | — |
| 10.5M: writes/s | 127.2 | 20.7 | 51.1 | 299.4 | **676.7** |
| 10.5M: write p50 / p99 (ms) | 7.7 / 12.1 | 62.8 / 86.7 | 11.9 / 108.8 | 1.9 / 14.8 | **1.4 / 3.2** |

Across three Sparkles runs, reads ranged from 1,877–2,077/s at 1.05M and 201–218/s
at 10.5M; writes ranged from 153–252/s and 122–128/s. All completed with zero errors.
The table shows the first run, preserving each run's paired read/write measurements.
Sparkles serves about 4.1× QLever's reads at 1.05M and 3.1× at 10.5M in that run.
Oxigraph commits more writes at both sizes, and Fluree does at 10.5M, but their
reads are far slower. At 10.5M no Oxigraph reader finished within the 30 s. QLever
failed 23,929 reads and 827 writes at 1.05M, and its log does not say why.

### HTTP write snapshot

The one-client table below uses the 1.05M-triple dataset on the same Intel i5-13500
host. These are release-build measurements from 2026-10-06, using rustc 1.99.0 and
default server features. Rows are separately collected cohorts; the query tables retain
their separately stated scope. Each run starts from its own durably copied store. Sparkles starts from a compacted base;
automatic compaction remains enabled and no compaction runs during these measurements.
The server uses one logical CPU and the HTTP client uses a separate logical CPU. Result
caches are disabled. Preparation, exports, validation and process shutdown occur outside
the request timers.

Every Sparkles acknowledgment follows synchronization of the write-ahead log and any new
vocabulary data. Full before/after states and effective changes are checked against the
operation manifest. The batched runs check exact per-request changes and contiguous
commit headers; the original serial run checks successful request totals and the
corresponding commit-head advance. These ordinary rows verify the complete state and
stop the owned server afterward; they do not include a separate reopen check. The
concurrent-reader workload below adds actual close/reopen and history verification. None
of these checks is a power-loss test.

| Default writes, one client | Effective changes per run | Changes per request | Fresh processes | Effective changes/s | Request p50 (ms) | Request p99 (ms) |
|---|---:|---:|---:|---:|---:|---:|
| Single-triple insert/delete churn | 5,000 | 1 | 2 | 1,046–1,062 | 0.856–0.870 | 1.536–1.609 |
| Inserts introducing a new literal | 50,000 | 100 | 2 | 53,979–59,414 | 1.493–1.515 | 3.964–7.118 |
| Insert/delete batches | 50,000 | 100 | 4 | 54,972–58,586 | 1.516–1.548 | 2.857–3.981 |

The batched rows contain 500 acknowledged transactions each. The fresh-literal manifest
preserves existing subjects and the predicate, introducing one new literal per inserted
quad. The insert/delete manifest is a separate workload. A request can contain multiple
DATA operations but commits once. Throughput counts effective changes, so it is not
interchangeable with transactions per second.

Ranges show the lowest and highest observed process results; latency ranges contain each
process's own percentile. They are not confidence intervals or percentiles pooled across
processes. The batched runs are short, and their p99 varies substantially even when
their median stays close. These measurements do not establish an advantage for every
write workload or sustained operation over hours.

On the same fresh-literal manifest, request size, client count and CPU placement, two
Oxigraph 0.5.11 processes produced 34,641 and 58,853 effective changes/s, with request
medians of 2.591 and 1.617 ms. Its default RocksDB acknowledgments do not synchronize
the WAL, so their durability contract differs from Sparkles'. The observed throughput
ranges overlap. These separate engine runs are a small reference, not an interleaved
statistical comparison.

At 10.5M triples, the one-client tests use the existing store fixture and the same
one-CPU server/separate-CPU client placement. Startup logs show age-triggered compaction
admission, without a completion timestamp. These rows retain that maintenance
qualification and are separate from the prepared compacted 1.05M table.

| Default writes at 10.5M, one client | Effective changes per run | Changes per request | Fresh processes | Effective changes/s | Request p50 (ms) | Request p99 (ms) |
|---|---:|---:|---:|---:|---:|---:|
| Single-triple insert/delete churn | 5,000 | 1 | 2 | 320–494 | 2.154–2.351 | 5.989–7.733 |
| Insert/delete batches | 50,000 | 100 | 2 | 45,788–46,123 | 1.613–1.622 | 16.070–16.364 |

Each batched request performs 50 effective inserts and 50 effective deletes. Every run
passes the same count, commit-head and complete-state checks. The serial repeats vary
considerably, and the batched p99 is much higher than its median. No corresponding
Oxigraph run at this size is included in this snapshot.

### Concurrent durable commits

At 1.05M triples, this workload applies the same 50,000 fresh-literal changes as the
one-client batch table, using four clients and one quad per request. The server uses ten logical CPUs and
the client process uses a separate logical CPU. It begins from a durably copied
compacted base with automatic compaction enabled; pre/post and post-close checks show no
pending, running or completed automatic compaction. The same binary runs in
default/group/group/default order, with two fresh processes per setting. The only
server-option difference is `--experimental-group-commit`. Grouping is off by default
and seals immediately, without an artificial collection delay.

| 50,000 effective changes, four writers | Default | Experimental group commit |
|---|---:|---:|
| Acknowledged transactions/s | 221.5–231.8 | 529.0–543.3 |
| Request p50 (ms) | 19.016–19.211 | 9.051–9.104 |
| Request p99 (ms) | 39.972–41.342 | 15.693–15.894 |
| Write interval (s) | 215.7–225.8 | 92.0–94.5 |

Across these two processes per setting, mean transaction throughput is 2.37× the default
and the mean of process medians is about 53% lower. These are descriptive comparisons of
this workload, not significance estimates. Each process completes exactly 50,000
effective inserts with contiguous receipts and the correct complete final state.
Ordinary acknowledgment durability is preserved; group commit can share synchronization
across eligible commits while publishing each acknowledged prefix. This workload has no
concurrent readers; the reader test below is separate. It does not establish a gain for
serial clients, all write shapes or hours of operation.

At 10.5M triples, the same manifest, four clients, CPU placement and run order produce
the following results, again with two fresh processes per setting:

| 10.5M, 50,000 effective changes, four writers | Default | Experimental group commit |
|---|---:|---:|
| Acknowledged transactions/s | 201.1–204.0 | 402.3–406.9 |
| Request p50 (ms) | 19.338–19.415 | 9.709–9.735 |
| Request p99 (ms) | 41.066–41.200 | 16.089–16.171 |

Mean transaction throughput is 2.00× the default, with mean process medians about 50%
lower and mean process p99 about 61% lower. All 200,000 effective inserts across the
four processes pass complete-state and contiguous-receipt checks. This uses the existing
10.5M fixture, whose startup logs show age-triggered compaction admission without a
completion timestamp; it does not have the prepared 1.05M fixture's maintenance guards.

### Concurrent writers with snapshot readers

This workload inserts 5,000 fresh quads using four writer clients while two scalar
readers each issue up to 25 queries/s. Reads count the inserted namespace and verify the
count against the commit snapshot reported in the response. It uses fresh subjects, a
fresh predicate and fresh objects; its vocabulary workload differs from the one-client
fresh-literal table. The server uses ten logical CPUs and the client process uses a
separate CPU. Result caching and reasoning are disabled. Connections are opened per
request.

The same Sparkles binary runs with the default writer and with experimental group
commit, in default/group/group/default order. Group commit remains opt-in and preserves
the acknowledgment durability contract.

| 1.05M, 5,000-quads run, four writers + two readers | Default | Experimental group commit |
|---|---:|---:|
| Single-quad requests: acknowledged transactions/s | 1,375–1,386 | 3,344–3,365 |
| Single-quad requests: writer p50 (ms) | 2.796–2.810 | 1.142–1.143 |
| Single-quad requests: reader p50 (ms) | 0.845–0.862 | 0.545–0.585 |
| Single-quad requests: completed reader responses during writes | 182 per run | 76 per run |
| 100-quad requests: acknowledged transactions/s | 742–752 | 1,186–1,233 |
| 100-quad requests: effective changes/s | 74,231–75,187 | 118,627–123,253 |

The 1.05M single-quad runs last about 3.61–3.64 s by default and 1.49–1.50 s with group commit.
Their reader rate is deliberately limited; the shorter write interval explains why fewer
read responses fit within it. Readers observe advancing commit snapshots, with full
state, history and reopen checks afterward.

The 100-quad runs have only 50 transactions and last about 41–67 ms. They produce only
two to four reader responses, so their throughput is a short-burst observation and their
reader tails are not a sustained-load estimate. Both settings pass the same receipt and
state checks. These results do not replace the separate 30-second star-join mixed-load
comparison.

At 10.5M triples, the same 5,000-change, single-quad reader workload uses the existing
store fixture. Startup logs show age-triggered compaction admitted about 23–25 s before
the write timer, but do not record its completion. These rows therefore have a different
startup and maintenance qualification from the compacted 1.05M controls above.

| 10.5M, 5,000 single-quad requests, four writers + two readers | Default | Experimental group commit |
|---|---:|---:|
| Acknowledged transactions/s | 202–231 | 370–386 |
| Writer p50 (ms) | 19.093–19.274 | 9.932–10.315 |
| Writer p99 (ms) | 40.221–40.947 | 15.735–17.861 |
| Reader p50 (ms) | 0.917–0.936 | 0.895–1.000 |
| Reader p99 (ms) | 1.410–1.549 | 1.960–2.143 |
| Completed reader responses during writes | 1,084–1,240 | 648–678 |

Both runs per setting pass the full-state, receipt, snapshot-count, history and actual
reopen checks. Mean write throughput with grouping is about 75% higher in these rows.
Reader medians are similar; reader p99 is higher by about 0.57 ms in the mean of process
percentiles. The write intervals are 21.7–24.8 s by default and 12.9–13.5 s with
grouping. Readers maintain the scheduled rate of about 50 queries/s in total and observe
advancing snapshots throughout. These results establish productive reads under this
load, rather than saturated query throughput.

## Cold starts

Cold mode restarts the server and requests eviction of its files from the OS page
cache before each query, three times per query. This is file-cache eviction, not a
guarantee of physical-device coldness. Request times are medians of three successful
curl requests, excluding process startup; readiness is measured separately.
Sparkles values are from 2026-10-06, competitors from 2026-10-03. The 1.05M
`types-grouped` cell uses an additional three-sample cohort: 4.9 ms median,
4.8–5.3 ms range. The other queries use the complete-suite cohort.

| Cold, median of 3 | 1.05M | 10.5M |
|---|---|---|
| Sparkles fastest | 18 of 28 | 24 of 28 |
| Time to ready after restart | 0.091 s | 0.087 s |

| Query, cold | Faster reference (1.05M / 10.5M) | 1.05M | 10.5M |
|---|---|---|---|
| `count-all` | fluree / — | 2.31× (3.1 against 1.3 ms) | — |
| `star-join` | fluree / — | 1.18× (41.3 against 34.9 ms) | — |
| `range-topk` | qlever / — | 1.17× (26.8 against 23.0 ms) | — |
| `contains` | fluree / — | 1.21× (15.0 against 12.4 ms) | — |
| `group-avg` | qlever / — | 1.03× (35.7 against 34.8 ms) | — |
| `path-plus` | oxigraph / oxigraph | 3.48× (6.8 against 1.9 ms) | 2.60× (5.0 against 1.9 ms) |
| `lang-filter` | fluree / — | 1.41× (12.5 against 8.9 ms) | — |
| `star-lookup` | oxigraph / oxigraph | 1.79× (26.6 against 14.9 ms) | 2.99× (46.7 against 15.7 ms) |
| `values-star` | oxigraph / oxigraph | 2.35× (13.0 against 5.5 ms) | 2.42× (11.6 against 4.8 ms) |
| `employee-docs` | fluree / oxigraph | 1.91× (19.4 against 10.2 ms) | 2.58× (33.0 against 12.8 ms) |

The clearest differences are sparse point lookups and several small-dataset scans or
statistics queries. Warm execution hides their first-use reads and block decoding.
At 10.5M Sparkles remains fastest on 24 of 28 cold queries; its four losses are
`star-lookup`, `values-star`, `employee-docs` and `path-plus`.

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
dense scans. These configuration measurements used `forge` on 2026-10-06 and
three cold requests per query. "Both off" sets
`SPARKLES_IO_HINTS=off SPARKLES_SPARSE_VOCAB=off`.

| 10.5M, cold, median of 3, ms | Both off | Both on (default) |
|---|---:|---:|
| `star-lookup` | 69.3 | **46.7** |
| `star-join` | 99.9 | **93.3** |
| `contains` | 82.4 | **33.5** |
| `regex-iri` | 55.2 | **31.3** |
| `lang-filter` | 36.1 | **27.9** |

This comparison changes both options together and uses separate runs. It does not
isolate either option or establish statistical significance.

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

WatDiv v0.6 basic testing at scale 100 has 10,973,381 triples, 20 templates and five
instances per template, except C1–C3, which have one: 88 instances. All answers agree
with the retained independent reference multisets. Sparkles ran on 2026-10-06; the
other engines retain 2026-10-03. Timings are geometric means over each template's
five-run instance means after one warmup. Category means weight templates equally.

| load (s) | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| load | **3.77** | 33.61 | 9.71 | 9.46 | 13.16 |

| template | instances | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|---:|
| L1 | 5 | **3.80** | 6.87 | 9.77 | 4.01 | 4.70 |
| L2 | 5 | **3.92** | 12.2 | 9.39 | 5.48 | 15.5 |
| L3 | 5 | **3.83** | 6.98 | 7.11 | 4.07 | 4.59 |
| L4 | 5 | **3.84** | 8.23 | 7.06 | 4.36 | 8.28 |
| L5 | 5 | **4.16** | 11.4 | 11.1 | 4.77 | 13.7 |
| S1 | 5 | **4.28** | 7.63 | 95.7 | 4.84 | 7.06 |
| S2 | 5 | **4.11** | 7.68 | 8.07 | 4.89 | 7.02 |
| S3 | 5 | **3.96** | 10.7 | 11.0 | 5.43 | 15.6 |
| S4 | 5 | **3.85** | 13.2 | 8.00 | 5.29 | 29.9 |
| S5 | 5 | **3.85** | 8.03 | 10.2 | 4.73 | 13.8 |
| S6 | 5 | **3.80** | 7.25 | 7.64 | 4.75 | 6.56 |
| S7 | 5 | **3.67** | 6.45 | 9.67 | 3.90 | 3.93 |
| F1 | 5 | **4.03** | 8.45 | 16.2 | 4.42 | 14.4 |
| F2 | 5 | **3.99** | 7.88 | 32.3 | 4.52 | 8.67 |
| F3 | 5 | **3.89** | 7.51 | 18.5 | 28.0 | 6.17 |
| F4 | 5 | **4.52** | 7.80 | 56.3 | 26.0 | 8.94 |
| F5 | 5 | **4.16** | 7.39 | 20.2 | 4.61 | 7.47 |
| C1 | 1 | **6.96** | 16.8 | 33.9 | 8.16 | 36.5 |
| C2 | 1 | **8.41** | 140 | 54.9 | 39.7 | 369 |
| C3 | 1 | **49.7** | 10247 | 289 | 104 | 23122 |

| category | templates | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|---:|
| **linear (L)** | 5 of 5 | **3.91** | 8.86 | 8.74 | 4.51 | 8.24 |
| **star (S)** | 7 of 7 | **3.93** | 8.47 | 12.6 | 4.81 | 9.73 |
| **snowflake (F)** | 5 of 5 | **4.11** | 7.80 | 25.6 | 9.23 | 8.75 |
| **complex (C)** | 3 of 3 | **14.3** | 289 | 81.3 | 32.3 | 678 |
| **all templates** | 20 of 20 | **4.82** | 14.2 | 18.2 | 7.41 | 17.2 |


Sparkles is fastest on all 20 templates and 87 of 88 instances. Fluree is faster on
`L5-1` by 1.1%, a descriptive tie; Sparkles and Fluree are within 10% on 27 instances,
and Sparkles and Oxigraph on five. The common-correct template geometric mean is
4.82 ms. Many selective cases are near the HTTP process floor. C3 returns 419,866 rows
in 49.7 ms, versus Fluree's 104 ms and QLever's 289 ms.

WatDiv was created by G. Aluç, O. Hartig, M. T. Özsu and K. Daudjee, “Diversified Stress
Testing of RDF Data Management Systems,” ISWC 2014. Instance results are retained
alongside the template summaries.

## Full-text search

The comparison uses Sparkles' Tantivy `text:query`, Fuseki's Lucene-backed jena-text,
and QLever's `ql:contains-word`. Sparkles and Jena index `foaf:name`, `ex:title` and
`rdfs:label`, with English title stemming. QLever indexes all literals and joins on
predicates; it searches the indexed form for the stemmed case and does not highlight.
Hit sets and counts agree under these query contracts. Scores and tied ranking order
are not asserted equal. Sparkles ran on 2026-10-06; competitors retain 2026-10-03.

| Text index | sparkles | jena-fuseki | qlever |
|---|---:|---:|---:|
| 1.05M: build (s) | 0.36 | 4.37 | 1.86 |
| 1.05M: size (MB) | 13.3 | 24.9 | 11.1 |
| 10.5M: build (s) | 6.01 | 25.98 | 19.35 |
| 10.5M: size (MB) | 123.5 | 227.8 | 99.9 |

Times are TSV HTTP mean ± standard deviation in ms: ten runs at 1.05M, five at 10.5M.

| query | sparkles 1.05M | jena-fuseki 1.05M | qlever 1.05M | sparkles 10.5M | jena-fuseki 10.5M | qlever 10.5M |
|---|---:|---:|---:|---:|---:|---:|
| rare-top10 | **3.9 ± 0.2** | 12.7 ± 1.5 | 6.7 ± 0.4 | **5.5 ± 0.2** | 15.0 ± 1.9 | 7.8 ± 0.5 |
| common-top10 | **4.2 ± 0.1** | 11.7 ± 1.5 | 7.8 ± 0.6 | **8.0 ± 0.2** | 14.9 ± 2.8 | 17.5 ± 2.7 |
| common-count | **4.7 ± 0.3** | 53.9 ± 3.4 | 7.2 ± 0.7 | 14.8 ± 0.5 | 406.9 ± 4.3 | **13.5 ± 1.1** |
| text-join | **5.5 ± 0.2** | 66.0 ± 1.5 | 11.3 ± 0.8 | **21.7 ± 0.7** | 585.4 ± 5.1 | 46.6 ± 7.6 |
| conjunction | **4.3 ± 0.1** | 15.0 ± 1.5 | 7.0 ± 0.3 | **8.2 ± 0.3** | 80.7 ± 0.8 | 14.3 ± 1.0 |
| highlight | **11.1 ± 0.3** | 101.8 ± 8.7 | 13.7 ± 1.3 | **69.2 ± 0.2** | 841.8 ± 46.2 | 94.8 ± 6.5 |
| stemmed | **4.0 ± 0.2** | 12.2 ± 0.6 | 5.9 ± 0.3 | **5.5 ± 0.2** | 53.5 ± 0.9 | 18.6 ± 2.6 |

Sparkles is fastest on all seven queries at 1.05M and six at 10.5M. QLever is slightly
faster on `common-count` at 10.5M, within 10%. That query counts 49,837 hits;1.05M has 4,937.
QLever's text index is smaller, 123.5 versus 99.9 MB at 10.5M and 13.3 versus 11.1 MB at 1.05M.
The highlight form returns plain literals in QLever and marked words in Sparkles/Jena.

## Vector search

This Sparkles-only snapshot ran on Forge on 2026-10-07 with 100,000 clustered
384-dimensional vectors and cosine distance. Each configuration answered 1,000
top-10 queries after 20 warmups, using a persistent HTTP connection and full TSV
responses. Recall is relative to Sparkles' exact search on the same vectors;
there is no independent vector-engine comparison in this suite.

| Search | Recall@10 vs exact | Mean (ms) | p50 (ms) | p99 (ms) |
|---|---:|---:|---:|---:|
| Exact | 1.0000 | 5.438 | 5.455 | 6.109 |
| HNSW `ef=16` | 0.9875 | 0.325 | 0.265 | 0.899 |
| HNSW `ef=32` | 0.9989 | 0.323 | 0.303 | 0.500 |
| HNSW `ef=64` | 0.9999 | 0.440 | 0.423 | 0.778 |
| HNSW `ef=128` | 0.9999 | 0.676 | 0.671 | 0.839 |
| HNSW `ef=256` | 0.9999 | 1.218 | 1.212 | 1.542 |

The HNSW build used `M=16` and `efConstruction=128`, taking 15.62 s with
867.9 MiB peak process RSS. The mapped index files occupy 170.5 MB; server RSS
after the query sweep was 248.0 MiB. Approximate search trades recall for latency,
and these figures describe this synthetic distribution rather than all vectors.

## Path search

These Sparkles-only runs compare `SERVICE path:search` with property paths on the
benchmark's `foaf:knows` graph. They ran on Forge on 2026-10-06 with a release build,
a warm server and the result cache off. Each query had one warmup and 31 measured
requests, interleaved with the other queries. The client was pinned to CPU 11; the
server inherited CPUs 0–19. Times include a fresh HTTP connection, form POST and
complete TSV response, without starting a `curl` process.

Answers were checked against an independent graph traversal of the input triples:
reachability, distances, shortest-path counts, five simple-path lengths and every
returned path edge. The `+` path from person 0 reaches 89,090 nodes at 1.05M and 892,556
at 10.5M. The last person is 99,999 or 999,999, with shortest chains of 12 and 17 edges.
QLever's path-search service uses other parameters and was not measured here.

| query | what it does | 1.05M min | 1.05M median | 10.5M min | 10.5M median |
|---|---|---:|---:|---:|---:|
| `reach-plus` | `COUNT` of `ex:0 foaf:knows+ ?x` | 11.47 | 12.65 | 138.54 | 139.41 |
| `ps-any` | `COUNT` of one shortest path to every reachable person | 17.07 | 17.43 | 244.73 | 257.10 |
| `ask-pair` | `ASK { ex:0 foaf:knows+ ex:last }` | 10.87 | 11.65 | 130.18 | 131.37 |
| `ps-pair` | the shortest path between the same pair, one row per edge | 0.84 | 1.00 | 3.02 | 3.30 |
| `ps-pair-all` | every shortest path between them (1 at 1.05M, 3 at 10.5M) | 0.74 | 0.87 | 2.84 | 3.08 |
| `ps-pair-both` | shortest path with `path:direction path:both` (7 or 8 edges) | 0.76 | 0.88 | 1.57 | 1.68 |
| `ps-pair-k5` | five shortest simple paths, by Yen's algorithm | 11.36 | 11.95 | 77.12 | 77.77 |
| `ps-values10` | distances from 10 people bound by `VALUES` to the last person | 2.42 | 2.57 | 15.02 | 15.23 |
| `ps-all4` | `COUNT` of every path of up to 4 edges from person 0 | 0.35 | 0.43 | 0.59 | 0.73 |
| `pp-len4` | the same count as a `UNION` of four fixed-length property paths | 0.47 | 0.53 | 0.68 | 0.81 |
| `ps-any-weighted` | `ps-any` with `path:weight`, using Dijkstra's algorithm | 94.39 | 96.68 | 1,582.32 | 1,606.80 |

Times are milliseconds. The pair search's median is about 12–40× faster than the
property-path ASK. Bidirectional search stops when the frontiers meet; the property
path explores the reachable graph. Searching from one person to everyone costs
about 1.4–1.8× the `+` count and also maintains path parents and levels. Weighted
search costs about 5.5–6.2× the unweighted search. No edge has a weight in this fixture,
so the default unit weight produces the same reachability count.

The two up-to-four-edge counts agree at 21 and 43 on these fixtures; this does not
establish equivalence of simple paths and walks on graphs with short cycles. The
`VALUES` queries return distances for 8 and 7 reachable sources out of 10. This suite
checks Sparkles' answers against the input graph, rather than comparing path-service
syntax or performance with another engine.

## Query execution features

Each executor optimization can be switched off with `SPARKLES_DISABLE_OPTIMIZATIONS`.
The Forge release-build comparison on 2026-10-06–07 ran the warm 10.5M suite with each of
33 switches in `Optimizations::NAMES` disabled individually, plus the default:
34 configurations. Each query had one warmup and five measured runs, using the main
suite's `curl`/`hyperfine` method and a client pinned to CPU 11. Times below are means;
a query is reported when its mean changes by more than 25%. The default configuration
served 241 `star-join` queries/s under 16 clients.

| Feature disabled | Effect at 10.5M |
|---|---|
| Correlated subquery decorrelation (`decorrelate_exists`) | `exists-join` 196× slower (2,827 against 14.4 ms), `not-exists` 63× slower (941 against 15.0 ms) |
| Leading-key top-k selection (`topk_first_key`) | `expr-order-key` 7.8× slower (177 against 22.7 ms) |
| Expression reuse (`expr_cache`) | `expr-bind-group` 5.1×, `expr-order-key` 3.5× and `expr-agg-arg` 1.9× slower |
| Batched joins (`batched_join`) | `values-star` 4.9×, `star-lookup` 3.2× and `employee-docs` 1.7× slower. Throughput 111 against 241 q/s. |
| Batched path traversal (`batched_paths`) | `knows-reach` 2.6× slower (385 against 145 ms) |
| Anti-joins (`anti_join`) | `minus` 2.6× slower (22.4 against 8.7 ms) |
| Counting joined runs (`count_join_runs`) | `two-hop-count` 2.1× slower (34.7 against 16.7 ms) |
| Counts from index metadata (`metadata_counts`) | `predicate-counts` 2.1× and `distinct-obj` 1.8× slower. Throughput 226 against 241 q/s. |
| Counting matching vocabulary runs (`count_filter_runs`) | `lang-filter` 1.8×, `contains` 1.7× and `regex-iri` 1.6× slower |
| Incremental aggregation (`incremental_group`) | `group-avg` 1.5× slower (83.4 against 53.9 ms) |
| Ordered top-k selection (`ordered_topk`) | `range-topk` 1.5× slower (25.4 against 16.4 ms) |
| Galloping index joins (`gallop_index_join`) | Throughput 130 against 241 q/s. No single query changed by more than 25%. |

All 34 configurations passed the answer checks for the 28-query suite; `export-500k`
was checked by its row count rather than an exact bag. Other features did not change
query means by more than 25%, apart from `expr-order-key` variability when disabling
spatial pushdown. Outside the two switches that directly changed that query, means
ranged from 20.2 to 29.1 ms, with a median of 22.7 ms, against 22.7 ms by default.
A single configuration run does not distinguish small effects from run variability.

Coverage matters. Spatial features need GeoSPARQL filters, text pushdown needs text
queries, and vector top-k needs vector search. Vocabulary prefix ranges need
`STRSTARTS` or anchored `REGEX`, as in DBpedia's `label-regex`. Prefetching matters on
cold servers, and delta statistics matter after writes. Planner estimates affect
latency when they change the selected plan. The fresh, warm synthetic suite does
not exercise all of those cases.

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
* **Write scope and long runs.** HTTP tests include 5,000 single-triple changes,
  50,000 effective changes in 100-change batches, and 50,000 single-quad changes
  from four concurrent writers. The separate snapshot-reader workload contains
  5,000 changes and is short. Hours of sustained writes and compaction under load
  were not compared with other engines.
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
| RDFS materialization, 1,001,418 → 2,751,996 inferred triples (Forge, 2026-10-06) | 3.838 s end to end; inference alone 2.287 s |
| SHACL validation, 20 shapes, 1.05M triples, 48,428 results (Forge, 2026-10-06) | 435 ms parallel, 934 ms sequential; medians of 5 |
| ShEx validation, 152,000 shape associations, 1.05M triples (Forge, 2026-10-06) | 1.512 s parallel, 2.192 s sequential; medians of 5 |
| `ASK { ?s ?p ?o }` / `SELECT * … LIMIT 100` at 10.5M (early termination) | 1.9 ms / 1.8 ms |

### Embedded JVM / Jena (1.05M quads)

Measured on 2026-10-07, these Sparkles-only results use the public Jena API through Kotlin, UniFFI and JNA on a local Intel Core Ultra X7 358H with 32 GB RAM. The dataset contains 1,052,801 quads. Java 21.0.12, Jena 5.6.0 and Rust 1.98.1/LLVM 22.1.8 run on four distinct CPU cores with Rayon 4 and an unchanged release build. The immutable source snapshot is `e12167c8`; the measured library SHA starts `f66ae0e1`.

All rows and terms are consumed. All 27 canonical query answers pass untimed lexical-bag checks against stored reference answers, and fallback remains zero. Result caching is disabled; the term cache uses its default 262,144 entries. Tables report the median of three fresh-JVM process medians. They describe this machine and method; they do not compare engines or hosts.

| Calibrated query | ms |
|---|---:|
| Count all | 0.282 |
| Types grouped | 0.238 |
| Path plus | 0.135 |
| Distinct objects | 0.161 |
| Predicate counts | 0.165 |
| Star lookup | 0.266 |
| VALUES star | 0.231 |
| Employee documents | 0.204 |

Each calibrated query uses 1,000 warmups and 100 measured repetitions per process. The following fully consumed work queries use 41 warmups and 41 repetitions per process:

| Work query | ms |
|---|---:|
| Star join | 3.423 |
| Group average | 5.510 |
| Full ordering, 102,000 rows | 54.022 |
| OPTIONAL chain | 7.926 |

Public graph operations include their automatic adapter scope. Small operations use 500 warmups and 101 repetitions; the complete name scan uses seven warmups and 11 repetitions:

| Graph operation | ms |
|---|---:|
| Contains any quad | 0.026 |
| Contains a bound subject/predicate | 0.032 |
| Default graph size | 0.024 |
| Find and consume 102,000 subject/name rows | 51.856 |

Fresh-JVM startup phase medians are 100.079 ms for native loading, 161.059 ms for Jena initialization and 13.553 ms for dataset open. These are separate sequential intervals. The store has already recovered its derived metadata, OS pages are warm, and the native library path is explicit. The measurements exclude bundled-library extraction, RDF parsing/index construction and device-cold startup.

Repeated applications can pass an existing Jena `Query` instead of parsing the same input text on every call:

```java
void run(DatasetGraph dataset, String sparql) {
    Query parsed = QueryFactory.create(sparql);
    try (QueryExec execution = QueryExec.dataset(dataset).query(parsed).build()) {
        RowSet rows = execution.select();
        while (rows.hasNext()) {
            Binding binding = rows.next();
            // Consume the binding.
        }
    }
}
```

A separate paired control uses 500 warmups and 101 repetitions in three alternating fresh-JVM pairs:

| Input | Count all ms | Star lookup ms | CONTAINS ms |
|---|---:|---:|---:|
| Query text | 0.274 | 0.381 | 1.039 |
| Pre-parsed Jena Query | 0.167 | 0.270 | 0.782 |

Every pair is faster with pre-parsed input, saving 0.09–0.34 ms per call. This removes Jena text parsing from the repeated call; adapter serialization and native parsing still occur.

### Compression (10.5M triples)

These runs measured Sparkles alone on `forge`, with a release build, on 2026-10-06. HTTP times are
`hyperfine` means over 5 runs and include `curl`. Server CPU is the server process's
user + system time per request.

| Response | Encoding | Bytes | Ratio | Wall (s) | Server CPU (s) |
|---|---|---:|---:|---:|---:|
| Full N-Quads export (`GET /bench/data`) | identity | 1,121,335,687 | 1.00 | 4.577 | 4.840 |
|  | gzip-6 | 74,937,481 | 14.96 | 9.430 | 14.287 |
|  | br-4 | 96,361,212 | 11.64 | 5.074 | 9.808 |
|  | zstd-3 | 83,282,032 | 13.46 | 4.635 | 6.110 |
|  | zstd-1 | 79,052,770 | 14.18 | 4.558 | 5.717 |
| `SELECT * … LIMIT 500000` as TSV | identity | 45,411,223 | 1.00 | 0.101 | 0.272 |
|  | gzip-6 | 2,919,841 | 15.55 | 0.427 | 0.678 |
|  | br-4 | 3,489,784 | 13.01 | 0.264 | 0.518 |
|  | zstd-3 | 3,134,075 | 14.49 | 0.105 | 0.322 |
|  | zstd-1 | 2,909,054 | 15.61 | 0.097 | 0.300 |

zstd streams the export as fast as identity and makes it 13–14× smaller. gzip doubles
the wall time and triples the server CPU. zstd-1 came out slightly smaller than zstd-3
on both responses.

`/$/backup` writes an N-Quads dump to a file. The times below are medians of 3 runs. The
CLI `sparkles backup --threads 16` with zstd-3 took 9.26 s, close to the HTTP
result; this workload did not benefit materially from the extra CLI threads.

| Codec | Seconds | Size |
|---|---:|---:|
| none | 5.24 | 1,121 MB |
| lz4 | 5.76 | 162 MB |
| zstd-3 | 9.31 | 81.5 MB |
| brotli-5 | 16.53 | 119.0 MB |
| gzip-6 | 44.67 | 74.9 MB |

`sparkles load` was timed on the 1.1 GB N-Triples file, raw and compressed. Times are
medians of 3 runs. CPU is the load's user + system time, and parallelism is CPU divided
by wall time.

| Input | File size | Seconds | CPU (s) | Parallelism |
|---|---:|---:|---:|---:|
| raw | 1,121 MB | 3.80 | 27.6 | 7.2 |
| lz4 | 160 MB | 4.20 | 36.8 | 8.7 |
| zstd-3 | 78.6 MB | 4.29 | 38.0 | 8.8 |
| gzip-6 | 81.9 MB | 4.47 | 37.8 | 8.5 |
| brotli-5 | 69.0 MB | 4.59 | 38.6 | 8.5 |

Decompression runs as a single stream in front of the parallel parser and costs
0.4–0.8 s here. The combined load process averages about 7–9 cores.

The full-text document store holds every string literal, 1,540,012 documents. With LZ4
it takes 117.0 MB and builds in 5.94 s. With zstd, the default, it takes 114.0 MB and
builds in 5.99 s. Build times are median wall times over three runs.

<a id="point-in-time-reads-105m-triples"></a>

### Point-in-time reads (1M triples)

These HTTP measurements ran on `forge` on 2026-10-06 with a 1,000,000-triple
base and 50,000 single-quad commits in one generation, written over four connections.
History retention covers all commits. Individual curl request times exclude process
startup; repeated rows use medians of 20 samples.

| Request | Time |
|---|---:|
| `COUNT(*)` at base + 25,000, first | 19.89 ms |
| The same, second / warm median | 1.04 / 0.95 ms |
| Selective read at base + 25,001, first / warm | 1.20 / 0.69 ms |
| `COUNT(*)` at sealed base, first / warm | 18.12 / 0.67 ms |
| Sealed base, first after restart (median of 3) | 14.40 ms |
| After restart, at base + 25,000 | 21.39 ms |

A separate native test on the same machine read the middle of a 50,000-commit history
in 42.33 ms on first use and 1.82 µs from its snapshot cache. A head diff of one commit
took 0.126 ms; 100 commits took 0.181 ms. Reopening and replaying all 50,000 commits
took 221.51 ms. Native timings exclude HTTP and have a different fixture and cache
history from the table.

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
database on Forge on 2026-10-07 (299.8 MB), with Sparkles alone, a release build
and a warm page cache. The `fs`
repository was on the same ext4 file system. Each figure is the median (min–max) of 3
rounds, in wall-clock time including process start.

| Case | Seconds | Size |
|---|---:|---|
| Full backup into an empty repository | 0.58 (0.58–0.61) | 299.8 MB logical, 281.1 MB stored, 513 MB/s |
| Incremental backup after 1,000 single-quad commits | 0.33 (0.32–0.33) | 55.2 KB added (3 new blobs, 29 reused) |
| Restore into a new directory (quick check) | 0.70 (0.69–0.76) | 300.0 MB |
| Verify at level `data` (every blob read and hashed) | 0.21 (0.21–0.21) | 33 GETs, ok |

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

This ignored release test ran on `forge` on 2026-10-06. It compares native
commit latency on the same store with the spatial index on and off. Each figure is a
median of 200 one-triple commits or 20 commits of 1,000 `geo:asWKT` points.

| | Index off | Index on |
|---|---:|---:|
| 1-triple non-geometry commit | 0.025 ms | 0.025 ms |
| 1,000-point commit | 3.546 ms | 4.119 ms |

Non-geometry commits showed no measurable index cost in this sample; the point batch
added 0.573 ms. These native-store measurements have a different durability and
request contract from the persistent HTTP write tables.

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

Both measurements are ignored tests of `sparkles-core`, run in release mode on
`forge` on 2026-10-06, with no concurrent benchmark or build.

`partial_compaction_at_scale` loads the 10.5M-triple benchmark data, then copies the
store for each delta and compacts once with `partial` forced to `always` and once to
`off`. Each delta is 90% new `foaf:knows` links and 10% deleted `foaf:age` quads, all
between existing terms. Concentrated deltas link 10 people to a narrow range of others
and delete consecutive ages; spread deltas choose people at random. The generation
has 2,254 blocks over seven permutations. Copies are flushed before timing.
These are single build timings per case, rather than scaling confidence intervals.

| Delta | Blocks rewritten | Partial build | Full build |
|---|---:|---:|---:|
| 1,000 concentrated | 102 | 0.112 s | 7.343 s |
| 10,000 concentrated | 107 | 0.584 s | 7.168 s |
| 100,000 concentrated | 117 | 0.234 s | 7.317 s |
| 1,000 spread | 1,201 | 0.572 s | 7.313 s |
| 10,000 spread | 1,223 | 0.597 s | 7.245 s |
| 100,000 spread | 1,223 | 0.669 s | 7.752 s |

Concentrated deltas touch relatively few blocks, while spread deltas rewrite about
54%. Full compaction merges the vocabulary and sorts every quad. Partial compaction
was 12–66× faster in these samples; writer-lock times ranged from 3.0 to 12.1 ms.
The automatic selection chose partial compaction for every delta here.

`compaction_lock_with_a_spatial_index` loads 100,000 point features and labels
(300,000 quads), enables the index, and makes 500 commits before each compaction
while another 100 commits arrive during its build. Three rounds compare building the
spatial base at the generation switch with building it alongside the generation.

| Case | Median writer lock at switch | Range |
|---|---:|---:|
| No spatial index | 4.9 ms | 4.0–12.5 ms |
| Spatial base built at switch | 64.5 ms | 56.4–67.2 ms |
| Spatial base built with generation | 25.3 ms | 5.3–37.7 ms |

Building the spatial base outside the writer lock reduces its cost at the switch,
though concurrent changes and publication still consume time under the lock.

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
