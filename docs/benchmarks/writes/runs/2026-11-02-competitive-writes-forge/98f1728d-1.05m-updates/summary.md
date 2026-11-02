| query | sparkles (ms) | oxigraph (ms) |
|---|---:|---:|
| count-all | — | — |
| types-grouped | — | — |
| star-join | **8.1 ± 0.3** | 2883.4 ± 20.2 |
| two-hop-count | **6.4 ± 0.5** | 6008.1 ± 13.0 |
| range-topk | — | — |
| optional-count | — | — |
| contains | **4.3 ± 0.2** | 154.8 ± 6.7 |
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
| regex-iri | **4.5 ± 0.1** | 376.2 ± 4.2 |
| knows-reach | — | — |
| distinct-join | — | — |
| lang-filter | — | — |
| star-lookup | — | — |
| values-star | — | — |
| employee-docs | — | — |
| expr-bind-group | — | — |
| expr-order-key | — | — |
| expr-agg-arg | — | — |
| **update** (1-triple INSERT DATA) | 3.9 ± 0.1 | **3.5 ± 0.0** |
| **throughput** star-join, 16 clients (queries/s) | **1120** | 3.1 |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles | oxigraph |
|---|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 142 | **22** |
| server RSS after every query ran once (MiB) | 264 | **74** |
| server RSS after the run (MiB) | **323** | 719 |
| peak RSS during the throughput run (MiB) | **346** | 728 |
| server RSS after the run less Sparkles' decoded-block cache of 85 MiB (MiB) | 238 | — |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles | oxigraph |
|---|---:|---:|
| star-join | **+37** / 240 | +45 / 69 |
| two-hop-count | +36 / 276 | **+1** / 70 |
| contains | +6 / 282 | **+4** / 74 |
| regex-iri | +1 / 283 | **+0** / 74 |

Churn before the queries: 5000 single-triple commits, one request each and the same for every engine. They are INSERT DATA and DELETE DATA in the default graph.

| commits | sparkles | oxigraph |
|---|---:|---:|
| commits/s | **935** | 744 |
| latency p50 (ms) | **1.15** | 1.35 |
| latency p99 (ms) | **1.76** | 2.09 |
| failed commits | 0 | 0 |
