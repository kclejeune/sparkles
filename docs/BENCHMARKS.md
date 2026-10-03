# Benchmarks

This page compares Sparkles with Apache Jena (TDB2 and Fuseki), QLever, Fluree and
Oxigraph, and lists every case where Sparkles loses or ties. All the comparisons were
measured on 2026-10-02 on one machine, `forge`, with Sparkles at commit `4995963`. The
sections under [Other measurements](#other-measurements) time Sparkles on its own, and
most of them ran earlier on a different machine, as each section says.

To reproduce the runs, use `mise run bench [people] [workdir]` or `scripts/bench.sh`. Add
`--engines fluree` to re-measure one engine and merge its results into an existing run.
Add `--answers-only` to re-check every engine's answers without timing anything.
`mise run bench:watdiv [scale]` runs WatDiv, and `scripts/bench-text.sh` runs the
full-text comparison.

## Summary

| | 1.05M triples | 10.5M triples |
|---|---|---|
| Bulk load | **0.69 s** (Fluree 1.21, Oxigraph 1.24, QLever 1.30, TDB2 4.69) | **6.50 s** (QLever 9.33, Fluree 10.52, Oxigraph 13.18, TDB2 45.49) |
| Fastest of the five | 27 of 28 queries | 28 of 28 queries |
| Losses | `range-topk` to QLever, 1.19× | none |
| Ties within 10% | Fluree on `star-lookup`, `distinct-obj`, `count-all` and `employee-docs`. Oxigraph on `path-plus`. | Fluree on `distinct-obj`, `count-all` and `star-lookup`. Oxigraph on `values-star` and `path-plus`. |
| vs. QLever | Faster on 27 of 28, median 2.7× | Faster on 28 of 28, median 3.7× |
| vs. Fluree | Faster on 28 of 28, median 3.3× | Faster on 27 of 27, median 9.8× |
| vs. Fuseki | 1.6–89× faster, median 13×. Fuseki fails `knows-reach`. | 1.7–1,312× faster, median 78× |
| vs. Oxigraph | 1.05–448× faster, median 24× | 1.08–8,553× faster, median 85× |
| Update latency, 1 triple | 4.2 ms (**Oxigraph 3.4**, QLever 3.8, Fluree 6.6, Fuseki 30.5) | 4.9 ms (**Oxigraph 3.7**, QLever 3.9, Fluree 4.9, Fuseki 33.2) |
| Throughput, 16 clients | **1,225 q/s** (QLever 463, Fluree 243, Fuseki 119, Oxigraph 25) | **242 q/s** (QLever 92, Fluree 22, Fuseki 12, Oxigraph 1.7) |
| Server RSS after the run | 380 MiB (**QLever 286**, Oxigraph 1,246, Fuseki 1,846, Fluree 3,789) | 892 MiB (**QLever 676**, Oxigraph 2,384, Fluree 3,804, Fuseki 4,798) |
| Server RSS less the block cache | 316 MiB | 513 MiB |
| Bulk load peak RSS | **329 MiB** (Fluree 469, QLever 642, Oxigraph 657, TDB2 1,734) | 2,065 MiB (**QLever 1,927**, Fluree 3,098, TDB2 4,449, Oxigraph 6,221) |

On WatDiv at scale 100 (10.97M triples), Sparkles is the fastest of the five on all 20
query templates and on all 88 query instances. Its geometric mean is 4.95 ms, against
7.39 ms for Fluree, 14.4 ms for Fuseki, 17.0 ms for Oxigraph and 17.9 ms for QLever. On
31 of the 88 instances, Fluree or Oxigraph comes within 10%.

In the full-text comparison, Sparkles is the fastest on all 7 queries at 1.05M and on 6
of 7 at 10.5M. At 10.5M, QLever counts the hits of a common word 1.11× faster.

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
* **Queries:** `scripts/bench.sh` has 28 queries. Twenty of them were in earlier versions
  of this page. The other eight are `not-exists` and `exists-join`, three point lookups
  (`star-lookup`, `values-star` and `employee-docs`), and three queries with expressions
  in BIND, ORDER BY and aggregate arguments (`expr-bind-group`, `expr-order-key` and
  `expr-agg-arg`).
* **Engines:** no engine was allowed to answer from a result cache.
  * Sparkles at commit `4995963`: `sparkles serve --result-cache-mb 0`, with every other
    setting at its default.
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
    of the queries take. Without pinning, every engine's small requests took either about
    5 or about 11 ms, and the mode changed between runs. A trivial single-threaded HTTP
    server showed the same two modes. Numbers measured before 2026-10-02 did not pin the
    client and are not comparable with these.
  * Loads: 1 run each, with peak RSS from GNU time.
  * Answers are checked before timing. `scripts/bench-answers.py` fetches every
    engine's answer to every query as SPARQL JSON and fingerprints it by:
    * the row count;
    * the solution multiset by RDF term identity;
    * the same multiset with numeric literals compared by value, to 12 significant
      digits. Engines print the non-terminating averages of `group-avg` to different
      precisions.

    An engine whose answer differs *in value* from the majority is marked † and not
    ranked. That happened once, to QLever on `types-grouped` after the update churn. A ‡
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

### What ran, and the failures

The window ran `scripts/bench.sh` in its default, `updates`, `mixed` and `cold` modes at
both sizes, then WatDiv and the full-text benchmark. The default and `updates` modes,
WatDiv and the full-text benchmark ran for all engines with the client pinned. The
`mixed` and `cold` modes, the optimization A/B and the cache-size runs were repeated for
Sparkles only on the pinned build. The other engines' `mixed` and `cold` numbers come
from an earlier pass the same morning on the same machine. Those two modes do not use the
pinned client. `mixed` drives the load with `oha`, and `cold` times one request with
curl's own timer, which excludes the process start.

The pinned window logged no failures. The earlier pass logged two, and both were handled
before the numbers on this page were taken:

* `main10m-fluree` failed after 41 minutes, because Fluree's `optional-chain` at 10.5M
  grew until the machine ran out of memory. The same query had reached 26 GB on the
  earlier 30 GB machine. The window resumed with the 12 GiB memory limit, and
  `optional-chain` is left out of Fluree's 10.5M runs.
* `cold1m-jena` failed on Fuseki's `knows-reach` error. The harness wrote curl's elapsed
  time and the error marker into the same field, which stopped the run. The harness now
  records a failed cold query as an error, and Jena's 1.05M cold run was repeated.

Fuseki's `knows-reach` at 10.5M overflows its stack and leaves TDB2's node cache wedged,
so every later query hangs. The first Jena 10.5M run hung this way and was restarted
without that query, which is left out of Jena's 10.5M runs.

## Results: 1.05M triples

At this size most queries take 3.5–10 ms, and the request itself takes about 3.5 ms of
that.

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| **load** (s) | **0.69** | 4.69 | 1.30 | 1.21 | 1.24 |
| count-all | **3.6 ± 0.2** | 199.6 ± 21.3 | 10.9 ± 0.6 ‡ | 3.8 ± 0.8 | 255.3 ± 3.2 |
| types-grouped | **3.6 ± 0.1** | 79.8 ± 7.5 | 5.8 ± 0.3 ‡ | 19.0 ± 1.1 | 43.0 ± 2.4 |
| star-join | **7.7 ± 0.4** | 31.3 ± 3.2 | 36.7 ± 4.8 ‡ | 22.0 ± 1.2 | 390.1 ± 6.1 |
| two-hop-count | **5.3 ± 0.2** | 414.6 ± 18.7 | 20.6 ± 1.8 ‡ | 7.2 ± 0.2 | 844.2 ± 11.5 |
| range-topk | 7.4 ± 0.4 ‡ | 96.9 ± 3.2 | **6.2 ± 0.3** | 19.6 ± 0.2 | 57.5 ± 2.1 |
| optional-count | **4.6 ± 0.2** | 80.9 ± 1.3 | 9.4 ± 0.4 ‡ | 13.7 ± 0.2 | 106.4 ± 5.4 |
| contains | **4.4 ± 0.2** | 43.5 ± 2.1 | 74.4 ± 3.0 ‡ | 8.6 ± 0.1 | 146.0 ± 6.6 |
| group-avg | **8.9 ± 0.3** | 212.8 ± 6.5 ‡ | 15.1 ± 1.6 ‡ | 57.7 ± 2.9 ‡ | 309.5 ± 6.8 |
| path-plus | **3.5 ± 0.1** | 6.6 ± 0.6 | 6.0 ± 0.3 ‡ | 7.5 ± 0.3 | 3.7 ± 0.2 |
| distinct-obj | **3.4 ± 0.1** | 94.2 ± 1.7 | 17.7 ± 3.0 ‡ | 3.6 ± 0.1 | 70.0 ± 2.1 |
| export-500k | **165.4 ± 0.7** | 500.4 ± 3.8 | 774.1 ± 6.4 | 242.6 ± 4.8 | 1717.7 ± 19.5 |
| predicate-counts | **3.6 ± 0.1** | 321.2 ± 2.3 | 15.1 ± 1.1 ‡ | 65.0 ± 1.2 | 287.5 ± 3.8 |
| order-by-full | **48.0 ± 2.9** | 558.4 ± 13.5 | 141.5 ± 4.7 | 114.5 ± 0.5 | 3818.1 ± 8.0 |
| optional-chain | **13.9 ± 0.1** | 228.8 ± 1.6 | 67.5 ± 5.9 | 1836.5 ± 91.2 | 362.6 ± 10.6 |
| minus | **3.9 ± 0.2** | 42.0 ± 1.1 | 7.1 ± 0.2 ‡ | 17.4 ± 14.9 | 35.6 ± 3.9 |
| not-exists | **4.6 ± 0.1** | 58.0 ± 1.9 | 7.0 ± 0.2 ‡ | 16.3 ± 0.5 | 115.2 ± 7.8 |
| exists-join | **4.8 ± 0.1** | 153.2 ± 5.7 | 10.7 ± 0.8 ‡ | 29.0 ± 0.4 | 1450.6 ± 6.9 |
| subquery-agg | **3.6 ± 0.1** | 51.1 ± 1.6 | 5.2 ± 0.4 ‡ | 17.1 ± 0.9 | 26.0 ± 4.3 |
| regex-iri | **4.4 ± 0.3** | 58.1 ± 1.8 | 73.2 ± 2.8 ‡ | 88.2 ± 0.7 | 176.5 ± 19.7 |
| knows-reach | **16.3 ± 0.4** | error ¹ | 62.5 ± 6.2 ‡ | 57.6 ± 0.7 | 342.7 ± 7.9 |
| distinct-join | **8.5 ± 0.4** | 417.1 ± 3.0 | 21.9 ± 2.2 | 97.8 ± 17.2 | 678.4 ± 7.3 |
| lang-filter | **4.3 ± 0.2** | 35.9 ± 1.1 | 43.6 ± 1.9 ‡ | 6.4 ± 0.1 | 83.8 ± 7.1 |
| star-lookup | **3.7 ± 0.2** ‡ | 6.0 ± 0.4 ‡ | 10.4 ± 0.3 ‡ | 3.8 ± 0.2 | 4.8 ± 0.3 |
| values-star | **3.6 ± 0.3** | 7.0 ± 0.5 | 10.9 ± 0.4 ‡ | 5.2 ± 3.3 | 4.1 ± 0.3 |
| employee-docs | **3.6 ± 0.1** | 7.0 ± 0.4 | 6.6 ± 0.3 | 3.9 ± 0.1 | 4.3 ± 0.3 |
| expr-bind-group | **4.5 ± 0.4** | 49.7 ± 1.2 ‡ | 7.9 ± 0.8 ‡ | 46.2 ± 0.7 | 44.7 ± 4.7 |
| expr-order-key | **6.0 ± 0.4** | 89.4 ± 0.9 | 14.7 ± 1.5 ‡ | 115.6 ± 0.6 | 2666.2 ± 25.3 |
| expr-agg-arg | **8.4 ± 0.2** | 183.0 ± 39.7 | 13.8 ± 0.4 ‡ | 85.1 ± 8.8 | 302.5 ± 7.9 |
| **update** (1-triple INSERT DATA) | 4.2 ± 0.2 | 30.5 ± 3.7 | 3.8 ± 0.1 | 6.6 ± 2.2 | **3.4 ± 0.1** |
| **throughput** star-join, 16 clients (queries/s) | **1225** | 119 | 463 | 243 | 25 |

## Results: 10.5M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| **load** (s) | **6.50** | 45.49 | 9.33 | 10.52 | 13.18 |
| count-all | **3.8 ± 0.3** | 1800.6 ± 30.9 | 48.2 ± 2.2 ‡ | 4.0 ± 0.4 | 2499.8 ± 25.0 |
| types-grouped | **3.9 ± 0.3** | 5181.1 ± 315.8 | 6.5 ± 0.6 ‡ | 109.6 ± 2.9 | 383.7 ± 3.9 |
| star-join | **31.5 ± 0.8** | 250.8 ± 13.5 | 116.9 ± 6.3 ‡ | 151.3 ± 0.5 | 4720.7 ± 29.5 |
| two-hop-count | **16.4 ± 0.3** | 3948.6 ± 61.8 | 66.4 ± 8.4 ‡ | 34.7 ± 0.2 | 8371.1 ± 32.0 |
| range-topk | **15.8 ± 1.0** | 858.3 ± 5.8 | 21.1 ± 3.3 | 154.7 ± 1.2 | 595.6 ± 2.8 |
| optional-count | **11.3 ± 1.1** | 1331.6 ± 157.5 | 51.0 ± 5.1 ‡ | 112.2 ± 3.1 | 984.7 ± 10.5 |
| contains | **7.0 ± 0.2** | 3853.2 ± 98.9 | 697.6 ± 25.2 ‡ | 17.5 ± 0.4 | 1533.0 ± 7.0 |
| group-avg | **56.4 ± 3.0** | 4373.8 ± 386.6 ‡ | 101.5 ± 8.2 ‡ | 554.8 ± 10.3 ‡ | 4051.0 ± 46.7 |
| path-plus | **3.8 ± 0.3** | 6.6 ± 0.4 | 10.6 ± 1.1 ‡ | 56.7 ± 1.3 | 4.2 ± 0.2 |
| distinct-obj | **3.9 ± 0.7** | 2279.0 ± 729.0 | 202.5 ± 9.6 ‡ | 4.0 ± 0.3 | 747.6 ± 3.6 |
| export-500k | **177.7 ± 3.1** | 706.0 ± 66.2 | 672.1 ± 9.5 | 260.5 ± 0.3 | 2096.8 ± 7.5 |
| predicate-counts | **3.7 ± 0.3** | 3136.5 ± 47.9 | 70.7 ± 2.3 ‡ | 614.2 ± 21.8 | 2807.2 ± 28.6 |
| order-by-full | **442.4 ± 27.7** | 13968.6 ± 775.1 | 1430.3 ± 11.7 | 1854.1 ± 3.4 | 55339.9 ± 151.0 |
| optional-chain | **102.3 ± 1.6** | 3662.6 ± 118.9 | 569.0 ± 4.5 | excluded ² | 4775.5 ± 28.5 |
| minus | **8.5 ± 0.3** | 2313.2 ± 241.8 | 18.5 ± 1.0 ‡ | 54.9 ± 0.9 | 352.0 ± 16.4 |
| not-exists | **14.7 ± 1.3** | 536.9 ± 27.6 | 23.5 ± 2.5 ‡ | 163.6 ± 1.0 | 1025.3 ± 10.3 |
| exists-join | **14.4 ± 0.6** | 1996.5 ± 302.7 | 65.7 ± 6.0 ‡ | 275.1 ± 2.5 | 123115.1 ± 54.5 |
| subquery-agg | **4.8 ± 0.3** | 2235.0 ± 910.9 | 14.5 ± 0.3 ‡ | 142.8 ± 3.4 | 181.9 ± 1.9 |
| regex-iri | **8.0 ± 0.3** | 1666.1 ± 23.0 | 655.3 ± 3.6 ‡ | 843.1 ± 2.9 | 2624.0 ± 20.5 |
| knows-reach | **145.9 ± 1.4** | excluded ¹ | 1155.5 ± 5.7 ‡ | 627.0 ± 1.1 | 4688.5 ± 26.6 |
| distinct-join | **49.8 ± 2.9** | 4489.3 ± 102.4 | 156.8 ± 5.7 | 1248.0 ± 23.2 | 6682.3 ± 26.0 |
| lang-filter | **6.3 ± 0.2** | 2789.3 ± 200.5 | 388.8 ± 3.4 ‡ | 28.3 ± 1.2 | 839.4 ± 5.9 |
| star-lookup | **3.9 ± 0.3** ‡ | 7.2 ± 0.5 | 30.5 ± 1.9 ‡ | 4.1 ± 0.3 | 5.3 ± 0.3 |
| values-star | **3.8 ± 0.3** | 7.0 ± 0.8 | 12.3 ± 1.0 ‡ | 4.4 ± 0.3 | 4.2 ± 0.2 |
| employee-docs | **3.9 ± 0.3** | 7.5 ± 0.5 | 12.9 ± 1.6 | 4.5 ± 0.3 | 4.7 ± 0.3 |
| expr-bind-group | **11.3 ± 2.5** | 420.9 ± 16.0 ‡ | 33.8 ± 1.5 ‡ | 395.7 ± 0.9 | 349.6 ± 2.9 |
| expr-order-key | **27.0 ± 4.5** | 905.3 ± 28.5 | 112.0 ± 5.9 ‡ | 1108.3 ± 3.7 | 38209.7 ± 58.6 |
| expr-agg-arg | **47.0 ± 2.4** | 1782.0 ± 1.7 | 90.6 ± 4.6 ‡ | 775.6 ± 3.2 | 3884.9 ± 12.0 |
| **update** (1-triple INSERT DATA) | 4.9 ± 0.8 | 33.2 ± 5.3 | 3.9 ± 0.0 | 4.9 ± 0.3 | **3.7 ± 0.2** |
| **throughput** star-join, 16 clients (queries/s) | **242** | 12 | 92 | 22 | 1.7 |

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
   out of Fluree's 10.5M runs. On the earlier 30 GB machine, it finished once in 87 s and
   was OOM-killed at 26 GB on the next run.

### Memory

These are the server's resident set (VmRSS, and VmHWM for peaks) and each bulk load's
peak RSS. Lower is better.

| 1.05M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 42 | 269 | 29 | 53 | **22** |
| server RSS after every query ran once (MiB) | 173 | 1083 | 146 | 2179 | **77** |
| server RSS after the run (MiB) | 380 | 1846 | **286** | 3789 | 1246 |
| peak RSS during the throughput run (MiB) | 501 | 1853 | **305** | 3789 | 1246 |
| Sparkles RSS after the run less its 64 MiB block cache (MiB) | 316 | — | — | — | — |
| bulk load peak RSS (MiB) | **329** | 1734 | 642 | 469 | 657 |
| index size on disk | 27 MiB | 132 MiB | **26 MiB** | 37 MiB | 147 MiB |

| 10.5M triples | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 45 | 266 | 43 | 257 | **29** |
| server RSS after every query ran once (MiB) | 721 | 3097 | 421 | 3508 | **408** |
| server RSS after the run (MiB) | 892 | 4798 | **676** | 3804 | 2384 |
| peak RSS during the throughput run (MiB) | 1282 | 4798 | **947** | 4959 | 2384 |
| Sparkles RSS after the run less its 379 MiB block cache (MiB) | 513 | — | — | — | — |
| bulk load peak RSS (MiB) | 2065 | 4449 | **1927** | 3098 | 6221 |
| index size on disk | 286 MiB | 1.3 GiB | **276 MiB** | 398 MiB | 1.4 GiB |

`scripts/rss-probe.sh` measures memory retention on a fresh Sparkles server. It runs
every query once, then 3 rounds of 160 concurrent `star-join` requests, and reads RSS 2 s
after each step. At 10.5M, RSS was 718 MiB after the queries and 892 MiB after the
concurrent rounds, with a peak of 1,131 MiB. The block cache held 378 MiB of that. At
1.05M, the figures were 176 MiB, 361 MiB and a peak of 384 MiB, with 64 MiB in the block
cache.

The peak rise per query shows what materialization costs. At 10.5M, Sparkles' largest
rises were `order-by-full` at 252 MiB, `two-hop-count` at 232 MiB, `star-join` at 171
MiB and `optional-chain` at 145 MiB. QLever's largest were `distinct-join` at 136 MiB
and `knows-reach` at 133 MiB, and it rose 38 MiB on `order-by-full` and 31 MiB on
`star-join`. Part of Sparkles' rise is the block cache filling. With the cache off,
`star-join` rose 47 MiB and `two-hop-count` 140 MiB, while `order-by-full` still rose 255
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
  rises above and as a higher peak under concurrent load, 1,282 MiB against QLever's 947
  MiB at 10.5M.
* **mimalloc** keeps freed memory in per-thread heaps. The server hands free memory back
  to the OS after it has been idle for a second (`--idle-release-ms`, default 1000).

The cache-size runs measured the 10.5M suite with the block cache at 0, 256 MiB and
4 GiB, on the same build and data. The 1 GiB row is the main 10.5M run above.

| Block cache (`--cache-mb`) | RSS at start | RSS after every query once | Peak RSS under throughput | RSS after the run | Held by the cache | Throughput (q/s) | Sum of the 28 query means |
|---|---:|---:|---:|---:|---:|---:|---:|
| 0 | 41 MiB | 137 MiB | 846 MiB | **319 MiB** | 0 | 128 | 3,282 ms |
| 256 MiB | 41 MiB | 536 MiB | 1,105 MiB | 731 MiB | 255 MiB | **242** | 1,211 ms |
| 1 GiB (default) | 45 MiB | 721 MiB | 1,282 MiB | 892 MiB | 379 MiB | **242** | 1,230 ms |
| 4 GiB | 42 MiB | 706 MiB | 1,306 MiB | 875 MiB | 379 MiB | 238 | **1,190 ms** |

* **No cache** saves 573 MiB of RSS against the default and brings it below QLever's
  676 MiB. It costs half the throughput, 128 against 242 q/s, and makes the suite 2.7×
  slower in total. `knows-reach` becomes 14× slower, 2,068 ms against 146 ms, which is
  slower than QLever (1,156 ms) and Fluree (627 ms). `star-lookup` becomes 5.2× slower,
  `employee-docs` 2.6× and `exists-join`, `range-topk` and `optional-count` about 1.6×.
* **256 MiB** is as fast as the default on this data. Only `star-lookup` was slower,
  5.3 against 3.9 ms, and it saves 161 MiB of RSS.
* **4 GiB** changes nothing at this size, because the suite touches only 379 MiB of
  blocks. A larger dataset fills more of the cache, so the default buys more there and
  costs more RSS.

At 10.5M, about 42% of Sparkles' RSS after the run is block cache, and the rest is
working memory and allocator retention. Less its block cache, the default server's RSS
after the run is 513 MiB, which is lower than QLever's 676 MiB, and a server without a
cache ends at 319 MiB. Setting `--cache-mb 256` keeps the speed on this data for 161 MiB
less. Setting `--cache-mb 0` gives the smallest footprint at about half the throughput.

### Allocator choice (2026-09-30, earlier machine)

These measurements ran on the laptop described under [Other
measurements](#other-measurements), before the client was pinned. Before that night's
allocator changes, the Sparkles server's RSS after the 10.5M run was 1,608 MiB. Most of
it was heap that glibc retained, not live data. The server now links mimalloc and
returns free heap memory to the OS after an idle second, and scans reserve their output
from the exact index count instead of growing it. The probe gave these figures on a fresh
10.5M server, each including the block cache:

| Build (Sparkles only, laptop, 2026-09-30) | after the queries | after the concurrent rounds | peak |
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

Sparkles is the fastest of the five engines on 27 of 28 queries at 1.05M and on all 28
at 10.5M. Against each engine:

* **vs. QLever:** Sparkles wins 27 of 28 at 1.05M (median 2.7×) and 28 of 28 at 10.5M
  (median 3.7×). QLever wins `range-topk` at 1.05M by 1.19× (6.2 against 7.4 ms).
* **vs. Fluree:** Sparkles wins all 28 at 1.05M (median 3.3×) and all 27 that Fluree runs
  at 10.5M (median 9.8×). Four queries at 1.05M and three at 10.5M are within 10%, which
  this page counts as ties.
* **vs. Jena/Fuseki:** Sparkles wins every query. It is 1.6–89× faster at 1.05M and
  1.7–1,312× faster at 10.5M. Fuseki fails `knows-reach`.
* **vs. Oxigraph:** Sparkles wins all 28 at both sizes, by a median of 24× at 1.05M and
  85× at 10.5M. On the point lookups `path-plus`, `values-star`, `employee-docs` and
  `star-lookup`, the margin is only 1.05–1.36×.

### Specific benchmarks

| Case | Loses to | By | Likely cause |
|---|---|---|---|
| `range-topk` at 1.05M | QLever | 1.19× (7.4 against 6.2 ms) | The 10% of salaries written in non-canonical form, such as `"175000.50"`, are stored as vocabulary literals, which are read and tested. Sparkles is 1.33× faster at 10.5M (15.8 against 21.1 ms). |
| `count-all`, `distinct-obj` and `star-lookup` at both sizes, `employee-docs` at 1.05M | Fluree | ties within 10%, 1.01–1.08× | Both engines answer from index statistics or a few index lookups. The times are 3.4–4.1 ms, which is mostly the request itself. |
| `path-plus` at both sizes, `values-star` at 10.5M | Oxigraph | ties within 10%, 1.05–1.09× | The same. Oxigraph follows a single path or a few lookups quickly. |
| Update latency, 1 triple | Oxigraph, QLever | 1.24× and 1.1× at 1.05M (4.2 against 3.4 and 3.8 ms), 1.32× and 1.26× at 10.5M (4.9 against 3.7 and 3.9 ms) | Sparkles fsyncs its WAL and publishes a new snapshot before it acknowledges. Oxigraph does not fsync, and QLever keeps updates in memory only. |
| Commit rate | Oxigraph, QLever, Fluree | 5,000 single-triple commits at 10.5M ran at 256 commits/s, against 1,046 for Oxigraph, 477 for QLever and 326 for Fluree. At 1.05M, Oxigraph ran 768 to Sparkles' 581. | Each commit waits for an fsync. Sparkles' median commit took 1.7 ms at 1.05M and 4.1 ms at 10.5M, and the cause of that growth has not been investigated. |
| Queries after 5,000 commits | Fluree, QLever | `distinct-obj` 1.16× at 10.5M (4.6 against 4.0 ms). At 1.05M, `range-topk` 1.23× to QLever and `two-hop-count` 1.11× to Fluree. | Reads merge the in-memory delta until a compaction folds it into the index. See [Updates and mixed load](#updates-and-mixed-load). |
| Queries after a restart with cold caches | Oxigraph, Fluree, QLever | Up to 4.1× on point lookups. See [Cold starts](#cold-starts). | Sparkles opens and decodes index blocks on first use. |
| Server RSS after the run | QLever | 1.33× at 1.05M (380 against 286 MiB), 1.32× at 10.5M (892 against 676 MiB) | The decoded-block cache and materialized intermediates. Without the cache, the figures are 316 and 513 MiB. See [Memory and the speed it buys](#memory-and-the-speed-it-buys). |
| Bulk load peak RSS at 10.5M | QLever | 1.07× (2,065 against 1,927 MiB) | Parallel parsing and in-memory sorting during the load. Sparkles loads 1.4× faster. |
| Full-text `common-count` at 10.5M | QLever | 1.11× (14.1 against 12.7 ms) | Counting all 49,837 hits of a common word. Sparkles wins the other six text queries. |
| Full-text index size | QLever | 1.23× at 10.5M (123.3 against 99.9 MB) | QLever's index covers every literal and is still smaller. Sparkles builds its index 3.3× faster. |
| Large results | QLever / Jena (in principle) | — | Sparkles streams responses over 1 MiB as they are serialized, but it materializes the result before serializing. `export-500k` is still fastest in Sparkles at both sizes. |

Where Sparkles beats QLever at 10.5M, the margin is often wide. It is 100× on `contains`,
82× on `regex-iri`, 61× on `lang-filter`, 52× on `distinct-obj`, 19× on
`predicate-counts`, 13× on `count-all`, 7.9× on `knows-reach` and `star-lookup`, 3.8× on
`export-500k`, 3.7× on `star-join` and 3.2× on `order-by-full`. The closest are
`range-topk` at 1.33×, `not-exists` at 1.6× and `types-grouped` at 1.65×.

Fluree is fast on single-pattern scans, counts and point lookups. It is much slower on
general joins, OPTIONAL, subqueries, grouping, sorting and expressions:

| Query | Fluree slower by (1.05M / 10.5M) |
|---|---|
| `optional-chain` | 133× (1.84 s) / runs out of memory |
| `predicate-counts` | 18× / 166× |
| `regex-iri` | 20× / 105× |
| `expr-order-key` | 19× / 41× |
| `expr-bind-group` | 10× / 35× |
| `subquery-agg` | 4.7× / 30× |
| `types-grouped` | 5.2× / 28× |
| `distinct-join` | 11× / 25× |
| `exists-join` | 6.0× / 19× |
| `path-plus` | 2.1× / 15× |
| `group-avg` | 6.5× / 9.8× |

Fluree's throughput is lower as well, 243 against 1,225 q/s at 1.05M and 22 against 242
at 10.5M. At 1.05M it uses about 10× the memory (3.7 GiB against 380 MiB).

Jena/Fuseki wins no query. Its one advantage is that it streams results.

Oxigraph follows a single path or a few lookups as fast as Sparkles, within 1.05–1.36×.
Every query that joins, groups, sorts or counts over many rows is one to three orders of
magnitude slower than in Sparkles, and the gap grows with the data:

| Query | Oxigraph slower by (1.05M / 10.5M) |
|---|---|
| `exists-join` | 299× / 8,553× (123 s) |
| `expr-order-key` | 448× / 1,413× |
| `predicate-counts` | 79× / 759× |
| `count-all` | 70× / 654× |
| `two-hop-count` | 160× / 510× |
| `star-join` | 51× / 150× |
| `distinct-join` | 79× / 134× |
| `order-by-full` | 80× / 125× |
| `export-500k` | 10× / 12× |

Oxigraph's throughput is 25 q/s at 1.05M and 1.7 q/s at 10.5M, against 1,225 and 242 for
Sparkles. Its server used 1.2 GiB and 2.3 GiB after the runs. It has the lowest update
latency of the five, 3.4 and 3.7 ms, because it does not fsync each commit. Its load,
including `optimize`, takes 1.24 s at 1.05M and 13.2 s at 10.5M.

## Updates and mixed load

The `updates` mode copies each engine's store, commits the same 5,000 single-triple
`INSERT DATA` and `DELETE DATA` requests to it, one request each, and then runs the full
query suite on the changed store. In Sparkles the commits stay in the in-memory delta,
since 5,000 quads is below the threshold for an automatic compaction.

| Churn of 5,000 commits | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: commits/s | 581 | 21 | 471 | 512 | **768** |
| 1.05M: latency p50 / p99 (ms) | 1.66 / 2.52 | 59.24 / 98.42 | 2.11 / 3.67 | 1.65 / 8.48 | **1.34 / 2.14** |
| 10.5M: commits/s | 256 | 18 | 477 | 326 | **1046** |
| 10.5M: latency p50 / p99 (ms) | 4.14 / 5.98 | 61.47 / 102.52 | 2.01 / 4.86 | 1.69 / 15.78 | **0.94 / 1.70** |

No engine failed a commit. After the churn, Sparkles was still the fastest on 25 of 28
queries at 1.05M and 26 of 28 at 10.5M. It lost `range-topk` to QLever (1.23×) and
`two-hop-count` to Fluree (1.11×) at 1.05M, and `distinct-obj` to Fluree at 10.5M
(1.16×). It tied Fluree on `star-lookup` at 1.05M and `count-all` at 10.5M. QLever's
answer to `types-grouped` differed from the majority after the churn, so it was not
ranked there.

The delta slows Sparkles' joins until the next compaction. At 10.5M, `star-join` went from
31.5 to 61.0 ms, `two-hop-count` from 16.4 to 35.4 ms, `contains` from 7.0 to 15.0 ms and
`regex-iri` from 8.0 to 17.9 ms. Throughput fell from 242 to 170 q/s, still ahead of
QLever's 89. RSS after the run rose from 892 to 1,406 MiB, of which 719 MiB was block
cache. Fluree's rose to 9,998 MiB.

The `mixed` mode runs 16 concurrent `star-join` readers with `oha` for 30 s while one
writer commits single-triple `INSERT DATA` requests as fast as it can. Every request opens
a new connection. The other engines' figures come from the earlier, unpinned pass.

| Mixed load | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| 1.05M: reads/s | **2052.8** | 107.2 | 507.7 | 34.5 | 13.6 |
| 1.05M: read p50 / p99 (ms) | **7.6 / 12.0** | 147.8 / 206.4 | 26.7 / 38.4 | 465.0 / 557.3 | 1156.4 / 1836.4 |
| 1.05M: writes/s | 342.2 | 57.5 | 112.3 | 399.3 | **1265.3** |
| 1.05M: write p50 / p99 (ms) | 2.6 / 6.5 | 16.8 / 25.6 | 6.1 / 24.8 | 2.0 / 9.7 | **0.8 / 1.0** |
| 1.05M: failed reads / writes | 0 / 0 | 0 / 0 | 32959 / 1147 | 0 / 0 | 0 / 0 |
| 10.5M: reads/s | **203.0** | 10.1 | 69.3 | 3.7 | 0 |
| 10.5M: read p50 / p99 (ms) | **77.4 / 101.9** | 1520.9 / 2148.9 | 230.8 / 272.9 | 4320.4 / 5328.9 | — |
| 10.5M: writes/s | 259.6 | 56.4 | 49.7 | 232.0 | **601.1** |
| 10.5M: write p50 / p99 (ms) | 3.5 / 9.3 | 17.0 / 25.0 | 11.9 / 122.6 | 3.3 / 22.9 | **1.5 / 3.5** |

Sparkles serves 4× QLever's reads under a concurrent writer at 1.05M and 2.9× at 10.5M.
Oxigraph commits more writes per second at both sizes, and Fluree does at 1.05M, but
their reads are far slower. At 10.5M no Oxigraph reader finished within the 30 s. QLever
failed 32,959 reads and 1,147 writes at 1.05M, and its log does not say why.

## Cold starts

The `cold` mode stops the server, evicts the engine's files from the page cache, starts
the server and times one query, three times per query. The tables give the median. The
other engines' figures come from the earlier pass.

| Cold, median of 3 | 1.05M | 10.5M |
|---|---|---|
| Sparkles fastest | 19 of 28 | 23 of 28 |
| Time to ready after the restart | 0.09 s (QLever 0.08, Fluree 0.08, Oxigraph 0.08, Fuseki 1.23) | 0.09 s (QLever 0.08, Fluree 0.08, Oxigraph 0.09, Fuseki 1.23) |

| Query, cold | Loses to | 1.05M | 10.5M |
|---|---|---|---|
| `star-lookup` | Oxigraph | 1.58× (23.7 against 15.0 ms) | 4.1× (67.1 against 16.2 ms) |
| `values-star` | Oxigraph | 2.3× (12.4 against 5.4 ms) | 3.4× (17.6 against 5.2 ms) |
| `employee-docs` | Fluree at 1.05M, Oxigraph at 10.5M | 1.43× (16.5 against 11.6 ms) | 2.5× (34.8 against 13.7 ms) |
| `path-plus` | Oxigraph | 2.4× (5.0 against 2.1 ms) | 3.3× (4.8 against 1.4 ms) |
| `range-topk` | QLever | 1.43× (24.6 against 17.2 ms) | 1.25× (54.7 against 43.9 ms) |
| `count-all` | Fluree | 3.0× (3.5 against 1.2 ms) | — |
| `distinct-obj` | Fluree | 4.0× (5.8 against 1.4 ms) | — |
| `lang-filter` | Fluree | 1.77× (12.1 against 6.9 ms) | — |
| `two-hop-count` | Fluree | 1.06× (22.5 against 21.2 ms) | — |

These are single requests timed by curl without process start, so they are lower than
the warm hyperfine times of small queries. The losses are point lookups and statistics,
where a cold Sparkles server reads and decodes whole index blocks for a few rows. On
queries that read many rows, Sparkles stays ahead when cold. At 10.5M, cold `star-join`
took 97 ms against QLever's 366 ms, and cold `order-by-full` 481 ms against QLever's
1,436 ms.

## WatDiv

WatDiv v0.6 basic testing at scale 100 has 10,973,381 triples, 20 query templates and 5
instances of each template, except C1–C3, which have one. Every engine returned the same
answer to all 88 instances. Times are in ms, the geometric mean over each template's
instances of the mean of 5 runs, after 1 warm-up.

| load (s) | sparkles | jena-fuseki | qlever | fluree | oxigraph |
|---|---:|---:|---:|---:|---:|
| load | **5.89** | 33.43 | 9.67 | 8.94 | 14.35 |

| template | instances | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|---:|
| L1 | 5 | **3.84** | 7.15 | 9.62 | 3.98 | 4.63 |
| L2 | 5 | **3.96** | 12.7 | 9.26 | 5.54 | 15.4 |
| L3 | 5 | **3.90** | 7.13 | 6.94 | 3.99 | 4.45 |
| L4 | 5 | **3.95** | 8.66 | 7.22 | 4.31 | 8.04 |
| L5 | 5 | **4.09** | 11.4 | 11.2 | 4.77 | 14.5 |
| S1 | 5 | **4.39** | 7.59 | 95.2 | 4.60 | 7.24 |
| S2 | 5 | **4.23** | 7.55 | 7.93 | 4.73 | 7.11 |
| S3 | 5 | **4.00** | 10.7 | 11.1 | 5.37 | 16.6 |
| S4 | 5 | **3.97** | 13.1 | 7.74 | 5.29 | 27.4 |
| S5 | 5 | **3.94** | 7.78 | 10.3 | 4.68 | 13.8 |
| S6 | 5 | **3.85** | 7.57 | 7.51 | 4.37 | 6.35 |
| S7 | 5 | **3.78** | 6.78 | 9.65 | 3.99 | 4.06 |
| F1 | 5 | **3.98** | 8.83 | 15.4 | 4.44 | 14.2 |
| F2 | 5 | **4.01** | 7.84 | 32.7 | 4.54 | 8.58 |
| F3 | 5 | **3.99** | 7.45 | 17.6 | 29.5 | 6.29 |
| F4 | 5 | **4.52** | 7.84 | 55.5 | 27.5 | 9.04 |
| F5 | 5 | **4.14** | 7.26 | 18.7 | 4.75 | 7.06 |
| C1 | 1 | **7.00** | 17.5 | 33.0 | 8.26 | 30.9 |
| C2 | 1 | **8.05** | 138 | 52.1 | 40.4 | 370 |
| C3 | 1 | **71.4** | 9984 | 296 | 100 | 23070 |

| category | templates | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|---:|
| **linear (L)** | 5 of 5 | **3.95** | 9.16 | 8.70 | 4.48 | 8.19 |
| **star (S)** | 7 of 7 | **4.02** | 8.50 | 12.5 | 4.70 | 9.75 |
| **snowflake (F)** | 5 of 5 | **4.12** | 7.82 | 24.7 | 9.51 | 8.67 |
| **complex (C)** | 3 of 3 | **15.9** | 289 | 79.8 | 32.2 | 641 |
| **all templates** | 20 of 20 | **4.95** | 14.4 | 17.9 | 7.39 | 17.0 |

Most WatDiv instances are selective, and Sparkles and Fluree answer most of them within
1 ms of the 3.5 ms cost of the request. Sparkles is the fastest on every one of the 88
instances, but Fluree comes within 10% on 30 of them and Oxigraph on one, mostly in the
L, S1 and S7 templates. Those are ties. The clear wins are the complex templates. C3 is a
star of six patterns with 419,866 result rows, and Sparkles answers it in 71 ms against
Fluree's 100 ms and QLever's 296 ms. `results/instances.md` in the WatDiv workdir lists every instance. WatDiv was
created by G. Aluç, O. Hartig, M. T. Özsu and K. Daudjee, "Diversified Stress Testing of
RDF Data Management Systems", ISWC 2014.

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
| 1.05M: build (s) | **0.87** | 4.54 | 1.87 |
| 1.05M: size on disk (MB) | 11.9 | 24.9 | **11.1** |
| 10.5M: build (s) | **5.80** | 25.99 | 19.31 |
| 10.5M: size on disk (MB) | 123.3 | 227.8 | **99.9** |

Query times are mean ± σ in ms, with TSV results, 10 runs at 1.05M and 5 at 10.5M:

| query | hits at 10.5M | sparkles 1.05M | jena-fuseki 1.05M | qlever 1.05M | sparkles 10.5M | jena-fuseki 10.5M | qlever 10.5M |
|---|---:|---:|---:|---:|---:|---:|---:|
| rare-top10 | 2 | **4.0 ± 0.3** | 13.1 ± 1.5 | 6.8 ± 0.6 | **5.9 ± 0.3** | 15.3 ± 2.2 | 7.5 ± 1.0 |
| common-top10 | 10 | **4.2 ± 0.1** | 12.4 ± 2.1 | 7.6 ± 0.7 | **7.7 ± 0.2** | 15.1 ± 2.2 | 15.3 ± 1.1 |
| common-count | 49,837 | **4.6 ± 0.2** | 54.1 ± 4.0 | 7.0 ± 0.4 | 14.1 ± 0.4 | 406.8 ± 4.4 | **12.7 ± 0.6** |
| text-join | 3,469 | **5.4 ± 0.2** | 69.2 ± 4.9 | 11.7 ± 1.5 | **25.3 ± 4.6** | 643.8 ± 124.5 | 50.6 ± 4.5 |
| conjunction | 2,893 | **4.1 ± 0.1** | 15.1 ± 1.2 | 7.2 ± 0.2 | **8.2 ± 0.3** | 81.4 ± 1.1 | 15.0 ± 1.3 |
| highlight | 49,837 | **12.2 ± 0.3** | 101.4 ± 9.2 | 15.4 ± 2.1 | **75.8 ± 0.3** | 838.0 ± 21.7 | 95.7 ± 5.5 |
| stemmed | 502 | **3.7 ± 0.2** | 12.7 ± 1.1 | 6.0 ± 0.4 | **5.5 ± 0.4** | 54.2 ± 0.9 | 18.2 ± 3.3 |

QLever has no highlighting, so its `highlight` form returns the plain literals.

## Path search

These runs time `SERVICE path:search` ([F07](specs/F07-path-search.md)) against the
property path queries that test the same reachability, on the `foaf:knows` graph of the
benchmark data. The `+` path from person 0 reaches 89,090 nodes at 1.05M and 892,556 at 10.5M. The pair
queries go from person 0 to the last person, 99,999 or 999,999, whose shortest chains
have 12 and 17 edges. The queries are Sparkles syntax, so they are not part of
`scripts/bench.sh`, which runs the same queries on every engine. QLever has a path
search service of its own with other parameters, and it was not measured.

The runs were on 2026-10-02 with Sparkles at the F07 commit, against a warm server with
the result cache off. Each query ran 31 times, interleaved with the others, with the
client pinned to one core. Other agents were compiling on the machine, and the load
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

## Optimization switches (A/B)

Each executor optimization can be switched off with `SPARKLES_DISABLE_OPTIMIZATIONS`. The
A/B ran the 10.5M suite once with each of 19 switches off, against the default run. A
query counts as changed when its mean moved by more than 25%.

| Switch off | Effect at 10.5M |
|---|---|
| `decorrelate_exists` | `exists-join` 202× slower (2,912 against 14.4 ms), `not-exists` 66× slower (970 against 14.7 ms) |
| `topk_first_key` | `expr-order-key` 6.7× slower (180 against 27 ms) |
| `batched_join` | `values-star` 4.9×, `star-lookup` 3.0× and `employee-docs` 1.7× slower. Throughput 117 against 242 q/s. |
| `expr_cache` | `expr-bind-group` 3.9×, `expr-order-key` 2.9× and `expr-agg-arg` 2.0× slower |
| `anti_join` | `minus` 2.7× slower (22.5 against 8.5 ms) |
| `metadata_counts` | `predicate-counts` 2.1× and `distinct-obj` 1.7× slower |
| `count_filter_runs` | `contains`, `lang-filter` and `regex-iri` 1.7× slower |
| `gallop_index_join` | Throughput 134 against 242 q/s. No single query changed by more than 25%. |
| `characteristic_sets`, `flat_hash_join`, `probed_keys`, `pruned_join_order`, `star_fusion` | No query slower by more than 25% |
| `delta_statistics`, `filter_key_ranges`, `filter_scan_runs`, `fused_star_costs`, `merge_left_join`, `sampled_filters` | No change over 25% |

Several runs show `expr-order-key` or `expr-bind-group` about 20% faster with a switch
off. The default run measured those two at 27.0 ± 4.5 and 11.3 ± 2.5 ms, while the A/B
runs measured them at a median of 23.5 and 9.9 ms, so that is noise in the default run,
not a gain. This A/B shows no benefit from the eleven switches in the last two rows on
this suite. `delta_statistics` corrects index counts for a delta, which a compacted store
does not have. Five of the others are planner estimates (`characteristic_sets`,
`probed_keys`, `pruned_join_order`, `sampled_filters` and `fused_star_costs`), and they
change a query's time only when they change its plan.

## What the benchmark does not cover

* **Large scale.** Nothing above 11M triples has been measured. QLever is built for
  10⁹–10¹¹ triples and routinely runs at that size (Wikidata, UniProt). Its design
  advantages grow with size: lazy evaluation, FSST vocabulary compression, block
  prefiltering and IRI encoding. Sparkles materializes intermediate results, so it would
  hit memory limits earlier on very large intermediate results. A harness for DBpedia up
  to 1.24 billion triples exists (`mise run bench:billion`).
* **Data larger than RAM.** The cold runs restart the server with an empty page cache,
  but every store here fits in memory.
* **Other standard benchmarks.** WatDiv's basic testing is covered. LUBM, BSBM, SP²Bench,
  WatDiv's stress testing and the Wikidata query log are not. The synthetic suite has a
  fairly regular shape, and its 28 queries are hand-picked.
* **Large writes and long runs.** The update runs use single-triple commits for at most
  30 s or 5,000 commits. Large batches, hours of sustained writes and compaction under
  load were not compared with other engines.
* **Spatial queries and inference-time reasoning.** GeoSPARQL workloads were not compared
  with other engines. The sections below give the spatial index's commit cost and the
  GeoSPARQL Compliance Benchmark results. Sparkles has no inference-time reasoning, so
  backward-chaining workloads were not benchmarked.
* **Result-cache benefit.** All runs had result caches off. With the cache on, repeated
  queries are mostly served from memory, which is not a fair comparison.
* **Other hardware.** Every comparison ran on one 20-thread machine with 15 GiB of RAM.

## Other measurements

These measurements time Sparkles on its own. Unless a section names another machine, they
ran on 2026-09-30 to 2026-10-02 on a laptop with an Intel Core Ultra X7 358H (16
threads), 30 GB of RAM and NVMe, before the client was pinned. Their small-request times
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
| gzip-6 (the default when measured; now zstd-3) | 40.96 | 74.9 MB |

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

### Point-in-time reads (10.5M triples)

The 10.5M database was compacted and then took 200,000 single-quad commits in one
generation, at 1,100 commits/s over 4 HTTP connections. History retention covered all
of them. Each figure is the median `time_total` of single `curl` requests.

| Query | Plain | First `?at=` (materializes) | Second `?at=` | Later `?at=` |
|---|---:|---:|---:|---:|
| `COUNT(*)` at commit base + 100,000 | 12.2 ms | 605 ms | 2.7 ms | 3.6 ms |
| one subject's triples at base + 100,001 | 1.0 ms | 654 ms | 0.7 ms | 1.0 ms |
| `COUNT(*)` at the base of the sealed generation, after compaction | 1.0 ms | 43 ms | 0.7 ms | 1.1 ms |

The first read at a commit replays 100,000 WAL commits into a snapshot. Other cold
commits take 0.55–0.65 s as well. Later reads at the same commit reuse the cached
snapshot. After a server restart, the first read at the base of the sealed generation
takes 89–93 ms, because it opens that generation's files. The first read at
base + 100,000 takes 605 ms again.

These figures predate the sparse WAL index. A read now starts from the nearest known
state, so a read near the head or near a cached commit replays only the commits in
between. At a smaller scale (1M triples and 50,000 commits), the first read in the middle
went from 213 ms to 67 ms, and a read 100 commits before the head from 486 ms to 2.7 ms
([F06 Outcome](specs/F06-snapshots-and-point-in-time.md#replay-speedups-rdf-patch-and-the-change-feed)).
The 10.5M run has not been repeated.

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

The full-text index is committed lazily. A write stages its documents, and they are
committed when the next text query needs them, on a tick about once a second, or once
about 16,000 changes are staged. This build and the build before the change were
measured alternately in one session:

| | Text search off | Text search on, commit per write | Text search on, lazy commit |
|---|---:|---:|---:|
| Build the index (every string literal, `sparkles text-index`) | — | 9.1 s, 1.3 GiB peak RSS, 102 MB on disk | 8.0 s, 1.3 GiB peak RSS, 102 MB on disk |
| 1,000-triple `INSERT DATA` | 24.4–26.0 ms (median) | 46.2 ± 7.7 ms | 28.4 ± 1.7 ms |
| 1-triple `INSERT DATA` (harness) | 5.7–9.4 ms | 13.0 ± 5.2 ms | 6.7 ± 3.0 ms |
| `text:query ("Perlman" 10)`, top 10 with scores | — | 14.2 ± 1.4 ms | 15.0 ± 0.7 ms |
| `COUNT` of `text:query "Zurich"` (1,954 hits) | — | 29.9 ± 1.6 ms | 28.6 ± 0.8 ms |

The text-off column comes from four alternating runs of the two builds on fresh servers,
30 runs each. Both builds' medians fall in 24.4–26.0 ms. With text search on, a
1,000-triple batch now costs about 3 ms more than with it off, down from about 21 ms.
Single-triple inserts vary by a few milliseconds between runs in both builds.

The index now syncs to disk at most once a second and replays the WAL after a crash.
Before that change, when it synced every commit, the 1-triple insert took
29.9 ± 0.8 ms with text search on and 8.9 ± 4.0 ms off.

Access logging and Prometheus metrics are on by default. Compared with `--no-access-log
--no-metrics`, the sum of the 20 harness queries differs by 0.6% (1,248 vs 1,240 ms).
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
`commit_latency` in `crates/sparkles/src/store/geo.rs`. The figures are medians of 200
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
build at `9de1492` and `--result-cache-mb 0`. Other builds and tests were running on the
machine (a load average of about 38 on 16 cores), so the tails are noisy. The scripts are
not checked in.

The statistics-answered queries are medians of 20 runs after 3 warm-up runs. "Before"
has the 50,000-quad delta with automatic compaction turned off for the dataset. "After"
follows the automatic compaction that started once the dataset was turned back on with
`idleSeconds` 5. That compaction took 2.0 s and held the writer lock for 5.7 ms.

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

### GeoSPARQL Compliance Benchmark

The GeoSPARQL Compliance Benchmark (Jovanovik, Homburg and Spasić, 2021) has 206 queries
over the 30 requirements of GeoSPARQL 1.0 and a 338-triple dataset. It was run with
`scripts/geosparql-benchmark.sh` on 2026-10-01, using the benchmark at commit `879e0746`
and a release build of `sparkles-server` at commit `70e4c45`. The build had its default
features and the spatial index enabled.

An answer counts as correct when it matches one of the expected result files. Solutions
are compared as multisets, and numbers within a relative 1e-6. Geometry literals are
compared by their coordinates, rounded to 6 decimals. The comparison ignores ring starts
and directions, line directions, vertices on straight runs and member order. Requirement
R17 has no query.

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
| R19 query functions | 6 | 28 |
| R20 `getSRID` | 1 | 2 |
| R21 `relate` | 1 | 4 |
| R22 Simple Features functions | 8 | 32 |
| R23 Egenhofer functions | 8 | 32 |
| R24 RCC8 functions | 8 | 32 |
| R25 RDFS entailment: basic graph patterns | 0 | 3 |
| R26 RDFS entailment: WKT geometry types | 0 | 2 |
| R27 RDFS entailment: GML geometry types | 0 | 1 |
| R28 query rewrite: Simple Features | 0 | 8 |
| R29 query rewrite: Egenhofer | 0 | 8 |
| R30 query rewrite: RCC8 | 0 | 8 |
| **Total** | **74** | **206** |

The mean of the per-requirement scores is 57.6% over the 29 requirements with queries.
Of the 77 queries that use only WKT literals and no RDFS entailment or query rewrite, 72
are answered as expected.
