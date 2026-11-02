| query | sparkles (ms) |
|---|---:|
| count-all | — |
| types-grouped | — |
| star-join | **42.6 ± 1.4** |
| two-hop-count | **31.1 ± 26.4** |
| range-topk | — |
| optional-count | — |
| contains | **8.1 ± 0.1** |
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
| regex-iri | **9.2 ± 0.2** |
| knows-reach | — |
| distinct-join | — |
| lang-filter | — |
| star-lookup | — |
| values-star | — |
| employee-docs | — |
| expr-bind-group | — |
| expr-order-key | — |
| expr-agg-arg | — |
| **update** (1-triple INSERT DATA) | **3.9 ± 0.1** |
| **throughput** star-join, 16 clients (queries/s) | **181** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 142 |
| server RSS after every query ran once (MiB) | 937 |
| server RSS after the run (MiB) | 1061 |
| peak RSS during the throughput run (MiB) | 1340 |
| server RSS after the run less Sparkles' decoded-block cache of 724 MiB (MiB) | 337 |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles |
|---|---:|
| star-join | +114 / 721 |
| two-hop-count | +229 / 950 |
| contains | +35 / 985 |
| regex-iri | +0 / 982 |

Churn before the queries: 5000 single-triple commits, one request each and the same for every engine. They are INSERT DATA and DELETE DATA in the default graph.

| commits | sparkles |
|---|---:|
| commits/s | 684 |
| latency p50 (ms) | 1.36 |
| latency p99 (ms) | 3.19 |
| failed commits | 0 |
