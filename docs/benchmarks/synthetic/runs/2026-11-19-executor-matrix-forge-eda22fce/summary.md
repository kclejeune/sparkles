# Eager-against-cursor matrix

- host: forge
- binary commit: eda22fce
- binary sha256: 915ba62e2bade5551f25d5b0c7acca90aa8578e21f180dfb7ccdbf6290fc023c
- started: 2026-11-19T02:31:44Z
- ended: 2026-11-19T04:54:54Z
- settings: 31 queries, scales 100000 1000000 people, threads 12 1, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server

Every cell's eager and streaming answers agree.

## 1.05m-1t: 1.05M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 2.796 | 2.799 | 1.001 | 0.935 | 0.928 |
| class-predicates | 2.796 | 2.800 | 1.001 | 0.940 | 0.937 |
| contains | 3.696 | 3.780 | 1.023 | 2.551 | 2.553 |
| count-all | 1.909 | 1.973 | 1.033 | 0.005 | 0.006 |
| distinct-join | 5.311 | 5.978 | 1.126 | 3.637 | 3.823 |
| distinct-obj | 1.894 | 1.973 | 1.042 | 0.004 | 0.005 |
| employee-docs | 1.981 | 1.984 | 1.001 | 0.038 | 0.042 |
| exists-join | 2.801 | 2.803 | 1.001 | 1.071 | 1.044 |
| export-500k | 232.748 | 346.026 | 1.487 | 2.305 | 0.393 |
| expr-agg-arg | 3.827 | 3.240 | 0.846 | 2.558 | 1.921 |
| expr-bind-group | 1.971 | 1.982 | 1.005 | 0.602 | 0.372 |
| expr-order-key | 2.803 | 2.801 | 0.999 | 1.154 | 1.139 |
| group-avg | 5.144 | 4.372 | 0.850 | 3.843 | 3.194 |
| knows-reach | 12.745 | 12.641 | 0.992 | 11.518 | 11.293 |
| lang-filter | 2.955 | 3.062 | 1.036 | 1.867 | 1.864 |
| minus | 1.972 | 1.974 | 1.002 | 0.412 | 0.261 |
| not-exists | 2.012 | 2.025 | 1.006 | 0.805 | 0.742 |
| optional-chain | 15.046 | 21.032 | 1.398 | 2.020 | 2.065 |
| optional-count | 1.973 | 1.973 | 1.000 | 0.484 | 0.542 |
| order-by-full | 76.071 | 95.216 | 1.252 | 41.226 | 41.550 |
| path-plus | 1.599 | 1.976 | 1.235 | 0.012 | 0.013 |
| predicate-counts | 1.950 | 1.982 | 1.017 | 0.005 | 0.007 |
| predicate-subjects | 1.762 | 1.983 | 1.125 | 0.008 | 0.009 |
| range-topk | 1.976 | 1.976 | 1.000 | 0.543 | 0.537 |
| regex-iri | 4.877 | 4.928 | 1.010 | 3.747 | 3.725 |
| star-join | 5.047 | 6.977 | 1.382 | 1.414 | 1.431 |
| star-lookup | 1.985 | 1.987 | 1.001 | 0.063 | 0.067 |
| subquery-agg | 1.974 | 1.975 | 1.001 | 0.187 | 0.186 |
| two-hop-count | 3.877 | 3.930 | 1.014 | 2.632 | 2.626 |
| types-grouped | 1.972 | 1.975 | 1.002 | 0.008 | 0.013 |
| values-star | 1.981 | 1.982 | 1.001 | 0.017 | 0.021 |
| **geometric mean** | | | 1.052 | | |

## 1.05m-12t: 1.05M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 1.677 | 1.946 | 1.160 | 0.992 | 0.991 |
| class-predicates | 1.700 | 1.970 | 1.159 | 1.006 | 1.006 |
| contains | 1.460 | 1.647 | 1.128 | 0.735 | 0.742 |
| count-all | 0.427 | 0.792 | 1.856 | 0.007 | 0.011 |
| distinct-join | 5.111 | 5.570 | 1.090 | 3.862 | 3.970 |
| distinct-obj | 0.482 | 0.762 | 1.582 | 0.006 | 0.011 |
| employee-docs | 0.694 | 0.860 | 1.238 | 0.052 | 0.055 |
| exists-join | 1.911 | 2.099 | 1.098 | 1.162 | 1.141 |
| export-500k | 218.201 | 274.778 | 1.259 | 2.345 | 0.377 |
| expr-agg-arg | 3.696 | 3.034 | 0.821 | 2.895 | 2.028 |
| expr-bind-group | 1.551 | 1.303 | 0.840 | 0.828 | 0.392 |
| expr-order-key | 2.018 | 2.109 | 1.045 | 1.255 | 1.238 |
| group-avg | 4.957 | 4.354 | 0.878 | 4.051 | 3.359 |
| knows-reach | 12.848 | 12.558 | 0.977 | 11.795 | 11.513 |
| lang-filter | 1.512 | 1.648 | 1.090 | 0.802 | 0.766 |
| minus | 1.152 | 1.195 | 1.036 | 0.458 | 0.285 |
| not-exists | 1.575 | 1.748 | 1.110 | 0.869 | 0.796 |
| optional-chain | 14.018 | 17.889 | 1.276 | 2.128 | 2.200 |
| optional-count | 1.288 | 1.489 | 1.156 | 0.575 | 0.580 |
| order-by-full | 41.102 | 48.714 | 1.185 | 8.752 | 9.174 |
| path-plus | 0.528 | 0.823 | 1.559 | 0.018 | 0.021 |
| predicate-counts | 0.514 | 0.845 | 1.641 | 0.007 | 0.011 |
| predicate-subjects | 0.501 | 0.740 | 1.478 | 0.010 | 0.015 |
| range-topk | 1.409 | 1.631 | 1.158 | 0.589 | 0.580 |
| regex-iri | 1.773 | 1.955 | 1.103 | 1.056 | 1.087 |
| star-join | 5.080 | 5.857 | 1.153 | 1.643 | 1.660 |
| star-lookup | 0.808 | 1.062 | 1.314 | 0.086 | 0.089 |
| subquery-agg | 0.892 | 1.026 | 1.151 | 0.250 | 0.222 |
| two-hop-count | 2.026 | 2.316 | 1.143 | 1.237 | 1.228 |
| types-grouped | 0.569 | 0.784 | 1.377 | 0.012 | 0.019 |
| values-star | 0.685 | 0.932 | 1.361 | 0.021 | 0.027 |
| **geometric mean** | | | 1.187 | | |

## 10.5m-1t: 10.5M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.460 | 11.683 | 1.019 | 10.057 | 10.345 |
| class-predicates | 11.317 | 11.755 | 1.039 | 9.966 | 10.375 |
| contains | 25.940 | 25.932 | 1.000 | 24.722 | 24.663 |
| count-all | 1.610 | 1.974 | 1.226 | 0.006 | 0.007 |
| distinct-join | 44.279 | 45.943 | 1.038 | 39.112 | 38.223 |
| distinct-obj | 1.971 | 1.976 | 1.003 | 0.005 | 0.005 |
| employee-docs | 1.982 | 1.986 | 1.002 | 0.049 | 0.053 |
| exists-join | 9.927 | 9.951 | 1.002 | 8.597 | 8.588 |
| export-500k | 236.022 | 342.020 | 1.449 | 2.182 | 0.404 |
| expr-agg-arg | 29.480 | 24.422 | 0.828 | 28.045 | 23.051 |
| expr-bind-group | 6.866 | 4.578 | 0.667 | 5.635 | 3.293 |
| expr-order-key | 13.058 | 12.239 | 0.937 | 11.057 | 10.738 |
| group-avg | 46.260 | 38.514 | 0.833 | 44.592 | 36.927 |
| knows-reach | 140.419 | 137.868 | 0.982 | 139.016 | 136.442 |
| lang-filter | 19.471 | 19.671 | 1.010 | 18.382 | 18.456 |
| minus | 5.479 | 3.624 | 0.661 | 4.232 | 2.464 |
| not-exists | 10.752 | 9.962 | 0.927 | 9.451 | 8.724 |
| optional-chain | 135.031 | 213.014 | 1.578 | 26.728 | 24.874 |
| optional-count | 6.566 | 6.591 | 1.004 | 5.275 | 5.275 |
| order-by-full | 850.098 | 1039.980 | 1.223 | 514.678 | 518.713 |
| path-plus | 1.973 | 1.976 | 1.002 | 0.009 | 0.009 |
| predicate-counts | 1.403 | 1.984 | 1.413 | 0.005 | 0.006 |
| predicate-subjects | 1.909 | 1.982 | 1.038 | 0.007 | 0.009 |
| range-topk | 6.497 | 6.469 | 0.996 | 5.151 | 5.091 |
| regex-iri | 38.922 | 38.896 | 0.999 | 37.660 | 37.635 |
| star-join | 36.072 | 51.153 | 1.418 | 14.074 | 14.875 |
| star-lookup | 1.985 | 1.986 | 1.001 | 0.058 | 0.061 |
| subquery-agg | 2.893 | 2.879 | 0.995 | 1.697 | 1.632 |
| two-hop-count | 29.000 | 29.034 | 1.001 | 27.656 | 27.656 |
| types-grouped | 1.971 | 1.976 | 1.002 | 0.008 | 0.013 |
| values-star | 1.981 | 1.983 | 1.001 | 0.017 | 0.021 |
| **geometric mean** | | | 1.024 | | |

## 10.5m-12t: 10.5M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.315 | 11.828 | 1.045 | 10.187 | 10.747 |
| class-predicates | 11.319 | 11.831 | 1.045 | 10.153 | 10.694 |
| contains | 5.089 | 5.250 | 1.032 | 4.273 | 4.251 |
| count-all | 0.514 | 0.782 | 1.520 | 0.009 | 0.012 |
| distinct-join | 44.830 | 45.326 | 1.011 | 39.425 | 39.107 |
| distinct-obj | 0.497 | 0.835 | 1.678 | 0.006 | 0.009 |
| employee-docs | 0.714 | 1.033 | 1.447 | 0.070 | 0.078 |
| exists-join | 10.024 | 10.104 | 1.008 | 8.941 | 8.953 |
| export-500k | 223.231 | 267.032 | 1.196 | 2.331 | 0.386 |
| expr-agg-arg | 28.645 | 24.643 | 0.860 | 27.528 | 23.439 |
| expr-bind-group | 5.646 | 4.522 | 0.801 | 4.857 | 3.451 |
| expr-order-key | 11.888 | 12.237 | 1.029 | 10.167 | 11.042 |
| group-avg | 47.074 | 39.129 | 0.831 | 45.628 | 37.877 |
| knows-reach | 142.483 | 140.913 | 0.989 | 141.198 | 139.670 |
| lang-filter | 3.914 | 4.048 | 1.034 | 3.125 | 3.109 |
| minus | 5.180 | 3.545 | 0.684 | 4.334 | 2.581 |
| not-exists | 10.742 | 9.971 | 0.928 | 9.653 | 8.951 |
| optional-chain | 133.476 | 162.232 | 1.215 | 26.849 | 26.847 |
| optional-count | 6.271 | 6.456 | 1.030 | 5.403 | 5.484 |
| order-by-full | 441.635 | 492.708 | 1.116 | 121.730 | 125.833 |
| path-plus | 0.516 | 0.754 | 1.460 | 0.014 | 0.014 |
| predicate-counts | 0.524 | 0.827 | 1.576 | 0.007 | 0.011 |
| predicate-subjects | 0.460 | 0.802 | 1.745 | 0.009 | 0.016 |
| range-topk | 2.962 | 3.099 | 1.046 | 2.068 | 2.010 |
| regex-iri | 7.133 | 7.343 | 1.030 | 6.316 | 6.329 |
| star-join | 33.495 | 42.390 | 1.266 | 13.274 | 13.704 |
| star-lookup | 0.799 | 1.049 | 1.313 | 0.073 | 0.084 |
| subquery-agg | 1.548 | 1.863 | 1.203 | 0.807 | 0.846 |
| two-hop-count | 12.529 | 12.311 | 0.983 | 11.370 | 11.167 |
| types-grouped | 0.552 | 0.853 | 1.545 | 0.012 | 0.019 |
| values-star | 0.687 | 0.914 | 1.330 | 0.022 | 0.029 |
| **geometric mean** | | | 1.133 | | |

Times are medians over every timed request of the client's wall time (curl time_total) and of the server's execMs. The ratio is of the wall times.
