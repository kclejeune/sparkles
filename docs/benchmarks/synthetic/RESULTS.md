# Synthetic suite results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Summary

The synthetic suite has 28 queries over generated data at 1.05 million and 10.5 million
triples. Medians are over the queries each engine completes.

| | 1.05M triples | 10.5M triples |
|---|---|---|
| Bulk load | **0.47 s** (QLever 1.36, Fluree 1.38, Oxigraph 4.42, TDB2 4.80) | **3.67 s** (QLever 9.35, Fluree 10.20, Oxigraph 18.22, TDB2 45.70) |
| Lowest native endpoint mean | 27 of 28 queries | 27 of 28 queries |
| Clear losses | none | none |
| Ties within 10% | Fluree on `count-all`, `distinct-obj`, `star-lookup` and `employee-docs`, Oxigraph on `path-plus` | Fluree on `count-all` and `distinct-obj`, Oxigraph on `path-plus` and `values-star` |
| Against QLever | Faster on 28 of 28, median 2.9× | Faster on 28 of 28, median 3.8× |
| Against Fluree | Lower native endpoint mean on 27 of 28; encoding differs | Faster on 26 of 27 completed; `count-all` is within 10% |
| Against Fuseki | Faster on all 28 completed | Faster on all 28 completed |
| Against Oxigraph | Faster on all 28, median 24× | Faster on all 28, median 100× |
| Throughput, 16 clients | **1,205 q/s** (QLever 475, Fluree 241, Fuseki 114, Oxigraph 25) | **239 q/s** (QLever 90, Fluree 22, Fuseki 12, Oxigraph 1.7) |
| Server RSS after the run | 408 MiB (**QLever 310**, Oxigraph 1,231, Fuseki 1,882, Fluree 3,077) | **780 MiB** (QLever 795, Oxigraph 2,074, Fluree 2,357, Fuseki 4,923) |
| Server RSS less the block cache | 345 MiB | 401 MiB |

## Synthetic suite

`scripts/gen-data.py` generates people, organisations, documents, typed literals,
`foaf:knows` and citation graphs, and the 28 queries of `scripts/bench.sh` cover scans,
counts, joins, OPTIONAL, property paths, subqueries, grouping, sorting, point lookups and
expressions. Queries are SPARQL protocol POSTs with TSV results, timed by hyperfine
around `curl`, so each time includes about 3.5 ms of process start and round trip. Times
are mean ± σ in ms over 10 runs after 2 warm-ups, and bold marks the fastest engine. Most
queries at 1.05M are close to the cost of starting curl and making the request.
`mise run bench` reproduces these tables.

The recovered Jena `knows-reach` cells use an explicit 256 MiB JVM thread stack with
an 8 GiB heap under the same 12 GiB server cap. The recovered Fluree `count-all` cell
uses a fresh isolated server. The [raw records and source-oracle receipt](runs/2026-10-25-error-recovery-forge/validation.json)
retain ten timed requests after two warm-ups and bind each query to its exact generated
source. Independent graph traversal verifies 89,090 reachable nodes at 1.05M and 892,556
at 10.5M. A source triple-set count verifies Fluree's 10,527,319 distinct triples. Raw
`agrees=null` values remain unchanged; these checks use source reconstruction rather
than a majority vote. The Fluree and Sparkles `count-all` means differ by less than 10%.

SELECT timers request TSV. Fluree returns a plain TSV dialect while its typed answer
checks use JSON, so these are native endpoint times with different serialization costs.
Fluree figures do not establish format-matched latency ratios.

### Synthetic suite at 1.05M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| count-all | **3.5 ± 0.1** | 170.1 ± 28.1 | 12.0 ± 1.0 ‡ | 3.6 ± 0.3 | 256.4 ± 3.1 |
| types-grouped | **3.6 ± 0.1** | 79.3 ± 6.7 | 5.4 ± 0.1 ‡ | 18.0 ± 1.9 | 43.5 ± 2.2 |
| star-join | **7.3 ± 0.3** | 30.9 ± 1.4 | 32.9 ± 5.5 ‡ | 18.3 ± 0.3 | 400.6 ± 18.5 |
| two-hop-count | **5.1 ± 0.2** | 386.8 ± 0.5 | 21.6 ± 2.9 ‡ | 7.0 ± 0.2 | 836.2 ± 6.5 |
| range-topk | **4.3 ± 0.1** ‡ | 95.2 ± 1.8 | 6.2 ± 0.2 | 20.0 ± 1.0 | 59.9 ± 3.0 |
| optional-count | **4.1 ± 0.2** | 81.7 ± 2.6 | 10.0 ± 0.5 ‡ | 13.8 ± 0.1 | 105.5 ± 8.6 |
| contains | **4.2 ± 0.2** | 42.7 ± 1.8 | 74.1 ± 3.4 ‡ | 8.7 ± 0.3 | 143.9 ± 3.6 |
| group-avg | **8.9 ± 0.1** | 210.1 ± 2.7 ‡ | 16.3 ± 1.6 ‡ | 56.1 ± 1.4 ‡ | 314.9 ± 5.6 |
| path-plus | **3.5 ± 0.1** | 6.3 ± 0.7 | 6.3 ± 0.3 ‡ | 7.3 ± 0.2 | 3.7 ± 0.3 |
| distinct-obj | 3.7 ± 0.1 | 92.1 ± 2.0 | 17.6 ± 2.6 ‡ | **3.6 ± 0.1** | 70.2 ± 1.5 |
| export-500k | **78.0 ± 1.6** | 500.5 ± 16.8 | 792.4 ± 17.1 | 219.9 ± 13.8 | 1738.4 ± 26.6 |
| predicate-counts | **3.5 ± 0.1** | 336.0 ± 2.8 | 15.1 ± 0.6 ‡ | 65.5 ± 0.6 | 288.5 ± 3.4 |
| order-by-full | **46.4 ± 6.1** | 540.2 ± 5.4 | 141.2 ± 3.1 | 105.4 ± 0.5 | 3852.4 ± 59.5 |
| optional-chain | **14.9 ± 0.5** | 228.3 ± 1.7 | 67.0 ± 6.7 | 1773.6 ± 88.9 | 358.6 ± 7.4 |
| minus | **3.8 ± 0.2** | 41.4 ± 1.3 | 7.2 ± 0.2 ‡ | 9.9 ± 0.3 | 32.6 ± 1.9 |
| not-exists | **4.6 ± 0.2** | 57.8 ± 1.0 | 7.2 ± 0.4 ‡ | 16.7 ± 0.9 | 113.9 ± 8.8 |
| exists-join | **5.0 ± 0.2** | 152.0 ± 1.0 | 9.9 ± 0.3 ‡ | 30.5 ± 6.3 | 1451.6 ± 21.6 |
| subquery-agg | **3.7 ± 0.3** | 52.3 ± 1.3 | 4.9 ± 0.1 ‡ | 25.7 ± 16.5 | 27.7 ± 4.4 |
| regex-iri | **4.3 ± 0.2** | 56.6 ± 2.0 | 72.8 ± 3.8 ‡ | 77.5 ± 0.7 | 171.7 ± 7.3 |
| knows-reach | **16.1 ± 0.2** | 364.1 ± 35.9 | 60.5 ± 6.1 ‡ | 58.2 ± 0.4 | 346.5 ± 4.0 |
| distinct-join | **8.2 ± 0.2** | 423.4 ± 1.4 | 25.9 ± 4.7 | 90.8 ± 3.5 | 679.6 ± 14.0 |
| lang-filter | **4.2 ± 0.2** | 32.3 ± 1.5 | 44.1 ± 3.8 ‡ | 6.3 ± 0.2 | 81.6 ± 6.1 |
| star-lookup | **3.7 ± 0.2** ‡ | 6.0 ± 0.4 ‡ | 10.5 ± 0.5 ‡ | 3.7 ± 0.1 | 4.7 ± 0.3 |
| values-star | **3.6 ± 0.2** | 6.5 ± 0.5 | 11.1 ± 0.7 ‡ | 4.0 ± 0.1 | 4.0 ± 0.1 |
| employee-docs | **3.6 ± 0.1** | 6.6 ± 0.2 | 6.4 ± 0.2 | 3.8 ± 0.1 | 4.3 ± 0.1 |
| expr-bind-group | **4.5 ± 0.3** | 49.8 ± 2.0 ‡ | 7.4 ± 0.2 ‡ | 45.2 ± 0.2 | 42.1 ± 4.9 |
| expr-order-key | **5.1 ± 0.2** | 93.6 ± 3.1 | 13.7 ± 1.1 ‡ | 115.0 ± 1.0 | 2660.9 ± 15.5 |
| expr-agg-arg | **8.3 ± 0.5** | 172.6 ± 1.5 | 15.5 ± 2.2 ‡ | 74.3 ± 3.2 | 300.0 ± 5.6 |
| **throughput** star-join, 16 clients (queries/s) | **1205** | 114 | 475 | 241 | 25 |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-18 and 2026-10-19. The [record](../2026-10-23-synthetic-1.05m-forge.json)
has every sample and answer fingerprint.*

### Synthetic suite at 10.5M triples

| query | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|
| count-all | 3.6 ± 0.2 | 1855.8 ± 6.6 | 51.7 ± 3.3 ‡ | **3.5 ± 0.1** | 2515.5 ± 18.0 |
| types-grouped | **3.6 ± 0.2** | 3722.1 ± 792.9 | 6.1 ± 0.3 ‡ | 110.6 ± 0.9 | 386.4 ± 7.7 |
| star-join | **36.7 ± 4.2** | 327.2 ± 136.4 | 118.5 ± 10.5 ‡ | 140.9 ± 3.3 | 4789.6 ± 32.7 |
| two-hop-count | **17.5 ± 1.0** | 3954.2 ± 30.2 | 66.9 ± 6.4 ‡ | 35.9 ± 0.2 | 8294.6 ± 71.1 |
| range-topk | **6.6 ± 0.4** | 882.2 ± 3.5 | 21.7 ± 3.5 | 153.1 ± 0.5 | 595.3 ± 6.6 |
| optional-count | **10.6 ± 1.4** | 1102.5 ± 129.9 | 46.2 ± 3.2 ‡ | 113.1 ± 0.7 | 997.2 ± 16.5 |
| contains | **7.0 ± 0.2** | 2669.0 ± 880.1 | 689.3 ± 3.6 ‡ | 18.0 ± 1.0 | 1537.5 ± 19.1 |
| group-avg | **47.7 ± 0.4** | 3759.1 ± 620.2 ‡ | 93.8 ± 8.5 ‡ | 547.9 ± 9.3 ‡ | 4223.8 ± 47.2 |
| path-plus | **3.5 ± 0.1** | 6.7 ± 0.8 | 9.9 ± 1.4 ‡ | 50.5 ± 0.8 | 3.7 ± 0.1 |
| distinct-obj | **3.5 ± 0.1** | 1387.0 ± 58.1 | 205.8 ± 4.9 ‡ | 3.7 ± 0.1 | 748.3 ± 3.4 |
| export-500k | **78.4 ± 0.6** | 535.7 ± 15.9 | 674.4 ± 5.4 | 289.5 ± 1.0 | 2113.7 ± 15.2 |
| predicate-counts | **3.5 ± 0.1** | 3203.1 ± 62.9 | 72.4 ± 2.9 ‡ | 597.2 ± 18.3 | 2821.0 ± 18.4 |
| order-by-full | **367.4 ± 12.8** | 16333.8 ± 370.2 | 1379.5 ± 9.7 | 1848.2 ± 54.3 | 55652.5 ± 120.9 |
| optional-chain | **82.0 ± 5.2** | 2906.7 ± 61.5 | 558.9 ± 9.4 | error | 4917.9 ± 21.7 |
| minus | **7.6 ± 0.4** | 543.8 ± 24.4 | 19.4 ± 1.9 ‡ | 52.1 ± 2.5 § | 354.0 ± 2.4 |
| not-exists | **15.0 ± 1.1** | 569.5 ± 150.3 | 25.7 ± 2.9 ‡ | 147.5 ± 1.4 § | 1042.3 ± 13.1 |
| exists-join | **15.3 ± 1.3** | 1582.9 ± 26.2 | 61.6 ± 3.6 ‡ | 263.2 ± 4.7 § | 122805.6 ± 269.9 |
| subquery-agg | **4.6 ± 0.1** | 593.1 ± 9.4 | 14.8 ± 0.7 ‡ | 140.4 ± 3.0 § | 182.8 ± 1.6 |
| regex-iri | **8.3 ± 0.1** | 4029.7 ± 119.8 | 660.3 ± 4.9 ‡ | 742.4 ± 2.4 § | 2701.9 ± 8.4 |
| knows-reach | **145.5 ± 0.7** | 6287.4 ± 267.0 | 1142.2 ± 44.2 ‡ | 639.3 ± 6.4 § | 4952.2 ± 27.4 |
| distinct-join | **49.0 ± 0.8** | 4415.9 ± 9.7 | 155.2 ± 6.6 | 1196.9 ± 10.2 § | 6666.6 ± 25.4 |
| lang-filter | **6.2 ± 0.2** | 1683.5 ± 1324.2 | 385.8 ± 1.9 ‡ | 29.3 ± 0.5 § | 855.0 ± 10.2 |
| star-lookup | **3.7 ± 0.1** ‡ | 7.1 ± 0.7 | 24.9 ± 3.2 ‡ | 4.1 ± 0.2 § | 5.1 ± 0.2 |
| values-star | **3.9 ± 0.5** | 8.0 ± 0.5 | 10.8 ± 0.6 ‡ | 4.2 ± 0.2 § | 4.0 ± 0.2 |
| employee-docs | **3.6 ± 0.1** | 7.1 ± 0.6 | 13.6 ± 1.4 | 4.4 ± 0.1 § | 4.5 ± 0.3 |
| expr-bind-group | **9.6 ± 0.8** | 421.7 ± 13.6 ‡ | 34.2 ± 4.2 ‡ | 386.3 ± 1.7 § | 352.8 ± 4.9 |
| expr-order-key | **16.5 ± 2.0** | 877.1 ± 13.1 | 108.9 ± 2.6 ‡ | 1090.7 ± 6.6 § | 38261.1 ± 72.7 |
| expr-agg-arg | **27.1 ± 0.9** | 1805.7 ± 27.6 | 92.9 ± 9.0 ‡ | 737.6 ± 6.9 § | 4056.5 ± 46.5 |
| **throughput** star-join, 16 clients (queries/s) | **239** | 12 | 90 | 22 | 1.7 |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-18 and 2026-10-19. The [record](../2026-10-23-synthetic-10.5m-forge.json)
has every sample and answer fingerprint.*

‡ Same values as different RDF terms. QLever returns integers such as counts and ages as
`xsd:int`, where the data and the other engines use `xsd:integer`. Sparkles writes
`xsd:decimal` values in XSD 1.1 canonical form, so the data's `"175000.50"` comes back as
`"175000.5"`. In `group-avg` the averages agree to 12 significant digits.

§ Timed, but the answer was not checked, because the server had been killed earlier in the
answer check. Such a time is not ranked.

Fuseki 5.1 fails `knows-reach` (`<person/0> foaf:knows* ?x`) with a `StackOverflowError`
in its property path engine and answers HTTP 500 at both sizes. Fluree's `optional-chain`
at 10.5M grows past the 12 GiB memory limit and the kernel kills the server, both in the
answer check and in the warm-up pass before the timings. The answer check therefore has no
Fluree answers for the 14 queries after `optional-chain`, and Fluree's `count-all` is an
error because the server was not running when that query was timed. The harness restarts
a server that has stopped before it times the next query.

### Request latency without client startup

Starting `curl` costs more than most small queries take, so this Sparkles-only run times
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

### Cold starts

Cold mode restarts the server and evicts its files from the OS page cache before each
query, three times per query, and reports the median request time without process
startup. Eviction does not guarantee that the device itself is cold. A Sparkles server is
ready 0.09 s after a restart at both scales. Sparkles is fastest on 21 of 28 cold queries
at 1.05M and on 24 of 28 at 10.5M. The cold losses are these:

Jena also completes `knows-reach` with the larger stack after three process restarts
and file-cache evictions at each scale. Its median request times are 1,417.885 ms and
20,563.924 ms, with readiness recorded separately. The [cold records](runs/2026-10-25-error-recovery-forge/validation.json)
retain every request and the source-verified answer fingerprints. Changed-store cells
keep their own measurements; a loaded-store recovery does not establish a changed-store
result.

| Query, cold | Faster engine | 1.05M | 10.5M |
|---|---|---|---|
| `count-all` | Fluree at 1.05M | 1.55× (3.4 against 2.2 ms) | — |
| `two-hop-count` | Fluree at 1.05M | 1.05× (18.6 against 17.7 ms) | — |
| `contains` | Fluree at 1.05M | 1.17× (14.2 against 12.1 ms) | — |
| `path-plus` | Oxigraph | 2.95× (6.5 against 2.2 ms) | 2.94× (4.7 against 1.6 ms) |
| `distinct-obj` | Fluree at 1.05M | 3.0× (4.8 against 1.6 ms) | — |
| `star-lookup` | Oxigraph at 10.5M | — | 1.31× (20.1 against 15.4 ms) |
| `values-star` | Oxigraph | 1.51× (8.0 against 5.3 ms) | 1.50× (8.1 against 5.4 ms) |
| `employee-docs` | Fluree at 1.05M, Oxigraph at 10.5M | 1.38× (11.7 against 8.5 ms) | 1.07× (14.7 against 13.7 ms) |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-19. Medians of three restarts are noisy at these sizes.*

Sparse point lookups and several small scans lose because warm execution hides their
first-use reads and block decoding. Sparkles memory-maps its index and vocabulary, and
these features limit what a cold lookup reads:

* Random-access mappings and explicit read-ahead request only the columns or term ranges
  a query reads. `SPARKLES_IO_HINTS=off` disables them.
* A sparse vocabulary index, `vocab.idx`, holds the first key of every 128th front-coded
  block. It narrows a lookup to one range of offsets and data, and costs 66 KB at 10.5M and
  8.4 MB of memory at 1.01B quads. `SPARKLES_SPARSE_VOCAB=off` disables its use.
* An index join with up to 16,384 keys requests its blocks before it decodes the first, so
  their reads overlap.
* A filter, sort or aggregate over non-canonical numeric literals reads 8-byte values from
  the [numeric column](../dbpedia/RESULTS.md#numeric-literals-in-the-vocabulary) instead of decoding each key
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

[X07](../../specs/X07-cold-sparse-reads.md#remaining-cold-gaps) adds one read window per scan
pass, batched hints for a query's constants and first-scan prefetch, and profiles the
remaining cold time.

Index format 3 splits each column of a block into frames of 2,048 rows, so a point lookup
decodes a sixteenth of a column. This table compares stores in format 3 with stores whose
blocks are not segmented, each loaded by its own build. Each 10.5M cell is the median of
four interleaved rounds of 11 samples timed by `bench-regressions.py --cold`, and each
1.05M cell the median of two. These times come from a Python client and are not
comparable with the `bench.sh` times above.

| Cold, forge, median ms | 1.05M unsegmented | 1.05M format 3 | 10.5M unsegmented | 10.5M format 3 |
|---|---:|---:|---:|---:|
| `star-lookup` | 15.4 | **14.9** | 20.4 | **12.7** |
| `employee-docs` | 13.2 | **11.7** | 16.8 | **12.2** |
| `values-star` | 9.8 | **7.7** | 10.1 | **7.6** |
| `path-plus` | 8.0 | **5.0** | 6.7 | **4.7** |
| `knows-reach` | 33.5 | **32.1** | 198.9 | **197.4** |
| `star-join` | 27.8 | **25.8** | **82.7** | 89.2 |
| `export-500k` | 121.8 | **120.4** | 129.8 | **127.7** |

*Measured on forge on 2026-10-23, with the format 3 build at commit e86a371a.*

`star-join` at 10.5M is the one cold query that is slower, by 6% to 8% in two runs of 12
rounds. Its index join reads up to a quarter of a block's segments one by one before the
block is read whole, and [X07](../../specs/X07-cold-sparse-reads.md#phase-3-results) records the
fixes that were tried. Warm instruction counts per request stay within 0.969 to 1.024 of
unsegmented blocks, and a warm pass makes no read hint. The store at 10.5M grows by 3.0%,
from 300.6 MB to 309.7 MB, and the load takes 3.7 seconds with a peak of about 2.05 GB
either way.

## Separate Wally optional-chain profile

The unchanged `optional-chain` query at 10.5M triples was also checked on the Core
i7-12800H Wally profile with a 26 GiB server cap. Jena uses a 16 GiB heap and 256 MiB
stack, and QLever's query allowance is 20 GiB. The generated source SHA-256 exactly
matches the Forge corpus. An independent relation-based oracle reconstructs both nested
OPTIONAL joins, preserving projected duplicates and unbound fields. All five engines'
complete typed JSON answers match its 300,191 rows and exact and value fingerprints.

| Engine | Native request mean ± σ ms | Answer and timing disposition |
|---|---:|---|
| Sparkles `6b7f32ce` | 90.3 ± 2.9 | Complete source-oracle match. |
| Jena | 2878.8 ± 26.5 | Complete source-oracle match. |
| QLever | 583.2 ± 14.5 | Complete source-oracle match. |
| Fluree | unavailable | One complete correct answer succeeds. Repeated warm requests exceed the 26 GiB memory-cgroup cap. |
| Oxigraph | 5448.8 ± 30.7 | Complete source-oracle match. |

The [raw records and validation receipt](runs/2026-10-25-error-recovery-wally-optional-chain/validation.json)
retain two warm-ups and three valid timed samples for each of the four completed timing
cells. Fluree's failed fast samples are discarded, and its correct single answer does
not establish a warm mean. Its first answer peaks at 12,067 MiB RSS. Kernel events bind
the later failure to the memory cgroup. These results remain separate from Forge and
do not replace its missing Fluree latency. The
[recovery inventory](../2026-10-25-query-error-recovery-inventory.json) names the remaining
changed-store and resource-limited comparisons without inferring success from this run.


## Short-query integration control

A quiet Intel control compares the matched public builds `5a7e4238` and `adc2dcbe`
after three queries crossed the initial Namespace screen threshold. Ten longer
Namespace rounds brought each below 10%. Twelve interleaved Forge rounds with
200 timed requests per process then measured server execution changes of −0.7% for
`minus`, −0.3% for `star-lookup` and +2.8% for `values-star`. Client time changed by
−0.8%, +0.2% and −0.5%. The small `values-star` change is about 0.4 microseconds
at a 14-microsecond baseline and remains under roadmap item 328.

These are internal comparisons on 1.05 million generated triples. They do not replace
the cross-engine tables. Both builds use identical compilers, run alone on Forge with
pinned server and client cores, and return matching sorted TSV answers for all three
queries. The [immutable run record](runs/2026-10-26-post-adc2dcbe-intel-control/validation.json)
contains the source and binary identities, settings, initial flags, longer rerun and
compressed request samples.

## Short-query check of the startup and credential changes

A Namespace screen of the batch that added bind-before-open startup and credential
expiry measured `values-star` about 16% slower in server time, 0.021 against 0.025 ms.
A pinned check on Forge did not reproduce a cost of that size. It built `34b69139`, the
parent of both changes, the flagged batch `65e369db` and `1acdc38a`, the head when the
check started, on the host with identical compilers. The base then ran against each of
the other two for 24 interleaved rounds of A, B, B and A, with 200 timed requests per query
and server process over 1.05 million generated triples. Each side loaded its own store.
The server and the client ran on separate cores of an Intel Core i5-13500 that held the
host's benchmark locks, and steal time was zero.

| query | base (ms) | `65e369db` (ms) | ratio | base (ms) | `1acdc38a` (ms) | ratio |
|---|---:|---:|---:|---:|---:|---:|
| `values-star` | 0.0143 | 0.0137 | 0.959 | 0.0140 | 0.0143 | 1.019 |
| `count-all` | 0.0041 | 0.0041 | 0.995 | 0.0042 | 0.0040 | 0.967 |
| `star-lookup` | 0.0579 | 0.0558 | 0.964 | 0.0579 | 0.0570 | 0.984 |
| `employee-docs` | 0.0315 | 0.0316 | 1.005 | 0.0314 | 0.0319 | 1.016 |
| `minus` | 0.2954 | 0.2951 | 0.999 | 0.2948 | 0.2964 | 1.006 |
| `path-plus` | 0.0091 | 0.0086 | 0.945 | 0.0091 | 0.0094 | 1.026 |
| `star-join` | 1.486 | 1.366 | 0.919 | 1.488 | 1.363 | 0.916 |

Each cell is the median server execution time over every sample of a side. The flagged
batch ran `values-star` 4.1% faster than the base and was slower in one round of 24. The
head ran it 1.9% slower, which is about 0.3 microseconds, and was slower in 18 rounds of
24 with round ratios from 0.939 to 1.065. The medians of the server processes of one
build spanned 10% to 13% of their own median, which is more than either change. Client
time is dominated by 2 ms of curl process start, and it moved by 0.4% or less on
`values-star`. All sorted answers match across the three builds.

The flagged batch is therefore not slower on Forge. The later drift is about the 2.8%
that the earlier [integration control](#short-query-integration-control) accepted under
roadmap item 328. The builds differ in both directions on the queries that take a few
microseconds, which matches code placement better than added work. `star-join` is 8%
faster in both builds. The
[immutable run record](runs/2026-10-31-values-star-startup-forge/validation.json)
holds the revisions, binary hashes, settings, per-round ratios and compressed request
samples. These are internal comparisons and make no cross-engine claim.
