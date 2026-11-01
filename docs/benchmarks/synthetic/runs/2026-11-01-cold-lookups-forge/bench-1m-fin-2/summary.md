| query | sparkles (ms) |
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

| query (cold) | sparkles (ms) |
|---|---:|
| star-join | **19.4** |
| path-plus | **3.3** |
| star-lookup | **12.1** |
| values-star | **7.2** |
| employee-docs | **9.0** |
| **time to ready** after the restart (median, s) | 0.09 |
