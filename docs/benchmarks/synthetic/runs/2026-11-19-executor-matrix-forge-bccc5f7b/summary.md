# Eager-against-cursor matrix

- host: forge
- binary commit: bccc5f7b
- binary sha256: 27cf6f507e4ad5ad12f65605fe4f6c4961d95c43dcf4affb7aa4ca90d5703cd6
- started: 2026-11-19T00:08:03Z
- ended: 2026-11-19T02:31:41Z
- settings: 31 queries, scales 100000 1000000 people, threads 12 1, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server

Every cell's eager and streaming answers agree.

## 1.05m-1t: 1.05M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 2.791 | 2.798 | 1.002 | 0.943 | 0.934 |
| class-predicates | 2.796 | 2.800 | 1.001 | 0.954 | 0.945 |
| contains | 3.519 | 3.760 | 1.068 | 2.534 | 2.536 |
| count-all | 1.970 | 1.973 | 1.002 | 0.005 | 0.006 |
| distinct-join | 5.056 | 5.979 | 1.183 | 3.556 | 3.741 |
| distinct-obj | 1.929 | 1.976 | 1.024 | 0.004 | 0.005 |
| employee-docs | 1.981 | 1.981 | 1.000 | 0.038 | 0.040 |
| exists-join | 2.801 | 2.802 | 1.000 | 1.060 | 1.038 |
| export-500k | 229.041 | 363.044 | 1.585 | 2.128 | 0.403 |
| expr-agg-arg | 3.513 | 3.280 | 0.934 | 2.287 | 1.930 |
| expr-bind-group | 1.975 | 1.980 | 1.003 | 0.507 | 0.364 |
| expr-order-key | 2.802 | 2.803 | 1.000 | 1.143 | 1.142 |
| group-avg | 4.893 | 4.374 | 0.894 | 3.580 | 3.186 |
| knows-reach | 12.569 | 12.691 | 1.010 | 11.339 | 11.394 |
| lang-filter | 2.800 | 2.969 | 1.060 | 1.785 | 1.785 |
| minus | 1.974 | 1.976 | 1.001 | 0.365 | 0.263 |
| not-exists | 1.974 | 1.978 | 1.002 | 0.764 | 0.742 |
| optional-chain | 15.050 | 22.042 | 1.465 | 2.008 | 2.061 |
| optional-count | 1.973 | 1.974 | 1.001 | 0.416 | 0.541 |
| order-by-full | 77.044 | 98.041 | 1.273 | 41.781 | 41.544 |
| path-plus | 1.970 | 1.974 | 1.002 | 0.013 | 0.013 |
| predicate-counts | 1.927 | 1.983 | 1.029 | 0.005 | 0.007 |
| predicate-subjects | 1.923 | 1.982 | 1.030 | 0.007 | 0.009 |
| range-topk | 1.974 | 1.974 | 1.000 | 0.542 | 0.535 |
| regex-iri | 4.884 | 4.971 | 1.018 | 3.754 | 3.767 |
| star-join | 4.944 | 7.005 | 1.417 | 1.398 | 1.417 |
| star-lookup | 1.984 | 1.987 | 1.002 | 0.062 | 0.063 |
| subquery-agg | 1.972 | 1.975 | 1.002 | 0.185 | 0.185 |
| two-hop-count | 3.861 | 3.904 | 1.011 | 2.635 | 2.625 |
| types-grouped | 1.396 | 1.976 | 1.415 | 0.008 | 0.013 |
| values-star | 1.979 | 1.982 | 1.001 | 0.016 | 0.019 |
| **geometric mean** | | | 1.068 | | |

## 1.05m-12t: 1.05M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 1.677 | 1.935 | 1.154 | 0.989 | 1.029 |
| class-predicates | 1.700 | 1.981 | 1.165 | 0.997 | 1.052 |
| contains | 1.451 | 1.685 | 1.162 | 0.732 | 0.762 |
| count-all | 0.450 | 0.825 | 1.834 | 0.007 | 0.010 |
| distinct-join | 4.949 | 5.543 | 1.120 | 3.695 | 3.919 |
| distinct-obj | 0.508 | 0.802 | 1.578 | 0.006 | 0.009 |
| employee-docs | 0.688 | 0.978 | 1.422 | 0.051 | 0.056 |
| exists-join | 1.916 | 2.135 | 1.114 | 1.145 | 1.142 |
| export-500k | 216.165 | 292.121 | 1.351 | 2.377 | 0.376 |
| expr-agg-arg | 3.494 | 3.110 | 0.890 | 2.679 | 2.068 |
| expr-bind-group | 1.370 | 1.334 | 0.974 | 0.668 | 0.395 |
| expr-order-key | 1.994 | 2.277 | 1.142 | 1.253 | 1.271 |
| group-avg | 4.737 | 4.478 | 0.945 | 3.887 | 3.418 |
| knows-reach | 12.710 | 12.824 | 1.009 | 11.640 | 11.705 |
| lang-filter | 1.455 | 1.727 | 1.187 | 0.756 | 0.816 |
| minus | 1.123 | 1.150 | 1.024 | 0.435 | 0.287 |
| not-exists | 1.533 | 1.701 | 1.110 | 0.833 | 0.813 |
| optional-chain | 14.146 | 18.663 | 1.319 | 2.141 | 2.173 |
| optional-count | 1.194 | 1.535 | 1.286 | 0.469 | 0.602 |
| order-by-full | 42.076 | 52.117 | 1.239 | 9.415 | 9.681 |
| path-plus | 0.516 | 0.770 | 1.491 | 0.020 | 0.019 |
| predicate-counts | 0.534 | 0.879 | 1.644 | 0.007 | 0.011 |
| predicate-subjects | 0.497 | 0.861 | 1.733 | 0.010 | 0.016 |
| range-topk | 1.384 | 1.673 | 1.209 | 0.570 | 0.579 |
| regex-iri | 1.717 | 1.837 | 1.070 | 0.974 | 1.025 |
| star-join | 4.876 | 5.943 | 1.219 | 1.540 | 1.637 |
| star-lookup | 0.819 | 1.097 | 1.338 | 0.087 | 0.092 |
| subquery-agg | 0.860 | 1.007 | 1.172 | 0.241 | 0.231 |
| two-hop-count | 2.032 | 2.321 | 1.143 | 1.242 | 1.252 |
| types-grouped | 0.578 | 0.817 | 1.413 | 0.011 | 0.021 |
| values-star | 0.681 | 0.839 | 1.231 | 0.020 | 0.029 |
| **geometric mean** | | | 1.230 | | |

## 10.5m-1t: 10.5M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.379 | 11.770 | 1.034 | 10.034 | 10.436 |
| class-predicates | 11.450 | 11.816 | 1.032 | 10.132 | 10.401 |
| contains | 25.801 | 25.846 | 1.002 | 24.574 | 24.593 |
| count-all | 1.352 | 1.975 | 1.461 | 0.006 | 0.007 |
| distinct-join | 43.587 | 45.750 | 1.050 | 38.455 | 37.551 |
| distinct-obj | 1.931 | 1.975 | 1.023 | 0.004 | 0.006 |
| employee-docs | 1.979 | 1.983 | 1.002 | 0.049 | 0.054 |
| exists-join | 9.989 | 9.963 | 0.997 | 8.560 | 8.576 |
| export-500k | 234.960 | 362.972 | 1.545 | 2.389 | 0.418 |
| expr-agg-arg | 26.712 | 23.871 | 0.894 | 25.271 | 22.701 |
| expr-bind-group | 6.034 | 4.494 | 0.745 | 4.799 | 3.209 |
| expr-order-key | 13.032 | 12.389 | 0.951 | 11.028 | 10.918 |
| group-avg | 41.080 | 39.546 | 0.963 | 39.472 | 37.981 |
| knows-reach | 138.230 | 138.493 | 1.002 | 136.810 | 137.127 |
| lang-filter | 18.801 | 18.869 | 1.004 | 17.574 | 17.631 |
| minus | 5.039 | 3.731 | 0.740 | 3.774 | 2.463 |
| not-exists | 10.241 | 9.947 | 0.971 | 8.985 | 8.624 |
| optional-chain | 136.004 | 222.053 | 1.633 | 26.270 | 25.122 |
| optional-count | 5.825 | 6.583 | 1.130 | 4.510 | 5.303 |
| order-by-full | 858.048 | 1066.690 | 1.243 | 518.709 | 520.744 |
| path-plus | 1.946 | 1.974 | 1.015 | 0.010 | 0.010 |
| predicate-counts | 1.298 | 1.982 | 1.527 | 0.005 | 0.007 |
| predicate-subjects | 1.971 | 1.982 | 1.006 | 0.007 | 0.009 |
| range-topk | 6.450 | 6.447 | 1.000 | 5.124 | 5.059 |
| regex-iri | 39.011 | 38.985 | 0.999 | 37.743 | 37.783 |
| star-join | 36.058 | 52.274 | 1.450 | 13.938 | 14.640 |
| star-lookup | 1.985 | 1.986 | 1.001 | 0.058 | 0.061 |
| subquery-agg | 2.878 | 2.913 | 1.012 | 1.699 | 1.644 |
| two-hop-count | 29.172 | 29.348 | 1.006 | 27.818 | 27.870 |
| types-grouped | 1.970 | 1.974 | 1.002 | 0.008 | 0.013 |
| values-star | 1.980 | 1.982 | 1.001 | 0.017 | 0.021 |
| **geometric mean** | | | 1.060 | | |

## 10.5m-12t: 10.5M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.535 | 11.824 | 1.025 | 10.433 | 10.690 |
| class-predicates | 11.462 | 11.782 | 1.028 | 10.329 | 10.678 |
| contains | 5.089 | 5.217 | 1.025 | 4.291 | 4.263 |
| count-all | 0.506 | 0.764 | 1.512 | 0.009 | 0.011 |
| distinct-join | 44.292 | 43.956 | 0.992 | 38.891 | 38.095 |
| distinct-obj | 0.520 | 0.780 | 1.498 | 0.007 | 0.009 |
| employee-docs | 0.719 | 0.937 | 1.303 | 0.069 | 0.079 |
| exists-join | 10.096 | 9.909 | 0.982 | 9.085 | 8.767 |
| export-500k | 228.272 | 289.663 | 1.269 | 2.306 | 0.383 |
| expr-agg-arg | 25.147 | 24.552 | 0.976 | 23.987 | 23.346 |
| expr-bind-group | 4.495 | 4.390 | 0.977 | 3.684 | 3.379 |
| expr-order-key | 11.870 | 12.401 | 1.045 | 10.079 | 11.275 |
| group-avg | 41.803 | 40.483 | 0.968 | 40.394 | 39.081 |
| knows-reach | 143.335 | 141.608 | 0.988 | 141.859 | 140.383 |
| lang-filter | 3.846 | 4.014 | 1.043 | 3.047 | 3.059 |
| minus | 4.739 | 3.614 | 0.762 | 3.907 | 2.622 |
| not-exists | 10.130 | 10.162 | 1.003 | 9.183 | 9.144 |
| optional-chain | 133.348 | 173.888 | 1.304 | 27.535 | 27.628 |
| optional-count | 5.508 | 6.565 | 1.192 | 4.646 | 5.521 |
| order-by-full | 469.392 | 566.968 | 1.208 | 146.426 | 166.012 |
| path-plus | 0.510 | 0.757 | 1.484 | 0.016 | 0.015 |
| predicate-counts | 0.524 | 0.853 | 1.627 | 0.008 | 0.011 |
| predicate-subjects | 0.482 | 0.754 | 1.564 | 0.011 | 0.015 |
| range-topk | 2.998 | 3.367 | 1.123 | 2.093 | 2.277 |
| regex-iri | 7.135 | 7.341 | 1.029 | 6.319 | 6.330 |
| star-join | 33.808 | 43.397 | 1.284 | 13.246 | 13.719 |
| star-lookup | 0.810 | 1.036 | 1.278 | 0.072 | 0.081 |
| subquery-agg | 1.525 | 1.714 | 1.124 | 0.783 | 0.800 |
| two-hop-count | 12.570 | 12.538 | 0.997 | 11.382 | 11.363 |
| types-grouped | 0.594 | 0.838 | 1.411 | 0.013 | 0.020 |
| values-star | 0.685 | 0.863 | 1.260 | 0.021 | 0.028 |
| **geometric mean** | | | 1.152 | | |

Times are medians over every timed request of the client's wall time (curl time_total) and of the server's execMs. The ratio is of the wall times.
