# Eager-against-cursor matrix

- host: forge
- binary commit: cc94ac7f
- binary sha256: 9efa1cd4655ff0d75af758bf0bb0f54ff5950f152b61b3e0afb765ff63e0fcba
- started: 2026-11-18T07:04:20Z
- ended: 2026-11-18T09:27:28Z
- settings: 31 queries, scales 100000 1000000 people, threads 12 1, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server

Every cell's eager and streaming answers agree.

## 1.05m-1t: 1.05M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 2.429 | 2.795 | 1.151 | 0.928 | 0.916 |
| class-predicates | 2.640 | 2.800 | 1.061 | 0.934 | 0.929 |
| contains | 3.671 | 3.727 | 1.015 | 2.510 | 2.514 |
| count-all | 1.954 | 1.970 | 1.008 | 0.005 | 0.006 |
| distinct-join | 5.032 | 5.975 | 1.187 | 3.459 | 3.700 |
| distinct-obj | 1.495 | 1.970 | 1.318 | 0.004 | 0.005 |
| employee-docs | 1.976 | 1.982 | 1.003 | 0.038 | 0.038 |
| exists-join | 2.801 | 2.800 | 0.999 | 1.027 | 1.032 |
| export-500k | 250.029 | 338.984 | 1.356 | 2.153 | 0.414 |
| expr-agg-arg | 3.862 | 3.204 | 0.830 | 2.598 | 1.895 |
| expr-bind-group | 1.971 | 1.978 | 1.004 | 0.604 | 0.371 |
| expr-order-key | 2.799 | 2.801 | 1.001 | 1.146 | 1.142 |
| group-avg | 5.320 | 4.510 | 0.848 | 3.952 | 3.170 |
| knows-reach | 12.800 | 12.659 | 0.989 | 11.514 | 11.339 |
| lang-filter | 2.802 | 2.837 | 1.012 | 1.640 | 1.641 |
| minus | 1.968 | 1.972 | 1.002 | 0.312 | 0.257 |
| not-exists | 1.974 | 1.974 | 1.000 | 0.800 | 0.740 |
| optional-chain | 15.129 | 21.047 | 1.391 | 2.002 | 1.951 |
| optional-count | 1.972 | 1.974 | 1.001 | 0.488 | 0.516 |
| order-by-full | 77.299 | 95.005 | 1.229 | 41.168 | 41.505 |
| path-plus | 1.346 | 1.975 | 1.467 | 0.012 | 0.012 |
| predicate-counts | 1.461 | 1.982 | 1.357 | 0.005 | 0.006 |
| predicate-subjects | 1.628 | 1.980 | 1.216 | 0.007 | 0.009 |
| range-topk | 1.973 | 1.974 | 1.001 | 0.535 | 0.529 |
| regex-iri | 4.654 | 4.702 | 1.010 | 3.481 | 3.480 |
| star-join | 5.069 | 6.111 | 1.205 | 1.411 | 1.425 |
| star-lookup | 1.984 | 1.986 | 1.001 | 0.063 | 0.064 |
| subquery-agg | 1.973 | 1.974 | 1.001 | 0.187 | 0.186 |
| two-hop-count | 3.873 | 3.921 | 1.012 | 2.646 | 2.628 |
| types-grouped | 1.601 | 1.972 | 1.232 | 0.009 | 0.013 |
| values-star | 1.977 | 1.980 | 1.002 | 0.015 | 0.020 |
| **geometric mean** | | | 1.083 | | |

## 1.05m-12t: 1.05M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 1.667 | 1.851 | 1.110 | 0.980 | 0.977 |
| class-predicates | 1.700 | 1.854 | 1.091 | 1.003 | 0.985 |
| contains | 1.470 | 1.607 | 1.093 | 0.730 | 0.759 |
| count-all | 0.449 | 0.723 | 1.612 | 0.007 | 0.009 |
| distinct-join | 4.849 | 5.688 | 1.173 | 3.579 | 3.766 |
| distinct-obj | 0.480 | 0.771 | 1.606 | 0.007 | 0.008 |
| employee-docs | 0.688 | 0.887 | 1.288 | 0.049 | 0.053 |
| exists-join | 1.861 | 2.114 | 1.136 | 1.104 | 1.101 |
| export-500k | 236.463 | 270.645 | 1.145 | 2.330 | 0.411 |
| expr-agg-arg | 3.766 | 2.992 | 0.794 | 2.936 | 2.007 |
| expr-bind-group | 1.542 | 1.286 | 0.835 | 0.823 | 0.398 |
| expr-order-key | 1.980 | 2.191 | 1.106 | 1.234 | 1.280 |
| group-avg | 5.027 | 4.462 | 0.888 | 4.141 | 3.401 |
| knows-reach | 12.796 | 12.763 | 0.997 | 11.768 | 11.655 |
| lang-filter | 1.458 | 1.538 | 1.055 | 0.754 | 0.729 |
| minus | 1.035 | 1.192 | 1.151 | 0.340 | 0.278 |
| not-exists | 1.562 | 1.758 | 1.125 | 0.856 | 0.797 |
| optional-chain | 14.561 | 17.710 | 1.216 | 2.129 | 2.097 |
| optional-count | 1.217 | 1.381 | 1.135 | 0.536 | 0.558 |
| order-by-full | 43.306 | 48.442 | 1.119 | 8.920 | 9.247 |
| path-plus | 0.499 | 0.794 | 1.589 | 0.019 | 0.020 |
| predicate-counts | 0.472 | 0.835 | 1.768 | 0.007 | 0.010 |
| predicate-subjects | 0.442 | 0.824 | 1.864 | 0.009 | 0.015 |
| range-topk | 1.377 | 1.554 | 1.128 | 0.572 | 0.567 |
| regex-iri | 1.712 | 1.792 | 1.046 | 0.986 | 0.958 |
| star-join | 5.020 | 5.739 | 1.143 | 1.614 | 1.643 |
| star-lookup | 0.819 | 1.080 | 1.319 | 0.084 | 0.094 |
| subquery-agg | 0.859 | 0.986 | 1.149 | 0.233 | 0.225 |
| two-hop-count | 2.010 | 2.251 | 1.120 | 1.214 | 1.253 |
| types-grouped | 0.541 | 0.778 | 1.439 | 0.012 | 0.019 |
| values-star | 0.675 | 0.877 | 1.299 | 0.019 | 0.028 |
| **geometric mean** | | | 1.188 | | |

## 10.5m-1t: 10.5M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.252 | 11.602 | 1.031 | 9.874 | 10.269 |
| class-predicates | 11.244 | 11.657 | 1.037 | 9.850 | 10.274 |
| contains | 25.625 | 25.700 | 1.003 | 24.418 | 24.460 |
| count-all | 1.889 | 1.974 | 1.045 | 0.006 | 0.007 |
| distinct-join | 43.716 | 44.942 | 1.028 | 38.489 | 37.485 |
| distinct-obj | 1.827 | 1.976 | 1.082 | 0.005 | 0.005 |
| employee-docs | 1.981 | 1.984 | 1.002 | 0.049 | 0.055 |
| exists-join | 9.870 | 9.873 | 1.000 | 8.484 | 8.472 |
| export-500k | 256.046 | 336.033 | 1.312 | 2.205 | 0.430 |
| expr-agg-arg | 30.605 | 23.742 | 0.776 | 29.223 | 22.778 |
| expr-bind-group | 6.980 | 4.579 | 0.656 | 5.753 | 3.298 |
| expr-order-key | 12.907 | 12.301 | 0.953 | 10.913 | 10.778 |
| group-avg | 46.930 | 38.327 | 0.817 | 45.314 | 36.706 |
| knows-reach | 140.928 | 138.226 | 0.981 | 139.616 | 136.913 |
| lang-filter | 17.261 | 17.263 | 1.000 | 16.005 | 15.980 |
| minus | 4.533 | 3.689 | 0.814 | 3.251 | 2.441 |
| not-exists | 10.807 | 9.956 | 0.921 | 9.440 | 8.644 |
| optional-chain | 142.572 | 207.165 | 1.453 | 27.693 | 22.519 |
| optional-count | 6.627 | 6.277 | 0.947 | 5.318 | 4.999 |
| order-by-full | 865.074 | 1029.990 | 1.191 | 514.724 | 517.212 |
| path-plus | 1.973 | 1.976 | 1.001 | 0.010 | 0.009 |
| predicate-counts | 1.969 | 1.984 | 1.008 | 0.005 | 0.007 |
| predicate-subjects | 1.969 | 1.982 | 1.007 | 0.008 | 0.009 |
| range-topk | 6.423 | 6.430 | 1.001 | 5.080 | 5.042 |
| regex-iri | 35.725 | 35.728 | 1.000 | 34.496 | 34.519 |
| star-join | 37.048 | 51.108 | 1.380 | 14.067 | 14.800 |
| star-lookup | 1.986 | 1.986 | 1.000 | 0.057 | 0.061 |
| subquery-agg | 2.907 | 2.915 | 1.002 | 1.698 | 1.652 |
| two-hop-count | 29.109 | 29.316 | 1.007 | 27.739 | 27.841 |
| types-grouped | 1.972 | 1.976 | 1.002 | 0.009 | 0.013 |
| values-star | 1.981 | 1.982 | 1.001 | 0.016 | 0.021 |
| **geometric mean** | | | 1.003 | | |

## 10.5m-12t: 10.5M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.285 | 11.634 | 1.031 | 10.213 | 10.540 |
| class-predicates | 11.261 | 11.741 | 1.043 | 10.147 | 10.562 |
| contains | 5.011 | 5.162 | 1.030 | 4.209 | 4.198 |
| count-all | 0.489 | 0.740 | 1.512 | 0.009 | 0.011 |
| distinct-join | 44.702 | 43.724 | 0.978 | 39.015 | 37.799 |
| distinct-obj | 0.474 | 0.800 | 1.685 | 0.006 | 0.009 |
| employee-docs | 0.718 | 0.978 | 1.363 | 0.067 | 0.080 |
| exists-join | 9.931 | 10.010 | 1.008 | 8.969 | 8.834 |
| export-500k | 242.538 | 266.629 | 1.099 | 2.583 | 0.423 |
| expr-agg-arg | 29.963 | 24.415 | 0.815 | 28.797 | 23.193 |
| expr-bind-group | 5.678 | 4.463 | 0.786 | 4.826 | 3.428 |
| expr-order-key | 11.899 | 12.267 | 1.031 | 10.106 | 11.111 |
| group-avg | 47.573 | 38.805 | 0.816 | 46.154 | 37.578 |
| knows-reach | 144.935 | 141.739 | 0.978 | 143.563 | 140.487 |
| lang-filter | 3.702 | 3.889 | 1.051 | 2.907 | 2.917 |
| minus | 4.103 | 3.567 | 0.869 | 3.302 | 2.571 |
| not-exists | 11.244 | 10.097 | 0.898 | 10.163 | 9.099 |
| optional-chain | 155.953 | 161.172 | 1.033 | 45.234 | 25.478 |
| optional-count | 6.354 | 6.244 | 0.983 | 5.464 | 5.180 |
| order-by-full | 528.413 | 555.395 | 1.051 | 187.553 | 191.771 |
| path-plus | 0.522 | 0.776 | 1.488 | 0.015 | 0.015 |
| predicate-counts | 0.520 | 0.821 | 1.581 | 0.008 | 0.011 |
| predicate-subjects | 0.448 | 0.800 | 1.787 | 0.009 | 0.015 |
| range-topk | 3.078 | 3.312 | 1.076 | 2.175 | 2.221 |
| regex-iri | 7.008 | 7.217 | 1.030 | 6.202 | 6.220 |
| star-join | 34.535 | 42.565 | 1.233 | 13.719 | 14.027 |
| star-lookup | 0.812 | 1.084 | 1.334 | 0.073 | 0.081 |
| subquery-agg | 1.590 | 1.864 | 1.173 | 0.806 | 0.836 |
| two-hop-count | 12.497 | 12.439 | 0.995 | 11.323 | 11.256 |
| types-grouped | 0.581 | 0.845 | 1.454 | 0.012 | 0.020 |
| values-star | 0.677 | 0.915 | 1.351 | 0.021 | 0.032 |
| **geometric mean** | | | 1.120 | | |

Times are medians over every timed request of the client's wall time (curl time_total) and of the server's execMs. The ratio is of the wall times.
