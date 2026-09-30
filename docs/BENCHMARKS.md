# Benchmarks

This document records how Sparkles compares with **Apache Jena (TDB2 + Fuseki)** and
**QLever** as of 2026-09-30, and where it still loses. Reproduce it with
`mise run bench [people] [workdir]` (or `scripts/bench.sh`).

## Setup

* **Data:** `scripts/gen-data.py N` generates synthetic data with about 10.5 triples per
  person: people, organisations, documents, typed literals (integers, decimals,
  dates), `foaf:knows` and citation graphs, language-tagged titles, and a small OWL
  class hierarchy. Two sizes were measured:
  * **1.05M triples** (N = 100k)
  * **10.5M triples** (N = 1M, 1.12 GB of N-Triples)
* **Engines:**
  * Sparkles: `sparkles serve --result-cache-mb 0`, so its result cache is off.
  * Jena: loaded with `tdb2.tdbloader` 6.2.0, served by Fuseki 5.1.0 with `-Xmx8G`
    (5.1.0 is the Fuseki that nixpkgs packages).
  * QLever 0.5.48: `-m 8G -j 16`. Its result cache is cleared before every
    measurement.
* **Method** (hyperfine):
  * Queries: SPARQL protocol POST, TSV results, 2 warm-ups + 10 runs at 1M, 1 + 5 at 10M.
  * Times are mean ± σ in ms and include about 10 ms of `curl` process overhead.
  * Loads: 1 run each.
  * Every engine's row count is checked before timing, and all agree except where
    marked `error`.
  * Extra measurements: a single-triple `INSERT DATA`; throughput of 160 `star-join`
    requests from 16 parallel clients; server RSS after the run.
* **Machine:** Intel Core Ultra X7 358H (16 threads), 30 GB RAM, NVMe, Linux 6.18. The
  OS page cache is warm, so these are not cold-start numbers.

## Results: 1.05M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) |
|---|---:|---:|---:|
| **load** (s) | **0.81** | 4.34 | 1.54 |
| count-all | **9.5 ± 3.7** | 177.1 ± 32.4 | 26.4 ± 5.9 |
| types-grouped | **9.5 ± 4.4** | 73.4 ± 11.7 | 17.9 ± 3.0 |
| star-join | **34.1 ± 1.6** | 59.0 ± 5.9 | 62.1 ± 3.5 |
| two-hop-count | **44.9 ± 2.6** | 313.1 ± 13.8 | 46.6 ± 9.3 |
| range-topk | 28.4 ± 1.0 | 87.2 ± 6.3 | **20.0 ± 1.6** |
| optional-count | **18.0 ± 6.2** | 94.3 ± 11.2 | 21.8 ± 7.0 |
| contains | **48.8 ± 8.0** | 61.1 ± 12.6 | 99.3 ± 11.4 |
| group-avg | **37.9 ± 6.9** | 197.0 ± 13.5 | 39.2 ± 2.0 |
| path-plus | **13.7 ± 0.9** | 21.6 ± 3.3 | 18.1 ± 1.3 |
| distinct-obj | **43.2 ± 2.0** | 109.7 ± 14.2 | 53.8 ± 5.0 |
| export-500k | **185.6 ± 2.9** | 452.0 ± 14.8 | 798.6 ± 56.6 |
| predicate-counts | **26.6 ± 1.1** | 276.2 ± 9.8 | 33.6 ± 4.3 |
| order-by-full | **108.5 ± 6.6** | 606.1 ± 23.9 | 228.8 ± 5.3 |
| optional-chain | **42.1 ± 5.9** | 244.9 ± 12.7 | 121.7 ± 20.0 |
| minus | 23.9 ± 0.9 | 71.6 ± 10.7 | **20.5 ± 2.2** |
| subquery-agg | **13.8 ± 0.9** | 79.2 ± 12.2 | 18.7 ± 1.3 |
| regex-iri | **45.8 ± 14.5** | 76.1 ± 16.2 | 95.2 ± 8.6 |
| knows-reach | **45.7 ± 7.7** | error | 119.7 ± 17.9 |
| distinct-join | **45.1 ± 1.5** | 365.8 ± 17.3 | 52.7 ± 8.3 |
| lang-filter | **31.9 ± 5.4** | 72.9 ± 2.4 | 87.3 ± 23.2 |
| **update** (1-triple INSERT DATA) | **10.3 ± 4.1** | 24.3 ± 9.2 | 14.1 ± 3.7 |
| **throughput** star-join, 16 clients (queries/s) | **657** | 58 | 505 |
| **server RSS** after the run (MiB) | 176 | 1948 | 172 |

## Results: 10.5M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) |
|---|---:|---:|---:|
| **load** (s) | **6.11** | 42.57 | 9.12 |
| count-all | **7.5 ± 2.2** | 1358.5 ± 14.4 | 58.2 ± 16.1 |
| types-grouped | 31.6 ± 1.3 | 5359.0 ± 434.0 | **18.1 ± 6.4** |
| star-join | **94.1 ± 6.9** | 234.9 ± 10.9 | 176.1 ± 21.3 |
| two-hop-count | 156.5 ± 4.5 | 2989.9 ± 18.4 | **94.0 ± 16.4** |
| range-topk | 88.2 ± 12.0 | 689.9 ± 24.9 | **49.7 ± 10.6** |
| optional-count | **60.1 ± 11.8** | 1319.4 ± 233.9 | 64.9 ± 13.2 |
| contains | **145.4 ± 12.7** | 3303.3 ± 201.9 | 634.4 ± 28.9 |
| group-avg | **143.8 ± 8.4** | 3813.0 ± 135.9 | 160.1 ± 13.5 |
| path-plus | **11.5 ± 4.3** | 15.8 ± 5.9 | 27.0 ± 8.4 |
| distinct-obj | **141.9 ± 9.4** | 1832.9 ± 579.5 | 246.1 ± 5.8 |
| export-500k | **195.6 ± 10.3** | 460.2 ± 27.5 | 650.3 ± 12.8 |
| predicate-counts | **45.3 ± 5.6** | 2546.9 ± 48.5 | 90.2 ± 17.3 |
| order-by-full | **512.2 ± 5.2** | 15055.6 ± 533.5 | 1254.1 ± 20.4 |
| optional-chain | **132.3 ± 10.1** | 2670.0 ± 57.7 | 560.7 ± 13.8 |
| minus | 52.2 ± 5.6 | 616.6 ± 224.2 | **51.7 ± 2.8** |
| subquery-agg | **15.8 ± 7.2** | 1844.7 ± 1028.0 | 31.6 ± 11.0 |
| regex-iri | **162.6 ± 16.3** | 2619.5 ± 33.9 | 562.8 ± 25.6 |
| knows-reach | **1037.4 ± 8.8** | error | 1315.3 ± 64.5 |
| distinct-join | **163.0 ± 5.3** | 4021.7 ± 71.2 | 248.2 ± 16.6 |
| lang-filter | **129.9 ± 18.5** | 2798.2 ± 128.6 | 350.6 ± 27.2 |
| **update** (1-triple INSERT DATA) | **13.5 ± 0.7** | 20.1 ± 7.1 | 18.2 ± 1.1 |
| **throughput** star-join, 16 clients (queries/s) | **116** | 5 | 57 |
| **server RSS** after the run (MiB) | 824 | 3735 | 293 |

Index size at 10.5M: Sparkles 286 MB, QLever 277 MB, TDB2 1.4 GB. Peak RSS during the
Sparkles bulk load: 1.9 GB.

## Where Sparkles loses

### Specific benchmarks

| Case | Loses to | By | Likely cause |
|---|---|---|---|
| `range-topk` (range FILTER on a decimal + ORDER BY … LIMIT) | QLever | 1.4× at 1M, 1.8× at 10M | QLever prefilters blocks on sorted ids, so it skips blocks outside the range. Sparkles decodes and filters every row of the predicate. |
| `types-grouped` (`?s a ?t` GROUP BY ?t) | QLever | 1.7× at 10M (Sparkles wins at 1M) | Both answer from index metadata or runs. QLever's per-relation metadata gives counts without touching blocks; Sparkles still walks the `rdf:type` runs. |
| `two-hop-count` (`?a knows ?b . ?b knows ?c`, COUNT) | QLever | 1.7× at 10M (tie at 1M) | Sparkles counts merge-join run products without materializing, but still scans both sides fully; QLever's lazy block zipper join overlaps decompression with the join. |
| `minus` | QLever | within noise (1.1×) | Both are fast; QLever's merge-based MINUS avoids a hash build. |
| Memory: server RSS after the 10M run | QLever | 2.8× (824 vs 293 MiB) | Sparkles materializes every intermediate result and keeps a 1 GiB decoded-block cache. QLever streams lazily and uses a memory-limited allocator. |
| Large results | QLever / Jena (in principle) | — | Sparkles serializes the whole response in memory before sending. The others stream. `export-500k` is still fastest in Sparkles at this size, but memory grows with result size. |

Jena/Fuseki wins no timed query in either run. Its one advantage is that it streams
results (see the memory row). It is 1.3–180× slower. Fuseki 5.1 returned HTTP 500 on
`knows-reach` (`<person/0> foaf:knows* ?x`) at both sizes.

In one earlier 10M attempt, QLever stalled for over 17 minutes on the 16-client
throughput test. The re-run above completed, so treat that as an intermittent
observation, not a result.

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
