# Interleaved A/B on forge

- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

Validity: valid. Same-binary spread 1.9% over 6 queries of at least 0.3 ms (at most 3%), server-process speed 0.983 to 1.014 of the median (within 5%).

A and B returned the same sorted TSV answers to all 8 queries.

Load: A 0.53 s, B 0.55 s (B/A 1.035). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 0.935 | 0.903 | 0.965 | -3.5% | 0.963 0.968 0.969 0.965 0.962 0.963 0.964 0.967 | 2.0% | 1.8% |
| class-predicates | 0.944 | 0.915 | 0.970 | -3.0% | 0.975 0.970 0.970 0.969 0.967 0.973 0.964 0.970 | 1.4% | 1.8% |
| distinct-join | 3.790 | 3.798 | 1.002 | +0.2% | 0.998 1.006 1.006 1.004 1.004 0.996 1.005 0.999 | 1.1% | 0.7% |
| distinct-obj | 0.005 | 0.005 | 1.002 | +0.2% | 0.980 1.033 1.017 0.959 1.002 1.025 1.045 0.985 | 15.1% | 18.9% |
| distinct-one | 2.919 | 2.846 | 0.975 | -2.5% | 0.974 0.985 0.973 0.977 0.975 0.972 0.977 0.971 | 2.0% | 1.1% |
| distinct-two | 6.542 | 6.593 | 1.008 | +0.8% | 1.009 1.008 1.008 1.004 1.010 1.006 1.009 1.007 | 0.9% | 0.5% |
| distinct-two-wide | 32.316 | 33.390 | 1.033 | +3.3% | 1.021 1.041 1.011 1.002 1.055 1.056 1.027 1.058 | 8.2% | 4.7% |
| predicate-subjects | 0.009 | 0.009 | 1.010 | +1.0% | 0.977 1.002 1.082 1.033 0.983 0.976 1.040 1.055 | 8.4% | 14.7% |
| **geometric mean** | | | 0.995 | -0.5% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 2.009 | 1.978 | 0.985 | -1.5% | 0.970 1.000 1.001 1.025 0.991 0.993 0.976 1.008 | 16.7% | 10.9% |
| class-predicates | 2.032 | 2.042 | 1.005 | +0.5% | 1.088 1.024 1.150 0.886 0.999 1.002 0.912 0.966 | 42.0% | 28.4% |
| distinct-join | 5.979 | 5.978 | 1.000 | -0.0% | 1.000 1.000 1.000 1.000 1.000 1.000 0.999 0.999 | 0.2% | 0.8% |
| distinct-obj | 0.977 | 0.977 | 1.000 | +0.0% | 0.998 0.972 0.997 0.999 1.002 1.060 1.002 0.998 | 42.9% | 102.3% |
| distinct-one | 22.058 | 21.992 | 0.997 | -0.3% | 0.997 1.000 0.997 0.997 1.000 0.996 0.997 0.997 | 0.4% | 0.3% |
| distinct-two | 25.038 | 24.992 | 0.998 | -0.2% | 0.998 1.000 0.999 0.966 1.000 0.999 0.975 0.980 | 0.3% | 3.6% |
| distinct-two-wide | 195.048 | 193.026 | 0.990 | -1.0% | 0.985 0.996 0.990 0.980 0.995 0.987 0.986 0.989 | 2.1% | 1.3% |
| predicate-subjects | 0.986 | 0.984 | 0.998 | -0.2% | 0.946 1.051 1.010 1.098 1.049 0.683 0.997 0.715 | 108.9% | 72.2% |
| **geometric mean** | | | 0.997 | -0.3% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
