| query | sparkles (ms) | fluree (ms) |
|---|---:|---:|
| **load** (s) | **0.68** | 1.21 |
| count-all | — | — |
| types-grouped | — | — |
| star-join | — | — |
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

Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). Lower is better.

| memory | sparkles | fluree |
|---|---:|---:|
| server RSS 2 s after start, before any query (MiB) | 166 | **61** |
| server RSS after every query ran once (MiB) | 248 | **232** |
| load peak RSS (MiB) | **460** | 480 |
| index size on disk | **29 MiB** | 37 MiB |

Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, in MiB. The peak is reset (clear_refs) before each request of the answer check.

| query (rise / peak) | sparkles | fluree |
|---|---:|---:|
| count-all | **+2** / 168 | +81 / 177 |
| two-hop-count | +61 / 234 | **+12** / 184 |
| contains | +12 / 246 | **+2** / 193 |
| path-plus | +1 / 247 | **+0** / 194 |
| distinct-obj | **+0** / 247 | +0 / 204 |
| star-lookup | +35 / 282 | **+6** / 210 |
| values-star | **+1** / 283 | +5 / 222 |
| employee-docs | **+0** / 283 | +0 / 232 |
