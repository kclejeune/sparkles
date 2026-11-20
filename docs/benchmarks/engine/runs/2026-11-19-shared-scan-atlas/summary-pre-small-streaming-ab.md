# Interleaved A/B on Namespace

- A: commit 9acd6d742115a10249e89e752dcb8444c3266832 (9acd6d74), built on wally
- A sha256: f0090fa03425eb22765931c552c74ab41d35448e95aebdf4f64f1100e2984e8f
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

Load: A 0.66 s, B 0.59 s (B/A 0.887). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.005 | 0.006 | 1.065 | +6.5% | 1.108 1.053 1.028 1.091 1.054 1.064 1.043 1.094 1.056 1.116 1.013 1.081 | 14.6% | 17.8% |
| distinct-obj | 0.005 | 0.005 | 1.056 | +5.6% | 1.129 1.086 1.012 1.077 1.042 1.069 1.013 1.074 1.083 1.102 1.043 1.016 | 21.4% | 22.4% |
| path-plus | 0.011 | 0.010 | 0.970 | -3.0% | 0.986 1.016 0.942 1.047 0.937 0.973 0.945 0.984 0.988 0.973 0.929 0.934 | 17.3% | 25.0% |
| predicate-counts | 0.006 | 0.006 | 1.032 | +3.2% | 1.120 1.004 1.034 1.102 1.023 1.032 1.029 1.065 1.038 1.015 1.026 0.979 | 21.9% | 34.7% |
| root-limit | 0.004 | 0.004 | 1.077 | +7.7% | 1.029 1.045 1.116 1.198 1.024 1.047 1.111 1.040 1.046 1.104 1.096 1.076 | 17.7% | 25.9% |
| root-scan | 0.082 | 0.085 | 1.028 | +2.8% | 1.020 1.025 1.022 1.028 1.025 1.025 1.031 1.022 1.022 1.024 1.037 1.043 | 2.9% | 3.6% |
| star-lookup | 0.064 | 0.063 | 0.989 | -1.1% | 0.964 0.979 0.986 1.026 1.009 0.982 1.009 1.000 0.977 1.004 0.990 0.969 | 8.2% | 16.3% |
| subquery-agg | 0.182 | 0.183 | 1.006 | +0.6% | 1.003 1.002 1.015 1.010 0.994 1.000 1.019 1.009 1.009 1.023 1.008 0.990 | 4.0% | 6.1% |
| values-star | 0.019 | 0.019 | 1.023 | +2.3% | 1.034 1.032 0.991 1.040 1.003 1.026 1.073 1.050 1.010 1.043 1.041 0.979 | 9.9% | 8.5% |
| **geometric mean** | | | 1.027 | +2.7% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.974 | 0.974 | 1.000 | +0.0% | 0.998 1.000 1.000 1.000 1.000 1.001 1.000 1.000 1.002 0.999 1.001 1.001 | 2.6% | 0.5% |
| distinct-obj | 0.976 | 0.977 | 1.001 | +0.1% | 0.999 1.029 1.008 1.061 1.000 1.000 0.999 1.000 1.001 1.000 1.000 0.999 | 0.6% | 55.9% |
| path-plus | 0.977 | 0.979 | 1.002 | +0.2% | 1.012 1.040 1.057 1.808 0.928 0.947 0.999 1.000 1.003 0.999 1.002 1.000 | 71.3% | 94.4% |
| predicate-counts | 1.399 | 1.708 | 1.221 | +22.1% | 1.142 0.934 1.217 1.287 0.615 1.599 1.348 1.436 1.071 1.382 1.272 1.003 | 100.6% | 100.9% |
| root-limit | 2.042 | 2.042 | 1.000 | +0.0% | 1.000 1.000 1.000 1.001 1.000 1.000 1.000 1.000 1.000 1.000 0.999 1.000 | 0.2% | 0.2% |
| root-scan | 51.054 | 51.983 | 1.018 | +1.8% | 1.018 1.019 1.018 1.018 1.018 1.018 1.018 1.018 1.018 1.019 1.018 1.018 | 0.1% | 0.1% |
| star-lookup | 1.982 | 1.982 | 1.000 | +0.0% | 1.000 0.999 1.001 0.999 0.999 1.000 1.001 1.000 1.001 0.999 1.001 0.999 | 0.4% | 0.3% |
| subquery-agg | 1.971 | 1.971 | 1.000 | +0.0% | 1.001 1.000 0.999 1.001 0.999 0.999 1.001 0.999 1.000 0.999 1.001 1.001 | 0.3% | 1.3% |
| values-star | 1.978 | 1.978 | 1.000 | +0.0% | 1.000 0.999 0.999 1.000 0.999 0.999 1.000 1.000 0.999 1.000 1.001 1.001 | 0.6% | 0.3% |
| **geometric mean** | | | 1.025 | +2.5% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
