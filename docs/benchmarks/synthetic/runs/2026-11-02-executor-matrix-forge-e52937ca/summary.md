# Eager-against-cursor matrix

- host: forge
- binary commit: e52937ca
- binary sha256: b403198489322036131b9e64908761ce12dbd9e67087cc90dca38943fe6b172e
- started: 2026-11-02T03:37:44Z
- ended: 2026-11-02T06:03:02Z
- settings: 31 queries, scales 100000 1000000 people, threads 12 1, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server

Every cell's eager and streaming answers agree.

## 1.05m-1t: 1.05M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 2.795 | 2.794 | 1.000 | 0.929 | 0.917 |
| class-predicates | 2.792 | 2.796 | 1.001 | 0.932 | 0.929 |
| contains | 3.563 | 3.688 | 1.035 | 2.410 | 2.473 |
| count-all | 1.951 | 1.950 | 0.999 | 0.005 | 0.006 |
| distinct-join | 5.051 | 5.542 | 1.097 | 3.488 | 3.643 |
| distinct-obj | 1.968 | 1.883 | 0.957 | 0.004 | 0.005 |
| employee-docs | 1.978 | 1.980 | 1.001 | 0.036 | 0.040 |
| exists-join | 2.797 | 2.798 | 1.000 | 1.025 | 1.039 |
| export-500k | 234.044 | 364.969 | 1.559 | 2.247 | 0.428 |
| expr-agg-arg | 3.837 | 3.358 | 0.875 | 2.548 | 1.899 |
| expr-bind-group | 1.972 | 1.981 | 1.004 | 0.593 | 0.369 |
| expr-order-key | 2.797 | 2.798 | 1.000 | 1.184 | 1.111 |
| group-avg | 5.457 | 4.790 | 0.878 | 4.012 | 3.446 |
| knows-reach | 12.759 | 12.571 | 0.985 | 11.531 | 11.361 |
| lang-filter | 2.796 | 2.808 | 1.004 | 1.606 | 1.627 |
| minus | 1.971 | 1.972 | 1.000 | 0.303 | 0.259 |
| not-exists | 1.974 | 1.985 | 1.006 | 0.799 | 0.750 |
| optional-chain | 14.130 | 22.043 | 1.560 | 1.991 | 1.969 |
| optional-count | 1.971 | 1.971 | 1.000 | 0.488 | 0.519 |
| order-by-full | 84.019 | 101.418 | 1.207 | 48.885 | 45.132 |
| path-plus | 1.966 | 1.972 | 1.003 | 0.011 | 0.012 |
| predicate-counts | 1.930 | 1.978 | 1.024 | 0.005 | 0.006 |
| predicate-subjects | 1.937 | 1.976 | 1.020 | 0.007 | 0.008 |
| range-topk | 1.972 | 1.971 | 1.000 | 0.664 | 0.528 |
| regex-iri | 4.705 | 4.793 | 1.019 | 3.543 | 3.678 |
| star-join | 4.865 | 6.455 | 1.327 | 1.383 | 1.423 |
| star-lookup | 1.980 | 1.983 | 1.002 | 0.061 | 0.066 |
| subquery-agg | 1.969 | 1.971 | 1.001 | 0.188 | 0.183 |
| two-hop-count | 3.850 | 3.893 | 1.011 | 2.620 | 2.615 |
| types-grouped | 1.276 | 1.973 | 1.546 | 0.007 | 0.012 |
| values-star | 1.974 | 1.977 | 1.001 | 0.015 | 0.019 |
| **geometric mean** | | | 1.056 | | |

## 1.05m-12t: 1.05M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 1.619 | 1.985 | 1.226 | 0.973 | 1.005 |
| class-predicates | 1.676 | 1.989 | 1.186 | 0.981 | 1.026 |
| contains | 1.419 | 1.714 | 1.208 | 0.718 | 0.750 |
| count-all | 0.498 | 0.841 | 1.688 | 0.007 | 0.010 |
| distinct-join | 4.742 | 5.687 | 1.199 | 3.529 | 3.876 |
| distinct-obj | 0.505 | 0.820 | 1.624 | 0.006 | 0.009 |
| employee-docs | 0.676 | 0.984 | 1.456 | 0.049 | 0.057 |
| exists-join | 1.841 | 2.205 | 1.198 | 1.091 | 1.135 |
| export-500k | 220.028 | 295.875 | 1.345 | 2.423 | 0.408 |
| expr-agg-arg | 3.844 | 3.111 | 0.809 | 3.002 | 2.010 |
| expr-bind-group | 1.560 | 1.389 | 0.890 | 0.855 | 0.393 |
| expr-order-key | 2.057 | 2.177 | 1.058 | 1.289 | 1.208 |
| group-avg | 5.314 | 4.729 | 0.890 | 4.480 | 3.591 |
| knows-reach | 12.941 | 12.733 | 0.984 | 11.838 | 11.688 |
| lang-filter | 1.409 | 1.664 | 1.181 | 0.705 | 0.748 |
| minus | 1.010 | 1.224 | 1.211 | 0.352 | 0.281 |
| not-exists | 1.577 | 1.679 | 1.065 | 0.865 | 0.799 |
| optional-chain | 13.739 | 18.934 | 1.378 | 2.136 | 2.078 |
| optional-count | 1.260 | 1.549 | 1.229 | 0.542 | 0.556 |
| order-by-full | 46.047 | 58.524 | 1.271 | 14.096 | 15.752 |
| path-plus | 0.534 | 0.772 | 1.446 | 0.018 | 0.022 |
| predicate-counts | 0.528 | 0.847 | 1.605 | 0.007 | 0.011 |
| predicate-subjects | 0.486 | 0.810 | 1.666 | 0.010 | 0.015 |
| range-topk | 1.501 | 1.664 | 1.109 | 0.713 | 0.569 |
| regex-iri | 1.671 | 1.979 | 1.184 | 0.922 | 0.997 |
| star-join | 4.761 | 6.753 | 1.419 | 1.541 | 1.651 |
| star-lookup | 0.798 | 1.155 | 1.447 | 0.084 | 0.089 |
| subquery-agg | 0.849 | 1.104 | 1.300 | 0.239 | 0.222 |
| two-hop-count | 2.039 | 2.251 | 1.104 | 1.268 | 1.249 |
| types-grouped | 0.578 | 0.830 | 1.436 | 0.011 | 0.019 |
| values-star | 0.688 | 0.871 | 1.265 | 0.020 | 0.026 |
| **geometric mean** | | | 1.241 | | |

## 10.5m-1t: 10.5M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.140 | 11.587 | 1.040 | 9.832 | 10.273 |
| class-predicates | 11.153 | 11.666 | 1.046 | 9.848 | 10.245 |
| contains | 24.556 | 25.316 | 1.031 | 23.466 | 24.045 |
| count-all | 1.031 | 1.970 | 1.910 | 0.006 | 0.007 |
| distinct-join | 43.444 | 45.758 | 1.053 | 38.249 | 37.922 |
| distinct-obj | 1.879 | 1.899 | 1.011 | 0.004 | 0.005 |
| employee-docs | 1.979 | 1.980 | 1.001 | 0.048 | 0.054 |
| exists-join | 9.919 | 9.954 | 1.003 | 8.564 | 8.560 |
| export-500k | 241.608 | 365.974 | 1.515 | 2.218 | 0.442 |
| expr-agg-arg | 30.915 | 24.308 | 0.786 | 29.446 | 23.001 |
| expr-bind-group | 6.847 | 4.590 | 0.670 | 5.570 | 3.299 |
| expr-order-key | 13.933 | 11.841 | 0.850 | 11.881 | 10.535 |
| group-avg | 49.127 | 40.904 | 0.833 | 47.515 | 39.342 |
| knows-reach | 142.966 | 138.848 | 0.971 | 141.565 | 137.470 |
| lang-filter | 16.900 | 17.176 | 1.016 | 15.654 | 15.909 |
| minus | 4.665 | 3.595 | 0.771 | 3.171 | 2.438 |
| not-exists | 10.991 | 10.060 | 0.915 | 9.770 | 8.869 |
| optional-chain | 135.058 | 217.966 | 1.614 | 29.844 | 22.853 |
| optional-count | 6.762 | 6.284 | 0.929 | 5.323 | 5.016 |
| order-by-full | 1010.510 | 1144.985 | 1.133 | 677.315 | 596.671 |
| path-plus | 1.970 | 1.971 | 1.001 | 0.009 | 0.009 |
| predicate-counts | 1.930 | 1.979 | 1.025 | 0.005 | 0.006 |
| predicate-subjects | 1.969 | 1.980 | 1.006 | 0.006 | 0.009 |
| range-topk | 7.174 | 6.476 | 0.903 | 5.851 | 5.095 |
| regex-iri | 36.688 | 38.055 | 1.037 | 35.546 | 36.740 |
| star-join | 35.047 | 52.969 | 1.511 | 13.891 | 14.512 |
| star-lookup | 1.983 | 1.982 | 0.999 | 0.056 | 0.060 |
| subquery-agg | 2.851 | 2.908 | 1.020 | 1.653 | 1.639 |
| two-hop-count | 28.959 | 29.069 | 1.004 | 27.661 | 27.732 |
| types-grouped | 1.962 | 1.973 | 1.006 | 0.007 | 0.013 |
| values-star | 1.974 | 1.977 | 1.001 | 0.017 | 0.020 |
| **geometric mean** | | | 1.027 | | |

## 10.5m-12t: 10.5M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.274 | 11.575 | 1.027 | 10.178 | 10.497 |
| class-predicates | 11.186 | 11.627 | 1.040 | 10.116 | 10.517 |
| contains | 4.774 | 5.053 | 1.059 | 4.004 | 4.080 |
| count-all | 0.499 | 0.747 | 1.495 | 0.008 | 0.012 |
| distinct-join | 43.656 | 44.433 | 1.018 | 38.520 | 38.051 |
| distinct-obj | 0.504 | 0.746 | 1.481 | 0.006 | 0.009 |
| employee-docs | 0.706 | 0.946 | 1.340 | 0.068 | 0.079 |
| exists-join | 9.789 | 9.988 | 1.020 | 8.852 | 8.825 |
| export-500k | 228.080 | 292.973 | 1.285 | 2.480 | 0.409 |
| expr-agg-arg | 29.171 | 24.481 | 0.839 | 28.034 | 23.225 |
| expr-bind-group | 5.639 | 4.473 | 0.793 | 4.812 | 3.436 |
| expr-order-key | 12.107 | 11.844 | 0.978 | 10.323 | 10.784 |
| group-avg | 49.380 | 41.511 | 0.841 | 47.820 | 40.089 |
| knows-reach | 144.315 | 141.678 | 0.982 | 142.627 | 140.373 |
| lang-filter | 3.577 | 3.812 | 1.066 | 2.806 | 2.869 |
| minus | 4.050 | 3.550 | 0.877 | 3.238 | 2.603 |
| not-exists | 10.841 | 10.002 | 0.923 | 9.798 | 8.934 |
| optional-chain | 130.175 | 171.385 | 1.317 | 28.455 | 25.517 |
| optional-count | 6.247 | 6.236 | 0.998 | 5.399 | 5.220 |
| order-by-full | 577.207 | 668.779 | 1.159 | 239.272 | 266.403 |
| path-plus | 0.522 | 0.770 | 1.476 | 0.014 | 0.014 |
| predicate-counts | 0.523 | 0.801 | 1.532 | 0.007 | 0.011 |
| predicate-subjects | 0.435 | 0.792 | 1.820 | 0.008 | 0.016 |
| range-topk | 3.313 | 3.442 | 1.039 | 2.456 | 2.247 |
| regex-iri | 6.793 | 7.359 | 1.083 | 5.991 | 6.239 |
| star-join | 32.124 | 43.770 | 1.363 | 13.063 | 13.501 |
| star-lookup | 0.792 | 1.139 | 1.438 | 0.073 | 0.082 |
| subquery-agg | 1.514 | 2.008 | 1.326 | 0.757 | 0.830 |
| two-hop-count | 12.369 | 12.401 | 1.003 | 11.194 | 11.240 |
| types-grouped | 0.565 | 0.814 | 1.441 | 0.010 | 0.020 |
| values-star | 0.667 | 0.956 | 1.432 | 0.020 | 0.027 |
| **geometric mean** | | | 1.151 | | |

Times are medians over every timed request of the client's wall time (curl time_total) and of the server's execMs. The ratio is of the wall times.
