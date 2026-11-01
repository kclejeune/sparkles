| query | oxigraph (ms) |
|---|---:|
| count-all | — |
| types-grouped | — |
| star-join | — |
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

Cold runs: the median of 5 runs of each query after a server restart with the engine's files evicted from the page cache.

| query (cold) | oxigraph (ms) |
|---|---:|
| star-join | **1162.9** |
| path-plus | **3.5** |
| star-lookup | **22.4** |
| values-star | **7.7** |
| employee-docs | **19.7** |
| **time to ready** after the restart (median, s) | 0.08 |
