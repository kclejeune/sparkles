# Benchmarks

This document records how Sparkles compares with **Apache Jena (TDB2 + Fuseki)**,
**QLever**, **Fluree** and **Oxigraph** as of 2026-09-30, and where it still loses. The Sparkles column
was re-measured after the executor and allocator changes of that night (same machine,
data and harness; the other engines' numbers are from the earlier run the same day).
Oxigraph was added later that day, measured on its own with the same harness, machine
and data.
Reproduce it with
`mise run bench [people] [workdir]` (or `scripts/bench.sh`). Add `--engines fluree` to
re-measure one engine and merge its results into an existing run, or `--answers-only` to
re-check every engine's answers without timing anything.

## Setup

* **Data:** `scripts/gen-data.py N` generates synthetic data with about 10.5 triples per
  person: people, organisations, documents, typed literals (integers, decimals,
  dates), `foaf:knows` and citation graphs, language-tagged titles, and a small OWL
  class hierarchy. Two sizes were measured:
  * **1.05M triples** (N = 100k)
  * **10.5M triples** (N = 1M, 1.12 GB of N-Triples)
* **Engines**, none of them allowed to answer from a result cache:
  * Sparkles: `sparkles serve --result-cache-mb 0`.
  * Jena: loaded with `tdb2.tdbloader` 6.2.0, served by Fuseki 5.1.0 with `-Xmx8G`
    (5.1.0 is the Fuseki that nixpkgs packages). Fuseki has no result cache.
  * QLever 0.5.48: `-m 8G -j 16 --cache-max-size-single-entry 0B`. That setting stops
    it from caching results (`--cache-max-num-entries 0` does not), and its cache is
    also cleared before every timed run.
  * Fluree 4.2.2 (BUSL-1.1): the release binary, downloaded and checksum-verified by the
    script.
    * Load: `fluree create bench --from data.nt --chunk-size-mb 16` (parallel bulk
      import).
    * Server: `fluree server run` with `FLUREE_CACHE_MAX_MB=4096`, and
      `FLUREE_PATH_MAX_VISITED=20000000` because the default 1M-node traversal cap
      rejects `knows-reach` at 10M.
    * No result cache.
  * Oxigraph 0.5.11 (nixpkgs): loaded with `oxigraph load` followed by `oxigraph optimize`
    (the compaction its loader recommends before read-heavy workloads; both are timed
    as the load), served by `oxigraph serve --timeout-s 600`. No result cache.
* **Method** (hyperfine):
  * Each engine was measured on its own, one after another, with only its own server
    running and nothing else heavy on the machine.
  * Queries: SPARQL protocol POST, TSV results, 2 warm-ups + 10 runs at 1M, 1 + 5 at 10M.
  * Times are mean ± σ in ms and include a few ms of `curl` process overhead.
  * Loads: 1 run each.
  * **Answers are checked before timing.** Every engine's answer to every query is
    fetched as SPARQL JSON and fingerprinted (`scripts/bench-answers.py`):
    * the row count;
    * the solution multiset by RDF term identity;
    * the same multiset with numeric literals compared by value, to 12 significant
      digits (engines print the non-terminating averages of `group-avg` to different
      precisions).

    An engine whose answer differs *in value* from the majority would be marked † and
    not ranked. None did in these runs. A ‡ marks equal values returned as different RDF
    terms. `export-500k` (LIMIT without ORDER BY) is checked by row count only.
  * **Update latency:** a single-triple `INSERT DATA` into a named graph. An untimed
    `DELETE DATA` runs before every timed request, so each one performs a real
    insertion, and an ASK before and after checks that the change is visible. Each
    engine uses its default durability:
    * Sparkles fsyncs its WAL before acknowledging;
    * TDB2 commits durably;
    * QLever keeps updates in memory only;
    * Fluree uses its default commit path;
    * Oxigraph commits a RocksDB transaction with RocksDB's default write options (the
      WAL is written but not fsynced).
  * **Throughput:** 160 `star-join` requests from 16 parallel clients.
  * **Memory:** server RSS after the run.
* **Machine:** Intel Core Ultra X7 358H (16 threads), 30 GB RAM, NVMe, Linux 6.18. The
  OS page cache is warm, so these are not cold-start numbers.

## Results: 1.05M triples

The Sparkles column of both tables is from the build of 2026-09-30 (evening, after the
ordered-scan top-k), measured alone; the other engines' columns are from earlier runs on
the same machine and data. At 1.05M most queries take 5–30 ms, of which `curl` and HTTP
are a few ms, and run-to-run noise is of the same size (30 runs per query here).

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| **load** (s) | **0.62** | 4.34 | 1.54 | 1.35 | 0.98 |
| count-all | 6.7 ± 2.6 | 148.3 ± 28.1 | 15.3 ± 3.8 ‡ | **6.5 ± 2.2** | 223.1 ± 2.2 |
| types-grouped | **6.1 ± 2.8** | 63.0 ± 11.0 | 19.1 ± 4.2 ‡ | 30.5 ± 8.8 | 47.6 ± 6.8 |
| star-join | **21.6 ± 6.7** | 42.4 ± 3.9 | 48.2 ± 8.0 ‡ | 38.2 ± 5.5 | 322.1 ± 3.3 |
| two-hop-count | 17.8 ± 6.6 | 276.1 ± 13.8 | 47.4 ± 3.3 ‡ | **17.0 ± 5.7** | 682.3 ± 4.2 |
| range-topk | **9.5 ± 4.5** ‡ | 64.2 ± 1.9 | 10.3 ± 4.6 | 40.1 ± 5.2 | 72.8 ± 5.8 |
| optional-count | **10.1 ± 4.8** | 67.2 ± 1.9 | 17.4 ± 6.9 ‡ | 34.0 ± 7.1 | 97.6 ± 3.9 |
| contains | **9.3 ± 4.5** | 42.9 ± 9.8 | 82.1 ± 11.1 ‡ | 24.4 ± 5.5 | 115.4 ± 2.4 |
| group-avg | **25.8 ± 7.3** | 171.6 ± 9.4 ‡ | 26.8 ± 9.5 ‡ | 70.3 ± 15.6 ‡ | 270.8 ± 4.2 |
| path-plus | **5.8 ± 2.7** | 15.0 ± 6.3 | 19.5 ± 1.0 ‡ | 17.7 ± 5.9 | 8.2 ± 3.1 |
| distinct-obj | 9.4 ± 4.1 | 86.0 ± 16.3 | 50.5 ± 7.8 ‡ | **7.4 ± 4.1** | 81.7 ± 6.0 |
| export-500k | **142.3 ± 2.1** | 394.6 ± 8.2 | 662.6 ± 16.7 | 221.8 ± 13.7 | 1323.7 ± 7.2 |
| predicate-counts | **8.5 ± 4.5** | 212.1 ± 10.8 | 32.3 ± 7.0 ‡ | 69.7 ± 8.2 | 255.3 ± 7.0 |
| order-by-full | **60.4 ± 8.7** | 471.5 ± 8.0 | 144.1 ± 5.2 | 100.5 ± 9.1 | 2770.0 ± 13.2 |
| optional-chain | **19.7 ± 2.1** | 186.9 ± 8.4 | 93.0 ± 4.1 | 1465.9 ± 111.2 | 310.2 ± 5.4 |
| minus | **9.4 ± 4.3** | 42.1 ± 10.3 | 21.2 ± 2.7 ‡ | 26.2 ± 25.6 | 36.1 ± 4.2 |
| subquery-agg | **10.4 ± 4.8** | 57.9 ± 11.8 | 16.8 ± 1.4 ‡ | 38.9 ± 4.9 | 32.9 ± 5.9 |
| regex-iri | **10.3 ± 3.3** | 64.3 ± 13.0 | 82.7 ± 11.4 ‡ | 81.0 ± 12.8 | 144.3 ± 2.5 |
| knows-reach | **21.5 ± 1.7** | error ¹ | 97.8 ± 13.5 ‡ | 63.0 ± 11.3 | 310.7 ± 3.8 |
| distinct-join | **28.5 ± 4.6** | 311.8 ± 14.9 | 51.7 ± 9.6 | 99.0 ± 11.0 | 556.7 ± 2.2 |
| lang-filter | **8.3 ± 4.0** | 34.4 ± 8.1 | 61.8 ± 15.6 ‡ | 14.2 ± 5.0 | 82.8 ± 5.7 |
| **update** (1-triple INSERT DATA) | 7.3 ± 3.6 | 41.7 ± 2.1 | 11.8 ± 3.5 | **6.5 ± 0.9** | 11.2 ± 3.7 |
| **throughput** star-join, 16 clients (queries/s) | **912** | 53 | 408 | 497 | 25 |
| **server RSS** after the run (MiB) | 440 | 1744 | 225 | 2275 | 890 |

## Results: 10.5M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| **load** (s) | **4.74** | 42.57 | 9.12 | 10.19 | 9.02 |
| count-all | **4.8 ± 0.5** | 1200.5 ± 13.1 | 37.9 ± 2.0 ‡ | 5.2 ± 0.4 | 2232.8 ± 88.4 |
| types-grouped | **6.5 ± 2.4** | 3776.9 ± 857.1 | 8.4 ± 0.5 ‡ | 113.2 ± 5.9 | 364.0 ± 4.3 |
| star-join | **36.5 ± 1.0** | 275.7 ± 47.2 | 113.6 ± 10.7 ‡ | 135.5 ± 13.0 | 4230.1 ± 90.7 |
| two-hop-count | **30.4 ± 0.2** | 2739.2 ± 7.4 | 61.8 ± 8.9 ‡ | 41.0 ± 1.6 | 6986.3 ± 111.4 |
| range-topk | **15.6 ± 2.6** | 592.2 ± 4.4 | 21.9 ± 1.8 | 138.5 ± 12.7 | 574.1 ± 9.8 |
| optional-count | **22.6 ± 0.4** | 997.1 ± 164.3 | 39.5 ± 1.3 ‡ | 113.1 ± 13.1 | 793.0 ± 10.1 |
| contains | 38.3 ± 5.6 | 2398.5 ± 669.6 | 557.0 ± 13.9 ‡ | **22.2 ± 0.7** | 1104.6 ± 8.1 |
| group-avg | **80.4 ± 3.0** | 3663.1 ± 575.8 ‡ | 91.4 ± 5.4 ‡ | 493.9 ± 16.4 ‡ | 3485.9 ± 8.4 |
| path-plus | **5.6 ± 1.2** | 10.1 ± 3.3 | 11.0 ± 1.6 ‡ | 67.2 ± 16.4 | 8.2 ± 4.0 |
| distinct-obj | 12.5 ± 4.7 | 1135.5 ± 122.7 | 222.6 ± 16.9 ‡ | **5.8 ± 0.3** | 682.7 ± 5.1 |
| export-500k | **136.5 ± 1.7** | 415.7 ± 10.6 | 562.6 ± 13.1 | 287.8 ± 12.1 | 1711.7 ± 26.1 |
| predicate-counts | **15.1 ± 5.2** | 2086.8 ± 14.5 | 70.7 ± 18.2 ‡ | 474.2 ± 7.5 | 2384.6 ± 18.0 |
| order-by-full | **386.5 ± 19.9** | 11353.7 ± 417.9 | 1167.4 ± 21.0 | 2106.9 ± 35.7 | 42401.4 ± 188.8 |
| optional-chain | **97.8 ± 1.3** | 2687.8 ± 279.2 | 481.5 ± 12.6 | OOM ² | 4070.2 ± 25.1 |
| minus | 22.3 ± 2.5 | 1301.6 ± 771.6 | **22.0 ± 2.5** ‡ | 57.9 ± 5.8 | 342.4 ± 5.9 |
| subquery-agg | **11.9 ± 5.2** | 686.2 ± 312.8 | 20.3 ± 1.6 ‡ | 122.6 ± 10.7 | 177.7 ± 4.4 |
| regex-iri | **44.1 ± 8.8** | 1906.4 ± 116.4 | 509.7 ± 19.5 ‡ | 673.1 ± 13.2 | 2278.1 ± 10.3 |
| knows-reach | **137.5 ± 2.5** | error ¹ | 1086.1 ± 21.0 ‡ | 564.4 ± 12.7 | 4084.4 ± 23.9 |
| distinct-join | **71.5 ± 3.9** | 3380.5 ± 9.7 | 182.7 ± 11.4 | 1577.2 ± 27.4 | 5402.9 ± 18.8 |
| lang-filter | **22.1 ± 4.7** | 1575.8 ± 888.8 | 310.5 ± 19.1 ‡ | 30.1 ± 3.7 | 607.6 ± 5.1 |
| **update** (1-triple INSERT DATA) | **5.4 ± 0.6** | 38.6 ± 3.8 | 15.8 ± 1.0 | 6.8 ± 0.3 | 10.7 ± 4.3 |
| **throughput** star-join, 16 clients (queries/s) | **193** | 7 | 57 | 51 | 2 |
| **server RSS** after the run (MiB) | 921 | 3968 | 362 | 3204 | 2317 |

‡ Same values, different RDF terms. QLever returns integers (counts, ages) as
`xsd:int` where the data and the other engines use `xsd:integer`. Sparkles writes
`xsd:decimal` values in XSD 1.1 canonical form (`"175000.5"` for the data's
`"175000.50"`). In `group-avg` the averages agree to 12 significant digits; Sparkles and
Oxigraph (which share the `oxsdatatypes` decimal type) print the same digits, and the
others print fewer or more.

1. Fuseki 5.1 fails `knows-reach` (`<person/0> foaf:knows* ?x`) at both sizes with a
   `StackOverflowError` in TDB2's node cache. At 10.5M the failure also left the cache
   wedged, so later queries hung until they timed out. `knows-reach` was therefore
   excluded from Jena's 10.5M run.
2. In an earlier 10.5M run, Fluree finished `optional-chain` once, in 87 s. On the next
   run the kernel OOM-killed it at 26 GB of anonymous RSS (the machine has 30 GB). It
   was not re-run.

Sparkles' RSS after the 10.5M run was 1608 MiB before those changes, mostly heap
retained by glibc rather than live data. The server now links mimalloc, and returns
free heap memory to the OS once it has been idle for a second (`--idle-release-ms`,
default 1000). Scans also reserve their output from the exact index count instead of
growing it. At 1.05M, RSS after the run grew from 229 to 364 MiB: mimalloc keeps more
memory in its per-thread heaps than glibc does at that size.

`scripts/rss-probe.sh` measures retention on a fresh 10.5M server: it runs every query
once, then 3 × 160 concurrent `star-join` requests, and reads RSS 2 s after each step.

| Build (Sparkles only, same machine, 2026-09-30) | after the queries | after the concurrent rounds | peak |
|---|---:|---:|---:|
| glibc malloc (the build measured above) | 701–704 MiB | 1592–1727 MiB | 1845 MiB |
| glibc, exact scan reservation | 675 MiB | 1426 MiB | 1430 MiB |
| glibc, reservation + idle release (`malloc_trim`) | 644 MiB | 838 MiB | 1439 MiB |
| **mimalloc, reservation + idle release (default build)** | 765 MiB | 920 MiB | 1682 MiB |

About 500 MiB of each figure is the decoded-block cache. The allocator comparison also
changed latency. Sparkles-only 10.5M runs of the default build against the glibc build:
* throughput 190 vs 144 q/s;
* `star-join` 38 vs 72 ms, `optional-count` 33 vs 55 ms, `minus` 27 vs 42 ms,
  `two-hop-count` 65 vs 96 ms;
* load 4.95 vs 5.5 s.

jemalloc and glibc with fixed mmap thresholds were also measured and were slower.

Index size at 10.5M:

| Sparkles | QLever | Fluree (including its commit log) | TDB2 | Oxigraph (after `optimize`) |
|---:|---:|---:|---:|---:|
| 286 MB | 277 MB | 413 MB | 1.4 GB | 1.5 GB |

Peak RSS during the Sparkles bulk load: 1.9 GB.

## Where Sparkles loses

Sparkles is the fastest of the five on 17 of 20 queries at both sizes. Head to head:
* **vs. QLever:** it wins all 20 at 1.05M (median 2.9×) and 19 of 20 at 10.5M (median
  3.1×); `minus` at 10.5M is a tie (22.3 vs 22.0 ms).
* **vs. Fluree:** it wins 17 of 20 at 1.05M and 18 of 20 at 10.5M (median 5.0×).
* **vs. Jena/Fuseki:** it wins every query: 2.0–25× faster at 1.05M and 1.8–580× at
  10.5M (Fuseki fails `knows-reach`).
* **vs. Oxigraph:** it wins all 20 at both sizes (median 11.5× at 1.05M, 42× at 10.5M).

### Specific benchmarks

| Case | Loses to | By | Likely cause |
|---|---|---|---|
| `contains` (`COUNT` + `FILTER(CONTAINS(?name, "Ada"))`) | Fluree | 1.7× at 10M (38.3 vs 22.2 ms; Sparkles 2.6× faster at 1M) | The filter tests stored vocabulary keys in parallel; most of the remaining time is materializing the 1M-row name column first. |
| `distinct-obj` (`COUNT(DISTINCT ?o)` over `foaf:knows`) | Fluree | 2.2× at 10M (12.5 vs 5.8 ms), 1.3× at 1M | Sparkles counts runs in the object-sorted index without materializing rows, but walks the 2.5M rows block by block on one thread. |
| `count-all`, `two-hop-count` | Fluree | ties at 1M (6.7 vs 6.5 ms, 17.8 vs 17.0 ms) | Within noise; Sparkles is 1.1× and 1.35× faster than Fluree at 10M. |
| `minus` | QLever | tie at 10M (22.3 vs 22.0 ms) | A few ms of `curl` overhead dominates. |
| Single-triple update latency | Fluree | 1.1× at 1M (7.3 vs 6.5 ms; Sparkles 1.3× faster at 10M, 5.4 vs 6.8 ms) | Sparkles fsyncs its WAL and publishes a new snapshot before acknowledging. Fluree documents that it indexes in the background. |
| Memory: server RSS after the run | QLever | 2.0× at 1M (440 vs 225 MiB), 2.5× at 10M (921 vs 362 MiB) | Sparkles materializes every intermediate result and keeps a 1 GiB decoded-block cache (about 450 MiB filled here); QLever streams lazily and uses a memory-limited allocator. |
| Large results | QLever / Jena (in principle) | — | Sparkles serializes the whole response in memory before sending. The others stream. `export-500k` is still fastest in Sparkles at this size, but memory grows with result size. |

`range-topk` (a range FILTER on a decimal, then ORDER BY … LIMIT 10), which QLever won by
1.8× at 10.5M, is now 1.4× faster in Sparkles (15.6 vs 21.9 ms; it was 40.2 ms). The
ordered-scan top-k reads the inline numbers from their best end and stops once ten rows
are certain; the 10% of salaries written in non-canonical form (`"175000.50"`) are
vocabulary literals that are still read and tested, and are most of the remaining time.

Where Sparkles wins against QLever at 10.5M, it is often by a wide margin:
`distinct-obj` 18×, `contains` 15×, `lang-filter` 14×, `regex-iri` 12×,
`knows-reach` 7.9×, `count-all` 7.9×, `export-500k` 4.1×, `star-join` 3.1× and
`order-by-full` 3.0×.

Fluree is fast on single-pattern scans and counts, but general joins, OPTIONAL,
subqueries, grouping and sorting are much slower:

| Query | Fluree slower by (1.05M / 10.5M) |
|---|---|
| `optional-chain` | 74× (1.47 s); OOM at 10.5M |
| `predicate-counts` | 8.2× / 31× |
| `distinct-join` | 3.5× / 22× |
| `regex-iri` | 7.9× / 15× |
| `subquery-agg` | 3.7× / 10× |
| `path-plus` | 3.1× / 12× |
| `order-by-full` | 1.7× / 5.5× |
| `group-avg` | 2.7× / 6.1× |
| `knows-reach` | 2.9× / 4.1× |

Fluree's throughput is also lower: 497 vs 912 q/s at 1.05M, and 51 vs 193 at 10.5M.
It uses about 5× the memory at 1.05M (2.2 GiB vs 440 MiB).

Jena/Fuseki wins no query at 1.05M. Its one advantage is that it streams results
(see the memory row).

Oxigraph loads quickly (0.98 s at 1.05M, second to Sparkles, and 9.0 s at 10.5M
including `optimize`) and follows a single path fast (`path-plus`). Every query that
joins, groups, sorts or counts over many rows is an order of magnitude slower, and the
gap grows with the data:

| Query | Oxigraph slower by (1.05M / 10.5M) |
|---|---|
| `count-all` | 33× / 465× |
| `two-hop-count` | 38× / 230× |
| `order-by-full` | 46× / 110× |
| `star-join` | 15× / 116× |
| `predicate-counts` | 30× / 158× |
| `distinct-join` | 20× / 76× |
| `export-500k` | 9.3× / 13× |
| `subquery-agg` | 3.2× / 15× |

Its throughput is 25 q/s at 1.05M and 2 q/s at 10.5M (940 and 191 for Sparkles), and
its server used 890 MiB and 2.3 GiB after the runs. Its update latency (11.2 and
10.7 ms) is behind only Sparkles' and Fluree's, without an fsync per commit.

### What the benchmark does not cover (so no claims either way)

* **Large scale.** Nothing above 10.5M triples. QLever is built for, and routinely
  runs, 10⁹–10¹¹ triples (Wikidata, UniProt). Its design advantages grow with size
  (lazy evaluation, FSST vocabulary compression, block prefiltering, IRI encoding),
  and the memory gap above suggests Sparkles would hit RAM limits much earlier on
  very large intermediate results.
* **Cold caches.** All runs had a warm OS page cache.
* **Standard benchmarks.** No LUBM, BSBM, SP²Bench, WatDiv or the Wikidata query log.
  The synthetic data has a fairly regular shape and 11 + 9 hand-picked query shapes.
* **Update-heavy workloads.** Only single-triple latency was measured. TDB2 updates its
  B+trees in place. Sparkles keeps updates in an in-memory delta: large batches trigger
  a full rebuild, and many small commits grow the delta until `compact`. Sustained
  mixed read/write workloads have not been measured.
* **Text search against other engines, spatial queries, inference-time reasoning.**
  Sparkles' full-text index is measured on its own below; it was not compared with
  Jena's `jena-text` or QLever's text joins. Sparkles has no spatial queries or
  inference-time reasoning, so GeoSPARQL and backward-chaining workloads were not
  benchmarked.
* **Result-cache benefit.** All runs had caches off. With the cache on, repeated
  queries are mostly served from memory, which is not a fair comparison.

## Other measurements

| | Sparkles |
|---|---:|
| RDFS materialization, 1.0M → 2.75M inferred triples | 3.9 s end to end |
| SHACL validation, 20 shapes, 1.05M triples, 48,428 results (`mise run bench:shacl 100000`) | 164 ms parallel, 741 ms sequential |
| `ASK { ?s ?p ?o }` / `SELECT * … LIMIT 100` at 10.5M (early termination) | 1.9 ms / 1.8 ms |

### Compression (10.5M triples)

Sparkles alone, release build, 2026-09-30. HTTP times are `hyperfine` means over 5 runs
(`curl` included); server CPU is the server process's user + system time per request.

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

zstd streams the export as fast as identity at a 13–14× smaller size; gzip doubles the
wall time and triples the server CPU. zstd-1 came out slightly smaller
than zstd-3 on both responses.

`/$/backup` (N-Quads dump to a file, median of 3; the CLI `sparkles backup --threads 16`
with zstd-3 takes the same 8.2 s, so the server's 4 threads are not the limit):

| Codec | Seconds | Size |
|---|---:|---:|
| none | 4.33 | 1,121 MB |
| lz4 | 4.73 | 162 MB |
| zstd-3 | 8.16 | 81.5 MB |
| brotli-5 | 12.82 | 119.0 MB |
| gzip-6 (default) | 40.96 | 74.9 MB |

`sparkles load` of the 1.1 GB N-Triples file, raw and compressed (median of 3; CPU is
user + system of the load; parallelism = CPU / wall):

| Input | File size | Seconds | CPU (s) | Parallelism |
|---|---:|---:|---:|---:|
| raw | 1,121 MB | 4.82 | 25.6 | 5.2 |
| lz4 | 160 MB | 5.32 | 25.5 | 4.7 |
| zstd-3 | 78.6 MB | 5.41 | 25.3 | 4.7 |
| gzip-6 | 81.9 MB | 5.66 | 25.9 | 4.5 |
| brotli-5 | 69.0 MB | 5.71 | 26.1 | 4.6 |

Decompression is a single stream in front of the parallel parser and costs 0.5–0.9 s;
the parser keeps about 4.5 cores busy either way.

Full-text document store (every string literal, 1,540,012 documents): LZ4 117.6 MB built
in 8.67 s, zstd 106.1 MB in 8.71 s (zstd is the default).

### Point-in-time reads (10.5M triples)

The 10.5M database compacted, then 200,000 single-quad commits in one generation
(1,100 commits/s over 4 HTTP connections), history retention covering all of them.
Single requests timed with `curl` (`time_total`), medians:

| Query | Plain | First `?at=` (materializes) | Second `?at=` | Later `?at=` |
|---|---:|---:|---:|---:|
| `COUNT(*)` at commit base + 100,000 | 12.2 ms | 605 ms | 2.7 ms | 3.6 ms |
| one subject's triples at base + 100,001 | 1.0 ms | 654 ms | 0.7 ms | 1.0 ms |
| `COUNT(*)` at the base of the sealed generation, after compaction | 1.0 ms | 43 ms | 0.7 ms | 1.1 ms |

The first read of a commit replays 100,000 WAL commits into a snapshot (0.55–0.65 s for
other cold commits as well); later reads of it reuse the cached snapshot. After a server
restart, the first read at the base of the sealed generation takes 89–93 ms (it opens
that generation's files), and at base + 100,000 605 ms again.

### Backup repositories (10.5M triples)

`mise run bench:backup` (`scripts/backup-bench.sh`) on the 10.5M-quad benchmark database
(301 MB), Sparkles alone, release build, an `fs` repository on the same ext4 file system,
warm page cache; 3 rounds, median (min–max), wall clock including process start:

| Case | Seconds | Size |
|---|---:|---|
| Full backup into an empty repository | 0.56 (0.55–0.60) | 301.3 MB logical, 281.5 MB stored, 534 MB/s |
| Incremental backup after 1,000 single-quad commits | 0.34 (0.31–0.35) | 55.3 KB added (3 new blobs, 28 reused) |
| Restore into a new directory (quick check) | 0.79 (0.78–0.93) | 301.5 MB |
| Verify at level `data` (every blob read and hashed) | 0.08 (0.07–0.13) | 32 blobs, ok |

An incremental backup stores only the appended tail of the update log and the files that
changed, so its size follows the writes since the last backup, not the database size.
Not measured yet: S3, cold caches, and databases larger than memory.

### Full-text index and observability (10.5M triples)

Sparkles alone, one configuration at a time, same machine and harness
(`scripts/bench.sh` with `ENGINES=sparkles SKIP_LOAD=1`, plus `hyperfine` and `oha`).
Update times are end to end over HTTP (`curl`).

The full-text index is committed lazily: a write stages its documents, and the next text
query that needs them, a tick about once a second, or a batch of about 16,000 staged
changes commits them. Measured against the build before that change, alternately, in one
session:

| | Text search off | Text search on, commit per write | Text search on, lazy commit |
|---|---:|---:|---:|
| Build the index (every string literal, `sparkles text-index`) | — | 9.1 s, 1.3 GiB peak RSS, 102 MB on disk | 8.0 s, 1.3 GiB peak RSS, 102 MB on disk |
| 1,000-triple `INSERT DATA` | 24.4–26.0 ms (median) | 46.2 ± 7.7 ms | 28.4 ± 1.7 ms |
| 1-triple `INSERT DATA` (harness) | 5.7–9.4 ms | 13.0 ± 5.2 ms | 6.7 ± 3.0 ms |
| `text:query ("Perlman" 10)`, top 10 with scores | — | 14.2 ± 1.4 ms | 15.0 ± 0.7 ms |
| `COUNT` of `text:query "Zurich"` (1,954 hits) | — | 29.9 ± 1.6 ms | 28.6 ± 0.8 ms |

The text-off column is four alternating runs of the two builds on fresh servers (30 runs
each; both builds' medians fall in 24.4–26.0 ms), so a 1,000-triple batch now costs about
3 ms more with text search on instead of about 21 ms. Single-triple inserts vary by a few
milliseconds between runs in both builds.

Earlier, before the index stopped syncing every commit to disk (it syncs at most once a
second and replays the WAL after a crash), the 1-triple insert took 29.9 ± 0.8 ms with
text search on and 8.9 ± 4.0 ms off.

Access logging and Prometheus metrics (the defaults) against `--no-access-log
--no-metrics`: the sum of the 20 harness queries differs by 0.6% (1,248 vs 1,240 ms,
within run-to-run noise; single queries vary by up to ±50% in both directions at
these sizes). Under load (`oha`, 16 connections, two alternating rounds each):

| | Logs and metrics on | Off |
|---|---:|---:|
| `ASK {}`, 50,000 requests | 71,034 / 70,175 req/s, p99 0.63 / 0.67 ms | 69,776 / 68,782 req/s, p99 0.63 / 0.66 ms |
| star join, 1,000 requests | 103 / 103 req/s, p50 155 / 156 ms | 104 / 100 req/s, p50 154 / 159 ms |

No measurable overhead.

### Spatial index: commit cost

Commit latency with the spatial index enabled against the same store without it, on a quiet
machine (2026-10-01, release build at `02e3a78`, `commit_latency` in
`crates/sparkles/src/store/geo.rs`; medians of 200 one-triple commits and 20 commits of
1,000 `geo:asWKT` points each, three runs):

| | Index off | Index on |
|---|---:|---:|
| 1-triple non-geometry commit | 0.012–0.016 ms | 0.015–0.017 ms |
| 1,000-point commit | 3.19–3.50 ms | 3.11–3.16 ms |

The index adds no measurable time to a commit: the differences are within run-to-run noise.

### GeoSPARQL Compliance Benchmark

The GeoSPARQL Compliance Benchmark (Jovanovik, Homburg and Spasić, 2021: 206 queries over
the 30 requirements of GeoSPARQL 1.0, a 338-triple dataset) run with
`scripts/geosparql-benchmark.sh` on 2026-10-01: the benchmark at commit `879e0746`, a
release build of `sparkles-server` with its default features at commit `70e4c45`, with
the spatial index enabled. An answer counts as correct when it matches one of the
expected result files: solutions as multisets, numbers within a relative 1e-6, geometry
literals by their coordinates (rounded to 6 decimals; ring starts and directions, line
directions, vertices on straight runs and member order ignored). Requirement R17 has no
query.

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
