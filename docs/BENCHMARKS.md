# Benchmarks

This document records how Sparkles compares with **Apache Jena (TDB2 + Fuseki)**,
**QLever** and **Fluree** as of 2026-09-30, and where it still loses. Reproduce it with
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
| **load** (s) | **0.81** | 4.34 | 1.54 | 1.35 |
| count-all | **4.7 ± 0.9** | 148.3 ± 28.1 | 15.3 ± 3.8 ‡ | 6.5 ± 2.2 |
| types-grouped | **5.1 ± 0.2** | 63.0 ± 11.0 | 19.1 ± 4.2 ‡ | 30.5 ± 8.8 |
| star-join | **23.6 ± 7.3** | 42.4 ± 3.9 | 48.2 ± 8.0 ‡ | 38.2 ± 5.5 |
| two-hop-count | 25.1 ± 5.8 | 276.1 ± 13.8 | 47.4 ± 3.3 ‡ | **17.0 ± 5.7** |
| range-topk | 19.4 ± 5.5 ‡ | 64.2 ± 1.9 | **10.3 ± 4.6** | 40.1 ± 5.2 |
| optional-count | **10.2 ± 5.3** | 67.2 ± 1.9 | 17.4 ± 6.9 ‡ | 34.0 ± 7.1 |
| contains | **11.2 ± 4.3** | 42.9 ± 9.8 | 82.1 ± 11.1 ‡ | 24.4 ± 5.5 |
| group-avg | **20.8 ± 1.1** | 171.6 ± 9.4 | 26.8 ± 9.5 | 70.3 ± 15.6 |
| path-plus | **4.5 ± 0.3** | 15.0 ± 6.3 | 19.5 ± 1.0 ‡ | 17.7 ± 5.9 |
| distinct-obj | **7.4 ± 2.7** | 86.0 ± 16.3 | 50.5 ± 7.8 ‡ | 7.4 ± 4.1 |
| export-500k | **171.9 ± 3.4** | 394.6 ± 8.2 | 662.6 ± 16.7 | 221.8 ± 13.7 |
| predicate-counts | **6.3 ± 0.8** | 212.1 ± 10.8 | 32.3 ± 7.0 ‡ | 69.7 ± 8.2 |
| order-by-full | **62.0 ± 8.4** | 471.5 ± 8.0 | 144.1 ± 5.2 | 100.5 ± 9.1 |
| optional-chain | **24.0 ± 3.5** | 186.9 ± 8.4 | 93.0 ± 4.1 | 1465.9 ± 111.2 |
| minus | **9.5 ± 1.1** | 42.1 ± 10.3 | 21.2 ± 2.7 ‡ | 26.2 ± 25.6 |
| subquery-agg | **13.1 ± 2.8** | 57.9 ± 11.8 | 16.8 ± 1.4 ‡ | 38.9 ± 4.9 |
| regex-iri | **15.9 ± 5.0** | 64.3 ± 13.0 | 82.7 ± 11.4 ‡ | 81.0 ± 12.8 |
| knows-reach | **39.7 ± 2.5** | error ¹ | 97.8 ± 13.5 ‡ | 63.0 ± 11.3 |
| distinct-join | **27.3 ± 5.9** | 311.8 ± 14.9 | 51.7 ± 9.6 | 99.0 ± 11.0 |
| lang-filter | **7.6 ± 1.1** | 34.4 ± 8.1 | 61.8 ± 15.6 ‡ | 14.2 ± 5.0 |
| **update** (1-triple INSERT DATA) | **6.4 ± 2.8** | 41.7 ± 2.1 | 11.8 ± 3.5 | 6.5 ± 0.9 |
| **throughput** star-join, 16 clients (queries/s) | **898** | 53 | 408 | 497 |
| **server RSS** after the run (MiB) | 229 | 1744 | 225 | 2275 |

## Results: 10.5M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) |
|---|---:|---:|---:|---:|
| **load** (s) | **6.11** | 42.57 | 9.12 | 10.19 |
| count-all | 5.9 ± 1.8 | 1200.5 ± 13.1 | 37.9 ± 2.0 ‡ | **5.2 ± 0.4** |
| types-grouped | 9.3 ± 3.7 | 3776.9 ± 857.1 | **8.4 ± 0.5** ‡ | 113.2 ± 5.9 |
| star-join | **73.3 ± 0.6** | 275.7 ± 47.2 | 113.6 ± 10.7 ‡ | 135.5 ± 13.0 |
| two-hop-count | 97.1 ± 0.7 | 2739.2 ± 7.4 | 61.8 ± 8.9 ‡ | **41.0 ± 1.6** |
| range-topk | 108.8 ± 11.6 | 592.2 ± 4.4 | **21.9 ± 1.8** | 138.5 ± 12.7 |
| optional-count | 54.9 ± 3.6 | 997.1 ± 164.3 | **39.5 ± 1.3** ‡ | 113.1 ± 13.1 |
| contains | 52.3 ± 11.7 | 2398.5 ± 669.6 | 557.0 ± 13.9 ‡ | **22.2 ± 0.7** |
| group-avg | 141.9 ± 12.3 | 3663.1 ± 575.8 | **91.4 ± 5.4** | 493.9 ± 16.4 |
| path-plus | **5.7 ± 2.5** | 10.1 ± 3.3 | 11.0 ± 1.6 ‡ | 67.2 ± 16.4 |
| distinct-obj | 13.4 ± 5.9 | 1135.5 ± 122.7 | 222.6 ± 16.9 ‡ | **5.8 ± 0.3** |
| export-500k | **171.1 ± 2.0** | 415.7 ± 10.6 | 562.6 ± 13.1 | 287.8 ± 12.1 |
| predicate-counts | **18.2 ± 7.1** | 2086.8 ± 14.5 | 70.7 ± 18.2 ‡ | 474.2 ± 7.5 |
| order-by-full | **448.8 ± 12.3** | 11353.7 ± 417.9 | 1167.4 ± 21.0 | 2106.9 ± 35.7 |
| optional-chain | **129.1 ± 2.8** | 2687.8 ± 279.2 | 481.5 ± 12.6 | OOM ² |
| minus | 42.6 ± 1.8 | 1301.6 ± 771.6 | **22.0 ± 2.5** ‡ | 57.9 ± 5.8 |
| subquery-agg | **10.0 ± 5.2** | 686.2 ± 312.8 | 20.3 ± 1.6 ‡ | 122.6 ± 10.7 |
| regex-iri | **43.8 ± 5.4** | 1906.4 ± 116.4 | 509.7 ± 19.5 ‡ | 673.1 ± 13.2 |
| knows-reach | 987.6 ± 38.1 | error ¹ | 1086.1 ± 21.0 ‡ | **564.4 ± 12.7** |
| distinct-join | **102.3 ± 17.1** | 3380.5 ± 9.7 | 182.7 ± 11.4 | 1577.2 ± 27.4 |
| lang-filter | 36.1 ± 6.8 | 1575.8 ± 888.8 | 310.5 ± 19.1 ‡ | **30.1 ± 3.7** |
| **update** (1-triple INSERT DATA) | 10.9 ± 4.1 | 38.6 ± 3.8 | 15.8 ± 1.0 | **6.8 ± 0.3** |
| **throughput** star-join, 16 clients (queries/s) | **144** | 7 | 57 | 51 |
| **server RSS** after the run (MiB) | 1608 | 3968 | 362 | 3204 |

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

Sparkles' RSS after the 10.5M run is mostly retained heap, not live data. The 16-client
throughput step makes glibc raise its dynamic mmap threshold, so large buffers that
were freed stay in the process. With `MALLOC_MMAP_THRESHOLD_=131072` the same workload
ends at 672 MiB. This includes the 500 MiB of decoded blocks cached by then.

Index size at 10.5M:

| Sparkles | QLever | Fluree (including its commit log) | TDB2 |
|---:|---:|---:|---:|
| 286 MB | 277 MB | 413 MB | 1.4 GB |

Peak RSS during the Sparkles bulk load: 1.9 GB.

## Where Sparkles loses

Sparkles is the fastest of the four on 18 of 20 queries at 1.05M and 9 of 20 at 10.5M.
Head to head:
* **vs. QLever:** it wins 19 of 20 at 1.05M and 14 of 20 at 10.5M.
* **vs. Fluree:** it wins 18 of 20 at 1.05M (plus one tie) and 14 of 20 at 10.5M.
* **vs. Jena/Fuseki:** it wins every query; Fuseki is 1.8–34× slower at 1.05M and
  1.8–400× slower at 10.5M.

### Specific benchmarks

| Case | Loses to | By | Likely cause |
|---|---|---|---|
| `range-topk` (range FILTER on a decimal + ORDER BY … LIMIT) | QLever | 1.9× at 1M, 5.0× at 10M | QLever prefilters blocks on sorted ids, so it skips blocks outside the range. Sparkles decodes and filters every row of the predicate, then sorts the survivors. |
| `two-hop-count` (`?a knows ?b . ?b knows ?c`, COUNT) | Fluree, QLever | Fluree 1.5× at 1M and 2.4× at 10M; QLever 1.6× at 10M | The count only needs per-key degrees on the shared `?b`. Sparkles' count join multiplies merge-join runs without materializing pairs, but still materializes both scans in full. |
| `contains` (`COUNT` + `FILTER(CONTAINS(?name, "Ada"))`) | Fluree | 2.4× at 10M (Sparkles 2.2× faster at 1M) | The filter itself tests stored vocabulary keys in parallel. About half the time goes to materializing the 1M-row name column first. |
| `distinct-obj` (`COUNT(DISTINCT ?o)` over `foaf:knows`) | Fluree | 2.3× at 10M (tie at 1M) | Sparkles counts runs in the object-sorted index without materializing rows, but walks its 2.5M rows block by block on one thread. |
| `minus` | QLever | 1.9× at 10M (Sparkles 2.2× faster at 1M) | QLever's MINUS merges sorted inputs; Sparkles materializes both sides and builds a hash set. |
| `knows-reach` (`foaf:knows*` from one node) | Fluree | 1.75× at 10M (Sparkles 1.6× faster at 1M) | Sparkles' transitive-path operator expands the frontier one node at a time; batched frontier expansion over sorted ids is not implemented yet. |
| `group-avg` | QLever | 1.6× at 10M | Generic GROUP BY builds per-row key vectors and collects values per group; incremental aggregation states would avoid that. |
| `optional-count` | QLever | 1.4× at 10M | Both sides of the OPTIONAL are materialized before joining. |
| `lang-filter` (`LANGMATCHES(LANG(?t), "en")`) | Fluree | 1.2× at 10M (Sparkles 1.9× faster at 1M) | As with `contains`, most of the time is spent materializing the 500k-row title column. |
| `count-all`, `types-grouped` | Fluree, QLever | within noise at 10M | All of them answer from index metadata or runs. |
| Single-triple update latency | Fluree | 1.6× at 10M (6.8 vs 10.9 ms; tie at 1M) | Sparkles fsyncs its WAL and publishes a new snapshot before acknowledging. Fluree documents that it indexes in the background. |
| Memory: server RSS after the 10M run | QLever | 4.4× (362 vs 1608 MiB) | See the note above: most of the difference is allocator retention after the concurrent throughput step. Sparkles also materializes every intermediate result and keeps a 1 GiB decoded-block cache; QLever streams lazily and uses a memory-limited allocator. |
| Large results | QLever / Jena (in principle) | — | Sparkles serializes the whole response in memory before sending. The others stream. `export-500k` is still fastest in Sparkles at this size, but memory grows with result size. |

Where Sparkles wins against QLever at 10.5M, it is often by a wide margin:
`distinct-obj` 17×, `regex-iri` 12×, `contains` 11×, `lang-filter` 8.6×,
`predicate-counts` 3.9×, `export-500k` 3.3× and `order-by-full` 2.6×.

Fluree is fast on single-pattern scans and counts, but general joins, OPTIONAL,
subqueries, grouping and sorting are much slower:

| Query | Fluree slower by (1.05M / 10.5M) |
|---|---|
| `optional-chain` | 61× (1.47 s); OOM at 10.5M |
| `predicate-counts` | 11× / 26× |
| `distinct-join` | 3.6× / 15× |
| `regex-iri` | 5.1× / 15× |
| `subquery-agg` | 3.0× / 12× |
| `path-plus` | 3.9× / 12× |
| `order-by-full` | 1.6× / 4.7× |
| `group-avg` | 3.4× / 3.5× |

Fluree's throughput is also lower: 497 vs 898 q/s at 1.05M, and 51 vs 144 at 10.5M.
It uses about 10× the memory at 1.05M (2.2 GiB vs 229 MiB).

Jena/Fuseki wins no query at either size. Its one advantage is that it streams results
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
* **Text search, spatial queries, inference-time reasoning.** Sparkles has none of these
  (see the README), so they were not benchmarked. Jena's `jena-text` / GeoSPARQL and
  QLever's text and spatial joins have no Sparkles counterpart.
* **Result-cache benefit.** All runs had caches off. With the cache on, repeated
  queries are mostly served from memory, which is not a fair comparison.

## Other measurements

| | Sparkles |
|---|---:|
| RDFS materialization, 1.0M → 2.75M inferred triples | 3.9 s end to end |
| SHACL validation, 20 shapes, 1.05M triples, 48,428 results (`mise run bench:shacl 100000`) | 164 ms parallel, 741 ms sequential |
| `ASK { ?s ?p ?o }` / `SELECT * … LIMIT 100` at 10.5M (early termination) | 1.9 ms / 1.8 ms |
