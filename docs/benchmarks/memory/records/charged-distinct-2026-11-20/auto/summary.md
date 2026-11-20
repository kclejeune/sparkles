# Interleaved A/B on forge

- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

Validity: valid. Same-binary spread 2.8% over 6 queries of at least 0.3 ms (at most 3%), server-process speed 0.986 to 1.013 of the median (within 5%).

A and B returned the same sorted TSV answers to all 8 queries.

Load: A 0.57 s, B 0.66 s (B/A 1.164). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 0.938 | 0.906 | 0.965 | -3.5% | 0.964 0.959 0.969 0.968 0.965 0.966 0.959 0.967 | 2.2% | 1.4% |
| class-predicates | 0.946 | 0.914 | 0.966 | -3.4% | 0.966 0.964 0.970 0.966 0.965 0.970 0.965 0.968 | 1.9% | 1.9% |
| distinct-join | 3.628 | 3.627 | 1.000 | -0.0% | 1.000 0.992 0.992 1.001 1.008 0.997 0.989 1.016 | 1.6% | 3.3% |
| distinct-obj | 0.004 | 0.004 | 0.999 | -0.1% | 0.973 0.949 1.004 1.070 1.056 1.016 0.980 0.981 | 13.6% | 13.9% |
| distinct-one | 2.977 | 2.977 | 1.000 | -0.0% | 0.999 0.999 1.006 1.005 0.999 0.997 0.992 1.000 | 2.8% | 2.3% |
| distinct-two | 7.661 | 7.693 | 1.004 | +0.4% | 1.011 1.003 1.007 1.005 0.998 1.002 1.004 1.002 | 2.7% | 2.4% |
| distinct-two-wide | 41.639 | 41.690 | 1.001 | +0.1% | 1.011 0.999 0.999 1.013 0.984 1.008 1.002 0.997 | 6.0% | 4.0% |
| predicate-subjects | 0.007 | 0.007 | 0.943 | -5.7% | 0.945 0.932 0.940 0.961 0.949 0.970 0.982 0.913 | 8.5% | 10.3% |
| **geometric mean** | | | 0.985 | -1.5% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 1.974 | 1.986 | 1.006 | +0.6% | 1.005 0.997 0.975 1.002 1.025 1.005 1.023 0.989 | 3.7% | 5.1% |
| class-predicates | 1.976 | 1.990 | 1.008 | +0.8% | 1.021 1.004 1.005 1.005 1.038 1.000 1.026 0.988 | 4.5% | 13.1% |
| distinct-join | 5.299 | 5.386 | 1.016 | +1.6% | 1.031 0.999 0.977 1.026 1.019 1.090 1.049 1.016 | 11.3% | 13.3% |
| distinct-obj | 0.973 | 0.976 | 1.003 | +0.3% | 0.970 1.003 1.019 1.001 1.041 1.002 1.024 0.982 | 6.8% | 31.0% |
| distinct-one | 20.002 | 19.334 | 0.967 | -3.3% | 0.995 0.987 1.000 0.963 0.954 0.984 0.955 1.000 | 5.1% | 5.0% |
| distinct-two | 29.055 | 29.065 | 1.000 | +0.0% | 0.998 1.006 1.004 1.002 1.000 1.006 1.000 1.000 | 0.8% | 3.6% |
| distinct-two-wide | 229.031 | 228.031 | 0.996 | -0.4% | 1.000 0.996 1.005 0.995 0.984 1.000 0.991 1.002 | 3.1% | 3.1% |
| predicate-subjects | 0.975 | 0.975 | 1.000 | +0.0% | 0.999 0.999 0.647 1.048 0.999 0.997 0.996 1.003 | 78.6% | 37.4% |
| **geometric mean** | | | 0.999 | -0.1% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
