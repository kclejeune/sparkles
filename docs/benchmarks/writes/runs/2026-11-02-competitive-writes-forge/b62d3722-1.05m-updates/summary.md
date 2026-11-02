| query | sparkles (ms) | oxigraph (ms) |
|---|---:|---:|
| count-all | — | — |
| types-grouped | — | — |
| star-join | **8.1 ± 0.3** | 2879.7 ± 19.4 |
| two-hop-count | **8.2 ± 0.2** | 5995.8 ± 22.5 |
| range-topk | — | — |
| optional-count | — | — |
| contains | **5.0 ± 0.2** | 151.8 ± 6.0 |
| group-avg | — | — |
| path-plus | — | — |
| distinct-obj | — | — |
| export-500k | — | — |
| predicate-counts | — | — |
| predicate-subjects | — | — |
| class-predicates | — | — |
| class-predicate-list | — | — |
| order-by-full | — | — |
| optional-chain | — | — |
| minus | — | — |
| not-exists | — | — |
| exists-join | — | — |
| subquery-agg | — | — |
| regex-iri | **5.2 ± 0.2** | 297.2 ± 5.7 |
| knows-reach | — | — |
| distinct-join | — | — |
| lang-filter | — | — |
| star-lookup | — | — |
| values-star | — | — |
| employee-docs | — | — |
| expr-bind-group | — | — |
| expr-order-key | — | — |
| expr-agg-arg | — | — |
| **update** (1-triple INSERT DATA) | 3.7 ± 0.1 | **3.5 ± 0.1** |
| **throughput** star-join, 16 clients (queries/s) | **1039** | 3.1 |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles | oxigraph |
|---|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 140 | **22** |
| server RSS after every query ran once (MiB) | 287 | **74** |
| server RSS after the run (MiB) | **467** | 703 |
| peak RSS during the throughput run (MiB) | **499** | 725 |
| server RSS after the run less Sparkles' decoded-block cache of 85 MiB (MiB) | 382 | — |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles | oxigraph |
|---|---:|---:|
| star-join | **+35** / 252 | +45 / 69 |
| two-hop-count | +25 / 277 | **+1** / 70 |
| contains | +22 / 299 | **+4** / 74 |
| regex-iri | +6 / 305 | **+0** / 74 |

Churn before the queries: 5000 single-triple commits, one request each and the same for every engine. They are INSERT DATA and DELETE DATA in the default graph.

| commits | sparkles | oxigraph |
|---|---:|---:|
| commits/s | 778 | **809** |
| latency p50 (ms) | 1.29 | **1.28** |
| latency p99 (ms) | **1.79** | 1.81 |
| failed commits | 0 | 0 |
