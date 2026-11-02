| query | sparkles (ms) |
|---|---:|
| **load** (s) | **4.53** |
| count-all | — |
| types-grouped | — |
| star-join | **32.1 ± 1.0** |
| two-hop-count | — |
| range-topk | — |
| optional-count | — |
| contains | — |
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
| regex-iri | — |
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
| **throughput** star-join, 16 clients (queries/s) | **202** |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles |
|---|---:|
| server RSS 2 s after start, before any query (MiB) | 143 |
| server RSS after every query ran once (MiB) | 276 |
| server RSS after the run (MiB) | 388 |
| peak RSS during the throughput run (MiB) | 705 |
| server RSS after the run less Sparkles' decoded-block cache of 107 MiB (MiB) | 281 |
| load peak RSS (MiB) | 1868 |
| index size on disk | 298 MiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles |
|---|---:|
| star-join | +137 / 281 |
