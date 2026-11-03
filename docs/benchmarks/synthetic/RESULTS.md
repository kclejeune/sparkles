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

Random-access mappings give up the kernel's automatic read-ahead, and the read windows
described below request it again per scan pass, so dense scans are faster with the
features than without them as well. These cold medians switch the first two features
together.

| Cold, median, ms | Both off | Both on (default) |
|---|---:|---:|
| 10.5M `star-lookup` | 69.3 | **46.7** |
| 10.5M `star-join` | 99.9 | **93.3** |
| 10.5M `contains` | 82.4 | **33.5** |
| 10.5M `regex-iri` | 55.2 | **31.3** |
| 10.5M `lang-filter` | 36.1 | **27.9** |
| 1.01B `entity-facts-1` | 84.1 | **8.7** |
| 1.01B `entity-summary-1` | 67.9 | **7.3** |
| 1.01B `label-regex` | 137.1 | **39.4** |
| 1.01B `abstract-contains` | 846 | **523** |
| 1.01B `export-1m` | 22,108 | **1,293** |

*The 1.01B rows are medians of 14 cold samples on a format 3 store, measured on forge with
`bench-regressions.py --cold` at commit 6d518b71. The 10.5M rows are older medians of
three restarts.* [X07](../../specs/X07-cold-sparse-reads.md#dense-reads-at-101b) also
compares read windows of 1, 4 and 16 MiB at 1.01B, which are within the noise of each
other except on `label-regex`.

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

In the build that introduced format 3, `star-join` at 10.5M was 6% to 8% slower than on
unsegmented blocks, because its index join read up to a quarter of a block's segments one
by one before it read the block whole. The join now looks ahead over its ranges at the
first cold segment of a block and reads the block whole at once when they span more than
a quarter of it. Warm instruction counts per request stay within 0.969 to 1.024 of
unsegmented blocks, and a warm pass makes no read hint. The store at 10.5M grows by 3.0%,
from 300.6 MB to 309.7 MB, and the load takes 3.7 seconds with a peak of about 2.05 GB
either way.

| Cold, forge, 10.5M, median ms | Unsegmented | Format 3 before the lookahead | Format 3 with it |
|---|---:|---:|---:|
| `star-join` | 77.8 | 83.8 to 85.7 | 76.9 to 78.5 |
| `star-lookup` | 20.9 | 11.1 to 11.8 | 10.9 to 11.6 |
| `employee-docs` | 15.0 | 10.3 to 11.0 | 10.1 to 10.6 |
| `knows-reach` | 215.2 | 201.2 to 202.0 | 199.5 to 201.4 |

*Measured on forge on 2026-11-01 with `bench-regressions.py --cold`, 7 or 11 samples in
interleaved rounds of A, B, B and A. The unsegmented column comes from the build at
72d85c7f on its own store, and the format 3 columns from 04d49b57 and from 38deb1e8 and
cd46d9c3, which differ only in the inlining of the lookahead.*

Before it reports ready, `sparkles serve` now runs one query that reads no data on a
scratch in-memory dataset and maps its own code into memory, which costs about 100 MB of
shared file pages in the resident set. This improves the server's first request after a
start, which is what a cold row measures. The harness is unchanged. It still waits for
Oxigraph with the query `ASK{}`, which runs Oxigraph's query path once before the timed
request, and for Sparkles on `/$/ready`, and both engines answer the same timed queries as
before, so the comparison stays fair. The cold rows of `bench.sh` against Oxigraph, in two
rounds of five restarts per engine and query, show Sparkles ahead on all five queries.

| Cold, `bench.sh`, median of 10, ms | Oxigraph 1.05M | Sparkles 1.05M | Oxigraph 10.5M | Sparkles 10.5M |
|---|---:|---:|---:|---:|
| `star-lookup` | 20.26 | **10.69** | 20.02 | **9.15** |
| `values-star` | 7.52 | **4.68** | 6.94 | **3.95** |
| `employee-docs` | 19.16 | **7.99** | 18.37 | **8.73** |
| `path-plus` | 2.54 | **2.49** | 2.25 | **1.80** |
| `star-join` | 1,189.0 | **19.0** | 12,466.8 | **78.6** |

*Measured on forge on 2026-11-02 at commit b1f206b9, with Oxigraph from the repository's
pinned packages. The build before the warm-up, 21da069d, took 4.15 and 3.52 ms on
`path-plus` in the same rounds. These rows come from a separate run from the 28-query
table above, and they supersede its rows for these five queries.*

Across all 31 queries of `bench-regressions.py --cold`, the warm-up build ran every query
faster than 21da069d, with a geometric mean ratio of 0.822 at 10.5M and 0.786 at 1.05M.
[X07](../../specs/X07-cold-sparse-reads.md#first-request-after-a-start) has the per-query
numbers and the warm comparison, and the
[run record](runs/2026-11-02-first-request-forge/validation.json) holds every sample.

`serve --prewarm` restarts with the blocks and pages that the last run used. Each sample
below starts a fresh server on an evicted store with the list that a warm pass of the same
query recorded, and waits for the background read to finish before it sends the query.
The time to ready stays at 52 ms with and without the prewarm.

| Cold, forge, median ms | 1.05M off | 1.05M on | Prewarm read | 10.5M off | 10.5M on | Prewarm read |
|---|---:|---:|---:|---:|---:|---:|
| `star-lookup` | 12.7 | **3.5** | 18.8 | 11.4 | **3.3** | 15.3 |
| `values-star` | 7.2 | **3.9** | 5.7 | 7.3 | **3.7** | 5.7 |
| `employee-docs` | 9.2 | **3.6** | 9.2 | 10.5 | **3.2** | 12.3 |
| `path-plus` | 3.7 | 3.5 | 2.7 | 3.4 | 3.3 | 2.8 |
| `star-join` | 21.5 | **11.4** | 18.5 | 77.9 | **33.4** | 71.1 |

The final build runs `group-avg` and `expr-agg-arg` at 10.5M within 0.5% of main warm and
no slower cold, with the instructions and cycles of main. Warm point lookups run 1.6% to 2.5%
more instructions for the check of cold segments, which
[item 516](../../ROADMAP.md#performance-follow-ups) covers.
The [immutable run record](runs/2026-11-01-cold-lookups-forge/validation.json) holds the
revisions, binary hashes, scripts, compressed samples and answer hashes of every step.

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

## Eager-against-cursor matrix

`mise run bench:executors` is the rewrite gate of the executor paths and the oracle of
the unified executor, roadmap item 453, as
[BENCHMARKING.md](../../BENCHMARKING.md#the-eager-against-cursor-matrix) describes. Its
baseline ran on forge alone with a release build of 04d49b57 and the scripts of 6cd4b67e.
It ran the 31 queries of `scripts/bench.sh` with `execution=eager` and with
`execution=streaming` at 1.05M and 10.5M triples, with 12 Rayon threads and with one. In
every cell the two modes returned the same answer to every query, and every timed answer
of every server process had the digest of that answer. For `export-500k`, a LIMIT without
ORDER BY, both modes returned the same number of distinct rows, all of them triples of the
input. The [run record](runs/2026-11-01-executor-matrix-forge-6cd4b67e/record.json) holds every sample, the answer fingerprints, the
compilers and the build configuration, and its [summary](runs/2026-11-01-executor-matrix-forge-6cd4b67e/summary.md) lists each
cell's wall and execution times. The [commands](runs/2026-11-01-executor-matrix-forge-6cd4b67e/run.sh.txt) and each cell's raw
results are kept beside it.

The table gives each query's median eager wall time in milliseconds and the ratio of the
streaming wall time to it, so a ratio above 1 means streaming execution was slower.

| Query | 1.05M, 12 threads | ratio | 1.05M, 1 thread | ratio | 10.5M, 12 threads | ratio | 10.5M, 1 thread | ratio |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| class-predicate-list | 1.67 | 1.18 | 2.79 | 1.00 | 11.3 | 1.05 | 11.3 | 1.04 |
| class-predicates | 1.69 | 1.17 | 2.80 | 1.00 | 11.3 | 1.06 | 11.3 | 1.04 |
| contains | 1.44 | 1.10 | 3.60 | 1.02 | 4.79 | 1.08 | 25.0 | 1.01 |
| count-all | 0.483 | 1.60 | 1.15 | 1.71 | 0.469 | 1.60 | 1.89 | 0.86 |
| distinct-join | 4.85 | 1.17 | 5.05 | 1.10 | 44.6 | 1.01 | 44.5 | 1.04 |
| distinct-obj | 0.496 | 1.54 | 1.89 | 1.04 | 0.476 | 1.58 | 1.92 | 1.01 |
| employee-docs | 0.658 | 1.38 | 1.98 | 1.00 | 0.695 | 1.43 | 1.98 | 1.00 |
| exists-join | 1.84 | 1.16 | 2.80 | 1.00 | 9.95 | 1.01 | 9.92 | 1.00 |
| export-500k | 247 | 1.21 | 265 | 1.40 | 254 | 1.16 | 272 | 1.36 |
| expr-agg-arg | 3.83 | 0.79 | 3.86 | 0.83 | 29.5 | 0.83 | 30.7 | 0.78 |
| expr-bind-group | 1.47 | 0.92 | 1.97 | 1.00 | 5.54 | 0.82 | 6.83 | 0.67 |
| expr-order-key | 2.03 | 1.07 | 2.80 | 1.00 | 12.1 | 0.98 | 13.9 | 0.83 |
| group-avg | 5.19 | 0.91 | 5.85 | 0.82 | 49.6 | 0.84 | 48.8 | 0.84 |
| knows-reach | 12.9 | 0.98 | 12.8 | 0.99 | 142 | 0.99 | 140 | 0.99 |
| lang-filter | 1.36 | 1.18 | 2.80 | 1.00 | 3.61 | 1.09 | 17.1 | 1.00 |
| minus | 0.978 | 1.18 | 1.97 | 1.00 | 4.08 | 0.88 | 4.49 | 0.82 |
| not-exists | 1.54 | 1.11 | 1.97 | 1.00 | 10.6 | 0.95 | 10.7 | 0.92 |
| optional-chain | 14.8 | 1.27 | 15.1 | 1.46 | 140 | 1.21 | 145 | 1.51 |
| optional-count | 1.24 | 1.15 | 1.97 | 1.00 | 6.22 | 1.00 | 6.58 | 0.95 |
| order-by-full | 48.8 | 1.17 | 85.5 | 1.19 | 518 | 1.08 | 1,036 | 1.08 |
| path-plus | 0.509 | 1.61 | 1.96 | 0.70 | 0.482 | 1.65 | 1.97 | 1.00 |
| predicate-counts | 0.48 | 1.77 | 1.30 | 1.52 | 0.494 | 1.63 | 1.97 | 1.01 |
| predicate-subjects | 0.465 | 1.80 | 1.97 | 0.83 | 0.467 | 1.70 | 1.94 | 1.02 |
| range-topk | 1.51 | 1.08 | 1.97 | 1.00 | 3.15 | 1.10 | 7.17 | 0.89 |
| regex-iri | 1.65 | 1.23 | 4.74 | 1.04 | 6.74 | 1.08 | 37.2 | 1.02 |
| star-join | 4.96 | 1.34 | 5.00 | 1.28 | 33.6 | 1.30 | 36.1 | 1.44 |
| star-lookup | 0.818 | 1.33 | 1.98 | 1.00 | 0.781 | 1.32 | 1.98 | 1.00 |
| subquery-agg | 0.84 | 1.27 | 1.97 | 1.00 | 1.46 | 1.35 | 2.84 | 1.02 |
| two-hop-count | 2.03 | 1.17 | 3.85 | 1.01 | 12.4 | 1.01 | 29.0 | 1.00 |
| types-grouped | 0.558 | 1.47 | 1.96 | 1.01 | 0.571 | 1.41 | 1.95 | 1.01 |
| values-star | 0.676 | 1.40 | 1.98 | 1.00 | 0.663 | 1.34 | 1.98 | 1.00 |
| **geometric mean** | | 1.23 | | 1.04 | | 1.15 | | 0.99 |

With 12 threads, a streamed response costs about 0.3 ms more wall time than an eager one
on the shortest queries, such as `count-all`, `distinct-obj` and `predicate-counts`,
while the server's execution times of the two modes differ by hundredths of a
millisecond. The geometric mean of the execution-time ratios is between 0.94 and 1.02 in
every cell. The one-thread cells hide the fixed cost under their wall-time floor of about
2 ms, and their shortest queries sometimes come in under that floor, which gives ratios
such as 1.71 for `count-all` and 0.70 for `path-plus` at 1.05M triples that say nothing
about the execution modes. Streaming is also slower end to end on the large answers of
`export-500k`, `optional-chain`, `star-join` and `order-by-full`, by 8% to 51%, where much
of the time goes to writing the rows. It is faster on the grouped aggregates
`expr-agg-arg` and `group-avg` in every cell, on `expr-bind-group` in every cell with 12
threads and at 10.5M triples on `minus` and `not-exists`. Item 534 covers the
streamed response's fixed cost and its losses on large answers, which the merged executor
must not inherit.

### Forge record after stages 1 and 3

Items 474 to 478 and 480 to 483 were screened on Namespace only, so the matrix ran again
on forge alone with a release build of e52937ca and the scripts of that commit. Its
[run record](runs/2026-11-02-executor-matrix-forge-e52937ca/record.json),
[summary](runs/2026-11-02-executor-matrix-forge-e52937ca/summary.md) and
[commands](runs/2026-11-02-executor-matrix-forge-e52937ca/run.sh.txt) are kept beside the
baseline's, and it is the baseline of stage 4. Both modes returned the same answers in
every cell, and no answer fingerprint changed against the baseline. The comparison of
execution times with `bench-gate.py compare` flagged 70 of 248 metrics at the 5% threshold
([execution times](runs/2026-11-02-executor-matrix-forge-e52937ca/compare-exec.md),
[wall times](runs/2026-11-02-executor-matrix-forge-e52937ca/compare-wall.md)). Sixty-one
of them are queries under 0.3 ms, which a single matrix cannot time to that precision, and
item 524 measures them with the latency harness of X14's decision D4.

Interleaved forge A/Bs of the two builds reran the flags above 0.3 ms in eight or ten
rounds of A,B,B,A in each order. `order-by-full` at 10.5M triples with 12 threads read
1.012 in eager execution time and 0.956 in wall time, and 1.000 and 0.994 when streamed. `expr-bind-group` and `lang-filter` at 1.05M read 0.988 and 0.997,
`range-topk` and `subquery-agg` at 10.5M read 1.005 and 1.011, and `optional-chain` at
10.5M with one thread read 0.978. The flags were therefore noise of the single matrix
run.

### Forge record after stage 4

The 12-thread cells of the matrix ran again on forge alone with a release build of
9d086646, the build with stage 4's mode-neutral kernels, and the scripts of e52937ca. Its
[run record](runs/2026-11-02-executor-matrix-forge-9d086646/record.json),
[summary](runs/2026-11-02-executor-matrix-forge-9d086646/summary.md) and
[commands](runs/2026-11-02-executor-matrix-forge-9d086646/run.sh.txt) are kept beside the
record of e52937ca. Both modes returned the same answers in every cell, and no answer
fingerprint changed. Against the record of e52937ca, `bench-gate.py compare` flagged 34 of
124 execution times
([execution times](runs/2026-11-02-executor-matrix-forge-9d086646/compare-exec.md),
[wall times](runs/2026-11-02-executor-matrix-forge-9d086646/compare-wall.md)). Twenty-seven
of them are queries under 0.3 ms. The other seven are eager `lang-filter`, `regex-iri`,
`star-join` and `subquery-agg` and streamed `lang-filter` at 10.5M triples, and six wall
times of large eager answers, such as `export-500k` at 1.13.

Interleaved A/Bs on forge then compared the stage with main at the commit it merged,
58bbfca5, in rounds of A,B,B,A in each order with 12 threads. The samples, answers and
settings of the A/Bs after the fixes are kept in their own
[run record](runs/2026-11-03-executor-stage4-ab-forge-815822db/summary.md) with their
[commands](runs/2026-11-03-executor-stage4-ab-forge-815822db/run.sh.txt). They traced the flags to
three costs, which the stage removed. The `application/x-sparkles+json` writer rebuilt
each eager cell's key on the checked path, which made eager `export-500k` 1.248 times as
slow in wall time. Each key of a regular-expression filter looked up its thread's copy of
the program, which made eager `regex-iri` at 1.05M triples 1.058 times as slow. A
parallel map took pieces of at least 4,096 rows, which left the FILTER of eager
`subquery-agg` at 10.5M triples on five of twelve threads and made it 1.12 times as slow.
After the fixes, eager `export-500k` read 1.0085 in wall time, and eager execution at
10.5M triples read 1.029 for `subquery-agg`, 1.023 for `expr-bind-group`, 1.020 for
`regex-iri`, 1.011 for `lang-filter`, 0.995 for `expr-agg-arg` and 0.998 for `group-avg`.
The same queries read at most 1.011 when streamed at 10.5M triples, and with `star-join` at
most 1.02 in both modes at 1.05M triples. Item 682 holds the costs of 1% to 3%
that remain in eager execution at 10.5M triples. The latency harness of X14's decision D4 passed every query under 0.3 ms
except `star-lookup` at 1.019 in eager execution and 1.017 when streamed, and eager
`types-grouped` at 1.186, about 1.1 and 1.3 µs, which the
[per-query costs after stage 4](#per-query-costs-after-stage-4) follow up. Streamed `expr-order-key` at 10.5M triples read 1.045 against the build
before the mode-neutral kernels, which item 680 investigates.

### Per-query costs after stage 4

The forge check after stage 4 merged found the shortest queries 0.5 to 1.0 µs slower with
one thread than 005df56d, the main before it, and eager `order-by-full` 3% slower with 12
threads. [X14's outcome](../../specs/X14-unified-executor/SPEC.md#per-query-costs-after-stage-4)
describes the five costs behind them and their fixes. Interleaved forge A/Bs of the fixed
build, in rounds of A,B,B,A in each order, are kept in their own
[run record](runs/2026-11-03-exec-overhead-ab-forge-b4d59853/summary.md) with their
[commands](runs/2026-11-03-exec-overhead-ab-forge-b4d59853/run.sh.txt).

With one thread at 1.05M triples, in the latency harness of X14's decision D4, the fixed
build ran `count-all` at 0.924 of 005df56d, `distinct-obj` at 0.948, `path-plus` at 0.941,
`predicate-counts` at 0.978 and `predicate-subjects` at 0.967. `types-grouped` read 1.054,
0.38 µs more, which passes the D4 rule. `star-lookup` read 1.019 of 005df56d in two runs,
1.1 µs more with an upper bound of 1.026, against 0.988 for the build before the last
change, and item NEW-ITEM-1 follows the search loop whose alignment moved it. With 12
threads at 1.05M triples, the fixed build
ran the 30 queries at a geometric mean of 0.959 of 62d9cb53, the build that merged stage 4,
with eager `order-by-full` at 0.643, 8.8 ms against 13.7 ms, and every other query within
1.02 except `distinct-join` at 1.036, which item NEW-ITEM-1 follows. At 10.5M triples the
eager costs of 2% to 4% that item 682 holds remained, with `subquery-agg` at 1.040 of
005df56d, `lang-filter` at 1.026, `regex-iri` at 1.021 and `expr-bind-group` at 1.020.

The batch that followed, 62d9cb53 to 125edd41, changed no executor code. With one thread at
1.05M triples it ran `count-all` at 0.966 of 62d9cb53, `distinct-obj` at 0.996,
`predicate-counts` at 0.958, `types-grouped` at 0.964 and `path-plus` at 1.020, 0.21 µs
more, so it added no cost to the short queries that the Namespace screen flagged.
