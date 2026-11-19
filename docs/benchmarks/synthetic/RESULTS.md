# Synthetic suite results

This is a [manual report](../README.md#evidence-and-generated-reports). Read the
[reference setup and answer rules](../reference/README.md) with each result.

## Retained-memory and optional-feature comparison

The [manual source-pair record](runs/41dff4bc69c62404fb7eb9a69aefb5c3cbbe1a0284de4bd80748ece7c2c9132c/README.md) compares
`765fc956` with `6e696f22` at 1.05M triples, with one query thread and result caching
disabled. Its Namespace rerun retains a `distinct-obj` execution ratio of 1.176406.
The exact same binaries on forge read 1.092276 in the forward source order and 1.029931
in the reversed order, after orienting both as later source over baseline. Intel’s
`count-all` execution ratio is 1.119036 forward and 1.014420 reversed. No aggregate
execution increase reaches 10% in both Intel source orders, and no aggregate wall
increase reaches 10% in either order. The failed Namespace screen remains distinct.

All 90,880 Intel timed samples and all 30+30+11 paired sorted TSV references are retained.
The reference checks do not cover every timed native response. Fresh-process spreads
and per-round ratios remain visible for every query and both metrics. Item 759 tracks
all positive and tiny-query leads, whose union covers all 30 queries, without claiming
that they are all confirmed costs or all noise. The record does not authorize publication
or establish the performance of a different source or build.

## Horn classification and kept-frame screen leads

The matched-compiler Namespace screen of `7070fa7c` against `aaa6f015` returns
the same sorted TSV answers for all 30 synthetic queries at 100,000 generated
people. Both binaries were built on atlas and ran on one AMD EPYC instance with
one Rayon thread, four A,B,B,A rounds, three warm-ups and 20 timed requests per
query and server process. No median execution or client-wall ratio reaches 1.10.
These are observations about this source pair, not measurements of another build
or confirmation that the integration has no query cost.

Eight queries have a higher median in at least one measure. Ratios below divide
the candidate median by the base median and retain both measures for each lead.

| Query | Server execution B/A | HTTP wall time B/A |
|---|---:|---:|
| `class-predicate-list` | 1.0949 | 1.0794 |
| `class-predicates` | 1.0854 | 1.0325 |
| `count-all` | 1.0051 | 0.9615 |
| `distinct-obj` | 1.0250 | 1.0675 |
| `employee-docs` | 1.0113 | 1.0067 |
| `lang-filter` | 0.9859 | 1.0152 |
| `minus` | 1.0270 | 1.0414 |
| `optional-count` | 1.0175 | 0.8773 |

The [manual run record](runs/horn-kept-integration-screen-aaa6f015/README.md)
preserves exact revisions, binary hashes, compiler identities, inputs, answers
and samples. Namespace process spreads are larger than these differences and do
not establish a cause or a cost on Intel. Item 752 retains every lead, including
client-only and microsecond changes, for a quiet pinned Intel comparison of the
exact accepted integration against `7070fa7c`. Any additional positive medians
accepted from that build's own screen belong to the same follow-up. Confirmed
costs require attribution across the integration before selecting a repair.

## Expression sort-key cost after the dependency refresh

A matched-build control on forge compares `6d55f1e6` with `6724abe9` at 1.05M
triples, with one query thread and result caching disabled. The `expr-order-key`
execution median rises from 1.104797 to 1.120524 ms, a consistent 1.4235% cost
of about 15.73 microseconds. The paired-round 95% ratio interval is 1.011584 to
1.016480. Complete HTTP wall time remains 2.804 against 2.803 ms, with an interval
that includes equal time. The standard TSV references and all 1,280 native answer
checks agree within their respective fingerprint formats.

The [manual run record](runs/2026-11-05-dependency-refresh-expr-order-key/README.md)
retains the query, raw samples, build and input receipts, controls and reproducible
analysis. The comparison includes the dependency and numeric correctness repairs,
rendering preferences and documentation changes, so it does not identify a single
cause. The cost is accepted while retaining the correctness repairs, and roadmap
item 748 tracks attribution and recovery. It is not classified as noise.

## Query layout after the classification batch

A pinned counter comparison of `ae8a6cd5` and `5b0acdf9` on forge, at 1.05M
quads on one thread, reproduces the `regex-iri` loss at 1.062 of the base's
server time, with a 95% interval of 1.061 to 1.063. The base takes 3.467 ms and
the candidate 3.682 ms. Both execute 75.202 million user instructions per HTTP
request, while the candidate uses 1.059 times the cycles. The same-binary
control reads 1.001, with an interval of 0.999 to 1.002.

The cached midpoint `4ab93ecb` reads 0.999 of the base, so the loss appears after
the UI merge. All six checked queries keep their plans and answers. Four
selected hot regex and key-filter functions keep their machine instructions
while their linked addresses move. This supports a layout explanation,
without identifying a particular cache or branch conflict. Generic cache misses
do not establish an increase. The small `types-grouped` loss does not reproduce
in this diagnostic, where its candidate time reads 0.981 of the base.

Later checks with a compiler-matched build of classifier merge `e951e2c3`
keep the instruction counts, but do not repeat the full initial contrast.
With the six-query mix the original candidate reads 1.015 of the base and the
rebuilt merge 1.014, while the unchanged base itself takes longer. The
responsible cache or process state and a stable latency attribution to the
merge remain unconfirmed.

The [manual run record](runs/2026-11-04-item733-layout-forge/README.md) retains
both comparison orders, the same-binary control, original compressed samples,
counter coverage, compiler identities and method limitations. The investigation
does not change the engine or recover the measured regex cost. Its recovery
remains a roadmap follow-up.

## Summary

The [query-layout diagnostic](#query-layout-after-the-classification-batch)
qualifies the warm regex cost of a historical classification batch independently
of the peer comparison tables.

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

## Pinned checks of the screen flags of batches 19 to 29

Namespace screens of batches 19 to 29 flagged `path-plus`, `star-lookup`, `minus` and
`distinct-obj` on short queries, and item 856 measured a fixed cost on the four queries
under 0.01 ms. A pinned check on forge ran each flag's pre-flag base against 8baa5686, the
main branch at batch 30, for 24 interleaved rounds of A, B, B and A with 200 timed
requests per query and server process. Both builds of a pair loaded the same 1,052,807
generated triples. The server ran on CPU 2 with one Rayon thread and the client on CPU 4,
with spinners on both, the host's benchmark locks held and zero steal time. Execution was
the server's default. All builds came from `scripts/nsc-build.sh` on the laptop with the
same compilers, and every answer matched across each pair.

A query under 0.3 ms passes when the upper end of the 95% interval of its ratio is at most
1.02 or its median added server time is at most 0.5 µs, the rule of the
[X14 acceptance gates](../../specs/X14-unified-executor/SPEC.md#acceptance-gates). The
interval is the t interval of the mean log round ratio over the 24 rounds. Each query is
judged on the pair of its own flag.

| query | base | base ms | 8baa5686 ms | ratio | 95% interval | added µs | slower rounds | result |
|---|---|--:|--:|--:|---|--:|--:|---|
| `path-plus` | 06ca04ef | 0.00982 | 0.01028 | 1.046 | 1.028 to 1.058 | 0.46 | 23 of 24 | pass |
| `star-lookup` | c2b5d7cf | 0.06188 | 0.06125 | 0.990 | 0.951 to 1.006 | −0.64 | 7 of 24 | pass |
| `distinct-obj` | f854145b | 0.00363 | 0.00413 | 1.137 | 1.121 to 1.160 | 0.50 | 24 of 24 | pass |
| `count-all` | 321508fe | 0.00431 | 0.00458 | 1.064 | 1.049 to 1.085 | 0.28 | 22 of 24 | pass |
| `distinct-obj` | 321508fe | 0.00376 | 0.00402 | 1.069 | 1.054 to 1.085 | 0.26 | 23 of 24 | pass |
| `predicate-counts` | 321508fe | 0.00430 | 0.00466 | 1.082 | 1.069 to 1.094 | 0.35 | 24 of 24 | pass |
| `predicate-subjects` | 321508fe | 0.00625 | 0.00652 | 1.043 | 1.034 to 1.052 | 0.27 | 24 of 24 | pass |

The `path-plus` and `distinct-obj` flags pass on added time alone, and `distinct-obj`
passes at 0.499 µs, at the limit of the rule. Its cost against f854145b is real, since all
24 rounds are slower, and it is of the same kind as item 856's. Against 321508fe, the main
branch before item 825, the four queries of item 856 are 0.26 to 0.35 µs slower with
nearly every round agreeing, which confirms the item's measurement at the batch 30 build.
Cross-checks in the same runs put `path-plus` 0.80 µs and `star-lookup` 2.5 µs slower
than 321508fe, 1.085 and 1.043 of its time.

`minus` takes one of two speeds for the life of each server process, about 0.25 ms or about
0.40 ms, in both builds of every pair. The two speeds match its cursor and eager execution
times in the [forge matrix](#forge-record-at-batch-30-against-cc94ac7f), so this is the
history of quick texts settling on one mode per process, as
[the execution mode of quick texts](#execution-mode-of-quick-texts-in-fresh-servers)
describes, and the pooled median depends on how many processes settle eagerly. Within each
mode, 8baa5686 runs `minus` at 0.2537 ms against 0.2530 ms for adbe757c and at 0.4022 ms
against 0.4048 ms, so the flags of batches 21 and 22 are not costs. On 321508fe the slow
mode takes 0.302 ms and on 06ca04ef, the merge of item 825, 0.402 ms, each in its own pair
against 8baa5686. The query-path changes between those two commits are item 825's, and the
rest are reasoner changes, so the eager mode of `minus` most likely became about 0.10 ms
slower with item 825. The matrix measures that cost in every cell.

The [run record](runs/2026-11-18-post-push-pinned-forge-8baa5686/validation.json) holds
the binary hashes, the settings, the per-mode medians of `minus`, the analysis script and
compressed request samples. These are internal comparisons and make no cross-engine claim.

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
streamed response's fixed cost, which the merged executor must not inherit. With item
240's buffered writers, separate atlas measurements at 1.05M triples with 12 threads show
streamed responses faster than eager ones on these four queries, as the
[engine results](../engine/RESULTS.md) record.

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
change, and item 696 follows the search loop whose alignment moved it. With 12
threads at 1.05M triples, the fixed build
ran the 30 queries at a geometric mean of 0.959 of 62d9cb53, the build that merged stage 4,
with eager `order-by-full` at 0.643, 8.8 ms against 13.7 ms, and every other query within
1.02 except `distinct-join` at 1.036, which item 696 follows. At 10.5M triples the
eager costs of 2% to 4% that item 682 holds remained, with `subquery-agg` at 1.040 of
005df56d, `lang-filter` at 1.026, `regex-iri` at 1.021 and `expr-bind-group` at 1.020.

The batch that followed, 62d9cb53 to 125edd41, changed no executor code. With one thread at
1.05M triples it ran `count-all` at 0.966 of 62d9cb53, `distinct-obj` at 0.996,
`predicate-counts` at 0.958, `types-grouped` at 0.964 and `path-plus` at 1.020, 0.21 µs
more, so it added no cost to the short queries that the Namespace screen flagged.

### Grouping operator

The grouping operator of item 826 was compared with its base, 7e272f4d, on forge alone in
an interleaved A/B of release builds made on wally with the same compilers, at 1.05M
triples with one thread, in twelve rounds of A,B,B,A with 200 timed requests per query and
server. The [run record](runs/2026-11-17-grouping-operator-ab-forge-d1f13592/eager/summary.md)
keeps the samples and answers of both cells and the eager plans of both builds with their
[commands](runs/2026-11-17-grouping-operator-ab-forge-d1f13592/run.sh.txt). Both builds
returned the same answers to all six queries in both modes, and steal was zero.

Eager `expr-bind-group` read 0.839 of the base, 0.50 ms against 0.60 ms, `expr-agg-arg`
0.888 and `group-avg` 0.906, with every round within 0.02 of those ratios. Eager
`expr-agg-arg` now keeps a running sum per group instead of collecting each group's rows,
and EXPLAIN marks it `[incremental]`. `group-avg` and `expr-bind-group` kept running state
in the base as well. Streamed, the same queries read 0.975, 1.004 and 0.980. `types-grouped`,
`predicate-counts` and `subquery-agg` keep their count plans and read 0.955 to 1.021 in
both modes, within the spread of the base. Eager `subquery-agg` read 1.009 and streamed
`predicate-counts` read 1.021. Eager `types-grouped` read 1.003, a median of 0.02 µs more
in this order of the A/B, under the 0.5 µs bound of the Intel rule. The
12-thread cells of these queries come from the forge matrix. Roadmap item 1067 follows
these small deltas.

An eager aggregate whose argument compares a variable with a constant IRI or holds an
EXISTS decoded that variable's column before it ran, and item 1098 removed that decode.
Its fix, 59d29b40, was compared on forge with main, bccc5f7b, and with 92371f0f, the main
before the grouping operator, at 10.5M triples with one thread. Each pair ran three
rounds of A,B,B,A with 42 timed requests per arm and mode, from builds made on forge with
the same compilers. The [run record](runs/ce40bfef42a7a7384020a8f4a5b3a4c9b194efe0db6878493e2f3de9ffabe4ec/README.md)
keeps the samples, answers, queries and scripts, and every query returned the same answer
in every server.

| Query | Main | Fix | Fix/main | Before 826 | Fix/before |
|---|--:|--:|--:|--:|--:|
| Eager `SUM(IF(?p = ex:nobody, 1, 0))` | 60.06 ms | 15.05 ms | 0.251 | 16.10 ms | 0.942 |
| Eager BIND of the same `IF` and `SUM` | 60.94 ms | 15.21 ms | 0.250 | 59.97 ms | 0.253 |
| Eager `SUM(IF(EXISTS { ?p ex:authorOf ?d }, 1, 0))` | 917.92 ms | 866.92 ms | 0.944 | 870.70 ms | 0.998 |

The fix column comes from the pair with main. The ratios against the build before item
826 divide the other pair's fix medians, 15.16, 15.20 and 868.90 ms. Every round read
within 0.02 of these ratios except those of the first query against the
build before item 826, which read 0.988, 0.701 and 0.981. The fix leaves the queries that
read values within 0.99 to 1.01 of main, such as `SUM(STRLEN(?n))`, `SUM(?s * 2)` and
`expr-agg-arg`, and every streamed query above 0.3 ms within 0.98 to 1.01 of main. Streamed `sum-if` and the streamed
BIND form take about 13.5 ms in most server processes and 17 to 22 ms in some, in all
three builds, so the second pair's streamed ratios of 1.35 and 1.32 are noise that a round
below 1.0 shows. Against the build before item 826, eager `SUM(STRLEN(?n))` grouped by
employer reads 0.784, and eager `SUM(?s * 2)` reads 1.163, 57.9 ms against 49.7 ms, which
roadmap item 1099 follows. With one thread at 10.5M triples, the same pair reads 1.039 for
streamed `SUM(STRLEN(STR(?o)))`, 1.074 for streamed `expr-agg-arg`, 1.013 for streamed
`SUM(?s * 2)` and 1.008 for streamed `SUM(STRLEN(?n))`. The fix is slower in all three
rounds for each query. Item 1067 holds these costs and the 12-thread `group-avg` follow-up
from the [pinned forge comparison](#pinned-checks-and-drift-at-batch-37).

### Forge record at batch 30 against cc94ac7f

The matrix ran on forge alone for cc94ac7f, the main branch before item 825 and the fixed
baseline that the [X14 acceptance gates](../../specs/X14-unified-executor/SPEC.md#acceptance-gates)
compare every later main branch with, and then for 8baa5686, the main branch at batch 30.
Both runs used the scripts of 8baa5686, the same data and release builds with the same
compilers and build configuration, GCC 15.3.0, clang 23.1.0git and rustc 1.99.1, made on
the laptop and copied to forge. The run records of
[cc94ac7f](runs/2026-11-18-executor-matrix-forge-cc94ac7f/summary.md) and
[8baa5686](runs/2026-11-18-executor-matrix-forge-8baa5686/summary.md) hold the records,
summaries and commands. Both modes returned the same answers in every cell of both runs,
and no answer fingerprint changed.

Against cc94ac7f, `bench-gate.py compare` flagged 67 of 248 execution times
([execution times](runs/2026-11-18-executor-matrix-forge-8baa5686/compare-cc94ac7f-exec.md),
[wall times](runs/2026-11-18-executor-matrix-forge-8baa5686/compare-cc94ac7f-wall.md)).
Forty-three of them are queries under 0.3 ms, which the matrix's ten timed requests per
server cannot resolve. The flags above 0.3 ms are the drift of batches 17 to 30 from the
baseline.

| query | mode | cells | ratio to cc94ac7f | cc94ac7f ms | 8baa5686 ms |
|---|---|---|--:|--:|--:|
| `minus` | eager | all four | 1.30 to 1.42 | 0.31 to 3.30 | 0.41 to 4.30 |
| `lang-filter` | both | one thread | 1.12 to 1.13 | 1.64 and 16.0 | 1.83 and 18.0 |
| `lang-filter` | either | 12 threads | 1.07 to 1.11 | 0.73 and 2.91 | 0.81 and 3.12 |
| `regex-iri` | both | one thread | 1.09 to 1.11 | 3.48 and 34.5 | 3.80 and 38.2 |
| `optional-chain` | streaming | three of four | 1.06 to 1.12 | 1.95 to 22.5 | 2.06 to 25.3 |
| `optional-count` | streaming | three of four | 1.05 to 1.06 | 0.52 to 5.18 | 0.54 to 5.45 |
| `distinct-join` | both | 1.05M, 12 threads | 1.05 and 1.07 | 3.58 and 3.77 | 3.76 and 4.02 |

Every server process of a cell agrees on `minus`, and with one thread on `regex-iri`,
`lang-filter` and streamed `optional-chain`. With 12 threads the process medians of the two
builds overlap for `lang-filter` and streamed `optional-chain`. Eager `minus` took 0.31 ms in each process
of cc94ac7f at 1.05M triples with one thread and 0.41 ms in each process of 8baa5686, and
the pinned checks place this cost between 321508fe and
06ca04ef, the range of item 825. The same cells are faster at 10.5M triples
with 12 threads, where `order-by-full` takes 0.56 to 0.58 of its time in both modes and
eager `optional-chain` 0.56, and four of the eight cells and modes of `export-500k` are 7% to 15% faster in execution.
Eager `optional-chain` at that size varied from 38.6 to 47.6 ms between the processes of
cc94ac7f. The e52937ca record took 28.5 ms there, but its compilers differ, so that ratio is
not a measurement of the code. In wall time, streamed `export-500k` is 5% to 6% slower in all four cells and
`lang-filter` and `regex-iri` 10% to 12% slower at 10.5M triples with one thread.

### Forge record at batch 33 against cc94ac7f

The matrix ran on forge alone for bccc5f7b, the main branch after batches 32 and 33, and
then for eda22fce, the main branch before batch 32. Both runs used the scripts and data of
the batch-30 runs and release builds made on the laptop whose build configuration and
compiler notes equal those of the cc94ac7f build. The engine of eda22fce differs from that
of 8baa5686 only in the OWL classifier, which no synthetic query runs, so the ratios of
eda22fce to 8baa5686 show how far two forge runs of the same query code differ on
different days. The run records of
[bccc5f7b](runs/2026-11-19-executor-matrix-forge-bccc5f7b/summary.md) and
[eda22fce](runs/2026-11-19-executor-matrix-forge-eda22fce/summary.md) hold the records,
summaries, commands and compare outputs. Both modes returned the same answers in every
cell of both runs, and every answer fingerprint equals that of cc94ac7f. These are
separate runs, not interleaved pairs, so their ratios locate candidates and do not measure
costs.

Between the two runs of the same query code, every one-thread execution time above 0.3 ms
read 0.98 to 1.02, apart from eager `export-500k`, whose 2 ms of execution inside a 230 ms
request read 0.94 and 1.10. With 12 threads, eight of the 85 cell times above 0.3 ms read
1.05 or more, up to 1.17 for streamed `order-by-full` at 10.5M triples. The streamed wall
times of `export-500k`, `optional-chain` and `order-by-full` in the eda22fce run were 2% to
6% lower than in the 8baa5686 run in every cell.

Against eda22fce, bccc5f7b ran several eager grouping queries faster with one thread.
Eager `expr-bind-group` read 0.84 and 0.85, eager `optional-count` 0.86, `group-avg` 0.89
and 0.93, `expr-agg-arg` 0.89 and 0.90 and eager `minus` 0.89. The table lists every time
above 0.3 ms that bccc5f7b ran at least 2% slower than both runs of the earlier code, with
each of its four server processes slower than every process of those two runs.

| query | mode | cell | 8baa5686 | eda22fce | bccc5f7b | ratio to eda22fce |
|---|---|---|--:|--:|--:|--:|
| `order-by-full` | eager | 10.5M, 12 threads | 108.7 ms | 121.7 ms | 146.4 ms | 1.203 |
| `order-by-full` | eager | 1.05M, 12 threads | 8.77 ms | 8.75 ms | 9.42 ms | 1.076 |
| `class-predicates` | streaming | 1.05M, 12 threads | 1.020 ms | 1.006 ms | 1.052 ms | 1.046 |
| `group-avg` | streaming | 10.5M, 12 threads | 37.72 ms | 37.88 ms | 39.08 ms | 1.032 |
| `group-avg` | streaming | 10.5M, one thread | 37.23 ms | 36.93 ms | 37.98 ms | 1.029 |
| `export-500k` | streaming | 10.5M, one thread | 0.400 ms | 0.404 ms | 0.418 ms | 1.035 |
| `export-500k` | streaming | 1.05M, one thread | 0.395 ms | 0.393 ms | 0.403 ms | 1.024 |
| `not-exists` | streaming | 1.05M, 12 threads | 0.793 ms | 0.797 ms | 0.813 ms | 1.021 |
| `expr-order-key` | streaming | 10.5M, 12 threads | 10.96 ms | 11.04 ms | 11.27 ms | 1.021 |

In wall time the same test selects streamed `order-by-full` at 10.5M triples with 12
threads, 1.151 of eda22fce, eager `order-by-full` there, 1.063, streamed `group-avg` there,
1.035, and streamed `optional-chain` there, 1.072. Streamed `export-500k` read 1.049 to
1.085 of eda22fce in wall time in all four cells and 1.017 to 1.021 of 8baa5686, and in
three of the four cells every server process of bccc5f7b was slower than every process of
both runs. Row 1063 puts the cost
of item 1039, which batch 32 merged, at about 3% of streamed `export-500k` execution time
with wall time unchanged, and row 1067 holds small streamed grouping costs. An interleaved
forge A/B of eda22fce and bccc5f7b decides which of these differences are costs of the
code.

Against cc94ac7f, `bench-gate.py compare` flagged 75 of 248 execution times
([execution times](runs/2026-11-19-executor-matrix-forge-bccc5f7b/compare-cc94ac7f-exec.md),
[wall times](runs/2026-11-19-executor-matrix-forge-bccc5f7b/compare-cc94ac7f-wall.md)), 51
of them under 0.3 ms. The table lists all 24 flags above 0.3 ms.

| query | mode | cells | ratio to cc94ac7f | cc94ac7f ms | bccc5f7b ms |
|---|---|---|--:|--:|--:|
| `minus` | eager | all four | 1.16 to 1.28 | 0.31 to 3.30 | 0.37 to 3.91 |
| `lang-filter` | both | one thread | 1.09 to 1.10 | 1.64 and 16.0 | 1.79 and 17.6 |
| `lang-filter` | streaming | 1.05M, 12 threads | 1.12 | 0.73 | 0.82 |
| `regex-iri` | both | one thread | 1.08 to 1.10 | 3.48 and 34.5 | 3.75 and 37.8 |
| `regex-iri` | streaming | 1.05M, 12 threads | 1.07 | 0.96 | 1.03 |
| `optional-chain` | streaming | three of four | 1.06 to 1.12 | 1.95 to 25.5 | 2.06 to 27.6 |
| `optional-count` | streaming | three of four | 1.06 to 1.08 | 0.56 to 5.18 | 0.60 to 5.52 |
| `class-predicates` | streaming | 1.05M, 12 threads | 1.07 | 0.99 | 1.05 |
| `class-predicate-list` | streaming | 1.05M, 12 threads | 1.05 | 0.98 | 1.03 |
| `order-by-full` | eager | 1.05M, 12 threads | 1.06 | 8.92 | 9.42 |
| `export-500k` | eager | 10.5M, one thread | 1.08 | 2.21 | 2.39 |

Eager `minus` read 1.30 to 1.42 of cc94ac7f in the batch-30 record and 1.16 to 1.28 here.
With one thread, every server process of a cell agrees on `lang-filter`, `regex-iri` and
streamed `optional-chain`, and their ratios to cc94ac7f are within 0.04 of those in the
[batch-30 record](#forge-record-at-batch-30-against-cc94ac7f). Row 1072 records
`lang-filter` at about 2.5% slower after item 678, and no row records a cost on
`regex-iri`, streamed `optional-chain` or streamed `optional-count`, so the drift of these
four queries from cc94ac7f is larger than the recorded costs that
[row 1074](../../ROADMAP.md) counts. An interleaved forge A/B of cc94ac7f and the current
main decides how much of that drift is a cost of the code before the range is bisected. In
wall time, streamed `export-500k` is 7% to 9% slower than cc94ac7f in all four cells, and
`lang-filter` and `regex-iri` 9% slower at 10.5M triples with one thread.

### Pinned checks and drift at batch 37

Forge ran eleven interleaved pairs alone under its benchmark and build locks, in rounds of
A,B,B,A, with release builds of the matrix recipe that the laptop made with the compilers
and build configuration of the cc94ac7f build. The
[run record](runs/2026-11-19-post-push-pinned-forge-stage3/README.md) holds the
scripts, samples, answer checksums, binary hashes and verdicts. For every pair, the sorted
TSV answers of both builds have the same checksums, and the timed responses were not
compared. A query whose arm medians are both under 0.3 ms is judged by the D4 rule of the
pinned checks above. Otherwise a ratio of 1.05 or more fails, and a lower ratio that is
slower in every round is a confirmed cost that needs a roadmap row. The record's
`verdict.txt` applies this rule to the output of `analyze.py`, whose own verdict field
tests only the ratio above 0.3 ms.

The screen flags of batches 33 and 35 pass. Against eda22fce, the main before batch 32,
bccc5f7b runs `types-grouped` at 0.984 eager and 0.969 streamed. Under automatic execution
it reads 1.030 with an interval that reaches 1.037, and it passes because the added time
is 0.22 µs. Against ffb18866, the main before batch 35, `subquery-agg` reads 0.997 under
automatic execution and 1.001 streamed, with intervals that reach 1.0.

The pairs of eda22fce and bccc5f7b rerun the candidates above 0.3 ms that the
[batch-33 record](#forge-record-at-batch-33-against-cc94ac7f) listed, apart from streamed
`group-avg` and `export-500k` at 10.5M triples with one thread, which have no pair. Every
ratio is below 1.05, and two are confirmed costs.

| query | mode | cell | ratio | rounds slower |
|---|---|---|--:|--:|
| `export-500k` | streaming | 1.05M, one thread | 1.032 | 12 of 12 |
| `group-avg` | streaming | 10.5M, 12 threads | 1.022 | 8 of 8 |
| `expr-order-key` | streaming | 10.5M, 12 threads | 1.022 | 7 of 8 |
| `order-by-full` | streaming | 10.5M, 12 threads | 1.009 | 7 of 8 |
| `order-by-full` | eager | 10.5M, 12 threads | 1.001 | 4 of 8 |
| `order-by-full` | eager | 1.05M, 12 threads | 1.003 | 6 of 12 |
| `class-predicates` | streaming | 1.05M, 12 threads | 1.006 | 9 of 12 |

Row 1063 holds the `export-500k` cost and row 1067 the `group-avg` cost. The pairs
span batches 32 and 33. Batch 32 merged items 826, 678 and 1039, which are
starting points for investigating `group-avg`. Streamed
`expr-order-key` has an interval from 1.010 to 1.031 but is faster in one of eight rounds,
which the rule above counts as noise. Eager `order-by-full` at 10.5M triples, which the
separate matrix runs read at 1.20, reads 1.001 here with an interval from 0.994 to 1.007.

The pair of cc94ac7f and 71c277b0, the main at batch 37, measures the drift of streamed
queries since the fixed baseline with one thread at 1.05M triples.

| query | cc94ac7f ms | 71c277b0 ms | ratio | 95% interval | rounds slower | row |
|---|--:|--:|--:|---|--:|---|
| `optional-chain` | 1.940 | 2.096 | 1.081 | 1.076 to 1.085 | 12 of 12 | 1202 |
| `optional-count` | 0.517 | 0.545 | 1.056 | 1.052 to 1.059 | 12 of 12 | 1203 |
| `regex-iri` | 3.476 | 3.593 | 1.034 | 1.031 to 1.038 | 12 of 12 | 1204 |
| `lang-filter` | 1.632 | 1.679 | 1.029 | 1.027 to 1.031 | 12 of 12 | 1204 |
| `export-500k` | 0.417 | 0.398 | 0.956 | 0.952 to 0.959 | 0 of 12 | none |

Each ratio is the whole drift since cc94ac7f. The
[separate timing of item 678](../../specs/X14-unified-executor/SPEC.md#charged-query-state-and-chunked-live-reservations)
compares `lang-filter` against that item's own base. The two comparisons leave the share of
the 1.029 endpoint ratio attributable to item 678 unresolved. The separate matrix runs of 8baa5686 and bccc5f7b read
`lang-filter` and `regex-iri` at 1.08 to 1.12 of the cc94ac7f run in the same cells, against
1.03 here. Their instruction counts are nearly flat from 8baa5686 to 71c277b0, so the gap
between the two methods is not explained. The 10.5M and 12-thread cells of these four
queries have no interleaved pair. In wall time this pair reads 0.650 for `optional-chain`
and 0.543 for `export-500k`, over every change from cc94ac7f to 71c277b0. The atlas
comparison of 8baa5686 and a016b7bd in the
[streamed response section](../../specs/X14-unified-executor/SPEC.md#the-streamed-response-cost)
measures item 240's buffered cursor writer on its own and does not divide this difference.
Server execution time covers the cursor's batches and leaves out the writer and its term
cache.

The [attribution files](runs/2026-11-19-post-push-pinned-forge-stage3/attribution/README.md)
count instructions per request on the laptop, streamed with one Rayon thread at 1.05M
triples. They compare consecutive matrix builds from cc94ac7f to 71c277b0 and, between
cc94ac7f and 8baa5686, cached screen builds of another toolchain. Counts locate a change in
the code, and only the forge pair sizes a cost.

- On `optional-count`, the screen builds 321508fe and 06ca04ef differ by 4.4%, and no
  other step adds more than 0.3%. The range holds item 825 and the Horn classifier
  with individuals, which no synthetic query runs.
- On `optional-chain`, the same range adds 0.5% while `export-500k` stays flat. The matrix
  step from ffb18866 to 1cbc23cd adds another 0.55% to `optional-chain` and 0.47% to
  `export-500k`, and from cc94ac7f to 8baa5686 the whole request runs 0.16% fewer
  instructions. Most of this request is the writer, so these totals do not show where the
  query's own instructions changed.
- On `lang-filter` and `regex-iri`, adbe757c to 65c570db, which is batch 21, adds 1.8% and
  1.6%, and no other step adds more than 0.06%. Item 948 changed the checked vocabulary
  decode in that batch and is a candidate that its own comparison would confirm.

[Flat profiles](runs/2026-11-19-post-push-pinned-forge-stage3/attribution/perf/) of
cc94ac7f and 71c277b0 on the laptop's performance cores, sampled on user-space instructions,
name the code without isolating the execution window. In cc94ac7f the cursor joined
`optional-chain` through its own hash index in `cursor::binary`. In 71c277b0 it joins
through the `join::hash` kernels that item 825 added, which build a partitioned index. The
join symbols, counting the merge bounds and zipper on both sides, take an estimated 8.9
million instructions per request against 6.9 million. The count of `optional-count` moved
from the cursor's grouping count over `zipper::lower_bounds` to `join::merge::count` and
`join::merge::bounds`, and those symbols with the zipper take an estimated 4.1 million
instructions per request against 3.8 million. The merge keeps both kernel families, so
both costs are on the kept path. The filter profiles do not locate the added instructions,
so row 1204 is on the kept path by default. The laptop carried other work, so the cycle
profiles are not measurements.

### Execution mode of quick texts in fresh servers

The Namespace screen of cf033f12, the batch that merged items 679 and 454, against 125edd41
flagged `expr-bind-group` at 1.465 in server execution time with identical answers. Its
plan did not change. Each server process answered it either as a cursor in about 0.41 ms
of execution or eagerly in about 0.65 ms for its whole life, and more processes of the
batch answered it eagerly. The server's history of quick texts ran a text's first run as
an automatic cursor and its second eagerly, and kept whichever of those two single runs was
faster. The first cursor run is cold, and with item 454 it took about 0.2 ms longer, so it
lost to the eager run in most fresh servers. The history now gives the slower mode a second
run before a quick text settles, as the
[X06 outcome](../../specs/X06-benchmark-coverage.md#query-threads-on-hybrid-cores)
describes. The [run record](runs/2026-11-03-mode-history-atlas-6b8f86e6/) holds the
samples, the answers, the scripts and the screen.

All runs used one Rayon thread with the server on CPU 2, the client on CPU 4 and spinners
on both, at 1.05M triples. The atlas runs went through `scripts/nsc-bench-remote.sh` on
atlas, while an unpinned single-threaded job of another lane ran there. The wally builds
used `scripts/nsc-build.sh`, and the server of cf033f12 is that of bf740bed, the merge of
item 454, since cf033f12 changed only a Kotlin test.

| build | commit | built on | sha256 |
|---|---|---|---|
| main, the screen's own binary | 125edd41 | the laptop | 9403375b8dbb728f39b876e085142a6ba289833022e1b5b8435dfb8331093bf1 |
| the batch, the screen's own binary | cf033f12 | the laptop | ce66ebae0524d84c18f3bd02c737cfce5522a102913a746a71580e189d174225 |
| main | 125edd41 | wally | 9af0131fa4215f251b392a561011fae8829dce2140b9ee37495411a3ce1dfba6 |
| the merge of item 679 | 40ca3535 | wally | faeb8132307af95ab96b4ad3be52553e9bb276bb391d172dfc1f5d76e1f6e538 |
| the batch | cf033f12 | wally | 91eff3a9d9f4c862f588ee6a1353aae55206b1765ca90257249ad97f4a7fbcb7 |
| the fix | 6b8f86e6 | wally | dde7a5a390d025dae553cee72258dd50052ccda14ea8d6e316150dda00c07768 |

Five fresh servers of each wally build each answered six requests of `expr-bind-group`
without a warm-up, in
[first-runs.txt](runs/2026-11-03-mode-history-atlas-6b8f86e6/first-runs.txt). A sequence
lists each request's mode, `C` for a cursor and `E` for eager.

| build | first cursor run, median total ms | mode sequences |
|---|--:|---|
| 125edd41 | 0.98 | `CECCCC` four times and `CEEEEE` once |
| 40ca3535 | 1.01 | `CECCCC` four times and `CEEEEE` once |
| cf033f12 | 1.19 | `CEEEEE` four times and `CECCCC` once |
| 6b8f86e6 | 1.20 | `CECECC` five times |

With `SPARKLES_DISABLE_OPTIMIZATIONS` set to the 454 switches `set_remainders`,
`node_sets`, `key_backoff` and `characteristic_sets`, the first cursor run took a median
of 1.18 ms on cf033f12 and 0.98 ms on 40ca3535 over eight fresh servers each, in
[switches-off.txt](runs/2026-11-03-mode-history-atlas-6b8f86e6/switches-off.txt). With
each build serving a copy of the other's store, 125edd41 over the store of cf033f12, which
holds the `correlations` member, took 0.98 ms and cf033f12 over the store of 125edd41,
which lacks it, took 1.02 ms, against 0.98 and 1.18 ms over their own stores, in
[cross-stores.txt](runs/2026-11-03-mode-history-atlas-6b8f86e6/cross-stores.txt). The
stores' index files are identical, and only `stats.json`, `meta.json` and `commit.json`
differ. The cost therefore needs both the code of item 454 and its statistics in the
store. With perf attached to a ready server, which changes the timing itself, the first
request ran 11.75 million user-space instructions on 125edd41 and 11.72 to 11.79 million on
cf033f12 in three servers each, with no more user-space cycles, as the files under
[perf](runs/2026-11-03-mode-history-atlas-6b8f86e6/perf/rec1.sh.txt) show. Item 708 looks
for that cold cost.

The interleaved A/Bs ran 8 rounds of A,B,B,A with 3 warm-ups and 40 timed requests per
query and server process, once with the candidate second and once with it first, and give
the candidate's execution time over the base's as the square root of the two runs' median
per-round ratios, with a 95% bootstrap interval. This is the rule of
`scripts/x08-gates/combine.py` with a minimum of 8 rounds. A process counts as eager when
its median execution time of `expr-bind-group` passed 0.5 ms. Every run's servers returned
the same answer fingerprints.

| base | candidate | query | base ms | ratio | 95% interval | eager processes of the base | eager processes of the candidate |
|---|---|---|--:|--:|---|--:|--:|
| 125edd41, laptop | cf033f12, laptop | `expr-bind-group` | 0.638 | 1.173 | 0.982 to 1.363 | 13 of 32 | 24 of 32 |
| 125edd41, laptop | cf033f12, laptop | `expr-order-key` | 1.331 | 1.004 | 0.975 to 1.025 | | |
| 125edd41, laptop | cf033f12, laptop | `group-avg` | 3.664 | 0.975 | 0.913 to 1.060 | | |
| 125edd41, wally | 6b8f86e6, wally | `expr-bind-group` | 0.652 | 0.620 | 0.618 to 0.664 | 29 of 32 | 0 of 32 |
| 125edd41, wally | 6b8f86e6, wally | `expr-order-key` | 1.241 | 0.988 | 0.984 to 0.994 | | |
| 125edd41, wally | 6b8f86e6, wally | `group-avg` | 3.466 | 1.002 | 0.922 to 1.004 | | |

The share of eager processes of one commit depends on the build and the host, as the two
builds of 125edd41 show, because its cold cursor run and its first eager run took nearly the
same time. On Namespace, the wally builds of 125edd41 and 6b8f86e6 ran in 4 rounds of 20
timed requests over all 30 queries. The fix ran `expr-bind-group` at 0.852 of 125edd41 and
the 30 queries at a geometric mean of 0.956. The first pass flagged `not-exists` at 1.119
and `star-lookup` at 1.232, and the rerun in 8 rounds of 40 read them at 1.010 and 0.985.
