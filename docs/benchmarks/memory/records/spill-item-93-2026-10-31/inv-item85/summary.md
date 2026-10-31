# Interleaved A/B on Namespace

- A: file /home/kclejeune/lanes/spill-7-93-20261031/bin/sparkles-5453f9d0
- A sha256: 6db286d65cc12d28b9b8be6fb9596cb48bafdebb75bd9b7fbd60a1a6b7c8f2fd
- B: file /home/kclejeune/lanes/spill-7-93-20261031/bin/sparkles-c88c5969
- B sha256: f7e94f791d3cb600515913fbaebc5d3ca7b50401d94865dd058e10a28543e62b
- data: /home/kclejeune/lanes/spill-7-93-20261031/data/people-1000000.nt.zst 3f51783448371dd0f57391e7d3fdadae66442f0c32ae260c93a3a96b43b1cbd4
- host: forge 7.2.9
- cpu:  13th Gen Intel(R) Core(TM) i5-13500
- serve A: 
- serve B: 
- share store: 0
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 4 queries.

Load: A 4.16 s, B 4.47 s (B/A 1.075). Store size: A 286 MiB, B 286 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| distinct-join | 39.669 | 39.834 | 1.004 | +0.4% | 1.000 1.003 1.006 1.003 1.005 1.002 1.003 1.006 1.005 1.003 1.004 1.005 1.006 1.003 1.003 1.005 | 3.8% | 3.4% |
| distinct-obj | 0.003 | 0.003 | 1.060 | +6.0% | 1.134 1.045 1.065 1.122 1.042 1.065 1.018 0.953 1.032 1.043 0.991 1.059 1.026 1.116 1.127 1.140 | 54.9% | 57.5% |
| employee-docs | 0.046 | 0.045 | 0.995 | -0.5% | 0.994 0.935 1.002 1.032 1.013 0.987 0.974 1.001 0.980 0.985 1.000 1.030 1.017 0.975 1.021 0.989 | 42.9% | 47.2% |
| expr-order-key | 12.174 | 12.599 | 1.035 | +3.5% | 1.040 1.033 1.033 1.039 1.031 1.032 1.037 1.036 1.034 1.035 1.036 1.034 1.038 1.037 1.033 1.035 | 1.5% | 2.0% |
| **geometric mean** | | | 1.023 | +2.3% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| distinct-join | 46.822 | 47.007 | 1.004 | +0.4% | 1.001 1.003 1.006 1.003 1.005 1.002 1.002 1.005 1.004 1.001 1.003 1.005 1.006 1.003 1.002 1.005 | 4.0% | 3.6% |
| distinct-obj | 1.973 | 1.973 | 1.000 | +0.0% | 1.001 1.001 0.999 1.000 1.000 0.999 0.999 0.999 0.998 1.001 0.999 1.001 1.001 0.999 1.000 0.998 | 0.7% | 1.1% |
| employee-docs | 1.977 | 1.977 | 1.000 | +0.0% | 1.000 1.002 0.999 1.004 0.999 0.982 1.004 0.989 0.993 0.996 1.000 1.008 0.999 1.000 0.999 0.986 | 13.9% | 15.5% |
| expr-order-key | 14.071 | 14.579 | 1.036 | +3.6% | 1.043 1.031 1.030 1.043 1.038 1.027 1.042 1.035 1.047 1.040 1.036 1.029 1.043 1.040 1.039 1.028 | 3.3% | 3.0% |
| **geometric mean** | | | 1.010 | +1.0% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
