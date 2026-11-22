# Eager-against-cursor matrix

- host: forge
- binary commit: 8baa5686
- binary sha256: 20a381e40aa22acf2b099436054025829a753a69ca618156342d8af9b1f59d03
- started: 2026-11-18T09:27:30Z
- ended: 2026-11-18T11:51:28Z
- settings: 31 queries, scales 100000 1000000 people, threads 12 1, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server

Every cell's eager and streaming answers agree.

## 1.05m-1t: 1.05M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 2.795 | 2.796 | 1.000 | 0.929 | 0.925 |
| class-predicates | 2.428 | 2.800 | 1.153 | 0.938 | 0.938 |
| contains | 3.716 | 3.761 | 1.012 | 2.548 | 2.548 |
| count-all | 1.577 | 1.944 | 1.233 | 0.005 | 0.006 |
| distinct-join | 5.244 | 5.978 | 1.140 | 3.593 | 3.837 |
| distinct-obj | 1.633 | 1.971 | 1.207 | 0.004 | 0.005 |
| employee-docs | 1.979 | 1.981 | 1.001 | 0.037 | 0.040 |
| exists-join | 2.803 | 2.801 | 1.000 | 1.049 | 1.043 |
| export-500k | 233.596 | 356.007 | 1.524 | 2.096 | 0.395 |
| expr-agg-arg | 3.803 | 3.238 | 0.851 | 2.545 | 1.922 |
| expr-bind-group | 1.973 | 1.981 | 1.004 | 0.601 | 0.370 |
| expr-order-key | 2.802 | 2.804 | 1.001 | 1.159 | 1.147 |
| group-avg | 5.200 | 4.558 | 0.877 | 3.890 | 3.200 |
| knows-reach | 12.760 | 12.636 | 0.990 | 11.498 | 11.329 |
| lang-filter | 2.981 | 3.056 | 1.025 | 1.833 | 1.839 |
| minus | 1.974 | 1.973 | 1.000 | 0.412 | 0.262 |
| not-exists | 2.020 | 1.976 | 0.978 | 0.806 | 0.741 |
| optional-chain | 15.035 | 22.037 | 1.466 | 2.017 | 2.061 |
| optional-count | 1.972 | 1.974 | 1.001 | 0.484 | 0.544 |
| order-by-full | 76.213 | 97.216 | 1.276 | 41.096 | 41.536 |
| path-plus | 1.888 | 1.974 | 1.046 | 0.011 | 0.013 |
| predicate-counts | 1.435 | 1.981 | 1.380 | 0.005 | 0.007 |
| predicate-subjects | 1.946 | 1.980 | 1.018 | 0.007 | 0.009 |
| range-topk | 1.973 | 1.975 | 1.001 | 0.543 | 0.537 |
| regex-iri | 4.970 | 5.003 | 1.007 | 3.805 | 3.800 |
| star-join | 5.044 | 7.007 | 1.389 | 1.414 | 1.439 |
| star-lookup | 1.985 | 1.987 | 1.001 | 0.062 | 0.065 |
| subquery-agg | 1.973 | 1.972 | 0.999 | 0.186 | 0.184 |
| two-hop-count | 3.853 | 3.902 | 1.013 | 2.622 | 2.616 |
| types-grouped | 1.972 | 1.972 | 1.000 | 0.008 | 0.013 |
| values-star | 1.980 | 1.982 | 1.001 | 0.015 | 0.020 |
| **geometric mean** | | | 1.073 | | |

## 1.05m-12t: 1.05M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 1.653 | 1.925 | 1.165 | 0.986 | 1.012 |
| class-predicates | 1.675 | 1.937 | 1.156 | 0.996 | 1.020 |
| contains | 1.480 | 1.673 | 1.131 | 0.759 | 0.773 |
| count-all | 0.504 | 0.768 | 1.523 | 0.008 | 0.010 |
| distinct-join | 4.978 | 5.625 | 1.130 | 3.760 | 4.015 |
| distinct-obj | 0.502 | 0.778 | 1.549 | 0.007 | 0.009 |
| employee-docs | 0.696 | 0.984 | 1.415 | 0.050 | 0.054 |
| exists-join | 1.899 | 2.216 | 1.166 | 1.139 | 1.150 |
| export-500k | 220.362 | 287.297 | 1.304 | 2.389 | 0.374 |
| expr-agg-arg | 3.674 | 3.078 | 0.838 | 2.901 | 2.046 |
| expr-bind-group | 1.489 | 1.352 | 0.908 | 0.762 | 0.398 |
| expr-order-key | 1.992 | 2.128 | 1.069 | 1.242 | 1.225 |
| group-avg | 4.902 | 4.382 | 0.894 | 4.035 | 3.362 |
| knows-reach | 12.820 | 12.756 | 0.995 | 11.811 | 11.678 |
| lang-filter | 1.493 | 1.657 | 1.110 | 0.778 | 0.810 |
| minus | 1.148 | 1.233 | 1.074 | 0.481 | 0.281 |
| not-exists | 1.579 | 1.722 | 1.091 | 0.867 | 0.793 |
| optional-chain | 14.244 | 18.607 | 1.306 | 2.113 | 2.234 |
| optional-count | 1.272 | 1.496 | 1.175 | 0.548 | 0.575 |
| order-by-full | 41.501 | 51.578 | 1.243 | 8.768 | 9.033 |
| path-plus | 0.492 | 0.786 | 1.595 | 0.018 | 0.020 |
| predicate-counts | 0.536 | 0.763 | 1.422 | 0.007 | 0.011 |
| predicate-subjects | 0.443 | 0.762 | 1.721 | 0.008 | 0.015 |
| range-topk | 1.371 | 1.619 | 1.182 | 0.582 | 0.575 |
| regex-iri | 1.761 | 1.929 | 1.095 | 1.024 | 1.017 |
| star-join | 4.941 | 5.976 | 1.210 | 1.591 | 1.651 |
| star-lookup | 0.813 | 1.065 | 1.309 | 0.083 | 0.093 |
| subquery-agg | 0.838 | 1.085 | 1.295 | 0.218 | 0.209 |
| two-hop-count | 2.010 | 2.267 | 1.128 | 1.232 | 1.229 |
| types-grouped | 0.558 | 0.782 | 1.402 | 0.012 | 0.021 |
| values-star | 0.680 | 0.931 | 1.368 | 0.021 | 0.031 |
| **geometric mean** | | | 1.208 | | |

## 10.5m-1t: 10.5M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.451 | 11.693 | 1.021 | 10.103 | 10.344 |
| class-predicates | 11.452 | 11.701 | 1.022 | 10.074 | 10.335 |
| contains | 25.886 | 25.951 | 1.002 | 24.649 | 24.693 |
| count-all | 1.898 | 1.970 | 1.038 | 0.006 | 0.008 |
| distinct-join | 44.424 | 45.942 | 1.034 | 39.158 | 38.214 |
| distinct-obj | 1.274 | 1.972 | 1.548 | 0.004 | 0.005 |
| employee-docs | 1.981 | 1.984 | 1.002 | 0.050 | 0.054 |
| exists-join | 9.983 | 10.030 | 1.005 | 8.601 | 8.629 |
| export-500k | 239.001 | 356.016 | 1.490 | 2.321 | 0.400 |
| expr-agg-arg | 29.938 | 24.521 | 0.819 | 28.530 | 23.127 |
| expr-bind-group | 6.979 | 4.577 | 0.656 | 5.765 | 3.295 |
| expr-order-key | 13.050 | 12.209 | 0.936 | 11.056 | 10.651 |
| group-avg | 46.736 | 38.805 | 0.830 | 45.206 | 37.228 |
| knows-reach | 141.146 | 138.632 | 0.982 | 140.011 | 137.307 |
| lang-filter | 19.155 | 19.264 | 1.006 | 18.041 | 18.074 |
| minus | 5.825 | 3.726 | 0.640 | 4.300 | 2.467 |
| not-exists | 10.759 | 9.969 | 0.927 | 9.477 | 8.737 |
| optional-chain | 137.047 | 220.019 | 1.605 | 26.970 | 25.312 |
| optional-count | 6.651 | 6.566 | 0.987 | 5.305 | 5.279 |
| order-by-full | 858.015 | 1061.575 | 1.237 | 517.200 | 519.819 |
| path-plus | 1.328 | 1.974 | 1.486 | 0.009 | 0.009 |
| predicate-counts | 1.333 | 1.982 | 1.487 | 0.005 | 0.007 |
| predicate-subjects | 1.970 | 1.980 | 1.005 | 0.007 | 0.009 |
| range-topk | 6.501 | 6.470 | 0.995 | 5.172 | 5.092 |
| regex-iri | 39.166 | 39.280 | 1.003 | 38.164 | 38.157 |
| star-join | 36.763 | 52.107 | 1.417 | 14.148 | 14.843 |
| star-lookup | 1.986 | 1.989 | 1.001 | 0.058 | 0.058 |
| subquery-agg | 2.902 | 2.905 | 1.001 | 1.694 | 1.626 |
| two-hop-count | 28.929 | 28.966 | 1.001 | 27.585 | 27.568 |
| types-grouped | 1.972 | 1.974 | 1.001 | 0.008 | 0.013 |
| values-star | 1.981 | 1.983 | 1.001 | 0.016 | 0.020 |
| **geometric mean** | | | 1.045 | | |

## 10.5m-12t: 10.5M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.551 | 11.724 | 1.015 | 10.484 | 10.600 |
| class-predicates | 11.402 | 11.899 | 1.044 | 10.284 | 10.722 |
| contains | 5.069 | 5.305 | 1.047 | 4.256 | 4.275 |
| count-all | 0.461 | 0.745 | 1.616 | 0.009 | 0.011 |
| distinct-join | 44.788 | 45.186 | 1.009 | 39.279 | 39.092 |
| distinct-obj | 0.512 | 0.764 | 1.491 | 0.007 | 0.009 |
| employee-docs | 0.726 | 0.960 | 1.324 | 0.069 | 0.077 |
| exists-join | 9.917 | 10.069 | 1.015 | 8.950 | 8.915 |
| export-500k | 225.286 | 283.745 | 1.259 | 2.198 | 0.383 |
| expr-agg-arg | 28.817 | 24.544 | 0.852 | 27.674 | 23.414 |
| expr-bind-group | 5.585 | 4.441 | 0.795 | 4.751 | 3.413 |
| expr-order-key | 11.866 | 12.134 | 1.023 | 10.103 | 10.959 |
| group-avg | 46.862 | 39.052 | 0.833 | 45.441 | 37.719 |
| knows-reach | 142.429 | 140.560 | 0.987 | 141.111 | 139.260 |
| lang-filter | 3.930 | 3.960 | 1.008 | 3.124 | 3.056 |
| minus | 5.137 | 3.564 | 0.694 | 4.303 | 2.586 |
| not-exists | 10.643 | 9.965 | 0.936 | 9.755 | 8.949 |
| optional-chain | 131.810 | 169.871 | 1.289 | 25.146 | 26.508 |
| optional-count | 6.286 | 6.481 | 1.031 | 5.402 | 5.452 |
| order-by-full | 434.529 | 501.717 | 1.155 | 108.713 | 107.363 |
| path-plus | 0.456 | 0.760 | 1.667 | 0.012 | 0.015 |
| predicate-counts | 0.479 | 0.807 | 1.683 | 0.007 | 0.011 |
| predicate-subjects | 0.448 | 0.797 | 1.779 | 0.009 | 0.017 |
| range-topk | 2.962 | 3.208 | 1.083 | 2.111 | 2.071 |
| regex-iri | 7.296 | 7.533 | 1.032 | 6.499 | 6.503 |
| star-join | 33.717 | 43.764 | 1.298 | 13.309 | 13.919 |
| star-lookup | 0.826 | 1.074 | 1.300 | 0.073 | 0.084 |
| subquery-agg | 1.606 | 1.677 | 1.045 | 0.822 | 0.773 |
| two-hop-count | 12.532 | 12.346 | 0.985 | 11.313 | 11.176 |
| types-grouped | 0.573 | 0.835 | 1.458 | 0.012 | 0.020 |
| values-star | 0.688 | 0.901 | 1.311 | 0.021 | 0.027 |
| **geometric mean** | | | 1.133 | | |

Times are medians over every timed request of the client's wall time (curl time_total) and of the server's execMs. The ratio is of the wall times.
