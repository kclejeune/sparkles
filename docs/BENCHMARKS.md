# Benchmarks

This document records how Sparkles compares with **Apache Jena (TDB2 + Fuseki)**,
**QLever** and **Fluree** as of 2026-09-30, and where it still loses. The Sparkles column
was re-measured after the executor and allocator changes of that night (same machine,
data and harness; the other engines' numbers are from the earlier run the same day).
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
    * the same multiset with numeric literals compared by value.

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
    * Fluree uses its default commit path.
  * **Throughput:** 160 `star-join` requests from 16 parallel clients.
  * **Memory:** server RSS after the run.
* **Machine:** Intel Core Ultra X7 358H (16 threads), 30 GB RAM, NVMe, Linux 6.18. The
  OS page cache is warm, so these are not cold-start numbers.

## Results: 1.05M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) |
|---|---:|---:|---:|---:|
| **load** (s) | **0.58** | 4.34 | 1.54 | 1.35 |
| count-all | **5.1 ± 0.3** | 148.3 ± 28.1 | 15.3 ± 3.8 ‡ | 6.5 ± 2.2 |
| types-grouped | **6.4 ± 1.8** | 63.0 ± 11.0 | 19.1 ± 4.2 ‡ | 30.5 ± 8.8 |
| star-join | **26.7 ± 3.8** | 42.4 ± 3.9 | 48.2 ± 8.0 ‡ | 38.2 ± 5.5 |
| two-hop-count | 17.2 ± 5.7 | 276.1 ± 13.8 | 47.4 ± 3.3 ‡ | **17.0 ± 5.7** |
| range-topk | **8.6 ± 2.5** ‡ | 64.2 ± 1.9 | 10.3 ± 4.6 | 40.1 ± 5.2 |
| optional-count | **8.1 ± 2.8** | 67.2 ± 1.9 | 17.4 ± 6.9 ‡ | 34.0 ± 7.1 |
| contains | **8.2 ± 2.3** | 42.9 ± 9.8 | 82.1 ± 11.1 ‡ | 24.4 ± 5.5 |
| group-avg | **17.5 ± 2.4** | 171.6 ± 9.4 | 26.8 ± 9.5 | 70.3 ± 15.6 |
| path-plus | **8.0 ± 3.1** | 15.0 ± 6.3 | 19.5 ± 1.0 ‡ | 17.7 ± 5.9 |
| distinct-obj | 11.3 ± 3.6 | 86.0 ± 16.3 | 50.5 ± 7.8 ‡ | **7.4 ± 4.1** |
| export-500k | **154.6 ± 3.0** | 394.6 ± 8.2 | 662.6 ± 16.7 | 221.8 ± 13.7 |
| predicate-counts | **6.1 ± 0.4** | 212.1 ± 10.8 | 32.3 ± 7.0 ‡ | 69.7 ± 8.2 |
| order-by-full | **65.1 ± 14.3** | 471.5 ± 8.0 | 144.1 ± 5.2 | 100.5 ± 9.1 |
| optional-chain | **24.6 ± 3.6** | 186.9 ± 8.4 | 93.0 ± 4.1 | 1465.9 ± 111.2 |
| minus | **14.7 ± 5.3** | 42.1 ± 10.3 | 21.2 ± 2.7 ‡ | 26.2 ± 25.6 |
| subquery-agg | **6.3 ± 2.6** | 57.9 ± 11.8 | 16.8 ± 1.4 ‡ | 38.9 ± 4.9 |
| regex-iri | **12.8 ± 5.8** | 64.3 ± 13.0 | 82.7 ± 11.4 ‡ | 81.0 ± 12.8 |
| knows-reach | **21.9 ± 5.9** | error ¹ | 97.8 ± 13.5 ‡ | 63.0 ± 11.3 |
| distinct-join | **26.3 ± 4.2** | 311.8 ± 14.9 | 51.7 ± 9.6 | 99.0 ± 11.0 |
| lang-filter | **8.2 ± 2.2** | 34.4 ± 8.1 | 61.8 ± 15.6 ‡ | 14.2 ± 5.0 |
| **update** (1-triple INSERT DATA) | **5.1 ± 0.4** | 41.7 ± 2.1 | 11.8 ± 3.5 | 6.5 ± 0.9 |
| **throughput** star-join, 16 clients (queries/s) | **940** | 53 | 408 | 497 |
| **server RSS** after the run (MiB) | 364 | 1744 | 225 | 2275 |

## Results: 10.5M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) |
|---|---:|---:|---:|---:|
| **load** (s) | **4.75** | 42.57 | 9.12 | 10.19 |
| count-all | **5.1 ± 0.6** | 1200.5 ± 13.1 | 37.9 ± 2.0 ‡ | 5.2 ± 0.4 |
| types-grouped | **5.5 ± 1.6** | 3776.9 ± 857.1 | 8.4 ± 0.5 ‡ | 113.2 ± 5.9 |
| star-join | **40.7 ± 6.3** | 275.7 ± 47.2 | 113.6 ± 10.7 ‡ | 135.5 ± 13.0 |
| two-hop-count | **29.9 ± 2.3** | 2739.2 ± 7.4 | 61.8 ± 8.9 ‡ | 41.0 ± 1.6 |
| range-topk | 40.2 ± 3.1 | 592.2 ± 4.4 | **21.9 ± 1.8** | 138.5 ± 12.7 |
| optional-count | **22.1 ± 0.4** | 997.1 ± 164.3 | 39.5 ± 1.3 ‡ | 113.1 ± 13.1 |
| contains | 35.2 ± 4.7 | 2398.5 ± 669.6 | 557.0 ± 13.9 ‡ | **22.2 ± 0.7** |
| group-avg | **82.5 ± 6.3** | 3663.1 ± 575.8 | 91.4 ± 5.4 | 493.9 ± 16.4 |
| path-plus | 12.2 ± 3.9 | **10.1 ± 3.3** | 11.0 ± 1.6 ‡ | 67.2 ± 16.4 |
| distinct-obj | 9.2 ± 0.9 | 1135.5 ± 122.7 | 222.6 ± 16.9 ‡ | **5.8 ± 0.3** |
| export-500k | **151.7 ± 1.4** | 415.7 ± 10.6 | 562.6 ± 13.1 | 287.8 ± 12.1 |
| predicate-counts | **25.7 ± 7.1** | 2086.8 ± 14.5 | 70.7 ± 18.2 ‡ | 474.2 ± 7.5 |
| order-by-full | **400.3 ± 27.7** | 11353.7 ± 417.9 | 1167.4 ± 21.0 | 2106.9 ± 35.7 |
| optional-chain | **105.4 ± 2.8** | 2687.8 ± 279.2 | 481.5 ± 12.6 | OOM ² |
| minus | 23.0 ± 4.0 | 1301.6 ± 771.6 | **22.0 ± 2.5** ‡ | 57.9 ± 5.8 |
| subquery-agg | **17.6 ± 5.1** | 686.2 ± 312.8 | 20.3 ± 1.6 ‡ | 122.6 ± 10.7 |
| regex-iri | **46.2 ± 8.7** | 1906.4 ± 116.4 | 509.7 ± 19.5 ‡ | 673.1 ± 13.2 |
| knows-reach | **138.0 ± 2.5** | error ¹ | 1086.1 ± 21.0 ‡ | 564.4 ± 12.7 |
| distinct-join | **68.2 ± 2.9** | 3380.5 ± 9.7 | 182.7 ± 11.4 | 1577.2 ± 27.4 |
| lang-filter | **28.3 ± 4.2** | 1575.8 ± 888.8 | 310.5 ± 19.1 ‡ | 30.1 ± 3.7 |
| **update** (1-triple INSERT DATA) | 7.6 ± 4.0 | 38.6 ± 3.8 | 15.8 ± 1.0 | **6.8 ± 0.3** |
| **throughput** star-join, 16 clients (queries/s) | **191** | 7 | 57 | 51 |
| **server RSS** after the run (MiB) | 897 | 3968 | 362 | 3204 |

‡ Same values, different RDF terms. QLever returns integers (counts, ages) as
`xsd:int` where the data and the other engines use `xsd:integer`. Sparkles writes
`xsd:decimal` values in XSD 1.1 canonical form (`"175000.5"` for the data's
`"175000.50"`).

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
The four-engine tables above have not been re-run with this build yet. Sparkles'
server RSS after its 10.5M run is now 947 MiB.

Index size at 10.5M:

| Sparkles | QLever | Fluree (including its commit log) | TDB2 |
|---:|---:|---:|---:|
| 286 MB | 277 MB | 413 MB | 1.4 GB |

Peak RSS during the Sparkles bulk load: 1.9 GB.

## Where Sparkles loses

Sparkles is the fastest of the four on 18 of 20 queries at 1.05M and 15 of 20 at 10.5M.
Head to head:
* **vs. QLever:** it wins all 20 at 1.05M and 17 of 20 at 10.5M.
* **vs. Fluree:** it wins 18 of 20 at 1.05M and 18 of 20 at 10.5M.
* **vs. Jena/Fuseki:** it wins every query except `path-plus` at 10.5M (within noise).
  Fuseki is 1.6–35× slower at 1.05M and 2.7–690× slower on the other queries at 10.5M.

### Specific benchmarks

| Case | Loses to | By | Likely cause |
|---|---|---|---|
| `range-topk` (range FILTER on a decimal + ORDER BY … LIMIT) | QLever | 1.8× at 10M (Sparkles 1.2× faster at 1M) | The range scan reads only the matching id ranges of inline integers and decimals, but the 10% of salaries written in non-canonical form (`"175000.50"`) are vocabulary literals that must be read and tested, and the top-k step still decodes the surviving rows. |
| `contains` (`COUNT` + `FILTER(CONTAINS(?name, "Ada"))`) | Fluree | 1.6× at 10M (Sparkles 3× faster at 1M) | The filter tests stored vocabulary keys in parallel; most of the remaining time is materializing the 1M-row name column first. |
| `distinct-obj` (`COUNT(DISTINCT ?o)` over `foaf:knows`) | Fluree | 1.6× at 10M, 1.5× at 1M | Sparkles counts runs in the object-sorted index without materializing rows, but walks the 2.5M rows block by block on one thread. |
| `two-hop-count` | Fluree | tie at 1M (17.2 vs 17.0 ms) | Counted from per-key runs of both sides; Sparkles is 1.4× faster than Fluree at 10M. |
| `minus`, `path-plus` | QLever, Jena | within noise at 10M | A few ms of `curl` overhead dominates both. |
| Single-triple update latency | Fluree | 1.1× at 10M (6.8 vs 7.6 ms) | Sparkles fsyncs its WAL and publishes a new snapshot before acknowledging. Fluree documents that it indexes in the background. |
| Memory: server RSS after the run | QLever | 1.6× at 1M (364 vs 225 MiB), 2.5× at 10M (897 vs 362 MiB) | Sparkles materializes every intermediate result and keeps a 1 GiB decoded-block cache (about 450 MiB filled here); QLever streams lazily and uses a memory-limited allocator. |
| Large results | QLever / Jena (in principle) | — | Sparkles serializes the whole response in memory before sending. The others stream. `export-500k` is still fastest in Sparkles at this size, but memory grows with result size. |

Where Sparkles wins against QLever at 10.5M, it is often by a wide margin:
`distinct-obj` 24×, `contains` 16×, `regex-iri` 11×, `lang-filter` 11×,
`knows-reach` 7.9×, `count-all` 7.4×, `export-500k` 3.7×, `order-by-full` 2.9× and
`star-join` 2.8×.

Fluree is fast on single-pattern scans and counts, but general joins, OPTIONAL,
subqueries, grouping and sorting are much slower:

| Query | Fluree slower by (1.05M / 10.5M) |
|---|---|
| `optional-chain` | 60× (1.47 s); OOM at 10.5M |
| `predicate-counts` | 11× / 18× |
| `distinct-join` | 3.8× / 23× |
| `regex-iri` | 6.3× / 15× |
| `subquery-agg` | 6.2× / 7.0× |
| `path-plus` | 2.2× / 5.5× |
| `order-by-full` | 1.5× / 5.3× |
| `group-avg` | 4.0× / 6.0× |
| `knows-reach` | 2.9× / 4.1× |

Fluree's throughput is also lower: 497 vs 940 q/s at 1.05M, and 51 vs 191 at 10.5M.
It uses about 6× the memory at 1.05M (2.2 GiB vs 364 MiB).

Jena/Fuseki wins no query at 1.05M. Its one advantage is that it streams results
(see the memory row).

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

### Full-text index and observability (10.5M triples)

Sparkles alone, one configuration at a time, same machine and harness
(`scripts/bench.sh` with `ENGINES=sparkles SKIP_LOAD=1`, plus `hyperfine` and `oha`).
Update times are end to end over HTTP (`curl`).

| | Text search off | Text search on |
|---|---:|---:|
| Build the index (every string literal, `sparkles text-index`) | — | 8.9 s, 1.3 GiB peak RSS, 113 MB on disk |
| 1-triple `INSERT DATA` (harness) | 9.2 ± 4.7 ms | 14.9 ± 7.1 ms |
| 1,000-triple `INSERT DATA` | 21.6 ± 9.6 ms | 49.0 ± 2.3 ms |
| `text:query ("Perlman" 10)`, top 10 with scores | — | 15.0 ± 1.0 ms |
| `COUNT` of `text:query "Zurich"` (1,954 hits) | — | 25.5 ± 5.4 ms |

Before the index stopped syncing every commit to disk (it now syncs at most once a
second, and replays the WAL after a crash), the same 1-triple insert took
29.9 ± 0.8 ms with text search on, and 8.9 ± 4.0 ms off. The 1,000-triple batch cost
about 27 ms more with text search on, both before and after that change: the rest is
indexing and the index commit, not disk syncs.

Access logging and Prometheus metrics (the defaults) against `--no-access-log
--no-metrics`: the sum of the 20 harness queries differs by 0.6% (1,248 vs 1,240 ms,
within run-to-run noise; single queries vary by up to ±50% in both directions at
these sizes). Under load (`oha`, 16 connections, two alternating rounds each):

| | Logs and metrics on | Off |
|---|---:|---:|
| `ASK {}`, 50,000 requests | 71,034 / 70,175 req/s, p99 0.63 / 0.67 ms | 69,776 / 68,782 req/s, p99 0.63 / 0.66 ms |
| star join, 1,000 requests | 103 / 103 req/s, p50 155 / 156 ms | 104 / 100 req/s, p50 154 / 159 ms |

No measurable overhead.
