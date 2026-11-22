# Interleaved A/B on Namespace

- A: commit c0948014705b4bb40aaa7fe4c86c3c671444b7e9 (c0948014), built on wally
- A sha256: 1b04d43f6bb91343a5de3204f366d973b976851dab1a44f94350bd0bd065edff
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 4e9185f15e1397660651dad90c7817be1beef91e (4e9185f1), built on wally
- B sha256: 3ba8fd687308e297e5056a7d69e7cda65b0264b5a57e7e5d2a4a1c284bcc9dc3
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py), 6.0M compressed
- host: atlas (ssh, nsc-bench-remote.sh run directly)
- settings: 9 queries, 12 rounds of A,B,B,A, 3 warm-up and 200 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners 1, execution streaming
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 9 queries.

Load: A 0.54 s, B 0.49 s (B/A 0.909). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.006 | 0.006 | 1.000 | +0.0% | 1.048 1.010 1.066 1.005 1.021 1.082 0.941 0.948 0.971 0.955 1.080 0.917 | 25.5% | 32.6% |
| distinct-obj | 0.005 | 0.005 | 1.068 | +6.8% | 1.127 1.107 1.099 1.036 1.102 1.070 1.036 1.033 1.061 1.011 1.114 1.069 | 17.5% | 23.5% |
| path-plus | 0.011 | 0.011 | 1.010 | +1.0% | 1.068 1.062 0.944 1.032 1.022 0.990 1.032 1.061 1.019 0.979 0.973 0.955 | 16.8% | 15.3% |
| predicate-counts | 0.006 | 0.007 | 1.077 | +7.7% | 1.140 1.109 1.053 1.067 1.102 1.066 1.101 1.064 1.058 1.032 1.085 1.055 | 17.9% | 15.5% |
| root-limit | 0.004 | 0.004 | 0.917 | -8.3% | 0.938 0.977 0.904 0.933 0.961 0.904 0.901 0.911 0.930 0.852 0.901 0.889 | 28.6% | 26.9% |
| root-scan | 0.086 | 0.084 | 0.984 | -1.6% | 0.969 0.986 0.982 0.983 0.992 0.981 0.987 0.989 0.989 0.977 0.983 0.983 | 1.5% | 2.1% |
| star-lookup | 0.064 | 0.064 | 0.996 | -0.4% | 1.002 1.014 0.970 1.029 1.035 1.015 1.019 0.993 1.009 0.888 1.024 0.971 | 16.6% | 15.5% |
| subquery-agg | 0.183 | 0.183 | 1.001 | +0.1% | 1.008 0.994 1.000 1.010 1.012 1.002 1.023 1.000 0.988 0.961 1.019 0.995 | 7.1% | 8.4% |
| values-star | 0.019 | 0.020 | 1.011 | +1.1% | 0.982 1.023 1.025 1.002 1.027 1.056 1.009 1.024 1.005 0.937 1.008 1.042 | 13.5% | 12.8% |
| **geometric mean** | | | 1.006 | +0.6% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.975 | 0.974 | 0.999 | -0.1% | 0.977 1.000 1.000 0.999 1.001 1.000 0.998 1.000 1.002 1.001 1.082 0.735 | 38.7% | 42.6% |
| distinct-obj | 0.977 | 0.977 | 1.000 | +0.0% | 1.006 0.999 1.002 1.001 1.000 0.999 0.991 1.000 1.000 0.998 1.044 0.972 | 34.5% | 45.0% |
| path-plus | 0.980 | 0.978 | 0.998 | -0.2% | 0.979 0.999 1.003 1.000 1.000 0.999 1.001 1.004 1.000 0.916 0.951 0.919 | 83.9% | 40.6% |
| predicate-counts | 1.530 | 1.446 | 0.945 | -5.5% | 0.751 1.173 0.687 1.221 0.886 0.852 1.136 1.115 1.063 0.850 1.039 0.938 | 95.2% | 94.5% |
| root-limit | 2.043 | 2.043 | 1.000 | +0.0% | 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 | 1.2% | 0.1% |
| root-scan | 51.068 | 51.998 | 1.018 | +1.8% | 1.018 1.018 1.018 1.018 1.018 1.019 1.018 1.018 1.018 1.018 1.019 1.018 | 0.1% | 0.2% |
| star-lookup | 1.982 | 1.983 | 1.001 | +0.1% | 1.001 1.000 1.000 1.002 0.999 1.001 1.001 1.001 1.000 1.001 1.001 0.999 | 1.7% | 1.6% |
| subquery-agg | 1.972 | 1.971 | 0.999 | -0.1% | 1.001 0.999 0.999 0.999 0.999 1.000 1.000 0.999 0.999 1.001 0.999 1.000 | 0.3% | 1.9% |
| values-star | 1.978 | 1.977 | 0.999 | -0.1% | 1.000 0.999 0.999 0.999 0.999 0.999 1.000 1.001 1.000 1.001 0.997 1.000 | 1.4% | 1.9% |
| **geometric mean** | | | 0.995 | -0.5% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
