# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/ws1a-gates-469-473/bin/sparkles-04d49b57
- binary sha256: 34aa23f8e08949fd166863059128e1afa3108501fb171b65d75494b0ba09e0fd
- binary commit: 04d49b57
- version: sparkles 0.1.0
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-01T04:47:16Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution eager,streaming
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
| class-predicate-list | 0.938 | 0.934 | 0.996 | -0.4% | 1.061 0.985 | 2.6% | 8.3% |
| class-predicates | 0.946 | 0.945 | 0.999 | -0.1% | 1.033 0.995 | 2.1% | 9.5% |
| contains | 2.442 | 2.482 | 1.016 | +1.6% | 1.021 1.013 | 0.5% | 1.6% |
| count-all | 0.004 | 0.006 | 1.379 | +37.9% | 1.807 1.389 | 20.7% | 86.2% |
| distinct-join | 3.519 | 3.698 | 1.051 | +5.1% | 1.074 1.048 | 1.9% | 3.0% |
| distinct-obj | 0.004 | 0.005 | 1.269 | +26.9% | 1.494 1.297 | 18.0% | 80.2% |
| employee-docs | 0.033 | 0.040 | 1.192 | +19.2% | 1.329 1.125 | 11.4% | 62.3% |
| exists-join | 1.020 | 1.035 | 1.014 | +1.4% | 1.041 1.004 | 2.7% | 8.3% |
| export-500k | 2.240 | 0.426 | 0.190 | -81.0% | 0.193 0.187 | 19.4% | 9.3% |
| expr-agg-arg | 2.591 | 1.912 | 0.738 | -26.2% | 0.737 0.737 | 2.1% | 1.8% |
| expr-bind-group | 0.591 | 0.367 | 0.621 | -37.9% | 0.616 0.621 | 1.8% | 1.9% |
| expr-order-key | 1.178 | 1.113 | 0.945 | -5.5% | 0.950 0.938 | 2.7% | 1.3% |
| group-avg | 4.038 | 3.473 | 0.860 | -14.0% | 0.863 0.859 | 2.1% | 1.2% |
| knows-reach | 11.520 | 11.363 | 0.986 | -1.4% | 0.987 0.987 | 0.4% | 0.6% |
| lang-filter | 1.626 | 1.619 | 0.995 | -0.5% | 0.993 0.997 | 0.8% | 0.3% |
| minus | 0.301 | 0.256 | 0.851 | -14.9% | 0.843 0.854 | 5.2% | 1.0% |
| not-exists | 0.794 | 0.731 | 0.921 | -7.9% | 0.915 0.935 | 2.1% | 1.7% |
| optional-chain | 2.033 | 1.999 | 0.984 | -1.6% | 0.986 0.963 | 4.4% | 3.9% |
| optional-count | 0.483 | 0.513 | 1.062 | +6.2% | 1.067 1.057 | 4.1% | 0.3% |
| order-by-full | 48.254 | 45.403 | 0.941 | -5.9% | 0.964 0.929 | 1.1% | 3.5% |
| path-plus | 0.011 | 0.012 | 1.117 | +11.7% | 1.383 1.131 | 11.6% | 85.4% |
| predicate-counts | 0.005 | 0.006 | 1.405 | +40.5% | 1.722 1.481 | 18.7% | 80.2% |
| predicate-subjects | 0.006 | 0.008 | 1.344 | +34.4% | 1.535 1.333 | 20.4% | 61.4% |
| range-topk | 0.660 | 0.526 | 0.797 | -20.3% | 0.820 0.796 | 0.4% | 7.2% |
| regex-iri | 3.573 | 3.669 | 1.027 | +2.7% | 1.037 1.025 | 0.8% | 2.2% |
| star-join | 1.358 | 1.402 | 1.033 | +3.3% | 1.068 1.037 | 1.8% | 6.6% |
| star-lookup | 0.060 | 0.062 | 1.047 | +4.7% | 1.442 1.040 | 9.0% | 77.9% |
| subquery-agg | 0.183 | 0.178 | 0.973 | -2.7% | 1.110 0.968 | 2.5% | 34.5% |
| two-hop-count | 2.650 | 2.620 | 0.989 | -1.1% | 1.020 0.986 | 0.5% | 6.2% |
| types-grouped | 0.007 | 0.013 | 1.816 | +81.6% | 2.386 1.822 | 11.3% | 83.1% |
| values-star | 0.015 | 0.019 | 1.254 | +25.4% | 1.606 1.219 | 4.1% | 61.1% |
| **geometric mean** | | | 0.977 | -2.3% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 2.802 | 2.801 | 1.000 | -0.0% | 0.997 1.001 | 0.2% | 0.4% |
| class-predicates | 2.804 | 2.803 | 1.000 | -0.0% | 0.998 1.001 | 0.2% | 0.4% |
| contains | 3.594 | 3.811 | 1.060 | +6.0% | 1.065 1.021 | 0.5% | 4.8% |
| count-all | 1.974 | 1.972 | 0.998 | -0.2% | 1.010 0.998 | 18.0% | 1.1% |
| distinct-join | 5.147 | 5.598 | 1.088 | +8.8% | 1.136 1.082 | 1.8% | 7.2% |
| distinct-obj | 1.948 | 1.969 | 1.011 | +1.1% | 0.989 1.013 | 1.4% | 30.2% |
| employee-docs | 1.980 | 1.973 | 0.997 | -0.3% | 0.997 0.988 | 2.4% | 43.7% |
| exists-join | 2.804 | 2.805 | 1.000 | +0.0% | 0.999 1.000 | 0.1% | 0.2% |
| export-500k | 265.856 | 371.006 | 1.396 | +39.6% | 1.401 1.395 | 0.6% | 0.6% |
| expr-agg-arg | 3.857 | 3.172 | 0.823 | -17.7% | 0.819 0.826 | 1.7% | 2.8% |
| expr-bind-group | 1.973 | 1.978 | 1.003 | +0.3% | 0.992 1.004 | 1.3% | 2.3% |
| expr-order-key | 2.804 | 2.806 | 1.001 | +0.1% | 1.000 1.000 | 0.2% | 0.2% |
| group-avg | 5.428 | 4.838 | 0.891 | -10.9% | 0.886 0.904 | 10.5% | 2.9% |
| knows-reach | 12.793 | 12.654 | 0.989 | -1.1% | 0.989 0.990 | 0.7% | 0.5% |
| lang-filter | 2.802 | 2.803 | 1.000 | +0.0% | 1.000 1.001 | 0.1% | 0.2% |
| minus | 1.974 | 1.975 | 1.001 | +0.1% | 1.002 1.000 | 0.4% | 18.0% |
| not-exists | 1.974 | 1.975 | 1.001 | +0.1% | 0.994 1.002 | 0.1% | 1.7% |
| optional-chain | 15.387 | 22.037 | 1.432 | +43.2% | 1.458 1.378 | 5.7% | 1.4% |
| optional-count | 1.974 | 1.973 | 0.999 | -0.1% | 1.000 0.998 | 0.3% | 21.5% |
| order-by-full | 85.073 | 102.073 | 1.200 | +20.0% | 1.212 1.187 | 0.8% | 2.4% |
| path-plus | 1.973 | 1.969 | 0.998 | -0.2% | 0.998 1.001 | 39.1% | 0.8% |
| predicate-counts | 1.970 | 1.978 | 1.004 | +0.4% | 0.797 1.016 | 1.5% | 46.6% |
| predicate-subjects | 1.970 | 1.980 | 1.005 | +0.5% | 1.001 1.015 | 1.6% | 1.4% |
| range-topk | 1.975 | 1.977 | 1.001 | +0.1% | 1.001 1.000 | 0.7% | 24.4% |
| regex-iri | 4.764 | 4.925 | 1.034 | +3.4% | 1.042 1.017 | 4.5% | 5.3% |
| star-join | 5.049 | 6.391 | 1.266 | +26.6% | 1.280 1.198 | 2.6% | 10.3% |
| star-lookup | 1.983 | 1.976 | 0.996 | -0.4% | 1.000 0.987 | 1.5% | 2.2% |
| subquery-agg | 1.972 | 1.971 | 0.999 | -0.1% | 0.996 1.001 | 0.3% | 0.8% |
| two-hop-count | 3.875 | 3.914 | 1.010 | +1.0% | 1.023 1.008 | 0.7% | 1.9% |
| types-grouped | 1.970 | 1.948 | 0.988 | -1.2% | 0.992 0.998 | 2.2% | 36.3% |
| values-star | 1.948 | 1.978 | 1.016 | +1.6% | 1.013 1.001 | 21.8% | 1.1% |
| **geometric mean** | | | 1.032 | +3.2% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
