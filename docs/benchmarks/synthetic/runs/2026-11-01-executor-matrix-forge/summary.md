# Eager-against-cursor matrix

- host: forge
- binary commit: 04d49b57
- binary sha256: 34aa23f8e08949fd166863059128e1afa3108501fb171b65d75494b0ba09e0fd
- started: 2026-11-01T04:47:16Z
- ended: 2026-11-01T05:47:10Z
- settings: 31 queries, scales 100000 1000000 people, threads 12 1, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server

Every cell's eager and streaming answers agree.

## 1.05m-1t: 1.05M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 2.802 | 2.801 | 1.000 | 0.938 | 0.934 |
| class-predicates | 2.804 | 2.803 | 1.000 | 0.946 | 0.945 |
| contains | 3.594 | 3.811 | 1.060 | 2.442 | 2.482 |
| count-all | 1.974 | 1.972 | 0.998 | 0.004 | 0.006 |
| distinct-join | 5.147 | 5.598 | 1.088 | 3.519 | 3.698 |
| distinct-obj | 1.948 | 1.969 | 1.011 | 0.004 | 0.005 |
| employee-docs | 1.980 | 1.973 | 0.997 | 0.033 | 0.040 |
| exists-join | 2.804 | 2.805 | 1.000 | 1.020 | 1.035 |
| export-500k | 265.856 | 371.006 | 1.396 | 2.240 | 0.426 |
| expr-agg-arg | 3.857 | 3.172 | 0.823 | 2.591 | 1.912 |
| expr-bind-group | 1.973 | 1.978 | 1.003 | 0.591 | 0.367 |
| expr-order-key | 2.804 | 2.806 | 1.001 | 1.178 | 1.113 |
| group-avg | 5.428 | 4.838 | 0.891 | 4.038 | 3.473 |
| knows-reach | 12.793 | 12.654 | 0.989 | 11.520 | 11.363 |
| lang-filter | 2.802 | 2.803 | 1.000 | 1.626 | 1.619 |
| minus | 1.974 | 1.975 | 1.001 | 0.301 | 0.256 |
| not-exists | 1.974 | 1.975 | 1.001 | 0.794 | 0.731 |
| optional-chain | 15.387 | 22.037 | 1.432 | 2.033 | 1.999 |
| optional-count | 1.974 | 1.973 | 0.999 | 0.483 | 0.513 |
| order-by-full | 85.073 | 102.073 | 1.200 | 48.254 | 45.403 |
| path-plus | 1.973 | 1.969 | 0.998 | 0.011 | 0.012 |
| predicate-counts | 1.970 | 1.978 | 1.004 | 0.005 | 0.006 |
| predicate-subjects | 1.970 | 1.980 | 1.005 | 0.006 | 0.008 |
| range-topk | 1.975 | 1.977 | 1.001 | 0.660 | 0.526 |
| regex-iri | 4.764 | 4.925 | 1.034 | 3.573 | 3.669 |
| star-join | 5.049 | 6.391 | 1.266 | 1.358 | 1.402 |
| star-lookup | 1.983 | 1.976 | 0.996 | 0.060 | 0.062 |
| subquery-agg | 1.972 | 1.971 | 0.999 | 0.183 | 0.178 |
| two-hop-count | 3.875 | 3.914 | 1.010 | 2.650 | 2.620 |
| types-grouped | 1.970 | 1.948 | 0.988 | 0.007 | 0.013 |
| values-star | 1.948 | 1.978 | 1.016 | 0.015 | 0.019 |
| **geometric mean** | | | 1.032 | | |

## 1.05m-12t: 1.05M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 1.671 | 1.892 | 1.133 | 1.010 | 1.015 |
| class-predicates | 1.686 | 1.912 | 1.134 | 1.022 | 1.025 |
| contains | 1.379 | 1.556 | 1.128 | 0.688 | 0.692 |
| count-all | 0.450 | 0.736 | 1.634 | 0.007 | 0.010 |
| distinct-join | 4.864 | 5.603 | 1.152 | 3.578 | 3.898 |
| distinct-obj | 0.464 | 0.790 | 1.703 | 0.006 | 0.009 |
| employee-docs | 0.647 | 0.939 | 1.452 | 0.045 | 0.053 |
| exists-join | 1.861 | 2.164 | 1.163 | 1.117 | 1.131 |
| export-500k | 250.067 | 301.130 | 1.204 | 2.383 | 0.407 |
| expr-agg-arg | 3.889 | 3.072 | 0.790 | 3.060 | 2.037 |
| expr-bind-group | 1.458 | 1.395 | 0.956 | 0.779 | 0.396 |
| expr-order-key | 1.994 | 2.200 | 1.104 | 1.248 | 1.210 |
| group-avg | 5.081 | 4.782 | 0.941 | 4.244 | 3.646 |
| knows-reach | 12.918 | 12.915 | 1.000 | 11.882 | 11.850 |
| lang-filter | 1.335 | 1.566 | 1.173 | 0.670 | 0.736 |
| minus | 0.986 | 1.211 | 1.228 | 0.351 | 0.276 |
| not-exists | 1.550 | 1.754 | 1.132 | 0.876 | 0.796 |
| optional-chain | 14.620 | 18.778 | 1.284 | 2.117 | 2.064 |
| optional-count | 1.230 | 1.500 | 1.220 | 0.535 | 0.550 |
| order-by-full | 48.451 | 56.598 | 1.168 | 13.750 | 14.336 |
| path-plus | 0.481 | 0.643 | 1.334 | 0.017 | 0.019 |
| predicate-counts | 0.475 | 0.688 | 1.448 | 0.006 | 0.011 |
| predicate-subjects | 0.449 | 0.718 | 1.599 | 0.009 | 0.016 |
| range-topk | 1.535 | 1.521 | 0.991 | 0.753 | 0.578 |
| regex-iri | 1.616 | 1.942 | 1.202 | 0.906 | 1.009 |
| star-join | 4.923 | 6.662 | 1.353 | 1.522 | 1.604 |
| star-lookup | 0.772 | 1.048 | 1.358 | 0.083 | 0.087 |
| subquery-agg | 0.820 | 1.077 | 1.313 | 0.220 | 0.224 |
| two-hop-count | 1.947 | 2.277 | 1.169 | 1.172 | 1.204 |
| types-grouped | 0.526 | 0.832 | 1.583 | 0.011 | 0.019 |
| values-star | 0.639 | 0.873 | 1.366 | 0.019 | 0.026 |
| **geometric mean** | | | 1.222 | | |

## 10.5m-1t: 10.5M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.345 | 11.755 | 1.036 | 10.005 | 10.421 |
| class-predicates | 11.412 | 11.868 | 1.040 | 10.055 | 10.463 |
| contains | 25.026 | 25.291 | 1.011 | 23.818 | 24.099 |
| count-all | 1.974 | 1.974 | 1.000 | 0.005 | 0.007 |
| distinct-join | 44.389 | 46.221 | 1.041 | 38.933 | 38.640 |
| distinct-obj | 1.974 | 1.956 | 0.991 | 0.004 | 0.005 |
| employee-docs | 1.950 | 1.981 | 1.016 | 0.040 | 0.047 |
| exists-join | 9.957 | 9.877 | 0.992 | 8.589 | 8.562 |
| export-500k | 273.008 | 368.998 | 1.352 | 2.262 | 0.445 |
| expr-agg-arg | 30.873 | 24.305 | 0.787 | 29.460 | 22.960 |
| expr-bind-group | 6.912 | 4.562 | 0.660 | 5.597 | 3.298 |
| expr-order-key | 13.976 | 11.617 | 0.831 | 11.981 | 10.404 |
| group-avg | 48.581 | 40.819 | 0.840 | 46.975 | 39.318 |
| knows-reach | 142.122 | 138.412 | 0.974 | 140.772 | 137.162 |
| lang-filter | 17.125 | 17.069 | 0.997 | 15.938 | 15.828 |
| minus | 4.470 | 3.673 | 0.822 | 3.143 | 2.423 |
| not-exists | 10.954 | 9.900 | 0.904 | 9.750 | 8.618 |
| optional-chain | 151.012 | 220.453 | 1.460 | 33.439 | 23.099 |
| optional-count | 6.616 | 6.232 | 0.942 | 5.278 | 4.970 |
| order-by-full | 1191.555 | 1259.525 | 1.057 | 824.043 | 698.021 |
| path-plus | 1.973 | 1.975 | 1.001 | 0.008 | 0.009 |
| predicate-counts | 1.972 | 1.958 | 0.993 | 0.004 | 0.006 |
| predicate-subjects | 1.976 | 1.982 | 1.003 | 0.006 | 0.008 |
| range-topk | 7.151 | 6.775 | 0.947 | 5.796 | 5.068 |
| regex-iri | 37.236 | 37.959 | 1.019 | 36.020 | 36.668 |
| star-join | 37.001 | 53.469 | 1.445 | 13.711 | 15.259 |
| star-lookup | 1.983 | 1.986 | 1.002 | 0.051 | 0.054 |
| subquery-agg | 2.833 | 2.902 | 1.025 | 1.643 | 1.631 |
| two-hop-count | 29.203 | 29.249 | 1.002 | 27.882 | 27.919 |
| types-grouped | 1.945 | 1.976 | 1.016 | 0.007 | 0.013 |
| values-star | 1.977 | 1.977 | 1.000 | 0.015 | 0.019 |
| **geometric mean** | | | 0.995 | | |

## 10.5m-12t: 10.5M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.300 | 11.817 | 1.046 | 10.221 | 10.645 |
| class-predicates | 11.357 | 11.883 | 1.046 | 10.207 | 10.746 |
| contains | 4.777 | 5.121 | 1.072 | 3.982 | 4.101 |
| count-all | 0.418 | 0.754 | 1.804 | 0.006 | 0.011 |
| distinct-join | 44.997 | 45.086 | 1.002 | 39.205 | 38.759 |
| distinct-obj | 0.421 | 0.792 | 1.881 | 0.005 | 0.009 |
| employee-docs | 0.665 | 0.945 | 1.422 | 0.059 | 0.069 |
| exists-join | 9.855 | 9.960 | 1.011 | 8.911 | 8.807 |
| export-500k | 257.729 | 295.261 | 1.146 | 2.662 | 0.419 |
| expr-agg-arg | 30.461 | 25.002 | 0.821 | 29.261 | 23.778 |
| expr-bind-group | 5.515 | 4.507 | 0.817 | 4.687 | 3.444 |
| expr-order-key | 12.680 | 11.883 | 0.937 | 10.762 | 10.852 |
| group-avg | 51.421 | 42.137 | 0.819 | 50.175 | 40.720 |
| knows-reach | 147.322 | 143.870 | 0.977 | 145.798 | 142.581 |
| lang-filter | 3.574 | 3.785 | 1.059 | 2.804 | 2.861 |
| minus | 4.110 | 3.540 | 0.862 | 3.320 | 2.526 |
| not-exists | 11.006 | 10.280 | 0.934 | 9.988 | 9.200 |
| optional-chain | 173.496 | 183.644 | 1.058 | 40.337 | 30.259 |
| optional-count | 6.332 | 6.205 | 0.980 | 5.509 | 5.255 |
| order-by-full | 639.828 | 691.200 | 1.080 | 262.736 | 280.187 |
| path-plus | 0.457 | 0.806 | 1.763 | 0.011 | 0.014 |
| predicate-counts | 0.442 | 0.794 | 1.795 | 0.006 | 0.010 |
| predicate-subjects | 0.449 | 0.790 | 1.760 | 0.008 | 0.014 |
| range-topk | 3.553 | 3.944 | 1.110 | 2.594 | 2.779 |
| regex-iri | 6.819 | 7.364 | 1.080 | 6.016 | 6.296 |
| star-join | 34.797 | 45.096 | 1.296 | 13.685 | 14.170 |
| star-lookup | 0.766 | 1.098 | 1.433 | 0.066 | 0.079 |
| subquery-agg | 1.458 | 1.964 | 1.347 | 0.711 | 0.810 |
| two-hop-count | 12.503 | 12.569 | 1.005 | 11.308 | 11.280 |
| types-grouped | 0.533 | 0.844 | 1.583 | 0.010 | 0.018 |
| values-star | 0.651 | 0.925 | 1.423 | 0.020 | 0.026 |
| **geometric mean** | | | 1.167 | | |

Times are medians over every timed request of the client's wall time (curl time_total) and of the server's execMs. The ratio is of the wall times.
