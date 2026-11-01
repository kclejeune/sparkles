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
| class-predicate-list | 10.005 | 10.421 | 1.042 | +4.2% | 1.038 1.044 | 0.7% | 0.4% |
| class-predicates | 10.055 | 10.463 | 1.041 | +4.1% | 1.040 1.041 | 2.1% | 1.7% |
| contains | 23.818 | 24.099 | 1.012 | +1.2% | 1.012 1.013 | 0.3% | 0.2% |
| count-all | 0.005 | 0.007 | 1.248 | +24.8% | 1.288 1.177 | 14.2% | 10.7% |
| distinct-join | 38.933 | 38.640 | 0.992 | -0.8% | 0.994 0.992 | 0.3% | 0.4% |
| distinct-obj | 0.004 | 0.005 | 1.223 | +22.3% | 1.287 1.158 | 22.2% | 6.7% |
| employee-docs | 0.040 | 0.047 | 1.167 | +16.7% | 1.134 1.160 | 7.2% | 3.9% |
| exists-join | 8.589 | 8.562 | 0.997 | -0.3% | 0.992 1.008 | 4.5% | 1.3% |
| export-500k | 2.262 | 0.445 | 0.197 | -80.3% | 0.160 0.214 | 95.5% | 2.1% |
| expr-agg-arg | 29.460 | 22.960 | 0.779 | -22.1% | 0.782 0.778 | 3.4% | 0.4% |
| expr-bind-group | 5.597 | 3.298 | 0.589 | -41.1% | 0.591 0.581 | 2.0% | 1.4% |
| expr-order-key | 11.981 | 10.404 | 0.868 | -13.2% | 0.867 0.870 | 1.0% | 0.7% |
| group-avg | 46.975 | 39.318 | 0.837 | -16.3% | 0.838 0.836 | 1.3% | 0.7% |
| knows-reach | 140.772 | 137.162 | 0.974 | -2.6% | 0.983 0.970 | 0.8% | 1.1% |
| lang-filter | 15.938 | 15.828 | 0.993 | -0.7% | 0.988 0.996 | 1.1% | 0.1% |
| minus | 3.143 | 2.423 | 0.771 | -22.9% | 0.744 0.772 | 5.0% | 0.6% |
| not-exists | 9.750 | 8.618 | 0.884 | -11.6% | 0.905 0.884 | 4.6% | 4.7% |
| optional-chain | 33.439 | 23.099 | 0.691 | -30.9% | 0.671 0.698 | 9.0% | 14.0% |
| optional-count | 5.278 | 4.970 | 0.942 | -5.8% | 0.948 0.940 | 1.0% | 1.0% |
| order-by-full | 824.043 | 698.021 | 0.847 | -15.3% | 0.811 0.862 | 4.1% | 4.5% |
| path-plus | 0.008 | 0.009 | 1.074 | +7.4% | 1.081 1.055 | 6.9% | 13.3% |
| predicate-counts | 0.004 | 0.006 | 1.414 | +41.4% | 1.467 1.391 | 4.7% | 8.9% |
| predicate-subjects | 0.006 | 0.008 | 1.346 | +34.6% | 1.377 1.327 | 13.0% | 5.5% |
| range-topk | 5.796 | 5.068 | 0.874 | -12.6% | 0.887 0.865 | 2.4% | 6.0% |
| regex-iri | 36.020 | 36.668 | 1.018 | +1.8% | 1.018 1.018 | 0.6% | 0.1% |
| star-join | 13.711 | 15.259 | 1.113 | +11.3% | 1.094 1.116 | 1.3% | 6.7% |
| star-lookup | 0.051 | 0.054 | 1.074 | +7.4% | 1.092 1.067 | 3.1% | 2.0% |
| subquery-agg | 1.643 | 1.631 | 0.993 | -0.7% | 0.995 0.990 | 1.5% | 0.4% |
| two-hop-count | 27.882 | 27.919 | 1.001 | +0.1% | 1.003 1.000 | 0.4% | 0.4% |
| types-grouped | 0.007 | 0.013 | 1.776 | +77.6% | 1.712 1.839 | 15.6% | 8.3% |
| values-star | 0.015 | 0.019 | 1.250 | +25.0% | 1.214 1.284 | 8.9% | 2.8% |
| **geometric mean** | | | 0.953 | -4.7% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.345 | 11.755 | 1.036 | +3.6% | 1.039 1.039 | 0.5% | 2.3% |
| class-predicates | 11.412 | 11.868 | 1.040 | +4.0% | 1.038 1.042 | 1.7% | 1.3% |
| contains | 25.026 | 25.291 | 1.011 | +1.1% | 1.011 1.009 | 0.4% | 0.3% |
| count-all | 1.974 | 1.974 | 1.000 | +0.0% | 1.001 0.998 | 0.9% | 15.1% |
| distinct-join | 44.389 | 46.221 | 1.041 | +4.1% | 1.054 1.040 | 0.3% | 1.4% |
| distinct-obj | 1.974 | 1.956 | 0.991 | -0.9% | 1.000 0.966 | 0.1% | 8.8% |
| employee-docs | 1.950 | 1.981 | 1.016 | +1.6% | 1.013 1.025 | 1.8% | 2.1% |
| exists-join | 9.957 | 9.877 | 0.992 | -0.8% | 0.982 1.010 | 4.7% | 2.5% |
| export-500k | 273.008 | 368.998 | 1.352 | +35.2% | 1.352 1.353 | 0.9% | 0.6% |
| expr-agg-arg | 30.873 | 24.305 | 0.787 | -21.3% | 0.793 0.785 | 4.6% | 3.2% |
| expr-bind-group | 6.912 | 4.562 | 0.660 | -34.0% | 0.671 0.652 | 2.7% | 2.3% |
| expr-order-key | 13.976 | 11.617 | 0.831 | -16.9% | 0.834 0.834 | 1.7% | 0.8% |
| group-avg | 48.581 | 40.819 | 0.840 | -16.0% | 0.842 0.841 | 1.6% | 0.9% |
| knows-reach | 142.122 | 138.412 | 0.974 | -2.6% | 0.984 0.970 | 0.9% | 1.2% |
| lang-filter | 17.125 | 17.069 | 0.997 | -0.3% | 0.990 0.999 | 0.9% | 0.1% |
| minus | 4.470 | 3.673 | 0.822 | -17.8% | 0.820 0.837 | 9.3% | 2.3% |
| not-exists | 10.954 | 9.900 | 0.904 | -9.6% | 0.910 0.900 | 2.4% | 3.9% |
| optional-chain | 151.012 | 220.453 | 1.460 | +46.0% | 1.449 1.467 | 4.1% | 2.5% |
| optional-count | 6.616 | 6.232 | 0.942 | -5.8% | 0.911 0.949 | 6.5% | 13.1% |
| order-by-full | 1191.555 | 1259.525 | 1.057 | +5.7% | 1.029 1.066 | 2.9% | 2.9% |
| path-plus | 1.973 | 1.975 | 1.001 | +0.1% | 1.000 1.005 | 15.5% | 0.6% |
| predicate-counts | 1.972 | 1.958 | 0.993 | -0.7% | 0.965 1.000 | 1.2% | 29.9% |
| predicate-subjects | 1.976 | 1.982 | 1.003 | +0.3% | 1.004 1.002 | 0.7% | 1.1% |
| range-topk | 7.151 | 6.775 | 0.947 | -5.3% | 0.962 0.909 | 10.4% | 7.9% |
| regex-iri | 37.236 | 37.959 | 1.019 | +1.9% | 1.019 1.021 | 0.3% | 0.2% |
| star-join | 37.001 | 53.469 | 1.445 | +44.5% | 1.446 1.444 | 1.4% | 2.0% |
| star-lookup | 1.983 | 1.986 | 1.002 | +0.2% | 1.001 1.003 | 1.7% | 3.2% |
| subquery-agg | 2.833 | 2.902 | 1.025 | +2.5% | 1.023 1.023 | 1.6% | 0.8% |
| two-hop-count | 29.203 | 29.249 | 1.002 | +0.2% | 1.003 1.000 | 0.5% | 0.2% |
| types-grouped | 1.945 | 1.976 | 1.016 | +1.6% | 1.017 1.019 | 18.2% | 16.7% |
| values-star | 1.977 | 1.977 | 1.000 | -0.0% | 1.000 0.988 | 0.1% | 2.9% |
| **geometric mean** | | | 0.995 | -0.5% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
