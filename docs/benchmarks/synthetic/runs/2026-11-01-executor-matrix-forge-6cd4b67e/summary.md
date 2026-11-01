# Eager-against-cursor matrix

- host: forge
- binary commit: 04d49b57
- binary sha256: 34aa23f8e08949fd166863059128e1afa3108501fb171b65d75494b0ba09e0fd
- started: 2026-11-01T10:02:19Z
- ended: 2026-11-01T12:27:47Z
- settings: 31 queries, scales 100000 1000000 people, threads 12 1, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server

Every cell's eager and streaming answers agree.

## 1.05m-1t: 1.05M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 2.795 | 2.796 | 1.001 | 0.933 | 0.927 |
| class-predicates | 2.796 | 2.798 | 1.001 | 0.941 | 0.939 |
| contains | 3.600 | 3.667 | 1.019 | 2.444 | 2.467 |
| count-all | 1.153 | 1.972 | 1.710 | 0.004 | 0.005 |
| distinct-join | 5.051 | 5.580 | 1.105 | 3.472 | 3.683 |
| distinct-obj | 1.891 | 1.969 | 1.041 | 0.004 | 0.005 |
| employee-docs | 1.981 | 1.981 | 1.000 | 0.033 | 0.037 |
| exists-join | 2.800 | 2.801 | 1.001 | 1.023 | 1.024 |
| export-500k | 264.968 | 370.170 | 1.397 | 2.280 | 0.411 |
| expr-agg-arg | 3.859 | 3.184 | 0.825 | 2.634 | 1.903 |
| expr-bind-group | 1.973 | 1.982 | 1.005 | 0.588 | 0.367 |
| expr-order-key | 2.802 | 2.801 | 1.000 | 1.177 | 1.105 |
| group-avg | 5.851 | 4.790 | 0.819 | 4.080 | 3.465 |
| knows-reach | 12.761 | 12.598 | 0.987 | 11.541 | 11.367 |
| lang-filter | 2.797 | 2.803 | 1.002 | 1.624 | 1.618 |
| minus | 1.973 | 1.973 | 1.000 | 0.307 | 0.253 |
| not-exists | 1.972 | 1.972 | 1.000 | 0.793 | 0.727 |
| optional-chain | 15.133 | 22.040 | 1.456 | 1.980 | 1.959 |
| optional-count | 1.973 | 1.973 | 1.000 | 0.488 | 0.516 |
| order-by-full | 85.541 | 101.487 | 1.186 | 48.477 | 45.227 |
| path-plus | 1.964 | 1.372 | 0.699 | 0.010 | 0.012 |
| predicate-counts | 1.302 | 1.982 | 1.522 | 0.004 | 0.006 |
| predicate-subjects | 1.972 | 1.642 | 0.833 | 0.006 | 0.008 |
| range-topk | 1.972 | 1.974 | 1.001 | 0.659 | 0.527 |
| regex-iri | 4.739 | 4.912 | 1.037 | 3.572 | 3.665 |
| star-join | 5.000 | 6.407 | 1.282 | 1.352 | 1.392 |
| star-lookup | 1.983 | 1.985 | 1.001 | 0.059 | 0.062 |
| subquery-agg | 1.972 | 1.973 | 1.001 | 0.183 | 0.181 |
| two-hop-count | 3.850 | 3.886 | 1.009 | 2.625 | 2.615 |
| types-grouped | 1.962 | 1.974 | 1.006 | 0.007 | 0.013 |
| values-star | 1.977 | 1.978 | 1.001 | 0.015 | 0.019 |
| **geometric mean** | | | 1.045 | | |

## 1.05m-12t: 1.05M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 1.670 | 1.970 | 1.180 | 0.996 | 1.015 |
| class-predicates | 1.690 | 1.978 | 1.171 | 1.001 | 1.014 |
| contains | 1.435 | 1.585 | 1.104 | 0.752 | 0.721 |
| count-all | 0.483 | 0.775 | 1.603 | 0.007 | 0.009 |
| distinct-join | 4.848 | 5.649 | 1.165 | 3.595 | 3.905 |
| distinct-obj | 0.496 | 0.767 | 1.545 | 0.006 | 0.008 |
| employee-docs | 0.658 | 0.909 | 1.381 | 0.047 | 0.051 |
| exists-join | 1.843 | 2.133 | 1.158 | 1.088 | 1.112 |
| export-500k | 246.856 | 298.730 | 1.210 | 2.554 | 0.408 |
| expr-agg-arg | 3.829 | 3.030 | 0.791 | 2.999 | 1.990 |
| expr-bind-group | 1.474 | 1.353 | 0.919 | 0.752 | 0.397 |
| expr-order-key | 2.034 | 2.175 | 1.069 | 1.282 | 1.198 |
| group-avg | 5.188 | 4.740 | 0.914 | 4.346 | 3.612 |
| knows-reach | 12.911 | 12.703 | 0.984 | 11.824 | 11.588 |
| lang-filter | 1.359 | 1.599 | 1.177 | 0.655 | 0.725 |
| minus | 0.978 | 1.155 | 1.181 | 0.345 | 0.275 |
| not-exists | 1.543 | 1.713 | 1.110 | 0.878 | 0.783 |
| optional-chain | 14.790 | 18.788 | 1.270 | 2.100 | 2.064 |
| optional-count | 1.240 | 1.426 | 1.150 | 0.542 | 0.550 |
| order-by-full | 48.822 | 57.093 | 1.169 | 13.953 | 14.287 |
| path-plus | 0.510 | 0.822 | 1.613 | 0.017 | 0.022 |
| predicate-counts | 0.480 | 0.849 | 1.766 | 0.006 | 0.011 |
| predicate-subjects | 0.465 | 0.835 | 1.796 | 0.010 | 0.015 |
| range-topk | 1.512 | 1.637 | 1.083 | 0.719 | 0.558 |
| regex-iri | 1.648 | 2.026 | 1.229 | 0.922 | 0.993 |
| star-join | 4.955 | 6.658 | 1.344 | 1.566 | 1.604 |
| star-lookup | 0.817 | 1.087 | 1.330 | 0.081 | 0.090 |
| subquery-agg | 0.840 | 1.064 | 1.267 | 0.225 | 0.217 |
| two-hop-count | 2.029 | 2.372 | 1.170 | 1.235 | 1.316 |
| types-grouped | 0.558 | 0.821 | 1.470 | 0.011 | 0.018 |
| values-star | 0.676 | 0.944 | 1.396 | 0.020 | 0.025 |
| **geometric mean** | | | 1.228 | | |

## 10.5m-1t: 10.5M triples, 1 thread

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.283 | 11.744 | 1.041 | 9.982 | 10.437 |
| class-predicates | 11.348 | 11.791 | 1.039 | 9.998 | 10.452 |
| contains | 25.028 | 25.317 | 1.012 | 23.825 | 24.100 |
| count-all | 1.889 | 1.621 | 0.858 | 0.006 | 0.006 |
| distinct-join | 44.484 | 46.126 | 1.037 | 39.055 | 38.578 |
| distinct-obj | 1.917 | 1.945 | 1.015 | 0.004 | 0.005 |
| employee-docs | 1.981 | 1.982 | 1.001 | 0.041 | 0.045 |
| exists-join | 9.921 | 9.924 | 1.000 | 8.573 | 8.587 |
| export-500k | 271.946 | 369.978 | 1.360 | 2.358 | 0.438 |
| expr-agg-arg | 30.704 | 24.008 | 0.782 | 29.354 | 22.898 |
| expr-bind-group | 6.827 | 4.544 | 0.666 | 5.583 | 3.288 |
| expr-order-key | 13.937 | 11.621 | 0.834 | 11.945 | 10.384 |
| group-avg | 48.752 | 40.724 | 0.835 | 47.198 | 39.198 |
| knows-reach | 140.482 | 138.602 | 0.987 | 139.139 | 137.255 |
| lang-filter | 17.095 | 17.051 | 0.997 | 15.871 | 15.814 |
| minus | 4.491 | 3.681 | 0.820 | 3.235 | 2.424 |
| not-exists | 10.745 | 9.911 | 0.922 | 9.511 | 8.580 |
| optional-chain | 145.118 | 218.977 | 1.509 | 27.872 | 23.022 |
| optional-count | 6.579 | 6.267 | 0.953 | 5.325 | 4.993 |
| order-by-full | 1036.165 | 1115.970 | 1.077 | 676.120 | 561.392 |
| path-plus | 1.969 | 1.970 | 1.001 | 0.008 | 0.008 |
| predicate-counts | 1.970 | 1.981 | 1.006 | 0.005 | 0.006 |
| predicate-subjects | 1.937 | 1.981 | 1.023 | 0.006 | 0.008 |
| range-topk | 7.174 | 6.369 | 0.888 | 5.819 | 4.992 |
| regex-iri | 37.239 | 37.963 | 1.019 | 35.995 | 36.653 |
| star-join | 36.090 | 52.075 | 1.443 | 13.588 | 14.283 |
| star-lookup | 1.983 | 1.985 | 1.001 | 0.052 | 0.053 |
| subquery-agg | 2.838 | 2.888 | 1.018 | 1.640 | 1.620 |
| two-hop-count | 29.005 | 29.133 | 1.004 | 27.732 | 27.769 |
| types-grouped | 1.948 | 1.975 | 1.014 | 0.007 | 0.013 |
| values-star | 1.978 | 1.978 | 1.000 | 0.015 | 0.020 |
| **geometric mean** | | | 0.992 | | |

## 10.5m-12t: 10.5M triples, 12 threads

| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |
|---|--:|--:|--:|--:|--:|
| class-predicate-list | 11.258 | 11.790 | 1.047 | 10.189 | 10.706 |
| class-predicates | 11.277 | 11.950 | 1.060 | 10.235 | 10.796 |
| contains | 4.786 | 5.157 | 1.077 | 3.991 | 4.127 |
| count-all | 0.470 | 0.751 | 1.600 | 0.008 | 0.011 |
| distinct-join | 44.566 | 45.123 | 1.012 | 39.181 | 38.912 |
| distinct-obj | 0.476 | 0.750 | 1.576 | 0.005 | 0.008 |
| employee-docs | 0.694 | 0.995 | 1.433 | 0.058 | 0.074 |
| exists-join | 9.947 | 10.058 | 1.011 | 8.983 | 8.941 |
| export-500k | 254.480 | 294.608 | 1.158 | 2.364 | 0.414 |
| expr-agg-arg | 29.543 | 24.390 | 0.826 | 28.400 | 23.143 |
| expr-bind-group | 5.535 | 4.517 | 0.816 | 4.732 | 3.413 |
| expr-order-key | 12.108 | 11.851 | 0.979 | 10.243 | 10.840 |
| group-avg | 49.620 | 41.776 | 0.842 | 48.110 | 40.365 |
| knows-reach | 142.282 | 140.788 | 0.990 | 141.045 | 139.427 |
| lang-filter | 3.614 | 3.938 | 1.090 | 2.827 | 2.924 |
| minus | 4.083 | 3.593 | 0.880 | 3.277 | 2.580 |
| not-exists | 10.643 | 10.152 | 0.954 | 9.689 | 9.085 |
| optional-chain | 140.377 | 169.669 | 1.209 | 27.437 | 24.907 |
| optional-count | 6.223 | 6.245 | 1.004 | 5.374 | 5.201 |
| order-by-full | 518.274 | 557.496 | 1.076 | 181.829 | 158.978 |
| path-plus | 0.482 | 0.796 | 1.652 | 0.013 | 0.015 |
| predicate-counts | 0.494 | 0.807 | 1.634 | 0.007 | 0.010 |
| predicate-subjects | 0.467 | 0.796 | 1.704 | 0.010 | 0.015 |
| range-topk | 3.146 | 3.446 | 1.096 | 2.275 | 2.240 |
| regex-iri | 6.743 | 7.309 | 1.084 | 5.907 | 6.245 |
| star-join | 33.566 | 43.495 | 1.296 | 12.776 | 13.463 |
| star-lookup | 0.782 | 1.031 | 1.319 | 0.066 | 0.073 |
| subquery-agg | 1.458 | 1.967 | 1.349 | 0.713 | 0.806 |
| two-hop-count | 12.367 | 12.451 | 1.007 | 11.241 | 11.270 |
| types-grouped | 0.571 | 0.806 | 1.411 | 0.010 | 0.019 |
| values-star | 0.663 | 0.887 | 1.338 | 0.019 | 0.028 |
| **geometric mean** | | | 1.152 | | |

Times are medians over every timed request of the client's wall time (curl time_total) and of the server's execMs. The ratio is of the wall times.
