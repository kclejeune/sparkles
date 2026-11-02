| query | sparkles (ms) |
|---|---:|
| count-all | — |
| types-grouped | — |
| star-join | **42.0 ± 2.1** |
| two-hop-count | **35.8 ± 0.1** |
| range-topk | — |
| optional-count | — |
| contains | **15.1 ± 2.6** |
| group-avg | — |
| path-plus | — |
| distinct-obj | — |
| export-500k | — |
| predicate-counts | — |
| predicate-subjects | — |
| class-predicates | — |
| class-predicate-list | — |
| order-by-full | — |
| optional-chain | — |
| minus | — |
| not-exists | — |
| exists-join | — |
| subquery-agg | — |
| regex-iri | **16.9 ± 0.4** |
| knows-reach | — |
| distinct-join | — |
| lang-filter | — |
| star-lookup | — |
| values-star | — |
| employee-docs | — |
| expr-bind-group | — |
| expr-order-key | — |
| expr-agg-arg | — |
| **update** (1-triple INSERT DATA) | **3.8 ± 0.1** |
| **throughput** star-join, 16 clients (queries/s) | **178** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 143 |
| server RSS after every query ran once (MiB) | 926 |
| server RSS after the run (MiB) | 1056 |
| peak RSS during the throughput run (MiB) | 1228 |
| server RSS after the run less Sparkles' decoded-block cache of 722 MiB (MiB) | 334 |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles |
|---|---:|
| star-join | +115 / 723 |
| two-hop-count | +200 / 923 |
| contains | +24 / 947 |
| regex-iri | +14 / 961 |

Churn before the queries: 5000 single-triple commits, one request each and the same for every engine. They are INSERT DATA and DELETE DATA in the default graph.

| commits | sparkles |
|---|---:|
| commits/s | 681 |
| latency p50 (ms) | 1.32 |
| latency p99 (ms) | 3.30 |
| failed commits | 0 |
