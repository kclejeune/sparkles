| query | sparkles (ms) | oxigraph (ms) |
|---|---:|---:|
| **load** (s) | **0.58** | 4.20 |
| count-all | — | — |
| types-grouped | — | — |
| star-join | **7.2 ± 0.3** | 386.9 ± 4.5 |
| two-hop-count | — | — |
| range-topk | — | — |
| optional-count | — | — |
| contains | — | — |
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
| regex-iri | — | — |
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
| **throughput** star-join, 16 clients (queries/s) | **1197** | 25 |

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles | oxigraph |
|---|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 141 | **22** |
| server RSS after every query ran once (MiB) | 186 | **67** |
| server RSS after the run (MiB) | **400** | 415 |
| peak RSS during the throughput run (MiB) | **417** | 421 |
| server RSS after the run less Sparkles' decoded-block cache of 23 MiB (MiB) | 377 | — |
| load peak RSS (MiB) | **461** | 824 |
| index size on disk | **29 MiB** | 147 MiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles | oxigraph |
|---|---:|---:|
| star-join | +46 / 187 | **+45** / 67 |

Mixed load: concurrent star-join readers (oha) and one writer of single-triple INSERT DATA commits. Each engine runs alone on a copy of its store.

| mixed | sparkles | oxigraph |
|---|---:|---:|
| reads/s | **2197.9** | 15.7 |
| read p50 (ms) | **6.9** | 935.4 |
| read p99 (ms) | **12.3** | 2027.4 |
| writes/s | 252.0 | **1276.9** |
| write p50 (ms) | 3.7 | **0.8** |
| write p99 (ms) | 8.3 | **1.0** |
| failed reads / writes | 0 / 0 | 0 / 0 |
