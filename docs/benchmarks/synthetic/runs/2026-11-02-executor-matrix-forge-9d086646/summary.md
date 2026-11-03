# Eager-against-cursor matrix

- host: forge
- binary commit: final
- binary sha256: c0774599858cac5034cfb26fc04c62b5d507b6d8115e3441bef3cb3945ae74f7
- started: 2026-11-02T18:32:08Z
- ended: 2026-11-02T20:02:01Z
- settings: 31 queries, scales 100000 1000000 people, threads 12, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server

Every cell's eager and streaming answers agree.

## 1.05m-12t: 1.05M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 1.661 | 1.910 | 1.150 | 0.966 | 0.994 |
| class-predicates | 1.672 | 1.948 | 1.165 | 0.973 | 1.009 |
| contains | 1.413 | 1.593 | 1.128 | 0.718 | 0.708 |
| count-all | 0.473 | 0.756 | 1.598 | 0.008 | 0.010 |
| distinct-join | 4.771 | 5.613 | 1.177 | 3.497 | 4.004 |
| distinct-obj | 0.475 | 0.780 | 1.641 | 0.007 | 0.009 |
| employee-docs | 0.685 | 0.899 | 1.312 | 0.050 | 0.054 |
| exists-join | 1.860 | 2.183 | 1.174 | 1.104 | 1.141 |
| export-500k | 249.233 | 279.998 | 1.123 | 2.370 | 0.408 |
| expr-agg-arg | 3.740 | 3.087 | 0.826 | 2.935 | 2.080 |
| expr-bind-group | 1.492 | 1.367 | 0.916 | 0.789 | 0.401 |
| expr-order-key | 1.970 | 2.221 | 1.128 | 1.247 | 1.261 |
| group-avg | 5.177 | 4.617 | 0.892 | 4.370 | 3.562 |
| knows-reach | 12.815 | 12.640 | 0.986 | 11.789 | 11.595 |
| lang-filter | 1.464 | 1.656 | 1.131 | 0.775 | 0.760 |
| minus | 1.001 | 1.142 | 1.141 | 0.348 | 0.298 |
| not-exists | 1.575 | 1.761 | 1.118 | 0.878 | 0.818 |
| optional-chain | 14.663 | 17.795 | 1.214 | 2.147 | 2.113 |
| optional-count | 1.257 | 1.429 | 1.136 | 0.555 | 0.544 |
| order-by-full | 48.629 | 54.140 | 1.113 | 14.168 | 14.482 |
| path-plus | 0.521 | 0.775 | 1.488 | 0.018 | 0.020 |
| predicate-counts | 0.464 | 0.802 | 1.729 | 0.006 | 0.010 |
| predicate-subjects | 0.481 | 0.756 | 1.572 | 0.011 | 0.015 |
| range-topk | 1.378 | 1.617 | 1.173 | 0.568 | 0.572 |
| regex-iri | 1.742 | 1.937 | 1.112 | 1.036 | 1.047 |
| star-join | 5.040 | 5.761 | 1.143 | 1.644 | 1.675 |
| star-lookup | 0.793 | 1.110 | 1.399 | 0.084 | 0.089 |
| subquery-agg | 0.821 | 1.046 | 1.274 | 0.221 | 0.188 |
| two-hop-count | 1.998 | 2.297 | 1.150 | 1.231 | 1.252 |
| types-grouped | 0.573 | 0.804 | 1.405 | 0.012 | 0.022 |
| values-star | 0.684 | 0.889 | 1.300 | 0.020 | 0.032 |
| **geometric mean** | | | 1.202 | | |

## 10.5m-12t: 10.5M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.068 | 11.581 | 1.046 | 9.945 | 10.508 |
| class-predicates | 11.139 | 11.610 | 1.042 | 10.037 | 10.442 |
| contains | 4.902 | 5.119 | 1.044 | 4.141 | 4.148 |
| count-all | 0.468 | 0.744 | 1.589 | 0.009 | 0.013 |
| distinct-join | 44.192 | 44.859 | 1.015 | 38.552 | 38.876 |
| distinct-obj | 0.470 | 0.748 | 1.593 | 0.007 | 0.011 |
| employee-docs | 0.710 | 0.927 | 1.304 | 0.068 | 0.077 |
| exists-join | 10.079 | 10.088 | 1.001 | 8.912 | 8.934 |
| export-500k | 253.042 | 271.828 | 1.074 | 2.328 | 0.409 |
| expr-agg-arg | 29.685 | 24.712 | 0.832 | 28.509 | 23.547 |
| expr-bind-group | 5.567 | 4.489 | 0.806 | 4.750 | 3.423 |
| expr-order-key | 12.267 | 12.305 | 1.003 | 10.496 | 11.234 |
| group-avg | 49.073 | 41.388 | 0.843 | 47.578 | 40.071 |
| knows-reach | 143.020 | 140.894 | 0.985 | 141.699 | 139.594 |
| lang-filter | 3.883 | 3.994 | 1.028 | 3.123 | 3.055 |
| minus | 4.072 | 3.552 | 0.872 | 3.287 | 2.583 |
| not-exists | 10.721 | 9.973 | 0.930 | 9.701 | 8.935 |
| optional-chain | 141.600 | 161.960 | 1.144 | 29.340 | 25.564 |
| optional-count | 6.229 | 6.242 | 1.002 | 5.360 | 5.222 |
| order-by-full | 595.639 | 649.162 | 1.090 | 250.395 | 268.349 |
| path-plus | 0.472 | 0.739 | 1.566 | 0.014 | 0.014 |
| predicate-counts | 0.533 | 0.833 | 1.564 | 0.008 | 0.015 |
| predicate-subjects | 0.459 | 0.822 | 1.793 | 0.009 | 0.015 |
| range-topk | 3.330 | 3.392 | 1.018 | 2.424 | 2.323 |
| regex-iri | 7.321 | 7.453 | 1.018 | 6.513 | 6.444 |
| star-join | 33.165 | 42.378 | 1.278 | 13.210 | 13.868 |
| star-lookup | 0.788 | 1.064 | 1.349 | 0.073 | 0.084 |
| subquery-agg | 1.585 | 1.742 | 1.099 | 0.816 | 0.810 |
| two-hop-count | 12.318 | 12.273 | 0.996 | 11.140 | 11.122 |
| types-grouped | 0.562 | 0.857 | 1.524 | 0.012 | 0.019 |
| values-star | 0.690 | 0.925 | 1.340 | 0.021 | 0.028 |
| **geometric mean** | | | 1.128 | | |

Times are medians over every timed request of the client's wall time (curl time_total) and of the server's execMs. The ratio is of the wall times.
