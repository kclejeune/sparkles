# Interleaved A/B on Namespace

- A: commit 4e9185f15e1397660651dad90c7817be1beef91e (4e9185f1), built on wally
- A sha256: 3ba8fd687308e297e5056a7d69e7cda65b0264b5a57e7e5d2a4a1c284bcc9dc3
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit c0948014705b4bb40aaa7fe4c86c3c671444b7e9 (c0948014), built on wally
- B sha256: 1b04d43f6bb91343a5de3204f366d973b976851dab1a44f94350bd0bd065edff
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

Load: A 0.77 s, B 0.55 s (B/A 0.717). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.006 | 0.006 | 0.974 | -2.6% | 1.026 1.073 0.978 0.942 1.001 0.983 1.014 0.992 0.963 0.961 0.913 0.906 | 20.8% | 24.4% |
| distinct-obj | 0.005 | 0.005 | 0.938 | -6.2% | 0.983 0.995 0.946 0.880 1.003 0.959 0.952 0.931 0.964 0.932 0.884 0.860 | 19.5% | 19.9% |
| path-plus | 0.010 | 0.010 | 0.979 | -2.1% | 1.040 1.033 0.982 0.918 1.001 0.978 1.069 0.942 1.012 0.969 0.938 0.869 | 25.9% | 15.7% |
| predicate-counts | 0.006 | 0.006 | 0.955 | -4.5% | 1.034 0.977 0.957 0.882 0.987 0.996 1.016 0.938 1.000 0.949 0.899 0.857 | 21.4% | 22.7% |
| root-limit | 0.004 | 0.004 | 0.977 | -2.3% | 1.056 1.023 0.982 0.972 1.040 0.933 1.049 0.934 0.964 0.950 0.978 0.913 | 29.2% | 26.5% |
| root-scan | 0.084 | 0.086 | 1.017 | +1.7% | 1.030 1.015 1.024 1.019 1.023 1.020 1.016 0.993 1.026 1.014 1.020 1.015 | 4.0% | 4.6% |
| star-lookup | 0.063 | 0.064 | 1.011 | +1.1% | 1.025 0.987 1.007 1.022 1.016 1.018 1.038 0.997 0.993 1.010 1.026 1.014 | 9.4% | 13.2% |
| subquery-agg | 0.181 | 0.180 | 0.995 | -0.5% | 1.002 1.000 0.978 1.001 0.992 0.993 0.994 1.002 0.995 1.013 0.981 0.997 | 5.0% | 5.5% |
| values-star | 0.019 | 0.019 | 1.018 | +1.8% | 1.017 1.018 1.013 1.042 1.039 1.018 1.015 0.997 1.015 0.997 1.015 1.038 | 8.9% | 9.7% |
| **geometric mean** | | | 0.985 | -1.5% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.975 | 0.975 | 1.000 | +0.0% | 0.999 1.001 1.001 1.003 1.000 1.002 1.000 0.997 0.996 0.999 1.001 0.998 | 5.0% | 2.8% |
| distinct-obj | 0.976 | 0.976 | 1.000 | +0.0% | 1.000 0.998 0.999 0.999 1.000 1.000 1.000 0.998 0.999 1.000 1.001 1.000 | 3.9% | 4.9% |
| path-plus | 0.978 | 0.977 | 0.999 | -0.1% | 0.971 1.002 1.000 1.018 1.001 0.999 1.000 0.998 1.000 1.000 1.001 0.998 | 49.1% | 3.8% |
| predicate-counts | 1.031 | 1.026 | 0.995 | -0.5% | 1.323 0.976 1.015 0.669 0.816 0.995 1.050 1.003 1.054 0.976 0.977 0.712 | 91.4% | 90.1% |
| root-limit | 2.042 | 2.042 | 1.000 | +0.0% | 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 | 0.1% | 0.2% |
| root-scan | 51.250 | 51.026 | 0.996 | -0.4% | 0.995 0.996 0.997 0.985 0.996 0.994 0.996 1.027 0.998 0.996 0.995 0.998 | 1.7% | 4.0% |
| star-lookup | 1.983 | 1.983 | 1.000 | +0.0% | 1.001 0.999 1.000 0.999 0.999 1.000 0.999 1.001 1.001 0.999 1.000 1.001 | 0.3% | 0.2% |
| subquery-agg | 1.972 | 1.972 | 1.000 | +0.0% | 1.000 0.999 1.000 0.999 1.001 1.000 0.999 1.001 0.999 1.000 1.000 1.000 | 0.4% | 0.2% |
| values-star | 1.979 | 1.978 | 0.999 | -0.1% | 1.000 0.999 1.000 0.999 1.000 1.001 1.001 0.999 0.999 0.999 0.999 0.999 | 1.6% | 0.3% |
| **geometric mean** | | | 0.999 | -0.1% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
