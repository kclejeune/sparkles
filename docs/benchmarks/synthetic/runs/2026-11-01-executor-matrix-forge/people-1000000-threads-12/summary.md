# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/ws1a-gates-469-473/bin/sparkles-04d49b57
- binary sha256: 34aa23f8e08949fd166863059128e1afa3108501fb171b65d75494b0ba09e0fd
- binary commit: 04d49b57
- version: sparkles 0.1.0
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-01T04:47:16Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=12, server CPU 0-11, client CPU 12, spinners off, execution eager,streaming
- A: /home/kclejeune/lanes/ws1a-gates-469-473/bin/sparkles-04d49b57
- B: /home/kclejeune/lanes/ws1a-gates-469-473/bin/sparkles-04d49b57
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 31 queries.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 10.221 | 10.645 | 1.041 | +4.1% | 1.044 1.036 | 2.1% | 1.4% |
| class-predicates | 10.207 | 10.746 | 1.053 | +5.3% | 1.063 1.041 | 3.4% | 1.1% |
| contains | 3.982 | 4.101 | 1.030 | +3.0% | 1.021 1.033 | 2.7% | 4.5% |
| count-all | 0.006 | 0.011 | 1.766 | +76.6% | 1.631 1.687 | 34.6% | 74.6% |
| distinct-join | 39.205 | 38.759 | 0.989 | -1.1% | 0.989 0.901 | 15.5% | 2.3% |
| distinct-obj | 0.005 | 0.009 | 1.817 | +81.7% | 1.880 2.044 | 42.5% | 68.1% |
| employee-docs | 0.059 | 0.069 | 1.173 | +17.3% | 1.079 1.236 | 11.3% | 22.5% |
| exists-join | 8.911 | 8.807 | 0.988 | -1.2% | 0.958 1.014 | 7.1% | 9.8% |
| export-500k | 2.662 | 0.419 | 0.158 | -84.2% | 0.108 0.168 | 87.5% | 3.7% |
| expr-agg-arg | 29.261 | 23.778 | 0.813 | -18.7% | 0.810 0.811 | 3.0% | 4.4% |
| expr-bind-group | 4.687 | 3.444 | 0.735 | -26.5% | 0.739 0.724 | 2.7% | 2.3% |
| expr-order-key | 10.762 | 10.852 | 1.008 | +0.8% | 1.025 0.972 | 6.1% | 1.7% |
| group-avg | 50.175 | 40.720 | 0.812 | -18.8% | 0.794 0.828 | 5.5% | 3.5% |
| knows-reach | 145.798 | 142.581 | 0.978 | -2.2% | 0.975 0.993 | 2.1% | 2.9% |
| lang-filter | 2.804 | 2.861 | 1.020 | +2.0% | 1.033 0.999 | 2.9% | 7.0% |
| minus | 3.320 | 2.526 | 0.761 | -23.9% | 0.776 0.772 | 5.5% | 7.2% |
| not-exists | 9.988 | 9.200 | 0.921 | -7.9% | 0.931 0.925 | 6.0% | 5.5% |
| optional-chain | 40.337 | 30.259 | 0.750 | -25.0% | 0.763 0.673 | 32.1% | 6.3% |
| optional-count | 5.509 | 5.255 | 0.954 | -4.6% | 0.949 0.972 | 2.7% | 1.4% |
| order-by-full | 262.736 | 280.187 | 1.066 | +6.6% | 1.121 1.014 | 7.6% | 16.6% |
| path-plus | 0.011 | 0.014 | 1.260 | +26.0% | 1.225 1.295 | 43.6% | 57.3% |
| predicate-counts | 0.006 | 0.010 | 1.850 | +85.0% | 1.985 1.767 | 24.0% | 34.3% |
| predicate-subjects | 0.008 | 0.014 | 1.742 | +74.2% | 1.938 1.593 | 37.3% | 23.9% |
| range-topk | 2.594 | 2.779 | 1.071 | +7.1% | 1.117 1.006 | 7.6% | 32.8% |
| regex-iri | 6.016 | 6.296 | 1.046 | +4.6% | 1.060 1.034 | 2.8% | 1.1% |
| star-join | 13.685 | 14.170 | 1.035 | +3.5% | 1.007 1.138 | 12.1% | 7.5% |
| star-lookup | 0.066 | 0.079 | 1.199 | +19.9% | 1.263 1.153 | 3.2% | 20.1% |
| subquery-agg | 0.711 | 0.810 | 1.140 | +14.0% | 1.180 1.072 | 16.3% | 4.6% |
| two-hop-count | 11.308 | 11.280 | 0.998 | -0.2% | 0.999 0.992 | 2.0% | 4.8% |
| types-grouped | 0.010 | 0.018 | 1.771 | +77.1% | 1.714 1.820 | 8.4% | 49.1% |
| values-star | 0.020 | 0.026 | 1.328 | +32.8% | 1.275 1.314 | 5.9% | 28.5% |
| **geometric mean** | | | 1.031 | +3.1% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.300 | 11.817 | 1.046 | +4.6% | 1.049 1.037 | 2.8% | 1.1% |
| class-predicates | 11.357 | 11.883 | 1.046 | +4.6% | 1.052 1.034 | 2.6% | 2.4% |
| contains | 4.777 | 5.121 | 1.072 | +7.2% | 1.059 1.090 | 3.7% | 3.3% |
| count-all | 0.418 | 0.754 | 1.804 | +80.4% | 1.793 1.867 | 9.6% | 25.0% |
| distinct-join | 44.997 | 45.086 | 1.002 | +0.2% | 1.004 0.937 | 13.1% | 3.2% |
| distinct-obj | 0.421 | 0.792 | 1.881 | +88.1% | 1.863 1.782 | 19.8% | 12.7% |
| employee-docs | 0.665 | 0.945 | 1.422 | +42.2% | 1.290 1.517 | 6.1% | 29.2% |
| exists-join | 9.855 | 9.960 | 1.011 | +1.1% | 0.981 1.042 | 9.5% | 10.1% |
| export-500k | 257.729 | 295.261 | 1.146 | +14.6% | 1.140 1.151 | 1.4% | 0.5% |
| expr-agg-arg | 30.461 | 25.002 | 0.821 | -17.9% | 0.822 0.816 | 2.7% | 4.3% |
| expr-bind-group | 5.515 | 4.507 | 0.817 | -18.3% | 0.827 0.807 | 2.5% | 1.8% |
| expr-order-key | 12.680 | 11.883 | 0.937 | -6.3% | 0.947 0.909 | 5.9% | 1.1% |
| group-avg | 51.421 | 42.137 | 0.819 | -18.1% | 0.801 0.833 | 5.6% | 3.3% |
| knows-reach | 147.322 | 143.870 | 0.977 | -2.3% | 0.973 0.992 | 1.8% | 2.8% |
| lang-filter | 3.574 | 3.785 | 1.059 | +5.9% | 1.079 1.040 | 3.2% | 5.5% |
| minus | 4.110 | 3.540 | 0.862 | -13.8% | 0.870 0.833 | 4.5% | 9.8% |
| not-exists | 11.006 | 10.280 | 0.934 | -6.6% | 0.944 0.934 | 8.0% | 5.3% |
| optional-chain | 173.496 | 183.644 | 1.058 | +5.8% | 1.026 1.091 | 30.8% | 7.9% |
| optional-count | 6.332 | 6.205 | 0.980 | -2.0% | 0.986 0.980 | 2.2% | 4.5% |
| order-by-full | 639.828 | 691.200 | 1.080 | +8.0% | 1.089 1.066 | 13.0% | 7.9% |
| path-plus | 0.457 | 0.806 | 1.763 | +76.3% | 1.772 1.707 | 25.3% | 27.4% |
| predicate-counts | 0.442 | 0.794 | 1.795 | +79.5% | 1.812 1.815 | 17.0% | 30.3% |
| predicate-subjects | 0.449 | 0.790 | 1.760 | +76.0% | 1.848 1.621 | 22.2% | 17.0% |
| range-topk | 3.553 | 3.944 | 1.110 | +11.0% | 1.177 1.033 | 11.4% | 20.6% |
| regex-iri | 6.819 | 7.364 | 1.080 | +8.0% | 1.089 1.056 | 3.6% | 2.6% |
| star-join | 34.797 | 45.096 | 1.296 | +29.6% | 1.284 1.290 | 53.3% | 3.6% |
| star-lookup | 0.766 | 1.098 | 1.433 | +43.3% | 1.359 1.430 | 8.5% | 7.7% |
| subquery-agg | 1.458 | 1.964 | 1.347 | +34.7% | 1.371 1.309 | 6.5% | 8.4% |
| two-hop-count | 12.503 | 12.569 | 1.005 | +0.5% | 1.004 1.004 | 1.3% | 4.6% |
| types-grouped | 0.533 | 0.844 | 1.583 | +58.3% | 1.565 1.588 | 9.2% | 8.2% |
| values-star | 0.651 | 0.925 | 1.423 | +42.3% | 1.437 1.427 | 4.7% | 13.3% |
| **geometric mean** | | | 1.167 | +16.7% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
