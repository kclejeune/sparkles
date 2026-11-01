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
| class-predicate-list | 1.010 | 1.015 | 1.005 | +0.5% | 1.000 0.991 | 5.2% | 2.3% |
| class-predicates | 1.022 | 1.025 | 1.004 | +0.4% | 1.009 0.998 | 4.7% | 0.6% |
| contains | 0.688 | 0.692 | 1.006 | +0.6% | 1.013 1.031 | 14.9% | 10.3% |
| count-all | 0.007 | 0.010 | 1.396 | +39.6% | 1.394 1.443 | 11.6% | 11.4% |
| distinct-join | 3.578 | 3.898 | 1.089 | +8.9% | 1.074 1.099 | 3.6% | 2.7% |
| distinct-obj | 0.006 | 0.009 | 1.584 | +58.4% | 1.404 1.782 | 18.4% | 22.6% |
| employee-docs | 0.045 | 0.053 | 1.175 | +17.5% | 1.170 1.274 | 9.3% | 16.5% |
| exists-join | 1.117 | 1.131 | 1.013 | +1.3% | 1.000 1.037 | 6.3% | 4.2% |
| export-500k | 2.383 | 0.407 | 0.171 | -82.9% | 0.173 0.168 | 10.4% | 1.6% |
| expr-agg-arg | 3.060 | 2.037 | 0.666 | -33.4% | 0.658 0.682 | 6.4% | 1.6% |
| expr-bind-group | 0.779 | 0.396 | 0.508 | -49.2% | 0.529 0.484 | 24.3% | 4.6% |
| expr-order-key | 1.248 | 1.210 | 0.970 | -3.0% | 0.948 0.967 | 6.0% | 3.8% |
| group-avg | 4.244 | 3.646 | 0.859 | -14.1% | 0.873 0.857 | 2.6% | 3.4% |
| knows-reach | 11.882 | 11.850 | 0.997 | -0.3% | 0.994 0.996 | 3.3% | 2.3% |
| lang-filter | 0.670 | 0.736 | 1.098 | +9.8% | 1.125 1.103 | 12.8% | 22.5% |
| minus | 0.351 | 0.276 | 0.787 | -21.3% | 0.771 0.801 | 6.1% | 2.9% |
| not-exists | 0.876 | 0.796 | 0.909 | -9.1% | 0.915 0.897 | 3.9% | 2.6% |
| optional-chain | 2.117 | 2.064 | 0.975 | -2.5% | 0.973 0.968 | 3.9% | 3.2% |
| optional-count | 0.535 | 0.550 | 1.029 | +2.9% | 1.012 1.047 | 4.0% | 5.0% |
| order-by-full | 13.750 | 14.336 | 1.043 | +4.3% | 1.052 1.060 | 3.9% | 4.5% |
| path-plus | 0.017 | 0.019 | 1.127 | +12.7% | 1.252 1.026 | 12.2% | 27.7% |
| predicate-counts | 0.006 | 0.011 | 1.667 | +66.7% | 1.712 1.591 | 18.0% | 89.2% |
| predicate-subjects | 0.009 | 0.016 | 1.856 | +85.6% | 2.185 1.961 | 34.4% | 69.6% |
| range-topk | 0.753 | 0.578 | 0.767 | -23.3% | 0.783 0.763 | 6.1% | 9.1% |
| regex-iri | 0.906 | 1.009 | 1.114 | +11.4% | 1.131 1.118 | 6.5% | 9.7% |
| star-join | 1.522 | 1.604 | 1.054 | +5.4% | 1.066 1.063 | 1.5% | 3.4% |
| star-lookup | 0.083 | 0.087 | 1.045 | +4.5% | 1.050 1.037 | 11.2% | 7.4% |
| subquery-agg | 0.220 | 0.224 | 1.020 | +2.0% | 0.890 1.054 | 29.0% | 51.8% |
| two-hop-count | 1.172 | 1.204 | 1.027 | +2.7% | 1.047 1.028 | 4.8% | 11.0% |
| types-grouped | 0.011 | 0.019 | 1.802 | +80.2% | 1.822 1.770 | 24.1% | 82.0% |
| values-star | 0.019 | 0.026 | 1.349 | +34.9% | 1.329 1.358 | 4.9% | 22.7% |
| **geometric mean** | | | 0.999 | -0.1% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 1.671 | 1.892 | 1.133 | +13.3% | 1.136 1.131 | 3.9% | 4.4% |
| class-predicates | 1.686 | 1.912 | 1.134 | +13.4% | 1.124 1.139 | 1.4% | 6.9% |
| contains | 1.379 | 1.556 | 1.128 | +12.8% | 1.171 1.110 | 8.4% | 8.9% |
| count-all | 0.450 | 0.736 | 1.634 | +63.4% | 1.689 1.501 | 6.9% | 30.9% |
| distinct-join | 4.864 | 5.603 | 1.152 | +15.2% | 1.136 1.187 | 2.9% | 7.8% |
| distinct-obj | 0.464 | 0.790 | 1.703 | +70.3% | 1.604 1.753 | 13.7% | 11.6% |
| employee-docs | 0.647 | 0.939 | 1.452 | +45.2% | 1.472 1.446 | 14.2% | 17.4% |
| exists-join | 1.861 | 2.164 | 1.163 | +16.3% | 1.164 1.156 | 2.7% | 6.6% |
| export-500k | 250.067 | 301.130 | 1.204 | +20.4% | 1.202 1.206 | 0.5% | 0.7% |
| expr-agg-arg | 3.889 | 3.072 | 0.790 | -21.0% | 0.784 0.825 | 5.7% | 1.3% |
| expr-bind-group | 1.458 | 1.395 | 0.956 | -4.4% | 0.945 0.926 | 12.6% | 5.5% |
| expr-order-key | 1.994 | 2.200 | 1.104 | +10.4% | 1.095 1.119 | 4.8% | 5.5% |
| group-avg | 5.081 | 4.782 | 0.941 | -5.9% | 0.956 0.932 | 2.5% | 2.2% |
| knows-reach | 12.918 | 12.915 | 1.000 | -0.0% | 0.995 1.002 | 3.7% | 2.4% |
| lang-filter | 1.335 | 1.566 | 1.173 | +17.3% | 1.176 1.183 | 8.4% | 22.0% |
| minus | 0.986 | 1.211 | 1.228 | +22.8% | 1.239 1.189 | 2.0% | 14.3% |
| not-exists | 1.550 | 1.754 | 1.132 | +13.2% | 1.138 1.121 | 1.1% | 8.0% |
| optional-chain | 14.620 | 18.778 | 1.284 | +28.4% | 1.314 1.276 | 2.7% | 4.1% |
| optional-count | 1.230 | 1.500 | 1.220 | +22.0% | 1.202 1.291 | 9.1% | 6.4% |
| order-by-full | 48.451 | 56.598 | 1.168 | +16.8% | 1.163 1.180 | 2.6% | 1.1% |
| path-plus | 0.481 | 0.643 | 1.334 | +33.4% | 1.354 1.221 | 7.7% | 43.1% |
| predicate-counts | 0.475 | 0.688 | 1.448 | +44.8% | 1.568 1.419 | 12.9% | 26.1% |
| predicate-subjects | 0.449 | 0.718 | 1.599 | +59.9% | 1.453 1.759 | 10.0% | 25.7% |
| range-topk | 1.535 | 1.521 | 0.991 | -0.9% | 0.994 0.986 | 3.7% | 16.7% |
| regex-iri | 1.616 | 1.942 | 1.202 | +20.2% | 1.176 1.221 | 3.8% | 4.1% |
| star-join | 4.923 | 6.662 | 1.353 | +35.3% | 1.339 1.353 | 1.7% | 1.1% |
| star-lookup | 0.772 | 1.048 | 1.358 | +35.8% | 1.425 1.280 | 12.5% | 11.7% |
| subquery-agg | 0.820 | 1.077 | 1.313 | +31.3% | 1.285 1.337 | 10.6% | 14.6% |
| two-hop-count | 1.947 | 2.277 | 1.169 | +16.9% | 1.189 1.141 | 2.3% | 6.1% |
| types-grouped | 0.526 | 0.832 | 1.583 | +58.3% | 1.599 1.454 | 15.0% | 17.2% |
| values-star | 0.639 | 0.873 | 1.366 | +36.6% | 1.441 1.297 | 1.6% | 22.2% |
| **geometric mean** | | | 1.222 | +22.2% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
